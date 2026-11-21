use super::*;
use compi_protocol::chat::{ChatDiff, ChatPicks, ChatState, ChatTool};

fn codex_chat() -> Inner {
    let mut inner = Inner {
        state: ChatState {
            harness: ChatHarness::Codex,
            status: ChatStatus::Ready,
            session: Some("t1".into()),
            ..ChatState::default()
        },
        initialized: true,
        driver: Driver::Codex(Box::default()),
        ..Inner::default()
    };
    inner.codex().thread = Some("t1".into());
    inner
}

fn note(inner: &mut Inner, method: &str, params: Value) {
    handle(inner, json!({ "method": method, "params": params }));
}

fn item(inner: &mut Inner, method: &str, item: Value) {
    note(
        inner,
        method,
        json!({ "threadId": "t1", "turnId": "u1", "item": item, "startedAtMs": 0, "completedAtMs": 0 }),
    );
}

fn sent(inner: &mut Inner) -> Vec<Value> {
    std::mem::take(&mut inner.codex().sent)
}

/// Starts a turn on `text` and has Codex name it `u1`.
fn turn(inner: &mut Inner, text: &str) {
    inner.start_prompt(Prompt {
        text: text.into(),
        files: Vec::new(),
        images: Vec::new(),
    });
    let request = sent(inner).pop().expect("turn/start");
    assert_eq!(request["method"], "turn/start");
    handle(
        inner,
        json!({ "id": request["id"], "result": { "turn": { "id": "u1", "status": "inProgress", "items": [] } } }),
    );
}

fn tool(inner: &Inner, index: usize) -> &ChatTool {
    match &inner.state.entries[index] {
        ChatEntry::Tool(tool) => tool,
        other => panic!("not a tool call: {other:?}"),
    }
}

#[test]
fn start_lists_models_opens_a_thread_on_the_picks_and_turns_send_what_changed() {
    let mut inner = Inner {
        state: ChatState {
            harness: ChatHarness::Codex,
            ..ChatState::default()
        },
        picks: ChatPicks {
            model: Some("gpt-5.5".into()),
            thought_level: Some("high".into()),
        },
        driver: Driver::Codex(Box::default()),
        ..Inner::default()
    };
    launch_args(&mut inner, Some("on-request/workspace-write"));
    start(&mut inner);
    let initialize = sent(&mut inner).pop().unwrap();
    assert_eq!(initialize["method"], "initialize");
    assert_eq!(initialize["params"]["clientInfo"]["name"], "compi");
    handle(
        &mut inner,
        json!({ "id": initialize["id"], "result": { "userAgent": "codex/0.169.0" } }),
    );
    let [initialized, list] = sent(&mut inner).try_into().unwrap();
    assert_eq!(initialized, json!({ "method": "initialized" }));
    assert_eq!(list["method"], "model/list");
    let model = |id: &str, name: &str, efforts: &[&str], default: &str| {
        json!({ "id": id, "model": id, "displayName": name, "description": "", "hidden": false,
            "isDefault": false, "defaultReasoningEffort": default,
            "supportedReasoningEfforts": efforts.iter().map(|effort| json!({ "reasoningEffort": effort, "description": "" })).collect::<Vec<_>>() })
    };
    handle(
        &mut inner,
        json!({ "id": list["id"], "result": { "data": [model("gpt-5.4", "GPT-5.4", &["low", "medium"], "medium")], "nextCursor": "p2" } }),
    );
    let page = sent(&mut inner).pop().unwrap();
    assert_eq!(page["params"]["cursor"], "p2");
    handle(
        &mut inner,
        json!({ "id": page["id"], "result": { "data": [model("gpt-5.5", "GPT-5.5", &["low", "medium", "high", "xhigh"], "medium")], "nextCursor": null } }),
    );
    let thread_start = sent(&mut inner).pop().unwrap();
    assert_eq!(thread_start["method"], "thread/start");
    assert_eq!(thread_start["params"]["model"], "gpt-5.5");
    assert_eq!(thread_start["params"]["approvalPolicy"], "on-request");
    assert_eq!(thread_start["params"]["sandbox"], "workspace-write");
    handle(
        &mut inner,
        json!({ "id": thread_start["id"], "result": {
            "thread": { "id": "t9", "name": null, "preview": "", "turns": [] },
            "model": "gpt-5.5", "reasoningEffort": "medium",
            "approvalPolicy": "on-request", "sandbox": { "type": "workspaceWrite" } } }),
    );
    assert_eq!(inner.state.session.as_deref(), Some("t9"));
    assert_eq!(inner.state.status, ChatStatus::Ready);
    assert_eq!(
        inner.state.approval_mode.as_deref(),
        Some("on-request/workspace-write")
    );
    let model_option = inner.state.option("model").expect("models");
    assert_eq!(model_option.current, "gpt-5.5");
    assert_eq!(model_option.current_name(), "GPT-5.5");
    assert_eq!(model_option.choices.len(), 2);
    // The user's reasoning effort, which the model offers, goes with the first turn.
    let level = inner.state.option("thought_level").expect("efforts");
    assert_eq!(level.current, "high");
    assert_eq!(level.choices.len(), 4);
    assert_eq!(inner.state.option("mode").unwrap().current, "default");

    set_option(&mut inner, "mode".into(), "plan".into()).unwrap();
    inner.start_prompt(Prompt {
        text: "hi".into(),
        files: vec!["/w/a.txt".into()],
        images: vec![compi_protocol::chat::ChatImage {
            name: "a.png".into(),
            mime: "image/png".into(),
            data: "AAAA".into(),
        }],
    });
    let request = sent(&mut inner).pop().unwrap();
    let params = &request["params"];
    assert_eq!(params["threadId"], "t9");
    assert_eq!(params["input"][0]["text"], "hi\n\n/w/a.txt");
    assert_eq!(params["input"][1]["url"], "data:image/png;base64,AAAA");
    assert_eq!(params["effort"], "high");
    assert!(params.get("model").is_none(), "the thread's own model");
    assert_eq!(params["collaborationMode"]["mode"], "plan");
    assert_eq!(params["collaborationMode"]["settings"]["model"], "gpt-5.5");
    assert_eq!(params["sandboxPolicy"], json!({ "type": "workspaceWrite" }));
}

