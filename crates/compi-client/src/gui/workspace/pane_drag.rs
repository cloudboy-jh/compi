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
/// The centre Swap box spans at most this share of a target pane each way, and at
/// most `SWAP_MAX` pixels. Panes of another tab cannot swap, so there the four edge
/// zones meet in the centre.
const SWAP_SHARE: f32 = 0.3;
const SWAP_MAX: f32 = 160.0;
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

/// The Swap box of a `width` × `height` pane in pane coordinates; a point at the
/// centre when the pane cannot swap.
fn swap_box(width: f32, height: f32, swap: bool) -> layout::Rect {
    let (w, h) = if swap {
        (
            (width * SWAP_SHARE).min(SWAP_MAX),
            (height * SWAP_SHARE).min(SWAP_MAX),
        )
    } else {
        (0.0, 0.0)
    };
    layout::Rect {
        x: (width - w) / 2.0,
        y: (height - h) / 2.0,
        width: w,
        height: h,
    }
}

/// The four edge zones as quads running from a pane edge's corners to the Swap
/// box's matching corners, in the order Left, Right, Top, Bottom.
fn edge_zones(width: f32, height: f32, inner: layout::Rect) -> [(DropZone, [(f32, f32); 4]); 4] {
    let (l, t, r, b) = (
        inner.x,
        inner.y,
        inner.x + inner.width,
        inner.y + inner.height,
    );
    let (w, h) = (width, height);
    [
        (DropZone::Left, [(0.0, 0.0), (l, t), (l, b), (0.0, h)]),
        (DropZone::Right, [(w, 0.0), (w, h), (r, b), (r, t)]),
        (DropZone::Top, [(0.0, 0.0), (w, 0.0), (r, t), (l, t)]),
        (DropZone::Bottom, [(0.0, h), (l, b), (r, b), (w, h)]),
    ]
}

/// The zone under `(x, y)` in pane coordinates, matching what `edge_zones` and
/// `swap_box` draw: Swap inside the box, otherwise the edge quad containing it.
fn zone_at(x: f32, y: f32, width: f32, height: f32, swap: bool) -> DropZone {
    let inner = swap_box(width, height, swap);
    if swap
        && x >= inner.x
        && x <= inner.x + inner.width
        && y >= inner.y
        && y <= inner.y + inner.height
    {
        return DropZone::Swap;
    }
    // Points on a shared edge belong to the first quad that contains them.
    let contains = |quad: &[(f32, f32); 4]| {
        let mut sign = 0.0_f32;
        for index in 0..4 {
            let (ax, ay) = quad[index];
            let (bx, by) = quad[(index + 1) % 4];
            let cross = (bx - ax) * (y - ay) - (by - ay) * (x - ax);
            if cross != 0.0 {
                if sign != 0.0 && cross.signum() != sign {
                    return false;
                }
                sign = cross.signum();
            }
        }
        true
    };
    edge_zones(width, height, inner)
        .into_iter()
        .find(|(_, quad)| contains(quad))
        .map_or(DropZone::Bottom, |(zone, _)| zone)
}

