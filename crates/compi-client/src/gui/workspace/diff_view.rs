//! One changed file drawn as a diff, an image compare, or a short message when there is
//! nothing to draw line by line: the body of a diff tab. Its own GPUI view, so it owns its
//! background work (the diff and the syntax highlighting are built off the UI thread), its
//! scroll position, the expanded context, find and hunk navigation.
//!
//! Long lines wrap inside their row (or half); the diff never scrolls sideways. SVG sides are
//! compared as images: GPUI rasterizes SVG itself, so a drawing reads better than its markup.
use super::content::{HIGHLIGHT_LIMIT, human_size, language_for};
use super::*;
use crate::diff::{
    self, DiffLine, Expand, FileDiff, Hunk, Layout, LineKind, Match, Row, Side, Whitespace,
};
use crate::gui::controls::{self, IconName, icon_button, text_button};
use base64::Engine as _;
use compi_protocol::{GitFileContent, GitImageFormat};
use gpui::{
    BorderStyle, ClickEvent, Div, Entity, EventEmitter, Font, HighlightStyle, Image, ImageFormat,
    ImageSource, ListAlignment, ListOffset, ListState, ObjectFit, Size, StyledImage as _,
    StyledText, Task, TextLayout, WeakEntity, img, list, quad,
};
use gpui_component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_component::input::{Input, InputEvent, InputState, Rope};
use gpui_component::scroll::Scrollbar;
use gpui_component::theme::Theme;
use std::rc::Rc;

/// Auto layout splits when the view is wider than this many terminal cells.
const SPLIT_MIN_CELLS: f32 = 112.0;
/// Hidden lines one click on a fold reveals.
const EXPAND_STEP: u32 = 20;
/// A tab advances to the next multiple of this many cells.
const TAB_CELLS: usize = 4;
/// Row tint of added and removed lines, and the stronger tint of their changed words.
const ROW_TINT: f32 = 0.12;
const WORD_TINT: f32 = 0.32;
const WORD_RADIUS: f32 = 3.0;
/// The bar at the left edge of added and removed lines.
const BAR_WIDTH: f32 = 2.0;
const FIND_FIELD_WIDTH: f32 = 180.0;
/// Checkerboard squares behind images.
const CHECKER: f32 = 8.0;
const IMAGE_GAP: f32 = 16.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum LayoutMode {
    /// Split when the view is wide enough, else unified.
    #[default]
    Auto,
    Unified,
    Split,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct DiffSettings {
    pub layout: LayoutMode,
    pub whitespace: Whitespace,
}

/// The window's colors and terminal font, set by the pane before each draw.
#[derive(Clone)]
pub(super) struct DiffStyle {
    pub colors: ThemeColors,
    pub typography: Arc<TerminalTypography>,
}

impl DiffStyle {
    fn same(&self, other: &DiffStyle) -> bool {
        self.colors == other.colors && Arc::ptr_eq(&self.typography, &other.typography)
    }
}

/// What the button on each hunk does, as the tab showing the diff decides: stage it from
/// a diff of the work tree against the index, unstage it from a diff of the index against
/// `HEAD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HunkButton {
    Stage,
    Unstage,
}

impl HunkButton {
    fn label(self) -> &'static str {
        match self {
            HunkButton::Stage => "Stage hunk",
            HunkButton::Unstage => "Unstage hunk",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            HunkButton::Stage => "Adds this hunk to the index",
            HunkButton::Unstage => "Takes this hunk out of the index",
        }
    }
}

/// A hunk's button was clicked: the diff on screen and the hunk of it to move.
pub(super) struct HunkClicked {
    pub diff: FileDiff,
    pub hunk: Hunk,
}

/// Both sides as read from the daemon.
struct Sources {
    old: GitFileContent,
    new: GitFileContent,
}

/// Syntax styles over each side's whole text, sorted and disjoint.
struct Highlights {
    theme: Arc<HighlightTheme>,
    old: Vec<(Range<usize>, HighlightStyle)>,
    new: Vec<(Range<usize>, HighlightStyle)>,
}

impl Highlights {
    fn side(&self, side: Side) -> &[(Range<usize>, HighlightStyle)] {
        match side {
            Side::Old => &self.old,
            Side::New => &self.new,
        }
    }
}

/// What a background build produced.
enum Built {
    Text {
        diff: FileDiff,
        highlights: Arc<Highlights>,
        /// Why there is nothing to draw, for an empty diff.
        note: Option<&'static str>,
    },
    Images {
        old: ImageSide,
        new: ImageSide,
    },
    Message(String),
}

enum ImageSide {
    Image {
        image: Arc<Image>,
        len: u64,
        /// Pixel size read from the header; `None` for SVG, which GPUI measures once drawn.
        pixels: Option<(u32, u32)>,
    },
    Missing,
    Unreadable {
        len: u64,
    },
}

impl ImageSide {
    fn image(&self) -> Option<&Arc<Image>> {
        match self {
            ImageSide::Image { image, .. } => Some(image),
            _ => None,
        }
    }
}

enum Body {
    /// The first build has not landed.
    Loading,
    Text(TextBody),
    Images {
        old: ImageSide,
        new: ImageSide,
    },
    Message(SharedString),
}

struct TextBody {
    /// Which `Sources` the diff was built from.
    sources: u64,
    diff: FileDiff,
    highlights: Arc<Highlights>,
    note: Option<&'static str>,
    rows: Rc<Vec<Row>>,
    /// The first row of each hunk with changes, and that hunk's index in the diff.
    hunks: Rc<Vec<(usize, usize)>>,
    /// The row showing each old and new line, built on demand.
    line_rows: Option<Rc<LineRows>>,
}

struct LineRows {
    old: Vec<u32>,
    new: Vec<u32>,
}

impl LineRows {
    fn row(&self, side: Side, line: u32) -> Option<usize> {
        let rows = match side {
            Side::Old => &self.old,
            Side::New => &self.new,
        };
        rows.get(line as usize).map(|&row| row as usize)
    }
}

/// Where the list is scrolled, by content: the first visible line.
#[derive(Clone, Copy)]
struct Anchor {
    side: Side,
    line: u32,
    offset: Pixels,
}

struct Find {
    input: Entity<InputState>,
    _events: Subscription,
    query: String,
    matches: Rc<Vec<Match>>,
    /// `(side, line, match)` sorted, to look up the matches of a drawn line.
    by_line: Rc<Vec<(u8, u32, u32)>>,
    current: Option<usize>,
    /// Focus to return to when find closes.
    return_focus: Option<FocusHandle>,
    /// The find bar: Tab moves between its field and buttons inside it.
    bar: FocusHandle,
}

pub(super) struct DiffView {
    /// This view, for row event handlers.
    view: WeakEntity<DiffView>,
    path: String,
    sources: Arc<Sources>,
    /// Bumped whenever `sources` is replaced.
    sources_id: u64,
    settings: DiffSettings,
    style: Option<DiffStyle>,
    body: Body,
    /// Bumped by every build; a build landing with an older value is dropped.
    generation: u64,
    build: Option<Task<()>>,
    /// A highlight for a changed syntax theme, and the theme it is for.
    rehighlight: Option<(Arc<HighlightTheme>, Task<()>)>,
    list: ListState,
    images_scroll: ScrollHandle,
    context: diff::Context,
    layout: Layout,
    /// The view's size when last drawn.
    size: Option<Size<Pixels>>,
    find: Option<Find>,
    /// A line (0-based) asked for before it could be scrolled to: before the first build
    /// landed or the view was sized.
    reveal: Option<(Side, u32)>,
    /// The button a hunk shows while it is under the pointer or current; `None` shows none.
    hunk_button: Option<HunkButton>,
    /// The hunk (index in the diff) whose write is in flight: its button shows a spinner.
    pending_hunk: Option<usize>,
    /// The row under the pointer: its hunk shows its button.
    hovered_row: Option<usize>,
    /// The hunk (index in the diff) `n`/`p` moved to: it shows its button, and Shift+S or
    /// Shift+U (the button's direction) clicks it.
    current_hunk: Option<usize>,
}

impl EventEmitter<HunkClicked> for DiffView {}

