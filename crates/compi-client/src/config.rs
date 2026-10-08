//! Writable TOML schema version 1. Tables: `font`, `appearance`, `layout`,
//! `layout_presets.<name>`, `keybindings`, `shell`, `environment`, `profiles.<name>`,
//! `limits`, `clipboard`, `updates`, `metadata`.
//! `default_profile` selects a named profile over the base shell/environment.
//! Missing settings retain defaults; invalid independent settings are diagnosed.
//! GUI writes preserve comments and unrelated keys through an atomic replacement.

#[cfg(unix)]
use std::fs::File;
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use compi_protocol::{LaunchContext, MAX_GRAPHICS_BYTES, SplitAxis};
use serde::{Deserialize, Serialize};

use crate::{
    arrangement::{MAX_NAMED_PRESETS, Shape, valid_preset_name},
    font_catalog::{TerminalFontPreset, UiFontPreset},
    theme::{BackgroundEffect, ThemeId, WorkspaceDensity},
};

pub const DEFAULT_SIDEBAR_WIDTH: f32 = 280.0;
pub const MIN_SIDEBAR_WIDTH: f32 = 200.0;
pub const MAX_SIDEBAR_WIDTH: f32 = 600.0;
pub const DEFAULT_TERMINAL_OPACITY: f32 = 1.0;
pub const MIN_TERMINAL_OPACITY: f32 = 0.1;
pub const MAX_TERMINAL_OPACITY: f32 = 1.0;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AppearanceSettings {
    pub theme: ThemeId,
    pub terminal_theme: ThemeId,
    pub terminal_theme_override: bool,
    pub transparent_background: bool,
    pub terminal_opacity: f32,
    pub background_effect: BackgroundEffect,
}

impl<'de> Deserialize<'de> for AppearanceSettings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct StoredAppearance {
            theme: ThemeId,
            terminal_theme: Option<ThemeId>,
            terminal_theme_override: Option<bool>,
            transparent_background: Option<bool>,
            terminal_opacity: f32,
            background_effect: BackgroundEffect,
        }
        let stored = StoredAppearance::deserialize(deserializer)?;
        let terminal_theme = stored
            .terminal_theme
            .unwrap_or_else(|| stored.theme.clone());
        Ok(Self {
            terminal_theme_override: stored
                .terminal_theme_override
                .unwrap_or(terminal_theme != stored.theme),
            transparent_background: stored
                .transparent_background
                .unwrap_or(stored.background_effect != BackgroundEffect::Opaque),
            terminal_theme,
            theme: stored.theme,
            terminal_opacity: stored.terminal_opacity,
            background_effect: if stored.background_effect == BackgroundEffect::Opaque {
                BackgroundEffect::Clear
            } else {
                stored.background_effect
            },
        })
    }
}

