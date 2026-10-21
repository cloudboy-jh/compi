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
    for row_index in start.row..=last_row {
        let Some(row) = row_at(snapshot, row_index) else {
            break;
        };
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
        if row_index != end.row && !row.wrapped {
            while result.ends_with(' ') {
                result.pop();
            }
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
    use compi_protocol::{Color, CursorState, HistoryRow, Row, TerminalModes, TextAttributes};

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
            scrollback: scrollback.iter().map(HistoryRow::from_row).collect(),
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

    #[test]
    fn copies_highlighted_spaces_across_wrapped_rows() {
        let screen = snapshot(vec![row("ab c", true)], vec![row("de f", false)]);
        let selection = Some(Selection {
            anchor: GridPoint { row: 0, col: 1 },
            head: GridPoint { row: 1, col: 2 },
        });
        assert_eq!(
            selected_text(Some(&screen), selection).as_deref(),
            Some("b cde ")
        );
    }

    #[test]
    fn history_rows_select_at_the_screen_width() {
        let screen = snapshot(
            vec![row("x", false), row("ab  ", false)],
            vec![row("cd", false)],
        );
        let line = line_selection(Some(&screen), GridPoint { row: 1, col: 1 }).unwrap();
        assert_eq!(line.head, GridPoint { row: 1, col: 3 });
        let word = word_selection(Some(&screen), GridPoint { row: 1, col: 0 }).unwrap();
        assert_eq!((word.anchor.col, word.head.col), (0, 1));
        let selection = Some(Selection {
            anchor: GridPoint { row: 0, col: 0 },
            head: GridPoint { row: 2, col: 3 },
        });
        assert_eq!(
            selected_text(Some(&screen), selection).as_deref(),
            Some("x\nab\ncd")
        );
    }
}