impl DiffView {
    /// `path` is repository-relative (picks the grammar); sides as read from the daemon.
    pub(super) fn new(
        path: String,
        old: GitFileContent,
        new: GitFileContent,
        settings: DiffSettings,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|this, cx| {
            if let Body::Images { old, new } = &this.body {
                release_images([old, new], cx);
            }
        })
        .detach();
        let mut view = Self {
            view: cx.weak_entity(),
            path,
            sources: Arc::new(Sources { old, new }),
            sources_id: 0,
            settings,
            style: None,
            body: Body::Loading,
            generation: 0,
            build: None,
            rehighlight: None,
            list: ListState::new(0, ListAlignment::Top, px(400.0)),
            images_scroll: ScrollHandle::new(),
            context: diff::Context::default(),
            layout: decide_layout(settings.layout, None, None),
            size: None,
            find: None,
            reveal: None,
            hunk_button: None,
            pending_hunk: None,
            hovered_row: None,
            current_hunk: None,
        };
        view.start_build(cx);
        view
    }

    /// Same file re-read (live refresh): keeps scroll anchor (by side+line), expanded context
    /// and find; no flicker.
    pub(super) fn replace(
        &mut self,
        old: GitFileContent,
        new: GitFileContent,
        cx: &mut Context<Self>,
    ) {
        self.sources = Arc::new(Sources { old, new });
        self.sources_id += 1;
        self.start_build(cx);
    }

    pub(super) fn set_settings(&mut self, settings: DiffSettings, cx: &mut Context<Self>) {
        if settings == self.settings {
            return;
        }
        let rebuild = settings.whitespace != self.settings.whitespace;
        self.settings = settings;
        if rebuild {
            self.start_build(cx);
        }
        self.relayout(cx);
        cx.notify();
    }

    /// Colors and font of the window; cheap when unchanged.
    pub(super) fn set_style(&mut self, style: DiffStyle, cx: &mut Context<Self>) {
        if self
            .style
            .as_ref()
            .is_some_and(|current| current.same(&style))
        {
            return;
        }
        let font_changed = self
            .style
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(&current.typography, &style.typography));
        self.style = Some(style);
        if font_changed {
            self.list.remeasure();
            self.relayout(cx);
        }
        cx.notify();
    }

    /// The button a hunk under the pointer or current shows; cheap when unchanged.
    pub(super) fn set_hunk_button(&mut self, button: Option<HunkButton>, cx: &mut Context<Self>) {
        if self.hunk_button != button {
            self.hunk_button = button;
            cx.notify();
        }
    }

    /// Hunk `index` of the diff on screen asks to move. Only exact diffs move hunks: under
    /// an ignore mode a hunk would carry differences the diff does not show.
    fn click_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        let Body::Text(text) = &self.body else {
            return;
        };
        if self.hunk_button.is_none() || self.settings.whitespace != Whitespace::Exact {
            return;
        }
        if let Some(hunk) = text.diff.hunks().get(index).cloned() {
            let diff = text.diff.clone();
            self.pending_hunk = Some(index);
            cx.emit(HunkClicked { diff, hunk });
            cx.notify();
        }
    }

    /// The write a hunk's button started has answered: its button acts again.
    pub(super) fn finish_hunk_write(&mut self, cx: &mut Context<Self>) {
        if self.pending_hunk.take().is_some() {
            cx.notify();
        }
    }

    /// The pointer entered (`hovered`) or left row `ix`; the hunk under it shows its button.
    fn hover_row(&mut self, ix: usize, hovered: bool, cx: &mut Context<Self>) {
        let row = if hovered {
            Some(ix)
        } else if self.hovered_row == Some(ix) {
            None
        } else {
            return;
        };
        if self.hovered_row != row {
            let before = self.hunk_at_row(self.hovered_row);
            self.hovered_row = row;
            if self.hunk_at_row(row) != before {
                cx.notify();
            }
        }
    }

    /// The hunk (index in the diff) row `row` belongs to: from a hunk's first row to the next
    /// hunk or fold.
    fn hunk_at_row(&self, row: Option<usize>) -> Option<usize> {
        let (Body::Text(text), Some(row)) = (&self.body, row) else {
            return None;
        };
        let at = text.hunks.partition_point(|&(start, _)| start <= row);
        let &(start, index) = text.hunks.get(at.checked_sub(1)?)?;
        let folded = text
            .rows
            .get(start..=row)?
            .iter()
            .any(|row| matches!(row, Row::Fold { .. }));
        (!folded).then_some(index)
    }

    /// The first build has landed: there is something to draw.
    pub(super) fn is_ready(&self) -> bool {
        !matches!(self.body, Body::Loading)
    }

    /// Added/removed under the current whitespace mode once built (for the file header).
    pub(super) fn stats(&self) -> Option<(u32, u32)> {
        match &self.body {
            Body::Text(text) => Some((text.diff.added(), text.diff.removed())),
            _ => None,
        }
    }

    /// Every unchanged line is shown.
    pub(super) fn context_expanded(&self) -> bool {
        self.context.is_all()
    }

    pub(super) fn set_context_expanded(&mut self, all: bool, cx: &mut Context<Self>) {
        if all == self.context.is_all() {
            return;
        }
        let anchor = self.anchor();
        if all {
            self.context.expand_all();
        } else {
            self.context.collapse_all();
        }
        self.rebuild_rows(anchor);
        cx.notify();
    }

    /// Scrolls 1-based `line` of `side` a third of the way down, unfolding it if hidden.
    /// Asked before the diff is built or drawn, it happens once it is.
    pub(super) fn reveal_line(&mut self, side: Side, line: u32, cx: &mut Context<Self>) {
        self.reveal = Some((side, line.saturating_sub(1)));
        self.apply_reveal();
        cx.notify();
    }

    /// The line a third of the way down the view, 1-based: a new-side line when the diff
    /// has one near there, else an old-side one. `None` without a text diff.
    pub(super) fn line_in_view(&self) -> Option<(Side, u32)> {
        let Body::Text(text) = &self.body else {
            return None;
        };
        let reference = (self.list.logical_scroll_top().item_ix + self.rows_above(1.0 / 3.0))
            .min(text.rows.len());
        // From the reference row down, then up from it.
        let near = |side: Side| {
            let (above, below) = text.rows.split_at(reference);
            below
                .iter()
                .chain(above.iter().rev())
                .find_map(|row| row_line(row, side))
        };
        near(Side::New)
            .map(|line| (Side::New, line + 1))
            .or_else(|| near(Side::Old).map(|line| (Side::Old, line + 1)))
    }

    /// Scrolls to the requested line once there are rows and a size to place it with.
    fn apply_reveal(&mut self) {
        let (Some((side, line)), Body::Text(text), Some(_)) = (self.reveal, &self.body, self.size)
        else {
            return;
        };
        let len = match side {
            Side::Old => text.diff.old_len(),
            Side::New => text.diff.new_len(),
        };
        self.reveal = None;
        if len == 0 {
            return;
        }
        let line = line.min(len - 1);
        let (rows, diff) = (text.rows.clone(), text.diff.clone());
        let row = self.line_rows().row(side, line);
        if !row.is_some_and(|row| rows.get(row).and_then(|row| row_line(row, side)) == Some(line)) {
            self.context.reveal(&diff, side, line);
            self.rebuild_rows(None);
        }
        if let Some(row) = self.line_rows().row(side, line) {
            self.scroll_row_to(row, 1.0 / 3.0);
        }
    }

    /// Keys while the pane has focus and no text field does: Ctrl/Cmd+F find, Enter/Shift+Enter
    /// next/prev match while find is open, Escape closes find, n/p next/previous hunk, e expand
    /// all/collapse, PageUp/PageDown/Home/End, Up/Down scroll by a line. Returns whether consumed.
    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        let key = keystroke.key.as_str();
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform && !modifiers.control
        } else {
            modifiers.control && !modifiers.platform
        };
        if primary && !modifiers.alt && !modifiers.shift && key.eq_ignore_ascii_case("f") {
            self.focus_find(window, cx);
            return true;
        }
        if self.find_key(event, window, cx) {
            return true;
        }
        if self.find_focused(window, cx) || modifiers.modified() && !modifiers.shift {
            return false;
        }
        let Body::Text(text) = &self.body else {
            return false;
        };
        if text.rows.is_empty() {
            return false;
        }
        let line = self.line_height();
        let page = (self.list.viewport_bounds().size.height - line * 2.0).max(line);
        match (key, modifiers.shift) {
            ("n", false) => self.step_hunk(true),
            ("p", false) => self.step_hunk(false),
            // The current hunk's button: Shift+S stages it, Shift+U unstages it.
            ("s", true) | ("u", true) => {
                let wanted = if key == "s" {
                    HunkButton::Stage
                } else {
                    HunkButton::Unstage
                };
                match self.current_hunk {
                    Some(index) if self.hunk_button == Some(wanted) => self.click_hunk(index, cx),
                    _ => return false,
                }
            }
            ("e", false) => {
                let all = !self.context.is_all();
                self.set_context_expanded(all, cx);
            }
            ("pagedown", _) => self.list.scroll_by(page),
            ("pageup", _) => self.list.scroll_by(-page),
            ("home", _) => self.list.scroll_to(ListOffset::default()),
            ("end", _) => self.list.scroll_to_end(),
            ("down", false) => self.list.scroll_by(line),
            ("up", false) => self.list.scroll_by(-line),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn focus_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.body, Body::Text(_)) {
            return;
        }
        let find = match &mut self.find {
            Some(find) => find,
            None => {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
                let events = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.query_changed(cx);
                    }
                });
                self.find.insert(Find {
                    input,
                    _events: events,
                    query: String::new(),
                    matches: Rc::default(),
                    by_line: Rc::default(),
                    current: None,
                    return_focus: None,
                    bar: cx.focus_handle(),
                })
            }
        };
        let field = find.input.read(cx).focus_handle(cx);
        if let Some(focused) = window.focused(cx).filter(|focused| *focused != field) {
            find.return_focus = Some(focused);
        }
        find.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    fn find_focused(&self, window: &Window, cx: &App) -> bool {
        self.find
            .as_ref()
            .is_some_and(|find| find.input.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Enter, Shift+Enter and Escape while find is open, and Tab/Shift+Tab inside the find
    /// bar. Plain Enter on one of the bar's buttons is the button's: it clicks it.
    fn find_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(find) = &self.find else {
            return false;
        };
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return false;
        }
        let bar = find.bar.clone();
        let in_bar = bar.contains_focused(window, cx);
        let on_button = in_bar && !find.input.read(cx).focus_handle(cx).is_focused(window);
        match event.keystroke.key.as_str() {
            "tab" if in_bar => {
                controls::cycle_focus(&bar, !modifiers.shift, window, cx);
            }
            "enter" if modifiers.shift || !on_button => self.step_match(!modifiers.shift, cx),
            "escape" => self.close_find(window, cx),
            _ => return false,
        }
        true
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.take() else {
            return;
        };
        if let Some(focus) = find.return_focus {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn query_changed(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        let query = find.input.read(cx).value().to_string();
        if query == find.query {
            return;
        }
        find.query = query;
        self.refresh_matches(false);
        if let Some(current) = self.find.as_ref().and_then(|find| find.current) {
            self.show_match(current, false);
        }
        cx.notify();
    }

    /// Matches for the query in the current diff. `keep` keeps the current match when it still
    /// exists; otherwise it becomes the first match at or below the top of the view.
    fn refresh_matches(&mut self, keep: bool) {
        let Body::Text(text) = &self.body else {
            return;
        };
        let Some(find) = &self.find else {
            return;
        };
        let matches = diff::find(&text.diff, &find.query);
        let mut by_line: Vec<(u8, u32, u32)> = matches
            .iter()
            .enumerate()
            .map(|(ix, hit)| (side_key(hit.side), hit.line, ix as u32))
            .collect();
        by_line.sort_unstable();
        let previous = find.current;
        let current = if matches.is_empty() {
            None
        } else if keep && previous.is_some_and(|current| current < matches.len()) {
            previous
        } else {
            let top = self.list.logical_scroll_top().item_ix;
            let rows = self.line_rows();
            Some(
                matches
                    .iter()
                    .position(|hit| rows.row(hit.side, hit.line).is_some_and(|row| row >= top))
                    .unwrap_or(0),
            )
        };
        if let Some(find) = &mut self.find {
            find.matches = Rc::new(matches);
            find.by_line = Rc::new(by_line);
            find.current = current;
        }
    }

    fn step_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        let count = find.matches.len();
        if count == 0 {
            return;
        }
        let next = match find.current {
            None => 0,
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
        };
        find.current = Some(next);
        self.show_match(next, true);
        cx.notify();
    }

    /// Scrolls match `ix` to the middle of the view, revealing it from a fold first. Unless
    /// `always`, a match already in view stays put.
    fn show_match(&mut self, ix: usize, always: bool) {
        let Some(hit) = self
            .find
            .as_ref()
            .and_then(|find| find.matches.get(ix))
            .cloned()
        else {
            return;
        };
        let Body::Text(text) = &self.body else {
            return;
        };
        let (rows, diff) = (text.rows.clone(), text.diff.clone());
        let mut row = self.line_rows().row(hit.side, hit.line);
        let shown = row.is_some_and(|row| {
            rows.get(row)
                .is_some_and(|row| row_line(row, hit.side) == Some(hit.line))
        });
        if !shown {
            let anchor = self.anchor();
            self.context.reveal(&diff, hit.side, hit.line);
            self.rebuild_rows(anchor);
            row = self.line_rows().row(hit.side, hit.line);
        } else if !always && self.row_in_view(row.unwrap_or_default()) {
            return;
        }
        if let Some(row) = row {
            self.scroll_row_to(row, 0.5);
        }
    }

    fn row_in_view(&self, row: usize) -> bool {
        let top = self.list.logical_scroll_top().item_ix;
        let visible = (self.list.viewport_bounds().size.height / self.line_height()) as usize;
        row >= top && row + 1 < top + visible
    }

    /// n / p: the next or previous hunk, a third of the way down the view; it becomes the
    /// current hunk.
    fn step_hunk(&mut self, forward: bool) {
        let Body::Text(text) = &self.body else {
            return;
        };
        let current = self
            .current_hunk
            .and_then(|current| text.hunks.iter().find(|&&(_, index)| index == current))
            .map(|&(start, _)| start)
            .filter(|&start| self.row_in_view(start));
        let top = self.list.logical_scroll_top().item_ix;
        let reference = top + self.rows_above(1.0 / 3.0);
        if let Some((start, index)) = next_hunk(&text.hunks, current, top, reference, forward) {
            self.current_hunk = Some(index);
            self.scroll_row_to(start, 1.0 / 3.0);
        }
    }

    /// Rows (counted as one line each) that fit above `fraction` of the view's height.
    fn rows_above(&self, fraction: f32) -> usize {
        let height = self.list.viewport_bounds().size.height;
        ((height * fraction) / self.line_height()).floor().max(0.0) as usize
    }

    fn scroll_row_to(&self, row: usize, fraction: f32) {
        self.list.scroll_to(ListOffset {
            item_ix: row.saturating_sub(self.rows_above(fraction)),
            offset_in_item: px(0.0),
        });
    }

    fn line_height(&self) -> Pixels {
        px(self
            .style
            .as_ref()
            .map_or(UI_SMALL_LINE_HEIGHT, |style| style.typography.cell_height))
    }

    fn start_build(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let sources = self.sources.clone();
        let sources_id = self.sources_id;
        let whitespace = self.settings.whitespace;
        let theme = Theme::global(cx).highlight_theme.clone();
        let reuse = match &self.body {
            Body::Text(text) if text.sources == sources_id && *text.highlights.theme == *theme => {
                Some(text.highlights.clone())
            }
            _ => None,
        };
        let language = language_for(&self.path);
        self.build = Some(cx.spawn(async move |this, cx| {
            let built = cx
                .background_executor()
                .spawn(async move { build(&sources, whitespace, language, theme, reuse) })
                .await;
            this.update(cx, |this, cx| this.land(generation, sources_id, built, cx))
                .ok();
        }));
    }

    fn land(&mut self, generation: u64, sources: u64, built: Built, cx: &mut Context<Self>) {
        if generation != self.generation {
            return;
        }
        self.build = None;
        // The hunks on screen change: a spinner would no longer mark the hunk it started on,
        // nor the current hunk the one `n`/`p` reached.
        self.pending_hunk = None;
        self.current_hunk = None;
        let anchor = self.anchor();
        let body = match built {
            Built::Text {
                diff,
                highlights,
                note,
            } => {
                let same_hunks = match &self.body {
                    Body::Text(previous) => previous.diff.hunks().len() == diff.hunks().len(),
                    _ => false,
                };
                if !same_hunks && !self.context.is_all() {
                    self.context = diff::Context::default();
                }
                Body::Text(TextBody {
                    sources,
                    diff,
                    highlights,
                    note,
                    rows: Rc::default(),
                    hunks: Rc::default(),
                    line_rows: None,
                })
            }
            Built::Images { old, new } => Body::Images { old, new },
            Built::Message(message) => Body::Message(message.into()),
        };
        let previous = std::mem::replace(&mut self.body, body);
        if let Body::Images { old, new } = &previous {
            let kept: Vec<u64> = match &self.body {
                Body::Images { old, new } => [old, new]
                    .into_iter()
                    .filter_map(|side| side.image().map(|image| image.id()))
                    .collect(),
                _ => Vec::new(),
            };
            let dropped = [old, new].into_iter().filter(|side| {
                side.image()
                    .is_some_and(|image| !kept.contains(&image.id()))
            });
            release_images(dropped, cx);
        }
        match &self.body {
            Body::Text(_) => {
                self.rebuild_rows(anchor);
                self.refresh_matches(true);
                self.apply_reveal();
            }
            _ => {
                self.find = None;
                self.reveal = None;
                self.list.reset(0);
            }
        }
        cx.notify();
    }

    /// The first visible line, to scroll back to after the rows change.
    fn anchor(&self) -> Option<Anchor> {
        let Body::Text(text) = &self.body else {
            return None;
        };
        let top = self.list.logical_scroll_top();
        text.rows
            .iter()
            .enumerate()
            .skip(top.item_ix)
            .find_map(|(ix, row)| {
                let (side, line) = row_line(row, Side::New)
                    .map(|line| (Side::New, line))
                    .or_else(|| row_line(row, Side::Old).map(|line| (Side::Old, line)))?;
                Some(Anchor {
                    side,
                    line,
                    offset: if ix == top.item_ix {
                        top.offset_in_item
                    } else {
                        px(0.0)
                    },
                })
            })
    }

    /// Rows for the current diff, context and layout, scrolled back to `anchor`.
    fn rebuild_rows(&mut self, anchor: Option<Anchor>) {
        let line_height = self.line_height();
        // Rows move: the pointer's row is known again once it moves.
        self.hovered_row = None;
        let Body::Text(text) = &mut self.body else {
            return;
        };
        let rows = diff::rows(&text.diff, &self.context, self.layout);
        // Formatting-only hunks have no first row; the others start rows in order.
        let changed = text
            .diff
            .hunks()
            .iter()
            .enumerate()
            .filter(|(_, hunk)| !hunk.formatting_only)
            .map(|(index, _)| index);
        text.hunks = Rc::new(diff::hunk_starts(&rows).into_iter().zip(changed).collect());
        text.rows = Rc::new(rows);
        text.line_rows = None;
        let count = text.rows.len();
        self.list.reset_with_uniform_height(count, line_height);
        if let Some(anchor) = anchor
            && let Some(row) = self.line_rows().row(anchor.side, anchor.line)
        {
            self.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: anchor.offset,
            });
        }
    }

    fn line_rows(&mut self) -> Rc<LineRows> {
        let Body::Text(text) = &mut self.body else {
            return Rc::new(LineRows {
                old: Vec::new(),
                new: Vec::new(),
            });
        };
        text.line_rows
            .get_or_insert_with(|| {
                Rc::new(LineRows {
                    old: line_rows(&text.rows, Side::Old, text.diff.old_len()),
                    new: line_rows(&text.rows, Side::New, text.diff.new_len()),
                })
            })
            .clone()
    }

    /// Re-decides the layout for the current size, mode and font.
    fn relayout(&mut self, cx: &mut Context<Self>) {
        let layout = decide_layout(
            self.settings.layout,
            self.size.map(|size| f32::from(size.width)),
            self.style.as_ref().map(|style| style.typography.cell_width),
        );
        if layout == self.layout {
            return;
        }
        let anchor = self.anchor();
        self.layout = layout;
        self.rebuild_rows(anchor);
        cx.notify();
    }

    fn resized(&mut self, size: Size<Pixels>, cx: &mut Context<Self>) {
        if self.size == Some(size) {
            return;
        }
        self.size = Some(size);
        self.relayout(cx);
        self.apply_reveal();
        if matches!(self.body, Body::Images { .. }) {
            cx.notify();
        }
    }

    fn expand_fold(&mut self, gap: usize, direction: Expand, cx: &mut Context<Self>) {
        let anchor = self.anchor();
        self.context.expand(gap, direction, EXPAND_STEP);
        self.rebuild_rows(anchor);
        cx.notify();
    }

    /// The syntax theme changed since the highlight was made: highlight again, keeping the old
    /// colors on screen until it lands.
    fn check_highlight_theme(&mut self, cx: &mut Context<Self>) {
        let theme = Theme::global(cx).highlight_theme.clone();
        let Body::Text(text) = &self.body else {
            return;
        };
        if *text.highlights.theme == *theme
            || self
                .rehighlight
                .as_ref()
                .is_some_and(|(pending, _)| **pending == *theme)
        {
            return;
        }
        let diff = text.diff.clone();
        let sources = text.sources;
        let language = language_for(&self.path);
        let task_theme = theme.clone();
        let task = cx.spawn(async move |this, cx| {
            let highlights = cx
                .background_executor()
                .spawn(async move { highlight_both(&diff, language, task_theme) })
                .await;
            this.update(cx, |this, cx| {
                this.rehighlight = None;
                if let Body::Text(text) = &mut this.body
                    && text.sources == sources
                {
                    text.highlights = Arc::new(highlights);
                    cx.notify();
                }
            })
            .ok();
        });
        self.rehighlight = Some((theme, task));
    }
}

