> _Companion for your computer._ A native Windows terminal whose WSL sessions survive the window. Built for the developer whose work is bash but whose OS is Windows.

---

## Thesis

Everyone builds *around* Windows. WSL. Web clients. "Just use SSH." Forked ports maintained on nights and weekends. Nobody builds the native terminal experience for the person who lives on Windows but works in bash.

Compi treats **Windows as the primary client, not an afterthought**: a native Windows daemon keeps WSL bash sessions alive while a GPUI client behaves like a normal terminal. **Bash is the native citizen. Windows is the glass, UNIX is the computer.**

The first release proves one thing: a human can close the Compi window, reopen it, and reattach to the same live shell without entering a multiplexer mode.

---

## Product contract

Compi is a **session-native terminal**. The daemon owns live shell processes and terminal state. The client is a disposable lens into them.

- Closing or crashing the client does not stop a session.
- Reopening the client can reattach to a live session.
- Stopping the daemon, rebooting Windows, or terminating WSL ends running processes.
- Persisted metadata can describe dead sessions, but Compi never claims to resurrect process state.
- Default launch creates a new session. Reattachment is explicit.
- No modes, prefix keys, or visible tmux-style operation.

Traditional terminal: open app → get shell → close app → shell dies.

Compi: create session → attach or detach clients → shell lives until explicitly killed or its daemon/WSL runtime stops.

---

## Feel bar

This is the acceptance criteria.

- **Sub-bounce launch.** Open Compi and reach a new shell with no perceptible wait.
- **Normal terminal behavior.** Shell startup, input, selection, clipboard, keys, profiles, history, and ANSI output behave as expected.
- **Client-independent sessions.** Closing the window leaves work running.
- **Sub-bounce reattach.** Attaching feels as fast as opening a normal tab.
- **Native local scrolling.** The client scrolls a local mirror rather than round-tripping each gesture.
- **No stale-session ambush.** A normal launch starts fresh; the switcher exposes detached sessions.

---

## Runtime model

```text
WSL bash
   ↕
Windows ConPTY
   ↕ raw input and VT output
compi-daemon
   ├── session manager
   ├── vte parser
   ├── authoritative ScreenState
   ├── server-side scrollback
   └── per-user Windows named pipe
              ↕ snapshots + sequenced deltas
compi client
   ├── disposable local screen/scrollback mirror
   ├── GPUI terminal grid
   ├── session tabs
   └── detached-session switcher
```

### Process boundary

- `compi-daemon` is one native Windows process per Windows user.
- Each session owns one ConPTY and one `wsl.exe` child process.
- P0 launches interactive Bash in the user's default WSL distribution. An explicit working directory pins that distribution for path resolution and process launch; distribution selection remains post-P0.
- **WSL2 only.** WSL1 is not tested or targeted. ConPTY works identically with both, but WSL2 provides full system-call compatibility, real process management, and native Docker support. Every Compi user is already on WSL2.
- A session ends when the shell exits, the user kills it, the daemon stops, Windows reboots, or WSL terminates.
- The daemon is supervised and restarts after failure, but shells lost with the daemon are reported as dead, not silently replaced.

### IPC

P0 uses a **Windows named pipe**, not TCP and not a vague cross-platform socket.

- Pipe access is restricted to the current Windows user.
- One daemon instance owns a stable per-user pipe name.
- A second daemon detects the existing owner and exits.
- No fixed port, firewall surface, shared token, or remote access in P0.
- Unix domain sockets are a later platform implementation detail.

---

## Terminal authority

The daemon is the only authority for terminal state.

1. ConPTY produces raw VT bytes.
2. The daemon feeds those bytes to `vte`.
3. A custom Rust `ScreenState` applies parser actions to the cell grid, cursor, modes, title, attributes, alternate screen, and scrollback.
4. The daemon sends clients a complete snapshot followed by ordered deltas.
5. The client applies deltas to a disposable local mirror and renders it.
6. If a client detects a missing sequence number, it discards uncertain state and requests a fresh snapshot.

The client does not independently parse ConPTY output. Terminal-generated replies such as device status and cursor-position responses are written back through ConPTY by the daemon.

### Initial screen model

`ScreenState` must cover the behavior needed by normal interactive shells before visual polish:

