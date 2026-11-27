//! New thread drafts. Every new chat starts as one, in the middle of its pane: its
//! project as the header (icon, name, and a line of the branch, changed files and
//! folder), the message box, the project's recent threads to continue instead, and a
//! row of what else the pane can be (a terminal, Files, Changes, a browser or Monitor),
//! each on Alt and its letter (Option on macOS). The chat's permissions sit in its model
//! picker. The daemon keeps no thread of a draft until its first prompt
//! (`ChatState::thread`), so closing an untouched one leaves nothing; closing one with
//! typed text offers Undo for a few seconds.
use super::*;
use crate::gui::controls::menu_item;
use compi_protocol::{
    BrowserContent, MonitorContent, MutationId, MutationReceipt, PaneReplacement, Project,
    SessionId, SplitAxis,
};
use std::sync::atomic::{AtomicU64, Ordering};

/// How long "Draft discarded · Undo" stays.
const UNDO_LIFETIME: Duration = Duration::from_secs(5);
/// The draft's column (the message box and the list under it, less the footer's 12 px
/// each side) takes this share of the pane's width, between these bounds; a pane
/// narrower than the least it fills.
const DRAFT_SHARE: f32 = 0.6;
const DRAFT_MIN_WIDTH: f32 = 544.0;
const DRAFT_MAX_WIDTH: f32 = 760.0;
/// The corner of a terminal whose hover shows its arrow back to a new thread.
const RETURN_CORNER_WIDTH: f32 = 72.0;
const RETURN_CORNER_HEIGHT: f32 = 48.0;

/// The draft column's width in a pane `area` wide (less its padding).
fn draft_width(area: Pixels, least: Pixels, most: Pixels) -> Pixels {
    (area * DRAFT_SHARE).clamp(least, most).min(area)
}

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

/// What a pane an "Open here" pick filled shows: a terminal's shell, or content of one
/// kind (Files, a browser, Monitor). The arrow back stays while the pane shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opened {
    Shell,
    Content(std::mem::Discriminant<PaneContent>),
}

impl Opened {
    /// What `leaf` shows now; `None` for a split.
    fn of(leaf: &LayoutNode) -> Option<Self> {
        match leaf.shown_content() {
            Some(content) => Some(Self::Content(std::mem::discriminant(content))),
            None => leaf.surface_id().map(|_| Self::Shell),
        }
    }
}

/// A pane an "Open here" pick filled: the draft it replaced, whose folder and agent the
/// arrow back starts a new thread with, and what the pick showed.
#[derive(Clone, Debug)]
pub(super) struct DraftReturn {
    draft: ChatContent,
    opened: Opened,
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

/// The line under a draft's project name: the branch, how many files have changes, and
/// `folder`; before Git answers, and outside Git, the folder alone.
fn draft_facts(repo: &Repo, folder: &str) -> String {
    let mut facts = Vec::new();
    if let Repo::At {
        branch,
        changes,
        truncated,
        ..
    } = repo
    {
        facts.extend(branch.clone());
        facts.push(match (*changes, *truncated) {
            (count, true) => format!("{count}+ changes"),
            (0, false) => "No changes".to_owned(),
            (1, false) => "1 change".to_owned(),
            (count, false) => format!("{count} changes"),
        });
    }
    facts.push(folder.to_owned());
    facts.join(" · ")
}

impl CompiApp {
    /// Split right or down from `beside`, which shows `source`: a new thread draft in
    /// `source`'s folder, with `source`'s agent, in a pane of its own.
    pub(in crate::gui) fn open_draft_split(
        &mut self,
        beside: &PaneId,
        axis: SplitAxis,
        source: &ChatContent,
    ) {
        let draft = crate::chat::new_chat(
            source.harness,
            source.environment.clone(),
            source.folder.clone(),
            source.prefer,
        );
        self.open_content_split(beside, axis, PaneContent::Chat(draft));
    }

