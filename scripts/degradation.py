#!/usr/bin/env python3
"""Run the interface through the terminal shapes it claims to support.

Each row starts the real binary in a pty, waits for the first frame, and checks
what the screen actually shows: the right art protocol for the setting, a
usable layout at that size, and no panic. Output is a table you can paste into
the docs.

    python3 scripts/degradation.py ./target/release/undertone

By default it talks to `scripts/fixture_server.py`, so it needs no network and
finishes in seconds; pass `--live` to run the same rows against a real server
(`UNDERTONE_SERVER`, `UNDERTONE_USER`, `UNDERTONE_PASSWORD`).
"""

import fcntl
import glob
import os
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

COLS, ROWS = 104, 30

SIXEL = re.compile(rb"\x1bP[0-9;]*q")
KITTY = re.compile(rb"\x1b_G")
# The glyph a cover slot shows before its art arrives. Its absence on a row
# with a cover means something was painted there.
PLACEHOLDER = "▚"


def split_complete(text):
    """Split into a part that can be parsed and a trailing incomplete escape."""
    start = text.rfind("\x1b")
    if start == -1:
        return text, ""
    tail = text[start:]
    if len(tail) == 1:
        return text[:start], tail
    kind = tail[1]
    if kind == "[":
        if re.match(r"\x1b\[[0-9;?]*[A-Za-z]", tail):
            return text, ""
        return text[:start], tail
    if kind == "]":
        if re.search(r"\x07|\x1b\\", tail):
            return text, ""
        return text[:start], tail
    return text, ""


