# Compi acceptance results — 2026-09-02

## Environment

- Windows 11 Home 10.0.26200, x64
- AMD Ryzen 7 7800X3D
- Ubuntu on WSL2; `docker-desktop` stopped
- NVIDIA GeForce RTX 4070 Ti SUPER, 3440×1440 at 100 Hz reported by `Win32_VideoController`
- Meta Virtual Monitor used by the automation surface; its refresh rate was not reported
- Rust release profile with thin LTO and stripped symbols
- Isolated daemon instances used for destructive tests; the normal daemon and its nine detached sessions were not stopped

## Automated release gate

| Check | Result | Evidence |
|---|---|---|
| `cargo fmt --all -- --check` | Pass | Exit 0 in the final chained gate. |
| `cargo clippy --all-targets --all-features -- -D warnings` | Pass | Exit 0. Cargo still reports upstream future-incompatibility in `proc-macro-error2 v2.0.1`; this is not a current Clippy failure. |
| `cargo test --all-targets` | Pass | 29 tests across five suites. This includes the Windows daemon lifecycle integration test. |
| Output-flood control and recovery | Pass | The integration test keeps the controlling client attached during three seconds of unbounded `yes` output, sends byte 3 (`Ctrl+C`), recovers delta gaps by snapshot, observes `FLOOD_INTERRUPTED`, and exits cleanly. |
| `cargo build --release --bins --target-dir target/verify-release` | Pass | Built with the Windows 10 SDK 10.0.22621.0 `fxc.exe` directory on `PATH`. |
| Product artifact names | Pass | Top-level release output contains only `compi.exe` and `compi-daemon.exe`. The probe exists only under `examples` when explicitly requested. |
| Product artifact sizes | Pass | `compi.exe`: 12.0 MB. `compi-daemon.exe`: 991 KB. |

The default `target/release` rebuild could not replace the running `compi-daemon.exe` (`Access is denied`). Verification therefore used an isolated target directory; no normal daemon sessions were terminated.

## Native client checks

| Check | Result | Evidence |
|---|---|---|
| Brand-area window drag | Pass | Automated native mouse drag moved the window by 70×35 px. |
| Empty-header window drag | Pass | Automated native mouse drag moved the window by 86×33 px. |
| Session-tab window drag | Pass | Automated native mouse drag moved the window by 70×35 px while retaining tab click handling. |
| Minimize control | Pass | Native click on the minimize hit target set `IsIconic=true`; the window was then restored. |
| Session palette | Partial pass | `Ctrl+Shift+P` opened the overlaid session palette with the session state visible. `PrintWindow` captured the terminal and palette but omitted right-side non-client control rendering, so icon appearance still needs direct physical-display review. |
| Count 1–100 and clipboard command input | Partial pass | Clipboard paste executed the scripted sequence; the captured viewport showed ordered output 70–91. The DPI-limited capture did not include the bottom rows, so it is not evidence that every rendered line was visually inspected. |
| `Ctrl+C` on `sleep 30` | Pass | The client rendered `^C`, accepted `echo COMPI_INTERRUPT_OK`, printed the marker, and returned a clean prompt. |
| Close/detach/reopen | Pass | The isolated GUI created and attached a session; stopping the client left it detached and a later client reattached to the same session. |
| Sustained-output GUI interrupt | Needs manual confirmation | Synthetic `WScript.SendKeys('^c')` did not stop a foreground `yes` process during the GUI run, even after event batching and screen backpressure fixes. The protocol-level flood test passes, but a physical keyboard test is required to distinguish synthetic-input behavior from remaining GUI input starvation. |

## Performance measurements

Six release-mode warm launches against an existing isolated daemon/session produced:

- first window frame: 344–409 ms, median 358 ms;
- first terminal frame: 381–467 ms, median 400 ms.

The sub-200 ms warm interaction target is not met.

Two sustained-output samples after visible-row paint ownership and row-shaping cache changes produced:

| Metric | Sample 1 | Sample 2 |
|---|---:|---:|
| Frame interval p50 | 28,171 µs | 28,124 µs |
| Frame interval p95 | 35,452 µs | 35,463 µs |
| Terminal paint p50 | 229 µs | 234 µs |
| Terminal paint p95 | 453 µs | 458 µs |
| Private bytes | 99,827,712 | 98,734,080 |
| Handles | 592 | 595 |

A separate post-output process sample reported 63,889,408 bytes working set and 98,689,024 private bytes. Conclusions:

- Terminal painting is well below a single 100/120 Hz frame budget.
- The observed frame interval does not meet 120 Hz pacing. The automation ran through Meta Virtual Monitor with no reported refresh rate, so repeat on the physical display before attributing the interval entirely to Compi.
- The 35 MB private-byte target is not met. A blank GPUI-window baseline is still needed to separate framework/GPU allocation from terminal state.
- No long-duration claim is available from these short samples.

## Implemented during this pass

- Renamed the crate/library to `compi`; moved the diagnostic executable to the explicit `compi-probe` example.
- Bumped protocol version to 4 and changed screen frames from JSON to bincode while retaining readable JSON control frames.
- Changed terminal cell text to inline `SmolStr` storage.
- Removed fixed 16 ms UI polling; terminal events now wake GPUI and are applied in a bounded display-paced batch.
- Limited queued screen frames while reserving queue capacity for input/control responses; dropped deltas recover through authoritative snapshots instead of disconnecting the client.
- Removed full-snapshot ownership from paint elements; paint models clone only visible rows and required cursor/graphics metadata.
- Added a bounded 512-row shaping cache.
- Moved Kitty base64/image decode off the UI thread and deduplicated pending decodes.
- Rebuilt the titlebar, adaptive tabs, vector icons, session palette, and native drag targets.
- Added `compi.exe --instance <name>` for safe isolated GUI acceptance runs.
- Added opt-in `COMPI_PERF_LOG=1` instrumentation for frame interval, paint time, private bytes, and handle count.
- Replaced `docs/testcmds.md` with the tiered canonical acceptance matrix.

## Still open before the release-feel gate passes

- Physical-keyboard `Ctrl+C` during sustained output.
- Physical-display frame pacing at 100/120 Hz.
- Blank GPUI baseline and extended memory/handle/GPU-memory soak.
- Full TUI, selection, scrollback-resize, Kitty graphics, mixed-DPI, multi-monitor, narrow/wide/maximized, and 30-minute interactive matrix.
- Sub-200 ms warm interaction; current median is about 400 ms to terminal content.
