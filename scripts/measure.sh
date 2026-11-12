#!/usr/bin/env bash
# Measure the real binary: time to first frame, idle RSS, idle output.
#
# The CI tests in tests/perf.rs guard the algorithms; this measures the
# process, which needs a terminal and a release build. Run it on the machine
# you care about — a Pi 2 will give very different numbers from a laptop, and
# that difference is the whole point.
#
#   ./scripts/measure.sh                       # against the demo server
#   ./scripts/measure.sh --player=null --art=off
#
set -euo pipefail

BIN=${BIN:-./target/release/undertone}
SERVER=${SERVER:-https://demo.navidrome.org}
USER_=${USER_:-demo}
PASSWORD=${UNDERTONE_PASSWORD:-demo}

if [ ! -x "$BIN" ]; then
    echo "no binary at $BIN — run: nix develop -c cargo build --release" >&2
    exit 1
fi

exec python3 - "$BIN" "$SERVER" "$USER_" "$PASSWORD" "$@" <<'PY'
import fcntl, os, pty, select, struct, sys, termios, time

binary, server, user, password = sys.argv[1:5]
extra = sys.argv[5:]

env = dict(os.environ)
env["UNDERTONE_PASSWORD"] = password
env["TERM"] = "xterm-256color"
argv = [binary, "--server", server, "--username", user] + extra

pid, fd = pty.fork()
if pid == 0:
    os.execve(binary, argv, env)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

def rss_kb(field):
    try:
        with open(f"/proc/{pid}/status") as handle:
            for line in handle:
                if line.startswith(field):
                    return int(line.split()[1])
    except OSError:
        pass
    return None

start = time.time()
first_frame = None
idle_start = None
idle_bytes = 0

while True:
    timeout = 0.05
    readable, _, _ = select.select([fd], [], [], timeout)
    if readable:
        try:
            data = os.read(fd, 262144)
        except OSError:
            break
        if not data:
            break
        if first_frame is None and data:
            first_frame = time.time() - start
        if idle_start is not None:
            idle_bytes += len(data)
    now = time.time() - start
    # Settle for two seconds after the first frame, then watch for one second
    # of silence — that is what "idle" means here.
    if first_frame is not None and idle_start is None and now > first_frame + 3:
        idle_start = now
    if idle_start is not None and now > idle_start + 5:
        break

rss = rss_kb("VmRSS:")
peak = rss_kb("VmHWM:")
os.write(fd, b"q")
time.sleep(0.5)
try:
    os.waitpid(pid, 0)
except ChildProcessError:
    pass

print(f"time to first frame : {first_frame * 1000:.0f} ms" if first_frame else "time to first frame : n/a")
print(f"idle RSS            : {rss / 1024:.1f} MB" if rss else "idle RSS            : n/a")
print(f"peak RSS (VmHWM)    : {peak / 1024:.1f} MB" if peak else "peak RSS            : n/a")
print(f"idle output (5 s)   : {idle_bytes} bytes")
PY
