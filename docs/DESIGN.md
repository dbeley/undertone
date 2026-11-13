# Undertone — Implementation Plan

**Goal:** A Subsonic/Navidrome music client for very weak Linux hardware (Raspberry Pi 2 class, slow ARM laptops) that keeps Covertone's discovery-oriented feature set while staying in a flat, small memory footprint with minimal CPU.

**Architecture:** Rust + ratatui single-process TUI, with `mpv` in a subprocess speaking JSON IPC for audio. Album art is rendered to sixel (foot) or kitty graphics (kitty/ghostty) by `chafa`, batched in the background, cached as final escape bytes on disk, and reconciled onto the screen by a placement painter that emits zero bytes when nothing changed.

**Tech stack:** Rust 2024, ratatui + crossterm, ureq + rustls, serde, toml, clap; `chafa` and `mpv` as external runtime tools supplied by Nix.

---

## Status

Phases 0–5 complete and merged (flake, CI, Subsonic transport, typed endpoints, TTL cache, player trait, mpv IPC, queue, scrobbler, render pipeline, a runnable TUI, album paging, Listen Later, Auto DJ, session restore). Verified live against Navidrome 0.64.2, with real mpv 0.42 through a null sink, with real chafa 1.18 output fixtures, and by running the TUI itself in a 100x30 pty. Phase 6: the resource harness and the error-path suite are done (see `docs/PERFORMANCE.md`); the degradation matrix and the remaining docs are outstanding. Discovery was dropped by request until the app is more complete. Numbers in this document were measured on 2 vCPU of an Intel N100; Pi 2 projections are estimates.

---

## Locked decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | Audio = mpv subprocess over JSON IPC | All codecs, gapless, correct seek, crash isolation. **Budget cost 66 MB — see D1-open.** |
| D1a | Playback is a playlist window of three, followed by `playlist-pos` | mpv gaplessly advances inside a window, which removes the inter-track gap that per-track `loadfile` cannot. The app maps mpv's playlist index back to a queue position, so `next`/`previous`/shuffle still belong to the app. Window growth is bounded (one URL string per entry) and topped up two entries ahead. |
| D2 | Language = Rust | No GC, flat RSS, clean armv7/aarch64 cross-compile. |
| D3 | No local metadata index | Server-side `search3` + paginated `albumList2` + TTL RAM cache cover it. Avoids schema/versioning/staleness bugs. |
| D4 | Scope = full Covertone identity | Discovery pane, Auto DJ, Listen Later, random-first sorting. |
| D5 | Scrobble = Subsonic `scrobble` only | Navidrome relays to Last.fm/ListenBrainz. No direct API clients. |
| D6 | Images = sixel primary (foot), kitty secondary (kitty/ghostty), symbols floor | The weak machine uses foot exclusively; sixel is the real target. |
| D7 | Art renderer = `chafa` subprocess, batched | One dependency covers decode + quantise + three output formats. Batching amortises spawn cost. |
| D8 | Album list = thumbnail-per-row | User requirement; made affordable by 16-colour thumbnails (measured, see below). |
| D9 | No TTY/framebuffer-console support | Terminal emulators only (foot, ghostty, kitty). Removes the CP437/16-colour/truecolor ladder. |
| D10 | NixOS first class, like Covertone | Flake with devShell + package + overlay + NixOS module. |

### D1-open — the audio backend memory fork

Measured, not estimated: **mpv floors at 66 MB** even with every legal flag (`--no-config --vo=null --ao=null --no-video --no-osc --no-osd-bar --no-input-default-bindings --no-terminal --cache=no --demuxer-max-bytes=2MiB`). My earlier 25–40 MB figure was wrong.

