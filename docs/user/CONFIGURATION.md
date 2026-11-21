# Appearance and configuration

Settings includes 41 bundled themes, including **Compi Neutral**, a restrained baseline for clear, blurred, or solid backgrounds. **Theme** selects application and terminal colors together. **Advanced → Terminal colors** defaults to **Follow theme** and allows an optional palette override; Appearance reports an active override. Existing Dark Glass selections, first-run defaults, and legacy split palettes are preserved.

**Transparent background** enables the opacity slider and **Blur background** toggle. Turning transparency off makes the window solid without forgetting opacity or blur preferences. Increase opacity on light or busy desktops, or turn transparency off for predictable contrast; very low opacity cannot guarantee readability against arbitrary wallpaper. Unsupported or reduced-transparency systems fall back to solid backgrounds automatically.

**Settings → Appearance** shows themes as tiles: the current theme first, then favorites and related themes, each with the Compi mark in its colors. Click a tile (or move with the arrow keys) to preview it on your panes; **Apply** (or Enter) keeps it and **Cancel** (or Esc, or leaving Settings) restores the previous one. **All themes ›** opens the full list inside the Settings pane with search, All/Dark/Light/Favorites filters, a favorite star on each theme, and the same preview, Apply and Cancel. The applied theme stays first with a **Current** chip, even during search or preview; favorites follow, then remaining themes alphabetically. **Advanced → Terminal colors → Choose override ›** uses the same list for terminal colors only. The list's **⋯ More** menu holds import, export, attribution and removal. **Import** loads a local Zed `.json` theme family without applying or publishing it; each variant becomes a list entry. **Export selected** writes standard Zed JSON with existing attribution, license notices, and unused fields preserved. **Remove local** removes only an unused imported variant and its favorite, keeping sibling variants and the external source file; themes referenced by defaults, open windows, or previews are protected.

