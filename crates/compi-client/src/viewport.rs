use compi_protocol::{Row, ScreenSnapshot};

use crate::selection::GridPoint;

pub fn row_at(snapshot: &ScreenSnapshot, row: usize) -> Option<&Row> {
    if row < snapshot.scrollback.len() {
        snapshot.scrollback.get(row)
    } else {
        snapshot.cells.get(row - snapshot.scrollback.len())
    }
}

pub fn visible_base(snapshot: &ScreenSnapshot, scroll_offset: usize) -> usize {
    snapshot.scrollback.len() - scroll_offset.min(snapshot.scrollback.len())
}

pub fn visible_rows(
    snapshot: &ScreenSnapshot,
    scroll_offset: usize,
) -> (impl Iterator<Item = &Row>, usize) {
    let base = visible_base(snapshot, scroll_offset);
    let rows = snapshot
        .scrollback
        .iter()
        .chain(&snapshot.cells)
        .skip(base)
        .take(snapshot.cells.len());
    (rows, base)
}

pub fn visible_to_absolute(
    snapshot: Option<&ScreenSnapshot>,
    scroll_offset: usize,
    point: GridPoint,
) -> Option<GridPoint> {
    let snapshot = snapshot?;
    Some(GridPoint {
        row: visible_base(snapshot, scroll_offset) + point.row,
        col: point.col.min(snapshot.cols.saturating_sub(1) as usize),
    })
}

pub fn visible_row(rows: i16, row: usize) -> usize {
    row.min(rows.saturating_sub(1) as usize)
}

pub fn hyperlink_at(snapshot: Option<&ScreenSnapshot>, point: GridPoint) -> Option<&str> {
    row_at(snapshot?, point.row)?
        .cells
        .get(point.col)?
        .hyperlink
        .as_deref()
}

pub fn web_link_at(snapshot: Option<&ScreenSnapshot>, point: GridPoint) -> Option<String> {
    let snapshot = snapshot?;
    if let Some(uri) = hyperlink_at(Some(snapshot), point).filter(|uri| is_allowed_hyperlink(uri)) {
        return Some(uri.to_owned());
    }

    let row_count = snapshot.scrollback.len() + snapshot.cells.len();
    if point.row >= row_count {
        return None;
    }
    let mut first_row = point.row;
    while first_row > 0 && row_at(snapshot, first_row - 1).is_some_and(|row| row.wrapped) {
        first_row -= 1;
    }
    let mut last_row = point.row;
    while last_row + 1 < row_count && row_at(snapshot, last_row).is_some_and(|row| row.wrapped) {
        last_row += 1;
    }

    let mut line = String::new();
    let mut clicked = None;
    for row_index in first_row..=last_row {
        let row = row_at(snapshot, row_index)?;
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.width == 0 {
                continue;
            }
            let start = line.len();
            line.push_str(&cell.text);
            if row_index == point.row && col == point.col {
                clicked = Some(start..line.len());
            }
        }
    }

    let clicked = clicked?;
    let token_start = line[..clicked.start]
        .rfind(char::is_whitespace)
        .map_or(0, |index| index + 1);
    let token_end = line[clicked.end..]
        .find(char::is_whitespace)
        .map_or(line.len(), |index| clicked.end + index);
    let token = &line[token_start..token_end];
    let lowercase = token.to_ascii_lowercase();
    let scheme_start = lowercase
        .find("https://")
        .or_else(|| lowercase.find("http://"))?;
    let candidate = token[scheme_start..]
        .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"']);
    let link_start = token_start + scheme_start;
    let link_end = link_start + candidate.len();
    (clicked.start >= link_start && clicked.start < link_end && is_allowed_hyperlink(candidate))
        .then(|| candidate.to_owned())
}

pub fn is_allowed_hyperlink(uri: &str) -> bool {
    if uri
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return false;
    }
    ["https://", "http://"].into_iter().any(|prefix| {
        uri.get(..prefix.len())
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case(prefix))
            && uri
                .get(prefix.len()..)
                .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
    })
}

pub fn inherited_working_directory(snapshot: Option<&ScreenSnapshot>) -> Option<String> {
    snapshot.and_then(|snapshot| snapshot.current_directory.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_only_web_hyperlinks() {
        assert!(is_allowed_hyperlink("https://example.com/docs?q=1"));
        assert!(is_allowed_hyperlink("HTTP://localhost:8080/"));
        assert!(!is_allowed_hyperlink("file:///etc/passwd"));
        assert!(!is_allowed_hyperlink("javascript:alert(1)"));
        assert!(!is_allowed_hyperlink("https:///missing-host"));
        assert!(!is_allowed_hyperlink("https://example.com/\nheader"));
    }

    fn row(text: &str, wrapped: bool) -> Row {
        Row {
            cells: text
                .chars()
                .map(|character| compi_protocol::Cell {
                    text: character.to_string().into(),
                    ..Default::default()
                })
                .collect(),
            wrapped,
        }
    }

    fn snapshot(rows: Vec<Row>) -> ScreenSnapshot {
        ScreenSnapshot {
            sequence: 1,
            cols: rows.first().map_or(0, |row| row.cells.len()) as u16,
            rows: rows.len() as u16,
            cells: rows,
            scrollback: Vec::new(),
            cursor: Default::default(),
            modes: Default::default(),
            title: String::new(),
            current_directory: None,
            images: Vec::new(),
            placements: Vec::new(),
        }
    }

    #[test]
    fn detects_plain_web_links_across_wrapped_rows() {
        let snapshot = snapshot(vec![
            row("Sign in: https://exam", true),
            row("ple.com/oauth?a=1).", false),
        ]);
        assert_eq!(
            web_link_at(Some(&snapshot), GridPoint { row: 0, col: 12 }).as_deref(),
            Some("https://example.com/oauth?a=1")
        );
        assert_eq!(
            web_link_at(Some(&snapshot), GridPoint { row: 1, col: 5 }).as_deref(),
            Some("https://example.com/oauth?a=1")
        );
        assert!(web_link_at(Some(&snapshot), GridPoint { row: 0, col: 2 }).is_none());
    }
}
