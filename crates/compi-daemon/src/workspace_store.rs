use crate::Result;
use compi_protocol::{
    LaunchRequest, LayoutNode, MutationReceipt, PaneId, ProcessLifetimeId, ServerGeneration,
    ServerId, SessionId, SurfaceId, SurfaceInfo, SurfaceStatus, TabId, WorkingDirectory,
    WorkspaceSession, WorkspaceSnapshot, WorkspaceTab,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
#[cfg(windows)]
use std::iter::once;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
#[cfg(windows)]
use windows::core::PCWSTR;

/// Format 2 (protocol 23) holds a content pane's open files and diffs as `tabs`; format 3
/// (protocol 25) holds one tab per file, a diff being that tab's mode. Format 4 (protocol
/// 27) tags every content leaf with its `kind`, so a content pane may show Settings.
/// Format 5 (protocol 28) adds the Monitor kind, stored so older daemons can read the
/// file far enough to refuse it (see `readable_by_older_formats`). Format 6 (protocol 29)
/// lets a terminal leaf carry a content `view`; older daemons ignore the key and reach
/// their format check. Format 7 (protocol 31) adds commit diff tabs, also stored so older
/// daemons can refuse the file.
const FORMAT_VERSION: u32 = 7;

#[derive(Clone)]
pub struct WorkspaceStore {
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredWorkspace {
    pub server_id: ServerId,
    pub revision: u64,
    pub initialized: bool,
    pub sessions: Vec<WorkspaceSession>,
    pub surfaces: Vec<SurfaceInfo>,
    pub receipts: Vec<MutationReceipt>,
    #[serde(default)]
    pub pending_removals: Vec<PendingRemoval>,
    pub recovery_message: Option<String>,
    /// The directory each shell last reported (OSC 7), so a shell lost when the daemon
    /// restarts comes back where the user was. Daemon-internal; never sent to clients.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub last_directories: BTreeMap<SurfaceId, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingRemoval {
    pub target: RemovalTarget,
    pub surfaces: Vec<(SurfaceId, ProcessLifetimeId)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemovalTarget {
    Pane { pane_id: PaneId },
    Tab { tab_id: TabId },
    Session { session_id: SessionId },
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format_version: u32,
    workspace: StoredWorkspace,
}

#[derive(Deserialize)]
struct LegacyManifest {
    format_version: u32,
    sessions: Vec<LegacySession>,
}

#[derive(Deserialize)]
struct LegacySession {
    id: String,
    created_at_ms: u64,
    status: LegacyStatus,
    cols: i16,
    rows: i16,
    exit_code: Option<u32>,
    error: Option<String>,
    working_directory: Option<WorkingDirectory>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyStatus {
    Starting,
    Running,
    Exited,
    Failed,
    Dead,
}

impl WorkspaceStore {
    pub fn memory() -> (Self, StoredWorkspace) {
        (
            Self { path: None },
            StoredWorkspace::empty(ServerId::new(new_id("server"))),
        )
    }

    pub fn open(instance: Option<&str>) -> Result<(Self, StoredWorkspace)> {
        let directory = compi_protocol::paths::data_dir()?;
        let suffix = instance
            .map(|instance| format!("-{instance}"))
            .unwrap_or_default();
        let path = directory.join(format!("workspace{suffix}-v1.json"));
        let legacy = [
            directory.join(format!("sessions{suffix}-v2.json")),
            directory.join(format!("sessions{suffix}-v1.json")),
        ];
        Self::open_path(path, &legacy)
    }

    pub(crate) fn open_path(
        path: PathBuf,
        legacy_paths: &[PathBuf],
    ) -> Result<(Self, StoredWorkspace)> {
        let store = Self {
            path: Some(path.clone()),
        };
        if path.is_file() {
            let bytes = fs::read(&path)?;
            let manifest =
                serde_json::from_slice::<serde_json::Value>(&bytes).and_then(|mut value| {
                    if value.get("format_version") == Some(&serde_json::Value::from(1)) {
                        migrate_content_tabs(&mut value);
                        value["format_version"] = 2.into();
                    }
                    if value.get("format_version") == Some(&serde_json::Value::from(2)) {
                        migrate_diff_tabs(&mut value);
                        value["format_version"] = 3.into();
                    }
                    if value.get("format_version") == Some(&serde_json::Value::from(3)) {
                        migrate_content_kinds(&mut value);
                        value["format_version"] = 4.into();
                    }
                    if value.get("format_version") == Some(&serde_json::Value::from(4)) {
                        value["format_version"] = 5.into();
                    }
                    if value.get("format_version") == Some(&serde_json::Value::from(5)) {
                        value["format_version"] = 6.into();
                    }
                    if value.get("format_version") == Some(&serde_json::Value::from(6)) {
                        value["format_version"] = 7.into();
                    }
                    if value.get("format_version") == Some(&serde_json::Value::from(7)) {
                        restore_monitors(&mut value);
                        restore_commit_diffs(&mut value);
                    }
                    serde_json::from_value::<Manifest>(value)
                });
            return match manifest {
                Ok(manifest) if manifest.format_version == FORMAT_VERSION => {
                    validate(&manifest.workspace)?;
                    let mut workspace = manifest.workspace;
                    // A recovery message reports what one daemon start found. It is shown
                    // during that run and must not reappear on every later start.
                    workspace.recovery_message = None;
                    recover_lost_surfaces(&mut workspace);
                    store.commit(&workspace)?;
                    Ok((store, workspace))
                }
                Ok(manifest) => Err(format!(
                    "workspace format {} is incompatible with supported format {FORMAT_VERSION}; preserve the file and upgrade Compi",
                    manifest.format_version
                )
                .into()),
                Err(error) => {
                    let quarantine = quarantine(&path)?;
                    let mut workspace = StoredWorkspace::empty(ServerId::new(new_id("server")));
                    workspace.recovery_message = Some(format!(
                        "Malformed workspace metadata was quarantined at {}: {error}",
                        quarantine.display()
                    ));
                    store.commit(&workspace)?;
                    Ok((store, workspace))
                }
            };
        }

        for legacy_path in legacy_paths {
            if !legacy_path.is_file() {
                continue;
            }
            let bytes = fs::read(legacy_path)?;
            let legacy: LegacyManifest = serde_json::from_slice(&bytes).map_err(|error| {
                format!(
                    "legacy session metadata {} is malformed and was not changed: {error}",
                    legacy_path.display()
                )
            })?;
            if !matches!(legacy.format_version, 1 | 2) {
                return Err(format!(
                    "legacy session format {} is incompatible and was not changed",
                    legacy.format_version
                )
                .into());
            }
            let backup = backup_path(legacy_path);
            copy_durable(legacy_path, &backup)?;
            let workspace = migrate_legacy(legacy, &backup);
            validate(&workspace)?;
            store.commit(&workspace)?;
            return Ok((store, workspace));
        }

        let workspace = StoredWorkspace::empty(ServerId::new(new_id("server")));
        store.commit(&workspace)?;
        Ok((store, workspace))
    }

    pub fn commit(&self, workspace: &StoredWorkspace) -> Result<()> {
        validate(workspace)?;
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        let mut manifest = serde_json::to_value(Manifest {
            format_version: FORMAT_VERSION,
            workspace: workspace.clone(),
        })?;
        readable_by_older_formats(&mut manifest);
        let bytes = serde_json::to_vec(&manifest)?;
        let temporary = path.with_extension("json.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    }
}

impl StoredWorkspace {
    pub fn empty(server_id: ServerId) -> Self {
        Self {
            pending_removals: Vec::new(),
            server_id,
            revision: 0,
            initialized: false,
            sessions: Vec::new(),
            surfaces: Vec::new(),
            receipts: Vec::new(),
            recovery_message: None,
            last_directories: BTreeMap::new(),
        }
    }

    pub fn snapshot(&self, generation: ServerGeneration) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            server_id: self.server_id.clone(),
            server_generation: generation,
            revision: self.revision,
            initialized: self.initialized,
            sessions: self.sessions.clone(),
            surfaces: self.surfaces.clone(),
            recovery_message: self.recovery_message.clone(),
        }
    }
}

fn migrate_legacy(mut legacy: LegacyManifest, backup: &Path) -> StoredWorkspace {
    legacy
        .sessions
        .sort_by_key(|session| (session.created_at_ms, session.id.clone()));
    let mut tabs = Vec::with_capacity(legacy.sessions.len());
    let mut surfaces = Vec::with_capacity(legacy.sessions.len());
    for session in legacy.sessions {
        let surface_id = SurfaceId::new(format!("surface-imported-{}", session.id));
        let status = match session.status {
            LegacyStatus::Starting | LegacyStatus::Running | LegacyStatus::Dead => {
                SurfaceStatus::Lost
            }
            LegacyStatus::Exited => SurfaceStatus::Exited,
            LegacyStatus::Failed => SurfaceStatus::Failed,
        };
        let error = if status == SurfaceStatus::Lost {
            Some(
                session
                    .error
                    .unwrap_or_else(|| "This shell ended when Compi restarted.".into()),
            )
        } else {
            session.error
        };
        tabs.push(WorkspaceTab {
            id: TabId::new(format!("tab-imported-{}", session.id)),
            label: format!("Shell {}", short_id(&session.id)),
            layout: LayoutNode::terminal(
                PaneId::new(format!("pane-imported-{}", session.id)),
                surface_id.clone(),
            ),
            previous_layout: None,
            merge: None,
        });
        surfaces.push(SurfaceInfo {
            id: surface_id,
            process_lifetime_id: ProcessLifetimeId::new(format!(
                "lifetime-imported-{}",
                session.id
            )),
            status,
            attached: false,
            cols: session.cols,
            rows: session.rows,
            created_at_ms: session.created_at_ms,
            exit_code: session.exit_code,
            error,
            launch: LaunchRequest {
                working_directory: session
                    .working_directory
                    .as_ref()
                    .map(|directory| directory.requested.clone()),
                profile: None,
            },
            working_directory: session.working_directory,
            distribution: None,
        });
    }
    let initialized = !tabs.is_empty();
    StoredWorkspace {
        server_id: ServerId::new(new_id("server")),
        revision: u64::from(initialized),
        initialized,
        pending_removals: Vec::new(),
        sessions: initialized
            .then(|| WorkspaceSession {
                id: SessionId::new("session-imported"),
                label: "Imported work".into(),
                tabs,
            })
            .into_iter()
            .collect(),
        surfaces,
        receipts: Vec::new(),
        recovery_message: Some(format!(
            "Imported legacy terminal metadata; backup retained at {}",
            backup.display()
        )),
        last_directories: BTreeMap::new(),
    }
}

/// Format 1 (protocol 22 and earlier) gave a content pane `files`, an `active_file` and a
/// `view`. Files become file tabs and the active file the active tab; a Changes view, whose
/// root is its repository, becomes that folder with nothing open. Every layout copy (current,
/// previous, merge records) holds content leaves, so the whole workspace is walked.
fn migrate_content_tabs(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(migrate_content_tabs),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("content")
                && let Some(Value::Object(content)) = object.get_mut("content")
            {
                let tab = |path: Value| serde_json::json!({ "type": "file", "path": path });
                content.remove("view");
                if let Some(Value::Array(files)) = content.remove("files") {
                    let tabs = files.into_iter().map(tab).collect();
                    content.insert("tabs".into(), Value::Array(tabs));
                }
                if let Some(active @ Value::String(_)) = content.remove("active_file") {
                    content.insert("active".into(), tab(active));
                }
                return;
            }
            object.values_mut().for_each(migrate_content_tabs);
        }
        _ => {}
    }
}

/// Format 2 gave a changed file's diff its own tab: `{ type: "diff", repo, path, scope }`
/// with `path` repository-relative. Each becomes its file's tab (`repo/path`) in Diff mode,
/// merged with that file's tab when one is open: the merged tab keeps the first one's place
/// and the active one's mode, else Edit. Every layout copy is walked, as in format 1.
fn migrate_diff_tabs(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(migrate_diff_tabs),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("content")
                && let Some(Value::Object(content)) = object.get_mut("content")
            {
                let active = content.remove("active").and_then(|tab| file_tab(&tab));
                let mut merged: Vec<(String, Value)> = Vec::new();
                if let Some(Value::Array(tabs)) = content.remove("tabs") {
                    for tab in tabs.iter().filter_map(file_tab) {
                        let key = same_file_key(&tab);
                        let is_active = active.as_ref() == Some(&tab);
                        match merged.iter_mut().find(|(open, _)| *open == key) {
                            Some((_, open)) => {
                                let open_is_active = active.as_ref() == Some(&*open);
                                let edit = tab.get("diff").is_none();
                                if is_active || (!open_is_active && edit) {
                                    *open = tab;
                                }
                            }
                            None => merged.push((key, tab)),
                        }
                    }
                }
                let active = active.and_then(|active| {
                    let key = same_file_key(&active);
                    merged
                        .iter()
                        .find(|(open, _)| *open == key)
                        .map(|(_, tab)| tab.clone())
                });
                if !merged.is_empty() {
                    let tabs = merged.into_iter().map(|(_, tab)| tab).collect();
                    content.insert("tabs".into(), Value::Array(tabs));
                }
                if let Some(active) = active {
                    content.insert("active".into(), active);
                }
                return;
            }
            object.values_mut().for_each(migrate_diff_tabs);
        }
        _ => {}
    }
}

