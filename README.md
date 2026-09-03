![Compi](assets/compi-readme.png)

# Compi

Compi is a native Windows terminal for persistent WSL2 sessions. The window is a client, while a per-user daemon owns each shell and its terminal state. Closing Compi detaches the window without stopping the work inside it.

## Product

- **Persistent by default.** Reopen Compi and reconnect to live shells, scrollback, and screen state.
- **Built for WSL2.** New sessions start in the Linux user's home directory in the default WSL2 distribution.
- **Native Windows client.** GPUI renders the terminal, tabs, titlebar, selection, images, and window controls.
- **Multiple independent sessions.** Create, detach, switch, and reconnect without adding an in-terminal multiplexer.
- **Modern terminal behavior.** Unicode, ANSI styling, alternate screens, local scrollback, mouse input, bracketed paste, and Kitty graphics are part of the core terminal model.

## How it works

```text
GPUI client  <->  per-user Compi daemon  <->  ConPTY  <->  WSL2 Bash
```

The daemon is the authority for session lifecycle and terminal state. The client can disappear and reconnect without becoming the owner of the shell process.

## Status

Compi is a pre-release Windows application with the core persistent-terminal experience, per-user daemon supervision, and a native installer implemented for dogfooding. The remaining release path is clean-profile installer qualification, broader terminal compatibility, performance validation, and repeatable signed Windows releases.

For an isolated source-tree run without Task Scheduler registration:

```powershell
cargo build --bins
.\target\debug\compi.exe --instance development
```

Build the per-user setup executable, portable ZIP, and SHA-256 manifest:

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\build-installer.ps1
```

The artifacts are written to `target\distribution`. Setup installs without elevation, registers the supervised daemon task, and exposes repair and removal through Windows Installed Apps. To inspect the installer without changing product state:

```powershell
cargo run --example compi-installer-preview -- ready
# ready | upgrade | installing | complete | error | remove
```

See [docs/NEXT_STEPS.md](docs/NEXT_STEPS.md) for the active roadmap.
