# 0.1.9

- Edit files beside your terminal, with tabs, search and safe saves
- Review diffs, stage hunks and commit without leaving Compi
- See status from supporting agents and tools on tabs and panes
- Settings is now a searchable pane with a live theme grid
- One palette for commands, tabs, projects and files

## New

- Content panes show a folder and a code editor next to your shells: `compi files`, `compi edit FILE:LINE`, Open file… (Ctrl+Shift+O, Cmd+P on macOS) or Ctrl-click a path in a terminal.
- Saves keep line endings, warn when a file changed on disk, and never drop unsaved edits; unsaved text is backed up and restored after a crash.
- Each file has one tab with an Edit / Diff switch (Ctrl+Shift+D). Diffs are unified or split, with changed words, folds and a Git gutter in the editor.
- The Changes list stages, unstages and discards files and single hunks, and a commit box commits or amends. The header shows the branch and ahead/behind; Fetch, Pull and Push run in a terminal beside the editor, and a picker switches or creates branches.
- Programs that support the Program Status Protocol (OSC 7501) show idle, working, blocked, done or error on their tab and pane; hover for the details. Each place can be turned off in Settings → Interface.
- Settings opens as a pane (Ctrl+, , Cmd+, on macOS) that splits, floats and closes like any pane. Type to search every section. Themes are a grid you preview on your real panes before applying.
- The palette is a drop-down that searches tabs and panes, commands, visited projects and files in the focused pane's folder. Choosing a project opens a new tab there.
- Right-click opens a small menu for what you clicked instead of the palette. Every pane menu has the same actions: Move to new tab, Float or Dock pane, End process… and Close pane. The tab menu lists its panes, each closable in one click.
- Drag a floating pane by its title onto another pane to dock it there. Panes glide into place when they move, and shells are resized once when they land. With reduced motion turned on, changes are instant.

## Changes

- End process… now asks first.
- Menus, selects and popovers use one look: lists are as wide as their contents and open next to what you clicked.
- Tiled panes show only the ⋯ grip on hover; the × moved to floating pane titles and menus.

## Fixes

- Opening Compi from Start or search no longer flashes a console window.
- Cursor flicker while programs or agents redraw is gone (synchronized output).
- macOS: prompt icons show, every Oh My Posh style is listed, and fullscreen no longer leaves a gap where the window buttons were.

## Updating

- Updating ends running shells. Save your work first.
