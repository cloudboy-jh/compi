use super::*;

// The rounded iris and pixel-derived pupil selected in the design checkpoint.
const PUPIL: &[(f32, f32)] = &[
    (368., 225.),
    (441., 225.),
    (441., 262.),
    (405., 262.),
    (405., 296.),
    (474., 296.),
    (474., 330.),
    (510., 330.),
    (510., 403.),
    (546., 403.),
    (546., 476.),
    (565., 476.),
    (565., 514.),
    (583., 514.),
    (583., 587.),
    (600., 587.),
    (600., 763.),
    (585., 763.),
    (585., 727.),
    (566., 727.),
    (566., 693.),
    (548., 693.),
    (548., 659.),
    (529., 659.),
    (529., 624.),
    (511., 624.),
    (511., 587.),
    (477., 587.),
    (477., 551.),
    (458., 551.),
    (458., 513.),
    (441., 513.),
    (441., 477.),
    (405., 477.),
    (405., 441.),
    (369., 441.),
    (369., 405.),
    (333., 405.),
    (333., 371.),
    (297., 371.),
    (297., 336.),
    (260., 336.),
    (260., 298.),
    (333., 298.),
    (333., 260.),
    (368., 260.),
];

pub(super) fn paint(bounds: Bounds<Pixels>, tint: Hsla, window: &mut Window) {
    let scale = f32::from(bounds.size.width.min(bounds.size.height)) / 1024.0;
    let at = |x: f32, y: f32| point(bounds.left() + px(x * scale), bounds.top() + px(y * scale));
    let mut path = PathBuilder::fill().with_style(gpui::PathStyle::Fill(
        gpui::FillOptions::default().with_fill_rule(gpui::FillRule::EvenOdd),
    ));
    path.move_to(at(85., 853.));
    path.cubic_bezier_to(at(403., 145.), at(-16., 661.), at(160., 313.));
    path.cubic_bezier_to(at(977., 266.), at(647., -23.), at(971., 28.));
    path.cubic_bezier_to(at(518., 949.), at(983., 494.), at(761., 818.));
    path.cubic_bezier_to(at(85., 853.), at(305., 1064.), at(133., 1001.));
    path.close();
    path.move_to(at(PUPIL[0].0, PUPIL[0].1));
    for &(x, y) in &PUPIL[1..] {
        path.line_to(at(x, y));
    }
    path.close();
    if bounds.size.width >= px(20.0) {
        path.move_to(at(386., 333.));
        path.line_to(at(440., 333.));
        path.line_to(at(440., 369.));
        path.line_to(at(386., 369.));
        path.close();
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, tint);
    }
}
