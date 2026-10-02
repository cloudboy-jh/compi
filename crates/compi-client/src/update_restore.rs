//! One-time, exact-window update recovery. No shell startup state is replayed.
use crate::{
    Result,
    client_state::{CLIENT_STATE_VERSION, valid_slot_id},
    config::LoadedConfig,
    connection::ConnectionTarget,
};
use compi_protocol::ServerId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_WINDOWS: usize = 32;
const MAX_AGE_SECONDS: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreWindow {
    pub slot_id: String,
    pub server_id: ServerId,
    #[serde(with = "target_serde")]
    pub target: ConnectionTarget,
    pub config: LoadedConfig,
}

mod target_serde {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        target: &ConnectionTarget,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        target.launch_options().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<ConnectionTarget, D::Error> {
        let (instance, connect) = <(Option<String>, Option<String>)>::deserialize(deserializer)?;
        ConnectionTarget::from_options(instance, connect).map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    version: u32,
    state_version: u32,
    expected_version: String,
    created: u64,
    consumed: bool,
    #[serde(default)]
    pending_receipt: Option<PendingReceipt>,
    windows: Vec<RestoreWindow>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingReceipt {
    path: PathBuf,
    token: String,
}

impl Handoff {
    pub fn create(path: &Path, expected_version: &str, windows: Vec<RestoreWindow>) -> Result<()> {
        if !path.is_absolute() {
            return Err("Update handoff requires an absolute owned path".into());
        }
        let handoff = Self {
            version: VERSION,
            state_version: CLIENT_STATE_VERSION,
            expected_version: expected_version.to_owned(),
            created: now()?,
            consumed: false,
            pending_receipt: None,
            windows,
        };
        handoff.validate(false)?;
        let _lock = lock_handoff(path)?;
        if path.exists() {
            return Err("An update handoff already exists; use its recovery action instead of overwriting it.".into());
        }
        persist(path, &handoff)
    }

    /// Inspect a prepared host without consuming it or requiring the newer build.
    pub fn inspect(path: &Path) -> Result<Vec<RestoreWindow>> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Update handoff exceeds eight MiB.".into());
        }
        let handoff: Self = serde_json::from_slice(&bytes)?;
        handoff.validate(false)?;
        if handoff.consumed {
            return Err("Update handoff is already consumed".into());
        }
        Ok(handoff.windows)
    }

    fn validate(&self, restoring: bool) -> Result<()> {
        if self.version != VERSION || self.state_version != CLIENT_STATE_VERSION {
            return Err("Update handoff/presentation schema is incompatible; retained state was not modified.".into());
        }
        let now = now()?;
        if self.created > now + 60 || now.saturating_sub(self.created) > MAX_AGE_SECONDS {
            return Err(
                "Update handoff expired; retained window state is available for manual recovery."
                    .into(),
            );
        }
        if self.expected_version.is_empty()
            || self.expected_version.len() > 128
            || (restoring && self.expected_version != env!("CARGO_PKG_VERSION"))
        {
            return Err("Update restore was launched by the wrong product build.".into());
        }
        if self.windows.is_empty() || self.windows.len() > MAX_WINDOWS {
            return Err("Update handoff must contain 1-32 windows.".into());
        }
        let mut slots = HashSet::new();
        let mut host = None;
        for window in &self.windows {
            if !valid_slot_id(&window.slot_id)
                || window.server_id.as_str().is_empty()
                || window.server_id.as_str().len() > 256
            {
                return Err(
                    "Invalid exact window slot or server identity in update handoff.".into(),
                );
            }
            let instance = window.target.state_instance();
            if let Some(existing) = &host {
                if existing != &instance {
                    return Err("One handoff cannot mix distinct GUI hosts.".into());
                }
            } else {
                host = Some(instance.clone());
            }
            if !slots.insert(&window.slot_id) {
                return Err("Duplicate exact window slot in update handoff.".into());
            }
            window.config.validate_launch()?;
            if !window.config.path.is_absolute() {
                return Err(
                    "Update handoff must retain an absolute selected configuration path.".into(),
                );
            }
        }
        Ok(())
    }
}

