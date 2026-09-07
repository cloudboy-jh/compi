#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, target_os = "macos", test))]
use compi_app::config::FontOverrides;
#[cfg(any(windows, target_os = "macos", test))]
use std::path::PathBuf;

#[cfg(any(windows, target_os = "macos", test))]
struct Arguments {
    instance: Option<String>,
    working_directory: Option<String>,
    config: Option<PathBuf>,
    font: FontOverrides,
    diagnostics: Vec<String>,
}

#[cfg(any(windows, target_os = "macos", test))]
fn parse_arguments(args: impl IntoIterator<Item = String>) -> Result<Arguments, String> {
    let mut args = args.into_iter();
    let mut instance = None;
    let mut working_directory = None;
    let mut config = None;
    let mut family = None;
    let mut size = None;
    let mut line_height = None;
    while let Some(argument) = args.next() {
        let setting = match argument.as_str() {
            "--instance" => &mut instance,
            "--working-directory" => &mut working_directory,
            "--config" => &mut config,
            "--font-family" => &mut family,
            "--font-size" => &mut size,
            "--line-height" => &mut line_height,
            _ if !argument.starts_with('-') && working_directory.is_none() => {
                working_directory = Some(argument);
                continue;
            }
            _ => return Err(format!("Unknown argument: {argument}")),
        };
        if setting.is_some() {
            return Err(format!("Repeated argument: {argument}"));
        }
        *setting = Some(
            args.next()
                .filter(|value| !value.is_empty() && !value.starts_with("--"))
                .ok_or_else(|| format!("{argument} requires a value"))?,
        );
    }
    let mut diagnostics = Vec::new();
    let mut numeric_override = |value: Option<String>, option: &str| {
        value.and_then(|value| match value.parse::<f32>() {
            Ok(number) => Some(number),
            Err(_) => {
                diagnostics.push(format!(
                    "Invalid {option}: expected a number, got {value:?}; ignoring this override"
                ));
                None
            }
        })
    };
    let font = FontOverrides {
        family,
        size: numeric_override(size, "--font-size"),
        line_height: numeric_override(line_height, "--line-height"),
    };
    Ok(Arguments {
        instance,
        working_directory,
        config: config.map(PathBuf::from),
        font,
        diagnostics,
    })
}

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    let args = match parse_arguments(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!(
                "{error}\nUsage: compi [--instance NAME] [--working-directory PATH | PATH] [--config PATH] [--font-family FAMILY] [--font-size SIZE] [--line-height MULTIPLIER]"
            );
            std::process::exit(2);
        }
    };
    let mut config = compi_app::config::load(args.config.as_deref(), args.font);
    config.diagnostics.extend(args.diagnostics);
    compi_app::gui::run(args.instance, args.working_directory, config);
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!(
        "The Compi native application runs on Windows and macOS; use compi-probe for headless Unix sessions"
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_numeric_override_preserves_other_launch_arguments() {
        let args = parse_arguments(
            [
                "--instance",
                "work",
                "--config",
                "chosen.toml",
                "--font-size",
                "large",
                "--line-height",
                "1.5",
                "--font-family",
                "CLI Font",
                "/project with spaces",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(args.instance.as_deref(), Some("work"));
        assert_eq!(
            args.working_directory.as_deref(),
            Some("/project with spaces")
        );
        assert_eq!(args.config, Some(PathBuf::from("chosen.toml")));
        assert_eq!(args.font.family.as_deref(), Some("CLI Font"));
        assert_eq!(args.font.size, None);
        assert_eq!(args.font.line_height, Some(1.5));
        assert_eq!(args.diagnostics.len(), 1);
        assert!(args.diagnostics[0].contains("--font-size"));
    }
}
