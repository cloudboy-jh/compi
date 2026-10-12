//! Unsaved edits: the prompt (Save / Don't save / Cancel) that stands between an action
//! and the dirty editors it would drop, and backups that keep each dirty editor's text
//! in the client data directory so edits survive crashes, restarts and updates.
//!
//! Backups live in `unsaved-edits-v1/<instance>/` of the data directory, one JSON file
//! per (environment, path) of the instance. One worker thread performs every backup
//! write, delete, read and listing in order, so a delete queued after a write always
//! wins and a read sees every write queued before it.
use super::content::{LineEndings, content_leaves};
use super::*;
use compi_protocol::{ContentEnvironment, FileVersion, LifecycleConsent};
use gpui_component::input::Rope;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a dirty editor waits before its text is written to its backup.
pub(super) const BACKUP_DELAY: Duration = Duration::from_millis(1000);
/// Backups older than this are deleted at startup.
const BACKUP_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const BACKUP_DIRECTORY: &str = "unsaved-edits-v1";
const TEMPORARY_SUFFIX: &str = ".json.tmp";

/// One file's unsaved edits, as kept on disk.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::gui) struct Backup {
    pub(super) environment: ContentEnvironment,
    pub(super) path: String,
    /// The file on disk when the edits started; a different file now means it changed.
    pub(super) base: FileVersion,
    pub(super) endings: LineEndings,
    pub(super) text: String,
    /// Seconds since the Unix epoch.
    pub(super) saved_at: u64,
}

/// A backup without its text, as the recover list shows it.
#[derive(Clone, Debug, Deserialize)]
pub(in crate::gui) struct BackupEntry {
    pub(super) environment: ContentEnvironment,
    pub(super) path: String,
    pub(super) saved_at: u64,
}

/// The backup directory of one instance.
#[derive(Clone)]
pub(in crate::gui) struct Backups {
    directory: PathBuf,
}

/// What the worker writes: the editor's text is copied out of its rope there, not on
/// the UI thread.
struct Draft {
    environment: ContentEnvironment,
    path: String,
    base: FileVersion,
    endings: LineEndings,
    text: Rope,
}

enum Job {
    Write { file: PathBuf, draft: Draft },
    Delete(PathBuf),
    Prune(PathBuf),
    Run(Box<dyn FnOnce() + Send>),
}

static WORKER: LazyLock<mpsc::Sender<Job>> = LazyLock::new(|| {
    let (sender, jobs) = mpsc::channel::<Job>();
    // Without a worker, sends fail and edits are simply not backed up.
    let _ = thread::Builder::new()
        .name("compi-unsaved-edits".into())
        .spawn(move || {
            for job in jobs {
                run_job(job);
            }
        });
    sender
});

fn enqueue(job: Job) {
    let _ = WORKER.send(job);
}

/// Runs `work` on the worker after every job queued before it, and waits for it.
fn ask<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (sender, answer) = mpsc::channel();
    enqueue(Job::Run(Box::new(move || {
        let _ = sender.send(work());
    })));
    answer.recv().ok()
}

/// Waits until every backup queued so far is written or deleted.
pub(super) fn sync() {
    ask(|| ());
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn run_job(job: Job) {
    match job {
        Job::Write { file, draft } => {
            let backup = Backup {
                environment: draft.environment,
                path: draft.path,
                base: draft.base,
                endings: draft.endings,
                text: draft.text.to_string(),
                saved_at: now_seconds(),
            };
            if let Err(error) = write_backup(&file, &backup) {
                eprintln!("Cannot keep unsaved edits of {}: {error}", backup.path);
            }
        }
        Job::Delete(file) => {
            if let Err(error) = fs::remove_file(&file)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("Cannot delete unsaved edits {}: {error}", file.display());
            }
        }
        Job::Prune(directory) => prune(&directory, now_seconds()),
        Job::Run(work) => work(),
    }
}

/// Writes beside the backup, then renames over it, so a crash leaves the old backup or
/// the new one, never part of either.
fn write_backup(file: &Path, backup: &Backup) -> crate::Result<()> {
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory)?;
    }
    let temporary = file.with_extension(&TEMPORARY_SUFFIX[1..]);
    fs::write(&temporary, serde_json::to_vec(backup)?)?;
    fs::rename(&temporary, file)?;
    Ok(())
}

