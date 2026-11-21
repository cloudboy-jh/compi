//! OpenCode 1.x's classic API, as recorded from `opencode-ai` 1.19
//! (`target/agent-notes/opencode-api/v1*`): a session's messages are made of parts
//! (`message.part.updated`, text streamed by `message.part.delta`), the session goes
//! busy and then idle (`session.status`, `session.idle`), and it asks through
//! `permission.asked` and `question.asked`, answered at `/permission/{id}/reply` and
//! `/question/{id}/reply`. Every request names the chat's folder (`directory`).
use super::tools::{self, Report, Text};
use super::{
    Inner, Model, Prompt, Role, ask_card, ask_permission, ask_question, chosen_model, ours,
    resolved, session_cost, session_error, step_tokens, title, turn_began, turn_ended,
};
use compi_protocol::chat::{ChatChoice, ChatEntry, ChatEvent, ChatField, ChatHarness, ChatToolStatus};
use serde_json::{Value, json};

/// The error OpenCode ends a turn with when it is stopped.
const ABORTED: &str = "MessageAbortedError";

/// The models of every provider OpenCode is set up with (`/config/providers`), and each
/// provider's default model, in its order.
pub(super) fn models(answer: &Value) -> (Vec<Model>, Vec<String>) {
    let mut models = Vec::new();
    let mut defaults = Vec::new();
    for provider in answer["providers"].as_array().into_iter().flatten() {
        let Some(id) = provider["id"].as_str() else {
            continue;
        };
        if let Some(model) = answer["default"][id].as_str() {
            defaults.push(format!("{id}/{model}"));
        }
        for (key, model) in provider["models"].as_object().into_iter().flatten() {
            let model_id = model["id"].as_str().unwrap_or(key);
            models.push(Model {
                provider: id.to_owned(),
                id: model_id.to_owned(),
                name: model["name"].as_str().unwrap_or(model_id).to_owned(),
                variants: model["variants"]
                    .as_object()
                    .map(|variants| variants.keys().cloned().collect())
                    .unwrap_or_default(),
                context: model["limit"]["context"].as_u64().unwrap_or_default(),
            });
        }
    }
    (models, defaults)
}

/// The primary agents (`/agent`): the modes a session runs in.
pub(super) fn agents(answer: &Value) -> Vec<(String, String)> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter(|agent| agent["mode"] != "subagent" && agent["hidden"] != true)
        .filter_map(|agent| {
            let name = agent["name"].as_str()?;
            Some((name.to_owned(), super::capitalized(name)))
        })
        .collect()
}

/// The configured model (`provider/model`) and agent (`/config`).
pub(super) fn defaults(config: &Value) -> (Option<String>, Option<String>) {
    let text = |key: &str| {
        config[key]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    (text("model"), text("default_agent"))
}

/// The models last picked in OpenCode, newest first, as `provider/model`, from its
/// `model.json`.
pub(super) fn recent_models(file: &str) -> Vec<String> {
    let Ok(state) = serde_json::from_str::<Value>(file) else {
        return Vec::new();
    };
    state["recent"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            Some(format!(
                "{}/{}",
                model["providerID"].as_str()?,
                model["modelID"].as_str()?
            ))
        })
        .collect()
}

/// Permission rules as a session takes them.
pub(super) fn rules(rules: &[(&str, &str, &str)]) -> Value {
    rules
        .iter()
        .map(|(permission, pattern, action)| {
            json!({ "permission": permission, "pattern": pattern, "action": action })
        })
        .collect()
}

/// A prompt as `prompt_async` takes it: its text with the attached files' paths, its
/// images inline, with the settings shown and the chat's note.
pub(super) fn prompt_body(inner: &Inner, prompt: &Prompt) -> Value {
    let mut parts = vec![json!({ "type": "text", "text": text_with_files(prompt) })];
    parts.extend(prompt.images.iter().map(|image| {
        json!({
            "type": "file",
            "mime": image.mime,
            "filename": image.name,
            "url": format!("data:{};base64,{}", image.mime, image.data),
        })
    }));
    let opencode = inner.opencode_state();
    let mut body = json!({ "parts": parts });
    if let Some((provider, model)) = chosen_model(opencode) {
        body["model"] = json!({ "providerID": provider, "modelID": model });
    }
    if let Some(agent) = &opencode.agent {
        body["agent"] = agent.clone().into();
    }
    if let Some(variant) = opencode
        .variant
        .as_ref()
        .filter(|variant| *variant != super::DEFAULT_VARIANT)
    {
        body["variant"] = variant.clone().into();
    }
    if let Some(note) = inner.note() {
        body["system"] = note.into();
    }
    body
}