#[test]
fn a_reply_and_reasoning_stream_and_reconcile_with_the_finished_items() {
    let mut inner = codex_chat();
    turn(&mut inner, "hi");
    note(
        &mut inner,
        "item/reasoning/summaryTextDelta",
        json!({ "threadId": "t1", "turnId": "u1", "itemId": "r1", "summaryIndex": 0, "delta": "Think" }),
    );
    note(
        &mut inner,
        "item/reasoning/summaryTextDelta",
        json!({ "threadId": "t1", "turnId": "u1", "itemId": "r1", "summaryIndex": 1, "delta": "More" }),
    );
    item(
        &mut inner,
        "item/completed",
        json!({ "type": "reasoning", "id": "r1", "summary": ["Think", "More"], "content": [] }),
    );
    for delta in ["Hel", "lo"] {
        note(
            &mut inner,
            "item/agentMessage/delta",
            json!({ "threadId": "t1", "turnId": "u1", "itemId": "m1", "delta": delta }),
        );
    }
    // Another thread's reply is not the chat's.
    note(
        &mut inner,
        "item/agentMessage/delta",
        json!({ "threadId": "other", "turnId": "u9", "itemId": "m9", "delta": "nope" }),
    );
    item(
        &mut inner,
        "item/completed",
        json!({ "type": "agentMessage", "id": "m1", "text": "Hello!" }),
    );
    let entries = &inner.state.entries;
    assert_eq!(entries.len(), 3, "{entries:?}");
    let ChatEntry::Thought { text, ended_ms, .. } = &entries[1] else {
        panic!("a thought")
    };
    assert_eq!(text, "Think\n\nMore");
    assert!(ended_ms.is_some());
    assert_eq!(
        entries[2],
        ChatEntry::Agent {
            text: "Hello!".into()
        }
    );
    note(
        &mut inner,
        "turn/completed",
        json!({ "threadId": "t1", "turn": { "id": "u1", "status": "completed", "items": [] } }),
    );
    assert_eq!(inner.state.status, ChatStatus::Ready);
}

