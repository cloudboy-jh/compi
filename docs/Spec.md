# Compi

## Status and authority

This document is Compi's authoritative product and technical baseline. It replaces the previous Windows-only contract and milestone ordering, preserved in Git history. It describes required behavior, not a claim that the behavior is already implemented.

The existing repository is the implementation starting point. Its terminal correctness, persistence, replication, rendering, and lifecycle work should be retained where it satisfies this contract. Windows-specific restrictions are not requirements to preserve.

The [README](../README.md) distinguishes the current implementation from this baseline. [Next steps](NEXT_STEPS.md) records the completed Phase 0 contracts, Phase 1 extraction evidence, and pending platform qualification. Dated acceptance reports and Windows test recipes are historical implementation evidence, not authority for the new scope or proof of cross-platform qualification.

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
- A split ratio is finite and strictly between zero and one. Rendering constrains the effective ratio by recursive minimum pane dimensions without rewriting the saved ratio.
- Removing a split leaf collapses its parent into the surviving sibling.
- A rejected split changes nothing. An accepted split commits a complete tree with a `starting` surface before launch; launch failure retains a `failed` pane, never an orphaned process or partial tree.
- Session/tab labels are editable independently of terminal-generated titles.
- Workspace mutations have a revision and are published in an ordered stream.

### Split behavior

Commands are named **Split right** and **Split down**, not ambiguous vertical/horizontal split labels.

- Splitting creates a new surface and a new pane beside the focused pane.
- The new surface inherits the current launch profile and a validated working directory, unless explicitly overridden.
- New splits start at an equal ratio.
- Splits may nest in either direction.
- Dragging a divider updates local layout immediately. PTY resize events are coalesced and the final ratio is committed on release.
- Double-clicking a divider requests an equal ratio; rendering still respects both children's recursive minimum dimensions.
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
| Scroll position and selection                                        | Client          | Keyed by server identity/generation, surface ID, and process lifetime; restored only when anchors remain valid |
| User configuration                                                   | User-owned file | Never rewritten to remember transient UI changes                                   |

The split tree belongs to the workspace. Whether that workspace is presented with a sidebar or a top strip belongs to the client.

Client state wins over configuration for remembered window geometry, layout presentation, and the selected theme preset. Configuration seeds the first run. Resetting client state restores configured defaults. An explicit CLI override applies to that invocation without silently rewriting either file. Theme previews are transient; only an explicitly accepted selection is remembered.

### Durable client identity and navigation

A durable `ClientId` identifies one remembered window-state slot, scoped to the local user and server instance. It is not a PID, connection ID, attachment, or authority to take control of a surface. A new connection gets a fresh `ConnectionId`; reconnecting does not change the window's `ClientId`.

- An instance has one primary slot. Ordinary launch claims it when free; otherwise it claims the oldest unclaimed auxiliary slot, ordered by creation ordinal, or creates an auxiliary slot from configured defaults. **New window** uses the same allocation rule. Opening one window does not automatically reopen every remembered window.
- Allocation is serialized through a local registry lock. Each active window holds an OS-released exclusive slot lock until exit; a crash releases ownership without deleting remembered state. No two windows write the same state file. Lock or storage errors are surfaced, not bypassed with an unlocked writer.
- Slots use versioned atomic state files. Closing or crashing a window preserves its slot; relaunch reuses the deterministic available slot. Malformed state is quarantined with a visible recovery message and configured defaults, without changing server work. A failed save leaves the live presentation usable but explicitly unsaved.
- Each slot remembers window geometry, sidebar/strip choice, sidebar width and manual collapse, font zoom, accepted theme, selected session, last selected tab per session, last focused pane per tab, and a set of hidden tab IDs. Navigation and hidden membership are scoped by stable server/workspace identity, not server generation, so they survive a server restart.
- Hidden membership is an exclusion set: tabs not hidden in this slot are listed in server order, including newly created tabs from another window. Hiding releases this window's attachments to that tab, changes no server structure, and does not hide it in other windows. Window close releases attachments but does not add tabs to the hidden set.
- Restoring a hidden tab removes its exclusion, selects its session/tab, and attempts attachment. The workspace list and palette include hidden tabs with their actual lifecycle state. A control conflict leaves an explicit unavailable pane with retry/open-other-work actions; it never steals control, unhides other tabs, or creates a replacement shell.
- On an authoritative workspace snapshot, prune references to removed objects. Keep hidden membership and navigation during temporary disconnection. If the selected tab is removed or hidden, choose the next non-hidden tab in server order, then the previous one; if none exists, show an empty session view with create/restore actions. If a session disappears, choose its next surviving session, then the previous one; at initial restore with no valid remembered position, choose the first session and first non-hidden tab.
- Remembered focus is restored only within the selected tab; an invalid pane reference falls back to its first leaf in tree traversal order. Merely switching sessions/tabs restores their last valid navigation and releases screen/control attachments from the previous visible tab.
- If every tab is hidden, or the workspace is intentionally emptied, show the empty view. Never seed another shell to repair navigation. First-run seeding occurs exactly once for a new, uninitialized workspace and is committed durably with its initialized marker.
- CLI overrides affect only that invocation. State saving excludes override-derived values unless a later explicit user action changes the setting. Reset client layout clears the slot's remembered geometry, presentation, navigation, and hidden set to configured defaults; it neither deletes work nor changes another slot. Only accepted theme choices are durable, never previews.

