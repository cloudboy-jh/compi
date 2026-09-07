use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalFrame {
    pub identity: crate::TerminalIdentity,
    pub message: ScreenMessage,
}

pub fn encode_screen(message: &ScreenMessage) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(message, bincode::config::standard())
}

pub fn decode_screen(payload: &[u8]) -> Result<ScreenMessage, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(payload, bincode::config::standard())
        .map(|(message, _)| message)
}

pub fn encode_terminal_frame(
    frame: &TerminalFrame,
) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(frame, bincode::config::standard())
}

pub fn decode_terminal_frame(payload: &[u8]) -> Result<TerminalFrame, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(payload, bincode::config::standard()).map(|(frame, _)| frame)
}
