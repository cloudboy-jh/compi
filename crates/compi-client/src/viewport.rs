use compi_protocol::{Row, ScreenSnapshot};
use std::ops::Range;

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
    let (line, clicked) = logical_line(snapshot, point)?;
    let (token_start, token) = token_at(&line, &clicked);
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

/// The text of the line holding `point`, joined across soft wraps, and the byte range
/// of the clicked cell in it.
fn logical_line(snapshot: &ScreenSnapshot, point: GridPoint) -> Option<(String, Range<usize>)> {
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
    Some((line, clicked?))
}

/// The whitespace-delimited word of `line` around `clicked`, and where it starts.
fn token_at<'a>(line: &'a str, clicked: &Range<usize>) -> (usize, &'a str) {
    let start = line[..clicked.start]
        .rfind(char::is_whitespace)
        .map_or(0, |index| index + 1);
    let end = line[clicked.end..]
        .find(char::is_whitespace)
        .map_or(line.len(), |index| clicked.end + index);
    (start, &line[start..end])
}

/// A file path shown in a terminal, with the 1-based line to reveal, if any. The path is
/// as printed: relative paths still need the terminal's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLocation {
    pub path: String,
    pub line: Option<u32>,
}

/// The file at `point`: the path of an OSC 8 `file://` hyperlink, or a path printed as
/// plain text (see `file_location`) that the click lands on. Web links are not files.
pub fn file_link_at(snapshot: Option<&ScreenSnapshot>, point: GridPoint) -> Option<FileLocation> {
    let snapshot = snapshot?;
    if let Some(uri) = hyperlink_at(Some(snapshot), point) {
        return file_uri_path(uri).map(|path| FileLocation { path, line: None });
    }
    let (line, clicked) = logical_line(snapshot, point)?;
    let (token_start, token) = token_at(&line, &clicked);
    let (span, location) = file_location(token)?;
    span.contains(&(clicked.start - token_start))
        .then_some(location)
}

/// The percent-decoded path of a `file://HOST/PATH` URI; any query or fragment is dropped.
fn file_uri_path(uri: &str) -> Option<String> {
    let rest = uri
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("file://"))
        .and(uri.get(7..))?;
    let path = &rest[rest.find('/')?..];
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |offset: usize| {
            bytes
                .get(index + offset)
                .and_then(|byte| (*byte as char).to_digit(16))
        };
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (hex(1), hex(2))
        {
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .ok()
        .filter(|path| !path.contains('\0'))
}

/// The file location in `token`, a run of terminal text without whitespace, and its
/// byte range there. Accepts `PATH`, `PATH:LINE`, `PATH:LINE:COL` (text may follow, as
/// in `grep -n` output) and `PATH(LINE)` or `PATH(LINE,COL)`, without the brackets,
/// quotes and punctuation around it. A path needs a separator or a file extension, so
/// plain words, numbers and URLs are not paths. Columns are accepted and ignored.
pub fn file_location(token: &str) -> Option<(Range<usize>, FileLocation)> {
    const OPENERS: [char; 7] = ['(', '[', '{', '<', '\'', '"', '`'];
    const CLOSERS: [char; 13] = [
        '.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '\'', '"', '`',
    ];
    if token.contains("://") {
        return None;
    }
    let start = token.len() - token.trim_start_matches(OPENERS).len();
    let rest = &token[start..];
    let bytes = rest.as_bytes();
    // A drive letter's colon belongs to the path; the next colon ends it.
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let from = if drive { 2 } else { 0 };
    let colon = rest[from..].find(':').map(|index| from + index);
    let mut path = &rest[..colon.unwrap_or(rest.len())];
    let mut end;
    let mut line = None;
    if let Some(colon) = colon
        && let Some((value, digits)) = leading_number(&rest[colon + 1..])
    {
        line = Some(value);
        end = colon + 1 + digits;
        if rest[end..].starts_with(':')
            && let Some((_, column)) = leading_number(&rest[end + 1..])
        {
            end += 1 + column;
        }
    } else {
        // `PATH(LINE,COL):` as MSVC and .NET print it.
        let trimmed =
            path.trim_end_matches(|character| character != ')' && CLOSERS.contains(&character));
        if let Some((open, value)) = parenthesized_line(trimmed) {
            line = Some(value);
            end = trimmed.len();
            path = &trimmed[..open];
        } else {
            path = path.trim_end_matches(CLOSERS);
            end = path.len();
        }
    }
    if !looks_like_path(path) {
        return None;
    }
    Some((
        start..start + end,
        FileLocation {
            path: path.to_owned(),
            // Line 0 is not a line; the file opens at its start.
            line: line.filter(|line| *line > 0),
        },
    ))
}

