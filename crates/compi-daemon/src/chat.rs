//! Native agent chats: the daemon runs each chat's harness (`omp acp`) like a shell and
//! speaks the Agent Client Protocol (JSON-RPC 2.0, one message per line) to it over its
//! standard input and output. The transcript lives here, so a chat keeps going with no
//! window open; windows watch it (`WatchChat`) and get its state, then every change.
//!
//! A chat is keyed by its pane's chat ID and ends when no pane or view holds it any more
//! (`ChatHub::retain`). The harness keeps its own session; a chat that knows its session
//! resumes it (`session/load`) when it starts again, after a restart of the daemon too.
use crate::program_status::ChatStatusHub;
use compi_protocol::chat::{
    ChatAnswer, ChatAnswers, ChatApproval, ChatChoice, ChatCommand, ChatContent, ChatDelivery,
    ChatDiff, ChatEntry, ChatEvent, ChatField, ChatHarness, ChatImage, ChatOption, ChatPermission,
    ChatPermissionOption, ChatPlan, ChatPlanLimit, ChatQuestion, ChatQueued, ChatSessionInfo,
    ChatState, ChatStatus, ChatTool, ChatToolStatus, ChatUsage, MAX_CHAT_ATTACHMENTS,
    MAX_CHAT_DIFF_BYTES, MAX_CHAT_IMAGE_BYTES, MAX_CHAT_IMAGES, MAX_CHAT_PROMPT_BYTES,
    MAX_CHAT_QUEUED, MAX_CHAT_TOOL_INPUT_BYTES, MAX_CHAT_TOOL_OUTPUT_BYTES,
};
use compi_protocol::program_status::{BlockedKind, ProgramState, ProgramStatusRecord};
use compi_protocol::{ContentEnvironment, LayoutNode, WorkspaceSnapshot};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::ops::{Deref, DerefMut};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod saved;

/// The ACP version Compi speaks.
const ACP_VERSION: u64 = 1;
/// Standard error kept to explain an exit.
const STDERR_TAIL_BYTES: usize = 2048;
/// How long a stopping harness may take to exit on its own once its input closes.
const STOP_GRACE: Duration = Duration::from_millis(500);
/// How long a window waits for the harness to list its sessions.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// Pages of a session listing read at most (OMP's are 50 sessions long).
const MAX_LIST_PAGES: usize = 40;
/// A whole state sent to a window stays this small; tool output and diffs of the oldest
/// calls are left out beyond it, so a long session still fits one control frame.
const WATCH_STATE_BYTES: usize = 6 * 1024 * 1024;
/// How long the lookup of OMP's own approval setting may take; the harness starts without
/// it after that.
const APPROVAL_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// The modes `omp acp --approval-mode` takes.
const OMP_APPROVAL_MODES: [&str; 4] = ["always-ask", "always-ask-cached", "write", "yolo"];
/// `omp` in a login shell, its arguments to follow. Where `omp` is installed when a login
/// shell's search path lacks it: its install script's and Bun's folders.
const UNIX_COMMAND: &str = r#"PATH="$PATH:$HOME/.local/bin:$HOME/.bun/bin" exec omp"#;
#[cfg(windows)]
const WSL_EXE: &str = r"C:\Windows\System32\wsl.exe";

/// Every chat this daemon runs.
#[derive(Default)]
pub struct ChatHub {
    chats: Mutex<HashMap<String, Arc<Chat>>>,
    status: ChatStatusHub,
}

/// The daemon's chats.
pub fn hub() -> &'static ChatHub {
    static HUB: LazyLock<ChatHub> = LazyLock::new(ChatHub::default);
    &HUB
}

impl ChatHub {
    /// The chat's whole state and a receiver of its later changes, starting the chat if it
    /// is not running. `approval` applies from the harness's next start, this one when it
    /// starts it.
    pub fn watch(
        &self,
        content: &ChatContent,
        approval: ChatApproval,
    ) -> (ChatState, mpsc::Receiver<ChatEvent>) {
        let chat = {
            let mut chats = lock(&self.chats);
            chats
                .entry(content.id.clone())
                .or_insert_with(|| Chat::start(content.clone(), approval))
                .clone()
        };
        let mut inner = chat.lock();
        inner.approval = approval;
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
        // RFC 3339 times in UTC, as OMP writes them, sort as text.
        sessions.sort_by(|a, b| b.updated.cmp(&a.updated));
        saved::fill(&chat.content, &mut sessions);
        Ok(sessions)
    }

    pub fn resume(
        &self,
        id: &str,
        session: Option<String>,
        approval: ChatApproval,
    ) -> Result<(), String> {
        let chat = self.get(id)?;
        chat.lock().approval = approval;
        chat.resume(session)
    }

    pub fn restart(&self, id: &str, approval: ChatApproval) -> Result<(), String> {
        let chat = self.get(id)?;
        chat.lock().approval = approval;
        chat.restart();
        Ok(())
    }

    /// Ends every chat no pane or view of `workspace` holds any more.
    pub fn retain(&self, workspace: &WorkspaceSnapshot) {
        let mut chats = lock(&self.chats);
        if chats.is_empty() {
            return;
        }
        let mut held = HashSet::new();
        for tab in workspace.sessions.iter().flat_map(|session| &session.tabs) {
            chat_ids(&tab.layout, &mut held);
        }
        chats.retain(|id, chat| {
            let keep = held.contains(id);
            if !keep {
                chat.close();
            }
            keep
        });
    }

    fn get(&self, id: &str) -> Result<Arc<Chat>, String> {
        lock(&self.chats)
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no chat {id} is running; watch it first"))
    }
}

fn chat_ids(node: &LayoutNode, output: &mut HashSet<String>) {
    match node {
        LayoutNode::Split { first, second, .. } => {
            chat_ids(first, output);
            chat_ids(second, output);
        }
        leaf => {
            if let Some(chat) = leaf.content().and_then(|content| content.chat()) {
                output.insert(chat.id.clone());
            }
        }
    }
}

/// `state` within `budget` bytes of JSON: the oldest tool calls lose their output, diffs
/// and input until it fits.
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

struct Chat {
    content: ChatContent,
    inner: Mutex<Inner>,
}

