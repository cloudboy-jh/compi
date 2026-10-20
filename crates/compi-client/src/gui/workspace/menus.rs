//! Menus at the pointer. A right-click opens a menu for what was clicked: a terminal,
//! a floating pane's title, the header, a pane in a content pane; a pane's grip ⋯
//! opens its pane menu. They are Kit popup menus (`controls::context_menu`).
//!
//! Every pane menu lists the same pane actions in the same order ([`PaneAction`]):
//! Focus (where the pane is not focused), Show terminal / Show Files, Settings or
//! Monitor (a terminal's view, whichever side is hidden), Move to new tab, Move to its
//! own pane (a terminal's view), Float or Dock pane, End process… (terminals; it always
//! asks first), Close Files / Settings / Monitor (a terminal's view) and Close pane.
//!
//! The tab menu lists the tab's actions, then a Panes group: one row per pane that
//! focuses it, a × that closes it in one click while the menu stays open, and a ›
//! submenu of its pane actions. Kit's popup menu cannot keep itself open or tell
//! which row the keyboard is on, so this one menu is drawn with Compi rows in the
//! menu's look (`controls::menu_surface`).
use super::*;
use crate::gui::controls::{IconName, MenuEntry, menu_item};
use gpui::{Anchor, Hsla, Keystroke, anchored, deferred};
use gpui_component::ActiveTheme as _;

/// What a pane menu does to its pane, in the order every pane menu lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum PaneAction {
    Focus,
    /// Show terminal while a terminal shows its view; Show Files, Settings or Monitor
    /// while it shows its shell with a view behind it.
    ShowSide,
    MoveToNewTab,
    /// A terminal's view into a pane of its own beside the terminal.
    MoveToOwnPane,
    /// Float pane, or Dock pane for a floating one.
    Float,
    /// End process…, for terminals; it asks first.
    EndProcess,
    /// Close Files, Settings or Monitor: a terminal's view goes, its shell stays.
    CloseView,
    Close,
}

impl PaneAction {
    const ALL: [Self; 8] = [
        Self::Focus,
        Self::ShowSide,
        Self::MoveToNewTab,
        Self::MoveToOwnPane,
        Self::Float,
        Self::EndProcess,
        Self::CloseView,
        Self::Close,
    ];

    /// The command that does the same to the focused pane, for its shortcut hint.
    fn command(self) -> Option<Command> {
        match self {
            Self::Focus | Self::MoveToOwnPane | Self::CloseView => None,
            Self::ShowSide => Some(Command::BrowseFiles),
            Self::MoveToNewTab => Some(Command::DetachPane),
            Self::Float => Some(Command::TogglePaneFloat),
            Self::EndProcess => Some(Command::EndSurface),
            Self::Close => Some(Command::RemovePane),
        }
    }

    fn label(self, facts: &PaneFacts) -> &'static str {
        match self {
            Self::Focus => "Focus",
            Self::ShowSide if facts.view_shown => "Show terminal",
            Self::ShowSide => match facts.view {
                Some("Settings") => "Show Settings",
                Some("Monitor") => "Show Monitor",
                _ => "Show Files",
            },
            Self::MoveToNewTab => "Move to new tab",
            Self::MoveToOwnPane => "Move to its own pane",
            Self::Float if facts.floating => "Dock pane",
            Self::Float => "Float pane",
            Self::EndProcess => "End process…",
            Self::CloseView => match facts.view {
                Some("Settings") => "Close Settings",
                Some("Monitor") => "Close Monitor",
                _ => "Close Files",
            },
            Self::Close => "Close pane",
        }
    }
}

/// What decides a pane's actions.
#[derive(Clone, Copy, Debug)]
pub(in crate::gui) struct PaneFacts {
    /// Panes in its tab.
    panes: usize,
    floating: bool,
    /// As many panes float as may.
    floats_full: bool,
    /// For a terminal, whether its process runs; `None` for a content pane.
    running: Option<bool>,
    /// What a terminal's view shows ([`super::views::view_label`]); `None` without one.
    view: Option<&'static str>,
    /// The view shows, in place of the shell.
    view_shown: bool,
    /// Why the view cannot move to its own pane now.
    view_busy: Option<&'static str>,
}

