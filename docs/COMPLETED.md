# Compi completed work

This file records completed implementation and dated verification evidence. Unfinished work is tracked only in [Next steps](NEXT_STEPS.md).

## Latest completed handoff

- The Windows UI, terminal resize, resource observability, and current-FPS work described below is complete in the current source checkpoint.
- Final Windows verification passed formatting, warning-denied workspace/all-target Clippy, and 128 tests across 10 suites. Native 144-DPI captures verified responsive flat modals, stable one-prompt resizing, and a 144 FPS idle / 142 FPS sustained-output overlay.

### Windows UI, resize, observability, and current FPS — 2026-09-13

- Repeated height changes no longer promote unused viewport rows into scrollback or duplicate prompts. Reflow trims trailing blank rows after the last meaningful row or cursor while preserving meaningful blank lines, cursor position, and alternate-screen behavior; focused terminal compatibility regressions cover the boundary.
- The native Windows surface now uses denser type, compact filled controls, clearer active states, and flat responsive overlays. Settings, Quick Appearance, theme catalog, command lists, and form dialogs remain centered and bounded in both 1462×974 and 1050×750 logical layouts at 144 DPI; Settings switches from its rail to wrapped section navigation when narrow.
- Workspace UI responsibilities are split into dedicated catalog, dialogs, media, performance, and settings modules without changing the persisted workspace or navigation contracts.
- Protocol 11 adds an on-demand runtime-metrics request. The daemon reports live/exited surface counts and optional cross-platform process counters; the client Performance section combines those with client resources, render timing, cache use, and bounded history. Sampling occurs only while Performance is visible or the overlay is enabled.
- The persisted per-window `Show FPS` option enables a Steam-style numeric overlay. Continuous next-frame scheduling is active only while that overlay or Performance is visible, so disabled windows remain event-driven. Native smoke measured 144 FPS while idle and 142 FPS during a 500-line output burst.
- README, specification, and manual qualification recipes now distinguish unfinished work from this completed implementation and document the new observability and UI contracts.

### Completed blockers — 2026-09-12

