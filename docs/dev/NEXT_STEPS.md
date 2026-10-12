# Compi next steps

Remaining phases of features 1 and 2 (editor, then diffs and Git), one phase per session. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`); phase 1, content panes (`4b458be`); phase 2, files and editor (see [Completed work](COMPLETED.md#files-and-editor-phase-2--2026-10-11)).

**Goal:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

Phase 2 left open: macOS qualification of the editor and the three macOS fixes, live TUI redraws through ConPTY with synchronized output, and IME in the editor.

## Phase 3: File safety

Already there from phase 2: a save refuses when the file changed on disk, a pane with unsaved files refuses to close, and read errors show in the pane.

1. **Line endings and encoding.** Check whether the GPUI Kit editor turns `\r\n` into `\n`; if so, keep each file's line endings, BOM and final newline on save. First, because it is silent damage today.
2. **Unsaved-changes prompt** (Save / Don't save / Cancel) for closing a file tab, a pane or a tab, detaching a tab to another window, closing the window, quitting, restarting the daemon and the `compi update` restart. Replaces the flat refusal.
3. **CLI close.** `compi pane close` / `tab close` ask the GUI host for unsaved files, list them and ask for confirmation; without an interactive terminal they exit 4, like close today.
4. **Unsaved-edit backups.** Unsaved text is backed up to the data directory and restored after a crash or restart.
5. **Save conflicts.** The banner offers Reload (drop my edits), Overwrite (deliberate, against the current disk version) and Save as…; Compare arrives with the diff view.
6. **Read-only files.** The daemon reports whether a file is writable; the editor opens it read-only with a badge and offers Save as….
7. **Outside edits.** Visible files are re-checked (size and time, then hash): an unchanged buffer reloads quietly and keeps the cursor; an edited one shows Changed on disk: Reload / Keep mine; a deleted file says so, and saving re-creates it.
8. **Binary and oversized files.** Keep the 1 MiB limit. Instead of the editor, show the size, path, a Copy path button and why it will not open. Larger read-only views belong to file previews (feature 5). Turn off syntax highlighting above about 512 KB if large files stutter.
9. **Where a file came from.** Tab tooltip and pane header name the environment: host, WSL distribution or SSH host. Reuse never mixes environments.
10. **Verify.** Tests for CRLF/BOM preservation, conflicts, read-only, deleted and binary files; `cargo dev` smoke on the repo under `/mnt/c` and a CRLF file.

**Done when:** saves never silently overwrite, and closing never silently loses edits.

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