    /// A draft pane, on one centre line: the project's icon and name with its branch,
    /// changes and folder under them, the message box in the middle, then what else the
    /// pane can be.
    pub(super) fn render_chat_draft(
        &self,
        pane_id: &PaneId,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        // The tab's project, else the repository the folder is in, as the sidebar shows
        // it: a saved project, else Home.
        let repo_project = match &model.repo {
            Repo::At { top, .. } => Some(Project {
                environment: chat.environment.clone(),
                root: top.clone(),
            }),
            Repo::Reading | Repo::None => None,
        };
        let project = self.section_project(self.pane_project(pane_id).or(repo_project.as_ref()));
        let name: SharedString = self.project_name(&project).into();
        let menu_chat = chat.id.clone();
        let header = div()
            .id("chat-draft-project")
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .child(
                div()
                    .id("chat-draft-project-name")
                    .max_w_full()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_2()
                    .rounded(ui_rems(8.0))
                    .cursor_pointer()
                    .hover(move |style| {
                        style.bg(color(blend_rgb(colors.surface, colors.foreground, 0.06)))
                    })
                    // Before the first message, the thread can start in another project.
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.open_draft_project_menu(&menu_chat, event.position(), window, cx);
                        cx.stop_propagation();
                    }))
                    .child(self.project_icon(Some(&project), 28.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rems(26.0))
                            .line_height(ui_rems(32.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color(colors.foreground))
                            .child(name),
                    )
                    .child(controls::glyph(IconName::ChevronDown, muted)),
            )
            .child(
                div()
                    .max_w_full()
                    .truncate()
                    .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                    .text_color(muted)
                    .child(draft_facts(&model.repo, &model.folder_label)),
            )
            .tooltip(controls::tooltip(
                model.folder.clone(),
                Some("Click the project to start this thread in another".into()),
                None,
            ));
        // The column's width follows the pane's, measured as it is drawn: from the next
        // frame, which a change asks for. Until then it is the least.
        let measured = model.draft_width.clone();
        let measure = gpui::canvas(
            move |bounds, window, _| {
                let rem = window.rem_size();
                let area = (bounds.size.width - rem * 2.0).max(px(0.));
                let width = draft_width(
                    area,
                    ui_rems(DRAFT_MIN_WIDTH).to_pixels(rem),
                    ui_rems(DRAFT_MAX_WIDTH).to_pixels(rem),
                );
                if measured.get() != width {
                    measured.set(width);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let width = model.draft_width.get();
        div()
            .id("chat-draft")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_4()
            .py_8()
            .child(measure)
            .child(
                div()
                    .map(|column| {
                        if width > px(0.) {
                            column.w(width)
                        } else {
                            column.w_full().max_w(ui_rems(DRAFT_MIN_WIDTH))
                        }
                    })
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(header)
                    .child(
                        div()
                            .mt_5()
                            .w_full()
                            .child(self.render_chat_footer(chat, model, true, cx)),
                    )
                    .child(
                        div()
                            .mt_4()
                            .w_full()
                            .px_3()
                            .child(self.render_open_instead(chat, model, cx)),
                    ),
            )
            .into_any_element()
    }

    /// What else the pane can be, as a list under the message box and as wide: a muted
    /// heading, then one row each with its icon and name. Changes is dimmed outside a
    /// Git repository.
    fn render_open_instead(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let foreground = color(colors.foreground);
        let hover = color(colors.surface_hover);
        let rows = OpenInstead::ALL
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                let unavailable =
                    choice == OpenInstead::Changes && matches!(model.repo, Repo::None);
                let id = chat.id.clone();
                div()
                    .id(("chat-open-instead", index))
                    .h(ui_rems(32.0))
                    .px(ui_rems(10.0))
                    .flex()
                    .items_center()
                    .gap(ui_rems(10.0))
                    .rounded(ui_rems(8.0))
                    .text_size(ui_rems(UI_BODY_TEXT_SIZE))
                    .text_color(muted)
                    .when(unavailable, |row| {
                        row.opacity(0.5).tooltip(controls::tooltip(
                            "Changes",
                            Some("This folder is not in a Git repository".into()),
                            None,
                        ))
                    })
                    .when(!unavailable, |row| {
                        row.cursor_pointer()
                            .hover(|style| style.bg(hover).text_color(foreground))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_instead(&id, choice, window, cx);
                                cx.notify();
                            }))
                    })
                    .child(
                        controls::glyph(choice.icon(), muted)
                            .with_size(gpui_component::Size::Small),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(choice.label()))
            });
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .mb_1()
                    .px(ui_rems(10.0))
                    .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .child("Open here"),
            )
            .children(rows)
            .into_any_element()
    }

    /// Draft `chat`'s pane and chat, and whether it is a terminal's view.
    fn draft_place(&self, chat: &str) -> Option<(PaneId, ChatContent, bool)> {
        let (pane_id, content) = self.chat_pane(chat)?;
        let view = self
            .workspace
            .as_ref()
            .and_then(|workspace| content::find_leaf(workspace, &pane_id))
            .is_some_and(|leaf| leaf.surface_id().is_some());
        Some((pane_id, content, view))
    }

    /// Puts `choice` in the place of draft `chat`, in its folder; the draft goes. A draft
    /// in a pane of its own is replaced in place; one a terminal shows as its view gives
    /// way to the terminal's shell, or becomes the terminal's view of `choice`. The pane
    /// keeps an arrow back to a new thread (`return_to_draft`).
    pub(super) fn open_instead(
        &mut self,
        chat: &str,
        choice: OpenInstead,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane_id, content, view)) = self.draft_place(chat) else {
            return;
        };
        let draft = content.clone();
        let ChatContent {
            environment,
            folder,
            ..
        } = content;
        let replacement = match choice {
            OpenInstead::Terminal if view => {
                self.mutate(
                    WorkspaceMutation::CloseView {
                        pane_id: pane_id.clone(),
                    },
                    false,
                );
                self.note_draft_return(pane_id, view, None, draft, Opened::Shell);
                return;
            }
            OpenInstead::Terminal => {
                let distribution = match environment {
                    ContentEnvironment::Wsl { distribution } => Some(distribution),
                    ContentEnvironment::Host => None,
                };
                let mutation = self.mutate_in(
                    WorkspaceMutation::ReplacePane {
                        pane_id: pane_id.clone(),
                        with: PaneReplacement::Terminal {
                            cols: self.terminal_cols,
                            rows: self.terminal_rows,
                            working_directory: Some(folder),
                        },
                    },
                    true,
                    distribution,
                );
                self.note_draft_return(pane_id, view, mutation, draft, Opened::Shell);
                return;
            }
            OpenInstead::Changes => {
                self.open_changes_instead(pane_id, view, draft, window, cx);
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
        self.open_pick(pane_id, view, replacement, draft, window, cx);
    }

    /// Changes in the place of `draft`: Files at the repository holding its folder, its
    /// sidebar on the Changes list, once the listing is read.
    fn open_changes_instead(
        &mut self,
        pane_id: PaneId,
        view: bool,
        draft: ChatContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (environment, folder) = (draft.environment.clone(), draft.folder.clone());
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
                        this.open_pick(pane_id, view, content, draft, window, cx);
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

    /// An "Open here" pick, `content`, in the place of `draft`, with the way back noted.
    fn open_pick(
        &mut self,
        pane_id: PaneId,
        view: bool,
        content: PaneContent,
        draft: ChatContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opened = Opened::Content(std::mem::discriminant(&content));
        let mutation = self.put_in_draft_place(&pane_id, view, content, window, cx);
        self.note_draft_return(pane_id, view, mutation, draft, opened);
    }

    /// Notes that `pane_id` (a terminal's own pane when `view`) is to show `opened` in
    /// the place of `draft`: at once for a terminal, which keeps its pane; else once the
    /// receipt of `mutation` names the pane that replaced the draft's.
    fn note_draft_return(
        &mut self,
        pane_id: PaneId,
        view: bool,
        mutation: Option<MutationId>,
        draft: ChatContent,
        opened: Opened,
    ) {
        let back = DraftReturn { draft, opened };
        if view {
            self.chat.returns.insert(pane_id, back);
        } else if let Some(mutation) = mutation {
            self.chat.returning = Some((mutation, back));
        }
    }

    /// Shows `content` where the draft of `pane_id` is: as the terminal's view when
    /// `view`, else in a pane replacing the draft's; the replacement's mutation.
    fn put_in_draft_place(
        &mut self,
        pane_id: &PaneId,
        view: bool,
        content: PaneContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<MutationId> {
        if view {
            self.show_view(pane_id, content, window, cx);
            return None;
        }
        self.mutate_in(
            WorkspaceMutation::ReplacePane {
                pane_id: pane_id.clone(),
                with: PaneReplacement::Content { content },
            },
            true,
            None,
        )
    }

    /// The way back `pane_id` offers while it shows what an "Open here" pick put there.
    fn draft_return(&self, pane_id: &PaneId) -> Option<&DraftReturn> {
        let back = self.chat.returns.get(pane_id)?;
        let leaf = content::find_leaf(self.workspace.as_ref()?, pane_id)?;
        (Opened::of(leaf) == Some(back.opened)).then_some(back)
    }

    /// The arrow back: `pane_id` becomes a new thread draft in the folder and with the
    /// agent of the draft it replaced. A terminal shows it as its view, its shell running
    /// on underneath; another pane is replaced, asking first about unsaved edits.
    fn return_to_draft(&mut self, pane_id: &PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(back) = self.chat.returns.remove(pane_id) else {
            return;
        };
        let DraftReturn { draft, .. } = back;
        let draft = PaneContent::Chat(crate::chat::new_chat(
            draft.harness,
            draft.environment,
            draft.folder,
            draft.prefer,
        ));
        let terminal = self
            .workspace
            .as_ref()
            .and_then(|workspace| content::find_leaf(workspace, pane_id))
            .is_some_and(|leaf| leaf.surface_id().is_some());
        if terminal {
            self.show_view(pane_id, draft, window, cx);
            return;
        }
        let replace = WorkspaceMutation::ReplacePane {
            pane_id: pane_id.clone(),
            with: PaneReplacement::Content { content: draft },
        };
        if self.has_unsaved(pane_id, window, cx) {
            self.guard_unsaved(
                unsaved::CloseScope::Panes(vec![pane_id.clone()]),
                unsaved::AfterUnsaved::Mutate(replace),
                window,
                cx,
            );
        } else {
            self.mutate(replace, true);
        }
    }

    /// The arrow back to a new thread, for a pane an "Open here" pick filled: a bar
    /// across the top of a content pane or view, always shown; for a terminal showing
    /// its shell (`over_shell`), in its top right corner, where a shell's lines rarely
    /// reach, and only while the pointer is in that corner, since it covers the text
    /// there (a terminal often fills the window, so hovering the pane is no sign).
    pub(in crate::gui::workspace) fn render_draft_return(
        &self,
        pane_id: &PaneId,
        over_shell: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let back = self.draft_return(pane_id)?;
        if over_shell != (back.opened == Opened::Shell) {
            return None;
        }
        let colors = *self.colors();
        let id = pane_id.clone();
        let button = controls::icon_button(
            SharedString::from(format!("draft-return-{pane_id}")),
            IconName::CornerDownLeft,
            "Back to a new thread",
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.focus_pane(id.clone());
            this.return_to_draft(&id, window, cx);
            cx.stop_propagation();
            cx.notify();
        }));
        Some(if over_shell {
            let corner = SharedString::from(format!("draft-return-corner-{pane_id}"));
            div()
                .absolute()
                .top_0()
                .right_0()
                .w(px(RETURN_CORNER_WIDTH))
                .h(px(RETURN_CORNER_HEIGHT))
                .group(corner.clone())
                .child(
                    div()
                        .absolute()
                        .top(px(4.0))
                        .right(px(4.0))
                        .invisible()
                        .group_hover(corner, |style| style.visible())
                        .rounded(ui_rems(6.0))
                        .border_1()
                        .border_color(color(colors.border))
                        .bg(color(colors.surface))
                        .shadow_sm()
                        .child(button),
                )
                .into_any_element()
        } else {
            div()
                .flex_none()
                .px_1()
                .py_0p5()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(color(colors.border))
                .child(button)
                .into_any_element()
        })
    }

    /// Forgets the way back of panes that are gone.
    pub(super) fn prune_draft_returns(&mut self) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        self.chat
            .returns
            .retain(|pane_id, _| content::find_leaf(workspace, pane_id).is_some());
    }

    /// Draft `chat` starts again with `harness`, in its place and keeping what was
    /// typed. Only a draft switches: a thread keeps the agent of its first message.
    pub(super) fn switch_draft_agent(
        &mut self,
        chat: &str,
        harness: ChatHarness,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane_id, content, view)) = self.draft_place(chat) else {
            return;
        };
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        if content.harness == harness || !model.draft(&content) {
            return;
        }
        let text = model.composer.read(cx).value().to_string();
        let switched = crate::chat::new_chat(
            harness,
            content.environment,
            content.folder,
            self.config.chat_prefer(harness),
        );
        let id = switched.id.clone();
        self.put_in_draft_place(&pane_id, view, PaneContent::Chat(switched), window, cx);
        if !text.trim().is_empty() {
            self.chat.pending.insert(id, text);
        }
    }

    /// The projects draft `chat` can start in instead, listed at `position`: the listed
    /// projects (Home among them), its own checked.
    fn open_draft_project_menu(
        &mut self,
        chat: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane_id, content, _)) = self.draft_place(chat) else {
            return;
        };
        let current = self.section_project(self.pane_project(&pane_id).or(Some(&Project {
            environment: content.environment.clone(),
            root: content.folder.clone(),
        })));
        let entries = self
            .listed_projects()
            .into_iter()
            .map(|listed| listed.project)
            .filter(|project| !self.project_archived(project))
            .map(|project| {
                let checked = project == current;
                let chat = chat.to_owned();
                let name = self.project_name(&project);
                menu_item(
                    name,
                    cx.listener(move |this, _, window, cx| {
                        this.switch_draft_project(&chat, project.clone(), window, cx);
                        cx.notify();
                    }),
                )
                .checked(checked)
                .into()
            })
            .collect();
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// Draft `chat` starts again in `project`'s folder, keeping its agent and what was
    /// typed. A draft in a tab of its own takes the tab into the project for good; one
    /// a terminal shows leaves the terminal's tab where it is.
    fn switch_draft_project(
        &mut self,
        chat: &str,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane_id, content, view)) = self.draft_place(chat) else {
            return;
        };
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        if !model.draft(&content)
            || (content.environment == project.environment && content.folder == project.root)
        {
            return;
        }
        let text = model.composer.read(cx).value().to_string();
        let switched = crate::chat::new_chat(
            content.harness,
            project.environment.clone(),
            project.root.clone(),
            content.prefer,
        );
        if !text.trim().is_empty() {
            self.chat.pending.insert(switched.id.clone(), text);
        }
        if view {
            self.show_view(&pane_id, PaneContent::Chat(switched), window, cx);
            return;
        }
        let alone = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .sessions
                .iter()
                .flat_map(|session| &session.tabs)
                .find(|tab| tab.layout.leaf_pane() == Some(&pane_id))
                .map(|tab| tab.id.clone())
        });
        self.replace_draft_in_project(pane_id, PaneContent::Chat(switched), alone, project);
    }

    /// Puts `content` in place of the draft in `pane_id`; `tab`, the tab the draft is
    /// alone in, then joins `project` for good.
    fn replace_draft_in_project(
        &mut self,
        pane_id: PaneId,
        content: PaneContent,
        tab: Option<TabId>,
        project: Project,
    ) {
        let mut steps: Vec<Step> = vec![Box::new(move |_| {
            Some(WorkspaceMutation::ReplacePane {
                pane_id,
                with: PaneReplacement::Content { content },
            })
        })];
        if let Some(tab_id) = tab {
            steps.push(Box::new(move |_| {
                Some(WorkspaceMutation::SetTabProject {
                    tab_id,
                    project: Some(project),
                })
            }));
        }
        self.submit_steps(steps);
    }

    /// The agents installed in `environment` as last asked; every agent while the daemon
    /// has not said, or could not.
    pub(in crate::gui::workspace) fn installed_agents(
        &self,
        environment: &ContentEnvironment,
    ) -> Vec<ChatHarness> {
        match self.chat.agents.get(environment) {
            Some(agents) if !agents.is_empty() => agents.clone(),
            _ => ChatHarness::ALL.to_vec(),
        }
    }

    /// Asks the daemon, off the UI thread and once per environment, which agents are
    /// installed in `environment`.
    pub(in crate::gui::workspace) fn want_agents(
        &mut self,
        environment: &ContentEnvironment,
        cx: &mut Context<Self>,
    ) {
        if self.chat.agents.contains_key(environment) {
            return;
        }
        self.chat.agents.insert(environment.clone(), Vec::new());
        let target = self.target.clone();
        let environment = environment.clone();
        cx.spawn(async move |this, cx| {
            let asked = environment.clone();
            let listed = cx
                .background_executor()
                .spawn(async move { target.connect()?.chat_agents(&asked) })
                .await;
            this.update(cx, |this, cx| {
                match listed {
                    Ok(agents) => {
                        this.chat.agents.insert(environment, agents);
                    }
                    // Asked again next time.
                    Err(_) => {
                        this.chat.agents.remove(&environment);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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

    /// A workspace change finished (`None`: it failed): an "Open here" pick notes the
    /// pane that took the draft's place, for the arrow back; a close that threw a draft
    /// away shows "Draft discarded · Undo" for `UNDO_LIFETIME`.
    pub(in crate::gui) fn draft_mutation_finished(
        &mut self,
        receipt: Option<&MutationReceipt>,
        cx: &mut Context<Self>,
    ) {
        if let Some((mutation, back)) = self.chat.returning.take()
            && let Some(pane_id) = receipt
                .filter(|receipt| receipt.mutation_id == mutation)
                .and_then(|receipt| receipt.affected_panes.last())
        {
            self.chat.returns.insert(pane_id.clone(), back);
        }
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
        let chat = crate::chat::new_chat(chat.harness, chat.environment, chat.folder, chat.prefer);
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

    #[test]
    fn the_draft_column_grows_with_the_pane_between_its_bounds() {
        let (least, most) = (px(544.), px(760.));
        assert_eq!(draft_width(px(1500.), least, most), most);
        assert_eq!(draft_width(px(1100.), least, most), px(660.));
        assert_eq!(draft_width(px(700.), least, most), least);
        // A pane narrower than the least is filled.
        assert_eq!(draft_width(px(400.), least, most), px(400.));
    }

    fn chat(id: &str) -> ChatContent {
        ChatContent {
            id: id.into(),
            harness: ChatHarness::Omp,
            environment: ContentEnvironment::Host,
            folder: "/work".into(),
            prefer: None,
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
    fn the_header_line_names_branch_changes_and_folder() {
        let at = |branch: Option<&str>, changes, truncated| Repo::At {
            top: "/work".into(),
            branch: branch.map(Into::into),
            changes,
            truncated,
        };
        assert_eq!(
            draft_facts(&at(Some("main"), 3, false), "…/proj/compi"),
            "main · 3 changes · …/proj/compi"
        );
        assert_eq!(
            draft_facts(&at(Some("main"), 1, false), "/w"),
            "main · 1 change · /w"
        );
        assert_eq!(draft_facts(&at(None, 0, false), "/w"), "No changes · /w");
        assert_eq!(
            draft_facts(&at(None, 5000, true), "/w"),
            "5000+ changes · /w"
        );
        assert_eq!(draft_facts(&Repo::None, "/w"), "/w");
        assert_eq!(draft_facts(&Repo::Reading, "/w"), "/w");
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
