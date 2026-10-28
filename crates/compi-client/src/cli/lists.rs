//! What workspace, tab, pane and window lists and details look like at a terminal.
//! The same commands print JSON when piped or under `--json`; see `human`.

use super::human::{self, Fit};
use crate::arrangement;
use crate::cli_window::{Action, Response};
use compi_protocol::{WorkspaceSession, WorkspaceSnapshot, WorkspaceTab};
use serde_json::Value;

/// A tab as lists name it: its position in its workspace, then its name if it has one.
pub(super) fn tab_name(session: &WorkspaceSession, tab: &WorkspaceTab) -> String {
    let position = session
        .tabs
        .iter()
        .position(|candidate| candidate.id == tab.id)
        .map_or(0, |index| index + 1);
    if tab.label.is_empty() {
        format!("#{position}")
    } else {
        format!("#{position} {}", tab.label)
    }
}

pub(super) fn workspaces(state: &WorkspaceSnapshot) -> String {
    let rows: Vec<Vec<String>> = state
        .sessions
        .iter()
        .map(|session| {
            vec![
                session.label.clone(),
                session.tabs.len().to_string(),
                session.id.to_string(),
            ]
        })
        .collect();
    human::table(
        &[
            ("WORKSPACE", Fit::End),
            ("TABS", Fit::Keep),
            ("ID", Fit::Keep),
        ],
        &rows,
        human::width(),
    )
}

pub(super) fn workspace(session: &WorkspaceSession) -> String {
    format!(
        "{}\n{}",
        human::fields(&[
            ("Workspace", session.label.clone()),
            ("ID", session.id.to_string()),
        ]),
        tabs([session].into_iter())
    )
}

pub(super) fn tabs<'a>(sessions: impl Iterator<Item = &'a WorkspaceSession>) -> String {
    let sessions: Vec<&WorkspaceSession> = sessions.collect();
    let several = sessions.len() > 1;
    let rows: Vec<Vec<String>> = sessions
        .iter()
        .flat_map(|session| {
            session.tabs.iter().map(move |tab| {
                let mut row = vec![
                    tab_name(session, tab),
                    arrangement::leaves(&tab.layout).len().to_string(),
                    tab.id.to_string(),
                ];
                if several {
                    row.insert(0, session.label.clone());
                }
                row
            })
        })
        .collect();
    let mut columns = vec![("TAB", Fit::End), ("PANES", Fit::Keep), ("ID", Fit::Keep)];
    if several {
        columns.insert(0, ("WORKSPACE", Fit::End));
    }
    human::table(&columns, &rows, human::width())
}

/// One `pane list` row: the pane's tab, what it is, what runs in it, where it is and
/// its ID, from the pane's JSON entry.
pub(super) fn pane_cells(tab: String, row: &Value) -> Vec<String> {
    vec![
        tab,
        pane_type(row).to_owned(),
        pane_state(row),
        pane_where(row),
        text(&row["pane_id"]),
    ]
}

pub(super) fn panes(rows: &[Vec<String>]) -> String {
    human::table(
        &[
            ("TAB", Fit::End),
            ("TYPE", Fit::Keep),
            ("STATE", Fit::End),
            ("WHERE", Fit::Start),
            ("ID", Fit::Keep),
        ],
        rows,
        human::width(),
    )
}

/// `pane show`: a pane's details as `name: value` lines.
pub(super) fn pane(workspace: &str, tab: String, row: &Value) -> String {
    let metadata = &row["metadata"];
    let surface = &row["surface"];
    let size = match (surface["cols"].as_i64(), surface["rows"].as_i64()) {
        (Some(cols), Some(rows)) => format!("{cols} × {rows}"),
        _ => String::new(),
    };
    let git = &metadata["git"]["value"];
    let branch = match (git["branch"].as_str(), git["changed"].as_bool()) {
        (Some(branch), Some(true)) => format!("{branch} (changed)"),
        (Some(branch), _) => branch.to_owned(),
        _ => String::new(),
    };
    human::fields(&[
        ("Pane", text(&row["pane_id"])),
        ("Type", pane_type(row).to_owned()),
        ("Workspace", workspace.to_owned()),
        ("Tab", tab),
        ("State", pane_state(row)),
        ("Where", pane_where(row)),
        ("Title", text(&metadata["title"])),
        ("Shell", text(&metadata["shell_executable"])),
        ("Git", branch),
        ("Size", size),
        ("Surface", text(&surface["id"])),
    ])
}

/// `window list`: each pane each window shows.
pub(super) fn windows(response: &Response) -> String {
    if response.windows.is_empty() {
        return "No Compi windows are open.\n".to_owned();
    }
    let rows: Vec<Vec<String>> = response
        .windows
        .iter()
        .flat_map(|window| {
            // A window showing nothing (an empty or ended tab) still gets a row.
            let empty = window.panes.is_empty().then(|| {
                vec![
                    window.window_id.clone(),
                    String::new(),
                    "no panes shown".to_owned(),
                    String::new(),
                ]
            });
            window
                .panes
                .iter()
                .map(|pane| {
                    let mut state = Vec::new();
                    for (on, name) in [
                        (pane.focused, "focused"),
                        (pane.floating, "floating"),
                        (pane.zoomed, "zoomed"),
                        (!pane.unsaved_files.is_empty(), "unsaved"),
                    ] {
                        if on {
                            state.push(name.to_owned());
                        }
                    }
                    if let Some(page) = &pane.page {
                        state.push(page.loading.clone().unwrap_or_else(|| page.url.clone()));
                    }
                    let pane_type = serde_json::to_value(pane.pane_type).unwrap_or_default();
                    vec![
                        window.window_id.clone(),
                        content_type_name(pane_type.as_str().unwrap_or_default()).to_owned(),
                        state.join(", "),
                        pane.pane_id.to_string(),
                    ]
                })
                .chain(empty)
        })
        .collect();
    human::table(
        &[
            ("WINDOW", Fit::Keep),
            ("TYPE", Fit::Keep),
            ("STATE", Fit::Start),
            ("PANE", Fit::Keep),
        ],
        &rows,
        human::width(),
    )
}

