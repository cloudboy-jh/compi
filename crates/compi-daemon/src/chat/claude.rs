//! Claude Code's driver: its headless JSON stream (`claude --input-format stream-json
//! --output-format stream-json`), one JSON message per line, as Anthropic's Agent SDK
//! speaks it. Compi writes user messages and control requests (`initialize`,
//! `interrupt`, `set_model`, …) and answers Claude Code's own (`can_use_tool`, which
//! asks permission for a tool call, or the user's answers for `AskUserQuestion`); Claude
//! Code writes its messages, as streamed (`stream_event`) and whole, its tool results,
//! its sub-agents' (`task_*`) news and each turn's `result`.
//!
//! A new chat's session is Claude Code's, named with its first prompt (`system/init`);
//! a chat on a saved session starts Claude Code on it (`--resume`). Claude Code's
//! sessions are its files (`store`). A thinking level is a start option only, so
//! changing it starts Claude Code again on the session.
mod store;

use super::launch::Arg;
use super::{Chat, Driver, Inner, Prompt, head, new_agent, now_ms, settle, short_line, tail};
use compi_protocol::chat::{
    ChatAgentStep, ChatAnswer, ChatAnswers, ChatChoice, ChatCommand, ChatDiff, ChatEntry,
    ChatEvent, ChatField, ChatHarness, ChatOption, ChatPermission, ChatPermissionOption,
    ChatQuestion, ChatSessionInfo, ChatStatus, ChatTool, ChatToolStatus, ChatUsage,
    MAX_CHAT_AGENT_RESULT_BYTES, MAX_CHAT_AGENT_STEPS, MAX_CHAT_DIFF_BYTES,
    MAX_CHAT_TOOL_INPUT_BYTES, MAX_CHAT_TOOL_OUTPUT_BYTES,
};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, mpsc};
use std::thread;

/// How Compi runs Claude Code: JSON in and out, every message, permissions asked of
/// Compi, replies streamed as they are written.
const STREAM_ARGS: [&str; 8] = [
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
    "--permission-prompt-tool",
    "stdio",
    "--include-partial-messages",
];
/// The thinking levels `--effort` takes.
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// The choice that keeps Claude Code's own model or thinking level.
const DEFAULT: &str = "default";
/// The permission modes the mode setting offers, as Claude Code names them.
const MODES: [(&str, &str); 4] = [
    ("default", "Default"),
    ("acceptEdits", "Accept edits"),
    ("plan", "Plan mode"),
    ("bypassPermissions", "Bypass permissions"),
];

/// What the Claude Code driver keeps of the running harness and its session.
#[derive(Default)]
pub(super) struct Claude {
    next_request: u64,
    /// The control requests Compi sent that Claude Code has not answered, by ID.
    pending: HashMap<String, Control>,
    /// Permissions and questions Claude Code waits on, oldest first: the first shows.
    asks: VecDeque<Ask>,
    /// The next start keeps the model and thinking level chosen in Compi, even on a
    /// new session: the thinking level changed.
    relaunch: bool,
    /// The running Claude Code was started on a saved session (`--resume`).
    resuming: bool,
    /// The user's model, for a new session once Claude Code lists its models.
    pick_model: Option<String>,
    models: Vec<Model>,
    /// The model chosen in Compi; `None` is Claude Code's own.
    model: Option<String>,
    /// The thinking level Claude Code was started with; `None` is its own.
    effort: Option<String>,
    /// Claude Code's permission mode, once it says.
    mode: Option<String>,
    /// Claude Code's configuration folder, as the running `claude` names it: where its
    /// sessions are.
    config: Option<String>,
    /// The chat's folder, as the running `claude` names it (`system/init`).
    cwd: Option<String>,
    /// The model of the latest reply, as Claude Code names it.
    reply_model: Option<String>,
    /// Tokens in the context, and the context window, once known.
    used: u64,
    size: u64,
    /// What the session's turns cost, in US dollars, and what the running process
    /// reported last (its running total).
    cost: f64,
    process_cost: f64,
    /// Compi asked the running turn to stop.
    interrupting: bool,
    /// The reply being streamed.
    stream: Stream,
    /// Sub-agents, by Claude Code's task ID.
    tasks: HashMap<String, Task>,
    /// The tool calls a sub-agent's card shows as steps, in order, by its Agent call.
    steps: HashMap<String, VecDeque<String>>,
}

impl Claude {
    /// The transcript was replaced: its entries are no longer where the driver saw them.
    pub(super) fn forget_transcript(&mut self) {
        self.stream = Stream::default();
        self.tasks.clear();
        self.steps.clear();
        self.used = 0;
        self.cost = 0.0;
    }
}

impl Inner {
    /// The Claude Code driver's state; only a Claude Code chat has one.
    fn claude(&mut self) -> &mut Claude {
        match &mut self.driver {
            Driver::Claude(claude) => claude,
            Driver::Acp(_) => unreachable!("a Claude Code chat speaks Claude Code's stream"),
        }
    }

    fn claude_state(&self) -> &Claude {
        match &self.driver {
            Driver::Claude(claude) => claude,
            Driver::Acp(_) => unreachable!("a Claude Code chat speaks Claude Code's stream"),
        }
    }
}

/// What a control request Compi sent was for.
enum Control {
    Initialize,
    /// The user's model for a new session; the chat is ready once it is set.
    Pick(String),
    SetModel(String),
    SetMode(String),
    Interrupt,
    ContextUsage,
}

/// A `can_use_tool` request Claude Code waits on.
struct Ask {
    request: String,
    /// The tool call's input, which an answer hands back.
    input: Value,
    kind: AskKind,
}

enum AskKind {
    Permission {
        shown: ChatPermission,
        /// What "Always allow" sets: Claude Code's own suggestions.
        suggestions: Vec<Value>,
        /// It asks to leave plan mode (`ExitPlanMode`).
        plan: bool,
    },
    /// `AskUserQuestion`: each field answers the question at its index.
    Question {
        shown: ChatQuestion,
        questions: Vec<String>,
    },
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

/// A model Claude Code offers.
#[derive(Debug, Clone, PartialEq)]
struct Model {
    value: String,
    name: String,
    description: Option<String>,
    /// The thinking levels it takes; none when it takes no effort setting.
    efforts: Vec<String>,
}

/// The content blocks of the reply being streamed, by their index.
#[derive(Default)]
struct Stream {
    message: Option<String>,
    blocks: Vec<Block>,
}

struct Block {
    index: u64,
    kind: BlockKind,
    /// The entry its text went to, and where in that entry's text it began.
    entry: Option<(usize, usize)>,
    /// Its text as streamed.
    text: String,
    started_ms: u64,
    /// The whole message's block replaced what was streamed.
    reconciled: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum BlockKind {
    Text,
    Thinking,
    Tool,
}

/// A sub-agent: the Agent call that started it, and its name on that call's card.
#[derive(Clone)]
struct Task {
    call: String,
    agent: String,
}

/// Claude Code's arguments: its stream, the approval mode when there is one, and the
/// session: a saved one is resumed; a new one, which Claude Code names with its first
/// prompt, starts on the user's thinking level.
pub(super) fn launch_args(inner: &mut Inner, approval_mode: Option<&'static str>) -> Vec<Arg> {
    let mut args: Vec<Arg> = STREAM_ARGS.map(Arg::word).into();
    if let Some(mode) = approval_mode {
        args.extend([Arg::word("--permission-mode"), Arg::word(mode)]);
    }
    let session = inner.state.session.clone();
    let picks = inner.picks.clone();
    let claude = inner.claude();
    claude.process_cost = 0.0;
    claude.pick_model = None;
    let relaunch = std::mem::take(&mut claude.relaunch);
    claude.resuming = session.is_some();
    match session {
        Some(session) => args.extend([Arg::word("--resume"), Arg::word(session)]),
        None if relaunch => {}
        None => {
            claude.model = None;
            claude.effort = picks
                .thought_level
                .filter(|level| EFFORT_LEVELS.contains(&level.as_str()));
            claude.pick_model = picks.model.filter(|model| model != DEFAULT);
        }
    }
    if let Some(model) = &claude.model {
        args.extend([Arg::word("--model"), Arg::word(model.clone())]);
    }
    if let Some(effort) = &claude.effort {
        args.extend([Arg::word("--effort"), Arg::word(effort.clone())]);
    }
    args
}

/// Asks Claude Code to initialize: it answers with its models, commands and mode.
pub(super) fn start(inner: &mut Inner) {
    control(
        inner,
        json!({ "subtype": "initialize", "hooks": null }),
        Control::Initialize,
    );
}

fn control(inner: &mut Inner, request: Value, purpose: Control) {
    let claude = inner.claude();
    claude.next_request += 1;
    let id = format!("compi-{}", claude.next_request);
    claude.pending.insert(id.clone(), purpose);
    inner.write(&json!({ "type": "control_request", "request_id": id, "request": request }));
}

fn reply(inner: &mut Inner, request: &str, response: Value) {
    inner.write(&json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request, "response": response },
    }));
}