fn release_images<'a>(sides: impl IntoIterator<Item = &'a ImageSide>, cx: &mut App) {
    for side in sides {
        if let Some(image) = side.image() {
            ImageSource::Image(image.clone()).remove_asset(cx);
        }
    }
}

fn side_key(side: Side) -> u8 {
    match side {
        Side::Old => 0,
        Side::New => 1,
    }
}

/// The layout for `mode` in a view `width` wide with terminal cells `cell_width` wide.
fn decide_layout(mode: LayoutMode, width: Option<f32>, cell_width: Option<f32>) -> Layout {
    match mode {
        LayoutMode::Unified => Layout::Unified,
        LayoutMode::Split => Layout::Split,
        LayoutMode::Auto => match (width, cell_width) {
            (Some(width), Some(cell)) if cell > 0.0 && width > SPLIT_MIN_CELLS * cell => {
                Layout::Split
            }
            _ => Layout::Unified,
        },
    }
}

/// The line of `side` a row shows.
fn row_line(row: &Row, side: Side) -> Option<u32> {
    match (row, side) {
        (Row::Fold { .. }, _) => None,
        (Row::Unified { old, .. }, Side::Old) => *old,
        (Row::Unified { new, .. }, Side::New) => *new,
        (Row::Split { old, .. }, Side::Old) => old.as_ref().map(|line| line.ix),
        (Row::Split { new, .. }, Side::New) => new.as_ref().map(|line| line.ix),
    }
}

