# Compi

## Status and authority

This document is Compi's authoritative product and technical baseline. It replaces the previous Windows-only contract and milestone ordering, preserved in Git history. It describes required behavior, not a claim that the behavior is already implemented.

The existing repository is the implementation starting point. Its terminal correctness, persistence, replication, rendering, and lifecycle work should be retained where it satisfies this contract. Windows-specific restrictions are not requirements to preserve.

The [README](../README.md) distinguishes the current implementation from this baseline. [Next steps](NEXT_STEPS.md) records the pending Phase 0 handoff. Dated acceptance reports and Windows test recipes are historical implementation evidence, not authority for the new scope or proof of cross-platform qualification.

## Product

Compi is a native, cross-platform terminal workspace backed by a persistent server.

Open it on a Mac and work in a native shell. Open it on Windows and work in WSL. Organize work into sessions, tabs, and split panes. Close the window and leave the work running. Reopen it and return to the same workspace.

Persistence and multiplexing must feel like ordinary terminal behavior. No prefix key, terminal mode, embedded tmux interface, or mandatory management ceremony.

Windows is first-class. macOS is first-class. Windows/WSL is a deployment arrangement, not the domain model.

### Product contract

- The server owns running processes, terminal state, and the persistent workspace structure.
- The client owns its window, focus, presentation preferences, and disposable terminal replicas.
- Closing or crashing a client never implicitly terminates work.
- Normal launch restores the last viewed live workspace. It does not create an extra shell on every launch.
- A fresh installation opens one session, one tab, and one shell without configuration.
- New sessions, tabs, and panes are explicit actions.
- Every live surface is discoverable and can be explicitly terminated.
- A stopped process is never presented as live or recoverable.
- Window closure, client detachment, process termination, and workspace deletion are different operations.
- The same core behavior is available on macOS and Windows. Mac usability does not wait for Windows signing or installer qualification.

### Feel bar

The baseline must be useful for daily development, not merely demonstrate a surviving shell.

- A usable terminal appears promptly, with no installer or configuration wizard in the development loop.
- Typing, scrolling, selection, clipboard, tab switching, and pane focus remain responsive during output floods.
- Tabs and panes have room to breathe. Layout controls do not compete with terminal content.
- A narrow window remains usable through overflow handling, minimum pane dimensions, and manual sidebar collapse.
- Switching between sidebar and top strip does not change or restart the workspace.
- Native window behavior, keyboard conventions, fonts, clipboard, and DPI handling fit the host platform.
- Reattaching does not replay raw output through a second terminal parser or wait for full scrollback transfer before painting.
- Persistence is visible through restored work, not through a dashboard the user must operate.

## Baseline scope

The baseline includes:

- Native macOS and Windows GPUI clients.
- A platform-neutral server that runs without a window system.
- Native Unix process hosting on macOS and Linux.
- WSL2 shell hosting for the Windows product.
- `portable-pty` integration rather than independent home-grown PTY implementations by default.
- A platform-independent terminal engine.
- A workspace actor and stable workspace identities.
- Protocol-only types and framing, independent of UI and platform code.
- Client replication isolated from rendering.
- Workspace, sessions, tabs, panes, and surfaces.
- Persisted, nested split trees with draggable dividers.
- Sidebar or top tab strip, with a resizable sidebar.
- Command palette and configurable platform-aware shortcuts.
- Client-local window and presentation state.
- Server-owned terminal and workspace state.
- Shell/profile, working-directory, environment, font, and whole-app theme-preset configuration, with an accessible live-preview theme picker.
- Discoverable detached work and explicit process-level termination.
- Headless diagnostics and cross-platform tests.

Native Linux server support is part of the foundation. Linux desktop rendering and distribution can follow the Mac/Windows baseline; they must not require a different server architecture.

## Workspace model

```text
Workspace
└── Sessions
    └── Tabs
        └── Split tree
            └── Panes
                └── Surfaces
```

### Definitions

| Term | Meaning |
|---|---|
| Server | One per-user process that owns a workspace and its live surfaces. Executable: `compi-daemon`. |
| Client | A native windowed application that presents a workspace. Executable: `compi`. |
| Workspace | All sessions, tabs, layouts, and surface records owned by one server instance. |
| Session | A named, ordered group of tabs for a body of work. It is not a PTY. |
| Tab | An ordered item in a session containing one split tree. |
| Split | A tree node with an axis, ratio, and two children. |
| Pane | A leaf of the layout tree that references one surface. It is a view location, not a process. |
| Surface | One process attached to a PTY, its authoritative terminal engine, history, graphics, and lifecycle state. |
| Replica | A client's disposable copy of the renderable state of one surface. |
| Attachment | A client's subscription to a surface. It is not the surface's lifetime. |
| Client state | Window geometry, presentation preferences, navigation, focus, and viewport state for one client instance. |

