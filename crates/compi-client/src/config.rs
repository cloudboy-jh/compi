//! Read-only TOML schema version 1. Tables: `font`, `appearance`, `layout`,
//! `keybindings`, `shell`, `environment`, `profiles.<name>`, `limits`, `clipboard`.
//! `default_profile` selects a named profile over the base shell/environment.
//! Missing settings retain defaults; invalid independent settings are diagnosed.
//! CLI overrides are invocation-local; this module never creates or rewrites files.

use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use compi_protocol::LaunchContext;
use serde::{Deserialize, Serialize};

use crate::theme::ThemePreset;

pub const DEFAULT_SIDEBAR_WIDTH: f32 = 280.0;
pub const MIN_SIDEBAR_WIDTH: f32 = 200.0;
pub const MAX_SIDEBAR_WIDTH: f32 = 600.0;

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoadedConfig {
    pub font: FontSettings,
    /// Pre-CLI defaults used for durable presentation and tear-off inheritance.
    pub configured_font: FontSettings,
    pub theme: ThemePreset,
    pub configured_theme: ThemePreset,
    pub sidebar_width: f32,
    pub configured_sidebar_width: f32,
    pub keybindings: HashMap<String, String>,
    /// A bad explicit launch selection remains an error, not a default shell.
    pub launch: Result<LaunchContext, String>,
    pub clipboard_policy: ClipboardPolicy,
    pub provenance: ConfigProvenance,
    pub diagnostics: Vec<String>,
    /// Selected file, or empty when the native configuration directory is unavailable.
    pub path: PathBuf,
}

impl Default for LoadedConfig {
    fn default() -> Self {
        Self {
            font: FontSettings::default(),
            configured_font: FontSettings::default(),
            theme: ThemePreset::default(),
            configured_theme: ThemePreset::default(),
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            configured_sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            keybindings: HashMap::new(),
            launch: Ok(LaunchContext::default()),
            clipboard_policy: ClipboardPolicy::default(),
            provenance: ConfigProvenance::default(),
            diagnostics: Vec::new(),
            path: PathBuf::new(),
        }
    }
}

impl LoadedConfig {
    /// Invocation overrides never replace the configured defaults used to seed
    /// a new slot or reset existing client state.
    pub fn apply_presentation_overrides(
        &mut self,
        theme: Option<&str>,
        sidebar_width: Option<f32>,
    ) {
        if let Some(value) = theme {
            if let Some(theme) = ThemePreset::parse(value) {
                self.theme = theme;
                self.provenance.theme = ValueSource::CommandLine;
            } else {
                invalid(self, "--theme", "dark-glass or warm-carbon", "CLI");
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
        {
            return Err("Forwarded configuration has invalid sidebar width".to_owned());
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
        if let Ok(launch) = &self.launch {
            if launch.scrollback_lines > 100_000 || launch.graphics_bytes > 4 * 1024 * 1024 {
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
    if let Some(appearance) = table(document, "appearance", loaded)
        && let Some(value) = appearance.get("theme")
    {
        if let Some(theme) = value.as_str().and_then(ThemePreset::parse) {
            loaded.theme = theme;
            loaded.provenance.theme = ValueSource::Configuration;
        } else {
            invalid(
                loaded,
                "appearance.theme",
                "dark-glass or warm-carbon",
                "configuration",
            );
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
                4 * 1024 * 1024,
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
    loaded.configured_theme = loaded.theme;
    loaded.configured_sidebar_width = loaded.sidebar_width;
    apply_overrides(overrides, &mut loaded);
    loaded
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
    apply_launch(&document, loaded);
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
        loaded.configured_theme = loaded.theme;
        loaded.configured_sidebar_width = loaded.sidebar_width;
        apply_overrides(overrides, &mut loaded);
        loaded
    }

    #[test]
    fn invalid_executable_blocks_launch_without_discarding_presentation() {
        let loaded = parse(
            "version = 1\n[shell]\nexecutable = ''\n[appearance]\ntheme = 'warm-carbon'\n[font]\nsize = 18",
            FontOverrides::default(),
        );
        assert!(loaded.launch.is_err());
        assert_eq!(loaded.theme, ThemePreset::WarmCarbon);
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
        assert_eq!(loaded.theme, ThemePreset::DarkGlass);
        assert_eq!(loaded.configured_theme, ThemePreset::WarmCarbon);
        assert_eq!(loaded.sidebar_width, 400.0);
        assert_eq!(loaded.configured_sidebar_width, 320.0);
        assert_eq!(loaded.font.size, 24.0);
        assert_eq!(loaded.configured_font.size, 16.0);
        assert_eq!(loaded.provenance.font_size, ValueSource::CommandLine);
        loaded.apply_presentation_overrides(Some("unknown"), Some(f32::NAN));
        assert_eq!(loaded.theme, ThemePreset::DarkGlass);
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
}
