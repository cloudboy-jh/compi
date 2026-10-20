//! Views in place. A terminal pane can carry a view (Files, Settings or Monitor) shown
//! in place of its shell, which keeps running underneath at its size: the layout still
//! sizes the pane in cells, so its PTY never shrinks for the view. The view shares the
//! terminal's `PaneId`, so its content's client state (`ContentUi`, `SettingsUi`,
//! `MonitorUi`) stays keyed by it whichever side shows. While the view shows, keys,
//! text, paste and the wheel go to it, never to the shell, whose program hears it lost
//! focus. A Terminal / <view> switch at the pane's top right, and Cmd/Ctrl+Shift+E,
//! flip the side; Move to its own pane puts the view in a pane of its own beside the
//! shell, and its client state follows it to that pane.
//!
//! `compi tree` shows the shell's folder in its Files view with **Enter folder**, which
//! answers the shell waiting for a folder to change to; leaving the view any other way
//! answers with nothing, which cancels.
use super::*;
use crate::gui::controls::text_button;
use compi_protocol::{MutationReceipt, PaneContent, PaneSide, PaneView};

/// Height of the strip above a shown view that holds its switch.
const VIEW_STRIP_HEIGHT: f32 = 28.0;
/// The switch sits this far from the pane's top and right edges, clear of the program
/// status dot in the top padding band, on either side.
const SWITCH_TOP: f32 = 2.0;
const SWITCH_RIGHT: f32 = 2.0 * PANE_RADIUS + 3.0 * TERMINAL_PADDING;

/// Client-only state of views in place.
#[derive(Default)]
pub(in crate::gui) struct ViewsUi {
    /// The shell waiting in `compi tree` for a folder to change to; its pane's Files
    /// view offers Enter folder.
    shell_reply: Option<ShellReply>,
    /// A `compi tree` that arrived without a window to act in; handled before drawing.
    browse: Option<PaneId>,
    /// The pane whose folder tree shows once its Files view does, for Enter folder.
    reveal_tree: Option<PaneId>,
    /// Move to its own pane on its way.
    moving: Option<MovingView>,
}

/// The shell process a `compi tree` reply goes to. Another process in the pane (a
/// restarted shell, a restarted daemon) never asked, so it is not answered.
struct ShellReply {
    pane_id: PaneId,
    surface_id: SurfaceId,
    lifetime: compi_protocol::ProcessLifetimeId,
    generation: compi_protocol::ServerGeneration,
}

/// A view moving from terminal pane `from` to a pane of its own.
struct MovingView {
    mutation: MutationId,
    from: PaneId,
    /// What it shows, to find its new pane when the receipt was lost.
    content: PaneContent,
    /// The mutation's outcome is unknown; the next workspace accepted tells.
    unconfirmed: bool,
    /// The pane it went to, found in a workspace; its state moves before drawing.
    to: Option<PaneId>,
}

impl ViewsUi {
    /// The terminal whose view is moving to its own pane: its client state stays.
    pub(super) fn moving_from(&self) -> Option<&PaneId> {
        self.moving.as_ref().map(|moving| &moving.from)
    }

    /// Whether `pane_id`'s shell waits in `compi tree` for Enter folder.
    pub(super) fn awaits_folder(&self, pane_id: &PaneId) -> bool {
        self.shell_reply
            .as_ref()
            .is_some_and(|reply| &reply.pane_id == pane_id)
    }
}

/// What the switch and the pane menu call a view.
pub(super) const fn view_label(content: &PaneContent) -> &'static str {
    match content {
        PaneContent::Files(_) => "Files",
        PaneContent::Settings(_) => "Settings",
        PaneContent::Monitor(_) => "Monitor",
    }
}

impl CompiApp {
    fn pane_view(&self, pane_id: &PaneId) -> Option<&PaneView> {
        content::find_leaf(self.workspace.as_ref()?, pane_id)?.view()
    }

    /// The focused pane when it is a terminal: what opens from it opens in its place.
    pub(super) fn focused_terminal(&self) -> Option<PaneId> {
        let pane_id = self.focused_pane()?;
        content::find_leaf(self.workspace.as_ref()?, pane_id)?.surface_id()?;
        Some(pane_id.clone())
    }