/// The pane actions a menu lists for a pane, each with why it is unavailable: Focus
/// only when the pane is not `focused`, End process… only for a terminal, the view's
/// actions only for a terminal carrying one.
fn pane_actions(
    facts: &PaneFacts,
    focused: bool,
) -> impl Iterator<Item = (PaneAction, Option<&'static str>)> + '_ {
    PaneAction::ALL
        .into_iter()
        .filter(move |action| match action {
            PaneAction::Focus => !focused,
            PaneAction::EndProcess => facts.running.is_some(),
            PaneAction::ShowSide | PaneAction::MoveToOwnPane | PaneAction::CloseView => {
                facts.view.is_some()
            }
            _ => true,
        })
        .map(|action| (action, pane_action_reason(action, facts)))
}

fn pane_action_reason(action: PaneAction, facts: &PaneFacts) -> Option<&'static str> {
    match action {
        PaneAction::MoveToNewTab if facts.panes < 2 => Some("The tab has only one pane"),
        PaneAction::Float if !facts.floating && facts.floats_full => {
            Some("Dock a floating pane first")
        }
        PaneAction::EndProcess if facts.running != Some(true) => Some("The process is not running"),
        PaneAction::MoveToOwnPane if facts.floating => Some("Dock this pane first"),
        PaneAction::MoveToOwnPane => facts.view_busy,
        _ => None,
    }
}

/// The tab menu's own actions, in groups; they act on the menu's tab.
const TAB_COMMANDS: [&[(Command, &str)]; 3] = [
    &[
        (Command::NewTab, "New tab"),
        (Command::RenameTab, "Rename tab"),
    ],
    &[
        (Command::SplitRight, "Split right"),
        (Command::SplitDown, "Split down"),
        (Command::ArrangePanes, "Arrange…"),
        (Command::RestoreArrangement, "Restore previous arrangement"),
    ],
    &[
        (Command::MoveTabToNewWindow, "Move to new window"),
        (Command::DetachTab, "Hide tab"),
        (Command::RemoveTab, "Close tab"),
    ],
];

/// The open tab menu.
pub(in crate::gui) struct TabMenu {
    tab_id: TabId,
    position: Point<Pixels>,
    focus: FocusHandle,
    /// Where the keyboard was before the menu took it.
    previous_focus: Option<FocusHandle>,
    /// The row the keyboard is on, lit like a hovered row.
    highlight: Option<TabMenuRow>,
    /// The pane whose › submenu is open, and the submenu row the keyboard is on.
    submenu: Option<(PaneId, Option<PaneAction>)>,
}

/// A row of the tab menu that can be picked.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TabMenuRow {
    Command(Command),
    Pane(PaneId),
}

impl CompiApp {
    /// The tab holding `pane_id`, and what decides the pane's actions.
    fn pane_facts(&self, pane_id: &PaneId) -> Option<(TabId, PaneFacts)> {
        let workspace = self.workspace.as_ref()?;
        let tab = workspace
            .sessions
            .iter()
            .flat_map(|session| &session.tabs)
            .find(|tab| layout_contains_pane(&tab.layout, pane_id))?;
        let mut leaves = Vec::new();
        collect_leaves(&tab.layout, &mut leaves);
        let surface = leaves
            .iter()
            .find(|(id, _)| id == pane_id)
            .and_then(|(_, surface)| surface.as_ref());
        let running = surface.map(|surface| {
            workspace.surface(surface).is_some_and(|surface| {
                matches!(
                    surface.status,
                    SurfaceStatus::Running | SurfaceStatus::Starting
                )
            })
        });
        let view = leaf_node(&tab.layout, pane_id).and_then(LayoutNode::view);
        let view_shown = view.is_some_and(|view| view.side == compi_protocol::PaneSide::View);
        let view = view.map(|view| super::views::view_label(&view.content));
        Some((
            tab.id.clone(),
            PaneFacts {
                panes: leaves.len(),
                floating: self.state.is_floating(pane_id),
                floats_full: self.state.floating.len() >= crate::client_state::MAX_FLOATING,
                running,
                view,
                view_shown,
                view_busy: view.and_then(|_| self.content_busy(pane_id)),
            },
        ))
    }

    /// The title of a pane's × : Close pane, with its shortcut when the pane is the
    /// focused one, which is what the shortcut closes.
    pub(super) fn close_pane_title(&self, pane_id: &PaneId) -> String {
        match (self.focused_pane() == Some(pane_id))
            .then(|| self.configured_shortcut(Command::RemovePane))
            .flatten()
        {
            Some(shortcut) => format!("Close pane · {}", controls::key_label(shortcut)),
            None => "Close pane".to_owned(),
        }
    }

    /// The pane action set as a "Pane" submenu, after a row's own actions.
    pub(super) fn pane_submenu(&self, pane_id: &PaneId, cx: &Context<Self>) -> MenuEntry {
        controls::submenu("Pane", self.pane_action_entries(pane_id, cx))
    }

