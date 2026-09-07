use crate::Result;
use compi_protocol::{
    LaunchRequest, LayoutNode, MutationReceipt, PaneId, ProcessLifetimeId, ServerGeneration,
    ServerId, SessionId, SurfaceId, SurfaceInfo, SurfaceStatus, TabId, WorkingDirectory,
    WorkspaceSession, WorkspaceSnapshot, WorkspaceTab,
};
use serde::{Deserialize, Serialize};
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

const FORMAT_VERSION: u32 = 1;

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

    fn open_path(path: PathBuf, legacy_paths: &[PathBuf]) -> Result<(Self, StoredWorkspace)> {
        let store = Self {
            path: Some(path.clone()),
        };
        if path.is_file() {
            let bytes = fs::read(&path)?;
            return match serde_json::from_slice::<Manifest>(&bytes) {
                Ok(manifest) if manifest.format_version == FORMAT_VERSION => {
                    validate(&manifest.workspace)?;
                    let mut workspace = manifest.workspace;
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
        let bytes = serde_json::to_vec(&Manifest {
            format_version: FORMAT_VERSION,
            workspace: workspace.clone(),
        })?;
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
                    .unwrap_or_else(|| "surface was owned by a previous daemon".into()),
            )
        } else {
            session.error
        };
        tabs.push(WorkspaceTab {
            id: TabId::new(format!("tab-imported-{}", session.id)),
            label: format!("Shell {}", short_id(&session.id)),
            layout: LayoutNode::Pane {
                pane_id: PaneId::new(format!("pane-imported-{}", session.id)),
                surface_id: surface_id.clone(),
            },
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
            },
            working_directory: session.working_directory,
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
            surface.error = Some("surface was owned by a previous daemon".into());
            changed = true;
        }
    }
    if !workspace.pending_removals.is_empty() {
        workspace.pending_removals.clear();
        workspace.recovery_message =
            Some("Interrupted workspace removal was retained for explicit recovery".into());
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
    for session in &workspace.sessions {
        if !session_ids.insert(session.id.clone()) {
            return Err("workspace contains duplicate session IDs".into());
        }
        for tab in &session.tabs {
            if !tab_ids.insert(tab.id.clone()) {
                return Err("workspace contains duplicate tab IDs".into());
            }
            validate_layout(&tab.layout, &surface_ids, &mut pane_ids)?;
        }
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
        } => {
            if !panes.insert(pane_id.clone()) {
                return Err("workspace contains duplicate pane IDs".into());
            }
            if !surfaces.contains(surface_id) {
                return Err(
                    format!("pane {pane_id} references missing surface {surface_id}").into(),
                );
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
            validate_layout(first, surfaces, panes)?;
            validate_layout(second, surfaces, panes)?;
        }
    }
    Ok(())
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

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "compi-workspace-store-{name}-{}-{}.json",
            std::process::id(),
            now_ms()
        ))
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
            },
            working_directory: None,
        });
        workspace.sessions.push(WorkspaceSession {
            id: SessionId::new("session-1"),
            label: "Default".into(),
            tabs: vec![WorkspaceTab {
                id: TabId::new("tab-1"),
                label: "Shell".into(),
                layout: LayoutNode::Pane {
                    pane_id: PaneId::new("pane-1"),
                    surface_id: SurfaceId::new("surface-1"),
                },
            }],
        });
        store.commit(&workspace).unwrap();
        let (_, reopened) = WorkspaceStore::open_path(path.clone(), &[]).unwrap();
        assert_eq!(reopened.surfaces[0].status, SurfaceStatus::Lost);
        assert!(!reopened.surfaces[0].attached);
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
            },
            working_directory: None,
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
                    first: Box::new(LayoutNode::Pane {
                        pane_id: PaneId::new("pane-1"),
                        surface_id: SurfaceId::new("surface-1"),
                    }),
                    second: Box::new(LayoutNode::Pane {
                        pane_id: PaneId::new("pane-2"),
                        surface_id: SurfaceId::new("surface-1"),
                    }),
                },
            }],
        });
        assert!(validate(&workspace).is_err());
    }
}
