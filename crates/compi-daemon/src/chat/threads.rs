//! The threads Compi keeps: each chat's transcript, saved by Compi itself so a chat
//! comes back as it was after its pane closes or the daemon restarts. A thread is one
//! file, `<data dir>/threads[-<instance>]/<chat id>.json`, replaced whole on each save
//! (written beside it first, then renamed over it). Saves go to a writer thread, the
//! latest per chat winning, so no chat waits on the disk. What the threads list shows of
//! each is read from the folder once, then kept current as threads are saved.
use compi_protocol::chat::{ChatContent, ChatEntry};
use compi_protocol::{SessionId, SurfaceId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

/// The version of a thread's file.
const FORMAT: u32 = 1;
/// The longest title a thread takes from its first prompt, in characters.
const PROMPT_TITLE_CHARS: usize = 80;

/// A thread as its file holds it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct ThreadRecord {
    pub format: u32,
    /// The chat, its session the latest one.
    pub chat: ChatContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_ms: u64,
    /// When its transcript last changed.
    pub updated_ms: u64,
    /// The terminal whose view last showed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_surface: Option<SurfaceId>,
    /// The workspace whose tab last held it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<SessionId>,
    pub entries: Vec<ChatEntry>,
}

impl ThreadRecord {
    pub fn new(
        chat: ChatContent,
        title: Option<String>,
        created_ms: u64,
        updated_ms: u64,
        last_surface: Option<SurfaceId>,
        workspace: Option<SessionId>,
        entries: Vec<ChatEntry>,
    ) -> Self {
        Self {
            format: FORMAT,
            chat,
            title,
            created_ms,
            updated_ms,
            last_surface,
            workspace,
            entries,
        }
    }
}

/// What the threads list shows of a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThreadSummary {
    pub chat: ChatContent,
    /// The session's title, else its first prompt's first line.
    pub title: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// The workspace whose tab last held it.
    pub workspace: Option<SessionId>,
}

impl ThreadSummary {
    fn of(record: &ThreadRecord) -> Self {
        Self {
            chat: record.chat.clone(),
            title: thread_title(record.title.as_deref(), &record.entries),
            created_ms: record.created_ms,
            updated_ms: record.updated_ms,
            workspace: record.workspace.clone(),
        }
    }
}

/// A thread's title: the session's, else its first prompt's first line, cut short.
pub(super) fn thread_title(title: Option<&str>, entries: &[ChatEntry]) -> Option<String> {
    if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
        return Some(title.to_owned());
    }
    entries.iter().find_map(|entry| match entry {
        ChatEntry::User { text, .. } => {
            let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
            Some(match line.char_indices().nth(PROMPT_TITLE_CHARS) {
                Some((end, _)) => format!("{}…", &line[..end]),
                None => line.to_owned(),
            })
        }
        _ => None,
    })
}

/// Whether `id` can name a thread's file: a chat ID's letters, digits and dashes.
pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= compi_protocol::chat::MAX_CHAT_ID_BYTES
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A change waiting for the writer.
enum Job {
    Save(Box<ThreadRecord>),
    Delete,
}

#[derive(Default)]
struct StoreState {
    /// Every thread's summary, once read from the folder.
    index: Option<HashMap<String, ThreadSummary>>,
    /// Changes not written yet, the latest per chat.
    pending: HashMap<String, Job>,
    /// A writer thread runs.
    writer: bool,
    /// The writer is writing changes it took from `pending`.
    writing: bool,
}

/// The threads of one daemon instance.
#[derive(Default)]
pub(super) struct ThreadStore {
    /// The folder of thread files; until it is set, nothing is kept.
    folder: OnceLock<PathBuf>,
    state: Mutex<StoreState>,
    /// Signalled when the writer finishes changes it took.
    changed: Condvar,
}

impl ThreadStore {
    /// A store in `folder`.
    #[cfg(test)]
    pub fn at(folder: PathBuf) -> Self {
        let store = Self::default();
        let _ = store.folder.set(folder);
        store
    }

    /// Keeps threads in the data folder from now on, apart for each `instance`.
    pub fn open(&self, instance: Option<&str>) {
        match compi_protocol::paths::data_dir() {
            Ok(directory) => {
                let suffix = instance
                    .map(|instance| format!("-{instance}"))
                    .unwrap_or_default();
                let _ = self.folder.set(directory.join(format!("threads{suffix}")));
            }
            Err(error) => eprintln!("compi-daemon: threads will not be kept: {error}"),
        }
    }

