use crate::Result;
use crate::protocol::{SessionInfo, SessionStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PCWSTR;

const FORMAT_VERSION: u32 = 1;
const MAX_TERMINAL_RECORDS: usize = 100;
const TERMINAL_RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1_000;

#[derive(Clone)]
pub struct SessionStore {
    state: Arc<Mutex<StoreState>>,
}

struct StoreState {
    path: Option<PathBuf>,
    records: HashMap<String, PersistedSession>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format_version: u32,
    sessions: Vec<PersistedSession>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PersistedSession {
    id: String,
    created_at_ms: u64,
    updated_at_ms: u64,
    status: SessionStatus,
    cols: i16,
    rows: i16,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl SessionStore {
    pub fn memory() -> Self {
        Self {
            state: Arc::new(Mutex::new(StoreState {
                path: None,
                records: HashMap::new(),
            })),
        }
    }

    pub fn open(instance: Option<&str>) -> Result<Self> {
        let directory = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or("LOCALAPPDATA is not set")?
            .join("Compi");
        fs::create_dir_all(&directory)?;
        let file_name = instance
            .map(|instance| format!("sessions-{instance}-v1.json"))
            .unwrap_or_else(|| "sessions-v1.json".to_owned());
        Self::open_path(directory.join(file_name))
    }

    fn open_path(path: PathBuf) -> Result<Self> {
        let mut records = HashMap::new();
        let mut rewrite = false;
        if path.is_file() {
            let bytes = fs::read(&path)?;
            match serde_json::from_slice::<Manifest>(&bytes) {
                Ok(manifest) if manifest.format_version == FORMAT_VERSION => {
                    for record in manifest.sessions {
                        records.insert(record.id.clone(), record);
                    }
                }
                Ok(_) | Err(_) => {
                    quarantine(&path);
                }
            }
        }

        let now = now_ms();
        for record in records.values_mut() {
            if matches!(
                record.status,
                SessionStatus::Starting | SessionStatus::Running
            ) {
                record.status = SessionStatus::Dead;
                record.updated_at_ms = now;
                record.exit_code = None;
                record.error = Some("session ended when the previous daemon stopped".to_owned());
                rewrite = true;
            }
        }
        rewrite |= prune_records(&mut records, now);

        let store = Self {
            state: Arc::new(Mutex::new(StoreState {
                path: Some(path),
                records,
            })),
        };
        if rewrite {
            store.persist()?;
        }
        Ok(store)
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        state.records.values().map(PersistedSession::info).collect()
    }

    pub fn get(&self, id: &str) -> Option<SessionInfo> {
        self.state
            .lock()
            .ok()?
            .records
            .get(id)
            .map(PersistedSession::info)
    }

    pub fn next_ordinal(&self) -> u64 {
        self.state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .records
                    .keys()
                    .filter_map(|id| u64::from_str_radix(id.rsplit('-').next()?, 16).ok())
                    .max()
            })
            .unwrap_or(0)
            .saturating_add(1)
    }

    pub fn record(&self, info: &SessionInfo) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "session metadata lock was poisoned")?;
        state
            .records
            .insert(info.id.clone(), PersistedSession::from_info(info));
        prune_records(&mut state.records, now_ms());
        persist_state(&state)
    }

    pub fn mark_dead(&self, session_ids: &[String], reason: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "session metadata lock was poisoned")?;
        let now = now_ms();
        for session_id in session_ids {
            if let Some(record) = state.records.get_mut(session_id) {
                record.status = SessionStatus::Dead;
                record.updated_at_ms = now;
                record.exit_code = None;
                record.error = Some(reason.to_owned());
            }
        }
        prune_records(&mut state.records, now);
        persist_state(&state)
    }

    fn persist(&self) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| "session metadata lock was poisoned")?;
        persist_state(&state)
    }
}

impl PersistedSession {
    fn from_info(info: &SessionInfo) -> Self {
        Self {
            id: info.id.clone(),
            created_at_ms: info.created_at_ms,
            updated_at_ms: now_ms(),
            status: info.status,
            cols: info.cols,
            rows: info.rows,
            exit_code: info.exit_code,
            error: info.error.clone(),
        }
    }

    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            status: self.status,
            attached: false,
            cols: self.cols,
            rows: self.rows,
            created_at_ms: self.created_at_ms,
            exit_code: self.exit_code,
            error: self.error.clone(),
        }
    }
}

