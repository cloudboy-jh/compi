//! `compi settings [SECTION]` and `compi monitor [PAGE]`: Settings or Monitor in the pane
//! they run from. A terminal shows it as its view, in place of its shell, which keeps
//! running underneath; a Settings or Monitor content pane changes its own section or page.
use super::content::{self, Placed};
use super::{CliError, Options, Output, metrics, submit, targets};
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::system_metrics::MonitorSource;
use compi_protocol::{
    LayoutNode, MonitorContent, MonitorPage, PaneContent, PaneSide, SettingsContent,
    SettingsSection, WorkspaceMutation, WorkspaceSnapshot,
};
use serde_json::json;

/// Monitor's pages as the command names them.
const PAGES: [(&str, MonitorPage); 7] = [
    ("overview", MonitorPage::Overview),
    ("processes", MonitorPage::Processes),
    ("cpu", MonitorPage::Cpu),
    ("memory", MonitorPage::Memory),
    ("gpu", MonitorPage::Gpu),
    ("disk", MonitorPage::Disk),
    ("network", MonitorPage::Network),
];

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let command = options.args[0].as_str();
    let (_, _, leaf) = targets::pane(state, options)?;
    let pane_id = leaf
        .leaf_pane()
        .expect("targets resolve only leaf nodes")
        .clone();
    let current = leaf.content();
    let source = options
        .values
        .get("--source")
        .map(|name| metrics::source(name))
        .transpose()?;
    let content = wanted(
        command,
        options.args.get(1).map(String::as_str),
        source.clone(),
        current,
    )?;
    if let Some(source) = &source {
        metrics::known(client, source)?;
    }
    let mutation = match &leaf {
        LayoutNode::Pane { view, .. } => {
            let shown = view
                .as_ref()
                .is_some_and(|view| view.side == PaneSide::View && view.content == content);
            (!shown).then(|| WorkspaceMutation::ShowView {
                pane_id: pane_id.clone(),
                content: content.clone(),
            })
        }
        LayoutNode::Content { content: shown, .. } if shown.same_kind(&content) => {
            (*shown != content).then(|| WorkspaceMutation::UpdateContent {
                pane_id: pane_id.clone(),
                content: content.clone(),
            })
        }
        LayoutNode::Content { .. } => {
            return Err(CliError::new(
                3,
                "terminal_pane_required",
                format!(
                    "pane {pane_id} is a content pane; {command} needs a terminal pane or a {command} pane"
                ),
            ));
        }
        LayoutNode::Split { .. } => unreachable!("targets resolve only leaf nodes"),
    };
    let receipt = mutation
        .map(|mutation| submit(client, state, mutation, None))
        .transpose()?;
    let placed = Placed {
        pane_id: pane_id.clone(),
        receipt,
    };
    let (subject, detail) = match &content {
        PaneContent::Settings(settings) => (
            format!("Settings · {}", settings.section.label()),
            json!({"section": section_name(settings.section)}),
        ),
        PaneContent::Monitor(monitor) => {
            let page = match monitor.page {
                MonitorPage::Overview => "Overview",
                MonitorPage::Processes => "Processes",
                MonitorPage::Cpu => "CPU",
                MonitorPage::Memory => "Memory",
                MonitorPage::Gpu => "GPU",
                MonitorPage::Disk => "Disk",
                MonitorPage::Network => "Network",
            };
            (
                match &monitor.source {
                    MonitorSource::Host => format!("Monitor · {page}"),
                    MonitorSource::Wsl { distribution } => {
                        format!("Monitor · {page} · WSL: {distribution}")
                    }
                },
                json!({
                    "page": page_name(monitor.page),
                    "source": metrics::source_name(&monitor.source),
                }),
            )
        }
        PaneContent::Files(_) => unreachable!("only Settings and Monitor are asked for"),
    };
    let (text, mut value) = content::report(
        client,
        target,
        options,
        &placed,
        &subject,
        Action::FocusPane { pane_id },
    )?;
    for (key, field) in detail.as_object().into_iter().flatten() {
        value[key] = field.clone();
    }
    Ok(Output::Report { text, value })
}