/// A prompt's text, then the paths of its attached files.
pub(super) fn text_with_files(prompt: &Prompt) -> String {
    let mut text = prompt.text.clone();
    if !prompt.files.is_empty() {
        if !text.trim().is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&prompt.files.join("\n"));
    }
    text
}

/// One event of the server's stream.
pub(super) fn event(inner: &mut Inner, event: &Value) {
    let properties = &event["properties"];
    let session = properties["sessionID"].as_str().unwrap_or_default();
    let root = inner.state.session.as_deref() == Some(session) && !session.is_empty();
    match event["type"].as_str().unwrap_or_default() {
        "session.created" | "session.updated" => {
            let info = &properties["info"];
            let id = info["id"].as_str().unwrap_or(session);
            if let Some(parent) = info["parentID"].as_str() {
                tools::adopt(inner, id, parent);
            }
            if inner.state.session.as_deref() == Some(id) {
                title(inner, info);
                session_cost(inner, info["cost"].as_f64());
            }
        }
        "session.status" if root => {
            if properties["status"]["type"] == "busy" && inner.state.working() {
                turn_began(inner);
            }
        }
        "session.idle" if root => turn_ended(inner, None),
        "session.idle" if ours(inner, session) => {
            tools::child_ended(inner, session, ChatToolStatus::Completed);
        }
        "session.error" if root || session.is_empty() => {
            let error = &properties["error"];
            session_error(inner, error_text(error), error["name"] == ABORTED);
        }
        "session.error" if ours(inner, session) => {
            let status = if properties["error"]["name"] == ABORTED {
                ChatToolStatus::Stopped
            } else {
                ChatToolStatus::Failed
            };
            tools::child_ended(inner, session, status);
        }
        "message.updated" => {
            let info = &properties["info"];
            if let Some(id) = info["id"].as_str() {
                let role = if info["role"] == "user" {
                    Role::User
                } else {
                    Role::Assistant
                };
                inner.opencode().roles.insert(id.to_owned(), role);
            }
        }
        "message.part.updated" => part(inner, &properties["part"], false),
        "message.part.delta" if root && properties["field"] == "text" => {
            let Some(id) = properties["partID"].as_str() else {
                return;
            };
            let Some(kind) = inner.opencode_state().parts.get(id).copied() else {
                return;
            };
            tools::delta(inner, id, kind, properties["delta"].as_str().unwrap_or_default());
        }
        "permission.asked" if ours(inner, session) => permission(inner, properties),
        "permission.replied" => {
            if let Some(id) = properties["requestID"].as_str() {
                resolved(inner, id);
            }
        }
        "question.asked" if ours(inner, session) => question(inner, properties),
        "question.replied" | "question.rejected" => {
            if let Some(id) = properties["requestID"].as_str() {
                resolved(inner, id);
            }
        }
        "todo.updated" if root => tools::todos(inner, &properties["todos"]),
        _ => {}
    }
}

