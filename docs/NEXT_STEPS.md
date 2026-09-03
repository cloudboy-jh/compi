# Compi next steps

Compi currently implements the persistent WSL2 session daemon, authoritative terminal state, protocol-v5 transport, and usable GPUI client described by Milestones 0–3. The remaining work below closes the unfinished P0 release requirements before Milestone 4 begins.

## Completed product baseline

- [x] Run multiple WSL2 Bash sessions through a persistent per-user Windows daemon.
- [x] Detach and reattach the GPUI client without interrupting live shell work.
- [x] Preserve authoritative terminal state, local scrollback, sequenced-delta recovery, and Kitty graphics over protocol v5.
- [x] Support tabs, selection, mouse input, native titlebar controls, and tab-body window dragging.
- [x] Support terminal-safe copy and paste, including `Ctrl+V`, `Ctrl+Shift+V`, and uninterrupted `Ctrl+C`.
- [x] Start new sessions in the Linux user's home directory without injecting shell startup commands.
- [x] Pass the full Rust regression suite, release build, and end-to-end persistence acceptance checks.
- [x] Ship only `compi.exe` and `compi-daemon.exe` in normal product builds; retain the diagnostic probe as an explicitly built example.
- [x] Use compact binary screen frames, inline cell text, bounded screen backpressure, visible-row paint models, cached row shaping, and background Kitty image decoding.
- [x] Wake the GPUI client from terminal events and cap event application to a display-paced UI budget instead of repainting from a fixed polling loop.
- [x] Provide a tiered terminal acceptance matrix in `docs/testcmds.md`.

## 1. Release contracts

The active roadmap follows the P0 contract in `Spec.md`: Windows client and daemon, WSL2 Bash through ConPTY, per-user named pipes, installer, supervision, compatibility, and release qualification. Milestone 4 begins only after those gates pass. Native Unix process hosting, Unix-domain IPC, remote transport, and agent-scale parking remain post-Milestone-4 work.

### Lifecycle truth table

| Event | Running shell truth | Daemon behavior | Client-visible result |
|---|---|---|---|
| GUI closes or crashes | Continues | Unchanged | Session becomes detached and can be reattached. |
| Shell exits normally | Ends | Retains terminal status metadata | Session is `exited` with its exit code. |
| User kills a session | Ends | Terminates and cleans up its ConPTY/WSL job | Session is `exited`; it is never recreated automatically. |
| Daemon receives intentional shutdown | Ends | Marks active records dead, cleans up, and exits successfully | Supervisor does not restart it; no shell is reported as recovered. |
| Daemon fails unexpectedly | Ends with the daemon | The scheduled `--supervise` process restarts a daemon child after a nonzero exit | On restart, prior `starting` and `running` records become `dead`. |
| WSL shuts down | Ends | Detects each exited `wsl.exe`; daemon remains available | Affected sessions become `exited` or `failed`, never recoverable. |
| Windows user signs out | Ends | Per-user daemon exits with the user session | At next sign-in, prior active records become `dead`. |
| Compi is upgraded | Ends after an explicit warning | Supervision is paused, daemon is stopped, binaries are replaced, then supervision resumes | Active records become `dead`; upgrade never claims process continuity. |
| Compi is uninstalled | Ends | Daemon and startup registration are removed before binaries | Binaries and application-owned state follow the documented uninstall policy. |

### Persisted session-record contract

- Persist metadata at `%LOCALAPPDATA%\Compi\sessions-v1.json`; the daemon is the only writer.
- Store `format_version` and, per session, `id`, `created_at_ms`, `updated_at_ms`, `status`, `cols`, `rows`, optional `exit_code`, and optional `error`.
- Valid persisted states are exactly `starting`, `running`, `exited`, `failed`, and `dead`.
- Write through a sibling temporary file and atomically replace the prior manifest after each lifecycle transition. Ignore and quarantine a malformed manifest rather than inventing live state.
- On daemon startup, map persisted `starting` and `running` records to `dead` with a daemon-loss reason before accepting clients.
- Never persist process handles, client attachment state, terminal input, screen state, scrollback, or Kitty payloads as recoverable process state.
- Retain at most 100 terminal records and prune terminal records older than 30 days on daemon startup.
- Adding `dead` to the public session status requires a protocol-version bump and explicit unsupported-peer errors.

### P0 release order

1. Establish Windows CI and attributable release measurements.
2. Implement persisted dead-session reporting and lifecycle hardening.
3. Add per-user daemon supervision.
4. Build and verify install, upgrade, repair, and uninstall.
5. Close terminal compatibility, startup, memory, display, and soak gates.
6. Produce and qualify versioned signed Windows artifacts.

