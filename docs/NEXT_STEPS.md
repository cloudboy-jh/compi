# Compi next steps

Phase 0 contracts and Phase 1 package extraction are implemented. Phase 2 now has a runnable native Mac application and shared Unix server. Mac window, native text input, fullscreen editor, resize, native close, and ordinary reopen have been exercised against real PTYs. Full Phase 2 qualification remains open: native Linux/Windows execution, Windows PTY-backend acceptance, and physical Mac input/display checks are not claimed.

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
- Established `compi-protocol`, `compi-terminal`, and `compi-client-core`; separated screen DTOs/codecs, `TerminalState`, and `ScreenMirror` with direct consumer migration and no old-module shims.
- Extracted pure input/mouse/paste/focus encoding, selection, viewport lookup, hyperlink policy, and shared theme constants out of GPUI.
- Established `compi-server` and `compi-app`; moved engine-to-wire conversion to the server boundary without additional grid copies. Server production/build dependencies contain no GPUI, window renderer, or installer package.
- Moved installer UI into its existing isolated `installer/bootstrapper` package. Preserved its locked registry versions while updating local package dependencies.
- Added [three-host neutral CI](../.github/workflows/core-ci.yml), including v7 fixtures, replica recovery, resolved dependency-boundary checks, and a native Windows daemon build without graphics setup. [Windows CI](../.github/workflows/windows-ci.yml) retains product build/installer checks and explicitly reports unavailable WSL runtime coverage.

### Verification from this pass

