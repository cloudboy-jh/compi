//! Tab groups in the tab strip. A group's label chip stands before its first visible
//! tab and its tabs are underlined in its colour; a folded group shows only its
//! label. Groups, their names, colours and folding are the daemon's, so every window
//! shows them alike; which tabs a window hides stays its own.
//!
//! Chips are as wide as their content (labels) or the header's tab width (tabs), so
//! the strip is laid out once per frame into x-ranges ([`TabStrip`]) that the drop
//! marks, drops and pane drags hit-test against.
use super::*;
use crate::gui::controls::{MenuEntry, menu_item};
use compi_protocol::{TabGroup, TabGroupColor, TabGroupId, TabPlacement, WorkspaceSession};

/// The widest a group label grows; a longer name ends in an ellipsis.
const LABEL_MAX_WIDTH: f32 = 160.0;
const LABEL_PADDING: f32 = 8.0;
const LABEL_GAP: f32 = 6.0;
/// The dot before a group's name, and the pill an unnamed group shows instead.
const LABEL_DOT: f32 = 8.0;
const LABEL_PILL: f32 = 18.0;
/// The line under a group's label and tabs.
const GROUP_LINE: f32 = 2.0;

/// One chip of the tab strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Chip {
    /// A group's label, before its first visible tab.
    Label(TabGroupId),
    Tab(TabId),
}

/// A chip and its x-range in the strip's content, before scrolling.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlacedChip {
    pub(super) chip: Chip,
    pub(super) start: f32,
    pub(super) end: f32,
}

impl PlacedChip {
    fn middle(&self) -> f32 {
        (self.start + self.end) / 2.0
    }
}

/// The selected workspace's tab strip as last laid out.
#[derive(Clone, Debug, Default)]
pub(in crate::gui) struct TabStrip {
    pub(super) chips: Vec<PlacedChip>,
}

impl TabStrip {
    /// `chips` side by side from 0, each `width` wide.
    pub(super) fn lay_out(chips: Vec<Chip>, mut width: impl FnMut(&Chip) -> f32) -> Self {
        let mut x = 0.0;
        let chips = chips
            .into_iter()
            .map(|chip| {
                let start = x;
                x += width(&chip);
                PlacedChip {
                    chip,
                    start,
                    end: x,
                }
            })
            .collect();
        Self { chips }
    }

    fn chip_at(&self, x: f32) -> Option<&PlacedChip> {
        self.chips
            .iter()
            .find(|chip| (chip.start..chip.end).contains(&x))
    }
}

/// The chips the strip shows for `visible`, the session's tabs a window does not
/// hide, in order: each group's label before its first one, then its tabs unless the
/// group is folded.
pub(super) fn strip_chips<'a>(
    session: &WorkspaceSession,
    visible: impl IntoIterator<Item = &'a WorkspaceTab>,
) -> Vec<Chip> {
    let mut chips = Vec::new();
    let mut run: Option<&TabGroupId> = None;
    for tab in visible {
        match tab.group.as_ref().and_then(|id| session.group(id)) {
            Some(group) => {
                if run != Some(&group.id) {
                    chips.push(Chip::Label(group.id.clone()));
                    run = Some(&group.id);
                }
                if !group.folded {
                    chips.push(Chip::Tab(tab.id.clone()));
                }
            }
            None => {
                run = None;
                chips.push(Chip::Tab(tab.id.clone()));
            }
        }
    }
    chips
}

/// What marks where a dragged tab would land.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum DropMark {
    /// An insertion bar at this strip x.
    Bar(f32),
    /// A folded group's label, lit: the tab joins the group.
    Label(TabGroupId),
}

/// Where a tab dropped on the strip lands: `MoveTab`'s index and placement.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TabDrop {
    pub(super) index: usize,
    pub(super) placement: TabPlacement,
    pub(super) mark: DropMark,
}

fn tab_position(session: &WorkspaceSession, id: &TabId) -> Option<usize> {
    session.tabs.iter().position(|tab| &tab.id == id)
}

/// The first and last of the session's tabs in `group`.
fn group_range(session: &WorkspaceSession, group: &TabGroupId) -> Option<(usize, usize)> {
    let first = session
        .tabs
        .iter()
        .position(|tab| tab.group.as_ref() == Some(group))?;
    let last = session
        .tabs
        .iter()
        .rposition(|tab| tab.group.as_ref() == Some(group))?;
    Some((first, last))
}

fn is_folded(session: &WorkspaceSession, group: &TabGroupId) -> bool {
    session.group(group).is_some_and(|group| group.folded)
}

fn tab_group<'a>(session: &'a WorkspaceSession, id: &TabId) -> Option<&'a TabGroupId> {
    session
        .tabs
        .iter()
        .find(|tab| &tab.id == id)
        .and_then(|tab| tab.group.as_ref())
}