#[test]
fn a_command_shows_its_output_and_fails_on_a_non_zero_exit() {
    let mut inner = codex_chat();
    turn(&mut inner, "run it");
    let command = |status: &str, output: Value, exit: Value| {
        json!({ "type": "commandExecution", "id": "c1", "command": "/bin/bash -lc 'ls missing'",
            "cwd": "/w", "commandActions": [{ "type": "unknown", "command": "ls missing" }],
            "status": status, "aggregatedOutput": output, "exitCode": exit })
    };
    item(
        &mut inner,
        "item/started",
        command("inProgress", Value::Null, Value::Null),
    );
    note(
        &mut inner,
        "item/commandExecution/outputDelta",
        json!({ "threadId": "t1", "turnId": "u1", "itemId": "c1", "delta": "ls: missing" }),
    );
    let running = tool(&inner, 1);
    assert_eq!(
        (
            running.kind.as_str(),
            running.title.as_str(),
            running.status
        ),
        ("execute", "ls missing", ChatToolStatus::Running)
    );
    assert_eq!(running.output.as_deref(), Some("ls: missing"));
    item(
        &mut inner,
        "item/completed",
        command("completed", json!("ls: missing: No such file\n"), json!(2)),
    );
    let ended = tool(&inner, 1);
    assert_eq!(ended.status, ChatToolStatus::Failed);
    assert_eq!(ended.output.as_deref(), Some("ls: missing: No such file\n"));
    assert!(ended.ended_ms.is_some());
}

#[test]
fn a_file_change_shows_each_file_as_a_diff() {
    let mut inner = codex_chat();
    turn(&mut inner, "edit");
    item(
        &mut inner,
        "item/completed",
        json!({ "type": "fileChange", "id": "f1", "status": "completed", "changes": [
            { "path": "/w/a.txt", "kind": { "type": "update", "move_path": null },
              "diff": "--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n@@ -9 +9 @@\n-x\n+y\n" },
            { "path": "/w/new.txt", "kind": { "type": "add" }, "diff": "hello\n" }
        ] }),
    );
    let edit = tool(&inner, 1);
    assert_eq!(
        (edit.kind.as_str(), edit.title.as_str()),
        ("edit", "Edit 2 files")
    );
    assert_eq!(edit.status, ChatToolStatus::Completed);
    assert_eq!(
        edit.diffs,
        [
            ChatDiff {
                path: "/w/a.txt".into(),
                old_text: Some("one\ntwo\nthree\n⋯\nx\n".into()),
                new_text: Some("one\nTWO\nthree\n⋯\ny\n".into()),
            },
            ChatDiff {
                path: "/w/new.txt".into(),
                old_text: None,
                new_text: Some("hello\n".into()),
            },
        ]
    );
}

#[test]
fn the_plan_is_one_card_per_turn_updated_in_place() {
    let mut inner = codex_chat();
    turn(&mut inner, "plan");
    let plan = |statuses: [&str; 2]| {
        json!({ "threadId": "t1", "turnId": "u1", "explanation": null, "plan": [
            { "step": "Read", "status": statuses[0] }, { "step": "Write", "status": statuses[1] }] })
    };
    note(
        &mut inner,
        "turn/plan/updated",
        plan(["inProgress", "pending"]),
    );
    note(
        &mut inner,
        "turn/plan/updated",
        plan(["completed", "inProgress"]),
    );
    assert_eq!(inner.state.entries.len(), 2);
    let card = tool(&inner, 1);
    assert_eq!(card.kind, "think");
    assert_eq!(card.title, "Plan · 1 of 2 done");
    assert_eq!(card.output.as_deref(), Some("[x] Read\n[>] Write"));
}