/// The row showing each of the `len` lines of `side`; a hidden line maps to its fold (the row
/// after the last shown line before it).
fn line_rows(rows: &[Row], side: Side, len: u32) -> Vec<u32> {
    const UNSET: u32 = u32::MAX;
    let mut map = vec![UNSET; len as usize];
    for (ix, row) in rows.iter().enumerate() {
        if let Some(slot) = row_line(row, side).and_then(|line| map.get_mut(line as usize))
            && *slot == UNSET
        {
            *slot = ix as u32;
        }
    }
    let last = rows.len().saturating_sub(1) as u32;
    let mut previous: Option<(u32, bool)> = None;
    for slot in &mut map {
        if *slot == UNSET {
            *slot = match previous {
                None => 0,
                Some((row, true)) => row,
                Some((row, false)) => (row + 1).min(last),
            };
            previous = Some((*slot, true));
        } else {
            previous = Some((*slot, false));
        }
    }
    map
}

fn build(
    sources: &Sources,
    whitespace: Whitespace,
    language: &'static str,
    theme: Arc<HighlightTheme>,
    reuse: Option<Arc<Highlights>>,
) -> Built {
    use GitFileContent::{Image, Missing, Text, TooLarge};
    match (&sources.old, &sources.new) {
        (Missing, Missing) => Built::Message("No changes".into()),
        (TooLarge { len }, _) | (_, TooLarge { len }) => {
            let len = match (&sources.old, &sources.new) {
                (TooLarge { len: old }, TooLarge { len: new }) => *old.max(new),
                _ => *len,
            };
            Built::Message(format!("Too large to compare ({})", human_size(len)))
        }
        (Text { .. } | Missing, Text { .. } | Missing) => {
            fn text(side: &GitFileContent) -> &str {
                match side {
                    Text { text } => text,
                    _ => "",
                }
            }
            let (old, new) = (text(&sources.old), text(&sources.new));
            let diff = FileDiff::new(old, new, whitespace);
            let note = diff
                .is_empty()
                .then(|| empty_note(&diff, old, new, whitespace));
            let highlights =
                reuse.unwrap_or_else(|| Arc::new(highlight_both(&diff, language, theme)));
            Built::Text {
                diff,
                highlights,
                note,
            }
        }
        (Image { .. } | Missing, Image { .. } | Missing) => Built::Images {
            old: decode_image(&sources.old),
            new: decode_image(&sources.new),
        },
        (old, new) => Built::Message(describe_change(old, new)),
    }
}

/// Why an empty diff of two texts has nothing to show.
fn empty_note(diff: &FileDiff, old: &str, new: &str, whitespace: Whitespace) -> &'static str {
    if old == new {
        return "No changes";
    }
    let same_lines = diff.old_len() == diff.new_len()
        && (0..diff.old_len()).all(|ix| diff.old_line(ix) == diff.new_line(ix));
    if same_lines {
        if diff.no_newline_at_end(Side::Old) != diff.no_newline_at_end(Side::New) {
            "Only the newline at end of file changed"
        } else {
            "Only line endings changed"
        }
    } else {
        match whitespace {
            Whitespace::IgnoreFormatting => "Only formatting changed",
            _ => "Only whitespace changed",
        }
    }
}

/// A file whose sides cannot be compared line by line or as images.
fn describe_change(old: &GitFileContent, new: &GitFileContent) -> String {
    fn kind(side: &GitFileContent) -> Option<(&'static str, u64)> {
        match side {
            GitFileContent::Text { text } => Some(("text", text.len() as u64)),
            GitFileContent::Image { len, .. } => Some(("image", *len)),
            GitFileContent::Binary { len } | GitFileContent::TooLarge { len } => {
                Some(("binary", *len))
            }
            GitFileContent::Missing => None,
        }
    }
    let sizes = |old: u64, new: u64| {
        if old == new {
            human_size(new)
        } else {
            format!("{} → {}", human_size(old), human_size(new))
        }
    };
    match (kind(old), kind(new)) {
        (Some(("binary", old)), Some(("binary", new))) => {
            format!("Binary file · {}", sizes(old, new))
        }
        (None, Some((kind, new))) => format!("Added {kind} file · {}", human_size(new)),
        (Some((kind, old)), None) => format!("Deleted {kind} file · {}", human_size(old)),
        (Some((from, old)), Some((to, new))) => {
            format!("Changed from {from} to {to} · {}", sizes(old, new))
        }
        (None, None) => "No changes".to_owned(),
    }
}

fn highlight_both(
    diff: &FileDiff,
    language: &'static str,
    theme: Arc<HighlightTheme>,
) -> Highlights {
    let old = highlight(diff.old_text(), language, &theme);
    let new = highlight(diff.new_text(), language, &theme);
    Highlights { theme, old, new }
}

/// Syntax styles over all of `text`, with the editor's grammar and limit.
fn highlight(
    text: &str,
    language: &str,
    theme: &HighlightTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if language == "text" || text.is_empty() || text.len() > HIGHLIGHT_LIMIT {
        return Vec::new();
    }
    let mut highlighter = SyntaxHighlighter::new(language);
    highlighter.update(None, &Rope::from(text), None);
    highlighter.styles(&(0..text.len()), theme)
}

fn decode_image(side: &GitFileContent) -> ImageSide {
    let GitFileContent::Image {
        format,
        base64,
        len,
    } = side
    else {
        return ImageSide::Missing;
    };
    let len = *len;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(base64) else {
        return ImageSide::Unreadable { len };
    };
    let (format, raster) = match format {
        GitImageFormat::Png => (ImageFormat::Png, Some(image::ImageFormat::Png)),
        GitImageFormat::Jpeg => (ImageFormat::Jpeg, Some(image::ImageFormat::Jpeg)),
        GitImageFormat::Gif => (ImageFormat::Gif, Some(image::ImageFormat::Gif)),
        GitImageFormat::Webp => (ImageFormat::Webp, Some(image::ImageFormat::WebP)),
        GitImageFormat::Bmp => (ImageFormat::Bmp, Some(image::ImageFormat::Bmp)),
        GitImageFormat::Ico => (ImageFormat::Ico, Some(image::ImageFormat::Ico)),
        GitImageFormat::Svg => (ImageFormat::Svg, None),
    };
    let pixels = match raster {
        Some(raster) => {
            match image::ImageReader::with_format(std::io::Cursor::new(&bytes), raster)
                .into_dimensions()
            {
                Ok(pixels) => Some(pixels),
                Err(_) => return ImageSide::Unreadable { len },
            }
        }
        None => None,
    };
    ImageSide::Image {
        image: Arc::new(Image::from_bytes(format, bytes)),
        len,
        pixels,
    }
}

