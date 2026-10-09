# Usage

## Files and projects in the terminal

- **Browse files:** `Ctrl+Shift+E` on Windows (`Cmd+Shift+E` on macOS), or “Browse files in terminal pane” in the command palette. The tree replaces only the focused pane's terminal view; it is not an editor or sidebar. Folder and file icons, branch guides, and single-click disclosure arrows show the expanded hierarchy; selection and hover highlight the item rather than the entire pane width. Double-clicking a folder row also expands it. `/` or `Ctrl+F` searches paths below the current folder. Arrow keys select, Enter expands a folder or copies a file path, `C` copies the selected path outside search (use `Ctrl+C` while searching), and Esc returns to the terminal.
- **Change the same shell's directory:** At an interactive Bash/Zsh prompt, run `compi tree`. Select a folder and use **Enter folder** or `Ctrl+Enter`; the shell itself performs `cd`. Esc cancels. The keyboard shortcut opens browsing and copying without changing the shell: a running program or partially typed command must never receive an injected `cd`.
- **Jump to visited directories:** Run `compi z` (or `compi jump`) from the prompt, type to filter, choose with arrows, and press Enter. This integration does not install a bare `z` command; `z orangebox` is not a Compi command. “Jump to project directory” in the palette opens the same picker for path copying; only the shell-origin picker changes cwd. Project history follows OSC 7 cwd reports, including ordinary `cd` in supported interactive shells.
- New tabs and splits inherit the focused pane's reported cwd. Tabs use the configurable metadata caption described below; manually named tabs retain their label, and split tabs show a pane-count badge. Full metadata is available through `compi --json pane list/show`, not a diagnostic hover card. Right-click any tab to open its menu without switching to it; its tab commands (rename, split, hide, …) switch to that tab before running. The menu also shows a compact, scrollable terminal list in layout order; select a terminal to reveal a small action bubble beneath its name inside the list. **Detach** moves that pane's running shell to a new tab without restarting it; **Float** shows that same terminal above the window, even from a tab you are not viewing (it reads **Dock** when the pane already floats, and floating panes are marked in the list); **End** ends its process tree in one click and keeps the final screen, dimmed. Detach is disabled for a lone pane. Arrow keys and Enter open the selected terminal's bubble; Tab switches between enabled actions, and Escape returns to the compact list. The command palette's detach action still targets the focused pane. Mouse dragging highlights terminal text; `Ctrl+C` copies a selection instead of interrupting the shell. `Ctrl++` increases font size on Windows; `Ctrl+Enter` is distinct from Enter when a terminal program enables Kitty keyboard disambiguation.
- **Float a pane:** Right-click a pane (or open the palette) and run **Float pane**, or pick it from the pane-actions menu. The same running shell moves into a movable, resizable frame above the window and stays visible while you switch tabs or workspaces; its siblings fill the space it left, and the server's split layout is unchanged. Drag the title to move it and the right/bottom edges or corner to resize; the terminal is resized to fit. **Dock** (on the title strip or as **Dock pane** in the palette) returns it to wherever its tab now places it without restarting anything. Docking never closes or ends work: closing a pane or tab is the separate **Close pane**/**Close terminal tab** action, and ending stays the explicit **End terminal** action. Only one pane receives typing; the floating pane that has it is outlined and labelled **Keyboard**. Click a pane or use **Switch focus between floating and tiled panes** / **Focus next floating pane** to move it. Floats are per window and are restored on relaunch.
- **Arrange panes and tabs:** Run **Arrange panes and tabs** from the palette, or **Arrange** from a tab's right-click menu. Under **Tabs to combine**, click (or press Enter on) any other tab in this workspace to fold its panes into the current tab; click it again to leave that tab out. Built-in arrangements are Columns, Rows, Grid, Main + stack (side), Main + stack (top), and Equalize, which keeps the existing splits and evens their sizes; saved presets appear below them. Clicking or arrowing to a layout only previews it: the preview shows each pane's numbered place, across every combined tab, before anything moves. Double-click, Enter, or **Apply** commits it; **Cancel**, Esc, or clicking outside leaves everything as it was. **M** mirrors left/right and **F** flips top/bottom. Arranging only moves and resizes existing panes: no shell restarts and no pane is replaced. Combined tabs become one tab and disappear from the tab strip. Main + stack puts the focused pane in the large slot. A preset with fewer slots than panes stacks the extra panes in its last slot; a preset with more slots drops the unused ones. **Mirror/Flip pane arrangement** work directly on the current tab, **Swap pane left/right/up/down** exchanges the focused pane with its neighbor, and **Restore previous pane arrangement** undoes the last change: it restores the earlier layout, or, right after combining, splits the tabs back out (restoring a layout again swaps back). **Split merged tabs back out** recreates the combined tabs at any later time, with their names, order, and splits; panes you detached or removed since stay where they are. **Save pane arrangement as preset** stores the tab's split shape, never its terminals, in `[layout_presets.NAME]` of `config.toml` (up to 32 presets, 2–16 panes each; reusing a name replaces it); select a saved preset and press Delete twice to remove it. Floating panes keep floating and dock into their assigned slot. Swap commands have no default shortcuts; bind them under `[keybindings]`, for example `swap_pane_left = "ctrl-alt-left"`.
- **Move a pane:** Hover a pane to reveal the **⋯** grip in the middle of its top edge. Click it for the pane menu, or drag it to move the pane. Over another pane of the same tab, release on an edge zone to place the pane beside it, sharing that pane's space evenly, or on the centre **Swap** to exchange the two; an outline shows exactly where it lands. Release on another tab to add the pane to it on the right; rest on a tab for a moment to switch to it while dragging, then drop beside one of its panes. Release on empty tab-bar space to give the pane its own tab. Esc, or releasing anywhere else, cancels. The shell keeps running: nothing restarts, and **Restore previous pane arrangement** undoes a move within a tab.

