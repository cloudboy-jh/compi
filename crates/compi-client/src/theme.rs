//! Application colors, independent terminal palettes, and native materials.

use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};

// Validated bundled data becomes a const-only catalog at build time.
include!(concat!(env!("OUT_DIR"), "/theme_catalog.rs"));

/// License notices for bundled palettes, available to native About/Diagnostics UI.
pub const THEME_ATTRIBUTION: &str = include_str!("../themes/ATTRIBUTION.txt");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundEffect {
    Clear,
    #[default]
    Blurred,
    Opaque,
}

impl BackgroundEffect {
    pub const ALL: [Self; 3] = [Self::Clear, Self::Blurred, Self::Opaque];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Blurred => "blurred",
            Self::Opaque => "opaque",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "Clear",
            Self::Blurred => "Blurred",
            Self::Opaque => "Opaque",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "clear" => Some(Self::Clear),
            "blurred" => Some(Self::Blurred),
            "opaque" => Some(Self::Opaque),
            _ => None,
        }
    }
}

/// Packed theme colors use `0xTTRRGGBB`, where `TT` is inverse alpha (`255 - alpha`).
/// Existing `0xRRGGBB` constants are therefore fully opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeColors {
    pub background: u32,
    pub surface: u32,
    pub surface_hover: u32,
    pub border: u32,
    pub foreground: u32,
    pub muted: u32,
    pub accent: u32,
    pub error: u32,
    pub selection: u32,
}

/// Theme tokens retain inverse alpha like [`ThemeColors`]. Explicit terminal cell backgrounds
/// are rendered opaque; only the semantic default exposes the pane's material transparency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalPalette {
    pub background: u32,
    pub foreground: u32,
    pub selection: u32,
    pub cursor: u32,
    pub ansi: [u32; 16],
}

impl TerminalPalette {
    /// Presets own ANSI 0–15; the standard color cube and gray ramp remain stable.
    pub fn indexed(&self, index: u8) -> u32 {
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let value = index - 16;
                let component = |value: u8| u32::from(if value == 0 { 0 } else { 55 + value * 40 });
                (component(value / 36) << 16)
                    | (component((value % 36) / 6) << 8)
                    | component(value % 6)
            }
            232..=255 => {
                let level = u32::from(8 + (index - 232) * 10);
                (level << 16) | (level << 8) | level
            }
        }
    }
}

/// Persistent identity does not depend on whether a palette is installed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ThemeId(String);

impl ThemeId {
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() <= 96
            && !value.is_empty()
            && value.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            }))
        .then(|| Self(value.to_owned()))
    }

    pub fn id(&self) -> &str {
        &self.0
    }
}

impl Default for ThemeId {
    fn default() -> Self {
        ThemePreset::default().into()
    }
}

impl From<ThemePreset> for ThemeId {
    fn from(preset: ThemePreset) -> Self {
        Self(preset.id().to_owned())
    }
}

impl<'de> Deserialize<'de> for ThemeId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| serde::de::Error::custom("invalid theme identity"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThemeMetadata {
    pub name: Cow<'static, str>,
    pub family: Cow<'static, str>,
    pub description: Cow<'static, str>,
    pub dark: bool,
    pub author: Cow<'static, str>,
    pub source: Cow<'static, str>,
    pub license: Cow<'static, str>,
    pub notices: Cow<'static, str>,
}

/// Immutable palette data, retained by an Arc throughout rendering and previews.
#[derive(Debug, PartialEq, Eq)]
pub struct ThemeDefinition {
    pub(crate) id: ThemeId,
    pub(crate) application: ThemeColors,
    pub(crate) terminal: TerminalPalette,
    pub(crate) metadata: ThemeMetadata,
    pub(crate) imported: bool,
}

impl ThemeDefinition {
    pub fn theme_id(&self) -> &ThemeId {
        &self.id
    }
    pub fn id(&self) -> &str {
        self.id.id()
    }
    pub fn label(&self) -> &str {
        &self.metadata.name
    }
    pub fn family(&self) -> &str {
        &self.metadata.family
    }
    pub fn description(&self) -> &str {
        &self.metadata.description
    }
    pub fn is_dark(&self) -> bool {
        self.metadata.dark
    }
    pub fn colors(&self) -> &ThemeColors {
        &self.application
    }
    pub fn terminal(&self) -> &TerminalPalette {
        &self.terminal
    }
    pub fn is_imported(&self) -> bool {
        self.imported
    }

    pub fn attribution(&self) -> String {
        format!(
            "{}\nAuthor: {}\nSource: {}\nLicense: {}\n\n{}",
            self.label(),
            self.metadata.author,
            self.metadata.source,
            self.metadata.license,
            self.metadata.notices
        )
    }
}

impl ThemePreset {
    pub(crate) fn definition(self) -> Arc<ThemeDefinition> {
        Arc::new(ThemeDefinition {
            id: self.into(),
            application: *self.colors(),
            terminal: *self.terminal(),
            metadata: ThemeMetadata {
                name: self.label().into(),
                family: self.family().into(),
                description: self.description().into(),
                dark: self.is_dark(),
                author: "Compi and upstream palette authors (see notices)".into(),
                source: self.source().into(),
                license: "Bundled palette licenses (see complete notices)".into(),
                notices: THEME_ATTRIBUTION.into(),
            },
            imported: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_valid_identity_survives_persistence_but_paths_are_rejected() {
        let id: ThemeId = serde_json::from_str("\"user-not-installed\"").unwrap();
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"user-not-installed\""
        );
        for invalid in [
            "",
            "../escape",
            "user-a/b",
            "user--name",
            "UPPER",
            "user-a.json",
        ] {
            assert!(ThemeId::parse(invalid).is_none(), "{invalid}");
        }
        assert!(ThemeId::parse(&"a".repeat(97)).is_none());
    }

    #[test]
    fn terminal_indexed_cube_and_ramp_keep_xterm_boundaries() {
        let mut palette = *ThemePreset::CompiNeutral.terminal();
        palette.ansi[15] |= 0x80000000;
        assert_eq!(palette.indexed(15), palette.ansi[15]);
        assert_eq!(palette.indexed(16), 0x000000);
        assert_eq!(palette.indexed(21), 0x0000ff);
        assert_eq!(palette.indexed(231), 0xffffff);
        assert_eq!(palette.indexed(232), 0x080808);
        assert_eq!(palette.indexed(255), 0xeeeeee);
    }
}
