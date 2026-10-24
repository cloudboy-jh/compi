use compi_protocol::{HistoryRow, Row, ScreenSnapshot};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ScreenMirror;
use crate::selection::{GridPoint, Selection};

/// Absolute row `row` (history first, then the screen), history rows expanded to the
/// screen's width.
pub fn row_at(snapshot: &ScreenSnapshot, row: usize) -> Option<Cow<'_, Row>> {
    match row.checked_sub(snapshot.scrollback.len()) {
        None => snapshot
            .scrollback
            .get(row)
            .map(|history| Cow::Owned(history.to_row(usize::from(snapshot.cols)))),
        Some(row) => snapshot.cells.get(row).map(Cow::Borrowed),
    }
}

fn wrapped_at(snapshot: &ScreenSnapshot, row: usize) -> bool {
    match row.checked_sub(snapshot.scrollback.len()) {
        None => snapshot.scrollback.get(row).is_some_and(|row| row.wrapped),
        Some(row) => snapshot.cells.get(row).is_some_and(|row| row.wrapped),
    }
}

pub fn visible_base(snapshot: &ScreenSnapshot, scroll_offset: usize) -> usize {
    snapshot.scrollback.len() - scroll_offset.min(snapshot.scrollback.len())
}

/// Where a view is after a history of `len` rows lost `evicted` rows at its front and
/// gained `appended` at its end. Scrolled up, the reading row stays put, or the view
/// shows the top of the history if that row was evicted. A selection follows its rows
/// and is dropped if any of them was evicted.
pub fn follow_append(
    len: usize,
    evicted: usize,
    appended: usize,
    scroll_offset: usize,
    selection: Option<Selection>,
) -> (usize, Option<Selection>) {
    let evicted = evicted.min(len);
    let shown = scroll_offset.min(len);
    let offset = if scroll_offset == 0 {
        0
    } else if len - shown >= evicted {
        shown + appended
    } else {
        len - evicted + appended
    };
    let selection = selection.and_then(|selection| {
        let (start, _) = selection.ordered();
        (start.row >= evicted).then(|| Selection {
            anchor: GridPoint {
                row: selection.anchor.row - evicted,
                ..selection.anchor
            },
            head: GridPoint {
                row: selection.head.row - evicted,
                ..selection.head
            },
        })
    });
    (offset, selection)
}

/// History rows by index: the history held, a replacement for it, or a viewport kept
/// to match against a later one.
pub trait HistoryRows {
    fn count(&self) -> usize;
    fn row(&self, index: usize) -> Option<&HistoryRow>;
}

impl HistoryRows for [HistoryRow] {
    fn count(&self) -> usize {
        self.len()
    }

    fn row(&self, index: usize) -> Option<&HistoryRow> {
        self.get(index)
    }
}

impl HistoryRows for VecDeque<HistoryRow> {
    fn count(&self) -> usize {
        self.len()
    }

    fn row(&self, index: usize) -> Option<&HistoryRow> {
        self.get(index)
    }
}

/// Whether `new` is `old` with rows appended.
pub fn extends(old: &(impl HistoryRows + ?Sized), new: &(impl HistoryRows + ?Sized)) -> bool {
    old.count() <= new.count() && (0..old.count()).all(|index| old.row(index) == new.row(index))
}

/// How long a clear waits for its reprint before the view returns to live output.
pub const REPRINT_GRACE: Duration = Duration::from_millis(250);
/// The same just after a resize: the program redraws once it hears the new size, which
/// takes a round trip through the shell.
pub const RESIZE_REPRINT_GRACE: Duration = Duration::from_secs(1);

/// A clear and its reprint can arrive in separate frames. Keep only the reading
/// viewport briefly, not another copy of the history. An actual clear expires.
pub struct PendingScroll {
    pub rows: Arc<[Row]>,
    pub base: usize,
    /// `rows` compact, to find them in a later history.
    anchor: Vec<HistoryRow>,
    /// The width `rows` were wrapped at.
    cols: usize,
    /// The history length `base` was in, to place a repeated match.
    history_len: usize,
    deadline: Instant,
    previous_offset: usize,
}