#[test]
fn approvals_queue_on_their_calls_and_each_answer_replies_as_codex_takes_it() {
    let mut inner = codex_chat();
    turn(&mut inner, "go");
    item(
        &mut inner,
        "item/started",
        json!({ "type": "commandExecution", "id": "c1", "command": "rm -rf build", "cwd": "/w",
            "commandActions": [], "status": "inProgress" }),
    );
    let approval = |id: u64, item: &str| {
        json!({ "id": id, "method": "item/commandExecution/requestApproval", "params": {
            "threadId": "t1", "turnId": "u1", "itemId": item, "command": "rm -rf build",
            "cwd": "/w", "startedAtMs": 0 } })
    };
    handle(&mut inner, approval(7, "c1"));
    handle(&mut inner, approval(8, "c2"));
    let permission = inner.state.permission.clone().expect("it asks");
    assert_eq!(permission.tool.as_deref(), Some("c1"));
    assert_eq!(permission.title, "rm -rf build");
    let ids: Vec<&str> = permission
        .options
        .iter()
        .map(|option| option.id.as_str())
        .collect();
    assert_eq!(ids, [ACCEPT, ACCEPT_FOR_SESSION, DECLINE]);
    assert_eq!(tool(&inner, 1).status, ChatToolStatus::Pending);
    assert!(answer(&mut inner, Some("never".into())).is_err());
    answer(&mut inner, Some(ACCEPT_FOR_SESSION.into())).unwrap();
    assert_eq!(
        sent(&mut inner),
        [json!({ "id": 7, "result": { "decision": "acceptForSession" } })]
    );
    assert_eq!(tool(&inner, 1).status, ChatToolStatus::Running);
    // The second shows once the first is answered; no answer declines it.
    let second = inner.state.permission.clone().expect("the next one");
    assert_eq!(second.tool.as_deref(), Some("c2"));
    answer(&mut inner, None).unwrap();
    assert_eq!(
        sent(&mut inner),
        [json!({ "id": 8, "result": { "decision": "decline" } })]
    );
    assert_eq!(inner.state.permission, None);

    // A question answers by question ID.
    handle(
        &mut inner,
        json!({ "id": "q", "method": "item/tool/requestUserInput", "params": {
            "threadId": "t1", "turnId": "u1", "itemId": "i1", "isBlocking": true,
            "questions": [{ "id": "drink", "header": "Drink", "question": "Tea or coffee?",
                "isOther": true, "options": [{ "label": "Tea", "description": "" }, { "label": "Coffee", "description": "" }] }] } }),
    );
    let question = inner.state.question.clone().expect("it asks");
    assert_eq!(question.message, "Drink");
    assert!(question.fields[0].other);
    answer_question(
        &mut inner,
        Some(ChatAnswers::from([(
            "drink".into(),
            ChatAnswer::Choices {
                values: vec!["Coffee".into()],
            },
        )])),
    )
    .unwrap();
    assert_eq!(
        sent(&mut inner),
        [json!({ "id": "q", "result": { "answers": { "drink": { "answers": ["Coffee"] } } } })]
    );
    assert_eq!(inner.state.question, None);

    // A request Compi does not offer is refused.
    handle(
        &mut inner,
        json!({ "id": 9, "method": "item/tool/call", "params": { "threadId": "t1" } }),
    );
    assert_eq!(sent(&mut inner)[0]["error"]["code"], -32601);

    // A stop cancels what waits, then interrupts the named turn.
    handle(&mut inner, approval(10, "c1"));
    cancel(&mut inner);
    assert_eq!(
        sent(&mut inner),
        [
            json!({ "id": 10, "result": { "decision": "cancel" } }),
            json!({ "id": 2, "method": "turn/interrupt", "params": { "threadId": "t1", "turnId": "u1" } }),
        ]
    );
    assert_eq!(inner.state.permission, None);
}

#[test]
fn an_interrupted_turn_ends_cancelled_and_a_failed_one_with_its_error() {
    let mut inner = codex_chat();
    turn(&mut inner, "stop");
    note(
        &mut inner,
        "turn/completed",
        json!({ "threadId": "t1", "turn": { "id": "u1", "status": "interrupted", "items": [] } }),
    );
    assert_eq!(inner.state.entries.last(), Some(&ChatEntry::Cancelled));
    assert_eq!(inner.state.status, ChatStatus::Ready);

    turn(&mut inner, "fail");
    note(
        &mut inner,
        "turn/completed",
        json!({ "threadId": "t1", "turn": { "id": "u1", "status": "failed", "items": [],
            "error": { "message": "Quota exceeded" } } }),
    );
    assert_eq!(
        inner.state.entries.last(),
        Some(&ChatEntry::Error {
            message: "Quota exceeded".into()
        })
    );
}

