# 0.1.6

- New Updates page: one status line, one button, sizes in MB
- Repair Compi: Setup now finds and fixes a broken install
- Setup upgrades installs stuck on 0.1.3
- Plain error messages instead of Windows Installer codes

## Updates

- **Settings → Updates** shows the running version, what's available and one button for the next step: Check now, Download, Restart to update.
- Checking is one switch, on by default. Downloads happen only when you click, unless you turn on **Download updates automatically**.
- A dot on the sidebar button shows when an update is ready.
- **What's new** shows the notes for both the available and the running version.
- **Advanced** has **Repair Compi** and **Open update log**.

## Repair

- `Compi-Setup.exe --repair` checks the install and lists what's wrong, such as a background service that won't restart if it stops, missing program files, an unfinished update or WSL problems. Fix each finding or all of them.
- **Repair Compi** in the app downloads the latest Setup, checks its signature and runs the same repair.
- Each release now includes a stable-named `Compi-Setup.exe`.

## Fixes

- Setup and in-app updates compared the daemon task's owner by name, and Windows can report that owner as a bare user name, so your own task was refused as "belongs to another Windows account". Owners are now compared by account SID.
- Setup over 0.1.3 left the background service on the old version, which newer Compi can't talk to. Setup now moves it to the new version, and asks first if that ends running shells.
- Removing Compi with a Setup file other than the one that installed it failed with "Windows Installer couldn't finish". Remove now always targets the installed copy.
- Remove now deletes the whole program folder.
- Setup's log (`%LOCALAPPDATA%\Compi\installer.log`) is plain readable text again. Windows Installer's own record goes to `installer-msi.log` and covers only the latest run.
- Shells that ended when Compi restarted are labeled **Ended** with a **Restart shell** button.
- Includes the 0.1.5 fixes: closing the daemon's console window no longer stops its supervisor, and a stopped supervisor no longer blocks updates.

## Updating

- Updating restarts the Compi daemon, which ends running shells. Save your work first.
- On 0.1.3, the in-app updater can't get past its own checks. Download `Compi-Setup.exe` from this release and run it instead.
