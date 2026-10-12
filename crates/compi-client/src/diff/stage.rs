//! Hunk staging: the `SetIndexLines` write that puts one hunk of a diff into the Git index,
//! or takes it back out. Line endings travel untouched: lines are sliced from the texts
//! the diff was built from with [`FileDiff::line_start`], so a CRLF line stays CRLF and a
//! last line without a newline stays without one. The daemon applies Git's clean filter
//! when it hashes the result, as `git add` would.

use super::{FileDiff, Hunk, Side};
use compi_protocol::{FileVersion, GitAction};
use sha2::{Digest, Sha256};

/// Which way a hunk moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkWrite {
    /// The diff compares the index (old) with the work tree (new): the hunk's index lines
    /// become its work-tree lines.
    Stage,
    /// The diff compares `HEAD` (old) with the index (new): the hunk's index lines go back
    /// to its `HEAD` lines.
    Unstage,
}

/// The version the daemon checks the index text against: its length and SHA-256.
pub fn text_version(text: &str) -> FileVersion {
    FileVersion {
        len: text.len() as u64,
        sha256: Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

/// The write that moves `hunk` of `diff` (one of its hunks) for the file at repository
/// path `path`.
pub fn set_index_lines(diff: &FileDiff, hunk: &Hunk, write: HunkWrite, path: String) -> GitAction {
    // The index is the old side when staging, the new side when unstaging; the
    // replacement comes from the other side.
    let (index, lines, source, from) = match write {
        HunkWrite::Stage => (Side::Old, hunk.old.clone(), Side::New, hunk.new.clone()),
        HunkWrite::Unstage => (Side::New, hunk.new.clone(), Side::Old, hunk.old.clone()),
    };
    let text = |side: Side| match side {
        Side::Old => diff.old_text(),
        Side::New => diff.new_text(),
    };
    let replacement =
        &text(source)[diff.line_start(source, from.start)..diff.line_start(source, from.end)];
    GitAction::SetIndexLines {
        path,
        expected: text_version(text(index)),
        lines,
        replacement: replacement.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Whitespace;
    use std::ops::Range;

    /// What the daemon does with the write: split the index text into lines keeping their
    /// endings, replace `lines`, join.
    fn apply(index: &str, action: &GitAction) -> String {
        let GitAction::SetIndexLines {
            expected,
            lines,
            replacement,
            ..
        } = action
        else {
            panic!("not a SetIndexLines: {action:?}");
        };
        assert_eq!(
            *expected,
            text_version(index),
            "expected must match the index"
        );
        let parts: Vec<&str> = index.split_inclusive('\n').collect();
        let Range { start, end } = lines.start as usize..lines.end as usize;
        let mut out = parts[..start].concat();
        out.push_str(replacement);
        out.push_str(&parts[end..].concat());
        out
    }

    fn stage(index: &str, work: &str, hunk: usize) -> String {
        let diff = FileDiff::new(index, work, Whitespace::Exact);
        let action = set_index_lines(
            &diff,
            &diff.hunks()[hunk],
            HunkWrite::Stage,
            "file.txt".into(),
        );
        apply(index, &action)
    }

    fn unstage(head: &str, index: &str, hunk: usize) -> String {
        let diff = FileDiff::new(head, index, Whitespace::Exact);
        let action = set_index_lines(
            &diff,
            &diff.hunks()[hunk],
            HunkWrite::Unstage,
            "file.txt".into(),
        );
        apply(index, &action)
    }

    #[test]
    fn the_version_is_length_and_lowercase_sha256() {
        let version = text_version("abc");
        assert_eq!(version.len, 3);
        assert_eq!(
            version.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(text_version("").len, 0);
    }

    #[test]
    fn staging_one_of_two_hunks_leaves_the_other_out() {
        let index = "a\nb\nc\nd\ne\nf\ng\n";
        let work = "a\nB\nc\nd\ne\nF\ng\n";
        assert_eq!(stage(index, work, 0), "a\nB\nc\nd\ne\nf\ng\n");
        assert_eq!(stage(index, work, 1), "a\nb\nc\nd\ne\nF\ng\n");
        // Unstaging the first of two staged hunks keeps the second.
        let staged = "a\nB\nc\nd\ne\nF\ng\n";
        assert_eq!(unstage(index, staged, 0), "a\nb\nc\nd\ne\nF\ng\n");
        assert_eq!(unstage(index, staged, 1), "a\nB\nc\nd\ne\nf\ng\n");
    }

    #[test]
    fn crlf_lines_keep_their_endings() {
        let index = "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n";
        let work = "one\r\nTWO\r\nthree\r\nfour\r\nfive\r\nSIX\r\nseven\r\n";
        let diff = FileDiff::new(index, work, Whitespace::Exact);
        let GitAction::SetIndexLines {
            lines, replacement, ..
        } = set_index_lines(&diff, &diff.hunks()[0], HunkWrite::Stage, "f".into())
        else {
            unreachable!()
        };
        assert_eq!((lines, replacement.as_str()), (1..2, "TWO\r\n"));
        assert_eq!(
            stage(index, work, 1),
            "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nSIX\r\nseven\r\n"
        );
        assert_eq!(
            unstage(index, work, 0),
            "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nSIX\r\nseven\r\n"
        );
    }

    #[test]
    fn a_last_line_without_newline_stays_without_one() {
        // The work tree drops the final newline and changes the last line.
        assert_eq!(stage("a\nb\nc\n", "a\nb\nC", 0), "a\nb\nC");
        // The index lacks the final newline; the work tree adds a line after it.
        assert_eq!(stage("a\nb", "a\nb\nc\n", 0), "a\nb\nc\n");
        // A CRLF file whose last line has no ending.
        assert_eq!(stage("x\r\ny\r\nz", "x\r\ny\r\nZ", 0), "x\r\ny\r\nZ");
        // Unstaging restores HEAD's missing newline.
        assert_eq!(unstage("a\nb", "a\nb\n", 0), "a\nb");
    }

    #[test]
    fn pure_insertions_and_deletions() {
        // Insertion: the index side of the hunk is empty, so lines start..start.
        let diff = FileDiff::new("a\nb\n", "a\nnew\nb\n", Whitespace::Exact);
        let GitAction::SetIndexLines {
            lines, replacement, ..
        } = set_index_lines(&diff, &diff.hunks()[0], HunkWrite::Stage, "f".into())
        else {
            unreachable!()
        };
        assert_eq!((lines, replacement.as_str()), (1..1, "new\n"));
        assert_eq!(stage("a\nb\n", "a\nnew\nb\n", 0), "a\nnew\nb\n");
        assert_eq!(stage("", "first\r\n", 0), "first\r\n");
        assert_eq!(stage("a\n", "a\nend\n", 0), "a\nend\n");

        // Deletion: the work-tree side is empty, so the replacement is too.
        let diff = FileDiff::new("a\nold\nb\n", "a\nb\n", Whitespace::Exact);
        let GitAction::SetIndexLines {
            lines, replacement, ..
        } = set_index_lines(&diff, &diff.hunks()[0], HunkWrite::Stage, "f".into())
        else {
            unreachable!()
        };
        assert_eq!((lines, replacement.as_str()), (1..2, ""));
        assert_eq!(stage("a\nold\nb\n", "a\nb\n", 0), "a\nb\n");
        assert_eq!(stage("only\n", "", 0), "");

        // Unstaging a staged insertion deletes it from the index; a staged deletion
        // comes back.
        assert_eq!(unstage("a\nb\n", "a\nnew\nb\n", 0), "a\nb\n");
        assert_eq!(unstage("a\nold\nb\n", "a\nb\n", 0), "a\nold\nb\n");
    }

    #[test]
    fn expected_names_the_index_side() {
        let (head, index, work) = ("h\n", "i\n", "w\n");
        let diff = FileDiff::new(index, work, Whitespace::Exact);
        let staged = set_index_lines(&diff, &diff.hunks()[0], HunkWrite::Stage, "p".into());
        let diff = FileDiff::new(head, index, Whitespace::Exact);
        let unstaged = set_index_lines(&diff, &diff.hunks()[0], HunkWrite::Unstage, "p".into());
        for action in [staged, unstaged] {
            let GitAction::SetIndexLines { path, expected, .. } = action else {
                unreachable!()
            };
            assert_eq!((path.as_str(), expected), ("p", text_version(index)));
        }
    }
}
