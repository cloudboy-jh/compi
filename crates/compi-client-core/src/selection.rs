use compi_protocol::{Cell, ScreenSnapshot};

use crate::viewport::row_at;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridPoint {
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub anchor: GridPoint,
    pub head: GridPoint,
}

impl Selection {
    pub fn ordered(self) -> (GridPoint, GridPoint) {
        if (self.anchor.row, self.anchor.col) <= (self.head.row, self.head.col) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CtrlCBehavior {
    Copy(String),
    Interrupt,
}

pub fn selected_text(
    snapshot: Option<&ScreenSnapshot>,
    selection: Option<Selection>,
) -> Option<String> {
    let selection = selection?;
    let snapshot = snapshot?;
    let (start, end) = selection.ordered();
    let row_count = snapshot.scrollback.len() + snapshot.cells.len();
    if start == end || start.row >= row_count {
        return None;
    }
    let mut result = String::new();
    let last_row = end.row.min(row_count - 1);
    for (row_index, row) in snapshot
        .scrollback
        .iter()
        .chain(&snapshot.cells)
        .enumerate()
        .take(last_row + 1)
        .skip(start.row)
    {
        let first = if row_index == start.row { start.col } else { 0 };
        let last = if row_index == end.row {
            end.col.saturating_add(1)
        } else {
            row.cells.len()
        }
        .min(row.cells.len());
        for cell in &row.cells[first.min(last)..last] {
            if cell.width > 0 {
                result.push_str(&cell.text);
            }
        }
        while result.ends_with(' ') {
            result.pop();
        }
        if row_index != end.row && !row.wrapped {
            result.push('\n');
        }
    }
    (!result.is_empty()).then_some(result)
}

pub fn ctrl_c_behavior(
    snapshot: Option<&ScreenSnapshot>,
    selection: Option<Selection>,
) -> CtrlCBehavior {
    selected_text(snapshot, selection)
        .map(CtrlCBehavior::Copy)
        .unwrap_or(CtrlCBehavior::Interrupt)
}

pub fn word_selection(snapshot: Option<&ScreenSnapshot>, point: GridPoint) -> Option<Selection> {
    let snapshot = snapshot?;
    let row = row_at(snapshot, point.row)?;
    let is_word = |cell: &Cell| cell.width > 0 && !cell.text.chars().all(char::is_whitespace);
    let mut start = point.col.min(row.cells.len().saturating_sub(1));
    let mut end = start;
    while start > 0 && is_word(&row.cells[start - 1]) {
        start -= 1;
    }
    while end + 1 < row.cells.len() && is_word(&row.cells[end + 1]) {
        end += 1;
    }
    Some(Selection {
        anchor: GridPoint {
            row: point.row,
            col: start,
        },
        head: GridPoint {
            row: point.row,
            col: end,
        },
    })
}

pub fn line_selection(snapshot: Option<&ScreenSnapshot>, point: GridPoint) -> Option<Selection> {
    let snapshot = snapshot?;
    let row = row_at(snapshot, point.row)?;
    Some(Selection {
        anchor: GridPoint {
            row: point.row,
            col: 0,
        },
        head: GridPoint {
            row: point.row,
            col: row.cells.len().saturating_sub(1),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::inherited_working_directory;
    use compi_protocol::{Color, CursorState, Row, TerminalModes, TextAttributes};

    fn row(text: &str, wrapped: bool) -> Row {
        Row {
            cells: text
                .chars()
                .map(|character| Cell {
                    text: character.to_string().into(),
                    width: 1,
                    foreground: Color::Default,
                    background: Color::Default,
                    attributes: TextAttributes::default(),
                    hyperlink: None,
                })
                .collect(),
            wrapped,
        }
    }

    fn snapshot(scrollback: Vec<Row>, cells: Vec<Row>) -> ScreenSnapshot {
        ScreenSnapshot {
            sequence: 1,
            cols: 4,
            rows: cells.len() as u16,
            cells,
            scrollback,
            cursor: CursorState::default(),
            modes: TerminalModes::default(),
            title: String::new(),
            current_directory: None,
            images: Vec::new(),
            placements: Vec::new(),
        }
    }

    #[test]
    fn inherits_current_directory_from_active_screen_state() {
        let mut screen = snapshot(Vec::new(), vec![row("", false)]);
        screen.current_directory = Some("/home/dev/project".to_owned());
        assert_eq!(
            inherited_working_directory(Some(&screen)),
            Some("/home/dev/project".to_owned())
        );
        assert_eq!(inherited_working_directory(None), None);
    }

    #[test]
    fn extracts_wrapped_and_multiline_selection() {
        let snapshot = snapshot(vec![row("abcd", true)], vec![row("efgh", false)]);
        let selection = Some(Selection {
            anchor: GridPoint { row: 0, col: 1 },
            head: GridPoint { row: 1, col: 2 },
        });
        assert_eq!(
            selected_text(Some(&snapshot), selection).as_deref(),
            Some("bcdefg")
        );
        assert_eq!(
            ctrl_c_behavior(Some(&snapshot), selection),
            CtrlCBehavior::Copy("bcdefg".to_owned())
        );
        assert_eq!(
            ctrl_c_behavior(Some(&snapshot), None),
            CtrlCBehavior::Interrupt
        );
    }
}