Client-state write failures cannot change server mutation results. Per-slot serialization and revision-based server conflicts are separate mechanisms: independent windows may remember different views of the same workspace while the server remains its sole structural writer.

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

### Process lifetimes and terminal identity

`ServerId` is stable for an instance's persisted workspace; `ServerGeneration` is fresh for every server process. `SurfaceId` is stable across explicit restart. A fresh opaque `ProcessLifetimeId` is allocated and persisted for every accepted launch attempt, including one that fails before producing a process. IDs are never reused; restoring metadata does not restore process handles.

The terminal identity key is `(ServerId, ServerGeneration, SurfaceId, ProcessLifetimeId)`. Attach replies, snapshots, deltas, history requests/replies, lifecycle observations, and transient terminal side effects carry this key. Input, resize, and termination requests target the expected lifetime as well as the current attachment where applicable. A stale target fails explicitly rather than operating on a replacement process.

- Only exited, failed, or lost surfaces may restart, after the runtime confirms no owned descendants remain. Restart preserves session/tab/pane/surface IDs, commits a new lifetime in `starting`, then launches. An old lifetime can never become current again.
- Within a lifetime, screen sequences increase monotonically; snapshots establish a baseline and deltas name their predecessor. Starting a new lifetime resets its screen sequence to zero. Resnapshot and reconnect do not reset a lifetime's sequence.
- Accepting a new lifetime immediately invalidates the old grid, terminal modes, title derived from output, graphics, history cache, scroll/selection anchors, and pending terminal effects. User labels and validated launch metadata remain. Show `Starting…` or the explicit failure until the new snapshot is available; do not paint an old final grid as the new process.
- Delayed deltas, page responses, input acknowledgements, and clipboard/bell effects with an old key are discarded. Asynchronous image decode results are also keyed, so they cannot populate a replacement lifetime's cache. A restart revokes old control attachments; clients must attach afresh before sending input or resize.
- Ordinary exit retains the current lifetime's final readable grid and exit information in server memory. It does not reset its identity or anchors. Server restart discards every terminal replica/cache and reports formerly `starting`, `running`, or `ending` records as `lost`; saved exited/failed records retain metadata but have no recoverable terminal grid.
- Reconnect always obtains an authoritative snapshot before accepting new deltas. For an unchanged key, restore selection/scroll only if all anchors remain valid. History anchors include a history epoch and stable logical-line IDs/offsets, never raw viewport row numbers. Reflow explicitly remaps anchors or advances the epoch; eviction invalidates removed anchors. Without a proven mapping, clear selection and return to the live bottom. Invalidate outstanding history pages from an obsolete epoch.
- Disk client state may remember keyed anchors, not terminal contents. A new generation or lifetime makes those anchors unusable. No snapshot replays clipboard or bell side effects.

### Durable workspace mutations

The actor has one durable committed workspace revision and at most one persistence commit in flight. A worker performs filesystem I/O; the actor continues handling connections and runtime observations while serializing structural commits. Admission queues are bounded; excess mutations return `Busy`, not an unbounded backlog.

Every mutation supplies `ServerId`, expected server generation, an opaque `MutationId`, and `expected_revision`. Wire request IDs correlate individual replies; mutation IDs identify the same attempted operation across reconnect. Check a duplicate mutation's receipt before checking its base revision. Otherwise validate the expected revision at execution time against the committed revision, not at queue admission; stale requests return `RevisionConflict` with the current revision and have no effects.

Commit ordering is:

1. Validate the complete proposed tree, IDs, launch metadata, operation eligibility, and any live-process confirmation against the expected revision. Reserve a candidate revision and IDs without exposing them as committed.
2. Persist the entire candidate, incremented revision, and mutation receipt as one atomic replacement. Durability requires flushing the file and completing the platform's durable replacement/directory-metadata procedure before reporting success. A storage backend that cannot provide this must fail explicitly.
3. Install the candidate as the committed actor state. Enqueue its revision event before the successful mutation reply on each affected connection; success includes the committed revision, affected IDs, and operation state. Clients tolerate reply/event duplication and recover gaps using a workspace snapshot.
4. Only after the intent is durable, dispatch its process side effect. A success reply for `starting` or `ending` acknowledges durable intent, not a successful spawn or completed termination. Runtime outcomes become later lifecycle commits.

No structural success or committed revision event is published before step 2. Workspace snapshots contain committed state only. A slow connection that cannot retain the ordered stream is marked for snapshot resynchronization, not allowed to stall commits.

