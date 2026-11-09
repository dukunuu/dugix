//! Canvas: shapes on a fixed 0–100 grid (origin top-left, y down) with
//! declarative keyframe animation. Still data, not code — a frontend asks for
//! the shapes at its clock time ([`Session::canvas_frame`]) and rasterizes them.
//!
//! Positions use both axes' 0–100 units. Circle radii and ray lengths use
//! height units, so a frontend can keep circles round on any width.

use super::{Document, Node, Session, Tone, item_count, line_ok, spend};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;

/// Most shapes one canvas may hold.
const MAX_SHAPES: usize = 48;
/// Most animations one shape may carry.
const MAX_ANIMS: usize = 4;
/// Most keyframes one animation may list.
const MAX_KEYFRAMES: usize = 12;
/// Most vertices one polygon may list.
const MAX_POINTS: usize = 24;
/// Longest duration or delay, in seconds.
const MAX_SECONDS: f64 = 120.0;

/// How an animation continues past its last keyframe.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    /// Start over from the first keyframe.
    #[default]
    Loop,
    /// Play forwards, then backwards.
    Bounce,
    /// Play once and hold the last keyframe.
    Once,
}

/// Timing between neighbouring keyframes.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    /// Slow in and out of each keyframe.
    #[default]
    Smooth,
    /// Constant speed.
    Linear,
}

/// Keyframes for one numeric property, evenly spaced over `seconds`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anim {
    /// Animated property: a shape field, or `dx`, `dy`, `on`.
    pub prop: String,
    /// Two to twelve numeric expressions.
    pub values: Vec<Value>,
    /// Duration of one pass.
    #[serde(default = "two")]
    pub seconds: f64,
    /// Behaviour after the last keyframe.
    #[serde(default)]
    pub repeat: Repeat,
    /// Keyframe timing.
    #[serde(default)]
    pub ease: Ease,
    /// Seconds before the first pass starts.
    #[serde(default)]
    pub delay: f64,
}
const fn two() -> f64 {
    2.0
}
const fn accent() -> Tone {
    Tone::Accent
}

/// One drawable. Numeric fields are expressions over panel state.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    /// A circle centred on `x`,`y`; `r` in height units.
    Circle {
        /// Centre x.
        x: Value,
        /// Centre y.
        y: Value,
        /// Radius in height units.
        r: Value,
        /// Theme role.
        #[serde(default = "accent")]
        tone: Tone,
        /// Filled rather than outlined.
        #[serde(default)]
        fill: bool,
        /// Keyframe animations.
        #[serde(default)]
        animate: Vec<Anim>,
        /// Action run when the shape is clicked or chosen.
        #[serde(default)]
        action: Option<String>,
    },
    /// An axis-aligned rectangle from its top-left corner.
    Rect {
        /// Left edge.
        x: Value,
        /// Top edge.
        y: Value,
        /// Width.
        w: Value,
        /// Height.
        h: Value,
        /// Theme role.
        #[serde(default = "accent")]
        tone: Tone,
        /// Filled rather than outlined.
        #[serde(default)]
        fill: bool,
        /// Keyframe animations.
        #[serde(default)]
        animate: Vec<Anim>,
        /// Action run when the shape is clicked or chosen.
        #[serde(default)]
        action: Option<String>,
    },
    /// A segment between two points.
    Line {
        /// Start x.
        x1: Value,
        /// Start y.
        y1: Value,
        /// End x.
        x2: Value,
        /// End y.
        y2: Value,
        /// Theme role.
        #[serde(default = "accent")]
        tone: Tone,
        /// Keyframe animations.
        #[serde(default)]
        animate: Vec<Anim>,
        /// Action run when the shape is clicked or chosen.
        #[serde(default)]
        action: Option<String>,
    },
    /// A segment from a point at an angle — clock hands, spokes, needles.
    Ray {
        /// Start x.
        x: Value,
        /// Start y.
        y: Value,
        /// Length in height units.
        len: Value,
        /// Degrees clockwise from twelve o'clock.
        angle: Value,
        /// Theme role.
        #[serde(default = "accent")]
        tone: Tone,
        /// Keyframe animations.
        #[serde(default)]
        animate: Vec<Anim>,
        /// Action run when the shape is clicked or chosen.
        #[serde(default)]
        action: Option<String>,
    },
    /// A single point.
    Dot {
        /// X.
        x: Value,
        /// Y.
        y: Value,
        /// Theme role.
        #[serde(default = "accent")]
        tone: Tone,
        /// Keyframe animations.
        #[serde(default)]
        animate: Vec<Anim>,
        /// Action run when the shape is clicked or chosen.
        #[serde(default)]
        action: Option<String>,
    },
    /// A closed polygon through `[x, y]` vertices.
    Poly {
        /// Two to twenty-four vertices.
        points: Vec<[Value; 2]>,
        /// Theme role.
        #[serde(default = "accent")]
        tone: Tone,
        /// Filled rather than outlined.
        #[serde(default)]
        fill: bool,
        /// Keyframe animations (`dx`, `dy`, `on`).
        #[serde(default)]
        animate: Vec<Anim>,
        /// Action run when the shape is clicked or chosen.
        #[serde(default)]
        action: Option<String>,
    },
    /// Text centred on `x`,`y`.
    Label {
        /// Text template.
        text: String,
        /// Centre x.
        x: Value,
        /// Row y.
        y: Value,
        /// Theme role.
        #[serde(default = "accent")]
        tone: Tone,
        /// Keyframe animations.
        #[serde(default)]
        animate: Vec<Anim>,
        /// Action run when the shape is clicked or chosen.
        #[serde(default)]
        action: Option<String>,
    },
}

