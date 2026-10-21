//! Notebook observation uses the same file transport as the editor, without a window.
use super::{CliError, Options, Output, content};
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget, notes};
use compi_protocol::{ContentTab, WorkspaceSnapshot};
use serde_json::json;

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let action = options.args.get(1).map(String::as_str);
    if !matches!(action, Some("search" | "backlinks")) {
        return open(client, state, target, options);
    }
    let caller = content::caller(client, state, options)?;
    let root = match options.args.get(3) {
        Some(folder) => content::resolve_path(caller.directory.as_deref(), folder, caller.windows)?,
        None => caller.directory()?.to_owned(),
    };
    let argument = &options.args[2];
    if argument.trim().is_empty() {
        return Err(CliError::usage(
            "notes search/backlinks needs a nonempty query or file",
        ));
    }
    let notebook = notes::load(client, caller.environment.clone(), &root)?;
    let (hits, file) = if action == Some("backlinks") {
        let file = notebook.resolve("", argument).map_err(|error| {
            let message = if notebook.warnings.is_empty() {
                error
            } else {
                format!(
                    "{error}\nNotebook warnings: {}",
                    notebook.warnings.join("; ")
                )
            };
            CliError::new(1, "note_not_found_or_ambiguous", message)
        })?;
        (notebook.backlinks(&file), Some(file))
    } else {
        (notebook.search(argument), None)
    };
    let mut text = String::new();
    for hit in &hits {
        text.push_str(&format!("{}:{}: {}\n", hit.path, hit.line, hit.text));
    }
    if hits.is_empty() {
        text.push_str("No matches.\n");
    }
    for warning in &notebook.warnings {
        text.push_str(&format!("Warning: {warning}\n"));
    }
    Ok(Output::Report {
        text,
        value: json!({
            "root": notebook.root,
            "environment": caller.environment,
            "query": if file.is_none() { Some(argument) } else { None },
            "file": file,
            "hits": hits,
            "partial": !notebook.warnings.is_empty(),
            "warnings": notebook.warnings,
        }),
    })
}

fn open(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let Some(argument) = options.args.get(1) else {
        return content::run(client, state, target, options);
    };
    if argument.is_empty() || argument.contains('\0') {
        return Err(CliError::usage(
            "notes needs a nonempty file or folder without NUL",
        ));
    }
    let caller = content::caller(client, state, options)?;
    let path = content::resolve_path(caller.directory.as_deref(), argument, caller.windows)?;
    if client
        .list_content_directory(caller.environment.clone(), path.clone())
        .is_ok()
    {
        return content::run(client, state, target, options);
    }
    if !notes::is_markdown(&path) {
        return Err(CliError::usage(
            "notes opens Markdown files (.md or .markdown) or folders",
        ));
    }
    let root = match caller.directory.as_deref() {
        Some(directory) if content::contains(directory, &path, caller.windows) => {
            directory.to_owned()
        }
        _ => content::resolve_path(None, &format!("{path}/.."), caller.windows)?,
    };
    let show = content::Show::Tab {
        tab: ContentTab::edit(path.clone()),
        file: path.clone(),
        within: None,
        root,
    };
    let placed = content::place(client, state, &caller, &show)?;
    let rendered = !options.switches.contains("--edit");
    let (text, mut value) = content::report(
        client,
        target,
        options,
        &placed,
        &path,
        Action::RevealNote {
            pane_id: placed.pane_id.clone(),
            file: path.clone(),
            rendered,
        },
    )?;
    value["file"] = json!(path);
    value["rendered"] = json!(rendered);
    Ok(Output::Report { text, value })
}