Compi installs its bundled Bash/Zsh shell integration into the shell's home directory when starting supported default shells (Windows/WSL2, macOS, and remote Unix daemons); it does not edit your startup files. Explicit custom executable/argument profiles are left untouched. A Bash login profile that replaces `PROMPT_COMMAND` may suppress cwd reports after ordinary `cd`; `compi tree` and `compi z` report the shell's current directory before opening their picker, so they start from the directory you are in even without the prompt hook.

On Windows/WSL2, Compi's default shell finds Windows commands (`pwsh.exe`, `code`, `explorer.exe`, …) through `~/.compi/wincmd.<id>`, small launchers for every command in your Windows search path, instead of WSL's `/mnt/c/...` search directories. Lookups of missing commands and command-name completion stay fast; Windows programs still see your Windows `PATH`. Compi refreshes the launchers when those Windows directories change. Directories your startup files add to `PATH` are kept as they are.

On Windows/WSL2, the tree can browse a shell directory linked into a mounted Windows drive. If the WSL share cannot follow the link, Compi resolves that directory in WSL and retries through its Windows path.

## Shell lifecycle and recovery

New tabs and splits open directly into the terminal canvas. Starting a shell shows no bar, spinner, delayed indicator, or reserved space.

Ended shells, launch failures, disconnected panes, and attachment conflicts retain their terminal canvas without recovery overlays, status buttons, or diagnostic hover cards. Final output and the last observed terminal name remain available after the shell ends. Use the existing pane-actions menu or command palette for explicit restart/attachment actions; inspect errors and exit codes through **Diagnostics** or `compi --json pane show`. Retrying attachment never takes control from another window. **Reconnect window** reconnects without restarting shells.

Shells that ended because Compi's background service restarted (for example after an update) come back on their own: the first time such a tab is on screen, it starts a fresh shell in the directory you were last in, or in its starting directory if that one no longer exists. Earlier output is gone. Shells you end yourself, or that fail to start, stay ended until you restart them.

## Shell prompt