Theme files use [Zed’s published JSON format](https://zed.dev/schema/themes/v0.2.0.json): a family has `name`, `author`, and `themes`; each variant has `name`, `appearance` (`dark` or `light`), and `style`. No Compi ID, custom wrapper, or special filename is required. Import an unmodified downloaded file, such as [Zed’s One Dark/One Light family](https://raw.githubusercontent.com/zed-industries/zed/main/assets/themes/one/one.json), or export a bundled theme as a starting point. RGB/RGBA hex colors and missing/null style values are supported; missing colors use consistent light/dark defaults. Unknown fields are retained for export, not executed. Files are bounded to 2 MiB and 256 variants; invalid families and conflicting variants are rejected without partial installation.

Import copies the family into `themes` under Compi’s data directory. Copying a `.json` file directly into that directory also installs it without applying it; on Windows the directory is `%LOCALAPPDATA%\Compi\themes`. Theme code, includes, assets, and network resources are never executed or fetched. Existing managed `.compi-theme.json` imports migrate once, preserving their saved IDs, selections, favorites, colors, and attribution; verified originals are retained under `.migrated`. Transparency, opacity, and blur remain separate preferences, including when a Zed theme specifies its own background appearance.

`compi theme install FILE.json` uses this same validator and library without opening a window or applying the family. Supported WSL shells resolve relative Linux paths against the invoking cwd and install into the Windows application's data directory; `--connect` is rejected because installation is local application state.

Compact rounded-rectangle tabs use a subtle active fill, transparent inactive tabs, and a faint hover fill. Hover reveals a close button in a reserved slot without moving the label. Tab geometry, keyboard cycling, overflow, reorder, and tear-off are shared across themes.

Five interface-font choices apply globally to Compi's chrome and controls. Four terminal-font choices apply independently to the fixed-width terminal grid.

The native system fonts remain the defaults. Interface selection uses a stable bundled ID, while terminal selection updates the existing `font.family` setting:

```toml
[appearance]
ui_font = "ibm-plex-sans"
density = "comfy" # comfy (rounded panes with gaps) or compact (edge to edge)
theme = "compi-neutral"
terminal_theme = "dark-glass" # remembered optional override
terminal_theme_override = false # Follow theme; true enables the override
transparent_background = true
background_effect = "clear" # clear or blurred; legacy opaque still migrates
terminal_opacity = 0.7 # remembered background opacity, not text opacity

[font]
family = "JetBrains Mono"
size = 14 # 6 through 72
```

Terminal presets include the platform default, JetBrains Mono, IBM Plex Mono, and Atkinson Hyperlegible Mono. Custom installed families remain supported through `font.family`. Bundled font notices and SIL Open Font License 1.1 text are in [`crates/compi-client/fonts/ATTRIBUTION.txt`](../../crates/compi-client/fonts/ATTRIBUTION.txt).

**Settings → Terminal → Font size** sizes terminal text with −, + and **Reset**; with a terminal focused, Ctrl+= and Ctrl+- (Cmd on macOS) step it by one, and Ctrl/Cmd+0 resets it. The size is saved as `font.size` and every window follows it, including after editing the file by hand. Reset removes the key, returning to the default of 14.

**Settings → Terminal → File text size** sizes file panes: the editor, diffs, the Markdown Read view, large text previews, and the Files and Changes lists. With one of them focused, Ctrl+=, Ctrl+- and Ctrl/Cmd+0 change this size instead of the terminal font. Until you set it, file panes follow the terminal font size. It is saved as `files.text_size` (8 through 32); Reset removes the key.

```toml
[files]
text_size = 15 # 8 through 32; unset follows font.size
```

## Chat

**Settings → Chat** holds what Compi controls about agent chats; the model, thinking level and mode are picked in each chat's model picker. A change made in Settings applies to every open window.

- **Text size** sizes agent chats with −, + and **Reset**. The messages and everything around them (header, tool cards, menus, the message box) grow together; the rest of the window keeps its size. With a chat focused, Ctrl+=, Ctrl+- and Ctrl+0 (Cmd on macOS) change the chat text size instead of the terminal font. Reset removes the key, returning to the default of 14.
- **Default agent** picks the agent every new thread starts with (New chat, New thread, a new tab, `compi chat`): **OMP** (the default), **Claude Code** or **Codex**. Settings lists the agents installed where new chats open, in WSL or on Windows, and dims the others with **Not installed**. A split from a chat keeps that chat's agent, and `compi chat --agent omp|claude|codex` overrides it for one chat.
- **Approvals** chooses how the agent asks before it runs tools. **Same as the agent** (the default) uses the agent's own setting, the one it follows in a terminal (OMP's `tools.approvalMode`, which Compi reads when the chat starts and hands to OMP, whose chat mode would otherwise ignore it; Claude Code's permissions; Codex's `approval_policy` and `sandbox_mode`). **Always ask** asks before anything but reading, **Allow edits** runs reads and edits and asks before commands, and **Allow everything** never asks. Each agent gets them in its own terms: OMP's `always-ask`, `write` and `yolo`; Claude Code's `default`, `acceptEdits` and `bypassPermissions`; Codex's approval policy and sandbox `untrusted` + `read-only`, `on-request` + `workspace-write` and `never` + `danger-full-access`. A change applies to new chats and to chats that start again (Restart in the ⋯ menu, or after the daemon restarts); a running chat keeps the mode it started with.
- **New tabs**: **Chat first** opens a new thread in the tab strip's + and Ctrl+T (Cmd+T); **Terminal first** opens a shell. Compi asks once, on first launch; right-click + for the other.
- **Enter while the agent works**: **Steer** (the default) sends the message into the running turn and Ctrl+Enter (Cmd+Enter on macOS) queues it for after; **Queue** swaps the two.
- **Opening a chat**: **In place of the shell** (the default) shows the chat over the terminal it starts from; **Beside it** opens it in a new pane next to that terminal.
- **Agents bar** (on) lists OMP's running sub-agents above the message box.

The model picker remembers, per agent, the model and thinking level you last picked, which new sessions start on, your favorite models and your five latest picks, under `[chat.<program>]`: `[chat.omp]`, `[chat.claude]` and `[chat.codex]`. Values are the agent's own names for its choices (Claude Code's and Codex's are bare, such as `opus` or `gpt-5.5`); one it no longer lists is skipped.