/// Decimal digits of `n`.
fn digits(n: u32) -> u32 {
    n.checked_ilog10().map_or(1, |log| log + 1)
}

/// The last line of each side (old, new) that ends without a newline while the other side's
/// text ends with one.
fn eof_lines(diff: &FileDiff) -> [Option<u32>; 2] {
    let (old, new) = (
        diff.no_newline_at_end(Side::Old),
        diff.no_newline_at_end(Side::New),
    );
    if old == new {
        return [None, None];
    }
    let last = |missing: bool, len: u32| (missing && len > 0).then(|| len - 1);
    [last(old, diff.old_len()), last(new, diff.new_len())]
}

/// `width` × `height` scaled down to fit `available`, never up.
fn fit(width: f32, height: f32, available: (f32, f32)) -> (f32, f32) {
    if width <= 0.0 || height <= 0.0 {
        return (0.0, 0.0);
    }
    let scale = (available.0 / width).min(available.1 / height).min(1.0);
    ((width * scale).max(1.0), (height * scale).max(1.0))
}

/// A line as drawn: tabs expanded to spaces, with byte offsets mapped from the line.
struct Expanded<'a> {
    text: std::borrow::Cow<'a, str>,
    /// `(byte of a tab in the line, bytes added through that tab)`.
    tabs: Vec<(usize, usize)>,
}

impl<'a> Expanded<'a> {
    fn new(line: &'a str) -> Self {
        if !line.contains('\t') {
            return Self {
                text: line.into(),
                tabs: Vec::new(),
            };
        }
        let mut text = String::with_capacity(line.len() + TAB_CELLS * 2);
        let mut tabs = Vec::new();
        let (mut column, mut added) = (0, 0);
        for (ix, c) in line.char_indices() {
            if c == '\t' {
                let spaces = TAB_CELLS - column % TAB_CELLS;
                text.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
                added += spaces - 1;
                tabs.push((ix, added));
            } else {
                text.push(c);
                column += 1;
            }
        }
        Self {
            text: text.into(),
            tabs,
        }
    }

    /// The drawn offset of line byte `ix`.
    fn offset(&self, ix: usize) -> usize {
        let before = self.tabs.partition_point(|&(tab, _)| tab < ix);
        ix + before.checked_sub(1).map_or(0, |tab| self.tabs[tab].1)
    }

    fn range(&self, range: &Range<usize>) -> Range<usize> {
        self.offset(range.start)..self.offset(range.end)
    }
}

/// The styles of the line spanning `line` (bytes of the whole text) relative to the line,
/// covering it without gaps; `None` where no style applies. `styles` are sorted and disjoint.
fn line_styles(
    styles: &[(Range<usize>, HighlightStyle)],
    line: Range<usize>,
) -> Vec<(Range<usize>, Option<HighlightStyle>)> {
    let mut out = Vec::new();
    let mut at = line.start;
    let first = styles.partition_point(|(range, _)| range.end <= line.start);
    for (range, style) in &styles[first..] {
        if range.start >= line.end {
            break;
        }
        let (start, end) = (range.start.max(at), range.end.min(line.end));
        if start >= end {
            continue;
        }
        if start > at {
            out.push((at - line.start..start - line.start, None));
        }
        out.push((start - line.start..end - line.start, Some(*style)));
        at = end;
    }
    if at < line.end {
        out.push((at - line.start..line.end - line.start, None));
    }
    out
}

/// `range` of a wrapped line split by visual line: `(visual line, range)`. `starts` holds the
/// byte where each visual line starts (the first is 0); `len` is the line's length.
fn segments(starts: &[usize], len: usize, range: Range<usize>) -> Vec<(usize, Range<usize>)> {
    let mut out = Vec::new();
    for (row, &start) in starts.iter().enumerate() {
        let end = starts.get(row + 1).copied().unwrap_or(len);
        let (from, to) = (range.start.max(start), range.end.min(end));
        if from < to {
            out.push((row, from..to));
        }
    }
    out
}

/// Colors of one draw.
#[derive(Clone, Copy)]
struct Palette {
    foreground: Hsla,
    muted: Hsla,
    dim: Hsla,
    number: Hsla,
    border: Hsla,
    surface: Hsla,
    hover: Hsla,
    added: Hsla,
    removed: Hsla,
    added_row: Hsla,
    removed_row: Hsla,
    added_word: Hsla,
    removed_word: Hsla,
    shade: Hsla,
    guide: Hsla,
    mark: Hsla,
    mark_current: Hsla,
    mark_border: Hsla,
    error: Hsla,
}

impl Palette {
    fn new(colors: &ThemeColors) -> Self {
        let foreground = color(colors.foreground);
        let muted = color(colors.muted);
        let added = color(colors.added);
        let removed = color(colors.removed);
        let selection = color(colors.selection);
        Self {
            foreground,
            muted,
            dim: muted.opacity(0.75),
            // As the editor's line numbers.
            number: muted.opacity(0.7),
            border: color(colors.border),
            surface: color(colors.surface),
            hover: color(colors.surface_hover),
            added,
            removed,
            added_row: added.opacity(ROW_TINT),
            removed_row: removed.opacity(ROW_TINT),
            added_word: added.opacity(WORD_TINT),
            removed_word: removed.opacity(WORD_TINT),
            shade: color(colors.surface).opacity(0.55),
            guide: foreground.opacity(0.12),
            mark: selection.opacity(0.55),
            mark_current: selection,
            mark_border: color(colors.accent),
            error: color(colors.error),
        }
    }
}

/// What drawing one list row needs; shared by every row of a draw.
struct Frame {
    view: WeakEntity<DiffView>,
    diff: FileDiff,
    rows: Rc<Vec<Row>>,
    highlights: Arc<Highlights>,
    find: Option<FrameFind>,
    palette: Palette,
    font: Font,
    cell: f32,
    line: f32,
    /// One line-number column.
    number_width: f32,
    /// Number columns before the marker of a row (two unified, one per split half).
    gutter: f32,
    eof: [Option<u32>; 2],
    /// The first row of each hunk with changes and the hunk's index, as `TextBody` holds.
    hunks: Rc<Vec<(usize, usize)>>,
    /// The button hunks show, and whether it acts (the whitespace mode is exact).
    hunk_button: Option<(HunkButton, bool)>,
    /// The hunk whose write is in flight.
    pending_hunk: Option<usize>,
    /// The hunks whose button shows: the one under the pointer and the current one.
    shown_hunks: [Option<usize>; 2],
    /// The current hunk, whose button takes Shift+S or Shift+U.
    current_hunk: Option<usize>,
}

struct FrameFind {
    matches: Rc<Vec<Match>>,
    by_line: Rc<Vec<(u8, u32, u32)>>,
    current: Option<usize>,
}

impl FrameFind {
    /// Matches on line `line` of `side`, with whether each is the current one.
    fn on_line(&self, side: Side, line: u32) -> impl Iterator<Item = (&Match, bool)> {
        let key = (side_key(side), line);
        let start = self.by_line.partition_point(|&(s, l, _)| (s, l) < key);
        self.by_line[start..]
            .iter()
            .take_while(move |&&(s, l, _)| (s, l) == key)
            .map(|&(_, _, ix)| {
                (
                    &self.matches[ix as usize],
                    self.current == Some(ix as usize),
                )
            })
    }
}

impl Frame {
    fn eof_at(&self, side: Side, line: u32) -> bool {
        self.eof[side_key(side) as usize] == Some(line)
    }
}

/// The hunk n (`forward`) or p moves to, from `hunks` (first row, index): next to the
/// current hunk's first row `current` while it is in view; else forward the first hunk at
/// or below the view's `top` row, back the last one above `reference` (a third of the way
/// down the view), so the hunks of a diff that fits the view are all reachable.
fn next_hunk(
    hunks: &[(usize, usize)],
    current: Option<usize>,
    top: usize,
    reference: usize,
    forward: bool,
) -> Option<(usize, usize)> {
    let mut starts = hunks.iter().copied();
    match (current, forward) {
        (Some(from), true) => starts.find(|&(start, _)| start > from),
        (Some(from), false) => starts.rev().find(|&(start, _)| start < from),
        (None, true) => starts.find(|&(start, _)| start >= top),
        (None, false) => starts.rev().find(|&(start, _)| start < reference),
    }
}

/// A list row, telling the view when the pointer enters or leaves it.
fn render_row(frame: &Frame, ix: usize) -> AnyElement {
    let view = frame.view.clone();
    div()
        .id(("diff-row", ix))
        .w_full()
        .on_hover(move |hovered, _: &mut Window, cx: &mut App| {
            view.update(cx, |this, cx| this.hover_row(ix, *hovered, cx))
                .ok();
        })
        .child(render_line_row(frame, ix, hunk_slot(frame, ix)))
        .into_any_element()
}

/// The Stage hunk / Unstage hunk button's place at the right end of a hunk's first row,
/// beside the text so it never covers it. The place is kept on every hunk's first row so
/// nothing reflows; the button shows only for the hunk under the pointer, the current hunk
/// and a hunk whose write is in flight. Under an ignore mode it is disabled and its
/// tooltip says why.
fn hunk_slot(frame: &Frame, ix: usize) -> Option<AnyElement> {
    let (button, enabled) = frame.hunk_button?;
    let at = frame
        .hunks
        .binary_search_by_key(&ix, |&(start, _)| start)
        .ok()?;
    let index = frame.hunks[at].1;
    let pending = frame.pending_hunk == Some(index);
    let shown = pending || frame.shown_hunks.contains(&Some(index));
    let view = frame.view.clone();
    let mut control = text_button(("hunk-button", index), button.label())
        .tab_stop(false)
        .loading(pending)
        .on_click(move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            view.update(cx, |this, cx| this.click_hunk(index, cx)).ok();
        });
    if frame.current_hunk == Some(index) {
        control = control.key(match button {
            HunkButton::Stage => "shift-s",
            HunkButton::Unstage => "shift-u",
        });
    }
    let control = if enabled {
        control.detail(button.detail())
    } else {
        control.tooltip("Needs exact whitespace").disabled_because(Some(
            "This diff ignores some whitespace, so the hunk would carry changes it does not show",
        ))
    };
    Some(
        div()
            .flex_none()
            .h(px(frame.line))
            .pl(px(frame.cell))
            .pr(px(frame.cell))
            .flex()
            .items_center()
            .when(!shown, |slot| slot.invisible())
            .child(control)
            .into_any_element(),
    )
}

