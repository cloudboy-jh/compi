# Compi next steps

Open work only. Product scope and order live in the vault's `feat and bugs.md`; shipped work and its checks are in [Completed work](COMPLETED.md) (the 0.7.3 updater and the 0.7.4 build notes are under [0.7.3 updater and 0.7.4 work](COMPLETED.md#073-updater-and-074-work-moved-from-next-steps-2026-11-10)). Release steps: [Release flow](RELEASE-FLOW.md).

## Release state

- **0.8.1** is Latest: published 2026-11-11 21:20 UTC, tagged `v0.8.1` at `d5fb6e3` (release prep `15c432e` plus a docs commit), release run [43099782821](https://github.com/cloudboy-jh/compi/actions/runs/43099782821), 12 assets. Windows and macOS package jobs passed. Signed update and Setup metadata say version 0.8.1, protocol 40 and the 0.8.1 notes, with hashes matching `SHA256SUMS.txt`. The working instance's in-app update from 0.8.0 rolled back (see Open bugs); Setup 0.8.1 then installed it, and its service was moved to 0.8.1 by hand on 2026-11-18. The 0.8.1 fixes still need rechecking there.
  - The first tag runs (on `15c432e`) stayed queued with no jobs for over an hour and the API refuses to cancel them ("completed"); a new push to `main` got jobs created, so the unpublished tag was moved to `d5fb6e3` before any draft existed. Those stale runs still show as queued.
- **0.8.0**: published 2026-11-11 16:10 UTC, tagged `v0.8.0` at `7c12262`, release run [43053398818](https://github.com/cloudboy-jh/compi/actions/runs/43053398818), 12 assets.

## Next: chat first

Order and scope: the vault's `feat and bugs.md` → **Chat first** (decided 2026-11-18). T3 Code behaviour it borrows, with source references: [T3 Code research](../research/t3-code/README.md). Compi stays a terminal workspace; nothing here removes a terminal path.

Implementation notes that the vault leaves out:

- **Threads** need a daemon-owned store: the transcript Compi shows plus `{agent, session id}`; a thread's agent is fixed, so there is no handoff ([threads](../research/t3-code/threads.md)). Today a chat reads OMP's session files (`compi-daemon/src/chat/saved.rs`) and ends with its pane.
- **Agents** get their own drivers for fidelity: Codex `app-server` (JSON-RPC over stdio), OpenCode's HTTP server and event stream, Pi `--mode rpc` (JSON lines). OMP stays on ACP with Compi's OMP extras. Claude Code: try its JSON stream mode from Rust first, else a Node helper running the Agent SDK. Plain ACP loses sub-agents, real rewind, steering and plan cards ([agent fidelity](../research/t3-code/agent-fidelity.md)). `ChatHarness` (`compi-protocol/src/chat.rs`) holds only `Omp` today; drivers report capabilities and the UI hides what an agent can't do. The agent is a setting (Default agent), with **New thread with…** for exceptions.
- **Project file search** on the daemon: a gitignore-aware fuzzy index per project folder, refreshed after agent turns; an empty query lists recent files.
- **Model picker:** today one ACP option list per setting (`chat/composer.rs` `render_chat_picker`), closing on pick. Effort levels come from the agent for its current model only. The starting model is the one the agent reports at session start; Compi remembers your last pick per agent (its own config, never the agent's). Icons for models and agents.
- **Project icons** ([project icons](../research/t3-code/project-icons.md)): the daemon runs T3's lookup order in the project folder (WSL or Windows) and sends image bytes; fallback is T3's monogram algorithm copied exactly. Pickers come with Settings → Projects ([settings UI](../research/t3-code/settings-ui.md)).
- **Sidebar** rebuild: replaces the `Workspaces · New` header, tab rows and text footer in `gui/workspace.rs` (sidebar render). Workspace groups stay; rows become T3-style cards.

## 0.7.4: not seen yet

- Clicking a chat's notification to open it; the focused chat staying quiet (needs Compi active); typing in the composer clearing done.
- A chat's pane dot, floating title, sidebar and palette lines and hover card.
- Stopping a turn while sub-agents run on screen; OMP's blocking task path (`details.results[]`). Saved completions after a cancelled parent turn have a daemon regression check.
- Settings → Chat: another Approvals choice picked in Settings, Queue, Beside, the agents bar off.
- A Windows (`omp.exe`) chat's approval lookup and status.
- Settings → Advanced: the performance readout is still a block of figures, not rows.

## 0.8.0: checks left

Commands from the other shell, dev-server cards and in-place browsing, Catppuccin language icons, clickable sub-agent task/step lists, completion recovery and ending-card corrections shipped in 0.8.0. Details and observed checks: [Completed work](COMPLETED.md).

Smoke test 2026-11-11 (installed 0.8.0, OMP chat in WSL, `compi-landingpage` repo):

- **Passed:** the Open card on a `python3 -m http.server` pane opened the page in place, Toggle view returned to the terminal and the server kept serving. In WSL Bash, PowerShell commands ran in PowerShell, `$compiDemo` survived between commands and `Set-Location C:\Windows` moved the Bash prompt to `/mnt/c/Windows`.
- **Failed:** PowerShell → WSL routing, chat code colours, finding a sub-agent to open (see Fixed after 0.8.0).
- **Not confirmed:** added/removed markers in chat diffs; a finished sub-agent no longer looking busy.

- macOS browser views and icon rendering: the only known Mac, `bruisedpear`, was offline on 2026-11-11, with no Mac SSH host configured. Native checks could not run. Physical Ctrl/Cmd-click on a local link and the view-toggle chord while the web page has the keyboard also remain unobserved.
- Alternative line editors are not intercepted; custom executable/argument profiles keep their existing behavior. The other-shell setting applies to new shells. Tab completion stays with the current shell.

## Planned, later

- **Chat → OMP's TUI in the same pane** (asked 2026-11-10). Today Ctrl/Cmd+Shift+E shows the plain shell under the chat. The ⋯ menu gets **Open in terminal**: the daemon stops the chat's `omp acp` (two OMP processes must not write one session file), the pane shows its shell, and Compi types `omp --resume <session>` there, in the session's saved folder. Going back loads the session again with `session/load`, turns made in the TUI included (OMP shares sessions between ACP and its TUI both ways). Open: a shell already running a program (ask, or open a new tab as **Jump to project** does), PowerShell panes (`omp.exe`), noticing the TUI's exit to offer the chat again.
- **Updates that keep shells.** Release metadata qualifies only the release's own daemon (`qualified_daemons = [version]` in the smoke scripts), so every update restarts the daemon, even with the protocol unchanged. Qualify the previous daemon when protocol and workspace format did not change.
- **Updater upkeep.** A repair from Settings → Apps after Setup removed its run folder is untested. Root `Compi-Setup.exe` still changes only through Setup. Setup and the in-app updater keep two journals and two scheduled-task guards. The 0.1.x–0.3.0 migration paths and `INSTALLER_UPDATES_PLAN.md` (a 0.1.3-era plan) are out of date. macOS activation has never run natively.

## Fixed after 0.8.0, not released

- **macOS Oh My Posh previews:** Oh My Posh wraps ANSI sequences in `%{` and `%}` for Zsh even when asked for unescaped output; Compi now removes those wrappers from previews while keeping the colours and text.

- **PowerShell → WSL routing.** A Windows checkout gave `assets/compi-other-shell.ps1` CRLF line endings, so the Bash companion written to WSL `/tmp` had them too: `trap`/the read loop failed and `rm -f -- "$0"` missed the file. The script now turns CRLF into LF before writing the companion.
- **Chat code colours.** The uncoloured "JavaScript block" was the output of a `read theme.js` tool call, fenced without a language. Read output now takes the file's language from its path. Message fences already coloured (javascript/js, typescript, rust, python, bash, json, yaml, toml, html, css, go); `jsx`, `shell`, `zsh`, `console` and capitalised tags still don't, as gpui-component has no aliases for them.
- **Sub-agents hard to find.** Each sub-agent line has a chevron and opens on a click anywhere on it; each running agent in the agents bar opens and scrolls to its line.
- **Question card layout.** The daemon read form fields from a sorted JSON map and sorted them again; `serde_json` now keeps order (`preserve_order` in `compi-daemon`) and fields follow the form. OMP's HTML-form `{name}_other` field now joins its choices (only `__other` did), the heading drops lines repeating a field title, and Other is a chip after the choices that opens its text field when picked (seen: typed "postgres" reached OMP as the field's answer).
- **`compi` in agent shells.** Chat harnesses get the terminal's CLI environment (`cli_environment`): `COMPI_INSTANCE`, the CLI paths, and `COMPI_SURFACE_ID` when the chat shows in place of a terminal (a chat beside one has no surface, so commands needing a pane take `--pane`). In WSL the `~/.compi/shell/compi` bridge is installed and put first on PATH after the login profile; Windows `omp.exe` chats get `compi.exe`'s folder first on PATH. Unchecked: a distribution whose `/etc/profile` resets PATH for OMP's own login shells.
- **Tool cards.** Actions are one line; failed ones stay folded and show their first error line. Opened, a command is one `$ command` line with Copy, and output shows its last 12 lines with **Show all**.
- **File text size** (asked 2026-11-11): Settings → Terminal → File text size (`[files] text_size`, 8–32, unset follows the terminal font). Zoom keys in an editor, diff, Markdown Read view, large text, Files or Changes step it instead of the terminal font.
- **Service left on the old version after an update** (seen 2026-11-18 on the installed copy). The in-app 0.8.0 → 0.8.1 update stopped the service, then rolled back because the old window didn't exit within 60 s; Setup 0.8.1 then kept the restarted 0.8.0 service and pinned its task (`selection.json` 0.8.1 / task 0.8.0), and `compi update` said up to date. Now: Setup always moves the task to the installed version and leaves a compatible running service alone until its next start. Repair lists **Background service still runs X** (restart, shells listed) and an idle task on another version. Settings → Updates shows a **Background service** row with Restart service, or Repair Compi when the task is still behind. A staged update the installation has moved past is dropped instead of logging "selection changed after staging" at every launch. Unit-tested; the row itself is unseen, as no installed copy lags now (the machine was fixed by hand: task re-registered to 0.8.1, service restarted from the palette).
- **Windows work from a WSL session** (vault plan `feat windows from wsl.md`). A WSL OMP chat starts with `-e ~/.compi/shell/compi-omp.js`, a bridge file whose `before_agent_start` hook ends the system prompt with `COMPI_AGENT_NOTE`: WSL and its distribution, the Windows home in both forms (`USERPROFILE`, and its `wslpath -u` answer kept per distribution), `.exe` commands, `compi screenshot`. Not `--append-system-prompt`: it replaces the user's `APPEND_SYSTEM.md` (checked with OMP 18.22.2). `compi screenshot` (Windows only, no daemon) lists app windows frontmost first and saves one with `PrintWindow(PW_RENDERFULLCONTENT)` cropped to its DWM frame, to `/tmp/compi-screenshots/` when run from WSL, else `%TEMP%\compi-screenshots\`. Seen in `cargo dev` 2026-11-18: the chat's `omp acp` had `-e` and the full note, and an agent given that note named both home forms and the command; through the chat's own bridge environment, `compi screenshot compi` saved a correct 1840×1392 PNG, and a minimized Slack exited 1 with no file. Not seen: a model asked inside the Compi chat UI; terminal `omp` gets no note.

Seen in `cargo dev` on Windows (2026-11-11): Ctrl+=/−/0 in the Markdown Read view and the diff changed only `files.text_size`; in a terminal they changed only `font.size`. A PowerShell tab ran `uname -sr`, kept an exported variable for the next command and left no companion script in WSL `/tmp`. In a WSL OMP chat, a failed `ls` stayed one line with its error, `command -v compi` gave `~/.compi/shell/compi` with `COMPI_INSTANCE`, the CLI paths and the pane's `COMPI_SURFACE_ID`, a Read of a `.rs` file opened with colours, a finished scout opened from its chevron line, and a radio-group-plus-textarea form showed the legend once, "Other…", then Notes, and its answer reached OMP. Not seen: the agents-bar click while agents run; macOS. Read output keeps OMP's `N:` line prefixes, so a line starting with a comment highlights as code.

## Open bugs

- Chat text overlap after resizing a window (reported 2026-11-09). One cause fixed for 0.7.3: tool rows whose title has line breaks drew over the rows below. Not confirmed to be the reported case; needs a screenshot or steps if it comes back.
- A PowerShell tab in a `\\wsl.localhost` folder does not report it, so splits start in the last Windows folder.
- `cargo dev` restart often says "did not exit after acknowledged shutdown" and keeps the old daemon (also seen with `cargo dev --stop` when the daemon had exited).
- Developer/probe daemon launches from a supervised Compi shell inherit `COMPI_SUPERVISOR_PID`.
- **In-app update: the old window didn't exit within 60 s** (0.8.0 → 0.8.1, 2026-11-11), after the service had already been stopped, so shells ended for nothing. No log says why. `compi command run restart_daemon` from WSL also answered "No GUI host is running" while `window list` saw the window (2026-11-18).
- `%LOCALAPPDATA%\Compi\update-gui-hosts-v1` keeps a `.lock` per GUI process ever run (hundreds since October).

## CI and release

- **Windows portable smoke is flaky.** 0.7.1 and 0.7.3 both failed it on the first attempt and passed on a rerun; 0.7.3's failed job then hung for 25 minutes in "Remove imported publisher certificate" and kept no logs. Make the step keep its diagnostics and time out instead of hanging.
- **Installed in-app update check** builds and installs since `0519b8e`, then times out on "test GUI attached to the original running shell" (dry run [42879581522](https://github.com/cloudboy-jh/compi/actions/runs/42879581522)), before any update. Next: evidence on timeout (`COMPI_PERF_LOG`, client startup log, screenshot), and a `workflow_dispatch` that reruns only `smoke-windows-installed-update.ps1`.
- **Daemon handle-count test** (`repeated_surface_cycles_release_daemon_process_handles`) fails on some Windows runs, about +15 handles each time (225→241). Find the leak before changing the limit.
- **Cargo 1.100 build layout** (fixed 2026-11-18, `cc8ada5`): stable Rust 1.100 moved test executables out of `target/<profile>/deps` into a folder per build unit, so the ConPTY test no longer found `OpenConsole.exe` and Core CI failed on Windows from 2026-11-14. `compi-daemon/build.rs` now finds the profile folder in both layouts, and the test copies the runtime beside itself. Run 44408665049 passed every Windows step. Under full-suite load on the OneDrive checkout, `lost_shell_restarts_in_its_last_reported_directory` once hit its 10 s save deadline; it passes alone.
- The daemon protocol is pinned by hand in five release scripts (`tools/sign-release-metadata.py`, `smoke-update-cycle.py`, `smoke-macos-bundle.sh`, `smoke-windows-distribution.ps1`, `smoke-windows-installed-update.ps1`); a missed bump already cost 0.5.0 a rebuild.

## Not yet checked

- **macOS on screen:** editor, file safety, diff and Git, settings phases 3a–3f; Monitor; Browser pane and home page; file previews (all five phases) and display scaling; notifications (a click on a notification, an installed signed build); Open with and Finder entries; tab groups; agent chat; native update activation.
- **Windows by hand** (posted input carries no modifier state): Ctrl+E, Ctrl+F in previews and pages; Ctrl+L, F6, Escape, Alt+Left/Right and Compi chords while a Browser page has the keyboard; a click on a Windows toast, Focus Assist; Open with clicked in Explorer on an installed build; physical wheel and trackpad scrolling, snap layouts and edge drags with omp redrawing.
- **Agent chat:** **Open in diff**; a Windows (`omp.exe`) chat's second listing page; a session missing from the first listing page after a daemon restart; the WSL "isn't installed" (exit 127) case; a link click.
- **Never seen live:** Recover unsaved edits; moving a tab with unsaved edits to another window; the `e` expand-all key; the `conflicted` gutter label; the Stale notice; a hook error under the commit box; a confirmed discard; hunk staging on a `/mnt/c` repo; `command run` of a command that opens a dialog.
- **Elsewhere:** a PC that never had WSL; Windows-host and SSH file environments; SSH note files; IME in the editor; Program Status settings rows on screen and Claude Code reports.

## Gaps to keep or drop

- Text selection in diffs; diff settings not saved between sessions.
- Long folder names squeeze their icon in the Files tree.
- The tab strip scrolls sideways without an indicator.
- material-light's muted text is below AA (4.2:1 opaque, 2.8:1 with transparency).