impl PendingScroll {
    pub fn new(
        mirror: &ScreenMirror,
        offset: usize,
        now: Instant,
        grace: Duration,
    ) -> Option<Self> {
        let (rows, base) = mirror.visible_rows(offset)?;
        let snapshot = mirror.snapshot();
        Some(Self {
            anchor: rows.iter().map(HistoryRow::from_row).collect(),
            rows,
            base,
            cols: snapshot.map_or(0, |snapshot| usize::from(snapshot.cols)),
            history_len: snapshot.map_or(0, |snapshot| snapshot.scrollback.len()),
            deadline: now + grace,
            previous_offset: offset,
        })
    }

    pub fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Where the kept viewport is in `history`, wrapped at `cols`.
    pub fn recover(
        &self,
        history: &(impl HistoryRows + ?Sized),
        cols: usize,
    ) -> Option<(usize, usize)> {
        let base = matching_base(self.anchor.as_slice(), 0, history).or_else(|| {
            let expected = proportional(self.base, self.history_len, history.count());
            word_base(
                (self.anchor.as_slice(), self.cols),
                0,
                (history, cols),
                expected,
            )
        })?;
        let offset = history.count().saturating_sub(base);
        Some((offset, offset.saturating_sub(self.previous_offset)))
    }
}

/// Keep the top reading row of a history replaced by `new`, using its neighbours to
/// disambiguate repeated prompts/blank lines. Compares compact rows; expands none.
/// When no row matches (the terminal or the program rewrapped the text at another
/// width), the reading position is found by its words, each history read at the width
/// its rows were wrapped at. None means that the reading text is no longer in the
/// received history.
pub fn anchored_scroll_offset(
    (old, old_cols): (&(impl HistoryRows + ?Sized), usize),
    (new, new_cols): (&(impl HistoryRows + ?Sized), usize),
    offset: usize,
) -> Option<usize> {
    if offset == 0 {
        return Some(0);
    }
    if extends(old, new) {
        return Some(
            offset
                .saturating_add(new.count() - old.count())
                .min(new.count()),
        );
    }
    let base = old.count().saturating_sub(offset);
    matching_base(old, base, new)
        .or_else(|| {
            let expected = proportional(base, old.count(), new.count());
            word_base((old, old_cols), base, (new, new_cols), expected)
        })
        .map(|base| new.count().saturating_sub(base))
}

/// Each kept cell's text and width.
fn cells(row: &HistoryRow) -> impl Iterator<Item = (&str, u8)> {
    let mut offset = 0;
    (0..row.len()).map(move |index| {
        let (len, width) = row
            .cells
            .get(index)
            .map_or((1, 1), |(len, width)| (usize::from(*len), *width));
        let text = row.text.get(offset..offset + len).unwrap_or_default();
        offset += len;
        (text, width)
    })
}

/// The same cell texts and widths, the trailing blanks a compact row drops counted as
/// the default blanks they were.
fn same_row_text(left: &HistoryRow, right: &HistoryRow) -> bool {
    if left.wrapped != right.wrapped {
        return false;
    }
    if left.cells.is_empty() && right.cells.is_empty() {
        return left.text.trim_end_matches(' ') == right.text.trim_end_matches(' ');
    }
    let blank = (" ", 1);
    let (mut left, mut right) = (cells(left), cells(right));
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (left, right) if left.unwrap_or(blank) != right.unwrap_or(blank) => return false,
            _ => {}
        }
    }
}