| Option | Player RSS | Total app | Work | Risk |
|---|---|---|---|---|
| A. mpv subprocess | **66 MB (measured)** | ~85 MB | low | none, proven |
| B. Native Rust (symphonia + cpal) | est. 10–20 MB (unmeasured) | ~30 MB | high — HTTP range streaming, seek, gapless, device handling | medium |
| C. mpg123 + server MP3 transcode | est. 10–15 MB (unmeasured) | ~30 MB | medium | quality loss, transcode coupling |
| D. Ship A behind a `Player` trait, swap later | as A | as A | low, opens B/C | none |

**Recommendation: D.** Ship mpv, keep `Player` a trait from the first commit so B or C is a contained replacement rather than a rewrite. Revisit if 85 MB proves unacceptable on the device.

---

## Measured data

Hardware: 2 vCPU of an Intel N100, virtualised, NixOS 26.11, 3.9 GB RAM. Pi 2 projections assume **5–8× slower per core** (assumption — must be validated on device).

### chafa encode cost and blob size (sixel, 500 px source)

| Cell box | Output px | 16 colours | 240 colours | 256 colours |
|---|---|---|---|---|
| 3x3 | 30x40 | 717 B | 5.9 KB | 6.2 KB |
| 4x3 | 40x40 | **963 B** | 7.4 KB | 7.8 KB |
| 6x3 | 60x60 | 1.7 KB | 11.0 KB | 11.5 KB |
| 8x6 | 80x80 | 2.8 KB | 16.2 KB | 16.8 KB |
| 20x10 | 200x200 | 11.6 KB | 62.4 KB | 64.2 KB |
| 32x16 | 320x320 | — | 138.9 KB | 142.6 KB |

Encode wall time: ~24 ms at thumbnail sizes, ~30 ms at 200 px, ~37 ms at 320 px. Dominated by process spawn (18.7 ms), not by encode.

**The decisive number: 16 colours costs 8× less than 256.** A 12-thumbnail album screen is ~11.5 KB at 16 colours versus ~94 KB at 256. At 16 colours, per-row sixel art is cheap enough to repaint — which is what makes D8 viable on foot. It also happens to be the durdraw/ANSI-art look.

### chafa process cost

| Metric | Value |
|---|---|
| spawn + `--version` | 18.7 ms |
| peak RSS | ~18 MB (transient) |
| batched 3 images, one spawn | 26.6 ms |
| 3 separate spawns | 68.6 ms |

Batching cuts per-image overhead ~2.5×. Twelve thumbnails ≈ one spawn + ~9 encodes ≈ 70 ms here, ~0.4–0.55 s on a Pi 2 — acceptable as a one-time background warm-up per screenful.

### chafa geometry (gotcha)

`--font-ratio` does **not** change sixel output geometry. Output pixels are derived from `--size` (cells × 10 × 20) and the source aspect ratio, letterboxed. Consequences:

- Do not predict pixel dimensions; **parse them from the sixel header** (`\x1bP…q"1;1;W;H`) and store them in the cache metadata.
- Choose target pixel sizes as multiples of 10 (width) and 20 (height) to minimise dead space, then reserve `ceil(px/cell_px)` terminal cells.
- Request art from Subsonic already scaled (`getCoverArt?size=N`) so chafa decodes a small JPEG: a 250 px source measured 21 ms versus 24 ms for 500 px, and the gap widens once spawn is amortised.

### mpv

| Configuration | Peak RSS |
|---|---|
| baseline (`--no-config --vo=null --ao=null --no-video`) | 71.9 MB |
| all legal flags | **66.3 MB** |
| `--cache=no --demuxer-thread=no` | 73.4 MB (worse) |
| 12 s playback | 71.2 MB |

`--no-scripts` is not a valid mpv 0.41 flag; use `--load-scripts=no`.

### Art pipeline measured end to end (`cargo run --release --example art_bench`)

12 album thumbnails at 4x3 cells, 16 colours, one 250 px source image:

| | Time | Notes |
|---|---|---|
| cold | **149 ms** | 2 chafa invocations (batch limit 8), 11 460 bytes total |
| warm | **4.2 ms** | all 12 from the blob cache |
| idle painter plan | **1.4 µs** | emits 0 ops |

