//! Word-level emphasis on paired changed lines.

use std::ops::Range;

use imara_diff::{Algorithm, Diff, Interner, Token};

use super::{Emphasis, Hunk, SideText, Whitespace};

/// Lines longer than this get no emphasis.
const MAX_LINE_BYTES: usize = 1000;
/// A pair keeping less than this share (in percent) of the shorter line's non-whitespace bytes was
/// rewritten rather than edited, and gets no emphasis.
const MIN_KEPT_PERCENT: usize = 50;
/// The same for a pair whose sides each changed in one place: a clean replacement reads well even
/// when it is most of a short line.
const MIN_KEPT_PERCENT_ONE_CHANGE: usize = 25;

/// Changed-word ranges of every paired line, for each side.
pub(super) fn emphasis(
    old: &SideText,
    new: &SideText,
    hunks: &[Hunk],
    whitespace: Whitespace,
) -> (Emphasis, Emphasis) {
    let mut words = Words::default();
    let mut old_out = Vec::new();
    let mut new_out = Vec::new();
    for hunk in hunks.iter().filter(|hunk| !hunk.formatting_only) {
        let pairs = hunk.old.len().min(hunk.new.len()) as u32;
        for i in 0..pairs {
            let (o, n) = (hunk.old.start + i, hunk.new.start + i);
            words.pair(old.line(o), new.line(n), whitespace == Whitespace::Exact);
            old_out.extend(words.old_ranges.drain(..).map(|range| (o, range)));
            new_out.extend(words.new_ranges.drain(..).map(|range| (n, range)));
        }
    }
    (old_out, new_out)
}

/// Scratch space reused across line pairs.
#[derive(Default)]
struct Words<'a> {
    interner: Interner<&'a str>,
    old_spans: Vec<Span>,
    new_spans: Vec<Span>,
    old_tokens: Vec<Token>,
    new_tokens: Vec<Token>,
    diff: Diff,
    old_ranges: Vec<Range<usize>>,
    new_ranges: Vec<Range<usize>>,
}

/// A token's bytes in its line.
#[derive(Clone)]
struct Span {
    range: Range<usize>,
    space: bool,
}

impl<'a> Words<'a> {
    /// Fills `old_ranges`/`new_ranges` with the changed words of one pair; both stay empty when the
    /// pair is too long or mostly rewritten.
    fn pair(&mut self, old: &'a str, new: &'a str, exact: bool) {
        if old.len() > MAX_LINE_BYTES || new.len() > MAX_LINE_BYTES {
            return;
        }
        tokenize(old, &mut self.old_spans);
        tokenize(new, &mut self.new_spans);
        // Myers sizes its tables by the largest token id; keep ids per pair.
        self.interner.clear();
        self.old_tokens.clear();
        self.old_tokens.extend(
            self.old_spans
                .iter()
                .map(|span| self.interner.intern(&old[span.range.clone()])),
        );
        self.new_tokens.clear();
        self.new_tokens.extend(
            self.new_spans
                .iter()
                .map(|span| self.interner.intern(&new[span.range.clone()])),
        );
        self.diff.compute_with(
            Algorithm::Myers,
            &self.old_tokens,
            &self.new_tokens,
            self.interner.num_tokens(),
        );

        let solid = |spans: &[Span]| -> usize {
            spans
                .iter()
                .filter(|span| !span.space)
                .map(|span| span.range.len())
                .sum()
        };
        let kept: usize = self
            .old_spans
            .iter()
            .enumerate()
            .filter(|(ix, span)| !span.space && !self.diff.is_removed(*ix as u32))
            .map(|(_, span)| span.range.len())
            .sum();
        let shorter = solid(&self.old_spans).min(solid(&self.new_spans));
        if shorter == 0 || kept * 100 < shorter * MIN_KEPT_PERCENT_ONE_CHANGE {
            return;
        }
        // Some solid token is kept on both sides, so neither side's ranges can cover the whole line.
        let diff = &self.diff;
        changed_ranges(
            &self.old_spans,
            |ix| diff.is_removed(ix),
            exact,
            &mut self.old_ranges,
        );
        changed_ranges(
            &self.new_spans,
            |ix| diff.is_added(ix),
            exact,
            &mut self.new_ranges,
        );
        let one_change = self.old_ranges.len() <= 1 && self.new_ranges.len() <= 1;
        if !one_change && kept * 100 < shorter * MIN_KEPT_PERCENT {
            self.old_ranges.clear();
            self.new_ranges.clear();
        }
    }
}

