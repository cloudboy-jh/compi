# 0.1.7

- WSL tab completion: 41.8 s → 0.07 s
- WSL unknown command: 95 → <1 ms
- CLI, many instances: 34 s → 0.5 s
- New tab: no startup bar, ~0.2 s
- New: Comfy panes, CLI, compi update

## Speed

- WSL finds Windows commands in one local folder, not dozens of /mnt/c folders.
- WSL launches skip build-only and duplicate search folders.
- Prompt hooks run once per prompt.

## New

- Comfy panes: rounded panes with gaps, default. Compact keeps them edge to edge (Settings → Interface).
- Tab info: directory, process, Git and size.
- Updates: one status line, one button.
- Repair Compi fixes a broken install.
- End in the tab menu ends a terminal in one click and dims its last screen.

## CLI

- compi controls workspaces, tabs, panes, layouts and windows from any terminal or script.
- compi split --panes 4 --layout grid builds the whole layout in one step.
- compi pane capture reads a pane's screen; compi pane send types into it.
- Every command has --json output and its own --help, for scripts and agents.
- Commands print one line, or nothing for send and run; --json keeps the full result.
- Fast back-to-back commands no longer fail while a window resizes panes.
- compi --version prints the version.
- compi update checks, downloads and restarts, asking first and listing shells that end.
- compi changes shows the running version and what changed in it.
- compi theme install adds Zed themes.
- Works inside Compi's WSL shells: clean output, and prompts can be answered.
- Without --instance, it finds the running Compi at once; --instance '' picks the default.

## Fixes

- Blank tabs that blocked new tabs.
- Shells ended by an update or restart come back on their own, in their last folder.
- The recovery notice no longer shows on every launch.
- Recovery overlays and hover cards removed.
- Setup moves the service to the new version and asks before ending shells.
- Setup accepts your own background service.
- Remove works from any Setup file and deletes the whole folder.
- Terminal scrolling and query replies.
- Readable Setup log.
- Closing the daemon console keeps it running.

## Updating

- Updating ends running shells. Save your work first.
- On 0.1.3, use Compi-Setup.exe from this release.
