//! Native agent chats: the daemon runs each chat's agent like a shell and talks to it
//! over its standard input and output, one JSON message per line, through the agent's
//! driver (`Wire`): OMP speaks the Agent Client Protocol (`acp`), Claude Code its
//! headless JSON stream (`claude`), Codex its app server's JSON-RPC (`codex`). OpenCode
//! runs a local HTTP server instead (`opencode`), which says where it listens on its
//! output. One launcher starts every agent where the chat is (`launch`). The transcript
//! lives here; windows watch a chat (`WatchChat`) and get its state, then every change.
//!
//! A chat is keyed by its pane's chat ID and lives as long as a pane or view holds it:
//! once none does, its turn and harness stop (`ChatHub::retain`). Compi saves its
//! transcript as a thread (`threads`) until the thread is deleted
//! (`ChatHub::delete_thread`). The harness keeps its own session; watching a saved
//! thread again starts the harness on that session, after a restart of the daemon too,
//! and the transcript Compi saved stands in for the harness's replay.
use crate::program_status::ChatStatusHub;
use compi_protocol::chat::{
    ChatAgent, ChatAgentsFinished, ChatAnswers, ChatApproval, ChatChange, ChatContent,
    ChatDelivery, ChatEntry, ChatEvent, ChatHarness, ChatImage, ChatPicks, ChatQueued,
    ChatSessionInfo, ChatState, ChatStatus, ChatTool, ChatToolStatus, MAX_CHAT_ATTACHMENTS,
    MAX_CHAT_IMAGE_BYTES, MAX_CHAT_IMAGES, MAX_CHAT_PROMPT_BYTES, MAX_CHAT_QUEUED, ThreadActivity,
    ThreadInfo,
};
use compi_protocol::program_status::{BlockedKind, ProgramState, ProgramStatusRecord};
use compi_protocol::{ContentEnvironment, LayoutNode, SurfaceId, WorkspaceSnapshot};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::ops::{Deref, DerefMut};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod acp;
mod claude;
mod codex;
mod launch;
mod opencode;
mod saved;
mod threads;

use acp::Acp;
use claude::Claude;
use codex::Codex;
use launch::{Arg, Prepared};
use opencode::OpenCode;
use threads::{ThreadRecord, ThreadStore};

/// Standard error kept to explain an exit.
const STDERR_TAIL_BYTES: usize = 2048;
/// How long a stopping harness may take to exit on its own once its input closes.
const STOP_GRACE: Duration = Duration::from_millis(500);
/// How long a window waits for the harness to list its sessions.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// A whole state sent to a window stays this small; tool output and diffs of the oldest
/// calls are left out beyond it, so a long session still fits one control frame.
const WATCH_STATE_BYTES: usize = 6 * 1024 * 1024;
/// The longest line a sub-agent's activity keeps, in characters.
const AGENT_ACTIVITY_CHARS: usize = 160;
#[cfg(windows)]
pub(crate) const WSL_EXE: &str = r"C:\Windows\System32\wsl.exe";

/// Every chat this daemon runs.
#[derive(Default)]
pub struct ChatHub {
    chats: Mutex<HashMap<String, Arc<Chat>>>,
    status: ChatStatusHub,
    /// The daemon's instance, which harnesses get as a terminal's shell does.
    instance: OnceLock<Option<String>>,
    threads: Arc<ThreadStore>,
}

/// The daemon's chats.
pub fn hub() -> &'static ChatHub {
    static HUB: LazyLock<ChatHub> = LazyLock::new(ChatHub::default);
    &HUB
}

impl ChatHub {
    /// Names the daemon's instance for the harnesses started from now on, and keeps its
    /// threads apart from other instances'.
    pub fn set_instance(&self, instance: Option<&str>) {
        let _ = self.instance.set(instance.map(str::to_owned));
        self.threads.open(instance);
    }

    /// The chat's whole state and a receiver of its later changes, starting the chat if it
    /// is not running: on its saved thread, its session resumed, when there is one.
    /// `approval` applies from the harness's next start, this one when it starts it; so
    /// does `surface`, the terminal whose view shows the chat, if one does. `picks` apply
    /// to the next new session, this one when it starts one.
    pub fn watch(
        &self,
        content: &ChatContent,
        approval: ChatApproval,
        picks: ChatPicks,
        surface: Option<SurfaceId>,
    ) -> (ChatState, mpsc::Receiver<ChatEvent>) {
        let running = lock(&self.chats).get(&content.id).cloned();
        let chat = match running {
            Some(chat) => chat,
            None => {
                let thread = self.threads.load(&content.id);
                lock(&self.chats)
                    .entry(content.id.clone())
                    .or_insert_with(|| {
                        let instance = self.instance.get().cloned().flatten();
                        Chat::start(
                            content.clone(),
                            instance,
                            approval,
                            picks.clone(),
                            surface.clone(),
                            thread,
                        )
                    })
                    .clone()
            }
        };
        let mut inner = chat.lock();
        inner.approval = approval;
        inner.picks = picks;
        inner.show_on(surface);
        let (sender, receiver) = mpsc::channel();
        inner.watchers.push(sender);
        (fit(inner.state.clone(), WATCH_STATE_BYTES), receiver)
    }

    /// Program status of every chat.
    pub fn program_status(&self) -> &ChatStatusHub {
        &self.status
    }

    /// The user has seen the chat's result: it no longer shows as done or failed.
    pub fn acknowledge(&self, id: &str) {
        if let Ok(chat) = self.get(id) {
            chat.lock().unseen = None;
        }
    }

    pub fn send(
        &self,
        id: &str,
        text: String,
        files: Vec<String>,
        images: Vec<ChatImage>,
        delivery: ChatDelivery,
    ) -> Result<(), String> {
        let prompt = Prompt {
            text,
            files,
            images,
        };
        self.get(id)?.send(prompt, delivery)
    }

    pub fn remove_queued(&self, id: &str, queued: u64) -> Result<(), String> {
        self.get(id)?.remove_queued(queued)
    }

    pub fn edit_queued(&self, id: &str, queued: u64, text: String) -> Result<(), String> {
        self.get(id)?.lock().edit_queued(queued, text)
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        self.get(id)?.cancel();
        Ok(())
    }

    pub fn answer(&self, id: &str, permission: u64, option: Option<String>) -> Result<(), String> {
        self.get(id)?.answer(permission, option)
    }

    pub fn answer_question(
        &self,
        id: &str,
        question: u64,
        answers: Option<ChatAnswers>,
    ) -> Result<(), String> {
        self.get(id)?.answer_question(question, answers)
    }

    pub fn set_option(&self, id: &str, option: String, value: String) -> Result<(), String> {
        self.get(id)?.set_option(option, value)
    }

    /// Blocks until the harness lists its sessions, at most `LIST_TIMEOUT`: newest first,
    /// with the first prompt and last reply its files hold.
    pub fn sessions(&self, id: &str) -> Result<Vec<ChatSessionInfo>, String> {
        let chat = self.get(id)?;
        let receiver = chat.list_sessions()?;
        let mut sessions = match receiver.recv_timeout(LIST_TIMEOUT) {
            Ok(result) => result?,
            Err(_) => return Err("the harness did not list its sessions in time".into()),
        };
        // RFC 3339 times in UTC, as every agent's listing gives them, sort as text.
        sessions.sort_by(|a, b| b.updated.cmp(&a.updated));
        let fill = lock(&chat.inner).driver.wire().fill_sessions;
        fill(&chat.content, &mut sessions);
        Ok(sessions)
    }

    pub fn resume(
        &self,
        id: &str,
        session: Option<String>,
        approval: ChatApproval,
        picks: ChatPicks,
    ) -> Result<(), String> {
        let chat = self.get(id)?;
        {
            let mut inner = chat.lock();
            inner.approval = approval;
            inner.picks = picks;
        }
        chat.resume(session)
    }

    pub fn restart(
        &self,
        id: &str,
        approval: ChatApproval,
        picks: ChatPicks,
    ) -> Result<(), String> {
        let chat = self.get(id)?;
        {
            let mut inner = chat.lock();
            inner.approval = approval;
            inner.picks = picks;
        }
        chat.restart();
        Ok(())
    }

    /// Notes which chats a pane or view of `workspace` holds, and the terminal each shows
    /// in for its harness's next start. A chat none holds ends at once: its running turn
    /// stops, its harness stops, its status goes and its thread is saved.
    pub fn retain(&self, workspace: &WorkspaceSnapshot) {
        let chats: Vec<(String, Arc<Chat>)> = lock(&self.chats)
            .iter()
            .map(|(id, chat)| (id.clone(), chat.clone()))
            .collect();
        if chats.is_empty() {
            return;
        }
        let mut held = held_chats(workspace);
        let mut ended = Vec::new();
        for (id, chat) in chats {
            match held.remove(&id) {
                Some(surface) => chat.lock().show_on(surface),
                None => ended.push((id, chat)),
            }
        }
        if ended.is_empty() {
            return;
        }
        let mut chats = lock(&self.chats);
        for (id, chat) in ended {
            if chats.get(&id).is_some_and(|kept| Arc::ptr_eq(kept, &chat)) {
                chats.remove(&id);
                if let Some(record) = chat.end() {
                    self.threads.save(record);
                }
            }
        }
    }

    /// Every thread kept, newest change first, with what runs in it and whether a pane or
    /// view of `workspace` holds it.
    pub fn threads(&self, workspace: &WorkspaceSnapshot) -> Vec<ThreadInfo> {
        let held = held_chats(workspace);
        let mut threads: Vec<ThreadInfo> = self
            .threads
            .list()
            .into_iter()
            .map(|mut stored| {
                let id = stored.chat.id.clone();
                let mut activity = ThreadActivity::Idle;
                let running = lock(&self.chats).get(&id).cloned();
                if let Some(chat) = running {
                    let inner = lock(&chat.inner);
                    activity = inner.activity();
                    stored.chat.session = inner.state.session.clone();
                    stored.title =
                        threads::thread_title(inner.state.title.as_deref(), &inner.state.entries);
                    if let Some(times) = inner.thread {
                        stored.updated_ms = times.updated_ms;
                    }
                }
                ThreadInfo {
                    project: crate::project::project_of(
                        &stored.chat.environment,
                        &stored.chat.folder,
                    ),
                    open: held.contains_key(&id),
                    chat: stored.chat,
                    title: stored.title,
                    created_ms: stored.created_ms,
                    updated_ms: stored.updated_ms,
                    activity,
                }
            })
            .collect();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_ms));
        threads
    }

    /// Deletes thread `id`: its harness stops and its transcript goes. A thread a pane or
    /// view of `workspace` holds is kept.
    pub fn delete_thread(&self, id: &str, workspace: &WorkspaceSnapshot) -> Result<(), String> {
        if held_chats(workspace).contains_key(id) {
            return Err("Close its pane first".into());
        }
        let running = lock(&self.chats).remove(id);
        if running.is_none() && !self.threads.contains(id) {
            return Err(format!("there is no thread {id}"));
        }
        if let Some(chat) = running {
            chat.close();
        }
        self.threads.delete(id);
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Arc<Chat>, String> {
        lock(&self.chats)
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no chat {id} is running; watch it first"))
    }
}

/// The terminal whose view shows chat `id` in `workspace`; `None` when a content pane
/// shows it, or nothing does.
pub fn chat_surface(workspace: &WorkspaceSnapshot, id: &str) -> Option<SurfaceId> {
    held_chats(workspace).remove(id).flatten()
}

/// The harnesses installed in `environment`, in `ChatHarness::ALL` order, as their
/// launch finds them. They are looked for at once, each for a few seconds at most.
pub fn installed_agents(environment: &ContentEnvironment) -> Vec<ChatHarness> {
    let lookups: Vec<_> = ChatHarness::ALL
        .into_iter()
        .map(|harness| {
            let environment = environment.clone();
            let lookup = thread::Builder::new()
                .name("compi-chat-lookup".into())
                .spawn(move || launch::installed(&environment, harness));
            (harness, lookup)
        })
        .collect();
    lookups
        .into_iter()
        .filter_map(|(harness, lookup)| lookup.ok()?.join().ok()?.then_some(harness))
        .collect()
}

/// Where every agent is installed on this PC, for Settings → Agents: its Windows program
/// and its Linux one (in the default WSL distribution on Windows), each as a chat's
/// launch finds it, with its version. Every program is looked for at once, each for a few
/// seconds at most.
pub fn agent_installs() -> compi_protocol::chat::AgentInstalls {
    type Lookup = std::io::Result<thread::JoinHandle<Option<compi_protocol::chat::AgentProgram>>>;
    fn spawn(
        find: impl FnOnce() -> Option<compi_protocol::chat::AgentProgram> + Send + 'static,
    ) -> Lookup {
        thread::Builder::new()
            .name("compi-chat-lookup".into())
            .spawn(find)
    }
    let windows: Vec<Lookup> = ChatHarness::ALL
        .into_iter()
        .map(|harness| spawn(move || launch::windows_install(harness)))
        .collect();
    let distribution = launch::default_distribution();
    let linux: Vec<Lookup> = ChatHarness::ALL
        .into_iter()
        .map(|harness| {
            let distribution = distribution.clone();
            spawn(move || launch::linux_install(harness, distribution.as_deref()))
        })
        .collect();
    let join = |lookup: Lookup| lookup.ok()?.join().ok()?;
    compi_protocol::chat::AgentInstalls {
        distribution,
        agents: ChatHarness::ALL
            .into_iter()
            .zip(windows.into_iter().zip(linux))
            .map(
                |(harness, (windows, linux))| compi_protocol::chat::AgentInstall {
                    harness,
                    windows: join(windows),
                    linux: join(linux),
                },
            )
            .collect(),
    }
}

/// Every chat a pane or view of `workspace` holds, with the terminal whose view shows
/// it, when a terminal's view does.
fn held_chats(workspace: &WorkspaceSnapshot) -> HashMap<String, Option<SurfaceId>> {
    let mut held = HashMap::new();
    for tab in workspace.sessions.iter().flat_map(|session| &session.tabs) {
        chat_leaves(&tab.layout, &mut held);
    }
    held
}

fn chat_leaves(node: &LayoutNode, output: &mut HashMap<String, Option<SurfaceId>>) {
    match node {
        LayoutNode::Split { first, second, .. } => {
            chat_leaves(first, output);
            chat_leaves(second, output);
        }
        leaf => {
            if let Some(chat) = leaf.content().and_then(|content| content.chat()) {
                output.insert(chat.id.clone(), leaf.surface_id().cloned());
            }
        }
    }
}