    /// Shows terminal pane `pane_id`'s shell or its view. Showing the shell answers a
    /// `compi tree` waiting there with nothing. `false` when the change was not sent.
    pub(super) fn set_pane_side(&mut self, pane_id: &PaneId, side: PaneSide) -> bool {
        if side == PaneSide::Terminal && self.views.awaits_folder(pane_id) {
            self.cancel_shell_reply();
        }
        self.mutate_in(
            WorkspaceMutation::SetPaneSide {
                pane_id: pane_id.clone(),
                side,
            },
            false,
            None,
        )
        .is_some()
    }

    /// Shows the view terminal pane `pane_id` carries while it shows its shell.
    pub(super) fn reveal_view(&mut self, pane_id: &PaneId) {
        if self
            .pane_view(pane_id)
            .is_some_and(|view| view.side == PaneSide::Terminal)
        {
            self.set_pane_side(pane_id, PaneSide::View);
        }
    }

    /// Shows `content` as terminal pane `pane_id`'s view, in place of whatever view it
    /// carries. A Files view in the same environment keeps its edits; one the change
    /// would drop asks about its unsaved edits first, as closing its pane would.
    /// `false` when nothing was sent or asked.
    pub(super) fn show_view(
        &mut self,
        pane_id: &PaneId,
        content: PaneContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keeps_edits = match (self.pane_view(pane_id).map(|view| &view.content), &content) {
            (Some(PaneContent::Files(old)), PaneContent::Files(new)) => {
                old.environment == new.environment
            }
            (Some(PaneContent::Files(_)), _) => false,
            _ => true,
        };
        let mutation = WorkspaceMutation::ShowView {
            pane_id: pane_id.clone(),
            content,
        };
        if keeps_edits {
            return self.mutate_in(mutation, false, None).is_some();
        }
        self.guard_unsaved(
            unsaved::CloseScope::Panes(vec![pane_id.clone()]),
            unsaved::AfterUnsaved::Mutate(mutation),
            window,
            cx,
        );
        true
    }

    /// Close Files, Settings or Monitor: the view goes and the shell shows. Unsaved
    /// edits in it ask first.
    pub(super) fn close_view(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.guard_unsaved(
            unsaved::CloseScope::Panes(vec![pane_id.clone()]),
            unsaved::AfterUnsaved::Mutate(WorkspaceMutation::CloseView {
                pane_id: pane_id.clone(),
            }),
            window,
            cx,
        );
    }

    /// Move to its own pane: terminal pane `pane_id`'s view goes to a new content pane
    /// split beside it, placed as `open_content_beside` places one; the receipt moves
    /// its client state there and focuses it.
    pub(super) fn move_view_to_own_pane(&mut self, pane_id: &PaneId) {
        let geometry = match self
            .content_busy(pane_id)
            .map_or_else(|| self.content_split(pane_id), Err)
        {
            Ok(geometry) => geometry,
            Err(reason) => {
                self.global_error = Some(reason.into());
                return;
            }
        };
        let content = self.pane_view(pane_id).map(|view| view.content.clone());
        if let Some(content) = content
            && let Some(mutation) = self.mutate_in(
                WorkspaceMutation::MoveViewToPane {
                    pane_id: pane_id.clone(),
                    axis: SplitAxis::Horizontal,
                    geometry,
                },
                true,
                None,
            )
        {
            self.views.moving = Some(MovingView {
                mutation,
                from: pane_id.clone(),
                content,
                unconfirmed: false,
                to: None,
            });
        }
    }

    /// A mutation finished (`None`: its outcome is unknown). After Move to its own
    /// pane, the view's client state moves from the terminal to the new pane before
    /// the workspace showing it is accepted, so nothing of it is pruned. With the
    /// outcome unknown it stays with the terminal until a workspace tells where the
    /// view is (see `views_settled`).
    pub(super) fn view_mutation_finished(
        &mut self,
        receipt: Option<&MutationReceipt>,
        cx: &mut Context<Self>,
    ) {
        let Some(moving) = self.views.moving.as_mut() else {
            return;
        };
        let Some(receipt) = receipt else {
            moving.unconfirmed = true;
            return;
        };
        let Some(moving) = self.views.moving.take() else {
            return;
        };
        if let Some(to) = (receipt.mutation_id == moving.mutation)
            .then(|| {
                receipt
                    .affected_panes
                    .iter()
                    .find(|pane| **pane != moving.from)
            })
            .flatten()
        {
            self.move_view_state(&moving.from, &to.clone(), cx);
        }
    }

