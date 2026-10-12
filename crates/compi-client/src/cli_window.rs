//! Explicit, acknowledged presentation control through the same-user GUI host.
use crate::connection::ConnectionTarget;
use compi_protocol::{PaneId, Result, SurfaceId, TabId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    List,
    FocusTab {
        tab_id: TabId,
    },
    DetachTab {
        tab_id: TabId,
    },
    FocusPane {
        pane_id: PaneId,
    },
    ResizePane {
        pane_id: PaneId,
        cols: i16,
        rows: i16,
    },
    ZoomPane {
        pane_id: PaneId,
    },
    UnzoomPane {
        pane_id: PaneId,
    },
    FloatPane {
        pane_id: PaneId,
    },
    DockPane {
        pane_id: PaneId,
    },
    /// Focus the content pane `pane_id`, show `file` (one of its open files) and put
    /// the cursor at the start of the 1-based `line`, scrolled into view.
    RevealFile {
        pane_id: PaneId,
        file: String,
        line: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetIdentity {
    pub instance: Option<String>,
    pub connect: Option<String>,
}
impl TargetIdentity {
    pub fn from_target(target: &ConnectionTarget) -> Self {
        let (instance, connect) = target.launch_options();
        Self { instance, connect }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub target: TargetIdentity,
    pub window: Option<String>,
    pub action: Action,
}
impl Request {
    pub(crate) fn validate(&self) -> Result<()> {
        if self
            .window
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 256)
        {
            return Err("window ID must be a nonempty stable window slot ID".into());
        }
        if let Action::ResizePane { cols, rows, .. } = self.action
            && (cols < crate::layout::MIN_COLUMNS as i16 || rows < crate::layout::MIN_ROWS as i16)
        {
            return Err("GUI panes require at least 20 columns and 4 rows".into());
        }
        if let Action::RevealFile { line: 0, .. } = self.action {
            return Err("file lines are numbered from 1".into());
        }
        Ok(())
    }
}

/// What a pane holds; reported as `"type": "terminal" | "content"`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaneType {
    Terminal,
    Content,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: PaneId,
    #[serde(rename = "type")]
    pub pane_type: PaneType,
    /// The terminal's surface and grid; `None` for a content pane, which has neither.
    pub surface_id: Option<SurfaceId>,
    pub cols: Option<i16>,
    pub rows: Option<i16>,
    pub focused: bool,
    pub floating: bool,
    pub zoomed: bool,
    /// Absolute paths of files with unsaved edits in this window's content pane; empty
    /// (and omitted) for terminal panes and for content panes with nothing unsaved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsaved_files: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window_id: String,
    pub target: TargetIdentity,
    pub server_id: Option<String>,
    pub selected_tab: Option<TabId>,
    pub visible_tabs: Vec<TabId>,
    pub panes: Vec<PaneInfo>,
    pub busy: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Applied {
    pub window_id: String,
    pub action: Action,
    pub destination_window_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub windows: Vec<WindowInfo>,
    pub applied: Option<Applied>,
}

/// Never starts a GUI, creates a shell, or assumes a window when several match.
pub fn execute(
    target: &ConnectionTarget,
    window: Option<&str>,
    action: Action,
) -> Result<Response> {
    send(target, window, action)?.ok_or_else(|| {
        if cfg!(any(windows, target_os = "macos")) {
            "No GUI host is running for this connection; open a Compi window first"
        } else {
            "GUI presentation control is unavailable on this platform; daemon workspace, pane, metadata, and input commands remain available. Run window controls on the native Windows or macOS GUI host"
        }
        .into()
    })
}

/// Every window of this connection, or `None` when no GUI host is running.
pub fn windows(target: &ConnectionTarget) -> Result<Option<Vec<WindowInfo>>> {
    Ok(send(target, None, Action::List)?.map(|response| response.windows))
}

/// `None` when no GUI host is running; none runs on platforms without the native GUI.
fn send(
    target: &ConnectionTarget,
    window: Option<&str>,
    action: Action,
) -> Result<Option<Response>> {
    let request = Request {
        target: TargetIdentity::from_target(target),
        window: window.map(str::to_owned),
        action,
    };
    request.validate()?;
    #[cfg(any(windows, target_os = "macos"))]
    {
        crate::window_host::control(target.state_instance().as_deref(), request)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Ok(None)
    }
}
