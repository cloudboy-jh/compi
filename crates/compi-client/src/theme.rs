//! Whole-application presets and native background material choices.
//! Terminal opacity is applied only to the default terminal canvas; explicit
//! ANSI and true-color cell backgrounds stay fully opaque.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePreset {
    #[default]
    DarkGlass,
    WarmCarbon,
}

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

impl ThemePreset {
    pub const ALL: [Self; 2] = [Self::DarkGlass, Self::WarmCarbon];

    pub const fn id(self) -> &'static str {
        match self {
            Self::DarkGlass => "dark-glass",
            Self::WarmCarbon => "warm-carbon",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::DarkGlass => "Neutral charcoal with crisp lime focus.",
            Self::WarmCarbon => "Softer warm neutrals for long sessions.",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::DarkGlass => "Dark Glass",
            Self::WarmCarbon => "Warm Carbon",
        }
    }

    pub const fn colors(self) -> &'static ThemeColors {
        match self {
            Self::DarkGlass => &DARK_GLASS,
            Self::WarmCarbon => &WARM_CARBON,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.id() == value)
    }
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

const DARK_GLASS: ThemeColors = ThemeColors {
    background: 0x151719,
    surface: 0x202326,
    surface_hover: 0x2b2f33,
    border: 0x42474c,
    foreground: 0xecefee,
    muted: 0xa5aeaf,
    accent: 0xdffb35,
    error: 0xf17c85,
    selection: 0x3e492d,
    cursor: 0xdffb35,
    ansi: [
        0x24282b, 0xf17c85, 0xa5ce82, 0xe8c985, 0x80b9ee, 0xc89de8, 0x7dcbd0, 0xd7dfdc, 0x788287,
        0xff98a0, 0xbce49b, 0xf3dfa2, 0xa4d0fa, 0xdbb5f5, 0x9edfe1, 0xf1f4f2,
    ],
};

const WARM_CARBON: ThemeColors = ThemeColors {
    background: 0x171613,
    surface: 0x211f1a,
    surface_hover: 0x2b2922,
    border: 0x403c31,
    foreground: 0xf4f1e8,
    muted: 0xaaa394,
    accent: 0xdffb35,
    error: 0xe06c75,
    selection: 0x4b5420,
    cursor: 0xdffb35,
    ansi: [
        0x1d1b18, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xd8d2c7, 0x827a73,
        0xff7a85, 0xb4d88a, 0xffd68a, 0x84c4ff, 0xdd91e8, 0x78dce8, 0xf5f0e8,
    ],
};

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