fn matching_base(
    old: &(impl HistoryRows + ?Sized),
    base: usize,
    new: &(impl HistoryRows + ?Sized),
) -> Option<usize> {
    // Prefer text over a blank top row, but never jump to unrelated offscreen text.
    let anchor = (base..old.count().min(base + 4))
        .find(|&index| {
            old.row(index)
                .is_some_and(|row| !row.text.trim().is_empty())
        })
        .unwrap_or(base);
    let row = old.row(anchor)?;
    let mut best = None;
    for index in 0..new.count() {
        if !new
            .row(index)
            .is_some_and(|candidate| same_row_text(row, candidate))
        {
            continue;
        }
        let mut context = 0;
        for distance in 1..=3 {
            if anchor >= distance
                && index >= distance
                && let (Some(left), Some(right)) =
                    (old.row(anchor - distance), new.row(index - distance))
                && same_row_text(left, right)
            {
                context += 1;
            }
            if let (Some(left), Some(right)) =
                (old.row(anchor + distance), new.row(index + distance))
                && same_row_text(left, right)
            {
                context += 1;
            }
        }
        let rank = (context, std::cmp::Reverse(index.abs_diff(anchor)));
        if best.as_ref().is_none_or(|(best_rank, _)| rank > *best_rank) {
            best = Some((rank, index.saturating_sub(anchor - base)));
        }
    }
    best.map(|(_, base)| base)
}

/// Most words a reading position is found by when its rows no longer match: enough to
/// span distinctive text, so a reprint still arriving cannot match it early where the
/// same phrase repeats.
const ANCHOR_WORDS: usize = 48;
/// Fewest: a shorter run of words could match unrelated text.
const MIN_ANCHOR_WORDS: usize = 4;

/// `row` of a history `from` rows long, at the same place in one `to` rows long.
fn proportional(row: usize, from: usize, to: usize) -> usize {
    if from == 0 {
        return 0;
    }
    (row as u128 * to as u128 / from as u128) as usize
}

/// Up to `limit` words of `rows`, wrapped at `cols`, from row `start`, each with the
/// row it starts in. Soft wraps join, so a word split by the terminal is one word; a
/// wrapped row shorter than `cols` ended in blanks its compact form dropped, so it ends
/// a word. A word continuing from the row before `start` is not counted. Words keep
/// only what lies between their first and last letter or digit, and those with neither
/// (borders, bullets, rules) are left out, so text rewrapped at another width, by the
/// terminal or by the program that printed it, gives the same words.
fn words(
    (rows, cols): (&(impl HistoryRows + ?Sized), usize),
    start: usize,
    limit: usize,
) -> Vec<(String, usize)> {
    fn finish(word: &mut String, row: usize, output: &mut Vec<(String, usize)>) {
        let key = word.trim_matches(|c: char| !c.is_alphanumeric());
        if !key.is_empty() {
            output.push((key.to_owned(), row));
        }
        word.clear();
    }
    // Whether a row's last word goes on into the next row.
    let continues = |row: &HistoryRow| row.wrapped && row.len() >= cols;
    let mut output = Vec::new();
    let mut word = String::new();
    let mut word_row = start;
    // A row continuing a soft-wrapped word starts mid-word.
    let mut skipping = start > 0 && rows.row(start - 1).is_some_and(continues);
    for index in start..rows.count() {
        let Some(row) = rows.row(index) else {
            break;
        };
        for character in row.text.chars() {
            if character.is_whitespace() {
                skipping = false;
                if !word.is_empty() {
                    finish(&mut word, word_row, &mut output);
                    if output.len() >= limit {
                        return output;
                    }
                }
            } else if !skipping {
                if word.is_empty() {
                    word_row = index;
                }
                word.push(character);
            }
        }
        if !continues(row) {
            skipping = false;
            if !word.is_empty() {
                finish(&mut word, word_row, &mut output);
                if output.len() >= limit {
                    return output;
                }
            }
        }
    }
    if !word.is_empty() {
        finish(&mut word, word_row, &mut output);
    }
    output.truncate(limit);
    output
}

