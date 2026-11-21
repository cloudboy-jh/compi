//! The driver against sessions recorded from the installed programs (`fixtures/`, cut
//! from `target/agent-notes/opencode-api/`): OpenCode 1.19 (`v1-*`) and 2.0.27
//! (`v2-*`), each a turn that runs a command, writes and edits a file, starts a
//! sub-agent and asks permission, a question, and a stopped turn.
use super::*;
use compi_protocol::chat::{ChatPicks, ChatState, ChatTool};

const V1_TURN: &str = include_str!("fixtures/v1-turn.jsonl");
const V1_INTERRUPT: &str = include_str!("fixtures/v1-interrupt.jsonl");
const V2_TURN: &str = include_str!("fixtures/v2-turn.jsonl");
const V2_QUESTION: &str = include_str!("fixtures/v2-question.jsonl");
const V2_INTERRUPT: &str = include_str!("fixtures/v2-interrupt.jsonl");

fn chat(dialect: Dialect, approval: Option<&'static str>) -> Inner {
    let mut inner = Inner {
        state: ChatState {
            harness: ChatHarness::OpenCode,
            ..ChatState::default()
        },
        driver: Driver::OpenCode(Box::default()),
        ..Inner::default()
    };
    launch_args(&mut inner, approval);
    inner.opencode().dialect = Some(dialect);
    inner
}

fn sent(inner: &mut Inner) -> Vec<Job> {
    std::mem::take(&mut inner.opencode().sent)
}

/// Answers the one request sent since the last look, which goes to `path`.
fn reply(inner: &mut Inner, path: &str, answer: Value) -> Job {
    let mut jobs = sent(inner);
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    let job = jobs.remove(0);
    assert!(job.path.starts_with(path), "{} is not {path}", job.path);
    answered(inner, job.id, Ok(answer));
    job
}

fn text_prompt(text: &str) -> Prompt {
    Prompt {
        text: text.into(),
        files: Vec::new(),
        images: Vec::new(),
    }
}

/// Feeds a recording's events; `ask` answers each permission and question it shows.
fn feed(inner: &mut Inner, recording: &str, mut ask: impl FnMut(&mut Inner)) {
    for line in recording.lines() {
        let event: Value = serde_json::from_str(line).unwrap();
        super::event(inner, &event);
        if inner.state.permission.is_some() || inner.state.question.is_some() {
            ask(inner);
        }
    }
}

fn tools(inner: &Inner) -> Vec<&ChatTool> {
    inner
        .state
        .entries
        .iter()
        .filter_map(|entry| match entry {
            ChatEntry::Tool(tool) => Some(&**tool),
            _ => None,
        })
        .collect()
}

fn tool<'a>(inner: &'a Inner, name: &str) -> &'a ChatTool {
    tools(inner)
        .into_iter()
        .find(|tool| tool.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no {name} card"))
}