    /// The pane action set for `pane_id` as menu entries; shortcut hints show when
    /// the pane is the focused one, which is what a shortcut acts on.
    fn pane_action_entries(&self, pane_id: &PaneId, cx: &Context<Self>) -> Vec<MenuEntry> {
        let Some((_, facts)) = self.pane_facts(pane_id) else {
            return Vec::new();
        };
        let focused = self.focused_pane() == Some(pane_id);
        pane_actions(&facts, focused)
            .map(|(action, reason)| {
                let pane_id = pane_id.clone();
                menu_item(
                    action.label(&facts),
                    cx.listener(move |this, _, window, cx| {
                        this.run_pane_action(&pane_id, action, window, cx);
                    }),
                )
                .key(
                    action
                        .command()
                        .filter(|_| focused)
                        .and_then(|command| self.configured_shortcut(command)),
                )
                .disabled_because(reason)
                .into()
            })
            .collect()
    }

    /// Does `action` to `pane_id`, wherever the pane is.
    pub(super) fn run_pane_action(
        &mut self,
        pane_id: &PaneId,
        action: PaneAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((tab_id, facts)) = self.pane_facts(pane_id) else {
            self.global_error = Some("This pane is no longer open".into());
            cx.notify();
            return;
        };
        if let Some(reason) = pane_action_reason(action, &facts) {
            self.global_error = Some(reason.into());
            cx.notify();
            return;
        }
        match action {
            PaneAction::Focus | PaneAction::ShowSide => {
                if self.selected_tab().is_none_or(|tab| tab.id != tab_id) {
                    self.select_terminal(tab_id, false);
                    self.rebuild_layout(window, true);
                }
                self.focus_pane(pane_id.clone());
                if action == PaneAction::ShowSide {
                    self.set_pane_side(
                        pane_id,
                        if facts.view_shown {
                            compi_protocol::PaneSide::Terminal
                        } else {
                            compi_protocol::PaneSide::View
                        },
                    );
                }
            }
            PaneAction::MoveToNewTab => self.mutate(
                WorkspaceMutation::DetachPane {
                    pane_id: pane_id.clone(),
                },
                true,
            ),
            PaneAction::Float if facts.floating => self.dock_pane(pane_id),
            PaneAction::Float => self.float_pane(pane_id.clone()),
            PaneAction::EndProcess => self.end_process(pane_id),
            PaneAction::MoveToOwnPane => {
                if !facts.floating && self.selected_tab().is_none_or(|tab| tab.id != tab_id) {
                    self.select_terminal(tab_id, false);
                    self.rebuild_layout(window, true);
                }
                self.move_view_to_own_pane(pane_id);
            }
            PaneAction::CloseView => self.close_view(pane_id, window, cx),
            PaneAction::Close => self.close_pane(pane_id, window, cx),
        }
        window.focus(&self.focus_handle, cx);
        self.rebuild_layout(window, true);
        cx.notify();
    }

    /// Close pane: a terminal ends its process tree (asking first only with "Confirm
    /// before closing"), a content pane asks only about unsaved edits. A tab's last
    /// pane closes the tab.
    pub(super) fn close_pane(
        &mut self,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((tab_id, facts)) = self.pane_facts(pane_id) else {
            return;
        };
        let pane_id = pane_id.clone();
        if facts.panes < 2 {
            self.close_work(
                WorkspaceMutation::RemoveTab { tab_id },
                "Close this tab and end every process in its panes?",
                "Close tab",
                window,
                cx,
            );
        } else if facts.running.is_none() {
            // No process ends; only unsaved edits would be lost.
            self.remove_work(WorkspaceMutation::RemovePane { pane_id }, window, cx);
        } else {
            self.close_work(
                WorkspaceMutation::RemovePane { pane_id },
                "Close this pane and end its process tree?",
                "Close pane",
                window,
                cx,
            );
        }
    }

    /// End process…: always asks, with End process focused. The pane stays, showing
    /// the final screen.
    pub(super) fn end_process(&mut self, pane_id: &PaneId) {
        let Some(surface) = self.workspace.as_ref().and_then(|workspace| {
            let mut leaves = Vec::new();
            for tab in workspace.sessions.iter().flat_map(|session| &session.tabs) {
                collect_leaves(&tab.layout, &mut leaves);
            }
            let (_, surface) = leaves.into_iter().find(|(id, _)| id == pane_id)?;
            workspace.surface(&surface?).cloned()
        }) else {
            return;
        };
        self.confirm_removal(
            WorkspaceMutation::EndSurface {
                surface_id: surface.id,
                expected_lifetime: surface.process_lifetime_id,
            },
            "End this terminal's process and its child processes? The pane stays open with its final screen.",
            "End process",
        );
        self.overlay_focus = 1;
    }

    /// Opens `entries` as a menu with its `anchor` corner at `position`; any other
    /// popup or menu closes.
    pub(super) fn open_menu(
        &mut self,
        position: Point<Pixels>,
        anchor: Anchor,
        entries: Vec<MenuEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.popup = None;
        self.close_tab_menu(window, cx);
        self.context_menu = Some(controls::context_menu(
            position,
            anchor,
            entries,
            window,
            cx,
            |this, menu, _, cx| {
                if this.context_menu.as_ref().is_some_and(|open| open.is(menu)) {
                    this.context_menu = None;
                    cx.notify();
                }
            },
        ));
        cx.notify();
    }