```toml
[chat]
agent = "omp" # omp (OMP), claude (Claude Code) or codex (Codex)
text_size = 14 # 10 through 32
approval = "harness" # harness (Same as the agent), always_ask, write or yolo
enter_while_working = "steer" # steer or queue
open = "in_place" # in_place or beside
new_tab = "chat" # chat or terminal; unset, Compi asks on launch
agents_bar = true

[chat.omp]
model = "anthropic/claude-opus-5-5" # new sessions start on it; unset uses OMP's own
thinking = "high"
favorites = ["anthropic/claude-opus-5-5", "openai-codex/gpt-5.5"]
recent = ["anthropic/claude-opus-5-5"] # newest first, at most five

[chat.claude]
model = "opus" # unset uses Claude Code's own
favorites = ["opus", "sonnet"]

[chat.codex]
model = "gpt-5.5" # unset uses Codex's own (its config.toml's model)
thinking = "high" # Codex's reasoning effort
```

## Projects

A project is the folder a tab or thread works in: its Git repository's top folder, else the folder itself. **Settings → Projects** lists the projects of open tabs under **Open**, then those of recent threads and any other project you set something for under **Recent**, pinned ones first in each. Click a project's card (or press Enter on it) to open its settings; one card is open at a time. Right-clicking a project header in the sidebar and choosing **Project settings…** opens its card here. Every change saves at once and applies in every window.

