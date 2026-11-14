# Undertone

A lightweight Subsonic/Navidrome music clientbuilt for speed. Terminal UI, keyboard only, small memory
footprint.

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

`~/.config/undertone/config.toml`, falling back to `/etc/undertone/config.toml`:

```toml
server = "https://music.example.org"
username = "david"
password_cmd = ["pass", "show", "navidrome"]

scrobble = true
auto_dj = true

[art]
mode = "auto"
thumb_colors = 16
# Painted over a cover being removed; set it to your terminal's background.
backdrop = "#000000"

[player]
mpv = "mpv"
```

## Keys

```
1-6        rail: albums · artists · starred · later · lists · queue
j / k      move down / up      Ctrl-d/u  page down / up
h / l      left / right (wall) · h back
w          list / wall toggle  g / G   first / last
Enter      open album · play track    p / Space  play / pause
> / <      next / prev         f / b   seek ±10s
s / r      shuffle / repeat    +/-     volume      m  mute
/          search              ?       help        Esc back
5          playlists
L          listen later        A       artists     F  starred
a          save / unsave album *       star / unstar
S / R      change sort / reload         q   quit
```

`/` opens a search box in the header: type, Enter runs the query, Esc closes
it. Results are one list — albums first, then songs — with each row saying which
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
from favourites returns to favourites.

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
cargo run -- --player=null --art=off   # no audio, no images
```
