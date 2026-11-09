# 0.7.1

- Chat text size: make agent chats larger or smaller without touching the terminal font
- Tool calls in chat show their input and output as readable fields you can select and copy
- Copy buttons in chat confirm with a check mark
- Tooltips no longer pop up or linger after a click
- Updating ends running shells: the daemon restarts

## Agent chat

- **Settings → Interface → Chat text size** sets the chat's text size with −, + and **Reset**, in every window. With a chat focused, Ctrl+=, Ctrl+- and Ctrl+0 (Cmd on macOS) change it instead of the terminal font. Everything in the chat grows together: messages, tool cards, code, menus and the message box.
- A tool call's **Input** and **Output** are no longer raw JSON: fields read as `key: value` lines, lined up, with nested values indented and lists joined (`options: Red, Blue`). Plain text output shows as it came.
- Tool input and output can be selected, and each has a **Copy** button. Copy buttons on code in chat turn into a check mark for two seconds after copying.

## Fixes

- Clicking a button while its tooltip was about to appear no longer brings the tooltip up afterwards, and it no longer stays on screen after the click moved things.

## Updating

- **Running shells end.** The update restarts the local daemon after you consent. Daemon protocol 38 and workspace format 10 are unchanged, so your workspace carries over. Save your work first. Remote daemons are not restarted by the local updater.
- **Windows, from 0.3.0 or earlier:** run [Compi 0.7.1 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.7.1/Compi-0.7.1-Setup.exe) once; the older update helper cannot replace itself. From 0.3.1, update in the app.

## Limitations

- Agent chat supports OMP only so far, and OMP must already be installed. Other agents come next.
- In the chat, **Open in diff** shows changes not yet staged; Resume lists sessions by ID because OMP gives them no titles; Retry resends attached files but not images.
- Tab groups and agent chat, including chat text size, have not yet been checked on screen on a Mac.
- Windows without WSL has not yet been tried on a PC that never had WSL installed.
- macOS: Open with, Finder integration, clicking a system notification, the browser pane and the PDF, audio and video previews have not yet been checked on screen on a Mac.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
