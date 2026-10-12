# Compi next steps

Remaining phases of features 1 and 2 (editor, then diffs and Git), one phase per session. Product scope lives in the separate features doc; finished work is in [Completed work](COMPLETED.md).

Done: phase 0, GPUI upgrade (`2e2b9fe`); phase 1, content panes (`4b458be`); phase 2, files and editor (see [Completed work](COMPLETED.md#files-and-editor-phase-2--2026-10-11)); phase 3, file safety (see [Completed work](COMPLETED.md#file-safety-phase-3--2026-10-12)); phase 4, diff view (see [Completed work](COMPLETED.md#diff-view-phase-4--2026-10-12)); phase 5, editor and diff (see [Completed work](COMPLETED.md#editor-and-diff-phase-5--2026-10-12)).

**Goal:** edit, save, run, review, commit and push Compi itself inside Compi, without opening Zed.

Left open from phases 2–5: macOS qualification of the editor, its file-safety behaviour and the three macOS fixes; live TUI redraws through ConPTY with synchronized output; IME in the editor; Windows-host and SSH file environments; the Recover unsaved edits list and moving a tab with unsaved edits to another window, which were not exercised live. From the diff work: the `e` expand-all key and the `conflicted` gutter label were not seen live; text selection in the diff; diff settings are not saved between sessions.

## Phase 6a: Stage, commit, discard

Git starts writing. Every write is an explicit action. The daemon runs them one at a time per repository and shows Git's own errors, index-lock conflicts included. The gutter and diff tabs stay display-only.

1. **Write requests (protocol 24).** Stage or unstage paths (`git add` / `git restore --staged`); apply a hunk patch to the index (`git apply --cached`, reversed to unstage); commit with a message, or amend; discard work-tree changes (`git restore`, and removal for untracked files). Each request runs in the repository's environment, through Git for Windows on `/mnt/c` checkouts.
2. **Changes list.** Stage/unstage on each row and section header (all files). Discard on unstaged rows and in the diff tab header, always after a confirmation naming the files.
3. **Hunk staging.** Every hunk in a diff tab gets a Stage button (Unstage in a staged diff). The patch is built from the diff engine's hunks against the index text the tab shows. If the index moved since the tab was read, refresh and refuse instead of guessing.
4. **Commit box** at the top of the Changes list: summary and body, an Amend toggle that loads the last message, and Ctrl/Cmd+Enter. Disabled with nothing staged (unless amending). Shows the staged count. On success, show the new short commit id and clear the box.
5. **Smoke in `cargo dev`.** On this repo under `/mnt/c` and a WSL scratch repo: stage a file, stage one hunk of a two-hunk change, unstage it, commit, amend, discard with confirmation. Check each result with `git status` / `git log` in the terminal.

**Done when:** review → stage (files and hunks) → commit works inside Compi, and nothing is discarded without a confirmation.

## Phase 6b: Remote and branches

1. **Branch status.** The Changes header shows the upstream and ahead/behind (`main ↑2 ↓1`) from the status Compi already reads.
2. **Fetch, pull, push in a terminal.** The buttons run `git fetch`, `git pull` or `git push` in a terminal pane beside the editor, opened or reused in the repository folder, so credential, SSH-key and 2FA prompts work and the output stays visible. Never force-push.
3. **Branches.** A picker to switch to a branch and create one from HEAD. If Git refuses because of uncommitted work, show its message; never stash or force anything.
4. **Smoke** on this repo: commit, push from the button, and see the ahead count drop to 0.

**Done when:** review → stage → commit → push works inside Compi.

Out of scope for Git: history graphs, interactive rebase, a stash UI, force-push, line-level staging.
