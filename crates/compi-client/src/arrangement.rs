//! Tab arrangements: built-in and named presets, mirror/flip, swap, and restore.
//!
//! Arrangements preserve existing leaves; growth plans add actor-assigned slots
//! for one atomic transaction. Panes fill slots in reading order (depth first).
use crate::layout::Rect;
use compi_protocol::{LayoutNode, PaneId, PlannedLayoutNode, SplitAxis, SurfaceId};
use serde::{Deserialize, Serialize};

pub const MAX_NAMED_PRESETS: usize = 32;
pub const MAX_PRESET_SLOTS: usize = 16;
pub const MAX_PRESET_NAME_CHARS: usize = 64;
const MAIN_RATIO: f32 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Columns,
    Rows,
    Grid,
    MainSide,
    MainTop,
    Equalize,
}

impl Builtin {
    pub const ALL: [Self; 6] = [
        Self::Columns,
        Self::Rows,
        Self::Grid,
        Self::MainSide,
        Self::MainTop,
        Self::Equalize,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Columns => "columns",
            Self::Rows => "rows",
            Self::Grid => "grid",
            Self::MainSide => "main-side",
            Self::MainTop => "main-top",
            Self::Equalize => "equalize",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Columns => "Columns",
            Self::Rows => "Rows",
            Self::Grid => "Grid",
            Self::MainSide => "Main + stack (side)",
            Self::MainTop => "Main + stack (top)",
            Self::Equalize => "Equalize",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|builtin| builtin.id() == id)
    }

    /// Whether the main pane (normally the focused pane) takes the first slot.
    pub const fn uses_main(self) -> bool {
        matches!(self, Self::MainSide | Self::MainTop)
    }
}

/// Mirror swaps left and right; flip swaps top and bottom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transform {
    pub mirror: bool,
    pub flip: bool,
}

/// A pane-free split shape, as saved by a named preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Shape {
    Slot,
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<Shape>,
        second: Box<Shape>,
    },
}

impl Shape {
    pub fn of(layout: &LayoutNode) -> Self {
        match layout {
            LayoutNode::Pane { .. } => Self::Slot,
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => Self::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(Self::of(first)),
                second: Box::new(Self::of(second)),
            },
        }
    }

    pub fn slots(&self) -> usize {
        match self {
            Self::Slot => 1,
            Self::Split { first, second, .. } => first.slots() + second.slots(),
        }
    }

    /// A named preset needs at least one split and a bounded slot count.
    pub fn validate(&self) -> Result<(), &'static str> {
        fn ratios(shape: &Shape) -> bool {
            match shape {
                Shape::Slot => true,
                Shape::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => valid_ratio(*ratio) && ratios(first) && ratios(second),
            }
        }
        let slots = self.slots();
        if slots < 2 {
            return Err("a layout preset needs at least two panes");
        }
        if slots > MAX_PRESET_SLOTS {
            return Err("a layout preset can hold at most 16 panes");
        }
        if !ratios(self) {
            return Err("split ratios must be strictly between zero and one");
        }
        Ok(())
    }
}

/// What to arrange a tab into.
#[derive(Clone, Copy, Debug)]
pub enum Preset<'a> {
    Builtin(Builtin),
    Named(&'a Shape),
}

pub fn valid_ratio(ratio: f32) -> bool {
    ratio.is_finite() && ratio > 0.0 && ratio < 1.0
}

pub fn valid_preset_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed == name
        && name.chars().count() <= MAX_PRESET_NAME_CHARS
        && !name.chars().any(char::is_control)
}

type Leaf = (PaneId, SurfaceId);

/// Leaves in reading order.
pub fn leaves(layout: &LayoutNode) -> Vec<Leaf> {
    fn collect(node: &LayoutNode, output: &mut Vec<Leaf>) {
        match node {
            LayoutNode::Pane {
                pane_id,
                surface_id,
            } => output.push((pane_id.clone(), surface_id.clone())),
            LayoutNode::Split { first, second, .. } => {
                collect(first, output);
                collect(second, output);
            }
        }
    }
    let mut output = Vec::new();
    collect(layout, &mut output);
    output
}

