# Compi release targets

These are product gates, not current capability claims. A release does not pass by moving a target to match a result; a miss requires optimization, an explained exception, or an explicit scope decision.

The canonical procedures and terminal correctness cases live in [`testcmds.md`](testcmds.md). Measured runs belong in dated acceptance reports such as [`ACCEPTANCE_RESULTS_2026-09-02.md`](ACCEPTANCE_RESULTS_2026-09-02.md), not in this target document.

## Measurement contract

- Test a release build on physical Windows hardware with an isolated daemon instance.
- Record Windows build, CPU, GPU, display resolution and refresh rate, WSL distribution, Compi commit, and whether the daemon and session were warm.
- Use one visible 100×30 terminal unless a case specifies another grid.
- Run at least 10 launch samples and report p50, p95, and worst result. Discard a sample only with a recorded external cause.
- Stabilize memory for six seconds before the initial sample. Record `compi.exe` and `compi-daemon.exe` separately, then report their sum. Do not count WSL, shell, or workload processes as Compi-owned memory.
- Record private bytes, working set, handle count, and dedicated/shared GPU memory. Private bytes are the primary Windows memory gate; working set and Ghostty RSS are context, not interchangeable measurements.
- Keep correctness mandatory. Dropped input, corrupt terminal state, hidden output loss, or reduced protocol recovery invalidates a performance result.

## Speed targets

### Release-feel gates

| Metric | Release target | Stretch target | 2026-09-02 Compi baseline |
|---|---:|---:|---:|
| First visible window frame, warm daemon, p95 | <100 ms | <50 ms | 344–409 ms; median 358 ms; fail |
| First terminal frame for an existing session, p95 | <200 ms | <100 ms | 381–467 ms; median 400 ms; fail |
| Ready-for-input round trip for an existing session, p95 | <200 ms | <100 ms | Not instrumented |
| Cold start with no running daemon, first terminal frame, p95 | <500 ms | <250 ms | Not measured |
| Active frame interval on a physical 100 Hz display, p95 | ≤10,000 µs | ≤8,333 µs | About 35,460 µs through Meta Virtual Monitor; inconclusive |
| Active frame interval on a physical 120 Hz display, p95 | ≤8,333 µs | ≤6,944 µs | Not measured |
| Terminal paint time, p95 | <1,000 µs | <500 µs | 453–458 µs; pass |
| Physical keypress-to-pixel latency, p95 | <16.7 ms | <8.3 ms | Not measured |
| Physical `Ctrl+C` to sustained-output stop, p95 | <100 ms | <50 ms | Protocol path passes; physical GUI path unconfirmed |

“First terminal frame” is the current measurable proxy for interactivity. It does not replace the ready-for-input gate: add a marker round trip from synthetic input to rendered output so shell startup and queued-input latency are included.

### Competitive throughput targets

These become release gates once the macOS-only reference harness has a reproducible Windows/WSL equivalent. Until then they are engineering targets, because OS, PTY, renderer, font, and grid differences dominate cross-machine comparisons.

| Workload | Release target | Stretch target | Reference to beat |
|---|---:|---:|---:|
| Plaintext I/O: `cat` the same 11 MB corpus, five-run average | ≤179 ms | ≤95 ms | Ghostty 179 ms; tty7 95 ms |
| DOOM-fire-zig, five runs of about 14 seconds | ≥552 fps | ≥888 fps | Ghostty 552 fps; tty7 888 fps |
| Sustained 10-second output flood | No input starvation, disconnect, or terminal corruption | Same, with physical `Ctrl+C` p95 <50 ms | Compi protocol regression passes; GUI path needs physical confirmation |

The plaintext result measures end-to-end PTY drain and parsing, not pure painting. DOOM-fire can become producer- and scheduler-bound; compare terminals back-to-back on one quiet machine and never treat cross-day FPS as exact.

## Memory targets

| Metric | Release target | Stretch target | 2026-09-02 Compi baseline |
|---|---:|---:|---:|
| `compi.exe` private bytes, one warm 100×30 session after six seconds | ≤35 MiB | ≤25 MiB | 94.2–95.2 MiB during active output; fail |
| Incremental combined private bytes per additional blank 100×30 session | ≤4 MiB | ≤2 MiB | Not measured |
| Client and daemon after closing added tabs, five-minute cooldown | No more than the pre-cycle baseline plus the greater of 5 MiB or 10% | Return to baseline plus 2 MiB | Not measured |
| Client and daemon after deleting Kitty images, five-minute cooldown | No more than the pre-image baseline plus the greater of 8 MiB or 10% | Return to baseline plus 2 MiB | Not measured |
| Thirty-minute mixed-workload soak | No monotonic private-byte, GPU-memory, or handle growth in the final 15 minutes | Return within 5% of the stabilized baseline after cleanup | Not measured |

A post-output sample reported about 60.9 MiB client working set and 94.1 MiB private bytes. The blank GPUI-window baseline is required to separate GPUI/D3D allocation from Compi terminal state, but framework cost does not silently relax the product ceiling.

For lifecycle checks, compare stabilized baselines before and after the exact same create/use/close cycle. Allocator retention may prevent an immediate byte-for-byte return; sustained positive slope or growth proportional to already-closed tabs/images is a failure.

