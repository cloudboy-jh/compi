use compi_protocol::{Row, ScreenMessage, ScreenSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorApply {
    Applied,
    Gap { expected: u64, actual: u64 },
}

#[derive(Default)]
pub struct ScreenMirror {
    snapshot: Option<ScreenSnapshot>,
}

impl ScreenMirror {
    pub fn snapshot(&self) -> Option<&ScreenSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn apply(&mut self, message: ScreenMessage) -> MirrorApply {
        match message {
            ScreenMessage::Snapshot { snapshot } => {
                self.snapshot = Some(snapshot);
                MirrorApply::Applied
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
                if let Some(scrollback) = delta.scrollback {
                    snapshot.scrollback = scrollback;
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
                MirrorApply::Applied
            }
        }
    }
}
