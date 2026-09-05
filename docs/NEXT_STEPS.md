# Compi next steps

Compi is the native Windows client for persistent WSL human and agent sessions. It is not trying to become a generic cross-platform terminal or beat every existing terminal on raw startup and memory.

The persistent daemon, protocol-v7 session lifecycle, metadata, supervision, installer, packaging, GPUI client, and terminal truth implementation are substantially complete. Release work remains paused for the operator-only physical keyboard/display gates and clean-machine qualification, not for known terminal-model feature gaps.

## Completed foundation

- Multiple independent WSL2 Bash sessions survive client detach, close, and reattach.
- The daemon owns ConPTY/WSL processes, terminal state, bounded scrollback, sequenced screen recovery, and persisted lifecycle metadata.
- Per-user supervision, install, repair, removal, portable packaging, version metadata, checksums, Windows CI, and release instrumentation exist.
- Sessions start in the Linux home directory or a validated WSL/Windows project directory; OSC 7 reports can drive directory inheritance.
- Client and daemon resource measurement, sustained-output testing, and create/kill soak coverage exist.

Closing the GUI or a tab detaches the client and leaves the shell running. Shell exit, explicit session termination, daemon loss, WSL shutdown, sign-out, upgrade, and uninstall retain the lifecycle semantics already defined in `Spec.md`: dead processes are never presented as recoverable sessions.

## Terminal truth sprint status

The implementation sprint is complete. Keep signing, installer release work, and Milestone 4 agent-session features paused until the two remaining physical-keyboard checks close:

- [x] Interactive Bash editing, completion, job control, and prompts.
- [x] `less`.
- [x] `vim` or `nvim`.
- [x] `fzf` with an inline preview.
- [x] OpenCode as the representative real agent harness.

The five workflows were exercised through the real Compi GUI and captured terminal streams. OSC 8 hyperlinks and size-limited, write-only OSC 52 clipboard updates are implemented; unsupported or rejected sequences are categorized and rate-limited with session/trace context.

### Compatibility failure workflow

Use this loop for every visible failure:

1. Reproduce the visible problem in a release build.
2. Capture the exact input and terminal byte stream.
3. Record unsupported or incorrectly handled CSI, ESC, OSC, DCS, and APC sequences.
4. Reduce the failure to the smallest deterministic replay.
5. Add a regression test that fails before the fix.
6. Fix the terminal model or GUI input/render boundary.
7. Re-run the focused test, full Rust suite, and affected interactive workflow.

### Ranked implementation checklist

Work in this order unless a reduced replay proves a lower item blocks an earlier workflow:

1. [x] Keyboard sequences, modifiers, `Ctrl+C`, `Ctrl+Z`, `Ctrl+D`, and application cursor/keypad modes. With a non-empty terminal selection, `Ctrl+C` copies without sending PTY input; without a selection, it sends the interrupt byte. `Ctrl+Shift+C` remains an explicit copy shortcut.
2. [x] Cursor movement, save/restore, origin mode, margins, scroll regions, insert/delete behavior, and alternate-screen transitions.
3. [x] Resize/reflow correctness and redraw behavior under sustained output.
4. [x] Mouse reporting, focus reporting, wheel input, and Shift-based selection override.
5. [x] OSC 8 hyperlink parsing, cell metadata, hit testing, safe Windows URL opening, and cursor feedback.
6. [x] OSC 52 clipboard support with explicit size and security limits.
7. [x] Unicode graphemes, combining marks, wide cells, emoji, selection, and copy behavior.
8. [x] Unsupported-sequence diagnostics that identify the originating workflow without flooding logs.

### Required session controls

- [x] Add an `End session` action only to the terminal session list, with inline confirmation and an `Ending…` state.
- [x] Terminating either an attached or detached session ends its shell and descendant processes, updates its lifecycle state, and closes any attached tab. Closing a tab or the client remains detach-only and never ends the shell.

### Completion gates

