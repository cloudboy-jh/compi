//! Chat panes: a conversation with an agent harness (OMP) that the daemon runs, shown in
//! a content pane or as a terminal's view. The chat (`PaneContent::Chat`) names the
//! harness, its folder and environment, and once the harness made one, its session, which
//! the pane keeps so a restarted daemon resumes it.
//!
//! The transcript lives in the daemon. While a pane shows a chat, a watcher of its own
//! (`watch_chat`, on Monitor's watching loop) keeps a connection that watches it:
//! `WatchChat` brings the whole state, then each change arrives in order. A failed
//! connection shows as reconnecting and is retried; the watcher stops [`LINGER`] after the
//! chat stops being shown. Sending, stopping, answering, settings, sessions and restarts
//! go over short connections of their own, off the UI thread, with errors shown in the
//! pane.
//!
//! Client state is per chat (`ChatModel`): the state as last reported, a row per entry
//! (`rows`: Markdown views, tool cards with their diffs, what the reader unfolded), the
//! transcript's scroll position, the composer's draft, attachments and completion, and
//! an open question's answers. It stays while the chat's pane or view is in the
//! workspace, shown or not.
use super::monitor::watch::{self, LINGER, SlotChange, WatchEnd, WatchLink, WatchSlot};
use super::*;
use crate::gui::controls::{self, IconName, icon_button, text_button};
use compi_protocol::chat::{
    ChatAnswer, ChatAnswers, ChatApproval, ChatContent, ChatDelivery, ChatEntry, ChatEvent,
    ChatHarness, ChatImage, ChatPermission, ChatQuestion, ChatSessionInfo, ChatState, ChatStatus,
};
use compi_protocol::{ContentEnvironment, ContentTab, FolderContent, GitScope, PaneContent};
use gpui::{
    AnyElement, Bounds, Element, ElementId, Entity, GlobalElementId, InspectorElementId, LayoutId,
    MouseButton, Pixels, ScrollStrategy, StyleRefinement, UniformListScrollHandle, WeakEntity, img,
    uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::spinner::Spinner;
use gpui_component::text::{TextView, TextViewState};
use gpui_component::{RopeExt as _, Sizable as _, Size};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

mod composer;
mod rows;
mod scroller;
mod sessions;

use composer::{CompiCommand, Completion, OptionPicker, QuestionForm};
use rows::{ChatRow, RowFrame};
use scroller::TranscriptScroll;
use sessions::SessionSheet;

const HEADER_HEIGHT: f32 = 34.0;
/// The composer grows with its text between these many lines.
const COMPOSER_MIN_ROWS: usize = 1;
const COMPOSER_MAX_ROWS: usize = 8;

/// `child` laid out and drawn with a rem of `rem`. A chat sizes everything in rems
/// ([`ui_rems`]), as GPUI Kit's components do, so the whole chat scales with its text size.
struct WithRemSize {
    rem: Pixels,
    child: AnyElement,
}

impl IntoElement for WithRemSize {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for WithRemSize {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = &mut self.child;
        let layout =
            window.with_rem_size(Some(self.rem), |window| child.request_layout(window, cx));
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_rem_size(Some(self.rem), |window| {
            child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_rem_size(Some(self.rem), |window| child.paint(window, cx));
    }
}

/// What a chat watcher reports, tagged with its chat and generation.
pub(in crate::gui) struct ChatUpdate {
    chat: Arc<str>,
    generation: u64,
    event: ChatWatchEvent,
}

enum ChatWatchEvent {
    /// Watching: the chat's whole state. Boxed: it is several times the size of the rest.
    Watching(Box<ChatState>),
    Changed(ChatEvent),
    /// The connection failed or ended; the watcher retries.
    Failed(String),
}

/// How a chat's watcher stands.
enum Link {
    Connecting,
    Live,
    /// Why the last connection failed; the watcher retries.
    Reconnecting(String),
}

/// The chat's menus and popovers; one is open at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ChatMenu {
    /// The header's ⋯ menu.
    Header,
    /// The picker of the harness setting with this id.
    Option(String),
    /// The other settings.
    More,
}

/// Whether the chat's folder is in a Git repository, read once: Open in diff needs one.
enum Repo {
    Reading,
    /// Its top level, and the branch checked out (`None` when detached or unborn).
    At {
        top: String,
        branch: Option<String>,
    },
    None,
}

/// One chat's client state.
struct ChatModel {
    /// What the watcher asks the daemon for: the pane's chat, its session kept current.
    watched: Arc<Mutex<ChatContent>>,
    slot: WatchSlot,
    link: Link,
    /// The chat as the daemon last reported it; `None` before its first report.
    state: Option<ChatState>,
    /// One per entry of `state`.
    rows: Rc<Vec<ChatRow>>,
    scroller: TranscriptScroll,
    composer: Entity<TextareaState>,
    _composer_events: Subscription,
    /// Keyboard focus of the pane; the composer and the transcript's text sit inside it.
    focus: FocusHandle,
    /// The composer card's: its accent ring follows the draft's focus.
    card: FocusHandle,
    /// The folder, as tool cards name paths relative to it and as the header shows it.
    folder: String,
    folder_label: SharedString,
    /// Files to attach to the next prompt: absolute paths in the chat's environment.
    attachments: Vec<String>,
    /// Images to send with the next prompt, read where they were picked.
    images: Vec<ChatImage>,
    /// The `/` command or `@` file list over the composer.
    completion: Option<Completion>,
    /// The draft Escape closed the completion on: it stays closed until the draft changes.
    dismissed: Option<SharedString>,
    /// The latest file search; earlier replies are dropped.
    file_request: u64,
    menu: Option<ChatMenu>,
    /// The search of an open setting picker with many choices.
    picker: Option<OptionPicker>,
    /// The session picker over the pane, while open.
    sessions: Option<SessionSheet>,
    repo: Repo,
    /// The answers being given to the open question.
    question: Option<QuestionForm>,
    /// Requests on their way.
    requests: usize,
    /// Why the last request failed.
    error: Option<String>,
    /// The session last sent to the daemon as the pane's.
    session_sent: Option<String>,
}

/// The chats of this window.
#[derive(Default)]
pub(in crate::gui) struct ChatUi {
    chats: HashMap<String, ChatModel>,
    /// A redraw is due for a working chat's elapsed times.
    ticking: bool,
    /// Settings → Chat → Approvals, which each watcher's next `WatchChat` passes on.
    approval: Arc<Mutex<ChatApproval>>,
}

/// Every chat in `node` with its pane: content panes, and terminal views on either side.
fn chat_leaves<'a>(node: &'a LayoutNode, output: &mut Vec<(&'a PaneId, &'a ChatContent)>) {
    match node {
        LayoutNode::Split { first, second, .. } => {
            chat_leaves(first, output);
            chat_leaves(second, output);
        }
        leaf => {
            if let (Some(pane_id), Some(PaneContent::Chat(chat))) =
                (leaf.leaf_pane(), leaf.content())
            {
                output.push((pane_id, chat));
            }
        }
    }
}

/// The harness's own mark, in its colours.
fn harness_mark(harness: ChatHarness) -> &'static str {
    match harness {
        ChatHarness::Omp => controls::OMP_MARK,
    }
}