/// What `command` shows: the named section or page, else the one the pane already shows,
/// else the first. Monitor's `source` follows the same rule, else the host.
fn wanted(
    command: &str,
    name: Option<&str>,
    source: Option<MonitorSource>,
    current: Option<&PaneContent>,
) -> Result<PaneContent, CliError> {
    Ok(if command == "settings" {
        let section = match name {
            Some(name) => SettingsSection::ALL
                .into_iter()
                .find(|section| section_name(*section) == name.to_ascii_lowercase())
                .ok_or_else(|| {
                    CliError::usage(format!(
                        "unknown Settings section {name}; use one of {}",
                        SettingsSection::ALL.map(section_name).join(", ")
                    ))
                })?,
            None => current
                .and_then(PaneContent::settings)
                .map(|settings| settings.section)
                .unwrap_or_default(),
        };
        PaneContent::Settings(SettingsContent { section })
    } else {
        let shown = match current {
            Some(PaneContent::Monitor(monitor)) => Some(monitor),
            _ => None,
        };
        let page = match name {
            Some(name) => PAGES
                .iter()
                .find(|(page, _)| *page == name.to_ascii_lowercase())
                .map(|(_, page)| *page)
                .ok_or_else(|| {
                    CliError::usage(format!(
                        "unknown Monitor page {name}; use one of {}",
                        PAGES.map(|(name, _)| name).join(", ")
                    ))
                })?,
            None => shown.map(|monitor| monitor.page).unwrap_or_default(),
        };
        let source = source
            .or_else(|| shown.map(|monitor| monitor.source.clone()))
            .unwrap_or_default();
        PaneContent::Monitor(MonitorContent { page, source })
    })
}

/// A section as the command names it: its label in lower case.
fn section_name(section: SettingsSection) -> String {
    section.label().to_ascii_lowercase()
}

fn page_name(page: MonitorPage) -> &'static str {
    PAGES
        .iter()
        .find(|(_, known)| *known == page)
        .map_or("overview", |(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_pick_a_section_or_page_and_omitted_ones_keep_what_shows() {
        let monitor = |page| {
            PaneContent::Monitor(MonitorContent {
                page,
                source: MonitorSource::Host,
            })
        };
        let settings = |section| PaneContent::Settings(SettingsContent { section });
        assert_eq!(
            wanted("settings", Some("Monitor"), None, None).unwrap(),
            settings(SettingsSection::Monitor)
        );
        assert_eq!(
            wanted(
                "settings",
                None,
                None,
                Some(&settings(SettingsSection::Keyboard))
            )
            .unwrap(),
            settings(SettingsSection::Keyboard)
        );
        // Another kind of view says nothing about the section.
        assert_eq!(
            wanted("settings", None, None, Some(&monitor(MonitorPage::Disk))).unwrap(),
            settings(SettingsSection::default())
        );
        assert_eq!(
            wanted("monitor", Some("cpu"), None, None).unwrap(),
            monitor(MonitorPage::Cpu)
        );
        assert_eq!(
            wanted("monitor", None, None, Some(&monitor(MonitorPage::Network))).unwrap(),
            monitor(MonitorPage::Network)
        );
        assert_eq!(
            wanted("monitor", None, None, None).unwrap(),
            monitor(MonitorPage::Overview)
        );
        // The section saved as "performance" is named for what it shows.
        assert_eq!(
            wanted("settings", Some("performance"), None, None)
                .unwrap_err()
                .exit,
            2
        );
        assert_eq!(
            wanted("monitor", Some("gpu"), None, None).unwrap(),
            monitor(MonitorPage::Gpu)
        );
        assert_eq!(
            wanted("monitor", Some("fans"), None, None)
                .unwrap_err()
                .exit,
            2
        );
    }

    #[test]
    fn a_monitor_pane_keeps_its_source_unless_one_is_named() {
        let ubuntu = MonitorSource::Wsl {
            distribution: "Ubuntu".into(),
        };
        let shown = PaneContent::Monitor(MonitorContent {
            page: MonitorPage::Cpu,
            source: ubuntu.clone(),
        });
        // A page alone keeps the source; a source alone keeps the page.
        assert_eq!(
            wanted("monitor", Some("disk"), None, Some(&shown)).unwrap(),
            PaneContent::Monitor(MonitorContent {
                page: MonitorPage::Disk,
                source: ubuntu.clone(),
            })
        );
        assert_eq!(
            wanted("monitor", None, Some(MonitorSource::Host), Some(&shown)).unwrap(),
            PaneContent::Monitor(MonitorContent {
                page: MonitorPage::Cpu,
                source: MonitorSource::Host,
            })
        );
        // A terminal or Settings view has no source to keep: the host.
        let settings = PaneContent::Settings(SettingsContent::default());
        assert_eq!(
            wanted("monitor", None, None, Some(&settings)).unwrap(),
            PaneContent::Monitor(MonitorContent::default())
        );
        assert_eq!(
            wanted("monitor", Some("gpu"), Some(ubuntu.clone()), None).unwrap(),
            PaneContent::Monitor(MonitorContent {
                page: MonitorPage::Gpu,
                source: ubuntu,
            })
        );
    }
}