fn reply_error(inner: &mut Inner, request: &str, error: &str) {
    inner.write(&json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request, "error": error },
    }));
}

/// One message from Claude Code.
pub(super) fn handle(chat: &Arc<Chat>, inner: &mut Inner, message: Value) {
    let parent = message["parent_tool_use_id"].as_str();
    match (message["type"].as_str().unwrap_or_default(), parent) {
        ("control_request", _) => control_request(inner, &message),
        ("control_response", _) => control_response(inner, &message["response"]),
        ("control_cancel_request", _) => {
            cancel_request(inner, message["request_id"].as_str().unwrap_or_default());
        }
        ("system", _) => system(inner, &message),
        // A sub-agent's reply streams too; its card shows its tool calls only.
        ("stream_event", None) => stream_event(inner, &message["event"]),
        ("assistant", None) => assistant(inner, &message["message"]),
        ("assistant", Some(call)) => agent_calls(inner, call, &message["message"]),
        ("user", None) => tool_results(inner, &message),
        ("user", Some(call)) => agent_results(inner, call, &message["message"]),
        ("result", _) => result(chat, inner, &message),
        _ => {}
    }
}

/// A request from Claude Code: permission for a tool call, or the user's answers; Compi
/// offers no others.
fn control_request(inner: &mut Inner, message: &Value) {
    let Some(id) = message["request_id"].as_str() else {
        return;
    };
    let request = &message["request"];
    match request["subtype"].as_str() {
        Some("can_use_tool") => ask(inner, id, request),
        subtype => reply_error(
            inner,
            id,
            &format!("Compi does not offer {}", subtype.unwrap_or("that")),
        ),
    }
}

fn control_response(inner: &mut Inner, response: &Value) {
    let Some(purpose) = response["request_id"]
        .as_str()
        .and_then(|id| inner.claude().pending.remove(id))
    else {
        return;
    };
    let result = match response["subtype"].as_str() {
        Some("error") => Err(response["error"]
            .as_str()
            .unwrap_or("unknown error")
            .to_owned()),
        _ => Ok(&response["response"]),
    };
    let harness = ChatHarness::Claude.name();
    match (purpose, result) {
        (Control::Initialize, Ok(result)) => initialized(inner, result),
        (Control::Initialize, Err(error)) => inner.emit(ChatEvent::Status {
            status: ChatStatus::Failed {
                message: format!("{harness} didn't start: {error}"),
            },
        }),
        (Control::Pick(model), result) => {
            match result {
                Ok(_) => {
                    inner.claude().model = Some(model);
                    publish_options(inner);
                }
                Err(error) => eprintln!(
                    "compi-daemon: chat {} kept its model, not {model}: {error}",
                    inner.chat_id
                ),
            }
            ready(inner);
        }
        (Control::SetModel(model), Ok(_)) => {
            inner.claude().model = (model != DEFAULT).then_some(model);
            publish_options(inner);
        }
        (Control::SetMode(mode), Ok(_)) => set_mode(inner, mode),
        (Control::SetModel(_) | Control::SetMode(_), Err(error)) => inner.emit(ChatEvent::Entry {
            entry: ChatEntry::Error {
                message: format!("{harness} didn't change the setting: {error}"),
            },
        }),
        (Control::ContextUsage, Ok(result)) => {
            let claude = inner.claude();
            if let Some(used) = result["totalTokens"].as_u64() {
                claude.used = used;
            }
            if let Some(size) = result["maxTokens"].as_u64().filter(|size| *size > 0) {
                claude.size = size;
            }
            publish_usage(inner);
        }
        (Control::Interrupt | Control::ContextUsage, Err(_)) | (Control::Interrupt, Ok(_)) => {}
    }
}

/// Claude Code answered `initialize`: its models, commands and permission mode. A new
/// session gets the user's model; then the chat is ready.
fn initialized(inner: &mut Inner, result: &Value) {
    inner.initialized = true;
    let claude = inner.claude();
    claude.models = models(&result["models"]);
    if let Some(mode) = result["current_permission_mode"].as_str() {
        claude.mode = Some(mode.to_owned());
    }
    claude.config = result["user_output_styles_dir"]
        .as_str()
        .and_then(parent_folder)
        .map(str::to_owned);
    let current = claude.model.as_deref().unwrap_or(DEFAULT);
    let pick = claude.pick_model.take().filter(|model| {
        model != current && claude.models.iter().any(|offered| offered.value == *model)
    });
    let mode = claude.mode.clone();
    let commands = commands(&result["commands"]);
    if commands != inner.state.commands {
        inner.emit(ChatEvent::Commands { commands });
    }
    publish_options(inner);
    if mode.is_some() && inner.state.approval_mode != mode {
        inner.emit(ChatEvent::ApprovalMode { mode });
    }
    match pick {
        Some(model) => control(
            inner,
            json!({ "subtype": "set_model", "model": model }),
            Control::Pick(model),
        ),
        None => ready(inner),
    }
}

fn ready(inner: &mut Inner) {
    inner.emit(ChatEvent::Status {
        status: ChatStatus::Ready,
    });
    inner.send_queued();
}

/// The folder holding `path`, a Windows or a Linux path.
fn parent_folder(path: &str) -> Option<&str> {
    let path = path.trim_end_matches(['/', '\\']);
    path.rfind(['/', '\\'])
        .map(|end| &path[..end])
        .filter(|parent| !parent.is_empty())
}

/// The models `initialize` lists.
fn models(value: &Value) -> Vec<Model> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let value = model["value"].as_str()?.to_owned();
            Some(Model {
                name: model["displayName"].as_str().unwrap_or(&value).to_owned(),
                description: model["description"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
                efforts: if model["supportsEffort"].as_bool() == Some(true) {
                    model["supportedEffortLevels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                } else {
                    Vec::new()
                },
                value,
            })
        })
        .collect()
}