/// `folder` shortened to its last two names.
fn folder_label(folder: &str) -> SharedString {
    let names: Vec<&str> = folder
        .split(['/', '\\'])
        .filter(|name| !name.is_empty())
        .collect();
    match names.as_slice() {
        [.., parent, name] if names.len() > 2 => format!("…/{parent}/{name}").into(),
        _ => folder.to_owned().into(),
    }
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// A span of `ms` milliseconds as the transcript shows it: `0.4s`, `12s`, `1m 05s`.
fn duration_label(ms: u64) -> String {
    if ms < 10_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let seconds = ms / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

/// What the header calls the chat: the session's title, else its first prompt's line.
fn chat_title(state: &ChatState) -> Option<String> {
    if let Some(title) = state.title.as_deref().map(str::trim)
        && !title.is_empty()
    {
        return Some(title.to_owned());
    }
    state.entries.iter().find_map(|entry| match entry {
        ChatEntry::User { text, .. } => {
            let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
            Some(match line.char_indices().nth(80) {
                Some((end, _)) => format!("{}…", &line[..end]),
                None => line.to_owned(),
            })
        }
        _ => None,
    })
}

/// Tokens as the footer shows them: `950`, `31k`, `1.2M`.
fn token_label(tokens: u64) -> String {
    let scaled = |value: u64, unit: u64, suffix: &str| {
        if value < unit * 10 && !value.is_multiple_of(unit) {
            format!("{:.1}{suffix}", value as f64 / unit as f64)
        } else {
            format!("{}{suffix}", value / unit)
        }
    };
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => scaled(tokens, 1_000, "k"),
        _ => scaled(tokens, 1_000_000, "M"),
    }
}

impl ChatModel {
    /// How Enter (or with `queue`, Ctrl+Enter) hands the draft over: into the running
    /// turn while one runs, else as the next turn; queued after it on request.
    fn delivery(&self, queue: bool) -> ChatDelivery {
        if queue {
            ChatDelivery::Queue
        } else if self.state.as_ref().is_some_and(ChatState::working) {
            ChatDelivery::Steer
        } else {
            ChatDelivery::Prompt
        }
    }

    /// Why a prompt cannot be handed over as `delivery` now.
    fn send_refusal(&self, delivery: ChatDelivery) -> Option<&'static str> {
        if self.requests > 0 {
            return Some("Wait for the last request to finish");
        }
        let Some(state) = &self.state else {
            return Some(match self.link {
                Link::Reconnecting(_) => "Reconnecting to the daemon",
                Link::Connecting | Link::Live => "Connecting to the chat",
            });
        };
        match (&state.status, delivery) {
            (ChatStatus::Ready, _)
            | (ChatStatus::Starting, ChatDelivery::Queue)
            | (ChatStatus::Working { .. }, ChatDelivery::Steer | ChatDelivery::Queue) => None,
            (ChatStatus::Starting, _) => Some("Wait for the chat to start"),
            (ChatStatus::Working { .. }, ChatDelivery::Prompt) => {
                Some("Wait for the reply, or stop it")
            }
            (ChatStatus::Failed { .. }, _) => Some("Restart the chat first"),
        }
    }

    /// The last prompt sent and its files, for Retry.
    fn last_prompt(&self) -> Option<(&str, &[String])> {
        self.state
            .as_ref()?
            .entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                ChatEntry::User { text, files, .. } => Some((text.as_str(), files.as_slice())),
                _ => None,
            })
    }

    /// Takes the chat's whole state. Entries that only went on keep their rows (their
    /// views grow, what the reader unfolded stays), so a reconnect keeps the transcript
    /// where the reader is; anything else starts the transcript over.
    fn adopt(&mut self, state: ChatState, cx: &mut App) {
        let old = self
            .state
            .as_ref()
            .map_or(&[][..], |state| state.entries.as_slice());
        let new = state.entries.as_slice();
        let rows = Rc::make_mut(&mut self.rows);
        let kept = rows.len() == old.len()
            && old.len() <= new.len()
            && old
                .iter()
                .zip(new)
                .all(|(was, now)| rows::continues(was, now));
        if kept {
            for (index, (was, now)) in old.iter().zip(new).enumerate() {
                if was == now {
                    continue;
                }
                rows::update_row(&mut rows[index], was, now, &self.folder, cx);
                self.scroller.remeasure_row(index);
            }
            let added = new.len() - old.len();
            rows.extend(
                new[old.len()..]
                    .iter()
                    .map(|entry| rows::chat_row(entry, &self.folder, cx)),
            );
            if added > 0 {
                self.scroller.append(added);
            }
        } else {
            *rows = new
                .iter()
                .map(|entry| rows::chat_row(entry, &self.folder, cx))
                .collect();
            self.scroller.reset(rows.len());
        }
        self.state = Some(state);
    }

    /// Applies one change, as the daemon did before sending it.
    fn apply(&mut self, event: ChatEvent, cx: &mut App) {
        if let ChatEvent::Reset { state } = event {
            self.adopt(*state, cx);
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            ChatEvent::Entry { entry } => {
                Rc::make_mut(&mut self.rows).push(rows::chat_row(&entry, &self.folder, cx));
                state.entries.push(entry);
                self.scroller.append(1);
            }
            ChatEvent::Text { index, text } => {
                if let Some(view) = self.rows.get(index).and_then(ChatRow::text_view) {
                    view.update(cx, |view, cx| view.push_str(&text, cx));
                    self.scroller.remeasure_row(index);
                }
                state.apply(ChatEvent::Text { index, text });
            }
            ChatEvent::Replace { index, entry } => {
                if let (Some(was), Some(row)) = (
                    state.entries.get(index),
                    Rc::make_mut(&mut self.rows).get_mut(index),
                ) {
                    rows::update_row(row, was, &entry, &self.folder, cx);
                    self.scroller.remeasure_row(index);
                }
                state.apply(ChatEvent::Replace { index, entry });
            }
            event => state.apply(event),
        }
    }

    /// Clears the composer and its attachments after `draft` was sent, unless the reader
    /// typed on meanwhile.
    fn clear_sent(&mut self, draft: &SharedString, window: &mut Window, cx: &mut App) {
        if self.composer.read(cx).value() != *draft {
            return;
        }
        self.composer.update(cx, |composer, cx| {
            composer.set_value(String::new(), window, cx);
        });
        self.attachments.clear();
        self.images.clear();
        self.completion = None;
    }
}

