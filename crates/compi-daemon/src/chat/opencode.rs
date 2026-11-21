//! OpenCode's driver: `opencode serve`, its local HTTP server. The server says where it
//! listens on its output (`Wire::line`), behind the password Compi starts it with; then
//! one thread sends Compi's requests in order (`Job`) and another reads the server's
//! event stream, each handing what it got to the chat under its lock, as long as the
//! program they were started for runs (its generation).
//!
//! OpenCode comes in two lines with different servers, found by asking (`Dialect`):
//! 1.x (`opencode-ai`) has its classic API (`v1`: `/session`, `/event`, messages made
//! of parts); 2.x (`@opencode/cli`) has `/api` (`v2`: sessions streaming typed
//! events). Both run a session as turns, stream replies and reasoning, report tool
//! calls, ask permissions and questions through the event stream and take the
//! answers as requests, and run sub-agents as sessions of their own.
//!
//! A session opens with the first prompt, so Compi leaves no empty ones; a chat with
//! a session resumes it (its messages replayed unless Compi kept the transcript).
//! Compi's permission levels go with the session as its permission rules
//! (`rules`). Models are every provider's OpenCode is set up with, as
//! `provider/model`; thinking levels are the model's variants and modes its primary
//! agents. 2.x takes a prompt into the running turn (steering); 1.x queues it.
mod http;
#[cfg(test)]
mod tests;
mod tools;
mod v1;
mod v2;

use super::launch::Arg;
use super::{Chat, Driver, Inner, Prompt, SessionsReply, Wire, rfc3339};
use compi_protocol::chat::{
    ChatAnswer, ChatAnswers, ChatChoice, ChatEntry, ChatEvent, ChatField, ChatHarness, ChatOption,
    ChatPermission, ChatPermissionOption, ChatQuestion, ChatSessionInfo, ChatStatus,
    ChatToolStatus, ChatUsage,
};
use serde_json::{Value, json};
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::BuildHasher as _;
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

/// The permission answers Compi offers, as OpenCode names them.
const ONCE: &str = "once";
const ALWAYS: &str = "always";
const REJECT: &str = "reject";
/// How often the event stream is opened again after it broke off while the server
/// runs, a second apart.
const EVENT_RETRIES: usize = 5;
/// The most pages of a session's messages read.
const MAX_MESSAGE_PAGES: usize = 40;
/// The most sessions a listing shows.
const LISTED_SESSIONS: &str = "100";
/// The thinking level that is the model's own.
const DEFAULT_VARIANT: &str = "default";
/// Actions that only read, allowed when Compi asks before everything else.
const READS: [&str; 10] = [
    "read",
    "glob",
    "grep",
    "list",
    "codesearch",
    "lsp",
    "todoread",
    "todowrite",
    "question",
    "skill",
];

/// OpenCode's driver, as the chat core calls it.
pub(super) const WIRE: Wire = Wire {
    // OpenCode reads its own `permission` rules.
    configured_approval: |_| None,
    launch_args,
    environment: |inner| {
        vec![(
            "OPENCODE_SERVER_PASSWORD",
            inner.opencode_state().password.clone(),
        )]
    },
    // Nothing is said until the server says where it listens.
    start: |_| {},
    // The server writes no messages on its output.
    handle: |_, _, _| {},
    line,
    session_with_first_prompt: true,
    prompt,
    steer: Some((can_steer, steer)),
    cancel,
    interrupt,
    answer,
    answer_question,
    set_option: |_, inner, option, value| set_option(inner, option, value),
    list_sessions: |_, inner| list_sessions(inner),
    fill_sessions: |_, _| {},
    switch_session: Some(|_, inner, session| switch_session(inner, session)),
    cancel_asks,
    forget_asks,
    fail_pending,
    forget_transcript: |inner| inner.opencode().forget_transcript(),
};

/// Which of OpenCode's servers runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// 1.x: the classic API.
    V1,
    /// 2.x: `/api`.
    V2,
}

/// What the OpenCode driver keeps of the running server and its session.
#[derive(Default)]
pub(super) struct OpenCode {
    /// The password the server starts with, new each start.
    password: String,
    /// The approval mode Compi chose; `None` keeps OpenCode's own rules.
    chosen: Option<&'static str>,
    /// Where requests go to be sent, once the server listens.
    jobs: Option<mpsc::Sender<Job>>,
    dialect: Option<Dialect>,
    /// The models, agents and defaults are loaded: the event stream connected once.
    loading: bool,
    next_request: u64,
    /// The requests sent and not answered, by ID.
    pending: HashMap<u64, Pending>,
    /// Every request made, for tests to read.
    #[cfg(test)]
    sent: Vec<Job>,
    models: Vec<Model>,
    /// The default model of each provider, in listing order (1.x).
    provider_defaults: Vec<String>,
    /// OpenCode's primary agents (modes): ID and name.
    agents: Vec<(String, String)>,
    /// The model (`provider/model`), thinking level and agent the settings show.
    model: Option<String>,
    variant: Option<String>,
    agent: Option<String>,
    /// The same as the session has them (2.x switches them before a prompt).
    session_model: Option<(String, String)>,
    session_agent: Option<String>,
    /// A prompt waiting for its session to open.
    waiting: Option<Value>,
    /// The running turn began on OpenCode's side (busy, or its execution started):
    /// only then does its end count.
    turn_started: bool,
    /// Compi asked the running turn to stop.
    interrupting: bool,
    /// The stop came before the session opened.
    interrupt_due: bool,
    /// What failed the running turn.
    failure: Option<String>,
    /// Permissions and questions OpenCode waits on, oldest first: the first shows.
    asks: VecDeque<Ask>,
    /// Replies and reasoning being streamed, by part.
    streams: HashMap<String, tools::Stream>,
    /// Message roles by ID (1.x tells them apart only so).
    roles: HashMap<String, Role>,
    /// Whether each text part is a reply or a thought, by part ID (1.x streams its
    /// text by part alone).
    parts: HashMap<String, tools::Text>,
    /// The name and input of each call, by call ID (2.x names a call once).
    calls: HashMap<String, (String, Value)>,
    /// Sub-agents' sessions, by ID.
    children: HashMap<String, tools::Child>,
    /// The turn's todo card, once it has one.
    todo_card: Option<String>,
    todo_cards: u64,
    /// Tokens in the context, the context window and the session's cost.
    used: u64,
    cost: f64,
}

impl OpenCode {
    /// The transcript was replaced: its entries are no longer where the driver saw them.
    fn forget_transcript(&mut self) {
        self.streams.clear();
        self.children.clear();
        self.parts.clear();
        self.todo_card = None;
        self.used = 0;
        self.cost = 0.0;
    }
}