/// `state` within `budget` bytes of JSON: the oldest tool calls lose their output, diffs,
/// input and their sub-agents' results, tasks and steps until it fits.
fn fit(mut state: ChatState, budget: usize) -> ChatState {
    let size = |state: &ChatState| serde_json::to_vec(state).map_or(0, |bytes| bytes.len());
    let mut over = size(&state).saturating_sub(budget);
    for index in 0..state.entries.len() {
        if over == 0 {
            break;
        }
        if let ChatEntry::Tool(tool) = &mut state.entries[index] {
            // An estimate of what this call's JSON loses; the total is measured again
            // once the estimate says it fits.
            let mut freed = tool.output.take().map_or(0, |text| text.len());
            freed += tool.input.take().map_or(0, |text| text.len());
            for diff in &mut tool.diffs {
                freed += diff.old_text.take().map_or(0, |text| text.len());
                freed += diff.new_text.take().map_or(0, |text| text.len());
            }
            for agent in &mut tool.agents {
                freed += agent.result.take().map_or(0, |text| text.len());
                freed += agent.assignment.take().map_or(0, |text| text.len());
                freed += std::mem::take(&mut agent.steps)
                    .iter()
                    .map(|step| step.text.len())
                    .sum::<usize>();
            }
            over = over.saturating_sub(freed);
            if over == 0 {
                over = size(&state).saturating_sub(budget);
            }
        }
    }
    state
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// `time` in RFC 3339, in UTC, to the second.
fn rfc3339(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Howard Hinnant's days-to-civil, for days since 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

struct Chat {
    content: ChatContent,
    /// The daemon's instance, as `COMPI_INSTANCE` names it.
    instance: Option<String>,
    inner: Mutex<Inner>,
}

/// A prompt on its way to the harness: its text, attached files and images.
struct Prompt {
    text: String,
    files: Vec<String>,
    images: Vec<ChatImage>,
}

impl Prompt {
    fn check(&self) -> Result<(), String> {
        if self.text.trim().is_empty() && self.files.is_empty() && self.images.is_empty() {
            return Err("nothing to send".into());
        }
        if self.text.len() > MAX_CHAT_PROMPT_BYTES {
            return Err(format!(
                "a prompt holds at most {MAX_CHAT_PROMPT_BYTES} bytes"
            ));
        }
        if self.files.len() > MAX_CHAT_ATTACHMENTS {
            return Err(format!(
                "a prompt attaches at most {MAX_CHAT_ATTACHMENTS} files"
            ));
        }
        if self.images.len() > MAX_CHAT_IMAGES {
            return Err(format!("a prompt carries at most {MAX_CHAT_IMAGES} images"));
        }
        // Base64 is four bytes for three.
        if let Some(image) = self
            .images
            .iter()
            .find(|image| image.data.len() > MAX_CHAT_IMAGE_BYTES.div_ceil(3) * 4)
        {
            return Err(format!("{} is too large to send", image.name));
        }
        Ok(())
    }

    fn image_names(&self) -> Vec<String> {
        self.images.iter().map(|image| image.name.clone()).collect()
    }

    /// As the transcript shows it, once the harness has it at `sent_ms`.
    fn entry(&self, sent_ms: u64) -> ChatEntry {
        ChatEntry::User {
            text: self.text.clone(),
            files: self.files.clone(),
            images: self.image_names(),
            sent_ms: Some(sent_ms),
            ended_ms: None,
        }
    }
}

/// The names of the model and thinking level a session has, when it has them.
#[derive(Clone)]
struct PromptSettings {
    model: Option<String>,
    thought_level: Option<String>,
}

struct FinishedAgentWave {
    entry: usize,
    tools: Vec<usize>,
}

/// The wire code of the chat's harness and what it keeps of it, chosen by
/// `ChatContent::harness`. Adding an agent adds its variant here and its `Wire`.
enum Driver {
    /// OMP: the Agent Client Protocol.
    Acp(Box<Acp>),
    /// Claude Code: its headless JSON stream.
    Claude(Box<Claude>),
    /// Codex: its app server's JSON-RPC.
    Codex(Box<Codex>),
    /// OpenCode: its local HTTP server and event stream.
    OpenCode(Box<OpenCode>),
}

impl Default for Driver {
    fn default() -> Self {
        Self::Acp(Box::default())
    }
}

impl Driver {
    fn new(harness: ChatHarness) -> Self {
        match harness {
            ChatHarness::Omp => Self::default(),
            ChatHarness::Claude => Self::Claude(Box::default()),
            ChatHarness::Codex => Self::Codex(Box::default()),
            ChatHarness::OpenCode => Self::OpenCode(Box::default()),
        }
    }

    /// What the core asks of this driver.
    fn wire(&self) -> &'static Wire {
        match self {
            Self::Acp(_) => &acp::WIRE,
            Self::Claude(_) => &claude::WIRE,
            Self::Codex(_) => &codex::WIRE,
            Self::OpenCode(_) => &opencode::WIRE,
        }
    }
}

/// A sessions listing's answer, which a window waits for.
type SessionsReply = mpsc::Receiver<Result<Vec<ChatSessionInfo>, String>>;

/// Everything the chat core asks of a driver, the same for every agent: each driver
/// module has one (`WIRE`). The core keeps the transcript, the queue, the thread, the
/// status and the process; a driver turns its agent's messages into `ChatEvent`s
/// through the core's `Inner` calls and Compi's requests into its agent's messages.
struct Wire {
    /// The agent's own approval mode, for an agent that does not read its setting by
    /// itself (OMP's `tools.approvalMode`), looked up before it starts on a thread of
    /// its own, no lock held; `None` when it reads its own.
    configured_approval: fn(&ChatContent) -> Option<&'static str>,
    /// The program's arguments, with the approval mode Compi chose, when it chose one.
    launch_args: fn(&mut Inner, Option<&'static str>) -> Vec<Arg>,
    /// Variables the program starts with besides Compi's shell context, asked after
    /// `launch_args` (OpenCode's server password).
    environment: fn(&Inner) -> Vec<(&'static str, String)>,
    /// Starts talking to the program once it runs.
    start: fn(&mut Inner),
    /// One message the program wrote.
    handle: fn(&Arc<Chat>, &mut Inner, Value),
    /// A line the program wrote that is no message (OpenCode says where its server
    /// listens so); it also goes with standard error to explain an exit.
    line: fn(&Arc<Chat>, &mut Inner, &str),
    /// The agent opens its session with the first prompt (Claude Code), rather than
    /// before any (`ChatState::session` set once it has).
    session_with_first_prompt: bool,
    /// Starts a turn on `prompt`, which the transcript already shows.
    prompt: fn(&mut Inner, &Prompt),
    /// Whether a prompt can go into the running turn now, and how it goes (OMP's
    /// steering); `None` for an agent that takes prompts only as turns of their own:
    /// they queue.
    steer: Option<(CanSteerFn, SteerFn)>,
    /// Stops the running turn: its open permission and question go unanswered.
    cancel: fn(&mut Inner),
    /// Asks the program to stop the running turn, nothing else.
    interrupt: fn(&mut Inner),
    /// Answers the permission `state.permission` shows, and shows the next one waiting.
    answer: fn(&mut Inner, Option<String>) -> Result<(), String>,
    /// Answers the question `state.question` shows, and shows the next one waiting.
    answer_question: fn(&mut Inner, Option<ChatAnswers>) -> Result<(), String>,
    /// Sets the session's setting to one of its choices.
    set_option: fn(&Arc<Chat>, &mut Inner, String, String) -> Result<(), String>,
    /// The agent's sessions in the chat's folder.
    list_sessions: fn(&Chat, &mut Inner) -> Result<SessionsReply, String>,
    /// Adds what the agent's files tell of each listed session (first prompt, last
    /// reply) when its listing lacks it.
    fill_sessions: fn(&ChatContent, &mut [ChatSessionInfo]),
    /// Opens a session, or a new one, in the running program, for an agent that can
    /// switch sessions (OMP); one that can't starts again on it.
    switch_session: Option<fn(&Chat, &mut Inner, Option<String>)>,
    /// Tells the program nobody will answer its open permission or question.
    cancel_asks: fn(&mut Inner),
    /// Forgets the open permission and question, which the program no longer waits on.
    forget_asks: fn(&mut Inner),
    /// Windows waiting on the program hear it is gone; nothing it was asked is out any
    /// more.
    fail_pending: fn(&mut Inner),
    /// The transcript was replaced: entries the driver points at are gone.
    forget_transcript: fn(&mut Inner),
}

/// Hands a prompt to the running turn (`Wire::steer`), naming the harness in errors.
type SteerFn = fn(&mut Inner, &str, Prompt) -> Result<(), String>;
/// Whether a prompt can go into the running turn now (`Wire::steer`).
type CanSteerFn = fn(&Inner) -> bool;

#[derive(Default)]
struct Inner {
    /// The chat's ID, which its program status goes under.
    chat_id: String,
    state: ChatState,
    /// How the harness asks before running tools, from its next start.
    approval: ChatApproval,
    /// The settings a new session starts on, from its next one.
    picks: ChatPicks,
    /// The names of the model and thinking level in effect at the session's previous
    /// prompt; `None` before its first.
    prompted_with: Option<PromptSettings>,
    /// How the last turn or the harness ended, until the user has seen it.
    unseen: Option<Unseen>,
    /// The program status record last published.
    published: Option<ProgramStatusRecord>,
    watchers: Vec<mpsc::Sender<ChatEvent>>,
    agent: Option<Agent>,
    /// Bumped on every start, so a stopped harness's reader changes nothing.
    generation: u64,
    closed: bool,
    /// The harness answered `initialize`.
    initialized: bool,
    driver: Driver,
    /// Prompts for after the running turn, in order, by ID; `state.queued` shows them.
    queue: VecDeque<(u64, Prompt)>,
    next_queued: u64,
    /// A turn that did not end on its own holds the queue until a prompt is sent or
    /// queued.
    queue_held: bool,
    /// Tool call IDs to their transcript entries.
    tools: HashMap<String, usize>,
    /// Entries of the tool calls whose sub-agents ran since none did; once none of them
    /// runs, the transcript gets a summary.
    agent_wave: Vec<usize>,
    /// Calls already included in a finished wave and the card to refresh on late reports.
    agents_summarized: HashMap<usize, Arc<FinishedAgentWave>>,
    /// Bumped when the transcript resets, even without restarting the harness.
    agents_revision: u64,
    /// Parent-file bytes already consumed, retained between waves of agents.
    agents_offset: u64,
    /// The harness generation and transcript revision watched by the file reader.
    agents_watched: Option<(u64, u64)>,
    next_permission: u64,
    next_question: u64,
    /// When the transcript last moved (a prompt, a reply chunk, a tool report): a thought
    /// that follows began then, since the harness sends its text only once it is done.
    activity_ms: u64,
    /// The terminal whose view shows the chat, which the harness's shell context names.
    surface: Option<SurfaceId>,
    /// The chat's thread, once it has one: when it began and when its transcript last
    /// changed.
    thread: Option<ThreadTimes>,
    /// The thread changed since it was last saved.
    save_due: bool,
    /// The transcript is Compi's own, so the session's replay while it loads is left out.
    skip_replay: bool,
    /// The chat's environment and folder; a turn's end marks its project's files changed.
    folder: Option<(ContentEnvironment, String)>,
    /// The running program, as its launch reports it: a Windows one, and the chat's
    /// folder as it names it. Until it says, the chat's folder.
    started: Option<launch::Started>,
    /// What the running program is told at start: the Linux program's note and the
    /// Windows program's (WSL chats).
    notes: (Option<String>, Option<String>),
}

/// When a thread began, and when its transcript last changed.
#[derive(Clone, Copy)]
struct ThreadTimes {
    created_ms: u64,
    updated_ms: u64,
}

/// What the user has not seen yet of a chat that waits.
#[derive(Debug, PartialEq)]
enum Unseen {
    Done,
    /// The turn failed, or the harness did; the message the transcript or status shows.
    Error(String),
}

/// A chat's locked state. Letting go of it publishes the chat's program status, so
/// watchers see where the changes made under the lock ended, never a step on the way,
/// and hands its thread to the store when it changed.
struct Locked<'a> {
    inner: MutexGuard<'a, Inner>,
    content: &'a ChatContent,
}

impl Deref for Locked<'_> {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        self.inner.publish_status();
        if let Some(record) = self.inner.thread_to_save(self.content) {
            hub().threads.save(record);
        }
    }
}

struct Agent {
    child: Child,
    input: ChildStdin,
    stderr: Arc<Mutex<String>>,
    #[cfg(windows)]
    _job: Option<std::os::windows::io::OwnedHandle>,
}

impl Chat {
    fn start(
        content: ChatContent,
        instance: Option<String>,
        approval: ChatApproval,
        picks: ChatPicks,
        surface: Option<SurfaceId>,
        thread: Option<ThreadRecord>,
    ) -> Arc<Self> {
        let mut inner = Inner {
            chat_id: content.id.clone(),
            state: ChatState {
                harness: content.harness,
                session: content.session.clone(),
                ..ChatState::default()
            },
            approval,
            picks,
            surface,
            folder: Some((content.environment.clone(), content.folder.clone())),
            driver: Driver::new(content.harness),
            ..Inner::default()
        };
        if let Some(thread) = thread {
            inner.seed(thread);
        }
        let chat = Arc::new(Self {
            inner: Mutex::new(inner),
            content,
            instance,
        });
        chat.spawn(&mut chat.lock());
        chat
    }

