//! Landing motion. After a committed change of where panes sit (a pane drop, a
//! float docked by dragging, the Float and Dock commands) pane frames glide from
//! where they were drawn to their new rects over `LANDING_DURATION`, easing out.
//! Only the drawn rects move: no PTY is resized while a landing plays, and one
//! resize follows as it ends. With reduced motion every change is instant.
use super::*;

const LANDING_DURATION: Duration = Duration::from_millis(150);
/// A committed change whose layout has not appeared by then lands without motion.
const ARM_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(in crate::gui) struct PaneMotion {
    /// A committed change waiting for its layout: when it was committed, and each
    /// visible pane's drawn rect at that moment.
    armed: Option<(Instant, HashMap<PaneId, layout::Rect>)>,
    landing: Option<Landing>,
}

struct Landing {
    from: HashMap<PaneId, layout::Rect>,
    started: Instant,
}

/// What `PaneMotion::advance` found for this layout pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MotionStep {
    /// Nothing is moving.
    Still,
    /// Drawn rects are interpolated; PTY resizes wait.
    Landing,
    /// A landing has just finished: resize now.
    Landed,
}

/// Ease-out progress `elapsed` into a landing: exactly 0 at the start and 1 from
/// `LANDING_DURATION` on.
fn landing_progress(elapsed: Duration) -> f32 {
    if elapsed >= LANDING_DURATION {
        return 1.0;
    }
    let t = elapsed.as_secs_f32() / LANDING_DURATION.as_secs_f32();
    1.0 - (1.0 - t).powi(3)
}

impl PaneMotion {
    /// A change was committed with panes drawn at `drawn`. With reduced motion
    /// nothing is remembered, so the change shows at once.
    fn arm(&mut self, drawn: HashMap<PaneId, layout::Rect>, now: Instant, reduced: bool) {
        self.armed = (!reduced).then_some((now, drawn));
    }

    /// One layout pass. `rects` gives the new target rects and is called only
    /// while a change is armed; a landing starts once they differ from the drawn
    /// rects remembered at the commit.
    pub(super) fn advance(
        &mut self,
        rects: impl FnOnce() -> HashMap<PaneId, layout::Rect>,
        now: Instant,
    ) -> MotionStep {
        if let Some((committed, drawn)) = &self.armed {
            if now.saturating_duration_since(*committed) >= ARM_TIMEOUT {
                self.armed = None;
            } else if rects() != *drawn
                && let Some((_, from)) = self.armed.take()
            {
                self.landing = Some(Landing { from, started: now });
            }
        }
        match &self.landing {
            None => MotionStep::Still,
            Some(landing) if now.saturating_duration_since(landing.started) < LANDING_DURATION => {
                MotionStep::Landing
            }
            Some(_) => {
                self.landing = None;
                MotionStep::Landed
            }
        }
    }

    /// Where `pane`, bound for `target`, is drawn at `now`. Panes that were not
    /// visible at the commit appear at their target.
    fn drawn(&self, pane: &PaneId, target: layout::Rect, now: Instant) -> layout::Rect {
        let Some(landing) = &self.landing else {
            return target;
        };
        let Some(from) = landing.from.get(pane) else {
            return target;
        };
        from.lerp(
            target,
            landing_progress(now.saturating_duration_since(landing.started)),
        )
    }

    /// A landing is playing.
    pub(super) fn landing(&self) -> bool {
        self.landing.is_some()
    }
}

impl CompiApp {
    /// Every visible pane's drawn rect: tiled panes (zoom aware) and float frames.
    fn drawn_rects(&self) -> HashMap<PaneId, layout::Rect> {
        let now = Instant::now();
        self.visible_layout()
            .into_iter()
            .flat_map(|layout| layout.panes.iter().map(|pane| (&pane.pane_id, pane.rect)))
            .chain(
                self.float_layouts
                    .iter()
                    .map(|float| (&float.pane.pane_id, float.frame)),
            )
            .map(|(pane, rect)| (pane.clone(), self.motion.drawn(pane, rect, now)))
            .collect()
    }

