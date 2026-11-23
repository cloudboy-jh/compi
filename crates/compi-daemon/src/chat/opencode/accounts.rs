//! OpenCode's providers and sign-ins, through the server the chat runs. 1.x has
//! `/provider` (with the `connected` ones), `/provider/auth` (each provider's ways in),
//! `/provider/{id}/oauth/authorize` and `…/callback`, and `/auth/{id}` for keys and
//! signing out. 2.x has `/api/integration` (each with its methods and connections),
//! `/api/integration/{id}/connect/key` and `…/connect/oauth`, whose attempt is followed
//! until it ends, and `/api/credential/{id}` to sign out.
//!
//! A browser sign-in finishes on its own (`auto`: OpenCode's own callback page gets
//! it) or asks for the code the provider shows (`code`). An API key is asked for in the
//! chat. Either way the answer goes to the server, never through a terminal.
use super::{Dialect, dialect, directory, http, location};
use crate::chat::{Chat, Inner, SignInRun, lock, login};
use compi_protocol::chat::{
    API_KEY_METHOD, ChatAccount, ChatEvent, ChatLogin, ChatLoginStep, ChatProvider,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// How long a sign-in in the browser may take.
const BROWSER_WAIT: Duration = Duration::from_secs(10 * 60);
/// How often a 2.x sign-in's attempt is asked how it goes.
const POLL: Duration = Duration::from_secs(1);
/// Before a browser method's ID: what `ChatSignInMethod::id` names it by.
const OAUTH: &str = "oauth:";

/// Where the chat's server is and how it is asked.
#[derive(Clone)]
struct Place {
    server: http::Server,
    dialect: Dialect,
    /// The query naming the chat's folder.
    query: String,
}

impl Place {
    fn of(inner: &Inner) -> Result<Self, String> {
        let server = inner
            .opencode_state()
            .server
            .clone()
            .ok_or("OpenCode is still starting; try again in a moment.")?;
        let dialect = dialect(inner);
        let query = match dialect {
            Dialect::V1 => directory(inner),
            Dialect::V2 => location(inner),
        };
        Ok(Self {
            server,
            dialect,
            query,
        })
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
        http::call(&self.server, method, path, body)
    }

    /// 2.x's integration `provider`.
    fn integration(&self, provider: &str) -> Result<Value, String> {
        let answer = self.call(
            "GET",
            &format!("/api/integration/{provider}{}", self.query),
            None,
        )?;
        Ok(answer["data"].clone())
    }
}

/// A sign-in through OpenCode's server, and what an answer to it does.
#[derive(Clone)]
pub(in crate::chat) struct SignIn {
    place: Place,
    provider: String,
    asks: Asks,
}

#[derive(Clone)]
enum Asks {
    /// Nothing: it is starting, or waits for the browser (2.x's `attempt`).
    Nothing { attempt: Option<String> },
    /// An API key.
    Key,
    /// The code the provider shows, for 1.x's method `method` or 2.x's `attempt`.
    Code {
        method: Value,
        attempt: Option<String>,
    },
}

impl SignIn {
    pub(in crate::chat) fn takes_key(&self) -> bool {
        matches!(self.asks, Asks::Key)
    }

    /// Ends it on the server too, when the server keeps it (2.x's attempt).
    pub(in crate::chat) fn cancel(self) {
        let (Asks::Nothing {
            attempt: Some(attempt),
        }
        | Asks::Code {
            attempt: Some(attempt),
            ..
        }) = self.asks
        else {
            return;
        };
        let path = format!(
            "/api/integration/{}/connect/oauth/{attempt}{}",
            self.provider, self.place.query
        );
        let _ = thread::Builder::new()
            .name("compi-opencode-sign-in".into())
            .spawn(move || self.place.call("DELETE", &path, None));
    }
}

/// Starts signing `provider` in as sign-in `run`, the way `method` says or the first
/// the server offers, a browser's before a key.
pub(in crate::chat) fn sign_in(
    chat: &Arc<Chat>,
    inner: &mut Inner,
    run: u64,
    provider: String,
    method: Option<String>,
) -> Result<(), String> {
    let place = Place::of(inner)?;
    inner.sign_in = Some(crate::chat::SignIn {
        run,
        way: SignInRun::Server(SignIn {
            place: place.clone(),
            provider: provider.clone(),
            asks: Asks::Nothing { attempt: None },
        }),
    });
    inner.emit(ChatEvent::Login {
        login: Some(ChatLogin {
            provider: provider.clone(),
            sign_out: false,
            url: None,
            step: ChatLoginStep::Waiting { note: None },
        }),
    });
    let name = provider_name(inner, &provider);
    let chat = chat.clone();
    let _ = thread::Builder::new()
        .name("compi-opencode-sign-in".into())
        .spawn(move || {
            if let Err(error) = start(&chat, run, &place, &provider, &name, method) {
                chat.sign_in_ended(Some(run), Err(error));
            }
        });
    Ok(())
}

fn start(
    chat: &Arc<Chat>,
    run: u64,
    place: &Place,
    provider: &str,
    name: &str,
    method: Option<String>,
) -> Result<(), String> {
    let method = match method {
        Some(method) => method,
        None => first_method(place, provider)?,
    };
    if method == API_KEY_METHOD {
        set_asks(chat, run, Asks::Key);
        chat.sign_in_signal(run, login::Signal::Prompt(format!("Paste your {name} API key")));
        return Ok(());
    }
    let id = method
        .strip_prefix(OAUTH)
        .ok_or_else(|| format!("OpenCode has no sign-in {method} for {name}."))?;
    let started = match place.dialect {
        Dialect::V1 => {
            let index: u64 = id
                .parse()
                .map_err(|_| format!("OpenCode has no sign-in {method} for {name}."))?;
            let answer = place.call(
                "POST",
                &format!("/provider/{provider}/oauth/authorize{}", place.query),
                Some(&json!({ "method": index })),
            )?;
            (answer, json!(index), None)
        }
        Dialect::V2 => {
            let integration = place.integration(provider)?;
            let form = integration["methods"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|known| known["id"] == id)
                .map(|known| form_defaults(&known["form"]))
                .unwrap_or_default();
            let mut body = json!({ "methodID": id });
            if !form.is_empty() {
                body["answer"] = Value::Object(form);
            }
            let answer = place.call(
                "POST",
                &format!("/api/integration/{provider}/connect/oauth{}", place.query),
                Some(&body),
            )?;
            let data = answer["data"].clone();
            let attempt = data["attemptID"].as_str().map(str::to_owned);
            set_asks(
                chat,
                run,
                Asks::Nothing {
                    attempt: attempt.clone(),
                },
            );
            (data, Value::Null, attempt)
        }
    };
    let (answer, method, attempt) = started;
    if let Some(url) = answer["url"].as_str() {
        chat.sign_in_signal(run, login::Signal::Url(url.to_owned()));
    }
    if let Some(instructions) = answer["instructions"].as_str() {
        chat.sign_in_signal(run, login::Signal::Note(instructions.to_owned()));
    }
    // 1.x says `method`, 2.x `mode`.
    let mode = answer["method"].as_str().or(answer["mode"].as_str());
    if mode == Some("code") {
        set_asks(chat, run, Asks::Code { method, attempt });
        chat.sign_in_signal(
            run,
            login::Signal::Prompt("Paste the authorization code".into()),
        );
        return Ok(());
    }
    match (place.dialect, attempt) {
        // Its callback answers once the browser has finished.
        (Dialect::V1, _) => {
            http::call_within(
                &place.server,
                "POST",
                &format!("/provider/{provider}/oauth/callback{}", place.query),
                Some(&json!({ "method": method })),
                BROWSER_WAIT,
            )?;
        }
        (Dialect::V2, Some(attempt)) => wait(chat, run, place, provider, &attempt)?,
        (Dialect::V2, None) => return Err("OpenCode started no sign-in.".into()),
    }
    chat.sign_in_ended(Some(run), Ok(None));
    Ok(())
}

/// The first way the server offers to sign `provider` in: a browser's, else a key.
fn first_method(place: &Place, provider: &str) -> Result<String, String> {
    let methods = match place.dialect {
        Dialect::V1 => {
            let auth = place.call("GET", &format!("/provider/auth{}", place.query), None)?;
            v1_methods(&auth[provider])
        }
        Dialect::V2 => v2_methods(&place.integration(provider)?["methods"]),
    };
    Ok(methods
        .into_iter()
        .map(|method| method.id)
        .find(|id| id.starts_with(OAUTH))
        .unwrap_or_else(|| API_KEY_METHOD.to_owned()))
}

/// Follows 2.x's sign-in `attempt` until it ends, or the chat's sign-in is no longer
/// `run`.
fn wait(
    chat: &Chat,
    run: u64,
    place: &Place,
    provider: &str,
    attempt: &str,
) -> Result<(), String> {
    let path = format!(
        "/api/integration/{provider}/connect/oauth/{attempt}{}",
        place.query
    );
    let mut waited = Duration::ZERO;
    while current(chat, run) {
        let answer = place.call("GET", &path, None)?;
        let status = &answer["data"];
        match status["status"].as_str() {
            Some("pending") if waited < BROWSER_WAIT => {
                thread::sleep(POLL);
                waited += POLL;
            }
            Some("complete") => return Ok(()),
            Some("pending") => return Err("The sign-in took too long.".into()),
            _ => {
                return Err(status["message"]
                    .as_str()
                    .unwrap_or("The sign-in didn't finish.")
                    .to_owned());
            }
        }
    }
    Ok(())
}

/// Hands sign-in `run` its answer: an API key or a code.
pub(in crate::chat) fn answer_sign_in(chat: &Arc<Chat>, run: u64, sign_in: SignIn, answer: String) {
    let chat = chat.clone();
    let _ = thread::Builder::new()
        .name("compi-opencode-sign-in".into())
        .spawn(move || {
            let result = answered(&chat, run, &sign_in, &answer);
            chat.sign_in_ended(Some(run), result.map(|()| None));
        });
}

fn answered(chat: &Chat, run: u64, sign_in: &SignIn, answer: &str) -> Result<(), String> {
    let SignIn {
        place, provider, ..
    } = sign_in;
    match (&sign_in.asks, place.dialect) {
        (Asks::Key, Dialect::V1) => place
            .call(
                "PUT",
                &format!("/auth/{provider}"),
                Some(&json!({ "type": "api", "key": answer })),
            )
            .map(drop),
        (Asks::Key, Dialect::V2) => place
            .call(
                "POST",
                &format!("/api/integration/{provider}/connect/key{}", place.query),
                Some(&json!({ "key": answer })),
            )
            .map(drop),
        (Asks::Code { method, .. }, Dialect::V1) => place
            .call(
                "POST",
                &format!("/provider/{provider}/oauth/callback{}", place.query),
                Some(&json!({ "method": method, "code": answer })),
            )
            .map(drop),
        (
            Asks::Code {
                attempt: Some(attempt),
                ..
            },
            Dialect::V2,
        ) => {
            place.call(
                "POST",
                &format!(
                    "/api/integration/{provider}/connect/oauth/{attempt}/complete{}",
                    place.query
                ),
                Some(&json!({ "code": answer })),
            )?;
            wait(chat, run, place, provider, attempt)
        }
        _ => Err("The sign-in isn't waiting for anything.".into()),
    }
}

/// Signs `provider` out as sign-in `run`: 2.x's credential `account`, else every
/// account it keeps.
pub(in crate::chat) fn sign_out(
    chat: &Arc<Chat>,
    inner: &mut Inner,
    run: u64,
    provider: String,
    account: Option<String>,
) -> Result<(), String> {
    let place = Place::of(inner)?;
    inner.sign_in = Some(crate::chat::SignIn {
        run,
        way: SignInRun::Server(SignIn {
            place: place.clone(),
            provider: provider.clone(),
            asks: Asks::Nothing { attempt: None },
        }),
    });
    let chat = chat.clone();
    let _ = thread::Builder::new()
        .name("compi-opencode-sign-out".into())
        .spawn(move || {
            let result = signed_out(&place, &provider, account);
            chat.sign_in_ended(Some(run), result.map(|()| None));
        });
    Ok(())
}

fn signed_out(place: &Place, provider: &str, account: Option<String>) -> Result<(), String> {
    if place.dialect == Dialect::V1 {
        return place
            .call("DELETE", &format!("/auth/{provider}"), None)
            .map(drop);
    }
    let accounts = match account {
        Some(account) => vec![account],
        None => place.integration(provider)?["connections"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|connection| connection["type"] == "credential")
            .filter_map(|connection| connection["id"].as_str().map(str::to_owned))
            .collect(),
    };
    if accounts.is_empty() {
        return Err("OpenCode keeps no account for it to sign out; one from a variable stays.".into());
    }
    for account in accounts {
        place.call("DELETE", &format!("/api/credential/{account}"), None)?;
    }
    Ok(())
}

/// Asks the server for its providers, off any lock, and shows them.
pub(in crate::chat) fn check_providers(chat: &Arc<Chat>) {
    let Ok(place) = Place::of(&lock(&chat.inner)) else {
        return;
    };
    let chat = chat.clone();
    let _ = thread::Builder::new()
        .name("compi-chat-providers".into())
        .spawn(move || {
            if let Ok(providers) = providers(&place) {
                chat.show_providers(providers);
            }
        });
}

/// The providers signed in, then the ones a browser signs in, in the server's order.
fn providers(place: &Place) -> Result<Vec<ChatProvider>, String> {
    let mut providers = match place.dialect {
        Dialect::V1 => {
            let listed = place.call("GET", &format!("/provider{}", place.query), None)?;
            let auth = place.call("GET", &format!("/provider/auth{}", place.query), None)?;
            v1_providers(&listed, &auth)
        }
        Dialect::V2 => {
            let listed = place.call("GET", &format!("/api/integration{}", place.query), None)?;
            v2_providers(&listed["data"])
        }
    };
    providers.sort_by_key(|provider| provider.accounts.is_empty());
    Ok(providers)
}

fn v1_providers(listed: &Value, auth: &Value) -> Vec<ChatProvider> {
    let connected: Vec<&str> = listed["connected"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    listed["all"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|provider| {
            let id = provider["id"].as_str()?;
            let methods = v1_methods(&auth[id]);
            let signed_in = connected.contains(&id);
            let browser = methods.iter().any(|method| method.id.starts_with(OAUTH));
            (signed_in || browser).then(|| ChatProvider {
                id: id.to_owned(),
                name: provider["name"].as_str().map(str::to_owned),
                accounts: signed_in
                    .then(ChatAccount::default)
                    .into_iter()
                    .collect(),
                methods,
                sign_out: true,
                adds_accounts: false,
            })
        })
        .collect()
}

/// 1.x's ways in for one provider: its browser ones by index, then a key; a provider
/// it lists none for takes a key.
fn v1_methods(auth: &Value) -> Vec<compi_protocol::chat::ChatSignInMethod> {
    let mut methods: Vec<_> = auth
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, method)| method["type"] == "oauth")
        .map(|(index, method)| {
            login::method(
                &format!("{OAUTH}{index}"),
                method["label"].as_str().unwrap_or("Sign in in the browser"),
            )
        })
        .collect();
    methods.push(login::method(API_KEY_METHOD, "API key"));
    methods
}

