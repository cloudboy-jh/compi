# Compi next steps

Remaining phases of features 1 and 2 (editor, then diffs and Git), one phase per session. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`); phase 1, content panes (`4b458be`); phase 2, files and editor (see [Completed work](COMPLETED.md#files-and-editor-phase-2--2026-10-11)); phase 3, file safety (see [Completed work](COMPLETED.md#file-safety-phase-3--2026-10-12)); phase 4, diff view (see [Completed work](COMPLETED.md#diff-view-phase-4--2026-10-12)); phase 5, editor and diff (see [Completed work](COMPLETED.md#editor-and-diff-phase-5--2026-10-12)).

**Goal:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

Left open from phases 2–5: macOS qualification of the editor, its file-safety behaviour and the three macOS fixes; live TUI redraws through ConPTY with synchronized output; IME in the editor; Windows-host and SSH file environments; the Recover unsaved edits list and moving a tab with unsaved edits to another window, which were not exercised live. From the diff work: the `e` expand-all key and the `conflicted` gutter label were not seen live; text selection in the diff; diff settings are not saved between sessions.

## Phase 6: Git actions (pending your decision)

- Stage/unstage, commit, fetch/pull/push, branch switching and creation. Discarding changes needs confirmation.

**Done when:** review → stage → commit → push works inside Compi.
