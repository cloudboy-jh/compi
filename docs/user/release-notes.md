# 0.7.0

- Tab groups: name and colour a set of tabs, fold it to one label, move it as one
- Agent chat: talk to OMP in a pane, with its tool calls, edits and questions shown as cards
- Chats keep running when you close Compi and pick up where they were
- Updating ends running shells: the daemon restarts

## Tab groups

- Right-click a tab → **Add to group ›**, or press Ctrl+Shift+G (Cmd+Shift+G on macOS), to put it in a new or existing group. Each group has a name and a colour line over its tabs.
- Click a group's label to fold its tabs into it and click again to unfold. Picking a folded tab unfolds its group.
- Drag a label to move the whole group; drag a tab into a group to join it, or out to leave. The group's menu renames, recolours, ungroups, closes, or moves it to a new window.
- From the command line: `compi tab group`, `compi tab ungroup` and `compi group list|rename|color|fold|unfold|move|ungroup|close`.

## Agent chat

- `compi chat`, or **OMP chat** in the command palette, opens a chat with OMP (oh-my-pi) in place of the terminal's shell, in its folder: in WSL, Windows or on a Mac. Ctrl+Shift+E switches back to the shell.
- Tool calls show as cards with their status and time; edits show an inline diff with **Open in diff**. Permissions and OMP's questions are answered on the card.
- Pick the model (searchable), thinking level and mode under the message box. Type `/` for OMP's commands and `@` to attach a file from the folder; **+** attaches files or images from your computer.
- The ⋯ menu starts a new session, resumes an older one or restarts OMP. The footer shows context used and cost.
- The chat runs in Compi's daemon: closing the window leaves a turn running, and the chat resumes its session after a restart.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent (daemon protocol 36 → 38). Your workspace carries over and moves to format 10, which 0.6.1 cannot open. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.7.0 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.7.0/Compi-0.7.0-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- Agent chat supports OMP only so far, and OMP must already be installed. Other agents come next.
- In the chat, **Open in diff** shows changes not yet staged; Resume lists sessions by ID because OMP gives them no titles; Retry resends attached files but not images.
- Tab groups and agent chat have not yet been checked on screen on a Mac.
- Windows without WSL has not yet been tried on a PC that never had WSL installed.
- macOS: Open with, Finder integration, clicking a system notification, the browser pane and the PDF, audio and video previews have not yet been checked on screen on a Mac.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