/// Row `ix`; `slot` (a hunk's button place) goes at the right end of its new side.
fn render_line_row(frame: &Frame, ix: usize, slot: Option<AnyElement>) -> AnyElement {
    match &frame.rows[ix] {
        Row::Fold {
            gap,
            hidden,
            function,
            can_expand_up,
            can_expand_down,
        } => fold_row(
            frame,
            *gap,
            *hidden,
            function.as_deref(),
            *can_expand_up,
            *can_expand_down,
        ),
        Row::Unified { old, new, line } => {
            let eof = frame.eof_at(line.side, line.ix)
                || line.kind == LineKind::Context
                    && old.is_some_and(|old| frame.eof_at(Side::Old, old));
            line_shell(frame, line.kind)
                .w_full()
                .child(number(frame, *old))
                .child(number(frame, *new))
                .child(marker(frame, line.kind))
                .child(line_text(frame, line, None, eof))
                .children(slot)
                .into_any_element()
        }
        Row::Split { old, new } => {
            let mirror = match (old, new) {
                (Some(old), Some(new)) if old.kind == LineKind::Context => Some(new.ix),
                _ => None,
            };
            div()
                .flex()
                .w_full()
                .child(half(frame, old.as_ref(), mirror, None))
                .child(div().flex_none().w(px(1.0)).bg(frame.palette.border))
                .child(half(frame, new.as_ref(), None, slot))
                .into_any_element()
        }
    }
}

/// A line row: tinted with a bar at its left edge when added or removed.
fn line_shell(frame: &Frame, kind: LineKind) -> Div {
    let palette = &frame.palette;
    let (tint, bar) = match kind {
        LineKind::Added => (Some(palette.added_row), Some(palette.added)),
        LineKind::Removed => (Some(palette.removed_row), Some(palette.removed)),
        LineKind::Context | LineKind::Formatting => (None, None),
    };
    div()
        .relative()
        .flex()
        .min_w_0()
        .when_some(tint, |row, tint| row.bg(tint))
        .when_some(bar, |row, bar| {
            row.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(BAR_WIDTH))
                    .bg(bar),
            )
        })
}

/// One side of a split row, `slot` at its right end; `None` is the blank side of an
/// unpaired line.
fn half(
    frame: &Frame,
    line: Option<&DiffLine>,
    mirror: Option<u32>,
    slot: Option<AnyElement>,
) -> Div {
    let Some(line) = line else {
        return div()
            .flex_1()
            .min_w_0()
            .bg(frame.palette.shade)
            .flex()
            .justify_end()
            .children(slot);
    };
    line_shell(frame, line.kind)
        .flex_1()
        .child(number(frame, Some(line.ix)))
        .child(marker(frame, line.kind))
        .child(line_text(
            frame,
            line,
            mirror,
            frame.eof_at(line.side, line.ix),
        ))
        .children(slot)
}

fn number(frame: &Frame, line: Option<u32>) -> Div {
    div()
        .flex_none()
        .w(px(frame.number_width))
        .pr(px(frame.cell))
        .text_right()
        .text_color(frame.palette.number)
        .children(line.map(|line| (line + 1).to_string()))
}

fn marker(frame: &Frame, kind: LineKind) -> Div {
    let (sign, color) = match kind {
        LineKind::Added => ("+", frame.palette.added),
        LineKind::Removed => ("-", frame.palette.removed),
        LineKind::Context | LineKind::Formatting => ("", frame.palette.muted),
    };
    div()
        .flex_none()
        .w(px(frame.cell * 2.0))
        .text_color(color)
        .child(sign)
}

/// The text of a line with its syntax colors, changed words, find matches and indent guides.
/// `mirror` is the new line paired with an unchanged old line: its matches show on both sides.
fn line_text(frame: &Frame, line: &DiffLine, mirror: Option<u32>, eof: bool) -> Div {
    let palette = &frame.palette;
    let raw = frame.diff.line(line.side, line.ix);
    let text = Expanded::new(raw);
    let runs: Vec<TextRun> = if line.kind == LineKind::Formatting {
        vec![text_run(text.text.len(), &frame.font, None, palette.muted)]
    } else {
        let start = frame.diff.line_start(line.side, line.ix);
        line_styles(frame.highlights.side(line.side), start..start + raw.len())
            .into_iter()
            .map(|(range, style)| {
                let len = text.offset(range.end) - text.offset(range.start);
                text_run(len, &frame.font, style.as_ref(), palette.foreground)
            })
            .filter(|run| run.len > 0)
            .collect()
    };
    let mut marks = Vec::new();
    let word = match line.kind {
        LineKind::Added => Some(palette.added_word),
        LineKind::Removed => Some(palette.removed_word),
        LineKind::Context | LineKind::Formatting => None,
    };
    if let Some(word) = word {
        marks.extend(line.emphasis.iter().map(|range| Mark {
            range: text.range(range),
            color: word,
            border: None,
        }));
    }
    if let Some(find) = &frame.find {
        let (side, ix) = match mirror {
            Some(new) if frame.diff.new_line(new) == raw => (Side::New, new),
            Some(_) => (line.side, u32::MAX),
            None => (line.side, line.ix),
        };
        marks.extend(find.on_line(side, ix).map(|(hit, current)| Mark {
            range: text.range(&hit.range),
            color: if current {
                palette.mark_current
            } else {
                palette.mark
            },
            border: current.then_some(palette.mark_border),
        }));
    }
    let paint = LinePaint {
        marks,
        guides: line.guides,
        guide: palette.guide,
    };
    let styled = StyledText::new(SharedString::from(text.text.into_owned())).with_runs(runs);
    let layout = styled.layout().clone();
    let body = div()
        .relative()
        .min_h(px(frame.line))
        .when(!paint.marks.is_empty() || paint.guides.is_some(), |body| {
            body.child(
                canvas(
                    |_, _, _| (),
                    move |_, (), window, _| paint_line(&layout, &paint, window),
                )
                .absolute()
                .inset_0(),
            )
        })
        .child(styled);
    div()
        .flex_1()
        .min_w_0()
        .pr(px(frame.cell))
        .flex()
        .flex_col()
        .child(body)
        .when(eof, |text| {
            text.child(
                div()
                    .text_color(palette.muted)
                    .child("\\ No newline at end of file"),
            )
        })
}