pub struct RestoreSession {
    path: PathBuf,
    handoff: Mutex<Handoff>,
    windows: Vec<RestoreWindow>,
    ready: Mutex<HashSet<String>>,
    _lock: File,
}

impl RestoreSession {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        let lock = lock_handoff(path)?;
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Update handoff exceeds eight MiB.".into());
        }
        let mut handoff: Handoff = serde_json::from_slice(&bytes)?;
        handoff.validate(true)?;
        // Receipt is the durable commit point. A crash after publishing readiness
        // but before writing `consumed` must not reopen already-restored windows.
        if !handoff.consumed && confirmed_receipt(&handoff)? {
            handoff.consumed = true;
            persist(path, &handoff)?;
        }
        if handoff.consumed {
            return Err(
                "Update handoff has already restored its windows; it cannot be replayed.".into(),
            );
        }
        Ok(Arc::new(Self {
            windows: handoff.windows.clone(),
            path: path.to_owned(),
            handoff: Mutex::new(handoff),
            ready: Mutex::new(HashSet::new()),
            _lock: lock,
        }))
    }

    pub fn windows(&self) -> &[RestoreWindow] {
        &self.windows
    }

    /// Called only after authoritative reconciliation and existing terminal attachment.
    pub fn mark_ready(&self, slot_id: &str) -> Result<bool> {
        if !self.windows.iter().any(|window| window.slot_id == slot_id) {
            return Err("Readiness names a window outside this handoff.".into());
        }
        let mut ready = self.ready.lock();
        ready.insert(slot_id.to_owned());
        Ok(ready.len() == self.windows.len())
    }

    pub fn publish_readiness(&self, path: &Path, token: &str) -> Result<()> {
        if self.ready.lock().len() != self.windows.len() {
            return Err("Not every restored window has reconciled and attached.".into());
        }
        let mut handoff = self.handoff.lock();
        if handoff.consumed {
            return Ok(());
        }
        if !path.is_absolute() || !(32..=256).contains(&token.len()) {
            return Err("Invalid private update readiness destination or token".into());
        }
        handoff.pending_receipt = Some(PendingReceipt {
            path: path.to_owned(),
            token: token.to_owned(),
        });
        persist(&self.path, &handoff)?;
        let receipt = compi_update::ReadinessReceipt {
            token: token.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            attached: true,
            error: None,
        };
        if let Err(error) = compi_update::publish_readiness(path, &receipt) {
            handoff.pending_receipt = None;
            persist(&self.path, &handoff)?;
            return Err(error.into());
        }
        handoff.consumed = true;
        persist(&self.path, &handoff)?;
        Ok(())
    }
}

