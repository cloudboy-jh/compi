pub mod trace;

use base64::Engine;
use compi_protocol::{
    Cell, Color, CursorShape, CursorState, DEFAULT_GRAPHICS_BYTES, KittyImage, KittyPlacement,
    MAX_DECODED_IMAGE_BYTES, MAX_GRAPHICS_BYTES, MouseMode, Row, RowUpdate, TerminalModes,
    TextAttributes,
};
use flate2::read::ZlibDecoder;
use smol_str::SmolStr;
use std::collections::{BTreeMap, HashMap, VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::io::Read;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vte::{Params, Parser, Perform};

pub const MAX_SCROLLBACK_BYTES: usize = 1024 * 1024;
const MAX_GRAPHICS_TRANSFERS: usize = 64;
const MAX_GRAPHICS_IMAGES: usize = 1024;
const MAX_GRAPHICS_PLACEMENTS: usize = 4096;
const MAX_APC_BYTES: usize = 8 * 1024 * 1024;
const MAX_OSC8_URI_BYTES: usize = 2048;
const MAX_UNSUPPORTED_SIGNATURES: usize = 128;
const MAX_OSC52_DECODED_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
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

struct Buffer {
    rows: Vec<Row>,
    scrollback: VecDeque<Row>,
    scrollback_bytes: usize,
    scrollback_line_limit: usize,
}

impl Buffer {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            rows: vec![Row::blank(cols); rows],
            scrollback: VecDeque::new(),
            scrollback_bytes: 0,
            scrollback_line_limit: 10_000,
        }
    }

    fn push_scrollback(&mut self, row: Row) {
        self.scrollback_bytes += row_memory(&row);
        self.scrollback.push_back(row);
        while self.scrollback_bytes > MAX_SCROLLBACK_BYTES
            || self.scrollback.len() > self.scrollback_line_limit
        {
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
    reserved_bytes: usize,
    quiet: u8,
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
    image_generation: u64,
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
    image_generation: u64,
    replies: Vec<Vec<u8>>,
    clipboard_writes: Vec<String>,
    images: HashMap<u32, KittyImage>,
    placements: Vec<KittyPlacement>,
    transfers: HashMap<u32, KittyTransfer>,
    next_image_id: u32,
    active_transfer: Option<u32>,
    diagnostics: UnsupportedDiagnostics,
    graphics_byte_limit: usize,
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
            image_generation: 0,
            placements: Vec::new(),
            transfers: HashMap::new(),
            next_image_id: 1,
            active_transfer: None,
            diagnostics: UnsupportedDiagnostics::default(),
            graphics_byte_limit: DEFAULT_GRAPHICS_BYTES,
        }
    }

    pub fn set_resource_limits(&mut self, scrollback_lines: usize, graphics_bytes: usize) {
        self.main.scrollback_line_limit = scrollback_lines;
        self.alternate.scrollback_line_limit = scrollback_lines;
        self.graphics_byte_limit = graphics_bytes.min(MAX_GRAPHICS_BYTES);
    }

    pub fn clear_scrollback(&mut self) {
        self.main.scrollback.clear();
        self.main.scrollback_bytes = 0;
        self.prune_history_placements();
        self.scrollback_generation = self.scrollback_generation.saturating_add(1);
        self.sequence = self.sequence.saturating_add(1);
    }
    pub fn set_diagnostic_context(&mut self, session_id: &str, trace_label: Option<&str>) {
        self.diagnostics.context = match trace_label {
            Some(label) => format!("session={session_id},trace={label}"),
            None => format!("session={session_id}"),
        };
    }

    pub fn snapshot(&self) -> Snapshot {
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
        Snapshot {
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

    pub fn advance(&mut self, bytes: &[u8]) -> (Option<Delta>, Vec<Vec<u8>>) {
        let before = self.change_baseline();
        let mut offset = 0;
        while offset < bytes.len() {
            if matches!(&self.input_state, InputState::Normal) {
                let run_length = bytes[offset..]
                    .iter()
                    .position(|byte| *byte == 0x1b)
                    .unwrap_or(bytes.len() - offset);
                if run_length > 0 {
                    self.feed_vte(&bytes[offset..offset + run_length]);
                    offset += run_length;
                    continue;
                }
            } else if matches!(&self.input_state, InputState::Apc(_)) {
                let run_length = bytes[offset..]
                    .iter()
                    .position(|byte| matches!(*byte, 0x1b | 0x9c))
                    .unwrap_or(bytes.len() - offset);
                if run_length > 0 {
                    let InputState::Apc(mut payload) =
                        std::mem::replace(&mut self.input_state, InputState::Normal)
                    else {
                        unreachable!("checked APC input state");
                    };
                    let remaining_capacity = MAX_APC_BYTES.saturating_sub(payload.len());
                    if run_length <= remaining_capacity {
                        payload.extend_from_slice(&bytes[offset..offset + run_length]);
                        self.input_state = InputState::Apc(payload);
                        offset += run_length;
                    } else {
                        payload.extend_from_slice(&bytes[offset..offset + remaining_capacity]);
                        self.reject_oversized_apc(&payload);
                        self.input_state = InputState::ApcDiscard;
                        offset += remaining_capacity + 1;
                    }
                    continue;
                }
            }
            self.advance_byte(bytes[offset]);
            offset += 1;
        }
        self.finish_change(before)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Option<Delta> {
        let cols = usize::from(cols.max(1));
        let rows = usize::from(rows.max(1));
        if cols == self.cols() && rows == self.rows() {
            return None;
        }
        let before = self.change_baseline();
        let old_scrollback = self.main.scrollback.len();
        let cursor_in_main = (!self.active_alternate).then(|| {
            (
                old_scrollback + usize::from(self.cursor.row),
                usize::from(self.cursor.col),
            )
        });
        let source_rows = old_scrollback + self.main.rows.len();
        let placement_anchors: Vec<_> = self
            .placements
            .iter()
            .enumerate()
            .filter(|(_, placement)| !placement.alternate_screen)
            .filter_map(|(index, placement)| {
                let row = i64::try_from(old_scrollback).ok()? + i64::from(placement.row);
                (row >= 0 && usize::try_from(row).ok()? < source_rows).then_some((
                    index,
                    (usize::try_from(row).unwrap(), usize::from(placement.col)),
                ))
            })
            .collect();
        let anchors: Vec<_> = placement_anchors
            .iter()
            .map(|(_, anchor)| *anchor)
            .collect();
        let (reflowed_cursor, mapped_anchors) =
            reflow_main_buffer(&mut self.main, cols, rows, cursor_in_main, &anchors);
        let mut keep = vec![true; self.placements.len()];
        let mut graphics_changed = false;
        for ((index, _), mapped) in placement_anchors.into_iter().zip(mapped_anchors) {
            match mapped {
                Some((row, col)) => {
                    let placement = &mut self.placements[index];
                    if placement.row != row || placement.col != col {
                        placement.row = row;
                        placement.col = col;
                        graphics_changed = true;
                    }
                }
                None => {
                    keep[index] = false;
                    graphics_changed = true;
                }
            }
        }
        if graphics_changed {
            let mut index = 0;
            self.placements.retain(|_| {
                let retain = keep[index];
                index += 1;
                retain
            });
            self.graphics_generation = self.graphics_generation.saturating_add(1);
        }
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
            image_generation: self.image_generation,
            cursor: self.cursor,
            modes: self.modes.clone(),
            title: self.title.clone(),
            current_directory: self.current_directory.clone(),
        }
    }

    fn finish_change(&mut self, before: ChangeBaseline) -> (Option<Delta>, Vec<Vec<u8>>) {
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
        let images = (before.image_generation != self.image_generation).then(|| {
            let mut images: Vec<_> = self.images.values().cloned().collect();
            images.sort_by_key(|image| image.id);
            images
        });
        let placements = graphics_changed.then(|| {
            let mut placements = self.placements.clone();
            placements.sort_by_key(|placement| {
                (
                    placement.z_index,
                    placement.image_id,
                    placement.placement_id.unwrap_or(0),
                )
            });
            placements
        });
        let delta = Delta {
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
            InputState::Apc(payload) => {
                self.reject_oversized_apc(&payload);
                self.input_state = InputState::ApcDiscard;
            }
            InputState::ApcEscape(payload) if byte == b'\\' => self.dispatch_apc(&payload),
            InputState::ApcEscape(mut payload) if payload.len() + 2 <= MAX_APC_BYTES => {
                payload.push(0x1b);
                payload.push(byte);
                self.input_state = InputState::Apc(payload);
            }
            InputState::ApcEscape(payload) => {
                self.reject_oversized_apc(&payload);
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

    fn reject_oversized_apc(&mut self, payload: &[u8]) {
        self.diagnostics.record("APC:oversized");
        let Some(control) = payload
            .strip_prefix(b"G")
            .and_then(|payload| payload.split(|byte| *byte == b';').next())
            .and_then(|control| std::str::from_utf8(control).ok())
        else {
            return;
        };
        let id = control
            .split(',')
            .find_map(|item| item.strip_prefix("i=")?.parse().ok())
            .or(self.active_transfer)
            .unwrap_or(0);
        let transfer = self.transfers.remove(&id);
        let quiet = control
            .split(',')
            .find_map(|item| item.strip_prefix("q=")?.parse().ok())
            .unwrap_or_else(|| transfer.as_ref().map_or(0, |transfer| transfer.quiet));
        if self.active_transfer == Some(id) {
            self.active_transfer = None;
        }
        self.graphics_error(id, quiet, "E2BIG:APC chunk exceeds limit");
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
                    && (placement.row >= top as i32 || (!active_alt && top == 0))
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
        self.prune_history_placements();
    }

    fn prune_history_placements(&mut self) {
        let min_row = -(self.main.scrollback.len() as i32);
        let before = self.placements.len();
        self.placements.retain(|placement| {
            placement.row
                >= if placement.alternate_screen {
                    0
                } else {
                    min_row
                }
        });
        if self.placements.len() != before {
            self.graphics_generation = self.graphics_generation.saturating_add(1);
        }
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
                    self.prune_history_placements();
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
        if let Some(key) = values.keys().find(|key| {
            !matches!(
                **key,
                "a" | "i" | "f" | "s" | "v" | "o" | "q" | "m" | "t" | "p" | "r" | "c" | "z" | "d"
            )
        }) {
            let id = values
                .get("i")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let quiet = values
                .get("q")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            self.diagnostics.record(format!("Kitty:{key}"));
            self.graphics_error(id, quiet, "ENOTSUP:unsupported control key");
            return;
        }
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
                loop {
                    let id = self.next_image_id;
                    self.next_image_id = self.next_image_id.wrapping_add(1).max(1);
                    if !self.images.contains_key(&id) && !self.transfers.contains_key(&id) {
                        break id;
                    }
                }
            });
        if action == "q" {
            self.replies
                .push(format!("\x1b_Gi={id};OK\x1b\\").into_bytes());
            return;
        }
        if matches!(action, "t" | "T") {
            // Explicit initial metadata restarts an abandoned transfer, but never
            // removes the previously committed image with the same ID.
            if values.contains_key("f") || values.contains_key("s") || values.contains_key("v") {
                self.transfers.remove(&id);
            }
            let mut transfer = self.transfers.remove(&id).unwrap_or_default();
            transfer.format = values
                .get("f")
                .and_then(|value| value.parse().ok())
                .unwrap_or(if transfer.format == 0 {
                    32
                } else {
                    transfer.format
                });
            transfer.width = values
                .get("s")
                .and_then(|value| value.parse().ok())
                .unwrap_or(transfer.width);
            transfer.height = values
                .get("v")
                .and_then(|value| value.parse().ok())
                .unwrap_or(transfer.height);
            transfer.compressed |= values.get("o") == Some(&"z");
            transfer.quiet = values
                .get("q")
                .and_then(|value| value.parse().ok())
                .unwrap_or(transfer.quiet);
            if transfer.placement.is_none() && action == "T" {
                transfer.placement = Some(self.kitty_placement(id, &values));
            }
            let result = self.append_transfer(id, &mut transfer, encoded, &values);
            if let Err(error) = result {
                if self.active_transfer == Some(id) {
                    self.active_transfer = None;
                }
                self.graphics_error(id, transfer.quiet, error);
                return;
            }
            if values.get("m") == Some(&"1") {
                self.transfers.insert(id, transfer);
                self.active_transfer = Some(id);
            } else {
                if self.active_transfer == Some(id) {
                    self.active_transfer = None;
                }
                let quiet = transfer.quiet;
                if let Err(error) = self.finish_transfer(id, transfer) {
                    self.graphics_error(id, quiet, error);
                }
            }
        } else if action == "p" {
            let quiet = values
                .get("q")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            if !self.images.contains_key(&id) {
                self.graphics_error(id, quiet, "ENOENT:unknown image");
            } else {
                let placement = self.kitty_placement(id, &values);
                if self.has_placement_capacity(&placement) {
                    self.push_placement(placement);
                } else {
                    self.graphics_error(id, quiet, "ENOSPC:placement limit exceeded");
                }
            }
        } else {
            let quiet = values
                .get("q")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            self.graphics_error(id, quiet, "ENOTSUP:unsupported action");
        }
    }

    fn graphics_error(&mut self, id: u32, quiet: u8, error: &str) {
        if quiet < 2 {
            self.replies
                .push(format!("\x1b_Gi={id};{error}\x1b\\").into_bytes());
        }
    }

    fn graphics_bytes(&self) -> usize {
        self.images
            .values()
            .map(|image| image.data.len())
            .chain(
                self.transfers
                    .values()
                    .map(|transfer| transfer.reserved_bytes),
            )
            .fold(0usize, usize::saturating_add)
    }

    fn reclaim_unreferenced_images(&mut self, preserve_id: u32) {
        let before = self.images.len();
        self.images.retain(|id, _| {
            *id == preserve_id
                || self
                    .placements
                    .iter()
                    .any(|placement| placement.image_id == *id)
        });
        if self.images.len() != before {
            self.graphics_generation = self.graphics_generation.saturating_add(1);
            self.image_generation = self.image_generation.saturating_add(1);
        }
    }

    fn append_transfer(
        &mut self,
        id: u32,
        transfer: &mut KittyTransfer,
        encoded: &[u8],
        values: &HashMap<&str, &str>,
    ) -> Result<(), &'static str> {
        if values.get("t").is_some_and(|medium| *medium != "d") {
            return Err("ENOTSUP:only direct transmission is supported");
        }
        if values
            .get("o")
            .is_some_and(|compression| *compression != "z")
        {
            return Err("ENOTSUP:unsupported compression");
        }
        let expected = match transfer.format {
            24 | 32 => {
                let rgba = decoded_image_size(transfer.width, transfer.height)?;
                Some(rgba / 4 * usize::from(transfer.format / 8))
            }
            100 => None,
            _ => return Err("ENOTSUP:unsupported image format"),
        };
        let pending = transfer
            .bytes
            .len()
            .saturating_add(decoded_base64_size(encoded));
        // Raw image dimensions reserve the entire final image before decoding the
        // first chunk, so later transfers cannot steal its completion capacity.
        let reservation = base64_size(pending.max(expected.unwrap_or(0)));
        if self.graphics_bytes().saturating_add(reservation) > self.graphics_byte_limit
            || self.images.len() >= MAX_GRAPHICS_IMAGES
        {
            self.reclaim_unreferenced_images(id);
        }
        if self.graphics_byte_limit == 0
            || self.graphics_bytes().saturating_add(reservation) > self.graphics_byte_limit
            || self.transfers.len() >= MAX_GRAPHICS_TRANSFERS
            || (!self.images.contains_key(&id)
                && self.images.len() + self.transfers.len() >= MAX_GRAPHICS_IMAGES)
            || transfer
                .placement
                .as_ref()
                .is_some_and(|placement| !self.has_placement_capacity(placement))
        {
            return Err("ENOSPC:graphics limit exceeded");
        }
        if !transfer.compressed && expected.is_some_and(|length| pending > length) {
            return Err("EINVAL:raw image byte count exceeds dimensions");
        }
        transfer.reserved_bytes = reservation;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "EINVAL:invalid base64 image payload")?;
        let capacity = if transfer.compressed {
            pending
        } else {
            expected.unwrap_or(pending)
        };
        transfer
            .bytes
            .try_reserve_exact(capacity.saturating_sub(transfer.bytes.len()))
            .map_err(|_| "ENOMEM:unable to allocate image transfer")?;
        transfer.bytes.extend_from_slice(&decoded);
        Ok(())
    }

    fn finish_transfer(
        &mut self,
        id: u32,
        mut transfer: KittyTransfer,
    ) -> Result<(), &'static str> {
        let available = self
            .graphics_byte_limit
            .saturating_sub(self.graphics_bytes());
        let raw_limit = available / 4 * 3;
        if transfer.compressed {
            let expected = match transfer.format {
                24 | 32 => {
                    decoded_image_size(transfer.width, transfer.height)? / 4
                        * usize::from(transfer.format / 8)
                }
                _ => raw_limit.min(MAX_DECODED_IMAGE_BYTES),
            };
            if base64_size(expected) > available {
                return Err("ENOSPC:graphics limit exceeded");
            }
            let mut decoded = Vec::new();
            ZlibDecoder::new(&transfer.bytes[..])
                .take(expected.saturating_add(1) as u64)
                .read_to_end(&mut decoded)
                .map_err(|_| "EINVAL:invalid compressed image")?;
            if decoded.len() > expected {
                return Err("ENOSPC:decompressed image exceeds limit");
            }
            transfer.bytes = decoded;
        }
        if transfer.format == 100 {
            // Validate PNG dimensions before any client image decoder is invoked.
            if transfer.bytes.len() < 24
                || &transfer.bytes[..8] != b"\x89PNG\r\n\x1a\n"
                || &transfer.bytes[12..16] != b"IHDR"
            {
                return Err("EINVAL:invalid PNG header");
            }
            transfer.width = u32::from_be_bytes(transfer.bytes[16..20].try_into().unwrap());
            transfer.height = u32::from_be_bytes(transfer.bytes[20..24].try_into().unwrap());
            decoded_image_size(transfer.width, transfer.height)?;
        } else {
            let expected = decoded_image_size(transfer.width, transfer.height)? / 4
                * usize::from(transfer.format / 8);
            if transfer.bytes.len() != expected {
                return Err("EINVAL:raw image byte count does not match dimensions");
            }
        }
        if base64_size(transfer.bytes.len()) > available {
            return Err("ENOSPC:graphics limit exceeded");
        }
        let placement = transfer.placement;
        if placement
            .as_ref()
            .is_some_and(|placement| !self.has_placement_capacity(placement))
        {
            return Err("ENOSPC:placement limit exceeded");
        }
        self.images.insert(
            id,
            KittyImage {
                id,
                format: transfer.format,
                width: transfer.width,
                height: transfer.height,
                data: base64::engine::general_purpose::STANDARD
                    .encode(&transfer.bytes)
                    .into(),
            },
        );
        self.graphics_generation = self.graphics_generation.saturating_add(1);
        self.image_generation = self.image_generation.saturating_add(1);
        if let Some(placement) = placement {
            self.push_placement(placement);
        }
        Ok(())
    }

    fn has_placement_capacity(&self, placement: &KittyPlacement) -> bool {
        self.placements.len()
            + self
                .transfers
                .values()
                .filter(|transfer| transfer.placement.is_some())
                .count()
            < MAX_GRAPHICS_PLACEMENTS
            || placement.placement_id.is_some_and(|id| {
                self.placements.iter().any(|existing| {
                    existing.image_id == placement.image_id && existing.placement_id == Some(id)
                })
            })
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
        if before.0 != self.images.len() {
            self.image_generation = self.image_generation.saturating_add(1);
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
            ('m', b"") => self.sgr(params),
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

type ReflowResult = (Option<(usize, usize)>, Vec<Option<(i32, u16)>>);
type LogicalLine = (Vec<Cell>, Option<usize>, Vec<(usize, usize)>);

fn reflow_main_buffer(
    buffer: &mut Buffer,
    cols: usize,
    rows: usize,
    cursor: Option<(usize, usize)>,
    anchors: &[(usize, usize)],
) -> ReflowResult {
    let mut source: Vec<Row> = buffer
        .scrollback
        .drain(..)
        .chain(buffer.rows.drain(..))
        .collect();
    if cursor.is_some() || !anchors.is_empty() {
        let blank = Cell::default();
        let last_content_row = source
            .iter()
            .rposition(|row| row.wrapped || row.cells.iter().any(|cell| cell != &blank));
        let last_cursor_row = cursor.map(|(row, _)| row);
        let last_anchor_row = anchors.iter().map(|(row, _)| *row).max();
        let last_used_row = [last_content_row, last_cursor_row, last_anchor_row]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(0);
        source.truncate(last_used_row.saturating_add(1).min(source.len()));
    }
    let mut anchors_by_row = BTreeMap::<usize, Vec<(usize, usize)>>::new();
    for (index, (row, col)) in anchors.iter().copied().enumerate() {
        anchors_by_row.entry(row).or_default().push((index, col));
    }
    let mut logical_lines: Vec<LogicalLine> = Vec::new();
    let mut line_cells = Vec::new();
    let mut line_cursor = None;
    let mut line_anchors = Vec::new();
    for (row_index, row) in source.into_iter().enumerate() {
        if let Some((cursor_row, cursor_col)) = cursor
            && cursor_row == row_index
        {
            line_cursor =
                Some(line_cells.len() + cursor_col.min(row.cells.len().saturating_sub(1)));
        }
        if let Some(row_anchors) = anchors_by_row.remove(&row_index) {
            for (index, col) in row_anchors {
                line_anchors.push((
                    index,
                    line_cells.len() + col.min(row.cells.len().saturating_sub(1)),
                ));
            }
        }
        line_cells.extend(row.cells);
        if !row.wrapped {
            trim_reflow_line(&mut line_cells, line_cursor, &line_anchors);
            line_anchors.sort_by_key(|(_, offset)| *offset);
            logical_lines.push((
                std::mem::take(&mut line_cells),
                line_cursor.take(),
                std::mem::take(&mut line_anchors),
            ));
        }
    }
    if !line_cells.is_empty() || logical_lines.is_empty() {
        trim_reflow_line(&mut line_cells, line_cursor, &line_anchors);
        line_anchors.sort_by_key(|(_, offset)| *offset);
        logical_lines.push((line_cells, line_cursor, line_anchors));
    }

    let mut physical_rows = Vec::new();
    let mut mapped_cursor = None;
    let mut mapped_anchors = vec![None; anchors.len()];
    for (cells, cursor_offset, line_anchors) in logical_lines {
        let line_start = physical_rows.len();
        let mut row = Row::blank(cols);
        let mut col = 0;
        let mut source_col = 0;
        let mut next_anchor = 0;
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
            while let Some((index, offset)) = line_anchors.get(next_anchor).copied()
                && offset < source_col.saturating_add(width)
            {
                if offset >= source_col {
                    mapped_anchors[index] =
                        Some((physical_rows.len(), col + offset.saturating_sub(source_col)));
                }
                next_anchor += 1;
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
        while let Some((index, offset)) = line_anchors.get(next_anchor).copied() {
            if offset == cells.len() {
                mapped_anchors[index] =
                    Some((physical_rows.len(), col.min(cols.saturating_sub(1))));
            }
            next_anchor += 1;
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
    let mut removed_rows = 0;
    while buffer.scrollback_bytes > MAX_SCROLLBACK_BYTES
        || buffer.scrollback.len() > buffer.scrollback_line_limit
    {
        let Some(removed) = buffer.scrollback.pop_front() else {
            break;
        };
        removed_rows += 1;
        buffer.scrollback_bytes = buffer.scrollback_bytes.saturating_sub(row_memory(&removed));
    }
    let mapped_anchors = mapped_anchors
        .into_iter()
        .map(|mapped| {
            let (row, col) = mapped?;
            if row < removed_rows {
                return None;
            }
            let relative = i64::try_from(row).ok()? - i64::try_from(viewport_start).ok()?;
            Some((
                i32::try_from(relative).ok()?,
                u16::try_from(col.min(cols.saturating_sub(1))).ok()?,
            ))
        })
        .collect();
    (
        mapped_cursor.map(|(row, col)| (row.saturating_sub(viewport_start), col)),
        mapped_anchors,
    )
}

fn trim_reflow_line(cells: &mut Vec<Cell>, cursor: Option<usize>, anchors: &[(usize, usize)]) {
    let minimum = cursor
        .into_iter()
        .chain(anchors.iter().map(|(_, offset)| *offset))
        .map(|offset| offset.saturating_add(1))
        .max()
        .unwrap_or(0);
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

fn base64_size(bytes: usize) -> usize {
    bytes.saturating_add(2) / 3 * 4
}

fn decoded_image_size(width: u32, height: u32) -> Result<usize, &'static str> {
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    match bytes {
        Some(bytes) if bytes > 0 && bytes <= MAX_DECODED_IMAGE_BYTES => Ok(bytes),
        _ => Err("E2BIG:decoded image dimensions exceed limit"),
    }
}

fn decoded_base64_size(encoded: &[u8]) -> usize {
    let padding = encoded
        .iter()
        .rev()
        .take(2)
        .take_while(|byte| **byte == b'=')
        .count();
    (encoded.len().saturating_add(3) / 4 * 3).saturating_sub(padding)
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

    fn visible_text(snapshot: &Snapshot, row: usize) -> String {
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
    fn keyboard_option_commands_preserve_text_rendition() {
        let mut terminal = TerminalState::new(12, 2);
        terminal.advance(b"\x1b[1;31mA\x1b[>4;2mB\x1b[>4;0mC\x1b[?4mD\x1b[0mE");
        let snapshot = terminal.snapshot();
        assert_eq!(visible_text(&snapshot, 0), "ABCDE");
        for cell in &snapshot.cells[0].cells[..4] {
            assert_eq!(cell.foreground, Color::Indexed(1));
            assert!(cell.attributes.bold);
            assert!(!cell.attributes.dim);
            assert!(!cell.attributes.underline);
        }
        assert_eq!(snapshot.cells[0].cells[4].foreground, Color::Default);
        assert!(!snapshot.cells[0].cells[4].attributes.bold);
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
        assert!(narrow.cells.first().is_some_and(|row| row.wrapped));

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
    fn reanchors_kitty_placements_to_logical_text_during_reflow() {
        let mut terminal = TerminalState::new(8, 3);
        terminal.advance(b"abcdef");
        terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=7,p=9;AQIDBA==\x1b\\");
        assert_eq!(
            (
                terminal.snapshot().placements[0].row,
                terminal.snapshot().placements[0].col
            ),
            (0, 6)
        );

        terminal.resize(4, 3);
        assert_eq!(
            (
                terminal.snapshot().placements[0].row,
                terminal.snapshot().placements[0].col
            ),
            (1, 2)
        );

        terminal.resize(10, 3);
        assert_eq!(
            (
                terminal.snapshot().placements[0].row,
                terminal.snapshot().placements[0].col
            ),
            (0, 6)
        );
    }

    #[test]
    fn height_resizes_do_not_turn_unused_rows_into_scrollback() {
        let mut terminal = TerminalState::new(20, 24);
        terminal.advance(b"prompt");

        for rows in [40, 12, 30, 8, 24] {
            terminal.resize(20, rows);
        }

        let snapshot = terminal.snapshot();
        assert!(snapshot.scrollback.is_empty());
        assert_eq!(visible_text(&snapshot, 0), "prompt");
        assert_eq!((snapshot.cursor.row, snapshot.cursor.col), (0, 6));
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
    fn rejects_unimplemented_kitty_controls_instead_of_claiming_support() {
        let mut terminal = TerminalState::new(10, 2);
        let (_, replies) = terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=7,X=1;AQIDBA==\x1b\\");
        assert!(terminal.snapshot().images.is_empty());
        assert_eq!(replies.len(), 1);
        assert!(String::from_utf8_lossy(&replies[0]).contains("ENOTSUP"));
        assert_eq!(terminal.diagnostics.counts.get("Kitty:X"), Some(&1));
    }

    #[test]
    fn records_sixel_as_an_unsupported_streaming_dcs() {
        let mut terminal = TerminalState::new(10, 2);
        terminal.advance(b"\x1bPq~\x1b\\");
        assert_eq!(terminal.diagnostics.counts.get("DCS:q"), Some(&1));
        assert!(terminal.snapshot().images.is_empty());
    }

    #[test]
    fn replies_to_terminal_status_queries() {
        let mut terminal = TerminalState::new(10, 2);
        terminal.advance(b"abc");
        let (_, replies) = terminal.advance(b"\x1b[6n");
        assert_eq!(replies, vec![b"\x1b[1;4R".to_vec()]);
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
        assert_eq!(&*snapshot.images[0].data, "AQIDBA==");
        assert_eq!(
            (snapshot.images[0].width, snapshot.images[0].height),
            (1, 1)
        );
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
        assert_eq!(&*compressed.data, "AQIDBA==");
    }

    #[test]
    fn configured_history_limit_and_clear_preserve_live_canvas() {
        let mut terminal = TerminalState::new(16, 2);
        terminal.set_resource_limits(2, MAX_GRAPHICS_BYTES);
        terminal.advance(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let before = terminal.snapshot();
        assert_eq!(
            before.scrollback.iter().map(row_text).collect::<Vec<_>>(),
            ["two", "three"]
        );
        terminal.clear_scrollback();
        let cleared = terminal.snapshot();
        assert!(cleared.scrollback.is_empty());
        assert_eq!(cleared.cells, before.cells);
        assert_eq!(cleared.cursor, before.cursor);
        assert!(cleared.sequence > before.sequence);
        terminal.advance(b"\r\nsix");
        assert_eq!(
            terminal
                .snapshot()
                .scrollback
                .iter()
                .map(row_text)
                .collect::<Vec<_>>(),
            ["four"]
        );
    }

    #[test]
    fn cap_rejection_preserves_existing_image_and_placement() {
        let mut terminal = TerminalState::new(10, 2);
        terminal.set_resource_limits(10, 8);
        terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=8,m=1;AQI=\x1b\\");
        terminal.advance(b"\x1b_Gm=0;AwQ=\x1b\\");
        let before = terminal.snapshot();
        assert_eq!(&*before.images[0].data, "AQIDBA==");
        for command in [
            b"\x1b_Ga=T,f=32,s=1,v=1,i=9;AQIDBA==\x1b\\".as_slice(),
            b"\x1b_Ga=T,f=32,s=1,v=1,i=8;BQYHCA==\x1b\\".as_slice(),
        ] {
            let (_, replies) = terminal.advance(command);
            assert_eq!(replies.len(), 1);
            assert!(String::from_utf8_lossy(&replies[0]).contains(";ENOSPC:"));
            let after = terminal.snapshot();
            assert_eq!(after.images, before.images);
            assert_eq!(after.placements, before.placements);
        }
    }

    #[test]
    fn decompression_respects_other_pending_graphics_transfers() {
        use std::io::Write;
        let data = vec![1_u8; 800];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
        let mut compressed =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        compressed.write_all(&data).unwrap();
        let compressed =
            base64::engine::general_purpose::STANDARD.encode(compressed.finish().unwrap());
        let mut terminal = TerminalState::new(10, 2);
        terminal.set_resource_limits(10, 1200);
        terminal.advance(format!("\x1b_Ga=t,f=32,s=200,v=1,i=1,m=1;{encoded}\x1b\\").as_bytes());
        let (_, replies) = terminal
            .advance(format!("\x1b_Ga=t,f=32,s=200,v=1,i=2,o=z;{compressed}\x1b\\").as_bytes());
        assert!(String::from_utf8_lossy(&replies[0]).starts_with("\x1b_Gi=2;ENOSPC:"));
        assert!(terminal.snapshot().images.is_empty());
        terminal.advance(b"\x1b_Gi=1,m=0;\x1b\\");
        assert_eq!(
            terminal
                .snapshot()
                .images
                .iter()
                .map(|image| image.id)
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(terminal.snapshot().images[0].data.as_ref(), encoded);
    }

    #[test]
    fn generated_image_id_never_overwrites_an_explicit_image() {
        let mut terminal = TerminalState::new(10, 2);
        terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AQIDBA==\x1b\\");
        let original = terminal.snapshot().images[0].clone();
        terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1;BQYHCA==\x1b\\");
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.images[0], original);
        assert_eq!(snapshot.images[1].id, 2);
        assert_eq!(
            snapshot
                .placements
                .iter()
                .map(|placement| placement.image_id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn transfer_and_placement_counts_reject_without_stealing_reservations() {
        let mut terminal = TerminalState::new(10, 2);
        for id in 1..=MAX_GRAPHICS_TRANSFERS {
            terminal.advance(format!("\x1b_Ga=t,f=32,s=1,v=1,i={id},m=1;AQI=\x1b\\").as_bytes());
        }
        let (_, replies) = terminal.advance(b"\x1b_Ga=t,f=32,s=1,v=1,i=100,m=1;AQI=\x1b\\");
        assert!(String::from_utf8_lossy(&replies[0]).contains(";ENOSPC:"));
        for id in 1..=MAX_GRAPHICS_TRANSFERS {
            terminal.advance(format!("\x1b_Gi={id},m=0;AwQ=\x1b\\").as_bytes());
        }
        assert_eq!(terminal.snapshot().images.len(), MAX_GRAPHICS_TRANSFERS);
        for _ in 0..MAX_GRAPHICS_PLACEMENTS {
            terminal.advance(b"\x1b_Ga=p,i=1\x1b\\");
        }
        let before = terminal.snapshot();
        let (_, replies) = terminal.advance(b"\x1b_Ga=p,i=1\x1b\\");
        assert!(String::from_utf8_lossy(&replies[0]).contains(";ENOSPC:"));
        assert_eq!(terminal.snapshot().placements, before.placements);
        assert_eq!(terminal.snapshot().images, before.images);
    }

    #[test]
    fn decompression_overflow_preserves_committed_pixels_and_releases_transfer() {
        use std::io::Write;
        let mut terminal = TerminalState::new(10, 2);
        terminal.set_resource_limits(10, 128);
        terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AQIDBA==\x1b\\");
        let before = terminal.snapshot();
        let mut compressed =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        compressed.write_all(&[0; 4096]).unwrap();
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(compressed.finish().unwrap());
        let (_, replies) =
            terminal.advance(format!("\x1b_Ga=T,f=32,s=1,v=1,i=2,o=z;{encoded}\x1b\\").as_bytes());
        assert!(String::from_utf8_lossy(&replies[0]).starts_with("\x1b_Gi=2;ENOSPC:"));
        assert_eq!(terminal.snapshot().images, before.images);
        assert_eq!(terminal.snapshot().placements, before.placements);
        terminal.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=2;BQYHCA==\x1b\\");
        assert_eq!(terminal.snapshot().images[1].data.as_ref(), "BQYHCA==");
    }
}
