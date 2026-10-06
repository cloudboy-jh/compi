//! The "Shell prompt" group in Settings → Terminal: detect the prompt providers in
//! the daemon's shell environment, browse every style with its rendered prompt,
//! and apply one from a dropdown that opens under it. Previews never touch the
//! terminal theme, layout, or anything persisted; files change only through a
//! plan the user confirmed.
use super::*;
use compi_protocol::prompt::{
    MAX_PREVIEW_STYLES, PromptConfig, PromptEnvironment, PromptFileChange, PromptOperation,
    PromptPlan, PromptProvider, PromptRender, PromptRequest, PromptResponse, PromptStyle,
    PromptStyleRender,
};
use gpui::{AnyElement, HighlightStyle, ListAlignment, ListState, StyledText, list};

static NEXT_PROMPT_REQUEST: AtomicU64 = AtomicU64::new(1);
const UNEXPECTED_RESPONSE: &str = "The daemon returned an unexpected prompt response.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum PromptJob {
    Detect,
    /// A batch of renders for the style list.
    Renders,
    Plan,
    Execute,
}

/// One focusable control. `prompt_actions` lists them in render order, so a
/// control's position is its offset after the Terminal section's own controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PromptAction {
    Details,
    Refresh,
    Distribution(usize),
    Provider(PromptProvider),
    /// The whole style list; arrow keys move the selection, Enter opens its dropdown.
    Styles,
    Outside,
    TurnOff,
    History,
    Restore(usize),
    /// The open dropdown's Apply, Turn off or Restore button.
    Confirm,
    Cancel,
}

/// Distribution the render came from, provider, and style.
type RenderKey = (Option<String>, PromptProvider, PromptStyle);

/// Provider output, parsed once when it arrives.
struct ParsedRender {
    left: Vec<Vec<AnsiSpan>>,
    right: Vec<AnsiSpan>,
}

impl ParsedRender {
    fn new(render: &PromptRender) -> Self {
        Self {
            left: ansi_lines(parse_ansi(&render.left)),
            right: ansi_lines(parse_ansi(&render.right))
                .into_iter()
                .find(|line| !line.is_empty())
                .unwrap_or_default(),
        }
    }
}

struct StyleRender {
    success: ParsedRender,
    error: Option<String>,
}

impl StyleRender {
    fn new(render: &PromptStyleRender) -> Self {
        Self {
            success: ParsedRender::new(&render.success),
            error: render.error.clone(),
        }
    }

    fn failed(error: String) -> Self {
        Self {
            success: ParsedRender::new(&PromptRender::default()),
            error: Some(error),
        }
    }
}

struct PendingRenders {
    request: u64,
    generation: u64,
    distribution: Option<String>,
    provider: PromptProvider,
    styles: Vec<PromptStyle>,
}

/// One entry of the style list; the applied style is pinned first.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StyleRow {
    style: PromptStyle,
    label: String,
    current: bool,
}

/// The confirm that opens under the clicked style row, Turn off, or a history
/// row. Its operation says where: Apply under its style, Disable under Turn
/// off, Restore under its backup.
struct PromptDropdown {
    operation: PromptOperation,
    /// Planned in the background; the confirm button waits for it.
    plan: Option<PromptPlan>,
    error: Option<String>,
}

pub(in crate::gui) struct PromptSettingsState {
    /// WSL2 distribution to configure; `None` is the default distribution.
    distribution: Option<String>,
    /// Distributions from the last successful detection, kept while another loads.
    distributions: Vec<String>,
    environment: Option<Result<PromptEnvironment, String>>,
    loading: bool,
    detect_request: u64,
    selected: Option<PromptConfig>,
    /// Also use the prompt in the login shell through its startup file.
    outside: bool,
    details: bool,
    history: bool,
    renders: HashMap<RenderKey, StyleRender>,
    /// Bumped by Refresh so renders requested before it are dropped.
    render_generation: u64,
    pending_renders: Option<PendingRenders>,
    /// At most one dropdown is open.
    dropdown: Option<PromptDropdown>,
    executing: bool,
    /// The open dropdown's plan or execute request; other replies are stale.
    action_request: u64,
    /// Focused pane's distribution and directory when the section opened.
    cwd: Option<(Option<String>, String)>,
    error: Option<String>,
    /// Style rows of the selected provider, in list order.
    rows: Vec<StyleRow>,
    /// Virtualized style list; item `i` is `rows[i]` plus any dropdown under it.
    style_list: ListState,
    /// Row to scroll fully into view once the list has measured it again.
    reveal_row: Option<usize>,
}

impl Default for PromptSettingsState {
    fn default() -> Self {
        Self {
            distribution: None,
            distributions: Vec::new(),
            environment: None,
            loading: false,
            detect_request: 0,
            selected: None,
            outside: false,
            details: false,
            history: false,
            renders: HashMap::new(),
            render_generation: 0,
            pending_renders: None,
            dropdown: None,
            executing: false,
            action_request: 0,
            cwd: None,
            error: None,
            rows: Vec::new(),
            style_list: ListState::new(0, ListAlignment::Top, px(200.0)),
            reveal_row: None,
        }
    }
}

impl PromptSettingsState {
    fn environment(&self) -> Option<&PromptEnvironment> {
        self.environment
            .as_ref()
            .and_then(|result| result.as_ref().ok())
    }

    /// Recomputes the style rows and resets the list when they change.
    fn sync_rows(&mut self) {
        let rows = match (self.environment(), &self.selected) {
            (Some(environment), Some(selected)) if installed(environment, selected.provider) => {
                style_rows(environment, selected.provider)
            }
            _ => Vec::new(),
        };
        if rows != self.rows {
            self.style_list.reset(rows.len());
            self.rows = rows;
        }
    }

    fn selected_row(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|row| row.style == selected.style)
    }

    /// Measures a row again after its height changed.
    fn remeasure_row(&self, index: Option<usize>) {
        if let Some(index) = index.filter(|index| *index < self.rows.len()) {
            self.style_list.splice(index..index + 1, 1);
        }
    }

    /// The style row an Apply dropdown sits under.
    fn dropdown_row(&self) -> Option<usize> {
        let PromptOperation::Apply { config, .. } = &self.dropdown.as_ref()?.operation else {
            return None;
        };
        if self.selected.as_ref()?.provider != config.provider {
            return None;
        }
        self.rows.iter().position(|row| row.style == config.style)
    }

    /// The control the dropdown opens under; `None` when that is not shown.
    fn dropdown_anchor(&self) -> Option<PromptAction> {
        let environment = self.environment()?;
        Some(match &self.dropdown.as_ref()?.operation {
            PromptOperation::Apply { .. } => {
                self.dropdown_row()?;
                PromptAction::Styles
            }
            PromptOperation::Disable => {
                anything_applied(environment).then_some(PromptAction::TurnOff)?
            }
            PromptOperation::Restore { backup } => {
                if !self.history {
                    return None;
                }
                PromptAction::Restore(
                    environment
                        .backups
                        .iter()
                        .position(|candidate| candidate.id == *backup)?,
                )
            }
        })
    }

    /// Changes the dropdown and re-measures the style rows it leaves and enters;
    /// the row it is under is revealed after that measurement.
    fn update_dropdown(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.dropdown_row();
        change(self);
        let after = self.dropdown_row();
        self.remeasure_row(before);
        if after != before {
            self.remeasure_row(after);
        }
        if after.is_some() {
            self.reveal_row = after;
        }
    }

    fn clear_renders(&mut self) {
        self.render_generation += 1;
        self.renders.clear();
        self.pending_renders = None;
    }
}

fn installed(environment: &PromptEnvironment, provider: PromptProvider) -> bool {
    environment
        .provider(provider)
        .is_some_and(|info| info.path.is_some())
}

/// The prompt Compi shells use, or the login shell's when only that is set.
fn applied_config(environment: &PromptEnvironment) -> Option<&PromptConfig> {
    environment
        .state
        .compi
        .as_ref()
        .or(environment.state.normal.as_ref())
        .map(|applied| &applied.config)
}

fn anything_applied(environment: &PromptEnvironment) -> bool {
    environment.state.compi.is_some() || environment.state.normal.is_some()
}