/// Whether a backup written at `saved_at` is past keeping at `now`.
fn expired(saved_at: u64, now: u64) -> bool {
    now.saturating_sub(saved_at) > BACKUP_LIFETIME.as_secs()
}

/// Deletes expired and unreadable backups and writes a crash left unfinished.
fn prune(directory: &Path, now: u64) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let stale = if name.ends_with(TEMPORARY_SUFFIX) {
            true
        } else if name.ends_with(".json") {
            read_entry(&entry.path()).is_none_or(|backup| expired(backup.saved_at, now))
        } else {
            false
        };
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn read_entry(file: &Path) -> Option<BackupEntry> {
    serde_json::from_slice(&fs::read(file).ok()?).ok()
}

/// The backup file name of `path` in `environment`: a hash of both, so every pair gets
/// its own file whatever characters the path holds, and equal paths in different
/// environments never meet.
fn backup_name(environment: &ContentEnvironment, path: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut hash = Sha256::new();
    match environment {
        ContentEnvironment::Host => hash.update(b"host\0"),
        ContentEnvironment::Wsl { distribution } => {
            hash.update(b"wsl\0");
            hash.update(distribution.as_bytes());
            hash.update(b"\0");
        }
    }
    hash.update(path.as_bytes());
    let mut name = String::with_capacity(69);
    for byte in hash.finalize() {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".json");
    name
}

impl Backups {
    /// The backups of `instance` (the connection's state instance), after deleting
    /// expired ones; `None` without a data directory.
    pub(super) fn for_instance(instance: Option<&str>) -> Option<Self> {
        let directory = compi_protocol::paths::data_dir()
            .ok()?
            .join(BACKUP_DIRECTORY)
            .join(crate::client_state::instance_directory(instance));
        enqueue(Job::Prune(directory.clone()));
        Some(Self { directory })
    }

    fn file(&self, environment: &ContentEnvironment, path: &str) -> PathBuf {
        self.directory.join(backup_name(environment, path))
    }

    pub(super) fn write(
        &self,
        environment: &ContentEnvironment,
        path: &str,
        base: FileVersion,
        endings: LineEndings,
        text: Rope,
    ) {
        enqueue(Job::Write {
            file: self.file(environment, path),
            draft: Draft {
                environment: environment.clone(),
                path: path.to_owned(),
                base,
                endings,
                text,
            },
        });
    }

    pub(super) fn delete(&self, environment: &ContentEnvironment, path: &str) {
        enqueue(Job::Delete(self.file(environment, path)));
    }

    /// The backup of `path`, after every write queued before. Blocks: call it off the
    /// UI thread.
    pub(super) fn load(&self, environment: &ContentEnvironment, path: &str) -> Option<Backup> {
        let file = self.file(environment, path);
        let (environment, path) = (environment.clone(), path.to_owned());
        ask(move || {
            serde_json::from_slice::<Backup>(&fs::read(&file).ok()?)
                .ok()
                .filter(|backup| backup.environment == environment && backup.path == path)
        })
        .flatten()
    }

    /// Every backup, newest first. Blocks: call it off the UI thread.
    pub(super) fn list(&self) -> Vec<BackupEntry> {
        let directory = self.directory.clone();
        ask(move || {
            let mut backups: Vec<BackupEntry> = fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    // Only files named for what they hold: no temporary or foreign files.
                    read_entry(&entry.path()).filter(|backup| {
                        entry.file_name().to_str()
                            == Some(backup_name(&backup.environment, &backup.path).as_str())
                    })
                })
                .collect();
            backups.sort_by_key(|backup| std::cmp::Reverse(backup.saved_at));
            backups
        })
        .unwrap_or_default()
    }
}

