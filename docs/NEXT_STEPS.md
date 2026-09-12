# Compi next steps

The Phase 4 workspace client is implemented on top of Phases 0–3. Native Windows/WSL workspace qualification passed, and the current Windows terminal interaction pass is complete. Focused UI/UX refinement is next; native Mac workspace qualification, Linux native CI, performance/resource measurement, and remaining physical-input/display gates are still open. Earlier platform evidence and its limits are retained below.

## Completed: Phase 0 — implementation contracts

- [x] Define process-lifetime identity and replica/history/selection invalidation when a surface restarts without a server restart. See [Process lifetimes and terminal identity](Spec.md#process-lifetimes-and-terminal-identity): stable surface, fresh lifetime per launch attempt, keyed messages/caches, fresh attachment/snapshot after restart, and explicit anchor invalidation.
- [x] Define durable workspace mutation acknowledgements, revision publication, and recovery after process-launch or persistence failure. See [Durable workspace mutations](Spec.md#durable-workspace-mutations): persist before publish/acknowledge, durable intent before side effects, bounded mutation receipts, retained failed panes, and truthful read-only recovery after uncertain persistence.
- [x] Define durable client identity, hidden-tab membership, remembered navigation, and behavior across relaunch and simultaneous windows. See [Durable client identity and navigation](Spec.md#durable-client-identity-and-navigation): exclusively owned remembered window slots, client-local hidden sets, deterministic navigation fallback, and no implicit shell creation or control takeover.
- [x] Define behavior when an existing split tree no longer fits the window. See [Constrained split layout](Spec.md#constrained-split-layout): 20-column by 4-row minimum canvases, recursive minima, scrollable workspace overflow, and reachable focus without automatic sidebar collapse, pane zoom, or saved-tree changes.
- [x] Map the specification's acceptance requirements to the implementation sequence, retaining useful Windows terminal regressions and identifying missing Unix/platform coverage. See [Acceptance-to-phase map](Spec.md#acceptance-to-phase-map) and [Phase 0 decision scenarios](Spec.md#phase-0-decision-scenarios).

The [first extraction change](Spec.md#first-extraction-change) now has explicit scope, exclusions, and observable acceptance criteria. These completed checkboxes mean the contracts are defined, not that their behavior is implemented or runtime-qualified.

## Phase 1 — implementation complete, qualification pending

The code changes in [Phase 1](Spec.md#1-extract-the-neutral-foundation) are implemented. The phase is not marked qualified: native Linux/Windows test execution, native Windows builds, and real Windows/WSL interaction remain outstanding.

### Implemented extraction

- Captured pre-extraction v7 fixtures for 12 client controls, 18 server controls, an initial screen snapshot, and nine output/resize steps. New codecs and engine output preserve those captured values and bytes.
- Established the three product crates: `compi-protocol`, `compi-daemon`, and `compi-client`. Protocol values and local transport are shared; terminal state and OS hosting belong to the daemon; replicas, probe, interaction logic, and GPUI belong to the client.
- Extracted pure input/mouse/paste/focus encoding, selection, viewport lookup, hyperlink policy, and shared theme constants into the client without compatibility shims.
- Kept engine-to-wire conversion at the daemon boundary without additional grid copies. The daemon's production dependencies contain no GPUI, client application, or installer package.
- Moved installer UI into its existing isolated `installer/bootstrapper` package. Preserved its locked registry versions while updating local package dependencies.
- Added [three-host neutral CI](../.github/workflows/core-ci.yml), including v7 fixtures, replica recovery, resolved dependency-boundary checks, and a native Windows daemon build without graphics setup. The unreliable standalone hosted Windows product/installer workflow was removed; native Windows product verification remains in [the acceptance recipes](testcmds.md), while signed distribution remains tag-triggered in `windows-release.yml`.

### Verification from this pass

| Check | Result |
|---|---|
| `cargo test --workspace --all-targets` on macOS | 36 tests passed. Windows-gated runtime/UI tests did not execute. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` on macOS | Passed for active host code. |
| `cargo check --locked -p compi-daemon --all-targets --target x86_64-pc-windows-msvc` | Passed, including Windows daemon/client transport/probe/integration-test compilation. No native linking or runtime claim. |
| Product crates `cargo check --all-targets --target x86_64-unknown-linux-gnu` | Passed. Cross-compilation is not Linux test execution. |
| `tools/check-dependencies.py` for macOS, Linux, and Windows | All configured production/build boundaries passed. Dev-only edges are excluded. |
| Separate executable linked to the extracted engine/protocol/server boundary | Repeated the original capture workload; both fixture files were byte-identical, including terminal replies, deltas, resize/reflow, modes, and graphics. Temporary capture/smoke programs were removed after verification. |
| Isolated installer `cargo check --offline --manifest-path installer/bootstrapper/Cargo.toml --all-targets` on macOS | Passed package/lock resolution and unsupported-host targets only, not Windows installer UI or packaging. |
| Windows app cross-check from macOS | Blocked in dependency `ring` before application type-checking: Windows C headers/toolchain unavailable (`assert.h` missing). Native app build and visual behavior remain unverified. |
| Native Linux/Windows CI, Windows resource embedding, WSL interaction, installer build | Not run in this pass. No remote runtime host is configured. |

### Remaining Phase 1 gate

1. Run the configured neutral CI jobs on Linux, macOS, and Windows.
2. Build the real Windows daemon without graphics tooling, then build the app/probe and isolated installer with the normal Windows SDK setup.
3. On a qualified Windows/WSL machine, run `cargo test --locked -p compi-daemon --test daemon_integration` and the retained interactive recipes, especially input, resize, detach/reattach, selection/clipboard, and explicit descendant termination.

Do not count a cross-check, a configured workflow, or an unsupported-host entrypoint as native runtime qualification. Phase 2 adds actual Unix server adapters and runtime coverage; a cfg-disabled Unix daemon does not qualify.

### Explicit exclusions from Phase 1

- Terminal-engine replacement.
- `portable-pty` migration, Unix process hosting, and Unix transport.
- Workspace actor, persistent hierarchy/schema migration, and new lifetime/mutation protocol.
- Split UI, command palette redesign, theme picker, and other workspace presentation changes.
- Signing, installer qualification, and distribution work.

## Current: Phase 2 — native Mac runtime implemented

### Implemented

- Shared server/session/client/probe modules run on Unix rather than compiling to unsupported-host entrypoints. `LaunchDescription` separates executable, argv, cwd, and explicit environment; native shells use the configured shell/account entry, with Mac login/interactive startup.
- Unix uses `portable-pty`, cancelable nonblocking PTY I/O, resize/wait, and explicit same-POSIX-session foreground/background cleanup. Windows uses the same host description through its existing suspended-before-job ConPTY backend.
- Current-user Unix sockets live in private platform paths, enforce peer identity, and use mode 0600. An OS-released exclusive instance lock precedes stale-socket recovery. Persistent metadata uses native atomic rename and parent-directory synchronization on Unix.
- Native Mac GPUI window, traffic lights, Menlo/system font fallbacks, Cmd shortcuts, clipboard/IME paths, and native drag/keypad adapters are enabled. Mac-only GPUI `font-kit` is required: without it, GPUI selects `NoopTextSystem` and silently paints no text.
- The probe can start a detached sibling daemon and attach through a real raw Unix terminal; Ctrl+] detaches without terminating the shell.
- Six real Unix integration scenarios cover cwd spaces, input, resize, controller conflict, detach/reconnect, incompatible protocol, natural exit, foreground/background termination, crash/stale endpoint recovery, metadata, private endpoints, and duplicate-instance rejection. CI runs them on Linux/macOS and builds the Mac app separately from headless checks.

### Verification — 2026-09-06

| Check | Result |
|---|---|
| `cargo build -p compi-client -p compi-daemon --bins` plus `cargo build -p compi-client --example compi-probe` | Native Mac client, daemon, and probe built and launched. |
| `cargo test --locked --workspace --all-targets` | 49 tests passed on macOS, including all six real Unix daemon scenarios. Windows-gated tests did not execute. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed. Cargo still reports future-compatibility notices in upstream `block` and `proc-macro-error2`. |
| Windows server all-targets cross-clippy with `-D warnings` | Passed, including the retained Windows integration suite and new immediate-descendant ownership test compilation. No native runtime claim. |
| Linux daemon `cargo check --locked -p compi-daemon --all-targets --target x86_64-unknown-linux-gnu` | Passed; not Linux test execution. |
| Dependency checker on macOS/Linux/Windows | All configured product dependency boundaries passed. |
| Independent executable using `LaunchDescription` and `PtySession` | Literal spaces/quotes/`$HOME` in argv, explicit environment, native cwd, real TTY stdin/stdout, and natural exit passed. Temporary project removed. |
| Native Mac cold launch | App auto-started sibling daemon and native zsh; no prestarted server or manually created shell required. |
| Native AppKit interaction | Debugger-invoked text-input callbacks executed a shell command, launched Vim, edited/saved a file, and returned to the shell. Native window resize to 720×420 propagated to an 83-column × 20-row PTY. Fullscreen Vim and readable terminal text were visually confirmed. No permanent automation hook was added. |
| Native close and reopen | AppKit `performClose:` exited the client with code 0; its server and shell remained running/detached. Ordinary reopen attached the original session without another live shell. Separate reconnect proof preserved shell PID 53428 and a shell variable across client exits, and `stty size` observed 27×93 after resize. |
| Native probe | Real console input printed the same shell PID/variable; `stty size` matched its 40×120 canvas; Ctrl+] exited the probe with code 0 while keeping the shell alive. |

The native smoke used an Apple M5 Pro, macOS 26.6.2, arm64 debug builds, a 960×640 logical-pixel window with 2× capture scale, and native zsh/Vim. One cold launch logged first terminal frame at 552 ms; one ordinary warm reattach logged 153 ms. Sampled resident memory was about 77 MiB for the one-tab client and 5.2 MiB for the one-shell daemon, excluding the shell and its children. These are single-run diagnostics, not release budgets, physical frame-pacing measurements, or input-to-presentation latency.

### Native Windows qualification — 2026-09-07

| Check | Result |
|---|---|
| Native prerequisites | Windows 11 x64, MSVC Rust 1.97.1, Windows SDK `fxc.exe`, and the default Ubuntu 24.04.1 WSL2 distribution were available. The dependency-boundary checker passed for every configured product boundary. |
| Release build | The native app, daemon, and probe built with the Windows SDK shader compiler in an isolated `target/qualification` directory. An existing daemon kept the normal release executable locked and was left untouched. |
| Native Windows/WSL regressions | All 56 selected core/server tests passed, including five real daemon integration scenarios and the suspended-before-job ConPTY regression that owns and terminates an immediate descendant after its launcher exits. |
| Native client interaction | A release GPUI window opened a real WSL shell. Native window messages exercised shell input, Vim, resize, selection/copy, bracketed paste, close, and reopen. Vim saved combining text, CJK, emoji, and a ZWJ sequence byte-for-byte. PTY geometry changed from 87×21 to 64×15. |
| Detach and cleanup | Native close exited the client with code 0 while preserving shell PID 164334, a shell variable, and background child 164619. Reopen recovered the same values. Isolated daemon shutdown then removed both processes. |
| Parser defect found and fixed | Vim's xterm keyboard-option commands such as `CSI > 4 ; 2 m` were incorrectly interpreted as SGR because every CSI final `m` reached the rendition parser. SGR now accepts only an empty intermediate prefix; a focused regression fails before the fix and passes after it. A rebuilt release client no longer painted Vim's blank rows as underlined. |

The visual smoke ran at 144 DPI on a 2560×1440 primary display, with a 3440×1440 100 Hz secondary display present. It verifies the actual native window and WSL execution path, but synthetic native input is not a claim about every physical keyboard layout, IME, mixed-DPI transition, or sustained frame pacing.


### Remaining Phase 2 gates and known limits

1. Run native Linux CI. Windows/WSL core, daemon, working-directory, reconnect, resize, cleanup, and ownership regressions passed natively on 2026-09-07; cross-compilation remains insufficient evidence for Linux.
2. The retained Windows suspended-before-job ConPTY backend is now runtime-qualified for immediate-descendant ownership and cleanup. A future `portable-pty` cutover still requires equivalent before-resume ownership proof; it is not required for the retained backend.
3. Exercise physical Cmd/Ctrl/Option keys, clipboard, dead keys/IME, window dragging/traffic lights, fullscreen/scaling, and display pacing. Accessibility automation was unavailable. AppKit callback injection proved the native text/resize/close path, not physical input or all native controls.
4. Unix cleanup contains the owned POSIX session, including job-control groups; descendants deliberately escaping with `setsid` are not contained. This is not a security sandbox. Ordinary foreground/background cleanup passed.
5. v7's working-directory metadata is WSL-specific. Native cwd is honored at launch but not stored as fictitious WSL metadata; generic persisted launch metadata and fresh per-client launch context remain part of the later protocol/workspace cutover.
6. Font-derived terminal geometry, versioned font configuration, installed Nerd/Powerline fallback discovery, platform symbol/emoji fallbacks, and fixed-cell glyph placement are implemented. Visual qualification of the user's exact prompt and alternate font candidates remains open because screen capture and accessibility automation were unavailable.

An explicit `--working-directory` invocation retains the existing new-work behavior. Omit it when reopening existing work. Splits, workspace migration, durable client slots, command-registry redesign, and themes remain excluded from this phase.

## Completed: Phase 3 — durable workspace ownership

### Implemented

- Protocol v8 introduces distinct server, generation, session, tab, pane, surface, process-lifetime, mutation, and attachment identities. Terminal controls and screen frames carry enough identity to reject traffic from stale attachments or restarted processes.
- A single-writer workspace actor serializes mutations and runtime observations through bounded queues. It validates server generation and expected revision, fingerprints mutation content with SHA-256, retains a bounded durable receipt history, persists before publication and acknowledgement, and schedules process effects only after durable intent.
- The versioned `workspace-v1.json` store atomically replaces durable state. Legacy v1/v2 shell records migrate once into an imported session with one tab/pane/surface per record while retaining an exact backup. Interrupted migration reuses an identical backup; malformed current metadata is quarantined with a visible recovery message; active records reopen as `Lost`.
- Stable surfaces now have a fresh process lifetime for every launch attempt and a fresh attachment identity for every controller. Restart preserves the surface ID, rejects old-lifetime attachment and terminal traffic, and resets terminal state through a fresh authoritative snapshot.
- Session/tab/pane removal persists `Ending` intent before process cleanup and removes structure only after cleanup succeeds. Cleanup failure retains the affected structure and error for explicit recovery.
- The daemon, typed client, diagnostic probe, Windows console, and existing GPUI surface integration use the new workspace protocol. Ordinary client launch initializes only a never-initialized workspace; intentionally empty or lost workspaces do not create shells implicitly. Full hierarchy presentation remains Phase 4.

### Verification — 2026-09-07

| Check | Result |
|---|---|
| `cargo test --workspace --all-targets` | 72 tests passed across 12 suites, including all 21 server library tests and six native Windows daemon scenarios. |
| `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` | Passed. Cargo still reports the upstream `proc-macro-error2` future-compatibility notice. |
| Focused native Windows daemon integration | Multi-surface terminal lifecycle, publish-before-ack mutation receipts, outcome lookup, same-surface restart with a new lifetime, stale-lifetime rejection, daemon-loss recovery, and malformed-workspace quarantine passed. |
| Isolated native probe smoke | Created a session and tab, split to two running surfaces, ended and restarted one stable surface with a new lifetime, removed the tab after process cleanup, observed zero remaining surfaces at revision 12, and shut down the isolated daemon. |

**Client integration:** Phase 4 now consumes these server contracts. Do not reintroduce shell-shaped “session” APIs or restart the persistence/PTY extraction.

## Phase 4: implemented, native Mac qualification pending

### Agreed UI direction — 2026-09-07

- **Primary:** Ghostty-like top terminal tabs. Each tab opens one terminal or its complete split layout, not an entire workspace. Creating, switching, reordering, and hiding tabs must work without opening the sidebar.
- **User-facing hierarchy:** Workspace → Terminal tabs → Panes. A named UI workspace maps to the existing server `Session`; the server's `Workspace` root remains the collection of all such groups. Do not introduce a separate session navigation layer or rename the protocol/persistence model for UI terminology.
- **Secondary:** a full Superterminal-style sidebar for browsing expandable workspaces and their tabs, organizing work, and finding/restoring hidden work. It supplements the top tab bar; it never replaces it. Every new or relaunched window starts with the sidebar closed, including tear-off windows. Remember width, not open visibility; expose a quiet chrome toggle, shortcut, and palette command, with no permanent rail, large selector, or reserved width when hidden. Reconnect, workspace switching, and tab creation/transfer never open it automatically.
- **Native windows:** dragging a terminal tab outside its window creates a new window containing that same tab and all its splits. Dropping onto another window transfers the tab there. Preserve workspace membership (server `Session`), surface identities, process lifetimes, contents, and valid focus/viewport state; never clone or respawn work.
- **Safe transfer:** keep the source attached during the drag; Escape cancels without moving anything. Accept a drop through an explicit attachment handoff, never concurrent controllers or PTY resize fights. Failed window creation/attachment keeps or restores the source view with a visible error. Moving the last tab leaves an empty source window, not a replacement shell.
- **Presentation ownership:** existing destination windows keep their own appearance overrides, zoom, and sidebar state. A new tear-off window inherits the source's resolved theme, opacity, background effect, zoom, and sidebar width but starts with its sidebar closed. Global appearance defaults live in TOML; private JSON stores only explicitly changed window fields; CLI theme overrides remain invocation-only.
- **Dark Glass and themes:** Dark Glass remains the default with neutral-dark surfaces, acid-green accent, and restrained native materials. Warm Carbon remains the coordinated alternative. Quick Appearance and Settings expose both presets plus 10–100% terminal opacity and clear/blurred native backgrounds, with readable opaque/reduced-transparency fallbacks.
- These decisions replace the earlier sidebar-first and opaque-terminal-only presentation plans. They are implemented; native Windows evidence and remaining qualification gates are recorded below.

### Implementation sequence and qualification

Steps 1–5 are implemented. The Windows portion of step 6 passed; native Mac execution remains unavailable in this environment.

1. **Workspace views and window state.** Replace the flat one-surface-per-UI-tab model with server-backed sessions, tabs, split trees, and independent surface views. Implement exclusively locked durable window slots, hidden membership, deterministic navigation fallback, state/config/CLI precedence, and lifetime-safe anchor restoration. Preserve the existing renderer, typography, input, and bounded replicas.
2. **Primary tabs and secondary sidebar.** Build the window shell with theme tokens and Dark Glass styling from the start. Deliver top terminal tabs, expandable workspace browsing, tab reorder/hide/restore, manual sidebar toggle/resize/reset, and explicit empty/disconnected/exited/failed/ending/lost/conflict states. Restore the workspace's last selected tab/pane on switching; keep the sidebar hidden until summoned. Keep window close and explicit hide non-destructive; make tab close a confirmed removal action, and expose end/restart separately. No navigation action may implicitly create a replacement shell.
3. **Nested splits and overflow.** Render all panes of the selected tab; add Split right/down, directional focus, draggable/keyboard-resizable dividers, and equalization. Enforce recursive 20-column by 4-row canvas minima using measured typography. Overflow scrolls the workspace without changing saved ratios, auto-collapsing the sidebar, or zooming panes. Coalesce PTY resizes; cancel stale drags and restore committed geometry on failure.
4. **Tab tear-off and cross-window transfer.** Implement drag previews, insertion targets, new-window creation, and drop into existing windows of the same server instance. Reuse one handoff path; preserve full split trees and process identities, reconcile source/destination hidden membership, and handle cancellation, destination failure, and attachment conflicts without stealing control. Reopen transferred work from its remembered destination window.
5. **Commands, configuration, and appearance.** Use one typed categorized command registry for the palette, menus, shortcuts, disabled states, Quick Appearance, Settings, configuration-file access, reconnect, and safe daemon restart. Persist global appearance by atomically updating the TOML `appearance` table and private per-window overrides as sparse JSON. Coordinate Dark Glass/Warm Carbon with 10–100% terminal opacity and clear/blurred native background effects while keeping explicit terminal cell backgrounds opaque.
6. **Native acceptance and handoff.** Exercise the complete workflow in real Mac and Windows/WSL windows. Keep focused regressions for slot races, navigation/anchor invalidation, split constraints, stale divider commits, transfer failure/cancellation, command focus, and theme precedence. Record attributable results in the existing roadmap and acceptance recipes.

### Phase 4 completion gate

- Create and switch user-facing workspaces and terminal tabs, build nested splits, reorder, hide/restore, and explicitly end/restart/remove work through the native client. No separate session navigation layer appears.
- Tear off a multi-pane tab, drop it into an existing window, cancel a drag, and recover from a failed transfer. Verify unchanged surface/process identities and no overlapping control or resize ownership.
- Close/crash/reopen windows with their own navigation, hidden tabs, presentation, and the same live work. Sidebar width persists but every new/relaunched window starts with the sidebar closed; reconnect and tab actions do not summon it. Empty views never reseed; failed saves and attachment conflicts stay visible.
- Shrink windows, change zoom/DPI, reveal offscreen panes, and toggle/resize the sidebar without changing the saved split tree. Flood one pane while typing in another without cross-pane input or state corruption.
- Verify command/Settings focus, global/window appearance precedence and relaunch, clear/blurred terminal transparency, safe configuration recovery, idle daemon restart, live-work restart confirmation/cancellation, and explicit lost-surface restart without silently resetting work.
- Run focused regressions and final workspace/dependency checks once integrated. Native Mac and Windows evidence is required for the new UI; existing Linux CI and physical-input/display gaps remain explicitly tracked, not claimed as passed.

Do not restart Phase 3 persistence/protocol work, replace the terminal engine, or port PTY backends in this phase. Any narrow attachment-handoff contract change must preserve the existing identity, revision, and ownership guarantees. Extended dogfooding, sustained resource budgets, and dogfood artifacts remain Phase 5; packaging/signing remains Phase 6.

### Implemented boundaries

- `gui.rs` and `gui/workspace.rs`: top terminal tabs, optional expandable workspace sidebar, all-pane rendering, native input/overlays, lifecycle actions, split overflow, window transfer, and whole-app appearance.
- `client_state.rs`: exclusively locked remembered slots, atomic state writes, quarantine/recovery, hidden membership, navigation fallback, and sequence/identity/fingerprint-validated viewport anchors.
- `layout.rs` and `commands.rs`: shared measured layout constraints and one typed command registry with platform shortcuts and disabled-action explanations.
- `config.rs` and `theme.rs`: versioned TOML with atomic appearance updates that preserve comments/unrelated keys, launch profiles/environment/limits, override provenance, Dark Glass/Warm Carbon, terminal opacity, and clear/blurred background effects.
- `window_host.rs`: authenticated, bounded same-user local forwarding so ordinary same-instance invocations create windows in one GUI process.
- Protocol v9/server integration: measured split admission, root-relative divider paths, ephemeral environment context with persistable launch profiles, authoritative Clear scrollback, retained read-only exited grids, and retirement of obsolete runtime/controllers.

### Windows verification: 2026-09-07

| Check | Result |
|---|---|
| Release workspace regressions | 97 passed, including six native Windows/WSL daemon integration scenarios and three real GUI-host named-pipe regressions. Unix-gated runtime tests did not execute. |
| Workspace Clippy with `-D warnings` | Passed. The upstream `proc-macro-error2` future-compatibility notice remains. |
| Native release app, daemon, and development examples | Built in isolated `target/qualification`; no installer or task registration needed. |
| Linux all-targets cross-check | Passed after restoring the daemon's explicit Unix `libc` dependency. Not native Linux test execution. |
| Dependency boundaries | Passed for Windows, Linux, and macOS target graphs. Not a Mac app build or runtime claim. |
| Actual GPUI window | Workspace/tab creation, rename, ordering, hide/restore, nested splits, pane focus, and sidebar controls exercised through targeted Win32 input. |
| Layout and transfer | Divider commit, minimum-sized overflow with sidebar open, offscreen focus, drag cancellation, three-pane tear-off, existing-window transfer, and failed-destination rollback verified without process replacement. |
| Process and input behavior | Native close/reopen preserved live work; input succeeded while another pane ran `yes`; explicit end removed an owned background child; restart changed the lifetime and confirmed removal collapsed the pane tree. |
| Appearance and persistence | Whole-app theme preview/cancel/accept, independent destination appearance, visible failed-save recovery, hidden-on-reopen sidebar, and selection restoration from an unchanged authoritative snapshot verified. |
| Ordinary multiwindow launch | A separate invocation exited successfully after creating a second window in the existing GUI process; no replacement shells or control takeover. |
| Isolated protocol smoke | Literal argv/environment, configured history bound, Clear scrollback, four-pane tree with outer-divider targeting, retained exited snapshots, stale controller rejection after restart, cleanup/removal, and intentionally empty workspace behavior passed. |

Native evidence used Windows 11 x64, release builds, Windows SDK `fxc.exe`, WSL2, Cascadia Mono at 14 logical pixels, and 144-DPI native windows. Win32 messages and actual GPUI `PrintWindow` captures are synthetic native interaction evidence, not physical-keyboard/IME or display-pacing qualification. No application automation hooks were added.

Runtime verification found and fixed a GPUI animation request outside a render callback, divider conflicts caused by preview resize metadata, required-name validation incorrectly rejecting automatic terminal titles, false EOF on idle Windows GUI-host pipes, late close-state saving, and the live-to-read-only attachment transition. Focused regressions and the native reproductions cover the relevant boundaries; temporary smoke programs were removed after verification.

### Windows terminal interaction refinement — 2026-09-08

- Restored the original Compi mark at the far left, gave the sidebar its own icon, removed the redundant pane path/status header, collapsed path-only tab titles to their basename, and replaced the new-terminal text glyph with a vector plus.
- Added native Windows editing behavior: `Ctrl+V` and `Ctrl+Shift+V` paste, `Shift+Insert` pastes, `Ctrl+Insert` copies, selection-aware `Ctrl+C` copies without stealing shell interrupts, and `Ctrl+Backspace` deletes the previous shell word.
- Aligned tab behavior with Windows conventions: `Ctrl+T` creates, `Ctrl+Tab` and `Ctrl+Shift+Tab` navigate, `Ctrl+W` hides, and `Ctrl+Shift+T` restores hidden tabs.
- Aligned pane behavior with Windows Terminal conventions: `Alt+Shift+Plus` splits right, `Alt+Shift+Minus` splits down, `Alt+Arrow` focuses, `Alt+Shift+Arrow` resizes, and `Ctrl+Shift+W` removes the focused pane after confirmation. The router accounts for GPUI folding Shift into printable `+` and `_` key values without binding plain `Alt+=` or `Alt+-`.
- Targeted Win32 input through the actual GPUI window verified tab create/navigation/hide/restore, split-right, split-down, directional pane focus, a ratio change from 0.50 to 0.55, and confirmed pane removal. `PrintWindow` captures verified the revised chrome and focus movement. This is native event-path evidence, not physical-keyboard or IME qualification.
- `cargo fmt --all -- --check`, warning-denied workspace Clippy, and all 99 workspace tests passed. The upstream `proc-macro-error2` future-compatibility notice remains.

### Windows settings, daemon, and TUI verification — 2026-09-10–11

- The release GPUI client exposed Quick Appearance and the full Settings surface through the typed command path. Native captures verified both theme cards, global/window scope, clear/blurred backgrounds, the continuous opacity slider, interface and terminal summaries, all 48 searchable commands, configuration-file access, and daemon controls without clipped window actions. The Windows shortcut family now uses Ctrl-Plus/Minus and Ctrl-0 for zoom, while Ctrl-Comma opens Settings.
- Global changes persisted and reloaded as a compact `terminal_opacity = 0.7` TOML value. A Warm Carbon **This window** change left global TOML unchanged and wrote only `"theme": "warm-carbon"` to sparse private JSON; **Use global defaults** removed the appearance object.
- An idle daemon restart completed without destructive confirmation. With one live surface, Settings listed the surface ID and required **End all work and restart daemon**; Escape preserved the same responsive shell, while confirmation converted the old surface to explicit lost state and **Restart surface** created a fresh responsive shell.
- At 144 DPI, the fixed logical viewport filled the complete area below the header with no false workspace overflow bars and kept all custom Windows controls reachable. Vim occupied the full alternate screen, accepted input, restored the main shell screen on exit, and showed no history scrollbar. Composed captures verified both unblurred clear and softened blurred terminal backgrounds at 70% while terminal foreground text stayed opaque.
- Focused `compi-client` regressions passed 49 tests; the final isolated all-target workspace run passed 110 tests across 10 suites. Formatting and warning-denied workspace Clippy passed; Cargo still reports the upstream `proc-macro-error2` future-compatibility notice. Synthetic Win32 input and composed desktop captures verify the native event/render path, not physical-keyboard, IME, or sustained display-pacing qualification.

### Core CI reliability — 2026-09-11

- Reproduced the Unix natural-exit panic in WSL2 Ubuntu: the old test expected reattachment to fail, contradicting the retained read-only grid contract and native client. The regression now verifies final output, exit code, unchanged process lifetime, read-only resize/reconnect, and rejected process input. CI's other Unix timeout came from checking markers only on new events, even when the retained replica already contained the requested text; waits now inspect the current replica first.
- Both six-scenario daemon integration suites serialize daemon lifetimes with in-process mutexes and use bounded, condition-driven polling. Windows markers cannot match echoed commands; built-in alternate-screen controls replace `top`/`tput`. Detached output uses an explicit release gate, and backpressure exercises over 16 MiB of in-place repaint output without draining the attached client before checking completion and recovery.
- Polling also exposed a real Windows teardown race: `DisconnectNamedPipe` discarded unread protocol-error replies. Synchronous final responses now flush before disconnect, retaining the existing bounded writer wait and cancellation path.
- Continued repetition after the first 25-pass batch exposed a late-exit race on the second batch's run 21: snapshot gap recovery targeted an already released controller. Exit collection now obtains the final grid through a fresh read-only connection, with bounded polling; final lifecycle assertions wait for both durable exit publication and detachment. Investigation also reproduced a transport bug where blocking client reads bypassed frames buffered by polling; both read modes now consume the same buffer. A real Unix socket regression failed before the fix and passes afterward.
- The Windows dependency checker failure was UTF-8 Cargo metadata decoded as CP1252, not a forbidden dependency. Metadata decoding is explicit; resolution/launch errors report Cargo diagnostics and exit 2, distinct from boundary violations (exit 1). All three production boundary rules remain unchanged.
- Full Linux execution exposed a previously masked v7 fixture dependency on JSON object insertion order. The protocol's test-only `serde_json/preserve_order` feature now makes that requirement explicit; neither fixture bytes nor production wire codecs changed.
- Final local verification, restarted after all race fixes: the exact locked three-crate test command passed 25 consecutive times on Windows 11/WSL2, 110 tests per run with zero failures or ignored tests (221.72 seconds total, a single diagnostic batch, not a performance budget). Native Linux execution inside WSL2 passed 90 tests, including all six Unix daemon scenarios, the buffered-read regression, v7 byte fixtures, and terminal compatibility. Workspace formatting and warning-denied Clippy passed; Windows still reports the upstream `proc-macro-error2` future-compatibility notice.
- Boundary checks passed for Windows, Linux, and macOS targets. A deliberately unavailable Rust toolchain produced readable exit 2 without a traceback; a temporary real Cargo graph verified that dev-only graphics remain allowed and a production PTY violation still exits 1.
- Core CI now checks formatting, lints the whole workspace, and provisions a required default Ubuntu WSL2 distribution before Windows product tests. Hosted results are tracked by the README's main-branch badge; local results alone are not hosted-CI evidence. No new graphical client qualification, performance budget, installer/signing result, or physical-input/display claim is made here. Linux's graphical client remains unqualified.

### Remaining qualification

1. Complete a focused UI/UX refinement pass without changing the workspace model, persistence/protocol contracts, or established navigation.
2. Build and exercise the full workspace client on a native Mac. No Mac SDK/runtime or configured SSH host was available here.
3. Keep native Linux core CI passing; WSL2 Linux runtime suites now pass locally, but neither cross-compilation nor headless tests qualify the Linux graphical client.
4. Retain physical keys, dead keys/IME, exact user font/glyph selection, mixed displays/DPI, native controls, and sustained pacing/resource qualification as explicit gates.
5. Continue Phase 5 daily-use/soak measurements and dogfood artifacts. Packaging/signing remains Phase 6.

## Current implementation: native refinement status

**Status:** typography and glyph integration are implemented, and the current Windows terminal interaction pass is complete. Focused UI/UX refinement is next, followed by performance/resource measurement, native Mac workspace qualification, and the remaining physical-input/display matrix. The terminal derives cell advance, line height, and baseline from the resolved primary font; uses one geometry source for painting, PTY sizing, cursor, selection, hit-testing, images, and IME; invalidates shaped rows on display-scale changes; and rebases shaped fallback glyphs to logical terminal cells. Versioned native configuration and CLI overrides are wired through startup. Ghostty-like terminal tabs and the secondary Superterminal-style sidebar are implemented.

### Completed — 2026-09-07

- Added the versioned native font configuration contract, explicit path/environment selection, invocation-local CLI overrides, field-wise recovery, and visible diagnostics.
- Added installed-font resolution with fixed-ASCII validation, font-derived device-snapped cell metrics, user fallbacks, an installed Nerd/Powerline fallback, and platform Unicode/symbol/emoji fallbacks.
- Replaced fixed Menlo-era geometry throughout PTY sizing, terminal painting, cursor, selection, hit-testing, scrolling, Kitty images, and IME caret placement.
- Rebased each shaped grapheme onto its protocol-defined logical cell while preserving intra-grapheme offsets, wide-cell spans, fallback font IDs, and color-emoji painting.
- Kept the bounded row-shaping cache and invalidated it when display scale changes. Native UI chrome continues to use the system font.
- Verified the Mac build, all 54 workspace tests, warning-free workspace Clippy, diff hygiene, first terminal frame, and an AppKit event-loop smoke. Pixel-level comparison and physical-input qualification remain blocked by unavailable screen-capture/accessibility permissions.

**Next:** complete the focused UI/UX refinement pass on the existing workspace model; then measure interaction latency, frame pacing, and resource behavior under sustained terminal output; qualify the complete workspace client on Mac; and continue Phase 5 dogfooding while retaining physical-key, IME, font, and mixed-display checks as explicit gates.

### 1. Establish the typography and glyph baseline — implementation complete, visual selection pending

- Inspect the current font selection, fixed cell metrics, row shaping/cache, cursor/selection geometry, and native input/repaint path in `crates/compi-client/src/gui.rs` and relevant client helpers.
- Identify the actual missing prompt/path codepoints and available fonts. Distinguish ordinary Unicode/emoji from Powerline/Nerd Font private-use symbols; do not change the user's shell prompt to conceal missing glyphs.
- Capture a compact comparison workload: the real prompt/path, ASCII, bold/italic text, combining accents, CJK, emoji sequences, and private-use icons. Include a fullscreen editor.
- Present two or three restrained font/spacing candidates before treating a new default as settled. The direction is calmer Mac typography, not a new visual theme.

### 2. Replace rigid typography with font-derived geometry — complete

- Keep terminal font family/size and line spacing in the existing TOML contract and summarize them in Settings; do not invent a parallel settings format.
- Derive cell advance, line height, and baseline from the chosen primary font instead of fixed Menlo-era constants. Keep fallback glyphs constrained to the terminal's logical cell widths.
- Migrate painting, PTY sizing, cursor, selection, hit-testing, IME caret geometry, and cache invalidation together when font/scale changes.
- Keep native system typography in chrome; adjust canvas padding and default spacing without changing the workspace model.
- Acceptance: readable regular/bold text with consistent baselines; selection and cursor remain aligned after font changes, zoom, and window resize.

### 3. Restore prompt symbols and emoji correctly — complete pending visual qualification

- Build an explicit fallback chain for text, Nerd/Powerline symbols, and color emoji. Prefer available fonts; if a bundled fallback is required, verify its license and coverage rather than assuming a font name exists.
- Preserve grapheme clusters, combining marks, emoji variation selectors/ZWJ sequences, and narrow/wide cell behavior. Fix the actual failing layer; do not replace unknown symbols with substitutes.
- Acceptance: the user's real shell path/prompt renders without missing-glyph boxes for supported symbols, and mixed text/icons/emoji do not overlap, clip, or shift the cursor. Copy/selection returns the original text.

### 4. Focused UI/UX refinement: in progress

- Preserve the established information architecture: top terminal tabs remain primary, the workspace sidebar remains optional and closed on launch, and terminal content keeps visual priority. The implemented Settings surface must remain an overlay rather than a navigation layer or status bar; no protocol work belongs in this pass.
- Capture the current native surface with populated terminals across normal, narrow, maximized, split, sidebar, palette, theme-picker, empty, loading, disconnected, exited, failed, lost, confirmation, and error-recovery states. Exercise Windows at representative display scales; repeat the complete surface on Mac during native qualification.
- Tighten top-chrome density, alignment, hit targets, and visual hierarchy. Clearly distinguish the Compi mark from the sidebar action; refine active/inactive tab anatomy, close affordances, drag/overflow feedback, pane focus, dividers, and workspace scrollbars.
- Ensure New terminal, pane actions, and custom Windows controls remain visible, reachable, and non-overlapping at supported widths and display scales. Preserve native drag, maximize, close, and platform-shortcut behavior.
- Standardize default, hover, focused, active, disabled, pending, and destructive states across chrome buttons, tabs, sidebar rows, command choices, overlays, and recovery actions. Use concise labels and explanations; do not rely on acid lime or color alone.
- Acceptance: the real Windows surface is visually coherent and keyboard-usable across the state/size/scale matrix, terminal readability and input remain intact, and the resulting interface is the baseline used for performance measurement and Mac qualification.

### 5. Separate latency from interaction roughness: native control pass complete, measurements pending

- Measure key receipt → PTY → replica → presentation, plus scroll/resize/frame pacing under idle and sustained-output workloads. Record build/display context and compare the same workload before/after; do not label startup timings as input latency.
- Fix demonstrated stalls, unnecessary shaping/allocation, repaint scheduling, or queue behavior only where evidence points. Keep caches and transport queues bounded; no terminal-engine rewrite.
- The Windows native control pass now covers paste/copy/word deletion, tab create/navigation/hide/restore, pane split/focus/resize/removal, title semantics, sidebar identity, and new-terminal affordance. Preserve these contracts while qualifying Mac behavior.
- Remaining acceptance: responsive typing during sustained output, smooth scrolling/resizing without duplicate input or flicker, and attributable latency/frame/resource measurements.

### 6. Qualify the complete workspace client on Mac

- Re-run native shell, fullscreen editor, input, resize, close/reopen, and same-process persistence scenarios on the actual window.
- Check physical Cmd/Ctrl/Option keys, clipboard, dead keys/IME, font fallback, and scaling where access permits. AppKit debugger callbacks are useful supplementary evidence, not a replacement for the physical-input matrix.
- Run focused regressions plus final workspace checks once integration is complete; keep permanent tests only for plausible failures such as grapheme/cell alignment, stale font caches, or input duplication.
- Preserve Windows/WSL behavior and headless dependency isolation. Report unavailable native platform checks explicitly.
- Finish with a runnable Mac build, before/after visual and interaction evidence, and updated qualification notes. These checks remain required for shared-baseline qualification even though the headless Phase 3 workspace migration proceeds first.

### Compaction handoff

- The implemented Phase 2 baseline and all previous evidence/gaps are recorded above. Do not restart the extraction or PTY port.
- Typography and glyph integration are implemented. Do not restart that work without a demonstrated rendering failure; retain the user's font selection and Mac physical-input matrix as qualification tasks.
- Keep Mac GPUI `font-kit` enabled. Its absence selects a no-op text system; the prior missing-all-text failure was fixed, not an outstanding GPU renderer problem.
- Native smoke processes and temporary automation/evidence files were removed. Qualification binaries remain under ignored build output.
- Current Windows checks: `cargo fmt --all -- --check`, `cargo test --locked --workspace --all-targets` (110 tests across 10 suites), `cargo clippy --locked --workspace --all-targets -- -D warnings`, dependency boundaries, isolated native builds, and targeted GPUI interaction passed. Cargo still reports an upstream future-compatibility notice for `proc-macro-error2`. The native event-path evidence and remaining physical-input/display gaps are recorded above. The earlier Mac build and interaction evidence remains valid; its pixel-level comparison and physical-input matrix are still open.

## Next phases

Continue in the [specification's migration and build order](Spec.md#migration-and-build-order):

- **Phase 3 — completed:** workspace ownership, durable mutations, process-lifetime identity, and metadata migration.
- **Phase 4: implementation complete, Mac qualification pending:** primary terminal tabs, secondary hidden workspace sidebar, nested splits/overflow, remembered windows, tab tear-off/transfer, palette, configuration, and whole-app themes.
- **Phase 5:** qualify shared daily use and produce Mac/Windows dogfood artifacts with attributable resource measurements.
- **Phase 6:** qualify distribution and extend process discovery/headless use.

Do not combine engine replacement, PTY migration, workspace migration, and UI redesign into one rewrite. Windows signing and installer qualification do not block the cross-platform foundation.

## Retained evidence

- [Windows terminal test recipes](testcmds.md): procedures for the existing implementation, to adapt using the acceptance map. Old shell-session and close-after-end UI expectations are not the new workspace contract.
- [Historical Windows acceptance results](ACCEPTANCE_RESULTS_2026-09-02.md): dated observations, not current cross-platform qualification.

The superseded specification, status report, release targets, and detailed old roadmap remain available in Git history.
