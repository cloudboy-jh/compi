//! Whole-application presets and native background material choices.
//! Terminal opacity controls the default terminal canvas and window header;
//! text, controls, and explicit ANSI/true-color cell backgrounds stay opaque.

use serde::{Deserialize, Serialize};

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
}

impl BackgroundEffect {
    pub const ALL: [Self; 2] = [Self::Clear, Self::Blurred];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Blurred => "blurred",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Clear => "Clear",
            Self::Blurred => "Blurred",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "clear" => Some(Self::Clear),
            "blurred" => Some(Self::Blurred),
            _ => None,
        }
    }
}

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
    pub cursor: u32,
    pub ansi: [u32; 16],
}

impl ThemeColors {
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

/// Deliberately not serializable: persistence and tear-off inheritance use only
/// `accepted()`, while rendering uses `visible()`.
#[derive(Clone, Copy, Debug)]
pub struct ThemePreview {
    accepted: ThemePreset,
    preview: Option<ThemePreset>,
}

impl ThemePreview {
    pub const fn new(accepted: ThemePreset) -> Self {
        Self {
            accepted,
            preview: None,
        }
    }

    pub fn visible(&self) -> ThemePreset {
        self.preview.unwrap_or(self.accepted)
    }

    pub const fn accepted(&self) -> ThemePreset {
        self.accepted
    }

    pub fn preview(&mut self, preset: ThemePreset) {
        self.preview = Some(preset);
    }

    pub fn accept(&mut self) -> ThemePreset {
        if let Some(preset) = self.preview.take() {
            self.accepted = preset;
        }
        self.accepted
    }

    pub fn cancel(&mut self) {
        self.preview = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_never_changes_remembered_theme_until_acceptance() {
        let mut theme = ThemePreview::new(ThemePreset::DarkGlass);
        theme.preview(ThemePreset::WarmCarbon);
        assert_eq!(theme.visible(), ThemePreset::WarmCarbon);
        assert_eq!(theme.accepted(), ThemePreset::DarkGlass);
        let destination = ThemePreview::new(theme.accepted());
        assert_eq!(destination.visible(), ThemePreset::DarkGlass);
        theme.cancel();
        assert_eq!(theme.visible(), ThemePreset::DarkGlass);
        theme.preview(ThemePreset::WarmCarbon);
        assert_eq!(theme.accept(), ThemePreset::WarmCarbon);
        theme.preview(ThemePreset::DarkGlass);
        theme.cancel();
        assert_eq!(theme.visible(), ThemePreset::WarmCarbon);
    }
}
