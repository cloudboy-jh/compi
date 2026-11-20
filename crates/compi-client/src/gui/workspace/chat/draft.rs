//! New thread drafts. Every new chat starts as one: its message box in the middle of the
//! pane, the project above it, the folder and the permissions the agent starts with
//! below, and a quiet "Open instead" row that puts a terminal, Files, Changes, a browser
//! or Monitor in the draft's place. The daemon keeps no thread of a draft until its first
//! prompt (`ChatState::thread`), so closing an untouched one leaves nothing; closing one
//! with typed text offers Undo for a few seconds.
use super::*;
use compi_protocol::{
    BrowserContent, MonitorContent, MutationId, MutationReceipt, PaneReplacement, SessionId,
    SplitAxis,
};
use std::sync::atomic::{AtomicU64, Ordering};

/// How long "Draft discarded · Undo" stays.
const UNDO_LIFETIME: Duration = Duration::from_secs(5);
/// The widest the draft's column grows.
const DRAFT_WIDTH: f32 = 680.0;

static NEXT_NOTICE: AtomicU64 = AtomicU64::new(0);

/// Whether a chat is a draft: nothing was sent, so the daemon keeps no thread of it.
/// Before the daemon first reports the chat, one whose pane names no session yet is.
pub(super) fn is_draft(state: Option<&ChatState>, session: Option<&str>) -> bool {
    match state {
        Some(state) => !state.thread && state.entries.is_empty(),
        None => session.is_none(),
    }
}

/// The chat Split right and Split down start a new thread draft beside `leaf` for, in its
/// folder: the one it shows, a chat pane's or a terminal's chat view. `None` for a
/// terminal showing its shell and every other pane, which get a terminal.
pub(in crate::gui) fn split_draft(leaf: &LayoutNode) -> Option<&ChatContent> {
    leaf.shown_content().and_then(PaneContent::chat)
}

/// What a draft's "Open instead" row puts in its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpenInstead {
    /// A new shell in the draft's folder.
    Terminal,
    /// Files at the draft's folder.
    Files,
    /// Files at the folder's repository, listing its changes.
    Changes,
    /// A blank page.
    Browser,
    Monitor,
}

impl OpenInstead {
    const ALL: [Self; 5] = [
        Self::Terminal,
        Self::Files,
        Self::Changes,
        Self::Browser,
        Self::Monitor,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Files => "Files",
            Self::Changes => "Changes",
            Self::Browser => "Browser",
            Self::Monitor => "Monitor",
        }
    }

    /// The letter that picks it in an empty draft, in GPUI keystroke syntax.
    fn key(self) -> &'static str {
        match self {
            Self::Terminal => "t",
            Self::Files => "f",
            Self::Changes => "d",
            Self::Browser => "b",
            Self::Monitor => "m",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Terminal => IconName::SquareTerminal,
            Self::Files => IconName::Folder,
            Self::Changes => IconName::FileDiff,
            Self::Browser => IconName::Globe,
            Self::Monitor => IconName::Gauge,
        }
    }
}

/// What `key`, pressed without a modifier in a draft, opens instead: one of the row's
/// letters, only while the draft is `empty` (no text, files or images); otherwise the
/// key types.
pub(super) fn open_instead_key(key: &str, empty: bool) -> Option<OpenInstead> {
    if !empty {
        return None;
    }
    OpenInstead::ALL
        .into_iter()
        .find(|choice| choice.key() == key)
}

/// Where a discarded draft was, so Undo opens it there again.
#[derive(Clone, Debug, PartialEq)]
enum DraftPlace {
    /// A pane of its own, split off `pane` along `axis`.
    Beside { pane: PaneId, axis: SplitAxis },
    /// The view of terminal pane `pane`.
    View { pane: PaneId },
    /// A tab of its own in workspace `session`.
    Tab { session: SessionId },
}

/// A draft with typed text that a close threw away.
pub(in crate::gui) struct Discarded {
    chat: ChatContent,
    text: String,
    place: DraftPlace,
}

/// "Draft discarded · Undo", while it shows.
pub(super) struct DiscardNotice {
    id: u64,
    draft: Discarded,
}