impl Inner {
    /// The OpenCode driver's state; only an OpenCode chat has one.
    fn opencode(&mut self) -> &mut OpenCode {
        match &mut self.driver {
            Driver::OpenCode(opencode) => opencode,
            _ => unreachable!("an OpenCode chat speaks OpenCode's server"),
        }
    }

    fn opencode_state(&self) -> &OpenCode {
        match &self.driver {
            Driver::OpenCode(opencode) => opencode,
            _ => unreachable!("an OpenCode chat speaks OpenCode's server"),
        }
    }
}

/// A message's author.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

/// A request for the sending thread.
#[derive(Debug, Clone, PartialEq)]
struct Job {
    id: u64,
    method: &'static str,
    path: String,
    body: Option<Value>,
}

/// What a request was for.
enum Pending {
    /// The models (1.x: providers; 2.x: models), then the agents, then the defaults.
    Models,
    Agents,
    Defaults,
    /// 1.x without a configured model: its state folder, which keeps the models last
    /// picked in it (`model.json`).
    StateFolder {
        agent: Option<String>,
    },
    /// The session to resume, then its rules set, then its messages page by page.
    Session,
    Rules,
    Messages {
        pages: usize,
    },
    /// A new session, for the waiting prompt.
    Create,
    Prompt,
    /// A prompt into the running turn: shown once taken, else queued first.
    Steer(Prompt),
    /// Requests whose answer changes nothing.
    Quiet,
    List(mpsc::Sender<Result<Vec<ChatSessionInfo>, String>>),
}

/// A model OpenCode offers.
#[derive(Debug, Clone, PartialEq)]
struct Model {
    provider: String,
    id: String,
    name: String,
    /// Its thinking levels, without the model's own (`DEFAULT_VARIANT`).
    variants: Vec<String>,
    /// Its context window, when known.
    context: u64,
}

impl Model {
    fn value(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// A permission or question OpenCode waits on.
struct Ask {
    /// Its ID, and its session's.
    id: String,
    session: String,
    kind: AskKind,
}

enum AskKind {
    Permission(ChatPermission),
    /// A question, with how its answers go back: by its fields' keys (2.x forms,
    /// their value types) or in order (1.x).
    Question(ChatQuestion, Vec<(String, String)>),
}

impl AskKind {
    fn event(&self) -> ChatEvent {
        match self {
            Self::Permission(shown) => ChatEvent::Permission {
                permission: Some(shown.clone()),
            },
            Self::Question(shown, _) => ChatEvent::Question {
                question: Some(shown.clone()),
            },
        }
    }
}

/// OpenCode's arguments: its server on the loopback interface, on a port it picks.
fn launch_args(inner: &mut Inner, approval_mode: Option<&'static str>) -> Vec<Arg> {
    let opencode = inner.opencode();
    *opencode = OpenCode {
        password: password(),
        chosen: approval_mode,
        todo_cards: opencode.todo_cards,
        ..OpenCode::default()
    };
    ["serve", "--hostname", "127.0.0.1", "--port", "0"]
        .into_iter()
        .map(Arg::word)
        .collect()
}

/// A new random password: 128 bits from the process's random hash keys.
fn password() -> String {
    let state = RandomState::new();
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{:016x}{:016x}",
        state.hash_one((now, std::process::id())),
        RandomState::new().hash_one(now)
    )
}

/// A line of the server's output: where it listens, once. Requests and events go
/// through threads of their own from then on.
fn line(chat: &Arc<Chat>, inner: &mut Inner, line: &str) {
    let opencode = inner.opencode();
    if opencode.jobs.is_some() {
        return;
    }
    let Some(server) = http::Server::listening(line, &opencode.password) else {
        return;
    };
    let (jobs, queue) = mpsc::channel();
    opencode.jobs = Some(jobs);
    let generation = inner.generation;
    let folder = inner.agent_folder().to_owned();
    let sender = (Arc::downgrade(chat), server.clone());
    let spawned = thread::Builder::new()
        .name("compi-opencode-requests".into())
        .spawn(move || work(&sender.0, generation, &sender.1, &queue))
        .and_then(|_| {
            let chat = Arc::downgrade(chat);
            thread::Builder::new()
                .name("compi-opencode-events".into())
                .spawn(move || listen(&chat, generation, &server, &folder))
        });
    if let Err(error) = spawned {
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Failed {
                message: format!("Couldn't talk to {}: {error}", ChatHarness::OpenCode.name()),
            },
        });
    }
}

/// Runs `chat`'s requests in order while its program of `generation` runs, each answer
/// handed to the driver.
fn work(chat: &Weak<Chat>, generation: u64, server: &http::Server, queue: &mpsc::Receiver<Job>) {
    for job in queue {
        let result = http::call(server, job.method, &job.path, job.body.as_ref());
        let Some(chat) = chat.upgrade() else {
            return;
        };
        let mut inner = chat.lock();
        if inner.closed || inner.generation != generation {
            return;
        }
        answered(&mut inner, job.id, result);
    }
}

/// Finds which server runs, then reads its events into `chat` while its program of
/// `generation` runs; a stream that breaks off while it does opens again.
fn listen(chat: &Weak<Chat>, generation: u64, server: &http::Server, folder: &str) {
    // 2.x tells its version at `/api/info`; 1.x has no such route.
    let dialect = match http::call(server, "GET", "/api/info", None) {
        Ok(info) if info["version"].is_string() => Dialect::V2,
        _ => Dialect::V1,
    };
    let current = |chat: &Arc<Chat>| {
        let inner = chat.lock();
        !inner.closed && inner.generation == generation && inner.agent.is_some()
    };
    {
        let Some(chat) = chat.upgrade() else {
            return;
        };
        let mut inner = chat.lock();
        if inner.closed || inner.generation != generation {
            return;
        }
        inner.opencode().dialect = Some(dialect);
    }
    let path = match dialect {
        Dialect::V1 => format!("/event{}", http::query(&[("directory", folder)])),
        Dialect::V2 => "/api/event".to_owned(),
    };
    let mut failure = String::new();
    for _ in 0..=EVENT_RETRIES {
        let result = http::subscribe(server, &path, |event| {
            let Some(chat) = chat.upgrade() else {
                return false;
            };
            let mut inner = chat.lock();
            if inner.closed || inner.generation != generation {
                return false;
            }
            self::event(&mut inner, &event);
            true
        });
        let Some(chat) = chat.upgrade() else {
            return;
        };
        if !current(&chat) {
            return;
        }
        if let Err(error) = result {
            failure = error;
        }
        thread::sleep(Duration::from_secs(1));
    }
    let Some(chat) = chat.upgrade() else {
        return;
    };
    let mut inner = chat.lock();
    if inner.closed || inner.generation != generation {
        return;
    }
    inner.emit(ChatEvent::Status {
        status: ChatStatus::Failed {
            message: format!("Lost {}'s events: {failure}", ChatHarness::OpenCode.name()),
        },
    });
}

