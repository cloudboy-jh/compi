# Compi next steps

Remaining phases of features 1 and 2 (editor, then diffs and Git), one phase per session. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`); phase 1, content panes (`4b458be`); phase 2, files and editor (see [Completed work](COMPLETED.md#files-and-editor-phase-2--2026-10-11)); phase 3, file safety (see [Completed work](COMPLETED.md#file-safety-phase-3--2026-10-12)); phase 4, diff view (see [Completed work](COMPLETED.md#diff-view-phase-4--2026-10-12)).

**Goal:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

Left open from phases 2–4: macOS qualification of the editor, its file-safety behaviour and the three macOS fixes; live TUI redraws through ConPTY with synchronized output; IME in the editor; Windows-host and SSH file environments; the Recover unsaved edits list and moving a tab with unsaved edits to another window, which were not exercised live. From the diff view: the `e` expand-all key and restart restore of a Changes pane were not captured live; text selection in the diff; diff settings are not saved between sessions.

## Phase 5: Editor ↔ diff

Diffs move into the editor pane: a diff opens as a tab beside file tabs, and Changes becomes a sidebar mode beside the folder tree. The standalone Changes pane (`ContentView::Changes`) is removed with a clean cutover, and saved Changes panes reopen as editor panes on their repository.

1. **Gutter spike.** GPUI Kit's editor has text and range decorations but no gutter marks. Try a strip drawn beside the editor, lined up with its line layout (no soft wrap), or a GPUI Kit patch. If neither is clean, tint changed lines with range decorations.
2. **Git gutter.** Compare the editor buffer, unsaved edits included, with the index using `crate::diff`. Mark added, changed and deleted lines, and put `vs index` in the editor header. Recompute on edits (debounced) and when the change stamp moves. Display only: it never stages or discards.
3. **Diff tabs.** `PaneContent` holds diff tabs (path + scope) alongside files. A diff tab hosts `DiffView` with its header and baseline, and its tab label marks it as a diff. **Edit file** opens the work-tree file at the line in view. The sidebar toggles Files ↔ Changes, keeping the list, sections, tree/list mode and live refresh. Protocol 23.
4. **Next/previous change** in the editor (palette commands plus keys), using the gutter's hunks.
5. **Ways in.** From the editor: `vs index` or a gutter mark opens the diff tab at that change. From the Files tree: status letters (M/A/D/?) and **Open diff**. Palette: **Changes** opens the sidebar mode and **Diff this file** opens the active file's diff. CLI: `compi diff [PATH[:LINE]]` (GUI handoff 9).
6. **Smoke in `cargo dev`.** Edit a file and watch the gutter, step through changes, and open diffs from every entry point. Confirm `git status` and the index stay untouched.

**Done when:** every comparison shows its baseline, and the gutter never stages or discards anything.

## Phase 6: Git actions (pending your decision)

- Stage/unstage, commit, fetch/pull/push, branch switching and creation. Discarding changes needs confirmation.

**Done when:** review → stage → commit → push works inside Compi.
