# 0.9.5

- Pick a provider from a list with `/login` and `/logout`
- A return arrow takes an "Open here" pick back to a new thread
- Sidebar thread cards show the model each thread is on
- The tab bar can show always, on hover, or not at all
- Updating restarts the local daemon and ends running shells and chats

## Chat

- `/login` and `/logout` open a list of the chat's providers, with their accounts, filtered as you type; Enter or a click signs in or out. The chat's own provider comes first. `/login <name>` still works directly.
- Picking `/providers` or `/model` from the `/` list runs it at once.
- When two providers list a model of the same name (Claude Opus 5 on your subscription and on OpenRouter), the model button and picker say which provider it is.
- A new thread's model picker shows only its own agent; the chevron beside it lists the others.

## New threads

- The message box of a new thread shows its blinking cursor again.
- After picking Terminal, Files, Changes, Browser or Monitor under **Open here**, a return arrow (↵) brings a new thread back in the same folder with the same agent: at the top left of the pane, or on a terminal in its top right corner while the pointer is there. A terminal's shell keeps running underneath.

## Sidebar and tabs

- A thread's card shows the model it is on.
- **Settings → General → Tab bar**: **Always**, **On hover** or **Sidebar only**.

## Fixes

- Scrolling beside a chat that is working no longer lags.
- Cmd+V (Ctrl+V) after clicking in a chat pastes into its message box.
- A tab change that crossed another change no longer fails with "outcome is unconfirmed"; it is retried.

## Updating

- **Save your work first.** The update restarts the local daemon after you consent, ending its running shells and chats. It does not restart remote daemons. Daemon protocol stays 44.
- **From 0.8.0 or 0.8.1 on Windows, use [Compi 0.9.5 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.9.5/Compi-0.9.5-Setup.exe).** Those versions' in-app updater can hang and roll back. From 0.3.0 or earlier, use Setup once to replace the old update helper.
- Windows packages are not publisher-signed. The macOS app may be ad-hoc signed and not notarized. Signed update metadata does not replace operating-system publisher signing.
