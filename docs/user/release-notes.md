# 0.7.2

- Steer OMP while it works, or queue messages for after the turn
- `/new`, `/resume` and `/sessions` in the chat's `/` list
- Paste screenshots into the chat with Ctrl+V
- Chat messages read at regular weight, with clearer headings and code that follows the chat text size
- The model picker groups models by subscription and provider, and its search takes typing
- Updating ends running shells: the daemon restarts

## Agent chat

- **Steering:** while OMP works, Enter sends your message into the running turn, which takes it at its next step. **Queueing:** Ctrl+Enter (Cmd+Enter on macOS) holds a message for after the turn. Queued messages wait above the message box, each with × to remove it, and go one at a time as turns end. After **Stop**, the queue waits and goes after the next message you send.
- The `/` list offers three commands of Compi's own, marked **Compi**: `/new` starts a new session, `/resume` and `/sessions` list the folder's sessions to pick from. Words after them (`/resume login`) narrow the list. A command of the same name from OMP replaces Compi's.
- Ctrl+V (Cmd+V) with an image on the clipboard, such as a screenshot, attaches it to the next message. Large pictures are made smaller to fit; copied text still pastes as text.
- Edits show only their diff. Their output shows when an edit failed or changed nothing.
- Messages and the message box use regular weight instead of the interface's medium, so bold text and headings stand out; headings step up clearly in size, and code in messages and tool output grows with **Chat text size**.
- The model picker lists your subscriptions first (for OMP, providers signed in with a plan, such as Claude or ChatGPT, with how much of the plan is left), then pay-per-use providers, each under its own heading. Search matches model and provider names, and arrow keys skip the headings.

## Fixes

- Typing into the model picker's search did nothing on Windows; it now filters as you type, and Space types a space instead of picking.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent. Daemon protocol 39 (workspace format 10 is unchanged, so your workspace carries over). Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.7.2 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.7.2/Compi-0.7.2-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- Agent chat supports OMP only so far, and OMP must already be installed. Other agents come next.
- In the chat, **Open in diff** shows changes not yet staged; Resume lists sessions by ID because OMP gives them no titles; Retry resends attached files but not images.
- A chat window resized can draw text overlapping in some cases; not yet reproduced, so not yet fixed.
- Tab groups and agent chat have not yet been checked on screen on a Mac.
- Windows without WSL has not yet been tried on a PC that never had WSL installed.
- macOS: Open with, Finder integration, clicking a system notification, the browser pane and the PDF, audio and video previews have not yet been checked on screen on a Mac.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
