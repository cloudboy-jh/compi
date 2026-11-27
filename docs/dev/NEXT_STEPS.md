# Compi next steps

Open work only. Product scope and order live in the vault's `feat and bugs.md`; shipped work and its checks are in [Completed work](COMPLETED.md) and [release notes](../user/release-notes.md). Release steps: [Release flow](RELEASE-FLOW.md).

Latest public release: [0.9.4](https://github.com/cloudboy-jh/compi/releases/tag/v0.9.4), daemon protocol 44, workspace format 11.

## Next

- **Pi native chat** is likely next. Add its RPC driver through the [agent recipe](ADDING_AN_AGENT.md), check it on Windows and WSL, and only show controls its driver supports.

## Release follow-through

- Flaky Windows/Linux daemon tests seen on Core CI: `closing_a_chat_pane_ends_its_terminal_like_any_other` and `lost_shell_restarts_in_its_last_reported_directory` (its 10 s save wait).
- Not seen natively: the macOS app, the Agents card's **Installed …** line off Windows.
- Not run yet: Claude Code's and Codex's browser sign-ins and sign-outs from the chat, and the expired-sign-in card live.
- Recheck the 0.8.1 and 0.8.2 fixes on an installed Windows build: PowerShell → WSL routing, chat code colours, sub-agents, question cards, `compi` in agent shells, one-line tool cards, File text size, Background service, the WSL chat note and `compi screenshot`.
- Not seen natively: the macOS app, providers and project dragging on macOS.
- Not seen natively: the macOS app, sidebar dragging on macOS.
- 0.9.5, not seen yet: the `/login` and `/logout` provider lists in a live chat; the return arrow on Windows; returning from Changes and Browser picks (Files, Terminal and Monitor were checked through a hook on macOS).

## 0.7.4: not seen yet

- Clicking a chat's notification to open it; the focused chat staying quiet (needs Compi active); typing in the composer clearing done.
- A chat's pane dot, floating title, sidebar and palette lines and hover card.
- Stopping a turn while sub-agents run on screen; OMP's blocking task path (`details.results[]`). Saved completions after a cancelled parent turn have a daemon regression check.
- Settings → Chat: another Approvals choice picked in Settings, Queue, Beside, the agents bar off.
- A Windows (`omp.exe`) chat's approval lookup and status.
- Settings → Advanced: the performance readout is still a block of figures, not rows.

## 0.8.0: checks left

- Added/removed markers in chat diffs; a finished sub-agent no longer looking busy.
- macOS browser views and icon rendering remain unchecked. Physical Ctrl/Cmd-click on a local link and the view-toggle chord while the web page has the keyboard also remain unobserved.

## Planned, later

- **Remote:** attach to another machine's sessions over Tailscale when wanted; no cloud component or phone app.
- **More Windows shells:** PowerShell 7 and cmd.

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
- **Installed in-app update check** timed out on "test GUI attached to the original running shell" in an earlier [dry run](https://github.com/cloudboy-jh/compi/actions/runs/42879581522), before any update. The 0.9.4 release's installed Setup migration and in-app update check passed. Investigate if the timeout recurs: collect `COMPI_PERF_LOG`, the client startup log and a screenshot; add a `workflow_dispatch` that reruns only `smoke-windows-installed-update.ps1`.
- **Daemon handle-count test** (`repeated_surface_cycles_release_daemon_process_handles`) fails on some Windows runs, about +15 handles each time (225→241). Find the leak before changing the limit.
- The protocol is still pinned by hand in five release scripts (listed in the [release flow](RELEASE-FLOW.md)); a missed bump previously cost 0.5.0 a rebuild.

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
