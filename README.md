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
- **Built-in observability** — Settings shows live client/server resources, render timing, cache use, and an optional per-window FPS overlay.

```text
Compi client  ⇄  Compi server  ⇄  shells and PTYs
 native UI        persistent        your work
```

### Can the server run remotely?

Yes. `compi --connect [user@]host[:port]` starts an OpenSSH stdio relay to `compi-daemon --server-stdio` on a host where the matching Compi daemon is installed. Compi opens no network listener and stores no SSH credentials; OpenSSH owns authentication, host keys, and tunneling. Automatic discovery, cloud relays, and cross-machine workspace synchronization are not shipped.

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
- `Compi-<version>-Windows-x64.zip`: portable Windows client, sibling daemon, and bundled ConPTY runtime/license; extract the entire archive together. WSL2 is required for either Windows package.
- `Compi-<version>-macOS-arm64.dmg`: open the disk image and drag `Compi.app` into Applications.
- `Compi-<version>-macOS-arm64.app.zip`: alternative Mac app archive. Move the entire extracted bundle, not just its executable.
- `SHA256SUMS.txt`: checksums for the release assets.

Both Windows packages include Microsoft's matching `conpty.dll` and `OpenConsole.exe` from pinned [`Microsoft.Windows.Console.ConPTY` 1.24.260710001](https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY/1.24.260710001), plus `ConPTY-LICENSE.txt` (MIT), beside `compi-daemon.exe`. Keep these files together: the daemon does not search PATH/current-directory or fall back to the Windows system ConPTY, which can strip Kitty graphics. Microsoft's binaries retain their original Microsoft signatures; optional Compi signing does not re-sign them.

A `v<workspace-version>` tag builds both platforms and creates one **draft** GitHub release only after both packaging checks pass. Drafts are not public downloads until published. Running the workflow manually produces downloadable Actions artifacts without creating a release; those contain platform-specific checksum manifests.

Compi's Windows executables are unsigned unless both signing secrets are configured; the bundled Microsoft runtime is always Microsoft-signed. SmartScreen may warn; for an artifact you trust, **More info → Run anyway** may be available unless device policy forbids it. Mac artifacts are ad-hoc signed for integrity, but have no Developer ID signature or notarization. If Gatekeeper blocks an artifact you trust, attempt to open it, then use **System Settings → Privacy & Security → Open Anyway**; older macOS versions also offer right-click → Open. Do not disable platform security globally.

Build locally with `pwsh -File tools/build-installer.ps1` on Windows or `bash tools/build-macos.sh` on an ARM64 Mac. Both accept an optional expected version tag (`-ExpectedTag` / `--expected-tag`). Windows signing remains available through `-SigningCertificateThumbprint`, with `-RequireSigning` for builds that must not be unsigned.

## Run from source

Requires Rust and the platform build tools. Windows also requires WSL2; macOS requires Xcode with the Metal compiler available through `xcrun`.

On Windows, prepare the pinned Microsoft runtime **before building or testing** (requires PowerShell 7 and HTTPS access to NuGet for the first download):

```powershell
pwsh -File tools/prepare-conpty.ps1
```

Preparation checks the pinned archive SHA256 and Microsoft Authenticode signatures, reuses only verified cached content, and stages `conpty.dll`, `OpenConsole.exe`, and `ConPTY-LICENSE.txt` in `target/conpty-runtime/x64`. The archive cache is `target/conpty-runtime/cache/`; the version, digest, package-signature provenance, and upstream license notice are recorded in `tools/prepare-conpty.ps1`. `-Architecture arm64` prepares the upstream ARM64 pair for a native ARM64 build; distributed Windows artifacts remain x64. Cargo copies the staged files beside the daemon and test executables, including custom target directories/profiles and explicit target triples. Compilation without preparation is allowed, but Windows terminal startup fails clearly if the bundled runtime is absent. The Windows installer build and both Windows CI workflows run preparation automatically.

Build and run on macOS:

```sh
cargo build --locked -p compi-client -p compi-daemon --bins
./target/debug/compi --instance development
```

Build and run an isolated source preview on Windows:

```powershell
cargo build --locked --target-dir target\compi-preview -p compi-client -p compi-daemon --bins
.\target\compi-preview\debug\compi.exe --instance compi-preview
```

