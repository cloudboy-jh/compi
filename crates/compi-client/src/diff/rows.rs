//! Display rows of a diff: context, folds and the unified or split layout.

use std::ops::Range;

use super::{FileDiff, Hunk, Side};

/// Unchanged lines shown on each side of a hunk by default, like git.
const CONTEXT: u32 = 3;
/// Longest function context shown on a fold, in chars.
const MAX_FUNCTION_CHARS: usize = 80;

/// How a fold is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expand {
    /// Reveal hidden lines upward from the hunk below the fold (its leading context grows).
    Up,
    /// Reveal hidden lines downward from the hunk above the fold (its trailing context grows).
    Down,
    /// Reveal the whole gap.
    All,
}

/// Which unchanged lines are revealed. Default: 3 lines of context around each hunk, like git.
///
/// Gap `g` is the run of unchanged lines before hunk `g`; the last gap follows the last hunk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    all: bool,
    /// Lines revealed beyond the default, per gap.
    gaps: Vec<Reveal>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Reveal {
    /// Below the hunk above the gap.
    down: u32,
    /// Above the hunk below the gap.
    up: u32,
}

impl Context {
    /// Reveals `lines` more hidden lines of `gap` (all of them for [`Expand::All`]).
    pub fn expand(&mut self, gap: usize, direction: Expand, lines: u32) {
        let reveal = self.reveal_mut(gap);
        match direction {
            Expand::Up => reveal.up = reveal.up.saturating_add(lines),
            Expand::Down => reveal.down = reveal.down.saturating_add(lines),
            Expand::All => {
                *reveal = Reveal {
                    down: u32::MAX,
                    up: u32::MAX,
                }
            }
        }
    }

    /// Shows every unchanged line.
    pub fn expand_all(&mut self) {
        self.all = true;
        self.gaps.clear();
    }

    /// Back to the default context.
    pub fn collapse_all(&mut self) {
        *self = Self::default();
    }

    /// Every unchanged line is shown.
    pub fn is_all(&self) -> bool {
        self.all
    }

    /// Reveal so that `line` of `side` is visible (for find hits / jumps), with default context
    /// around it.
    pub fn reveal(&mut self, diff: &FileDiff, side: Side, line: u32) {
        let hunks = diff.hunks();
        let gap = hunks.partition_point(|hunk| side_range(hunk, side).end <= line);
        let span = Gap::new(diff, gap);
        let start = match side {
            Side::Old => span.old,
            Side::New => span.new,
        };
        if line < start || line >= start + span.len {
            return; // Inside a hunk: always shown.
        }
        let at = line - start;
        let (top, bottom) = self.split(gap, &span);
        if at < top || at >= span.len - bottom {
            return;
        }
        let from_top = at + 1 - top;
        let from_bottom = span.len - bottom - at;
        let reveal = self.reveal_mut(gap);
        if span.above && (!span.below || from_top <= from_bottom) {
            reveal.down = reveal.down.max(at + 1);
        } else {
            reveal.up = reveal.up.max(span.len - at);
        }
    }

    fn reveal_mut(&mut self, gap: usize) -> &mut Reveal {
        if self.gaps.len() <= gap {
            self.gaps.resize(gap + 1, Reveal::default());
        }
        &mut self.gaps[gap]
    }

    /// Lines shown at the top and the bottom of a gap; `(len, 0)` when the whole gap shows. A
    /// single hidden line is shown rather than folded.
    fn split(&self, gap: usize, span: &Gap) -> (u32, u32) {
        if self.all {
            return (span.len, 0);
        }
        let reveal = self.gaps.get(gap).copied().unwrap_or_default();
        let top = if span.above {
            CONTEXT.saturating_add(reveal.down)
        } else {
            0
        };
        let bottom = if span.below {
            CONTEXT.saturating_add(reveal.up)
        } else {
            0
        };
        if top.saturating_add(bottom).saturating_add(1) >= span.len {
            (span.len, 0)
        } else {
            (top, bottom)
        }
    }
}

/// A run of unchanged lines between hunks (or before the first / after the last).
struct Gap {
    /// First old line.
    old: u32,
    /// First new line.
    new: u32,
    len: u32,
    /// A hunk precedes the gap.
    above: bool,
    /// A hunk follows the gap.
    below: bool,
}

impl Gap {
    fn new(diff: &FileDiff, gap: usize) -> Self {
        let hunks = diff.hunks();
        let prev = gap.checked_sub(1).and_then(|ix| hunks.get(ix));
        let next = hunks.get(gap);
        let old = prev.map_or(0, |hunk| hunk.old.end);
        let new = prev.map_or(0, |hunk| hunk.new.end);
        let end = next.map_or(diff.old_len(), |hunk| hunk.old.start);
        Self {
            old,
            new,
            len: end - old,
            above: prev.is_some(),
            below: next.is_some(),
        }
    }
}