/// Slash commands, as `initialize` and `commands_changed` list them.
fn commands(value: &Value) -> Vec<ChatCommand> {
    value
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
                hint: command["argumentHint"]
                    .as_str()
                    .filter(|hint| !hint.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect()
}

/// The session's settings: the model, the thinking level when the model takes one, and
/// the permission mode.
fn options(claude: &Claude) -> Vec<ChatOption> {
    let mut options = Vec::new();
    let model = claude.model.as_deref().unwrap_or(DEFAULT);
    if !claude.models.is_empty() {
        options.push(ChatOption {
            id: "model".into(),
            name: "Model".into(),
            category: Some("model".into()),
            current: model.to_owned(),
            choices: claude
                .models
                .iter()
                .map(|offered| ChatChoice {
                    value: offered.value.clone(),
                    name: offered.name.clone(),
                    description: offered.description.clone(),
                    group: None,
                })
                .collect(),
        });
    }
    if let Some(offered) = claude
        .models
        .iter()
        .find(|offered| offered.value == model)
        .filter(|offered| !offered.efforts.is_empty())
    {
        let level = |value: &str, name: &str| ChatChoice {
            value: value.to_owned(),
            name: name.to_owned(),
            description: None,
            group: None,
        };
        options.push(ChatOption {
            id: "thought_level".into(),
            name: "Thinking".into(),
            category: Some("thought_level".into()),
            current: claude.effort.as_deref().unwrap_or(DEFAULT).to_owned(),
            choices: std::iter::once(level(DEFAULT, "Default"))
                .chain(offered.efforts.iter().map(|effort| level(effort, effort)))
                .collect(),
        });
    }
    if let Some(mode) = &claude.mode {
        let mut choices: Vec<ChatChoice> = MODES
            .iter()
            .map(|(value, name)| ChatChoice {
                value: (*value).to_owned(),
                name: (*name).to_owned(),
                description: None,
                group: None,
            })
            .collect();
        // A mode Compi does not offer (`auto`, `dontAsk`) still shows as current.
        if !choices.iter().any(|choice| choice.value == *mode) {
            choices.push(ChatChoice {
                value: mode.clone(),
                name: mode.clone(),
                description: None,
                group: None,
            });
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
    let options = options(inner.claude_state());
    if options != inner.state.options {
        inner.emit(ChatEvent::Options { options });
    }
}

/// Claude Code's permission mode is now `mode`: the mode setting and the approval mode
/// show it.
fn set_mode(inner: &mut Inner, mode: String) {
    inner.claude().mode = Some(mode.clone());
    publish_options(inner);
    if inner.state.approval_mode.as_deref() != Some(mode.as_str()) {
        inner.emit(ChatEvent::ApprovalMode { mode: Some(mode) });
    }
}

/// The context use and cost, once the context window is known.
fn publish_usage(inner: &mut Inner) {
    let claude = inner.claude_state();
    if claude.size == 0 {
        return;
    }
    let usage = ChatUsage {
        used: claude.used,
        size: claude.size,
        cost: Some(claude.cost),
        currency: Some("USD".into()),
    };
    if inner.state.usage.as_ref() != Some(&usage) {
        inner.emit(ChatEvent::Usage { usage: Some(usage) });
    }
}

/// Starts a turn on `prompt`: its text, its files named by path (`@path`), its images.
pub(super) fn prompt(inner: &mut Inner, prompt: &Prompt) {
    inner.claude().interrupting = false;
    inner.write(&json!({
        "type": "user",
        "message": { "role": "user", "content": prompt_content(prompt) },
        "parent_tool_use_id": null,
    }));
}

fn prompt_content(prompt: &Prompt) -> Vec<Value> {
    let mut text = prompt.text.clone();
    let mentions: Vec<String> = prompt
        .files
        .iter()
        .map(|path| {
            if path.contains(char::is_whitespace) {
                format!("@\"{path}\"")
            } else {
                format!("@{path}")
            }
        })
        .collect();
    if !mentions.is_empty() {
        if !text.trim().is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&mentions.join(" "));
    }
    let mut content = Vec::new();
    if !text.trim().is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    content.extend(prompt.images.iter().map(|image| {
        json!({
            "type": "image",
            "source": { "type": "base64", "media_type": image.mime, "data": image.data },
        })
    }));
    content
}

/// Stops the running turn: what Claude Code waits on is declined.
pub(super) fn cancel(inner: &mut Inner) {
    inner.claude().interrupting = true;
    inner.cancel_requests();
    interrupt(inner);
}

/// Asks Claude Code to stop the running turn.
pub(super) fn interrupt(inner: &mut Inner) {
    control(inner, json!({ "subtype": "interrupt" }), Control::Interrupt);
}

/// Declines every permission and question Claude Code waits on.
pub(super) fn cancel_asks(inner: &mut Inner) {
    let asks: Vec<Ask> = inner.claude().asks.drain(..).collect();
    for ask in asks {
        reply(
            inner,
            &ask.request,
            json!({ "behavior": "deny", "message": "The user stopped the turn." }),
        );
    }
}

pub(super) fn forget_asks(inner: &mut Inner) {
    inner.claude().asks.clear();
}

pub(super) fn fail_pending(inner: &mut Inner) {
    inner.claude().pending.clear();
}

/// Claude Code asks permission for a tool call, or asks the user questions.
fn ask(inner: &mut Inner, request_id: &str, request: &Value) {
    let tool = request["tool_name"].as_str().unwrap_or_default();
    let input = request["input"].clone();
    let call = request["tool_use_id"].as_str();
    let kind = if tool == "AskUserQuestion" {
        inner.next_question += 1;
        let (shown, questions) = question(inner.next_question, &input);
        AskKind::Question { shown, questions }
    } else {
        // The card sits on its tool call, waiting.
        if let Some(call) = call {
            let mut waiting = inner.tool_call(call);
            if waiting.name.is_none() {
                describe(&mut waiting, tool, &input);
            }
            if waiting.status.is_open() {
                waiting.status = ChatToolStatus::Pending;
            }
            inner.put_tool(waiting);
        }
        let plan = tool == "ExitPlanMode";
        let suggestions = request["permission_suggestions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        inner.next_permission += 1;
        let title = request["title"]
            .as_str()
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if plan {
                    "Start on this plan?".to_owned()
                } else {
                    tool_title(tool, &input)
                }
            });
        let shown = ChatPermission {
            id: inner.next_permission,
            tool: call.map(str::to_owned),
            title,
            options: permission_options(plan, !suggestions.is_empty()),
        };
        AskKind::Permission {
            shown,
            suggestions,
            plan,
        }
    };
    inner.claude().asks.push_back(Ask {
        request: request_id.to_owned(),
        input,
        kind,
    });
    show_ask(inner);
}

/// Shows the first permission or question Claude Code waits on, unless it shows.
fn show_ask(inner: &mut Inner) {
    let Some(event) = inner.claude().asks.front().map(|ask| ask.kind.event()) else {
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

fn permission_options(plan: bool, always: bool) -> Vec<ChatPermissionOption> {
    let option = |id: &str, name: &str, kind: &str| ChatPermissionOption {
        id: id.to_owned(),
        name: name.to_owned(),
        kind: kind.to_owned(),
    };
    if plan {
        return vec![
            option("accept_edits", "Yes, and accept edits", "allow_always"),
            option("allow", "Yes, and ask before edits", "allow_once"),
            option("reject", "No, keep planning", "reject_once"),
        ];
    }
    let mut options = vec![option("allow", "Allow once", "allow_once")];
    if always {
        options.push(option("always", "Always allow", "allow_always"));
    }
    options.push(option("reject", "Reject", "reject_once"));
    options
}

/// `AskUserQuestion`'s questions as a form: a field per question, its options as
/// choices, answerable in words too.
fn question(id: u64, input: &Value) -> (ChatQuestion, Vec<String>) {
    let mut fields = Vec::new();
    let mut questions = Vec::new();
    let mut headers = Vec::new();
    for item in input["questions"].as_array().into_iter().flatten() {
        let Some(text) = item["question"].as_str() else {
            continue;
        };
        let choices: Vec<ChatChoice> = item["options"]
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
        if let Some(header) = item["header"].as_str().filter(|header| !header.is_empty()) {
            headers.push(header);
        }
        fields.push(ChatField {
            key: questions.len().to_string(),
            title: text.to_owned(),
            other: !choices.is_empty(),
            choices,
            multi: item["multiSelect"].as_bool() == Some(true),
            required: true,
        });
        questions.push(text.to_owned());
    }
    let message = if headers.is_empty() {
        format!("{} asks", ChatHarness::Claude.name())
    } else {
        headers.join(" · ")
    };
    (
        ChatQuestion {
            id,
            message,
            fields,
        },
        questions,
    )
}

/// Answers the permission that shows with `option`; `None` declines it.
pub(super) fn answer(inner: &mut Inner, option: Option<String>) -> Result<(), String> {
    let Some(Ask {
        kind: AskKind::Permission { plan, .. },
        ..
    }) = inner.claude().asks.front()
    else {
        return Err("that permission was already answered".into());
    };
    let plan = *plan;
    let known = match option.as_deref() {
        None | Some("allow" | "reject") => true,
        Some("always") => !plan,
        Some("accept_edits") => plan,
        Some(_) => false,
    };
    if !known {
        return Err(format!(
            "{} is not an answer to this permission",
            option.unwrap_or_default()
        ));
    }
    let Some(Ask {
        request,
        input,
        kind: AskKind::Permission {
            shown, suggestions, ..
        },
    }) = inner.claude().asks.pop_front()
    else {
        unreachable!("the first ask is this permission");
    };
    let set_mode_to =
        |mode: &str| json!([{ "type": "setMode", "mode": mode, "destination": "session" }]);
    let (permissions, mode) = match option.as_deref() {
        Some("accept_edits") => (Some(set_mode_to("acceptEdits")), Some("acceptEdits")),
        Some("allow") if plan => (Some(set_mode_to("default")), Some("default")),
        Some("allow") => (None, None),
        Some("always") => {
            let mode = suggestions
                .iter()
                .rev()
                .find(|suggestion| suggestion["type"] == "setMode")
                .and_then(|suggestion| suggestion["mode"].as_str());
            (Some(Value::Array(suggestions.clone())), mode)
        }
        _ => {
            reply(
                inner,
                &request,
                json!({ "behavior": "deny", "message": "The user declined this tool call." }),
            );
            inner.emit(ChatEvent::Permission { permission: None });
            show_ask(inner);
            return Ok(());
        }
    };
    let mut response = json!({ "behavior": "allow", "updatedInput": input });
    if let Some(permissions) = permissions {
        response["updatedPermissions"] = permissions;
    }
    reply(inner, &request, response);
    inner.emit(ChatEvent::Permission { permission: None });
    if let Some(call) = &shown.tool
        && inner.tools.contains_key(call)
    {
        let mut running = inner.tool_call(call);
        if running.status == ChatToolStatus::Pending {
            running.status = ChatToolStatus::Running;
            inner.put_tool(running);
        }
    }
    if let Some(mode) = mode {
        set_mode(inner, mode.to_owned());
    }
    show_ask(inner);
    Ok(())
}

/// Answers the questions that show, as `AskUserQuestion` takes answers: the question's
/// text to the picked labels, or the words typed; `None` declines them.
pub(super) fn answer_question(
    inner: &mut Inner,
    answers: Option<ChatAnswers>,
) -> Result<(), String> {
    if !matches!(
        inner.claude().asks.front(),
        Some(Ask {
            kind: AskKind::Question { .. },
            ..
        })
    ) {
        return Err("that question was already answered".into());
    }
    let Some(Ask {
        request,
        mut input,
        kind: AskKind::Question { questions, .. },
    }) = inner.claude().asks.pop_front()
    else {
        unreachable!("the first ask is this question");
    };
    let response = match answers {
        Some(answers) => {
            let mut given = Map::new();
            for (index, question) in questions.iter().enumerate() {
                let text = match answers.get(&index.to_string()) {
                    Some(ChatAnswer::Choices { values }) => values.join(", "),
                    Some(ChatAnswer::Text { text }) => text.clone(),
                    None => continue,
                };
                given.insert(question.clone(), Value::String(text));
            }
            input["answers"] = Value::Object(given);
            json!({ "behavior": "allow", "updatedInput": input })
        }
        None => json!({ "behavior": "deny", "message": "The user declined to answer." }),
    };
    reply(inner, &request, response);
    inner.emit(ChatEvent::Question { question: None });
    show_ask(inner);
    Ok(())
}

/// Claude Code no longer waits on request `id`.
fn cancel_request(inner: &mut Inner, id: &str) {
    let claude = inner.claude();
    let Some(position) = claude.asks.iter().position(|ask| ask.request == id) else {
        return;
    };
    let gone = claude.asks.remove(position).expect("found");
    if position == 0 {
        match gone.kind {
            AskKind::Permission { .. } => inner.emit(ChatEvent::Permission { permission: None }),
            AskKind::Question { .. } => inner.emit(ChatEvent::Question { question: None }),
        }
        show_ask(inner);
    }
}

/// Sets `option` to `value`, one of its choices: the model and the mode at once, the
/// thinking level by starting Claude Code again on the session.
pub(super) fn set_option(
    chat: &Arc<Chat>,
    inner: &mut Inner,
    option: String,
    value: String,
) -> Result<(), String> {
    match option.as_str() {
        "model" => control(
            inner,
            json!({ "subtype": "set_model", "model": value }),
            Control::SetModel(value),
        ),
        "mode" => control(
            inner,
            json!({ "subtype": "set_permission_mode", "mode": value }),
            Control::SetMode(value),
        ),
        "thought_level" => {
            if inner.state.working() {
                return Err(format!(
                    "{} changes its thinking level between turns: stop the running turn first",
                    ChatHarness::Claude.name()
                ));
            }
            let claude = inner.claude();
            claude.effort = (value != DEFAULT).then_some(value);
            claude.relaunch = true;
            publish_options(inner);
            inner.stop();
            inner.emit(ChatEvent::Status {
                status: ChatStatus::Starting,
            });
            chat.spawn(inner);
        }
        _ => return Err(format!("the harness has no setting {option}")),
    }
    Ok(())
}

/// Lists Claude Code's sessions in the chat's folder, from its files, on a thread of its
/// own.
pub(super) fn list_sessions(
    chat: &Chat,
    inner: &Inner,
) -> Result<mpsc::Receiver<Result<Vec<ChatSessionInfo>, String>>, String> {
    let claude = inner.claude_state();
    let harness = ChatHarness::Claude.name();
    let config = claude
        .config
        .as_deref()
        .ok_or_else(|| format!("{harness} didn't say where it keeps its sessions"))?;
    let folder = store::sessions_folder(&chat.content, config, claude.cwd.as_deref())
        .ok_or_else(|| format!("{harness}'s sessions can't be read from here"))?;
    let (reply, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("compi-chat-sessions".into())
        .spawn(move || {
            let _ = reply.send(Ok(store::list(&folder)));
        })
        .map_err(|error| format!("Couldn't list {harness}'s sessions: {error}"))?;
    Ok(receiver)
}

fn system(inner: &mut Inner, message: &Value) {
    match message["subtype"].as_str() {
        Some("init") => init(inner, message),
        Some("commands_changed") => {
            let commands = commands(&message["commands"]);
            if commands != inner.state.commands {
                inner.emit(ChatEvent::Commands { commands });
            }
        }
        Some("status") => {
            if let Some(mode) = message["permissionMode"].as_str()
                && inner.claude().mode.as_deref() != Some(mode)
            {
                set_mode(inner, mode.to_owned());
            }
        }
        Some("task_started") => task_started(inner, message),
        Some("task_progress") => task_progress(inner, message),
        Some("task_notification") => task_ended(
            inner,
            message["task_id"].as_str().unwrap_or_default(),
            message["status"].as_str().unwrap_or_default(),
            message["summary"].as_str(),
            &message["usage"],
        ),
        Some("task_updated") => task_ended(
            inner,
            message["task_id"].as_str().unwrap_or_default(),
            message["patch"]["status"].as_str().unwrap_or_default(),
            None,
            &Value::Null,
        ),
        _ => {}
    }
}

/// A turn begins (`system/init`): the session, saved now, and the folder as Claude Code
/// names it. A turn Claude Code begins by itself, a background task's end waking it,
/// shows as working.
fn init(inner: &mut Inner, message: &Value) {
    let claude = inner.claude();
    if let Some(cwd) = message["cwd"].as_str() {
        claude.cwd = Some(cwd.to_owned());
    }
    // The session is saved from its first prompt on: a start after this resumes it.
    if let Some(session) = message["session_id"].as_str()
        && inner.state.session.as_deref() != Some(session)
    {
        inner.emit(ChatEvent::Session {
            session: Some(session.to_owned()),
        });
        inner.save_thread(false);
    }
    if let Some(mode) = message["permissionMode"].as_str()
        && inner.claude().mode.as_deref() != Some(mode)
    {
        set_mode(inner, mode.to_owned());
    }
    if inner.initialized && inner.state.status == ChatStatus::Ready {
        let since_ms = now_ms();
        inner.unseen = None;
        inner.activity_ms = since_ms;
        inner.emit(ChatEvent::Status {
            status: ChatStatus::Working { since_ms },
        });
    }
}

/// One event of the reply being streamed: a block begins, or text comes for it.
fn stream_event(inner: &mut Inner, event: &Value) {
    let index = event["index"].as_u64().unwrap_or_default();
    match event["type"].as_str() {
        Some("message_start") => {
            inner.claude().stream = Stream {
                message: event["message"]["id"].as_str().map(str::to_owned),
                blocks: Vec::new(),
            };
        }
        Some("content_block_start") => {
            let block = &event["content_block"];
            let kind = match block["type"].as_str() {
                Some("text") => BlockKind::Text,
                Some("thinking") => BlockKind::Thinking,
                Some("tool_use") => BlockKind::Tool,
                _ => return,
            };
            if kind != BlockKind::Thinking {
                inner.end_thought();
            }
            if kind == BlockKind::Tool
                && let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str())
            {
                let mut tool = inner.tool_call(id);
                describe(&mut tool, name, &Value::Null);
                inner.put_tool(tool);
                inner.activity_ms = now_ms();
            }
            inner.claude().stream.blocks.push(Block {
                index,
                kind,
                entry: None,
                text: String::new(),
                started_ms: now_ms(),
                reconciled: false,
            });
        }
        Some("content_block_delta") => {
            let delta = &event["delta"];
            let text = match delta["type"].as_str() {
                Some("text_delta") => delta["text"].as_str(),
                Some("thinking_delta") => delta["thinking"].as_str(),
                _ => None,
            };
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                stream_text(inner, index, text);
            }
        }
        _ => {}
    }
}

/// Streamed text for block `index`: onto its entry, else the last entry it continues,
/// else a new one.
fn stream_text(inner: &mut Inner, index: u64, text: &str) {
    let Some(position) = inner
        .claude()
        .stream
        .blocks
        .iter()
        .position(|block| block.index == index)
    else {
        return;
    };
    let block = &inner.claude().stream.blocks[position];
    let (kind, entry, started_ms) = (block.kind, block.entry, block.started_ms);
    let entry = match entry {
        Some((entry, start)) => {
            inner.emit(ChatEvent::Text {
                index: entry,
                text: text.to_owned(),
            });
            (entry, start)
        }
        None => {
            let start = append(inner, kind, text.to_owned(), started_ms);
            (inner.state.entries.len() - 1, start)
        }
    };
    if kind == BlockKind::Text {
        inner.activity_ms = now_ms();
    }
    let block = &mut inner.claude().stream.blocks[position];
    block.entry = Some(entry);
    block.text.push_str(text);
}

/// Adds `text` of `kind` to the last entry when it continues it, else as a new entry;
/// where in the entry's text it begins.
fn append(inner: &mut Inner, kind: BlockKind, text: String, started_ms: u64) -> usize {
    let start = match (kind, inner.state.entries.last()) {
        (BlockKind::Text, Some(ChatEntry::Agent { text })) => text.len(),
        (
            BlockKind::Thinking,
            Some(ChatEntry::Thought {
                text,
                ended_ms: None,
                ..
            }),
        ) => text.len(),
        _ => 0,
    };
    match kind {
        BlockKind::Thinking => inner.append(
            text,
            |text| ChatEntry::Thought {
                text,
                started_ms,
                ended_ms: None,
            },
            |entry| matches!(entry, ChatEntry::Thought { ended_ms: None, .. }),
        ),
        _ => {
            inner.end_thought();
            inner.append(
                text,
                |text| ChatEntry::Agent { text },
                |entry| matches!(entry, ChatEntry::Agent { .. }),
            );
        }
    }
    start
}

/// A whole reply message: its text and thinking replace what was streamed of them, its
/// tool calls get their input, and its usage says how full the context is.
fn assistant(inner: &mut Inner, message: &Value) {
    let streamed = inner.claude().stream.message.as_deref() == message["id"].as_str()
        && message["id"].is_string();
    for block in message["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => whole_text(inner, streamed, BlockKind::Text, block["text"].as_str()),
            Some("thinking") => whole_text(
                inner,
                streamed,
                BlockKind::Thinking,
                block["thinking"].as_str(),
            ),
            Some("tool_use") => {
                let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) else {
                    continue;
                };
                inner.end_thought();
                let mut tool = inner.tool_call(id);
                describe(&mut tool, name, &block["input"]);
                if tool.status == ChatToolStatus::Pending {
                    tool.status = ChatToolStatus::Running;
                }
                inner.put_tool(tool);
                inner.activity_ms = now_ms();
                if let Some(streamed) = inner
                    .claude()
                    .stream
                    .blocks
                    .iter_mut()
                    .find(|block| block.kind == BlockKind::Tool && !block.reconciled)
                {
                    streamed.reconciled = true;
                }
            }
            _ => {}
        }
    }
    let usage = &message["usage"];
    if usage.is_object() {
        let tokens = |key: &str| usage[key].as_u64().unwrap_or_default();
        let claude = inner.claude();
        claude.used = tokens("input_tokens")
            + tokens("cache_creation_input_tokens")
            + tokens("cache_read_input_tokens")
            + tokens("output_tokens");
        claude.reply_model = message["model"].as_str().map(str::to_owned);
        publish_usage(inner);
    }
}