Average blob: **955 bytes per thumbnail**, matching the 963 B measured for a single 4x3 render. A full 12-thumbnail screen is 11.5 KB — the number that makes per-row sixel art viable on foot.

### chafa symbols (fallback)

| Cell box | Bytes |
|---|---|
| 4x2 | 173 B |
| 8x4 | 646 B |
| 20x10 | 3.5 KB |

---

## Budget

| Metric | Target | Hard fail | Source |
|---|---|---|---|
| UI process RSS | < 25 MB | 40 MB | TBD by harness |
| Total RSS with mpv | < 85 MB | 100 MB | 66 MB measured + UI |
| Idle CPU | < 1 % of a core | 3 % | harness |
| First frame | < 150 ms | 400 ms | harness |
| Keypress → visible change | < 50 ms | 100 ms | harness |
| Full-screen text redraw | < 3 ms | 8 ms | bench |
| Cold art blob render | < 60 ms/image (N100) | — | measured |
| Cached art blob load | < 1 ms | 5 ms | harness |
| Binary size (LTO, strip) | < 12 MB | 20 MB | `nix build` |
| Art disk cache | 20 MB LRU | — | config |

**Memory must be flat**: it must not grow with library size, queue length, or how much you browse. That is the property defended in CI, more than any absolute number.

---

## Architecture

### Module map

```
src/
  main.rs                 startup: config → terminal → capability probe → run loop
  cli.rs                  clap args (--server, --config, --no-art, --debug)
  config.rs               TOML config, XDG paths, password_cmd
  app.rs                  App state + update() (the only place state mutates)
  event.rs                crossterm reader thread → mpsc; 1 Hz tick; resize
  subsonic/
    mod.rs
    client.rs             ureq agent, md5 token auth, error mapping
    types.rs              serde structs (subset of Subsonic responses)
    endpoints.rs          albumList2, album, artists, artist, similarSongs2,
                          topSongs, randomSongs, search3, playlists, playlist,
                          starred2, star/unstar, scrobble, coverArt, stream
    url.rs                coverArt/stream URL construction
  player/
    mod.rs                Player trait + PlayerEvent + PlaybackState
    mpv.rs                subprocess, unix-socket JSON IPC, reconnect
    scrobble.rs           now-playing, 50%/240s rule, disk-backed retry queue
  queue.rs                queue, shuffle, repeat, autoDJ hook
  library.rs              album list pagination + sort modes + letter jumps
  discovery.rs            similar artists/albums, lazy, cancellable, TTL
  listen_later.rs         local JSON shelf
  render/
    mod.rs
    protocol.rs           capability detection, cell pixel size query
    chafa.rs              batched subprocess, sixel/kitty blob splitting
    cache.rs              blob disk cache + LRU
    worker.rs             single low-priority render thread
    painter.rs            placement reconciler  ← the core piece
  ui/
    mod.rs, app_view.rs, album_list.rs, album_page.rs, artist_page.rs,
    playlist_page.rs, now_playing.rs, queue_view.rs, search.rs,
    discovery_pane.rs, listen_later_page.rs, settings.rs,
    shortcuts.rs, theme.rs, widgets/
tests/
docs/
```

### Event loop (one timer, nothing else)

```
loop {
    select! {
        ev = crossterm_events.recv_timeout(until_next_tick) => handle_key(ev),
        _  = tick_1hz                                            => handle_tick(),
        ev = player_events.recv()                                => handle_player(ev),
        ev = render_done.recv()                                  => handle_art_ready(ev),
    }
    if state.dirty { draw(); }        // dirty flag, never unconditional redraw
}
```

One 1 Hz tick drives the progress bar, scrobble timing and duration checks. No per-widget intervals. The progress bar is the only widget that redraws on tick, and only its own cells change.

### Render pipeline