Retain the most recent 1,024 committed mutation receipts with a cryptographic fingerprint of the canonical request, revision, and non-secret result in workspace state; never persist raw request environment values or terminal input in a receipt. Reusing a retained ID with a different fingerprint fails. An exact retry returns the stored result without repeating side effects, even if its base revision is now old. Without a receipt, a mutation submission follows the normal generation/revision checks; a separate read-only outcome lookup returns `OutcomeUnknown` and never submits work. Clients keep at most one unresolved mutation per window and reconcile its receipt/snapshot before issuing another. Never automatically retry uncertain work with a new ID or updated base revision: even after receipt eviction, its original base revision cannot apply twice because every successful mutation advances the revision.

Receipts survive restart for result lookup, but do not imply that a process still exists; replies include the current generation and require the client to refresh current lifecycle state. An unrecorded request targeting an old generation is rejected, never executed after restart. A crash after durable replacement but before publication is recovered by loading that committed revision and receipt.

| Operation or failure | Required outcome |
|---|---|
| Create session/tab, split, or restart | Commit a valid structure and fresh lifetime in `starting` before spawn. Launch success commits `running`; launch failure commits `failed` with an actionable reason and retains the pane. If a partial launch produced owned processes, clean them up before reporting `failed`; incomplete cleanup remains `ending` with an error and retry action. No automatic relaunch or removal. Initial seeding uses this same path. |
| Validation failure or definite write failure before replacement | No published revision, successful receipt, spawn, or termination. The previous committed workspace stays authoritative; report the failure. A rejected split leaves its original tree unchanged. |
| Ambiguous replacement/flush failure | Stop admitting mutations and dispatching new process side effects. Report persistence unavailable; reconcile the old and candidate file with the worker and complete durability before publishing either outcome. If that cannot be established, remain read-only with explicit recovery guidance; never guess that rollback occurred. |
| Rename, reorder, or final divider ratio | Persist the new structure before success. Failure restores the committed presentation; optimistic UI is visibly pending until acknowledged. Existing processes and lifetime IDs do not change. |
| End surface | Commit `ending` intent, then asynchronously terminate and verify the owned process tree. Cancel an undispatched spawn; if spawn is in flight, await its result and terminate any resulting owned tree. Do not publish completion while a launch can still produce a process. Commit `exited` only after cleanup is confirmed. Failure retains `ending` with a termination error and retry action, not a false `exited` state. |
| Remove pane/tab/session | Confirm the exact live lifetime set at the expected revision. Persist removal intent and mark its live targets `ending`, retaining all records/tree nodes during cleanup. Reject competing structural changes/restarts within that subtree until removal resolves; unrelated work remains mutable. Once all targets are confirmed stopped, commit deletion and collapse affected splits atomically. With no live targets, delete in one commit. Partial termination failure retains the complete affected structure and reports which targets remain. |
| Runtime result arrives during a commit | Queue/coalesce observations by lifetime; never apply a result to a replacement lifetime. A rapid spawn-and-exit may commit the final state directly without inventing an intervening durable `running` revision. |
| Process changes but its lifecycle write fails | Report actual runtime status as an explicitly non-durable observation, separate from the committed workspace stream. Never show a known-dead process as live or pretend termination was undone. Pause mutations/side-effect dispatch; keep consuming output from surviving work and allow existing input/read access. Resume after persisting reconciled observations. |
| Server crashes with pending launch/removal/termination | On startup, mark potentially live old lifetimes `lost` in a recovery commit before admitting mutations. Mark pending operations interrupted and release their subtree mutation reservations; retain their records and error context, not an automatically resumable removal. Do not launch saved commands or silently finish pending removals. Preserve positions and expose explicit restart or removal actions. Confirm old owned processes are absent through the platform adapter before restart; uncertainty blocks relaunch rather than duplicating work. |

A non-durable lifecycle observation carries its terminal identity key and latest committed revision, is clearly distinguished from a workspace revision, and cannot authorize structural operations. New subscribers receive current observations alongside the committed snapshot so they do not mistake stale metadata for live runtime status. On persistence recovery, fold actual runtime state into ordered durable revisions before reopening mutation admission. No automatic server shutdown is a persistence-recovery strategy: it would destroy otherwise usable live work.

