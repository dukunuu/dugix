//! Model-authored UI lifecycle, rendering and input. Source remains in normal
//! messages; temporary state is keyed by message, fence ordinal and revision.

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::popups::chrome;
use crate::ui::style::glyph;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use nexus_core::app::{AppCommand, MAX_CHAT_TASKS, Popup};
use nexus_core::interactive::{
    Accent, Button, CalloutTone, Card, Column, Document, Frame as DocumentFrame, Mark, Node,
    Session, Stat, StepState, Tone,
};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Sparkline, Widget, Wrap};
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

mod bold;
mod canvas;
mod components;
mod inputs;
pub(crate) mod live;
mod persist;
mod practice;
pub(crate) use persist::{flush, flush_settled, open_pinned, pin_focused, sync_saved};

/// How long a pressed control stays inverted.
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
        nexus_core::remote_image::Status::Failed
    ) && !lines.is_empty()
    {
        let failure = crate::app_view::image_placeholder(
            url,
            alt,
            &nexus_core::remote_image::Status::Failed,
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
/// Why a control that would send can't yet.
const SEND_BLOCKED: &str = "Finish the reply and clear your draft before reviewing a UI response.";
/// Whether a canvas shape's action sends a message (and so waits for the
/// reply to finish, like a sending button).
fn shape_sends(session: &Session, node: usize, shape: usize) -> bool {
    matches!(session.document().node(node), Some(Node::Canvas { shapes, .. })
        if shapes.get(shape).and_then(nexus_core::interactive::Shape::action)
            .is_some_and(|action| session.document().sends(action)))
}
/// The `@Keys` name of a key: arrows, space, a–z, 0–9.
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
/// No modifier other than Shift: the key may drive panel shortcuts.
fn plain(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}
/// Hand the terminal caret to a focused text field (for IMEs), replacing
/// the reversed-video cell the editor drew there.
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
    /// The control last pressed and when, for the brief press flash.
    pressed: Option<(usize, std::time::Instant)>,
}
impl Entry {
    fn new(session: Result<Session, String>) -> Self {
        Self {
            session,
            focus: 0,
            error: String::new(),
            scroll: 0,
            inputs: std::cell::RefCell::default(),
            born: std::time::Instant::now(),
            pressed: None,
        }
    }
    /// Move the animation clock to now; whether anything is still moving.
    /// With motion off the clock stays put: canvases hold their first frame,
    /// values snap and charts draw whole.
    fn advance_clock(&mut self, motion: bool, timers: bool) -> bool {
        let elapsed = self.born.elapsed().as_secs_f64();
        let pressing = motion && self.press_flash().is_some();
        let mut failure = None;
        let moving = self.session.as_mut().is_ok_and(|session| {
            session.set_motion(motion);
            if motion {
                session.set_clock(elapsed);
            }
            // Timers run on wall time whether or not motion is on.
            if timers && let Err(error) = session.tick(elapsed) {
                failure = Some(error.to_string());
            }
            session.animating() || (timers && session.has_timers())
        });
        if let Some(error) = failure {
            self.error = error;
        }
        moving || pressing
    }
    /// The control whose press is still flashing.
    fn press_flash(&self) -> Option<usize> {
        self.pressed
            .filter(|(_, at)| at.elapsed() < std::time::Duration::from_millis(PRESS_FLASH_MS))
            .map(|(control, _)| control)
    }
    /// The focused text field, as its node index in the document.
    fn focused_input(&self) -> Option<usize> {
        match self.controls().get(self.focus) {
            Some(Control::Input(node)) => Some(*node),
            _ => None,
        }
    }
    /// Terminal caret of the focused field on a canvas this entry just drew.
    fn input_caret(&self, buffer: &Buffer) -> Option<Position> {
        self.focused_input()?;
        self.inputs.borrow().caret(buffer)
    }
    /// Layouts are display-only: never focusable, pinned or counted as panels.
    fn is_interactive(&self) -> bool {
        self.session
            .as_ref()
            .is_ok_and(|s| !s.document().is_static())
    }
    fn controls(&self) -> Vec<Control> {
        self.session.as_ref().map_or_else(
            |_| Vec::new(),
            |session| {
                session
                    .visible_nodes()
                    .into_iter()
                    .flat_map(|(index, node)| match node {
                        Node::Buttons { items, .. } => items
                            .iter()
                            .enumerate()
                            .map(|(item, _)| Control::Action((index, item)))
                            .collect(),
                        Node::Input { .. } => vec![Control::Input(index)],
                        Node::Toggle { key, .. } => vec![Control::Toggle(key.clone())],
                        Node::Checklist { items } => items
                            .iter()
                            .map(|item| Control::Toggle(item.key.clone()))
                            .collect(),
                        Node::Select { .. } => vec![Control::Select(index)],
                        Node::Stepper { .. } => vec![Control::Stepper(index)],
                        Node::Table {
                            select,
                            sortable,
                            page_size,
                            search,
                            ..
                        } if select.is_some() || *sortable || page_size.is_some() || *search => {
                            vec![Control::Table(index, select.clone().unwrap_or_default())]
                        }
                        Node::Tabs { .. } => vec![Control::Tabs(index)],
                        Node::Accordion { items } => (0..items.len())
                            .map(|section| Control::Section(index, section))
                            .collect(),
                        Node::Form { .. } => vec![Control::Form(index)],
                        Node::Rating { .. } => vec![Control::Rating(index)],
                        Node::Slider { .. } => vec![Control::Slider(index)],
                        Node::Code { .. } => vec![Control::Copy(index)],
                        Node::Editor { .. } => vec![Control::Editor(index)],
                        Node::Choice { options, .. } => (0..options.len())
                            .map(|option| Control::Choice(index, option))
                            .collect(),
                        Node::Quiz { .. } => vec![Control::Quiz(index), Control::Reveal(index)],
                        Node::Canvas { shapes, .. } => shapes
                            .iter()
                            .enumerate()
                            .filter(|(_, shape)| shape.action().is_some())
                            .map(|(shape, _)| Control::Shape(index, shape))
                            .collect(),
                        _ => Vec::new(),
                    })
                    .chain(session.needs_data_load().then_some(Control::Load))
                    .collect()
            },
        )
    }
    fn choices(&self) -> Vec<usize> {
        let Ok(session) = &self.session else {
            return Vec::new();
        };
        self.controls()
            .iter()
            .enumerate()
            .filter_map(|(control, value)| {
                let Control::Action((node, _)) = value else {
                    return None;
                };
                let Some(Node::Buttons { items, .. }) = session.document().node(*node) else {
                    return None;
                };
                choice_list(session.document(), items).then_some(control)
            })
            .collect()
    }
    fn answering(&self) -> bool {
        let Some(Control::Action((node, item))) = self.controls().get(self.focus).cloned() else {
            return false;
        };
        self.session.as_ref().is_ok_and(|session| {
            let Some(Node::Buttons { items, .. }) = session.document().node(node) else {
                return false;
            };
            session.document().answers(&items[item].action)
        })
    }
    fn activate(&mut self, control: usize, can_send: bool, can_answer: bool) -> Option<String> {
        self.focus = control;
        self.pressed = Some((control, std::time::Instant::now()));
        let control = self.controls().get(control).cloned()?;
        let Ok(session) = &mut self.session else {
            return None;
        };
        let result = match control {
            Control::Action((node, item)) => {
                let Some(Node::Buttons { items, .. }) = session.document().node(node) else {
                    return None;
                };
                let action = &items[item].action;
                if session.document().answers(action) {
                    if !can_answer {
                        self.error = "wait for the reply to finish, then pick again".into();
                        return None;
                    }
                } else if session.document().sends(action) && !can_send {
                    self.error = SEND_BLOCKED.into();
                    return None;
                }
                session.activate_button(node, item)
            }
            Control::Toggle(key) => session.toggle(&key).map(|()| None),
            Control::Select(node) => session.cycle(node, true).map(|()| None),
            Control::Tabs(node) => session
                .select_tab(
                    node,
                    (session.tab(node) + 1)
                        % match session.document().node(node) {
                            Some(Node::Tabs { items, .. }) => items.len(),
                            _ => 1,
                        },
                )
                .map(|()| None),
            Control::Section(node, section) => session.expand_section(node, section).map(|()| None),
            Control::Form(node) => {
                let Some(Node::Form { action, .. }) = session.document().node(node) else {
                    return None;
                };
                if session.document().sends(action) && !can_send {
                    self.error = SEND_BLOCKED.into();
                    return None;
                }
                session.submit_form(node)
            }
            Control::Rating(node) => {
                let Some(Node::Rating { key, max, .. }) = session.document().node(node) else {
                    return None;
                };
                let value = u16::try_from(session.selected_row(key)).unwrap_or(0);
                session.rate(node, (value + 1) % (max + 1)).map(|()| None)
            }
            Control::Load => {
                session.load_data(std::time::Instant::now());
                return None;
            }
            Control::Copy(node) => {
                session.copy_code(node);
                return None;
            }
            Control::Editor(node) => session.set_editing(node, true).map(|()| None),
            Control::Choice(node, option) => session.choose(node, option).map(|()| None),
            Control::Quiz(node) => {
                let count = match session.document().node(node) {
                    Some(Node::Quiz { items, .. }) => items.len(),
                    _ => 1,
                };
                session
                    .select_question(node, (session.question(node) + 1) % count)
                    .map(|()| None)
            }
            Control::Reveal(node) => session.reveal(node).map(|()| None),
            Control::Shape(node, shape) => {
                if !can_send && shape_sends(session, node, shape) {
                    self.error = SEND_BLOCKED.into();
                    return None;
                }
                session.activate_shape(node, shape)
            }
            Control::Input(_) | Control::Stepper(_) | Control::Table(..) | Control::Slider(_) => {
                return None;
            }
        };
        match result {
            Ok(message) => {
                self.error.clear();
                message.map(|message| {
                    session
                        .assistant_payload()
                        .map_or(message.clone(), |payload| {
                            format!("{message}\ncontext and form values = {payload}")
                        })
                })
            }
            Err(error) => {
                self.error = error.to_string();
                None
            }
        }
    }
    #[allow(clippy::too_many_lines)]
    fn navigate(&mut self, code: KeyCode) -> bool {
        let Some(control) = self.controls().get(self.focus).cloned() else {
            return false;
        };
        let Ok(session) = &mut self.session else {
            return false;
        };
        let result = match (control, code) {
            (Control::Select(node), KeyCode::Left | KeyCode::Right) => {
                session.cycle(node, code == KeyCode::Right)
            }
            (Control::Stepper(node) | Control::Slider(node), KeyCode::Left | KeyCode::Right) => {
                session.step(node, code == KeyCode::Right)
            }
            (Control::Tabs(node), KeyCode::Left | KeyCode::Right) => {
                let Some(Node::Tabs { items, .. }) = session.document().node(node) else {
                    return false;
                };
                let count = items.len();
                session.select_tab(
                    node,
                    (session.tab(node) + if code == KeyCode::Right { 1 } else { count - 1 })
                        % count,
                )
            }
            (Control::Section(node, section), KeyCode::Left | KeyCode::Right) => {
                if session.section_open(node, section) == (code == KeyCode::Right) {
                    Ok(())
                } else {
                    session.expand_section(node, section)
                }
            }
            (Control::Quiz(node), KeyCode::Left | KeyCode::Right) => {
                let question = session.question(node);
                session.select_question(
                    node,
                    if code == KeyCode::Right {
                        question + 1
                    } else {
                        question.saturating_sub(1)
                    },
                )
            }
            (Control::Rating(node), KeyCode::Left | KeyCode::Right) => {
                let Some(Node::Rating { key, .. }) = session.document().node(node) else {
                    return false;
                };
                let value = u16::try_from(session.selected_row(key)).unwrap_or(0);
                session.rate(
                    node,
                    if code == KeyCode::Right {
                        value.saturating_add(1)
                    } else {
                        value.saturating_sub(1)
                    },
                )
            }
            (Control::Table(node, key), KeyCode::Up | KeyCode::Down) if !key.is_empty() => {
                let rows = session.table_rows(node);
                let row = rows
                    .iter()
                    .position(|row| *row == session.selected_row(&key))
                    .unwrap_or(0);
                let next = if code == KeyCode::Down {
                    row.saturating_add(1)
                } else {
                    row.saturating_sub(1)
                }
                .min(rows.len().saturating_sub(1));
                rows.get(next)
                    .map_or(Ok(()), |row| session.select_row(node, *row))
            }
            (Control::Table(node, _), KeyCode::PageUp | KeyCode::PageDown) => {
                session.page_table(node, code == KeyCode::PageDown)
            }
            (Control::Table(node, _), KeyCode::Left | KeyCode::Right | KeyCode::Char('s'))
                if !session.table_state(node).searching =>
            {
                let Some(Node::Table { columns, .. }) = session.document().node(node) else {
                    return false;
                };
                let column = session.table_state(node).sort.map_or(0, |(column, _)| {
                    if code == KeyCode::Char('s') {
                        column
                    } else {
                        (column
                            + if code == KeyCode::Right {
                                1
                            } else {
                                columns.len() - 1
                            })
                            % columns.len()
                    }
                });
                session.sort_table(node, column)
            }
            (Control::Table(node, _), KeyCode::Char('/') | KeyCode::Enter) => {
                let state = session.table_state(node);
                session.search_table(node, state.query, !state.searching)
            }
            (Control::Table(node, _), KeyCode::Char(ch)) if session.table_state(node).searching => {
                let mut query = session.table_state(node).query;
                query.push(ch);
                session.search_table(node, query, true)
            }
            (Control::Table(node, _), KeyCode::Backspace)
                if session.table_state(node).searching =>
            {
                let mut query = session.table_state(node).query;
                query.pop();
                session.search_table(node, query, true)
            }
            _ => return false,
        };
        self.error = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        true
    }
    fn edit(&mut self, text: &str, backspace: bool) {
        let control = self.controls().get(self.focus).cloned();
        let Ok(session) = &mut self.session else {
            return;
        };
        if let Some(Control::Table(node, _)) = control {
            let state = session.table_state(node);
            if state.searching {
                let mut query = state.query;
                if backspace {
                    query.pop();
                } else {
                    query.extend(text.chars().filter(|ch| !ch.is_control()).take(1024));
                }
                self.error = session
                    .search_table(node, query, true)
                    .err()
                    .map_or_else(String::new, |error| error.to_string());
            }
            return;
        }
        if let Some(Control::Editor(node)) = control {
            if !backspace {
                self.error = session
                    .edit_code(
                        node,
                        &nexus_core::interactive::EditorKey::Insert(text.into()),
                    )
                    .err()
                    .map_or_else(String::new, |error| error.to_string());
            }
            return;
        }
        let Some(Control::Input(node)) = control else {
            return;
        };
        let result = if backspace {
            self.inputs
                .get_mut()
                .key(session, node, KeyCode::Backspace.into())
                .map(|_| ())
        } else {
            self.inputs.get_mut().paste(session, node, text)
        };
        self.error = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
    }
    /// The card's bottom hint line: data status, a pending link, the focused
    /// control's keys (with the vim mode when vim keys are on), or how to start.
    fn hint(&self, width: u16, focused: bool) -> String {
        let choices = self.choices().len().min(9);
        let pending_url = self.session.as_ref().ok().and_then(Session::pending_url);
        if let Some(status) = self.session.as_ref().ok().and_then(Session::data_hint) {
            status.into()
        } else if let Some(url) = pending_url {
            persist::url_prompt(url, usize::from(width))
        } else if focused
            && let Some(node) = practice::editing(self)
            && let Some(mode) = self.inputs.borrow().code_mode_label(node)
        {
            match mode {
                "INSERT" => "INSERT · Esc normal".into(),
                "NORMAL" => "NORMAL · i insert · Esc stop editing".into(),
                mode => format!("{mode} · Esc normal"),
            }
        } else if focused && practice::editing(self).is_some() {
            "typing edits the code · Esc stop editing".into()
        } else if focused
            && let Some(node) = self.focused_input()
            && let Ok(session) = &self.session
            && let Some(mode) = self.inputs.borrow().mode_label(session, node)
        {
            match mode {
                "INSERT" if session.document().node(node).is_some_and(Node::is_textarea) => {
                    "INSERT · Enter newline · Esc normal · Tab next".into()
                }
                "INSERT" => "INSERT · Esc normal · Enter/Tab next".into(),
                "NORMAL" => "NORMAL · i insert · Tab next · Esc done".into(),
                mode => format!("{mode} · Esc normal"),
            }
        } else if focused && let Some(node) = self.focused_input() {
            if self
                .session
                .as_ref()
                .is_ok_and(|session| session.document().node(node).is_some_and(Node::is_textarea))
            {
                "Enter newline · Tab next · Esc done".into()
            } else {
                "Tab next · Shift+arrows select · Ctrl+A all · Esc done".into()
            }
        } else if choices > 0 {
            format!("1–{choices} pick · F2 keyboard")
        } else if focused {
            "Tab next · Enter press · Esc done".into()
        } else {
            "F2 keyboard".into()
        }
    }
    fn canvas(
        &self,
        width: u16,
        theme: &Theme,
        focused: bool,
        can_send: bool,
    ) -> (Buffer, Vec<(Rect, usize)>) {
        let regions = self
            .session
            .as_ref()
            .map_or_else(|_| Vec::new(), |session| node_regions(session, width));
        let height = self.session.as_ref().map_or(6, |_| card_height(&regions));
        let hint = self.hint(width, focused);
        let mut buffer = self.session.as_ref().map_or_else(
            |_| card_buffer(width, height, "UI unavailable", theme, &hint),
            |session| document_buffer(width, height, session, theme, &hint),
        );
        let area = buffer.area;
        let hits = Vec::new();
        let Ok(session) = &self.session else {
            let error = self
                .session
                .as_ref()
                .err()
                .map_or("UI unavailable", String::as_str);
            Paragraph::new(format!("Interactive UI unavailable: {error}"))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(theme.error))
                .render(area, &mut buffer);
            return (buffer, hits);
        };
        if !self.error.is_empty() {
            Paragraph::new(safe(&self.error))
                .style(Style::default().fg(theme.error))
                .render(Rect::new(2, 1, width.saturating_sub(4), 1), &mut buffer);
        }
        let mut hits = render_nodes(
            session,
            &regions,
            theme,
            focused.then_some(self.focus),
            can_send,
            &mut buffer,
        );
        // "You are here": a marker in the panel's left gutter beside the
        // focused control when it starts at the content edge, unless it is a
        // pill (which marks itself).
        if focused
            && let Some((rect, _)) = hits.iter().find(|(_, control)| *control == self.focus)
            && rect.x == CONTENT_X
            && buffer
                .cell((0, rect.y))
                .is_some_and(|cell| cell.symbol() == " ")
            && buffer
                .cell((rect.x, rect.y))
                .is_some_and(|cell| cell.symbol() != glyph::CAP_LEFT)
        {
            buffer.set_string(
                0,
                rect.y,
                glyph::SELECTED,
                Style::default().fg(theme.accent),
            );
        }
        // A just-pressed control flashes inverted for a moment.
        if let Some(control) = self.press_flash()
            && session.motion()
            && let Some((rect, _)) = hits.iter().find(|(_, hit)| *hit == control)
        {
            buffer.set_style(*rect, Style::default().add_modifier(Modifier::REVERSED));
        }
        if focused && let Some(node) = self.focused_input() {
            self.inputs
                .borrow_mut()
                .draw_focused(session, node, width, theme, &mut buffer);
        }
        if session.needs_data_load() {
            let rect = Rect::new(
                2,
                height.saturating_sub(1),
                width.saturating_sub(4).min(30),
                1,
            );
            let control = session_controls(session).saturating_sub(1);
            Paragraph::new(" Load data (may cost) ")
                .style(control_style(theme, focused && self.focus == control))
                .render(rect, &mut buffer);
            hits.push((rect, control));
        }
        if width >= 8 && !session.document().is_static() {
            let expand = Rect::new(width - 4, 0, 3, 1);
            if focused && self.focus == session_controls(session) {
                Paragraph::new(" ⤢ ")
                    .style(
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::UNDERLINED),
                    )
                    .render(expand, &mut buffer);
            }
            hits.push((expand, session_controls(session)));
        }
        (buffer, hits)
    }
    fn image_targets(&self, width: u16) -> Vec<ImageTarget> {
        self.session
            .as_ref()
            .map_or_else(|_| Vec::new(), |session| image_targets(session, width))
    }
    /// The image `v` opens: the focused image button's, else the first one.
    fn focused_image(&self) -> Option<(usize, String)> {
        let session = self.session.as_ref().ok()?;
        let images = document_images(session.document());
        let focused = match self.controls().get(self.focus) {
            Some(Control::Action(target)) => Some(*target),
            _ => None,
        };
        let index = images
            .iter()
            .position(|image| focused.is_some() && image.button == focused)
            .unwrap_or(0);
        images.get(index).map(|image| (index, image.url.clone()))
    }
    fn nodes(
        session: &Session,
        regions: &[Rect],
        theme: &Theme,
        focus: Option<usize>,
        can_send: bool,
        buffer: &mut Buffer,
    ) -> Vec<(Rect, usize)> {
        Self::branch(
            session.document().nodes(),
            session,
            regions,
            theme,
            focus,
            can_send,
            buffer,
            &mut 0,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn branch(
        nodes: &[Node],
        session: &Session,
        regions: &[Rect],
        theme: &Theme,
        focus: Option<usize>,
        can_send: bool,
        buffer: &mut Buffer,
        next_control: &mut usize,
    ) -> Vec<(Rect, usize)> {
        let mut hits = Vec::new();
        let mut control = *next_control;
        for (node, region) in nodes.iter().zip(regions) {
            let index = session.document().node_index(node);
            let region = *region;
            match node {
                Node::Text { text, tone, .. } => {
                    let color = match tone {
                        Tone::Normal | Tone::Accent => theme.fg,
                        Tone::Muted => theme.fg_dim,
                        Tone::Success => theme.success,
                        Tone::Warning => theme.warning,
                        Tone::Error => theme.error,
                    };
                    Paragraph::new(text_lines(session, text, region.width))
                        .style(Style::default().fg(color))
                        .render(region, buffer);
                }
                Node::Buttons { items, .. } => {
                    hits.extend(render_button_group(
                        session,
                        (index, items),
                        region,
                        theme,
                        (focus, control, can_send),
                        buffer,
                    ));
                    control += items.len();
                }
                Node::Input { .. } => {
                    inputs::render_static(
                        node,
                        session,
                        region,
                        theme,
                        focus == Some(control),
                        buffer,
                    );
                    hits.push((region, control));
                    control += 1;
                }
                _ => {
                    if let Some((count, new_hits)) = components::render(
                        node,
                        session,
                        region,
                        theme,
                        (focus, control, can_send),
                        buffer,
                    ) {
                        control += count;
                        hits.extend(new_hits);
                    } else {
                        let new_hits =
                            render_component(node, session, region, theme, focus, control, buffer);
                        control += new_hits.len();
                        hits.extend(new_hits);
                    }
                }
            }
        }
        *next_control = control;
        hits
    }
}
/// One image drawn inside a document, in document order. `button` names the
/// (node, item) of an image button, whose click belongs to its action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocImage {
    pub url: String,
    pub button: Option<(usize, usize)>,
}
/// Every image a document draws, in the order `image_targets` lays them out.
pub(crate) fn document_images(document: &Document) -> Vec<DocImage> {
    let plain = |url: &String| DocImage {
        url: url.clone(),
        button: None,
    };
    let mut out = Vec::new();
    for (index, node) in document.all_nodes().into_iter().enumerate() {
        match node {
            Node::Image { src, .. } => out.push(plain(src)),
            Node::Hero {
                image: Some(url), ..
            } => out.push(plain(url)),
            Node::Cards { items } => {
                out.extend(items.iter().filter_map(|c| c.image.as_ref().map(plain)));
            }
            Node::Buttons { items, .. } => {
                out.extend(items.iter().enumerate().filter_map(|(item, button)| {
                    button.image.as_ref().map(|url| DocImage {
                        url: url.clone(),
                        button: Some((index, item)),
                    })
                }));
            }
            _ => {}
        }
    }
    out
}
/// Where one image sits on a document canvas. Plain images are clickable
/// over their whole area; an image button exposes only its corner tag so the
/// rest of the button keeps its action.
#[derive(Clone, Debug)]
pub(crate) struct ImageTarget {
    pub rect: Rect,
    pub url: String,
    pub index: usize,
}
/// Card images in the same grid `cards_lines` draws.
fn card_image_rects(items: &[Card], session: &Session, region: Rect) -> Vec<(Rect, String, bool)> {
    let theme = Theme::default();
    let fit = usize::from((region.width.saturating_add(2) / 26).clamp(1, 3)).min(items.len());
    let count = items.len().div_ceil(items.len().div_ceil(fit));
    let columns = u16::try_from(count).unwrap_or(1);
    let cell = region.width.saturating_sub(2 * (columns - 1)) / columns;
    let mut out = Vec::new();
    let mut y = region.y;
    for (row, chunk) in items.chunks(count).enumerate() {
        if row > 0 {
            y = y.saturating_add(1);
        }
        let rows = chunk
            .iter()
            .map(|item| {
                card_lines(item, session, cell, &theme, 0)
                    .len()
                    .saturating_sub(2)
            })
            .max()
            .unwrap_or(0);
        for (column, item) in chunk.iter().enumerate() {
            if let Some(url) = &item.image {
                let head = card_head(item, session, cell, &theme).len();
                out.push((
                    Rect::new(
                        // Past the tile's rail and its two-column padding.
                        region.x + u16::try_from(column).unwrap_or(0) * (cell + 2) + 3,
                        y.saturating_add(1 + u16::try_from(head).unwrap_or(0)),
                        cell.saturating_sub(4),
                        6,
                    ),
                    url.clone(),
                    false,
                ));
            }
        }
        y = y.saturating_add(u16::try_from(rows + 2).unwrap_or(u16::MAX));
    }
    out
}
pub(crate) fn image_targets(session: &Session, width: u16) -> Vec<ImageTarget> {
    let mut targets = Vec::new();
    for (node, region) in components::visible_regions(session, width) {
        let base: usize = session
            .document()
            .all_nodes()
            .into_iter()
            .take(session.document().node_index(node))
            .map(components::image_count)
            .sum();
        let mut found = Vec::new();
        match node {
            Node::Image { src, .. } => found.push((region, src.clone(), false)),
            Node::Hero {
                image: Some(url), ..
            } => found.push((
                Rect::new(region.x, region.y, region.width, 6.min(region.height)),
                url.clone(),
                false,
            )),
            Node::Cards { items } => found.extend(card_image_rects(items, session, region)),
            Node::Buttons { items, .. } => {
                for (item, rect) in button_regions(session, items, region) {
                    if let Some(url) = &items[item].image {
                        let area =
                            Rect::new(rect.x + 1, rect.y + 1, rect.width.saturating_sub(2), 6);
                        // A button too narrow to draw the tag still counts in
                        // the order; it just has nothing to click.
                        let tag = if area.width >= VIEW_TAG_WIDTH + 5 {
                            view_tag(area)
                        } else {
                            Rect::new(area.x, area.y, 0, 0)
                        };
                        found.push((tag, url.clone(), true));
                    }
                }
            }
            _ => {}
        }
        targets.extend(
            found
                .into_iter()
                .enumerate()
                .map(|(index, (rect, url, _))| ImageTarget {
                    rect,
                    url,
                    index: base + index,
                }),
        );
    }
    targets
}
/// Render validated nodes into their measured, padded regions.
fn render_nodes(
    session: &Session,
    regions: &[Rect],
    theme: &Theme,
    focus: Option<usize>,
    can_send: bool,
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    Entry::nodes(session, regions, theme, focus, can_send, buffer)
}
fn text_lines(session: &Session, text: &str, width: u16) -> Vec<Line<'static>> {
    safe(&session.text(text).unwrap_or_else(|error| error.to_string()))
        .split('\n')
        .flat_map(|line| {
            crate::ui::markdown::wrap_styled_line(&Line::from(line), usize::from(width))
        })
        .collect()
}
fn primary_button(document: &Document) -> Option<(usize, usize)> {
    let buttons = || {
        document
            .all_nodes()
            .into_iter()
            .enumerate()
            .flat_map(|(node, value)| {
                let items = match value {
                    Node::Buttons { items, .. } => items.as_slice(),
                    _ => &[],
                };
                let all_answers = items.iter().all(|button| document.answers(&button.action));
                items
                    .iter()
                    .enumerate()
                    .map(move |(item, button)| ((node, item), button, all_answers))
            })
    };
    buttons()
        .find(|(_, button, _)| button.primary)
        .or_else(|| {
            buttons()
                .find(|(_, button, all_answers)| !all_answers && document.sends(&button.action))
        })
        .map(|(index, _, _)| index)
}
fn session_controls(session: &Session) -> usize {
    session
        .visible_nodes()
        .iter()
        .map(|(_, node)| components::control_count(node))
        .sum::<usize>()
        + usize::from(session.needs_data_load())
}
fn button_label(session: &Session, button: &Button) -> String {
    format!(
        "{}{}",
        display(session, &button.label),
        if session.document().answers(&button.action) {
            " →"
        } else if session.document().sends(&button.action) {
            " ↵"
        } else {
            ""
        }
    )
}
fn choice_list(document: &Document, items: &[Button]) -> bool {
    !items.is_empty()
        && items.iter().all(|button| {
            button.body.is_empty() && button.image.is_none() && document.answers(&button.action)
        })
}
fn render_button_group(
    session: &Session,
    (index, items): (usize, &[Button]),
    area: Rect,
    theme: &Theme,
    (focus, control, can_send): (Option<usize>, usize, bool),
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    let mut hits = Vec::new();
    let primary = primary_button(session.document());
    let choices = choice_list(session.document(), items);
    let choice: usize = session
        .visible_nodes()
        .into_iter()
        .take_while(|(node_index, _)| *node_index < index)
        .filter_map(|(_, node)| match node {
            Node::Buttons { items, .. } if choice_list(session.document(), items) => {
                Some(items.len())
            }
            _ => None,
        })
        .sum();
    let tiles = items
        .iter()
        .any(|button| !button.body.is_empty() || button.image.is_some());
    for (item, rect) in button_regions(session, items, area) {
        let button = &items[item];
        let selected = focus == Some(control + item);
        if choices {
            render_choice(
                &display(session, &button.label),
                choice + item + 1,
                rect,
                theme,
                selected,
                buffer,
            );
            hits.push((rect, control + item));
            continue;
        }
        let enabled = can_send
            || session.document().answers(&button.action)
            || !session.document().sends(&button.action);
        let label = button_label(session, button);
        if !tiles {
            let kind = if !enabled {
                bold::Kind::Disabled
            } else if primary == Some((index, item)) {
                bold::Kind::Primary
            } else {
                bold::Kind::Secondary
            };
            let rect = bold::button(
                buffer,
                (rect.x, rect.y),
                rect.width,
                &label,
                (kind, selected),
                accent_color(session.document(), theme),
                theme,
            );
            hits.push((rect, control + item));
            continue;
        }
        let mut body: Vec<_> = button.body.iter().map(String::as_str).collect();
        if button.image.is_some() {
            if body.is_empty() {
                body.push("");
            }
            body.splice(0..0, [""; 6]);
        }
        hits.extend(chrome::render_buttons(
            buffer,
            rect,
            &[chrome::Button {
                action: control + item,
                label: &label,
                body: &body,
                selected,
                primary: primary == Some((index, item)),
                enabled,
            }],
            theme,
        ));
        if let Some(image) = &button.image {
            let area = Rect::new(rect.x + 1, rect.y + 1, rect.width.saturating_sub(2), 6);
            Paragraph::new(image_lines(
                image,
                button.alt.as_deref().unwrap_or_default(),
                area.width,
                area.height,
                theme,
            ))
            .render(area, buffer);
            // The button keeps its click for the action; this tag (and the
            // `v` key) opens the image in the viewer instead.
            if area.width >= VIEW_TAG_WIDTH + 5 {
                Paragraph::new(VIEW_TAG)
                    .style(Style::default().fg(theme.fg).bg(theme.control))
                    .render(view_tag(area), buffer);
            }
        }
    }
    hits
}
/// The small affordance drawn over an image button's top-right corner.
const VIEW_TAG: &str = " v ";
const VIEW_TAG_WIDTH: u16 = 3;
fn view_tag(image_area: Rect) -> Rect {
    let width = VIEW_TAG_WIDTH.min(image_area.width);
    Rect::new(image_area.right() - width, image_area.y, width, 1)
}
fn choice_lines(label: &str, width: u16) -> Vec<Line<'static>> {
    wrapped_text(label, width.saturating_sub(6), Style::default())
}
fn render_choice(
    label: &str,
    number: usize,
    area: Rect,
    theme: &Theme,
    selected: bool,
    buffer: &mut Buffer,
) {
    if area.is_empty() {
        return;
    }
    let mut style = Style::default().fg(if selected { theme.accent } else { theme.fg });
    if selected {
        style = style.bg(theme.selection);
    }
    buffer.set_style(area, style);
    let indent = area.width.min(4);
    Paragraph::new(choice_lines(label, area.width))
        .style(style)
        .render(
            Rect::new(
                area.x + indent,
                area.y,
                area.width.saturating_sub(6),
                area.height,
            ),
            buffer,
        );
    if number <= 9 {
        Paragraph::new(format!(" {number} "))
            .style(Style::default().fg(theme.fg).bg(theme.control))
            .render(Rect::new(area.x, area.y, area.width.min(3), 1), buffer);
    }
    Paragraph::new("→")
        .style(Style::default().fg(theme.fg_dim))
        .render(Rect::new(area.right() - 1, area.y, 1, 1), buffer);
}
fn button_regions(session: &Session, items: &[Button], area: Rect) -> Vec<(usize, Rect)> {
    let mut regions = Vec::new();
    if choice_list(session.document(), items) {
        let mut y = area.y;
        for (item, button) in items.iter().enumerate() {
            let height =
                u16::try_from(choice_lines(&display(session, &button.label), area.width).len())
                    .unwrap_or(u16::MAX)
                    .max(1);
            regions.push((item, Rect::new(area.x, y, area.width, height)));
            y = y.saturating_add(height);
        }
    } else if items
        .iter()
        .any(|button| !button.body.is_empty() || button.image.is_some())
    {
        let content_width = items
            .iter()
            .flat_map(|button| {
                std::iter::once(button_label(session, button).width())
                    .chain(button.body.iter().map(|line| safe(line).width()))
            })
            .max()
            .unwrap_or(0);
        let minimum_width = u16::try_from(content_width)
            .unwrap_or(u16::MAX)
            .saturating_add(4)
            .max(if items.iter().any(|button| button.image.is_some()) {
                20
            } else {
                0
            });
        let columns =
            usize::from((area.width.saturating_add(2) / minimum_width.saturating_add(2)).max(1))
                .min(items.len());
        let count = u16::try_from(columns).unwrap_or(1);
        let width = area.width.saturating_sub((count - 1) * 2) / count;
        let mut y = area.y;
        for (row, chunk) in items.chunks(columns).enumerate() {
            let height = chunk
                .iter()
                .map(|button| {
                    u16::try_from(button.body.len()).unwrap_or(0)
                        + 3
                        + if button.image.is_some() { 6 } else { 0 }
                })
                .max()
                .unwrap_or(3);
            for item in 0..chunk.len() {
                let x = area.x + u16::try_from(item).unwrap_or(0) * (width + 2);
                regions.push((row * columns + item, Rect::new(x, y, width, height)));
            }
            y = y.saturating_add(height).saturating_add(1);
        }
    } else {
        let mut x = area.x;
        let mut y = area.y;
        let review = |item: usize| {
            session.document().sends(&items[item].action)
                && !session.document().answers(&items[item].action)
        };
        let reviews = (0..items.len()).filter(|item| review(*item)).count();
        let order = (0..items.len())
            .filter(|item| !review(*item))
            .chain((0..items.len()).filter(|item| review(*item)));
        for item in order {
            let width = bold::pill_width(&button_label(session, &items[item])).min(area.width);
            let fits = x > area.x && x.saturating_add(width) <= area.right();
            if x > area.x && !fits {
                x = area.x;
                y = y.saturating_add(1);
            }
            if review(item) && reviews == 1 && items.len() > 1 && fits && y == area.y {
                x = area.right().saturating_sub(width);
            }
            regions.push((item, Rect::new(x, y, width, 1)));
            x = x.saturating_add(width).saturating_add(1);
        }
    }
    regions
}
/// Column where panel content starts: two cells in from the edge, leaving a
/// gutter for the focus marker.
const CONTENT_X: u16 = 2;
/// Measure actual content at the available width; declared rows remain core caps.
fn node_regions(session: &Session, width: u16) -> Vec<Rect> {
    branch_regions(
        session,
        session.document().nodes(),
        Rect::new(CONTENT_X, 2, width.saturating_sub(4), u16::MAX),
    )
}
fn branch_regions(session: &Session, nodes: &[Node], area: Rect) -> Vec<Rect> {
    let width = area.width;
    let mut y = area.y;
    let mut previous_text = false;
    nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let text = matches!(node, Node::Text { .. });
            if index > 0 && !(text && previous_text) {
                y = y.saturating_add(1);
            }
            previous_text = text;
            let height = components::height(node, session, width).unwrap_or_else(|| match node {
                Node::Text { text, .. } => u16::try_from(text_lines(session, text, width).len())
                    .unwrap_or(u16::MAX)
                    .max(1),
                Node::Buttons { items, .. } => {
                    button_regions(session, items, Rect::new(2, 0, width, 0))
                        .iter()
                        .map(|(_, rect)| rect.bottom())
                        .max()
                        .unwrap_or(1)
                }
                Node::Input { rules, hint, .. } => {
                    node.rows() + u16::from(!rules.is_empty() || !hint.is_empty())
                }
                _ => layout_lines(node, session, width, &Theme::default()).map_or_else(
                    || node.rows(),
                    |lines| u16::try_from(lines.len()).unwrap_or(u16::MAX).max(1),
                ),
            });
            let region = Rect::new(
                area.x,
                y,
                width,
                height.min(u16::MAX.saturating_sub(y.saturating_add(2))),
            );
            y = region.bottom();
            region
        })
        .collect()
}
fn card_height(regions: &[Rect]) -> u16 {
    regions
        .last()
        .map_or(4, |rect| rect.bottom().saturating_add(2))
}
pub(crate) fn source_height(source: &str, width: u16) -> u16 {
    Document::preview(source)
        .and_then(|document| Session::new(document).ok())
        .map_or(4, |session| card_height(&node_regions(&session, width)))
}