/// Sends a request; `pending` says what its answer is for.
fn request(
    inner: &mut Inner,
    method: &'static str,
    path: String,
    body: Option<Value>,
    pending: Pending,
) {
    let opencode = inner.opencode();
    opencode.next_request += 1;
    let job = Job {
        id: opencode.next_request,
        method,
        path,
        body,
    };
    opencode.pending.insert(job.id, pending);
    #[cfg(test)]
    opencode.sent.push(job.clone());
    if let Some(jobs) = &opencode.jobs {
        // A closed queue means the server is gone; its exit is reported.
        let _ = jobs.send(job);
    }
}

fn dialect(inner: &Inner) -> Dialect {
    inner.opencode_state().dialect.unwrap_or(Dialect::V1)
}

/// The query naming the chat's folder (1.x).
fn directory(inner: &Inner) -> String {
    http::query(&[("directory", inner.agent_folder())])
}

/// The query naming the chat's folder (2.x).
fn location(inner: &Inner) -> String {
    http::query(&[("location[directory]", inner.agent_folder())])
}

/// One event of the server's stream.
fn event(inner: &mut Inner, event: &Value) {
    if event["type"] == "server.connected" {
        connected(inner);
        return;
    }
    match dialect(inner) {
        Dialect::V1 => v1::event(inner, event),
        Dialect::V2 => v2::event(inner, event),
    }
}

/// The event stream is open: the models, agents and defaults load, then the session
/// opens. A stream opened again changes nothing.
fn connected(inner: &mut Inner) {
    let opencode = inner.opencode();
    if std::mem::replace(&mut opencode.loading, true) {
        return;
    }
    let path = match dialect(inner) {
        Dialect::V1 => format!("/config/providers{}", directory(inner)),
        Dialect::V2 => format!("/api/model{}", location(inner)),
    };
    request(inner, "GET", path, None, Pending::Models);
}

/// An answer from the server.
fn answered(inner: &mut Inner, id: u64, result: Result<Value, String>) {
    let Some(pending) = inner.opencode().pending.remove(&id) else {
        return;
    };
    let harness = ChatHarness::OpenCode.name();
    let dialect = dialect(inner);
    match pending {
        Pending::Models => {
            match result {
                Ok(answer) => {
                    let (models, defaults) = match dialect {
                        Dialect::V1 => v1::models(&answer),
                        Dialect::V2 => (v2::models(&answer), Vec::new()),
                    };
                    let opencode = inner.opencode();
                    opencode.models = models;
                    opencode.provider_defaults = defaults;
                }
                // Without its models the chat still works, on OpenCode's own.
                Err(error) => eprintln!(
                    "compi-daemon: chat {} has no model list from {harness}: {error}",
                    inner.chat_id
                ),
            }
            let path = match dialect {
                Dialect::V1 => format!("/agent{}", directory(inner)),
                Dialect::V2 => format!("/api/agent{}", location(inner)),
            };
            request(inner, "GET", path, None, Pending::Agents);
        }
        Pending::Agents => {
            if let Ok(answer) = result {
                inner.opencode().agents = match dialect {
                    Dialect::V1 => v1::agents(&answer),
                    Dialect::V2 => v2::agents(&answer),
                };
            }
            let path = match dialect {
                Dialect::V1 => format!("/config{}", directory(inner)),
                Dialect::V2 => format!("/api/model/default{}", location(inner)),
            };
            request(inner, "GET", path, None, Pending::Defaults);
        }
        Pending::Defaults => {
            let (model, agent) = match (&result, dialect) {
                (Ok(answer), Dialect::V1) => v1::defaults(answer),
                (Ok(answer), Dialect::V2) => (v2::default_model(answer), None),
                (Err(_), _) => (None, None),
            };
            // 1.x's own default without one configured is the model last picked in it,
            // which its state folder keeps.
            if model.is_none() && dialect == Dialect::V1 {
                let path = format!("/path{}", directory(inner));
                request(inner, "GET", path, None, Pending::StateFolder { agent });
                return;
            }
            defaults_known(inner, model, agent);
        }
        // The folder is OpenCode's own, outside the chat's, which its server won't read.
        Pending::StateFolder { agent } => {
            let recent = result
                .ok()
                .and_then(|paths| {
                    let state = paths["state"].as_str()?;
                    std::fs::read_to_string(state_file(inner, state, "model.json")).ok()
                })
                .map(|text| v1::recent_models(&text))
                .unwrap_or_default();
            let opencode = inner.opencode_state();
            let model = recent.into_iter().find(|model| {
                opencode
                    .models
                    .iter()
                    .any(|offered| offered.value() == *model)
            });
            defaults_known(inner, model, agent);
        }
        Pending::Session => match result {
            Ok(answer) => {
                let session = match dialect {
                    Dialect::V1 => answer,
                    Dialect::V2 => answer["data"].clone(),
                };
                resumed(inner, &session);
            }
            Err(error) => resume_failed(inner, &error),
        },
        Pending::Rules => {
            if let Err(error) = result {
                eprintln!(
                    "compi-daemon: chat {} kept its session's permission rules: {error}",
                    inner.chat_id
                );
            }
            if inner.skip_replay {
                ready(inner);
            } else {
                messages(inner, None, 1);
            }
        }
        Pending::Messages { pages } => {
            let Ok(answer) = result else {
                ready(inner);
                return;
            };
            let next = match dialect {
                Dialect::V1 => {
                    v1::replay(inner, &answer);
                    None
                }
                Dialect::V2 => {
                    v2::replay(inner, &answer["data"]);
                    answer["cursor"]["next"]
                        .as_str()
                        .filter(|cursor| !cursor.is_empty() && pages < MAX_MESSAGE_PAGES)
                        .map(str::to_owned)
                }
            };
            match next {
                Some(cursor) => messages(inner, Some(cursor), pages + 1),
                None => {
                    inner.end_thought();
                    inner.opencode().streams.clear();
                    if inner.thread.is_none() && !inner.state.entries.is_empty() {
                        inner.start_thread();
                    }
                    ready(inner);
                }
            }
        }
        Pending::Create => match result {
            Ok(answer) => {
                let session = match dialect {
                    Dialect::V1 => &answer["id"],
                    Dialect::V2 => &answer["data"]["id"],
                };
                match session.as_str() {
                    Some(session) => created(inner, session.to_owned()),
                    None => fail_turn(inner, format!("{harness} opened no session")),
                }
            }
            Err(error) => fail_turn(inner, format!("{harness} didn't open a session: {error}")),
        },
        Pending::Prompt => {
            if let Err(error) = result
                && inner.state.working()
            {
                inner.opencode().turn_started = false;
                inner.prompt_ended(Err(error));
            }
        }
        Pending::Steer(prompt) => match result {
            Ok(_) => {
                inner.end_thought();
                inner.activity_ms = super::now_ms();
                inner.push_prompt(&prompt);
            }
            // The turn ended first, or the server declined: the prompt waits its turn,
            // first in line.
            Err(_) => {
                inner.enqueue(prompt, true);
                inner.send_queued();
            }
        },
        Pending::Quiet => {}
        Pending::List(reply) => {
            let _ = reply.send(result.map(|answer| match dialect {
                Dialect::V1 => sessions(&answer),
                Dialect::V2 => sessions(&answer["data"]),
            }));
        }
    }
}

