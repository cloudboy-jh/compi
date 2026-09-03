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
| `cargo test --all-targets` | Pass | 52 tests across six suites as of 2026-09-03, including Windows daemon lifecycle, transient-connection cleanup, working-directory, protocol-v6, metadata-v2 migration, and terminal recovery coverage. |
| Output-flood control and recovery | Pass | The integration test keeps the controlling client attached during three seconds of unbounded `yes` output, sends byte 3 (`Ctrl+C`), recovers delta gaps by snapshot, observes `FLOOD_INTERRUPTED`, and exits cleanly. |
| `cargo build --release --bins --target-dir target/verify-release` | Pass | Built with the Windows 10 SDK 10.0.22621.0 `fxc.exe` directory on `PATH`. |
| Product artifact names | Pass | Top-level release output contains only `compi.exe` and `compi-daemon.exe`. The probe exists only under `examples` when explicitly requested. |
| Product artifact sizes | Pass | `compi.exe`: 12.0 MB. `compi-daemon.exe`: 1.4 MB in the 2026-09-03 portable package. |

The default `target/release` rebuild could not replace the running `compi-daemon.exe` (`Access is denied`). Verification therefore used an isolated target directory; no normal daemon sessions were terminated.

## 2026-09-03 implementation and qualification update

- Protocol v6 and persisted metadata v2 now carry validated requested/resolved working-directory data, the selected default WSL2 distribution, warnings, and OSC 7 current-directory state. Metadata v1 is upgraded atomically.
- Explicit absolute WSL and Windows paths are validated in the selected default WSL2 distribution. New tabs inherit a valid OSC 7 path; omitted paths still start at `~`.
- A release UI smoke launched in `C:\Users\johns\OneDrive\Desktop\Proj\compi` and rendered a Bash prompt at `/mnt/c/Users/johns/OneDrive/Desktop/Proj/compi`.
- A 30-minute automated soak passed with three sustained-output sessions plus repeated create/kill and transient-client churn. Daemon handles stayed at 152–153 during active load and fell to 115 after cleanup. Client private memory stayed between 89.29 and 89.54 MiB; daemon private memory peaked at 37.31 MiB.
- Final warm diagnostics measured first-window p95 at 376 ms, ready-for-input p95 at 544 ms, and input-to-render p95 at 153 ms. `gpui::Application::new` consumed roughly 321–369 ms in typical samples, and disabling thin LTO produced no improvement. Reducing the initial absent-daemon wait from 250 ms to 25 ms lowered the best cold first-terminal p95 to 935 ms; subsequent fresh unsigned daemon launches showed a repeatable 3.17-second host-side process-start delay despite 36 ms of instrumented daemon initialization.
- The final unsigned `0.1.0` setup and portable ZIP build successfully, their embedded product versions agree, and both generated SHA-256 values match the packaged bytes. The release workflow requires signing secrets and creates a draft GitHub release.

The 2026-09-03 launch measurements used the Meta Virtual Monitor and are diagnostic only. Code signing, a version-to-version upgrade, physical-display/keyboard checks, the full compatibility matrix, and clean Windows 10/11 qualification remain release blockers.


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

### Phase 2 diagnostic baseline

The new release harness exercised ten empty-window launches, ten cold-daemon launches, and ten warm existing-session launches. It ran through the Meta Virtual Monitor, so these results are diagnostic and explicitly not a qualified physical-display run. The environment record identified Windows 10.0.26200, an AMD Ryzen 7 7800X3D, Ubuntu on WSL2, and a 3440×1440 100 Hz NVIDIA display that was not the automation surface.

| Mode and metric | p50 | p95 | Worst |
|---|---:|---:|---:|
| Empty first window | 354 ms | 1,200 ms | 1,200 ms |
| Warm first window | 351 ms | 1,149 ms | 1,149 ms |
| Warm first terminal | 374 ms | 1,171 ms | 1,171 ms |
| Warm ready for input | 498 ms | 1,272 ms | 1,272 ms |
| Warm input to rendered marker | 110 ms | 152 ms | 152 ms |
| Cold first window | 348 ms | 381 ms | 381 ms |
| Cold first terminal | 1,182 ms | 1,260 ms | 1,260 ms |
| Cold ready for input | 1,293 ms | 1,370 ms | 1,370 ms |
| Cold input to rendered marker | 137 ms | 141 ms | 141 ms |

The warm run used one unreported warm-up launch before its ten recorded samples so shell startup was not mislabeled as an existing-session reattach. No samples were discarded. The cold and empty measurements came from run `release-20260902-155926-9472`; the corrected warmed-session measurements came from run `release-20260902-160416-29572`. Both runs used the working tree based on commit `cbd2b118f1bf84df9c006f0916e825c58bead988`.

| Stabilized resource metric | p50 | p95 |
|---|---:|---:|
| Empty client private bytes | 86.15 MiB | 87.41 MiB |
| Warm client private bytes | 90.09 MiB | 91.31 MiB |
| Cold client private bytes | 89.60 MiB | 90.79 MiB |
| Empty client dedicated GPU memory | 53.85 MiB | 53.85 MiB |
| Warm client dedicated GPU memory | 54.87 MiB | 54.87 MiB |
| Cold daemon private bytes | 1.92 MiB | 1.92 MiB |

An additional isolated diagnostic measured fresh daemons and clients with one, two, and four attached blank sessions:

