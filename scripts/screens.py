#!/usr/bin/env python3
"""Regenerate docs/SCREENS.md from the real binary.

There is no graphical terminal to screenshot here, and a screenshot of a
terminal is just text anyway. This drives the app through a pty against the
fixture server and writes down what each view actually renders, so the frames in
the document cannot drift from the code.

    python3 scripts/screens.py ./target/release/undertone
"""

import fcntl
import os
import pty
import select
import struct
import sys
import tempfile
import termios
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from degradation import SYMBOLS_TERM, Screen, start_fixture  # noqa: E402

COLS, ROWS = 104, 30


class App:
    def __init__(self, binary, args, state_dir=None):
        env = dict(os.environ)
        env.setdefault("UNDERTONE_PASSWORD", "fixture")
        # The terminal decides the art protocol, and the protocol decides how
        # many cells a cover slot takes, so the frames differ between foot,
        # ghostty and a plain xterm - in ways that have nothing to do with what
        # this document is for. Present as a symbols terminal everywhere: it is
        # the floor the app falls back to, and it is what draws the placeholder
        # the frames show.
        for key, value in SYMBOLS_TERM.items():
            if value is None:
                env.pop(key, None)
            else:
                env[key] = value
        # A throwaway state directory: otherwise a session saved by a real run
        # is restored, and the frames depend on where you last were. The config
        # directory comes along, because the bar names the file it loaded and
        # that path is different on every machine.
        if state_dir:
            env["XDG_STATE_HOME"] = state_dir
            env["XDG_CACHE_HOME"] = state_dir
            env["XDG_CONFIG_HOME"] = state_dir
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(binary, [binary, *args], env)
        fcntl.ioctl(
            self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0)
        )
        self.screen = Screen(COLS, ROWS)

    def wait(self, seconds=1.2):
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
                self.screen.feed(data)
        return self.screen.text()

    def send(self, keys, seconds=1.2):
        os.write(self.fd, keys.encode())
        return self.wait(seconds)

    def quit(self):
        try:
            os.write(self.fd, b"q")
            self.wait(0.5)
            os.waitpid(self.pid, 0)
        except (OSError, ChildProcessError):
            pass


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "./target/release/undertone"
    # No covers: the fixture's art is a gradient chafa turns into characters
    # picked from whatever font it detects, which differs between machines.
    # With 404s the cover slots keep their placeholder, so the frames are the
    # same everywhere and CI can diff them. Art is covered by the degradation
    # matrix, which asserts on protocols rather than on glyphs.
    fixture, server = start_fixture(["--no-covers"])

    shots = []
    try:
        app = App(
            binary,
            ["--player=null", "--server", server, "--username", "demo"],
            state_dir=tempfile.mkdtemp(prefix="undertone-screens-"),
        )
        app.wait(6)
        shots.append(("The library", "What the app opens on: a wall of covers, with the rail and the three-column bar.", app.wait(0.5)))

        app.send("w", 2.5)  # rows
        shots.append(
            (
                "The list",
                "`w` turns any wall back into rows; each list keeps its own choice.",
                app.wait(0.6),
            )
        )
        app.send("w", 2.5)  # back to the wall

        app.send("\r", 3)  # open the first album
        shots.append(("An album", "The album's cover and its tracks; the bar keeps playing state.", app.wait(0.5)))

        app.send("h", 1.5)
        app.send("/", 0.4)
        for ch in "fixture":
            app.send(ch, 0.05)
        app.send("\r", 2.5)
        shots.append(("Search", "One flat result list: albums first, then songs, each row labelled.", app.wait(0.5)))

        app.send("h", 1.5)
        app.send("5", 2)  # playlists
        shots.append(("Playlists", "The server's playlists, with their size.", app.wait(0.5)))

        app.send("2", 2.5)  # artists
        shots.append(("Artists", "Artists as a list; `w` turns this into a wall of covers.", app.wait(0.5)))

        app.send("\r", 3)  # open the first artist
        shots.append(
            (
                "An artist",
                "Three panes: albums, similar artists, biography. Tab moves between them.",
                app.wait(0.6),
            )
        )

        app.send("t", 2)  # the left pane's other tab
        shots.append(
            (
                "An artist, top songs",
                "The left pane has two tabs: albums and the artist's most played tracks.",
                app.wait(0.5),
            )
        )

        app.send("6", 1.5)  # queue
        shots.append(("The queue", "What plays next; Enter starts from the cursor.", app.wait(0.5)))
        app.quit()
    finally:
        fixture.terminate()

    lines = [
        "# Screens",
        "",
        "Rendered by `scripts/screens.py` from the real binary driving a pty",
        "against `scripts/fixture_server.py`, so what is written here is what the",
        "interface draws. Regenerate with",
        "`python3 scripts/screens.py ./target/release/undertone`.",
        "",
        "Covers are shown as their placeholder slot: the fixture server serves no",
        "cover art, so these frames are identical on every machine and CI can",
        "diff them. The app is started as a plain `xterm-256color`, which pins the",
        "art protocol too: a pixel terminal gives a cover a slightly different",
        "number of cells, and these frames are about layout, not about pixels.",
        "What the art itself looks like in each protocol is in",
        "`docs/COMPATIBILITY.md`, and is asserted there rather than drawn here.",
        "",
    ]
    for title, note, frame in shots:
        lines += [f"## {title}", "", note, "", "```", frame, "```", ""]

    out = os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs", "SCREENS.md"
    )
    with open(out, "w") as handle:
        handle.write("\n".join(lines))
    print(f"wrote {out} ({len(shots)} frames)")


if __name__ == "__main__":
    main()
