//! Presentation motion on the panel clock: bound values glide to new targets
//! and charts draw in the first time a panel shows. Off by default, so
//! streaming previews, exports and tests always see final values; a
//! frontend turns it on for live panels (and leaves it off under
//! reduce-motion). Never state, never saved.

use super::{Node, Session};
use std::collections::HashMap;

/// How long a value takes to reach a new target.
const GLIDE: f64 = 0.25;
/// How long a chart or canvas takes to draw in.
const DRAW_IN: f64 = 0.4;

/// One displayed value easing from `from` to `to`, starting at `since`.
#[derive(Clone, Copy, Debug)]
struct Glide {
    from: f64,
    to: f64,
    since: f64,
}

impl Glide {
    fn at(&self, clock: f64) -> f64 {
        let t = ((clock - self.since) / GLIDE).clamp(0.0, 1.0);
        // Ease out: quick start, gentle landing.
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from + (self.to - self.from) * eased
    }
    fn moving(&self, clock: f64) -> bool {
        (self.to - self.from).abs() > f64::EPSILON && clock < self.since + GLIDE
    }
}

/// Glides keyed by (node index, item) — presentation state for one session.
#[derive(Debug, Default)]
pub(super) struct Motion {
    on: bool,
    glides: std::cell::RefCell<HashMap<(usize, usize), Glide>>,
}

impl Session {
    /// Turn motion on or off. Off shows targets immediately and charts whole.
    pub fn set_motion(&mut self, on: bool) {
        self.motion.on = on;
        if !on {
            self.motion.glides.borrow_mut().clear();
        }
    }
    /// Whether motion is on.
    #[must_use]
    pub fn motion(&self) -> bool {
        self.motion.on
    }
    /// The value to draw for `key` now that its target is `target`: the
    /// target itself on first sight, then a glide whenever it changes.
    #[must_use]
    pub fn eased(&self, key: (usize, usize), target: f64) -> f64 {
        if !self.motion.on || !target.is_finite() {
            return target;
        }
        let clock = self.clock();
        let mut glides = self.motion.glides.borrow_mut();
        let glide = glides.entry(key).or_insert(Glide {
            from: target,
            to: target,
            since: clock,
        });
        if (glide.to - target).abs() > f64::EPSILON {
            *glide = Glide {
                from: glide.at(clock),
                to: target,
                since: clock,
            };
        }
        glide.at(clock)
    }
    /// How much of a chart, sparkline or canvas to draw, 0–1, left to right.
    #[must_use]
    pub fn drawn_fraction(&self) -> f64 {
        if self.motion.on {
            let t = (self.clock() / DRAW_IN).clamp(0.0, 1.0);
            1.0 - (1.0 - t).powi(2)
        } else {
            1.0
        }
    }
    /// Whether a glide or draw-in is still running at the current clock.
    pub(super) fn settling(&self) -> bool {
        if !self.motion.on {
            return false;
        }
        let clock = self.clock();
        let drawing = clock < DRAW_IN
            && self.visible_nodes().into_iter().any(|(_, node)| {
                matches!(
                    node,
                    Node::Chart { .. } | Node::Sparkline { .. } | Node::Canvas { .. }
                )
            });
        drawing
            || self
                .motion
                .glides
                .borrow()
                .values()
                .any(|glide| glide.moving(clock))
    }
}

#[cfg(test)]
mod tests {
    use crate::interactive::{Document, Session};

    fn session() -> Session {
        Session::new(Document::parse(r#"root = Panel("M", [Sparkline([1, 2, 3])])"#).unwrap())
            .unwrap()
    }

    #[test]
    fn values_glide_to_new_targets_only_with_motion_on() {
        let mut session = session();
        assert!((session.eased((0, 0), 0.5) - 0.5).abs() < 1e-9);
        session.set_motion(true);
        session.set_clock(1.0);
        // First sight shows the target; a change glides from where it was.
        assert!((session.eased((0, 0), 0.2) - 0.2).abs() < 1e-9);
        assert!((session.eased((0, 0), 1.0) - 0.2).abs() < 1e-9);
        assert!(session.settling());
        session.set_clock(1.1);
        let mid = session.eased((0, 0), 1.0);
        assert!(mid > 0.2 && mid < 1.0, "{mid}");
        session.set_clock(1.3);
        assert!((session.eased((0, 0), 1.0) - 1.0).abs() < 1e-9);
        assert!(!session.settling());
    }

    #[test]
    fn charts_draw_in_once_from_the_panel_clock() {
        let mut session = session();
        assert!((session.drawn_fraction() - 1.0).abs() < 1e-9);
        session.set_motion(true);
        assert!(session.drawn_fraction() < 1e-9);
        assert!(session.settling());
        session.set_clock(0.2);
        assert!(session.drawn_fraction() > 0.5 && session.drawn_fraction() < 1.0);
        session.set_clock(0.5);
        assert!((session.drawn_fraction() - 1.0).abs() < 1e-9);
        assert!(!session.settling());
    }
}