/// A format-2 tab as a format-3 file tab; anything else is dropped.
fn file_tab(tab: &serde_json::Value) -> Option<serde_json::Value> {
    let text = |key: &str| tab.get(key).and_then(serde_json::Value::as_str);
    match text("type")? {
        "file" => Some(serde_json::json!({ "type": "file", "path": text("path")? })),
        "diff" => {
            let repo = text("repo")?.trim_end_matches('/');
            Some(serde_json::json!({
                "type": "file",
                "path": format!("{repo}/{}", text("path")?),
                "diff": text("scope")?,
            }))
        }
        _ => None,
    }
}

/// Which file a tab is, matching a Windows path whichever separator either side used.
fn same_file_key(tab: &serde_json::Value) -> String {
    tab["path"].as_str().unwrap_or_default().replace('\\', "/")
}

/// Format 3 content leaves were all files and carried no `kind`; each becomes a files leaf.
/// Every layout copy is walked, as in format 1.
fn migrate_content_kinds(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(migrate_content_kinds),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("content")
                && let Some(Value::Object(content)) = object.get_mut("content")
            {
                content
                    .entry("kind")
                    .or_insert_with(|| Value::from("files"));
                return;
            }
            object.values_mut().for_each(migrate_content_kinds);
        }
        _ => {}
    }
}

