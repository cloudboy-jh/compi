# Appearance and configuration

Settings includes 41 bundled themes, including **Compi Neutral**, a restrained baseline for clear, blurred, or solid backgrounds. **Theme** selects application and terminal colors together. **Advanced → Terminal colors** defaults to **Follow theme** and allows an optional palette override; Appearance reports an active override. Existing Dark Glass selections, first-run defaults, and legacy split palettes are preserved.

**Transparent background** enables the opacity slider and **Blur background** toggle. Turning transparency off makes the window solid without forgetting opacity or blur preferences. Increase opacity on light or busy desktops, or turn transparency off for predictable contrast; very low opacity cannot guarantee readability against arbitrary wallpaper. Unsupported or reduced-transparency systems fall back to solid backgrounds automatically.

The existing theme catalog supports search, dark/light filters, favorites, previews, and scoped apply/cancel. The applied theme stays first with an inline **Current** selector, even during search or preview; favorites follow, then remaining themes alphabetically. **Import…** loads a local Zed `.json` theme family without applying or publishing it; each variant becomes a catalog entry. **More…** holds export, attribution, and removal actions. **Export selected…** writes standard Zed JSON with existing attribution, license notices, and unused fields preserved. **Remove local…** removes only an unused imported variant and its favorite, keeping sibling variants and the external source file; themes referenced by defaults, open windows, or previews are protected.

Theme files use [Zed’s published JSON format](https://zed.dev/schema/themes/v0.2.0.json): a family has `name`, `author`, and `themes`; each variant has `name`, `appearance` (`dark` or `light`), and `style`. No Compi ID, custom wrapper, or special filename is required. Import an unmodified downloaded file, such as [Zed’s One Dark/One Light family](https://raw.githubusercontent.com/zed-industries/zed/main/assets/themes/one/one.json), or export a bundled theme as a starting point. RGB/RGBA hex colors and missing/null style values are supported; missing colors use consistent light/dark defaults. Unknown fields are retained for export, not executed. Files are bounded to 2 MiB and 256 variants; invalid families and conflicting variants are rejected without partial installation.

Import copies the family into `themes` under Compi’s data directory. Copying a `.json` file directly into that directory also installs it without applying it; on Windows the directory is `%LOCALAPPDATA%\Compi\themes`. Theme code, includes, assets, and network resources are never executed or fetched. Existing managed `.compi-theme.json` imports migrate once, preserving their saved IDs, selections, favorites, colors, and attribution; verified originals are retained under `.migrated`. Transparency, opacity, and blur remain separate preferences, including when a Zed theme specifies its own background appearance.

Compact rounded-rectangle tabs use a subtle active fill, transparent inactive tabs, and a faint hover fill. Hover reveals a close button in a reserved slot without moving the label. Tab geometry, keyboard cycling, overflow, reorder, and tear-off are shared across themes.

Five interface-font choices apply globally to Compi's chrome and controls. Four terminal-font choices apply independently to the fixed-width terminal grid.

The native system fonts remain the defaults. Interface selection uses a stable bundled ID, while terminal selection updates the existing `font.family` setting:

```toml
[appearance]
ui_font = "ibm-plex-sans"
theme = "compi-neutral"
terminal_theme = "dark-glass" # remembered optional override
terminal_theme_override = false # Follow theme; true enables the override
transparent_background = true
background_effect = "clear" # clear or blurred; legacy opaque still migrates
terminal_opacity = 0.7 # remembered background opacity, not text opacity

[font]
family = "JetBrains Mono"
```

Terminal presets include the platform default, JetBrains Mono, IBM Plex Mono, and Atkinson Hyperlegible Mono. Custom installed families remain supported through `font.family`. Bundled font notices and SIL Open Font License 1.1 text are in [`crates/compi-client/fonts/ATTRIBUTION.txt`](../crates/compi-client/fonts/ATTRIBUTION.txt).

[Back to README](../README.md)
