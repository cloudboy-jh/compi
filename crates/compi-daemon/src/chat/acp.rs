//! OMP's driver: the Agent Client Protocol (JSON-RPC 2.0, one message per line) over
//! `omp acp`'s standard input and output, with OMP's extensions: steering a running turn,
//! its subscription plans, and its sub-agents, whose ends and steps its session files
//! tell (`saved`).
use super::launch::Arg;
use super::{
    Chat, Driver, Inner, Prompt, Wire, head, new_agent, now_ms, saved, settle, short_line, tail,
};
use compi_protocol::chat::{
    ChatAgent, ChatAnswer, ChatAnswers, ChatChoice, ChatCommand, ChatContent, ChatDiff, ChatEntry,
    ChatEvent, ChatField, ChatHarness, ChatOption, ChatPermission, ChatPermissionOption, ChatPicks,
    ChatPlan, ChatPlanLimit, ChatQuestion, ChatSessionInfo, ChatStatus, ChatToolStatus, ChatUsage,
    MAX_CHAT_AGENT_RESULT_BYTES, MAX_CHAT_DIFF_BYTES, MAX_CHAT_TOOL_INPUT_BYTES,
    MAX_CHAT_TOOL_OUTPUT_BYTES,
};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::process::Stdio;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

/// The ACP version Compi speaks.
const ACP_VERSION: u64 = 1;
/// Pages of a session listing read at most (OMP's are 50 sessions long).
const MAX_LIST_PAGES: usize = 40;
/// How long the lookup of OMP's own approval setting may take; the harness starts without
/// it after that.
const APPROVAL_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// The modes `omp acp --approval-mode` takes.
const OMP_APPROVAL_MODES: [&str; 4] = ["always-ask", "always-ask-cached", "write", "yolo"];
/// How often a chat with sub-agents running looks in the session's file for their ends.
const AGENT_POLL: Duration = Duration::from_secs(2);

/// What the ACP driver keeps of the running harness.
#[derive(Default)]
pub(super) struct Acp {
    can_load: bool,
    can_list: bool,
    /// The folder each listed session was saved under. OMP lists a folder's sessions
    /// under every name of it (a link and its target) but loads one only under the name
    /// it was saved with.
    session_folders: HashMap<String, String>,
    /// The harness takes prompts into a running turn (`_session/steering`).
    can_steer: bool,
    /// A plans request is out; another waits for it.
    reading_plans: bool,
    next_request: u64,
    pub(super) pending: HashMap<u64, Pending>,
    /// The JSON-RPC ID of the permission request `state.permission` shows.
    permission_request: Option<Value>,
    /// The JSON-RPC ID of the form `state.question` shows, and how to send its answers.
    question_request: Option<(Value, Vec<FieldShape>)>,
}

/// OMP's driver, as the chat core calls it.
pub(super) const WIRE: Wire = Wire {
    configured_approval,
    launch_args,
    environment: |_| Vec::new(),
    start,
    handle: |chat, inner, message| chat.handle(inner, message),
    line: |_, _, _| {},
    session_with_first_prompt: false,
    prompt,
    steer: Some((can_steer, steer)),
    cancel,
    interrupt,
    answer,
    answer_question,
    set_option: |_, inner, option, value| set_option(inner, option, value),
    list_sessions,
    fill_sessions: saved::fill,
    switch_session: Some(|chat, inner, session| chat.open_session(inner, session)),
    cancel_asks,
    forget_asks,
    fail_pending,
    forget_transcript: |_| {},
};

impl Inner {
    /// The ACP driver's state; only an OMP chat has one.
    pub(super) fn acp(&mut self) -> &mut Acp {
        match &mut self.driver {
            Driver::Acp(acp) => acp,
            _ => unreachable!("an OMP chat speaks ACP"),
        }
    }

    pub(super) fn acp_state(&self) -> &Acp {
        match &self.driver {
            Driver::Acp(acp) => acp,
            _ => unreachable!("an OMP chat speaks ACP"),
        }
    }
}

/// `omp acp`'s arguments: its approval mode, when there is one, and Compi's extension,
/// which ends its system prompt with the chat's note and sends its bash tool's commands
/// where the chat is.
fn launch_args(_: &mut Inner, approval_mode: Option<&'static str>) -> Vec<Arg> {
    let mut args = vec![Arg::word("acp")];
    if let Some(mode) = approval_mode {
        args.extend([Arg::word("--approval-mode"), Arg::word(mode)]);
    }
    args.push(Arg::Bridge {
        flag: "-e",
        file: "compi-omp.js",
    });
    args
}

