//! Device-pixel-aligned split geometry. Rendering never rewrites the saved tree.
use compi_protocol::{LayoutNode, PaneId, SplitAxis, SurfaceId};

pub const MIN_COLUMNS: usize = 20;
pub const MIN_ROWS: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn right(self) -> f32 {
        self.x + self.width
    }
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }
    pub fn size(self) -> Size {
        Size {
            width: self.width,
            height: self.height,
        }
    }
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LayoutMetrics {
    pub cell_width: f32,
    pub line_height: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub pane_chrome_height: f32,
    pub divider_thickness: f32,
    pub scale_factor: f32,
}

impl LayoutMetrics {
    fn normalized(self) -> Self {
        Self {
            cell_width: positive(self.cell_width, 1.0),
            line_height: positive(self.line_height, 1.0),
            padding_x: nonnegative(self.padding_x),
            padding_y: nonnegative(self.padding_y),
            pane_chrome_height: nonnegative(self.pane_chrome_height),
            divider_thickness: nonnegative(self.divider_thickness),
            scale_factor: positive(self.scale_factor, 1.0),
        }
    }

    fn ceil(self, value: f32) -> f32 {
        (value * self.scale_factor).ceil() / self.scale_factor
    }
    fn round(self, value: f32) -> f32 {
        (value * self.scale_factor).round() / self.scale_factor
    }
    pub fn divider_size(self) -> f32 {
        let metrics = self.normalized();
        metrics.ceil(metrics.divider_thickness)
    }
    pub fn leaf_minimum(self) -> Size {
        let m = self.normalized();
        Size {
            width: m.ceil(MIN_COLUMNS as f32 * m.cell_width) + 2.0 * m.ceil(m.padding_x),
            height: m.ceil(MIN_ROWS as f32 * m.line_height)
                + 2.0 * m.ceil(m.padding_y)
                + m.ceil(m.pane_chrome_height),
        }
    }
}

fn positive(value: f32, fallback: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}
fn nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug)]
pub struct PaneLayout {
    pub pane_id: PaneId,
    pub surface_id: SurfaceId,
    pub rect: Rect,
    pub canvas: Rect,
    pub parent_divider: Option<usize>,
}

impl PaneLayout {
    /// Dimensions use the whole allocation, never its clipped viewport intersection.
    pub fn grid_size(&self, metrics: LayoutMetrics) -> (i16, i16) {
        let m = metrics.normalized();
        (
            (self.canvas.width / m.cell_width)
                .floor()
                .clamp(MIN_COLUMNS as f32, i16::MAX as f32) as i16,
            (self.canvas.height / m.line_height)
                .floor()
                .clamp(MIN_ROWS as f32, i16::MAX as f32) as i16,
        )
    }
}

#[derive(Clone, Debug)]
pub struct DividerLayout {
    /// Root-relative split address: false selects first, true selects second.
    /// Only valid at the workspace revision from which this layout was computed.
    pub path: Vec<bool>,
    pub first_leaf: PaneId,
    /// A direct leaf child, if present. Use `path`, not a descendant pane, for mutations.
    pub immediate_parent_pane: Option<PaneId>,
    pub axis: SplitAxis,
    pub rect: Rect,
    pub bounds: Rect,
    pub min_ratio: f32,
    pub max_ratio: f32,
    pub effective_ratio: f32,
    pub saved_ratio: f32,
}

impl DividerLayout {
    pub fn disabled_reason(&self) -> Option<&'static str> {
        (self.max_ratio - self.min_ratio <= f32::EPSILON)
            .then_some("Both sides are at their minimum size")
    }
    pub fn clamp_ratio(&self, ratio: f32) -> f32 {
        if ratio.is_finite() {
            ratio.clamp(self.min_ratio, self.max_ratio)
        } else {
            self.effective_ratio
        }
    }
    pub fn ratio_at(&self, point: Point) -> f32 {
        let (position, length) = match self.axis {
            SplitAxis::Horizontal => (
                point.x - self.bounds.x - self.rect.width / 2.0,
                self.bounds.width - self.rect.width,
            ),
            SplitAxis::Vertical => (
                point.y - self.bounds.y - self.rect.height / 2.0,
                self.bounds.height - self.rect.height,
            ),
        };
        self.clamp_ratio(position / length)
    }
}

/// A drag cannot accidentally commit against a different tree after an async update.
#[derive(Clone, Debug)]
pub struct DividerDrag {
    pub revision: u64,
    pub divider: DividerLayout,
}