    fn state(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn path(folder: &Path, id: &str) -> PathBuf {
        folder.join(format!("{id}.json"))
    }

    /// The thread of chat `id`, as last saved.
    pub fn load(&self, id: &str) -> Option<ThreadRecord> {
        let folder = self.folder.get()?;
        if !valid_id(id) {
            return None;
        }
        {
            let mut state = self.state();
            loop {
                match state.pending.get(id) {
                    Some(Job::Save(record)) => return Some((**record).clone()),
                    Some(Job::Delete) => return None,
                    // A write under way may be of this thread.
                    None if state.writing => {
                        state = self
                            .changed
                            .wait(state)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    None => break,
                }
            }
        }
        read_record(&Self::path(folder, id)).filter(|record| record.chat.id == id)
    }

    /// Saves `record` in the background, replacing what waits to be saved of it.
    pub fn save(self: &Arc<Self>, record: ThreadRecord) {
        if self.folder.get().is_none() || !valid_id(&record.chat.id) {
            return;
        }
        let id = record.chat.id.clone();
        let mut state = self.state();
        if let Some(index) = state.index.as_mut() {
            index.insert(id.clone(), ThreadSummary::of(&record));
        }
        state.pending.insert(id, Job::Save(Box::new(record)));
        self.wake_writer(state);
    }

    /// Deletes thread `id` in the background.
    pub fn delete(self: &Arc<Self>, id: &str) {
        if self.folder.get().is_none() || !valid_id(id) {
            return;
        }
        let mut state = self.state();
        if let Some(index) = state.index.as_mut() {
            index.remove(id);
        }
        state.pending.insert(id.to_owned(), Job::Delete);
        self.wake_writer(state);
    }

    /// Whether thread `id` is kept.
    pub fn contains(&self, id: &str) -> bool {
        self.indexed()
            .index
            .as_ref()
            .is_some_and(|index| index.contains_key(id))
    }

    /// Every thread kept, in no order.
    pub fn list(&self) -> Vec<ThreadSummary> {
        self.indexed()
            .index
            .as_ref()
            .map(|index| index.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Blocks until every change waiting so far is written.
    #[cfg(test)]
    pub fn flush(&self) {
        let mut state = self.state();
        while state.writer || !state.pending.is_empty() {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The store's state with its index read from the folder, the first time once no
    /// write is under way, so each file is read whole, with the changes not written yet
    /// on top.
    fn indexed(&self) -> MutexGuard<'_, StoreState> {
        let mut state = self.state();
        while state.index.is_none() && state.writing {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if state.index.is_none() {
            let mut index = self
                .folder
                .get()
                .map(|folder| read_index(folder))
                .unwrap_or_default();
            for (id, job) in &state.pending {
                match job {
                    Job::Save(record) => {
                        index.insert(id.clone(), ThreadSummary::of(record));
                    }
                    Job::Delete => {
                        index.remove(id);
                    }
                }
            }
            state.index = Some(index);
        }
        state
    }

    /// Starts the writer unless one runs; it takes the changes waiting.
    fn wake_writer(self: &Arc<Self>, mut state: MutexGuard<'_, StoreState>) {
        if state.writer {
            return;
        }
        state.writer = true;
        drop(state);
        let store = self.clone();
        let started = thread::Builder::new()
            .name("compi-chat-threads".into())
            .spawn(move || store.write_pending());
        if started.is_err() {
            self.write_pending();
        }
    }

    /// Writes changes until none waits, then ends.
    fn write_pending(&self) {
        let Some(folder) = self.folder.get() else {
            return;
        };
        let mut state = self.state();
        loop {
            let jobs: Vec<(String, Job)> = state.pending.drain().collect();
            if jobs.is_empty() {
                state.writer = false;
                self.changed.notify_all();
                return;
            }
            state.writing = true;
            drop(state);
            for (id, job) in jobs {
                let path = Self::path(folder, &id);
                let result = match job {
                    Job::Save(record) => write_record(folder, &path, &record),
                    Job::Delete => remove(&path),
                };
                if let Err(error) = result {
                    eprintln!("compi-daemon: couldn't keep thread {id}: {error}");
                }
            }
            state = self.state();
            state.writing = false;
            self.changed.notify_all();
        }
    }
}

fn read_record(path: &Path) -> Option<ThreadRecord> {
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice::<ThreadRecord>(&bytes) {
        Ok(record) if record.format == FORMAT => Some(record),
        Ok(record) => {
            eprintln!(
                "compi-daemon: thread {} is format {}, not {FORMAT}",
                path.display(),
                record.format
            );
            None
        }
        Err(error) => {
            eprintln!(
                "compi-daemon: couldn't read thread {}: {error}",
                path.display()
            );
            None
        }
    }
}

/// Every thread file in `folder`, summed up.
fn read_index(folder: &Path) -> HashMap<String, ThreadSummary> {
    let Ok(files) = fs::read_dir(folder) else {
        return HashMap::new();
    };
    files
        .filter_map(Result::ok)
        .filter_map(|file| {
            let path = file.path();
            let id = path
                .file_name()?
                .to_str()?
                .strip_suffix(".json")
                .filter(|id| valid_id(id))?
                .to_owned();
            let record = read_record(&path).filter(|record| record.chat.id == id)?;
            Some((id, ThreadSummary::of(&record)))
        })
        .collect()
}

/// Writes `record` beside `path`, then puts it in its place.
fn write_record(folder: &Path, path: &Path, record: &ThreadRecord) -> std::io::Result<()> {
    fs::create_dir_all(folder)?;
    let bytes = serde_json::to_vec(record)?;
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

fn remove(path: &Path) -> std::io::Result<()> {
    let _ = fs::remove_file(path.with_extension("json.tmp"));
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Moves `temporary` over `path` in one step, through to the disk.
#[cfg(windows)]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;
    let wide =
        |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain(once(0)).collect() };
    let (temporary, destination) = (wide(temporary), wide(path));
    // SAFETY: both paths are NUL-terminated wide strings that outlive the call.
    unsafe {
        MoveFileExW(
            PCWSTR(temporary.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(std::io::Error::other)
}

/// Moves `temporary` over `path` in one step, through to the disk.
#[cfg(unix)]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(temporary, path)?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
