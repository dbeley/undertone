# Compatibility and degradation

What the interface does with the terminals, sizes and failures it will meet.
Every row below is checked by `scripts/degradation.py`, which starts the real
binary in a pty, waits for the first frame, and asserts on the rendered screen —
not on a claim in a document.

```
case                result
sixel, 104x30       ok
kitty, 104x30       ok
symbols, 104x30     ok
no art at all       ok
narrow, 60x24       ok
too small, 18x4     ok
dead server         ok
missing player      ok
```

Run it with `python3 scripts/degradation.py ./target/release/undertone`. It
serves its own library from `scripts/fixture_server.py`, so it needs no network
and takes about a minute. `--live` points the same rows at a real server
(`UNDERTONE_SERVER`, `UNDERTONE_USER`, `UNDERTONE_PASSWORD`). CI runs it.

## Art protocols

| Setting | What is drawn | Verified |
| --- | --- | --- |
| `--art=sixel` | sixel frames, cached as blobs, deleted by overpaint | sixel escape in the stream, thumbnails on disk |
| `--art=kitty` | kitty APC images, tagged with an id and deleted by id | APC escape in the stream, thumbnails on disk |
| `--art=symbols` | characters and colour, drawn as cells | chafa glyphs on screen, thumbnails on disk |
| `--no-art` | nothing; the cover slot keeps its placeholder | no graphics escapes, **no thumbnails fetched** |

Symbol art is the fallback that matters most: a framebuffer console and any
terminal without graphics support both land there. It is drawn through ratatui
rather than the painter, so it takes part in normal cell diffing — and it had
been silently broken, because three separate gates all assumed "not pixel means
no art": the art service refused to encode it, the placement collector skipped
it, and no source was handed to the service. One chafa call per cover fixes the
batched-split problem, since symbol output has no frame to split on.

## Layout

| Width | Behaviour | Verified |
| --- | --- | --- |
| ≥ 64 columns | rail, divider, header, body, three-column bar | default frame |
| < 64 columns | rail is dropped, the body takes the full width | no jump keys, view still named |
| < 20×5 | "terminal too small", no drawing | message instead of a panic |

The bar's key hints are dropped below 70 columns; the progress section and the
queue list stay.

## Failures

| Failure | Behaviour | Verified by |
| --- | --- | --- |
| server unreachable at startup | warning naming the URL, empty list, `R` retries | `dead server` row, `tests/errors.rs` |
| wrong password | warning, empty list, no crash | `tests/errors.rs` |
| server returns HTML or garbage | decode error naming the endpoint | `tests/errors.rs` |
| art cache unwritable | art off, a warning, everything else works | `tests/errors.rs` |
| `mpv` missing | silent player, warning, browsing still works | `missing player` row, `tests/errors.rs` |
| `chafa` missing | art off, a warning | `tests/errors.rs` |
| a track fails mid-playlist | message, playback moves on | `tests/errors.rs`, `src/app.rs` |

What the interface looks like is in `docs/SCREENS.md`, rendered from the real
binary by `scripts/screens.py`.

## Not covered here

- **A real framebuffer console.** No pty row can stand in for `fbcon`; the
  symbol path is verified in a terminal, and the console needs a device.
- **Sixel on real `foot`.** The escape sequences are verified, the pixels are
  not: this environment has no audio device and no graphical terminal.
- **A Raspberry Pi 2.** Every measurement in `docs/PERFORMANCE.md` comes from a
  desktop x86_64 machine. The budgets are chosen for the Pi; they are not
  measured on one.
- **Real audio.** No audio device here, so playback is verified with mpv's null
  sink and against a fixture stream.