The daemon keeps shells alive after the client closes, so Windows cannot overwrite a running `compi-daemon.exe`. If Cargo reports `Access is denied`, the build failed: do **not** launch an existing executable and assume it contains your changes. Use a fresh target directory and instance name for another preview, or first shut down only a disposable preview daemon whose shells you no longer need. Opening an old tab reconnects to its old shell; create a new tab in the newly built instance to load the current shell bridge.

Compi starts its sibling server automatically. Relaunch with the same instance name to reconnect to the existing workspace. Use `--working-directory PATH` when you intentionally want new work to start in a specific directory.

To open the native client against an SSH host:

```sh
compi --connect dev@example.com:2222
```

The headless diagnostic client accepts the same `--connect` and optional `--instance` options. For example, `compi-probe --connect dev@example.com workspace` prints the remote hierarchy and lifecycle state, while its session, tab, pane, surface, soak, and shutdown commands operate through the same protocol. The remote host must provide `compi-daemon` on `PATH`.

## Files and projects in the terminal

- **Browse files:** `Ctrl+Shift+E` on Windows (`Cmd+Shift+E` on macOS), or “Browse files in terminal pane” in the command palette. The tree replaces only the focused pane's terminal view; it is not an editor or sidebar. Folder and file icons, branch guides, and single-click disclosure arrows show the expanded hierarchy; selection and hover highlight the item rather than the entire pane width. Double-clicking a folder row also expands it. `/` or `Ctrl+F` searches paths below the current folder. Arrow keys select, Enter expands a folder or copies a file path, `C` copies the selected path outside search (use `Ctrl+C` while searching), and Esc returns to the terminal.
- **Change the same shell's directory:** At an interactive Bash/Zsh prompt, run `compi tree`. Select a folder and use **Enter folder** or `Ctrl+Enter`; the shell itself performs `cd`. Esc cancels. The keyboard shortcut opens browsing and copying without changing the shell: a running program or partially typed command must never receive an injected `cd`.
- **Jump to visited directories:** Run `compi z` (or `compi jump`) from the prompt, type to filter, choose with arrows, and press Enter. This integration does not install a bare `z` command; `z orangebox` is not a Compi command. “Jump to project directory” in the palette opens the same picker for path copying; only the shell-origin picker changes cwd. Project history follows OSC 7 cwd reports, including ordinary `cd` in supported interactive shells.
- New tabs and splits inherit the focused pane's reported cwd. Right-click a tab and choose **Detach Split to New Tab** to move that tab's focused split without restarting either shell; the option is disabled on an unsplit tab. Mouse dragging highlights terminal text; `Ctrl+C` copies a selection instead of interrupting the shell. `Ctrl++` increases font size on Windows; `Ctrl+Enter` is distinct from Enter when a terminal program enables Kitty keyboard disambiguation.

Compi installs its bundled Bash/Zsh shell integration into the shell's home directory when starting supported default shells (Windows/WSL2, macOS, and remote Unix daemons); it does not edit your startup files. Explicit custom executable/argument profiles are left untouched. A Bash login profile that replaces `PROMPT_COMMAND` may suppress cwd reports after ordinary `cd`; `compi tree` and `compi z` report the shell's current directory before opening their picker, so they start from the directory you are in even without the prompt hook.

On Windows/WSL2, the tree can browse a shell directory linked into a mounted Windows drive. If the WSL share cannot follow the link, Compi resolves that directory in WSL and retries through its Windows path.

## Appearance and typography

Settings includes 40 bundled whole-application themes, five interface-font choices, and four terminal-font choices. Interface fonts apply globally to Compi's chrome and controls. Terminal fonts apply independently to the fixed-width terminal grid.

The native system fonts remain the defaults. Interface selection uses a stable bundled ID, while terminal selection updates the existing `font.family` setting:

```toml
[appearance]
ui_font = "ibm-plex-sans"

[font]
family = "JetBrains Mono"
```

Terminal presets include the platform default, JetBrains Mono, IBM Plex Mono, and Atkinson Hyperlegible Mono. Custom installed families remain supported through `font.family`. Bundled font notices and SIL Open Font License 1.1 text are in [`crates/compi-client/fonts/ATTRIBUTION.txt`](crates/compi-client/fonts/ATTRIBUTION.txt).

## Test

```sh
cargo test --locked -p compi-protocol -p compi-daemon -p compi-client
```

## Documentation

- [Product and technical specification](docs/Spec.md)
- [Next steps](docs/NEXT_STEPS.md)
- [Completed work and verification history](docs/COMPLETED.md)
- [Windows terminal test recipes](docs/testcmds.md)

## License
MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Jack Horton.