/// The pane a draft closed out of `layout` from pane `pane` reopens beside, and the axis
/// of the split they shared: the leaf of its sibling nearest it.
fn draft_neighbor(layout: &LayoutNode, pane: &PaneId) -> Option<(PaneId, SplitAxis)> {
    let LayoutNode::Split {
        axis,
        first,
        second,
        ..
    } = layout
    else {
        return None;
    };
    if first.leaf_pane() == Some(pane) {
        return edge_leaf(second, true).map(|leaf| (leaf, *axis));
    }
    if second.leaf_pane() == Some(pane) {
        return edge_leaf(first, false).map(|leaf| (leaf, *axis));
    }
    draft_neighbor(first, pane).or_else(|| draft_neighbor(second, pane))
}

/// The first leaf of `node`, or with `first` false its last.
fn edge_leaf(node: &LayoutNode, first: bool) -> Option<PaneId> {
    match node {
        LayoutNode::Split {
            first: head,
            second: tail,
            ..
        } => edge_leaf(if first { head } else { tail }, first),
        leaf => leaf.leaf_pane().cloned(),
    }
}

impl CompiApp {
    /// Split right or down from `beside`, which shows `source`: a new thread draft in
    /// `source`'s folder, in a pane of its own.
    pub(in crate::gui) fn open_draft_split(
        &mut self,
        beside: &PaneId,
        axis: SplitAxis,
        source: &ChatContent,
    ) {
        let draft = crate::chat::new_chat(source.environment.clone(), source.folder.clone());
        self.open_content_split(beside, axis, PaneContent::Chat(draft));
    }

