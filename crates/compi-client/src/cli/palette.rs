//! `compi command list` and `compi command run ID`: the palette's commands, listed from
//! the registry with no window needed, and run in the window that owns the selected pane
//! as the palette would run them there.
use super::human::{self, Fit};
use super::{CliError, Options, Output, targets};
use crate::cli_window::{self, Action, WindowInfo};
use crate::commands::{self, CommandSpec, Platform, REGISTRY};
use crate::connection::ConnectionTarget;
use compi_protocol::{TabId, WorkspaceSnapshot};
use serde_json::{Value, json};
use std::collections::HashMap;

/// Every palette command with this platform's shortcut, `overrides` (the configured
/// keybindings) applied.
pub(super) fn list(overrides: &HashMap<String, String>) -> Output {
    let platform = Platform::current();
    let commands: Vec<Value> = REGISTRY
        .iter()
        .map(|spec| entry(spec, platform, overrides))
        .collect();
    let rows: Vec<Vec<String>> = commands
        .iter()
        .map(|command| {
            ["id", "name", "shortcut"]
                .map(|key| command[key].as_str().unwrap_or_default().to_owned())
                .to_vec()
        })
        .collect();
    Output::Report {
        text: human::table(
            &[
                ("ID", Fit::Keep),
                ("NAME", Fit::End),
                ("SHORTCUT", Fit::Keep),
            ],
            &rows,
            human::width(),
        ),
        value: json!({"platform": platform_name(platform), "commands": commands}),
    }
}

/// One command as `command list --json` reports it. `shortcut` is the binding in effect
/// (`null` when unbound); `default_shortcut` the platform's own.
fn entry(
    spec: &'static CommandSpec,
    platform: Platform,
    overrides: &HashMap<String, String>,
) -> Value {
    json!({
        "id": spec.id,
        "name": spec.label,
        "category": spec.category().label(),
        "shortcut": spec.configured_shortcut(platform, overrides),
        "default_shortcut": spec.shortcut(platform),
        "description": spec.command.description(),
    })
}

fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "mac",
        Platform::Windows => "windows",
        Platform::Linux => "linux",
    }
}

/// Runs the command `validate` accepted in the window that shows the selected pane's
/// tab (or `--window`), with that pane focused first.
pub(super) fn run(
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let spec = commands::by_id(&options.args[2]).expect("validate accepts only registry ids");
    let (_, tab, leaf) = targets::pane(state, options)?;
    let pane_id = leaf
        .leaf_pane()
        .expect("targets resolve only leaf nodes")
        .clone();
    let window = match &options.window {
        Some(window) => Some(window.clone()),
        // Without a GUI host, `execute` reports that no window runs.
        None => match cli_window::windows(target)? {
            Some(windows) => Some(owner(&windows, &tab.id).ok_or_else(|| {
                CliError::new(
                    1,
                    "tab_not_shown",
                    format!(
                        "no Compi window shows tab {}; show it in a window first or select one with --window",
                        tab.id
                    ),
                )
            })?),
            None => None,
        },
    };
    let response = cli_window::execute(
        target,
        window.as_deref(),
        Action::RunCommand {
            pane_id: pane_id.clone(),
            command: spec.id.to_owned(),
        },
    )?;
    if let Some(reason) = response.disabled_reason {
        return Err(CliError::new(
            1,
            "command_disabled",
            format!("{} is disabled: {reason}", spec.label),
        ));
    }
    let window_id = response.applied.map(|applied| applied.window_id);
    Ok(Output::Report {
        text: format!("Ran {} in pane {pane_id}\n", spec.label),
        value: json!({"command": spec.id, "name": spec.label, "pane_id": pane_id, "window_id": window_id}),
    })
}

