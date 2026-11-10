//! The header's ⋯ menu (New session, Resume…, Restart) and the session picker Resume…
//! and `/resume` open over the pane: the harness's sessions in the chat's folder, newest
//! first under the day they last changed, kept to every word of the search, the current
//! one checked, with a preview of the highlighted one. Enter, a double click or a click
//! on the highlighted one resumes it.
use super::*;

/// The sheet's size at the default text size; it shrinks to the pane.
const SHEET_WIDTH: f32 = 880.0;
const SHEET_HEIGHT: f32 = 560.0;
/// A narrower sheet leaves the preview out.
const PREVIEW_MIN_WIDTH: f32 = 640.0;
const PREVIEW_WIDTH: f32 = 340.0;
const ROW_HEIGHT: f32 = 30.0;
/// Sessions Page Up and Page Down move the highlight by.
const PAGE_ROWS: usize = 10;
/// The most characters of a row's label and detail.
const LABEL_CHARS: usize = 160;

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

/// `bytes` as the picker shows a session's size: `512 B`, `716 KB`, `1.4 MB`.
fn size_label(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * KB;
    match bytes {
        0..KB => format!("{bytes} B"),
        KB..MB => format!("{} KB", (bytes + KB / 2) / KB),
        _ => format!("{:.1} MB", bytes as f64 / MB as f64),
    }
}

/// Seconds since the Unix epoch.
fn now_seconds() -> i64 {
    i64::try_from(now_ms() / 1000).unwrap_or(i64::MAX)
}

/// The heading a session sits under, by the day it last changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Day {
    Today,
    Yesterday,
    /// Two to six days back.
    ThisWeek,
    Earlier,
}

impl Day {
    fn title(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Yesterday => "Yesterday",
            Self::ThisWeek => "This week",
            Self::Earlier => "Earlier",
        }
    }
}

/// The day of `updated` as seen at `now` (both seconds since the epoch) in a zone
/// `offset` seconds east of UTC: local days, so just after midnight is already today. A
/// time ahead of `now` is today.
fn day_group(updated: i64, now: i64, offset: i64) -> Day {
    let day = |at: i64| (at + offset).div_euclid(86_400);
    match day(now) - day(updated) {
        ..=0 => Day::Today,
        1 => Day::Yesterday,
        2..=6 => Day::ThisWeek,
        _ => Day::Earlier,
    }
}

