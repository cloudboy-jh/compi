# 0.7.4

- Agent chats show their status on tabs, panes, the sidebar and the palette, and notify you, like OMP in a terminal
- Chats follow your OMP approval setting, so they stop asking before every command if you told OMP not to
- OMP's sub-agents show in the chat while they run and after they finish
- A new Settings → Chat page, and every Settings page now says what it covers
- Updating ends running shells: the daemon restarts

## Agent chat

- A chat reports working while OMP works, waiting on you while a permission or question waits, done when a turn ends, and failed when a turn ends in an error or OMP stops. The tab glyph, pane dot, floating title, sidebar and palette status lines and hover cards show it, in background tabs too. A chat waiting for a message shows nothing.
- **Needs attention** and **Finished** notifications cover chats: a notification says OMP needs permission, has a question or finished, and clicking it opens the chat. The chat you're using stays quiet.
- Typing in a chat's message box clears its done or failed mark, as a key press does in a terminal.
- OMP's chat mode ignored OMP's own approval setting and asked before every command even with `tools.approvalMode: yolo`. Compi now reads that setting when a chat starts and hands it to OMP.
- **Sub-agents:** when OMP starts agents, their line lists each one with its state, what it is doing, time and cost, led by a faint π and one dot per agent that fills as each finishes. Click a finished agent for its result. While any run, a bar above the message box names them; a thin "3 agents finished · 32s · $0.24" line closes the run. OMP's "Waiting for…" rows are left out.
- `compi --json pane list` and `pane show` include a chat pane's `program_status`.

## Settings

- **Settings → Chat:** Text size (moved from Interface), **Approvals** (Same as OMP, Always ask, Allow edits, Allow everything), **Enter while OMP works** (steer the running turn or queue for after it), **Opening a chat** (in place of the shell or beside it, also for `compi chat`) and **Agents bar**.
- Every page opens with its name and a line on what it covers, every setting sits under a group label with a line saying what it does, and no row repeats its group's name: **Show notifications**, **Notification sound**, **Default shell**, **Current theme**, and **Tab details** (was Tab metadata).
- Settings → Updates shows **Repair and update log** as a plain row instead of a fold-out.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent. Daemon protocol 40 (was 39); workspace format 10 is unchanged, so your workspace carries over. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.7.4 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.7.4/Compi-0.7.4-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- Agent chat supports OMP only so far, and OMP must already be installed. Other agents come next.
- Stopping a turn while sub-agents run may leave them shown as running until OMP restarts; a resumed session may not show an earlier run's agents. Clicking a chat's notification to open it has not yet been checked on screen.
- In the chat, **Open in diff** shows changes not yet staged; Retry resends attached files but not images.
- Text overlapping after resizing a chat window is still being looked into.
- Agent chat, tab groups and the session picker have not yet been checked on screen on a Mac; chat status has not been checked with a Windows (not WSL) chat.
- Windows without WSL has not yet been tried on a PC that never had WSL installed.
- macOS: Open with, Finder integration, clicking a system notification, the browser pane and the PDF, audio and video previews have not yet been checked on screen on a Mac.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