#[test]
fn a_sub_agent_shows_on_its_spawn_call_with_its_threads_calls_as_steps() {
    let mut inner = codex_chat();
    turn(&mut inner, "delegate");
    note(
        &mut inner,
        "thread/started",
        json!({ "thread": { "id": "k1", "parentThreadId": "t1", "agentNickname": "Scout", "agentRole": "explorer" } }),
    );
    item(
        &mut inner,
        "item/completed",
        json!({ "type": "collabAgentToolCall", "id": "s1", "tool": "spawnAgent", "status": "completed",
            "prompt": "Read package.json", "senderThreadId": "t1", "receiverThreadIds": ["k1"],
            "agentsStates": { "k1": { "status": "running", "message": null } } }),
    );
    note(
        &mut inner,
        "item/started",
        json!({ "threadId": "k1", "turnId": "x1", "item": { "type": "commandExecution", "id": "kc",
            "command": "cat package.json", "cwd": "/w", "commandActions": [], "status": "inProgress" } }),
    );
    let spawn = tool(&inner, 1);
    assert_eq!(
        spawn.status,
        ChatToolStatus::Running,
        "while its agent runs"
    );
    let agent = &spawn.agents[0];
    assert_eq!(
        (agent.id.as_str(), agent.agent.as_str()),
        ("Scout", "explorer")
    );
    assert_eq!(agent.steps.len(), 1);
    assert_eq!(agent.steps[0].text, "cat package.json");
    note(
        &mut inner,
        "item/completed",
        json!({ "threadId": "k1", "turnId": "x1", "item": { "type": "agentMessage", "id": "km", "text": "name: compi" } }),
    );
    note(
        &mut inner,
        "turn/completed",
        json!({ "threadId": "k1", "turn": { "id": "x1", "status": "completed", "items": [] } }),
    );
    let spawn = tool(&inner, 1);
    let agent = &spawn.agents[0];
    assert_eq!(agent.status, ChatToolStatus::Completed);
    assert_eq!(agent.result.as_deref(), Some("name: compi"));
    // A step still running when its agent ended did not finish; the card shows it so.
    assert_eq!(agent.steps[0].status, ChatToolStatus::Running);
    assert_eq!(spawn.status, ChatToolStatus::Completed);
}

#[test]
fn thread_list_pages_become_sessions_newest_first() {
    let mut inner = codex_chat();
    let (reply, receiver) = mpsc::channel();
    list_page(
        &mut inner,
        None,
        Pending::List {
            reply,
            listed: Vec::new(),
            pages: 1,
        },
    );
    let request = sent(&mut inner).pop().unwrap();
    assert_eq!(request["method"], "thread/list");
    assert_eq!(request["params"]["sortKey"], "updated_at");
    let thread = |id: &str, name: Value, preview: &str, updated: u64| {
        json!({ "id": id, "name": name, "preview": preview, "updatedAt": updated, "createdAt": 0,
            "cwd": "/w", "ephemeral": false })
    };
    handle(
        &mut inner,
        json!({ "id": request["id"], "result": { "data": [
            thread("a", json!("Fix the build"), "fix it", 86_400),
            thread("empty", Value::Null, "", 50),
        ], "nextCursor": "c2" } }),
    );
    let page = sent(&mut inner).pop().unwrap();
    assert_eq!(page["params"]["cursor"], "c2");
    handle(
        &mut inner,
        json!({ "id": page["id"], "result": { "data": [thread("b", Value::Null, "hello", 0)], "nextCursor": null } }),
    );
    let sessions = receiver.recv().unwrap().unwrap();
    let ids: Vec<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(sessions[0].title.as_deref(), Some("Fix the build"));
    assert_eq!(sessions[0].updated.as_deref(), Some("1970-01-02T00:00:00Z"));
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("fix it"));
    assert_eq!(sessions[1].folder.as_deref(), Some("/w"));
}