    fn lock(&self) -> Locked<'_> {
        Locked {
            inner: lock(&self.inner),
            content: &self.content,
        }
    }

    /// Starts the harness as the chat's approval says. What it needs first is done on a
    /// thread of its own, no lock held, the status `Starting` meanwhile: where its
    /// programs are, Compi's shell bridge and agent shell, the notes (`launch::prepare`),
    /// and, at the same time, the agent's own approval setting when it does not read it
    /// itself (OMP).
    fn spawn(self: &Arc<Self>, inner: &mut Inner) {
        inner.generation += 1;
        inner.initialized = false;
        let generation = inner.generation;
        let chosen = self.content.harness.approval_mode(inner.approval);
        let configured = inner.driver.wire().configured_approval;
        let chat = self.clone();
        let preparing = thread::Builder::new()
            .name("compi-chat-start".into())
            .spawn(move || {
                let (prepared, mode) = thread::scope(|scope| {
                    let lookup = chosen
                        .is_none()
                        .then(|| scope.spawn(|| configured(&chat.content)));
                    let prepared = launch::prepare(&chat.content);
                    let mode = chosen.or_else(|| lookup?.join().ok().flatten());
                    (prepared, mode)
                });
                let mut inner = chat.lock();
                if !inner.closed && inner.generation == generation {
                    chat.launch(&mut inner, generation, mode, &prepared);
                }
            });
        if preparing.is_err() {
            self.launch(inner, generation, chosen, &launch::programs(&self.content));
        }
    }

    /// Runs the harness, with `approval_mode` when there is one, and starts talking to
    /// it; the rest follows its answers. It gets the shell context a terminal in its
    /// folder gets, so `compi` there acts on this workspace, and its note, when there is
    /// one, at the end of its system prompt.
    fn launch(
        self: &Arc<Self>,
        inner: &mut Inner,
        generation: u64,
        approval_mode: Option<&'static str>,
        prepared: &Prepared,
    ) {
        inner.emit(ChatEvent::ApprovalMode {
            mode: approval_mode.map(str::to_owned),
        });
        let wire = inner.driver.wire();
        let mut args = (wire.launch_args)(inner, approval_mode);
        // A bridge file only where the bridge is.
        if !prepared.bridge {
            args.retain(|arg| !matches!(arg, Arg::Bridge { .. }));
        }
        inner.notes = (prepared.linux_note.clone(), prepared.windows_note.clone());
        inner.started = None;
        let own = (wire.environment)(inner);
        let wsl = prepared.runs_in_wsl(&self.content);
        let mut child = match crate::shell_integration::cli_environment(
            inner.surface.as_ref().map(SurfaceId::as_str),
            self.instance.as_deref(),
            wsl,
        )
        .map_err(|error| format!("Couldn't prepare Compi's shell context: {error}"))
        .and_then(|mut environment| {
            environment.extend(own);
            launch::command(&self.content, &environment, &args, prepared)
        })
        .and_then(|(mut command, started)| {
            inner.started = started;
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| self.spawn_error(&error))
        }) {
            Ok(child) => child,
            Err(message) => {
                inner.emit(ChatEvent::Status {
                    status: ChatStatus::Failed { message },
                });
                return;
            }
        };
        #[cfg(windows)]
        let job = unsafe { crate::conpty::create_kill_on_close_job(&child) }.ok();
        let output = child.stdout.take().expect("stdout is piped");
        let errors = child.stderr.take().expect("stderr is piped");
        let input = child.stdin.take().expect("stdin is piped");
        let stderr = Arc::new(Mutex::new(String::new()));
        {
            let stderr = stderr.clone();
            let _ = thread::Builder::new()
                .name("compi-chat-stderr".into())
                .spawn(move || keep_tail(errors, &stderr));
        }
        {
            let (chat, tail) = (self.clone(), stderr.clone());
            let _ = thread::Builder::new()
                .name("compi-chat".into())
                .spawn(move || chat.read(output, generation, tail));
        }
        inner.agent = Some(Agent {
            child,
            input,
            stderr,
            #[cfg(windows)]
            _job: job,
        });
        (wire.start)(inner);
    }

    fn spawn_error(&self, error: &std::io::Error) -> String {
        if error.kind() == std::io::ErrorKind::NotFound {
            not_installed(&self.content)
        } else {
            format!(
                "Couldn't start {} in {}: {error}",
                self.content.harness.name(),
                self.content.folder
            )
        }
    }

    /// Reads the harness's messages until its output ends, then reports why it stopped.
    /// Lines that are not messages (a launcher's own error, `wsl.exe`'s included) go to
    /// the driver (`Wire::line`) and to `tail` with its standard error, to explain an
    /// exit.
    fn read(self: Arc<Self>, output: impl Read, generation: u64, tail: Arc<Mutex<String>>) {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(started) = launch::started(line) {
                let mut inner = self.lock();
                if inner.closed || inner.generation != generation {
                    return;
                }
                inner.started = Some(started);
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(line) else {
                push_tail(&tail, line);
                let mut inner = self.lock();
                if inner.closed || inner.generation != generation {
                    return;
                }
                (inner.driver.wire().line)(&self, &mut inner, line);
                continue;
            };
            let mut inner = self.lock();
            if inner.closed || inner.generation != generation {
                return;
            }
            (inner.driver.wire().handle)(&self, &mut inner, message);
        }
        let mut inner = self.lock();
        if inner.closed || inner.generation != generation {
            return;
        }
        let Some(mut agent) = inner.agent.take() else {
            return;
        };
        let code = agent.child.wait().ok().and_then(|status| status.code());
        let tail = lock(&agent.stderr).trim().to_owned();
        let message = if code == Some(127) {
            not_installed(&self.content)
        } else {
            let mut message = format!("{} stopped", self.content.harness.name());
            if let Some(code) = code {
                message.push_str(&format!(" (exit code {code})"));
            }
            if !tail.is_empty() {
                message.push_str(": ");
                message.push_str(last_lines(&tail, 6));
            }
            message
        };
        inner.fail_pending();
        inner.stop_agents();
        inner.end_turn();
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Failed { message },
        });
    }

    /// Whether the harness takes a prompt now: it waits for one, and has a session to
    /// take it to when its prompts go to one.
    fn ready(&self, inner: &Inner) -> Result<(), String> {
        let harness = self.content.harness.name();
        match &inner.state.status {
            ChatStatus::Ready => {}
            ChatStatus::Starting => return Err(format!("{harness} is still starting")),
            ChatStatus::Working { .. } => return Err(format!("{harness} is still working")),
            ChatStatus::Failed { .. } => return Err(format!("{harness} isn't running")),
        }
        if !inner.has_session() {
            return Err(format!("{harness} has no session yet"));
        }
        Ok(())
    }

    /// Hands `prompt` to the harness as `delivery` says.
    fn send(self: &Arc<Self>, prompt: Prompt, delivery: ChatDelivery) -> Result<(), String> {
        prompt.check()?;
        let harness = self.content.harness.name();
        let mut inner = self.lock();
        let steer = delivery == ChatDelivery::Steer;
        // An agent that takes prompts only as turns of their own queues a steer.
        let steer_now = inner
            .driver
            .wire()
            .steer
            .filter(|(can, _)| can(&inner))
            .map(|(_, send)| send);
        if let Some(send) = steer_now.filter(|_| steer && inner.state.working()) {
            send(&mut inner, harness, prompt)?;
        } else if delivery == ChatDelivery::Prompt
            || (steer && matches!(inner.state.status, ChatStatus::Ready))
        {
            self.ready(&inner)?;
            inner.start_prompt(prompt);
        } else {
            if let ChatStatus::Failed { .. } = inner.state.status {
                return Err(format!("{harness} isn't running"));
            }
            if inner.queue.len() >= MAX_CHAT_QUEUED {
                return Err(format!("at most {MAX_CHAT_QUEUED} prompts wait at once"));
            }
            inner.enqueue(prompt, false);
            inner.queue_held = false;
            inner.send_queued();
        }
        Ok(())
    }

    fn remove_queued(&self, queued: u64) -> Result<(), String> {
        let mut inner = self.lock();
        let Some(index) = inner.queue.iter().position(|(id, _)| *id == queued) else {
            return Err("that prompt was already sent".into());
        };
        inner.queue.remove(index);
        inner.publish_queue();
        Ok(())
    }

    fn cancel(&self) {
        let mut inner = self.lock();
        if !inner.state.working() {
            return;
        }
        (inner.driver.wire().cancel)(&mut inner);
    }

    fn answer(&self, permission: u64, option: Option<String>) -> Result<(), String> {
        let mut inner = self.lock();
        if inner
            .state
            .permission
            .as_ref()
            .is_none_or(|shown| shown.id != permission)
        {
            return Err("that permission was already answered".into());
        }
        // It shows the permission or question waiting next, if one does.
        (inner.driver.wire().answer)(&mut inner, option)
    }

    fn answer_question(&self, question: u64, answers: Option<ChatAnswers>) -> Result<(), String> {
        let mut inner = self.lock();
        if inner
            .state
            .question
            .as_ref()
            .is_none_or(|shown| shown.id != question)
        {
            return Err("that question was already answered".into());
        }
        (inner.driver.wire().answer_question)(&mut inner, answers)
    }

    fn set_option(self: &Arc<Self>, option: String, value: String) -> Result<(), String> {
        let mut inner = self.lock();
        let Some(shown) = inner.state.options.iter().find(|shown| shown.id == option) else {
            return Err(format!("the harness has no setting {option}"));
        };
        if !shown.choices.iter().any(|choice| choice.value == value) {
            return Err(format!("{value} is not a choice of {}", shown.name));
        }
        (inner.driver.wire().set_option)(self, &mut inner, option, value)
    }

    fn list_sessions(&self) -> Result<SessionsReply, String> {
        let mut inner = self.lock();
        if !inner.initialized || inner.agent.is_none() {
            return Err(format!("{} isn't running", self.content.harness.name()));
        }
        (inner.driver.wire().list_sessions)(self, &mut inner)
    }

    /// Shows `session`, its history replayed, or a new session, in the running harness
    /// when it can switch sessions (`Wire::switch_session`); a harness that isn't
    /// running, or can't, starts again on it.
    fn resume(self: &Arc<Self>, session: Option<String>) -> Result<(), String> {
        compi_protocol::chat::validate_session(session.as_deref())?;
        let mut inner = self.lock();
        if inner.state.working() {
            return Err("stop the running turn first".into());
        }
        let state = ChatState {
            harness: self.content.harness,
            session: session.clone(),
            thread: inner.thread.is_some(),
            ..ChatState::default()
        };
        let switch = inner.driver.wire().switch_session;
        match switch.filter(|_| inner.initialized && inner.agent.is_some()) {
            Some(switch) => {
                inner.cancel_requests();
                inner.emit(ChatEvent::Reset {
                    state: Box::new(state),
                });
                switch(self, &mut inner, session);
            }
            None => {
                inner.stop();
                inner.emit(ChatEvent::Reset {
                    state: Box::new(state),
                });
                self.spawn(&mut inner);
            }
        }
        Ok(())
    }

    /// Ends the harness and starts it again; a chat with a session resumes it, except a
    /// draft, which has nothing to resume and starts on a new session. The transcript
    /// stays as it is, the session's replay left out: what the harness left open ends as
    /// at a turn's end, and the queue empties.
    fn restart(self: &Arc<Self>) {
        let mut inner = self.lock();
        if inner.thread.is_none() && inner.state.entries.is_empty() && inner.state.session.is_some()
        {
            inner.emit(ChatEvent::Session { session: None });
        }
        inner.stop();
        inner.stop_agents();
        inner.end_turn();
        inner.agents_summarized.clear();
        inner.agents_revision += 1;
        inner.agents_watched = None;
        inner.unseen = None;
        inner.prompted_with = None;
        inner.queue_held = false;
        if !inner.queue.is_empty() {
            inner.queue.clear();
            inner.publish_queue();
        }
        inner.skip_replay = true;
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Starting,
        });
        self.spawn(&mut inner);
    }

    /// Ends the chat for good: its harness stops and its status goes. Its thread is
    /// saved no more.
    fn close(&self) {
        let mut inner = self.lock();
        inner.closed = true;
        inner.stop();
        inner.watchers.clear();
    }

    /// Ends the chat once no pane or view holds it: a running turn is cancelled and ends
    /// as a restart ends it, the harness stops and its status goes. Returns its thread to
    /// save one last time, when it has one.
    fn end(&self) -> Option<ThreadRecord> {
        let mut inner = self.lock();
        let working = inner.state.working();
        if working {
            (inner.driver.wire().interrupt)(&mut inner);
        }
        inner.stop();
        inner.stop_agents();
        if working {
            inner.end_turn();
        }
        inner.save_thread(false);
        let record = inner.thread_to_save(&self.content);
        inner.closed = true;
        inner.watchers.clear();
        record
    }
}