/// The half of a `width` × `height` pane an edge drop gives the moved pane.
fn landing_half(zone: DropZone, width: f32, height: f32) -> Option<layout::Rect> {
    let (half_w, half_h) = (width / 2.0, height / 2.0);
    let (x, y, w, h) = match zone {
        DropZone::Left => (0.0, 0.0, half_w, height),
        DropZone::Right => (half_w, 0.0, half_w, height),
        DropZone::Top => (0.0, 0.0, width, half_h),
        DropZone::Bottom => (0.0, half_h, width, half_h),
        DropZone::Swap => return None,
    };
    Some(layout::Rect {
        x,
        y,
        width: w,
        height: h,
    })
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
    /// is in, and the arrangement a drop there would commit.
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
        let zone = zone_at(x - rect.x, y - rect.y, rect.width, rect.height, !across);
        let layout = if across {
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
        Some(DropTarget::Pane {
            pane_id: target.pane_id.clone(),
            zone,
            layout,
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

    /// A pane is being moved (past the drag threshold).
    pub(super) fn pane_drag_moving(&self) -> bool {
        self.pane_drag.as_ref().is_some_and(|drag| drag.moving)
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

    /// Thin seams between the edge zones, the half an edge drop fills, and, within
    /// the dragged pane's own tab, the Swap box. Only the hovered zone is filled.
    fn render_drop_zones(
        &self,
        hovered: DropZone,
        width: f32,
        height: f32,
        across: bool,
    ) -> AnyElement {
        let colors = *self.colors();
        let accent = color(colors.accent);
        let inner = swap_box(width, height, !across);
        let swap = hovered == DropZone::Swap;
        let radius = if self.comfy() { PANE_RADIUS } else { 0.0 };
        div()
            .absolute()
            .inset_0()
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let at =
                            |x: f32, y: f32| point(bounds.left() + px(x), bounds.top() + px(y));
                        // Each corner of the pane joins the matching Swap-box corner.
                        let (w, h) = (width, height);
                        let (l, t) = (inner.x, inner.y);
                        let (r, b) = (inner.x + inner.width, inner.y + inner.height);
                        let mut seams = PathBuilder::stroke(px(1.0));
                        for ((ox, oy), (ix, iy)) in [
                            ((0.0, 0.0), (l, t)),
                            ((w, 0.0), (r, t)),
                            ((0.0, h), (l, b)),
                            ((w, h), (r, b)),
                        ] {
                            seams.move_to(at(ox, oy));
                            seams.line_to(at(ix, iy));
                        }
                        if let Ok(path) = seams.build() {
                            window.paint_path(path, accent.opacity(0.35));
                        }
                    },
                )
                .size_full(),
            )
            .when_some(landing_half(hovered, width, height), |zones, half| {
                zones.child(
                    div()
                        .absolute()
                        .left(px(half.x))
                        .top(px(half.y))
                        .w(px(half.width))
                        .h(px(half.height))
                        .rounded(px(radius))
                        .border_2()
                        .border_color(accent)
                        .bg(accent.opacity(0.18)),
                )
            })
            .when(!across, |zones| {
                zones.child(
                    div()
                        .absolute()
                        .left(px(inner.x))
                        .top(px(inner.y))
                        .w(px(inner.width))
                        .h(px(inner.height))
                        .rounded_md()
                        .border_1()
                        .border_color(accent.opacity(if swap { 1.0 } else { 0.5 }))
                        .when(swap, |swap_box| swap_box.bg(accent.opacity(0.3)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .text_color(color(colors.foreground))
                        .child("Swap"),
                )
            })
            .into_any_element()
    }

    /// The card that follows the pointer while a pane is moved.
    pub(super) fn render_pane_drag_card(&self) -> Option<AnyElement> {
        let drag = self.pane_drag.as_ref().filter(|drag| drag.moving)?;
        let colors = *self.colors();
        let (title, _) = self.floating_caption(&drag.pane_id);
        Some(
            div()
                .absolute()
                // Below and right of the pointer, clear of the zone it is over.
                .left(drag.position.x + px(18.0))
                .top(drag.position.y + px(22.0))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_follow_the_drawn_wedges_and_swap_box() {
        // 1000 × 400: the Swap box is 160 × 120, centred at (420..580, 140..260).
        let at = |x, y| zone_at(x, y, 1000.0, 400.0, true);
        assert_eq!(at(500.0, 200.0), DropZone::Swap);
        assert_eq!(at(421.0, 141.0), DropZone::Swap);
        assert_eq!(at(10.0, 200.0), DropZone::Left);
        assert_eq!(at(990.0, 200.0), DropZone::Right);
        assert_eq!(at(500.0, 10.0), DropZone::Top);
        assert_eq!(at(500.0, 390.0), DropZone::Bottom);
        // Just above the box is Top, though the left edge is nearer in proportion.
        assert_eq!(at(430.0, 130.0), DropZone::Top);
        // Beside the box, the side edge's wedge reaches the box.
        assert_eq!(at(410.0, 200.0), DropZone::Left);
    }

    #[test]
    fn panes_of_another_tab_have_no_swap() {
        let at = |x, y| zone_at(x, y, 600.0, 600.0, false);
        assert_eq!(at(300.0, 300.0 - 1.0), DropZone::Top);
        assert_eq!(at(300.0, 300.0 + 1.0), DropZone::Bottom);
        assert_eq!(at(299.0, 300.0), DropZone::Left);
        assert_eq!(at(301.0, 300.0), DropZone::Right);
    }

    #[test]
    fn edge_drops_fill_the_matching_half() {
        let half = |zone| landing_half(zone, 800.0, 400.0).map(|r| (r.x, r.y, r.width, r.height));
        assert_eq!(half(DropZone::Left), Some((0.0, 0.0, 400.0, 400.0)));
        assert_eq!(half(DropZone::Right), Some((400.0, 0.0, 400.0, 400.0)));
        assert_eq!(half(DropZone::Top), Some((0.0, 0.0, 800.0, 200.0)));
        assert_eq!(half(DropZone::Bottom), Some((0.0, 200.0, 800.0, 200.0)));
        assert_eq!(half(DropZone::Swap), None);
    }
}
