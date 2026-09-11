//! Exclusively owned, durable window presentation and pure workspace navigation.
//!
//! Only pass authoritative snapshots to `reconcile`. A disconnected window keeps
//! its last snapshot and state; connection generations are deliberately not keys.
use crate::{
    Result,
    config::{
        DEFAULT_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH, MAX_TERMINAL_OPACITY, MIN_SIDEBAR_WIDTH,
        MIN_TERMINAL_OPACITY,
    },
    theme::{BackgroundEffect, ThemePreset},
};
use compi_protocol::{
    LayoutNode, PaneId, ServerId, SessionId, TabId, TerminalIdentity, WorkspaceSession,
    WorkspaceSnapshot, WorkspaceTab,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const CLIENT_STATE_VERSION: u32 = 2;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_REFERENCES: usize = 16_384;
const MAX_ID_BYTES: usize = 256;
const MAX_SAVED_HISTORY_ROWS: usize = 1_048_576;

/// A conservative anchor, not terminal contents. The GUI must compare the exact
/// identity, dimensions, and full-row fingerprint before applying these values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedViewport {
    pub identity: TerminalIdentity,
    pub cols: u16,
    pub rows: u16,
    pub scroll_offset: usize,
    /// Inclusive selection endpoints, each stored as `[row, column]`.
    pub selection: Option<[[usize; 2]; 2]>,
    pub fingerprint: [u8; 32],
    /// Missing legacy sequence cannot prove that history was not evicted offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

impl SavedViewport {
    fn valid(&self, surface_id: &str) -> bool {
        valid_id(surface_id)
            && self.identity.surface_id.as_str() == surface_id
            && valid_id(self.identity.server_id.as_str())
            && valid_id(self.identity.server_generation.as_str())
            && valid_id(self.identity.process_lifetime_id.as_str())
            && (1..=i16::MAX as u16).contains(&self.cols)
            && (1..=i16::MAX as u16).contains(&self.rows)
            && self.scroll_offset <= MAX_SAVED_HISTORY_ROWS
            && self.selection.is_none_or(|points| {
                points.iter().all(|[row, col]| {
                    *row < MAX_SAVED_HISTORY_ROWS + usize::from(self.rows)
                        && *col < usize::from(self.cols)
                })
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowAppearanceOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemePreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_effect: Option<BackgroundEffect>,
}

impl WindowAppearanceOverrides {
    fn valid(&self) -> bool {
        self.terminal_opacity.is_none_or(|opacity| {
            opacity.is_finite() && (MIN_TERMINAL_OPACITY..=MAX_TERMINAL_OPACITY).contains(&opacity)
        })
    }

    fn is_default(value: &Self) -> bool {
        *value == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientState {
    pub version: u32,
    pub geometry: Option<WindowGeometry>,
    pub sidebar_width: f32,
    pub font_zoom: f32,
    #[serde(default, skip_serializing_if = "WindowAppearanceOverrides::is_default")]
    pub appearance: WindowAppearanceOverrides,
    pub selected_session: Option<SessionId>,
    pub selected_tabs: HashMap<String, TabId>,
    pub focused_panes: HashMap<String, PaneId>,
    pub hidden_tabs: HashSet<TabId>,
    #[serde(default)]
    pub viewports: HashMap<String, SavedViewport>,
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            version: CLIENT_STATE_VERSION,
            geometry: None,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            font_zoom: 1.0,
            appearance: WindowAppearanceOverrides::default(),
            selected_session: None,
            selected_tabs: HashMap::new(),
            focused_panes: HashMap::new(),
            hidden_tabs: HashSet::new(),
            viewports: HashMap::new(),
        }
    }
}

impl ClientState {
    pub fn selected_session<'a>(
        &self,
        workspace: &'a WorkspaceSnapshot,
    ) -> Option<&'a WorkspaceSession> {
        self.selected_session
            .as_ref()
            .and_then(|id| workspace.sessions.iter().find(|session| &session.id == id))
            .or_else(|| workspace.sessions.first())
    }

    pub fn visible_tabs<'a>(
        &'a self,
        session: &'a WorkspaceSession,
    ) -> impl Iterator<Item = &'a WorkspaceTab> {
        session
            .tabs
            .iter()
            .filter(|tab| !self.hidden_tabs.contains(&tab.id))
    }

    pub fn selected_tab<'a>(&self, workspace: &'a WorkspaceSnapshot) -> Option<&'a WorkspaceTab> {
        let session = self.selected_session(workspace)?;
        self.tab_in_session(session)
    }

    fn tab_in_session<'a>(&self, session: &'a WorkspaceSession) -> Option<&'a WorkspaceTab> {
        self.selected_tabs
            .get(session.id.as_str())
            .and_then(|id| {
                session
                    .tabs
                    .iter()
                    .find(|tab| &tab.id == id && !self.hidden_tabs.contains(id))
            })
            .or_else(|| {
                session
                    .tabs
                    .iter()
                    .find(|tab| !self.hidden_tabs.contains(&tab.id))
            })
    }

    pub fn focused_pane<'a>(&self, workspace: &'a WorkspaceSnapshot) -> Option<&'a PaneId> {
        let tab = self.selected_tab(workspace)?;
        self.focused_panes
            .get(tab.id.as_str())
            .and_then(|id| find_pane(&tab.layout, id))
            .or_else(|| Some(first_pane(&tab.layout)))
    }

    /// Prune only in response to an authoritative snapshot, never a disconnect.
    /// `previous` supplies the removed object's former position for fallback.
    pub fn reconcile(
        &mut self,
        previous: Option<&WorkspaceSnapshot>,
        authoritative: &WorkspaceSnapshot,
    ) {
        let previous = previous.filter(|old| old.server_id == authoritative.server_id);
        let session_ids: HashSet<_> = authoritative
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect();
        let tabs: HashMap<_, _> = authoritative
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .map(|tab| (tab.id.as_str(), tab))
            .collect();
        self.hidden_tabs.retain(|id| tabs.contains_key(id.as_str()));
        self.selected_tabs
            .retain(|id, _| session_ids.contains(id.as_str()));
        self.focused_panes
            .retain(|id, _| tabs.contains_key(id.as_str()));
        let surfaces: HashSet<_> = authoritative
            .surfaces
            .iter()
            .map(|surface| surface.id.as_str())
            .collect();
        self.viewports
            .retain(|id, _| surfaces.contains(id.as_str()));

        if self
            .selected_session
            .as_ref()
            .is_none_or(|id| !session_ids.contains(id.as_str()))
        {
            self.selected_session = self
                .selected_session
                .as_ref()
                .and_then(|id| {
                    let old = previous?;
                    let index = old.sessions.iter().position(|session| &session.id == id)?;
                    old.sessions[index + 1..]
                        .iter()
                        .chain(old.sessions[..index].iter().rev())
                        .find(|session| session_ids.contains(session.id.as_str()))
                        .map(|session| session.id.clone())
                })
                .or_else(|| {
                    authoritative
                        .sessions
                        .first()
                        .map(|session| session.id.clone())
                });
        }

        for session in &authoritative.sessions {
            let selected = self.selected_tabs.get(session.id.as_str());
            let valid = selected.is_some_and(|id| {
                session
                    .tabs
                    .iter()
                    .any(|tab| &tab.id == id && !self.hidden_tabs.contains(id))
            });
            if !valid {
                let old_session =
                    previous.and_then(|old| old.sessions.iter().find(|old| old.id == session.id));
                let replacement = selected
                    .and_then(|id| self.neighbor_tab(session, old_session, id))
                    .or_else(|| {
                        session
                            .tabs
                            .iter()
                            .find(|tab| !self.hidden_tabs.contains(&tab.id))
                    });
                if let Some(tab) = replacement {
                    self.selected_tabs
                        .insert(session.id.to_string(), tab.id.clone());
                } else {
                    self.selected_tabs.remove(session.id.as_str());
                }
            }
            for tab in &session.tabs {
                if self
                    .focused_panes
                    .get(tab.id.as_str())
                    .is_none_or(|id| find_pane(&tab.layout, id).is_none())
                {
                    self.focused_panes
                        .insert(tab.id.to_string(), first_pane(&tab.layout).clone());
                }
            }
        }
    }

    fn neighbor_tab<'a>(
        &self,
        session: &'a WorkspaceSession,
        previous: Option<&WorkspaceSession>,
        id: &TabId,
    ) -> Option<&'a WorkspaceTab> {
        let order = if session.tabs.iter().any(|tab| &tab.id == id) {
            session
        } else {
            previous?
        };
        let index = order.tabs.iter().position(|tab| &tab.id == id)?;
        order.tabs[index + 1..]
            .iter()
            .chain(order.tabs[..index].iter().rev())
            .find_map(|candidate| {
                session
                    .tabs
                    .iter()
                    .find(|tab| tab.id == candidate.id && !self.hidden_tabs.contains(&tab.id))
            })
    }

    pub fn select_session(&mut self, workspace: &WorkspaceSnapshot, id: &SessionId) -> bool {
        let Some(session) = workspace.sessions.iter().find(|session| &session.id == id) else {
            return false;
        };
        self.selected_session = Some(id.clone());
        if let Some(tab) = self.tab_in_session(session) {
            let tab_id = tab.id.clone();
            self.selected_tabs.insert(id.to_string(), tab_id.clone());
            self.restore_focus(tab);
        }
        true
    }

    /// Select visible work only. Restoring hidden work is an explicit action.
    pub fn select_tab(&mut self, workspace: &WorkspaceSnapshot, id: &TabId) -> bool {
        if self.hidden_tabs.contains(id) {
            return false;
        }
        let Some((session, tab)) = locate_tab(workspace, id) else {
            return false;
        };
        self.selected_session = Some(session.id.clone());
        self.selected_tabs
            .insert(session.id.to_string(), id.clone());
        self.restore_focus(tab);
        true
    }

    fn restore_focus(&mut self, tab: &WorkspaceTab) {
        if self
            .focused_panes
            .get(tab.id.as_str())
            .is_none_or(|id| find_pane(&tab.layout, id).is_none())
        {
            self.focused_panes
                .insert(tab.id.to_string(), first_pane(&tab.layout).clone());
        }
    }

    pub fn focus_pane(&mut self, workspace: &WorkspaceSnapshot, id: &PaneId) -> bool {
        let Some(tab) = self.selected_tab(workspace) else {
            return false;
        };
        if find_pane(&tab.layout, id).is_none() {
            return false;
        }
        self.focused_panes.insert(tab.id.to_string(), id.clone());
        true
    }

    pub fn hide_tab(&mut self, workspace: &WorkspaceSnapshot, id: &TabId) -> bool {
        let Some((session, _)) = locate_tab(workspace, id) else {
            return false;
        };
        let was_selected = self
            .tab_in_session(session)
            .is_some_and(|tab| &tab.id == id);
        self.hidden_tabs.insert(id.clone());
        if was_selected {
            if let Some(next) = self.neighbor_tab(session, None, id) {
                self.selected_tabs
                    .insert(session.id.to_string(), next.id.clone());
                self.restore_focus(next);
            } else {
                self.selected_tabs.remove(session.id.as_str());
            }
        }
        true
    }

    pub fn restore_tab(&mut self, workspace: &WorkspaceSnapshot, id: &TabId) -> bool {
        if locate_tab(workspace, id).is_none() {
            return false;
        }
        self.hidden_tabs.remove(id);
        self.select_tab(workspace, id)
    }

    pub fn reset(&mut self, defaults: &ClientState) {
        *self = defaults.clone();
        self.version = CLIENT_STATE_VERSION;
        self.viewports.clear();
    }

    /// Initialize only a newly allocated tear-off destination, before attachment.
    /// Pass the source's durable state, not its CLI overrides or theme preview.
    /// Existing destinations use `restore_tab` instead and keep their presentation.
    pub fn initialize_transfer(
        &mut self,
        workspace: &WorkspaceSnapshot,
        id: &TabId,
        source: &ClientState,
    ) -> bool {
        let Some((_, tab)) = locate_tab(workspace, id) else {
            return false;
        };
        self.geometry = None;
        self.appearance = source.appearance;
        self.font_zoom = source.font_zoom;
        self.sidebar_width = source.sidebar_width;
        self.selected_session = None;
        self.selected_tabs.clear();
        self.focused_panes.clear();
        self.viewports.clear();
        copy_viewports(&tab.layout, &source.viewports, &mut self.viewports);
        if let Some(pane) = source.focused_panes.get(id.as_str()) {
            self.focused_panes.insert(id.to_string(), pane.clone());
        }
        self.hidden_tabs = workspace
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .filter(|tab| &tab.id != id)
            .map(|tab| tab.id.clone())
            .collect();
        self.select_tab(workspace, id)
    }

    /// Bound file-derived presentation without consulting or creating server work.
    /// Returns whether recoverable values were sanitized; structural excess is an error.
    fn sanitize(&mut self, defaults: &ClientState) -> Result<bool> {
        let mut changed = false;
        match self.version {
            CLIENT_STATE_VERSION => {}
            // Version 1 stored a materialized theme copied from configuration.
            // Ignore that legacy field so global TOML remains authoritative.
            1 => {
                self.version = CLIENT_STATE_VERSION;
                changed = true;
            }
            version => return Err(format!("unsupported client-state version {version}").into()),
        }
        if self.selected_tabs.len() > MAX_REFERENCES
            || self.focused_panes.len() > MAX_REFERENCES
            || self.hidden_tabs.len() > MAX_REFERENCES
            || self.viewports.len() > MAX_REFERENCES
        {
            return Err("client state exceeds its navigation reference limit".into());
        }
        if !self.appearance.valid() {
            self.appearance.terminal_opacity = None;
            changed = true;
        }
        changed |= sanitize_number(
            &mut self.sidebar_width,
            defaults.sidebar_width,
            MIN_SIDEBAR_WIDTH,
            MAX_SIDEBAR_WIDTH,
            DEFAULT_SIDEBAR_WIDTH,
        );
        changed |= sanitize_number(&mut self.font_zoom, defaults.font_zoom, 0.5, 3.0, 1.0);
        if let Some(geometry) = &mut self.geometry {
            if [geometry.x, geometry.y, geometry.width, geometry.height]
                .iter()
                .any(|value| !value.is_finite())
            {
                self.geometry = None;
                changed = true;
            } else {
                changed |= sanitize_number(&mut geometry.x, 0.0, -65_536.0, 65_536.0, 0.0);
                changed |= sanitize_number(&mut geometry.y, 0.0, -65_536.0, 65_536.0, 0.0);
                changed |= sanitize_number(&mut geometry.width, 1100.0, 320.0, 32_768.0, 1100.0);
                changed |= sanitize_number(&mut geometry.height, 760.0, 200.0, 32_768.0, 760.0);
            }
        }
        if self
            .selected_session
            .as_ref()
            .is_some_and(|id| !valid_id(id.as_str()))
        {
            self.selected_session = None;
            changed = true;
        }
        let before = self.selected_tabs.len()
            + self.focused_panes.len()
            + self.hidden_tabs.len()
            + self.viewports.len();
        self.selected_tabs
            .retain(|key, id| valid_id(key) && valid_id(id.as_str()));
        self.focused_panes
            .retain(|key, id| valid_id(key) && valid_id(id.as_str()));
        self.hidden_tabs.retain(|id| valid_id(id.as_str()));
        self.viewports.retain(|id, viewport| viewport.valid(id));
        changed |= before
            != self.selected_tabs.len()
                + self.focused_panes.len()
                + self.hidden_tabs.len()
                + self.viewports.len();
        Ok(changed)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES && !id.chars().any(char::is_control)
}

