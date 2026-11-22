//! Signing model providers in from the chat. A card above the message box offers it
//! when the chat's model belongs to a provider whose sign-in the agent can no longer
//! use (an expired refresh token), or when the last turn failed for want of one; then it
//! follows the sign-in the daemon runs where the chat runs (`SignInChat`): the page to
//! open, a code to paste into the message box, and how it ended. OMP only, for now.
use super::models::provider_name;
use super::*;
use compi_protocol::chat::{ChatExpiredLogin, ChatLogin, model_provider};

/// What the chat offers to sign in.
#[derive(Debug, PartialEq)]
pub(super) struct Offer<'a> {
    pub(super) provider: &'a str,
    /// The sign-in the agent lost, when that is why.
    pub(super) expired: Option<&'a ChatExpiredLogin>,
}

/// The sign-in to offer: the chat's model's provider, when the agent lost its sign-in
/// or the last turn failed for want of one.
pub(super) fn sign_in_offer(state: &ChatState) -> Option<Offer<'_>> {
    if state.harness != ChatHarness::Omp {
        return None;
    }
    let provider = state
        .option("model")
        .and_then(|model| model_provider(&model.current))?;
    if let Some(lost) = state
        .expired_logins
        .iter()
        .find(|lost| lost.provider == provider)
    {
        return Some(Offer {
            provider,
            expired: Some(lost),
        });
    }
    let failed = matches!(
        state.entries.last(),
        Some(ChatEntry::Error { message }) if auth_failure(message)
    );
    failed.then_some(Offer {
        provider,
        expired: None,
    })
}

/// Whether a turn's error says the provider wants signing in.
fn auth_failure(message: &str) -> bool {
    let message = message.to_lowercase();
    [
        "401",
        "unauthorized",
        "invalid_grant",
        "refresh token",
        "not logged in",
        "not authenticated",
        "authentication failed",
        "no api key",
        "no credentials",
        "/login",
    ]
    .iter()
    .any(|said| message.contains(said))
}

/// Where the chat's agent runs, as the card names it.
fn place(chat: &ChatContent) -> String {
    match &chat.environment {
        ContentEnvironment::Wsl { distribution } => format!("WSL ({distribution})"),
        ContentEnvironment::Host if cfg!(windows) => "Windows".to_owned(),
        ContentEnvironment::Host => "this computer".to_owned(),
    }
}

