![Compi](assets/compi-readme.png)

# Compi

**The terminal you can live in.**

Shells that keep running when you close the window, with an editor, diffs and Git beside them. Native on Windows (WSL2) and macOS.

[![Core CI](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml/badge.svg?branch=main)](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml)

![Compi working on itself: omp in a terminal pane checks the 0.1.9 release notes against the Git log, beside the Settings pane with its theme grid and an editor pane showing the diff of those notes](assets/compi-workspace.png)

## Terminal

- Shells live in a background server: close the window, rebuild the client or reconnect over SSH, and they are still running.
- Workspaces, tabs and split panes. Drag a pane to move it, swap it or send it to another tab. Float any pane.
- Layout presets, prompt styles from Oh My Posh or Starship, themes and fonts.

## Edit

- Open files right where you are: `compi edit src/main.rs:42`, Ctrl-click a path in any output, or the command palette show them in the terminal's own pane, and Ctrl+Shift+E flips back to the shell, still running.
- A native code editor with syntax highlighting and search. Saves never overwrite changes made outside it, and unsaved edits survive a crash.

## Review

- Every uncommitted change, unstaged and staged, side by side or inline, with the changed words highlighted.
- One tab per file: switch between Edit and Diff with Ctrl+Shift+D and stay on the same line.
- Git marks in the editor's gutter as you type. Jump between changes with Alt+F5, open a change from the terminal with `compi diff path:line`, or a past commit with `compi diff HEAD~1`.

## Commit

- Stage files or single hunks, write the message, commit or amend.
- See how far you are ahead or behind, switch or create branches, and fetch, pull or push in a terminal beside the editor, so logins just work.

## Built for scripts and agents

The `compi` command opens panes, sends input, reads output, and opens files and diffs, so an agent can drive the same workspace you're looking at.

Programs that speak the [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status) (OSC 7501) tell Compi what they're doing. The tab shows when an agent or build is blocked on you, still working, done or failed, even in a background tab, and hovering it says why.

## Download

Get Compi from [GitHub Releases](https://github.com/cloudboy-jh/compi/releases).

- **Windows:** x64, WSL2 required. Installer or portable archive.
- **macOS:** Apple Silicon, macOS 14+. Move the app into Applications.

Windows builds are unsigned and macOS builds are ad-hoc signed, so your OS may warn on first launch. Linux runs the headless server; its window is not yet supported.

## Development

Requires Rust and platform build tools. Windows also needs WSL2 and PowerShell 7; macOS needs Xcode with the Metal compiler.

```bash
pwsh -File tools/prepare-conpty.ps1   # Windows only, once
cargo dev                             # isolated preview that rebuilds on save
cargo test --locked -p compi-protocol -p compi-daemon -p compi-client -p compi-dev
```

Shells in the preview survive client rebuilds. See the [development guide](docs/dev/DEVELOPMENT.md).

## Documentation

- Users: [installation](docs/user/INSTALLATION.md), [usage](docs/user/USAGE.md), [configuration](docs/user/CONFIGURATION.md), [release notes](docs/user/release-notes.md)
- Developers: [development](docs/dev/DEVELOPMENT.md), [architecture](docs/dev/Spec.md), [next steps](docs/dev/NEXT_STEPS.md), [verification history](docs/dev/COMPLETED.md), [distribution](docs/dev/DISTRIBUTION.md), [release flow](docs/dev/RELEASE-FLOW.md), [terminal test recipes](docs/dev/testcmds.md)

## License

[MIT](LICENSE). Copyright (c) 2026 Jack Horton.