```
album id ──► coverArt?size=N ──► 20 MB disk image cache
                                        │
                            render worker (1 thread, low priority)
                            chafa batch: -f sixel|kitty, --size, --colors 16
                                        │
                            split on ESC \ ; parse header for real px
                                        │
                            blob cache: <cache>/art/<id>/<px>-<colors>-<proto>.blob
                                        │
                            painter: desired placements vs painted regions
                            unchanged → 0 bytes │ moved → erase+place │ new → paint
```

**Painter contract** (the piece that makes sixel usable):

- Each frame the view layer yields `Vec<Placement { key, rect_cells, blob_key }>`.
- The painter compares against `HashMap<PlacementKey, Rect>` of what is actually on screen.
- Bytes emitted, in this order: **erase** for moved/gone → `terminal.draw()` (text diff) → **paint** for new/changed. The split order matters: an erase must not wipe text the diff just wrote.
- Zero bytes when nothing changed — the default case while music plays.
- On resize or alt-screen entry, drop all tracked placements and repaint.

**Symbols mode bypasses the painter.** When the selected renderer is `symbols`, art is rasterised into cells and drawn through the ratatui buffer like ordinary text — no placements, no reconciliation. The painter only manages pixel blobs, so `symbols` mode and `off` mode have no painter cost at all.

**Thumbnail strategy (D8):** 2-line album rows, thumbnail target ~32–40 px, **16 colours**. Estimated 12 thumbs ≈ 11.5 KB per full repaint. Settle-paint (~80 ms debounce) during fast scrolling; immediate paint on selection change.

**Detail views** (now-playing, album page): single large cover, 200–240 px at 240 colours (62 KB), one image on screen.

### Player contract (mpv IPC)

- Spawn: `mpv --no-config --vo=null --ao=<auto> --video=no --audio-display=no --input-ipc-server=<sock> --idle=yes --load-scripts=no --demuxer-max-bytes=8MiB --demuxer-max-back-bytes=2MiB --cache-secs=20`
- Commands: `loadfile <url> replace|append`, `set pause`, `seek absolute+<s>`, `set volume`, `playlist-next`, `stop`.
- Observed properties: `time-pos`, `duration`, `pause`, `media-title`, `metadata`, `idle-active`.
- Events consumed: `file-loaded`, `end-file` (with reason), `property-change`.
- On socket EOF/child exit: restart the child, restore queue position, emit `PlayerEvent::Crashed`.

### Subsonic surface

Used: `ping`, `getAlbumList2` (random/newest/alphabeticalByArtist/byYear/byGenre/starred/frequent/recent), `getAlbum`, `getArtists`, `getArtist`, `getArtistInfo2`, `getTopSongs`, `getSimilarSongs2`, `getRandomSongs`, `search3`, `getPlaylists`, `getPlaylist`, `getStarred2`, `star`, `unstar`, `scrobble`, `getCoverArt`, `stream`.

Dropped: `getScanStatus`, `startScan`.

Auth: `t = md5(password + salt)`, `s = salt`, per-request random salt; `u`, `v=1.16.1`, `c=undertone`, `f=json`.

### Config (`~/.config/undertone/config.toml`)

```toml
server = "https://music.example.org"
username = "david"
# password XOR password_cmd
password = "..."
# password_cmd = ["pass", "show", "navidrome"]

scrobble = true
auto_dj = true
start_view = "random"

[art]
mode = "auto"        # auto | sixel | kitty | symbols | off
thumb_colors = 16
cover_colors = 240
cache_mb = 20

[player]
mpv = "mpv"
transcode = "never"  # never | mp3-192
```

Password precedence: CLI flag → `password_cmd` → `password` → prompt. File mode 0600 enforced with a warning if not.

### Feature map — Covertone → Undertone

