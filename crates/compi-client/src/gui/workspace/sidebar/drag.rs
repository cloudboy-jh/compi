//! Dragging sidebar cards. A picked-up card follows the pointer, and over the sidebar a
//! line shows where it lands:
//!
//! - Among a project's cards it moves there: among the pinned cards it is pinned in
//!   that place, among the others it is unpinned and moves in its workspace's tab
//!   order (a card of another workspace marks no place in it).
//! - Over another project's heading or cards it joins that project for good
//!   (`SetTabProject`), wherever its shell goes; Home included.
//! - Dropped on the panes, its tab merges into the shown tab, on the right.
//!
//! Escape cancels.
use super::*;
use compi_protocol::{SessionId, SplitAxis, WorkspaceSnapshot};
use gpui::DragMoveEvent;

/// How thick the line marking where a card lands is.
const DROP_LINE: f32 = 2.0;

/// A sidebar card being dragged, drawn under the pointer as a small copy of the card.
#[derive(Clone)]
pub(in crate::gui) struct CardDrag {
    tab: TabId,
    session: SessionId,
    /// The section it was picked up from.
    project: Project,
    pinned: bool,
    title: SharedString,
    face: CardFace,
}

#[derive(Clone, Copy)]
struct CardFace {
    background: u32,
    foreground: u32,
    border: u32,
}

impl Render for CardDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(220.0))
            .px_2p5()
            .py_1p5()
            .rounded(px(8.0))
            .bg(color(self.face.background))
            .border_1()
            .border_color(color(self.face.border))
            .shadow_md()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(self.face.foreground))
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .child(self.title.clone())
    }
}

/// Where a dragged card would land: in `project`'s section, before card `before`, else
/// at the section's end.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CardDrop {
    pub(super) project: Project,
    pub(super) before: Option<TabId>,
    /// The section is not the one the card came from.
    pub(super) joins: bool,
}

/// Where a moved tab goes in its workspace: before one tab, or after one.
#[derive(Clone, Debug, PartialEq)]
enum Anchor {
    Before(TabId),
    After(TabId),
}

/// The index `MoveTab` puts `tab` at so it lands at `anchor` in `order`, the
/// workspace's tabs; `None` when it is there already or the anchor is gone.
fn landing_index(order: &[TabId], tab: &TabId, anchor: &Anchor) -> Option<usize> {
    let current = order.iter().position(|id| id == tab)?;
    let rest: Vec<&TabId> = order.iter().filter(|id| *id != tab).collect();
    let index = match anchor {
        Anchor::Before(id) => rest.iter().position(|known| *known == id)?,
        Anchor::After(id) => rest.iter().position(|known| *known == id)? + 1,
    };
    (index != current).then_some(index)
}

/// `pinned` with `tab` pinned just before `before`.
fn pinned_before(pinned: &[TabId], tab: &TabId, before: &TabId) -> Vec<TabId> {
    let mut order: Vec<TabId> = pinned.iter().filter(|id| *id != tab).cloned().collect();
    let index = order
        .iter()
        .position(|id| id == before)
        .unwrap_or(order.len());
    order.insert(index, tab.clone());
    order
}

impl CompiApp {
    /// What a card carries when it is dragged.
    pub(super) fn card_drag(&self, card: &SidebarCard) -> CardDrag {
        let colors = *self.colors();
        CardDrag {
            tab: card.tab.clone(),
            session: card.session.clone(),
            project: card.project.clone(),
            pinned: card.pinned,
            title: card.title.clone().into(),
            face: CardFace {
                background: blend_rgb(colors.surface, colors.foreground, 0.09),
                foreground: colors.foreground,
                border: colors.accent,
            },
        }
    }

    /// The pointer moved over a card or heading of `project`'s section during a drag.
    pub(super) fn hover_card_drop(
        &mut self,
        event: &DragMoveEvent<CardDrag>,
        project: Project,
        before: Option<TabId>,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let joins = event.drag(cx).project != project;
        let drop = Some(CardDrop {
            project,
            before,
            joins,
        });
        if self.sidebar.drop != drop {
            self.sidebar.drop = drop;
            cx.notify();
        }
    }

