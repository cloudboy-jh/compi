# 0.9.1

- New tabs and threads open in the project you're working in
- Tabs find their project, including WSL tabs in a Windows project folder
- **Home** replaces Loose tabs as the project for everything else
- Drag sidebar cards to reorder them or move them to another project
- Updating restarts the local daemon and ends running shells and chats

## Projects and the sidebar

- The tab strip’s **+**, Ctrl/Cmd+T and the sidebar’s buttons open new tabs and threads in the selected tab’s project, in the folder you’re in when it’s inside that project. With no tab selected they use the project you used last.
- A tab that starts outside a Git repository, such as your home folder, joins the first repository its shell enters or a thread opens in, then stays there. A WSL tab in `/mnt/c/…`, or behind a link to it, now shows under the project saved as `C:\…`.
- **Home** is your Start folder. It holds every tab outside a saved project and has its own name, icon and agent defaults in **Settings → Projects**. It can’t be archived or removed.
- Drag a thread or terminal card to reorder it, pin it among pinned cards, or drop it on another project to move it there for good. Drop it on the panes to add its tab to the one you’re looking at. Esc cancels.
- A new thread’s draft names its project at the top. Click it to start the thread in another project before you send.
- Sidebar cards are one line each, indented under their project.
- **New terminal here** on a Windows project uses your chosen shell instead of always opening PowerShell.

## Fixes

- The theme list in **Settings → Appearance** scrolls on its own, in a framed box with a scrollbar, instead of scrolling the whole page.

## Updating

- **Save your work first.** This release changes daemon protocol 41 → 42. The update restarts the local daemon after you consent, ending its running shells and chats. It does not restart remote daemons.
- **From 0.8.0 or 0.8.1 on Windows, use [Compi 0.9.1 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.9.1/Compi-0.9.1-Setup.exe).** Those versions’ in-app updater can hang and roll back. From 0.3.0 or earlier, use Setup once to replace the old update helper.
- Windows packages are not publisher-signed. The macOS app may be ad-hoc signed and not notarized. Signed update metadata does not replace operating-system publisher signing.