The [main-branch Core CI run at d7552028](https://github.com/cloudboy-jh/compi/actions/runs/34668270744) passed Ubuntu, macOS, and Windows core jobs plus the native Mac client build. This supersedes the earlier 20-failure status; the detailed fixes and local evidence are retained under **Core CI reliability**.

1. **MIT license — complete.** LICENSE, workspace `license = "MIT"`, and the README License section are present.
2. **Daemon integration reliability — complete.** Both suites serialize daemon lifetimes and use bounded state polling; built-in terminal controls replaced `top`/`tput` dependencies.
3. **Unix runtime failures — complete.** Retained-grid exit semantics, buffered reads, soft-wrapped markers, and fixture ordering are corrected; both hosted Unix jobs passed.
4. **Dependency boundary diagnostics — complete.** Explicit UTF-8 metadata decoding and readable resolution/launch errors distinguish exit 2 from real boundary violations (exit 1).
5. **Lint gap — complete.** Core CI checks workspace formatting and warning-denied workspace/all-target Clippy.
6. **Gate — complete.** The final tracing-enabled Windows batch passed 25 consecutive runs after all corrections; the subsequent hosted run passed every core job.

### Distribution — Phase 6, reduced scope

- `.github/workflows/release.yml` replaces the signing-required Windows-only workflow. Windows and ARM64 macOS package jobs must both pass before one tag-triggered draft release is assembled. Manual dispatch builds downloadable Actions artifacts without creating a tag or release.
- Windows signing secrets are optional as a pair; partial configuration fails explicitly. `-RequireSigning` remains available for deliberately signed builds. All three installer Cargo builds are locked; the isolated bootstrapper lockfile and obsolete theme imports were corrected after the real package build exposed them.
- `tools/build-macos.sh` builds a macOS 14+ ARM64 `Compi.app` with sibling client/daemon executables, native icon, license, ad-hoc signature, app ZIP, DMG, and checksums. Ad-hoc signing is not Developer ID signing or notarization.
- Local Windows qualification built and WiX-validated the unsigned installer and portable ZIP, verified checksums/license contents, visually exercised the real installer Ready surface without installing into the live account, and launched the extracted portable client twice with unchanged daemon/surface/process identities. Installer library tests passed 4/4; the known per-user MSI ICE91 warnings and upstream `proc-macro-error2` notice remain visible.
- [Release run 34711439577](https://github.com/cloudboy-jh/compi/actions/runs/34711439577), source commit `21a3af9`, passed the ARM64 Mac build and copied-DMG-bundle LaunchServices/reconnect smoke. Downloaded Mac artifacts matched their checksums and retained executable permissions and the license. The hosted Windows installer lifecycle result was not observed to completion before the local watcher was stopped at the user's direction; consult that run rather than treating this note as current CI status.
- README documents draft-release visibility, unsigned Windows warnings, and macOS Gatekeeper approval. No release tag or public release was created during this session.

### Completed: image input, themes, and graphics

- The native header uses the selected rounded iris with the desktop icon's pixel-derived hooked swoop. Its accent follows theme selection and preview; the desktop artwork is unchanged.
- The bundled catalog contains 40 whole-app themes (25 dark and 15 light): the original Compi, Catppuccin, Tokyo Night, Solarized, Nord, and Dracula set; OpenCode-derived Aura, Ayu, Everforest, Flexoki, GitHub, Gruvbox, Kanagawa, Material, Matrix, One Dark, Vercel, and Cursor variants; and OMP-derived Titanium, Obsidian, Poimandres, Mahogany, Alabaster, Honeycomb, Quartz, and Eucalyptus palettes. Build-time validated data generates the constant palette API. Native browsing includes search, dark/light/favorite filters, miniature terminal/header previews, and bundled license text. The expansion passed macOS smoke coverage for search, filters, preview/cancel, global application through an isolated configuration, favorites, attribution scrolling, narrow/full-height list scrolling, and representative light/dark ANSI output. The original Windows slice exercised favorite persistence, global propagation to existing windows, and preservation of explicit window overrides; Windows qualification of the expansion remains open. Theme changes preserve terminal/header opacity.
- PNG and native Windows DIB bitmap clipboard images, plus actual OLE file drops, produced readable WSL paths and compact previews without automatic execution. The native inspector rendered the full 3840×2160 image, zoomed/panned, copied matching pixels, and saved the original bytes through the native Save dialog. An apostrophe/space-containing filename was quoted correctly. Managed image files survived window closure; thumbnail dismissal does not delete them.
- Source image storage/reservations default to 64 MiB per surface; decoded caches admit 64 MiB per pane. Offscreen decoded assets and sprite-atlas entries are released without deleting referenced authoritative pixels. Transfers, decoder queues, frame queues, and cache admission remain bounded; rejection preserves existing images and produces Kitty errors. This is not a daemon-wide memory cap.
- The first real Windows graphics test exposed stock ConPTY dropping every Kitty APC sequence: the bounded raw PTY trace contained 439 bytes and the completion marker, but zero image sequences. The approved scope expansion bundles official Microsoft ConPTY/OpenConsole 1.24.260710001 with SHA-256-pinned preparation, Microsoft signature verification, and license text. Source builds, Windows CI, MSI and portable packaging use the matching pair without an unsupported system fallback.
- The real 4K RGBA fixture transmits 31.64 MiB of pixels / 42.19 MiB of base64, then compares image bytes and placement after a new attachment. It passes on Windows with the pinned runtime and on native Linux under WSL. Large-frame polling was corrected to drain up to 1 MiB per call, avoiding artificial 32 KiB-per-poll throttling and the resulting Linux timeout.
- Resize preserves image bytes and grid coordinates/extents while text reflows independently. Main-screen scrollback anchors now continue moving below row -1; expiry/clear releases expired placements.
- Wire protocol is now 10; GUI window-forwarding protocol is 2. Older clients/daemons must be closed/restarted deliberately after accounting for running work. The old v7 golden fixtures remain unchanged.
- Final verification: 123 Windows tests and 99 native Linux tests passed, including real 4K image reconnect and unchanged v7 fixtures. Workspace formatting, warning-denied Clippy and production dependency boundaries passed. The rebuilt Windows MSI validated and the extracted portable package passed native launch/sibling-daemon/reconnect smoke with the pinned runtime.


## Completed: Phase 0 — implementation contracts

- [x] Define process-lifetime identity and replica/history/selection invalidation when a surface restarts without a server restart. See [Process lifetimes and terminal identity](Spec.md#process-lifetimes-and-terminal-identity): stable surface, fresh lifetime per launch attempt, keyed messages/caches, fresh attachment/snapshot after restart, and explicit anchor invalidation.
- [x] Define durable workspace mutation acknowledgements, revision publication, and recovery after process-launch or persistence failure. See [Durable workspace mutations](Spec.md#durable-workspace-mutations): persist before publish/acknowledge, durable intent before side effects, bounded mutation receipts, retained failed panes, and truthful read-only recovery after uncertain persistence.
- [x] Define durable client identity, hidden-tab membership, remembered navigation, and behavior across relaunch and simultaneous windows. See [Durable client identity and navigation](Spec.md#durable-client-identity-and-navigation): exclusively owned remembered window slots, client-local hidden sets, deterministic navigation fallback, and no implicit shell creation or control takeover.
- [x] Define behavior when an existing split tree no longer fits the window. See [Constrained split layout](Spec.md#constrained-split-layout): 20-column by 4-row minimum canvases, recursive minima, scrollable workspace overflow, and reachable focus without automatic sidebar collapse, pane zoom, or saved-tree changes.
- [x] Map the specification's acceptance requirements to the implementation sequence, retaining useful Windows terminal regressions and identifying missing Unix/platform coverage. See [Acceptance-to-phase map](Spec.md#acceptance-to-phase-map) and [Phase 0 decision scenarios](Spec.md#phase-0-decision-scenarios).

The [first extraction change](Spec.md#first-extraction-change) now has explicit scope, exclusions, and observable acceptance criteria. These completed checkboxes mean the contracts are defined, not that their behavior is implemented or runtime-qualified.

## Phase 1 — neutral foundation implemented

The code changes in [Phase 1](Spec.md#1-extract-the-neutral-foundation) are implemented.

### Implemented extraction

- Captured pre-extraction v7 fixtures for 12 client controls, 18 server controls, an initial screen snapshot, and nine output/resize steps. New codecs and engine output preserve those captured values and bytes.
- Established the three product crates: `compi-protocol`, `compi-daemon`, and `compi-client`. Protocol values and local transport are shared; terminal state and OS hosting belong to the daemon; replicas, probe, interaction logic, and GPUI belong to the client.
- Extracted pure input/mouse/paste/focus encoding, selection, viewport lookup, hyperlink policy, and shared theme constants into the client without compatibility shims.
- Kept engine-to-wire conversion at the daemon boundary without additional grid copies. The daemon's production dependencies contain no GPUI, client application, or installer package.
- Moved installer UI into its existing isolated `installer/bootstrapper` package. Preserved its locked registry versions while updating local package dependencies.
- Added [three-host neutral CI](../.github/workflows/core-ci.yml), including v7 fixtures, replica recovery, resolved dependency-boundary checks, and a native Windows daemon build without graphics setup. The unreliable standalone hosted Windows product/installer workflow was removed; native Windows product verification remains in [the acceptance recipes](testcmds.md). The former signing-required tag workflow is now superseded by the cross-platform [release workflow](../.github/workflows/release.yml).

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

## Phase 2 — native Mac runtime implemented

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

## Phase 4 — workspace client implemented

### Agreed UI direction — 2026-09-07

- **Primary:** Ghostty-like top terminal tabs. Each tab opens one terminal or its complete split layout, not an entire workspace. Creating, switching, reordering, and hiding tabs must work without opening the sidebar.
- **User-facing hierarchy:** Workspace → Terminal tabs → Panes. A named UI workspace maps to the existing server `Session`; the server's `Workspace` root remains the collection of all such groups. Do not introduce a separate session navigation layer or rename the protocol/persistence model for UI terminology.
- **Secondary:** a full Superterminal-style sidebar for browsing expandable workspaces and their tabs, organizing work, and finding/restoring hidden work. It supplements the top tab bar; it never replaces it. Every new or relaunched window starts with the sidebar closed, including tear-off windows. Remember width, not open visibility; expose a quiet chrome toggle, shortcut, and palette command, with no permanent rail, large selector, or reserved width when hidden. Reconnect, workspace switching, and tab creation/transfer never open it automatically.
- **Native windows:** dragging a terminal tab outside its window creates a new window containing that same tab and all its splits. Dropping onto another window transfers the tab there. Preserve workspace membership (server `Session`), surface identities, process lifetimes, contents, and valid focus/viewport state; never clone or respawn work.
- **Safe transfer:** keep the source attached during the drag; Escape cancels without moving anything. Accept a drop through an explicit attachment handoff, never concurrent controllers or PTY resize fights. Failed window creation/attachment keeps or restores the source view with a visible error. Moving the last tab leaves an empty source window, not a replacement shell.
- **Presentation ownership:** existing destination windows keep their own appearance overrides, zoom, and sidebar state. A new tear-off window inherits the source's resolved theme, opacity, background effect, zoom, and sidebar width but starts with its sidebar closed. Global appearance defaults live in TOML; private JSON stores only explicitly changed window fields; CLI theme overrides remain invocation-only.
- **Dark Glass and themes:** Dark Glass remains the default with neutral-dark surfaces, acid-green accent, and restrained native materials. Warm Carbon remains the coordinated alternative. Quick Appearance and Settings expose both presets plus 10–100% terminal opacity and clear/blurred native backgrounds, with readable opaque/reduced-transparency fallbacks.
- These decisions replace the earlier sidebar-first and opaque-terminal-only presentation plans.


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

### Unified terminal and header opacity — 2026-09-12

- The existing **Terminal opacity** slider controls terminal and window-header/tab backgrounds together. No separate header setting, override, or slider was introduced. Text, icons, window controls, and explicit terminal cell backgrounds remain opaque.
- Removed percentage quantization from pointer mapping and avoid reapplying native material on every drag pixel. The appearance overlay leaves the header unobscured for live preview; only releasing the slider persists the selected scope.
- Native Windows verification exercised the single slider on the release client: both backgrounds faded during the drag, the isolated TOML stayed unchanged during preview, and release saved the fractional `terminal_opacity = 0.400822` value. Composed desktop captures confirmed transparent backgrounds and opaque text/controls. The updated slider regression passed; [Core CI run 34711433745](https://github.com/cloudboy-jh/compi/actions/runs/34711433745) then passed all three native core jobs and the Mac client build at final source commit `21a3af9`.
- The behavior and two focused specification updates landed in `6fe9438`; unrelated pre-existing specification edits remain outside that commit. This does not qualify physical keys, IME, or sustained frame pacing.

### Core CI reliability — 2026-09-11

- Reproduced the Unix natural-exit panic in WSL2 Ubuntu: the old test expected reattachment to fail, contradicting the retained read-only grid contract and native client. The regression now verifies final output, exit code, unchanged process lifetime, read-only resize/reconnect, and rejected process input. CI's other Unix timeout came from checking markers only on new events, even when the retained replica already contained the requested text; waits now inspect the current replica first.
- Both six-scenario daemon integration suites serialize daemon lifetimes with in-process mutexes and use bounded, condition-driven polling. Windows markers cannot match echoed commands; built-in alternate-screen controls replace `top`/`tput`. Detached output uses an explicit release gate, and backpressure exercises over 16 MiB of in-place repaint output without draining the attached client before checking completion and recovery.
- Polling also exposed a real Windows teardown race: `DisconnectNamedPipe` discarded unread protocol-error replies. Synchronous final responses now flush before disconnect, retaining the existing bounded writer wait and cancellation path.
- Continued repetition after the first 25-pass batch exposed a late-exit race on the second batch's run 21: snapshot gap recovery targeted an already released controller. Exit collection now obtains the final grid through a fresh read-only connection, with bounded polling; final lifecycle assertions wait for both durable exit publication and detachment. Investigation also reproduced a transport bug where blocking client reads bypassed frames buffered by polling; both read modes now consume the same buffer. A real Unix socket regression failed before the fix and passes afterward.
- The Windows dependency checker failure was UTF-8 Cargo metadata decoded as CP1252, not a forbidden dependency. Metadata decoding is explicit; resolution/launch errors report Cargo diagnostics and exit 2, distinct from boundary violations (exit 1). All three production boundary rules remain unchanged.
- Full Linux execution exposed a previously masked v7 fixture dependency on JSON object insertion order. The protocol's test-only `serde_json/preserve_order` feature now makes that requirement explicit; neither fixture bytes nor production wire codecs changed.
- The first hosted run after these fixes passed Ubuntu core and the native Mac client build, but exposed additional platform cases: a macOS prompt soft-wrapped the resize marker, and Windows reported a five-second surface-creation timeout and early PTY exits. Both screen collectors now preserve soft-wrapped logical lines, with a deliberately wrapped Unix marker regression; the Windows-only pointer test import is gated accordingly. Surface creation uses a 30-second deadline with the last observed state in timeout errors, and failed CI runs retain bounded terminal traces for diagnosing native shell failures. A passing local batch alone is not evidence that those hosted failures are resolved.
- The next hosted run passed both Unix core jobs and all Windows daemon scenarios except the handle budget. Enabling CI traces exposed one retained trace-file handle per completed surface. The same failure reproduced locally (126 baseline handles, 138 after 12 cycles); recorders now close after PTY output drains while final grids and trace files remain available. The original handle allowance is unchanged.
- Final local verification was restarted after all corrections with terminal tracing enabled to match CI: the exact locked three-crate test command passed 25 consecutive times on Windows 11/WSL2, 110 tests per run with zero failures or ignored tests. Native Linux execution inside WSL2 passed 90 tests, including all six Unix daemon scenarios, the buffered-read regression, v7 byte fixtures, and terminal compatibility. Workspace formatting and warning-denied Clippy passed; Windows still reports the upstream `proc-macro-error2` future-compatibility notice.
- Boundary checks passed for Windows, Linux, and macOS targets. A deliberately unavailable Rust toolchain produced readable exit 2 without a traceback; a temporary real Cargo graph verified that dev-only graphics remain allowed and a production PTY violation still exits 1.
- Core CI now checks formatting, lints the whole workspace, and provisions a required default Ubuntu WSL2 distribution before Windows product tests. Hosted results are tracked by the README's main-branch badge; local results alone are not hosted-CI evidence. No new graphical client qualification, performance budget, installer/signing result, or physical-input/display claim is made here. Linux's graphical client remains unqualified.
- Hosted gate closed on 2026-09-12: [run 34668270744](https://github.com/cloudboy-jh/compi/actions/runs/34668270744), commit `d7552028`, passed all three core jobs and `mac-client`. This is native hosted build/test evidence, not bundled-app or physical-input qualification.
- The distribution/unified-opacity source at `21a3af9` also passed every job in [Core CI run 34711433745](https://github.com/cloudboy-jh/compi/actions/runs/34711433745). The ARM64 Mac release job additionally passed native copied-DMG-bundle launch and same-process reconnect; this is artifact lifecycle evidence, not the full workspace/physical-input matrix.

## Typography and glyph integration — completed 2026-09-07

- Added the versioned native font configuration contract, explicit path/environment selection, invocation-local CLI overrides, field-wise recovery, and visible diagnostics.
- Added installed-font resolution with fixed-ASCII validation, font-derived device-snapped cell metrics, user fallbacks, an installed Nerd/Powerline fallback, and platform Unicode/symbol/emoji fallbacks.
- Replaced fixed Menlo-era geometry throughout PTY sizing, terminal painting, cursor, selection, hit-testing, scrolling, Kitty images, and IME caret placement.
- Rebased each shaped grapheme onto its protocol-defined logical cell while preserving intra-grapheme offsets, wide-cell spans, fallback font IDs, and color-emoji painting.
- Kept the bounded row-shaping cache and invalidated it when display scale changes. Native UI chrome continues to use the system font.
- Verified the Mac build, all 54 workspace tests, warning-free workspace Clippy, diff hygiene, first terminal frame, and an AppKit event-loop smoke. Pixel-level comparison and physical-input qualification remain blocked by unavailable screen-capture/accessibility permissions.

## Historical references

- [Windows terminal test recipes](testcmds.md): procedures for the existing implementation, to adapt using the acceptance map. Old shell-session and close-after-end UI expectations are not the new workspace contract.
- [Historical Windows acceptance results](ACCEPTANCE_RESULTS_2026-09-02.md): dated observations, not current cross-platform qualification.

The superseded specification, status report, release targets, and detailed old roadmap remain available in Git history.
