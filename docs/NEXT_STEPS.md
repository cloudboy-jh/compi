# Compi next steps

Compi currently implements the persistent WSL2 session daemon, authoritative terminal state, protocol-v4 transport, and usable GPUI client described by Milestones 0–3. The remaining work below closes the unfinished P0 release requirements before Milestone 4 begins.

## Completed product baseline

- [x] Run multiple WSL2 Bash sessions through a persistent per-user Windows daemon.
- [x] Detach and reattach the GPUI client without interrupting live shell work.
- [x] Preserve authoritative terminal state, local scrollback, protocol-v2 recovery, and Kitty graphics.
- [x] Support tabs, selection, mouse input, native titlebar controls, and tab-body window dragging.
- [x] Support terminal-safe copy and paste, including `Ctrl+V`, `Ctrl+Shift+V`, and uninterrupted `Ctrl+C`.
- [x] Start new sessions in the Linux user's home directory without injecting shell startup commands.
- [x] Pass the full Rust regression suite, release build, and end-to-end persistence acceptance checks.
- [x] Ship only `compi.exe` and `compi-daemon.exe` in normal product builds; retain the diagnostic probe as an explicitly built example.
- [x] Use compact binary screen frames, inline cell text, bounded screen backpressure, visible-row paint models, cached row shaping, and background Kitty image decoding.
- [x] Wake the GPUI client from terminal events and cap event application to a display-paced UI budget instead of repainting from a fixed polling loop.
- [x] Provide a tiered terminal acceptance matrix in `docs/testcmds.md`.

## 1. Finish Milestone 3 and P0

### Installer and daemon supervision

- [ ] Build a Windows installer that installs `compi.exe`, `compi-daemon.exe`, and the required application assets.
- [ ] Detect Windows and WSL prerequisites during setup and report actionable failures for unsupported Windows versions, missing WSL, WSL1-only systems, and missing default distributions.
- [ ] Register per-user daemon startup without requiring administrator privileges.
- [ ] Restart the daemon after an unexpected daemon failure. Do not imply that shells lost with the daemon were recovered.
- [ ] Start the GUI independently from daemon supervision; opening and closing the client must not define daemon lifetime.
- [ ] Define upgrade and uninstall behavior for binaries, startup registration, logs, and persisted session metadata.
- [ ] Verify install, upgrade, repair, and uninstall on a clean Windows user profile.

### Session lifecycle hardening

- [ ] Show sessions whose process died with the daemon as dead instead of live or recoverable.
- [ ] Exercise daemon crash, forced WSL shutdown, Windows sign-out, and stale metadata recovery.
- [ ] Verify that one controlling client remains enforced during reconnect races and concurrent client launches.
- [ ] Verify clean cleanup of ConPTY, WSL, job, pipe, and client resources after shell exit, explicit kill, and daemon shutdown.
- [ ] Add soak coverage for repeated create, attach, detach, resize, and kill cycles.

### Terminal compatibility pass

- [ ] Run the interactive acceptance matrix against Bash, `top`, `less`, `vim`, `nano`, Git prompts, long-running commands, and alternate-screen applications.
- [ ] Cover keyboard combinations that Windows terminals commonly mishandle, including Ctrl, Alt, Shift, function, navigation, and application-key sequences.
- [ ] Verify Unicode input and output, combining marks, wide characters, emoji, bracketed paste, mouse reporting, and selection across wrapped rows.
- [ ] Exercise Kitty image chunking, zlib payloads, placement, deletion, clipping, scrollback, resize, detach, and reattach with representative applications.
- [ ] Confirm that client delta gaps always recover from a fresh snapshot without duplicating or losing terminal state.

## 2. Meet the release feel bar

- [x] Measure cold and warm GUI launch-to-first-shell times using the startup instrumentation.
- [x] Measure detached-session reattach time and identify synchronous work on the GPUI render/input path.
- [ ] Keep terminal input, local scrollback, selection, tab switching, and window controls responsive while another session emits sustained output. Protocol-level flood interruption and recovery pass; the full interactive GUI soak remains open.
- [ ] Validate normal, narrow, wide, maximized, minimized, mixed-DPI, and multi-monitor window behavior.
- [x] Verify that a normal launch creates a fresh session while detached sessions remain available through the switcher.
- [ ] Run an extended memory, handle, and process-leak soak with multiple active and detached sessions.

Current measured baseline on 2026-09-02:

- Six release-mode warm launches reached the first window frame in 344–409 ms (median 358 ms) and the first terminal frame in 381–467 ms (median 400 ms). The sub-200 ms interaction target is not met.
- Under sustained output on the Meta Virtual Monitor, terminal paint p50 was 229–234 µs and p95 was 453–458 µs. Observed frame-interval p50 was about 28.1 ms and p95 about 35.5 ms; repeat on the physical 120 Hz display before judging the 8,333 µs target.
- The release GUI executable is 12.0 MB and the daemon is 991 KB.
- The release GUI used roughly 98–100 MB private bytes and 63–64 MB working set under the sampled workload. The initial 35 MB private-byte target is not met; GPUI/platform baseline measurement is still required.
- The flood regression now proves that a controlling client stays attached, sends `Ctrl+C`, recovers a dropped-delta gap from a snapshot, and receives a clean prompt marker after three seconds of unbounded output.

Detailed evidence and remaining manual checks: [`ACCEPTANCE_RESULTS_2026-09-02.md`](ACCEPTANCE_RESULTS_2026-09-02.md).

## 3. Establish repeatable Windows releases

- [ ] Add Windows CI for formatting, tests, strict Clippy, and release builds.
- [ ] Provision the Windows SDK shader compiler in CI or document a reproducible `GPUI_FXC_PATH` configuration.
- [ ] Produce versioned installer and portable artifacts from a tagged build.
- [ ] Embed version metadata consistently in the GUI, daemon, and installer.
- [ ] Sign release binaries and the installer before distribution.
- [ ] Publish checksums and concise install, upgrade, uninstall, and troubleshooting instructions.
- [ ] Verify release artifacts on supported clean Windows 10 and Windows 11 environments with WSL2.

## 4. Milestone 4: agent sessions

Start this only after the P0 installer, supervision, compatibility, and release gates above pass.

### Protocol and daemon

- [ ] Add a session driver field with exactly `human` and `agent` variants.
- [ ] Add optional agent metadata for agent name, repository, and external session ID.
- [ ] Version the protocol change and preserve explicit decoding errors for unsupported peers.
- [ ] Add headless agent-session creation with no controlling client attached.
- [ ] Keep process hosting in Compi while leaving memory, steering, and orchestration outside Compi.

### Client

- [ ] Display agent sessions in the existing tab strip and detached-session switcher.
- [ ] Distinguish human and agent sessions without creating a second session-management model.
- [ ] Show available agent metadata without making it mandatory for session operation.
- [ ] Preserve the same attach, detach, reconnect, resize, kill, and snapshot-recovery behavior for both drivers.

### Acceptance

- [ ] Spawn an agent session headlessly, observe it in the GUI, attach, detach, and reattach without interrupting the process.
- [ ] Exercise mixed human and agent sessions through daemon restart failure reporting and stale metadata handling.
- [ ] Verify that agent metadata survives client disconnects and cannot corrupt framing or terminal state.

## Deferred beyond Milestone 4

These remain intentionally outside the immediate plan: daemon/reboot survival of running processes, remote or Linux daemon mode, macOS client support, multi-viewer broadcast, split layouts, ligatures and animation polish, persisted scrollback across daemon restart, preferences UI, forced themes, Windows-path translation, Sixel, default-terminal registration, and embedded multi-agent orchestration.