## 2. Establish repeatable measurement and CI

- [x] Add Windows CI for formatting, strict Clippy, all-target tests, and release binaries.
- [x] Discover `fxc.exe` from the installed Windows SDK and set `GPUI_FXC_PATH` to the executable path for release builds.
- [x] Measure blank-window client cost separately from one-session and additional-session costs.
- [x] Record GUI and daemon private bytes, working set, handle count, and dedicated/shared GPU memory separately.
- [x] Instrument cold versus warm daemon connection and a synthetic-input-to-rendered-marker ready-for-input round trip.
- [ ] Run at least ten launch samples on a physical display and record p50, p95, and worst values under the `release-targets.md` measurement contract.

Memory work is evidence-driven. The current approximately 95 MiB failure is a `compi.exe` measurement; it does not justify daemon scrollback compression or PTY parking. Establish the blank GPUI baseline and marginal client/daemon cost first. If the 35 MiB release ceiling still fails, profile allocations and either optimize them or record an explicit approved release exception.

## 3. Finish Milestone 3 and P0

### Session lifecycle hardening

- [x] Implement the persisted session-record contract above and display daemon-lost sessions as dead.
- [ ] Exercise daemon crash, forced WSL shutdown, Windows sign-out, and malformed or stale metadata recovery.
- [x] Verify that one controlling client remains enforced during reconnect races and concurrent client launches.
- [x] Verify clean cleanup of ConPTY, WSL, job, pipe, and client resources after shell exit, explicit kill, and daemon shutdown.
- [ ] Add soak coverage for repeated create, attach, detach, resize, and kill cycles.

### Installer and daemon supervision

- [x] Register a per-user Task Scheduler entry at sign-in without requiring administrator privileges.
- [x] Restart the daemon child only after an unexpected nonzero exit; intentional shutdown stops the supervising task without a restart loop.
- [x] Start the GUI independently. If the daemon is unavailable, activate or await the registered task instead of creating an unmanaged daemon.

The task runs `compi-daemon.exe --supervise` under the current user's interactive token at least privilege. The supervising process launches the normal daemon as a child, writes `%LOCALAPPDATA%\Compi\daemon.log`, and retries nonzero exits after 1, 5, and 30 seconds. A successful intentional daemon exit ends supervision. Isolated `--instance` runs continue to launch directly and never use the registered task.

- [x] Build a per-user Windows installer for `compi.exe`, `compi-daemon.exe`, and required assets.
- [x] Provide a branded GPUI setup surface plus an independent preview executable for ready, upgrade, installing, complete, error, and remove states.
- [ ] Detect unsupported Windows versions, missing WSL, WSL1-only defaults, and missing default distributions with actionable messages. Missing WSL, WSL1 defaults, and missing default distributions are covered; the explicit supported-Windows-version gate remains.
- [x] Define and implement upgrade, repair, and uninstall handling for locked binaries, startup registration, logs, and session metadata.
- [ ] Verify install, upgrade, repair, and uninstall on a clean non-administrator Windows user profile. Install, repair, and complete uninstall passed under a non-elevated token after clearing local pre-release product state; a fresh profile and a real version-to-version upgrade remain.

### Terminal compatibility

- [ ] Run the complete matrix in `docs/testcmds.md` against Bash, `top`, `less`, `vim`, `nano`, Git prompts, and alternate-screen applications.
- [ ] Cover Ctrl, Alt, Shift, function, navigation, and application-key sequences with a physical keyboard.
- [ ] Verify Unicode, combining marks, wide characters, emoji, bracketed paste, mouse reporting, and selection across wrapped rows.
- [ ] Exercise Kitty image chunking, zlib payloads, placement, deletion, clipping, scrollback, resize, detach, and reattach.
- [ ] Confirm every client delta gap recovers from a fresh snapshot without duplicated or lost terminal state.

### Project working directories

Compi remains WSL-native while allowing a session to start directly in a project stored on either filesystem. It resolves the requested directory once at spawn and then lets WSL and the shell access the project in place. Compi does not copy, mirror, synchronize, or virtualize project files.

- [ ] Add an optional working directory to session creation and persist it as session metadata.
- [ ] Accept absolute WSL paths and absolute Windows paths. Resolve Windows paths through the selected WSL distribution before spawning Bash; keep `~` as the default when no directory is supplied.
- [ ] Track the shell's current directory through OSC 7 so a new tab can inherit the active session's directory without injecting `cd` commands or rewriting shell startup files.
- [ ] Warn, but do not block, when a Windows-hosted project is under OneDrive or another synchronized directory.
- [ ] Use the same resolved working directory contract for human and agent sessions so both operate on the same project in place.
- [ ] Cover WSL and Windows paths with spaces, Unicode, mixed case, missing directories, and unavailable distributions.
- [ ] Verify Git and two representative agent harnesses against projects under both `/home/...` and `/mnt/c/...`, with edits immediately visible to the Windows editor and Explorer where applicable.
- [ ] Measure Linux-filesystem and mounted-Windows-filesystem workloads separately. Filesystem results must not be reported as terminal renderer or protocol results.

