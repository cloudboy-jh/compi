//! Standard Zed theme-family JSON, shared by the build script and runtime.
//!
//! Unknown fields are retained, never interpreted as paths, URLs, or executable content.
//! Published Zed v0.2.0 field shapes and colors are validated, including fields
//! Compi does not render; genuinely unknown fields remain opaque.

use std::collections::HashSet;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_VARIANTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeFamily {
    pub name: String,
    pub author: String,
    pub themes: Vec<ThemeVariant>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeVariant {
    pub name: String,
    pub appearance: ThemeAppearance,
    pub style: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeAppearance {
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTheme {
    /// Background, surface, hover, border, foreground, muted, accent, error, selection.
    pub application: [u32; 9],
    /// Background, foreground, selection, cursor, then ANSI 0 through 15.
    pub terminal: [u32; 20],
}

/// Parse and validate every variant before accepting any part of a family.
pub fn parse(bytes: &[u8]) -> Result<ThemeFamily, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("theme family exceeds {MAX_BYTES} bytes"));
    }
    let family: ThemeFamily = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid Zed theme family: {error}"))?;
    validate(&family)?;
    Ok(family)
}

/// Serialize standard Zed JSON, retaining unsupported fields and bounding output.
pub fn encode(family: &ThemeFamily) -> Result<Vec<u8>, String> {
    validate(family)?;
    struct BoundedOutput(Vec<u8>);
    impl Write for BoundedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
                return Err(io::Error::other("theme family exceeds 2 MiB"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedOutput(Vec::new());
    serde_json::to_writer_pretty(&mut output, family)
        .map_err(|error| format!("cannot encode Zed theme family: {error}"))?;
    Ok(output.0)
}

/// Zed accepts #RGB, #RGBA, #RRGGBB, and #RRGGBBAA (case insensitive).
/// Internal colors store inverse alpha above RGB, keeping opaque RGB unchanged.
pub fn parse_color(value: &str) -> Result<u32, String> {
    let hex = value.trim().strip_prefix('#').unwrap_or("");
    if !matches!(hex.len(), 3 | 4 | 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid color {value:?}: expected #RGB, #RGBA, #RRGGBB, or #RRGGBBAA"
        ));
    }
    let number = u32::from_str_radix(hex, 16).map_err(|error| error.to_string())?;
    let (rgb, alpha) = match hex.len() {
        3 | 4 => {
            let rgb = if hex.len() == 4 { number >> 4 } else { number };
            let r = (rgb >> 8) & 15;
            let g = (rgb >> 4) & 15;
            let b = rgb & 15;
            let alpha = if hex.len() == 4 {
                (number & 15) * 17
            } else {
                255
            };
            ((r * 17) << 16 | (g * 17) << 8 | (b * 17), alpha)
        }
        6 => (number, 255),
        8 => (number >> 8, number & 255),
        _ => unreachable!(),
    };
    Ok((255 - alpha) << 24 | rgb)
}

pub fn encode_color(value: u32) -> String {
    let rgb = value & 0x00ff_ffff;
    let alpha = 255 - (value >> 24);
    if alpha == 255 {
        format!("#{rgb:06x}")
    } else {
        format!("#{rgb:06x}{alpha:02x}")
    }
}

