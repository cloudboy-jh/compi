//! The header's ⋯ menu (New session, Resume…, Restart) and the session list Resume…
//! opens: the harness's sessions in the chat's folder, newest first, the current one
//! checked; a click resumes one.
use super::*;

/// Seconds since the Unix epoch of an RFC 3339 time (`2026-11-08T09:30:00.5Z`,
/// `…+01:00`); `None` when it reads otherwise.
fn rfc3339_seconds(text: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = text.get(range)?;
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !matches!(text.as_bytes().get(10), Some(b'T' | b't' | b' ')) || !(1..=12).contains(&month) {
        return None;
    }
    // The zone follows the seconds and any fraction.
    let zone = text[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match zone.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let hours: i64 = zone.get(1..3)?.parse().ok()?;
            let minutes: i64 = zone.get(4..6)?.parse().ok()?;
            let offset = hours * 3600 + minutes * 60;
            if *sign == b'+' { offset } else { -offset }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// How long before `now` (seconds since the epoch) `updated` was, as the list shows it;
/// a time it cannot read shows as given.
fn updated_label(updated: &str, now: i64) -> String {
    let Some(at) = rfc3339_seconds(updated) else {
        return updated.to_owned();
    };
    let ago = (now - at).max(0);
    match ago {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{}m ago", ago / 60),
        3600..86_400 => format!("{}h ago", ago / 3600),
        86_400..2_592_000 => format!("{}d ago", ago / 86_400),
        _ => updated.get(..10).unwrap_or(updated).to_owned(),
    }
}

impl CompiApp {
    /// Opens the session list and reads the harness's sessions for it.
    fn list_chat_sessions(&mut self, chat: &str, cx: &mut Context<Self>) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        model.menu = Some(ChatMenu::Sessions);
        model.sessions = Sessions::Reading;
        let target = self.target.clone();
        let chat = chat.to_owned();
        cx.spawn(async move |this, cx| {
            let asked = chat.clone();
            let listed = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<_> { target.connect()?.chat_sessions(&asked) })()
                        .map_err(|error| error.to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(model) = this.chat.chats.get_mut(&chat) {
                    model.sessions = match listed {
                        Ok(sessions) => Sessions::Listed(sessions),
                        Err(error) => Sessions::Failed(error),
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The header's ⋯ button: its menu, or the session list it opened.
    pub(super) fn render_chat_menu(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let menu_open = model.menu == Some(ChatMenu::Header);
        let sessions_open = model.menu == Some(ChatMenu::Sessions);
        // The popover reports clicks on its trigger through `on_open_change`.
        let trigger = icon_button("chat-menu-button", IconName::Ellipsis, "Chat")
            .menu_open(menu_open || sessions_open);
        if sessions_open {
            let id = chat.id.clone();
            return controls::popover("chat-sessions", trigger, true)
                .anchor(Anchor::TopRight)
                .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
                    this.set_chat_menu(&id, ChatMenu::Sessions, *open);
                    cx.notify();
                }))
                .content(self.render_chat_sessions(chat, model, cx))
                .into_any_element();
        }
        let item =
            |label: &'static str,
             act: fn(&mut CompiApp, &str, &mut Window, &mut Context<CompiApp>)| {
                let id = chat.id.clone();
                controls::menu_item(
                    label,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        act(this, &id, window, cx);
                        cx.notify();
                    }),
                )
            };
        let id = chat.id.clone();
        controls::menu(
            "chat-menu",
            trigger,
            menu_open,
            [
                item("New session", |this, chat, window, cx| {
                    this.resume_chat(chat, None, window, cx);
                }),
                item("Resume…", |this, chat, _, cx| {
                    this.list_chat_sessions(chat, cx)
                }),
                item("Restart", |this, chat, window, cx| {
                    this.set_chat_menu(chat, ChatMenu::Header, false);
                    this.restart_chat(chat, window, cx);
                }),
            ],
        )
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.set_chat_menu(&id, ChatMenu::Header, *open);
            cx.notify();
        }))
        .into_any_element()
    }

    /// The harness's sessions in the chat's folder: title (else id), when it last
    /// changed and its messages; the current one checked.
    fn render_chat_sessions(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let note = |text: String| {
            div()
                .px_2()
                .min_h(px(26.0))
                .flex()
                .items_center()
                .text_color(muted)
                .child(text)
                .into_any_element()
        };
        let current = model
            .state
            .as_ref()
            .and_then(|state| state.session.as_deref());
        let now = i64::try_from(now_ms() / 1000).unwrap_or(i64::MAX);
        let rows: Vec<AnyElement> = match &model.sessions {
            Sessions::Reading => vec![note("Reading sessions…".to_owned())],
            Sessions::Failed(error) => vec![note(error.clone())],
            Sessions::Listed(sessions) if sessions.is_empty() => {
                vec![note("No earlier sessions in this folder".to_owned())]
            }
            Sessions::Listed(sessions) => sessions
                .iter()
                .enumerate()
                .map(|(index, session)| {
                    let here = current == Some(session.id.as_str());
                    let mut detail = session
                        .updated
                        .as_deref()
                        .map(|updated| updated_label(updated, now))
                        .unwrap_or_default();
                    if let Some(messages) = session.messages {
                        if !detail.is_empty() {
                            detail.push_str(" · ");
                        }
                        detail.push_str(&format!(
                            "{messages} message{}",
                            if messages == 1 { "" } else { "s" }
                        ));
                    }
                    let (id, session_id) = (chat.id.clone(), session.id.clone());
                    controls::menu_row(("chat-session", index), false, here, cx)
                        .gap_x_2()
                        .child(div().flex_none().w(px(12.0)).when(here, |mark| {
                            mark.child(controls::glyph(IconName::Check, muted))
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(session.title.clone().unwrap_or_else(|| session.id.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .text_color(muted)
                                .child(detail),
                        )
                        .when(!here, |row| {
                            row.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.resume_chat(&id, Some(session_id.clone()), window, cx);
                                cx.notify();
                            }))
                        })
                        .into_any_element()
                })
                .collect(),
        };
        controls::menu_surface(cx)
            .id("chat-session-list")
            .w(px(380.0))
            .max_w(px(380.0))
            .max_h(px(360.0))
            .overflow_y_scroll()
            .child(controls::menu_label_row("Resume a session", cx))
            .children(rows)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_times_read_as_ages() {
        assert_eq!(rfc3339_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_seconds("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(
            rfc3339_seconds("2000-03-01T01:00:00.250+01:00"),
            Some(951_868_800)
        );
        assert_eq!(rfc3339_seconds("yesterday"), None);
        let now = 951_868_800;
        assert_eq!(updated_label("2000-03-01T00:00:00Z", now + 30), "just now");
        assert_eq!(updated_label("2000-03-01T00:00:00Z", now + 7200), "2h ago");
        assert_eq!(
            updated_label("2000-03-01T00:00:00Z", now + 90 * 86_400),
            "2000-03-01"
        );
        assert_eq!(updated_label("soon", now), "soon");
    }
}