impl AppearanceSettings {
    /// The stored terminal ID is a remembered override, not the followed palette.
    pub fn effective_terminal_theme(&self) -> &ThemeId {
        if self.terminal_theme_override {
            &self.terminal_theme
        } else {
            &self.theme
        }
    }
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: ThemeId::default(),
            terminal_theme: ThemeId::default(),
            terminal_theme_override: false,
            transparent_background: true,
            terminal_opacity: DEFAULT_TERMINAL_OPACITY,
            background_effect: BackgroundEffect::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueSource {
    #[default]
    Default,
    Configuration,
    CommandLine,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConfigProvenance {
    pub font_family: ValueSource,
    pub font_size: ValueSource,
    pub line_height: ValueSource,
    pub theme: ValueSource,
    pub terminal_opacity: ValueSource,
    pub background_effect: ValueSource,
    pub sidebar_width: ValueSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardPolicy {
    /// Permit bounded, write-only OSC 52 requests from the attached terminal.
    #[default]
    Allow,
    /// Ignore terminal-originated OSC 52 writes; explicit copy/paste still works.
    Deny,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FontSettings {
    pub family: String,
    pub size: f32,
    pub line_height: f32,
    pub fallbacks: Vec<String>,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            family: if cfg!(windows) {
                "Cascadia Mono"
            } else if cfg!(target_os = "macos") {
                "Menlo"
            } else {
                "monospace"
            }
            .to_owned(),
            size: 14.0,
            line_height: 1.35,
            fallbacks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FontOverrides {
    pub family: Option<String>,
    pub size: Option<f32>,
    pub line_height: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Checks on launch when a day has passed, then daily while running.
    pub check_for_updates: bool,
    /// Downloads and verifies an available update after a check; never installs it.
    pub download_updates_automatically: bool,
    pub last_check_unix: Option<u64>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check_for_updates: true,
            download_updates_automatically: false,
            last_check_unix: None,
        }
    }
}

/// Pre-0.1.6 `automatic_checks` value; replaced by `check_for_updates`.
const LEGACY_UPDATE_CHECKS: &str = "automatic_checks";

fn legacy_update_checks(value: &toml::Value) -> Option<bool> {
    match value.as_str()? {
        "never" => Some(false),
        "on_launch" | "on-launch" | "daily" => Some(true),
        _ => None,
    }
}

/// Fields appended to automatic tab captions. Manual labels always win.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetadataSettings {
    pub directory: bool,
    pub process: bool,
    pub git: bool,
    pub dimensions: bool,
}

impl Default for MetadataSettings {
    fn default() -> Self {
        Self {
            directory: true,
            process: false,
            git: false,
            dimensions: false,
        }
    }
}

static CONFIG_WRITES: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoadedConfig {
    pub font: FontSettings,
    /// Pre-CLI defaults used for durable presentation and tear-off inheritance.
    pub configured_font: FontSettings,
    pub appearance: AppearanceSettings,
    pub configured_appearance: AppearanceSettings,
    #[serde(default)]
    pub theme_favorites: Vec<ThemeId>,
    #[serde(default)]
    pub ui_font: UiFontPreset,
    #[serde(default)]
    pub density: WorkspaceDensity,
    pub sidebar_width: f32,
    pub configured_sidebar_width: f32,
    pub keybindings: HashMap<String, String>,
    /// A bad explicit launch selection remains an error, not a default shell.
    pub launch: Result<LaunchContext, String>,
    pub clipboard_policy: ClipboardPolicy,
    #[serde(default)]
    pub updates: UpdateSettings,
    #[serde(default)]
    pub metadata: MetadataSettings,
    pub provenance: ConfigProvenance,
    pub diagnostics: Vec<String>,
    /// Named pane arrangements, shape only, keyed by preset name.
    #[serde(default)]
    pub layout_presets: BTreeMap<String, Shape>,
    /// Selected file, or empty when the native configuration directory is unavailable.
    pub path: PathBuf,
}

impl Default for LoadedConfig {
    fn default() -> Self {
        Self {
            font: FontSettings::default(),
            configured_font: FontSettings::default(),
            appearance: AppearanceSettings::default(),
            configured_appearance: AppearanceSettings::default(),
            theme_favorites: Vec::new(),
            ui_font: UiFontPreset::default(),
            density: WorkspaceDensity::default(),
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            configured_sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            keybindings: HashMap::new(),
            launch: Ok(LaunchContext::default()),
            clipboard_policy: ClipboardPolicy::default(),
            updates: UpdateSettings::default(),
            metadata: MetadataSettings::default(),
            provenance: ConfigProvenance::default(),
            layout_presets: BTreeMap::new(),
            diagnostics: Vec::new(),
            path: PathBuf::new(),
        }
    }
}

impl LoadedConfig {
    pub fn save_metadata_settings(&mut self, metadata: MetadataSettings) -> Result<(), String> {
        update_table(&self.path, "metadata", |table| {
            set_table_value(table, "directory", metadata.directory.into());
            set_table_value(table, "process", metadata.process.into());
            set_table_value(table, "git", metadata.git.into());
            set_table_value(table, "dimensions", metadata.dimensions.into());
        })?;
        self.metadata = metadata;
        Ok(())
    }

    pub fn save_update_settings(&mut self, updates: UpdateSettings) -> Result<(), String> {
        update_table(&self.path, "updates", |table| {
            // The new key takes over the legacy line and its comments.
            if let Some((legacy_key, legacy)) = table.remove_entry(LEGACY_UPDATE_CHECKS)
                && !table.contains_key("check_for_updates")
            {
                let mut key = toml_edit::Key::new("check_for_updates");
                *key.leaf_decor_mut() = legacy_key.leaf_decor().clone();
                let mut value = toml_edit::Value::from(updates.check_for_updates);
                if let Some(previous) = legacy.as_value() {
                    *value.decor_mut() = previous.decor().clone();
                }
                table.insert_formatted(&key, toml_edit::Item::Value(value));
            }
            set_table_value(table, "check_for_updates", updates.check_for_updates.into());
            set_table_value(
                table,
                "download_updates_automatically",
                updates.download_updates_automatically.into(),
            );
            if let Some(timestamp) = updates.last_check_unix {
                set_table_value(
                    table,
                    "last_check_unix",
                    (timestamp.min(i64::MAX as u64) as i64).into(),
                );
            }
        })?;
        self.updates = updates;
        Ok(())
    }

    /// Invocation overrides never replace the configured defaults used to seed
    /// a new slot or reset existing client state.
    pub fn apply_presentation_overrides(
        &mut self,
        theme: Option<&str>,
        sidebar_width: Option<f32>,
    ) {
        if let Some(value) = theme {
            if let Some(theme) = ThemeId::parse(value) {
                self.appearance.terminal_theme = theme.clone();
                self.appearance.theme = theme;
                self.appearance.terminal_theme_override = false;
                self.provenance.theme = ValueSource::CommandLine;
            } else {
                invalid(self, "--theme", "a valid theme ID", "CLI");
            }
        }
        if let Some(width) = sidebar_width {
            if valid_sidebar_width(f64::from(width)) {
                self.sidebar_width = width;
                self.provenance.sidebar_width = ValueSource::CommandLine;
            } else {
                invalid(
                    self,
                    "--sidebar-width",
                    "a finite number from 200 through 600",
                    "CLI",
                );
            }
        }
    }

    /// Persist user-facing global appearance without rewriting comments,
    /// launch configuration, or unknown future keys.
    pub fn save_global_appearance(&mut self, appearance: AppearanceSettings) -> Result<(), String> {
        save_appearance(&self.path, &appearance)?;
        self.provenance.terminal_opacity = ValueSource::Configuration;
        self.provenance.background_effect = ValueSource::Configuration;
        if self.provenance.theme != ValueSource::CommandLine {
            self.appearance.theme = appearance.theme.clone();
            self.appearance.terminal_theme = appearance.terminal_theme.clone();
            self.appearance.terminal_theme_override = appearance.terminal_theme_override;
            self.provenance.theme = ValueSource::Configuration;
        }
        self.appearance.terminal_opacity = appearance.terminal_opacity;
        self.appearance.transparent_background = appearance.transparent_background;
        self.appearance.background_effect = appearance.background_effect;
        self.configured_appearance = appearance;
        Ok(())
    }

    /// Persist global favorites in display order without changing appearance.
    pub fn save_theme_favorites(&mut self, favorites: &[ThemeId]) -> Result<(), String> {
        let mut unique = Vec::with_capacity(favorites.len());
        for theme in favorites {
            if !unique.contains(theme) {
                unique.push(theme.clone());
            }
        }
        update_appearance(&self.path, |table| {
            let favorites = unique
                .iter()
                .map(|theme| theme.id())
                .collect::<toml_edit::Array>();
            set_table_value(table, "favorites", toml_edit::Value::Array(favorites));
        })?;
        self.theme_favorites = unique;
        Ok(())
    }

    /// Persist the global application-chrome font without changing terminal typography.
    pub fn save_ui_font(&mut self, ui_font: UiFontPreset) -> Result<(), String> {
        update_appearance(&self.path, |table| {
            set_table_value(table, "ui_font", ui_font.id().into());
        })?;
        self.ui_font = ui_font;
        Ok(())
    }

    /// Persist the global pane density; it applies to every window and theme.
    pub fn save_density(&mut self, density: WorkspaceDensity) -> Result<(), String> {
        update_appearance(&self.path, |table| {
            set_table_value(table, "density", density.id().into());
        })?;
        self.density = density;
        Ok(())
    }

    /// Persist a bundled terminal font without changing its size, line height, or fallbacks.
    pub fn save_terminal_font(&mut self, terminal_font: TerminalFontPreset) -> Result<(), String> {
        let family = terminal_font.family();
        update_table(&self.path, "font", |table| {
            set_table_value(table, "family", family.into());
        })?;
        self.configured_font.family = family.to_owned();
        if self.provenance.font_family != ValueSource::CommandLine {
            self.font.family = family.to_owned();
            self.provenance.font_family = ValueSource::Configuration;
        }
        Ok(())
    }

    /// Save or replace a named arrangement without touching other settings.
    pub fn save_layout_preset(&mut self, name: &str, shape: &Shape) -> Result<(), String> {
        if !valid_preset_name(name) {
            return Err(format!(
                "Preset names must be 1–{} characters without leading/trailing spaces or control characters",
                crate::arrangement::MAX_PRESET_NAME_CHARS
            ));
        }
        shape.validate().map_err(str::to_owned)?;
        if !self.layout_presets.contains_key(name) && self.layout_presets.len() >= MAX_NAMED_PRESETS
        {
            return Err(format!(
                "At most {MAX_NAMED_PRESETS} layout presets are supported; delete one first"
            ));
        }
        let shape = rounded_shape(shape);
        let toml_edit::Value::InlineTable(root) = shape_to_toml(&shape) else {
            unreachable!("a validated preset has a root split");
        };
        update_table(&self.path, "layout_presets", |table| {
            table.set_implicit(true);
            table.insert(name, toml_edit::Item::Table(root.into_table()));
        })?;
        self.layout_presets.insert(name.to_owned(), shape);
        Ok(())
    }

    pub fn remove_layout_preset(&mut self, name: &str) -> Result<(), String> {
        if !self.layout_presets.contains_key(name) {
            return Err(format!("Layout preset {name:?} does not exist"));
        }
        update_table(&self.path, "layout_presets", |table| {
            table.remove(name);
        })?;
        self.layout_presets.remove(name);
        Ok(())
    }

    /// Check a same-user forwarded configuration before it reaches native UI or
    /// launch code. A stored launch error is a valid, inspectable configuration;
    /// callers must separately require `launch.as_ref()` when creating work.
    pub fn validate_launch(&self) -> Result<(), String> {
        for font in [&self.font, &self.configured_font] {
            if !valid_family(&font.family)
                || !valid_number(f64::from(font.size), 6.0, 72.0)
                || !valid_number(f64::from(font.line_height), 1.0, 3.0)
                || font.fallbacks.iter().any(|family| !valid_family(family))
            {
                return Err("Forwarded configuration has invalid font settings".to_owned());
            }
        }
        if !valid_sidebar_width(f64::from(self.sidebar_width))
            || !valid_sidebar_width(f64::from(self.configured_sidebar_width))
            || !valid_terminal_opacity(f64::from(self.appearance.terminal_opacity))
            || !valid_terminal_opacity(f64::from(self.configured_appearance.terminal_opacity))
        {
            return Err("Forwarded configuration has invalid presentation values".to_owned());
        }
        for (id, shortcut) in &self.keybindings {
            if crate::commands::by_id(id).is_none() {
                return Err(format!(
                    "Forwarded configuration names unknown command {id:?}"
                ));
            }
            crate::commands::validate_shortcut(shortcut)
                .map_err(|error| format!("Invalid shortcut for {id}: {error}"))?;
        }
        if self.layout_presets.len() > MAX_NAMED_PRESETS
            || self
                .layout_presets
                .iter()
                .any(|(name, shape)| !valid_preset_name(name) || shape.validate().is_err())
        {
            return Err("Forwarded configuration has invalid layout presets".to_owned());
        }
        if let Ok(launch) = &self.launch {
            if launch.scrollback_lines > 100_000 || launch.graphics_bytes > MAX_GRAPHICS_BYTES {
                return Err("Forwarded configuration exceeds terminal resource limits".to_owned());
            }
            for value in [
                &launch.profile.executable,
                &launch.profile.working_directory,
                &launch.profile.distribution,
            ]
            .into_iter()
            .flatten()
            {
                if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(
                        "Forwarded launch has an invalid executable, directory, or distribution"
                            .to_owned(),
                    );
                }
            }
            if launch.profile.args.iter().any(|arg| arg.contains('\0'))
                || launch.env.iter().any(|(key, value)| {
                    key.is_empty() || key.contains(['=', '\0']) || value.contains('\0')
                })
            {
                return Err("Forwarded launch contains invalid arguments or environment".to_owned());
            }
        }
        Ok(())
    }
}

fn valid_sidebar_width(width: f64) -> bool {
    valid_number(
        width,
        f64::from(MIN_SIDEBAR_WIDTH),
        f64::from(MAX_SIDEBAR_WIDTH),
    )
}

fn valid_terminal_opacity(opacity: f64) -> bool {
    valid_number(
        opacity,
        f64::from(MIN_TERMINAL_OPACITY),
        f64::from(MAX_TERMINAL_OPACITY),
    )
}

fn table<'a>(
    document: &'a toml::Table,
    key: &str,
    loaded: &mut LoadedConfig,
) -> Option<&'a toml::Table> {
    let value = document.get(key)?;
    match value.as_table() {
        Some(table) => Some(table),
        None => {
            invalid(loaded, key, "a table", "configuration");
            None
        }
    }
}

