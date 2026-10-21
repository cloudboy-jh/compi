//! The Processes table's columns as the window arranges them (`ClientState::
//! monitor_columns`, saved with the window): dragging the seam at a header's right edge
//! sets its width, dragging a header moves its column (Name stays first), and a right
//! click on the headers opens a menu with every column checked while shown (Name always
//! is) and Reset columns. A narrow table still gives up User, then Threads, then CPU
//! time from the columns shown (`ColumnLayout::visible_columns`).
use super::processes::{ProcessList, process_cell};
use super::*;
use crate::monitor::{Column, ColumnLayout, Direction, NAME_MIN_WIDTH, reorder_target};

/// The seam a header's width drags by, inside its right edge.
const SEAM_WIDTH: f32 = 6.0;
/// A header moves once dragged this far; less is a click, which sorts.
const MOVE_THRESHOLD: f32 = 4.0;

/// A header held by the pointer.
pub(super) struct ColumnDrag {
    pane_id: PaneId,
    column: Column,
    /// The pointer's x in the window when it went down.
    origin: f32,
    kind: DragKind,
}

enum DragKind {
    /// The seam, from the width the column had.
    Resize { width: f32 },
    /// The header, over the columns as drawn when it was taken, `offset` from there.
    Move {
        drawn: Vec<(Column, f32)>,
        offset: f32,
    },
}

impl ColumnDrag {
    /// Whether the drag did what a click must not do again: any resize, or a header
    /// moved past [`MOVE_THRESHOLD`].
    fn took_click(&self) -> bool {
        match &self.kind {
            DragKind::Resize { .. } => true,
            DragKind::Move { offset, .. } => offset.abs() >= MOVE_THRESHOLD,
        }
    }

    /// While a header moves: the column it would go before, or `None` for last.
    fn target(&self) -> Option<Option<Column>> {
        match &self.kind {
            DragKind::Move { drawn, offset } if offset.abs() >= MOVE_THRESHOLD => {
                Some(reorder_target(drawn, self.column, *offset))
            }
            _ => None,
        }
    }
}

/// The columns `layout` shows in a table `table_width` wide, each at the width drawn:
/// Name takes what the others leave.
fn drawn_columns(layout: &ColumnLayout, table_width: f32) -> Vec<(Column, f32)> {
    let columns = layout.visible_columns(table_width);
    let others: f32 = columns
        .iter()
        .filter(|&&column| column != Column::Name)
        .map(|&column| layout.width(column))
        .sum();
    columns
        .into_iter()
        .map(|column| {
            let width = if column == Column::Name {
                (table_width - others).max(NAME_MIN_WIDTH)
            } else {
                layout.width(column)
            };
            (column, width)
        })
        .collect()
}