    fn move_view_state(&mut self, from: &PaneId, to: &PaneId, cx: &mut Context<Self>) {
        self.move_content_pane(from, to, cx);
        self.move_settings_pane(from, to, cx);
        self.move_monitor_pane(from, to, cx);
    }

    /// Cmd/Ctrl+Shift+E: the focused terminal flips between its view and its shell;
    /// without a view it shows its shell's folder in a Files view in its place.
    pub(super) fn toggle_pane_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane_id) = self.focused_terminal() else {
            self.global_error = Some("Select a terminal pane first".into());
            return;
        };
        match self.pane_view(&pane_id).map(|view| view.side) {
            Some(PaneSide::View) => {
                self.set_pane_side(&pane_id, PaneSide::Terminal);
            }
            Some(PaneSide::Terminal) => {
                self.set_pane_side(&pane_id, PaneSide::View);
            }
            None => {
                self.show_shell_folder(&pane_id, window, cx);
            }
        }
    }

    /// The Files view of terminal pane `pane_id` shows its shell's folder. `false`, with
    /// the reason shown, when the shell has not said where it is.
    fn show_shell_folder(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.shell_folder(pane_id) {
            Ok((environment, folder)) => {
                self.open_content_file(
                    pane_id,
                    environment,
                    Some(folder),
                    None,
                    None,
                    false,
                    window,
                    cx,
                );
                true
            }
            Err(reason) => {
                self.global_error = Some(reason.into());
                false
            }
        }
    }

    /// `compi tree` ran in the shell of `pane_id`; the next frame shows its Files view.
    /// One still waiting from another shell is answered with nothing.
    pub(super) fn browse_from_shell(&mut self, pane_id: PaneId) {
        if let Some(previous) = self.views.browse.replace(pane_id.clone())
            && previous != pane_id
        {
            self.reply_to_shell(&previous, None);
        }
    }

    /// Before drawing: a `compi tree` that arrived shows its shell's folder in the
    /// pane's Files view, which then offers Enter folder. A shell still waiting from an
    /// earlier one elsewhere is answered with nothing; one that cannot be shown, too.
    pub(super) fn prepare_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(moving) = self.views.moving.take_if(|moving| moving.to.is_some())
            && let Some(to) = &moving.to
        {
            self.move_view_state(&moving.from, to, cx);
        }
        self.prepare_tree_reveal();
        let Some(pane_id) = self.views.browse.take() else {
            return;
        };
        if !self.views.awaits_folder(&pane_id) {
            self.cancel_shell_reply();
        }
        self.views.shell_reply = None;
        let reply = self.workspace.as_ref().and_then(|workspace| {
            let surface_id = content::find_leaf(workspace, &pane_id)?.surface_id()?;
            Some(ShellReply {
                pane_id: pane_id.clone(),
                surface_id: surface_id.clone(),
                lifetime: workspace.surface(surface_id)?.process_lifetime_id.clone(),
                generation: workspace.server_generation.clone(),
            })
        });
        if self.overlay.is_some() {
            self.reply_to_shell(&pane_id, None);
            self.global_error = Some("Close the active dialog before browsing directories".into());
        } else if let Some(reply) = reply
            && self.show_shell_folder(&pane_id, window, cx)
        {
            self.views.shell_reply = Some(reply);
            self.views.reveal_tree = Some(pane_id);
        } else {
            self.reply_to_shell(&pane_id, None);
        }
    }

    /// Once the Files view a `compi tree` opened shows, its folder tree shows with
    /// Enter folder, beside an open file too.
    fn prepare_tree_reveal(&mut self) {
        let Some(pane_id) = self.views.reveal_tree.clone() else {
            return;
        };
        if !self.views.awaits_folder(&pane_id) {
            self.views.reveal_tree = None;
            return;
        }
        let shown = !self.mutation_pending
            && self.content_panes().any(|pane| pane == &pane_id)
            && self.workspace.as_ref().is_some_and(|workspace| {
                content::find_shown_content(workspace, &pane_id)
                    .is_some_and(|content| content.folder().is_some())
            });
        if shown {
            self.views.reveal_tree = None;
            self.show_folder_tree(&pane_id);
        }
    }

    /// Enter folder: the shell waiting in `compi tree` in `pane_id` changes to `folder`,
    /// and the pane shows the shell again.
    pub(super) fn enter_view_folder(&mut self, pane_id: &PaneId, folder: String) {
        if !self.views.awaits_folder(pane_id) {
            return;
        }
        self.views.shell_reply = None;
        self.reply_to_shell(pane_id, Some(&folder));
        self.set_pane_side(pane_id, PaneSide::Terminal);
    }

    /// The shell waiting in `compi tree` stays where it is.
    pub(super) fn cancel_shell_reply(&mut self) {
        if let Some(reply) = self.views.shell_reply.take() {
            self.reply_to_shell(&reply.pane_id, None);
        }
    }

    /// `cancel_shell_reply` when the waiting shell's pane is in `layout`.
    pub(super) fn cancel_shell_reply_in(&mut self, layout: &LayoutNode) {
        if self
            .views
            .shell_reply
            .as_ref()
            .is_some_and(|reply| leaf_node(layout, &reply.pane_id).is_some())
        {
            self.cancel_shell_reply();
        }
    }

    /// After a workspace update, with `before` the focused terminal view and whether
    /// its shell was hidden then:
    /// - a shell waiting in `compi tree` whose process is gone (restarted, or the
    ///   daemon was) is forgotten unanswered; one whose pane no longer shows its Files
    ///   view (flipped, closed, replaced, moved out or gone) is answered with nothing;
    /// - a view whose move to its own pane had no receipt is found: still on its
    ///   terminal, or in a new pane of the terminal's tab showing the same content;
    /// - the focused terminal's program hears it lost focus when its view came to
    ///   show, or regained it when its shell did.
    pub(super) fn views_settled(&mut self, before: Option<(u64, bool)>) {
        if let Some(reply) = &self.views.shell_reply {
            let same_shell = self.workspace.as_ref().is_some_and(|workspace| {
                workspace.server_generation == reply.generation
                    && workspace
                        .surface(&reply.surface_id)
                        .is_some_and(|surface| surface.process_lifetime_id == reply.lifetime)
            });
            if !same_shell {
                self.views.shell_reply = None;
            }
        }
        if !self.mutation_pending
            && let Some(pane_id) = self.views.shell_reply.as_ref().map(|r| r.pane_id.clone())
        {
            let files_shown = self.workspace.as_ref().is_some_and(|workspace| {
                content::find_leaf(workspace, &pane_id).is_some_and(|leaf| {
                    leaf.surface_id().is_some()
                        && leaf
                            .shown_content()
                            .is_some_and(|content| content.folder().is_some())
                })
            });
            if !files_shown {
                self.cancel_shell_reply();
            }
        }
        self.find_unconfirmed_move();
        let now = self.shell_focus();
        if let (Some((was, was_hidden)), Some((view, hidden))) = (before, now)
            && was == view
            && was_hidden != hidden
            && self.overlay.is_none()
        {
            self.report_focus(!hidden);
        }
    }

    /// Where a view whose move had no receipt went, by the workspace just accepted.
    fn find_unconfirmed_move(&mut self) {
        let Some(moving) = self
            .views
            .moving
            .as_ref()
            .filter(|moving| moving.unconfirmed && moving.to.is_none())
        else {
            return;
        };
        let Some(workspace) = self.workspace.as_ref() else {
            return;
        };
        let tab = workspace
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|tab| leaf_node(&tab.layout, &moving.from).is_some());
        let stayed = tab
            .and_then(|tab| leaf_node(&tab.layout, &moving.from))
            .is_some_and(|leaf| leaf.view().is_some());
        let to = tab.filter(|_| !stayed).and_then(|tab| {
            crate::arrangement::leaves(&tab.layout)
                .into_iter()
                .find_map(|leaf| match leaf {
                    LayoutNode::Content { pane_id, content }
                        if pane_id != moving.from && content == moving.content =>
                    {
                        Some(pane_id)
                    }
                    _ => None,
                })
        });
        match to {
            Some(to) => {
                if let Some(moving) = self.views.moving.as_mut() {
                    moving.to = Some(to);
                }
            }
            // Still on its terminal, or gone: its state stays or goes with the terminal.
            None => self.views.moving = None,
        }
    }

    /// The focused terminal view, and whether its shell is hidden behind its view.
    pub(super) fn shell_focus(&self) -> Option<(u64, bool)> {
        self.focused_view
            .map(|view| (view, self.focused_shell_hidden()))
    }

    /// The Terminal / <view> switch of terminal pane `pane_id`, which carries `view`;
    /// both options are keyed to Cmd/Ctrl+Shift+E, as the Edit / Diff switch is to its
    /// shortcut.
    fn render_view_switch(
        &self,
        pane_id: &PaneId,
        view: &PaneView,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let colors = *self.colors();
        let label = view_label(&view.content);
        let shortcut = crate::commands::Command::BrowseFiles
            .spec()
            .shortcut(crate::commands::Platform::current());
        let option = |id: &'static str, title: &'static str, side: PaneSide, detail: String| {
            let pane = pane_id.clone();
            let mut option = text_button(id, title)
                .selected(view.side == side)
                .detail(detail)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    this.focus_pane(pane.clone());
                    if this.pane_view(&pane).is_some_and(|view| view.side != side) {
                        this.set_pane_side(&pane, side);
                    }
                    cx.notify();
                }));
            if let Some(key) = shortcut {
                option = option.key(key);
            }
            option
        };
        div()
            .id("pane-view-switch")
            .absolute()
            .top(px(SWITCH_TOP))
            .right(px(SWITCH_RIGHT))
            .rounded_sm()
            .bg(color(colors.surface))
            // The switch, not the terminal under it, takes the click.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(controls::segmented(
                "pane-view-sides",
                [
                    option(
                        "pane-view-terminal",
                        "Terminal",
                        PaneSide::Terminal,
                        "Shows the shell, which kept running underneath".to_owned(),
                    ),
                    option(
                        "pane-view-content",
                        label,
                        PaneSide::View,
                        format!("Shows {label} in place of the shell"),
                    ),
                ],
            ))
    }

    /// The switch over a terminal pane showing its shell, when it carries a view; it
    /// overlays the terminal and takes none of its rows.
    pub(super) fn render_shell_switch(
        &self,
        pane_id: &PaneId,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let view = self
            .pane_view(pane_id)
            .filter(|view| view.side == PaneSide::Terminal)?;
        Some(
            self.render_view_switch(pane_id, view, cx)
                .into_any_element(),
        )
    }

    /// Terminal pane `geometry` showing its view at `rect`: the view's content below a
    /// slim strip holding the switch. `None` while it shows its shell.
    pub(super) fn render_shown_view(
        &self,
        id: impl Into<gpui::ElementId>,
        geometry: &layout::PaneLayout,
        rect: layout::Rect,
        cx: &Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let view = self
            .pane_view(&geometry.pane_id)
            .filter(|view| view.side == PaneSide::View)?;
        let colors = *self.colors();
        let body = layout::Rect {
            x: 0.0,
            y: VIEW_STRIP_HEIGHT,
            width: rect.width,
            height: (rect.height - VIEW_STRIP_HEIGHT).max(1.0),
        };
        let (focus_id, menu_id) = (geometry.pane_id.clone(), geometry.pane_id.clone());
        Some(
            div()
                .id(id)
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.width))
                .h(px(rect.height))
                .bg(material_color(
                    colors.background,
                    self.effective_background_opacity(),
                ))
                .child(
                    div()
                        .id("pane-view-strip")
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(rect.width))
                        .h(px(VIEW_STRIP_HEIGHT))
                        .border_b_1()
                        .border_color(color(colors.border))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if this.focused_pane() != Some(&focus_id) {
                                    this.focus_pane(focus_id.clone());
                                    cx.notify();
                                }
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.focus_pane(menu_id.clone());
                                this.open_pane_menu(
                                    &menu_id,
                                    event.position,
                                    Anchor::TopLeft,
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
                .child(self.render_content_pane("pane-view-body", geometry, body, cx))
                .child(self.render_view_switch(&geometry.pane_id, view, cx)),
        )
    }
}
