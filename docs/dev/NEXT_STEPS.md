# Compi next steps

Remaining phases of features 1 and 2 (editor, then diffs and Git), one phase per session. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`); phase 1, content panes (`4b458be`); phase 2, files and editor (see [Completed work](COMPLETED.md#files-and-editor-phase-2--2026-10-11)); phase 3, file safety (see [Completed work](COMPLETED.md#file-safety-phase-3--2026-10-12)); phase 4, diff view (see [Completed work](COMPLETED.md#diff-view-phase-4--2026-10-12)); phase 5, editor and diff (see [Completed work](COMPLETED.md#editor-and-diff-phase-5--2026-10-12)); phase 6a, stage, commit, discard (see [Completed work](COMPLETED.md#stage-commit-discard-phase-6a--2026-10-12)).

**Goal:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

Left open from phases 2–6a: macOS qualification of the editor, its file-safety behaviour and the three macOS fixes; live TUI redraws through ConPTY with synchronized output; IME in the editor; Windows-host and SSH file environments; the Recover unsaved edits list and moving a tab with unsaved edits to another window, which were not exercised live. From the diff work: the `e` expand-all key and the `conflicted` gutter label were not seen live; text selection in the diff; diff settings are not saved between sessions. From 6a, to test by hand: a confirmed discard, hunk staging through the UI on a `/mnt/c` repo, the Stale notice, Stage/Unstage all, `s`/`u`, a hook error under the commit box.

## Phase 6b: Remote and branches

1. **Branch status.** The Changes header shows the upstream and ahead/behind (`main ↑2 ↓1`) from the status Compi already reads.
2. **Fetch, pull, push in a terminal.** The buttons run `git fetch`, `git pull` or `git push` in a terminal pane beside the editor, opened or reused in the repository folder, so credential, SSH-key and 2FA prompts work and the output stays visible. Never force-push.
3. **Branches.** A picker to switch to a branch and create one from HEAD. If Git refuses because of uncommitted work, show its message; never stash or force anything.
4. **Smoke** on this repo: commit, push from the button, and see the ahead count drop to 0.

**Done when:** review → stage → commit → push works inside Compi.

Out of scope for Git: history graphs, interactive rebase, a stash UI, force-push, line-level staging.