/// Asks the harness to initialize; the rest follows its answers.
pub(super) fn start(inner: &mut Inner) {
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

/// What a request Compi sent the harness was for.
pub(super) enum Pending {
    Initialize,
    NewSession,
    LoadSession,
    Prompt,
    /// One of the user's picks set on the new `session`; `rest` follow, in order.
    Pick {
        session: String,
        option: String,
        value: String,
        rest: VecDeque<(&'static str, String)>,
    },
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

/// How a question field's answer goes back to the harness.
#[derive(Clone)]
pub(super) struct FieldShape {
    key: String,
    /// The schema's value type: `string`, `array`, `boolean`, `number` or `integer`.
    kind: String,
    /// The companion free-text key, when the field has one.
    other: Option<String>,
}

impl Prompt {
    /// As ACP content blocks: the text, a link per file, the images.
    pub(super) fn blocks(&self) -> Vec<Value> {
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
}

impl Chat {
    pub(super) fn handle(self: &Arc<Self>, inner: &mut Inner, message: Value) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => inner.agent_request(method, id, &message["params"]),
            (Some("session/update"), None) => {
                inner.update(&message["params"]["update"]);
            }
            (Some(_), None) => {}
            (None, Some(id)) => {
                let Some(pending) = id.as_u64().and_then(|id| inner.acp().pending.remove(&id))
                else {
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
        self.watch_agents(inner);
    }

    /// While sub-agents run, reads OMP's session files for what ACP leaves out: the ones
    /// that finished, and each one's tool calls. OMP reports a sub-agent over ACP only
    /// while a turn runs: one that ends after its turn would show as running for good.
    fn watch_agents(self: &Arc<Self>, inner: &mut Inner) {
        let generation = inner.generation;
        let revision = inner.agents_revision;
        let watch = (generation, revision);
        // Replayed calls must all exist before saved completions are consumed.
        if inner.loading() || inner.agents_watched == Some(watch) || inner.open_agents().is_empty()
        {
            return;
        }
        let offset = inner.agents_offset;
        let chat = self.clone();
        let watching = thread::Builder::new()
            .name("compi-chat-agents".into())
            .spawn(move || {
                // Replay existing completions too: they may predate this process.
                let mut file = None;
                // Each open agent's own file, read from its start.
                let mut steps: HashMap<String, saved::AgentSteps> = HashMap::new();
                loop {
                    let (session, open) = {
                        let mut inner = chat.lock();
                        let open = inner.open_agents();
                        if inner.closed
                            || inner.generation != generation
                            || inner.agents_revision != revision
                            || open.is_empty()
                        {
                            if inner.agents_watched == Some(watch) {
                                inner.agents_watched = None;
                            }
                            return;
                        }
                        (inner.state.session.clone(), open)
                    };
                    match &mut file {
                        None => {
                            file = session
                                .and_then(|session| saved::session_file(&chat.content, &session))
                                .map(|path| (path, offset));
                        }
                        Some((path, offset)) => {
                            // Their calls first, so one that ended shows its last ones.
                            steps.retain(|id, _| open.contains(id));
                            let folder = path.with_extension("");
                            for id in &open {
                                // An agent's name is a file's name here, never a path.
                                if id.contains(['/', '\\']) || id.starts_with('.') {
                                    continue;
                                }
                                let read = steps.entry(id.clone()).or_default();
                                if read.read(&folder.join(format!("{id}.jsonl"))) {
                                    let mut inner = chat.lock();
                                    if !inner.closed
                                        && inner.generation == generation
                                        && inner.agents_revision == revision
                                    {
                                        inner.agent_steps(id, read);
                                    }
                                }
                            }
                            let jobs = saved::finished_agents(path, offset);
                            let mut inner = chat.lock();
                            if !inner.closed
                                && inner.generation == generation
                                && inner.agents_revision == revision
                            {
                                inner.agents_offset = *offset;
                                inner.fill_results(&jobs, now_ms());
                            }
                        }
                    }
                    thread::sleep(AGENT_POLL);
                }
            });
        if watching.is_ok() {
            inner.agents_watched = Some(watch);
        }
    }

    pub(super) fn answered(
        &self,
        inner: &mut Inner,
        pending: Pending,
        result: Result<Value, String>,
    ) {
        let harness = self.content.harness.name();
        match (pending, result) {
            (Pending::Initialize, Ok(result)) => {
                let capabilities = &result["agentCapabilities"];
                inner.initialized = true;
                let acp = inner.acp();
                acp.can_load = capabilities["loadSession"].as_bool().unwrap_or(false);
                acp.can_list = capabilities["sessionCapabilities"]["list"].is_object();
                acp.can_steer = result["_meta"]["steering"]["supported"]
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
            (Pending::NewSession, Ok(result)) => {
                inner.session_opened(&result);
                let ChatPicks {
                    model,
                    thought_level,
                } = inner.picks.clone();
                // The model first: the thinking levels on offer may depend on it.
                let picks = [("model", model), ("thought_level", thought_level)]
                    .into_iter()
                    .filter_map(|(category, value)| Some((category, value?)))
                    .collect();
                inner.apply_picks(picks);
                self.read_plans(inner);
            }
            // A resumed session keeps its own settings. A chat that had no thread gets
            // one once the session it resumed shows.
            (Pending::LoadSession, Ok(result)) => {
                inner.skip_replay = false;
                inner.session_opened(&result);
                if inner.thread.is_none() && !inner.state.entries.is_empty() {
                    inner.start_thread();
                }
                inner.apply_picks(VecDeque::new());
                self.read_plans(inner);
            }
            // The transcript so far stays, above the error and the new session.
            (Pending::LoadSession, Err(error)) => {
                inner.skip_replay = false;
                inner.emit(ChatEvent::Session { session: None });
                inner.emit(ChatEvent::Entry {
                    entry: ChatEntry::Error {
                        message: format!("Couldn't resume the earlier session: {error}"),
                    },
                });
                inner.save_thread(true);
                self.open_session(inner, None);
            }
            (Pending::NewSession, Err(error)) => inner.emit(ChatEvent::Status {
                status: ChatStatus::Failed {
                    message: format!("{harness} couldn't start a session: {error}"),
                },
            }),
            (Pending::SetOption, Ok(result)) => inner.options(&result["configOptions"]),
            (
                Pending::Pick {
                    session,
                    option,
                    value,
                    rest,
                },
                result,
            ) => {
                // A session that replaced this one got picks of its own.
                if inner.state.session.as_deref() != Some(session.as_str()) {
                    return;
                }
                match result {
                    Ok(result) => inner.options(&result["configOptions"]),
                    Err(error) => eprintln!(
                        "compi-daemon: chat {} kept its {option} setting, not {value}: {error}",
                        inner.chat_id
                    ),
                }
                inner.apply_picks(rest);
            }
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
                match (inner.acp().session_folders.get(&session).cloned(), cursor) {
                    (Some(folder), _) => self.load_session(inner, session, &folder),
                    (None, Some(cursor)) => self.list_page(
                        inner,
                        Some(cursor),
                        Pending::Locate {
                            session,
                            pages: pages + 1,
                        },
                    ),
                    (None, None) => {
                        let folder = inner.agent_folder().to_owned();
                        self.load_session(inner, session, &folder);
                    }
                }
            }
            (Pending::Steer(prompt), Ok(result)) if result["outcome"] == "injected" => {
                inner.end_thought();
                inner.activity_ms = now_ms();
                inner.push_prompt(&prompt);
                // A steer turns down the permission the harness was waiting on.
                inner.acp().permission_request = None;
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
                inner.acp().reading_plans = false;
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
        if self.content.harness != ChatHarness::Omp || inner.acp().reading_plans {
            return;
        }
        inner.acp().reading_plans = true;
        inner.request("_omp/usage", json!({}), Pending::Plans);
    }

    /// Loads `session`, whose history the harness replays, in the folder it was saved
    /// under (looked up in the listing first when not known yet), or starts a new one.
    pub(super) fn open_session(&self, inner: &mut Inner, session: Option<String>) {
        let acp = inner.acp_state();
        let (can_load, can_list) = (acp.can_load, acp.can_list);
        let saved_in = session
            .as_ref()
            .and_then(|session| acp.session_folders.get(session).cloned());
        match session.filter(|_| can_load) {
            Some(session) => match saved_in {
                Some(folder) => self.load_session(inner, session, &folder),
                None if can_list => {
                    self.list_page(inner, None, Pending::Locate { session, pages: 1 });
                }
                None => {
                    let folder = inner.agent_folder().to_owned();
                    self.load_session(inner, session, &folder);
                }
            },
            None => {
                let cwd = inner.agent_folder().to_owned();
                inner.request(
                    "session/new",
                    json!({ "cwd": cwd, "mcpServers": [] }),
                    Pending::NewSession,
                );
            }
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
        let mut params = json!({ "cwd": inner.agent_folder() });
        if let Some(cursor) = cursor {
            params["cursor"] = cursor.into();
        }
        inner.request("session/list", params, pending);
    }
}

/// Starts a turn on `prompt`, which the transcript already shows.
pub(super) fn prompt(inner: &mut Inner, prompt: &Prompt) {
    // Prompts wait for a session (`Inner::has_session`).
    let session = inner.state.session.clone().unwrap_or_default();
    let blocks = prompt.blocks();
    inner.request(
        "session/prompt",
        json!({ "sessionId": session, "prompt": blocks }),
        Pending::Prompt,
    );
}

/// The harness takes prompts into a running turn.
pub(super) fn can_steer(inner: &Inner) -> bool {
    inner.acp_state().can_steer
}

/// Hands `prompt` to the running turn (`_session/steering`); it waits its turn when the
/// harness declines.
pub(super) fn steer(inner: &mut Inner, harness: &str, prompt: Prompt) -> Result<(), String> {
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
    Ok(())
}

/// Stops the running turn: its open permission and question go unanswered.
pub(super) fn cancel(inner: &mut Inner) {
    inner.cancel_requests();
    interrupt(inner);
}

/// Asks the harness to stop the running turn.
pub(super) fn interrupt(inner: &mut Inner) {
    if let Some(session) = inner.state.session.clone() {
        inner.write(&json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": { "sessionId": session },
        }));
    }
}

/// Answers the permission `state.permission` shows with `option`, or cancels it.
pub(super) fn answer(inner: &mut Inner, option: Option<String>) -> Result<(), String> {
    let Some(request) = inner.acp().permission_request.take() else {
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

/// Answers the form `state.question` shows with `answers`, or declines it.
pub(super) fn answer_question(
    inner: &mut Inner,
    answers: Option<ChatAnswers>,
) -> Result<(), String> {
    let Some((request, shapes)) = inner.acp().question_request.take() else {
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

/// Sets the session's setting `option` to `value`, one of its choices.
pub(super) fn set_option(inner: &mut Inner, option: String, value: String) -> Result<(), String> {
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

/// Asks the running harness for its sessions in the chat's folder, page by page.
pub(super) fn list_sessions(
    chat: &Chat,
    inner: &mut Inner,
) -> Result<mpsc::Receiver<Result<Vec<ChatSessionInfo>, String>>, String> {
    if !inner.acp_state().can_list {
        return Err(format!(
            "{} doesn't list its sessions",
            chat.content.harness.name()
        ));
    }
    let (reply, receiver) = mpsc::channel();
    chat.list_page(
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

/// Tells the harness nobody will answer its open permission or question.
pub(super) fn cancel_asks(inner: &mut Inner) {
    if let Some(request) = inner.acp().permission_request.take() {
        inner.respond(request, json!({ "outcome": { "outcome": "cancelled" } }));
    }
    if let Some((request, _)) = inner.acp().question_request.take() {
        inner.respond(request, json!({ "action": "cancel" }));
    }
}

/// Forgets the open permission and question, which the harness no longer waits on.
pub(super) fn forget_asks(inner: &mut Inner) {
    let acp = inner.acp();
    acp.permission_request = None;
    acp.question_request = None;
}

/// Windows waiting on a session listing hear the harness is gone; nothing it was asked is
/// out any more.
pub(super) fn fail_pending(inner: &mut Inner) {
    let acp = inner.acp();
    for (_, pending) in acp.pending.drain() {
        if let Pending::List { reply, .. } = pending {
            let _ = reply.send(Err("the harness stopped".into()));
        }
    }
    acp.reading_plans = false;
}

impl Inner {
    /// A session is loading.
    fn loading(&self) -> bool {
        self.acp_state()
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::LoadSession))
    }

    /// The harness opened a session, new or resumed: its ID and settings. Its prompts
    /// start over, so the first one shows no switch.
    pub(super) fn session_opened(&mut self, result: &Value) {
        self.end_thought();
        self.prompted_with = None;
        if let Some(session) = result["sessionId"].as_str() {
            self.emit(ChatEvent::Session {
                session: Some(session.to_owned()),
            });
        }
        self.options(&result["configOptions"]);
        self.save_thread(false);
    }

    /// Sets the first of `picks` (category, value) the session offers and does not have
    /// yet, the rest once the harness answers, as one may change the choices of the
    /// next. A setting the session lacks, or a value it does not list, is passed over.
    /// The chat stays `Starting`, prompts queued, until none is left.
    fn apply_picks(&mut self, mut picks: VecDeque<(&'static str, String)>) {
        while let Some((category, value)) = picks.pop_front() {
            let Some(option) = self.state.option(category).filter(|option| {
                option.current != value && option.choices.iter().any(|c| c.value == value)
            }) else {
                continue;
            };
            let option = option.id.clone();
            let Some(session) = self.state.session.clone() else {
                break;
            };
            self.request(
                "session/set_config_option",
                json!({ "sessionId": session, "configId": option, "value": value }),
                Pending::Pick {
                    session,
                    option,
                    value,
                    rest: picks,
                },
            );
            return;
        }
        self.emit(ChatEvent::Status {
            status: ChatStatus::Ready,
        });
        self.send_queued();
    }

    fn request(&mut self, method: &str, params: Value, pending: Pending) {
        let acp = self.acp();
        acp.next_request += 1;
        let id = acp.next_request;
        acp.pending.insert(id, pending);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn respond(&mut self, id: Value, result: Value) {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// Remembers the folder each session in a listing page was saved under.
    fn note_folders(&mut self, result: &Value) {
        for session in result["sessions"].as_array().into_iter().flatten() {
            if let (Some(id), Some(folder)) =
                (session["sessionId"].as_str(), session["cwd"].as_str())
            {
                self.acp()
                    .session_folders
                    .insert(id.to_owned(), folder.to_owned());
            }
        }
    }

    /// The harness's settings, from `configOptions`; only select settings are offered.
    pub(super) fn options(&mut self, value: &Value) {
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

    pub(super) fn permission(&mut self, id: Value, params: &Value) {
        // A newer request replaces one nobody answered.
        if let Some(previous) = self.acp().permission_request.take() {
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
        self.acp().permission_request = Some(id);
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

    pub(super) fn question(&mut self, id: Value, params: &Value) {
        if let Some((previous, _)) = self.acp().question_request.take() {
            self.respond(previous, json!({ "action": "cancel" }));
        }
        let (fields, shapes) = form_fields(&params["requestedSchema"]);
        self.next_question += 1;
        self.acp().question_request = Some((id, shapes));
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
    pub(super) fn update(&mut self, update: &Value) {
        let kind = update["sessionUpdate"].as_str().unwrap_or_default();
        let entry_kind = matches!(
            kind,
            "agent_message_chunk"
                | "agent_thought_chunk"
                | "user_message_chunk"
                | "tool_call"
                | "tool_call_update"
        );
        // The transcript Compi saved already holds what the session replays as it loads.
        if entry_kind && self.skip_replay && self.loading() {
            return;
        }
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
                        self.save_thread(false);
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
                            sent_ms: None,
                            ended_ms: None,
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
                    sent_ms,
                    ended_ms,
                },
            )) => {
                let (mut files, mut images) = (files.clone(), images.clone());
                add(&mut files, &mut images);
                let entry = ChatEntry::User {
                    text: text.clone(),
                    files,
                    images,
                    sent_ms: *sent_ms,
                    ended_ms: *ended_ms,
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
                        sent_ms: None,
                        ended_ms: None,
                    },
                });
            }
        }
    }

    /// A tool call or an update to one: fields present replace what was reported.
    fn tool(&mut self, update: &Value) {
        let Some(id) = update["toolCallId"].as_str() else {
            return;
        };
        let mut tool = self.tool_call(id);
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
        let was = tool.status;
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
        let details = &update["rawOutput"]["details"];
        let now = now_ms();
        for item in details["progress"].as_array().into_iter().flatten() {
            agent_progress(&mut tool.agents, item, now);
        }
        for item in details["results"].as_array().into_iter().flatten() {
            agent_result(&mut tool.agents, item);
        }
        if let Some(jobs) = details["jobs"].as_array().filter(|jobs| !jobs.is_empty()) {
            tool.waits_on_agents = jobs.iter().all(|job| job["type"] == "task");
        }
        settle(&mut tool, was, now);
        self.put_tool(tool);
        if let Some(jobs) = details["jobs"].as_array() {
            self.fill_results(jobs, now);
        }
    }

    /// A `wait`'s jobs: what each finished sub-agent answered goes to it, on the latest
    /// call that started one of its name.
    pub(super) fn fill_results(&mut self, jobs: &[Value], now: u64) {
        for job in jobs {
            if job["type"].as_str().is_some_and(|kind| kind != "task") {
                continue;
            }
            let Some(id) = job["agentUrlId"].as_str().or_else(|| job["id"].as_str()) else {
                continue;
            };
            let found =
                self.state.entries.iter().enumerate().rev().find_map(
                    |(index, entry)| match entry {
                        ChatEntry::Tool(tool) => tool
                            .agents
                            .iter()
                            .position(|agent| agent.id == id)
                            .map(|agent| (index, agent, tool)),
                        _ => None,
                    },
                );
            let Some((index, agent, tool)) = found else {
                continue;
            };
            let mut tool = tool.clone();
            job_result(&mut tool.agents[agent], job);
            let was = tool.status;
            settle(&mut tool, was, now);
            let entry = ChatEntry::Tool(tool);
            if self.state.entries[index] != entry {
                self.emit(ChatEvent::Replace { index, entry });
                self.note_agents(index);
            }
        }
    }

    /// The tool calls the sub-agent `id` made, as its own session file tells them, on
    /// the latest call that started one of its name: they replace the ones shown, and
    /// the latest says what it does now.
    pub(super) fn agent_steps(&mut self, id: &str, read: &saved::AgentSteps) {
        let found = self
            .state
            .entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| match entry {
                ChatEntry::Tool(tool) => tool
                    .agents
                    .iter()
                    .position(|agent| agent.id == id)
                    .map(|agent| (index, agent, tool)),
                _ => None,
            });
        let Some((index, agent, tool)) = found else {
            return;
        };
        let mut tool = tool.clone();
        let agent = &mut tool.agents[agent];
        agent.steps = read.steps.iter().cloned().collect();
        agent.tool_count = agent.tool_count.max(read.count);
        if agent.status.is_open()
            && let Some(step) = read.steps.back()
        {
            agent.activity = Some(step.text.clone());
        }
        let entry = ChatEntry::Tool(tool);
        if self.state.entries[index] != entry {
            self.emit(ChatEvent::Replace { index, entry });
        }
    }
}

/// A sub-agent's status as OMP names it.
fn agent_status(status: Option<&str>) -> ChatToolStatus {
    match status {
        Some("running") => ChatToolStatus::Running,
        Some("completed") => ChatToolStatus::Completed,
        Some("failed" | "error") => ChatToolStatus::Failed,
        Some("aborted" | "cancelled") => ChatToolStatus::Stopped,
        _ => ChatToolStatus::Pending,
    }
}

/// One item of a `task` call's `details.progress` into its sub-agent in `agents`. A
/// finished agent stays finished: a later report of it running is stale.
fn agent_progress(agents: &mut Vec<ChatAgent>, item: &Value, now: u64) {
    let Some(id) = item["id"].as_str() else {
        return;
    };
    let agent = match agents.iter().position(|agent| agent.id == id) {
        Some(index) => &mut agents[index],
        None => {
            agents.push(new_agent(id));
            agents.last_mut().expect("just pushed")
        }
    };
    let status = agent_status(item["status"].as_str());
    if !agent.status.is_open() && status.is_open() {
        return;
    }
    agent.status = status;
    if let Some(kind) = item["agent"].as_str() {
        kind.clone_into(&mut agent.agent);
    }
    if let Some(activity) = activity(item) {
        agent.activity = Some(activity);
    }
    agent.tool_count = item["toolCount"].as_u64().unwrap_or(agent.tool_count);
    agent.duration_ms = item["durationMs"].as_u64().unwrap_or(agent.duration_ms);
    if status == ChatToolStatus::Running && agent.started_ms.is_none() {
        agent.started_ms = Some(now.saturating_sub(agent.duration_ms));
    }
    if let Some(cost) = item["cost"].as_f64().filter(|cost| *cost >= 0.0) {
        agent.cost = Some(cost);
    }
    if let Some(model) = item["resolvedModel"].as_str() {
        agent.model = Some(model.to_owned());
    }
    if let Some(task) = item["assignment"]
        .as_str()
        .or_else(|| item["task"].as_str())
        .map(str::trim)
        .filter(|task| !task.is_empty())
    {
        agent.assignment = Some(head(task.to_owned(), MAX_CHAT_TOOL_INPUT_BYTES));
    }
}

/// One item of a `task` call's `details.results`, as a call that waited for its
/// sub-agents ends: the agent finished, with its output.
fn agent_result(agents: &mut Vec<ChatAgent>, item: &Value) {
    let Some(id) = item["id"].as_str() else {
        return;
    };
    let agent = match agents.iter().position(|agent| agent.id == id) {
        Some(index) => &mut agents[index],
        None => {
            agents.push(new_agent(id));
            agents.last_mut().expect("just pushed")
        }
    };
    if agent.status.is_open() {
        agent.status = if item["aborted"].as_bool() == Some(true) {
            ChatToolStatus::Stopped
        } else if item["exitCode"].as_i64() == Some(0) {
            ChatToolStatus::Completed
        } else {
            ChatToolStatus::Failed
        };
    }
    agent.duration_ms = item["durationMs"].as_u64().unwrap_or(agent.duration_ms);
    if let Some(kind) = item["agent"].as_str() {
        kind.clone_into(&mut agent.agent);
    }
    if let Some(model) = item["resolvedModel"].as_str() {
        agent.model = Some(model.to_owned());
    }
    if let Some(cost) = item["usage"]["cost"]["total"]
        .as_f64()
        .filter(|cost| *cost >= 0.0)
    {
        agent.cost = Some(cost);
    }
    let output = [&item["output"], &item["error"]]
        .into_iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|text| !text.is_empty());
    if let Some(output) = output {
        agent.result = Some(head(output.to_owned(), MAX_CHAT_AGENT_RESULT_BYTES));
    }
}

/// A `wait` job of a sub-agent: once finished, how it ended, how long it ran and what it
/// answered.
fn job_result(agent: &mut ChatAgent, job: &Value) {
    let status = agent_status(job["status"].as_str());
    if status.is_open() {
        return;
    }
    if agent.status.is_open() {
        agent.status = status;
    }
    agent.duration_ms = job["durationMs"].as_u64().unwrap_or(agent.duration_ms);
    let output = job["resultText"]
        .as_str()
        .map(task_output)
        .into_iter()
        .chain(job["errorText"].as_str())
        .map(str::trim)
        .find(|text| !text.is_empty());
    if let Some(output) = output {
        agent.result = Some(head(output.to_owned(), MAX_CHAT_AGENT_RESULT_BYTES));
    } else if agent.result.is_none() {
        // A finished job can have no text; it is still no longer awaiting its answer.
        agent.result = Some(String::new());
    }
}

/// The output in a `<task-result …><output>…</output></task-result>` block (or its
/// `<preview>`); other text as it is.
pub(super) fn task_output(text: &str) -> &str {
    if !text.trim_start().starts_with("<task-result") {
        return text;
    }
    for tag in ["output", "preview"] {
        let open = format!("<{tag}");
        let Some(start) = text.find(&open) else {
            continue;
        };
        let rest = &text[start + open.len()..];
        if !rest.starts_with(['>', ' ', '\t', '\n']) {
            continue;
        }
        let Some(body) = rest.find('>').map(|end| &rest[end + 1..]) else {
            continue;
        };
        if let Some(end) = body.rfind(&format!("</{tag}>")) {
            return &body[..end];
        }
    }
    text
}

/// What a sub-agent does now: its current tool, else its latest (OMP lists them newest
/// first), else its last stated intent; a tool as its intent, else its name and
/// arguments. One line, at most `AGENT_ACTIVITY_CHARS`.
pub(super) fn activity(item: &Value) -> Option<String> {
    let describe = |tool: &str, intent: &Value, args: &Value| {
        if let Some(intent) = intent
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return intent.to_owned();
        }
        let args = match args {
            Value::String(text) => text.trim().to_owned(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        if args.is_empty() {
            tool.to_owned()
        } else {
            format!("{tool} {args}")
        }
    };
    let recent = &item["recentTools"][0];
    let text = if let Some(tool) = item["currentTool"].as_str() {
        describe(tool, &item["currentToolIntent"], &item["currentToolArgs"])
    } else if let Some(tool) = recent["tool"].as_str() {
        describe(tool, &recent["intent"], &recent["args"])
    } else {
        item["lastIntent"].as_str()?.to_owned()
    };
    short_line(&text)
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

/// A form's fields in the order the form lists them, and how to send each answer back.
/// A choice field's free-text companion (see `companion`) becomes its Other.
pub(super) fn form_fields(schema: &Value) -> (Vec<ChatField>, Vec<FieldShape>) {
    let empty = Map::new();
    let properties = schema["properties"].as_object().unwrap_or(&empty);
    let required: HashSet<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let listed: Vec<(&String, &Value, String, Vec<ChatChoice>, bool)> = properties
        .iter()
        .map(|(key, property)| {
            let kind = property["type"].as_str().unwrap_or("string").to_owned();
            let (choices, multi) = field_choices(&kind, property);
            (key, property, kind, choices, multi)
        })
        .collect();
    // Each choice field's companion; one whose choice field is missing stands alone.
    let companions: Vec<(&str, String)> = listed
        .iter()
        .filter(|(_, _, _, choices, _)| !choices.is_empty())
        .filter_map(|(key, ..)| Some((key.as_str(), companion(properties, key)?)))
        .collect();
    let mut fields = Vec::new();
    let mut shapes = Vec::new();
    for (key, property, kind, choices, multi) in listed {
        if companions.iter().any(|(_, other)| other == key) {
            continue;
        }
        let other_key = companions
            .iter()
            .find(|(base, _)| base == key)
            .map(|(_, other)| other.clone());
        fields.push(ChatField {
            key: key.clone(),
            title: property["title"]
                .as_str()
                .or_else(|| property["description"].as_str())
                .unwrap_or(key)
                .to_owned(),
            choices,
            multi,
            other: other_key.is_some(),
            required: required.contains(key.as_str()),
        });
        shapes.push(FieldShape {
            key: key.clone(),
            kind,
            other: other_key,
        });
    }
    (fields, shapes)
}

/// A field's choices, from its `oneOf`, `anyOf` or `enum` (its items' for an array, which
/// takes several), or Yes and No for a boolean; none for a text field.
fn field_choices(kind: &str, property: &Value) -> (Vec<ChatChoice>, bool) {
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
    match kind {
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
    }
}

/// The key of choice field `key`'s free-text companion, if the form has one: `{key}__other`
/// (OMP's `ask`), or `{key}_other` with a `_` added for each name the form already used
/// (OMP's HTML forms).
fn companion(properties: &Map<String, Value>, key: &str) -> Option<String> {
    let ask = format!("{key}__other");
    if properties.contains_key(&ask) {
        return Some(ask);
    }
    let mut other = format!("{key}_other");
    if !properties.contains_key(&other) {
        return None;
    }
    while properties.contains_key(&format!("{other}_")) {
        other.push('_');
    }
    Some(other)
}

/// The form's `content`: each answer in its field's type; typed text for a choice field
/// goes to its free-text companion, which the harness takes over the choice.
pub(super) fn form_content(shapes: &[FieldShape], mut answers: ChatAnswers) -> Value {
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
pub(super) fn file_uri(path: &str) -> String {
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
pub(super) fn path_of_uri(uri: &str) -> String {
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

/// OMP's own `tools.approvalMode` in the chat's environment and folder, when it is a mode
/// `omp acp` takes; `None` when the lookup fails or takes longer than
/// `APPROVAL_LOOKUP_TIMEOUT`. The value is the last line: a login shell's profile may
/// print before it.
fn configured_approval(content: &ChatContent) -> Option<&'static str> {
    let args = ["config", "get", "tools.approvalMode"].map(Arg::word);
    let programs = super::launch::programs(content);
    let (mut command, _) = super::launch::command(content, &[], &args, &programs).ok()?;
    let mut child = command
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
