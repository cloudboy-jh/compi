//! Model providers from the chat, for every agent. `/providers` lists the providers the
//! chat's agent can sign in to, with the accounts it keeps and buttons to sign in, add
//! an account or sign out; `/login` and `/logout` do it for the chat's provider or the
//! one named. A card above the message box offers a sign-in when the chat's model
//! belongs to a provider whose sign-in the agent can no longer use, or when the last
//! turn failed for want of one; then it follows the sign-in the daemon runs where the
//! chat runs (`SignInChat`): the page to open, a code or key to paste into the message
//! box, and how it ended. `/update` updates the agent, and its card says how it went.
use super::models::provider_name;
use super::*;
use compi_protocol::chat::{
    ChatAccount, ChatAgentUpdate, ChatLogin, ChatProvider, ChatUpdateStep, model_provider,
};

/// What the chat offers to sign in.
#[derive(Debug, PartialEq)]
pub(super) struct Offer<'a> {
    pub(super) provider: &'a str,
    /// The account the agent can no longer use, when that is why.
    pub(super) expired: Option<&'a ChatAccount>,
}

/// The provider the chat's model comes from: its model's provider, else the agent's
/// only one.
pub(super) fn chat_provider(state: &ChatState) -> Option<&str> {
    if let Some(provider) = state
        .option("model")
        .and_then(|model| model_provider(&model.current))
    {
        return Some(provider);
    }
    match (state.providers.as_deref(), state.harness) {
        (Some([only]), _) => Some(&only.id),
        (_, ChatHarness::Claude) => Some("anthropic"),
        (_, ChatHarness::Codex) => Some("openai"),
        _ => None,
    }
}

/// The sign-in to offer: the chat's provider, when the agent lost its sign-in or the
/// last turn failed for want of one.
pub(super) fn sign_in_offer(state: &ChatState) -> Option<Offer<'_>> {
    let provider = chat_provider(state)?;
    let expired = state
        .providers
        .iter()
        .flatten()
        .filter(|known| known.id == provider)
        .flat_map(|known| &known.accounts)
        .find(|account| account.expired.is_some());
    if expired.is_some() {
        return Some(Offer { provider, expired });
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
        "invalid api key",
        "no api key",
        "no credentials",
        "oauth token has expired",
        "/login",
    ]
    .iter()
    .any(|said| message.contains(said))
}

/// The provider `named` names, by its ID or name, among the chat's; else `named` as an
/// ID.
fn find_provider(state: &ChatState, named: &str) -> String {
    state
        .providers
        .iter()
        .flatten()
        .find(|provider| {
            provider.id.eq_ignore_ascii_case(named)
                || provider_label(state, &provider.id).eq_ignore_ascii_case(named)
        })
        .map_or_else(|| named.to_lowercase(), |provider| provider.id.clone())
}

/// `provider`'s name, as the agent gives it or Compi knows it.
fn provider_label(state: &ChatState, provider: &str) -> String {
    state
        .providers
        .iter()
        .flatten()
        .find(|known| known.id == provider)
        .and_then(|known| known.name.clone())
        .unwrap_or_else(|| provider_name(provider))
}

/// Where the chat's agent runs, as the cards name it.
fn place(chat: &ChatContent) -> String {
    match &chat.environment {
        ContentEnvironment::Wsl { distribution } => format!("WSL ({distribution})"),
        ContentEnvironment::Host if cfg!(windows) => "Windows".to_owned(),
        ContentEnvironment::Host => "this computer".to_owned(),
    }
}

