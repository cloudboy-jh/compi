# 0.9.4

- New chats use one install of each agent, in WSL, in every folder
- Rename a thread and the name sticks everywhere, even after a restart
- Sidebar cards count their panes and list what runs in each
- Updating restarts the local daemon and ends running shells and chats

## Agents

- On Windows, an agent installed both in WSL and on Windows now runs its WSL install for new chats, in Windows folders too, so its sign-ins, settings and sessions are the same everywhere. Settings → Agents has an **Ubuntu | Windows** switch for each such agent to pick the other one. Threads you already have keep running where they started.
- On macOS and Linux the agent card just shows the installed version and **Update to …** when a newer one is out.

## Sidebar and threads

- Renaming a thread's card or tab names the thread: the sidebar, the tab, the chat header and **Threads** show it, it stays after a restart, and the agent's own titles no longer replace it.
- A tab with more than one pane says how many (**2 threads**, **2 terminals**, **3 panes**). Hover a card to see each pane and what runs in it.

## Fixes

- In a narrow pane the message box keeps **+** and **Send** visible; its hint and usage shrink first.

## Updating

- **Save your work first.** The update restarts the local daemon after you consent, ending its running shells and chats. It does not restart remote daemons. Daemon protocol 43 → 44.
- **From 0.8.0 or 0.8.1 on Windows, use [Compi 0.9.4 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.9.4/Compi-0.9.4-Setup.exe).** Those versions' in-app updater can hang and roll back. From 0.3.0 or earlier, use Setup once to replace the old update helper.
- Windows packages are not publisher-signed. The macOS app may be ad-hoc signed and not notarized. Signed update metadata does not replace operating-system publisher signing.