A session contains tabs. A tab contains panes. A pane displays a surface. Those identities must not be interchangeable in source code or protocol messages.

### Structural invariants

- IDs are stable opaque identifiers, never process IDs, labels, paths, or array positions.
- A tab always has a valid layout root. Its leaves reference valid panes and surface records.
- The baseline gives each surface one workspace pane. Multiple viewers of the same surface are not needed to deliver the baseline.
- Moving or reordering a pane does not respawn its surface.
- A split ratio is validated and constrained by minimum pane dimensions.
- Removing a split leaf collapses its parent into the surviving sibling.
- A failed split does not leave an orphaned process or partially committed layout.
- Session/tab labels are editable independently of terminal-generated titles.
- Workspace mutations have a revision and are published in an ordered stream.

### Split behavior

Commands are named **Split right** and **Split down**, not ambiguous vertical/horizontal split labels.

- Splitting creates a new surface and a new pane beside the focused pane.
- The new surface inherits the current launch profile and a validated working directory, unless explicitly overridden.
- New splits start at an equal ratio.
- Splits may nest in either direction.
- Dragging a divider updates local layout immediately. PTY resize events are coalesced and the final ratio is committed on release.
- Double-clicking a divider restores an equal ratio.
- Keyboard commands move focus between panes and resize the focused split.
- Split requests that cannot satisfy minimum dimensions are disabled with an explanation.
- Closing a client preserves the complete split tree and every live surface.

## State ownership and persistence

| State                                                                | Authority       | Persistence                                                                        |
| -------------------------------------------------------------------- | --------------- | ---------------------------------------------------------------------------------- |
| Workspace sessions, tab order, labels, pane references, split ratios | Server          | Versioned workspace file                                                           |
| Process and PTY handles                                              | Server          | Never serialized as recoverable processes                                          |
| Lifecycle, exit code, last known cwd, launch description             | Server          | Versioned workspace/surface metadata                                               |
| Grid, terminal modes, cursor, history, graphics                      | Server          | In memory across client disconnects; disk terminal checkpoints are not baseline    |
| Visible terminal replicas and history cache                          | Client          | Disposable; rebuilt from server state                                              |
| Window size, position where supported, maximized state               | Client          | Per-client state file                                                              |
| Sidebar/strip choice, sidebar width/collapse, font zoom, selected theme preset | Client     | Per-client state file                                                              |
| Selected session/tab and pane focus                                 | Client          | Per-client state where meaningful                                                  |
| Scroll position and selection                                        | Client          | Keyed by surface ID and server generation; restored only when anchors remain valid |
| User configuration                                                   | User-owned file | Never rewritten to remember transient UI changes                                   |

The split tree belongs to the workspace. Whether that workspace is presented with a sidebar or a top strip belongs to the client.

Client state wins over configuration for remembered window geometry, layout presentation, and the selected theme preset. Configuration seeds the first run. Resetting client state restores configured defaults. An explicit CLI override applies to that invocation without silently rewriting either file. Theme previews are transient; only an explicitly accepted selection is remembered.

### Server persistence

- Persist structural and lifecycle changes through versioned, atomic writes.
- Validate loaded IDs, tree shape, references, ratios, sizes, and launch metadata.
- Preserve a recoverable backup during schema migration.
- Quarantine malformed state and report the recovery action; do not silently discard the user's workspace.
- On server restart, previously running surfaces become `lost`, with the prior launch metadata retained.
- Preserve their tab and pane positions and show an explicit restart action.
- Restart creates a new process lifetime and clearly resets unavailable terminal state. It does not claim to resume the old process.
- Do not silently execute saved commands on server startup. Initial seeding applies only to a new empty workspace.
- Do not persist terminal input or secret-bearing environment values in workspace metadata.

### Lifecycle semantics

| Action/event | Result |
|---|---|
| Close window or quit client | Detach all that client's subscriptions; keep workspace and processes. |
| Client crash | Same server behavior as disconnect; relaunch can reattach. |
| Hide/detach tab from this client | Preserve its server-owned tab, tree, and surfaces; keep it discoverable in the workspace list. |
| Switch session or tab | Change navigation and subscriptions, never process lifetime. |
| Shell exits | Retain its final readable grid and exit status while the server remains alive. |
| End surface | Explicitly terminate its owned process tree and retain an exited pane for inspection or restart. |
| Remove pane | If live, confirm termination first; then remove its record and collapse the split. |
| Remove tab/session | Confirm the affected live-process count before ending and removing its contents. |
| Server failure or shutdown | Running processes are not recoverable; next server reports them as lost. |
| WSL runtime ends | Affected surfaces end or fail; unrelated server state remains truthful. |
| Reboot/sign-out/upgrade requiring server stop | No process-survival promise; disclose loss before intentional interruption. |