impl DividerDrag {
    pub fn ratio_at(&self, revision: u64, point: Point) -> Result<f32, &'static str> {
        if revision != self.revision {
            return Err("Workspace changed; divider drag canceled");
        }
        if let Some(reason) = self.divider.disabled_reason() {
            return Err(reason);
        }
        Ok(self.divider.ratio_at(point))
    }
}

#[derive(Clone, Debug)]
pub struct WorkspaceLayout {
    pub viewport: Size,
    pub canvas: Size,
    pub minimum: Size,
    pub panes: Vec<PaneLayout>,
    pub dividers: Vec<DividerLayout>,
    metrics: LayoutMetrics,
}

impl WorkspaceLayout {
    pub fn has_overflow(&self) -> bool {
        self.minimum.width > self.viewport.width || self.minimum.height > self.viewport.height
    }
    pub fn pane(&self, id: &PaneId) -> Option<&PaneLayout> {
        self.panes.iter().find(|pane| &pane.pane_id == id)
    }
    pub fn divider_for_pane(&self, id: &PaneId) -> Option<&DividerLayout> {
        self.pane(id)?
            .parent_divider
            .and_then(|index| self.dividers.get(index))
    }
    pub fn split_feasibility(&self, pane: &PaneId, axis: SplitAxis) -> Result<Rect, &'static str> {
        let pane = self.pane(pane).ok_or("The focused pane no longer exists")?;
        if self.has_overflow() {
            return Err("Enlarge the workspace before adding a split");
        }
        let leaf = self.metrics.leaf_minimum();
        let required = combine(leaf, leaf, axis, self.metrics.divider_size());
        if pane.rect.width < required.width || pane.rect.height < required.height {
            return Err("The focused pane is too small for two 20-column by 4-row terminals");
        }
        Ok(pane.rect)
    }
    pub fn focus_neighbor(&self, pane: &PaneId, direction: Direction) -> Option<&PaneId> {
        let source = self.pane(pane)?.rect;
        let mut best: Option<(&PaneId, _)> = None;
        for candidate in &self.panes {
            if &candidate.pane_id == pane {
                continue;
            }
            let target = candidate.rect;
            let (primary, gap, overlap, perpendicular) = match direction {
                Direction::Left => (
                    source.x + source.width / 2.0 - target.x - target.width / 2.0,
                    (source.x - target.right()).max(0.0),
                    source.y < target.bottom() && target.y < source.bottom(),
                    (source.y + source.height / 2.0 - target.y - target.height / 2.0).abs(),
                ),
                Direction::Right => (
                    target.x + target.width / 2.0 - source.x - source.width / 2.0,
                    (target.x - source.right()).max(0.0),
                    source.y < target.bottom() && target.y < source.bottom(),
                    (source.y + source.height / 2.0 - target.y - target.height / 2.0).abs(),
                ),
                Direction::Up => (
                    source.y + source.height / 2.0 - target.y - target.height / 2.0,
                    (source.y - target.bottom()).max(0.0),
                    source.x < target.right() && target.x < source.right(),
                    (source.x + source.width / 2.0 - target.x - target.width / 2.0).abs(),
                ),
                Direction::Down => (
                    target.y + target.height / 2.0 - source.y - source.height / 2.0,
                    (target.y - source.bottom()).max(0.0),
                    source.x < target.right() && target.x < source.right(),
                    (source.x + source.width / 2.0 - target.x - target.width / 2.0).abs(),
                ),
            };
            if primary <= 0.0 {
                continue;
            }
            let score = (!overlap, gap, perpendicular, primary);
            if best.as_ref().is_none_or(|(_, current)| score < *current) {
                best = Some((&candidate.pane_id, score));
            }
        }
        best.map(|(id, _)| id)
    }
    pub fn clamp_scroll(&self, scroll: Point) -> Point {
        Point {
            x: nonnegative(scroll.x).min((self.canvas.width - self.viewport.width).max(0.0)),
            y: nonnegative(scroll.y).min((self.canvas.height - self.viewport.height).max(0.0)),
        }
    }
    /// Cursor is in workspace coordinates, not terminal cell or viewport coordinates.
    pub fn reveal_pane(&self, pane: &PaneId, cursor: Option<Rect>, scroll: Point) -> Point {
        let scroll = self.clamp_scroll(scroll);
        let Some(pane) = self.pane(pane) else {
            return scroll;
        };
        let cursor = cursor.unwrap_or(Rect {
            x: pane.canvas.x,
            y: pane.canvas.y,
            width: self.metrics.cell_width,
            height: self.metrics.line_height,
        });
        let horizontal = if pane.rect.width > self.viewport.width {
            cursor
        } else {
            pane.rect
        };
        let vertical = if pane.rect.height > self.viewport.height {
            cursor
        } else {
            pane.rect
        };
        self.clamp_scroll(Point {
            x: reveal_axis(
                scroll.x,
                self.viewport.width,
                horizontal.x,
                horizontal.width,
            ),
            y: reveal_axis(scroll.y, self.viewport.height, vertical.y, vertical.height),
        })
    }
}

