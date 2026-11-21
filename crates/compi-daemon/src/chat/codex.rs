//! Codex's driver: its app server (`codex app-server`), JSON-RPC 2.0 one message per
//! line, without the `jsonrpc` member. Compi asks it to `initialize` (with the
//! experimental API on, for collaboration modes), says `initialized`, lists the models
//! (`model/list`) and opens the chat's thread: `thread/resume` for a chat with a session,
//! else `thread/start`. Each prompt is a turn (`turn/start`, stopped by
//! `turn/interrupt`); Codex streams it as items (`item/started`, their deltas,
//! `item/completed`) and ends it with `turn/completed`. It asks for approvals and the
//! user's answers as requests of its own, which Compi answers.
//!
//! A thread is the chat's session. Replies and plan-mode plans stream into agent entries,
//! reasoning into thoughts; commands, file changes, MCP, web and other tool calls,
//! sub-agents (`collabAgentToolCall`, whose own threads' items become their steps) and
//! the turn's plan (`turn/plan/updated`, one card per turn) into tool cards (`items`).
//! The model, reasoning effort and collaboration mode go with the next `turn/start`, so
//! changing one restarts nothing. Codex lists its threads itself (`thread/list`), and the
//! running app server opens another one in place.
mod items;
#[cfg(test)]
mod tests;

use super::acp::{FieldShape, form_content, form_fields};
use super::launch::Arg;
use super::{Driver, Inner, Prompt, SessionsReply, Wire, rfc3339};
use compi_protocol::chat::{
    ChatAnswer, ChatAnswers, ChatChoice, ChatEntry, ChatEvent, ChatField, ChatHarness, ChatOption,
    ChatPermission, ChatPermissionOption, ChatQuestion, ChatSessionInfo, ChatStatus,
    ChatToolStatus, ChatUsage, MAX_CHAT_SESSION_PREVIEW_CHARS,
};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::time::{Duration, UNIX_EPOCH};

/// The approval decisions Compi's permission options send, as Codex names them.
const ACCEPT: &str = "accept";
const ACCEPT_FOR_SESSION: &str = "acceptForSession";
const DECLINE: &str = "decline";
/// The most pages of a listing read.
const MAX_LIST_PAGES: usize = 40;
/// The thread sources a listing shows: people's own conversations, not `codex exec`
/// runs or sub-agents.
const LISTED_SOURCES: [&str; 3] = ["cli", "vscode", "appServer"];
/// Codex's collaboration modes, as `turn/start` takes them.
const MODES: [(&str, &str); 2] = [("default", "Default"), ("plan", "Plan")];

/// Codex's driver, as the chat core calls it.
pub(super) const WIRE: Wire = Wire {
    // Codex reads its own `approval_policy` and `sandbox_mode`.
    configured_approval: |_| None,
    launch_args,
    start,
    handle: |_, inner, message| handle(inner, message),
    session_with_first_prompt: false,
    prompt,
    // Codex takes prompts as turns of their own: they queue.
    steer: None,
    cancel,
    interrupt,
    answer,
    answer_question,
    set_option: |_, inner, option, value| set_option(inner, option, value),
    list_sessions: |_, inner| list_sessions(inner),
    // A listing carries each thread's first prompt (`preview`).
    fill_sessions: |_, _| {},
    switch_session: Some(|_, inner, session| switch_session(inner, session)),
    cancel_asks,
    forget_asks,
    fail_pending,
    forget_transcript: |inner| inner.codex().forget_transcript(),
};

/// What the Codex driver keeps of the running app server and its thread.
#[derive(Default)]
pub(super) struct Codex {
    next_request: u64,
    /// The requests Compi sent that Codex has not answered, by ID.
    pending: HashMap<u64, Pending>,
    /// Approvals and questions Codex waits on, oldest first: the first shows.
    asks: VecDeque<Ask>,
    /// The approval mode Compi chose, as `policy/sandbox`; `None` keeps Codex's own.
    chosen: Option<&'static str>,
    models: Vec<Model>,
    /// The chat's thread, once open.
    thread: Option<String>,
    /// The running turn, once Codex named it.
    turn: Option<String>,
    /// Compi asked to stop the turn before Codex named it.
    interrupt_due: bool,
    /// Compi asked the running turn to stop.
    interrupting: bool,
    /// What failed the running turn, as Codex's last `error` said.
    failure: Option<String>,
    /// The model, reasoning effort and collaboration mode the settings show; `None` for
    /// the effort is the model's default.
    model: Option<String>,
    effort: Option<String>,
    mode: Option<String>,
    /// The same as Codex last had them: what differs goes with the next turn.
    thread_model: Option<String>,
    thread_effort: Option<String>,
    thread_mode: Option<String>,
    /// Tokens in the context, and the context window, once known.
    used: u64,
    size: u64,
    /// Replies, plans and reasoning being streamed, by item ID.
    streams: HashMap<String, items::Stream>,
    /// Sub-agents' threads, by thread ID.
    children: HashMap<String, items::Child>,
    /// Sub-agents' nicknames and roles, from their threads' `thread/started`.
    nicknames: HashMap<String, (Option<String>, Option<String>)>,
    /// Every message written, for tests to read.
    #[cfg(test)]
    sent: Vec<Value>,
}

impl Codex {
    /// The transcript was replaced: its entries are no longer where the driver saw them.
    fn forget_transcript(&mut self) {
        self.streams.clear();
        self.children.clear();
        self.used = 0;
    }
}

impl Inner {
    /// The Codex driver's state; only a Codex chat has one.
    fn codex(&mut self) -> &mut Codex {
        match &mut self.driver {
            Driver::Codex(codex) => codex,
            _ => unreachable!("a Codex chat speaks Codex's app server"),
        }
    }

    fn codex_state(&self) -> &Codex {
        match &self.driver {
            Driver::Codex(codex) => codex,
            _ => unreachable!("a Codex chat speaks Codex's app server"),
        }
    }
}

/// What a request Compi sent was for.
enum Pending {
    Initialize,
    /// The models listed so far, page by page (`nextCursor`).
    Models {
        listed: Vec<Model>,
        pages: usize,
    },
    StartThread,
    ResumeThread,
    Turn,
    /// Requests whose answer changes nothing (`turn/interrupt`, `thread/unsubscribe`).
    Quiet,
    /// The threads for a window, page by page.
    List {
        reply: mpsc::Sender<Result<Vec<ChatSessionInfo>, String>>,
        listed: Vec<ChatSessionInfo>,
        pages: usize,
    },
}