Window-close controls always mean detach. A tab close-view control also means detach/hide, preserving Compi's non-destructive close behavior. Destructive removal and **End surface** are separately named actions, not hidden behind the same control.

Termination must be asynchronous, show `Ending…`, escalate after a bounded grace period, and report failure rather than falsely marking surviving descendants dead. Platform tests must verify descendant cleanup, including WSL descendants. Do not assume closing a PTY or killing a launcher is sufficient.

## Architecture

```text
Native GPUI client
  ├── workspace presentation and commands
  ├── client-local window state
  ├── terminal replicas and history cache
  └── native grid renderer
              ↕ typed local protocol
Platform-neutral server
  ├── connection handling
  ├── workspace actor
  ├── surface runtimes
  │     ├── platform-independent terminal engine
  │     └── portable-pty integration
  └── workspace persistence
              ↕ host launch adapter
       native Unix shell or WSL shell
```

### Server boundary

The server runs without GPUI, a GPU driver, a window system, a desktop app host, or an installer process. It can be launched and tested independently from the client.

Its domain operations use launch descriptions, dimensions, surface IDs, workspace IDs, and byte streams. They do not require an HWND, Windows SID, WSL distribution, named-pipe handle, or platform-specific command line.

Platform code supplies identity, storage locations, PTY/process behavior, endpoint creation, and lifecycle integration. Platform differences do not compile away the entire server or application.

### Workspace actor

One actor is the single writer of workspace structure and lifecycle metadata.

It:

- Validates and serializes workspace mutations.
- Allocates IDs and coordinates surface creation and removal.
- Commits split, ordering, naming, and lifecycle changes.
- Persists the workspace and broadcasts ordered revisions.
- Resolves concurrent requests against explicit revisions.
- Ensures a failed process launch leaves a clear failed surface or rolls back a pending mutation consistently.

The actor must not parse terminal output, shape text, decode images, or wait synchronously for process termination or filesystem writes. Surface runtimes perform those jobs and report results through bounded channels.

### Surface runtime

Each surface owns its terminal engine and PTY lifecycle. Reader, writer, process-exit, and resize work must not hold the workspace actor hostage.

- PTY output feeds the authoritative terminal engine once.
- Engine-generated replies return to the same PTY.
- Damage produces sequenced screen updates.
- Outbound queues and retained terminal resources have explicit limits.
- A slow or disconnected client never stops the child process's output from being consumed.
- Blocking PTY I/O may use dedicated threads initially. Shared polling and thread parking are optimizations, not prerequisites for portability.

### Crate boundaries

Use a small Cargo workspace with actual dependency boundaries:

| Crate | Responsibility |
|---|---|
| `compi-protocol` | IDs, wire DTOs, frame codecs, versions, protocol limits. No GPUI, PTY, or OS APIs. |
| `compi-terminal` | Platform-independent authoritative engine, cells, modes, history, graphics state, replay tests. |
| `compi-client-core` | Replica application, recovery, viewport/selection logic, history cache, terminal key encoding. No GPUI or OS I/O. |
| `compi-server` | Workspace actor, surface runtimes, persistence, endpoint handling, host adapters; builds `compi-daemon`. |
| `compi-app` | GPUI renderer, workspace UI, command registry, client state, native platform integration; builds `compi`. |

The protocol must not depend on the terminal engine implementation. Convert between engine internals and wire DTOs at the server boundary. Client replication consumes the wire contract, not a server `ScreenState` instance.

PTY, transport, configuration, and platform support can begin as focused modules. Extract more crates only when dependency isolation or independent reuse justifies them. Installer code is not part of the core application library.

## Platforms and process hosting

### Supported arrangements

| Environment | Baseline arrangement |
|---|---|
| macOS | Native GPUI client, native local server, Unix PTY, user's native shell. |
| Windows | Native GPUI client, native local server, ConPTY via `portable-pty`, WSL2 shell launch adapter. |
| Linux | Headless native server and native Unix PTYs, exercised in CI. Desktop client qualification follows. |

A native Windows server is the initial migration path because Compi already has working lifecycle and deployment code. It is not a domain constraint. Moving the Windows product's server into WSL is a separate deployment decision, not required for Mac bring-up and not an implicit extra baseline transport.

### PTY integration

Use `portable-pty` behind a narrow integration layer for spawn, read, write, resize, wait, and termination coordination.