/// An error as people read it: its message, else its name.
fn error_text(error: &Value) -> String {
    error["data"]["message"]
        .as_str()
        .or_else(|| error["message"].as_str())
        .or_else(|| error["name"].as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("The turn failed.")
        .to_owned()
}

/// A part of a message, live or replayed (`replay`).
fn part(inner: &mut Inner, part: &Value, replay: bool) {
    let session = part["sessionID"].as_str().unwrap_or_default();
    if !replay && !ours(inner, session) {
        return;
    }
    let root = replay || inner.state.session.as_deref() == Some(session);
    let role = part["messageID"]
        .as_str()
        .and_then(|message| inner.opencode_state().roles.get(message).copied());
    if role == Some(Role::User) {
        return;
    }
    let id = part["id"].as_str().unwrap_or_default();
    match part["type"].as_str().unwrap_or_default() {
        kind @ ("text" | "reasoning") => {
            if part["synthetic"] == true || part["ignored"] == true {
                return;
            }
            let kind = if kind == "text" { Text::Reply } else { Text::Thought };
            let text = part["text"].as_str().unwrap_or_default();
            let ended = !part["time"]["end"].is_null() || replay;
            if !root {
                if ended && kind == Text::Reply {
                    tools::child_reply(inner, session, text);
                }
                return;
            }
            inner.opencode().parts.insert(id.to_owned(), kind);
            if ended {
                tools::done(inner, id, kind, text);
            } else {
                tools::so_far(inner, id, kind, text);
            }
        }
        "tool" => {
            let state = &part["state"];
            let call = part["callID"].as_str().unwrap_or(id);
            let (status, output) = match state["status"].as_str() {
                Some("completed") => (
                    ChatToolStatus::Completed,
                    state["output"].as_str().map(str::to_owned),
                ),
                Some("error") => {
                    let error = state["error"].as_str().unwrap_or_default();
                    let stopped = inner.opencode_state().interrupting || error.contains("abort");
                    (
                        if stopped {
                            ChatToolStatus::Stopped
                        } else {
                            ChatToolStatus::Failed
                        },
                        Some(error.to_owned()),
                    )
                }
                Some("running") => (
                    ChatToolStatus::Running,
                    state["metadata"]["output"].as_str().map(str::to_owned),
                ),
                _ => (ChatToolStatus::Pending, None),
            };
            let report = Report {
                id: call,
                name: part["tool"].as_str().unwrap_or("tool"),
                input: &state["input"],
                metadata: &state["metadata"],
                status,
                output,
                replay,
            };
            if root {
                tools::call(inner, &report);
            } else {
                tools::child_call(inner, session, &report);
            }
        }
        "step-finish" if root && !replay => step_tokens(inner, &part["tokens"]),
        _ => {}
    }
}

/// OpenCode asks permission to run a call.
fn permission(inner: &mut Inner, asked: &Value) {
    let (Some(id), Some(session)) = (asked["id"].as_str(), asked["sessionID"].as_str()) else {
        return;
    };
    let metadata = &asked["metadata"];
    let patterns: Vec<&str> = asked["patterns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let patterns = patterns.join(", ");
    let text = |key: &str| metadata[key].as_str().filter(|text| !text.is_empty());
    let permission = asked["permission"].as_str().unwrap_or("tool");
    let title = match permission {
        "bash" => text("command").map_or_else(|| patterns.clone(), str::to_owned),
        "edit" => format!("Edit {}", text("filepath").unwrap_or(&patterns)),
        "read" => format!("Read {patterns}"),
        "task" => format!(
            "Start a sub-agent: {}",
            text("description").unwrap_or(&patterns)
        ),
        "webfetch" => format!("Fetch {}", text("url").unwrap_or(&patterns)),
        "external_directory" => format!("Work outside the folder: {patterns}"),
        "doom_loop" => "Go on with a call it keeps repeating".to_owned(),
        other if patterns.is_empty() || patterns == "*" => format!("Use {other}"),
        other => format!("{other}: {patterns}"),
    };
    let diffs = match (text("filepath"), text("diff")) {
        (Some(path), Some(diff)) => tools::patch_diff(path, diff, false).into_iter().collect(),
        _ => Vec::new(),
    };
    let card = ask_card(inner, session, asked["tool"]["callID"].as_str());
    ask_permission(inner, id, session, card, title, diffs);
}

/// OpenCode asks the user questions (its `question` tool): a field each, its options as
/// choices, answered in order.
fn question(inner: &mut Inner, asked: &Value) {
    let (Some(id), Some(session)) = (asked["id"].as_str(), asked["sessionID"].as_str()) else {
        return;
    };
    let mut headers = Vec::new();
    let fields = asked["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, question)| {
            if let Some(header) = question["header"].as_str().filter(|text| !text.is_empty()) {
                headers.push(header.to_owned());
            }
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
            let field = ChatField {
                key: index.to_string(),
                title: question["question"].as_str().unwrap_or_default().to_owned(),
                other: !choices.is_empty() && question["custom"] != false,
                multi: question["multiple"] == true,
                choices,
                required: true,
            };
            (field, String::new())
        })
        .collect();
    let message = if headers.is_empty() {
        format!("{} asks", ChatHarness::OpenCode.name())
    } else {
        headers.join(" · ")
    };
    ask_question(inner, id, session, message, fields);
}

/// A resumed session's messages, oldest first: prompts, replies, reasoning and calls,
/// and how a turn ended when it did not finish.
pub(super) fn replay(inner: &mut Inner, messages: &Value) {
    for message in messages.as_array().into_iter().flatten() {
        let info = &message["info"];
        let parts = message["parts"].as_array().into_iter().flatten();
        if info["role"] == "user" {
            let mut texts = Vec::new();
            let mut images = Vec::new();
            for part in parts {
                match part["type"].as_str() {
                    Some("text") if part["synthetic"] != true => {
                        texts.extend(part["text"].as_str());
                    }
                    Some("file") => images.extend(
                        part["filename"]
                            .as_str()
                            .filter(|_| part["mime"].as_str().is_some_and(|mime| mime.starts_with("image/")))
                            .map(str::to_owned),
                    ),
                    _ => {}
                }
            }
            tools::replay_prompt(inner, texts.join("\n"), images);
            continue;
        }
        for item in parts {
            part(inner, item, true);
        }
        inner.end_thought();
        let error = &info["error"];
        if error.is_object() {
            let entry = if error["name"] == ABORTED {
                ChatEntry::Cancelled
            } else {
                ChatEntry::Error {
                    message: error_text(error),
                }
            };
            inner.emit(ChatEvent::Entry { entry });
        }
    }
}