/// A request Codex waits on.
struct Ask {
    request: Value,
    kind: AskKind,
}

enum AskKind {
    Permission { shown: ChatPermission, reply: Reply },
    Question { shown: ChatQuestion, reply: Answers },
}

impl AskKind {
    fn event(&self) -> ChatEvent {
        match self {
            Self::Permission { shown, .. } => ChatEvent::Permission {
                permission: Some(shown.clone()),
            },
            Self::Question { shown, .. } => ChatEvent::Question {
                question: Some(shown.clone()),
            },
        }
    }
}

/// How a permission's answer goes back.
enum Reply {
    /// `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`.
    Decision,
    /// The older `execCommandApproval`, `applyPatchApproval`.
    Review,
    /// An MCP server's confirmation, a form without fields.
    Confirm,
    /// `item/permissions/requestApproval`: what it asked for, granted as asked.
    Permissions(Value),
}

impl Reply {
    /// The answer for `option`, one of `permission_options`; `None` declines.
    fn answer(&self, option: Option<&str>) -> Value {
        let accepted = matches!(option, Some(ACCEPT | ACCEPT_FOR_SESSION));
        match self {
            Self::Decision => json!({ "decision": option.unwrap_or(DECLINE) }),
            Self::Review => json!({ "decision": match option {
                Some(ACCEPT) => json!("approved"),
                Some(ACCEPT_FOR_SESSION) => json!("approved_for_session"),
                _ => json!({ "denied": { "rejection": "The user declined." } }),
            } }),
            Self::Confirm if accepted => json!({ "action": "accept", "content": {} }),
            Self::Confirm => json!({ "action": "decline" }),
            Self::Permissions(requested) if accepted => json!({
                "permissions": requested,
                "scope": if option == Some(ACCEPT) { "turn" } else { "session" },
            }),
            Self::Permissions(_) => json!({ "permissions": {} }),
        }
    }

    /// The answer when nobody will answer: the turn stops.
    fn cancelled(&self) -> Value {
        match self {
            Self::Decision => json!({ "decision": "cancel" }),
            Self::Review => json!({ "decision": "abort" }),
            Self::Confirm => json!({ "action": "cancel" }),
            Self::Permissions(_) => json!({ "permissions": {} }),
        }
    }
}

/// How a question's answers go back.
enum Answers {
    /// `item/tool/requestUserInput`: answers by question ID.
    UserInput,
    /// An MCP server's form.
    Form(Vec<FieldShape>),
}

/// A model Codex offers.
#[derive(Debug, Clone, PartialEq)]
struct Model {
    /// As `thread/start` and `turn/start` take it.
    value: String,
    name: String,
    description: Option<String>,
    /// Its reasoning efforts, with what each is for.
    efforts: Vec<(String, Option<String>)>,
    default_effort: Option<String>,
}

/// Codex's arguments: its app server. The approval mode Compi chose goes with the
/// thread and each turn.
fn launch_args(inner: &mut Inner, approval_mode: Option<&'static str>) -> Vec<Arg> {
    let codex = inner.codex();
    codex.chosen = approval_mode;
    codex.thread = None;
    codex.turn = None;
    codex.interrupt_due = false;
    codex.interrupting = false;
    codex.models.clear();
    vec![Arg::word("app-server")]
}

/// Asks Codex to initialize; its models and the thread follow.
fn start(inner: &mut Inner) {
    request(
        inner,
        "initialize",
        json!({
            "clientInfo": {
                "name": "compi",
                "title": "Compi",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "experimentalApi": true,
                "optOutNotificationMethods": ["turn/diff/updated"],
            },
        }),
        Pending::Initialize,
    );
}

/// Writes `message`.
fn send(inner: &mut Inner, message: Value) {
    #[cfg(test)]
    inner.codex().sent.push(message.clone());
    inner.write(&message);
}

fn request(inner: &mut Inner, method: &str, params: Value, pending: Pending) {
    let codex = inner.codex();
    codex.next_request += 1;
    let id = codex.next_request;
    codex.pending.insert(id, pending);
    send(
        inner,
        json!({ "id": id, "method": method, "params": params }),
    );
}

fn respond(inner: &mut Inner, id: Value, result: Value) {
    send(inner, json!({ "id": id, "result": result }));
}

/// One message from Codex: an answer, a request or a notification.
fn handle(inner: &mut Inner, message: Value) {
    match (message.get("id").cloned(), message["method"].as_str()) {
        (Some(id), Some(method)) => server_request(inner, id, method, &message["params"]),
        (Some(id), None) => response(inner, &id, &message),
        (None, Some(method)) => notification(inner, method, &message["params"]),
        (None, None) => {}
    }
}

/// A JSON-RPC error as people read it.
fn error_text(error: &Value) -> String {
    error["message"]
        .as_str()
        .filter(|message| !message.is_empty())
        .unwrap_or("unknown error")
        .to_owned()
}

