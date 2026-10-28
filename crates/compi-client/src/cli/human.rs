//! Output for a person at a terminal: whether there is one, how wide it is, and tables
//! and wrapped text that fit it. Lists print as tables only when stdout is a terminal;
//! piped or redirected, they print JSON as before, and `--json` always wins.

use std::io::{self, IsTerminal};

/// The width assumed when the terminal's cannot be read.
const DEFAULT_WIDTH: usize = 80;
/// The narrowest a shortened column gets, ellipsis included.
const MIN_COLUMN: usize = 6;

/// Whether stdout is a terminal. From a WSL terminal the Compi shell bridge runs the
/// Windows CLI on pipes, only when its own stdout is a terminal, and says so.
pub(super) fn wanted() -> bool {
    io::stdout().is_terminal() || super::bridged_terminal()
}

/// The terminal's width in columns: what the WSL shell bridge passes, else the console's
/// or tty's, else `COLUMNS`, else 80.
pub(super) fn width() -> usize {
    std::env::var("COMPI_CLI_COLUMNS")
        .ok()
        .filter(|_| super::bridged_terminal())
        .and_then(|columns| columns.trim().parse().ok())
        .or_else(native_width)
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|columns| columns.trim().parse().ok())
        })
        .filter(|columns: &usize| *columns >= 20)
        .unwrap_or(DEFAULT_WIDTH)
}

#[cfg(windows)]
fn native_width() -> Option<usize> {
    use windows::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: the standard output handle stays open for the process lifetime.
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE).ok()?;
        GetConsoleScreenBufferInfo(handle, &mut info).ok()?;
    }
    let columns = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
    usize::try_from(columns).ok().filter(|columns| *columns > 0)
}

#[cfg(unix)]
fn native_width() -> Option<usize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one `winsize` into `size`.
    let read = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    (read == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

/// How a table column gives way when the table is wider than the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fit {
    /// Never shortened: IDs, which must stay whole to be copied.
    Keep,
    /// Shortened at its end with `…`.
    End,
    /// Shortened at its start with `…`, keeping the end: paths and addresses.
    Start,
}