fn text_run(len: usize, font: &Font, style: Option<&HighlightStyle>, foreground: Hsla) -> TextRun {
    let mut font = font.clone();
    let mut color = foreground;
    if let Some(style) = style {
        if let Some(weight) = style.font_weight {
            font.weight = weight;
        }
        if let Some(font_style) = style.font_style {
            font.style = font_style;
        }
        if let Some(style_color) = style.color {
            color = style_color;
        }
    }
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// A tinted range behind a line's text.
struct Mark {
    range: Range<usize>,
    color: Hsla,
    border: Option<Hsla>,
}

struct LinePaint {
    marks: Vec<Mark>,
    guides: Option<diff::IndentGuides>,
    guide: Hsla,
}

/// Paints behind the text laid out in `layout`: indent guides, then the marks, split where
/// the line wraps.
fn paint_line(layout: &TextLayout, paint: &LinePaint, window: &mut Window) {
    let lines = layout.line_layouts();
    let Some(line) = lines.first() else {
        return;
    };
    let origin = layout.bounds().origin;
    let height = layout.line_height();
    let shaped = &line.unwrapped_layout;
    if let Some(guides) = paint.guides {
        for level in 0..usize::from(guides.levels) {
            let x = origin.x + shaped.x_for_index(level * usize::from(guides.unit));
            window.paint_quad(fill(
                Bounds::new(point(x, origin.y), size(px(1.0), height)),
                paint.guide,
            ));
        }
    }
    if paint.marks.is_empty() {
        return;
    }
    let starts: Vec<usize> = std::iter::once(0)
        .chain(
            line.wrap_boundaries()
                .iter()
                .map(|boundary| shaped.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index),
        )
        .collect();
    for mark in &paint.marks {
        for (row, range) in segments(&starts, line.len(), mark.range.clone()) {
            let left = shaped.x_for_index(starts[row]);
            let from = shaped.x_for_index(range.start) - left;
            let to = shaped.x_for_index(range.end) - left;
            let bounds = Bounds::new(
                point(origin.x + from, origin.y + height * row as f32 + px(1.0)),
                size(to - from, height - px(2.0)),
            );
            window.paint_quad(quad(
                bounds,
                px(WORD_RADIUS),
                mark.color,
                px(if mark.border.is_some() { 1.0 } else { 0.0 }),
                mark.border.unwrap_or(mark.color),
                BorderStyle::default(),
            ));
        }
    }
}

/// `N unchanged lines` after an ellipsis and the enclosing function, with a hairline to the
/// right edge. A click reveals EXPAND_STEP lines toward the hunk below (or above, at the end
/// of the file); on hover chevrons pick the direction and Show all (or Alt/Shift+click)
/// reveals the whole gap.
fn fold_row(
    frame: &Frame,
    gap: usize,
    hidden: u32,
    function: Option<&str>,
    up: bool,
    down: bool,
) -> AnyElement {
    let palette = frame.palette;
    let group = SharedString::from(format!("diff-fold-{gap}"));
    let partial = hidden > EXPAND_STEP;
    let view = frame.view.clone();
    let click = move |direction: Expand| {
        let view = view.clone();
        move |event: &ClickEvent, _: &mut Window, cx: &mut App| {
            let modifiers = event.modifiers();
            let direction = if modifiers.alt || modifiers.shift {
                Expand::All
            } else {
                direction
            };
            cx.stop_propagation();
            view.update(cx, |this, cx| this.expand_fold(gap, direction, cx))
                .ok();
        }
    };
    // Hidden until the row is hovered; hovering a button hovers its row, so it stays.
    let on_hover = |control: controls::Control| {
        div()
            .flex_none()
            .opacity(0.0)
            .group_hover(group.clone(), |style| style.opacity(1.0))
            .child(control.tab_stop(false))
    };
    let mut arrows = Vec::new();
    if partial && down {
        arrows.push(on_hover(
            icon_button(
                ("fold-down", gap),
                IconName::ChevronDown,
                format!("Show {EXPAND_STEP} more lines below"),
            )
            .detail("After the hunk above. Alt+click shows all lines.")
            .on_click(click(Expand::Down)),
        ));
    }
    if partial && up {
        arrows.push(on_hover(
            icon_button(
                ("fold-up", gap),
                IconName::ChevronUp,
                format!("Show {EXPAND_STEP} more lines above"),
            )
            .detail("Before the hunk below. Alt+click shows all lines.")
            .on_click(click(Expand::Up)),
        ));
    }
    let default = match (partial, up) {
        (false, _) => Expand::All,
        (true, true) => Expand::Up,
        (true, false) => Expand::Down,
    };
    div()
        .id(("diff-fold", gap))
        .group(group.clone())
        .flex()
        .items_center()
        .w_full()
        .h(px(frame.line + 6.0))
        .text_color(palette.muted)
        .cursor_pointer()
        .hover(move |style| style.bg(palette.hover.opacity(0.5)))
        .on_click(click(default))
        .child(
            div()
                .flex_none()
                .min_w(px(frame.gutter))
                .pr(px(frame.cell * 0.5))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(2.0))
                .children(arrows),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .whitespace_nowrap()
                .child(controls::glyph(IconName::Ellipsis, palette.muted))
                .child(format!("{hidden} unchanged lines")),
        )
        .when_some(function, |row, function| {
            row.child(
                div()
                    .min_w_0()
                    .ml(px(frame.cell * 2.0))
                    .text_color(palette.dim)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(function.to_owned()),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w(px(frame.cell * 2.0))
                .h(px(1.0))
                .mx(px(frame.cell))
                .bg(palette.border),
        )
        .when(partial, |row| {
            row.child(
                on_hover(
                    text_button(("fold-all", gap), "Show all")
                        .ghost()
                        .detail(format!("Shows all {hidden} unchanged lines"))
                        .on_click(click(Expand::All)),
                )
                .mr(px(frame.cell)),
            )
        })
        .into_any_element()
}

fn message(text: SharedString, palette: &Palette) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .text_size(px(UI_SMALL_TEXT_SIZE))
        .text_color(palette.muted)
        .child(text)
        .into_any_element()
}

/// A subtle checkerboard over `bounds`, behind transparent image pixels.
fn paint_checker(bounds: Bounds<Pixels>, base: Hsla, square: Hsla, window: &mut Window) {
    window.paint_quad(fill(bounds, base));
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (columns, rows) = (
        (width / CHECKER).ceil() as usize,
        (height / CHECKER).ceil() as usize,
    );
    for row in 0..rows {
        for column in (row % 2..columns).step_by(2) {
            let (x, y) = (column as f32 * CHECKER, row as f32 * CHECKER);
            window.paint_quad(fill(
                Bounds::new(
                    bounds.origin + point(px(x), px(y)),
                    size(px(CHECKER.min(width - x)), px(CHECKER.min(height - y))),
                ),
                square,
            ));
        }
    }
}

impl DiffView {
    fn render_rows(
        &self,
        text: &TextBody,
        style: &DiffStyle,
        palette: Palette,
        cx: &App,
    ) -> AnyElement {
        let _ = cx;
        let typography = &style.typography;
        let cell = typography.cell_width;
        let number_width =
            (digits(text.diff.old_len().max(text.diff.new_len())) as f32 + 2.0) * cell;
        let frame = Rc::new(Frame {
            view: self.view.clone(),
            diff: text.diff.clone(),
            rows: text.rows.clone(),
            highlights: text.highlights.clone(),
            find: self.find.as_ref().map(|find| FrameFind {
                matches: find.matches.clone(),
                by_line: find.by_line.clone(),
                current: find.current,
            }),
            palette,
            font: typography.font.clone(),
            cell,
            line: typography.cell_height,
            number_width,
            gutter: match self.layout {
                Layout::Unified => number_width * 2.0,
                Layout::Split => number_width,
            },
            eof: eof_lines(&text.diff),
            hunks: text.hunks.clone(),
            hunk_button: self
                .hunk_button
                .map(|button| (button, self.settings.whitespace == Whitespace::Exact)),
            pending_hunk: self.pending_hunk,
            shown_hunks: [self.hunk_at_row(self.hovered_row), self.current_hunk],
            current_hunk: self.current_hunk,
        });
        div()
            .relative()
            .size_full()
            .font(typography.font.clone())
            .text_size(px(typography.font_size))
            .line_height(px(typography.cell_height))
            .text_color(palette.foreground)
            .child(
                list(self.list.clone(), move |ix, _, _| render_row(&frame, ix))
                    .size_full()
                    .py(px(4.0)),
            )
            .child(Scrollbar::vertical(&self.list))
            .into_any_element()
    }

    fn render_images(
        &self,
        old: &ImageSide,
        new: &ImageSide,
        palette: Palette,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let view = self.size.unwrap_or_else(|| size(px(800.0), px(600.0)));
        let stacked = self.layout == Layout::Unified;
        // Room for the padding, the gap between the panels and each caption.
        let caption = 28.0;
        let width = f32::from(view.width) - IMAGE_GAP * 2.0;
        let height = f32::from(view.height) - IMAGE_GAP * 2.0;
        let available = if stacked {
            (width, (height - IMAGE_GAP) / 2.0 - caption)
        } else {
            ((width - IMAGE_GAP) / 2.0, height - caption)
        };
        let available = (available.0.max(48.0), available.1.max(48.0));
        let before = image_panel("Before", "Added", old, available, palette, window, cx);
        let after = image_panel("After", "Deleted", new, available, palette, window, cx);
        div()
            .id("diff-images")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.images_scroll)
            .p(px(IMAGE_GAP))
            .text_size(px(UI_SMALL_TEXT_SIZE))
            .child(
                div()
                    .flex()
                    .when(stacked, |panels| panels.flex_col())
                    .gap(px(IMAGE_GAP))
                    .child(before)
                    .child(after),
            )
            .into_any_element()
    }

    fn render_find(&self, find: &Find, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let count = match find.current {
            _ if find.query.is_empty() => String::new(),
            Some(current) => format!("{}/{}", current + 1, find.matches.len()),
            None => "No results".to_owned(),
        };
        let step_blocked = if find.query.is_empty() {
            Some("Type text to find")
        } else if find.matches.is_empty() {
            Some("No matches")
        } else {
            None
        };
        div()
            .track_focus(&find.bar)
            .absolute()
            .top(px(6.0))
            .right(px(18.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .pl_2()
            .pr_1()
            .py(px(2.0))
            .rounded(px(controls::CONTROL_RADIUS))
            .border_1()
            .border_color(palette.border)
            .bg(palette.surface)
            .shadow_md()
            .text_size(px(UI_SMALL_TEXT_SIZE))
            // Also keeps a press on the bar itself from taking the focus from the field.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.find_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .w(px(FIND_FIELD_WIDTH))
                    .child(Input::new(&find.input).appearance(false)),
            )
            .child(
                div()
                    .flex_none()
                    .min_w(px(56.0))
                    .px_1()
                    .text_color(if find.current.is_none() && !find.query.is_empty() {
                        palette.error
                    } else {
                        palette.muted
                    })
                    .child(count),
            )
            .child(
                icon_button("diff-find-previous", IconName::ChevronUp, "Previous match")
                    .key("shift-enter")
                    .disabled_because(step_blocked)
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.step_match(false, cx)),
                    ),
            )
            .child(
                icon_button("diff-find-next", IconName::ChevronDown, "Next match")
                    .key("enter")
                    .disabled_because(step_blocked)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.step_match(true, cx))),
            )
            .child(
                icon_button("diff-find-close", IconName::X, "Close")
                    .key("escape")
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.close_find(window, cx)),
                    ),
            )
            .into_any_element()
    }
}