impl CompiApp {
    /// Heading `column` (`index` of `drawn`, `width` wide) of pane `pane_id`'s table:
    /// its label with the sort arrow, its seam, and while a header moves, the accent
    /// edge where it would land.
    pub(super) fn render_column_header(
        &self,
        pane_id: &PaneId,
        list: &ProcessList,
        (index, column, width): (usize, Column, f32),
        drawn: &[(Column, f32)],
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = *self.colors();
        let muted = color(modal_text_color(colors.muted, &colors));
        let accent = color(colors.accent);
        let drag = self
            .monitor
            .column_drag
            .as_ref()
            .filter(|drag| drag.pane_id == *pane_id);
        let target = drag.and_then(ColumnDrag::target);
        let held = drag.is_some_and(|drag| drag.column == column && drag.took_click());
        let lands_before = target == Some(Some(column)) && !held;
        let lands_after = target == Some(None) && index + 1 == drawn.len();
        let sorted = list.sort.column == column;
        let arrow = sorted.then(|| {
            controls::glyph(
                match list.sort.direction {
                    Direction::Ascending => IconName::ArrowUp,
                    Direction::Descending => IconName::ArrowDown,
                },
                muted,
            )
        });
        // The arrow sits on the inner side: before right-aligned labels, after others.
        let (before, after) = if column.numeric() {
            (arrow, None)
        } else {
            (None, arrow)
        };
        let (sort_pane, move_pane, resize_pane) =
            (pane_id.clone(), pane_id.clone(), pane_id.clone());
        process_cell(div().id(("monitor-column", index)), column, width)
            .relative()
            .gap_1()
            .cursor_pointer()
            .rounded_sm()
            .hover(move |style| style.bg(color(colors.surface_hover)))
            .when(sorted, |cell| cell.font_weight(FontWeight::SEMIBOLD))
            .when(held, |cell| cell.bg(color(colors.surface_hover)))
            .when(lands_before, |cell| cell.border_l_2().border_color(accent))
            .when(lands_after, |cell| cell.border_r_2().border_color(accent))
            .children(before)
            .child(column.label())
            .children(after)
            .when(column != Column::Name, |cell| {
                cell.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, _| {
                        this.take_column(&move_pane, column, event.position.x, false);
                    }),
                )
                .child(
                    div()
                        .id(("monitor-column-seam", index))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(px(SEAM_WIDTH))
                        .cursor_col_resize()
                        .hover(move |style| style.bg(accent.opacity(0.4)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.take_column(&resize_pane, column, event.position.x, true);
                                cx.stop_propagation();
                            }),
                        ),
                )
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                // A drag that resized or moved a column is not a click.
                if this
                    .monitor
                    .column_drag
                    .as_ref()
                    .is_some_and(ColumnDrag::took_click)
                {
                    return;
                }
                this.sort_process_table(&sort_pane, column);
                cx.notify();
            }))
            .into_any_element()
    }

    /// The pointer took `column`'s header (or, `resize`, its seam) at window x `x`.
    fn take_column(&mut self, pane_id: &PaneId, column: Column, x: Pixels, resize: bool) {
        let layout = &self.state.monitor_columns;
        let kind = if resize {
            DragKind::Resize {
                width: layout.width(column),
            }
        } else {
            let Some(pane) = self.monitor.panes.get(pane_id) else {
                return;
            };
            DragKind::Move {
                drawn: drawn_columns(layout, pane.list.table_width.get()),
                offset: 0.0,
            }
        };
        self.monitor.column_drag = Some(ColumnDrag {
            pane_id: pane_id.clone(),
            column,
            origin: f32::from(x),
            kind,
        });
    }

    /// The pointer, holding a header, is at window x `x`: a seam sets its column's
    /// width as it goes; a header shows where it would land. Whether a header is held.
    pub(in crate::gui::workspace) fn drag_monitor_column(&mut self, x: Pixels) -> bool {
        let Some(drag) = &mut self.monitor.column_drag else {
            return false;
        };
        let delta = f32::from(x) - drag.origin;
        match &mut drag.kind {
            DragKind::Resize { width } => {
                self.state
                    .monitor_columns
                    .set_width(drag.column, *width + delta);
            }
            DragKind::Move { offset, .. } => *offset = delta,
        }
        true
    }

    /// The pointer let go of a header: a moved one lands, and the layout is saved.
    /// Whether a header was held.
    pub(in crate::gui::workspace) fn finish_monitor_column_drag(&mut self) -> bool {
        let Some(drag) = self.monitor.column_drag.take() else {
            return false;
        };
        let target = drag.target();
        match (drag.kind, target) {
            (DragKind::Resize { .. }, _) => {}
            (DragKind::Move { .. }, Some(before)) => {
                self.state.monitor_columns.move_before(drag.column, before);
            }
            // Barely moved: a click, which sorted.
            (DragKind::Move { .. }, None) => return true,
        }
        self.save_state();
        true
    }

    /// Right-click on the headers: every column, checked while shown (Name always
    /// shows), then Reset columns.
    pub(super) fn open_columns_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let layout = &self.state.monitor_columns;
        let mut entries: Vec<controls::MenuEntry> = layout
            .order()
            .iter()
            .map(|&column| {
                let shown = !layout.is_hidden(column);
                controls::menu_item(
                    column.label(),
                    cx.listener(move |this, _, _, cx| {
                        this.state.monitor_columns.set_hidden(column, shown);
                        this.save_state();
                        cx.notify();
                    }),
                )
                .checked(shown)
                .disabled(column == Column::Name)
                .into()
            })
            .collect();
        entries.push(controls::MenuEntry::Separator);
        entries.push(
            controls::menu_item(
                "Reset columns",
                cx.listener(|this, _, _, cx| {
                    this.state.monitor_columns = ColumnLayout::default();
                    this.save_state();
                    cx.notify();
                }),
            )
            .disabled(layout.is_default())
            .into(),
        );
        self.open_menu(position, Anchor::TopLeft, entries, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_drawn_with_what_the_other_columns_leave() {
        let mut layout = ColumnLayout::default();
        layout.set_hidden(Column::User, true);
        layout.set_width(Column::Cpu, 100.0);
        let drawn = drawn_columns(&layout, 1000.0);
        let others: f32 = drawn[1..].iter().map(|(_, width)| width).sum();
        assert_eq!(drawn[0], (Column::Name, 1000.0 - others));
        assert!(drawn.contains(&(Column::Cpu, 100.0)));
        assert!(!drawn.iter().any(|(column, _)| *column == Column::User));
        // Narrow, Name keeps its least width and columns drop.
        let narrow = drawn_columns(&layout, 300.0);
        assert_eq!(narrow[0], (Column::Name, NAME_MIN_WIDTH));
        assert!(narrow.len() < drawn.len());
    }
}
