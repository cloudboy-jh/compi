//! Line diffs of two texts for the review pane: hunks, word emphasis, folds and find.
//!
//! Pure data; no GPUI. Build a [`FileDiff`] off the UI thread, then derive display [`Row`]s from it.

mod find;
mod rows;
mod words;

use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use imara_diff::{Algorithm, Diff, InternedInput, Interner};

pub use find::{Match, find};
pub use rows::{Context, DiffLine, Expand, IndentGuides, Layout, LineKind, Row, hunk_starts, rows};

/// How whitespace counts when lines are compared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Whitespace {
    /// Every byte counts; only line endings (`\n`, `\r\n`) are ignored.
    #[default]
    Exact,
    /// Lines are compared with all whitespace removed (git `-w`).
    IgnoreSpace,
    /// Like `IgnoreSpace`; a change whose old and new lines hold the same text once all whitespace is
    /// removed (reflow, line breaks, blank lines) is formatting only.
    IgnoreFormatting,
}

/// One of the two compared texts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The text before the change.
    Old,
    /// The text after the change.
    New,
}

/// One changed region with no context: old lines `old`, new lines `new` (0-based, half-open).
/// Either may be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The replaced old lines.
    pub old: Range<u32>,
    /// The new lines replacing them.
    pub new: Range<u32>,
    /// Lines differing only in formatting (`IgnoreFormatting`): drawn as context, not counted.
    pub formatting_only: bool,
}

/// Two texts compared line by line. Built off the UI thread; cheap to share (Arc inside).
#[derive(Clone)]
pub struct FileDiff {
    inner: Arc<Inner>,
}

struct Inner {
    old: SideText,
    new: SideText,
    hunks: Vec<Hunk>,
    added: u32,
    removed: u32,
}

/// One side's text, split into lines.
struct SideText {
    text: String,
    /// Each line's bytes in `text`, line ending excluded.
    lines: Vec<Range<usize>>,
    no_newline: bool,
    emphasis: Emphasis,
}

/// Changed-word byte ranges of paired lines as `(line, range)`, the range relative to the line,
/// sorted by line.
type Emphasis = Vec<(u32, Range<usize>)>;

impl SideText {
    fn new(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut lines = Vec::with_capacity(bytes.len() / 32 + 1);
        let mut start = 0;
        while start < bytes.len() {
            let (end, next) = match bytes[start..].iter().position(|&b| b == b'\n') {
                Some(at) => (start + at, start + at + 1),
                None => (bytes.len(), bytes.len()),
            };
            let end = if end > start && bytes[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            lines.push(start..end);
            start = next;
        }
        Self {
            text: text.to_owned(),
            lines,
            no_newline: !bytes.is_empty() && !bytes.ends_with(b"\n"),
            emphasis: Vec::new(),
        }
    }

    fn len(&self) -> u32 {
        self.lines.len() as u32
    }

    fn line(&self, ix: u32) -> &str {
        &self.text[self.lines[ix as usize].clone()]
    }

    /// Interning keys of every line.
    fn keys(&self, squash: bool) -> impl Iterator<Item = LineKey<'_>> {
        let last = self.lines.len().wrapping_sub(1);
        self.lines
            .iter()
            .enumerate()
            .map(move |(ix, range)| LineKey {
                text: &self.text[range.clone()],
                squash,
                no_newline: self.no_newline && ix == last,
            })
    }

    /// All non-whitespace bytes of `lines`, in order.
    fn squashed(&self, lines: Range<u32>) -> impl Iterator<Item = u8> + '_ {
        lines.flat_map(move |ix| non_space(self.line(ix)))
    }
}

/// A line as compared by the diff.
#[derive(Clone, Copy)]
struct LineKey<'a> {
    text: &'a str,
    /// Compare with all whitespace removed.
    squash: bool,
    /// The last line of a text with no final newline; never equal to a line that has one.
    no_newline: bool,
}

impl PartialEq for LineKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.no_newline == other.no_newline
            && if self.squash {
                non_space(self.text).eq(non_space(other.text))
            } else {
                self.text == other.text
            }
    }
}