impl Inner {
    /// Starts the chat on its saved thread: its transcript, title and session, with the
    /// entries a crash left open ended as a turn's end would, as of the thread's last
    /// change. The session's replay is left out when it loads.
    fn seed(&mut self, thread: ThreadRecord) {
        let ended = thread.updated_ms;
        let mut entries = thread.entries;
        for entry in &mut entries {
            settle_saved(entry, ended);
        }
        self.tools = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                ChatEntry::Tool(tool) => Some((tool.id.clone(), index)),
                _ => None,
            })
            .collect();
        self.state.entries = entries;
        self.state.title = thread.title;
        if thread.chat.session.is_some() {
            self.state.session = thread.chat.session;
        }
        self.thread = Some(ThreadTimes {
            created_ms: thread.created_ms,
            updated_ms: thread.updated_ms,
        });
        self.state.thread = true;
        self.skip_replay = true;
    }

    /// Gives the chat its thread when it has none, its transcript changed.
    fn start_thread(&mut self) {
        if self.thread.is_none() {
            let now = now_ms();
            self.thread = Some(ThreadTimes {
                created_ms: now,
                updated_ms: now,
            });
            self.state.thread = true;
        }
        self.save_thread(true);
    }

    /// Has the chat's thread, when it has one, saved once the lock is let go; `changed`
    /// says its transcript changed.
    fn save_thread(&mut self, changed: bool) {
        let Some(thread) = self.thread.as_mut() else {
            return;
        };
        if changed {
            thread.updated_ms = now_ms();
        }
        self.save_due = true;
    }

    /// The thread to save, when it changed since it was last saved and the chat goes on.
    fn thread_to_save(&mut self, content: &ChatContent) -> Option<ThreadRecord> {
        if !std::mem::take(&mut self.save_due) || self.closed {
            return None;
        }
        let times = self.thread?;
        Some(ThreadRecord::new(
            ChatContent {
                session: self.state.session.clone(),
                ..content.clone()
            },
            self.state.title.clone(),
            times.created_ms,
            times.updated_ms,
            self.state.entries.clone(),
        ))
    }

    /// Notes `surface`, the terminal whose view shows the chat now, if one does.
    fn show_on(&mut self, surface: Option<SurfaceId>) {
        self.surface = surface;
    }

    /// What the chat is doing, for the threads list.
    fn activity(&self) -> ThreadActivity {
        if self.state.permission.is_some() || self.state.question.is_some() {
            ThreadActivity::Waiting
        } else if self.state.working() {
            ThreadActivity::Working
        } else {
            ThreadActivity::Idle
        }
    }

    /// Applies `event` and sends it to every watcher still listening.
    fn emit(&mut self, event: ChatEvent) {
        match &event {
            ChatEvent::Reset { .. } => {
                self.tools.clear();
                self.agent_wave.clear();
                self.agents_summarized.clear();
                self.agents_revision += 1;
                self.agents_watched = None;
                self.agents_offset = 0;
                self.queue.clear();
                self.queue_held = false;
                self.unseen = None;
                self.prompted_with = None;
                self.skip_replay = false;
                (self.driver.wire().forget_transcript)(self);
            }
            ChatEvent::Status {
                status: ChatStatus::Failed { message },
            } => self.unseen = Some(Unseen::Error(message.clone())),
            _ => {}
        }
        self.watchers
            .retain(|watcher| watcher.send(event.clone()).is_ok());
        self.state.apply(event);
    }

    /// What the chat's program status shows (state, blocked kind, message): what waits
    /// on the user, else a running turn, else a result the user has not seen; nothing
    /// while the chat waits for a prompt, or once it has ended.
    fn status_shown(&self) -> Option<(ProgramState, Option<BlockedKind>, Option<&str>)> {
        if self.closed {
            return None;
        }
        let title = self.state.title.as_deref();
        if self.state.permission.is_some() {
            Some((ProgramState::Blocked, Some(BlockedKind::Permission), title))
        } else if self.state.question.is_some() {
            Some((ProgramState::Blocked, Some(BlockedKind::Question), title))
        } else if self.state.working() {
            Some((ProgramState::Working, None, title))
        } else {
            match &self.unseen {
                Some(Unseen::Done) => Some((ProgramState::Done, None, title)),
                Some(Unseen::Error(message)) => Some((ProgramState::Error, None, Some(message))),
                None => None,
            }
        }
    }

    /// Publishes the chat's program status when it differs from what was published.
    fn publish_status(&mut self) {
        let shown = self.status_shown();
        let unchanged = match (shown, &self.published) {
            (None, None) => true,
            (Some((state, kind, message)), Some(published)) => {
                published.state == state
                    && published.kind == kind
                    && published.message.as_deref() == message
            }
            _ => false,
        };
        if unchanged {
            return;
        }
        let record = shown.map(|(state, kind, message)| ProgramStatusRecord {
            id: None,
            state,
            kind,
            progress: None,
            app: Some(self.state.harness.name().to_owned()),
            title: None,
            message: message.map(str::to_owned),
        });
        hub()
            .status
            .publish(&self.chat_id, record.iter().cloned().collect());
        self.published = record;
    }

    /// Shows a prompt the harness has, after a `Switch` when the model or thinking level
    /// changed since the session's previous prompt.
    fn push_prompt(&mut self, prompt: &Prompt) {
        let name = |category| {
            self.state
                .option(category)
                .map(|option| option.current_name().to_owned())
        };
        let settings = PromptSettings {
            model: name("model"),
            thought_level: name("thought_level"),
        };
        if let Some(previous) = self.prompted_with.replace(settings.clone()) {
            let change = |from: Option<String>, to: Option<String>| match (from, to) {
                (Some(from), Some(to)) if from != to => Some(ChatChange { from, to }),
                _ => None,
            };
            let model = change(previous.model, settings.model);
            let thought_level = change(previous.thought_level, settings.thought_level);
            if model.is_some() || thought_level.is_some() {
                self.emit(ChatEvent::Entry {
                    entry: ChatEntry::Switch {
                        model,
                        thought_level,
                    },
                });
            }
        }
        self.emit(ChatEvent::Entry {
            entry: prompt.entry(now_ms()),
        });
        self.start_thread();
    }

    /// Whether prompts have a session to go to: the session the agent opened, or, for
    /// an agent that opens it with the first prompt (Claude Code), any.
    fn has_session(&self) -> bool {
        self.state.session.is_some() || self.driver.wire().session_with_first_prompt
    }

    /// The chat's folder as the running program names it: a Windows program in a WSL
    /// chat names it by its `\\wsl.localhost` path, a Linux one in a Windows folder by
    /// its mount.
    fn agent_folder(&self) -> &str {
        match (&self.started, &self.folder) {
            (Some(started), _) => &started.folder,
            (None, Some((_, folder))) => folder,
            (None, None) => "",
        }
    }

    /// What the running program is told at start, when it is told anything: the note
    /// of its side.
    fn note(&self) -> Option<&str> {
        let windows = self.started.as_ref().is_some_and(|started| started.windows);
        let (linux, windows_note) = &self.notes;
        if windows { windows_note } else { linux }.as_deref()
    }

    /// Starts a turn on `prompt`, the harness having a session for it (`has_session`).
    fn start_prompt(&mut self, prompt: Prompt) {
        self.unseen = None;
        self.push_prompt(&prompt);
        let since_ms = now_ms();
        self.activity_ms = since_ms;
        self.queue_held = false;
        self.emit(ChatEvent::Status {
            status: ChatStatus::Working { since_ms },
        });
        (self.driver.wire().prompt)(self, &prompt);
    }

    /// The running turn ended with `result`. How it ended stays unseen until the user
    /// looks, unless it was cancelled; a turn that did not end on its own holds the queue,
    /// one that did goes on to the next prompt.
    fn prompt_ended(&mut self, result: Result<Value, String>) {
        let stop = match &result {
            Ok(result) => result["stopReason"]
                .as_str()
                .unwrap_or("end_turn")
                .to_owned(),
            Err(_) => "error".to_owned(),
        };
        self.end_turn();
        let unseen = match (stop.as_str(), result) {
            ("end_turn", _) => Some(Unseen::Done),
            ("cancelled", _) => {
                self.emit(ChatEvent::Entry {
                    entry: ChatEntry::Cancelled,
                });
                None
            }
            (_, Err(message)) => Some(Unseen::Error(message)),
            (reason, Ok(_)) => Some(Unseen::Error(match reason {
                "max_tokens" => "The reply reached the model's token limit.".into(),
                "max_turn_requests" => "The turn reached its request limit.".into(),
                "refusal" => "The model refused to go on.".into(),
                other => format!("The turn ended: {other}."),
            })),
        };
        if let Some(Unseen::Error(message)) = &unseen {
            self.emit(ChatEvent::Entry {
                entry: ChatEntry::Error {
                    message: message.clone(),
                },
            });
        }
        self.unseen = unseen;
        if !matches!(self.state.status, ChatStatus::Failed { .. }) {
            self.emit(ChatEvent::Status {
                status: ChatStatus::Ready,
            });
        }
        self.queue_held = stop != "end_turn";
        self.send_queued();
    }

    /// Puts `prompt` at the end of the queue, or first in it.
    fn enqueue(&mut self, prompt: Prompt, first: bool) {
        self.next_queued += 1;
        let queued = (self.next_queued, prompt);
        if first {
            self.queue.push_front(queued);
        } else {
            self.queue.push_back(queued);
        }
        self.publish_queue();
    }

    /// Replaces the text of queued prompt `queued`, which keeps its place.
    fn edit_queued(&mut self, queued: u64, text: String) -> Result<(), String> {
        let Some((_, prompt)) = self.queue.iter_mut().find(|(id, _)| *id == queued) else {
            return Err("that prompt was already sent".into());
        };
        let was = std::mem::replace(&mut prompt.text, text);
        if let Err(error) = prompt.check() {
            prompt.text = was;
            return Err(error);
        }
        self.publish_queue();
        Ok(())
    }

    fn publish_queue(&mut self) {
        let queued = self
            .queue
            .iter()
            .map(|(id, prompt)| ChatQueued {
                id: *id,
                text: prompt.text.clone(),
                files: prompt.files.clone(),
                images: prompt.image_names(),
            })
            .collect();
        self.emit(ChatEvent::Queued { queued });
    }

    /// Sends the first queued prompt when the chat waits for one and the queue is not
    /// held.
    fn send_queued(&mut self) {
        if self.queue_held || !matches!(self.state.status, ChatStatus::Ready) {
            return;
        }
        if !self.has_session() {
            return;
        }
        let Some((_, prompt)) = self.queue.pop_front() else {
            return;
        };
        self.publish_queue();
        self.start_prompt(prompt);
    }

    fn write(&mut self, message: &Value) {
        let Some(agent) = self.agent.as_mut() else {
            return;
        };
        let mut line = message.to_string();
        line.push('\n');
        // A write that fails means the harness ended; its reader reports that.
        let _ = agent
            .input
            .write_all(line.as_bytes())
            .and_then(|()| agent.input.flush());
    }

    /// Tells the harness nobody will answer its open permission or question.
    fn cancel_requests(&mut self) {
        (self.driver.wire().cancel_asks)(self);
        if self.state.permission.is_some() {
            self.emit(ChatEvent::Permission { permission: None });
        }
        if self.state.question.is_some() {
            self.emit(ChatEvent::Question { question: None });
        }
    }

    /// The harness's open permission and question need no answer any more.
    fn forget_requests(&mut self) {
        (self.driver.wire().forget_asks)(self);
    }

    /// Windows waiting on the harness hear it is gone; nothing it was asked is out any
    /// more.
    fn fail_pending(&mut self) {
        (self.driver.wire().fail_pending)(self);
    }

    /// A turn ended: tool calls still open will not report again, unless sub-agents they
    /// started still run (they outlive the turn), and nobody needs to answer its
    /// permission or question any more.
    fn end_turn(&mut self) {
        self.end_thought();
        if let Some((environment, folder)) = &self.folder {
            crate::project::files_changed(environment, folder);
        }
        let now = now_ms();
        let open: Vec<usize> = self
            .state
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                ChatEntry::Tool(tool)
                    if tool.status.is_open()
                        && !tool.agents.iter().any(|agent| agent.status.is_open()) =>
                {
                    Some(index)
                }
                _ => None,
            })
            .collect();
        for index in open {
            if let Some(ChatEntry::Tool(tool)) = self.state.entries.get(index) {
                let mut tool = tool.clone();
                tool.status = ChatToolStatus::Stopped;
                tool.ended_ms = Some(now);
                self.emit(ChatEvent::Replace {
                    index,
                    entry: ChatEntry::Tool(tool),
                });
            }
        }
        // The prompts that went into the turn end with it.
        let timed: Vec<usize> = self
            .state
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                ChatEntry::User {
                    sent_ms: Some(_),
                    ended_ms: None,
                    ..
                } => Some(index),
                _ => None,
            })
            .collect();
        for index in timed {
            if let Some(ChatEntry::User {
                text,
                files,
                images,
                sent_ms,
                ..
            }) = self.state.entries.get(index)
            {
                let entry = ChatEntry::User {
                    text: text.clone(),
                    files: files.clone(),
                    images: images.clone(),
                    sent_ms: *sent_ms,
                    ended_ms: Some(now),
                };
                self.emit(ChatEvent::Replace { index, entry });
            }
        }
        self.forget_requests();
        if self.state.permission.is_some() {
            self.emit(ChatEvent::Permission { permission: None });
        }
        if self.state.question.is_some() {
            self.emit(ChatEvent::Question { question: None });
        }
        self.save_thread(true);
    }

    /// The harness ended: sub-agents still open will not report again. They get no
    /// summary.
    fn stop_agents(&mut self) {
        self.agent_wave.clear();
        let now = now_ms();
        for index in 0..self.state.entries.len() {
            let Some(ChatEntry::Tool(tool)) = self.state.entries.get(index) else {
                continue;
            };
            if !tool.agents.iter().any(|agent| agent.status.is_open()) {
                continue;
            }
            let mut tool = tool.clone();
            for agent in tool
                .agents
                .iter_mut()
                .filter(|agent| agent.status.is_open())
            {
                agent.status = ChatToolStatus::Stopped;
            }
            tool.status = ChatToolStatus::Stopped;
            tool.ended_ms = Some(now);
            self.emit(ChatEvent::Replace {
                index,
                entry: ChatEntry::Tool(tool),
            });
        }
    }

    /// Marks a thought still streaming as finished.
    fn end_thought(&mut self) {
        let Some(index) = self.state.entries.len().checked_sub(1) else {
            return;
        };
        if let Some(ChatEntry::Thought {
            text,
            started_ms,
            ended_ms: None,
        }) = self.state.entries.get(index)
        {
            let entry = ChatEntry::Thought {
                text: text.clone(),
                started_ms: *started_ms,
                ended_ms: Some(now_ms()),
            };
            self.emit(ChatEvent::Replace { index, entry });
        }
    }

    /// Stops the harness: its input closes, which ends it, and it is killed if it takes
    /// longer than `STOP_GRACE`.
    fn stop(&mut self) {
        self.generation += 1;
        self.initialized = false;
        self.fail_pending();
        self.forget_requests();
        if let Some(Agent {
            mut child,
            input,
            #[cfg(windows)]
            _job,
            ..
        }) = self.agent.take()
        {
            drop(input);
            let _ = thread::Builder::new()
                .name("compi-chat-stop".into())
                .spawn(move || {
                    let deadline = std::time::Instant::now() + STOP_GRACE;
                    while std::time::Instant::now() < deadline {
                        if let Ok(Some(_)) = child.try_wait() {
                            return;
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    #[cfg(windows)]
                    drop(_job);
                });
        }
    }

    /// Adds streamed text to the last entry when `same` says it continues it, else starts
    /// one.
    fn append(
        &mut self,
        text: String,
        entry: impl FnOnce(String) -> ChatEntry,
        same: impl Fn(&ChatEntry) -> bool,
    ) {
        match self.state.entries.last() {
            Some(last) if same(last) => {
                let index = self.state.entries.len() - 1;
                self.emit(ChatEvent::Text { index, text });
            }
            _ => self.emit(ChatEvent::Entry { entry: entry(text) }),
        }
    }

    /// Tool call `id` as the transcript shows it, else a new one, pending since now.
    fn tool_call(&self, id: &str) -> ChatTool {
        match self
            .tools
            .get(id)
            .and_then(|index| self.state.entries.get(*index))
        {
            Some(ChatEntry::Tool(tool)) => (**tool).clone(),
            _ => ChatTool {
                id: id.to_owned(),
                name: None,
                title: String::new(),
                kind: "other".into(),
                status: ChatToolStatus::Pending,
                input: None,
                output: None,
                diffs: Vec::new(),
                locations: Vec::new(),
                started_ms: now_ms(),
                ended_ms: None,
                agents: Vec::new(),
                waits_on_agents: false,
            },
        }
    }

    /// Shows `tool`: the entry of its call changes, when it differs, or the call gets one.
    fn put_tool(&mut self, tool: ChatTool) {
        let known = self.tools.get(&tool.id).copied();
        let id = tool.id.clone();
        let entry = ChatEntry::Tool(Box::new(tool));
        match known {
            Some(index) => {
                if self.state.entries.get(index) != Some(&entry) {
                    self.emit(ChatEvent::Replace { index, entry });
                    self.note_agents(index);
                }
            }
            None => {
                let index = self.state.entries.len();
                self.tools.insert(id, index);
                self.emit(ChatEvent::Entry { entry });
                self.note_agents(index);
            }
        }
    }

    /// Recomputes one wave from reported agent times, not the time a late update arrived.
    fn agent_summary(&self, tools: &[usize]) -> Option<ChatAgentsFinished> {
        let mut count = 0;
        let mut started_ms: Option<u64> = None;
        let mut ended_ms = 0;
        let mut duration_ms = 0;
        let mut cost = None;
        for &index in tools {
            let Some(ChatEntry::Tool(tool)) = self.state.entries.get(index) else {
                continue;
            };
            for agent in &tool.agents {
                if agent.status.is_open() {
                    return None;
                }
                count += 1;
                duration_ms = duration_ms.max(agent.duration_ms);
                if let Some(started) = agent.started_ms {
                    started_ms = Some(started_ms.map_or(started, |first| first.min(started)));
                    let ended = if agent.duration_ms == 0 {
                        tool.ended_ms.unwrap_or(started)
                    } else {
                        started.saturating_add(agent.duration_ms)
                    };
                    ended_ms = ended_ms.max(ended);
                }
                if let Some(reported) = agent.cost {
                    cost = Some(cost.unwrap_or(0.0) + reported);
                }
            }
        }
        (count > 0).then(|| ChatAgentsFinished {
            tool: tools.iter().copied().max().unwrap_or(0),
            count,
            duration_ms: duration_ms
                .max(started_ms.map_or(0, |started| ended_ms.saturating_sub(started))),
            cost,
        })
    }

    /// Refreshes an existing card in place, or ends the current wave with one new card.
    fn note_agents(&mut self, index: usize) {
        let Some(ChatEntry::Tool(tool)) = self.state.entries.get(index) else {
            return;
        };
        if let Some(wave) = self.agents_summarized.get(&index) {
            let Some(summary) = self.agent_summary(&wave.tools) else {
                return;
            };
            let index = wave.entry;
            let entry = ChatEntry::AgentsFinished(summary);
            if self.state.entries.get(index) != Some(&entry) {
                self.emit(ChatEvent::Replace { index, entry });
            }
            return;
        }
        if !tool.agents.is_empty() && !self.agent_wave.contains(&index) {
            self.agent_wave.push(index);
        }
        let Some(summary) = self.agent_summary(&self.agent_wave) else {
            return;
        };
        let wave = Arc::new(FinishedAgentWave {
            entry: self.state.entries.len(),
            tools: std::mem::take(&mut self.agent_wave),
        });
        for &tool in &wave.tools {
            self.agents_summarized.insert(tool, Arc::clone(&wave));
        }
        self.emit(ChatEvent::Entry {
            entry: ChatEntry::AgentsFinished(summary),
        });
    }

    /// The sub-agents still running or waiting for the saved answer after a final progress report.
    fn open_agents(&self) -> Vec<String> {
        self.state
            .entries
            .iter()
            .filter_map(|entry| match entry {
                ChatEntry::Tool(tool) => Some(&tool.agents),
                _ => None,
            })
            .flatten()
            .filter(|agent| {
                agent.status.is_open()
                    || (agent.status == ChatToolStatus::Completed && agent.result.is_none())
            })
            .map(|agent| agent.id.clone())
            .collect()
    }
}

/// An entry of a saved thread whose harness ended before it did: a running tool call
/// and its sub-agents stop, an unanswered prompt's turn and an unfinished thought end, at
/// `ended`.
fn settle_saved(entry: &mut ChatEntry, ended: u64) {
    match entry {
        ChatEntry::Tool(tool) => {
            for agent in tool
                .agents
                .iter_mut()
                .filter(|agent| agent.status.is_open())
            {
                agent.status = ChatToolStatus::Stopped;
            }
            if tool.status.is_open() {
                tool.status = ChatToolStatus::Stopped;
                tool.ended_ms.get_or_insert(ended);
            }
        }
        ChatEntry::User {
            sent_ms: Some(_),
            ended_ms: ended_ms @ None,
            ..
        }
        | ChatEntry::Thought {
            ended_ms: ended_ms @ None,
            ..
        } => *ended_ms = Some(ended),
        _ => {}
    }
}

/// A call with sub-agents runs while one of them does: its own report of done once it
/// started them, or of running after they finished, does not change that. A call that
/// failed ends those still open.
fn settle(tool: &mut ChatTool, was: ChatToolStatus, now: u64) {
    if tool.agents.is_empty() {
        return;
    }
    if tool.status == ChatToolStatus::Failed {
        for agent in tool
            .agents
            .iter_mut()
            .filter(|agent| agent.status.is_open())
        {
            agent.status = ChatToolStatus::Failed;
        }
    }
    if tool.agents.iter().any(|agent| agent.status.is_open()) {
        tool.status = ChatToolStatus::Running;
        tool.ended_ms = None;
    } else {
        if tool.status.is_open() {
            tool.status = if was.is_open() {
                ChatToolStatus::Completed
            } else {
                was
            };
        }
        tool.ended_ms.get_or_insert(now);
    }
}

fn new_agent(id: &str) -> ChatAgent {
    ChatAgent {
        id: id.to_owned(),
        agent: String::new(),
        status: ChatToolStatus::Pending,
        activity: None,
        tool_count: 0,
        started_ms: None,
        duration_ms: 0,
        cost: None,
        model: None,
        assignment: None,
        result: None,
        steps: Vec::new(),
    }
}

/// `text` on one line, at most `AGENT_ACTIVITY_CHARS`; nothing of a blank one.
fn short_line(text: &str) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    Some(match line.char_indices().nth(AGENT_ACTIVITY_CHARS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line,
    })
}