/// How many of the session's tabs before `boundary` are not `moved`: an index in
/// the tabs without the moved ones.
fn index_without(
    session: &WorkspaceSession,
    boundary: usize,
    moved: impl Fn(&WorkspaceTab) -> bool,
) -> usize {
    session.tabs[..boundary.min(session.tabs.len())]
        .iter()
        .filter(|tab| !moved(tab))
        .count()
}

/// Where `dragged` dropped at strip `x` lands. On a folded group's label it joins
/// the group after its tabs. Otherwise the slot is before the first shown tab (or
/// folded label) whose middle is right of `x`; within a group's span, its label to
/// its last shown tab, the tab joins that group, and anywhere else it leaves its
/// group.
pub(super) fn tab_drop(
    session: &WorkspaceSession,
    chips: &[PlacedChip],
    dragged: &TabId,
    x: f32,
) -> TabDrop {
    let moved = |tab: &WorkspaceTab| &tab.id == dragged;
    for chip in chips {
        if let Chip::Label(group) = &chip.chip
            && is_folded(session, group)
            && (chip.start..chip.end).contains(&x)
            && let Some((_, last)) = group_range(session, group)
        {
            return TabDrop {
                index: index_without(session, last + 1, moved),
                placement: TabPlacement::Join {
                    group_id: group.clone(),
                },
                mark: DropMark::Label(group.clone()),
            };
        }
    }
    let joined = chips.iter().enumerate().find_map(|(index, chip)| {
        let Chip::Label(group) = &chip.chip else {
            return None;
        };
        if is_folded(session, group) {
            return None;
        }
        let end = chips[index + 1..]
            .iter()
            .take_while(|member| {
                matches!(&member.chip, Chip::Tab(id) if tab_group(session, id) == Some(group))
            })
            .last()
            .map_or(chip.end, |member| member.end);
        (chip.start..end).contains(&x).then(|| (group.clone(), end))
    });
    // Shown tabs and folded labels are the slots' units: (chip, first, last tab).
    let units: Vec<_> = chips
        .iter()
        .enumerate()
        .filter_map(|(index, chip)| match &chip.chip {
            Chip::Tab(id) => tab_position(session, id).map(|at| (index, at, at)),
            Chip::Label(group) if is_folded(session, group) => {
                group_range(session, group).map(|(first, last)| (index, first, last))
            }
            Chip::Label(_) => None,
        })
        .collect();
    let slot = units
        .iter()
        .find(|(index, ..)| chips[*index].middle() > x)
        .copied();
    let unit_group = |index: usize| match &chips[index].chip {
        Chip::Tab(id) => tab_group(session, id).filter(|group| !is_folded(session, group)),
        Chip::Label(_) => None,
    };
    let (boundary, bar) = match (&joined, slot) {
        // Past the group's last shown tab, still in its span: right after its tabs.
        (Some((group, end)), slot)
            if slot.is_none_or(|(index, ..)| unit_group(index) != Some(group)) =>
        {
            let last = group_range(session, group).map_or(session.tabs.len(), |(_, last)| last + 1);
            (last, *end)
        }
        (_, Some((index, first, _))) => match unit_group(index) {
            // Leaving, before a group's first shown tab: before the group and its label.
            Some(group) if joined.is_none() => {
                let first = group_range(session, group).map_or(first, |(first, _)| first);
                let bar = index
                    .checked_sub(1)
                    .filter(|label| matches!(&chips[*label].chip, Chip::Label(id) if id == group))
                    .map_or(chips[index].start, |label| chips[label].start);
                (first, bar)
            }
            _ => (first, chips[index].start),
        },
        (_, None) => match units.last() {
            Some((index, _, last)) => (last + 1, chips[*index].end),
            None => (session.tabs.len(), 0.0),
        },
    };
    TabDrop {
        index: index_without(session, boundary, moved),
        placement: match joined {
            Some((group_id, _)) => TabPlacement::Join { group_id },
            None => TabPlacement::Leave,
        },
        mark: DropMark::Bar(bar),
    }
}