    /// A menu row running `command` as the palette would.
    fn command_entry(
        &self,
        command: Command,
        label: &'static str,
        context: &commands::CommandContext,
        cx: &Context<Self>,
    ) -> MenuEntry {
        menu_item(
            label,
            cx.listener(move |this, _, window, cx| {
                this.execute(command, window, cx);
            }),
        )
        .key(self.configured_shortcut(command))
        .disabled_because(command.disabled_reason(context))
        .into()
    }

    /// Right-click on a terminal: clipboard, splits, then the pane actions. The
    /// terminal takes focus first, as a click would.
    pub(super) fn open_terminal_menu(
        &mut self,
        pane_id: &PaneId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.focus_pane(pane_id.clone());
        let context = self.command_context(cx);
        let mut entries = vec![
            self.command_entry(Command::Copy, "Copy", &context, cx),
            self.command_entry(Command::Paste, "Paste", &context, cx),
            MenuEntry::Separator,
            self.command_entry(Command::SplitRight, "Split right", &context, cx),
            self.command_entry(Command::SplitDown, "Split down", &context, cx),
            MenuEntry::Separator,
        ];
        entries.extend(self.pane_action_entries(pane_id, cx));
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// The pane actions alone: a floating pane's title, a pane's grip, a content
    /// pane outside its rows.
    pub(super) fn open_pane_menu(
        &mut self,
        pane_id: &PaneId,
        position: Point<Pixels>,
        anchor: Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.pane_action_entries(pane_id, cx);
        self.open_menu(position, anchor, entries, window, cx);
    }

    /// Right-click on empty header space.
    pub(super) fn open_header_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let context = self.command_context(cx);
        let entries = vec![
            self.command_entry(Command::NewTab, "New tab", &context, cx),
            self.command_entry(Command::CreateWorkspace, "New workspace", &context, cx),
            MenuEntry::Separator,
            self.command_entry(Command::SplitRight, "Split right", &context, cx),
            self.command_entry(Command::SplitDown, "Split down", &context, cx),
            MenuEntry::Separator,
            self.command_entry(Command::ToggleSidebar, "Toggle sidebar", &context, cx),
        ];
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }

    /// The narrow header's pane button: the split and zoom buttons it stands for,
    /// then the focused pane's actions, under the button.
    pub(super) fn open_header_pane_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let context = self.command_context(cx);
        let mut entries = vec![
            self.command_entry(Command::SplitRight, "Split right", &context, cx),
            self.command_entry(Command::SplitDown, "Split down", &context, cx),
            self.command_entry(
                Command::TogglePaneZoom,
                self.command_label(Command::TogglePaneZoom),
                &context,
                cx,
            ),
        ];
        if let Some(pane_id) = self.focused_pane().cloned() {
            entries.push(MenuEntry::Separator);
            entries.push(MenuEntry::Label("Focused pane".into()));
            entries.extend(self.pane_action_entries(&pane_id, cx));
        }
        let (viewport_width, _) = logical_viewport_dimensions(window);
        let position = point(
            px(viewport_width - WINDOW_CONTROLS_WIDTH),
            px(CHROME_HEIGHT),
        );
        self.open_menu(position, Anchor::TopRight, entries, window, cx);
    }

    /// Right-click on a tab chip: the tab menu, at the pointer, holding the keyboard.
    pub(super) fn open_tab_menu(
        &mut self,
        tab_id: TabId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.popup = None;
        self.context_menu = None;
        let previous_focus = match self.tab_menu.take() {
            Some(menu) => menu.previous_focus,
            None => window.focused(cx),
        };
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.tab_menu = Some(TabMenu {
            tab_id,
            position,
            focus,
            previous_focus,
            highlight: None,
            submenu: None,
        });
        cx.notify();
    }

    /// Closes the tab menu, returning the keyboard where it was unless something
    /// else took it.
    pub(super) fn close_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.tab_menu.take() else {
            return;
        };
        if menu.focus.contains_focused(window, cx) || window.focused(cx).is_none() {
            window.focus(
                menu.previous_focus.as_ref().unwrap_or(&self.focus_handle),
                cx,
            );
        }
        cx.notify();
    }

    /// The tab menu goes with its tab.
    pub(super) fn prune_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .tab_menu
            .as_ref()
            .is_some_and(|menu| self.tab_by_id(&menu.tab_id).is_none())
        {
            self.close_tab_menu(window, cx);
        }
    }

    /// The tab menu's command rows with why each is unavailable. The reasons hold for
    /// the selected tab; a command for another tab is checked once that tab is
    /// selected.
    fn tab_menu_commands(
        &self,
        tab_id: &TabId,
        cx: &App,
    ) -> Vec<Vec<(Command, &'static str, Option<&'static str>)>> {
        let selected = self.selected_tab().is_some_and(|tab| &tab.id == tab_id);
        let context = self.command_context(cx);
        TAB_COMMANDS
            .iter()
            .map(|group| {
                group
                    .iter()
                    .map(|&(command, label)| {
                        let reason = selected
                            .then(|| command.disabled_reason(&context))
                            .flatten();
                        (command, label, reason)
                    })
                    .collect()
            })
            .collect()
    }

    /// The rows the keyboard moves through, in order.
    fn tab_menu_rows(&self, tab_id: &TabId, cx: &App) -> Vec<TabMenuRow> {
        let mut rows: Vec<_> = self
            .tab_menu_commands(tab_id, cx)
            .into_iter()
            .flatten()
            .filter(|(_, _, reason)| reason.is_none())
            .map(|(command, _, _)| TabMenuRow::Command(command))
            .collect();
        if let Some(tab) = self.tab_by_id(tab_id) {
            rows.extend(
                self.tab_panes(tab)
                    .into_iter()
                    .map(|pane| TabMenuRow::Pane(pane.pane_id)),
            );
        }
        rows
    }

    /// The submenu actions the keyboard moves through for `pane_id`.
    fn tab_menu_actions(&self, pane_id: &PaneId) -> Vec<PaneAction> {
        let Some((_, facts)) = self.pane_facts(pane_id) else {
            return Vec::new();
        };
        pane_actions(&facts, self.focused_pane() == Some(pane_id))
            .filter(|(_, reason)| reason.is_none())
            .map(|(action, _)| action)
            .collect()
    }

    /// ↑↓ move, Enter picks, Delete closes the highlighted pane, → opens its
    /// submenu, ← and Escape go back; Escape on the menu closes it.
    fn tab_menu_key(&mut self, key: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &self.tab_menu else {
            return;
        };
        let (tab_id, highlight) = (menu.tab_id.clone(), menu.highlight.clone());
        if let Some((pane_id, sub_highlight)) = menu.submenu.clone() {
            let actions = self.tab_menu_actions(&pane_id);
            match key.key.as_str() {
                "up" | "down" => {
                    let next = step(&actions, sub_highlight.as_ref(), key.key == "down");
                    if let Some(menu) = &mut self.tab_menu {
                        menu.submenu = Some((pane_id, next));
                    }
                }
                "enter" | "space" => {
                    if let Some(action) = sub_highlight {
                        self.close_tab_menu(window, cx);
                        self.run_pane_action(&pane_id, action, window, cx);
                    }
                }
                "left" | "escape" => {
                    if let Some(menu) = &mut self.tab_menu {
                        menu.submenu = None;
                    }
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        match key.key.as_str() {
            "up" | "down" => {
                let rows = self.tab_menu_rows(&tab_id, cx);
                let next = step(&rows, highlight.as_ref(), key.key == "down");
                if let Some(menu) = &mut self.tab_menu {
                    menu.highlight = next;
                }
            }
            "enter" | "space" => {
                if let Some(row) = highlight {
                    self.pick_tab_menu_row(&tab_id, row, window, cx);
                }
            }
            "delete" | "backspace" => {
                if let Some(TabMenuRow::Pane(pane_id)) = highlight {
                    self.close_tab_menu_pane(&tab_id, &pane_id, window, cx);
                }
            }
            "right" => {
                if let Some(TabMenuRow::Pane(pane_id)) = highlight {
                    let first = self.tab_menu_actions(&pane_id).first().copied();
                    if let Some(menu) = &mut self.tab_menu {
                        menu.submenu = Some((pane_id, first));
                    }
                }
            }
            "escape" => self.close_tab_menu(window, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Runs a tab menu row: a command on the menu's tab, or focusing a pane there.
    fn pick_tab_menu_row(
        &mut self,
        tab_id: &TabId,
        row: TabMenuRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tab_menu(window, cx);
        match row {
            TabMenuRow::Command(command) => {
                // The command acts on the selected tab; `execute` checks it there.
                if self.selected_tab().is_none_or(|tab| &tab.id != tab_id) {
                    self.select_terminal(tab_id.clone(), false);
                    self.rebuild_layout(window, true);
                }
                self.execute(command, window, cx);
            }
            TabMenuRow::Pane(pane_id) => {
                self.run_pane_action(&pane_id, PaneAction::Focus, window, cx)
            }
        }
        cx.notify();
    }

    /// The × on a pane row: Close pane. The menu stays open with the row gone,
    /// unless closing asks first or closes the tab.
    fn close_tab_menu_pane(
        &mut self,
        tab_id: &TabId,
        pane_id: &PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The keyboard moves on to the next pane, or the one before at the end.
        let rows = self.tab_menu_rows(tab_id, cx);
        let at = rows
            .iter()
            .position(|row| row == &TabMenuRow::Pane(pane_id.clone()));
        let next = at.and_then(|at| {
            rows.get(at + 1)
                .or_else(|| at.checked_sub(1).and_then(|before| rows.get(before)))
                .filter(|row| matches!(row, TabMenuRow::Pane(_)))
                .cloned()
        });
        if let Some(menu) = &mut self.tab_menu {
            menu.submenu = None;
            if menu.highlight.as_ref() == Some(&TabMenuRow::Pane(pane_id.clone())) {
                menu.highlight = next;
            }
        }
        self.close_pane(pane_id, window, cx);
        if self.overlay.is_some() {
            // A confirmation or an unsaved-edits prompt takes over.
            self.tab_menu = None;
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    /// The open menus, over everything else in the window.
    pub(super) fn render_menus(&self, window: &Window, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut menus = Vec::new();
        if let Some(menu) = &self.context_menu {
            menus.push(menu.render());
        }
        if let Some(menu) = self.render_tab_menu(window, cx) {
            menus.push(menu);
        }
        menus
    }

    fn render_tab_menu(&self, window: &Window, cx: &Context<Self>) -> Option<AnyElement> {
        let menu = self.tab_menu.as_ref()?;
        let tab = self.tab_by_id(&menu.tab_id)?;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (group, commands) in self
            .tab_menu_commands(&menu.tab_id, cx)
            .into_iter()
            .enumerate()
        {
            if group > 0 {
                rows.push(controls::menu_separator_row(cx).into_any_element());
            }
            for (command, label, reason) in commands {
                let row = TabMenuRow::Command(command);
                let highlighted = menu.highlight.as_ref() == Some(&row);
                let tab_id = menu.tab_id.clone();
                rows.push(
                    controls::menu_row(
                        SharedString::from(format!("tab-menu-{}", command.spec().id)),
                        highlighted,
                        reason.is_some(),
                        cx,
                    )
                    .when(reason.is_none(), |item| {
                        let hovered_row = row.clone();
                        item.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered && let Some(menu) = &mut this.tab_menu {
                                menu.highlight = Some(hovered_row.clone());
                                menu.submenu = None;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.pick_tab_menu_row(&tab_id, row.clone(), window, cx);
                            },
                        ))
                    })
                    .child(controls::menu_item_body(
                        label.into(),
                        self.configured_shortcut(command),
                        reason.map(Into::into),
                        cx,
                    ))
                    .into_any_element(),
                );
            }
        }
        rows.push(controls::menu_separator_row(cx).into_any_element());
        rows.push(controls::menu_label_row("Panes", cx).into_any_element());
        for (index, pane) in self.tab_panes(tab).into_iter().enumerate() {
            rows.push(self.render_tab_menu_pane(menu, index, pane, muted, cx));
        }
        let (width, height) = (window.viewport_size().width, window.viewport_size().height);
        Some(
            deferred(
                anchored().position(point(px(0.0), px(0.0))).child(
                    div()
                        .id("tab-menu-backdrop")
                        .w(width)
                        .h(height)
                        .occlude()
                        .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                            this.close_tab_menu(window, cx);
                            cx.stop_propagation();
                        }))
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(
                            anchored()
                                .position(menu.position)
                                .snap_to_window_with_margin(px(8.0))
                                .child(
                                    controls::menu_surface(cx)
                                        .id("tab-menu")
                                        .occlude()
                                        .track_focus(&menu.focus)
                                        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                                        .on_key_down(cx.listener(
                                            |this, event: &KeyDownEvent, window, cx| {
                                                this.tab_menu_key(&event.keystroke, window, cx);
                                                cx.stop_propagation();
                                            },
                                        ))
                                        .children(rows),
                                ),
                        ),
                ),
            )
            .with_priority(gpui_base::POPUP_PRIORITY)
            .into_any_element(),
        )
    }

    /// A pane row: its number, title, folder, Floating mark and program status;
    /// on hover or the keyboard a × closes it; › opens its pane actions.
    fn render_tab_menu_pane(
        &self,
        menu: &TabMenu,
        index: usize,
        pane: TabPane,
        muted: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let row = TabMenuRow::Pane(pane.pane_id.clone());
        let highlighted = menu.highlight.as_ref() == Some(&row);
        let open = menu
            .submenu
            .as_ref()
            .filter(|(pane_id, _)| pane_id == &pane.pane_id);
        let lit = highlighted || open.is_some();
        let text = if lit {
            theme.accent_foreground
        } else {
            theme.popover_foreground
        };
        let detail = if lit { text.opacity(0.75) } else { muted };
        let group = SharedString::from(format!("tab-menu-pane-{index}"));
        let tab_id = menu.tab_id.clone();
        let (hover_row, click_id, close_tab, close_id, sub_id) = (
            row.clone(),
            pane.pane_id.clone(),
            tab_id.clone(),
            pane.pane_id.clone(),
            pane.pane_id.clone(),
        );
        let glyph = pane
            .surface_id
            .as_ref()
            .and_then(|surface| self.program_glyph(surface));
        let close_title = self.close_pane_title(&pane.pane_id);
        controls::menu_row(("tab-menu-pane", index), lit, false, cx)
            .group(group.clone())
            .relative()
            .gap_2()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && let Some(menu) = &mut this.tab_menu {
                    menu.highlight = Some(hover_row.clone());
                    if menu
                        .submenu
                        .as_ref()
                        .is_some_and(|(pane_id, _)| TabMenuRow::Pane(pane_id.clone()) != hover_row)
                    {
                        menu.submenu = None;
                    }
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.pick_tab_menu_row(&tab_id, TabMenuRow::Pane(click_id.clone()), window, cx);
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .text_color(detail)
                            .child(format!("{}", index + 1)),
                    )
                    .child(div().flex_none().whitespace_nowrap().child(pane.title))
                    .children(pane.directory.map(|directory| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(UI_MICRO_TEXT_SIZE))
                            .text_color(detail)
                            .child(directory)
                    }))
                    .when(pane.floating, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .whitespace_nowrap()
                                .text_color(detail)
                                .child("· Floating"),
                        )
                    })
                    .children(glyph),
            )
            .child(
                div()
                    .id(("tab-menu-pane-close", index))
                    .flex_none()
                    .size(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.0))
                    .when(!lit, |button| {
                        button
                            .invisible()
                            .group_hover(group, |style| style.visible())
                    })
                    .hover(move |style| style.bg(text.opacity(0.16)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_tab_menu_pane(&close_tab, &close_id, window, cx);
                        cx.stop_propagation();
                    }))
                    .tooltip(controls::tooltip(close_title, None, None))
                    .child(controls::glyph(IconName::X, text)),
            )
            .child(
                div()
                    .id(("tab-menu-pane-actions", index))
                    .flex_none()
                    .size(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.0))
                    .hover(move |style| style.bg(text.opacity(0.16)))
                    .on_hover(cx.listener({
                        let sub_id = sub_id.clone();
                        move |this, hovered: &bool, _, cx| {
                            if *hovered && let Some(menu) = &mut this.tab_menu {
                                if menu
                                    .submenu
                                    .as_ref()
                                    .is_none_or(|(pane_id, _)| pane_id != &sub_id)
                                {
                                    menu.submenu = Some((sub_id.clone(), None));
                                }
                                cx.notify();
                            }
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(menu) = &mut this.tab_menu {
                            if menu
                                .submenu
                                .as_ref()
                                .is_none_or(|(pane_id, _)| pane_id != &sub_id)
                            {
                                menu.submenu = Some((sub_id.clone(), None));
                            }
                            menu.highlight = Some(TabMenuRow::Pane(sub_id.clone()));
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(controls::glyph(IconName::ChevronRight, text)),
            )
            .when_some(open, |row, (pane_id, highlight)| {
                row.child(
                    div().absolute().top(px(-4.0)).right(px(-4.0)).child(
                        deferred(
                            anchored()
                                .snap_to_window_with_margin(px(8.0))
                                .child(self.render_tab_menu_actions(pane_id, *highlight, cx)),
                        )
                        .with_priority(gpui_base::POPUP_PRIORITY + 1),
                    ),
                )
            })
            .into_any_element()
    }

    /// A pane row's › submenu: its pane actions.
    fn render_tab_menu_actions(
        &self,
        pane_id: &PaneId,
        highlight: Option<PaneAction>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some((_, facts)) = self.pane_facts(pane_id) else {
            return div().into_any_element();
        };
        let focused = self.focused_pane() == Some(pane_id);
        let rows = pane_actions(&facts, focused).map(|(action, reason)| {
            let (hover_id, run_id) = (pane_id.clone(), pane_id.clone());
            controls::menu_row(
                SharedString::from(format!("tab-menu-action-{action:?}")),
                highlight == Some(action),
                reason.is_some(),
                cx,
            )
            .when(reason.is_none(), |row| {
                row.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered && let Some(menu) = &mut this.tab_menu {
                        menu.submenu = Some((hover_id.clone(), Some(action)));
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_tab_menu(window, cx);
                    this.run_pane_action(&run_id, action, window, cx);
                }))
            })
            .child(controls::menu_item_body(
                action.label(&facts).into(),
                action
                    .command()
                    .filter(|_| focused)
                    .and_then(|command| self.configured_shortcut(command)),
                reason.map(Into::into),
                cx,
            ))
        });
        controls::menu_surface(cx)
            .id("tab-menu-actions")
            .occlude()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .children(rows)
            .into_any_element()
    }
}

