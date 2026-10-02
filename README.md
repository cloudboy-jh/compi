![Compi](assets/compi-readme.png)

# Compi

[![Core CI](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml/badge.svg?branch=main)](https://github.com/cloudboy-jh/compi/actions/workflows/core-ci.yml)

**A terminal and multiplexer in one.**

Compi is a **superterminal**: it feels like a normal native terminal, but its shells, tabs, split panes, and history live in a persistent background server. Close every window and your work keeps running. Open Compi again and pick up where you left off.

No prefix key. No nested multiplexer UI. No session-management ceremony.

![Compi workspace](assets/compi-workspace.png)

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
- `Compi-<version>-Windows-x64.zip`: portable Windows launcher, versioned client/daemon payload, updater, and bundled ConPTY runtime/license. Extract the entire archive together; do not copy individual executables. WSL2 is required for either Windows package.
- `Compi-<version>-macOS-arm64.dmg`: open the disk image and drag `Compi.app` into Applications.
- `Compi-<version>-macOS-arm64.app.zip`: alternative Mac app archive. Move the entire extracted bundle, not just its executable.
- `Compi-<version>-Windows-x64-update.zip`: complete Windows payload consumed by the updater, not a standalone installer.
- `compi-update-<platform>.json`: Ed25519-signed release metadata covering the exact package size/hash, release notes, platform, and qualified daemon versions.
- `SHA256SUMS.txt`: checksums for the release assets.

Both Windows packages include Microsoft's matching `conpty.dll` and `OpenConsole.exe` from pinned [`Microsoft.Windows.Console.ConPTY` 1.24.260710001](https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY/1.24.260710001), plus `ConPTY-LICENSE.txt` (MIT), beside `compi-daemon.exe`. Keep these files together: the daemon does not search PATH/current-directory or fall back to the Windows system ConPTY, which can strip Kitty graphics. Microsoft's binaries retain their original Microsoft signatures; optional Compi signing does not re-sign them.

A `v<workspace-version>` tag builds both platforms and creates one **draft** GitHub release only after the portable/copied-bundle smoke checks and metadata signing pass. Publication remains deliberate; stable update discovery ignores drafts and prereleases. A manually dispatched workflow creates dogfood Actions artifacts, not a public release.

Official tag builds require the updater verification key and signed release metadata. Windows publisher signing and macOS Developer ID signing/notarization remain optional, matching the previous release policy: without those credentials, Windows artifacts are unsigned and macOS artifacts are ad-hoc signed, so OS trust warnings remain. A build without an updater key disables public updates rather than accepting unsigned metadata. Microsoft's bundled runtime retains its Microsoft signatures.

Build locally with `pwsh -File tools/build-installer.ps1` on Windows or `bash tools/build-macos.sh` on an ARM64 Mac. Both accept an expected version tag (`-ExpectedTag` / `--expected-tag`). Official update-key enforcement uses `-RequireUpdateKey` / `--require-update-key`. For deliberately publisher-signed builds, also use `-RequireSigning` or `--require-signing` with `COMPI_MACOS_SIGNING_IDENTITY` and a `COMPI_MACOS_NOTARY_PROFILE` configured through `notarytool`.

### Updates and data retention

Open **Settings → Updates** or run **Check for Updates** from the command palette. Automatic checks are **Never**, **On launch**, or **Daily** (default); they never download, install, close windows, or restart a daemon silently. The surface shows client/daemon versions, signed notes, byte progress, affected work, deferral, and recovery actions.

Compatible activation relaunches clients into their exact saved views while retaining the existing daemon and shell lifetimes. A required local daemon restart needs explicit current-work consent and ends its shells; remote daemons are never restarted by the local updater. If the new build then fails to attach, rollback stops the replacement daemon it started only while that daemon is idle; otherwise rollback waits and reports which shells to close. Legacy 0.1.2 migration requires a deliberate old-install stop because its MSI removal hook cannot preserve live work.

Windows uses `versions/<version>` plus `selection.json`; keep old payloads while their daemon/supervisor still runs. macOS updates replace the complete writable app bundle, not an app on a mounted DMG. Failed activation retains a prior version and recovery journal; success requires a real new-build attachment receipt.

Upgrade, repair, rollback, and default uninstall preserve workspaces, settings, and custom themes. Maintenance offers a separate unchecked managed-data removal choice. External projects/configuration/theme sources and WSL distributions are not removed.

### Release trust configuration

Set repository variable `COMPI_UPDATE_PUBLIC_KEY` to the base64-encoded 32-byte Ed25519 public key and secret `COMPI_UPDATE_SIGNING_KEY` to its base64-encoded 32-byte private seed. Generate a key pair with `compi-release-metadata keygen --private-key <owner-only-file>`; never commit the seed or reuse the disposable qualification key.

Optional Windows CI secrets: `WINDOWS_SIGNING_CERTIFICATE_BASE64`, `WINDOWS_SIGNING_CERTIFICATE_PASSWORD` (configure both or neither). Optional macOS CI secrets: `MACOS_SIGNING_CERTIFICATE_BASE64`, `MACOS_SIGNING_CERTIFICATE_PASSWORD`, `MACOS_SIGNING_IDENTITY`, `APPLE_NOTARY_ID`, `APPLE_NOTARY_TEAM_ID`, `APPLE_NOTARY_PASSWORD` (configure all six or none). Native qualification receipts bind metadata to exact artifact hashes; mixed daemon versions are advertised only when qualified.

Current implementation evidence and outstanding native/MSI/production-trust gates are recorded in [Completed work](docs/COMPLETED.md). Local test signatures do not establish production OS trust.

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
- New tabs and splits inherit the focused pane's reported cwd. A terminal tab shows its pane's name, both names for two panes, or the first name with a `3+` (or larger) count; a manually named tab keeps its name and shows a pane-count badge when split. Hover over a tab for a compact view of its pane names and reported directories; a split tab numbers the panes, while a single-pane tab skips the redundant heading. Right-click the tab for a compact, scrollable terminal list in layout order, then select a terminal to reveal a small two-action bubble beneath its name inside the list. **Detach** moves that pane's running shell to a new tab without restarting it; **End…** opens a separate confirmation before ending its process tree and retaining the final grid. Detach is disabled for a lone pane. Arrow keys and Enter open the selected terminal's bubble; Tab switches between enabled actions, and Escape returns to the compact list. The command palette's detach action still targets the focused pane. Mouse dragging highlights terminal text; `Ctrl+C` copies a selection instead of interrupting the shell. `Ctrl++` increases font size on Windows; `Ctrl+Enter` is distinct from Enter when a terminal program enables Kitty keyboard disambiguation.

Compi installs its bundled Bash/Zsh shell integration into the shell's home directory when starting supported default shells (Windows/WSL2, macOS, and remote Unix daemons); it does not edit your startup files. Explicit custom executable/argument profiles are left untouched. A Bash login profile that replaces `PROMPT_COMMAND` may suppress cwd reports after ordinary `cd`; `compi tree` and `compi z` report the shell's current directory before opening their picker, so they start from the directory you are in even without the prompt hook.

On Windows/WSL2, the tree can browse a shell directory linked into a mounted Windows drive. If the WSL share cannot follow the link, Compi resolves that directory in WSL and retries through its Windows path.

## Appearance and typography

Settings includes 41 bundled themes, including **Compi Neutral**, a restrained baseline for clear, blurred, or solid backgrounds. **Theme** selects application and terminal colors together. **Advanced → Terminal colors** defaults to **Follow theme** and allows an optional palette override; Appearance reports an active override. Existing Dark Glass selections, first-run defaults, and legacy split palettes are preserved.

**Transparent background** enables the opacity slider and **Blur background** toggle. Turning transparency off makes the window solid without forgetting opacity or blur preferences. Increase opacity on light or busy desktops, or turn transparency off for predictable contrast; very low opacity cannot guarantee readability against arbitrary wallpaper. Unsupported or reduced-transparency systems fall back to solid backgrounds automatically.

The existing theme catalog supports search, dark/light filters, favorites, previews, and scoped apply/cancel. The applied theme stays first with an inline **Current** selector, even during search or preview; favorites follow, then remaining themes alphabetically. **Import…** loads a local Zed `.json` theme family without applying or publishing it; each variant becomes a catalog entry. **More…** holds export, attribution, and removal actions. **Export selected…** writes standard Zed JSON with existing attribution, license notices, and unused fields preserved. **Remove local…** removes only an unused imported variant and its favorite, keeping sibling variants and the external source file; themes referenced by defaults, open windows, or previews are protected.

Theme files use [Zed’s published JSON format](https://zed.dev/schema/themes/v0.2.0.json): a family has `name`, `author`, and `themes`; each variant has `name`, `appearance` (`dark` or `light`), and `style`. No Compi ID, custom wrapper, or special filename is required. Import an unmodified downloaded file, such as [Zed’s One Dark/One Light family](https://raw.githubusercontent.com/zed-industries/zed/main/assets/themes/one/one.json), or export a bundled theme as a starting point. RGB/RGBA hex colors and missing/null style values are supported; missing colors use consistent light/dark defaults. Unknown fields are retained for export, not executed. Files are bounded to 2 MiB and 256 variants; invalid families and conflicting variants are rejected without partial installation.

Import copies the family into `themes` under Compi’s data directory. Copying a `.json` file directly into that directory also installs it without applying it; on Windows the directory is `%LOCALAPPDATA%\Compi\themes`. Theme code, includes, assets, and network resources are never executed or fetched. Existing managed `.compi-theme.json` imports migrate once, preserving their saved IDs, selections, favorites, colors, and attribution; verified originals are retained under `.migrated`. Transparency, opacity, and blur remain separate preferences, including when a Zed theme specifies its own background appearance.

Compact rounded-rectangle tabs use a subtle active fill, transparent inactive tabs, and a faint hover fill. Hover reveals a close button in a reserved slot without moving the label. Tab geometry, keyboard cycling, overflow, reorder, and tear-off are shared across themes.

Five interface-font choices apply globally to Compi's chrome and controls. Four terminal-font choices apply independently to the fixed-width terminal grid.

The native system fonts remain the defaults. Interface selection uses a stable bundled ID, while terminal selection updates the existing `font.family` setting:

```toml
[appearance]
ui_font = "ibm-plex-sans"
theme = "compi-neutral"
terminal_theme = "dark-glass" # remembered optional override
terminal_theme_override = false # Follow theme; true enables the override
transparent_background = true
background_effect = "clear" # clear or blurred; legacy opaque still migrates
terminal_opacity = 0.7 # remembered background opacity, not text opacity

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
