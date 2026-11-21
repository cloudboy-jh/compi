//! What you set for each project (`[projects."<key>"]`): the name it goes by, its icon,
//! whether it is pinned or archived and what new threads in it start with; and
//! Settings → Projects, where you set it. Every place a project shows reads it through
//! these helpers, so a change applies in every window at once.
//!
//! The page lists every project Compi knows (of open tabs, of threads, of settings) as
//! one compact row each: its icon, name and folder, a Pinned chip, the mark of the agent
//! its new threads start with when it picks one, and a pin button. The rows sit under
//! Pinned, Open, Recent and Archived, which stays folded until opened; a search shows
//! once there are more than [`SEARCH_AFTER`]. A row opens its project's detail under
//! it, one at a time, the current tab's project when the page shows: a large icon, the
//! name and folder with Open in new tab and Reveal folder, then the name (edited in
//! place: Enter saves, Escape cancels), the icon, the pin, the agent and model new
//! threads start with, and Archive and Remove from Compi. A sidebar project header's
//! Project settings… opens its project's.
//!
//! Archiving hides a project from the palette's Projects and the page's main list; its
//! threads and settings stay. Removing it forgets its settings and its recent entry
//! and deletes its threads, after asking; never while a tab is open in it.

use super::settings::{
    RowCx, SettingsAction, SettingsHost, Stop, Tile, choice_index, settings_card,
    settings_group_label, share_config_change,
};
use super::*;
use crate::config::{NewTab, ProjectSettings, project_key};
use crate::gui::controls::{ChipTone, IconName};
use crate::gui::open_with;
use compi_protocol::chat::{ChatHarness, ChatPicks};
use compi_protocol::{ContentEnvironment, Project, SettingsSection};
use gpui::Entity;
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
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

/// A control of the Projects page: a part of the row (or detail) of `project`, its
/// place in `CompiApp::listed_projects`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProjectControl {
    pub(super) project: usize,
    pub(super) part: ProjectPart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectPart {
    /// The row: opens its detail, or closes it.
    Select,
    /// The row's pin button.
    RowPin,
    OpenTab,
    Reveal,
    /// The name field: the keyboard goes into it.
    Name,
    ResetName,
    ChangeIcon,
    ResetIcon,
    /// The detail's pin switch.
    Pin,
    /// New threads' agent: Left and Right step through Default and the agents, Enter
    /// moves to the next.
    Agent,
    /// New threads' model: opens the model picker.
    Model,
    /// Archive, or Unarchive.
    Archive,
    Remove,
}

/// Where the page lists a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProjectGroup {
    Pinned,
    /// A tab of it is open.
    Open,
    Recent,
    Archived,
}

impl ProjectGroup {
    const ALL: [Self; 4] = [Self::Pinned, Self::Open, Self::Recent, Self::Archived];

    fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::Open => "Open",
            Self::Recent => "Recent",
            Self::Archived => "Archived",
        }
    }
}