/// The whole text of a reply or thinking block: it replaces what was streamed of the
/// same block, or shows as streamed text would.
fn whole_text(inner: &mut Inner, streamed: bool, kind: BlockKind, text: Option<&str>) {
    let text = text.unwrap_or_default();
    let found = if streamed {
        inner
            .claude()
            .stream
            .blocks
            .iter_mut()
            .find(|block| block.kind == kind && !block.reconciled)
            .map(|block| {
                block.reconciled = true;
                (
                    block.entry,
                    std::mem::take(&mut block.text),
                    block.started_ms,
                )
            })
    } else {
        None
    };
    let started_ms = match found {
        Some((Some((entry, start)), shown, _)) => {
            if shown != text {
                replace_text(inner, entry, start, shown.len(), text);
            }
            return;
        }
        Some((None, _, started_ms)) => started_ms,
        None => match inner.activity_ms {
            0 => now_ms(),
            since => since,
        },
    };
    if !text.is_empty() {
        append(inner, kind, text.to_owned(), started_ms);
        if kind == BlockKind::Text {
            inner.activity_ms = now_ms();
        }
    }
}

/// Puts `text` in place of the `length` bytes from `start` of entry `index`'s text.
fn replace_text(inner: &mut Inner, index: usize, start: usize, length: usize, text: &str) {
    let replace = |shown: &str| -> Option<String> {
        let end = start.checked_add(length)?;
        Some(format!(
            "{}{text}{}",
            shown.get(..start)?,
            shown.get(end..)?
        ))
    };
    let entry = match inner.state.entries.get(index) {
        Some(ChatEntry::Agent { text: shown }) => {
            replace(shown).map(|text| ChatEntry::Agent { text })
        }
        Some(ChatEntry::Thought {
            text: shown,
            started_ms,
            ended_ms,
        }) => replace(shown).map(|text| ChatEntry::Thought {
            text,
            started_ms: *started_ms,
            ended_ms: *ended_ms,
        }),
        _ => None,
    };
    if let Some(entry) = entry {
        inner.emit(ChatEvent::Replace { index, entry });
    }
}