fn response(inner: &mut Inner, id: &Value, message: &Value) {
    let Some(pending) = id.as_u64().and_then(|id| inner.codex().pending.remove(&id)) else {
        return;
    };
    let result = match message.get("error") {
        Some(error) => Err(error_text(error)),
        None => Ok(&message["result"]),
    };
    let harness = ChatHarness::Codex.name();
    match (pending, result) {
        (Pending::Initialize, Ok(_)) => {
            inner.initialized = true;
            send(inner, json!({ "method": "initialized" }));
            list_models(inner, None, Vec::new(), 1);
        }
        (Pending::Initialize, Err(error)) => inner.emit(ChatEvent::Status {
            status: ChatStatus::Failed {
                message: format!("{harness} didn't start: {error}"),
            },
        }),
        (Pending::Models { mut listed, pages }, Ok(result)) => {
            listed.extend(models(&result["data"]));
            match next_cursor(result, pages) {
                Some(cursor) => list_models(inner, Some(cursor), listed, pages + 1),
                None => {
                    inner.codex().models = listed;
                    let session = inner.state.session.clone();
                    open_thread(inner, session);
                }
            }
        }
        // Without its models the chat still works, on the thread's model.
        (Pending::Models { listed, .. }, Err(error)) => {
            eprintln!(
                "compi-daemon: chat {} has no model list from {harness}: {error}",
                inner.chat_id
            );
            inner.codex().models = listed;
            let session = inner.state.session.clone();
            open_thread(inner, session);
        }
        (Pending::StartThread, Ok(result)) => opened(inner, result, false),
        (Pending::StartThread, Err(error)) => inner.emit(ChatEvent::Status {
            status: ChatStatus::Failed {
                message: format!("{harness} didn't start a thread: {error}"),
            },
        }),
        (Pending::ResumeThread, Ok(result)) => opened(inner, result, true),
        // The transcript so far stays, above the error and a new thread.
        (Pending::ResumeThread, Err(error)) => {
            inner.skip_replay = false;
            inner.emit(ChatEvent::Session { session: None });
            inner.emit(ChatEvent::Entry {
                entry: ChatEntry::Error {
                    message: format!("Couldn't resume the earlier session: {error}"),
                },
            });
            inner.save_thread(true);
            open_thread(inner, None);
        }
        (Pending::Turn, Ok(result)) => {
            let turn = &result["turn"];
            if inner.state.working()
                && turn["status"] == "inProgress"
                && let Some(turn) = turn["id"].as_str()
            {
                turn_named(inner, turn);
            }
        }
        (Pending::Turn, Err(error)) => {
            inner.codex().interrupt_due = false;
            if inner.state.working() {
                inner.prompt_ended(Err(error));
            }
        }
        (Pending::Quiet, _) => {}
        (
            Pending::List {
                reply,
                mut listed,
                pages,
            },
            Ok(result),
        ) => {
            listed.extend(sessions(result));
            match next_cursor(result, pages) {
                Some(cursor) => list_page(
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
    }
}

/// The cursor of a listing's next page, unless `pages` were read already.
fn next_cursor(result: &Value, pages: usize) -> Option<String> {
    result["nextCursor"]
        .as_str()
        .filter(|cursor| !cursor.is_empty() && pages < MAX_LIST_PAGES)
        .map(str::to_owned)
}

fn list_models(inner: &mut Inner, cursor: Option<String>, listed: Vec<Model>, pages: usize) {
    let mut params = json!({ "includeHidden": false });
    if let Some(cursor) = cursor {
        params["cursor"] = cursor.into();
    }
    request(
        inner,
        "model/list",
        params,
        Pending::Models { listed, pages },
    );
}

/// The models a `model/list` page lists.
fn models(data: &Value) -> Vec<Model> {
    data.as_array()
        .into_iter()
        .flatten()
        .filter(|model| model["hidden"].as_bool() != Some(true))
        .filter_map(|model| {
            let value = model["model"]
                .as_str()
                .or_else(|| model["id"].as_str())?
                .to_owned();
            Some(Model {
                name: model["displayName"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(&value)
                    .to_owned(),
                description: model["description"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
                efforts: model["supportedReasoningEfforts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|option| {
                        Some((
                            option["reasoningEffort"].as_str()?.to_owned(),
                            option["description"]
                                .as_str()
                                .filter(|text| !text.is_empty())
                                .map(str::to_owned),
                        ))
                    })
                    .collect(),
                default_effort: model["defaultReasoningEffort"].as_str().map(str::to_owned),
                value,
            })
        })
        .collect()
}

/// Opens `session`, Codex's thread, or a new one in the chat's folder, as Compi's
/// approval mode says. A new thread starts on the user's model when Codex lists it.
fn open_thread(inner: &mut Inner, session: Option<String>) {
    let mut params = json!({ "cwd": inner.agent_folder() });
    if let Some((policy, sandbox)) = inner.codex_state().chosen.and_then(split_mode) {
        params["approvalPolicy"] = policy.into();
        params["sandbox"] = sandbox.into();
    }
    if let Some(note) = inner.note() {
        params["developerInstructions"] = note.into();
    }
    match session {
        Some(thread) => {
            params["threadId"] = thread.into();
            // A transcript Compi kept needs no replay.
            if inner.skip_replay {
                params["excludeTurns"] = true.into();
            }
            request(inner, "thread/resume", params, Pending::ResumeThread);
        }
        None => {
            let codex = inner.codex_state();
            if let Some(model) = inner
                .picks
                .model
                .as_ref()
                .filter(|model| codex.models.iter().any(|offered| offered.value == **model))
            {
                params["model"] = model.clone().into();
            }
            request(inner, "thread/start", params, Pending::StartThread);
        }
    }
}

/// Opens another thread in the running app server, leaving the one before.
fn switch_session(inner: &mut Inner, session: Option<String>) {
    if let Some(before) = inner.codex().thread.take()
        && session.as_ref() != Some(&before)
    {
        request(
            inner,
            "thread/unsubscribe",
            json!({ "threadId": before }),
            Pending::Quiet,
        );
    }
    let codex = inner.codex();
    codex.turn = None;
    codex.interrupt_due = false;
    codex.interrupting = false;
    open_thread(inner, session);
}

/// Codex's approval mode `mode` as its approval policy and sandbox mode.
fn split_mode(mode: &str) -> Option<(&str, &str)> {
    mode.split_once('/')
}

/// A sandbox mode as `turn/start` takes it, a policy.
fn sandbox_policy(sandbox: &str) -> Value {
    let kind = match sandbox {
        "read-only" => "readOnly",
        "danger-full-access" => "dangerFullAccess",
        _ => "workspaceWrite",
    };
    json!({ "type": kind })
}

/// The approval mode a thread runs with, `policy/sandbox`, from its approval policy and
/// sandbox policy; a granular policy is `granular`.
fn approval_mode(policy: &Value, sandbox: &Value) -> Option<String> {
    let policy = match policy {
        Value::String(policy) => policy.as_str(),
        Value::Object(policy) if policy.contains_key("granular") => "granular",
        _ => return None,
    };
    let sandbox = match sandbox["type"].as_str()? {
        "readOnly" => "read-only",
        "workspaceWrite" => "workspace-write",
        "dangerFullAccess" => "danger-full-access",
        "externalSandbox" => "external-sandbox",
        other => other,
    };
    Some(format!("{policy}/{sandbox}"))
}

/// Codex opened the chat's thread: its ID, title, settings and approval mode, and a
/// resumed thread's turns unless the transcript Compi kept holds them. A new thread gets
/// the user's reasoning effort, which goes with its first turn; a resumed one keeps its
/// own. Then the chat is ready.
fn opened(inner: &mut Inner, result: &Value, resumed: bool) {
    let thread = &result["thread"];
    let Some(id) = thread["id"].as_str() else {
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Failed {
                message: format!("{} opened no thread", ChatHarness::Codex.name()),
            },
        });
        return;
    };
    inner.end_thought();
    inner.prompted_with = None;
    let pick = inner.picks.thought_level.clone().filter(|_| !resumed);
    let codex = inner.codex();
    codex.thread = Some(id.to_owned());
    codex.turn = None;
    codex.thread_model = result["model"].as_str().map(str::to_owned);
    codex.model = codex.thread_model.clone();
    codex.thread_effort = result["reasoningEffort"].as_str().map(str::to_owned);
    codex.effort = codex.thread_effort.clone();
    codex.thread_mode = Some(
        result["collaborationMode"]["mode"]
            .as_str()
            .unwrap_or(MODES[0].0)
            .to_owned(),
    );
    codex.mode = codex.thread_mode.clone();
    let offered = |model: &Model| {
        model
            .efforts
            .iter()
            .any(|(effort, _)| Some(effort) == pick.as_ref())
    };
    if current_model(codex).is_some_and(offered) {
        codex.effort = pick;
    }
    if inner.state.session.as_deref() != Some(id) {
        inner.emit(ChatEvent::Session {
            session: Some(id.to_owned()),
        });
    }
    inner.save_thread(false);
    if let Some(title) = thread["name"].as_str().filter(|title| !title.is_empty())
        && inner.state.title.as_deref() != Some(title)
    {
        inner.emit(ChatEvent::Title {
            title: Some(title.to_owned()),
        });
    }
    if let Some(mode) = approval_mode(&result["approvalPolicy"], &result["sandbox"])
        && inner.state.approval_mode.as_ref() != Some(&mode)
    {
        inner.emit(ChatEvent::ApprovalMode { mode: Some(mode) });
    }
    publish_options(inner);
    if resumed && !inner.skip_replay {
        items::replay(inner, &thread["turns"]);
        if inner.thread.is_none() && !inner.state.entries.is_empty() {
            inner.start_thread();
        }
    }
    inner.skip_replay = false;
    inner.emit(ChatEvent::Status {
        status: ChatStatus::Ready,
    });
    inner.send_queued();
}

/// The model the settings show, when Codex lists it.
fn current_model(codex: &Codex) -> Option<&Model> {
    let model = codex.model.as_deref()?;
    codex.models.iter().find(|offered| offered.value == model)
}

/// The reasoning effort as people read it.
fn effort_name(effort: &str) -> String {
    match effort {
        "xhigh" => "Extra high".to_owned(),
        other => {
            let mut chars = other.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

/// The thread's settings: the model, its reasoning effort when it offers any, and the
/// collaboration mode.
fn options(codex: &Codex) -> Vec<ChatOption> {
    let choice = |value: &str, name: &str, description: Option<String>| ChatChoice {
        value: value.to_owned(),
        name: name.to_owned(),
        description,
        group: None,
    };
    let mut options = Vec::new();
    if let Some(model) = &codex.model {
        let mut choices: Vec<ChatChoice> = codex
            .models
            .iter()
            .map(|offered| choice(&offered.value, &offered.name, offered.description.clone()))
            .collect();
        // A model Codex does not list (a custom provider's) still shows as current.
        if !choices.iter().any(|offered| offered.value == *model) {
            choices.push(choice(model, model, None));
        }
        options.push(ChatOption {
            id: "model".into(),
            name: "Model".into(),
            category: Some("model".into()),
            current: model.clone(),
            choices,
        });
    }
    if let Some(model) = current_model(codex).filter(|model| !model.efforts.is_empty()) {
        let current = codex
            .effort
            .clone()
            .or_else(|| model.default_effort.clone())
            .unwrap_or_else(|| model.efforts[0].0.clone());
        let mut choices: Vec<ChatChoice> = model
            .efforts
            .iter()
            .map(|(effort, description)| choice(effort, &effort_name(effort), description.clone()))
            .collect();
        if !choices.iter().any(|offered| offered.value == current) {
            choices.push(choice(&current, &effort_name(&current), None));
        }
        options.push(ChatOption {
            id: "thought_level".into(),
            name: "Reasoning".into(),
            category: Some("thought_level".into()),
            current,
            choices,
        });
    }
    if let Some(mode) = &codex.mode {
        let mut choices: Vec<ChatChoice> = MODES
            .iter()
            .map(|(value, name)| choice(value, name, None))
            .collect();
        if !choices.iter().any(|offered| offered.value == *mode) {
            choices.push(choice(mode, mode, None));
        }
        options.push(ChatOption {
            id: "mode".into(),
            name: "Mode".into(),
            category: Some("mode".into()),
            current: mode.clone(),
            choices,
        });
    }
    options
}

fn publish_options(inner: &mut Inner) {
    let options = options(inner.codex_state());
    if options != inner.state.options {
        inner.emit(ChatEvent::Options { options });
    }
}

/// Sets `option` to `value`, one of its choices, for the next turn. A model that lacks
/// the reasoning effort shown takes its own default.
fn set_option(inner: &mut Inner, option: String, value: String) -> Result<(), String> {
    let codex = inner.codex();
    match option.as_str() {
        "model" => {
            codex.model = Some(value);
            let unoffered = |model: &&Model| {
                codex.effort.as_ref().is_some_and(|effort| {
                    !model.efforts.iter().any(|(offered, _)| offered == effort)
                })
            };
            if let Some(default) = current_model(codex)
                .filter(unoffered)
                .map(|model| model.default_effort.clone())
            {
                codex.effort = default;
            }
        }
        "thought_level" => codex.effort = Some(value),
        "mode" => codex.mode = Some(value),
        _ => return Err(format!("the harness has no setting {option}")),
    }
    publish_options(inner);
    Ok(())
}

/// Starts a turn on `prompt`, with the settings the user changed since the thread's
/// last turn and the approval mode Compi chose.
fn prompt(inner: &mut Inner, prompt: &Prompt) {
    let cwd = inner.agent_folder().to_owned();
    let codex = inner.codex();
    codex.interrupting = false;
    codex.interrupt_due = false;
    codex.failure = None;
    codex.turn = None;
    // Prompts wait for the thread (`Inner::has_session`).
    let Some(thread) = codex.thread.clone() else {
        inner.prompt_ended(Err(format!(
            "{} has no thread open",
            ChatHarness::Codex.name()
        )));
        return;
    };
    let mut params = json!({ "threadId": thread, "input": prompt_input(prompt), "cwd": cwd });
    if codex.model != codex.thread_model
        && let Some(model) = &codex.model
    {
        params["model"] = model.clone().into();
    }
    if codex.effort != codex.thread_effort
        && let Some(effort) = &codex.effort
    {
        params["effort"] = effort.clone().into();
    }
    if codex.mode != codex.thread_mode
        && let (Some(mode), Some(model)) = (&codex.mode, &codex.model)
    {
        let mut settings = json!({ "model": model });
        if let Some(effort) = &codex.effort {
            settings["reasoning_effort"] = effort.clone().into();
        }
        params["collaborationMode"] = json!({ "mode": mode, "settings": settings });
    }
    if let Some((policy, sandbox)) = codex.chosen.and_then(split_mode) {
        params["approvalPolicy"] = policy.into();
        params["sandboxPolicy"] = sandbox_policy(sandbox);
    }
    codex.thread_model = codex.model.clone();
    codex.thread_effort = codex.effort.clone();
    codex.thread_mode = codex.mode.clone();
    request(inner, "turn/start", params, Pending::Turn);
}

/// A prompt as Codex's input: its text with the attached files' paths, then its images
/// as data URLs.
fn prompt_input(prompt: &Prompt) -> Vec<Value> {
    let mut text = prompt.text.clone();
    if !prompt.files.is_empty() {
        if !text.trim().is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&prompt.files.join("\n"));
    }
    let mut input = Vec::new();
    if !text.trim().is_empty() {
        input.push(json!({ "type": "text", "text": text, "text_elements": [] }));
    }
    input.extend(prompt.images.iter().map(|image| {
        json!({ "type": "image", "url": format!("data:{};base64,{}", image.mime, image.data) })
    }));
    input
}

/// Codex named the running turn: a stop asked for before goes now.
fn turn_named(inner: &mut Inner, turn: &str) {
    let codex = inner.codex();
    codex.turn = Some(turn.to_owned());
    if std::mem::take(&mut codex.interrupt_due) {
        interrupt(inner);
    }
}

/// Stops the running turn: what Codex waits on is cancelled.
fn cancel(inner: &mut Inner) {
    inner.codex().interrupting = true;
    inner.cancel_requests();
    interrupt(inner);
}

/// Asks Codex to stop the running turn, once it has named it.
fn interrupt(inner: &mut Inner) {
    let codex = inner.codex();
    match (codex.thread.clone(), codex.turn.clone()) {
        (Some(thread), Some(turn)) => request(
            inner,
            "turn/interrupt",
            json!({ "threadId": thread, "turnId": turn }),
            Pending::Quiet,
        ),
        _ => codex.interrupt_due = true,
    }
}

/// A notification from Codex: the chat's thread's, a sub-agent's thread's, or the app
/// server's own.
fn notification(inner: &mut Inner, method: &str, params: &Value) {
    let thread = params["threadId"].as_str();
    let codex = inner.codex_state();
    let ours = thread.is_some() && thread == codex.thread.as_deref();
    let child = thread.filter(|thread| codex.children.contains_key(*thread));
    let item = || params["itemId"].as_str().unwrap_or_default();
    let delta = || params["delta"].as_str().unwrap_or_default();
    match method {
        "thread/started" => items::thread_started(inner, &params["thread"]),
        "serverRequest/resolved" => resolved(inner, &params["requestId"]),
        _ if ours => match method {
            "error" => error(inner, params),
            "thread/name/updated" => {
                let title = params["threadName"]
                    .as_str()
                    .filter(|title| !title.is_empty())
                    .map(str::to_owned);
                if title != inner.state.title {
                    inner.emit(ChatEvent::Title { title });
                }
            }
            "thread/tokenUsage/updated" => usage(inner, &params["tokenUsage"]),
            "thread/settings/updated" => settings(inner, &params["threadSettings"]),
            "turn/started" => turn_started(inner, &params["turn"]),
            "turn/completed" => turn_completed(inner, &params["turn"]),
            "turn/plan/updated" => items::plan_updated(inner, params),
            "item/started" => items::started(inner, &params["item"]),
            "item/completed" => items::completed(inner, &params["item"], false),
            "item/agentMessage/delta" | "item/plan/delta" => {
                items::reply_delta(inner, item(), delta());
            }
            "item/reasoning/summaryTextDelta" => items::thought_delta(
                inner,
                item(),
                params["summaryIndex"].as_i64().unwrap_or_default(),
                delta(),
            ),
            // Raw reasoning, when Codex shares it, counts its parts apart from summaries.
            "item/reasoning/textDelta" => items::thought_delta(
                inner,
                item(),
                -1 - params["contentIndex"].as_i64().unwrap_or_default(),
                delta(),
            ),
            "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => {
                items::output_delta(inner, item(), delta());
            }
            "item/fileChange/patchUpdated" => {
                items::patch_updated(inner, item(), &params["changes"]);
            }
            "item/mcpToolCall/progress" => {
                items::progress(
                    inner,
                    item(),
                    params["message"].as_str().unwrap_or_default(),
                );
            }
            _ => {}
        },
        _ => {
            let Some(child) = child else {
                return;
            };
            match method {
                "turn/started" => items::child_turn(inner, child, &params["turn"], false),
                "turn/completed" => items::child_turn(inner, child, &params["turn"], true),
                "item/started" => items::child_item(inner, child, &params["item"], false),
                "item/completed" => items::child_item(inner, child, &params["item"], true),
                _ => {}
            }
        }
    }
}

/// An error in the running turn: one Codex retries shows; one it does not ends the
/// turn, whose end says it.
fn error(inner: &mut Inner, params: &Value) {
    let message = turn_error(&params["error"]);
    if params["willRetry"].as_bool() == Some(true) {
        inner.emit(ChatEvent::Entry {
            entry: ChatEntry::Error {
                message: format!("{message} (retrying)"),
            },
        });
    } else if inner.state.working() {
        inner.codex().failure = Some(message);
    } else {
        inner.emit(ChatEvent::Entry {
            entry: ChatEntry::Error { message },
        });
    }
}

/// A `TurnError` as people read it: its message, then its details.
fn turn_error(error: &Value) -> String {
    let message = error["message"]
        .as_str()
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or("unknown error");
    match error["additionalDetails"]
        .as_str()
        .map(str::trim)
        .filter(|details| !details.is_empty())
    {
        Some(details) => format!("{message}: {details}"),
        None => message.to_owned(),
    }
}

/// Context use: the last request's tokens, of the model's context window.
fn usage(inner: &mut Inner, usage: &Value) {
    let codex = inner.codex();
    if let Some(used) = usage["last"]["totalTokens"].as_u64() {
        codex.used = used;
    }
    if let Some(size) = usage["modelContextWindow"]
        .as_u64()
        .filter(|size| *size > 0)
    {
        codex.size = size;
    }
    if codex.size == 0 {
        return;
    }
    let usage = ChatUsage {
        used: codex.used,
        size: codex.size,
        cost: None,
        currency: None,
    };
    if inner.state.usage.as_ref() != Some(&usage) {
        inner.emit(ChatEvent::Usage { usage: Some(usage) });
    }
}

/// The thread's settings changed: what the user did not change since follows them, and
/// the approval mode shows them.
fn settings(inner: &mut Inner, settings: &Value) {
    let follow = |shown: &mut Option<String>, own: &mut Option<String>, now: Option<&str>| {
        let now = now.map(str::to_owned);
        if shown == own {
            shown.clone_from(&now);
        }
        *own = now;
    };
    let codex = inner.codex();
    follow(
        &mut codex.model,
        &mut codex.thread_model,
        settings["model"].as_str(),
    );
    follow(
        &mut codex.effort,
        &mut codex.thread_effort,
        settings["effort"].as_str(),
    );
    if let Some(mode) = settings["collaborationMode"]["mode"].as_str() {
        follow(&mut codex.mode, &mut codex.thread_mode, Some(mode));
    }
    publish_options(inner);
    if let Some(mode) = approval_mode(&settings["approvalPolicy"], &settings["sandboxPolicy"])
        && inner.state.approval_mode.as_ref() != Some(&mode)
    {
        inner.emit(ChatEvent::ApprovalMode { mode: Some(mode) });
    }
}

/// A turn of the chat's thread began; one Codex began by itself shows as working.
fn turn_started(inner: &mut Inner, turn: &Value) {
    if let Some(id) = turn["id"].as_str() {
        turn_named(inner, id);
    }
    if inner.initialized && inner.state.status == ChatStatus::Ready {
        let since_ms = super::now_ms();
        inner.unseen = None;
        inner.activity_ms = since_ms;
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Working { since_ms },
        });
    }
}

/// The chat's turn ended: completed, interrupted or failed.
fn turn_completed(inner: &mut Inner, turn: &Value) {
    let codex = inner.codex();
    codex.turn = None;
    codex.interrupt_due = false;
    codex.streams.clear();
    let interrupting = std::mem::take(&mut codex.interrupting);
    let failure = codex.failure.take();
    if !inner.state.working() {
        return;
    }
    let stop = |reason: &str| Ok(json!({ "stopReason": reason }));
    let result = match turn["status"].as_str() {
        Some("interrupted") => stop("cancelled"),
        _ if interrupting => stop("cancelled"),
        Some("failed") => Err(if turn["error"].is_object() {
            turn_error(&turn["error"])
        } else {
            failure.unwrap_or_else(|| "The turn failed.".to_owned())
        }),
        _ => stop("end_turn"),
    };
    inner.prompt_ended(result);
}

/// A request from Codex: an approval or the user's answers; Compi offers no others.
fn server_request(inner: &mut Inner, id: Value, method: &str, params: &Value) {
    let ask = match method {
        "item/commandExecution/requestApproval" => command_approval(inner, params),
        "item/fileChange/requestApproval" => file_approval(inner, params),
        "item/permissions/requestApproval" => permissions_approval(inner, params),
        "execCommandApproval" => legacy_command_approval(inner, params),
        "applyPatchApproval" => legacy_patch_approval(inner, params),
        "item/tool/requestUserInput" => user_input(inner, params),
        "mcpServer/elicitation/request" => match elicitation(inner, params) {
            Some(kind) => kind,
            None => {
                respond(inner, id, json!({ "action": "decline" }));
                return;
            }
        },
        _ => {
            send(
                inner,
                json!({
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Compi does not offer {method}"),
                    },
                }),
            );
            return;
        }
    };
    inner.codex().asks.push_back(Ask {
        request: id,
        kind: ask,
    });
    show_ask(inner);
}