/// A project the Projects page lists, and the group it sits in.
pub(super) struct ListedProject {
    pub(super) project: Project,
    group: ProjectGroup,
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

/// The project `key` names (`project_key`), for a project listed only by its settings.
fn project_from_key(key: &str) -> Option<Project> {
    if let Some(root) = key.strip_prefix("host:") {
        return Some(Project {
            environment: ContentEnvironment::Host,
            root: root.to_owned(),
        });
    }
    let (distribution, root) = key.strip_prefix("wsl:")?.split_once(':')?;
    Some(Project {
        environment: ContentEnvironment::Wsl {
            distribution: distribution.to_owned(),
        },
        root: root.to_owned(),
    })
}

/// The projects `open` (of open tabs) then `recent` (of threads, then of settings), each
/// once, by group: archived ones (`flags` says `(pinned, archived)`) last, pinned ones
/// first, then those with a tab open; in each group in the order given.
fn listed_order(
    open: impl IntoIterator<Item = Project>,
    recent: impl IntoIterator<Item = Project>,
    flags: impl Fn(&Project) -> (bool, bool),
) -> Vec<ListedProject> {
    let mut listed: Vec<ListedProject> = Vec::new();
    for (project, open) in open
        .into_iter()
        .map(|project| (project, true))
        .chain(recent.into_iter().map(|project| (project, false)))
    {
        if listed.iter().any(|known| known.project == project) {
            continue;
        }
        let group = match flags(&project) {
            (_, true) => ProjectGroup::Archived,
            (true, false) => ProjectGroup::Pinned,
            (false, false) if open => ProjectGroup::Open,
            (false, false) => ProjectGroup::Recent,
        };
        listed.push(ListedProject { project, group });
    }
    // Stable: in each group as given.
    listed.sort_by_key(|listed| listed.group);
    listed
}

/// Whether every word of `query` (lowercase) is in the project's `name` or `place`.
fn project_matches(query: &str, name: &str, place: &str) -> bool {
    let (name, place) = (name.to_lowercase(), place.to_lowercase());
    query
        .split_whitespace()
        .all(|word| name.contains(word) || place.contains(word))
}

/// Where a project's row says it is: its folder, and the WSL distribution it is in.
fn project_place(project: &Project) -> String {
    match &project.environment {
        ContentEnvironment::Host => project.root.clone(),
        ContentEnvironment::Wsl { distribution } => format!("{} · {distribution}", project.root),
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

/// What the page shows of the projects listed: the search when there are enough of
/// them, then each group with a project the search matches, its projects by their place
/// in the list; the archived ones only while their group is unfolded or searched.
struct PageLayout {
    search: bool,
    groups: Vec<(ProjectGroup, Vec<usize>)>,
    /// The Archived group shows its heading only.
    archived_folded: bool,
}

/// The Projects page's text fields in one Settings pane: its search and the name
/// field of the project whose detail is open.
pub(super) struct ProjectFields {
    search: Entity<InputState>,
    /// The search, trimmed and lowercased.
    query: String,
    name: Entity<InputState>,
    /// The project whose name the name field holds, by `project_key`.
    name_for: Option<String>,
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
                let Some(index) = this.selected_project_index() else {
                    return;
                };
                this.set_settings_focus(
                    SettingsHost::Pane(named.clone()),
                    Stop::Control {
                        row: PROJECTS_ROW,
                        action: SettingsAction::Project(ProjectControl {
                            project: index,
                            part: ProjectPart::Name,
                        }),
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
        self.config.projects.get(&project_key(project))
    }

    /// The name `project` goes by: the one you gave it, else its folder's.
    pub(in crate::gui) fn project_name(&self, project: &Project) -> String {
        self.project_settings(project)
            .and_then(|settings| settings.name.clone())
            .unwrap_or_else(|| project.name().to_owned())
    }

    /// Whether `project` is pinned, so it comes first among projects.
    pub(in crate::gui) fn project_pinned(&self, project: &Project) -> bool {
        self.project_settings(project)
            .is_some_and(|settings| settings.pinned)
    }

    /// Whether `project` is archived, out of the palette's Projects and the page's main
    /// list.
    fn project_archived(&self, project: &Project) -> bool {
        self.project_settings(project)
            .is_some_and(|settings| settings.archived)
    }

    /// Saves what you set for `project` and gives it to every window; nothing set removes
    /// its table.
    pub(in crate::gui) fn set_project_settings(
        &mut self,
        project: &Project,
        settings: ProjectSettings,
        cx: &mut Context<Self>,
    ) {
        let key = project_key(project);
        if let Err(error) = self.config.save_project_settings(&key, settings.clone()) {
            self.global_error = Some(error);
            return;
        }
        share_config_change(cx, move |config| {
            if settings == ProjectSettings::default() {
                config.projects.remove(&key);
            } else {
                config.projects.insert(key.clone(), settings.clone());
            }
        });
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

    /// The projects Settings → Projects lists, by group (`listed_order`): of open tabs,
    /// then of threads newest first, then any other project something is set for.
    pub(super) fn listed_projects(&self) -> Vec<ListedProject> {
        let open: Vec<Project> = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .flat_map(|session| &session.tabs)
            .filter_map(|tab| tab.project.clone())
            .collect();
        let mut configured: Vec<(&String, Project)> = self
            .config
            .projects
            .keys()
            .filter_map(|key| Some((key, project_from_key(key)?)))
            .collect();
        configured.sort_by(|a, b| a.0.cmp(b.0));
        let recent = self
            .thread_projects()
            .cloned()
            .chain(configured.into_iter().map(|(_, project)| project));
        listed_order(open, recent, |project| {
            (self.project_pinned(project), self.project_archived(project))
        })
    }

    /// The project the Projects page lists at `index`.
    fn listed_project(&self, index: usize) -> Option<Project> {
        self.listed_projects()
            .into_iter()
            .nth(index)
            .map(|listed| listed.project)
    }

    /// Where the Projects page lists the selected project.
    fn selected_project_index(&self) -> Option<usize> {
        let selected = self.settings.project_selected.as_deref()?;
        self.listed_projects()
            .iter()
            .position(|listed| project_key(&listed.project) == selected)
    }

    /// Whether `project`'s detail is open on the Projects page.
    fn project_selected(&self, project: &Project) -> bool {
        self.settings.project_selected.as_deref() == Some(project_key(project).as_str())
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
        let groups = ProjectGroup::ALL
            .into_iter()
            .filter_map(|group| {
                let members: Vec<usize> = listed
                    .iter()
                    .enumerate()
                    .filter(|(_, listed)| listed.group == group)
                    .filter(|(_, listed)| {
                        query.is_empty()
                            || project_matches(
                                query,
                                &self.project_name(&listed.project),
                                &project_place(&listed.project),
                            )
                    })
                    .map(|(index, _)| index)
                    .collect();
                (!members.is_empty()).then_some((group, members))
            })
            .collect();
        PageLayout {
            search,
            groups,
            archived_folded: !self.settings.archived_open && query.is_empty(),
        }
    }

    /// The Projects page's keyboard order: the search, then each row and its pin
    /// button, the open detail's controls after its row; the Archived heading before
    /// the archived rows.
    pub(super) fn project_stops(&self) -> Vec<SettingsAction> {
        let listed = self.listed_projects();
        let layout = self.page_layout(&listed, self.project_query(None));
        let mut stops = Vec::new();
        if layout.search {
            stops.push(SettingsAction::ProjectSearch);
        }
        for (group, members) in &layout.groups {
            if *group == ProjectGroup::Archived {
                stops.push(SettingsAction::ArchivedProjects);
                if layout.archived_folded {
                    continue;
                }
            }
            for &index in members {
                let stop = |part| {
                    SettingsAction::Project(ProjectControl {
                        project: index,
                        part,
                    })
                };
                stops.extend([stop(ProjectPart::Select), stop(ProjectPart::RowPin)]);
                let project = &listed[index].project;
                if !self.project_selected(project) {
                    continue;
                }
                let settings = self.project_settings(project).cloned().unwrap_or_default();
                stops.extend([
                    stop(ProjectPart::OpenTab),
                    stop(ProjectPart::Reveal),
                    stop(ProjectPart::Name),
                ]);
                if settings.name.is_some() {
                    stops.push(stop(ProjectPart::ResetName));
                }
                stops.push(stop(ProjectPart::ChangeIcon));
                if settings.icon.is_some() {
                    stops.push(stop(ProjectPart::ResetIcon));
                }
                stops.extend([
                    stop(ProjectPart::Pin),
                    stop(ProjectPart::Agent),
                    stop(ProjectPart::Model),
                    stop(ProjectPart::Archive),
                    stop(ProjectPart::Remove),
                ]);
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
        let Some(project) = self.listed_project(control.project) else {
            return;
        };
        match control.part {
            ProjectPart::Select => {
                let key = project_key(&project);
                self.settings.project_selected =
                    (self.settings.project_selected.as_ref() != Some(&key)).then_some(key);
                self.close_project_model_picker();
            }
            ProjectPart::RowPin | ProjectPart::Pin => {
                self.edit_project(&project, |settings| settings.pinned = !settings.pinned, cx);
                self.follow_moved_project(&project, control.part);
            }
            ProjectPart::OpenTab => self.open_project_tab(&project),
            ProjectPart::Reveal => self.reveal_project(&project, cx),
            // The name field takes the keyboard (`settings_keyboard`).
            ProjectPart::Name => {}
            ProjectPart::ResetName => {
                self.edit_project(&project, |settings| settings.name = None, cx)
            }
            ProjectPart::ChangeIcon => self.open_project_icon_picker(project, window, cx),
            ProjectPart::ResetIcon => {
                self.edit_project(&project, |settings| settings.icon = None, cx)
            }
            ProjectPart::Agent => self.step_project_agent(control, true, true, cx),
            ProjectPart::Model => self.open_project_model_picker(&project, window, cx),
            ProjectPart::Archive => {
                let archived = !self.project_archived(&project);
                self.edit_project(&project, |settings| settings.archived = archived, cx);
                // An archived project stays in view, under Archived.
                self.settings.archived_open |= archived;
                self.follow_moved_project(&project, control.part);
            }
            ProjectPart::Remove => self.ask_remove_project(project, cx),
        }
    }

    /// The highlight follows `project` to its new place after pinning or archiving
    /// moved it to another group.
    fn follow_moved_project(&mut self, project: &Project, part: ProjectPart) {
        let Some(index) = self
            .listed_projects()
            .iter()
            .position(|listed| listed.project == *project)
        else {
            return;
        };
        let Some(host) = self.settings_focus_host() else {
            return;
        };
        self.set_settings_focus(
            host,
            Stop::Control {
                row: PROJECTS_ROW,
                action: SettingsAction::Project(ProjectControl {
                    project: index,
                    part,
                }),
            },
        );
    }

    /// Open in new tab: a tab at `project`'s root in the selected workspace, a thread
    /// or a terminal as Settings → General's New tabs says.
    fn open_project_tab(&mut self, project: &Project) {
        let Some(session) = self.state.selected_session.clone() else {
            self.global_error = Some("Open a workspace first".into());
            return;
        };
        match self.config.new_tab.unwrap_or_default() {
            NewTab::Chat => self.new_project_thread(session, project),
            NewTab::Terminal => self.new_project_terminal(session, project),
        }
    }

    /// Why Reveal folder cannot show `project`'s folder: a window on another computer,
    /// or a folder this computer cannot reach.
    fn reveal_refusal(&self, project: &Project) -> Option<&'static str> {
        if self.target.is_remote() {
            Some("Its folder is on the computer this window connects to")
        } else if open_with::native_path(&project.environment, &project.root).is_none() {
            Some("Its folder is not on this computer's file system")
        } else {
            None
        }
    }

    /// Reveal folder: `project`'s folder in File Explorer (Finder on macOS).
    fn reveal_project(&mut self, project: &Project, cx: &mut Context<Self>) {
        if let Some(reason) = self.reveal_refusal(project) {
            self.global_error = Some(reason.into());
            return;
        }
        if let Some(path) = open_with::native_path(&project.environment, &project.root) {
            self.open_natively(path, open_with::Action::Reveal, cx);
        }
    }

    /// New threads' agent from the keyboard: the next choice after the current one,
    /// `forward` or back; Enter (`wrap`) goes round, Left and Right stop at the ends.
    pub(super) fn step_project_agent(
        &mut self,
        control: ProjectControl,
        forward: bool,
        wrap: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.listed_project(control.project) else {
            return;
        };
        let current = self
            .project_settings(&project)
            .and_then(|settings| settings.agent);
        let at = AGENT_CHOICES
            .iter()
            .position(|agent| *agent == current)
            .unwrap_or(0);
        let next = choice_index(at, AGENT_CHOICES.len(), forward, wrap);
        self.set_project_agent(&project, AGENT_CHOICES[next], cx);
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
        self.settings.project_selected = Some(project_key(project));
        // Shown now, the page keeps this selection rather than the current tab's.
        self.settings.projects_shown = true;
        if self.project_archived(project) {
            self.settings.archived_open = true;
        }
        let stop = self
            .listed_projects()
            .iter()
            .position(|listed| listed.project == *project)
            .map(|index| Stop::Control {
                row: PROJECTS_ROW,
                action: SettingsAction::Project(ProjectControl {
                    project: index,
                    part: ProjectPart::Select,
                }),
            });
        self.open_settings(Some(SettingsSection::Projects), stop, window, cx);
    }

    /// Before drawing: showing the page anew selects the current tab's project; each
    /// name field holds the selected project's name unless it is being edited; the model
    /// picker closes with the page.
    pub(super) fn prepare_projects_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.settings_row_shown(PROJECTS_ROW);
        if shown
            && !self.settings.projects_shown
            && let Some(project) = self.selected_project()
        {
            self.settings.project_selected = Some(project_key(project));
        }
        self.settings.projects_shown = shown;
        if !shown {
            self.close_project_model_picker();
            return;
        }
        let selected = self.settings.project_selected.clone();
        let project = selected.as_deref().and_then(project_from_key);
        let name = project
            .as_ref()
            .map(|project| self.project_name(project))
            .unwrap_or_default();
        let placeholder = project
            .as_ref()
            .map(|project| project.name().to_owned())
            .unwrap_or_default();
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
        let Some(project) = fields.name_for.as_deref().and_then(project_from_key) else {
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
            .as_deref()
            .and_then(project_from_key)
            .map(|project| self.project_name(&project));
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
                        SettingsAction::Project(ProjectControl {
                            part: ProjectPart::Select,
                            ..
                        })
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
            || self.listed_projects().len() <= SEARCH_AFTER
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
        let tabs: Vec<String> = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .flat_map(|session| &session.tabs)
            .filter(|tab| tab.project.as_ref() == Some(project))
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
                        let ids = threads
                            .iter()
                            .filter(|thread| thread.project.as_ref() == Some(&project))
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
        if self.project_selected(project) {
            self.settings.project_selected = None;
        }
        self.close_project_model_picker();
        self.set_project_settings(project, ProjectSettings::default(), cx);
        let project = project.clone();
        cx.defer(move |cx| {
            for handle in cx.windows() {
                if let Some(typed) = handle.downcast::<CompiApp>() {
                    let _ = typed.update(cx, |app, _, cx| {
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

    /// Settings → Projects: its search once there are enough projects, then the
    /// projects in groups, each a compact row, the selected one's detail under it.
    pub(super) fn render_projects_page(&self, row: &RowCx, cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let listed = self.listed_projects();
        if listed.is_empty() {
            return settings_card(&colors)
                .px_4()
                .child(
                    self.settings_row(
                        "No projects yet",
                        "A project is the folder a tab or thread works in: its Git repository's \
                     top folder, else the folder itself. Projects show here once you work in one."
                            .into(),
                        div().into_any_element(),
                        row.compact,
                    ),
                )
                .into_any_element();
        }
        let fields = match &row.host {
            SettingsHost::Pane(pane) => self.page_project_fields(Some(pane)),
            SettingsHost::Quick => None,
        };
        let layout = self.page_layout(
            &listed,
            fields.map_or("", |fields| fields.query.as_str()),
        );
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
        let groups = layout.groups.iter().map(|(group, members)| {
            let folded = *group == ProjectGroup::Archived && layout.archived_folded;
            let heading = if *group == ProjectGroup::Archived {
                self.render_archived_heading(row, members.len(), folded, cx)
            } else {
                settings_group_label(group.label(), &colors).into_any_element()
            };
            let card = (!folded).then(|| {
                settings_card(&colors).children(members.iter().enumerate().map(
                    |(at, &index)| {
                        let project = &listed[index].project;
                        let selected = self.project_selected(project);
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .when(at > 0, |line| {
                                line.border_t_1().border_color(color(colors.border))
                            })
                            .child(self.render_project_row(row, index, project, selected, cx))
                            .when(selected, |line| {
                                line.child(self.render_project_detail(row, index, project, cx))
                            })
                    },
                ))
            });
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(heading)
                .children(card)
        });
        let empty = (layout.groups.is_empty()).then(|| {
            div()
                .text_color(color(modal_text_color(colors.muted, &colors)))
                .child("No project matches the search.")
        });
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_6()
            .children(search)
            .children(groups)
            .children(empty)
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

    /// One project's row: its icon, name and folder (stacked when `compact`), a Pinned
    /// chip, the mark of the agent its new threads start with when it picks one, the
    /// pin button and the chevron of its detail. A click opens or closes its detail.
    fn render_project_row(
        &self,
        row: &RowCx,
        index: usize,
        project: &Project,
        selected: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let settings = self.project_settings(project).cloned().unwrap_or_default();
        let action = |part| {
            SettingsAction::Project(ProjectControl {
                project: index,
                part,
            })
        };
        let select = action(ProjectPart::Select);
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
            .child(project_place(project));
        let text = div()
            .min_w_0()
            .flex_1()
            .flex()
            .when(row.compact, |text| text.flex_col())
            .when(!row.compact, |text| text.items_baseline().gap_2())
            .child(name)
            .child(place);
        let agent = settings.agent.map(|agent| {
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
        let pin = self.settings_control(
            row,
            action(ProjectPart::RowPin),
            controls::icon_button(
                ("project-row-pin", index),
                if settings.pinned {
                    IconName::PinOff
                } else {
                    IconName::Pin
                },
                if settings.pinned { "Unpin" } else { "Pin" },
            )
            .detail("Pinned projects come first in the sidebar, the palette and here"),
            cx,
        );
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
            .when(selected, |line| {
                line.bg(color(blend_rgb(colors.surface, colors.foreground, 0.06)))
            })
            .hover(move |style| style.bg(color(hover)))
            .on_click(self.settings_click(row, select, cx))
            .child(
                div()
                    .flex_none()
                    .child(self.project_icon(Some(project), ROW_ICON_SIZE)),
            )
            .child(text)
            .when(settings.pinned && !row.compact, |line| {
                line.child(controls::chip("Pinned", ChipTone::Neutral))
            })
            .children(agent)
            .child(pin)
            .child(controls::glyph(
                if selected {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                },
                muted,
            ))
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

    /// The selected project's detail, under its row: a large icon, its name and folder
    /// with Open in new tab and Reveal folder, then its Name, Icon, Pin and New threads
    /// rows, and Archive and Remove from Compi.
    fn render_project_detail(
        &self,
        row: &RowCx,
        index: usize,
        project: &Project,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let settings = self.project_settings(project).cloned().unwrap_or_default();
        let action = |part| {
            SettingsAction::Project(ProjectControl {
                project: index,
                part,
            })
        };
        let button = |part, id: &'static str, label: &'static str| {
            self.settings_control(
                row,
                action(part),
                controls::text_button((id, index), label).header(),
                cx,
            )
        };
        let reset = |part, id: &'static str, back: String| {
            self.settings_control(
                row,
                action(part),
                controls::text_button((id, index), "Reset")
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
                .border_t_1()
                .border_color(color(colors.border))
                .child(child)
        };

        // The head: the icon, name and folder, and where to go from here.
        let reveal = self.settings_control(
            row,
            action(ProjectPart::Reveal),
            controls::text_button(("project-reveal", index), "Reveal folder")
                .header()
                .icon(IconName::FolderOpen)
                .detail(open_with::REVEAL_LABEL)
                .disabled_because(self.reveal_refusal(project)),
            cx,
        );
        let open = self.settings_control(
            row,
            action(ProjectPart::OpenTab),
            controls::text_button(("project-open-tab", index), "Open in new tab")
                .header()
                .icon(IconName::Plus)
                .detail(match self.config.new_tab.unwrap_or_default() {
                    NewTab::Chat => "A new thread at its folder, as New tabs is Chat first",
                    NewTab::Terminal => "A terminal at its folder, as New tabs is Terminal first",
                }),
            cx,
        );
        let head = div()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_2()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
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
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(16.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.project_name(project)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(UI_SMALL_TEXT_SIZE))
                                    .text_color(muted)
                                    .child(project_place(project)),
                            ),
                    ),
            )
            .child(buttons().flex_none().child(open).child(reveal));

        // Name: edited in place; Reset back to the folder's name.
        let name_field = match &row.host {
            SettingsHost::Pane(pane) => self.page_project_fields(Some(pane)),
            SettingsHost::Quick => None,
        }
        .map(|fields| {
            let focused = self.settings_focused(row, action(ProjectPart::Name));
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
            .when(settings.name.is_some(), |controls| {
                controls.child(reset(
                    ProjectPart::ResetName,
                    "project-reset-name",
                    format!("Back to {}", project.name()),
                ))
            })
            .children(name_field)
            .into_any_element();

        // Icon: what shows now, Reset back to Automatic, and Change icon….
        let icon_controls = buttons()
            .child(
                div()
                    .flex_none()
                    .mr_1()
                    .child(self.project_icon(Some(project), 24.0)),
            )
            .when(settings.icon.is_some(), |controls| {
                controls.child(reset(
                    ProjectPart::ResetIcon,
                    "project-reset-icon",
                    "Back to Automatic".to_owned(),
                ))
            })
            .child(button(
                ProjectPart::ChangeIcon,
                "project-change-icon",
                "Change icon…",
            ))
            .into_any_element();

        // Pin.
        let pin_action = action(ProjectPart::Pin);
        let pin_focused = self.settings_focused(row, pin_action);
        let pin_row = row.clone();
        let pin = controls::highlight(
            pin_focused,
            controls::switch(("project-pin", index), settings.pinned)
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
            (index, action(ProjectPart::Agent)),
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
        let model_action = action(ProjectPart::Model);
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
            action(ProjectPart::Archive),
            controls::text_button(
                ("project-archive", index),
                if settings.archived {
                    "Unarchive"
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
            action(ProjectPart::Remove),
            controls::text_button(("project-remove", index), "Remove from Compi…")
                .header()
                .danger()
                .icon(IconName::Trash)
                .detail("Forgets its settings and deletes its threads; files on disk stay")
                .disabled_because(self.project_tabs_refusal(project)),
            cx,
        );
        let ending = if settings.archived {
            "Archived: out of the palette's Projects and the list above. Its threads and \
             settings stay. Removing it deletes its threads too; its files stay."
        } else {
            "Archiving hides it from the palette's Projects and the list above; its threads \
             and settings stay. Removing it deletes its threads too; its files stay."
        };

        div()
            .min_w_0()
            .px_4()
            .pt_4()
            .pb_2()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(color(colors.border))
            .bg(color(blend_rgb(colors.surface, colors.foreground, 0.02)))
            .child(div().pb_3().child(head))
            .child(line(self.settings_row(
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
            .child(line(
                self.settings_row(
                    "Pin",
                    "Pinned projects come first in the sidebar's project headers, the palette \
                     and this list."
                        .to_owned(),
                    pin,
                    row.compact,
                ),
            ))
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
            .child(line(self.settings_row(
                if settings.archived {
                    "Archived"
                } else {
                    "Archive or remove"
                },
                ending.to_owned(),
                buttons().child(archive).child(remove).into_any_element(),
                row.compact,
            )))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn projects_list_once_each_as_pinned_open_recent_then_archived() {
        let (a, b, c, d, e) = (host("/a"), host("/b"), host("/c"), host("/d"), host("/e"));
        let listed = listed_order(
            [a.clone(), b.clone(), a.clone(), e.clone()],
            [c.clone(), b.clone(), d.clone()],
            |project| match project.root.as_str() {
                "/b" | "/d" => (true, false),
                // Archived wins over pinned and open.
                "/e" => (true, true),
                _ => (false, false),
            },
        );
        let order: Vec<(&str, ProjectGroup)> = listed
            .iter()
            .map(|listed| (listed.project.root.as_str(), listed.group))
            .collect();
        assert_eq!(
            order,
            [
                ("/b", ProjectGroup::Pinned),
                ("/d", ProjectGroup::Pinned),
                ("/a", ProjectGroup::Open),
                ("/c", ProjectGroup::Recent),
                ("/e", ProjectGroup::Archived),
            ]
        );
    }

    #[test]
    fn the_search_matches_every_word_in_the_name_or_folder() {
        assert!(project_matches("compi", "Compi app", "/home/me/proj/compi"));
        assert!(project_matches("app proj", "Compi app", "/home/me/proj/compi"));
        assert!(project_matches("ubuntu", "site", "/srv/site · Ubuntu"));
        assert!(!project_matches("app web", "Compi app", "/home/me/proj/compi"));
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
