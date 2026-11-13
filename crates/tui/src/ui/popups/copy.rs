use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;

use super::chrome::{self, PopupFrame, hint, keys};

pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "copy", chrome::SMALL);
    let n = app.copy_options.len();
    let count = format!("{n} item{}", if n == 1 { "" } else { "s" });
    let mut hints = if app.copy_options.is_empty() {
        Vec::new()
    } else {
        vec![hint(keys::MOVE, ""), hint(keys::ENTER, "copy")]
    };
    hints.push(hint(keys::ESC, "close"));
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Copy",
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );

    let items: Vec<ListItem> = app
        .copy_options
        .iter()
        .map(|o| {
            ListItem::new(Line::from(Span::styled(
                chrome::truncate(
                    &o.label,
                    (area.width.saturating_sub(5 + 2 * chrome::PAD)) as usize,
                ),
                Style::default().fg(app.theme.fg),
            )))
        })
        .collect();
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.copy_options.is_empty() {
        state.select(Some(app.copy_selected.min(app.copy_options.len() - 1)));
    }
    chrome::render_list(f, list, &mut state, inner, app.copy_options.len(), 1, app);
    if app.copy_options.is_empty() {
        chrome::render_empty(f, inner, "Nothing to copy here yet.", &app.theme);
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.popup = nexus_core::app::Popup::None,
        KeyCode::Enter => app.confirm_copy(),
        KeyCode::Up => app.move_copy_selection(-1),
        KeyCode::Down => app.move_copy_selection(1),
        KeyCode::Char('k') if !super::is_chord(key) => app.move_copy_selection(-1),
        KeyCode::Char('j') if !super::is_chord(key) => app.move_copy_selection(1),
        KeyCode::PageUp => app.move_copy_selection(-10),
        KeyCode::PageDown => app.move_copy_selection(10),
        KeyCode::Home => app.move_copy_selection(i32::MIN / 2),
        KeyCode::End => app.move_copy_selection(i32::MAX / 2),
        _ => {}
    }
}