/// Arrange `current` into `preset`. `main` takes the first slot of the main +
/// stack presets; other presets keep reading order.
pub fn arrange(
    current: &LayoutNode,
    preset: Preset<'_>,
    main: Option<&PaneId>,
    transform: Transform,
) -> LayoutNode {
    let arranged = match preset {
        Preset::Builtin(Builtin::Equalize) => equalize(current),
        Preset::Builtin(builtin) => {
            let mut order = leaves(current);
            if builtin.uses_main()
                && let Some(index) =
                    main.and_then(|main| order.iter().position(|(id, _)| id == main))
            {
                let leaf = order.remove(index);
                order.insert(0, leaf);
            }
            fill(&builtin_shape(builtin, order.len()), &mut order.into_iter())
        }
        Preset::Named(shape) => {
            let order = leaves(current);
            fill(&fit(shape.clone(), order.len()), &mut order.into_iter())
        }
    };
    apply_transform(arranged, transform)
}

/// Propose a complete arrangement with new slots, without allocating identities
/// or changing any existing pane. The actor validates and commits the whole plan.
pub fn plan_growth(current: &LayoutNode, preset: Preset<'_>, total: usize) -> PlannedLayoutNode {
    fn collect_panes<'a>(node: &'a LayoutNode, panes: &mut Vec<&'a PaneId>) {
        match node {
            LayoutNode::Pane { pane_id, .. } => panes.push(pane_id),
            LayoutNode::Split { first, second, .. } => {
                collect_panes(first, panes);
                collect_panes(second, panes);
            }
        }
    }
    fn fill_plan<'a>(
        shape: &Shape,
        panes: &mut impl Iterator<Item = &'a PaneId>,
    ) -> PlannedLayoutNode {
        match shape {
            Shape::Slot => panes.next().map_or(PlannedLayoutNode::NewPane, |pane_id| {
                PlannedLayoutNode::ExistingPane {
                    pane_id: pane_id.clone(),
                }
            }),
            Shape::Split {
                axis,
                ratio,
                first,
                second,
            } => PlannedLayoutNode::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(fill_plan(first, panes)),
                second: Box::new(fill_plan(second, panes)),
            },
        }
    }
    let shape = match preset {
        Preset::Builtin(Builtin::Equalize) => equalize_shape(&fit(Shape::of(current), total)),
        Preset::Builtin(builtin) => builtin_shape(builtin, total),
        Preset::Named(shape) => fit(shape.clone(), total),
    };
    let mut panes = Vec::with_capacity(total);
    collect_panes(current, &mut panes);
    fill_plan(&shape, &mut panes.into_iter())
}

/// Several tabs' trees side by side with equal widths, each keeping its own
/// splits. Presets applied to the result arrange every pane of every tab, in
/// tab order, which is how tabs are merged into one.
pub fn combine(layouts: &[&LayoutNode]) -> Option<LayoutNode> {
    let (first, rest) = layouts.split_first()?;
    Some(match combine(rest) {
        None => (*first).clone(),
        Some(rest) => LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            ratio: 1.0 / layouts.len() as f32,
            first: Box::new((*first).clone()),
            second: Box::new(rest),
        },
    })
}

/// Mirror and/or flip an arrangement in place of its current tree.
pub fn apply_transform(layout: LayoutNode, transform: Transform) -> LayoutNode {
    match layout {
        pane @ LayoutNode::Pane { .. } => pane,
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first = apply_transform(*first, transform);
            let second = apply_transform(*second, transform);
            let reverse = match axis {
                SplitAxis::Horizontal => transform.mirror,
                SplitAxis::Vertical => transform.flip,
            };
            let (first, second, ratio) = if reverse {
                (second, first, 1.0 - ratio)
            } else {
                (first, second, ratio)
            };
            LayoutNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }
        }
    }
}

/// Exchange two panes' positions. `None` when either is absent or they match.
pub fn swap(layout: &LayoutNode, a: &PaneId, b: &PaneId) -> Option<LayoutNode> {
    if a == b {
        return None;
    }
    let all = leaves(layout);
    let leaf_a = all.iter().find(|(id, _)| id == a)?.clone();
    let leaf_b = all.iter().find(|(id, _)| id == b)?.clone();
    fn replace(node: &LayoutNode, a: &Leaf, b: &Leaf) -> LayoutNode {
        match node {
            LayoutNode::Pane { pane_id, .. } => {
                let (pane_id, surface_id) = if pane_id == &a.0 {
                    b.clone()
                } else if pane_id == &b.0 {
                    a.clone()
                } else {
                    return node.clone();
                };
                LayoutNode::Pane {
                    pane_id,
                    surface_id,
                }
            }
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => LayoutNode::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(replace(first, a, b)),
                second: Box::new(replace(second, a, b)),
            },
        }
    }
    Some(replace(layout, &leaf_a, &leaf_b))
}

