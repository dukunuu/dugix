use super::{Control, Entry, no_modifier_but_shift, pin_focused, safe};
use crate::app_view::AppView;
use crate::ui::popups::chrome::{self, PopupFrame, hint, keys};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use dugix_core::app::{AppCommand, MAX_CHAT_TASKS, Popup};
use dugix_core::interactive::{Node, Session};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

fn answer_block(app: &AppView) -> Option<&'static str> {
    if app.viewing_stream() {
        Some("wait for the reply to finish, then pick again")
    } else if app.user_command_running() {
        Some("wait for the command to finish, then pick again")
    } else {
        None
    }
}
/// A proposed response must not race a reply or overwrite a draft.
#[must_use]
pub(crate) fn can_send(app: &AppView) -> bool {
    !app.viewing_stream() && app.input_text().is_empty()
}
pub(super) fn review(app: &mut AppView, message: Option<String>) {
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
/// Copy/cut a nonempty field selection; false when nothing is selected.
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
    if !app.interactive.input_focused() && !no_modifier_but_shift(&event) {
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
        let viewport = viewport.intersection(f.area());
        let enabled = can_send(app);
        let Some(entry) = app
            .interactive
            .focused
            .as_ref()
            .and_then(|key| app.interactive.entries.get(key))
        else {
            return;
        };
        let title = entry.session.as_ref().map_or_else(
            |_| "UI unavailable".into(),
            |session| safe(session.document().title()),
        );
        let hints = if super::practice::editing(entry).is_some() || entry.focused_input().is_some()
        {
            entry
                .hint(viewport.width, true)
                .split(" · ")
                .map(|part| {
                    let (key, verb) = part.split_once(' ').unwrap_or((part, ""));
                    hint(key.to_string(), verb.to_string())
                })
                .collect()
        } else {
            vec![
                hint(keys::TAB, "next"),
                hint(keys::ENTER, "choose"),
                hint(keys::ESC, "close"),
            ]
        };
        chrome::render_popup(
            f,
            viewport,
            &PopupFrame {
                title: &title,
                count: "",
                hints: &hints,
                tone: chrome::Tone::Normal,
                focused: true,
            },
            app,
        );
        // The panel painter owns its content and hits; keep only the shared outer chrome.
        let edge = |x: u16, y: u16| {
            y == viewport.y
                || y + 1 == viewport.bottom()
                || x == viewport.x
                || x + 1 == viewport.right()
        };
        let border: Vec<_> = viewport
            .positions()
            .filter(|p| edge(p.x, p.y))
            .map(|p| (p, f.buffer_mut()[(p.x, p.y)].clone()))
            .collect();
        app.interactive.popup(f, &app.theme, viewport, enabled);
        for (p, cell) in border {
            f.buffer_mut()[(p.x, p.y)] = cell;
        }
    } else {
        // Repaint just this card at full strength: the popup dispatcher dims the base view.
        app.interactive.hits.clear();
        app.interactive
            .paint(f, viewport, top, &placements, &app.theme, true);
    }
}