- Use a real PTY. Piped stdin/stdout is not a Unix implementation.
- Keep platform-specific process-group and descendant cleanup where required.
- Retain custom ConPTY code only if an acceptance test demonstrates a requirement the library path cannot satisfy.
- A fallback must implement the same host interface and pass the same behavior tests.
- Never make workspace objects conditional on the PTY backend.

### Launch contract

A launch description contains:

- Profile reference or executable and argument array.
- Working directory in the selected host's path namespace.
- Explicit environment overrides and approved inherited context.
- Initial rows and columns.
- Optional display name and non-secret process metadata.

Do not accept a single concatenated shell command as the universal launch API. Shell interpretation is explicit. Arguments and paths must survive spaces and quoting correctly.

Profiles resolve into host-specific executable, argv, cwd, and environment data at the launch boundary. The core does not inspect whether a profile is WSL or Unix to manage a session.

### Shell and environment fidelity

- macOS defaults to the user's configured native shell with platform-appropriate interactive/login behavior; it must not assume Bash.
- Windows defaults to the chosen WSL2 distribution and configured shell, preserving the existing Bash default until changed by configuration.
- Linux defaults to the user's configured shell.
- Explicit profiles can choose a shell or directly launch a process without changing workspace semantics.
- Preserve shell startup files, job control, SSH agent access, credential helpers, locale, and normal environment behavior.
- Terminal identity and required terminal variables are set deliberately and consistently with supported capabilities.
- Do not reorder PATH or inject startup commands to simulate correct cwd handling.
- Long-lived servers receive a validated fresh launch context for new surfaces; they must not blindly reuse stale client-sensitive environment values.
- Logs and persisted state must not expose environment secrets.

### Working directories

- New surfaces inherit a valid current directory from the focused surface when available, otherwise the profile's starting directory or home.
- OSC 7 supplies cwd updates, with host/path validation and a last-known fallback.
- Resolve Windows paths to WSL paths only in the WSL launch adapter, using the selected distribution's `wslpath` behavior.
- `/mnt/c/...` remains a supported working location. No copy bridge, automatic repository relocation, or forced migration into a Linux filesystem.
- Mac paths remain Mac paths. Linux paths remain Linux paths.
- Invalid directories produce an actionable error or an explicitly accepted fallback, not a silent different project location.

## Local transport and protocol

### Transport

Expose a small listener/stream boundary with OS-local implementations:

- Unix-domain sockets on macOS/Linux.
- Current-user-restricted named pipes on native Windows.

An existing local-socket library may provide the mechanical plumbing, but security and lifecycle behavior must be verified on each OS. A common API does not establish authorization by itself.

- Restrict endpoints to the current user through permissions/ACLs and applicable platform peer checks.
- Store endpoints in private, platform-appropriate locations.
- Enforce one server per user and instance with race-safe ownership.
- Recover stale endpoints only after confirming the previous owner is absent.
- Baseline local operation does not open a network listener.
- TCP, remote discovery, authentication, and tunneling are separate scope. Loopback is not treated as inherently authenticated.

### Wire contract

Retain typed length-prefixed framing where practical:

```text
u32 payload length | u8 message type | payload
```

Specify endianness, exactly what the length counts, and maximum accepted sizes in the protocol crate. Reject invalid or oversized frames before allocation.

Use readable JSON for low-rate control messages and compact binary screen payloads. Preserve useful existing binary work; do not regress to JSON grids or introduce a second encoding stack merely to mirror another project.

- Hello negotiates protocol version, capabilities, server identity, and server generation.
- Request IDs correlate replies; workspace revisions order structural updates.
- Separate ID types distinguish sessions, tabs, panes, surfaces, and client connections.
- Unsupported peers fail explicitly. Incompatible state is never decoded speculatively.
- Workspace hierarchy changes require a deliberate migration/version boundary from the old session-equals-shell model.
- Control and screen traffic are distinct logical classes. Separate connections are optional, not a requirement.
- A shared connection must bound screen traffic and prioritize input/control without corrupting ordered state.

### Replication

- Attach returns a snapshot with an authoritative sequence baseline.
- Deltas describe changed rows and required cursor, modes, title, graphics, and history metadata.
- Each delta identifies the state sequence it depends on. Coalescing must not turn valid skipped intermediate frames into undetectable corruption.
- A sequence gap requests a fresh snapshot and discards uncertain updates.
- Reconnect verifies server generation before restoring selections or cached history.
- History is fetched in bounded pages and cached locally; first paint does not require transferring all retained history.
- Resize/reflow invalidates or remaps history and selection anchors explicitly.
- Visible panes receive active screen streams. Hidden work receives lightweight status/title updates without unnecessary continuous grid transfer.
- Queue overflow is handled by recoverable snapshot resynchronization, not unbounded memory or silent dropped keystrokes.