fn side_range(hunk: &Hunk, side: Side) -> &Range<u32> {
    match side {
        Side::Old => &hunk.old,
        Side::New => &hunk.new,
    }
}

/// How the two sides are laid out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Layout {
    /// One column; removed lines, then added lines.
    #[default]
    Unified,
    /// Old and new side by side.
    Split,
}

/// What a displayed line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Unchanged.
    Context,
    /// Only in the new text.
    Added,
    /// Only in the old text.
    Removed,
    /// Part of a formatting-only change; drawn like context.
    Formatting,
}

/// A displayed line of one side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// The side the text comes from.
    pub side: Side,
    /// The line on that side (0-based).
    pub ix: u32,
    /// What the line is.
    pub kind: LineKind,
    /// Byte ranges of changed words in that line (word-level emphasis).
    pub emphasis: Vec<Range<usize>>,
    /// Indent guides through the leading whitespace of added and removed lines.
    pub guides: Option<IndentGuides>,
}

/// Indent guides through a line's leading whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndentGuides {
    /// Cells per indent level: 4 with tabs or an indent that is a multiple of 4, else 2.
    pub unit: u8,
    /// Guides to draw: the indent in cells divided by `unit`.
    pub levels: u8,
}

/// One display row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// Unchanged lines hidden between hunks (or before the first / after the last).
    Fold {
        /// Index of the gap, for [`Context::expand`].
        gap: usize,
        /// Hidden lines, at least 2.
        hidden: u32,
        /// Enclosing declaration of the following hunk, trimmed.
        function: Option<String>,
        /// A hunk follows the fold, so [`Expand::Up`] applies.
        can_expand_up: bool,
        /// A hunk precedes the fold, so [`Expand::Down`] applies.
        can_expand_down: bool,
    },
    /// Unified: one line; context lines carry both numbers.
    Unified {
        /// Old line number (0-based), `None` on an added or unpaired formatting line.
        old: Option<u32>,
        /// New line number (0-based), `None` on a removed line.
        new: Option<u32>,
        /// The text shown: the new side, except on removed lines.
        line: DiffLine,
    },
    /// Split: old and new halves; `None` is the blank side of an unpaired line.
    Split {
        /// The old half.
        old: Option<DiffLine>,
        /// The new half.
        new: Option<DiffLine>,
    },
}

impl Row {
    fn is_change(&self) -> bool {
        let changed = |line: &DiffLine| matches!(line.kind, LineKind::Added | LineKind::Removed);
        match self {
            Row::Fold { .. } => false,
            Row::Unified { line, .. } => changed(line),
            Row::Split { old, new } => old.iter().chain(new).any(changed),
        }
    }
}

/// The rows to display. Empty when the diff [is empty](FileDiff::is_empty).
pub fn rows(diff: &FileDiff, context: &Context, layout: Layout) -> Vec<Row> {
    let mut out = Vec::new();
    if diff.is_empty() {
        return out;
    }
    let hunks = diff.hunks();
    // (fold row, first old line after it) of folds followed by a hunk.
    let mut anchors = Vec::new();
    for gap in 0..=hunks.len() {
        let span = Gap::new(diff, gap);
        let (top, bottom) = context.split(gap, &span);
        push_context(&mut out, &span, 0..top, layout);
        if top < span.len {
            if span.below {
                anchors.push((out.len(), span.old + span.len - bottom));
            }
            out.push(Row::Fold {
                gap,
                hidden: span.len - top - bottom,
                function: None,
                can_expand_up: span.below,
                can_expand_down: span.above,
            });
            push_context(&mut out, &span, span.len - bottom..span.len, layout);
        }
        if let Some(hunk) = hunks.get(gap) {
            push_hunk(&mut out, diff, hunk, layout);
        }
    }
    fill_functions(diff, &mut out, &anchors);
    out
}

/// Index of the first row of each hunk in `rows` (for next/previous change). Formatting-only hunks
/// are skipped.
pub fn hunk_starts(rows: &[Row]) -> Vec<usize> {
    let mut previous = false;
    let mut starts = Vec::new();
    for (ix, row) in rows.iter().enumerate() {
        let change = row.is_change();
        if change && !previous {
            starts.push(ix);
        }
        previous = change;
    }
    starts
}

