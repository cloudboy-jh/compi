//! The model picker: one popover, opening above the composer, for the harness's model,
//! thinking level and mode, and the chat's permissions. A strip of tabs on the left
//! picks what the model list shows: your favorites and recent picks, or one provider's
//! models; typing searches every model instead. The thinking levels sit beside the list,
//! the modes and permissions in the footer. Picking a model moves the keyboard to the
//! thinking levels; picking a level, a mode or permissions closes it. Model picks are
//! remembered per harness for its new sessions (`ChatModels`); permissions apply to this
//! chat only. Behaviour, strip and spacing follow T3 Code's picker (decided 2026-11-19).
//!
//! Settings → Projects' New threads opens the same picker, without thinking levels,
//! modes or permissions, for the model a project's new threads start on. No agent runs
//! there to list its models, so it lists what the project's agent listed last in any
//! chat (`ListedModels`), with the favorites and recent picks Compi remembers of it.
use super::*;
use crate::config::ChatModels;
use crate::gui::model_icons;
use crate::listed_models::AgentModels;
use compi_protocol::Project;
use compi_protocol::chat::{ChatChoice, ChatOption, ChatPlan};
use gpui::{Div, Hsla, canvas, linear_color_stop, linear_gradient, svg};
use gpui_component::{ActiveTheme as _, Icon};
use std::cell::Cell;

const STRIP_WIDTH: f32 = 44.0;
const LIST_WIDTH: f32 = 340.0;
const THINKING_WIDTH: f32 = 176.0;
/// The strip, list and thinking levels keep this height, so switching tabs or models
/// never resizes the popover.
const BODY_HEIGHT: f32 = 336.0;
const ROW_HEIGHT: f32 = 32.0;
/// A row and the gap under it.
const ROW_PITCH: f32 = 34.0;
/// Rows Page Up and Page Down move by.
const PAGE_ROWS: usize = 8;
const ICON_SIZE: f32 = 16.0;
/// The search's and the thinking header's line, ruled under; one rule runs across both.
const HEADER_HEIGHT: f32 = 34.0;
/// Between the popover and the composer under it.
const GAP: f32 = 6.0;
/// Frames after opening in which the search takes the keyboard back.
const CLAIM_FRAMES: u8 = 5;

/// What the model list shows while nothing is typed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PickerTab {
    /// Your favorites, then your recent picks.
    Saved,
    /// One provider's models (`None`: models naming no provider).
    Provider(Option<String>),
}

/// The part of the picker the arrow keys and Enter act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PickerColumn {
    Strip,
    Models,
    Thinking,
    Mode,
    /// The chat's permissions, in the footer beside the modes.
    Approval,
}

/// The open model picker: its search and tab, the column with the keyboard, and each
/// column's highlighted row.
pub(super) struct ModelPicker {
    search: Entity<InputState>,
    _events: Subscription,
    tab: PickerTab,
    column: PickerColumn,
    /// Index into the listed models.
    highlight: usize,
    /// Index into the thinking setting's choices.
    thinking: usize,
    /// Index into the mode setting's choices.
    mode: usize,
    /// Index into `CHAT_APPROVALS`.
    approval: usize,
    scroll: UniformListScrollHandle,
    /// Frames left in which the search takes the keyboard back: the popover takes it
    /// for itself a frame or two after opening.
    claim_keyboard: u8,
}

/// What a model picker picks for: chat `id`'s harness, or the new threads of the project
/// Settings → Projects shows (one at a time, `ChatUi::project_picker`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum PickerOwner {
    Chat(String),
    Project,
}

/// Settings → Projects' model picker for a project's new threads.
pub(super) struct ProjectPicker {
    project: Project,
    /// The agent the project's new threads start with, whose models it lists.
    harness: ChatHarness,
    /// The models offered; `current` is the project's model, empty for the agent's own.
    option: ChatOption,
    plans: Vec<ChatPlan>,
    /// No chat of the agent listed its models yet.
    unlisted: bool,
    picker: ModelPicker,
}

/// A model `value` no listing named, as the picker shows it: named by its ID.
fn unlisted_choice(value: &str) -> ChatChoice {
    ChatChoice {
        value: value.to_owned(),
        name: value.rsplit('/').next().unwrap_or(value).to_owned(),
        description: None,
        group: None,
    }
}

/// The models a project's picker offers: what its agent listed last (`listed`), then
/// the models Compi remembers of it that the listing lacks (favorites, recent picks,
/// what it starts on) and the project's `current` one.
fn project_choices(
    listed: Option<&AgentModels>,
    remembered: &ChatModels,
    current: Option<&str>,
) -> Vec<ChatChoice> {
    let mut choices = listed.map_or_else(Vec::new, |listed| listed.choices.clone());
    let saved = remembered
        .favorites
        .iter()
        .chain(&remembered.recents)
        .chain(&remembered.picks.model)
        .map(String::as_str)
        .chain(current);
    for value in saved {
        if !choices.iter().any(|choice| choice.value == value) {
            choices.push(unlisted_choice(value));
        }
    }
    choices
}

/// Where the composer card and the model button were last drawn, in window
/// coordinates: the popover opens above the card, from the button.
#[derive(Default)]
pub(super) struct PickerAnchor {
    pub(super) card_top: Cell<Pixels>,
    button_top: Cell<Pixels>,
}

/// A row of the model list.
#[derive(Debug, PartialEq)]
enum PickerRow {
    /// A label over the models after it: Favorites, Recent, or the provider with its
    /// plan's usage or that it is paid per use.
    Label {
        title: String,
        detail: Option<String>,
    },
    /// A model, by index into `option.choices`, with what tells it apart: its provider
    /// (in Favorites, Recent and search results) and its ID (when another listed model
    /// has the same name).
    Choice {
        index: usize,
        provider: bool,
        id: bool,
    },
}

/// The rows of the model list, and the row of each model in the order arrow keys step
/// through them.
#[derive(Default)]
struct PickerLayout {
    rows: Vec<PickerRow>,
    choices: Vec<usize>,
}

impl PickerLayout {
    /// The model (index into `option.choices`) listed `listed`th.
    fn choice(&self, listed: usize) -> Option<usize> {
        match self.rows.get(*self.choices.get(listed)?)? {
            PickerRow::Choice { index, .. } => Some(*index),
            PickerRow::Label { .. } => None,
        }
    }

    /// Where model `index` is listed.
    fn listed(&self, index: usize) -> Option<usize> {
        (0..self.choices.len()).find(|&listed| self.choice(listed) == Some(index))
    }

    /// The row to bring into view for the model listed `listed`th: the label over it
    /// when it is the first under one.
    fn reveal(&self, listed: usize) -> usize {
        let row = self.choices[listed];
        if row > 0 && matches!(self.rows[row - 1], PickerRow::Label { .. }) {
            row - 1
        } else {
            row
        }
    }

    fn push_label(&mut self, title: impl Into<String>, detail: Option<String>) {
        self.rows.push(PickerRow::Label {
            title: title.into(),
            detail,
        });
    }

    fn push_models(&mut self, models: &[usize], provider: bool) {
        for &index in models {
            self.choices.push(self.rows.len());
            self.rows.push(PickerRow::Choice {
                index,
                provider,
                id: false,
            });
        }
    }

    /// Marks models whose name another listed model of the same provider shares, so
    /// their IDs show; across providers, the provider tells them apart.
    fn mark_namesakes(&mut self, option: &ChatOption) {
        let name = |row: &PickerRow| match row {
            PickerRow::Choice { index, .. } => {
                let choice = &option.choices[*index];
                Some((choice.name.as_str(), choice_provider(choice)))
            }
            PickerRow::Label { .. } => None,
        };
        let names: Vec<Option<(&str, Option<&str>)>> = self.rows.iter().map(name).collect();
        for (at, row) in self.rows.iter_mut().enumerate() {
            if let PickerRow::Choice { id, .. } = row {
                *id = names
                    .iter()
                    .enumerate()
                    .any(|(other, name)| other != at && name.is_some() && *name == names[at]);
            }
        }
    }
}

/// The provider a model choice belongs to: its group, else its value before the `/`.
fn choice_provider(choice: &ChatChoice) -> Option<&str> {
    choice
        .group
        .as_deref()
        .or_else(|| choice.value.split_once('/').map(|(provider, _)| provider))
}

/// A model's ID as its provider knows it: its value after the last `/`.
fn choice_id(choice: &ChatChoice) -> &str {
    choice.value.rsplit('/').next().unwrap_or(&choice.value)
}

/// A provider as people know it: `openai-codex` is OpenAI Codex.
pub(super) fn provider_name(provider: &str) -> String {
    let known = match provider {
        "anthropic" => "Anthropic",
        "openai" => "OpenAI",
        "openai-codex" => "OpenAI Codex",
        "openrouter" => "OpenRouter",
        "amazon-bedrock" => "Amazon Bedrock",
        "google" => "Google",
        "google-vertex" => "Google Vertex",
        "github-copilot" => "GitHub Copilot",
        "xai" => "xAI",
        "deepseek" => "DeepSeek",
        "opencode" => "OpenCode",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    provider
        .split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut letters = word.chars();
            letters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(letters).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the strip and list call `provider`; models naming none are the agent's own
/// (`AgentEntry::own_models`), else Other.
fn provider_title(provider: Option<&str>, harness: ChatHarness) -> String {
    match provider {
        Some(provider) => provider_name(provider),
        None if harness.entry().own_models => harness.name().to_owned(),
        None => "Other".to_owned(),
    }
}

/// The providers in the strip's order: those with a subscription plan first, then
/// those paid per use; in each, the current model's provider first, then by name.
fn providers<'a>(option: &'a ChatOption, plans: &[ChatPlan]) -> Vec<Option<&'a str>> {
    let current = option
        .choices
        .iter()
        .find(|choice| choice.value == option.current)
        .and_then(choice_provider);
    let mut providers: Vec<Option<&str>> = Vec::new();
    for provider in option.choices.iter().map(choice_provider) {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    providers.sort_by_key(|&provider| {
        (
            plan(plans, provider).is_none(),
            provider.is_none(),
            provider != current,
            provider.map(provider_name).unwrap_or_default(),
        )
    });
    providers
}

fn plan<'a>(plans: &'a [ChatPlan], provider: Option<&str>) -> Option<&'a ChatPlan> {
    plans
        .iter()
        .find(|plan| Some(plan.provider.as_str()) == provider)
}

