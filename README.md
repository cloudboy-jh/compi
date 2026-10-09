![Compi](assets/compi-readme.png)

# Compi

[![Core CI](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml/badge.svg?branch=main)](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml)

Compi is a native terminal workspace for Windows and macOS.

![Compi with three panes: an agent tracing a codebase, a second agent building a snake game, and a live view of Compi's own client and daemon memory](assets/compi-workspace.png)

- Shells keep running when you close the window.
- Workspaces, tabs, and split panes.
- Floating terminals and layout presets.
- A `compi` command that lets scripts and agents open panes, send input, and read output.
- Shell prompt styles from Oh My Posh or Starship.
- Remote connections over SSH.
- Themes, fonts, and appearance settings.

## Download

Get Compi from [GitHub Releases](https://github.com/cloudboy-jh/compi/releases).

- **Windows:** x64, with WSL2 required. Use the installer or extract the complete portable archive.
- **macOS:** Apple Silicon, macOS 14+. Move the complete app into Applications. Native workspace qualification remains open.

Windows builds are unsigned and macOS builds are ad-hoc signed unless publisher credentials are configured; OS trust warnings may appear. Linux has a headless server, but its graphical client is not yet qualified.

## Development

Requires Rust and platform build tools. Windows also needs WSL2 and PowerShell 7; macOS needs Xcode with the Metal compiler.

On Windows, prepare the bundled terminal runtime first:

```bash
pwsh -File tools/prepare-conpty.ps1
```

Then start the isolated development preview:

```bash
cargo dev
```

Client edits rebuild and relaunch the preview while its shells keep running. Explicit daemon restarts end dev shells. See the [development guide](docs/dev/DEVELOPMENT.md) for controls and cleanup scope.

Run tests:

```bash
cargo test --locked -p compi-protocol -p compi-daemon -p compi-client -p compi-dev
```

## Documentation

### User guides

- [Installation, updates, and repair](docs/user/INSTALLATION.md)
- [Usage and SSH](docs/user/USAGE.md)
- [Appearance and configuration](docs/user/CONFIGURATION.md)
- [Release notes](docs/user/release-notes.md)

### Developer notes

- [Development](docs/dev/DEVELOPMENT.md)
- [Packaging and distribution](docs/dev/DISTRIBUTION.md)
- [Release flow](docs/dev/RELEASE-FLOW.md)
- [Architecture and specification](docs/dev/Spec.md)
- [Next steps](docs/dev/NEXT_STEPS.md) and [verification history](docs/dev/COMPLETED.md)
- [Terminal test recipes](docs/dev/testcmds.md)
- [Installer and updates plan](docs/dev/INSTALLER_UPDATES_PLAN.md)
- [Historical acceptance results](docs/dev/ACCEPTANCE_RESULTS_2026-09-02.md)

## License

[MIT](LICENSE). Copyright (c) 2026 Jack Horton.
