# Compi next steps

This file tracks only unfinished work. Completed implementation and verification history is in [Completed work](COMPLETED.md).

Work in the order below. Keep changes on the existing workspace, persistence, protocol, and navigation contracts unless a listed item requires a narrow extension.

## 1. Remote SSH transport

- Add daemon framing on stdin/stdout behind `--server-stdio`.
- Add a client-spawned `ssh -T` connection behind `--connect [user@]host[:port]`, using batch mode and sshd authentication. Do not add a network listener or stored credentials.
- Resolve instance names and paths on the remote host. Keep local peer-identity checks for local endpoints; a remote peer must never satisfy them.
- Prove the smallest real local-to-remote attach and reconnect path first.
- Cover host-key and authentication failures, dropped connections, snapshot resync, sequence gaps, process-lifetime preservation, and exclusive control of each surface.

## 2. Remaining UI/UX qualification

- Preserve the completed top-tab navigation, optional workspace sidebar, flat responsive overlays, typography, and compact control system. Change them only where native evidence exposes a defect.
- Capture the still-unqualified Windows states: empty, loading, disconnected, exited, failed, lost, confirmation, recovery, drag/overflow, and mixed-DPI transitions. Cover normal, narrow, maximized, split, sidebar, palette, and theme-picker layouts at representative display scales.
- Verify hover, focus, active, disabled, pending, and destructive states without relying on color alone. Keep New terminal, pane actions, and custom Windows controls visible, reachable, and non-overlapping.
- Exercise physical keyboard navigation, terminal input, native drag, maximize, close, platform shortcuts, clipboard, IME/dead keys, exact user fonts, and fallback glyphs on supported Windows display configurations.

## 3. Performance and daily-use qualification

- Measure key receipt → PTY → replica → presentation latency, scrolling, resizing, and frame pacing under idle and sustained-output workloads. Use the current-FPS overlay for live feedback, not as a substitute for attributed timing.
- Record build and display context and compare identical workloads before and after any fix. Confirm opt-in continuous frame scheduling remains inactive when Performance and the FPS overlay are both hidden.
- Fix only demonstrated stalls, avoidable shaping or allocation, repaint scheduling, or queue problems. Keep caches and transport queues bounded; do not replace the terminal engine.
- Run `tools/measure-release.ps1` and `tools/soak-release.ps1` for the shared daily-use matrix.
- Produce attributable resource measurements and runnable Windows and Mac dogfood artifacts.

## 4. Native Mac and shared-platform qualification

- Qualify the bundled application, not a bare `cargo run`, across the complete workspace flow: shell and fullscreen-editor input, resize, close/reopen, same-process persistence, workspace/tab/split operations, tear-off and transfer, Settings, appearance, and recovery states.
- Exercise physical Cmd/Ctrl/Option keys, clipboard, dead keys/IME, exact user font and fallback glyphs, native controls, fullscreen, scaling, mixed displays/DPI, and display pacing.
- Repeat the UI state/size/scale matrix on Mac and verify image paste, file drop, preview, inspector, save, and reconnect behavior.
- Keep native Linux core CI passing and qualify the Linux graphical client separately; cross-compilation and headless tests are insufficient graphical evidence.
- Run focused regressions and final workspace/dependency checks after integration. Report unavailable physical-platform checks explicitly.

## 5. Remaining graphics work

- Add remote image upload semantics after SSH transport is stable.
- Add image-aware logical-line reanchoring during reflow.
- Decide and document the required sixel and remaining Kitty protocol coverage before implementation.
- Re-run bounded storage, transfer, decoder, frame-queue, cache-admission, expiry, clear, resize, and detach/reattach checks for each added graphics path.

## 6. Distribution and headless use

- Inspect the outstanding Windows installer lifecycle result from the existing release run and rerun it only if current source or unavailable evidence requires it.
- Qualify Windows signing and macOS Developer ID signing/notarization when credentials are available; retain explicit unsigned/ad-hoc behavior otherwise.
- Complete remaining process-discovery and headless-use work from Phase 6.
- Keep tag releases gated on all required package jobs and verify produced installers, archives, checksums, licenses, launch, and reconnect behavior.

## Completion gate

The shared baseline is complete when the owner can build, run, develop, and dogfood the native client on Mac and Windows; representative workspace workflows and mixed-pane soak pass on both; latency/resource results are attributable; and any remaining distribution-only gaps are explicit.