fn reveal_axis(scroll: f32, viewport: f32, start: f32, size: f32) -> f32 {
    if start < scroll {
        start
    } else if start + size > scroll + viewport {
        (start + size.min(viewport) - viewport).max(0.0)
    } else {
        scroll
    }
}

pub fn first_leaf(node: &LayoutNode) -> &PaneId {
    match node {
        LayoutNode::Pane { pane_id, .. } => pane_id,
        LayoutNode::Split { first, .. } => first_leaf(first),
    }
}

pub fn node_at_path<'a>(mut node: &'a LayoutNode, path: &[bool]) -> Option<&'a LayoutNode> {
    for second_child in path {
        let LayoutNode::Split { first, second, .. } = node else {
            return None;
        };
        node = if *second_child { second } else { first };
    }
    Some(node)
}

fn combine(first: Size, second: Size, axis: SplitAxis, divider: f32) -> Size {
    match axis {
        SplitAxis::Horizontal => Size {
            width: first.width + divider + second.width,
            height: first.height.max(second.height),
        },
        SplitAxis::Vertical => Size {
            width: first.width.max(second.width),
            height: first.height + divider + second.height,
        },
    }
}

#[derive(Clone, Copy)]
struct Measured<'a> {
    minimum: Size,
    first: usize,
    second: usize,
    first_leaf: &'a PaneId,
}

fn measure<'a>(
    node: &'a LayoutNode,
    leaf: Size,
    divider: f32,
    measured: &mut Vec<Measured<'a>>,
) -> usize {
    let item = match node {
        LayoutNode::Pane { pane_id, .. } => Measured {
            minimum: leaf,
            first: 0,
            second: 0,
            first_leaf: pane_id,
        },
        LayoutNode::Split {
            axis,
            first,
            second,
            ..
        } => {
            let first = measure(first, leaf, divider, measured);
            let second = measure(second, leaf, divider, measured);
            Measured {
                minimum: combine(
                    measured[first].minimum,
                    measured[second].minimum,
                    *axis,
                    divider,
                ),
                first,
                second,
                first_leaf: measured[first].first_leaf,
            }
        }
    };
    let index = measured.len();
    measured.push(item);
    index
}

/// Full-tree layout with cached recursive minima. Reserve scrollbar gutters in the viewport.
pub fn compute_layout(
    tree: &LayoutNode,
    viewport: Size,
    metrics: LayoutMetrics,
) -> WorkspaceLayout {
    compute_layout_with_preview(tree, viewport, metrics, None)
}

/// Preview overrides one addressed split without cloning or mutating the server tree.
pub fn compute_layout_with_preview(
    tree: &LayoutNode,
    viewport: Size,
    metrics: LayoutMetrics,
    preview: Option<(&[bool], f32)>,
) -> WorkspaceLayout {
    let metrics = metrics.normalized();
    let viewport = Size {
        width: nonnegative(viewport.width),
        height: nonnegative(viewport.height),
    };
    let mut measured = Vec::new();
    let root = measure(
        tree,
        metrics.leaf_minimum(),
        metrics.divider_size(),
        &mut measured,
    );
    let minimum = measured[root].minimum;
    let canvas = Size {
        width: metrics.ceil(viewport.width.max(minimum.width)),
        height: metrics.ceil(viewport.height.max(minimum.height)),
    };
    let mut output = WorkspaceLayout {
        viewport,
        canvas,
        minimum,
        panes: Vec::with_capacity(measured.len().div_ceil(2)),
        dividers: Vec::with_capacity(measured.len() / 2),
        metrics,
    };
    Allocation {
        measured: &measured,
        preview,
        output: &mut output,
        path: Vec::new(),
    }
    .allocate(
        tree,
        root,
        Rect {
            width: canvas.width,
            height: canvas.height,
            ..Rect::default()
        },
        None,
    );
    output
}