**Settings → Terminal → Shell prompt** (or “Prompt settings” in the palette) styles the prompt your shells already draw with [Oh My Posh](https://ohmyposh.dev) or [Starship](https://starship.rs). Compi does not install either one, has no prompt of its own, and uses each style exactly as the provider defines it. It works where your shells run: the local account, the WSL2 distribution you pick, or the SSH host.

- **Details** shows where Compi is looking, your login shell, provider paths, startup-file lines that already set up a prompt, and any warning, such as an Oh My Posh installed only on Windows that cannot draw a WSL prompt. Bash and Zsh are supported. Fish, PowerShell, and custom launch profiles are not.
- **Pick a style:** every style is listed with its real prompt rendered under its name: your current configuration first, then the provider default, Oh My Posh themes, or Starship presets. Browsing changes nothing, and the prompt is independent of the terminal theme and layout.
- **Apply:** click a style and choose **Apply** in the dropdown under it, or **Cancel**. Compi shells use the style without any edit to your startup files; new ones start with it and running ones switch at their next prompt. **Also use outside Compi** additionally adds a marked `# >>> compi prompt >>>` block to the end of your login shell's `~/.bashrc` or `~/.zshrc`; the dropdown then shows that file's diff first. Nothing else in the file changes, and shells outside Compi need a new shell.
- **Back up, restore, turn off:** each change backs up the managed files to `~/.compi/prompt/backups`. **History** restores any of the last 20 backups and **Turn off** removes every prompt Compi manages, each confirmed in the same dropdown. Your own provider configuration is copied, never edited.

## Command-line workspace control

The public `compi` CLI uses the same persistent workspace as the native client. Supported Compi shells expose the matching CLI to child processes on `PATH` and inherit instance and pane context, including the Windows application bridge from WSL. No agent-specific integration is required:

```sh
compi split --panes 4 --layout grid
compi --json pane list
compi run -- printf '%s\n' 'hello world'
compi pane capture --scope scrollback --format text
```

`--panes` is the total, not the number to add. Existing shells and IDs survive; missing panes are created. A lower total is rejected without deleting anything. Creating missing panes and arranging the final tree is one revision-checked atomic transaction: preflight or revision failure leaves no partial additions. JSON returns one mutation receipt containing every affected pane and surface ID.

Outside a Compi shell, choose an existing daemon with `--instance NAME` or `--connect USER@HOST[:PORT]`; `--instance ''` selects the default instance. Without a target, the CLI uses the only running local daemon and fails with the list of running instances when there are several, including older ones it cannot inspect. CLI commands do not start a missing daemon. Bare `compi` still opens the GUI. Targets are stable `--workspace ID|NAME`, `--tab ID|WORKSPACE/NAME`, `--pane ID|SURFACE_ID`, and native `--window ID`; ambiguous names or windows fail rather than guessing. An explicit pane selects its containing tab/workspace, and explicit parent selectors replace incompatible inherited shell context.

| Command | Actions |
|---|---|
| `workspace` | `list`, `show`, `create NAME`, `rename NAME`, `close` |
| `tab` | `list`, `open [--name NAME] [--cwd PATH]`, `rename NAME`, `focus`, `move --index N [--workspace DEST]`, `detach`, `close`, `merge --with SOURCE... [--layout PRESET]`, `unmerge` |
| `pane` | `list`, `show`, `focus`, `resize [--cols N] [--rows N]`, `swap --with OTHER`, `zoom`, `unzoom`, `float`, `dock`, `close`, `terminate`, `send (--text TEXT | --key KEY)`, `capture [--scope screen|scrollback] [--format text|ansi]` |
| `window` | `list` |
| `split` | `[--right | --down] [--cwd PATH]`, or `--panes TOTAL --layout PRESET [--cwd PATH]` |
| `layout` | `list`, preset ID/name, `mirror`, `flip`, `restore` |
| `update` | `[--check]` |
| Other | `run -- COMMAND [ARG...]`, `attach`, `detach`, `theme install FILE.json`, `changes`, `--version` |

Built-in layouts: `columns`, `rows`, `grid`, `main-side`, `main-top`, `equalize`; named `[layout_presets]` work too. Move indices are zero-based. Resize changes pane geometry through existing dividers or a float frame, not the native window size; at least one dimension is required. GUI presentation commands require an existing native GUI host on the machine executing the CLI and an explicit or uniquely eligible window; they do not open a substitute preview or bypass an open dialog. Only explicit focus commands activate a window; tab detach opens its destination in the background.

`pane send --text` is literal, with no implicit Enter; use `--key enter` to submit. `run` safely quotes POSIX shell argv and submits Enter, but does not wait for completion; it rejects unsupported shells, alternate-screen applications, and control characters. Capture observes screen text/styles without attaching, resizing, or taking input ownership; scrollback includes retained history and screen. `attach` requires an interactive console and an unattached pane; Ctrl-] leaves it. `detach` releases only a CLI console attachment, never a GUI view. `tab detach` transfers the existing native view.