impl Eq for LineKey<'_> {}

impl Hash for LineKey<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.no_newline.hash(state);
        if self.squash {
            // FNV-1a over the kept bytes, so the hash does not depend on where whitespace split them.
            let hash = non_space(self.text).fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
            state.write_u64(hash);
        } else {
            self.text.hash(state);
        }
    }
}

impl AsRef<[u8]> for LineKey<'_> {
    fn as_ref(&self) -> &[u8] {
        self.text.as_bytes()
    }
}

/// ASCII whitespace as git `-w` sees it.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b'\x0b' | b'\x0c')
}

fn non_space(text: &str) -> impl Iterator<Item = u8> + '_ {
    text.bytes().filter(|&byte| !is_space(byte))
}

impl FileDiff {
    /// Compares `old` with `new` line by line.
    pub fn new(old: &str, new: &str, whitespace: Whitespace) -> Self {
        let mut old = SideText::new(old);
        let mut new = SideText::new(new);
        let squash = whitespace != Whitespace::Exact;

        let mut input = InternedInput {
            before: Vec::new(),
            after: Vec::new(),
            interner: Interner::new(old.lines.len() + new.lines.len()),
        };
        input.update_before(old.keys(squash));
        input.update_after(new.keys(squash));
        let mut diff = Diff::compute(Algorithm::Histogram, &input);
        diff.postprocess_lines(&input);
        drop(input);

        let mut hunks = Vec::new();
        let (mut added, mut removed) = (0, 0);
        for hunk in diff.hunks() {
            let formatting_only = whitespace == Whitespace::IgnoreFormatting
                && old
                    .squashed(hunk.before.clone())
                    .eq(new.squashed(hunk.after.clone()));
            if !formatting_only {
                removed += hunk.before.len() as u32;
                added += hunk.after.len() as u32;
            }
            hunks.push(Hunk {
                old: hunk.before,
                new: hunk.after,
                formatting_only,
            });
        }

        let (old_emphasis, new_emphasis) = words::emphasis(&old, &new, &hunks, whitespace);
        old.emphasis = old_emphasis;
        new.emphasis = new_emphasis;

        Self {
            inner: Arc::new(Inner {
                old,
                new,
                hunks,
                added,
                removed,
            }),
        }
    }

    fn side(&self, side: Side) -> &SideText {
        match side {
            Side::Old => &self.inner.old,
            Side::New => &self.inner.new,
        }
    }

    /// Old line `ix` (0-based), without its line ending.
    pub fn old_line(&self, ix: u32) -> &str {
        self.inner.old.line(ix)
    }

    /// New line `ix` (0-based), without its line ending.
    pub fn new_line(&self, ix: u32) -> &str {
        self.inner.new.line(ix)
    }

    /// Line `ix` (0-based) of `side`, without its line ending.
    pub fn line(&self, side: Side, ix: u32) -> &str {
        self.side(side).line(ix)
    }

    /// Number of old lines.
    pub fn old_len(&self) -> u32 {
        self.inner.old.len()
    }

    /// Number of new lines.
    pub fn new_len(&self) -> u32 {
        self.inner.new.len()
    }

    /// The whole old text, for the highlighter.
    pub fn old_text(&self) -> &str {
        &self.inner.old.text
    }

    /// The whole new text, for the highlighter.
    pub fn new_text(&self) -> &str {
        &self.inner.new.text
    }

    /// Byte offset of each line start in old_text/new_text (for mapping highlighter styles).
    /// `ix` equal to the line count gives the text length.
    pub fn line_start(&self, side: Side, ix: u32) -> usize {
        let side = self.side(side);
        side.lines
            .get(ix as usize)
            .map_or(side.text.len(), |line| line.start)
    }

    /// Every changed region in order, formatting-only ones included.
    pub fn hunks(&self) -> &[Hunk] {
        &self.inner.hunks
    }

    /// Lines added under the chosen whitespace mode.
    pub fn added(&self) -> u32 {
        self.inner.added
    }

    /// Lines removed under the chosen whitespace mode.
    pub fn removed(&self) -> u32 {
        self.inner.removed
    }

