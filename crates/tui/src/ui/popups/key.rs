use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::widgets::Paragraph;

use crate::app_view::AppView;
use nexus_core::app::Popup;

use super::chrome::{self, PopupFrame, hint, keys};

/// API-key entry with a masked field.
pub fn render(f: &mut Frame, app: &AppView) {
    let outer = chrome::popup_area(app, f.area(), "key", chrome::SMALL);
    let masked = "•".repeat(app.key_input.chars().count());
    let title = format!("{} key", app.key_target_label());
    let inner = chrome::render_popup(
        f,
        outer,
        &PopupFrame {
            title: &title,
            count: "",
            hints: &[hint(keys::ENTER, "save"), hint(keys::ESC, "back")],
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let inner = chrome::render_field(
        f,
        inner,
        "✎",
        &masked,
        "Paste or type the key",
        false,
        &app.theme,
    );
    if app.settings.hide_hints {
        return;
    }
    f.render_widget(
        Paragraph::new("Keys stay on this machine, in your config file.")
            .style(Style::default().fg(app.theme.fg_dim)),
        inner,
    );
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.popup = Popup::Login,
        KeyCode::Enter => app.confirm_key(),
        KeyCode::Char(c) if !super::is_chord(key) => app.key_input.push(c),
        KeyCode::Backspace => {
            app.key_input.pop();
        }
        _ => {}
    }
}