fn sanitize_number(value: &mut f32, configured: f32, min: f32, max: f32, fallback: f32) -> bool {
    let old = *value;
    *value = if value.is_finite() {
        value.clamp(min, max)
    } else if configured.is_finite() {
        configured.clamp(min, max)
    } else {
        fallback
    };
    old != *value
}

fn locate_tab<'a>(
    workspace: &'a WorkspaceSnapshot,
    id: &TabId,
) -> Option<(&'a WorkspaceSession, &'a WorkspaceTab)> {
    workspace.sessions.iter().find_map(|session| {
        session
            .tabs
            .iter()
            .find(|tab| &tab.id == id)
            .map(|tab| (session, tab))
    })
}

fn copy_viewports(
    layout: &LayoutNode,
    source: &HashMap<String, SavedViewport>,
    destination: &mut HashMap<String, SavedViewport>,
) {
    match layout {
        LayoutNode::Pane { surface_id, .. } => {
            if let Some(viewport) = source.get(surface_id.as_str()) {
                destination.insert(surface_id.to_string(), viewport.clone());
            }
        }
        LayoutNode::Split { first, second, .. } => {
            copy_viewports(first, source, destination);
            copy_viewports(second, source, destination);
        }
    }
}

fn first_pane(layout: &LayoutNode) -> &PaneId {
    match layout {
        LayoutNode::Pane { pane_id, .. } => pane_id,
        LayoutNode::Split { first, .. } => first_pane(first),
    }
}

