//! What you set for each project (`[projects."<key>"]`): the name it goes by, its icon,
//! whether it is pinned and what new threads in it start with; and Settings → Projects,
//! where you set it. Every place a project shows reads it through these helpers, so a
//! change applies in every window at once.
//!
//! The page lists the projects of open tabs first, then those of recent threads, then
//! any other project something is set for, pinned ones first in each. Each project is a
//! card: its icon, name and folder, opening to its rows (Name, Icon, Pin, and the agent
//! and model new threads start with). One card is open at a time; a sidebar project
//! header's Project settings… opens its project's.
use super::settings::{
    RowCx, SettingsAction, Stop, settings_card, settings_group_label, share_config_change,
};
use super::*;
use crate::config::{ProjectIcon, ProjectSettings, project_key};
use crate::gui::controls::IconName;
use compi_protocol::chat::{ChatHarness, ChatPicks};
use compi_protocol::{ContentEnvironment, Project, SettingsSection};

/// The Projects page, one row drawn as the page.
pub(super) const PROJECTS_ROW: &str = "projects";
/// The size of the icon on a project's card.
const CARD_ICON_SIZE: f32 = 24.0;

/// A control of the Projects page: a part of the card of `project`, its place in
/// `CompiApp::listed_projects`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProjectControl {
    pub(super) project: usize,
    pub(super) part: ProjectPart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectPart {
    /// The card's head: opens or closes its rows.
    Open,
    Rename,
    ResetName,
    ResetIcon,
    ChooseIcon,
    ChooseFile,
    Pin,
    /// New threads' agent: Left and Right step through Default and the agents, Enter
    /// moves to the next.
    Agent,
    /// New threads' model: opens its list.
    Model,
}

/// A project the Projects page lists, and whether a tab of it is open.
pub(super) struct ListedProject {
    pub(super) project: Project,
    open: bool,
}

/// New threads' agent choices: Default (Settings → Chat's), then each agent.
const AGENT_CHOICES: [Option<ChatHarness>; 3] =
    [None, Some(ChatHarness::Omp), Some(ChatHarness::Claude)];

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
/// once, pinned ones first in each group and otherwise in the order given.
fn listed_order(
    open: impl IntoIterator<Item = Project>,
    recent: impl IntoIterator<Item = Project>,
    pinned: impl Fn(&Project) -> bool,
) -> Vec<ListedProject> {
    let mut listed: Vec<ListedProject> = Vec::new();
    for (project, open) in open
        .into_iter()
        .map(|project| (project, true))
        .chain(recent.into_iter().map(|project| (project, false)))
    {
        if !listed.iter().any(|known| known.project == project) {
            listed.push(ListedProject { project, open });
        }
    }
    // Stable: open before recent, pinned before not, else as given.
    listed.sort_by_key(|listed| (!listed.open, !pinned(&listed.project)));
    listed
}

/// Where a project's card says it is: its folder, and the WSL distribution it is in.
fn project_place(project: &Project) -> String {
    match &project.environment {
        ContentEnvironment::Host => project.root.clone(),
        ContentEnvironment::Wsl { distribution } => format!("{} · {distribution}", project.root),
    }
}

