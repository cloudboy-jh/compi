use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::sync::Arc;

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
    pub fn blank(cols: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols],
            wrapped: false,
        }
    }
}

/// A history row kept compactly: its cells' text joined, cut after the last cell that is
/// not a default blank, with the cells' styles as runs. [`HistoryRow::to_row`] rebuilds
/// the full row at any width that holds it. Screen frames are bincode, which is not
/// self-describing, so no field may be skipped when empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct HistoryRow {
    /// Every kept cell's text, in order.
    pub text: String,
    /// Each kept cell's text length in bytes and its width, packed `(len, width)`; empty
    /// when every kept cell is one byte wide by one column, so `text.len()` cells.
    pub cells: Vec<(u16, u8)>,
    /// The kept cells' styles, as runs covering them in order.
    pub styles: Vec<StyleRun>,
    pub wrapped: bool,
}

/// `cells` consecutive cells sharing one style.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StyleRun {
    pub cells: u16,
    pub foreground: Color,
    pub background: Color,
    pub attributes: TextAttributes,
    pub hyperlink: Option<SmolStr>,
}

impl StyleRun {
    fn of(cell: &Cell) -> Self {
        Self {
            cells: 1,
            foreground: cell.foreground,
            background: cell.background,
            attributes: cell.attributes.clone(),
            hyperlink: cell.hyperlink.clone(),
        }
    }

    fn styles(&self, cell: &Cell) -> bool {
        self.foreground == cell.foreground
            && self.background == cell.background
            && self.attributes == cell.attributes
            && self.hyperlink == cell.hyperlink
    }
}

impl HistoryRow {
    /// `row` without its trailing default blank cells.
    pub fn from_row(row: &Row) -> Self {
        let default = Cell::default();
        let kept = row
            .cells
            .iter()
            .rposition(|cell| *cell != default)
            .map_or(0, |last| last + 1);
        let cells = &row.cells[..kept];
        let mut text = String::with_capacity(cells.len());
        let mut lengths = Vec::new();
        let mut simple = true;
        let mut styles: Vec<StyleRun> = Vec::new();
        for (index, cell) in cells.iter().enumerate() {
            // A cell's text is a grapheme; one past `u16::MAX` bytes is cut at a character.
            let mut kept_text = cell.text.as_str();
            while kept_text.len() > usize::from(u16::MAX) {
                let mut end = usize::from(u16::MAX);
                while !kept_text.is_char_boundary(end) {
                    end -= 1;
                }
                kept_text = &kept_text[..end];
            }
            text.push_str(kept_text);
            let len = u16::try_from(kept_text.len()).expect("cut to u16 above");
            if simple && (len != 1 || cell.width != 1) {
                simple = false;
                lengths = vec![(1, 1); index];
            }
            if !simple {
                lengths.push((len, cell.width));
            }
            match styles.last_mut() {
                Some(run) if run.cells < u16::MAX && run.styles(cell) => run.cells += 1,
                _ => styles.push(StyleRun::of(cell)),
            }
        }
        // One run of default style says nothing the absence of runs does not.
        if let [run] = styles.as_slice()
            && run.styles(&default)
        {
            styles.clear();
        }
        text.shrink_to_fit();
        lengths.shrink_to_fit();
        styles.shrink_to_fit();
        Self {
            text,
            cells: lengths,
            styles,
            wrapped: row.wrapped,
        }
    }