| Check | Result |
|---|---|
| `cargo test --workspace --all-targets` on macOS | 36 tests passed. Windows-gated runtime/UI tests did not execute. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` on macOS | Passed for active host code. |
| `cargo check --locked -p compi-server --all-targets --target x86_64-pc-windows-msvc` | Passed, including Windows server/client transport/probe/integration-test compilation. No native linking or runtime claim. |
| Neutral crates `cargo check --all-targets --target x86_64-unknown-linux-gnu` | Passed. Cross-compilation is not Linux test execution. |
| `tools/check-dependencies.py` for macOS, Linux, and Windows | All four checked production/build boundaries passed. Dev-only edges are excluded. |
| Separate executable linked to the extracted engine/protocol/server boundary | Repeated the original capture workload; both fixture files were byte-identical, including terminal replies, deltas, resize/reflow, modes, and graphics. Temporary capture/smoke programs were removed after verification. |
| Isolated installer `cargo check --offline --manifest-path installer/bootstrapper/Cargo.toml --all-targets` on macOS | Passed package/lock resolution and unsupported-host targets only, not Windows installer UI or packaging. |
| Windows app cross-check from macOS | Blocked in dependency `ring` before application type-checking: Windows C headers/toolchain unavailable (`assert.h` missing). Native app build and visual behavior remain unverified. |
| Native Linux/Windows CI, Windows resource embedding, WSL interaction, installer build | Not run in this pass. No remote runtime host is configured. |

### Remaining Phase 1 gate

1. Run the configured neutral CI jobs on Linux, macOS, and Windows.
2. Build the real Windows daemon without graphics tooling, then build the app/probe and isolated installer with the normal Windows SDK setup.
3. On a qualified Windows/WSL machine, run `cargo test --locked -p compi-server --test daemon_integration` and the retained interactive recipes, especially input, resize, detach/reattach, selection/clipboard, and explicit descendant termination.

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
| `cargo build -p compi-app -p compi-server --bins --example compi-probe` | Native Mac client, daemon, and probe built and launched. |
| `cargo test --locked --workspace --all-targets` | 49 tests passed on macOS, including all six real Unix daemon scenarios. Windows-gated tests did not execute. |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Passed. Cargo still reports future-compatibility notices in upstream `block` and `proc-macro-error2`. |
| Windows server all-targets cross-clippy with `-D warnings` | Passed, including the retained Windows integration suite and new immediate-descendant ownership test compilation. No native runtime claim. |
| Linux server `cargo check --locked -p compi-server --all-targets --target x86_64-unknown-linux-gnu` | Passed; not Linux test execution. |
| Dependency checker on macOS/Linux/Windows | All neutral and server production/build boundaries passed. |
| Independent executable using `LaunchDescription` and `PtySession` | Literal spaces/quotes/`$HOME` in argv, explicit environment, native cwd, real TTY stdin/stdout, and natural exit passed. Temporary project removed. |
| Native Mac cold launch | App auto-started sibling daemon and native zsh; no prestarted server or manually created shell required. |
| Native AppKit interaction | Debugger-invoked text-input callbacks executed a shell command, launched Vim, edited/saved a file, and returned to the shell. Native window resize to 720×420 propagated to an 83-column × 20-row PTY. Fullscreen Vim and readable terminal text were visually confirmed. No permanent automation hook was added. |
| Native close and reopen | AppKit `performClose:` exited the client with code 0; its server and shell remained running/detached. Ordinary reopen attached the original session without another live shell. Separate reconnect proof preserved shell PID 53428 and a shell variable across client exits, and `stty size` observed 27×93 after resize. |
| Native probe | Real console input printed the same shell PID/variable; `stty size` matched its 40×120 canvas; Ctrl+] exited the probe with code 0 while keeping the shell alive. |

The native smoke used an Apple M5 Pro, macOS 26.6.2, arm64 debug builds, a 960×640 logical-pixel window with 2× capture scale, and native zsh/Vim. One cold launch logged first terminal frame at 552 ms; one ordinary warm reattach logged 153 ms. Sampled resident memory was about 77 MiB for the one-tab client and 5.2 MiB for the one-shell daemon, excluding the shell and its children. These are single-run diagnostics, not release budgets, physical frame-pacing measurements, or input-to-presentation latency.

### Remaining Phase 2 gates and known limits

1. Run native Linux CI and Windows/WSL regressions. No remote host was configured; local Docker had no running daemon. Cross-compilation is not native execution.
2. Qualify Windows PTY ownership. `portable-pty` exposes its Windows child after resuming it, while the current backend assigns a kill-on-close job before resume. The safer existing backend is retained; the new immediate-descendant regression is compiled but has not run on Windows. Windows portable-PTY cutover/fallback acceptance remains open.
3. Exercise physical Cmd/Ctrl/Option keys, clipboard, dead keys/IME, window dragging/traffic lights, fullscreen/scaling, and display pacing. Accessibility automation was unavailable. AppKit callback injection proved the native text/resize/close path, not physical input or all native controls.
4. Unix cleanup contains the owned POSIX session, including job-control groups; descendants deliberately escaping with `setsid` are not contained. This is not a security sandbox. Ordinary foreground/background cleanup passed.
5. v7's working-directory metadata is WSL-specific. Native cwd is honored at launch but not stored as fictitious WSL metadata; generic persisted launch metadata and fresh per-client launch context remain part of the later protocol/workspace cutover.
6. Font-derived terminal geometry, versioned font configuration, installed Nerd/Powerline fallback discovery, platform symbol/emoji fallbacks, and fixed-cell glyph placement are implemented. Visual qualification of the user's exact prompt and alternate font candidates remains open because screen capture and accessibility automation were unavailable.

An explicit `--working-directory` invocation retains the existing new-work behavior. Omit it when reopening existing work. Splits, workspace migration, durable client slots, command-registry redesign, and themes remain excluded from this phase.

## Current implementation: Mac refinement before workspace expansion

**Status:** typography and glyph integration implemented; interaction refinement and full native qualification remain. The terminal now derives cell advance, line height, and baseline from the resolved primary font; uses one geometry source for painting, PTY sizing, cursor, selection, hit-testing, images, and IME; invalidates shaped rows on display-scale changes; and rebases shaped fallback glyphs to logical terminal cells. Versioned native configuration and CLI overrides are wired through startup. The user has not selected a preferred reference terminal/font or clarified which interactions feel clanky.

### Completed — 2026-09-07

- Added the versioned native font configuration contract, explicit path/environment selection, invocation-local CLI overrides, field-wise recovery, and visible diagnostics.
- Added installed-font resolution with fixed-ASCII validation, font-derived device-snapped cell metrics, user fallbacks, an installed Nerd/Powerline fallback, and platform Unicode/symbol/emoji fallbacks.
- Replaced fixed Menlo-era geometry throughout PTY sizing, terminal painting, cursor, selection, hit-testing, scrolling, Kitty images, and IME caret placement.
- Rebased each shaped grapheme onto its protocol-defined logical cell while preserving intra-grapheme offsets, wide-cell spans, fallback font IDs, and color-emoji painting.
- Kept the bounded row-shaping cache and invalidated it when display scale changes. Native UI chrome continues to use the system font.
- Verified the Mac build, all 54 workspace tests, warning-free workspace Clippy, diff hygiene, first terminal frame, and an AppKit event-loop smoke. Pixel-level comparison and physical-input qualification remain blocked by unavailable screen-capture/accessibility permissions.

**Next:** manually compare the user's real prompt and restrained font/spacing candidates, then measure and fix demonstrated interaction roughness before Phase 3.

### 1. Establish the typography and glyph baseline — implementation complete, visual selection pending

- Inspect the current font selection, fixed cell metrics, row shaping/cache, cursor/selection geometry, and native input/repaint path in `crates/compi-app/src/gui.rs` and relevant `compi-client-core` helpers.
- Identify the actual missing prompt/path codepoints and available fonts. Distinguish ordinary Unicode/emoji from Powerline/Nerd Font private-use symbols; do not change the user's shell prompt to conceal missing glyphs.
- Capture a compact comparison workload: the real prompt/path, ASCII, bold/italic text, combining accents, CJK, emoji sequences, and private-use icons. Include a fullscreen editor.
- Present two or three restrained font/spacing candidates before treating a new default as settled. The direction is calmer Mac typography, not a new visual theme.

### 2. Replace rigid typography with font-derived geometry — complete

- Make terminal font family/size and line spacing configurable through the specification's existing configuration contract; do not invent a parallel settings format or build a preferences application.
- Derive cell advance, line height, and baseline from the chosen primary font instead of fixed Menlo-era constants. Keep fallback glyphs constrained to the terminal's logical cell widths.
- Migrate painting, PTY sizing, cursor, selection, hit-testing, IME caret geometry, and cache invalidation together when font/scale changes.
- Keep native system typography in chrome; adjust canvas padding and default spacing without changing the workspace model.
- Acceptance: readable regular/bold text with consistent baselines; selection and cursor remain aligned after font changes, zoom, and window resize.

### 3. Restore prompt symbols and emoji correctly — complete pending visual qualification

- Build an explicit fallback chain for text, Nerd/Powerline symbols, and color emoji. Prefer available fonts; if a bundled fallback is required, verify its license and coverage rather than assuming a font name exists.
- Preserve grapheme clusters, combining marks, emoji variation selectors/ZWJ sequences, and narrow/wide cell behavior. Fix the actual failing layer; do not replace unknown symbols with substitutes.
- Acceptance: the user's real shell path/prompt renders without missing-glyph boxes for supported symbols, and mixed text/icons/emoji do not overlap, clip, or shift the cursor. Copy/selection returns the original text.

### 4. Separate latency from interaction roughness, then fix both

- Measure key receipt → PTY → replica → presentation, plus scroll/resize/frame pacing under idle and sustained-output workloads. Record build/display context and compare the same workload before/after; do not label startup timings as input latency.
- Fix demonstrated stalls, unnecessary shaping/allocation, repaint scheduling, or queue behavior only where evidence points. Keep caches and transport queues bounded; no terminal-engine rewrite.
- Refine native focus, tab activation/close, titlebar controls/dragging, scroll behavior, and resize coalescing. Preserve Cmd application shortcuts and Ctrl terminal semantics.
- Do not add decorative animation to disguise stalls, or silently bundle splits, a palette redesign, or themes into this pass.
- Acceptance: responsive typing during output, predictable focus/tab actions, smooth scrolling/resizing without duplicate input, flicker, or shell restarts.

### 5. Qualify the refined Mac client

- Re-run native shell, fullscreen editor, input, resize, close/reopen, and same-process persistence scenarios on the actual window.
- Check physical Cmd/Ctrl/Option keys, clipboard, dead keys/IME, font fallback, and scaling where access permits. AppKit debugger callbacks are useful supplementary evidence, not a replacement for the physical-input matrix.
- Run focused regressions plus final workspace checks once integration is complete; keep permanent tests only for plausible failures such as grapheme/cell alignment, stale font caches, or input duplication.
- Preserve Windows/WSL behavior and headless dependency isolation. Report unavailable native platform checks explicitly.
- Finish with a runnable Mac build, before/after visual and interaction evidence, and updated qualification notes. Only then resume the Phase 3 workspace migration.

### Compaction handoff

- The implemented Phase 2 baseline and all previous evidence/gaps are recorded above. Do not restart the extraction or PTY port.
- Start the refinement with typography/glyph inspection; do not assume a chosen font, reference terminal, or diagnosed cause of “clanky.”
- Keep Mac GPUI `font-kit` enabled. Its absence selects a no-op text system; the prior missing-all-text failure was fixed, not an outstanding GPU renderer problem.
- Native smoke processes and temporary text/launch examples were removed. Production binaries remain in `target/debug`; ordinary launch is `./target/debug/compi --instance development`.
- Current checks: `cargo test --workspace` passed all 54 tests; `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build -p compi-app --bin compi`, and `git diff --check` passed. Native Mac launch reached the first terminal frame in 382 ms, and a mixed interactive workload remained in the normal AppKit event loop. Pixel-level comparison and physical-input qualification remain blocked by unavailable screen-capture/accessibility permissions. Windows cross-compilation was attempted, but this machine lacks the target C headers required by `ring`.

## Next phases

Continue in the [specification's migration and build order](Spec.md#migration-and-build-order):

- **Phase 3:** introduce workspace ownership, durable mutations, process-lifetime identity, and metadata migration.
- **Phase 4:** deliver the full workspace client, remembered window state, split overflow behavior, command palette, and whole-app themes.
- **Phase 5:** qualify shared daily use and produce Mac/Windows dogfood artifacts with attributable resource measurements.
- **Phase 6:** qualify distribution and extend process discovery/headless use.

Do not combine engine replacement, PTY migration, workspace migration, and UI redesign into one rewrite. Windows signing and installer qualification do not block the cross-platform foundation.

## Retained evidence

- [Windows terminal test recipes](testcmds.md): procedures for the existing implementation, to adapt using the acceptance map. Old shell-session and close-after-end UI expectations are not the new workspace contract.
- [Historical Windows acceptance results](ACCEPTANCE_RESULTS_2026-09-02.md): dated observations, not current cross-platform qualification.

The superseded specification, status report, release targets, and detailed old roadmap remain available in Git history.
