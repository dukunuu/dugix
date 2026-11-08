//! Native text fields. The existing composer editor owns caret/selection/history;
//! the session remains authoritative and rejected edits restore the whole editor.

use super::{components, safe};
use crate::theme::Theme;
use crate::vim::{Outcome, Vim};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::interactive::{Node, Session};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Modifier, Style},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Widget},
};
use std::collections::HashMap;
use tui_textarea::{CursorMove, TextArea, WrapMode};

#[derive(Default)]
pub(super) struct Inputs {
    fields: HashMap<String, TextArea<'static>>,
    /// Vim keys on (mirrors the app's `/vim` setting before keys and draws).
    pub(super) vim: bool,
    /// Each field's vim mode and pending command.
    vims: HashMap<String, Vim>,
    /// Vim state of each code `Editor`, by node.
    code_vims: HashMap<usize, Vim>,
    /// Text body of the focused field as last drawn on the panel canvas, so
    /// the terminal caret is looked up only inside that field.
    body: Option<Rect>,
}

/// Soft-wrap prose in a textarea; a single-line input scrolls horizontally.
fn wrap_mode(field: &Node) -> WrapMode {
    if field.is_textarea() {
        WrapMode::WordOrGlyph
    } else {
        WrapMode::None
    }
}

fn editor(value: &str, field: &Node) -> TextArea<'static> {
    let mut editor = TextArea::new(value.split('\n').map(str::to_owned).collect());
    editor.move_cursor(CursorMove::Bottom);
    editor.move_cursor(CursorMove::End);
    editor.set_max_histories(64);
    editor.set_wrap_mode(wrap_mode(field));
    editor
}

/// Keep pasted text within the field: drop control characters, keep newlines
/// in a textarea, and fold them to spaces in a single-line input.
fn sanitize_paste(text: &str, field: &Node) -> String {
    let multiline = field.is_textarea();
    let limit = field.input_limit().unwrap_or_default();
    let mut clean = String::new();
    for ch in text.replace("\r\n", "\n").chars() {
        let ch = match ch {
            '\n' if !multiline => ' ',
            '\n' => '\n',
            ch if ch.is_control() => continue,
            ch => ch,
        };
        // One byte over the limit still trips the session's rejection path.
        if clean.len() + ch.len_utf8() > limit + 1 {
            break;
        }
        clean.push(ch);
    }
    clean
}