/// Starts the watcher of chat `id`: each connection watches `chat` as it is then, so a
/// session the pane learned resumes after a daemon restart, with `approval` as it is then.
fn watch_chat(
    link: WatchLink,
    id: Arc<str>,
    chat: Arc<Mutex<ChatContent>>,
    approval: Arc<Mutex<ChatApproval>>,
    generation: u64,
    stop: mpsc::Receiver<()>,
) {
    thread::spawn(move || {
        let send = |event| {
            link.sender.send(UiEvent::Chat(ChatUpdate {
                chat: id.clone(),
                generation,
                event,
            }))
        };
        watch::run(
            &link,
            &stop,
            |error| send(ChatWatchEvent::Failed(error)),
            |client| {
                let Ok(content) = chat.lock().map(|chat| chat.clone()) else {
                    return WatchEnd::Stopped;
                };
                let Ok(approval) = approval.lock().map(|approval| *approval) else {
                    return WatchEnd::Stopped;
                };
                let state = match client.watch_chat(content, approval) {
                    Ok(state) => state,
                    Err(error) => {
                        return WatchEnd::Failed(format!("The daemon can't run the chat: {error}"));
                    }
                };
                if !send(ChatWatchEvent::Watching(Box::new(state))) {
                    return WatchEnd::Stopped;
                }
                watch::poll(
                    client,
                    &link,
                    &stop,
                    // A chat is quiet between turns; that is not staleness.
                    || true,
                    |message| {
                        let ServerMessage::ChatChanged { event } = message else {
                            return None;
                        };
                        Some(send(ChatWatchEvent::Changed(event)))
                    },
                )
            },
        );
    });
}