class Screen:
    """Just enough of a terminal to know what is on it."""

    def __init__(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.grid = [[" "] * cols for _ in range(rows)]
        self.x = self.y = 0
        self.pending = b""

    def feed(self, data):
        # An escape sequence can be split across two reads; hold the incomplete
        # tail until the rest arrives, or its parameters land in the cells.
        self.pending += data
        text = self.pending.decode("utf-8", "ignore")
        complete, tail = split_complete(text)
        self.pending = tail.encode()

        i = 0
        while i < len(complete):
            ch = complete[i]
            if ch == "\x1b":
                m = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", complete[i:])
                if m:
                    self._csi(m.group(1), m.group(2))
                    i += m.end()
                    continue
                m = re.match(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", complete[i:])
                if m:
                    i += m.end()
                    continue
                m = re.match(r"\x1b.", complete[i:])
                i += m.end() if m else 1
                continue
            if ch == "\r":
                self.x = 0
            elif ch == "\n":
                self.y = min(self.y + 1, self.rows - 1)
            elif ch >= " ":
                if self.x < self.cols and self.y < self.rows:
                    self.grid[self.y][self.x] = ch
                self.x += 1
                if self.x >= self.cols:
                    self.x, self.y = 0, min(self.y + 1, self.rows - 1)
            i += 1
        return self

    def _csi(self, params, final):
        nums = (
            [int(p) if p.isdigit() else 0 for p in params.split(";")] if params else []
        )
        if final in "Hf":
            row = (nums[0] if nums and nums[0] else 1) - 1
            col = (nums[1] if len(nums) > 1 and nums[1] else 1) - 1
            self.y, self.x = min(row, self.rows - 1), min(col, self.cols - 1)
        elif final == "J" and (not nums or nums[0] in (2, 3)):
            self.grid = [[" "] * self.cols for _ in range(self.rows)]

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.grid)


def run(binary, args, cols=COLS, rows=ROWS, seconds=7.0, env_extra=None):
    env = dict(os.environ)
    env.setdefault("TERM", "xterm-256color")
    env.update(env_extra or {})
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(binary, [binary, *args], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    raw = b""
    end = time.time() + seconds
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if ready:
            try:
                chunk = os.read(fd, 262144)
            except OSError:
                break
            if not chunk:
                break
            raw += chunk

    screen = Screen(cols, rows).feed(raw)
    alive = True
    try:
        os.write(fd, b"q")
        time.sleep(0.4)
    except OSError:
        alive = False
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    return raw, screen.text(), alive


def start_fixture():
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixture_server.py")
    process = subprocess.Popen(
        [sys.executable, script], stdout=subprocess.PIPE, text=True
    )
    assert process.stdout is not None
    port = int(process.stdout.readline().strip())
    return process, f"http://127.0.0.1:{port}"


def main():
    args_in = [a for a in sys.argv[1:] if not a.startswith("--")]
    live = "--live" in sys.argv
    binary = args_in[0] if args_in else "./target/release/undertone"

    fixture = None
    if live:
        server = os.environ.get("UNDERTONE_SERVER", "https://demo.navidrome.org")
        user = os.environ.get("UNDERTONE_USER", "demo")
    else:
        fixture, server = start_fixture()
        user = "demo"
        os.environ.setdefault("UNDERTONE_PASSWORD", "fixture")

    library = ["--server", server, "--username", user]
    # Layout rows need no server: a closed port keeps them hermetic while still
    # exercising the empty-list rendering.
    local = ["--server", "http://127.0.0.1:9", "--username", "demo"]

    rows = []

    def check(
        name, args, want, cols=COLS, height=ROWS, expect_blobs=None, env_extra=None
    ):
        cache = tempfile.mkdtemp(prefix="undertone-degradation-")
        raw, text, alive = run(
            binary,
            args,
            cols,
            height,
            env_extra={
                "XDG_CACHE_HOME": cache,
                "XDG_STATE_HOME": os.path.join(cache, "state"),
                **(env_extra or {}),
            },
        )

        blobs_ok = True
        if expect_blobs is not None:
            blobs = glob.glob(os.path.join(cache, "undertone", "blobs", "*"))
            blobs_ok = bool(blobs) == expect_blobs
            if not blobs_ok:
                print(f"{name}: expected {expect_blobs} thumbnails, found {len(blobs)}")

        ok = want(raw, text) and alive and blobs_ok
        rows.append((name, "ok" if ok else "FAIL"))
        if not ok:
            print(f"--- {name} failed\n{text}\n---")
        return ok

    try:
        check(
            "sixel, 104x30",
            library + ["--art=sixel", "--player=null"],
            lambda raw, text: bool(SIXEL.search(raw)) and "Albums · random" in text,
            expect_blobs=True,
        )
        check(
            "kitty, 104x30",
            library + ["--art=kitty", "--player=null"],
            lambda raw, text: bool(KITTY.search(raw)) and "Albums" in text,
            expect_blobs=True,
        )
        check(
            "symbols, 104x30",
            library + ["--art=symbols", "--player=null"],
            # Symbol art is cells: no graphics escape, and the placeholder is
            # gone because a real cover took its place. Which glyphs chafa picks
            # depends on the font it thinks it has, so the placeholder's absence
            # is the invariant, not any particular character.
            lambda raw, text: not SIXEL.search(raw)
            and not KITTY.search(raw)
            and "▚" not in text
            and "Fixture One" in text,
            expect_blobs=True,
        )
        check(
            "no art at all",
            library + ["--no-art", "--player=null"],
            lambda raw, text: not SIXEL.search(raw)
            and not KITTY.search(raw)
            and "▚" in text,  # the cover slot keeps its placeholder
            expect_blobs=False,
        )
        check(
            "narrow, 60x24",
            local + ["--art=symbols", "--player=null"],
            lambda raw, text: "Albums" in text and "Albums   1" not in text,
            cols=60,
            height=24,
        )
        check(
            "too small, 18x4",
            local + ["--art=symbols", "--player=null"],
            lambda raw, text: "small" in text.lower() or "resize" in text.lower(),
            cols=18,
            height=4,
        )
        check(
            "dead server",
            local + ["--art=symbols", "--player=null"],
            lambda raw, text: "cannot reach" in text and "Albums" in text,
        )
        # A config naming a player that does not exist. PATH starvation cannot
        # test this under the Nix wrapper, which puts mpv back on PATH itself.
        config = os.path.join(
            tempfile.mkdtemp(prefix="undertone-config-"), "config.toml"
        )
        with open(config, "w") as handle:
            handle.write(
                f'server = "{server}"\n'
                f'username = "{user}"\n'
                "[player]\n"
                'mpv = "/nonexistent/mpv"\n'
            )
        check(
            "missing player",
            ["--config", config, "--art=symbols"],
            lambda raw, text: "mpv unavailable" in text and "Albums" in text,
        )
    finally:
        if fixture:
            fixture.terminate()

    width = max(len(name) for name, _ in rows) + 2
    print(f"{'case'.ljust(width)}result")
    for name, result in rows:
        print(f"{name.ljust(width)}{result}")
    return 0 if all(result == "ok" for _, result in rows) else 1


if __name__ == "__main__":
    sys.exit(main())