/// The first `limit` bytes of `text`, on a character boundary.
fn head(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        let mut cut = limit;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push('…');
    }
    text
}

/// The last `limit` bytes of `text`, on a character boundary.
fn tail(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut cut = text.len() - limit;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("…{}", &text[cut..])
}

fn not_installed(content: &ChatContent) -> String {
    let place = match &content.environment {
        ContentEnvironment::Host if cfg!(windows) => "on Windows or in WSL".to_owned(),
        ContentEnvironment::Host => "here".to_owned(),
        ContentEnvironment::Wsl { distribution } => {
            format!("in WSL ({distribution}) or on Windows")
        }
    };
    format!(
        "{} isn't installed {place}. Install it, then Retry.",
        content.harness.name()
    )
}

/// Keeps the last `STDERR_TAIL_BYTES` of what the harness writes to standard error.
fn keep_tail(errors: impl Read, tail: &Mutex<String>) {
    let mut reader = BufReader::new(errors);
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
        push_tail(tail, &line);
        line.clear();
    }
}

/// Adds `text` to the end of `tail`, keeping its last `STDERR_TAIL_BYTES`. NULs go:
/// `wsl.exe` reports its own failures in UTF-16.
fn push_tail(tail: &Mutex<String>, text: &str) {
    let mut tail = lock(tail);
    tail.extend(text.chars().filter(|c| *c != '\0'));
    if !tail.ends_with('\n') {
        tail.push('\n');
    }
    if tail.len() > STDERR_TAIL_BYTES {
        let mut cut = tail.len() - STDERR_TAIL_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail.drain(..cut);
    }
}

fn last_lines(text: &str, count: usize) -> &str {
    let start = text
        .rmatch_indices('\n')
        .nth(count - 1)
        .map_or(0, |(index, _)| index + 1);
    &text[start..]
}

/// What an agent running in `distribution` is told at start; `None` when the Windows
/// home folder has no Linux path there (a failure is logged).
#[cfg(windows)]
fn wsl_note(distribution: &str) -> Option<String> {
    let home = std::env::var("USERPROFILE")
        .ok()
        .filter(|home| !home.is_empty())?;
    match linux_home(distribution, &home) {
        Ok(linux) => Some(format!(
            "You are running in WSL ({distribution}) on a Windows PC, inside Compi. \
             The Windows user folder is {home}, which is {linux} here. \
             Windows programs run from this shell by their .exe names, such as cmd.exe, \
             powershell.exe, tasklist.exe and winget.exe. To see an open Windows app window, \
             such as a dialog you caused: `compi screenshot` lists the windows, and \
             `compi screenshot NAME` saves one as a PNG and prints its path for you to read."
        )),
        Err(error) => {
            eprintln!("compi-daemon: no WSL note for {distribution}: {error}");
            None
        }
    }
}

/// The one thing a Windows agent in a WSL chat is told beyond `wsl_note`: its shell is
/// that WSL's, but its own file tools take Windows paths, which Compi cannot translate.
#[cfg(windows)]
fn windows_paths_note(distribution: &str) -> String {
    format!(
        r"Your file tools take Windows paths: the Linux path /a/b is \\wsl.localhost\{distribution}\a\b, and /mnt/c/a/b is C:\a\b."
    )
}