/// Older daemons parse the whole file before their format check, and a newer shape they
/// fail to parse would quarantine the file, dropping every pane. So newer shapes are
/// written in older ones (`mark_monitors`, `hide_commit_diffs`), which lets those daemons
/// parse far enough to refuse the format and keep the file. Loading undoes both.
fn readable_by_older_formats(value: &mut serde_json::Value) {
    mark_monitors(value);
    hide_commit_diffs(value);
}

/// A format-3 daemon (protocol 26) reads every content leaf as a folder: it needs an
/// `environment` and a `root` and ignores other keys. A format-4 daemon (protocol 27) knows
/// only the `files` and `settings` kinds. So a Monitor leaf is written as a Settings leaf
/// marked `shows: monitor`, and Settings leaves get placeholder `environment` and `root`.
/// Loading ignores the placeholders and `restore_monitors` undoes the marking.
fn mark_monitors(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(mark_monitors),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("content")
                && let Some(Value::Object(content)) = object.get_mut("content")
            {
                if content.get("kind").and_then(Value::as_str) == Some("monitor") {
                    content.insert("kind".into(), Value::from("settings"));
                    content.insert("shows".into(), Value::from("monitor"));
                    content.insert("section".into(), Value::from("appearance"));
                }
                if content.get("kind").and_then(Value::as_str) == Some("settings") {
                    content
                        .entry("environment")
                        .or_insert_with(|| serde_json::json!({ "type": "host" }));
                    content.entry("root").or_insert_with(|| Value::from("/"));
                }
                return;
            }
            object.values_mut().for_each(mark_monitors);
        }
        _ => {}
    }
}

/// Undoes `mark_monitors`, keeping a Monitor leaf's own fields (the page). Every layout copy
/// is walked.
fn restore_monitors(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(restore_monitors),
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("content")
                && let Some(Value::Object(content)) = object.get_mut("content")
            {
                if content.get("shows").and_then(Value::as_str) == Some("monitor") {
                    for placeholder in ["shows", "section", "environment", "root"] {
                        content.remove(placeholder);
                    }
                    content.insert("kind".into(), Value::from("monitor"));
                }
                return;
            }
            object.values_mut().for_each(restore_monitors);
        }
        _ => {}
    }
}

/// A format-6 daemon (protocol 29) knows only the unstaged and staged diff modes. So a
/// commit diff tab, `diff: { commit: id }`, is written as its file's Edit tab carrying the id
/// as `diff_commit`, a key that daemon ignores; `restore_commit_diffs` puts it back. Every
/// files content is walked: content leaves and terminal views, in every layout copy.
fn hide_commit_diffs(value: &mut serde_json::Value) {
    walk_tabs(value, &mut |tab| {
        if let Some(id) = tab.get("diff").and_then(|diff| diff.get("commit")).cloned() {
            tab.remove("diff");
            tab.insert("diff_commit".into(), id);
        }
    });
}

/// Undoes `hide_commit_diffs`.
fn restore_commit_diffs(value: &mut serde_json::Value) {
    walk_tabs(value, &mut |tab| {
        if let Some(id) = tab.remove("diff_commit") {
            tab.insert("diff".into(), serde_json::json!({ "commit": id }));
        }
    });
}

/// Calls `visit` on every tab, open or active, of every files content within `value`.
fn walk_tabs(
    value: &mut serde_json::Value,
    visit: &mut impl FnMut(&mut serde_json::Map<String, serde_json::Value>),
) {
    use serde_json::Value;
    match value {
        Value::Array(items) => items.iter_mut().for_each(|item| walk_tabs(item, visit)),
        Value::Object(object) => {
            if object.get("kind").and_then(Value::as_str) == Some("files") {
                let tabs = object.get_mut("tabs").and_then(Value::as_array_mut);
                for tab in tabs.into_iter().flatten() {
                    if let Value::Object(tab) = tab {
                        visit(tab);
                    }
                }
                if let Some(Value::Object(active)) = object.get_mut("active") {
                    visit(active);
                }
                return;
            }
            object.values_mut().for_each(|item| walk_tabs(item, visit));
        }
        _ => {}
    }
}

fn recover_lost_surfaces(workspace: &mut StoredWorkspace) {
    let mut changed = false;
    for surface in &mut workspace.surfaces {
        if matches!(
            surface.status,
            SurfaceStatus::Starting | SurfaceStatus::Running | SurfaceStatus::Ending
        ) {
            surface.status = SurfaceStatus::Lost;
            surface.attached = false;
            surface.exit_code = None;
            surface.error = Some("This shell ended when Compi restarted.".into());
            changed = true;
        }
    }
    if !workspace.pending_removals.is_empty() {
        // The removal never committed, so its tab, pane or workspace is still present.
        workspace.pending_removals.clear();
        workspace.recovery_message = Some(
            "Compi restarted before a close finished, so the tab was kept. Close it again if you no longer need it."
                .into(),
        );
        changed = true;
    }
    if changed {
        workspace.revision = workspace.revision.saturating_add(1);
    }
}

pub(crate) fn validate(workspace: &StoredWorkspace) -> Result<()> {
    use std::collections::HashSet;
    if workspace.receipts.len() > compi_protocol::MAX_MUTATION_RECEIPTS {
        return Err("workspace mutation receipt limit exceeded".into());
    }
    let surface_ids: HashSet<_> = workspace
        .surfaces
        .iter()
        .map(|surface| surface.id.clone())
        .collect();
    if surface_ids.len() != workspace.surfaces.len() {
        return Err("workspace contains duplicate surface IDs".into());
    }
    let mut session_ids = HashSet::new();
    let mut tab_ids = HashSet::new();
    let mut pane_ids = HashSet::new();
    let mut merged_ids = HashSet::new();
    for session in &workspace.sessions {
        if !session_ids.insert(session.id.clone()) {
            return Err("workspace contains duplicate session IDs".into());
        }
        for tab in &session.tabs {
            if !tab_ids.insert(tab.id.clone()) {
                return Err("workspace contains duplicate tab IDs".into());
            }
            let mut tab_panes = HashSet::new();
            validate_layout(&tab.layout, &surface_ids, &mut tab_panes)?;
            if let Some(previous) = &tab.previous_layout {
                validate_previous_layout(previous, &tab.layout, &mut HashSet::new())?;
            }
            if let Some(merge) = &tab.merge {
                if merge.tabs.is_empty() {
                    return Err("a merge record must name at least one merged tab".into());
                }
                // Each pane belongs to at most one recreated tab or the receiver.
                let mut recorded = HashSet::new();
                if let Some(own) = &merge.own_layout {
                    validate_previous_layout(own, &tab.layout, &mut recorded)?;
                }
                for merged in &merge.tabs {
                    if !merged_ids.insert(merged.id.clone()) {
                        return Err("workspace records a merged tab ID twice".into());
                    }
                    validate_previous_layout(&merged.layout, &tab.layout, &mut recorded)?;
                }
            }
            for pane in tab_panes {
                if !pane_ids.insert(pane) {
                    return Err("workspace contains duplicate pane IDs".into());
                }
            }
        }
    }
    if merged_ids.iter().any(|id| tab_ids.contains(id)) {
        return Err("a merged tab record reuses a live tab ID".into());
    }
    Ok(())
}