- Unicode cells, wide characters, combining marks, and blank continuation cells
- Foreground/background colors and common text attributes
- Cursor position, shape, visibility, save/restore
- Line wrapping, insertion/deletion, erase operations, and scroll regions
- Main and alternate screen buffers
- Terminal title and bracketed paste mode
- Server-side scrollback ring with a defined memory cap
- Resize with deterministic reflow policy
- **Kitty graphics protocol** — placement, delete, transmit, compositing over text
The screen model maintains a composited graphics layer over the character grid. Image cells survive scrollback and reattach. The client renders images as GPU texture quads over the terminal grid.

Unsupported sequences must fail safely and be observable in debug logs.

---

## Environment fidelity

The adoption risk is correctness, not polish.

- Launch the user's default WSL distribution unless explicitly configured otherwise.
- Preserve normal login versus interactive bash semantics.
- Do not rewrite PATH, reorder environment variables, or inject shell startup scripts.
- Preserve `.bashrc`, `inputrc`, readline behavior, history, key bindings, SSH agent access, and credential helpers.
- Accept an optional absolute WSL or Windows working directory at session creation. Resolve and validate it once through the default WSL2 distribution, then pass it to `wsl.exe --cd` without shell interpolation.
- Track valid `file://` OSC 7 paths as live terminal state so new tabs can inherit the active directory. Do not inject `cd`, rewrite startup files, or treat OSC 7 state as recoverable process state.
- `git push`, interactive TUIs, editors, agents, and Ctrl+C/Ctrl+Z must behave as they do in an established WSL terminal.
- **WSL2 only.** WSL1 is not tested or targeted. ConPTY works identically with both, but WSL2 provides full system-call compatibility, real process management, and native Docker support. Every Compi user is already on WSL2.
- Every live session must be visible and killable.
- Default-terminal registration is optional, explicit, and reversible.

---

## Client surface

The GPUI client starts deliberately small.

### P0

- Native Windows window
- GPU-rendered terminal cell grid
- Text selection, copy, paste, mouse input, focus, and resize
- Tabs for attached live sessions
- New, attach, detach, and kill actions
- Transient switcher for detached sessions
- Local screen and scrollback mirror
- Carbon warm application chrome
- User-controlled terminal background, foreground, and ANSI palette

### Deferred client work

- Dock framework and arbitrary pane layouts
- Draggable split trees
- Ligatures
- Smooth scrolling and animation polish
- Theme/preferences UI
- Mac client build and Mac rendering polish

A fixed tab strip is enough for the first usable build. `gpui-component` may supply basic controls, but Compi does not depend on its dock system for P0.

---

## Protocol

Every frame uses permanent outer framing:

```text
u32 payload length | u8 message type | payload
```

P0 favors debuggability over premature compression.

### Control messages

JSON payloads for:

- hello and protocol version
- list sessions
- create session
- Protocol v6 create-session messages carry an optional working directory; session metadata distinguishes requested paths from resolved WSL paths and the selected distribution.
- attach and detach
- input
- resize
- kill
- request snapshot
- errors and session exit

### Screen transport

- Attach returns a complete snapshot with a sequence baseline.
- Subsequent screen deltas carry monotonically increasing sequence numbers.
- A gap triggers a snapshot request.
- Snapshots and deltas carry validated OSC 7 current-directory state.
- The first implementation may encode screen snapshots and deltas as JSON behind the permanent frame envelope.
- Compact binary screen payloads are introduced only after profiling proves JSON is a bottleneck.
- P0 permits one attached controlling client per session. A second attach returns `already_attached`; automatic takeover and multi-viewer behavior are deferred.

Protocol versions must reject incompatible clients clearly rather than corrupt state.

---

## Build order

### Milestone 0: `compi-probe`

An ugly console executable that de-risks Windows and WSL before GPUI work:

- Create and close a ConPTY
- Start the user's default WSL distribution and interactive bash
- Forward keyboard input
- Stream output
- Propagate terminal resize
- Handle Ctrl+C and clean shell exit
- Keep the shell alive while one console client disconnects and reconnects

Done when common shell commands and an interactive TUI survive repeated attach/detach cycles without corruption or leaked processes.

### Milestone 1: persistent human sessions

- `compi-daemon` process and per-user named pipe
- Session create, list, attach, detach, and kill
- Multiple live WSL bash sessions
- Clear daemon/shell lifecycle and failure reporting
- Integration tests for framing, reconnect, resize, and process cleanup

### Milestone 2: terminal state

- `vte` parser integration
- Custom authoritative `ScreenState`
- Snapshot plus sequenced-delta transport
- **Kitty graphics protocol support** — transmit, placement, delete, compositing over text grid
- Main/alternate screens and bounded scrollback
- Attach recovery after a missed delta
- Compatibility pass against representative shells and TUIs