The baseline supports one controlling attachment per surface. A client can control all visible panes. A second client requesting an already-controlled surface receives an explicit conflict with a safe detach/open-other-work path. No silent resize fights or takeover. Multi-viewer semantics may extend this later.

## Terminal engine and rendering

### Engine decision

Retain Compi's existing `vte` plus custom `ScreenState` as the initial platform-independent terminal engine, extracted and tested without Windows or GPUI dependencies.

Platform independence does not require replacing it with `alacritty_terminal`. A mature replacement can be evaluated if real compatibility or maintenance evidence warrants it. Any replacement must preserve required terminal behavior, graphics support, reflow, tracing, and deterministic regression coverage.

Do not rewrite the engine at the same time as the workspace and platform migration without a demonstrated blocker.

### Required terminal behavior

- Unicode graphemes, combining marks, wide cells, emoji, and correct copied text.
- ANSI colors, attributes, cursor shapes, cursor visibility, and save/restore.
- Main/alternate screens, margins, origin mode, scrolling regions, insertion/deletion, wrapping, and erase operations.
- Resize and logical-line reflow without corrupting history or selection.
- Bounded server scrollback and bounded client history caches.
- Bracketed paste, application cursor/keypad modes, modified/navigation/function keys, and job-control input.
- Mouse reporting, focus reporting, wheel behavior, and Shift selection override.
- OSC 7 cwd, OSC 8 hyperlinks, and bounded write-only OSC 52 handling.
- Kitty graphics with bounded transfers, decoding, placements, deletion, clipping, resize, and reattachment behavior.
- Required device-status and color replies generated by the server-side engine.
- Categorized, rate-limited diagnostics for unsupported or rejected sequences.

Escape sequences are untrusted process output. Hyperlink opening validates schemes and requires user interaction. OSC 52 obeys user policy, never exposes clipboard reads, and is routed only to an appropriate controlling client. Replayed snapshots must not replay clipboard or bell side effects.

### Native rendering

- GPUI renders terminal cells from client replicas, never by independently parsing PTY output.
- Terminal input stays in Rust and does not require a web or JavaScript layer.
- Paint visible rows and required graphics, not full history snapshots.
- Cache shaped runs/rows using keys that include relevant font, style, scale, and content state.
- Decode images off the UI thread with explicit memory limits.
- Repaint on state changes, cursor timers, and interaction, coalesced to display opportunities.
- Clipboard, text composition/IME, font fallback, links, window controls, and DPI handling are platform adapters.
- Mac and Windows terminal paint paths share the same replica and layout logic.

## Client workspace and controls

### Workspace navigation

- A session selector names the current body of work and exposes create, switch, rename, and remove actions.
- The active session's tabs appear in the sidebar or top strip.
- Detached/hidden tabs remain available through the workspace list and palette, with clear running/exited/lost state.
- Tab labels show an explicit user label when set, otherwise a useful terminal title or cwd label.
- Tabs can be reordered without restarting processes.
- The visible tab renders its full split tree; focus is visibly identifiable without a thick decorative frame.
- Empty, disconnected, exited, failed, ending, and lost states have explicit recovery actions.

### Sidebar and strip

- Sidebar is the first-run default and can be switched to a top strip at runtime.
- Sidebar width is draggable, clamped, and remembered.
- Double-click reset restores the configured default width.
- Top strip uses scrolling/overflow rather than compressing every label into an unreadable sliver.
- Manual sidebar collapse and expansion recover terminal space on small windows. The control remains reachable when collapsed, and the collapsed state is remembered.
- Navigation areas scroll independently from the terminal.
- Window controls reserve platform-appropriate space, including Mac traffic lights.
- The layout must not depend on a single fixed window size or Windows-only titlebar geometry.
- Narrowing a window does not automatically collapse the sidebar, zoom a pane, or switch to a single-pane presentation. Presentation changes do not rewrite the persisted split tree.

### Command registry

One typed registry drives the command palette, keybindings, menus, and enabled/disabled states.

Baseline commands cover:

- Create, switch, rename, and remove session.
- New tab, switch tab, reorder tab, detach tab, restore hidden tab.
- Split right/down, focus pane, resize split, remove pane.
- End surface and restart exited/lost surface.
- Toggle sidebar/strip, collapse/expand sidebar, reset sidebar width.
- Copy, paste, select all where appropriate, and clear scrollback.
- Font zoom in/out/reset.
- Change theme through the live-preview theme picker.
- Open configuration, reset client layout, reconnect, and open diagnostics.
- Quit client, explicitly separate from stopping the server.

The palette supports query filtering, keyboard navigation, Enter to execute, Escape to dismiss, visible shortcuts, and explanations for disabled actions. It is not a terminal mode.