fn confirmed_receipt(handoff: &Handoff) -> Result<bool> {
    let Some(pending) = &handoff.pending_receipt else {
        return Ok(false);
    };
    if !pending.path.is_absolute() || !(32..=256).contains(&pending.token.len()) {
        return Err("Invalid pending readiness acknowledgement".into());
    }
    let file = match File::open(&pending.path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err("Pending readiness acknowledgement exceeds bound".into());
    }
    let receipt: compi_update::ReadinessReceipt = serde_json::from_slice(&bytes)?;
    Ok(receipt.token == pending.token
        && receipt.version == handoff.expected_version
        && receipt.attached
        && receipt.error.is_none())
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn lock_handoff(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .ok_or("Update handoff requires an owned directory")?;
    fs::create_dir_all(parent)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(path.with_extension("lock"))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => {
            Err("Another launch owns this update handoff; no duplicate windows were opened.".into())
        }
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}
fn persist(path: &Path, handoff: &Handoff) -> Result<()> {
    let bytes = serde_json::to_vec(handoff)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Update handoff exceeds eight MiB.".into());
    }
    let temporary = path.with_extension("pending");
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
        crate::client_state::replace_file(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                    "compi-update-restore-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )))
        }
        fn window(&self, slot_id: &str) -> RestoreWindow {
            let config = LoadedConfig {
                path: self.0.join("selected.toml"),
                ..LoadedConfig::default()
            };
            RestoreWindow {
                slot_id: slot_id.to_owned(),
                server_id: ServerId::from("stable"),
                target: ConnectionTarget::Local {
                    instance: Some("restore-tests".to_owned()),
                },
                config,
            }
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn failed_and_partial_launch_remain_recoverable_without_duplicate_ownership() {
        let directory = Directory::new();
        let path = directory.0.join("handoff.json");
        Handoff::create(
            &path,
            env!("CARGO_PKG_VERSION"),
            vec![
                directory.window("primary"),
                directory.window("aux-00000000000000000001"),
            ],
        )
        .unwrap();
        let session = RestoreSession::open(&path).unwrap();
        assert!(RestoreSession::open(&path).is_err());
        assert!(!session.mark_ready("primary").unwrap());
        assert!(!session.mark_ready("primary").unwrap());
        assert!(session.mark_ready("unrelated").is_err());
        assert!(
            session
                .publish_readiness(&directory.0.join("receipt.json"), &"x".repeat(32))
                .is_err()
        );
        drop(session);
        let recovered = RestoreSession::open(&path).unwrap();
        assert!(!recovered.mark_ready("primary").unwrap());
        assert!(recovered.mark_ready("aux-00000000000000000001").unwrap());
        assert!(
            recovered
                .publish_readiness(&directory.0.join("receipt.json"), "invalid")
                .is_err()
        );
        drop(recovered);
        assert!(RestoreSession::open(&path).is_ok());
    }

    #[test]
    fn consumed_handoff_is_not_replayed_and_receipt_requires_all_windows() {
        let directory = Directory::new();
        let path = directory.0.join("handoff.json");
        let receipt = directory.0.join("receipt.json");
        Handoff::create(
            &path,
            env!("CARGO_PKG_VERSION"),
            vec![directory.window("primary")],
        )
        .unwrap();
        let session = RestoreSession::open(&path).unwrap();
        assert!(
            session
                .publish_readiness(&receipt, &"x".repeat(32))
                .is_err()
        );
        assert!(!receipt.exists());
        assert!(session.mark_ready("primary").unwrap());
        session
            .publish_readiness(&receipt, &"x".repeat(32))
            .unwrap();
        let acknowledgement: compi_update::ReadinessReceipt =
            serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
        assert!(acknowledgement.attached);
        assert_eq!(acknowledgement.version, env!("CARGO_PKG_VERSION"));
        drop(session);
        assert!(RestoreSession::open(&path).is_err());
    }

    #[test]
    fn duplicate_slots_and_wrong_build_leave_retained_handoff_intact() {
        let directory = Directory::new();
        let path = directory.0.join("handoff.json");
        assert!(
            Handoff::create(
                &path,
                env!("CARGO_PKG_VERSION"),
                vec![directory.window("primary"), directory.window("primary")]
            )
            .is_err()
        );
        assert!(!path.exists());
        Handoff::create(&path, "99.0.0", vec![directory.window("primary")]).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(RestoreSession::open(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn receipt_commit_survives_crash_before_consumed_write() {
        let directory = Directory::new();
        let path = directory.0.join("handoff.json");
        let receipt = directory.0.join("receipt.json");
        let token = "x".repeat(32);
        Handoff::create(
            &path,
            env!("CARGO_PKG_VERSION"),
            vec![directory.window("primary")],
        )
        .unwrap();
        let session = RestoreSession::open(&path).unwrap();
        {
            let mut handoff = session.handoff.lock();
            handoff.pending_receipt = Some(PendingReceipt {
                path: receipt.clone(),
                token: token.clone(),
            });
            persist(&path, &handoff).unwrap();
        }
        drop(session);
        // No receipt: failed launch remains recoverable.
        let retry = RestoreSession::open(&path).unwrap();
        drop(retry);
        compi_update::publish_readiness(
            &receipt,
            &compi_update::ReadinessReceipt {
                token,
                version: env!("CARGO_PKG_VERSION").to_owned(),
                attached: true,
                error: None,
            },
        )
        .unwrap();
        // Receipt committed, consumed write interrupted: no replay is possible.
        assert!(RestoreSession::open(&path).is_err());
        let handoff: Handoff = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert!(handoff.consumed);
    }
}