    /// A draft pane: the project, the message box and under it the folder and the
    /// permissions, then the "Open instead" row, in the middle of the pane.
    pub(super) fn render_chat_draft(
        &self,
        pane_id: &PaneId,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let project = self.pane_project(pane_id);
        let name: SharedString = match project {
            Some(project) => project.name().to_owned().into(),
            None => model.folder_label.clone(),
        };
        div()
            .id("chat-draft")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_4()
            .py_6()
            .child(
                div()
                    .w_full()
                    .max_w(ui_rems(DRAFT_WIDTH))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(self.project_icon(project, 20.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_rems(UI_BODY_TEXT_SIZE + 2.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(name),
                            ),
                    )
                    .child(self.render_chat_footer(chat, model, true, cx))
                    .child(self.render_open_instead(chat, cx)),
            )
            .into_any_element()
    }

    /// "Open instead:" and its choices, each with its letter.
    fn render_open_instead(&self, chat: &ChatContent, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let hover = color(colors.surface_hover);
        let foreground = color(colors.foreground);
        let border = color(colors.border);
        let choices = OpenInstead::ALL
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                let id = chat.id.clone();
                div()
                    .id(("chat-open-instead", index))
                    .flex_none()
                    .h(ui_rems(22.0))
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(ui_rems(4.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(hover).text_color(foreground))
                    .child(controls::glyph(choice.icon(), muted))
                    .child(choice.label())
                    .child(
                        div()
                            .px_1()
                            .rounded(ui_rems(3.0))
                            .border_1()
                            .border_color(border)
                            .child(choice.key().to_uppercase()),
                    )
                    .tooltip(controls::tooltip(
                        format!("Open {} here instead", choice.label()),
                        Some("Or press its letter while the draft is empty".into()),
                        Some(choice.key()),
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_instead(&id, choice, window, cx);
                        cx.notify();
                    }))
            });
        div()
            .px_3()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_center()
            .gap_1()
            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
            .text_color(muted)
            .child(div().mr_1().child("Open instead:"))
            .children(choices)
            .into_any_element()
    }

    /// The draft's permissions: Settings → Chat → Approvals' choices, for this chat only.
    pub(super) fn render_chat_approval(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = model.menu == Some(ChatMenu::Approval);
        let current = model.approval(self.config.chat_approval);
        let label = super::super::settings::chat_approval_label;
        let trigger = text_button("chat-approval", label(current))
            .ghost()
            .menu_open(open)
            .tooltip("Permissions")
            .detail(format!(
                "How {} asks before it runs tools, for this chat only",
                chat.harness.name()
            ));
        // Entries are built only for an open menu: a closed one draws just its button.
        let entries = crate::config::CHAT_APPROVALS
            .into_iter()
            .filter(|_| open)
            .map(|approval| {
                let id = chat.id.clone();
                controls::menu_item(
                    label(approval),
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.pick_draft_approval(&id, approval, window, cx);
                        cx.notify();
                    }),
                )
                .checked(approval == current)
            });
        let id = chat.id.clone();
        controls::menu(
            "chat-approval-menu",
            trigger,
            open,
            entries.collect::<Vec<_>>(),
        )
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.set_chat_menu(&id, ChatMenu::Approval, *open);
            cx.notify();
        }))
        .into_any_element()
    }

    /// Starts chat `chat`'s harness with `approval`: its watcher passes it on, and a
    /// harness already running starts again with it, on a new session, as a draft has
    /// nothing to resume.
    fn pick_draft_approval(
        &mut self,
        chat: &str,
        approval: ChatApproval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let default = self.config.chat_approval;
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        model.menu = None;
        let changed = model.approval(default) != approval;
        if let Ok(mut chosen) = model.approval.lock() {
            *chosen = Some(approval);
        }
        if changed && model.state.is_some() {
            self.restart_chat(chat, window, cx);
        }
    }

    /// Puts `choice` in the place of draft `chat`, in its folder; the draft goes. A draft
    /// in a pane of its own is replaced in place; one a terminal shows as its view gives
    /// way to the terminal's shell, or becomes the terminal's view of `choice`.
    pub(super) fn open_instead(
        &mut self,
        chat: &str,
        choice: OpenInstead,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane_id, content)) = self.chat_pane(chat) else {
            return;
        };
        let view = self
            .workspace
            .as_ref()
            .and_then(|workspace| content::find_leaf(workspace, &pane_id))
            .is_some_and(|leaf| leaf.surface_id().is_some());
        let ChatContent {
            environment,
            folder,
            ..
        } = content;
        let replacement = match choice {
            OpenInstead::Terminal if view => {
                self.mutate(WorkspaceMutation::CloseView { pane_id }, false);
                return;
            }
            OpenInstead::Terminal => {
                let distribution = match environment {
                    ContentEnvironment::Wsl { distribution } => Some(distribution),
                    ContentEnvironment::Host => None,
                };
                self.mutate_in(
                    WorkspaceMutation::ReplacePane {
                        pane_id,
                        with: PaneReplacement::Terminal {
                            cols: self.terminal_cols,
                            rows: self.terminal_rows,
                            working_directory: Some(folder),
                        },
                    },
                    true,
                    distribution,
                );
                return;
            }
            OpenInstead::Changes => {
                self.open_changes_instead(pane_id, view, environment, folder, window, cx);
                return;
            }
            OpenInstead::Files => PaneContent::Files(FolderContent {
                environment,
                root: folder,
                tabs: Vec::new(),
                active: None,
            }),
            OpenInstead::Browser => PaneContent::Browser(BrowserContent {
                url: BrowserContent::BLANK.into(),
            }),
            OpenInstead::Monitor => PaneContent::Monitor(MonitorContent::default()),
        };
        self.put_in_draft_place(&pane_id, view, replacement, window, cx);
    }

    /// Changes in the draft's place: Files at the repository holding `folder`, its
    /// sidebar on the Changes list, once the listing is read.
    fn open_changes_instead(
        &mut self,
        pane_id: PaneId,
        view: bool,
        environment: ContentEnvironment,
        folder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = {
                let (environment, folder) = (environment.clone(), folder.clone());
                cx.background_executor()
                    .spawn(async move {
                        (|| -> crate::Result<_> {
                            target.connect()?.read_git_changes(environment, folder)
                        })()
                        .map_err(|error| error.to_string())
                    })
                    .await
            };
            this.update_in(cx, |this, window, cx| {
                match read {
                    Ok(Some(listing)) => {
                        let content = PaneContent::Files(FolderContent {
                            environment: environment.clone(),
                            root: listing.repo.clone(),
                            tabs: Vec::new(),
                            active: None,
                        });
                        this.content.changes_seed = Some((environment, listing));
                        this.put_in_draft_place(&pane_id, view, content, window, cx);
                    }
                    Ok(None) => this.global_error = Some(format!("Not a Git repository: {folder}")),
                    Err(error) => {
                        this.global_error =
                            Some(format!("Cannot read the Git changes of {folder}: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Shows `content` where the draft of `pane_id` is: as the terminal's view when
    /// `view`, else in a pane replacing the draft's.
    fn put_in_draft_place(
        &mut self,
        pane_id: &PaneId,
        view: bool,
        content: PaneContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if view {
            self.show_view(pane_id, content, window, cx);
            return;
        }
        self.mutate(
            WorkspaceMutation::ReplacePane {
                pane_id: pane_id.clone(),
                with: PaneReplacement::Content { content },
            },
            true,
        );
    }

    /// The draft with typed text that `operation` closes, and where it was; `None` when
    /// it closes none, or only untouched ones.
    pub(in crate::gui) fn discarded_draft(
        &self,
        operation: &WorkspaceMutation,
        cx: &App,
    ) -> Option<Discarded> {
        let workspace = self.workspace.as_ref()?;
        let (only, view) = match operation {
            WorkspaceMutation::RemovePane { pane_id } => (Some(pane_id), false),
            WorkspaceMutation::CloseView { pane_id } => (Some(pane_id), true),
            WorkspaceMutation::RemoveTab { .. }
            | WorkspaceMutation::RemoveTabGroup { .. }
            | WorkspaceMutation::RemoveSession { .. } => (None, false),
            _ => return None,
        };
        for session in &workspace.sessions {
            for tab in &session.tabs {
                let closed = match operation {
                    WorkspaceMutation::RemoveTab { tab_id } => tab.id == *tab_id,
                    WorkspaceMutation::RemoveTabGroup { group_id } => {
                        tab.group.as_ref() == Some(group_id)
                    }
                    WorkspaceMutation::RemoveSession { session_id } => session.id == *session_id,
                    _ => only.is_some_and(|pane| layout_contains_pane(&tab.layout, pane)),
                };
                if !closed {
                    continue;
                }
                let mut chats = Vec::new();
                chat_leaves(&tab.layout, &mut chats);
                for (pane, chat) in chats {
                    if only.is_some_and(|only| only != pane) {
                        continue;
                    }
                    let Some(model) = self.chat.chats.get(&chat.id) else {
                        continue;
                    };
                    let text = model.composer.read(cx).value();
                    if !model.draft(chat) || text.trim().is_empty() {
                        continue;
                    }
                    let in_tab = DraftPlace::Tab {
                        session: session.id.clone(),
                    };
                    let place = match only {
                        Some(_) if view => DraftPlace::View { pane: pane.clone() },
                        Some(_) => draft_neighbor(&tab.layout, pane)
                            .map_or(in_tab, |(pane, axis)| DraftPlace::Beside { pane, axis }),
                        None => in_tab,
                    };
                    return Some(Discarded {
                        chat: chat.clone(),
                        text: text.to_string(),
                        place,
                    });
                }
            }
        }
        None
    }

    /// Notes the draft `discarded` that the close `mutation` (when it was sent) throws
    /// away, to offer Undo once the daemon took the close.
    pub(in crate::gui) fn note_discarding(
        &mut self,
        mutation: Option<MutationId>,
        discarded: Option<Discarded>,
    ) {
        self.chat.discarding = mutation.zip(discarded);
    }

    /// A workspace change finished (`None`: it failed): a close that threw a draft
    /// away shows "Draft discarded · Undo" for `UNDO_LIFETIME`.
    pub(in crate::gui) fn draft_mutation_finished(
        &mut self,
        receipt: Option<&MutationReceipt>,
        cx: &mut Context<Self>,
    ) {
        let Some((mutation, draft)) = self.chat.discarding.take() else {
            return;
        };
        if receipt.is_none_or(|receipt| receipt.mutation_id != mutation) {
            return;
        }
        let id = NEXT_NOTICE.fetch_add(1, Ordering::Relaxed);
        self.chat.discarded = Some(DiscardNotice { id, draft });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_LIFETIME).await;
            this.update(cx, |this, cx| {
                if this
                    .chat
                    .discarded
                    .as_ref()
                    .is_some_and(|notice| notice.id == id)
                {
                    this.chat.discarded = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Undo: the discarded draft opens again where it was, with its text; beside the
    /// pane it shared a split with, as the view of its terminal, or in a tab of its
    /// workspace. Where that is gone, in a tab of the selected workspace.
    fn undo_discarded_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DiscardNotice { draft, .. }) = self.chat.discarded.take() else {
            return;
        };
        let Discarded { chat, text, place } = draft;
        let Some(workspace) = &self.workspace else {
            return;
        };
        let leaf = |pane: &PaneId| content::find_leaf(workspace, pane);
        let place = match place {
            DraftPlace::View { pane } => match leaf(&pane) {
                Some(found) if found.surface_id().is_some() && found.view().is_none() => {
                    Some(DraftPlace::View { pane })
                }
                Some(_) => Some(DraftPlace::Beside {
                    pane,
                    axis: SplitAxis::Horizontal,
                }),
                None => None,
            },
            DraftPlace::Beside { pane, axis } => {
                leaf(&pane).map(|_| DraftPlace::Beside { pane, axis })
            }
            DraftPlace::Tab { session } => workspace
                .sessions
                .iter()
                .any(|kept| kept.id == session)
                .then_some(DraftPlace::Tab { session }),
        };
        let place = match place {
            Some(place) => place,
            None => match self.state.selected_session(workspace) {
                Some(session) => DraftPlace::Tab {
                    session: session.id.clone(),
                },
                None => return,
            },
        };
        let chat = crate::chat::new_chat(chat.environment, chat.folder);
        let id = chat.id.clone();
        let content = PaneContent::Chat(chat);
        let opened = match place {
            DraftPlace::View { pane } => self.show_view(&pane, content, window, cx),
            DraftPlace::Beside { pane, axis } => self.open_content_split(&pane, axis, content),
            DraftPlace::Tab { session } => {
                self.mutate(
                    WorkspaceMutation::CreateContentTab {
                        session_id: session,
                        label: String::new(),
                        content,
                    },
                    true,
                );
                true
            }
        };
        if opened {
            self.chat.pending.insert(id, text);
        }
    }

    /// "Draft discarded · Undo", in the window's bottom-right corner while it shows.
    pub(in crate::gui) fn render_discard_notice(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.chat.discarded.as_ref()?;
        let colors = *self.colors();
        Some(
            div()
                .id("draft-discarded")
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .rounded_md()
                .border_1()
                .border_color(color(colors.border))
                .bg(color(colors.surface))
                .shadow_lg()
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .text_color(color(modal_text_color(colors.muted, &colors)))
                        .child("Draft discarded"),
                )
                .child(
                    div()
                        .id("draft-undo")
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color(colors.accent))
                        .cursor_pointer()
                        .child("Undo")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.undo_discarded_draft(window, cx);
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::SurfaceId;

    fn chat(id: &str) -> ChatContent {
        ChatContent {
            id: id.into(),
            harness: ChatHarness::Omp,
            environment: ContentEnvironment::Host,
            folder: "/work".into(),
            session: None,
        }
    }

    #[test]
    fn a_chat_is_a_draft_until_it_has_a_thread_or_a_transcript() {
        // Before the daemon reports, a pane naming no session yet holds a draft.
        assert!(is_draft(None, None));
        assert!(!is_draft(None, Some("s1")));
        let mut state = ChatState::default();
        assert!(is_draft(Some(&state), Some("s1")));
        state.entries.push(ChatEntry::Agent { text: "hi".into() });
        assert!(!is_draft(Some(&state), None));
        // A thread cleared by a new session is no draft.
        state.entries.clear();
        state.thread = true;
        assert!(!is_draft(Some(&state), None));
    }

    #[test]
    fn splits_start_a_draft_from_a_chat_and_a_terminal_otherwise() {
        let terminal = LayoutNode::terminal(PaneId::new("pane-1"), SurfaceId::new("surface-1"));
        assert_eq!(split_draft(&terminal), None);

        let shown = LayoutNode::Pane {
            pane_id: PaneId::new("pane-1"),
            surface_id: SurfaceId::new("surface-1"),
            view: Some(compi_protocol::PaneView {
                content: PaneContent::Chat(chat("chat-1")),
                side: compi_protocol::PaneSide::View,
            }),
        };
        assert_eq!(
            split_draft(&shown).map(|chat| chat.id.as_str()),
            Some("chat-1")
        );

        // A terminal showing its shell splits as a terminal, its chat view aside.
        let LayoutNode::Pane {
            pane_id,
            surface_id,
            view: Some(view),
        } = shown
        else {
            unreachable!()
        };
        let shell = LayoutNode::Pane {
            pane_id,
            surface_id,
            view: Some(compi_protocol::PaneView {
                side: compi_protocol::PaneSide::Terminal,
                ..view
            }),
        };
        assert_eq!(split_draft(&shell), None);

        let pane = LayoutNode::Content {
            pane_id: PaneId::new("pane-2"),
            content: PaneContent::Chat(chat("chat-2")),
        };
        assert_eq!(
            split_draft(&pane).map(|chat| chat.id.as_str()),
            Some("chat-2")
        );

        for other in [
            PaneContent::Browser(BrowserContent {
                url: BrowserContent::BLANK.into(),
            }),
            PaneContent::Monitor(MonitorContent::default()),
            PaneContent::Files(FolderContent {
                environment: ContentEnvironment::Host,
                root: "/work".into(),
                tabs: Vec::new(),
                active: None,
            }),
        ] {
            let leaf = LayoutNode::Content {
                pane_id: PaneId::new("pane-3"),
                content: other,
            };
            assert_eq!(split_draft(&leaf), None);
        }
    }

    #[test]
    fn open_instead_letters_work_only_in_an_empty_draft() {
        assert_eq!(open_instead_key("t", true), Some(OpenInstead::Terminal));
        assert_eq!(open_instead_key("f", true), Some(OpenInstead::Files));
        assert_eq!(open_instead_key("d", true), Some(OpenInstead::Changes));
        assert_eq!(open_instead_key("b", true), Some(OpenInstead::Browser));
        assert_eq!(open_instead_key("m", true), Some(OpenInstead::Monitor));
        // Other keys type, and once there is text every key types.
        assert_eq!(open_instead_key("x", true), None);
        assert_eq!(open_instead_key("enter", true), None);
        for choice in OpenInstead::ALL {
            assert_eq!(open_instead_key(choice.key(), false), None);
        }
    }

    #[test]
    fn a_discarded_draft_reopens_beside_the_nearest_pane_of_its_split() {
        let leaf = |id: &str| LayoutNode::Content {
            pane_id: PaneId::new(id),
            content: PaneContent::Chat(chat(id)),
        };
        let split = |axis, first, second| LayoutNode::Split {
            axis,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        // a | (b over draft) | ... : the draft sat under b.
        let layout = split(
            SplitAxis::Horizontal,
            leaf("a"),
            split(SplitAxis::Vertical, leaf("b"), leaf("draft")),
        );
        assert_eq!(
            draft_neighbor(&layout, &PaneId::new("draft")),
            Some((PaneId::new("b"), SplitAxis::Vertical))
        );
        // A draft first in its split reopens beside the first leaf of what followed it.
        let layout = split(
            SplitAxis::Horizontal,
            leaf("draft"),
            split(SplitAxis::Vertical, leaf("b"), leaf("c")),
        );
        assert_eq!(
            draft_neighbor(&layout, &PaneId::new("draft")),
            Some((PaneId::new("b"), SplitAxis::Horizontal))
        );
        assert_eq!(draft_neighbor(&leaf("draft"), &PaneId::new("draft")), None);
    }
}