/// The previous arrangement fitted to the tab's current panes: panes keep their
/// earlier positions and panes added since then join the last slot.
pub fn restore(previous: &LayoutNode, current: &LayoutNode) -> Option<LayoutNode> {
    let present = leaves(current);
    let kept =
        crate::layout::without_panes(previous, &|id| !present.iter().any(|(pane, _)| pane == id))?;
    let mut order: Vec<Leaf> = leaves(&kept)
        .into_iter()
        .filter_map(|(id, _)| present.iter().find(|(pane, _)| pane == &id).cloned())
        .collect();
    for leaf in &present {
        if !order.iter().any(|(id, _)| id == &leaf.0) {
            order.push(leaf.clone());
        }
    }
    let shape = fit(Shape::of(&kept), order.len());
    Some(fill(&shape, &mut order.into_iter()))
}

/// Each pane's placement as fractions of the tab area, for previews.
pub fn pane_fractions(layout: &LayoutNode) -> Vec<(PaneId, Rect)> {
    fn place(node: &LayoutNode, area: Rect, output: &mut Vec<(PaneId, Rect)>) {
        match node {
            LayoutNode::Pane { pane_id, .. } => output.push((pane_id.clone(), area)),
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, b) = match axis {
                    SplitAxis::Horizontal => {
                        let width = area.width * ratio;
                        (
                            Rect { width, ..area },
                            Rect {
                                x: area.x + width,
                                width: area.width - width,
                                ..area
                            },
                        )
                    }
                    SplitAxis::Vertical => {
                        let height = area.height * ratio;
                        (
                            Rect { height, ..area },
                            Rect {
                                y: area.y + height,
                                height: area.height - height,
                                ..area
                            },
                        )
                    }
                };
                place(first, a, output);
                place(second, b, output);
            }
        }
    }
    let mut output = Vec::new();
    place(
        layout,
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        &mut output,
    );
    output
}

fn builtin_shape(builtin: Builtin, count: usize) -> Shape {
    let count = count.max(1);
    match builtin {
        Builtin::Columns => even(SplitAxis::Horizontal, count),
        Builtin::Rows => even(SplitAxis::Vertical, count),
        Builtin::Grid => {
            let columns = (1..=count).find(|c| c * c >= count).unwrap_or(1);
            let rows = count.div_ceil(columns);
            let row_shapes = (0..rows)
                .map(|row| {
                    let cells = if row + 1 == rows {
                        count - columns * (rows - 1)
                    } else {
                        columns
                    };
                    even(SplitAxis::Horizontal, cells)
                })
                .collect();
            chain(SplitAxis::Vertical, row_shapes)
        }
        Builtin::MainSide | Builtin::MainTop if count == 1 => Shape::Slot,
        Builtin::MainSide => main_stack(SplitAxis::Horizontal, count),
        Builtin::MainTop => main_stack(SplitAxis::Vertical, count),
        Builtin::Equalize => unreachable!("equalize keeps the current shape"),
    }
}

fn main_stack(axis: SplitAxis, count: usize) -> Shape {
    Shape::Split {
        axis,
        ratio: MAIN_RATIO,
        first: Box::new(Shape::Slot),
        second: Box::new(even(perpendicular(Some(axis)), count - 1)),
    }
}

fn even(axis: SplitAxis, count: usize) -> Shape {
    chain(axis, vec![Shape::Slot; count.max(1)])
}

/// Equal shares along `axis`, preserving item order.
fn chain(axis: SplitAxis, mut items: Vec<Shape>) -> Shape {
    let len = items.len();
    if len <= 1 {
        return items.pop().unwrap_or(Shape::Slot);
    }
    let rest = items.split_off(1);
    Shape::Split {
        axis,
        ratio: 1.0 / len as f32,
        first: Box::new(items.pop().expect("chain has a first item")),
        second: Box::new(chain(axis, rest)),
    }
}