/// How a provider is paid: its plan and what is left of its tightest limit, or per use.
fn plan_detail(plans: &[ChatPlan], provider: Option<&str>) -> Option<String> {
    match plan(plans, provider) {
        Some(plan) => Some(
            match plan.limits.iter().min_by_key(|limit| limit.left_percent) {
                Some(limit) => format!(
                    "Subscription · {}: {}% left",
                    limit.label, limit.left_percent
                ),
                None => "Subscription".to_owned(),
            },
        ),
        None => provider.map(|_| "Pay per use".to_owned()),
    }
}

/// The models matching `query`, as indices into `option.choices`: every word of it in
/// the name, value or provider, ignoring case.
fn matching_models(option: &ChatOption, query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    option
        .choices
        .iter()
        .enumerate()
        .filter(|(_, choice)| {
            words.is_empty() || {
                let haystack = format!(
                    "{} {} {}",
                    choice.name,
                    choice.value,
                    choice_provider(choice)
                        .map(provider_name)
                        .unwrap_or_default()
                )
                .to_lowercase();
                words.iter().all(|word| haystack.contains(word))
            }
        })
        .map(|(index, _)| index)
        .collect()
}

/// Your favorites still listed, in the order you starred them, and your recent picks
/// that are not favorites, newest first; as indices into `option.choices`.
fn saved_models(option: &ChatOption, remembered: &ChatModels) -> (Vec<usize>, Vec<usize>) {
    let index = |value: &String| {
        option
            .choices
            .iter()
            .position(|choice| choice.value == *value)
    };
    let favorites: Vec<usize> = remembered.favorites.iter().filter_map(index).collect();
    let recent: Vec<usize> = remembered
        .recents
        .iter()
        .filter_map(index)
        .filter(|index| !favorites.contains(index))
        .collect();
    (favorites, recent)
}

/// The model list: with `query`, every matching model in the strip's provider order;
/// else tab `tab`'s, under its labels.
fn picker_layout(
    option: &ChatOption,
    query: &str,
    plans: &[ChatPlan],
    remembered: &ChatModels,
    tab: &PickerTab,
    harness: ChatHarness,
) -> PickerLayout {
    let mut layout = PickerLayout::default();
    if !query.trim().is_empty() {
        let matches = matching_models(option, query);
        for provider in providers(option, plans) {
            let models: Vec<usize> = matches
                .iter()
                .copied()
                .filter(|&index| choice_provider(&option.choices[index]) == provider)
                .collect();
            layout.push_models(&models, true);
        }
    } else {
        match tab {
            PickerTab::Saved => {
                let (favorites, recent) = saved_models(option, remembered);
                if !favorites.is_empty() {
                    layout.push_label("Favorites", None);
                    layout.push_models(&favorites, true);
                }
                if !recent.is_empty() {
                    layout.push_label("Recent", None);
                    layout.push_models(&recent, true);
                }
            }
            PickerTab::Provider(provider) => {
                let provider = provider.as_deref();
                layout.push_label(
                    provider_title(provider, harness),
                    plan_detail(plans, provider),
                );
                let models: Vec<usize> = (0..option.choices.len())
                    .filter(|&index| choice_provider(&option.choices[index]) == provider)
                    .collect();
                layout.push_models(&models, false);
            }
        }
    }
    layout.mark_namesakes(option);
    layout
}

/// The tab the picker opens on: your favorites and recent picks when there are any,
/// else the current model's provider.
fn opening_tab(option: &ChatOption, remembered: &ChatModels) -> PickerTab {
    let (favorites, recent) = saved_models(option, remembered);
    if !favorites.is_empty() || !recent.is_empty() {
        return PickerTab::Saved;
    }
    let current = option
        .choices
        .iter()
        .find(|choice| choice.value == option.current);
    PickerTab::Provider(current.and_then(choice_provider).map(str::to_owned))
}

/// The strip's tabs, in order.
fn strip_tabs(option: &ChatOption, plans: &[ChatPlan]) -> Vec<PickerTab> {
    std::iter::once(PickerTab::Saved)
        .chain(
            providers(option, plans)
                .into_iter()
                .map(|provider| PickerTab::Provider(provider.map(str::to_owned))),
        )
        .collect()
}

/// The mark drawn for model `choice`: its family's, else its provider's, else for a
/// harness's own model the harness's family's.
fn model_mark(choice: &ChatChoice, harness: ChatHarness) -> Option<&'static str> {
    model_icons::for_model(&choice.value, &choice.name).or_else(|| match choice_provider(choice) {
        Some(provider) => model_icons::for_provider(provider),
        None if harness.entry().own_models => model_icons::for_harness(harness),
        None => None,
    })
}

fn mark(path: Option<&'static str>, size: f32, tint: Hsla) -> AnyElement {
    match path {
        Some(path) => svg()
            .flex_none()
            .path(path)
            .size(ui_rems(size))
            .text_color(tint)
            .into_any_element(),
        None => div().flex_none().size(ui_rems(size)).into_any_element(),
    }
}

impl ChatModel {
    /// The harness's settings the picker shows: model, thinking level and mode.
    fn picker_options(&self) -> [Option<&ChatOption>; 3] {
        let option = |category| self.state.as_ref().and_then(|state| state.option(category));
        [option("model"), option("thought_level"), option("mode")]
    }

    /// Why this chat's permissions cannot change now: they restart its harness, and a
    /// started thread's restart would end the running reply or drop its queued prompts.
    /// A draft's can always change.
    fn approval_refusal(&self, chat: &ChatContent) -> Option<String> {
        if self.draft(chat) {
            return None;
        }
        let state = self.state.as_ref()?;
        let harness = chat.harness.name();
        if state.working() {
            Some(format!(
                "Changing permissions restarts {harness}: wait for the reply, or stop it"
            ))
        } else if !state.queued.is_empty() {
            Some(format!(
                "Changing permissions restarts {harness}, which drops the queued messages"
            ))
        } else {
            None
        }
    }
}

/// The icon and name of a permissions choice Compi makes, for a chat with `harness`.
fn approval_face(approval: ChatApproval, harness: ChatHarness) -> (IconName, &'static str) {
    match approval {
        ChatApproval::Harness => harness_setting_face(harness),
        ChatApproval::AlwaysAsk => (IconName::ShieldCheck, "Ask before anything"),
        ChatApproval::Write => (IconName::ShieldHalf, "Edits allowed"),
        ChatApproval::Yolo => (IconName::ShieldOff, "Full access"),
    }
}

/// The icon and name of the agent's own approval setting.
fn harness_setting_face(harness: ChatHarness) -> (IconName, &'static str) {
    (IconName::Shield, harness.entry().own_setting)
}

/// The icon and name of an approval mode `harness` runs with, by its own name for it: the
/// Compi level it is, under the mode's own name when it has one.
fn approval_mode_face(harness: ChatHarness, mode: &str) -> (IconName, &'static str) {
    match harness.approval_level(mode) {
        Some((level, name)) => {
            let (icon, label) = approval_face(level, harness);
            (icon, name.unwrap_or(label))
        }
        None => harness_setting_face(harness),
    }
}

/// How permissions `approval` show in a chat with `harness`: for the harness's own
/// setting, the mode it runs with (`running`, as the daemon reports it) when known.
fn approval_shown(
    approval: ChatApproval,
    harness: ChatHarness,
    running: Option<&str>,
) -> (IconName, &'static str) {
    match (approval, running) {
        (ChatApproval::Harness, Some(mode)) => approval_mode_face(harness, mode),
        (chosen, _) => approval_face(chosen, harness),
    }
}

/// Whether the composer row shows the shield: a chat's own permissions (`chosen`) that
/// differ from `default` (Settings → Agents → Approvals).
fn approval_differs(chosen: Option<ChatApproval>, default: ChatApproval) -> bool {
    chosen.is_some_and(|chosen| chosen != default)
}

/// The index of `option`'s current choice.
fn current_index(option: Option<&ChatOption>) -> usize {
    option
        .and_then(|option| {
            option
                .choices
                .iter()
                .position(|choice| choice.value == option.current)
        })
        .unwrap_or(0)
}