### Keyboard and input policy

- macOS uses Command-based application shortcuts and preserves terminal Control input.
- Windows uses terminal-safe application shortcuts, generally Control+Shift combinations.
- Retain selection-aware Windows Control+C: copy when a non-empty selection exists, otherwise forward interrupt. Control+Shift+C remains explicit copy.
- Platform-specific exceptions are configurable and tested, not scattered across widget handlers.
- Palette/menu focus owns its navigation keys while open; terminal focus resumes on dismissal.
- Bracketed paste is honored. Pasting never implicitly executes an extra newline beyond the clipboard contents.
- IME and composed text follow native input paths.

## Configuration and appearance

Use a versioned TOML configuration with documented defaults. A configuration file, an **Open configuration** command, and a small in-app theme picker are baseline; a full graphical preferences application is not required.

Configuration includes profiles, shell/login behavior, starting directory, environment overrides, fonts, font size, line height, the initial whole-app theme preset, initial layout, keybinding overrides, scrollback/graphics limits, and clipboard policy.

Invalid configuration must produce a useful diagnostic and a safe recovery path. Apply independent valid settings where possible; never silently launch an unintended executable. Configuration and client state are separate files.

### Theme presets and access

- The first-run theme is **Dark Glass**: neutral-dark application surfaces, restrained glass in the sidebar and window chrome, and an acid-green accent. Warm Carbon is an optional whole-app preset, not a mandatory application color system.
- Each preset supplies a coordinated application and terminal appearance: chrome, opaque terminal canvas, text, borders, focus, selection, cursor, and ANSI colors. Selecting a preset changes the whole appearance together.
- The baseline offers whole-app presets, not independent terminal-palette or accent overrides, custom theme files, or automatic system light/dark switching.
- A visible **Appearance…** menu action and the command palette's **Change theme** command open the same picker. Choosing a theme never requires editing TOML.
- The picker lists named presets and previews them live in the current window. Keyboard navigation changes the preview; Enter or an explicit apply action accepts it; Escape or dismissal cancels it and restores the previously selected preset.
- Only an accepted theme selection is saved to client state. Previewing or selecting a theme does not rewrite user configuration, restart a process, detach a surface, or reset terminal contents.
- The picker owns keyboard input while open and returns focus on dismissal. Labels, focus, and selection remain readable and keyboard-accessible.
- The appearance UI is limited to theme selection. Fonts and other existing configuration remain available through TOML and the already-defined font-zoom commands; separate graphical font, transparency, palette, accent, and reset controls are not baseline.

### Glass and readability

- Glass is limited to the sidebar and window chrome. Terminal canvases remain opaque in every baseline preset; there is no terminal-transparency control.
- Use native background materials where supported. Provide a readable opaque equivalent when materials are unavailable or the platform requests reduced transparency. Identical blur across platforms is not required.
- Presets must provide readable text and controls, adequate contrast, and visible focus in both material and opaque presentations. Color alone must not be the only indication of focus or lifecycle state.

### Brand

Compi keeps its acid-lime dinosaur-eye mark with the black `/` pupil. Acid green (`#DFFB35`) is the default theme accent. The mark establishes product identity without forcing warm chrome or a single terminal ANSI palette across all presets.

The optional **Carbon** preset retains these warm color tokens; they are not the default Dark Glass palette:

| Role | Color |
|---|---|
| App background | `#171613` |
| Chrome | `#211F1A` |
| Raised surface | `#2B2922` |
| Border | `#403C31` |
| UI text | `#F4F1E8` |
| Muted text | `#AAA394` |
| Accent | `#DFFB35` |
| Initial terminal canvas | `#1A1916` |

Default typography is IBM Plex Sans for chrome and IBM Plex Mono for the terminal, with appropriate native fallbacks. Users can configure terminal typography independently of application chrome; colors are selected together through a whole-app preset.

Use restrained spacing, readable labels, real icons, horizontal peer actions, visible focus, and sentence case. Avoid oversized dashboard chrome, decorative status bars, and fixed compactness that makes the terminal feel squeezed.

## Startup, storage, and distribution

- A source checkout on Mac or Windows must build and run without an installer or registration step.
- The client discovers the current user's server and starts it when absent, using race-safe startup and a bounded readiness handshake.
- Auto-started server lifetime is independent of the launching client.
- Existing platform supervision may be reused, but registration is optional for development and never required by domain logic.
- An idle server may exit only when no live work requires it. Detached work is not idle merely because no window is open.
- Instance selection isolates development/test state, endpoints, logs, and processes from the normal user workspace.
- Platform adapters resolve configuration, state, cache, logs, and endpoint paths. Core code does not embed `%LOCALAPPDATA%`, `/tmp`, or a username.
- Server/client version mismatch is explained without silently killing work to upgrade.
- Updates requiring server termination warn about affected processes.
- Mac bundles and Windows portable builds are produced early enough to dogfood on real machines.
- Signing, notarization, clean-profile installers, repair, and uninstall are distribution gates, not gates on cross-platform architecture work.