impl CompiApp {
    /// OMP chat: a new chat in the focused terminal's folder, shown in its place; from
    /// another pane, or a terminal already carrying a chat, in that pane's folder,
    /// beside it.
    pub(super) fn open_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            self.dismiss_overlay();
        }
        self.popup = None;
        let (environment, folder) = match self.chat_scope() {
            Ok(scope) => scope,
            Err(reason) => {
                self.global_error = Some(reason.into());
                return;
            }
        };
        let content = PaneContent::Chat(crate::chat::new_chat(environment, folder));
        // A chat ends when its view goes, so one a terminal carries is not replaced.
        let in_place = self
            .focused_terminal()
            .filter(|terminal| self.pane_chat_content(terminal).is_none());
        if let Some(terminal) = in_place {
            self.show_view(&terminal, content, window, cx);
        } else if let Some(beside) = self.focused_pane().cloned() {
            self.open_content_beside(&beside, content);
        }
    }

    /// Where a new chat works: the focused terminal's shell folder, else the focused
    /// pane's folder as Files would open it.
    pub(super) fn chat_scope(&self) -> Result<(ContentEnvironment, String), &'static str> {
        if let Some(terminal) = self.focused_terminal() {
            return self.shell_folder(&terminal);
        }
        self.file_scope()
            .map(|scope| (scope.environment, scope.folder))
    }

    /// The chat `pane_id` holds, shown or not.
    fn pane_chat_content(&self, pane_id: &PaneId) -> Option<&ChatContent> {
        content::find_pane_content(self.workspace.as_ref()?, pane_id)?.chat()
    }

    /// The client state of the chat `pane_id` holds, shown or not.
    fn pane_chat(&self, pane_id: &PaneId) -> Option<&ChatModel> {
        self.chat.chats.get(&self.pane_chat_content(pane_id)?.id)
    }

    fn focused_chat(&self) -> Option<&ChatModel> {
        self.pane_chat(self.focused_pane()?)
    }

    /// Whether the focused pane shows a chat, as a content pane or a terminal's view: Zoom
    /// in, out and reset then size chat text.
    pub(super) fn focused_pane_shows_chat(&self) -> bool {
        let (Some(workspace), Some(pane)) = (self.workspace.as_ref(), self.focused_pane()) else {
            return false;
        };
        self.pane_chat_content(pane).is_some()
            && (self.shows_view(pane)
                || matches!(
                    content::find_leaf(workspace, pane),
                    Some(LayoutNode::Content { .. })
                ))
    }

    /// Zoom in, out or reset (`command`) of the chat text size, in every window.
    pub(super) fn step_chat_text_size(
        &mut self,
        command: Command,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let size = zoom_size(
            command,
            self.config.chat_text_size,
            crate::config::MIN_CHAT_TEXT_SIZE,
            crate::config::MAX_CHAT_TEXT_SIZE,
        );
        if size.unwrap_or(crate::config::DEFAULT_CHAT_TEXT_SIZE) == self.config.chat_text_size {
            return;
        }
        if let Err(error) = self.config.save_chat_text_size(size) {
            self.global_error = Some(error);
            return;
        }
        self.remeasure_chats();
        self.broadcast_global_appearance(window, cx);
        cx.notify();
    }

    /// Lays every chat's transcript out again, after its text size changed.
    pub(super) fn remeasure_chats(&self) {
        for model in self.chat.chats.values() {
            model.scroller.remeasure();
        }
    }

    /// The pane holding chat `chat`, and the chat.
    fn chat_pane(&self, chat: &str) -> Option<(PaneId, ChatContent)> {
        let workspace = self.workspace.as_ref()?;
        let mut leaves = Vec::new();
        for session in &workspace.sessions {
            for tab in &session.tabs {
                chat_leaves(&tab.layout, &mut leaves);
            }
        }
        leaves
            .into_iter()
            .find(|(_, content)| content.id == chat)
            .map(|(pane_id, content)| (pane_id.clone(), content.clone()))
    }

    /// Before drawing: state for each chat shown, kept while its pane or view is in the
    /// workspace; a watcher per chat shown (lingering after); the form of a question
    /// asked; each chat's session sent to the daemon once the harness has one; and a
    /// redraw each second while one works.
    pub(super) fn prepare_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let mut present = Vec::new();
        for session in &workspace.sessions {
            for tab in &session.tabs {
                chat_leaves(&tab.layout, &mut present);
            }
        }
        self.chat
            .chats
            .retain(|id, _| present.iter().any(|(_, chat)| chat.id == *id));
        let shown: Vec<String> = self
            .shown_content_panes()
            .filter_map(|pane| {
                let chat = content::find_pane_content(workspace, pane)?.chat()?;
                Some(chat.id.clone())
            })
            .collect();
        for &(_, chat) in &present {
            if shown.contains(&chat.id) && !self.chat.chats.contains_key(&chat.id) {
                let model = self.new_chat_model(chat, window, cx);
                self.chat.chats.insert(chat.id.clone(), model);
            }
        }
        let mutation_pending = self.mutation_pending;
        let mut update = None;
        for &(pane_id, chat) in &present {
            let Some(model) = self.chat.chats.get_mut(&chat.id) else {
                continue;
            };
            if let Ok(mut watched) = model.watched.lock()
                && *watched != *chat
            {
                *watched = chat.clone();
            }
            let Some(session) = model
                .state
                .as_ref()
                .and_then(|state| state.session.as_ref())
            else {
                continue;
            };
            if chat.session.as_ref() != Some(session)
                && model.session_sent.as_ref() != Some(session)
                && !mutation_pending
                && update.is_none()
            {
                model.session_sent = Some(session.clone());
                update = Some((
                    pane_id.clone(),
                    ChatContent {
                        session: Some(session.clone()),
                        ..chat.clone()
                    },
                ));
            }
        }
        if let Some((pane_id, chat)) = update {
            self.mutate(
                WorkspaceMutation::UpdateContent {
                    pane_id,
                    content: PaneContent::Chat(chat),
                },
                false,
            );
        }
        for id in &shown {
            self.prepare_chat_question(id, window, cx);
            self.prepare_chat_picker(id, window, cx);
        }
        if let Ok(mut approval) = self.chat.approval.lock() {
            *approval = self.config.chat_approval;
        }
        let link = self.watch_link();
        let mut linger = false;
        for (id, model) in &mut self.chat.chats {
            match model.slot.want(shown.contains(id)) {
                SlotChange::Start(generation, stop) => {
                    model.link = Link::Connecting;
                    watch_chat(
                        link.clone(),
                        id.as_str().into(),
                        model.watched.clone(),
                        self.chat.approval.clone(),
                        generation,
                        stop,
                    );
                }
                SlotChange::Linger => linger = true,
                SlotChange::Unchanged => {}
            }
        }
        if linger {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LINGER).await;
                this.update(cx, |this, cx| {
                    if this.expire_chat_watchers() {
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        let working = shown.iter().any(|id| {
            self.chat
                .chats
                .get(id)
                .and_then(|model| model.state.as_ref())
                .is_some_and(ChatState::working)
        });
        if working && !self.chat.ticking {
            self.chat.ticking = true;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                this.update(cx, |this, cx| {
                    this.chat.ticking = false;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn new_chat_model(
        &self,
        chat: &ChatContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ChatModel {
        let composer = cx.new(|cx| {
            // The card draws the harness's mark and hints over the empty draft.
            TextareaState::new(window, cx)
                .auto_grow(COMPOSER_MIN_ROWS, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
        let id = chat.id.clone();
        let events = cx.subscribe_in(
            &composer,
            window,
            move |this, _, event: &InputEvent, window, cx| match event {
                // Enter sends (or completes), Ctrl+Enter queues; Shift+Enter is a new
                // line, the field's own.
                InputEvent::PressEnter {
                    secondary,
                    shift: false,
                } => {
                    this.send_chat(&id, *secondary, window, cx);
                    cx.notify();
                }
                InputEvent::Change => {
                    this.acknowledge_chat_results(&id);
                    this.chat_composer_changed(&id, cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        self.read_chat_repo(chat, cx);
        ChatModel {
            watched: Arc::new(Mutex::new(chat.clone())),
            slot: WatchSlot::default(),
            link: Link::Connecting,
            state: None,
            rows: Rc::default(),
            scroller: TranscriptScroll::new(cx.entity().downgrade()),
            composer,
            _composer_events: events,
            focus: cx.focus_handle(),
            card: cx.focus_handle(),
            folder: chat.folder.clone(),
            folder_label: folder_label(&chat.folder),
            attachments: Vec::new(),
            images: Vec::new(),
            completion: None,
            dismissed: None,
            file_request: 0,
            menu: None,
            picker: None,
            sessions: None,
            repo: Repo::Reading,
            question: None,
            requests: 0,
            error: None,
            session_sent: None,
        }
    }

    /// Reads whether `chat`'s folder is in a Git repository, for Open in diff, and its
    /// branch, for the line under the composer.
    fn read_chat_repo(&self, chat: &ChatContent, cx: &mut Context<Self>) {
        let target = self.target.clone();
        let (id, environment, folder) = (
            chat.id.clone(),
            chat.environment.clone(),
            chat.folder.clone(),
        );
        cx.spawn(async move |this, cx| {
            let listing = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<_> {
                        target.connect()?.read_git_changes(environment, folder)
                    })()
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(model) = this.chat.chats.get_mut(&id) {
                    model.repo = match listing {
                        Ok(Some(listing)) => Repo::At {
                            top: listing.repo,
                            branch: listing.branch,
                        },
                        Ok(None) | Err(_) => Repo::None,
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Stops each watcher no pane has shown for [`LINGER`]; whether one stopped.
    fn expire_chat_watchers(&mut self) -> bool {
        let mut stopped = false;
        for model in self.chat.chats.values_mut() {
            stopped |= model.slot.expire();
        }
        stopped
    }

    pub(super) fn chat_event(&mut self, update: ChatUpdate, cx: &mut Context<Self>) {
        let Some(model) = self.chat.chats.get_mut(&*update.chat) else {
            return;
        };
        if !model.slot.is_current(update.generation) {
            return;
        }
        match update.event {
            ChatWatchEvent::Watching(state) => {
                model.link = Link::Live;
                model.adopt(*state, cx);
            }
            ChatWatchEvent::Changed(event) => model.apply(event, cx),
            ChatWatchEvent::Failed(error) => model.link = Link::Reconnecting(error),
        }
    }

    /// Sends `message` about chat `chat` on a connection of its own; a failure shows in
    /// the pane. A prompt sent from `draft` is cleared from the composer once accepted.
    fn chat_request(
        &mut self,
        chat: &str,
        message: ClientMessage,
        draft: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        model.requests += 1;
        model.error = None;
        let target = self.target.clone();
        let chat = chat.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<()> {
                        target.connect()?.chat_request(message)?;
                        Ok(())
                    })()
                    .map_err(|error| error.to_string())
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if let Some(model) = this.chat.chats.get_mut(&chat) {
                    model.requests = model.requests.saturating_sub(1);
                    match result {
                        Ok(()) => {
                            if let Some(draft) = draft {
                                model.clear_sent(&draft, window, cx);
                            }
                        }
                        Err(error) => model.error = Some(error),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A request that only needs `ChatAccepted`.
    fn chat_action(
        &mut self,
        chat: &str,
        message: ClientMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chat_request(chat, message, None, window, cx);
    }

    /// Sends the composer's text, with its attachments: as the next prompt when the chat
    /// waits for one, into the running turn while one runs, or, with `queue`, after it.
    /// Runs Compi's own `/` command instead; with a completion open, Enter completes.
    fn send_chat(&mut self, chat: &str, queue: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.acknowledge_chat_results(chat);
        if self.accept_chat_completion(chat, window, cx) {
            return;
        }
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        let draft = model.composer.read(cx).value();
        let text = draft.trim();
        let harness_commands = model
            .state
            .as_ref()
            .map(|state| state.commands.as_slice())
            .unwrap_or_default();
        if let Some((command, argument)) = composer::compi_command(text, harness_commands) {
            let argument = argument.to_owned();
            self.run_compi_command(chat, command, argument, window, cx);
            return;
        }
        let delivery = model.delivery(queue);
        if model.send_refusal(delivery).is_some() {
            return;
        }
        if text.is_empty() && model.attachments.is_empty() && model.images.is_empty() {
            return;
        }
        let message = ClientMessage::SendChat {
            chat: chat.to_owned(),
            text: text.to_owned(),
            files: model.attachments.clone(),
            images: model.images.clone(),
            delivery,
        };
        self.chat_request(chat, message, Some(draft), window, cx);
    }

    /// Drops queued prompt `queued` before it is sent.
    fn remove_queued_chat(
        &mut self,
        chat: &str,
        queued: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = ClientMessage::RemoveQueuedChat {
            chat: chat.to_owned(),
            queued,
        };
        self.chat_action(chat, message, window, cx);
    }

    /// Runs one of Compi's own `/` commands and clears the draft; attachments stay for
    /// the next prompt.
    fn run_compi_command(
        &mut self,
        chat: &str,
        command: CompiCommand,
        argument: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        model.completion = None;
        model.composer.update(cx, |composer, cx| {
            composer.set_value(String::new(), window, cx);
        });
        match command {
            CompiCommand::New => self.resume_chat(chat, None, window, cx),
            CompiCommand::Resume | CompiCommand::Sessions => {
                self.open_chat_sessions(chat, &argument, window, cx);
            }
        }
    }

    /// Retry after a failed turn: the last prompt again, with its files.
    fn retry_chat(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        if model.send_refusal(ChatDelivery::Prompt).is_some() {
            return;
        }
        let Some((text, files)) = model.last_prompt() else {
            return;
        };
        let message = ClientMessage::SendChat {
            chat: chat.to_owned(),
            text: text.to_owned(),
            files: files.to_vec(),
            images: Vec::new(),
            delivery: ChatDelivery::Prompt,
        };
        self.chat_action(chat, message, window, cx);
    }

    fn cancel_chat(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        let message = ClientMessage::CancelChat {
            chat: chat.to_owned(),
        };
        self.chat_action(chat, message, window, cx);
    }

    fn restart_chat(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        let message = ClientMessage::RestartChat {
            chat: chat.to_owned(),
            approval: self.config.chat_approval,
        };
        self.chat_action(chat, message, window, cx);
    }

    /// Starts the chat over in `session`, or in a new session.
    fn resume_chat(
        &mut self,
        chat: &str,
        session: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(model) = self.chat.chats.get_mut(chat) {
            model.menu = None;
        }
        let message = ClientMessage::ResumeChat {
            chat: chat.to_owned(),
            session,
            approval: self.config.chat_approval,
        };
        self.chat_action(chat, message, window, cx);
    }

    fn set_chat_option(
        &mut self,
        chat: &str,
        option: String,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(model) = self.chat.chats.get_mut(chat) {
            model.menu = None;
        }
        let message = ClientMessage::SetChatOption {
            chat: chat.to_owned(),
            option,
            value,
        };
        self.chat_action(chat, message, window, cx);
    }

    /// Answers the waiting permission with `option`.
    fn answer_chat_permission(
        &mut self,
        chat: &str,
        option: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(permission) = self
            .chat
            .chats
            .get(chat)
            .and_then(|model| model.state.as_ref()?.permission.as_ref())
        else {
            return;
        };
        let message = ClientMessage::AnswerChatPermission {
            chat: chat.to_owned(),
            permission: permission.id,
            option: Some(option),
        };
        self.chat_action(chat, message, window, cx);
    }

    /// Answers the waiting permission with its first option of `kinds`, as Enter and
    /// Esc do; whether it had one.
    fn answer_chat_permission_by_kind(
        &mut self,
        chat: &str,
        kinds: &[&str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(option) = self
            .chat
            .chats
            .get(chat)
            .and_then(|model| model.state.as_ref()?.permission.as_ref())
            .and_then(|permission| {
                permission
                    .options
                    .iter()
                    .find(|option| kinds.contains(&option.kind.as_str()))
            })
            .map(|option| option.id.clone())
        else {
            return false;
        };
        self.answer_chat_permission(chat, option, window, cx);
        true
    }

    /// Answers the open question with `answers`, or turns it down (`None`).
    fn answer_chat_question(
        &mut self,
        chat: &str,
        answers: Option<ChatAnswers>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(question) = self
            .chat
            .chats
            .get(chat)
            .and_then(|model| model.state.as_ref()?.question.as_ref())
        else {
            return;
        };
        let message = ClientMessage::AnswerChatQuestion {
            chat: chat.to_owned(),
            question: question.id,
            answers,
        };
        self.chat_action(chat, message, window, cx);
    }

    /// Opens or closes one of the chat's menus.
    fn set_chat_menu(&mut self, chat: &str, menu: ChatMenu, open: bool) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        if open {
            model.menu = Some(menu);
        } else if model.menu.as_ref() == Some(&menu) {
            model.menu = None;
        }
    }

    /// Unfolds or folds row `index` of chat `chat`: a tool card or a thought.
    fn toggle_chat_row(&mut self, chat: &str, index: usize) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        if let Some(row) = Rc::make_mut(&mut model.rows).get_mut(index) {
            row.toggle();
            model.scroller.remeasure_row(index);
        }
    }

    /// Open in diff: the uncommitted change of `path`, which a tool of chat `chat`
    /// edited, in Compi's diff view: in a files pane of the chat's tab whose folder
    /// holds it, else in a new one beside the chat.
    fn open_chat_diff(
        &mut self,
        chat: &str,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane_id, content)) = self.chat_pane(chat) else {
            return;
        };
        let Some(Repo::At { top: repo, .. }) = self.chat.chats.get(chat).map(|model| &model.repo)
        else {
            return;
        };
        let repo = repo.clone();
        let windows = self.windows_paths(&content.environment);
        let file = match crate::content_path::resolve(Some(&content.folder), path, windows) {
            Ok(file) => file,
            Err(error) => {
                self.global_error = Some(format!("Cannot open {path}: {error}"));
                return;
            }
        };
        let Some(relative) = crate::content_path::repo_relative(&repo, &file) else {
            self.global_error = Some(format!("{file} is outside the repository {repo}"));
            return;
        };
        let reuse = self.workspace.as_ref().and_then(|workspace| {
            let tab = workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| layout_contains_pane(&tab.layout, &pane_id))?;
            content::reusable_content(
                &tab.layout,
                &pane_id,
                &content.environment,
                None,
                Some(&file),
            )
            .map(|(pane, _)| pane.clone())
        });
        match reuse {
            Some(files) => {
                self.open_diff(&files, relative, Some(GitScope::Unstaged), None, window, cx);
            }
            None => {
                let tab = ContentTab::diff(
                    crate::content_path::work_tree_path(&repo, &relative),
                    GitScope::Unstaged,
                );
                self.open_content_beside(
                    &pane_id,
                    PaneContent::Files(FolderContent {
                        environment: content.environment,
                        root: repo,
                        tabs: vec![tab.clone()],
                        active: Some(tab),
                    }),
                );
            }
        }
    }

    /// Whether `focused` is inside one of this window's chat panes.
    pub(super) fn chat_owns(&self, focused: &FocusHandle, window: &Window, cx: &App) -> bool {
        self.chat.chats.values().any(|model| {
            *focused == model.focus
                || model.focus.contains_focused(window, cx)
                || model.composer.focus_handle(cx) == *focused
                || model
                    .sessions
                    .as_ref()
                    .is_some_and(|sheet| sheet.search_focus(cx) == *focused)
        })
    }

    /// Whether `pane` shows a chat with client state, and its focus handle.
    pub(super) fn chat_pane_focus(&self, pane_id: &PaneId) -> Option<&FocusHandle> {
        self.pane_chat(pane_id).map(|model| &model.focus)
    }

    /// What should have the keyboard in chat pane `pane_id`: the session picker's search
    /// while it is open, else its composer; `None` when that, the transcript's text, a
    /// question's field or the pane already has it. A field focused in the frame that
    /// created it is not yet inside the pane's focus.
    pub(super) fn chat_keyboard(
        &self,
        pane_id: &PaneId,
        window: &Window,
        cx: &App,
    ) -> Option<FocusHandle> {
        let model = self.pane_chat(pane_id)?;
        let wanted = match &model.sessions {
            Some(sheet) => sheet.search_focus(cx),
            None => model.composer.focus_handle(cx),
        };
        let held = model.focus.contains_focused(window, cx) || wanted.is_focused(window);
        (!held).then_some(wanted)
    }

    /// The focused chat pane's composer has the keyboard.
    pub(super) fn chat_composer_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focused_chat()
            .is_some_and(|model| model.composer.focus_handle(cx).is_focused(window))
    }

    /// Keys in the focused chat's composer, before the field's own: Ctrl+V (Cmd+V) with an
    /// image on the clipboard attaches it; the transcript's paging keys; the completion
    /// list's arrows, Tab and Escape; with the draft empty, Enter and Escape answer a
    /// waiting permission (its first allow or reject option); else Escape leaves the
    /// draft and hands the keyboard to the pane, where Escape again shows the shell.
    pub(super) fn chat_composer_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.chat_composer_has_keyboard(window, cx) {
            return false;
        }
        let Some(chat) = self
            .focused_pane()
            .and_then(|pane| self.pane_chat_content(pane))
            .map(|chat| chat.id.clone())
        else {
            return false;
        };
        let modifiers = &key.modifiers;
        let paste_modifier = if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        };
        if key.key == "v" && paste_modifier && !modifiers.alt && !modifiers.shift {
            return self.paste_chat_image(cx);
        }
        if self.chat_scroll_key(&chat, key) {
            return true;
        }
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
            return false;
        }
        if self.chat_completion_key(&chat, &key.key, window, cx) {
            return true;
        }
        let Some(model) = self.chat.chats.get(&chat) else {
            return false;
        };
        let empty = model.composer.read(cx).value().trim().is_empty();
        match key.key.as_str() {
            "enter" if empty => self.answer_chat_permission_by_kind(
                &chat,
                &["allow_once", "allow_always"],
                window,
                cx,
            ),
            "escape" => {
                if empty
                    && self.answer_chat_permission_by_kind(
                        &chat,
                        &["reject_once", "reject_always"],
                        window,
                        cx,
                    )
                {
                    return true;
                }
                if let Some(model) = self.chat.chats.get(&chat) {
                    window.focus(&model.focus, cx);
                }
                true
            }
            _ => false,
        }
    }

    /// Page Up and Page Down scroll chat `chat`'s transcript by a page less a line, and
    /// Ctrl+End (Cmd+Down) goes back to the newest entry, while no list or menu of the
    /// chat is open; whether `key` was one of them.
    fn chat_scroll_key(&self, chat: &str, key: &Keystroke) -> bool {
        let Some(model) = self.chat.chats.get(chat) else {
            return false;
        };
        if model.menu.is_some()
            || model.picker.is_some()
            || model.completion.is_some()
            || model.sessions.is_some()
        {
            return false;
        }
        let modifiers = &key.modifiers;
        let plain = !modifiers.control && !modifiers.alt && !modifiers.platform && !modifiers.shift;
        let latest = !modifiers.alt
            && !modifiers.shift
            && if cfg!(target_os = "macos") {
                modifiers.platform && !modifiers.control && key.key == "down"
            } else {
                modifiers.control && !modifiers.platform && key.key == "end"
            };
        match key.key.as_str() {
            "pageup" | "pagedown" if plain => {
                // A line of message text stays in view across the page.
                let line = self.chat_body_size() * 1.5;
                model.scroller.page(key.key == "pagedown", line);
            }
            _ if latest => model.scroller.scroll_to_end(),
            _ => return false,
        }
        true
    }

    /// Keys in a chat pane that its composer and fields left alone.
    fn chat_key_down(
        &mut self,
        pane_id: &PaneId,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chat) = self.pane_chat_content(pane_id).map(|chat| chat.id.clone()) else {
            return;
        };
        let Some(model) = self.chat.chats.get(&chat) else {
            return;
        };
        if model.composer.focus_handle(cx).is_focused(window) {
            // Text belongs to the composer; its keys were taken before it.
            return;
        }
        let key = &event.keystroke;
        if let Some(sheet) = &model.sessions {
            // Its search took its keys before it, and types the rest.
            if !sheet.search_focus(cx).is_focused(window) {
                self.chat_sessions_key(&chat, key, window, cx);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.chat_scroll_key(&chat, key) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let modifiers = &key.modifiers;
        let plain = !modifiers.control && !modifiers.alt && !modifiers.platform && !modifiers.shift;
        let escape = key.key == "escape" && plain;
        if !model.focus.is_focused(window) && !escape {
            // The transcript's text or a question's field has the keyboard.
            return;
        }
        let permission = model
            .state
            .as_ref()
            .is_some_and(|state| state.permission.is_some());
        let answered = permission
            && plain
            && match key.key.as_str() {
                "enter" => self.answer_chat_permission_by_kind(
                    &chat,
                    &["allow_once", "allow_always"],
                    window,
                    cx,
                ),
                "escape" => self.answer_chat_permission_by_kind(
                    &chat,
                    &["reject_once", "reject_always"],
                    window,
                    cx,
                ),
                _ => false,
            };
        // Esc that let go of everything shows a terminal's shell again.
        if !answered && !self.handle_app_key(event, window, cx) && escape {
            self.escape_to_shell(pane_id);
        }
        // Nothing typed in a chat reaches a shell.
        cx.stop_propagation();
        cx.notify();
    }

    /// Chat pane `pane_id`, showing `chat`: a header, the transcript, and the composer
    /// under whatever waits on the reader; the session picker over them while open.
    pub(super) fn render_chat_pane(
        &self,
        pane_id: &PaneId,
        chat: &ChatContent,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(model) = self.chat.chats.get(&chat.id) else {
            return div().flex_1().into_any_element();
        };
        let colors = *self.colors();
        let key_pane = pane_id.clone();
        let pane = div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .track_focus(&model.focus)
            .text_size(ui_rems(UI_BODY_TEXT_SIZE))
            .text_color(color(modal_text_color(colors.foreground, &colors)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.chat_key_down(&key_pane, event, window, cx);
            }))
            .child(self.render_chat_header(chat, model, cx))
            .child(self.render_chat_transcript(chat, model, cx))
            .child(self.render_chat_footer(chat, model, cx))
            .when_some(model.sessions.as_ref(), |pane, sheet| {
                pane.child(self.render_chat_sessions(chat, model, sheet, cx))
            });
        WithRemSize {
            rem: px(
                DEFAULT_REM * self.config.chat_text_size / crate::config::DEFAULT_CHAT_TEXT_SIZE
            ),
            child: pane.into_any_element(),
        }
        .into_any_element()
    }

    /// Message text's size in pixels at the chat text size: `UI_BODY_TEXT_SIZE` in the
    /// chat's rem.
    fn chat_body_size(&self) -> Pixels {
        px(UI_BODY_TEXT_SIZE * self.config.chat_text_size / crate::config::DEFAULT_CHAT_TEXT_SIZE)
    }

    /// The harness, the chat's title and folder, whether it is reachable, and its menu.
    fn render_chat_header(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let title = model.state.as_ref().and_then(chat_title);
        let reconnecting = match &model.link {
            Link::Reconnecting(error) => Some(error.clone()),
            Link::Connecting | Link::Live => None,
        };
        div()
            .flex_none()
            .h(ui_rems(HEADER_HEIGHT))
            .pl_3()
            .pr_1()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(color(colors.border))
            .child(
                img(harness_mark(chat.harness))
                    .flex_none()
                    .size(ui_rems(14.0)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                    .font_weight(FontWeight::MEDIUM)
                    .child(chat.harness.name()),
            )
            .when_some(title, |header, title| {
                header.child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                        .child(title),
                )
            })
            .child(
                div()
                    .id("chat-folder")
                    .flex_none()
                    .max_w(ui_rems(200.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(model.folder_label.clone())
                    .tooltip(controls::tooltip(chat.folder.clone(), None, None)),
            )
            .child(div().flex_1())
            .when_some(reconnecting, |header, error| {
                header.child(
                    div()
                        .id("chat-reconnecting")
                        .flex_none()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child("Reconnecting…")
                        .tooltip(controls::tooltip(error, None, None)),
                )
            })
            .child(self.render_chat_menu(chat, model, cx))
            .into_any_element()
    }

    /// A centred message in place of the transcript, with an action under it.
    fn chat_message(
        &self,
        title: SharedString,
        detail: Option<String>,
        action: Option<AnyElement>,
    ) -> AnyElement {
        let colors = *self.colors();
        div()
            .flex_1()
            .min_h_0()
            .p_4()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(div().text_size(ui_rems(UI_BODY_TEXT_SIZE)).child(title))
            .when_some(detail, |message, detail| {
                message.child(
                    div()
                        .max_w(ui_rems(480.0))
                        .text_center()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .child(detail),
                )
            })
            .when_some(action, |message, action| {
                message.child(div().mt_1().child(action))
            })
            .into_any_element()
    }

    /// Restart (`RestartChat`) for a chat whose harness failed.
    fn chat_restart_button(&self, chat: &ChatContent, cx: &Context<Self>) -> AnyElement {
        let id = chat.id.clone();
        text_button("chat-restart", "Retry")
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.restart_chat(&id, window, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// The transcript, following the newest entry while the reader is at the bottom and
    /// staying put while they read further up; before there is one, what is happening.
    fn render_chat_transcript(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let harness = chat.harness.name();
        let Some(state) = &model.state else {
            return match &model.link {
                Link::Reconnecting(error) => {
                    self.chat_message("Can't reach the chat".into(), Some(error.clone()), None)
                }
                Link::Connecting | Link::Live => {
                    self.chat_message(format!("Starting {harness}…").into(), None, None)
                }
            };
        };
        if state.entries.is_empty() {
            return match &state.status {
                ChatStatus::Starting => {
                    self.chat_message(format!("Starting {harness}…").into(), None, None)
                }
                ChatStatus::Failed { message } => self.chat_message(
                    format!("{harness} stopped").into(),
                    Some(message.clone()),
                    Some(self.chat_restart_button(chat, cx)),
                ),
                ChatStatus::Ready | ChatStatus::Working { .. } => self.chat_message(
                    format!("Ask {harness} about {}", model.folder_label).into(),
                    None,
                    None,
                ),
            };
        }
        let rows = model.rows.clone();
        let frame = Rc::new(RowFrame {
            app: cx.entity().downgrade(),
            chat: chat.id.as_str().into(),
            colors: *self.colors(),
            body_size: self.chat_body_size(),
            mono: self.typography.font.family.clone(),
            permission: state.permission.clone(),
            // Retry resends the last prompt after a turn that failed.
            retry: model.send_refusal(ChatDelivery::Prompt).is_none(),
            diff_refusal: match &model.repo {
                Repo::At { .. } => None,
                Repo::Reading => Some("Checking for a Git repository"),
                Repo::None => Some("The chat's folder is not in a Git repository"),
            },
        });
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                model
                    .scroller
                    .render(move |index| rows::render_row(&rows, index, &frame), cx),
            )
            .into_any_element()
    }
}