const fn perpendicular(axis: Option<SplitAxis>) -> SplitAxis {
    match axis {
        Some(SplitAxis::Horizontal) => SplitAxis::Vertical,
        Some(SplitAxis::Vertical) | None => SplitAxis::Horizontal,
    }
}

/// Resize a shape to `count` slots: drop trailing slots, or stack extra panes
/// evenly in the last slot, perpendicular to that slot's parent split.
fn fit(mut shape: Shape, count: usize) -> Shape {
    let count = count.max(1);
    while shape.slots() > count {
        shape = without_last_slot(shape).unwrap_or(Shape::Slot);
    }
    let missing = count - shape.slots();
    if missing > 0 {
        shape = expand_last_slot(shape, missing + 1, None);
    }
    shape
}

fn without_last_slot(shape: Shape) -> Option<Shape> {
    match shape {
        Shape::Slot => None,
        Shape::Split {
            axis,
            ratio,
            first,
            second,
        } => Some(match without_last_slot(*second) {
            None => *first,
            Some(second) => Shape::Split {
                axis,
                ratio,
                first,
                second: Box::new(second),
            },
        }),
    }
}

fn expand_last_slot(shape: Shape, count: usize, parent: Option<SplitAxis>) -> Shape {
    match shape {
        Shape::Slot => even(perpendicular(parent), count),
        Shape::Split {
            axis,
            ratio,
            first,
            second,
        } => Shape::Split {
            axis,
            ratio,
            first,
            second: Box::new(expand_last_slot(*second, count, Some(axis))),
        },
    }
}

/// Assign leaves to slots in reading order; `shape` must have exactly as many slots.
fn fill(shape: &Shape, order: &mut impl Iterator<Item = Leaf>) -> LayoutNode {
    match shape {
        Shape::Slot => {
            let (pane_id, surface_id) = order.next().expect("shape fits the pane count");
            LayoutNode::Pane {
                pane_id,
                surface_id,
            }
        }
        Shape::Split {
            axis,
            ratio,
            first,
            second,
        } => LayoutNode::Split {
            axis: *axis,
            ratio: *ratio,
            first: Box::new(fill(first, order)),
            second: Box::new(fill(second, order)),
        },
    }
}

/// Keep the structure; give every pane an equal share along each split's axis.
fn equalize(layout: &LayoutNode) -> LayoutNode {
    fn span(node: &LayoutNode, along: SplitAxis) -> usize {
        match node {
            LayoutNode::Split {
                axis,
                first,
                second,
                ..
            } if *axis == along => span(first, along) + span(second, along),
            _ => 1,
        }
    }
    match layout {
        LayoutNode::Pane { .. } => layout.clone(),
        LayoutNode::Split {
            axis,
            first,
            second,
            ..
        } => {
            let a = span(first, *axis);
            let b = span(second, *axis);
            LayoutNode::Split {
                axis: *axis,
                ratio: a as f32 / (a + b) as f32,
                first: Box::new(equalize(first)),
                second: Box::new(equalize(second)),
            }
        }
    }
}