impl Inputs {
    /// The editor for `node`, rebuilt when the session's value changed outside
    /// it (patches, reset buttons, live data); editors of removed fields are
    /// dropped on the way.
    fn sync_editor(&mut self, session: &Session, node: usize) -> Option<&mut TextArea<'static>> {
        let input @ Node::Input { key, .. } = session.document().node(node)? else {
            return None;
        };
        // Patches, reset buttons and live data may replace the value externally.
        let live = |key: &String| {
            session
                .document()
                .all_nodes()
                .iter()
                .any(|node| matches!(node, Node::Input { key: field, .. } if field == key))
        };
        self.fields.retain(|key, _| live(key));
        self.vims.retain(|key, _| live(key));
        let value = session.value(key);
        let field = self
            .fields
            .entry(key.clone())
            .or_insert_with(|| editor(&value, input));
        if field.lines().join("\n") != value {
            *field = editor(&value, input);
        }
        field.set_wrap_mode(wrap_mode(input));
        Some(field)
    }

    fn change(
        &mut self,
        session: &mut Session,
        node: usize,
        change: impl FnOnce(&mut TextArea<'static>),
    ) -> anyhow::Result<()> {
        let Some(Node::Input { key, .. }) = session.document().node(node) else {
            return Ok(());
        };
        let key = key.clone();
        let Some(field) = self.sync_editor(session, node) else {
            return Ok(());
        };
        let previous = field.clone();
        change(field);
        let value = field.lines().join("\n");
        if value != session.value(&key)
            && let Err(error) = session.input(&key, value)
        {
            *field = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Vim first: `Some(consumed)` when vim decided, `None` to fall through
    /// to the plain field keys (Insert-mode typing).
    fn vim_key(
        &mut self,
        session: &mut Session,
        node: usize,
        key: KeyEvent,
    ) -> anyhow::Result<Option<bool>> {
        let Some(Node::Input { key: name, .. }) = session.document().node(node) else {
            return Ok(Some(false));
        };
        let name = name.clone();
        let mut vim = self.vims.remove(&name).unwrap_or_default();
        let mut outcome = Outcome::Passthrough;
        let result = self.change(session, node, |field| outcome = vim.key(field, key));
        self.vims.insert(name, vim);
        result?;
        Ok(match outcome {
            Outcome::Handled => Some(true),
            Outcome::Submit | Outcome::Leave => Some(false),
            Outcome::Passthrough => None,
        })
    }

    /// Apply an editing key; `Ok(true)` means the field consumed it, and
    /// `Ok(false)` leaves it to the panel (Tab, Esc, Enter in a single-line
    /// input, PageUp/PageDown outside a textarea).
    pub(super) fn key(
        &mut self,
        session: &mut Session,
        node: usize,
        mut key: KeyEvent,
    ) -> anyhow::Result<bool> {
        let Some(field) = session.document().node(node) else {
            return Ok(false);
        };
        if !matches!(field, Node::Input { .. }) {
            return Ok(false);
        }
        let textarea = field.is_textarea();
        if self.vim
            && let Some(consumed) = self.vim_key(session, node, key)?
        {
            return Ok(consumed);
        }
        if matches!(
            key.code,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Esc | KeyCode::F(_)
        ) || (!textarea
            && matches!(
                key.code,
                KeyCode::Enter | KeyCode::PageUp | KeyCode::PageDown
            ))
        {
            return Ok(false);
        }
        // Do not let panel shortcuts (digits, p, v, space) steal typed text.
        let command = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        if key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
        {
            key.modifiers.remove(KeyModifiers::ALT);
            key.modifiers.insert(KeyModifiers::CONTROL);
        }
        self.change(session, node, |field| match key.code {
            KeyCode::Char('a' | 'A') if command => field.select_all(),
            KeyCode::Char('z' | 'Z') if command => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    field.redo();
                } else {
                    field.undo();
                }
            }
            KeyCode::Char('y' | 'Y') if command => {
                field.redo();
            }
            KeyCode::Backspace if command => {
                field.delete_word();
            }
            KeyCode::Delete if command => {
                field.delete_next_word();
            }
            KeyCode::Home | KeyCode::End if command => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    if !field.is_selecting() {
                        field.start_selection();
                    }
                } else {
                    field.cancel_selection();
                }
                let end = key.code == KeyCode::End;
                field.move_cursor(if end {
                    CursorMove::Bottom
                } else {
                    CursorMove::Top
                });
                field.move_cursor(if end {
                    CursorMove::End
                } else {
                    CursorMove::Head
                });
            }
            KeyCode::Char(ch)
                if !command && !key.modifiers.contains(KeyModifiers::ALT) && !ch.is_control() =>
            {
                field.input(key);
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Enter => {
                field.input(key);
            }
            _ => {}
        })?;
        Ok(true)
    }

    pub(super) fn selection(
        &mut self,
        session: &mut Session,
        node: usize,
        cut: bool,
    ) -> anyhow::Result<Option<String>> {
        let Some(field) = self.sync_editor(session, node) else {
            return Ok(None);
        };
        if field
            .selection_range()
            .is_none_or(|(start, end)| start == end)
        {
            return Ok(None);
        }
        let mut copy = field.clone();
        copy.copy();
        let text = copy.yank_text();
        if cut {
            self.change(session, node, |field| {
                field.cut();
            })?;
        }
        Ok(Some(text))
    }

    pub(super) fn paste(
        &mut self,
        session: &mut Session,
        node: usize,
        text: &str,
    ) -> anyhow::Result<()> {
        let Some(field) = session.document().node(node) else {
            return Ok(());
        };
        let text = sanitize_paste(text, field);
        if text.is_empty() {
            return Ok(());
        }
        self.change(session, node, |field| {
            field.insert_str(text);
        })
    }

    /// Overlay the focused field with its live editor (caret and selection).
    pub(super) fn draw_focused(
        &mut self,
        session: &Session,
        node: usize,
        width: u16,
        theme: &Theme,
        buffer: &mut Buffer,
    ) {
        self.body = None;
        let Some((input, area)) = components::visible_regions(session, width)
            .into_iter()
            .find(|(input, _)| session.document().node_index(input) == node)
        else {
            return;
        };
        let name = match input {
            Node::Input { key, .. } => key.clone(),
            _ => String::new(),
        };
        let vim = self
            .vim
            .then(|| self.vims.remove(&name).unwrap_or_default());
        if let Some(field) = self.sync_editor(session, node) {
            self.body = Some(paint(
                field,
                FieldView {
                    node: input,
                    session,
                    area,
                    theme,
                    selected: true,
                    vim: vim.as_ref(),
                },
                buffer,
            ));
        }
        if let Some(vim) = vim {
            self.vims.insert(name, vim);
        }
    }

    /// The vim state for code editor `node`.
    pub(super) fn code_vim(&mut self, node: usize) -> &mut Vim {
        self.code_vims.entry(node).or_default()
    }

    /// A code editor's vim mode label, when vim keys are on.
    pub(super) fn code_mode_label(&self, node: usize) -> Option<&'static str> {
        self.vim
            .then(|| self.code_vims.get(&node).map_or("INSERT", Vim::label))
    }

    /// The focused field's vim mode label, when vim keys are on.
    pub(super) fn mode_label(&self, session: &Session, node: usize) -> Option<&'static str> {
        let Some(Node::Input { key, .. }) = session.document().node(node) else {
            return None;
        };
        self.vim
            .then(|| self.vims.get(key).map_or("INSERT", Vim::label))
    }

    /// The terminal caret inside the focused field's last drawn body.
    pub(super) fn caret(&self, buffer: &Buffer) -> Option<Position> {
        caret(buffer, self.body?.intersection(buffer.area))
    }

    /// Hit-test through the editor's own rendered caret. Its soft-wrap map is
    /// private; bounded visual moves avoid a second, divergent Unicode/wrap map.
    pub(super) fn click(
        &mut self,
        session: &Session,
        node: usize,
        position: Position,
        width: u16,
        extend: bool,
    ) {
        let Some(input @ Node::Input { .. }) = session.document().node(node) else {
            return;
        };
        let area = Rect::new(0, 0, width, input.rows());
        let Some(field) = self.sync_editor(session, node) else {
            return;
        };
        let mut buffer = Buffer::empty(area);
        let body = paint(
            field,
            FieldView {
                node: input,
                session,
                area,
                theme: &Theme::default(),
                selected: true,
                vim: None,
            },
            &mut buffer,
        );
        // A drag that leaves the text keeps extending to the nearest edge.
        let position = if extend {
            Position::new(
                position
                    .x
                    .clamp(body.left(), body.right().saturating_sub(1)),
                position
                    .y
                    .clamp(body.top(), body.bottom().saturating_sub(1)),
            )
        } else {
            position
        };
        if !body.contains(position) {
            return;
        }
        let Some(mut caret) = caret(&buffer, body) else {
            return;
        };
        // Probe on one clone, not a history-buffer clone per display cell.
        // Only the resulting data cursor is applied to the real editor.
        let mut candidate = field.clone();
        candidate.cancel_selection();
        for _ in 0..body.height {
            if caret.y == position.y {
                break;
            }
            let before = candidate.cursor();
            candidate.move_cursor(if caret.y > position.y {
                CursorMove::Up
            } else {
                CursorMove::Down
            });
            caret = probe(&candidate, body);
            if candidate.cursor() == before {
                break;
            }
        }
        for _ in 0..=body.width {
            if caret.x == position.x {
                break;
            }
            let before = candidate.cursor();
            let forward = caret.x < position.x;
            candidate.move_cursor(if forward {
                CursorMove::Forward
            } else {
                CursorMove::Back
            });
            let next = probe(&candidate, body);
            if next.y != caret.y || (forward && next.x > position.x) {
                candidate.move_cursor(jump(before));
                break;
            }
            caret = next;
            if candidate.cursor() == before {
                break;
            }
        }
        if extend {
            if !field.is_selecting() {
                field.start_selection();
            }
        } else {
            field.cancel_selection();
        }
        field.move_cursor(jump(candidate.cursor()));
        if !extend {
            field.start_selection();
        }
    }
}

