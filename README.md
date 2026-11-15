# Undertone

A lightweight Subsonic/Navidrome music client built for speed. Terminal UI, keyboard only, small
memory footprint.

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

| key | default | what it does |
| --- | ------- | ------------ |
| `server` | - (required) | Subsonic/Navidrome base URL |
| `username` | - (required) | account name |
| `password_cmd` | - | argv whose stdout is the password; wins over `password` |
| `password` | - | the password itself; bypassed by `UNDERTONE_PASSWORD` |
| `scrobble` | `true` | report plays, and "now playing", to the server |
| `auto_dj` | `true` | append similar tracks when the queue is nearly out (needs the server's similar-songs endpoint) |
| `art.palette` | `partial` | colours for every cover on every screen: `grayscale` (2), `partial` (16) or `full` (24-bit) |
| `art.cache_mb` | `20` | cap for the cache of encoded escape blobs; the least recently used are dropped past it. The downloaded covers next to them are not counted against it |
| `art.backdrop` | `#000000` | colour painted over a removed cover; set it to your terminal's background |
| `player.mpv` | `mpv` | mpv binary; point it at an absolute path if it is not on `PATH` |

The art backend is not configurable: `undertone` asks the terminal and uses
sixel (foot), kitty graphics (kitty, ghostty, WezTerm) or, where neither is
available, chafa's Unicode art. `art.palette` is the one art knob that costs
bytes. `partial` keeps a cover at ~11 KB on a phone-sized screen and is what
makes the wall scroll smoothly; `full` is 24-bit and costs roughly 7–10× the
bytes; `grayscale` is two colours and halves them again.

Precedence is command line, then environment, then this file, then the
built-in defaults. The environment variables are `UNDERTONE_SERVER`,
`UNDERTONE_USERNAME`, `UNDERTONE_PASSWORD` and `UNDERTONE_CONFIG`; the flags that
override settings are `--server`, `--username`, `--player=mpv|null`,
`-c/--config`, `--debug` and `--clear-cache`.

Other paths: the art cache lives in `$XDG_CACHE_HOME/undertone` - `--clear-cache`
wipes it and exits - and the queue, sort order and list layouts are restored from
`$XDG_STATE_HOME/undertone/session.json`. `--debug` writes a key log there for
tracking down a phantom keypress.

## Keys

```
1-6        rail: albums · artists · starred · later · lists · queue
j / k      move down / up      Ctrl-d/u  page down / up
h / l      left / right (wall) · back / forward
w          list / wall toggle  g / G   first / last
Enter      open album · play track    p / Space  play / pause
> / <      next / prev         f / b   seek ±10s
s / r      shuffle / repeat    +/-     volume      m  mute
/          search              ?       help        Esc back
a / A      queue after now / at the end   z  save / unsave album
x / J / K  queue: remove / move down / move up
*          star / unstar album
o / i      album / artist of the selected queue track
i          artist of the open album
Tab        switch pane (artist)  t     albums / songs (artist)
S / R      change sort / reload         q   quit
```

The views are the numbered rail entries only; the old letter shortcuts for
artists, starred and listen later are gone, so `1`–`6` is the whole
navigation vocabulary. `/` opens search, and the rail carries a `Search` row
purely as a reminder.

`/` opens a search box in the header: type, Enter runs the query, Esc closes
it. Results are one list - albums first, then songs - with each row saying which
it is; Enter opens an album or plays the song. The query runs on Enter rather
than on every keystroke, because the client is synchronous and a request per
letter would freeze the interface mid-word.

Playlists open like albums: `5` lists them with their size, Enter shows the
tracks, Enter plays from that point with the rest of the playlist queued behind
it.

`S` cycles the sort of the list you are looking at: albums run
random → recent → most played → a-z, artists run most albums → a-z → most
played. Artist "most played" is a ranking built from the server's frequent
album list, since the Subsonic artist index carries no play counts; artists
with no frequent albums come last, alphabetically.

`h` and `Esc` go back to the screen you came from, not always to the library:
an album opened from an artist page returns to that artist, and an album opened
from favourites returns to favourites. `l` walks that history forward again,
including artist to artist when you followed similar artists; each page comes
back with its panes, cursor and scroll where you left them.

The album list pages as you scroll: a hundred albums at a time, fetched when
the cursor gets within a screen of the end, and a short page means the end of
the list. No count is ever held in memory beyond what you have scrolled past.

The bottom bar is always there, in three columns: the playing track with its
elapsed and total time either side of a progress rule, plus the album and
container/bitrate; the play state and the keys that work on this screen; and
what plays next. Sorting lives in each view's own header, next to its name.

## Development

```bash
nix develop
cargo build
cargo test
cargo run -- --player=null             # no audio
```