/// What a window did, in one sentence.
pub(super) fn applied(response: &Response, action: &Action) -> String {
    let window = response
        .applied
        .as_ref()
        .map_or("its window", |applied| applied.window_id.as_str());
    let sentence = match action {
        Action::FocusTab { tab_id } => format!("Focused tab {tab_id} in window {window}"),
        Action::DetachTab { tab_id } => {
            let destination = response
                .applied
                .as_ref()
                .and_then(|applied| applied.destination_window_id.as_deref())
                .unwrap_or("a new window");
            format!("Moved tab {tab_id} to window {destination}")
        }
        Action::FocusPane { pane_id } => format!("Focused pane {pane_id} in window {window}"),
        Action::ResizePane {
            pane_id,
            cols,
            rows,
        } => format!("Resized pane {pane_id} to {cols} × {rows}"),
        Action::ZoomPane { pane_id } => format!("Zoomed pane {pane_id}"),
        Action::UnzoomPane { pane_id } => format!("Unzoomed pane {pane_id}"),
        Action::FloatPane { pane_id } => format!("Floated pane {pane_id}"),
        Action::DockPane { pane_id } => format!("Docked pane {pane_id}"),
        _ => "Done".to_owned(),
    };
    format!("{sentence}.\n")
}

fn pane_type(row: &Value) -> &'static str {
    content_type_name(row["type"].as_str().unwrap_or_default())
}

/// A pane type as people call it: a content pane showing files is `files`.
fn content_type_name(pane_type: &str) -> &'static str {
    match pane_type {
        "terminal" => "terminal",
        "content" => "files",
        "settings" => "settings",
        "monitor" => "monitor",
        "browser" => "browser",
        _ => "pane",
    }
}

/// What runs in a terminal: its foreground program while it runs, else how it ended.
fn pane_state(row: &Value) -> String {
    let surface = &row["surface"];
    match surface["status"].as_str() {
        Some("running") => row["metadata"]["process"]["value"]["name"]
            .as_str()
            .unwrap_or("running")
            .to_owned(),
        Some("exited") => match surface["exit_code"].as_i64() {
            Some(code) => format!("exited {code}"),
            None => "exited".to_owned(),
        },
        Some(status) => status.to_owned(),
        None => String::new(),
    }
}

/// Where a pane is: a terminal's directory, a Files pane's open file or folder,
/// Settings' section, Monitor's page and source, a Browser pane's address.
fn pane_where(row: &Value) -> String {
    if row["type"] == "terminal" {
        let directory = &row["metadata"]["directory"]["value"];
        return directory
            .as_str()
            .or_else(|| row["surface"]["working_directory"]["requested"].as_str())
            .unwrap_or_default()
            .to_owned();
    }
    let content = &row["content"];
    match content["kind"].as_str() {
        Some("files") => content["active"]["path"]
            .as_str()
            .or_else(|| content["root"].as_str())
            .unwrap_or_default()
            .to_owned(),
        Some("settings") => text(&content["section"]),
        Some("monitor") => match content["source"]["distribution"].as_str() {
            Some(distribution) => format!("{} · wsl:{distribution}", text(&content["page"])),
            None => text(&content["page"]),
        },
        Some("browser") => text(&content["url"]),
        _ => String::new(),
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pane_rows_say_what_runs_where() {
        let running = json!({
            "pane_id": "pane-1", "type": "terminal",
            "surface": {"status": "running", "working_directory": {"requested": "/start"}},
            "metadata": {"process": {"value": {"name": "python3"}}, "directory": {"value": "/home/me/api"}}
        });
        assert_eq!(
            pane_cells("#2 api".into(), &running),
            ["#2 api", "terminal", "python3", "/home/me/api", "pane-1"]
        );
        // No metadata yet: the shell is running where it was started.
        let starting = json!({
            "pane_id": "pane-2", "type": "terminal",
            "surface": {"status": "running", "working_directory": {"requested": "/start"}},
            "metadata": {"process": {"value": null}, "directory": {"value": null}}
        });
        assert_eq!(
            pane_cells("#1".into(), &starting)[2..4],
            ["running", "/start"]
        );
        let exited = json!({"pane_id": "p", "type": "terminal", "surface": {"status": "exited", "exit_code": 2}});
        assert_eq!(pane_state(&exited), "exited 2");
        let files = json!({"pane_id": "p", "type": "content", "content": {"kind": "files", "root": "/r", "active": {"type": "file", "path": "/r/a.rs"}}});
        assert_eq!(
            pane_cells("#1".into(), &files)[1..4],
            ["files", "", "/r/a.rs"]
        );
        let browser = json!({"pane_id": "p", "type": "browser", "content": {"kind": "browser", "url": "http://localhost:5173/"}});
        assert_eq!(pane_where(&browser), "http://localhost:5173/");
        let monitor = json!({"pane_id": "p", "type": "monitor", "content": {"kind": "monitor", "page": "processes", "source": {"type": "wsl", "distribution": "Ubuntu"}}});
        assert_eq!(pane_where(&monitor), "processes · wsl:Ubuntu");
    }
}
