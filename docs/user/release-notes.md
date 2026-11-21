# 0.9.0

- Choose whether new tabs start as chats or terminals
- Use OMP, Claude Code, Codex or OpenCode in native chat
- Add a folder or clone a Git URL directly into your saved projects
- Start the first WSL tab faster when WSL is already running
- Updating restarts the local daemon and ends running shells and chats

## Chats and terminals

- Choose **Chat first** or **Terminal first** at setup or in **Settings → General**. The choice applies to the tab strip’s **+** and Ctrl/Cmd+T. Right-click **+** to open the other kind of tab or start a thread with a different agent.
- A new thread starts as a draft. Send a message to keep it as a thread, or use **Open here** to open a terminal, files, changes, browser or monitor in the same place. Reopen a closed thread from **Threads** in the command palette.
- Native chat works with installed OMP, Claude Code, Codex and OpenCode. New threads use your default agent or the project’s choice. You can switch agents in an unsent draft without losing your text. Available controls depend on the agent.
- Compi starts shell preparation in the background instead of making new tabs wait on each other. On Windows, **Automatic** uses PowerShell for Windows folders and WSL for Linux-only folders. Existing explicit shell choices are kept.

## Projects and settings

- **Add project…** in the sidebar and Settings opens the same picker. Open an existing folder or clone an HTTPS or SSH Git URL into the selected environment. Adding a project does not launch a tab.
- Saved projects stay in the sidebar even with no tabs. Start a thread or terminal from the project’s menu, or open its settings. Other tabs stay under **Loose tabs**. Existing workspace commands and tab layouts remain available.
- Use **Settings → Projects** to change the name, icon, pin, default agent and model. Archive a project to hide it without losing threads. **Remove from Compi…** confirms thread deletion and leaves files on disk.
- General holds startup defaults. Agents shows installed agents and their approval settings. Window and layout controls are now under Appearance.

## Updating

- **Save your work first.** This release changes daemon protocol 40 → 41 and workspace format 10 → 11. The update restarts the local daemon after you consent, ending its running shells and chats. It does not restart remote daemons.
- **From 0.8.0 or 0.8.1 on Windows, use [Compi 0.9.0 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.9.0/Compi-0.9.0-Setup.exe).** Those versions’ in-app updater can hang and roll back. From 0.3.0 or earlier, use Setup once to replace the old update helper.
- Windows packages are not publisher-signed. The macOS app may be ad-hoc signed and not notarized. Signed update metadata does not replace operating-system publisher signing.
