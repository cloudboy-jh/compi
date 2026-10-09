//! Pane drag-and-drop. A grip at the top centre of each tiled pane opens the pane
//! menu on click and moves the pane on drag:
//!
//! - Over a pane of the same tab: four edge zones and a centre Swap; release
//!   commits one `ArrangeTab`.
//! - Over another tab: release adds the pane to that tab, split to the right;
//!   holding still switches to the tab so an edge of one of its panes can be picked.
//! - Over empty tab-bar space: release makes the pane its own tab.
//!
//! Moving into another tab detaches the pane into its own tab first (unless it
//! already is one), then merges that tab in; no process starts or ends, and a
//! failure between the steps leaves the pane in its own tab. Escape cancels, and
//! no PTY is resized until a move commits.
use super::*;
use crate::arrangement::{self, DropZone};

/// The grip's hit area spans the top padding band, so it never covers a cell.
const GRIP_WIDTH: f32 = 44.0;
const GRIP_DOT: f32 = 3.0;
/// Share of a target pane's width or height taken by each edge zone; the rest is
/// Swap. Panes of another tab cannot swap, so their edge zones meet in the centre.
const EDGE_ZONE: f32 = 0.25;
const CROSS_TAB_EDGE_ZONE: f32 = 0.5;
/// How long the pointer rests on a tab before the drag switches to it.
const SPRING_LOAD_DELAY: Duration = Duration::from_millis(400);

pub(in crate::gui) struct PaneDrag {
    pane_id: PaneId,
    surface_id: SurfaceId,
    tab_id: TabId,
    /// The tab's tree at pickup; a different tree at release cancels a move within it.
    tree: LayoutNode,
    origin: Point<Pixels>,
    position: Point<Pixels>,
    /// Set once the pointer passes the drag threshold; a release before then is a click.
    moving: bool,
    target: Option<DropTarget>,
    /// The unselected tab under the pointer, waiting for `SPRING_LOAD_DELAY`.
    spring: Option<TabId>,
}

enum DropTarget {
    Pane {
        pane_id: PaneId,
        zone: DropZone,
        /// Within the dragged pane's tab: the arrangement to commit. Across tabs the
        /// tree is rebuilt from fresh state when the move commits.
        layout: LayoutNode,
        /// Where the dragged pane lands, in canvas coordinates.
        landing: layout::Rect,
    },
    Tab(TabId),
    NewTab,
}

/// Where a pane moved into another tab goes.
#[derive(Clone)]
enum Placement {
    Right,
    Beside(PaneId, DropZone),
}

/// `target`'s tree with the moved leaf placed. The leaf first joins on the
/// right, so `drop_pane` can take it out again and put it beside a pane.
fn placed(target: &LayoutNode, leaf: LayoutNode, placement: &Placement) -> LayoutNode {
    let pane_id = match &leaf {
        LayoutNode::Pane { pane_id, .. } => pane_id.clone(),
        LayoutNode::Split { .. } => unreachable!("only a pane leaf is moved"),
    };
    let joined = LayoutNode::Split {
        axis: SplitAxis::Horizontal,
        ratio: 0.5,
        first: Box::new(target.clone()),
        second: Box::new(leaf),
    };
    match placement {
        Placement::Right => joined,
        Placement::Beside(beside, zone) => {
            arrangement::drop_pane(&joined, &pane_id, beside, *zone).unwrap_or(joined)
        }
    }
}

