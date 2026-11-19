# 0.8.2

- Fixes the Windows in-app update that rolled back when the old window didn't close; from 0.8.1, install this one with Setup
- Agents in WSL chats know they run on Windows and where your Windows home folder is
- `compi screenshot` lets you or an agent see another app's window
- The background service moves to the installed version after Setup
- Updating restarts the daemon and ends running shells and chats

## Fixes

- **In-app update:** in 0.8.0 and 0.8.1 on Windows, the old Compi window could stay on screen after the update asked it to close, so the update gave up after a minute, rolled back, and had already ended your shells. The window now closes and the update continues.
- **"No GUI host is running":** `compi command run` and other window commands could report no window when two requests arrived close together. They now wait for the window.
- **Background service version:** after Setup, the background service and its sign-in task could stay on the previous version while `compi update` said you were up to date. Setup now moves the task to the installed version. **Settings → Updates** shows a **Background service** row with **Restart service**, or **Repair Compi** while the task is behind, and Repair lists a service still running another version.
- **macOS Oh My Posh previews:** prompt previews for Zsh no longer show `%{` and `%}` around the colours.

## Windows from WSL

- A WSL chat's agent is told at start that it runs in WSL on Windows, your Windows home folder in both forms (`C:\Users\you` and `/mnt/c/Users/you`), and that Windows programs run by their `.exe` names. Your own `APPEND_SYSTEM.md` still applies. An `omp` you start yourself in a terminal does not get this note.
- `compi screenshot` lists the open Windows app windows. `compi screenshot NAME` saves one as a PNG, even when other windows cover it, and prints its path: under `/tmp/compi-screenshots/` from WSL, under `%TEMP%\compi-screenshots\` otherwise. A minimized window has to be restored first.

## Updating

- **Save your work first.** Updating requires a local daemon restart after you consent, ending its running shells and chats. Daemon protocol 40 and workspace format 10 are unchanged; the new daemon is required for the WSL chat note. Remote daemons are not restarted.
- **From 0.8.1 or 0.8.0 on Windows, install with [Compi 0.8.2 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.8.2/Compi-0.8.2-Setup.exe).** Those versions' in-app update can still leave their window stuck and roll back; the fix applies to updates from 0.8.2 on. On macOS, update in the app.
- From 0.3.0 or earlier, run Setup once as well so the old update helper can be replaced.

## Limitations

- Agent chat supports installed OMP only. **Open in diff** shows changes not yet staged; Retry resends attached files but not images. Text overlap after resizing a chat window is still being investigated.
- `compi screenshot` runs on Windows only.
- Windows UI checks covered the update window closing, window commands, the WSL chat note and `compi screenshot` in a development build. The update from 0.8.1 on an installed copy and native macOS UI checks for this release remain unperformed; CI package checks do not replace them.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