fn apply_presentation(document: &toml::Table, loaded: &mut LoadedConfig) {
    if let Some(appearance) = table(document, "appearance", loaded) {
        if let Some(value) = appearance.get("theme") {
            if let Some(theme) = value.as_str().and_then(ThemeId::parse) {
                loaded.appearance.terminal_theme = theme.clone();
                loaded.appearance.theme = theme;
                loaded.provenance.theme = ValueSource::Configuration;
            } else {
                invalid(
                    loaded,
                    "appearance.theme",
                    "a valid theme ID",
                    "configuration",
                );
            }
        }
        if let Some(value) = appearance.get("terminal_theme") {
            if let Some(theme) = value.as_str().and_then(ThemeId::parse) {
                loaded.appearance.terminal_theme = theme;
            } else {
                invalid(
                    loaded,
                    "appearance.terminal_theme",
                    "a valid theme ID",
                    "configuration",
                );
            }
        }
        loaded.appearance.terminal_theme_override =
            loaded.appearance.terminal_theme != loaded.appearance.theme;
        if let Some(value) = appearance.get("terminal_theme_override") {
            if let Some(enabled) = value.as_bool() {
                loaded.appearance.terminal_theme_override = enabled;
            } else {
                invalid(
                    loaded,
                    "appearance.terminal_theme_override",
                    "a boolean",
                    "configuration",
                );
            }
        }
        if let Some(value) = appearance.get("favorites") {
            if let Some(favorites) = value.as_array() {
                for (index, value) in favorites.iter().enumerate() {
                    if let Some(theme) = value.as_str().and_then(ThemeId::parse) {
                        if !loaded.theme_favorites.contains(&theme) {
                            loaded.theme_favorites.push(theme);
                        }
                    } else {
                        invalid(
                            loaded,
                            &format!("appearance.favorites[{index}]"),
                            "a valid theme ID",
                            "configuration",
                        );
                    }
                }
            } else {
                invalid(
                    loaded,
                    "appearance.favorites",
                    "an array of valid theme IDs",
                    "configuration",
                );
            }
        }
        if let Some(value) = appearance.get("terminal_opacity") {
            if let Some(opacity) = number(value).filter(|value| valid_terminal_opacity(*value)) {
                loaded.appearance.terminal_opacity = opacity as f32;
                loaded.provenance.terminal_opacity = ValueSource::Configuration;
            } else {
                invalid(
                    loaded,
                    "appearance.terminal_opacity",
                    "a finite number from 0.1 through 1.0",
                    "configuration",
                );
            }
        }
        if let Some(value) = appearance.get("background_effect") {
            if let Some(effect) = value.as_str().and_then(BackgroundEffect::parse) {
                loaded.appearance.background_effect = effect;
                loaded.provenance.background_effect = ValueSource::Configuration;
            } else {
                invalid(
                    loaded,
                    "appearance.background_effect",
                    "opaque, clear or blurred",
                    "configuration",
                );
            }
        }
        loaded.appearance.transparent_background =
            loaded.appearance.background_effect != BackgroundEffect::Opaque;
        if loaded.appearance.background_effect == BackgroundEffect::Opaque {
            loaded.appearance.background_effect = BackgroundEffect::Clear;
        }
        if let Some(value) = appearance.get("transparent_background") {
            if let Some(enabled) = value.as_bool() {
                loaded.appearance.transparent_background = enabled;
            } else {
                invalid(
                    loaded,
                    "appearance.transparent_background",
                    "a boolean",
                    "configuration",
                );
            }
        }
        if let Some(value) = appearance.get("ui_font") {
            if let Some(ui_font) = value.as_str().and_then(UiFontPreset::parse) {
                loaded.ui_font = ui_font;
            } else {
                invalid(
                    loaded,
                    "appearance.ui_font",
                    "a bundled UI font ID",
                    "configuration",
                );
            }
        }
        if let Some(value) = appearance.get("density") {
            if let Some(density) = value.as_str().and_then(WorkspaceDensity::parse) {
                loaded.density = density;
            } else {
                invalid(
                    loaded,
                    "appearance.density",
                    "\"comfy\" or \"compact\"",
                    "configuration",
                );
            }
        }
    }
    if let Some(layout) = table(document, "layout", loaded) {
        if let Some(value) = layout.get("sidebar_width") {
            if let Some(width) = number(value).filter(|width| valid_sidebar_width(*width)) {
                loaded.sidebar_width = width as f32;
                loaded.provenance.sidebar_width = ValueSource::Configuration;
            } else {
                invalid(
                    loaded,
                    "layout.sidebar_width",
                    "a finite number from 200 through 600",
                    "configuration",
                );
            }
        }
        // Visibility is never a seed: every window, including tear-offs, starts closed.
        if layout.contains_key("sidebar_visible") {
            loaded.diagnostics.push("layout.sidebar_visible is not supported: every window starts with its workspace sidebar closed".to_owned());
        }
    }
    if let Some(bindings) = table(document, "keybindings", loaded) {
        for (id, value) in bindings {
            let field = format!("keybindings.{id}");
            if crate::commands::by_id(id).is_none() {
                invalid(
                    loaded,
                    &field,
                    "a registered command identifier",
                    "configuration",
                );
                continue;
            }
            match value.as_str() {
                Some(shortcut) => match crate::commands::validate_shortcut(shortcut) {
                    Ok(()) => {
                        loaded.keybindings.insert(id.clone(), shortcut.to_owned());
                    }
                    Err(expected) => invalid(loaded, &field, expected, "configuration"),
                },
                None => invalid(
                    loaded,
                    &field,
                    "a shortcut string (empty to unbind)",
                    "configuration",
                ),
            }
        }
    }
    if let Some(clipboard) = table(document, "clipboard", loaded)
        && let Some(value) = clipboard.get("policy")
    {
        match value.as_str() {
            Some("allow") => loaded.clipboard_policy = ClipboardPolicy::Allow,
            Some("deny") => loaded.clipboard_policy = ClipboardPolicy::Deny,
            _ => invalid(
                loaded,
                "clipboard.policy",
                "allow or deny (terminal OSC 52 writes only)",
                "configuration",
            ),
        }
    }
}

fn apply_layout_presets(document: &toml::Table, loaded: &mut LoadedConfig) {
    let Some(presets) = table(document, "layout_presets", loaded) else {
        return;
    };
    for (name, value) in presets {
        let field = format!("layout_presets.{name}");
        if loaded.layout_presets.len() >= MAX_NAMED_PRESETS {
            loaded.diagnostics.push(format!(
                "Ignoring {field} in configuration ({}): at most {MAX_NAMED_PRESETS} layout presets are supported",
                loaded.path.display()
            ));
            continue;
        }
        match shape_from_toml(value)
            .filter(|shape| valid_preset_name(name) && shape.validate().is_ok())
        {
            Some(shape) => {
                loaded.layout_presets.insert(name.clone(), shape);
            }
            None => invalid(
                loaded,
                &field,
                "a 1–64 character name and a split table with split = \"right\" or \"down\", a ratio strictly between 0 and 1, and first/second set to \"pane\" or a nested split, holding 2–16 panes",
                "configuration",
            ),
        }
    }
}

fn shape_from_toml(value: &toml::Value) -> Option<Shape> {
    if value.as_str() == Some("pane") {
        return Some(Shape::Slot);
    }
    let table = value.as_table()?;
    if table
        .keys()
        .any(|key| !matches!(key.as_str(), "split" | "ratio" | "first" | "second"))
    {
        return None;
    }
    let axis = match table.get("split")?.as_str()? {
        "right" => SplitAxis::Horizontal,
        "down" => SplitAxis::Vertical,
        _ => return None,
    };
    Some(Shape::Split {
        axis,
        ratio: number(table.get("ratio")?)? as f32,
        first: Box::new(shape_from_toml(table.get("first")?)?),
        second: Box::new(shape_from_toml(table.get("second")?)?),
    })
}

fn shape_to_toml(shape: &Shape) -> toml_edit::Value {
    match shape {
        Shape::Slot => "pane".into(),
        Shape::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let mut table = toml_edit::InlineTable::new();
            let split = match axis {
                SplitAxis::Horizontal => "right",
                SplitAxis::Vertical => "down",
            };
            table.insert("split", split.into());
            table.insert("ratio", rounded_ratio(*ratio).into());
            table.insert("first", shape_to_toml(first));
            table.insert("second", shape_to_toml(second));
            toml_edit::Value::InlineTable(table)
        }
    }
}

