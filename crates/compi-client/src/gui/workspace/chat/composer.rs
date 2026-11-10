//! Under the transcript: what waits on the reader (a failed harness, a question, a
//! permission whose tool call is not in the transcript, the turn running), then the
//! composer with its attachments and its `/` command and `@` file lists, and a row of the
//! harness's settings, context use and Send or Stop.
use super::*;
use compi_protocol::chat::{
    ChatChoice, ChatCommand, ChatOption, ChatPlan, MAX_CHAT_IMAGE_BYTES, MAX_CHAT_IMAGES,
};
use controls::MenuEntry;

/// Typing pauses this long before a file search.
const FILE_SEARCH_DELAY: Duration = Duration::from_millis(150);
/// The most files and commands a list shows.
const MAX_COMPLETIONS: usize = 50;
/// The settings the composer row shows as pickers, in order; the rest go under More.
const PICKED_SETTINGS: [&str; 3] = ["model", "thought_level", "mode"];
/// The composer card's corner radius.
const CARD_RADIUS: f32 = 10.0;
/// The harness's mark in the empty draft.
const MARK_SIZE: f32 = 14.0;
/// A setting with more choices than this opens a searched list instead of a menu.
const SEARCHED_CHOICES: usize = 16;
const PICKER_WIDTH: f32 = 420.0;
const PICKER_ROW_HEIGHT: f32 = 28.0;
/// Rows a searched list shows at once.
const PICKER_ROWS: usize = 12;

/// An open searched setting list: its search and highlighted match.
pub(super) struct OptionPicker {
    /// The setting it picks for (`ChatOption::id`).
    option: String,
    search: Entity<InputState>,
    _events: Subscription,
    /// Index into the listed choices, in their grouped order.
    highlight: usize,
    scroll: UniformListScrollHandle,
}

impl OptionPicker {
    /// The choices matching the search, as indices into `option.choices`: every word
    /// of the search in the name, value, group or provider, ignoring case.
    fn matches(&self, option: &ChatOption, cx: &App) -> Vec<usize> {
        let query = self.search.read(cx).value().to_lowercase();
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

    /// The matches laid out under their providers' headings.
    fn layout(&self, option: &ChatOption, plans: &[ChatPlan], cx: &App) -> PickerLayout {
        picker_layout(option, &self.matches(option, cx), plans)
    }
}

/// A row of a searched setting list.
#[derive(Debug, PartialEq)]
enum PickerRow {
    /// A provider's heading: its name, its plan's usage or that it is paid per use, and
    /// how many of its choices are listed.
    Heading {
        title: String,
        detail: Option<String>,
        count: usize,
    },
    /// A choice, by index into `option.choices`.
    Choice(usize),
}

/// The rows of a searched setting list, and the row of each choice in the order arrow
/// keys step through them.
struct PickerLayout {
    rows: Vec<PickerRow>,
    choices: Vec<usize>,
}

impl PickerLayout {
    /// The choice (index into `option.choices`) listed `listed`th.
    fn choice(&self, listed: usize) -> Option<usize> {
        match self.rows.get(*self.choices.get(listed)?)? {
            PickerRow::Choice(index) => Some(*index),
            PickerRow::Heading { .. } => None,
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

/// `matches` (indices into `option.choices`) under a heading per provider: providers
/// with a subscription plan first, then those paid per use; in each, the current
/// choice's provider first, then by name. Choices keep their order within a provider.
/// Without providers, the matches as they are.
fn picker_layout(option: &ChatOption, matches: &[usize], plans: &[ChatPlan]) -> PickerLayout {
    let provider_of = |index: usize| choice_provider(&option.choices[index]);
    if matches.iter().all(|&index| provider_of(index).is_none()) {
        return PickerLayout {
            rows: matches
                .iter()
                .map(|&index| PickerRow::Choice(index))
                .collect(),
            choices: (0..matches.len()).collect(),
        };
    }
    let current = option
        .choices
        .iter()
        .find(|choice| choice.value == option.current)
        .and_then(choice_provider);
    let mut providers: Vec<Option<&str>> = Vec::new();
    for &index in matches {
        if !providers.contains(&provider_of(index)) {
            providers.push(provider_of(index));
        }
    }
    let plan = |provider: Option<&str>| {
        plans
            .iter()
            .find(|plan| Some(plan.provider.as_str()) == provider)
    };
    providers.sort_by_key(|&provider| {
        (
            plan(provider).is_none(),
            provider.is_none(),
            provider != current,
            provider.map(provider_name).unwrap_or_default(),
        )
    });
    let mut layout = PickerLayout {
        rows: Vec::new(),
        choices: Vec::new(),
    };
    for provider in providers {
        let listed: Vec<usize> = matches
            .iter()
            .copied()
            .filter(|&index| provider_of(index) == provider)
            .collect();
        let detail = match plan(provider) {
            Some(plan) => {
                let tightest = plan.limits.iter().min_by_key(|limit| limit.left_percent);
                Some(match tightest {
                    Some(limit) => format!(
                        "Subscription · {}: {}% left",
                        limit.label, limit.left_percent
                    ),
                    None => "Subscription".to_owned(),
                })
            }
            None => provider.map(|_| "Pay per use".to_owned()),
        };
        layout.rows.push(PickerRow::Heading {
            title: provider.map_or_else(|| "Other".to_owned(), provider_name),
            detail,
            count: listed.len(),
        });
        for index in listed {
            layout.choices.push(layout.rows.len());
            layout.rows.push(PickerRow::Choice(index));
        }
    }
    layout
}

/// What the system picker returned, sorted: files by the path the chat's environment
/// knows them by, images read, and why any were left out.
struct PickedAttachments {
    files: Vec<String>,
    images: Vec<ChatImage>,
    refused: Option<String>,
}

/// Reads `picked` for a chat in `environment`: images by their bytes, other files by
/// path, which a remote chat cannot reach.
fn read_attachments(
    environment: &ContentEnvironment,
    remote: bool,
    picked: Vec<std::path::PathBuf>,
) -> PickedAttachments {
    use base64::Engine as _;
    let mut read = PickedAttachments {
        files: Vec::new(),
        images: Vec::new(),
        refused: None,
    };
    let mut left_out = Vec::new();
    for path in picked {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        if let Some(mime) = ChatImage::mime_for(&name) {
            match std::fs::read(&path) {
                Ok(bytes) if bytes.len() <= MAX_CHAT_IMAGE_BYTES => read.images.push(ChatImage {
                    name,
                    mime: mime.to_owned(),
                    data: base64::engine::general_purpose::STANDARD.encode(bytes),
                }),
                Ok(_) => left_out.push(format!(
                    "{name} (over {:.1} MB)",
                    MAX_CHAT_IMAGE_BYTES as f64 / (1024.0 * 1024.0)
                )),
                Err(error) => left_out.push(format!("{name} ({error})")),
            }
            continue;
        }
        let picked = path.to_string_lossy();
        match crate::gui::open_with::picked_path(environment, &picked).filter(|_| !remote) {
            Some(file) => read.files.push(file),
            None => left_out.push(name),
        }
    }
    if !left_out.is_empty() {
        read.refused = Some(format!(
            "Not attached: {}. Files must be where the chat can read them; type @ to \
             attach from its folder.",
            left_out.join(", ")
        ));
    }
    read
}

/// The list open over the composer.
pub(super) struct Completion {
    kind: CompletionKind,
    /// The highlighted item.
    highlight: usize,
    /// The list's scroll, kept on the highlighted item.
    scroll: ScrollHandle,
}

/// The widest the `/` and `@` lists get; narrower composers make them narrower.
const COMPLETION_MAX_WIDTH: f32 = 560.0;

enum CompletionKind {
    /// `/` at the start: the harness's commands starting with `query`.
    Commands { query: String },
    /// `@` before the cursor: files of the chat's folder matching `query`, which starts
    /// after the `@` at byte `at`.
    Files {
        at: usize,
        query: String,
        found: Found,
    },
}

enum Found {
    /// Nothing typed after `@` yet.
    Waiting,
    Searching,
    /// Absolute paths.
    Listed(Vec<String>),
    Failed(String),
}

/// The answers being given to an open question, by field.
pub(super) struct QuestionForm {
    id: u64,
    fields: Vec<FieldForm>,
}

struct FieldForm {
    /// The choice values picked.
    picked: Vec<String>,
    /// A text field's answer, or a choice field's Other.
    text: Option<Entity<InputState>>,
}

/// What the text before the cursor asks to complete.
#[derive(Debug, PartialEq, Eq)]
enum Wanted<'a> {
    Command(&'a str),
    File { at: usize, query: &'a str },
}

fn wanted(text: &str, cursor: usize) -> Option<Wanted<'_>> {
    let before = text.get(..cursor)?;
    if let Some(name) = before.strip_prefix('/')
        && !name.contains(char::is_whitespace)
    {
        return Some(Wanted::Command(name));
    }
    let at = before.rfind('@')?;
    let query = &before[at + 1..];
    let starts_word = before[..at]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    (starts_word && !query.contains(char::is_whitespace)).then_some(Wanted::File { at, query })
}

/// A `/` command Compi runs itself, whatever the harness; offered unless the harness has
/// a command of the same name, which then wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompiCommand {
    New,
    Resume,
    Sessions,
}

impl CompiCommand {
    const ALL: [Self; 3] = [Self::New, Self::Resume, Self::Sessions];

    fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Resume => "resume",
            Self::Sessions => "sessions",
        }
    }