/// A column's heading line, with the rule under it level across the popover.
fn column_header(child: impl IntoElement, border: Hsla) -> Div {
    div().flex_none().px_3().pt_2().child(
        div()
            .h(ui_rems(HEADER_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .child(child),
    )
}

impl CompiApp {
    /// What Compi remembers of `harness`'s models.
    fn remembered_models(&self, harness: ChatHarness) -> ChatModels {
        self.config
            .chat_models
            .get(&harness)
            .cloned()
            .unwrap_or_default()
    }

    /// Changes what Compi remembers of `harness`'s models, and what its new sessions
    /// start on.
    fn remember_models(&mut self, harness: ChatHarness, change: impl FnOnce(&mut ChatModels)) {
        let mut models = self.remembered_models(harness);
        change(&mut models);
        if let Err(error) = self.config.save_chat_models(harness, models) {
            self.global_error = Some(error);
        }
    }

    /// `owner`'s open picker.
    fn owner_picker(&self, owner: &PickerOwner) -> Option<&ModelPicker> {
        match owner {
            PickerOwner::Chat(chat) => self.chat.chats.get(chat)?.picker.as_ref(),
            PickerOwner::Project => self
                .chat
                .project_picker
                .as_ref()
                .map(|project| &project.picker),
        }
    }

    fn owner_picker_mut(&mut self, owner: &PickerOwner) -> Option<&mut ModelPicker> {
        match owner {
            PickerOwner::Chat(chat) => self.chat.chats.get_mut(chat)?.picker.as_mut(),
            PickerOwner::Project => self
                .chat
                .project_picker
                .as_mut()
                .map(|project| &mut project.picker),
        }
    }

    /// What `owner`'s picker lists: the harness, its model setting and its plans.
    fn owner_models(&self, owner: &PickerOwner) -> Option<(ChatHarness, &ChatOption, &[ChatPlan])> {
        match owner {
            PickerOwner::Chat(chat) => {
                let state = self.chat.chats.get(chat)?.state.as_ref()?;
                Some((state.harness, state.option("model")?, &state.plans))
            }
            PickerOwner::Project => {
                let project = self.chat.project_picker.as_ref()?;
                Some((project.harness, &project.option, &project.plans))
            }
        }
    }

    /// The settings `owner`'s picker shows: model, thinking level and mode; a project's
    /// has only the model.
    fn owner_options(&self, owner: &PickerOwner) -> [Option<&ChatOption>; 3] {
        match owner {
            PickerOwner::Chat(chat) => self
                .chat
                .chats
                .get(chat)
                .map_or([None; 3], ChatModel::picker_options),
            PickerOwner::Project => [
                self.chat
                    .project_picker
                    .as_ref()
                    .map(|project| &project.option),
                None,
                None,
            ],
        }
    }

    /// The composer row's model button: the model's mark and name, then a brain and the
    /// thinking level, opening the picker above the composer; the mode when it is not
    /// the first (Plan, say); and a shield when the chat's permissions are not the
    /// normal ones.
    pub(super) fn render_chat_models(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let [models, thinking, mode] = model.picker_options();
        let picker = models.is_some() || thinking.is_some() || mode.is_some();
        let shield = self.render_chat_approval_chip(chat, model, picker, cx);
        if !picker {
            return shield.into_iter().collect();
        }
        let open = model.menu == Some(ChatMenu::Models);
        let label = models
            .or(thinking)
            .map_or("", |option| option.current_name());
        let mut button = text_button("chat-models", label.to_owned())
            .ghost()
            .tooltip(format!("Model and thinking ({})", picker_key_label()))
            .menu_open(open);
        let current_model = models.and_then(|option| {
            option
                .choices
                .iter()
                .find(|choice| choice.value == option.current)
        });
        if let Some(path) = current_model.and_then(|choice| model_mark(choice, chat.harness)) {
            button = button.icon(Icon::empty().path(path));
        } else if models.is_none() && thinking.is_some() {
            button = button.icon(IconName::Brain);
        }
        if let (Some(_), Some(thinking)) = (models, thinking) {
            button = button.trailing(IconName::Brain, thinking.current_name().to_owned());
        }
        let anchor = model.picker_anchor.clone();
        let measured = anchor.clone();
        let trigger = div().relative().child(button).child(
            canvas(
                move |bounds, _, _| measured.button_top.set(bounds.origin.y),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        );
        // The popover's bottom meets the trigger's top; it rises past the rest of the
        // card, so the draft stays in view.
        let rise = (anchor.button_top.get() - anchor.card_top.get()).max(px(0.)) + px(GAP);
        let id = chat.id.clone();
        let on_open_change = cx.listener(move |this, open: &bool, window, cx| {
            if *open {
                this.open_model_picker(&id, PickerColumn::Models, window, cx);
            } else {
                this.set_chat_menu(&id, ChatMenu::Models, false);
            }
            cx.notify();
        });
        let keys = {
            let (app, owner) = (cx.entity().downgrade(), PickerOwner::Chat(chat.id.clone()));
            move |key: &Keystroke, window: &mut Window, cx: &mut App| {
                if let Some(app) = app.upgrade() {
                    app.update(cx, |this, cx| {
                        this.model_picker_key(&owner, key, window, cx)
                    });
                }
            }
        };
        let content = model
            .picker
            .as_ref()
            .filter(|_| open)
            .map(|picker| self.render_model_picker(chat, model, picker, cx));
        let popover = controls::popover("chat-models-menu", trigger, open)
            .anchor(Anchor::BottomLeft)
            .offset(rise)
            .on_open_change(on_open_change)
            .on_key(keys);
        let mut shown = vec![match content {
            Some(content) => popover.content(content).into_any_element(),
            None => popover.into_any_element(),
        }];
        if let Some(mode) = mode.filter(|mode| {
            mode.choices
                .first()
                .is_some_and(|first| first.value != mode.current)
        }) {
            let id = chat.id.clone();
            shown.push(
                text_button("chat-mode", mode.current_name().to_owned())
                    .ghost()
                    .tooltip(mode.name.clone())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_model_picker(&id, PickerColumn::Mode, window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        shown.extend(shield);
        shown
    }

    /// The composer row's shield, only while this chat's permissions differ from
    /// Settings → Agents → Approvals, as the mode shows only when it is not the first. It
    /// opens the picker on the permissions; until the harness reports its settings
    /// (`picker`) there is no picker to open.
    fn render_chat_approval_chip(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        picker: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let default = self.config.chat_approval;
        let chosen = model.chosen_approval();
        if !approval_differs(chosen, default) {
            return None;
        }
        let running = model
            .state
            .as_ref()
            .and_then(|state| state.approval_mode.as_deref());
        let (icon, label) = approval_shown(model.approval(default), chat.harness, running);
        let id = chat.id.clone();
        Some(
            text_button("chat-approval", label)
                .ghost()
                .icon(icon)
                .tooltip("Permissions for this chat")
                .detail(format!(
                    "Settings → Agents → Approvals: {}",
                    super::super::settings::chat_approval_label(default)
                ))
                .disabled_because(
                    (!picker).then(|| format!("Waiting for {} to start", chat.harness.name())),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_model_picker(&id, PickerColumn::Approval, window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// Opens chat `chat`'s model picker with the keyboard in `column`, on the tab of
    /// your saved models (or the current model's provider) with the current model
    /// highlighted; an open one keeps its state, its search taking the keyboard again.
    pub(super) fn open_model_picker(
        &mut self,
        chat: &str,
        column: PickerColumn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_, content)) = self.chat_pane(chat)
            && self.chat_draft(&content) == Some(true)
        {
            self.want_agents(&content.environment, cx);
        }
        let remembered = self
            .chat
            .chats
            .get(chat)
            .map(|model| self.remembered_models(model.harness()))
            .unwrap_or_default();
        let default = self.config.chat_approval;
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        if model.menu == Some(ChatMenu::Models)
            && let Some(picker) = &model.picker
        {
            window.focus(&picker.search.focus_handle(cx), cx);
            return;
        }
        let [models, thinking, mode] = model.picker_options();
        if models.is_none() && thinking.is_none() && mode.is_none() {
            return;
        }
        let column = match column {
            PickerColumn::Models | PickerColumn::Strip if models.is_none() => {
                PickerColumn::Thinking
            }
            column => column,
        };
        let tab = models.map_or(PickerTab::Saved, |option| opening_tab(option, &remembered));
        let (thinking, mode) = (current_index(thinking), current_index(mode));
        let approval = crate::config::CHAT_APPROVALS
            .iter()
            .position(|approval| *approval == model.approval(default))
            .unwrap_or(0);
        model.menu = Some(ChatMenu::Models);
        let owner = PickerOwner::Chat(chat.to_owned());
        let (search, events) = Self::picker_search(owner.clone(), window, cx);
        model.picker = Some(ModelPicker {
            search,
            _events: events,
            tab: tab.clone(),
            column,
            highlight: 0,
            thinking,
            mode,
            approval,
            scroll: UniformListScrollHandle::new(),
            claim_keyboard: CLAIM_FRAMES,
        });
        self.show_model_tab(&owner, Some(tab), cx);
    }

    /// A picker's search, holding the keyboard: typing searches `owner`'s models, Enter
    /// picks.
    fn picker_search(
        owner: PickerOwner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<InputState>, Subscription) {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        let events = cx.subscribe_in(
            &search,
            window,
            move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.show_model_tab(&owner, None, cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.model_picker_enter(&owner, window, cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        window.focus(&search.focus_handle(cx), cx);
        (search, events)
    }

    /// Shows tab `tab` (or, with `None`, the search's results in the current one) with
    /// the current model highlighted when listed, else the first, in view.
    fn show_model_tab(&mut self, owner: &PickerOwner, tab: Option<PickerTab>, cx: &App) {
        if let Some(tab) = tab
            && let Some(picker) = self.owner_picker_mut(owner)
        {
            picker.tab = tab;
        }
        let listed = self
            .model_layout(owner, cx)
            .map(|(layout, current)| (layout.listed(current), layout));
        let Some(picker) = self.owner_picker_mut(owner) else {
            return;
        };
        picker.column = match picker.column {
            PickerColumn::Strip => PickerColumn::Strip,
            _ => PickerColumn::Models,
        };
        match listed {
            Some((Some(listed), layout)) => {
                picker.highlight = listed;
                picker
                    .scroll
                    .scroll_to_item(layout.reveal(listed), ScrollStrategy::Center);
            }
            _ => {
                picker.highlight = 0;
                picker.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
        }
    }

    /// `owner`'s open picker's model list, and the current model's index.
    fn model_layout(&self, owner: &PickerOwner, cx: &App) -> Option<(PickerLayout, usize)> {
        let picker = self.owner_picker(owner)?;
        let (harness, option, plans) = self.owner_models(owner)?;
        let layout = picker_layout(
            option,
            &picker.search.read(cx).value(),
            plans,
            &self.remembered_models(harness),
            &picker.tab,
            harness,
        );
        Some((layout, current_index(Some(option))))
    }

    /// Drops chat `chat`'s picker once its popover closed; while it opens, gives its
    /// search the keyboard.
    pub(super) fn prepare_model_picker(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        if model.menu != Some(ChatMenu::Models) {
            model.picker = None;
            return;
        }
        if let Some(picker) = model.picker.as_mut()
            && picker.claim_keyboard > 0
        {
            picker.claim_keyboard -= 1;
            let search = picker.search.focus_handle(cx);
            if !search.is_focused(window) {
                window.focus(&search, cx);
            }
            cx.notify();
        }
    }

    /// Keys in the open picker: Up, Down, Page Up and Page Down move in the column (in
    /// the strip, between tabs); Left, Right, Tab and Shift+Tab change columns (on the
    /// modes and permissions, Left and Right pick between them); Enter picks; Ctrl+D
    /// (Cmd+D) stars the highlighted model.
    fn model_picker_key(
        &mut self,
        owner: &PickerOwner,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key.key == "enter" {
            // With the search holding the keyboard, its own Enter (`PressEnter`) picks.
            let searching = self
                .owner_picker(owner)
                .is_some_and(|picker| picker.search.focus_handle(cx).is_focused(window));
            if !searching {
                self.model_picker_enter(owner, window, cx);
                cx.notify();
            }
            return;
        }
        let modifiers = &key.modifiers;
        // A key typed before the search took the keyboard (right after opening) goes
        // into it.
        if let Some(text) = key.key_char.as_deref()
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
            && let Some(picker) = self.owner_picker(owner)
        {
            let search = picker.search.clone();
            window.focus(&search.focus_handle(cx), cx);
            search.update(cx, |search, cx| search.insert(text.to_owned(), window, cx));
            self.show_model_tab(owner, None, cx);
            cx.notify();
            return;
        }
        let favorite_modifier = if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        };
        if key.key == "d" && favorite_modifier && !modifiers.alt && !modifiers.shift {
            self.toggle_highlighted_favorite(owner, cx);
            cx.notify();
            return;
        }
        let layout = self.model_layout(owner, cx);
        let Some(picker) = self.owner_picker(owner) else {
            return;
        };
        let [models, thinking, mode] = self.owner_options(owner);
        let searching = !picker.search.read(cx).value().trim().is_empty();
        let tabs = self
            .owner_models(owner)
            .map(|(_, option, plans)| strip_tabs(option, plans))
            .unwrap_or_default();
        let chat = matches!(owner, PickerOwner::Chat(_));
        let columns: Vec<PickerColumn> = [
            (models.is_some() && !searching).then_some(PickerColumn::Strip),
            models.map(|_| PickerColumn::Models),
            thinking.map(|_| PickerColumn::Thinking),
            mode.map(|_| PickerColumn::Mode),
            chat.then_some(PickerColumn::Approval),
        ]
        .into_iter()
        .flatten()
        .collect();
        let (thinking_count, mode_count, approval_count) = (
            thinking.map_or(0, |option| option.choices.len()),
            mode.map_or(0, |option| option.choices.len()),
            crate::config::CHAT_APPROVALS.len(),
        );
        let (column, tab) = (picker.column, picker.tab.clone());
        let at = columns
            .iter()
            .position(|&shown| shown == column)
            .unwrap_or(0);
        let step_column = |forward: bool| -> PickerColumn {
            let count = columns.len().max(1);
            let next = if forward {
                (at + 1) % count
            } else {
                (at + count - 1) % count
            };
            columns.get(next).copied().unwrap_or(PickerColumn::Models)
        };
        let step = |at: usize, count: usize, by: isize| -> usize {
            if count == 0 {
                return 0;
            }
            (at as isize + by).clamp(0, count as isize - 1) as usize
        };
        let move_by = match key.key.as_str() {
            "down" => Some(1),
            "up" => Some(-1),
            "pagedown" => Some(PAGE_ROWS as isize),
            "pageup" => Some(-(PAGE_ROWS as isize)),
            _ => None,
        };
        if let (PickerColumn::Strip, Some(by)) = (column, move_by) {
            let at = tabs.iter().position(|shown| *shown == tab).unwrap_or(0);
            let next = tabs[step(at, tabs.len(), by.signum())].clone();
            self.show_model_tab(owner, Some(next), cx);
            cx.notify();
            return;
        }
        let Some(picker) = self.owner_picker_mut(owner) else {
            return;
        };
        match (column, key.key.as_str(), move_by) {
            (_, "tab", _) => picker.column = step_column(!modifiers.shift),
            (PickerColumn::Mode, "left", _) => picker.mode = step(picker.mode, mode_count, -1),
            (PickerColumn::Mode, "right", _) => picker.mode = step(picker.mode, mode_count, 1),
            (PickerColumn::Mode | PickerColumn::Approval, "up", _) => {
                picker.column = step_column(false);
            }
            (PickerColumn::Approval, "left", _) => {
                picker.approval = step(picker.approval, approval_count, -1);
            }
            (PickerColumn::Approval, "right", _) => {
                picker.approval = step(picker.approval, approval_count, 1);
            }
            (_, "right", _) => picker.column = step_column(true),
            (_, "left", _) => picker.column = step_column(false),
            (PickerColumn::Thinking, _, Some(by)) => {
                picker.thinking = step(picker.thinking, thinking_count, by);
            }
            (PickerColumn::Models, _, Some(by)) => {
                let Some((layout, _)) = layout else {
                    return;
                };
                let count = layout.choices.len();
                if count == 0 {
                    return;
                }
                picker.highlight = step(picker.highlight.min(count - 1), count, by);
                picker
                    .scroll
                    .scroll_to_item(layout.reveal(picker.highlight), ScrollStrategy::Nearest);
            }
            _ => return,
        }
        cx.notify();
    }

    /// Enter in the picker: in the strip, moves to the models; on a model, picks it and
    /// moves to the thinking levels; on a level, mode or permissions, picks it and closes.
    fn model_picker_enter(
        &mut self,
        owner: &PickerOwner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layout = self.model_layout(owner, cx);
        let [models, thinking, mode] = self.owner_options(owner);
        let Some(picker) = self.owner_picker(owner) else {
            return;
        };
        let chosen = |option: Option<&ChatOption>, at: usize| {
            let option = option?;
            Some((option.id.clone(), option.choices.get(at)?.value.clone()))
        };
        let column = picker.column;
        let model = layout.and_then(|(layout, _)| {
            let listed = picker.highlight.min(layout.choices.len().checked_sub(1)?);
            chosen(models, layout.choice(listed)?)
        });
        let thinking = chosen(thinking, picker.thinking);
        let mode = chosen(mode, picker.mode);
        let approval = crate::config::CHAT_APPROVALS.get(picker.approval).copied();
        match (column, owner) {
            (PickerColumn::Strip, _) => {
                if let Some(picker) = self.owner_picker_mut(owner) {
                    picker.column = PickerColumn::Models;
                }
            }
            (PickerColumn::Models, _) => {
                if let Some((option, value)) = model {
                    self.pick_model(owner, option, value, window, cx);
                }
            }
            (PickerColumn::Thinking, PickerOwner::Chat(chat)) => {
                if let Some((option, value)) = thinking {
                    self.pick_thinking(chat, option, value, window, cx);
                }
            }
            (PickerColumn::Mode, PickerOwner::Chat(chat)) => {
                if let Some((option, value)) = mode {
                    self.set_chat_menu(chat, ChatMenu::Models, false);
                    self.send_chat_option(chat, option, value, window, cx);
                }
            }
            (PickerColumn::Approval, PickerOwner::Chat(chat)) => {
                if let Some(approval) = approval {
                    self.pick_chat_approval(chat, approval, window, cx);
                }
            }
            _ => {}
        }
    }

    /// Picks model `value`. A chat changes to it, remembers it for new sessions and
    /// recent, and moves the keyboard to the thinking levels (closing the picker without
    /// any); a project's new threads start on it, and the picker closes.
    fn pick_model(
        &mut self,
        owner: &PickerOwner,
        option: String,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PickerOwner::Chat(chat) = owner else {
            self.pick_project_model(Some(value), cx);
            return;
        };
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let harness = model.harness();
        let thinking = model.picker_options()[1].map(|option| current_index(Some(option)));
        match (model.picker.as_mut(), thinking) {
            (Some(picker), Some(thinking)) => {
                picker.column = PickerColumn::Thinking;
                picker.thinking = thinking;
            }
            _ => model.menu = None,
        }
        self.send_chat_option(chat, option, value.clone(), window, cx);
        self.remember_models(harness, |models| models.picked_model(&value));
    }

    /// New threads in the picker's project start on `model` (`None`: its agent's own),
    /// with the agent the picker lists; the picker closes.
    fn pick_project_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        let Some(picker) = self.chat.project_picker.take() else {
            return;
        };
        self.set_project_model(&picker.project, picker.harness, model, cx);
    }

    /// Sets the chat's thinking level to `value`, remembers it for new sessions, and
    /// closes the picker.
    fn pick_thinking(
        &mut self,
        chat: &str,
        option: String,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let harness = model.harness();
        model.menu = None;
        self.send_chat_option(chat, option, value.clone(), window, cx);
        self.remember_models(harness, |models| models.picks.thought_level = Some(value));
    }

    /// Gives chat `chat` the permissions `approval` and closes the picker: its watcher
    /// passes them on, and a harness already running starts again with them, a draft on
    /// a new session as it has nothing to resume, a thread on its own session. Nothing
    /// changes while `approval_refusal` says why not.
    fn pick_chat_approval(
        &mut self,
        chat: &str,
        approval: ChatApproval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let default = self.config.chat_approval;
        let content = self.chat_pane(chat).map(|(_, content)| content);
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        if content.is_some_and(|content| model.approval_refusal(&content).is_some()) {
            return;
        }
        model.menu = None;
        let changed = model.approval(default) != approval;
        if let Ok(mut chosen) = model.approval.lock() {
            *chosen = Some(approval);
        }
        if changed && model.state.is_some() {
            self.restart_chat(chat, window, cx);
        }
    }

    /// Stars or unstars the highlighted model.
    fn toggle_highlighted_favorite(&mut self, owner: &PickerOwner, cx: &App) {
        let layout = self.model_layout(owner, cx);
        let (Some(picker), Some((layout, _))) = (self.owner_picker(owner), layout) else {
            return;
        };
        if picker.column != PickerColumn::Models {
            return;
        }
        if let Some(index) = layout.choice(picker.highlight) {
            self.toggle_favorite(owner, index, cx);
        }
    }

    /// Stars or unstars model `index` (into the model setting's choices), which stays
    /// highlighted where the list now has it.
    fn toggle_favorite(&mut self, owner: &PickerOwner, index: usize, cx: &App) {
        let Some((harness, option, _)) = self.owner_models(owner) else {
            return;
        };
        let Some(value) = option.choices.get(index).map(|choice| choice.value.clone()) else {
            return;
        };
        self.remember_models(harness, |models| models.toggle_favorite(&value));
        let Some((layout, _)) = self.model_layout(owner, cx) else {
            return;
        };
        let Some(picker) = self.owner_picker_mut(owner) else {
            return;
        };
        match layout.listed(index) {
            Some(listed) => {
                picker.highlight = listed;
                picker
                    .scroll
                    .scroll_to_item(layout.reveal(listed), ScrollStrategy::Nearest);
            }
            None => picker.highlight = picker.highlight.min(layout.choices.len().saturating_sub(1)),
        }
    }

    /// The picker's popover: the strip, the search over the model list and the
    /// thinking levels beside it, then a footer with the keys, the modes and the
    /// chat's permissions.
    fn render_model_picker(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        picker: &ModelPicker,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let border = color(colors.border);
        let shade = cx.theme().muted.opacity(0.4);
        let [models, thinking, mode] = model.picker_options();
        let remembered = self.remembered_models(chat.harness);
        let plans = model
            .state
            .as_ref()
            .map(|state| state.plans.as_slice())
            .unwrap_or_default();
        let searching = !picker.search.read(cx).value().trim().is_empty();
        let owner = PickerOwner::Chat(chat.id.clone());
        let strip = models.filter(|_| !searching).map(|option| {
            self.render_model_strip(&owner, chat.harness, option, plans, picker, Some(chat), cx)
        });
        let list = models.map(|option| {
            self.render_model_column(
                &owner,
                chat.harness,
                option,
                plans,
                picker,
                &remembered,
                "No favorites or recent models yet",
                cx,
            )
        });
        let thinking_column = thinking.map(|option| {
            let rows = option.choices.iter().enumerate().map(|(index, choice)| {
                let (id, setting, value) =
                    (chat.id.clone(), option.id.clone(), choice.value.clone());
                controls::menu_row(
                    ("chat-thinking-row", index),
                    picker.column == PickerColumn::Thinking && picker.thinking == index,
                    false,
                    cx,
                )
                .w_full()
                .h(ui_rems(ROW_HEIGHT))
                .gap_x_2()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .child(choice.name.clone()),
                )
                .when(choice.value == option.current, |row| {
                    row.child(controls::glyph(IconName::Check, muted))
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.pick_thinking(&id, setting.clone(), value.clone(), window, cx);
                    cx.notify();
                }))
            });
            let model_name = models.map(|option| option.current_name().to_owned());
            div()
                .flex_none()
                .w(ui_rems(THINKING_WIDTH))
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(border)
                .bg(shade)
                .child(column_header(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(controls::glyph(IconName::Brain, muted))
                        .child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(option.name.clone()),
                        )
                        .child(div().flex_1())
                        .when_some(model_name, |header, name| {
                            header.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                                    .text_color(muted)
                                    .child(name),
                            )
                        }),
                    border,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .px_2()
                        .pt_1p5()
                        .pb_1p5()
                        .flex()
                        .flex_col()
                        .gap(px(ROW_PITCH - ROW_HEIGHT))
                        .children(rows),
                )
        });
        let modes = mode.map(|option| {
            let segments = option.choices.iter().enumerate().map(|(index, choice)| {
                let (id, setting, value) =
                    (chat.id.clone(), option.id.clone(), choice.value.clone());
                let current = choice.value == option.current;
                let highlighted = picker.column == PickerColumn::Mode && picker.mode == index;
                div()
                    .id(("chat-mode-choice", index))
                    .h(ui_rems(24.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .text_color(if current {
                        color(modal_text_color(colors.foreground, &colors))
                    } else {
                        muted
                    })
                    .border_1()
                    .border_color(if highlighted {
                        color(colors.accent)
                    } else {
                        gpui::transparent_black()
                    })
                    .when(current, |segment| segment.bg(cx.theme().muted))
                    .hover(|segment| segment.text_color(color(colors.foreground)))
                    .child(choice.name.clone())
                    .when_some(choice.description.clone(), |segment, description| {
                        segment.tooltip(controls::tooltip(description, None, None))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_chat_menu(&id, ChatMenu::Models, false);
                        this.send_chat_option(&id, setting.clone(), value.clone(), window, cx);
                        cx.notify();
                    }))
            });
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child(option.name.clone()),
                )
                .child(
                    div()
                        .flex()
                        .rounded_lg()
                        .p(px(2.0))
                        .bg(shade)
                        .children(segments),
                )
        });
        let footer = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(border)
            .bg(shade)
            .child(picker_footnote(picker_keys(), muted))
            .children(modes)
            .child(self.render_picker_approval(chat, model, picker, shade, cx));
        let width = if models.is_some() {
            STRIP_WIDTH + LIST_WIDTH + THINKING_WIDTH
        } else {
            THINKING_WIDTH * 2.0
        };
        // Kit's popover is the frame; its padding is undone so the strip, the columns
        // and the footer reach its edges, as one panel.
        div()
            .m(gpui::rems(-0.75))
            .rounded(cx.theme().radius)
            .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
            .w(ui_rems(width))
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .h(ui_rems(BODY_HEIGHT))
                    .children(strip)
                    .children(list)
                    .children(thinking_column),
            )
            .child(footer)
            .into_any_element()
    }

    /// The footer's permissions for this chat alone: a shield per choice, the current
    /// one lit, each named in its tooltip. Dimmed, with why, while a thread cannot
    /// restart for them (`approval_refusal`).
    fn render_picker_approval(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        picker: &ModelPicker,
        shade: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let foreground = color(modal_text_color(colors.foreground, &colors));
        let current = model.approval(self.config.chat_approval);
        let running = model
            .state
            .as_ref()
            .and_then(|state| state.approval_mode.as_deref());
        let refusal = model.approval_refusal(chat);
        let segments =
            crate::config::CHAT_APPROVALS
                .into_iter()
                .enumerate()
                .map(|(index, approval)| {
                    let (icon, name) = approval_face(approval, chat.harness);
                    let lit = approval == current;
                    let highlighted =
                        picker.column == PickerColumn::Approval && picker.approval == index;
                    let mut detail = match approval {
                        ChatApproval::Harness => {
                            format!("{}'s own approval setting", chat.harness.name())
                        }
                        approval => {
                            super::super::settings::chat_approval_detail(approval).to_owned()
                        }
                    };
                    if approval == ChatApproval::Harness
                        && let Some(mode) = running
                    {
                        detail =
                            format!("{detail}; now {}", approval_mode_face(chat.harness, mode).1);
                    }
                    if let Some(refusal) = &refusal {
                        detail = refusal.clone();
                    }
                    let id = chat.id.clone();
                    div()
                        .id(("chat-approval-choice", index))
                        .h(ui_rems(24.0))
                        .px_1p5()
                        .flex()
                        .items_center()
                        .rounded_md()
                        .border_1()
                        .border_color(if highlighted {
                            color(colors.accent)
                        } else {
                            gpui::transparent_black()
                        })
                        .when(lit, |segment| segment.bg(cx.theme().muted))
                        .child(controls::glyph(icon, if lit { foreground } else { muted }))
                        .tooltip(controls::tooltip(name, Some(detail.into()), None))
                        .when(refusal.is_none(), |segment| {
                            segment.cursor_pointer().on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| {
                                    this.pick_chat_approval(&id, approval, window, cx);
                                    cx.notify();
                                },
                            ))
                        })
                });
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child("Permissions"),
            )
            .child(
                div()
                    .flex()
                    .rounded_lg()
                    .p(px(2.0))
                    .bg(shade)
                    .when(refusal.is_some(), |group| group.opacity(0.55))
                    .children(segments),
            )
            .into_any_element()
    }

    /// The search over the model list, and the list: `owner`'s models of `harness`;
    /// `empty` says why the saved tab lists none.
    #[allow(clippy::too_many_arguments)]
    fn render_model_column(
        &self,
        owner: &PickerOwner,
        harness: ChatHarness,
        option: &ChatOption,
        plans: &[ChatPlan],
        picker: &ModelPicker,
        remembered: &ChatModels,
        empty: &str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let query = picker.search.read(cx).value();
        let layout = picker_layout(option, &query, plans, remembered, &picker.tab, harness);
        let empty = if query.trim().is_empty() {
            empty
        } else {
            "No models match"
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(column_header(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(controls::glyph(IconName::Search, muted))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&picker.search).appearance(false)),
                    ),
                color(colors.border),
            ))
            .child(self.render_model_list(
                owner, harness, option, layout, picker, remembered, empty, cx,
            ))
            .children(self.render_picker_sign_in(owner, harness, picker, cx))
            .into_any_element()
    }

    /// Under a provider's models in an OMP chat's picker: whether the agent can still use
    /// its sign-in to it, and a button to sign it in, or in again.
    fn render_picker_sign_in(
        &self,
        owner: &PickerOwner,
        harness: ChatHarness,
        picker: &ModelPicker,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let (PickerOwner::Chat(chat), ChatHarness::Omp, PickerTab::Provider(Some(provider))) =
            (owner, harness, &picker.tab)
        else {
            return None;
        };
        let state = self.chat.chats.get(chat)?.state.as_ref()?;
        let colors = *self.colors();
        let muted = modal_text_color(colors.muted, &colors);
        let expired = state
            .expired_logins
            .iter()
            .find(|lost| lost.provider == *provider);
        let subscribed = state.plans.iter().any(|plan| plan.provider == *provider);
        let (text, tint) = match expired {
            Some(lost) => (format!("Sign-in expired: {}", lost.reason), colors.error),
            None if subscribed => ("Signed in with a subscription".to_owned(), muted),
            None => (
                "Sign in with a subscription, or use an API key".to_owned(),
                muted,
            ),
        };
        let label = if expired.is_some() || subscribed {
            "Sign in again"
        } else {
            "Sign in"
        };
        let (id, provider) = (chat.clone(), provider.clone());
        let button = controls::text_button("chat-picker-sign-in", label).on_click(cx.listener(
            move |this, _: &ClickEvent, window, cx| {
                if let Some(model) = this.chat.chats.get_mut(&id) {
                    model.menu = None;
                }
                this.sign_in_chat(&id, provider.clone(), window, cx);
                cx.notify();
            },
        ));
        Some(
            div()
                .flex_none()
                .mx_3()
                .py_2()
                .border_t_1()
                .border_color(color(colors.border))
                .flex()
                .items_center()
                .gap_2()
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .child(div().flex_1().min_w_0().text_color(color(tint)).child(text))
                .child(button)
                .into_any_element(),
        )
    }

    /// The strip: your saved models' star, then a mark per provider, the shown one lit
    /// with a bar; each one's tooltip names it and how it is paid. A `draft` chat's
    /// strip leads with the agents it can start with instead.
    #[allow(clippy::too_many_arguments)]
    fn render_model_strip(
        &self,
        owner: &PickerOwner,
        harness: ChatHarness,
        option: &ChatOption,
        plans: &[ChatPlan],
        picker: &ModelPicker,
        draft: Option<&ChatContent>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let foreground = color(modal_text_color(colors.foreground, &colors));
        let accent = color(colors.accent);
        let lit = cx.theme().muted;
        let keyboard = picker.column == PickerColumn::Strip;
        let tab = |index: usize,
                   tab: PickerTab,
                   icon: AnyElement,
                   name: String,
                   detail: Option<String>| {
            let selected = picker.tab == tab;
            let owner = owner.clone();
            div()
                .id(("chat-model-tab", index))
                .relative()
                .w_full()
                .h(ui_rems(STRIP_WIDTH - 8.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .text_color(if selected { foreground } else { muted })
                .hover(move |button| button.bg(lit).text_color(foreground))
                .when(selected, |button| button.bg(lit))
                .when(selected && keyboard, |button| {
                    button.border_1().border_color(accent)
                })
                .child(icon)
                .when(selected, |button| {
                    button.child(
                        div()
                            .absolute()
                            .right(px(-4.0))
                            .top(ui_rems(8.0))
                            .w(px(3.0))
                            .h(ui_rems(STRIP_WIDTH - 24.0))
                            .rounded_full()
                            .bg(accent),
                    )
                })
                .tooltip(controls::tooltip(
                    name,
                    detail.map(SharedString::from),
                    None,
                ))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.show_model_tab(&owner, Some(tab.clone()), cx);
                    if let Some(picker) = this.owner_picker_mut(&owner) {
                        picker.column = PickerColumn::Models;
                        window.focus(&picker.search.focus_handle(cx), cx);
                    }
                    cx.notify();
                }))
        };
        let saved = tab(
            0,
            PickerTab::Saved,
            controls::glyph(IconName::StarFill, muted).into_any_element(),
            "Favorites and recent".to_owned(),
            None,
        );
        // A draft can still change agent: one mark per installed agent heads the strip,
        // its own lit, another starting the draft again with that agent.
        let agents: Vec<AnyElement> = match draft.filter(|chat| self.chat_draft(chat) == Some(true))
        {
            Some(chat) => self
                .installed_agents(&chat.environment)
                .into_iter()
                .enumerate()
                .map(|(index, agent)| {
                    let current = agent == chat.harness;
                    let id = chat.id.clone();
                    div()
                        .id(("chat-agent-tab", index))
                        .w_full()
                        .h(ui_rems(STRIP_WIDTH - 8.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .child(img(harness_mark(agent)).flex_none().size(ui_rems(18.0)))
                        .when(current, |button| button.bg(lit))
                        .when(!current, |button| {
                            button
                                .cursor_pointer()
                                .opacity(0.55)
                                .hover(move |button| button.bg(lit).opacity(1.0))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.switch_draft_agent(&id, agent, window, cx);
                                    cx.notify();
                                }))
                        })
                        .tooltip(controls::tooltip(
                            agent.name(),
                            Some(if current {
                                "This draft's agent".into()
                            } else {
                                "Start this draft with it instead; what you typed stays".into()
                            }),
                            None,
                        ))
                        .into_any_element()
                })
                .collect(),
            None => Vec::new(),
        };
        let own_models = harness.entry().own_models;
        let providers =
            providers(option, plans)
                .into_iter()
                .enumerate()
                .map(|(index, provider)| {
                    let name = provider_title(provider, harness);
                    let icon = match (provider.and_then(model_icons::for_provider), provider) {
                        (Some(path), _) => mark(Some(path), 18.0, muted),
                        // The harness's own models: its mark, in its colours.
                        (None, None) if own_models => img(harness_mark(harness))
                            .flex_none()
                            .size(ui_rems(18.0))
                            .into_any_element(),
                        (None, _) => div()
                            .size(ui_rems(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .border_1()
                            .border_color(color(colors.border))
                            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name.chars().next().map(String::from).unwrap_or_default())
                            .into_any_element(),
                    };
                    tab(
                        index + 1,
                        PickerTab::Provider(provider.map(str::to_owned)),
                        icon,
                        name,
                        plan_detail(plans, provider),
                    )
                });
        div()
            .id("chat-model-strip")
            .flex_none()
            .w(ui_rems(STRIP_WIDTH))
            .h_full()
            .overflow_y_scroll()
            .bg(cx.theme().muted.opacity(0.3))
            .border_r_1()
            .border_color(color(colors.border))
            .p_1()
            .flex()
            .flex_col()
            .gap_1()
            .when(!agents.is_empty(), |strip| {
                strip
                    .children(agents)
                    .child(div().mx_1().border_b_1().border_color(color(colors.border)))
            })
            .child(saved)
            .child(div().mx_1().border_b_1().border_color(color(colors.border)))
            .children(providers)
            .into_any_element()
    }

    /// The model list: models with their marks, the current one checked, favorites
    /// starred (others show the star on hover); it fades at an edge with more beyond.
    /// With none, `empty` says why.
    #[allow(clippy::too_many_arguments)]
    fn render_model_list(
        &self,
        owner: &PickerOwner,
        harness: ChatHarness,
        option: &ChatOption,
        layout: PickerLayout,
        picker: &ModelPicker,
        remembered: &ChatModels,
        empty: &str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        if layout.choices.is_empty() {
            return div()
                .flex_1()
                .px_4()
                .pt_3()
                .text_color(muted)
                .child(empty.to_owned())
                .into_any_element();
        }
        let highlighted = (picker.column == PickerColumn::Models)
            .then(|| {
                layout
                    .choices
                    .get(picker.highlight.min(layout.choices.len() - 1))
                    .copied()
            })
            .flatten();
        let count = layout.rows.len();
        let rows = Rc::new(layout.rows);
        let choices = Rc::new(option.choices.clone());
        let favorites: Rc<[String]> = remembered.favorites.clone().into();
        let saved_tab = picker.tab == PickerTab::Saved;
        let current = option.current.clone();
        let (owner, setting) = (owner.clone(), option.id.clone());
        let list = uniform_list(
            "chat-model-list",
            count,
            cx.processor(move |_, range: Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let body = match &rows[row] {
                            PickerRow::Label { title, detail } => div()
                                .size_full()
                                .px_2()
                                .flex()
                                .items_end()
                                .pb_1()
                                .gap_2()
                                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                                .text_color(muted)
                                .child(
                                    div()
                                        .flex_none()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title.clone()),
                                )
                                .when_some(detail.clone(), |label, detail| {
                                    label.child(div().min_w_0().truncate().child(detail))
                                })
                                .into_any_element(),
                            PickerRow::Choice {
                                index,
                                provider,
                                id: show_id,
                            } => {
                                let choice = &choices[*index];
                                let favorite = favorites.contains(&choice.value);
                                let lit = Some(row) == highlighted;
                                let detail = [
                                    provider
                                        .then(|| choice_provider(choice).map(provider_name))
                                        .flatten(),
                                    show_id.then(|| choice_id(choice).to_owned()),
                                ]
                                .into_iter()
                                .flatten()
                                .collect::<Vec<_>>()
                                .join(" · ");
                                // In the saved tab every favorite is starred, so its star
                                // shows only on hover, as others' do everywhere.
                                let star_shown = lit || (favorite && !saved_tab);
                                let (pick_owner, pick_setting, pick_value) =
                                    (owner.clone(), setting.clone(), choice.value.clone());
                                let (star_owner, star_index) = (owner.clone(), *index);
                                controls::menu_row(("chat-model-row", row), lit, false, cx)
                                    .group("chat-model-row")
                                    .w_full()
                                    .h(ui_rems(ROW_HEIGHT))
                                    .gap_x_2()
                                    .child(mark(model_mark(choice, harness), ICON_SIZE, muted))
                                    .child(div().flex_none().child(choice.name.clone()))
                                    .when(choice.value == current, |row| {
                                        row.child(controls::glyph(IconName::Check, muted))
                                    })
                                    .when(!detail.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                                                .text_color(muted)
                                                .child(detail),
                                        )
                                    })
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .flex_none()
                                            .opacity(if star_shown { 1. } else { 0. })
                                            .group_hover("chat-model-row", |star| star.opacity(1.))
                                            .child(
                                                icon_button(
                                                    ("chat-model-star", row),
                                                    if favorite {
                                                        IconName::StarFill
                                                    } else {
                                                        IconName::Star
                                                    },
                                                    if favorite {
                                                        "Remove from favorites"
                                                    } else {
                                                        "Add to favorites"
                                                    },
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _: &ClickEvent, _, cx| {
                                                        this.toggle_favorite(
                                                            &star_owner,
                                                            star_index,
                                                            cx,
                                                        );
                                                        cx.stop_propagation();
                                                        cx.notify();
                                                    },
                                                )),
                                            ),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.pick_model(
                                                &pick_owner,
                                                pick_setting.clone(),
                                                pick_value.clone(),
                                                window,
                                                cx,
                                            );
                                            cx.notify();
                                        },
                                    ))
                                    .into_any_element()
                            }
                        };
                        div()
                            .h(ui_rems(ROW_PITCH))
                            .pb(px(ROW_PITCH - ROW_HEIGHT))
                            .child(body)
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(&picker.scroll);
        // Fades say the list goes on past an edge.
        let (offset, max) = {
            let state = picker.scroll.0.borrow();
            (
                state.base_handle.offset().y,
                state.base_handle.max_offset().y,
            )
        };
        let surface = cx.theme().popover;
        let fade = |angle: f32| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .h(ui_rems(16.0))
                .bg(linear_gradient(
                    angle,
                    linear_color_stop(surface, 0.),
                    linear_color_stop(surface.opacity(0.), 1.),
                ))
        };
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .px_2()
            .pt_1p5()
            .pb_1p5()
            .child(list)
            .when(offset < px(-0.5), |list| list.child(fade(180.).top_0()))
            .when(-offset < max - px(0.5), |list| {
                list.child(fade(0.).bottom_0())
            })
            .into_any_element()
    }

    /// Opens the model picker of Settings → Projects' New threads for `project`: the
    /// models of the agent its new threads start with, on your saved models' tab (or the
    /// project model's provider) with its model highlighted. Open for it already, its
    /// search takes the keyboard again.
    pub(in crate::gui::workspace) fn open_project_model_picker(
        &mut self,
        project: &Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(open) = &self.chat.project_picker
            && open.project == *project
        {
            window.focus(&open.picker.search.focus_handle(cx), cx);
            return;
        }
        let harness = self.new_thread_agent(Some(project));
        let remembered = self.remembered_models(harness);
        let current = self
            .project_settings(project)
            .and_then(|settings| settings.model.clone());
        let listed = self.chat.listed.get(harness);
        let option = ChatOption {
            id: "model".to_owned(),
            name: "Model".to_owned(),
            category: Some("model".to_owned()),
            choices: project_choices(listed, &remembered, current.as_deref()),
            current: current.unwrap_or_default(),
        };
        let plans = listed.map_or_else(Vec::new, |listed| {
            listed
                .plans
                .iter()
                .map(|provider| ChatPlan {
                    provider: provider.clone(),
                    limits: Vec::new(),
                })
                .collect()
        });
        let unlisted = listed.is_none();
        let tab = opening_tab(&option, &remembered);
        let (search, events) = Self::picker_search(PickerOwner::Project, window, cx);
        self.chat.project_picker = Some(ProjectPicker {
            project: project.clone(),
            harness,
            option,
            plans,
            unlisted,
            picker: ModelPicker {
                search,
                _events: events,
                tab: tab.clone(),
                column: PickerColumn::Models,
                highlight: 0,
                thinking: 0,
                mode: 0,
                approval: 0,
                scroll: UniformListScrollHandle::new(),
                claim_keyboard: CLAIM_FRAMES,
            },
        });
        self.show_model_tab(&PickerOwner::Project, Some(tab), cx);
    }

    /// Closes the project model picker.
    pub(in crate::gui::workspace) fn close_project_model_picker(&mut self) {
        self.chat.project_picker = None;
    }

    /// The project model picker's search, while it is open.
    pub(in crate::gui::workspace) fn project_model_search(&self) -> Option<&Entity<InputState>> {
        self.chat
            .project_picker
            .as_ref()
            .map(|project| &project.picker.search)
    }

    /// While the project model picker opens, gives its search the keyboard.
    pub(in crate::gui::workspace) fn prepare_project_model_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = self
            .chat
            .project_picker
            .as_mut()
            .map(|project| &mut project.picker)
            && picker.claim_keyboard > 0
        {
            picker.claim_keyboard -= 1;
            let search = picker.search.focus_handle(cx);
            if !search.is_focused(window) {
                window.focus(&search, cx);
            }
            cx.notify();
        }
    }

    /// New threads' model button for `project`: the model's mark and name, or its
    /// agent's own model when none is set, opening the picker under it. `on_open` runs
    /// first when a click opens it.
    pub(in crate::gui::workspace) fn render_project_model_button(
        &self,
        project: &Project,
        on_open: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) -> AnyElement {
        let harness = self.new_thread_agent(Some(project));
        let model = self
            .project_settings(project)
            .and_then(|settings| settings.model.as_deref());
        let open = self
            .chat
            .project_picker
            .as_ref()
            .is_some_and(|picker| picker.project == *project);
        let choice = model.map(|model| {
            self.chat
                .listed
                .get(harness)
                .and_then(|listed| listed.choices.iter().find(|choice| choice.value == model))
                .cloned()
                .unwrap_or_else(|| unlisted_choice(model))
        });
        let label = match &choice {
            Some(choice) => choice.name.clone(),
            None => format!("{}'s default", harness.name()),
        };
        let mut button = text_button("project-model", label)
            .header()
            .menu_open(open)
            .tooltip(format!("{}'s model for new threads", harness.name()))
            .detail(match &choice {
                Some(choice) => choice.value.clone(),
                None => format!("The model {} picks itself", harness.name()),
            });
        if let Some(path) = choice
            .as_ref()
            .and_then(|choice| model_mark(choice, harness))
        {
            button = button.icon(Icon::empty().path(path));
        }
        let opening = project.clone();
        let on_open_change = cx.listener(move |this, open: &bool, window, cx| {
            if *open {
                on_open(this, window, cx);
                this.open_project_model_picker(&opening, window, cx);
            } else {
                this.close_project_model_picker();
            }
            cx.notify();
        });
        let keys = {
            let app = cx.entity().downgrade();
            move |key: &Keystroke, window: &mut Window, cx: &mut App| {
                if let Some(app) = app.upgrade() {
                    app.update(cx, |this, cx| {
                        this.model_picker_key(&PickerOwner::Project, key, window, cx);
                    });
                }
            }
        };
        let content = self
            .chat
            .project_picker
            .as_ref()
            .filter(|_| open)
            .map(|picker| self.render_project_model_picker(picker, cx));
        let popover = controls::popover("project-model-menu", button, open)
            .anchor(Anchor::TopRight)
            .offset(px(GAP))
            .on_open_change(on_open_change)
            .on_key(keys);
        match content {
            Some(content) => popover.content(content).into_any_element(),
            None => popover.into_any_element(),
        }
    }

    /// The project model picker's popover: the strip and the searched model list, then
    /// a footer with the keys (or that the agent never listed its models) and its
    /// agent's own model.
    fn render_project_model_picker(
        &self,
        project: &ProjectPicker,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let picker = &project.picker;
        let (owner, harness, name) = (
            PickerOwner::Project,
            project.harness,
            project.harness.name(),
        );
        let remembered = self.remembered_models(harness);
        let searching = !picker.search.read(cx).value().trim().is_empty();
        let strip = (!searching).then(|| {
            self.render_model_strip(
                &owner,
                harness,
                &project.option,
                &project.plans,
                picker,
                None,
                cx,
            )
        });
        let empty = if project.unlisted {
            format!("Open a chat with {name} once to list its models.")
        } else {
            "No favorites or recent models yet".to_owned()
        };
        let list = self.render_model_column(
            &owner,
            harness,
            &project.option,
            &project.plans,
            picker,
            &remembered,
            &empty,
            cx,
        );
        let note = if project.unlisted {
            format!("Only saved models: open a chat with {name} once to list them all")
        } else {
            picker_keys()
        };
        let footer = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(color(colors.border))
            .bg(cx.theme().muted.opacity(0.4))
            .child(picker_footnote(note, muted))
            .child(
                text_button("project-model-default", format!("{name}'s default"))
                    .ghost()
                    .selected(project.option.current.is_empty())
                    .detail(format!(
                        "New threads start on the model {name} picks itself"
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.pick_project_model(None, cx);
                        cx.notify();
                    })),
            );
        div()
            .m(gpui::rems(-0.75))
            .rounded(cx.theme().radius)
            .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
            .w(ui_rems(STRIP_WIDTH + LIST_WIDTH))
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .h(ui_rems(BODY_HEIGHT))
                    .children(strip)
                    .child(list),
            )
            .child(footer)
            .into_any_element()
    }
}