fn persist_state(state: &StoreState) -> Result<()> {
    let Some(path) = state.path.as_ref() else {
        return Ok(());
    };
    let mut sessions: Vec<_> = state.records.values().cloned().collect();
    sessions.sort_by_key(|record| (record.created_at_ms, record.id.clone()));
    let bytes = serde_json::to_vec(&Manifest {
        format_version: FORMAT_VERSION,
        sessions,
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

    let temporary = wide(&temporary);
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

fn prune_records(records: &mut HashMap<String, PersistedSession>, now: u64) -> bool {
    let original_len = records.len();
    let cutoff = now.saturating_sub(TERMINAL_RETENTION_MS);
    records.retain(|_, record| !is_terminal(record.status) || record.updated_at_ms >= cutoff);

    let mut terminal: Vec<_> = records
        .values()
        .filter(|record| is_terminal(record.status))
        .map(|record| (record.updated_at_ms, record.id.clone()))
        .collect();
    terminal.sort_unstable_by(|left, right| right.cmp(left));
    for (_, id) in terminal.into_iter().skip(MAX_TERMINAL_RECORDS) {
        records.remove(&id);
    }
    records.len() != original_len
}

fn is_terminal(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Exited | SessionStatus::Failed | SessionStatus::Dead
    )
}

fn quarantine(path: &Path) {
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("sessions");
    for ordinal in 0..100 {
        let quarantine = path.with_file_name(format!("{stem}.corrupt-{}-{ordinal}.json", now_ms()));
        if !quarantine.exists() {
            if let Err(error) = fs::rename(path, &quarantine) {
                eprintln!(
                    "compi-daemon: could not quarantine malformed session metadata {}: {error}",
                    path.display()
                );
            }
            return;
        }
    }
    eprintln!(
        "compi-daemon: could not allocate a quarantine name for malformed session metadata {}",
        path.display()
    );
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(once(0)).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "compi-session-store-{name}-{}-{}.json",
            std::process::id(),
            now_ms()
        ))
    }

    #[test]
    fn running_records_become_dead_after_reopen() {
        let path = test_path("dead");
        let store = SessionStore::open_path(path.clone()).unwrap();
        store
            .record(&SessionInfo {
                id: "s-1-1".to_owned(),
                status: SessionStatus::Running,
                attached: true,
                cols: 80,
                rows: 24,
                created_at_ms: 1,
                exit_code: None,
                error: None,
            })
            .unwrap();
        drop(store);

        let reopened = SessionStore::open_path(path.clone()).unwrap();
        let sessions = reopened.list();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, SessionStatus::Dead);
        assert!(!sessions[0].attached);
        assert!(
            sessions[0]
                .error
                .as_deref()
                .unwrap()
                .contains("previous daemon")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_manifest_is_quarantined() {
        let path = test_path("malformed");
        fs::write(&path, b"{not-json").unwrap();
        let store = SessionStore::open_path(path.clone()).unwrap();
        assert!(store.list().is_empty());
        assert!(!path.exists());
        let stem = path.file_stem().unwrap().to_string_lossy();
        let quarantined = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|candidate| {
                candidate.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(&format!("{stem}.corrupt-"))
                })
            })
            .expect("malformed manifest was not quarantined");
        let _ = fs::remove_file(quarantined);
    }

    #[test]
    fn terminal_record_count_is_bounded() {
        let now = now_ms();
        let mut records = HashMap::new();
        for ordinal in 0..105 {
            let id = format!("s-1-{ordinal:x}");
            records.insert(
                id.clone(),
                PersistedSession {
                    id,
                    created_at_ms: ordinal,
                    updated_at_ms: now + ordinal,
                    status: SessionStatus::Dead,
                    cols: 80,
                    rows: 24,
                    exit_code: None,
                    error: None,
                },
            );
        }
        assert!(prune_records(&mut records, now));
        assert_eq!(records.len(), MAX_TERMINAL_RECORDS);
        assert!(!records.contains_key("s-1-0"));
    }
}
