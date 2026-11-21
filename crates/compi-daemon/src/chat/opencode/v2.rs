//! OpenCode 2.x's `/api`, as recorded from `@opencode/cli` 2.0.27
//! (`target/agent-notes/opencode-api/v2*`): a session's turn is an execution
//! (`session.execution.started`, then `.succeeded`, `.failed` or `.interrupted`), made
//! of steps that stream text and reasoning by message and block
//! (`session.text.delta`, `session.reasoning.delta`) and report calls
//! (`session.tool.*`, named once by `session.tool.input.started`). It asks permission
//! with `permission.asked` and questions with forms (`form.created`), answered under the
//! session.
use super::tools::{self, Report, Text};
use super::{
    Inner, Model, Prompt, ask_card, ask_permission, ask_question, ours, resolved, session_cost,
    session_error, step_tokens, title, turn_began, turn_ended,
};
use compi_protocol::chat::{ChatChoice, ChatEntry, ChatEvent, ChatField, ChatHarness, ChatToolStatus};
use serde_json::{Map, Value, json};

/// The models of every provider OpenCode is set up with (`/api/model`).
pub(super) fn models(answer: &Value) -> Vec<Model> {
    answer["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model["id"].as_str()?;
            Some(Model {
                provider: model["providerID"].as_str()?.to_owned(),
                id: id.to_owned(),
                name: model["name"].as_str().unwrap_or(id).to_owned(),
                variants: model["variants"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|variant| variant["id"].as_str().map(str::to_owned))
                    .collect(),
                context: model["limit"]["context"].as_u64().unwrap_or_default(),
            })
        })
        .collect()
}