| Covertone | Undertone | Change |
|---|---|---|
| Home / Albums / Artists / Playlists / Favorites / Search | same six views, `1`–`6` | no tabs, no numeric tab bar |
| Album / Artist / Playlist detail | same | art-rich |
| Now Playing view | same, large cover | new render budget |
| Queue drawer | full-screen queue view (`q`) | reorder, remove, jump |
| Discovery panel (similar artists + genre albums + AI text) | `D` side pane, **no AI** | lazy, TTL-cached, cancellable |
| Auto DJ | same | `getSimilarSongs2` at queue end |
| Listen Later | same | local JSON |
| Random-first album sorting | same, default | server `type=random` |
| Guess the Artist game | **cut** | — |
| Tabbed browsing | **cut** | — |
| Accent colour | **cut** | fixed palette, 16-colour floor |
| Library scan | **cut** | — |
| Offline cache / downloads / sync | **cut** | art blob cache only, metadata only |
| PWA / Docker / Capacitor / Android | **cut** | — |
| vim `h/j/k/l` grid navigation | list-based `j/k`, `h/l` for panes | terminal-native |

New in Undertone:

- Fuzzy finder (`/`) over server search with 150 ms debounce.
- Multi-select (`v`) + append-to-queue (`a`).
- Session restore: queue, track, position.
- Control socket (`--control-socket`) accepting one-line JSON commands for scripting.

### Keybindings

```
1-6 views   / search   ? help   q queue   D discovery   v now-playing
j/k move    h/l pane   Enter activate      Space pause   >/< next/prev
s shuffle   r repeat   f/b seek ±10s      +/- volume    m mute
F star      L listen-later toggle          Esc close/cancel
g g top     G bottom   : command line (later, optional)
```

---

## Implementation phases

Each task ends with tests passing and a commit. Tests are `cargo test`; UI logic is tested against a pure state model, not a terminal.

### Phase 0 — Scaffold and Nix (3 tasks)

**Task 0.1 — Repository + flake**
- Create `undertone` repo (public) via `gh repo create dbeley/undertone`.
- Files: `flake.nix`, `flake.lock`, `.envrc`, `.gitignore`, `rust-toolchain.toml`, `Cargo.toml`, `src/main.rs` (prints version), `README.md`, `LICENSE` (GPL-3.0, matching Covertone).
- `flake.nix` provides: `devShells.default` (rust 1.8x, cargo, clippy, rustfmt, chafa, mpv, just), `packages.default` (`rustPlatform.buildRustPackage`, `LTO=fat`, `strip`), `packages.undertone`, `overlays.default`, `nixosModules.default`.
- NixOS module options: `services.undertone.enable`, `.settings` (attrset → generated TOML), `.package`, `.user`.
- Verify: `nix develop -c cargo build`, `nix build .#`, `nix flake check`.

**Task 0.2 — CI**
- `.github/workflows/ci.yml`: `nix flake check`, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, binary size gate (< 12 MB, fails the job).
- Verify: push, workflow green.

**Task 0.3 — Error type and logging**
- `src/error.rs` (`thiserror`), `src/log.rs` (file logger at `$XDG_STATE_HOME/undertone/undertone.log`, off by default, `--debug` enables).
- Tests: error display strings, log path resolution with `XDG_STATE_HOME` overridden.

### Phase 1 — Subsonic client (4 tasks)

**Task 1.1 — Auth + request builder**
- `src/subsonic/client.rs`. Salt generation, `md5(password+salt)`, query assembly, `ureq` agent with `Connection: keep-alive`.
- Tests: deterministic salt injection → exact expected query string; token matches a known md5 vector.

**Task 1.2 — Response types**
- `src/subsonic/types.rs`. `serde` structs for the endpoint subset; `#[serde(rename = "subsonic-response")]` envelope; error envelope (`failed` + `error.code`/`message`).
- Tests: fixture JSON for each endpoint → deserialises; error envelope maps to `SubsonicError::Server{code,message}`.

**Task 1.3 — Endpoints**
- `src/subsonic/endpoints.rs`. One method per used endpoint, typed params, `size` on `getCoverArt`, optional `format`/`maxBitRate` on `stream`.
- Tests: `ureq` test transport recording requests → assert exact path+params for each method.