/// Left-aligned columns two spaces apart, the last unpadded, fitted to `width`: the
/// widest shortenable columns give way first, down to a few characters each; columns
/// marked `Keep` never do, so a table of IDs may still run past a narrow terminal.
pub(super) fn table(columns: &[(&str, Fit)], rows: &[Vec<String>], width: usize) -> String {
    let count = columns.len();
    let mut widths: Vec<usize> = columns.iter().map(|(title, _)| length(title)).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(length(cell));
        }
    }
    let gaps = 2 * count.saturating_sub(1);
    let mut excess = (widths.iter().sum::<usize>() + gaps).saturating_sub(width);
    while excess > 0 {
        let widest = (0..count)
            .filter(|&index| {
                columns[index].1 != Fit::Keep
                    && widths[index] > MIN_COLUMN.max(length(columns[index].0))
            })
            .max_by_key(|&index| widths[index]);
        let Some(index) = widest else {
            break;
        };
        widths[index] -= 1;
        excess -= 1;
    }
    let mut text = String::new();
    let header: Vec<String> = columns
        .iter()
        .map(|(title, _)| (*title).to_owned())
        .collect();
    for row in std::iter::once(&header).chain(rows) {
        let mut line = String::new();
        for (index, cell) in row.iter().enumerate() {
            let cell = shorten(cell, widths[index], columns[index].1);
            if index + 1 == count {
                line.push_str(&cell);
            } else {
                let pad = widths[index] - length(&cell);
                line.push_str(&cell);
                line.extend(std::iter::repeat_n(' ', pad + 2));
            }
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

/// `name: value` lines with the values aligned; empty values are left out.
pub(super) fn fields(rows: &[(&str, String)]) -> String {
    let rows: Vec<_> = rows.iter().filter(|(_, value)| !value.is_empty()).collect();
    let label = rows.iter().map(|(name, _)| length(name)).max().unwrap_or(0);
    let mut text = String::new();
    for (name, value) in rows {
        text.push_str(name);
        text.push(':');
        text.extend(std::iter::repeat_n(' ', label - length(name) + 1));
        text.push_str(value);
        text.push('\n');
    }
    text
}

/// `text` wrapped at word boundaries to `width`, every line after the first indented
/// by `indent` spaces. Line breaks in `text` are kept; a word longer than a line stays
/// whole.
pub(super) fn wrap(text: &str, width: usize, indent: usize) -> String {
    let width = width.max(indent + 20);
    let mut wrapped = String::new();
    for (index, paragraph) in text.split('\n').enumerate() {
        if index > 0 {
            wrapped.push('\n');
            wrapped.extend(std::iter::repeat_n(' ', indent));
        }
        let mut column = if index > 0 { indent } else { 0 };
        let mut first = true;
        for word in paragraph.split(' ').filter(|word| !word.is_empty()) {
            if !first && column + 1 + length(word) > width {
                wrapped.push('\n');
                wrapped.extend(std::iter::repeat_n(' ', indent));
                column = indent;
            } else if !first {
                wrapped.push(' ');
                column += 1;
            }
            wrapped.push_str(word);
            column += length(word);
            first = false;
        }
    }
    wrapped
}

/// `cell` cut to `width` characters with `…` at its `fit` end.
fn shorten(cell: &str, width: usize, fit: Fit) -> String {
    let length = length(cell);
    if length <= width || fit == Fit::Keep || width == 0 {
        return cell.to_owned();
    }
    let keep = width - 1;
    match fit {
        Fit::End => {
            let kept: String = cell.chars().take(keep).collect();
            format!("{}…", kept.trim_end())
        }
        Fit::Start => {
            let kept: String = cell.chars().skip(length - keep).collect();
            format!("…{}", kept.trim_start())
        }
        Fit::Keep => unreachable!(),
    }
}

fn length(text: &str) -> usize {
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
            .collect()
    }

    #[test]
    fn tables_shorten_descriptions_and_never_ids() {
        let columns = [("NAME", Fit::End), ("WHERE", Fit::Start), ("ID", Fit::Keep)];
        let rows = rows(&[&[
            "a long tab name",
            "/home/me/projects/compi/crates",
            "pane-1a17e9e24a8-364c-3",
        ]]);
        let wide = table(&columns, &rows, 200);
        assert_eq!(
            wide.lines().nth(1).unwrap(),
            "a long tab name  /home/me/projects/compi/crates  pane-1a17e9e24a8-364c-3"
        );
        // The widest descriptive columns give way first; the ID stays whole.
        assert_eq!(
            table(&columns, &rows, 50).lines().nth(1).unwrap(),
            "a long tab…   …mpi/crates  pane-1a17e9e24a8-364c-3"
        );
        // Too narrow even for shortened columns: the ID still prints whole.
        assert_eq!(
            table(&columns, &rows, 20).lines().nth(1).unwrap(),
            "a lon…  …rates  pane-1a17e9e24a8-364c-3"
        );
    }

    #[test]
    fn wrapping_breaks_at_words_and_indents_following_lines() {
        assert_eq!(
            wrap("one two three four five six", 24, 4),
            "one two three four five\n    six"
        );
        assert_eq!(wrap("first\nsecond line", 80, 2), "first\n  second line");
        // A word longer than the line is not split.
        let long = "x".repeat(40);
        assert_eq!(wrap(&format!("a {long}"), 30, 0), format!("a\n{long}"));
    }

    #[test]
    fn fields_align_values_and_skip_empty_ones() {
        assert_eq!(
            fields(&[
                ("ID", "pane-1".into()),
                ("Title", String::new()),
                ("Directory", "/home/me".into())
            ]),
            "ID:        pane-1\nDirectory: /home/me\n"
        );
    }
}