/// The primary agents (`/api/agent`): the modes a session runs in.
pub(super) fn agents(answer: &Value) -> Vec<(String, String)> {
    answer["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|agent| agent["mode"] != "subagent" && agent["hidden"] != true)
        .filter_map(|agent| {
            let id = agent["id"].as_str()?;
            let name = agent["name"].as_str().map_or_else(|| super::capitalized(id), str::to_owned);
            Some((id.to_owned(), name))
        })
        .collect()
}

/// OpenCode's default model (`/api/model/default`), as `provider/model`.
pub(super) fn default_model(answer: &Value) -> Option<String> {
    let model = &answer["data"];
    Some(format!(
        "{}/{}",
        model["providerID"].as_str()?,
        model["id"].as_str()?
    ))
}

/// Permission rules as a session takes them.
pub(super) fn rules(rules: &[(&str, &str, &str)]) -> Value {
    rules
        .iter()
        .map(|(action, resource, effect)| {
            json!({ "action": action, "resource": resource, "effect": effect })
        })
        .collect()
}

/// A model as a session takes it; the model's own thinking level goes unnamed.
pub(super) fn model(provider: &str, id: &str, variant: Option<&str>) -> Value {
    let mut model = json!({ "id": id, "providerID": provider });
    if let Some(variant) = variant.filter(|variant| *variant != super::DEFAULT_VARIANT) {
        model["variant"] = variant.into();
    }
    model
}

/// A prompt as `/prompt` takes it: its text with the attached files' paths, its images
/// inline, into the running turn for `delivery` `steer`.
pub(super) fn prompt_body(prompt: &Prompt, delivery: Option<&str>) -> Value {
    let mut body = json!({ "text": super::v1::text_with_files(prompt) });
    if !prompt.images.is_empty() {
        body["files"] = prompt
            .images
            .iter()
            .map(|image| {
                json!({
                    "uri": format!("data:{};base64,{}", image.mime, image.data),
                    "name": image.name,
                })
            })
            .collect();
    }
    if let Some(delivery) = delivery {
        body["delivery"] = delivery.into();
    }
    body
}

/// A form's answers by field key, each in its field's type: several choices as a list,
/// numbers and yes or no as such, else the text or choice.
pub(super) fn form_answer(values: Vec<(String, String, Vec<String>)>) -> Value {
    let mut answer = Map::new();
    for (key, kind, given) in values {
        let first = given.first().cloned().unwrap_or_default();
        let value = match kind.as_str() {
            "array" => json!(given),
            "boolean" => json!(matches!(first.trim(), "true" | "yes" | "Yes")),
            "number" | "integer" => match first.trim().parse::<f64>() {
                Ok(number) if kind == "integer" && number.fract() == 0.0 => {
                    json!(number as i64)
                }
                Ok(number) => json!(number),
                Err(_) => continue,
            },
            _ if given.is_empty() => continue,
            _ => json!(first),
        };
        answer.insert(key, value);
    }
    Value::Object(answer)
}

/// The key of a message's text or reasoning block.
fn block(data: &Value, kind: Text) -> String {
    format!(
        "{}:{}:{}",
        data["assistantMessageID"].as_str().unwrap_or_default(),
        data["ordinal"].as_i64().unwrap_or_default(),
        if kind == Text::Reply { "text" } else { "reasoning" }
    )
}

/// One event of the server's stream.
pub(super) fn event(inner: &mut Inner, event: &Value) {
    let data = &event["data"];
    let session = data["sessionID"].as_str().unwrap_or_default();
    let root = !session.is_empty() && inner.state.session.as_deref() == Some(session);
    let child = !root && ours(inner, session);
    match event["type"].as_str().unwrap_or_default() {
        "session.created" => {
            if let Some(parent) = data["parentID"].as_str() {
                tools::adopt(inner, session, parent);
            }
            if root {
                title(inner, data);
            }
        }
        "session.renamed" if root => title(inner, data),
        "session.execution.started" if root => {
            if inner.state.working() {
                turn_began(inner);
            }
        }
        "session.execution.succeeded" if root => turn_ended(inner, None),
        "session.execution.failed" if root => turn_ended(inner, Some(error_text(data))),
        "session.execution.interrupted" if root => {
            inner.opencode().interrupting = true;
            turn_ended(inner, None);
        }
        "session.execution.succeeded" if child => {
            tools::child_ended(inner, session, ChatToolStatus::Completed);
        }
        "session.execution.failed" if child => {
            tools::child_ended(inner, session, ChatToolStatus::Failed);
        }
        "session.execution.interrupted" if child => {
            tools::child_ended(inner, session, ChatToolStatus::Stopped);
        }
        "session.error" if root => session_error(inner, error_text(data), false),
        "session.text.delta" if root => {
            let delta = data["delta"].as_str().unwrap_or_default();
            tools::delta(inner, &block(data, Text::Reply), Text::Reply, delta);
        }
        "session.reasoning.delta" if root => {
            let delta = data["delta"].as_str().unwrap_or_default();
            tools::delta(inner, &block(data, Text::Thought), Text::Thought, delta);
        }
        "session.text.ended" if root => {
            let text = data["text"].as_str().unwrap_or_default();
            tools::done(inner, &block(data, Text::Reply), Text::Reply, text);
        }
        "session.reasoning.ended" if root => {
            let text = data["text"].as_str().unwrap_or_default();
            tools::done(inner, &block(data, Text::Thought), Text::Thought, text);
        }
        "session.text.ended" if child => {
            tools::child_reply(inner, session, data["text"].as_str().unwrap_or_default());
        }
        "session.tool.input.started" if root || child => {
            if let (Some(id), Some(name)) = (data["id"].as_str(), data["name"].as_str()) {
                inner
                    .opencode()
                    .calls
                    .insert(id.to_owned(), (name.to_owned(), Value::Null));
                call(inner, session, root, data, ChatToolStatus::Pending, None);
            }
        }
        "session.tool.called" if root || child => {
            if let Some((_, input)) = data["id"]
                .as_str()
                .and_then(|id| inner.opencode().calls.get_mut(id))
            {
                *input = data["input"].clone();
            }
            call(inner, session, root, data, ChatToolStatus::Running, None);
        }
        "session.tool.progress" if root || child => {
            call(inner, session, root, data, ChatToolStatus::Running, None);
        }
        "session.tool.success" if root || child => {
            let output = content_text(&data["content"]);
            call(inner, session, root, data, ChatToolStatus::Completed, output);
        }
        "session.tool.failed" if root || child => {
            let error = &data["error"];
            let status = if error["type"] == "aborted" || inner.opencode_state().interrupting {
                ChatToolStatus::Stopped
            } else {
                ChatToolStatus::Failed
            };
            let output = error["message"].as_str().map(str::to_owned);
            call(inner, session, root, data, status, output);
        }
        "session.step.ended" if root => step_tokens(inner, &data["tokens"]),
        "session.usage.updated" if root => session_cost(inner, data["cost"].as_f64()),
        "permission.asked" if root || child => permission(inner, data),
        "permission.replied" => {
            if let Some(id) = data["requestID"].as_str() {
                resolved(inner, id);
            }
        }
        "form.created" => form(inner, &data["form"]),
        "form.replied" | "form.cancelled" => {
            if let Some(id) = data["id"].as_str() {
                resolved(inner, id);
            }
        }
        _ => {}
    }
}

/// What an execution's or session's failure says.
fn error_text(data: &Value) -> String {
    [
        &data["error"]["message"],
        &data["error"]["data"]["message"],
        &data["message"],
        &data["error"],
    ]
    .into_iter()
    .find_map(|text| text.as_str().map(str::trim).filter(|text| !text.is_empty()))
    .unwrap_or("The turn failed.")
    .to_owned()
}

/// A call's text output, its blocks joined.
fn content_text(content: &Value) -> Option<String> {
    let texts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// A call's report, by the name and input it was given.
fn call(
    inner: &mut Inner,
    session: &str,
    root: bool,
    data: &Value,
    status: ChatToolStatus,
    output: Option<String>,
) {
    let Some(id) = data["id"].as_str() else {
        return;
    };
    let Some((name, input)) = inner.opencode_state().calls.get(id).cloned() else {
        return;
    };
    let report = Report {
        id,
        name: &name,
        input: &input,
        metadata: &data["metadata"],
        status,
        output,
        replay: false,
    };
    if root {
        tools::call(inner, &report);
    } else {
        tools::child_call(inner, session, &report);
    }
}

/// OpenCode asks permission to run a call.
fn permission(inner: &mut Inner, asked: &Value) {
    let (Some(id), Some(session)) = (asked["id"].as_str(), asked["sessionID"].as_str()) else {
        return;
    };
    let resources: Vec<&str> = asked["resources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let call = asked["source"]["id"].as_str();
    let card = ask_card(inner, session, call);
    // A command shows as the call put it; OpenCode splits it into its commands.
    let shown = call
        .filter(|call| inner.tools.contains_key(*call))
        .map(|call| inner.tool_call(call).title)
        .filter(|title| !title.is_empty());
    let resources = resources.join(", ");
    let action = asked["action"].as_str().unwrap_or("tool");
    let title = match action {
        "shell" | "bash" => shown.unwrap_or(resources),
        "edit" => format!("Edit {resources}"),
        "read" => format!("Read {resources}"),
        "subagent" | "task" => format!("Start a sub-agent: {resources}"),
        other if resources.is_empty() || resources == "*" => format!("Use {other}"),
        other => format!("{other}: {resources}"),
    };
    let diffs = asked["metadata"]["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            tools::patch_diff(
                file["file"].as_str()?,
                file["patch"].as_str()?,
                file["status"] == "added",
            )
        })
        .collect();
    ask_permission(inner, id, session, card, title, diffs);
}

/// OpenCode asks the user (a form, its `question` tool's): a field each, answered by
/// key in the field's type.
fn form(inner: &mut Inner, form: &Value) {
    let (Some(id), Some(session)) = (form["id"].as_str(), form["sessionID"].as_str()) else {
        return;
    };
    if !ours(inner, session) {
        return;
    }
    let mut headers = Vec::new();
    let fields = form["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|field| field["hidden"] != true)
        .filter_map(|field| {
            let key = field["key"].as_str()?;
            let text = |name: &str| field[name].as_str().filter(|text| !text.is_empty());
            let kind = field["type"].as_str().unwrap_or("string");
            let question = text("description").or_else(|| text("title")).unwrap_or(key);
            if let Some(header) = text("title").filter(|header| *header != question) {
                headers.push(header.to_owned());
            }
            let options = field["options"]
                .as_array()
                .or_else(|| field["items"]["options"].as_array());
            let choices: Vec<ChatChoice> = options
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    let value = option["value"].as_str()?.to_owned();
                    Some(ChatChoice {
                        name: option["label"].as_str().unwrap_or(&value).to_owned(),
                        value,
                        description: option["description"]
                            .as_str()
                            .filter(|text| !text.is_empty())
                            .map(str::to_owned),
                        group: None,
                    })
                })
                .collect();
            let shown = ChatField {
                key: key.to_owned(),
                title: question.to_owned(),
                other: !choices.is_empty() && field["custom"] == true,
                multi: kind == "array",
                choices,
                required: field["required"] != false,
            };
            Some((shown, kind.to_owned()))
        })
        .collect();
    let message = match form["title"].as_str() {
        _ if !headers.is_empty() => headers.join(" · "),
        Some(title) if !title.is_empty() && title != "Questions" => title.to_owned(),
        _ => format!("{} asks", ChatHarness::OpenCode.name()),
    };
    ask_question(inner, id, session, message, fields);
}