struct Allocation<'a, 'tree> {
    measured: &'a [Measured<'tree>],
    preview: Option<(&'a [bool], f32)>,
    output: &'a mut WorkspaceLayout,
    path: Vec<bool>,
}

impl Allocation<'_, '_> {
    fn allocate(&mut self, node: &LayoutNode, index: usize, rect: Rect, parent: Option<usize>) {
        let m = self.output.metrics;
        match node {
            LayoutNode::Pane {
                pane_id,
                surface_id,
            } => {
                let px = m.ceil(m.padding_x);
                let py = m.ceil(m.padding_y);
                let chrome = m.ceil(m.pane_chrome_height);
                self.output.panes.push(PaneLayout {
                    pane_id: pane_id.clone(),
                    surface_id: surface_id.clone(),
                    rect,
                    canvas: Rect {
                        x: rect.x + px,
                        y: rect.y + py + chrome,
                        width: rect.width - 2.0 * px,
                        height: rect.height - 2.0 * py - chrome,
                    },
                    parent_divider: parent,
                });
            }
            LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let first_index = self.measured[index].first;
                let first_min = self.measured[first_index].minimum;
                let second_index = self.measured[index].second;
                let second_min = self.measured[second_index].minimum;
                let divider = m.divider_size();
                let (length, low, second_length) = match axis {
                    SplitAxis::Horizontal => {
                        (rect.width - divider, first_min.width, second_min.width)
                    }
                    SplitAxis::Vertical => {
                        (rect.height - divider, first_min.height, second_min.height)
                    }
                };
                // Child minima are already pixel aligned; re-ceiling accumulated floats
                // can otherwise introduce an extra pixel at fractional display scales.
                let high = (length - second_length).max(low);
                let requested = self
                    .preview
                    .filter(|(address, _)| *address == self.path.as_slice())
                    .map_or(*ratio, |(_, ratio)| ratio);
                let requested = if requested.is_finite() {
                    requested
                } else {
                    0.5
                };
                let first_length = m.round(length * requested).clamp(low, high);
                let (first_rect, divider_rect, second_rect) = match axis {
                    SplitAxis::Horizontal => (
                        Rect {
                            width: first_length,
                            ..rect
                        },
                        Rect {
                            x: rect.x + first_length,
                            width: divider,
                            ..rect
                        },
                        Rect {
                            x: rect.x + first_length + divider,
                            width: length - first_length,
                            ..rect
                        },
                    ),
                    SplitAxis::Vertical => (
                        Rect {
                            height: first_length,
                            ..rect
                        },
                        Rect {
                            y: rect.y + first_length,
                            height: divider,
                            ..rect
                        },
                        Rect {
                            y: rect.y + first_length + divider,
                            height: length - first_length,
                            ..rect
                        },
                    ),
                };
                let parent = self.output.dividers.len();
                let immediate_parent_pane = match (first.as_ref(), second.as_ref()) {
                    (LayoutNode::Pane { pane_id, .. }, _)
                    | (_, LayoutNode::Pane { pane_id, .. }) => Some(pane_id.clone()),
                    _ => None,
                };
                self.output.dividers.push(DividerLayout {
                    path: self.path.clone(),
                    first_leaf: self.measured[index].first_leaf.clone(),
                    immediate_parent_pane,
                    axis: *axis,
                    rect: divider_rect,
                    bounds: rect,
                    min_ratio: low / length,
                    max_ratio: high / length,
                    effective_ratio: first_length / length,
                    saved_ratio: *ratio,
                });
                self.path.push(false);
                self.allocate(first, first_index, first_rect, Some(parent));
                *self.path.last_mut().expect("child path") = true;
                self.allocate(second, second_index, second_rect, Some(parent));
                self.path.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metrics() -> LayoutMetrics {
        LayoutMetrics {
            cell_width: 8.0,
            line_height: 16.0,
            padding_x: 4.0,
            padding_y: 2.0,
            pane_chrome_height: 20.0,
            divider_thickness: 4.0,
            scale_factor: 1.0,
        }
    }
    fn leaf(id: &str) -> LayoutNode {
        LayoutNode::Pane {
            pane_id: id.into(),
            surface_id: id.into(),
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

    #[test]
    fn nested_minima_overflow_and_saved_ratios_survive_contraction() {
        let tree = split(
            SplitAxis::Horizontal,
            0.8,
            leaf("a"),
            split(SplitAxis::Vertical, 0.25, leaf("b"), leaf("c")),
        );
        let original = tree.clone();
        let small = compute_layout(
            &tree,
            Size {
                width: 200.0,
                height: 100.0,
            },
            metrics(),
        );
        assert_eq!(
            small.minimum,
            Size {
                width: 340.0,
                height: 180.0
            }
        );
        assert_eq!(small.canvas, small.minimum);
        assert!(small.panes.iter().all(|pane| {
            let (cols, rows) = pane.grid_size(metrics());
            cols >= 20 && rows >= 4
        }));
        assert!(
            small
                .split_feasibility(&"a".into(), SplitAxis::Vertical)
                .is_err()
        );
        assert!(small.dividers[0].disabled_reason().is_some());
        let expanded = compute_layout(
            &tree,
            Size {
                width: 1000.0,
                height: 500.0,
            },
            metrics(),
        );
        assert!(!expanded.has_overflow());
        assert!((expanded.dividers[0].effective_ratio - 0.8).abs() < 0.001);
        assert_eq!(tree, original);
    }

    #[test]
    fn exact_split_boundary_and_fractional_scale_preserve_canvas_minima() {
        let m = LayoutMetrics {
            scale_factor: 1.25,
            cell_width: 7.33,
            line_height: 15.13,
            ..metrics()
        };
        let minimum = m.leaf_minimum();
        let required = combine(minimum, minimum, SplitAxis::Horizontal, m.divider_size());
        let layout = compute_layout(&leaf("a"), required, m);
        assert!(
            layout
                .split_feasibility(&"a".into(), SplitAxis::Horizontal)
                .is_ok()
        );
        let smaller = compute_layout(
            &leaf("a"),
            Size {
                width: required.width - 1.0,
                ..required
            },
            m,
        );
        assert!(
            smaller
                .split_feasibility(&"a".into(), SplitAxis::Horizontal)
                .is_err()
        );
        let divided = compute_layout(
            &split(SplitAxis::Horizontal, 0.99, leaf("a"), leaf("b")),
            required,
            m,
        );
        assert!(
            divided
                .panes
                .iter()
                .all(|pane| pane.canvas.width >= 20.0 * m.cell_width
                    && pane.canvas.height >= 4.0 * m.line_height)
        );
    }

    #[test]
    fn focus_reveals_offscreen_pane_and_oversized_cursor_without_resizing() {
        let tree = split(SplitAxis::Horizontal, 0.5, leaf("a"), leaf("b"));
        let layout = compute_layout(
            &tree,
            Size {
                width: 190.0,
                height: 50.0,
            },
            metrics(),
        );
        assert_eq!(
            layout.focus_neighbor(&"a".into(), Direction::Right),
            Some(&PaneId::from("b"))
        );
        assert_eq!(layout.focus_neighbor(&"a".into(), Direction::Left), None);
        let scroll = layout.reveal_pane(
            &"b".into(),
            Some(Rect {
                x: 200.0,
                y: 72.0,
                width: 8.0,
                height: 16.0,
            }),
            Point::default(),
        );
        assert_eq!(scroll, Point { x: 150.0, y: 38.0 });
        assert_eq!(layout.reveal_pane(&"missing".into(), None, scroll), scroll);
        assert_eq!(
            layout.pane(&"b".into()).unwrap().grid_size(metrics()),
            (20, 4)
        );
    }

    #[test]
    fn nested_addresses_and_stale_drag_never_target_another_split() {
        let tree = split(
            SplitAxis::Horizontal,
            0.5,
            split(SplitAxis::Vertical, 0.5, leaf("a"), leaf("b")),
            split(SplitAxis::Vertical, 0.5, leaf("c"), leaf("d")),
        );
        let layout = compute_layout(
            &tree,
            Size {
                width: 1000.0,
                height: 500.0,
            },
            metrics(),
        );
        assert!(layout.dividers[0].immediate_parent_pane.is_none());
        assert_eq!(layout.dividers[2].path, vec![true]);
        assert_eq!(
            first_leaf(node_at_path(&tree, &layout.dividers[2].path).unwrap()),
            &PaneId::from("c")
        );
        let drag = DividerDrag {
            revision: 7,
            divider: layout.dividers[0].clone(),
        };
        assert!(drag.ratio_at(8, Point { x: 900.0, y: 0.0 }).is_err());
        let preview =
            compute_layout_with_preview(&tree, layout.viewport, metrics(), Some((&[], 0.7)));
        assert!((preview.dividers[0].effective_ratio - 0.7).abs() < 0.001);
        assert_eq!(preview.dividers[0].saved_ratio, 0.5);
    }
}
