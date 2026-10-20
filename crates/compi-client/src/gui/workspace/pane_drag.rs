//! Pane drag-and-drop. A grip at the top centre of each tiled pane, shown on hover,
//! opens the pane menu on click and moves the pane on drag; the × beside it closes
//! the pane:
//!
//! - Over a pane of the same tab: four edge zones and a centre Swap; release
//!   commits one `ArrangeTab`.
//! - Over another tab: release adds the pane to that tab, split to the right;
//!   holding still switches to the tab so an edge of one of its panes can be picked.
//! - Over empty tab-bar space: release makes the pane its own tab.
//!
//! Only the zone under the pointer is drawn: the half an edge drop gives the pane,
//! or the Swap box. A float moved by its title docks the same way into a pane of
//! its own tab.
//!
//! Moving into another tab detaches the pane into its own tab first (unless it
//! already is one), then merges that tab in; no process starts or ends, and a
//! failure between the steps leaves the pane in its own tab. Escape cancels, and
//! no PTY is resized until a move commits and its landing (`motion`) ends.
use super::*;
use crate::arrangement::{self, DropZone};
use gpui::{Animation, AnimationExt as _, ease_out_quint};

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
/// How long the drag card takes to lift off at pickup.
const PICKUP_DURATION: Duration = Duration::from_millis(120);
/// How far the drag card rises as it lifts.
const PICKUP_RISE: f32 = 6.0;
/// The accent fill of the zone under the pointer.
const ZONE_FILL_ALPHA: f32 = 0.16;
/// Numbers each pickup, so the drag card's lift plays once per drag.
static NEXT_PICKUP: AtomicU64 = AtomicU64::new(0);

pub(in crate::gui) struct PaneDrag {
    pane_id: PaneId,
    /// The dragged terminal or content leaf, as it is in `tree`.
    leaf: LayoutNode,
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
    pickup: u64,
}

enum DropTarget {
    Pane(PaneTarget),
    Tab(TabId),
    NewTab,
}

/// A tiled pane under a dragged pane or float, the zone the pointer is in, and
/// what a release there commits.
pub(in crate::gui) struct PaneTarget {
    pane_id: PaneId,
    zone: DropZone,
    /// Within the dragged pane's tab: the arrangement to commit. Across tabs the
    /// tree is rebuilt from fresh state when the move commits.
    layout: LayoutNode,
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
    let pane_id = leaf.leaf_pane().expect("only a pane leaf is moved").clone();
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

/// The tiled pane at `(x, y)` in terminal-area coordinates, other than `skip`,
/// and the zone of it the point is in. `None` over gutters and empty space.
fn pane_zone_at<'a>(
    panes: &'a [layout::PaneLayout],
    x: f32,
    y: f32,
    skip: &PaneId,
    swap: bool,
) -> Option<(&'a layout::PaneLayout, DropZone)> {
    let target = panes
        .iter()
        .find(|pane| &pane.pane_id != skip && pane.rect.contains(layout::Point { x, y }))?;
    let rect = target.rect;
    let zone = zone_at(x - rect.x, y - rect.y, rect.width, rect.height, swap);
    Some((target, zone))
}

/// `tree` with floating pane `float` docked at `zone` of `target`. An edge drop
/// that would leave the tree as it is docks the float into its current tile;
/// `None` when `float` is not in `tree` (it floats from another tab).
fn dock_layout(
    tree: &LayoutNode,
    float: &PaneId,
    target: &PaneId,
    zone: DropZone,
) -> Option<LayoutNode> {
    if !layout_contains_pane(tree, float) || !layout_contains_pane(tree, target) {
        return None;
    }
    Some(arrangement::drop_pane(tree, float, target, zone).unwrap_or_else(|| tree.clone()))
}