/// The picker's keys, as its footer lists them.
fn picker_keys() -> String {
    format!(
        "↑↓ move · ←→ column · ↵ pick · {} star",
        if cfg!(target_os = "macos") {
            "⌘D"
        } else {
            "Ctrl+D"
        }
    )
}

/// A muted line at the left of a picker's footer, taking the room left.
fn picker_footnote(text: String, muted: Hsla) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
        .text_color(muted)
        .child(text)
}

/// The picker's shortcut as the tooltip names it.
fn picker_key_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘⇧M"
    } else {
        "Ctrl+Shift+M"
    }
}

/// Whether `key` opens the model picker: Ctrl+Shift+M (Cmd+Shift+M on macOS).
pub(super) fn opens_picker(key: &Keystroke) -> bool {
    let modifiers = &key.modifiers;
    let primary = if cfg!(target_os = "macos") {
        modifiers.platform && !modifiers.control
    } else {
        modifiers.control && !modifiers.platform
    };
    key.key == "m" && primary && modifiers.shift && !modifiers.alt
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::chat::ChatPlanLimit;

    fn choice(value: &str) -> ChatChoice {
        ChatChoice {
            value: value.into(),
            name: value.rsplit('/').next().unwrap().into(),
            description: None,
            group: None,
        }
    }

    fn option() -> ChatOption {
        ChatOption {
            id: "model".into(),
            name: "Model".into(),
            category: Some("model".into()),
            current: "openrouter/glm".into(),
            choices: vec![
                choice("amazon-bedrock/haiku"),
                choice("anthropic/opus"),
                choice("openrouter/glm"),
                choice("openai-codex/gpt"),
                choice("anthropic/sonnet"),
            ],
        }
    }

    fn shown(option: &ChatOption, layout: &PickerLayout) -> Vec<String> {
        layout
            .rows
            .iter()
            .map(|row| match row {
                PickerRow::Label { title, detail } => {
                    format!("# {title} ({})", detail.as_deref().unwrap_or_default())
                }
                PickerRow::Choice { index, id, .. } => {
                    let value = option.choices[*index].value.clone();
                    if *id { format!("{value} [id]") } else { value }
                }
            })
            .collect()
    }

    fn plans() -> [ChatPlan; 2] {
        [
            ChatPlan {
                provider: "openai-codex".into(),
                limits: Vec::new(),
            },
            ChatPlan {
                provider: "anthropic".into(),
                limits: vec![
                    ChatPlanLimit {
                        label: "5 Hour".into(),
                        left_percent: 60,
                    },
                    ChatPlanLimit {
                        label: "7 Day".into(),
                        left_percent: 80,
                    },
                ],
            },
        ]
    }

    #[test]
    fn a_projects_picker_lists_what_the_agent_listed_then_saved_models_it_lacks() {
        let listed = AgentModels {
            choices: vec![choice("anthropic/opus"), choice("openai/gpt")],
            plans: Vec::new(),
        };
        let mut remembered = ChatModels::default();
        remembered.toggle_favorite("anthropic/opus");
        remembered.toggle_favorite("google/gemini");
        remembered.picked_model("xai/grok");
        let values = |choices: Vec<ChatChoice>| -> Vec<String> {
            choices.into_iter().map(|choice| choice.value).collect()
        };
        assert_eq!(
            values(project_choices(
                Some(&listed),
                &remembered,
                Some("deepseek/r2")
            )),
            [
                "anthropic/opus",
                "openai/gpt",
                "google/gemini",
                "xai/grok",
                "deepseek/r2"
            ]
        );
        // Never listed: only what Compi remembers, named by its ID.
        let unlisted = project_choices(None, &remembered, None);
        assert_eq!(
            values(unlisted.clone()),
            ["anthropic/opus", "google/gemini", "xai/grok"]
        );
        assert_eq!(unlisted[1].name, "gemini");
        assert_eq!(choice_provider(&unlisted[1]), Some("google"));
    }

    #[test]
    fn the_strip_lists_subscriptions_first_then_the_current_provider_then_by_name() {
        let option = option();
        let tabs = strip_tabs(&option, &plans());
        let names: Vec<String> = tabs
            .iter()
            .map(|tab| match tab {
                PickerTab::Saved => "saved".to_owned(),
                PickerTab::Provider(provider) => provider.clone().unwrap_or_default(),
            })
            .collect();
        assert_eq!(
            names,
            [
                "saved",
                "anthropic",
                "openai-codex",
                "openrouter",
                "amazon-bedrock"
            ]
        );
        let layout = picker_layout(
            &option,
            "",
            &plans(),
            &ChatModels::default(),
            &PickerTab::Provider(Some("anthropic".into())),
            ChatHarness::Omp,
        );
        assert_eq!(
            shown(&option, &layout),
            [
                "# Anthropic (Subscription · 5 Hour: 60% left)",
                "anthropic/opus",
                "anthropic/sonnet",
            ]
        );
        // Arrow keys step through models only.
        assert_eq!(layout.choice(1), Some(4));
        assert_eq!(layout.listed(4), Some(1));
    }

    #[test]
    fn the_saved_tab_lists_favorites_then_recent_once_and_search_spans_every_provider() {
        let option = option();
        let remembered = ChatModels {
            favorites: vec!["anthropic/sonnet".into(), "gone/model".into()],
            recents: vec![
                "anthropic/sonnet".into(),
                "openrouter/glm".into(),
                "amazon-bedrock/haiku".into(),
            ],
            ..ChatModels::default()
        };
        assert_eq!(opening_tab(&option, &remembered), PickerTab::Saved);
        let layout = picker_layout(
            &option,
            "",
            &[],
            &remembered,
            &PickerTab::Saved,
            ChatHarness::Omp,
        );
        assert_eq!(
            shown(&option, &layout),
            [
                "# Favorites ()",
                "anthropic/sonnet",
                "# Recent ()",
                "openrouter/glm",
                "amazon-bedrock/haiku",
            ]
        );
        // A search ignores the tab and lists matches in the strip's order, unlabelled
        // ("o" is in every provider's name).
        let layout = picker_layout(
            &option,
            "o",
            &plans(),
            &remembered,
            &PickerTab::Saved,
            ChatHarness::Omp,
        );
        assert_eq!(
            shown(&option, &layout),
            [
                "anthropic/opus",
                "anthropic/sonnet",
                "openai-codex/gpt",
                "openrouter/glm",
                "amazon-bedrock/haiku",
            ]
        );
        // With nothing saved, the picker opens on the current model's provider.
        assert_eq!(
            opening_tab(&option, &ChatModels::default()),
            PickerTab::Provider(Some("openrouter".into()))
        );
    }

    #[test]
    fn models_sharing_a_name_show_their_ids() {
        let mut option = option();
        option.choices.push(ChatChoice {
            name: "opus".into(),
            ..choice("anthropic/opus-20250929")
        });
        let layout = picker_layout(
            &option,
            "",
            &[],
            &ChatModels::default(),
            &PickerTab::Provider(Some("anthropic".into())),
            ChatHarness::Omp,
        );
        assert_eq!(
            shown(&option, &layout),
            [
                "# Anthropic (Pay per use)",
                "anthropic/opus [id]",
                "anthropic/sonnet",
                "anthropic/opus-20250929 [id]",
            ]
        );
        assert_eq!(choice_id(&option.choices[5]), "opus-20250929");
    }

    #[test]
    fn picking_a_model_leads_the_recent_and_stars_toggle() {
        let mut models = ChatModels::default();
        for value in ["a", "b", "c", "d", "e", "f", "b"] {
            models.picked_model(value);
        }
        assert_eq!(models.recents, ["b", "f", "e", "d", "c"]);
        assert_eq!(models.picks.model.as_deref(), Some("b"));
        models.toggle_favorite("a");
        models.toggle_favorite("c");
        models.toggle_favorite("a");
        assert_eq!(models.favorites, ["c"]);
    }

    #[test]
    fn claude_codes_models_name_no_provider_and_list_under_it() {
        let named = |value: &str, name: &str| ChatChoice {
            name: name.into(),
            ..choice(value)
        };
        let option = ChatOption {
            current: "default".into(),
            choices: vec![
                named("default", "Default (recommended)"),
                named("opus", "Opus"),
                named("haiku", "Haiku"),
            ],
            ..option()
        };
        let claude = ChatHarness::Claude;
        // One provider, the harness's own, with no plan to hover.
        assert_eq!(
            strip_tabs(&option, &[]),
            [PickerTab::Saved, PickerTab::Provider(None)]
        );
        assert_eq!(
            opening_tab(&option, &ChatModels::default()),
            PickerTab::Provider(None)
        );
        let layout = picker_layout(
            &option,
            "",
            &[],
            &ChatModels::default(),
            &PickerTab::Provider(None),
            claude,
        );
        assert_eq!(
            shown(&option, &layout),
            ["# Claude Code ()", "default", "opus", "haiku"]
        );
        // OMP's models without a provider stay Other.
        assert_eq!(provider_title(None, ChatHarness::Omp), "Other");
        // Search, favorites and recents work on the bare IDs.
        let layout = picker_layout(
            &option,
            "opus",
            &[],
            &ChatModels::default(),
            &PickerTab::Saved,
            claude,
        );
        assert_eq!(shown(&option, &layout), ["opus"]);
        let mut remembered = ChatModels::default();
        remembered.picked_model("haiku");
        remembered.toggle_favorite("opus");
        let layout = picker_layout(&option, "", &[], &remembered, &PickerTab::Saved, claude);
        assert_eq!(
            shown(&option, &layout),
            ["# Favorites ()", "opus", "# Recent ()", "haiku"]
        );
        // Every model wears Claude's mark, named or not.
        assert!(
            option
                .choices
                .iter()
                .all(|choice| model_mark(choice, claude) == Some("models/claude.svg"))
        );
        assert_eq!(model_mark(&option.choices[0], ChatHarness::Omp), None);
        // Codex's own models wear OpenAI's mark, named for it or not.
        for choice in [named("gpt-5.5", "GPT-5.5"), named("custom", "Custom")] {
            assert_eq!(
                model_mark(&choice, ChatHarness::Codex),
                Some("models/openai.svg")
            );
        }
        assert_eq!(provider_title(None, ChatHarness::Codex), "Codex");
    }

    #[test]
    fn permissions_name_each_harness_own_modes() {
        let (omp, claude) = (ChatHarness::Omp, ChatHarness::Claude);
        assert_eq!(
            approval_face(ChatApproval::Harness, omp).1,
            "OMP's own setting"
        );
        assert_eq!(
            approval_face(ChatApproval::Harness, claude).1,
            "Same as Claude Code"
        );
        for (mode, name) in [
            ("default", "Ask before anything"),
            ("acceptEdits", "Edits allowed"),
            ("plan", "Plan"),
            ("bypassPermissions", "Full access"),
            ("dontAsk", "Same as Claude Code"),
        ] {
            assert_eq!(approval_mode_face(claude, mode).1, name, "{mode}");
        }
        assert_eq!(approval_mode_face(omp, "write").1, "Edits allowed");
        assert_eq!(
            approval_mode_face(omp, "always-ask-cached"),
            (IconName::ShieldCheck, "Ask once per tool")
        );
        assert_eq!(
            approval_mode_face(omp, "acceptEdits").1,
            "OMP's own setting"
        );
        let codex = ChatHarness::Codex;
        assert_eq!(
            approval_face(ChatApproval::Harness, codex).1,
            "Same as Codex"
        );
        for (mode, face) in [
            (
                "untrusted/read-only",
                (IconName::ShieldCheck, "Ask before anything"),
            ),
            (
                "on-request/workspace-write",
                (IconName::ShieldHalf, "Edits allowed"),
            ),
            (
                "never/danger-full-access",
                (IconName::ShieldOff, "Full access"),
            ),
            ("on-request/read-only", (IconName::ShieldCheck, "Read only")),
            (
                "on-request/danger-full-access",
                (IconName::ShieldOff, "Full access"),
            ),
            (
                "granular/workspace-write",
                (IconName::Shield, "Same as Codex"),
            ),
        ] {
            assert_eq!(approval_mode_face(codex, mode), face, "{mode}");
        }
        assert_eq!(
            approval_shown(ChatApproval::Harness, claude, Some("plan")).1,
            "Plan"
        );
        assert_eq!(
            approval_shown(ChatApproval::Yolo, claude, Some("plan")).1,
            "Full access"
        );
    }

    #[test]
    fn the_shield_shows_only_for_permissions_other_than_the_normal_ones() {
        for default in crate::config::CHAT_APPROVALS {
            // A chat that never picked follows the normal setting: no shield.
            assert!(!approval_differs(None, default));
            for chosen in crate::config::CHAT_APPROVALS {
                assert_eq!(approval_differs(Some(chosen), default), chosen != default);
            }
        }
    }
}