    /// No hunks other than formatting-only ones: nothing to review.
    pub fn is_empty(&self) -> bool {
        self.inner.hunks.iter().all(|hunk| hunk.formatting_only)
    }

    /// The text of `side` does not end with a newline.
    pub fn no_newline_at_end(&self, side: Side) -> bool {
        self.side(side).no_newline
    }

    /// Changed-word byte ranges of line `ix` of `side`, relative to the line.
    fn emphasis(&self, side: Side, ix: u32) -> &[(u32, Range<usize>)] {
        let emphasis = &self.side(side).emphasis;
        let start = emphasis.partition_point(|(line, _)| *line < ix);
        let end = start + emphasis[start..].partition_point(|(line, _)| *line == ix);
        &emphasis[start..end]
    }
}

impl std::fmt::Debug for FileDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDiff")
            .field("old_len", &self.old_len())
            .field("new_len", &self.new_len())
            .field("hunks", &self.inner.hunks)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old: Range<u32>, new: Range<u32>) -> Hunk {
        Hunk {
            old,
            new,
            formatting_only: false,
        }
    }

    #[test]
    fn crlf_is_stripped_for_comparison_and_display() {
        let diff = FileDiff::new("a\r\nb\r\n", "a\nb\n", Whitespace::Exact);
        assert!(diff.is_empty());
        assert_eq!(diff.old_line(0), "a");
        assert_eq!(diff.line_start(Side::Old, 1), 3);
        assert_eq!(diff.line_start(Side::Old, 2), 6);
        assert!(!diff.no_newline_at_end(Side::Old));
    }

    #[test]
    fn missing_final_newline_changes_the_last_line() {
        let diff = FileDiff::new("a\nb", "a\nb\n", Whitespace::Exact);
        assert!(diff.no_newline_at_end(Side::Old));
        assert!(!diff.no_newline_at_end(Side::New));
        assert_eq!(diff.hunks(), &[hunk(1..2, 1..2)]);
        assert_eq!((diff.added(), diff.removed()), (1, 1));

        let diff = FileDiff::new("", "", Whitespace::Exact);
        assert_eq!(diff.old_len(), 0);
        assert!(!diff.no_newline_at_end(Side::Old));
        assert!(diff.is_empty());
    }

    #[test]
    fn ignore_space_drops_whitespace_changes() {
        let old = "fn a() {\n    x(1,2);\n}\n";
        let new = "fn a() {\n\tx(1, 2);\n}\n";
        assert_eq!(
            FileDiff::new(old, new, Whitespace::Exact).hunks(),
            &[hunk(1..2, 1..2)]
        );
        assert!(FileDiff::new(old, new, Whitespace::IgnoreSpace).is_empty());
        assert!(
            FileDiff::new(old, new, Whitespace::IgnoreSpace)
                .hunks()
                .is_empty()
        );
    }

    #[test]
    fn ignore_formatting_marks_reflow_and_blank_lines() {
        let old = "a\nlet x = f(one, two);\nb\nc\n";
        let new = "a\nlet x = f(\n    one,\n    two,\n);\nb\n\nc\n";
        let diff = FileDiff::new(old, new, Whitespace::IgnoreSpace);
        assert_eq!(diff.hunks().len(), 2);
        assert_eq!((diff.added(), diff.removed()), (5, 1));

        // The trailing comma is a real change; the blank line is formatting.
        let diff = FileDiff::new(old, new, Whitespace::IgnoreFormatting);
        assert_eq!(diff.hunks()[0], hunk(1..2, 1..5));
        assert!(diff.hunks()[1].formatting_only);
        assert_eq!((diff.added(), diff.removed()), (4, 1));

        let new = "a\nlet x = f(\n    one, two);\nb\n\nc\n";
        let diff = FileDiff::new(old, new, Whitespace::IgnoreFormatting);
        assert!(diff.hunks().iter().all(|hunk| hunk.formatting_only));
        assert!(diff.is_empty());
        assert_eq!((diff.added(), diff.removed()), (0, 0));
    }
}
