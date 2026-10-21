# 0.3.0

- Files, editing, diffs, Settings and Monitor in your terminal's own pane
- Full scrollback that stays put while output arrives
- Markdown notes with diagrams, maths, wikilinks and backlinks

## New

- Open a view in place without losing the shell underneath. Use Ctrl/Cmd+Shift+E or the pane menu to switch between Files and the terminal; move a view to its own pane when you want both visible.
- Monitor adds CPU, memory, GPU, disk and network detail pages, per-core and per-device views, and a process inspector with recent history. Quit and Force quit require confirmation and check process identity. Process columns can be resized, reordered and hidden, with choices saved.
- Monitor Windows or an individual WSL distribution without mixing their totals or processes. Connected SSH windows monitor their connected host. Settings now includes a Monitor overview; performance diagnostics live under Advanced.
- Review a historical commit with `compi diff COMMIT [PATH[:LINE]]`. Commit reviews are read-only and compare against the first parent.
- Markdown files have Edit, Render and Diff modes in the existing editor. Render tables, task lists, callouts, Mermaid diagrams and LaTeX maths offline. Follow wikilinks and use the Notes panel for search and backlinks.
- `compi notes [FILE|FOLDER] [--edit]`, `notes search` and `notes backlinks` expose notes from the CLI, including JSON output. Commands, Settings, Monitor totals, processes, sources, inspection and confirmed process quitting are also reachable from the CLI.

## Fixes

- Scrolling keeps your reading position through new output, redraws, history eviction and window resizing. A new-lines indicator takes you back to live output.
- The terminal retains the configured scrollback, including the full default 10,000 lines, instead of an effectively much smaller history.
- Fractional wheel movement accumulates correctly, and pagers can use alternate-screen wheel scrolling.
- Moving between diff files stays within the active commit or working-tree review.
- Ended Windows terminals release Monitor's job handle while keeping their final screen available.

## Limitations

- Render mode is local to the window, not restored after a client restart. Use `compi notes FILE` or Render to reopen it.
- Toggling a task in Render saves the whole current editor buffer, including other unsaved edits. Normal undo and file-conflict protection still apply.
- Unsupported or invalid diagrams and maths show an error with their source. Rendering does not load external files or packages.
- macOS cannot read threads of other users' processes; those values show a dash. Linux app groups use initials rather than native app icons.
- Physical trackpad/mouse qualification, macOS visual and IME checks, and SSH note-file qualification remain open.
- Windows packages are not publisher-signed. The macOS app is ad-hoc signed, not notarized. Operating-system trust warnings may appear; signed update metadata does not replace publisher signing.

## Updating

- Updating ends running shells. Save your work first.
- This release changes the daemon protocol and workspace format. Restart Compi and its daemon together; older versions cannot read the new workspace format. Do not downgrade an existing workspace without a backup.