**Task 1.4 — TTL cache**
- `src/subsonic/cache.rs`. In-RAM LRU, 5 min TTL, bounded to ~500 entries.
- Tests: hit/miss, expiry, eviction order, memory bound (entry count).

### Phase 2 — Player (4 tasks)

**Task 2.1 — `Player` trait**
- `src/player/mod.rs`: `trait Player { fn load(...); fn play(); fn pause(); fn seek(...); fn set_volume(...); fn state() -> PlaybackState; }` + `PlayerEvent`.
- Tests: a `MockPlayer` exercises the contract; trait compiles for both impls.

**Task 2.2 — mpv IPC**
- `src/player/mpv.rs`: spawn with the measured flag set, unix socket, line-delimited JSON, id correlation, property observation, restart on EOF.
- Tests: fake mpv script that echoes JSON on the socket → assert command encoding and event parsing; simulated crash → `Crashed` event + restart.

**Task 2.3 — Queue model**
- `src/queue.rs`: append/replace/remove/move, shuffle (stable order restore), repeat off/all/one, current index, autoDJ hook.
- Tests: shuffle/repeat matrices, reorder invariants, autoDJ trigger at end.

**Task 2.4 — Scrobbling**
- `src/player/scrobble.rs`: `submission=false` at start, `submission=true` at 50 % or 240 s (whichever first), dedupe per track instance, disk-backed retry queue.
- Tests: fake clock → assert exactly one now-playing and one submission per play; seek-past-50 %; skip-before-50 % scrobbles nothing; failed submission retried once and dropped after N.

### Phase 3 — Render pipeline (5 tasks) ← highest risk, do early

**Task 3.1 — Capability detection**
- `src/render/protocol.rs`: `$TMUX`/`$ZELLIJ` → symbols; `$KITTY_WINDOW_ID`/`TERM_PROGRAM` → kitty; DA1 probe with 100 ms timeout → sixel; else symbols. Cell pixel size via `TIOCGWINSZ` `ws_xpixel/ws_ypixel`, fallback `CSI 16 t`, fallback 8×17.
- Tests: env matrices → protocol choice; zero cell pixels → fallback; probe timeout does not block.

**Task 3.2 — chafa batching + splitting**
- `src/render/chafa.rs`: build one invocation for N images, split stdout on `ESC \`, parse `"1;1;W;H` header, reject malformed output.
- Tests: recorded chafa output fixtures → correct split count, px parsing, blob attribution order.

**Task 3.3 — Blob cache**
- `src/render/cache.rs`: path scheme `<id>/<px>-<colors>-<proto>.blob`, LRU eviction to configured MB, atomic writes.
- Tests: round-trip, eviction under a forced small cap, concurrent-safe write (temp+rename).

**Task 3.4 — Render worker**
- `src/render/worker.rs`: single thread, bounded channel (64), viewport priority, batch collection (up to 8), cancellation of requests no longer visible.
- Tests: priority ordering, cancellation, no unbounded queue growth when the user scrolls fast (assert queue length stays ≤ cap).

**Task 3.5 — Painter (reconciler)**
- `src/render/painter.rs`: desired vs painted diff, emit order erase→draw→paint, zero bytes when unchanged, full invalidation on resize.
- Tests: byte-count assertions — unchanged frame emits 0; one scrolled row emits exactly one erase + one paint; resize emits full repaint; erase never precedes draw for the same cell.

### Phase 4 — UI shell (5 tasks)

**Task 4.1 — Theme + 16-colour floor**
- `src/ui/theme.rs`: palette that degrades to 16 colours; `COLORTERM` detection for truecolor accents; no user theming.
- Tests: every semantic token resolves in 16-colour mode.

**Task 4.2 — App shell + navigation**
- `src/app.rs`, `src/ui/app_view.rs`: view enum, `1`–`6`, back stack, dirty tracking, status line.
- Tests: navigation state machine, back stack, unknown key is a no-op.