fn push_context(out: &mut Vec<Row>, span: &Gap, lines: Range<u32>, layout: Layout) {
    let line = |side, ix| DiffLine {
        side,
        ix,
        kind: LineKind::Context,
        emphasis: Vec::new(),
        guides: None,
    };
    out.extend(lines.map(|at| {
        let (old, new) = (span.old + at, span.new + at);
        match layout {
            Layout::Unified => Row::Unified {
                old: Some(old),
                new: Some(new),
                line: line(Side::New, new),
            },
            Layout::Split => Row::Split {
                old: Some(line(Side::Old, old)),
                new: Some(line(Side::New, new)),
            },
        }
    }));
}

fn push_hunk(out: &mut Vec<Row>, diff: &FileDiff, hunk: &Hunk, layout: Layout) {
    let old_len = hunk.old.len() as u32;
    let new_len = hunk.new.len() as u32;
    let line = |side, ix| {
        if hunk.formatting_only {
            return DiffLine {
                side,
                ix,
                kind: LineKind::Formatting,
                emphasis: Vec::new(),
                guides: None,
            };
        }
        DiffLine {
            side,
            ix,
            kind: match side {
                Side::Old => LineKind::Removed,
                Side::New => LineKind::Added,
            },
            emphasis: diff
                .emphasis(side, ix)
                .iter()
                .map(|(_, range)| range.clone())
                .collect(),
            guides: indent_guides(diff.line(side, ix)),
        }
    };
    match layout {
        Layout::Split => out.extend((0..old_len.max(new_len)).map(|i| Row::Split {
            old: (i < old_len).then(|| line(Side::Old, hunk.old.start + i)),
            new: (i < new_len).then(|| line(Side::New, hunk.new.start + i)),
        })),
        // Formatting: the new lines, with the old number where paired.
        Layout::Unified if hunk.formatting_only => out.extend((0..new_len).map(|i| {
            let new = hunk.new.start + i;
            Row::Unified {
                old: (i < old_len).then_some(hunk.old.start + i),
                new: Some(new),
                line: line(Side::New, new),
            }
        })),
        Layout::Unified => {
            out.extend(hunk.old.clone().map(|old| Row::Unified {
                old: Some(old),
                new: None,
                line: line(Side::Old, old),
            }));
            out.extend(hunk.new.clone().map(|new| Row::Unified {
                old: None,
                new: Some(new),
                line: line(Side::New, new),
            }));
        }
    }
}

/// Leading whitespace width in cells (a tab counts 4), and whether it holds a tab.
fn indent(line: &str) -> (u32, bool) {
    let mut cells = 0;
    let mut tabs = false;
    for byte in line.bytes() {
        match byte {
            b' ' => cells += 1,
            b'\t' => {
                cells += 4;
                tabs = true;
            }
            _ => break,
        }
    }
    (cells, tabs)
}

/// Guides for a line indented by 2 or more cells.
fn indent_guides(line: &str) -> Option<IndentGuides> {
    let (cells, tabs) = indent(line);
    if cells < 2 {
        return None;
    }
    let unit = if tabs || cells % 4 == 0 { 4 } else { 2 };
    Some(IndentGuides {
        unit: unit as u8,
        levels: (cells / unit).min(u8::MAX as u32) as u8,
    })
}

/// Sets each fold's function context: the declaration enclosing the old line after it (`anchors`
/// ascending). That is the nearest line above that is indented less and is not blank, a closing
/// bracket or a comment; else git's default, the nearest line above starting with a letter, `_` or
/// `$`. One pass over the old lines.
fn fill_functions(diff: &FileDiff, rows: &mut [Row], anchors: &[(usize, u32)]) {
    let blank = |line: &str| line.trim().is_empty();
    // Candidate declarations above the scan position, indents strictly increasing: a line hides
    // every earlier one indented as much or more.
    let mut stack: Vec<(u32, u32)> = Vec::new();
    let mut fallback = None;
    let mut next = 0;
    for &(row, anchor) in anchors {
        for ix in next..anchor {
            let line = diff.old_line(ix);
            if blank(line) {
                continue;
            }
            let text = line.trim_start();
            if !text.starts_with(['}', ')', ']']) && !is_comment(text) {
                let cells = indent(line).0;
                while stack.last().is_some_and(|&(top, _)| top >= cells) {
                    stack.pop();
                }
                stack.push((cells, ix));
            }
            if line.starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '$') {
                fallback = Some(ix);
            }
        }
        next = anchor;
        let target = (anchor..diff.old_len())
            .map(|ix| diff.old_line(ix))
            .find(|line| !blank(line))
            .map_or(0, |line| indent(line).0);
        let enclosing = stack.partition_point(|&(cells, _)| cells < target);
        let found = enclosing.checked_sub(1).map(|at| stack[at].1).or(fallback);
        if let Row::Fold { function, .. } = &mut rows[row] {
            *function = found.map(|ix| cap(diff.old_line(ix).trim()));
        }
    }
}

