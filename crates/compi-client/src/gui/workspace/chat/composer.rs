//! Under the transcript: what waits on the reader (a failed harness, a question, a
//! permission whose tool call is not in the transcript, the turn running), then the
//! composer with its attachments and its `/` command and `@` file lists, and a row of the
//! harness's settings, context use and Send or Stop.
use super::*;
use compi_protocol::chat::{
    ChatCommand, ChatOption, MAX_CHAT_IMAGE_BYTES, MAX_CHAT_IMAGES, MAX_CHAT_PROMPT_BYTES,
};
use controls::MenuEntry;

/// Typing pauses this long before a file search.
const FILE_SEARCH_DELAY: Duration = Duration::from_millis(150);
/// The most files and commands a list shows.
const MAX_COMPLETIONS: usize = 50;
/// The settings the model picker holds, by category; the rest go under More.
const PICKER_SETTINGS: [&str; 3] = ["model", "thought_level", "mode"];
/// The composer card's corner radius.
const CARD_RADIUS: f32 = 10.0;
/// A draft pane's composer card: rounder, and its field two lines tall.
const DRAFT_CARD_RADIUS: f32 = 14.0;
const DRAFT_FIELD_HEIGHT: f32 = 60.0;
/// The harness's mark in the empty draft.
const MARK_SIZE: f32 = 14.0;
/// A paste this long, or one that would make the draft too long to send, is attached as
/// a text file instead.
const LARGE_PASTE_BYTES: usize = 32 * 1024;
/// The side of an attached image's thumbnail.
const THUMB_SIZE: f32 = 48.0;

/// An image attached to the draft, with its thumbnail.
pub(super) struct DraftImage {
    pub(super) image: ChatImage,
    thumb: Arc<gpui::Image>,
}

impl DraftImage {
    /// `bytes` of type `mime`, already small enough to send, named `name`.
    fn new(name: String, mime: &str, bytes: Vec<u8>) -> Self {
        use base64::Engine as _;
        let format = match mime {
            "image/jpeg" => gpui::ImageFormat::Jpeg,
            "image/gif" => gpui::ImageFormat::Gif,
            "image/webp" => gpui::ImageFormat::Webp,
            _ => gpui::ImageFormat::Png,
        };
        Self {
            image: ChatImage {
                name,
                mime: mime.to_owned(),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            },
            thumb: Arc::new(gpui::Image::from_bytes(format, bytes)),
        }
    }

    /// Frees the thumbnail's decoded frames.
    pub(super) fn release(&self, cx: &mut App) {
        gpui::ImageSource::Image(self.thumb.clone()).remove_asset(cx);
    }
}

/// Up and Down walking the chat's earlier prompts.
pub(super) struct Recall {
    /// How far back from the newest prompt: 0 is the newest.
    back: usize,
    /// The prompt shown; editing it ends the walk.
    text: SharedString,
}

/// A queued prompt open in the composer for editing, and the draft it set aside.
pub(super) struct QueuedEdit {
    pub(super) id: u64,
    pub(super) draft: SharedString,
    pub(super) attachments: Vec<String>,
    pub(super) images: Vec<DraftImage>,
}

/// The chat's prompts, newest first, a prompt sent twice in a row once.
fn earlier_prompts(entries: &[ChatEntry]) -> Vec<&str> {
    let mut prompts: Vec<&str> = Vec::new();
    for entry in entries.iter().rev() {
        if let ChatEntry::User { text, .. } = entry {
            let text = text.trim();
            if !text.is_empty() && prompts.last() != Some(&text) {
                prompts.push(text);
            }
        }
    }
    prompts
}

/// Keeps pasted `text` as `pasted-text.txt`, in a folder of its own under the system's
/// temporary folder, for the chat to read.
fn write_paste(text: &str) -> std::io::Result<std::path::PathBuf> {
    let folder = std::env::temp_dir()
        .join("compi-paste")
        .join(now_ms().to_string());
    std::fs::create_dir_all(&folder)?;
    let path = folder.join("pasted-text.txt");
    std::fs::write(&path, text)?;
    Ok(path)
}

/// `text` as a Markdown code block, fenced with more backticks than any run in it.
pub(super) fn fenced(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{}\n{fence}", text.trim_end_matches(['\n', '\r']))
}

/// What was picked or dropped, sorted: files by the path the chat's environment knows
/// them by, images read, and why any were left out.
struct PickedAttachments {
    files: Vec<String>,
    images: Vec<DraftImage>,
    refused: Option<String>,
}

