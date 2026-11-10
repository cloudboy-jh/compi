//! `compi chat`: a new chat with OMP in the terminal it runs from, in the shell's current
//! directory. As `[chat] open` says, the terminal shows it as its view, in place of its
//! shell, which keeps running underneath (Cmd/Ctrl+Shift+E or Esc returns to the shell),
//! or a new pane beside the terminal shows it.
use super::content::{self, Placed};
use super::{CliError, Options, Output, submit, targets};
use crate::config::ChatOpen;
use crate::{DaemonClient, cli_window::Action, connection::ConnectionTarget};
use compi_protocol::{
    LayoutNode, PaneContent, SplitAxis, SplitGeometry, WorkspaceMutation, WorkspaceSnapshot,
};
use serde_json::json;

pub(super) fn run(
    client: &mut DaemonClient,
    state: &WorkspaceSnapshot,
    target: &ConnectionTarget,
    options: &Options,
    open: ChatOpen,
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
    let content = PaneContent::Chat(chat.clone());
    let placed = match (open, caller.surface) {
        (ChatOpen::Beside, Some(surface)) => {
            let receipt = submit(
                client,
                state,
                WorkspaceMutation::OpenContent {
                    pane_id: caller.id.clone(),
                    axis: SplitAxis::Horizontal,
                    // In the terminal's cells; the window lays the panes out in pixels.
                    geometry: SplitGeometry {
                        width: f32::from(surface.cols),
                        height: f32::from(surface.rows),
                        min_width: 20.0,
                        min_height: 4.0,
                        divider: 1.0,
                    },
                    content,
                },
                None,
            )?;
            let opened = receipt
                .affected_panes
                .iter()
                .find(|pane| **pane != caller.id)
                .cloned()
                .ok_or_else(|| {
                    CliError::new(1, "pane_missing", "the new chat pane was not reported")
                })?;
            Placed {
                pane_id: opened,
                receipt: Some(receipt),
            }
        }
        _ => {
            let receipt = submit(
                client,
                state,
                WorkspaceMutation::ShowView {
                    pane_id: caller.id.clone(),
                    content,
                },
                None,
            )?;
            Placed {
                pane_id: caller.id.clone(),
                receipt: Some(receipt),
            }
        }
    };
    let pane_id = placed.pane_id.clone();
    let (text, mut value) = content::report(
        client,
        target,
        options,
        &placed,
        &format!("{} · {}", chat.harness.name(), chat.folder),
        Action::FocusPane { pane_id },
    )?;
    value["chat"] = json!(chat.id);
    value["folder"] = json!(chat.folder);
    Ok(Output::Report { text, value })
}
