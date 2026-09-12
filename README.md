![Compi](assets/compi-readme.png)

# Compi

[![Core CI](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml/badge.svg?branch=main)](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml)

**A terminal and multiplexer in one.**

Compi is a **superterminal**: it feels like a normal native terminal, but its shells, tabs, split panes, and history live in a persistent background server. Close every window and your work keeps running. Open Compi again and pick up where you left off.

No prefix key. No nested multiplexer UI. No session-management ceremony.

## Why Compi?

A terminal gives you a shell. A multiplexer keeps that shell alive and organizes several of them. Compi treats both jobs as one product:

- **Persistent work** — closing or crashing the client does not terminate your shells.
- **Built-in multiplexing** — tabs, split panes, focus, resize, detach, and restore are native terminal actions.
- **Native experience** — native windows and shortcuts on macOS and Windows, with WSL2 as the Windows shell environment.
- **Real terminal compatibility** — Unicode, reflow, selection, mouse input, bracketed paste, and Kitty graphics.
- **Headless core** — the server owns processes and terminal state; the client is a disposable view of them.

```text
Compi client  ⇄  Compi server  ⇄  shells and PTYs
 native UI        persistent        your work
```

### Can the server run anywhere?

Not yet. Compi already has a real client/server architecture, and the server runs headlessly on macOS, Linux, and Windows. Today, however, a Compi client connects to a per-user server on the same machine. Remote transport, discovery, authentication, and tunneling are not currently shipped.

## Platform status

| Platform | Client | Server and shells |
|---|---|---|
| macOS | Native client | Native server and Unix PTYs |
| Windows | Native client | Native server and WSL2 shells |
| Linux | Not yet qualified | Headless server and Unix PTYs |

The workspace client is implemented and verified on Windows/WSL. Native macOS workspace qualification and broader performance/resource qualification remain open.

## Distribution

The **Release** workflow builds Windows x64 and Apple Silicon macOS 14+ artifacts:

- `Compi-<version>-Setup.exe`: per-user Windows installer, including repair and uninstall.
- `Compi-<version>-Windows-x64.zip`: portable Windows client and sibling daemon; extract both together. WSL2 is required for either Windows package.
- `Compi-<version>-macOS-arm64.dmg`: open the disk image and drag `Compi.app` into Applications.
- `Compi-<version>-macOS-arm64.app.zip`: alternative Mac app archive. Move the entire extracted bundle, not just its executable.
- `SHA256SUMS.txt`: checksums for the release assets.

A `v<workspace-version>` tag builds both platforms and creates one **draft** GitHub release only after both packaging checks pass. Drafts are not public downloads until published. Running the workflow manually produces downloadable Actions artifacts without creating a release; those contain platform-specific checksum manifests.

Windows artifacts are unsigned unless both signing secrets are configured. SmartScreen may warn; for an artifact you trust, **More info → Run anyway** may be available unless device policy forbids it. Mac artifacts are ad-hoc signed for integrity, but have no Developer ID signature or notarization. If Gatekeeper blocks an artifact you trust, attempt to open it, then use **System Settings → Privacy & Security → Open Anyway**; older macOS versions also offer right-click → Open. Do not disable platform security globally.

Build locally with `pwsh -File tools/build-installer.ps1` on Windows or `bash tools/build-macos.sh` on an ARM64 Mac. Both accept an optional expected version tag (`-ExpectedTag` / `--expected-tag`). Windows signing remains available through `-SigningCertificateThumbprint`, with `-RequireSigning` for builds that must not be unsigned.

## Run from source

Requires Rust and the platform build tools. Windows also requires WSL2; macOS requires Xcode with the Metal compiler available through `xcrun`.

Build the client and server:

```sh
cargo build --locked -p compi-client -p compi-daemon --bins
```

Run on macOS:

```sh
./target/debug/compi --instance development
```

Run on Windows:

```powershell
.\target\debug\compi.exe --instance development
```

Compi starts its sibling server automatically. Relaunch with the same instance name to reconnect to the existing workspace. Use `--working-directory PATH` when you intentionally want new work to start in a specific directory.

## Test

```sh
cargo test --locked -p compi-protocol -p compi-daemon -p compi-client
```

## Documentation

- [Product and technical specification](docs/Spec.md)
- [Implementation status and next steps](docs/NEXT_STEPS.md)
- [Windows terminal test recipes](docs/testcmds.md)

## License
MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Jack Horton.
