use super::layout::{
    CONTENT_X, card_buffer, card_height, choice_list, follow_focus, left_out_note, node_regions,
    scrolled, streaming_buffer,
};
use super::lines::{select_option_at, stepper_arrows};
use super::{
    Control, Entry, Key, Placement, Widgets, binding_name, components, place_caret, plain, practice,
};
use crate::theme::Theme;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use nexus_core::interactive::{Document, Node, Session};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph};
use std::collections::HashMap;

#[derive(Clone)]
pub(super) struct Hit {
    pub(super) rect: Rect,
    pub(super) key: Key,
    pub(super) control: Option<usize>,
    pub(super) row_offset: u16,
    /// An image region: its URL and position among the document's images.
    pub(super) image: Option<(String, usize)>,
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
    /// Where Diagram components in complete, on-transcript panels sit, as
    /// transcript placements for the diagram painter. Only diagrams that
    /// start at the content edge line up with its fixed inset; others (a
    /// row's right column, a pinned panel) keep their text placeholder.
    pub(crate) fn diagram_placements(
        &self,
        placements: &[Placement],
        viewport_width: u16,
    ) -> Vec<crate::diagrams::Placement> {
        let mut out = Vec::new();
        for placement in placements.iter().filter(|p| p.complete) {
            let key = placement.key();
            if self.pinned && self.focused.as_ref() == Some(&key) {
                continue;
            }
            let Some(Ok(session)) = self.entries.get(&key).map(|entry| &entry.session) else {
                continue;
            };
            let width = placement.width.min(viewport_width.saturating_sub(2));
            for (node, region) in components::visible_regions(session, width) {
                if let Node::Diagram { source, .. } = node
                    && region.x == CONTENT_X
                {
                    out.push(crate::diagrams::Placement {
                        row: placement.row + usize::from(region.y),
                        width: region.width,
                        source: source.clone(),
                        complete: true,
                    });
                }
            }
        }
        out
    }
    /// Timers run while the window has focus and someone used it recently;
    /// a panel left alone for half an hour stops ticking.
    pub(super) fn timers_live(&self) -> bool {
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
    pub(super) fn textarea_focused(&self) -> bool {
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
    pub(super) fn take_selection(&mut self, cut: bool) -> Option<String> {
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
    pub(super) fn register(&mut self, key: &Key) {
        if self.entries.contains_key(key) {
            return;
        }
        let recovered = Document::recover(&key.source).map_err(|errors| {
            errors
                .iter()
                .map(|error| format!("{}: {}", error.path, error.message))
                .collect::<Vec<_>>()
                .join("\n")
        });
        let left_out = recovered
            .as_ref()
            .ok()
            .and_then(|(_, dropped)| dropped.first())
            .map(left_out_note);
        let mut session = recovered
            .and_then(|(document, _)| Session::new(document).map_err(|error| error.to_string()));
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
        let mut entry = Entry::new(session);
        entry.left_out = left_out;
        self.entries.insert(key.clone(), entry);
    }
    pub(super) fn reconcile_patches(&mut self, placements: &[Placement]) {
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
            let mut hits = hits;
            let mut caret = if active && focused {
                self.entries
                    .get(&key)
                    .and_then(|entry| entry.input_caret(&canvas))
            } else {
                None
            };
            if focused && !pinned && placement.complete {
                // Tab reached a control outside the visible rows of a tall
                // panel: pin it, and the pinned view scrolls to that control.
                let hidden = self.entries.get(&key).is_some_and(|entry| {
                    hits.iter().any(|(rect, control)| {
                        *control == entry.focus
                            && (usize::from(rect.y) < first || usize::from(rect.bottom()) > end)
                    })
                });
                if hidden {
                    self.pinned = true;
                    self.animating = true;
                }
            }
            if pinned && let Some(entry) = self.entries.get_mut(&key) {
                let scroll = follow_focus(entry, &canvas, &hits, caret, viewport.height);
                if scroll > 0 {
                    canvas = scrolled(&canvas, scroll);
                    hits = hits
                        .into_iter()
                        .filter(|(rect, _)| rect.bottom() > scroll)
                        .map(|(rect, control)| {
                            let y = rect.y.max(scroll);
                            (
                                Rect::new(rect.x, y - scroll, rect.width, rect.bottom() - y),
                                control,
                            )
                        })
                        .collect();
                    caret = caret.and_then(|caret| {
                        Some(Position::new(caret.x, caret.y.checked_sub(scroll)?))
                    });
                }
            }
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
            if let Some(caret) = caret
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
    pub(super) fn click(
        &mut self,
        mouse: MouseEvent,
        screen: Rect,
        can_send: bool,
    ) -> (bool, Option<String>) {
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
            Some((index, Control::Select(node)))
                if let Ok(session) = &mut entry.session
                    && let Some(Node::Select { key, options, .. }) =
                        session.document().node(node).cloned()
                    && let Some(target) = select_option_at(
                        session.document().node(node).expect("control node"),
                        session,
                        hit.rect,
                        pos.x,
                    ) =>
            {
                // A segment click picks that option; cycling forward reaches
                // it through the session's own validation.
                entry.focus = index;
                self.focused = Some(hit.key);
                for _ in 0..options.len() {
                    if session.value(&key) == options[target] {
                        break;
                    }
                    if let Err(error) = session.cycle(node, true) {
                        entry.error = error.to_string();
                        break;
                    }
                }
                (true, None)
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
    pub(super) fn key(&mut self, key: KeyEvent, can_send: bool) -> (bool, Option<String>) {
        self.last_input = Some(std::time::Instant::now());
        let result = self.handle_key(key, can_send);
        self.open_trusted();
        result
    }
    #[allow(clippy::too_many_lines)]
    pub(super) fn handle_key(&mut self, key: KeyEvent, can_send: bool) -> (bool, Option<String>) {
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
    pub(super) fn popup(&mut self, f: &mut Frame, app_theme: &Theme, area: Rect, can_send: bool) {
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