fn equalize_shape(shape: &Shape) -> Shape {
    fn span(shape: &Shape, along: SplitAxis) -> usize {
        match shape {
            Shape::Split {
                axis,
                first,
                second,
                ..
            } if *axis == along => span(first, along) + span(second, along),
            _ => 1,
        }
    }
    match shape {
        Shape::Slot => Shape::Slot,
        Shape::Split {
            axis,
            first,
            second,
            ..
        } => {
            let a = span(first, *axis);
            let b = span(second, *axis);
            Shape::Split {
                axis: *axis,
                ratio: a as f32 / (a + b) as f32,
                first: Box::new(equalize_shape(first)),
                second: Box::new(equalize_shape(second)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: PaneId::new(id),
            surface_id: SurfaceId::new(format!("{id}-surface")),
        }
    }

    fn split(axis: SplitAxis, ratio: f32, first: LayoutNode, second: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Panes as `(id, [x, y, width, height])` rounded to hundredths.
    fn placement(layout: &LayoutNode) -> Vec<(String, [i32; 4])> {
        pane_fractions(layout)
            .into_iter()
            .map(|(id, rect)| {
                let r = |v: f32| (v * 100.0).round() as i32;
                (
                    id.to_string(),
                    [r(rect.x), r(rect.y), r(rect.width), r(rect.height)],
                )
            })
            .collect()
    }

    fn sorted_leaves(layout: &LayoutNode) -> Vec<Leaf> {
        let mut all = leaves(layout);
        all.sort();
        all
    }

    fn three() -> LayoutNode {
        split(
            SplitAxis::Horizontal,
            0.5,
            pane("a"),
            split(SplitAxis::Vertical, 0.5, pane("b"), pane("c")),
        )
    }

    #[test]
    fn every_builtin_keeps_exactly_the_same_panes() {
        for count in 1..=9 {
            let mut tree = pane("p0");
            for index in 1..count {
                tree = split(SplitAxis::Vertical, 0.5, tree, pane(&format!("p{index}")));
            }
            for builtin in Builtin::ALL {
                for (mirror, flip) in [(false, false), (true, false), (false, true), (true, true)] {
                    let arranged = arrange(
                        &tree,
                        Preset::Builtin(builtin),
                        Some(&PaneId::new("p1")),
                        Transform { mirror, flip },
                    );
                    assert_eq!(sorted_leaves(&arranged), sorted_leaves(&tree));
                }
            }
        }
    }

    #[test]
    fn grid_fills_rows_and_leaves_the_short_row_last() {
        let five = split(
            SplitAxis::Horizontal,
            0.5,
            three(),
            split(SplitAxis::Horizontal, 0.5, pane("d"), pane("e")),
        );
        let grid = arrange(
            &five,
            Preset::Builtin(Builtin::Grid),
            None,
            Transform::default(),
        );
        assert_eq!(
            placement(&grid),
            [
                ("a".into(), [0, 0, 33, 50]),
                ("b".into(), [33, 0, 33, 50]),
                ("c".into(), [67, 0, 33, 50]),
                ("d".into(), [0, 50, 50, 50]),
                ("e".into(), [50, 50, 50, 50]),
            ]
        );
        let flipped = arrange(
            &five,
            Preset::Builtin(Builtin::Grid),
            None,
            Transform {
                mirror: false,
                flip: true,
            },
        );
        assert_eq!(placement(&flipped)[0], ("d".into(), [0, 0, 50, 50]));
    }

    #[test]
    fn main_stack_puts_main_first_and_mirror_moves_it_right() {
        let main = PaneId::new("c");
        let side = arrange(
            &three(),
            Preset::Builtin(Builtin::MainSide),
            Some(&main),
            Transform::default(),
        );
        assert_eq!(
            placement(&side),
            [
                ("c".into(), [0, 0, 60, 100]),
                ("a".into(), [60, 0, 40, 50]),
                ("b".into(), [60, 50, 40, 50]),
            ]
        );
        let mirrored = apply_transform(
            side,
            Transform {
                mirror: true,
                flip: false,
            },
        );
        assert_eq!(
            placement(&mirrored),
            [
                ("a".into(), [0, 0, 40, 50]),
                ("b".into(), [0, 50, 40, 50]),
                ("c".into(), [40, 0, 60, 100]),
            ]
        );
    }

    #[test]
    fn named_presets_drop_trailing_slots_or_stack_extra_panes_in_the_last() {
        // Main left at 70%, then two rows on the right.
        let shape = Shape::of(&split(
            SplitAxis::Horizontal,
            0.7,
            pane("x"),
            split(SplitAxis::Vertical, 0.5, pane("y"), pane("z")),
        ));
        let two = split(SplitAxis::Vertical, 0.5, pane("a"), pane("b"));
        assert_eq!(
            placement(&arrange(
                &two,
                Preset::Named(&shape),
                None,
                Transform::default()
            )),
            [
                ("a".into(), [0, 0, 70, 100]),
                ("b".into(), [70, 0, 30, 100])
            ]
        );
        let four = split(SplitAxis::Vertical, 0.5, three(), pane("d"));
        assert_eq!(
            placement(&arrange(
                &four,
                Preset::Named(&shape),
                None,
                Transform::default()
            )),
            [
                ("a".into(), [0, 0, 70, 100]),
                ("b".into(), [70, 0, 30, 50]),
                ("c".into(), [70, 50, 15, 50]),
                ("d".into(), [85, 50, 15, 50]),
            ]
        );
    }

    #[test]
    fn swap_exchanges_positions_and_rejects_unknown_panes() {
        let swapped = swap(&three(), &PaneId::new("a"), &PaneId::new("c")).unwrap();
        assert_eq!(
            placement(&swapped),
            [
                ("c".into(), [0, 0, 50, 100]),
                ("b".into(), [50, 0, 50, 50]),
                ("a".into(), [50, 50, 50, 50]),
            ]
        );
        assert_eq!(sorted_leaves(&swapped), sorted_leaves(&three()));
        assert!(swap(&three(), &PaneId::new("a"), &PaneId::new("missing")).is_none());
        assert!(swap(&three(), &PaneId::new("a"), &PaneId::new("a")).is_none());
    }

    #[test]
    fn restore_keeps_old_positions_drops_removed_and_appends_new_panes() {
        let previous = three();
        // Since then: b was removed and d was added.
        let current = split(
            SplitAxis::Vertical,
            0.5,
            pane("d"),
            split(SplitAxis::Horizontal, 0.5, pane("c"), pane("a")),
        );
        let restored = restore(&previous, &current).unwrap();
        assert_eq!(
            placement(&restored),
            [
                ("a".into(), [0, 0, 50, 100]),
                ("c".into(), [50, 0, 50, 50]),
                ("d".into(), [50, 50, 50, 50]),
            ]
        );
        assert_eq!(sorted_leaves(&restored), sorted_leaves(&current));
        assert!(restore(&pane("gone"), &current).is_none());
    }

    #[test]
    fn combined_tabs_sit_side_by_side_and_arrange_in_tab_order() {
        let second = split(SplitAxis::Vertical, 0.3, pane("d"), pane("e"));
        let combined = combine(&[&three(), &second, &pane("f")]).unwrap();
        assert_eq!(
            placement(&combined),
            [
                ("a".into(), [0, 0, 17, 100]),
                ("b".into(), [17, 0, 17, 50]),
                ("c".into(), [17, 50, 17, 50]),
                ("d".into(), [33, 0, 33, 30]),
                ("e".into(), [33, 30, 33, 70]),
                ("f".into(), [67, 0, 33, 100]),
            ]
        );
        let rows = arrange(
            &combined,
            Preset::Builtin(Builtin::Rows),
            None,
            Transform::default(),
        );
        assert_eq!(
            leaves(&rows)
                .into_iter()
                .map(|(id, _)| id.to_string())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d", "e", "f"]
        );
        assert!(combine(&[]).is_none());
        assert_eq!(combine(&[&second]), Some(second.clone()));
    }

    #[test]
    fn equalize_gives_equal_shares_along_each_axis() {
        let skewed = split(
            SplitAxis::Horizontal,
            0.9,
            pane("a"),
            split(
                SplitAxis::Horizontal,
                0.2,
                pane("b"),
                split(SplitAxis::Vertical, 0.8, pane("c"), pane("d")),
            ),
        );
        let even = arrange(
            &skewed,
            Preset::Builtin(Builtin::Equalize),
            None,
            Transform::default(),
        );
        assert_eq!(
            placement(&even),
            [
                ("a".into(), [0, 0, 33, 100]),
                ("b".into(), [33, 0, 33, 100]),
                ("c".into(), [67, 0, 33, 50]),
                ("d".into(), [67, 50, 33, 50]),
            ]
        );
    }

    #[test]
    fn preset_shapes_and_names_are_bounded() {
        assert!(Shape::Slot.validate().is_err());
        assert!(Shape::of(&three()).validate().is_ok());
        assert!(
            builtin_shape(Builtin::Columns, MAX_PRESET_SLOTS + 1)
                .validate()
                .is_err()
        );
        let bad_ratio = Shape::Split {
            axis: SplitAxis::Horizontal,
            ratio: 1.0,
            first: Box::new(Shape::Slot),
            second: Box::new(Shape::Slot),
        };
        assert!(bad_ratio.validate().is_err());
        assert!(valid_preset_name("dev"));
        assert!(!valid_preset_name(" dev"));
        assert!(!valid_preset_name(""));
        assert!(!valid_preset_name("a\u{7}"));
        assert!(!valid_preset_name(&"x".repeat(MAX_PRESET_NAME_CHARS + 1)));
    }
}