/// How long ago `saved_at` was at `now`, as the restore note says it.
pub(super) fn age(saved_at: u64, now: u64) -> String {
    let seconds = now.saturating_sub(saved_at);
    let (count, unit) = match seconds {
        0..60 => return "less than a minute ago".into(),
        60..3600 => (seconds / 60, "minute"),
        3600..86400 => (seconds / 3600, "hour"),
        _ => (seconds / 86400, "day"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

/// The note above an editor restored from a backup written at `saved_at`.
pub(super) fn restored_note(saved_at: u64) -> String {
    format!(
        "Restored unsaved edits from {}",
        age(saved_at, now_seconds())
    )
}

/// How a restored editor stands against the disk.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Restore {
    /// The file is still what the edits started from.
    Current,
    /// The file changed since: the editor keeps the edits and warns, as for a change
    /// while editing.
    Changed,
}

pub(super) fn restore_against(base: &FileVersion, disk: &FileVersion) -> Restore {
    if base == disk {
        Restore::Current
    } else {
        Restore::Changed
    }
}

/// The dirty editors an action would drop.
#[derive(Clone, Debug)]
pub(super) enum CloseScope {
    File {
        pane_id: PaneId,
        path: String,
    },
    Panes(Vec<PaneId>),
    /// Every editor of this window.
    Window,
    /// Every editor of every window: quitting, restarting the daemon, updating.
    App,
}

impl CloseScope {
    /// Whether the scope drops the editor of `path` in `pane_id`, within one window.
    pub(super) fn covers(&self, pane_id: &PaneId, path: &str) -> bool {
        match self {
            Self::File {
                pane_id: pane,
                path: file,
            } => pane == pane_id && file == path,
            Self::Panes(panes) => panes.contains(pane_id),
            Self::Window | Self::App => true,
        }
    }
}

/// The content panes of `layout`.
fn layout_panes(layout: &LayoutNode, panes: &mut Vec<PaneId>) {
    content_leaves(layout, &mut |pane_id, _| panes.push(pane_id.clone()));
}

/// The content panes in tab `tab_id`.
pub(super) fn tab_panes(workspace: &WorkspaceSnapshot, tab_id: &TabId) -> Vec<PaneId> {
    let mut panes = Vec::new();
    for tab in workspace
        .sessions
        .iter()
        .flat_map(|session| &session.tabs)
        .filter(|tab| tab.id == *tab_id)
    {
        layout_panes(&tab.layout, &mut panes);
    }
    panes
}

/// The panes `mutation` removes from `workspace`.
pub(super) fn removed_panes(
    workspace: &WorkspaceSnapshot,
    mutation: &WorkspaceMutation,
) -> Vec<PaneId> {
    match mutation {
        WorkspaceMutation::RemovePane { pane_id } => vec![pane_id.clone()],
        WorkspaceMutation::RemoveTab { tab_id } => tab_panes(workspace, tab_id),
        WorkspaceMutation::RemoveSession { session_id } => {
            let mut panes = Vec::new();
            for tab in workspace
                .sessions
                .iter()
                .filter(|session| session.id == *session_id)
                .flat_map(|session| &session.tabs)
            {
                layout_panes(&tab.layout, &mut panes);
            }
            panes
        }
        _ => Vec::new(),
    }
}

/// The action an unsaved-changes prompt stands before.
#[derive(Clone)]
pub(in crate::gui) enum AfterUnsaved {
    CloseFile {
        pane_id: PaneId,
        path: String,
    },
    Mutate(WorkspaceMutation),
    Transfer {
        tab_id: TabId,
        destination: Option<AnyWindowHandle>,
    },
    CloseWindow,
    Quit,
    RestartDaemon(LifecycleConsent),
    Install(Vec<SurfaceId>),
}

/// A dirty file the prompt lists, and the window whose editor holds it.
#[derive(Clone)]
pub(in crate::gui) struct PromptFile {
    pub(super) window: AnyWindowHandle,
    pub(super) pane_id: PaneId,
    pub(super) path: String,
    /// Its folder and environment.
    pub(super) place: String,
}

/// The saves the prompt's Save started. Each holds a ticket; when the last ticket is
/// dropped the window that asked hears whether every file was saved.
pub(in crate::gui) struct SaveBatch {
    pub(super) id: u64,
    remaining: Cell<usize>,
    failed: Cell<bool>,
    origin: WindowHandle<CompiApp>,
    notify: UiEventSender,
}

/// One file's part of a `SaveBatch`. Dropped without `saved`, the file counts as not
/// saved: its save failed, or its editor went away first.
pub(in crate::gui) struct SaveTicket {
    batch: Rc<SaveBatch>,
    saved: bool,
}

impl SaveTicket {
    fn new(batch: &Rc<SaveBatch>) -> Self {
        batch.remaining.set(batch.remaining.get() + 1);
        Self {
            batch: batch.clone(),
            saved: false,
        }
    }

    pub(super) fn saved(mut self) {
        self.saved = true;
    }
}

impl Drop for SaveTicket {
    fn drop(&mut self) {
        let batch = &self.batch;
        if !self.saved {
            batch.failed.set(true);
        }
        let remaining = batch.remaining.get() - 1;
        batch.remaining.set(remaining);
        if remaining == 0 {
            batch.notify.send(UiEvent::UnsavedSaved(batch.id));
        }
    }
}

static NEXT_SAVE_BATCH: AtomicU64 = AtomicU64::new(1);

impl CompiApp {
    /// Runs `then`, first asking what to do with the dirty editors in `scope` it would
    /// drop, if there are any.
    pub(super) fn guard_unsaved(
        &mut self,
        scope: CloseScope,
        then: AfterUnsaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let files = self.unsaved_in(&scope, window, cx);
        if files.is_empty() {
            self.run_after_unsaved(then, window, cx);
        } else {
            self.ask_unsaved(files, then, window, cx);
        }
    }

    fn ask_unsaved(
        &mut self,
        files: Vec<PromptFile>,
        then: AfterUnsaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_overlay(
            Overlay::Unsaved {
                files,
                then,
                saving: None,
            },
            "",
        );
        // Save has focus, so Enter saves.
        self.overlay_focus = 2;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// The window's close button: unsaved edits ask first.
    pub(in crate::gui) fn request_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.guard_unsaved(CloseScope::Window, AfterUnsaved::CloseWindow, window, cx);
    }

    /// The system asks to close the window (Alt+F4, the taskbar): unsaved edits ask
    /// first, and the window closes once they are saved or dropped.
    pub(in crate::gui) fn window_should_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let files = self.unsaved_in(&CloseScope::Window, window, cx);
        if files.is_empty() {
            self.flush_state();
            return true;
        }
        self.ask_unsaved(files, AfterUnsaved::CloseWindow, window, cx);
        false
    }

    /// Dirty editors `scope` covers: this window's, and with `App` every window's.
    fn unsaved_in(&self, scope: &CloseScope, window: &Window, cx: &App) -> Vec<PromptFile> {
        let here = Window::window_handle(window);
        let mut files = self.prompt_files(scope, here);
        if matches!(scope, CloseScope::App) {
            for handle in cx.windows() {
                if handle == here {
                    continue;
                }
                // Reading the window being updated fails; that one is `self`.
                if let Some(typed) = handle.downcast::<CompiApp>()
                    && let Ok(other) = typed.read(cx)
                {
                    files.extend(other.prompt_files(scope, handle));
                }
            }
        }
        files
    }

    fn prompt_files(&self, scope: &CloseScope, window: AnyWindowHandle) -> Vec<PromptFile> {
        self.unsaved_editors()
            .into_iter()
            .filter(|(pane_id, path, _)| scope.covers(pane_id, path))
            .map(|(pane_id, path, environment)| PromptFile {
                window,
                place: format!(
                    "{} · {}",
                    content::parent_folder(&path),
                    self.environment_label(&environment)
                ),
                pane_id,
                path,
            })
            .collect()
    }

    /// After a workspace change: drops prompt entries whose file no pane shows any more
    /// (closed elsewhere, for example by `compi pane close` after its own confirmation) or
    /// whose editor here is no longer dirty. With none left the prompt closes without its
    /// action, since what it asked about is gone.
    pub(super) fn refresh_unsaved_prompt(&mut self) {
        let live: HashSet<(PaneId, String)> = self
            .unsaved_editors()
            .into_iter()
            .map(|(pane_id, path, _)| (pane_id, path))
            .collect();
        let here: HashSet<PaneId> = self.content_panes().cloned().collect();
        let Some(workspace) = &self.workspace else {
            return;
        };
        let Some(Overlay::Unsaved {
            files,
            saving: None,
            ..
        }) = &mut self.overlay
        else {
            return;
        };
        files.retain(|file| {
            let shown = content::find_content(workspace, &file.pane_id)
                .is_some_and(|content| content.files.contains(&file.path));
            shown
                && (!here.contains(&file.pane_id)
                    || live.contains(&(file.pane_id.clone(), file.path.clone())))
        });
        if files.is_empty() {
            self.dismiss_overlay();
        }
    }

    pub(super) fn run_after_unsaved(
        &mut self,
        then: AfterUnsaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match then {
            AfterUnsaved::CloseFile { pane_id, path } => self.drop_content_file(&pane_id, &path),
            AfterUnsaved::Mutate(operation) => self.mutate(operation, false),
            AfterUnsaved::Transfer {
                tab_id,
                destination,
            } => self.transfer_tab_mode(tab_id, destination, true, window, cx),
            AfterUnsaved::CloseWindow => {
                self.flush_state();
                window.remove_window();
            }
            AfterUnsaved::Quit => cx.quit(),
            AfterUnsaved::RestartDaemon(consent) => self.begin_daemon_restart(consent),
            AfterUnsaved::Install(shells) => self.updates.install(shells),
        }
        self.rebuild_layout(window, true);
        cx.notify();
    }

    /// A prompt button: 0 Cancel, 1 Don't save, 2 Save.
    pub(super) fn answer_unsaved(
        &mut self,
        choice: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::Unsaved {
            files,
            then,
            saving: None,
        }) = self.overlay.clone()
        else {
            if choice == 0 {
                self.dismiss_overlay();
            }
            return;
        };
        let here = Window::window_handle(window);
        match choice {
            0 => self.dismiss_overlay(),
            1 => {
                for file in &files {
                    if file.window == here {
                        self.discard_edits(&file.pane_id, &file.path);
                    } else if let Some(typed) = file.window.downcast::<CompiApp>() {
                        let _ = typed.update(cx, |other, _, _| {
                            other.discard_edits(&file.pane_id, &file.path)
                        });
                    }
                }
                self.dismiss_overlay();
                self.run_after_unsaved(then, window, cx);
            }
            _ => {
                let Some(origin) = here.downcast::<CompiApp>() else {
                    return;
                };
                let batch = Rc::new(SaveBatch {
                    id: NEXT_SAVE_BATCH.fetch_add(1, Ordering::Relaxed),
                    remaining: Cell::new(0),
                    failed: Cell::new(false),
                    origin,
                    notify: self.event_tx.clone(),
                });
                // Holds the batch open until every save has its ticket.
                let opening = SaveTicket::new(&batch);
                self.overlay = Some(Overlay::Unsaved {
                    files: files.clone(),
                    then,
                    saving: Some(batch.clone()),
                });
                for file in files {
                    let ticket = SaveTicket::new(&batch);
                    if file.window == here {
                        self.save_for_prompt(&file.pane_id, &file.path, ticket, cx);
                    } else if let Some(typed) = file.window.downcast::<CompiApp>() {
                        // A window that is gone drops the ticket: not saved.
                        let _ = typed.update(cx, |other, _, cx| {
                            other.save_for_prompt(&file.pane_id, &file.path, ticket, cx)
                        });
                    }
                }
                opening.saved();
            }
        }
        cx.notify();
    }

    /// Every save of batch `id` finished: the action goes ahead if all succeeded and the
    /// prompt still waits for them; otherwise the banners say what failed.
    pub(super) fn unsaved_saved(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(Overlay::Unsaved {
            then,
            saving: Some(batch),
            ..
        }) = &self.overlay
        else {
            return;
        };
        if batch.id != id || self.overlay_closing_since.is_some() {
            return;
        }
        let (then, failed, origin) = (then.clone(), batch.failed.get(), batch.origin);
        self.dismiss_overlay();
        if failed {
            self.global_error = Some(
                "Not every file was saved, so nothing was closed. The file's banner or notice says why."
                    .into(),
            );
            return;
        }
        cx.defer(move |cx| {
            let _ = origin.update(cx, |this, window, cx| {
                this.run_after_unsaved(then, window, cx)
            });
        });
    }

    /// Recover unsaved edits: lists backups off the UI thread.
    pub(super) fn list_backups(&mut self) {
        let Some(backups) = self.content.backups().cloned() else {
            self.global_error = Some("Unsaved edits are not kept: no data directory".into());
            return;
        };
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            sender.send(UiEvent::UnsavedBackupsListed(backups.list()));
        });
    }

    /// Offers the listed backups whose file no pane of this instance has open.
    pub(super) fn offer_backups(&mut self, backups: Vec<BackupEntry>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let mut open = Vec::new();
        for tab in workspace.sessions.iter().flat_map(|session| &session.tabs) {
            content_leaves(&tab.layout, &mut |_, content| {
                open.extend(
                    content
                        .files
                        .iter()
                        .map(|path| (content.environment.clone(), path.clone())),
                )
            });
        }
        let backups: Vec<_> = backups
            .into_iter()
            .filter(|backup| {
                !open.iter().any(|(environment, path)| {
                    *environment == backup.environment && *path == backup.path
                })
            })
            .collect();
        if backups.is_empty() {
            self.global_error = Some("No unsaved edits to recover".into());
            return;
        }
        self.open_overlay(Overlay::Recover { backups }, "");
    }

    /// The recover list's choices that match `query`.
    pub(super) fn recover_choices(&self, backups: &[BackupEntry], query: &str) -> Vec<Choice> {
        let now = now_seconds();
        backups
            .iter()
            .enumerate()
            .filter(|(_, backup)| backup.path.to_lowercase().contains(query))
            .map(|(index, backup)| Choice {
                title: content::file_name(&backup.path).to_owned(),
                detail: format!(
                    "{} · {} · {}",
                    content::parent_folder(&backup.path),
                    self.environment_label(&backup.environment),
                    age(backup.saved_at, now)
                ),
                group: None,
                reason: None,
                action: ChoiceAction::Recover(index),
            })
            .collect()
    }

    /// Opens a backed-up file beside the focused pane; its editor restores the edits.
    pub(super) fn recover_backup(
        &mut self,
        backup: BackupEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(beside) = self.focused_pane().cloned() else {
            self.global_error = Some("Select a pane first".into());
            return;
        };
        self.open_content_file(
            &beside,
            backup.environment,
            None,
            Some(backup.path),
            None,
            false,
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::{
        PaneContent, ServerGeneration, ServerId, SessionId, SplitAxis, WorkspaceSession,
        WorkspaceTab,
    };

    fn version(sha: &str) -> FileVersion {
        FileVersion {
            len: 1,
            sha256: sha.into(),
        }
    }

    #[test]
    fn backup_names_are_stable_and_never_shared_across_environments() {
        let ubuntu = ContentEnvironment::Wsl {
            distribution: "Ubuntu".into(),
        };
        let name = backup_name(&ContentEnvironment::Host, "/tmp/a.txt");
        // Renaming would orphan every backup written before.
        assert_eq!(
            name,
            "90233bdddaff6abe68347a8015495a6c814f5a843227db3618c9e12ecd2d0be3.json"
        );
        assert_eq!(name, backup_name(&ContentEnvironment::Host, "/tmp/a.txt"));
        assert_ne!(name, backup_name(&ubuntu, "/tmp/a.txt"));
        assert_ne!(name, backup_name(&ContentEnvironment::Host, "/tmp/b.txt"));
        // The separator keeps a distribution name from running into the path.
        assert_ne!(
            backup_name(
                &ContentEnvironment::Wsl {
                    distribution: "Ubuntu/".into()
                },
                "x"
            ),
            backup_name(&ubuntu, "/x")
        );
        assert!(name.len() <= 69 && name.bytes().all(|byte| byte.is_ascii_graphic()));
    }

    #[test]
    fn backups_round_trip_and_reject_another_files_contents() {
        let directory = std::env::temp_dir().join(format!(
            "compi-unsaved-{}-{}",
            std::process::id(),
            now_seconds()
        ));
        let backups = Backups {
            directory: directory.clone(),
        };
        let environment = ContentEnvironment::Host;
        backups.write(
            &environment,
            "/tmp/a.txt",
            version("a"),
            LineEndings::Crlf,
            Rope::from("edited\n"),
        );
        let restored = backups.load(&environment, "/tmp/a.txt").unwrap();
        assert_eq!(restored.text, "edited\n");
        assert_eq!(restored.base, version("a"));
        assert_eq!(restored.endings, LineEndings::Crlf);
        assert_eq!(backups.list().len(), 1);
        // A file holding another path's backup (a hash collision) is not restored.
        fs::copy(
            backups.file(&environment, "/tmp/a.txt"),
            backups.file(&environment, "/tmp/b.txt"),
        )
        .unwrap();
        assert!(backups.load(&environment, "/tmp/b.txt").is_none());
        assert_eq!(backups.list().len(), 1);
        backups.delete(&environment, "/tmp/a.txt");
        assert!(backups.load(&environment, "/tmp/a.txt").is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn backups_expire_after_thirty_days() {
        let day = 24 * 60 * 60;
        let now = 100 * day;
        assert!(!expired(now, now));
        assert!(!expired(now - 30 * day, now));
        assert!(expired(now - 30 * day - 1, now));
        // A clock set back keeps backups rather than deleting them.
        assert!(!expired(now + day, now));
        assert_eq!(age(now - 59, now), "less than a minute ago");
        assert_eq!(age(now - 60, now), "1 minute ago");
        assert_eq!(age(now - 2 * 3600, now), "2 hours ago");
        assert_eq!(age(now - 3 * day, now), "3 days ago");
    }

    #[test]
    fn a_restored_backup_warns_when_the_file_changed_since() {
        assert_eq!(
            restore_against(&version("a"), &version("a")),
            Restore::Current
        );
        assert_eq!(
            restore_against(&version("a"), &version("b")),
            Restore::Changed
        );
    }

    #[test]
    fn close_actions_cover_the_dirty_files_they_drop() {
        let pane = |id: &str| LayoutNode::Content {
            pane_id: PaneId::new(id),
            content: PaneContent {
                environment: ContentEnvironment::Host,
                root: "/work".into(),
                files: Vec::new(),
                active_file: None,
            },
        };
        let tab = |id: &str, layout| WorkspaceTab {
            id: TabId::new(id),
            label: id.into(),
            layout,
            previous_layout: None,
            merge: None,
        };
        let workspace = WorkspaceSnapshot {
            server_id: ServerId::new("server"),
            server_generation: ServerGeneration::new("generation"),
            revision: 1,
            initialized: true,
            sessions: vec![
                WorkspaceSession {
                    id: SessionId::new("one"),
                    label: "one".into(),
                    tabs: vec![
                        tab(
                            "split",
                            LayoutNode::Split {
                                axis: SplitAxis::Horizontal,
                                ratio: 0.5,
                                first: Box::new(LayoutNode::Pane {
                                    pane_id: PaneId::new("terminal"),
                                    surface_id: SurfaceId::new("surface"),
                                }),
                                second: Box::new(pane("left")),
                            },
                        ),
                        tab("single", pane("right")),
                    ],
                },
                WorkspaceSession {
                    id: SessionId::new("two"),
                    label: "two".into(),
                    tabs: vec![tab("other", pane("elsewhere"))],
                },
            ],
            surfaces: Vec::new(),
            recovery_message: None,
        };
        let dirty = [
            ("left", "/work/a.rs"),
            ("left", "/work/b.rs"),
            ("right", "/work/a.rs"),
            ("elsewhere", "/work/c.rs"),
        ];
        let affected = |scope: CloseScope| -> Vec<String> {
            dirty
                .iter()
                .filter(|(pane, path)| scope.covers(&PaneId::new(*pane), path))
                .map(|(pane, path)| format!("{pane}:{path}"))
                .collect()
        };
        let removing = |mutation| CloseScope::Panes(removed_panes(&workspace, &mutation));
        assert_eq!(
            affected(CloseScope::File {
                pane_id: PaneId::new("left"),
                path: "/work/a.rs".into()
            }),
            ["left:/work/a.rs"]
        );
        assert_eq!(
            affected(removing(WorkspaceMutation::RemovePane {
                pane_id: PaneId::new("left")
            })),
            ["left:/work/a.rs", "left:/work/b.rs"]
        );
        assert_eq!(
            affected(removing(WorkspaceMutation::RemoveTab {
                tab_id: TabId::new("single")
            })),
            ["right:/work/a.rs"]
        );
        assert_eq!(
            affected(CloseScope::Panes(tab_panes(
                &workspace,
                &TabId::new("split")
            ))),
            ["left:/work/a.rs", "left:/work/b.rs"]
        );
        assert_eq!(
            affected(removing(WorkspaceMutation::RemoveSession {
                session_id: SessionId::new("one")
            })),
            ["left:/work/a.rs", "left:/work/b.rs", "right:/work/a.rs"]
        );
        assert!(
            affected(removing(WorkspaceMutation::RemovePane {
                pane_id: PaneId::new("terminal")
            }))
            .is_empty()
        );
        assert_eq!(affected(CloseScope::Window).len(), dirty.len());
    }
}