fn is_comment(text: &str) -> bool {
    ["//", "#", "/*", "*", "--"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn cap(text: &str) -> String {
    match text.char_indices().nth(MAX_FUNCTION_CHARS) {
        Some((at, _)) => text[..at].to_owned(),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Whitespace;

    fn numbered(lines: Range<u32>) -> String {
        lines.map(|ix| format!("line {ix}\n")).collect()
    }

    /// 40 lines with lines 10 and 30 changed.
    fn two_hunks() -> FileDiff {
        let old = numbered(0..40);
        let new = old
            .replace("line 10\n", "LINE 10\n")
            .replace("line 30\n", "LINE 30\n");
        FileDiff::new(&old, &new, Whitespace::Exact)
    }

    fn folds(rows: &[Row]) -> Vec<(usize, u32)> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Fold { gap, hidden, .. } => Some((*gap, *hidden)),
                _ => None,
            })
            .collect()
    }

    fn shown_new(rows: &[Row]) -> Vec<u32> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Unified { new, .. } => *new,
                _ => None,
            })
            .collect()
    }

    #[test]
    fn split_pairs_lines_and_leaves_blanks() {
        let diff = FileDiff::new("a\nb\nc\nz\n", "A\nz\n", Whitespace::Exact);
        let rows = rows(&diff, &Context::default(), Layout::Split);
        let sides: Vec<_> = rows
            .iter()
            .map(|row| match row {
                Row::Split { old, new } => (
                    old.as_ref().map(|line| (line.ix, line.kind)),
                    new.as_ref().map(|line| (line.ix, line.kind)),
                ),
                row => panic!("unexpected {row:?}"),
            })
            .collect();
        assert_eq!(
            sides,
            [
                (Some((0, LineKind::Removed)), Some((0, LineKind::Added))),
                (Some((1, LineKind::Removed)), None),
                (Some((2, LineKind::Removed)), None),
                (Some((3, LineKind::Context)), Some((1, LineKind::Context))),
            ]
        );

        let unified = super::rows(&diff, &Context::default(), Layout::Unified);
        let order: Vec<_> = unified
            .iter()
            .map(|row| match row {
                Row::Unified { old, new, .. } => (*old, *new),
                row => panic!("unexpected {row:?}"),
            })
            .collect();
        assert_eq!(
            order,
            [
                (Some(0), None),
                (Some(1), None),
                (Some(2), None),
                (None, Some(0)),
                (Some(3), Some(1))
            ]
        );
        assert_eq!(hunk_starts(&unified), [0]);
    }

    #[test]
    fn folds_keep_three_lines_and_never_hide_one() {
        let diff = two_hunks();
        let rows = rows(&diff, &Context::default(), Layout::Unified);
        // Gap 0: lines 0..10, 3 shown; gap 1: 11..30, 3 + 3 shown; gap 2: 31..40, 3 shown.
        assert_eq!(folds(&rows), [(0, 7), (1, 13), (2, 6)]);
        assert_eq!(hunk_starts(&rows).len(), 2);

        // A gap of 7 lines between hunks hides only one, so it shows whole.
        let old = numbered(0..20);
        let new = old.replace("line 5\n", "x\n").replace("line 13\n", "y\n");
        let diff = FileDiff::new(&old, &new, Whitespace::Exact);
        let rows = super::rows(&diff, &Context::default(), Layout::Unified);
        assert_eq!(folds(&rows), [(0, 2), (2, 3)]);
    }

    #[test]
    fn expand_up_down_and_all() {
        let diff = two_hunks();
        let mut context = Context::default();
        context.expand(1, Expand::Down, 5);
        let rows = rows(&diff, &context, Layout::Unified);
        assert_eq!(folds(&rows)[1], (1, 8));
        // Down grows below hunk 0: lines 11..=18 shown, then 27..=29.
        let shown = shown_new(&rows);
        assert!(shown.contains(&18) && !shown.contains(&19) && shown.contains(&27));

        context.expand(1, Expand::Up, 20);
        let rows = super::rows(&diff, &context, Layout::Unified);
        assert_eq!(folds(&rows), [(0, 7), (2, 6)]);

        context.expand(0, Expand::All, 0);
        assert_eq!(
            folds(&super::rows(&diff, &context, Layout::Unified)),
            [(2, 6)]
        );
        context.expand_all();
        assert!(context.is_all());
        let rows = super::rows(&diff, &context, Layout::Unified);
        assert!(folds(&rows).is_empty());
        assert_eq!(rows.len(), 42);
        context.collapse_all();
        assert_eq!(context, Context::default());
    }

    #[test]
    fn fold_expand_directions_follow_the_neighbouring_hunks() {
        let rows = rows(&two_hunks(), &Context::default(), Layout::Split);
        let flags: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Fold {
                    can_expand_up,
                    can_expand_down,
                    ..
                } => Some((*can_expand_up, *can_expand_down)),
                _ => None,
            })
            .collect();
        assert_eq!(flags, [(true, false), (true, true), (false, true)]);
    }

    #[test]
    fn reveal_opens_from_the_nearer_hunk() {
        let diff = two_hunks();
        let mut context = Context::default();
        context.reveal(&diff, Side::New, 17);
        let shown = shown_new(&rows(&diff, &context, Layout::Unified));
        assert!(shown.contains(&17) && shown.contains(&20) && !shown.contains(&21));

        let mut context = Context::default();
        context.reveal(&diff, Side::Old, 24);
        let shown = shown_new(&rows(&diff, &context, Layout::Unified));
        assert!(shown.contains(&21) && !shown.contains(&20));

        // Before the first hunk only the hunk below can open.
        let mut context = Context::default();
        context.reveal(&diff, Side::New, 1);
        let rows = super::rows(&diff, &context, Layout::Unified);
        assert!(shown_new(&rows).contains(&1));
        assert_eq!(folds(&rows)[0], (1, 13));

        // Shown and changed lines need nothing.
        let mut context = Context::default();
        context.reveal(&diff, Side::New, 10);
        context.reveal(&diff, Side::New, 12);
        assert_eq!(context, Context::default());
    }

    #[test]
    fn unchanged_text_has_no_rows() {
        let diff = FileDiff::new("a\nb\n", "a\nb\n", Whitespace::Exact);
        assert!(diff.is_empty());
        assert!(rows(&diff, &Context::default(), Layout::Split).is_empty());
    }

    #[test]
    fn formatting_hunks_draw_as_context_and_are_not_navigated() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nfoo(x, y)\n";
        let new = "a\nb  c\nd\ne\nf\ng\nh\ni\nj\nfoo(x, z)\n";
        let diff = FileDiff::new(old, new, Whitespace::IgnoreFormatting);
        assert!(diff.hunks()[0].formatting_only);
        let mut context = Context::default();
        context.expand_all();
        let unified = rows(&diff, &context, Layout::Unified);
        assert!(matches!(
            &unified[1],
            Row::Unified { old: Some(1), new: Some(1), line } if line.kind == LineKind::Formatting
        ));
        assert!(matches!(
            &unified[2],
            Row::Unified {
                old: Some(3),
                new: Some(2),
                ..
            }
        ));
        assert_eq!(hunk_starts(&unified), [9]);

        let split = rows(&diff, &context, Layout::Split);
        assert!(
            matches!(&split[2], Row::Split { old: Some(old), new: None } if old.kind == LineKind::Formatting)
        );
        assert_eq!(hunk_starts(&split), [10]);
    }

    #[test]
    fn indent_guide_units() {
        let guides = |line| indent_guides(line).map(|g| (g.unit, g.levels));
        assert_eq!(guides("x"), None);
        assert_eq!(guides(" x"), None);
        assert_eq!(guides("  x"), Some((2, 1)));
        assert_eq!(guides("      x"), Some((2, 3)));
        assert_eq!(guides("        x"), Some((4, 2)));
        assert_eq!(guides("\t\tx"), Some((4, 2)));
        assert_eq!(guides("\t  x"), Some((4, 1)));
    }

    #[test]
    fn function_context_finds_the_enclosing_declaration() {
        let old = "\
use std::fmt;

impl Widget {
    /// Draws it.
    pub fn draw(&self, cx: &mut Cx) {
        let a = 1;
        if a > 0 {
            cx.paint();
        }
        let b = 2;
        let c = 3;
        let d = 4;
        let e = 5;
        let f = 6;
        let g = 7;
        let h = 8;
    }
}

fn helper() {
    one();
    two();
    three();
    four();
    five();
    six();
}
";
        let new = old
            .replace("let h = 8;", "let h = 9;")
            .replace("    six();", "    seven();");
        let diff = FileDiff::new(old, &new, Whitespace::Exact);
        let rows = rows(&diff, &Context::default(), Layout::Unified);
        let functions: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Fold { function, .. } => Some(function.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            functions,
            [
                Some("pub fn draw(&self, cx: &mut Cx) {".to_owned()),
                Some("fn helper() {".to_owned()),
            ]
        );
    }
}
