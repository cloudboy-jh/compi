# Compi

## Status and authority

This document is Compi's authoritative product and technical baseline. It replaces the previous Windows-only contract and milestone ordering, preserved in Git history. It describes required behavior, not a claim that the behavior is already implemented.

The existing repository is the implementation starting point. Its terminal correctness, persistence, replication, rendering, and lifecycle work should be retained where it satisfies this contract. Windows-specific restrictions are not requirements to preserve.

The [README](../../README.md) distinguishes the current implementation from this baseline. [Next steps](NEXT_STEPS.md) tracks unfinished work; [completed work](COMPLETED.md) records implemented phases and dated verification evidence. Dated acceptance reports and Windows test recipes are historical implementation evidence, not authority for the new scope or proof of cross-platform qualification.

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
- Showing or hiding the optional workspace sidebar leaves the primary top terminal tabs available and does not change or restart the workspace.
- Native window behavior, keyboard conventions, fonts, clipboard, and DPI handling fit the host platform.
- Reattaching does not replay raw output through a second terminal parser or wait for full scrollback transfer before painting.
- Persistence is visible through restored work, not through a dashboard the user must operate.

## Baseline scope

The baseline includes:

- Native macOS and Windows GPUI clients.
- A platform-neutral server that runs without a window system.
- Native Unix process hosting on macOS and Linux.
- WSL2 shell hosting for the Windows product.
- `portable-pty` for native Unix hosting; retain Windows' tested ConPTY ownership adapter until an equivalent library path is proved.
- A platform-independent terminal engine.
- A workspace actor and stable workspace identities.
- Protocol-only types and framing, independent of UI and platform code.
- Client replication isolated from rendering.
- Workspace, sessions, tabs, panes, and surfaces.
- Persisted, nested split trees with draggable dividers.
- Primary top terminal tabs, an optional resizable workspace sidebar, and tab tear-off into native windows.
- Command palette and configurable platform-aware shortcuts.
- Client-local window and presentation state.
- Server-owned terminal and workspace state.
- Shell/profile, working-directory, environment, font, and independently scoped application theme, terminal palette, and background material through TOML, Quick Appearance, and Settings.
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

**Client terminology:** present each named server `Session` as a **Workspace**, with the user-facing hierarchy **Workspace → Terminal tabs → Panes**. The server `Workspace` root is the collection of these groups, not an additional navigation layer. Protocol IDs, persistence records, and technical terminology below retain their existing meanings; this is a UI naming contract, not a domain-model rename.

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
| Sidebar width and font zoom                                         | Client          | Per-client state file                                                              |
| Explicit per-window application/terminal themes, background opacity, and material | Client | Per-client state file |
| Sidebar visibility                                                   | Client          | Current window only; every new/relaunched window starts closed                     |
| Selected session/tab and pane focus                                  | Client          | Per-client state where meaningful                                                  |
| Scroll position and selection                                        | Client          | Keyed by server identity/generation, surface ID, and process lifetime; restored only when anchors remain valid |
| User configuration and global appearance defaults                    | User-owned file | Read on launch; explicit global appearance, favorites, and interface/terminal font changes update their relevant TOML keys atomically |

The split tree belongs to the workspace. Top terminal tabs are the primary navigation; the optional workspace sidebar supplements them. Sidebar presentation and window placement belong to the client.

Global TOML supplies appearance defaults. Explicit per-window appearance overrides in private client JSON win over those defaults; an explicit CLI theme override wins for that invocation and is never made durable implicitly. Remembered window geometry, sidebar width, font zoom, and navigation remain client-owned. Direct **Global defaults** changes preserve comments and unrelated TOML keys; **This window** changes never rewrite TOML. Resetting window appearance clears its overrides and resumes inheritance.

### Durable client identity and navigation

A durable `ClientId` identifies one remembered window-state slot, scoped to the local user and server instance. It is not a PID, connection ID, attachment, or authority to take control of a surface. A new connection gets a fresh `ConnectionId`; reconnecting does not change the window's `ClientId`.

- An instance has one primary slot. Ordinary launch claims it when free; otherwise it claims the oldest unclaimed auxiliary slot, ordered by creation ordinal, or creates an auxiliary slot from configured defaults. **New window** uses the same allocation rule. Opening one window does not automatically reopen every remembered window.
- Allocation is serialized through a local registry lock. Each active window holds an OS-released exclusive slot lock until exit; a crash releases ownership without deleting remembered state. No two windows write the same state file. Lock or storage errors are surfaced, not bypassed with an unlocked writer.
- Slots use versioned atomic state files. Closing or crashing a window preserves its slot; relaunch reuses the deterministic available slot. Malformed state is quarantined with a visible recovery message and configured defaults, without changing server work. A failed save leaves the live presentation usable but explicitly unsaved.
- Each slot remembers window geometry, sidebar width, font zoom, explicit appearance overrides, selected session, last selected tab per session, last focused pane per tab, a set of hidden tab IDs, and its floating panes (placement, stacking order, and whether a float owns the keyboard). Sidebar visibility is transient and starts closed for every new or relaunched window, regardless of prior state or initial-layout configuration. Navigation and hidden membership are scoped by stable server/workspace identity, not server generation, so they survive a server restart.
- Hidden membership is an exclusion set: tabs not hidden in this slot are listed in server order, including newly created tabs from another window. Hiding releases this window's attachments to that tab, changes no server structure, and does not hide it in other windows. Window close releases attachments but does not add tabs to the hidden set.
- Restoring a hidden tab removes its exclusion, selects its session/tab, and attempts attachment. The workspace list and palette include hidden tabs with their actual lifecycle state. A control conflict leaves an explicit unavailable pane with retry/open-other-work actions; it never steals control, unhides other tabs, or creates a replacement shell.
- On an authoritative workspace snapshot, prune references to removed objects. Keep hidden membership and navigation during temporary disconnection. If the selected tab is removed or hidden, choose the next non-hidden tab in server order, then the previous one; if none exists, show an empty session view with create/restore actions. If a session disappears, choose its next surviving session, then the previous one; at initial restore with no valid remembered position, choose the first session and first non-hidden tab.
- Remembered focus is restored only within the selected tab; an invalid pane reference falls back to its first leaf in tree traversal order. Merely switching sessions/tabs restores their last valid navigation and releases screen/control attachments from the previous visible tab.
- If every tab is hidden, or the workspace is intentionally emptied, show the empty view. Never seed another shell to repair navigation. First-run seeding occurs exactly once for a new, uninitialized workspace and is committed durably with its initialized marker.
- CLI overrides affect only that invocation and are excluded from state saving. Reset client layout clears the slot's remembered geometry, presentation overrides, navigation, and hidden set to configured defaults; it neither deletes work nor changes another slot. Resetting window appearance clears only its appearance overrides.
- Tab tear-off uses the same exclusive slot allocation, but explicitly initializes the destination view to the transferred tab rather than restoring unrelated navigation. Other existing tabs start hidden in that destination slot and remain discoverable. Seed the resolved theme, opacity, background effect, zoom, and sidebar width from the source without durably recording CLI overrides; the new sidebar starts closed. An existing destination keeps its own presentation, including current sidebar visibility. Successful transfer hides the tab in the source slot and reveals/selects it in the destination slot; unrelated windows and the tab's session membership are unchanged.

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

**Current implementation distinction:** Protocol 18 has no history-page request/reply or history-epoch/logical-line anchor fields. It sends bounded scrollback in snapshots and deltas; client restoration checks identity, snapshot sequence, dimensions, and content fingerprint and discards uncertain viewport state. The stronger paging and anchor contract above remains a requirement, not a claim of shipped behavior.

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

Window-close controls always mean detach. Ctrl-W and the explicit hide command detach a tab while preserving its work. A tab close control requests confirmed tab removal and process termination; destructive removal stays explicitly named in its confirmation. **End surface** is explicitly named in every command surface and acts in one click, because the pane and its final grid remain.

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
| `compi-protocol` | Shared process contract: IDs, wire DTOs, codecs, endpoint identity, local transport, paths, and diagnostics. No PTY or UI dependency. |
| `compi-daemon` | Workspace actor, terminal engine, surface runtimes, persistence, process supervision, and host adapters; builds `compi-daemon`. |
| `compi-client` | Daemon connection, replica recovery, interaction logic, diagnostic probe, and GPUI application; builds `compi` and `compi-probe`. |

The protocol must not depend on the terminal engine implementation. Convert between engine internals and wire DTOs at the daemon boundary. Client replication consumes the wire contract, not a daemon `TerminalState` instance.

Keep PTY, terminal, transport, configuration, and platform concerns as focused modules inside their owning product crate. Extract another crate only for an independently consumed contract or shipped artifact. Installer code remains isolated from the product workspace.

## Platforms and process hosting

### Supported arrangements

| Environment | Baseline arrangement |
|---|---|
| macOS | Native GPUI client, native local server, Unix PTY, user's native shell. |
| Windows | Native GPUI client, native local server, pinned ConPTY runtime with the tested process-ownership adapter, WSL2 shell launch adapter. |
| Linux | Headless native server and native Unix PTYs, exercised in CI. Desktop client qualification follows. |

The current Windows product uses a native server and WSL2 shell adapter. This is a deployment choice, not a domain constraint; moving the server into WSL would be a separate decision, not a prerequisite for Mac support.

### PTY integration

Use `portable-pty` behind a narrow integration layer for native Unix spawn, read, write, resize, wait, and termination coordination. Retain the tested Windows ConPTY adapter unless a replacement proves equivalent process ownership and terminal behavior.

