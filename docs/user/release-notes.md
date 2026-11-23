# 0.9.2

- Sign in, sign out and see your model providers from the chat, for OMP, Claude Code, Codex and OpenCode, on Windows and WSL
- `/update` updates the chat's agent; **Settings → Agents** offers updates when a newer version is out
- Drag projects in the sidebar to reorder them; Home starts pinned and moves like any project
- New tabs from a linked WSL folder stay in WSL
- Updating restarts the local daemon and ends running shells and chats

## Providers and sign-in

- `/providers` lists what the chat's agent can sign in to, with each account, its plan and any sign-in the agent can no longer use. Buttons sign in (Claude subscription or Anthropic Console, ChatGPT or an API key, OpenCode's browser sign-ins or a key), add another account where the agent keeps several, and sign out where the agent allows it. OMP signs out only in its own window.
- `/login` and `/logout` do the same for the chat's provider, or the one you name (`/login moonshot`).
- Compi runs the agent's own sign-in where the chat runs, so it lands in the same program the chat uses. A card above the message box opens the sign-in page; when the provider asks for a code or an API key, paste it into the message box. The agent then starts again on the same conversation.
- When a reply fails because a provider isn't signed in, or a sign-in expired, the card offers **Sign in**.
- In OMP and OpenCode chats the model picker shows each provider's sign-in under its models, with a button only when something needs doing.

## Agent updates

- `/update` updates the chat's agent with its own updater (Windows or WSL, wherever the chat runs it) and shows the version before and after. Chats using that agent stop while it runs and start again on their conversations; it waits for running turns to finish first.
- **Settings → Agents** shows each install's version and **Update … to …** when a newer one is published. Nothing installs until you click.

## Projects and the sidebar

- Drag a project's heading to reorder projects. The order is saved and shared with **Settings → Projects**. Pinned projects stay together at the top.
- **Home** starts pinned but can be unpinned or moved like any project. It still can't be archived or removed.

## Fixes

- A new tab or thread opened from a WSL folder linked into a Windows project (`~/proj/x` → `/mnt/c/…/x`) keeps WSL and that folder, instead of switching to the Windows agent and its separate sign-ins.
- The model picker lines up with the message box.
- A new thread's message box grows with its pane, up to a comfortable width, and fills a narrow one.

## Updating

- **Save your work first.** This release changes daemon protocol 42 → 43. The update restarts the local daemon after you consent, ending its running shells and chats. It does not restart remote daemons.
- **From 0.8.0 or 0.8.1 on Windows, use [Compi 0.9.2 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.9.2/Compi-0.9.2-Setup.exe).** Those versions' in-app updater can hang and roll back. From 0.3.0 or earlier, use Setup once to replace the old update helper.
- Windows packages are not publisher-signed. The macOS app may be ad-hoc signed and not notarized. Signed update metadata does not replace operating-system publisher signing.
