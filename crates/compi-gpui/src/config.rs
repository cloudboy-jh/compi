//! Read-only, versioned user configuration. The currently supported schema is:
//! ```toml
//! version = 1
//! [font]
//! family = "Menlo" # Provisional macOS default; Windows uses "Cascadia Mono".
//! size = 14.0 # Logical pixels, inclusive range 6..=72.
//! line_height = 1.35 # Primary-font line-height multiplier, range 1..=3.
//! fallbacks = [] # Ordered requests; installed-font resolution owns automatic fallbacks.
//! ```
//! Unknown keys/tables are ignored so independently implemented settings can coexist.
//! CLI overrides are invocation-local; this module never creates or rewrites files.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
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

#[derive(Debug, Default)]
pub struct FontOverrides {
    pub family: Option<String>,
    pub size: Option<f32>,
    pub line_height: Option<f32>,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub font: FontSettings,
    pub diagnostics: Vec<String>,
    /// Selected file, or empty when the native configuration directory is unavailable.
    pub path: PathBuf,
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
    let mut loaded = LoadedConfig {
        font: FontSettings::default(),
        diagnostics: Vec::new(),
        path: PathBuf::new(),
    };
    match selected {
        Ok(path) => {
            loaded.path = path;
            match std::fs::read_to_string(&loaded.path) {
                Ok(source) => apply_source(&source, &mut loaded),
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound && !explicitly_selected => {}
                Err(error) => loaded.diagnostics.push(format!(
                    "Cannot read configuration {}: {error}; using font defaults",
                    loaded.path.display()
                )),
            }
        }
        Err(error) => loaded.diagnostics.push(error),
    }
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
        "Cannot locate the native configuration directory; set COMPI_CONFIG_FILE or --config; using font defaults".to_owned()
    })
}

fn apply_source(source: &str, loaded: &mut LoadedConfig) {
    let document = match source.parse::<toml::Table>() {
        Ok(document) => document,
        Err(error) => {
            loaded.diagnostics.push(format!(
                "Invalid TOML in {}: {error}; using font defaults",
                loaded.path.display()
            ));
            return;
        }
    };
    if document.get("version").and_then(toml::Value::as_integer) != Some(1) {
        loaded.diagnostics.push(format!(
            "Unsupported or missing configuration version in {}; expected version = 1; using font defaults",
            loaded.path.display()
        ));
        return;
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
            font: FontSettings::default(),
            diagnostics: Vec::new(),
            path: PathBuf::from("example.toml"),
        };
        apply_source(source, &mut loaded);
        apply_overrides(overrides, &mut loaded);
        loaded
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
