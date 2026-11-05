# 0.6.0

- Windows: Compi no longer needs WSL. New tabs start Windows PowerShell or WSL, your choice
- **Font size**: one size for every window, in Settings → Terminal
- **Open with** and **Reveal** for files, and **Open in Compi** from File Explorer and Finder
- **Settings → Keyboard** lists every command; click a shortcut to change it
- Updating ends running shells: the daemon restarts

## Windows without WSL

- Setup and Compi start without WSL. WSL is checked only when a WSL tab opens, and its errors say what to run (`wsl --install`, `wsl --set-version NAME 2`).
- **Settings → Terminal → Shell** picks what new tabs and splits start: Windows PowerShell or WSL. If you used Compi before, you keep WSL; new installs start PowerShell. Open tabs keep their shell, also after a restart.
- PowerShell tabs report their folder and program status, keep `$?` and `$LASTEXITCODE`, put `compi` on `PATH` and leave your profile alone. Ctrl+C interrupts.
- Files, search, project history, Git (Changes, Fetch, Pull, Push) and pasted image paths work in PowerShell tabs.
- **Settings → Terminal → Starting folder**: **Choose…** a folder, or **Home**. WSL tabs open the same Windows folder.
- **Shell prompt** offers Oh My Posh and Starship styles for PowerShell, with previews. Compi's block sits outside your profile and can be turned off.

## Font size

- **Settings → Terminal → Font size** shows the terminal and editor size with −, + and **Reset**. Ctrl+= and Ctrl+- (Cmd on macOS) step it by one and Ctrl/Cmd+0 resets it. It is saved as `font.size`, every window follows it, and editing the file applies at once.
- It replaces the per-window zoom: a window you had zoomed shows the saved size.

## Open with and Settings → Keyboard

- A file in Files or Changes, and a file tab's menu, offer **Open with ▸** (the default app, the apps your system suggests, **Choose another app…**) and **Reveal in File Explorer** / **Reveal in Finder**.
- Setup adds Compi to File Explorer's **Open with** for text, code, Markdown, image, PDF and media files, and **Open in Compi** on folders. On macOS, Finder can open files and folders with Compi.
- **Settings → Keyboard** lists every command by category with its shortcut. Click a shortcut to record new keys, Backspace to unbind, **Reset** to restore it.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent: daemon protocol 36 (was 35). Workspace format 8 is unchanged, so your workspace carries over. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.6.0 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.6.0/Compi-0.6.0-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- Windows without WSL has not yet been tried on a PC that never had WSL installed.
- Windows PowerShell 5.1 is the only native shell; PowerShell 7 and cmd are not offered yet. A PowerShell command that starts no other program shows as idle.
- macOS: Open with, Finder integration, clicking a system notification, the browser pane and the PDF, audio and video previews have not yet been checked on screen on a Mac.
- Animated GIF and WebP show their first frame only.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
