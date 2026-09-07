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
}