fn jump((row, column): (usize, usize)) -> CursorMove {
    CursorMove::Jump(
        u16::try_from(row).unwrap_or(u16::MAX),
        u16::try_from(column).unwrap_or(u16::MAX),
    )
}

fn probe(field: &TextArea<'_>, body: Rect) -> Position {
    let mut buffer = Buffer::empty(body);
    field.render(body, &mut buffer);
    caret(&buffer, body).unwrap_or(body.as_position())
}

pub(super) fn caret(buffer: &Buffer, area: Rect) -> Option<Position> {
    area.positions()
        .find(|&position| buffer[position].modifier.contains(Modifier::REVERSED))
}

/// Draw a field from its session value alone, without a live editor.
pub(super) fn render_static(
    node: &Node,
    session: &Session,
    area: Rect,
    theme: &Theme,
    selected: bool,
    buffer: &mut Buffer,
) {
    if let Node::Input { key, .. } = node {
        paint(
            &mut editor(&session.value(key), node),
            FieldView {
                node,
                session,
                area,
                theme,
                selected,
                vim: None,
            },
            buffer,
        );
    }
}

#[derive(Clone, Copy)]
struct FieldView<'a> {
    node: &'a Node,
    session: &'a Session,
    area: Rect,
    theme: &'a Theme,
    selected: bool,
    /// The focused field's vim state, for its Visual selection.
    vim: Option<&'a Vim>,
}