    /// How many cells are kept.
    pub fn len(&self) -> usize {
        if self.cells.is_empty() {
            self.text.len()
        } else {
            self.cells.len()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The full row, `width` cells wide (or as wide as its kept cells, if wider), the
    /// dropped trailing cells default blanks again.
    pub fn to_row(&self, width: usize) -> Row {
        let kept = self.len();
        let mut cells = Vec::with_capacity(kept.max(width));
        let mut runs = self.styles.iter();
        let mut run = runs.next();
        let mut left_in_run = run.map_or(usize::MAX, |run| usize::from(run.cells));
        let mut offset = 0;
        for index in 0..kept {
            let (len, width) = self
                .cells
                .get(index)
                .map_or((1, 1), |(len, width)| (usize::from(*len), *width));
            let text = self.text.get(offset..offset + len).unwrap_or_default();
            offset += len;
            let mut cell = Cell {
                text: text.into(),
                width,
                ..Cell::default()
            };
            if let Some(style) = run {
                cell.foreground = style.foreground;
                cell.background = style.background;
                cell.attributes = style.attributes.clone();
                cell.hyperlink = style.hyperlink.clone();
                left_in_run -= 1;
                if left_in_run == 0 {
                    run = runs.next();
                    left_in_run = run.map_or(usize::MAX, |run| usize::from(run.cells));
                }
            }
            cells.push(cell);
        }
        cells.resize(kept.max(width), Cell::default());
        Row {
            cells,
            wrapped: self.wrapped,
        }
    }

    /// The bytes this row holds on the heap and in itself: what a history budget counts.
    pub fn memory(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.text.capacity()
            + self.cells.capacity() * std::mem::size_of::<(u16, u8)>()
            + self.styles.capacity() * std::mem::size_of::<StyleRun>()
            + self
                .styles
                .iter()
                .filter_map(|run| run.hyperlink.as_ref())
                .map(|uri| {
                    if uri.is_heap_allocated() {
                        uri.len()
                    } else {
                        0
                    }
                })
                .sum::<usize>()
    }
}

/// How a delta changes the history: rows scrolled in at the end after dropping
/// `evicted` rows from the front of the history the client holds, or a whole new history
/// (a real clear, a reset, or a resize that rewrapped it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScrollbackChange {
    Append { evicted: u32, rows: Vec<HistoryRow> },
    Replace { rows: Vec<HistoryRow> },
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
    /// Kitty keyboard protocol flags negotiated by the foreground application.
    #[serde(default)]
    pub keyboard_protocol_flags: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KittyImage {
    pub id: u32,
    pub format: u16,
    pub width: u32,
    pub height: u32,
    pub data: Arc<str>,
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
    pub scrollback: std::collections::VecDeque<HistoryRow>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ShellAction {
    BrowseFiles,
    JumpProject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenDelta {
    pub sequence: u64,
    pub cols: u16,
    pub rows: u16,
    pub row_updates: Vec<RowUpdate>,
    pub scrollback: Option<ScrollbackChange>,
    pub cursor: CursorState,
    pub modes: TerminalModes,
    pub title: String,
    pub current_directory: Option<String>,
    pub images: Option<Vec<KittyImage>>,
    pub latency_ids: Vec<u64>,
    pub placements: Option<Vec<KittyPlacement>>,
    pub clipboard_writes: Vec<String>,
    #[serde(default)]
    pub shell_action: Option<ShellAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScreenMessage {
    Snapshot { snapshot: ScreenSnapshot },
    Delta { delta: ScreenDelta },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalFrame {
    pub identity: crate::TerminalIdentity,
    pub message: ScreenMessage,
}

pub fn encode_screen(message: &ScreenMessage) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(message, bincode::config::standard())
}

pub fn decode_screen(payload: &[u8]) -> Result<ScreenMessage, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(
        payload,
        bincode::config::standard().with_limit::<{ crate::frame::MAX_SCREEN_PAYLOAD }>(),
    )
    .map(|(message, _)| message)
}

pub fn encode_terminal_frame(
    frame: &TerminalFrame,
) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(frame, bincode::config::standard())
}

pub fn decode_terminal_frame(payload: &[u8]) -> Result<TerminalFrame, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(
        payload,
        bincode::config::standard().with_limit::<{ crate::frame::MAX_SCREEN_PAYLOAD }>(),
    )
    .map(|(frame, _)| frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(text: &str, width: u8) -> Cell {
        Cell {
            text: text.into(),
            width,
            ..Cell::default()
        }
    }

    #[test]
    fn history_changes_survive_the_binary_screen_encoding() {
        let mut styled = Row::blank(8);
        styled.cells[0] = cell("界", 2);
        styled.cells[1] = cell("", 0);
        styled.cells[2].foreground = Color::Rgb(1, 2, 3);
        styled.cells[2].hyperlink = Some("https://example.com".into());
        let mut plain = Row::blank(8);
        plain.cells[0] = cell("x", 1);
        // Rows with and without lengths and styles, and an empty one, side by side: a
        // field left out of one would shift every row after it.
        let rows: Vec<HistoryRow> = [plain, styled, Row::blank(8)]
            .iter()
            .map(HistoryRow::from_row)
            .collect();
        let delta = ScreenDelta {
            sequence: 7,
            cols: 8,
            rows: 2,
            row_updates: Vec::new(),
            scrollback: Some(ScrollbackChange::Append {
                evicted: 3,
                rows: rows.clone(),
            }),
            cursor: CursorState::default(),
            modes: TerminalModes::default(),
            title: "t".into(),
            current_directory: None,
            images: None,
            latency_ids: Vec::new(),
            placements: None,
            clipboard_writes: Vec::new(),
            shell_action: None,
        };
        let message = ScreenMessage::Delta { delta };
        let decoded = decode_screen(&encode_screen(&message).unwrap()).unwrap();
        assert_eq!(decoded, message);
        let snapshot = ScreenMessage::Snapshot {
            snapshot: ScreenSnapshot {
                sequence: 7,
                cols: 8,
                rows: 1,
                cells: vec![Row::blank(8)],
                scrollback: rows.into(),
                cursor: CursorState::default(),
                modes: TerminalModes::default(),
                title: String::new(),
                current_directory: None,
                images: Vec::new(),
                placements: Vec::new(),
            },
        };
        assert_eq!(
            decode_screen(&encode_screen(&snapshot).unwrap()).unwrap(),
            snapshot
        );
    }

    #[test]
    fn history_rows_drop_trailing_blanks_and_rebuild_exactly() {
        let mut styled = cell("b", 1);
        styled.foreground = Color::Indexed(2);
        styled.attributes.bold = true;
        let mut linked = cell("c", 1);
        linked.hyperlink = Some("https://example.com/a/rather/long/link".into());
        let mut row = Row::blank(10);
        row.cells[0] = cell("a", 1);
        row.cells[1] = styled;
        row.cells[2] = linked;
        row.wrapped = true;
        let compact = HistoryRow::from_row(&row);
        assert_eq!(compact.text, "abc");
        assert!(
            compact.cells.is_empty(),
            "plain one-byte cells need no lengths"
        );
        assert_eq!(compact.styles.len(), 3);
        assert_eq!(compact.to_row(10), row);
        // A colored blank is kept; only default blanks are dropped.
        let mut background = Row::blank(4);
        background.cells[3].background = Color::Rgb(1, 2, 3);
        assert_eq!(HistoryRow::from_row(&background).len(), 4);
        assert_eq!(HistoryRow::from_row(&background).to_row(4), background);
        // Wide and multi-byte cells keep their lengths and widths.
        let mut wide = Row::blank(6);
        wide.cells[0] = cell("é", 1);
        wide.cells[1] = cell("界", 2);
        wide.cells[2] = cell("", 0);
        wide.cells[3] = cell("x", 1);
        let compact = HistoryRow::from_row(&wide);
        assert_eq!(compact.cells, [(2, 1), (3, 2), (0, 0), (1, 1)]);
        assert!(compact.styles.is_empty());
        assert_eq!(compact.to_row(6), wide);
        // An empty row keeps nothing and rebuilds at any width.
        let blank = HistoryRow::from_row(&Row::blank(80));
        assert!(blank.is_empty() && blank.text.is_empty());
        assert_eq!(blank.to_row(80), Row::blank(80));
        assert!(blank.memory() < 100);
    }

    #[test]
    fn a_plain_line_of_text_costs_about_its_bytes() {
        let mut row = Row::blank(120);
        for (index, ch) in "cargo test --workspace -- --nocapture".chars().enumerate() {
            row.cells[index] = cell(&ch.to_string(), 1);
        }
        let compact = HistoryRow::from_row(&row);
        assert!(compact.styles.is_empty() && compact.cells.is_empty());
        assert!(compact.memory() < 37 + 96, "{}", compact.memory());
    }
}