## 4. Close the release-feel gates

- [ ] Reduce warm first-window p95 below 100 ms and warm ready-for-input p95 below 200 ms, or record an explicit approved release exception.
- [ ] Measure cold start with no daemon and keep first-terminal-frame p95 below 500 ms.
- [ ] Keep input, local scrollback, selection, tab switching, and window controls responsive while another session emits sustained output.
- [ ] Validate normal, narrow, wide, maximized, minimized, mixed-DPI, and multi-monitor behavior.
- [ ] Verify physical 100/120 Hz frame pacing and physical `Ctrl+C` under sustained output.
- [ ] Run the 30-minute mixed memory, GPU-memory, handle, and process-leak soak.
- [ ] Pass all three acceptance tiers from the same release build used for performance measurements.

Current measured baseline on 2026-09-02:

- Six release-mode warm launches reached the first window frame in 344–409 ms (median 358 ms) and the first terminal frame in 381–467 ms (median 400 ms). The launch targets are not met.
- Under sustained output on the Meta Virtual Monitor, terminal paint p50 was 229–234 µs and p95 was 453–458 µs. Frame-interval p50 was about 28.1 ms and p95 about 35.5 ms; this is not physical-display release evidence.
- The release GUI executable is 12.0 MB and the daemon is 991 KB.
- The blank GPUI client used 86.15 MiB private bytes at p50; a warm one-session client used 90.09 MiB at p50. The daemon used about 2 MiB, and a diagnostic one/two/four-session run measured about 0.6 MiB of marginal daemon private memory per blank session.
- Protocol flood recovery passes, including controlling-client attachment, `Ctrl+C`, snapshot recovery, and a clean prompt marker.

Detailed evidence and remaining manual checks: [`ACCEPTANCE_RESULTS_2026-09-02.md`](ACCEPTANCE_RESULTS_2026-09-02.md).

## 5. Establish repeatable Windows releases

- [ ] Produce versioned installer and portable artifacts from tagged builds.
- [ ] Embed the same version metadata in the GUI, daemon, installer, and artifact names.
- [ ] Sign both executables and the installer.
- [ ] Publish checksums and concise install, upgrade, uninstall, and troubleshooting instructions.
- [ ] Qualify the exact signed artifacts on supported clean Windows 10 and Windows 11 WSL2 environments.

## 6. Milestone 4: agent sessions

Start only after the P0 installer, supervision, compatibility, performance, and release gates pass.

### Spawn contract

- [ ] Decide whether agent creation starts the normal interactive WSL Bash for later input or accepts an explicit command, arguments, working directory, and environment.
- [ ] Keep process hosting in Compi while leaving memory, steering, credentials, and orchestration outside Compi.
- [ ] Reuse the human-session lifecycle and authorization model rather than creating a second management path.

### Protocol and daemon

- [ ] Add a session driver field with exactly `human` and `agent` variants.
- [ ] Add optional agent name, repository, and external session ID metadata.
- [ ] Version the protocol change and preserve explicit decoding errors for unsupported peers.
- [ ] Add headless agent-session creation with no controlling client attached.

### Client and acceptance

- [ ] Display both drivers in the existing tab strip and detached-session switcher.
- [ ] Show available agent metadata without making it mandatory for session operation.
- [ ] Preserve attach, detach, reconnect, resize, kill, and snapshot recovery for both drivers.
- [ ] Spawn a real agent process headlessly, discover it in the GUI, attach, detach, and reattach without interrupting it.
- [ ] Exercise mixed human and agent sessions through daemon failure reporting and stale metadata recovery.

## Deferred beyond Milestone 4

- `PtySession` abstraction and native Unix process hosting.
- Unix-domain IPC and any transport discovery mechanism.
- Linux daemon, remote access, and macOS client work.
- IOCP/epoll shared PTY polling, screen-state parking, scrollback compression, and client-buffer parking, after marginal measurements justify them.
- Daemon/reboot survival of running processes, multi-viewer broadcast, split layouts, ligatures, animation polish, persisted scrollback, preferences UI, forced themes, Sixel, default-terminal registration, and embedded multi-agent orchestration.