/// `name` in OpenCode's state folder `state` as the daemon reads it: a Linux program's
/// in WSL through `\\wsl.localhost`.
fn state_file(inner: &Inner, state: &str, name: &str) -> std::path::PathBuf {
    let windows = inner
        .started
        .as_ref()
        .is_some_and(|started| started.windows);
    match &inner.folder {
        Some((compi_protocol::ContentEnvironment::Wsl { distribution }, _)) if !windows => format!(
            r"\\wsl.localhost\{distribution}{}\{name}",
            state.replace('/', "\\")
        )
        .into(),
        _ => std::path::Path::new(state).join(name),
    }
}

/// OpenCode's default model and agent are known: the session opens.
fn defaults_known(inner: &mut Inner, model: Option<String>, agent: Option<String>) {
    pick_defaults(inner, model, agent);
    let session = inner.state.session.clone();
    open_session(inner, session);
}

/// The settings a new session starts on: the user's picks when OpenCode offers them,
/// else its own defaults.
fn pick_defaults(inner: &mut Inner, model: Option<String>, agent: Option<String>) {
    let picks = inner.picks.clone();
    let opencode = inner.opencode();
    let offered = |value: &String| opencode.models.iter().any(|model| model.value() == *value);
    let model = picks
        .model
        .filter(offered)
        .or(model)
        .or_else(|| {
            opencode
                .provider_defaults
                .iter()
                .find(|value| offered(value))
                .cloned()
        })
        .or_else(|| opencode.models.first().map(Model::value));
    let variants = model
        .as_ref()
        .and_then(|model| {
            opencode
                .models
                .iter()
                .find(|offered| offered.value() == *model)
        })
        .map(|model| model.variants.clone())
        .unwrap_or_default();
    opencode.variant = picks
        .thought_level
        .filter(|level| variants.contains(level))
        .or_else(|| Some(DEFAULT_VARIANT.to_owned()));
    opencode.model = model;
    opencode.agent = agent
        .filter(|agent| opencode.agents.iter().any(|(id, _)| id == agent))
        .or_else(|| {
            let build = opencode.agents.iter().find(|(id, _)| id == "build");
            build.or(opencode.agents.first()).map(|(id, _)| id.clone())
        });
}

/// Opens `session`, or none: a new one opens with the first prompt.
fn open_session(inner: &mut Inner, session: Option<String>) {
    let opencode = inner.opencode();
    opencode.session_model = None;
    opencode.session_agent = None;
    let Some(session) = session else {
        inner.skip_replay = false;
        ready(inner);
        return;
    };
    let path = match dialect(inner) {
        Dialect::V1 => format!("/session/{session}{}", directory(inner)),
        Dialect::V2 => format!("/api/session/{session}"),
    };
    request(inner, "GET", path, None, Pending::Session);
}

/// The session to resume: its title and settings, then Compi's rules for it.
fn resumed(inner: &mut Inner, session: &Value) {
    let Some(id) = session["id"].as_str().map(str::to_owned) else {
        resume_failed(inner, "it has no ID");
        return;
    };
    if let Some(title) = session_title(session)
        && inner.state.title.as_deref() != Some(&title)
    {
        inner.emit(ChatEvent::Title { title: Some(title) });
    }
    let model = &session["model"];
    let opencode = inner.opencode();
    if let (Some(provider), Some(model_id)) = (model["providerID"].as_str(), model["id"].as_str()) {
        let value = format!("{provider}/{model_id}");
        opencode.model = Some(value.clone());
        opencode.variant = Some(
            model["variant"]
                .as_str()
                .unwrap_or(DEFAULT_VARIANT)
                .to_owned(),
        );
        opencode.session_model = Some((value, opencode.variant.clone().unwrap_or_default()));
    }
    if let Some(agent) = session["agent"].as_str() {
        opencode.agent = Some(agent.to_owned());
        opencode.session_agent = Some(agent.to_owned());
    }
    if inner.state.session.as_deref() != Some(&id) {
        inner.emit(ChatEvent::Session {
            session: Some(id.clone()),
        });
    }
    inner.save_thread(false);
    let rules = rules(inner.opencode_state().chosen);
    let (path, body) = match dialect(inner) {
        Dialect::V1 => (
            format!("/session/{id}{}", directory(inner)),
            json!({ "permission": v1::rules(&rules) }),
        ),
        Dialect::V2 => (
            format!("/api/session/{id}"),
            json!({ "permissions": v2::rules(&rules) }),
        ),
    };
    request(inner, "PATCH", path, Some(body), Pending::Rules);
}

/// The session could not be resumed: the transcript so far stays, above the error, and
/// the next prompt opens a new one.
fn resume_failed(inner: &mut Inner, error: &str) {
    inner.skip_replay = false;
    inner.emit(ChatEvent::Session { session: None });
    inner.emit(ChatEvent::Entry {
        entry: ChatEntry::Error {
            message: format!("Couldn't resume the earlier session: {error}"),
        },
    });
    inner.save_thread(true);
    ready(inner);
}

/// Reads the session's messages, oldest first, from `cursor` (2.x pages).
fn messages(inner: &mut Inner, cursor: Option<String>, pages: usize) {
    let Some(session) = inner.state.session.clone() else {
        ready(inner);
        return;
    };
    let path = match dialect(inner) {
        Dialect::V1 => format!("/session/{session}/message{}", directory(inner)),
        Dialect::V2 => {
            let query = match &cursor {
                Some(cursor) => http::query(&[("cursor", cursor)]),
                None => http::query(&[("order", "asc"), ("limit", "200")]),
            };
            format!("/api/session/{session}/message{query}")
        }
    };
    request(inner, "GET", path, None, Pending::Messages { pages });
}

