//! Dragging sidebar cards and project headings. A picked-up card or heading follows the
//! pointer, and over the sidebar a line shows where it lands:
//!
//! - Among a project's cards it moves there: among the pinned cards it is pinned in
//!   that place, among the others it is unpinned and moves in its workspace's tab
//!   order (a card of another workspace marks no place in it).
//! - Over another project's heading or cards it joins that project for good
//!   (`SetTabProject`), wherever its shell goes; Home included.
//! - Dropped on the panes, its tab merges into the shown tab, on the right.
//! - A project heading dropped over another heading moves the project before or after
//!   it, within its own group: pinned projects stay first, the others after them. The
//!   order is saved (`[projects."<key>"] order`).
//!
//! Escape cancels.
use super::*;
use crate::gui::workspace::project_settings::ListedProject;
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

impl CardFace {
    /// What a dragged card or heading looks like under the pointer: `title` on the card's
    /// colours.
    fn render(self, title: SharedString) -> Div {
        div()
            .w(px(220.0))
            .px_2p5()
            .py_1p5()
            .rounded(px(8.0))
            .bg(color(self.background))
            .border_1()
            .border_color(color(self.border))
            .shadow_md()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(self.foreground))
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .child(title)
    }
}

impl Render for CardDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.face.render(self.title.clone())
    }
}

/// A project heading being dragged to move the project, drawn as a dragged card is.
#[derive(Clone)]
pub(super) struct ProjectDrag {
    project: Project,
    title: SharedString,
    face: CardFace,
}

impl Render for ProjectDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.face.render(self.title.clone())
    }
}

/// Where a dragged heading's project would land, as the line shows it.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ProjectLine {
    /// Over this project's heading.
    Before(Project),
    /// Under the list's last row.
    Last,
}

/// The heading a dragged heading is over, and where its project would land.
pub(super) struct ProjectDrop {
    moved: Project,
    over: Project,
    /// The pointer is over the heading's lower half: the project lands after it.
    after: bool,
    /// Nothing when the project would not move.
    line: Option<ProjectLine>,
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

/// `listed` with `moved` put before `over`, or with `after` after it, but kept in its
/// own group (`ListedProject::group`): dropped past the group's end it lands at that
/// end. `None` when it would not move or either is not listed.
fn moved_order<'a>(
    listed: &'a [ListedProject],
    moved: &Project,
    over: &Project,
    after: bool,
) -> Option<Vec<&'a ListedProject>> {
    let from = listed.iter().position(|listed| listed.project == *moved)?;
    let group = listed[from].group();
    let mut order: Vec<&ListedProject> = listed
        .iter()
        .filter(|listed| listed.project != *moved)
        .collect();
    let target = order.iter().position(|listed| listed.project == *over)? + usize::from(after);
    // The list is sorted by group, so the group's places run from its first to its last.
    let start = order
        .iter()
        .position(|listed| listed.group() >= group)
        .unwrap_or(order.len());
    let end = order
        .iter()
        .position(|listed| listed.group() > group)
        .unwrap_or(order.len());
    let index = target.clamp(start, end);
    (index != from).then(|| {
        order.insert(index, &listed[from]);
        order
    })
}

/// Where the line shows for `moved` in `order`: over the next shown (unarchived)
/// project's heading, else under the last row.
fn project_line(order: &[&ListedProject], moved: &Project) -> Option<ProjectLine> {
    let at = order.iter().position(|listed| listed.project == *moved)?;
    Some(match order.get(at + 1).filter(|listed| !listed.archived) {
        Some(next) => ProjectLine::Before(next.project.clone()),
        None => ProjectLine::Last,
    })
}

impl CompiApp {
    /// How a dragged card or heading is drawn under the pointer.
    fn drag_face(&self) -> CardFace {
        let colors = *self.colors();
        CardFace {
            background: blend_rgb(colors.surface, colors.foreground, 0.09),
            foreground: colors.foreground,
            border: colors.accent,
        }
    }

    /// What a card carries when it is dragged.
    pub(super) fn card_drag(&self, card: &SidebarCard) -> CardDrag {
        CardDrag {
            tab: card.tab.clone(),
            session: card.session.clone(),
            project: card.project.clone(),
            pinned: card.pinned,
            title: card.title.clone().into(),
            face: self.drag_face(),
        }
    }