/// The number `text` starts with and how many digits it has.
fn leading_number(text: &str) -> Option<(u32, usize)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    Some((text[..digits].parse().ok()?, digits))
}

/// Where `(LINE)` or `(LINE,COL)` ending `path` starts, and the line.
fn parenthesized_line(path: &str) -> Option<(usize, u32)> {
    let before = path.strip_suffix(')')?;
    let open = before.rfind('(')?;
    let inner = &before[open + 1..];
    let (line, column) = inner.split_once(',').unwrap_or((inner, "0"));
    let numeric = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if open == 0 || !numeric(line) || !numeric(column) {
        return None;
    }
    Some((open, line.parse().ok()?))
}

/// A separator or a file extension (letters and digits, at least one letter), and no
/// character a file path in terminal output would not have.
fn looks_like_path(path: &str) -> bool {
    let body = match path.as_bytes() {
        [drive, b':', ..] if drive.is_ascii_alphabetic() => &path[2..],
        _ => path,
    };
    if body.is_empty()
        || body.chars().any(|character| {
            character.is_control() || matches!(character, '<' | '>' | '|' | '"' | '*' | '?' | ':')
        })
    {
        return false;
    }
    if body.contains(['/', '\\']) {
        return !body.trim_matches(['/', '\\', '.']).is_empty();
    }
    body.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && (1..=12).contains(&extension.len())
            && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && extension.bytes().any(|byte| byte.is_ascii_alphabetic())
    })
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

    fn file(token: &str) -> Option<(String, Option<u32>)> {
        file_location(token).map(|(_, location)| (location.path, location.line))
    }

    #[test]
    fn finds_file_paths_with_lines_in_terminal_words() {
        let found = |path: &str, line| Some((path.to_owned(), line));
        assert_eq!(file("src/foo.rs:12:3"), found("src/foo.rs", Some(12)));
        assert_eq!(file("./a/b.ts:4"), found("./a/b.ts", Some(4)));
        assert_eq!(file("/abs/path.rs"), found("/abs/path.rs", None));
        assert_eq!(file("crates/x/y.rs"), found("crates/x/y.rs", None));
        assert_eq!(file("foo.rs:12:"), found("foo.rs", Some(12)));
        assert_eq!(file("(foo.rs:12)"), found("foo.rs", Some(12)));
        assert_eq!(file("'src/main.rs'."), found("src/main.rs", None));
        assert_eq!(file("src/a.rs:7:pub"), found("src/a.rs", Some(7)));
        assert_eq!(file("Program.cs(12,5):"), found("Program.cs", Some(12)));
        assert_eq!(
            file(r"C:\work\main.rs:3"),
            found(r"C:\work\main.rs", Some(3))
        );
        assert_eq!(file("README.md"), found("README.md", None));
        assert_eq!(file("foo.rs:0"), found("foo.rs", None));
        assert_eq!(file("https://example.com/a.rs"), None);
        assert_eq!(file("mailto:dev@example.com"), None);
        assert_eq!(file("hello"), None);
        assert_eq!(file("hello:12"), None);
        assert_eq!(file("1.5"), None);
        assert_eq!(file("v1.2.3"), None);
        assert_eq!(file("..."), None);
        assert_eq!(file("/"), None);
        assert_eq!(file("C:"), None);
    }

    #[test]
    fn file_links_cover_only_the_location_and_follow_wraps() {
        let (span, _) = file_location("(src/a.rs:12:3):").unwrap();
        assert_eq!(span, 1..14);
        let snapshot = snapshot(vec![
            row("error at crates/compi-cl", true),
            row("ient/src/gui.rs:42:7: x", false),
        ]);
        let at = |row, col| file_link_at(Some(&snapshot), GridPoint { row, col });
        let expected = Some(FileLocation {
            path: "crates/compi-client/src/gui.rs".into(),
            line: Some(42),
        });
        assert_eq!(at(0, 12), expected);
        assert_eq!(at(1, 18), expected);
        assert_eq!(at(1, 20), None);
        assert_eq!(at(0, 3), None);
    }

    #[test]
    fn file_hyperlinks_open_their_decoded_path() {
        let mut link = row("main", false);
        for cell in &mut link.cells {
            cell.hyperlink = Some("file://host/work/My%20App/main.rs".into());
        }
        let mut web = row("site", false);
        web.cells[0].hyperlink = Some("https://example.com/a.rs".into());
        let snapshot = snapshot(vec![link, web]);
        assert_eq!(
            file_link_at(Some(&snapshot), GridPoint { row: 0, col: 1 }),
            Some(FileLocation {
                path: "/work/My App/main.rs".into(),
                line: None,
            })
        );
        assert_eq!(
            file_link_at(Some(&snapshot), GridPoint { row: 1, col: 0 }),
            None
        );
    }
}