/// The chat takes prompts.
fn ready(inner: &mut Inner) {
    inner.initialized = true;
    inner.skip_replay = false;
    publish_options(inner);
    inner.emit(ChatEvent::Status {
        status: ChatStatus::Ready,
    });
    inner.send_queued();
}

/// A session's title, unless it is OpenCode's placeholder for an untitled one.
fn session_title(session: &Value) -> Option<String> {
    session["title"]
        .as_str()
        .map(str::trim)
        .filter(|title| !title.is_empty() && !title.starts_with("New session - "))
        .map(str::to_owned)
}

/// The permission rules for Compi's approval mode `chosen`, last match winning, as
/// (action, pattern, effect); none for OpenCode's own.
fn rules(chosen: Option<&str>) -> Vec<(&'static str, &'static str, &'static str)> {
    let mut rules = Vec::new();
    match chosen {
        Some("allow-all") => rules.push(("*", "*", "allow")),
        Some(mode @ ("ask" | "allow-edits")) => {
            rules.push(("*", "*", "ask"));
            rules.extend(READS.iter().map(|action| (*action, "*", "allow")));
            // Secrets in `.env` files still ask, as OpenCode's own rules have it.
            rules.extend([
                ("read", "*.env", "ask"),
                ("read", "*.env.*", "ask"),
                ("read", "*.env.example", "allow"),
            ]);
            if mode == "allow-edits" {
                rules.extend([
                    ("edit", "*", "allow"),
                    ("task", "*", "allow"),
                    ("subagent", "*", "allow"),
                ]);
            }
        }
        _ => {}
    }
    rules
}

/// The model the settings show, when OpenCode lists it.
fn current_model(opencode: &OpenCode) -> Option<&Model> {
    let model = opencode.model.as_deref()?;
    opencode
        .models
        .iter()
        .find(|offered| offered.value() == model)
}