/// Reads `picked` for a chat in `environment`: images by their bytes, shrunk to fit a
/// prompt, other files by path, which a remote chat cannot reach.
fn read_attachments(
    environment: &ContentEnvironment,
    remote: bool,
    picked: Vec<std::path::PathBuf>,
) -> PickedAttachments {
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
        if ChatImage::mime_for(&name).is_some() {
            let image = std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| crate::image_input::chat_image(bytes, MAX_CHAT_IMAGE_BYTES));
            match image {
                Ok((bytes, mime)) => read.images.push(DraftImage::new(name, mime, bytes)),
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
    /// A choice field's Other chip is picked, showing its text field.
    other: bool,
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

/// A `/` command Compi runs itself, whatever the harness. `/new`, `/resume` and
/// `/sessions` give way to a harness command of the same name; the rest are always
/// Compi's (`always_compi`), and a harness command of their name is not listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompiCommand {
    New,
    Resume,
    Sessions,
    Model,
    Providers,
    Login,
    Logout,
    Update,
}

impl CompiCommand {
    const ALL: [Self; 8] = [
        Self::New,
        Self::Resume,
        Self::Sessions,
        Self::Model,
        Self::Providers,
        Self::Login,
        Self::Logout,
        Self::Update,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Resume => "resume",
            Self::Sessions => "sessions",
            Self::Model => "model",
            Self::Providers => "providers",
            Self::Login => "login",
            Self::Logout => "logout",
            Self::Update => "update",
        }
    }

    fn hint(self) -> Option<&'static str> {
        match self {
            Self::New | Self::Model | Self::Providers | Self::Update => None,
            Self::Resume | Self::Sessions => Some("[title or id]"),
            Self::Login | Self::Logout => Some("[provider]"),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::New => "Start a new session",
            Self::Resume => "Pick an earlier session in this folder to continue",
            Self::Sessions => "List the sessions in this folder",
            Self::Model => "Pick the model and thinking level",
            Self::Providers => "See the model providers and accounts this agent uses",
            Self::Login => "Sign in to a model provider",
            Self::Logout => "Sign out of a model provider",
            Self::Update => "Update this agent to its newest version",
        }
    }

    /// Compi's own whatever the harness offers: what the harness's command of the same
    /// name does needs its own window (`/login`), or Compi's does more (`/model`).
    fn always_compi(self) -> bool {
        !matches!(self, Self::New | Self::Resume | Self::Sessions)
    }

    /// Whether the harness offers a command of this name that replaces Compi's.
    fn shadowed(self, harness: &[ChatCommand]) -> bool {
        !self.always_compi() && harness.iter().any(|command| self.named(command))
    }

    /// Whether `command` has this command's name.
    fn named(self, command: &ChatCommand) -> bool {
        command
            .name
            .trim_start_matches('/')
            .eq_ignore_ascii_case(self.name())
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
        .chain(
            harness
                .iter()
                .filter(|command| {
                    !CompiCommand::ALL
                        .iter()
                        .any(|compi| compi.always_compi() && compi.named(command))
                })
                .map(ListedCommand::Harness),
        )
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

/// A question's message without the lines that repeat a field's title, which shows over
/// its field instead (an HTML form's legend is both).
fn question_heading(question: &ChatQuestion) -> String {
    question
        .message
        .lines()
        .filter(|line| {
            let line = line.trim();
            !question
                .fields
                .iter()
                .any(|field| field.title.trim() == line)
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
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

    /// Completes the highlighted item: a command fills in `/name ` (`/model`, which takes
    /// nothing more, runs at once), a file becomes an attachment and its `@` text goes.
    /// Whether there was one.
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
        if let CompletionKind::Commands { query } = &completion.kind {
            let harness = model
                .state
                .as_ref()
                .map(|state| state.commands.as_slice())
                .unwrap_or_default();
            if matches!(
                matching_commands(harness, query).get(completion.highlight),
                Some(ListedCommand::Compi(CompiCommand::Model))
            ) {
                self.run_compi_command(chat, CompiCommand::Model, String::new(), window, cx);
                return true;
            }
        }
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
                        "Your answer"
                    };
                    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
                }),
                other: false,
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
            form.other = false;
        }
    }

    /// Picks or unpicks the Other chip of field `field`: picked, its text field shows and
    /// takes the keyboard; one-choice fields drop their picked choice.
    fn pick_chat_other(&mut self, chat: &str, field: usize, window: &mut Window, cx: &mut App) {
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
        form.other = !form.other;
        if form.other {
            if !multi {
                form.picked.clear();
            }
            if let Some(text) = &form.text {
                text.update(cx, |state, cx| state.focus(window, cx));
            }
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
            // A choice field's text counts only while its Other chip is picked.
            let text = answer
                .text
                .as_ref()
                .filter(|_| field.choices.is_empty() || answer.other)
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

    /// Under the transcript: notices, what waits on the reader, the composer and its row,
    /// then where the chat works; a `draft` names that in its header instead, and a
    /// failed harness shows here, as a draft has no transcript to show it in.
    pub(super) fn render_chat_footer(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        draft: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let harness = chat.harness.name();
        let state = model.state.as_ref();
        let transcript = state.is_some_and(|state| !state.entries.is_empty());
        let status = state.map(|state| &state.status);
        let failed = match status {
            Some(ChatStatus::Failed { message }) if transcript || draft => Some(message.clone()),
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
            dark: self.theme.is_dark(),
            syntax: gpui_component::theme::Theme::global(cx)
                .highlight_theme
                .clone(),
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
            .children(state.and_then(|state| self.render_chat_providers(chat, state, cx)))
            .children(state.and_then(|state| self.render_chat_update(chat, state, cx)))
            .children(state.and_then(|state| self.render_chat_sign_in(chat, state, cx)))
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
                        .child(controls::spinner(muted))
                        .child(format!(
                            "Working · {}",
                            duration_label(now_ms().saturating_sub(since_ms))
                        )),
                )
            })
            .when_some(
                state.filter(|state| !state.queued.is_empty()),
                |footer, state| {
                    let editing = model.editing.as_ref().map(|edit| edit.id);
                    footer.child(self.render_chat_queue(chat, state, editing, cx))
                },
            )
            .child(self.render_chat_card(chat, model, &frame, draft, cx))
            .when(!draft, |footer| {
                footer.child(self.render_chat_context(chat, model))
            })
            .into_any_element()
    }

    /// Prompts waiting for the running turn, oldest first, each with a pencil to edit it
    /// in the composer (the one being edited marked) and × to drop it. A queue left after
    /// a stopped turn waits for the next prompt.
    fn render_chat_queue(
        &self,
        chat: &ChatContent,
        state: &ChatState,
        editing: Option<u64>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let held = matches!(state.status, ChatStatus::Ready);
        let rows = state.queued.iter().enumerate().map(|(index, queued)| {
            let queued_id = queued.id;
            let (edit_id, remove_id) = (chat.id.clone(), chat.id.clone());
            let edited = editing == Some(queued_id);
            let attached = queued.files.len() + queued.images.len();
            let text = queued.text.lines().find(|line| !line.trim().is_empty());
            div()
                .flex()
                .items_center()
                .gap_2()
                .pl_2()
                .rounded(ui_rems(6.0))
                .border_1()
                .border_color(color(if edited {
                    colors.accent
                } else {
                    colors.surface
                }))
                .bg(color(colors.surface))
                .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                .child(
                    div()
                        .flex_none()
                        .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                        .text_color(muted)
                        .child(if edited {
                            "Editing · Esc cancels"
                        } else {
                            "Queued"
                        }),
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
                .when(!edited, |row| {
                    row.child(
                        icon_button(("chat-queued-edit", index), IconName::SquarePen, "Edit")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.edit_queued_chat(&edit_id, queued_id, window, cx);
                                cx.notify();
                            })),
                    )
                })
                .child(
                    icon_button(("chat-queued", index), IconName::Close, "Remove from queue")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.remove_queued_chat(&remove_id, queued_id, window, cx);
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

    /// A question the harness asks: its message, then each field in the form's order
    /// under its title (shown once, not repeated by the message): a row of choices (one or
    /// several) with an Other chip that opens a text field where the harness takes one, a
    /// text field for a field without choices, then Submit and Skip.
    fn render_chat_question(
        &self,
        chat: &ChatContent,
        question: &ChatQuestion,
        form: &QuestionForm,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let refusal = Self::chat_question_answers(question, form, cx).err();
        let heading = question_heading(question);
        let fields =
            question
                .fields
                .iter()
                .zip(&form.fields)
                .enumerate()
                .map(|(index, (field, answer))| {
                    let other = field.other && !field.choices.is_empty();
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
                        })
                        .chain(other.then(|| {
                            let id = chat.id.clone();
                            text_button(("chat-choice", index * 1000 + 999), "Other")
                                .selected(answer.other)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.pick_chat_other(&id, index, window, cx);
                                    cx.notify();
                                }))
                        }));
                    let text = answer
                        .text
                        .as_ref()
                        .filter(|_| field.choices.is_empty() || answer.other);
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .when(!field.title.is_empty(), |column| {
                            column.child(
                                div()
                                    .text_size(ui_rems(UI_SMALL_TEXT_SIZE))
                                    .child(field.title.clone()),
                            )
                        })
                        .when(!field.choices.is_empty(), |column| {
                            column.child(div().flex().flex_wrap().gap_1().children(choices))
                        })
                        .when_some(text, |column, text| {
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
            .when(!heading.is_empty(), |card| {
                card.child(div().text_size(ui_rems(UI_SMALL_TEXT_SIZE)).child(heading))
            })
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
    /// card puts the keyboard in the draft. A `draft` pane's card is rounder and two
    /// lines tall.
    fn render_chat_card(
        &self,
        chat: &ChatContent,
        model: &ChatModel,
        frame: &RowFrame,
        draft: bool,
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
                        model.images.remove(index).release(cx);
                    } else if !image && index < model.attachments.len() {
                        model.attachments.remove(index);
                    }
                }
                cx.notify();
            }))
        };
        // A file outside the chat's folder (a long paste, say) goes by its name.
        let files = model.attachments.iter().enumerate().map(|(index, file)| {
            let label = rows::relative_to(file, &model.folder);
            let label = if label == *file {
                file.rsplit(['/', '\\']).next().unwrap_or(file).to_owned()
            } else {
                label
            };
            rows::file_chip(label.into(), &frame.colors)
                .pr_0()
                .child(remove(index, false))
        });
        let images = model.images.iter().enumerate().map(|(index, draft)| {
            div()
                .id(("chat-thumb", index))
                .relative()
                .flex_none()
                .size(ui_rems(THUMB_SIZE))
                .rounded(ui_rems(6.0))
                .overflow_hidden()
                .border_1()
                .border_color(color(colors.border))
                .child(
                    img(gpui::ImageSource::Image(draft.thumb.clone()))
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .rounded(ui_rems(4.0))
                        .bg(color(colors.surface))
                        .child(remove(index, true)),
                )
                .tooltip(controls::tooltip(draft.image.name.clone(), None, None))
        });
        div()
            .relative()
            .flex()
            .flex_col()
            .rounded(ui_rems(if draft { DRAFT_CARD_RADIUS } else { CARD_RADIUS }))
            .border_1()
            .border_color(color(colors.border))
            .in_focus(move |style| style.border_color(color(colors.accent)))
            .group_drag_over::<gpui::ExternalPaths>(CHAT_DROP_GROUP, move |style| {
                style.border_color(color(colors.accent)).border_dashed()
            })
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
                            .items_end()
                            .gap_1()
                            .children(images)
                            .children(files),
                    )
                },
            )
            .child(
                div()
                    .relative()
                    .px_0p5()
                    .pt_1()
                    .when(draft, |field| field.min_h(ui_rems(DRAFT_FIELD_HEIGHT)))
                    // The field pads its text 10 px across and 8 px down; the hint sits
                    // where typing starts, 12 px in from the card and 12 px down.
                    .child(
                        Textarea::new(&model.composer)
                            .appearance(false)
                            .font_weight(FontWeight::NORMAL),
                    )
                    .when(empty, |draft| {
                        // The hint ends in "…" where the card is too narrow for it.
                        draft.child(
                            div()
                                .absolute()
                                .top(ui_rems(12.0))
                                .left(ui_rems(12.0))
                                .right(ui_rems(12.0))
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_color(muted)
                                .child(
                                    img(harness_mark(chat.harness))
                                        .flex_none()
                                        .size(ui_rems(MARK_SIZE)),
                                )
                                .child(div().min_w_0().truncate().child(if working {
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
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .px_1p5()
                    // With Send's right margin, Send sits as far from the bottom edge
                    // as from the right.
                    .pb_2p5()
                    .pt_1()
                    .child(self.render_chat_controls(chat, model, cx)),
            )
            .child(self.render_chat_drop_zone(&colors))
            // The card's width, which the model picker takes, and the picker, which opens
            // from its top edge.
            .child({
                let anchor = model.picker_anchor.clone();
                gpui::canvas(
                    move |bounds, _, _| anchor.card_width.set(bounds.size.width),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .children(self.render_chat_model_popover(chat, model, cx))
            .into_any_element()
    }

    /// Over the composer card while files are dragged over the chat: what dropping them
    /// does. Hidden otherwise; the card's border turns dashed with it.
    fn render_chat_drop_zone(&self, colors: &ThemeColors) -> AnyElement {
        let accent = color(colors.accent);
        let muted = color(modal_text_color(colors.muted, colors));
        div()
            .absolute()
            .inset_0()
            .rounded(ui_rems(CARD_RADIUS))
            .overflow_hidden()
            .bg(color(colors.surface).opacity(0.94))
            .opacity(0.)
            .group_drag_over::<gpui::ExternalPaths>(CHAT_DROP_GROUP, |style| style.opacity(1.))
            // GPUI applies drag styles only to an element with a hit area; this gives the
            // zone one, which lets clicks through to the card under it.
            .drag_over::<gpui::ExternalPaths>(|style, _, _, _| style)
            .child(
                div()
                    .size_full()
                    .bg(accent.opacity(0.08))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(controls::glyph(IconName::Paperclip, accent))
                            .child("Drop to attach"),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(UI_MICRO_TEXT_SIZE))
                            .text_color(muted)
                            .child("Images go with your message · other files by their path"),
                    ),
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
        let others: Vec<&ChatOption> = state.map_or_else(Vec::new, |state| {
            state
                .options
                .iter()
                .filter(|option| {
                    !option
                        .category
                        .as_deref()
                        .is_some_and(|category| PICKER_SETTINGS.contains(&category))
                })
                .collect()
        });
        let pickers = self.render_chat_models(chat, model, cx);
        let usage = state.and_then(|state| state.usage.as_ref()).map(|usage| {
            let mut label = format!("{} / {}", token_label(usage.used), token_label(usage.size));
            if let Some(cost) = usage.cost {
                label.push_str("  ·  ");
                label.push_str(&cost_label(cost, usage.currency.as_deref()));
            }
            div()
                .id("chat-usage")
                .min_w_0()
                // Shrinks well before the model button does.
                .flex_shrink(8.0)
                .truncate()
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
        let action = if model.editing.is_some() {
            icon_button("chat-save-queued", IconName::Check, "Save queued message")
                .primary()
                .header()
                .key("enter")
                .loading(model.requests > 0)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.send_chat(&id, false, window, cx);
                    cx.notify();
                }))
        } else if working {
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
        // Narrow, the usage gives way first, then the model button; + and Send stay.
        div()
            .flex()
            .items_center()
            .gap_0p5()
            .min_w_0()
            .child(div().flex_none().child(attach))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .min_w_0()
                    .overflow_hidden()
                    .children(pickers)
                    .when(!others.is_empty(), |row| {
                        row.child(self.render_chat_more(chat, model, &others, cx))
                    }),
            )
            .child(div().flex_1())
            .children(usage)
            // Inset like the + glyph and the draft on the left, not flush with the row.
            .child(div().flex_none().ml_2().mr_1().child(action))
            .into_any_element()
    }

    /// The card's +: the system's file picker, attaching what it returns.
    fn pick_chat_attachments(&mut self, chat: &str, cx: &mut Context<Self>) {
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
            this.update(cx, |this, cx| this.attach_chat_paths(&chat, picked, cx))
                .ok();
        })
        .detach();
    }

    /// Attaches `paths`, picked or dropped on chat `chat`, to its next prompt. Images
    /// (PNG, JPEG, GIF, WebP) are read here, shrunk to fit and sent with the prompt;
    /// other files are attached by the path the chat's environment knows them by. A file
    /// it cannot reach, an unreadable image, or images past the limit are named under
    /// the card instead.
    pub(super) fn attach_chat_paths(
        &mut self,
        chat: &str,
        paths: Vec<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some((_, content)) = self.chat_pane(chat) else {
            return;
        };
        let remote = self.target.is_remote();
        let chat = chat.to_owned();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { read_attachments(&content.environment, remote, paths) })
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
                let mut over_limit = false;
                for image in read.images {
                    if model
                        .images
                        .iter()
                        .any(|kept| kept.image.data == image.image.data)
                    {
                        continue;
                    }
                    if model.images.len() < MAX_CHAT_IMAGES {
                        model.images.push(image);
                    } else {
                        over_limit = true;
                    }
                }
                model.error = match (over_limit, read.refused) {
                    (true, Some(refused)) => Some(format!(
                        "A prompt carries at most {MAX_CHAT_IMAGES} images. {refused}"
                    )),
                    (true, None) => {
                        Some(format!("A prompt carries at most {MAX_CHAT_IMAGES} images"))
                    }
                    (false, refused) => refused,
                };
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
                        let extension = mime.trim_start_matches("image/").replace("jpeg", "jpg");
                        let name = format!("Pasted image {}.{extension}", model.images.len() + 1);
                        let image = DraftImage::new(name, mime, bytes);
                        if !model
                            .images
                            .iter()
                            .any(|kept| kept.image.data == image.image.data)
                        {
                            model.images.push(image);
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

    /// Ctrl+V (Cmd+V) with text on the clipboard in chat `chat`'s composer: a paste of
    /// [`LARGE_PASTE_BYTES`] or more, or one that would make the draft too long to send,
    /// is attached as a text file instead; with `inline` (Ctrl+Shift+V) it is typed in
    /// whatever its length. Whether it was handled here; else the field pastes.
    pub(super) fn paste_chat_text(
        &mut self,
        chat: &str,
        inline: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return false;
        };
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return false;
        };
        if inline {
            model.composer.update(cx, |composer, cx| {
                composer.insert(text, window, cx);
            });
            self.chat_composer_changed(chat, cx);
            return true;
        }
        let draft = model.composer.read(cx).value().len();
        if text.len() < LARGE_PASTE_BYTES && draft + text.len() <= MAX_CHAT_PROMPT_BYTES {
            return false;
        }
        match write_paste(&text) {
            Ok(path) => self.attach_chat_paths(chat, vec![path], cx),
            Err(error) => {
                model.error = Some(format!("Could not keep the paste as a file: {error}"))
            }
        }
        true
    }

    /// Up or Down (`older`) in chat `chat`'s composer walks its earlier prompts: Up in an
    /// empty draft brings the newest back, Up on a recalled prompt's first line the one
    /// before it, Down on its last line the one after, and past the newest an empty draft.
    /// Editing a recalled prompt ends the walk. Whether the key was taken.
    pub(super) fn recall_chat_prompt(
        &mut self,
        chat: &str,
        older: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return false;
        };
        if model.editing.is_some() {
            return false;
        }
        let composer = model.composer.read(cx);
        let (value, cursor) = (composer.value(), composer.cursor());
        let walking = model
            .recall
            .as_ref()
            .filter(|recall| recall.text == value)
            .map(|recall| recall.back);
        let back = match walking {
            Some(back) => {
                let beyond = if older {
                    value.get(..cursor)
                } else {
                    value.get(cursor..)
                };
                if beyond.is_none_or(|beyond| beyond.contains('\n')) {
                    return false;
                }
                if older {
                    back + 1
                } else if let Some(back) = back.checked_sub(1) {
                    back
                } else {
                    model.recall = None;
                    model.composer.update(cx, |composer, cx| {
                        composer.set_value(String::new(), window, cx);
                    });
                    return true;
                }
            }
            None if older
                && value.is_empty()
                && model.attachments.is_empty()
                && model.images.is_empty() =>
            {
                0
            }
            None => return false,
        };
        let entries = model
            .state
            .as_ref()
            .map_or(&[][..], |state| state.entries.as_slice());
        let Some(text) = earlier_prompts(entries)
            .get(back)
            .map(|text| SharedString::from(text.to_string()))
        else {
            // Up on the oldest stays on it.
            return walking.is_some();
        };
        model.composer.update(cx, |composer, cx| {
            composer.set_value(text.clone(), window, cx);
        });
        model.recall = Some(Recall { back, text });
        model.completion = None;
        true
    }

    /// Opens queued prompt `queued` of chat `chat` in its composer for editing, setting
    /// the draft and its attachments aside until the edit is saved or dropped.
    pub(super) fn edit_queued_chat(
        &mut self,
        chat: &str,
        queued: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let Some(text) = model
            .state
            .as_ref()
            .and_then(|state| state.queued.iter().find(|item| item.id == queued))
            .map(|item| item.text.clone())
        else {
            return;
        };
        if model.requests > 0 {
            return;
        }
        model.end_queued_edit(window, cx);
        let draft = model.composer.read(cx).value();
        model.editing = Some(QueuedEdit {
            id: queued,
            draft,
            attachments: std::mem::take(&mut model.attachments),
            images: std::mem::take(&mut model.images),
        });
        model.recall = None;
        model.completion = None;
        model.composer.update(cx, |composer, cx| {
            composer.set_value(text.clone(), window, cx);
            let end = composer.text().offset_to_position(text.len());
            composer.set_cursor_position(end, window, cx);
        });
        window.focus(&model.composer.focus_handle(cx), cx);
    }

    /// Alt+Up with the caret at the draft's start edits the last queued prompt; whether
    /// there was one.
    pub(super) fn edit_last_queued_chat(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(model) = self.chat.chats.get(chat) else {
            return false;
        };
        if model.editing.is_some() || model.composer.read(cx).cursor() != 0 {
            return false;
        }
        let Some(last) = model
            .state
            .as_ref()
            .and_then(|state| state.queued.last())
            .map(|item| item.id)
        else {
            return false;
        };
        self.edit_queued_chat(chat, last, window, cx);
        true
    }

    /// The queued prompt being edited in chat `chat` went to the harness first: the edit
    /// stays as the draft when nothing was set aside, else the set-aside draft comes back.
    pub(super) fn settle_queued_edit(
        &mut self,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.chat.chats.get_mut(chat) else {
            return;
        };
        let (Some(edit), Some(state)) = (&model.editing, &model.state) else {
            return;
        };
        if model.requests > 0 || state.queued.iter().any(|item| item.id == edit.id) {
            return;
        }
        if edit.draft.is_empty() && edit.attachments.is_empty() && edit.images.is_empty() {
            model.editing = None;
            model.error = Some(
                "That message went before your edit was saved; the edit is in the message box"
                    .into(),
            );
        } else {
            model.end_queued_edit(window, cx);
            model.error = Some("That message went before your edit was saved".into());
        }
    }

    /// Adds the text waiting for each chat with client state (terminal text sent to it)
    /// to the end of its draft, or of the draft a queued edit set aside.
    pub(super) fn take_pending_chat_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ready: Vec<String> = self
            .chat
            .pending
            .keys()
            .filter(|id| self.chat.chats.contains_key(*id))
            .cloned()
            .collect();
        for id in ready {
            let (Some(text), Some(model)) =
                (self.chat.pending.remove(&id), self.chat.chats.get_mut(&id))
            else {
                continue;
            };
            let join = |draft: &str| {
                if draft.trim().is_empty() {
                    text.clone()
                } else {
                    format!("{}\n\n{text}", draft.trim_end())
                }
            };
            if let Some(edit) = &mut model.editing {
                edit.draft = join(&edit.draft).into();
                continue;
            }
            let joined = join(&model.composer.read(cx).value());
            model.recall = None;
            model.composer.update(cx, |composer, cx| {
                composer.set_value(joined.clone(), window, cx);
                let end = composer.text().offset_to_position(joined.len());
                composer.set_cursor_position(end, window, cx);
            });
        }
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
    fn model_always_opens_compis_picker() {
        let model = ChatCommand {
            name: "/model".into(),
            description: String::new(),
            hint: None,
        };
        let harness = std::slice::from_ref(&model);
        assert_eq!(
            compi_command("/model", harness),
            Some((CompiCommand::Model, ""))
        );
        let listed = matching_commands(harness, "mod");
        assert_eq!(listed.len(), 1);
        assert!(matches!(
            listed[0],
            ListedCommand::Compi(CompiCommand::Model)
        ));
    }

    #[test]
    fn recall_walks_typed_prompts_newest_first_without_repeats_in_a_row() {
        let user = |text: &str| ChatEntry::User {
            text: text.into(),
            files: Vec::new(),
            images: Vec::new(),
            sent_ms: None,
            ended_ms: None,
        };
        let entries = [
            user("first"),
            ChatEntry::Agent { text: "ok".into() },
            user("again\n"),
            user("again"),
            user("  "),
            user("first"),
        ];
        assert_eq!(earlier_prompts(&entries), ["first", "again", "first"]);
    }

    #[test]
    fn a_fence_outlasts_the_backticks_inside() {
        assert_eq!(fenced("ls\n"), "```\nls\n```");
        assert_eq!(fenced("a ```` b"), "`````\na ```` b\n`````");
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
