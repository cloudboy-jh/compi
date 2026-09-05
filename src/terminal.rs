use base64::Engine;
use flate2::read::ZlibDecoder;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::collections::{BTreeMap, HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::io::Read;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vte::{Params, Parser, Perform};

pub const MAX_SCROLLBACK_BYTES: usize = 1024 * 1024;
pub const MAX_GRAPHICS_BYTES: usize = 4 * 1024 * 1024;
const MAX_APC_BYTES: usize = 8 * 1024 * 1024;
const MAX_OSC8_URI_BYTES: usize = 2048;
const MAX_UNSUPPORTED_SIGNATURES: usize = 128;
const MAX_OSC52_DECODED_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TextAttributes {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strike: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Cell {
    pub text: SmolStr,
    pub width: u8,
    pub foreground: Color,
    pub background: Color,
    pub attributes: TextAttributes,
    pub hyperlink: Option<SmolStr>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            width: 1,
            foreground: Color::Default,
            background: Color::Default,
            attributes: TextAttributes::default(),
            hyperlink: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub wrapped: bool,
}

impl Row {
    fn blank(cols: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols],
            wrapped: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub shape: CursorShape,
    pub blinking: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MouseMode {
    #[default]
    None,
    Normal,
    ButtonMotion,
    AnyMotion,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TerminalModes {
    pub alternate_screen: bool,
    pub bracketed_paste: bool,
    pub origin: bool,
    pub auto_wrap: bool,
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub mouse: MouseMode,
    pub sgr_mouse: bool,
    pub focus_events: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KittyImage {
    pub id: u32,
    pub format: u16,
    pub width: u32,
    pub height: u32,
    pub data: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct KittyPlacement {
    pub image_id: u32,
    pub placement_id: Option<u32>,
    pub row: i32,
    pub col: u16,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
    pub z_index: i32,
    pub alternate_screen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenSnapshot {
    pub sequence: u64,
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Row>,
    pub scrollback: Vec<Row>,
    pub cursor: CursorState,
    pub modes: TerminalModes,
    pub title: String,
    pub current_directory: Option<String>,
    pub images: Vec<KittyImage>,
    pub placements: Vec<KittyPlacement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RowUpdate {
    pub index: u16,
    pub row: Row,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenDelta {
    pub sequence: u64,
    pub cols: u16,
    pub rows: u16,
    pub row_updates: Vec<RowUpdate>,
    pub scrollback: Option<Vec<Row>>,
    pub cursor: CursorState,
    pub modes: TerminalModes,
    pub title: String,
    pub current_directory: Option<String>,
    pub images: Option<Vec<KittyImage>>,
    pub latency_ids: Vec<u64>,
    pub placements: Option<Vec<KittyPlacement>>,
    pub clipboard_writes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScreenMessage {
    Snapshot { snapshot: ScreenSnapshot },
    Delta { delta: ScreenDelta },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorApply {
    Applied,
    Gap { expected: u64, actual: u64 },
}

#[derive(Default)]
pub struct ScreenMirror {
    snapshot: Option<ScreenSnapshot>,
}

impl ScreenMirror {
    pub fn snapshot(&self) -> Option<&ScreenSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn apply(&mut self, message: ScreenMessage) -> MirrorApply {
        match message {
            ScreenMessage::Snapshot { snapshot } => {
                self.snapshot = Some(snapshot);
                MirrorApply::Applied
            }
            ScreenMessage::Delta { delta } => {
                let Some(snapshot) = self.snapshot.as_mut() else {
                    return MirrorApply::Gap {
                        expected: 0,
                        actual: delta.sequence,
                    };
                };
                let expected = snapshot.sequence.saturating_add(1);
                if delta.sequence != expected {
                    return MirrorApply::Gap {
                        expected,
                        actual: delta.sequence,
                    };
                }
                snapshot.sequence = delta.sequence;
                snapshot.cols = delta.cols;
                snapshot.rows = delta.rows;
                if snapshot.cells.len() != usize::from(delta.rows) {
                    snapshot.cells =
                        vec![Row::blank(usize::from(delta.cols)); usize::from(delta.rows)];
                }
                for update in delta.row_updates {
                    if let Some(row) = snapshot.cells.get_mut(usize::from(update.index)) {
                        *row = update.row;
                    }
                }
                if let Some(scrollback) = delta.scrollback {
                    snapshot.scrollback = scrollback;
                }
                snapshot.cursor = delta.cursor;
                snapshot.modes = delta.modes;
                snapshot.title = delta.title;
                snapshot.current_directory = delta.current_directory;
                if let Some(images) = delta.images {
                    snapshot.images = images;
                }
                if let Some(placements) = delta.placements {
                    snapshot.placements = placements;
                }
                MirrorApply::Applied
            }
        }
    }
}

struct Buffer {
    rows: Vec<Row>,
    scrollback: VecDeque<Row>,
    scrollback_bytes: usize,
}

impl Buffer {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            rows: vec![Row::blank(cols); rows],
            scrollback: VecDeque::new(),
            scrollback_bytes: 0,
        }
    }

    fn push_scrollback(&mut self, row: Row) {
        self.scrollback_bytes += row_memory(&row);
        self.scrollback.push_back(row);
        while self.scrollback_bytes > MAX_SCROLLBACK_BYTES {
            let Some(removed) = self.scrollback.pop_front() else {
                break;
            };
            self.scrollback_bytes = self.scrollback_bytes.saturating_sub(row_memory(&removed));
        }
    }
}

#[derive(Default)]
struct Rendition {
    foreground: Color,
    background: Color,
    attributes: TextAttributes,
}

#[derive(Default)]
struct KittyTransfer {
    format: u16,
    width: u32,
    height: u32,
    compressed: bool,
    bytes: Vec<u8>,
    placement: Option<KittyPlacement>,
}

enum InputState {
    Normal,
    Escape,

    Apc(Vec<u8>),
    ApcEscape(Vec<u8>),
    ApcDiscard,
    ApcDiscardEscape,
}
#[derive(Default)]
struct UnsupportedDiagnostics {
    context: String,
    counts: BTreeMap<String, u64>,
}

impl UnsupportedDiagnostics {
    fn record(&mut self, signature: impl Into<String>) {
        let mut signature = signature.into();
        if !self.counts.contains_key(&signature) && self.counts.len() >= MAX_UNSUPPORTED_SIGNATURES
        {
            signature = "other".to_owned();
        }
        let count = self.counts.entry(signature.clone()).or_default();
        *count = count.saturating_add(1);
        if *count == 1 || count.is_power_of_two() {
            eprintln!(
                "compi-daemon: unsupported terminal sequence context={} signature={} count={}",
                self.context, signature, count
            );
        }
    }
}

impl Drop for UnsupportedDiagnostics {
    fn drop(&mut self) {
        if self.counts.is_empty() {
            return;
        }
        let summary = self
            .counts
            .iter()
            .map(|(signature, count)| format!("{signature}={count}"))
            .collect::<Vec<_>>()
            .join(",");
        eprintln!(
            "compi-daemon: unsupported terminal summary context={} counts={summary}",
            self.context
        );
    }
}
struct ChangeBaseline {
    cols: usize,
    rows: usize,
    active_alternate: bool,
    row_hashes: Vec<u64>,
    scrollback_generation: u64,
    graphics_generation: u64,
    cursor: CursorState,
    modes: TerminalModes,
    title: String,
    current_directory: Option<String>,
}

pub struct TerminalState {
    parser: Parser,
    input_state: InputState,
    main: Buffer,
    alternate: Buffer,
    active_alternate: bool,
    cursor: CursorState,
    saved_cursor: CursorState,
    rendition: Rendition,
    active_hyperlink: Option<SmolStr>,
    title: String,
    current_directory: Option<String>,
    modes: TerminalModes,
    scroll_top: usize,
    scroll_bottom: usize,
    pending_wrap: bool,
    sequence: u64,
    scrollback_generation: u64,
    graphics_generation: u64,
    replies: Vec<Vec<u8>>,
    clipboard_writes: Vec<String>,
    images: HashMap<u32, KittyImage>,
    placements: Vec<KittyPlacement>,
    transfers: HashMap<u32, KittyTransfer>,
    next_image_id: u32,
    active_transfer: Option<u32>,
    diagnostics: UnsupportedDiagnostics,
}

impl TerminalState {
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = usize::from(cols.max(1));
        let rows = usize::from(rows.max(1));
        Self {
            parser: Parser::new(),
            input_state: InputState::Normal,
            main: Buffer::new(cols, rows),
            alternate: Buffer::new(cols, rows),
            active_alternate: false,
            cursor: CursorState {
                visible: true,
                ..CursorState::default()
            },
            saved_cursor: CursorState::default(),
            rendition: Rendition::default(),
            active_hyperlink: None,
            title: String::new(),
            current_directory: None,
            modes: TerminalModes {
                auto_wrap: true,
                ..TerminalModes::default()
            },
            scroll_top: 0,
            scroll_bottom: rows,
            pending_wrap: false,
            sequence: 0,
            replies: Vec::new(),
            clipboard_writes: Vec::new(),
            images: HashMap::new(),
            scrollback_generation: 0,
            graphics_generation: 0,
            placements: Vec::new(),
            transfers: HashMap::new(),
            next_image_id: 1,
            active_transfer: None,
            diagnostics: UnsupportedDiagnostics::default(),
        }
    }
    pub fn set_diagnostic_context(&mut self, session_id: &str, trace_label: Option<&str>) {
        self.diagnostics.context = match trace_label {
            Some(label) => format!("session={session_id},trace={label}"),
            None => format!("session={session_id}"),
        };
    }

    pub fn snapshot(&self) -> ScreenSnapshot {
        let mut images: Vec<_> = self.images.values().cloned().collect();
        images.sort_by_key(|image| image.id);
        let mut placements = self.placements.clone();
        placements.sort_by_key(|placement| {
            (
                placement.z_index,
                placement.image_id,
                placement.placement_id.unwrap_or(0),
            )
        });
        ScreenSnapshot {
            sequence: self.sequence,
            cols: self.cols() as u16,
            rows: self.rows() as u16,
            cells: self.buffer().rows.clone(),
            scrollback: self.main.scrollback.iter().cloned().collect(),
            cursor: self.cursor,
            modes: self.modes.clone(),
            title: self.title.clone(),
            current_directory: self.current_directory.clone(),
            images,
            placements,
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) -> (Option<ScreenDelta>, Vec<Vec<u8>>) {
        let before = self.change_baseline();
        for &byte in bytes {
            self.advance_byte(byte);
        }
        self.finish_change(before)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Option<ScreenDelta> {
        let cols = usize::from(cols.max(1));
        let rows = usize::from(rows.max(1));
        if cols == self.cols() && rows == self.rows() {
            return None;
        }
        let before = self.change_baseline();
        let cursor_in_main = (!self.active_alternate).then(|| {
            (
                self.main.scrollback.len() + usize::from(self.cursor.row),
                usize::from(self.cursor.col),
            )
        });
        let reflowed_cursor = reflow_main_buffer(&mut self.main, cols, rows, cursor_in_main);
        resize_buffer_cells(&mut self.alternate, cols, rows);
        self.scrollback_generation = self.scrollback_generation.saturating_add(1);
        if let Some((row, col)) = reflowed_cursor {
            self.cursor.row = row.min(rows.saturating_sub(1)) as u16;
            self.cursor.col = col.min(cols.saturating_sub(1)) as u16;
        } else {
            self.cursor.col = self.cursor.col.min(cols.saturating_sub(1) as u16);
            self.cursor.row = self.cursor.row.min(rows.saturating_sub(1) as u16);
        }
        self.saved_cursor.col = self.saved_cursor.col.min(cols.saturating_sub(1) as u16);
        self.saved_cursor.row = self.saved_cursor.row.min(rows.saturating_sub(1) as u16);
        self.scroll_top = 0;
        self.scroll_bottom = rows;
        self.pending_wrap = false;
        self.finish_change(before).0
    }

    fn change_baseline(&self) -> ChangeBaseline {
        ChangeBaseline {
            cols: self.cols(),
            rows: self.rows(),
            active_alternate: self.active_alternate,
            row_hashes: self.buffer().rows.iter().map(row_hash).collect(),
            scrollback_generation: self.scrollback_generation,
            graphics_generation: self.graphics_generation,
            cursor: self.cursor,
            modes: self.modes.clone(),
            title: self.title.clone(),
            current_directory: self.current_directory.clone(),
        }
    }

    fn finish_change(&mut self, before: ChangeBaseline) -> (Option<ScreenDelta>, Vec<Vec<u8>>) {
        let replies = std::mem::take(&mut self.replies);
        let clipboard_writes = std::mem::take(&mut self.clipboard_writes);
        let row_hashes: Vec<_> = self.buffer().rows.iter().map(row_hash).collect();
        let all_rows = before.cols != self.cols()
            || before.rows != self.rows()
            || before.active_alternate != self.active_alternate;
        let scrollback_changed = before.scrollback_generation != self.scrollback_generation;
        let graphics_changed = before.graphics_generation != self.graphics_generation;
        let changed = all_rows
            || before.row_hashes != row_hashes
            || scrollback_changed
            || graphics_changed
            || before.cursor != self.cursor
            || before.modes != self.modes
            || before.title != self.title
            || before.current_directory != self.current_directory
            || !clipboard_writes.is_empty();
        if !changed {
            return (None, replies);
        }
        self.sequence = self.sequence.saturating_add(1);
        let row_updates = self
            .buffer()
            .rows
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                all_rows || before.row_hashes.get(*index) != row_hashes.get(*index)
            })
            .map(|(index, row)| RowUpdate {
                index: index as u16,
                row: row.clone(),
            })
            .collect();
        let (images, placements) = if graphics_changed {
            let snapshot = self.snapshot();
            (Some(snapshot.images), Some(snapshot.placements))
        } else {
            (None, None)
        };
        let delta = ScreenDelta {
            sequence: self.sequence,
            cols: self.cols() as u16,
            rows: self.rows() as u16,
            row_updates,
            scrollback: scrollback_changed.then(|| self.main.scrollback.iter().cloned().collect()),
            cursor: self.cursor,
            modes: self.modes.clone(),
            title: self.title.clone(),
            current_directory: self.current_directory.clone(),
            images,
            latency_ids: Vec::new(),
            placements,
            clipboard_writes,
        };
        (Some(delta), replies)
    }

    fn advance_byte(&mut self, byte: u8) {
        let state = std::mem::replace(&mut self.input_state, InputState::Normal);
        match state {
            InputState::Normal if byte == 0x1b => self.input_state = InputState::Escape,
            InputState::Normal => self.feed_vte(&[byte]),
            InputState::Escape if byte == b'_' => self.input_state = InputState::Apc(Vec::new()),
            InputState::Escape => self.feed_vte(&[0x1b, byte]),
            InputState::Apc(payload) if byte == 0x1b => {
                self.input_state = InputState::ApcEscape(payload)
            }
            InputState::Apc(payload) if byte == 0x9c => self.dispatch_apc(&payload),
            InputState::Apc(mut payload) if payload.len() < MAX_APC_BYTES => {
                payload.push(byte);
                self.input_state = InputState::Apc(payload);
            }
            InputState::Apc(_) => {
                self.diagnostics.record("APC:oversized");
                self.input_state = InputState::ApcDiscard;
            }
            InputState::ApcEscape(payload) if byte == b'\\' => self.dispatch_apc(&payload),
            InputState::ApcEscape(mut payload) if payload.len() + 2 <= MAX_APC_BYTES => {
                payload.push(0x1b);
                payload.push(byte);
                self.input_state = InputState::Apc(payload);
            }
            InputState::ApcEscape(_) => {
                self.diagnostics.record("APC:oversized");
                self.input_state = InputState::ApcDiscard;
            }
            InputState::ApcDiscard if byte == 0x1b => {
                self.input_state = InputState::ApcDiscardEscape
            }
            InputState::ApcDiscard if byte == 0x9c => {}
            InputState::ApcDiscard => self.input_state = InputState::ApcDiscard,
            InputState::ApcDiscardEscape if byte == b'\\' => {}
            InputState::ApcDiscardEscape => self.input_state = InputState::ApcDiscard,
        }
    }

    fn feed_vte(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        parser.advance(self, bytes);
        self.parser = parser;
    }

    fn cols(&self) -> usize {
        self.main.rows.first().map_or(1, |row| row.cells.len())
    }

    fn rows(&self) -> usize {
        self.main.rows.len()
    }

    fn buffer(&self) -> &Buffer {
        if self.active_alternate {
            &self.alternate
        } else {
            &self.main
        }
    }

    fn buffer_mut(&mut self) -> &mut Buffer {
        if self.active_alternate {
            &mut self.alternate
        } else {
            &mut self.main
        }
    }

    fn blank_cell(&self) -> Cell {
        Cell {
            foreground: self.rendition.foreground,
            background: self.rendition.background,
            attributes: self.rendition.attributes.clone(),
            ..Cell::default()
        }
    }

    fn print_char(&mut self, character: char) {
        let width = UnicodeWidthChar::width(character).unwrap_or(0).min(2);
        if width == 0 {
            self.append_combining(character);
            return;
        }
        if self.append_grapheme_extension(character) {
            return;
        }
        let cols = self.cols();
        if self.pending_wrap && self.modes.auto_wrap {
            self.cursor.col = 0;
            self.linefeed(true);
        }
        if width == 2 && usize::from(self.cursor.col) + 1 >= cols {
            if self.modes.auto_wrap {
                self.cursor.col = 0;
                self.linefeed(true);
            } else {
                return;
            }
        }
        let row = usize::from(self.cursor.row).min(self.rows() - 1);
        let col = usize::from(self.cursor.col).min(cols - 1);
        let cell = Cell {
            text: SmolStr::new(character.to_string()),
            width: width as u8,
            foreground: self.rendition.foreground,
            background: self.rendition.background,
            attributes: self.rendition.attributes.clone(),
            hyperlink: self.active_hyperlink.clone(),
        };
        self.buffer_mut().rows[row].cells[col] = cell;
        if width == 2 {
            let mut continuation = self.blank_cell();
            continuation.hyperlink = self.active_hyperlink.clone();
            continuation.text = SmolStr::new_static("");
            continuation.width = 0;
            self.buffer_mut().rows[row].cells[col + 1] = continuation;
        }
        let next = col + width;
        if next >= cols {
            self.cursor.col = (cols - 1) as u16;
            self.pending_wrap = true;
        } else {
            self.cursor.col = next as u16;
            self.pending_wrap = false;
        }
    }

    fn previous_base_cell(&self) -> Option<(usize, usize)> {
        let row = usize::from(self.cursor.row).min(self.rows() - 1);
        let mut col = if self.pending_wrap {
            usize::from(self.cursor.col)
        } else {
            usize::from(self.cursor.col).checked_sub(1)?
        };
        while col > 0 && self.buffer().rows[row].cells[col].width == 0 {
            col -= 1;
        }
        (self.buffer().rows[row].cells[col].width > 0).then_some((row, col))
    }

    fn append_grapheme_extension(&mut self, character: char) -> bool {
        let Some((row, col)) = self.previous_base_cell() else {
            return false;
        };
        let mut text = self.buffer().rows[row].cells[col].text.to_string();
        text.push(character);
        if text.graphemes(true).count() != 1 {
            return false;
        }
        let new_width = UnicodeWidthStr::width(text.as_str()).clamp(1, 2);
        let old_width = usize::from(self.buffer().rows[row].cells[col].width);
        if new_width > old_width && col + new_width <= self.cols() {
            let mut continuation = self.buffer().rows[row].cells[col].clone();
            continuation.text = SmolStr::new_static("");
            continuation.width = 0;
            self.buffer_mut().rows[row].cells[col + 1] = continuation;
            let next = col + new_width;
            if next >= self.cols() {
                self.cursor.col = (self.cols() - 1) as u16;
                self.pending_wrap = true;
            } else {
                self.cursor.col = next as u16;
            }
        }
        let cell = &mut self.buffer_mut().rows[row].cells[col];
        cell.text = text.into();
        cell.width = new_width as u8;
        true
    }

    fn append_combining(&mut self, character: char) {
        let (row, col) = self.previous_base_cell().unwrap_or_else(|| {
            (
                usize::from(self.cursor.row).min(self.rows() - 1),
                usize::from(self.cursor.col).min(self.cols() - 1),
            )
        });
        let cell = &mut self.buffer_mut().rows[row].cells[col];
        let mut text = cell.text.to_string();
        text.push(character);
        cell.text = text.into();
    }

    fn linefeed(&mut self, wrapped: bool) {
        let row = usize::from(self.cursor.row);
        if let Some(current) = self.buffer_mut().rows.get_mut(row) {
            current.wrapped = wrapped;
        }
        if row + 1 >= self.scroll_bottom {
            self.scroll_up(1);
        } else {
            self.cursor.row = (row + 1).min(self.rows() - 1) as u16;
        }
        self.pending_wrap = false;
    }

    fn reverse_index(&mut self) {
        let row = usize::from(self.cursor.row);
        if row <= self.scroll_top {
            let cols = self.cols();
            let top = self.scroll_top;
            let bottom = self.scroll_bottom;
            let buffer = self.buffer_mut();
            buffer.rows.insert(top, Row::blank(cols));
            buffer.rows.remove(bottom);
        } else {
            self.cursor.row -= 1;
        }
        self.pending_wrap = false;
    }

    fn scroll_up(&mut self, count: usize) {
        let cols = self.cols();
        let top = self.scroll_top;
        let bottom = self.scroll_bottom;
        let active_alt = self.active_alternate;
        for _ in 0..count.min(bottom.saturating_sub(top)) {
            let removed = {
                let buffer = self.buffer_mut();
                let removed = buffer.rows.remove(top);
                buffer.rows.insert(bottom - 1, Row::blank(cols));
                removed
            };
            if !active_alt && top == 0 {
                self.main.push_scrollback(removed);
                self.scrollback_generation = self.scrollback_generation.saturating_add(1);
            }
            let mut moved_placement = false;
            for placement in &mut self.placements {
                if placement.alternate_screen == active_alt
                    && placement.row >= top as i32
                    && placement.row < bottom as i32
                {
                    placement.row -= 1;
                    moved_placement = true;
                }
            }
            if moved_placement {
                self.graphics_generation = self.graphics_generation.saturating_add(1);
            }
        }
        let min_row = -(self.main.scrollback.len() as i32);
        self.placements
            .retain(|placement| placement.alternate_screen || placement.row >= min_row);
    }

    fn scroll_down(&mut self, count: usize) {
        let cols = self.cols();
        let top = self.scroll_top;
        let bottom = self.scroll_bottom;
        for _ in 0..count.min(bottom.saturating_sub(top)) {
            let buffer = self.buffer_mut();
            buffer.rows.remove(bottom - 1);
            buffer.rows.insert(top, Row::blank(cols));
        }
    }

    fn clear_row_range(&mut self, row: usize, start: usize, end: usize) {
        let blank = self.blank_cell();
        if let Some(row) = self.buffer_mut().rows.get_mut(row) {
            let end = end.min(row.cells.len());
            for cell in &mut row.cells[start.min(end)..end] {
                *cell = blank.clone();
            }
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let rows = self.rows();
        let cols = self.cols();
        match mode {
            0 => {
                self.clear_row_range(row, col, cols);
                for index in row + 1..rows {
                    self.clear_row_range(index, 0, cols);
                }
            }
            1 => {
                for index in 0..row {
                    self.clear_row_range(index, 0, cols);
                }
                self.clear_row_range(row, 0, col + 1);
            }
            2 | 3 => {
                for index in 0..rows {
                    self.clear_row_range(index, 0, cols);
                }
                if mode == 3 {
                    self.main.scrollback.clear();
                    self.main.scrollback_bytes = 0;
                    self.scrollback_generation = self.scrollback_generation.saturating_add(1);
                }
            }
            _ => self.unsupported("erase display", mode),
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let cols = self.cols();
        match mode {
            0 => self.clear_row_range(row, col, cols),
            1 => self.clear_row_range(row, 0, col + 1),
            2 => self.clear_row_range(row, 0, cols),
            _ => self.unsupported("erase line", mode),
        }
    }

    fn insert_chars(&mut self, count: usize) {
        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let cols = self.cols();
        let blank = self.blank_cell();
        let cells = &mut self.buffer_mut().rows[row].cells;
        for _ in 0..count.min(cols - col) {
            cells.insert(col, blank.clone());
            cells.pop();
        }
    }

    fn delete_chars(&mut self, count: usize) {
        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let cols = self.cols();
        let blank = self.blank_cell();
        let cells = &mut self.buffer_mut().rows[row].cells;
        for _ in 0..count.min(cols - col) {
            cells.remove(col);
            cells.push(blank.clone());
        }
    }

    fn insert_lines(&mut self, count: usize) {
        let row = usize::from(self.cursor.row);
        if row < self.scroll_top || row >= self.scroll_bottom {
            return;
        }
        let cols = self.cols();
        let bottom = self.scroll_bottom;
        for _ in 0..count.min(bottom - row) {
            let buffer = self.buffer_mut();
            buffer.rows.insert(row, Row::blank(cols));
            buffer.rows.remove(bottom);
        }
    }

    fn delete_lines(&mut self, count: usize) {
        let row = usize::from(self.cursor.row);
        if row < self.scroll_top || row >= self.scroll_bottom {
            return;
        }
        let cols = self.cols();
        let bottom = self.scroll_bottom;
        for _ in 0..count.min(bottom - row) {
            let buffer = self.buffer_mut();
            buffer.rows.remove(row);
            buffer.rows.insert(bottom - 1, Row::blank(cols));
        }
    }

    fn set_cursor(&mut self, row: usize, col: usize) {
        let origin = if self.modes.origin {
            self.scroll_top
        } else {
            0
        };
        let max_row = if self.modes.origin {
            self.scroll_bottom.saturating_sub(1)
        } else {
            self.rows().saturating_sub(1)
        };
        self.cursor.row = (origin + row).min(max_row) as u16;
        self.cursor.col = col.min(self.cols().saturating_sub(1)) as u16;
        self.pending_wrap = false;
    }

    fn set_mode(&mut self, private: bool, params: &Params, enabled: bool) {
        for value in flat_params(params) {
            if private {
                match value {
                    1 => self.modes.application_cursor = enabled,
                    6 => self.modes.origin = enabled,
                    7 => self.modes.auto_wrap = enabled,
                    25 => self.cursor.visible = enabled,
                    47 | 1047 | 1049 => self.use_alternate(enabled, value == 1049),
                    1000 => self.set_mouse_mode(MouseMode::Normal, enabled),
                    1002 => self.set_mouse_mode(MouseMode::ButtonMotion, enabled),
                    1003 => self.set_mouse_mode(MouseMode::AnyMotion, enabled),
                    1004 => self.modes.focus_events = enabled,
                    1006 => self.modes.sgr_mouse = enabled,
                    2004 => self.modes.bracketed_paste = enabled,
                    _ => self.unsupported("DEC mode", value),
                }
            }
        }
    }

    fn set_mouse_mode(&mut self, mode: MouseMode, enabled: bool) {
        if enabled {
            self.modes.mouse = mode;
        } else if self.modes.mouse == mode {
            self.modes.mouse = MouseMode::None;
        }
    }

    fn use_alternate(&mut self, enabled: bool, save_cursor: bool) {
        if enabled == self.active_alternate {
            return;
        }
        if enabled {
            if save_cursor {
                self.saved_cursor = self.cursor;
            }
            let cols = self.cols();
            let rows = self.rows();
            self.alternate = Buffer::new(cols, rows);
            let placements = self.placements.len();
            self.placements
                .retain(|placement| !placement.alternate_screen);
            if self.placements.len() != placements {
                self.graphics_generation = self.graphics_generation.saturating_add(1);
            }
            self.cursor.row = 0;
            self.cursor.col = 0;
        } else if save_cursor {
            self.cursor = self.saved_cursor;
        }
        self.active_alternate = enabled;
        self.modes.alternate_screen = enabled;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows();
        self.pending_wrap = false;
    }

    fn sgr(&mut self, params: &Params) {
        let values: Vec<u16> = if params.is_empty() {
            vec![0]
        } else {
            flat_params(params)
        };
        let mut index = 0;
        while index < values.len() {
            let value = values[index];
            match value {
                0 => self.rendition = Rendition::default(),
                1 => self.rendition.attributes.bold = true,
                2 => self.rendition.attributes.dim = true,
                3 => self.rendition.attributes.italic = true,
                4 => self.rendition.attributes.underline = true,
                5 => self.rendition.attributes.blink = true,
                7 => self.rendition.attributes.inverse = true,
                8 => self.rendition.attributes.hidden = true,
                9 => self.rendition.attributes.strike = true,
                22 => {
                    self.rendition.attributes.bold = false;
                    self.rendition.attributes.dim = false;
                }
                23 => self.rendition.attributes.italic = false,
                24 => self.rendition.attributes.underline = false,
                25 => self.rendition.attributes.blink = false,
                27 => self.rendition.attributes.inverse = false,
                28 => self.rendition.attributes.hidden = false,
                29 => self.rendition.attributes.strike = false,
                30..=37 => self.rendition.foreground = Color::Indexed((value - 30) as u8),
                39 => self.rendition.foreground = Color::Default,
                40..=47 => self.rendition.background = Color::Indexed((value - 40) as u8),
                49 => self.rendition.background = Color::Default,
                90..=97 => self.rendition.foreground = Color::Indexed((value - 90 + 8) as u8),
                100..=107 => self.rendition.background = Color::Indexed((value - 100 + 8) as u8),
                38 | 48 => {
                    let foreground = value == 38;
                    if values.get(index + 1) == Some(&5) && values.get(index + 2).is_some() {
                        let color = Color::Indexed(values[index + 2].min(255) as u8);
                        if foreground {
                            self.rendition.foreground = color;
                        } else {
                            self.rendition.background = color;
                        }
                        index += 2;
                    } else if values.get(index + 1) == Some(&2) && values.len() > index + 4 {
                        let color = Color::Rgb(
                            values[index + 2].min(255) as u8,
                            values[index + 3].min(255) as u8,
                            values[index + 4].min(255) as u8,
                        );
                        if foreground {
                            self.rendition.foreground = color;
                        } else {
                            self.rendition.background = color;
                        }
                        index += 4;
                    }
                }
                _ => self.unsupported("SGR", value),
            }
            index += 1;
        }
    }

    fn cursor_style(&mut self, value: u16) {
        let (shape, blinking) = match value {
            0 | 1 => (CursorShape::Block, true),
            2 => (CursorShape::Block, false),
            3 => (CursorShape::Underline, true),
            4 => (CursorShape::Underline, false),
            5 => (CursorShape::Bar, true),
            6 => (CursorShape::Bar, false),
            _ => return self.unsupported("cursor style", value),
        };
        self.cursor.shape = shape;
        self.cursor.blinking = blinking;
    }

    fn dispatch_apc(&mut self, payload: &[u8]) {
        if !payload.starts_with(b"G") {
            self.diagnostics.record("APC:unsupported");
            return;
        }
        let payload = &payload[1..];
        let (control, encoded) = payload
            .iter()
            .position(|byte| *byte == b';')
            .map_or((payload, &[][..]), |index| {
                (&payload[..index], &payload[index + 1..])
            });
        let control = String::from_utf8_lossy(control);
        let values: HashMap<&str, &str> = control
            .split(',')
            .filter_map(|item| item.split_once('='))
            .collect();
        let action = values.get("a").copied().unwrap_or("t");
        if action == "d" {
            self.delete_graphics(&values);
            return;
        }
        let explicit_id = values.get("i").and_then(|value| value.parse().ok());
        let id = explicit_id
            .or_else(|| {
                matches!(action, "t" | "T")
                    .then_some(self.active_transfer)
                    .flatten()
            })
            .unwrap_or_else(|| {
                let id = self.next_image_id;
                self.next_image_id = self.next_image_id.saturating_add(1);
                id
            });
        if action == "q" {
            self.replies
                .push(format!("\x1b_Gi={id};OK\x1b\\").into_bytes());
            return;
        }
        if matches!(action, "t" | "T") {
            let placement = (action == "T").then(|| self.kitty_placement(id, &values));
            let transfer = self.transfers.entry(id).or_default();
            transfer.format = values
                .get("f")
                .and_then(|value| value.parse().ok())
                .unwrap_or(32);
            transfer.width = values
                .get("s")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            transfer.height = values
                .get("v")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            transfer.compressed |= values.get("o") == Some(&"z");
            transfer.placement = transfer.placement.or(placement);
            match base64::engine::general_purpose::STANDARD.decode(encoded) {
                Ok(bytes) => transfer.bytes.extend(bytes),
                Err(error) => {
                    eprintln!("compi-daemon: invalid Kitty image payload: {error}");
                    self.transfers.remove(&id);
                    if self.active_transfer == Some(id) {
                        self.active_transfer = None;
                    }
                    return;
                }
            }
            if values.get("m") == Some(&"1") {
                self.active_transfer = Some(id);
            } else {
                if self.active_transfer == Some(id) {
                    self.active_transfer = None;
                }
                if !self.finish_transfer(id) {
                    return;
                }
            }
        }
        if action == "p" {
            self.place_image(id, &values);
        }
    }

    fn finish_transfer(&mut self, id: u32) -> bool {
        let Some(mut transfer) = self.transfers.remove(&id) else {
            return self.images.contains_key(&id);
        };
        if transfer.compressed {
            let mut decoded = Vec::new();
            if let Err(error) = ZlibDecoder::new(&transfer.bytes[..]).read_to_end(&mut decoded) {
                eprintln!("compi-daemon: invalid compressed Kitty image: {error}");
                return false;
            }
            transfer.bytes = decoded;
        }
        let current_bytes: usize = self
            .images
            .values()
            .map(|image| image.data.len().saturating_mul(3) / 4)
            .sum();
        if current_bytes.saturating_add(transfer.bytes.len()) > MAX_GRAPHICS_BYTES {
            eprintln!("compi-daemon: Kitty graphics memory limit exceeded");
            return false;
        }
        let placement = transfer.placement;
        self.images.insert(
            id,
            KittyImage {
                id,
                format: transfer.format,
                width: transfer.width,
                height: transfer.height,
                data: base64::engine::general_purpose::STANDARD.encode(&transfer.bytes),
            },
        );
        self.graphics_generation = self.graphics_generation.saturating_add(1);
        if let Some(placement) = placement {
            self.push_placement(placement);
        }
        true
    }

    fn place_image(&mut self, id: u32, values: &HashMap<&str, &str>) {
        if !self.images.contains_key(&id) {
            eprintln!("compi-daemon: Kitty placement references unknown image {id}");
            return;
        }
        let placement = self.kitty_placement(id, values);
        self.push_placement(placement);
    }

    fn kitty_placement(&self, id: u32, values: &HashMap<&str, &str>) -> KittyPlacement {
        KittyPlacement {
            image_id: id,
            placement_id: values.get("p").and_then(|value| value.parse().ok()),
            row: i32::from(self.cursor.row),
            col: self.cursor.col,
            rows: values.get("r").and_then(|value| value.parse().ok()),
            cols: values.get("c").and_then(|value| value.parse().ok()),
            z_index: values
                .get("z")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            alternate_screen: self.active_alternate,
        }
    }

    fn push_placement(&mut self, placement: KittyPlacement) {
        if let Some(placement_id) = placement.placement_id {
            self.placements.retain(|existing| {
                existing.image_id != placement.image_id
                    || existing.placement_id != Some(placement_id)
            });
        }
        self.placements.push(placement);
        self.graphics_generation = self.graphics_generation.saturating_add(1);
    }

    fn delete_graphics(&mut self, values: &HashMap<&str, &str>) {
        let before = (
            self.images.len(),
            self.placements.len(),
            self.transfers.len(),
        );
        match values.get("d").copied().unwrap_or("a") {
            "a" | "A" => {
                self.images.clear();
                self.placements.clear();
                self.transfers.clear();
                self.active_transfer = None;
            }
            "i" | "I" => {
                if let Some(id) = values.get("i").and_then(|value| value.parse().ok()) {
                    self.images.remove(&id);
                    self.placements.retain(|placement| placement.image_id != id);
                    self.transfers.remove(&id);
                    if self.active_transfer == Some(id) {
                        self.active_transfer = None;
                    }
                }
            }
            "p" | "P" => {
                if let Some(id) = values.get("p").and_then(|value| value.parse().ok()) {
                    self.placements
                        .retain(|placement| placement.placement_id != Some(id));
                }
            }
            mode => self
                .diagnostics
                .record(format!("Kitty-delete:{}", mode.to_ascii_lowercase())),
        }
        if before
            != (
                self.images.len(),
                self.placements.len(),
                self.transfers.len(),
            )
        {
            self.graphics_generation = self.graphics_generation.saturating_add(1);
        }
    }

    fn unsupported(&mut self, kind: &str, value: u16) {
        self.diagnostics.record(format!("{kind}:{value}"));
    }
}

impl Perform for TerminalState {
    fn print(&mut self, character: char) {
        self.print_char(character);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {}
            0x08 => {
                self.cursor.col = self.cursor.col.saturating_sub(1);
                self.pending_wrap = false;
            }
            0x09 => {
                let next = (usize::from(self.cursor.col) / 8 + 1) * 8;
                self.cursor.col = next.min(self.cols() - 1) as u16;
                self.pending_wrap = false;
            }
            0x0a..=0x0c => self.linefeed(false),
            0x0d => {
                self.cursor.col = 0;
                self.pending_wrap = false;
            }
            _ => {}
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], ignore: bool, action: char) {
        if !ignore {
            self.diagnostics.record(format!("DCS:{action}"));
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(command) = params
            .first()
            .and_then(|value| std::str::from_utf8(value).ok())
        else {
            return;
        };
        match command {
            "0" | "2" => {
                self.title = params
                    .get(1)
                    .map(|value| String::from_utf8_lossy(value).into_owned())
                    .unwrap_or_default();
            }
            "7" => {
                if let Some(path) = params.get(1).and_then(|value| parse_osc7_path(value)) {
                    self.current_directory = Some(path);
                }
            }
            "8" => self.active_hyperlink = parse_osc8_uri(params),
            "52" => {
                if let Some(text) = parse_osc52_clipboard(params) {
                    self.clipboard_writes.push(text);
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            self.diagnostics.record("CSI:oversized");
            return;
        }
        let private = intermediates == b"?";
        let count = first_param(params, 1).max(1) as usize;
        match (action, intermediates) {
            ('A', _) => self.cursor.row = self.cursor.row.saturating_sub(count as u16),
            ('B', _) => {
                self.cursor.row = (usize::from(self.cursor.row) + count).min(self.rows() - 1) as u16
            }
            ('C', _) => {
                self.cursor.col = (usize::from(self.cursor.col) + count).min(self.cols() - 1) as u16
            }
            ('D', _) => self.cursor.col = self.cursor.col.saturating_sub(count as u16),
            ('E', _) => {
                self.cursor.row =
                    (usize::from(self.cursor.row) + count).min(self.rows() - 1) as u16;
                self.cursor.col = 0;
            }
            ('F', _) => {
                self.cursor.row = self.cursor.row.saturating_sub(count as u16);
                self.cursor.col = 0;
            }
            ('G' | '`', _) => self.set_cursor(usize::from(self.cursor.row), count - 1),
            ('H' | 'f', _) => {
                let values = flat_params(params);
                let row = values.first().copied().unwrap_or(1).max(1) as usize - 1;
                let col = values.get(1).copied().unwrap_or(1).max(1) as usize - 1;
                self.set_cursor(row, col);
            }
            ('d', _) => self.set_cursor(count - 1, usize::from(self.cursor.col)),
            ('J', _) => self.erase_display(first_param(params, 0)),
            ('K', _) => self.erase_line(first_param(params, 0)),
            ('X', _) => {
                let row = usize::from(self.cursor.row);
                let col = usize::from(self.cursor.col);
                self.clear_row_range(row, col, col.saturating_add(count));
            }
            ('@', _) => self.insert_chars(count),
            ('P', _) => self.delete_chars(count),
            ('L', _) => self.insert_lines(count),
            ('M', _) => self.delete_lines(count),
            ('S', _) => self.scroll_up(count),
            ('T', _) => self.scroll_down(count),
            ('r', _) => {
                let values = flat_params(params);
                let top = values.first().copied().unwrap_or(1).max(1) as usize - 1;
                let bottom = values.get(1).copied().unwrap_or(self.rows() as u16) as usize;
                if top < bottom && bottom <= self.rows() {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.set_cursor(0, 0);
                }
            }
            ('m', _) => self.sgr(params),
            ('h', _) => self.set_mode(private, params, true),
            ('l', _) => self.set_mode(private, params, false),
            ('s', _) => self.saved_cursor = self.cursor,
            ('u', _) => self.cursor = self.saved_cursor,
            ('n', _) if first_param(params, 0) == 5 => self.replies.push(b"\x1b[0n".to_vec()),
            ('n', _) if first_param(params, 0) == 6 => self.replies.push(
                format!("\x1b[{};{}R", self.cursor.row + 1, self.cursor.col + 1).into_bytes(),
            ),
            ('c', _) => self.replies.push(b"\x1b[?1;2c".to_vec()),
            ('q', b" ") => self.cursor_style(first_param(params, 0)),
            _ => self.diagnostics.record(format!(
                "CSI:{action}:private={private}:intermediates={intermediates:?}:params={:?}",
                flat_params(params)
            )),
        }
        if !matches!(action, 'm' | 'h' | 'l' | 'n' | 'c' | 'q') {
            self.pending_wrap = false;
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match byte {
            b'7' => self.saved_cursor = self.cursor,
            b'8' => self.cursor = self.saved_cursor,
            b'D' => self.linefeed(false),
            b'E' => {
                self.cursor.col = 0;
                self.linefeed(false);
            }
            b'M' => self.reverse_index(),
            b'c' => {
                let cols = self.cols() as u16;
                let rows = self.rows() as u16;
                let diagnostics = std::mem::take(&mut self.diagnostics);
                *self = Self::new(cols, rows);
                self.diagnostics = diagnostics;
            }
            b'=' => self.modes.application_keypad = true,
            b'>' => self.modes.application_keypad = false,
            b'H' => {}
            _ => self.diagnostics.record(format!("ESC:{byte:02x}")),
        }
    }
}

fn parse_osc7_path(value: &[u8]) -> Option<String> {
    let uri = std::str::from_utf8(value).ok()?;
    let location = uri.strip_prefix("file://")?;
    let path = &location[location.find('/')?..];
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_digit(*bytes.get(index + 1)?)?;
            let low = hex_digit(*bytes.get(index + 2)?)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    if decoded.contains(&0) {
        return None;
    }
    String::from_utf8(decoded)
        .ok()
        .filter(|path| path.starts_with('/'))
}

fn parse_osc8_uri(params: &[&[u8]]) -> Option<SmolStr> {
    let uri_len = params
        .iter()
        .skip(2)
        .map(|part| part.len())
        .sum::<usize>()
        .saturating_add(params.len().saturating_sub(3));
    if uri_len == 0 || uri_len > MAX_OSC8_URI_BYTES {
        return None;
    }
    let mut uri = Vec::with_capacity(uri_len);
    for (index, part) in params.iter().skip(2).enumerate() {
        if index != 0 {
            uri.push(b';');
        }
        uri.extend_from_slice(part);
    }
    let uri = std::str::from_utf8(&uri).ok()?;
    if uri.chars().any(char::is_control) {
        return None;
    }
    Some(SmolStr::new(uri))
}

fn parse_osc52_clipboard(params: &[&[u8]]) -> Option<String> {
    let target = *params.get(1)?;
    if !matches!(target, b"" | b"c") {
        return None;
    }
    let encoded = *params.get(2)?;
    if encoded == b"?" || encoded.len() > MAX_OSC52_DECODED_BYTES.saturating_add(2) / 3 * 4 {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if decoded.len() > MAX_OSC52_DECODED_BYTES {
        return None;
    }
    String::from_utf8(decoded).ok()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn first_param(params: &Params, default: u16) -> u16 {
    params
        .iter()
        .next()
        .and_then(|param| param.first())
        .copied()
        .unwrap_or(default)
}

fn flat_params(params: &Params) -> Vec<u16> {
    params
        .iter()
        .flat_map(|param| param.iter().copied())
        .collect()
}

fn row_memory(row: &Row) -> usize {
    row.cells
        .iter()
        .map(|cell| 32 + cell.text.len() + cell.hyperlink.as_ref().map_or(0, |uri| uri.len()))
        .sum::<usize>()
        + 1
}
fn row_hash(row: &Row) -> u64 {
    let mut hasher = DefaultHasher::new();
    row.hash(&mut hasher);
    hasher.finish()
}

fn reflow_main_buffer(
    buffer: &mut Buffer,
    cols: usize,
    rows: usize,
    cursor: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let source: Vec<Row> = buffer
        .scrollback
        .drain(..)
        .chain(buffer.rows.drain(..))
        .collect();
    let mut logical_lines: Vec<(Vec<Cell>, Option<usize>)> = Vec::new();
    let mut line_cells = Vec::new();
    let mut line_cursor = None;
    for (row_index, row) in source.into_iter().enumerate() {
        if let Some((cursor_row, cursor_col)) = cursor
            && cursor_row == row_index
        {
            line_cursor =
                Some(line_cells.len() + cursor_col.min(row.cells.len().saturating_sub(1)));
        }
        line_cells.extend(row.cells);
        if !row.wrapped {
            trim_reflow_line(&mut line_cells, line_cursor);
            logical_lines.push((std::mem::take(&mut line_cells), line_cursor.take()));
        }
    }
    if !line_cells.is_empty() || logical_lines.is_empty() {
        trim_reflow_line(&mut line_cells, line_cursor);
        logical_lines.push((line_cells, line_cursor));
    }

    let mut physical_rows = Vec::new();
    let mut mapped_cursor = None;
    for (cells, cursor_offset) in logical_lines {
        let line_start = physical_rows.len();
        let mut row = Row::blank(cols);
        let mut col = 0;
        let mut source_col = 0;
        while source_col < cells.len() {
            let cell = &cells[source_col];
            let width = usize::from(cell.width.max(1));
            if col + width > cols {
                row.wrapped = true;
                physical_rows.push(row);
                row = Row::blank(cols);
                col = 0;
            }
            if cursor_offset.is_some_and(|offset| {
                offset >= source_col && offset < source_col.saturating_add(width)
            }) {
                mapped_cursor = Some((
                    physical_rows.len(),
                    col + cursor_offset.unwrap().saturating_sub(source_col),
                ));
            }
            if cell.width == 0 {
                source_col += 1;
                continue;
            }
            row.cells[col] = cell.clone();
            if width == 2 && col + 1 < cols {
                row.cells[col + 1] = cells
                    .get(source_col + 1)
                    .filter(|continuation| continuation.width == 0)
                    .cloned()
                    .unwrap_or_else(|| Cell {
                        text: SmolStr::new_static(""),
                        width: 0,
                        ..Cell::default()
                    });
            }
            col += width;
            source_col += width;
        }
        if cursor_offset == Some(cells.len()) {
            mapped_cursor = Some((physical_rows.len(), col.min(cols.saturating_sub(1))));
        }
        physical_rows.push(row);
        if mapped_cursor.is_none() && cursor_offset.is_some() {
            mapped_cursor = Some((line_start, 0));
        }
    }

    while physical_rows.len() < rows {
        physical_rows.push(Row::blank(cols));
    }
    let viewport_start = physical_rows.len().saturating_sub(rows);
    let viewport = physical_rows.split_off(viewport_start);
    buffer.scrollback = physical_rows.into();
    buffer.rows = viewport;
    buffer.scrollback_bytes = buffer.scrollback.iter().map(row_memory).sum();
    while buffer.scrollback_bytes > MAX_SCROLLBACK_BYTES {
        let Some(removed) = buffer.scrollback.pop_front() else {
            break;
        };
        buffer.scrollback_bytes = buffer.scrollback_bytes.saturating_sub(row_memory(&removed));
    }
    mapped_cursor.map(|(row, col)| (row.saturating_sub(viewport_start), col))
}

fn trim_reflow_line(cells: &mut Vec<Cell>, cursor: Option<usize>) {
    let minimum = cursor.map_or(0, |offset| offset.saturating_add(1));
    while cells.len() > minimum && cells.last().is_some_and(|cell| cell == &Cell::default()) {
        cells.pop();
    }
}

fn resize_buffer_cells(buffer: &mut Buffer, cols: usize, rows: usize) {
    for row in &mut buffer.rows {
        row.cells.resize(cols, Cell::default());
        repair_wide_cells(row);
    }
    while buffer.rows.len() > rows {
        buffer.rows.remove(0);
    }
    while buffer.rows.len() < rows {
        buffer.rows.push(Row::blank(cols));
    }
}

fn repair_wide_cells(row: &mut Row) {
    for index in 0..row.cells.len() {
        if row.cells[index].width == 0 && (index == 0 || row.cells[index - 1].width != 2) {
            row.cells[index] = Cell::default();
        }
        if row.cells[index].width == 2 && index + 1 >= row.cells.len() {
            row.cells[index] = Cell::default();
        }
    }
}

pub fn encode_screen(message: &ScreenMessage) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(message, bincode::config::standard())
}

pub fn decode_screen(payload: &[u8]) -> Result<ScreenMessage, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(payload, bincode::config::standard())
        .map(|(message, _)| message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(row: &Row) -> String {
        row.cells
            .iter()
            .filter(|cell| cell.width != 0)
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn visible_text(snapshot: &ScreenSnapshot, row: usize) -> String {
        row_text(&snapshot.cells[row])
    }

    #[test]
    fn parses_unicode_attributes_cursor_and_title() {
        let mut terminal = TerminalState::new(12, 3);
        terminal.advance(b"A\x1b[31mB\x1b[0m\xe7\x95\x8c\x1b]2;Compi\x07");
        let snapshot = terminal.snapshot();
        assert_eq!(visible_text(&snapshot, 0), "AB界");
        assert_eq!(snapshot.cells[0].cells[1].foreground, Color::Indexed(1));
        assert_eq!(snapshot.cells[0].cells[2].width, 2);
        assert_eq!(snapshot.cells[0].cells[3].width, 0);
        assert_eq!(snapshot.title, "Compi");
    }

    #[test]
    fn keeps_combining_emoji_and_flags_in_single_cells() {
        let mut terminal = TerminalState::new(16, 2);
        terminal.advance("e\u{301} 👩\u{200d}💻 🇺🇸".as_bytes());
        let snapshot = terminal.snapshot();
        let row = &snapshot.cells[0];
        assert_eq!(row.cells[0].text, "e\u{301}");
        assert_eq!((row.cells[0].width, row.cells[1].width), (1, 1));
        assert_eq!(row.cells[2].text, "👩\u{200d}💻", "{:?}", row.cells);
        assert_eq!((row.cells[2].width, row.cells[3].width), (2, 0));
        assert_eq!(row.cells[5].text, "🇺🇸");
        assert_eq!((row.cells[5].width, row.cells[6].width), (2, 0));
    }

    #[test]
    fn tracks_percent_decoded_osc7_working_directory() {
        let mut terminal = TerminalState::new(12, 3);
        let (delta, _) =
            terminal.advance(b"\x1b]7;file://wsl-host/home/dev/Agent%20Projects/%CF%80\x07");
        assert_eq!(
            terminal.snapshot().current_directory.as_deref(),
            Some("/home/dev/Agent Projects/π")
        );
        assert_eq!(
            delta.and_then(|delta| delta.current_directory),
            Some("/home/dev/Agent Projects/π".to_owned())
        );

        terminal.advance(b"\x1b]7;https://example.invalid/path\x07");
        assert_eq!(
            terminal.snapshot().current_directory.as_deref(),
            Some("/home/dev/Agent Projects/π")
        );
    }

    #[test]
    fn tracks_osc8_hyperlinks_on_printed_cells() {
        let mut terminal = TerminalState::new(12, 2);
        terminal.advance(b"\x1b]8;;https://example.com/docs\x07link\x1b]8;;\x07 plain");
        let snapshot = terminal.snapshot();
        for cell in &snapshot.cells[0].cells[..4] {
            assert_eq!(cell.hyperlink.as_deref(), Some("https://example.com/docs"));
        }
        assert!(snapshot.cells[0].cells[4].hyperlink.is_none());
    }

    #[test]
    fn emits_only_valid_windows_clipboard_writes() {
        let mut terminal = TerminalState::new(12, 2);
        let (delta, _) = terminal.advance(b"\x1b]52;c;Y29waWVkIHRleHQ=\x07");
        assert_eq!(delta.unwrap().clipboard_writes, ["copied text".to_owned()]);

        assert!(
            terminal
                .advance(b"\x1b]52;p;bm90IHN1cHBvcnRlZA==\x07")
                .0
                .is_none()
        );
        assert!(terminal.advance(b"\x1b]52;c;?\x07").0.is_none());
        assert!(terminal.advance(b"\x1b]52;c;not-base64\x07").0.is_none());
    }

    #[test]
    fn tracks_alternate_screen_and_bounded_scrollback() {
        let mut terminal = TerminalState::new(8, 2);
        terminal.advance(b"one\r\ntwo\r\nthree");
        assert!(!terminal.snapshot().scrollback.is_empty());
        terminal.advance(b"\x1b[?1049hALT");
        assert!(terminal.snapshot().modes.alternate_screen);
        assert_eq!(visible_text(&terminal.snapshot(), 0), "ALT");
        terminal.advance(b"\x1b[?1049l");
        assert!(!terminal.snapshot().modes.alternate_screen);
        assert_eq!(visible_text(&terminal.snapshot(), 1), "three");
    }

    #[test]
    fn marks_source_rows_as_wrapped_and_reflows_logical_lines() {
        let mut terminal = TerminalState::new(5, 3);
        terminal.advance(b"abcdefgh");
        let initial = terminal.snapshot();
        assert!(initial.cells[0].wrapped);
        assert!(!initial.cells[1].wrapped);

        terminal.resize(4, 3);
        let narrow = terminal.snapshot();
        let narrow_text: Vec<_> = narrow
            .scrollback
            .iter()
            .chain(&narrow.cells)
            .map(row_text)
            .filter(|text| !text.is_empty())
            .collect();
        assert_eq!(narrow_text, ["abcd", "efgh"]);
        assert!(narrow.scrollback.first().is_some_and(|row| row.wrapped));

        terminal.resize(10, 3);
        let wide = terminal.snapshot();
        let wide_text: Vec<_> = wide
            .scrollback
            .iter()
            .chain(&wide.cells)
            .map(row_text)
            .filter(|text| !text.is_empty())
            .collect();
        assert_eq!(wide_text, ["abcdefgh"]);
        assert_eq!((wide.cursor.row, wide.cursor.col), (0, 8));
    }
    #[test]
    fn counts_repeated_unsupported_sequences_by_signature() {
        let mut terminal = TerminalState::new(8, 2);
        terminal.set_diagnostic_context("test-session", Some("workflow"));
        terminal.advance(b"\x1b[?9001h\x1b[?9001l\x1b[?9001h");
        assert_eq!(
            terminal.diagnostics.counts.get("DEC mode:9001").copied(),
            Some(3)
        );
    }

    #[test]
    fn tracks_application_cursor_and_keypad_modes() {
        let mut terminal = TerminalState::new(8, 2);
        terminal.advance(b"\x1b[?1h\x1b=");
        let enabled = terminal.snapshot();
        assert!(enabled.modes.application_cursor);
        assert!(enabled.modes.application_keypad);

        terminal.advance(b"\x1b[?1l\x1b>");
        let disabled = terminal.snapshot();
        assert!(!disabled.modes.application_cursor);
        assert!(!disabled.modes.application_keypad);
    }

    #[test]
    fn tracks_mouse_and_focus_reporting_modes() {
        let mut terminal = TerminalState::new(8, 2);
        terminal.advance(b"\x1b[?1002;1004;1006h");
        let enabled = terminal.snapshot();
        assert_eq!(enabled.modes.mouse, MouseMode::ButtonMotion);
        assert!(enabled.modes.focus_events);
        assert!(enabled.modes.sgr_mouse);

        terminal.advance(b"\x1b[?1003h");
        assert_eq!(terminal.snapshot().modes.mouse, MouseMode::AnyMotion);
        terminal.advance(b"\x1b[?1002l");
        assert_eq!(terminal.snapshot().modes.mouse, MouseMode::AnyMotion);
        terminal.advance(b"\x1b[?1003;1004;1006l");
        let disabled = terminal.snapshot();
        assert_eq!(disabled.modes.mouse, MouseMode::None);
        assert!(!disabled.modes.focus_events);
        assert!(!disabled.modes.sgr_mouse);
    }

    #[test]
    fn produces_gap_detectable_sequenced_deltas() {
        let mut terminal = TerminalState::new(10, 2);
        let snapshot = terminal.snapshot();
        let (first, _) = terminal.advance(b"first");
        let (second, _) = terminal.advance(b"second");
        let mut mirror = ScreenMirror::default();
        assert_eq!(
            mirror.apply(ScreenMessage::Snapshot { snapshot }),
            MirrorApply::Applied
        );
        assert_eq!(
            mirror.apply(ScreenMessage::Delta {
                delta: second.unwrap()
            }),
            MirrorApply::Gap {
                expected: 1,
                actual: 2
            }
        );
        assert!(first.is_some());
    }

    #[test]
    fn parses_kitty_transmit_place_and_delete() {
        let mut terminal = TerminalState::new(10, 2);
        terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=9;AQIDBA==\x1b\\");
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.images.len(), 1);
        assert_eq!(snapshot.images[0].id, 7);
        assert_eq!(snapshot.placements[0].placement_id, Some(9));
        terminal.advance(b"\x1b_Ga=d,d=i,i=7\x1b\\");
        assert!(terminal.snapshot().images.is_empty());
        assert!(terminal.snapshot().placements.is_empty());
    }

    #[test]
    fn replies_to_terminal_status_queries() {
        let mut terminal = TerminalState::new(10, 2);
        terminal.advance(b"abc");
        let (_, replies) = terminal.advance(b"\x1b[6n");
        assert_eq!(replies, vec![b"\x1b[1;4R".to_vec()]);
    }
    #[test]
    fn mirror_matches_authority_after_deltas_and_resize() {
        let mut terminal = TerminalState::new(8, 2);
        let mut mirror = ScreenMirror::default();
        assert_eq!(
            mirror.apply(ScreenMessage::Snapshot {
                snapshot: terminal.snapshot(),
            }),
            MirrorApply::Applied
        );
        let (delta, _) = terminal.advance(b"hello");
        assert_eq!(
            mirror.apply(ScreenMessage::Delta {
                delta: delta.unwrap(),
            }),
            MirrorApply::Applied
        );
        assert_eq!(mirror.snapshot(), Some(&terminal.snapshot()));
        let delta = terminal.resize(5, 3).unwrap();
        assert_eq!(
            mirror.apply(ScreenMessage::Delta { delta }),
            MirrorApply::Applied
        );
        assert_eq!(mirror.snapshot(), Some(&terminal.snapshot()));
    }

    #[test]
    fn applies_editing_modes_and_cursor_operations() {
        let mut terminal = TerminalState::new(10, 3);
        terminal.advance(b"abcde\x1b[2D\x1b[@Z\x1b[?2004h");
        let snapshot = terminal.snapshot();
        assert_eq!(visible_text(&snapshot, 0), "abcZde");
        assert_eq!(snapshot.cursor.col, 4);
        assert!(snapshot.modes.bracketed_paste);
        terminal.advance(b"\x1b[2K");
        assert_eq!(visible_text(&terminal.snapshot(), 0), "");
    }

    #[test]
    fn bounds_scrollback_by_memory() {
        let mut terminal = TerminalState::new(16, 2);
        let mut output = Vec::new();
        for index in 0..8_000 {
            output.extend_from_slice(format!("{index:08}\r\n").as_bytes());
        }
        terminal.advance(&output);
        assert!(terminal.main.scrollback_bytes <= MAX_SCROLLBACK_BYTES);
        assert!(terminal.main.scrollback.len() < 8_000);
    }

    #[test]
    fn completes_chunked_kitty_transfers_and_queries() {
        let mut terminal = TerminalState::new(10, 2);
        terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=8,p=3,m=1;AQI=\x1b\\");
        assert!(terminal.snapshot().images.is_empty());
        terminal.advance(b"\x1b_Gm=0;AwQ=\x1b\\");
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.images[0].data, "AQIDBA==");
        assert_eq!(snapshot.placements[0].placement_id, Some(3));
        let (_, replies) = terminal.advance(b"\x1b_Ga=q,i=8\x1b\\");
        assert_eq!(replies, vec![b"\x1b_Gi=8;OK\x1b\\".to_vec()]);
        terminal.advance(b"\x1b_Ga=t,f=32,s=1,v=1,i=9,o=z;eJxjZGJmAQAAGAAL\x1b\\");
        let compressed = terminal
            .snapshot()
            .images
            .into_iter()
            .find(|image| image.id == 9)
            .unwrap();
        assert_eq!(compressed.data, "AQIDBA==");
    }

    #[test]
    fn screen_messages_round_trip_as_binary() {
        let message = ScreenMessage::Snapshot {
            snapshot: TerminalState::new(4, 2).snapshot(),
        };
        assert_eq!(
            decode_screen(&encode_screen(&message).unwrap()).unwrap(),
            message
        );
    }
}