/// A thinking level as people read it.
fn variant_name(variant: &str) -> String {
    match variant {
        DEFAULT_VARIANT => "Default".to_owned(),
        "xhigh" => "Extra high".to_owned(),
        other => capitalized(other),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The session's settings: the model, its thinking level when it has variants, and the
/// agent (mode).
fn options(opencode: &OpenCode) -> Vec<ChatOption> {
    let choice = |value: &str, name: &str| ChatChoice {
        value: value.to_owned(),
        name: name.to_owned(),
        description: None,
        group: None,
    };
    let mut options = Vec::new();
    if let Some(model) = &opencode.model {
        let mut choices: Vec<ChatChoice> = opencode
            .models
            .iter()
            .map(|offered| choice(&offered.value(), &offered.name))
            .collect();
        if !choices.iter().any(|offered| offered.value == *model) {
            choices.push(choice(model, model));
        }
        options.push(ChatOption {
            id: "model".into(),
            name: "Model".into(),
            category: Some("model".into()),
            current: model.clone(),
            choices,
        });
    }
    if let Some(model) = current_model(opencode).filter(|model| !model.variants.is_empty()) {
        let current = opencode
            .variant
            .clone()
            .unwrap_or_else(|| DEFAULT_VARIANT.to_owned());
        let mut choices = vec![choice(DEFAULT_VARIANT, &variant_name(DEFAULT_VARIANT))];
        choices.extend(
            model
                .variants
                .iter()
                .map(|variant| choice(variant, &variant_name(variant))),
        );
        if !choices.iter().any(|offered| offered.value == current) {
            choices.push(choice(&current, &variant_name(&current)));
        }
        options.push(ChatOption {
            id: "thought_level".into(),
            name: "Thinking".into(),
            category: Some("thought_level".into()),
            current,
            choices,
        });
    }
    if let Some(agent) = &opencode.agent {
        let mut choices: Vec<ChatChoice> = opencode
            .agents
            .iter()
            .map(|(id, name)| choice(id, name))
            .collect();
        if !choices.iter().any(|offered| offered.value == *agent) {
            choices.push(choice(agent, &capitalized(agent)));
        }
        options.push(ChatOption {
            id: "mode".into(),
            name: "Mode".into(),
            category: Some("mode".into()),
            current: agent.clone(),
            choices,
        });
    }
    options
}

fn publish_options(inner: &mut Inner) {
    let options = options(inner.opencode_state());
    if options != inner.state.options {
        inner.emit(ChatEvent::Options { options });
    }
    usage(inner);
}

/// Sets `option` to `value` for the next prompt. A model without the thinking level
/// shown takes its own.
fn set_option(inner: &mut Inner, option: String, value: String) -> Result<(), String> {
    let opencode = inner.opencode();
    match option.as_str() {
        "model" => {
            opencode.model = Some(value);
            let variants = current_model(opencode)
                .map(|model| model.variants.clone())
                .unwrap_or_default();
            if opencode
                .variant
                .as_ref()
                .is_some_and(|variant| variant != DEFAULT_VARIANT && !variants.contains(variant))
            {
                opencode.variant = Some(DEFAULT_VARIANT.to_owned());
            }
        }
        "thought_level" => opencode.variant = Some(value),
        "mode" => opencode.agent = Some(value),
        _ => return Err(format!("the harness has no setting {option}")),
    }
    publish_options(inner);
    Ok(())
}

/// The model the settings show as (provider, model ID), when there is one.
fn chosen_model(opencode: &OpenCode) -> Option<(String, String)> {
    let model = opencode.model.as_deref()?;
    let (provider, id) = model.split_once('/')?;
    Some((provider.to_owned(), id.to_owned()))
}

/// Starts a turn on `prompt`: in the session, or in one it opens first.
fn prompt(inner: &mut Inner, prompt: &Prompt) {
    let opencode = inner.opencode();
    opencode.turn_started = false;
    opencode.interrupting = false;
    opencode.interrupt_due = false;
    opencode.failure = None;
    opencode.todo_card = None;
    let body = match dialect(inner) {
        Dialect::V1 => v1::prompt_body(inner, prompt),
        Dialect::V2 => v2::prompt_body(prompt, None),
    };
    match inner.state.session.clone() {
        Some(session) => send_prompt(inner, &session, body),
        None => {
            inner.opencode().waiting = Some(body);
            create_session(inner);
        }
    }
}

/// Opens a new session in the chat's folder, on the settings shown and Compi's rules.
fn create_session(inner: &mut Inner) {
    let rules = rules(inner.opencode_state().chosen);
    let (path, body) = match dialect(inner) {
        Dialect::V1 => {
            let mut body = json!({});
            if !rules.is_empty() {
                body["permission"] = v1::rules(&rules);
            }
            (format!("/session{}", directory(inner)), body)
        }
        Dialect::V2 => {
            let opencode = inner.opencode_state();
            let mut body = json!({ "location": { "directory": inner.agent_folder() } });
            if let Some((provider, id)) = chosen_model(opencode) {
                body["model"] = v2::model(&provider, &id, opencode.variant.as_deref());
            }
            if let Some(agent) = &opencode.agent {
                body["agent"] = agent.clone().into();
            }
            if !rules.is_empty() {
                body["permissions"] = v2::rules(&rules);
            }
            ("/api/session".to_owned(), body)
        }
    };
    request(inner, "POST", path, Some(body), Pending::Create);
}

/// The new session opened: it is the chat's, and the waiting prompt goes to it.
fn created(inner: &mut Inner, session: String) {
    inner.emit(ChatEvent::Session {
        session: Some(session.clone()),
    });
    inner.save_thread(false);
    if dialect(inner) == Dialect::V2 {
        let opencode = inner.opencode();
        opencode.session_model = chosen_model(opencode).map(|(provider, id)| {
            (
                format!("{provider}/{id}"),
                opencode.variant.clone().unwrap_or_default(),
            )
        });
        opencode.session_agent = opencode.agent.clone();
        if let Some(note) = inner.note().map(str::to_owned) {
            request(
                inner,
                "PUT",
                format!("/api/experimental/session/{session}/instructions/entries/compi"),
                Some(json!({ "value": note })),
                Pending::Quiet,
            );
        }
    }
    let Some(body) = inner.opencode().waiting.take() else {
        return;
    };
    if std::mem::take(&mut inner.opencode().interrupt_due) {
        inner.prompt_ended(Ok(json!({ "stopReason": "cancelled" })));
        return;
    }
    send_prompt(inner, &session, body);
}

/// Sends a prompt to `session`; 2.x first takes the settings changed since its last.
fn send_prompt(inner: &mut Inner, session: &str, body: Value) {
    let path = match dialect(inner) {
        Dialect::V1 => format!("/session/{session}/prompt_async{}", directory(inner)),
        Dialect::V2 => {
            switch_settings(inner, session);
            format!("/api/session/{session}/prompt")
        }
    };
    request(inner, "POST", path, Some(body), Pending::Prompt);
}

/// 2.x: the model and agent shown, where they differ from the session's.
fn switch_settings(inner: &mut Inner, session: &str) {
    let opencode = inner.opencode();
    let shown = chosen_model(opencode).map(|(provider, id)| {
        (
            (
                format!("{provider}/{id}"),
                opencode.variant.clone().unwrap_or_default(),
            ),
            v2::model(&provider, &id, opencode.variant.as_deref()),
        )
    });
    let agent = opencode
        .agent
        .clone()
        .filter(|agent| opencode.session_agent.as_ref() != Some(agent));
    if let Some((key, model)) = shown
        && opencode.session_model.as_ref() != Some(&key)
    {
        opencode.session_model = Some(key);
        request(
            inner,
            "POST",
            format!("/api/session/{session}/model"),
            Some(json!({ "model": model })),
            Pending::Quiet,
        );
    }
    if let Some(agent) = agent {
        inner.opencode().session_agent = Some(agent.clone());
        request(
            inner,
            "POST",
            format!("/api/session/{session}/agent"),
            Some(json!({ "agent": agent })),
            Pending::Quiet,
        );
    }
}

/// The turn failed before OpenCode began it.
fn fail_turn(inner: &mut Inner, message: String) {
    inner.opencode().waiting = None;
    if inner.state.working() {
        inner.prompt_ended(Err(message));
    }
}

/// 2.x takes prompts into a running turn of an open session.
fn can_steer(inner: &Inner) -> bool {
    let opencode = inner.opencode_state();
    opencode.dialect == Some(Dialect::V2) && opencode.turn_started && inner.state.session.is_some()
}

/// Hands `prompt` to the running turn (2.x's steering).
fn steer(inner: &mut Inner, harness: &str, prompt: Prompt) -> Result<(), String> {
    let session = inner
        .state
        .session
        .clone()
        .ok_or_else(|| format!("{harness} has no session yet"))?;
    let body = v2::prompt_body(&prompt, Some("steer"));
    request(
        inner,
        "POST",
        format!("/api/session/{session}/prompt"),
        Some(body),
        Pending::Steer(prompt),
    );
    Ok(())
}

/// Stops the running turn: what OpenCode waits on is turned down.
fn cancel(inner: &mut Inner) {
    inner.cancel_requests();
    interrupt(inner);
}

/// Asks OpenCode to stop the running turn; one whose session is still opening stops
/// before its prompt goes.
fn interrupt(inner: &mut Inner) {
    let opencode = inner.opencode();
    opencode.interrupting = true;
    if opencode.waiting.is_some() {
        opencode.interrupt_due = true;
        return;
    }
    let Some(session) = inner.state.session.clone() else {
        return;
    };
    let path = match dialect(inner) {
        Dialect::V1 => format!("/session/{session}/abort{}", directory(inner)),
        Dialect::V2 => format!("/api/session/{session}/interrupt"),
    };
    request(inner, "POST", path, None, Pending::Quiet);
}

/// Opens another session in the running server, or none until the next prompt.
fn switch_session(inner: &mut Inner, session: Option<String>) {
    let opencode = inner.opencode();
    opencode.turn_started = false;
    opencode.interrupting = false;
    opencode.interrupt_due = false;
    opencode.waiting = None;
    opencode.asks.clear();
    open_session(inner, session);
}

/// The running turn began on OpenCode's side; one OpenCode began by itself shows as
/// working.
fn turn_began(inner: &mut Inner) {
    inner.opencode().turn_started = true;
    if inner.initialized && inner.state.status == ChatStatus::Ready {
        let since_ms = super::now_ms();
        inner.unseen = None;
        inner.activity_ms = since_ms;
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Working { since_ms },
        });
    }
}

/// The running turn ended: by Compi's stop, with `failure`, or done.
fn turn_ended(inner: &mut Inner, failure: Option<String>) {
    let opencode = inner.opencode();
    if !std::mem::take(&mut opencode.turn_started) {
        return;
    }
    opencode.streams.clear();
    let interrupting = std::mem::take(&mut opencode.interrupting);
    let failure = failure.or_else(|| opencode.failure.take());
    if !inner.state.working() {
        return;
    }
    let result = match failure {
        _ if interrupting => Ok(json!({ "stopReason": "cancelled" })),
        Some(message) => Err(message),
        None => Ok(json!({ "stopReason": "end_turn" })),
    };
    inner.prompt_ended(result);
}