/// Where group `dragged` dropped at strip `x` lands: `MoveTabGroup`'s index, among
/// the tabs outside the group, and the insertion bar's x. Groups move whole: the
/// slot is before the first ungrouped tab or group whose middle is right of `x`.
pub(super) fn group_drop(
    session: &WorkspaceSession,
    chips: &[PlacedChip],
    dragged: &TabGroupId,
    x: f32,
) -> (usize, f32) {
    // Units: (start, end, first tab, last tab).
    let mut units = Vec::new();
    let mut index = 0;
    while index < chips.len() {
        let chip = &chips[index];
        match &chip.chip {
            Chip::Label(group) => {
                let members = chips[index + 1..]
                    .iter()
                    .take_while(|member| {
                        matches!(&member.chip, Chip::Tab(id) if tab_group(session, id) == Some(group))
                    })
                    .count();
                let end = chips[index + members].end;
                if let Some((first, last)) = group_range(session, group) {
                    units.push((chip.start, end, first, last));
                }
                index += members + 1;
            }
            Chip::Tab(id) => {
                if let Some(at) = tab_position(session, id) {
                    units.push((chip.start, chip.end, at, at));
                }
                index += 1;
            }
        }
    }
    let (boundary, bar) = match units
        .iter()
        .find(|(start, end, ..)| (start + end) / 2.0 > x)
    {
        Some((start, _, first, _)) => (*first, *start),
        None => units
            .last()
            .map_or((session.tabs.len(), 0.0), |(_, end, _, last)| {
                (last + 1, *end)
            }),
    };
    (
        index_without(session, boundary, |tab| tab.group.as_ref() == Some(dragged)),
        bar,
    )
}

/// Move tab left or right one step along the strip `chips`: out of its own group
/// first where it is the group's first or last tab, then into an open group it
/// stands beside, past a neighbour tab, or past a folded group whole. `MoveTab`'s
/// index and placement.
pub(super) fn keyboard_move(
    session: &WorkspaceSession,
    chips: &[Chip],
    tab_id: &TabId,
    left: bool,
) -> Option<(usize, TabPlacement)> {
    let at = chips
        .iter()
        .position(|chip| matches!(chip, Chip::Tab(id) if id == tab_id))?;
    let own = tab_group(session, tab_id);
    let moved = |tab: &WorkspaceTab| &tab.id == tab_id;
    let neighbour = if left {
        at.checked_sub(1).map(|index| &chips[index])
    } else {
        chips.get(at + 1)
    };
    if let Some(group) = own {
        let edge = match neighbour {
            Some(Chip::Tab(other)) => tab_group(session, other) != Some(group),
            _ => true,
        };
        if edge {
            let (first, last) = group_range(session, group)?;
            let boundary = if left { first } else { last + 1 };
            return Some((index_without(session, boundary, moved), TabPlacement::Leave));
        }
    }
    match neighbour? {
        Chip::Tab(other) => match tab_group(session, other) {
            Some(group) if own != Some(group) => Some((
                index_without(session, tab_position(session, tab_id)?, moved),
                TabPlacement::Join {
                    group_id: group.clone(),
                },
            )),
            _ => {
                let at = tab_position(session, other)?;
                Some((
                    index_without(session, if left { at } else { at + 1 }, moved),
                    own.map_or(TabPlacement::Leave, |group| TabPlacement::Join {
                        group_id: group.clone(),
                    }),
                ))
            }
        },
        Chip::Label(group) => {
            let (first, last) = group_range(session, group)?;
            if is_folded(session, group) {
                let boundary = if left { first } else { last + 1 };
                Some((index_without(session, boundary, moved), TabPlacement::Leave))
            } else {
                Some((
                    index_without(session, first, moved),
                    TabPlacement::Join {
                        group_id: group.clone(),
                    },
                ))
            }
        }
    }
}

/// A group's name, or its colour for an unnamed one.
pub(super) fn group_title(group: &TabGroup) -> String {
    if group.name.is_empty() {
        format!("{} group", group.color.label())
    } else {
        group.name.clone()
    }
}

/// The text on a group's label: its name, with its tab count while folded; an
/// unnamed group shows only its colour, and its count while folded.
fn label_text(group: &TabGroup, tabs: usize) -> Option<String> {
    match (group.name.is_empty(), group.folded) {
        (true, false) => None,
        (true, true) => Some(tabs.to_string()),
        (false, false) => Some(group.name.clone()),
        (false, true) => Some(format!("{} · {tabs}", group.name)),
    }
}

/// What the tab bar holds under the pointer while a pane is dragged.
pub(super) enum TabBarHit {
    Tab(TabId),
    Label,
    /// Empty space after the chips.
    Space,
}

impl CompiApp {
    /// A group colour from the theme: grey is its muted text, the others its
    /// terminal palette's.
    pub(super) fn group_color(&self, group: TabGroupColor) -> u32 {
        let ansi = &self.theme.terminal().ansi;
        match group {
            TabGroupColor::Grey => self.colors().muted,
            TabGroupColor::Red => ansi[1],
            TabGroupColor::Green => ansi[2],
            TabGroupColor::Yellow => ansi[3],
            TabGroupColor::Blue => ansi[4],
            TabGroupColor::Magenta => ansi[5],
            TabGroupColor::Cyan => ansi[6],
        }
    }

    fn selected_session_ref(&self) -> Option<&WorkspaceSession> {
        self.state.selected_session(self.workspace.as_ref()?)
    }