// Scalar color properties in https://zed.dev/schema/themes/v0.2.0.json.
// ANSI 0..15 properties are listed separately below because resolution uses
// their terminal order, rather than the schema's alphabetical order.
const COLOR_FIELDS: &[&str] = &[
    "background",
    "border",
    "border.disabled",
    "border.focused",
    "border.selected",
    "border.transparent",
    "border.variant",
    "conflict",
    "conflict.background",
    "conflict.border",
    "created",
    "created.background",
    "created.border",
    "deleted",
    "deleted.background",
    "deleted.border",
    "drop_target.background",
    "editor.active_line.background",
    "editor.active_line_number",
    "editor.active_wrap_guide",
    "editor.background",
    "editor.document_highlight.bracket_background",
    "editor.document_highlight.read_background",
    "editor.document_highlight.write_background",
    "editor.foreground",
    "editor.gutter.background",
    "editor.highlighted_line.background",
    "editor.indent_guide",
    "editor.indent_guide_active",
    "editor.invisible",
    "editor.line_number",
    "editor.subheader.background",
    "editor.wrap_guide",
    "element.active",
    "element.background",
    "element.disabled",
    "element.hover",
    "element.selected",
    "elevated_surface.background",
    "error",
    "error.background",
    "error.border",
    "ghost_element.active",
    "ghost_element.background",
    "ghost_element.disabled",
    "ghost_element.hover",
    "ghost_element.selected",
    "hidden",
    "hidden.background",
    "hidden.border",
    "hint",
    "hint.background",
    "hint.border",
    "icon",
    "icon.accent",
    "icon.disabled",
    "icon.muted",
    "icon.placeholder",
    "ignored",
    "ignored.background",
    "ignored.border",
    "info",
    "info.background",
    "info.border",
    "link_text.hover",
    "modified",
    "modified.background",
    "modified.border",
    "pane.focused_border",
    "pane_group.border",
    "panel.background",
    "panel.focused_border",
    "panel.indent_guide",
    "panel.indent_guide_active",
    "panel.indent_guide_hover",
    "predictive",
    "predictive.background",
    "predictive.border",
    "renamed",
    "renamed.background",
    "renamed.border",
    "scrollbar.thumb.background",
    "scrollbar.thumb.border",
    "scrollbar.thumb.hover_background",
    "scrollbar.track.background",
    "scrollbar.track.border",
    "search.match_background",
    "status_bar.background",
    "success",
    "success.background",
    "success.border",
    "surface.background",
    "tab.active_background",
    "tab.inactive_background",
    "tab_bar.background",
    "terminal.ansi.background",
    "terminal.ansi.dim_black",
    "terminal.ansi.dim_blue",
    "terminal.ansi.dim_cyan",
    "terminal.ansi.dim_green",
    "terminal.ansi.dim_magenta",
    "terminal.ansi.dim_red",
    "terminal.ansi.dim_white",
    "terminal.ansi.dim_yellow",
    "terminal.background",
    "terminal.bright_foreground",
    "terminal.dim_foreground",
    "terminal.foreground",
    "text",
    "text.accent",
    "text.disabled",
    "text.muted",
    "text.placeholder",
    "title_bar.background",
    "title_bar.inactive_background",
    "toolbar.background",
    "unreachable",
    "unreachable.background",
    "unreachable.border",
    "warning",
    "warning.background",
    "warning.border",
];

const ANSI_FIELDS: [&str; 16] = [
    "terminal.ansi.black",
    "terminal.ansi.red",
    "terminal.ansi.green",
    "terminal.ansi.yellow",
    "terminal.ansi.blue",
    "terminal.ansi.magenta",
    "terminal.ansi.cyan",
    "terminal.ansi.white",
    "terminal.ansi.bright_black",
    "terminal.ansi.bright_red",
    "terminal.ansi.bright_green",
    "terminal.ansi.bright_yellow",
    "terminal.ansi.bright_blue",
    "terminal.ansi.bright_magenta",
    "terminal.ansi.bright_cyan",
    "terminal.ansi.bright_white",
];

fn name(value: &str, field: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(format!(
            "{field} must be nonblank and contain no control characters"
        ));
    }
    Ok(())
}

fn reserved(extra: &Map<String, Value>, fields: &[&str]) -> Result<(), String> {
    if let Some(field) = fields.iter().find(|field| extra.contains_key(**field)) {
        return Err(format!("extra fields cannot override {field}"));
    }
    Ok(())
}

fn validate(family: &ThemeFamily) -> Result<(), String> {
    name(&family.name, "family name")?;
    if family.author.chars().any(char::is_control) {
        return Err("author must contain no control characters".into());
    }
    reserved(&family.extra, &["name", "author", "themes"])?;
    if family.themes.is_empty() || family.themes.len() > MAX_VARIANTS {
        return Err(format!(
            "theme family must contain 1 to {MAX_VARIANTS} variants"
        ));
    }
    let mut names = HashSet::with_capacity(family.themes.len());
    for variant in &family.themes {
        if !names.insert(&variant.name) {
            return Err(format!("duplicate theme variant name {:?}", variant.name));
        }
        validate_variant(variant)?;
    }
    Ok(())
}

