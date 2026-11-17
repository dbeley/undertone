#!/usr/bin/env python3
"""Draw the README's screenshot from the real binary.

There is no graphical terminal to photograph here, and a photograph of one is
text anyway. So this drives the app through a pty and rebuilds the screen from
what it writes: the characters with their colours, and the covers at the
pixels the kitty protocol carried. The result is an SVG - a rectangle per cell,
an image per cover - which stays sharp at any zoom and is generated rather
than retouched.

    python3 scripts/screenshot.py ./result/bin/undertone

It writes two frames, `docs/screenshot-wall.svg` (the library it opens on) and
`docs/screenshot-album.svg` (an album, playing), because the two say different
things: one is a library of covers, the other is what the bar does while a
track runs.

It reads the public demo server by default, because the screenshot is the one
place the app has to look like itself rather than like a test fixture; set
`UNDERTONE_SERVER`, `UNDERTONE_USER` and `UNDERTONE_PASSWORD` to photograph
your own library instead. `--fixture` runs it against
`scripts/fixture_server.py`: no network, three albums, the same frame on every
machine - which is what a check would need, if this were checked.

The terminal presents as ghostty, so the app picks kitty graphics and a cover
arrives as pixels instead of as characters chafa picked for whatever font it
found. The cell is pinned at 10x22; the layout divides the window by it, so the
frame comes out the same shape on a real terminal with different pixels.
"""

import base64
import fcntl
import os
import pty
import re
import select
import shutil
import struct
import sys
import tempfile
import termios
import time
import zlib
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from degradation import start_fixture  # noqa: E402

COLS, ROWS = 100, 26
CELL_W, CELL_H = 10, 22

# The app draws in the terminal's own palette - magenta for what the eye should
# land on, dark grey for the quiet text - so a screenshot has to pick a theme.
# This is Tokyo Night: dark blue ground, soft purple accent, dim grey mute.
# Change it and you get the same app in a different terminal.
PALETTE = {
    "bg": "#1a1b26",
    "fg": "#c0caf5",
    "indexed": [
        "#15161e", "#f7768e", "#9ece6a", "#e0af68",
        "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
        "#565f89", "#f7768e", "#9ece6a", "#e0af68",
        "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5",
    ],
}

DEFAULT_FG = "default-fg"
DEFAULT_BG = "default-bg"


class Blob:
    """One image on its way in: kitty sends it in chunks, in order."""

    def __init__(self, id, width, height, rows, cols):
        self.id = id
        self.width, self.height, self.rows, self.cols = width, height, rows, cols
        self.data = bytearray()