/// The tool card an approval of `item` in `thread` sits on: the item's own on the
/// chat's thread, a sub-agent's call for its thread's.
fn ask_card(inner: &Inner, thread: Option<&str>, item: Option<&str>) -> Option<String> {
    let codex = inner.codex_state();
    match thread {
        Some(thread) if codex.thread.as_deref() != Some(thread) => {
            codex.children.get(thread).map(|child| child.call.clone())
        }
        _ => item.map(str::to_owned),
    }
}

/// The card `call` waits on an approval.
fn wait_on(inner: &mut Inner, call: &str) {
    if !inner.tools.contains_key(call) {
        return;
    }
    let mut tool = inner.tool_call(call);
    if tool.status.is_open() && tool.agents.is_empty() {
        tool.status = ChatToolStatus::Pending;
        inner.put_tool(tool);
    }
}

fn permission(
    inner: &mut Inner,
    tool: Option<String>,
    title: String,
    always: bool,
    reply: Reply,
) -> AskKind {
    inner.next_permission += 1;
    AskKind::Permission {
        shown: ChatPermission {
            id: inner.next_permission,
            tool,
            title,
            options: permission_options(always),
        },
        reply,
    }
}

fn permission_options(always: bool) -> Vec<ChatPermissionOption> {
    let option = |id: &str, name: &str, kind: &str| ChatPermissionOption {
        id: id.to_owned(),
        name: name.to_owned(),
        kind: kind.to_owned(),
    };
    let mut options = vec![option(ACCEPT, "Allow once", "allow_once")];
    if always {
        options.push(option(ACCEPT_FOR_SESSION, "Always allow", "allow_always"));
    }
    options.push(option(DECLINE, "Reject", "reject_once"));
    options
}