/// Four decimals keep hand-edited files readable; the ratio stays inside (0, 1).
fn rounded_ratio(ratio: f32) -> f64 {
    ((f64::from(ratio) * 10_000.0).round() / 10_000.0).clamp(0.0001, 0.9999)
}

fn rounded_shape(shape: &Shape) -> Shape {
    match shape {
        Shape::Slot => Shape::Slot,
        Shape::Split {
            axis,
            ratio,
            first,
            second,
        } => Shape::Split {
            axis: *axis,
            ratio: rounded_ratio(*ratio) as f32,
            first: Box::new(rounded_shape(first)),
            second: Box::new(rounded_shape(second)),
        },
    }
}

fn apply_launch(document: &toml::Table, loaded: &mut LoadedConfig) {
    let mut launch = LaunchContext::default();
    let mut errors = Vec::new();
    let mut shell = toml::Table::new();
    if let Some(value) = document.get("shell") {
        match value.as_table() {
            Some(values) => shell.extend(values.clone()),
            None => errors.push("shell must be a table".to_owned()),
        }
    }
    let mut environment = toml::Table::new();
    if let Some(value) = document.get("environment") {
        match value.as_table() {
            Some(values) => environment.extend(values.clone()),
            None => invalid(
                loaded,
                "environment",
                "a table of environment strings",
                "configuration",
            ),
        }
    }
    if let Some(value) = document.get("default_profile") {
        match value.as_str().filter(|name| valid_family(name)) {
            Some(name) => {
                match document
                    .get("profiles")
                    .and_then(toml::Value::as_table)
                    .and_then(|profiles| profiles.get(name))
                    .and_then(toml::Value::as_table)
                {
                    Some(profile) => {
                        for (key, value) in profile {
                            if key == "environment" {
                                match value.as_table() {
                                    Some(values) => environment.extend(values.clone()),
                                    None => invalid(
                                        loaded,
                                        &format!("profiles.{name}.environment"),
                                        "a table of environment strings",
                                        "configuration",
                                    ),
                                }
                            } else {
                                shell.insert(key.clone(), value.clone());
                            }
                        }
                    }
                    None => errors.push(format!(
                        "default_profile {name:?} does not name a profile table"
                    )),
                }
            }
            None => errors.push("default_profile must be a nonempty profile name".to_owned()),
        }
    }
    for (key, target) in [
        ("executable", &mut launch.profile.executable),
        ("working_directory", &mut launch.profile.working_directory),
        ("distribution", &mut launch.profile.distribution),
    ] {
        if let Some(value) = shell.get(key) {
            if let Some(text) = value
                .as_str()
                .filter(|text| !text.trim().is_empty() && !text.chars().any(char::is_control))
            {
                // Do not split/expand shell strings or test WSL paths on Windows.
                // Host executable and cwd existence is validated by the daemon.
                *target = Some(text.to_owned());
            } else {
                errors.push(format!(
                    "shell.{key} must be nonempty text without control characters"
                ));
            }
        }
    }
    if let Some(value) = shell.get("args") {
        match value.as_array() {
            Some(values) => {
                for (index, value) in values.iter().enumerate() {
                    match value.as_str().filter(|value| !value.contains('\0')) {
                        Some(value) => launch.profile.args.push(value.to_owned()),
                        None => {
                            errors.push(format!("shell.args[{index}] must be a string without NUL"))
                        }
                    }
                }
            }
            None => errors.push("shell.args must be an array of literal arguments".to_owned()),
        }
    }
    if let Some(value) = shell.get("login") {
        match value.as_bool() {
            Some(login) => launch.profile.login = Some(login),
            None => errors.push("shell.login must be true or false".to_owned()),
        }
    }
    for (key, value) in environment {
        if key.is_empty() || key.contains(['=', '\0']) {
            invalid(
                loaded,
                &format!("environment.{key}"),
                "a nonempty name without = or NUL",
                "configuration",
            );
            continue;
        }
        match value.as_str().filter(|value| !value.contains('\0')) {
            Some(value) => {
                launch.env.insert(key, value.to_owned());
            }
            None => invalid(
                loaded,
                &format!("environment.{key}"),
                "a string without NUL",
                "configuration",
            ),
        }
    }
    if let Some(limits) = table(document, "limits", loaded) {
        for (key, max, target) in [
            ("scrollback_lines", 100_000, &mut launch.scrollback_lines),
            (
                "graphics_bytes",
                MAX_GRAPHICS_BYTES as i64,
                &mut launch.graphics_bytes,
            ),
        ] {
            if let Some(value) = limits.get(key) {
                match value.as_integer().filter(|value| (0..=max).contains(value)) {
                    Some(value) => *target = value as usize,
                    None => invalid(
                        loaded,
                        &format!("limits.{key}"),
                        &format!("an integer from 0 through {max}"),
                        "configuration",
                    ),
                }
            }
        }
    }
    if errors.is_empty() {
        loaded.launch = Ok(launch);
    } else {
        let message = format!(
            "Cannot launch configured shell ({}): {}. Fix the configuration before creating work; no fallback executable was selected.",
            loaded.path.display(),
            errors.join("; ")
        );
        loaded.diagnostics.push(message.clone());
        loaded.launch = Err(message);
    }
}

/// Explicit path > COMPI_CONFIG_FILE > native path; CLI font values > file > defaults.
/// A missing native default file is normal. A missing explicitly selected file is diagnosed.
pub fn load(path: Option<&Path>, overrides: FontOverrides) -> LoadedConfig {
    let environment_path = std::env::var_os("COMPI_CONFIG_FILE").filter(|value| !value.is_empty());
    let explicitly_selected = path.is_some() || environment_path.is_some();
    let selected = config_path(path, std::env::consts::OS, |key| {
        if key == "COMPI_CONFIG_FILE" {
            environment_path.clone()
        } else {
            std::env::var_os(key)
        }
    });
    let mut loaded = LoadedConfig::default();
    match selected {
        Ok(path) => {
            loaded.path = path;
            match std::fs::read_to_string(&loaded.path) {
                Ok(source) => apply_source(&source, &mut loaded),
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound && !explicitly_selected => {}
                Err(error) => {
                    let message = format!(
                        "Cannot read configuration {}: {error}; using presentation defaults and blocking new launches until configuration is repaired",
                        loaded.path.display()
                    );
                    loaded.launch = Err(message.clone());
                    loaded.diagnostics.push(message);
                }
            }
        }
        Err(error) => loaded.diagnostics.push(error),
    }
    loaded.configured_font = loaded.font.clone();
    loaded.configured_appearance = loaded.appearance.clone();
    loaded.configured_sidebar_width = loaded.sidebar_width;
    apply_overrides(overrides, &mut loaded);
    loaded
}

fn save_appearance(path: &Path, appearance: &AppearanceSettings) -> Result<(), String> {
    if !valid_terminal_opacity(f64::from(appearance.terminal_opacity)) {
        return Err("Terminal opacity must be between 0.1 and 1.0".to_owned());
    }
    update_appearance(path, |table| {
        set_table_value(table, "theme", appearance.theme.id().into());
        set_table_value(
            table,
            "terminal_theme",
            appearance.terminal_theme.id().into(),
        );
        set_table_value(
            table,
            "terminal_theme_override",
            appearance.terminal_theme_override.into(),
        );
        set_table_value(
            table,
            "transparent_background",
            appearance.transparent_background.into(),
        );
        let opacity = (f64::from(appearance.terminal_opacity) * 1_000_000.0).round() / 1_000_000.0;
        set_table_value(table, "terminal_opacity", opacity.into());
        set_table_value(
            table,
            "background_effect",
            appearance.background_effect.id().into(),
        );
    })
}

fn set_table_value(table: &mut toml_edit::Table, key: &str, mut value: toml_edit::Value) {
    if let Some(previous) = table.get(key).and_then(toml_edit::Item::as_value) {
        *value.decor_mut() = previous.decor().clone();
    }
    table[key] = toml_edit::Item::Value(value);
}

fn update_appearance(
    path: &Path,
    update: impl FnOnce(&mut toml_edit::Table),
) -> Result<(), String> {
    update_table(path, "appearance", update)
}