/// What a search looks in: `session`'s title, first prompt, last reply, folder and id,
/// lowercased.
fn search_text(session: &ChatSessionInfo) -> String {
    [
        session.title.as_deref(),
        session.first_prompt.as_deref(),
        session.last_reply.as_deref(),
        session.folder.as_deref(),
        Some(session.id.as_str()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
    .to_lowercase()
}

/// Whether `text` (from [`search_text`]) holds every word of `query`, ignoring case.
fn text_matches(text: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .all(|word| text.contains(&word.to_lowercase()))
}

/// `text` on one line, its whitespace runs single spaces, cut to [`LABEL_CHARS`].
fn one_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(LABEL_CHARS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line,
    }
}

/// A session as the picker shows it, worked out once as it is listed.
struct SessionItem {
    info: ChatSessionInfo,
    /// Its title, else its first prompt on one line, else its id.
    label: SharedString,
    /// Its first prompt on one line, beside a title.
    detail: Option<SharedString>,
    /// When it last changed, in seconds since the epoch.
    updated: Option<i64>,
    /// What a search looks in.
    text: String,
}

impl SessionItem {
    fn new(info: ChatSessionInfo) -> Self {
        let title = info
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(one_line);
        let prompt = info
            .first_prompt
            .as_deref()
            .map(one_line)
            .filter(|prompt| !prompt.is_empty());
        let (label, detail) = match (title, prompt) {
            (Some(title), prompt) => (title, prompt),
            (None, Some(prompt)) => (prompt, None),
            (None, None) => (info.id.clone(), None),
        };
        Self {
            label: label.into(),
            detail: detail.map(Into::into),
            updated: info.updated.as_deref().and_then(rfc3339_seconds),
            text: search_text(&info),
            info,
        }
    }
}

/// The harness's sessions in the chat's folder, read as the picker opens.
enum Listing {
    Reading,
    Listed(Rc<Vec<SessionItem>>),
    Failed(String),
}

/// A row of the picker's list.
#[derive(Clone, Copy)]
enum SheetRow {
    Heading(Day),
    /// By index into the listing.
    Session(usize),
}

/// The session picker over a chat pane.
pub(super) struct SessionSheet {
    search: Entity<InputState>,
    _events: Subscription,
    listing: Listing,
    /// The sessions matching the search under their days' headings.
    rows: Rc<Vec<SheetRow>>,
    /// The row of each matching session, in order.
    matches: Vec<usize>,
    /// Index into `matches`.
    highlight: usize,
    scroll: UniformListScrollHandle,
    /// Whether the sheet was last drawn wide enough for the preview.
    wide: bool,
}

impl SessionSheet {
    pub(super) fn search_focus(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }

    /// Lays the listing out again for the search, highlighting the first match.
    fn arrange(&mut self, cx: &App) {
        let mut rows = Vec::new();
        self.matches.clear();
        if let Listing::Listed(items) = &self.listing {
            let query = self.search.read(cx).value();
            let now = now_seconds();
            let offset = crate::monitor::local_offset(u64::try_from(now).unwrap_or_default());
            let mut last = None;
            for (index, item) in items.iter().enumerate() {
                if !text_matches(&item.text, &query) {
                    continue;
                }
                let day = item
                    .updated
                    .map_or(Day::Earlier, |updated| day_group(updated, now, offset));
                if last != Some(day) {
                    rows.push(SheetRow::Heading(day));
                    last = Some(day);
                }
                self.matches.push(rows.len());
                rows.push(SheetRow::Session(index));
            }
        }
        self.rows = Rc::new(rows);
        self.highlight = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    /// The highlighted session.
    fn highlighted(&self) -> Option<&SessionItem> {
        let Listing::Listed(items) = &self.listing else {
            return None;
        };
        match self.rows.get(*self.matches.get(self.highlight)?)? {
            SheetRow::Session(index) => items.get(*index),
            SheetRow::Heading(_) => None,
        }
    }

    /// Highlights match `position`, its day's heading brought into view with the first.
    fn highlight(&mut self, position: usize) {
        let Some(&row) = self.matches.get(position) else {
            return;
        };
        self.highlight = position;
        let row = if row > 0 && matches!(self.rows[row - 1], SheetRow::Heading(_)) {
            row - 1
        } else {
            row
        };
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
    }
}

impl CompiApp {
    /// Opens the session picker over chat `chat`'s pane, its search holding `query`,
    /// and reads the harness's sessions for it.
    pub(super) fn open_chat_sessions(
        &mut self,
        chat: &str,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        model.menu = None;
        let search = cx.new(|cx| {
            let mut search = InputState::new(window, cx).placeholder("Search sessions");
            search.set_value(query.to_owned(), window, cx);
            search
        });
        let id = chat.to_owned();
        let events = cx.subscribe_in(
            &search,
            window,
            move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if let Some(sheet) = this
                        .chat
                        .chats
                        .get_mut(&id)
                        .and_then(|model| model.sessions.as_mut())
                    {
                        sheet.arrange(cx);
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.resume_highlighted_session(&id, window, cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        // A menu closing hands the keyboard back as the frame ends; the next frame
        // gives it to the search.
        let handle = search.focus_handle(cx);
        window.focus(&handle, cx);
        window.on_next_frame(move |window, cx| window.focus(&handle, cx));
        model.sessions = Some(SessionSheet {
            search,
            _events: events,
            listing: Listing::Reading,
            rows: Rc::default(),
            matches: Vec::new(),
            highlight: 0,
            scroll: UniformListScrollHandle::new(),
            wide: true,
        });
        self.read_chat_sessions(chat, cx);
    }

    /// Reads the harness's sessions into chat `chat`'s picker, highlighting the newest
    /// that is not the chat's own.
    fn read_chat_sessions(&mut self, chat: &str, cx: &mut Context<Self>) {
        let Some(sheet) = self
            .chat
            .chats
            .get_mut(chat)
            .and_then(|model| model.sessions.as_mut())
        else {
            return;
        };
        sheet.listing = Listing::Reading;
        sheet.arrange(cx);
        let target = self.target.clone();
        let chat = chat.to_owned();
        cx.spawn(async move |this, cx| {
            let asked = chat.clone();
            let listed = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<_> { target.connect()?.chat_sessions(&asked) })()
                        .map(|sessions| {
                            sessions
                                .into_iter()
                                .map(SessionItem::new)
                                .collect::<Vec<_>>()
                        })
                        .map_err(|error| error.to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(model) = this.chat.chats.get_mut(&chat) else {
                    return;
                };
                let current = model
                    .state
                    .as_ref()
                    .and_then(|state| state.session.as_deref());
                let Some(sheet) = model.sessions.as_mut() else {
                    return;
                };
                sheet.listing = match listed {
                    Ok(items) => Listing::Listed(Rc::new(items)),
                    Err(error) => Listing::Failed(error),
                };
                sheet.arrange(cx);
                if let Listing::Listed(items) = &sheet.listing
                    && let Some(position) = sheet.matches.iter().position(|&row| {
                        matches!(sheet.rows[row], SheetRow::Session(index)
                            if Some(items[index].info.id.as_str()) != current)
                    })
                {
                    sheet.highlight(position);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Closes chat `chat`'s session picker, handing the keyboard back to its composer.
    fn close_chat_sessions(&mut self, chat: &str, window: &mut Window, cx: &mut App) {
        if let Some(model) = self.chat.chats.get_mut(chat) {
            model.sessions = None;
            let composer = model.composer.focus_handle(cx);
            window.focus(&composer, cx);
        }
    }

    /// Resumes the highlighted session of chat `chat`'s picker and closes it; the chat's
    /// own session only closes it.
    fn resume_highlighted_session(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        let Some(session) = model
            .sessions
            .as_ref()
            .and_then(SessionSheet::highlighted)
            .map(|item| item.info.id.clone())
        else {
            return;
        };
        let current = model
            .state
            .as_ref()
            .and_then(|state| state.session.as_ref());
        let resume = current != Some(&session);
        self.close_chat_sessions(chat, window, cx);
        if resume {
            self.resume_chat(chat, Some(session), window, cx);
        }
    }

    /// A click on row `row` of chat `chat`'s picker: highlights its session, and resumes
    /// it when it was highlighted already (the second click of a double click too).
    fn click_chat_session(
        &mut self,
        chat: &str,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sheet) = self
            .chat
            .chats
            .get_mut(chat)
            .and_then(|model| model.sessions.as_mut())
        else {
            return;
        };
        let Some(position) = sheet.matches.iter().position(|&listed| listed == row) else {
            return;
        };
        if position == sheet.highlight {
            self.resume_highlighted_session(chat, window, cx);
            return;
        }
        sheet.highlight(position);
    }

    /// Keys of chat `chat`'s open picker: Up, Down, Page Up, Page Down, Home and End move
    /// the highlight, Enter resumes it, Escape closes the picker. Whether `key` was one.
    pub(super) fn chat_sessions_key(
        &mut self,
        chat: &str,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = &key.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
            return false;
        }
        match key.key.as_str() {
            "escape" => {
                self.close_chat_sessions(chat, window, cx);
                return true;
            }
            "enter" => {
                self.resume_highlighted_session(chat, window, cx);
                return true;
            }
            _ => {}
        }
        let Some(sheet) = self
            .chat
            .chats
            .get_mut(chat)
            .and_then(|model| model.sessions.as_mut())
        else {
            return false;
        };
        let last = sheet.matches.len().saturating_sub(1);
        let position = match key.key.as_str() {
            "down" => (sheet.highlight + 1).min(last),
            "up" => sheet.highlight.saturating_sub(1),
            "pagedown" => (sheet.highlight + PAGE_ROWS).min(last),
            "pageup" => sheet.highlight.saturating_sub(PAGE_ROWS),
            "home" => 0,
            "end" => last,
            _ => return false,
        };
        sheet.highlight(position);
        true
    }

    /// The focused chat pane's session search has the keyboard.
    pub(in crate::gui::workspace) fn chat_sessions_search_has_keyboard(
        &self,
        window: &Window,
        cx: &App,
    ) -> bool {
        self.focused_chat()
            .and_then(|model| model.sessions.as_ref())
            .is_some_and(|sheet| sheet.search_focus(cx).is_focused(window))
    }

    /// Keys in the focused chat pane's session search, before the field's own: the
    /// picker's ([`Self::chat_sessions_key`]).
    pub(in crate::gui::workspace) fn chat_sessions_search_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.chat_sessions_search_has_keyboard(window, cx) {
            return false;
        }
        let Some(chat) = self
            .focused_pane()
            .and_then(|pane| self.pane_chat_content(pane))
            .map(|chat| chat.id.clone())
        else {
            return false;
        };
        self.chat_sessions_key(&chat, key, window, cx)
    }

    /// The header's ⋯ button and its menu.
    pub(super) fn render_chat_menu(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = model.menu == Some(ChatMenu::Header);
        // The menu reports clicks on its trigger through `on_open_change`.
        let trigger = icon_button("chat-menu-button", IconName::Ellipsis, "Chat").menu_open(open);
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
            open,
            [
                item("New session", |this, chat, window, cx| {
                    this.resume_chat(chat, None, window, cx);
                }),
                item("Resume…", |this, chat, window, cx| {
                    this.open_chat_sessions(chat, "", window, cx);
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

    /// The session picker over chat `chat`'s pane: a dimmed backdrop, a click on which
    /// closes it, under a sheet with the search, the list, the highlighted session's
    /// preview where there is room, and Resume, New session and Close.
    pub(super) fn render_chat_sessions(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        sheet: &SessionSheet,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let query = sheet.search.read(cx).value();
        let count = match &sheet.listing {
            Listing::Listed(items) if !query.trim().is_empty() => {
                Some(format!("{} of {}", sheet.matches.len(), items.len()))
            }
            _ => None,
        };
        let highlighted = sheet.highlighted();
        // The sheet's width as last laid out decides the preview.
        let measure = {
            let (app, id, wide) = (cx.entity().downgrade(), chat.id.clone(), sheet.wide);
            gpui::canvas(
                move |bounds, window, cx| {
                    let fits = bounds.size.width
                        >= ui_rems(PREVIEW_MIN_WIDTH).to_pixels(window.rem_size());
                    if fits != wide {
                        cx.defer(move |cx| {
                            app.update(cx, |this, cx| {
                                if let Some(sheet) = this
                                    .chat
                                    .chats
                                    .get_mut(&id)
                                    .and_then(|model| model.sessions.as_mut())
                                {
                                    sheet.wide = fits;
                                    cx.notify();
                                }
                            })
                            .ok();
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0()
        };
        let head = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .pt_1()
            .pb_2()
            .border_b_1()
            .border_color(color(colors.border))
            .child(controls::glyph(IconName::RotateCcwClock, muted))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Resume a session"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&sheet.search).appearance(true)),
            )
            .when_some(count, |head, count| {
                head.child(
                    div()
                        .flex_none()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child(count),
                )
            })
            .children(controls::kbd("escape").map(|key| {
                key.outline()
                    .flex_none()
                    .text_color(muted)
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
            }));
        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .pt_1()
                    .child(self.render_session_list(chat, model, sheet, &query, cx)),
            )
            .when_some(highlighted.filter(|_| sheet.wide), |body, item| {
                body.child(self.render_session_preview(item))
            });
        let (close, new, resume) = (chat.id.clone(), chat.id.clone(), chat.id.clone());
        let foot = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .pt_2()
            .border_t_1()
            .border_color(color(colors.border))
            .child(
                text_button("chat-sessions-new", "New session").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.close_chat_sessions(&new, window, cx);
                        this.resume_chat(&new, None, window, cx);
                        cx.notify();
                    },
                )),
            )
            .child(div().flex_1())
            .child(
                text_button("chat-sessions-close", "Close")
                    .key("escape")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.close_chat_sessions(&close, window, cx);
                        cx.notify();
                    })),
            )
            .child(
                text_button("chat-sessions-resume", "Resume")
                    .primary()
                    .key("enter")
                    .disabled_because(highlighted.is_none().then_some("Pick a session first"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.resume_highlighted_session(&resume, window, cx);
                        cx.notify();
                    })),
            );
        let id = chat.id.clone();
        // A click in the sheet keeps the keyboard on its search, not the pane behind.
        let search = sheet.search_focus(cx);
        div()
            .id("chat-sessions-backdrop")
            .absolute()
            .inset_0()
            .p_3()
            .flex()
            .justify_center()
            .items_start()
            .bg(color(colors.background).opacity(MODAL_SCRIM_OPACITY))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.close_chat_sessions(&id, window, cx);
                    cx.notify();
                }),
            )
            .child(
                controls::menu_surface(cx)
                    .id("chat-sessions")
                    .relative()
                    .w(ui_rems(SHEET_WIDTH))
                    .max_w_full()
                    .h(ui_rems(SHEET_HEIGHT))
                    .max_h_full()
                    .p_2()
                    .gap_0()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        window.focus(&search, cx);
                        cx.stop_propagation();
                    })
                    .child(measure)
                    .child(head)
                    .child(body)
                    .child(foot),
            )
            .into_any_element()
    }

    /// The picker's list, or what stands in for it: reading, a failure with Retry, no
    /// sessions, or none matching the search with Clear search.
    fn render_session_list(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        sheet: &SessionSheet,
        query: &str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let note = |text: String, action: Option<AnyElement>| {
            div()
                .flex_1()
                .p_4()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(muted)
                .child(div().text_center().child(text))
                .children(action)
                .into_any_element()
        };
        let items = match &sheet.listing {
            Listing::Reading => return note("Reading sessions…".to_owned(), None),
            Listing::Failed(error) => {
                let id = chat.id.clone();
                let retry = text_button("chat-sessions-retry", "Retry").on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.read_chat_sessions(&id, cx);
                        cx.notify();
                    },
                ));
                return note(error.clone(), Some(retry.into_any_element()));
            }
            Listing::Listed(items) if items.is_empty() => {
                return note("No earlier sessions in this folder".to_owned(), None);
            }
            Listing::Listed(_) if sheet.matches.is_empty() => {
                let id = chat.id.clone();
                let clear = text_button("chat-sessions-clear", "Clear search").on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if let Some(sheet) = this
                            .chat
                            .chats
                            .get_mut(&id)
                            .and_then(|model| model.sessions.as_mut())
                        {
                            // Setting the value reports no change.
                            sheet
                                .search
                                .update(cx, |search, cx| search.set_value("", window, cx));
                            sheet.arrange(cx);
                        }
                        cx.notify();
                    }),
                );
                return note(
                    format!("No session matches “{}”", query.trim()),
                    Some(clear.into_any_element()),
                );
            }
            Listing::Listed(items) => items.clone(),
        };
        let rows = sheet.rows.clone();
        let highlighted = sheet.matches.get(sheet.highlight).copied();
        let current = model.state.as_ref().and_then(|state| state.session.clone());
        let now = now_seconds();
        let id = chat.id.clone();
        uniform_list(
            "chat-sessions-list",
            rows.len(),
            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row| match rows[row] {
                        SheetRow::Heading(day) => div()
                            .id(("chat-sessions-heading", row))
                            .h(ui_rems(ROW_HEIGHT))
                            .px_2()
                            .pb_1()
                            .flex()
                            .items_end()
                            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child(day.title())
                            .into_any_element(),
                        SheetRow::Session(index) => {
                            let item = &items[index];
                            let lit = Some(row) == highlighted;
                            let here = current.as_deref() == Some(item.info.id.as_str());
                            // Lit rows take the accent's text color.
                            let dim = |text: gpui::Div| {
                                text.text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                                    .when(!lit, |text| text.text_color(muted))
                            };
                            let age = item
                                .info
                                .updated
                                .as_deref()
                                .map(|updated| updated_label(updated, now));
                            let id = id.clone();
                            controls::menu_row(("chat-session", row), lit, false, cx)
                                .w_full()
                                .h(ui_rems(ROW_HEIGHT))
                                .gap_x_2()
                                .child(div().flex_none().w(ui_rems(12.0)).when(here, |mark| {
                                    mark.child(
                                        gpui_component::Icon::new(IconName::Check)
                                            .with_size(Size::XSmall)
                                            .when(!lit, |check| check.text_color(muted)),
                                    )
                                }))
                                .child(div().min_w_0().truncate().child(item.label.clone()))
                                .child(
                                    dim(div().flex_1().min_w_0().truncate())
                                        .children(item.detail.clone()),
                                )
                                .when(here, |line| {
                                    line.child(dim(div().flex_none()).child("current"))
                                })
                                .children(age.map(|age| dim(div().flex_none()).child(age)))
                                .children(item.info.size.map(|size| {
                                    dim(div().flex_none().w(ui_rems(56.0)).text_right())
                                        .child(size_label(size))
                                }))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.click_chat_session(&id, row, window, cx);
                                    cx.notify();
                                }))
                                .into_any_element()
                        }
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .w_full()
        .flex_1()
        .min_h_0()
        .track_scroll(&sheet.scroll)
        .into_any_element()
    }

    /// The highlighted session in full: its label, where and when it was saved, its size,
    /// its first prompt and the end of its last reply.
    fn render_session_preview(&self, item: &SessionItem) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let fact = |name: &'static str, value: String| {
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(ui_rems(64.0))
                        .text_color(muted)
                        .child(name),
                )
                .child(div().flex_1().min_w_0().child(value))
        };
        let block = |name: &'static str, text: Option<&String>| {
            text.filter(|text| !text.trim().is_empty()).map(|text| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_color(muted)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(div().child(text.trim().to_owned()))
            })
        };
        let info = &item.info;
        let updated = info.updated.as_deref().map(|updated| {
            let age = updated_label(updated, now_seconds());
            match item
                .updated
                .and_then(|at| u64::try_from(at).ok())
                .map(crate::monitor::format_local_time)
            {
                Some(date) => format!("{age} · {}", date.get(..16).unwrap_or(&date)),
                None => age,
            }
        });
        div()
            .id("chat-session-preview")
            .flex_none()
            .w(ui_rems(PREVIEW_WIDTH))
            .ml_2()
            .pl_3()
            .pr_1()
            .py_2()
            .border_l_1()
            .border_color(color(colors.border))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
            .child(
                div()
                    .text_size(ui_rems(UI_BODY_TEXT_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(item.label.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .children(info.folder.clone().map(|folder| fact("Folder", folder)))
                    .children(updated.map(|updated| fact("Updated", updated)))
                    .children(info.size.map(|size| fact("Size", size_label(size)))),
            )
            .children(block("First prompt", info.first_prompt.as_ref()))
            .children(block("Last reply", info.last_reply.as_ref()))
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

    #[test]
    fn sessions_group_by_local_day() {
        // 2000-03-01T00:00:00Z, and a minute after.
        let midnight = 951_868_800;
        let now = midnight + 60;
        assert_eq!(day_group(midnight, now, 0), Day::Today);
        assert_eq!(day_group(now + 600, now, 0), Day::Today);
        assert_eq!(day_group(midnight - 1, now, 0), Day::Yesterday);
        assert_eq!(day_group(midnight - 86_400, now, 0), Day::Yesterday);
        assert_eq!(day_group(midnight - 86_401, now, 0), Day::ThisWeek);
        // Six days back is this week, seven is earlier.
        assert_eq!(day_group(midnight - 5 * 86_400 - 1, now, 0), Day::ThisWeek);
        assert_eq!(day_group(midnight - 6 * 86_400 - 1, now, 0), Day::Earlier);
        // An hour west of UTC it is still 23:01 the day before, which began at 01:00 UTC.
        assert_eq!(day_group(midnight - 1800, now, -3600), Day::Today);
        assert_eq!(day_group(midnight - 82_800, now, -3600), Day::Today);
        assert_eq!(day_group(midnight - 82_801, now, -3600), Day::Yesterday);
        // An hour east, midnight UTC was 01:00 local, after local midnight.
        assert_eq!(day_group(midnight - 3600, now, 3600), Day::Today);
        assert_eq!(day_group(midnight - 3601, now, 3600), Day::Yesterday);
    }

    #[test]
    fn sessions_match_every_word_anywhere() {
        let session = ChatSessionInfo {
            id: "0193-ABC".to_owned(),
            title: Some("Fix login".to_owned()),
            updated: None,
            folder: Some("/work/app".to_owned()),
            size: None,
            first_prompt: Some("The login page\nhangs".to_owned()),
            last_reply: Some("Patched the OAuth callback".to_owned()),
        };
        let text = search_text(&session);
        assert!(text_matches(&text, ""));
        assert!(text_matches(&text, "oauth"));
        assert!(text_matches(&text, "LOGIN  fix"));
        assert!(text_matches(&text, "work abc"));
        assert!(text_matches(&text, "page hangs"));
        assert!(!text_matches(&text, "login logout"));
    }
}
