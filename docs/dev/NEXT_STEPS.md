# Compi next steps

Open work only. Product scope and order live in the vault's `feat and bugs.md`; shipped work and its checks are in [Completed work](COMPLETED.md) (the 0.7.3 updater and the 0.7.4 build notes are under [0.7.3 updater and 0.7.4 work](COMPLETED.md#073-updater-and-074-work-moved-from-next-steps-2026-11-10)). Release steps: [Release flow](RELEASE-FLOW.md).

## Release state

- [**0.9.3**](https://github.com/cloudboy-jh/compi/releases/tag/v0.9.3) is the latest public release (2026-11-23 06:25 UTC, tag at `6f5e8f0`, [release run](https://github.com/cloudboy-jh/compi/actions/runs/45118175702), [Core CI on main](https://github.com/cloudboy-jh/compi/actions/runs/45118172564)). Fixes 0.9.2's model picker, whose strip and left side closed it instead of picking. Daemon protocol 43, workspace format 11. Both native jobs passed, the Windows installed Setup migration and in-app update check included; 12 downloads, signed update and Setup metadata with version 0.9.3, protocol 43 and the 0.9.3 notes, the update package hash matching `SHA256SUMS.txt`. The installed 0.9.2 CLI's `compi update --check` reports 0.9.3. Core CI on the tag failed once on Windows (`closing_a_chat_pane_ends_its_terminal_like_any_other` read the workspace before the surface was removed) and its rerun hung in the test step and was cancelled; the same commit passed Core CI on `main` on every platform.
- [**0.9.2**](https://github.com/cloudboy-jh/compi/releases/tag/v0.9.2) was published (2026-11-23 02:05 UTC, tag at `80ce2ea`, [release run](https://github.com/cloudboy-jh/compi/actions/runs/45096824353), [Core CI](https://github.com/cloudboy-jh/compi/actions/runs/45096821732)). Daemon protocol 43, workspace format 11. Both native jobs passed, the Windows installed Setup migration and in-app update check included. Verified all 12 downloads; signed update metadata for Windows and macOS and Setup metadata with version 0.9.2, protocol 43 and the 0.9.2 notes; the Windows update package hash matches `SHA256SUMS.txt`. The installed 0.9.1 CLI's `compi update --check` reports 0.9.2 with its headline. Before tagging: Windows `cargo test --workspace` and clippy `-D warnings`, Linux daemon and protocol tests.
- [**0.9.1**](https://github.com/cloudboy-jh/compi/releases/tag/v0.9.1) was published 2026-11-22 19:57 UTC (tag at `da61ba0`, [release run](https://github.com/cloudboy-jh/compi/actions/runs/45050713719)). Daemon protocol 42, workspace format 11.
- [**0.9.0**](https://github.com/cloudboy-jh/compi/releases/tag/v0.9.0) was published 2026-11-21 23:04 UTC (tag at `cd2b7e8`, [release run](https://github.com/cloudboy-jh/compi/actions/runs/44958925248)). Daemon protocol 41, workspace format 11. The daily Windows install is on 0.9.0.
- [**0.8.2**](https://github.com/cloudboy-jh/compi/releases/tag/v0.8.2) was published 2026-11-19 (`382509d`, [release run](https://github.com/cloudboy-jh/compi/actions/runs/44435599983)). Daemon protocol 40, workspace format 10. Windows installs on 0.8.0 or 0.8.1 should use Setup because their in-app updater can hang.
- **0.8.1**: published 2026-11-11 21:20 UTC, tagged `v0.8.1` at `d5fb6e3`, release run [43099782821](https://github.com/cloudboy-jh/compi/actions/runs/43099782821), 12 assets. Its in-app update from 0.8.0 rolled back on the working instance (the window hang, fixed in 0.8.2); Setup installed it.
- **0.8.0**: published 2026-11-11 16:10 UTC, tagged `v0.8.0` at `7c12262`, release run [43053398818](https://github.com/cloudboy-jh/compi/actions/runs/43053398818), 12 assets.

## v0.9.2 follow-through

This release adds providers, sign-in and sign-out from the chat for all four agents (`/providers`, `/login`, `/logout`), `/update` and Settings → Agents updates, dragged project order with Home pinned by default, the linked-WSL new-tab fix, the model picker lined up with the message box, and a new thread's box growing with its pane. Protocol 42 → 43. See [Completed work](COMPLETED.md#providers-and-agent-updates-v092).

- Not run yet: Claude Code's and Codex's browser sign-ins and sign-outs from the chat, and the expired-sign-in card live.
- Run the in-app upgrade from 0.9.1 on the daily Windows install when convenient; protocol 42 → 43 restarts the local daemon, ending open shells and chats.
- Not seen natively: the macOS app, providers and project dragging on macOS.

## v0.9.1 follow-through

This release adds tabs finding their project (WSL and linked folders included), Home in place of Loose tabs, sidebar card dragging (`SetTabProject`, protocol 42), new tabs and threads in the selected tab's project with a draft's project switch, one-line indented sidebar cards, and the theme grid's own scroll. See [Completed work](COMPLETED.md#chat-first-v090).

- Run the in-app upgrade from 0.9.0 on the daily Windows install when convenient; protocol 41 → 42 restarts the local daemon, ending open shells and chats.
- Not seen natively: the macOS app, sidebar dragging on macOS.

## v0.9.0 follow-through

This release includes chat-first setup and new tabs, four native agents (OMP, Claude Code, Codex, OpenCode), faster first tabs with WSL already running, reorganized Settings, and one saved project list shared by the sidebar and Settings. See [Completed work](COMPLETED.md#chat-first-v090) and [T3 Code research](../research/t3-code/README.md). Pi is next, not part of v0.9.0.

- Check an actual in-app upgrade from installed Windows 0.8.2 on an isolated machine. Protocol 40 → 41 and workspace format 10 → 11 require restarting the local daemon, which ends open shells and chats. Leave the daily install alone.
- Try the physical Ctrl/Cmd+T chord in both new-tab modes. Check the project picker on a native Mac and clone a private repo over SSH; the Windows dev check used public HTTPS.

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

- **Pi native chat** is the next agent, after v0.9.0. Add its RPC driver through the [agent recipe](ADDING_AN_AGENT.md), check it on Windows and WSL, and only show controls its driver supports.
- **Chat → OMP's TUI in the same pane** (asked 2026-11-10). Today Ctrl/Cmd+Shift+E shows the plain shell under the chat. The ⋯ menu gets **Open in terminal**: the daemon stops the chat's `omp acp` (two OMP processes must not write one session file), the pane shows its shell, and Compi types `omp --resume <session>` there, in the session's saved folder. Going back loads the session again with `session/load`, turns made in the TUI included (OMP shares sessions between ACP and its TUI both ways). Open: a shell already running a program (ask, or open a new tab as **Jump to project** does), PowerShell panes (`omp.exe`), noticing the TUI's exit to offer the chat again.
- **Updates that keep shells.** Release metadata qualifies only the release's own daemon (`qualified_daemons = [version]` in the smoke scripts), so every update restarts the daemon, even with the protocol unchanged. Qualify the previous daemon when protocol and workspace format did not change.
- **Updater upkeep.** A repair from Settings → Apps after Setup removed its run folder is untested. Root `Compi-Setup.exe` still changes only through Setup. Setup and the in-app updater keep two journals and two scheduled-task guards. The 0.1.x–0.3.0 migration paths and `INSTALLER_UPDATES_PLAN.md` (a 0.1.3-era plan) are out of date. macOS activation has never run natively.

## Open bugs

- Chat text overlap after resizing a window (reported 2026-11-09). One cause fixed for 0.7.3: tool rows whose title has line breaks drew over the rows below. Not confirmed to be the reported case; needs a screenshot or steps if it comes back.
- A PowerShell tab in a `\\wsl.localhost` folder does not report it, so splits start in the last Windows folder.
- `cargo dev` restart often says "did not exit after acknowledged shutdown" and keeps the old daemon (also seen with `cargo dev --stop` when the daemon had exited).
- Developer/probe daemon launches from a supervised Compi shell inherit `COMPI_SUPERVISOR_PID`.
- `%LOCALAPPDATA%\Compi\update-gui-hosts-v1` keeps a `.lock` per GUI process ever run (hundreds since October).

## CI and release

- **Windows portable smoke is flaky.** 0.7.1 and 0.7.3 both failed it on the first attempt and passed on a rerun; 0.7.3's failed job then hung for 25 minutes in "Remove imported publisher certificate" and kept no logs. Make the step keep its diagnostics and time out instead of hanging.
- **Installed in-app update check** builds and installs since `0519b8e`, then times out on "test GUI attached to the original running shell" (dry run [42879581522](https://github.com/cloudboy-jh/compi/actions/runs/42879581522)), before any update. Next: evidence on timeout (`COMPI_PERF_LOG`, client startup log, screenshot), and a `workflow_dispatch` that reruns only `smoke-windows-installed-update.ps1`.
- **Daemon handle-count test** (`repeated_surface_cycles_release_daemon_process_handles`) fails on some Windows runs, about +15 handles each time (225→241). Find the leak before changing the limit.
- **Cargo 1.100 build layout** (fixed 2026-11-18, `cc8ada5`): stable Rust 1.100 moved test executables out of `target/<profile>/deps` into a folder per build unit, so the ConPTY test no longer found `OpenConsole.exe` and Core CI failed on Windows from 2026-11-14. `compi-daemon/build.rs` now finds the profile folder in both layouts, and the test copies the runtime beside itself. Run 44408665049 passed every Windows step. Under full-suite load on the OneDrive checkout, `lost_shell_restarts_in_its_last_reported_directory` once hit its 10 s save deadline; it passes alone.
- The protocol is still pinned by hand in five release scripts (listed in the [release flow](RELEASE-FLOW.md)). All five currently match 41; a missed bump previously cost 0.5.0 a rebuild.

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
