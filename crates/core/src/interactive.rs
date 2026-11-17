//! Bounded, model-authored interactive UI documents. This is data, not code:
//! scalar state, a small expression language, controls and transactional actions.
//! Tool access is isolated in the explicitly activated, bounded live-data host.

/// The fenced-code language of an interactive panel in a reply.
pub const UI_FENCE: &str = "dugix-ui";

/// Whether a fence language opens an interactive panel.
pub fn is_ui_fence(lang: &str) -> bool {
    lang.eq_ignore_ascii_case(UI_FENCE)
}

mod canvas;
mod extras;
pub use extras::{Branch, DIAGRAM_ROWS, Lane, flatten as tree_branches};
mod motion;
mod play;
pub use canvas::{Anim, Drawn, Ease, Repeat, Shape};
pub use play::Timer;
mod components;
pub use components::{ChartKind, ChartSeries, Section, Tab, TableState, TimelineEvent};
mod practice;
pub use practice::{EditorKey, EditorState, Question, line_col};
mod data;
pub mod live;
mod persist;
mod stream;
mod validation;
pub use components::ViewSnapshot;
pub use persist::{MAX_RECORD, PanelSnapshot, panel_key, url_host};
pub use stream::{StreamParser, component_signatures, merge_program};

mod check;
mod document;
mod expr;
mod node;
mod session;
pub use check::{CheckError, NODE_TYPES};
pub use document::Document;
pub use expr::scalar_text;
pub use node::{
    Accent, BarItem, Button, CalloutTone, Card, CheckItem, Column, ColumnItem, DisplayStep, Frame,
    Mark, Node, Stat, StepState, Tone,
};
pub use session::Session;

use check::Step;
use expr::{
    binding_ok, eval, https_url, identifier, item_count, line_ok, scalar_ok, spend, text_ok,
    validate_expr,
};
use session::Effects;

#[cfg(test)]
use anyhow::Result;
#[cfg(test)]
use serde_json::{Value, json};

/// Model guidance with positional signatures derived from the native library.
#[must_use]
pub fn instructions() -> String {
    format!(
        "{INSTRUCTIONS}\n### Terminal component signatures\n{}",
        component_signatures()
    )
}

/// Model instructions advertised only by clients with an interactive renderer.
pub const INSTRUCTIONS: &str = include_str!("../assets/prompts/interactive-ui.md");
/// Display-only layout guidance, advertised according to reply style.
pub const LAYOUTS: &str = include_str!("../assets/prompts/layouts.md");
/// The `interactive` reply style: a card for nearly every reply, with
/// follow-up answer buttons.
pub const INTERACTIVE_FIRST: &str = include_str!("../assets/prompts/interactive-first.md");

/// Stable transcript-local panel identifier, shared by replay and the TUI.
#[must_use]
pub fn panel_id(owner: usize, ordinal: usize) -> String {
    format!("p{}-{}", owner + 1, ordinal + 1)
}

/// Split the host's completed patch marker from its statement text.
#[must_use]
pub fn edit_patch(source: &str) -> Option<(&str, &str)> {
    let (line, body) = source.trim_start().split_once('\n')?;
    let id = line.trim().strip_prefix("edit ")?.trim();
    (!id.is_empty() && !id.contains(char::is_whitespace)).then_some((id, body))
}

/// Maximum serialized UI document size.
pub const MAX_SOURCE: usize = 16 * 1024;
/// Validation row budget, including title and interaction hint. A panel
/// taller than the screen scrolls to its focused control.
pub const ROWS: u16 = 48;
const MAX_TEXT: usize = 4096;
const MAX_VALUE: usize = 1024;

#[cfg(test)]
#[path = "interactive/power_tests.rs"]
mod power_tests;

#[cfg(test)]
mod check_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "interactive/input_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "interactive/answer_tests.rs"]
mod answer_tests;

#[cfg(test)]
#[path = "interactive/layouts_tests.rs"]
mod layouts_tests;

#[cfg(test)]
#[path = "interactive/image_tests.rs"]
mod image_tests;

#[cfg(test)]
#[path = "interactive/components_tests.rs"]
mod components_tests;

#[cfg(test)]
#[path = "interactive/extended_tests.rs"]
mod extended_tests;

#[cfg(test)]
#[path = "interactive/persist_tests.rs"]
mod persist_tests;

#[cfg(test)]
#[path = "interactive/stepper_tests.rs"]
mod stepper_tests;

#[cfg(test)]
#[path = "interactive/containers_tests.rs"]
mod containers_tests;

#[cfg(test)]
#[path = "interactive/play_tests.rs"]
mod play_tests;