    /// A committed change is on its way: its panes land from where they are drawn
    /// now, once the next layout pass shows them elsewhere.
    pub(super) fn arm_landing(&mut self) {
        let reduced = *focus::REDUCED_MOTION;
        let drawn = if reduced {
            HashMap::new()
        } else {
            self.drawn_rects()
        };
        self.motion.arm(drawn, Instant::now(), reduced);
    }

    /// Where `pane`, laid out at `target`, is drawn this frame.
    pub(super) fn drawn_rect(&self, pane: &PaneId, target: layout::Rect) -> layout::Rect {
        self.motion.drawn(pane, target, Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> layout::Rect {
        layout::Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn interpolation_hits_both_endpoints_exactly() {
        let from = rect(10.3, 20.7, 300.1, 200.9);
        let to = rect(400.0, 0.0, 512.5, 640.25);
        assert_eq!(from.lerp(to, 0.0), from);
        assert_eq!(from.lerp(to, 1.0), to);
        assert_eq!(from.lerp(to, -0.5), from);
        assert_eq!(from.lerp(to, 1.5), to);
        let middle = rect(0.0, 0.0, 100.0, 100.0).lerp(rect(100.0, 50.0, 300.0, 100.0), 0.5);
        assert_eq!(middle, rect(50.0, 25.0, 200.0, 100.0));
        assert_eq!(landing_progress(Duration::ZERO), 0.0);
        assert_eq!(landing_progress(LANDING_DURATION), 1.0);
        assert_eq!(landing_progress(LANDING_DURATION * 2), 1.0);
        let early = landing_progress(LANDING_DURATION / 4);
        assert!(early > 0.25 && early < 1.0, "eases out: {early}");
    }

    #[test]
    fn a_committed_change_lands_then_resizes_once() {
        let pane = PaneId::new("a");
        let (old, new) = (rect(0.0, 0.0, 100.0, 100.0), rect(100.0, 0.0, 100.0, 100.0));
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.arm(HashMap::from([(pane.clone(), old)]), start, false);
        // The new layout has not arrived yet: nothing moves.
        let same = || HashMap::from([(pane.clone(), old)]);
        assert_eq!(motion.advance(same, start), MotionStep::Still);
        let moved = || HashMap::from([(pane.clone(), new)]);
        assert_eq!(motion.advance(moved, start), MotionStep::Landing);
        assert_eq!(motion.drawn(&pane, new, start), old);
        let halfway = motion.drawn(&pane, new, start + LANDING_DURATION / 2);
        assert!(halfway.x > 0.0 && halfway.x < 100.0);
        let later = start + LANDING_DURATION;
        assert_eq!(motion.advance(moved, later), MotionStep::Landed);
        assert_eq!(motion.drawn(&pane, new, later), new);
        assert_eq!(motion.advance(moved, later), MotionStep::Still);
    }

    #[test]
    fn reduced_motion_is_instant() {
        let pane = PaneId::new("a");
        let (old, new) = (rect(0.0, 0.0, 100.0, 100.0), rect(100.0, 0.0, 100.0, 100.0));
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.arm(HashMap::from([(pane.clone(), old)]), start, true);
        let moved = || HashMap::from([(pane.clone(), new)]);
        assert_eq!(motion.advance(moved, start), MotionStep::Still);
        assert_eq!(motion.drawn(&pane, new, start), new);
    }

    #[test]
    fn an_armed_change_that_never_arrives_expires() {
        let pane = PaneId::new("a");
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.arm(
            HashMap::from([(pane.clone(), rect(0.0, 0.0, 1.0, 1.0))]),
            start,
            false,
        );
        let moved = || HashMap::from([(pane.clone(), rect(5.0, 0.0, 1.0, 1.0))]);
        assert_eq!(
            motion.advance(moved, start + ARM_TIMEOUT),
            MotionStep::Still
        );
        assert!(!motion.landing());
    }
}