Close confirms interactively before ending live processes. Noninteractive close returns exit 4; there is no `--yes`/`--force` bypass. Scripts must explicitly `pane terminate`, inspect the resulting state, then close. `--json` emits `{ok,result}` or `{ok,error}`; stable IDs, lifetimes, revisions, metadata availability, and mutation receipts are available to automation. Exit codes: 0 success, 1 operation/transport failure, 2 arguments, 3 missing/ambiguous target, 4 confirmation, 5 unavailable daemon/attachment, 6 revision/generation/lifetime conflict. Run `compi COMMAND ACTION --help` for individual syntax.

Without `--json`, commands that change something print one line, for example `New pane pane-…` after a split or `2 new panes: …` after growing a tab, and `pane send` and `run` print nothing on success. `compi --version` prints the version.

`compi changes` prints the running version with its headline and the other highlights, then each section of **What's new** under its title. Like `--version`, it needs no running Compi. `--json` returns `version`, `summary`, `highlights` and `sections` (each with `title` and `notes`).

```text
Compi 0.1.7 · WSL tab completion: 41.8 s → 0.07 s
  WSL unknown command: 95 → <1 ms
  …

Speed
  WSL finds Windows commands in one local folder, not dozens of /mnt/c folders.
  …
```

### Updating from the terminal

`compi update --check` prints the running version and, when there is one, the newer release with its headline:

```text
Compi 0.1.7 · 0.1.8 available
  WSL tab completion: 41.8 s → 0.07 s
```

`compi update` downloads and verifies the same signed package as **Settings → Updates**, on one progress line. It then lists the shells a restart would end, saying so when the shell you typed in is one of them, and asks `Restart now? [y/N]`. Only `y` restarts. The running Compi window performs the restart exactly as the Updates page does: it saves the windows, ends only the listed shells and reopens into the new version, so a listed shell running the command closes with it. With no window open, Compi opens first. Without an interactive terminal, for example from a script or agent, the command stops after the download with exit 4; restart from a terminal or **Settings → Updates**. There is no bypass. `--json` reports the same results, and builds without a release verification key report that updates are unavailable.

## Tab and pane metadata

**Settings → Interface → Tab metadata** controls directory, active process, Git branch/changes, and dimensions independently. Directory is on by default; other fields are off. A manual tab label remains primary. Otherwise the focused pane (or first layout pane) supplies the caption; split tabs keep a pane-count badge and a separate dirty/stale indication. Captions truncate and retain the last meaningful value when metadata is unavailable. Full per-pane metadata is available through CLI inspection; diagnostic hover cards are removed.

Collection runs in the terminal's actual environment: native Unix, the selected WSL distribution, or the SSH daemon host. The process is the foreground job, not the Windows launcher. Git uses the reported cwd; unavailable and stale fields are explicit rather than fabricated. Refresh is cached and bounded, runs off the UI thread, and never attaches or resizes a pane. `compi --json pane list/show` exposes the same environment, process, Git, directory, dimensions, and per-field state.

## Remote connections

Compi starts its sibling server automatically, and relaunching with the same instance name reconnects to that workspace. Use `--working-directory PATH` when you intentionally want new work to start in a specific directory.

To open the native client against an SSH host:

```sh
compi --connect dev@example.com:2222
```

The headless diagnostic client accepts the same `--connect` and optional `--instance` options. For example, `compi-probe --connect dev@example.com workspace` prints the remote hierarchy and lifecycle state, while its session, tab, pane, surface, soak, and shutdown commands operate through the same protocol. `compi-probe tab arrange <tab-id> <preset> [--with <tab-id>]... [--main <pane-id>] [--mirror] [--flip]`, `tab mirror|flip|restore-arrangement|split-merged <tab-id>`, and `pane swap <pane-id> <pane-id>` rearrange existing panes in place; each `--with` merges another tab of the same workspace into `<tab-id>`. Presets are the built-in IDs (`columns`, `rows`, `grid`, `main-side`, `main-top`, `equalize`) or a `[layout_presets]` name from the local configuration. The remote host must provide `compi-daemon` on `PATH`.

[Back to README](../../README.md)
