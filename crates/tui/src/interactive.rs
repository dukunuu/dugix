//! Model-authored UI lifecycle, rendering and input. Source remains in normal
//! messages; temporary state is keyed by message, fence ordinal and revision.

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::style::glyph;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dugix_core::interactive::{Node, Session};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use std::collections::HashMap;

mod bold;
mod canvas;
mod components;
mod entry;
mod extras;
mod inputs;
mod keys;
mod layout;
mod lines;
pub(crate) mod live;
mod persist;
mod practice;
mod widgets;
pub(crate) use persist::{flush, flush_settled, open_pinned, pin_focused, sync_saved};

pub(crate) use keys::{can_send, copy_selection};
pub use keys::{focus, key, mouse, render_popup};
pub(crate) use layout::{ImageTarget, block_height, document_images, image_targets};
pub(crate) use widgets::{PendingImage, safe};

#[cfg(test)]
pub(crate) use layout::source_height;

use keys::review;
use layout::{Run, accent_color, branch_regions, run_kind, tone_color, wrapped_text};
use lines::{
    BarRow, clipped, control_style, display, ratio_text, render_bar, stat_lines, table_columns,
};
use widgets::Hit;

#[cfg(test)]
use crate::ui::popups::chrome;
#[cfg(test)]
use crossterm::event::{KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
#[cfg(test)]
use dugix_core::app::{AppCommand, Popup};
#[cfg(test)]
use dugix_core::interactive::Document;
#[cfg(test)]
use layout::{button_label, card_height, node_regions, primary_button, streaming_buffer};
#[cfg(test)]
use lines::render_table;
#[cfg(test)]
use ratatui::buffer::Buffer;
#[cfg(test)]
use ratatui::style::Color;
#[cfg(test)]
use unicode_width::UnicodeWidthStr;

const PRESS_FLASH_MS: u64 = 140;

/// Fixed image reservation with a visible, centered failure placeholder.
pub(crate) fn image_lines(
    url: &str,
    alt: &str,
    width: u16,
    rows: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = crate::app_view::image_lines(url, alt, width, rows, theme);
    if matches!(
        crate::app_view::remote_image(url),
        dugix_core::remote_image::Status::Failed
    ) && !lines.is_empty()
    {
        let failure = crate::app_view::image_placeholder(
            url,
            alt,
            &dugix_core::remote_image::Status::Failed,
            theme,
            usize::from(width),
        );
        let pattern = format!("{} ", glyph::MEDIA).repeat(usize::from(width).div_ceil(2));
        let pattern: String = pattern.chars().take(usize::from(width)).collect();
        lines.fill(Line::styled(
            pattern,
            Style::default()
                .fg(theme.fg_dim)
                .add_modifier(Modifier::DIM),
        ));
        let center = lines.len() / 2;
        lines[center] = failure.centered();
    }
    lines
}

/// Stable reserved region in a message's line-based layout.
#[derive(Clone, Debug)]
pub struct Placement {
    pub row: usize,
    pub width: u16,
    pub owner: usize,
    pub ordinal: usize,
    pub height: u16,
    pub source: String,
    pub complete: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    owner: usize,
    ordinal: usize,
    source: String,
}
impl Placement {
    fn key(&self) -> Key {
        Key {
            owner: self.owner,
            ordinal: self.ordinal,
            source: self.source.clone(),
        }
    }
}
#[derive(Clone)]
enum Control {
    Action((usize, usize)),
    Input(usize),
    Toggle(String),
    Select(usize),
    Stepper(usize),
    Table(usize, String),
    Tabs(usize),
    Section(usize, usize),
    Form(usize),
    Rating(usize),
    Slider(usize),
    Copy(usize),
    Editor(usize),
    Choice(usize, usize),
    Quiz(usize),
    Reveal(usize),
    /// A clickable canvas shape: canvas node, shape index.
    Shape(usize, usize),
    Load,
}
const SEND_BLOCKED: &str = "Finish the reply and clear your draft before reviewing a UI response.";
fn shape_sends(session: &Session, node: usize, shape: usize) -> bool {
    matches!(session.document().node(node), Some(Node::Canvas { shapes, .. })
        if shapes.get(shape).and_then(dugix_core::interactive::Shape::action)
            .is_some_and(|action| session.document().sends(action)))
}
fn binding_name(code: KeyCode) -> Option<String> {
    Some(match code {
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(ch) if ch.is_ascii_alphanumeric() => ch.to_ascii_lowercase().to_string(),
        _ => return None,
    })
}
fn no_modifier_but_shift(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}
/// Hand the terminal caret to a focused text field so IMEs follow it.
fn place_caret(f: &mut Frame, position: Position) {
    f.buffer_mut()[position].modifier.remove(Modifier::REVERSED);
    f.set_cursor_position(position);
}
struct Entry {
    session: Result<Session, String>,
    focus: usize,
    error: String,
    scroll: u16,
    inputs: std::cell::RefCell<inputs::Inputs>,
    /// When this panel was first shown; canvas animations count from here.
    born: std::time::Instant,
    /// The control last pressed and when, for the press flash.
    pressed: Option<(usize, std::time::Instant)>,
    /// Why part of the model's panel was left out.
    left_out: Option<String>,
}
/// Disposable UI instances and this frame's hit areas; no filesystem writes.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // Independent input, layout and OS-focus flags.
pub struct Widgets {
    entries: HashMap<Key, Entry>,
    previews: HashMap<(usize, usize), dugix_core::interactive::StreamParser>,
    hits: Vec<Hit>,
    visible: Vec<Key>,
    focused: Option<Key>,
    screen: Option<Rect>,
    send_ready: bool,
    answer_blocked: Option<&'static str>,
    expanded: bool,
    pinned: bool,
    layout: Option<(Rect, usize, Vec<Placement>)>,
    /// An image the user asked to see; the app opens the viewer.
    pending_image: Option<PendingImage>,
    pending_status: Option<String>,
    applied_patches: std::collections::HashSet<Key>,
    data_calls: HashMap<String, usize>,
    data_running: Option<live::Running>,
    window_unfocused: bool,
    animating: bool,
    /// The last key or click the panels saw; timers idle after 30 minutes.
    last_input: Option<std::time::Instant>,
    /// Motion in panels (`/motion on|off`), off by default so tests render final frames.
    pub(crate) motion: bool,
    /// Saved-state bookkeeping and pinned apps.
    store: persist::Store,
    /// Vim keys in text fields and code editors (the app's `/vim` setting).
    pub(crate) vim: bool,
}

#[cfg(test)]
#[path = "interactive/data_tests.rs"]
mod data_tests;

#[cfg(test)]
#[path = "interactive/persist_tests.rs"]
mod persist_tests;

#[cfg(test)]
#[path = "interactive/power_tests.rs"]
mod power_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "interactive/answer_tests.rs"]
mod answer_tests;

