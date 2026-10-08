#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, target_os = "macos", test))]
use compi_client::config::FontOverrides;
#[cfg(any(windows, target_os = "macos", test))]
use std::path::PathBuf;

#[cfg(any(windows, target_os = "macos", test))]
// Update handoff fields are consumed only by the desktop entry point.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
struct Arguments {
    instance: Option<String>,
    connect: Option<String>,
    working_directory: Option<String>,
    config: Option<PathBuf>,
    font: FontOverrides,
    theme: Option<String>,
    sidebar_width: Option<f32>,
    diagnostics: Vec<String>,
    update_restore: Option<PathBuf>,
    update_receipt: Option<PathBuf>,
    update_receipt_token: Option<String>,
}

#[cfg(any(windows, target_os = "macos", test))]
fn parse_arguments(args: impl IntoIterator<Item = String>) -> Result<Arguments, String> {
    let mut args = args.into_iter();
    let mut instance = None;
    let mut connect = None;
    let mut working_directory = None;
    let mut config = None;
    let mut family = None;
    let mut size = None;
    let mut line_height = None;
    let mut theme = None;
    let mut sidebar_width = None;
    let mut update_restore = None;
    let mut update_receipt = None;
    let mut update_receipt_token = None;
    while let Some(argument) = args.next() {
        let setting = match argument.as_str() {
            "--instance" => &mut instance,
            "--connect" => &mut connect,
            "--working-directory" => &mut working_directory,
            "--config" => &mut config,
            "--font-family" => &mut family,
            "--font-size" => &mut size,
            "--line-height" => &mut line_height,
            "--theme" => &mut theme,
            "--sidebar-width" => &mut sidebar_width,
            "--update-restore" => &mut update_restore,
            "--update-receipt" => &mut update_receipt,
            "--update-receipt-token" => &mut update_receipt_token,
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
    if update_restore.is_some()
        && (working_directory.is_some()
            || instance.is_some()
            || connect.is_some()
            || config.is_some()
            || family.is_some()
            || size.is_some()
            || line_height.is_some()
            || theme.is_some()
            || sidebar_width.is_some())
    {
        return Err(
            "--update-restore uses only the retained handoff; launch overrides are not allowed"
                .into(),
        );
    }
    if update_restore.is_some() != update_receipt.is_some()
        || update_receipt.is_some() != update_receipt_token.is_some()
    {
        return Err(
            "update restore requires both --update-receipt and --update-receipt-token".into(),
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
    let sidebar_width = numeric_override(sidebar_width, "--sidebar-width");
    Ok(Arguments {
        instance,
        connect,
        working_directory,
        config: config.map(PathBuf::from),
        font,
        theme,
        sidebar_width,
        diagnostics,
        update_restore: update_restore.map(PathBuf::from),
        update_receipt: update_receipt.map(PathBuf::from),
        update_receipt_token,
    })
}

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if let Some(exit) = compi_client::cli::dispatch(&raw) {
        std::process::exit(exit);
    }
    if let [mode, root, version] = raw.as_slice()
        && mode == "--stop-idle-update-daemons"
    {
        if let Err(error) =
            compi_client::updates::stop_idle_update_daemons(std::path::Path::new(root), version)
        {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    let args = match parse_arguments(raw) {
        Ok(args) => args,
        Err(error) => {
            eprintln!(
                "{error}\nUsage: compi [--instance NAME] [--connect [USER@]HOST[:PORT]] [--working-directory PATH | PATH] [--config PATH] [--font-family FAMILY] [--font-size SIZE] [--line-height MULTIPLIER] [--theme NAME] [--sidebar-width WIDTH]"
            );
            std::process::exit(2);
        }
    };
    if let Some(path) = &args.update_restore {
        let result = (|| -> compi_client::Result<()> {
            let session = compi_client::update_restore::RestoreSession::open(path)?;
            let first = &session.windows()[0];
            let host_instance = first.target.state_instance();
            let request = compi_client::window_host::LaunchRequest::restore(
                path.clone(),
                first.config.clone(),
            );
            match compi_client::window_host::acquire(host_instance.as_deref(), request)? {
                compi_client::window_host::HostAcquisition::Forwarded => {
                    return Err("update restore must never forward into an old host".into());
                }
                compi_client::window_host::HostAcquisition::Host(mut host, _) => {
                    compi_client::updates::set_readiness_receipt(
                        args.update_receipt.clone().unwrap(),
                        args.update_receipt_token.clone().unwrap(),
                    );
                    let receiver = host.take_receiver();
                    compi_client::gui::run_restore(session, Some(receiver));
                    drop(host);
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!(
                "Could not restore updated Compi: {error}. The handoff is retained for recovery."
            );
            std::process::exit(1);
        }
        return;
    }
    let target = match compi_client::connection::ConnectionTarget::from_options(
        args.instance.clone(),
        args.connect.clone(),
    ) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("Could not configure Compi connection: {error}");
            std::process::exit(2);
        }
    };
    let host_instance = target.state_instance();
    let mut config = compi_client::config::load(args.config.as_deref(), args.font);
    config.apply_presentation_overrides(args.theme.as_deref(), args.sidebar_width);
    config.diagnostics.extend(args.diagnostics);
    let request = compi_client::window_host::LaunchRequest::new(args.working_directory, config);
    match compi_client::window_host::acquire(host_instance.as_deref(), request) {
        Ok(compi_client::window_host::HostAcquisition::Forwarded) => {}
        Ok(compi_client::window_host::HostAcquisition::Host(mut host, request)) => {
            let receiver = host.take_receiver();
            compi_client::gui::run(
                target,
                request.initial_working_directory,
                request.config,
                Some(receiver),
            );
            drop(host);
        }
        Err(error) => {
            eprintln!("Could not open Compi window: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if let Some(exit) = compi_client::cli::dispatch(&raw) {
        std::process::exit(exit);
    }
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
                "--connect",
                "dev@example.com:2222",
                "--config",
                "chosen.toml",
                "--font-size",
                "large",
                "--line-height",
                "1.5",
                "--font-family",
                "CLI Font",
                "--theme",
                "warm-carbon",
                "--sidebar-width",
                "360",
                "/project with spaces",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(args.instance.as_deref(), Some("work"));
        assert_eq!(args.connect.as_deref(), Some("dev@example.com:2222"));
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

        let mut config = compi_client::config::LoadedConfig {
            appearance: compi_client::config::AppearanceSettings {
                theme: compi_client::theme::ThemeId::from(
                    compi_client::theme::ThemePreset::DarkGlass,
                ),
                ..Default::default()
            },
            configured_appearance: compi_client::config::AppearanceSettings {
                theme: compi_client::theme::ThemeId::from(
                    compi_client::theme::ThemePreset::DarkGlass,
                ),
                ..Default::default()
            },
            sidebar_width: 240.0,
            configured_sidebar_width: 240.0,
            ..Default::default()
        };
        config.apply_presentation_overrides(args.theme.as_deref(), args.sidebar_width);
        assert_eq!(config.appearance.theme.id(), "warm-carbon");
        assert_eq!(config.appearance.terminal_theme.id(), "warm-carbon");
        assert!(!config.appearance.terminal_theme_override);
        assert_eq!(config.sidebar_width, 360.0);
        assert_eq!(config.configured_appearance.theme.id(), "dark-glass");
        assert_eq!(
            config.configured_appearance.effective_terminal_theme().id(),
            "dark-glass"
        );
        assert_eq!(config.configured_sidebar_width, 240.0);
    }

    #[test]
    fn update_restore_rejects_startup_replay_and_missing_receipt() {
        let restore = [
            "--update-restore",
            "handoff.json",
            "--update-receipt",
            "receipt.json",
            "--update-receipt-token",
            "private-token",
        ];
        for overrides in [
            vec!["--working-directory", "/project"],
            vec!["--instance", "other"],
            vec!["--connect", "other-host"],
            vec!["--config", "other.toml"],
            vec!["--theme", "dracula"],
        ] {
            assert!(
                parse_arguments(restore.into_iter().chain(overrides).map(str::to_owned)).is_err()
            );
        }
        assert!(parse_arguments(["--update-restore", "handoff.json"].map(str::to_owned)).is_err());
        assert!(
            parse_arguments(
                [
                    "--update-receipt",
                    "receipt.json",
                    "--update-receipt-token",
                    "private-token"
                ]
                .map(str::to_owned)
            )
            .is_err()
        );
    }
}