/// A shape resolved at one clock time, in 0–100 canvas units.
#[derive(Clone, Debug, PartialEq)]
pub enum Drawn {
    /// Circle; `r` in height units.
    Circle {
        /// Centre.
        x: f64,
        /// Centre.
        y: f64,
        /// Radius in height units.
        r: f64,
        /// Filled.
        fill: bool,
    },
    /// Rectangle from its top-left corner.
    Rect {
        /// Left.
        x: f64,
        /// Top.
        y: f64,
        /// Width.
        w: f64,
        /// Height.
        h: f64,
        /// Filled.
        fill: bool,
    },
    /// Segment.
    Line {
        /// Start.
        x1: f64,
        /// Start.
        y1: f64,
        /// End.
        x2: f64,
        /// End.
        y2: f64,
    },
    /// Polar segment; `len` in height units.
    Ray {
        /// Start.
        x: f64,
        /// Start.
        y: f64,
        /// Length in height units.
        len: f64,
        /// Degrees clockwise from twelve o'clock.
        angle: f64,
    },
    /// Point.
    Dot {
        /// X.
        x: f64,
        /// Y.
        y: f64,
    },
    /// Closed polygon.
    Poly {
        /// Vertices.
        points: Vec<(f64, f64)>,
        /// Filled.
        fill: bool,
    },
    /// Centred text.
    Label {
        /// Rendered text.
        text: String,
        /// Centre x.
        x: f64,
        /// Row y.
        y: f64,
    },
}

impl Shape {
    /// Theme role.
    #[must_use]
    pub fn tone(&self) -> Tone {
        match self {
            Self::Circle { tone, .. }
            | Self::Rect { tone, .. }
            | Self::Line { tone, .. }
            | Self::Ray { tone, .. }
            | Self::Dot { tone, .. }
            | Self::Poly { tone, .. }
            | Self::Label { tone, .. } => *tone,
        }
    }
    /// The action this shape runs when clicked, if it is clickable.
    #[must_use]
    pub fn action(&self) -> Option<&str> {
        match self {
            Self::Circle { action, .. }
            | Self::Rect { action, .. }
            | Self::Line { action, .. }
            | Self::Ray { action, .. }
            | Self::Dot { action, .. }
            | Self::Poly { action, .. }
            | Self::Label { action, .. } => action.as_deref(),
        }
    }
    fn animations(&self) -> &[Anim] {
        match self {
            Self::Circle { animate, .. }
            | Self::Rect { animate, .. }
            | Self::Line { animate, .. }
            | Self::Ray { animate, .. }
            | Self::Dot { animate, .. }
            | Self::Poly { animate, .. }
            | Self::Label { animate, .. } => animate,
        }
    }
    /// Named numeric fields, in declaration order.
    fn fields(&self) -> Vec<(&'static str, &Value)> {
        match self {
            Self::Circle { x, y, r, .. } => vec![("x", x), ("y", y), ("r", r)],
            Self::Rect { x, y, w, h, .. } => vec![("x", x), ("y", y), ("w", w), ("h", h)],
            Self::Line { x1, y1, x2, y2, .. } => {
                vec![("x1", x1), ("y1", y1), ("x2", x2), ("y2", y2)]
            }
            Self::Ray {
                x, y, len, angle, ..
            } => vec![("x", x), ("y", y), ("len", len), ("angle", angle)],
            Self::Dot { x, y, .. } | Self::Label { x, y, .. } => vec![("x", x), ("y", y)],
            Self::Poly { .. } => Vec::new(),
        }
    }
}