impl CompiApp {
    /// The sign-in card: the sign-in running, else one to offer; nothing otherwise.
    pub(super) fn render_chat_sign_in(
        &self,
        chat: &ChatContent,
        state: &ChatState,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let agent = format!("{} in {}", chat.harness.name(), place(chat));
        if let Some(login) = &state.login {
            return Some(self.render_sign_in_progress(chat, login, &agent, cx));
        }
        let offer = sign_in_offer(state)?;
        let name = provider_name(offer.provider);
        let (title, detail) = match offer.expired {
            Some(lost) => (
                format!("{name} sign-in expired"),
                format!(
                    "{}{}. Sign in again for {agent}.",
                    lost.account
                        .as_deref()
                        .map_or_else(String::new, |account| format!("{account}: ")),
                    lost.reason
                ),
            ),
            None => (
                format!("Sign in to {name}"),
                format!("The last reply failed: {agent} isn't signed in to {name}."),
            ),
        };
        let (id, provider) = (chat.id.clone(), offer.provider.to_owned());
        let sign_in = text_button("chat-sign-in", "Sign in")
            .primary()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.sign_in_chat(&id, provider.clone(), window, cx);
                cx.notify();
            }));
        Some(self.sign_in_card(title, detail, vec![sign_in.into_any_element()]))
    }

    fn render_sign_in_progress(
        &self,
        chat: &ChatContent,
        login: &ChatLogin,
        agent: &str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let name = provider_name(&login.provider);
        let open = login.url.clone().map(|url| {
            text_button("chat-sign-in-open", "Open sign-in page")
                .primary()
                .on_click(move |_: &ClickEvent, _, cx| cx.open_url(&url))
                .into_any_element()
        });
        let id = chat.id.clone();
        let dismiss = |label: &'static str| {
            let id = id.clone();
            text_button("chat-sign-in-dismiss", label)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    let message = ClientMessage::CancelChatSignIn { chat: id.clone() };
                    this.chat_action(&id, message, window, cx);
                    cx.notify();
                }))
                .into_any_element()
        };
        let (title, detail, buttons) = match &login.step {
            ChatLoginStep::Waiting { note } => (
                format!("Signing in to {name}"),
                note.clone().unwrap_or_else(|| {
                    if login.url.is_some() {
                        "Sign in on the page that opened, or open it here.".to_owned()
                    } else {
                        format!("Starting {agent}'s sign-in…")
                    }
                }),
                open.into_iter().chain([dismiss("Cancel")]).collect(),
            ),
            ChatLoginStep::Code { prompt } => (
                format!("Signing in to {name}"),
                format!("{prompt}: paste it in the message box below and press Enter."),
                open.into_iter().chain([dismiss("Cancel")]).collect(),
            ),
            ChatLoginStep::Done { account } => (
                match account {
                    Some(account) => format!("Signed in to {name} as {account}"),
                    None => format!("Signed in to {name}"),
                },
                format!("{agent} starts again with it."),
                vec![dismiss("Dismiss")],
            ),
            ChatLoginStep::Failed { message } => {
                let (id, provider) = (chat.id.clone(), login.provider.clone());
                let retry = text_button("chat-sign-in-retry", "Try again")
                    .primary()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.sign_in_chat(&id, provider.clone(), window, cx);
                        cx.notify();
                    }))
                    .into_any_element();
                (
                    format!("Couldn't sign in to {name}"),
                    message.clone(),
                    vec![retry, dismiss("Dismiss")],
                )
            }
        };
        self.sign_in_card(title, detail, buttons)
    }

    /// A card above the message box: a title, a line under it, then its buttons.
    fn sign_in_card(&self, title: String, detail: String, buttons: Vec<AnyElement>) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(ui_rems(8.0))
            .border_1()
            .border_color(color(colors.accent).opacity(0.5))
            .bg(color(colors.surface))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color(colors.foreground))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(buttons),
            )
            .into_any_element()
    }

    /// Starts signing `provider` in for chat `chat`'s agent.
    pub(super) fn sign_in_chat(
        &mut self,
        chat: &str,
        provider: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = ClientMessage::SignInChat {
            chat: chat.to_owned(),
            provider,
        };
        self.chat_action(chat, message, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::chat::{ChatChoice, ChatOption};

    fn state(model: &str) -> ChatState {
        ChatState {
            harness: ChatHarness::Omp,
            options: vec![ChatOption {
                id: "model".into(),
                name: "Model".into(),
                category: Some("model".into()),
                current: model.into(),
                choices: Vec::<ChatChoice>::new(),
            }],
            ..ChatState::default()
        }
    }

    #[test]
    fn the_models_provider_is_offered_when_its_sign_in_expired_or_a_turn_wanted_one() {
        let mut chat = state("anthropic/claude-opus-5-5");
        assert_eq!(sign_in_offer(&chat), None);
        // Another provider's lost sign-in is not this model's problem.
        chat.expired_logins.push(ChatExpiredLogin {
            provider: "openai-codex".into(),
            account: None,
            reason: "revoked".into(),
        });
        assert_eq!(sign_in_offer(&chat), None);
        chat.entries.push(ChatEntry::Error {
            message: "anthropic: 401 Unauthorized".into(),
        });
        assert_eq!(
            sign_in_offer(&chat),
            Some(Offer {
                provider: "anthropic",
                expired: None
            })
        );
        let lost = ChatExpiredLogin {
            provider: "anthropic".into(),
            account: Some("me@example.com".into()),
            reason: "Refresh token expired".into(),
        };
        chat.expired_logins.push(lost.clone());
        assert_eq!(
            sign_in_offer(&chat),
            Some(Offer {
                provider: "anthropic",
                expired: Some(&lost)
            })
        );
    }

    #[test]
    fn other_failures_and_other_agents_offer_nothing() {
        let mut chat = state("anthropic/claude-opus-5-5");
        chat.entries.push(ChatEntry::Error {
            message: "The model is overloaded".into(),
        });
        assert_eq!(sign_in_offer(&chat), None);
        chat.entries.push(ChatEntry::Error {
            message: "refresh token expired".into(),
        });
        chat.harness = ChatHarness::Claude;
        assert_eq!(sign_in_offer(&chat), None);
    }
}