    /// The pointer left the sidebar during a drag: no line shows.
    pub(super) fn leave_card_drop(
        &mut self,
        event: &DragMoveEvent<CardDrag>,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position) && self.sidebar.drop.take().is_some() {
            cx.notify();
        }
    }

    /// The line under the pointer, while a card is dragged.
    pub(super) fn card_drop(&self, cx: &App) -> Option<&CardDrop> {
        self.sidebar.drop.as_ref().filter(|_| cx.has_active_drag())
    }

    /// Escape during a card drag: nothing moves.
    pub(in crate::gui) fn cancel_card_drag(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.sidebar.drop = None;
        cx.stop_active_drag(window)
    }

    /// An accent line `DROP_LINE` thick across the top, or with `bottom` the bottom, of
    /// the card it is put in.
    pub(super) fn card_drop_line(&self, bottom: bool) -> Div {
        let line = div()
            .absolute()
            .left_1()
            .right_1()
            .h(px(DROP_LINE))
            .rounded_full()
            .bg(color(self.colors().accent));
        if bottom {
            line.bottom(px(-DROP_LINE))
        } else {
            line.top(px(-DROP_LINE))
        }
    }

    /// A card was dropped on the sidebar: it lands where the line showed.
    pub(super) fn drop_card(&mut self, drag: &CardDrag, cx: &mut Context<Self>) {
        let Some(target) = self.sidebar.drop.take() else {
            return;
        };
        let sections = self.sidebar_sections();
        let Some(section) = sections
            .iter()
            .find(|section| section.project == target.project)
        else {
            return;
        };
        let mut steps: Vec<Step> = Vec::new();
        if target.project != drag.project {
            let (tab_id, project) = (drag.tab.clone(), target.project.clone());
            steps.push(Box::new(move |_| {
                Some(WorkspaceMutation::SetTabProject {
                    tab_id,
                    project: Some(project),
                })
            }));
        }
        let before = target
            .before
            .as_ref()
            .and_then(|id| section.cards.iter().find(|card| &card.tab == id));
        if before.is_some_and(|card| card.pinned) {
            let before = before.map(|card| card.tab.clone()).expect("a pinned card");
            if before != drag.tab {
                self.state.pinned_tabs = pinned_before(&self.state.pinned_tabs, &drag.tab, &before);
                self.save_state();
            }
        } else {
            if drag.pinned {
                self.state.set_pinned(&drag.tab, false);
                self.save_state();
            }
            let anchor = match before {
                Some(card) if card.tab == drag.tab => None,
                Some(card) => {
                    (card.session == drag.session).then(|| Anchor::Before(card.tab.clone()))
                }
                None => section
                    .cards
                    .iter()
                    .rfind(|card| card.session == drag.session && card.tab != drag.tab)
                    .map(|card| Anchor::After(card.tab.clone())),
            };
            if let Some(anchor) = anchor {
                let (tab_id, session_id) = (drag.tab.clone(), drag.session.clone());
                steps.push(Box::new(move |workspace| {
                    let session = workspace
                        .sessions
                        .iter()
                        .find(|session| session.id == session_id)?;
                    let order: Vec<TabId> = session.tabs.iter().map(|tab| tab.id.clone()).collect();
                    let index = landing_index(&order, &tab_id, &anchor)?;
                    Some(WorkspaceMutation::MoveTab {
                        session_id,
                        tab_id,
                        index,
                        group: TabPlacement::Infer,
                    })
                }));
            }
        }
        self.submit_steps(steps);
        cx.notify();
    }

    /// A card was dropped on the panes: its tab joins the shown tab, on the right,
    /// moving into the shown tab's workspace first.
    pub(in crate::gui) fn drop_card_on_panes(&mut self, drag: &CardDrag, cx: &mut Context<Self>) {
        self.sidebar.drop = None;
        let Some(shown) = self.selected_tab().map(|tab| tab.id.clone()) else {
            return;
        };
        if shown == drag.tab {
            return;
        }
        let tab_id = drag.tab.clone();
        let (moved, receiving) = (tab_id.clone(), shown.clone());
        let session_of = |workspace: &WorkspaceSnapshot, tab: &TabId| {
            workspace
                .sessions
                .iter()
                .find(|session| session.tabs.iter().any(|known| &known.id == tab))
                .map(|session| (session.id.clone(), session.tabs.len()))
        };
        let steps: Vec<Step> = vec![
            Box::new(move |workspace| {
                let (target, count) = session_of(workspace, &receiving)?;
                let (source, _) = session_of(workspace, &moved)?;
                (target != source).then_some(WorkspaceMutation::MoveTab {
                    session_id: target,
                    index: count,
                    tab_id: moved,
                    group: TabPlacement::Leave,
                })
            }),
            Box::new(move |workspace| {
                let tabs: Vec<&WorkspaceTab> = workspace
                    .sessions
                    .iter()
                    .flat_map(|session| &session.tabs)
                    .collect();
                let target = tabs.iter().find(|tab| tab.id == shown)?;
                let source = tabs.iter().find(|tab| tab.id == tab_id)?;
                Some(WorkspaceMutation::MergeTabs {
                    tab_id: shown.clone(),
                    sources: vec![tab_id.clone()],
                    layout: LayoutNode::Split {
                        axis: SplitAxis::Horizontal,
                        ratio: 0.5,
                        first: Box::new(target.layout.clone()),
                        second: Box::new(source.layout.clone()),
                    },
                })
            }),
        ];
        self.submit_steps(steps);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(ids: &[&str]) -> Vec<TabId> {
        ids.iter().map(|id| TabId::new(*id)).collect()
    }

    #[test]
    fn a_moved_tab_lands_before_or_after_its_anchor() {
        let order = tabs(&["a", "b", "c", "d"]);
        let tab = |id: &str| TabId::new(id);
        let before = |id: &str| Anchor::Before(tab(id));
        let after = |id: &str| Anchor::After(tab(id));
        // Down: before d puts a third (index 2 once a is out).
        assert_eq!(landing_index(&order, &tab("a"), &before("d")), Some(2));
        assert_eq!(landing_index(&order, &tab("a"), &after("d")), Some(3));
        // Up: before a puts d first.
        assert_eq!(landing_index(&order, &tab("d"), &before("a")), Some(0));
        // Where it is already: nothing to send.
        assert_eq!(landing_index(&order, &tab("b"), &before("c")), None);
        assert_eq!(landing_index(&order, &tab("b"), &after("a")), None);
        // An anchor that closed meanwhile: nothing to send.
        assert_eq!(landing_index(&order, &tab("a"), &before("z")), None);
    }

    #[test]
    fn a_card_pinned_in_place_lands_before_its_neighbour() {
        let pinned = tabs(&["a", "b", "c"]);
        assert_eq!(
            pinned_before(&pinned, &TabId::new("c"), &TabId::new("a")),
            tabs(&["c", "a", "b"])
        );
        assert_eq!(
            pinned_before(&pinned, &TabId::new("x"), &TabId::new("b")),
            tabs(&["a", "x", "b", "c"])
        );
    }
}