fn replies(inner: &Inner) -> Vec<&str> {
    inner
        .state
        .entries
        .iter()
        .filter_map(|entry| match entry {
            ChatEntry::Agent { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Loads 2.x's models, agents and default model, and takes the chat to Ready.
fn start_v2(inner: &mut Inner) {
    super::event(inner, &json!({ "type": "server.connected", "data": {} }));
    reply(
        inner,
        "/api/model",
        json!({ "data": [
            { "id": "big-pickle", "providerID": "opencode", "name": "Big Pickle", "variants": [],
              "limit": { "context": 200000 } },
            { "id": "gpt-5.5", "providerID": "openai", "name": "GPT-5.5",
              "variants": [{ "id": "low" }, { "id": "high" }], "limit": { "context": 400000 } },
        ] }),
    );
    reply(
        inner,
        "/api/agent",
        json!({ "data": [
            { "id": "build", "name": "Build", "mode": "primary", "hidden": false },
            { "id": "general", "name": "General", "mode": "subagent", "hidden": false },
            { "id": "title", "name": "Title", "mode": "primary", "hidden": true },
            { "id": "plan", "name": "Plan", "mode": "primary", "hidden": false },
        ] }),
    );
    reply(
        inner,
        "/api/model/default",
        json!({ "data": { "id": "big-pickle", "providerID": "opencode" } }),
    );
}

/// Loads 1.x's providers, agents and configuration, and takes the chat to Ready.
fn start_v1(inner: &mut Inner) {
    super::event(
        inner,
        &json!({ "type": "server.connected", "properties": {} }),
    );
    reply(
        inner,
        "/config/providers",
        json!({
            "providers": [
                { "id": "anthropic", "name": "Anthropic", "models": {
                    "claude-opus-4-5": { "id": "claude-opus-4-5", "name": "Claude Opus 4.5",
                        "variants": { "high": {}, "low": {} }, "limit": { "context": 200000 } } } },
                { "id": "opencode", "name": "OpenCode", "models": {
                    "big-pickle": { "id": "big-pickle", "name": "Big Pickle", "limit": { "context": 200000 } } } },
            ],
            "default": { "anthropic": "claude-opus-4-5", "opencode": "big-pickle" },
        }),
    );
    reply(
        inner,
        "/agent",
        json!([
            { "name": "build", "mode": "primary" },
            { "name": "explore", "mode": "subagent" },
            { "name": "plan", "mode": "primary" },
            { "name": "summary", "mode": "primary", "hidden": true },
        ]),
    );
    reply(inner, "/config", json!({ "model": "opencode/big-pickle" }));
}

#[test]
fn the_server_line_starts_nothing_until_it_names_its_address() {
    let mut inner = chat(Dialect::V1, None);
    let password = inner.opencode_state().password.clone();
    assert_eq!(password.len(), 32);
    assert_ne!(password, chat(Dialect::V1, None).opencode_state().password);
    assert_eq!(
        (WIRE.environment)(&inner),
        [("OPENCODE_SERVER_PASSWORD", password)]
    );
    assert_eq!(
        launch_args(&mut inner, None),
        ["serve", "--hostname", "127.0.0.1", "--port", "0"].map(Arg::word)
    );
}

#[test]
fn compi_s_levels_become_session_rules_the_last_match_winning() {
    assert!(rules(None).is_empty());
    assert_eq!(rules(Some("allow-all")), [("*", "*", "allow")]);
    let ask = rules(Some("ask"));
    assert_eq!(ask[0], ("*", "*", "ask"));
    assert!(ask.contains(&("read", "*", "allow")));
    assert!(ask.contains(&("read", "*.env", "ask")));
    assert!(!ask.contains(&("edit", "*", "allow")));
    let edits = rules(Some("allow-edits"));
    assert!(edits.contains(&("edit", "*", "allow")));
    assert!(!edits.contains(&("bash", "*", "allow")));
    assert_eq!(
        v1::rules(&[("*", "*", "ask")]),
        json!([{ "permission": "*", "pattern": "*", "action": "ask" }])
    );
    assert_eq!(
        v2::rules(&[("*", "*", "ask")]),
        json!([{ "action": "*", "resource": "*", "effect": "ask" }])
    );
}

#[test]
fn v2_loads_its_models_by_provider_then_a_first_prompt_opens_a_session_on_the_picks() {
    let mut inner = chat(Dialect::V2, Some("ask"));
    inner.picks = ChatPicks {
        model: Some("openai/gpt-5.5".into()),
        thought_level: Some("high".into()),
    };
    start_v2(&mut inner);
    assert_eq!(inner.state.status, ChatStatus::Ready);
    let model = inner.state.option("model").unwrap();
    assert_eq!(model.current, "openai/gpt-5.5");
    assert_eq!(
        model
            .choices
            .iter()
            .map(|choice| choice.value.as_str())
            .collect::<Vec<_>>(),
        ["opencode/big-pickle", "openai/gpt-5.5"]
    );
    let thinking = inner.state.option("thought_level").unwrap();
    assert_eq!(thinking.current, "high");
    assert_eq!(thinking.choices[0].name, "Default");
    let mode = inner.state.option("mode").unwrap();
    assert_eq!(mode.current, "build");
    assert_eq!(mode.choices.len(), 2);

    inner.start_prompt(text_prompt("hi"));
    let create = reply(
        &mut inner,
        "/api/session",
        json!({ "data": { "id": "ses_1" } }),
    );
    let body = create.body.unwrap();
    assert_eq!(
        body["model"],
        json!({ "id": "gpt-5.5", "providerID": "openai", "variant": "high" })
    );
    assert_eq!(body["agent"], "build");
    assert_eq!(
        body["permissions"][0],
        json!({ "action": "*", "resource": "*", "effect": "ask" })
    );
    assert_eq!(inner.state.session.as_deref(), Some("ses_1"));
    let prompt = sent(&mut inner).pop().unwrap();
    assert_eq!(prompt.path, "/api/session/ses_1/prompt");
    assert_eq!(prompt.body.unwrap(), json!({ "text": "hi" }));

    // A changed setting switches the session before the next prompt.
    inner.prompt_ended(Ok(json!({ "stopReason": "end_turn" })));
    set_option(&mut inner, "model".into(), "opencode/big-pickle".into()).unwrap();
    assert_eq!(inner.opencode_state().variant.as_deref(), Some("default"));
    inner.start_prompt(text_prompt("again"));
    let jobs = sent(&mut inner);
    assert_eq!(jobs[0].path, "/api/session/ses_1/model");
    assert_eq!(
        jobs[0].body,
        Some(json!({ "model": { "id": "big-pickle", "providerID": "opencode" } }))
    );
    assert_eq!(jobs[1].path, "/api/session/ses_1/prompt");
}

#[test]
fn a_v2_turn_streams_its_reply_runs_its_calls_asks_and_ends() {
    let mut inner = chat(Dialect::V2, Some("ask"));
    start_v2(&mut inner);
    inner.start_prompt(text_prompt("Do these steps in order"));
    reply(
        &mut inner,
        "/api/session",
        json!({ "data": { "id": "ses_e03fb86dcffeF6W0A8x4PsPmHt" } }),
    );
    sent(&mut inner);
    let mut asked = Vec::new();
    feed(&mut inner, V2_TURN, |inner| {
        let permission = inner.state.permission.clone().unwrap();
        asked.push(permission.title.clone());
        answer(inner, Some(ONCE.into())).unwrap();
        let job = sent(inner).pop().unwrap();
        assert!(job.path.contains("/permission/per_"), "{}", job.path);
        assert_eq!(job.body, Some(json!({ "decision": "once" })));
    });
    assert_eq!(asked.len(), 7, "{asked:?}");
    assert!(asked.contains(&"uname -s > os.txt && cat os.txt".to_owned()));
    assert!(asked.contains(&"Start a sub-agent: general".to_owned()));
    // The turn ended: the chat is ready, with no ask open.
    assert_eq!(inner.state.status, ChatStatus::Ready);
    assert!(inner.state.permission.is_none());
    assert_eq!(
        inner.state.title.as_deref(),
        Some("Sequential os.txt command and hello.txt edits")
    );
    assert_eq!(replies(&inner).last(), Some(&"All steps are done."));
    assert!(inner.state.entries.iter().any(|entry| matches!(
        entry,
        ChatEntry::Thought {
            ended_ms: Some(_),
            ..
        }
    )));

    let shell = tool(&inner, "shell");
    assert_eq!(shell.kind, "execute");
    assert_eq!(shell.title, "uname -s > os.txt && cat os.txt");
    assert_eq!(shell.output.as_deref(), Some("Linux\n"));
    assert_eq!(shell.status, ChatToolStatus::Completed);

    let edit = tool(&inner, "edit");
    assert_eq!(edit.title, "Edit /tmp/ocrec/f1/hello.txt");
    assert_eq!(edit.diffs[0].old_text.as_deref(), Some("hi\n"));
    assert_eq!(edit.diffs[0].new_text.as_deref(), Some("hello\n"));
    let write = tool(&inner, "write");
    assert_eq!(write.kind, "edit");
    assert!(write.title.starts_with("Write /tmp/ocrec/f1/"));

    let task = tool(&inner, "subagent");
    assert_eq!(task.title, "Read hello.txt content");
    assert_eq!(task.status, ChatToolStatus::Completed);
    let agent = &task.agents[0];
    assert_eq!(agent.agent, "general");
    assert_eq!(agent.status, ChatToolStatus::Completed);
    assert_eq!(agent.steps.len(), 1);
    assert_eq!(agent.steps[0].text, "Read /tmp/ocrec/f1/hello.txt");
    assert!(agent.result.as_deref().unwrap().contains("hello"));
    assert!(
        inner
            .state
            .entries
            .iter()
            .any(|entry| matches!(entry, ChatEntry::AgentsFinished(_)))
    );
    let usage = inner.state.usage.as_ref().unwrap();
    assert_eq!(usage.size, 200_000);
    assert!(usage.used > 0);
}

#[test]
fn a_v2_form_is_a_question_answered_by_field_key() {
    let mut inner = chat(Dialect::V2, None);
    start_v2(&mut inner);
    inner.start_prompt(text_prompt("red or blue?"));
    reply(
        &mut inner,
        "/api/session",
        json!({ "data": { "id": "ses_e03f36794ffefzvYJwB3gS7wS5" } }),
    );
    sent(&mut inner);
    let mut answered_once = false;
    feed(&mut inner, V2_QUESTION, |inner| {
        let question = inner.state.question.clone().unwrap();
        assert_eq!(question.message, "Color preference");
        assert_eq!(question.fields[0].title, "Do you prefer red or blue?");
        assert_eq!(question.fields[0].choices.len(), 2);
        let mut answers = ChatAnswers::new();
        answers.insert(
            "q0".into(),
            ChatAnswer::Choices {
                values: vec!["Blue".into()],
            },
        );
        answer_question(inner, Some(answers)).unwrap();
        let job = sent(inner).pop().unwrap();
        assert!(
            job.path
                .ends_with("/form/frm_1fc0ca10c001mmTJ86Ir51vulb/reply")
        );
        assert_eq!(job.body, Some(json!({ "answer": { "q0": "Blue" } })));
        answered_once = true;
    });
    assert!(answered_once);
    assert_eq!(inner.state.status, ChatStatus::Ready);
    assert_eq!(
        tool(&inner, "question").title,
        "Ask: Do you prefer red or blue?"
    );
}

#[test]
fn stopping_a_v2_turn_interrupts_its_session_and_the_call_stops() {
    let mut inner = chat(Dialect::V2, None);
    start_v2(&mut inner);
    inner.start_prompt(text_prompt("sleep"));
    reply(
        &mut inner,
        "/api/session",
        json!({ "data": { "id": "ses_e03f34364ffe1xKglqeF0QAuXv" } }),
    );
    sent(&mut inner);
    let split = V2_INTERRUPT.find("session.tool.failed").unwrap();
    let start = &V2_INTERRUPT[..V2_INTERRUPT[..split].rfind('\n').unwrap() + 1];
    feed(&mut inner, start, |_| {});
    assert!(can_steer(&inner));
    cancel(&mut inner);
    let job = sent(&mut inner).pop().unwrap();
    assert_eq!(
        job.path,
        "/api/session/ses_e03f34364ffe1xKglqeF0QAuXv/interrupt"
    );
    feed(&mut inner, &V2_INTERRUPT[start.len()..], |_| {});
    assert_eq!(inner.state.status, ChatStatus::Ready);
    assert_eq!(inner.state.entries.last(), Some(&ChatEntry::Cancelled));
    assert_eq!(tool(&inner, "shell").status, ChatToolStatus::Stopped);
}

#[test]
fn a_v2_steer_shows_once_taken_and_waits_its_turn_when_declined() {
    let mut inner = chat(Dialect::V2, None);
    start_v2(&mut inner);
    inner.start_prompt(text_prompt("first"));
    reply(
        &mut inner,
        "/api/session",
        json!({ "data": { "id": "ses_1" } }),
    );
    sent(&mut inner);
    assert!(!can_steer(&inner));
    super::event(
        &mut inner,
        &json!({ "type": "session.execution.started", "data": { "sessionID": "ses_1" } }),
    );
    assert!(can_steer(&inner));
    steer(&mut inner, "OpenCode", text_prompt("also this")).unwrap();
    let job = sent(&mut inner).pop().unwrap();
    assert_eq!(
        job.body,
        Some(json!({ "text": "also this", "delivery": "steer" }))
    );
    answered(&mut inner, job.id, Ok(json!({ "data": {} })));
    assert!(matches!(
        inner.state.entries.last(),
        Some(ChatEntry::User { text, .. }) if text == "also this"
    ));
    steer(&mut inner, "OpenCode", text_prompt("later")).unwrap();
    let job = sent(&mut inner).pop().unwrap();
    answered(&mut inner, job.id, Err("busy".into()));
    assert_eq!(inner.state.queued[0].text, "later");
}

#[test]
fn v1_streams_parts_shows_todos_and_sub_agents_and_answers_by_request_id() {
    let mut inner = chat(Dialect::V1, Some("ask"));
    start_v1(&mut inner);
    let model = inner.state.option("model").unwrap();
    assert_eq!(model.current, "opencode/big-pickle");
    inner.start_prompt(text_prompt("Do these steps in order"));
    let create = reply(
        &mut inner,
        "/session",
        json!({ "id": "ses_e03fa66b8ffemu6bPdQu9QQT33" }),
    );
    assert_eq!(create.body.unwrap()["permission"][0]["action"], "ask");
    let prompt = sent(&mut inner).pop().unwrap();
    assert!(
        prompt
            .path
            .starts_with("/session/ses_e03fa66b8ffemu6bPdQu9QQT33/prompt_async")
    );
    let body = prompt.body.unwrap();
    assert_eq!(body["parts"][0]["text"], "Do these steps in order");
    assert_eq!(
        body["model"],
        json!({ "providerID": "opencode", "modelID": "big-pickle" })
    );
    assert_eq!(body["agent"], "build");

    let mut permissions = 0;
    let mut questions = 0;
    feed(&mut inner, V1_TURN, |inner| {
        if inner.state.permission.is_some() {
            permissions += 1;
            answer(inner, Some(ALWAYS.into())).unwrap();
            let job = sent(inner).pop().unwrap();
            assert!(job.path.starts_with("/permission/per_"));
            assert_eq!(job.body, Some(json!({ "reply": "always" })));
        } else {
            questions += 1;
            let question = inner.state.question.clone().unwrap();
            assert_eq!(question.message, "Color");
            assert_eq!(question.fields[0].key, "0");
            let mut answers = ChatAnswers::new();
            answers.insert(
                "0".into(),
                ChatAnswer::Choices {
                    values: vec!["red".into()],
                },
            );
            answer_question(inner, Some(answers)).unwrap();
            let job = sent(inner).pop().unwrap();
            assert!(
                job.path
                    .starts_with("/question/que_1fc061e03001hsrYVoo8G3p164/reply")
            );
            assert_eq!(job.body, Some(json!({ "answers": [["red"]] })));
        }
    });
    assert_eq!((permissions, questions), (6, 1));
    assert_eq!(inner.state.status, ChatStatus::Ready);
    // The prompt OpenCode echoes is not a reply.
    assert!(
        !replies(&inner)
            .iter()
            .any(|text| text.starts_with("Do these steps"))
    );
    assert!(!replies(&inner).is_empty());

    let todos = tools(&inner)
        .into_iter()
        .filter(|tool| tool.name.as_deref() == Some("todowrite"))
        .collect::<Vec<_>>();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "Todos · 3 of 3 done");
    assert!(todos[0].output.as_deref().unwrap().starts_with("[x] "));

    let bash = tool(&inner, "bash");
    assert_eq!(bash.title, "echo %OS% > os.txt & type os.txt");
    assert_eq!(bash.status, ChatToolStatus::Completed);
    let edit = tool(&inner, "edit");
    assert_eq!(edit.diffs[0].old_text.as_deref(), Some("hi\n"));
    assert_eq!(edit.diffs[0].new_text.as_deref(), Some("hello\n"));
    let task = tool(&inner, "task");
    let agent = &task.agents[0];
    assert_eq!(agent.agent, "general");
    assert_eq!(agent.status, ChatToolStatus::Completed);
    assert_eq!(agent.result.as_deref(), Some("`hello`"));
    assert!(agent.steps[0].text.starts_with("Read "));
    assert_eq!(
        inner.state.title.as_deref(),
        Some("Ordered command, file edits, color question")
    );
}

#[test]
fn stopping_a_v1_turn_aborts_its_session_and_ends_it_cancelled() {
    let mut inner = chat(Dialect::V1, None);
    start_v1(&mut inner);
    inner.start_prompt(text_prompt("sleep"));
    reply(
        &mut inner,
        "/session",
        json!({ "id": "ses_e03f0e29dffetLWhT2JnlEVMmM" }),
    );
    sent(&mut inner);
    let split = V1_INTERRUPT.find("\"session.error\"").unwrap();
    let start = &V1_INTERRUPT[..V1_INTERRUPT[..split].rfind('\n').unwrap() + 1];
    feed(&mut inner, start, |_| {});
    assert!(inner.state.working());
    assert!(!can_steer(&inner));
    cancel(&mut inner);
    let job = sent(&mut inner).pop().unwrap();
    assert!(
        job.path
            .starts_with("/session/ses_e03f0e29dffetLWhT2JnlEVMmM/abort")
    );
    feed(&mut inner, &V1_INTERRUPT[start.len()..], |_| {});
    assert_eq!(inner.state.status, ChatStatus::Ready);
    assert!(inner.state.entries.contains(&ChatEntry::Cancelled));
    assert_eq!(tool(&inner, "bash").status, ChatToolStatus::Stopped);
}

#[test]
fn a_chat_with_a_session_resumes_it_with_compi_s_rules_and_replays_its_messages() {
    let mut inner = chat(Dialect::V2, Some("allow-all"));
    inner.state.session = Some("ses_9".into());
    start_v2(&mut inner);
    reply(
        &mut inner,
        "/api/session/ses_9",
        json!({ "data": { "id": "ses_9", "title": "Old work", "agent": "plan",
            "model": { "id": "gpt-5.5", "providerID": "openai", "variant": "low" } } }),
    );
    assert_eq!(inner.state.title.as_deref(), Some("Old work"));
    let rules = reply(&mut inner, "/api/session/ses_9", Value::Null);
    assert_eq!(rules.method, "PATCH");
    assert_eq!(
        rules.body,
        Some(json!({ "permissions": [{ "action": "*", "resource": "*", "effect": "allow" }] }))
    );
    reply(
        &mut inner,
        "/api/session/ses_9/message",
        json!({ "data": [
            { "id": "m1", "type": "user", "text": "fix it" },
            { "id": "m2", "type": "assistant", "content": [
                { "type": "reasoning", "text": "Looking." },
                { "type": "tool", "id": "c1", "name": "shell", "state": { "status": "completed",
                  "input": { "command": "ls" }, "content": [{ "type": "text", "text": "a\n" }] } },
                { "type": "text", "text": "Fixed." },
            ] },
            { "id": "m3", "type": "idle", "outcome": "interrupted" },
        ], "cursor": {} }),
    );
    assert_eq!(inner.state.status, ChatStatus::Ready);
    assert!(matches!(&inner.state.entries[0], ChatEntry::User { text, .. } if text == "fix it"));
    assert_eq!(tool(&inner, "shell").output.as_deref(), Some("a\n"));
    assert_eq!(replies(&inner), ["Fixed."]);
    assert_eq!(inner.state.entries.last(), Some(&ChatEntry::Cancelled));
    assert_eq!(
        inner.state.option("model").unwrap().current,
        "openai/gpt-5.5"
    );
    assert_eq!(inner.state.option("thought_level").unwrap().current, "low");
    assert_eq!(inner.state.option("mode").unwrap().current, "plan");
}

#[test]
fn listings_leave_out_sub_agents_and_sessions_never_prompted() {
    let listed = json!([
        { "id": "a", "title": "Fix the build", "directory": "/w", "time": { "created": 1, "updated": 5 } },
        { "id": "b", "title": "New session - 2026", "time": { "created": 2, "updated": 2 } },
        { "id": "c", "parentID": "a", "title": "child", "time": { "created": 3, "updated": 9 } },
        { "id": "d", "title": "New session - 2026", "time": { "created": 1, "updated": 7 } },
    ]);
    let sessions = sessions(&listed);
    assert_eq!(
        sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        ["d", "a"]
    );
    assert_eq!(sessions[0].title, None);
    assert_eq!(sessions[1].folder.as_deref(), Some("/w"));
}

#[test]
fn v1_without_a_configured_model_starts_on_the_one_last_picked_in_it() {
    let mut inner = chat(Dialect::V1, None);
    super::event(
        &mut inner,
        &json!({ "type": "server.connected", "properties": {} }),
    );
    reply(
        &mut inner,
        "/config/providers",
        json!({ "providers": [
            { "id": "anthropic", "models": { "claude-opus-4-5": { "name": "Claude Opus 4.5" } } },
            { "id": "openai", "models": { "gpt-6.1-sol": { "name": "GPT-6.1 Sol" } } },
        ], "default": { "anthropic": "claude-opus-4-5" } }),
    );
    reply(
        &mut inner,
        "/agent",
        json!([{ "name": "build", "mode": "primary" }]),
    );
    reply(&mut inner, "/config", json!({}));
    let state = std::env::temp_dir().join(format!("compi-opencode-state-{}", std::process::id()));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("model.json"),
        r#"{"recent":[{"providerID":"gone","modelID":"x"},{"providerID":"openai","modelID":"gpt-6.1-sol"}]}"#,
    )
    .unwrap();
    reply(
        &mut inner,
        "/path",
        json!({ "state": state.to_str().unwrap() }),
    );
    std::fs::remove_dir_all(&state).unwrap();
    assert_eq!(inner.state.status, ChatStatus::Ready);
    assert_eq!(
        inner.state.option("model").unwrap().current,
        "openai/gpt-6.1-sol"
    );
}
