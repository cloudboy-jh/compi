# Installation, updates, and repair

## Installation

Download Compi from [GitHub Releases](https://github.com/cloudboy-jh/compi/releases).

### Windows

Windows x64 is required. Compi uses Windows PowerShell; WSL2 is optional, for Linux shells (`wsl --install`). Choose either:

- **Installer:** run `Compi-<version>-Setup.exe` for a per-user installation with repair and uninstall. `Compi-Setup.exe` is the same installer under a stable name for the latest release. Setup also lists Compi in File Explorer's **Open with** and adds **Open in Compi** to folders, and an installed Compi adds them back when it starts after an update in the app; uninstalling removes both. Windows may ask once which app opens a file type after a new app registers for it.
- **Portable:** extract the entire `Compi-<version>-Windows-x64.zip` archive together. Do not copy individual executables; the bundled terminal runtime and license must stay with the daemon. The portable copy does not register with File Explorer.

### macOS

Apple Silicon and macOS 14+ are required. Open `Compi-<version>-macOS-arm64.dmg` and drag `Compi.app` into Applications. Alternatively, extract `Compi-<version>-macOS-arm64.app.zip` and move the entire app bundle, not just its executable. Run the app from a writable location, not the mounted disk image, to allow updates. Native workspace qualification remains open.

Windows builds are unsigned and macOS builds are ad-hoc signed unless publisher credentials are configured; OS trust warnings may appear.

## Updates

Open **Settings → Updates** or run **Check for Updates** from the command palette. The page starts with the installed version, the updater's state, when Compi last checked, the download size and, before you download, what updating ends: the shells a daemon restart closes, or a restart of Compi's background service. One button offers the next step. Below are the notes of the available version (or of the installed one), then **Automatic**: **Check for updates** (on by default) checks when Compi starts and every 6 hours while it runs, or an hour after a failed check; **Download updates automatically** is off by default, so nothing downloads until you click **Download**. Nothing installs, closes windows or restarts a daemon without a click. An available or downloaded update also shows as a card in the sidebar and a dot on the sidebar button; the card's **Download**, **Restart to update** or **Restart and end N shells** do what the page's button does, and **Later** hides the card and dot until a newer version arrives. After an update, the new version's notes appear once as a **What's new** card in the sidebar; fresh installs skip the card. **Advanced** holds **Repair Compi** and **Open update log** (`.compi-update/update.log` in the install folder). Old versions and leftover update files are removed quietly when Compi starts.

An update that can keep the running daemon relaunches clients into their saved views without ending shells. If a local daemon restart is required, Compi asks for consent and ends its shells; save your work first. The local updater never restarts remote daemons.

Installing an update includes reopening Compi; starting the helper is not completion. Installation ownership is checked before windows close or a consented daemon stops. A failed activation or new-build attachment records the error and reopens the previous saved views when safe. If the previous views cannot reopen safely, the update journal and worker log retain the failure and recovery details.

See the current [release notes](release-notes.md) for version-specific update instructions.

## Repair and removal

On Windows, `Compi-Setup.exe --repair` lists problems with the install (program files, unfinished updates, the background task and daemon, WSL) and fixes them one at a time or all together. In-app **Repair Compi** downloads the latest Setup and runs the same repair.

Repair logs to `%LOCALAPPDATA%\Compi\doctor.log`. Setup's other runs log to `installer.log` there, and Windows Installer's record of the latest run goes to `installer-msi.log`. Remove always removes the registered install, whichever Setup build runs it.

Setup keeps one copy of the installed package, `%LOCALAPPDATA%\Compi\installer\Compi.msi`, which Windows Installer repairs from. Each run works in a folder next to it and deletes the folder when it completes. The next Setup deletes the folder of a failed run, and keeps an interrupted run's folder until it has undone that run.

Upgrade, repair, rollback, and default uninstall preserve workspaces, settings, and custom themes. Maintenance offers a separate unchecked managed-data removal choice. External projects/configuration/theme sources and WSL distributions are not removed.

[Back to README](../../README.md)