/// Merges changed tokens into ranges; changed tokens separated only by whitespace join. Changed
/// whitespace counts only when `exact`.
fn changed_ranges(
    spans: &[Span],
    changed: impl Fn(u32) -> bool,
    exact: bool,
    out: &mut Vec<Range<usize>>,
) {
    let mut open: Option<Range<usize>> = None;
    for (ix, span) in spans.iter().enumerate() {
        let is_changed = changed(ix as u32);
        if span.space && !(is_changed && exact) {
            continue;
        }
        if is_changed {
            match &mut open {
                Some(range) => range.end = span.range.end,
                None => open = Some(span.range.clone()),
            }
        } else if let Some(range) = open.take() {
            out.push(range);
        }
    }
    out.extend(open);
}

#[derive(PartialEq)]
enum Class {
    Word,
    Space,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Punct
    }
}

/// Splits a line into words (alphanumeric/`_` runs), whitespace runs and single other chars.
fn tokenize(line: &str, out: &mut Vec<Span>) {
    out.clear();
    let mut chars = line.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let kind = class(c);
        let mut end = start + c.len_utf8();
        if kind != Class::Punct {
            while let Some(&(at, next)) = chars.peek() {
                if class(next) != kind {
                    break;
                }
                end = at + next.len_utf8();
                chars.next();
            }
        }
        out.push(Span {
            range: start..end,
            space: kind == Class::Space,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(old: &str, new: &str, exact: bool) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
        let mut words = Words::default();
        words.pair(old, new, exact);
        (words.old_ranges, words.new_ranges)
    }

    fn texts<'a>(line: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|range| &line[range.clone()]).collect()
    }

    #[test]
    fn emphasises_changed_words_only() {
        let (old, new) = ("let x = 1;", "let x = 42;");
        let (o, n) = pair(old, new, true);
        assert_eq!(texts(old, &o), ["1"]);
        assert_eq!(texts(new, &n), ["42"]);
    }

    #[test]
    fn merges_changes_across_whitespace_only() {
        let (old, new) = (
            "return serve(host, port);",
            "return serve(addr hostname, port);",
        );
        let (o, n) = pair(old, new, true);
        assert_eq!(texts(old, &o), ["host"]);
        assert_eq!(texts(new, &n), ["addr hostname"]);

        // An unchanged word between two changes splits them.
        let (old, new) = ("a b c d e f", "x b c y e f");
        let (o, _) = pair(old, new, true);
        assert_eq!(texts(old, &o), ["a", "d"]);
    }

    #[test]
    fn whitespace_changes_count_only_when_exact() {
        let (old, new) = ("f(a,b) + g", "f(a, b) - g");
        let (_, n) = pair(old, new, true);
        assert_eq!(texts(new, &n), [" ", "-"]);
        let (_, n) = pair(old, new, false);
        assert_eq!(texts(new, &n), ["-"]);
    }

    #[test]
    fn skips_rewritten_and_long_lines() {
        let (o, n) = pair("let total = items.len();", "for item in list {", true);
        assert!(o.is_empty() && n.is_empty());

        // Scattered changes over most of the line read as a rewrite...
        let (o, n) = pair("match tree {", "leaf_node(tree, pane).is_some()", true);
        assert!(o.is_empty() && n.is_empty());
        // ...one replacement does not.
        let (old, new) = (
            ".left(px(TITLEBAR_BRAND_WIDTH))",
            ".left(px(leading_width))",
        );
        let (o, n) = pair(old, new, true);
        assert_eq!(texts(old, &o), ["TITLEBAR_BRAND_WIDTH"]);
        assert_eq!(texts(new, &n), ["leading_width"]);
        let (o, n) = pair("fn foo() {", "struct Bar {", true);
        assert!(o.is_empty() && n.is_empty());

        let long = format!("let x = [{}];", "1, ".repeat(400));
        let longer = long.replace("];", "2];");
        let (o, n) = pair(&long, &longer, true);
        assert!(o.is_empty() && n.is_empty());

        // A blank side has nothing to compare against.
        let (o, n) = pair("", "foo", true);
        assert!(o.is_empty() && n.is_empty());
    }
}