| Attached sessions | Client private bytes | Daemon private bytes |
|---:|---:|---:|
| 1 | 88.73 MiB | 1.94 MiB |
| 2 | 90.79 MiB | 2.52 MiB |
| 4 | 91.25 MiB | 3.83 MiB |

This single run (`release-20260902-161010-12532`) indicates approximately 0.6 MiB of marginal daemon private memory per blank session. It is diagnostic, not a distribution or release gate.

This establishes that most of the one-session private-byte gap exists in the blank GPUI client: the warm client adds roughly 4 MiB at p50, while the daemon is roughly 2 MiB. Daemon scrollback compression and PTY parking therefore do not address the P0 client-memory failure. All measured launch gates still fail, and the 1,149–1,200 ms p95 first-window outliers require reproduction on the physical display before attribution.

## Installer lifecycle

The per-user installer was built and exercised under a non-elevated Windows token:

| Check | Result | Evidence |
|---|---|---|
| Reproducible package build | Pass | `tools/build-installer.ps1` restored the pinned WiX 6.0.2 tool, built the product, MSI, maintenance surface, and bootstrapper, validated the MSI, and emitted `Compi-0.1.0-Setup.exe`, `Compi-0.1.0-Windows-x64.zip`, and `SHA256SUMS.txt`. |
| Independent UI states | Pass | The GPUI preview executable rendered ready, upgrade, installing, complete, error, and remove states without changing installer or product state. |
| Fresh local product install | Pass | The setup installed `compi.exe`, `compi-daemon.exe`, and the maintenance executable under `%LOCALAPPDATA%\Programs\Compi`, persisted its MSI source, registered Add/Remove Programs repair and remove commands, and created the `Compi Daemon` scheduled task. |
| Repair | Pass | The installed maintenance command completed and preserved the product binaries and daemon task. |
| Uninstall | Pass | The installed uninstaller relocated itself before invoking MSI removal, then removed the install directory, app data, Add/Remove Programs registration, installed marker, and scheduled task. The temporary uninstaller removed itself after the completion window closed. |
| Upgrade | Not yet qualified | Major-upgrade sequencing and daemon/task replacement are implemented, but no older versioned release artifact exists for an end-to-end version-to-version test. |
| Clean-profile qualification | Not yet qualified | The install/repair/uninstall run used a non-elevated token after old pre-release product state was cleared, not a newly created Windows user profile. |

## Implemented during this pass

- Renamed the crate/library to `compi`; moved the diagnostic executable to the explicit `compi-probe` example.
- Bumped protocol version to 5; version 4 introduced bincode screen frames while retaining readable JSON control frames, and version 5 adds explicit dead-session status.
- Changed terminal cell text to inline `SmolStr` storage.
- Removed fixed 16 ms UI polling; terminal events now wake GPUI and are applied in a bounded display-paced batch.
- Limited queued screen frames while reserving queue capacity for input/control responses; dropped deltas recover through authoritative snapshots instead of disconnecting the client.
- Removed full-snapshot ownership from paint elements; paint models clone only visible rows and required cursor/graphics metadata.
- Added a bounded 512-row shaping cache.
- Moved Kitty base64/image decode off the UI thread and deduplicated pending decodes.
- Rebuilt the titlebar, adaptive tabs, vector icons, session palette, and native drag targets.
- Added `compi.exe --instance <name>` for safe isolated GUI acceptance runs.
- Added opt-in process-separated resource logging, empty-window sampling, cold/warm connection labels, a rendered ready-for-input probe, GPU-memory collection, and the repeatable release measurement harness.
- Replaced `docs/testcmds.md` with the tiered canonical acceptance matrix.
- Added atomic `%LOCALAPPDATA%\Compi\sessions-v1.json` metadata, malformed-manifest quarantine, bounded dead-record retention, and startup conversion of unrecoverable active records to `dead`.
- Added explicit dead-session reporting in the probe and GUI switcher; dead rows retain the daemon-loss reason and reject attachment.
- Added integration coverage for unexpected and intentional daemon restart, malformed metadata, controlling-client races, and bounded daemon handles across repeated kill cycles.
- Added a least-privilege per-user Task Scheduler entry with sign-in and on-demand activation; the GUI now uses it for the default daemon while isolated instances retain direct startup.
- Added `compi-daemon.exe --supervise`, which restarts a failed daemon child with bounded backoff and exits without restarting after intentional shutdown.
- Verified Task Scheduler registration and removal, cold GUI activation, isolated startup, intentional no-restart behavior, forced child-process restart, and complete supervisor/daemon process cleanup.
- Added a WiX MSI and custom GPUI bootstrapper for per-user, non-elevated installation, repair, upgrade, and removal.
- Added an installed GPUI maintenance surface and relocation-based uninstall path so the running maintenance executable cannot lock the install directory.
- Added an independent installer-state preview executable and a pinned, reproducible packaging script that emits setup, portable ZIP, and SHA-256 checksum artifacts.

## Still open before the release-feel gate passes

- Physical-keyboard `Ctrl+C` during sustained output.
- Physical-display frame pacing at 100/120 Hz.
- Physical-display confirmation of the blank GPUI baseline plus extended memory/handle/GPU-memory soak.
- Full TUI, selection, scrollback-resize, Kitty graphics, mixed-DPI, multi-monitor, narrow/wide/maximized, and 30-minute interactive matrix.
- Sub-200 ms warm interaction; current median is about 400 ms to terminal content.