/// A user message's tool results: each ends its call.
fn tool_results(inner: &mut Inner, message: &Value) {
    let results: Vec<&Value> = message["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "tool_result")
        .collect();
    // What the tool reported besides, when the message answers one call.
    let reported = (results.len() == 1).then_some(&message["tool_use_result"]);
    let interrupting = inner.claude().interrupting;
    for block in results {
        let Some(id) = block["tool_use_id"].as_str() else {
            continue;
        };
        let now = now_ms();
        let mut tool = inner.tool_call(id);
        let was = tool.status;
        let failed = block["is_error"].as_bool() == Some(true);
        tool.status = match (failed, interrupting) {
            (false, _) => ChatToolStatus::Completed,
            (true, false) => ChatToolStatus::Failed,
            (true, true) => ChatToolStatus::Stopped,
        };
        tool.ended_ms.get_or_insert(now);
        let output = result_text(&block["content"]);
        if let Some(output) = &output {
            tool.output = Some(tail(output.clone(), MAX_CHAT_TOOL_OUTPUT_BYTES));
        }
        if let Some(reported) = reported {
            // A file a Write replaced: its old text.
            if let ([diff], Some(original)) =
                (tool.diffs.as_mut_slice(), reported["originalFile"].as_str())
                && tool.name.as_deref() == Some("write")
                && original.len() <= MAX_CHAT_DIFF_BYTES
            {
                diff.old_text = Some(original.to_owned());
            }
            // An agent that ran in the foreground ends with its call.
            if reported["isAsync"].as_bool() != Some(true) {
                for agent in tool
                    .agents
                    .iter_mut()
                    .filter(|agent| agent.status.is_open())
                {
                    agent.status = tool.status;
                    if let Some(output) = output
                        .as_deref()
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                    {
                        agent.result = Some(head(output.to_owned(), MAX_CHAT_AGENT_RESULT_BYTES));
                    }
                }
            }
        }
        settle(&mut tool, was, now);
        inner.put_tool(tool);
    }
}

/// A tool result's text: its text blocks, an image noted as one.
fn result_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter_map(|block| match block["type"].as_str() {
                    Some("text") => block["text"].as_str(),
                    Some("image") => Some("[image]"),
                    _ => None,
                })
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    }
}

