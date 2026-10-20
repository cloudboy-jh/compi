//! How Monitor's pages divide their width, kept pure: the Overview grid (how many
//! columns, how wide a card, where each card sits and what the keyboard reaches from
//! it) and the resource pages' figure grid, graph height and Memory by type ring.
//!
//! Widths are whole pixels, so columns and gaps never add up past the content.

/// A card narrower than this would squeeze its figures; the grid drops a column.
pub const MIN_CARD_WIDTH: f32 = 264.0;
/// Content never grows wider than this; a wider page centres it.
pub const MAX_CONTENT_WIDTH: f32 = 1280.0;
/// Cards narrower than this draw compactly.
pub const COMPACT_CARD_WIDTH: f32 = 300.0;
/// Pages narrower than this get the smaller padding.
const WIDE_PAGE: f32 = 480.0;
/// Content narrower than this gets the smaller gap.
const WIDE_CONTENT: f32 = 480.0;
/// A figure of a resource page wants at least this much width.
const FIGURE_WIDTH: f32 = 150.0;
/// Memory by type draws ring and legend side by side from this card width.
const RING_BESIDE_WIDTH: f32 = 480.0;
/// Memory by type cards narrower than this draw the smaller ring.
const SMALL_RING_BELOW: f32 = 360.0;

/// The cards of Overview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OverviewCard {
    Cpu,
    Memory,
    Disk,
    Network,
    MemoryByType,
    MemoryByApp,
}

/// A card in its row and how many columns it spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub card: OverviewCard,
    pub span: usize,
}

const fn placed(card: OverviewCard, span: usize) -> Placed {
    Placed { card, span }
}

/// Overview's rows, top to bottom, each a full row of the grid.
pub type OverviewRows = &'static [&'static [Placed]];

use OverviewCard::{Cpu, Disk, Memory, MemoryByApp, MemoryByType, Network};

static THREE_WITH_MEMORY: OverviewRows = &[
    &[placed(Cpu, 1), placed(Memory, 1), placed(Disk, 1)],
    &[
        placed(Network, 1),
        placed(MemoryByType, 1),
        placed(MemoryByApp, 1),
    ],
];
static TWO_WITH_MEMORY: OverviewRows = &[
    &[placed(Cpu, 1), placed(Memory, 1)],
    &[placed(Disk, 1), placed(Network, 1)],
    &[placed(MemoryByType, 1), placed(MemoryByApp, 1)],
];
static ONE_WITH_MEMORY: OverviewRows = &[
    &[placed(Cpu, 1)],
    &[placed(Memory, 1)],
    &[placed(Disk, 1)],
    &[placed(Network, 1)],
    &[placed(MemoryByType, 1)],
    &[placed(MemoryByApp, 1)],
];
static THREE: OverviewRows = &[
    &[placed(Cpu, 1), placed(Memory, 1), placed(Disk, 1)],
    &[placed(Network, 1), placed(MemoryByApp, 2)],
];
static TWO: OverviewRows = &[
    &[placed(Cpu, 1), placed(Memory, 1)],
    &[placed(Disk, 1), placed(Network, 1)],
    &[placed(MemoryByApp, 2)],
];
static ONE: OverviewRows = &[
    &[placed(Cpu, 1)],
    &[placed(Memory, 1)],
    &[placed(Disk, 1)],
    &[placed(Network, 1)],
    &[placed(MemoryByApp, 1)],
];

/// The padding around a page `page_width` wide and the width of its content.
pub fn page_frame(page_width: f32) -> (f32, f32) {
    let padding = if page_width >= WIDE_PAGE { 24.0 } else { 16.0 };
    let content_width = (page_width - 2.0 * padding).clamp(1.0, MAX_CONTENT_WIDTH);
    (padding, content_width)
}