/// What a request Compi sent the harness was for.
enum Pending {
    Initialize,
    NewSession,
    LoadSession,
    Prompt,
    /// A prompt into the running turn; it waits its turn when the harness declines.
    Steer(Prompt),
    /// OMP's subscription plans and what is left of them (`_omp/usage`).
    Plans,
    SetOption,
    /// The harness's sessions for a window, page by page (`nextCursor`).
    List {
        reply: mpsc::Sender<Result<Vec<ChatSessionInfo>, String>>,
        listed: Vec<ChatSessionInfo>,
        pages: usize,
    },
    /// Pages of the listing searched for the folder `session` was saved under, to load
    /// it there.
    Locate {
        session: String,
        pages: usize,
    },
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

    /// As ACP content blocks: the text, a link per file, the images.
    fn blocks(&self) -> Vec<Value> {
        let mut blocks = vec![json!({ "type": "text", "text": self.text })];
        blocks.extend(self.files.iter().map(|path| {
            json!({
                "type": "resource_link",
                "uri": file_uri(path),
                "name": file_name(path),
            })
        }));
        blocks.extend(
            self.images.iter().map(
                |image| json!({ "type": "image", "mimeType": image.mime, "data": image.data }),
            ),
        );
        blocks
    }

    fn image_names(&self) -> Vec<String> {
        self.images.iter().map(|image| image.name.clone()).collect()
    }

    /// As the transcript shows it, once the harness has it.
    fn entry(&self) -> ChatEntry {
        ChatEntry::User {
            text: self.text.clone(),
            files: self.files.clone(),
            images: self.image_names(),
        }
    }
}

#[derive(Default)]
struct Inner {
    /// The chat's ID, which its program status goes under.
    chat_id: String,
    state: ChatState,
    /// How the harness asks before running tools, from its next start.
    approval: ChatApproval,
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
    can_load: bool,
    can_list: bool,
    /// The folder each listed session was saved under. OMP lists a folder's sessions
    /// under every name of it (a link and its target) but loads one only under the name
    /// it was saved with.
    session_folders: HashMap<String, String>,
    /// The harness takes prompts into a running turn (`_session/steering`).
    can_steer: bool,
    /// Prompts for after the running turn, in order, by ID; `state.queued` shows them.
    queue: VecDeque<(u64, Prompt)>,
    next_queued: u64,
    /// A turn that did not end on its own holds the queue until a prompt is sent or
    /// queued.
    queue_held: bool,
    /// A plans request is out; another waits for it.
    reading_plans: bool,
    next_request: u64,
    pending: HashMap<u64, Pending>,
    /// Tool call IDs to their transcript entries.
    tools: HashMap<String, usize>,
    /// The JSON-RPC ID of the permission request `state.permission` shows.
    permission_request: Option<Value>,
    next_permission: u64,
    /// The JSON-RPC ID of the form `state.question` shows, and how to send its answers.
    question_request: Option<(Value, Vec<FieldShape>)>,
    next_question: u64,
    /// When the transcript last moved (a prompt, a reply chunk, a tool report): a thought
    /// that follows began then, since the harness sends its text only once it is done.
    activity_ms: u64,
}

/// What the user has not seen yet of a chat that waits.
#[derive(Debug, PartialEq)]
enum Unseen {
    Done,
    /// The turn failed, or the harness did; the message the transcript or status shows.
    Error(String),
}

/// A chat's locked state. Letting go of it publishes the chat's program status, so
/// watchers see where the changes made under the lock ended, never a step on the way.
struct Locked<'a>(MutexGuard<'a, Inner>);

impl Deref for Locked<'_> {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.0
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        self.0.publish_status();
    }
}

/// How a question field's answer goes back to the harness.
#[derive(Clone)]
struct FieldShape {
    key: String,
    /// The schema's value type: `string`, `array`, `boolean`, `number` or `integer`.
    kind: String,
    /// The companion free-text key, when the field has one.
    other: Option<String>,
}

struct Agent {
    child: Child,
    input: ChildStdin,
    stderr: Arc<Mutex<String>>,
    #[cfg(windows)]
    _job: Option<std::os::windows::io::OwnedHandle>,
}

impl Chat {
    fn start(content: ChatContent, approval: ChatApproval) -> Arc<Self> {
        let chat = Arc::new(Self {
            inner: Mutex::new(Inner {
                chat_id: content.id.clone(),
                state: ChatState {
                    harness: content.harness,
                    session: content.session.clone(),
                    ..ChatState::default()
                },
                approval,
                ..Inner::default()
            }),
            content,
        });
        chat.spawn(&mut chat.lock());
        chat
    }