    /// The group `id` and its session.
    pub(super) fn group_by_id(&self, id: &TabGroupId) -> Option<(&WorkspaceSession, &TabGroup)> {
        self.workspace
            .as_ref()?
            .sessions
            .iter()
            .find_map(|session| session.group(id).map(|group| (session, group)))
    }

    /// The group holding `tab_id`.
    pub(super) fn tab_group(&self, tab_id: &TabId) -> Option<&TabGroup> {
        let workspace = self.workspace.as_ref()?;
        workspace.sessions.iter().find_map(|session| {
            let tab = session.tabs.iter().find(|tab| &tab.id == tab_id)?;
            session.group(tab.group.as_ref()?)
        })
    }

    /// The selected tab's group.
    pub(super) fn selected_group(&self) -> Option<&TabGroup> {
        self.tab_group(&self.selected_tab()?.id)
    }

    /// The group's tabs, in order.
    fn group_tab_ids(&self, id: &TabGroupId) -> Vec<TabId> {
        self.group_by_id(id)
            .map(|(session, _)| {
                session
                    .tabs
                    .iter()
                    .filter(|tab| tab.group.as_ref() == Some(id))
                    .map(|tab| tab.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The strip's chips for the selected workspace, before they are laid out.
    pub(super) fn tab_strip_chips(&self) -> Vec<Chip> {
        self.selected_session_ref()
            .map(|session| strip_chips(session, self.state.visible_tabs(session)))
            .unwrap_or_default()
    }

    /// Lays the strip out for this frame: labels as wide as their text, tabs as
    /// wide as the header gives them.
    pub(super) fn refresh_tab_strip(&mut self, window: &Window) {
        let (window_width, _) = logical_viewport_dimensions(window);
        let tab_width = header_metrics(window_width, titlebar_leading_width(window)).tab_width;
        let chips = self.tab_strip_chips();
        let mut font = gpui::font(self.ui_font.family());
        font.weight = FontWeight::MEDIUM;
        let session = self.selected_session_ref();
        let strip = TabStrip::lay_out(chips, |chip| match chip {
            Chip::Tab(_) => tab_width,
            Chip::Label(id) => {
                let Some(session) = session else {
                    return 0.0;
                };
                let Some(group) = session.group(id) else {
                    return 0.0;
                };
                let tabs = session
                    .tabs
                    .iter()
                    .filter(|tab| tab.group.as_ref() == Some(id))
                    .count();
                let marker = if group.name.is_empty() {
                    LABEL_PILL
                } else {
                    LABEL_DOT
                };
                let text = label_text(group, tabs).map_or(0.0, |text| {
                    let run = TextRun {
                        len: text.len(),
                        font: font.clone(),
                        color: Hsla::default(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let line = window.text_system().shape_line(
                        text.into(),
                        px(UI_SMALL_TEXT_SIZE),
                        &[run],
                        None,
                    );
                    LABEL_GAP + f32::from(line.width)
                });
                (LABEL_PADDING * 2.0 + marker + text)
                    .ceil()
                    .min(LABEL_MAX_WIDTH)
            }
        });
        self.tab_strip = strip;
    }

    /// The strip's content x under window point `position`, when it is over the
    /// tab region.
    pub(super) fn strip_x(&self, position: Point<Pixels>, window: &Window) -> Option<f32> {
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        let (viewport_width, _) = logical_viewport_dimensions(window);
        let leading_width = titlebar_leading_width(window);
        let header = header_metrics(viewport_width, leading_width);
        let tab_region_right = viewport_width
            - WINDOW_CONTROLS_WIDTH
            - HEADER_BUTTON_SLOT_WIDTH
            - header.pane_actions_width;
        ((0.0..=CHROME_HEIGHT).contains(&y) && x >= leading_width && x < tab_region_right)
            .then(|| x - leading_width - f32::from(self.tab_scroll_handle.offset().x))
    }

    /// What a dragged pane is over in the tab bar; `None` outside it.
    pub(super) fn tab_bar_hit(
        &self,
        position: Point<Pixels>,
        window: &Window,
    ) -> Option<TabBarHit> {
        let x = self.strip_x(position, window)?;
        Some(match self.tab_strip.chip_at(x).map(|chip| &chip.chip) {
            Some(Chip::Tab(id)) => TabBarHit::Tab(id.clone()),
            Some(Chip::Label(_)) => TabBarHit::Label,
            None => TabBarHit::Space,
        })
    }

    /// The strip chip index to scroll to for `tab_id`: its own, or its folded
    /// group's label.
    pub(super) fn strip_index(&self, tab_id: &TabId) -> Option<usize> {
        let chips = self.tab_strip_chips();
        chips
            .iter()
            .position(|chip| chip == &Chip::Tab(tab_id.clone()))
            .or_else(|| {
                let group = self.tab_group(tab_id)?;
                chips
                    .iter()
                    .position(|chip| chip == &Chip::Label(group.id.clone()))
            })
    }

    /// Where the dragged tab or group would land, in the strip's content x.
    pub(super) fn strip_drop_mark(&self, window: &Window) -> Option<DropMark> {
        let x = self.strip_x(self.drag_position?, window)?;
        let session = self.selected_session_ref()?;
        if let Some(tab_id) = &self.dragging_tab {
            return Some(tab_drop(session, &self.tab_strip.chips, tab_id, x).mark);
        }
        let group = self.dragging_group.as_ref()?;
        Some(DropMark::Bar(
            group_drop(session, &self.tab_strip.chips, group, x).1,
        ))
    }

    /// A tab or group label let go after dragging: dropped on the strip it moves
    /// there; dropped outside the window it moves to the window under the pointer,
    /// or a new one. A label let go without dragging folds or unfolds its group.
    pub(super) fn finish_strip_drag(
        &mut self,
        position: Point<Pixels>,
        moved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.dragging_tab.take();
        let group = self.dragging_group.take();
        if !moved {
            if let Some(group) = group {
                self.toggle_group_fold(&group);
            }
            return;
        }
        let (viewport_width, viewport_height) = logical_viewport_dimensions(window);
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        let inside = x >= 0.0 && y >= 0.0 && x < viewport_width && y < viewport_height;
        if !inside {
            let tab_ids = match (tab, group) {
                (Some(tab), _) => vec![tab],
                (None, Some(group)) => self.group_tab_ids(&group),
                (None, None) => return,
            };
            let destination = self.window_under(position, window, cx);
            self.transfer_tabs(tab_ids, destination, window, cx);
            return;
        }
        let Some(strip_x) = self.strip_x(position, window) else {
            return;
        };
        let Some(session) = self.selected_session_ref() else {
            return;
        };
        let session_id = session.id.clone();
        let operation = if let Some(tab_id) = tab {
            let drop = tab_drop(session, &self.tab_strip.chips, &tab_id, strip_x);
            WorkspaceMutation::MoveTab {
                session_id,
                tab_id,
                index: drop.index,
                group: drop.placement,
            }
        } else if let Some(group_id) = group {
            let (index, _) = group_drop(session, &self.tab_strip.chips, &group_id, strip_x);
            WorkspaceMutation::MoveTabGroup { group_id, index }
        } else {
            return;
        };
        self.mutate(operation, false);
    }

    /// Another Compi window of this server under window point `position`.
    fn window_under(
        &self,
        position: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyWindowHandle> {
        let global = window.bounds().origin + position;
        let source = Window::window_handle(window);
        cx.windows().into_iter().find(|handle| {
            *handle != source
                && handle.downcast::<CompiApp>().is_some_and(|typed| {
                    typed
                        .update(cx, |other, target, _| {
                            other.target == self.target && target.bounds().contains(&global)
                        })
                        .unwrap_or(false)
                })
        })
    }

    /// Selecting a tab in a folded group unfolds the group.
    pub(super) fn unfold_for(&mut self, tab_id: &TabId) {
        if let Some(group) = self.tab_group(tab_id).filter(|group| group.folded) {
            let group_id = group.id.clone();
            self.mutate(
                WorkspaceMutation::UpdateTabGroup {
                    group_id,
                    name: None,
                    color: None,
                    folded: Some(false),
                },
                false,
            );
        }
    }

    pub(super) fn toggle_group_fold(&mut self, id: &TabGroupId) {
        let Some((_, group)) = self.group_by_id(id) else {
            return;
        };
        let folded = !group.folded;
        self.mutate(
            WorkspaceMutation::UpdateTabGroup {
                group_id: id.clone(),
                name: None,
                color: None,
                folded: Some(folded),
            },
            false,
        );
    }

    fn set_group_color(&mut self, id: &TabGroupId, color: TabGroupColor) {
        self.mutate(
            WorkspaceMutation::UpdateTabGroup {
                group_id: id.clone(),
                name: None,
                color: Some(color),
                folded: None,
            },
            false,
        );
    }

    pub(super) fn rename_group(&mut self, id: &TabGroupId) {
        let Some((_, group)) = self.group_by_id(id) else {
            return;
        };
        let name = group.name.clone();
        self.open_overlay(
            Overlay::Text {
                purpose: TextPurpose::RenameTabGroup(id.clone()),
                title: "Rename tab group",
            },
            &name,
        );
    }

    pub(super) fn ungroup(&mut self, id: &TabGroupId) {
        self.mutate(
            WorkspaceMutation::Ungroup {
                group_id: id.clone(),
            },
            false,
        );
    }

    /// Close group…: every tab of the group, asking as closing a tab does.
    pub(super) fn close_group(
        &mut self,
        id: &TabGroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.group_tab_ids(id).len();
        let title = if tabs == 1 {
            "Close 1 tab and end every process in its panes?".to_owned()
        } else {
            format!("Close {tabs} tabs and end every process in their panes?")
        };
        self.close_work(
            WorkspaceMutation::RemoveTabGroup {
                group_id: id.clone(),
            },
            &title,
            "Close group",
            window,
            cx,
        );
    }

    pub(super) fn move_group_to_new_window(
        &mut self,
        id: &TabGroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_ids = self.group_tab_ids(id);
        if !tab_ids.is_empty() {
            self.transfer_tabs(tab_ids, None, window, cx);
        }
    }

    /// Add tab to group…: name a new group straight away when the workspace has no
    /// other group to join, else choose.
    pub(super) fn choose_tab_group(&mut self, tab_id: TabId) {
        let joinable = self.joinable_groups(&tab_id).next().is_some();
        if joinable {
            self.open_overlay(Overlay::TabGroups { tab_id }, "");
        } else {
            self.name_new_group(tab_id);
        }
    }

    /// The groups of `tab_id`'s workspace it is not in.
    pub(super) fn joinable_groups(&self, tab_id: &TabId) -> impl Iterator<Item = &TabGroup> {
        let session = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .sessions
                .iter()
                .find(|session| session.tabs.iter().any(|tab| &tab.id == tab_id))
        });
        let own = self.tab_group(tab_id).map(|group| group.id.clone());
        session
            .into_iter()
            .flat_map(|session| &session.groups)
            .filter(move |group| Some(&group.id) != own.as_ref())
    }

    pub(super) fn name_new_group(&mut self, tab_id: TabId) {
        self.open_overlay(
            Overlay::Text {
                purpose: TextPurpose::NewTabGroup(tab_id),
                title: "New tab group",
            },
            "",
        );
    }

    /// A new group holding `tab_id`, in the colour fewest groups of its workspace use.
    pub(super) fn create_group(&mut self, tab_id: TabId, name: String) {
        let color = self
            .workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .sessions
                    .iter()
                    .find(|session| session.tabs.iter().any(|tab| tab.id == tab_id))
            })
            .map_or(TabGroupColor::Grey, |session| {
                TabGroupColor::next_free(&session.groups)
            });
        self.mutate(
            WorkspaceMutation::CreateTabGroup {
                tab_ids: vec![tab_id],
                name,
                color,
            },
            false,
        );
    }

    pub(super) fn join_group(&mut self, tab_id: TabId, group_id: Option<TabGroupId>) {
        self.mutate(WorkspaceMutation::SetTabGroup { tab_id, group_id }, false);
    }

    /// The add-to-group chooser: New group…, then the groups to join.
    pub(super) fn tab_group_choices(&self, tab_id: &TabId, query: &str) -> Vec<Choice> {
        let mut choices = vec![Choice {
            title: "New group…".into(),
            detail: "Name it, or leave it unnamed to show only its colour".into(),
            group: None,
            reason: None,
            action: ChoiceAction::NewTabGroup(tab_id.clone()),
        }];
        let session = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .sessions
                .iter()
                .find(|session| session.tabs.iter().any(|tab| &tab.id == tab_id))
        });
        for group in self.joinable_groups(tab_id) {
            let title = group_title(group);
            if !title.to_lowercase().contains(query) {
                continue;
            }
            let tabs = session.map_or(0, |session| {
                session
                    .tabs
                    .iter()
                    .filter(|tab| tab.group.as_ref() == Some(&group.id))
                    .count()
            });
            choices.push(Choice {
                title,
                detail: format!(
                    "{} · {tabs} {}{}",
                    group.color.label(),
                    if tabs == 1 { "tab" } else { "tabs" },
                    if group.folded { " · Folded" } else { "" }
                ),
                group: None,
                reason: None,
                action: ChoiceAction::JoinTabGroup(tab_id.clone(), group.id.clone()),
            });
        }
        choices
    }

    /// The label chip of `group`, `width` wide: a dot and its name, or a pill for an
    /// unnamed group; its tab count while folded. Click folds or unfolds, drag moves
    /// the group, right-click opens its menu.
    pub(super) fn render_group_label(
        &self,
        index: usize,
        group: &TabGroup,
        tabs: usize,
        width: f32,
        lit: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let header_alpha = self.effective_background_opacity();
        let tint = color(self.group_color(group.color));
        let text = label_text(group, tabs);
        let (drag_id, menu_id) = (group.id.clone(), group.id.clone());
        let title = group_title(group);
        let count = if tabs == 1 {
            "1 tab".to_owned()
        } else {
            format!("{tabs} tabs")
        };
        let detail = format!(
            "{count} · Click to {}",
            if group.folded { "unfold" } else { "fold" }
        );
        let tooltip = self.tab_menu.is_none()
            && self.context_menu.is_none()
            && self.dragging_tab.is_none()
            && self.dragging_group.is_none();
        let marker = if group.name.is_empty() {
            div().w(px(LABEL_PILL)).h(px(LABEL_DOT))
        } else {
            div().size(px(LABEL_DOT))
        };
        div()
            .id(("tab-group", index))
            .group("tab-group")
            .relative()
            .h_full()
            .w(px(width))
            .flex_none()
            .px(px(LABEL_PADDING))
            .flex()
            .items_center()
            .gap(px(LABEL_GAP))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(if group.folded {
                colors.muted
            } else {
                colors.foreground
            }))
            .hover(|style| style.cursor_pointer())
            .child(
                div()
                    .absolute()
                    .left(px(2.0))
                    .right(px(2.0))
                    .top(px(4.0))
                    .bottom(px(4.0))
                    .rounded(px(4.0))
                    .group_hover("tab-group", move |style| {
                        style
                            .bg(color(blend_rgb(colors.background, colors.foreground, 0.04)))
                            .opacity(header_alpha)
                    })
                    .when(lit, |chip| {
                        chip.border_1()
                            .border_color(color(colors.accent))
                            .bg(color(colors.accent).opacity(0.18))
                    }),
            )
            .child(marker.flex_none().rounded_full().bg(tint))
            .children(text.map(|text| {
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(text)
            }))
            .child(
                div()
                    .absolute()
                    .left(px(4.0))
                    .right_0()
                    .bottom_0()
                    .h(px(GROUP_LINE))
                    .bg(tint),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.dragging_group = Some(drag_id.clone());
                    this.tab_drag_origin = Some(event.position);
                    this.titlebar_drag = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_group_menu(menu_id.clone(), event.position, window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .when(tooltip, |label| {
                label.tooltip(controls::tooltip(title, Some(detail.into()), None))
            })
            .into_any_element()
    }

    /// The group label's menu: Rename group…, Colour ▸, Fold or Unfold, Move group to
    /// new window, Ungroup and Close group….
    pub(super) fn open_group_menu(
        &mut self,
        id: TabGroupId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, group)) = self.group_by_id(&id) else {
            return;
        };
        let (current, folded) = (group.color, group.folded);
        // The group commands' reasons, for this group rather than the selected tab's.
        let context = commands::CommandContext {
            has_tab: true,
            tab_grouped: true,
            ..self.command_context(cx)
        };
        let reason = |command: Command| command.disabled_reason(&context);
        let entry = |label: &'static str,
                     command: Command,
                     run: fn(&mut Self, &TabGroupId, &mut Window, &mut Context<Self>)|
         -> MenuEntry {
            let id = id.clone();
            menu_item(
                label,
                cx.listener(move |this, _, window, cx| {
                    run(this, &id, window, cx);
                    cx.notify();
                }),
            )
            .disabled_because(reason(command))
            .into()
        };
        let colours = TabGroupColor::ALL.into_iter().map(|choice| {
            let id = id.clone();
            menu_item(
                choice.label(),
                cx.listener(move |this, _, _, cx| {
                    this.set_group_color(&id, choice);
                    cx.notify();
                }),
            )
            .checked(choice == current)
            .swatch(color(self.group_color(choice)))
            .disabled_because(reason(Command::RenameTabGroup))
            .into()
        });
        let entries = vec![
            entry(
                "Rename group…",
                Command::RenameTabGroup,
                |this, id, _, _| this.rename_group(id),
            ),
            controls::submenu("Colour", colours),
            entry(
                if folded { "Unfold group" } else { "Fold group" },
                Command::ToggleTabGroupFold,
                |this, id, _, _| this.toggle_group_fold(id),
            ),
            MenuEntry::Separator,
            entry(
                "Move group to new window",
                Command::MoveTabGroupToNewWindow,
                |this, id, window, cx| this.move_group_to_new_window(id, window, cx),
            ),
            entry("Ungroup", Command::UngroupTabGroup, |this, id, _, _| {
                this.ungroup(id)
            }),
            entry(
                "Close group…",
                Command::CloseTabGroup,
                |this, id, window, cx| this.close_group(id, window, cx),
            ),
        ];
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::SurfaceId;

    /// Tabs a, b, c, d, e, f: b and c in open group "g", e and f in folded group "f".
    fn session() -> WorkspaceSession {
        let group = |name: &str, folded| TabGroup {
            id: TabGroupId::from(name),
            name: name.into(),
            color: TabGroupColor::Blue,
            folded,
        };
        WorkspaceSession {
            id: SessionId::from("session"),
            label: "session".into(),
            tabs: [
                ("a", None),
                ("b", Some("g")),
                ("c", Some("g")),
                ("d", None),
                ("e", Some("f")),
                ("f", Some("f")),
            ]
            .into_iter()
            .map(|(id, group)| WorkspaceTab {
                id: TabId::from(id),
                label: id.into(),
                layout: LayoutNode::terminal(
                    PaneId::from(format!("{id}-pane")),
                    SurfaceId::from(format!("{id}-surface")),
                ),
                previous_layout: None,
                merge: None,
                group: group.map(TabGroupId::from),
            })
            .collect(),
            groups: vec![group("g", false), group("f", true)],
        }
    }

    /// Tabs 100 wide, labels 40: a 0–100, g 100–140, b 140–240, c 240–340,
    /// d 340–440, f 440–480.
    fn strip(session: &WorkspaceSession) -> TabStrip {
        TabStrip::lay_out(strip_chips(session, &session.tabs), |chip| match chip {
            Chip::Tab(_) => 100.0,
            Chip::Label(_) => 40.0,
        })
    }

    fn join(group: &str) -> TabPlacement {
        TabPlacement::Join {
            group_id: TabGroupId::from(group),
        }
    }

    #[test]
    fn labels_stand_before_their_groups_and_folded_groups_hide_their_tabs() {
        let session = session();
        assert_eq!(
            strip_chips(&session, &session.tabs),
            [
                Chip::Tab(TabId::from("a")),
                Chip::Label(TabGroupId::from("g")),
                Chip::Tab(TabId::from("b")),
                Chip::Tab(TabId::from("c")),
                Chip::Tab(TabId::from("d")),
                Chip::Label(TabGroupId::from("f")),
            ]
        );
        // A hidden first member: the label moves to the next visible one.
        let visible = session.tabs.iter().filter(|tab| tab.id.as_str() != "b");
        assert_eq!(
            strip_chips(&session, visible)[1..3],
            [
                Chip::Label(TabGroupId::from("g")),
                Chip::Tab(TabId::from("c")),
            ]
        );
    }

    #[test]
    fn a_dropped_tab_joins_the_group_whose_span_holds_the_pointer_and_leaves_elsewhere() {
        let session = session();
        let chips = strip(&session).chips;
        let drop = |dragged: &str, x| tab_drop(&session, &chips, &TabId::from(dragged), x);
        // a's right half, before g's label: between a and g, ungrouped.
        assert_eq!(
            drop("d", 60.0),
            TabDrop {
                index: 1,
                placement: TabPlacement::Leave,
                mark: DropMark::Bar(100.0),
            }
        );
        // On g's label: first in g.
        assert_eq!(
            drop("d", 110.0),
            TabDrop {
                index: 1,
                placement: join("g"),
                mark: DropMark::Bar(140.0),
            }
        );
        // c's right half: last in g.
        assert_eq!(
            drop("a", 300.0),
            TabDrop {
                index: 2,
                placement: join("g"),
                mark: DropMark::Bar(340.0),
            }
        );
        // d's left half, past g's span: after g, out of it.
        assert_eq!(
            drop("b", 350.0),
            TabDrop {
                index: 2,
                placement: TabPlacement::Leave,
                mark: DropMark::Bar(340.0),
            }
        );
        // d's right half: before the folded group f, out of it.
        assert_eq!(
            drop("a", 420.0),
            TabDrop {
                index: 3,
                placement: TabPlacement::Leave,
                mark: DropMark::Bar(440.0),
            }
        );
        // On the folded label: into f, after its tabs.
        assert_eq!(
            drop("a", 450.0),
            TabDrop {
                index: 5,
                placement: join("f"),
                mark: DropMark::Label(TabGroupId::from("f")),
            }
        );
        // Past every chip: last, ungrouped.
        assert_eq!(
            drop("b", 900.0),
            TabDrop {
                index: 5,
                placement: TabPlacement::Leave,
                mark: DropMark::Bar(480.0),
            }
        );
    }

    #[test]
    fn a_dropped_group_lands_between_tabs_and_groups_never_inside_one() {
        let session = session();
        let chips = strip(&session).chips;
        let drop = |dragged: &str, x| group_drop(&session, &chips, &TabGroupId::from(dragged), x);
        // f before a.
        assert_eq!(drop("f", 10.0), (0, 0.0));
        // f over g's tabs: g moves whole, so f lands before or after it.
        assert_eq!(drop("f", 200.0), (1, 100.0));
        assert_eq!(drop("f", 300.0), (3, 340.0));
        // g after d; the index counts the tabs outside g.
        assert_eq!(drop("g", 420.0), (2, 440.0));
        assert_eq!(drop("g", 900.0), (4, 480.0));
    }
}