/// An error in the session: one that stopped the turn ends it with that message; one
/// outside a turn shows.
fn session_error(inner: &mut Inner, message: String, aborted: bool) {
    let working = inner.state.working();
    let opencode = inner.opencode();
    let began = opencode.turn_started || opencode.waiting.is_some();
    if aborted {
        opencode.interrupting = true;
    } else if working {
        opencode.failure = Some(message);
    } else {
        inner.emit(ChatEvent::Entry {
            entry: ChatEntry::Error { message },
        });
    }
    // A turn that failed before OpenCode began it says no more: it ends now.
    if working && !began {
        inner.opencode().turn_started = true;
        turn_ended(inner, None);
    }
}

/// Context use from the last step's tokens, of the model's window, and the session's
/// cost when it has one.
fn step_tokens(inner: &mut Inner, tokens: &Value) {
    let count = |value: &Value| value.as_u64().unwrap_or_default();
    let used = count(&tokens["input"])
        + count(&tokens["output"])
        + count(&tokens["reasoning"])
        + count(&tokens["cache"]["read"])
        + count(&tokens["cache"]["write"]);
    if used > 0 {
        inner.opencode().used = used;
        usage(inner);
    }
}

fn session_cost(inner: &mut Inner, cost: Option<f64>) {
    if let Some(cost) = cost.filter(|cost| cost.is_finite() && *cost >= 0.0) {
        inner.opencode().cost = cost;
        usage(inner);
    }
}

fn usage(inner: &mut Inner) {
    let opencode = inner.opencode_state();
    let size = current_model(opencode).map_or(0, |model| model.context);
    if size == 0 || opencode.used == 0 {
        return;
    }
    let usage = ChatUsage {
        used: opencode.used,
        size,
        cost: (opencode.cost > 0.0).then_some(opencode.cost),
        currency: (opencode.cost > 0.0).then(|| "USD".to_owned()),
    };
    if inner.state.usage.as_ref() != Some(&usage) {
        inner.emit(ChatEvent::Usage { usage: Some(usage) });
    }
}

/// The session's title changed.
fn title(inner: &mut Inner, session: &Value) {
    if let Some(title) = session_title(session)
        && inner.state.title.as_deref() != Some(&title)
    {
        inner.emit(ChatEvent::Title { title: Some(title) });
    }
}

/// Whether `session` is the chat's or one of its sub-agents'.
fn ours(inner: &Inner, session: &str) -> bool {
    inner.state.session.as_deref() == Some(session)
        || inner.opencode_state().children.contains_key(session)
}

/// The card an ask of `session` about `call` sits on: the call's own in the chat's
/// session, the sub-agent's card for one of its sessions.
fn ask_card(inner: &Inner, session: &str, call: Option<&str>) -> Option<String> {
    if let Some(child) = inner.opencode_state().children.get(session) {
        return Some(child.call.clone());
    }
    call.filter(|call| inner.tools.contains_key(*call))
        .map(str::to_owned)
}

/// OpenCode asks permission: on the card of its call, which waits.
fn ask_permission(
    inner: &mut Inner,
    id: &str,
    session: &str,
    card: Option<String>,
    title: String,
    diffs: Vec<compi_protocol::chat::ChatDiff>,
) {
    if let Some(card) = &card {
        let mut tool = inner.tool_call(card);
        if tool.diffs.is_empty() && !diffs.is_empty() {
            tool.locations = diffs.iter().map(|diff| diff.path.clone()).collect();
            tool.diffs = diffs;
        }
        if tool.status.is_open() && tool.agents.is_empty() {
            tool.status = ChatToolStatus::Pending;
        }
        inner.end_thought();
        inner.put_tool(tool);
    }
    inner.next_permission += 1;
    let option = |id: &str, name: &str, kind: &str| ChatPermissionOption {
        id: id.to_owned(),
        name: name.to_owned(),
        kind: kind.to_owned(),
    };
    let shown = ChatPermission {
        id: inner.next_permission,
        tool: card,
        title,
        options: vec![
            option(ONCE, "Allow once", "allow_once"),
            option(ALWAYS, "Always allow", "allow_always"),
            option(REJECT, "Reject", "reject_once"),
        ],
    };
    inner.opencode().asks.push_back(Ask {
        id: id.to_owned(),
        session: session.to_owned(),
        kind: AskKind::Permission(shown),
    });
    show_ask(inner);
}

/// OpenCode asks the user: each field a question, with the value types its answers
/// go back in.
fn ask_question(
    inner: &mut Inner,
    id: &str,
    session: &str,
    message: String,
    fields: Vec<(ChatField, String)>,
) {
    inner.next_question += 1;
    let (fields, kinds): (Vec<ChatField>, Vec<(String, String)>) = fields
        .into_iter()
        .map(|(field, kind)| {
            let key = field.key.clone();
            (field, (key, kind))
        })
        .unzip();
    let shown = ChatQuestion {
        id: inner.next_question,
        message,
        fields,
    };
    inner.opencode().asks.push_back(Ask {
        id: id.to_owned(),
        session: session.to_owned(),
        kind: AskKind::Question(shown, kinds),
    });
    show_ask(inner);
}

/// Shows the first permission or question OpenCode waits on, unless it shows.
fn show_ask(inner: &mut Inner) {
    let Some(event) = inner.opencode().asks.front().map(|ask| ask.kind.event()) else {
        return;
    };
    let shown = match &event {
        ChatEvent::Permission { permission } => inner.state.permission == *permission,
        ChatEvent::Question { question } => inner.state.question == *question,
        _ => true,
    };
    if !shown {
        inner.emit(event);
    }
}

/// OpenCode no longer waits on ask `id`: it was answered elsewhere, or its turn ended.
fn resolved(inner: &mut Inner, id: &str) {
    let opencode = inner.opencode();
    let Some(position) = opencode.asks.iter().position(|ask| ask.id == id) else {
        return;
    };
    let gone = opencode.asks.remove(position).expect("found");
    if position == 0 {
        match gone.kind {
            AskKind::Permission(_) => inner.emit(ChatEvent::Permission { permission: None }),
            AskKind::Question(..) => inner.emit(ChatEvent::Question { question: None }),
        }
        show_ask(inner);
    }
}