fn validate_layout(
    node: &LayoutNode,
    surfaces: &std::collections::HashSet<SurfaceId>,
    panes: &mut std::collections::HashSet<PaneId>,
) -> Result<()> {
    match node {
        LayoutNode::Pane {
            pane_id,
            surface_id,
            view,
        } => {
            if !panes.insert(pane_id.clone()) {
                return Err("workspace contains duplicate pane IDs".into());
            }
            if !surfaces.contains(surface_id) {
                return Err(
                    format!("pane {pane_id} references missing surface {surface_id}").into(),
                );
            }
            // Only shape checks, as for content leaves.
            if let Some(view) = view {
                view.content
                    .validate()
                    .map_err(|error| format!("view of pane {pane_id}: {error}"))?;
            }
        }
        // Only shape checks: whether paths are absolute depends on the host, and a
        // stored workspace must load wherever it was saved.
        LayoutNode::Content { pane_id, content } => {
            if !panes.insert(pane_id.clone()) {
                return Err("workspace contains duplicate pane IDs".into());
            }
            content
                .validate()
                .map_err(|error| format!("content pane {pane_id}: {error}"))?;
        }
        LayoutNode::Split {
            ratio,
            first,
            second,
            ..
        } => {
            if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                return Err("split ratio must be finite and strictly between zero and one".into());
            }
            validate_layout(first, surfaces, panes)?;
            validate_layout(second, surfaces, panes)?;
        }
    }
    Ok(())
}

/// A restorable arrangement may only reference this tab's current leaves, each once.
fn validate_previous_layout(
    node: &LayoutNode,
    current: &LayoutNode,
    panes: &mut std::collections::HashSet<PaneId>,
) -> Result<()> {
    match node {
        LayoutNode::Pane { pane_id, .. } | LayoutNode::Content { pane_id, .. } => {
            if !panes.insert(pane_id.clone()) || !contains_leaf(current, node) {
                return Err(format!(
                    "previous arrangement references pane {pane_id} outside its tab"
                )
                .into());
            }
        }
        LayoutNode::Split {
            ratio,
            first,
            second,
            ..
        } => {
            if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                return Err("split ratio must be finite and strictly between zero and one".into());
            }
            validate_previous_layout(first, current, panes)?;
            validate_previous_layout(second, current, panes)?;
        }
    }
    Ok(())
}

/// Whether `current` holds `leaf` unchanged: the same pane with the same surface or
/// content.
fn contains_leaf(current: &LayoutNode, leaf: &LayoutNode) -> bool {
    match current {
        LayoutNode::Split { first, second, .. } => {
            contains_leaf(first, leaf) || contains_leaf(second, leaf)
        }
        current => current == leaf,
    }
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.migration-backup",
        path.file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("sessions.json")
    ))
}