/// An account as its line says it: who, how it pays, and why it no longer works.
fn account_line(account: &ChatAccount) -> String {
    let mut line: Vec<String> = Vec::new();
    line.extend(account.label.clone());
    line.extend(account.plan.clone());
    if let Some(reason) = &account.expired {
        line.push(format!("Expired: {reason}"));
    }
    if line.is_empty() {
        "Signed in".to_owned()
    } else {
        line.join(" · ")
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
            return Some(self.render_sign_in_progress(chat, state, login, &agent, cx));
        }
        let offer = sign_in_offer(state)?;
        let name = provider_label(state, offer.provider);
        let (title, detail) = match offer.expired {
            Some(lost) => (
                format!("{name} sign-in expired"),
                format!(
                    "{}{}. Sign in again for {agent}.",
                    lost.label
                        .as_deref()
                        .map_or_else(String::new, |account| format!("{account}: ")),
                    lost.expired.as_deref().unwrap_or_default()
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
                this.sign_in_chat(&id, provider.clone(), None, window, cx);
                cx.notify();
            }));
        let id = chat.id.clone();
        let more = text_button("chat-sign-in-providers", "Providers").on_click(cx.listener(
            move |this, _: &ClickEvent, window, cx| {
                this.open_chat_providers(&id, window, cx);
                cx.notify();
            },
        ));
        Some(self.chat_card(
            title,
            detail,
            vec![sign_in.into_any_element(), more.into_any_element()],
        ))
    }

    fn render_sign_in_progress(
        &self,
        chat: &ChatContent,
        state: &ChatState,
        login: &ChatLogin,
        agent: &str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let name = provider_label(state, &login.provider);
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
        let (doing, done, failed) = if login.sign_out {
            ("Signing out of", "Signed out of", "Couldn't sign out of")
        } else {
            ("Signing in to", "Signed in to", "Couldn't sign in to")
        };
        let (title, detail, buttons) = match &login.step {
            ChatLoginStep::Waiting { note } => (
                format!("{doing} {name}"),
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
                format!("{doing} {name}"),
                format!("{prompt}: paste it in the message box below and press Enter."),
                open.into_iter().chain([dismiss("Cancel")]).collect(),
            ),
            ChatLoginStep::Done { account } => (
                match account {
                    Some(account) => format!("{done} {name} as {account}"),
                    None => format!("{done} {name}"),
                },
                format!("{agent} starts again with it."),
                vec![dismiss("Dismiss")],
            ),
            ChatLoginStep::Failed { message } => {
                let (id, provider, sign_out) =
                    (chat.id.clone(), login.provider.clone(), login.sign_out);
                let retry = text_button("chat-sign-in-retry", "Try again")
                    .primary()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if sign_out {
                            this.sign_out_chat(&id, provider.clone(), None, window, cx);
                        } else {
                            this.sign_in_chat(&id, provider.clone(), None, window, cx);
                        }
                        cx.notify();
                    }))
                    .into_any_element();
                (
                    format!("{failed} {name}"),
                    message.clone(),
                    vec![retry, dismiss("Dismiss")],
                )
            }
        };
        self.chat_card(title, detail, buttons)
    }

    /// The update card while the daemon shows the chat's agent's update and the user
    /// hasn't dismissed it.
    pub(super) fn render_chat_update(
        &self,
        chat: &ChatContent,
        state: &ChatState,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let update = state.update.as_ref()?;
        let model = self.chat.chats.get(&chat.id)?;
        if model.update_seen.as_ref() == Some(update) {
            return None;
        }
        let agent = format!("{} in {}", chat.harness.name(), place(chat));
        let dismiss = {
            let (id, update) = (chat.id.clone(), update.clone());
            text_button("chat-update-dismiss", "Dismiss")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(model) = this.chat.chats.get_mut(&id) {
                        model.update_seen = Some(update.clone());
                    }
                    cx.notify();
                }))
                .into_any_element()
        };
        let (title, detail, buttons) = match &update.step {
            ChatUpdateStep::Running { note } => (
                format!("Updating {agent}"),
                note.clone()
                    .unwrap_or_else(|| "Running its updater…".to_owned()),
                Vec::new(),
            ),
            ChatUpdateStep::Done => {
                let ChatAgentUpdate { from, to, .. } = update;
                let (title, detail) = match (from, to) {
                    (Some(from), Some(to)) if from != to => (
                        format!("Updated {agent}"),
                        format!("{from} → {to}. Its chats started again on their sessions."),
                    ),
                    (_, Some(to)) => (
                        format!("{agent} is up to date"),
                        format!("It has the newest version, {to}."),
                    ),
                    _ => (
                        format!("Updated {agent}"),
                        "Its chats started again on their sessions.".to_owned(),
                    ),
                };
                (title, detail, vec![dismiss])
            }
            ChatUpdateStep::Failed { message } => {
                let id = chat.id.clone();
                let retry = text_button("chat-update-retry", "Try again")
                    .primary()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let message = ClientMessage::UpdateChatAgent { chat: id.clone() };
                        this.chat_action(&id, message, window, cx);
                        cx.notify();
                    }))
                    .into_any_element();
                (
                    format!("Couldn't update {agent}"),
                    message.clone(),
                    vec![retry, dismiss],
                )
            }
        };
        Some(self.chat_card(title, detail, buttons))
    }

    /// The providers list (`/providers`), while open: each provider with its accounts and
    /// its buttons.
    pub(super) fn render_chat_providers(
        &self,
        chat: &ChatContent,
        state: &ChatState,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.chat.chats.get(&chat.id)?.providers_open {
            return None;
        }
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let agent = format!("{} in {}", chat.harness.name(), place(chat));
        let id = chat.id.clone();
        let close = text_button("chat-providers-close", "Close").on_click(cx.listener(
            move |this, _: &ClickEvent, _, cx| {
                if let Some(model) = this.chat.chats.get_mut(&id) {
                    model.providers_open = false;
                }
                cx.notify();
            },
        ));
        let id = chat.id.clone();
        let refresh = text_button("chat-providers-refresh", "Refresh").on_click(cx.listener(
            move |this, _: &ClickEvent, window, cx| {
                let message = ClientMessage::CheckChatProviders { chat: id.clone() };
                this.chat_action(&id, message, window, cx);
                cx.notify();
            },
        ));
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color(colors.foreground))
                            .child("Model providers"),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child(format!("What {agent} can sign in to")),
                    ),
            )
            .child(refresh)
            .child(close);
        let body = match &state.providers {
            None => div()
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .text_color(muted)
                .child(format!("Asking {agent}…"))
                .into_any_element(),
            Some(providers) if providers.is_empty() => div()
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .text_color(muted)
                .child(format!(
                    "{agent} lists no providers. Sign in with /login and a provider's name."
                ))
                .into_any_element(),
            Some(providers) => div()
                .id("chat-providers-list")
                .max_h(ui_rems(280.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(providers.iter().enumerate().map(|(index, provider)| {
                    self.render_provider_row(chat, state, index, provider, cx)
                }))
                .into_any_element(),
        };
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(ui_rems(8.0))
                .border_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }

    fn render_provider_row(
        &self,
        chat: &ChatContent,
        state: &ChatState,
        index: usize,
        provider: &ChatProvider,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let signed_in = !provider.accounts.is_empty();
        let mut buttons: Vec<AnyElement> = Vec::new();
        let sign_in = |label: String, method: Option<String>, button: usize| {
            let (id, provider) = (chat.id.clone(), provider.id.clone());
            text_button(("chat-provider-sign-in", index * 8 + button), label)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.sign_in_chat(&id, provider.clone(), method.clone(), window, cx);
                    cx.notify();
                }))
                .into_any_element()
        };
        if provider.methods.is_empty() {
            let label = match (signed_in, provider.adds_accounts) {
                (false, _) => "Sign in",
                (true, true) => "Add account",
                (true, false) => "Sign in again",
            };
            buttons.push(sign_in(label.to_owned(), None, 0));
        } else {
            for (button, method) in provider.methods.iter().enumerate().take(7) {
                buttons.push(sign_in(
                    method.label.clone(),
                    Some(method.id.clone()),
                    button,
                ));
            }
        }
        // One sign-out for the provider, unless its accounts sign out one by one.
        let one_by_one = provider.accounts.iter().all(|account| account.id.is_some());
        if provider.sign_out && signed_in && !one_by_one {
            buttons.push(self.sign_out_button(chat, provider, None, index * 8 + 7, cx));
        }
        let accounts: Vec<AnyElement> = if signed_in {
            provider
                .accounts
                .iter()
                .enumerate()
                .map(|(number, account)| {
                    let line = div()
                        .flex_1()
                        .min_w_0()
                        .text_color(if account.expired.is_some() {
                            color(colors.error)
                        } else {
                            muted
                        })
                        .child(account_line(account));
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(line)
                        .when(provider.sign_out && one_by_one, |row| {
                            row.child(self.sign_out_button(
                                chat,
                                provider,
                                account.id.clone(),
                                (index * 8 + 7) * 64 + number,
                                cx,
                            ))
                        })
                        .into_any_element()
                })
                .collect()
        } else {
            vec![
                div()
                    .text_color(muted)
                    .child("Not signed in")
                    .into_any_element(),
            ]
        };
        div()
            .flex()
            .items_start()
            .gap_3()
            .py_1p5()
            .when(index > 0, |row| {
                row.border_t_1().border_color(color(colors.border))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .child(
                        div()
                            .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                            .text_color(color(colors.foreground))
                            .child(provider_label(state, &provider.id)),
                    )
                    .children(accounts),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .max_w(ui_rems(360.0))
                    .gap_1()
                    .children(buttons),
            )
            .into_any_element()
    }

    fn sign_out_button(
        &self,
        chat: &ChatContent,
        provider: &ChatProvider,
        account: Option<String>,
        key: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (id, provider) = (chat.id.clone(), provider.id.clone());
        text_button(("chat-provider-sign-out", key), "Sign out")
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.sign_out_chat(&id, provider.clone(), account.clone(), window, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// A card above the message box: a title, a line under it, then its buttons.
    fn chat_card(&self, title: String, detail: String, buttons: Vec<AnyElement>) -> AnyElement {
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

    /// Opens the providers list and asks the agent for them again.
    pub(super) fn open_chat_providers(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        model.providers_open = true;
        let message = ClientMessage::CheckChatProviders {
            chat: chat.to_owned(),
        };
        self.chat_action(chat, message, window, cx);
    }

    /// `/login` and `/logout`: the provider `argument` names, else the chat's; the
    /// providers list when there is neither.
    pub(super) fn login_command(
        &mut self,
        chat: &str,
        argument: &str,
        sign_out: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self
            .chat
            .chats
            .get(chat)
            .and_then(|model| model.state.as_ref())
        else {
            return;
        };
        let provider = if argument.is_empty() {
            chat_provider(state).map(str::to_owned)
        } else {
            Some(find_provider(state, argument))
        };
        match provider {
            None => self.open_chat_providers(chat, window, cx),
            Some(provider) if sign_out => self.sign_out_chat(chat, provider, None, window, cx),
            Some(provider) => self.sign_in_chat(chat, provider, None, window, cx),
        }
    }

    /// Starts signing `provider` in for chat `chat`'s agent, the way `method` says or
    /// its own.
    pub(super) fn sign_in_chat(
        &mut self,
        chat: &str,
        provider: String,
        method: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = ClientMessage::SignInChat {
            chat: chat.to_owned(),
            provider,
            method,
        };
        self.chat_action(chat, message, window, cx);
    }

    /// Signs `provider` out of chat `chat`'s agent: `account`, else all its accounts.
    fn sign_out_chat(
        &mut self,
        chat: &str,
        provider: String,
        account: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = ClientMessage::SignOutChat {
            chat: chat.to_owned(),
            provider,
            account,
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

    fn provider(id: &str, accounts: Vec<ChatAccount>) -> ChatProvider {
        ChatProvider {
            id: id.into(),
            name: None,
            accounts,
            methods: Vec::new(),
            sign_out: false,
            adds_accounts: true,
        }
    }

    #[test]
    fn the_models_provider_is_offered_when_its_sign_in_expired_or_a_turn_wanted_one() {
        let mut chat = state("anthropic/claude-opus-5-5");
        assert_eq!(sign_in_offer(&chat), None);
        // Another provider's lost sign-in is not this model's problem.
        let revoked = ChatAccount {
            expired: Some("revoked".into()),
            ..ChatAccount::default()
        };
        chat.providers = Some(vec![provider("openai-codex", vec![revoked])]);
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
        let lost = ChatAccount {
            label: Some("me@example.com".into()),
            expired: Some("Refresh token expired".into()),
            ..ChatAccount::default()
        };
        chat.providers
            .as_mut()
            .unwrap()
            .push(provider("anthropic", vec![lost.clone()]));
        assert_eq!(
            sign_in_offer(&chat),
            Some(Offer {
                provider: "anthropic",
                expired: Some(&lost)
            })
        );
    }

    #[test]
    fn agents_with_one_provider_offer_it_when_a_turn_wanted_a_sign_in() {
        let mut chat = state("opus");
        chat.harness = ChatHarness::Claude;
        chat.entries.push(ChatEntry::Error {
            message: "Invalid API key · Please run /login".into(),
        });
        assert_eq!(
            sign_in_offer(&chat),
            Some(Offer {
                provider: "anthropic",
                expired: None
            })
        );
        chat.harness = ChatHarness::Codex;
        chat.entries.push(ChatEntry::Error {
            message: "The model is overloaded".into(),
        });
        assert_eq!(sign_in_offer(&chat), None);
    }

    #[test]
    fn a_provider_is_found_by_its_id_or_name() {
        let mut chat = state("anthropic/claude-opus-5-5");
        chat.providers = Some(vec![ChatProvider {
            name: Some("GitHub Copilot".into()),
            ..provider("github-copilot", Vec::new())
        }]);
        assert_eq!(find_provider(&chat, "GitHub Copilot"), "github-copilot");
        assert_eq!(find_provider(&chat, "github-copilot"), "github-copilot");
        assert_eq!(find_provider(&chat, "Moonshot"), "moonshot");
    }
}
