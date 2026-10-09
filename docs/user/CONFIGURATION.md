# Appearance and configuration

Settings includes 41 bundled themes, including **Compi Neutral**, a restrained baseline for clear, blurred, or solid backgrounds. **Theme** selects application and terminal colors together. **Advanced → Terminal colors** defaults to **Follow theme** and allows an optional palette override; Appearance reports an active override. Existing Dark Glass selections, first-run defaults, and legacy split palettes are preserved.

**Transparent background** enables the opacity slider and **Blur background** toggle. Turning transparency off makes the window solid without forgetting opacity or blur preferences. Increase opacity on light or busy desktops, or turn transparency off for predictable contrast; very low opacity cannot guarantee readability against arbitrary wallpaper. Unsupported or reduced-transparency systems fall back to solid backgrounds automatically.

The existing theme catalog supports search, dark/light filters, favorites, previews, and scoped apply/cancel. The applied theme stays first with an inline **Current** selector, even during search or preview; favorites follow, then remaining themes alphabetically. **Import** loads a local Zed `.json` theme family without applying or publishing it; each variant becomes a catalog entry. **More** holds export, attribution, and removal actions. **Export selected** writes standard Zed JSON with existing attribution, license notices, and unused fields preserved. **Remove local** removes only an unused imported variant and its favorite, keeping sibling variants and the external source file; themes referenced by defaults, open windows, or previews are protected.

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
```

Terminal presets include the platform default, JetBrains Mono, IBM Plex Mono, and Atkinson Hyperlegible Mono. Custom installed families remain supported through `font.family`. Bundled font notices and SIL Open Font License 1.1 text are in [`crates/compi-client/fonts/ATTRIBUTION.txt`](../../crates/compi-client/fonts/ATTRIBUTION.txt).

## Pane density

**Settings → Interface → Pane density** chooses how terminal panes sit in every window, independent of themes, prompts and transparency:

- **Comfy** (default): each pane is a rounded island with an 8 px corner radius, separated from its neighbours and the window edge by 6 px gutters. The gutters use a slightly darker shade of the terminal background, or show the window material when the background is translucent. The header has no rule under it, and dragging a gutter resizes the split. The workspace sidebar is an island too; drag the gap beside it to resize it, or double-click the gap to restore its default width.
- **Compact**: panes touch, separated by one-pixel seams, and the sidebar sits flush against the window edge, for the most terminal space.

When more than one pane is visible, the focused pane has a thin accent outline; terminal colours are unchanged. Floating panes keep their shadow and title strip and use the same corner radius. Switching density resizes terminals through the normal resize path, so running programs keep running and reflow once.

## Tab metadata

**Settings → Interface → Tab metadata** writes independent global caption/hover preferences:

```toml
[metadata]
directory = true
process = false
git = false
dimensions = false
```

A manual tab label takes precedence over automatic names. Enabled fields follow the focused pane, with a first-layout-pane fallback; pane count and dirty/stale indicators remain separate from truncated text. The hover card lists every pane in layout order. Collection uses the shell's actual Unix/WSL/SSH environment, caches results for five seconds, and marks stale/unavailable data explicitly. Disabling every field stops GUI metadata queries; CLI inspection remains available on demand.

## Closing tabs and panes

Closing a tab (its ×, **Close terminal tab**) or a pane (**Close pane**) ends its processes in one click. **Settings → Interface → Confirm before closing** asks first instead; Enter closes and Esc cancels. To keep a tab's processes running, hide it instead.

```toml
[workspace]
confirm_close = false
```

Removing a whole workspace and restarting the daemon always ask. The `compi` CLI keeps its own interactive confirmation.

[Back to README](../../README.md)
