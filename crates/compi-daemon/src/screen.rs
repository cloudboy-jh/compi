//! Ownership-preserving conversion from engine output to protocol v7 values.
use crate::terminal::{Delta, Snapshot};
use compi_protocol::{ScreenDelta, ScreenSnapshot};

pub fn snapshot(state: Snapshot) -> ScreenSnapshot {
    ScreenSnapshot {
        sequence: state.sequence,
        cols: state.cols,
        rows: state.rows,
        cells: state.cells,
        scrollback: state.scrollback,
        cursor: state.cursor,
        modes: state.modes,
        title: state.title,
        current_directory: state.current_directory,
        images: state.images,
        placements: state.placements,
    }
}

pub fn delta(change: Delta) -> ScreenDelta {
    ScreenDelta {
        sequence: change.sequence,
        cols: change.cols,
        rows: change.rows,
        row_updates: change.row_updates,
        scrollback: change.scrollback,
        cursor: change.cursor,
        modes: change.modes,
        title: change.title,
        current_directory: change.current_directory,
        images: change.images,
        latency_ids: change.latency_ids,
        placements: change.placements,
        clipboard_writes: change.clipboard_writes,
    }
}
