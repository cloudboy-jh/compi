# 0.1.4

- Floating terminals
- Arrange panes and tabs
- Shell prompt styles

## New

- **Floating terminals:** float any pane above the window, then move, resize, or dock it back. Floats are restored on relaunch.
- **Arrange panes and tabs:** built-in layouts (Columns, Rows, Grid, Main + stack, Equalize) and saved presets, with mirror, flip, swap, and restore. Combine tabs into one and split them back out.
- **Shell prompt:** pick an Oh My Posh or Starship style in Settings → Terminal, with live previews. It applies to Compi shells, and optionally outside Compi too. Includes backups, history, and turn off.

## Improvements

- Thinner split and sidebar seams.
- Right-click a tab to open its menu without switching to it.
- Shorter button and command labels.
- What's new appears once in the sidebar after an update, and stays in Settings → Updates.

## Updating

- Updating restarts the Compi daemon, which ends running shells. Save your work first.

## Limitations

- Shell prompt works with Bash and Zsh only. Compi does not install Oh My Posh or Starship. Starship is not yet tested.
- macOS is not natively tested for floating panes, layouts, or the shell prompt.
- Windows builds are unsigned and macOS builds are ad-hoc signed, so the OS may show warnings.