- **Name** is what tabs' tooltips, the sidebar's project headers and cards, the palette's project list and a new thread's draft call the project; typing it in the palette finds the project, as its folder does. **Rename…** gives it a name; **Reset** (or renaming it to its folder's name) goes back to the folder's.
- **Icon** shows the icon every place uses. **Automatic** is the project's favicon or logo, else its first letter on a coloured tile. **Choose icon…** picks a Lucide icon (search them all by name) or a monogram of one or two letters or numbers, each in a colour of the terminal palette, or an emoji; **Choose file…** takes a PNG, SVG or ICO, which Compi copies into its data folder (`project-icons/`). **Reset** goes back to Automatic.
- **Pin** puts the project first among the sidebar's project headers and in the palette's project list.
- **New threads' agent** is the agent new threads in the project start with (New thread here, New thread, a new tab, New chat, `compi chat` without `--agent`): **Default** follows **Settings → Chat → Default agent**, or pick **OMP**, **Claude Code** or **Codex**. Changing it clears the model.
- **New threads' model** is the model those threads start on: the agent's default (what it starts on elsewhere, see the model picker under [Chat](#chat)), or one of that agent's favorite and recent models. Picking a model while the agent is Default keeps the agent it belongs to as the project's.

Projects are kept under `[projects]`, each under its key: `host:<folder>`, or `wsl:<distribution>:<folder>` for a project in WSL. A project with nothing set has no table.

```toml
[projects."wsl:Ubuntu:/home/me/compi"]
name = "Compi"
icon = { kind = "lucide", name = "rocket", color = 4 } # color: terminal palette index, 0 through 15
pinned = true
agent = "claude" # omp, claude or codex; unset follows [chat] agent
model = "opus" # the agent's own name for the model; unset starts on the agent's default

[projects.'host:C:\Users\me\site']
icon = { kind = "monogram", text = "S", color = 2 } # one or two letters or numbers
# icon = { kind = "emoji", emoji = "🚀" }
# icon = { kind = "file", path = "C:\\Users\\me\\AppData\\…\\project-icons\\….png" }
```

## Pane density

**Settings → Interface → Pane density** chooses how terminal panes sit in every window, independent of themes, prompts and transparency:

- **Comfy** (default): each pane is a rounded island with an 8 px corner radius, separated from its neighbours and the window edge by 6 px gutters. The gutters use a slightly darker shade of the terminal background, or show the window material when the background is translucent. The header has no rule under it, and dragging a gutter resizes the split. The workspace sidebar is an island too; drag the gap beside it to resize it, or double-click the gap to restore its default width.
- **Compact**: panes touch, separated by one-pixel seams, and the sidebar sits flush against the window edge, for the most terminal space.

When more than one pane is visible, the focused pane is marked as set under **Focus indicator** below; terminal colours are unchanged. Floating panes keep their shadow and title strip and use the same corner radius. Switching density resizes terminals through the normal resize path, so running programs keep running and reflow once.

## Tab details

**Settings → Interface → Tab details** writes independent global caption/hover preferences:

```toml
[metadata]
directory = true
process = false
git = false
dimensions = false
```

A manual tab label takes precedence over automatic names. Enabled fields follow the focused pane, with a first-layout-pane fallback; pane count and dirty/stale indicators remain separate from truncated text. The hover card lists every pane in layout order. Collection uses the shell's actual Unix/WSL/SSH environment, caches results for five seconds, and marks stale/unavailable data explicitly. Disabling every field stops GUI metadata queries; CLI inspection remains available on demand.

## Program status

**Settings → Interface → Program status** chooses where Compi shows what programs report through OSC 7501. Each place can be turned off on its own; status is still followed, so turning one back on shows the current state at once.

```toml
[program_status]
tabs = true     # glyph on tab chips
panes = true    # dot on tiled panes, glyph and progress on floating title strips
sidebar = true  # dot on sidebar cards, state at the start of the palette status line
```

## Notifications

**Settings → Notifications** chooses when programs reporting through OSC 7501 notify you, and where. Changes made in Settings apply to every open window; edits to the file apply when Compi next opens.

```toml
[notifications]
delivery = "automatic"  # "automatic" (in Compi while it is active, else from the system), "in_app", "os" or "both"
attention = true        # a program is blocked on you: a permission, a question or a sign-in
finished = false        # a program reports its work done
sound = "off"           # or "default"; Windows: "message", "mail", "reminder", "text_message"; macOS: "basso", "blow", "bottle", "frog", "funk", "glass", "hero", "morse", "ping", "pop", "purr", "sosumi", "submarine", "tink". Once per notification, unless the system is quiet; the other system's sounds play the default
focused_pane_after = 30 # seconds without a key press, click or scroll in Compi before the pane you're in notifies too; 0 never, at most 3600
```

## Closing tabs and panes

Closing a tab (its ×, **Close terminal tab**) or a pane (**Close pane**) ends its processes in one click. **Settings → Interface → Confirm before closing** asks first instead; Enter closes and Esc cancels. To keep a tab's processes running, hide it instead.

```toml
[workspace]
confirm_close = false
```

Removing a whole workspace and restarting the daemon always ask. The `compi` CLI keeps its own interactive confirmation.

## Focus indicator

**Settings → Interface → Focus indicator** chooses how the focused pane stands out when more than one pane is visible:

- **Marker** (default): a short accent bar on the middle of the pane's bottom edge, plus an accent outline that fades within 0.8 s whenever focus moves to a pane.
- **Outline**: a permanent accent outline.
- **Dim others**: every other pane is slightly darkened.
- **None**: only the cursor, which unfocused panes never draw.

```toml
[workspace]
focus_indicator = "marker" # marker, outline, dim, or none
```

Floating panes keep their **Keyboard** label with every option. When the system asks for reduced motion (Windows Animation effects off, or macOS Reduce motion), the arrival outline shows for the same time without fading; this is read when Compi starts.

## Shell (Windows)

Compi picks the shell each new tab or split starts: WSL in a folder only Linux has, Windows PowerShell anywhere else. **Settings → Terminal → Advanced → Shell** overrides it. Open tabs keep their shell. Configurations from before the choice existed stay on WSL. See [Usage](USAGE.md) for what each shell gets.

```toml
[shell]
windows = "automatic" # automatic, powershell, or wsl
working_directory = 'C:\Projects' # where a tab opened from no other pane starts; absent: home
```

## Dev servers

**Settings → Terminal → When a dev server starts** chooses what a terminal does once a local address it printed, such as `http://localhost:5173/`, answers: **Offer to open** (default) shows a card with the address at the pane's bottom right, **Open automatically** shows the page in place of the shell once for each start of the server, and **Do nothing** leaves the terminal as it is.

```toml
[workspace]
dev_server = "offer" # offer, open, or nothing
```

[Back to README](../../README.md)
