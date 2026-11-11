use super::layout::{
    CONTENT_X, card_buffer, card_height, choice_list, document_buffer, draw_left_out, node_regions,
    render_button_group, render_nodes, session_controls, text_lines,
};
use super::lines::{control_style, render_component};
use super::widgets::safe;
use super::{
    Control, Entry, ImageTarget, PRESS_FLASH_MS, SEND_BLOCKED, components, document_images,
    image_targets, inputs, persist, practice, shape_sends,
};
use crate::theme::Theme;
use crate::ui::style::glyph;
use crossterm::event::KeyCode;
use nexus_core::interactive::{Node, Session, Tone};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Paragraph, Widget, Wrap};

impl Entry {
    pub(super) fn new(session: Result<Session, String>) -> Self {
        Self {
            session,
            focus: 0,
            error: String::new(),
            scroll: 0,
            inputs: std::cell::RefCell::default(),
            born: std::time::Instant::now(),
            pressed: None,
            left_out: None,
        }
    }
    /// Move the animation clock to now; whether anything is still moving.
    /// With motion off the clock stays put: canvases hold their first frame,
    /// values snap and charts draw whole.
    pub(super) fn advance_clock(&mut self, motion: bool, timers: bool) -> bool {
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
    pub(super) fn focused_input(&self) -> Option<usize> {
        match self.controls().get(self.focus) {
            Some(Control::Input(node)) => Some(*node),
            _ => None,
        }
    }
    /// Terminal caret of the focused field on a canvas this entry just drew.
    pub(super) fn input_caret(&self, buffer: &Buffer) -> Option<Position> {
        self.focused_input()?;
        self.inputs.borrow().caret(buffer)
    }
    /// Layouts are display-only: never focusable, pinned or counted as panels.
    pub(super) fn is_interactive(&self) -> bool {
        self.session
            .as_ref()
            .is_ok_and(|s| !s.document().is_static())
    }
    pub(super) fn controls(&self) -> Vec<Control> {
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
                        // Visible expandable branches toggle like sections.
                        Node::Tree { items } => nexus_core::interactive::tree_branches(items)
                            .iter()
                            .enumerate()
                            .filter(|(branch, (_, item))| {
                                !item.items.is_empty() && session.branch_visible(index, *branch)
                            })
                            .map(|(branch, _)| Control::Section(index, branch))
                            .collect(),
                        _ => Vec::new(),
                    })
                    .chain(session.needs_data_load().then_some(Control::Load))
                    .collect()
            },
        )
    }
    pub(super) fn choices(&self) -> Vec<usize> {
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
    pub(super) fn answering(&self) -> bool {
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
    pub(super) fn activate(
        &mut self,
        control: usize,
        can_send: bool,
        can_answer: bool,
    ) -> Option<String> {
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
    pub(super) fn navigate(&mut self, code: KeyCode) -> bool {
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
    pub(super) fn edit(&mut self, text: &str, backspace: bool) {
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
    pub(super) fn canvas(
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
        if let Some(note) = &self.left_out {
            let hint = if session.document().is_static() {
                ""
            } else {
                hint.as_str()
            };
            draw_left_out(&mut buffer, note, hint, theme);
        }
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
                .cell((rect.x + 1, rect.y))
                .is_some_and(|cell| cell.symbol() != glyph::SELECTED)
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
    pub(super) fn image_targets(&self, width: u16) -> Vec<ImageTarget> {
        self.session
            .as_ref()
            .map_or_else(|_| Vec::new(), |session| image_targets(session, width))
    }
    /// The image `v` opens: the focused image button's, else the first one.
    pub(super) fn focused_image(&self) -> Option<(usize, String)> {
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
    pub(super) fn nodes(
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
    pub(super) fn branch(
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