/// Where the words from row `base` of `old` start in `new`, as `new`'s base: the
/// occurrence nearest row `expected` when they repeat.
fn word_base(
    old: (&(impl HistoryRows + ?Sized), usize),
    base: usize,
    new: (&(impl HistoryRows + ?Sized), usize),
    expected: usize,
) -> Option<usize> {
    let anchor = words(old, base, ANCHOR_WORDS);
    if anchor.len() < MIN_ANCHOR_WORDS {
        return None;
    }
    // Rows above the first word stay above it.
    let lead = anchor[0].1 - base;
    let stream = words(new, 0, usize::MAX);
    stream
        .windows(anchor.len())
        .filter(|window| {
            window
                .iter()
                .zip(&anchor)
                .all(|((found, _), (wanted, _))| found == wanted)
        })
        .map(|window| window[0].1.saturating_sub(lead))
        .min_by_key(|row| row.abs_diff(expected))
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

fn hyperlink_at(snapshot: &ScreenSnapshot, point: GridPoint) -> Option<String> {
    Some(
        row_at(snapshot, point.row)?
            .cells
            .get(point.col)?
            .hyperlink
            .as_deref()?
            .to_owned(),
    )
}

pub fn web_link_at(snapshot: Option<&ScreenSnapshot>, point: GridPoint) -> Option<String> {
    let snapshot = snapshot?;
    if let Some(uri) = hyperlink_at(snapshot, point).filter(|uri| is_allowed_hyperlink(uri)) {
        return Some(uri);
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
    while first_row > 0 && wrapped_at(snapshot, first_row - 1) {
        first_row -= 1;
    }
    let mut last_row = point.row;
    while last_row + 1 < row_count && wrapped_at(snapshot, last_row) {
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
    if let Some(uri) = hyperlink_at(snapshot, point) {
        return file_uri_path(&uri).map(|path| FileLocation { path, line: None });
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
            scrollback: VecDeque::new(),
            cursor: Default::default(),
            modes: Default::default(),
            title: String::new(),
            current_directory: None,
            images: Vec::new(),
            placements: Vec::new(),
        }
    }

    fn line(index: usize) -> Row {
        row(&format!("line {index:04}"), false)
    }

    fn history(range: Range<usize>) -> VecDeque<HistoryRow> {
        range
            .map(|index| HistoryRow::from_row(&line(index)))
            .collect()
    }

    #[test]
    fn appends_keep_the_reading_row_until_evicted_and_move_the_selection_with_its_rows() {
        let mut rows = history(0..100);
        // Reading line 70, lines 72 to 75 selected.
        let mut offset = 30;
        let mut selection = Some(Selection {
            anchor: GridPoint { row: 75, col: 3 },
            head: GridPoint { row: 72, col: 1 },
        });
        for step in 1..=5 {
            let len = rows.len();
            rows.extend(history(85 + 15 * step..100 + 15 * step));
            let evicted = rows.len() - 100;
            rows.drain(..evicted);
            (offset, selection) = follow_append(len, evicted, 15, offset, selection);
            let first = 15 * step;
            if first <= 70 {
                assert_eq!(rows[rows.len() - offset], HistoryRow::from_row(&line(70)));
            } else {
                assert_eq!(offset, rows.len(), "the reading row left: the top shows");
            }
            match selection {
                Some(selection) => {
                    assert_eq!(
                        selection.anchor,
                        GridPoint {
                            row: 75 - first,
                            col: 3
                        }
                    );
                    assert_eq!(
                        selection.head,
                        GridPoint {
                            row: 72 - first,
                            col: 1
                        }
                    );
                }
                None => assert!(first > 72, "dropped only once a selected row left"),
            }
        }
        assert_eq!(selection.map(|selection| selection.head.row), None);

        // Live output stays live; rows only appended move neither view nor selection.
        let on_screen = Some(Selection {
            anchor: GridPoint { row: 101, col: 0 },
            head: GridPoint { row: 103, col: 2 },
        });
        let (offset, kept) = follow_append(100, 0, 4, 0, on_screen);
        assert_eq!(offset, 0);
        assert_eq!(
            kept.map(|selection| selection.ordered()),
            on_screen.map(|s| s.ordered())
        );
        assert_eq!(follow_append(100, 0, 4, 10, None).0, 14);
    }

    #[test]
    fn replaced_history_keeps_the_reading_row_by_its_text() {
        let old = history(0..100);
        for new in [history(0..110), history(10..110)] {
            let offset = anchored_scroll_offset((&old, 9), (&new, 9), 50).unwrap();
            assert!(same_row_text(&old[50], &new[new.len() - offset]));
            assert_eq!(offset, 60);
        }
        assert_eq!(
            anchored_scroll_offset((&old, 9), (&history(60..160), 9), 50),
            None
        );
        assert_eq!(
            anchored_scroll_offset((&old, 9), (&history(10..110), 9), 0),
            Some(0)
        );
        assert!(extends(&old, &history(0..101)));
        assert!(!extends(&old, &history(1..101)));
    }

    #[test]
    fn compact_rows_compare_by_text_whatever_blanks_or_styles_they_keep() {
        let plain = HistoryRow::from_row(&row("abc   ", false));
        let mut styled_blank = row("abc ", false);
        styled_blank.cells[3].background = compi_protocol::Color::Indexed(4);
        let styled_blank = HistoryRow::from_row(&styled_blank);
        assert_ne!(plain.len(), styled_blank.len());
        assert!(same_row_text(&plain, &styled_blank));
        let mut wide = row("a界 ", false);
        wide.cells[1].width = 2;
        wide.cells[2] = compi_protocol::Cell {
            text: "".into(),
            width: 0,
            ..Default::default()
        };
        let wide = HistoryRow::from_row(&wide);
        assert!(same_row_text(&wide, &wide.clone()));
        assert!(!same_row_text(
            &wide,
            &HistoryRow::from_row(&row("a界", false))
        ));
        assert!(!same_row_text(
            &plain,
            &HistoryRow::from_row(&row("abc", true))
        ));
        assert!(!same_row_text(
            &plain,
            &HistoryRow::from_row(&row("abcd", false))
        ));
    }

    #[test]
    fn redraw_keeps_text_anchor_and_disambiguates_repeated_lines() {
        let repeated = HistoryRow::from_row(&row("repeated ", false));
        let mut recolored = row("repeated ", false);
        recolored.cells[0].foreground = compi_protocol::Color::Indexed(2);
        let mut old = history(0..30);
        old[8] = repeated.clone();
        old[18] = repeated.clone();
        let mut new = history(5..35);
        new[3] = repeated;
        new[13] = HistoryRow::from_row(&recolored);
        assert_eq!(anchored_scroll_offset((&old, 9), (&new, 9), 12), Some(17));
        let mut reprint = history(0..5);
        reprint.extend(old.iter().cloned());
        assert_eq!(
            anchored_scroll_offset((&old, 9), (&reprint, 9), 12),
            Some(12)
        );
    }

    #[test]
    fn split_clear_and_reprint_recovers_but_real_clear_expires() {
        let mut screen = snapshot((100..110).map(line).collect());
        screen.scrollback = history(0..100);
        let mut mirror = ScreenMirror::default();
        mirror.apply(compi_protocol::ScreenMessage::Snapshot { snapshot: screen });
        let now = Instant::now();
        let pending = PendingScroll::new(&mirror, 50, now, REPRINT_GRACE).unwrap();
        assert_eq!(pending.recover(&VecDeque::new(), 9), None);
        assert_eq!(pending.recover(&history(0..40), 9), None);
        assert_eq!(pending.recover(&history(0..110), 9), Some((60, 10)));
        assert!(!pending.expired(now + Duration::from_millis(249)));
        assert!(pending.expired(now + Duration::from_millis(250)));
        assert_eq!(pending.base, 50);
        assert_eq!(
            pending.rows.as_ref(),
            (50..60).map(line).collect::<Vec<_>>()
        );
    }

    /// `paragraphs` as a program prints them at `width`: each word-wrapped, inside
    /// `│ … │` borders, continuation lines indented by two.
    fn program_wrapped(paragraphs: &[&str], width: usize) -> VecDeque<HistoryRow> {
        let inner = width - 4;
        let mut rows = VecDeque::new();
        for paragraph in paragraphs {
            let mut line = String::new();
            for word in paragraph.split_whitespace() {
                if !line.is_empty() && line.len() + 1 + word.len() > inner {
                    rows.push_back(format!("│ {line:<inner$} │"));
                    line = "  ".into();
                }
                if !line.trim().is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
            rows.push_back(format!("│ {line:<inner$} │"));
            rows.push_back(format!("│{}│", " ".repeat(width - 2)));
        }
        rows.iter()
            .map(|text| HistoryRow::from_row(&row(text, false)))
            .collect()
    }

    /// `text` as the terminal soft-wraps it at `width`.
    fn soft_wrapped(text: &str, width: usize) -> VecDeque<HistoryRow> {
        let characters: Vec<char> = text.chars().collect();
        let chunks: Vec<String> = characters
            .chunks(width)
            .map(|chunk| chunk.iter().collect())
            .collect();
        chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| HistoryRow::from_row(&row(chunk, index + 1 < chunks.len())))
            .collect()
    }

    fn row_containing(rows: &VecDeque<HistoryRow>, word: &str) -> usize {
        rows.iter().position(|row| row.text.contains(word)).unwrap()
    }

    const PARAGRAPHS: [&str; 6] = [
        "The daemon keeps every terminal running while windows come and go.",
        "Resizing the window asks the program to redraw at its new width, and a redraw can wrap every line differently.",
        "Reading position alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima.",
        "Another paragraph follows with ordinary words so the reading position has neighbours.",
        "Then the output continues with more lines below the reading position.",
        "Finally the prompt returns.",
    ];

    #[test]
    fn a_program_rewrapping_its_text_keeps_the_reading_words_at_the_top() {
        for (from, to) in [(40, 64), (64, 32)] {
            let old = program_wrapped(&PARAGRAPHS, from);
            let new = program_wrapped(&PARAGRAPHS, to);
            // Reading from the row holding "alpha".
            let base = row_containing(&old, "alpha");
            let offset =
                anchored_scroll_offset((&old, from), (&new, to), old.len() - base).unwrap();
            assert_eq!(
                new.len() - offset,
                row_containing(&new, "alpha"),
                "{from}→{to}"
            );
        }
    }

    #[test]
    fn terminal_reflow_of_a_long_line_keeps_the_reading_words() {
        let text = PARAGRAPHS.join(" ");
        let (from, to) = (30, 47);
        let old = soft_wrapped(&text, from);
        let new = soft_wrapped(&text, to);
        for base in [5, 9, 13] {
            // The first whole word from the reading row's first character: a row that
            // starts mid-word is placed by the word after.
            let start = base * from;
            let word = if text.as_bytes()[start - 1] == b' ' {
                start
            } else {
                start + text[start..].find(' ').unwrap() + 1
            };
            // Rows above that word's row stay above it.
            let above = word / from - base;
            let offset =
                anchored_scroll_offset((&old, from), (&new, to), old.len() - base).unwrap();
            assert_eq!(new.len() - offset, word / to - above, "row {base}");
        }
    }

    #[test]
    fn repeated_words_resolve_to_the_nearest_place_and_vanished_words_to_none() {
        let section = [
            "alpha bravo charlie delta echo foxtrot golf hotel",
            "filler text here",
        ];
        let repeated = [section[0], section[1], section[1], section[0], section[1]];
        let old = program_wrapped(&repeated, 30);
        let new = program_wrapped(&repeated, 50);
        // Reading the second occurrence, near the end.
        let second = old
            .iter()
            .rposition(|row| row.text.contains("alpha"))
            .unwrap();
        let offset = anchored_scroll_offset((&old, 30), (&new, 50), old.len() - second).unwrap();
        let expected = new
            .iter()
            .rposition(|row| row.text.contains("alpha"))
            .unwrap();
        assert_eq!(new.len() - offset, expected);
        // A program that reprints only its last screen: the words are gone.
        let old = program_wrapped(&PARAGRAPHS, 40);
        let base = row_containing(&old, "alpha");
        let last_screen = program_wrapped(&PARAGRAPHS[4..], 50);
        assert_eq!(
            anchored_scroll_offset((&old, 40), (&last_screen, 50), old.len() - base),
            None
        );
    }

    #[test]
    fn a_reprint_after_a_resize_recovers_the_view_by_its_words() {
        let old = program_wrapped(&PARAGRAPHS, 40);
        let base = row_containing(&old, "alpha");
        let mut screen = snapshot(vec![row("$ ", false)]);
        screen.scrollback = old.clone();
        let mut mirror = ScreenMirror::default();
        mirror.apply(compi_protocol::ScreenMessage::Snapshot { snapshot: screen });
        let now = Instant::now();
        let pending =
            PendingScroll::new(&mirror, old.len() - base, now, RESIZE_REPRINT_GRACE).unwrap();
        let reprint = program_wrapped(&PARAGRAPHS, 64);
        let (offset, _) = pending.recover(&reprint, 64).unwrap();
        assert_eq!(reprint.len() - offset, row_containing(&reprint, "alpha"));
        assert!(!pending.expired(now + Duration::from_millis(999)));
        assert!(pending.expired(now + RESIZE_REPRINT_GRACE));
    }

    #[test]
    fn a_reprint_still_arriving_does_not_match_a_repeated_phrase_early() {
        // Like a transcript of many similar messages: each body is the same vocabulary
        // rotated, so a dozen words repeat throughout; the numbered headers do not.
        let vocabulary = "terminal window daemon resize wrap redraw reading position scroll \
                          history transcript program output prompt pane split float shell agent";
        let vocabulary: Vec<&str> = vocabulary.split_whitespace().collect();
        let paragraphs: Vec<String> = (0..120)
            .map(|number| {
                let body: Vec<&str> = (0..40 + number % 13)
                    .map(|index| vocabulary[(number * 7 + index) % vocabulary.len()])
                    .collect();
                format!(
                    "Paragraph {number:03} marker{number:03}: {}.",
                    body.join(" ")
                )
            })
            .collect();
        let paragraphs: Vec<&str> = paragraphs.iter().map(String::as_str).collect();
        let old = program_wrapped(&paragraphs, 78);
        // Reading from inside a body, where the top rows hold only repeated words.
        let base = row_containing(&old, "Paragraph 093") + 3;
        let mut screen = snapshot(vec![row("$ ", false); 24]);
        screen.scrollback = old.clone();
        let mut mirror = ScreenMirror::default();
        mirror.apply(compi_protocol::ScreenMessage::Snapshot { snapshot: screen });
        let now = Instant::now();
        let pending =
            PendingScroll::new(&mirror, old.len() - base, now, RESIZE_REPRINT_GRACE).unwrap();
        let reprint = program_wrapped(&paragraphs, 60);
        let arrived: VecDeque<HistoryRow> =
            reprint.iter().take(reprint.len() / 3).cloned().collect();
        assert_eq!(pending.recover(&arrived, 60), None);
        let (offset, _) = pending.recover(&reprint, 60).unwrap();
        let paragraph =
            row_containing(&reprint, "Paragraph 093")..row_containing(&reprint, "Paragraph 094");
        assert!(paragraph.contains(&(reprint.len() - offset)));
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

    #[test]
    fn links_wrap_from_history_onto_the_screen() {
        // A soft-wrapped row fills the width; the screen row after it continues the line.
        let mut start = row("see https://example", true);
        start.cells[0].hyperlink = Some("file://host/notes.txt".into());
        let mut screen = snapshot(vec![row(".com/a and b", false)]);
        screen.cols = 19;
        screen.scrollback = [
            HistoryRow::from_row(&row("older", false)),
            HistoryRow::from_row(&start),
        ]
        .into_iter()
        .collect();
        let at = |row, col| web_link_at(Some(&screen), GridPoint { row, col });
        assert_eq!(at(1, 6).as_deref(), Some("https://example.com/a"));
        assert_eq!(at(2, 3).as_deref(), Some("https://example.com/a"));
        assert_eq!(at(2, 9), None);
        assert_eq!(at(0, 10), None);
        assert_eq!(
            file_link_at(Some(&screen), GridPoint { row: 1, col: 0 }),
            Some(FileLocation {
                path: "/notes.txt".into(),
                line: None,
            })
        );
        assert_eq!(row_at(&screen, 1).unwrap().cells.len(), 19);
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