/// The window showing `tab`.
fn owner(windows: &[WindowInfo], tab: &TabId) -> Option<String> {
    windows
        .iter()
        .find(|window| window.visible_tabs.contains(tab))
        .map(|window| window.window_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_window::TargetIdentity;
    use std::collections::HashSet;

    #[test]
    fn registry_ids_are_unique_and_every_command_has_a_name_and_category() {
        let mut ids = HashSet::new();
        for spec in REGISTRY {
            assert!(ids.insert(spec.id), "duplicate id {}", spec.id);
            assert!(!spec.label.is_empty(), "{}", spec.id);
            assert!(commands::by_id(spec.id).is_some());
        }
    }

    #[test]
    fn list_reports_each_command_with_its_platform_shortcut_and_overrides() {
        let overrides = HashMap::from([
            ("open_settings".to_owned(), "ctrl-alt-s".to_owned()),
            ("quit".to_owned(), String::new()),
        ]);
        let Output::Report { text, value } = list(&overrides) else {
            panic!("command list reports text and JSON");
        };
        let platform = Platform::current();
        assert_eq!(value["platform"], platform_name(platform));
        let commands = value["commands"].as_array().unwrap();
        assert_eq!(commands.len(), REGISTRY.len());
        for (command, spec) in commands.iter().zip(REGISTRY) {
            let mut keys: Vec<&str> = command
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                [
                    "category",
                    "default_shortcut",
                    "description",
                    "id",
                    "name",
                    "shortcut"
                ]
            );
            assert_eq!(command["id"], spec.id);
            assert_eq!(command["name"], spec.label);
            assert_eq!(command["category"], spec.category().label());
            assert_eq!(command["default_shortcut"], json!(spec.shortcut(platform)));
        }
        let find = |id: &str| commands.iter().find(|command| command["id"] == id).unwrap();
        let palette = find("open_palette");
        let expected = if platform == Platform::Mac {
            "cmd-shift-p"
        } else {
            "ctrl-shift-p"
        };
        assert_eq!(palette["shortcut"], expected);
        assert_eq!(palette["description"], Value::Null);
        assert_eq!(find("open_settings")["shortcut"], "ctrl-alt-s");
        // An empty override unbinds the command.
        assert_eq!(find("quit")["shortcut"], Value::Null);
        assert!(find("quit")["default_shortcut"].is_string());
        assert_eq!(
            find("remove_pane")["description"],
            "Stops this terminal and removes its pane."
        );
        assert_eq!(
            find("remove_pane")["shortcut"],
            json!(if platform == Platform::Mac {
                None
            } else {
                Some("ctrl-shift-w")
            })
        );

        let mut lines = text.lines();
        assert_eq!(lines.next().unwrap().split_whitespace().next(), Some("ID"));
        assert_eq!(lines.count(), REGISTRY.len());
        assert!(
            text.lines()
                .any(|line| line.starts_with("open_settings") && line.ends_with("ctrl-alt-s"))
        );
    }

    #[test]
    fn shortcuts_follow_the_platform() {
        let palette = commands::by_id("open_palette").unwrap();
        assert_eq!(palette.shortcut(Platform::Mac), Some("cmd-shift-p"));
        assert_eq!(palette.shortcut(Platform::Windows), Some("ctrl-shift-p"));
        assert_eq!(palette.shortcut(Platform::Linux), Some("ctrl-shift-p"));
        let overrides = HashMap::new();
        let settings = entry(
            commands::by_id("open_settings").unwrap(),
            Platform::Windows,
            &overrides,
        );
        assert_eq!(settings["shortcut"], "ctrl-,");
        let settings = entry(
            commands::by_id("open_settings").unwrap(),
            Platform::Mac,
            &overrides,
        );
        assert_eq!(settings["shortcut"], "cmd-,");
    }

    #[test]
    fn commands_run_in_the_window_showing_the_tab() {
        let window = |id: &str, tabs: &[&str]| WindowInfo {
            window_id: id.to_owned(),
            target: TargetIdentity {
                instance: None,
                connect: None,
            },
            server_id: None,
            selected_tab: None,
            visible_tabs: tabs.iter().map(|tab| TabId::new(*tab)).collect(),
            panes: Vec::new(),
            busy: false,
        };
        let windows = [window("primary", &["a", "b"]), window("aux", &["c"])];
        assert_eq!(owner(&windows, &TabId::new("c")).as_deref(), Some("aux"));
        assert_eq!(
            owner(&windows, &TabId::new("a")).as_deref(),
            Some("primary")
        );
        assert_eq!(owner(&windows, &TabId::new("hidden")), None);
    }

    #[test]
    fn window_requests_name_only_registry_commands() {
        let request = |command: &str| cli_window::Request {
            target: TargetIdentity {
                instance: None,
                connect: None,
            },
            window: None,
            action: Action::RunCommand {
                pane_id: compi_protocol::PaneId::new("pane"),
                command: command.to_owned(),
            },
        };
        assert!(request("open_settings").validate().is_ok());
        assert!(request("open_setings").validate().is_err());
    }
}
