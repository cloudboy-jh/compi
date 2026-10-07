# Distribution

The **Release** workflow builds Windows x64 and Apple Silicon macOS 14+ artifacts:

- `Compi-<version>-Setup.exe`: per-user Windows installer, including repair and uninstall.
- `Compi-Setup.exe`: the same Setup under a stable name, so `releases/latest/download/Compi-Setup.exe` always gets the newest one. In-app **Repair Compi** downloads it.
- `Compi-<version>-Windows-x64.zip`: portable Windows launcher, versioned client/daemon payload, updater, and bundled ConPTY runtime/license. Extract the entire archive together; do not copy individual executables. WSL2 is required for either Windows package.
- `Compi-<version>-macOS-arm64.dmg`: open the disk image and drag `Compi.app` into Applications.
- `Compi-<version>-macOS-arm64.app.zip`: alternative Mac app archive. Move the entire extracted bundle, not just its executable.
- `Compi-<version>-Windows-x64-update.zip`: complete Windows payload consumed by the updater, not a standalone installer.
- `compi-update-<platform>.json`: Ed25519-signed release metadata covering the exact package size/hash, release notes, platform, and qualified daemon versions.
- `compi-setup-<platform>.json`: signed size/hash of `Compi-Setup.exe`, kept apart from the update metadata so clients up to 0.1.5 can still read theirs.
- `SHA256SUMS.txt`: checksums for the release assets.

Both Windows packages include Microsoft's matching `conpty.dll` and `OpenConsole.exe` from pinned [`Microsoft.Windows.Console.ConPTY` 1.24.260710001](https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY/1.24.260710001), plus `ConPTY-LICENSE.txt` (MIT), beside `compi-daemon.exe`. Keep these files together: the daemon does not search PATH/current-directory or fall back to the Windows system ConPTY, which can strip Kitty graphics. Microsoft's binaries retain their original Microsoft signatures; optional Compi signing does not re-sign them.

A `v<workspace-version>` tag builds both platforms and creates one **draft** GitHub release only after the portable/copied-bundle smoke checks and metadata signing pass. Publication remains deliberate; stable update discovery ignores drafts and prereleases. A manually dispatched workflow creates dogfood Actions artifacts, not a public release.

Official tag builds require the updater verification key and signed release metadata. Windows publisher signing and macOS Developer ID signing/notarization remain optional, matching the previous release policy: without those credentials, Windows artifacts are unsigned and macOS artifacts are ad-hoc signed, so OS trust warnings remain. A build without an updater key disables public updates rather than accepting unsigned metadata. Microsoft's bundled runtime retains its Microsoft signatures.

Build locally with `pwsh -File tools/build-installer.ps1` on Windows or `bash tools/build-macos.sh` on an ARM64 Mac. Both accept an expected version tag (`-ExpectedTag` / `--expected-tag`). Official update-key enforcement uses `-RequireUpdateKey` / `--require-update-key`. For deliberately publisher-signed builds, also use `-RequireSigning` or `--require-signing` with `COMPI_MACOS_SIGNING_IDENTITY` and a `COMPI_MACOS_NOTARY_PROFILE` configured through `notarytool`.

### Updates and data retention

Open **Settings → Updates** or run **Check for Updates** from the command palette. **Check for updates** is one switch, on by default; **Download updates automatically** is off by default, so nothing downloads until you click **Download**. Nothing installs, closes windows or restarts a daemon without a click. The page shows one status line and one button for the next step, sizes in MB, the notes for the available and running versions, and a dot on the sidebar button when an update is ready. After an update, the new version's notes appear once as a **What's new** card in the sidebar; fresh installs skip the card. **Advanced** holds **Repair Compi** and **Open update log** (`.compi-update/update.log` in the install folder).

**Repair:** `Compi-Setup.exe --repair` lists problems with the install (program files, unfinished updates, the background task and daemon, WSL) and fixes them one at a time or all together. It logs to `%LOCALAPPDATA%\Compi\doctor.log`. Setup's other runs log to `installer.log` there, and Windows Installer's record of the latest run goes to `installer-msi.log`. Remove always removes the registered install, whichever Setup build runs it.

Compatible activation relaunches clients into their exact saved views while retaining the existing daemon and shell lifetimes. A required local daemon restart needs explicit current-work consent and ends its shells; remote daemons are never restarted by the local updater. If the new build then fails to attach, rollback stops the replacement daemon it started only while that daemon is idle; otherwise rollback waits and reports which shells to close. Legacy 0.1.2 migration requires a deliberate old-install stop because its MSI removal hook cannot preserve live work.

Windows uses `versions/<version>` plus `selection.json`; keep old payloads while their daemon/supervisor still runs. macOS updates replace the complete writable app bundle, not an app on a mounted DMG. Failed activation retains a prior version and recovery journal; success requires a real new-build attachment receipt.

Upgrade, repair, rollback, and default uninstall preserve workspaces, settings, and custom themes. Maintenance offers a separate unchecked managed-data removal choice. External projects/configuration/theme sources and WSL distributions are not removed.

### Release trust configuration

Set repository variable `COMPI_UPDATE_PUBLIC_KEY` to the base64-encoded 32-byte Ed25519 public key and secret `COMPI_UPDATE_SIGNING_KEY` to its base64-encoded 32-byte private seed. Generate a key pair with `compi-release-metadata keygen --private-key <owner-only-file>`; never commit the seed or reuse the disposable qualification key.

Optional Windows CI secrets: `WINDOWS_SIGNING_CERTIFICATE_BASE64`, `WINDOWS_SIGNING_CERTIFICATE_PASSWORD` (configure both or neither). Optional macOS CI secrets: `MACOS_SIGNING_CERTIFICATE_BASE64`, `MACOS_SIGNING_CERTIFICATE_PASSWORD`, `MACOS_SIGNING_IDENTITY`, `APPLE_NOTARY_ID`, `APPLE_NOTARY_TEAM_ID`, `APPLE_NOTARY_PASSWORD` (configure all six or none). Native qualification receipts bind metadata to exact artifact hashes; mixed daemon versions are advertised only when qualified.

Current implementation evidence and outstanding native/MSI/production-trust gates are recorded in [Completed work](COMPLETED.md). Local test signatures do not establish production OS trust.

[Back to README](../README.md)
