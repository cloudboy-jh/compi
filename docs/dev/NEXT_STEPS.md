# Compi next steps

Open work only. Product scope and order live in the vault's `feat and bugs.md`; shipped work and its checks are in [Completed work](COMPLETED.md) (the 0.7.3 updater and the 0.7.4 build notes are under [0.7.3 updater and 0.7.4 work](COMPLETED.md#073-updater-and-074-work-moved-from-next-steps-2026-11-10)). Release steps: [Release flow](RELEASE-FLOW.md).

## Release state

- **0.7.4** tagged `v0.7.4` at `715fefa` (2026-11-10); release run [42919485431](https://github.com/cloudboy-jh/compi/actions/runs/42919485431). Next: check the draft (12 assets, signed metadata 0.7.4 / protocol 40 / qualified daemon 0.7.4, hashes against `SHA256SUMS.txt`), publish, then update from the installed 0.7.3. Protocol 39 → 40, so updating restarts the daemon.
- **0.7.3** is Latest until then (published 2026-11-10 19:10 UTC).

## 0.7.4: not seen yet

- Clicking a chat's notification to open it; the focused chat staying quiet (needs Compi active); typing in the composer clearing done.
- A chat's pane dot, floating title, sidebar and palette lines and hover card.
- Stopping a turn while sub-agents run (OMP may send no final status, leaving them "running" until restart); a resumed session's replay of an old task call; OMP's blocking task path (`details.results[]`).
- Settings → Chat: another Approvals choice picked in Settings, Queue, Beside, the agents bar off.
- A Windows (`omp.exe`) chat's approval lookup and status.
- Settings → Advanced: the performance readout is still a block of figures, not rows.

## Next: Windows work from a WSL session

Vault plan: `feat windows from wsl.md` (from an OMP session on 2026-11-08 that installed mods through Vortex from WSL `/tmp`). An agent in a WSL folder had to find the Windows home by listing `/mnt/c` and asking `pwsh.exe` for `$env:USERPROFILE`; `pwsh.exe` ran through `~/.compi/wincmd…` but `tasklist.exe` needed its full path; it could not see a Windows dialog it caused; searching `C:\Users\johns` from WSL timed out twice.

1. At session start in WSL, tell the agent it runs in WSL on Windows and give it the Windows home folder in both forms (`C:\Users\…`, `/mnt/c/Users/…`). Where it goes is open: the shell bridge's environment (`COMPI_WINDOWS_FOLDERS` already carries `Home=`), `compi` CLI output an agent reads, or a file agents load.
2. Put `cmd.exe`, `tasklist.exe` and `winget.exe` in the same `wincmd` folder as `pwsh.exe` (`shell_integration::WindowsCommands`), so they run without full paths.
3. Let an agent capture an open Windows app window (a `compi` command returning an image path), so it can see dialogs it caused.

Check: a fresh session in WSL `/tmp` names the Windows home folder, runs `tasklist` and `winget --version` without full paths, and describes a dialog open in another app. Not seen yet: UAC prompts, `winget` installs, why `cmd.exe /c start steam://…` did not start the game.

## Planned, later

- **Chat → OMP's TUI in the same pane** (asked 2026-11-10). Today Ctrl/Cmd+Shift+E shows the plain shell under the chat. The ⋯ menu gets **Open in terminal**: the daemon stops the chat's `omp acp` (two OMP processes must not write one session file), the pane shows its shell, and Compi types `omp --resume <session>` there, in the session's saved folder. Going back loads the session again with `session/load`, turns made in the TUI included (OMP shares sessions between ACP and its TUI both ways). Open: a shell already running a program (ask, or open a new tab as **Jump to project** does), PowerShell panes (`omp.exe`), noticing the TUI's exit to offer the chat again.
- **Updates that keep shells.** Release metadata qualifies only the release's own daemon (`qualified_daemons = [version]` in the smoke scripts), so every update restarts the daemon, even with the protocol unchanged. Qualify the previous daemon when protocol and workspace format did not change.
- **Updater upkeep.** A repair from Settings → Apps after Setup removed its run folder is untested. Root `Compi-Setup.exe` still changes only through Setup. Setup and the in-app updater keep two journals and two scheduled-task guards. The 0.1.x–0.3.0 migration paths and `INSTALLER_UPDATES_PLAN.md` (a 0.1.3-era plan) are out of date. macOS activation has never run natively.
- **Other chat harnesses:** OpenCode, Pi, Codex, Claude Code (vault order).

## Open bugs

- Chat text overlap after resizing a window (reported 2026-11-09). One cause fixed for 0.7.3: tool rows whose title has line breaks drew over the rows below. Not confirmed to be the reported case; needs a screenshot or steps if it comes back.
- A PowerShell tab in a `\\wsl.localhost` folder does not report it, so splits start in the last Windows folder.
- `cargo dev` restart often says "did not exit after acknowledged shutdown" and keeps the old daemon (also seen with `cargo dev --stop` when the daemon had exited).
- Developer/probe daemon launches from a supervised Compi shell inherit `COMPI_SUPERVISOR_PID`.

## CI and release

- **Windows portable smoke is flaky.** 0.7.1 and 0.7.3 both failed it on the first attempt and passed on a rerun; 0.7.3's failed job then hung for 25 minutes in "Remove imported publisher certificate" and kept no logs. Make the step keep its diagnostics and time out instead of hanging.
- **Installed in-app update check** builds and installs since `0519b8e`, then times out on "test GUI attached to the original running shell" (dry run [42879581522](https://github.com/cloudboy-jh/compi/actions/runs/42879581522)), before any update. Next: evidence on timeout (`COMPI_PERF_LOG`, client startup log, screenshot), and a `workflow_dispatch` that reruns only `smoke-windows-installed-update.ps1`.
- **Daemon handle-count test** (`repeated_surface_cycles_release_daemon_process_handles`) fails on some Windows runs, about +15 handles each time (225→241). Find the leak before changing the limit.
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
