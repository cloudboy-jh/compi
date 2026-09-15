# Compi next steps

This file tracks only unfinished work. Completed implementation and verification history is in [Completed work](COMPLETED.md).

Work in the order below. Keep changes on the existing workspace, persistence, protocol, and navigation contracts unless a listed item requires a narrow extension.

## 1. Remaining UI/UX qualification

- Preserve the completed top-tab navigation, optional workspace sidebar, flat responsive overlays, typography, and compact control system. Change them only where native evidence exposes a defect.
- Qualify the transient loading frame, native tab tear-off drag, and real mixed-DPI transitions on supported Windows displays. Automated captures already cover normal, narrow, maximized, split, sidebar, palette, settings, theme catalog, overflow, empty, disconnected, exited, failed, lost, confirmation, and recovery states.
- Verify hover, focus, active, disabled, pending, and destructive states without relying on color alone. Keep New terminal, pane actions, and custom Windows controls visible, reachable, and non-overlapping.
- Exercise physical keyboard navigation, terminal input, native drag, maximize, close, platform shortcuts, clipboard, IME/dead keys, exact user fonts, and fallback glyphs on supported Windows display configurations.

## 2. Performance and daily-use qualification

- Measure key receipt → PTY → replica → presentation latency, scrolling, resizing, and frame pacing under idle and sustained-output workloads. Use the current-FPS overlay for live feedback, not as a substitute for attributed timing.
- Record build and display context and compare identical workloads before and after any fix. Confirm opt-in continuous frame scheduling remains inactive when Performance and the FPS overlay are both hidden.
- Fix only demonstrated stalls, avoidable shaping or allocation, repaint scheduling, or queue problems. Keep caches and transport queues bounded; do not replace the terminal engine.
- Run the macOS measurement/soak runner on native release binaries. Repeat the Windows harnesses only after relevant source or environment changes.
- Produce attributable Mac resource measurements and a current Mac dogfood artifact; the current Windows release artifact and measurements are complete.

## 3. Native Mac and shared-platform qualification

- Qualify the bundled application, not a bare `cargo run`, across the complete workspace flow: shell and fullscreen-editor input, resize, close/reopen, same-process persistence, workspace/tab/split operations, tear-off and transfer, Settings, appearance, and recovery states.
- Exercise physical Cmd/Ctrl/Option keys, clipboard, dead keys/IME, exact user font and fallback glyphs, native controls, fullscreen, scaling, mixed displays/DPI, and display pacing.
- Repeat the UI state/size/scale matrix on Mac and verify image paste, file drop, preview, inspector, save, and reconnect behavior.
- Keep native Linux core CI passing and qualify the Linux graphical client separately; cross-compilation and headless tests are insufficient graphical evidence.
- Run focused regressions and final workspace/dependency checks after integration. Report unavailable physical-platform checks explicitly.

## 4. Distribution and process discovery

- Qualify one real SSH host end to end with host-key verification and authentication: install the matching daemon, launch/attach/detach, drop the relay, reconnect to the same server and process lifetime, upload an image, and exercise the remote headless commands. Deterministic relay tests and the actual OpenSSH failure path are complete, but no reachable SSH server was available for a successful real-host run.
- Qualify Windows signing with an Authenticode certificate and macOS Developer ID signing/notarization with Apple credentials when those credentials and a Mac are available. Unsigned Windows and ad-hoc macOS artifacts must remain explicit otherwise.
- Run a version-to-version Windows upgrade and current clean-profile package checks on a disposable host. The existing hosted install/repair/uninstall lifecycle is complete but did not have an older release to upgrade.
- Add optional non-secret agent discovery metadata only when a concrete agent consumer defines the required identifier and kind. Do not infer agents from command lines or process names, and keep credentials, memory, steering, and orchestration outside the process protocol.
- Run the tag-triggered workflow from the final committed source and publish only after both package jobs, merged checksums, licenses, native launch, and reconnect checks pass.

## Completion gate

The shared baseline is complete when the owner can build, run, develop, and dogfood the native client on Mac and Windows; representative workspace workflows and mixed-pane soak pass on both; latency/resource results are attributable; and any remaining distribution-only gaps are explicit.