class Capture:
    """A pty, and just enough of a terminal to rebuild the screen from it.

    Like `degradation.Screen`, except that it keeps each cell's colours, the
    bold and dim flags, and the rectangle every cover was placed on - the parts
    a screenshot needs and a text frame does not.
    """

    def __init__(self, binary, args, cols=COLS, rows=ROWS, env_extra=None):
        self.cols, self.rows = cols, rows
        self.chars = [[" "] * cols for _ in range(rows)]
        self.styles = [[(DEFAULT_FG, DEFAULT_BG, "")] * cols for _ in range(rows)]
        # id -> (row, col, rows, cols, png). Kitty images are objects: one
        # stays on screen until the app deletes it by id, so a cover from a
        # previous frame is still part of this one unless it was erased.
        self.images = {}
        self.untagged = 0
        self.x = self.y = 0
        self.fg, self.bg, self.flags = DEFAULT_FG, DEFAULT_BG, set()
        self.pending = ""
        self.blob = None  # the image being received
        self.blob_at = (0, 0)

        env = dict(os.environ)
        env.update(env_extra or {})
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(binary, [binary, *args], env)
        fcntl.ioctl(
            self.fd,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, cols, CELL_W * cols, CELL_H * rows),
        )

    def wait(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if ready:
                try:
                    data = os.read(self.fd, 262144)
                except OSError:
                    break
                if not data:
                    break
                self.feed(data)
        return self

    def send(self, keys, seconds=1.5):
        os.write(self.fd, keys.encode())
        return self.wait(seconds)

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.chars)

    def quit(self):
        try:
            os.write(self.fd, b"q")
            self.wait(0.5)
            os.waitpid(self.pid, 0)
        except (OSError, ChildProcessError):
            pass

    # ── the terminal ─────────────────────────────────────────────────────
    def feed(self, data):
        text = self.pending + data.decode("utf-8", "replace")
        self.pending = ""
        i = 0
        while i < len(text):
            ch = text[i]
            if ch == "\x1b":
                end = self._escape(text, i)
                if end is None:  # split across reads; keep the rest for later
                    self.pending = text[i:]
                    return
                i = end
                continue
            if ch == "\r":
                self.x = 0
            elif ch == "\n":
                self.y = min(self.y + 1, self.rows - 1)
            elif ch >= " ":
                self._put(ch)
            i += 1

    def _put(self, ch):
        if self.x >= self.cols:  # autowrap
            self.x, self.y = 0, min(self.y + 1, self.rows - 1)
        if self.x < self.cols and self.y < self.rows:
            self.chars[self.y][self.x] = ch
            self.styles[self.y][self.x] = (
                self.fg,
                self.bg,
                " ".join(sorted(self.flags)),
            )
        self.x += 1

    def _escape(self, text, i):
        """Handle one sequence at `i`; return where it ends, or None if cut.

        An incomplete sequence must return None, not consume a guess: a read
        can end anywhere, and swallowing `\x1b[17` leaves `;15H` to be drawn as
        text in the middle of the track list.
        """
        rest = text[i:]
        if len(rest) < 2:
            return None
        kind = rest[1]
        if kind == "[":
            match = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", rest)
            if not match:
                return None
            self._csi(match.group(1), match.group(2))
            return i + match.end()
        if kind == "]":
            match = re.match(r"\x1b\]([^\x07\x1b]*)(?:\x07|\x1b\\)", rest)
            return i + match.end() if match else None
        if kind == "_":
            match = re.match(r"\x1b_G([^;\x1b]*)(?:;([^\x1b]*))?\x1b\\", rest)
            if not match:
                return None
            self._apc(match.group(1), match.group(2) or "")
            return i + match.end()
        if kind in "P^X":
            # DCS (sixel), PM and SOS: string sequences, not text.
            match = re.match(r"\x1b[_P^X][^\x07\x1b]*(?:\x07|\x1b\\)", rest)
            return i + match.end() if match else None
        return i + 2

    def _csi(self, params, final):
        nums = [int(p) if p.isdigit() else 0 for p in params.split(";")] if params else []
        if final in "Hf":
            row = (nums[0] if nums and nums[0] else 1) - 1
            col = (nums[1] if len(nums) > 1 and nums[1] else 1) - 1
            self.y, self.x = min(row, self.rows - 1), min(col, self.cols - 1)
        elif final == "J" and (not nums or nums[0] in (2, 3)):
            self.chars = [[" "] * self.cols for _ in range(self.rows)]
            self.styles = [[(DEFAULT_FG, DEFAULT_BG, "")] * self.cols for _ in range(self.rows)]
        elif final == "K":
            for x in range(self.x, self.cols):
                self.chars[self.y][x] = " "
                self.styles[self.y][x] = (self.fg, self.bg, "")
        elif final == "m":
            self._sgr(params)

    def _sgr(self, params):
        codes = [int(p) if re.fullmatch(r"[0-9]+", p) else 0 for p in params.split(";")] if params else [0]
        i = 0
        while i < len(codes):
            code = codes[i]
            if code == 0:
                self.fg, self.bg, self.flags = DEFAULT_FG, DEFAULT_BG, set()
            elif code == 1:
                self.flags.add("bold")
            elif code == 2:
                self.flags.add("dim")
            elif code == 3:
                self.flags.add("italic")
            elif code == 22:
                self.flags -= {"bold", "dim"}
            elif code == 23:
                self.flags.discard("italic")
            elif code == 39:
                self.fg = DEFAULT_FG
            elif code == 49:
                self.bg = DEFAULT_BG
            elif 30 <= code <= 37:
                self.fg = f"#{code - 30}"
            elif 90 <= code <= 97:
                self.fg = f"#{code - 90 + 8}"
            elif 40 <= code <= 47:
                self.bg = f"#{code - 40}"
            elif 100 <= code <= 107:
                self.bg = f"#{code - 100 + 8}"
            elif code in (38, 48) and i + 1 < len(codes):
                colour, used = self._colour(codes, i + 1)
                if code == 38:
                    self.fg = colour
                else:
                    self.bg = colour
                i += used
            i += 1

    @staticmethod
    def _colour(codes, start):
        """Read `5;n` or `2;r;g;b`, returning the colour and how many codes."""
        if codes[start] == 5 and start + 1 < len(codes):
            return f"#{codes[start + 1]}", 2
        if codes[start] == 2 and start + 3 < len(codes):
            r, g, b = codes[start + 1 : start + 4]
            return "#%02x%02x%02x" % (r, g, b), 4
        return DEFAULT_FG, 1

    def _apc(self, keys, payload):
        fields = dict(part.split("=", 1) for part in keys.split(",") if "=" in part)

        if fields.get("a") == "d":
            # Deletion: `d=i` names one image, `d=a`/`d=A` means all of them.
            if fields.get("d") in ("a", "A") or "i" not in fields:
                self.images.clear()
            else:
                self.images.pop(fields["i"], None)
            return

        if fields.get("a") == "T":
            # A new image starts here, at the cursor. `c`/`r` are the cells it
            # covers; `s`/`v` are its pixels; `i` is the id an erase names.
            # The app strips `c`/`r` so the terminal draws a cover at the
            # pixels chafa made it (and square); keep 0 to mean "native size".
            self.untagged += 1
            self.blob = Blob(
                fields.get("i", f"untagged-{self.untagged}"),
                int(fields.get("s", CELL_W)),
                int(fields.get("v", CELL_H)),
                int(fields["r"]) if "r" in fields else 0,
                int(fields["c"]) if "c" in fields else 0,
            )
            self.blob_at = (self.y, self.x)
        if self.blob is None:
            return
        if payload:
            self.blob.data += base64.b64decode(payload + "=" * (-len(payload) % 4))
        if fields.get("m", "0") == "0":
            row, col = self.blob_at
            self.images[self.blob.id] = (
                row,
                col,
                self.blob.rows,
                self.blob.cols,
                self.blob.width,
                self.blob.height,
                png(self.blob.width, self.blob.height, bytes(self.blob.data)),
            )
            self.blob = None