## Performance and resource discipline

Correctness and responsive interaction are mandatory. Performance targets must identify platform, hardware, build, display, workload, and measurement boundaries.

Do not claim a startup or memory win from another project's architecture or screenshot. Existing Windows measurements are historical evidence, not assumed Mac results.

Measure separately:

- Empty GPUI client cost.
- Server with no surfaces.
- Marginal cost of each idle, active, and detached surface.
- Visible pane count and cached history cost.
- Cold launch, warm launch, reconnect, and ready-for-input.
- Input event through queue, server, PTY, engine, and frame presentation.
- Paint CPU time versus actual frame pacing.
- Private memory, working set, GPU memory, threads, handles/file descriptors, and process counts.

Baseline gates:

- Input/control remains usable during sustained output.
- Paint fits the measured display frame budget under the representative workload.
- Per-surface and per-client queues, history, graphics, and trace retention are bounded.
- Repeated create/split/resize/detach/reconnect/end cycles leak no owned resources.
- A mixed-workload soak shows no unexplained sustained resource growth.
- Mac and Windows reports distinguish automated timing from physical input/display qualification.

Set numeric release budgets from measured platform baselines and explicitly review regressions. Do not reintroduce unmeasured fixed startup/memory ceilings that freeze product development. Parking, history compression, and shared PTY polling follow measured marginal costs.

## Diagnostics and acceptance

### Headless tools

Retain `compi-probe` as a diagnostic target, backed by the normal protocol. It must inspect workspace structure, list surfaces and lifecycle state, exercise isolated launches, attach to screen state, resize, and request explicit termination without GPUI.

Diagnostic traces are opt-in and bounded. They can contain sensitive terminal output, must remain local by default, and must never be silently uploaded. Workspace metadata is not a substitute for a terminal trace.

### Automated coverage

- Protocol round trips, malformed/oversized frames, version rejection, and capability negotiation.
- Workspace tree invariants, revisions, split rollback, reordering, removals, and persistence migration.
- Replica application equivalence, gap recovery, history eviction, reflow, and generation changes.
- Deterministic terminal replays preserving existing compatibility cases.
- Real Unix PTY tests on macOS/Linux and real ConPTY/WSL tests on Windows.
- Spawn argv/cwd/env correctness, job control, resize, exit, and descendant cleanup.
- Disconnect/reconnect without process loss or unintended shell creation.
- Server loss, stale endpoints, malformed workspace files, and explicit restart behavior.
- Pure layout, command routing, client-state precedence, and shortcut tests independent of GPUI.

CI must build/test neutral crates and the server on Linux, macOS, and Windows. It must build the native client on macOS and Windows. WSL runtime tests require a qualified runner; an unavailable WSL environment is reported as missing coverage, never a passing runtime test.

### Daily-use qualification

On both macOS and Windows/WSL:

1. Open a native window into a real shell without an installer.
2. Create sessions, reorder tabs, and build nested splits.
3. Resize the window and dividers; manually collapse/expand the sidebar; switch sidebar/strip and relaunch. Verify presentation choices persist without changing the split tree.
4. Run shell editing/job control, Git, `less`, Vim/Neovim, `fzf` preview, and a real agent harness.
5. Verify Unicode, clipboard, mouse reporting, links, graphics, and wrapped selection.
6. Close and reopen the client; recover the same live processes and split tree.
7. Discover hidden/detached work and explicitly terminate it, including descendants.
8. Exercise output floods while typing and navigating another pane.
9. Test a narrow window, maximized/fullscreen behavior, display scaling, and native window controls.
10. Run an isolated mixed-workload soak and record failures with reproducible traces.

Physical keyboard/display checks remain necessary for a public release. Their absence must not defer Mac implementation or basic workspace functionality.

## Migration and build order

This is a migration of a working terminal, not a blank-slate rewrite. Keep the existing Windows behavior covered while making the same product usable on Mac.

### 1. Extract the neutral foundation

- Separate protocol, terminal engine, and client replication from GPUI and Windows imports.
- Make the server build graph independent of GPUI and installer dependencies.
- Preserve terminal replay and protocol recovery coverage.
- Add Linux/macOS neutral-core CI immediately.

Done when core tests run on all three OS targets and headless server builds do not require graphics tooling.

### 2. Prove a real Mac terminal end to end