#[cfg(test)]
#[path = "interactive/components_tests.rs"]
mod components_tests;

#[cfg(test)]
#[path = "interactive/viewer_tests.rs"]
mod viewer_tests;

#[cfg(test)]
#[path = "interactive/layouts_tests.rs"]
mod layouts_tests;

#[cfg(test)]
#[path = "interactive/stepper_tests.rs"]
mod stepper_tests;

#[cfg(test)]
mod failure_band_tests {
    use super::*;

    #[test]
    fn failed_band_keeps_height_and_centers_the_failure_line() {
        let fetcher = dugix_core::remote_image::Fetcher::new(
            std::env::temp_dir().join(format!("dugix-failure-{}", uuid::Uuid::new_v4())),
        );
        crate::app_view::use_fetcher_for_tests(&fetcher);
        let url = "https://img.example.invalid/player.jpg";
        let theme = Theme::default();
        let loading = image_lines(url, "Player", 70, 6, &theme);
        fetcher.seed_failure(url);
        let failed = image_lines(url, "Player", 70, 6, &theme);
        assert_eq!(failed.len(), loading.len());
        assert_eq!(
            failed
                .iter()
                .position(|line| line.to_string().contains("couldn't load")),
            Some(3)
        );
        for (index, line) in failed.iter().enumerate() {
            if index != 3 {
                assert!(line.to_string().contains(glyph::MEDIA));
                assert_eq!(line.width(), 70);
                assert!(line.style.add_modifier.contains(Modifier::DIM));
            }
        }
    }
}

#[cfg(test)]
#[path = "interactive/image_tests.rs"]
mod image_tests;

#[cfg(test)]
#[path = "interactive/stream_tests.rs"]
mod stream_tests;

#[cfg(test)]
#[path = "interactive/extended_tests.rs"]
mod extended_tests;

#[cfg(test)]
#[path = "interactive/practice_tests.rs"]
mod practice_tests;

#[cfg(test)]
#[path = "interactive/input_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "interactive/containers_tests.rs"]
mod containers_tests;
#[cfg(test)]
#[path = "interactive/gallery_tests.rs"]
mod gallery_tests;
#[cfg(test)]
mod reference_tests;
#[cfg(test)]
mod terminal_library_tests;
#[cfg(test)]
mod terminal_overview_tests;

#[cfg(test)]
#[path = "interactive/motion_tests.rs"]
mod motion_tests;

#[cfg(test)]
#[path = "interactive/extras_tests.rs"]
mod extras_tests;
#[cfg(test)]
#[path = "interactive/play_tests.rs"]
mod play_tests;
