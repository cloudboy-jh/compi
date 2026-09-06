![Compi](assets/compi-readme.png)

# Compi

Compi is a native terminal workspace backed by a persistent server. The product baseline makes macOS and Windows first-class: native shells on Mac, WSL2 shells on Windows, and a headless native Linux server. Closing the client leaves live work running.

## Product baseline

[docs/Spec.md](docs/Spec.md) is the authoritative product and technical contract. It calls for server-owned sessions, tabs, and split panes; remembered client presentation; a command palette; and selectable whole-app themes. The default is neutral Dark Glass with an acid-green accent, glass limited to sidebar/window chrome, and an opaque terminal canvas.

These are requirements, not a claim that the complete workspace is implemented. The documentation baseline is established; Phase 0 implementation-contract work has not started. See [docs/NEXT_STEPS.md](docs/NEXT_STEPS.md) for the handoff.

## Current implementation

The code currently implements a Windows GPUI client and per-user Windows daemon hosting WSL2 Bash shells through ConPTY. It includes detached shell ownership, terminal replicas, Unicode/reflow, selection, mouse input, bracketed paste, and Kitty graphics.

```text
GPUI client  <->  per-user Compi daemon  <->  ConPTY  <->  WSL2 Bash
```

The daemon owns shell lifecycle and authoritative terminal state. Current server "sessions" each represent a shell; client tabs are not yet the persistent session/tab/pane hierarchy in the new specification. macOS support, Unix hosting, workspace splits, the command palette, and the theme picker remain implementation work.

## Run the current Windows build

Requires Windows 10 version 2004 (build 19041) or newer, a default WSL2 distribution, and the Rust/Windows SDK build tooling. There is no working native Mac launch path yet.

For an isolated source-tree run without Task Scheduler registration:

```powershell
cargo build --bins
.\target\debug\compi.exe --instance development
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
cargo run --example compi-installer-preview -- ready
# ready | upgrade | installing | complete | error | remove
```

## Documentation

- [Product and technical specification](docs/Spec.md) — authoritative cross-platform baseline.
- [Next steps](docs/NEXT_STEPS.md) — Phase 0 handoff; implementation not started.
- [Windows terminal test recipes](docs/testcmds.md) — existing regression procedures to adapt during migration.
- [Historical Windows acceptance results](docs/ACCEPTANCE_RESULTS_2026-09-02.md) — dated evidence, not qualification of the new baseline.
