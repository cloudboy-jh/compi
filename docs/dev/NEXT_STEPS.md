# Compi next steps

Remaining phases of features 1 and 2 (editor, then diffs and Git), one phase per session. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`); phase 1, content panes (`4b458be`); phase 2, files and editor (see [Completed work](COMPLETED.md#files-and-editor-phase-2--2026-10-11)); phase 3, file safety (see [Completed work](COMPLETED.md#file-safety-phase-3--2026-10-12)).

**Goal:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

Left open from phases 2–3: macOS qualification of the editor, its file-safety behaviour and the three macOS fixes; live TUI redraws through ConPTY with synchronized output; IME in the editor; Windows-host and SSH file environments; the Recover unsaved edits list and moving a tab with unsaved edits to another window, which were not exercised live.

## Phase 4: Diff view

- Native review pane: changed-files list, unified and split layouts, word-level changes, expandable context.
- Git runs as a subprocess in the repository's environment.

**Done when:** you can review a real multi-file change beside the terminal that made it, without changing any files or Git state.

## Phase 5: Editor ↔ diff

- Git gutter, next/previous change, and opening the diff from the editor, the Files pane or the palette (`compi diff`, Changes).

**Done when:** every comparison shows its baseline, and the gutter never stages or discards anything.

## Phase 6: Git actions (pending your decision)

- Stage/unstage, commit, fetch/pull/push, branch switching and creation. Discarding changes needs confirmation.

**Done when:** review → stage → commit → push works inside Compi.