/// What the Icon row says of `icon`.
fn icon_detail(icon: Option<&ProjectIcon>) -> String {
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

    /// Gives `project` the name `label` from Rename project; its folder's name, or
    /// nothing, goes back to that.
    pub(super) fn rename_project(
        &mut self,
        project: &Project,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        let label = label.trim();
        let name = (!label.is_empty() && label != project.name()).then(|| label.to_owned());
        self.edit_project(project, |settings| settings.name = name, cx);
    }

    /// The project of the selected tab, where new threads start.
    pub(super) fn selected_project(&self) -> Option<&Project> {
        self.selected_tab()?.project.as_ref()
    }

    /// The agent a new thread in `project` starts with: the project's, else
    /// Settings → Chat's (`LoadedConfig::new_thread_agent`, as `compi chat` has it).
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

    /// The projects Settings → Projects lists: of open tabs, then of threads newest
    /// first, then any other project something is set for; pinned first in each.
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
        listed_order(open, recent, |project| self.project_pinned(project))
    }

    /// The project the Projects page lists at `index`.
    fn listed_project(&self, index: usize) -> Option<Project> {
        self.listed_projects()
            .into_iter()
            .nth(index)
            .map(|listed| listed.project)
    }

    /// Whether the card of `project` is open on the Projects page.
    fn project_card_open(&self, project: &Project) -> bool {
        self.settings.project_open.as_deref() == Some(project_key(project).as_str())
    }

    /// The Projects page's keyboard order: each card's head, and the open card's
    /// controls in the order they show.
    pub(super) fn project_stops(&self) -> Vec<SettingsAction> {
        let mut stops = Vec::new();
        for (index, listed) in self.listed_projects().iter().enumerate() {
            let stop = |part| {
                SettingsAction::Project(ProjectControl {
                    project: index,
                    part,
                })
            };
            stops.push(stop(ProjectPart::Open));
            if !self.project_card_open(&listed.project) {
                continue;
            }
            let settings = self
                .project_settings(&listed.project)
                .cloned()
                .unwrap_or_default();
            if settings.name.is_some() {
                stops.push(stop(ProjectPart::ResetName));
            }
            stops.push(stop(ProjectPart::Rename));
            if settings.icon.is_some() {
                stops.push(stop(ProjectPart::ResetIcon));
            }
            stops.extend([
                stop(ProjectPart::ChooseIcon),
                stop(ProjectPart::ChooseFile),
                stop(ProjectPart::Pin),
                stop(ProjectPart::Agent),
                stop(ProjectPart::Model),
            ]);
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
            ProjectPart::Open => {
                let key = project_key(&project);
                self.settings.project_open =
                    (self.settings.project_open.as_ref() != Some(&key)).then_some(key);
                if self.popup == Some(Popup::ProjectModel) {
                    self.popup = None;
                }
            }
            ProjectPart::Rename => {
                let name = self.project_name(&project);
                self.open_overlay(
                    Overlay::Text {
                        purpose: TextPurpose::RenameProject(project),
                        title: "Rename project",
                    },
                    &name,
                );
            }
            ProjectPart::ResetName => {
                self.edit_project(&project, |settings| settings.name = None, cx)
            }
            ProjectPart::ResetIcon => {
                self.edit_project(&project, |settings| settings.icon = None, cx)
            }
            ProjectPart::ChooseIcon => self.open_project_icon_picker(project, window, cx),
            ProjectPart::ChooseFile => self.choose_project_icon_file(project, window, cx),
            ProjectPart::Pin => {
                self.edit_project(&project, |settings| settings.pinned = !settings.pinned, cx);
            }
            ProjectPart::Agent => self.step_project_agent(control, true, true, cx),
            ProjectPart::Model => self.popup = Some(Popup::ProjectModel),
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
        let last = AGENT_CHOICES.len() - 1;
        let next = match (forward, wrap) {
            (true, true) => (at + 1) % AGENT_CHOICES.len(),
            (true, false) => (at + 1).min(last),
            (false, true) => (at + last) % AGENT_CHOICES.len(),
            (false, false) => at.saturating_sub(1),
        };
        self.set_project_agent(&project, AGENT_CHOICES[next], cx);
    }

    /// New threads in `project` start with `agent` (`None`: Settings → Chat's). A
    /// model picked for another agent goes.
    fn set_project_agent(
        &mut self,
        project: &Project,
        agent: Option<ChatHarness>,
        cx: &mut Context<Self>,
    ) {
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

    /// The models New threads offers for `agent`: its own default (`None`), then your
    /// favorites, your recent picks and what it starts on now, then `current`.
    fn project_model_choices(
        &self,
        agent: ChatHarness,
        current: Option<&str>,
    ) -> Vec<Option<String>> {
        let models = self.config.chat_models.get(&agent);
        let mut choices = vec![None];
        let remembered = models.into_iter().flat_map(|models| {
            models
                .favorites
                .iter()
                .chain(&models.recents)
                .chain(&models.picks.model)
        });
        for model in remembered.map(String::as_str).chain(current) {
            if !choices.iter().any(|known| known.as_deref() == Some(model)) {
                choices.push(Some(model.to_owned()));
            }
        }
        choices
    }

    /// Opens Settings → Projects at `project`'s card, open: a sidebar project header's
    /// Project settings….
    pub(super) fn open_project_settings(
        &mut self,
        project: &Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.project_open = Some(project_key(project));
        let stop = self
            .listed_projects()
            .iter()
            .position(|listed| listed.project == *project)
            .map(|index| Stop::Control {
                row: PROJECTS_ROW,
                action: SettingsAction::Project(ProjectControl {
                    project: index,
                    part: ProjectPart::Open,
                }),
            });
        self.open_settings(Some(SettingsSection::Projects), stop, window, cx);
    }

    /// Settings → Projects: the projects of open tabs, then the recent ones, as cards.
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
        let split = listed
            .iter()
            .position(|listed| !listed.open)
            .unwrap_or(listed.len());
        let group = |label: &'static str, range: std::ops::Range<usize>| {
            (!range.is_empty()).then(|| {
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(settings_group_label(label))
                    .children(range.map(|index| {
                        self.render_project_card(row, index, &listed[index].project, cx)
                    }))
            })
        };
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_5()
            .children(group("Open", 0..split))
            .children(group("Recent", split..listed.len()))
            .into_any_element()
    }

    /// One project's card: its head (icon, name and folder), and while open its rows.
    fn render_project_card(
        &self,
        row: &RowCx,
        index: usize,
        project: &Project,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let settings = self.project_settings(project).cloned().unwrap_or_default();
        let open = self.project_card_open(project);
        let action = |part| {
            SettingsAction::Project(ProjectControl {
                project: index,
                part,
            })
        };
        let head_action = action(ProjectPart::Open);
        let focused = self.settings_focused(row, head_action);
        let head = div()
            .id(("project-card", index))
            .relative()
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .hover(move |style| style.bg(color(blend_rgb(colors.surface, colors.foreground, 0.04))))
            .on_click(self.settings_click(row, head_action, cx))
            .child(
                div()
                    .flex_none()
                    .child(self.project_icon(Some(project), CARD_ICON_SIZE)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.project_name(project)),
                            )
                            .when(settings.pinned, |name| {
                                name.child(controls::glyph(IconName::Pin, muted))
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(UI_SMALL_TEXT_SIZE))
                            .line_height(px(UI_SMALL_LINE_HEIGHT))
                            .text_color(muted)
                            .child(project_place(project)),
                    ),
            )
            .child(controls::glyph(
                if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                },
                muted,
            ))
            .when(focused, |head| {
                head.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded_md()
                        .border_2()
                        .border_color(color(colors.accent)),
                )
            })
            .child(self.settings_focus_anchor(focused, cx));
        let card = settings_card(&colors).child(head);
        if !open {
            return card.into_any_element();
        }
        let line = |child: AnyElement| {
            div()
                .min_w_0()
                .px_4()
                .border_t_1()
                .border_color(color(colors.border))
                .child(child)
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
        let buttons = || div().flex().items_center().gap_2();

        // Name: Reset back to the folder's name, and Rename….
        let name_detail = match &settings.name {
            Some(_) => format!(
                "Its folder is {}. Tabs, the sidebar, the palette and new threads use this name.",
                project.name()
            ),
            None => "Its folder's name. Tabs, the sidebar, the palette and new threads use it."
                .to_owned(),
        };
        let name_controls = buttons()
            .when(settings.name.is_some(), |controls| {
                controls.child(reset(
                    ProjectPart::ResetName,
                    "project-reset-name",
                    format!("Back to {}", project.name()),
                ))
            })
            .child(button(ProjectPart::Rename, "project-rename", "Rename…"))
            .into_any_element();

        // Icon: what shows now, Reset back to Automatic, Choose icon… and Choose file….
        let icon_controls = buttons()
            .child(
                div()
                    .flex_none()
                    .mr_1()
                    .child(self.project_icon(Some(project), CARD_ICON_SIZE)),
            )
            .when(settings.icon.is_some(), |controls| {
                controls.child(reset(
                    ProjectPart::ResetIcon,
                    "project-reset-icon",
                    "Back to Automatic".to_owned(),
                ))
            })
            .child(button(
                ProjectPart::ChooseIcon,
                "project-choose-icon",
                "Choose icon…",
            ))
            .child(button(
                ProjectPart::ChooseFile,
                "project-choose-file",
                "Choose file…",
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

        // New threads: the agent, then its model.
        let agent_action = action(ProjectPart::Agent);
        let agent_focused = self.settings_focused(row, agent_action);
        let default_agent = self.config.chat_agent;
        let agents = AGENT_CHOICES
            .into_iter()
            .enumerate()
            .map(|(choice, agent)| {
                let (row, project) = (row.clone(), project.clone());
                controls::text_button(
                    ("project-agent", index * AGENT_CHOICES.len() + choice),
                    agent_label(agent),
                )
                .selected(settings.agent == agent)
                .detail(match agent {
                    None => format!(
                        "Settings → Chat's Default agent, now {}",
                        default_agent.name()
                    ),
                    Some(agent) => format!("New threads here start with {}", agent.name()),
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.focus_settings_control(&row, agent_action);
                    this.set_project_agent(&project, agent, cx);
                    cx.notify();
                }))
            });
        let agent = controls::highlight(
            agent_focused,
            controls::segmented(("project-agents", index), agents),
            cx,
        )
        .child(self.settings_focus_anchor(agent_focused, cx))
        .into_any_element();
        let agent_now = settings.agent.unwrap_or(default_agent);
        let agent_detail = match settings.agent {
            Some(agent) => format!("New threads in this project start with {}.", agent.name()),
            None => format!(
                "New threads in this project start with Settings → Chat's Default agent, now {}.",
                default_agent.name()
            ),
        };
        let model = self.render_project_model(row, index, project, &settings, agent_now, cx);
        let model_detail = match &settings.model {
            Some(model) => format!("New {} threads here start on {model}.", agent_now.name()),
            None => format!(
                "New {} threads here start on what {} starts on elsewhere.",
                agent_now.name(),
                agent_now.name()
            ),
        };

        card.child(line(self.settings_row(
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
                "Pinned projects come first in the sidebar's project headers and the palette."
                    .to_owned(),
                pin,
                row.compact,
            ),
        ))
        .child(line(self.settings_row(
            "New threads' agent",
            agent_detail,
            agent,
            row.compact,
        )))
        .child(line(self.settings_row(
            "New threads' model",
            model_detail,
            model,
            row.compact,
        )))
        .into_any_element()
    }

    /// New threads' model: a list of `agent`'s own default, your favorites and recent
    /// picks of it. Picking one keeps `agent` as the project's, so the model stays with
    /// the agent it belongs to.
    fn render_project_model(
        &self,
        row: &RowCx,
        index: usize,
        project: &Project,
        settings: &ProjectSettings,
        agent: ChatHarness,
        cx: &Context<Self>,
    ) -> AnyElement {
        let action = SettingsAction::Project(ProjectControl {
            project: index,
            part: ProjectPart::Model,
        });
        let focused = self.settings_focused(row, action);
        let choices = self.project_model_choices(agent, settings.model.as_deref());
        let starts_on = self
            .config
            .chat_models
            .get(&agent)
            .and_then(|models| models.picks.model.clone());
        let options = choices.iter().map(|choice| match choice {
            None => controls::select_option(format!("{}'s default", agent.name())).detail(
                match &starts_on {
                    Some(model) => format!("What {} starts on elsewhere: {model}", agent.name()),
                    None => format!("The model {} picks itself", agent.name()),
                },
            ),
            Some(model) => controls::select_option(model.clone()),
        });
        let selected = choices.iter().position(|choice| *choice == settings.model);
        let placeholder = settings
            .model
            .clone()
            .unwrap_or_else(|| format!("{}'s default", agent.name()));
        let (open_row, project) = (row.clone(), project.clone());
        let select = controls::select(
            ("project-model", index),
            options,
            selected,
            self.popup == Some(Popup::ProjectModel),
        )
        .placeholder(placeholder)
        .focused(focused)
        .anchor(Anchor::TopRight)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            this.focus_settings_control(&open_row, action);
            this.set_popup(Popup::ProjectModel, *open);
            cx.notify();
        }))
        .on_pick(cx.listener(move |this, picked: &usize, _, cx| {
            if let Some(model) = choices.get(*picked).cloned() {
                this.edit_project(
                    &project,
                    |settings| {
                        settings.agent = settings.agent.or(model.is_some().then_some(agent));
                        settings.model = model;
                    },
                    cx,
                );
            }
            cx.notify();
        }));
        div()
            .relative()
            .flex_none()
            .child(select)
            .child(self.settings_focus_anchor(focused, cx))
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
    fn projects_list_open_then_recent_once_each_pinned_first() {
        let (a, b, c, d) = (host("/a"), host("/b"), host("/c"), host("/d"));
        let listed = listed_order(
            [a.clone(), b.clone(), a.clone()],
            [c.clone(), b.clone(), d.clone()],
            |project| project.root == "/b" || project.root == "/d",
        );
        let order: Vec<(&str, bool)> = listed
            .iter()
            .map(|listed| (listed.project.root.as_str(), listed.open))
            .collect();
        assert_eq!(
            order,
            [("/b", true), ("/a", true), ("/d", false), ("/c", false)]
        );
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
