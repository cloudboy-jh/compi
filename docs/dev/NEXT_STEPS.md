# Compi next steps

Features 1 and 2 (editor, then diffs and Git) are complete. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`); phase 1, content panes (`4b458be`); phase 2, files and editor (see [Completed work](COMPLETED.md#files-and-editor-phase-2--2026-10-11)); phase 3, file safety (see [Completed work](COMPLETED.md#file-safety-phase-3--2026-10-12)); phase 4, diff view (see [Completed work](COMPLETED.md#diff-view-phase-4--2026-10-12)); phase 5, editor and diff (see [Completed work](COMPLETED.md#editor-and-diff-phase-5--2026-10-12)); phase 6a, stage, commit, discard (see [Completed work](COMPLETED.md#stage-commit-discard-phase-6a--2026-10-12)); phase 6b, editor controls, one tab per file, remote and branches (see [Completed work](COMPLETED.md#editor-controls-tabs-remote-and-branches-phase-6b--2026-10-12)).

**Goal reached:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

## Left open

- macOS qualification of the editor, file safety, the diff and Git work, and the three earlier macOS fixes; Windows-host and SSH file environments; IME in the editor; live TUI redraws through ConPTY with synchronized output.
- Not seen live: the Recover unsaved edits list; moving a tab with unsaved edits to another window; the `e` expand-all key; the `conflicted` gutter label; the Stale notice; a hook error under the commit box; a confirmed discard; hunk staging through the UI on a `/mnt/c` repo.
- Gaps to keep or drop: text selection in diffs; diff settings are not saved between sessions; tooltips can linger after a click changes the layout (GPUI); long folder names squeeze their icon in the Files tree; the tab strip scrolls sideways without an indicator; material-light's muted text is below AA (4.2:1 opaque, 2.8:1 with transparency).

## Next

1. **Program Status Protocol (OSC 7501)**, the next feature in the features doc.
   - Done, phase A: the daemon parses reports and answers feature detection, keeps at most 64 records per terminal with the prompt (OSC 133 `A` from the shell bridge), exit, reset and acknowledgement rules, and bounds every OSC string before parsing. Protocol 26 adds `WatchProgramStatus` (current records, then coalesced changes) and `AcknowledgeProgramStatus`. Covered by `daemon_integration` `program_status_reaches_watchers_through_the_pty_and_follows_its_lifecycle` through ConPTY and WSL Bash.
   - Done, phase B: one watching connection per window feeds tab glyphs, tiled-pane dots, floating title strips and the sidebar/palette status line, with hover listing each pane's records; a keypress or typed text in a pane acknowledges its finished results; `[program_status] tabs/panes/sidebar` toggles each place (Settings → Interface → Program status); `compi --json pane list/show` carries `program_status`. Seen in `cargo dev` on Windows/WSL: the blocked glyph on a background tab, hover cards, prompt clearing, keypress acknowledgement, a floating pane at 65%, and status surviving a preview restart. See [Completed work](COMPLETED.md#program-status-osc-7501-phases-a-and-b--2026-10-12).
   - Phase C: check the Settings rows on screen, real programs (Claude Code, Pi) and a GUI reconnect against them, and a Unix twin of the integration test on a Mac.

Out of scope for Git: history graphs, interactive rebase, a stash UI, force-push, line-level staging.
