# Performance

Two halves, deliberately separate:

- **`tests/perf.rs`** guards the algorithms in CI: an idle frame must plan to
  zero operations, planning must stay in the microsecond range, the cache must
  not do anything silly, and building the app must touch no network. Thresholds
  there are loose on purpose — they are a regression alarm, not a benchmark.
- **`scripts/measure.sh`** measures the process on real hardware: time to first
  frame, idle RSS, and whether an idle app writes anything at all.

RSS in CI would measure the CI runner, not the app. Idle output in CI would
measure nothing at all, because the terminal is a pipe. So those live in the
script, and are recorded here by hand.

## Reproducing

```bash
nix develop -c cargo build --release
nix shell nixpkgs#python3 nixpkgs#chafa --command ./scripts/measure.sh
nix develop -c cargo test --release --test perf -- --nocapture
```

`scripts/measure.sh` opens a real pty, so it measures the app the way you
actually run it. Pass `--player=null --art=off` after the script name for a
floor measurement with no mpv and no image encoding.

## Measured

Machine: 2 vCPU of an Intel N100, virtualised, SSD, `demo.navidrome.org` over
the public internet (a LAN server will be much faster). Release build.

| Metric | Value | How |
|---|---|---|
| Time to first frame | 546 ms | `measure.sh`; includes ping and the first album page over the network |
| Idle RSS (app) | 6.0 MB | `VmRSS` 5 s after launch, art on, mpv detached |
| Peak RSS (app) | 6.0 MB | `VmHWM` at exit |
| Idle output | **0 bytes / 5 s** | `measure.sh` counts everything written while nothing moves |
| App construction | 386 µs | `tests/perf.rs`, no network, no I/O |
| Idle frame plan | 33 µs (debug) / **7.3 µs** (release) | 5 000 iterations on a 30-placement still screen |
| Cold plan, 30 placements | 15 µs | one full screen of new art |
| 10 000 queue advances | 152 µs | navigation is O(1) per step |
| Blob cache, 100 writes | 3.6 ms | after removing `fsync` per blob |
| Blob cache, 100 reads | 744 µs | |

Art pipeline, measured earlier with `examples/art_bench.rs` and `chafa` 1.18:

| Metric | Value |
|---|---|
| chafa, 3 images batched | 26.6 ms (vs 68.6 ms one at a time) |
| Thumbnail size, 16 colours | 955 B |
| Full 12-thumbnail screen | 11.5 KB |
| Cold render of 12 covers | 149 ms |
| Warm render (cache hit) | 4.2 ms |
| Cover fetch from the demo | 142 ms each, serial |
| First cover on screen | ~1.0 s cold, 14 covers in 1.8 s |

## Memory, honestly

The app itself is 6 MB. mpv, when playing, is a separate process at roughly
66 MB, which is why the budget in `docs/DESIGN.md` says ~85 MB steady state
rather than 6. That is the price of using mpv instead of a native decoder plus
output; it is documented rather than hidden, and the `Player` trait exists so
it can be swapped later.

Without mpv (`--player=null`) the measured floor is the 6 MB above.

## On a Pi 2

None of these numbers were measured on the target hardware. The Pi 2 class
figure that matters is CPU, not RSS: the idle path is a 250 ms timer that walks
a list and plans against an unchanged screen, which costs 7.3 µs here and
should stay well under a millisecond on an ARMv7 core at 900 MHz.

What will be slower on a Pi 2:

- **chafa** — roughly 5–8× slower per image, so a cold screenful is on the
  order of one second rather than 149 ms.
- **TLS handshakes** — the Cortex-A7 has no AES extensions; expect cover
  fetches to dominate first-load time.
- **SD card writes** — the reason blob writes are no longer fsynced. A durable
  write per cover cost 7.2 ms here and would cost far more there.

Run `scripts/measure.sh` on the device and put the numbers in this file; until
then, treat the Pi column as an estimate.