    fn lock(&self) -> Locked<'_> {
        Locked(lock(&self.inner))
    }

    /// Starts the harness as the chat's approval says. OMP's own setting, which `omp acp`
    /// does not read, is looked up first on a thread of its own, no lock held; the status
    /// stays `Starting` meanwhile.
    fn spawn(self: &Arc<Self>, inner: &mut Inner) {
        inner.generation += 1;
        inner.initialized = false;
        let generation = inner.generation;
        if let Some(mode) = inner.approval.omp_mode() {
            self.launch(inner, generation, Some(mode));
            return;
        }
        let chat = self.clone();
        let looking_up = thread::Builder::new()
            .name("compi-chat-approval".into())
            .spawn(move || {
                let mode = configured_approval(&chat.content);
                let mut inner = chat.lock();
                if !inner.closed && inner.generation == generation {
                    chat.launch(&mut inner, generation, mode);
                }
            });
        if looking_up.is_err() {
            self.launch(inner, generation, None);
        }
    }

    /// Runs the harness, with `approval_mode` when there is one, and asks it to
    /// initialize; the rest follows its answers.
    fn launch(
        self: &Arc<Self>,
        inner: &mut Inner,
        generation: u64,
        approval_mode: Option<&'static str>,
    ) {
        let mut args = vec!["acp"];
        if let Some(mode) = approval_mode {
            args.extend(["--approval-mode", mode]);
        }
        let mut child = match command(&self.content, &args).and_then(|mut command| {
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
        inner.request(
            "initialize",
            json!({
                "protocolVersion": ACP_VERSION,
                "clientInfo": { "name": "compi", "version": env!("CARGO_PKG_VERSION") },
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                    "elicitation": { "form": {} },
                },
            }),
            Pending::Initialize,
        );
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
    /// `tail` with its standard error, to explain an exit.
    fn read(self: Arc<Self>, output: impl Read, generation: u64, tail: Arc<Mutex<String>>) {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(line) else {
                push_tail(&tail, line);
                continue;
            };
            let mut inner = self.lock();
            if inner.closed || inner.generation != generation {
                return;
            }
            self.handle(&mut inner, message);
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
        inner.end_turn();
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Failed { message },
        });
    }

    fn handle(self: &Arc<Self>, inner: &mut Inner, message: Value) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => inner.agent_request(method, id, &message["params"]),
            (Some("session/update"), None) => inner.update(&message["params"]["update"]),
            (Some(_), None) => {}
            (None, Some(id)) => {
                let Some(pending) = id.as_u64().and_then(|id| inner.pending.remove(&id)) else {
                    return;
                };
                let result = match message.get("error") {
                    Some(error) => Err(error_text(error)),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                self.answered(inner, pending, result);
            }
            (None, None) => {}
        }
    }

    fn answered(&self, inner: &mut Inner, pending: Pending, result: Result<Value, String>) {
        let harness = self.content.harness.name();
        match (pending, result) {
            (Pending::Initialize, Ok(result)) => {
                let capabilities = &result["agentCapabilities"];
                inner.initialized = true;
                inner.can_load = capabilities["loadSession"].as_bool().unwrap_or(false);
                inner.can_list = capabilities["sessionCapabilities"]["list"].is_object();
                inner.can_steer = result["_meta"]["steering"]["supported"]
                    .as_bool()
                    .unwrap_or(false);
                let session = inner.state.session.clone();
                self.open_session(inner, session);
            }
            (Pending::Initialize, Err(error)) => inner.emit(ChatEvent::Status {
                status: ChatStatus::Failed {
                    message: format!("{harness} didn't start: {error}"),
                },
            }),
            (Pending::NewSession | Pending::LoadSession, Ok(result)) => {
                inner.end_thought();
                if let Some(session) = result["sessionId"].as_str() {
                    inner.emit(ChatEvent::Session {
                        session: Some(session.to_owned()),
                    });
                }
                inner.options(&result["configOptions"]);
                inner.emit(ChatEvent::Status {
                    status: ChatStatus::Ready,
                });
                inner.send_queued();
                self.read_plans(inner);
            }
            (Pending::LoadSession, Err(error)) => {
                inner.emit(ChatEvent::Session { session: None });
                inner.emit(ChatEvent::Entry {
                    entry: ChatEntry::Error {
                        message: format!("Couldn't resume the earlier session: {error}"),
                    },
                });
                self.open_session(inner, None);
            }
            (Pending::NewSession, Err(error)) => inner.emit(ChatEvent::Status {
                status: ChatStatus::Failed {
                    message: format!("{harness} couldn't start a session: {error}"),
                },
            }),
            (Pending::SetOption, Ok(result)) => inner.options(&result["configOptions"]),
            (Pending::SetOption, Err(message)) => inner.emit(ChatEvent::Entry {
                entry: ChatEntry::Error {
                    message: format!("{harness} didn't change the setting: {message}"),
                },
            }),
            (
                Pending::List {
                    reply,
                    mut listed,
                    pages,
                },
                Ok(result),
            ) => {
                inner.note_folders(&result);
                listed.extend(sessions(&result));
                match next_cursor(&result, pages) {
                    Some(cursor) => self.list_page(
                        inner,
                        Some(cursor),
                        Pending::List {
                            reply,
                            listed,
                            pages: pages + 1,
                        },
                    ),
                    None => {
                        let _ = reply.send(Ok(listed));
                    }
                }
            }
            (Pending::List { reply, .. }, Err(error)) => {
                let _ = reply.send(Err(error));
            }
            (Pending::Locate { session, pages }, result) => {
                let cursor = result.ok().and_then(|result| {
                    inner.note_folders(&result);
                    next_cursor(&result, pages)
                });
                match (inner.session_folders.get(&session).cloned(), cursor) {
                    (Some(folder), _) => self.load_session(inner, session, &folder),
                    (None, Some(cursor)) => self.list_page(
                        inner,
                        Some(cursor),
                        Pending::Locate {
                            session,
                            pages: pages + 1,
                        },
                    ),
                    (None, None) => self.load_session(inner, session, &self.content.folder),
                }
            }
            (Pending::Steer(prompt), Ok(result)) if result["outcome"] == "injected" => {
                inner.end_thought();
                inner.activity_ms = now_ms();
                inner.emit(ChatEvent::Entry {
                    entry: prompt.entry(),
                });
                // A steer turns down the permission the harness was waiting on.
                inner.permission_request = None;
                if inner.state.permission.is_some() {
                    inner.emit(ChatEvent::Permission { permission: None });
                }
            }
            // The turn ended first, or the harness declined: the prompt waits its turn,
            // first in line.
            (Pending::Steer(prompt), _) => {
                inner.enqueue(prompt, true);
                inner.send_queued();
            }
            (Pending::Plans, result) => {
                inner.reading_plans = false;
                // A harness without the method, or one that failed, keeps what it had.
                if let Ok(result) = result {
                    let plans = plans(&result);
                    if plans != inner.state.plans {
                        inner.emit(ChatEvent::Plans { plans });
                    }
                }
            }
            (Pending::Prompt, result) => {
                inner.prompt_ended(result);
                self.read_plans(inner);
            }
        }
    }

    /// Asks OMP for its subscription plans, unless a request is out already.
    fn read_plans(&self, inner: &mut Inner) {
        if self.content.harness != ChatHarness::Omp || inner.reading_plans {
            return;
        }
        inner.reading_plans = true;
        inner.request("_omp/usage", json!({}), Pending::Plans);
    }

    /// Loads `session`, whose history the harness replays, in the folder it was saved
    /// under (looked up in the listing first when not known yet), or starts a new one.
    fn open_session(&self, inner: &mut Inner, session: Option<String>) {
        match session.filter(|_| inner.can_load) {
            Some(session) => match inner.session_folders.get(&session).cloned() {
                Some(folder) => self.load_session(inner, session, &folder),
                None if inner.can_list => {
                    self.list_page(inner, None, Pending::Locate { session, pages: 1 });
                }
                None => self.load_session(inner, session, &self.content.folder),
            },
            None => inner.request(
                "session/new",
                json!({ "cwd": self.content.folder, "mcpServers": [] }),
                Pending::NewSession,
            ),
        }
    }

    fn load_session(&self, inner: &mut Inner, session: String, folder: &str) {
        inner.request(
            "session/load",
            json!({ "sessionId": session, "cwd": folder, "mcpServers": [] }),
            Pending::LoadSession,
        );
    }

    /// Asks for a page of the sessions in the chat's folder; `None` is the first.
    fn list_page(&self, inner: &mut Inner, cursor: Option<String>, pending: Pending) {
        let mut params = json!({ "cwd": self.content.folder });
        if let Some(cursor) = cursor {
            params["cursor"] = cursor.into();
        }
        inner.request("session/list", params, pending);
    }

    fn ready_session(&self, inner: &Inner) -> Result<String, String> {
        let harness = self.content.harness.name();
        match &inner.state.status {
            ChatStatus::Ready => {}
            ChatStatus::Starting => return Err(format!("{harness} is still starting")),
            ChatStatus::Working { .. } => return Err(format!("{harness} is still working")),
            ChatStatus::Failed { .. } => return Err(format!("{harness} isn't running")),
        }
        inner
            .state
            .session
            .clone()
            .ok_or_else(|| format!("{harness} has no session yet"))
    }

    /// Hands `prompt` to the harness as `delivery` says.
    fn send(&self, prompt: Prompt, delivery: ChatDelivery) -> Result<(), String> {
        prompt.check()?;
        let harness = self.content.harness.name();
        let mut inner = self.lock();
        let steer = delivery == ChatDelivery::Steer;
        if steer && inner.state.working() && inner.can_steer {
            let session = inner
                .state
                .session
                .clone()
                .ok_or_else(|| format!("{harness} has no session yet"))?;
            let blocks = prompt.blocks();
            inner.request(
                "_session/steering",
                json!({ "sessionId": session, "prompt": blocks }),
                Pending::Steer(prompt),
            );
        } else if delivery == ChatDelivery::Prompt
            || (steer && matches!(inner.state.status, ChatStatus::Ready))
        {
            let session = self.ready_session(&inner)?;
            inner.start_prompt(session, prompt);
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
        inner.cancel_requests();
        if let Some(session) = inner.state.session.clone() {
            inner.write(&json!({
                "jsonrpc": "2.0",
                "method": "session/cancel",
                "params": { "sessionId": session },
            }));
        }
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
        let Some(request) = inner.permission_request.take() else {
            return Err("that permission was already answered".into());
        };
        let outcome = match option {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        inner.respond(request, json!({ "outcome": outcome }));
        inner.emit(ChatEvent::Permission { permission: None });
        Ok(())
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
        let Some((request, shapes)) = inner.question_request.take() else {
            return Err("that question was already answered".into());
        };
        let result = match answers {
            Some(answers) => {
                json!({ "action": "accept", "content": form_content(&shapes, answers) })
            }
            None => json!({ "action": "decline" }),
        };
        inner.respond(request, result);
        inner.emit(ChatEvent::Question { question: None });
        Ok(())
    }

    fn set_option(&self, option: String, value: String) -> Result<(), String> {
        let mut inner = self.lock();
        let Some(shown) = inner.state.options.iter().find(|shown| shown.id == option) else {
            return Err(format!("the harness has no setting {option}"));
        };
        if !shown.choices.iter().any(|choice| choice.value == value) {
            return Err(format!("{value} is not a choice of {}", shown.name));
        }
        let Some(session) = inner.state.session.clone() else {
            return Err("the harness has no session yet".into());
        };
        inner.request(
            "session/set_config_option",
            json!({ "sessionId": session, "configId": option, "value": value }),
            Pending::SetOption,
        );
        Ok(())
    }

    fn list_sessions(
        &self,
    ) -> Result<mpsc::Receiver<Result<Vec<ChatSessionInfo>, String>>, String> {
        let mut inner = self.lock();
        if !inner.initialized || inner.agent.is_none() {
            return Err(format!("{} isn't running", self.content.harness.name()));
        }
        if !inner.can_list {
            return Err(format!(
                "{} doesn't list its sessions",
                self.content.harness.name()
            ));
        }
        let (reply, receiver) = mpsc::channel();
        self.list_page(
            &mut inner,
            None,
            Pending::List {
                reply,
                listed: Vec::new(),
                pages: 1,
            },
        );
        Ok(receiver)
    }

    /// Shows `session`, its history replayed, or a new session, in the running harness;
    /// a harness that isn't running starts again on it.
    fn resume(self: &Arc<Self>, session: Option<String>) -> Result<(), String> {
        compi_protocol::chat::validate_session(session.as_deref())?;
        let mut inner = self.lock();
        if inner.state.working() {
            return Err("stop the running turn first".into());
        }
        let state = ChatState {
            harness: self.content.harness,
            session: session.clone(),
            ..ChatState::default()
        };
        if inner.initialized && inner.agent.is_some() {
            inner.cancel_requests();
            inner.emit(ChatEvent::Reset {
                state: Box::new(state),
            });
            self.open_session(&mut inner, session);
        } else {
            inner.stop();
            inner.emit(ChatEvent::Reset {
                state: Box::new(state),
            });
            self.spawn(&mut inner);
        }
        Ok(())
    }

    /// Ends the harness and starts it again; a chat with a session resumes it, its
    /// history replayed.
    fn restart(self: &Arc<Self>) {
        let mut inner = self.lock();
        inner.stop();
        let state = ChatState {
            harness: self.content.harness,
            session: inner.state.session.clone(),
            ..ChatState::default()
        };
        inner.emit(ChatEvent::Reset {
            state: Box::new(state),
        });
        self.spawn(&mut inner);
    }

    fn close(&self) {
        let mut inner = self.lock();
        inner.closed = true;
        inner.stop();
        inner.watchers.clear();
    }
}

impl Inner {
    /// Applies `event` and sends it to every watcher still listening.
    fn emit(&mut self, event: ChatEvent) {
        match &event {
            ChatEvent::Reset { .. } => {
                self.tools.clear();
                self.queue.clear();
                self.queue_held = false;
                self.unseen = None;
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

    /// Starts a turn on `prompt`.
    fn start_prompt(&mut self, session: String, prompt: Prompt) {
        let blocks = prompt.blocks();
        self.unseen = None;
        self.emit(ChatEvent::Entry {
            entry: prompt.entry(),
        });
        let since_ms = now_ms();
        self.activity_ms = since_ms;
        self.queue_held = false;
        self.emit(ChatEvent::Status {
            status: ChatStatus::Working { since_ms },
        });
        self.request(
            "session/prompt",
            json!({ "sessionId": session, "prompt": blocks }),
            Pending::Prompt,
        );
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
        let Some(session) = self.state.session.clone() else {
            return;
        };
        let Some((_, prompt)) = self.queue.pop_front() else {
            return;
        };
        self.publish_queue();
        self.start_prompt(session, prompt);
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

    fn request(&mut self, method: &str, params: Value, pending: Pending) {
        self.next_request += 1;
        let id = self.next_request;
        self.pending.insert(id, pending);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn respond(&mut self, id: Value, result: Value) {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// Tells the harness nobody will answer its open permission or question.
    fn cancel_requests(&mut self) {
        if let Some(request) = self.permission_request.take() {
            self.respond(request, json!({ "outcome": { "outcome": "cancelled" } }));
        }
        if let Some((request, _)) = self.question_request.take() {
            self.respond(request, json!({ "action": "cancel" }));
        }
        if self.state.permission.is_some() {
            self.emit(ChatEvent::Permission { permission: None });
        }
        if self.state.question.is_some() {
            self.emit(ChatEvent::Question { question: None });
        }
    }

    /// Windows waiting on a session listing hear the harness is gone; nothing it was
    /// asked is out any more.
    fn fail_pending(&mut self) {
        for (_, pending) in self.pending.drain() {
            if let Pending::List { reply, .. } = pending {
                let _ = reply.send(Err("the harness stopped".into()));
            }
        }
        self.reading_plans = false;
    }

    /// Remembers the folder each session in a listing page was saved under.
    fn note_folders(&mut self, result: &Value) {
        for session in result["sessions"].as_array().into_iter().flatten() {
            if let (Some(id), Some(folder)) =
                (session["sessionId"].as_str(), session["cwd"].as_str())
            {
                self.session_folders
                    .insert(id.to_owned(), folder.to_owned());
            }
        }
    }

    /// A turn ended: tool calls still open will not report again, and nobody needs to
    /// answer its permission or question any more.
    fn end_turn(&mut self) {
        self.end_thought();
        let now = now_ms();
        let open: Vec<usize> = self
            .state
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                ChatEntry::Tool(tool) if tool.status.is_open() => Some(index),
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
        self.permission_request = None;
        self.question_request = None;
        if self.state.permission.is_some() {
            self.emit(ChatEvent::Permission { permission: None });
        }
        if self.state.question.is_some() {
            self.emit(ChatEvent::Question { question: None });
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
        self.permission_request = None;
        self.question_request = None;
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

    /// The harness's settings, from `configOptions`; only select settings are offered.
    fn options(&mut self, value: &Value) {
        let Some(list) = value.as_array() else {
            return;
        };
        let options: Vec<ChatOption> = list.iter().filter_map(option).collect();
        if options != self.state.options {
            self.emit(ChatEvent::Options { options });
        }
    }

    /// A request from the harness: a permission or a question waits for a window; Compi
    /// offers no other client methods.
    fn agent_request(&mut self, method: &str, id: Value, params: &Value) {
        match method {
            "session/request_permission" => self.permission(id, params),
            "elicitation/create" if params["mode"].as_str().unwrap_or("form") == "form" => {
                self.question(id, params)
            }
            _ => self.write(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Compi does not offer {method}") },
            })),
        }
    }

    fn permission(&mut self, id: Value, params: &Value) {
        // A newer request replaces one nobody answered.
        if let Some(previous) = self.permission_request.take() {
            self.respond(previous, json!({ "outcome": { "outcome": "cancelled" } }));
        }
        let call = &params["toolCall"];
        // The card sits on its tool call, which the harness may not have reported yet.
        self.tool(call);
        let title = call["title"]
            .as_str()
            .filter(|title| !title.is_empty())
            .or_else(|| call["rawInput"]["command"].as_str())
            .unwrap_or("Run a tool")
            .to_owned();
        let options = params["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|option| {
                Some(ChatPermissionOption {
                    id: option["optionId"].as_str()?.to_owned(),
                    name: option["name"].as_str()?.to_owned(),
                    kind: option["kind"].as_str().unwrap_or_default().to_owned(),
                })
            })
            .collect();
        self.next_permission += 1;
        self.permission_request = Some(id);
        let permission = ChatPermission {
            id: self.next_permission,
            tool: call["toolCallId"].as_str().map(str::to_owned),
            title,
            options,
        };
        self.emit(ChatEvent::Permission {
            permission: Some(permission),
        });
    }

    fn question(&mut self, id: Value, params: &Value) {
        if let Some((previous, _)) = self.question_request.take() {
            self.respond(previous, json!({ "action": "cancel" }));
        }
        let (fields, shapes) = form_fields(&params["requestedSchema"]);
        self.next_question += 1;
        self.question_request = Some((id, shapes));
        let question = ChatQuestion {
            id: self.next_question,
            message: params["message"].as_str().unwrap_or_default().to_owned(),
            fields,
        };
        self.emit(ChatEvent::Question {
            question: Some(question),
        });
    }

    /// One `session/update` notification.
    fn update(&mut self, update: &Value) {
        let kind = update["sessionUpdate"].as_str().unwrap_or_default();
        if kind != "agent_thought_chunk" && kind.ends_with("_chunk")
            || kind.starts_with("tool_call")
        {
            self.end_thought();
        }
        let text = || {
            update["content"]["text"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        match kind {
            "agent_message_chunk" => {
                if let Some(text) = text() {
                    self.append(
                        text,
                        |text| ChatEntry::Agent { text },
                        |entry| matches!(entry, ChatEntry::Agent { .. }),
                    );
                    self.activity_ms = now_ms();
                }
            }
            "agent_thought_chunk" => {
                if let Some(text) = text() {
                    let started_ms = match self.activity_ms {
                        0 => now_ms(),
                        since => since,
                    };
                    self.append(
                        text,
                        |text| ChatEntry::Thought {
                            text,
                            started_ms,
                            ended_ms: None,
                        },
                        |entry| matches!(entry, ChatEntry::Thought { ended_ms: None, .. }),
                    );
                }
            }
            "user_message_chunk" => self.user_chunk(&update["content"]),
            "tool_call" | "tool_call_update" => {
                self.tool(update);
                self.activity_ms = now_ms();
            }
            "config_option_update" => self.options(&update["configOptions"]),
            "available_commands_update" => {
                let commands: Vec<ChatCommand> = update["availableCommands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|command| {
                        Some(ChatCommand {
                            name: command["name"].as_str()?.to_owned(),
                            description: command["description"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                            hint: command["input"]["hint"].as_str().map(str::to_owned),
                        })
                    })
                    .collect();
                if commands != self.state.commands {
                    self.emit(ChatEvent::Commands { commands });
                }
            }
            "usage_update" => {
                let usage = ChatUsage {
                    used: update["used"].as_u64().unwrap_or_default(),
                    size: update["size"].as_u64().unwrap_or_default(),
                    cost: update["cost"]["amount"].as_f64(),
                    currency: update["cost"]["currency"].as_str().map(str::to_owned),
                };
                self.emit(ChatEvent::Usage { usage: Some(usage) });
            }
            "session_info_update" => {
                if let Some(title) = update.get("title") {
                    let title = title
                        .as_str()
                        .filter(|title| !title.is_empty())
                        .map(str::to_owned);
                    if title != self.state.title {
                        self.emit(ChatEvent::Title { title });
                    }
                }
            }
            _ => {}
        }
    }

    /// A piece of a sent prompt, as a resumed session replays it.
    fn user_chunk(&mut self, content: &Value) {
        match content["type"].as_str() {
            Some("text") => {
                if let Some(text) = content["text"].as_str().filter(|text| !text.is_empty()) {
                    self.append(
                        text.to_owned(),
                        |text| ChatEntry::User {
                            text,
                            files: Vec::new(),
                            images: Vec::new(),
                        },
                        |entry| matches!(entry, ChatEntry::User { .. }),
                    );
                }
            }
            Some("resource_link" | "resource") => {
                let uri = content["uri"]
                    .as_str()
                    .or_else(|| content["resource"]["uri"].as_str())
                    .unwrap_or_default();
                let path = path_of_uri(uri);
                self.attach_replayed(|files, _| files.push(path));
            }
            // A replayed image has no name; it is counted, not shown.
            Some("image") => self.attach_replayed(|_, images| images.push("image".into())),
            _ => {}
        }
    }

    /// Adds a replayed attachment to the prompt it belongs to: the last entry when it is
    /// a prompt, else a prompt of its own.
    fn attach_replayed(&mut self, add: impl FnOnce(&mut Vec<String>, &mut Vec<String>)) {
        let last = self.state.entries.len().checked_sub(1);
        match last.and_then(|index| self.state.entries.get(index).map(|entry| (index, entry))) {
            Some((
                index,
                ChatEntry::User {
                    text,
                    files,
                    images,
                },
            )) => {
                let (mut files, mut images) = (files.clone(), images.clone());
                add(&mut files, &mut images);
                let entry = ChatEntry::User {
                    text: text.clone(),
                    files,
                    images,
                };
                self.emit(ChatEvent::Replace { index, entry });
            }
            _ => {
                let (mut files, mut images) = (Vec::new(), Vec::new());
                add(&mut files, &mut images);
                self.emit(ChatEvent::Entry {
                    entry: ChatEntry::User {
                        text: String::new(),
                        files,
                        images,
                    },
                });
            }
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

    /// A tool call or an update to one: fields present replace what was reported.
    fn tool(&mut self, update: &Value) {
        let Some(id) = update["toolCallId"].as_str() else {
            return;
        };
        let known = self.tools.get(id).copied();
        let mut tool = match known.and_then(|index| self.state.entries.get(index)) {
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
            },
        };
        if let Some(name) = update["name"].as_str() {
            tool.name = Some(name.to_owned());
        }
        if let Some(title) = update["title"].as_str().filter(|title| !title.is_empty()) {
            tool.title = title.to_owned();
        }
        if tool.title.is_empty() {
            tool.title = tool.name.clone().unwrap_or_else(|| "Tool".into());
        }
        if let Some(kind) = update["kind"].as_str() {
            tool.kind = kind.to_owned();
        }
        if let Some(status) = update["status"].as_str() {
            tool.status = match status {
                "in_progress" => ChatToolStatus::Running,
                "completed" => ChatToolStatus::Completed,
                "failed" => ChatToolStatus::Failed,
                _ => ChatToolStatus::Pending,
            };
            if !tool.status.is_open() && tool.ended_ms.is_none() {
                tool.ended_ms = Some(now_ms());
            }
        }
        if let Some(input) = update.get("rawInput").filter(|input| !input.is_null()) {
            let text = match input {
                Value::String(text) => text.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            };
            tool.input = Some(head(text, MAX_CHAT_TOOL_INPUT_BYTES));
        }
        if let Some(locations) = update["locations"].as_array() {
            tool.locations = locations
                .iter()
                .filter_map(|location| location["path"].as_str().map(str::to_owned))
                .collect();
        }
        let raw_output = update.get("rawOutput").and_then(|output| {
            text_of(&output["content"]).or_else(|| output.as_str().map(str::to_owned))
        });
        if let Some(content) = update["content"].as_array() {
            let diffs: Vec<ChatDiff> = content
                .iter()
                .filter(|item| item["type"] == "diff")
                .filter_map(|item| {
                    let side = |key: &str| {
                        item[key]
                            .as_str()
                            .filter(|text| text.len() <= MAX_CHAT_DIFF_BYTES)
                            .map(str::to_owned)
                    };
                    Some(ChatDiff {
                        path: item["path"].as_str()?.to_owned(),
                        old_text: side("oldText"),
                        new_text: side("newText"),
                    })
                })
                .collect();
            if !diffs.is_empty() {
                tool.diffs = diffs;
            }
            if raw_output.is_none() {
                let texts: Vec<&str> = content
                    .iter()
                    .filter(|item| item["type"] == "content")
                    .filter_map(|item| item["content"]["text"].as_str())
                    .collect();
                if !texts.is_empty() {
                    tool.output = Some(tail(texts.join("\n"), MAX_CHAT_TOOL_OUTPUT_BYTES));
                }
            }
        }
        if let Some(output) = raw_output {
            tool.output = Some(tail(output, MAX_CHAT_TOOL_OUTPUT_BYTES));
        }
        let entry = ChatEntry::Tool(Box::new(tool));
        match known {
            Some(index) => {
                if self.state.entries.get(index) != Some(&entry) {
                    self.emit(ChatEvent::Replace { index, entry });
                }
            }
            None => {
                self.tools.insert(id.to_owned(), self.state.entries.len());
                self.emit(ChatEvent::Entry { entry });
            }
        }
    }
}

/// The text blocks of an ACP content list, joined.
fn text_of(content: &Value) -> Option<String> {
    let texts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
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

/// One ACP config option, when it is a select.
fn option(value: &Value) -> Option<ChatOption> {
    if value["type"].as_str() != Some("select") {
        return None;
    }
    let mut choices = Vec::new();
    for item in value["options"].as_array()? {
        match item["options"].as_array() {
            Some(group) => choices.extend(group.iter().filter_map(|choice| {
                self::choice(choice, item["name"].as_str().map(str::to_owned))
            })),
            None => choices.extend(self::choice(item, None)),
        }
    }
    Some(ChatOption {
        id: value["id"].as_str()?.to_owned(),
        name: value["name"].as_str().unwrap_or_default().to_owned(),
        category: value["category"].as_str().map(str::to_owned),
        current: value["currentValue"].as_str()?.to_owned(),
        choices,
    })
}

fn choice(value: &Value, group: Option<String>) -> Option<ChatChoice> {
    Some(ChatChoice {
        value: value["value"].as_str()?.to_owned(),
        name: value["name"].as_str()?.to_owned(),
        description: value["description"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        group,
    })
}

/// The subscription plans in OMP's usage reports: each provider, and per window the
/// share left.
fn plans(result: &Value) -> Vec<ChatPlan> {
    result["reports"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|report| {
            let provider = report["provider"].as_str()?.to_owned();
            let limits = report["limits"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|limit| {
                    let amount = &limit["amount"];
                    let left = amount["remainingFraction"].as_f64().or_else(|| {
                        Some(amount["remaining"].as_f64()? / amount["limit"].as_f64()?)
                    })?;
                    Some(ChatPlanLimit {
                        label: limit["label"].as_str()?.to_owned(),
                        left_percent: (left.clamp(0.0, 1.0) * 100.0).round() as u8,
                    })
                })
                .collect();
            Some(ChatPlan { provider, limits })
        })
        .collect()
}

/// A listing page's sessions that hold a message.
fn sessions(result: &Value) -> Vec<ChatSessionInfo> {
    result["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|session| session["_meta"]["messageCount"].as_u64() != Some(0))
        .filter_map(|session| {
            Some(ChatSessionInfo {
                id: session["sessionId"].as_str()?.to_owned(),
                title: session["title"]
                    .as_str()
                    .filter(|title| !title.is_empty())
                    .map(str::to_owned),
                updated: session["updatedAt"].as_str().map(str::to_owned),
                folder: session["cwd"].as_str().map(str::to_owned),
                size: session["_meta"]["size"].as_u64(),
                first_prompt: None,
                last_reply: None,
            })
        })
        .collect()
}

/// The cursor of a listing's next page, unless `pages` were read already.
fn next_cursor(result: &Value, pages: usize) -> Option<String> {
    result["nextCursor"]
        .as_str()
        .filter(|cursor| !cursor.is_empty() && pages < MAX_LIST_PAGES)
        .map(str::to_owned)
}

/// A JSON-RPC error as people read it: its message, then the harness's details (OMP
/// says only "Internal error" in the message).
fn error_text(error: &Value) -> String {
    let message = error["message"].as_str().unwrap_or("unknown error");
    match error["data"]["details"]
        .as_str()
        .filter(|details| !details.is_empty())
    {
        Some(details) => format!("{message}: {details}"),
        None => message.to_owned(),
    }
}

/// Suffix `__other` names a choice field's free-text companion (OMP's `ask`).
const OTHER_SUFFIX: &str = "__other";

/// A form's fields, in the order its keys sort naturally (`q2` before `q10`), and how to
/// send each answer back.
fn form_fields(schema: &Value) -> (Vec<ChatField>, Vec<FieldShape>) {
    let empty = Map::new();
    let properties = schema["properties"].as_object().unwrap_or(&empty);
    let required: HashSet<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut keys: Vec<&String> = properties
        .keys()
        .filter(|key| {
            // A companion stands alone only when its choice field is missing.
            key.strip_suffix(OTHER_SUFFIX)
                .is_none_or(|base| !properties.contains_key(base))
        })
        .collect();
    keys.sort_by_key(|key| natural(key));
    let mut fields = Vec::new();
    let mut shapes = Vec::new();
    for key in keys {
        let property = &properties[key.as_str()];
        let kind = property["type"].as_str().unwrap_or("string").to_owned();
        let listed = |value: &Value| -> Vec<ChatChoice> {
            let from_one_of = value["oneOf"]
                .as_array()
                .or_else(|| value["anyOf"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    let value = item["const"].as_str()?.to_owned();
                    Some(ChatChoice {
                        name: item["title"].as_str().unwrap_or(&value).to_owned(),
                        value,
                        description: None,
                        group: None,
                    })
                });
            let from_enum = value["enum"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    let value = item.as_str()?.to_owned();
                    Some(ChatChoice {
                        name: value.clone(),
                        value,
                        description: None,
                        group: None,
                    })
                });
            from_one_of.chain(from_enum).collect()
        };
        let (choices, multi) = match kind.as_str() {
            "array" => (listed(&property["items"]), true),
            "boolean" => (
                vec![
                    ChatChoice {
                        value: "true".into(),
                        name: "Yes".into(),
                        description: None,
                        group: None,
                    },
                    ChatChoice {
                        value: "false".into(),
                        name: "No".into(),
                        description: None,
                        group: None,
                    },
                ],
                false,
            ),
            _ => (listed(property), false),
        };
        let other_key = format!("{key}{OTHER_SUFFIX}");
        let other = properties.contains_key(&other_key);
        fields.push(ChatField {
            key: key.clone(),
            title: property["title"]
                .as_str()
                .or_else(|| property["description"].as_str())
                .unwrap_or(key)
                .to_owned(),
            choices,
            multi,
            other,
            required: required.contains(key.as_str()),
        });
        shapes.push(FieldShape {
            key: key.clone(),
            kind,
            other: other.then_some(other_key),
        });
    }
    (fields, shapes)
}

/// Sorts `q2` before `q10`: runs of digits compare as numbers.
fn natural(key: &str) -> Vec<(String, u64)> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut digits = String::new();
    for c in key.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if !digits.is_empty() {
                parts.push((
                    std::mem::take(&mut text),
                    digits.parse().unwrap_or(u64::MAX),
                ));
                digits.clear();
            }
            text.push(c);
        }
    }
    parts.push((text, digits.parse().unwrap_or(0)));
    parts
}

/// The form's `content`: each answer in its field's type; typed text for a choice field
/// goes to its free-text companion, which the harness takes over the choice.
fn form_content(shapes: &[FieldShape], mut answers: ChatAnswers) -> Value {
    let mut content = Map::new();
    for shape in shapes {
        let Some(answer) = answers.remove(&shape.key) else {
            continue;
        };
        let (key, value) = match (answer, &shape.other) {
            (ChatAnswer::Text { text }, Some(other)) => (other.clone(), Value::String(text)),
            (ChatAnswer::Text { text }, None) => (
                shape.key.clone(),
                match shape.kind.as_str() {
                    "number" | "integer" => text
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map_or(Value::String(text), Value::Number),
                    _ => Value::String(text),
                },
            ),
            (ChatAnswer::Choices { values }, _) => (
                shape.key.clone(),
                match shape.kind.as_str() {
                    "array" => Value::from(values),
                    "boolean" => Value::Bool(values.first().is_some_and(|value| value == "true")),
                    _ => values.into_iter().next().map_or(Value::Null, Value::String),
                },
            ),
        };
        if !value.is_null() {
            content.insert(key, value);
        }
    }
    Value::Object(content)
}

/// A `file://` URI for an absolute path: `/a b` → `file:///a%20b`, `C:\x` →
/// `file:///C:/x`.
fn file_uri(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// The path a `file://` URI names, as `file_uri` wrote it; other URIs stay as they are.
fn path_of_uri(uri: &str) -> String {
    let Some(path) = uri.strip_prefix("file://") else {
        return uri.to_owned();
    };
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(byte) = path
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let path = String::from_utf8_lossy(&decoded).into_owned();
    // `/C:/x` is a Windows path.
    match path.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => path[1..].replace('/', "\\"),
        _ => path,
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_owned()
}

fn not_installed(content: &ChatContent) -> String {
    let place = match &content.environment {
        ContentEnvironment::Host if cfg!(windows) => "on Windows".to_owned(),
        ContentEnvironment::Host => "here".to_owned(),
        ContentEnvironment::Wsl { distribution } => format!("in WSL ({distribution})"),
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

/// OMP's own `tools.approvalMode` in the chat's environment and folder, when it is a mode
/// `omp acp` takes; `None` when the lookup fails or takes longer than
/// `APPROVAL_LOOKUP_TIMEOUT`. The value is the last line: a login shell's profile may
/// print before it.
fn configured_approval(content: &ChatContent) -> Option<&'static str> {
    let mut child = command(content, &["config", "get", "tools.approvalMode"])
        .ok()?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = child.stdout.take().expect("stdout is piped");
    let (sender, receiver) = mpsc::channel();
    // Read on a thread of its own, so a lookup that hangs is given up on time.
    let _ = thread::Builder::new()
        .name("compi-chat-config".into())
        .spawn(move || {
            let mut text = String::new();
            let _ = output.take(16 * 1024).read_to_string(&mut text);
            let _ = sender.send(text);
        });
    let text = receiver.recv_timeout(APPROVAL_LOOKUP_TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
    let text = text.ok()?;
    let value = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    OMP_APPROVAL_MODES.into_iter().find(|mode| *mode == value)
}

/// The harness's command with `args` in its environment and folder. The arguments go
/// into a shell script unquoted, so they are fixed words.
fn command(content: &ChatContent, args: &[&'static str]) -> Result<Command, String> {
    if matches!(content.environment, ContentEnvironment::Host)
        && !std::path::Path::new(&content.folder).is_dir()
    {
        return Err(format!("The folder {} doesn't exist.", content.folder));
    }
    match &content.environment {
        #[cfg(windows)]
        ContentEnvironment::Host => {
            use std::os::windows::process::CommandExt as _;
            let program =
                windows_program(content.harness.program()).ok_or_else(|| not_installed(content))?;
            let mut command = Command::new(program);
            command
                .args(args)
                .current_dir(&content.folder)
                .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
            Ok(command)
        }
        #[cfg(windows)]
        ContentEnvironment::Wsl { distribution } => {
            use std::os::windows::process::CommandExt as _;
            let mut command = Command::new(WSL_EXE);
            command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
            if let Some(path) = compi_protocol::wsl::launch_path() {
                command.env("PATH", path);
            }
            // The user's login shell, for the search path their profile sets.
            let script = format!(
                r#"shell=$(getent passwd "$(id -u)" | cut -d: -f7); exec "${{shell:-/bin/sh}}" -lc '{UNIX_COMMAND} {}'"#,
                args.join(" ")
            );
            command
                .args([
                    "--distribution",
                    distribution,
                    "--cd",
                    &content.folder,
                    "--exec",
                ])
                .args(["/bin/sh", "-c", &script]);
            Ok(command)
        }
        #[cfg(unix)]
        ContentEnvironment::Host => {
            let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
            let mut command = Command::new(shell);
            command
                .args(["-lc", &format!("{UNIX_COMMAND} {}", args.join(" "))])
                .current_dir(&content.folder);
            Ok(command)
        }
        #[cfg(unix)]
        ContentEnvironment::Wsl { .. } => Err("WSL chats need a Windows host".into()),
    }
}

/// `<program>.exe` on the search path, else where Bun installs it.
#[cfg(windows)]
fn windows_program(program: &str) -> Option<std::path::PathBuf> {
    let file = format!("{program}.exe");
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .chain(
            std::env::var_os("USERPROFILE")
                .map(|home| std::path::PathBuf::from(home).join(".bun").join("bin")),
        )
        .map(|folder| folder.join(&file))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_entry(inner: &Inner, index: usize) -> &ChatTool {
        match &inner.state.entries[index] {
            ChatEntry::Tool(tool) => tool,
            other => panic!("entry {index} is {other:?}"),
        }
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
        assert_eq!(keys, ["ok", "q0", "q2", "q10"]);
        let colour = &fields[1];
        assert!(colour.other && colour.required && !colour.multi);
        assert_eq!(colour.choices.len(), 2);
        assert!(fields[2].multi);

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
            })));
        }
        let fitted = fit(state, 2500);
        assert!(serde_json::to_vec(&fitted).unwrap().len() <= 2500);
        // The newest call keeps its output.
        assert!(matches!(&fitted.entries[3], ChatEntry::Tool(tool) if tool.output.is_some()));
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
        inner.start_prompt("s".into(), prompt("next"));
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

        inner.start_prompt("s".into(), prompt());
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
        inner.start_prompt("s".into(), prompt());
        inner.prompt_ended(Ok(json!({ "stopReason": "cancelled" })));
        assert_eq!(shown(&inner), None, "a cancelled turn leaves no result");

        inner.start_prompt("s".into(), prompt());
        inner.prompt_ended(Ok(json!({ "stopReason": "end_turn" })));
        inner.unseen = None;
        assert_eq!(shown(&inner), None, "acknowledged");

        inner.start_prompt("s".into(), prompt());
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
}