**Task 4.3 — Album list with thumbnails**
- `src/ui/album_list.rs`: 2-line rows, thumb block, pagination via `albumList2` offset, random default, sort cycling.
- Tests: row rendering from a state fixture (into a `TestBackend`), pagination fetch trigger at 80 % scroll, thumb placement rects.

**Task 4.4 — Detail views**
- `album_page.rs`, `artist_page.rs`, `playlist_page.rs`, `now_playing.rs`: track lists, big cover, play-from-track, star toggle.
- Tests: track list selection, play action produces the right queue, star toggles optimistic + revert on error.

**Task 4.5 — Search, queue view, help**
- `search.rs` (debounced `search3`), `queue_view.rs`, `shortcuts.rs`.
- Tests: debounce coalesces keystrokes (< 150 ms → one request), queue reorder via keys, help overlay content.

### Phase 5 — Discovery and persistence (4 tasks)

**Task 5.1 — Discovery pane**
- `src/discovery.rs` + `ui/discovery_pane.rs`: similar artists, same-genre albums, artist bio text. Lazy, TTL-cached, one request at a time, cancellable, never on the startup path.
- Tests: toggle does not fetch at startup; fetch cancellation on track change; cache prevents a second fetch inside TTL.

**Task 5.2 — Auto DJ**
- Extend `queue.rs`: on `end-file` with playlist end, fetch `getSimilarSongs2` for the last track, append, cap the lookahead.
- Tests: triggers once, does not trigger when repeat=all, respects a max fetch chain.

**Task 5.3 — Listen Later**
- `src/listen_later.rs`: local JSON at `$XDG_DATA_HOME/undertone/listen-later.json`, atomic write, dedupe by album id.
- Tests: add/remove/dedupe, corrupt file → empty list + warning, atomic write survives a simulated partial write.

**Task 5.5 — Gapless playback**
- Hand the player a window of tracks and follow its playlist position; top the window up before it runs out. `Ended` becomes informational (mpv reports the end of every entry), and `Idle` means the window ran out.

**Task 5.4 — Session restore + config**
- `src/config.rs` completion + session state file (queue, index, position, view).
- Tests: round-trip; missing/corrupt session → clean start; config precedence (CLI > cmd > file); 0600 permission warning.

### Phase 6 — Hardening (4 tasks)

**Task 6.1 — Resource harness**
- `tests/perf.rs` + `scripts/measure.sh`: startup to first frame, idle RSS (read `/proc/self/status` VmHWM), full redraw timing, cache-hit latency. Thresholds from the budget table, generous enough for CI noise.
- Verify: `cargo test --release --test perf` passes on CI.

**Task 6.2 — Degradation matrix** — done
- `scripts/degradation.py` drives the real binary through a pty and asserts on the rendered screen: `sixel`, `kitty`, `symbols`, `--no-art`, 60×24, 18×4, dead server, missing player.
- It serves its own library (`scripts/fixture_server.py`), so it needs no network, and runs in CI.
- Results and the cases it cannot cover are in `docs/COMPATIBILITY.md`. The matrix is what found symbol art being dead in three places at once.

**Task 6.3 — Error paths**
- Server down at startup, auth failure, mid-playback network loss, mpv missing from PATH, mpv crash, malformed JSON, unreadable cache dir.
- Verify: each surfaces as a non-fatal status-line message; the app never exits from a recoverable error.

**Task 6.4 — Docs** — mostly done
- `docs/DESIGN.md` (this file), `docs/PERFORMANCE.md` (how to reproduce the measurements), `docs/COMPATIBILITY.md` (what each terminal and each failure does), `docs/SCREENS.md` (frames rendered by `scripts/screens.py` from the real binary, so they cannot drift).
- `README.md` is the entry point, kept short on purpose.
- Not done: `vhs` recordings. The frames in `docs/SCREENS.md` are text because a terminal screenshot is text; an animated recording would show the art protocols and the gapless transitions, which text cannot.