/// Position of `t` within an animation, as a keyframe-space fraction 0–1.
fn progress(anim: &Anim, t: f64) -> f64 {
    let local = (t - anim.delay).max(0.0) / anim.seconds;
    match anim.repeat {
        Repeat::Once => local.min(1.0),
        Repeat::Loop => local.fract(),
        Repeat::Bounce => {
            let phase = local % 2.0;
            if phase <= 1.0 { phase } else { 2.0 - phase }
        }
    }
}

/// Interpolate keyframes at fraction `p`, easing within each segment.
fn sample(values: &[f64], p: f64, ease: Ease) -> f64 {
    let Some(&first) = values.first() else {
        return 0.0;
    };
    if values.len() == 1 {
        return first;
    }
    #[allow(clippy::cast_precision_loss)]
    let segments = (values.len() - 1) as f64;
    let position = p.clamp(0.0, 1.0) * segments;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = (position.floor() as usize).min(values.len() - 2);
    #[allow(clippy::cast_precision_loss)]
    let mut f = position - index as f64;
    if ease == Ease::Smooth {
        f = 0.5 - 0.5 * (std::f64::consts::PI * f).cos();
    }
    values[index] + (values[index + 1] - values[index]) * f
}

impl Session {
    /// Seconds since the frontend first showed this panel; drives canvas
    /// animation. Presentation only: never state, never saved.
    #[must_use]
    pub fn clock(&self) -> f64 {
        self.clock
    }
    /// Advance the animation clock. Does not mark the session changed.
    pub fn set_clock(&mut self, seconds: f64) {
        if seconds.is_finite() {
            self.clock = seconds.max(0.0);
        }
    }
    /// Shapes at the current clock, in drawing order, with their index in
    /// `shapes` and their tones. Shapes switched off (`on` below one half) are
    /// omitted; a shape whose numbers fail to evaluate is skipped rather than
    /// blanking the canvas.
    #[must_use]
    pub fn canvas_frame(&self, shapes: &[Shape]) -> Vec<(usize, Drawn, Tone)> {
        shapes
            .iter()
            .enumerate()
            .filter_map(|(index, shape)| Some((index, self.resolve(shape)?, shape.tone())))
            .collect()
    }
    fn resolve(&self, shape: &Shape) -> Option<Drawn> {
        let t = self.clock;
        let animated = |prop: &str| -> Option<Option<f64>> {
            let Some(anim) = shape.animations().iter().rev().find(|a| a.prop == prop) else {
                return Some(None);
            };
            let values = anim
                .values
                .iter()
                .map(|value| self.number(value).ok())
                .collect::<Option<Vec<_>>>()?;
            Some(Some(sample(&values, progress(anim, t), anim.ease)))
        };
        let get = |prop: &str, base: Option<&Value>| -> Option<f64> {
            match animated(prop)? {
                Some(value) => Some(value),
                None => match base {
                    Some(value) => self.number(value).ok(),
                    None => Some(if prop == "on" { 1.0 } else { 0.0 }),
                },
            }
        };
        if get("on", None)? < 0.5 {
            return None;
        }
        let dx = get("dx", None)?;
        let dy = get("dy", None)?;
        let fields = shape.fields();
        let field = |name: &str| -> Option<f64> {
            let base = fields.iter().find(|(key, _)| *key == name).map(|(_, v)| *v);
            get(name, base)
        };
        Some(match shape {
            Shape::Circle { fill, .. } => Drawn::Circle {
                x: field("x")? + dx,
                y: field("y")? + dy,
                r: field("r")?.max(0.0),
                fill: *fill,
            },
            Shape::Rect { fill, .. } => Drawn::Rect {
                x: field("x")? + dx,
                y: field("y")? + dy,
                w: field("w")?.max(0.0),
                h: field("h")?.max(0.0),
                fill: *fill,
            },
            Shape::Line { .. } => Drawn::Line {
                x1: field("x1")? + dx,
                y1: field("y1")? + dy,
                x2: field("x2")? + dx,
                y2: field("y2")? + dy,
            },
            Shape::Ray { .. } => Drawn::Ray {
                x: field("x")? + dx,
                y: field("y")? + dy,
                len: field("len")?.max(0.0),
                angle: field("angle")?,
            },
            Shape::Dot { .. } => Drawn::Dot {
                x: field("x")? + dx,
                y: field("y")? + dy,
            },
            Shape::Poly { points, fill, .. } => Drawn::Poly {
                points: points
                    .iter()
                    .map(|[x, y]| Some((self.number(x).ok()? + dx, self.number(y).ok()? + dy)))
                    .collect::<Option<Vec<_>>>()?,
                fill: *fill,
            },
            Shape::Label { text, .. } => Drawn::Label {
                text: self.text(text).ok()?,
                x: field("x")? + dx,
                y: field("y")? + dy,
            },
        })
    }
    /// Whether any visible canvas is still moving at the current clock, so a
    /// frontend knows to keep redrawing. A finished `once` animation is still.
    #[must_use]
    pub fn animating(&self) -> bool {
        self.settling()
            || self.visible_nodes().into_iter().any(|(_, node)| {
                let Node::Canvas { shapes, .. } = node else {
                    return false;
                };
                shapes.iter().flat_map(Shape::animations).any(|anim| {
                    anim.repeat != Repeat::Once || self.clock < anim.delay + anim.seconds
                })
            })
    }
}

