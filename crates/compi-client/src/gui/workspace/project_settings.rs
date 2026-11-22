//! What you set for each project (`[projects."<key>"]`): the name it goes by, its icon,
//! whether it is pinned or archived and what new threads in it start with; and
//! Settings → Projects, where you set it. Every place a project shows reads it through
//! these helpers, so a change applies in every window at once.
//!
//! Settings lists saved projects, pinned first, with archived projects folded away.
//! A row or a sidebar's Project settings… opens a dedicated detail page.
//!
//! Archiving hides a project from the palette's Projects and the page's main list; its
//! threads and settings stay. Removing it forgets its settings and its recent entry
//! and deletes its threads, after asking; never while a tab is open in it.

use super::settings::{
    RowCx, SettingsAction, SettingsHost, Stop, Tile, choice_index, settings_card,
    settings_group_label, share_config_change,
};
use super::*;
use crate::config::{ProjectSettings, project_folder_key, project_from_key};
use crate::gui::controls::{ChipTone, IconName};
use compi_protocol::chat::{ChatHarness, ChatPicks};
use compi_protocol::{ContentEnvironment, Project, SettingsSection};
use gpui::Entity;
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

/// The Projects page, one row drawn as the page.
pub(super) const PROJECTS_ROW: &str = "projects";
/// The size of the icon on a project's row.
const ROW_ICON_SIZE: f32 = 20.0;
/// The size of the icon heading a project's detail.
const DETAIL_ICON_SIZE: f32 = 40.0;
/// The size of the agent's mark on a project's row.
const ROW_MARK_SIZE: f32 = 16.0;
/// Past this many projects the page offers a search.
const SEARCH_AFTER: usize = 8;

/// The Projects page's controls. Only list rows depend on their position in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectControl {
    Add,
    Select(usize),
    Back,
    /// The name field: the keyboard goes into it.
    Name,
    ResetName,
    ChangeIcon,
    ResetIcon,
    Pin,
    /// Left and Right step through the agents; Enter moves to the next.
    Agent,
    Model,
    Archive,
    Remove,
}

pub(super) struct ListedProject {
    pub(super) project: Project,
    archived: bool,
}

/// New threads' agent choices: Default (Settings → General's), then each agent.
const AGENT_CHOICES: [Option<ChatHarness>; ChatHarness::ALL.len() + 1] = {
    let mut choices = [None; ChatHarness::ALL.len() + 1];
    let mut index = 0;
    while index < ChatHarness::ALL.len() {
        choices[index + 1] = Some(ChatHarness::ALL[index]);
        index += 1;
    }
    choices
};

/// The saved projects and `home`, which is listed even unsaved and is never archived.
/// Archived projects come last; within each section pinned projects come first, and
/// Home after the other unpinned ones.
fn listed_order(projects: &HashMap<String, ProjectSettings>, home: &Project) -> Vec<ListedProject> {
    let home_folder = project_folder_key(home);
    let mut entries: Vec<_> = projects
        .iter()
        .filter_map(|(key, settings)| {
            let project = project_from_key(key)?;
            let is_home = project_folder_key(&project) == home_folder;
            Some((
                project,
                settings.archived && !is_home,
                settings.pinned,
                is_home,
                key,
            ))
        })
        .collect();
    let home_key = crate::config::project_key(home);
    if !entries.iter().any(|entry| entry.3) {
        entries.push((home.clone(), false, false, true, &home_key));
    }
    entries.sort_unstable_by_key(|&(_, archived, pinned, is_home, key)| {
        (archived, !pinned, is_home, key)
    });
    entries
        .into_iter()
        .map(|(project, archived, ..)| ListedProject { project, archived })
        .collect()
}

/// Whether every word of `query` (lowercase) is in the project's `name` or `place`.
fn project_matches(query: &str, name: &str, place: &str) -> bool {
    let (name, place) = (name.to_lowercase(), place.to_lowercase());
    query
        .split_whitespace()
        .all(|word| name.contains(word) || place.contains(word))
}

/// The folder and its environment, which together identify the project.
fn project_place(project: &Project) -> Cow<'_, str> {
    match &project.environment {
        ContentEnvironment::Host => Cow::Borrowed(project.root.as_str()),
        ContentEnvironment::Wsl { distribution } => {
            Cow::Owned(format!("{} · {distribution}", project.root))
        }
    }
}

/// What the Icon row says of `icon`.
fn icon_detail(icon: Option<&crate::config::ProjectIcon>) -> String {
    use crate::config::ProjectIcon;
    match icon {
        None => "Automatic: the project's favicon or logo, else its first letter.".to_owned(),
        Some(ProjectIcon::Lucide { name, .. }) => format!("The {name} icon."),
        Some(ProjectIcon::Emoji { emoji }) => format!("The emoji {emoji}."),
        Some(ProjectIcon::Monogram { text, .. }) => format!("The monogram {text}."),
        Some(ProjectIcon::File { path }) => {
            format!("The image {}.", content::file_name(path))
        }
    }
}

/// What the New threads agent switch calls `agent`.
fn agent_label(agent: Option<ChatHarness>) -> &'static str {
    agent.map_or("Default", ChatHarness::name)
}

/// What Remove from Compi asks before removing the project called `name`, which has
/// `threads` threads.
pub(super) fn remove_project_message(name: &str, threads: usize) -> String {
    let deletes = match threads {
        0 => "It has no threads.".to_owned(),
        1 => "Deletes 1 thread.".to_owned(),
        count => format!("Deletes {count} threads."),
    };
    format!(
        "Remove {name} from Compi? Compi forgets its name, icon and settings and its place \
         among recent projects. {deletes} Files on disk are untouched."
    )
}

/// What the list shows, after applying its search.
struct PageLayout {
    search: bool,
    active: Vec<usize>,
    archived: Vec<usize>,
    archived_folded: bool,
}

/// The Projects page's text fields in one Settings pane: its search and the name
/// field of the project whose detail is open.
pub(super) struct ProjectFields {
    search: Entity<InputState>,
    /// The search, trimmed and lowercased.
    query: String,
    name: Entity<InputState>,
    /// The project whose name the name field holds.
    name_for: Option<Project>,
    _events: [Subscription; 2],
}