---

## Navigation and layout state

Three rules keep the screens coherent as the app grew:

- **An artist is a place, not a list.** Its page has three panes — albums,
  similar artists, biography — and `Tab` moves between them; the cursor follows
  the focused pane, and `Enter` acts on what that pane holds. The left pane has
  two tabs, albums and the artist's most played tracks, switched with `t`:
  albums and top songs answer the same question and both open something, so
  they share the biggest pane rather than growing a fourth column. Top songs
  come from `getTopSongs`, which is keyed by artist *name* and is optional on a
  server without listening history. The biography is
  optional data from `getArtistInfo2`: a server without it still gets a usable
  page, and the pane says so. `A` on an album opens the artist behind it, using
  the album's own `artistId` rather than a lookup by name.
- **Walls are per list.** The library and the artist list can each be a list of
  rows or a wall of covers, toggled with `w`, and each remembers its own choice:
  wanting the library as a wall says nothing about wanting artists that way.
  Both walls come from one `draw_wall`, so tile geometry, whole-row scrolling
  and art requests cannot drift between them.
- **History is a stack in both directions.** `h` goes back to where you came
  from and `l` comes forward again, so going back is reversible; `Left`/`Right`
  do the same. A wall keeps `h`/`l` for moving between covers, and a rail jump
  or a fresh drill-down clears the forward history, because those are new
  journeys rather than a walk back along an old one.

Sort and layout are saved in the session file, not the config: they are how you
left the place, set by pressing `S` and `w`, and restoring them before the first
fetch means the album list is asked for in the right order in one request.

## Tests and validation

| Level | Tool | Covers |
|---|---|---|
| Unit | `cargo test` | client URLs/auth, serde fixtures, queue, scrobbler, painter diffs, cache eviction |
| Render | `TestBackend` + recorded fixtures | row/detail layout, placement rects |
| Perf | `tests/perf.rs` | startup, RSS, redraw time, cache hit |
| Nix | `nix flake check`, `nix build` | package builds, module evaluates, binary size gate |
| Degradation | `scripts/degradation.py` | art protocol per setting, narrow/tiny layouts, dead server, missing player |
| Manual | device | real foot sixel on the Pi 2, scroll smoothness, audio device handover |

Renderer fixtures are recorded from real `chafa` output and committed, so tests do not require chafa at test time.

## Risks

| Risk | Mitigation |
|---|---|
| mpv 66 MB pushes total to ~85 MB | `Player` trait from commit one; D1-open alternatives B/C |
| Pi 2 sixel decode slower than projected | 16-colour thumbnails, settle-paint, batching, per-screen thumb cap; measure on device before tuning |
| chafa geometry differs per version | parse px from output, never predict; pin chafa in the flake |
| Discovery does excess API calls | lazy + serialised + TTL + cancellable; never at startup |
| Scroll performance regressions | byte-count assertions in painter tests; perf gate in CI |
| armv7 has no prebuilt crates (`ring`, etc.) | verify `cargo build --target armv7-unknown-linux-gnueabihf` in Nix cross shell in Task 0.1 |

## Open questions

1. **D1-open**: accept ~85 MB total with mpv, or commit to the native decoder (~30 MB, +2–3 weeks)?
2. **Row height**: 2-line rows (~12 albums/screen, clearer thumbs) or 3-line (~8, larger art)? Default proposed: 2.
3. **Transcoding**: default `never` (full fidelity, more FLAC decode CPU on the Pi) or `mp3-192` (less client CPU, server load)?
4. ~~**Repo**: public `dbeley/undertone`? MIT licence?~~ **Resolved:** public `dbeley/undertone`, GPL-3.0 to match Covertone.

## Non-goals

Tabs, game, AI, accent-colour theming, library scanning, offline audio cache/downloads, PWA, Docker, Android, mouse input, TTY/framebuffer support, podcast/radio support, multi-server.