fn fallback_label(style: &PromptStyle) -> String {
    match style {
        PromptStyle::Default => "Default".into(),
        PromptStyle::Theme { path } => path.rsplit('/').next().unwrap_or(path).to_owned(),
        PromptStyle::Preset { name } => name.clone(),
        PromptStyle::Existing { path } => format!("Existing ({path})"),
    }
}

fn style_rows(environment: &PromptEnvironment, provider: PromptProvider) -> Vec<StyleRow> {
    let applied = applied_config(environment).filter(|config| config.provider == provider);
    let entries = environment
        .provider(provider)
        .map(|info| info.styles.as_slice())
        .unwrap_or_default();
    let mut rows = Vec::with_capacity(entries.len() + 1);
    if let Some(applied) = applied {
        rows.push(StyleRow {
            style: applied.style.clone(),
            label: entries
                .iter()
                .find(|entry| entry.style == applied.style)
                .map_or_else(
                    || fallback_label(&applied.style),
                    |entry| entry.label.clone(),
                ),
            current: true,
        });
    }
    rows.extend(
        entries
            .iter()
            .filter(|entry| applied.is_none_or(|applied| applied.style != entry.style))
            .map(|entry| StyleRow {
                style: entry.style.clone(),
                label: entry.label.clone(),
                current: false,
            }),
    );
    rows
}

/// The applied style, otherwise the first installed provider's first style.
fn initial_selection(environment: &PromptEnvironment) -> Option<PromptConfig> {
    if let Some(config) = applied_config(environment) {
        return Some(config.clone());
    }
    let provider = environment
        .providers
        .iter()
        .find(|info| info.path.is_some())?
        .provider;
    Some(PromptConfig {
        provider,
        style: style_rows(environment, provider)
            .into_iter()
            .next()
            .map_or(PromptStyle::Default, |row| row.style),
    })
}

/// On when the login shell already has a Compi prompt and is still supported.
fn initial_outside(environment: &PromptEnvironment) -> bool {
    environment.state.normal.is_some() && environment.login_shell.shell.is_some()
}

/// Whether applying `config` with this scope would change nothing.
fn already_applied(environment: &PromptEnvironment, config: &PromptConfig, outside: bool) -> bool {
    let state = &environment.state;
    state
        .compi
        .as_ref()
        .is_some_and(|applied| applied.config == *config)
        && match &state.normal {
            Some(applied) => outside && applied.config == *config,
            None => !outside,
        }
}

fn unsupported_login_shell(environment: &PromptEnvironment) -> String {
    format!(
        "Your login shell {} is not bash or zsh, so Compi cannot manage its prompt.",
        environment.login_shell.path
    )
}

fn provider_site(provider: PromptProvider) -> &'static str {
    match provider {
        PromptProvider::OhMyPosh => "ohmyposh.dev",
        PromptProvider::Starship => "starship.rs",
    }
}

fn backup_age(created_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let minutes = u64::try_from(now)
        .unwrap_or(u64::MAX)
        .saturating_sub(created_ms)
        / 60_000;
    match minutes {
        0 => "Just now".into(),
        1..=59 => format!("{minutes} min ago"),
        60..=1_439 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1_440),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AnsiColor {
    Indexed(u8),
    Rgb(u32),
    /// The theme's default foreground, used by inverse video.
    Foreground,
    /// The theme's default background, used by inverse video.
    Background,
}

impl AnsiColor {
    fn resolve(self, palette: &TerminalPalette) -> u32 {
        let value = match self {
            Self::Indexed(index) => palette.indexed(index),
            Self::Rgb(value) => value,
            Self::Foreground => palette.foreground,
            Self::Background => palette.background,
        };
        value & 0x00ff_ffff
    }
}

/// Text sharing one SGR style. `None` colors are the terminal defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct AnsiSpan {
    pub text: String,
    pub fg: Option<AnsiColor>,
    pub bg: Option<AnsiColor>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl AnsiSpan {
    fn same_style(&self, other: &Self) -> bool {
        self.fg == other.fg
            && self.bg == other.bg
            && self.bold == other.bold
            && self.italic == other.italic
            && self.underline == other.underline
    }
}

#[derive(Clone, Copy, Default)]
struct SgrState {
    fg: Option<AnsiColor>,
    bg: Option<AnsiColor>,
    bold: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl SgrState {
    fn span(&self) -> AnsiSpan {
        let (fg, bg) = if self.inverse {
            (
                Some(self.bg.unwrap_or(AnsiColor::Background)),
                Some(self.fg.unwrap_or(AnsiColor::Foreground)),
            )
        } else {
            (self.fg, self.bg)
        };
        AnsiSpan {
            text: String::new(),
            fg,
            bg,
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
        }
    }

    fn apply(&mut self, params: &str) {
        let values: Vec<u32> = params
            .split([';', ':'])
            .map(|value| value.parse().unwrap_or(0))
            .collect();
        let mut index = 0;
        while index < values.len() {
            match values[index] {
                0 => *self = Self::default(),
                1 => self.bold = true,
                3 => self.italic = true,
                4 => self.underline = true,
                7 => self.inverse = true,
                22 => self.bold = false,
                23 => self.italic = false,
                24 => self.underline = false,
                27 => self.inverse = false,
                code @ 30..=37 => self.fg = Some(AnsiColor::Indexed((code - 30) as u8)),
                39 => self.fg = None,
                code @ 40..=47 => self.bg = Some(AnsiColor::Indexed((code - 40) as u8)),
                49 => self.bg = None,
                code @ 90..=97 => self.fg = Some(AnsiColor::Indexed((code - 90 + 8) as u8)),
                code @ 100..=107 => self.bg = Some(AnsiColor::Indexed((code - 100 + 8) as u8)),
                code @ (38 | 48) => {
                    let (color, used) = extended_color(&values[index + 1..]);
                    if let Some(color) = color {
                        if code == 38 {
                            self.fg = Some(color);
                        } else {
                            self.bg = Some(color);
                        }
                    }
                    index += used;
                }
                _ => {}
            }
            index += 1;
        }
    }
}

/// Parses the parameters after 38/48; returns the color and parameters consumed.
fn extended_color(values: &[u32]) -> (Option<AnsiColor>, usize) {
    match values {
        [5, index, ..] => (u8::try_from(*index).ok().map(AnsiColor::Indexed), 2),
        [2, red, green, blue, ..] => (
            match (
                u8::try_from(*red),
                u8::try_from(*green),
                u8::try_from(*blue),
            ) {
                (Ok(red), Ok(green), Ok(blue)) => Some(AnsiColor::Rgb(
                    u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue),
                )),
                _ => None,
            },
            4,
        ),
        _ => (None, values.len()),
    }
}

/// Skips an OSC, DCS, SOS, PM or APC string up to BEL or ST.
fn skip_control_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(character) = chars.next() {
        match character {
            '\x07' | '\u{9c}' => return,
            '\x1b' => {
                if chars.peek() == Some(&'\\') {
                    chars.next();
                }
                return;
            }
            _ => {}
        }
    }
}

/// Splits raw provider output into styled text. Only SGR is interpreted; other
/// CSI, OSC and escape sequences and control characters are dropped, and one
/// leading newline is removed.
pub(super) fn parse_ansi(input: &str) -> Vec<AnsiSpan> {
    let mut spans: Vec<AnsiSpan> = Vec::new();
    let mut state = SgrState::default();
    let mut leading = true;
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        let character = match character {
            '\x1b' => {
                match chars.next() {
                    Some('[') => {
                        let mut params = String::new();
                        let mut last = None;
                        for next in chars.by_ref() {
                            if ('\x40'..='\x7e').contains(&next) {
                                last = Some(next);
                                break;
                            }
                            params.push(next);
                        }
                        if last == Some('m')
                            && params
                                .chars()
                                .all(|value| value.is_ascii_digit() || matches!(value, ';' | ':'))
                        {
                            state.apply(&params);
                        }
                    }
                    Some(']' | 'P' | 'X' | '^' | '_') => skip_control_string(&mut chars),
                    Some(next) if ('\x20'..='\x2f').contains(&next) => {
                        while chars
                            .next()
                            .is_some_and(|next| ('\x20'..='\x2f').contains(&next))
                        {
                        }
                    }
                    _ => {}
                }
                continue;
            }
            '\n' if leading => {
                leading = false;
                continue;
            }
            '\t' => ' ',
            '\n' => '\n',
            character if character.is_control() => continue,
            character => character,
        };
        leading = false;
        let style = state.span();
        match spans.last_mut() {
            Some(last) if last.same_style(&style) => last.text.push(character),
            _ => {
                let mut span = style;
                span.text.push(character);
                spans.push(span);
            }
        }
    }
    spans
}