# ── png ──────────────────────────────────────────────────────────────────
def png(width, height, rgba):
    """The pixels kitty sent, wrapped in a PNG (they arrive as raw RGBA)."""
    raw = b"".join(
        b"\x00" + rgba[y * width * 4 : (y + 1) * width * 4] for y in range(height)
    )

    def chunk(kind, data):
        body = kind + data
        return (
            struct.pack(">I", len(data))
            + body
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 6))
        + chunk(b"IEND", b"")
    )


# ── svg ──────────────────────────────────────────────────────────────────
# Box drawing is drawn, not typed: a font's `│` is as tall as its em box, not
# as tall as the terminal's cell, so a column of them comes out dashed. Cells
# holding a rule are painted as rectangles and left out of the text runs; every
# other glyph is text.
RULES = {
    "│": ("v", 0.5),
    "┃": ("v", 0.5),
    "┊": ("v", 0.5),
    "─": ("h", 0.5),
    "━": ("h", 0.5),
    "┄": ("h", 0.5),
}

FONT_SIZE = CELL_W / 0.6  # monospace advance is ~0.6em
BASELINE = CELL_H * 0.76


def resolve(colour):
    if colour == DEFAULT_FG:
        return PALETTE["fg"]
    if colour == DEFAULT_BG:
        return PALETTE["bg"]
    if colour.startswith("#") and colour[1:].isdigit():
        index = int(colour[1:])
        if index < len(PALETTE["indexed"]):
            return PALETTE["indexed"][index]
    return colour if colour.startswith("#") else PALETTE["fg"]