fn color(style: &Map<String, Value>, field: &str) -> Result<Option<u32>, String> {
    match style.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => parse_color(value)
            .map(Some)
            .map_err(|error| format!("{field}: {error}")),
        Some(_) => Err(format!("{field} must be a hex color string or null")),
    }
}

fn player(style: &Map<String, Value>) -> Result<Option<&Map<String, Value>>, String> {
    match style.get("players") {
        None => Ok(None),
        Some(Value::Array(players)) => match players.first() {
            None => Ok(None),
            Some(Value::Object(player)) => Ok(Some(player)),
            Some(_) => Err("players[0] must be an object".into()),
        },
        Some(_) => Err("players must be an array".into()),
    }
}

fn nullable_choice(value: Option<&Value>, field: &str, choices: &[&str]) -> Result<(), String> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(value)) if choices.contains(&value.as_str()) => Ok(()),
        Some(_) => Err(format!(
            "{field} must be one of {} or null",
            choices.join(", ")
        )),
    }
}

fn validate_collections(style: &Map<String, Value>) -> Result<(), String> {
    if let Some(value) = style.get("players") {
        let players = value.as_array().ok_or("players must be an array")?;
        for (index, value) in players.iter().enumerate() {
            let player = value
                .as_object()
                .ok_or_else(|| format!("players[{index}] must be an object"))?;
            for field in ["cursor", "selection", "background"] {
                color(player, field).map_err(|error| format!("players[{index}].{error}"))?;
            }
        }
    }
    if let Some(value) = style.get("accents") {
        let accents = value.as_array().ok_or("accents must be an array")?;
        for (index, value) in accents.iter().enumerate() {
            match value {
                Value::Null => {}
                Value::String(value) => {
                    parse_color(value).map_err(|error| format!("accents[{index}]: {error}"))?;
                }
                _ => {
                    return Err(format!(
                        "accents[{index}] must be a hex color string or null"
                    ));
                }
            }
        }
    }
    if let Some(value) = style.get("syntax") {
        let syntax = value.as_object().ok_or("syntax must be an object")?;
        for (node, value) in syntax {
            let highlight = value
                .as_object()
                .ok_or_else(|| format!("syntax.{node} must be an object"))?;
            for field in ["color", "background_color"] {
                color(highlight, field).map_err(|error| format!("syntax.{node}.{error}"))?;
            }
            nullable_choice(
                highlight.get("font_style"),
                "font_style",
                &["normal", "italic", "oblique"],
            )
            .map_err(|error| format!("syntax.{node}.{error}"))?;
            match highlight.get("font_weight") {
                None | Some(Value::Null) => {}
                Some(value)
                    if value.as_f64().is_some_and(|weight| {
                        (100.0..=900.0).contains(&weight) && weight % 100.0 == 0.0
                    }) => {}
                Some(_) => {
                    return Err(format!(
                        "syntax.{node}.font_weight must be 100 through 900 in steps of 100 or null"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_variant(variant: &ThemeVariant) -> Result<(), String> {
    let result = (|| {
        name(&variant.name, "variant name")?;
        reserved(&variant.extra, &["name", "appearance", "style"])?;
        for field in COLOR_FIELDS.iter().copied().chain(ANSI_FIELDS) {
            color(&variant.style, field)?;
        }
        nullable_choice(
            variant.style.get("background.appearance"),
            "background.appearance",
            &["opaque", "transparent", "blurred"],
        )?;
        validate_collections(&variant.style)?;
        Ok(())
    })();
    result.map_err(|error: String| format!("theme {:?}: {error}", variant.name))
}

fn first(style: &Map<String, Value>, fields: &[&str], fallback: u32) -> Result<u32, String> {
    for field in fields {
        if let Some(value) = color(style, field)? {
            return Ok(value);
        }
    }
    Ok(fallback)
}

impl ThemeFamily {
    /// Resolve using the first non-null field in each chain:
    ///
    /// - background; surface.background -> elevated_surface.background -> background
    /// - element.hover -> ghost_element.hover -> surface
    /// - border -> border.variant; text -> editor.foreground
    /// - text.muted -> text.placeholder -> foreground
    /// - text.accent -> icon.accent -> border.focused; error -> deleted
    /// - element.selected -> ghost_element.selected
    /// - terminal.background -> editor.background -> application background
    /// - terminal.foreground -> editor.foreground -> application foreground
    /// - players[0].selection -> application selection; players[0].cursor -> accent
    ///
    /// Remaining values use Compi Neutral dark and Catppuccin Latte light
    /// defaults. Each ANSI entry falls back independently to its appearance
    /// default. Explicit transparent colors are values, not missing fields.
    /// Window material settings are validated but do not override Compi settings.
    pub fn resolve(&self, index: usize) -> Result<ResolvedTheme, String> {
        let variant = self
            .themes
            .get(index)
            .ok_or_else(|| format!("theme variant index {index} is out of range"))?;
        validate_variant(variant)?;
        let (defaults, ansi) = match variant.appearance {
            ThemeAppearance::Dark => (
                [
                    0x18191b, 0x222326, 0x2d2f33, 0x4c5057, 0xe9eaec, 0xaeb2b9, 0x99baf0, 0xf18b93,
                    0x35445c,
                ],
                [
                    0x222326, 0xe58b92, 0x9fbd99, 0xd4bd88, 0x99baf0, 0xbdabdc, 0x8cbfc3, 0xd8dade,
                    0x828892, 0xf6a0a6, 0xb1d0aa, 0xe7d09b, 0xaecafa, 0xcfbeeb, 0xa1d1d5, 0xf0f1f3,
                ],
            ),
            ThemeAppearance::Light => (
                [
                    0xeff1f5, 0xe6e9ef, 0xccd0da, 0x9ca0b0, 0x4c4f69, 0x6c6f85, 0x8839ef, 0xd20f39,
                    0xccd0da,
                ],
                [
                    0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
                    0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
                ],
            ),
        };
        let style = &variant.style;
        let background = first(style, &["background"], defaults[0])?;
        let surface = first(
            style,
            &["surface.background", "elevated_surface.background"],
            if color(style, "background")?.is_some() {
                background
            } else {
                defaults[1]
            },
        )?;
        let foreground = first(style, &["text", "editor.foreground"], defaults[4])?;
        let application = [
            background,
            surface,
            first(
                style,
                &["element.hover", "ghost_element.hover"],
                if color(style, "surface.background")?.is_some()
                    || color(style, "elevated_surface.background")?.is_some()
                    || color(style, "background")?.is_some()
                {
                    surface
                } else {
                    defaults[2]
                },
            )?,
            first(style, &["border", "border.variant"], defaults[3])?,
            foreground,
            first(
                style,
                &["text.muted", "text.placeholder"],
                if color(style, "text")?.is_some() || color(style, "editor.foreground")?.is_some() {
                    foreground
                } else {
                    defaults[5]
                },
            )?,
            first(
                style,
                &["text.accent", "icon.accent", "border.focused"],
                defaults[6],
            )?,
            first(style, &["error", "deleted"], defaults[7])?,
            first(
                style,
                &["element.selected", "ghost_element.selected"],
                defaults[8],
            )?,
        ];
        let player = player(style)?;
        let mut terminal = [0; 20];
        terminal[0] = first(
            style,
            &["terminal.background", "editor.background"],
            background,
        )?;
        terminal[1] = first(
            style,
            &["terminal.foreground", "editor.foreground"],
            foreground,
        )?;
        terminal[2] = match player {
            Some(player) => color(player, "selection")?,
            None => None,
        }
        .unwrap_or(application[8]);
        terminal[3] = match player {
            Some(player) => color(player, "cursor")?,
            None => None,
        }
        .unwrap_or(application[6]);
        for (index, field) in ANSI_FIELDS.iter().enumerate() {
            terminal[index + 4] = color(style, field)?.unwrap_or(ansi[index]);
        }
        Ok(ResolvedTheme {
            application,
            terminal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn family(style: Value) -> ThemeFamily {
        parse(
            serde_json::to_vec(&json!({
                "name": "Example", "author": "", "themes": [
                    {"name": "Dark", "appearance": "dark", "style": style}
                ]
            }))
            .unwrap()
            .as_slice(),
        )
        .unwrap()
    }

    #[test]
    fn zed_hex_forms_preserve_alpha_and_channels() {
        for (text, packed) in [
            ("#abc", 0x00aabbcc),
            ("#AbC8", 0x77aabbcc),
            (" #AABBCCff ", 0x00aabbcc),
            ("#12345600", 0xff123456),
            ("#12345680", 0x7f123456),
            ("#000000", 0),
        ] {
            assert_eq!(parse_color(text).unwrap(), packed);
            assert_eq!(parse_color(&encode_color(packed)).unwrap(), packed);
        }
        for invalid in ["red", "123456", "#12", "#12345", "#ggg", "#ééé"] {
            assert!(parse_color(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn unused_zed_content_and_variants_survive_export() {
        let input = json!({
            "$schema": "https://zed.dev/schema/themes/v0.2.0.json",
            "name": "One", "author": "Zed Industries", "custom": {"resource": "not-fetched"},
            "themes": [
                {"name": "One Dark", "appearance": "dark", "future": [1, 2], "style": {
                    "background": "#3b414dff", "panel.focused_border": null,
                    "syntax": {"link_text": {"color": "#73ade9ff", "background_color": "#0000", "font_style": "italic", "font_weight": null, "future": {"weight": true}}},
                    "background.appearance": "blurred",
                    "accents": ["#abc", null, "#12345680"],
                    "players": [{"cursor": "#74ade8ff", "selection": "#74ade83d"}, {"background": null, "cursor": "#abcd", "future": 3}],
                    "future.color": false, "editor.future": {"arbitrary": ["not-a-color", true]}
                }},
                {"name": "One Light", "appearance": "light", "style": {"text": null}}
            ]
        });
        let parsed = parse(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&encode(&parsed).unwrap()).unwrap(),
            input
        );
        assert_eq!(parsed.resolve(0).unwrap().terminal[2], 0xc274ade8);
        assert_ne!(
            parsed.resolve(0).unwrap().application[0],
            parsed.resolve(1).unwrap().application[0]
        );
    }

    #[test]
    fn null_omitted_fields_and_fallback_precedence() {
        let parsed = family(json!({
            "background": "#111111", "surface.background": null,
            "elevated_surface.background": "#222222", "element.hover": "#333333",
            "ghost_element.hover": "#444444", "text": "#555555",
            "editor.foreground": "#666666", "terminal.foreground": "#777777",
            "element.selected": "#888888", "text.accent": "#999999",
            "players": [{"cursor": null, "selection": "#00000000"}],
            "terminal.ansi.bright_red": "#abcdef"
        }));
        let resolved = parsed.resolve(0).unwrap();
        assert_eq!(&resolved.application[..3], &[0x111111, 0x222222, 0x333333]);
        assert_eq!(resolved.application[4], 0x555555);
        assert_eq!(
            &resolved.terminal[..4],
            &[0x111111, 0x777777, 0xff000000, 0x999999]
        );
        assert_eq!(resolved.terminal[13], 0xabcdef);
        let omitted = parse(br#"{"name":"Empty","author":"","themes":[{"name":"Dark","appearance":"dark","style":{}},{"name":"Light","appearance":"light","style":{}}]}"#).unwrap();
        assert_eq!(omitted.author, "");
        let dark_with_nulls =
            family(json!({"background": null, "surface.background": null, "text.accent": null}));
        assert_eq!(
            omitted.resolve(0).unwrap(),
            dark_with_nulls.resolve(0).unwrap()
        );
        let mut light_with_nulls = dark_with_nulls;
        light_with_nulls.themes[0].appearance = ThemeAppearance::Light;
        assert_eq!(
            omitted.resolve(1).unwrap(),
            light_with_nulls.resolve(0).unwrap()
        );
        assert_ne!(
            omitted.resolve(0).unwrap().application,
            omitted.resolve(1).unwrap().application
        );
        assert!(omitted.resolve(2).is_err());
    }

    #[test]
    fn malformed_later_variant_rejects_entire_family() {
        let valid = family(json!({}));
        for style in [
            json!({"text": 7}),
            json!({"border": "#badhex"}),
            json!({"players": [{} , {}], "terminal.ansi.blue": "blue"}),
            json!({"players": [{"selection": false}]}),
            json!({"players": {}}),
            json!({"editor.active_line.background": false}),
            json!({"terminal.dim_foreground": "#nothex"}),
            json!({"players": [{}, 3]}),
            json!({"players": [{}, {"background": false}]}),
            json!({"players": null}),
            json!({"accents": null}),
            json!({"accents": {}}),
            json!({"accents": ["#123456", false]}),
            json!({"accents": ["#badhex"]}),
            json!({"syntax": null}),
            json!({"syntax": []}),
            json!({"syntax": {"comment": null}}),
            json!({"syntax": {"comment": {"background_color": false}}}),
            json!({"syntax": {"comment": {"color": "#badhex"}}}),
            json!({"syntax": {"comment": {"font_style": "bold"}}}),
            json!({"syntax": {"comment": {"font_style": false}}}),
            json!({"syntax": {"comment": {"font_weight": "700"}}}),
            json!({"syntax": {"comment": {"font_weight": 750}}}),
            json!({"background.appearance": "acrylic"}),
            json!({"background.appearance": false}),
        ] {
            let mut invalid = valid.clone();
            invalid.themes.push(ThemeVariant {
                name: "Later".into(),
                appearance: ThemeAppearance::Light,
                style: style.as_object().unwrap().clone(),
                extra: Map::new(),
            });
            let bytes = serde_json::to_vec(&invalid).unwrap();
            assert!(parse(&bytes).is_err());
            assert!(encode(&invalid).is_err());
        }
        let mut invalid = valid.clone();
        invalid.themes.push(valid.themes[0].clone());
        assert!(encode(&invalid).is_err());
        invalid.themes.clear();
        assert!(encode(&invalid).is_err());
        for bytes in [br#"{"name":"x","author":"","themes":[{"name":"x","appearance":"unknown","style":{}}]}"#.as_slice(),
            br#"{"name":" ","author":"","themes":[]}"#.as_slice(),
            br#"{"name":"x","themes":[{"name":"x","appearance":"dark","style":{}}]}"#.as_slice(),
            br#"{"name":"x","author":null,"themes":[{"name":"x","appearance":"dark","style":{}}]}"#.as_slice(),
            br#"{"name":"x","author":"","themes":[{"name":"x","appearance":"dark"}]}"#.as_slice(),
            br#"{"name":"x","author":"","themes":[{"name":"x","appearance":"dark","style":null}]}"#.as_slice(),
            b"{} {}".as_slice()] {
            assert!(parse(bytes).is_err());
        }
    }

    #[test]
    fn family_size_and_variant_boundaries_are_enforced() {
        let mut parsed = family(json!({}));
        let variant = parsed.themes[0].clone();
        parsed.themes = (0..MAX_VARIANTS)
            .map(|index| ThemeVariant {
                name: format!("Variant {index}"),
                ..variant.clone()
            })
            .collect();
        assert_eq!(
            parse(&encode(&parsed).unwrap()).unwrap().themes.len(),
            MAX_VARIANTS
        );
        parsed.themes.push(ThemeVariant {
            name: "Too many".into(),
            ..variant
        });
        assert!(encode(&parsed).is_err());
        assert!(parse(&serde_json::to_vec(&parsed).unwrap()).is_err());
        parsed.themes.truncate(1);
        parsed
            .extra
            .insert("unused".into(), Value::String("x".repeat(MAX_BYTES)));
        assert!(encode(&parsed).is_err());
        assert!(parse(&vec![b' '; MAX_BYTES + 1]).is_err());
        parsed.extra.clear();
        let bytes = encode(&parsed).unwrap();
        let mut boundary = bytes;
        boundary.resize(MAX_BYTES, b' ');
        assert_eq!(parse(&boundary).unwrap(), parsed);
    }
}