fn update_table(
    path: &Path,
    table_name: &str,
    update: impl FnOnce(&mut toml_edit::Table),
) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Err("No configuration path could be resolved; see Diagnostics".to_owned());
    }
    let _serialized = CONFIG_WRITES
        .lock()
        .map_err(|_| "Configuration write lock is unavailable".to_owned())?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
    }
    let lock_path = path.with_extension("toml.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("Cannot lock {}: {error}", path.display()))?;
    lock_file
        .lock()
        .map_err(|error| format!("Cannot lock {}: {error}", path.display()))?;
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "version = 1\n".to_owned(),
        Err(error) => return Err(format!("Cannot read {}: {error}", path.display())),
    };
    let mut document = source.parse::<toml_edit::DocumentMut>().map_err(|error| {
        format!(
            "Cannot update malformed TOML in {}: {error}",
            path.display()
        )
    })?;
    if document
        .get("version")
        .and_then(toml_edit::Item::as_integer)
        != Some(1)
    {
        return Err(format!(
            "Cannot update {}: expected version = 1",
            path.display()
        ));
    }
    match document.get(table_name) {
        Some(item) if !item.is_table() => {
            return Err(format!(
                "Cannot update {}: {table_name} must be a table",
                path.display()
            ));
        }
        None => {
            document[table_name] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        Some(_) => {}
    }
    let table = document[table_name]
        .as_table_mut()
        .expect("configuration table was created or validated");
    update(table);

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension("toml.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(document.to_string().as_bytes())?;
        file.sync_all()?;
        drop(file);
        replace_config_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("Cannot save {}: {error}", path.display()))
}

#[cfg(unix)]
fn replace_config_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(temporary, destination)?;
    if let Some(parent) = destination.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(windows)]
