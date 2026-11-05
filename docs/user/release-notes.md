# 0.6.1

- `cd` works across Windows and WSL: `cd ~/proj/app` in PowerShell, `cd C:/Users/you` in WSL
- `compi z` works in PowerShell
- OneDrive: `cd ~/Desktop` finds the Desktop OneDrive moved, Files marks online-only files, and Changes groups line-ending-only changes
- Compi shows in File Explorer's **Open with** after updating in the app
- Updating ends running shells: the daemon restarts

## Going anywhere from either shell

- In Compi's tabs, `cd` also takes the other side's paths. When the shell cannot find a path, Compi tries it on the other side: in PowerShell `cd ~/proj/app`, `cd /home/you/app` and `cd /mnt/c/Users` work, and `cd proj/app` at your Windows home finds it in your Linux home; in WSL `cd C:/Users/you` and `cd 'C:\Users\you'` go to `/mnt/c/Users/you`. A `cd` your profile already defines (zoxide's, say) is kept.
- `compi z` and `compi tree` work in Windows PowerShell, and a folder from either shell opens on the right side.
- New tabs and splits land in the right folder whichever shell they start, and your Windows home as starting folder opens your Linux home in WSL tabs.

## OneDrive

- `cd ~/Desktop` (and Documents, Pictures, Music, Videos) goes to the folder OneDrive moved it to, in PowerShell and WSL, not the empty one Windows leaves behind.
- Files marks files OneDrive keeps only online with a cloud; opening one says it is downloading.
- The first time a terminal is in a OneDrive folder, a note says builds, installs and Git run slower there. It shows once.
- Changes lists files that differ only in line endings (CRLF against LF) under **Line endings only**, with **Restore**.

## Fixes

- After an update in the app, Compi adds itself to File Explorer's **Open with** and **Open in Compi** on folders, which before only Setup did. Uninstalling removes them.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent. Daemon protocol 36 and workspace format 8 are unchanged, so your workspace carries over. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.6.1 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.6.1/Compi-0.6.1-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- Windows without WSL has not yet been tried on a PC that never had WSL installed.
- Windows PowerShell 5.1 is the only native shell. A PowerShell tab in a WSL folder (`\\wsl.localhost\…`) does not report it, so splits there start in the last Windows folder.
- macOS: Open with, Finder integration, clicking a system notification, the browser pane and the PDF, audio and video previews have not yet been checked on screen on a Mac.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
