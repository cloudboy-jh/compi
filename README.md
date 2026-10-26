![Compi](assets/compi-readme.png)

# Compi

**A native terminal workspace that any agent can drive.**

You and your agent share the same workspace. It can arrange terminals, run work, inspect results and open changes for you to review, all in the workspace you're looking at. Close the window and your shells keep running.

Native on Windows (WSL2) and macOS. [Download Compi](https://github.com/cloudboy-jh/compi/releases).

[![Core CI](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml/badge.svg?branch=main)](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml)

![Compi working on itself: an omp agent reviews Windows terminal resource cleanup beside the live TorchedLeek host Monitor and the corresponding read-only Rust commit diff](assets/compi-workspace.png)

## Agent control

An agent running in Compi can do more than use its own shell. Through the `compi` CLI it can operate the workspace around it: make room for another task, read another terminal's output, open a local app in a browser pane or bring up a diff for your review.

Workspace controls and palette actions share one command interface, available to any agent that can run shell commands. Compi does not choose your agent or orchestrate its work.

### Inspect, act, read

From a Compi shell in this repository, an agent can inspect its pane, run the protocol tests and read their output:

```sh
compi --json pane show
compi run -- cargo test --locked -p compi-protocol
compi pane capture --scope scrollback --format text
```

`run` submits the command without waiting; capture reads the output available so far. The agent can capture again to check progress. Inside a supported Compi shell, the CLI is on `PATH` and knows its instance and pane. Stable IDs let it target other panes, and `--json` provides structured results.

`compi command list` exposes the palette's actions, and `compi command run ID` invokes them. Presentation actions use an open Compi window; destructive confirmations remain in place. See the [CLI reference](docs/user/USAGE.md#command-line-workspace-control) for the full interface.

## Know when a program needs you

See when a program is working, waiting on you, done or failed, even in a background tab. Hover its status indicator for the message, progress and what it needs from you.

Supporting programs report this through the [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status) (OSC 7501). Compi displays their reports rather than guessing from terminal output, and exposes the same status through the CLI. [How program status works](docs/user/USAGE.md#program-status).

## Beyond the terminal

- **Persistent workspaces.** Organize shells in tabs, splits and floating panes. Rearrange them without restarting their programs, or connect to a remote Compi server over SSH.
- **Edit and review.** Open files beside your work, inspect diffs, stage hunks and commit. Switch between a file view and its running shell.
- **Browser and files.** Keep a local app, readable Markdown or a file preview in the workspace instead of switching windows.
- **Monitor.** Inspect processes and resource usage on the host or a WSL distribution.
- **Appearance.** Choose themes, fonts and layouts, with prompt styles from Oh My Posh or Starship.

## Install

Get Compi from [GitHub Releases](https://github.com/cloudboy-jh/compi/releases).

- **Windows:** x64, WSL2 required. Installer or portable archive.
- **macOS:** Apple Silicon, macOS 14+. Move the app into Applications.

Windows builds are unsigned and macOS builds are ad-hoc signed, so your OS may warn on first launch. Linux runs the headless server; its window is not yet supported.

See [installation](docs/user/INSTALLATION.md) for setup and [release notes](docs/user/release-notes.md) for limitations and version-specific upgrade warnings.

## Development

Requires Rust and platform build tools. Windows also needs WSL2 and PowerShell 7; macOS needs Xcode with the Metal compiler.

```bash
pwsh -File tools/prepare-conpty.ps1   # Windows only, once
cargo dev                             # isolated preview that rebuilds on save
cargo test --locked -p compi-protocol -p compi-daemon -p compi-client -p compi-dev
```

Shells in the preview survive client rebuilds. See the [development guide](docs/dev/DEVELOPMENT.md).

## Documentation

- [Usage and CLI reference](docs/user/USAGE.md)
- [Configuration](docs/user/CONFIGURATION.md)
- [Development](docs/dev/DEVELOPMENT.md) and [architecture](docs/dev/Spec.md)

## License

[MIT](LICENSE). Copyright (c) 2026 Jack Horton.