    fn hint(self) -> Option<&'static str> {
        match self {
            Self::New => None,
            Self::Resume | Self::Sessions => Some("[title or id]"),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::New => "Start a new session",
            Self::Resume => "Pick an earlier session in this folder to continue",
            Self::Sessions => "List the sessions in this folder",
        }
    }

    /// Whether the harness offers a command of this name.
    fn shadowed(self, harness: &[ChatCommand]) -> bool {
        harness.iter().any(|command| {
            command
                .name
                .trim_start_matches('/')
                .eq_ignore_ascii_case(self.name())
        })
    }
}

/// Compi's command `text` runs, with what follows its name; `None` when `text` is not
/// one, or the harness has a command of that name.
pub(super) fn compi_command<'a>(
    text: &'a str,
    harness: &[ChatCommand],
) -> Option<(CompiCommand, &'a str)> {
    let rest = text.strip_prefix('/')?;
    let (name, argument) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let command = CompiCommand::ALL
        .into_iter()
        .find(|command| command.name().eq_ignore_ascii_case(name))?;
    (!command.shadowed(harness)).then_some((command, argument.trim()))
}

/// A row of the `/` list: Compi's command or the harness's.
#[derive(Clone, Copy)]
enum ListedCommand<'a> {
    Compi(CompiCommand),
    Harness(&'a ChatCommand),
}

impl<'a> ListedCommand<'a> {
    fn name(self) -> &'a str {
        match self {
            Self::Compi(command) => command.name(),
            Self::Harness(command) => command.name.trim_start_matches('/'),
        }
    }

    fn hint(self) -> Option<&'a str> {
        match self {
            Self::Compi(command) => command.hint(),
            Self::Harness(command) => command.hint.as_deref(),
        }
    }

    fn description(self) -> &'a str {
        match self {
            Self::Compi(command) => command.description(),
            Self::Harness(command) => &command.description,
        }
    }
}

/// The commands whose name starts with `query`, ignoring case: Compi's first, then the
/// harness's.
fn matching_commands<'a>(harness: &'a [ChatCommand], query: &str) -> Vec<ListedCommand<'a>> {
    let query = query.to_lowercase();
    CompiCommand::ALL
        .into_iter()
        .filter(|command| !command.shadowed(harness))
        .map(ListedCommand::Compi)
        .chain(harness.iter().map(ListedCommand::Harness))
        .filter(|command| command.name().to_lowercase().starts_with(&query))
        .take(MAX_COMPLETIONS)
        .collect()
}

/// A setting's choices as menu entries, under their groups' headings.
fn option_entries(
    option: &ChatOption,
    pick: impl Fn(String) -> Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
) -> Vec<MenuEntry> {
    let mut entries = Vec::new();
    let mut group = None;
    for choice in &option.choices {
        if choice.group.is_some() && choice.group != group {
            group.clone_from(&choice.group);
            entries.push(MenuEntry::Label(
                choice.group.clone().unwrap_or_default().into(),
            ));
        }
        let handler = pick(choice.value.clone());
        entries.push(
            controls::menu_item(choice.name.clone(), move |event, window, cx| {
                handler(event, window, cx)
            })
            .checked(choice.value == option.current)
            .into(),
        );
    }
    entries
}

/// Cost as the footer shows it, and the transcript's sub-agents.
pub(super) fn cost_label(cost: f64, currency: Option<&str>) -> String {
    match currency {
        None | Some("USD") => format!("${cost:.2}"),
        Some(currency) => format!("{cost:.2} {currency}"),
    }
}

impl Completion {
    fn count(&self, state: Option<&ChatState>) -> usize {
        match &self.kind {
            CompletionKind::Commands { query } => matching_commands(
                state
                    .map(|state| state.commands.as_slice())
                    .unwrap_or_default(),
                query,
            )
            .len(),
            CompletionKind::Files {
                found: Found::Listed(files),
                ..
            } => files.len(),
            CompletionKind::Files { .. } => 0,
        }
    }
}