## Required suite

Run all three tiers in [`testcmds.md`](testcmds.md) before a milestone or installer release:

1. **Automated regression:** formatting, Clippy with warnings denied, all-target tests, and release binaries.
2. **Scripted terminal and performance:** resize, output flood, large corpus, Unicode, color, alternate screen, clipboard, TUI resize, paste, and reattach under output with instrumentation enabled.
3. **Interactive client acceptance:** shell control, TUIs, selection, scrollback resize, tabs, session palette, window chrome, persistence, failure handling, Kitty graphics, mixed DPI/monitors, and the 30-minute soak.

Every performance report must include the correctness result from the same build. A fast run that skips a tier is diagnostic evidence, not release evidence.

## Ghostty reference metrics

Ghostty is the quality bar, not a directly comparable control. The reproducible throughput data below is macOS-only; the memory figures mix macOS and Linux, RSS and application-reported memory, different Ghostty revisions, window counts, and workloads. Preserve those qualifiers when quoting any number.

### Reproducible tty7 harness

The [`l0ng-ai/tty7` benchmark harness](https://github.com/l0ng-ai/tty7/blob/main/scripts/bench/README.md) opens real terminal windows at a common 155×40 grid on an Apple M1 Pro with 32 GB RAM and macOS 26.3.1. Plaintext is a five-run average over an 11 MB corpus; DOOM-fire is five runs of about 14 seconds; memory is RSS after a cold launch and six idle seconds.

| Run | Ghostty plaintext | Ghostty DOOM-fire | Ghostty idle RSS |
|---|---:|---:|---:|
| 2026-07-03 | 183 ms | 533 fps | 112 MB |
| 2026-07-04 | 179 ms | 552 fps | 128 MB |

The same 2026-07-04 pass measured tty7 at 95 ms, 888 fps, and 115 MB RSS; Alacritty at 239 ms, 485 fps, and 105 MB RSS; and Kitty at 185 ms, 616 fps, and 130 MB RSS. The harness also reports a raw-reader ceiling around 1,050–1,100 DOOM-fire fps on that machine and warns that macOS heavily throttles hidden or fully occluded windows.

### Startup observations

A January 2026 [Ghostty startup discussion](https://github.com/ghostty-org/ghostty/discussions/10426) reported these Debian 13.3 observations for a development build:

| Path | Reported elapsed time | Qualification |
|---|---:|---|
| First normal launch | About 500 ms | User estimate, not instrumented ready-for-input latency |
| Second normal launch with GTK single-instance enabled | 206 ms | Command launch-through-exit proxy |
| `ghostty +new-window` through an existing process | 33 ms | D-Bus command launch-through-exit proxy; explicitly not a ready-for-input measurement |
| `xterm -e true` comparison | 45 ms | Launch-through-exit proxy |

Ghostty maintainers explicitly recommend measuring from user intent until the new window is ready for input. Compi should do the same rather than optimize only process launch or first paint.

### Memory engineering history

Ghostty’s closed [memory tracking issue #254](https://github.com/ghostty-org/ghostty/issues/254) records the following snapshots. They are useful design references, not one coherent benchmark series:

| Platform and workload | Before | After | Context |
|---|---:|---:|---|
| Linux GTK, 20 empty windows | 146 MB | 107 MB | Paged-terminal work, March 2024 |
| Linux GTK, five windows each reading a 20 MB file | 678 MB | 152 MB | Scrollback-heavy paged-terminal work, March 2024 |
| Linux, 20 blank tabs | 145.6 MB | 112.5 MB | Shared font-stack branch, April 2024 |
| Linux, five tabs with 20 emoji visible | 110.1 MB | 95.4 MB | Shared font-stack branch, April 2024 |
| macOS, 10 windows running `htop` | 158 MB | 151 MB | Shared font-stack branch, April 2024 |
| macOS, five windows showing emoji | 170 MB | 154 MB | Shared font-stack branch, April 2024 |

The same issue states that the paged-terminal merge reduced Ghostty memory by roughly 40% in its tested scenarios. At closure in June 2024, the maintainer reported about 45 MB for one empty Ghostty window versus about 25 MB for an empty Cocoa window on an ARM Mac.

For historical scale only, the issue began in August 2023 with an 812 MiB Ghostty GTK process after opening 20 windows and closing 19, versus 84 MiB for xfce4-terminal. That result predates the terminal-state, font-sharing, renderer, and lifecycle fixes and must not be presented as current Ghostty performance.

A June 2026 [macOS user report](https://github.com/ghostty-org/ghostty/discussions/12972) described 500 MB–1 GB after several days of opening and closing tabs/windows. It was triaged as a duplicate and not established as a controlled baseline. This is why Compi’s release gate includes post-close recovery and long-soak slope, not only fresh-process memory.

## Interpretation

The immediate Compi gap is startup and private allocation, not glyph painting. Current terminal-paint p95 is already below 0.5 ms, while terminal availability is around 400 ms and private bytes are around 95 MiB. Optimize the startup critical path and establish the blank-GPUI baseline before trading away correctness or adding lower-level rendering complexity.
