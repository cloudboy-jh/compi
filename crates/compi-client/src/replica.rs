use compi_protocol::{Row, ScreenMessage, ScreenSnapshot, ScrollbackChange};
use parking_lot::Mutex;
use std::sync::Arc;

use crate::viewport::visible_base;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorApply {
    Applied,
    Gap { expected: u64, actual: u64 },
}

#[derive(Default)]
pub struct ScreenMirror {
    snapshot: Option<ScreenSnapshot>,
    /// The identity of the first history row. Rows keep theirs while rows are appended
    /// and evicted; a replaced history gets identities never used before.
    first_line: u64,
    /// Bumped by every applied message: screen rows have no identity of their own.
    revision: u64,
    /// The rows last shown, expanded at the screen's width.
    visible: Mutex<Option<VisibleRows>>,
}

struct VisibleRows {
    first_line: u64,
    width: u16,
    revision: u64,
    rows: Arc<[Row]>,
}

impl ScreenMirror {
    pub fn snapshot(&self) -> Option<&ScreenSnapshot> {
        self.snapshot.as_ref()
    }

    /// The screen's height of rows shown `scroll_offset` rows up into the history, and
    /// the absolute row of the first. History rows are expanded only when the window
    /// moves to rows not shown before or when the screen rows in it may have changed.
    pub fn visible_rows(&self, scroll_offset: usize) -> Option<(Arc<[Row]>, usize)> {
        let snapshot = self.snapshot.as_ref()?;
        let base = visible_base(snapshot, scroll_offset);
        let first_line = self.first_line + base as u64;
        let height = snapshot.cells.len();
        let mut visible = self.visible.lock();
        if let Some(cached) = visible.as_ref()
            && cached.first_line == first_line
            && cached.width == snapshot.cols
            && cached.rows.len() == height
            && (cached.revision == self.revision || base + height <= snapshot.scrollback.len())
        {
            return Some((cached.rows.clone(), base));
        }
        let width = usize::from(snapshot.cols);
        let rows: Arc<[Row]> = snapshot
            .scrollback
            .range(base..)
            .map(|row| row.to_row(width))
            .chain(snapshot.cells.iter().cloned())
            .take(height)
            .collect();
        *visible = Some(VisibleRows {
            first_line,
            width: snapshot.cols,
            revision: self.revision,
            rows: rows.clone(),
        });
        Some((rows, base))
    }

