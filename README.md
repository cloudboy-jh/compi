![Compi](assets/compi-readme.png)

# Compi

Compi is a native terminal workspace backed by a persistent server. The product baseline makes macOS and Windows first-class: native shells on Mac, WSL2 shells on Windows, and a headless native Linux server. Closing the client leaves live work running.

## Product baseline

[docs/Spec.md](docs/Spec.md) is the authoritative product and technical contract. It calls for server-owned sessions, tabs, and split panes; remembered client presentation; a command palette; and selectable whole-app themes. The default is neutral Dark Glass with an acid-green accent, glass limited to sidebar/window chrome, and an opaque terminal canvas.

These are requirements, not a claim that the complete workspace client is implemented. Phases 0–3 now provide the shared process contract, native daemon/runtime foundation, and durable daemon-owned workspace hierarchy. Phase 4 client presentation, remembered windows, split layout, command palette, and themes remain outstanding. See [docs/NEXT_STEPS.md](docs/NEXT_STEPS.md).

## Current implementation

The code implements a native GPUI client on macOS and Windows, plus a headless server on macOS/Linux/Windows. Unix hosts use real PTYs through `portable-pty`; Windows retains its suspended-before-job ConPTY backend for WSL2 descendant ownership. It includes detached shell ownership, terminal replicas, Unicode/reflow, selection, mouse input, bracketed paste, and Kitty graphics.

```text
GPUI client <-> per-user daemon <-> portable-pty <-> native Mac/Unix shell
                               \-> ConPTY       <-> Windows/WSL2 Bash
```

The daemon owns shell lifecycle, authoritative terminal state, and the durable workspace hierarchy: sessions contain ordered tabs, tabs contain split trees, and pane leaves reference stable surfaces. Protocol v8 separates every identity type, adds optimistic durable mutations and receipts, and keys attachments and screen traffic to a server generation plus process lifetime. The current GPUI client consumes the new surface model but still presents its pre-Phase-4 tab/switcher UI; full split rendering and remembered client state remain later work.

The Cargo workspace has three product crates: `compi-protocol` owns the shared process contract and local transport, `compi-daemon` owns shell lifecycle, terminal semantics, persistence, and OS hosting, and `compi-client` owns daemon consumption, replicas, the diagnostic probe, and the GPUI application. The daemon's production graph contains no graphics dependencies; the installer remains isolated under `installer/bootstrapper`.

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
```

Use `--config PATH` or `COMPI_CONFIG_FILE` to select another file. Invocation-local `--font-family`, `--font-size`, and `--line-height` values take precedence. Invalid fields are ignored independently and reported in the client without discarding valid siblings.


Mac shortcuts include Cmd-T (new tab), Cmd-W (detach tab), Cmd-C/V (copy/paste), Cmd-Shift-P (session switcher), and Cmd-Q (quit client). Ctrl-C remains terminal interrupt.

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