def svg(capture, title="undertone"):
    pad, bar = 16, 34
    width = capture.cols * CELL_W
    height = capture.rows * CELL_H
    total_w, total_h = width + pad * 2, height + bar + pad * 2
    ground = PALETTE["bg"]
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" '
        f'width="{total_w}" height="{total_h}" viewBox="0 0 {total_w} {total_h}" '
        'font-family="ui-monospace, SFMono-Regular, \'JetBrains Mono\', \'DejaVu Sans Mono\', Menlo, Consolas, monospace">',
        # Everything is clipped to the window, so a run that reaches the last
        # column ends there - as it does in the terminal - instead of bleeding
        # over the rounded corner.
        f'<defs><clipPath id="window"><rect width="{total_w}" height="{total_h}" rx="10"/></clipPath></defs>',
        f'<g clip-path="url(#window)">',
        f'<rect width="{total_w}" height="{total_h}" fill="#11111b"/>',
        f'<rect width="{total_w}" height="{bar + pad}" fill="#181825"/>',
        f'<circle cx="{pad + 5}" cy="{pad + 5}" r="5" fill="#45475a"/>',
        f'<circle cx="{pad + 21}" cy="{pad + 5}" r="5" fill="#45475a"/>',
        f'<circle cx="{pad + 37}" cy="{pad + 5}" r="5" fill="#45475a"/>',
        f'<text x="{pad + 60}" y="{pad + 10}" font-size="12" fill="#6c7086">{escape(title)}</text>',
        f'<g transform="translate({pad},{pad + bar})">',
        f'<rect width="{width}" height="{height}" fill="{ground}"/>',
    ]

    # Backgrounds first, as runs, so the text sits on top of them.
    for y in range(capture.rows):
        x = 0
        while x < capture.cols:
            bg = capture.styles[y][x][1]
            start = x
            while x < capture.cols and capture.styles[y][x][1] == bg:
                x += 1
            if bg != DEFAULT_BG:
                out.append(
                    f'<rect x="{start * CELL_W}" y="{y * CELL_H}" '
                    f'width="{(x - start) * CELL_W}" height="{CELL_H}" '
                    f'fill="{resolve(bg)}"/>'
                )

    # The covers, in the cells the app placed them and at the pixels it sent.
    # A cover with no cell rectangle is drawn at its own pixel size.
    for row, col, rows, cols, width, height, data in capture.images.values():
        box_w = cols * CELL_W if cols else width
        box_h = rows * CELL_H if rows else height
        out.append(
            f'<image x="{col * CELL_W}" y="{row * CELL_H}" '
            f'width="{box_w}" height="{box_h}" '
            'preserveAspectRatio="none" '
            f'xlink:href="data:image/png;base64,{base64.b64encode(data).decode()}"/>'
        )

    # Then the rules, and then the glyphs, one <text> per run of equal style.
    for y in range(capture.rows):
        runs, start, style = [], None, None
        for x in range(capture.cols + 1):
            char = capture.chars[y][x] if x < capture.cols else None
            cell = capture.styles[y][x] if x < capture.cols else None
            kind = RULES.get(char)
            if kind:
                axis, offset = kind
                colour = resolve(cell[0])
                if axis == "v":
                    out.append(
                        f'<rect x="{x * CELL_W + CELL_W * offset - 0.5:.1f}" y="{y * CELL_H}" '
                        f'width="1" height="{CELL_H}" fill="{colour}"/>'
                    )
                else:
                    out.append(
                        f'<rect x="{x * CELL_W}" y="{y * CELL_H + CELL_H * offset:.1f}" '
                        f'width="{CELL_W}" height="1" fill="{colour}"/>'
                    )
            if kind or cell is None or cell != style:
                if start is not None and start < x:
                    runs.append((start, x, style))
                start, style = None, None
            if cell is not None and not kind and start is None:
                start, style = x, cell
        for start, end, style in runs:
            run = "".join(capture.chars[y][start:end])
            if not run.strip():
                continue
            fg, _, flags = style
            attrs = [
                f'x="{start * CELL_W}"',
                f'y="{y * CELL_H + BASELINE:.1f}"',
                f'font-size="{FONT_SIZE:.1f}"',
                f'textLength="{(end - start) * CELL_W}"',
                'lengthAdjust="spacingAndGlyphs"',
                f'fill="{resolve(fg)}"',
            ]
            if "bold" in flags:
                attrs.append('font-weight="bold"')
            if "dim" in flags:
                attrs.append('opacity="0.55"')
            if "italic" in flags:
                attrs.append('font-style="italic"')
            out.append(f'<text {" ".join(attrs)} xml:space="preserve">{escape(run)}</text>')

    out += ["</g>", "</g>", "</svg>", ""]
    return "\n".join(out)


def escape(text):
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def silent_mpv():
    """A `player.mpv` that plays for real and makes no sound.

    The null player reports a fixed 200 second track, so the bar would claim
    3:20 over a track the list says is 1:25. Real mpv knows the duration and
    moves the clock; the wrapper points it at a null audio device, so
    regenerating the screenshot does not play a track at whoever runs it.
    """
    real = shutil.which("mpv")
    if not real:
        return None
    wrapper = Path(tempfile.mkdtemp(prefix="undertone-mpv-")) / "mpv"
    wrapper.write_text(f'#!/bin/sh\nexec "{real}" --ao=null "$@"\n')
    wrapper.chmod(0o755)
    return wrapper


