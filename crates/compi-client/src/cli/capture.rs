use compi_protocol::{Color, Row, ScreenSnapshot};
use std::fmt::Write;

/// Join soft-wrapped physical rows; preserve spaces, wide glyphs and all hard breaks.
pub fn render(snapshot: &ScreenSnapshot, scrollback: bool, ansi: bool) -> String {
    render_rows(
        snapshot
            .scrollback
            .iter()
            .filter(|_| scrollback)
            .chain(&snapshot.cells),
        ansi,
    )
}

fn render_rows<'a>(rows: impl Iterator<Item = &'a Row>, ansi: bool) -> String {
    let mut result = String::new();
    let mut rows = rows.peekable();
    let mut previous_style = None;
    let mut previous_link = None;
    while let Some(row) = rows.next() {
        for cell in &row.cells {
            if cell.width == 0 {
                continue;
            }
            if ansi {
                let style = (cell.foreground, cell.background, &cell.attributes);
                if previous_style != Some(style) {
                    result.push_str("\x1b[0m");
                    for (enabled, code) in [
                        (cell.attributes.bold, 1),
                        (cell.attributes.dim, 2),
                        (cell.attributes.italic, 3),
                        (cell.attributes.underline, 4),
                        (cell.attributes.blink, 5),
                        (cell.attributes.inverse, 7),
                        (cell.attributes.hidden, 8),
                        (cell.attributes.strike, 9),
                    ] {
                        if enabled {
                            let _ = write!(result, "\x1b[{code}m");
                        }
                    }
                    color(&mut result, cell.foreground, true);
                    color(&mut result, cell.background, false);
                    previous_style = Some(style);
                }
                let link = cell.hyperlink.as_deref();
                if previous_link != link {
                    if previous_link.is_some() {
                        result.push_str("\x1b]8;;\x1b\\");
                    }
                    if let Some(link) = link {
                        let _ = write!(result, "\x1b]8;;{link}\x1b\\");
                    }
                    previous_link = link;
                }
            }
            result.push_str(&cell.text);
        }
        if !row.wrapped && rows.peek().is_some() {
            result.push('\n');
        }
    }
    if ansi {
        if previous_link.is_some() {
            result.push_str("\x1b]8;;\x1b\\");
        }
        result.push_str("\x1b[0m");
    }
    result
}

fn color(output: &mut String, color: Color, foreground: bool) {
    let code = if foreground { 38 } else { 48 };
    match color {
        Color::Default => {
            let _ = write!(output, "\x1b[{}m", code + 1);
        }
        Color::Indexed(index) => {
            let _ = write!(output, "\x1b[{code};5;{index}m");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(output, "\x1b[{code};2;{r};{g};{b}m");
        }
    }
}

pub fn quote_argv(args: &[String]) -> String {
    let capacity = args
        .iter()
        .map(|arg| arg.len() + 3 + 3 * arg.bytes().filter(|byte| *byte == b'\'').count())
        .sum();
    let mut result = String::with_capacity(capacity);
    for (index, arg) in args.iter().enumerate() {
        if index != 0 {
            result.push(' ');
        }
        result.push('\'');
        for part in arg.split_inclusive('\'') {
            if let Some(prefix) = part.strip_suffix('\'') {
                result.push_str(prefix);
                result.push_str("'\\''");
            } else {
                result.push_str(part);
            }
        }
        result.push('\'');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::Cell;
    #[test]
    fn quotes_shell_metacharacters_and_empty_arguments() {
        assert_eq!(
            quote_argv(&["a'b".into(), "$HOME;$(touch nope)".into(), "".into()]),
            "'a'\\''b' '$HOME;$(touch nope)' ''"
        );
    }
    #[test]
    fn capture_skips_wide_continuations_and_joins_only_soft_wraps() {
        let rows = [
            Row {
                cells: vec![
                    Cell {
                        text: "界".into(),
                        width: 2,
                        ..Cell::default()
                    },
                    Cell {
                        text: "".into(),
                        width: 0,
                        ..Cell::default()
                    },
                ],
                wrapped: true,
            },
            Row {
                cells: vec![Cell {
                    text: "x ".into(),
                    ..Cell::default()
                }],
                wrapped: false,
            },
            Row {
                cells: vec![Cell {
                    text: "y".into(),
                    foreground: Color::Rgb(1, 2, 3),
                    attributes: compi_protocol::TextAttributes {
                        bold: true,
                        ..Default::default()
                    },
                    ..Cell::default()
                }],
                wrapped: false,
            },
        ];
        assert_eq!(render_rows(rows.iter(), false), "界x \ny");
        let ansi = render_rows(rows.iter(), true);
        assert!(ansi.contains("\x1b[1m\x1b[38;2;1;2;3m"));
        assert!(ansi.ends_with("y\x1b[0m"));
    }
}