/// Paint one field into `view.area`; returns the editor's text body.
fn paint(field: &mut TextArea<'static>, view: FieldView<'_>, buffer: &mut Buffer) -> Rect {
    let FieldView {
        node,
        session,
        area,
        theme,
        selected,
        vim,
    } = view;
    let Node::Input {
        key,
        label,
        rows,
        placeholder,
        hint,
        rules,
    } = node
    else {
        return Rect::default();
    };
    Clear.render(area, buffer);
    buffer.set_style(area, theme.background_style());
    let error = session.input_error(key);
    // Focus wins over the error colour so the focused field stays findable;
    // the error itself still shows on the row below.
    let border = if selected {
        theme.accent
    } else if error.is_some() {
        theme.error
    } else {
        theme.border_dim
    };
    let required = if rules.get("required").and_then(serde_json::Value::as_bool) == Some(true) {
        " *"
    } else {
        ""
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(theme.raised))
        .padding(Padding::horizontal(u16::from(area.width >= 5)))
        .title(format!(" {}{required} ", safe(label)));
    let frame = Rect::new(
        area.x,
        area.y,
        area.width,
        rows.saturating_add(2).min(area.height),
    );
    let body = block.inner(frame);
    block.render(frame, buffer);
    field.set_style(Style::default().fg(theme.fg).bg(theme.raised));
    field.set_cursor_line_style(Style::default());
    field.set_selection_style(Style::default().fg(theme.fg).bg(theme.selection));
    field.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    field.set_placeholder_text(safe(placeholder));
    field.set_placeholder_style(Style::default().fg(theme.fg_dim).bg(theme.raised));
    crate::editor::decorate(
        field,
        crate::editor::Decor {
            syntax: node.is_textarea().then_some(("markdown", 0)),
            colors: crate::ui::style::SyntaxColors::from_theme(theme),
            vim,
            selection: Style::default().fg(theme.fg).bg(theme.selection),
        },
    );
    if body.width > 0 && body.height > 0 {
        (&*field).render(body, buffer);
        if !selected && let Some(caret) = caret(buffer, body) {
            buffer[caret].modifier.remove(Modifier::REVERSED);
        }
    }
    if frame.bottom() < area.bottom() {
        Paragraph::new(safe(error.as_deref().unwrap_or(hint)))
            .style(
                Style::default()
                    .fg(if error.is_some() {
                        theme.error
                    } else {
                        theme.fg_dim
                    })
                    .bg(theme.bg),
            )
            .render(Rect::new(area.x, frame.bottom(), area.width, 1), buffer);
    }
    body
}
