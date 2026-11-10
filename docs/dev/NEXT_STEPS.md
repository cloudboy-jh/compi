# Compi next steps

Open work only. Product scope and order live in the vault's `feat and bugs.md`; shipped work and its checks are in [Completed work](COMPLETED.md) (everything this file held until 2026-11-10 is under [Shipped 0.2–0.7.3 and their checks](COMPLETED.md#shipped-02073-and-their-checks-moved-from-next-steps-2026-11-10)). Release steps: [Release flow](RELEASE-FLOW.md).

## Release state

- **0.7.2** is published and Latest (2026-11-10 UTC). Daemon protocol 39, workspace format 10.
- **0.7.3** is a draft, not published: tag `v0.7.3` at `9fae749`, release run [42758236641](https://github.com/cloudboy-jh/compi/actions/runs/42758236641). It holds resuming sessions in their saved folder, the session picker, transcript paging and one-line tool heads (`72545db`); protocol 39, format 10; updating restarts the daemon. Its draft read back clean (12 assets, signed metadata 0.7.3 / protocol 39, hashes matching `SHA256SUMS.txt`).
- Decided 2026-11-10: the updater work below joins 0.7.3. Then delete the draft, move the unpublished tag to the new commit, rebuild, read back, publish.

## Now: updater cleanup and visibility (for 0.7.3)

Found 2026-11-10 on the owner's Windows install and in the code:

- Nothing is ever removed. Here: 12 folders in `versions\` (0.1.8 to 0.7.2, 1.1 GB besides the current one), 730 MB of `stage-*` packages and about 180 `handoff`/`review` files in `.compi-update\`, 847 MB of old Setup packages in `%LOCALAPPDATA%\Compi\installer\`.
- An available update is invisible: the sidebar dot needs a downloaded update (`updates::shows_dot`), and automatic download is off by default.
- Checks run at most once a day (`daily_check_due`); a failed check also waits a day (`last_check_unix` is written after every check).
- A failure kept from an earlier attempt is shown ahead of a newer release, and its Try again reinstalls the old package; the timer's check does not clear it (only `request()` does).
- 0.7.1 was the last in-app update here; 0.7.2 went in through Setup.

Changes:

1. Check at launch and every 6 hours; after a failed check, try again within the hour.
2. Show a sidebar dot and one line when an update is available, not only when it is downloaded.
3. A newer release replaces a kept failure.
4. After an update completes (in the app or through Setup), keep the current and the previous version and remove older `versions\` folders, finished `stage-*` packages, worker copies, old handoff/review/receipt files and old Setup packages. Never touch anything an unfinished journal still names.

Check: an install carrying old versions updates from 0.7.2 to the new build; the extra folders go, the previous version stays, and the sidebar showed the update before it was downloaded.

## Planned, later

- **Chat → OMP's TUI in the same pane** (asked 2026-11-10). Today Ctrl/Cmd+Shift+E shows the plain shell under the chat. The ⋯ menu gets **Open in terminal**: the daemon stops the chat's `omp acp` (two OMP processes must not write one session file), the pane shows its shell, and Compi types `omp --resume <session>` there, in the session's saved folder. Going back loads the session again with `session/load`, turns made in the TUI included (OMP shares sessions between ACP and its TUI both ways). Open: a shell already running a program (ask, or open a new tab as **Jump to project** does), PowerShell panes (`omp.exe`), noticing the TUI's exit to offer the chat again.
- **Updates that keep shells.** Release metadata qualifies only the release's own daemon (`qualified_daemons = [version]` in the smoke scripts), so every update restarts the daemon, even with the protocol unchanged. Qualify the previous daemon when protocol and workspace format did not change.
- **Updater upkeep.** On Windows the root `compi-update-worker.exe`, launcher and `Compi-Setup.exe` stay at the version Setup last installed; in-app updates never refresh them. Setup and the in-app updater keep two journals and two scheduled-task guards. The 0.1.x–0.3.0 migration paths and `INSTALLER_UPDATES_PLAN.md` (a 0.1.3-era plan) are out of date. macOS activation has never run natively.
- **Other chat harnesses:** OpenCode, Pi, Codex, Claude Code (vault order).
- Vault features after chat: remote connections, general file drag-and-drop, more Windows shells.

## Open bugs

- Chat text overlap after resizing a window (reported 2026-11-09). One cause fixed for 0.7.3: tool rows whose title has line breaks drew over the rows below. Not confirmed to be the reported case; needs a screenshot or steps if it comes back.
- A PowerShell tab in a `\\wsl.localhost` folder does not report it, so splits start in the last Windows folder.
- `cargo dev` restart often says "did not exit after acknowledged shutdown" and keeps the old daemon (also seen with `cargo dev --stop` when the daemon had exited).
- Developer/probe daemon launches from a supervised Compi shell inherit `COMPI_SUPERVISOR_PID`.

## CI and release

- **Windows portable smoke is flaky.** 0.7.1 and 0.7.3 both failed it on the first attempt and passed on a rerun; 0.7.3's failed job then hung for 25 minutes in "Remove imported publisher certificate" and kept no logs. Make the step keep its diagnostics and time out instead of hanging.
- **Installed in-app update check times out** waiting for the test GUI to attach (45 s), so release CI runs it with `continue-on-error`. First add evidence on timeout (`COMPI_PERF_LOG`, client startup log, screenshot), then a `workflow_dispatch` that reruns only `smoke-windows-installed-update.ps1` against an earlier run's artifact.
- **Daemon handle-count test** (`repeated_surface_cycles_release_daemon_process_handles`) fails on some Windows runs, about +15 handles each time (225→241). Find the leak before changing the limit.
- The daemon protocol is pinned by hand in five release scripts (`tools/sign-release-metadata.py`, `smoke-update-cycle.py`, `smoke-macos-bundle.sh`, `smoke-windows-distribution.ps1`, `smoke-windows-installed-update.ps1`); a missed bump already cost 0.5.0 a rebuild.

## Not yet checked

- **macOS on screen:** editor, file safety, diff and Git, settings phases 3a–3f; Monitor; Browser pane and home page; file previews (all five phases) and display scaling; notifications (a click on a notification, an installed signed build); Open with and Finder entries; tab groups; agent chat; native update activation.
- **Windows by hand** (posted input carries no modifier state): Ctrl+E, Ctrl+F in previews and pages; Ctrl+L, F6, Escape, Alt+Left/Right and Compi chords while a Browser page has the keyboard; a click on a Windows toast, Focus Assist; Open with clicked in Explorer on an installed build; physical wheel and trackpad scrolling, snap layouts and edge drags with omp redrawing.
- **Agent chat:** a Windows (`omp.exe`) chat's second listing page; a session missing from the first listing page after a daemon restart; the WSL "isn't installed" (exit 127) case; a link click.
- **Never seen live:** Recover unsaved edits; moving a tab with unsaved edits to another window; the `e` expand-all key; the `conflicted` gutter label; the Stale notice; a hook error under the commit box; a confirmed discard; hunk staging on a `/mnt/c` repo; `command run` of a command that opens a dialog.
- **Elsewhere:** a PC that never had WSL; Windows-host and SSH file environments; SSH note files; IME in the editor; Program Status settings rows on screen and Claude Code reports.

## Gaps to keep or drop

- Text selection in diffs; diff settings not saved between sessions.
- Long folder names squeeze their icon in the Files tree.
- The tab strip scrolls sideways without an indicator.
- material-light's muted text is below AA (4.2:1 opaque, 2.8:1 with transparency).