For destructive confirmation and removal, a live target includes a pending spawn or any surviving owned descendant, not merely a launcher whose status is `running`. An exited launcher does not prove its process tree is gone. If cleanup state is unknown, retain the record and require verification/explicit cleanup rather than taking the no-live-target deletion path.

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
- Uses the durable-intent ordering and retained failed-pane policy defined in [Durable workspace mutations](#durable-workspace-mutations).

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

The protocol must not depend on the terminal engine implementation. Convert between engine internals and wire DTOs at the server boundary. Client replication consumes the wire contract, not a server `TerminalState` instance.

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
- Separate ID types distinguish sessions, tabs, panes, surfaces, process lifetimes, durable clients, client connections, and mutations.
- Unsupported peers fail explicitly. Incompatible state is never decoded speculatively.
- Workspace hierarchy changes require a deliberate migration/version boundary from the old session-equals-shell model.
- Control and screen traffic are distinct logical classes. Separate connections are optional, not a requirement.
- A shared connection must bound screen traffic and prioritize input/control without corrupting ordered state.

### Replication

- Attach returns a snapshot with an authoritative sequence baseline.
- Deltas describe changed rows and required cursor, modes, title, graphics, and history metadata.
- Each delta identifies the state sequence it depends on. Coalescing must not turn valid skipped intermediate frames into undetectable corruption.
- A sequence gap requests a fresh snapshot and discards uncertain updates.
- Reconnect verifies the complete terminal identity key and history epoch before restoring selections or cached history, as defined in [Process lifetimes and terminal identity](#process-lifetimes-and-terminal-identity).
- History is fetched in bounded pages and cached locally; first paint does not require transferring all retained history.
- Resize/reflow invalidates or remaps history and selection anchors explicitly.
- Visible panes receive active screen streams. Hidden work receives lightweight status/title updates without unnecessary continuous grid transfer.
- Queue overflow is handled by recoverable snapshot resynchronization, not unbounded memory or silent dropped keystrokes.

The baseline supports one controlling attachment per surface. A client can control all visible panes. A second client requesting an already-controlled surface receives an explicit conflict with a safe detach/open-other-work path. No silent resize fights or takeover. Multi-viewer semantics may extend this later.

## Terminal engine and rendering

### Engine decision

Retain Compi's existing `vte` plus custom `TerminalState` as the initial platform-independent terminal engine, extracted and tested without Windows or GPUI dependencies.

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

### Constrained split layout

The baseline minimum terminal canvas is **20 columns by 4 rows per pane**, excluding pane chrome and dividers. Compute its logical-pixel size from the current measured cell width and line height; include pane chrome, divider thickness, and device-pixel rounding in layout. Font zoom and DPI changes recompute these minima, not the saved tree.

- A leaf's minimum is its canvas minimum plus chrome. For a right split, recursive minimum width is the sum of child widths plus the divider and minimum height is their maximum; a down split uses the inverse. Layout runs on the full tree, not just visible leaves.
- The workspace canvas is at least that recursive minimum and at least the available viewport size on each axis. When it exceeds the viewport, expose horizontal/vertical workspace scrollbars. This scrolls the split layout, independently of terminal history and navigation scrolling. Ordinary terminal wheel events keep their terminal semantics; workspace scrollbars and focus reveal provide access to overflow.
- Allocate each split using its saved ratio, clamped to both children's recursive minima. Clamp only the effective layout ratio. Window resize, font zoom, DPI changes, sidebar collapse, and switching sidebar/strip never persist the clamp or mutate the tree.
- Scrolling the workspace does not resize PTYs. A pane's PTY dimensions follow its full allocated canvas, not its clipped intersection with the viewport. Offscreen panes in the selected tab remain part of its active attachments; clipping must not end processes or masquerade as hiding a tab.
- Focusing a pane scrolls the workspace just enough to reveal it. If a pane exceeds the viewport, reveal its cursor region; keep the outer scroll controls reachable. Preserve focus and permit navigation to every leaf without pane zoom or single-pane presentation.
- Divider dragging and keyboard resizing clamp to the feasible interval. If no movement is possible, disable resizing with an explanation. Preview locally and coalesce PTY resizes; on commit failure/conflict restore the latest committed ratio and corresponding PTY sizes. A tree revision change cancels a stale drag rather than committing against a different layout.
- Enable a new split only when the current workspace has no overflow and the focused pane's allocated rectangle can contain both new minimum-sized children and the divider. Send that measured rectangle with the expected workspace revision; server validation checks finite dimensions and the proposed minima/ratio, while window geometry remains client-owned. A smaller window in another client does not invalidate the shared tree.
- Expanding the window removes overflow when it fits and returns to the saved ratios where feasible. Never automatically collapse/expand the sidebar. Its saved width/collapse remains unchanged by temporary rendering constraints; its expand/collapse control and workspace scroll controls remain reachable at the native window minimum.

### Command registry

One typed registry drives the command palette, keybindings, menus, and enabled/disabled states.

Baseline commands cover:

- Create, switch, rename, and remove session.
- New tab, switch tab, reorder tab, detach tab, restore hidden tab.
- Split right/down, focus pane, resize split, remove pane.
- End surface and restart exited/failed/lost surface.
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
- Workspace tree invariants, revisions, rejection without partial changes, retained failed panes, reordering, removals, and persistence migration.
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

### Acceptance-to-phase map

This map assigns implementation and proof obligations, not passing status. **P1–P6** refer to the migration phases below. **All hosts** means Linux/macOS/Windows headless or pure-core coverage; **both clients** means native macOS and Windows/WSL. A later qualification phase repeats behavior established earlier; it does not excuse missing implementation coverage.

Existing evidence sources are [frame tests](../crates/compi-protocol/src/frame.rs), [control protocol tests](../crates/compi-protocol/src/lib.rs), [captured v7 wire fixtures](../crates/compi-protocol/tests/wire_v7.rs), [engine tests](../crates/compi-terminal/src/lib.rs), [replica boundary tests](../crates/compi-server/tests/terminal_compatibility.rs), [trace replay tests](../crates/compi-terminal/src/trace.rs), [metadata tests](../crates/compi-server/src/session_store.rs), [Windows daemon integration](../crates/compi-server/tests/daemon_integration.rs), [Windows recipes](testcmds.md), and [historical observations](ACCEPTANCE_RESULTS_2026-09-02.md). Neutral CI is configured for Linux/macOS/Windows; Windows runtime integration remains platform-gated and reports missing WSL coverage explicitly. Existing test presence and old reports are not current execution evidence; current exercised coverage is recorded in [Next steps](NEXT_STEPS.md).

#### Automated and architectural acceptance

| ID | Requirement and observable proof | Implement / qualify | Platforms and retained evidence or gap |
|---|---|---|---|
| A1 | Control/screen round trips and old/new v7 byte compatibility; truncated/oversized frames reject before oversized allocation; incompatible versions reject; capabilities negotiate explicitly. | P1 preserves v7; P2 adds negotiation at an explicit version boundary; P3 adds hierarchy protocol. | All hosts. Frame/protocol tests and pre-extraction v7 fixtures exist; native three-host CI results and capability negotiation remain outstanding. |
| A2 | Create nested trees, reject invalid mutations without partial changes, retain launch-failed panes, reorder/remove, conflict at stale revisions, and restore migrated metadata/backup. Inject failure around every commit boundary. | P3; P5 integrated repeat. | All hosts. Old metadata migration/quarantine cases are inputs; workspace actor and durable-receipt coverage are missing. |
| A3 | Replica equals authority after deltas/resize; missing predecessors resnapshot; history pages stay bounded; eviction/reflow invalidate anchors; old generation/lifetime messages cannot corrupt new state. | P1 preserves existing replication; P2 adds paged history and explicit predecessor recovery; P3 adds lifetime restarts; P4 restores client anchors. | All hosts. Mirror/gap/reflow tests exist; page/epoch/lifetime coverage is missing. |
| A4 | Deterministic output/resize replays preserve Unicode, combining/wide cells, attributes/cursor, alternate screen/editing/margins, modes, engine replies, OSC handling, and Kitty transfer/place/delete behavior. | P1 and every engine-affecting change; P5 real applications. | All hosts; both clients for interaction. Engine and trace tests plus Windows graphics/TUI recipes exist; not all required interactions have automated coverage. |
| A5 | Actual PTYs support interactive shells, wait/exit, resize, and terminal replies; no piped-stdio substitute. | P2; P5 repeat. | Real Unix PTYs on macOS/Linux; ConPTY/WSL on qualified Windows runner. Windows integration exists; Unix path/coverage is missing. |
| A6 | Executable/argv/cwd/env preserve quoting, spaces, native shell startup, locale, fresh context, job control, agent/helper access, OSC 7 inheritance, invalid-cwd diagnostics, and verified descendant cleanup. No secret environment values in metadata/logs. | P2; P3 lifecycle actions; P5 interactive repeat. | All hosts. Windows cwd/lifecycle tests and recipes retained; Unix launch/process-group and WSL descendant proof must be exercised explicitly. |
| A7 | Detach, client crash, reconnect, and simultaneous launch preserve the same process without extra shell creation; attachment conflict never steals control or causes resize fights. | P2 simple surfaces; P3 hierarchy; P4 windows/navigation. | All hosts headless; both clients. Existing Windows lifecycle coverage is partial; multi-window and Unix coverage are missing. |
| A8 | Server loss reports truthful lost work; stale endpoint recovery checks ownership; malformed state is quarantined visibly; saved commands never auto-run; restart is explicit. | P2 endpoints/server failure; P3 durable recovery/restart. | All hosts. Windows restart/quarantine tests retained but old `dead` terminology migrates to `lost`; Unix endpoint and workspace recovery coverage is missing. |
| A9 | Pure layout minima/overflow/focus, command enablement/routing, platform shortcuts, slot ownership, state precedence, hidden membership, and fallback navigation have deterministic outcomes. | P1 extracts existing pure client behavior; P4 implements new workspace behavior. | All hosts pure tests; both clients runtime checks. Input/selection/viewport helpers are extracted and tested without GPUI; new workspace layout, slots, and navigation remain unimplemented. |
| A10 | Protocol has no engine/UI/PTY/OS dependency; replicas consume wire DTOs, not engine instances; headless server graph excludes GPUI/installer; native clients use shared core. | P1 dependency extraction; P2 real all-host server. | Five-package workspace and resolved production/build dependency checks are implemented; native Windows build qualification remains outstanding. |
| A11 | Per-user private endpoints/ACLs and peer authorization reject unauthorized access; race-safe instance ownership avoids duplicate servers; development instances isolate state/processes; no network listener. | P2; P5 platform repeat. | All hosts. Existing Windows adapter is input, not proof of Unix security or common-interface authorization. |
| A12 | `compi-probe` uses the normal protocol for inspect, launch, attach, resize, and terminate; traces are opt-in, local, bounded, and replayable without UI. | P1 retains probe/replay; P2 Unix probe; P3 hierarchy commands; P6 agent discovery metadata. | All hosts. Existing probe and trace tests retained; Unix/hierarchy paths are missing. |
| A13 | Core tests run on three OSs; server builds/tests become real on three OSs; client builds on both primary OSs. Missing WSL runtime is reported, never silently counted as a pass. | P1 core CI and graphics-free Windows server; P2 all-host runtime/server and Mac client CI. | Three-host neutral CI and a graphics-free Windows daemon build job are configured; macOS tests and Windows/Linux cross-checks ran locally. Native Linux/Windows test results and qualified WSL execution remain pending. A cfg-disabled Unix daemon is not a working server build. |

#### Daily-use acceptance

The D-number matches the numbered daily-use procedure above. All rows qualify on **both clients in P5**, with platform/build/display/workload attribution.

| ID | Observable workflow | Implementation prerequisite | Existing input / missing coverage |
|---|---|---|---|
| D1 | Source launch opens a real shell without installation; first run seeds once, later launches do not duplicate work. | P2 shell/startup; P3 initialized workspace. | Windows development launch recipe; Mac launch and durable seeding missing. |
| D2 | Create sessions, reorder tabs, and build nested splits without respawning moved work. | P3 hierarchy; P4 UI. | Existing tabs are shell sessions, not hierarchy/split coverage. |
| D3 | Resize window/dividers, manually collapse/expand sidebar, switch sidebar/strip, relaunch; presentation persists and tree remains unchanged by window constraints. | P4 layout/client state. | Windows display recipe; new split/sidebar persistence coverage missing. |
| D4 | Shell editing/job control, Git, `less`, Vim/Neovim, `fzf` preview, and a real agent harness work interactively. | P1 preserved engine; P2 host/input. | Windows shell/TUI recipes retained; both-platform current runs required. |
| D5 | Unicode and exact wrapped selection copy, bracketed paste, mouse/focus/Shift override, validated links, OSC 52 policy, and graphics work; reattach never repeats clipboard/bell effects. | P1 core; P2 native adapters; P4 command focus. | Windows clipboard/graphics/TUI recipes retained; Mac IME/clipboard and snapshot-side-effect coverage missing. |
| D6 | Close/crash/reopen client onto the same live processes and split tree with valid navigation/anchors. | P2 persistence; P3 tree; P4 client state. | Windows detach recipe/integration retained; hierarchy and Unix scenarios missing. |
| D7 | Discover and restore hidden work; end/remove only via explicit confirmation; inspect final state and verify descendants are gone. | P2 cleanup; P3 lifecycle; P4 discovery/actions. | Windows detach/end recipes retained; hidden-slot membership and retained exited panes replace old close-after-end UI expectations. |
| D8 | Flood one pane while typing/navigating another; control/input remains usable and resync recovers bounded overflow. | P2 queues; P4 multiple panes. | Windows sustained-output recipe and soak input; multi-pane/macOS proof missing. |
| D9 | Narrow/minimum, maximized/fullscreen, scaling, mixed displays, native controls, IME/fonts, and offscreen focus remain usable without auto-collapse or pane zoom. | P2 native window/input; P4 constrained layout. | Windows display recipes and dated observations retained; physical checks on both platforms still required. |
| D10 | Isolated mixed-workload soak has no crash, input loss, cross-surface corruption, unexplained growth, or lifecycle leak; failures have bounded reproducible traces. | P1 trace retention; P2 host; P4 full workspace. | Existing Windows soak tooling is input; adapt to hierarchy and add Unix resource measurement. |

#### Appearance, resource, and distribution acceptance

| ID | Requirement and observable proof | Implement / qualify | Platforms and gap |
|---|---|---|---|
| C1 | Versioned TOML validates independent settings; invalid executable never silently launches; profile/env/cwd/font/keybinding/limit/clipboard settings take effect; Open configuration works without rewriting user config. | P2 launch settings; P4 complete config/commands; P5 repeat. | Both clients, all-host launch resolution. New config and precedence coverage required. |
| C2 | Visible Appearance action and palette open one keyboard-accessible picker; preview changes the whole app; cancel restores prior preset; accept persists per slot; focus returns without detaching/resetting terminal work. | P4; P5 repeat. | Both clients. New feature, not covered by existing fixed theme. |
| C3 | Dark Glass is first-run default; acid-green brand remains; canvases stay opaque; chrome-only materials have readable opaque/reduced-transparency fallback; text/font fallback, focus, and lifecycle labels remain legible without color alone. | P4; P5 native/display qualification. | Both clients. New presets/material/fallback coverage required; no new appearance controls beyond the baseline. |
| R1 | During sustained output, input/control and navigation remain usable; paint meets the measured display frame budget. Record input-to-presentation separately from paint CPU and physical frame pacing. | P2 simple view; P4 panes; P5 qualification. | Both clients. Historical Windows timing is diagnostic input only. |
| R2 | Queues, history/pages, image transfers/decodes/caches, and traces remain bounded; render visible rows/graphics, decode off UI thread, and coalesce repaint. | P1 preserve core limits; P2 history/queues; P4 renderer; P5 measure. | All hosts core; both clients resources. Existing bounded-engine/trace tests are partial; new caches require proof. |
| R3 | Repeated create/split/resize/detach/reconnect/end and mixed soak show no leaked processes, threads, handles/FDs, CPU, private/working-set or GPU-memory growth. Measure empty client/server, marginal active/idle/detached surfaces, and pane/history costs separately. | P2 lifecycle; P4 full operations; P5 qualification. | Both clients plus Linux headless cycles. Windows handle-cycle test/tools retained; Unix FD/process and new pane/cache coverage missing. |
| R4 | Attribute cold/warm launch, reconnect, ready-for-input, and memory/latency reports to hardware/build/display/workload; set numeric budgets from those baselines and report missing physical checks. | P2 diagnostic baseline; P5 release budgets. | Both clients. Old Windows numbers cannot serve as Mac budgets. |
| X1 | Source builds need no installer; idle shutdown never abandons live detached work; version mismatch never silently upgrades/kills processes; intentional interruption warns about affected work. | P2 lifecycle/version handling; P3 incompatible metadata upgrade; P6 distribution. | All hosts server; both clients. Existing Windows supervisor/installer behavior is input only. |
| X2 | Runnable Mac/Windows dogfood artifacts precede public-release qualification; packaging, signing/notarization, upgrade, repair/removal, and clean-machine checks use exact attributable artifacts. | P5 dogfood; P6 distribution. | Both clients. Windows tooling retained; Mac distribution and current qualification missing. Distribution-only gaps do not block P1–P4. |

Together A1–A13, D1–D10, C1–C3, R1–R4, and X1–X2 cover the automated list, daily-use list, resource gates, and product definition of done. They do not qualify Linux desktop or any explicitly excluded feature.

### Phase 0 decision scenarios

These are expected outcomes to turn into behavior checks in the assigned phases, not executed tests or proof that the features exist.

| Scenario | Required outcome | Owner |
|---|---|---|
| Restart surface while server survives; old delta/history/image result arrives late. | New lifetime and fresh attachment/snapshot; all old-key work is ignored and old anchors cleared. | P3, A3 |
| Reconnect unchanged lifetime after history eviction or reflow. | Snapshot first; restore only proven anchors, otherwise clear selection and return to live bottom. | P2/P4, A3 |
| Disconnect after a durable split commit but before its reply. | Exact mutation retry returns receipt; one pane and at most one launch attempt. Expired receipt requires reconciliation, not blind resubmission. | P3, A2 |
| Split spawn fails after durable `starting`; initial persistence fails before commit. | First case retains a complete failed pane; second changes nothing and launches nothing. | P3, A2 |
| Replacement succeeds but durability result is uncertain; process exit cannot be persisted. | Mutations/dispatch pause; no false success. Reconcile durability, report real exit non-durably, then commit it before resuming. | P3, A2/A8 |
| Removal kills some descendants but another survives. | Retain affected tree and per-target truthful status with retry; never publish completed deletion. | P3, A2/A6 |
| Server restarts with `starting` or pending removal. | Old potentially live lifetimes become lost; no auto-launch or auto-delete; explicit recovery only. | P3, A8 |
| Two windows open together, hide different tabs, and close/relaunch. | Exclusive distinct slots, independent exclusions, deterministic slot reuse, no extra shells or control takeover. | P4, A7/A9 |
| Selected tab disappears, all tabs are hidden, or a workspace was deliberately emptied. | Deterministic next/previous fallback or empty view; prune only against authoritative state; never reseed. | P3/P4, A9/D1 |
| CLI theme override, canceled preview, failed state write, and reset client layout. | Overrides/previews do not leak into saved preference; failed save is visible; reset is slot-local and non-destructive. | P4, C1/C2 |
| Nested split tree exceeds the window after shrinking/font zoom/DPI change. | Minimum-sized canvas scrolls; all leaves remain reachable, PTYs size to allocated panes, tree/ratios/sidebar collapse are unchanged. | P4, A9/D3/D9 |
| Divider drag races a tree change or its final commit fails. | Cancel stale drag or restore committed layout and PTY sizes; never commit against another tree revision. | P4, A2/A9 |

## Migration and build order

This is a migration of a working terminal, not a blank-slate rewrite. Keep the existing Windows behavior covered while making the same product usable on Mac.

### 1. Extract the neutral foundation

- Separate protocol, terminal engine, and client replication from GPUI and Windows imports.
- Make the server build graph independent of GPUI and installer dependencies.
- Preserve terminal replay and protocol recovery coverage.
- Add Linux/macOS neutral-core CI immediately.

Done when core tests run on all three OS targets and headless server builds do not require graphics tooling.

#### First extraction change

This is a compatibility-preserving dependency change, not the complete Phase 1 or a cross-platform runtime claim.

**Scope**

- Establish the Cargo workspace and three real neutral packages: `compi-protocol`, `compi-terminal`, and `compi-client-core`.
- Extract the pre-migration protocol/framing and terminal screen DTOs/codecs into `compi-protocol`. Their current owners are `crates/compi-protocol/src/{lib,frame,screen}.rs`. Preserve public wire fields, serde names, enum/field ordering, bincode configuration, frame kind values, and protocol version **7**.
- Extract the current `TerminalState`/parser, buffers, terminal semantics, and deterministic replay behavior into `compi-terminal`. Keep wire DTO definitions independent of engine implementation. Convert engine output to wire DTOs in the existing server/session boundary; use ownership transfer or shared immutable value types rather than avoidable whole-grid copies.
- Move `ScreenMirror` and `MirrorApply` into `compi-client-core`, consuming only protocol DTOs. Pure GUI selection/key/viewport helpers are a separate extraction responsibility; these have also been moved into client-core during Phase 1.
- Migrate all affected imports in the server, client, renderer, probe, traces, and tests directly. No old-module re-export shims or duplicate parsers/codecs. Preserve opt-in bounded trace recording and UI-independent replay without introducing a fourth neutral crate.
- Add Linux/macOS/Windows CI jobs that build and run the neutral packages without graphics tooling; retain the Windows application/integration path separately. Server/app packages and isolated installer dependencies are now separated; native platform acceptance is still required.

**Observable acceptance**

1. Before moving code, capture representative v7 control and screen wire fixtures using the current codecs. Both old and extracted decoders consume them with equal values; extracted encoding produces the same bytes, including snapshots, deltas, graphics, and optional control fields. The frame remains little-endian `u32` payload-byte count, then `u8` kind, then payload; the count excludes both header fields. Preserve the existing 16 MiB frame and 1 MiB control limits.
2. Existing terminal/replay/replica behavioral cases run against their new owners on all three OSs. Replaying output and resize yields the same renderable state and terminal replies; a missing delta still requests recovery and resnapshot restores equivalence.
3. Dependency inspection proves the three neutral packages have no GPUI, Windows API, PTY, installer, or window-system dependency, and `compi-protocol` and `compi-client-core` have no engine implementation dependency.
4. Windows binaries and probe still build. On a qualified Windows/WSL host, exercise real create/input/resize/detach/reattach/terminate plus the existing daemon integration regressions. Missing host access is reported as unverified, not a runtime pass or completed extraction gate.
5. Terminal semantics, persistence format, control conflicts, and user-visible Windows behavior remain unchanged. No new Phase 0 identity/revision fields are slipped into v7.

**Excluded:** engine replacement, `portable-pty`, Unix hosting/transport, workspace schema migration/actor, process-lifetime protocol changes, splits, theme picker, and UI redesign. These belong to later named phases.

The pure client helper extraction and server/app/installer dependency separation are now implemented. Phase 1 closes only after its platform qualification gate passes. Phase 2 supplies the real Unix adapters and all-host runtime; a cfg-disabled no-op daemon on Unix never counts as acceptance.

### 2. Prove a real Mac terminal end to end

- Integrate `portable-pty` and host launch descriptions.
- Add Unix local transport and platform paths.
- Enable the same server/client application modules on Mac.
- Implement native Mac window, font, clipboard, key, and input behavior.
- Keep the initial view simple while proving the shared execution path.

Done when a Mac window opens a native shell, handles interactive programs and resize, then closes/reopens onto the same live surface. Windows/WSL still passes its regression path.

**Implementation status (2026-09-06):** the shared Unix server, native Mac GPUI client, launch description, private Unix transport/paths, and Linux runtime CI coverage are implemented. Real Mac verification includes native shell startup, readable rendering, AppKit text input, Vim editing, resize, native close, and ordinary reattachment to the live session. This is not full phase qualification: physical Mac input/display checks and native Linux/Windows regressions remain outstanding. Windows retains the suspended-before-job ConPTY backend pending portable-PTY ownership acceptance. See [Phase 2 evidence and remaining gates](NEXT_STEPS.md#current-phase-2--native-mac-runtime-implemented).

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
- [Next steps](NEXT_STEPS.md): Phase 0 contracts, extraction and native Mac runtime evidence, and remaining platform qualification.
- [Windows terminal test recipes](testcmds.md): existing exercises to adapt to the cross-platform qualification matrix.
- [Historical Windows acceptance results](ACCEPTANCE_RESULTS_2026-09-02.md): dated evidence from the previous implementation, not current baseline qualification.
