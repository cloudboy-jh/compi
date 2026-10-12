//! Find in diff.

use std::ops::Range;

use super::{FileDiff, Side};

/// A find hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// The side the line is on.
    pub side: Side,
    /// The line on that side (0-based).
    pub line: u32,
    /// Byte range in the line.
    pub range: Range<usize>,
}

/// Case-insensitive unless `query` has an uppercase letter (smart case).
///
/// Searches every new line and the old lines of non-formatting hunks (unchanged old lines repeat
/// new ones), in unified display order: within a hunk, its old lines come before its new lines.
/// Empty for an empty query or an [empty](FileDiff::is_empty) diff.
pub fn find(diff: &FileDiff, query: &str) -> Vec<Match> {
    let mut out = Vec::new();
    if query.is_empty() || diff.is_empty() {
        return out;
    }
    let matcher = Matcher::new(query);
    let mut search = |side: Side, lines: Range<u32>| {
        for line in lines {
            matcher.each(diff.line(side, line), |range| {
                out.push(Match { side, line, range })
            });
        }
    };
    let mut next_new = 0;
    for hunk in diff.hunks() {
        search(Side::New, next_new..hunk.new.start);
        if !hunk.formatting_only {
            search(Side::Old, hunk.old.clone());
        }
        search(Side::New, hunk.new.clone());
        next_new = hunk.new.end;
    }
    search(Side::New, next_new..diff.new_len());
    out
}

enum Matcher<'a> {
    /// Case-sensitive.
    Exact(&'a str),
    /// ASCII query, case-insensitive.
    Ascii(&'a [u8]),
    /// Any other query, lowercased.
    Folded(Vec<char>),
}

impl<'a> Matcher<'a> {
    fn new(query: &'a str) -> Self {
        if query.chars().any(char::is_uppercase) {
            Matcher::Exact(query)
        } else if query.is_ascii() {
            Matcher::Ascii(query.as_bytes())
        } else {
            Matcher::Folded(query.chars().flat_map(char::to_lowercase).collect())
        }
    }

    /// Calls `hit` with each non-overlapping match in `line`, left to right.
    fn each(&self, line: &str, mut hit: impl FnMut(Range<usize>)) {
        match self {
            Matcher::Exact(query) => {
                for (at, _) in line.match_indices(query) {
                    hit(at..at + query.len());
                }
            }
            Matcher::Ascii(query) => {
                let bytes = line.as_bytes();
                let mut at = 0;
                while at + query.len() <= bytes.len() {
                    if bytes[at..at + query.len()].eq_ignore_ascii_case(query) {
                        hit(at..at + query.len());
                        at += query.len();
                    } else {
                        at += 1;
                    }
                }
            }
            Matcher::Folded(query) => {
                let mut from = 0;
                for (at, _) in line.char_indices() {
                    if at < from {
                        continue;
                    }
                    if let Some(end) = folded_prefix(&line[at..], query) {
                        hit(at..at + end);
                        from = at + end;
                    }
                }
            }
        }
    }
}

/// Byte length of the prefix of `text` that lowercases to `query`.
fn folded_prefix(text: &str, query: &[char]) -> Option<usize> {
    let mut next = 0;
    for (at, c) in text.char_indices() {
        for lower in c.to_lowercase() {
            if query.get(next) != Some(&lower) {
                return None;
            }
            next += 1;
        }
        if next == query.len() {
            return Some(at + c.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Whitespace;

    fn hits(diff: &FileDiff, query: &str) -> Vec<(Side, u32, Range<usize>)> {
        find(diff, query)
            .into_iter()
            .map(|hit| (hit.side, hit.line, hit.range))
            .collect()
    }

    #[test]
    fn smart_case_and_display_order() {
        let diff = FileDiff::new(
            "Foo one\nkeep foo\nfoo old\n",
            "Foo one\nkeep foo\nFOO new foo\n",
            Whitespace::Exact,
        );
        assert_eq!(
            hits(&diff, "foo"),
            [
                (Side::New, 0, 0..3),
                (Side::New, 1, 5..8),
                (Side::Old, 2, 0..3),
                (Side::New, 2, 0..3),
                (Side::New, 2, 8..11),
            ]
        );
        assert_eq!(hits(&diff, "FOO"), [(Side::New, 2, 0..3)]);
        assert!(hits(&diff, "").is_empty());
    }

    #[test]
    fn folds_non_ascii_case() {
        let diff = FileDiff::new("x\n", "Größe ÄRGER\n", Whitespace::Exact);
        assert_eq!(hits(&diff, "äR"), []);
        assert_eq!(hits(&diff, "är"), [(Side::New, 0, 8..11)]);
        assert_eq!(hits(&diff, "größe"), [(Side::New, 0, 0..7)]);
    }
}