- Use a real PTY. Piped stdin/stdout is not a Unix implementation.
- Keep platform-specific process-group and descendant cleanup where required.
- Windows retains its suspended-before-job ConPTY backend with the pinned runtime: process-tree ownership and Kitty APC output must survive any `portable-pty` replacement. An alternative backend must implement the same host interface and pass the same behavior tests.
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
- Preserve shell startup files, job control, SSH agent access, credential helpers, locale, and normal environment behavior. The only startup-file edit Compi makes is the reviewed login-shell prompt block described under [shell prompt settings](#shell-prompt-settings).
- Terminal identity and required terminal variables are set deliberately and consistently with supported capabilities.
- Do not reorder PATH or inject startup commands to simulate correct cwd handling.
- On Windows, the default WSL shell replaces the Windows directories WSL appends to `PATH` (those on its drive mounts, present before startup files run) with one Compi-maintained directory of launchers for the same commands, in the same precedence. Bash otherwise searches every Windows directory through the drive bridge for each missing command and command completion. Windows programs still receive the Windows `PATH`; directories added by startup files are untouched; with `appendWindowsPath` disabled nothing changes.
- Long-lived servers receive a validated fresh launch context for new surfaces; they must not blindly reuse stale client-sensitive environment values.
- Logs and persisted state must not expose environment secrets.

### Working directories

- New surfaces inherit a valid current directory from the focused surface when available, otherwise the profile's starting directory or home.
- OSC 7 supplies cwd updates, with host/path validation and a last-known fallback.
- Resolve Windows paths to WSL paths at the WSL host boundary, using the selected distribution's `wslpath` behavior.
- `/mnt/c/...` remains a supported working location. No copy bridge, automatic repository relocation, or forced migration into a Linux filesystem.
- Mac paths remain Mac paths. Linux paths remain Linux paths.
- Invalid directories produce an actionable error or an explicitly accepted fallback, not a silent different project location.

### Shell-aware file navigation

The file browser replaces the focused terminal canvas, not the workspace sidebar. Directory listing and bounded descendant search run in the daemon using the surface's Unix/WSL path namespace; results carry folder/file distinctions and absolute shell paths. Search skips inaccessible descendants while reporting an inaccessible root. UI search is asynchronous and discards stale replies. Double-click or keyboard navigation expands folders; selection can be copied as a path.

Default interactive Bash/Zsh launches load a bundled bridge from private shell-home files without editing user startup files. The bridge emits OSC 7 after startup and cwd changes, and `compi tree` / `compi z` request the in-pane browser/project picker through a one-shot OSC action. Only a shell-origin request accepts a base64-encoded selected directory on its waiting TTY and invokes `builtin cd --` in that same shell; cancel sends an empty reply. Keyboard/palette browsing does not inject input into a busy terminal. Project history is bounded client state derived from reported directories; tabs and splits inherit the focused pane's latest valid cwd.

### Shell prompt settings

The **Shell prompt** group at the bottom of Settings → Terminal configures the user's own prompt provider, Oh My Posh or Starship, for Bash and Zsh. Compi has no prompt of its own, never installs a provider, and uses each style exactly as the provider defines it. All work runs in the daemon's shell environment: the local Unix account, the selected WSL2 distribution, or the remote account behind SSH.

- **Detection** happens in the background; the main view shows only the controls, progress, and errors. Details list the target, the login shell, provider paths, startup-file lines that initialize a provider, install hints, and warnings, including a Windows-only Oh My Posh that WSL shells cannot use. Styles are the user's current configuration, the built-in default, Oh My Posh themes from its cache, and Starship presets. Fish, PowerShell, and custom launch profiles are unsupported.
- **Picker** lists every style with its real rendered prompt after a quick success. The daemon renders up to 32 styles per request and the client fetches the list in batches. Previewing changes nothing; prompt, terminal theme, and layout stay independent.
- **Scope.** Applying always covers *Compi shells*: `~/.compi/prompt/compi.{bash,zsh}`, which the bundled bridge loads after the user's startup files; user files are untouched. *Also use outside Compi* additionally writes `~/.compi/prompt/normal.<shell>` and a marked `# >>> compi prompt >>>` block at the end of the login shell's `~/.bashrc` or `~/.zshrc`, rewritten in place so links and permissions survive; applying without it removes that block. Unrelated lines are never changed. Turn off removes every managed prompt.
- **Application.** New Compi shells load the applied prompt, and running Compi shells reload at their next prompt through a token file read by the bridge's prompt hook; Compi never types into a terminal. Shells outside Compi need a new shell.
- **Plan, confirm, back up.** Every apply, turn-off, and restore is planned first, in the background, when its dropdown opens under the clicked style, **Turn off**, or the history row; the dropdown holds the confirm and Cancel, and the confirm waits for the plan. One dropdown is open at a time; Escape or Cancel closes it and writes nothing. Re-applying the current style and scope opens none. Only a change to the user's own startup file is shown, as a line diff. The plan carries a token over the files' current contents; execution with a stale token is rejected. Before writing, Compi backs up every managed file to `~/.compi/prompt/backups/<id>/` (newest 20 kept). Restoring returns Compi's files to that moment; in a user's startup file only Compi's block is restored, so later unrelated edits survive.
- **Live reload safety.** The bridge snapshots the prompt the user's startup files produced (PS1/PS0/PS2/RPS1, `PROMPT_COMMAND` or precmd/preexec hooks, the DEBUG trap, provider session variables, and ble.sh prompt options and hooks) before loading a managed prompt, restores that snapshot before each reload, and keeps its own hook last so providers read the real exit status. Under ble.sh the prompt loads from ble.sh's attach and pre-command hooks because ble.sh owns PS1 and `PROMPT_COMMAND` once attached.

## Local transport and protocol

### Transport

Expose a small stream boundary with three transports:

- Unix-domain sockets on macOS/Linux.
- Current-user-restricted named pipes on native Windows.
- An SSH stdio relay started as `ssh -T ... compi-daemon --server-stdio`; authentication, host-key policy, and tunneling remain OpenSSH responsibilities.

An existing local-socket library may provide mechanical plumbing, but security and lifecycle behavior must be verified on each OS. A common API does not establish authorization by itself.

- Restrict local endpoints to the current user through permissions/ACLs and applicable platform peer checks.
- Store local endpoints in private, platform-appropriate locations.
- Enforce one server per user and instance with race-safe ownership.
- Recover stale endpoints only after confirming the previous owner is absent.
- Remote targets use `[user@]host[:port]`, batch mode, a bounded connection timeout, and the remote host's own instance namespace and private storage.
- Baseline operation opens no Compi network listener, stores no SSH credentials, and never treats a remote relay as a local peer.
- TCP listeners, automatic remote discovery, cloud relays, and cross-machine workspace synchronization remain separate scope. Loopback is not treated as inherently authenticated.

### Wire contract

Retain typed length-prefixed framing where practical:

```text
u32 payload length | u8 message type | payload
```

The frame uses a little-endian `u32` payload-byte count excluding the five-byte header. The protocol crate enforces 1 MiB for control frames, 128 MiB for screen frames, and 16 MiB for other frame kinds; oversized frames are rejected before allocating their payload.

Use readable JSON for low-rate control messages and compact binary screen payloads. Preserve useful existing binary work; do not regress to JSON grids or introduce a second encoding stack merely to mirror another project.

- Hello negotiates protocol version, capabilities, server identity, and server generation.
- Request IDs correlate replies; workspace revisions order structural updates.
- Separate ID types distinguish sessions, tabs, panes, surfaces, process lifetimes, durable clients, client connections, and mutations.
- Unsupported peers fail explicitly. Incompatible state is never decoded speculatively.
- Workspace hierarchy changes used a deliberate migration/version boundary from the old session-equals-shell model.
- Control and screen traffic are distinct logical classes. Separate connections are optional, not a requirement.
- A shared connection must bound screen traffic and prioritize input/control without corrupting ordered state.

**Current implementation distinction:** Protocol 18 `Hello` exchanges and checks the exact protocol version only. Server identity and generation are supplied by workspace/terminal messages, not negotiated in `Hello`; capability negotiation remains a requirement rather than a shipped handshake field.

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

**Current implementation distinction:** Screen snapshots/deltas currently include bounded scrollback directly and expose no on-demand history-page RPC or history epoch. The page and anchor rules above specify the intended stronger replication contract.

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
- OSC 7 cwd, OSC 8 hyperlinks, validated Ctrl/Cmd-click web-link detection across wrapped logical lines, and bounded write-only OSC 52 handling.
- Kitty graphics with bounded transfers, decoding, placements, deletion, clipping, resize, and reattachment behavior.
- Required device-status and color replies generated by the server-side engine.
- Categorized, rate-limited diagnostics for unsupported or rejected sequences.

Escape sequences are untrusted process output. Hyperlink opening validates schemes and requires user interaction. OSC 52 obeys user policy, never exposes clipboard reads, and is routed only to an appropriate controlling client. Replayed snapshots must not replay clipboard or bell side effects.

### Graphics retention and transport

- Each surface admits at most 64 MiB of retained base64 image data and pending-transfer reservations. Individual decoded images and each visible pane's decoded cache are bounded to 64 MiB. A raw 3840×2160 RGBA image is 31.64 MiB and occupies 42.19 MiB as retained base64.
- Capacity pressure may reclaim unreferenced images, never pixels referenced by retained placements. Rejected transfers return Kitty errors without replacing previously committed pixels or placements. Immutable payloads are shared between snapshots; placement-only updates do not retransmit image pixels.
- Decoded images are requested only for visible placements. Offscreen cache entries and their native sprite-atlas textures are released independently of authoritative image retention. Saturated decode queues retry without requiring reconnection.
- Protocol 18 retains the bounded screen frames (up to 128 MiB), 1 MiB control-frame limit, on-demand runtime metrics, and protocol 12 remote image uploads. Uploads have a 64 MiB per-image limit, 256 KiB chunks, at most four active uploads per connection, exact offsets, SHA-256 verification, and disconnect cleanup. Uploaded images enter a private, content-addressed store capped at 512 MiB and 4096 files. Writer budgets include in-flight data; queued recovery and client pending-screen backlogs remain bounded. Polling drains at most 1 MiB of available data per call.
- Main-screen image anchors follow scrolling into retained history and logical-line text through width-changing reflow; expired history anchors are removed. Alternate-screen placements retain grid-relative resize behavior. Reflow preserves image payloads and placement extents.
- These are per-surface, per-cache and per-connection bounds, not a daemon-wide memory ceiling. Many retained surfaces, transfer buffers, frame serialization and GPU allocations require separate resource qualification.

### Graphics protocol coverage

- Required Kitty support is APC `G` direct transmission for raw RGB (`f=24`), raw RGBA (`f=32`), and PNG (`f=100`), including chunking, zlib compression, transmit/place/query/delete actions, image and placement IDs, row/column extents, z-order, clipping, scrolling, history, reflow, and reattachment.
- Kitty file, temporary-file, and shared-memory transmission; animation; Unicode placeholders; source rectangles; pixel offsets; virtual placements; and composition are outside the baseline. Unknown controls and actions are rejected with a Kitty `ENOTSUP` response and a rate-limited diagnostic rather than accepted silently.
- Sixel rendering is deferred because current qualification provides no product requirement for it. Sixel DCS payloads are consumed without rendering and recorded as an unsupported `DCS:q` diagnostic. Adding sixel requires bounded decoding, retention, reflow, rendering, and reconnect coverage equivalent to the required Kitty path.


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

- Workspace browsing exposes create, switch, rename, and remove actions for server sessions through the optional workspace sidebar and commands, labeled as workspaces in the UI. Ordinary terminal use does not require opening the sidebar or operating a large workspace selector first; no separate session navigation layer is shown.
- The active workspace's non-hidden terminal tabs appear in the primary top tab bar. Each tab selects one terminal or a complete split layout, not an entire workspace. Switching workspaces restores the last selected tab and pane.
- Detached/hidden tabs remain available through the workspace list and palette, with clear running/exited/lost state.
- A terminal tab shows its single pane's useful title or concise cwd basename, two independently truncated names when split into two panes, and the first name plus a total `N+` count for three or more. A user label takes precedence but retains a pane-count badge for split tabs. Never expose a full machine path as the naked fallback label; inactive panes use their last observed titles where available.
- Hover over a top tab to read all pane names and reported directories in a compact card: a single pane shows only its name and directory, while split tabs show a count and numbered panes; a custom tab label appears when present. Right-clicking a tab opens its menu without selecting it; the menu lists panes in layout order, with compact command rows and a bounded scroll area. Its tab command rows act on the clicked tab and select it first. Selecting a pane reveals a small bubble beneath its name inside that same list, with quiet **Detach**, **Float** (**Dock** when that pane already floats), and **End** controls and a visible keyboard focus state without redundant selection outlines. Floating panes are marked in the list and hover card. Detach is disabled for an unsplit tab and preserves the shell when moving it to a new tab; Float applies to the selected pane, including a lone pane or one in an unselected tab, and gives it the keyboard; End terminates its process tree in one click and retains the final grid, dimmed under one veil in the terminal background color. Arrow keys select a pane; Enter opens its bubble, where Tab switches between enabled actions and Escape returns to the compact list. Menus, the palette, and bubble actions bind the clicked tab and pane plus the workspace structure: server identity, hierarchy, labels, pane leaves, and process lifetimes. Changes to that structure require reopening the menu. Size, status, and split-ratio observations advance the server revision but never invalidate an open menu. Destructive confirmations still bind the exact revision. The command palette's detach action still targets the focused pane.
- Split and sidebar seams are one device pixel, drawn in the theme's border color blended 40% toward the terminal background, so they stay crisp and quiet at every display scale. Each seam has a wider transparent drag zone over adjacent padding, never terminal text, and turns the accent color on hover and while dragging. A viewport that covers the layout minima is used exactly, so sub-pixel rounding never creates workspace scrollbars.
- Tabs can be reordered without restarting processes.
- The visible tab renders its full split tree; focus is visibly identifiable without a thick decorative frame.
- Empty, disconnected, exited, failed, ending, and lost states have explicit recovery actions.

### Primary tabs and optional sidebar

- Ghostty-like top terminal tabs are first-class navigation. The secondary Superterminal-style sidebar offers full workspace management with expandable workspaces and their terminal tabs, lifecycle status, organization, and hidden-work discovery/restoration. It supplements the tab bar rather than replacing it; its actions are also reachable through commands.
- The sidebar starts closed in every new or relaunched window, including tear-off windows. Open visibility is not persisted or inherited. It opens only through deliberate user action, never because of reconnect, workspace switching, or tab creation/transfer. Opening it leaves the top tab bar present.
- Sidebar width is draggable, clamped, and remembered.
- Double-click reset restores the configured default width.
- Top strip uses scrolling/overflow rather than compressing every label into an unreadable sliver.
- The established Compi terminal mark remains visible beside a distinct, quiet sidebar toggle. The keyboard shortcut and palette command also show/hide the sidebar. When hidden, no permanent sidebar rail, large workspace selector, or reserved width remains. Its current visibility survives ordinary interaction within the window, but not window relaunch.
- The header keeps New terminal fixed beside the scrolling tab strip, followed by focused-pane actions for Split right, Split down, and Zoom pane. These actions use the command registry's current enablement, disabled reason, and configured shortcut. Remove pane remains in the palette, pane context menu, and shortcut only.
- When header width cannot preserve a usable active tab, the three pane actions collapse into one compact menu before they can collide with native window controls. The compact menu exposes the same commands and states.
- Pane zoom is explicit, window-local presentation state scoped per tab. It renders the focused pane across the content area without changing the authoritative split tree or disconnecting other surfaces. Directional focus retargets the presented pane; splitting restores the full layout before using the existing split mutation; stale or removed pane targets clear zoom safely.
- Floating is explicit, window-local presentation of an existing pane above the content area. It reuses the pane's single attached view, never a duplicate surface or second attachment, and stays visible while other tabs or workspaces are selected. The authoritative split tree is unchanged; in this window the floated leaf leaves its tab's layout so siblings fill its space, and divider mutations still address the saved tree. Other windows keep their own presentation. A float's PTY follows its frame; frames move and resize within the content area, never below one 20-column by 4-row canvas plus title, and are remembered per window slot as fractions of that area.
- Docking is presentation only: the float record is removed and the pane reappears wherever the server tree now places it, so moves by other clients are followed rather than undone. Closing content stays the existing Remove pane/tab confirmation, and terminating stays **End terminal**. A float ends when its pane is removed or its tab is hidden or moved out of this window, releasing that attachment. A tab whose every pane floats keeps its place with a Dock action.
- Exactly one view owns the keyboard: the front float when it has focus, otherwise the selected tab's tiled focus. The owning float is outlined and labelled **Keyboard**. Clicking a float raises and focuses it; **Switch focus between floating and tiled panes** and **Focus next floating pane** move ownership without the pointer. Split, zoom, and divider commands are disabled for a floating pane with a Dock reason. Browser, editor, note, diff, and media panes reuse this presentation as those views ship.
- Pane arrangements change the authoritative split tree, like splitting and divider moves, so every window that later shows the tab sees the result; floating and zoom stay window-local. **Arrange panes and tabs** opens a picker bound to the selected tab: a **Tabs to combine** checklist of the workspace's other tabs visible in this window (tabs hidden here, including those owned by another window, are never offered), the current arrangement (tabs side by side when combining), six built-ins (Columns, Rows, Grid, Main + stack side/top, Equalize), and named presets from configuration. A click or Enter on a tab toggles it. Clicking or arrowing to a layout only previews it; double-click, Enter, or **Apply** commits it, and **Cancel**, Esc, or clicking outside changes nothing. Apply is hidden when the selection would not change anything. A wireframe previews each numbered pane's destination across every combined tab without resizing any PTY and warns when panes would fall below the 20-column by 4-row minimum; M mirrors, F flips, and Delete twice removes the selected named preset. Panes fill slots in reading order, the selected tab's panes first and then each combined tab's in tab order; Main + stack places the focused pane in the main slot. Named presets with fewer slots drop trailing slots; extra panes stack evenly in the last slot, perpendicular to its parent split. **Mirror/Flip pane arrangement**, **Swap pane left/right/up/down** (the same neighbor as directional focus), **Restore previous pane arrangement**, and **Split merged tabs back out** act directly. Applying clears zoom for that tab. Floating panes keep floating and dock into their assigned slot.
- Every arrangement is one `ArrangeTab` mutation whose tree must hold exactly the tab's current pane/surface leaves; the daemon rejects missing, duplicate, foreign, or re-paired leaves and invalid ratios, launches and ends nothing, and keeps the replaced tree as the tab's `previous_layout`. Removing or detaching a pane also removes it from that tree, which is dropped once it no longer contains a split. Restore fits the previous tree to the current panes: surviving panes return to their positions and panes added since then join the last slot; restoring is itself an arrangement, so repeating it swaps back.
- Combining tabs is one `MergeTabs` mutation: the sources must be distinct other tabs of the receiving tab's session, and the tree must hold exactly all of their leaves. The emptied tabs are removed, nothing is launched or ended, and the receiving tab keeps a merge record: its own pre-merge tree plus each merged tab's ID, label, tree, and session index. Merging a merged tab flattens its record so one split restores every original tab. `SplitMergedTabs` recreates those tabs with the same IDs, labels, splits, and approximate order and moves their panes out; the receiving tab returns to its own original tree when its pane set is unchanged, and otherwise keeps its current tree minus the moved panes, so panes added after the merge stay. Remove/detach drop panes from the record. A merge clears `previous_layout`, so **Restore previous pane arrangement** right after merging splits the tabs back out; after a later arrangement it undoes that arrangement, and **Split merged tabs back out** remains available.
- Navigation areas scroll independently from the terminal.
- Window controls reserve platform-appropriate space, including Mac traffic lights.
- The layout must not depend on a single fixed window size or Windows-only titlebar geometry.
- Narrowing a window does not automatically collapse the sidebar, zoom a pane, or switch to a single-pane presentation. Presentation changes do not rewrite the persisted split tree.

### Tab tear-off and window transfer

- Dragging within the tab bar reorders tabs. Releasing a tab outside its window creates a new native window containing that same tab and its full split layout. Dropping onto an existing Compi window for the same server instance transfers it there; this is not a cross-server or cross-session move.
- Use a lightweight drag preview and clear insertion targets. Until a drop is accepted, keep the original view attached. Escape or drag cancellation leaves navigation, attachments, and server structure unchanged.
- Transfer the existing tab, panes, surfaces, and process lifetimes, not copies or replacement shells. Preserve pane focus and only valid viewport/selection anchors. Destination geometry may resize PTYs but must not change saved split ratios.
- Coordinate an explicit attachment handoff so source and destination never concurrently send input or resize the transferred surfaces. Do not steal control from an unrelated window. If window creation or attachment fails, keep or restore the source view and report the actual failure; partial handoffs must reconcile every pane without falsely reporting success.
- Reconcile the two slots' hidden membership and navigation only with a truthful transfer outcome. Client-state save failures remain visible; the server-owned tab must remain discoverable even after either window crashes.
- Moving the last tab out leaves an empty source view with create/restore actions. It does not close the source window or create another shell automatically. Closing the destination detaches normally and preserves the moved work for relaunch.
- A new tear-off window inherits source theme, font zoom, and sidebar width under the durable slot rules, but its sidebar starts closed. An existing destination keeps its own theme, font zoom, sidebar width, and current visibility. Workspace membership (server session) remains unchanged even when the destination was previously viewing another workspace.

### Constrained split layout

The baseline minimum terminal canvas is **20 columns by 4 rows per pane**, excluding pane chrome and dividers. Compute its logical-pixel size from the current measured cell width and line height; include pane chrome, divider thickness, and device-pixel rounding in layout. Font zoom and DPI changes recompute these minima, not the saved tree.

- A leaf's minimum is its canvas minimum plus chrome. For a right split, recursive minimum width is the sum of child widths plus the divider and minimum height is their maximum; a down split uses the inverse. Layout runs on the full tree, not just visible leaves.
- The workspace canvas is at least that recursive minimum and at least the available viewport size on each axis. When it exceeds the viewport, expose horizontal/vertical workspace scrollbars. This scrolls the split layout, independently of terminal history and navigation scrolling. Ordinary terminal wheel events keep their terminal semantics; workspace scrollbars and focus reveal provide access to overflow.
- Allocate each split using its saved ratio, clamped to both children's recursive minima. Clamp only the effective layout ratio. Window resize, font zoom, DPI changes, and sidebar visibility/width changes never persist the clamp or mutate the tree.
- Scrolling the workspace does not resize PTYs. A pane's PTY dimensions follow its full allocated canvas, not its clipped intersection with the viewport. Offscreen panes in the selected tab remain part of its active attachments; clipping must not end processes or masquerade as hiding a tab.
- Focusing a pane scrolls the workspace just enough to reveal it. If a pane exceeds the viewport, reveal its cursor region; keep the outer scroll controls reachable. Preserve focus and permit navigation to every leaf without pane zoom or single-pane presentation.
- Divider dragging and keyboard resizing clamp to the feasible interval. If no movement is possible, disable resizing with an explanation. Preview locally and coalesce PTY resizes; on commit failure/conflict restore the latest committed ratio and corresponding PTY sizes. A tree revision change cancels a stale drag rather than committing against a different layout.
- Enable a new split only when the current workspace has no overflow and the focused pane's allocated rectangle can contain both new minimum-sized children and the divider. Send that measured rectangle with the expected workspace revision; server validation checks finite dimensions and the proposed minima/ratio, while window geometry remains client-owned. A smaller window in another client does not invalidate the shared tree.
- Expanding the window removes overflow when it fits and returns to the saved ratios where feasible. Never automatically collapse/expand the sidebar. Its saved width and current visibility remain unchanged by temporary rendering constraints; its show/hide control and workspace scroll controls remain reachable at the native window minimum.

### Command registry

One typed registry drives the command palette, keybindings, menus, and enabled/disabled states.

Baseline commands cover:

- Create, switch, rename, and remove workspace (operating on server sessions).
- New tab, switch tab, reorder tab, detach tab, restore hidden tab, new window, and move tab to a new window.
- Split right/down, focus pane, resize split, remove pane, zoom pane, float/dock pane, move keyboard focus between floating and tiled panes, arrange/mirror/flip/restore pane arrangements, combine tabs into one arrangement and split them back out, save an arrangement as a preset, and swap panes.
- End surface and restart exited/failed/lost surface.
- Show/hide workspace sidebar and reset sidebar width.
- Copy, paste, select all where appropriate, and clear scrollback.
- Font zoom in/out/reset.
- Open compact Quick Appearance and responsive two-pane Settings for appearance, interface, terminal, keyboard, performance, and advanced controls.
- Open the TOML configuration file, reset client layout, rebuild client rendering caches without touching terminal processes, reconnect the current window, safely restart the daemon, and open diagnostics.
- Quit client, explicitly separate from stopping the server.

The palette supports query filtering, keyboard navigation, Enter to execute, Escape to dismiss, visible shortcuts, and explanations for disabled actions. It is not a terminal mode.

### Keyboard and input policy

- macOS follows native Command/Option application and editing conventions while preserving terminal Control input.
- Windows follows native Windows Terminal tab, pane, text-editing, and zoom conventions. Ctrl-T/W create and hide tabs; Ctrl-Tab / Ctrl-Shift-Tab cycle them; Ctrl-Shift-T restores a hidden tab. Ctrl-Plus/Minus change font zoom, Ctrl-0 resets it, and Ctrl-Comma opens Settings. Alt-Shift-Plus/Minus split right/down; Alt-Arrow moves pane focus; Alt-Shift-Arrow resizes; Ctrl-Shift-W removes the focused pane with confirmation. Ctrl-V and Shift-Insert paste; Ctrl-Backspace deletes the previous word; Ctrl-Insert and selection-aware Ctrl-C copy. With no selection, Ctrl-C forwards terminal interrupt. Ctrl-Shift-C/V remain explicit compatibility bindings.
- Platform-specific behavior is centralized, configurable where it represents an application command, and tested rather than scattered across widget handlers.
- Palette/menu focus owns its navigation keys while open; terminal focus resumes on dismissal.
- Bracketed paste is honored. Pasting never implicitly executes an extra newline beyond the clipboard contents.
- IME and composed text follow native input paths.

### Image input and inspection

- Image paste and file drop are input operations, separate from applications emitting Kitty graphics. PNG, JPEG, WebP, GIF first-frame and BMP input is validated and decoded on bounded background workers.
- Windows/WSL resolves readable local file paths through the selected distribution; macOS uses native paths. For an SSH target, the client uploads the original bytes through the bounded daemon protocol and receives an absolute remote POSIX path. The terminal receives one properly quoted path, with no automatic execution/newline. Prepared input is rejected if its target surface, process lifetime or server generation changed.
- Local clipboard originals are stored privately under `clipboard-images` using content-addressed names. The local and remote managed stores each admit at most 512 MiB/4096 files; they do not delete files when a preview or window closes. Standard Windows DIB/DIBV5 bitmap clipboard formats are supported alongside encoded image formats.
- A compact, dismissible thumbnail shows filename, dimensions and size outside the terminal grid. Inspection loads the full image on demand and supports fit, zoom, pan, copy and a native Save dialog. Dismissing a preview never edits the shell line or deletes its source file.
- Input files and remote uploads are bounded to 64 MiB encoded data, 8192 pixels per side and the decoded-memory limit. Application-specific attachment protocols remain outside the baseline.


## Configuration and appearance

Use a versioned TOML configuration with documented defaults. An **Open configuration file** command, compact **Quick Appearance**, and a comprehensive in-window **Settings** panel are baseline. Settings edits appearance and bundled interface/terminal font choices directly, and shell prompt settings in the target environment (stored there under `~/.compi/prompt`, not in TOML); launch profiles, limits, custom font families, and advanced keybindings remain inspectable/editable through TOML.

Settings uses a persistent section rail at normal widths and wrapped section controls in narrow windows. Overlay focus stays trapped; Settings and form dialogs use Tab/Shift-Tab for actionable controls, list dialogs use arrow navigation, Escape cancels or returns from nested theme browsing, and destructive confirmations default to Cancel. Modal text and focus treatments meet WCAG AA contrast against every bundled application surface.

Configuration includes profiles, shell/login behavior, starting directory, environment overrides, fonts, font size, line height, theme and optional terminal-palette override, transparency, shared background opacity, blur preference, sidebar width, keybinding overrides, scrollback/graphics limits, and terminal-initiated clipboard policy. Sidebar visibility is never a persisted or configured startup state.

Invalid configuration must produce a useful diagnostic and a safe recovery path. Apply independent valid settings where possible; never silently launch an unintended executable. Configuration and client state are separate files.

The implemented version-1 TOML schema uses `font`, `appearance`, `layout`, `layout_presets.NAME`, `keybindings`, `shell`, `environment`, `profiles.NAME`, `limits`, and `clipboard` tables, with `default_profile` selecting a named profile. `font.family` accepts a valid installed or bundled font family; Settings writes the concrete family name for bundled terminal presets. `appearance.theme` selects application colors and the default terminal palette. `appearance.terminal_theme` remembers a palette override, used only when `appearance.terminal_theme_override` is true. Both IDs may be stable bundled or imported `user-` IDs. Legacy differing palette IDs enable the override during migration; missing/equal terminal IDs follow the theme. `appearance.favorites` is an optional ordered list of IDs. `appearance.ui_font` uses the bundled interface-font catalog. `appearance.transparent_background` controls transparency without discarding the remembered 0.1–1.0 `appearance.terminal_opacity` or `appearance.background_effect` (`clear`/`blurred`). Legacy `opaque` migrates to transparency disabled with a remembered Clear preference. `layout.sidebar_width` is 200–600 logical pixels. Each `layout_presets.NAME` table is a split with `split = "right"` (side by side) or `"down"` (stacked), a `ratio` strictly between 0 and 1 for the first child, and `first`/`second` set to `"pane"` or a nested inline split; names are 1–64 characters, presets hold 2–16 panes, at most 32 load, and invalid entries are diagnosed and skipped. Saving writes ratios to four decimals and preserves unrelated content. `limits.scrollback_lines` is 0–100,000 alongside the fixed 1 MiB history byte bound; `limits.graphics_bytes` is 0–64 MiB of retained image data/reservations per surface. `clipboard.policy` controls OSC 52 (`allow`/`deny`), not explicit user Copy/Paste. Explicit program argv is literal; use `login` deliberately for shell profiles.

`[metadata]` independently selects `directory` (default true), `process`, `git`, and `dimensions` (default false). Settings → Interface writes these global preferences. Manual labels remain primary; automatic captions use the focused/first pane, truncate fields separately from dirty/stale/count badges, and expose every pane's full values in a scrollable hover card. A bounded background worker queries protocol-17 metadata in the actual daemon environment with five-second caching; paint never spawns process/Git work, and disabling all fields stops GUI queries.

### Theme presets and access

- **Compi Neutral** is a first-class restrained baseline: neutral charcoal, subdued separators, and limited focus emphasis. It works with clear, blurred, or solid backgrounds without decorative accent-heavy surfaces. Dark Glass remains the first-run default; existing selections are not reassigned.
- Each theme supplies application tokens and a terminal palette. Theme normally updates both; an Advanced terminal override preserves its chosen palette while the application theme changes. Terminal-only changes leave application colors and transparency preferences unchanged. Application colors style chrome, native file browser/search inputs, and image-preview frames. Native document editors, notes, document previews, webviews, and diff viewers are not implemented; terminal-hosted programs retain terminal color semantics.
- **Quick Appearance** and **Settings → Appearance** expose one whole-application Theme and a **Transparent background** toggle. When transparency is on, show a continuous 10–100% **Opacity** slider and **Blur background** toggle. Turning transparency off keeps remembered opacity and blur settings. Native blur is window-scoped, not per-pane. **Advanced → Terminal colors** defaults to **Follow theme** and optionally overrides the terminal palette; Appearance identifies an active override. **Global defaults** and **This window** scopes remain available.
- The existing catalog contains 41 bundled themes (26 dark and 15 light), plus validated local themes. All-word search, dark/light/favorites filters, miniature header/terminal previews, and keyboard navigation remain. The applied theme stays first with an inline Current selector, unchanged during preview; matching favorites follow, then matching remaining themes alphabetically, without duplicates. Search occupies its own row. More holds export, attribution, and removal. Cancel/Apply remain in the footer. Advanced terminal overrides use a terminal-only catalog target, not Application/Terminal/Both controls in normal browsing.
- Preview never writes accepted appearance. Cancel restores the latest accepted selection, including global changes received during browsing. Global Apply writes shared defaults and clears only the targeted initiating-window overrides; other windows' explicit fields and material preferences are preserved. A failed global write keeps the catalog and truthful preview open with an error. Window saves remain asynchronous and visibly report unsaved state on failure.
- Favorites and local library operations persist independently of preview/cancel. Import adds every variant in a family but neither applies nor publishes it. Export writes a selected variant as standard Zed JSON, preserving family metadata, unused fields, and supplied attribution/license/notices; bundled exports include complete bundled notices. Remove local protects selected variants referenced by global defaults, open windows, CLI choices, or previews. Removal rewrites a family with its remaining variants, or deletes its installed file when empty; sibling variants and external source/export files remain untouched. Bundled entries are undeletable.
- The public format is Zed theme-family JSON, following the published v0.2.0 schema: required family `name`, `author`, and `themes`; required variant `name`, `appearance`, and `style`. No public Compi ID, version wrapper, or custom suffix is required. Files are data-only, at most 2 MiB, with at most 256 uniquely named variants. RGB/RGBA hex colors and missing/null optional values are supported. Known scalar colors and collection structures are validated, including unsupported editor/syntax fields; unknown future fields are preserved, not interpreted or fetched. Missing colors use appearance-specific defaults, with Compi Neutral supplying sparse dark chrome. Application surfaces/text/borders/control states and terminal colors resolve through the same codec for bundled and imported themes; Zed player cursor/selection supply terminal cursor/selection fallbacks. A theme's background-appearance field is retained but never changes Compi's transparency/blur preferences.
- Up to 256 imported variants live in `themes` under Compi's data directory. Native file import and direct `.json` directory discovery share validation and stable internal family/variant identities; identical import is a no-op, and conflicting variants or capacity failures reject the whole installation. Mutations use an OS-backed lock and atomic replacement. `ThemeLibrary::import_file` is the headless installation entry point used by the UI; CLI command wiring is reserved for the separate CLI feature session, which must target the Windows application's library when invoked from WSL.
- Bundled source families use the same standard JSON while a private manifest retains all 41 existing preset IDs, metadata, colors, and the Dark Glass default. Managed legacy imports migrate once through a private converter and identity index, without rewriting saved selections/favorites. Migration preserves attribution and verifies identity and resolved colors before retiring the old managed record to `.migrated`; failures retain the original. The legacy format is no longer accepted by public import/export. Unspecified license metadata does not assert permission to publish. Online publishing remains a separate, unimplemented action requiring explicit consent, authorship, licensing, and validation.
- Private client-state version 5 stores theme, optional terminal override, transparency, opacity, and blur preferences independently. Earlier explicit theme/palette overrides migrate without changing the effective appearance; version-1 materialized top-level themes remain inherited. Missing but syntactically valid theme IDs diagnose and render Compi Neutral without discarding layout/navigation or rewriting the saved selection. **Use global defaults** clears window appearance overrides. `--theme` remains invocation-local and locks both palette axes, never transparency or blur.
- Appearance changes never restart a process, detach a surface, or reset terminal contents. Explicit RGB and indexed 16–255 remain exact; only terminal defaults and ANSI 0–15 follow the terminal palette. Explicit cell backgrounds stay opaque even when equal to the default RGB, including inverse cells and RGBA theme tokens. Disabled, unsupported, or reduced transparency forces solid main/header background RGB; enabled transparency composes theme alpha with the user's opacity. Surface/selection alpha remains available over those backgrounds. Catalog miniature previews show the opaque color baseline independently of window material, and UI contrast accounts for alpha surfaces over the application baseline. Application/material changes retain terminal row caches; terminal-palette changes invalidate colors in every pane.
- Tabs share compact rounded-rectangle geometry across themes: subtle active fill, transparent inactive tabs, faint hover fill, and no bright accent border or full-pill shape. Close buttons reveal on hover in reserved slots without moving labels. Keyboard cycling, overflow, reorder, and tear-off keep their existing interaction contracts. New tear-off windows inherit accepted appearance, not a temporary theme/opacity preview; existing destinations retain their own presentation.

### Interface font presets

- The native system font remains the default so upgrades preserve the current platform-native presentation.
- **Settings → Interface** offers System default, IBM Plex Sans, Inter, and Atkinson Hyperlegible Next. Each row renders its own preview and applies immediately.
- The three non-system families are embedded in the client binary, registered before the first production window opens, and licensed under the SIL Open Font License 1.1.
- Interface-font selection is global and synchronizes across open windows using the same configuration path. It does not create a per-window override.
- Interface fonts apply to application chrome, controls, dialogs, palettes, and text fields. Terminal cells and miniature terminal previews retain fixed-width terminal typography.
- Unknown or unavailable selections produce a diagnostic and render with the native system font. Changing interface typography never restarts, detaches, resizes, or otherwise changes terminal work.

### Terminal font presets

- The platform-native monospace family remains the default: Cascadia Mono on Windows and Menlo on macOS.
- **Settings → Terminal** offers System monospace, JetBrains Mono, IBM Plex Mono, and Atkinson Hyperlegible Mono. Each keyboard-accessible row renders a code-oriented preview and applies immediately.
- The three non-system families include the normal, bold, and italic faces used by terminal rendering. They are embedded from the pinned Google Fonts catalog and licensed under the SIL Open Font License 1.1.
- Selection updates `font.family` atomically without changing size, line height, fallback families, terminal contents, processes, or layout structure. Custom installed family names remain supported through TOML and command-line overrides remain invocation-local.
- The selected family is fixed-cell validated before use. Missing, unloadable, or proportional families produce a diagnostic and fall back to the native monospace cascade.
- Terminal-font selection is global and synchronizes across open windows using the same configuration path. Interface typography remains unchanged.


### Glass and readability

- Terminal canvases and window headers are opaque by default and share user-selected 10–100% background opacity with explicit Clear, Blurred, or Opaque material. Header text, icons, window controls, and explicit application cell backgrounds remain opaque. Dragging previews both backgrounds continuously; the selected scope is saved on release.
- Use native background materials where supported. Opaque selection, full opacity, unavailable materials, or platform reduced-transparency preferences resolve both native material and default fills to fully opaque backgrounds. Retain the requested glass preference for later use; identical blur across platforms is not required.
- Presets must provide readable text and controls, adequate contrast, and visible focus in both material and opaque presentations. Color alone must not be the only indication of focus or lifecycle state.

### Brand

The in-terminal brand mark uses the selected rounded iris and pixel-derived, hooked swoop from the desktop icon. It is a filled vector with a pupil cutout, not a font glyph or a scaled desktop bitmap. The iris inherits the active theme accent, including during theme preview, and remains opaque while header backgrounds fade. Acid green (`#DFFB35`) remains the Dark Glass default; other themes supply their own accents. The desktop artwork is unchanged.

The optional **Warm Carbon** preset uses these warm color tokens; they are not the default Dark Glass palette:

| Role | Color |
|---|---|
| App background | `#171613` |
| Chrome | `#211F1A` |
| Raised surface | `#2B2922` |
| Border | `#403C31` |
| UI text | `#F4F1E8` |
| Muted text | `#AAA394` |
| Accent | `#E5C07B` |
| Initial terminal canvas | `#171613` |

Default interface typography is the native system font, with bundled alternatives. Terminal typography remains independently configurable and fixed-cell validated. Application colors, terminal palette, and background material are independently selected; program-supplied terminal RGB is never contrast-adjusted by UI helpers.

Use restrained spacing, readable labels, real icons, horizontal peer actions, visible focus, and sentence case. Avoid oversized dashboard chrome, decorative status bars, and fixed compactness that makes the terminal feel squeezed.

## Startup, storage, and distribution

- A source checkout on Mac or Windows must build and run without an installer or registration step.
- Windows requires the pinned Microsoft ConPTY/OpenConsole runtime pair beside the daemon. Source preparation is documented in README and automated in Windows packaging/CI. The stock runtime is not a fallback: native tracing showed it discarding Kitty APC output. Creation, resize and close use matching runtime APIs while preserving suspended-before-job-before-resume process ownership.
- Client/daemon protocol 18 and GUI launch/control handoff version 5 reject older incompatible binaries. Upgrades must not silently terminate existing work; close older clients and deliberately restart an older daemon only after accounting for its live processes.
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

The built-in Performance section samples only while visible or while its per-window FPS overlay is enabled. It reports client and daemon CPU, preferred resident/private memory, handles or file descriptors where supported, daemon surface counts, bounded render timing, display refresh when available, and shaped-row/decoded-image cache occupancy. Rendered-update FPS is an application redraw measure, not swap-chain presentation telemetry. Rebuild renderer clears client visual caches and preserves daemon-owned terminal state; reconnect remains a separate transport recovery action.

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

`compi-probe --connect [user@]host[:port]` applies the same workspace, hierarchy, attach, inspect, resize, restart, end, soak, and shutdown commands through the SSH transport. Its `workspace` JSON is the authoritative process-discovery surface: stable hierarchy IDs, surface and lifetime IDs, labels, lifecycle state, attachment state, dimensions, and errors.

Agent-specific discovery remains a narrow future extension, not process inference. Add optional non-secret launch metadata only when a concrete agent consumer defines the required identifier and kind. Keep memory, credentials, steering, provider state, and orchestration outside the process protocol.

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

CI must build and test the protocol, daemon, and client crates on Linux, macOS, and Windows. It must build the native client on macOS and Windows. WSL runtime tests require a qualified runner; an unavailable WSL environment is reported as missing coverage, never a passing runtime test.

### Daily-use qualification

On both macOS and Windows/WSL:

1. Open a native window into a real shell without an installer.
2. Create and switch user-facing workspaces, reorder terminal tabs, and build nested splits; restore each workspace's last selected tab/pane without a separate session navigation layer.
3. Resize the window and dividers; manually show/hide and resize the sidebar while retaining top tabs; tear off a split tab into a new window and transfer it to an existing window; cancel a drag and exercise failed-transfer recovery; relaunch. Verify remembered sidebar width and transferred-tab placement without changing process identities or the split tree. Every new/relaunched window starts with the sidebar closed; reconnect, workspace switching, and tab actions do not open it.
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

Existing evidence sources are [frame tests](../../crates/compi-protocol/src/frame.rs), [control protocol tests](../../crates/compi-protocol/src/lib.rs), [engine tests](../../crates/compi-daemon/src/terminal/mod.rs), [replica boundary tests](../../crates/compi-daemon/tests/terminal_compatibility.rs), [trace replay tests](../../crates/compi-daemon/src/terminal/trace.rs), [metadata tests](../../crates/compi-daemon/src/workspace_store.rs), [Windows daemon integration](../../crates/compi-daemon/tests/daemon_integration.rs), [Unix daemon integration](../../crates/compi-daemon/tests/unix_daemon_integration.rs), [Windows recipes](testcmds.md), and [historical observations](ACCEPTANCE_RESULTS_2026-09-02.md). [Completed work](COMPLETED.md) records dated native Windows/WSL and hosted three-OS core CI evidence; [next steps](NEXT_STEPS.md) records remaining qualification. Test presence and dated reports do not establish the full current Mac workspace or physical-input/display gate.

#### Automated and architectural acceptance

| ID | Requirement and observable proof | Implement / qualify | Platforms and retained evidence or gap |
|---|---|---|---|
| A1 | Control/screen round trips at the negotiated current protocol version; truncated/oversized frames reject before oversized allocation; incompatible versions reject; capabilities negotiate explicitly. | P1 establishes framed transport; P2 adds version handshake; P3 adds hierarchy protocol. | All hosts. Protocol 18 checks exact version and frame limits; explicit capability negotiation remains a baseline requirement, not a current Hello field. Historical v7 byte fixtures are regression evidence, not a compatibility promise. |
| A2 | Create nested trees, reject invalid mutations without partial changes, retain launch-failed panes, reorder/remove, conflict at stale revisions, and restore migrated metadata/backup. Inject failure around every commit boundary. | P3; P5 integrated repeat. | All hosts. Workspace actor, durable receipts, migrated backup/quarantine, and native Windows mutation/restart/probe coverage exist; exhaustive fault injection and full Mac workspace qualification remain to be proved. |
| A3 | Replica equals authority after deltas/resize; missing predecessors resnapshot; history pages stay bounded; eviction/reflow invalidate anchors; old generation/lifetime messages cannot corrupt new state. | P1 core; P2 recovery; P3 lifetime restarts; P4 client anchors; P5 repeat. | All hosts. Mirror/gap/reflow and stale-lifetime recovery are covered. Protocol 18 sends bounded scrollback in snapshots/deltas, not on-demand history pages or history-epoch/logical-line anchors; client restoration instead checks identity, sequence, dimensions, and content fingerprint. Paging/epoch requirements remain open. |
| A4 | Deterministic output/resize replays preserve Unicode, combining/wide cells, attributes/cursor, alternate screen/editing/margins, modes, engine replies, OSC handling, and Kitty transfer/place/delete behavior. | P1 and every engine-affecting change; P5 real applications. | All hosts; both clients for interaction. Engine and trace tests plus Windows graphics/TUI recipes exist; not all required interactions have automated coverage. |
| A5 | Actual PTYs support interactive shells, wait/exit, resize, and terminal replies; no piped-stdio substitute. | P2; P5 repeat. | Real Unix PTYs run on macOS/Linux and ConPTY/WSL on Windows; hosted three-OS core CI and native Mac/Windows shell smoke exist. Full interactive Mac qualification remains. |
| A6 | Executable/argv/cwd/env preserve quoting, spaces, native shell startup, locale, fresh context, job control, agent/helper access, OSC 7 inheritance, invalid-cwd diagnostics, and verified descendant cleanup. No secret environment values in metadata/logs. | P2; P3 lifecycle; P5 interactive repeat. | All hosts. Unix argv/cwd/env and descendant cleanup, Windows WSL shell/descendant tests, and Bash/Zsh cwd bridge coverage exist; physical Mac shell integration and full agent/helper/locale matrix remain to qualify. |
| A7 | Detach, client crash, reconnect, and simultaneous launch preserve the same process without extra shell creation; attachment conflict never steals control or causes resize fights. | P2 simple surfaces; P3 hierarchy; P4 windows/navigation. | All hosts headless; both clients. Unix and Windows detach/reconnect/conflict tests, native Mac simple reattach, and Windows multiwindow handoff evidence exist; full Mac workspace transfer remains unqualified. |
| A8 | Server loss reports truthful lost work; stale endpoint recovery checks ownership; malformed state is quarantined visibly; saved commands never auto-run; restart is explicit. | P2 endpoints/server failure; P3 durable recovery/restart. | All hosts. Unix private endpoint/stale-owner tests and Windows lost-work/quarantine/restart coverage exist; cross-platform recovery and failure-injection matrix remains a qualification obligation. |
| A9 | Pure layout minima/overflow/focus, command enablement/routing, platform shortcuts, slot ownership, state precedence, hidden membership, and fallback navigation have deterministic outcomes. | P1 extracts pure client behavior; P4 workspace client. | All hosts pure tests; both clients runtime checks. Layout, commands, exclusive window slots, and hidden navigation are implemented and exercised on Windows; native Mac layout/input behavior remains unqualified. |
| A10 | Protocol has no engine/UI/PTY/OS dependency; replicas consume wire DTOs, not engine instances; headless server graph excludes GPUI/installer; native clients use shared core. | P1 dependency extraction; P2 real all-host server. | Three product crates and isolated installer, dependency-boundary checks, hosted all-OS core jobs, and native Windows/Mac builds establish the architecture; Mac workspace interaction remains a separate qualification gate. |
| A11 | Per-user private endpoints/ACLs and peer authorization reject unauthorized access; race-safe instance ownership avoids duplicate servers; development instances isolate state/processes; no network listener. | P2; P5 platform repeat. | All hosts. Unix private sockets, peer checks and instance ownership plus Windows restricted pipes/isolated instances are implemented; security and lifecycle must continue to be verified per OS. |
| A12 | `compi-probe` uses the normal protocol for inspect, launch, attach, resize, and terminate; traces are opt-in, local, bounded, and replayable without UI. | P1 probe/replay; P2 Unix probe; P3 hierarchy; P6 optional agent metadata. | All hosts. Unix and Windows probe paths, hierarchy actions, bounded traces, and SSH relay support exist; real SSH host qualification and agent metadata await their stated gates. |
| A13 | Core tests run on three OSs; server builds/tests become real on three OSs; client builds on both primary OSs. Missing WSL runtime is reported, never silently counted as a pass. | P1 core CI; P2 all-host runtime and Mac client CI; P5 native qualification. | Hosted Linux/macOS/Windows core jobs and native Mac client build passed; qualified Windows WSL and Unix runtime integration also ran. Passing CI does not qualify Mac graphical workspace, physical input, or Linux desktop. |

#### Daily-use acceptance

The D-number matches the numbered daily-use procedure above. All rows qualify on **both clients in P5**, with platform/build/display/workload attribution.

| ID | Observable workflow | Implementation prerequisite | Retained evidence / remaining qualification |
|---|---|---|---|
| D1 | Source launch opens a real shell without installation; first run seeds once, later launches do not duplicate work. | P2 shell/startup; P3 initialized workspace. | Native Mac shell close/reopen and Windows first-run/empty-workspace evidence exist; full Mac workspace restoration remains. |
| D2 | Create sessions, reorder tabs, and build nested splits without respawning moved work. | P3 hierarchy; P4 UI. | Server hierarchy and native Windows tab/split/reorder workflows are implemented; native Mac workspace workflow remains unqualified. |
| D3 | Resize window/dividers, show/hide/resize the secondary sidebar, tear off/transfer tabs, cancel/fail a transfer, and relaunch; primary tabs remain available, sidebar width persists but new/relaunched windows start closed, and processes/tree survive window changes. | P4 layout/client state/window transfer. | Native Windows evidence covers split/sidebar, handoff, canceled drag, failed-destination rollback, and persistence; full Mac workspace transfer and physical native drag remain. |
| D4 | Shell editing/job control, Git, `less`, Vim/Neovim, `fzf` preview, and a real agent harness work interactively. | P1 preserved engine; P2 host/input. | Windows shell/TUI recipes retained; both-platform current runs required. |
| D5 | Unicode and exact wrapped selection copy, bracketed paste, mouse/focus/Shift override, validated links, OSC 52 policy, and graphics work; reattach never repeats clipboard/bell effects. | P1 core; P2 native adapters; P4 command focus. | Windows native terminal interaction and graphics coverage plus link and renderer tests exist; Mac IME/clipboard/graphics and the full physical interaction matrix remain. |
| D6 | Close/crash/reopen client onto the same live processes and split tree with valid navigation/anchors. | P2 persistence; P3 tree; P4 client state. | Native Mac single-surface and Windows split-tree close/reopen/selection evidence exist; complete Mac split navigation and process-preserving transfer remain. |
| D7 | Discover and restore hidden work; remove only via explicit confirmation and end only via the explicitly named End action; inspect final state and verify descendants are gone. | P2 cleanup; P3 lifecycle; P4 discovery/actions. | Hidden-slot membership, retained exited panes, descendant cleanup, and Windows confirmation UI are implemented; full Mac workflow and physical interaction remain. |
| D8 | Flood one pane while typing/navigating another; control/input remains usable and resync recovers bounded overflow. | P2 queues; P4 multiple panes. | Windows native `yes`-in-one-pane/input-in-another and bounded-backpressure integration tests exist; Mac multi-pane and attributable sustained-load measurements remain. |
| D9 | Narrow/minimum, maximized/fullscreen, scaling, mixed displays, native controls, IME/fonts, and offscreen focus remain usable without auto-collapse or pane zoom. | P2 native window/input; P4 constrained layout. | Windows display recipes and dated observations retained; physical checks on both platforms still required. |
| D10 | Isolated mixed-workload soak has no crash, input loss, cross-surface corruption, unexplained growth, or lifecycle leak; failures have bounded reproducible traces. | P1 trace retention; P2 host; P4 full workspace. | Windows soak tooling and resource checks exist; current mixed-pane Mac soak and attributable Mac resource measurements remain. |

#### Appearance, resource, and distribution acceptance

| ID | Requirement and observable proof | Implement / qualify | Platforms and gap |
|---|---|---|---|
| C1 | Versioned TOML validates independent settings; invalid executable never silently launches; profile/env/cwd/font/keybinding/limit/clipboard settings take effect; Open configuration works without rewriting user config. | P2 launch settings; P4 config/commands; P5 repeat. | Both clients, all-host launch resolution. Versioned configuration, provenance, scoped appearance saves, and Windows settings coverage exist; Mac native configuration interactions remain. |
| C2 | One keyboard-accessible catalog supports bundled/local search, favorites, targeted application/terminal preview, cancel, scoped apply, validated import, attributed export, and guarded local removal without detaching/resetting work. | P4; P5 repeat. | Both clients. Release tests and Windows native import/export, validation, target isolation, cancel, removal protection, write failure, and scoped persistence are exercised; Mac native qualification remains. |
| C3 | Compi Neutral is selectable without changing Dark Glass first-run defaults; shared background opacity and explicit Clear/Blurred/Opaque remain independent from both palettes; explicit cell backgrounds stay opaque; compact tab geometry preserves hover-close, keyboard focus, overflow, reorder, and tear-off. | P4; P5 native/display qualification. | Both clients. Windows native light/dark backdrop material checks, tab hover/cycling/reorder/tear-off, and narrow layouts are exercised; physical Mac display/accessibility and mixed-DPI qualification remain. |
| R1 | During sustained output, input/control and navigation remain usable; paint meets the measured display frame budget. Record input-to-presentation separately from paint CPU and physical frame pacing. | P2 simple view; P4 panes; P5 qualification. | Both clients. Historical Windows timing is diagnostic input only. |
| R2 | Queues, history/pages, image transfers/decodes/caches, and traces remain bounded; render visible rows/graphics, decode off UI thread, and coalesce repaint. | P1 core limits; P2 queues; P4 renderer; P5 measure. | Bounded queues, images, caches, and traces exist; on-demand history paging is a requirement not implemented in protocol 18, and all-platform aggregate resource proof remains. |
| R3 | Repeated create/split/resize/detach/reconnect/end and mixed soak show no leaked processes, threads, handles/FDs, CPU, private/working-set or GPU-memory growth. Measure empty client/server, marginal active/idle/detached surfaces, and pane/history costs separately. | P2 lifecycle; P4 full operations; P5 qualification. | Windows handle-cycle tests and native Linux PTY/process tests exist; Mac mixed-pane soak and attributable marginal resources remain. |
| R4 | Attribute cold/warm launch, reconnect, ready-for-input, and memory/latency reports to hardware/build/display/workload; set numeric budgets from those baselines and report missing physical checks. | P2 diagnostic baseline; P5 release budgets. | Mac simple-shell startup diagnostics and Windows metrics exist, not complete Mac workspace or input-to-presentation/frame-pacing budgets. |
| X1 | Source builds need no installer; idle shutdown never abandons live detached work; version mismatch never silently upgrades/kills processes; intentional interruption warns about affected work. | P2 lifecycle/version handling; P3 incompatible metadata upgrade; P6 distribution. | Mac/Windows source and packaged client/daemon smoke preserve work; protocol mismatch rejects rather than killing an older daemon. Cross-version upgrades require deliberate process accounting. |
| X2 | Runnable Mac/Windows dogfood artifacts precede public-release qualification; packaging, signing/notarization, upgrade, repair/removal, and clean-machine checks use exact attributable artifacts. | P5 dogfood; P6 distribution. | Windows unsigned installer/portable package and ad-hoc-signed Mac app/DMG built and passed native launch/reconnect smoke. Mac dogfood, real signing/notarization, old-to-new upgrade, and clean-profile qualification remain; no public release claim. |

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
| Tear off a split tab, drop into another window, cancel, or fail midway through attachment handoff. | Same tab/tree/process lifetimes, exclusive control, independent window presentation, truthful recovery, and no implicit shell creation; transferred membership survives relaunch. | P4, A7/A9/D3 |

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

- Establish three product crates: shared `compi-protocol`, process-owning `compi-daemon`, and user-facing `compi-client`.
- Extract the pre-migration protocol/framing and terminal screen DTOs/codecs into `compi-protocol`. Preserve public wire fields, serde names, enum/field ordering, bincode configuration, frame kind values, and protocol version **7**.
- Keep `TerminalState`, parser, buffers, terminal semantics, and deterministic replay behavior in the daemon's terminal module. Keep wire DTO definitions independent of engine implementation and convert engine output at the daemon/session boundary without avoidable whole-grid copies.
- Keep `ScreenMirror`, `MirrorApply`, pure interaction helpers, daemon connection, probe, and GPUI renderer in `compi-client`, consuming only the shared protocol contract.
- Migrate all affected imports in the daemon, client, renderer, probe, traces, and tests directly. No old-module re-export shims or duplicate parsers/codecs. Preserve opt-in bounded trace recording and UI-independent replay.
- Add Linux/macOS/Windows CI jobs that test the three product crates; retain the Windows application/integration path separately. The daemon's production dependency graph must remain free of GPUI and installer dependencies.

**Observable acceptance**

1. Before moving code, capture representative v7 control and screen wire fixtures using the current codecs. Both old and extracted decoders consume them with equal values; extracted encoding produces the same bytes, including snapshots, deltas, graphics, and optional control fields. The frame remains little-endian `u32` payload-byte count, then `u8` kind, then payload; the count excludes both header fields. Preserve the existing 16 MiB frame and 1 MiB control limits.
2. Existing terminal/replay/replica behavioral cases run against their new owners on all three OSs. Replaying output and resize yields the same renderable state and terminal replies; a missing delta still requests recovery and resnapshot restores equivalence.
3. Dependency inspection proves `compi-protocol` has no PTY or graphics dependency, `compi-client` has no daemon or PTY dependency, and the daemon's production graph has no GPUI, client, or installer dependency.
4. Windows binaries and probe still build. On a qualified Windows/WSL host, exercise real create/input/resize/detach/reattach/terminate plus the existing daemon integration regressions. Missing host access is reported as unverified, not a runtime pass or completed extraction gate.
5. Terminal semantics, persistence format, control conflicts, and user-visible Windows behavior remain unchanged. No new Phase 0 identity/revision fields are slipped into v7.

**Excluded:** engine replacement, `portable-pty`, Unix hosting/transport, workspace schema migration/actor, process-lifetime protocol changes, splits, theme picker, and UI redesign. These belong to later named phases.

The three product boundaries and isolated installer are implemented; Phase 1's hosted three-OS core CI gate passed. Phase 2 supplied real Unix adapters and an all-host runtime, not a cfg-disabled no-op daemon.

### 2. Prove a real Mac terminal end to end

- Integrate `portable-pty` and host launch descriptions.
- Add Unix local transport and platform paths.
- Enable the same server/client application modules on Mac.
- Implement native Mac window, font, clipboard, key, and input behavior.
- Keep the initial view simple while proving the shared execution path.

Done when a Mac window opens a native shell, handles interactive programs and resize, then closes/reopens onto the same live surface. Windows/WSL still passes its regression path.

**Implementation status (2026-09-06):** the shared Unix server, native Mac GPUI client, launch description, private Unix transport/paths, and Linux runtime CI coverage are implemented. Real Mac verification includes native shell startup, readable rendering, AppKit text input, Vim editing, resize, native close, and ordinary reattachment to the live session. This is not full phase qualification: physical Mac input/display checks and native Linux/Windows regressions remain outstanding. Windows retains the suspended-before-job ConPTY backend pending portable-PTY ownership acceptance. See the [Phase 2 implementation evidence](COMPLETED.md#phase-2--native-mac-runtime-implemented) and [remaining qualification](NEXT_STEPS.md#3-native-mac-and-shared-platform-qualification).

### 3. Introduce workspace ownership and migration

- Add session/tab/pane/surface IDs and the workspace actor.
- Map each old shell-session record to a surface inside a tab in an imported session.
- Migrate metadata with backup and explicit version checks.
- Require a warned server restart for incompatible live-state upgrades; do not promise in-place process migration.
- Add atomic workspace persistence and lost-surface recovery.

Done when the protocol and headless tools manipulate the hierarchy, survive client disconnection, and restore truthful structural state after server restart.

### 4. Deliver the full workspace client

- Build workspace navigation over existing server sessions, terminal tab ordering, and nested splits, without a separate user-facing session layer.
- Add draggable dividers, pane focus, primary top terminal tabs, and a secondary resizable workspace sidebar hidden until explicitly summoned. Remember sidebar width, not visibility across relaunch; every new window starts closed.
- Add tab tear-off into new native windows and transfer into existing windows, with explicit attachment handoff, failure recovery, and durable per-window membership.
- Add the command registry, palette, and platform keybindings.
- Establish application and terminal tokens, Dark Glass and Compi Neutral, then extend the existing catalog, Quick Appearance, and Settings with independent axes and local data-only import/export. Support scoped 10–100% shared background opacity with Clear/Blurred/Opaque material, keep explicit terminal cell backgrounds opaque, and provide readable reduced-transparency fallbacks.
- Persist client-local state independently of server structure.
- Implement non-destructive detach and clearly named termination/removal actions.

Done when the full daily-use workspace can be created, moved between native windows, and reopened on Mac and Windows without process restarts caused by layout or window changes.

**Implementation status (2026-09-07):** the full client is implemented with native Windows/WSL evidence in [completed work](COMPLETED.md#phase-4--workspace-client-implemented). Protocol v9 contains the narrow launch/split/clear-history extensions needed by this client; there is no new PTY or terminal-engine replacement. Native Mac execution and shared-baseline qualification remain open in [next steps](NEXT_STEPS.md#3-native-mac-and-shared-platform-qualification). Viewport restoration requires matching server generation, process lifetime, snapshot sequence, dimensions, and content fingerprint; uncertain anchors are discarded. Divider commits may refresh metadata-only revisions only while the captured tree and server identity remain unchanged, and only explicitly rejected revision conflicts may be retried.

**Current implementation (2026-10-05):** protocol 16 also includes bounded directory listing/search, one-shot shell-origin file/project pickers, Kitty keyboard flags, split-pane detachment without process replacement, `ArrangeTab`/`MergeTabs`/`SplitMergedTabs` pane arrangements that only move existing leaves, and shell prompt detection/preview/planned application. Native Windows/WSL interaction and hosted three-OS core CI have evidence in [completed work](COMPLETED.md); full native Mac workspace, physical input/display and mixed-pane resource qualification, and a successful real-host SSH relay run remain in [next steps](NEXT_STEPS.md). These are qualification gaps, not absent server hierarchy or client controls.

### 5. Qualify the shared baseline

- Run the representative workflows and mixed-pane soak on both platforms.
- Address narrow-window, font, IME, clipboard, scaling, and native-control failures.
- Publish attributable latency/resource measurements and missing coverage.
- Produce runnable Mac and Windows dogfood artifacts.

Done when the owner can work on Compi using Compi on either primary machine. A Windows-only release candidate is not completion of this baseline.

### 6. Qualify distribution and extend process use

- Finish platform signing/notarization, version-to-version upgrade, and clean-machine qualification.
- Keep the local and SSH `compi-probe` workflows on the same launch, workspace, and lifecycle contracts.
- Add optional agent discovery metadata only for a concrete consumer; do not infer agents from commands or process names.
- Keep process hosting separate from agent memory, credentials, steering, and orchestration.

The launch API is generic. An agent is a process in a surface, not a different server architecture.

## Explicitly outside this baseline

- Process resurrection after server death, reboot, or runtime termination.
- Compi-owned network listeners, automatic remote discovery, cloud accounts/relays, and cross-machine workspace synchronization.
- Shared multi-user workspaces or simultaneous controlling clients on one surface.
- Arbitrary dock frameworks, detachable tool panels outside the window, and non-terminal pane applications. Floating existing panes inside a window is covered by [Primary tabs and optional sidebar](#primary-tabs-and-optional-sidebar).
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
- Primary terminal tabs remain usable without the optional sidebar. Sidebar width, window state, focus, and accepted theme remain client-local and remembered; sidebar visibility is transient and every new/relaunched window starts closed.
- Terminal tabs and their complete split layouts move into new or existing native windows without changing process lifetimes, stealing control, or losing discoverable work on failure.
- The command palette exposes the ordinary workspace and process controls.
- Users can discover, import/export, preview, cancel, independently select, and restore application/terminal themes across relaunch without editing configuration or interrupting terminal work.
- Closing and reopening returns to live work without duplication, data corruption, or implicit termination.
- Lost processes are reported truthfully and restart requires explicit action.
- Both primary platforms pass the shared daily-use qualification with remaining distribution-only gaps identified.

Compi is not a Windows application waiting for a Mac port. It is one persistent terminal workspace with native platform integrations.

## References

- [Compi repository](https://github.com/cloudboy-jh/compi): existing implementation to migrate and preserve where compatible.
- [SuperTerminal repository](https://github.com/sonnylazuardi/superterminal): reference for workspace structure, native terminal-first interaction, and platform separation, not a mandate to copy its stack.
- [Next steps](NEXT_STEPS.md): unfinished implementation and qualification work.
- [Completed work](COMPLETED.md): implemented phases and dated verification evidence.
- [Windows terminal test recipes](testcmds.md): existing exercises to adapt to the cross-platform qualification matrix.
- [Historical Windows acceptance results](ACCEPTANCE_RESULTS_2026-09-02.md): dated evidence from the previous implementation, not current baseline qualification.