/// Answers the permission that shows with `option`; `None` rejects it. The next ask
/// waiting shows.
fn answer(inner: &mut Inner, option: Option<String>) -> Result<(), String> {
    let Some(Ask {
        kind: AskKind::Permission(shown),
        ..
    }) = inner.opencode().asks.front()
    else {
        return Err("that permission was already answered".into());
    };
    if let Some(option) = &option
        && !shown.options.iter().any(|offered| offered.id == *option)
    {
        return Err(format!("{option} is not an answer to this permission"));
    }
    let Some(Ask {
        id,
        session,
        kind: AskKind::Permission(shown),
    }) = inner.opencode().asks.pop_front()
    else {
        unreachable!("the first ask is this permission");
    };
    let reply = option.clone().unwrap_or_else(|| REJECT.to_owned());
    reply_permission(inner, &id, &session, &reply);
    inner.emit(ChatEvent::Permission { permission: None });
    if reply != REJECT
        && let Some(call) = &shown.tool
        && inner.tools.contains_key(call)
    {
        let mut running = inner.tool_call(call);
        if running.status == ChatToolStatus::Pending {
            running.status = ChatToolStatus::Running;
            inner.put_tool(running);
        }
    }
    show_ask(inner);
    Ok(())
}

fn reply_permission(inner: &mut Inner, id: &str, session: &str, reply: &str) {
    let (path, body) = match dialect(inner) {
        Dialect::V1 => (
            format!("/permission/{id}/reply{}", directory(inner)),
            json!({ "reply": reply }),
        ),
        Dialect::V2 => (
            format!("/api/session/{session}/permission/{id}/reply"),
            json!({ "decision": reply }),
        ),
    };
    request(inner, "POST", path, Some(body), Pending::Quiet);
}

/// Answers the questions that show; `None` turns them down. The next ask waiting
/// shows.
fn answer_question(inner: &mut Inner, answers: Option<ChatAnswers>) -> Result<(), String> {
    if !matches!(
        inner.opencode().asks.front(),
        Some(Ask {
            kind: AskKind::Question(..),
            ..
        })
    ) {
        return Err("that question was already answered".into());
    }
    let Some(Ask {
        id,
        session,
        kind: AskKind::Question(_, kinds),
    }) = inner.opencode().asks.pop_front()
    else {
        unreachable!("the first ask is this question");
    };
    match answers {
        Some(mut answers) => {
            let values: Vec<(String, String, Vec<String>)> = kinds
                .into_iter()
                .map(|(key, kind)| {
                    let given = match answers.remove(&key) {
                        Some(ChatAnswer::Choices { values }) => values,
                        Some(ChatAnswer::Text { text }) => vec![text],
                        None => Vec::new(),
                    };
                    (key, kind, given)
                })
                .collect();
            let (path, body) = match dialect(inner) {
                Dialect::V1 => (
                    format!("/question/{id}/reply{}", directory(inner)),
                    json!({ "answers": values.into_iter().map(|(_, _, given)| given).collect::<Vec<_>>() }),
                ),
                Dialect::V2 => (
                    format!("/api/session/{session}/form/{id}/reply"),
                    json!({ "answer": v2::form_answer(values) }),
                ),
            };
            request(inner, "POST", path, Some(body), Pending::Quiet);
        }
        None => turn_down_question(inner, &id, &session),
    }
    inner.emit(ChatEvent::Question { question: None });
    show_ask(inner);
    Ok(())
}

fn turn_down_question(inner: &mut Inner, id: &str, session: &str) {
    let (method, path) = match dialect(inner) {
        Dialect::V1 => ("POST", format!("/question/{id}/reject{}", directory(inner))),
        Dialect::V2 => ("DELETE", format!("/api/session/{session}/form/{id}")),
    };
    request(inner, method, path, None, Pending::Quiet);
}

/// Turns down every permission and question OpenCode waits on.
fn cancel_asks(inner: &mut Inner) {
    let asks: Vec<Ask> = inner.opencode().asks.drain(..).collect();
    for ask in asks {
        match ask.kind {
            AskKind::Permission(_) => reply_permission(inner, &ask.id, &ask.session, REJECT),
            AskKind::Question(..) => turn_down_question(inner, &ask.id, &ask.session),
        }
    }
}

fn forget_asks(inner: &mut Inner) {
    inner.opencode().asks.clear();
}

/// Windows waiting on a listing hear OpenCode is gone; nothing it was asked is out any
/// more, and nothing more is sent.
fn fail_pending(inner: &mut Inner) {
    let opencode = inner.opencode();
    opencode.jobs = None;
    for (_, pending) in opencode.pending.drain() {
        if let Pending::List(reply) = pending {
            let _ = reply.send(Err(format!("{} stopped", ChatHarness::OpenCode.name())));
        }
    }
}

/// Asks OpenCode for the sessions in the chat's folder, newest first.
fn list_sessions(inner: &mut Inner) -> Result<SessionsReply, String> {
    let (reply, receiver) = mpsc::channel();
    let path = match dialect(inner) {
        Dialect::V1 => format!(
            "/session{}",
            http::query(&[
                ("directory", inner.agent_folder()),
                ("roots", "true"),
                ("limit", LISTED_SESSIONS),
            ])
        ),
        Dialect::V2 => format!(
            "/api/session{}",
            http::query(&[
                ("directory", inner.agent_folder()),
                ("order", "desc"),
                ("limit", LISTED_SESSIONS),
            ])
        ),
    };
    request(inner, "GET", path, None, Pending::List(reply));
    Ok(receiver)
}

/// The sessions a listing lists, newest first: sub-agents' sessions and sessions never
/// prompted (unchanged since they opened) are left out.
fn sessions(listed: &Value) -> Vec<ChatSessionInfo> {
    let mut sessions: Vec<(u64, ChatSessionInfo)> = listed
        .as_array()
        .into_iter()
        .flatten()
        .filter(|session| session["parentID"].as_str().is_none())
        .filter_map(|session| {
            let time = &session["time"];
            let updated = time["updated"].as_u64()?;
            if time["created"].as_u64() == Some(updated) {
                return None;
            }
            Some((
                updated,
                ChatSessionInfo {
                    id: session["id"].as_str()?.to_owned(),
                    title: session_title(session),
                    updated: Some(rfc3339(UNIX_EPOCH + Duration::from_millis(updated))),
                    folder: session["directory"]
                        .as_str()
                        .or_else(|| session["location"]["directory"].as_str())
                        .map(str::to_owned),
                    size: None,
                    first_prompt: None,
                    last_reply: None,
                },
            ))
        })
        .collect();
    sessions.sort_by_key(|(updated, _)| std::cmp::Reverse(*updated));
    sessions.into_iter().map(|(_, session)| session).collect()
}