impl ProjectFields {
    pub(super) fn new(pane_id: &PaneId, window: &mut Window, cx: &mut Context<CompiApp>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects"));
        let name = cx.new(|cx| InputState::new(window, cx));
        let events = project_field_events(pane_id, &search, &name, cx);
        Self {
            search,
            query: String::new(),
            name,
            name_for: None,
            _events: events,
        }
    }

    /// The fields' pane moved to `pane_id`.
    pub(super) fn follow_pane(&mut self, pane_id: &PaneId, cx: &mut Context<CompiApp>) {
        self._events = project_field_events(pane_id, &self.search, &self.name, cx);
    }

    pub(super) fn has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.search.focus_handle(cx).is_focused(window)
            || self.name.focus_handle(cx).is_focused(window)
    }

    pub(super) fn owns(&self, focused: &FocusHandle, cx: &App) -> bool {
        self.search.focus_handle(cx) == *focused || self.name.focus_handle(cx) == *focused
    }

    pub(super) fn search_focus(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }

    pub(super) fn name_focus(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

/// What typing in, entering and leaving the Projects page's fields of Settings pane
/// `pane_id` do: the search filters the projects; the name field takes the highlight
/// when clicked, and leaving it keeps what was typed.
fn project_field_events(
    pane_id: &PaneId,
    search: &Entity<InputState>,
    name: &Entity<InputState>,
    cx: &mut Context<CompiApp>,
) -> [Subscription; 2] {
    let searched = pane_id.clone();
    let search = cx.subscribe(search, move |this, search, event: &InputEvent, cx| {
        match event {
            InputEvent::Change => {
                let query = search.read(cx).value().trim().to_lowercase();
                if let Some(fields) = this.project_fields_mut(&searched) {
                    fields.query = query;
                }
            }
            InputEvent::Focus => this.set_settings_focus(
                SettingsHost::Pane(searched.clone()),
                Stop::Control {
                    row: PROJECTS_ROW,
                    action: SettingsAction::ProjectSearch,
                },
            ),
            InputEvent::PressEnter { .. } | InputEvent::Blur => return,
        }
        cx.notify();
    });
    let named = pane_id.clone();
    let name = cx.subscribe(name, move |this, _, event: &InputEvent, cx| {
        match event {
            InputEvent::Focus => {
                this.set_settings_focus(
                    SettingsHost::Pane(named.clone()),
                    Stop::Control {
                        row: PROJECTS_ROW,
                        action: SettingsAction::Project(ProjectControl::Name),
                    },
                );
            }
            InputEvent::Blur => this.save_project_name(&named, cx),
            InputEvent::Change | InputEvent::PressEnter { .. } => return,
        }
        cx.notify();
    });
    [search, name]
}

impl CompiApp {
    /// What you set for `project`; nothing set when it has no table.
    pub(in crate::gui) fn project_settings(&self, project: &Project) -> Option<&ProjectSettings> {
        self.config
            .projects
            .get(&self.config.project_settings_key(project))
    }

    /// The name `project` goes by: the one you gave it, else "Home" for Home and the
    /// folder's for the rest.
    pub(in crate::gui) fn project_name(&self, project: &Project) -> String {
        self.project_settings(project)
            .and_then(|settings| settings.name.clone())
            .unwrap_or_else(|| {
                if self.is_home_project(project) {
                    "Home".to_owned()
                } else {
                    project.name().to_owned()
                }
            })
    }

    /// The Home project: the Start folder, holding every tab outside a saved project;
    /// named as it is kept when its settings are saved under another name.
    pub(in crate::gui) fn home_project(&self) -> Project {
        let home = self.config.home_project();
        self.config.kept_project(&home).unwrap_or(home)
    }

    /// Whether `project` is Home.
    pub(in crate::gui) fn is_home_project(&self, project: &Project) -> bool {
        self.config.is_home_project(project)
    }

    /// The listed project a tab in `project` shows under: the saved project for its
    /// folder while not archived, else Home.
    pub(in crate::gui) fn section_project(&self, project: Option<&Project>) -> Project {
        project
            .and_then(|project| self.config.kept_project(project))
            .filter(|project| !self.project_archived(project))
            .unwrap_or_else(|| self.home_project())
    }

    /// Whether `project` is archived and hidden from the main project list.
    pub(super) fn project_archived(&self, project: &Project) -> bool {
        self.project_settings(project)
            .is_some_and(|settings| settings.archived)
    }

    /// Saves an edit from the icon picker or the detail page. Once edited, a project
    /// stays saved even if this edit resets its last customization.
    pub(in crate::gui) fn set_project_settings(
        &mut self,
        project: &Project,
        mut settings: ProjectSettings,
        cx: &mut Context<Self>,
    ) {
        settings.saved = true;
        self.persist_project_settings(project, settings, cx);
    }

    /// Saves (or restores) the project chosen by Add project without launching it.
    pub(super) fn save_added_project(&mut self, project: &Project, cx: &mut Context<Self>) -> bool {
        let mut settings = self.project_settings(project).cloned().unwrap_or_default();
        settings.saved = true;
        settings.archived = false;
        self.persist_project_settings(project, settings, cx)
    }

    pub(super) fn persist_project_settings(
        &mut self,
        project: &Project,
        settings: ProjectSettings,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = self.config.project_settings_key(project);
        if let Err(error) = self.config.save_project_settings(&key, settings.clone()) {
            self.global_error = Some(error);
            return false;
        }
        share_config_change(cx, move |config| {
            if settings == ProjectSettings::default() {
                config.projects.remove(&key);
            } else {
                config.projects.insert(key.clone(), settings.clone());
            }
        });
        true
    }

    /// Changes what you set for `project` with `change`, and saves it.
    fn edit_project(
        &mut self,
        project: &Project,
        change: impl FnOnce(&mut ProjectSettings),
        cx: &mut Context<Self>,
    ) {
        let mut settings = self.project_settings(project).cloned().unwrap_or_default();
        change(&mut settings);
        self.set_project_settings(project, settings, cx);
    }

    /// Gives `project` the name `label`; its folder's name, or nothing, goes back to that.
    fn rename_project(&mut self, project: &Project, label: &str, cx: &mut Context<Self>) {
        let label = label.trim();
        let name = (!label.is_empty() && label != project.name()).then(|| label.to_owned());
        if self
            .project_settings(project)
            .and_then(|settings| settings.name.as_ref())
            != name.as_ref()
        {
            self.edit_project(project, |settings| settings.name = name, cx);
        }
    }

    /// New threads in `project` start on `model` (`None`: its agent's own) with `agent`,
    /// which the project keeps so the model stays with the agent it belongs to.
    pub(super) fn set_project_model(
        &mut self,
        project: &Project,
        agent: ChatHarness,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.edit_project(
            project,
            |settings| {
                settings.agent = settings.agent.or(model.is_some().then_some(agent));
                settings.model = model;
            },
            cx,
        );
    }

    /// The project of the selected tab, where new threads start.
    pub(super) fn selected_project(&self) -> Option<&Project> {
        self.selected_tab()?.project.as_ref()
    }

    /// The agent a new thread in `project` starts with: the project's, else
    /// Settings → General's (`LoadedConfig::new_thread_agent`, as `compi chat` has it).
    pub(super) fn new_thread_agent(&self, project: Option<&Project>) -> ChatHarness {
        self.config.new_thread_agent(project)
    }

    /// The agent a new thread started from the selected tab begins with: its project's.
    pub(super) fn new_tab_agent(&self) -> ChatHarness {
        self.new_thread_agent(Some(&self.new_tab_project()))
    }

    /// What a new session of `harness` in `project` starts on: what Compi remembers of
    /// `harness`, on the project's model when the project's new threads start with
    /// `harness`.
    pub(super) fn new_session_picks(
        &self,
        harness: ChatHarness,
        project: Option<&Project>,
    ) -> ChatPicks {
        let mut picks = self
            .config
            .chat_models
            .get(&harness)
            .map(|models| models.picks.clone())
            .unwrap_or_default();
        if let Some(settings) = project.and_then(|project| self.project_settings(project))
            && let Some(model) = &settings.model
            && settings.agent.unwrap_or(self.config.chat_agent) == harness
        {
            picks.model = Some(model.clone());
        }
        picks
    }

    /// Saved projects, including those with pre-existing customizations.
    pub(super) fn listed_projects(&self) -> Vec<ListedProject> {
        listed_order(&self.config.projects, &self.home_project())
    }

    /// The Projects page's search, as `host` (else the Settings pane with the
    /// highlight) has it.
    fn project_query(&self, host: Option<&SettingsHost>) -> &str {
        let pane = match host {
            Some(SettingsHost::Pane(pane)) => Some(pane),
            _ => None,
        };
        self.page_project_fields(pane)
            .map_or("", |fields| fields.query.as_str())
    }

    /// What the Projects page shows of `listed` for `query`.
    fn page_layout(&self, listed: &[ListedProject], query: &str) -> PageLayout {
        let search = listed.len() > SEARCH_AFTER;
        let query = if search { query } else { "" };
        let mut active = Vec::new();
        let mut archived = Vec::new();
        for (index, listed) in listed.iter().enumerate() {
            if !query.is_empty()
                && !project_matches(
                    query,
                    &self.project_name(&listed.project),
                    &project_place(&listed.project),
                )
            {
                continue;
            }
            if listed.archived {
                archived.push(index);
            } else {
                active.push(index);
            }
        }
        PageLayout {
            search,
            active,
            archived,
            archived_folded: !self.settings.archived_open,
        }
    }

    /// Keyboard order for the project list or its detail, never both at once.
    pub(super) fn project_stops(&self) -> Vec<SettingsAction> {
        use ProjectControl as Control;
        let action = SettingsAction::Project;
        if let Some(project) = &self.settings.project_selected {
            let settings = self.project_settings(project);
            let mut stops = vec![action(Control::Back), action(Control::Name)];
            if settings.is_some_and(|settings| settings.name.is_some()) {
                stops.push(action(Control::ResetName));
            }
            stops.push(action(Control::ChangeIcon));
            if settings.is_some_and(|settings| settings.icon.is_some()) {
                stops.push(action(Control::ResetIcon));
            }
            let removable = if self.is_home_project(project) {
                &[][..]
            } else {
                &[Control::Archive, Control::Remove][..]
            };
            stops.extend(
                [Control::Pin, Control::Agent, Control::Model]
                    .iter()
                    .chain(removable)
                    .copied()
                    .map(action),
            );
            return stops;
        }
        let listed = self.listed_projects();
        let layout = self.page_layout(&listed, self.project_query(None));
        let mut stops = vec![action(Control::Add)];
        if layout.search {
            stops.push(SettingsAction::ProjectSearch);
        }
        let select = |index| action(Control::Select(index));
        stops.extend(layout.active.into_iter().map(select));
        if !layout.archived.is_empty() {
            stops.push(SettingsAction::ArchivedProjects);
            if !layout.archived_folded {
                stops.extend(layout.archived.into_iter().map(select));
            }
        }
        stops
    }

    /// Does what the Projects page's `control` does.
    pub(super) fn activate_project_control(
        &mut self,
        control: ProjectControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ProjectControl as Control;
        match control {
            Control::Add => {
                self.open_add_project(true, window, cx);
                return;
            }
            Control::Select(index) => {
                if let Some(listed) = self.listed_projects().into_iter().nth(index) {
                    self.settings.project_selected = Some(listed.project);
                    self.focus_project_control(Control::Back);
                }
                return;
            }
            Control::Back => {
                if let Some(host) = self.settings_focus_host() {
                    self.back_to_projects(host);
                }
                return;
            }
            Control::Agent => {
                self.step_project_agent(true, true, cx);
                return;
            }
            _ => {}
        }
        let Some(project) = self.settings.project_selected.clone() else {
            return;
        };
        match control {
            Control::Pin => {
                self.edit_project(&project, |settings| settings.pinned = !settings.pinned, cx);
            }
            Control::ResetName => {
                self.edit_project(&project, |settings| settings.name = None, cx);
                self.focus_project_control(Control::ChangeIcon);
            }
            Control::ChangeIcon => self.open_project_icon_picker(project, window, cx),
            Control::ResetIcon => {
                self.edit_project(&project, |settings| settings.icon = None, cx);
                self.focus_project_control(Control::ChangeIcon);
            }
            Control::Model => self.open_project_model_picker(&project, window, cx),
            Control::Archive => {
                let archived = !self.project_archived(&project);
                self.edit_project(&project, |settings| settings.archived = archived, cx);
            }
            Control::Remove => self.ask_remove_project(project, cx),
            _ => {}
        }
    }

    fn focus_project_control(&mut self, control: ProjectControl) {
        if let Some(host) = self.settings_focus_host() {
            self.set_settings_focus(
                host,
                Stop::Control {
                    row: PROJECTS_ROW,
                    action: SettingsAction::Project(control),
                },
            );
        }
    }

    /// Returns to the list and moves the highlight back to the project's row.
    pub(super) fn back_to_projects(&mut self, host: SettingsHost) {
        let Some(project) = self.settings.project_selected.take() else {
            return;
        };
        self.close_project_model_picker();
        let listed = self.listed_projects();
        self.settings.archived_open |= self.project_archived(&project);
        let layout = self.page_layout(&listed, self.project_query(Some(&host)));
        let action = layout
            .active
            .iter()
            .chain(&layout.archived)
            .find_map(|&index| {
                (listed[index].project == project)
                    .then_some(SettingsAction::Project(ProjectControl::Select(index)))
            });
        let action = action.unwrap_or(if layout.search {
            SettingsAction::ProjectSearch
        } else {
            SettingsAction::Project(ProjectControl::Add)
        });
        self.set_settings_focus(
            host,
            Stop::Control {
                row: PROJECTS_ROW,
                action,
            },
        );
    }

    /// New threads' agent from the keyboard: the next choice after the current one,
    /// `forward` or back; Enter (`wrap`) goes round, Left and Right stop at the ends.
    pub(super) fn step_project_agent(&mut self, forward: bool, wrap: bool, cx: &mut Context<Self>) {
        let Some(project) = self.settings.project_selected.as_ref() else {
            return;
        };
        let current = self
            .project_settings(project)
            .and_then(|settings| settings.agent);
        let at = AGENT_CHOICES
            .iter()
            .position(|agent| *agent == current)
            .unwrap_or(0);
        let next = choice_index(at, AGENT_CHOICES.len(), forward, wrap);
        if next != at {
            let project = project.clone();
            self.set_project_agent(&project, AGENT_CHOICES[next], cx);
        }
    }

    /// New threads in `project` start with `agent` (`None`: Settings → General's). A
    /// model picked for another agent goes.
    fn set_project_agent(
        &mut self,
        project: &Project,
        agent: Option<ChatHarness>,
        cx: &mut Context<Self>,
    ) {
        self.close_project_model_picker();
        self.edit_project(
            project,
            |settings| {
                if settings.agent != agent {
                    settings.agent = agent;
                    settings.model = None;
                }
            },
            cx,
        );
    }

    /// Opens Settings → Projects at `project`'s detail: a sidebar project header's
    /// Project settings….
    pub(super) fn open_project_settings(
        &mut self,
        project: &Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stop = Stop::Control {
            row: PROJECTS_ROW,
            action: SettingsAction::Project(ProjectControl::Back),
        };
        self.open_settings(Some(SettingsSection::Projects), Some(stop), window, cx);
        self.settings.project_selected = Some(project.clone());
    }

    /// Keep the name field in sync while it is not being edited.
    pub(super) fn prepare_projects_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self
            .settings
            .project_selected
            .as_ref()
            .filter(|_| self.settings_row_shown(PROJECTS_ROW))
        else {
            self.close_project_model_picker();
            return;
        };
        let name = self.project_name(project);
        let placeholder = project.name().to_owned();
        let selected = self.settings.project_selected.clone();
        let stale: Vec<Entity<InputState>> = self
            .project_fields_each()
            .filter(|fields| !fields.name.focus_handle(cx).is_focused(window))
            .filter_map(|fields| {
                let held = fields.name_for == selected && fields.name.read(cx).value() == name;
                fields.name_for = selected.clone();
                (!held).then(|| fields.name.clone())
            })
            .collect();
        for field in stale {
            field.update(cx, |field, cx| {
                field.set_value(name.clone(), window, cx);
                field.set_placeholder(placeholder.clone(), window, cx);
            });
        }
        self.prepare_project_model_picker(window, cx);
    }

    /// Saves what the name field of pane `pane_id` holds as its project's name.
    fn save_project_name(&mut self, pane_id: &PaneId, cx: &mut Context<Self>) {
        let Some(fields) = self.page_project_fields(Some(pane_id)) else {
            return;
        };
        let label = fields.name.read(cx).value();
        let Some(project) = fields.name_for.clone() else {
            return;
        };
        self.rename_project(&project, &label, cx);
    }

    /// Keys in the Projects page's fields, before the field's own. In the name field:
    /// Enter saves, Escape puts back the saved name (and, with nothing changed, leaves
    /// like Escape elsewhere), Tab, Up and Down save and move on. In the search: Tab
    /// moves on, Down and Enter go to the first project, Escape clears it or leaves.
    pub(super) fn projects_field_key(
        &mut self,
        pane_id: &PaneId,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(fields) = self.page_project_fields(Some(pane_id)) else {
            return false;
        };
        let (name, search) = (fields.name.clone(), fields.search.clone());
        let saved = fields
            .name_for
            .as_ref()
            .map(|project| self.project_name(project));
        let searched = !fields.query.is_empty();
        let shift = key.modifiers.shift;
        if name.focus_handle(cx).is_focused(window) {
            match key.key.as_str() {
                "enter" => self.save_project_name(pane_id, cx),
                "escape" => match saved.filter(|saved| *saved != name.read(cx).value()) {
                    Some(saved) => name.update(cx, |name, cx| name.set_value(saved, window, cx)),
                    None => {
                        self.settings_escape(pane_id);
                        self.apply_settings_focus(pane_id, window, cx);
                    }
                },
                "tab" | "up" | "down" => {
                    self.save_project_name(pane_id, cx);
                    let forward = key.key == "down" || (key.key == "tab" && !shift);
                    self.step_settings_focus(pane_id, forward, window, cx);
                }
                _ => return false,
            }
            return true;
        }
        match key.key.as_str() {
            "tab" => self.step_settings_focus(pane_id, !shift, window, cx),
            "down" | "enter" => {
                let first = self.project_stops().into_iter().find(|stop| {
                    matches!(
                        stop,
                        SettingsAction::Project(ProjectControl::Select(_))
                            | SettingsAction::ArchivedProjects
                    )
                });
                if let Some(action) = first {
                    self.set_settings_focus(
                        SettingsHost::Pane(pane_id.clone()),
                        Stop::Control {
                            row: PROJECTS_ROW,
                            action,
                        },
                    );
                    self.apply_settings_focus(pane_id, window, cx);
                }
            }
            "escape" if searched => {
                search.update(cx, |search, cx| search.set_value("", window, cx));
                if let Some(fields) = self.project_fields_mut(pane_id) {
                    fields.query.clear();
                }
            }
            "escape" => {
                self.settings_escape(pane_id);
                self.apply_settings_focus(pane_id, window, cx);
            }
            _ => return false,
        }
        true
    }

    /// Typing on the Projects page, with no field holding the keyboard, searches its
    /// projects when it has a search; whether it did.
    pub(super) fn type_in_project_search(
        &mut self,
        pane_id: &PaneId,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.settings_pane_shows(pane_id, SettingsSection::Projects)
            || self.settings.project_selected.is_some()
            || self.config.projects.len() <= SEARCH_AFTER
        {
            return false;
        }
        let Some(fields) = self.project_fields_mut(pane_id) else {
            return false;
        };
        let search = fields.search.clone();
        let query = format!("{}{text}", search.read(cx).value());
        search.update(cx, |search, cx| search.set_value(query, window, cx));
        fields.query = search.read(cx).value().trim().to_lowercase();
        self.set_settings_focus(
            SettingsHost::Pane(pane_id.clone()),
            Stop::Control {
                row: PROJECTS_ROW,
                action: SettingsAction::ProjectSearch,
            },
        );
        true
    }

    /// Why `project` cannot be removed now: the tabs open in it, by name.
    fn project_tabs_refusal(&self, project: &Project) -> Option<String> {
        let folder = project_folder_key(project);
        let tabs: Vec<String> = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .flat_map(|session| &session.tabs)
            .filter(|tab| {
                tab.project
                    .as_ref()
                    .is_some_and(|project| project_folder_key(project) == folder)
            })
            .map(|tab| format!("“{}”", self.tab_label(tab)))
            .collect();
        match tabs.len() {
            0 => None,
            1 => Some(format!("A tab is open in it: close {} first", tabs[0])),
            count => Some(format!(
                "{count} tabs are open in it: close {} first",
                tabs.join(", ")
            )),
        }
    }

    /// Remove from Compi…: lists the daemon's threads off the UI thread, then asks,
    /// with how many of them are `project`'s.
    fn ask_remove_project(&mut self, project: Project, cx: &mut Context<Self>) {
        if let Some(reason) = self.project_tabs_refusal(&project) {
            self.global_error = Some(reason);
            return;
        }
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { palette::fetch_threads(&target) })
                .await;
            this.update(cx, |this, cx| {
                match listed {
                    Ok(threads) => {
                        let folder = project_folder_key(&project);
                        let ids = threads
                            .iter()
                            .filter(|thread| {
                                thread
                                    .project
                                    .as_ref()
                                    .is_some_and(|project| project_folder_key(project) == folder)
                            })
                            .map(|thread| thread.chat.id.clone())
                            .collect();
                        this.sidebar_threads_listed(threads);
                        this.open_overlay(
                            Overlay::ConfirmRemoveProject {
                                project,
                                threads: ids,
                            },
                            "",
                        );
                    }
                    Err(error) => {
                        this.global_error =
                            Some(format!("Cannot list the project's threads: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Removes `project` from Compi once confirmed: deletes `threads` off the UI thread,
    /// then forgets what is set for it and its recent entry in every window. A thread
    /// that cannot go keeps the project, and says why.
    pub(super) fn remove_project(
        &mut self,
        project: Project,
        threads: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.project_tabs_refusal(&project) {
            self.global_error = Some(reason);
            return;
        }
        let target = self.target.clone();
        cx.spawn(async move |this, cx| {
            let count = threads.len();
            let (failures, listed) = cx
                .background_executor()
                .spawn(async move {
                    let mut client = match target.connect() {
                        Ok(client) => client,
                        Err(error) => return (vec![error.to_string()], None),
                    };
                    let failures: Vec<String> = threads
                        .iter()
                        .filter_map(|chat| client.delete_thread(chat).err())
                        .map(|error| error.to_string())
                        .collect();
                    (failures, client.list_threads().ok())
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(listed) = listed {
                    this.sidebar_threads_listed(listed);
                }
                match failures.first() {
                    Some(error) => {
                        this.global_error = Some(format!(
                            "{} of {count} threads could not be deleted, so {} stays: {error}",
                            failures.len(),
                            this.project_name(&project)
                        ));
                    }
                    None => this.forget_project(&project, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Forgets `project`: what is set for it, in every window, and each window's
    /// recent entry, recent files and folded header of it.
    fn forget_project(&mut self, project: &Project, cx: &mut Context<Self>) {
        if !self.persist_project_settings(project, ProjectSettings::default(), cx) {
            return;
        }
        let project = project.clone();
        cx.defer(move |cx| {
            for handle in cx.windows() {
                if let Some(typed) = handle.downcast::<CompiApp>() {
                    let _ = typed.update(cx, |app, _, cx| {
                        for fields in app.project_fields_each() {
                            if fields.name_for.as_ref() == Some(&project) {
                                fields.name_for = None;
                            }
                        }
                        if app.settings.project_selected.as_ref() == Some(&project) {
                            if let Some(host @ SettingsHost::Pane(_)) = app.settings_focus_host() {
                                app.back_to_projects(host);
                            } else {
                                app.settings.project_selected = None;
                                app.close_project_model_picker();
                            }
                        }
                        let state = &mut app.state;
                        let files = state.project_files.len();
                        let folded = state.folded_projects.len();
                        state.project_files.retain(|kept| kept.project != project);
                        state
                            .folded_projects
                            .retain(|kept| kept.project.as_ref() != Some(&project));
                        let changed = state.project_history.forget(&project.root)
                            || state.project_files.len() != files
                            || state.folded_projects.len() != folded;
                        if changed {
                            app.save_state();
                        }
                        cx.notify();
                    });
                }
            }
        });
    }

    /// The saved-project list or, after picking a project, its settings page.
    pub(super) fn render_projects_page(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        if let Some(project) = &self.settings.project_selected {
            return self.render_project_detail(row, project, cx);
        }
        let colors = *self.colors();
        let listed = self.listed_projects();
        let add = self.settings_control(
            row,
            SettingsAction::Project(ProjectControl::Add),
            controls::text_button("settings-add-project", "Add project…")
                .header()
                .icon(IconName::Plus),
            cx,
        );
        let heading = div()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x_3()
            .gap_y_2()
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Projects"),
            )
            .child(add);
        let fields = match &row.host {
            SettingsHost::Pane(pane) => self.page_project_fields(Some(pane)),
            SettingsHost::Quick => None,
        };
        let layout = self.page_layout(&listed, fields.map_or("", |fields| fields.query.as_str()));
        let search = fields.filter(|_| layout.search).map(|fields| {
            let focused = self.settings_focused(row, SettingsAction::ProjectSearch);
            div()
                .relative()
                .w_full()
                .max_w(px(360.0))
                .child(
                    Input::new(&fields.search)
                        .appearance(true)
                        .cleanable(true)
                        .with_size(gpui_component::Size::Small),
                )
                .child(self.settings_focus_anchor(focused, cx))
        });
        let list = |members: &[usize]| {
            settings_card(&colors).children(members.iter().enumerate().map(|(at, &index)| {
                div()
                    .min_w_0()
                    .when(at > 0, |line| {
                        line.border_t_1().border_color(color(colors.border))
                    })
                    .child(self.render_project_row(row, index, &listed[index].project, cx))
            }))
        };
        let archived = (!layout.archived.is_empty()).then(|| {
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(self.render_archived_heading(
                    row,
                    layout.archived.len(),
                    layout.archived_folded,
                    cx,
                ))
                .when(!layout.archived_folded, |section| {
                    section.child(list(&layout.archived))
                })
        });
        let empty = layout.active.is_empty().then(|| {
            div()
                .text_color(color(modal_text_color(colors.muted, &colors)))
                .child(if listed.is_empty() {
                    "Add a folder or clone a repository to save your first project."
                } else if layout.archived.is_empty() {
                    "No projects match the search."
                } else {
                    "No active projects."
                })
        });
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_6()
            .child(heading)
            .children(search)
            .children((!layout.active.is_empty()).then(|| list(&layout.active)))
            .children(empty)
            .children(archived)
            .into_any_element()
    }

    /// The Archived heading: its label and count, folding or unfolding its projects.
    fn render_archived_heading(
        &self,
        row: &RowCx,
        count: usize,
        folded: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let action = SettingsAction::ArchivedProjects;
        let focused = self.settings_focused(row, action);
        controls::highlight(
            focused,
            div()
                .id("projects-archived")
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .on_click(self.settings_click(row, action, cx))
                .child(controls::glyph(
                    if folded {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    },
                    muted,
                ))
                .child(settings_group_label(format!("Archived · {count}"), &colors)),
            cx,
        )
        .child(self.settings_focus_anchor(focused, cx))
        .into_any_element()
    }

    /// A compact row opens the project's dedicated detail.
    fn render_project_row(
        &self,
        row: &RowCx,
        index: usize,
        project: &Project,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let settings = self.project_settings(project);
        let select = SettingsAction::Project(ProjectControl::Select(index));
        let focused = self.settings_focused(row, select);
        let name = div()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .font_weight(FontWeight::MEDIUM)
            .child(self.project_name(project));
        let place = div()
            .min_w_0()
            .flex_1()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .line_height(px(UI_SMALL_LINE_HEIGHT))
            .text_color(muted)
            .child(project_place(project).into_owned());
        let text = div()
            .min_w_0()
            .flex_1()
            .flex()
            .when(row.compact, |text| text.flex_col())
            .when(!row.compact, |text| text.items_baseline().gap_2())
            .child(name)
            .child(place);
        let agent = settings.and_then(|settings| settings.agent).map(|agent| {
            div()
                .id(("project-agent", index))
                .flex_none()
                .child(
                    gpui::img(super::chat::harness_mark(agent))
                        .flex_none()
                        .size(px(ROW_MARK_SIZE)),
                )
                .tooltip(controls::tooltip(
                    format!("New threads start with {}", agent.name()),
                    None,
                    None,
                ))
        });
        let hover = blend_rgb(colors.surface, colors.foreground, 0.04);
        div()
            .id(("project-row", index))
            .relative()
            .min_w_0()
            .w_full()
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .hover(move |style| style.bg(color(hover)))
            .on_click(self.settings_click(row, select, cx))
            .child(
                div()
                    .flex_none()
                    .child(self.project_icon(Some(project), ROW_ICON_SIZE)),
            )
            .child(text)
            .when(settings.is_some_and(|settings| settings.pinned), |line| {
                line.child(controls::chip("Pinned", ChipTone::Neutral))
            })
            .children(agent)
            .child(controls::glyph(IconName::ChevronRight, muted))
            .when(focused, |line| {
                line.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_2()
                        .border_color(color(colors.accent)),
                )
            })
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    /// The project's settings, independent of its place in the list.
    fn render_project_detail(
        &self,
        row: &RowCx,
        project: &Project,
        cx: &Context<Self>,
    ) -> AnyElement {
        use ProjectControl as Control;
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let defaults = ProjectSettings::default();
        let settings = self.project_settings(project).unwrap_or(&defaults);
        let action = SettingsAction::Project;
        let reset = |part, id: &'static str, back: String| {
            self.settings_control(
                row,
                action(part),
                controls::text_button(id, "Reset")
                    .header()
                    .ghost()
                    .detail(back),
                cx,
            )
        };
        let buttons = || div().flex().flex_wrap().items_center().gap_2();
        let line = |child: AnyElement| {
            div()
                .min_w_0()
                .px_4()
                .border_t_1()
                .border_color(color(colors.border))
                .child(child)
        };

        let back = self.settings_control(
            row,
            action(Control::Back),
            controls::text_button("projects-back", "Back to projects")
                .header()
                .ghost()
                .icon(IconName::ChevronLeft),
            cx,
        );
        let head = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .child(self.project_icon(Some(project), DETAIL_ICON_SIZE)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(18.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.project_name(project)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .text_color(muted)
                            .child(project_place(project).into_owned()),
                    ),
            );

        // Name: edited in place; Reset back to the folder's name.
        let name_field = match &row.host {
            SettingsHost::Pane(pane) => self.page_project_fields(Some(pane)),
            SettingsHost::Quick => None,
        }
        .map(|fields| {
            let focused = self.settings_focused(row, action(Control::Name));
            div()
                .relative()
                .w(px(220.0))
                .child(
                    Input::new(&fields.name)
                        .appearance(true)
                        .with_size(gpui_component::Size::Small),
                )
                .child(self.settings_focus_anchor(focused, cx))
        });
        let name_detail = match &settings.name {
            Some(_) => format!(
                "Its folder is {}. Enter saves, Escape puts it back.",
                project.name()
            ),
            None => "Its folder's name. Type another; Enter saves, Escape puts it back.".to_owned(),
        };
        let name_controls = buttons()
            .children(name_field)
            .when(settings.name.is_some(), |controls| {
                controls.child(reset(
                    Control::ResetName,
                    "project-reset-name",
                    format!("Back to {}", project.name()),
                ))
            })
            .into_any_element();

        // Icon: what shows now, Change icon…, and Reset back to Automatic.
        let icon_controls = buttons()
            .child(
                div()
                    .flex_none()
                    .mr_1()
                    .child(self.project_icon(Some(project), 24.0)),
            )
            .child(self.settings_control(
                row,
                action(Control::ChangeIcon),
                controls::text_button("project-change-icon", "Change icon…").header(),
                cx,
            ))
            .when(settings.icon.is_some(), |controls| {
                controls.child(reset(
                    Control::ResetIcon,
                    "project-reset-icon",
                    "Back to Automatic".to_owned(),
                ))
            })
            .into_any_element();

        // Pin.
        let pin_action = action(Control::Pin);
        let pin_focused = self.settings_focused(row, pin_action);
        let pin_row = row.clone();
        let pin = controls::highlight(
            pin_focused,
            controls::switch("project-pin", settings.pinned)
                .label("Pin")
                .on_toggle(cx.listener(move |this, _: &bool, window, cx| {
                    this.focus_settings_control(&pin_row, pin_action);
                    this.activate_settings_action(pin_action, window, cx);
                    cx.notify();
                })),
            cx,
        )
        .child(self.settings_focus_anchor(pin_focused, cx))
        .into_any_element();

        // New threads: the agent, as tiles of each agent's mark after Default (General's
        // gear beside the agent it picks now), then its model.
        let default_agent = self.config.chat_agent;
        let tiles = AGENT_CHOICES.map(|agent| Tile {
            label: match agent {
                None => format!("{} ({})", agent_label(None), default_agent.name()).into(),
                Some(agent) => agent_label(Some(agent)).into(),
            },
            preview: match agent {
                None => self
                    .sketch_agent(default_agent)
                    .gap_1p5()
                    .child(controls::glyph(IconName::Settings, muted)),
                Some(agent) => self.sketch_agent(agent),
            },
            picked: settings.agent == agent,
            unavailable: None,
        });
        let picked = project.clone();
        let agent = self.render_tiles(
            row,
            (0, action(Control::Agent)),
            tiles,
            Rc::new(move |this, choice, _, cx| {
                this.set_project_agent(&picked, AGENT_CHOICES[choice], cx);
            }),
            cx,
        );
        let agent_now = settings.agent.unwrap_or(default_agent);
        let agent_detail = match settings.agent {
            Some(agent) => format!("New threads in this project start with {}.", agent.name()),
            None => format!(
                "New threads in this project start with Settings → General's Default agent, now {}.",
                default_agent.name()
            ),
        };
        let model_action = action(Control::Model);
        let model_focused = self.settings_focused(row, model_action);
        let model_row = row.clone();
        let model = controls::highlight(
            model_focused,
            self.render_project_model_button(
                project,
                move |this, _, _| this.focus_settings_control(&model_row, model_action),
                cx,
            ),
            cx,
        )
        .child(self.settings_focus_anchor(model_focused, cx))
        .into_any_element();
        let model_detail = match &settings.model {
            Some(model) => format!("New {} threads here start on {model}.", agent_now.name()),
            None => format!(
                "New {} threads here start on what {} starts on elsewhere.",
                agent_now.name(),
                agent_now.name()
            ),
        };

        // Archive, and Remove from Compi.
        let archive = self.settings_control(
            row,
            action(Control::Archive),
            controls::text_button(
                "project-archive",
                if settings.archived {
                    "Restore"
                } else {
                    "Archive"
                },
            )
            .header()
            .icon(if settings.archived {
                IconName::ArchiveRestore
            } else {
                IconName::Archive
            }),
            cx,
        );
        let remove = self.settings_control(
            row,
            action(Control::Remove),
            controls::text_button("project-remove", "Remove from Compi…")
                .header()
                .danger()
                .icon(IconName::Trash)
                .detail("Forgets its settings and deletes its threads; files on disk stay")
                .disabled_because(self.project_tabs_refusal(project)),
            cx,
        );
        let ending = if settings.archived {
            "This project is hidden from the main list. Its threads and settings are kept."
        } else {
            "Archiving hides the project from the main list but keeps its threads and settings."
        };

        let home = self.is_home_project(project);
        let card = settings_card(&colors)
            .child(div().px_4().child(self.settings_row(
                "Name",
                name_detail,
                name_controls,
                row.compact,
            )))
            .child(line(self.settings_row(
                "Icon",
                icon_detail(settings.icon.as_ref()),
                icon_controls,
                row.compact,
            )))
            .child(line(self.settings_row(
                "Pin",
                "Pinned projects come first in the sidebar and the project list.".into(),
                pin,
                row.compact,
            )))
            .child(line(self.settings_row(
                "New threads' agent",
                agent_detail,
                agent,
                true,
            )))
            .child(line(self.settings_row(
                "New threads' model",
                model_detail,
                model,
                row.compact,
            )))
            // Home holds every tab outside a saved project: it stays.
            .when(!home, |card| {
                card.child(line(self.settings_row(
                    if settings.archived {
                        "Archived"
                    } else {
                        "Archive or remove"
                    },
                    ending.to_owned(),
                    buttons().child(archive).child(remove).into_any_element(),
                    row.compact,
                )))
            });
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().flex().child(back))
            .child(head)
            .child(card)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::project_key;

    fn host(root: &str) -> Project {
        Project {
            environment: ContentEnvironment::Host,
            root: root.into(),
        }
    }

    #[test]
    fn a_project_key_names_its_project_again() {
        let wsl = Project {
            environment: ContentEnvironment::Wsl {
                distribution: "Ubuntu".into(),
            },
            root: "/home/me/compi".into(),
        };
        for project in [host(r"C:\work\site"), host("/work/compi"), wsl] {
            assert_eq!(project_from_key(&project_key(&project)), Some(project));
        }
        assert_eq!(project_from_key("elsewhere:/work"), None);
        assert_eq!(project_from_key("wsl:no-root"), None);
    }

    #[test]
    fn a_folder_is_one_project_by_any_of_its_names() {
        let wsl = |distribution: &str, root: &str| Project {
            environment: ContentEnvironment::Wsl {
                distribution: distribution.into(),
            },
            root: root.into(),
        };
        let saved = project_folder_key(&host(r"C:\Users\me\Proj\compi"));
        for same in [
            wsl("Ubuntu", "/mnt/c/Users/me/Proj/compi"),
            wsl("Debian", "/mnt/c/users/me/proj/compi/"),
            host("c:/Users/me/Proj/compi"),
        ] {
            assert_eq!(project_folder_key(&same), saved, "{same:?}");
        }
        for other in [
            wsl("Ubuntu", "/mnt/d/Users/me/Proj/compi"),
            wsl("Ubuntu", "/home/me/Proj/compi"),
            host(r"C:\Users\me\Proj\compi-old"),
        ] {
            assert_ne!(project_folder_key(&other), saved, "{other:?}");
        }
        // A Windows path into a distribution is that distribution's folder.
        assert_eq!(
            project_folder_key(&host(r"\\wsl.localhost\Ubuntu\home\me\compi")),
            project_folder_key(&wsl("Ubuntu", "/home/me/compi"))
        );
        assert_eq!(
            project_folder_key(&host(r"\\wsl$\Ubuntu\home\me\compi")),
            project_folder_key(&wsl("Ubuntu", "/home/me/compi"))
        );
        // Linux folders keep their case and their distribution.
        assert_ne!(
            project_folder_key(&wsl("Ubuntu", "/home/me/Compi")),
            project_folder_key(&wsl("Ubuntu", "/home/me/compi"))
        );
        assert_ne!(
            project_folder_key(&wsl("Debian", "/home/me/compi")),
            project_folder_key(&wsl("Ubuntu", "/home/me/compi"))
        );
    }

    #[test]
    fn saved_projects_list_pinned_first_and_archived_last() {
        let saved: HashMap<_, _> = [
            (
                "host:/a",
                ProjectSettings {
                    saved: true,
                    ..Default::default()
                },
            ),
            (
                "host:/b",
                ProjectSettings {
                    pinned: true,
                    ..Default::default()
                },
            ),
            (
                "host:/c",
                ProjectSettings {
                    name: Some("Custom".into()),
                    ..Default::default()
                },
            ),
            (
                "host:/d",
                ProjectSettings {
                    pinned: true,
                    archived: true,
                    ..Default::default()
                },
            ),
        ]
        .into_iter()
        .map(|(key, settings)| (key.to_owned(), settings))
        .collect();
        let home = Project {
            environment: ContentEnvironment::Host,
            root: "/home/me".into(),
        };
        let listed = listed_order(&saved, &home);
        let order: Vec<_> = listed
            .iter()
            .map(|listed| (listed.project.root.as_str(), listed.archived))
            .collect();
        // Home is listed unsaved, after the other unpinned projects.
        assert_eq!(
            order,
            [
                ("/b", false),
                ("/a", false),
                ("/c", false),
                ("/home/me", false),
                ("/d", true)
            ]
        );
        // Saved Home is listed once, and never archived.
        let mut saved = saved;
        saved.insert(
            "host:/home/me".into(),
            ProjectSettings {
                pinned: true,
                archived: true,
                ..Default::default()
            },
        );
        let order: Vec<_> = listed_order(&saved, &home)
            .iter()
            .map(|listed| (listed.project.root.clone(), listed.archived))
            .collect();
        assert_eq!(
            order[..2],
            [("/b".into(), false), ("/home/me".into(), false)]
        );
        assert_eq!(order.len(), 5);
    }

    #[test]
    fn the_search_matches_every_word_in_the_name_or_folder() {
        assert!(project_matches("compi", "Compi app", "/home/me/proj/compi"));
        assert!(project_matches(
            "app proj",
            "Compi app",
            "/home/me/proj/compi"
        ));
        assert!(project_matches("ubuntu", "site", "/srv/site · Ubuntu"));
        assert!(!project_matches(
            "app web",
            "Compi app",
            "/home/me/proj/compi"
        ));
        assert!(project_matches("", "anything", "/x"));
    }

    #[test]
    fn removing_asks_with_the_threads_it_deletes() {
        assert_eq!(
            remove_project_message("Compi", 12),
            "Remove Compi from Compi? Compi forgets its name, icon and settings and its \
             place among recent projects. Deletes 12 threads. Files on disk are untouched."
        );
        assert!(remove_project_message("site", 1).contains("Deletes 1 thread."));
        assert!(remove_project_message("site", 0).contains("It has no threads."));
    }

    #[test]
    fn the_agent_choices_lead_with_default() {
        assert_eq!(AGENT_CHOICES[0], None);
        assert!(
            ChatHarness::ALL
                .iter()
                .all(|agent| AGENT_CHOICES.contains(&Some(*agent)))
        );
        assert_eq!(agent_label(None), "Default");
    }
}
