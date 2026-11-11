# 0.8.1

- Bash commands typed in Windows PowerShell run in WSL again
- Agent chats are easier to read: one line per action, and failures say what went wrong
- Zoom keys size the editor, diffs, Markdown and file lists without touching your terminals
- Agents can run `compi` from their own shells
- Updating restarts the daemon and ends running shells and chats

## Fixes

- **PowerShell → WSL:** in 0.8.0, Bash commands typed or pasted into Windows PowerShell failed with "The Bash companion stopped" and left a script in WSL `/tmp` each time. They run again, and variables carry over between commands. Scripts left by 0.8.0 can be removed with `rm -f /tmp/compi*.sh` in WSL.
- **Question cards:** a choice's heading shows once, fields appear in the order the agent asked them, and **Other** is a choice of its own that opens a text field when picked.
- **`compi` in agent shells:** OMP chats get `compi` on `PATH` with the same `COMPI_*` variables as a terminal, in WSL and on Windows. A chat shown in place of a terminal targets that terminal's pane; a chat beside one needs `--pane` for pane commands.

## Agent chat

- Each tool call is one line: what it did, how long it took, and a check or a cross. Failed calls stay folded and show their first error line instead of opening a large error block.
- Opened, a command shows as one `$ command` line with Copy, and output shows its last 12 lines; **Show all** reveals the rest.
- File reads are coloured by the file's language.
- Each sub-agent line has an arrow and opens on a click anywhere on it. Clicking a running agent in the agents bar opens it and scrolls to it.

## File text size

- **Settings → Terminal → File text size** sizes the editor, diffs, the Markdown Read view, large text previews, and the Files and Changes lists. Until you set it, they follow the terminal font size.
- With one of those focused, Ctrl+=, Ctrl+- and Ctrl+0 (Cmd on macOS) change the file text size instead of the terminal font. Terminals and chats keep their own sizes.

## Updating

- **Save your work first.** Updating from 0.8.0 requires a local daemon restart after you consent, ending its running shells and chats. Daemon protocol 40 and workspace format 10 are unchanged; the new daemon is required for the fixes. Remote daemons are not restarted.
- From 0.3.1 or newer, update in the app. From 0.3.0 or earlier, run [Compi 0.8.1 Setup](https://github.com/cloudboy-jh/compi/releases/download/v0.8.1/Compi-0.8.1-Setup.exe) once so the old update helper can be replaced.

## Limitations

- Agent chat supports installed OMP only. **Open in diff** shows changes not yet staged; Retry resends attached files but not images. Text overlap after resizing a chat window is still being investigated.
- File reads keep OMP's line numbers, so a line that starts with a comment can be coloured as code. Code fences tagged `jsx`, `shell`, `zsh`, `console` or with capital letters are not coloured.
- If a WSL distribution's `/etc/profile` resets `PATH` for login shells, an agent's login shell may not find `compi`.
- Windows UI checks covered PowerShell → WSL commands, file text size, chat tool lines, sub-agents, question cards and `compi` in a WSL agent's shell. Native macOS UI checks for this release remain unperformed; CI package checks do not replace them.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.
