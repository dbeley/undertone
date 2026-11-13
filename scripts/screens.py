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
from degradation import Screen, start_fixture  # noqa: E402

COLS, ROWS = 104, 30


class App:
    def __init__(self, binary, args, state_dir=None):
        env = dict(os.environ)
        env.setdefault("TERM", "xterm-256color")
        env.setdefault("UNDERTONE_PASSWORD", "fixture")
        # A throwaway state directory: otherwise a session saved by a real run
        # is restored, and the frames depend on where you last were.
        if state_dir:
            env["XDG_STATE_HOME"] = state_dir
            env["XDG_CACHE_HOME"] = state_dir
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
    fixture, server = start_fixture()

    shots = []
    try:
        app = App(
            binary,
            ["--player=null", "--art=symbols", "--server", server, "--username", "demo"],
            state_dir=tempfile.mkdtemp(prefix="undertone-screens-"),
        )
        app.wait(6)
        shots.append(("The library", "Album list, with the rail, the list's own header and the three-column bar.", app.wait(0.5)))

        app.send("w", 2.5)  # the wall
        shots.append(
            (
                "The wall",
                "`w` turns any album list into a wall of covers; each list keeps its own choice.",
                app.wait(0.6),
            )
        )
        app.send("w", 2.5)  # back to rows

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
        "Covers are characters here because the fixture runs with `--art=symbols`;",
        "on a terminal with sixel or kitty support they are images in the same",
        "places. See `docs/COMPATIBILITY.md`.",
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