fn v2_providers(listed: &Value) -> Vec<ChatProvider> {
    listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|integration| {
            let id = integration["id"].as_str()?;
            let name = integration["name"].as_str();
            let methods = v2_methods(&integration["methods"]);
            let accounts: Vec<ChatAccount> = integration["connections"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|connection| ChatAccount {
                    id: (connection["type"] == "credential")
                        .then(|| connection["id"].as_str().map(str::to_owned))
                        .flatten(),
                    label: connection["label"]
                        .as_str()
                        .filter(|label| Some(*label) != name)
                        .map(str::to_owned),
                    plan: (connection["method"] == "key").then(|| "API key".to_owned()),
                    expired: None,
                })
                .collect();
            let browser = methods.iter().any(|method| method.id.starts_with(OAUTH));
            (!accounts.is_empty() || browser).then(|| ChatProvider {
                id: id.to_owned(),
                name: name.map(str::to_owned),
                accounts,
                methods,
                sign_out: true,
                adds_accounts: true,
            })
        })
        .collect()
}

/// 2.x's ways in: its browser ones, then a key; variables are set outside Compi.
fn v2_methods(methods: &Value) -> Vec<compi_protocol::chat::ChatSignInMethod> {
    let methods = methods.as_array().into_iter().flatten();
    let mut browser: Vec<_> = methods
        .clone()
        .filter(|method| method["type"] == "oauth")
        .filter_map(|method| {
            Some(login::method(
                &format!("{OAUTH}{}", method["id"].as_str()?),
                method["label"].as_str().unwrap_or("Sign in in the browser"),
            ))
        })
        .collect();
    browser.extend(
        methods
            .filter(|method| method["type"] == "key")
            .take(1)
            .map(|method| {
                login::method(
                    API_KEY_METHOD,
                    method["label"].as_str().unwrap_or("API key"),
                )
            }),
    );
    browser
}

