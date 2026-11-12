# Undertone

A Subsonic/Navidrome music client for very weak Linux hardware — Raspberry Pi 2
class ARM boards and slow laptops. Terminal UI, keyboard only, flat memory
footprint.

> **Status: early development.** Nothing usable yet. Design is settled; see
> `docs/DESIGN.md` once it lands, and the plan this project is being built from.

## Why

Covertone is a browser-based Subsonic client with a discovery-oriented feature
set. Undertone keeps the parts worth keeping — discovery, random-first browsing,
Auto DJ, Listen Later — and drops everything that needs a browser, a GPU, or a
daemon:

| Kept | Dropped |
|---|---|
| Discovery pane, Auto DJ, Listen Later, random-first sorting | Tabs, trivia game, AI integration |
| Subsonic library, playlists, favourites, search | Library rescans |
| Scrobbling (via the Subsonic `scrobble` endpoint) | Offline audio cache and downloads |
| Album art — real pixels via sixel/kitty | Accent-colour theming |
| Full keyboard control | Mouse, hover, rich text |

## Design in one paragraph

Audio plays through an `mpv` subprocess over JSON IPC. Album art is fetched
pre-scaled from the server, encoded to sixel (foot) or kitty graphics
(kitty/ghostty) by `chafa` in a background worker, cached on disk as final
escape bytes, and reconciled onto the screen by a painter that emits zero bytes
when nothing changed. There is no local library index: the server's search and
paginated list endpoints are the database. One 1 Hz timer, one event loop, no
polling.

## Requirements

- A terminal with image support: **foot** (sixel), **kitty** or **ghostty**
  (kitty graphics). Anything else degrades to Unicode art.
- `mpv` and `chafa` at runtime.
- NixOS: the flake provides both.

## Install

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

[player]
mpv = "mpv"
```

## Keys

```
j / k      move down / up      Ctrl-d/u  page down / up
h / l      left / right (grid) · h back
w          list / grid toggle  g / G   first / last
Enter      open album · play track    p / Space  play / pause
> / <      next / prev         f / b   seek ±10s
s / r      shuffle / repeat    +/-     volume      m  mute
Q          queue               ?       help        Esc back
S / R      change sort / reload         q   quit
```

The album list pages as you scroll: a hundred albums at a time, fetched when
the cursor gets within a screen of the end, and a short page means the end of
the list. No count is ever held in memory beyond what you have scrolled past.

The bottom bar shows the playing track with its cover, album, container and
bitrate, a thin progress rule, and a preview of what plays next.

## Development

```bash
nix develop
cargo build
cargo test
cargo run -- --player=null --art=off   # no audio, no images
```

## Licence

GPL-3.0-or-later, matching Covertone.