impl CompiApp {
    /// The `⋯` grip of tiled pane `index`, revealed while the pointer is over the pane
    /// (`group`). `width` is the pane's width.
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
            .absolute()
            .top_0()
            .left(px(((width - GRIP_WIDTH) / 2.0).max(0.0)))
            .h(px(TERMINAL_PADDING))
            .flex()
            .items_center()
            .invisible()
            .group_hover(group, |style| style.visible())
            .child(
                div()
                    .id(("pane-grip", index))
                    .w(px(GRIP_WIDTH))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(GRIP_DOT))
                    .rounded_full()
                    .hover(move |style| style.bg(color(colors.surface_hover)))
                    .cursor(gpui::CursorStyle::OpenHand)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus_handle, cx);
                            this.focus_pane(pane_id.clone());
                            this.start_pane_drag(pane_id.clone(), event.position);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .children([dot(), dot(), dot()]),
            )
            .into_any_element()
    }

    /// The tiled pane's menu from its grip ⋯, hanging under the grip.
    pub(super) fn open_grip_menu(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rect) = self
            .visible_layout()
            .and_then(|layout| layout.panes.iter().find(|pane| &pane.pane_id == pane_id))
            .map(|pane| pane.rect)
        else {
            return;
        };
        // The grip's centre: it is centred on the pane.
        let offset = self.workspace_scroll.offset();
        let position = point(
            px(rect.x + rect.width / 2.0 + self.sidebar_extent()) + offset.x,
            px(rect.y + TERMINAL_PADDING + CHROME_HEIGHT) + offset.y,
        );
        self.open_pane_menu(pane_id, position, Anchor::TopCenter, window, cx);
    }

    fn start_pane_drag(&mut self, pane_id: PaneId, origin: Point<Pixels>) {
        let Some(tab) = self.selected_tab() else {
            return;
        };
        let Some(leaf) = arrangement::leaves(&tab.layout)
            .into_iter()
            .find(|leaf| leaf.leaf_pane() == Some(&pane_id))
        else {
            return;
        };
        self.pane_drag = Some(PaneDrag {
            pane_id,
            leaf,
            tab_id: tab.id.clone(),
            tree: tab.layout.clone(),
            origin,
            position: origin,
            moving: false,
            target: None,
            spring: None,
            pickup: NEXT_PICKUP.fetch_add(1, Ordering::Relaxed),
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
        let hovered = self.tab_bar_target(position, window);
        // Over the same pane and zone the target stands; nothing is recomputed.
        let hit = if hovered.is_none() {
            let across = self.selected_tab().is_some_and(|tab| tab.id != drag.tab_id);
            self.zone_under(position, &drag.pane_id, !across)
        } else {
            None
        };
        let keep = hit
            .as_ref()
            .is_some_and(|hit| same_target(drag.target.as_ref(), hit));
        let target = if keep {
            None
        } else {
            match &hovered {
                Some(Some(tab_id)) if tab_id == &drag.tab_id => None,
                Some(Some(tab_id)) => Some(DropTarget::Tab(tab_id.clone())),
                Some(None) => self.can_detach(drag).then_some(DropTarget::NewTab),
                None => hit.and_then(|hit| self.pane_drop_target(drag, hit)),
            }
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
        if !keep {
            drag.target = target;
        }
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
    fn tab_bar_target(&self, position: Point<Pixels>, window: &Window) -> Option<Option<TabId>> {
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        let (viewport_width, _) = logical_viewport_dimensions(window);
        let leading_width = titlebar_leading_width(window);
        let header = header_metrics(viewport_width, leading_width);
        let tab_region_right = viewport_width
            - WINDOW_CONTROLS_WIDTH
            - HEADER_BUTTON_SLOT_WIDTH
            - header.pane_actions_width;
        if !(0.0..=CHROME_HEIGHT).contains(&y) || x < leading_width || x >= tab_region_right {
            return None;
        }
        let offset = x - leading_width - f32::from(self.tab_scroll_handle.offset().x);
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

    /// `position` in terminal-area coordinates: those of the tiled pane rects.
    fn canvas_point(&self, position: Point<Pixels>) -> (f32, f32) {
        let offset = self.workspace_scroll.offset();
        (
            f32::from(position.x - offset.x) - self.sidebar_extent(),
            f32::from(position.y - offset.y) - CHROME_HEIGHT,
        )
    }

    /// The visible tiled pane under `position` (not `skip`) and the zone the
    /// pointer is in.
    fn zone_under(
        &self,
        position: Point<Pixels>,
        skip: &PaneId,
        swap: bool,
    ) -> Option<(PaneId, DropZone)> {
        let layout = self.visible_layout()?;
        let (x, y) = self.canvas_point(position);
        pane_zone_at(&layout.panes, x, y, skip, swap)
            .map(|(pane, zone)| (pane.pane_id.clone(), zone))
    }

    /// A drop of the dragged pane at `zone` of tiled pane `pane_id`, with the
    /// arrangement it would commit.
    fn pane_drop_target(
        &self,
        drag: &PaneDrag,
        (pane_id, zone): (PaneId, DropZone),
    ) -> Option<DropTarget> {
        let tab = self.selected_tab()?;
        let layout = if tab.id != drag.tab_id {
            placed(
                &tab.layout,
                drag.leaf.clone(),
                &Placement::Beside(pane_id.clone(), zone),
            )
        } else {
            arrangement::drop_pane(&tab.layout, &drag.pane_id, &pane_id, zone)?
        };
        Some(DropTarget::Pane(PaneTarget {
            pane_id,
            zone,
            layout,
        }))
    }

    /// While floating pane `float` is moved by its title: the pane of its own,
    /// selected tab under the pointer that it would dock into. Floats of other
    /// tabs never dock here.
    pub(super) fn refresh_float_dock(&mut self, float: &PaneId, position: Point<Pixels>) {
        let hit = self.zone_under(position, float, true);
        let current = self.float_drag.as_ref().and_then(|drag| drag.dock.as_ref());
        if let (Some(hit), Some(current)) = (&hit, current)
            && current.pane_id == hit.0
            && current.zone == hit.1
        {
            return;
        }
        let dock = hit.and_then(|(pane_id, zone)| {
            let tab = self.selected_tab()?;
            let layout = dock_layout(&tab.layout, float, &pane_id, zone)?;
            Some(PaneTarget {
                pane_id,
                zone,
                layout,
            })
        });
        if let Some(drag) = &mut self.float_drag {
            drag.dock = dock;
        }
    }

    /// Release over a dock target: the float leaves its frame and takes its place
    /// in the tab in one step, gliding there. The tree is rebuilt from the tab as
    /// it is now; when the commit cannot be sent the float stays where it was let go.
    pub(super) fn dock_float(&mut self, float: PaneId, target: PaneTarget) {
        let Some(tab) = self.selected_tab() else {
            return;
        };
        let Some(layout) = dock_layout(&tab.layout, &float, &target.pane_id, target.zone) else {
            self.save_state();
            return;
        };
        if layout != tab.layout {
            let tab_id = tab.id.clone();
            let arranged = self.mutate_in(
                WorkspaceMutation::ArrangeTab {
                    tab_id,
                    layout: layout.clone(),
                },
                false,
                None,
            );
            if arranged.is_none() {
                self.save_state();
                return;
            }
            self.preview_layout = Some(layout);
        }
        self.dock_pane(&float);
    }

    /// Release: a click opens the pane menu under the grip; a drag commits its target.
    pub(in crate::gui) fn finish_pane_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.pane_drag.take() else {
            return;
        };
        if !drag.moving {
            self.open_grip_menu(&drag.pane_id, window, cx);
            return;
        }
        let Some(target) = drag.target else {
            return;
        };
        let selected = self.selected_tab().map(|tab| tab.id.clone());
        match target {
            DropTarget::Pane(target) if selected.as_ref() == Some(&drag.tab_id) => {
                if self
                    .tab_by_id(&drag.tab_id)
                    .is_none_or(|tab| tab.layout != drag.tree)
                {
                    self.global_error =
                        Some("The tab changed while dragging; the move was cancelled".into());
                    return;
                }
                let arranged = self.mutate_in(
                    WorkspaceMutation::ArrangeTab {
                        tab_id: drag.tab_id,
                        layout: target.layout.clone(),
                    },
                    false,
                    None,
                );
                // Shown at once and landed while the daemon confirms it.
                if arranged.is_some() {
                    self.arm_landing();
                    self.preview_layout = Some(target.layout);
                    self.rebuild_layout(window, true);
                }
            }
            DropTarget::Pane(target) => {
                if let Some(tab_id) = selected {
                    self.move_pane_to_tab(
                        drag.pane_id,
                        tab_id,
                        Placement::Beside(target.pane_id, target.zone),
                    );
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
        // Beside a pane of the shown tab, its panes glide aside when the merge lands.
        if matches!(placement, Placement::Beside(..)) {
            self.arm_landing();
        }
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
            DropTarget::Pane(_) => None,
        }
    }

    /// A pane is being moved (past the drag threshold).
    pub(super) fn pane_drag_moving(&self) -> bool {
        self.pane_drag.as_ref().is_some_and(|drag| drag.moving)
    }

    /// While a pane is being moved, its spot is dimmed under a veil.
    pub(super) fn decorate_dragged_pane(
        &self,
        pane: gpui::Stateful<gpui::Div>,
        pane_id: &PaneId,
    ) -> gpui::Stateful<gpui::Div> {
        if !self
            .pane_drag
            .as_ref()
            .is_some_and(|drag| drag.moving && &drag.pane_id == pane_id)
        {
            return pane;
        }
        pane.child(div().absolute().inset_0().bg(material_color(
            self.terminal_theme.terminal().background,
            0.6,
        )))
    }

    /// Over the split canvas while a pane or float is dragged onto a pane: only the
    /// zone under the pointer, the half an edge drop gives it or the Swap box.
    pub(super) fn render_drop_preview(
        &self,
        layout: &layout::WorkspaceLayout,
    ) -> Option<AnyElement> {
        let (target, swap) = match self.pane_drag.as_ref() {
            Some(drag) => {
                let Some(DropTarget::Pane(target)) = drag.target.as_ref().filter(|_| drag.moving)
                else {
                    return None;
                };
                let across = self.selected_tab().is_some_and(|tab| tab.id != drag.tab_id);
                (target, !across)
            }
            None => {
                let drag = self.float_drag.as_ref()?;
                (drag.dock.as_ref()?, true)
            }
        };
        let rect = layout.pane(&target.pane_id)?.rect;
        let colors = *self.colors();
        let accent = color(colors.accent);
        let radius = if self.comfy() { PANE_RADIUS } else { 0.0 };
        let place = |area: layout::Rect| {
            div()
                .absolute()
                .left(px(area.x))
                .top(px(area.y))
                .w(px(area.width))
                .h(px(area.height))
        };
        let swapping = target.zone == DropZone::Swap;
        let zone = landing_half(target.zone, rect.width, rect.height)
            .unwrap_or_else(|| swap_box(rect.width, rect.height, swap));
        let zone = layout::Rect {
            x: rect.x + zone.x,
            y: rect.y + zone.y,
            ..zone
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    place(zone)
                        .when(swapping, |zone| zone.rounded_md())
                        .when(!swapping, |zone| zone.rounded(px(radius)))
                        .border_2()
                        .border_color(accent)
                        .bg(accent.opacity(ZONE_FILL_ALPHA))
                        .when(swapping, |zone| {
                            zone.flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(UI_SMALL_TEXT_SIZE))
                                .text_color(color(colors.foreground))
                                .child("Swap")
                        }),
                )
                .into_any_element(),
        )
    }

    /// The card that follows the pointer while a pane is moved. It lifts off at
    /// pickup, fading in as it rises; with reduced motion it is simply there.
    pub(super) fn render_pane_drag_card(&self) -> Option<AnyElement> {
        let drag = self.pane_drag.as_ref().filter(|drag| drag.moving)?;
        let colors = *self.colors();
        let (title, _) = self.floating_caption(&drag.pane_id);
        // Below and right of the pointer, clear of the zone it is over.
        let top = drag.position.y + px(22.0);
        let card = div()
            .absolute()
            .left(drag.position.x + px(18.0))
            .top(top)
            .max_w(px(280.0))
            .px_3()
            .py_2()
            .rounded_md()
            .shadow_xl()
            .bg(color(colors.surface))
            .border_1()
            .border_color(color(colors.accent))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(colors.foreground))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(title);
        if *focus::REDUCED_MOTION {
            return Some(card.into_any_element());
        }
        Some(
            card.with_animation(
                ("pane-drag-card", drag.pickup as usize),
                Animation::new(PICKUP_DURATION).with_easing(ease_out_quint()),
                move |card, progress| {
                    card.opacity(0.4 + 0.6 * progress)
                        .top(top + px(PICKUP_RISE * (1.0 - progress)))
                },
            )
            .into_any_element(),
        )
    }
}

/// `target` already stands for the pane and zone in `hit`.
fn same_target(target: Option<&DropTarget>, (pane_id, zone): &(PaneId, DropZone)) -> bool {
    matches!(target, Some(DropTarget::Pane(target)) if &target.pane_id == pane_id && target.zone == *zone)
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

    fn pane(id: &str) -> LayoutNode {
        LayoutNode::terminal(PaneId::new(id), SurfaceId::new(format!("{id}-surface")))
    }

    fn tiled(id: &str, x: f32, width: f32) -> layout::PaneLayout {
        let rect = layout::Rect {
            x,
            y: 0.0,
            width,
            height: 400.0,
        };
        layout::PaneLayout {
            pane_id: PaneId::new(id),
            surface_id: None,
            rect,
            canvas: rect,
            parent_divider: None,
        }
    }

    #[test]
    fn the_zone_under_the_pointer_skips_the_moved_pane_and_gutters() {
        // Two 500 px panes with a 6 px gutter between them.
        let panes = [tiled("a", 0.0, 500.0), tiled("b", 506.0, 500.0)];
        let at = |x, skip: &str| {
            pane_zone_at(&panes, x, 200.0, &PaneId::new(skip), true)
                .map(|(pane, zone)| (pane.pane_id.as_str().to_owned(), zone))
        };
        assert_eq!(at(756.0, "a"), Some(("b".into(), DropZone::Swap)));
        assert_eq!(at(520.0, "a"), Some(("b".into(), DropZone::Left)));
        assert_eq!(at(990.0, "a"), Some(("b".into(), DropZone::Right)));
        // Over the moved pane itself, a gutter, or nothing: no target.
        assert_eq!(at(250.0, "a"), None);
        assert_eq!(at(503.0, "a"), None);
        assert_eq!(at(2000.0, "a"), None);
        assert_eq!(at(250.0, "b"), Some(("a".into(), DropZone::Swap)));
    }

    #[test]
    fn a_float_docks_beside_or_in_place_of_a_pane_of_its_own_tab() {
        let (a, b, f) = (PaneId::new("a"), PaneId::new("b"), PaneId::new("f"));
        let split = |axis, first, second| LayoutNode::Split {
            axis,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        // `f` floats out of the tab's right half; `a` and `b` share the rest.
        let tree = split(
            SplitAxis::Horizontal,
            split(SplitAxis::Vertical, pane("a"), pane("b")),
            pane("f"),
        );
        assert_eq!(
            dock_layout(&tree, &f, &a, DropZone::Left),
            Some(split(
                SplitAxis::Vertical,
                split(SplitAxis::Horizontal, pane("f"), pane("a")),
                pane("b"),
            ))
        );
        assert_eq!(
            dock_layout(&tree, &f, &b, DropZone::Swap),
            Some(split(
                SplitAxis::Horizontal,
                split(SplitAxis::Vertical, pane("a"), pane("f")),
                pane("b"),
            ))
        );
        // A drop that changes nothing still docks, into the float's own tile.
        let pair = split(SplitAxis::Horizontal, pane("a"), pane("f"));
        assert_eq!(
            dock_layout(&pair, &f, &a, DropZone::Right),
            Some(pair.clone())
        );
        // A float from another tab never docks here.
        let other = split(SplitAxis::Horizontal, pane("a"), pane("b"));
        assert_eq!(dock_layout(&other, &f, &a, DropZone::Left), None);
    }
}