impl Document {
    pub(super) fn validate_canvas(
        &self,
        shapes: &[Shape],
        rows: u16,
        alt: &str,
        budget: &mut usize,
    ) -> Result<()> {
        ensure!((3..=16).contains(&rows), "canvas rows must be 3–16");
        item_count(shapes.len(), 1, MAX_SHAPES)?;
        line_ok(alt, 120)?;
        for shape in shapes {
            spend(budget)?;
            if let Some(action) = shape.action() {
                ensure!(
                    self.actions.contains_key(action),
                    "unknown shape action: {action}"
                );
            }
            for (_, value) in shape.fields() {
                self.numeric_ok(value, budget)?;
            }
            match shape {
                Shape::Poly { points, .. } => {
                    item_count(points.len(), 2, MAX_POINTS)?;
                    for value in points.iter().flatten() {
                        self.numeric_ok(value, budget)?;
                    }
                }
                Shape::Label { text, .. } => {
                    line_ok(text, 120)?;
                    Self::template_ok(text)?;
                }
                _ => {}
            }
            let animations = shape.animations();
            item_count(animations.len(), 0, MAX_ANIMS)?;
            let fields = shape.fields();
            for anim in animations {
                ensure!(
                    matches!(anim.prop.as_str(), "dx" | "dy" | "on")
                        || fields.iter().any(|(name, _)| *name == anim.prop),
                    "cannot animate {:?} — valid: {}",
                    anim.prop,
                    fields
                        .iter()
                        .map(|(name, _)| *name)
                        .chain(["dx", "dy", "on"])
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                ensure!(
                    (2..=MAX_KEYFRAMES).contains(&anim.values.len()),
                    "animations need 2–{MAX_KEYFRAMES} keyframe values"
                );
                for value in &anim.values {
                    self.numeric_ok(value, budget)?;
                }
                ensure!(
                    anim.seconds.is_finite() && (0.1..=MAX_SECONDS).contains(&anim.seconds),
                    "animation seconds must be 0.1–120"
                );
                ensure!(
                    anim.delay.is_finite() && (0.0..=MAX_SECONDS).contains(&anim.delay),
                    "animation delay must be 0–120 seconds"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim(repeat: Repeat, ease: Ease) -> Anim {
        Anim {
            prop: "x".into(),
            values: vec![0.into(), 100.into()],
            seconds: 2.0,
            repeat,
            ease,
            delay: 1.0,
        }
    }

    #[test]
    fn progress_waits_for_delay_then_repeats() {
        let a = anim(Repeat::Loop, Ease::Linear);
        assert!(progress(&a, 0.5).abs() < 1e-9);
        assert!((progress(&a, 2.0) - 0.5).abs() < 1e-9);
        assert!((progress(&a, 3.5) - 0.25).abs() < 1e-9);
        let a = anim(Repeat::Bounce, Ease::Linear);
        assert!((progress(&a, 4.0) - 0.5).abs() < 1e-9);
        assert!((progress(&a, 4.5) - 0.25).abs() < 1e-9);
        let a = anim(Repeat::Once, Ease::Linear);
        assert!((progress(&a, 50.0) - 1.0).abs() < 1e-9);
    }

    fn session(source: &str) -> Session {
        Session::new(Document::parse(source).unwrap()).unwrap()
    }
    fn canvas(session: &Session) -> &[Shape] {
        match session.document().nodes() {
            [Node::Canvas { shapes, .. }] => shapes,
            other => panic!("expected one canvas, got {other:?}"),
        }
    }

    #[test]
    fn language_builds_shapes_with_state_and_animation() {
        let mut session = session(
            r#"root = Panel("Orbit", [art])
$size = 10
art = Canvas([
  Circle(50, 50, $size, "warning", true),
  Ray(50, 50, 40, 0, "muted", [Anim("angle", [0, 360], 4, "loop", "linear")]),
  Dot(10, 90, null, [Anim("x", [10, 90], 2, "bounce", "linear", 1)]),
  Label("Sun " + $size, 50, 5),
  Poly([[0, 100], [10, 80], [20, 100]], "success", true, [Anim("on", [1, 0], 1, "loop", "linear")])
], 10, "A ray sweeping around a sun")"#,
        );
        assert_eq!(session.document().nodes()[0].rows(), 10);
        assert!(session.animating());
        let frame = |session: &Session| {
            session
                .canvas_frame(canvas(session))
                .into_iter()
                .map(|(_, drawn, _)| drawn)
                .collect::<Vec<_>>()
        };
        let start = frame(&session);
        assert_eq!(
            start[0],
            Drawn::Circle {
                x: 50.0,
                y: 50.0,
                r: 10.0,
                fill: true
            }
        );
        assert!(matches!(start[3], Drawn::Label { ref text, .. } if text == "Sun 10"));
        assert_eq!(start.len(), 5);
        session.set_clock(2.0);
        let later = frame(&session);
        assert!(matches!(later[1], Drawn::Ray { angle, .. } if (angle - 180.0).abs() < 1e-9));
        // One second past its one-second delay: half way along.
        assert!(matches!(later[2], Drawn::Dot { x, .. } if (x - 50.0).abs() < 1e-9));
        session.set_clock(0.75);
        assert_eq!(frame(&session).len(), 4, "the polygon blinks off");
        assert!(
            session
                .document()
                .plain_text()
                .contains("[drawing: A ray sweeping around a sun]")
        );
    }

    #[test]
    fn once_animations_settle_and_stop_requesting_frames() {
        let mut session = session(
            r#"root = Panel("Grow", [Canvas([Rect(0, 0, 10, 10, null, true, [Anim("w", [10, 100], 2, "once")])])])"#,
        );
        assert!(session.animating());
        session.set_clock(5.0);
        assert!(!session.animating());
        assert!(matches!(
            session.canvas_frame(canvas(&session))[0].1,
            Drawn::Rect { w, .. } if (w - 100.0).abs() < 1e-9
        ));
        let still = Document::parse(r#"root = Panel("Still", [Canvas([Line(0, 0, 100, 100)])])"#)
            .and_then(Session::new)
            .unwrap();
        assert!(!still.animating());
    }

    #[test]
    fn validation_rejects_unknown_props_and_runaway_timing() {
        for (source, message) in [
            (
                r#"Canvas([Circle(1, 1, 1, null, null, [Anim("w", [1, 2])])])"#,
                "cannot animate \"w\"",
            ),
            (
                r#"Canvas([Dot(1, 1, null, [Anim("x", [1])])])"#,
                "2–12 keyframe",
            ),
            (
                r#"Canvas([Dot(1, 1, null, [Anim("x", [1, 2], 0)])])"#,
                "seconds must be",
            ),
            (r"Canvas([Dot(1, 1)], 40)", "height"),
            (r"Canvas([Dot(1, 1)], 2)", "canvas rows must be 3–16"),
            (r#"Canvas([Dot("a", 1)])"#, "numeric"),
        ] {
            let error = Document::parse(&format!("root = Panel(\"Bad\", [art])\nart = {source}"))
                .err()
                .unwrap_or_else(|| panic!("{source} should fail"))
                .to_string();
            assert!(error.contains(message), "{source}: {error}");
        }
    }

    #[test]
    fn sample_eases_within_each_segment() {
        let values = [0.0, 10.0, 30.0];
        assert!((sample(&values, 0.25, Ease::Linear) - 5.0).abs() < 1e-9);
        assert!((sample(&values, 0.75, Ease::Linear) - 20.0).abs() < 1e-9);
        assert!((sample(&values, 0.25, Ease::Smooth) - 5.0).abs() < 1e-9);
        assert!(sample(&values, 0.1, Ease::Smooth) < sample(&values, 0.1, Ease::Linear));
        assert!((sample(&values, 1.0, Ease::Smooth) - 30.0).abs() < 1e-9);
    }
}