/// The answers a 2.x method's form takes when nobody picks: each field's default, else
/// the first choice of one that needs an answer; fields that depend on another are left
/// out.
fn form_defaults(form: &Value) -> serde_json::Map<String, Value> {
    form.as_array()
        .into_iter()
        .flatten()
        .filter(|field| field["when"].is_null())
        .filter_map(|field| {
            let key = field["key"].as_str()?;
            let value = if field["default"].is_null() {
                (field["required"] == true)
                    .then(|| field["options"][0]["value"].clone())
                    .filter(|value| !value.is_null())?
            } else {
                field["default"].clone()
            };
            Some((key.to_owned(), value))
        })
        .collect()
}

/// `provider`'s name, as the providers the chat shows have it.
fn provider_name(inner: &Inner, provider: &str) -> String {
    inner
        .state
        .providers
        .iter()
        .flatten()
        .find(|known| known.id == provider)
        .and_then(|known| known.name.clone())
        .unwrap_or_else(|| provider.to_owned())
}

/// Whether the chat's sign-in is still `run`.
fn current(chat: &Chat, run: u64) -> bool {
    lock(&chat.inner)
        .sign_in
        .as_ref()
        .is_some_and(|sign_in| sign_in.run == run)
}

/// Sign-in `run` now waits for `asks`.
fn set_asks(chat: &Chat, run: u64, asks: Asks) {
    let mut inner = lock(&chat.inner);
    if let Some(crate::chat::SignIn {
        run: running,
        way: SignInRun::Server(sign_in),
    }) = inner.sign_in.as_mut()
        && *running == run
    {
        sign_in.asks = asks;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_1_lists_the_providers_signed_in_and_the_ones_a_browser_signs_in() {
        let listed = json!({
            "all": [
                { "id": "anthropic", "name": "Anthropic" },
                { "id": "groq", "name": "Groq" },
                { "id": "deepseek", "name": "DeepSeek" },
            ],
            "connected": ["deepseek"],
        });
        let auth = json!({
            "anthropic": [
                { "type": "oauth", "label": "Claude Pro/Max" },
                { "type": "api", "label": "Manually enter API Key" },
            ],
        });
        let providers = v1_providers(&listed, &auth);
        let ids: Vec<_> = providers.iter().map(|provider| provider.id.as_str()).collect();
        assert_eq!(ids, ["anthropic", "deepseek"]);
        assert_eq!(
            providers[0].methods,
            [
                login::method("oauth:0", "Claude Pro/Max"),
                login::method(API_KEY_METHOD, "API key"),
            ]
        );
        assert!(providers[0].accounts.is_empty());
        assert_eq!(providers[1].accounts.len(), 1);
    }

    #[test]
    fn opencode_2_lists_integrations_with_their_credentials_and_browser_ways_in() {
        let listed = json!([
            {
                "id": "openai", "name": "OpenAI",
                "methods": [
                    { "type": "key" },
                    { "type": "env", "names": ["OPENAI_API_KEY"] },
                    { "id": "chatgpt-browser", "type": "oauth", "label": "Sign in with ChatGPT" },
                ],
                "connections": [
                    { "type": "credential", "id": "cred_1", "label": "OpenAI", "method": "oauth" },
                ],
            },
            {
                "id": "302ai", "name": "302.AI",
                "methods": [{ "type": "key" }, { "type": "env", "names": ["302AI_API_KEY"] }],
                "connections": [],
            },
        ]);
        let providers = v2_providers(&listed);
        assert_eq!(providers.len(), 1);
        assert_eq!(
            providers[0].accounts,
            [ChatAccount {
                id: Some("cred_1".into()),
                ..ChatAccount::default()
            }]
        );
        assert_eq!(
            providers[0].methods,
            [
                login::method("oauth:chatgpt-browser", "Sign in with ChatGPT"),
                login::method(API_KEY_METHOD, "API key"),
            ]
        );
    }

    #[test]
    fn a_form_is_answered_with_its_defaults_and_first_choices() {
        let form = json!([
            {
                "key": "deploymentType", "required": true, "type": "string",
                "options": [{ "value": "github.com" }, { "value": "enterprise" }],
            },
            {
                "key": "enterpriseUrl", "required": true, "type": "string",
                "when": [{ "key": "deploymentType", "op": "eq", "value": "enterprise" }],
            },
            { "key": "server", "hidden": true, "default": "https://opencode.ai/console" },
        ]);
        let answers = form_defaults(&form);
        assert_eq!(
            Value::Object(answers),
            json!({ "deploymentType": "github.com", "server": "https://opencode.ai/console" })
        );
    }
}