    pub fn apply(&mut self, message: ScreenMessage) -> MirrorApply {
        match message {
            ScreenMessage::Snapshot { snapshot } => {
                self.first_line += self
                    .snapshot
                    .as_ref()
                    .map_or(0, |old| old.scrollback.len() as u64);
                self.snapshot = Some(snapshot);
            }
            ScreenMessage::Delta { delta } => {
                let Some(snapshot) = self.snapshot.as_mut() else {
                    return MirrorApply::Gap {
                        expected: 0,
                        actual: delta.sequence,
                    };
                };
                let expected = snapshot.sequence.saturating_add(1);
                if delta.sequence != expected {
                    return MirrorApply::Gap {
                        expected,
                        actual: delta.sequence,
                    };
                }
                snapshot.sequence = delta.sequence;
                snapshot.cols = delta.cols;
                snapshot.rows = delta.rows;
                if snapshot.cells.len() != usize::from(delta.rows) {
                    snapshot.cells =
                        vec![Row::blank(usize::from(delta.cols)); usize::from(delta.rows)];
                }
                for update in delta.row_updates {
                    if let Some(row) = snapshot.cells.get_mut(usize::from(update.index)) {
                        *row = update.row;
                    }
                }
                match delta.scrollback {
                    Some(ScrollbackChange::Append { evicted, rows }) => {
                        let evicted = (evicted as usize).min(snapshot.scrollback.len());
                        snapshot.scrollback.drain(..evicted);
                        self.first_line += evicted as u64;
                        snapshot.scrollback.extend(rows);
                    }
                    Some(ScrollbackChange::Replace { rows }) => {
                        self.first_line += snapshot.scrollback.len() as u64;
                        snapshot.scrollback = rows.into();
                    }
                    None => {}
                }
                snapshot.cursor = delta.cursor;
                snapshot.modes = delta.modes;
                snapshot.title = delta.title;
                snapshot.current_directory = delta.current_directory;
                if let Some(images) = delta.images {
                    snapshot.images = images;
                }
                if let Some(placements) = delta.placements {
                    snapshot.placements = placements;
                }
            }
        }
        self.revision += 1;
        MirrorApply::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compi_protocol::{Cell, Color, HistoryRow, ScreenDelta};
    use std::collections::VecDeque;

    fn row(text: &str, wrapped: bool) -> Row {
        let mut row = Row::blank(8);
        for (cell, character) in row.cells.iter_mut().zip(text.chars()) {
            cell.text = character.to_string().into();
        }
        row.wrapped = wrapped;
        row
    }

    fn line(index: usize) -> Row {
        let mut row = row(&format!("l{index}"), index.is_multiple_of(3));
        row.cells[0].foreground = Color::Indexed((index % 8) as u8);
        row
    }

    fn snapshot(sequence: u64, scrollback: VecDeque<HistoryRow>) -> ScreenSnapshot {
        ScreenSnapshot {
            sequence,
            cols: 8,
            rows: 2,
            cells: vec![row("screen", false), row("prompt", false)],
            scrollback,
            cursor: Default::default(),
            modes: Default::default(),
            title: String::new(),
            current_directory: None,
            images: Vec::new(),
            placements: Vec::new(),
        }
    }

    fn delta(sequence: u64, scrollback: Option<ScrollbackChange>) -> ScreenMessage {
        ScreenMessage::Delta {
            delta: ScreenDelta {
                sequence,
                cols: 8,
                rows: 2,
                row_updates: Vec::new(),
                scrollback,
                cursor: Default::default(),
                modes: Default::default(),
                title: String::new(),
                current_directory: None,
                images: None,
                latency_ids: Vec::new(),
                placements: None,
                clipboard_writes: Vec::new(),
                shell_action: None,
            },
        }
    }

    fn history(lines: std::ops::Range<usize>) -> VecDeque<HistoryRow> {
        lines
            .map(|index| HistoryRow::from_row(&line(index)))
            .collect()
    }

    #[test]
    fn appends_with_eviction_and_replaces_like_the_authority() {
        let mut mirror = ScreenMirror::default();
        mirror.apply(ScreenMessage::Snapshot {
            snapshot: snapshot(1, history(0..5)),
        });
        // An authority bounded to 6 lines, its rows built from full rows, appended to in
        // uneven steps that evict.
        let mut authority = history(0..5);
        let mut next = 5;
        for (sequence, appended) in (2..).zip([1, 4, 0, 6, 2]) {
            let rows: Vec<_> = history(next..next + appended).into();
            next += appended;
            authority.extend(rows.iter().cloned());
            let evicted = authority.len().saturating_sub(6);
            authority.drain(..evicted);
            assert_eq!(
                mirror.apply(delta(
                    sequence,
                    Some(ScrollbackChange::Append {
                        evicted: evicted as u32,
                        rows,
                    }),
                )),
                MirrorApply::Applied
            );
            assert_eq!(
                mirror.snapshot().unwrap(),
                &snapshot(sequence, authority.clone())
            );
        }
        assert_eq!(authority, history(12..18));
        let rewrapped = history(40..43);
        mirror.apply(delta(
            7,
            Some(ScrollbackChange::Replace {
                rows: rewrapped.iter().cloned().collect(),
            }),
        ));
        assert_eq!(mirror.snapshot().unwrap(), &snapshot(7, rewrapped));
        mirror.apply(delta(8, None));
        assert_eq!(mirror.snapshot().unwrap().scrollback, history(40..43));
        assert_eq!(
            mirror.apply(delta(10, None)),
            MirrorApply::Gap {
                expected: 9,
                actual: 10
            }
        );
    }

    #[test]
    fn visible_history_rows_expand_at_the_screen_width_and_are_reused_while_appending() {
        let mut mirror = ScreenMirror::default();
        mirror.apply(ScreenMessage::Snapshot {
            snapshot: snapshot(1, history(0..10)),
        });
        let (rows, base) = mirror.visible_rows(4).unwrap();
        assert_eq!(base, 6);
        assert_eq!(rows.as_ref(), [line(6), line(7)]);
        let (live, base) = mirror.visible_rows(1).unwrap();
        assert_eq!(base, 9);
        assert_eq!(live.as_ref(), [line(9), row("screen", false)]);

        // The same history rows after three are appended and one evicted, so four rows
        // up becomes seven: the rows expanded before, not expanded again.
        let (rows, _) = mirror.visible_rows(4).unwrap();
        mirror.apply(delta(
            2,
            Some(ScrollbackChange::Append {
                evicted: 1,
                rows: history(10..13).into(),
            }),
        ));
        let (kept, base) = mirror.visible_rows(7).unwrap();
        assert_eq!(base, 5);
        assert!(Arc::ptr_eq(&rows, &kept));

        // A window that shows screen rows is rebuilt after any change.
        let (live, _) = mirror.visible_rows(1).unwrap();
        let mut message = delta(3, None);
        if let ScreenMessage::Delta { delta } = &mut message {
            delta.row_updates.push(compi_protocol::RowUpdate {
                index: 0,
                row: row("changed", false),
            });
        }
        mirror.apply(message);
        let (changed, _) = mirror.visible_rows(1).unwrap();
        assert!(!Arc::ptr_eq(&live, &changed));
        assert_eq!(changed.as_ref(), [line(12), row("changed", false)]);

        // Replaced rows are new rows, even at the same offset.
        mirror.apply(delta(
            4,
            Some(ScrollbackChange::Replace {
                rows: history(20..32).into(),
            }),
        ));
        let (replaced, _) = mirror.visible_rows(6).unwrap();
        assert_eq!(replaced.as_ref(), [line(26), line(27)]);
        assert_eq!(
            replaced[0].cells[5],
            Cell::default(),
            "trailing blanks come back as default cells"
        );
    }
}
