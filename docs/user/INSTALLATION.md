# Installation, updates, and repair

## Installation

Download Compi from [GitHub Releases](https://github.com/cloudboy-jh/compi/releases).

### Windows

Windows x64 with WSL2 is required. Choose either:

- **Installer:** run `Compi-<version>-Setup.exe` for a per-user installation with repair and uninstall. `Compi-Setup.exe` is the same installer under a stable name for the latest release.
- **Portable:** extract the entire `Compi-<version>-Windows-x64.zip` archive together. Do not copy individual executables; the bundled terminal runtime and license must stay with the daemon.

### macOS

Apple Silicon and macOS 14+ are required. Open `Compi-<version>-macOS-arm64.dmg` and drag `Compi.app` into Applications. Alternatively, extract `Compi-<version>-macOS-arm64.app.zip` and move the entire app bundle, not just its executable. Run the app from a writable location, not the mounted disk image, to allow updates. Native workspace qualification remains open.

Windows builds are unsigned and macOS builds are ad-hoc signed unless publisher credentials are configured; OS trust warnings may appear.

## Updates

Open **Settings → Updates** or run **Check for Updates** from the command palette. **Check for updates** is one switch, on by default; **Download updates automatically** is off by default, so nothing downloads until you click **Download**. Nothing installs, closes windows or restarts a daemon without a click. The page shows one status line and one button for the next step, sizes in MB, the notes for the available and running versions, and a dot on the sidebar button when an update is ready. After an update, the new version's notes appear once as a **What's new** card in the sidebar; fresh installs skip the card. **Advanced** holds **Repair Compi** and **Open update log** (`.compi-update/update.log` in the install folder).

An update that can keep the running daemon relaunches clients into their saved views without ending shells. If a local daemon restart is required, Compi asks for consent and ends its shells; save your work first. The local updater never restarts remote daemons.

See the current [release notes](release-notes.md) for version-specific update instructions.

## Repair and removal

On Windows, `Compi-Setup.exe --repair` lists problems with the install (program files, unfinished updates, the background task and daemon, WSL) and fixes them one at a time or all together. In-app **Repair Compi** downloads the latest Setup and runs the same repair.

Repair logs to `%LOCALAPPDATA%\Compi\doctor.log`. Setup's other runs log to `installer.log` there, and Windows Installer's record of the latest run goes to `installer-msi.log`. Remove always removes the registered install, whichever Setup build runs it.

Upgrade, repair, rollback, and default uninstall preserve workspaces, settings, and custom themes. Maintenance offers a separate unchecked managed-data removal choice. External projects/configuration/theme sources and WSL distributions are not removed.

[Back to README](../../README.md)