/// A resumed session's messages, oldest first: prompts, replies, reasoning and calls,
/// and how a turn ended when it did not finish.
pub(super) fn replay(inner: &mut Inner, messages: &Value) {
    for message in messages.as_array().into_iter().flatten() {
        match message["type"].as_str() {
            Some("user") => {
                let images = message["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|file| file["name"].as_str().map(str::to_owned))
                    .collect();
                let text = message["text"].as_str().unwrap_or_default().to_owned();
                tools::replay_prompt(inner, text, images);
            }
            Some("assistant") => {
                let id = message["id"].as_str().unwrap_or_default();
                for (index, item) in message["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    replay_item(inner, id, index, item);
                }
            }
            Some("idle") => {
                inner.end_thought();
                match message["outcome"].as_str() {
                    Some("interrupted") => inner.emit(ChatEvent::Entry {
                        entry: ChatEntry::Cancelled,
                    }),
                    Some("failed") => inner.emit(ChatEvent::Entry {
                        entry: ChatEntry::Error {
                            message: error_text(message),
                        },
                    }),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// One block of a replayed reply: text, reasoning or a call.
fn replay_item(inner: &mut Inner, message: &str, index: usize, item: &Value) {
    let key = format!("{message}:{index}");
    match item["type"].as_str() {
        Some("text") => {
            tools::done(inner, &key, Text::Reply, item["text"].as_str().unwrap_or_default());
        }
        Some("reasoning") => {
            tools::done(inner, &key, Text::Thought, item["text"].as_str().unwrap_or_default());
        }
        Some("tool") => {
            let (Some(id), Some(name)) = (item["id"].as_str(), item["name"].as_str()) else {
                return;
            };
            let state = &item["state"];
            let (status, output) = match state["status"].as_str() {
                Some("completed") => (ChatToolStatus::Completed, content_text(&state["content"])),
                Some("failed") => (
                    ChatToolStatus::Failed,
                    state["error"]["message"].as_str().map(str::to_owned),
                ),
                _ => (ChatToolStatus::Running, None),
            };
            tools::call(
                inner,
                &Report {
                    id,
                    name,
                    input: &state["input"],
                    metadata: &state["metadata"],
                    status,
                    output,
                    replay: true,
                },
            );
        }
        _ => {}
    }
}