fn find_pane<'a>(layout: &'a LayoutNode, id: &PaneId) -> Option<&'a PaneId> {
    match layout {
        LayoutNode::Pane { pane_id, .. } => (pane_id == id).then_some(pane_id),
        LayoutNode::Split { first, second, .. } => {
            find_pane(first, id).or_else(|| find_pane(second, id))
        }
    }
}

/// The separate lock inode remains held even while the JSON is atomically replaced.
/// Never clone this owner or open the state file as an unlocked writer.
pub struct StateSlot {
    pub state: ClientState,
    pub diagnostics: Vec<String>,
    id: String,
    path: PathBuf,
    _lock: File,
    persisted: Option<ClientState>,
    last_save_failed: bool,
}

impl StateSlot {
    pub fn claim(
        instance: Option<&str>,
        server_id: &ServerId,
        defaults: &ClientState,
    ) -> Result<Self> {
        // Match the transport's instance validation and distinguish Windows' unnamed instance.
        if let Some(instance) = instance
            && (instance.is_empty()
                || instance.len() > 32
                || !instance
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
        {
            return Err("instance names must be 1-32 ASCII letters, digits, '-' or '_'".into());
        }
        if !valid_id(server_id.as_str()) {
            return Err("invalid stable server identity for client state".into());
        }
        let root = compi_protocol::paths::data_dir()?.join("client-state-v1");
        #[cfg(unix)]
        let instance = Some(instance.unwrap_or("default"));
        let root = root.join(
            instance
                .map(|name| format!("instance-{name}"))
                .unwrap_or_else(|| "primary-instance".to_owned()),
        );
        // Hex encoding is lossless and separator-free, including on case-insensitive filesystems.
        // Chunk long identities so no path component exceeds native filename limits.
        let mut root = root;
        for chunk in server_id.as_str().as_bytes().chunks(48) {
            let mut name = String::from("server-");
            use std::fmt::Write as _;
            for byte in chunk {
                write!(&mut name, "{byte:02x}")?;
            }
            root.push(name);
        }
        Self::claim_in(&root, defaults)
    }

    fn claim_in(root: &Path, defaults: &ClientState) -> Result<Self> {
        fs::create_dir_all(root)?;
        let registry = open_lock(&root.join("registry.lock"))?;
        match registry.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "Another window is allocating its state; retry opening the window.",
                )
                .into());
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let mut ordinals = vec![0];
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(ordinal) = name
                .strip_prefix("aux-")
                .and_then(|name| name.strip_suffix(".lock"))
                .and_then(|name| name.parse::<u64>().ok())
                && ordinal > 0
                && name == format!("aux-{ordinal:020}.lock")
            {
                ordinals.push(ordinal);
            }
        }
        ordinals.sort_unstable();
        ordinals.dedup();
        let next = ordinals
            .last()
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("client slot ordinal exhausted")?;
        for ordinal in ordinals.into_iter().chain(std::iter::once(next)) {
            let id = if ordinal == 0 {
                "primary".to_owned()
            } else {
                format!("aux-{ordinal:020}")
            };
            let lock = open_lock(&root.join(format!("{id}.lock")))?;
            match lock.try_lock() {
                Ok(()) => {
                    let mut slot = Self {
                        state: defaults.clone(),
                        diagnostics: Vec::new(),
                        path: root.join(format!("{id}.json")),
                        id,
                        _lock: lock,
                        persisted: None,
                        last_save_failed: false,
                    };
                    slot.state.version = CLIENT_STATE_VERSION;
                    slot.state.sanitize(defaults)?;
                    slot.load(defaults)?;
                    // Retain ownership and a usable live view on save failure, with an explicit diagnostic.
                    if slot.persisted.is_none() {
                        let _ = slot.save();
                    }
                    return Ok(slot);
                }
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        Err("could not claim an exclusive client state slot".into())
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn is_unsaved(&self) -> bool {
        self.last_save_failed || self.persisted.as_ref() != Some(&self.state)
    }

    fn load(&mut self, defaults: &ClientState) -> Result<()> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
        let parsed = (|| -> Result<ClientState> {
            if bytes.len() as u64 > MAX_STATE_BYTES {
                return Err("client state exceeds one MiB".into());
            }
            let state: ClientState = serde_json::from_slice(&bytes)?;
            Ok(state)
        })();
        let parsed = parsed.and_then(|mut state| {
            let changed = state.sanitize(defaults)?;
            Ok((state, changed))
        });
        match parsed {
            Ok((state, changed)) => {
                if changed {
                    self.diagnostics.push(
                        "Corrected invalid client-state presentation or navigation values."
                            .to_owned(),
                    );
                }
                if !changed {
                    self.persisted = Some(state.clone());
                }
                self.state = state;
            }
            Err(error) => {
                let quarantine = quarantine(&self.path)?;
                self.diagnostics.push(format!("Recovered malformed client state: {error}. Preserved it at {}; configured defaults restored.", quarantine.display()));
            }
        }
        Ok(())
    }

    /// Errors never roll back live presentation or alter server mutation results.
    /// Keep invocation-only overrides and unaccepted previews outside `state`.
    pub fn save(&mut self) -> Result<()> {
        let result = self.save_inner();
        match result {
            Ok(()) => {
                self.persisted = Some(self.state.clone());
                self.last_save_failed = false;
                Ok(())
            }
            Err(error) => {
                self.last_save_failed = true;
                let message = format!("Window state is unsaved: {error}");
                if self.diagnostics.last() != Some(&message) {
                    if self.diagnostics.len() >= 32 {
                        self.diagnostics.remove(0);
                    }
                    self.diagnostics.push(message);
                }
                Err(error)
            }
        }
    }

    fn save_inner(&self) -> Result<()> {
        let state = &self.state;
        let geometry_valid = state.geometry.as_ref().is_none_or(|geometry| {
            (-65_536.0..=65_536.0).contains(&geometry.x)
                && (-65_536.0..=65_536.0).contains(&geometry.y)
                && (320.0..=32_768.0).contains(&geometry.width)
                && (200.0..=32_768.0).contains(&geometry.height)
        });
        if state.version != CLIENT_STATE_VERSION
            || !geometry_valid
            || !(MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH).contains(&state.sidebar_width)
            || !(0.5..=3.0).contains(&state.font_zoom)
            || !state.appearance.valid()
            || state.focused_panes.len() > MAX_REFERENCES
            || state.hidden_tabs.len() > MAX_REFERENCES
            || state.viewports.len() > MAX_REFERENCES
            || state
                .selected_session
                .as_ref()
                .is_some_and(|id| !valid_id(id.as_str()))
            || state
                .selected_tabs
                .iter()
                .any(|(key, id)| !valid_id(key) || !valid_id(id.as_str()))
            || state
                .focused_panes
                .iter()
                .any(|(key, id)| !valid_id(key) || !valid_id(id.as_str()))
            || state.hidden_tabs.iter().any(|id| !valid_id(id.as_str()))
            || state
                .viewports
                .iter()
                .any(|(id, viewport)| !viewport.valid(id))
        {
            return Err("client state contains invalid presentation or navigation values".into());
        }
        let bytes = serde_json::to_vec_pretty(&self.state)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("client state exceeds one MiB".into());
        }
        let temporary = self.path.with_extension("json.tmp");
        // An exclusively held slot permits deterministic recovery of its stale temporary file.
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            replace_file(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn open_lock(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn quarantine(path: &Path) -> Result<PathBuf> {
    for ordinal in 0..10_000 {
        let destination = path.with_extension(format!("corrupt-{ordinal}.json"));
        if !destination.try_exists()? {
            fs::rename(path, &destination)?;
            #[cfg(unix)]
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
            return Ok(destination);
        }
    }
    Err("could not allocate a quarantine file for malformed client state".into())
}

#[cfg(unix)]
fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
    fs::rename(temporary, destination)?;
    if let Some(parent) = destination.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };
    let temporary: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(temporary.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::{ServerGeneration, SurfaceId};
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    };

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                    "compi-client-state-{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )))
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn workspace() -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            server_id: ServerId::from("server"),
            server_generation: ServerGeneration::from("generation"),
            revision: 1,
            initialized: true,
            surfaces: Vec::new(),
            recovery_message: None,
            sessions: ["one", "two", "three"]
                .into_iter()
                .map(|session| WorkspaceSession {
                    id: SessionId::from(session),
                    label: session.to_owned(),
                    tabs: ["a", "b", "c"]
                        .into_iter()
                        .map(|tab| {
                            let id = format!("{session}-{tab}");
                            WorkspaceTab {
                                id: TabId::from(id.clone()),
                                label: id.clone(),
                                layout: LayoutNode::Pane {
                                    pane_id: PaneId::from(format!("{id}-pane")),
                                    surface_id: SurfaceId::from(format!("{id}-surface")),
                                },
                            }
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn legacy_materialized_theme_migrates_to_inherited_global_appearance() {
        let mut state: ClientState = serde_json::from_value(serde_json::json!({
            "version": 1,
            "geometry": null,
            "sidebar_width": 280.0,
            "font_zoom": 1.0,
            "theme": "warm-carbon",
            "selected_session": null,
            "selected_tabs": {},
            "focused_panes": {},
            "hidden_tabs": [],
            "viewports": {}
        }))
        .unwrap();
        assert!(state.sanitize(&ClientState::default()).unwrap());
        assert_eq!(state.version, CLIENT_STATE_VERSION);
        assert_eq!(state.appearance, WindowAppearanceOverrides::default());

        state.appearance.terminal_opacity = Some(f32::NAN);
        assert!(state.sanitize(&ClientState::default()).unwrap());
        assert_eq!(state.appearance.terminal_opacity, None);
    }

    #[test]
    fn registry_contention_is_retryable_without_an_unlocked_writer() {
        let directory = Directory::new();
        fs::create_dir_all(&directory.0).unwrap();
        let registry = open_lock(&directory.0.join("registry.lock")).unwrap();
        registry.lock().unwrap();
        let result = StateSlot::claim_in(&directory.0, &ClientState::default());
        let error = result
            .err()
            .expect("a held registry must prevent allocation");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!directory.0.join("primary.json").exists());
        drop(registry);
        assert_eq!(
            StateSlot::claim_in(&directory.0, &ClientState::default())
                .unwrap()
                .id(),
            "primary"
        );
    }

    #[test]
    fn concurrent_claims_are_exclusive_and_reuse_oldest_free_slot() {
        let directory = Directory::new();
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let root = directory.0.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    loop {
                        match StateSlot::claim_in(&root, &ClientState::default()) {
                            Ok(slot) => break slot,
                            Err(error)
                                if error.downcast_ref::<std::io::Error>().is_some_and(
                                    |error| error.kind() == std::io::ErrorKind::WouldBlock,
                                ) =>
                            {
                                assert!(
                                    std::time::Instant::now() < deadline,
                                    "registry allocation never completed"
                                );
                                std::thread::yield_now();
                            }
                            Err(error) => panic!("slot allocation failed: {error}"),
                        }
                    }
                })
            })
            .collect();
        let mut slots: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let ids: HashSet<_> = slots.iter().map(|slot| slot.id().to_owned()).collect();
        assert_eq!(ids.len(), 8);
        assert!(ids.contains("primary"));
        let index = slots
            .iter()
            .position(|slot| slot.id() == "aux-00000000000000000002")
            .unwrap();
        let mut released = slots.swap_remove(index);
        released.state.font_zoom = 1.25;
        released.save().unwrap();
        drop(released);
        let reclaimed = StateSlot::claim_in(&directory.0, &ClientState::default()).unwrap();
        assert_eq!(reclaimed.id(), "aux-00000000000000000002");
        assert_eq!(reclaimed.state.font_zoom, 1.25);
        drop(reclaimed);
        let primary = slots
            .iter()
            .position(|slot| slot.id() == "primary")
            .unwrap();
        drop(slots.swap_remove(primary));
        assert_eq!(
            StateSlot::claim_in(&directory.0, &ClientState::default())
                .unwrap()
                .id(),
            "primary"
        );
    }

    #[test]
    fn malformed_state_is_preserved_and_failed_save_keeps_live_state() {
        let directory = Directory::new();
        let slot = StateSlot::claim_in(&directory.0, &ClientState::default()).unwrap();
        let path = slot.path.clone();
        drop(slot);
        fs::write(&path, b"{broken").unwrap();
        let mut slot = StateSlot::claim_in(&directory.0, &ClientState::default()).unwrap();
        assert_eq!(
            fs::read(path.with_extension("corrupt-0.json")).unwrap(),
            b"{broken"
        );
        let saved = fs::read(&path).unwrap();
        fs::create_dir(path.with_extension("json.tmp")).unwrap();
        slot.state.font_zoom = 1.5;
        assert!(slot.save().is_err());
        assert!(slot.is_unsaved());
        assert_eq!(slot.state.font_zoom, 1.5);
        assert_eq!(fs::read(&path).unwrap(), saved);
        fs::remove_dir(path.with_extension("json.tmp")).unwrap();
        slot.save().unwrap();
        assert!(!slot.is_unsaved());
    }

    #[test]
    fn viewport_anchors_follow_surface_membership_not_connection_generation() {
        let mut workspace = workspace();
        let viewport = SavedViewport {
            identity: TerminalIdentity {
                server_id: workspace.server_id.clone(),
                server_generation: workspace.server_generation.clone(),
                surface_id: SurfaceId::from("one-a-surface"),
                process_lifetime_id: compi_protocol::ProcessLifetimeId::from("lifetime"),
            },
            cols: 80,
            rows: 24,
            scroll_offset: 10,
            selection: Some([[1, 2], [3, 4]]),
            fingerprint: [42; 32],
            sequence: Some(7),
        };
        workspace.surfaces.push(compi_protocol::SurfaceInfo {
            id: viewport.identity.surface_id.clone(),
            process_lifetime_id: viewport.identity.process_lifetime_id.clone(),
            status: compi_protocol::SurfaceStatus::Running,
            attached: false,
            cols: 80,
            rows: 24,
            created_at_ms: 0,
            exit_code: None,
            error: None,
            launch: compi_protocol::LaunchRequest {
                working_directory: None,
                profile: None,
            },
            working_directory: None,
        });
        let mut source = ClientState::default();
        source
            .viewports
            .insert("one-a-surface".to_owned(), viewport.clone());
        let mut unrelated = viewport.clone();
        unrelated.identity.surface_id = SurfaceId::from("two-a-surface");
        source
            .viewports
            .insert("two-a-surface".to_owned(), unrelated);
        let mut destination = ClientState::default();
        destination.initialize_transfer(&workspace, &TabId::from("one-a"), &source);
        assert!(destination.viewports.contains_key("one-a-surface"));
        assert!(!destination.viewports.contains_key("two-a-surface"));
        let previous = workspace.clone();
        workspace.server_generation = ServerGeneration::from("next-connection-generation");
        destination.reconcile(Some(&previous), &workspace);
        assert_eq!(destination.viewports.get("one-a-surface"), Some(&viewport));
        let previous = workspace.clone();
        workspace.surfaces.clear();
        workspace.sessions[0].tabs.remove(0);
        destination.reconcile(Some(&previous), &workspace);
        assert!(!destination.viewports.contains_key("one-a-surface"));
        source.reset(&ClientState::default());
        assert!(source.viewports.is_empty());
    }

    #[test]
    fn navigation_falls_forward_then_backward_and_preserves_other_sessions() {
        let mut workspace = workspace();
        let mut state = ClientState::default();
        state.reconcile(None, &workspace);
        state.select_tab(&workspace, &TabId::from("one-b"));
        state.select_tab(&workspace, &TabId::from("two-b"));
        state.select_session(&workspace, &SessionId::from("one"));
        assert_eq!(state.selected_tab(&workspace).unwrap().id.as_str(), "one-b");
        state.hide_tab(&workspace, &TabId::from("one-b"));
        assert_eq!(state.selected_tab(&workspace).unwrap().id.as_str(), "one-c");
        let previous = workspace.clone();
        workspace.sessions[0].tabs.pop();
        workspace.server_generation = ServerGeneration::from("restarted");
        state.reconcile(Some(&previous), &workspace);
        assert_eq!(state.selected_tab(&workspace).unwrap().id.as_str(), "one-a");
        assert!(state.hidden_tabs.contains(&TabId::from("one-b")));
        state.select_session(&workspace, &SessionId::from("two"));
        assert_eq!(state.selected_tab(&workspace).unwrap().id.as_str(), "two-b");
        let previous = workspace.clone();
        workspace.sessions.remove(1);
        state.reconcile(Some(&previous), &workspace);
        assert_eq!(
            state.selected_session(&workspace).unwrap().id.as_str(),
            "three"
        );
        assert!(!state.selected_tabs.contains_key("two"));
    }

    #[test]
    fn all_hidden_is_empty_and_transfer_is_local_and_keeps_valid_focus() {
        let mut workspace = workspace();
        workspace.sessions[0].tabs[0].layout = LayoutNode::Split {
            axis: compi_protocol::SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(workspace.sessions[0].tabs[0].layout.clone()),
            second: Box::new(LayoutNode::Pane {
                pane_id: PaneId::from("second-pane"),
                surface_id: SurfaceId::from("second-surface"),
            }),
        };
        let mut source = ClientState::default();
        source.select_tab(&workspace, &TabId::from("one-a"));
        assert!(source.focus_pane(&workspace, &PaneId::from("second-pane")));
        let mut destination = ClientState::default();
        assert!(destination.initialize_transfer(&workspace, &TabId::from("one-a"), &source));
        assert_eq!(
            destination.focused_pane(&workspace).unwrap().as_str(),
            "second-pane"
        );
        assert!(source.hidden_tabs.is_empty());
        destination.hide_tab(&workspace, &TabId::from("one-a"));
        assert!(destination.selected_tab(&workspace).is_none());
        assert!(destination.restore_tab(&workspace, &TabId::from("one-a")));
        let previous = workspace.clone();
        workspace.sessions[0].tabs[0].layout = LayoutNode::Pane {
            pane_id: PaneId::from("replacement"),
            surface_id: SurfaceId::from("replacement-surface"),
        };
        destination.reconcile(Some(&previous), &workspace);
        assert_eq!(
            destination.focused_pane(&workspace).unwrap().as_str(),
            "replacement"
        );
        assert!(destination.hidden_tabs.contains(&TabId::from("two-a")));
    }
}
