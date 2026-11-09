//! Panels that run on their own: timers (`@Every`), key bindings (`@Keys`)
//! and clickable canvas shapes. Still local data: timer and key actions may
//! only change panel state, so nothing sends, answers, opens a URL or runs a
//! tool without the user pressing a real control.

use super::{Document, Effects, Session, Shape, Step};
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::collections::BTreeSet;

/// Most timers one panel may run.
const MAX_TIMERS: usize = 4;
/// Most key bindings one panel may declare.
const MAX_KEYS: usize = 16;
/// Shortest timer interval, in seconds.
const MIN_INTERVAL: f64 = 0.1;

/// Runs a local action every `seconds` while the panel is on screen.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timer {
    /// Interval, at least a tenth of a second.
    pub seconds: f64,
    /// Local action name.
    pub action: String,
}

/// Whether `key` may be bound: arrows, space, a–z and 0–9. Tab, Enter and
/// Esc stay with navigation.
fn bindable(key: &str) -> bool {
    matches!(key, "left" | "right" | "up" | "down" | "space")
        || (key.len() == 1
            && key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

impl Document {
    /// Declared timers.
    #[must_use]
    pub fn timers(&self) -> &[Timer] {
        &self.timers
    }
    /// The action bound to `key`, if any.
    #[must_use]
    pub fn key_action(&self, key: &str) -> Option<&str> {
        self.keys.get(key).map(String::as_str)
    }

    /// Fails unless `action` only changes local state (directly or through
    /// called actions).
    fn local_only(&self, action: &str, seen: &mut BTreeSet<String>) -> Result<()> {
        ensure!(seen.len() < 32, "action calls nest too deeply");
        if !seen.insert(action.to_string()) {
            return Ok(());
        }
        let steps = self
            .actions
            .get(action)
            .ok_or_else(|| anyhow::anyhow!("unknown action: {action}"))?;
        for step in steps {
            match step {
                Step::Set { .. } | Step::Reset { .. } => {}
                Step::Call { action } => self.local_only(action, seen)?,
                _ => anyhow::bail!(
                    "timer and key actions may only @Set or @Reset state — a person must press a button to send, open links or run tools"
                ),
            }
        }
        Ok(())
    }

    pub(super) fn validate_play(&self) -> Result<()> {
        ensure!(
            self.timers.len() <= MAX_TIMERS,
            "maximum {MAX_TIMERS} timers per panel"
        );
        for timer in &self.timers {
            ensure!(
                timer.seconds.is_finite() && (MIN_INTERVAL..=3600.0).contains(&timer.seconds),
                "timer interval must be 0.1–3600 seconds"
            );
            self.local_only(&timer.action, &mut BTreeSet::new())?;
        }
        ensure!(
            self.keys.len() <= MAX_KEYS,
            "maximum {MAX_KEYS} key bindings per panel"
        );
        for (key, action) in &self.keys {
            ensure!(
                bindable(key),
                "unbindable key {key:?} — valid: left, right, up, down, space, a–z, 0–9"
            );
            self.local_only(action, &mut BTreeSet::new())?;
        }
        for node in self.all_nodes() {
            if let super::Node::Canvas { shapes, .. } = node {
                for action in shapes.iter().filter_map(Shape::action) {
                    ensure!(
                        !self.answering.contains(action),
                        "shape actions can't answer; use answer buttons"
                    );
                }
            }
        }
        Ok(())
    }
}

/// When each timer fires next, on the panel's wall clock.
#[derive(Debug, Default)]
pub(super) struct Schedule {
    due: Vec<f64>,
}

impl Session {
    /// Whether this panel runs timers.
    #[must_use]
    pub fn has_timers(&self) -> bool {
        !self.document.timers.is_empty()
    }
    /// Run every timer that has come due by `now` (seconds since the panel
    /// was first shown). A timer that fell behind — the panel was off screen —
    /// runs once and resumes its rhythm rather than replaying the backlog.
    /// Returns whether any timer ran.
    /// # Errors
    /// Returns a failing timer's error; later timers still run next tick.
    pub fn tick(&mut self, now: f64) -> Result<bool> {
        let timers = self.document.timers.clone();
        self.schedule.due.resize(timers.len(), f64::NAN);
        let mut ran = false;
        for (index, timer) in timers.iter().enumerate() {
            let due = &mut self.schedule.due[index];
            if due.is_nan() {
                *due = now + timer.seconds;
                continue;
            }
            if now < *due {
                continue;
            }
            *due = if now - *due > timer.seconds {
                now + timer.seconds
            } else {
                *due + timer.seconds
            };
            self.run_local(&timer.action)?;
            ran = true;
        }
        Ok(ran)
    }
    /// Run a local action from a timer: like a press, but not user
    /// interaction, so it neither wakes live data nor waits on a mutation.
    fn run_local(&mut self, action: &str) -> Result<()> {
        if self.continuation.is_some() {
            return Ok(());
        }
        let mut next = self.state.clone();
        self.run(action, &mut next, &mut Effects::default(), &mut 1024)?;
        self.bindings_ok(&mut next)?;
        self.commit(next)
    }
    /// Run the action bound to `key`; whether one was bound.
    /// # Errors
    /// Returns the action's error.
    pub fn press_key(&mut self, key: &str) -> Result<bool> {
        let Some(action) = self.document.key_action(key).map(str::to_string) else {
            return Ok(false);
        };
        self.activate_action(&action)?;
        Ok(true)
    }
    /// Run a clickable shape's action, like a button press.
    /// # Errors
    /// Rejects shapes without actions; returns the action's error.
    pub fn activate_shape(&mut self, node: usize, shape: usize) -> Result<Option<String>> {
        let Some(super::Node::Canvas { shapes, .. }) = self.document.node(node) else {
            anyhow::bail!("not a canvas");
        };
        let action = shapes
            .get(shape)
            .and_then(Shape::action)
            .ok_or_else(|| anyhow::anyhow!("that shape has no action"))?
            .to_string();
        self.activate_action(&action)
    }
}
