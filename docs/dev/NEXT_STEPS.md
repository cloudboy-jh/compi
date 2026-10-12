# Compi next steps

Remaining phases of features 1 and 2 (editor, then diffs and Git), one phase per session. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`), and phase 1, content panes (`4b458be`).

**Goal:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

## Phase 2: Files and editor

- The daemon reads and saves files in the shell's real environment (local, WSL or SSH), with versioned saves.
- With no file open, the content pane shows the folder across the whole pane. Picking a file turns it into the GPUI Kit editor, with file tabs and an optional collapsible tree inside the pane. The workspace sidebar doesn't change.
- Editor: syntax highlighting, line numbers, search, file switching and save.
- Open it with `compi files [dir]`, `compi edit [file[:line]]`, the palette (Files, Open file…, New editor) and terminal file links. New panes open beside the current terminal, with no new shell or tab.
- Risk: GPUI Kit components may need their own root view; if so, Compi's window root gets wrapped here.
- Also qualify content panes on Windows/WSL, which phase 1 could not.

**Done when:** you can open a real Compi source file, edit it, save it, then run the changed code in the terminal next to it.

## Phase 3: File safety

- Unsaved-changes prompts, save conflicts, read-only files, outside edits, binary and oversized files.
- Remember whether a file came from local, WSL or SSH.

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