/// `home`'s Linux path in `distribution`, asked once per distribution and kept.
#[cfg(windows)]
fn linux_home(distribution: &str, home: &str) -> compi_protocol::Result<String> {
    static HOMES: LazyLock<Mutex<HashMap<(String, String), String>>> =
        LazyLock::new(Mutex::default);
    let key = (distribution.to_owned(), home.to_owned());
    if let Some(linux) = lock(&HOMES).get(&key) {
        return Ok(linux.clone());
    }
    let linux = compi_protocol::wsl::wsl_path_for_windows(home, distribution)?;
    lock(&HOMES).insert(key, linux.clone());
    Ok(linux)
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp::{Pending, activity, file_uri, form_content, form_fields, path_of_uri, task_output};
    use compi_protocol::chat::{ChatAnswer, ChatDiff};
    use serde_json::json;

    fn tool_entry(inner: &Inner, index: usize) -> &ChatTool {
        match &inner.state.entries[index] {
            ChatEntry::Tool(tool) => tool,
            other => panic!("entry {index} is {other:?}"),
        }
    }

    #[test]
    fn a_chat_has_the_surface_of_the_terminal_carrying_it_and_none_in_a_content_pane() {
        use compi_protocol::{PaneContent, PaneId, PaneSide, PaneView, SplitAxis};
        let chat = |id: &str| {
            PaneContent::Chat(ChatContent {
                id: id.into(),
                harness: ChatHarness::Omp,
                environment: ContentEnvironment::Host,
                folder: "/work".into(),
                session: None,
            })
        };
        let layout = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutNode::Pane {
                pane_id: PaneId::new("pane-1"),
                surface_id: SurfaceId::new("surface-1"),
                view: Some(PaneView {
                    content: chat("in-terminal"),
                    side: PaneSide::View,
                }),
            }),
            second: Box::new(LayoutNode::Content {
                pane_id: PaneId::new("pane-2"),
                content: chat("beside"),
            }),
        };
        let mut held = HashMap::new();
        chat_leaves(&layout, &mut held);
        assert_eq!(
            held,
            HashMap::from([
                ("in-terminal".to_owned(), Some(SurfaceId::new("surface-1"))),
                ("beside".to_owned(), None),
            ])
        );
    }

    #[test]
    fn streamed_chunks_join_one_entry_per_speaker_and_tool_calls_update_in_place() {
        let mut inner = Inner::default();
        let chunk = |kind: &str, text: &str| json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } });
        inner.update(&chunk("agent_thought_chunk", "Plan"));
        inner.update(&chunk("agent_message_chunk", "Hel"));
        inner.update(&chunk("agent_message_chunk", "lo"));
        inner.update(&json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "$ ls", "name": "bash",
            "kind": "execute", "status": "pending", "rawInput": { "command": "ls" },
        }));
        inner.update(&json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "in_progress",
            "content": [{ "type": "content", "content": { "type": "text", "text": "$ ls" } }],
            "rawOutput": { "content": [{ "type": "text", "text": "a.txt\n" }] },
        }));
        inner.update(&chunk("agent_message_chunk", "Done"));
        assert_eq!(inner.state.entries.len(), 4);
        // The reply that followed ended the thought.
        assert!(matches!(
            inner.state.entries[0],
            ChatEntry::Thought {
                ended_ms: Some(_),
                ..
            }
        ));
        assert_eq!(
            inner.state.entries[1],
            ChatEntry::Agent {
                text: "Hello".into()
            }
        );
        let tool = tool_entry(&inner, 2);
        assert_eq!(
            (tool.title.as_str(), tool.status),
            ("$ ls", ChatToolStatus::Running)
        );
        // The tool's own output wins over the harness's display text; the title stays.
        assert_eq!(tool.output.as_deref(), Some("a.txt\n"));
        assert!(
            tool.input
                .as_deref()
                .is_some_and(|input| input.contains("\"ls\""))
        );
        // A turn that ends leaves no tool call running.
        inner.end_turn();
        let tool = tool_entry(&inner, 2);
        assert_eq!(tool.status, ChatToolStatus::Stopped);
        assert!(tool.ended_ms.is_some());
    }

    #[test]
    fn an_edit_keeps_its_diff_and_a_permission_creates_its_tool_call() {
        let mut inner = Inner::default();
        inner.permission(
            json!(7),
            &json!({
                "toolCall": { "toolCallId": "t2", "title": "python3 x.py", "kind": "execute" },
                "options": [{ "optionId": "a", "name": "Allow once", "kind": "allow_once" }],
            }),
        );
        let permission = inner.state.permission.clone().unwrap();
        assert_eq!(permission.tool.as_deref(), Some("t2"));
        assert_eq!(tool_entry(&inner, 0).title, "python3 x.py");
        // The tool call the harness reports afterwards is the same entry.
        inner.update(
            &json!({ "sessionUpdate": "tool_call", "toolCallId": "t2", "status": "pending" }),
        );
        assert_eq!(inner.state.entries.len(), 1);

        inner.update(&json!({
            "sessionUpdate": "tool_call", "toolCallId": "e1", "title": "Edit", "kind": "edit",
            "status": "completed",
            "content": [{ "type": "diff", "path": "/w/a.txt", "oldText": "a\n", "newText": "b\n" }],
        }));
        // A later update without a diff keeps it.
        inner.update(&json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "e1",
            "content": [{ "type": "content", "content": { "type": "text", "text": "ok" } }],
        }));
        let tool = tool_entry(&inner, 1);
        assert_eq!(
            tool.diffs,
            [ChatDiff {
                path: "/w/a.txt".into(),
                old_text: Some("a\n".into()),
                new_text: Some("b\n".into()),
            }]
        );
        assert!(tool.ended_ms.is_some());
    }

    #[test]
    fn a_form_becomes_fields_and_answers_go_back_in_their_types() {
        let schema = json!({
            "type": "object",
            "properties": {
                "q10": { "type": "string", "title": "Tenth" },
                "q2": { "type": "array", "title": "Pick", "items": { "enum": ["a", "b"] } },
                "q0": { "type": "string", "title": "Colour?",
                        "oneOf": [{ "const": "Red", "title": "Red" }, { "const": "Blue", "title": "Blue" }] },
                "q0__other": { "type": "string", "title": "Other" },
                "ok": { "type": "boolean" },
            },
            "required": ["q0"],
        });
        let (fields, shapes) = form_fields(&schema);
        let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
        assert_eq!(keys, ["q10", "q2", "q0", "ok"]);
        let colour = &fields[2];
        assert!(colour.other && colour.required && !colour.multi);
        assert_eq!(colour.choices.len(), 2);
        assert!(fields[1].multi);

        let answers = ChatAnswers::from([
            (
                "q0".into(),
                ChatAnswer::Text {
                    text: "green".into(),
                },
            ),
            (
                "q2".into(),
                ChatAnswer::Choices {
                    values: vec!["a".into(), "b".into()],
                },
            ),
            (
                "ok".into(),
                ChatAnswer::Choices {
                    values: vec!["true".into()],
                },
            ),
            ("q10".into(), ChatAnswer::Text { text: "x".into() }),
        ]);
        assert_eq!(
            form_content(&shapes, answers),
            json!({ "q0__other": "green", "q2": ["a", "b"], "ok": true, "q10": "x" })
        );
    }

    #[test]
    fn an_html_form_keeps_its_order_and_its_other_field_joins_its_choices() {
        // OMP's HTML form: a radio group with an Other field, then a textarea.
        let schema = json!({
            "type": "object",
            "properties": {
                "store": { "type": "string", "title": "Session store",
                           "oneOf": [{ "const": "redis", "title": "Redis" }, { "const": "sql", "title": "SQL" }] },
                "store_other": { "type": "string", "title": "Session store — Other" },
                "notes": { "type": "string", "title": "Notes" },
            },
        });
        let (fields, shapes) = form_fields(&schema);
        let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
        assert_eq!(keys, ["store", "notes"]);
        assert!(fields[0].other && fields[1].choices.is_empty());
        let answers = ChatAnswers::from([(
            "store".into(),
            ChatAnswer::Text {
                text: "files".into(),
            },
        )]);
        assert_eq!(
            form_content(&shapes, answers),
            json!({ "store_other": "files" })
        );
    }

    #[test]
    fn attached_paths_round_trip_through_file_uris() {
        for path in ["/home/me/a b.rs", r"C:\Users\me\x.txt"] {
            assert_eq!(path_of_uri(&file_uri(path)), path);
        }
        assert_eq!(file_uri(r"C:\x y"), "file:///C:/x%20y");
    }

    #[test]
    fn watchers_see_the_same_state_the_chat_keeps() {
        let mut inner = Inner::default();
        let (sender, receiver) = mpsc::channel();
        let mut seen = inner.state.clone();
        inner.watchers.push(sender);
        inner.update(&json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "a" } }));
        inner.options(&json!([{ "id": "model", "name": "Model", "category": "model", "type": "select",
            "currentValue": "x/y", "options": [{ "group": "x", "name": "X", "options": [{ "value": "x/y", "name": "Model Y" }] }] }]));
        inner.update(&json!({ "sessionUpdate": "usage_update", "used": 10, "size": 100, "cost": { "amount": 0.5, "currency": "USD" } }));
        for event in receiver.try_iter() {
            seen.apply(event);
        }
        assert_eq!(seen, inner.state);
        let model = seen.option("model").unwrap();
        assert_eq!(model.current_name(), "Model Y");
        assert_eq!(model.choices[0].group.as_deref(), Some("X"));
    }

    #[test]
    fn a_long_session_fits_one_watch_frame() {
        let mut state = ChatState::default();
        for index in 0..4 {
            state.entries.push(ChatEntry::Tool(Box::new(ChatTool {
                id: index.to_string(),
                name: None,
                title: "t".into(),
                kind: "read".into(),
                status: ChatToolStatus::Completed,
                input: None,
                output: Some("x".repeat(1000)),
                diffs: Vec::new(),
                locations: Vec::new(),
                started_ms: 0,
                ended_ms: None,
                agents: Vec::new(),
                waits_on_agents: false,
            })));
        }
        let fitted = fit(state, 2500);
        assert!(serde_json::to_vec(&fitted).unwrap().len() <= 2500);
        // The newest call keeps its output.
        assert!(matches!(&fitted.entries[3], ChatEntry::Tool(tool) if tool.output.is_some()));
    }

    /// A `task` call's update, its agents' progress as OMP reports it.
    fn task_update(status: &str, progress: Value) -> Value {
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "task1", "status": status,
            "rawOutput": {
                "content": [{ "type": "text", "text": "Started 2 agents." }],
                "details": { "progress": progress },
            },
        })
    }

    fn agent_progress_item(id: &str, status: &str, cost: f64) -> Value {
        json!({
            "id": id, "agent": "sonic", "status": status, "assignment": format!("Do {id}"),
            "recentTools": [
                { "tool": "read", "args": "src/lib.rs", "intent": "Reading the library", "isError": false, "endMs": 5 },
                { "tool": "grep", "args": "x", "isError": false, "endMs": 2 },
            ],
            "toolCount": 3, "requests": 2, "tokens": 900, "cost": cost, "durationMs": 4000,
            "resolvedModel": "anthropic/claude-haiku",
        })
    }

    fn start_task(inner: &mut Inner) {
        inner.update(&json!({
            "sessionUpdate": "tool_call", "toolCallId": "task1", "name": "task", "title": "task",
            "kind": "other", "status": "pending",
            "rawInput": { "tasks": [
                { "name": "TaskA", "agent": "sonic", "task": "Do TaskA" },
                { "name": "TaskB", "agent": "sonic", "task": "Do TaskB" },
            ] },
        }));
    }

    fn summaries(inner: &Inner) -> Vec<&ChatAgentsFinished> {
        inner
            .state
            .entries
            .iter()
            .filter_map(|entry| match entry {
                ChatEntry::AgentsFinished(summary) => Some(summary),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn task_progress_and_wait_jobs_fill_the_sub_agents() {
        let mut inner = Inner::default();
        start_task(&mut inner);
        assert!(tool_entry(&inner, 0).agents.is_empty());
        // Done once it started them, yet the call runs while they do.
        inner.update(&task_update(
            "completed",
            json!([
                agent_progress_item("TaskA", "running", 0.05),
                agent_progress_item("TaskB", "pending", 0.0),
            ]),
        ));
        let tool = tool_entry(&inner, 0);
        assert_eq!(
            (tool.status, tool.ended_ms),
            (ChatToolStatus::Running, None)
        );
        assert_eq!(tool.output.as_deref(), Some("Started 2 agents."));
        let [a, b] = tool.agents.as_slice() else {
            panic!("{:?}", tool.agents);
        };
        assert_eq!(
            (a.id.as_str(), a.agent.as_str(), a.status),
            ("TaskA", "sonic", ChatToolStatus::Running)
        );
        // The latest tool comes first.
        assert_eq!(a.activity.as_deref(), Some("Reading the library"));
        assert_eq!((a.tool_count, a.duration_ms), (3, 4000));
        assert_eq!(a.cost, Some(0.05));
        assert_eq!(a.model.as_deref(), Some("anthropic/claude-haiku"));
        assert_eq!(a.assignment.as_deref(), Some("Do TaskA"));
        assert!(a.started_ms.is_some());
        assert_eq!((b.status, b.started_ms), (ChatToolStatus::Pending, None));

        inner.update(&json!({
            "sessionUpdate": "tool_call", "toolCallId": "w1", "name": "wait", "title": "wait",
            "kind": "other", "status": "completed",
            "rawOutput": { "details": { "jobs": [
                { "id": "TaskA", "type": "task", "status": "completed", "durationMs": 6000,
                  "resultText": "<task-result id=\"TaskA\" agent=\"sonic\" status=\"completed\" duration=\"6s\">\n<output>\nAll done.\n</output>\n</task-result>" },
                { "id": "TaskB", "type": "task", "status": "running", "durationMs": 3000, "cost": 0.01 },
            ] } },
        }));
        assert_eq!(inner.state.entries.len(), 2);
        let tool = tool_entry(&inner, 0);
        assert_eq!(tool.status, ChatToolStatus::Running);
        let a = &tool.agents[0];
        assert_eq!(
            (a.status, a.duration_ms, a.result.as_deref()),
            (ChatToolStatus::Completed, 6000, Some("All done."))
        );
        assert_eq!(tool.agents[1].status, ChatToolStatus::Pending);
        assert!(summaries(&inner).is_empty());
        // A wait on agents only folds into their call; the task call itself does not.
        assert!(tool_entry(&inner, 1).waits_on_agents);
        assert!(!tool.waits_on_agents);
    }

    #[test]
    fn an_agents_activity_is_one_short_line() {
        assert_eq!(
            activity(&json!({ "recentTools": [{ "tool": "bash", "args": "cargo\n  test" }] }))
                .as_deref(),
            Some("bash cargo test")
        );
        // The tool running now wins over the latest finished one.
        assert_eq!(
            activity(&json!({
                "currentTool": "edit", "currentToolIntent": "Fixing the parser",
                "recentTools": [{ "tool": "read", "intent": "Reading" }],
            }))
            .as_deref(),
            Some("Fixing the parser")
        );
        assert_eq!(activity(&json!({ "recentTools": [] })), None);
        let long = activity(&json!({ "lastIntent": "x".repeat(400) })).unwrap();
        assert_eq!(long.chars().count(), AGENT_ACTIVITY_CHARS + 1);
        assert_eq!(
            task_output("<task-result id=\"a\"><preview>Short</preview></task-result>"),
            "Short"
        );
        assert_eq!(task_output("plain"), "plain");
    }

    #[test]
    fn the_last_agent_to_finish_ends_the_transcript_with_one_summary() {
        let mut inner = Inner::default();
        let (sender, receiver) = mpsc::channel();
        let mut seen = inner.state.clone();
        inner.watchers.push(sender);
        start_task(&mut inner);
        inner.update(&task_update(
            "completed",
            json!([
                agent_progress_item("TaskA", "running", 0.05),
                agent_progress_item("TaskB", "running", 0.1),
            ]),
        ));
        // The agents outlive the turn.
        inner.end_turn();
        assert_eq!(tool_entry(&inner, 0).status, ChatToolStatus::Running);
        inner.update(&task_update(
            "in_progress",
            json!([
                agent_progress_item("TaskA", "completed", 0.05),
                agent_progress_item("TaskB", "running", 0.1),
            ]),
        ));
        // A stale report of TaskA running does not bring it back.
        inner.update(&task_update(
            "in_progress",
            json!([
                agent_progress_item("TaskA", "running", 0.05),
                agent_progress_item("TaskB", "running", 0.15),
            ]),
        ));
        assert_eq!(
            tool_entry(&inner, 0).agents[0].status,
            ChatToolStatus::Completed
        );
        assert!(summaries(&inner).is_empty());
        inner.update(&task_update(
            "completed",
            json!([
                agent_progress_item("TaskA", "completed", 0.05),
                agent_progress_item("TaskB", "completed", 0.15),
            ]),
        ));
        let tool = tool_entry(&inner, 0);
        assert_eq!(tool.status, ChatToolStatus::Completed);
        assert!(tool.ended_ms.is_some());
        let [summary] = summaries(&inner)[..] else {
            panic!("{:?}", inner.state.entries);
        };
        assert_eq!((summary.tool, summary.count), (0, 2));
        assert!(summary.duration_ms >= 4000);
        assert!((summary.cost.unwrap() - 0.2).abs() < 1e-9);
        assert!(matches!(
            inner.state.entries.last(),
            Some(ChatEntry::AgentsFinished(_))
        ));
        // OMP flips the call to running and done again; neither reopens it or repeats the
        // summary.
        inner.update(&task_update(
            "in_progress",
            json!([agent_progress_item("TaskB", "running", 0.15)]),
        ));
        inner.update(&task_update(
            "completed",
            json!([agent_progress_item("TaskB", "completed", 0.15)]),
        ));
        assert_eq!(tool_entry(&inner, 0).status, ChatToolStatus::Completed);
        assert_eq!(summaries(&inner).len(), 1);
        for event in receiver.try_iter() {
            seen.apply(event);
        }
        assert_eq!(seen, inner.state);
    }

    #[test]
    fn saved_completions_finish_replayed_agents_after_a_cancelled_parent_turn() {
        let folder =
            std::env::temp_dir().join(format!("compi-replay-agents-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("parent.jsonl");
        // The completion already exists when ACP replays the last running progress.
        let completion = json!({
            "type": "custom_message", "customType": "async-result",
            "content": "<task-result id=\"TaskA\"><output>Reviewed the sample.</output></task-result>",
            "details": { "jobs": [{
                "agentId": "TaskA", "jobId": "TaskA", "type": "task",
                "status": "completed", "durationMs": 9000,
            }] },
        });
        std::fs::write(&path, format!("{completion}\n")).unwrap();
        let steps_path = folder.join("TaskA.jsonl");
        std::fs::write(
            &steps_path,
            format!(
                "{}\n{}\n",
                json!({ "type": "custom", "customType": "tool_execution_start", "data": {
                "toolCallId": "read1", "toolName": "read", "intent": "Reading the sample",
            } }),
                json!({ "type": "message", "message": {
                "role": "toolResult", "toolCallId": "read1", "isError": false,
            } }),
            ),
        )
        .unwrap();
        let mut inner = Inner::default();
        let mut seen = inner.state.clone();
        let (sender, receiver) = mpsc::channel();
        inner.watchers.push(sender);
        start_task(&mut inner);
        inner.update(&task_update(
            "completed",
            json!([agent_progress_item("TaskA", "running", 0.05),]),
        ));
        inner.prompt_ended(Ok(json!({ "stopReason": "cancelled" })));
        assert_eq!(
            tool_entry(&inner, 0).agents[0].status,
            ChatToolStatus::Running
        );

        let mut steps = saved::AgentSteps::default();
        assert!(steps.read(&steps_path));
        inner.agent_steps("TaskA", &steps);
        let mut offset = 0;
        let jobs = saved::finished_agents(&path, &mut offset);
        inner.fill_results(&jobs, now_ms());
        let tool = tool_entry(&inner, 0);
        assert_eq!(tool.status, ChatToolStatus::Completed);
        let agent = &tool.agents[0];
        assert_eq!(agent.status, ChatToolStatus::Completed);
        assert_eq!(agent.result.as_deref(), Some("Reviewed the sample."));
        assert_eq!(agent.steps[0].status, ChatToolStatus::Completed);
        assert!(inner.open_agents().is_empty());
        assert_eq!(summaries(&inner).len(), 1);
        assert!(summaries(&inner)[0].duration_ms >= 9000);
        inner.fill_results(&jobs, now_ms());
        assert_eq!(summaries(&inner).len(), 1);
        for event in receiver.try_iter() {
            seen.apply(event);
        }
        assert_eq!(seen, inner.state);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn late_agent_updates_refresh_their_wave_without_moving_other_cards() {
        let mut inner = Inner::default();
        let mut seen = inner.state.clone();
        let (sender, receiver) = mpsc::channel();
        inner.watchers.push(sender);
        let report = |call: &str, id: &str, status: &str, cost: f64| {
            let mut update =
                task_update("completed", json!([agent_progress_item(id, status, cost)]));
            update["toolCallId"] = json!(call);
            update
        };
        let call = |id| {
            json!({
                "sessionUpdate": "tool_call", "toolCallId": id, "name": "task",
                "kind": "other", "status": "pending",
            })
        };
        start_task(&mut inner);
        inner.update(&report("task1", "TaskA", "running", 0.05));
        inner.update(&call("task2"));
        inner.update(&report("task2", "TaskB", "running", 0.10));
        inner.update(&report("task1", "TaskA", "completed", 0.05));
        inner.update(&report("task2", "TaskB", "completed", 0.10));
        let first_card = inner.state.entries.len() - 1;
        assert_eq!(summaries(&inner)[0].count, 2);

        inner.update(&call("task3"));
        inner.update(&report("task3", "TaskC", "completed", 0.20));
        let second_card = inner.state.entries.len() - 1;
        let second = summaries(&inner)[1].clone();
        let entries = inner.state.entries.len();
        let mut late = task_update(
            "completed",
            json!([
                agent_progress_item("TaskA", "completed", 0.08),
                agent_progress_item("TaskD", "completed", 0.02),
            ]),
        );
        late["rawOutput"]["details"]["progress"][0]["durationMs"] = json!(8000);
        late["rawOutput"]["details"]["progress"][1]["durationMs"] = json!(7000);
        inner.update(&late);
        // Repeated reports do not append another card or change a later wave's card.
        inner.update(&late);
        assert_eq!(inner.state.entries.len(), entries);
        let ChatEntry::AgentsFinished(first) = &inner.state.entries[first_card] else {
            panic!("the original ending card moved");
        };
        assert_eq!((first.tool, first.count), (1, 3));
        assert!(first.duration_ms >= 8000);
        assert!((first.cost.unwrap() - 0.20).abs() < 1e-9);
        assert_eq!(
            inner.state.entries[second_card],
            ChatEntry::AgentsFinished(second)
        );
        assert_eq!(summaries(&inner).len(), 2);
        for event in receiver.try_iter() {
            seen.apply(event);
        }
        assert_eq!(seen, inner.state);
    }

    #[test]
    fn late_final_results_refresh_time_and_zero_cost_without_elapsed_time_drift() {
        let mut inner = Inner::default();
        start_task(&mut inner);
        inner.update(&task_update(
            "in_progress",
            json!([agent_progress_item("TaskA", "running", 0.05),]),
        ));
        // A fixed start makes using the late report's wall clock visibly incorrect.
        let ChatEntry::Tool(tool) = &mut inner.state.entries[0] else {
            unreachable!();
        };
        tool.started_ms = 1000;
        tool.agents[0].started_ms = Some(1000);
        inner.update(&task_update(
            "completed",
            json!([agent_progress_item("TaskA", "completed", 0.10),]),
        ));
        assert_eq!(summaries(&inner)[0].duration_ms, 4000);
        inner.update(&task_update(
            "completed",
            json!([agent_progress_item("TaskA", "completed", 0.20),]),
        ));
        assert_eq!(summaries(&inner)[0].duration_ms, 4000);
        assert_eq!(summaries(&inner)[0].cost, Some(0.20));
        inner.update(&json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "task1", "status": "completed",
            "rawOutput": { "details": { "results": [{
                "id": "TaskA", "exitCode": 0, "durationMs": 7000,
                "usage": { "cost": { "total": 0.0 } }, "output": "Final review.",
            }] } },
        }));
        assert_eq!(summaries(&inner)[0].duration_ms, 7000);
        assert_eq!(summaries(&inner)[0].cost, Some(0.0));
        let jobs = [json!({
            "id": "TaskA", "type": "task", "status": "completed",
            "durationMs": 9000, "resultText": "Final timing.",
        })];
        inner.fill_results(&jobs, now_ms());
        inner.fill_results(&jobs, now_ms());
        assert_eq!(summaries(&inner).len(), 1);
        assert_eq!(summaries(&inner)[0].duration_ms, 9000);
        assert_eq!(summaries(&inner)[0].cost, Some(0.0));
    }

    #[test]
    fn agents_the_harness_left_running_stop_with_it() {
        let mut inner = Inner::default();
        start_task(&mut inner);
        inner.update(&task_update(
            "completed",
            json!([agent_progress_item("TaskA", "running", 0.05)]),
        ));
        inner.stop_agents();
        inner.end_turn();
        let tool = tool_entry(&inner, 0);
        assert_eq!(
            (tool.status, tool.agents[0].status),
            (ChatToolStatus::Stopped, ChatToolStatus::Stopped)
        );
        assert!(summaries(&inner).is_empty());
    }

    #[test]
    fn an_exit_message_keeps_the_last_lines() {
        assert_eq!(last_lines("a\nb\nc", 2), "b\nc");
        assert_eq!(last_lines("a", 3), "a");
    }

    #[test]
    fn queued_prompts_wait_for_a_ready_chat_in_order_and_a_held_queue_for_the_next_prompt() {
        let prompt = |text: &str| Prompt {
            text: text.into(),
            files: Vec::new(),
            images: Vec::new(),
        };
        let last_user = |inner: &Inner| {
            inner
                .state
                .entries
                .iter()
                .rev()
                .find_map(|entry| match entry {
                    ChatEntry::User { text, .. } => Some(text.clone()),
                    _ => None,
                })
        };
        let ready = |inner: &mut Inner| {
            inner.emit(ChatEvent::Status {
                status: ChatStatus::Ready,
            });
        };
        let mut inner = Inner {
            state: ChatState {
                session: Some("s".into()),
                status: ChatStatus::Working { since_ms: 1 },
                ..ChatState::default()
            },
            ..Inner::default()
        };
        inner.enqueue(prompt("one"), false);
        inner.enqueue(prompt("two"), false);
        inner.send_queued();
        assert_eq!(
            inner.state.queued.len(),
            2,
            "nothing goes while a turn runs"
        );

        // A stopped turn holds the queue.
        ready(&mut inner);
        inner.queue_held = true;
        inner.send_queued();
        assert_eq!(inner.state.queued.len(), 2);

        // The next prompt goes first; the queue follows it, oldest first.
        inner.start_prompt(prompt("next"));
        assert_eq!(last_user(&inner).as_deref(), Some("next"));
        ready(&mut inner);
        inner.send_queued();
        assert_eq!(last_user(&inner).as_deref(), Some("one"));
        assert!(inner.state.working());
        let left: Vec<&str> = inner
            .state
            .queued
            .iter()
            .map(|queued| queued.text.as_str())
            .collect();
        assert_eq!(left, ["two"]);

        // A declined steer waits first in line.
        inner.enqueue(prompt("steer"), true);
        assert_eq!(inner.state.queued[0].text, "steer");

        // An edit keeps the prompt's place; an empty one, or one already sent, is refused.
        let steer = inner.state.queued[0].id;
        inner.edit_queued(steer, "steer harder".into()).unwrap();
        assert_eq!(inner.state.queued[0].text, "steer harder");
        assert_eq!(inner.state.queued[1].text, "two");
        assert!(inner.edit_queued(steer, " ".into()).is_err());
        assert!(inner.edit_queued(steer + 100, "gone".into()).is_err());
    }

    #[test]
    fn program_status_shows_what_waits_then_an_unseen_result_until_a_prompt_or_acknowledge() {
        let prompt = || Prompt {
            text: "go".into(),
            files: Vec::new(),
            images: Vec::new(),
        };
        let shown = |inner: &Inner| {
            inner
                .status_shown()
                .map(|(state, kind, message)| (state, kind, message.map(str::to_owned)))
        };
        let title = Some("Fix it".to_owned());
        let mut inner = Inner {
            chat_id: "chat-status-test".into(),
            state: ChatState {
                session: Some("s".into()),
                title: title.clone(),
                status: ChatStatus::Ready,
                ..ChatState::default()
            },
            ..Inner::default()
        };
        assert_eq!(shown(&inner), None, "an idle chat reports nothing");

        inner.start_prompt(prompt());
        assert_eq!(
            shown(&inner),
            Some((ProgramState::Working, None, title.clone()))
        );
        inner.permission(
            json!(1),
            &json!({
                "toolCall": { "toolCallId": "t1", "title": "rm x", "kind": "execute" },
                "options": [{ "optionId": "a", "name": "Allow once", "kind": "allow_once" }],
            }),
        );
        inner.question(
            json!(2),
            &json!({ "message": "Which?", "requestedSchema": { "type": "object", "properties": {} } }),
        );
        assert_eq!(
            shown(&inner),
            Some((
                ProgramState::Blocked,
                Some(BlockedKind::Permission),
                title.clone()
            )),
            "a permission beats a question and a running turn"
        );
        inner.emit(ChatEvent::Permission { permission: None });
        assert_eq!(
            shown(&inner),
            Some((
                ProgramState::Blocked,
                Some(BlockedKind::Question),
                title.clone()
            ))
        );

        inner.prompt_ended(Ok(json!({ "stopReason": "end_turn" })));
        assert_eq!(
            shown(&inner),
            Some((ProgramState::Done, None, title.clone()))
        );
        inner.publish_status();
        assert_eq!(
            inner
                .published
                .as_ref()
                .map(|record| (record.state, record.app.as_deref())),
            Some((ProgramState::Done, Some("OMP")))
        );
        // Sending clears it.
        inner.start_prompt(prompt());
        inner.prompt_ended(Ok(json!({ "stopReason": "cancelled" })));
        assert_eq!(shown(&inner), None, "a cancelled turn leaves no result");

        inner.start_prompt(prompt());
        inner.prompt_ended(Ok(json!({ "stopReason": "end_turn" })));
        inner.unseen = None;
        assert_eq!(shown(&inner), None, "acknowledged");

        inner.start_prompt(prompt());
        inner.prompt_ended(Err("boom".into()));
        assert_eq!(
            shown(&inner),
            Some((ProgramState::Error, None, Some("boom".into())))
        );
        inner.emit(ChatEvent::Reset {
            state: Box::default(),
        });
        assert_eq!(shown(&inner), None, "a restart clears it");

        inner.emit(ChatEvent::Status {
            status: ChatStatus::Failed {
                message: "OMP stopped".into(),
            },
        });
        assert_eq!(
            shown(&inner),
            Some((ProgramState::Error, None, Some("OMP stopped".into())))
        );
        inner.closed = true;
        inner.publish_status();
        assert_eq!(inner.published, None, "a closed chat reports nothing");
    }

    fn text_prompt(text: &str) -> Prompt {
        Prompt {
            text: text.into(),
            files: Vec::new(),
            images: Vec::new(),
        }
    }

    /// A session's `configOptions`: a model and a thinking level.
    fn config_options(model: &str, thinking: &str) -> Value {
        json!([
            { "type": "select", "id": "model", "name": "Model", "category": "model",
              "currentValue": model,
              "options": [{ "value": "sonnet", "name": "Claude Sonnet 5" },
                          { "value": "opus", "name": "Claude Opus 5.5" }] },
            { "type": "select", "id": "effort", "name": "Thinking", "category": "thought_level",
              "currentValue": thinking,
              "options": [{ "value": "low", "name": "low" }, { "value": "high", "name": "high" }] },
        ])
    }

    #[test]
    fn a_new_session_starts_on_the_listed_picks_before_any_prompt_and_a_resumed_one_keeps_its_own()
    {
        let picks_sent = |inner: &Inner| -> Vec<(String, String)> {
            inner
                .acp_state()
                .pending
                .values()
                .filter_map(|pending| match pending {
                    Pending::Pick { option, value, .. } => Some((option.clone(), value.clone())),
                    _ => None,
                })
                .collect()
        };
        let chat = Arc::new(Chat {
            content: ChatContent {
                id: "picks-test".into(),
                harness: ChatHarness::Omp,
                environment: ContentEnvironment::Host,
                folder: "/w".into(),
                session: None,
            },
            instance: None,
            inner: Mutex::new(Inner {
                chat_id: "picks-test".into(),
                picks: ChatPicks {
                    model: Some("opus".into()),
                    thought_level: Some("max".into()),
                },
                ..Inner::default()
            }),
        });
        {
            let mut inner = chat.lock();
            chat.answered(
                &mut inner,
                Pending::NewSession,
                Ok(json!({ "sessionId": "s1", "configOptions": config_options("sonnet", "low") })),
            );
            assert_eq!(picks_sent(&inner), [("model".into(), "opus".into())]);
            assert_eq!(inner.state.status, ChatStatus::Starting);
        }
        // A prompt waits for the picks.
        assert!(chat.send(text_prompt("hi"), ChatDelivery::Prompt).is_err());
        chat.send(text_prompt("hi"), ChatDelivery::Queue).unwrap();
        let mut inner = chat.lock();
        assert!(inner.state.entries.is_empty());
        let id = *inner
            .acp()
            .pending
            .iter()
            .find(|(_, pending)| matches!(pending, Pending::Pick { .. }))
            .unwrap()
            .0;
        let pick = inner.acp().pending.remove(&id).unwrap();
        chat.answered(
            &mut inner,
            pick,
            Ok(json!({ "configOptions": config_options("opus", "low") })),
        );
        // The session lists no thinking level "max": it keeps its own.
        assert!(picks_sent(&inner).is_empty());
        let current = |inner: &Inner, category| {
            inner
                .state
                .option(category)
                .map(|option| option.current.clone())
        };
        assert_eq!(current(&inner, "model").as_deref(), Some("opus"));
        assert_eq!(current(&inner, "thought_level").as_deref(), Some("low"));
        assert!(inner.state.working(), "the queued prompt went once set");
        assert!(
            matches!(inner.state.entries.as_slice(), [ChatEntry::User { text, .. }] if text == "hi")
        );

        // A resumed session keeps its settings.
        inner.emit(ChatEvent::Reset {
            state: Box::new(ChatState {
                session: Some("s0".into()),
                ..ChatState::default()
            }),
        });
        inner.acp().pending.clear();
        chat.answered(
            &mut inner,
            Pending::LoadSession,
            Ok(json!({ "sessionId": "s0", "configOptions": config_options("sonnet", "low") })),
        );
        assert!(picks_sent(&inner).is_empty());
        assert_eq!(current(&inner, "model").as_deref(), Some("sonnet"));
        assert_eq!(inner.state.status, ChatStatus::Ready);
    }

    #[test]
    fn a_prompt_after_a_model_change_follows_a_switch_naming_only_that_change() {
        let user = |text: &str| ChatEntry::User {
            text: text.into(),
            files: Vec::new(),
            images: Vec::new(),
            sent_ms: None,
            ended_ms: None,
        };
        // Sent prompts are timed from sending to their turn's end; replayed ones are not.
        // The times are checked, then left out of the comparison.
        let untimed = |inner: &Inner| -> Vec<ChatEntry> {
            inner
                .state
                .entries
                .iter()
                .map(|entry| match entry {
                    ChatEntry::User {
                        text,
                        sent_ms,
                        ended_ms,
                        ..
                    } => {
                        if let Some(sent) = sent_ms {
                            assert!(ended_ms.is_some_and(|ended| ended >= *sent));
                        }
                        user(text)
                    }
                    entry => entry.clone(),
                })
                .collect()
        };
        let turn = |inner: &mut Inner, text: &str| {
            inner.start_prompt(text_prompt(text));
            inner.prompt_ended(Ok(json!({ "stopReason": "end_turn" })));
        };
        let mut inner = Inner::default();
        inner.session_opened(
            &json!({ "sessionId": "s", "configOptions": config_options("sonnet", "high") }),
        );
        turn(&mut inner, "one");
        inner.options(&config_options("opus", "high"));
        turn(&mut inner, "two");
        turn(&mut inner, "three");
        let switch = ChatEntry::Switch {
            model: Some(ChatChange {
                from: "Claude Sonnet 5".into(),
                to: "Claude Opus 5.5".into(),
            }),
            thought_level: None,
        };
        assert_eq!(
            untimed(&inner),
            [user("one"), switch, user("two"), user("three")]
        );
        assert!(matches!(
            inner.state.entries[0],
            ChatEntry::User {
                sent_ms: Some(_),
                ended_ms: Some(_),
                ..
            }
        ));

        // A resumed session's replayed prompts and its first prompt show none.
        inner.emit(ChatEvent::Reset {
            state: Box::new(ChatState {
                session: Some("s".into()),
                ..ChatState::default()
            }),
        });
        inner.update(&json!({
            "sessionUpdate": "user_message_chunk",
            "content": { "type": "text", "text": "earlier" },
        }));
        inner.session_opened(
            &json!({ "sessionId": "s", "configOptions": config_options("opus", "low") }),
        );
        turn(&mut inner, "four");
        assert_eq!(untimed(&inner), [user("earlier"), user("four")]);
        assert!(matches!(
            inner.state.entries[0],
            ChatEntry::User { sent_ms: None, .. }
        ));
    }

    fn thread_chat(id: &str) -> ChatContent {
        ChatContent {
            id: id.into(),
            harness: ChatHarness::Omp,
            environment: ContentEnvironment::Host,
            folder: "/work".into(),
            session: Some("s1".into()),
        }
    }

    fn running_tool(id: &str) -> ChatEntry {
        ChatEntry::Tool(Box::new(ChatTool {
            id: id.into(),
            name: Some("bash".into()),
            title: "$ make".into(),
            kind: "execute".into(),
            status: ChatToolStatus::Running,
            input: None,
            output: None,
            diffs: Vec::new(),
            locations: Vec::new(),
            started_ms: 5,
            ended_ms: None,
            agents: Vec::new(),
            waits_on_agents: false,
        }))
    }

    #[test]
    fn a_thread_round_trips_through_its_file_and_lists_by_its_first_prompt() {
        let folder = std::env::temp_dir().join(format!("compi-threads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let store = Arc::new(ThreadStore::at(folder.clone()));
        let record = ThreadRecord::new(
            thread_chat("chat-1"),
            None,
            10,
            20,
            vec![
                ChatEntry::User {
                    text: "\n  Fix the build\nthen test".into(),
                    files: Vec::new(),
                    images: Vec::new(),
                    sent_ms: Some(10),
                    ended_ms: Some(20),
                },
                ChatEntry::Agent {
                    text: "Done".into(),
                },
            ],
        );
        store.save(record.clone());
        store.flush();
        assert!(folder.join("chat-1.json").is_file());
        assert!(!folder.join("chat-1.json.tmp").exists());

        let reopened = Arc::new(ThreadStore::at(folder.clone()));
        assert_eq!(reopened.load("chat-1"), Some(record.clone()));
        let listed = reopened.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title.as_deref(), Some("Fix the build"));
        assert_eq!((listed[0].created_ms, listed[0].updated_ms), (10, 20));
        assert_eq!(listed[0].chat.session.as_deref(), Some("s1"));

        // The latest save wins, and the index follows it before the disk does.
        let mut renamed = record.clone();
        renamed.title = Some("Build fix".into());
        renamed.updated_ms = 30;
        reopened.save(renamed.clone());
        assert_eq!(reopened.list()[0].title.as_deref(), Some("Build fix"));
        reopened.flush();
        assert_eq!(
            ThreadStore::at(folder.clone()).load("chat-1"),
            Some(renamed)
        );

        reopened.delete("chat-1");
        assert!(reopened.list().is_empty());
        reopened.flush();
        assert_eq!(ThreadStore::at(folder.clone()).load("chat-1"), None);
        assert!(!ThreadStore::at(folder.clone()).contains("chat-1"));
        // A name that is not a chat ID never becomes a path.
        assert_eq!(reopened.load("../chat-1"), None);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_seeded_chat_settles_what_a_crash_left_open_and_skips_the_sessions_replay() {
        let mut inner = Inner::default();
        inner.seed(ThreadRecord::new(
            thread_chat("chat-seed"),
            Some("Make it".into()),
            5,
            50,
            vec![
                ChatEntry::User {
                    text: "make".into(),
                    files: Vec::new(),
                    images: Vec::new(),
                    sent_ms: Some(5),
                    ended_ms: None,
                },
                running_tool("t1"),
            ],
        ));
        assert_eq!(inner.state.session.as_deref(), Some("s1"));
        assert_eq!(inner.state.title.as_deref(), Some("Make it"));
        assert!(matches!(
            inner.state.entries[0],
            ChatEntry::User {
                ended_ms: Some(50),
                ..
            }
        ));
        let tool = tool_entry(&inner, 1);
        assert_eq!(
            (tool.status, tool.ended_ms),
            (ChatToolStatus::Stopped, Some(50))
        );
        assert_eq!(inner.tools.get("t1"), Some(&1));

        let seeded = inner.state.entries.clone();
        inner.acp().pending.insert(1, Pending::LoadSession);
        inner.update(&json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "make" } }));
        inner.update(&json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "ok" } }));
        inner.update(&json!({
            "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "$ make", "status": "completed",
        }));
        inner.update(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "in_progress" }));
        assert_eq!(inner.state.entries, seeded, "the replay adds nothing");
        inner.update(&json!({ "sessionUpdate": "session_info_update", "title": "Made it" }));
        inner.update(&json!({ "sessionUpdate": "usage_update", "used": 10, "size": 100 }));
        assert_eq!(inner.state.title.as_deref(), Some("Made it"));
        assert!(inner.state.usage.is_some());

        // Once the session has loaded, what the harness reports is new.
        inner.acp().pending.clear();
        inner.update(&json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "next" } }));
        assert_eq!(inner.state.entries.len(), seeded.len() + 1);

        // A session resumed in its place shows its replay.
        inner.emit(ChatEvent::Reset {
            state: Box::default(),
        });
        inner.acp().pending.insert(2, Pending::LoadSession);
        inner.update(&json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "other" } }));
        assert_eq!(inner.state.entries.len(), 1);
    }

    #[test]
    fn a_chat_no_pane_holds_ends_at_once_and_saves_its_thread() {
        use compi_protocol::{PaneContent, PaneId, ServerGeneration, ServerId, TabId};
        use compi_protocol::{WorkspaceSession, WorkspaceTab};
        let folder = std::env::temp_dir().join(format!("compi-threads-end-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let hub = ChatHub {
            threads: Arc::new(ThreadStore::at(folder.clone())),
            ..ChatHub::default()
        };
        let chat = |id: &str| {
            let content = thread_chat(id);
            let mut inner = Inner {
                chat_id: content.id.clone(),
                state: ChatState {
                    session: content.session.clone(),
                    status: ChatStatus::Ready,
                    ..ChatState::default()
                },
                ..Inner::default()
            };
            inner.start_prompt(text_prompt("go"));
            let chat = Arc::new(Chat {
                content,
                instance: None,
                inner: Mutex::new(inner),
            });
            lock(&hub.chats).insert(id.to_owned(), chat.clone());
            chat
        };
        let held = chat("chat-held");
        let closed = chat("chat-closed");
        let workspace = WorkspaceSnapshot {
            server_id: ServerId::new("server"),
            server_generation: ServerGeneration::new("1"),
            revision: 1,
            initialized: true,
            sessions: vec![WorkspaceSession {
                id: compi_protocol::SessionId::new("session-1"),
                label: "Default".into(),
                tabs: vec![WorkspaceTab {
                    id: TabId::new("tab-1"),
                    label: "Chat".into(),
                    layout: LayoutNode::Content {
                        pane_id: PaneId::new("pane-1"),
                        content: PaneContent::Chat(thread_chat("chat-held")),
                    },
                    previous_layout: None,
                    merge: None,
                    group: None,
                    project: None,
                }],
                groups: Vec::new(),
            }],
            surfaces: Vec::new(),
            recovery_message: None,
        };
        hub.retain(&workspace);

        assert!(lock(&hub.chats).contains_key("chat-held"));
        let kept = lock(&held.inner);
        assert!(!kept.closed);
        assert!(kept.state.working(), "a held chat's turn goes on");
        drop(kept);

        assert!(!lock(&hub.chats).contains_key("chat-closed"));
        let ended = lock(&closed.inner);
        assert!(ended.closed);
        assert!(ended.agent.is_none());
        assert!(ended.status_shown().is_none(), "its status goes");
        assert!(matches!(
            ended.state.entries[0],
            ChatEntry::User {
                ended_ms: Some(_),
                ..
            }
        ));
        drop(ended);
        hub.threads.flush();
        let saved = hub
            .threads
            .load("chat-closed")
            .expect("its thread is saved");
        assert!(matches!(
            saved.entries[0],
            ChatEntry::User {
                ended_ms: Some(_),
                ..
            }
        ));
        assert_eq!(saved.chat.session.as_deref(), Some("s1"));
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// A workspace whose one tab holds a content pane for each chat of `ids`.
    fn holding(ids: &[&str]) -> WorkspaceSnapshot {
        use compi_protocol::{PaneContent, PaneId, ServerGeneration, ServerId, TabId};
        use compi_protocol::{WorkspaceSession, WorkspaceTab};
        let tabs = ids
            .iter()
            .enumerate()
            .map(|(index, id)| WorkspaceTab {
                id: TabId::new(format!("tab-{index}")),
                label: "Chat".into(),
                layout: LayoutNode::Content {
                    pane_id: PaneId::new(format!("pane-{index}")),
                    content: PaneContent::Chat(thread_chat(id)),
                },
                previous_layout: None,
                merge: None,
                group: None,
                project: None,
            })
            .collect();
        WorkspaceSnapshot {
            server_id: ServerId::new("server"),
            server_generation: ServerGeneration::new("1"),
            revision: 1,
            initialized: true,
            sessions: vec![WorkspaceSession {
                id: compi_protocol::SessionId::new("session-1"),
                label: "Default".into(),
                tabs,
                groups: Vec::new(),
            }],
            surfaces: Vec::new(),
            recovery_message: None,
        }
    }

    #[test]
    fn a_draft_is_no_thread_until_its_first_prompt_and_closing_it_leaves_nothing() {
        let folder =
            std::env::temp_dir().join(format!("compi-threads-draft-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let hub = ChatHub {
            threads: Arc::new(ThreadStore::at(folder.clone())),
            ..ChatHub::default()
        };
        let draft = |id: &str| {
            let content = thread_chat(id);
            let inner = Inner {
                chat_id: content.id.clone(),
                state: ChatState {
                    session: content.session.clone(),
                    status: ChatStatus::Ready,
                    ..ChatState::default()
                },
                ..Inner::default()
            };
            let chat = Arc::new(Chat {
                content,
                instance: None,
                inner: Mutex::new(inner),
            });
            lock(&hub.chats).insert(id.to_owned(), chat.clone());
            chat
        };
        let sent = draft("chat-sent");
        let untouched = draft("chat-untouched");
        let workspace = holding(&["chat-sent", "chat-untouched"]);
        assert!(
            hub.threads(&workspace).is_empty(),
            "an open draft is not listed"
        );
        assert!(!lock(&untouched.inner).state.thread);

        // Closing an untouched draft ends its chat and saves nothing.
        hub.retain(&holding(&["chat-sent"]));
        hub.threads.flush();
        assert!(lock(&untouched.inner).closed);
        assert!(hub.threads.load("chat-untouched").is_none());
        assert!(hub.threads(&workspace).is_empty());

        // The first prompt makes the draft a thread, which whole states then say.
        lock(&sent.inner).start_prompt(text_prompt("go"));
        assert!(lock(&sent.inner).state.thread);
        hub.retain(&holding(&[]));
        hub.threads.flush();
        let listed = hub.threads(&holding(&[]));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].chat.id, "chat-sent");
        assert_eq!(listed[0].title.as_deref(), Some("go"));
        let _ = std::fs::remove_dir_all(&folder);
    }
}