/// In-flight panels are disposable drawings, never registered UI instances.
fn streaming_buffer(document: Option<Document>, width: u16, theme: &Theme) -> Buffer {
    let session = document.and_then(|document| Session::new(document).ok());
    let Some(session) = session else {
        let mut buffer = card_buffer(width, 4, "Designing your interface", theme, "");
        Paragraph::new("◌ Waiting for the first element…")
            .style(Style::default().fg(theme.fg_dim))
            .render(Rect::new(2, 2, width.saturating_sub(4), 1), &mut buffer);
        return buffer;
    };
    let regions = node_regions(&session, width);
    let height = card_height(&regions);
    let mut buffer = document_buffer(width, height, &session, theme, "");
    render_nodes(&session, &regions, theme, None, false, &mut buffer);
    // Replace final-panel affordances with an explicit, inert streaming status.
    if width >= 8 {
        let fill = accent_color(session.document(), theme);
        buffer.set_string(
            width - 3,
            0,
            "◌",
            Style::default().fg(theme.on(fill)).bg(fill),
        );
    }
    let count = session.document().nodes().len();
    let status = format!(
        "◌ composing · {count} element{}",
        if count == 1 { "" } else { "s" }
    );
    let status = clipped(&status, width.saturating_sub(6));
    Paragraph::new(format!(" {status} "))
        .style(Style::default().fg(theme.fg_dim))
        .render(
            Rect::new(2, height - 1, width.saturating_sub(4), 1),
            &mut buffer,
        );
    buffer
}
fn card_buffer(width: u16, height: u16, title: &str, theme: &Theme, hint: &str) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    buffer.set_style(area, theme.background_style());
    panel_chrome(
        &mut buffer,
        &Chrome {
            title: Some(title),
            fill: theme.accent2,
            frame: DocumentFrame::Rounded,
            expand: true,
            hint,
        },
        theme,
    );
    buffer
}
/// What the Bold panel chrome draws around a panel's content.
struct Chrome<'a> {
    /// Title text; `None` leaves the bar empty (a Hero already says it).
    title: Option<&'a str>,
    /// The panel accent: the title bar's fill.
    fill: Color,
    frame: DocumentFrame,
    /// Show the expand affordance at the bar's right end.
    expand: bool,
    /// Key hints, right-aligned on the bottom rule.
    hint: &'a str,
}
/// Bold chrome: a filled title bar on the top row and a rule carrying the
/// hint on the bottom row. Content keeps its two-column inset, so heights and
/// regions are unchanged from the boxed design. `plain` frames draw an
/// unfilled accent title over a rule instead of the bar.
fn panel_chrome(buffer: &mut Buffer, chrome: &Chrome, theme: &Theme) {
    let area = buffer.area;
    if area.width < 4 || area.height < 2 {
        return;
    }
    let width = area.width;
    let title = chrome
        .title
        .map(|title| clipped(&safe(title).to_uppercase(), width.saturating_sub(10)))
        .unwrap_or_default();
    let rule = match chrome.frame {
        DocumentFrame::Double => "═",
        DocumentFrame::Heavy => "━",
        DocumentFrame::Dashed => "╌",
        DocumentFrame::Rounded | DocumentFrame::Plain => "─",
    };
    let dim = Style::default().fg(theme.border_dim);
    if matches!(chrome.frame, DocumentFrame::Plain) {
        buffer.set_string(0, 0, rule.repeat(usize::from(width)), dim);
        if !title.is_empty() {
            buffer.set_string(
                0,
                0,
                format!("{title} "),
                Style::default()
                    .fg(chrome.fill)
                    .add_modifier(Modifier::BOLD),
            );
        }
    } else {
        let ink = theme.on(chrome.fill);
        let bar = Style::default().fg(ink).bg(chrome.fill);
        buffer.set_string(0, 0, glyph::CAP_LEFT, Style::default().fg(chrome.fill));
        buffer.set_string(1, 0, " ".repeat(usize::from(width - 2)), bar);
        buffer.set_string(
            width - 1,
            0,
            glyph::CAP_RIGHT,
            Style::default().fg(chrome.fill),
        );
        if !title.is_empty() {
            buffer.set_string(2, 0, &title, bar.add_modifier(Modifier::BOLD));
        }
    }
    if chrome.expand {
        let style = if matches!(chrome.frame, DocumentFrame::Plain) {
            Style::default().fg(theme.fg_dim)
        } else {
            Style::default().fg(theme.on(chrome.fill)).bg(chrome.fill)
        };
        buffer.set_string(width - 3, 0, "⤢", style);
    }
    let bottom = area.height - 1;
    buffer.set_string(0, bottom, rule.repeat(usize::from(width)), dim);
    if !chrome.hint.is_empty() {
        let hint = format!(" {} ", clipped(chrome.hint, width.saturating_sub(6)));
        let x = width.saturating_sub(2 + u16::try_from(hint.width()).unwrap_or(width));
        buffer.set_string(x, bottom, hint, Style::default().fg(theme.fg_dim));
    }
}
fn accent_color(document: &Document, theme: &Theme) -> Color {
    match document.accent() {
        Accent::Accent => theme.accent,
        Accent::Accent2 => theme.accent2,
        Accent::Success => theme.success,
        Accent::Warning => theme.warning,
        Accent::Error => theme.error,
        Accent::Muted => theme.fg_dim,
    }
}
fn tone_color(tone: Option<Tone>, accent: Color, theme: &Theme) -> Color {
    match tone {
        None => accent,
        Some(Tone::Normal) => theme.fg,
        Some(Tone::Muted) => theme.fg_dim,
        Some(Tone::Accent) => theme.accent,
        Some(Tone::Success) => theme.success,
        Some(Tone::Warning) => theme.warning,
        Some(Tone::Error) => theme.error,
    }
}
fn document_buffer(
    width: u16,
    height: u16,
    session: &Session,
    theme: &Theme,
    hint: &str,
) -> Buffer {
    let doc = session.document();
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    buffer.set_style(area, theme.background_style());
    let duplicate = matches!(doc.nodes().first(), Some(Node::Hero { title, .. })
        if safe(&session.text(title).unwrap_or_default()).trim().to_lowercase()
            == safe(doc.title()).trim().to_lowercase());
    panel_chrome(
        &mut buffer,
        &Chrome {
            title: (!duplicate).then(|| doc.title()),
            fill: accent_color(doc, theme),
            frame: doc.frame(),
            expand: !doc.is_static(),
            hint: if doc.is_static() { "" } else { hint },
        },
        theme,
    );
    buffer
}
fn wrapped_text(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    safe(text)
        .split('\n')
        .flat_map(|line| {
            crate::ui::markdown::wrap_styled_line(&Line::from(line), usize::from(width.max(1)))
        })
        .map(|line| line.style(style))
        .collect()
}
fn styled_text(session: &Session, text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    wrapped_text(&session.text(text).unwrap_or_default(), width, style)
}
fn join_columns(columns: &[Vec<Line<'static>>], widths: &[u16], gap: u16) -> Vec<Line<'static>> {
    let height = columns.iter().map(Vec::len).max().unwrap_or(0);
    (0..height)
        .map(|row| {
            let mut spans = Vec::new();
            for (index, column) in columns.iter().enumerate() {
                let line = column.get(row).cloned().unwrap_or_default();
                let used = line.width();
                let style = line.style;
                spans.extend(
                    line.spans
                        .into_iter()
                        .map(|span| Span::styled(span.content, style.patch(span.style))),
                );
                if index + 1 < columns.len() {
                    let cell = usize::from(widths.get(index).copied().unwrap_or(0) + gap);
                    spans.push(Span::raw(" ".repeat(cell.saturating_sub(used))));
                }
            }
            Line::from(spans)
        })
        .collect()
}
fn prefixed(lines: Vec<Line<'static>>, prefix: &str, style: Style) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let line_style = line.style;
            let mut spans = vec![Span::styled(prefix.to_string(), style)];
            spans.extend(
                line.spans
                    .into_iter()
                    .map(|span| Span::styled(span.content, line_style.patch(span.style))),
            );
            Line::from(spans)
        })
        .collect()
}
fn columns_lines(
    items: &[Column],
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let stacked = width < 56;
    let count = u16::try_from(items.len()).unwrap_or(1);
    let cell = if stacked {
        width
    } else {
        width.saturating_sub(3 * (count - 1)) / count
    };
    let accent = accent_color(session.document(), theme);
    let columns: Vec<_> = items
        .iter()
        .map(|column| {
            let mut lines = styled_text(
                session,
                &column.title,
                cell,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            );
            for item in &column.items {
                let (mark, color) = match item.mark {
                    Mark::Ok => ("✓ ", theme.success),
                    Mark::No => ("✗ ", theme.error),
                    Mark::Dot => ("● ", accent),
                    Mark::None => ("", theme.fg),
                };
                let body = styled_text(
                    session,
                    &item.text,
                    cell.saturating_sub(u16::try_from(mark.width()).unwrap_or(0)),
                    Style::default().fg(theme.fg),
                );
                for (index, line) in body.into_iter().enumerate() {
                    let prefix = if index == 0 {
                        mark.to_string()
                    } else {
                        " ".repeat(mark.width())
                    };
                    lines.extend(prefixed(vec![line], &prefix, Style::default().fg(color)));
                }
            }
            lines
        })
        .collect();
    if stacked {
        let mut lines = Vec::new();
        for column in columns {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(column);
        }
        lines
    } else {
        join_columns(&columns, &vec![cell; columns.len()], 3)
    }
}
fn hero_lines(
    title: &str,
    subtitle: Option<&str>,
    badge: Option<&str>,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let accent = accent_color(session.document(), theme);
    let badge = badge.map(|s| display(session, s));
    let badge_width = badge
        .as_ref()
        .map_or(0, |s| bold::pill_width(s).saturating_add(2));
    let mut lines = styled_text(
        session,
        title,
        width.saturating_sub(badge_width).max(1),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    );
    // The badge rides right after the title's last line.
    if let Some(badge) = badge
        && let Some(last) = lines.last_mut()
    {
        last.spans.push(Span::raw("  "));
        last.spans.extend(bold::pill_spans(
            &clipped(&badge, width.saturating_sub(6)),
            (accent, theme.on(accent)),
            theme.surface,
        ));
    }
    if let Some(subtitle) = subtitle {
        lines.extend(styled_text(
            session,
            subtitle,
            width,
            Style::default().fg(theme.fg_dim),
        ));
    }
    lines
}
/// A card's title and badge rows: everything above its image.
fn card_head(item: &Card, session: &Session, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(4);
    let accent = accent_color(session.document(), theme);
    let color = tone_color(item.tone, accent, theme);
    let mut content = styled_text(
        session,
        &item.title,
        inner,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    );
    if let Some(badge) = &item.badge {
        let badge = display(session, badge);
        let badge_width = bold::pill_width(&badge);
        let pill = bold::pill_spans(
            &clipped(&badge, inner.saturating_sub(4)),
            (color, theme.on(color)),
            theme.raised,
        );
        // On the title row when both fit, so a badge costs no height.
        let single_title = content.len() == 1;
        match content.first_mut() {
            Some(title)
                if single_title
                    && title.width() + usize::from(badge_width) < usize::from(inner) =>
            {
                let gap = usize::from(inner) - title.width() - usize::from(badge_width);
                title.spans.push(Span::raw(" ".repeat(gap)));
                title.spans.extend(pill);
            }
            _ => content.push(Line::from(pill)),
        }
    }
    content
}
/// One bordered card, padded to at least `rows` content rows so cards
/// sharing a grid row line up.
fn card_lines(
    item: &Card,
    session: &Session,
    width: u16,
    theme: &Theme,
    rows: usize,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(4);
    let mut content = card_head(item, session, width, theme);
    if let Some(image) = &item.image {
        content.extend(image_lines(
            image,
            item.alt.as_deref().unwrap_or_default(),
            inner,
            6,
            theme,
        ));
    }
    content.push(Line::default());
    content.extend(styled_text(
        session,
        &item.text,
        inner,
        Style::default().fg(theme.fg),
    ));
    content.resize(content.len().max(rows), Line::default());
    // A raised tile with a tone rail: the rail still groups the card when a
    // transparent background leaves `raised` unpainted.
    let rail_color = tone_color(item.tone, accent_color(session.document(), theme), theme);
    let tile = Style::default().bg(theme.raised);
    let rail = Style::default().fg(rail_color).bg(theme.raised);
    let blank = || {
        Line::from(vec![
            Span::styled(glyph::RAIL, rail),
            Span::styled(" ".repeat(usize::from(width.saturating_sub(1))), tile),
        ])
    };
    let mut lines = vec![blank()];
    for line in content {
        let padding = usize::from(width.saturating_sub(3)).saturating_sub(line.width());
        let mut row = prefixed(vec![line], glyph::RAIL, rail).remove(0);
        row.spans.insert(1, Span::styled("  ", tile));
        row.spans.push(Span::styled(" ".repeat(padding), tile));
        lines.push(row.style(tile));
    }
    lines.push(blank());
    lines
}
fn cards_lines(items: &[Card], session: &Session, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let fit = usize::from((width.saturating_add(2) / 26).clamp(1, 3)).min(items.len());
    // Balance the rows: four cards form 2x2 rather than 3 + 1.
    let count = items.len().div_ceil(items.len().div_ceil(fit));
    let columns = u16::try_from(count).unwrap_or(1);
    let cell = width.saturating_sub(2 * (columns - 1)) / columns;
    let mut lines = Vec::new();
    for chunk in items.chunks(count) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        // Cards in a row share the tallest card's height; empty cells keep a
        // short last grid row left-aligned at the same width.
        let rows = chunk
            .iter()
            .map(|item| {
                card_lines(item, session, cell, theme, 0)
                    .len()
                    .saturating_sub(2)
            })
            .max()
            .unwrap_or(0);
        let mut cards: Vec<_> = chunk
            .iter()
            .map(|item| card_lines(item, session, cell, theme, rows))
            .collect();
        cards.resize(count, Vec::new());
        lines.extend(join_columns(&cards, &vec![cell; count], 2));
    }
    lines
}
fn stat_lines(
    item: &Stat,
    session: &Session,
    (width, big): (u16, bool),
    theme: &Theme,
) -> Vec<Line<'static>> {
    let color = tone_color(item.tone, accent_color(session.document(), theme), theme);
    let value_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
    let value = display(session, &item.value);
    let (number, unit) = crate::ui::style::display_split(&value);
    let mut lines = match crate::ui::style::display_font(number)
        .filter(|rows| big && rows[0].width() + unit.width() <= usize::from(width))
    {
        // Big numerals, with any unit set small on the baseline row.
        Some([top, middle, bottom]) => vec![
            Line::styled(top, value_style),
            Line::styled(middle, value_style),
            Line::from(vec![
                Span::styled(bottom, value_style),
                Span::styled(unit.to_string(), value_style),
            ]),
        ],
        None => styled_text(session, &item.value, width, value_style),
    };
    lines.extend(styled_text(
        session,
        &item.label,
        width,
        Style::default().fg(theme.fg_dim),
    ));
    if let Some(delta) = &item.delta {
        let delta = display(session, delta);
        let negative = delta.trim_start().starts_with('-');
        let arrow = if negative { "▼" } else { "▲" };
        lines.extend(wrapped_text(
            &format!("{arrow} {}", delta.trim_start_matches(['+', '-'])),
            width,
            Style::default().fg(if negative { theme.error } else { theme.success }),
        ));
    }
    lines
}
fn stats_lines(
    items: &[Stat],
    session: &Session,
    (width, big): (u16, bool),
    theme: &Theme,
) -> Vec<Line<'static>> {
    const GAP: u16 = 4;
    // Size each metric to its content when they all fit on one row; otherwise
    // fall back to an equal-width grid that wraps onto further rows.
    let natural: Vec<_> = items
        .iter()
        .map(|item| stat_lines(item, session, (width, big), theme))
        .collect();
    let widths: Vec<_> = natural
        .iter()
        .map(|lines| {
            u16::try_from(lines.iter().map(Line::width).max().unwrap_or(0)).unwrap_or(u16::MAX)
        })
        .collect();
    let total = widths
        .iter()
        .fold(0u16, |sum, w| sum.saturating_add(*w))
        .saturating_add(GAP * u16::try_from(items.len().saturating_sub(1)).unwrap_or(0));
    if total <= width {
        return join_columns(&natural, &widths, GAP);
    }
    let count = usize::from((width.saturating_add(3) / 15).max(1)).min(items.len());
    let columns = u16::try_from(count).unwrap_or(1);
    let cell = width.saturating_sub(3 * (columns - 1)) / columns;
    let mut lines = Vec::new();
    for chunk in items.chunks(count) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let mut stats: Vec<_> = chunk
            .iter()
            .map(|item| stat_lines(item, session, (cell, big), theme))
            .collect();
        stats.resize(count, Vec::new());
        lines.extend(join_columns(&stats, &vec![cell; count], 3));
    }
    lines
}
fn steps_lines(
    items: &[nexus_core::interactive::DisplayStep],
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let accent = accent_color(session.document(), theme);
    let marker = |state| match state {
        StepState::Done => ("●", theme.success),
        StepState::Current => ("◉", accent),
        StepState::Todo => ("○", theme.fg_dim),
    };
    let mut horizontal = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            horizontal.push(Span::raw("   "));
        }
        let (mark, color) = marker(item.state);
        horizontal.push(Span::styled(format!("{mark} "), Style::default().fg(color)));
        horizontal.push(Span::styled(
            display(session, &item.title),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ));
        if let Some(text) = &item.text {
            horizontal.push(Span::styled(
                format!(" — {}", display(session, text)),
                Style::default().fg(theme.fg_dim),
            ));
        }
    }
    let horizontal = Line::from(horizontal);
    if horizontal.width() <= usize::from(width) {
        return vec![horizontal];
    }
    let mut lines = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let (mark, color) = marker(item.state);
        let titles = styled_text(
            session,
            &item.title,
            width.saturating_sub(2),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        );
        for (row, title) in titles.into_iter().enumerate() {
            lines.extend(prefixed(
                vec![title],
                &format!("{} ", if row == 0 { mark } else { "│" }),
                Style::default().fg(color),
            ));
        }
        if let Some(text) = &item.text {
            let body = styled_text(
                session,
                text,
                width.saturating_sub(2),
                Style::default().fg(theme.fg_dim),
            );
            lines.extend(prefixed(
                body,
                if index + 1 < items.len() {
                    "│ "
                } else {
                    "  "
                },
                Style::default().fg(theme.border_dim),
            ));
        }
        if index + 1 < items.len() {
            lines.push(Line::from(Span::styled(
                "│",
                Style::default().fg(theme.border_dim),
            )));
        }
    }
    lines
}
/// A callout: a tinted band behind a rail in the callout's tone.
fn callout_lines(
    tone: CalloutTone,
    title: Option<&str>,
    text: &str,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let color = match tone {
        CalloutTone::Info => theme.accent,
        CalloutTone::Tip => accent_color(session.document(), theme),
        CalloutTone::Warning => theme.warning,
        CalloutTone::Danger => theme.error,
    };
    let mut lines = Vec::new();
    if let Some(title) = title {
        lines.extend(styled_text(
            session,
            title,
            width.saturating_sub(3),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    lines.extend(styled_text(
        session,
        text,
        width.saturating_sub(3),
        Style::default().fg(theme.fg),
    ));
    // A tinted band behind a tone rail.
    let band = Style::default().bg(theme.raised);
    prefixed(lines, "▌ ", Style::default().fg(color).bg(theme.raised))
        .into_iter()
        .map(|mut line| {
            let padding = usize::from(width).saturating_sub(line.width());
            line.spans.push(Span::styled(" ".repeat(padding), band));
            line.style(band)
        })
        .collect()
}
fn layout_lines(
    node: &Node,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Option<Vec<Line<'static>>> {
    let _accent = accent_color(session.document(), theme);
    let lines = match node {
        Node::Image { src, alt, rows } => image_lines(src, alt, width, *rows, theme),
        Node::Hero {
            title,
            subtitle,
            badge,
            image,
            alt,
        } => {
            let mut lines = Vec::new();
            if let Some(image) = image {
                lines.extend(image_lines(
                    image,
                    alt.as_deref().unwrap_or_default(),
                    width,
                    6,
                    theme,
                ));
            }
            lines.extend(hero_lines(
                title,
                subtitle.as_deref(),
                badge.as_deref(),
                session,
                width,
                theme,
            ));
            lines
        }
        Node::Columns { items } => columns_lines(items, session, width, theme),
        Node::Cards { items } => cards_lines(items, session, width, theme),
        Node::Stats { items, big } => stats_lines(items, session, (width, *big), theme),
        Node::Steps { items } => steps_lines(items, session, width, theme),
        Node::Callout { tone, title, text } => {
            callout_lines(*tone, title.as_deref(), text, session, width, theme)
        }
        Node::Quote { text, by } => {
            let mut lines = prefixed(
                styled_text(
                    session,
                    text,
                    width.saturating_sub(2),
                    Style::default().fg(theme.fg).add_modifier(Modifier::ITALIC),
                ),
                "│ ",
                Style::default().fg(theme.border_dim),
            );
            if let Some(by) = by {
                lines.extend(wrapped_text(
                    &format!("— {}", display(session, by)),
                    width,
                    Style::default().fg(theme.fg_dim),
                ));
            }
            lines
        }
        Node::Divider { label } => {
            let label = label
                .as_ref()
                .map_or_else(String::new, |s| format!(" {} ", display(session, s)));
            let label = clipped(&label, width);
            let remaining = usize::from(width).saturating_sub(label.width());
            vec![Line::from(Span::styled(
                format!(
                    "{}{label}{}",
                    "─".repeat(remaining / 2),
                    "─".repeat(remaining.div_ceil(2))
                ),
                Style::default().fg(theme.border_dim),
            ))]
        }
        _ => return None,
    };
    Some(lines)
}
fn display(session: &Session, template: &str) -> String {
    safe(&session.text(template).unwrap_or_default()).replace('\n', " ")
}
/// One focus treatment for unfilled controls: your accent, bold.
fn control_style(theme: &Theme, selected: bool) -> Style {
    let style = Style::default().fg(if selected { theme.accent } else { theme.fg });
    if selected {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}
fn render_toggle(
    session: &Session,
    (key, label): (&str, &str),
    area: Rect,
    (theme, focused): (&Theme, bool),
    buffer: &mut Buffer,
) {
    let on = session.value(key) == "true";
    let mark = if on {
        Span::styled(
            glyph::DOT,
            Style::default().fg(accent_color(session.document(), theme)),
        )
    } else {
        Span::styled(glyph::RING, Style::default().fg(theme.fg_dim))
    };
    Paragraph::new(Line::from(vec![
        mark,
        Span::raw(" "),
        Span::styled(display(session, label), control_style(theme, focused)),
    ]))
    .render(area, buffer);
}
/// Arrow hit regions for an inline stepper: `◀ value ▶` after the label
/// column.
fn stepper_arrows(node: &Node, session: &Session, area: Rect) -> Option<[Rect; 2]> {
    let Node::Stepper { key, label, .. } = node else {
        return None;
    };
    let value_width = u16::try_from(session.value(key).width()).unwrap_or(u16::MAX);
    if area.width < value_width.saturating_add(4) {
        return None;
    }
    let offset = if display(session, label).is_empty() {
        0
    } else {
        bold::label_column(session, area.width)
            .min(area.width.saturating_sub(value_width.saturating_add(4)))
    };
    Some([
        Rect::new(area.x + offset, area.y, 1, 1),
        Rect::new(area.x + offset + value_width + 3, area.y, 1, 1),
    ])
}
/// `LABEL  ◀ value ▶` in the shared label column; arrows dim at the ends of
/// the range or while unfocused.
fn render_arrows(
    (label, value): (&str, &str),
    (at_min, at_max): (bool, bool),
    area: Rect,
    (theme, selected): (&Theme, bool),
    session: &Session,
    buffer: &mut Buffer,
) {
    let value_width = u16::try_from(value.width()).unwrap_or(u16::MAX);
    let column = if label.is_empty() {
        0
    } else {
        bold::label_column(session, area.width)
            .min(area.width.saturating_sub(value_width.saturating_add(4)))
    };
    bold::label(
        buffer,
        Rect::new(area.x, area.y, column, 1),
        label,
        selected,
        theme,
    );
    let live = if selected {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.fg)
    };
    let dim = Style::default().fg(theme.fg_dim);
    Paragraph::new(Line::from(vec![
        Span::styled("◀", if at_min { dim } else { live }),
        Span::raw(" "),
        Span::styled(
            value.to_string(),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("▶", if at_max { dim } else { live }),
    ]))
    .render(
        Rect::new(
            area.x + column,
            area.y,
            area.width.saturating_sub(column),
            1,
        ),
        buffer,
    );
}
fn render_stepper(
    node: &Node,
    session: &Session,
    region: Rect,
    theme: &Theme,
    selected: bool,
    buffer: &mut Buffer,
) {
    let Node::Stepper {
        key,
        label,
        min,
        max,
        ..
    } = node
    else {
        return;
    };
    let value = session.value(key);
    if stepper_arrows(node, session, region).is_some() {
        let number = value.parse::<f64>().unwrap_or_default();
        render_arrows(
            (&display(session, label), &value),
            (number <= *min, number >= *max),
            region,
            (theme, selected),
            session,
            buffer,
        );
    } else {
        Paragraph::new(value)
            .style(control_style(theme, selected).add_modifier(Modifier::BOLD))
            .render(region, buffer);
    }
}
fn render_component(
    node: &Node,
    session: &Session,
    region: Rect,
    theme: &Theme,
    focus: Option<usize>,
    mut control: usize,
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    let mut hits = Vec::new();
    match node {
        Node::Checklist { items } => {
            for (index, item) in items.iter().enumerate() {
                let row = Rect::new(
                    region.x,
                    region.y + u16::try_from(index).unwrap_or(0),
                    region.width,
                    1,
                );
                render_toggle(
                    session,
                    (&item.key, &item.label),
                    row,
                    (theme, focus == Some(control)),
                    buffer,
                );
                hits.push((row, control));
                control += 1;
            }
        }
        Node::Toggle { key, label } => {
            render_toggle(
                session,
                (key, label),
                region,
                (theme, focus == Some(control)),
                buffer,
            );
            hits.push((region, control));
        }
        Node::Select { key, label, .. } => {
            render_arrows(
                (
                    &display(session, label),
                    &display(session, &session.value(key)),
                ),
                (false, false),
                region,
                (theme, focus == Some(control)),
                session,
                buffer,
            );
            hits.push((region, control));
        }
        Node::Stepper { .. } => {
            render_stepper(node, session, region, theme, focus == Some(control), buffer);
            hits.push((region, control));
        }
        Node::Table {
            select,
            sortable,
            page_size,
            search,
            ..
        } => {
            render_table(
                node,
                session,
                region,
                theme,
                focus == Some(control)
                    && (select.is_some() || *sortable || page_size.is_some() || *search),
                buffer,
            );
            if select.is_some() || *sortable || page_size.is_some() || *search {
                hits.push((region, control));
            }
        }
        _ => {
            if let Some(lines) = layout_lines(node, session, region.width, theme) {
                Paragraph::new(lines).render(region, buffer);
            } else {
                render_data(node, session, region, theme, buffer);
            }
        }
    }
    hits
}
fn clipped(text: &str, width: u16) -> String {
    if text.width() <= usize::from(width) {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let size = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + size >= usize::from(width) {
            break;
        }
        result.push(ch);
        used += size;
    }
    result.push('…');
    result
}
fn render_table(
    node: &Node,
    session: &Session,
    area: Rect,
    theme: &Theme,
    focused: bool,
    buffer: &mut Buffer,
) {
    let Node::Table {
        columns,
        rows_data,
        select,
        sortable,
        page_size,
        search,
    } = node
    else {
        return;
    };
    let count = u16::try_from(columns.len()).unwrap_or(1);
    let selected = select.as_ref().map(|key| session.selected_row(key));
    let index = session.document().node_index(node);
    let state = session.table_state(index);
    let rows = session.table_rows(index);
    let size = page_size.unwrap_or(rows_data.len()).max(1);
    let page = state.page.min(rows.len().div_ceil(size).saturating_sub(1));
    let mut data = vec![(None, columns)];
    data.extend(
        rows.iter()
            .skip(page * size)
            .take(size)
            .map(|row| (Some(*row), &rows_data[*row])),
    );
    if *search {
        Paragraph::new(format!(
            "/ Search: {}{}",
            state.query,
            if state.searching { "▁" } else { "" }
        ))
        .style(control_style(theme, focused))
        .render(Rect::new(area.x, area.y, area.width, 1), buffer);
    }
    for (row, (original, cells)) in data.into_iter().enumerate() {
        let is_selected = original.is_some() && selected == original;
        // A filled header, zebra rows, and a filled selection.
        let style = if row == 0 {
            Style::default()
                .fg(theme.fg_dim)
                .bg(theme.control)
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            control_style(theme, focused).bg(theme.selection)
        } else if row % 2 == 0 {
            Style::default().fg(theme.fg).bg(theme.raised)
        } else {
            Style::default().fg(theme.fg)
        };
        let marker = if is_selected { glyph::SELECTED } else { " " };
        let y = area.y + u16::from(*search) + u16::try_from(row).unwrap_or(0);
        buffer.set_style(Rect::new(area.x, y, area.width, 1), style);
        let lead = u16::from(select.is_some()).min(area.width);
        if lead > 0 {
            buffer.set_string(area.x, y, marker, style);
        }
        let width = area.width.saturating_sub(lead);
        for (column, cell) in cells.iter().enumerate() {
            let column = u16::try_from(column).unwrap_or(0);
            let x = width * column / count;
            let end = width * (column + 1) / count;
            let cell_width = (end - x).saturating_sub(u16::from(column + 1 < count));
            let mut text = display(session, cell);
            if row == 0
                && *sortable
                && let Some((sorted, descending)) = state.sort
                && sorted == usize::from(column)
            {
                text.push_str(if descending { " ▾" } else { " ▴" });
            }
            Paragraph::new(clipped(&text, cell_width))
                .style(style)
                .render(Rect::new(area.x + lead + x, y, cell_width, 1), buffer);
        }
    }
    if page_size.is_some() {
        Paragraph::new(format!(
            "‹ {}/{} › · PgUp/PgDn",
            page + 1,
            rows.len().div_ceil(size).max(1)
        ))
        .style(Style::default().fg(theme.fg_dim))
        .render(
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
            buffer,
        );
    }
}
/// One labelled bar row in the Bold layout: label column, bar, value.
struct BarRow<'a> {
    label: &'a str,
    /// Control-style capital label rather than a data category.
    caps: bool,
    value: &'a str,
    /// Width reserved for values, shared by sibling rows so bars line up.
    value_width: u16,
    ratio: f64,
    fill: Color,
}
fn render_bar(row: &BarRow, session: &Session, area: Rect, theme: &Theme, buffer: &mut Buffer) {
    let column = bold::label_column(session, area.width);
    let label_area = Rect::new(area.x, area.y, column, 1);
    if row.caps {
        bold::label(buffer, label_area, row.label, false, theme);
    } else {
        Paragraph::new(clipped(row.label, column.saturating_sub(1)))
            .style(Style::default().fg(theme.fg))
            .render(label_area, buffer);
    }
    let value_width = row.value_width.min(area.width.saturating_sub(column));
    let bar_width = area.width.saturating_sub(column + value_width + 2);
    bold::bar(
        buffer,
        Rect::new(area.x + column, area.y, bar_width, 1),
        row.ratio,
        row.fill,
        theme,
    );
    bold::value(
        buffer,
        Rect::new(area.right() - value_width, area.y, value_width, 1),
        row.value,
        theme,
    );
}
/// "2 / 5", or a percentage when the maximum is one.
fn ratio_text(value: f64, max: f64) -> String {
    if (max - 1.0).abs() < f64::EPSILON {
        format!("{}%", (value * 100.0).round())
    } else {
        format!("{value} / {max}")
    }
}
/// Bars on one shared scale, in the label column layout.
fn render_bars(
    (node, items): (usize, &[nexus_core::interactive::BarItem]),
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    let values: Vec<_> = items
        .iter()
        .map(|item| session.number(&item.value).unwrap_or(0.0).max(0.0))
        .collect();
    let max = values.iter().copied().fold(0.0, f64::max);
    let value_width = values
        .iter()
        .map(|value| value.to_string().width())
        .max()
        .unwrap_or(0);
    for (index, (item, value)) in items.iter().zip(values).enumerate() {
        render_bar(
            &BarRow {
                label: &display(session, &item.label),
                caps: false,
                value: &value.to_string(),
                value_width: u16::try_from(value_width).unwrap_or(0),
                ratio: session.eased((node, index), if max > 0.0 { value / max } else { 0.0 }),
                fill: accent_color(session.document(), theme),
            },
            session,
            Rect::new(
                area.x,
                area.y + u16::try_from(index).unwrap_or(0),
                area.width,
                1,
            ),
            theme,
            buffer,
        );
    }
}
fn render_data(node: &Node, session: &Session, area: Rect, theme: &Theme, buffer: &mut Buffer) {
    match node {
        Node::Kv { items } => {
            let labels: Vec<_> = items
                .iter()
                .map(|pair| display(session, &pair[0]))
                .collect();
            let label_width = u16::try_from(labels.iter().map(|s| s.width()).max().unwrap_or(0))
                .unwrap_or(u16::MAX)
                .min(area.width / 2);
            for (index, (pair, label)) in items.iter().zip(&labels).enumerate() {
                let y = area.y + u16::try_from(index).unwrap_or(0);
                Paragraph::new(clipped(label, label_width))
                    .style(Style::default().fg(theme.fg_dim))
                    .render(Rect::new(area.x, y, label_width, 1), buffer);
                let offset = (label_width + 1).min(area.width);
                Paragraph::new(display(session, &pair[1]))
                    .style(Style::default().fg(theme.fg))
                    .render(
                        Rect::new(area.x + offset, y, area.width - offset, 1),
                        buffer,
                    );
            }
        }
        Node::Progress { label, value, max } => {
            let (value, max) = session.progress(value, max).unwrap_or_default();
            let text = ratio_text(value, max);
            render_bar(
                &BarRow {
                    label: &display(session, label),
                    caps: true,
                    value: &text,
                    value_width: u16::try_from(text.width()).unwrap_or(0),
                    ratio: session.eased(
                        (session.document().node_index(node), 0),
                        if max > 0.0 { value / max } else { 0.0 },
                    ),
                    fill: accent_color(session.document(), theme),
                },
                session,
                area,
                theme,
                buffer,
            );
        }
        Node::Bars { items } => render_bars(
            (session.document().node_index(node), items),
            session,
            area,
            theme,
            buffer,
        ),
        Node::Sparkline { values, .. } => {
            let values: Vec<_> = values
                .iter()
                .map(|value| session.number(value).unwrap_or(0.0).max(0.0))
                .collect();
            // Draw in left to right the first time the panel shows.
            let area = Rect::new(
                area.x,
                area.y,
                bold::revealed(area.width, session),
                area.height,
            );
            let max = values.iter().copied().fold(0.0, f64::max);
            // Normalize before integer conversion to preserve even tiny fractional samples.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let values: Vec<_> = values
                .iter()
                .map(|value| {
                    if max > 0.0 {
                        (value / max * 1000.0) as u64
                    } else {
                        0
                    }
                })
                .collect();
            let levels = glyph::LEVELS;
            let bars = ratatui::symbols::bar::Set {
                empty: " ",
                one_eighth: levels[0],
                one_quarter: levels[1],
                three_eighths: levels[2],
                half: levels[3],
                five_eighths: levels[4],
                three_quarters: levels[5],
                seven_eighths: levels[6],
                full: levels[7],
            };
            Sparkline::default()
                .data(values)
                .bar_set(bars)
                .style(Style::default().fg(accent_color(session.document(), theme)))
                .render(area, buffer);
        }
        _ => {}
    }
}
#[derive(Clone)]
struct Hit {
    rect: Rect,
    key: Key,
    control: Option<usize>,
    row_offset: u16,
    /// An image region: its URL and position among the document's images.
    image: Option<(String, usize)>,
}

/// An image the user asked to see full size, waiting for the app to open the
/// viewer (the document layer has no popup of its own to open).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingImage {
    pub owner: usize,
    pub ordinal: usize,
    pub index: usize,
    pub url: String,
}

/// The rows of `rect` that fall inside `first..limit`, as (low, high).
fn visible_rows(rect: Rect, first: usize, limit: usize) -> Option<(usize, usize)> {
    let low = usize::from(rect.y).max(first);
    let high = usize::from(rect.bottom()).min(limit);
    (low < high).then_some((low, high))
}

/// Disposable UI instances and this frame's hit areas. No filesystem writes.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // Independent input, layout and OS-focus flags.
pub struct Widgets {
    entries: HashMap<Key, Entry>,
    previews: HashMap<(usize, usize), nexus_core::interactive::StreamParser>,
    hits: Vec<Hit>,
    visible: Vec<Key>,
    focused: Option<Key>,
    screen: Option<Rect>,
    send_ready: bool,
    answer_blocked: Option<&'static str>,
    expanded: bool,
    pinned: bool,
    layout: Option<(Rect, usize, Vec<Placement>)>,
    /// Set by a click on an image (or `v` on a focused one); the app takes it.
    pending_image: Option<PendingImage>,
    pending_status: Option<String>,
    applied_patches: std::collections::HashSet<Key>,
    data_calls: HashMap<String, usize>,
    data_running: Option<live::Running>,
    window_unfocused: bool,
    /// A canvas drawn this frame is still animating.
    animating: bool,
    /// The last key or click the panels saw; timers idle after 30 minutes.
    last_input: Option<std::time::Instant>,
    /// Motion in panels (`/motion on|off`): glides, draw-in, press flash and
    /// canvas animation. Off by default so tests render final frames; the
    /// app turns it on from the `motion` setting.
    pub(crate) motion: bool,
    /// Saved-state bookkeeping and pinned apps; see `persist`.
    store: persist::Store,
    /// Vim keys in text fields and code editors (the app's `/vim` setting).
    pub(crate) vim: bool,
}
impl Widgets {
    /// Discard all local state and stale hit areas on a viewport/session reset.
    pub fn clear(&mut self) {
        let calls = std::mem::take(&mut self.data_calls);
        let data_running = self.data_running.take();
        let window_unfocused = self.window_unfocused;
        let store = std::mem::take(&mut self.store).retained();
        // Settings outlive a viewport reset: without this a session switch
        // silently turned vim keys and motion back to their defaults.
        let (vim, motion) = (self.vim, self.motion);
        *self = Self {
            data_calls: calls,
            data_running,
            window_unfocused,
            store,
            vim,
            motion,
            ..Self::default()
        };
    }
    /// Rebuild visible instances/hits and submission readiness for this frame.
    pub fn begin_frame(&mut self, send_ready: bool) {
        self.send_ready = send_ready;
        self.hits.clear();
        self.visible.clear();
        self.animating = false;
    }
    /// Timers run while the window has focus and someone used it recently;
    /// a panel left alone for half an hour stops ticking.
    fn timers_live(&self) -> bool {
        !self.window_unfocused
            && self
                .last_input
                .is_none_or(|at| at.elapsed() < std::time::Duration::from_mins(30))
    }
    /// Whether the last frame drew a moving canvas, so the event loop should
    /// keep redrawing. Off while the terminal window is unfocused.
    pub(crate) fn animating(&self) -> bool {
        self.animating && !self.window_unfocused
    }
    /// Whether a document currently owns the keyboard.
    pub(crate) fn is_focused(&self) -> bool {
        self.focused.is_some()
    }
    /// Whether a text field owns editing keys (including terminal key repeats).
    pub(crate) fn input_focused(&self) -> bool {
        self.focused
            .as_ref()
            .and_then(|key| self.entries.get(key))
            .is_some_and(|entry| entry.focused_input().is_some())
    }
    /// Whether the focused field is a multiline textarea, which keeps
    /// PageUp/PageDown for itself; other panels let them scroll the transcript.
    fn textarea_focused(&self) -> bool {
        self.focused
            .as_ref()
            .and_then(|key| self.entries.get(key))
            .is_some_and(|entry| {
                entry.focused_input().is_some_and(|node| {
                    entry.session.as_ref().is_ok_and(|session| {
                        session.document().node(node).is_some_and(Node::is_textarea)
                    })
                })
            })
    }
    /// Copy (or cut) the focused field's nonempty selection.
    fn take_selection(&mut self, cut: bool) -> Option<String> {
        let entry = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))?;
        let node = entry.focused_input()?;
        let Ok(session) = &mut entry.session else {
            return None;
        };
        match entry.inputs.get_mut().selection(session, node, cut) {
            Ok(text) => text,
            Err(error) => {
                entry.error = error.to_string();
                None
            }
        }
    }
    /// Return keyboard ownership without resetting document state.
    pub fn unfocus(&mut self) {
        self.focused = None;
        self.expanded = false;
        self.pinned = false;
    }
    /// Focus the last visible document, if any.
    pub fn focus_latest(&mut self) -> bool {
        self.focused = self.visible.last().cloned();
        if let Some(session) = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))
            .and_then(|entry| entry.session.as_mut().ok())
        {
            session.interact_data(std::time::Instant::now());
        }
        self.expanded = false;
        self.pinned = false;
        self.focused.is_some()
    }
    fn register(&mut self, key: &Key) {
        if self.entries.contains_key(key) {
            return;
        }
        let mut session = Document::parse(&key.source)
            .and_then(Session::new)
            .map_err(|error| error.to_string());
        // Layouts are inert, so they do not count against the interactive-panel limit.
        let is_static = session.as_ref().is_ok_and(|s| s.document().is_static());
        let count = self
            .entries
            .values()
            .filter(|entry| {
                entry
                    .session
                    .as_ref()
                    .is_ok_and(|s| s.document().is_static())
                    == is_static
            })
            .count();
        if count >= if is_static { 512 } else { 64 } {
            return;
        }
        if let Ok(session) = &mut session {
            self.restore_message_panel(key, session);
        }
        self.entries.insert(key.clone(), Entry::new(session));
    }
    fn reconcile_patches(&mut self, placements: &[Placement]) {
        self.index_occurrences(placements);
        let mut targets = HashMap::new();
        for placement in placements.iter().filter(|p| p.complete) {
            let key = placement.key();
            let Some((id, patch)) = nexus_core::interactive::edit_patch(&placement.source) else {
                targets.insert(
                    nexus_core::interactive::panel_id(placement.owner, placement.ordinal),
                    key,
                );
                continue;
            };
            if !self.applied_patches.insert(key.clone()) {
                continue;
            }
            let result = (|| {
                let target = targets
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("unknown panel {id}"))?;
                self.register(target);
                let entry = self
                    .entries
                    .get_mut(target)
                    .ok_or_else(|| anyhow::anyhow!("panel limit reached"))?;
                let session = entry
                    .session
                    .as_mut()
                    .map_err(|error| anyhow::anyhow!(error.clone()))?;
                session.patch(patch)?;
                Document::parse(&format!(
                    "root = Panel(\"Panel updated\", [Text({})])",
                    serde_json::to_string(&format!("Updated {id}"))?
                ))
                .and_then(Session::new)
            })()
            .map_err(|error: anyhow::Error| error.to_string());
            self.entries.insert(key, Entry::new(result));
        }
    }
    /// Keep transcript reservations in sync with locally edited, wrapping content.
    pub(crate) fn reflow(
        &mut self,
        cache: &mut crate::history_cache::HistoryCache,
        top: usize,
        height: u16,
    ) {
        self.reconcile_patches(&cache.widgets);
        for index in 0..cache.widgets.len() {
            let placement = &cache.widgets[index];
            if placement.complete
                && placement.row < top + usize::from(height)
                && placement.row + usize::from(placement.height) > top
            {
                self.register(&placement.key());
            }
            let Some(Ok(session)) = self
                .entries
                .get(&placement.key())
                .map(|entry| &entry.session)
            else {
                continue;
            };
            let height = card_height(&node_regions(session, placement.width));
            if height == placement.height {
                continue;
            }
            let start = placement.row;
            let old_height = placement.height;
            let old_end = start + usize::from(old_height);
            let count = usize::from(height);
            let owner = cache.owner[start];
            let code = cache.code[start];
            cache
                .lines
                .splice(start..old_end, vec![Line::default(); count]);
            cache
                .plain
                .splice(start..old_end, vec![String::new(); count]);
            cache.owner.splice(start..old_end, vec![owner; count]);
            cache.code.splice(start..old_end, vec![code; count]);
            cache
                .image_at_line
                .splice(start..old_end, vec![None; count]);
            for diagram in &mut cache.diagrams {
                if diagram.row >= old_end {
                    diagram.row = diagram.row - usize::from(old_height) + count;
                }
            }
            for widget in &mut cache.widgets[index + 1..] {
                widget.row = widget.row - usize::from(old_height) + count;
            }
            cache.widgets[index].height = height;
        }
    }
    /// Overlay visible regions and capture exact-frame, clipped control hits.
    #[allow(clippy::too_many_lines)]
    pub fn paint(
        &mut self,
        f: &mut Frame,
        viewport: Rect,
        top: usize,
        placements: &[Placement],
        theme: &Theme,
        active: bool,
    ) {
        self.reconcile_patches(placements);
        let timers = self.timers_live();
        self.screen = Some(f.area());
        self.layout = Some((viewport, top, placements.to_vec()));
        self.previews.retain(|&(owner, ordinal), _| {
            placements.iter().any(|p| {
                !p.complete
                    && p.owner == owner
                    && p.ordinal == ordinal
                    && p.row < top.saturating_add(usize::from(viewport.height))
                    && p.row.saturating_add(usize::from(p.height)) > top
            })
        });
        if self.focused.as_ref().is_some_and(|key| {
            !self.is_app(key) && !placements.iter().any(|p| p.complete && p.key() == *key)
        }) {
            self.unfocus();
        }
        if self.focused.as_ref().is_some_and(|key| {
            placements.iter().any(|p| {
                p.key() == *key
                    && (p.row + usize::from(p.height) <= top
                        || p.row >= top + usize::from(viewport.height))
            })
        }) {
            self.pinned = true;
        }
        let mut ordered: Vec<_> = placements.iter().collect();
        ordered.sort_by_key(|p| self.pinned && self.focused.as_ref() == Some(&p.key()));
        for placement in ordered {
            let key = placement.key();
            let pinned = self.pinned && self.focused.as_ref() == Some(&key);
            let row = if pinned { top } else { placement.row };
            let first = top.saturating_sub(row).min(usize::from(placement.height));
            let end = (top + usize::from(viewport.height))
                .saturating_sub(row)
                .min(usize::from(placement.height));
            let width = placement.width.min(viewport.width.saturating_sub(2));
            if first >= end || width == 0 {
                continue;
            }
            let focused = self.focused.as_ref() == Some(&key);
            let (mut canvas, hits) = if placement.complete {
                self.register(&key);
                let Some(entry) = self.entries.get_mut(&key) else {
                    let y = viewport.y + u16::try_from(row.saturating_sub(top)).unwrap_or(0);
                    f.render_widget(
                        Paragraph::new("UI instance limit reached; start a new session.")
                            .style(Style::default().fg(theme.error)),
                        Rect::new(viewport.x + 2, y, width, 1),
                    );
                    continue;
                };
                if entry.is_interactive() {
                    self.visible.push(key.clone());
                }
                entry.inputs.borrow_mut().vim = self.vim;
                self.animating |= entry.advance_clock(self.motion, timers);
                entry.canvas(width, theme, focused, self.send_ready)
            } else {
                let document = if placement.source.trim_start().starts_with('{') {
                    Document::preview(&placement.source)
                } else {
                    self.previews
                        .entry((placement.owner, placement.ordinal))
                        .or_default()
                        .set(&placement.source)
                };
                (streaming_buffer(document, width, theme), Vec::new())
            };
            let end = end.min(usize::from(canvas.area.height));
            if pinned {
                let overlay = Rect::new(
                    viewport.x + 2,
                    viewport.y,
                    width,
                    u16::try_from(end).unwrap_or(0),
                );
                self.hits
                    .retain(|hit| hit.rect.intersection(overlay).is_empty());
                let bottom = u16::try_from(end).unwrap_or(0).saturating_sub(1);
                let border = card_buffer(width, bottom + 1, "", theme, "▴ pinned · Esc to unpin");
                for x in 0..width {
                    canvas[(x, bottom)] = border[(x, bottom)].clone();
                }
            }
            if first >= end {
                continue;
            }
            for offset in first..end {
                let y = viewport.y + u16::try_from(row + offset - top).unwrap_or(0);
                for x in 0..width {
                    f.buffer_mut()[(viewport.x + 2 + x, y)] =
                        canvas[(x, u16::try_from(offset).unwrap_or(0))].clone();
                }
            }
            if active
                && focused
                && let Some(caret) = self
                    .entries
                    .get(&key)
                    .and_then(|entry| entry.input_caret(&canvas))
                && usize::from(caret.y) >= first
                && usize::from(caret.y) < end.saturating_sub(usize::from(pinned))
            {
                place_caret(
                    f,
                    Position::new(
                        viewport.x + 2 + caret.x,
                        viewport.y + u16::try_from(row + usize::from(caret.y) - top).unwrap_or(0),
                    ),
                );
            }
            if active && placement.complete {
                let limit = end.saturating_sub(usize::from(pinned));
                let screen = |rect: Rect, low: usize, high: usize| {
                    Rect::new(
                        viewport.x + 2 + rect.x,
                        viewport.y + u16::try_from(row + low - top).unwrap_or(0),
                        rect.width,
                        u16::try_from(high - low).unwrap_or(0),
                    )
                };
                for (rect, control) in hits {
                    if let Some((low, high)) = visible_rows(rect, first, limit) {
                        self.hits.push(Hit {
                            rect: screen(rect, low, high),
                            key: key.clone(),
                            control: Some(control),
                            row_offset: u16::try_from(low).unwrap_or(0).saturating_sub(rect.y),
                            image: None,
                        });
                    }
                }
                // After the controls: an image button's corner tag must win
                // over the button it sits on.
                for target in self
                    .entries
                    .get(&key)
                    .into_iter()
                    .flat_map(|entry| entry.image_targets(width))
                {
                    if let Some((low, high)) = visible_rows(target.rect, first, limit) {
                        self.hits.push(Hit {
                            rect: screen(target.rect, low, high),
                            key: key.clone(),
                            control: None,
                            row_offset: 0,
                            image: Some((target.url, target.index)),
                        });
                    }
                }
            }
        }
    }
    fn click(&mut self, mouse: MouseEvent, screen: Rect, can_send: bool) -> (bool, Option<String>) {
        self.last_input = Some(std::time::Instant::now());
        let result = self.handle_click(mouse, screen, can_send);
        self.open_trusted();
        result
    }
    #[allow(clippy::too_many_lines)]
    fn handle_click(
        &mut self,
        mouse: MouseEvent,
        screen: Rect,
        can_send: bool,
    ) -> (bool, Option<String>) {
        let dragging = mouse.kind == MouseEventKind::Drag(MouseButton::Left);
        if (!dragging && mouse.kind != MouseEventKind::Down(MouseButton::Left))
            || self.screen != Some(screen)
        {
            return (false, None);
        }
        let pos = Position::new(mouse.column, mouse.row);
        // A drag keeps selecting in the focused field even after the pointer
        // leaves it; the field clamps the position to its edge.
        let hit = if dragging {
            self.hits
                .iter()
                .rev()
                .find(|hit| {
                    self.focused.as_ref() == Some(&hit.key)
                        && self.entries.get(&hit.key).is_some_and(|entry| {
                            hit.control == Some(entry.focus) && entry.focused_input().is_some()
                        })
                })
                .cloned()
        } else {
            self.hits
                .iter()
                .rev()
                .find(|hit| hit.rect.contains(pos))
                .cloned()
        };
        let Some(hit) = hit else {
            return (false, None);
        };
        if let Some((url, index)) = hit.image {
            // Viewing an image neither focuses the document nor presses
            // anything: it only asks the app for the viewer.
            self.pending_image = Some(PendingImage {
                owner: hit.key.owner,
                ordinal: hit.key.ordinal,
                index,
                url,
            });
            return (true, None);
        }
        let Some(entry) = self.entries.get_mut(&hit.key) else {
            return (false, None);
        };
        if self.focused.as_ref() != Some(&hit.key) {
            self.pinned = false;
        }
        self.focused = Some(hit.key.clone());
        if let Ok(session) = &mut entry.session {
            session.interact_data(std::time::Instant::now());
        }
        match hit.control.and_then(|index| {
            entry
                .controls()
                .get(index)
                .cloned()
                .map(|control| (index, control))
        }) {
            Some((index, Control::Action(_) | Control::Load | Control::Shape(..))) => (
                true,
                entry.activate(index, can_send, self.answer_blocked.is_none()),
            ),
            Some((index, Control::Input(node))) => {
                entry.focus = index;
                if let Ok(session) = &entry.session {
                    entry.inputs.get_mut().click(
                        session,
                        node,
                        Position::new(
                            mouse.column.saturating_sub(hit.rect.x),
                            mouse.row.saturating_sub(hit.rect.y) + hit.row_offset,
                        ),
                        hit.rect.width,
                        dragging || mouse.modifiers.contains(KeyModifiers::SHIFT),
                    );
                }
                self.focused = Some(hit.key);
                (true, None)
            }
            Some((index, Control::Stepper(node))) => {
                entry.focus = index;
                if let Ok(session) = &entry.session
                    && let Some(arrows) = stepper_arrows(
                        session.document().node(node).expect("control node"),
                        session,
                        hit.rect,
                    )
                    && let Some(direction) = arrows.iter().position(|rect| rect.contains(pos))
                {
                    entry.navigate(if direction == 0 {
                        KeyCode::Left
                    } else {
                        KeyCode::Right
                    });
                }
                (true, None)
            }
            Some((index, Control::Table(node, _))) => {
                entry.focus = index;
                self.focused = Some(hit.key);
                if let Ok(session) = &mut entry.session {
                    let row = hit.row_offset + mouse.row - hit.rect.y;
                    let result = components::click_table(
                        session,
                        node,
                        row,
                        mouse.column - hit.rect.x,
                        hit.rect,
                    );
                    entry.error = result
                        .err()
                        .map_or_else(String::new, |error| error.to_string());
                }
                (true, None)
            }
            Some((index, control @ (Control::Editor(_) | Control::Quiz(_)))) => {
                entry.focus = index;
                self.focused = Some(hit.key);
                if !practice::click(entry, &control, pos, hit.rect, hit.row_offset) {
                    return (
                        true,
                        entry.activate(index, can_send, self.answer_blocked.is_none()),
                    );
                }
                (true, None)
            }
            Some((
                index,
                control @ (Control::Tabs(_)
                | Control::Section(..)
                | Control::Form(_)
                | Control::Rating(_)
                | Control::Slider(_)
                | Control::Copy(_)),
            )) => {
                entry.focus = index;
                if components::click(entry, &control, pos, hit.rect) {
                    (true, None)
                } else {
                    (
                        true,
                        entry.activate(index, can_send, self.answer_blocked.is_none()),
                    )
                }
            }
            Some((
                index,
                Control::Toggle(_) | Control::Select(_) | Control::Choice(..) | Control::Reveal(_),
            )) => {
                self.focused = Some(hit.key);
                (
                    true,
                    entry.activate(index, can_send, self.answer_blocked.is_none()),
                )
            }
            None => {
                entry.focus = entry.controls().len();
                self.expanded = true;
                self.focused = Some(hit.key);
                (true, None)
            }
        }
    }
    /// Paste into the focused input only; never into the underlying composer.
    pub fn paste(&mut self, text: &str) {
        if let Some(entry) = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))
        {
            if let Ok(session) = &mut entry.session {
                session.interact_data(std::time::Instant::now());
            }
            entry.edit(text, false);
        }
    }
    /// Offer `key` to the focused text field; whether it consumed it.
    fn field_takes(&mut self, key: KeyEvent) -> bool {
        let vim = self.vim;
        let Some(entry) = self.focused.as_ref().and_then(|k| self.entries.get_mut(k)) else {
            return false;
        };
        let Some(node) = entry.focused_input() else {
            return false;
        };
        let Ok(session) = &mut entry.session else {
            return false;
        };
        let inputs = entry.inputs.get_mut();
        inputs.vim = vim;
        match inputs.key(session, node, key) {
            Ok(consumed) => consumed,
            Err(error) => {
                entry.error = error.to_string();
                true
            }
        }
    }
    fn key(&mut self, key: KeyEvent, can_send: bool) -> (bool, Option<String>) {
        self.last_input = Some(std::time::Instant::now());
        let result = self.handle_key(key, can_send);
        self.open_trusted();
        result
    }
    #[allow(clippy::too_many_lines)]
    fn handle_key(&mut self, key: KeyEvent, can_send: bool) -> (bool, Option<String>) {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return (false, None);
        }
        if self.pending_url_focused() {
            if key.kind == KeyEventKind::Press
                && plain(&key)
                && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('a'))
            {
                self.resolve_url(key.code);
            }
            return (false, None);
        }
        let vim = self.vim;
        if let Some(entry) = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))
            && (plain(&key) || vim)
            && let Some(node) = practice::editing(entry)
            && practice::editor_key(entry, node, key, vim)
        {
            return (false, None);
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::F(2)) && plain(&key) {
            // With vim keys, Esc first leaves Insert/Visual in a text field.
            if key.code == KeyCode::Esc && self.vim && self.field_takes(key) {
                return (false, None);
            }
            self.unfocus();
            return (true, None);
        }
        let Some(entry) = self
            .focused
            .as_ref()
            .and_then(|key| self.entries.get_mut(key))
        else {
            return (true, None);
        };
        if let Ok(session) = &mut entry.session {
            session.interact_data(std::time::Instant::now());
        }
        let controls = entry.controls();
        entry.inputs.get_mut().vim = self.vim;
        if let Some(node) = entry.focused_input()
            && let Ok(session) = &mut entry.session
        {
            match entry.inputs.get_mut().key(session, node, key) {
                Ok(true) => {
                    entry.error.clear();
                    return (false, None);
                }
                Err(error) => {
                    entry.error = error.to_string();
                    return (false, None);
                }
                Ok(false) => {}
            }
        }
        if !plain(&key) {
            return (false, None);
        }
        let count = controls.len() + 1;
        entry.focus %= count;
        if entry.focus == controls.len() && matches!(key.code, KeyCode::Enter | KeyCode::Char(' '))
        {
            self.expanded = true;
            return (false, None);
        }
        let searching = matches!(controls.get(entry.focus), Some(Control::Table(node, _)) if entry.session.as_ref().is_ok_and(|session| session.table_state(*node).searching));
        // The panel's own key bindings win over shortcuts and navigation,
        // except while typing (text fields, code, table search).
        if !searching
            && !matches!(
                controls.get(entry.focus),
                Some(Control::Input(_) | Control::Editor(_))
            )
            && let Some(name) = binding_name(key.code)
            && let Ok(session) = &mut entry.session
            && session.document().key_action(&name).is_some()
        {
            match session.press_key(&name) {
                Ok(_) => entry.error.clear(),
                Err(error) => entry.error = error.to_string(),
            }
            return (false, None);
        }
        if key.code == KeyCode::Char('p')
            && !searching
            && !matches!(controls.get(entry.focus), Some(Control::Input(_)))
        {
            self.store.pending_pin = true;
            return (false, None);
        }
        if let KeyCode::Char(ch @ '1'..='9') = key.code
            && !searching
            && let Some(control) = entry.choices().get(ch as usize - '1' as usize)
        {
            return (
                false,
                entry.activate(*control, can_send, self.answer_blocked.is_none()),
            );
        }
        if let Some(Control::Action((node, item))) = controls.get(entry.focus)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
            && let Ok(session) = &entry.session
            && let Some(Node::Buttons { items, .. }) = session.document().node(*node)
            && choice_list(session.document(), items)
        {
            let next = if key.code == KeyCode::Down {
                (item + 1) % items.len()
            } else {
                (item + items.len() - 1) % items.len()
            };
            entry.focus = entry.focus - item + next;
            return (false, None);
        }
        if entry.navigate(key.code) {
            return (false, None);
        }
        if key.code == KeyCode::Char('v')
            && !matches!(controls.get(entry.focus), Some(Control::Input(_)))
            && let Some((index, url)) = entry.focused_image()
            && let Some(focused) = &self.focused
        {
            self.pending_image = Some(PendingImage {
                owner: focused.owner,
                ordinal: focused.ordinal,
                index,
                url,
            });
            return (false, None);
        }
        match key.code {
            KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                entry.focus = (entry.focus + 1) % count;
            }
            KeyCode::BackTab | KeyCode::Tab | KeyCode::Up | KeyCode::Left => {
                entry.focus = (entry.focus + count - 1) % count;
            }
            KeyCode::Down | KeyCode::Right => entry.focus = (entry.focus + 1) % count,
            KeyCode::Enter => {
                if matches!(controls.get(entry.focus), Some(Control::Input(_))) {
                    entry.focus = (entry.focus + 1) % count;
                } else {
                    return (
                        false,
                        entry.activate(entry.focus, can_send, self.answer_blocked.is_none()),
                    );
                }
            }
            KeyCode::Char(ch) if matches!(controls.get(entry.focus), Some(Control::Input(_))) => {
                entry.edit(&ch.to_string(), false);
            }
            KeyCode::Backspace => entry.edit("", true),
            KeyCode::Char(' ') => {
                return (
                    false,
                    entry.activate(entry.focus, can_send, self.answer_blocked.is_none()),
                );
            }
            _ => {}
        }
        (false, None)
    }
    fn popup(&mut self, f: &mut Frame, app_theme: &Theme, area: Rect, can_send: bool) {
        self.hits.clear();
        self.screen = Some(f.area());
        let Some(key) = self.focused.clone() else {
            return;
        };
        let timers = self.timers_live();
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        entry.inputs.get_mut().vim = self.vim;
        self.animating |= entry.advance_clock(self.motion, timers);
        let (canvas, hits) = entry.canvas(area.width, app_theme, true, can_send);
        let caret = entry.input_caret(&canvas);
        let target = caret
            .map(|caret| Rect::new(caret.x, caret.y, 1, 1))
            .or_else(|| {
                hits.iter()
                    .find(|(_, control)| *control == entry.focus)
                    .map(|(rect, _)| *rect)
            });
        if let Some(rect) = target {
            if rect.bottom() > entry.scroll + area.height {
                entry.scroll = rect.bottom().saturating_sub(area.height);
            }
            if rect.y < entry.scroll {
                entry.scroll = rect.y;
            }
        }
        let height = canvas.area.height;
        entry.scroll = entry.scroll.min(height.saturating_sub(area.height));
        f.render_widget(Clear, area);
        f.buffer_mut().set_style(area, app_theme.background_style());
        for y in 0..area.height.min(height) {
            for x in 0..area.width {
                f.buffer_mut()[(area.x + x, area.y + y)] = canvas[(x, y + entry.scroll)].clone();
            }
        }
        if let Some(caret) = caret
            && caret.y >= entry.scroll
            && caret.y < entry.scroll + area.height
        {
            place_caret(
                f,
                Position::new(area.x + caret.x, area.y + caret.y - entry.scroll),
            );
        }
        let scroll = entry.scroll;
        let shown = |rect: Rect| {
            let low = rect.y.max(scroll);
            let high = rect.bottom().min(scroll + area.height);
            (low < high).then_some((low, high))
        };
        let placed = |rect: Rect, low: u16, high: u16| {
            Rect::new(
                area.x + rect.x,
                area.y + low - scroll,
                rect.width,
                high - low,
            )
        };
        for (rect, control) in hits {
            if let Some((low, high)) = shown(rect) {
                self.hits.push(Hit {
                    rect: placed(rect, low, high),
                    key: key.clone(),
                    control: Some(control),
                    row_offset: low.saturating_sub(rect.y),
                    image: None,
                });
            }
        }
        for target in entry.image_targets(area.width) {
            if let Some((low, high)) = shown(target.rect) {
                self.hits.push(Hit {
                    rect: placed(target.rect, low, high),
                    key: key.clone(),
                    control: None,
                    row_offset: 0,
                    image: Some((target.url, target.index)),
                });
            }
        }
    }
}
pub(crate) fn safe(text: &str) -> String {
    text.chars()
        .filter(|ch| *ch != '\u{FE0F}')
        .map(|ch| {
            if ch.is_control() && ch != '\n' {
                ch.escape_default().to_string()
            } else {
                ch.to_string()
            }
        })
        .collect()
}
/// Why an answer button can't send right now: a reply is streaming or a
/// `!` command's output is still on its way into the conversation.
fn answer_block(app: &AppView) -> Option<&'static str> {
    if app.viewing_stream() {
        Some("wait for the reply to finish, then pick again")
    } else if app.user_command_running() {
        Some("wait for the command to finish, then pick again")
    } else {
        None
    }
}
/// A proposed response must not race a reply or overwrite the user's draft.
#[must_use]
pub(crate) fn can_send(app: &AppView) -> bool {
    !app.viewing_stream() && app.input_text().is_empty()
}
fn review(app: &mut AppView, message: Option<String>) {
    if let Some(text) = message {
        // Never promote model-authored text into a user instruction unseen,
        // overwrite a draft, or route it through the slash-command parser.
        if !can_send(app) {
            return;
        }
        app.set_input(&text);
        app.popup = Popup::None;
        app.interactive.unfocus();
        app.push_status(
            "UI response in the composer for review — edit/discard it, or press Enter to send",
        );
    }
}
fn respond(app: &mut AppView, message: Option<String>) {
    if let Some(source) = app
        .interactive
        .focused
        .as_ref()
        .and_then(|key| app.interactive.entries.get_mut(key))
        .and_then(|entry| entry.session.as_mut().ok())
        .and_then(Session::take_copy)
    {
        let status = crate::composer::copy_to_clipboard(&mut app.clipboard, &source);
        app.push_status(status);
    }
    let entry = app
        .interactive
        .focused
        .as_ref()
        .and_then(|key| app.interactive.entries.get(key));
    if !entry.is_some_and(Entry::answering) {
        review(app, message);
        return;
    }
    if let Some(reason) = app.interactive.answer_blocked {
        app.push_status(reason);
        return;
    }
    let Some(text) = message else {
        return;
    };
    // Avoid the composer's send-failure restore events: answers never own a draft.
    if app.chat_task_count() >= MAX_CHAT_TASKS {
        app.push_status(format!("chat task limit reached ({MAX_CHAT_TASKS})"));
        return;
    }
    if !app.backends.any() {
        app.push_status("set your API key first with /login");
        return;
    }
    if app.current_model.is_none() {
        app.push_status("pick a model first with /model");
        return;
    }
    app.popup = Popup::None;
    app.interactive.unfocus();
    if let Err(error) = app.execute(AppCommand::Send { text: safe(&text) }) {
        app.push_status(format!("UI answer failed: {error}"));
    }
}
/// Focus the newest visible model-authored output, without a slash command.
pub fn focus(app: &mut AppView) {
    if app.interactive.focus_latest() {
        app.popup = Popup::InteractiveUi;
        app.sel.clear();
        app.push_status("UI send actions prepare a message for review; answer buttons send now");
    } else {
        app.push_status("no interactive output in view");
    }
}
/// Route inline/popup controls before transcript selection or composer input.
pub fn mouse(app: &mut AppView, event: MouseEvent, screen: Rect) -> bool {
    if app.popup == Popup::InteractiveUi {
        match event.kind {
            MouseEventKind::ScrollUp => {
                app.scroll = app.scroll.saturating_add(3).min(app.max_scroll);
                return true;
            }
            MouseEventKind::ScrollDown => {
                app.scroll = app.scroll.saturating_sub(3);
                return true;
            }
            MouseEventKind::Down(MouseButton::Left)
                if app
                    .input_inner
                    .contains(Position::new(event.column, event.row)) =>
            {
                app.interactive.unfocus();
                app.popup = Popup::None;
                return true;
            }
            _ => {}
        }
    }
    app.interactive.answer_blocked = answer_block(app);
    let (handled, message) = app.interactive.click(event, screen, can_send(app));
    if app.interactive.focused.is_some() {
        if handled && app.popup != Popup::InteractiveUi {
            app.push_status(
                "UI send actions prepare a message for review; answer buttons send now",
            );
        }
        app.popup = Popup::InteractiveUi;
    }
    if handled {
        app.sel.clear();
    }
    respond(app, message);
    open_pending_image(app);
    handled
}
/// Hand an image the user picked to the viewer, remembering whether to come
/// back to the focused document when it closes.
fn open_pending_image(app: &mut AppView) {
    if let Some(pending) = app.interactive.pending_image.take() {
        let back = if app.interactive.focused.is_some() {
            crate::image_viewer::Return::Interactive
        } else {
            crate::image_viewer::Return::Nothing
        };
        app.open_widget_image(&pending, back);
    }
}
/// Copy/cut a nonempty field selection to the clipboard. Returns false when
/// nothing is selected, so Ctrl+C can still back out of the panel.
pub(crate) fn copy_selection(app: &mut AppView, cut: bool) -> bool {
    let Some(text) = app.interactive.take_selection(cut) else {
        return false;
    };
    let status = crate::composer::copy_to_clipboard(&mut app.clipboard, &text);
    if !status.is_empty() {
        app.push_status(status);
    }
    true
}