- [x] All five workflows complete without visible corruption, stuck input, incorrect cursor state, broken resize, or a required restart.
- [x] Every discovered compatibility failure has a deterministic regression test; bounded opt-in traces capture input, output, resize, timing, and workflow context for future reductions.
- [ ] Physical `Ctrl+C` interrupts sustained output promptly.
- [ ] With selected text, physical `Ctrl+C` copies the exact selection without interrupting the foreground process; with no selection, it sends `0x03`. `Ctrl+Shift+C` copies in both cases.
- [x] The session list can end attached and detached sessions after inline confirmation; the process tree exits, lifecycle state updates, and ordinary tab/client close still preserves the session.
- [x] Input-to-present latency is correlated at these boundaries:
  1. GPUI key event receipt.
  2. Client queue and daemon send.
  3. Daemon input receipt.
  4. PTY output receipt.
  5. Terminal-state update and screen sequence.
  6. Next frame presentation.
- [x] A 120-keystroke interactive sample reported 29.347 ms p50, 36.861 ms p95, and 45.175 ms worst input-to-present latency with all 120 IDs complete and ordered.
- [x] Terminal paint remains below the available frame budget; the highest recorded per-window paint p95 during the qualification work was 2.795 ms.
- [x] A 30-minute mixed-TUI run completed 196 `less`/Vim/`fzf`/sustained-output/resize cycles and 62 resource samples. The final 15 minutes showed no monotonic private-byte, working-set, GPU-memory, or handle growth; the trace stayed within its 16 MiB bound.

### Architecture baselines, not terminal-correctness gates

Current warm measurements are approximately 376 ms to first window, 544 ms to ready for input, and 89–96 MiB of client private memory. The earlier launch-marker input-to-render p95 was 153 ms; direct correlated input-to-present measurement now reports 36.861 ms p95 in an automated 120-keystroke run. These are baselines for the current GPUI architecture, not terminal compatibility failures and not reasons to replace GPUI.

Keep GPUI's provisional release budgets separate from the correctness gates above. The old 100 ms startup and 35 MiB client-memory aspirations are longer-term architecture targets; this sprint does not require them. Performance work during the sprint is limited to instrumenting the complete input-to-present path, staying within the available paint budget, and preventing resource growth.

## Immediate priority: physical and release qualification

Terminal implementation is no longer the release blocker. Complete the remaining operator and release gates in this order:

- Confirm sustained-output interruption and selection-aware copy with a physical keyboard; OS-synthesized key events already pass through the GPUI path but do not satisfy the physical-input gate.
- Exercise daemon crash, forced WSL shutdown, Windows sign-out, and malformed or stale metadata recovery.
- Verify fresh-profile install, real version-to-version upgrade, repair, and uninstall as a non-administrator.
- Run physical-display pacing, mixed-DPI, multi-monitor, window-state, and launch measurements.
- Produce, sign, and qualify the exact versioned installer and portable artifacts on supported Windows 10 and Windows 11 WSL2 systems.
- Validate Git and project workflows under both `/home/...` and `/mnt/c/...` without reporting filesystem performance as terminal performance.

## Milestone 4 agent sessions after release qualification

Milestone 4 remains paused until terminal truth and release qualification pass. When it resumes, human and agent sessions must share one process-hosting, authorization, lifecycle, attach/detach, reconnect, resize, termination, and screen-recovery model. Compi may add agent driver and discovery metadata, but memory, steering, credentials, and orchestration remain outside Compi.

## Deferred scope

- Native Unix process hosting, Unix-domain IPC, Linux daemon, remote transport, and macOS client work.
- IOCP/epoll shared PTY polling, screen-state parking, scrollback compression, and client-buffer parking until measurements justify them.
- Daemon/reboot survival of running processes, multi-viewer broadcast, split layouts, ligatures, animation polish, persisted scrollback, preferences UI, forced themes, Sixel, default-terminal registration, and embedded multi-agent orchestration.
