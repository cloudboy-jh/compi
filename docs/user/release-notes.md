# 0.7.3

- Earlier chats open again from Resume… and `/resume`
- A session picker with search and a preview of each session
- Page Up and Page Down scroll the chat
- Updating ends running shells: the daemon restarts

## Agent chat

- **Resume…** in the ⋯ menu, `/resume` and `/sessions` open the session picker over the chat: the folder's sessions under Today, Yesterday, This week and Earlier, each with its title or first prompt, age and size, and a preview of the highlighted one's first prompt and last reply. Typing keeps the sessions holding every word in their title, first prompt, last reply, folder or ID. Up and Down, Page Up, Page Down, Home and End move; Enter resumes; Esc closes.
- Page Up and Page Down scroll the conversation, also while typing a message; Ctrl+End (Cmd+Down on macOS) goes back to the newest message.

## Fixes

- Resuming an earlier chat failed, and the chat started a new session instead, when the folder had been reached under another name, such as a link (`~/proj` pointing into `/mnt/c/…`). Each session now opens in the folder it was saved in.
- The session list shows every session, not only the first 50 on Windows, and leaves out sessions with no messages.
- A tool call whose title runs over several lines (OMP's code tool) no longer draws its code over the rows below.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent. Daemon protocol 39 and workspace format 10 are unchanged, so your workspace carries over. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.7.3 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.7.3/Compi-0.7.3-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- Agent chat supports OMP only so far, and OMP must already be installed. Other agents come next.
- In the chat, **Open in diff** shows changes not yet staged; Retry resends attached files but not images.
- Text overlapping after resizing a chat window is still being looked into; one cause, multi-line tool titles, is fixed above.
- The session picker has not yet been checked on screen on a Mac or with a Windows (not WSL) chat.
- Tab groups and agent chat have not yet been checked on screen on a Mac.
- Windows without WSL has not yet been tried on a PC that never had WSL installed.
- macOS: Open with, Finder integration, clicking a system notification, the browser pane and the PDF, audio and video previews have not yet been checked on screen on a Mac.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