    /// What a project heading carries when it is dragged.
    pub(super) fn project_drag(&self, project: &Project) -> ProjectDrag {
        ProjectDrag {
            project: project.clone(),
            title: self.project_name(project).into(),
            face: self.drag_face(),
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

    /// The pointer moved over `project`'s heading during a heading drag: over its upper
    /// half the dragged project would land before it, over its lower half after it.
    pub(super) fn hover_project_drop(
        &mut self,
        event: &DragMoveEvent<ProjectDrag>,
        project: &Project,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let after = event.event.position.y > event.bounds.center().y;
        let moved = &event.drag(cx).project;
        if self.sidebar.project_drop.as_ref().is_some_and(|drop| {
            drop.moved == *moved && drop.over == *project && drop.after == after
        }) {
            return;
        }
        let listed = self.listed_projects();
        let line = moved_order(&listed, moved, project, after)
            .and_then(|order| project_line(&order, moved));
        self.sidebar.project_drop = Some(ProjectDrop {
            moved: moved.clone(),
            over: project.clone(),
            after,
            line,
        });
        cx.notify();
    }

    /// The pointer left the sidebar during a heading drag: no line shows.
    pub(super) fn leave_project_drop(
        &mut self,
        event: &DragMoveEvent<ProjectDrag>,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position)
            && self.sidebar.project_drop.take().is_some()
        {
            cx.notify();
        }
    }

    /// Where the line shows, while a heading is dragged.
    pub(super) fn project_drop_line(&self, cx: &App) -> Option<&ProjectLine> {
        self.sidebar
            .project_drop
            .as_ref()
            .filter(|_| cx.has_active_drag())
            .and_then(|drop| drop.line.as_ref())
    }

    /// A heading was dropped on the sidebar: its project lands where the line showed,
    /// and the order is saved.
    pub(super) fn drop_project(&mut self, drag: &ProjectDrag, cx: &mut Context<Self>) {
        let Some(target) = self.sidebar.project_drop.take() else {
            return;
        };
        let listed = self.listed_projects();
        if let Some(order) = moved_order(&listed, &drag.project, &target.over, target.after) {
            let order: Vec<Project> = order
                .into_iter()
                .map(|listed| listed.project.clone())
                .collect();
            self.save_project_order(&order, cx);
        }
        cx.notify();
    }

    /// Escape during a card or heading drag: nothing moves.
    pub(in crate::gui) fn cancel_sidebar_drag(
        &mut self,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        self.sidebar.drop = None;
        self.sidebar.project_drop = None;
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

    #[test]
    fn a_dragged_project_moves_within_its_own_group() {
        let project = |root: &str| Project {
            environment: compi_protocol::ContentEnvironment::Host,
            root: root.into(),
        };
        let listed: Vec<ListedProject> = [
            ("p1", true, false),
            ("p2", true, false),
            ("u1", false, false),
            ("u2", false, false),
            ("u3", false, false),
            ("x", false, true),
        ]
        .into_iter()
        .map(|(root, pinned, archived)| ListedProject {
            project: project(root),
            pinned,
            archived,
        })
        .collect();
        let moved = |root: &str, over: &str, after: bool| {
            moved_order(&listed, &project(root), &project(over), after).map(|order| {
                let line = project_line(&order, &project(root));
                let roots: Vec<&str> = order
                    .iter()
                    .map(|listed| listed.project.root.as_str())
                    .collect();
                (roots, line)
            })
        };
        let before = |root: &str| Some(ProjectLine::Before(project(root)));
        assert_eq!(
            moved("u3", "u1", false),
            Some((vec!["p1", "p2", "u3", "u1", "u2", "x"], before("u1")))
        );
        assert_eq!(
            moved("u1", "u3", true),
            Some((
                vec!["p1", "p2", "u2", "u3", "u1", "x"],
                Some(ProjectLine::Last)
            ))
        );
        // Over the pinned group, an unpinned project stops at its own group's start.
        assert_eq!(
            moved("u3", "p1", false),
            Some((vec!["p1", "p2", "u3", "u1", "u2", "x"], before("u1")))
        );
        assert_eq!(moved("u1", "p2", true), None);
        // Past the pinned group's end, a pinned project stops at that end.
        assert_eq!(
            moved("p1", "u2", true),
            Some((vec!["p2", "p1", "u1", "u2", "u3", "x"], before("u1")))
        );
        // Archived projects stay last.
        assert_eq!(
            moved("u2", "x", true),
            Some((
                vec!["p1", "p2", "u1", "u3", "u2", "x"],
                Some(ProjectLine::Last)
            ))
        );
        // On itself, or where it is already: nothing moves.
        assert_eq!(moved("u2", "u2", false), None);
        assert_eq!(moved("u2", "u3", false), None);
        assert_eq!(moved("u2", "u1", true), None);
    }
}