def track_count(frame):
    """How many tracks the open album has, from its own header."""
    match = re.search(r"(\d+) tracks?", frame)
    return int(match.group(1)) if match else 0


def open_a_busy_album(app, want=6, tries=10):
    """Open albums until one has a track list worth looking at.

    Which album the first row holds is the server's business: one run gave a
    one-track single, which draws an empty page. So walk the list until an
    album is long enough to show the list, the cursor and the duration column
    doing their job, and leave that one open.
    """
    for _ in range(tries):
        app.send("\r", 3)
        if track_count(app.text()) >= want:
            return True
        app.send("h", 1.5)  # back to the list
        app.send("j", 1.0)  # and on to the next album
    # Nothing was long enough (a small library, or the fixture's three sample
    # albums): take the first one rather than whichever the walk ended on.
    app.send("g", 1.0)
    app.send("\r", 3)
    return False


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "./target/release/undertone"
    # The README's copy comes from the public demo server, because a screenshot
    # of three albums called "Fixture One" says nothing about the app. That
    # library changes, so this output cannot be diffed in CI the way
    # docs/SCREENS.md is - regenerating gives whatever the demo has today.
    offline = "--fixture" in sys.argv
    if not shutil.which("chafa"):
        # The app encodes every cover with chafa, even in kitty mode; without
        # it the cover slots stay placeholders and the frame is not worth
        # looking at. `result/bin/undertone` from the flake wraps PATH for this.
        print("warning: chafa is not on PATH, covers will be placeholders", file=sys.stderr)

    env = {
        # ghostty speaks kitty graphics, so covers arrive as pixels rather than
        # as characters chafa picked for whatever font it found.
        "TERM": "xterm-kitty",
        "TERM_PROGRAM": "ghostty",
        "KITTY_WINDOW_ID": None,
        "TMUX": None,
        "ZELLIJ": None,
        "COLORTERM": "truecolor",
        "UNDERTONE_PASSWORD": os.environ.get("UNDERTONE_PASSWORD", "")
        or ("fixture" if offline else "demo"),
        # Fresh state and a fresh art cache: a leftover session file would open
        # the app on whatever screen the last run left it on.
        "XDG_CONFIG_HOME": tempfile.mkdtemp(prefix="undertone-shot-"),
        "XDG_STATE_HOME": tempfile.mkdtemp(prefix="undertone-shot-"),
        "XDG_CACHE_HOME": tempfile.mkdtemp(prefix="undertone-shot-"),
    }
    env = {k: v for k, v in env.items() if v is not None}

    fixture = None
    if offline:
        fixture, server = start_fixture()
        user = "demo"
    else:
        server = os.environ.get("UNDERTONE_SERVER", "https://demo.navidrome.org")
        user = os.environ.get("UNDERTONE_USER", "demo")

    player = silent_mpv()
    args = ["--server", server, "--username", user]
    if player:
        config = Path(env["XDG_CONFIG_HOME"]) / "undertone"
        config.mkdir(parents=True, exist_ok=True)
        (config / "config.toml").write_text(f'[player]\nmpv = "{player}"\n')
    else:
        args.append("--player=null")

    shots = []
    try:
        app = Capture(binary, args, env_extra=env)
        # The wall fills in one cover at a time as the worker encodes them, so
        # the wait has to cover the whole screen, not just the first frame.
        app.wait(8 if offline else 25)
        # The status line outlives its TTL in the frame it was drawn in - the
        # TTL only hides it at the next redraw, and an idle app draws none. So
        # the startup "config: /tmp/..." would sit in the middle of the bar for
        # ever. Down a row and back up is that redraw, and moves nothing.
        app.send("jk", 1.5)
        shots.append(("wall", svg(app)))

        # Then an album, playing: the bottom bar is the other half of the UI.
        # Every path through `open_a_busy_album` leaves an album open.
        open_a_busy_album(app)
        app.send("\r", 5 if player else 2)  # playback runs on the clock
        shots.append(("album", svg(app)))
        app.quit()
    finally:
        if fixture:
            fixture.terminate()

    for name, document in shots:
        path = Path(__file__).resolve().parent.parent / "docs" / f"screenshot-{name}.svg"
        path.write_text(document)
        print(f"wrote {path} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