/// The gap between cards in content `content_width` wide.
pub fn gap(content_width: f32) -> f32 {
    if content_width < WIDE_CONTENT {
        12.0
    } else {
        16.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverviewLayout {
    pub padding: f32,
    pub gap: f32,
    pub content_width: f32,
    pub columns: usize,
    pub card_width: f32,
    /// The columns Memory by type spans; 0 without it.
    pub memory_span: usize,
    /// The columns Memory by app spans.
    pub apps_span: usize,
    pub compact: bool,
    pub rows: OverviewRows,
}

impl OverviewLayout {
    /// The width of a card spanning `span` columns.
    pub fn width(&self, span: usize) -> f32 {
        let span = span.max(1);
        span as f32 * self.card_width + (span - 1) as f32 * self.gap
    }
}

/// Overview on a page `page_width` wide, with or without the Memory by type card: the
/// page's padding around [`overview_grid`] of the content.
pub fn overview_layout(page_width: f32, memory_by_type: bool) -> OverviewLayout {
    let (padding, content_width) = page_frame(page_width);
    OverviewLayout {
        padding,
        ..overview_grid(content_width, memory_by_type)
    }
}

/// Overview's cards across `content_width`, without padding (Settings → Monitor draws
/// them on its own page): the most columns (of 3, 2, 1) whose cards stay at least
/// [`MIN_CARD_WIDTH`], and the cards placed so every row is full.
pub fn overview_grid(content_width: f32, memory_by_type: bool) -> OverviewLayout {
    let content_width = content_width.clamp(1.0, MAX_CONTENT_WIDTH);
    let gap = gap(content_width);
    let per_column = |columns: usize| (content_width - gap * (columns - 1) as f32) / columns as f32;
    let columns = [3, 2, 1]
        .into_iter()
        .find(|&columns| per_column(columns) >= MIN_CARD_WIDTH)
        .unwrap_or(1);
    let card_width = per_column(columns).floor().max(1.0);
    let rows = match (columns, memory_by_type) {
        (3, true) => THREE_WITH_MEMORY,
        (2, true) => TWO_WITH_MEMORY,
        (_, true) => ONE_WITH_MEMORY,
        (3, false) => THREE,
        (2, false) => TWO,
        (_, false) => ONE,
    };
    let span_of = |card: OverviewCard| {
        rows.iter()
            .flat_map(|row| row.iter())
            .find(|placed| placed.card == card)
            .map_or(0, |placed| placed.span)
    };
    OverviewLayout {
        padding: 0.0,
        gap,
        content_width,
        columns,
        card_width,
        memory_span: span_of(MemoryByType),
        apps_span: span_of(MemoryByApp),
        compact: card_width < COMPACT_CARD_WIDTH,
        rows,
    }
}

/// The height of Memory by app's treemap in a card whose body is `inner_width` wide.
pub fn treemap_height(inner_width: f32) -> f32 {
    (inner_width * 2.0 / 3.0).clamp(160.0, 320.0)
}

/// A keyboard move between Overview's cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardMove {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// Where `card` sits: its row, its index in the row and the columns it covers.
fn locate(rows: OverviewRows, card: OverviewCard) -> Option<(usize, usize, (usize, usize))> {
    rows.iter().enumerate().find_map(|(row, items)| {
        let mut start = 0;
        items.iter().enumerate().find_map(|(index, placed)| {
            let columns = (start, start + placed.span);
            start += placed.span;
            (placed.card == card).then_some((row, index, columns))
        })
    })
}

/// The card of `row` covering most of `columns`; the leftmost on a tie.
fn overlapping(items: &[Placed], (from, to): (usize, usize)) -> Option<OverviewCard> {
    let mut start = 0;
    let mut best: Option<(usize, OverviewCard)> = None;
    for placed in items {
        let end = start + placed.span;
        let overlap = to.min(end).saturating_sub(from.max(start));
        if best.is_none_or(|(most, _)| overlap > most) {
            best = Some((overlap, placed.card));
        }
        start = end;
    }
    best.map(|(_, card)| card)
}

/// The card highlighted after `step` from `current` in `rows`: Left and Right move
/// within a row, Up and Down to the card above or below covering most of this one's
/// columns, Home and End to the first and last card. Without a highlight, or with one on
/// a card not drawn, every move starts at the first card; `None` without cards.
pub fn move_card(
    rows: OverviewRows,
    current: Option<OverviewCard>,
    step: CardMove,
) -> Option<OverviewCard> {
    let first = rows.first()?.first()?.card;
    let Some((row, index, columns)) = current.and_then(|card| locate(rows, card)) else {
        return Some(first);
    };
    let items = rows[row];
    Some(match step {
        CardMove::Left => items[index.saturating_sub(1)].card,
        CardMove::Right => items[(index + 1).min(items.len() - 1)].card,
        CardMove::Up => row
            .checked_sub(1)
            .and_then(|above| overlapping(rows[above], columns))
            .unwrap_or(items[index].card),
        CardMove::Down => rows
            .get(row + 1)
            .and_then(|below| overlapping(below, columns))
            .unwrap_or(items[index].card),
        CardMove::Home => first,
        CardMove::End => rows.last()?.last()?.card,
    })
}

/// Columns of a resource page's figure grid `inner_width` wide holding `count`
/// figures: as many [`FIGURE_WIDTH`] columns as fit, at least two, at most `count`.
pub fn figure_columns(inner_width: f32, count: usize) -> usize {
    ((inner_width / FIGURE_WIDTH).floor().max(0.0) as usize)
        .max(2)
        .min(count.max(1))
}

/// The width of each of `columns` equal cells in `width`, with `gap` between them.
pub fn cell_width(width: f32, columns: usize, gap: f32) -> f32 {
    let columns = columns.max(1);
    ((width - gap * (columns - 1) as f32) / columns as f32)
        .floor()
        .max(1.0)
}

/// A resource page's history graph height for content `content_width` wide.
pub fn graph_height(content_width: f32) -> f32 {
    (content_width * 0.28).clamp(120.0, 260.0)
}

/// A resource page's big value size for content `content_width` wide.
pub fn resource_value_size(content_width: f32) -> f32 {
    if content_width < WIDE_CONTENT {
        28.0
    } else {
        36.0
    }
}

/// How a Memory by type card `card_width` wide draws: ring beside the legend (else
/// above it, centred) and the ring's diameter.
pub fn ring_layout(card_width: f32) -> (bool, f32) {
    (
        card_width >= RING_BESIDE_WIDTH,
        if card_width < SMALL_RING_BELOW {
            104.0
        } else {
            120.0
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cards(layout: &OverviewLayout) -> Vec<Vec<(OverviewCard, usize)>> {
        layout
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|placed| (placed.card, placed.span))
                    .collect()
            })
            .collect()
    }

    /// The page width whose content is exactly `content` wide (wide padding).
    fn page(content: f32) -> f32 {
        content + 48.0
    }

    #[test]
    fn padding_gap_and_content_follow_the_page() {
        let wide = overview_layout(800.0, true);
        assert_eq!(
            (wide.padding, wide.content_width, wide.gap),
            (24.0, 752.0, 16.0)
        );
        let narrow = overview_layout(479.0, true);
        assert_eq!(
            (narrow.padding, narrow.content_width, narrow.gap),
            (16.0, 447.0, 12.0)
        );
        let at = overview_layout(480.0, true);
        assert_eq!((at.padding, at.content_width, at.gap), (24.0, 432.0, 12.0));
        assert_eq!(
            overview_layout(3000.0, true).content_width,
            MAX_CONTENT_WIDTH
        );
        assert_eq!(overview_layout(10.0, false).content_width, 1.0);
        assert_eq!(overview_layout(10.0, false).card_width, 1.0);
    }

    #[test]
    fn columns_are_the_most_that_keep_cards_wide_enough() {
        for memory in [true, false] {
            // 3 columns need 3 * 264 + 2 * 16 = 824.
            assert_eq!(overview_layout(page(824.0), memory).columns, 3);
            assert_eq!(overview_layout(page(823.0), memory).columns, 2);
            // 2 columns need 2 * 264 + 16 = 544.
            assert_eq!(overview_layout(page(544.0), memory).columns, 2);
            assert_eq!(overview_layout(page(543.0), memory).columns, 1);
            assert_eq!(overview_layout(page(3000.0), memory).columns, 3);
        }
        assert_eq!(overview_layout(200.0, false).columns, 1);
        assert_eq!(overview_layout(page(824.0), true).card_width, 264.0);
        assert_eq!(overview_layout(page(825.0), true).card_width, 264.0);
        assert_eq!(overview_layout(page(827.0), true).card_width, 265.0);
    }

    #[test]
    fn rows_never_overflow_the_content() {
        for memory in [false, true] {
            for width in 200..=3000 {
                let layout = overview_layout(width as f32, memory);
                for row in layout.rows {
                    let spans: usize = row.iter().map(|placed| placed.span).sum();
                    assert_eq!(spans, layout.columns, "row not full at {width}");
                    let used: f32 = row
                        .iter()
                        .map(|placed| layout.width(placed.span))
                        .sum::<f32>()
                        + layout.gap * (row.len() - 1) as f32;
                    assert!(
                        used <= layout.content_width || layout.card_width == 1.0,
                        "{used} > {} at {width}",
                        layout.content_width
                    );
                }
                assert_eq!(layout.card_width, layout.card_width.floor());
            }
        }
    }

    #[test]
    fn cards_are_placed_in_full_rows() {
        let three = overview_layout(page(900.0), true);
        assert_eq!(
            cards(&three),
            [
                vec![(Cpu, 1), (Memory, 1), (Disk, 1)],
                vec![(Network, 1), (MemoryByType, 1), (MemoryByApp, 1)],
            ]
        );
        assert_eq!((three.memory_span, three.apps_span), (1, 1));
        assert_eq!(three.width(2), 2.0 * three.card_width + three.gap);
        let two = overview_layout(page(600.0), true);
        assert_eq!(
            cards(&two),
            [
                vec![(Cpu, 1), (Memory, 1)],
                vec![(Disk, 1), (Network, 1)],
                vec![(MemoryByType, 1), (MemoryByApp, 1)],
            ]
        );
        let one = overview_layout(page(400.0), true);
        assert_eq!(one.rows.len(), 6);
        assert_eq!((one.memory_span, one.apps_span), (1, 1));
        let three = overview_layout(page(900.0), false);
        assert_eq!(
            cards(&three),
            [
                vec![(Cpu, 1), (Memory, 1), (Disk, 1)],
                vec![(Network, 1), (MemoryByApp, 2)],
            ]
        );
        assert_eq!((three.memory_span, three.apps_span), (0, 2));
        let two = overview_layout(page(600.0), false);
        assert_eq!(
            cards(&two),
            [
                vec![(Cpu, 1), (Memory, 1)],
                vec![(Disk, 1), (Network, 1)],
                vec![(MemoryByApp, 2)],
            ]
        );
        let one = overview_layout(page(400.0), false);
        assert_eq!(one.rows.len(), 5);
        assert_eq!(one.apps_span, 1);
        assert_eq!(treemap_height(120.0), 160.0);
        assert_eq!(treemap_height(300.0), 200.0);
        assert_eq!(treemap_height(900.0), 320.0);
    }

    #[test]
    fn narrow_cards_are_compact() {
        // Two columns of 299: 2 * 299 + 16 = 614.
        assert!(overview_layout(page(614.0), true).compact);
        assert_eq!(overview_layout(page(614.0), true).card_width, 299.0);
        assert!(!overview_layout(page(616.0), true).compact);
        assert!(overview_layout(page(824.0), true).compact);
        assert!(!overview_layout(page(932.0), true).compact);
    }

    #[test]
    fn keys_follow_the_grid() {
        let rows = THREE_WITH_MEMORY;
        let step = |from, step| move_card(rows, Some(from), step);
        assert_eq!(move_card(rows, None, CardMove::Down), Some(Cpu));
        assert_eq!(step(MemoryByType, CardMove::Up), Some(Memory));
        assert_eq!(step(MemoryByType, CardMove::Left), Some(Network));
        assert_eq!(step(MemoryByType, CardMove::Right), Some(MemoryByApp));
        assert_eq!(step(MemoryByApp, CardMove::Right), Some(MemoryByApp));
        assert_eq!(step(MemoryByApp, CardMove::Up), Some(Disk));
        assert_eq!(step(MemoryByApp, CardMove::Down), Some(MemoryByApp));
        assert_eq!(step(Disk, CardMove::Down), Some(MemoryByApp));
        assert_eq!(step(Cpu, CardMove::Down), Some(Network));
        assert_eq!(step(Network, CardMove::Up), Some(Cpu));
        assert_eq!(step(Cpu, CardMove::Left), Some(Cpu));
        assert_eq!(step(Disk, CardMove::Right), Some(Disk));
        assert_eq!(step(Disk, CardMove::Home), Some(Cpu));
        assert_eq!(step(Cpu, CardMove::End), Some(MemoryByApp));
        // Without Memory by type, Memory by app spans two columns: from it, Up goes to
        // Memory (the leftmost of the two it covers), and from Disk, Down reaches it.
        let three = |from, step| move_card(THREE, Some(from), step);
        assert_eq!(three(MemoryByApp, CardMove::Up), Some(Memory));
        assert_eq!(three(Disk, CardMove::Down), Some(MemoryByApp));
        assert_eq!(three(MemoryByApp, CardMove::Left), Some(Network));
        // Two columns: Memory by app spans the last row.
        let two = |from, step| move_card(TWO, Some(from), step);
        assert_eq!(two(Network, CardMove::Down), Some(MemoryByApp));
        assert_eq!(two(MemoryByApp, CardMove::Up), Some(Disk));
        let two = |from, step| move_card(TWO_WITH_MEMORY, Some(from), step);
        assert_eq!(two(Network, CardMove::Down), Some(MemoryByApp));
        assert_eq!(two(MemoryByType, CardMove::Right), Some(MemoryByApp));
        // A highlight on a card not drawn starts over at the first card.
        assert_eq!(
            move_card(THREE, Some(MemoryByType), CardMove::Left),
            Some(Cpu)
        );
        assert_eq!(move_card(ONE, Some(Memory), CardMove::Down), Some(Disk));
        assert_eq!(move_card(&[], None, CardMove::Down), None);
    }

    #[test]
    fn resource_pages_size_their_figures_graph_and_ring() {
        assert_eq!(figure_columns(1000.0, 6), 6);
        assert_eq!(figure_columns(600.0, 6), 4);
        assert_eq!(figure_columns(299.0, 6), 2);
        assert_eq!(figure_columns(100.0, 6), 2);
        assert_eq!(figure_columns(1000.0, 4), 4);
        assert_eq!(cell_width(100.0, 3, 5.0), 30.0);
        assert_eq!(cell_width(10.0, 4, 8.0), 1.0);
        assert_eq!(graph_height(300.0), 120.0);
        assert_eq!(graph_height(600.0), 168.0);
        assert_eq!(graph_height(1280.0), 260.0);
        assert_eq!(resource_value_size(479.0), 28.0);
        assert_eq!(resource_value_size(480.0), 36.0);
        assert_eq!(ring_layout(480.0), (true, 120.0));
        assert_eq!(ring_layout(479.0), (false, 120.0));
        assert_eq!(ring_layout(359.0), (false, 104.0));
    }
}