/// Focused keyboard events stay inside the selected document.
pub fn key(app: &mut AppView, event: KeyEvent) {
    if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return;
    }
    app.interactive.vim = app.vim_enabled;
    if event
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        && matches!(event.code, KeyCode::Char('c' | 'C' | 'x' | 'X'))
        && copy_selection(app, matches!(event.code, KeyCode::Char('x' | 'X')))
    {
        return;
    }
    if !app.interactive.input_focused() && !plain(&event) {
        return;
    }
    let textarea_focused = app.interactive.textarea_focused();
    let table_focused = app
        .interactive
        .focused
        .as_ref()
        .and_then(|key| app.interactive.entries.get(key))
        .is_some_and(|entry| matches!(entry.controls().get(entry.focus), Some(Control::Table(node, _)) if entry.session.as_ref().is_ok_and(|session| matches!(session.document().node(*node), Some(Node::Table { page_size: Some(_), .. })))));
    match event.code {
        KeyCode::PageUp if !table_focused && !textarea_focused => {
            app.scroll = app
                .scroll
                .saturating_add(app.history_page())
                .min(app.max_scroll);
            return;
        }
        KeyCode::PageDown if !table_focused && !textarea_focused => {
            app.scroll = app.scroll.saturating_sub(app.history_page());
            return;
        }
        _ => {}
    }
    app.interactive.answer_blocked = answer_block(app);
    let (closed, message) = app.interactive.key(event, can_send(app));
    if closed {
        app.popup = Popup::None;
    }
    if app.interactive.take_pin_request() {
        pin_focused(app, None);
    }
    respond(app, message);
    if let Some(status) = app.interactive.pending_status.take() {
        app.push_status(status);
    }
    open_pending_image(app);
}
/// Render a keyboard-accessible enlarged view of the same inline state.
pub fn render_popup(f: &mut Frame, app: &mut AppView) {
    let Some((viewport, top, placements)) = app.interactive.layout.clone().or_else(|| {
        // A pinned app owns the whole view and needs no transcript layout.
        app.interactive
            .focused_app()
            .then(|| (f.area(), 0, Vec::new()))
    }) else {
        return;
    };
    if app.interactive.expanded {
        let enabled = can_send(app);
        app.interactive.popup(f, &app.theme, viewport, enabled);
    } else {
        // The popup dispatcher dims the base view. Repaint just this card at
        // full strength while retaining inline keyboard ownership.
        app.interactive.hits.clear();
        app.interactive
            .paint(f, viewport, top, &placements, &app.theme, true);
    }
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
        let fetcher = nexus_core::remote_image::Fetcher::new(
            std::env::temp_dir().join(format!("nexus-failure-{}", uuid::Uuid::new_v4())),
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
#[path = "interactive/motion_tests.rs"]
mod motion_tests;

#[cfg(test)]
#[path = "interactive/play_tests.rs"]
mod play_tests;
