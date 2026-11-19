# Undertone

A lightweight Subsonic/Navidrome music client built for speed. Terminal UI, keyboard-only controls, small
memory footprint.

![Undertone's library: a wall of album covers with the rail on the left, and the bar
along the bottom saying what is playing, the keys that work here and what plays
next.](docs/screenshot-wall.svg)

![An album open: its cover, its tracks, and the same bar with elapsed and total time
either side of a progress rule.](docs/screenshot-album.svg)

Every other screen - artists, search, the queue - is in
[docs/SCREENS.md](docs/SCREENS.md).

## Requirements

- A terminal with image support: **foot** (sixel), **kitty** or **ghostty**
  (kitty graphics). Anything else degrades to Unicode art.
- `mpv` and `chafa` at runtime.
- NixOS: the flake provides both.

## Run with the Nix package manager

```bash
nix run github:dbeley/undertone
```

Or with the NixOS module:

```nix
{
  inputs.undertone.url = "github:dbeley/undertone";
  imports = [ inputs.undertone.nixosModules.default ];
  programs.undertone.enable = true;
  # settings go to /etc/undertone/config.toml (no password there)
}
```

## Configuration

Read from `$XDG_CONFIG_HOME/undertone/config.toml` (usually
`~/.config/undertone/config.toml`), falling back to
`/etc/undertone/config.toml`. `-c/--config PATH` or `UNDERTONE_CONFIG` picks a
file somewhere else. Unknown keys are a startup error, so a typo tells you
instead of being silently ignored.

Every key, with its default - a real file can be shorter, but nothing is
missing here:

```toml
# ── server ────────────────────────────────────────────────────────────────
server   = "https://music.example.org"
username = "david"

# Password: prefer a command that prints it on stdout; `password` works too and
# UNDERTONE_PASSWORD overrides it (but not the command). Do not put either in
# /etc, which the NixOS module renders world-readable.
password_cmd = ["pass", "show", "navidrome"]
# password   = "hunter2"

# ── behaviour ─────────────────────────────────────────────────────────────
scrobble = true    # report plays to the server                  (default: true)
auto_dj  = true    # top the queue up with similar tracks        (default: true)

# ── album art ─────────────────────────────────────────────────────────────
[art]
palette  = "partial"   # grayscale | partial | full, for every cover
cache_mb = 20          # cap for the encoded blobs (covers are not counted)
backdrop = "#000000"   # painted over a cover being removed (sixel only)

# ── playback ──────────────────────────────────────────────────────────────
[player]
mpv = "mpv"            # binary name or absolute path
```

## Keyboard Shortcuts

```
1-6        navigation: albums · artists · starred · later · lists · queue
j / k      move down / up      Ctrl-d/u  page down / up
h / l      left / right (wall) · back / forward
w          list / wall toggle  g / G   first / last
Enter      open album · play track    p / Space  play / pause
> / <      next / prev         f / b   seek ±10s
s / r      shuffle / repeat    +/-     volume      m  mute
/          search              ?       help        Esc back
a / A      queue after now / at the end   z  save / unsave album
x / C      queue: remove one / clear all
J / K      queue: move down / move up
*          star / unstar album
o / i      album / artist of a queue or search row
i          artist of the open album
Tab        switch pane (artist)  t     albums / songs (artist)
S / R      change sort / reload         q   quit
```

## Development

```bash
nix develop
cargo build
cargo test
cargo run -- --player=null                                # no audio
python3 scripts/screenshot.py ./target/release/undertone  # redraw docs/screenshot.svg
```
