//! `compi chat`: a new chat with OMP in the terminal it runs from, in the shell's current
//! directory. The terminal shows it as its view, in place of its shell, which keeps
//! running underneath; Cmd/Ctrl+Shift+E or Esc returns to the shell.
use super::content::{self, Placed};
use super::{CliError, Options, Output, submit, targets};
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::{LayoutNode, PaneContent, WorkspaceMutation, WorkspaceSnapshot};
use serde_json::json;

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
) -> Result<Output, CliError> {
    let (_, _, leaf) = targets::pane(state, options)?;
    if let LayoutNode::Content { pane_id, content } = &leaf {
        return Err(CliError::new(
            3,
            "terminal_pane_required",
            format!(
                "pane {pane_id} shows {}; chat needs a terminal pane",
                content.label()
            ),
        ));
    }
    let caller = content::caller(client, state, options)?;
    let folder = caller.directory.clone().ok_or_else(|| {
        CliError::usage(
            "the shell has not reported its current directory; enable OSC 7 shell integration",
        )
    })?;
    let chat = crate::chat::new_chat(caller.environment.clone(), folder);
    chat.validate().map_err(CliError::usage)?;
    let receipt = submit(
        client,
        state,
        WorkspaceMutation::ShowView {
            pane_id: caller.id.clone(),
            content: PaneContent::Chat(chat.clone()),
        },
        None,
    )?;
    let placed = Placed {
        pane_id: caller.id.clone(),
        receipt: Some(receipt),
    };
    let (text, mut value) = content::report(
        client,
        target,
        options,
        &placed,
        &format!("{} · {}", chat.harness.name(), chat.folder),
        Action::FocusPane { pane_id: caller.id },
    )?;
    value["chat"] = json!(chat.id);
    value["folder"] = json!(chat.folder);
    Ok(Output::Report { text, value })
}