impl CompiApp {
    /// The composer's draft changed: the completion follows what precedes the cursor.
    pub(super) fn chat_composer_changed(&mut self, chat: &str, cx: &mut Context<Self>) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let composer = model.composer.read(cx);
        let (value, cursor) = (composer.value(), composer.cursor());
        if model.dismissed.as_ref() == Some(&value) {
            model.completion = None;
            return;
        }
        model.dismissed = None;
        let mut search = None;
        model.completion = match wanted(&value, cursor) {
            None => None,
            Some(Wanted::Command(query)) => {
                let same = matches!(
                    &model.completion,
                    Some(Completion {
                        kind: CompletionKind::Commands { query: shown },
                        ..
                    }) if shown == query
                );
                if same {
                    model.completion.take()
                } else {
                    Some(Completion {
                        highlight: 0,
                        scroll: ScrollHandle::new(),
                        kind: CompletionKind::Commands {
                            query: query.to_owned(),
                        },
                    })
                }
            }
            Some(Wanted::File { at, query }) => {
                let same = matches!(
                    &model.completion,
                    Some(Completion {
                        kind: CompletionKind::Files { at: shown_at, query: shown, .. },
                        ..
                    }) if *shown_at == at && shown == query
                );
                if same {
                    model.completion.take()
                } else {
                    if !query.is_empty() {
                        search = Some(query.to_owned());
                    }
                    Some(Completion {
                        highlight: 0,
                        scroll: ScrollHandle::new(),
                        kind: CompletionKind::Files {
                            at,
                            query: query.to_owned(),
                            found: if query.is_empty() {
                                Found::Waiting
                            } else {
                                Found::Searching
                            },
                        },
                    })
                }
            }
        };
        if let Some(query) = search {
            self.search_chat_files(chat, query, cx);
        }
    }

    /// Searches the chat's folder for files matching `query` once typing pauses; only
    /// the latest search's reply is shown.
    fn search_chat_files(&mut self, chat: &str, query: String, cx: &mut Context<Self>) {
        let Some((_, content)) = self.chat_pane(chat) else {
            return;
        };
        let surface = self.search_surface(&content.environment, &content.folder, None);
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let surface = match surface {
            Ok(surface) => surface,
            Err(reason) => {
                if let Some(Completion {
                    kind: CompletionKind::Files { found, .. },
                    ..
                }) = &mut model.completion
                {
                    *found = Found::Failed(reason.to_owned());
                }
                return;
            }
        };
        model.file_request += 1;
        let request = model.file_request;
        let target = self.target.clone();
        let chat = chat.to_owned();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FILE_SEARCH_DELAY).await;
            let latest = this
                .read_with(cx, |this, _| {
                    this.chat
                        .chats
                        .get(&chat)
                        .is_some_and(|model| model.file_request == request)
                })
                .unwrap_or(false);
            if !latest {
                return;
            }
            let folder = content.folder;
            let result = cx
                .background_executor()
                .spawn(async move {
                    (|| -> crate::Result<_> {
                        target.connect()?.search_directory(surface, folder, query)
                    })()
                    .map_err(|error| error.to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(model) = this.chat.chats.get_mut(&chat) else {
                    return;
                };
                if model.file_request != request {
                    return;
                }
                if let Some(Completion {
                    kind: CompletionKind::Files { found, .. },
                    highlight,
                    scroll,
                }) = &mut model.completion
                {
                    *highlight = 0;
                    scroll.scroll_to_item(0);
                    *found = match result {
                        Ok(entries) => Found::Listed(
                            entries
                                .into_iter()
                                .filter(|entry| !entry.is_directory)
                                .take(MAX_COMPLETIONS)
                                .map(|entry| entry.path)
                                .collect(),
                        ),
                        Err(error) => Found::Failed(error),
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Keys for the open completion list: arrows move, Tab and Enter complete, Escape
    /// closes it until the draft changes.
    pub(super) fn chat_completion_key(
        &mut self,
        chat: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return false;
        };
        let Some(completion) = &mut model.completion else {
            return false;
        };
        let count = completion.count(model.state.as_ref());
        match key {
            "escape" => {
                model.dismissed = Some(model.composer.read(cx).value());
                model.completion = None;
                true
            }
            "up" | "down" if count > 0 => {
                completion.highlight = if key == "up" {
                    (completion.highlight + count - 1) % count
                } else {
                    (completion.highlight + 1) % count
                };
                completion.scroll.scroll_to_item(completion.highlight);
                true
            }
            "tab" | "enter" => self.accept_chat_completion(chat, window, cx),
            _ => false,
        }
    }

    /// Completes the highlighted item: a command fills in `/name `, a file becomes an
    /// attachment and its `@` text goes. Whether there was one.
    pub(super) fn accept_chat_completion(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return false;
        };
        let Some(completion) = &model.completion else {
            return false;
        };
        let composer = model.composer.read(cx);
        let (value, cursor) = (composer.value(), composer.cursor());
        let Some(rest) = value.get(cursor..) else {
            return false;
        };
        let (text, caret) = match &completion.kind {
            CompletionKind::Commands { query } => {
                let harness = model
                    .state
                    .as_ref()
                    .map(|state| state.commands.as_slice())
                    .unwrap_or_default();
                let Some(command) = matching_commands(harness, query)
                    .get(completion.highlight)
                    .copied()
                else {
                    return false;
                };
                let head = format!("/{} ", command.name());
                let caret = head.len();
                (head + rest.trim_start(), caret)
            }
            CompletionKind::Files {
                at,
                found: Found::Listed(files),
                ..
            } => {
                let Some(file) = files.get(completion.highlight) else {
                    return false;
                };
                if !model.attachments.contains(file) {
                    model.attachments.push(file.clone());
                }
                (format!("{}{rest}", &value[..*at]), *at)
            }
            CompletionKind::Files { .. } => return false,
        };
        model.completion = None;
        model.composer.update(cx, |composer, cx| {
            composer.set_value(text, window, cx);
            let caret = composer.text().offset_to_position(caret);
            composer.set_cursor_position(caret, window, cx);
        });
        true
    }

    /// Builds the form of the question chat `chat` asks, once per question.
    pub(super) fn prepare_chat_question(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let Some(question) = model
            .state
            .as_ref()
            .and_then(|state| state.question.clone())
        else {
            model.question = None;
            return;
        };
        if model
            .question
            .as_ref()
            .is_some_and(|form| form.id == question.id)
        {
            return;
        }
        let fields = question
            .fields
            .iter()
            .map(|field| FieldForm {
                picked: Vec::new(),
                text: (field.choices.is_empty() || field.other).then(|| {
                    let placeholder = if field.choices.is_empty() {
                        "Answer"
                    } else {
                        "Other"
                    };
                    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
                }),
            })
            .collect();
        model.question = Some(QuestionForm {
            id: question.id,
            fields,
        });
    }

    /// Picks or unpicks `value` of field `field` of the open question.
    fn pick_chat_choice(&mut self, chat: &str, field: usize, value: String) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let multi = model
            .state
            .as_ref()
            .and_then(|state| state.question.as_ref()?.fields.get(field))
            .is_some_and(|field| field.multi);
        let Some(form) = model
            .question
            .as_mut()
            .and_then(|form| form.fields.get_mut(field))
        else {
            return;
        };
        if let Some(index) = form.picked.iter().position(|picked| *picked == value) {
            form.picked.remove(index);
        } else if multi {
            form.picked.push(value);
        } else {
            form.picked = vec![value];
        }
    }

    /// The open question's answers, or why it cannot be answered yet. Typed text replaces
    /// a field's choice.
    fn chat_question_answers(
        question: &ChatQuestion,
        form: &QuestionForm,
        cx: &App,
    ) -> Result<ChatAnswers, String> {
        let mut answers = ChatAnswers::new();
        for (field, answer) in question.fields.iter().zip(&form.fields) {
            let text = answer
                .text
                .as_ref()
                .map(|text| text.read(cx).value().trim().to_owned())
                .filter(|text| !text.is_empty());
            if let Some(text) = text {
                answers.insert(field.key.clone(), ChatAnswer::Text { text });
            } else if !answer.picked.is_empty() {
                answers.insert(
                    field.key.clone(),
                    ChatAnswer::Choices {
                        values: answer.picked.clone(),
                    },
                );
            } else if field.required {
                return Err(format!("Answer “{}” first", field.title));
            }
        }
        Ok(answers)
    }

    fn submit_chat_question(&mut self, chat: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        let (Some(question), Some(form)) = (
            model
                .state
                .as_ref()
                .and_then(|state| state.question.as_ref()),
            model.question.as_ref(),
        ) else {
            return;
        };
        if let Ok(answers) = Self::chat_question_answers(question, form, cx) {
            self.answer_chat_question(chat, Some(answers), window, cx);
        }
    }

    /// Under the transcript: notices, what waits on the reader, the composer and its row.
    pub(super) fn render_chat_footer(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let harness = chat.harness.name();
        let state = model.state.as_ref();
        let transcript = state.is_some_and(|state| !state.entries.is_empty());
        let status = state.map(|state| &state.status);
        let failed = match status {
            Some(ChatStatus::Failed { message }) if transcript => Some(message.clone()),
            _ => None,
        };
        let starting = transcript && matches!(status, Some(ChatStatus::Starting));
        let working = match status {
            Some(ChatStatus::Working { since_ms }) => Some(*since_ms),
            _ => None,
        };
        // A permission whose tool call is not in the transcript is asked here.
        let orphan = state.and_then(|state| {
            let permission = state.permission.as_ref()?;
            let drawn = permission.tool.as_ref().is_some_and(|tool| {
                state
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, ChatEntry::Tool(call) if call.id == *tool))
            });
            (!drawn).then_some(permission)
        });
        let frame = RowFrame {
            app: cx.entity().downgrade(),
            chat: chat.id.as_str().into(),
            colors,
            body_size: self.chat_body_size(),
            mono: self.typography.font.family.clone(),
            mark: harness_mark(chat.harness),
            permission: None,
            retry: false,
            diff_refusal: None,
        };
        div()
            .flex_none()
            .px_3()
            .pt_1()
            .pb_2()
            .flex()
            .flex_col()
            .gap_2()
            .when_some(model.error.clone(), |footer, error| {
                footer.child(
                    div()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(color(colors.error))
                        .child(error),
                )
            })
            .when(starting, |footer| {
                footer.child(
                    div()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child(format!("Starting {harness}…")),
                )
            })
            .when_some(failed, |footer, message| {
                footer.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(color(colors.error))
                                .child(message),
                        )
                        .child(self.chat_restart_button(chat, cx)),
                )
            })
            .when_some(orphan, |footer, permission| {
                footer.child(rows::render_permission_block(None, permission, &frame))
            })
            .when_some(
                state
                    .and_then(|state| state.question.as_ref())
                    .zip(model.question.as_ref()),
                |footer, (question, form)| {
                    footer.child(self.render_chat_question(chat, question, form, cx))
                },
            )
            .when_some(working, |footer, since_ms| {
                footer.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child(Spinner::new().with_size(Size::XSmall).color(muted))
                        .child(format!(
                            "Working · {}",
                            duration_label(now_ms().saturating_sub(since_ms))
                        )),
                )
            })
            .when_some(
                state.filter(|state| !state.queued.is_empty()),
                |footer, state| footer.child(self.render_chat_queue(chat, state, cx)),
            )
            .child(self.render_chat_card(chat, model, &frame, cx))
            .child(self.render_chat_context(chat, model))
            .into_any_element()
    }

    /// Prompts waiting for the running turn, oldest first, each with × to drop it. A
    /// queue left after a stopped turn waits for the next prompt.
    fn render_chat_queue(
        &self,
        chat: &ChatContent,
        state: &ChatState,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let held = matches!(state.status, ChatStatus::Ready);
        let rows = state.queued.iter().enumerate().map(|(index, queued)| {
            let (id, queued_id) = (chat.id.clone(), queued.id);
            let attached = queued.files.len() + queued.images.len();
            let text = queued.text.lines().find(|line| !line.trim().is_empty());
            div()
                .flex()
                .items_center()
                .gap_2()
                .pl_2()
                .rounded(ui_rems(6.0))
                .bg(color(colors.surface))
                .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                .child(
                    div()
                        .flex_none()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child("Queued"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(text.unwrap_or_default().trim().to_owned()),
                )
                .when(attached > 0, |row| {
                    row.child(
                        div()
                            .flex_none()
                            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child(format!("+{attached} attached")),
                    )
                })
                .child(
                    icon_button(("chat-queued", index), IconName::Close, "Remove from queue")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.remove_queued_chat(&id, queued_id, window, cx);
                            cx.notify();
                        })),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .when(held, |queue| {
                queue.child(
                    div()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child("Stopped: these go after your next message"),
                )
            })
            .children(rows)
            .into_any_element()
    }

    /// A question the harness asks: its message, a row of choices per field (one or
    /// several), a text field where it takes words, then Submit and Skip.
    fn render_chat_question(
        &self,
        chat: &ChatContent,
        question: &ChatQuestion,
        form: &QuestionForm,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let refusal = Self::chat_question_answers(question, form, cx).err();
        let fields =
            question
                .fields
                .iter()
                .zip(&form.fields)
                .enumerate()
                .map(|(index, (field, answer))| {
                    let choices = field
                        .choices
                        .iter()
                        .enumerate()
                        .map(|(choice_index, choice)| {
                            let (id, value) = (chat.id.clone(), choice.value.clone());
                            text_button(
                                ("chat-choice", index * 1000 + choice_index),
                                choice.name.clone(),
                            )
                            .selected(answer.picked.contains(&choice.value))
                            .when_some(choice.description.clone(), |button, description| {
                                button.detail(description)
                            })
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    this.pick_chat_choice(&id, index, value.clone());
                                    cx.notify();
                                },
                            ))
                        });
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .when(
                            !field.title.is_empty()
                                && field.title.trim() != question.message.trim(),
                            |column| {
                                column.child(
                                    div()
                                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                                        .text_color(muted)
                                        .child(field.title.clone()),
                                )
                            },
                        )
                        .when(!field.choices.is_empty(), |column| {
                            column.child(div().flex().flex_wrap().gap_1().children(choices))
                        })
                        .when_some(answer.text.as_ref(), |column, text| {
                            column.child(Input::new(text).appearance(true))
                        })
                });
        let (submit_id, skip_id) = (chat.id.clone(), chat.id.clone());
        div()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(ui_rems(6.0))
            .bg(color(colors.surface))
            .child(
                div()
                    .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                    .child(question.message.clone()),
            )
            .children(fields)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        text_button("chat-question-submit", "Submit")
                            .primary()
                            .header()
                            .disabled_because(refusal)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.submit_chat_question(&submit_id, window, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        text_button("chat-question-skip", "Skip")
                            .ghost()
                            .header()
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.answer_chat_question(&skip_id, None, window, cx);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The composer: one card holding the attached files, the draft (with the harness's
    /// mark and hints while it is empty), and a row of attach, the harness's settings,
    /// usage and Send or Stop; the completion list opens over it. A click anywhere on the
    /// card puts the keyboard in the draft.
    fn render_chat_card(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        frame: &RowFrame,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = frame.colors;
        let muted = color(modal_text_color(colors.muted, &colors));
        let empty = model.composer.read(cx).value().is_empty();
        let working = model.state.as_ref().is_some_and(ChatState::working);
        let composer = model.composer.clone();
        let remove = |index: usize, image: bool| {
            let id = chat.id.clone();
            icon_button(
                ("chat-attachment", index * 2 + usize::from(image)),
                IconName::Close,
                "Remove",
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if let Some(model) = this.chat.chats.get_mut(&id) {
                    if image && index < model.images.len() {
                        model.images.remove(index);
                    } else if !image && index < model.attachments.len() {
                        model.attachments.remove(index);
                    }
                }
                cx.notify();
            }))
        };
        let files = model.attachments.iter().enumerate().map(|(index, file)| {
            rows::file_chip(rows::relative_to(file, &model.folder).into(), &frame.colors)
                .pr_0()
                .child(remove(index, false))
        });
        let images = model.images.iter().enumerate().map(|(index, image)| {
            rows::image_chip(image.name.clone().into(), &frame.colors)
                .pr_0()
                .child(remove(index, true))
        });
        div()
            .relative()
            .flex()
            .flex_col()
            .rounded(ui_rems(CARD_RADIUS))
            .border_1()
            .border_color(color(colors.border))
            .in_focus(move |style| style.border_color(color(colors.accent)))
            .bg(color(colors.surface))
            .track_focus(&model.card)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&composer.focus_handle(cx), cx);
            })
            .when_some(model.completion.as_ref(), |card, completion| {
                card.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_full()
                        .mb_1()
                        .child(self.render_chat_completion(chat, model, completion, cx)),
                )
            })
            .when(
                !model.attachments.is_empty() || !model.images.is_empty(),
                |card| {
                    card.child(
                        div()
                            .px_3()
                            .pt_2()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(files)
                            .children(images),
                    )
                },
            )
            .child(
                div()
                    .relative()
                    .px_0p5()
                    .pt_1()
                    // The field pads its text 10 px across and 8 px down; the hint sits
                    // where typing starts, 12 px in from the card and 12 px down.
                    .child(
                        Textarea::new(&model.composer)
                            .appearance(false)
                            .font_weight(FontWeight::NORMAL),
                    )
                    .when(empty, |draft| {
                        draft.child(
                            div()
                                .absolute()
                                .top(ui_rems(12.0))
                                .left(ui_rems(12.0))
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_color(muted)
                                .child(
                                    img(harness_mark(chat.harness))
                                        .flex_none()
                                        .size(ui_rems(MARK_SIZE)),
                                )
                                .child(if working {
                                    let harness = chat.harness.name();
                                    match self.config.chat_enter {
                                        crate::config::ChatEnter::Steer => format!(
                                            "Enter steers {harness}  ·  {SECONDARY_KEY} queues for after"
                                        ),
                                        crate::config::ChatEnter::Queue => format!(
                                            "Enter queues for after  ·  {SECONDARY_KEY} steers {harness}"
                                        ),
                                    }
                                } else {
                                    format!(
                                        "Ask {} anything  ·  / commands  ·  @ files",
                                        chat.harness.name()
                                    )
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .px_1p5()
                    .pb_1p5()
                    .pt_1()
                    .child(self.render_chat_controls(chat, model, cx)),
            )
            .into_any_element()
    }

    /// Under the composer, quietly: the folder, where it lives, and its Git branch.
    fn render_chat_context(&self, chat: &ChatContent, model: &ChatModel) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let place: SharedString = match &chat.environment {
            ContentEnvironment::Host if cfg!(windows) => "Windows".into(),
            ContentEnvironment::Host => "Local".into(),
            ContentEnvironment::Wsl { distribution } => format!("WSL: {distribution}").into(),
        };
        let branch = match &model.repo {
            Repo::At {
                branch: Some(branch),
                ..
            } => Some(branch.clone()),
            _ => None,
        };
        div()
            .px_1()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
            .text_color(muted)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(controls::glyph(IconName::Folder, muted))
                    .child(model.folder_label.clone()),
            )
            .child("/")
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(controls::glyph(IconName::Terminal, muted))
                    .child(place),
            )
            .when_some(branch, |line, branch| {
                line.child(
                    div()
                        .ml_2()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(controls::glyph(IconName::GitBranch, muted))
                        .child(div().truncate().child(branch)),
                )
            })
            .into_any_element()
    }

    /// The `/` command list or the `@` file list.
    fn render_chat_completion(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        completion: &Completion,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let note = |text: String| {
            div()
                .px_2()
                .min_h(ui_rems(26.0))
                .flex()
                .items_center()
                .text_color(muted)
                .child(text)
                .into_any_element()
        };
        let pick = |index: usize| {
            let id = chat.id.clone();
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                if let Some(Completion { highlight, .. }) = this
                    .chat
                    .chats
                    .get_mut(&id)
                    .and_then(|model| model.completion.as_mut())
                {
                    *highlight = index;
                }
                this.accept_chat_completion(&id, window, cx);
                cx.notify();
            })
        };
        let rows: Vec<AnyElement> = match &completion.kind {
            CompletionKind::Commands { query } => {
                let commands = matching_commands(
                    model
                        .state
                        .as_ref()
                        .map(|state| state.commands.as_slice())
                        .unwrap_or_default(),
                    query,
                );
                if commands.is_empty() {
                    vec![note(format!("No command starts with /{query}"))]
                } else {
                    commands
                        .into_iter()
                        .enumerate()
                        .map(|(index, command)| {
                            let name = command.name();
                            let line = || {
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                            };
                            controls::menu_row(
                                ("chat-command", index),
                                index == completion.highlight,
                                false,
                                cx,
                            )
                            .flex_none()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .py_0p5()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .gap_2()
                                            .child(div().flex_none().child(format!("/{name}")))
                                            .when_some(command.hint(), |row, hint| {
                                                row.child(
                                                    line().opacity(0.7).child(hint.to_owned()),
                                                )
                                            }),
                                    )
                                    .when(!command.description().is_empty(), |column| {
                                        column.child(
                                            line()
                                                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                                                .opacity(0.7)
                                                .child(command.description().to_owned()),
                                        )
                                    }),
                            )
                            .when(matches!(command, ListedCommand::Compi(_)), |row| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                                        .opacity(0.7)
                                        .child("Compi"),
                                )
                            })
                            .on_click(pick(index))
                            .into_any_element()
                        })
                        .collect()
                }
            }
            CompletionKind::Files { query, found, .. } => match found {
                Found::Waiting => vec![note(format!(
                    "Type to search files in {}",
                    model.folder_label
                ))],
                Found::Searching => vec![note("Searching…".to_owned())],
                Found::Failed(error) => vec![note(error.clone())],
                Found::Listed(files) if files.is_empty() => {
                    vec![note(format!("No files match {query}"))]
                }
                Found::Listed(files) => files
                    .iter()
                    .enumerate()
                    .map(|(index, file)| {
                        controls::menu_row(
                            ("chat-file", index),
                            index == completion.highlight,
                            false,
                            cx,
                        )
                        .flex_none()
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(rows::relative_to(file, &model.folder)),
                        )
                        .on_click(pick(index))
                        .into_any_element()
                    })
                    .collect(),
            },
        };
        controls::menu_surface(cx)
            .id("chat-completion")
            .w_full()
            .max_w(ui_rems(COMPLETION_MAX_WIDTH))
            .max_h(ui_rems(260.0))
            .overflow_y_scroll()
            .track_scroll(&completion.scroll)
            .children(rows)
            .into_any_element()
    }

    /// The card's bottom row: attach, the harness's settings (model, thinking, mode,
    /// More), context use and cost, and Send or Stop.
    fn render_chat_controls(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let state = model.state.as_ref();
        let picked: Vec<&ChatOption> = PICKED_SETTINGS
            .iter()
            .filter_map(|category| state?.option(category))
            .collect();
        let others: Vec<&ChatOption> = state.map_or_else(Vec::new, |state| {
            state
                .options
                .iter()
                .filter(|option| !picked.iter().any(|shown| shown.id == option.id))
                .collect()
        });
        let pickers = picked
            .iter()
            .enumerate()
            .map(|(index, option)| self.render_chat_option(chat, model, index, option, cx));
        let usage = state.and_then(|state| state.usage.as_ref()).map(|usage| {
            let mut label = format!("{} / {}", token_label(usage.used), token_label(usage.size));
            if let Some(cost) = usage.cost {
                label.push_str("  ·  ");
                label.push_str(&cost_label(cost, usage.currency.as_deref()));
            }
            div()
                .id("chat-usage")
                .flex_none()
                .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                .text_color(muted)
                .child(label)
                .tooltip(controls::tooltip(
                    "Context used / context window",
                    usage
                        .cost
                        .map(|_| SharedString::from("Then the session's cost so far")),
                    None,
                ))
        });
        let working = state.is_some_and(ChatState::working);
        let id = chat.id.clone();
        let action = if working {
            icon_button("chat-stop", IconName::Square, "Stop")
                .primary()
                .header()
                .disabled_because((model.requests > 0).then_some("Wait for the last request"))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.cancel_chat(&id, window, cx);
                    cx.notify();
                }))
        } else {
            icon_button("chat-send", IconName::ArrowUp, "Send")
                .primary()
                .header()
                .key("enter")
                .loading(model.requests > 0)
                .disabled_because(model.send_refusal(ChatDelivery::Prompt))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.send_chat(&id, false, window, cx);
                    cx.notify();
                }))
        };
        let attach_id = chat.id.clone();
        let attach = icon_button("chat-attach", IconName::Plus, "Attach files or images").on_click(
            cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.pick_chat_attachments(&attach_id, cx);
                cx.notify();
            }),
        );
        div()
            .flex()
            .items_center()
            .gap_0p5()
            .min_w_0()
            .child(attach)
            .children(pickers)
            .when(!others.is_empty(), |row| {
                row.child(self.render_chat_more(chat, model, &others, cx))
            })
            .child(div().flex_1())
            .children(usage)
            .child(div().flex_none().ml_2().child(action))
            .into_any_element()
    }

    /// The card's +: the system's file picker. Images (PNG, JPEG, GIF, WebP) are read here
    /// and sent with the prompt; other files are attached by the path the chat's
    /// environment knows them by. A file it cannot reach, or an image too large, is
    /// named under the card instead.
    fn pick_chat_attachments(&mut self, chat: &str, cx: &mut Context<Self>) {
        let Some((_, content)) = self.chat_pane(chat) else {
            return;
        };
        let remote = self.target.is_remote();
        let picker = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        let chat = chat.to_owned();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(picked))) = picker.await else {
                return;
            };
            let read = cx
                .background_executor()
                .spawn(async move { read_attachments(&content.environment, remote, picked) })
                .await;
            this.update(cx, |this, cx| {
                let Some(model) = this.chat.chats.get_mut(&chat) else {
                    return;
                };
                for file in read.files {
                    if !model.attachments.contains(&file) {
                        model.attachments.push(file);
                    }
                }
                for image in read.images {
                    if model.images.len() < MAX_CHAT_IMAGES
                        && !model.images.iter().any(|kept| kept.data == image.data)
                    {
                        model.images.push(image);
                    }
                }
                model.error = read.refused;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ctrl+V (Cmd+V) in the focused chat's composer: an image on the clipboard (a
    /// screenshot, say) is attached, made a PNG or JPEG small enough to send; whether
    /// there was one. Text is left to the field's own paste.
    pub(super) fn paste_chat_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(chat) = self
            .focused_pane()
            .and_then(|pane| self.pane_chat_content(pane))
            .map(|chat| chat.id.clone())
        else {
            return false;
        };
        let clipboard = cx.read_from_clipboard();
        #[cfg(windows)]
        let has_text = clipboard
            .as_ref()
            .and_then(|item| item.text())
            .is_some_and(|text| !text.is_empty());
        let pasted = clipboard.and_then(|item| {
            item.into_entries().find_map(|entry| match entry {
                gpui::ClipboardEntry::Image(image) => Some(image.bytes),
                _ => None,
            })
        });
        // Windows screenshots often come only as a bitmap, which GPUI does not read; text
        // copied with a picture of itself (from Office, say) pastes as text.
        #[cfg(windows)]
        let pasted = match pasted {
            Some(bytes) => Some(Ok(bytes)),
            None if has_text => None,
            None => crate::image_input::clipboard_bitmap().transpose(),
        };
        #[cfg(not(windows))]
        let pasted = pasted.map(Ok);
        let Some(pasted) = pasted else {
            return false;
        };
        let Some(model) = self.chat.chats.get_mut(&chat) else {
            return true;
        };
        let bytes = match pasted {
            Ok(bytes) if model.images.len() < MAX_CHAT_IMAGES => bytes,
            Ok(_) => {
                model.error = Some(format!("A prompt carries at most {MAX_CHAT_IMAGES} images"));
                return true;
            }
            Err(error) => {
                model.error = Some(error);
                return true;
            }
        };
        cx.spawn(async move |this, cx| {
            let image = cx
                .background_executor()
                .spawn(async move { crate::image_input::chat_image(bytes, MAX_CHAT_IMAGE_BYTES) })
                .await;
            this.update(cx, |this, cx| {
                let Some(model) = this.chat.chats.get_mut(&chat) else {
                    return;
                };
                match image {
                    Ok((bytes, mime)) if model.images.len() < MAX_CHAT_IMAGES => {
                        use base64::Engine as _;
                        let extension = mime.trim_start_matches("image/").replace("jpeg", "jpg");
                        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                        if !model.images.iter().any(|kept| kept.data == data) {
                            model.images.push(ChatImage {
                                name: format!(
                                    "Pasted image {}.{extension}",
                                    model.images.len() + 1
                                ),
                                mime: mime.to_owned(),
                                data,
                            });
                        }
                        model.error = None;
                    }
                    Ok(_) => {
                        model.error =
                            Some(format!("A prompt carries at most {MAX_CHAT_IMAGES} images"));
                    }
                    Err(error) => model.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        true
    }

    /// A picker of one harness setting: its current choice, opening the choices.
    fn render_chat_option(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        index: usize,
        option: &ChatOption,
        cx: &Context<Self>,
    ) -> AnyElement {
        let menu = ChatMenu::Option(option.id.clone());
        let open = model.menu.as_ref() == Some(&menu);
        // The menu's popover reports clicks on its trigger through `on_open_change`; a
        // click handler of the button's own would see the state that click already
        // changed and close it again.
        let mut trigger = text_button(("chat-option", index), option.current_name().to_owned())
            .ghost()
            .tooltip(option.name.clone())
            .menu_open(open);
        if option.category.as_deref() == Some("thought_level") {
            trigger = trigger.icon(IconName::Brain);
        }
        let id = chat.id.clone();
        let on_open_change = cx.listener(move |this, open: &bool, _, cx| {
            this.set_chat_menu(&id, menu.clone(), *open);
            cx.notify();
        });
        if option.choices.len() > SEARCHED_CHOICES {
            let content = model
                .picker
                .as_ref()
                .filter(|picker| open && picker.option == option.id)
                .map(|picker| {
                    let plans = model
                        .state
                        .as_ref()
                        .map(|state| state.plans.as_slice())
                        .unwrap_or_default();
                    self.render_chat_picker(chat, option, picker, plans, cx)
                });
            let keys = {
                let (app, id) = (cx.entity().downgrade(), chat.id.clone());
                move |key: &Keystroke, window: &mut Window, cx: &mut App| {
                    if let Some(app) = app.upgrade() {
                        app.update(cx, |this, cx| this.chat_picker_key(&id, key, window, cx));
                    }
                }
            };
            let picker = controls::popover(("chat-option-menu", index), trigger, open)
                .anchor(Anchor::BottomLeft)
                .on_open_change(on_open_change)
                .on_key(keys);
            return match content {
                Some(content) => picker.content(content).into_any_element(),
                None => picker.into_any_element(),
            };
        }
        // Entries are built only for an open menu: a closed one draws just its button.
        let entries = if open {
            option_entries(option, |value| {
                let (id, setting) = (chat.id.clone(), option.id.clone());
                Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.set_chat_option(&id, setting.clone(), value.clone(), window, cx);
                    cx.notify();
                }))
            })
        } else {
            Vec::new()
        };
        controls::menu(("chat-option-menu", index), trigger, open, entries)
            .anchor(Anchor::BottomLeft)
            .on_open_change(on_open_change)
            .into_any_element()
    }

    /// A long choice list (models): a search field over a virtual list of the choices
    /// matching it under their providers' headings, the current one checked, the
    /// highlighted one picked by Enter.
    fn render_chat_picker(
        &self,
        chat: &ChatContent,
        option: &ChatOption,
        picker: &OptionPicker,
        plans: &[ChatPlan],
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let layout = picker.layout(option, plans, cx);
        let highlighted = layout
            .choices
            .get(picker.highlight.min(layout.choices.len().saturating_sub(1)))
            .copied();
        let grouped = layout
            .rows
            .iter()
            .any(|row| matches!(row, PickerRow::Heading { .. }));
        let empty = layout.choices.is_empty();
        let count = layout.rows.len();
        let rows = Rc::new(layout.rows);
        let choices = Rc::new(option.choices.clone());
        let current = option.current.clone();
        let (id, setting) = (chat.id.clone(), option.id.clone());
        let list = uniform_list(
            "chat-picker-list",
            count,
            cx.processor(move |_, range: Range<usize>, _, cx| {
                range
                    .map(|row| match &rows[row] {
                        PickerRow::Heading {
                            title,
                            detail,
                            count,
                        } => div()
                            .id(("chat-picker-heading", row))
                            .h(ui_rems(PICKER_ROW_HEIGHT))
                            .px_2()
                            .flex()
                            .items_end()
                            .pb_1()
                            .gap_x_2()
                            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                                    .text_color(color(modal_text_color(colors.foreground, &colors)))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title.clone()),
                            )
                            .when_some(detail.clone(), |heading, detail| {
                                heading.child(div().min_w_0().truncate().child(detail))
                            })
                            .child(div().flex_1())
                            .child(div().flex_none().child(count.to_string()))
                            .into_any_element(),
                        PickerRow::Choice(index) => {
                            let choice = &choices[*index];
                            let (id, setting, value) =
                                (id.clone(), setting.clone(), choice.value.clone());
                            // A heading names the provider; ungrouped choices keep their
                            // own detail.
                            let detail = (!grouped)
                                .then(|| {
                                    choice.group.clone().or_else(|| choice.description.clone())
                                })
                                .flatten();
                            controls::menu_row(
                                ("chat-picker-row", row),
                                Some(row) == highlighted,
                                choice.value == current,
                                cx,
                            )
                            .h(ui_rems(PICKER_ROW_HEIGHT))
                            .gap_x_2()
                            .child(
                                div()
                                    .flex_none()
                                    .w(ui_rems(12.0))
                                    .when(choice.value == current, |mark| {
                                        mark.child(controls::glyph(IconName::Check, muted))
                                    }),
                            )
                            .child(div().flex_none().child(choice.name.clone()))
                            .when_some(detail, |row, detail| {
                                row.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                                        .text_color(muted)
                                        .child(detail),
                                )
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.set_chat_option(
                                    &id,
                                    setting.clone(),
                                    value.clone(),
                                    window,
                                    cx,
                                );
                                this.set_chat_menu(&id, ChatMenu::Option(setting.clone()), false);
                                cx.notify();
                            }))
                            .into_any_element()
                        }
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .h(ui_rems(
            PICKER_ROW_HEIGHT * count.clamp(1, PICKER_ROWS) as f32,
        ))
        .track_scroll(&picker.scroll);
        controls::menu_surface(cx)
            .w(ui_rems(PICKER_WIDTH))
            .flex()
            .flex_col()
            .gap_1()
            .child(Input::new(&picker.search).appearance(true))
            .when(empty, |panel| {
                panel.child(
                    div()
                        .px_2()
                        .h(ui_rems(PICKER_ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .text_color(muted)
                        .child(format!("No {} matches", option.name.to_lowercase())),
                )
            })
            .when(!empty, |panel| panel.child(list))
            .into_any_element()
    }

    /// Opens the search of setting picker `option` of chat `chat`, or drops one whose
    /// menu closed, before drawing.
    pub(super) fn prepare_chat_picker(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let wanted = match &model.menu {
            Some(ChatMenu::Option(option))
                if model
                    .state
                    .as_ref()
                    .and_then(|state| state.options.iter().find(|shown| shown.id == *option))
                    .is_some_and(|shown| shown.choices.len() > SEARCHED_CHOICES) =>
            {
                Some(option.clone())
            }
            _ => None,
        };
        let Some(option) = wanted else {
            model.picker = None;
            return;
        };
        if model
            .picker
            .as_ref()
            .is_some_and(|picker| picker.option == option)
        {
            return;
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let id = chat.to_owned();
        let events = cx.subscribe_in(
            &search,
            window,
            move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if let Some(picker) = this
                        .chat
                        .chats
                        .get_mut(&id)
                        .and_then(|model| model.picker.as_mut())
                    {
                        picker.highlight = 0;
                        picker.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.pick_chat_choice_highlighted(&id, window, cx),
                _ => {}
            },
        );
        // The popover takes the keyboard as it opens, after this; the next frame gives
        // it back to the search.
        let handle = search.focus_handle(cx);
        window.focus(&handle, cx);
        window.on_next_frame(move |window, cx| window.focus(&handle, cx));
        let (highlight, current_row) = model
            .state
            .as_ref()
            .and_then(|state| {
                let shown = state.options.iter().find(|shown| shown.id == option)?;
                let current = shown
                    .choices
                    .iter()
                    .position(|choice| choice.value == shown.current)?;
                let all: Vec<usize> = (0..shown.choices.len()).collect();
                let layout = picker_layout(shown, &all, &state.plans);
                let listed = layout
                    .choices
                    .iter()
                    .position(|&row| layout.rows[row] == PickerRow::Choice(current))?;
                Some((listed, layout.choices[listed]))
            })
            .unzip();
        let scroll = UniformListScrollHandle::new();
        if let Some(row) = current_row {
            scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
        model.picker = Some(OptionPicker {
            option,
            search,
            _events: events,
            highlight: highlight.unwrap_or(0),
            scroll,
        });
    }

    /// Up, Down and Enter in an open setting picker.
    fn chat_picker_key(
        &mut self,
        chat: &str,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key.key == "enter" {
            self.pick_chat_choice_highlighted(chat, window, cx);
            return;
        }
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let (Some(picker), Some(state)) = (model.picker.as_mut(), model.state.as_ref()) else {
            return;
        };
        let Some(option) = state.options.iter().find(|shown| shown.id == picker.option) else {
            return;
        };
        let layout = picker.layout(option, &state.plans, cx);
        let count = layout.choices.len();
        if count == 0 {
            return;
        }
        picker.highlight = match key.key.as_str() {
            "down" => (picker.highlight + 1).min(count - 1),
            "up" => picker.highlight.saturating_sub(1),
            "pagedown" => (picker.highlight + PICKER_ROWS).min(count - 1),
            "pageup" => picker.highlight.saturating_sub(PICKER_ROWS),
            _ => return,
        };
        // The first choice of a provider brings its heading into view too.
        let row = layout.choices[picker.highlight];
        let row = if row > 0 && matches!(layout.rows[row - 1], PickerRow::Heading { .. }) {
            row - 1
        } else {
            row
        };
        picker.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Picks the highlighted choice of the open setting picker and closes it.
    fn pick_chat_choice_highlighted(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get(chat) else {
            return;
        };
        let (Some(picker), Some(state)) = (model.picker.as_ref(), model.state.as_ref()) else {
            return;
        };
        let Some(option) = state.options.iter().find(|shown| shown.id == picker.option) else {
            return;
        };
        let layout = picker.layout(option, &state.plans, cx);
        let listed = picker.highlight.min(layout.choices.len().saturating_sub(1));
        let Some(index) = layout.choice(listed) else {
            return;
        };
        let (setting, value) = (option.id.clone(), option.choices[index].value.clone());
        self.set_chat_menu(chat, ChatMenu::Option(setting.clone()), false);
        self.set_chat_option(chat, setting, value, window, cx);
        cx.notify();
    }

    /// The settings without a picker of their own, each a submenu.
    fn render_chat_more(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        options: &[&ChatOption],
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = model.menu == Some(ChatMenu::More);
        let trigger =
            icon_button("chat-more", IconName::SlidersHorizontal, "More settings").menu_open(open);
        // Entries are built only for an open menu: a closed one draws just its button.
        let entries = options.iter().filter(|_| open).map(|option| {
            controls::submenu(
                format!("{}: {}", option.name, option.current_name()),
                option_entries(option, |value| {
                    let (id, setting) = (chat.id.clone(), option.id.clone());
                    Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_chat_option(&id, setting.clone(), value.clone(), window, cx);
                        cx.notify();
                    }))
                }),
            )
        });
        let id = chat.id.clone();
        controls::menu("chat-more-menu", trigger, open, entries.collect::<Vec<_>>())
            .anchor(Anchor::BottomLeft)
            .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
                this.set_chat_menu(&id, ChatMenu::More, *open);
                cx.notify();
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_follow_the_text_before_the_cursor() {
        assert_eq!(wanted("/rev", 4), Some(Wanted::Command("rev")));
        assert_eq!(wanted("/review now", 11), None);
        assert_eq!(
            wanted("look at @src/ma", 15),
            Some(Wanted::File {
                at: 8,
                query: "src/ma"
            })
        );
        assert_eq!(wanted("@", 1), Some(Wanted::File { at: 0, query: "" }));
        assert_eq!(wanted("me@example.com", 14), None);
        assert_eq!(wanted("@a b", 4), None);
        assert_eq!(wanted("plain", 5), None);
    }

    #[test]
    fn commands_match_by_name_prefix_compi_first() {
        let command = |name: &str| ChatCommand {
            name: name.into(),
            description: String::new(),
            hint: None,
        };
        let commands = [command("review"), command("/plan"), command("Reset")];
        let names: Vec<&str> = matching_commands(&commands, "re")
            .iter()
            .map(|command| command.name())
            .collect();
        assert_eq!(names, ["resume", "review", "Reset"]);
        assert_eq!(matching_commands(&commands, "pl").len(), 1);
    }

    #[test]
    fn models_list_subscriptions_first_then_the_current_provider_then_by_name() {
        let choice = |value: &str| ChatChoice {
            value: value.into(),
            name: value.rsplit('/').next().unwrap().into(),
            description: None,
            group: None,
        };
        let option = ChatOption {
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
        };
        let plans = [
            ChatPlan {
                provider: "openai-codex".into(),
                limits: Vec::new(),
            },
            ChatPlan {
                provider: "anthropic".into(),
                limits: vec![
                    compi_protocol::chat::ChatPlanLimit {
                        label: "5 Hour".into(),
                        left_percent: 60,
                    },
                    compi_protocol::chat::ChatPlanLimit {
                        label: "7 Day".into(),
                        left_percent: 80,
                    },
                ],
            },
        ];
        let layout = picker_layout(&option, &[0, 1, 2, 3, 4], &plans);
        let shown: Vec<String> = layout
            .rows
            .iter()
            .map(|row| match row {
                PickerRow::Heading {
                    title,
                    detail,
                    count,
                } => format!(
                    "# {title} ({}) {count}",
                    detail.as_deref().unwrap_or_default()
                ),
                PickerRow::Choice(index) => option.choices[*index].value.clone(),
            })
            .collect();
        assert_eq!(
            shown,
            [
                "# Anthropic (Subscription · 5 Hour: 60% left) 2",
                "anthropic/opus",
                "anthropic/sonnet",
                "# OpenAI Codex (Subscription) 1",
                "openai-codex/gpt",
                "# OpenRouter (Pay per use) 1",
                "openrouter/glm",
                "# Amazon Bedrock (Pay per use) 1",
                "amazon-bedrock/haiku",
            ]
        );
        // Arrow keys step through choices only.
        assert_eq!(layout.choice(2), Some(3));
        assert_eq!(layout.choices.len(), 5);
    }

    #[test]
    fn a_harness_command_replaces_compis_of_the_same_name() {
        let resume = ChatCommand {
            name: "Resume".into(),
            description: String::new(),
            hint: None,
        };
        assert_eq!(
            compi_command("/resume fix login", &[]),
            Some((CompiCommand::Resume, "fix login"))
        );
        assert_eq!(compi_command("/new", &[]), Some((CompiCommand::New, "")));
        assert_eq!(compi_command("/newer", &[]), None);
        assert_eq!(
            compi_command("/resume", std::slice::from_ref(&resume)),
            None
        );
        let listed = matching_commands(std::slice::from_ref(&resume), "res");
        assert_eq!(listed.len(), 1);
        assert!(matches!(listed[0], ListedCommand::Harness(_)));
    }

    #[test]
    fn usage_reads_short() {
        assert_eq!(token_label(950), "950");
        assert_eq!(token_label(31_000), "31k");
        assert_eq!(token_label(1_500), "1.5k");
        assert_eq!(token_label(1_000_000), "1M");
        assert_eq!(cost_label(0.4199, None), "$0.42");
        assert_eq!(cost_label(1.0, Some("EUR")), "1.00 EUR");
    }
}
