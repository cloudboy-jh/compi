//! The model picker: one popover, opening above the composer, for the harness's model,
//! thinking level and mode. A strip of tabs on the left picks what the model list shows:
//! your favorites and recent picks, or one provider's models; typing searches every
//! model instead. The thinking levels sit beside the list and the modes in the footer.
//! Picking a model moves the keyboard to the thinking levels; picking a level or a mode
//! closes it. Picks are remembered per harness for its new sessions (`ChatModels`).
//! Behaviour, strip and spacing follow T3 Code's picker (decided 2026-11-19).
use super::*;
use crate::config::ChatModels;
use crate::gui::model_icons;
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
    scroll: UniformListScrollHandle,
    /// Frames left in which the search takes the keyboard back: the popover takes it
    /// for itself a frame or two after opening.
    claim_keyboard: u8,
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
fn provider_name(provider: &str) -> String {
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
                    provider.map_or_else(|| "Other".to_owned(), provider_name),
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

/// The mark drawn for model `choice`: its family's, else its provider's.
fn model_mark(choice: &ChatChoice) -> Option<&'static str> {
    model_icons::for_model(&choice.value, &choice.name)
        .or_else(|| choice_provider(choice).and_then(model_icons::for_provider))
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
        self.sync_chat_picks();
    }

    /// The composer row's model button: the model's mark and name, then a brain and the
    /// thinking level, opening the picker above the composer; and the mode when it is not
    /// the first (Plan, say).
    pub(super) fn render_chat_models(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let [models, thinking, mode] = model.picker_options();
        if models.is_none() && thinking.is_none() && mode.is_none() {
            return Vec::new();
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
        if let Some(path) = current_model.and_then(model_mark) {
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
            let (app, id) = (cx.entity().downgrade(), chat.id.clone());
            move |key: &Keystroke, window: &mut Window, cx: &mut App| {
                if let Some(app) = app.upgrade() {
                    app.update(cx, |this, cx| this.model_picker_key(&id, key, window, cx));
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
        shown
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
        let remembered = self
            .chat
            .chats
            .get(chat)
            .map(|model| self.remembered_models(model.harness()))
            .unwrap_or_default();
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
        model.menu = Some(ChatMenu::Models);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        let id = chat.to_owned();
        let events = cx.subscribe_in(
            &search,
            window,
            move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.show_model_tab(&id, None, cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.model_picker_enter(&id, window, cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        window.focus(&search.focus_handle(cx), cx);
        model.picker = Some(ModelPicker {
            search,
            _events: events,
            tab: tab.clone(),
            column,
            highlight: 0,
            thinking,
            mode,
            scroll: UniformListScrollHandle::new(),
            claim_keyboard: CLAIM_FRAMES,
        });
        self.show_model_tab(chat, Some(tab), cx);
    }

    /// Shows tab `tab` (or, with `None`, the search's results in the current one) with
    /// the current model highlighted when listed, else the first, in view.
    fn show_model_tab(&mut self, chat: &str, tab: Option<PickerTab>, cx: &App) {
        if let Some(tab) = tab
            && let Some(picker) = self
                .chat
                .chats
                .get_mut(chat)
                .and_then(|model| model.picker.as_mut())
        {
            picker.tab = tab;
        }
        let listed = self
            .model_layout(chat, cx)
            .map(|(layout, current)| (layout.listed(current), layout));
        let Some(picker) = self
            .chat
            .chats
            .get_mut(chat)
            .and_then(|model| model.picker.as_mut())
        else {
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

    /// The open picker's model list for chat `chat`, and the current model's index.
    fn model_layout(&self, chat: &str, cx: &App) -> Option<(PickerLayout, usize)> {
        let model = self.chat.chats.get(chat)?;
        let picker = model.picker.as_ref()?;
        let state = model.state.as_ref()?;
        let option = state.option("model")?;
        let layout = picker_layout(
            option,
            &picker.search.read(cx).value(),
            &state.plans,
            &self.remembered_models(state.harness),
            &picker.tab,
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
    /// modes, Left and Right pick between them); Enter picks; Ctrl+D (Cmd+D) stars the
    /// highlighted model.
    fn model_picker_key(
        &mut self,
        chat: &str,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key.key == "enter" {
            // With the search holding the keyboard, its own Enter (`PressEnter`) picks.
            let searching = self
                .chat
                .chats
                .get(chat)
                .and_then(|model| model.picker.as_ref())
                .is_some_and(|picker| picker.search.focus_handle(cx).is_focused(window));
            if !searching {
                self.model_picker_enter(chat, window, cx);
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
            && let Some(picker) = self
                .chat
                .chats
                .get(chat)
                .and_then(|model| model.picker.as_ref())
        {
            let search = picker.search.clone();
            window.focus(&search.focus_handle(cx), cx);
            search.update(cx, |search, cx| search.insert(text.to_owned(), window, cx));
            self.show_model_tab(chat, None, cx);
            cx.notify();
            return;
        }
        let favorite_modifier = if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        };
        if key.key == "d" && favorite_modifier && !modifiers.alt && !modifiers.shift {
            self.toggle_highlighted_favorite(chat, cx);
            cx.notify();
            return;
        }
        let layout = self.model_layout(chat, cx);
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        let Some(picker) = model.picker.as_ref() else {
            return;
        };
        let [models, thinking, mode] = model.picker_options();
        let searching = !picker.search.read(cx).value().trim().is_empty();
        let tabs = models
            .zip(model.state.as_ref())
            .map(|(option, state)| strip_tabs(option, &state.plans))
            .unwrap_or_default();
        let columns: Vec<PickerColumn> = [
            (models.is_some() && !searching).then_some(PickerColumn::Strip),
            models.map(|_| PickerColumn::Models),
            thinking.map(|_| PickerColumn::Thinking),
            mode.map(|_| PickerColumn::Mode),
        ]
        .into_iter()
        .flatten()
        .collect();
        let (thinking_count, mode_count) = (
            thinking.map_or(0, |option| option.choices.len()),
            mode.map_or(0, |option| option.choices.len()),
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
            self.show_model_tab(chat, Some(next), cx);
            cx.notify();
            return;
        }
        let Some(picker) = self
            .chat
            .chats
            .get_mut(chat)
            .and_then(|model| model.picker.as_mut())
        else {
            return;
        };
        match (column, key.key.as_str(), move_by) {
            (_, "tab", _) => picker.column = step_column(!modifiers.shift),
            (PickerColumn::Mode, "left", _) => picker.mode = step(picker.mode, mode_count, -1),
            (PickerColumn::Mode, "right", _) => picker.mode = step(picker.mode, mode_count, 1),
            (PickerColumn::Mode, "up", _) => picker.column = step_column(false),
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
    /// moves to the thinking levels; on a level or mode, picks it and closes.
    fn model_picker_enter(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.model_layout(chat, cx);
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let [models, thinking, mode] = model.picker_options();
        let Some(picker) = model.picker.as_ref() else {
            return;
        };
        match picker.column {
            PickerColumn::Strip => {
                if let Some(picker) = model.picker.as_mut() {
                    picker.column = PickerColumn::Models;
                }
            }
            PickerColumn::Models => {
                let Some((value, option)) = layout.and_then(|(layout, _)| {
                    let models = models?;
                    let listed = picker.highlight.min(layout.choices.len().checked_sub(1)?);
                    let index = layout.choice(listed)?;
                    Some((models.choices[index].value.clone(), models.id.clone()))
                }) else {
                    return;
                };
                self.pick_model(chat, option, value, window, cx);
            }
            PickerColumn::Thinking => {
                let Some(option) = thinking else {
                    return;
                };
                let Some(choice) = option.choices.get(picker.thinking) else {
                    return;
                };
                let (option, value) = (option.id.clone(), choice.value.clone());
                self.pick_thinking(chat, option, value, window, cx);
            }
            PickerColumn::Mode => {
                let Some(option) = mode else {
                    return;
                };
                let Some(choice) = option.choices.get(picker.mode) else {
                    return;
                };
                let (option, value) = (option.id.clone(), choice.value.clone());
                self.set_chat_menu(chat, ChatMenu::Models, false);
                self.send_chat_option(chat, option, value, window, cx);
            }
        }
    }

    /// Sets the chat's model to `value`, remembers it for new sessions and recent, and
    /// moves the keyboard to the thinking levels (closing the picker without any).
    fn pick_model(
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

    /// Stars or unstars the highlighted model.
    fn toggle_highlighted_favorite(&mut self, chat: &str, cx: &App) {
        let layout = self.model_layout(chat, cx);
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        let (Some(picker), Some((layout, _))) = (model.picker.as_ref(), layout) else {
            return;
        };
        if picker.column != PickerColumn::Models {
            return;
        }
        if let Some(index) = layout.choice(picker.highlight) {
            self.toggle_favorite(chat, index, cx);
        }
    }

    /// Stars or unstars model `index` (into the model setting's choices), which stays
    /// highlighted where the list now has it.
    fn toggle_favorite(&mut self, chat: &str, index: usize, cx: &App) {
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        let Some(value) = model.picker_options()[0]
            .and_then(|models| models.choices.get(index))
            .map(|choice| choice.value.clone())
        else {
            return;
        };
        let harness = model.harness();
        self.remember_models(harness, |models| models.toggle_favorite(&value));
        let Some((layout, _)) = self.model_layout(chat, cx) else {
            return;
        };
        let Some(picker) = self
            .chat
            .chats
            .get_mut(chat)
            .and_then(|model| model.picker.as_mut())
        else {
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
    /// thinking levels beside it, then a footer with the keys and the modes.
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
        let query = picker.search.read(cx).value();
        let searching = !query.trim().is_empty();
        let strip = models
            .filter(|_| !searching)
            .map(|option| self.render_model_strip(chat, option, plans, picker, cx));
        let list = models.map(|option| {
            let layout = picker_layout(option, &query, plans, &remembered, &picker.tab);
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
                    border,
                ))
                .child(self.render_model_list(chat, option, layout, picker, &remembered, cx))
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
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                    .text_color(muted)
                    .child(format!(
                        "↑↓ move · ←→ column · ↵ pick · {} star",
                        if cfg!(target_os = "macos") {
                            "⌘D"
                        } else {
                            "Ctrl+D"
                        }
                    )),
            )
            .children(modes);
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

    /// The strip: your saved models' star, then a mark per provider, the shown one lit
    /// with a bar; each one's tooltip names it and how it is paid.
    fn render_model_strip(
        &self,
        chat: &ChatContent,
        option: &ChatOption,
        plans: &[ChatPlan],
        picker: &ModelPicker,
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
            let id = chat.id.clone();
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
                    this.show_model_tab(&id, Some(tab.clone()), cx);
                    if let Some(picker) = this
                        .chat
                        .chats
                        .get_mut(&id)
                        .and_then(|model| model.picker.as_mut())
                    {
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
        let providers =
            providers(option, plans)
                .into_iter()
                .enumerate()
                .map(|(index, provider)| {
                    let name = provider.map_or_else(|| "Other".to_owned(), provider_name);
                    let icon = match provider.and_then(model_icons::for_provider) {
                        Some(path) => mark(Some(path), 18.0, muted),
                        None => div()
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
            .child(saved)
            .child(div().mx_1().border_b_1().border_color(color(colors.border)))
            .children(providers)
            .into_any_element()
    }

    /// The model list: models with their marks, the current one checked, favorites
    /// starred (others show the star on hover); it fades at an edge with more beyond.
    fn render_model_list(
        &self,
        chat: &ChatContent,
        option: &ChatOption,
        layout: PickerLayout,
        picker: &ModelPicker,
        remembered: &ChatModels,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        if layout.choices.is_empty() {
            let empty = if picker.search.read(cx).value().trim().is_empty() {
                "No favorites or recent models yet"
            } else {
                "No models match"
            };
            return div()
                .flex_1()
                .px_4()
                .pt_3()
                .text_color(muted)
                .child(empty)
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
        let (id, setting) = (chat.id.clone(), option.id.clone());
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
                                let (pick_id, pick_setting, pick_value) =
                                    (id.clone(), setting.clone(), choice.value.clone());
                                let (star_chat, star_index) = (id.clone(), *index);
                                controls::menu_row(("chat-model-row", row), lit, false, cx)
                                    .group("chat-model-row")
                                    .w_full()
                                    .h(ui_rems(ROW_HEIGHT))
                                    .gap_x_2()
                                    .child(mark(model_mark(choice), ICON_SIZE, muted))
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
                                                            &star_chat, star_index, cx,
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
                                                &pick_id,
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
        let layout = picker_layout(&option, "", &[], &remembered, &PickerTab::Saved);
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
        let layout = picker_layout(&option, "o", &plans(), &remembered, &PickerTab::Saved);
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
}