- Integrate `portable-pty` and host launch descriptions.
- Add Unix local transport and platform paths.
- Enable the same server/client application modules on Mac.
- Implement native Mac window, font, clipboard, key, and input behavior.
- Keep the initial view simple while proving the shared execution path.

Done when a Mac window opens a native shell, handles interactive programs and resize, then closes/reopens onto the same live surface. Windows/WSL still passes its regression path.

### 3. Introduce workspace ownership and migration

- Add session/tab/pane/surface IDs and the workspace actor.
- Map each old shell-session record to a surface inside a tab in an imported session.
- Migrate metadata with backup and explicit version checks.
- Require a warned server restart for incompatible live-state upgrades; do not promise in-place process migration.
- Add atomic workspace persistence and lost-surface recovery.

Done when the protocol and headless tools manipulate the hierarchy, survive client disconnection, and restore truthful structural state after server restart.

### 4. Deliver the full workspace client

- Build session navigation, tab ordering, and nested splits.
- Add draggable dividers, pane focus, sidebar/strip switching, and a resizable, manually collapsible sidebar.
- Add the command registry, palette, and platform keybindings.
- Add whole-app theme presets and the keyboard-accessible live-preview picker, with glass limited to sidebar/window chrome and readable opaque fallbacks.
- Persist client-local state independently of server structure.
- Implement non-destructive detach and clearly named termination/removal actions.

Done when the full daily-use workspace can be created and reopened on Mac and Windows without process restarts caused by layout changes.

### 5. Qualify the shared baseline

- Run the representative workflows and mixed-pane soak on both platforms.
- Address narrow-window, font, IME, clipboard, scaling, and native-control failures.
- Publish attributable latency/resource measurements and missing coverage.
- Produce runnable Mac and Windows dogfood artifacts.

Done when the owner can work on Compi using Compi on either primary machine. A Windows-only release candidate is not completion of this baseline.

### 6. Qualify distribution and extend process use

- Finish platform packaging, signing/notarization, upgrade, repair, and clean-machine qualification.
- Add agent discovery metadata and polished headless workflows using the same launch, workspace, and lifecycle contracts.
- Keep process hosting separate from agent memory, credentials, steering, and orchestration.

The launch API is generic from the beginning. An agent is a process in a surface, not a different server architecture.

## Explicitly outside this baseline

- Process resurrection after server death, reboot, or runtime termination.
- Remote SSH hosts, network listeners, cloud accounts, relays, and cross-machine workspace synchronization.
- Shared multi-user workspaces or simultaneous controlling clients on one surface.
- Arbitrary dock frameworks, floating tool panels, and non-terminal pane applications.
- A web client, phone client, plugin runtime, or React/Bun/gpuix migration.
- A full graphical preferences application.
- Disk-persisted terminal history/checkpoint restoration across server restart.
- Unmeasured memory parking and process/thread scheduling redesigns.
- Full graphics-protocol parity beyond the declared terminal support.
- Embedded agent orchestration, model providers, memory systems, or credential management.

These exclusions do not include Mac support, native Unix hosting, split layouts, or basic customization. Those are the baseline.

## Definition of done

Compi's new baseline is complete when:

- The owner can build, run, develop, and dogfood the native client on Mac and Windows.
- The server and terminal engine are platform-neutral in their dependencies and domain model.
- Real PTYs and local transports work through platform adapters.
- Workspaces, sessions, tabs, panes, and surfaces are distinct, functioning concepts.
- Nested split trees persist without owning the lifetime of their processes through the client.
- Sidebar/strip choice, sidebar width/collapse, window state, focus, and the accepted theme preset remain client-local.
- The command palette exposes the ordinary workspace and process controls.
- Users can discover, preview, cancel, select, and restore their selected whole-app theme across relaunch without editing configuration or interrupting terminal work.
- Closing and reopening returns to live work without duplication, data corruption, or implicit termination.
- Lost processes are reported truthfully and restart requires explicit action.
- Both primary platforms pass the shared daily-use qualification with remaining distribution-only gaps identified.

Compi is not a Windows application waiting for a Mac port. It is one persistent terminal workspace with native platform integrations.

## References

- [Compi repository](https://github.com/cloudboy-jh/compi): existing implementation to migrate and preserve where compatible.
- [SuperTerminal repository](https://github.com/sonnylazuardi/superterminal): reference for workspace structure, native terminal-first interaction, and platform separation, not a mandate to copy its stack.
- [Next steps](NEXT_STEPS.md): pending Phase 0 implementation-contract decisions.
- [Windows terminal test recipes](testcmds.md): existing exercises to adapt to the cross-platform qualification matrix.
- [Historical Windows acceptance results](ACCEPTANCE_RESULTS_2026-09-02.md): dated evidence from the previous implementation, not current baseline qualification.
