//! A squarified treemap (Bruls, Huizing and van Wijk): rectangles whose areas are in
//! proportion to their values, laid out row by row along the shorter side of what is
//! left, each row grown while that makes its worst aspect ratio better.

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One rectangle per value inside `bounds`, in the order of `values`, largest laid out
/// first. Values that are not positive get an empty rectangle.
pub fn squarify(values: &[f64], bounds: Rect) -> Vec<Rect> {
    let mut rects = vec![
        Rect {
            x: bounds.x,
            y: bounds.y,
            width: 0.0,
            height: 0.0,
        };
        values.len()
    ];
    let mut order: Vec<usize> = (0..values.len())
        .filter(|&index| values[index] > 0.0 && values[index].is_finite())
        .collect();
    let total: f64 = order.iter().map(|&index| values[index]).sum();
    let area = f64::from(bounds.width.max(0.0)) * f64::from(bounds.height.max(0.0));
    if order.is_empty() || area <= 0.0 {
        return rects;
    }
    // Largest first; equal values keep their order, so the layout is deterministic.
    order.sort_by(|&a, &b| values[b].total_cmp(&values[a]).then(a.cmp(&b)));
    let scale = area / total;
    let areas: Vec<(usize, f64)> = order
        .into_iter()
        .map(|index| (index, values[index] * scale))
        .collect();
    let mut free = (
        f64::from(bounds.x),
        f64::from(bounds.y),
        f64::from(bounds.width),
        f64::from(bounds.height),
    );
    let mut start = 0;
    while start < areas.len() {
        let side = free.2.min(free.3);
        // Grow the row while its worst aspect ratio does not get worse.
        let mut end = start + 1;
        while end < areas.len()
            && worst(&areas[start..=end], side) <= worst(&areas[start..end], side)
        {
            end += 1;
        }
        let row = &areas[start..end];
        let last = end == areas.len();
        free = lay_row(row, free, last, &mut rects);
        start = end;
    }
    rects
}

/// The worst aspect ratio of `row` laid along a side `side` long.
fn worst(row: &[(usize, f64)], side: f64) -> f64 {
    let sum: f64 = row.iter().map(|(_, area)| area).sum();
    let (smallest, largest) = row
        .iter()
        .fold((f64::MAX, 0.0_f64), |(low, high), (_, area)| {
            (low.min(*area), high.max(*area))
        });
    let (side2, sum2) = (side * side, sum * sum);
    (side2 * largest / sum2).max(sum2 / (side2 * smallest))
}

/// Lays `row` along the shorter side of `free` (a column on the left when it is wider
/// than tall, else a row at the top) and returns what is left. The `last` row fills
/// what is left exactly; within a row the last rectangle takes the rest of the side.
fn lay_row(
    row: &[(usize, f64)],
    (x, y, width, height): (f64, f64, f64, f64),
    last: bool,
    rects: &mut [Rect],
) -> (f64, f64, f64, f64) {
    let sum: f64 = row.iter().map(|(_, area)| area).sum();
    let column = width >= height;
    let side = if column { height } else { width };
    let thickness = if last {
        if column { width } else { height }
    } else {
        sum / side
    };
    let mut offset = 0.0;
    for (position, &(index, area)) in row.iter().enumerate() {
        let length = if position + 1 == row.len() {
            side - offset
        } else {
            area / thickness
        };
        rects[index] = if column {
            rect(x, y + offset, thickness, length)
        } else {
            rect(x + offset, y, length, thickness)
        };
        offset += length;
    }
    if column {
        (x + thickness, y, (width - thickness).max(0.0), height)
    } else {
        (x, y + thickness, width, (height - thickness).max(0.0))
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x: x as f32,
        y: y as f32,
        width: width.max(0.0) as f32,
        height: height.max(0.0) as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Rect = Rect {
        x: 10.0,
        y: 20.0,
        width: 600.0,
        height: 400.0,
    };

    fn overlap(a: &Rect, b: &Rect) -> f32 {
        let width = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
        let height = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
        width.max(0.0) * height.max(0.0)
    }

    fn check(values: &[f64], bounds: Rect) {
        let rects = squarify(values, bounds);
        assert_eq!(rects.len(), values.len());
        let total: f64 = values.iter().filter(|value| **value > 0.0).sum();
        let area = bounds.width * bounds.height;
        for (value, rect) in values.iter().zip(&rects) {
            let expected = if *value > 0.0 {
                (*value / total) as f32 * area
            } else {
                0.0
            };
            let actual = rect.width * rect.height;
            assert!(
                (actual - expected).abs() <= area * 1e-4 + 0.5,
                "{value}: {actual} vs {expected}"
            );
            assert!(rect.x >= bounds.x - 1e-3 && rect.y >= bounds.y - 1e-3);
            assert!(rect.x + rect.width <= bounds.x + bounds.width + 1e-3);
            assert!(rect.y + rect.height <= bounds.y + bounds.height + 1e-3);
        }
        for (first, a) in rects.iter().enumerate() {
            for b in &rects[first + 1..] {
                assert!(overlap(a, b) < 1e-2, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn areas_follow_values_inside_the_bounds_without_overlap() {
        check(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], BOUNDS);
        check(&[500.0, 400.0, 75.0], BOUNDS);
        check(&[1.0], BOUNDS);
        check(&[3.0, 0.0, 1.0, -2.0], BOUNDS);
        check(
            &[1.0, 1000.0, 2.0, 3.0, 50.0, 7.0, 7.0, 0.5],
            Rect {
                x: 0.0,
                y: 0.0,
                width: 180.0,
                height: 320.0,
            },
        );
        for count in 1..20 {
            let values: Vec<f64> = (1..=count).map(|value| f64::from(value * value)).collect();
            check(&values, BOUNDS);
        }
    }

    #[test]
    fn one_value_fills_the_bounds_and_nothing_draws_nothing() {
        assert_eq!(squarify(&[5.0], BOUNDS), [BOUNDS]);
        assert!(squarify(&[], BOUNDS).is_empty());
        let empty = squarify(&[0.0, 0.0], BOUNDS);
        assert!(
            empty
                .iter()
                .all(|rect| rect.width == 0.0 && rect.height == 0.0)
        );
        let flat = squarify(
            &[1.0, 2.0],
            Rect {
                width: 0.0,
                ..BOUNDS
            },
        );
        assert!(flat.iter().all(|rect| rect.width * rect.height == 0.0));
    }

    #[test]
    fn layouts_are_deterministic_and_squarish() {
        let values = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        assert_eq!(squarify(&values, BOUNDS), squarify(&values, BOUNDS));
        // The classic example: no tile is more than about three times longer than wide.
        for rect in squarify(&values, BOUNDS) {
            let ratio = (rect.width / rect.height).max(rect.height / rect.width);
            assert!(ratio < 3.0, "{rect:?}");
        }
    }
}
