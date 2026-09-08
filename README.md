![Compi](assets/compi-readme.png)

# Compi

Compi is a native terminal workspace backed by a persistent server. The product baseline makes macOS and Windows first-class: native shells on Mac, WSL2 shells on Windows, and a headless native Linux server. Closing the client leaves live work running.

## Product baseline

[docs/Spec.md](docs/Spec.md) is the authoritative product and technical contract. It calls for server-owned sessions, tabs, and split panes; remembered client presentation; a command palette; and selectable whole-app themes. The default is neutral Dark Glass with an acid-green accent, glass limited to sidebar/window chrome, and an opaque terminal canvas.

The Phase 4 workspace client is implemented, with native Windows/WSL verification. Native Mac workspace qualification and the broader Phase 5 daily-use/resource qualification remain open. See [docs/NEXT_STEPS.md](docs/NEXT_STEPS.md) for the exact evidence and remaining gates.

## Current implementation

The code implements a native GPUI client on macOS and Windows, plus a headless server on macOS/Linux/Windows. Unix hosts use real PTYs through `portable-pty`; Windows retains its suspended-before-job ConPTY backend for WSL2 descendant ownership. It includes detached shell ownership, terminal replicas, Unicode/reflow, selection, mouse input, bracketed paste, and Kitty graphics.

```text
GPUI client <-> per-user daemon <-> portable-pty <-> native Mac/Unix shell
                               \-> ConPTY       <-> Windows/WSL2 Bash
```

The daemon owns shell lifecycle, authoritative terminal state, and the durable workspace hierarchy. The client presents **Workspace → Terminal tabs → Panes**: a user-facing workspace is the server's `Session`, not another navigation layer. Primary top tabs contain terminal split trees; the full workspace sidebar stays hidden until summoned. Protocol v9 retains durable mutations, receipts, and process-lifetime identities, and adds measured split requests, path-addressed dividers, invocation-local launch context, and authoritative Clear scrollback.

The Cargo workspace has three product crates: `compi-protocol` owns the shared process contract and local transport, `compi-daemon` owns shell lifecycle, terminal semantics, persistence, and OS hosting, and `compi-client` owns daemon consumption, replicas, the diagnostic probe, and the GPUI application. The daemon's production graph contains no graphics dependencies; the installer remains isolated under `installer/bootstrapper`.

### Workspace controls

- Create, rename, reorder, hide, and restore terminal tabs; browse named workspaces in the optional sidebar.
- Split right/down, move pane focus, and drag dividers. Small windows scroll the complete layout instead of rewriting it or automatically collapsing the sidebar.
- Drag a tab out to create a native window, or drop it into another Compi window. The same tab, panes, and processes move; ordinary same-instance launches share one GUI host.
- Window close and tab hide detach without ending processes. End, restart, and confirmed removal are separate actions; exited output remains available for inspection.
- Window slots remember geometry, navigation, hidden tabs, sidebar width, zoom, accepted theme, and provably valid viewport anchors. Every new/reopened window starts with its sidebar closed.
- **Appearance…** and the palette's **Change theme** open one live-preview picker. Dark Glass is the default; Warm Carbon is optional. Escape cancels, Enter/Apply accepts. Glass is limited to chrome/sidebar, with opaque fallbacks; terminal canvases remain opaque.

Use matching client/daemon builds. Protocol v9 intentionally rejects older live daemons rather than interrupting their processes. Try a separate `--instance phase4` when an older daemon still owns valuable work.

## Check the product boundaries

On macOS, Linux, or Windows:

```sh
cargo test --locked -p compi-protocol -p compi-daemon -p compi-client
cargo test --locked -p compi-daemon --test terminal_compatibility
```

The second command exercises engine-to-wire conversion and replica recovery. On macOS/Linux, exercise real PTYs, private sockets, resize, reconnect, exit, descendant cleanup, and daemon restart with:

```sh
cargo test --locked -p compi-daemon --test unix_daemon_integration
```

## Run the native Mac client

Requires Rust and Xcode with its Metal compiler available through `xcrun`.

```sh
cargo build --locked -p compi-client -p compi-daemon --bins
./target/debug/compi --instance development
```

The client starts its sibling daemon automatically. The default shell comes from `SHELL` or the user account, with native interactive/login startup. Close the window, then run the same command to return to the live shell.

Use `--working-directory /absolute/project/path` to explicitly create new work in that directory. Omit it on ordinary relaunch to reconnect instead of creating another shell.
Terminal typography is read from `~/Library/Application Support/Compi/config.toml` on macOS or `%LOCALAPPDATA%\Compi\config.toml` on Windows. The file is optional and versioned:

```toml
version = 1

[font]
family = "Menlo"
size = 14.0
line_height = 1.35
fallbacks = ["JetBrainsMono Nerd Font Mono"]

[appearance]
theme = "dark-glass"

[layout]
sidebar_width = 280.0

[limits]
scrollback_lines = 10000
graphics_bytes = 4194304

[clipboard]
policy = "deny" # Terminal-initiated OSC 52 writes; explicit Copy/Paste still work.
```

Use `--config PATH` or `COMPI_CONFIG_FILE` to select another file. Invocation-local `--font-family`, `--font-size`, `--line-height`, `--theme`, and `--sidebar-width` override presentation without rewriting configuration or remembered defaults. Independent invalid presentation fields are diagnosed; invalid selected launch configuration blocks new launches instead of silently choosing another executable.

Launch configuration uses `[shell]` (`executable`, `args`, `login`, `working_directory`, `distribution`), `[environment]`, and optional `[profiles.NAME]` selected by `default_profile`. Explicit program arguments remain literal; environment overrides are not persisted in workspace metadata. Named profiles override base launch fields/environment. `[keybindings]` maps command IDs (for example `split_right`) to shortcuts; an empty binding unbinds a command. Scrollback also retains its fixed 1 MiB byte bound; supported graphics storage is 0–4 MiB.


Mac shortcuts include Cmd-T/W (new/hide tab), Cmd-D / Cmd-Shift-D (split right/down), Cmd-B (sidebar), Cmd-Shift-P (palette), Cmd-C/V, and Cmd-Q. Windows uses Ctrl-Shift-T/W, Ctrl-Shift-D/E, Ctrl-Shift-B, and Ctrl-Shift-P. Windows Ctrl-C copies a nonempty selection, otherwise sends terminal interrupt; Ctrl-Shift-C/V are explicit copy/paste. Palette/picker navigation does not leak into the terminal.

Mac metadata and diagnostic logs live under `~/Library/Application Support/Compi`; Linux uses `$XDG_STATE_HOME/compi` or `~/.local/state/compi`. Unix sockets use `$XDG_RUNTIME_DIR/compi` or a private `/tmp/compi-UID` directory. `COMPI_DATA_DIR` and `COMPI_RUNTIME_DIR` override those locations for isolated runs; overrides must be absolute, current-user-owned private directories.

For Linux headless use, or a diagnostic Mac console:

```sh
cargo build --locked -p compi-daemon --bin compi-daemon
cargo build --locked -p compi-client --example compi-probe
./target/debug/examples/compi-probe --instance development start
# Ctrl+] detaches without ending the shell.
```

## Run the current Windows build

Requires Windows 10 version 2004 (build 19041) or newer, a default WSL2 distribution, and the Rust/Windows SDK build tooling.

For an isolated source-tree run without Task Scheduler registration:

```powershell
cargo build --bins
.\target\debug\compi.exe --instance development
```

Build the Windows daemon alone, without GPUI or its shader compiler:

```powershell
cargo build --locked --release -p compi-daemon --bin compi-daemon
```

Pass a project path directly or with `--working-directory`; new tabs inherit valid OSC 7 directory reports from the active shell:

```powershell
.\target\debug\compi.exe --instance development --working-directory C:\src\project
```

## Existing Windows distribution tooling

These commands remain available for the current implementation. Windows signing and installer qualification do not gate the new cross-platform foundation.

Build the per-user setup executable, portable ZIP, and SHA-256 manifest:

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\build-installer.ps1
```

Artifacts are written to `target\distribution`. Setup runs as the target non-administrator user, registers the supervised daemon task, and exposes repair and removal through Windows Installed Apps. Upgrades warn that active shells will end; removal deletes binaries, task registration, logs, and session metadata.

Tagged releases use `.github/workflows/windows-release.yml` and require the repository secrets `WINDOWS_SIGNING_CERTIFICATE_BASE64` and `WINDOWS_SIGNING_CERTIFICATE_PASSWORD`. A local signed build uses the same guarded path:

```powershell
.\tools\build-installer.ps1 -ExpectedTag v0.1.0 `
  -SigningCertificateThumbprint <thumbprint> -RequireSigning
```

Run the mixed sustained-output and lifecycle resource soak against the exact release binaries:

```powershell
cargo build --release --bins --example compi-probe
.\tools\soak-release.ps1 -Minutes 30
```

Preview installer states without installing:

```powershell
cargo run --manifest-path installer/bootstrapper/Cargo.toml --example compi-installer-preview -- ready
# ready | upgrade | installing | complete | error | remove
```

## Documentation

- [Product and technical specification](docs/Spec.md) — authoritative cross-platform baseline.
- [Next steps](docs/NEXT_STEPS.md) — extraction and native Mac implementation evidence, platform qualification gaps, and workspace handoff.
- [Windows terminal test recipes](docs/testcmds.md) — existing regression procedures to adapt during migration.
- [Historical Windows acceptance results](docs/ACCEPTANCE_RESULTS_2026-09-02.md) — dated evidence, not qualification of the new baseline.
