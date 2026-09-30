//! One-time conversion of the retired managed theme format. Public import never uses this parser.

use std::{fs::File, io::Read, path::Path};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{
    theme::ThemeId,
    theme_file::{self, ThemeAppearance, ThemeFamily, ThemeVariant},
};

const MAX_LEGACY_BYTES: usize = 128 * 1024;
pub(crate) const LEGACY_SUFFIX: &str = ".compi-theme.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyApplication {
    background: String,
    surface: String,
    surface_hover: String,
    border: String,
    foreground: String,
    muted: String,
    accent: String,
    error: String,
    selection: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTerminal {
    background: String,
    foreground: String,
    selection: String,
    cursor: String,
    ansi: [String; 16],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTheme {
    version: u32,
    id: ThemeId,
    name: String,
    family: String,
    description: String,
    mode: ThemeAppearance,
    author: String,
    source: String,
    license: String,
    notices: String,
    application: LegacyApplication,
    terminal: LegacyTerminal,
}

pub(crate) struct MigratedTheme {
    pub id: ThemeId,
    pub family: ThemeFamily,
    pub original: Vec<u8>,
}

pub(crate) fn read(path: &Path) -> Result<MigratedTheme, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_LEGACY_BYTES as u64 {
        return Err("Legacy theme must be a regular file no larger than 128 KiB".into());
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_LEGACY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_LEGACY_BYTES {
        return Err("Legacy theme exceeds 128 KiB".into());
    }
    let legacy: LegacyTheme = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if legacy.version != 1 || !legacy.id.id().starts_with("user-") {
        return Err("Invalid legacy managed theme version or identity".into());
    }
    if path.file_name().and_then(|name| name.to_str())
        != Some(format!("{}{LEGACY_SUFFIX}", legacy.id.id()).as_str())
    {
        return Err("Legacy managed filename must match its saved identity".into());
    }
    for (name, value, limit) in [
        ("name", &legacy.name, 128),
        ("family", &legacy.family, 128),
        ("description", &legacy.description, 4096),
        ("author", &legacy.author, 512),
        ("source", &legacy.source, 4096),
        ("license", &legacy.license, 4096),
    ] {
        if value.trim().is_empty() || value.len() > limit || value.chars().any(char::is_control) {
            return Err(format!("Invalid legacy {name}"));
        }
    }
    if legacy.notices.len() > 64 * 1024
        || legacy
            .notices
            .chars()
            .any(|value| value.is_control() && !matches!(value, '\n' | '\r' | '\t'))
    {
        return Err("Invalid legacy license notices".into());
    }
    let a = legacy.application;
    let t = legacy.terminal;
    let mut style = Map::new();
    for (key, value) in [
        ("background", a.background),
        ("surface.background", a.surface),
        ("element.hover", a.surface_hover),
        ("border", a.border),
        ("text", a.foreground),
        ("text.muted", a.muted),
        ("text.accent", a.accent),
        ("error", a.error),
        ("element.selected", a.selection),
        ("terminal.background", t.background),
        ("terminal.foreground", t.foreground),
    ] {
        validate_rgb(&value)?;
        style.insert(key.into(), Value::String(value));
    }
    let ansi_names = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "bright_black",
        "bright_red",
        "bright_green",
        "bright_yellow",
        "bright_blue",
        "bright_magenta",
        "bright_cyan",
        "bright_white",
    ];
    for (name, value) in ansi_names.into_iter().zip(t.ansi) {
        validate_rgb(&value)?;
        style.insert(format!("terminal.ansi.{name}"), Value::String(value));
    }
    validate_rgb(&t.cursor)?;
    validate_rgb(&t.selection)?;
    style.insert(
        "players".into(),
        json!([{
            "cursor": t.cursor,
            "background": t.cursor,
            "selection": t.selection,
        }]),
    );
    let extra = Map::from_iter([
        ("description".into(), Value::String(legacy.description)),
        ("source".into(), Value::String(legacy.source)),
        ("license".into(), Value::String(legacy.license)),
        ("notices".into(), Value::String(legacy.notices)),
    ]);
    let family = ThemeFamily {
        name: legacy.family,
        author: legacy.author,
        themes: vec![ThemeVariant {
            name: legacy.name,
            appearance: legacy.mode,
            style,
            extra,
        }],
        extra: Map::from_iter([(
            "$schema".into(),
            Value::String("https://zed.dev/schema/themes/v0.2.0.json".into()),
        )]),
    };
    // Conversion must pass the public codec before any managed file is changed.
    theme_file::encode(&family)?;
    Ok(MigratedTheme {
        id: legacy.id,
        family,
        original: bytes,
    })
}

fn validate_rgb(value: &str) -> Result<(), String> {
    if value.len() != 7 {
        return Err("Legacy colors must be #RRGGBB".into());
    }
    theme_file::parse_color(value).map(|_| ())
}