/// Codex asks to run a command: on its card, made from the request when the item has
/// not shown yet.
fn command_approval(inner: &mut Inner, params: &Value) -> AskKind {
    let item = params["itemId"].as_str();
    let card = ask_card(inner, params["threadId"].as_str(), item);
    let command = params["command"]
        .as_str()
        .map(|command| items::command_line(command, &params["commandActions"]));
    if let Some(item) = item.filter(|item| card.as_deref() == Some(*item)) {
        let mut tool = inner.tool_call(item);
        if tool.title.is_empty() {
            items::describe_command(
                &mut tool,
                params["command"].as_str().unwrap_or_default(),
                &params["commandActions"],
                &params["cwd"],
            );
        }
        inner.end_thought();
        inner.put_tool(tool);
    }
    if let Some(card) = &card {
        wait_on(inner, card);
    }
    let reason = params["reason"]
        .as_str()
        .filter(|reason| !reason.is_empty());
    let title = command
        .filter(|command| !command.is_empty())
        .or(reason.map(str::to_owned))
        .unwrap_or_else(|| "Run a command".to_owned());
    // A managed checkpoint takes a one-time decision only.
    let always = params["kind"] != "humanCommand";
    permission(inner, card, title, always, Reply::Decision)
}

/// Codex asks to change files: its card shows the changes.
fn file_approval(inner: &mut Inner, params: &Value) -> AskKind {
    let card = ask_card(
        inner,
        params["threadId"].as_str(),
        params["itemId"].as_str(),
    );
    let shown = card
        .as_deref()
        .filter(|card| inner.tools.contains_key(*card))
        .map(|card| inner.tool_call(card).title)
        .filter(|title| !title.is_empty());
    if let Some(card) = &card {
        wait_on(inner, card);
    }
    let title = match params["grantRoot"].as_str().filter(|root| !root.is_empty()) {
        Some(root) => format!("Allow writing in {root}"),
        None => shown
            .or_else(|| params["reason"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "Change files".to_owned()),
    };
    permission(inner, card, title, true, Reply::Decision)
}

/// Codex asks for more than its sandbox allows: network access, folders to write.
fn permissions_approval(inner: &mut Inner, params: &Value) -> AskKind {
    let card = ask_card(
        inner,
        params["threadId"].as_str(),
        params["itemId"].as_str(),
    )
    .filter(|card| inner.tools.contains_key(card));
    let requested = &params["permissions"];
    let writes: Vec<&str> = requested["fileSystem"]["write"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let title = match params["reason"]
        .as_str()
        .filter(|reason| !reason.is_empty())
    {
        Some(reason) => reason.to_owned(),
        None if !writes.is_empty() => format!("Allow writing to {}", writes.join(", ")),
        None if requested["network"]["enabled"].as_bool() == Some(true) => {
            "Allow network access".to_owned()
        }
        None => "Allow more permissions".to_owned(),
    };
    permission(
        inner,
        card,
        title,
        true,
        Reply::Permissions(requested.clone()),
    )
}

/// The older approval of a command, by its call.
fn legacy_command_approval(inner: &mut Inner, params: &Value) -> AskKind {
    let card = params["callId"]
        .as_str()
        .filter(|call| inner.tools.contains_key(*call))
        .map(str::to_owned);
    if let Some(card) = &card {
        wait_on(inner, card);
    }
    let command: Vec<&str> = params["command"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let title = if command.is_empty() {
        "Run a command".to_owned()
    } else {
        command.join(" ")
    };
    permission(inner, card, title, true, Reply::Review)
}

/// The older approval of a patch, by its call.
fn legacy_patch_approval(inner: &mut Inner, params: &Value) -> AskKind {
    let card = params["callId"]
        .as_str()
        .filter(|call| inner.tools.contains_key(*call))
        .map(str::to_owned);
    if let Some(card) = &card {
        wait_on(inner, card);
    }
    let paths: Vec<&String> = params["fileChanges"]
        .as_object()
        .map(|changes| changes.keys().collect())
        .unwrap_or_default();
    let title = match paths.as_slice() {
        [path] => format!("Edit {path}"),
        [] => "Change files".to_owned(),
        paths => format!("Edit {} files", paths.len()),
    };
    permission(inner, card, title, true, Reply::Review)
}

/// Codex asks the user questions (`request_user_input`): a field per question, its
/// options as choices.
fn user_input(inner: &mut Inner, params: &Value) -> AskKind {
    let mut fields = Vec::new();
    let mut headers = Vec::new();
    for question in params["questions"].as_array().into_iter().flatten() {
        let (Some(key), Some(text)) = (question["id"].as_str(), question["question"].as_str())
        else {
            continue;
        };
        let choices: Vec<ChatChoice> = question["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|option| {
                let label = option["label"].as_str()?.to_owned();
                Some(ChatChoice {
                    value: label.clone(),
                    name: label,
                    description: option["description"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned),
                    group: None,
                })
            })
            .collect();
        if let Some(header) = question["header"]
            .as_str()
            .filter(|header| !header.is_empty())
        {
            headers.push(header.to_owned());
        }
        fields.push(ChatField {
            key: key.to_owned(),
            title: text.to_owned(),
            other: !choices.is_empty() && question["isOther"].as_bool() == Some(true),
            choices,
            multi: false,
            required: true,
        });
    }
    inner.next_question += 1;
    AskKind::Question {
        shown: ChatQuestion {
            id: inner.next_question,
            message: if headers.is_empty() {
                format!("{} asks", ChatHarness::Codex.name())
            } else {
                headers.join(" · ")
            },
            fields,
        },
        reply: Answers::UserInput,
    }
}

/// An MCP server asks through Codex: a form with fields is a question, one without is
/// a confirmation (a tool call's approval). One that sends the user to a page is
/// declined (`None`).
fn elicitation(inner: &mut Inner, params: &Value) -> Option<AskKind> {
    if params["mode"] == "url" {
        return None;
    }
    let message = params["message"]
        .as_str()
        .filter(|message| !message.is_empty())
        .map_or_else(
            || format!("{} asks", ChatHarness::Codex.name()),
            str::to_owned,
        );
    let (fields, shapes) = form_fields(&params["requestedSchema"]);
    if fields.is_empty() {
        return Some(permission(inner, None, message, false, Reply::Confirm));
    }
    inner.next_question += 1;
    Some(AskKind::Question {
        shown: ChatQuestion {
            id: inner.next_question,
            message,
            fields,
        },
        reply: Answers::Form(shapes),
    })
}

/// Shows the first approval or question Codex waits on, unless it shows.
fn show_ask(inner: &mut Inner) {
    let Some(event) = inner.codex().asks.front().map(|ask| ask.kind.event()) else {
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

/// Answers the approval that shows with `option`; `None` declines it. The next one
/// waiting shows.
fn answer(inner: &mut Inner, option: Option<String>) -> Result<(), String> {
    let Some(Ask {
        kind: AskKind::Permission { shown, .. },
        ..
    }) = inner.codex().asks.front()
    else {
        return Err("that permission was already answered".into());
    };
    if let Some(option) = &option
        && !shown.options.iter().any(|offered| offered.id == *option)
    {
        return Err(format!("{option} is not an answer to this permission"));
    }
    let Some(Ask {
        request,
        kind: AskKind::Permission { shown, reply },
    }) = inner.codex().asks.pop_front()
    else {
        unreachable!("the first ask is this permission");
    };
    respond(inner, request, reply.answer(option.as_deref()));
    inner.emit(ChatEvent::Permission { permission: None });
    if option.as_deref().is_some_and(|option| option != DECLINE)
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

/// Answers the questions that show; `None` declines them. The next ask waiting shows.
fn answer_question(inner: &mut Inner, answers: Option<ChatAnswers>) -> Result<(), String> {
    if !matches!(
        inner.codex().asks.front(),
        Some(Ask {
            kind: AskKind::Question { .. },
            ..
        })
    ) {
        return Err("that question was already answered".into());
    }
    let Some(Ask {
        request,
        kind: AskKind::Question { reply, .. },
    }) = inner.codex().asks.pop_front()
    else {
        unreachable!("the first ask is this question");
    };
    let response = match (reply, answers) {
        (Answers::UserInput, answers) => {
            let mut given = Map::new();
            for (key, answer) in answers.into_iter().flatten() {
                let values = match answer {
                    ChatAnswer::Choices { values } => values,
                    ChatAnswer::Text { text } => vec![text],
                };
                given.insert(key, json!({ "answers": values }));
            }
            json!({ "answers": given })
        }
        (Answers::Form(shapes), Some(answers)) => {
            json!({ "action": "accept", "content": form_content(&shapes, answers) })
        }
        (Answers::Form(_), None) => json!({ "action": "decline" }),
    };
    respond(inner, request, response);
    inner.emit(ChatEvent::Question { question: None });
    show_ask(inner);
    Ok(())
}

/// Cancels every approval and question Codex waits on.
fn cancel_asks(inner: &mut Inner) {
    let asks: Vec<Ask> = inner.codex().asks.drain(..).collect();
    for ask in asks {
        let response = match &ask.kind {
            AskKind::Permission { reply, .. } => reply.cancelled(),
            AskKind::Question {
                reply: Answers::UserInput,
                ..
            } => json!({ "answers": {} }),
            AskKind::Question {
                reply: Answers::Form(_),
                ..
            } => json!({ "action": "cancel" }),
        };
        respond(inner, ask.request, response);
    }
}

fn forget_asks(inner: &mut Inner) {
    inner.codex().asks.clear();
}

/// Windows waiting on a listing hear Codex is gone; nothing it was asked is out any more.
fn fail_pending(inner: &mut Inner) {
    for (_, pending) in inner.codex().pending.drain() {
        if let Pending::List { reply, .. } = pending {
            let _ = reply.send(Err(format!("{} stopped", ChatHarness::Codex.name())));
        }
    }
}

/// Codex no longer waits on request `id`: it was answered elsewhere, or its turn ended.
fn resolved(inner: &mut Inner, id: &Value) {
    let codex = inner.codex();
    let Some(position) = codex.asks.iter().position(|ask| ask.request == *id) else {
        return;
    };
    let gone = codex.asks.remove(position).expect("found");
    if position == 0 {
        match gone.kind {
            AskKind::Permission { .. } => inner.emit(ChatEvent::Permission { permission: None }),
            AskKind::Question { .. } => inner.emit(ChatEvent::Question { question: None }),
        }
        show_ask(inner);
    }
}

/// Asks Codex for its threads in the chat's folder, newest first, page by page.
fn list_sessions(inner: &mut Inner) -> Result<SessionsReply, String> {
    let (reply, receiver) = mpsc::channel();
    list_page(
        inner,
        None,
        Pending::List {
            reply,
            listed: Vec::new(),
            pages: 1,
        },
    );
    Ok(receiver)
}

fn list_page(inner: &mut Inner, cursor: Option<String>, pending: Pending) {
    let mut params = json!({
        "cwd": inner.agent_folder(),
        "sortKey": "updated_at",
        "sortDirection": "desc",
        "sourceKinds": LISTED_SOURCES,
    });
    if let Some(cursor) = cursor {
        params["cursor"] = cursor.into();
    }
    request(inner, "thread/list", params, pending);
}

/// The threads a listing page lists, each with its first prompt (`preview`); threads
/// without one, sub-agents' and ephemeral ones are left out.
fn sessions(result: &Value) -> Vec<ChatSessionInfo> {
    result["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|thread| thread["parentThreadId"].as_str().is_none() && thread["ephemeral"] != true)
        .filter_map(|thread| {
            let preview = thread["preview"]
                .as_str()
                .map(str::trim)
                .filter(|preview| !preview.is_empty())?;
            Some(ChatSessionInfo {
                id: thread["id"].as_str()?.to_owned(),
                title: thread["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
                updated: thread["updatedAt"]
                    .as_u64()
                    .map(|seconds| rfc3339(UNIX_EPOCH + Duration::from_secs(seconds))),
                folder: thread["cwd"].as_str().map(str::to_owned),
                size: None,
                first_prompt: Some(
                    preview
                        .chars()
                        .take(MAX_CHAT_SESSION_PREVIEW_CHARS)
                        .collect(),
                ),
                last_reply: None,
            })
        })
        .collect()
}
