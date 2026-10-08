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
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: PaneId,
    pub surface_id: SurfaceId,
    pub cols: i16,
    pub rows: i16,
    pub focused: bool,
    pub floating: bool,
    pub zoomed: bool,
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
        Err("GUI presentation control is unavailable on this platform; daemon workspace, pane, metadata, and input commands remain available. Run window controls on the native Windows or macOS GUI host".into())
    }
}