fn replace_config_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };
    let temporary: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(temporary.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

// Environment lookup is injected so all native path rules can be exercised on any host.
fn config_path(
    explicit: Option<&Path>,
    platform: &str,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    let value = |key| {
        env(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(path) = value("COMPI_CONFIG_FILE") {
        return Ok(path);
    }
    let directory = match platform {
        "windows" => value("LOCALAPPDATA").map(|path| path.join("Compi")),
        "macos" => value("HOME").map(|path| path.join("Library/Application Support/Compi")),
        _ => value("XDG_CONFIG_HOME")
            .or_else(|| value("HOME").map(|path| path.join(".config")))
            .map(|path| path.join("compi")),
    };
    directory.map(|path| path.join("config.toml")).ok_or_else(|| {
        "Cannot locate the native configuration directory; set COMPI_CONFIG_FILE or --config; using defaults".to_owned()
    })
}

fn apply_source(source: &str, loaded: &mut LoadedConfig) {
    let document = match source.parse::<toml::Table>() {
        Ok(document) => document,
        Err(error) => {
            let message = format!(
                "Invalid TOML in {}: {error}; using presentation defaults and blocking new launches until configuration is repaired",
                loaded.path.display()
            );
            loaded.launch = Err(message.clone());
            loaded.diagnostics.push(message);
            return;
        }
    };
    if document.get("version").and_then(toml::Value::as_integer) != Some(1) {
        let message = format!(
            "Unsupported or missing configuration version in {}; expected version = 1; using presentation defaults and blocking new launches until configuration is repaired",
            loaded.path.display()
        );
        loaded.launch = Err(message.clone());
        loaded.diagnostics.push(message);
        return;
    }
    apply_presentation(&document, loaded);
    apply_layout_presets(&document, loaded);
    apply_launch(&document, loaded);
    if let Some(metadata) = table(&document, "metadata", loaded) {
        for key in ["directory", "process", "git", "dimensions"] {
            if let Some(value) = metadata.get(key) {
                if let Some(enabled) = value.as_bool() {
                    match key {
                        "directory" => loaded.metadata.directory = enabled,
                        "process" => loaded.metadata.process = enabled,
                        "git" => loaded.metadata.git = enabled,
                        _ => loaded.metadata.dimensions = enabled,
                    }
                } else {
                    invalid(
                        loaded,
                        &format!("metadata.{key}"),
                        "true or false",
                        "configuration",
                    );
                }
            }
        }
    }
    if let Some(updates) = table(&document, "updates", loaded) {
        if let Some(value) = updates.get("check_for_updates") {
            match value.as_bool() {
                Some(enabled) => loaded.updates.check_for_updates = enabled,
                None => invalid(
                    loaded,
                    "updates.check_for_updates",
                    "true or false",
                    "configuration",
                ),
            }
        } else if let Some(value) = updates.get(LEGACY_UPDATE_CHECKS) {
            match legacy_update_checks(value) {
                Some(enabled) => loaded.updates.check_for_updates = enabled,
                None => invalid(
                    loaded,
                    "updates.automatic_checks",
                    "never, on_launch, or daily",
                    "configuration",
                ),
            }
        }
        if let Some(value) = updates.get("download_updates_automatically") {
            match value.as_bool() {
                Some(enabled) => loaded.updates.download_updates_automatically = enabled,
                None => invalid(
                    loaded,
                    "updates.download_updates_automatically",
                    "true or false",
                    "configuration",
                ),
            }
        }
        if let Some(value) = updates.get("last_check_unix") {
            match value.as_integer().filter(|value| *value >= 0) {
                Some(value) => loaded.updates.last_check_unix = Some(value as u64),
                None => invalid(
                    loaded,
                    "updates.last_check_unix",
                    "a nonnegative Unix timestamp",
                    "configuration",
                ),
            }
        }
    }
    let Some(font) = document.get("font") else {
        return;
    };
    let Some(font) = font.as_table() else {
        invalid(loaded, "font", "a table", "configuration");
        return;
    };
    if let Some(value) = font.get("family") {
        if let Some(family) = value.as_str().filter(|family| valid_family(family)) {
            loaded.font.family = family.trim().to_owned();
            loaded.provenance.font_family = ValueSource::Configuration;
        } else {
            invalid(
                loaded,
                "font.family",
                "nonempty text of at most 256 characters without control characters",
                "configuration",
            );
        }
    }
    if let Some(value) = font.get("size") {
        if let Some(size) = number(value).filter(|value| valid_number(*value, 6.0, 72.0)) {
            loaded.font.size = size as f32;
            loaded.provenance.font_size = ValueSource::Configuration;
        } else {
            invalid(
                loaded,
                "font.size",
                "a finite number from 6 through 72",
                "configuration",
            );
        }
    }
    if let Some(value) = font.get("line_height") {
        if let Some(height) = number(value).filter(|value| valid_number(*value, 1.0, 3.0)) {
            loaded.font.line_height = height as f32;
            loaded.provenance.line_height = ValueSource::Configuration;
        } else {
            invalid(
                loaded,
                "font.line_height",
                "a finite multiplier from 1 through 3",
                "configuration",
            );
        }
    }
    if let Some(value) = font.get("fallbacks") {
        if let Some(fallbacks) = value.as_array() {
            for (index, fallback) in fallbacks.iter().enumerate() {
                if let Some(family) = fallback.as_str().filter(|family| valid_family(family)) {
                    loaded.font.fallbacks.push(family.trim().to_owned());
                } else {
                    invalid(
                        loaded,
                        &format!("font.fallbacks[{index}]"),
                        "a nonempty font family without control characters, at most 256 characters",
                        "configuration",
                    );
                }
            }
        } else {
            invalid(
                loaded,
                "font.fallbacks",
                "an array of font family names",
                "configuration",
            );
        }
    }
}

fn apply_overrides(overrides: FontOverrides, loaded: &mut LoadedConfig) {
    if let Some(family) = overrides.family {
        if valid_family(&family) {
            loaded.font.family = family.trim().to_owned();
            loaded.provenance.font_family = ValueSource::CommandLine;
        } else {
            invalid(
                loaded,
                "--font-family",
                "nonempty text of at most 256 characters without control characters",
                "CLI",
            );
        }
    }
    if let Some(size) = overrides.size {
        if valid_number(f64::from(size), 6.0, 72.0) {
            loaded.font.size = size;
            loaded.provenance.font_size = ValueSource::CommandLine;
        } else {
            invalid(
                loaded,
                "--font-size",
                "a finite number from 6 through 72",
                "CLI",
            );
        }
    }
    if let Some(height) = overrides.line_height {
        if valid_number(f64::from(height), 1.0, 3.0) {
            loaded.font.line_height = height;
            loaded.provenance.line_height = ValueSource::CommandLine;
        } else {
            invalid(
                loaded,
                "--line-height",
                "a finite multiplier from 1 through 3",
                "CLI",
            );
        }
    }
}

fn number(value: &toml::Value) -> Option<f64> {
    value
        .as_float()
        .or_else(|| value.as_integer().map(|value| value as f64))
}

fn valid_number(value: f64, min: f64, max: f64) -> bool {
    value.is_finite() && (min..=max).contains(&value)
}

fn valid_family(family: &str) -> bool {
    !family.trim().is_empty()
        && family.chars().count() <= 256
        && !family.chars().any(char::is_control)
}

fn invalid(loaded: &mut LoadedConfig, field: &str, expected: &str, origin: &str) {
    loaded.diagnostics.push(format!(
        "Invalid {field} in {origin} ({}): expected {expected}; ignoring this setting",
        loaded.path.display()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str, overrides: FontOverrides) -> LoadedConfig {
        let mut loaded = LoadedConfig {
            path: PathBuf::from("example.toml"),
            ..LoadedConfig::default()
        };
        apply_source(source, &mut loaded);
        loaded.configured_font = loaded.font.clone();
        loaded.configured_appearance = loaded.appearance.clone();
        loaded.configured_sidebar_width = loaded.sidebar_width;
        apply_overrides(overrides, &mut loaded);
        loaded
    }

    #[test]
    fn legacy_and_independent_theme_ids_preserve_missing_library_entries() {
        let legacy = parse(
            "version = 1\n[appearance]\ntheme = 'user-uninstalled'\nfavorites = ['nord', 'user-uninstalled']\nterminal_opacity = 0.65\nbackground_effect = 'clear'",
            FontOverrides::default(),
        );
        assert_eq!(legacy.appearance.theme.id(), "user-uninstalled");
        assert_eq!(legacy.appearance.terminal_theme.id(), "user-uninstalled");
        assert_eq!(legacy.appearance.terminal_opacity, 0.65);
        assert_eq!(legacy.appearance.background_effect, BackgroundEffect::Clear);
        assert_eq!(legacy.theme_favorites[1].id(), "user-uninstalled");
        assert!(legacy.diagnostics.is_empty());

        let independent = parse(
            "version = 1\n[appearance]\ntheme = 'user-uninstalled'\nterminal_theme = 'nord'",
            FontOverrides::default(),
        );
        assert_eq!(independent.appearance.theme.id(), "user-uninstalled");
        assert_eq!(independent.appearance.terminal_theme.id(), "nord");

        let legacy_json = serde_json::json!({
            "theme": "user-uninstalled",
            "terminal_opacity": 0.65,
            "background_effect": "clear"
        });
        let migrated: AppearanceSettings = serde_json::from_value(legacy_json).unwrap();
        assert_eq!(migrated.theme, migrated.terminal_theme);
        let serialized = serde_json::to_value(&independent.appearance).unwrap();
        assert_eq!(serialized["terminal_theme"], "nord");
        let restored: AppearanceSettings = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored, independent.appearance);
    }

    #[test]
    fn legacy_material_and_split_palettes_migrate_without_losing_preferences() {
        for (effect, transparent, remembered) in [
            ("opaque", false, BackgroundEffect::Clear),
            ("clear", true, BackgroundEffect::Clear),
            ("blurred", true, BackgroundEffect::Blurred),
        ] {
            let source = format!(
                "version = 1\n[appearance]\ntheme = 'nord'\nterminal_theme = 'dracula'\nterminal_opacity = 0.43\nbackground_effect = '{effect}'"
            );
            let loaded = parse(&source, FontOverrides::default());
            assert!(loaded.appearance.terminal_theme_override);
            assert_eq!(loaded.appearance.effective_terminal_theme().id(), "dracula");
            assert_eq!(loaded.appearance.transparent_background, transparent);
            assert_eq!(loaded.appearance.terminal_opacity, 0.43);
            assert_eq!(loaded.appearance.background_effect, remembered);
            let migrated: AppearanceSettings = serde_json::from_value(serde_json::json!({
                "theme": "nord",
                "terminal_theme": "dracula",
                "terminal_opacity": 0.43,
                "background_effect": effect,
            }))
            .unwrap();
            assert_eq!(migrated, loaded.appearance);
        }
    }

    #[test]
    fn explicit_follow_and_transparency_flags_preserve_remembered_choices() {
        let loaded = parse(
            "version = 1\n[appearance]\ntheme = 'nord'\nterminal_theme = 'dracula'\nterminal_theme_override = false\ntransparent_background = false\nterminal_opacity = 0.43\nbackground_effect = 'blurred'",
            FontOverrides::default(),
        );
        let mut appearance = loaded.appearance;
        assert_eq!(appearance.effective_terminal_theme().id(), "nord");
        appearance.theme = ThemeId::parse("warm-carbon").unwrap();
        assert_eq!(appearance.effective_terminal_theme().id(), "warm-carbon");
        let restored: AppearanceSettings =
            serde_json::from_value(serde_json::to_value(&appearance).unwrap()).unwrap();
        assert_eq!(restored, appearance);
        appearance.terminal_theme_override = true;
        appearance.transparent_background = true;
        assert_eq!(appearance.effective_terminal_theme().id(), "dracula");
        assert_eq!(appearance.terminal_opacity, 0.43);
        assert_eq!(appearance.background_effect, BackgroundEffect::Blurred);
    }

    #[test]
    fn failed_appearance_and_favorites_save_leave_memory_unchanged() {
        let mut loaded = parse(
            "version = 1\n[appearance]\ntheme = 'warm-carbon'\nterminal_theme = 'nord'\nfavorites = ['nord']",
            FontOverrides::default(),
        );
        loaded.path = PathBuf::new();
        loaded.apply_presentation_overrides(Some("user-cli"), None);
        let before = serde_json::to_value(&loaded).unwrap();
        let appearance = AppearanceSettings {
            theme: ThemeId::parse("user-new").unwrap(),
            terminal_theme: ThemeId::parse("catppuccin-latte").unwrap(),
            terminal_theme_override: true,
            transparent_background: false,
            terminal_opacity: 0.5,
            background_effect: BackgroundEffect::Opaque,
        };
        assert!(loaded.save_global_appearance(appearance).is_err());
        assert_eq!(serde_json::to_value(&loaded).unwrap(), before);
        assert!(
            loaded
                .save_theme_favorites(&[ThemeId::parse("user-new").unwrap()])
                .is_err()
        );
        assert_eq!(serde_json::to_value(&loaded).unwrap(), before);
    }

    #[test]
    fn invalid_executable_blocks_launch_without_discarding_presentation() {
        let loaded = parse(
            "version = 1\n[shell]\nexecutable = ''\n[appearance]\ntheme = 'warm-carbon'\n[font]\nsize = 18",
            FontOverrides::default(),
        );
        assert!(loaded.launch.is_err());
        assert_eq!(loaded.appearance.theme.id(), "warm-carbon");
        assert_eq!(loaded.font.size, 18.0);
        assert!(loaded.validate_launch().is_ok());
        let missing_profile = parse(
            "version = 1\ndefault_profile = 'missing'",
            FontOverrides::default(),
        );
        assert!(missing_profile.launch.is_err());
    }

    #[test]
    fn selected_profile_overrides_base_and_preserves_independent_limits() {
        let loaded = parse(
            "version = 1\ndefault_profile = 'dev'\n[shell]\nexecutable = 'base'\nlogin = true\n[environment]\nKEEP = 'kept'\nREPLACE = 'base'\n[profiles.dev]\nexecutable = 'dev-shell'\nargs = ['literal argument', '$HOME']\nworking_directory = '/work space'\n[profiles.dev.environment]\nREPLACE = 'profile'\nINVALID = 7\n[limits]\nscrollback_lines = 0\ngraphics_bytes = -1\n[clipboard]\npolicy = 'deny'",
            FontOverrides::default(),
        );
        let launch = loaded.launch.unwrap();
        assert_eq!(launch.profile.executable.as_deref(), Some("dev-shell"));
        assert_eq!(launch.profile.login, Some(true));
        assert_eq!(launch.profile.args, ["literal argument", "$HOME"]);
        assert_eq!(
            launch.profile.working_directory.as_deref(),
            Some("/work space")
        );
        assert_eq!(launch.env["KEEP"], "kept");
        assert_eq!(launch.env["REPLACE"], "profile");
        assert!(!launch.env.contains_key("INVALID"));
        assert_eq!(launch.scrollback_lines, 0);
        assert_eq!(
            launch.graphics_bytes,
            LaunchContext::default().graphics_bytes
        );
        assert_eq!(loaded.clipboard_policy, ClipboardPolicy::Deny);
    }

    #[test]
    fn invocation_overrides_leave_reset_and_inheritance_defaults_unchanged() {
        let mut loaded = parse(
            "version = 1\n[appearance]\ntheme = 'warm-carbon'\n[layout]\nsidebar_width = 320\n[font]\nsize = 16",
            FontOverrides {
                size: Some(24.0),
                ..Default::default()
            },
        );
        loaded.apply_presentation_overrides(Some("dark-glass"), Some(400.0));
        assert_eq!(loaded.appearance.theme.id(), "dark-glass");
        assert_eq!(loaded.appearance.terminal_theme.id(), "dark-glass");
        assert_eq!(loaded.configured_appearance.theme.id(), "warm-carbon");
        assert_eq!(
            loaded.configured_appearance.terminal_theme.id(),
            "warm-carbon"
        );
        assert_eq!(loaded.sidebar_width, 400.0);
        assert_eq!(loaded.configured_sidebar_width, 320.0);
        assert_eq!(loaded.font.size, 24.0);
        assert_eq!(loaded.configured_font.size, 16.0);
        assert_eq!(loaded.provenance.font_size, ValueSource::CommandLine);
        loaded.apply_presentation_overrides(Some("Invalid!"), Some(f32::NAN));
        assert_eq!(loaded.appearance.theme.id(), "dark-glass");
        assert_eq!(loaded.sidebar_width, 400.0);
    }

    #[test]
    fn valid_overrides_win_and_invalid_fields_do_not_discard_siblings() {
        let loaded = parse(
            "version = 1\n[font]\nfamily = 'File Font'\nsize = 'large'\nline_height = 1.6\nfallbacks = ['Symbols', 7, 'Emoji']\n[future]\nenabled = true",
            FontOverrides {
                family: Some("CLI Font".into()),
                size: Some(18.0),
                line_height: Some(f32::NAN),
            },
        );
        assert_eq!(loaded.font.family, "CLI Font");
        assert_eq!(loaded.font.size, 18.0);
        assert_eq!(loaded.font.line_height, 1.6);
        assert_eq!(loaded.font.fallbacks, ["Symbols", "Emoji"]);
        assert_eq!(loaded.diagnostics.len(), 3);
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|message| message.contains("font.size"))
        );
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|message| message.contains("font.fallbacks[1]"))
        );
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|message| message.contains("--line-height"))
        );
    }

    #[test]
    fn nonfinite_and_out_of_range_values_recover_independently() {
        let loaded = parse(
            "version = 1\n[font]\nfamily = ''\nsize = inf\nline_height = 0.5\nfallbacks = ['Retained']",
            FontOverrides::default(),
        );
        let defaults = FontSettings::default();
        assert_eq!(loaded.font.family, defaults.family);
        assert_eq!(loaded.font.size, defaults.size);
        assert_eq!(loaded.font.line_height, defaults.line_height);
        assert_eq!(loaded.font.fallbacks, ["Retained"]);
        assert_eq!(loaded.diagnostics.len(), 3);
    }

    #[test]
    fn unsupported_version_and_malformed_document_reject_file_but_allow_cli() {
        for source in [
            "version = 2\n[font]\nfamily = 'Must Not Apply'\nsize = 30",
            "[font]\nfamily = 'Must Not Apply'\nsize = 30",
            "version = '1'\n[font]\nsize = 30",
            "version = 1\n[font]\nsize = 30\nfamily = [",
        ] {
            let loaded = parse(
                source,
                FontOverrides {
                    family: Some("CLI Font".into()),
                    ..Default::default()
                },
            );
            assert_eq!(loaded.font.family, "CLI Font");
            assert_eq!(loaded.font.size, FontSettings::default().size);
            assert_eq!(loaded.diagnostics.len(), 1);
            assert!(loaded.diagnostics[0].contains("example.toml"));
        }
    }

    #[test]
    fn global_appearance_save_is_atomic_preserves_toml_and_respects_cli_precedence() {
        let root = std::env::temp_dir().join(format!(
            "compi-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        fs::write(
            &path,
            "# retained comment\nversion = 1\n[appearance]\ntheme = 'warm-carbon' # theme comment\nfavorites = ['nord'] # favorites comment\n[future]\nanswer = 42\n",
        )
        .unwrap();
        let mut loaded = load(Some(&path), FontOverrides::default());
        loaded.apply_presentation_overrides(Some("dark-glass"), None);
        loaded
            .save_global_appearance(AppearanceSettings {
                theme: crate::theme::ThemeId::from(crate::theme::ThemePreset::WarmCarbon),
                terminal_theme: ThemeId::parse("user-saved-palette").unwrap(),
                terminal_theme_override: true,
                transparent_background: false,
                terminal_opacity: 0.72,
                background_effect: BackgroundEffect::Clear,
            })
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# retained comment"));
        assert!(saved.contains("# theme comment"));
        assert!(saved.contains("# favorites comment"));
        let document = saved.parse::<toml::Table>().unwrap();
        assert_eq!(document["future"]["answer"].as_integer(), Some(42));
        assert_eq!(loaded.appearance.theme.id(), "dark-glass");
        assert_eq!(loaded.appearance.terminal_theme.id(), "dark-glass");
        assert!(!loaded.appearance.terminal_theme_override);
        assert!(!loaded.appearance.transparent_background);
        assert_eq!(loaded.configured_appearance.theme.id(), "warm-carbon");
        assert_eq!(
            loaded.configured_appearance.terminal_theme.id(),
            "user-saved-palette"
        );
        let reloaded = load(Some(&path), FontOverrides::default());
        assert_eq!(reloaded.theme_favorites, [ThemeId::parse("nord").unwrap()]);
        assert_eq!(
            reloaded.appearance.terminal_theme.id(),
            "user-saved-palette"
        );
        assert!(reloaded.appearance.terminal_theme_override);
        assert!(!reloaded.appearance.transparent_background);
        assert_eq!(reloaded.appearance.terminal_opacity, 0.72);
        assert_eq!(
            reloaded.appearance.background_effect,
            BackgroundEffect::Clear
        );
        loaded
            .save_theme_favorites(&[
                crate::theme::ThemeId::from(crate::theme::ThemePreset::CatppuccinLatte),
                crate::theme::ThemeId::from(crate::theme::ThemePreset::Nord),
                crate::theme::ThemeId::from(crate::theme::ThemePreset::CatppuccinLatte),
            ])
            .unwrap();
        let reloaded = load(Some(&path), FontOverrides::default());
        assert_eq!(
            reloaded.theme_favorites,
            [
                ThemeId::parse("catppuccin-latte").unwrap(),
                ThemeId::parse("nord").unwrap()
            ]
        );
        assert_eq!(reloaded.theme_favorites, loaded.theme_favorites);
        assert_eq!(reloaded.appearance.theme.id(), "warm-carbon");
        assert_eq!(
            reloaded.appearance.terminal_theme.id(),
            "user-saved-palette"
        );
        assert_eq!(reloaded.appearance.terminal_opacity, 0.72);
        assert_eq!(
            reloaded.appearance.background_effect,
            BackgroundEffect::Clear
        );
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# favorites comment"));
        assert_eq!(
            saved.parse::<toml::Table>().unwrap()["future"]["answer"].as_integer(),
            Some(42)
        );
        loaded.save_theme_favorites(&[]).unwrap();
        assert!(
            load(Some(&path), FontOverrides::default())
                .theme_favorites
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn font_selections_round_trip_independently() {
        let root = std::env::temp_dir().join(format!(
            "compi-font-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        fs::write(
            &path,
            "version = 1\n[appearance]\nui_font = 'inter'\n[font]\nfamily = 'Terminal Face'\nsize = 15.5\n# retain me\n",
        )
        .unwrap();

        let mut loaded = load(Some(&path), FontOverrides::default());
        assert_eq!(loaded.ui_font, UiFontPreset::Inter);
        assert_eq!(loaded.font.family, "Terminal Face");
        loaded
            .save_ui_font(UiFontPreset::AtkinsonHyperlegibleNext)
            .unwrap();
        loaded
            .save_terminal_font(TerminalFontPreset::JetBrainsMono)
            .unwrap();

        let reloaded = load(Some(&path), FontOverrides::default());
        assert_eq!(reloaded.ui_font, UiFontPreset::AtkinsonHyperlegibleNext);
        assert_eq!(
            reloaded.font.family,
            TerminalFontPreset::JetBrainsMono.family()
        );
        assert_eq!(reloaded.font.size, 15.5);
        assert!(fs::read_to_string(&path).unwrap().contains("# retain me"));

        let mut reloaded = reloaded;
        reloaded.save_ui_font(UiFontPreset::JetBrainsMono).unwrap();
        let jetbrains = load(Some(&path), FontOverrides::default());
        assert_eq!(jetbrains.ui_font, UiFontPreset::JetBrainsMono);
        assert_eq!(
            jetbrains.font.family,
            TerminalFontPreset::JetBrainsMono.family()
        );

        let invalid = parse(
            "version = 1\n[appearance]\nui_font = 'unknown'",
            FontOverrides::default(),
        );
        assert_eq!(invalid.ui_font, UiFontPreset::System);
        assert!(
            invalid
                .diagnostics
                .iter()
                .any(|message| message.contains("appearance.ui_font"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn density_defaults_to_comfy_round_trips_and_rejects_unknown_values() {
        let root = std::env::temp_dir().join(format!(
            "compi-density-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        fs::write(
            &path,
            "version = 1\n[appearance]\nui_font = 'inter'\n# retain me\n",
        )
        .unwrap();

        let mut loaded = load(Some(&path), FontOverrides::default());
        assert_eq!(loaded.density, WorkspaceDensity::Comfy);
        loaded.save_density(WorkspaceDensity::Compact).unwrap();
        let reloaded = load(Some(&path), FontOverrides::default());
        assert_eq!(reloaded.density, WorkspaceDensity::Compact);
        assert_eq!(reloaded.ui_font, UiFontPreset::Inter);
        assert!(fs::read_to_string(&path).unwrap().contains("# retain me"));

        let invalid = parse(
            "version = 1\n[appearance]\ndensity = 'roomy'",
            FontOverrides::default(),
        );
        assert_eq!(invalid.density, WorkspaceDensity::Comfy);
        assert!(
            invalid
                .diagnostics
                .iter()
                .any(|message| message.contains("appearance.density"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn layout_presets_parse_round_trip_and_diagnose_invalid_entries() {
        let loaded = parse(
            "version = 1\n[layout_presets.dev]\nsplit = 'right'\nratio = 0.6\nfirst = 'pane'\nsecond = { split = 'down', ratio = 0.5, first = 'pane', second = 'pane' }\n[layout_presets.lone]\nsplit = 'right'\nratio = 0.5\nfirst = 'pane'\nsecond = 'pane'\nextra = 1\n[layout_presets.wide]\nsplit = 'down'\nratio = 1\nfirst = 'pane'\nsecond = 'pane'",
            FontOverrides::default(),
        );
        assert_eq!(loaded.layout_presets.len(), 1);
        assert_eq!(loaded.layout_presets["dev"].slots(), 3);
        for field in ["layout_presets.lone", "layout_presets.wide"] {
            assert!(
                loaded
                    .diagnostics
                    .iter()
                    .any(|message| message.contains(field)),
                "{field} should be diagnosed"
            );
        }

        let root = std::env::temp_dir().join(format!(
            "compi-preset-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        fs::write(
            &path,
            "version = 1\n# keep me\n[layout]\nsidebar_width = 300\n",
        )
        .unwrap();
        let mut config = load(Some(&path), FontOverrides::default());
        let shape = Shape::Split {
            axis: SplitAxis::Horizontal,
            ratio: 2.0 / 3.0,
            first: Box::new(Shape::Slot),
            second: Box::new(loaded.layout_presets["dev"].clone()),
        };
        config.save_layout_preset("My layout", &shape).unwrap();
        let reloaded = load(Some(&path), FontOverrides::default());
        assert!(
            reloaded.diagnostics.is_empty(),
            "{:?}",
            reloaded.diagnostics
        );
        assert_eq!(reloaded.sidebar_width, 300.0);
        assert_eq!(reloaded.layout_presets, config.layout_presets);
        assert_eq!(reloaded.layout_presets["My layout"].slots(), 4);
        assert!(fs::read_to_string(&path).unwrap().contains("# keep me"));

        assert!(config.save_layout_preset(" padded", &shape).is_err());
        assert!(config.save_layout_preset("single", &Shape::Slot).is_err());
        config.remove_layout_preset("My layout").unwrap();
        assert!(
            load(Some(&path), FontOverrides::default())
                .layout_presets
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn favorites_keep_valid_entries_in_order_when_siblings_are_invalid() {
        let loaded = parse(
            "version = 1\n[appearance]\nfavorites = ['nord', 'Invalid!', 'catppuccin-latte', 7, 'nord']",
            FontOverrides::default(),
        );
        assert_eq!(
            loaded.theme_favorites,
            [
                ThemeId::parse("nord").unwrap(),
                ThemeId::parse("catppuccin-latte").unwrap()
            ]
        );
        assert_eq!(loaded.diagnostics.len(), 2);
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|message| message.contains("appearance.favorites[1]"))
        );
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|message| message.contains("appearance.favorites[3]"))
        );
    }

    #[test]
    fn explicit_path_and_environment_override_native_paths() {
        let env = |key: &str| match key {
            "COMPI_CONFIG_FILE" => Some(OsString::from("env.toml")),
            "HOME" | "LOCALAPPDATA" | "XDG_CONFIG_HOME" => Some(OsString::from("native")),
            _ => None,
        };
        for platform in ["macos", "windows", "linux"] {
            assert_eq!(
                config_path(Some(Path::new("explicit.toml")), platform, env).unwrap(),
                PathBuf::from("explicit.toml")
            );
            assert_eq!(
                config_path(None, platform, env).unwrap(),
                PathBuf::from("env.toml")
            );
        }
        let xdg = |key: &str| match key {
            "HOME" => Some(OsString::from("home")),
            "XDG_CONFIG_HOME" => Some(OsString::from("xdg")),
            _ => None,
        };
        assert_eq!(
            config_path(None, "linux", xdg).unwrap(),
            PathBuf::from("xdg/compi/config.toml")
        );
    }

    #[test]
    fn concurrent_update_and_appearance_writes_preserve_both_changes() {
        let root = std::env::temp_dir().join(format!(
            "compi-update-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        fs::write(&path, "version = 1\n# keep me\n[future]\nanswer = 42\n").unwrap();
        let mut updates = load(Some(&path), FontOverrides::default());
        let mut appearance = updates.clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            let first = barrier.clone();
            scope.spawn(move || {
                first.wait();
                updates
                    .save_update_settings(UpdateSettings {
                        check_for_updates: false,
                        download_updates_automatically: true,
                        last_check_unix: Some(1234),
                    })
                    .unwrap();
            });
            scope.spawn(move || {
                barrier.wait();
                appearance.save_ui_font(UiFontPreset::Inter).unwrap();
            });
        });
        let loaded = load(Some(&path), FontOverrides::default());
        assert!(!loaded.updates.check_for_updates);
        assert!(loaded.updates.download_updates_automatically);
        assert_eq!(loaded.updates.last_check_unix, Some(1234));
        assert_eq!(loaded.ui_font, UiFontPreset::Inter);
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# keep me"));
        assert_eq!(
            saved.parse::<toml::Table>().unwrap()["future"]["answer"].as_integer(),
            Some(42)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_settings_migrate_legacy_checks_and_default_when_missing() {
        let defaults = parse("version = 1\n", FontOverrides::default());
        assert_eq!(defaults.updates, UpdateSettings::default());
        assert!(defaults.updates.check_for_updates);
        assert!(!defaults.updates.download_updates_automatically);
        for (legacy, expected) in [("never", false), ("on_launch", true), ("daily", true)] {
            let loaded = parse(
                &format!("version = 1\n[updates]\nautomatic_checks = '{legacy}'\n"),
                FontOverrides::default(),
            );
            assert_eq!(loaded.updates.check_for_updates, expected, "{legacy}");
            assert!(!loaded.updates.download_updates_automatically);
            assert!(loaded.diagnostics.is_empty(), "{legacy}");
        }
        // The new key wins over a stale legacy key.
        let both = parse(
            "version = 1\n[updates]\nautomatic_checks = 'never'\ncheck_for_updates = true\n",
            FontOverrides::default(),
        );
        assert!(both.updates.check_for_updates);
    }

    #[test]
    fn saving_update_settings_replaces_legacy_key_and_keeps_its_comment() {
        let root = std::env::temp_dir().join(format!(
            "compi-update-migrate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        fs::write(
            &path,
            "version = 1\n[updates]\n# how often\nautomatic_checks = \"never\"\n",
        )
        .unwrap();
        let mut loaded = load(Some(&path), FontOverrides::default());
        assert!(!loaded.updates.check_for_updates);
        loaded.save_update_settings(loaded.updates.clone()).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("automatic_checks"), "{saved}");
        assert!(
            saved.contains("# how often\ncheck_for_updates = false"),
            "{saved}"
        );
        let reloaded = load(Some(&path), FontOverrides::default());
        assert!(!reloaded.updates.check_for_updates);
        assert!(!reloaded.updates.download_updates_automatically);
        fs::remove_dir_all(root).unwrap();
    }
}