/// The item after (or before) `from` in `items`, wrapping; the first (or last) when
/// none is current.
fn step<T: Clone + PartialEq>(items: &[T], from: Option<&T>, forward: bool) -> Option<T> {
    let count = items.len();
    if count == 0 {
        return None;
    }
    let at = from.and_then(|from| items.iter().position(|item| item == from));
    let next = match (at, forward) {
        (None, true) => 0,
        (None, false) => count - 1,
        (Some(at), true) => (at + 1) % count,
        (Some(at), false) => (at + count - 1) % count,
    };
    items.get(next).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(panes: usize, floating: bool, floats_full: bool, running: Option<bool>) -> PaneFacts {
        PaneFacts {
            panes,
            floating,
            floats_full,
            running,
            view: None,
            view_shown: false,
            view_busy: None,
        }
    }

    fn listed(facts: &PaneFacts, focused: bool) -> Vec<(PaneAction, Option<&'static str>)> {
        pane_actions(facts, focused).collect()
    }

    #[test]
    fn pane_actions_say_why_they_are_unavailable() {
        use PaneAction::*;
        // A lone running terminal, focused: no Focus, and it cannot move to a new tab.
        assert_eq!(
            listed(&facts(1, false, false, Some(true)), true),
            [
                (MoveToNewTab, Some("The tab has only one pane")),
                (Float, None),
                (EndProcess, None),
                (Close, None),
            ]
        );
        // Every float slot taken: a tiled pane cannot float, a floating one can dock.
        assert_eq!(
            pane_action_reason(Float, &facts(2, false, true, Some(true))),
            Some("Dock a floating pane first")
        );
        assert_eq!(
            pane_action_reason(Float, &facts(2, true, true, Some(true))),
            None
        );
        // An ended terminal keeps End process… listed, disabled; a content pane has none.
        assert_eq!(
            pane_action_reason(EndProcess, &facts(2, false, false, Some(false))),
            Some("The process is not running")
        );
        assert_eq!(
            listed(&facts(2, false, false, None), false)
                .into_iter()
                .map(|(action, _)| action)
                .collect::<Vec<_>>(),
            [Focus, MoveToNewTab, Float, Close]
        );
    }

    #[test]
    fn only_a_terminal_carrying_a_view_offers_its_actions() {
        use PaneAction::*;
        let carrying = |view, view_busy| PaneFacts {
            view: Some(view),
            view_busy,
            ..facts(2, false, false, Some(true))
        };
        let actions = |facts: &PaneFacts| {
            listed(facts, true)
                .into_iter()
                .map(|(action, _)| action)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            actions(&facts(2, false, false, Some(true))),
            [MoveToNewTab, Float, EndProcess, Close]
        );
        assert_eq!(
            actions(&carrying("Files", None)),
            [
                ShowSide,
                MoveToNewTab,
                MoveToOwnPane,
                Float,
                EndProcess,
                CloseView,
                Close
            ]
        );
        // Named for what the view shows.
        assert_eq!(CloseView.label(&carrying("Files", None)), "Close Files");
        assert_eq!(CloseView.label(&carrying("Monitor", None)), "Close Monitor");
        assert_eq!(
            CloseView.label(&carrying("Settings", None)),
            "Close Settings"
        );
        // Show names the hidden side: the view behind the shell, or the shell behind it.
        assert_eq!(ShowSide.label(&carrying("Files", None)), "Show Files");
        assert_eq!(ShowSide.label(&carrying("Settings", None)), "Show Settings");
        assert_eq!(ShowSide.label(&carrying("Monitor", None)), "Show Monitor");
        let shown = PaneFacts {
            view_shown: true,
            ..carrying("Monitor", None)
        };
        assert_eq!(ShowSide.label(&shown), "Show terminal");
        assert_eq!(pane_action_reason(ShowSide, &shown), None);
        assert_eq!(ShowSide.command(), Some(Command::BrowseFiles));
        // A save under way keeps the view where it is until it lands.
        let saving = carrying("Files", Some("Wait for the save to finish"));
        assert_eq!(
            pane_action_reason(MoveToOwnPane, &saving),
            Some("Wait for the save to finish")
        );
        assert_eq!(pane_action_reason(CloseView, &saving), None);
        // A float has no split to move the view into.
        let floating = PaneFacts {
            floating: true,
            ..carrying("Files", None)
        };
        assert_eq!(
            pane_action_reason(MoveToOwnPane, &floating),
            Some("Dock this pane first")
        );
    }
}
