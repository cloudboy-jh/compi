![Compi](assets/compi-readme.png)

# Compi Milestone 3

Compi pairs a native GPUI terminal window with a Windows daemon that owns persistent WSL2 Bash sessions. Closing the window detaches the client; reopening it reconnects to the same live shell and authoritative terminal state.

## Requirements

- Windows 10 version 1809 or newer
- WSL2 with a default distribution
- Rust 1.89 or newer with the MSVC Windows target
- Windows SDK build tools with `fxc.exe` on `PATH`, or `GPUI_FXC_PATH` set to its full path

WSL1 is rejected. Sessions launch `/bin/bash -i` in the default WSL2 distribution without rewriting the environment or injecting shell startup code.

## Run

```powershell
cargo build --release --bins
target\release\compi.exe
```

The desktop client creates or reconnects to a session automatically. `Ctrl+T` opens a tab, `Ctrl+W` detaches the active tab, `Ctrl+Tab` and `Ctrl+Shift+Tab` cycle tabs, `Ctrl+Shift+C`/`Ctrl+Shift+V` copy and paste, and `Ctrl+Shift+P` opens the detached-session switcher.

The diagnostic probe remains available:

Session controls:

```powershell
target\release\compi-probe.exe create
target\release\compi-probe.exe list
target\release\compi-probe.exe attach <session-id>
target\release\compi-probe.exe inspect <session-id>
target\release\compi-probe.exe kill <session-id>
target\release\compi-probe.exe shutdown
```

The daemon uses a stable per-user named pipe secured to the current Windows SID. A second daemon instance for the same user is rejected. `--instance <name>` creates an isolated development daemon.

## Current capabilities

- Multiple live WSL2 Bash sessions with one controlling client per session
- `vte` parsing owned exclusively by the daemon
- Unicode, wide and combining cells, ANSI colors, text attributes, cursor state, and terminal title
- Main and alternate screens, bracketed-paste state, editing operations, and scroll regions
- Deterministic row-preserving resize behavior
- Server-side scrollback capped at approximately 1 MiB
- Kitty image transmit, chunking, zlib decompression, placement, deletion, and compositing state
- Protocol-v2 snapshots and monotonically sequenced row deltas
- Snapshot recovery when a client detects a missing delta
- Terminal-generated status and cursor replies written back through ConPTY
- Diagnostic JSON snapshots through `compi-probe inspect`
- Daemon logs under `%LOCALAPPDATA%\Compi`

The Milestone 3 GPUI client renders terminal cells, cursor state, ANSI styling, Kitty images, tabs, selection, clipboard, keyboard input, mouse reporting, local scrollback, reconnect states, and detached-session switching. Tabs share the custom Windows titlebar, and the executable embeds the Compi desktop icon.

## Verification

```powershell
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The suite covers parser behavior, Unicode and ANSI state, editing operations, alternate screens, bounded scrollback, Kitty graphics, mirror equivalence, sequence-gap detection, snapshot recovery, representative shell and `top` behavior, multiple sessions, reconnects, resize, process exit, daemon shutdown, and slow-client backpressure.