/// A sub-agent's reply: each tool call it makes becomes a step of its card.
fn agent_calls(inner: &mut Inner, call: &str, message: &Value) {
    if !inner.tools.contains_key(call) {
        return;
    }
    for block in message["content"].as_array().into_iter().flatten() {
        let (Some("tool_use"), Some(id)) = (block["type"].as_str(), block["id"].as_str()) else {
            continue;
        };
        let Some(text) = step_line(block["name"].as_str().unwrap_or("Tool"), &block["input"])
        else {
            continue;
        };
        let mut tool = inner.tool_call(call);
        let Some(agent) = tool.agents.last_mut() else {
            return;
        };
        let ids = inner.claude().steps.entry(call.to_owned()).or_default();
        if agent.steps.len() >= MAX_CHAT_AGENT_STEPS {
            agent.steps.remove(0);
            ids.pop_front();
        }
        ids.push_back(id.to_owned());
        agent.steps.push(ChatAgentStep {
            text: text.clone(),
            status: ChatToolStatus::Running,
        });
        agent.tool_count += 1;
        if agent.status.is_open() {
            agent.activity = Some(text);
        }
        inner.put_tool(tool);
    }
}

/// A sub-agent's tool results: each ends its step.
fn agent_results(inner: &mut Inner, call: &str, message: &Value) {
    if !inner.tools.contains_key(call) {
        return;
    }
    for block in message["content"].as_array().into_iter().flatten() {
        let (Some("tool_result"), Some(id)) =
            (block["type"].as_str(), block["tool_use_id"].as_str())
        else {
            continue;
        };
        let Some(step) = inner
            .claude()
            .steps
            .get(call)
            .and_then(|ids| ids.iter().position(|step| step == id))
        else {
            continue;
        };
        let mut tool = inner.tool_call(call);
        let Some(shown) = tool
            .agents
            .last_mut()
            .and_then(|agent| agent.steps.get_mut(step))
        else {
            continue;
        };
        shown.status = if block["is_error"].as_bool() == Some(true) {
            ChatToolStatus::Failed
        } else {
            ChatToolStatus::Completed
        };
        inner.put_tool(tool);
    }
}

