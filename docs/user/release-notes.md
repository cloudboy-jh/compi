# 0.8.0

- Type or paste commands from the other shell without switching terminals
- Open a running dev server in place of its terminal while the server keeps running
- Language icons, code labels and syntax colours make chat code and diffs easier to read
- Open running sub-agents to see their task and steps; finished agents no longer stay spinning
- Updating restarts the daemon and ends running shells and chats

## Shell commands and prompts

- Clearly PowerShell commands typed or pasted into WSL Bash/Zsh run in PowerShell on Enter; clearly Bash commands in Windows PowerShell run in WSL. Commands the current shell already knows stay there, and ambiguous input stays native.
- Each terminal keeps a persistent companion shell, so its variables survive between commands. Folder changes sync back to the terminal. Original command text stays in history.
- **Settings → Terminal → Commands from the other shell** turns this on or off for new shells. Completion stays with the current shell; alternative line editors and custom executable/argument profiles are left unchanged.
- PowerShell prompt changes check PSReadLine support before replacing a working prompt. Details explains how to update an older line editor. Turning off Compi's managed override returns to the prompt your global profile set; Compi does not edit that profile unless you choose **Also use outside Compi**.

## Dev servers and browsing

- After a terminal prints a local dev-server address, an **Open** card offers the app once the address answers. Open replaces that terminal with the Browser view, rather than adding a split. The dev server keeps running underneath; **Toggle view** returns to its terminal.
- **Settings → Terminal → When a dev server starts** chooses **Offer to open**, **Open automatically** or **Do nothing**. Automatic opening happens once per server start, not every time you return to the shell.
- `compi browse` also accepts a bare port such as `5173`, a local HTML file or an explicit folder such as `.` containing `index.html`. The CLI still opens beside the terminal. Apps using JavaScript modules, fetch or server routes should use their dev server, not a `file://` page.

## Chat code and sub-agents

- Catppuccin language icons identify chat file tools, code blocks and inline diffs, as well as Files/Changes trees and file tabs. Dark and light themes use their matching icon sets. Code blocks show their language beside Copy; inline diffs show syntax colours and added/removed markers.
- Click a sub-agent while it runs or after it finishes to open its task, steps and result. The running bar and ending line open the relevant agent rows.
- Compi reads completion records when OMP stops sending updates after a parent turn, including records already saved when a chat resumes. Completed agents no longer stay marked running indefinitely.
- Ending cards refresh late time/cost corrections in place, including zero-cost corrections. They do not duplicate cards, change a later run's card or inflate elapsed time just because a report arrived later.

## Updating

- **Save your work first.** Updating from 0.7.4 requires a local daemon restart after you consent, ending its running shells and chats. Daemon protocol 40 and workspace format 10 are unchanged; the new daemon is required for the fixes. Remote daemons are not restarted.
- From 0.3.1 or newer, update in the app. From 0.3.0 or earlier, run [Compi 0.8.0 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.8.0/Compi-0.8.0-Setup.exe) once so the old update helper can be replaced.

## Limitations

- Agent chat supports installed OMP only. **Open in diff** shows changes not yet staged; Retry resends attached files but not images. Text overlap after resizing a chat window is still being investigated.
- Windows UI checks covered shell routing and prompts, dev-server opening, dark/light chat icons and diffs, and live sub-agent completion/navigation. Native macOS UI checks for this batch remain unperformed; CI package checks do not replace them.
- Windows without WSL has not been checked on a PC that never had WSL installed. Routing to the other shell requires that shell to be installed.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