### Milestone 3: usable GPUI client

- GPUI window and terminal grid
- **GPU texture-based image rendering** for kitty graphics placements
- Keyboard and mouse input
- Selection and clipboard
- Local scrollback mirror
- Tabs and transient detached-session switcher
- Carbon warm application chrome
- Installer that starts and supervises the daemon

### Milestone 4: agent sessions

Agent support ships only after persistent human sessions work.

- Session driver tag: `human` or `agent`
- Optional agent metadata: agent name, repository, external session ID
- Headless spawn with no client attached
- Agent sessions appear in the same tabs and switcher
- Compi hosts processes; it does not embed memory, steering, or orchestration

---

## P0 scope

P0 ends after Milestone 3.

### Included

- Native Windows `compi-daemon`
- WSL bash through ConPTY
- Human session create, attach, detach, list, and kill
- Per-user Windows named pipe
- Pure-Rust `vte` parsing and custom `ScreenState`
- **Kitty graphics protocol** on daemon and GPU-accelerated image rendering on the client
- Authoritative snapshots, sequenced deltas, and bounded server scrollback
- Basic GPUI cell renderer
- Tabs and transient detached-session switcher
- Input, resize, selection, clipboard, and clean exit behavior
- Setup flow that detects WSL, installs Compi, and supervises the daemon

### Not in P0

- Agent tags or headless agent spawning
- Daemon/reboot survival of running processes
- Remote or Linux daemon mode
- Mac client
- Multi-viewer broadcast
- Arbitrary docks and split layouts
- Binary cell-stream optimization
- Ligatures and animation polish
- Scrollback persistence across daemon restart
- Theme/preferences UI
- Forced terminal theme
- Sixel protocol support
- Default-terminal registration
- Memory, steering, or multi-agent orchestration

---

## Platform strategy

| Platform | Role |
|---|---|
| **Windows** | P0 client and daemon. GPUI renders the client; ConPTY hosts WSL bash. |
| **macOS** | Later GPUI client. Not assumed to be a free build; platform work is budgeted explicitly. |
| **Linux** | Possible later daemon for remote sessions. |
| **Phone** | Future protocol client, if justified. |

---

## Brand and UI palette

Compi uses **Carbon warm** for application chrome. It does not force a terminal color scheme.

| Role | Hex | Use |
|---|---|---|
| **App background** | `#171613` | Main application shell |
| **Chrome** | `#211F1A` | Title bar and tab bar |
| **Raised surface** | `#2B2922` | Switchers, menus, selected tabs, overlays |
| **Border** | `#403C31` | Dividers, outlines, inactive controls |
| **UI text** | `#F4F1E8` | Primary interface text |
| **Muted text** | `#AAA394` | Secondary labels and metadata |
| **Acid lime** | `#DFFB35` | Brand mark, focus, activity, and selection |
| **Default terminal canvas** | `#1A1916` | Initial default only; user-configurable |

The acid-lime dinosaur eye and black `/` are the primary mark. Illustrated green-blue scale tones belong to the icon, not the product UI. Acid lime is used sparingly so focus and live activity remain obvious.

---

## Stack

| Layer | Technology | Reason |
|---|---|---|
| **Daemon** | Rust + `windows` crate | Native ConPTY and Windows named-pipe access |
| **Shell** | `wsl.exe` + interactive bash | WSL/bash-first product contract |
| **VT parsing** | `vte` + custom `ScreenState` | Pure Rust and daemon-owned state |
| **Client** | Rust + GPUI | Native GPU-rendered Windows UI |
| **Components** | gpui-component where useful | Controls and chrome without making dock layout a P0 dependency |
| **Protocol** | Length-prefixed typed frames; JSON first | Stable envelope, debuggable implementation, binary later if measured |

`libghostty-vt` is rejected. Ghostty is written in Zig and has no Windows build. Compi does not add Zig, C ABI, or Ghostty build dependencies for VT parsing.

---

## Naming

- Product: **Compi**
- Client executable/crate: `compi`
- Daemon executable/crate: `compi-daemon`
- Risk-reduction spike: `compi-probe`
- Public domain: `compiterminal.dev`

Do not use inherited `sl` or `sl-server` names.

---

## Why Compi

Short for "computer." The name suggests a companion without describing implementation.

Windows exists, and terminal vendors still largely build around it instead of for it. Ghostty has no Windows build. Superlogical is explicitly macOS/iOS. Compi occupies the open lane: a native Windows client connected to server-owned WSL sessions, without making the user operate a visible multiplexer.