/// A sub-agent began on an Agent call: the call's card shows it.
fn task_started(inner: &mut Inner, message: &Value) {
    let (Some(task), Some(call)) = (message["task_id"].as_str(), message["tool_use_id"].as_str())
    else {
        return;
    };
    // Background commands are tasks too; only agents get a card.
    if !message["task_type"]
        .as_str()
        .is_some_and(|kind| kind.ends_with("agent"))
        || !inner.tools.contains_key(call)
    {
        return;
    }
    let description = message["description"].as_str().unwrap_or_default().trim();
    let name = agent_name(
        inner,
        if description.is_empty() {
            task
        } else {
            description
        },
    );
    let now = now_ms();
    let mut agent = new_agent(&name);
    agent.agent = message["subagent_type"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    agent.status = ChatToolStatus::Running;
    agent.activity = short_line(description);
    agent.started_ms = Some(now);
    agent.assignment = message["prompt"]
        .as_str()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .map(|prompt| head(prompt.to_owned(), MAX_CHAT_TOOL_INPUT_BYTES));
    let mut tool = inner.tool_call(call);
    let was = tool.status;
    tool.agents.push(agent);
    settle(&mut tool, was, now);
    inner.put_tool(tool);
    inner.claude().tasks.insert(
        task.to_owned(),
        Task {
            call: call.to_owned(),
            agent: name,
        },
    );
}

/// `name`, else with a number, so no other sub-agent in the transcript has it.
fn agent_name(inner: &Inner, name: &str) -> String {
    let taken = |candidate: &str| {
        inner.state.entries.iter().any(|entry| match entry {
            ChatEntry::Tool(tool) => tool.agents.iter().any(|agent| agent.id == candidate),
            _ => false,
        })
    };
    let name = short_line(name).unwrap_or_else(|| "Agent".to_owned());
    if !taken(&name) {
        return name;
    }
    (2..)
        .map(|number| format!("{name} ({number})"))
        .find(|candidate| !taken(candidate))
        .expect("some number is free")
}

/// A sub-agent's progress: what it does now, its tool calls and time.
fn task_progress(inner: &mut Inner, message: &Value) {
    let Some(Task { call, agent }) = message["task_id"]
        .as_str()
        .and_then(|task| inner.claude().tasks.get(task).cloned())
    else {
        return;
    };
    let mut tool = inner.tool_call(&call);
    let Some(shown) = tool.agents.iter_mut().find(|shown| shown.id == agent) else {
        return;
    };
    if !shown.status.is_open() {
        return;
    }
    if let Some(activity) = message["description"].as_str().and_then(short_line) {
        shown.activity = Some(activity);
    }
    task_usage(shown, &message["usage"]);
    inner.put_tool(tool);
}

fn task_usage(agent: &mut compi_protocol::chat::ChatAgent, usage: &Value) {
    if let Some(count) = usage["tool_uses"].as_u64() {
        agent.tool_count = agent.tool_count.max(count);
    }
    if let Some(duration) = usage["duration_ms"].as_u64() {
        agent.duration_ms = duration;
    }
}

/// A sub-agent ended (`status` `completed`, `failed`, `stopped` or `killed`), with what
/// it answered when it says.
fn task_ended(inner: &mut Inner, task: &str, status: &str, summary: Option<&str>, usage: &Value) {
    let status = match status {
        "completed" => ChatToolStatus::Completed,
        "failed" => ChatToolStatus::Failed,
        "stopped" | "killed" => ChatToolStatus::Stopped,
        _ => return,
    };
    let Some(Task { call, agent }) = inner.claude().tasks.get(task).cloned() else {
        return;
    };
    let now = now_ms();
    let mut tool = inner.tool_call(&call);
    let was = tool.status;
    let Some(shown) = tool.agents.iter_mut().find(|shown| shown.id == agent) else {
        return;
    };
    if shown.status.is_open() {
        shown.status = status;
    }
    if let Some(summary) = summary.map(str::trim).filter(|text| !text.is_empty()) {
        shown.result = Some(head(summary.to_owned(), MAX_CHAT_AGENT_RESULT_BYTES));
    }
    task_usage(shown, usage);
    if shown.duration_ms == 0
        && let Some(started) = shown.started_ms
    {
        shown.duration_ms = now.saturating_sub(started);
    }
    settle(&mut tool, was, now);
    inner.put_tool(tool);
}

/// The turn ended (`result`): its cost and context use, and how it ended. A `result`
/// before Claude Code initialized is a session it could not resume.
fn result(chat: &Arc<Chat>, inner: &mut Inner, message: &Value) {
    let initialized = inner.initialized;
    let claude = inner.claude();
    claude.stream = Stream::default();
    if claude.resuming && !initialized {
        resume_failed(chat, inner, &result_error(message));
        return;
    }
    let interrupting = std::mem::take(&mut claude.interrupting);
    if let Some(total) = message["total_cost_usd"]
        .as_f64()
        .filter(|total| *total >= 0.0)
    {
        // The process's running total; a cleared conversation starts it over.
        claude.cost += if total >= claude.process_cost {
            total - claude.process_cost
        } else {
            total
        };
        claude.process_cost = total;
    }
    if let Some(size) = context_window(&message["modelUsage"], claude.reply_model.as_deref()) {
        claude.size = size;
    }
    publish_usage(inner);
    if inner.agent.is_some() {
        control(
            inner,
            json!({ "subtype": "get_context_usage" }),
            Control::ContextUsage,
        );
    }
    if inner.state.working() {
        inner.prompt_ended(turn_end(message, interrupting));
    }
}

/// The context window of the model that replied last, else the largest the turn used.
fn context_window(usage: &Value, model: Option<&str>) -> Option<u64> {
    let usage = usage.as_object()?;
    let window = |entry: &Value| entry["contextWindow"].as_u64().filter(|size| *size > 0);
    model
        .and_then(|model| usage.get(model))
        .and_then(window)
        .or_else(|| usage.values().filter_map(window).max())
}

/// How a turn ended, as the chat's turns end (`stopReason`), or its error.
fn turn_end(message: &Value, interrupting: bool) -> Result<Value, String> {
    let stop = |reason: &str| Ok(json!({ "stopReason": reason }));
    let terminal = message["terminal_reason"].as_str().unwrap_or_default();
    if interrupting || terminal.starts_with("aborted") {
        return stop("cancelled");
    }
    if message["subtype"] == "error_max_turns" {
        return stop("max_turn_requests");
    }
    if message["is_error"].as_bool() == Some(true) {
        return Err(result_error(message));
    }
    match message["stop_reason"].as_str() {
        Some("max_tokens") => stop("max_tokens"),
        Some("refusal") => stop("refusal"),
        _ => stop("end_turn"),
    }
}

/// What went wrong, from an error `result`: its errors, else its text, else its kind.
fn result_error(message: &Value) -> String {
    let errors: Vec<&str> = message["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|error| !error.is_empty() && !error.starts_with("[ede_diagnostic]"))
        .collect();
    if !errors.is_empty() {
        return errors.join("; ");
    }
    if let Some(text) = message["result"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return text.to_owned();
    }
    if let Some(kind) = message["subtype"]
        .as_str()
        .filter(|kind| !kind.is_empty() && *kind != "success")
    {
        return kind.to_owned();
    }
    match message["api_error_status"].as_u64() {
        Some(status) => format!("API error (HTTP {status})"),
        None => "unknown error".to_owned(),
    }
}

/// Claude Code could not resume the chat's session: the transcript stays, above the error
/// and a new session.
fn resume_failed(chat: &Arc<Chat>, inner: &mut Inner, error: &str) {
    inner.emit(ChatEvent::Session { session: None });
    inner.emit(ChatEvent::Entry {
        entry: ChatEntry::Error {
            message: format!("Couldn't resume the earlier session: {error}"),
        },
    });
    inner.save_thread(true);
    inner.stop();
    chat.spawn(inner);
}

/// Fills in a Claude Code tool call as the transcript shows it: its name, kind, title,
/// input, the files it works on, and an edit's changes.
fn describe(tool: &mut ChatTool, name: &str, input: &Value) {
    tool.name = Some(match name {
        "Task" | "Agent" => "task".to_owned(),
        other => other.to_ascii_lowercase(),
    });
    tool.kind = match name {
        "Bash" | "BashOutput" | "KillShell" | "KillBash" | "PowerShell" => "execute",
        "Read" | "NotebookRead" => "read",
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => "edit",
        "Grep" | "Glob" | "LS" => "search",
        "WebFetch" | "WebSearch" => "fetch",
        "EnterPlanMode" | "ExitPlanMode" => "switch_mode",
        _ => "other",
    }
    .to_owned();
    tool.title = tool_title(name, input);
    if input.is_null() {
        return;
    }
    let text = match input {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    tool.input = Some(head(text, MAX_CHAT_TOOL_INPUT_BYTES));
    let path = input["file_path"]
        .as_str()
        .or_else(|| input["notebook_path"].as_str());
    tool.locations = path.map(str::to_owned).into_iter().collect();
    let side = |text: &Value| {
        text.as_str()
            .filter(|text| text.len() <= MAX_CHAT_DIFF_BYTES)
            .map(str::to_owned)
    };
    let Some(path) = path else {
        return;
    };
    let diffs: Vec<ChatDiff> = match name {
        "Edit" => vec![ChatDiff {
            path: path.to_owned(),
            old_text: side(&input["old_string"]),
            new_text: side(&input["new_string"]),
        }],
        "MultiEdit" => input["edits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|edit| ChatDiff {
                path: path.to_owned(),
                old_text: side(&edit["old_string"]),
                new_text: side(&edit["new_string"]),
            })
            .collect(),
        "Write" => vec![ChatDiff {
            path: path.to_owned(),
            old_text: None,
            new_text: side(&input["content"]),
        }],
        _ => Vec::new(),
    };
    // Diffs once shown stay: a Write's may have its old text from its result.
    if tool.diffs.is_empty() {
        tool.diffs = diffs;
    }
}

/// A tool call in one line: the command it runs, the file it works on, what it looks
/// for, else its name.
fn tool_title(name: &str, input: &Value) -> String {
    let field = |key: &str| {
        input[key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let title = match name {
        "Bash" | "PowerShell" => field("command").map(str::to_owned),
        "Read" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => field("file_path")
            .or_else(|| field("notebook_path"))
            .map(|path| format!("{name} {path}")),
        "Grep" | "Glob" => field("pattern").map(|pattern| format!("{name} {pattern}")),
        "WebFetch" => field("url").map(str::to_owned),
        "WebSearch" => field("query").map(str::to_owned),
        "Task" | "Agent" => field("description").map(str::to_owned),
        "TodoWrite" => Some("Update the to-do list".to_owned()),
        _ => None,
    };
    title.unwrap_or_else(|| name.to_owned())
}

/// A sub-agent's tool call in one short line: its name and the argument that says most.
fn step_line(name: &str, input: &Value) -> Option<String> {
    let argument = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
    ]
    .into_iter()
    .find_map(|key| input[key].as_str());
    match argument {
        Some(argument) => short_line(&format!("{name} {argument}")),
        None => short_line(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::chat::ChatState;

    fn claude_chat() -> Inner {
        Inner {
            state: ChatState {
                harness: ChatHarness::Claude,
                status: ChatStatus::Ready,
                ..ChatState::default()
            },
            initialized: true,
            driver: Driver::Claude(Box::default()),
            ..Inner::default()
        }
    }

    fn stream(event: Value) -> Value {
        json!({ "type": "stream_event", "event": event, "parent_tool_use_id": null })
    }

    fn line(inner: &mut Inner, message: Value) {
        // Only a `result` before initialize, or a thinking level change, needs the chat.
        match message["type"].as_str() {
            Some("stream_event") => stream_event(inner, &message["event"]),
            Some("assistant") => assistant(inner, &message["message"]),
            Some("user") => tool_results(inner, &message),
            Some("system") => system(inner, &message),
            Some("control_request") => control_request(inner, &message),
            Some("control_response") => control_response(inner, &message["response"]),
            _ => unreachable!("not used here"),
        }
    }

    #[test]
    fn streamed_text_is_reconciled_and_a_tool_call_shows_its_command_then_output() {
        let mut inner = claude_chat();
        line(
            &mut inner,
            json!({ "type": "system", "subtype": "init", "session_id": "s1", "cwd": "/w" }),
        );
        assert_eq!(inner.state.session.as_deref(), Some("s1"));
        assert!(inner.state.working(), "a turn began");
        line(
            &mut inner,
            stream(json!({ "type": "message_start", "message": { "id": "m1" } })),
        );
        line(
            &mut inner,
            stream(
                json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            ),
        );
        line(
            &mut inner,
            stream(
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hel" } }),
            ),
        );
        line(
            &mut inner,
            stream(
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "lo" } }),
            ),
        );
        // The whole message wins over a stream that missed a piece.
        line(
            &mut inner,
            json!({ "type": "assistant", "message": { "id": "m1", "model": "claude-haiku-5-5",
            "content": [{ "type": "text", "text": "Hello!" }] } }),
        );
        line(
            &mut inner,
            stream(json!({ "type": "content_block_start", "index": 1,
            "content_block": { "type": "tool_use", "id": "t1", "name": "Bash", "input": {} } })),
        );
        line(
            &mut inner,
            json!({ "type": "assistant", "message": { "id": "m1", "model": "claude-haiku-5-5",
            "content": [{ "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "ls", "description": "List" } }] } }),
        );
        line(
            &mut inner,
            json!({ "type": "user", "message": { "role": "user",
            "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "a.txt", "is_error": false }] } }),
        );
        line(
            &mut inner,
            json!({ "type": "assistant", "message": { "id": "m2", "model": "claude-haiku-5-5",
            "content": [{ "type": "tool_use", "id": "e1", "name": "Edit",
                "input": { "file_path": "/w/a.txt", "old_string": "a", "new_string": "b" } }] } }),
        );
        // A turn Claude Code began itself has no prompt in the transcript.
        assert_eq!(inner.state.entries.len(), 3);
        assert_eq!(
            inner.state.entries[0],
            ChatEntry::Agent {
                text: "Hello!".into()
            }
        );
        let ChatEntry::Tool(bash) = &inner.state.entries[1] else {
            panic!("a tool call")
        };
        assert_eq!((bash.kind.as_str(), bash.title.as_str()), ("execute", "ls"));
        assert_eq!(
            (bash.status, bash.output.as_deref()),
            (ChatToolStatus::Completed, Some("a.txt"))
        );
        let ChatEntry::Tool(edit) = &inner.state.entries[2] else {
            panic!("a tool call")
        };
        assert_eq!(edit.kind, "edit");
        assert_eq!(
            edit.diffs,
            [ChatDiff {
                path: "/w/a.txt".into(),
                old_text: Some("a".into()),
                new_text: Some("b".into())
            }]
        );
    }

    #[test]
    fn a_permission_shows_on_its_call_and_always_allow_sends_the_suggestions_back() {
        let mut inner = claude_chat();
        line(
            &mut inner,
            json!({ "type": "assistant", "message": { "id": "m1", "model": "m",
            "content": [{ "type": "tool_use", "id": "t1", "name": "Edit",
                "input": { "file_path": "/w/a.txt", "old_string": "a", "new_string": "b" } }] } }),
        );
        line(
            &mut inner,
            json!({ "type": "control_request", "request_id": "r1", "request": {
            "subtype": "can_use_tool", "tool_name": "Edit", "tool_use_id": "t1",
            "input": { "file_path": "/w/a.txt", "old_string": "a", "new_string": "b" },
            "permission_suggestions": [{ "type": "setMode", "mode": "acceptEdits", "destination": "session" }] } }),
        );
        let permission = inner.state.permission.clone().expect("it asks");
        assert_eq!(permission.tool.as_deref(), Some("t1"));
        assert_eq!(permission.title, "Edit /w/a.txt");
        let ids: Vec<&str> = permission
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        assert_eq!(ids, ["allow", "always", "reject"]);
        assert!(answer(&mut inner, Some("never".into())).is_err());
        answer(&mut inner, Some("always".into())).unwrap();
        assert_eq!(inner.state.permission, None);
        // Claude Code switched to accepting edits, as the suggestion said.
        assert_eq!(
            inner.state.option("mode").map(|mode| mode.current.as_str()),
            Some("acceptEdits")
        );
        assert_eq!(inner.state.approval_mode.as_deref(), Some("acceptEdits"));
        assert!(
            answer(&mut inner, Some("allow".into())).is_err(),
            "already answered"
        );

        // A question form answers with the picked labels by question.
        line(
            &mut inner,
            json!({ "type": "control_request", "request_id": "r2", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "tool_use_id": "q1",
            "input": { "questions": [{ "question": "Tea or coffee?", "header": "Drink",
                "options": [{ "label": "Tea" }, { "label": "Coffee" }], "multiSelect": false }] } } }),
        );
        let question = inner.state.question.clone().expect("it asks");
        assert_eq!(
            (question.message.as_str(), question.fields[0].choices.len()),
            ("Drink", 2)
        );
        let answers = ChatAnswers::from([(
            "0".into(),
            ChatAnswer::Choices {
                values: vec!["Coffee".into()],
            },
        )]);
        answer_question(&mut inner, Some(answers)).unwrap();
        assert_eq!(inner.state.question, None);
    }

    #[test]
    fn a_result_ends_the_turn_with_its_cost_and_context_and_an_interrupt_cancels_it() {
        let mut inner = claude_chat();
        line(
            &mut inner,
            json!({ "type": "control_response", "response": { "subtype": "success",
            "request_id": "none", "response": {} } }),
        );
        inner.start_prompt(Prompt {
            text: "hi".into(),
            files: Vec::new(),
            images: Vec::new(),
        });
        line(
            &mut inner,
            json!({ "type": "assistant", "message": { "id": "m1", "model": "claude-haiku-5-5",
            "content": [{ "type": "text", "text": "hi" }],
            "usage": { "input_tokens": 2, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 10, "output_tokens": 4 } } }),
        );
        let result = json!({ "type": "result", "subtype": "success", "is_error": false, "stop_reason": "end_turn",
            "total_cost_usd": 0.5, "modelUsage": { "claude-haiku-5-5": { "contextWindow": 1000 } } });
        let chat = Arc::new(Chat {
            content: compi_protocol::chat::ChatContent {
                id: "claude-test".into(),
                harness: ChatHarness::Claude,
                environment: compi_protocol::ContentEnvironment::Host,
                folder: "/w".into(),
                session: None,
            },
            instance: None,
            inner: std::sync::Mutex::new(Inner::default()),
        });
        super::result(&chat, &mut inner, &result);
        assert_eq!(inner.state.status, ChatStatus::Ready);
        let usage = inner.state.usage.clone().expect("usage");
        assert_eq!((usage.used, usage.size, usage.cost), (116, 1000, Some(0.5)));
        // The process's running total grows; the chat counts what was added.
        inner.start_prompt(Prompt {
            text: "stop".into(),
            files: Vec::new(),
            images: Vec::new(),
        });
        cancel(&mut inner);
        let stopped = json!({ "type": "result", "subtype": "error_during_execution", "is_error": true,
            "terminal_reason": "aborted_tools", "total_cost_usd": 0.75 });
        super::result(&chat, &mut inner, &stopped);
        assert_eq!(inner.state.entries.last(), Some(&ChatEntry::Cancelled));
        assert_eq!(inner.state.usage.and_then(|usage| usage.cost), Some(0.75));
        assert!(inner.state.status == ChatStatus::Ready);
    }
}