/// One side of an image compare: the image on a checkerboard, scaled down to fit
/// `available`, over a caption with its size; or a placeholder.
fn image_panel(
    label: &'static str,
    missing: &'static str,
    side: &ImageSide,
    available: (f32, f32),
    palette: Palette,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let placeholder = |text: &'static str| {
        let (width, height) = fit(240.0, 160.0, available);
        div()
            .w(px(width))
            .h(px(height))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .bg(palette.shade)
            .text_color(palette.muted)
            .child(text)
    };
    let (content, detail) = match side {
        ImageSide::Image { image, len, pixels } => {
            let pixels = pixels.or_else(|| {
                image.clone().use_render_image(window, cx).map(|render| {
                    let size = render.size(0);
                    (size.width.0.max(0) as u32, size.height.0.max(0) as u32)
                })
            });
            let (width, height) = pixels.map_or((160.0, 120.0), |(width, height)| {
                fit(width as f32, height as f32, available)
            });
            let (base, square) = (palette.surface, palette.foreground.opacity(0.06));
            let content = div()
                .relative()
                .flex_none()
                .w(px(width))
                .h(px(height))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, (), window, _| paint_checker(bounds, base, square, window),
                    )
                    .absolute()
                    .inset_0(),
                )
                .child(
                    img(ImageSource::Image(image.clone()))
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain)
                        .with_fallback(move || {
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(palette.muted)
                                .child("Cannot show image")
                                .into_any_element()
                        }),
                );
            let detail = match pixels {
                Some((width, height)) => format!("{width} × {height} · {}", human_size(*len)),
                None => human_size(*len),
            };
            (content, detail)
        }
        ImageSide::Missing => (placeholder(missing), String::new()),
        ImageSide::Unreadable { len } => (placeholder("Cannot read image"), human_size(*len)),
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(content)
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .text_color(palette.foreground)
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                )
                .child(div().text_color(palette.muted).child(detail)),
        )
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().relative().size_full().overflow_hidden();
        let Some(style) = self.style.clone() else {
            return root;
        };
        self.check_highlight_theme(cx);
        let palette = Palette::new(&style.colors);
        let view = self.view.clone();
        let known = self.size;
        let measure = canvas(
            move |bounds, _, cx| {
                if known != Some(bounds.size) {
                    cx.defer(move |cx| {
                        view.update(cx, |this, cx| this.resized(bounds.size, cx))
                            .ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let body = match &self.body {
            Body::Loading => div().into_any_element(),
            Body::Message(text) => message(text.clone(), &palette),
            Body::Images { old, new } => self.render_images(old, new, palette, window, cx),
            Body::Text(text) if text.rows.is_empty() => {
                message(text.note.unwrap_or("No changes").into(), &palette)
            }
            Body::Text(text) => self.render_rows(text, &style, palette, cx),
        };
        let find = self
            .find
            .as_ref()
            .map(|find| self.render_find(find, palette, cx));
        root.child(measure).child(body).children(find)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(color: u32) -> HighlightStyle {
        HighlightStyle {
            color: Some(super::color(color)),
            ..HighlightStyle::default()
        }
    }

    #[test]
    fn auto_layout_splits_only_past_112_cells() {
        assert_eq!(
            decide_layout(LayoutMode::Auto, None, Some(8.0)),
            Layout::Unified
        );
        assert_eq!(
            decide_layout(LayoutMode::Auto, Some(896.0), Some(8.0)),
            Layout::Unified
        );
        assert_eq!(
            decide_layout(LayoutMode::Auto, Some(897.0), Some(8.0)),
            Layout::Split
        );
        assert_eq!(
            decide_layout(LayoutMode::Auto, Some(2000.0), None),
            Layout::Unified
        );
        assert_eq!(
            decide_layout(LayoutMode::Split, Some(10.0), Some(8.0)),
            Layout::Split
        );
        assert_eq!(
            decide_layout(LayoutMode::Unified, Some(5000.0), Some(8.0)),
            Layout::Unified
        );
    }

    #[test]
    fn hunk_steps_follow_the_current_hunk_else_the_view() {
        let hunks = [(2, 0), (10, 1), (40, 2)];
        // Nothing current, the whole diff in view: n starts at the first hunk.
        assert_eq!(next_hunk(&hunks, None, 0, 12, true), Some((2, 0)));
        assert_eq!(next_hunk(&hunks, Some(2), 0, 12, true), Some((10, 1)));
        assert_eq!(next_hunk(&hunks, Some(10), 0, 12, true), Some((40, 2)));
        assert_eq!(next_hunk(&hunks, Some(40), 30, 42, true), None);
        assert_eq!(next_hunk(&hunks, Some(40), 30, 42, false), Some((10, 1)));
        assert_eq!(next_hunk(&hunks, Some(2), 0, 12, false), None);
        // Scrolled away from the current hunk: from the view.
        assert_eq!(next_hunk(&hunks, None, 5, 17, true), Some((10, 1)));
        assert_eq!(next_hunk(&hunks, None, 5, 17, false), Some((10, 1)));
    }

    #[test]
    fn hidden_lines_map_to_their_fold() {
        let old: String = (0..30).map(|ix| format!("line {ix}\n")).collect();
        let new = old.replace("line 15\n", "changed\n");
        let diff = FileDiff::new(&old, &new, Whitespace::Exact);
        for layout in [Layout::Unified, Layout::Split] {
            let rows = diff::rows(&diff, &diff::Context::default(), layout);
            let map = line_rows(&rows, Side::New, diff.new_len());
            assert!(matches!(rows[map[0] as usize], Row::Fold { gap: 0, .. }));
            assert!(matches!(rows[map[11] as usize], Row::Fold { gap: 0, .. }));
            assert_eq!(row_line(&rows[map[12] as usize], Side::New), Some(12));
            assert_eq!(row_line(&rows[map[15] as usize], Side::New), Some(15));
            assert!(matches!(rows[map[19] as usize], Row::Fold { gap: 1, .. }));
            assert!(matches!(rows[map[29] as usize], Row::Fold { gap: 1, .. }));
            let old_map = line_rows(&rows, Side::Old, diff.old_len());
            assert_eq!(row_line(&rows[old_map[15] as usize], Side::Old), Some(15));
        }
    }

    #[test]
    fn tabs_expand_to_the_next_stop_and_offsets_follow() {
        let text = Expanded::new("\tab\tc");
        assert_eq!(text.text, "    ab  c");
        assert_eq!(text.offset(0), 0);
        assert_eq!(text.offset(1), 4);
        assert_eq!(text.offset(3), 6);
        assert_eq!(text.offset(4), 8);
        assert_eq!(text.offset(5), 9);
        assert_eq!(text.range(&(1..3)), 4..6);
        let plain = Expanded::new("no tabs");
        assert_eq!(plain.offset(3), 3);
    }

    #[test]
    fn line_styles_clip_to_the_line_and_fill_gaps() {
        let (a, b) = (style(0x112233), style(0x445566));
        let styles = vec![(0..4, a), (6..12, b), (14..20, a)];
        assert_eq!(
            line_styles(&styles, 2..16),
            vec![
                (0..2, Some(a)),
                (2..4, None),
                (4..10, Some(b)),
                (10..12, None),
                (12..14, Some(a)),
            ]
        );
        assert_eq!(line_styles(&styles, 20..24), vec![(0..4, None)]);
        assert_eq!(line_styles(&[], 0..0), vec![]);
    }

    #[test]
    fn marks_split_where_the_line_wraps() {
        assert_eq!(segments(&[0], 10, 2..5), vec![(0, 2..5)]);
        assert_eq!(
            segments(&[0, 8, 16], 20, 6..18),
            vec![(0, 6..8), (1, 8..16), (2, 16..18)]
        );
        assert_eq!(segments(&[0, 8], 12, 8..8), vec![]);
    }

    #[test]
    fn images_shrink_to_fit_but_never_grow() {
        assert_eq!(fit(100.0, 50.0, (400.0, 400.0)), (100.0, 50.0));
        assert_eq!(fit(800.0, 400.0, (400.0, 400.0)), (400.0, 200.0));
        assert_eq!(fit(400.0, 800.0, (400.0, 200.0)), (100.0, 200.0));
        assert_eq!(fit(0.0, 10.0, (400.0, 400.0)), (0.0, 0.0));
    }

    #[test]
    fn empty_diffs_say_what_changed() {
        let note = |old: &str, new: &str, whitespace| {
            empty_note(&FileDiff::new(old, new, whitespace), old, new, whitespace)
        };
        assert_eq!(note("a\nb\n", "a\nb\n", Whitespace::Exact), "No changes");
        assert_eq!(
            note("a\r\nb\r\n", "a\nb\n", Whitespace::Exact),
            "Only line endings changed"
        );
        assert_eq!(
            note("a\nb", "a\nb\n", Whitespace::Exact),
            "Only the newline at end of file changed"
        );
        assert_eq!(
            note("a  b\n", "a b\n", Whitespace::IgnoreSpace),
            "Only whitespace changed"
        );
        assert_eq!(
            note("f(a,b)\n", "f(a, b)\n", Whitespace::IgnoreFormatting),
            "Only formatting changed"
        );
    }

    #[test]
    fn unreadable_pairs_are_described() {
        let binary = |len| GitFileContent::Binary { len };
        let text = GitFileContent::Text {
            text: "x".repeat(12 * 1024),
        };
        assert_eq!(
            describe_change(&binary(12 * 1024), &binary(14 * 1024)),
            "Binary file · 12 KB → 14 KB"
        );
        assert_eq!(
            describe_change(&binary(500), &binary(500)),
            "Binary file · 500 B"
        );
        assert_eq!(
            describe_change(&GitFileContent::Missing, &binary(12 * 1024)),
            "Added binary file · 12 KB"
        );
        assert_eq!(
            describe_change(&binary(12 * 1024), &GitFileContent::Missing),
            "Deleted binary file · 12 KB"
        );
        assert_eq!(
            describe_change(&text, &binary(14 * 1024)),
            "Changed from text to binary · 12 KB → 14 KB"
        );
    }

    #[test]
    fn missing_final_newline_marks_the_last_line() {
        let diff = FileDiff::new("a\nb\n", "a\nc", Whitespace::Exact);
        assert_eq!(eof_lines(&diff), [None, Some(1)]);
        let same = FileDiff::new("a", "b", Whitespace::Exact);
        assert_eq!(eof_lines(&same), [None, None]);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(0), 1);
    }
}