/// Splits spans into lines; always returns at least one line.
fn ansi_lines(spans: Vec<AnsiSpan>) -> Vec<Vec<AnsiSpan>> {
    let mut lines = vec![Vec::new()];
    for span in spans {
        let mut parts = span.text.split('\n');
        if let Some(first) = parts.next()
            && !first.is_empty()
        {
            lines
                .last_mut()
                .expect("lines start non-empty")
                .push(AnsiSpan {
                    text: first.to_owned(),
                    ..span.clone()
                });
        }
        for part in parts {
            lines.push(Vec::new());
            if !part.is_empty() {
                lines
                    .last_mut()
                    .expect("line was just pushed")
                    .push(AnsiSpan {
                        text: part.to_owned(),
                        ..span.clone()
                    });
            }
        }
    }
    lines
}

fn ansi_text(spans: &[AnsiSpan], palette: &TerminalPalette) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let style = HighlightStyle {
            color: span.fg.map(|value| color(value.resolve(palette))),
            background_color: span.bg.map(|value| color(value.resolve(palette))),
            font_weight: span.bold.then_some(FontWeight::BOLD),
            font_style: span.italic.then_some(FontStyle::Italic),
            underline: span.underline.then_some(UnderlineStyle {
                thickness: px(1.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        if style != HighlightStyle::default() {
            highlights.push((start..text.len(), style));
        }
    }
    if text.is_empty() {
        text.push(' ');
    }
    StyledText::new(text).with_highlights(highlights)
}

impl CompiApp {
    /// Detects the prompt environment the first time Settings → Terminal opens
    /// for a distribution; Refresh detects again.
    pub(super) fn enter_prompt_settings(&mut self) {
        let (distribution, cwd) = self.focused_prompt_location();
        let state = &mut self.prompt_settings;
        if state.environment.is_none() && !state.loading && state.distribution.is_none() {
            state.distribution = distribution.clone();
        }
        state.cwd = cwd.map(|path| (distribution, path));
        if state.environment.is_none() && !state.loading {
            self.request_prompt_detect();
        }
    }

    /// Focuses the first control of the group and scrolls it into view.
    pub(super) fn focus_prompt_group(&mut self) {
        self.overlay_focus = self.prompt_focus(0);
        self.settings_scroll_to_focus = true;
    }

    fn focused_prompt_location(&self) -> (Option<String>, Option<String>) {
        let Some(view) = self.focused_view() else {
            return (None, None);
        };
        let surface = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.surface(&view.surface_id));
        let cwd = view
            .mirror
            .snapshot()
            .and_then(|snapshot| snapshot.current_directory.clone())
            .or_else(|| {
                surface
                    .and_then(|surface| surface.working_directory.as_ref())
                    .map(|cwd| cwd.resolved_wsl_path.clone())
            })
            .filter(|path| path.starts_with('/'));
        #[cfg(windows)]
        let distribution = surface
            .and_then(|surface| surface.working_directory.as_ref())
            .map(|cwd| cwd.distribution.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                surface
                    .and_then(|surface| surface.launch.profile.as_ref())
                    .and_then(|profile| profile.distribution.clone())
            })
            .filter(|_| !self.target.is_remote());
        #[cfg(not(windows))]
        let distribution = None;
        (distribution, cwd)
    }

    /// WSL2 distribution for requests; always `None` outside local Windows.
    fn prompt_request_distribution(&self) -> Option<String> {
        if !cfg!(windows) || self.target.is_remote() {
            return None;
        }
        let state = &self.prompt_settings;
        state.distribution.clone().or_else(|| {
            state
                .environment()
                .and_then(|environment| environment.distribution.clone())
        })
    }

    fn prompt_preview_width(&self) -> u16 {
        u16::try_from(self.terminal_cols.clamp(40, 160)).unwrap_or(80)
    }

    /// The focused pane's directory when it lives where previews render.
    fn prompt_preview_cwd(&self) -> Option<String> {
        let any_namespace = !cfg!(windows) || self.target.is_remote();
        let state = &self.prompt_settings;
        let environment = state.environment()?;
        state
            .cwd
            .as_ref()
            .filter(|(distribution, _)| any_namespace || *distribution == environment.distribution)
            .map(|(_, path)| path.clone())
    }

    fn send_prompt_request(
        &self,
        job: PromptJob,
        request: u64,
        distribution: Option<String>,
        prompt: PromptRequest,
    ) {
        let target = self.target.clone();
        let sender = self.event_tx.clone();
        thread::spawn(move || {
            let result =
                (|| -> crate::Result<_> { target.connect()?.prompt(distribution, prompt) })()
                    .map_err(|error| error.to_string());
            sender.send(UiEvent::PromptResponded {
                job,
                request,
                result,
            });
        });
    }

    fn request_prompt_detect(&mut self) {
        let request = NEXT_PROMPT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let distribution = self.prompt_request_distribution();
        let state = &mut self.prompt_settings;
        state.detect_request = request;
        state.loading = true;
        self.send_prompt_request(
            PromptJob::Detect,
            request,
            distribution,
            PromptRequest::Detect,
        );
    }

    /// Requests the next batch of renders for the selected provider; one batch
    /// is in flight at a time.
    fn fetch_prompt_renders(&mut self) {
        if self.prompt_settings.pending_renders.is_some() {
            return;
        }
        let distribution = self.prompt_request_distribution();
        let width = self.prompt_preview_width();
        let cwd = self.prompt_preview_cwd();
        let state = &mut self.prompt_settings;
        let Some(environment) = state.environment() else {
            return;
        };
        let Some(provider) = state.selected.as_ref().map(|config| config.provider) else {
            return;
        };
        if !installed(environment, provider) {
            return;
        }
        let styles: Vec<PromptStyle> = style_rows(environment, provider)
            .into_iter()
            .map(|row| row.style)
            .filter(|style| {
                !state
                    .renders
                    .contains_key(&(distribution.clone(), provider, style.clone()))
            })
            .take(MAX_PREVIEW_STYLES)
            .collect();
        if styles.is_empty() {
            return;
        }
        let request = NEXT_PROMPT_REQUEST.fetch_add(1, Ordering::Relaxed);
        state.pending_renders = Some(PendingRenders {
            request,
            generation: state.render_generation,
            distribution: distribution.clone(),
            provider,
            styles: styles.clone(),
        });
        self.send_prompt_request(
            PromptJob::Renders,
            request,
            distribution,
            PromptRequest::Preview {
                provider,
                styles,
                cwd,
                width,
            },
        );
    }

    /// Opens the dropdown for `operation`, replacing any other, and plans it in
    /// the background.
    fn open_prompt_dropdown(&mut self, operation: PromptOperation) {
        let request = NEXT_PROMPT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let distribution = self.prompt_request_distribution();
        let state = &mut self.prompt_settings;
        state.action_request = request;
        let dropdown = PromptDropdown {
            operation: operation.clone(),
            plan: None,
            error: None,
        };
        state.update_dropdown(|state| state.dropdown = Some(dropdown));
        self.send_prompt_request(
            PromptJob::Plan,
            request,
            distribution,
            PromptRequest::Plan { operation },
        );
    }

    /// Opens the dropdown for `operation` with its confirm focused, or closes it
    /// when it is already open there.
    fn toggle_prompt_dropdown(&mut self, operation: PromptOperation) {
        if self
            .prompt_settings
            .dropdown
            .as_ref()
            .is_some_and(|dropdown| dropdown.operation == operation)
        {
            self.close_prompt_dropdown();
            return;
        }
        self.open_prompt_dropdown(operation);
        self.focus_prompt_action(PromptAction::Confirm);
    }

    /// The dropdown under the selected style; none when applying it would change nothing.
    fn toggle_style_dropdown(&mut self) {
        let state = &self.prompt_settings;
        let (Some(environment), Some(config)) = (state.environment(), state.selected.clone())
        else {
            return;
        };
        let outside = state.outside;
        if already_applied(environment, &config, outside) {
            self.close_prompt_dropdown();
            return;
        }
        self.toggle_prompt_dropdown(PromptOperation::Apply { config, outside });
    }

    /// Flips the outside toggle; an open Apply dropdown plans the new scope.
    fn toggle_prompt_outside(&mut self) {
        let state = &mut self.prompt_settings;
        state.outside = !state.outside;
        let outside = state.outside;
        let Some(PromptOperation::Apply { config, .. }) = state
            .dropdown
            .as_ref()
            .map(|dropdown| dropdown.operation.clone())
        else {
            return;
        };
        if state
            .environment()
            .is_some_and(|environment| already_applied(environment, &config, outside))
        {
            self.close_prompt_dropdown();
        } else {
            self.open_prompt_dropdown(PromptOperation::Apply { config, outside });
        }
    }

    fn execute_prompt_plan(&mut self) {
        let distribution = self.prompt_request_distribution();
        let state = &mut self.prompt_settings;
        let Some(PromptDropdown {
            operation,
            plan: Some(plan),
            ..
        }) = &state.dropdown
        else {
            return;
        };
        let prompt = PromptRequest::Execute {
            operation: operation.clone(),
            token: plan.token.clone(),
        };
        let request = NEXT_PROMPT_REQUEST.fetch_add(1, Ordering::Relaxed);
        state.action_request = request;
        state.update_dropdown(|state| {
            state.executing = true;
            if let Some(dropdown) = state.dropdown.as_mut() {
                dropdown.error = None;
            }
        });
        self.send_prompt_request(PromptJob::Execute, request, distribution, prompt);
    }

    /// Closes the dropdown; nothing is written. False when none is open or its
    /// change is being applied.
    pub(super) fn close_prompt_dropdown(&mut self) -> bool {
        let focused = self.focused_prompt_action();
        let state = &mut self.prompt_settings;
        if state.executing || state.dropdown.is_none() {
            return false;
        }
        let anchor = state.dropdown_anchor();
        state.action_request = 0;
        state.update_dropdown(|state| state.dropdown = None);
        // Its buttons leave the focus order: keep focus on the same control.
        match focused {
            Some(PromptAction::Confirm | PromptAction::Cancel) => {
                if let Some(anchor) = anchor {
                    self.focus_prompt_action(anchor);
                }
            }
            Some(action) => {
                if let Some(index) = self
                    .prompt_actions()
                    .iter()
                    .position(|candidate| *candidate == action)
                {
                    self.overlay_focus = self.prompt_focus(index);
                }
            }
            None => {}
        }
        true
    }

    fn prompt_focus(&self, index: usize) -> usize {
        self.settings_content_focus(self.prompt_offset_base() + index)
    }

    fn prompt_group_visible(&self) -> bool {
        matches!(self.overlay, Some(Overlay::Settings))
            && self.settings_section == SettingsSection::Terminal
    }

    fn focused_prompt_action(&self) -> Option<PromptAction> {
        if !self.prompt_group_visible() {
            return None;
        }
        let offset = self.overlay_focus.checked_sub(self.prompt_focus(0))?;
        self.prompt_actions().get(offset).copied()
    }

    fn focus_prompt_action(&mut self, action: PromptAction) -> bool {
        if !self.prompt_group_visible() {
            return false;
        }
        let Some(index) = self
            .prompt_actions()
            .iter()
            .position(|candidate| *candidate == action)
        else {
            return false;
        };
        self.overlay_focus = self.prompt_focus(index);
        self.settings_scroll_to_focus = true;
        true
    }

    fn adopt_prompt_environment(&mut self, environment: PromptEnvironment, keep_selection: bool) {
        let state = &mut self.prompt_settings;
        let reselect = !keep_selection || state.selected.is_none();
        if reselect {
            state.selected = initial_selection(&environment);
            state.outside = initial_outside(&environment);
        }
        if environment.login_shell.shell.is_none() {
            state.outside = false;
        }
        if !environment.distributions.is_empty() {
            state.distributions = environment.distributions.clone();
        }
        state.environment = Some(Ok(environment));
        state.sync_rows();
        if reselect {
            state.style_list.reset(state.rows.len());
        }
        if state.dropdown_anchor().is_none() {
            state.dropdown = None;
            state.action_request = 0;
        }
        self.fetch_prompt_renders();
    }

    pub(super) fn prompt_responded(
        &mut self,
        job: PromptJob,
        request: u64,
        result: Result<PromptResponse, String>,
    ) {
        let state = &mut self.prompt_settings;
        match job {
            PromptJob::Detect => {
                if request != state.detect_request {
                    return;
                }
                state.loading = false;
                match result {
                    Ok(PromptResponse::Detected { environment }) => {
                        let keep_selection = state.environment().is_some_and(|previous| {
                            previous.distribution == environment.distribution
                        });
                        self.adopt_prompt_environment(environment, keep_selection);
                    }
                    other => {
                        state.environment =
                            Some(Err(other.err().unwrap_or(UNEXPECTED_RESPONSE.into())));
                        state.selected = None;
                        state.dropdown = None;
                        state.sync_rows();
                    }
                }
            }
            PromptJob::Renders => {
                let Some(pending) = state
                    .pending_renders
                    .take_if(|pending| pending.request == request)
                else {
                    return;
                };
                if pending.generation == state.render_generation {
                    let mut renders = match result {
                        Ok(PromptResponse::Previewed { preview }) => {
                            preview.renders.iter().map(StyleRender::new).collect()
                        }
                        other => {
                            let error = other.err().unwrap_or(UNEXPECTED_RESPONSE.into());
                            pending
                                .styles
                                .iter()
                                .map(|_| StyleRender::failed(error.clone()))
                                .collect::<Vec<_>>()
                        }
                    }
                    .into_iter();
                    for style in pending.styles {
                        let render = renders.next().unwrap_or_else(|| {
                            StyleRender::failed(
                                "The daemon returned no preview for this style.".into(),
                            )
                        });
                        state.renders.insert(
                            (pending.distribution.clone(), pending.provider, style),
                            render,
                        );
                    }
                }
                self.fetch_prompt_renders();
            }
            PromptJob::Plan => {
                if request != state.action_request || state.dropdown.is_none() {
                    return;
                }
                match result {
                    // Nothing would change, so there is nothing to confirm.
                    Ok(PromptResponse::Planned { plan }) if plan.changes.is_empty() => {
                        self.close_prompt_dropdown();
                    }
                    Ok(PromptResponse::Planned { plan }) => state.update_dropdown(|state| {
                        if let Some(dropdown) = state.dropdown.as_mut() {
                            dropdown.plan = Some(plan);
                        }
                    }),
                    other => {
                        let error = other.err().unwrap_or(UNEXPECTED_RESPONSE.into());
                        state.update_dropdown(|state| {
                            if let Some(dropdown) = state.dropdown.as_mut() {
                                dropdown.error = Some(error);
                            }
                        });
                    }
                }
            }
            PromptJob::Execute => {
                if request != state.action_request {
                    return;
                }
                match result {
                    Ok(PromptResponse::Executed { environment }) => {
                        state.executing = false;
                        state.dropdown = None;
                        state.action_request = 0;
                        self.adopt_prompt_environment(environment, false);
                        if !self.focus_prompt_action(PromptAction::Styles) {
                            self.focus_prompt_action(PromptAction::Refresh);
                        }
                    }
                    other => {
                        let error = other.err().unwrap_or(UNEXPECTED_RESPONSE.into());
                        state.update_dropdown(|state| {
                            state.executing = false;
                            if let Some(dropdown) = state.dropdown.as_mut() {
                                dropdown.error = Some(error);
                            }
                        });
                    }
                }
            }
        }
    }

    /// Focusable controls in render order; see `render_prompt_group`. An open
    /// dropdown's confirm and Cancel follow the control it opened under.
    pub(super) fn prompt_actions(&self) -> Vec<PromptAction> {
        let state = &self.prompt_settings;
        let environment = state.environment();
        let mut actions = Vec::new();
        if environment.is_some() {
            actions.push(PromptAction::Details);
        }
        actions.push(PromptAction::Refresh);
        if state.distributions.len() > 1 {
            actions.extend((0..state.distributions.len()).map(PromptAction::Distribution));
        }
        let Some(environment) = environment else {
            return actions;
        };
        if PromptProvider::ALL
            .iter()
            .all(|provider| installed(environment, *provider))
        {
            actions.extend(PromptProvider::ALL.map(PromptAction::Provider));
        }
        if !state.rows.is_empty() {
            actions.push(PromptAction::Styles);
        }
        actions.push(PromptAction::Outside);
        if anything_applied(environment) {
            actions.push(PromptAction::TurnOff);
        }
        actions.push(PromptAction::History);
        if state.history {
            actions.extend((0..environment.backups.len()).map(PromptAction::Restore));
        }
        if let Some(anchor) = state.dropdown_anchor()
            && let Some(index) = actions.iter().position(|action| *action == anchor)
        {
            actions.splice(
                index + 1..index + 1,
                [PromptAction::Confirm, PromptAction::Cancel],
            );
        }
        actions
    }

    fn prompt_action_reason(&self, action: PromptAction) -> Option<String> {
        let state = &self.prompt_settings;
        if matches!(action, PromptAction::Details | PromptAction::History) {
            return None;
        }
        if state.executing {
            return Some("Wait for the prompt change to finish.".into());
        }
        let environment = state.environment()?;
        match action {
            PromptAction::Outside if environment.login_shell.shell.is_none() => {
                Some(unsupported_login_shell(environment))
            }
            _ => None,
        }
    }

    pub(super) fn activate_prompt_action(&mut self, action: PromptAction) {
        if let Some(reason) = self.prompt_action_reason(action) {
            self.prompt_settings.error = Some(reason);
            return;
        }
        let state = &mut self.prompt_settings;
        state.error = None;
        match action {
            PromptAction::Details => state.details = !state.details,
            PromptAction::History => {
                if matches!(
                    state.dropdown.as_ref().map(|dropdown| &dropdown.operation),
                    Some(PromptOperation::Restore { .. })
                ) {
                    self.close_prompt_dropdown();
                }
                let state = &mut self.prompt_settings;
                state.history = !state.history;
            }
            PromptAction::Refresh => {
                self.close_prompt_dropdown();
                self.prompt_settings.clear_renders();
                self.request_prompt_detect();
            }
            PromptAction::Distribution(index) => self.select_prompt_distribution(index),
            PromptAction::Provider(provider) => self.select_prompt_provider(provider),
            PromptAction::Styles => self.toggle_style_dropdown(),
            PromptAction::Outside => self.toggle_prompt_outside(),
            PromptAction::TurnOff => self.toggle_prompt_dropdown(PromptOperation::Disable),
            PromptAction::Restore(index) => {
                let backup = state
                    .environment()
                    .and_then(|environment| environment.backups.get(index))
                    .map(|backup| backup.id.clone());
                if let Some(backup) = backup {
                    self.toggle_prompt_dropdown(PromptOperation::Restore { backup });
                }
            }
            PromptAction::Cancel => {
                self.close_prompt_dropdown();
            }
            PromptAction::Confirm => self.execute_prompt_plan(),
        }
        if !matches!(
            action,
            PromptAction::Styles
                | PromptAction::TurnOff
                | PromptAction::Restore(_)
                | PromptAction::Cancel
                | PromptAction::Confirm
        ) {
            self.focus_prompt_action(action);
        }
    }

    fn select_prompt_distribution(&mut self, index: usize) {
        let active = self.prompt_request_distribution();
        let state = &mut self.prompt_settings;
        let Some(name) = state.distributions.get(index).cloned() else {
            return;
        };
        if active.as_ref() == Some(&name) {
            return;
        }
        state.distribution = Some(name);
        state.environment = None;
        state.selected = None;
        state.dropdown = None;
        state.action_request = 0;
        state.sync_rows();
        self.request_prompt_detect();
    }

    fn select_prompt_provider(&mut self, provider: PromptProvider) {
        let state = &mut self.prompt_settings;
        let Some(environment) = state.environment() else {
            return;
        };
        if state
            .selected
            .as_ref()
            .is_some_and(|selected| selected.provider == provider)
        {
            return;
        }
        let style = style_rows(environment, provider)
            .into_iter()
            .next()
            .map_or(PromptStyle::Default, |row| row.style);
        state.selected = Some(PromptConfig { provider, style });
        state.dropdown = None;
        state.action_request = 0;
        state.sync_rows();
        state.style_list.reset(state.rows.len());
        self.fetch_prompt_renders();
    }

    fn select_prompt_style(&mut self, index: usize) {
        let state = &mut self.prompt_settings;
        let Some(style) = state.rows.get(index).map(|row| row.style.clone()) else {
            return;
        };
        let Some(selected) = state.selected.as_mut() else {
            return;
        };
        selected.style = style;
        state.style_list.scroll_to_reveal_item(index);
    }

    /// Moves the style selection for an arrow, Home or End key; a dropdown
    /// under the previous row closes.
    pub(super) fn move_prompt_style(&mut self, key: &str) {
        let state = &self.prompt_settings;
        if state.rows.is_empty() || state.executing {
            return;
        }
        let last = state.rows.len() - 1;
        let current = state.selected_row().unwrap_or(0);
        let next = match key {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(last),
            "home" => 0,
            _ => last,
        };
        self.prompt_settings.error = None;
        if next != current {
            self.close_prompt_dropdown();
            self.select_prompt_style(next);
        }
    }

    fn prompt_note(&self, text: impl Into<SharedString>) -> AnyElement {
        let colors = *self.colors();
        let text: SharedString = text.into();
        div()
            .min_w_0()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .text_color(color(modal_text_color(colors.muted, &colors)))
            .child(text)
            .into_any_element()
    }

    fn prompt_error(&self, text: impl Into<SharedString>) -> AnyElement {
        let colors = *self.colors();
        let text: SharedString = text.into();
        div()
            .min_w_0()
            .text_color(color(modal_text_color(colors.error, &colors)))
            .child(text)
            .into_any_element()
    }

    fn prompt_index(&self, actions: &[PromptAction], action: PromptAction) -> Option<usize> {
        actions.iter().position(|candidate| *candidate == action)
    }

    fn prompt_focused(&self, index: Option<usize>) -> bool {
        index.is_some_and(|index| self.overlay_focus == self.prompt_focus(index))
    }

    fn prompt_listener(
        &self,
        index: Option<usize>,
        action: PromptAction,
        cx: &Context<Self>,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _, _, cx| {
            if let Some(index) = index {
                this.overlay_focus = this.prompt_focus(index);
            }
            this.activate_prompt_action(action);
            cx.stop_propagation();
            cx.notify();
        })
    }

    fn prompt_segment(
        &self,
        actions: &[PromptAction],
        action: PromptAction,
        label: impl Into<SharedString>,
        active: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let index = self.prompt_index(actions, action);
        let focused = self.prompt_focused(index);
        div()
            .relative()
            .flex_none()
            .child(self.settings_segment_button(
                ("prompt-control", index.unwrap_or(usize::MAX)),
                label,
                active,
                focused,
                self.prompt_listener(index, action, cx),
            ))
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    fn prompt_button(
        &self,
        actions: &[PromptAction],
        action: PromptAction,
        label: impl Into<SharedString>,
        primary: bool,
        destructive: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let index = self.prompt_index(actions, action);
        let focused = self.prompt_focused(index);
        let id = ("prompt-control", index.unwrap_or(usize::MAX));
        let listener = self.prompt_listener(index, action, cx);
        div()
            .relative()
            .flex_none()
            .child(if primary && !destructive {
                self.settings_primary_button(id, label, focused, listener)
            } else {
                self.settings_action_button(id, label, focused, destructive, listener)
            })
            .child(self.settings_focus_anchor(focused, cx))
            .into_any_element()
    }

    /// The "Shell prompt" group at the bottom of Settings → Terminal. Focus
    /// offsets follow `prompt_actions`.
    pub(super) fn render_prompt_group(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let actions = self.prompt_actions();
        let state = &self.prompt_settings;
        let environment = state.environment();
        let mut header = div().min_w_0().flex().flex_wrap().items_center().gap_2();
        if environment.is_some() {
            header = header.child(self.prompt_button(
                &actions,
                PromptAction::Details,
                if state.details {
                    "Hide details"
                } else {
                    "Details"
                },
                false,
                false,
                cx,
            ));
        }
        header = header.child(self.prompt_button(
            &actions,
            PromptAction::Refresh,
            if matches!(state.environment, Some(Err(_))) {
                "Retry"
            } else {
                "Refresh"
            },
            false,
            false,
            cx,
        ));
        if state.loading {
            header = header.child(self.prompt_note(if environment.is_some() {
                "Refreshing…"
            } else {
                "Detecting…"
            }));
        }
        let mut group = div()
            .min_w_0()
            .pt_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.settings_subheading("Shell prompt"))
            .child(header);
        if let Some(Err(error)) = &state.environment {
            group =
                group.child(self.prompt_error(format!("Cannot inspect the shell prompt: {error}")));
        }
        if let Some(environment) = environment
            && state.details
        {
            group = group.child(self.render_prompt_details(environment));
        }
        if state.distributions.len() > 1 {
            group = group.child(self.render_prompt_distributions(&actions, compact, cx));
        }
        if let Some(environment) = environment {
            group = group.child(self.render_prompt_form(&actions, environment, compact, cx));
        } else if let Some(error) = &state.error {
            group = group.child(self.prompt_error(error.clone()));
        }
        group.into_any_element()
    }

    fn render_prompt_distributions(
        &self,
        actions: &[PromptAction],
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let active = self.prompt_request_distribution();
        let controls =
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(self.prompt_settings.distributions.iter().enumerate().map(
                    |(index, name)| {
                        self.prompt_segment(
                            actions,
                            PromptAction::Distribution(index),
                            name.clone(),
                            active.as_ref() == Some(name),
                            cx,
                        )
                    },
                ))
                .into_any_element();
        self.settings_row("WSL distribution", String::new(), controls, compact)
    }

    fn render_prompt_details(&self, environment: &PromptEnvironment) -> AnyElement {
        let colors = *self.colors();
        let shell = |role: &str, info: &compi_protocol::prompt::PromptShellInfo| match info.shell {
            Some(_) => format!("{role}: {}", info.path),
            None => format!(
                "{role}: {} · not supported; Compi manages bash and zsh prompts",
                info.path
            ),
        };
        let mut details = div()
            .min_w_0()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_sm()
            .bg(color(blend_rgb(colors.surface, colors.foreground, 0.03)))
            .child(self.prompt_note(shell("Compi shell", &environment.compi_shell)))
            .child(self.prompt_note(shell(
                "Login shell (outside Compi)",
                &environment.login_shell,
            )));
        for info in &environment.providers {
            details = details.child(self.prompt_note(match &info.path {
                Some(path) => format!(
                    "{}: {}{path}",
                    info.provider.label(),
                    info.version
                        .as_deref()
                        .map(|version| format!("{version} · "))
                        .unwrap_or_default()
                ),
                None => format!(
                    "{}: not installed — Compi does not install prompt providers; install it yourself from {}, then Refresh.",
                    info.provider.label(),
                    provider_site(info.provider)
                ),
            }));
        }
        if !environment.startup_lines.is_empty() {
            details = details.child(self.prompt_note("Startup files:"));
            for line in &environment.startup_lines {
                let tag = if line.managed {
                    Some("Compi block")
                } else {
                    line.provider.map(PromptProvider::label)
                };
                details = details.child(
                    div()
                        .min_w_0()
                        .flex()
                        .gap_2()
                        .text_size(px(UI_SMALL_TEXT_SIZE))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font(self.typography.font.clone())
                                .child(format!(
                                    "{}:{}  {}",
                                    line.path,
                                    line.line,
                                    line.text.trim()
                                )),
                        )
                        .when_some(tag, |row, tag| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_color(color(modal_text_color(colors.muted, &colors)))
                                    .child(tag),
                            )
                        }),
                );
            }
        }
        if let Ok(launch) = &self.config.launch
            && (launch.profile.executable.is_some() || !launch.profile.args.is_empty())
        {
            details = details.child(self.prompt_note(
                "Your default profile sets a custom shell command, so Compi-only prompts do not load there.",
            ));
        }
        for note in &environment.notes {
            details = details.child(self.prompt_note(note.clone()));
        }
        details.into_any_element()
    }

    fn render_prompt_form(
        &self,
        actions: &[PromptAction],
        environment: &PromptEnvironment,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let state = &self.prompt_settings;
        let mut form = div().min_w_0().flex().flex_col().gap_2();
        if PromptProvider::ALL
            .iter()
            .all(|provider| installed(environment, *provider))
        {
            let providers = div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(PromptProvider::ALL.map(|provider| {
                    self.prompt_segment(
                        actions,
                        PromptAction::Provider(provider),
                        provider.label(),
                        state
                            .selected
                            .as_ref()
                            .is_some_and(|selected| selected.provider == provider),
                        cx,
                    )
                }))
                .into_any_element();
            form = form.child(self.settings_row("Provider", String::new(), providers, compact));
        }
        form = form.child(match &state.selected {
            Some(selected) if !installed(environment, selected.provider) => self.prompt_note(
                format!("{} is not installed here.", selected.provider.label()),
            ),
            Some(_) if !state.rows.is_empty() => self.render_prompt_styles(actions, cx),
            Some(_) => self.prompt_note("No styles found."),
            None => self.prompt_note("Install Oh My Posh or Starship here, then Refresh."),
        });
        let outside_index = self.prompt_index(actions, PromptAction::Outside);
        form = form.child(
            self.settings_row(
                "Also use outside Compi",
                String::new(),
                div()
                    .when(environment.login_shell.shell.is_none(), |toggle| {
                        toggle.opacity(0.5)
                    })
                    .child(self.prompt_toggle(
                        state.outside,
                        PromptAction::Outside,
                        // Never index 0: a missing index must not claim another control's focus.
                        outside_index.unwrap_or(actions.len()),
                        cx,
                    ))
                    .into_any_element(),
                compact,
            ),
        );
        let mut buttons = div().flex().flex_wrap().gap_2();
        if anything_applied(environment) {
            buttons = buttons.child(self.prompt_button(
                actions,
                PromptAction::TurnOff,
                "Turn off",
                false,
                true,
                cx,
            ));
        }
        buttons = buttons.child(self.prompt_button(
            actions,
            PromptAction::History,
            if state.history {
                "Hide history"
            } else {
                "History"
            },
            false,
            false,
            cx,
        ));
        form = form.child(buttons);
        if state.dropdown_anchor() == Some(PromptAction::TurnOff)
            && let Some(dropdown) = &state.dropdown
        {
            form = form.child(self.render_prompt_dropdown(actions, dropdown, cx));
        }
        if let Some(error) = &state.error {
            form = form.child(self.prompt_error(error.clone()));
        }
        if state.history {
            form = form.child(self.render_prompt_history(actions, environment, cx));
        }
        form.into_any_element()
    }

    /// Only rows in view are laid out; see `PromptSettingsState::style_list`.
    fn render_prompt_styles(&self, actions: &[PromptAction], cx: &Context<Self>) -> AnyElement {
        let colors = *self.colors();
        let index = self.prompt_index(actions, PromptAction::Styles);
        let focused = self.prompt_focused(index);
        div()
            .relative()
            .min_w_0()
            .h(px(360.0))
            .overflow_hidden()
            .rounded_sm()
            .border_1()
            .border_color(color(if focused {
                colors.accent
            } else {
                colors.border
            }))
            // The list scrolls itself; keep the settings panel still.
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                list(
                    self.prompt_settings.style_list.clone(),
                    cx.processor(move |this, row: usize, _, cx| {
                        this.render_prompt_style_row(row, index, cx)
                    }),
                )
                .size_full(),
            )
            .child(self.settings_focus_anchor(focused, cx))
            .child(self.prompt_reveal_anchor(cx))
            .into_any_element()
    }

    /// Painted after the list, so a re-measured dropdown row is revealed with its
    /// real height rather than the stale one `scroll_to_reveal_item` would see
    /// right after the dropdown opens.
    fn prompt_reveal_anchor(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        canvas(
            |_, _, _| (),
            move |_, _, window, cx| {
                entity.update(cx, |this, _| {
                    if let Some(row) = this.prompt_settings.reveal_row.take() {
                        this.prompt_settings.style_list.scroll_to_reveal_item(row);
                        window.refresh();
                    }
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    fn render_prompt_style_row(
        &self,
        row_index: usize,
        focus_index: Option<usize>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let state = &self.prompt_settings;
        let (Some(row), Some(selected)) = (state.rows.get(row_index), state.selected.as_ref())
        else {
            return div().into_any_element();
        };
        let palette = *self.terminal_theme.terminal();
        let key = (
            self.prompt_request_distribution(),
            selected.provider,
            row.style.clone(),
        );
        let render = state.renders.get(&key);
        let active = row.style == selected.style;
        let background = if active {
            blend_rgb(colors.surface, colors.accent, 0.10)
        } else {
            colors.surface
        };
        let success = match render {
            Some(render) if render.error.is_some() => {
                self.prompt_error(render.error.clone().unwrap_or_default())
            }
            Some(render) => self.render_prompt_block(&render.success, &palette),
            None => self.prompt_placeholder(&palette),
        };
        let dropdown = state
            .dropdown
            .as_ref()
            .filter(|_| state.dropdown_row() == Some(row_index));
        div()
            .id(("prompt-style", row_index))
            .w_full()
            .min_w_0()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_b_1()
            .border_color(color(colors.border))
            .bg(color(background))
            .hover(move |style| {
                style
                    .bg(color(if active {
                        background
                    } else {
                        colors.surface_hover
                    }))
                    .cursor_pointer()
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(index) = focus_index {
                    this.overlay_focus = this.prompt_focus(index);
                }
                if this.prompt_action_reason(PromptAction::Styles).is_none() {
                    this.select_prompt_style(row_index);
                }
                this.activate_prompt_action(PromptAction::Styles);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_weight(if active {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .child(row.label.clone()),
                    )
                    .when(row.current, |header| {
                        header.child(
                            div()
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(color(colors.accent))
                                .text_size(px(UI_MICRO_TEXT_SIZE))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Current"),
                        )
                    }),
            )
            .child(success)
            .when_some(dropdown, |row, dropdown| {
                row.child(self.render_prompt_dropdown(&self.prompt_actions(), dropdown, cx))
            })
            .into_any_element()
    }

    fn prompt_block_frame(&self, palette: &TerminalPalette) -> gpui::Div {
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .rounded_sm()
            .bg(material_color(palette.background, 1.0))
            .px_2()
            .py_1()
            .font(self.typography.font.clone())
            .text_size(px(self.typography.font_size))
            .line_height(px(self.typography.cell_height))
            .text_color(color(palette.foreground & 0x00ff_ffff))
            .flex()
            .flex_col()
    }

    fn prompt_placeholder(&self, palette: &TerminalPalette) -> AnyElement {
        self.prompt_block_frame(palette)
            .child(
                div()
                    .opacity(0.5)
                    .text_size(px(UI_SMALL_TEXT_SIZE))
                    .child("Rendering…"),
            )
            .into_any_element()
    }

    fn render_prompt_block(&self, render: &ParsedRender, palette: &TerminalPalette) -> AnyElement {
        let last = render.left.len() - 1;
        self.prompt_block_frame(palette)
            .children(render.left.iter().enumerate().map(|(index, line)| {
                div()
                    .min_w_0()
                    .flex()
                    .justify_between()
                    .gap_2()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .child(ansi_text(line, palette)),
                    )
                    .when(index == last && !render.right.is_empty(), |row| {
                        row.child(div().flex_none().child(ansi_text(&render.right, palette)))
                    })
            }))
            .into_any_element()
    }

    fn render_prompt_history(
        &self,
        actions: &[PromptAction],
        environment: &PromptEnvironment,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        if environment.backups.is_empty() {
            return self.prompt_note("No backups yet.");
        }
        let state = &self.prompt_settings;
        let anchor = state.dropdown_anchor();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .children(
                environment
                    .backups
                    .iter()
                    .enumerate()
                    .map(|(index, backup)| {
                        let dropdown = state
                            .dropdown
                            .as_ref()
                            .filter(|_| anchor == Some(PromptAction::Restore(index)));
                        div()
                            .min_w_0()
                            .py_2()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .border_b_1()
                            .border_color(color(colors.border))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_auto()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child(backup.description.clone()),
                                            )
                                            .child(self.prompt_note(backup_age(backup.created_ms))),
                                    )
                                    .child(self.prompt_button(
                                        actions,
                                        PromptAction::Restore(index),
                                        "Restore",
                                        false,
                                        false,
                                        cx,
                                    )),
                            )
                            .when_some(dropdown, |row, dropdown| {
                                row.child(self.render_prompt_dropdown(actions, dropdown, cx))
                            })
                    }),
            )
            .into_any_element()
    }

    /// A user file the plan changes, with its line diff.
    fn render_prompt_diff(&self, change: &PromptFileChange) -> AnyElement {
        let colors = *self.colors();
        let palette = self.terminal_theme.terminal();
        let background = blend_rgb(colors.surface, colors.foreground, 0.06);
        div()
            .min_w_0()
            .p_2()
            .flex()
            .flex_col()
            .rounded_sm()
            .bg(color(background))
            .font(self.typography.font.clone())
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .child(
                div()
                    .min_w_0()
                    .pb_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(change.path.clone()),
            )
            .children(
                change
                    .diff
                    .iter()
                    .flat_map(|diff| diff.lines())
                    .map(|line| {
                        let tint = match line.as_bytes().first() {
                            Some(b'+') => palette.ansi[2],
                            Some(b'-') => palette.ansi[1],
                            Some(b'@') => palette.ansi[6],
                            _ => colors.foreground,
                        };
                        div()
                            .min_w_0()
                            .text_color(color(ui_text_color(tint, background)))
                            .child(if line.is_empty() {
                                " ".to_owned()
                            } else {
                                line.to_owned()
                            })
                    }),
            )
            .into_any_element()
    }

    /// The confirm under a style row, Turn off, or a history row: the diff of
    /// any user file the plan changes, then the confirm and Cancel buttons. The
    /// confirm waits for the plan.
    fn render_prompt_dropdown(
        &self,
        actions: &[PromptAction],
        dropdown: &PromptDropdown,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let state = &self.prompt_settings;
        let user_files: Vec<&PromptFileChange> = dropdown
            .plan
            .iter()
            .flat_map(|plan| &plan.changes)
            .filter(|change| change.user_file)
            .collect();
        let (label, destructive) = match &dropdown.operation {
            PromptOperation::Apply { .. } => ("Apply", !user_files.is_empty()),
            PromptOperation::Disable => ("Turn off", true),
            PromptOperation::Restore { .. } => ("Restore", !user_files.is_empty()),
        };
        let ready = dropdown.plan.is_some() && !state.executing;
        let mut panel = div()
            .min_w_0()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_sm()
            .border_1()
            .border_color(color(colors.border))
            .bg(color(colors.surface))
            // Clicks inside must not reach the style row under it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for change in user_files {
            panel = panel.child(self.render_prompt_diff(change));
        }
        if state.executing {
            panel = panel.child(self.prompt_note("Applying…"));
        }
        if let Some(error) = &dropdown.error {
            panel = panel.child(self.prompt_error(error.clone()));
        }
        panel
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .when(!ready, |button| button.opacity(0.5))
                            .child(self.prompt_button(
                                actions,
                                PromptAction::Confirm,
                                label,
                                true,
                                destructive,
                                cx,
                            )),
                    )
                    .child(self.prompt_button(
                        actions,
                        PromptAction::Cancel,
                        "Cancel",
                        false,
                        false,
                        cx,
                    )),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::prompt::{
        PromptApplied, PromptProviderInfo, PromptShell, PromptShellInfo, PromptState,
        PromptStyleEntry,
    };

    fn text(spans: &[AnsiSpan]) -> String {
        spans.iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn parses_truecolor_and_256_colors() {
        let spans = parse_ansi("\x1b[38;2;255;0;128mA\x1b[48;5;21mB\x1b[38;5;196mC");
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].fg, Some(AnsiColor::Rgb(0xff_00_80)));
        assert_eq!(spans[0].bg, None);
        assert_eq!(spans[1].fg, Some(AnsiColor::Rgb(0xff_00_80)));
        assert_eq!(spans[1].bg, Some(AnsiColor::Indexed(21)));
        assert_eq!(spans[2].fg, Some(AnsiColor::Indexed(196)));
        assert_eq!(spans[2].bg, Some(AnsiColor::Indexed(21)));
    }

    #[test]
    fn parses_basic_bright_and_reset() {
        let spans = parse_ansi("\x1b[31;1mR\x1b[92;22mG\x1b[104mB\x1b[39;49mD\x1b[4;3mU\x1b[mN");
        assert_eq!(text(&spans), "RGBDUN");
        assert_eq!(spans[0].fg, Some(AnsiColor::Indexed(1)));
        assert!(spans[0].bold);
        assert_eq!(spans[1].fg, Some(AnsiColor::Indexed(10)));
        assert!(!spans[1].bold);
        assert_eq!(spans[2].bg, Some(AnsiColor::Indexed(12)));
        assert_eq!((spans[3].fg, spans[3].bg), (None, None));
        assert!(spans[4].underline && spans[4].italic);
        assert_eq!(
            spans[5],
            AnsiSpan {
                text: "N".into(),
                ..AnsiSpan::default()
            }
        );
    }

    #[test]
    fn inverse_swaps_colors_including_defaults() {
        let spans = parse_ansi("\x1b[7mI\x1b[31mR\x1b[27mN");
        assert_eq!(spans[0].fg, Some(AnsiColor::Background));
        assert_eq!(spans[0].bg, Some(AnsiColor::Foreground));
        assert_eq!(spans[1].fg, Some(AnsiColor::Background));
        assert_eq!(spans[1].bg, Some(AnsiColor::Indexed(1)));
        assert_eq!(spans[2].fg, Some(AnsiColor::Indexed(1)));
        assert_eq!(spans[2].bg, None);
    }

    #[test]
    fn strips_osc_other_sequences_controls_and_leading_newline() {
        let spans = parse_ansi(
            "\n\x1b]0;title\x07A\x1b]2;other\x1b\\B\x1b[2K\x1b[?25lC\x1b(B\x1b7D\x01E\x02\r",
        );
        assert_eq!(text(&spans), "ABCDE");
        assert_eq!(spans.len(), 1);
        let lines = ansi_lines(parse_ansi("\x1b[32mtop\nbottom"));
        assert_eq!(lines.len(), 2);
        assert_eq!(text(&lines[1]), "bottom");
        assert_eq!(lines[1][0].fg, Some(AnsiColor::Indexed(2)));
    }

    fn preset(name: &str) -> PromptStyle {
        PromptStyle::Preset { name: name.into() }
    }

    fn provider(provider: PromptProvider, installed: bool, styles: &[&str]) -> PromptProviderInfo {
        PromptProviderInfo {
            provider,
            path: installed.then(|| format!("/usr/bin/{}", provider.executable())),
            version: None,
            styles: styles
                .iter()
                .map(|name| PromptStyleEntry {
                    style: preset(name),
                    label: (*name).into(),
                })
                .collect(),
        }
    }

    fn applied(provider: PromptProvider, style: PromptStyle) -> PromptApplied {
        PromptApplied {
            config: PromptConfig { provider, style },
            shell: None,
            startup_file: None,
            applied_at_ms: 1,
        }
    }

    fn environment(providers: Vec<PromptProviderInfo>, state: PromptState) -> PromptEnvironment {
        let shell = PromptShellInfo {
            path: "/bin/bash".into(),
            shell: Some(PromptShell::Bash),
        };
        PromptEnvironment {
            target: "WSL Ubuntu".into(),
            distribution: None,
            distributions: Vec::new(),
            home: "/home/user".into(),
            compi_shell: shell.clone(),
            login_shell: shell,
            providers,
            startup_lines: Vec::new(),
            state,
            backups: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn selection_defaults_to_first_installed_provider_style() {
        let detected = environment(
            vec![
                provider(PromptProvider::OhMyPosh, false, &["atomic"]),
                provider(PromptProvider::Starship, true, &["pastel", "plain"]),
            ],
            PromptState::default(),
        );
        assert_eq!(
            initial_selection(&detected),
            Some(PromptConfig {
                provider: PromptProvider::Starship,
                style: preset("pastel"),
            })
        );
        let none = environment(
            vec![provider(PromptProvider::Starship, false, &["pastel"])],
            PromptState::default(),
        );
        assert_eq!(initial_selection(&none), None);
    }

    #[test]
    fn applied_style_is_selected_and_pinned_first() {
        let providers = vec![
            provider(
                PromptProvider::Starship,
                true,
                &["pastel", "plain", "jetpack"],
            ),
            provider(PromptProvider::OhMyPosh, true, &["atomic"]),
        ];
        let compi = environment(
            providers.clone(),
            PromptState {
                compi: Some(applied(PromptProvider::Starship, preset("plain"))),
                normal: Some(applied(PromptProvider::OhMyPosh, preset("atomic"))),
            },
        );
        assert_eq!(
            initial_selection(&compi),
            Some(PromptConfig {
                provider: PromptProvider::Starship,
                style: preset("plain"),
            })
        );
        let rows = style_rows(&compi, PromptProvider::Starship);
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["plain", "pastel", "jetpack"]
        );
        assert!(rows[0].current && !rows[1].current);
        assert!(
            style_rows(&compi, PromptProvider::OhMyPosh)
                .iter()
                .all(|row| !row.current)
        );

        let outside_only = environment(
            providers,
            PromptState {
                compi: None,
                normal: Some(applied(
                    PromptProvider::OhMyPosh,
                    PromptStyle::Theme {
                        path: "/home/user/mine.omp.json".into(),
                    },
                )),
            },
        );
        assert_eq!(
            initial_selection(&outside_only).map(|config| config.provider),
            Some(PromptProvider::OhMyPosh)
        );
        let rows = style_rows(&outside_only, PromptProvider::OhMyPosh);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].current);
        assert_eq!(rows[0].label, "mine.omp.json");
    }

    #[test]
    fn outside_defaults_to_an_applied_supported_login_shell() {
        let starship = || vec![provider(PromptProvider::Starship, true, &["pastel"])];
        let mut detected = environment(starship(), PromptState::default());
        assert!(!initial_outside(&detected));
        detected.state.normal = Some(applied(PromptProvider::Starship, preset("pastel")));
        assert!(initial_outside(&detected));
        detected.login_shell = PromptShellInfo {
            path: "/usr/bin/fish".into(),
            shell: None,
        };
        assert!(!initial_outside(&detected));
    }

    #[test]
    fn only_the_applied_style_and_scope_skips_the_dropdown() {
        let config = |name: &str| PromptConfig {
            provider: PromptProvider::Starship,
            style: preset(name),
        };
        let mut detected = environment(
            vec![provider(
                PromptProvider::Starship,
                true,
                &["pastel", "plain"],
            )],
            PromptState {
                compi: Some(applied(PromptProvider::Starship, preset("pastel"))),
                normal: None,
            },
        );
        assert!(already_applied(&detected, &config("pastel"), false));
        // Same style, wider scope: the login shell still changes.
        assert!(!already_applied(&detected, &config("pastel"), true));
        assert!(!already_applied(&detected, &config("plain"), false));
        detected.state.normal = Some(applied(PromptProvider::Starship, preset("pastel")));
        assert!(already_applied(&detected, &config("pastel"), true));
        // Narrower scope removes the login-shell prompt.
        assert!(!already_applied(&detected, &config("pastel"), false));
        detected.state.normal = Some(applied(PromptProvider::Starship, preset("plain")));
        assert!(!already_applied(&detected, &config("pastel"), true));
        assert!(!already_applied(
            &environment(Vec::new(), PromptState::default()),
            &config("pastel"),
            false
        ));
    }
}
