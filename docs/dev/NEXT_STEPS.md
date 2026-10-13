# Compi next steps

Features 1 and 2 (editor, then diffs and Git) are complete. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`); phase 1, content panes (`4b458be`); phase 2, files and editor (see [Completed work](COMPLETED.md#files-and-editor-phase-2--2026-10-11)); phase 3, file safety (see [Completed work](COMPLETED.md#file-safety-phase-3--2026-10-12)); phase 4, diff view (see [Completed work](COMPLETED.md#diff-view-phase-4--2026-10-12)); phase 5, editor and diff (see [Completed work](COMPLETED.md#editor-and-diff-phase-5--2026-10-12)); phase 6a, stage, commit, discard (see [Completed work](COMPLETED.md#stage-commit-discard-phase-6a--2026-10-12)); phase 6b, editor controls, one tab per file, remote and branches (see [Completed work](COMPLETED.md#editor-controls-tabs-remote-and-branches-phase-6b--2026-10-12)).

**Goal reached:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

## Left open

- macOS qualification of the editor, file safety, the diff and Git work, and the three earlier macOS fixes; Windows-host and SSH file environments; IME in the editor; live TUI redraws through ConPTY with synchronized output.
- Not seen live: the Recover unsaved edits list; moving a tab with unsaved edits to another window; the `e` expand-all key; the `conflicted` gutter label; the Stale notice; a hook error under the commit box; a confirmed discard; hunk staging through the UI on a `/mnt/c` repo.
- Gaps to keep or drop: text selection in diffs; diff settings are not saved between sessions; tooltips can linger after a click changes the layout (GPUI); the Files tree's selection fill stops at the name and long folder names squeeze their icon; the tab strip scrolls sideways without an indicator; material-light's muted text is below AA (4.2:1 opaque, 2.8:1 with transparency).

## Next

1. **README hero screenshot.** The text is rewritten ("The terminal you can live in.", sections in workflow order). The screenshot is still the old terminal-only one: replace `assets/compi-workspace.png` with Compi working on itself (an agent in a terminal, the editor with gutter marks, a split diff and the Changes list with the commit box) and update its alt text.
2. **Program Status Protocol (OSC 7501)**, the next feature in the features doc.

Out of scope for Git: history graphs, interactive rebase, a stash UI, force-push, line-level staging.