impl CompiApp {
    /// The `⋯` grip of tiled pane `index`, revealed while the pointer is over the
    /// pane (`group`). `width` is the pane's width.
    pub(super) fn render_pane_grip(
        &self,
        index: usize,
        group: SharedString,
        pane_id: PaneId,
        width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let dot = || {
            div()
                .size(px(GRIP_DOT))
                .rounded_full()
                .bg(color(colors.muted))
        };
        div()
            .id(("pane-grip", index))
            .absolute()
            .top_0()
            .left(px(((width - GRIP_WIDTH) / 2.0).max(0.0)))
            .w(px(GRIP_WIDTH))
            .h(px(TERMINAL_PADDING))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(GRIP_DOT))
            .rounded_full()
            .invisible()
            .group_hover(group, |style| style.visible())
            .hover(move |style| style.bg(color(colors.surface_hover)))
            .cursor(gpui::CursorStyle::OpenHand)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle);
                    this.focus_pane(pane_id.clone());
                    this.start_pane_drag(pane_id.clone(), event.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .children([dot(), dot(), dot()])
            .into_any_element()
    }

    fn start_pane_drag(&mut self, pane_id: PaneId, origin: Point<Pixels>) {
        let Some(tab) = self.selected_tab() else {
            return;
        };
        let Some((_, surface_id)) = arrangement::leaves(&tab.layout)
            .into_iter()
            .find(|(id, _)| id == &pane_id)
        else {
            return;
        };
        self.pane_drag = Some(PaneDrag {
            pane_id,
            surface_id,
            tab_id: tab.id.clone(),
            tree: tab.layout.clone(),
            origin,
            position: origin,
            moving: false,
            target: None,
            spring: None,
        });
    }

    pub(in crate::gui) fn update_pane_drag(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &self.pane_drag else {
            return;
        };
        if !drag.moving && !drag_threshold_crossed(drag.origin, position) {
            return;
        }
        let (viewport_width, _) = logical_viewport_dimensions(window);
        let hovered = self.tab_bar_target(position, viewport_width);
        let target = match &hovered {
            Some(Some(tab_id)) if tab_id == &drag.tab_id => None,
            Some(Some(tab_id)) => Some(DropTarget::Tab(tab_id.clone())),
            Some(None) => self.can_detach(drag).then_some(DropTarget::NewTab),
            None => self.pane_drop_target(drag, position),
        };
        let selected = self.selected_tab().map(|tab| tab.id.clone());
        let spring = match hovered {
            Some(Some(tab_id)) if Some(&tab_id) != selected.as_ref() => Some(tab_id),
            _ => None,
        };
        let Some(drag) = &mut self.pane_drag else {
            return;
        };
        drag.moving = true;
        drag.position = position;
        drag.target = target;
        if drag.spring != spring {
            drag.spring = spring.clone();
            if let Some(tab_id) = spring {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SPRING_LOAD_DELAY).await;
                    this.update(cx, |this, cx| this.spring_load(&tab_id, cx))
                        .ok();
                })
                .detach();
            }
        }
    }

    /// Still resting on the same tab: switch to it, keeping the drag.
    fn spring_load(&mut self, tab_id: &TabId, cx: &mut Context<Self>) {
        if self
            .pane_drag
            .as_ref()
            .is_none_or(|drag| drag.spring.as_ref() != Some(tab_id))
        {
            return;
        }
        self.select_terminal(tab_id.clone(), false);
        if let Some(drag) = &mut self.pane_drag {
            drag.spring = None;
        }
        cx.notify();
    }

    /// `Some(Some(tab))` over a visible tab, `Some(None)` over empty tab-bar space,
    /// `None` outside the tab bar.
    fn tab_bar_target(
        &self,
        position: Point<Pixels>,
        viewport_width: f32,
    ) -> Option<Option<TabId>> {
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        let header = header_metrics(viewport_width);
        let tab_region_right = viewport_width
            - WINDOW_CONTROLS_WIDTH
            - HEADER_BUTTON_SLOT_WIDTH
            - header.pane_actions_width;
        if !(0.0..=CHROME_HEIGHT).contains(&y) || x < TITLEBAR_BRAND_WIDTH || x >= tab_region_right
        {
            return None;
        }
        let offset = x - TITLEBAR_BRAND_WIDTH - f32::from(self.tab_scroll_handle.offset().x);
        let index = (offset / header.tab_width).floor().max(0.0) as usize;
        let workspace = self.workspace.as_ref()?;
        let session_id = self.state.selected_session.as_ref()?;
        let session = workspace
            .sessions
            .iter()
            .find(|session| &session.id == session_id)?;
        Some(
            self.state
                .visible_tabs(session)
                .nth(index)
                .map(|tab| tab.id.clone()),
        )
    }

    /// A pane can become its own tab only if its tab has other panes.
    fn can_detach(&self, drag: &PaneDrag) -> bool {
        self.tab_by_id(&drag.tab_id)
            .is_some_and(|tab| matches!(tab.layout, LayoutNode::Split { .. }))
    }

    /// The tiled pane under `position` (not the dragged one), the zone the pointer
    /// is in, and where the dragged pane would land.
    fn pane_drop_target(&self, drag: &PaneDrag, position: Point<Pixels>) -> Option<DropTarget> {
        let layout = self.visible_layout()?;
        let offset = self.workspace_scroll.offset();
        let x = f32::from(position.x - offset.x) - self.sidebar_extent();
        let y = f32::from(position.y - offset.y) - CHROME_HEIGHT;
        let target = layout.panes.iter().find(|pane| {
            let rect = pane.rect;
            pane.pane_id != drag.pane_id
                && x >= rect.x
                && x < rect.x + rect.width
                && y >= rect.y
                && y < rect.y + rect.height
        })?;
        let tab = self.selected_tab()?;
        let across = tab.id != drag.tab_id;
        let rect = target.rect;
        let u = (x - rect.x) / rect.width;
        let v = (y - rect.y) / rect.height;
        let (distance, edge) = [
            (u, DropZone::Left),
            (1.0 - u, DropZone::Right),
            (v, DropZone::Top),
            (1.0 - v, DropZone::Bottom),
        ]
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))?;
        let zone = if across || distance < EDGE_ZONE {
            edge
        } else {
            DropZone::Swap
        };
        let next = if across {
            let leaf = LayoutNode::Pane {
                pane_id: drag.pane_id.clone(),
                surface_id: drag.surface_id.clone(),
            };
            placed(
                &tab.layout,
                leaf,
                &Placement::Beside(target.pane_id.clone(), zone),
            )
        } else {
            arrangement::drop_pane(&tab.layout, &drag.pane_id, &target.pane_id, zone)?
        };
        let floating = |pane: &PaneId| self.state.is_floating(pane);
        let tiled = layout::without_panes(&next, &floating)?;
        let landing = layout::compute_layout(&tiled, self.float_area, self.metrics())
            .pane(&drag.pane_id)?
            .rect;
        Some(DropTarget::Pane {
            pane_id: target.pane_id.clone(),
            zone,
            layout: next,
            landing,
        })
    }

    /// Release: a click opens the pane menu at the grip; a drag commits its target.
    pub(in crate::gui) fn finish_pane_drag(&mut self, position: Point<Pixels>) {
        let Some(drag) = self.pane_drag.take() else {
            return;
        };
        if !drag.moving {
            self.open_overlay(
                Overlay::PaneActions {
                    position: Some(position),
                },
                "",
            );
            return;
        }
        let Some(target) = drag.target else {
            return;
        };
        let selected = self.selected_tab().map(|tab| tab.id.clone());
        match target {
            DropTarget::Pane { layout, .. } if selected.as_ref() == Some(&drag.tab_id) => {
                if self
                    .tab_by_id(&drag.tab_id)
                    .is_none_or(|tab| tab.layout != drag.tree)
                {
                    self.global_error =
                        Some("The tab changed while dragging; the move was cancelled".into());
                    return;
                }
                self.mutate(
                    WorkspaceMutation::ArrangeTab {
                        tab_id: drag.tab_id,
                        layout,
                    },
                    false,
                );
            }
            DropTarget::Pane { pane_id, zone, .. } => {
                if let Some(tab_id) = selected {
                    self.move_pane_to_tab(drag.pane_id, tab_id, Placement::Beside(pane_id, zone));
                }
            }
            DropTarget::Tab(tab_id) => {
                self.select_terminal(tab_id.clone(), false);
                self.move_pane_to_tab(drag.pane_id, tab_id, Placement::Right);
            }
            DropTarget::NewTab => self.mutate(
                WorkspaceMutation::DetachPane {
                    pane_id: drag.pane_id,
                },
                true,
            ),
        }
    }

    /// Move `pane_id` into `tab_id` (same workspace): detach it into its own tab
    /// unless it already is one, then merge that tab in at `placement`. Both steps
    /// read fresh state, so the tree sent matches the daemon's tabs.
    fn move_pane_to_tab(&mut self, pane_id: PaneId, tab_id: TabId, placement: Placement) {
        if self.update_quiesced.load(Ordering::Acquire) {
            return;
        }
        if self.mutation_pending {
            self.global_error = Some("A workspace change is still pending".into());
            return;
        }
        self.mutation_pending = true;
        let sender = self.event_tx.clone();
        let target = self.target.clone();
        thread::spawn(move || {
            let request = |workspace: &WorkspaceSnapshot, operation| MutationRequest {
                server_id: workspace.server_id.clone(),
                expected_generation: workspace.server_generation.clone(),
                expected_revision: workspace.revision,
                mutation_id: MutationId::new(format!(
                    "gui-{}-{:x}-{}",
                    std::process::id(),
                    *MUTATION_RUN_NONCE,
                    NEXT_MUTATION_ID.fetch_add(1, Ordering::Relaxed)
                )),
                operation,
                launch: None,
            };
            let tab_of = |workspace: &WorkspaceSnapshot, pane: &PaneId| {
                workspace
                    .sessions
                    .iter()
                    .flat_map(|session| &session.tabs)
                    .find(|tab| layout_contains_pane(&tab.layout, pane))
                    .cloned()
            };
            let result = (|| -> crate::Result<_> {
                let mut client = target.connect()?;
                let mut workspace = client.workspace()?;
                let source =
                    tab_of(&workspace, &pane_id).ok_or("The moved pane no longer exists")?;
                if matches!(source.layout, LayoutNode::Split { .. }) {
                    client.submit_mutation(request(
                        &workspace,
                        WorkspaceMutation::DetachPane {
                            pane_id: pane_id.clone(),
                        },
                    ))?;
                    workspace = client.workspace()?;
                }
                let source =
                    tab_of(&workspace, &pane_id).ok_or("The moved pane no longer exists")?;
                let receiving = workspace
                    .sessions
                    .iter()
                    .flat_map(|session| &session.tabs)
                    .find(|tab| tab.id == tab_id)
                    .ok_or("The target tab no longer exists")?;
                let layout = placed(&receiving.layout, source.layout.clone(), &placement);
                let receipt = client.submit_mutation(request(
                    &workspace,
                    WorkspaceMutation::MergeTabs {
                        tab_id: tab_id.clone(),
                        sources: vec![source.id.clone()],
                        layout,
                    },
                ))?;
                Ok((client.workspace()?, receipt))
            })()
            .map_err(|error| error.to_string());
            sender.send(UiEvent::MutationFinished {
                result,
                select_created: false,
            });
        });
    }

    /// Escape: drop the drag without changing anything.
    pub(in crate::gui) fn cancel_pane_drag(&mut self) -> bool {
        self.pane_drag.take().is_some()
    }

    /// The tab-bar drop under a moving pane: a tab to merge into, or new-tab space.
    pub(super) fn pane_drag_tab_target(&self) -> Option<Option<&TabId>> {
        match self.pane_drag.as_ref()?.target.as_ref()? {
            DropTarget::Tab(tab_id) => Some(Some(tab_id)),
            DropTarget::NewTab => Some(None),
            DropTarget::Pane { .. } => None,
        }
    }

    /// While a pane is being moved: dim it where it was, and show the zones over
    /// the pane under the pointer.
    pub(super) fn decorate_dragged_pane(
        &self,
        pane: gpui::Stateful<gpui::Div>,
        pane_id: &PaneId,
        width: f32,
        height: f32,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(drag) = self.pane_drag.as_ref().filter(|drag| drag.moving) else {
            return pane;
        };
        if &drag.pane_id == pane_id {
            return pane.child(div().absolute().inset_0().bg(material_color(
                self.terminal_theme.terminal().background,
                0.6,
            )));
        }
        let across = self.selected_tab().is_some_and(|tab| tab.id != drag.tab_id);
        match &drag.target {
            Some(DropTarget::Pane {
                pane_id: target,
                zone,
                ..
            }) if target == pane_id => {
                pane.child(self.render_drop_zones(*zone, width, height, across))
            }
            _ => pane,
        }
    }

    /// Edge wedges, plus the Swap box within the dragged pane's own tab.
    fn render_drop_zones(
        &self,
        hovered: DropZone,
        width: f32,
        height: f32,
        across: bool,
    ) -> AnyElement {
        let colors = *self.colors();
        let accent = color(colors.accent);
        let edge = if across {
            CROSS_TAB_EDGE_ZONE
        } else {
            EDGE_ZONE
        };
        let (ix, iy) = (width * edge, height * edge);
        let swap = hovered == DropZone::Swap;
        div()
            .absolute()
            .inset_0()
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let at =
                            |x: f32, y: f32| point(bounds.left() + px(x), bounds.top() + px(y));
                        let (w, h) = (width, height);
                        for (zone, corners) in [
                            (
                                DropZone::Left,
                                [(0.0, 0.0), (ix, iy), (ix, h - iy), (0.0, h)],
                            ),
                            (
                                DropZone::Right,
                                [(w, 0.0), (w - ix, iy), (w - ix, h - iy), (w, h)],
                            ),
                            (
                                DropZone::Top,
                                [(0.0, 0.0), (w, 0.0), (w - ix, iy), (ix, iy)],
                            ),
                            (
                                DropZone::Bottom,
                                [(0.0, h), (w, h), (w - ix, h - iy), (ix, h - iy)],
                            ),
                        ] {
                            let mut path = PathBuilder::fill();
                            path.move_to(at(corners[0].0, corners[0].1));
                            for (x, y) in &corners[1..] {
                                path.line_to(at(*x, *y));
                            }
                            path.close();
                            if let Ok(path) = path.build() {
                                let alpha = if zone == hovered { 0.3 } else { 0.1 };
                                window.paint_path(path, accent.opacity(alpha));
                            }
                        }
                        // Seams from each corner to the Swap box, so every zone reads.
                        let mut seams = PathBuilder::stroke(px(1.0));
                        for (corner, inner) in [
                            ((0.0, 0.0), (ix, iy)),
                            ((w, 0.0), (w - ix, iy)),
                            ((0.0, h), (ix, h - iy)),
                            ((w, h), (w - ix, h - iy)),
                        ] {
                            seams.move_to(at(corner.0, corner.1));
                            seams.line_to(at(inner.0, inner.1));
                        }
                        if let Ok(path) = seams.build() {
                            window.paint_path(path, accent.opacity(0.4));
                        }
                    },
                )
                .size_full(),
            )
            .when(!across, |zones| {
                zones.child(
                    div()
                        .absolute()
                        .left(px(ix))
                        .top(px(iy))
                        .w(px(width - 2.0 * ix))
                        .h(px(height - 2.0 * iy))
                        .rounded_md()
                        .border_1()
                        .border_color(accent.opacity(0.6))
                        .bg(accent.opacity(if swap { 0.3 } else { 0.06 }))
                        .flex()
                        .justify_center()
                        // The label sits at the top so the card under the pointer rarely
                        // hides it.
                        .items_start()
                        .pt_2()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(colors.foreground))
                        .child("Swap"),
                )
            })
            .into_any_element()
    }

    /// The outline of where the dragged pane lands, in canvas coordinates.
    pub(super) fn render_drop_landing(&self) -> Option<AnyElement> {
        let Some(DropTarget::Pane { landing: rect, .. }) = self.pane_drag.as_ref()?.target.as_ref()
        else {
            return None;
        };
        let rect = *rect;
        let accent = color(self.colors().accent);
        Some(
            div()
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.width))
                .h(px(rect.height))
                .when(self.comfy(), |landing| landing.rounded(px(PANE_RADIUS)))
                .border_2()
                .border_color(accent)
                .bg(accent.opacity(0.1))
                .into_any_element(),
        )
    }

    /// The card that follows the pointer while a pane is moved.
    pub(super) fn render_pane_drag_card(&self) -> Option<AnyElement> {
        let drag = self.pane_drag.as_ref().filter(|drag| drag.moving)?;
        let colors = *self.colors();
        let (title, _) = self.floating_caption(&drag.pane_id);
        Some(
            div()
                .absolute()
                .left(drag.position.x + px(12.0))
                .top(drag.position.y + px(12.0))
                .max_w(px(280.0))
                .px_3()
                .py_2()
                .rounded_md()
                .shadow_lg()
                .bg(color(colors.surface))
                .border_1()
                .border_color(color(colors.accent))
                .text_size(px(UI_SMALL_TEXT_SIZE))
                .text_color(color(colors.foreground))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(title)
                .into_any_element(),
        )
    }
}
