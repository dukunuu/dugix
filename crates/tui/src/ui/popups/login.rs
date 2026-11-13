//! `/login`'s provider selector; a local runtime takes no key and opens `/local`.

use crossterm::event::{KeyCode, KeyEvent};
use nexus_core::provider::BackendTag;
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;

use super::chrome::{self, PopupFrame, hint, keys};
use crate::ui::style::glyph;

// Preserve the login selector's historical order, which differs from routing order.
pub(crate) const LOGIN_ORDER: [BackendTag; 6] = [
    BackendTag::OpenRouter,
    BackendTag::OpencodeGo,
    BackendTag::OpenAi,
    BackendTag::Codex,
    BackendTag::Mistral,
    BackendTag::Local,
];

pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "login", chrome::SMALL);
    let configured = LOGIN_ORDER
        .iter()
        .filter(|&&tag| app.backends.configured(tag))
        .count();
    let count = format!("{configured} of {} set", LOGIN_ORDER.len());
    let hints = [
        hint(keys::MOVE, ""),
        hint(keys::ENTER, "pick"),
        hint(keys::ESC, "close"),
    ];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Login",
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );

    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = LOGIN_ORDER
        .iter()
        .map(|&tag| {
            let (name, hint) = if let Some(key) = &tag.info().key {
                let action = if tag == BackendTag::Mistral {
                    "Vibe plan key (Code > Vibe CLI)"
                } else {
                    "paste a key"
                };
                (key.label, format!("{action}, or reads ${}", key.env_var))
            } else if tag == BackendTag::Codex {
                (
                    tag.name(),
                    "ChatGPT subscription — device-code login".to_string(),
                )
            } else {
                (
                    "Local runtime",
                    "Ollama / llama.cpp / MLX / LM Studio / edge0 — no key".to_string(),
                )
            };
            // State chip: which backends already have a key configured.
            let configured = app.backends.configured(tag);
            let chip = if configured {
                Span::styled(
                    format!("{} configured", glyph::OK),
                    Style::default().fg(app.theme.success),
                )
            } else {
                Span::styled(format!("{} no key", glyph::RING), dim)
            };
            let width = area.width.saturating_sub(6 + 2 * chrome::PAD) as usize;
            let name = chrome::truncate(
                name,
                width.saturating_sub(crate::ui::style::width(&chip.content) + 2),
            );
            let pad = width.saturating_sub(
                crate::ui::style::width(&name) + crate::ui::style::width(&chip.content) + 2,
            );
            let top = Line::from(vec![
                ratatui::text::Span::styled(name, Style::default().fg(app.theme.fg)),
                ratatui::text::Span::raw(" ".repeat(pad)),
                chip,
            ]);
            ListItem::new(vec![
                top,
                Line::from(ratatui::text::Span::styled(format!("  {hint}"), dim)),
                Line::from(""),
            ])
        })
        .collect();

    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    state.select(Some(app.login_selected));
    chrome::render_list(f, list, &mut state, inner, LOGIN_ORDER.len(), 3, app);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.popup = nexus_core::app::Popup::None,
        KeyCode::Up => app.move_login_selection(-1),
        KeyCode::Down => app.move_login_selection(1),
        KeyCode::Char('k') if !super::is_chord(key) => app.move_login_selection(-1),
        KeyCode::Char('j') if !super::is_chord(key) => app.move_login_selection(1),
        KeyCode::PageUp => app.move_login_selection(-10),
        KeyCode::PageDown => app.move_login_selection(10),
        KeyCode::Home => app.move_login_selection(i32::MIN / 2),
        KeyCode::End => app.move_login_selection(i32::MAX / 2),
        KeyCode::Enter => app.confirm_login_selection(),
        _ => {}
    }
}