fn copy_durable(source: &Path, destination: &Path) -> Result<()> {
    let bytes = fs::read(source)?;
    if destination.is_file() {
        if fs::read(destination)? == bytes {
            return Ok(());
        }
        return Err(format!(
            "migration backup {} already exists with different content",
            destination.display()
        )
        .into());
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn quarantine(path: &Path) -> Result<PathBuf> {
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("workspace");
    for ordinal in 0..100 {
        let quarantine = path.with_file_name(format!("{stem}.corrupt-{}-{ordinal}.json", now_ms()));
        if !quarantine.exists() {
            fs::rename(path, &quarantine)?;
            return Ok(quarantine);
        }
    }
    Err("could not allocate a quarantine name for malformed workspace metadata".into())
}

#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(once(0)).collect()
}

#[cfg(windows)]
fn replace_file(temporary: &Path, path: &Path) -> Result<()> {
    let temporary = wide(temporary);
    let destination = wide(path);
    unsafe {
        MoveFileExW(
            PCWSTR(temporary.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn replace_file(temporary: &Path, path: &Path) -> Result<()> {
    fs::rename(temporary, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}-{:x}-{:x}", now_ms(), std::process::id())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn short_id(id: &str) -> String {
    id.rsplit('-')
        .next()
        .unwrap_or(id)
        .chars()
        .take(8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::system_metrics::MonitorSource;
    use compi_protocol::{
        ContentEnvironment, ContentTab, FolderContent, GitScope, MonitorContent, MonitorPage,
        PaneContent, PaneSide, PaneView, SettingsContent, SettingsSection, SplitAxis,
    };

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "compi-workspace-store-{name}-{}-{}.json",
            std::process::id(),
            now_ms()
        ))
    }

    /// The folder a files content leaf shows.
    fn folder(node: &LayoutNode) -> &FolderContent {
        match node {
            LayoutNode::Content {
                content: PaneContent::Files(folder),
                ..
            } => folder,
            other => panic!("expected a files content pane, found {other:?}"),
        }
    }

    #[test]
    fn running_surfaces_become_lost_after_reopen() {
        let path = test_path("lost");
        let (store, mut workspace) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        workspace.initialized = true;
        workspace.surfaces.push(SurfaceInfo {
            id: SurfaceId::new("surface-1"),
            process_lifetime_id: ProcessLifetimeId::new("lifetime-1"),
            status: SurfaceStatus::Running,
            attached: true,
            cols: 80,
            rows: 24,
            created_at_ms: 1,
            exit_code: None,
            error: None,
            launch: LaunchRequest {
                working_directory: None,
                profile: None,
            },
            working_directory: None,
            distribution: None,
        });
        workspace.sessions.push(WorkspaceSession {
            id: SessionId::new("session-1"),
            label: "Default".into(),
            tabs: vec![WorkspaceTab {
                id: TabId::new("tab-1"),
                label: "Shell".into(),
                layout: LayoutNode::terminal(PaneId::new("pane-1"), SurfaceId::new("surface-1")),
                previous_layout: None,
                merge: None,
            }],
        });
        store.commit(&workspace).unwrap();
        let (_, reopened) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(reopened.surfaces[0].status, SurfaceStatus::Lost);
        assert!(!reopened.surfaces[0].attached);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn recovery_message_shows_for_one_daemon_start_only() {
        let path = test_path("recovery-once");
        let (store, mut workspace) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        workspace.recovery_message = Some("from an earlier start".into());
        workspace.pending_removals.push(PendingRemoval {
            target: RemovalTarget::Tab {
                tab_id: TabId::new("tab-1"),
            },
            surfaces: Vec::new(),
        });
        store.commit(&workspace).unwrap();
        let (_, interrupted) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        let message = interrupted
            .recovery_message
            .expect("the interruption is reported");
        assert_ne!(message, "from an earlier start");
        assert!(interrupted.pending_removals.is_empty());
        let (_, later) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(later.recovery_message, None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_metadata_is_backed_up_and_imported() {
        let path = test_path("migration");
        let legacy_path = test_path("legacy");
        fs::write(
            &legacy_path,
            br#"{"format_version":2,"sessions":[{"id":"s-1-1","created_at_ms":1,"updated_at_ms":1,"status":"running","cols":80,"rows":24,"exit_code":null,"error":null,"working_directory":null}]}"#,
        )
        .unwrap();
        let (_, workspace) =
            WorkspaceStore::open_path(path.clone(), std::slice::from_ref(&legacy_path)).unwrap();
        assert!(workspace.initialized);
        assert_eq!(workspace.sessions[0].tabs.len(), 1);
        assert_eq!(workspace.surfaces[0].status, SurfaceStatus::Lost);
        assert!(backup_path(&legacy_path).is_file());
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(&legacy_path);
        let _ = fs::remove_file(backup_path(&legacy_path));
    }

    #[test]
    fn interrupted_migration_reuses_an_identical_backup() {
        let path = test_path("migration-resume");
        let legacy_path = test_path("legacy-resume");
        let legacy = br#"{"format_version":2,"sessions":[]}"#;
        fs::write(&legacy_path, legacy).unwrap();
        fs::write(backup_path(&legacy_path), legacy).unwrap();

        let (_, workspace) =
            WorkspaceStore::open_path(path.clone(), std::slice::from_ref(&legacy_path)).unwrap();

        assert!(!workspace.initialized);
        assert!(path.is_file());
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(&legacy_path);
        let _ = fs::remove_file(backup_path(&legacy_path));
    }

    #[test]
    fn malformed_workspace_is_quarantined_with_visible_recovery() {
        let path = test_path("malformed");
        fs::write(&path, b"{not-json").unwrap();
        let (_, workspace) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert!(workspace.recovery_message.is_some());
        assert!(path.is_file());
        let parent = path.parent().unwrap();
        let stem = path.file_stem().unwrap().to_string_lossy();
        let quarantined = fs::read_dir(parent)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|candidate| {
                candidate.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(&format!("{stem}.corrupt-"))
                })
            })
            .unwrap();
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(quarantined);
    }

    #[test]
    fn invalid_split_ratio_is_rejected() {
        let (_, mut workspace) = WorkspaceStore::memory();
        workspace.surfaces.push(SurfaceInfo {
            id: SurfaceId::new("surface-1"),
            process_lifetime_id: ProcessLifetimeId::new("lifetime-1"),
            status: SurfaceStatus::Lost,
            attached: false,
            cols: 80,
            rows: 24,
            created_at_ms: 1,
            exit_code: None,
            error: None,
            launch: LaunchRequest {
                working_directory: None,
                profile: None,
            },
            working_directory: None,
            distribution: None,
        });
        workspace.sessions.push(WorkspaceSession {
            id: SessionId::new("session-1"),
            label: "Default".into(),
            tabs: vec![WorkspaceTab {
                id: TabId::new("tab-1"),
                label: "Shell".into(),
                layout: LayoutNode::Split {
                    axis: compi_protocol::SplitAxis::Horizontal,
                    ratio: f32::NAN,
                    first: Box::new(LayoutNode::terminal(
                        PaneId::new("pane-1"),
                        SurfaceId::new("surface-1"),
                    )),
                    second: Box::new(LayoutNode::terminal(
                        PaneId::new("pane-2"),
                        SurfaceId::new("surface-1"),
                    )),
                },
                previous_layout: None,
                merge: None,
            }],
        });
        assert!(validate(&workspace).is_err());
    }

    /// A tab with a terminal beside a content pane whose previous arrangement swaps them.
    fn content_workspace(status: SurfaceStatus) -> (StoredWorkspace, LayoutNode) {
        let (_, mut workspace) = WorkspaceStore::memory();
        workspace.initialized = true;
        workspace.surfaces.push(SurfaceInfo {
            id: SurfaceId::new("surface-1"),
            process_lifetime_id: ProcessLifetimeId::new("lifetime-1"),
            status,
            attached: false,
            cols: 80,
            rows: 24,
            created_at_ms: 1,
            exit_code: None,
            error: None,
            launch: LaunchRequest {
                working_directory: None,
                profile: None,
            },
            working_directory: None,
            distribution: None,
        });
        // WSL paths load on every host: loading applies no host path rules.
        let content = LayoutNode::Content {
            pane_id: PaneId::new("pane-2"),
            content: PaneContent::Files(FolderContent {
                environment: ContentEnvironment::Wsl {
                    distribution: "Ubuntu".into(),
                },
                root: "/home/user/project".into(),
                tabs: vec![ContentTab::edit("/home/user/project/a.rs")],
                active: Some(ContentTab::edit("/home/user/project/a.rs")),
            }),
        };
        let terminal = LayoutNode::terminal(PaneId::new("pane-1"), SurfaceId::new("surface-1"));
        let split = |first: &LayoutNode, second: &LayoutNode| LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(first.clone()),
            second: Box::new(second.clone()),
        };
        workspace.sessions.push(WorkspaceSession {
            id: SessionId::new("session-1"),
            label: "Default".into(),
            tabs: vec![WorkspaceTab {
                id: TabId::new("tab-1"),
                label: String::new(),
                layout: split(&terminal, &content),
                previous_layout: Some(Box::new(split(&content, &terminal))),
                merge: None,
            }],
        });
        (workspace, content)
    }

    #[test]
    fn content_panes_survive_reopen_while_terminals_become_lost() {
        let path = test_path("content");
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        let (workspace, _) = content_workspace(SurfaceStatus::Running);
        store.commit(&workspace).unwrap();
        let (_, reopened) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(reopened.surfaces[0].status, SurfaceStatus::Lost);
        assert_eq!(reopened.sessions, workspace.sessions);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn content_leaves_must_be_well_formed_and_match_their_history() {
        let (mut workspace, content) = content_workspace(SurfaceStatus::Lost);
        validate(&workspace).unwrap();
        let LayoutNode::Content {
            pane_id,
            content: PaneContent::Files(shown),
        } = content
        else {
            unreachable!()
        };
        let tab = &mut workspace.sessions[0].tabs[0];
        let Some(LayoutNode::Split { first, .. }) = tab.previous_layout.as_deref_mut() else {
            unreachable!()
        };
        **first = LayoutNode::Content {
            pane_id: pane_id.clone(),
            content: PaneContent::Files(FolderContent {
                tabs: Vec::new(),
                active: None,
                ..shown.clone()
            }),
        };
        assert!(validate(&workspace).is_err());

        let tab = &mut workspace.sessions[0].tabs[0];
        tab.previous_layout = None;
        let LayoutNode::Split { second, .. } = &mut tab.layout else {
            unreachable!()
        };
        **second = LayoutNode::Content {
            pane_id,
            content: PaneContent::Files(FolderContent {
                active: Some(ContentTab::edit("/home/user/project/missing.rs")),
                ..shown
            }),
        };
        assert!(validate(&workspace).is_err());
    }

    #[test]
    fn protocol_22_content_panes_load_as_tabs() {
        let path = test_path("format-1");
        let leaf = |pane: &str, content: serde_json::Value| serde_json::json!({ "type": "content", "pane_id": pane, "content": content });
        let editor = leaf(
            "pane-1",
            serde_json::json!({
                "environment": { "type": "host" },
                "root": "/work",
                "files": ["/work/a.rs", "/work/b.rs"],
                "active_file": "/work/b.rs",
            }),
        );
        let changes = leaf(
            "pane-2",
            serde_json::json!({
                "environment": { "type": "wsl", "distribution": "Ubuntu" },
                "root": "/home/user/repo",
                "view": { "type": "changes", "selected": { "path": "src/a.rs", "scope": "staged" } },
            }),
        );
        let split = |first: &serde_json::Value, second: &serde_json::Value| {
            serde_json::json!({
                "type": "split", "axis": "horizontal", "ratio": 0.5,
                "first": first, "second": second,
            })
        };
        let (_, mut empty) = WorkspaceStore::memory();
        empty.initialized = true;
        let mut manifest = serde_json::json!({ "format_version": 1, "workspace": empty });
        manifest["workspace"]["sessions"] = serde_json::json!([{
            "id": "session-1",
            "label": "Default",
            "tabs": [{
                "id": "tab-1",
                "label": "",
                "layout": split(&editor, &changes),
                "previous_layout": split(&changes, &editor),
            }],
        }]);
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let (_, workspace) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        let file = |path: &str| ContentTab::edit(path);
        let tab = &workspace.sessions[0].tabs[0];
        let LayoutNode::Split { first, second, .. } = &tab.layout else {
            panic!("the split survives");
        };
        let content = folder(first);
        assert_eq!(content.tabs, [file("/work/a.rs"), file("/work/b.rs")]);
        assert_eq!(content.active, Some(file("/work/b.rs")));
        let content = folder(second);
        assert_eq!(content.root, "/home/user/repo");
        assert!(content.tabs.is_empty() && content.active.is_none());
        let Some(LayoutNode::Split { second, .. }) = tab.previous_layout.as_deref() else {
            panic!("the previous arrangement survives");
        };
        assert_eq!(folder(second).tabs.len(), 2);
        // Saved again in the current format.
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["format_version"], FORMAT_VERSION);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn protocol_24_diff_tabs_become_their_files_tab_in_diff_mode() {
        let path = test_path("format-2");
        let leaf = |pane: &str, content: serde_json::Value| serde_json::json!({ "type": "content", "pane_id": pane, "content": content });
        let diff = |path: &str, scope: &str| serde_json::json!({ "type": "diff", "repo": "/work/", "path": path, "scope": scope });
        let file = |path: &str| serde_json::json!({ "type": "file", "path": path });
        // A diff beside its file's tab, active: the merged tab keeps the file's place in Diff mode.
        let merged = leaf(
            "pane-1",
            serde_json::json!({
                "environment": { "type": "host" },
                "root": "/work",
                "tabs": [file("/work/a.rs"), file("/work/b.rs"), diff("a.rs", "unstaged"), diff("c.rs", "staged")],
                "active": diff("a.rs", "unstaged"),
            }),
        );
        // Both scopes of one file and its file tab, none active: one tab, in Edit mode, where
        // the first of them was.
        let edited = leaf(
            "pane-2",
            serde_json::json!({
                "environment": { "type": "host" },
                "root": "/work",
                "tabs": [diff("a.rs", "staged"), file("/work/b.rs"), diff("a.rs", "unstaged"), file("/work/a.rs")],
                "active": file("/work/b.rs"),
            }),
        );
        let split = serde_json::json!({
            "type": "split", "axis": "horizontal", "ratio": 0.5,
            "first": merged, "second": edited,
        });
        let (_, mut empty) = WorkspaceStore::memory();
        empty.initialized = true;
        let mut manifest = serde_json::json!({ "format_version": 2, "workspace": empty });
        manifest["workspace"]["sessions"] = serde_json::json!([{
            "id": "session-1",
            "label": "Default",
            "tabs": [{ "id": "tab-1", "label": "", "layout": split, "previous_layout": merged }],
        }]);
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let (_, workspace) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        let edit = |path: &str| ContentTab::edit(path);
        let diff = |path: &str, scope| ContentTab::diff(path, scope);
        let tab = &workspace.sessions[0].tabs[0];
        let LayoutNode::Split { first, second, .. } = &tab.layout else {
            panic!("the split survives");
        };
        let content = folder(first);
        let a = diff("/work/a.rs", GitScope::Unstaged);
        assert_eq!(
            content.tabs,
            [
                a.clone(),
                edit("/work/b.rs"),
                diff("/work/c.rs", GitScope::Staged)
            ]
        );
        assert_eq!(content.active, Some(a));
        let content = folder(second);
        assert_eq!(content.tabs, [edit("/work/a.rs"), edit("/work/b.rs")]);
        assert_eq!(content.active, Some(edit("/work/b.rs")));
        assert_eq!(
            folder(tab.previous_layout.as_deref().unwrap()).tabs.len(),
            3
        );
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["format_version"], FORMAT_VERSION);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn protocol_26_content_leaves_load_as_files() {
        let path = test_path("format-3");
        let (expected, _) = content_workspace(SurfaceStatus::Lost);
        let file = serde_json::json!({ "type": "file", "path": "/home/user/project/a.rs" });
        let content = serde_json::json!({
            "type": "content",
            "pane_id": "pane-2",
            "content": {
                "environment": { "type": "wsl", "distribution": "Ubuntu" },
                "root": "/home/user/project",
                "tabs": [file],
                "active": file,
            },
        });
        let terminal =
            serde_json::json!({ "type": "pane", "pane_id": "pane-1", "surface_id": "surface-1" });
        let split = |first: &serde_json::Value, second: &serde_json::Value| {
            serde_json::json!({
                "type": "split", "axis": "horizontal", "ratio": 0.5,
                "first": first, "second": second,
            })
        };
        let mut manifest = serde_json::json!({ "format_version": 3, "workspace": expected });
        let stored = &mut manifest["workspace"]["sessions"][0]["tabs"][0];
        stored["layout"] = split(&terminal, &content);
        stored["previous_layout"] = split(&content, &terminal);
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let (_, workspace) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(workspace.sessions, expected.sessions);
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["format_version"], FORMAT_VERSION);
        let tab = &saved["workspace"]["sessions"][0]["tabs"][0];
        assert_eq!(tab["layout"]["second"]["content"]["kind"], "files");
        assert_eq!(tab["previous_layout"]["first"]["content"]["kind"], "files");
        let _ = fs::remove_file(path);
    }

    /// `content_workspace` with Settings and Monitor panes beside the files pane, in
    /// both arrangements; the Monitor watches a WSL distribution.
    fn content_kinds_workspace() -> StoredWorkspace {
        let (mut workspace, files) = content_workspace(SurfaceStatus::Lost);
        let settings = LayoutNode::Content {
            pane_id: PaneId::new("pane-3"),
            content: PaneContent::Settings(SettingsContent {
                section: SettingsSection::Terminal,
            }),
        };
        let monitor = LayoutNode::Content {
            pane_id: PaneId::new("pane-4"),
            content: PaneContent::Monitor(MonitorContent {
                page: MonitorPage::Processes,
                source: MonitorSource::Wsl {
                    distribution: "Ubuntu".into(),
                },
            }),
        };
        let terminal = LayoutNode::terminal(PaneId::new("pane-1"), SurfaceId::new("surface-1"));
        let split = |first: &LayoutNode, second: &LayoutNode| LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(first.clone()),
            second: Box::new(second.clone()),
        };
        let tab = &mut workspace.sessions[0].tabs[0];
        tab.layout = split(&terminal, &split(&files, &split(&settings, &monitor)));
        tab.previous_layout = Some(Box::new(split(
            &split(&monitor, &split(&settings, &files)),
            &terminal,
        )));
        workspace
    }

    #[test]
    fn settings_and_monitor_panes_survive_reopen() {
        let path = test_path("content-kinds");
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        let workspace = content_kinds_workspace();
        store.commit(&workspace).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let panes = &saved["workspace"]["sessions"][0]["tabs"][0]["layout"]["second"]["second"];
        assert_eq!(panes["first"]["content"]["kind"], "settings");
        assert_eq!(panes["first"]["content"]["section"], "terminal");
        // Monitor is stored as a marked Settings leaf for older daemons, and the
        // placeholders written for them are ignored when loading.
        assert_eq!(panes["second"]["content"]["kind"], "settings");
        assert_eq!(panes["second"]["content"]["shows"], "monitor");
        assert_eq!(panes["first"]["content"]["root"], "/");
        let (_, reopened) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(reopened.sessions, workspace.sessions);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_format_4_daemon_reaches_its_version_check_on_monitor_panes() {
        // Protocol 27's shapes, as far as a format-4 daemon parses a layout.
        #[derive(Deserialize)]
        struct OldManifest {
            format_version: u32,
            workspace: OldWorkspace,
        }
        #[derive(Deserialize)]
        struct OldWorkspace {
            sessions: Vec<OldSession>,
        }
        #[derive(Deserialize)]
        struct OldSession {
            tabs: Vec<OldTab>,
        }
        #[derive(Deserialize)]
        struct OldTab {
            #[allow(dead_code)]
            layout: OldLayout,
            #[allow(dead_code)]
            previous_layout: Option<OldLayout>,
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldLayout {
            Pane {},
            Content {
                #[allow(dead_code)]
                content: OldContent,
            },
            Split {
                #[allow(dead_code)]
                first: Box<OldLayout>,
                #[allow(dead_code)]
                second: Box<OldLayout>,
            },
        }
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum OldContent {
            Files(#[allow(dead_code)] FolderContent),
            Settings(#[allow(dead_code)] SettingsContent),
        }

        let path = test_path("monitor-downgrade");
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        store.commit(&content_kinds_workspace()).unwrap();
        let old: OldManifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(old.format_version, FORMAT_VERSION);
        assert_eq!(old.workspace.sessions[0].tabs.len(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_format_3_daemon_reaches_its_version_check_on_settings_panes() {
        // Protocol 26's shapes, as far as a format-3 daemon parses a layout.
        #[derive(Deserialize)]
        struct OldManifest {
            format_version: u32,
            workspace: OldWorkspace,
        }
        #[derive(Deserialize)]
        struct OldWorkspace {
            sessions: Vec<OldSession>,
        }
        #[derive(Deserialize)]
        struct OldSession {
            tabs: Vec<OldTab>,
        }
        #[derive(Deserialize)]
        struct OldTab {
            layout: OldLayout,
            previous_layout: Option<OldLayout>,
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldLayout {
            Pane {},
            Content {
                content: OldContent,
            },
            Split {
                first: Box<OldLayout>,
                second: Box<OldLayout>,
            },
        }
        #[derive(Deserialize)]
        struct OldContent {
            #[allow(dead_code)]
            environment: ContentEnvironment,
            root: String,
        }
        /// Each content leaf's root, in order.
        fn roots(node: &OldLayout) -> Vec<&str> {
            match node {
                OldLayout::Pane {} => Vec::new(),
                OldLayout::Content { content } => vec![content.root.as_str()],
                OldLayout::Split { first, second } => [roots(first), roots(second)].concat(),
            }
        }

        let path = test_path("settings-downgrade");
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        store.commit(&content_kinds_workspace()).unwrap();
        let old: OldManifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(old.format_version, FORMAT_VERSION);
        let tab = &old.workspace.sessions[0].tabs[0];
        assert_eq!(roots(&tab.layout), ["/home/user/project", "/", "/"]);
        assert_eq!(
            roots(tab.previous_layout.as_ref().unwrap()),
            ["/", "/", "/home/user/project"]
        );
        let _ = fs::remove_file(path);
    }

    /// `content_kinds_workspace` with the terminal carrying a Monitor view, its shell
    /// shown, in both arrangements.
    fn views_workspace() -> StoredWorkspace {
        let mut workspace = content_kinds_workspace();
        let view = PaneView {
            content: PaneContent::Monitor(MonitorContent {
                page: MonitorPage::Processes,
                source: MonitorSource::Host,
            }),
            side: PaneSide::Terminal,
        };
        let tab = &mut workspace.sessions[0].tabs[0];
        let Some(LayoutNode::Split { second, .. }) = tab.previous_layout.as_deref_mut() else {
            unreachable!()
        };
        let LayoutNode::Split { first, .. } = &mut tab.layout else {
            unreachable!()
        };
        for terminal in [first, second] {
            let LayoutNode::Pane { view: slot, .. } = &mut **terminal else {
                unreachable!()
            };
            *slot = Some(view.clone());
        }
        workspace
    }

    #[test]
    fn a_terminal_view_survives_reopen_and_must_be_well_formed() {
        let path = test_path("views");
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        let workspace = views_workspace();
        store.commit(&workspace).unwrap();
        // Only content leaves are marked for older daemons; a view is stored as it is.
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let view = &saved["workspace"]["sessions"][0]["tabs"][0]["layout"]["first"]["view"];
        assert_eq!(
            *view,
            serde_json::json!({ "content": { "kind": "monitor", "page": "processes" }, "side": "terminal" })
        );
        let (_, reopened) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(reopened.sessions, workspace.sessions);
        let _ = fs::remove_file(path);

        let mut malformed = workspace;
        let tab = &mut malformed.sessions[0].tabs[0];
        tab.previous_layout = None;
        let LayoutNode::Split { first, .. } = &mut tab.layout else {
            unreachable!()
        };
        let LayoutNode::Pane {
            view: Some(view), ..
        } = &mut **first
        else {
            unreachable!()
        };
        view.content = PaneContent::Files(FolderContent {
            environment: ContentEnvironment::Host,
            root: "/project".into(),
            tabs: Vec::new(),
            active: Some(ContentTab::edit("/project/missing.rs")),
        });
        assert!(validate(&malformed).is_err());
    }

    #[test]
    fn format_5_and_6_files_load_as_format_7() {
        let expected = content_kinds_workspace();
        for format in [5, 6] {
            let path = test_path(&format!("format-{format}"));
            // As a format-5 or format-6 daemon wrote it: Monitor leaves marked for older
            // daemons.
            let mut manifest = serde_json::to_value(Manifest {
                format_version: format,
                workspace: expected.clone(),
            })
            .unwrap();
            readable_by_older_formats(&mut manifest);
            fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();

            let (_, workspace) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
            assert_eq!(workspace.sessions, expected.sessions);
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["format_version"], 7);
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn a_format_5_daemon_reaches_its_version_check_on_terminal_views() {
        // Protocol 28's shapes, as far as a format-5 daemon parses a layout.
        #[derive(Deserialize)]
        struct OldManifest {
            format_version: u32,
            workspace: OldWorkspace,
        }
        #[derive(Deserialize)]
        struct OldWorkspace {
            sessions: Vec<OldSession>,
        }
        #[derive(Deserialize)]
        struct OldSession {
            tabs: Vec<OldTab>,
        }
        #[derive(Deserialize)]
        struct OldTab {
            #[allow(dead_code)]
            layout: OldLayout,
            #[allow(dead_code)]
            previous_layout: Option<OldLayout>,
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldLayout {
            Pane {
                #[allow(dead_code)]
                pane_id: PaneId,
                #[allow(dead_code)]
                surface_id: SurfaceId,
            },
            Content {
                #[allow(dead_code)]
                content: PaneContent,
            },
            Split {
                #[allow(dead_code)]
                first: Box<OldLayout>,
                #[allow(dead_code)]
                second: Box<OldLayout>,
            },
        }

        let path = test_path("view-downgrade");
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        store.commit(&views_workspace()).unwrap();
        let old: OldManifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(old.format_version, FORMAT_VERSION);
        assert_eq!(old.workspace.sessions[0].tabs.len(), 1);
        let _ = fs::remove_file(path);
    }

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// `content_kinds_workspace` with the files pane showing a commit diff, and the
    /// terminal carrying a files view that has one open, in both arrangements.
    fn commit_diffs_workspace() -> StoredWorkspace {
        let mut workspace = content_kinds_workspace();
        let diff = ContentTab::diff("/home/user/project/b.rs", GitScope::Commit(COMMIT.into()));
        let files = FolderContent {
            environment: ContentEnvironment::Host,
            root: "/project".into(),
            tabs: vec![
                ContentTab::edit("/project/a.rs"),
                ContentTab::diff("/project/b.rs", GitScope::Commit(COMMIT.into())),
            ],
            active: Some(ContentTab::edit("/project/a.rs")),
        };
        let view = PaneView {
            content: PaneContent::Files(files),
            side: PaneSide::Terminal,
        };
        fn visit(node: &mut LayoutNode, diff: &ContentTab, view: &PaneView) {
            match node {
                LayoutNode::Split { first, second, .. } => {
                    visit(first, diff, view);
                    visit(second, diff, view);
                }
                LayoutNode::Pane { view: slot, .. } => *slot = Some(view.clone()),
                LayoutNode::Content {
                    content: PaneContent::Files(folder),
                    ..
                } => {
                    folder.tabs.push(diff.clone());
                    folder.active = Some(diff.clone());
                }
                LayoutNode::Content { .. } => {}
            }
        }
        let tab = &mut workspace.sessions[0].tabs[0];
        visit(&mut tab.layout, &diff, &view);
        visit(tab.previous_layout.as_mut().unwrap(), &diff, &view);
        workspace
    }

    #[test]
    fn commit_diff_tabs_survive_reopen() {
        let path = test_path("commit-diffs");
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        let workspace = commit_diffs_workspace();
        store.commit(&workspace).unwrap();
        // Stored as Edit tabs carrying the commit, for older daemons.
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let layout = &saved["workspace"]["sessions"][0]["tabs"][0]["layout"];
        let hidden = serde_json::json!({
            "type": "file",
            "path": "/home/user/project/b.rs",
            "diff_commit": COMMIT,
        });
        let files = &layout["second"]["first"]["content"];
        assert_eq!(files["tabs"][1], hidden);
        assert_eq!(files["active"], hidden);
        let view = &layout["first"]["view"]["content"];
        assert_eq!(view["tabs"][1]["diff_commit"], COMMIT);
        assert!(view["tabs"][1].get("diff").is_none());
        let (_, reopened) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(reopened.sessions, workspace.sessions);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_format_6_daemon_reaches_its_version_check_on_commit_diffs() {
        // Protocol 30's shapes, as far as a format-6 daemon parses a layout.
        #[derive(Deserialize)]
        struct OldManifest {
            format_version: u32,
            workspace: OldWorkspace,
        }
        #[derive(Deserialize)]
        struct OldWorkspace {
            sessions: Vec<OldSession>,
        }
        #[derive(Deserialize)]
        struct OldSession {
            tabs: Vec<OldTab>,
        }
        #[derive(Deserialize)]
        struct OldTab {
            layout: OldLayout,
            previous_layout: Option<OldLayout>,
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldLayout {
            Pane {
                view: Option<OldView>,
            },
            Content {
                content: OldContent,
            },
            Split {
                first: Box<OldLayout>,
                second: Box<OldLayout>,
            },
        }
        #[derive(Deserialize)]
        struct OldView {
            content: OldContent,
        }
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum OldContent {
            Files(OldFolder),
            Settings(#[allow(dead_code)] SettingsContent),
            Monitor(#[allow(dead_code)] MonitorContent),
        }
        #[derive(Deserialize)]
        struct OldFolder {
            #[serde(default)]
            tabs: Vec<OldFileTab>,
            active: Option<OldFileTab>,
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldFileTab {
            File { diff: Option<OldScope> },
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum OldScope {
            Unstaged,
            Staged,
        }
        /// Each tab of every files content, open or active: whether it is a diff.
        fn diffs(node: &OldLayout) -> Vec<bool> {
            let folder = |content: &OldContent| match content {
                OldContent::Files(folder) => folder
                    .tabs
                    .iter()
                    .chain(&folder.active)
                    .map(|OldFileTab::File { diff }| {
                        matches!(diff, Some(OldScope::Unstaged | OldScope::Staged))
                    })
                    .collect(),
                OldContent::Settings(_) | OldContent::Monitor(_) => Vec::new(),
            };
            match node {
                OldLayout::Pane { view } => {
                    view.iter().flat_map(|view| folder(&view.content)).collect()
                }
                OldLayout::Content { content } => folder(content),
                OldLayout::Split { first, second } => [diffs(first), diffs(second)].concat(),
            }
        }

        let workspace = commit_diffs_workspace();
        // Without the rewrite, a commit diff would fail to parse and quarantine the file.
        let plain = serde_json::to_value(Manifest {
            format_version: FORMAT_VERSION,
            workspace: workspace.clone(),
        })
        .unwrap();
        assert!(serde_json::from_value::<OldManifest>(plain).is_err());

        let path = test_path("commit-diff-downgrade");
        let (store, _) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        store.commit(&workspace).unwrap();
        let old: OldManifest = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(old.format_version, FORMAT_VERSION);
        let tab = &old.workspace.sessions[0].tabs[0];
        // The view's tabs and active one, then the files pane's; every commit diff reads as
        // an Edit tab.
        assert_eq!(diffs(&tab.layout), [false; 6]);
        assert_eq!(diffs(tab.previous_layout.as_ref().unwrap()), [false; 6]);
        let _ = fs::remove_file(path);
    }
}
