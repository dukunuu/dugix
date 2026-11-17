//! `/local`'s runtime selector: picking a row writes the machine-local `[local]`
//! block and reloads the catalog; custom endpoints stay a config-file concern.

use std::fmt::Write as _;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use dugix_core::provider::local::{LocalConfig, LocalRuntime};
use dugix_core::provider::serve::{RuntimeStatus, format_kb};

use crate::app_view::AppView;

use super::chrome::{self, PopupFrame, hint, keys};
use crate::ui::style::glyph;

/// The runtime rows, plus a trailing "off" row.
pub const ROW_COUNT: usize = LocalRuntime::ALL.len() + 1;

/// One row: label, discovery command, endpoint, and whether it is the active
/// runtime. Both hints are shown because either can explain a failed pick.
fn row(index: usize, current: Option<&LocalConfig>) -> (String, String, String, bool) {
    match LocalRuntime::ALL.get(index) {
        Some(&runtime) => (
            runtime.label().to_string(),
            match runtime {
                LocalRuntime::Ollama => "ollama list",
                LocalRuntime::Mlx => "scans the Hugging Face cache",
                LocalRuntime::MlxServe => "mlx-serve list",
                LocalRuntime::Lmstudio => "lms ls --json",
                LocalRuntime::Edge0 => "edge0 models",
                LocalRuntime::LlamaCpp => "llama-server --cache-list",
            }
            .to_string(),
            LocalConfig::for_runtime(runtime, current)
                .endpoint()
                .to_string(),
            current.is_some_and(|config| config.provider == runtime),
        ),
        None => (
            "Off".to_string(),
            "disable local inference".to_string(),
            String::new(),
            current.is_none(),
        ),
    }
}

/// The server line for a row: whether the endpoint answers, what its process
/// tree costs, and who started it. Ordered by urgency because the popup is
/// narrow enough to truncate, with the endpoint last as the most predictable.
fn server_line(status: Option<&RuntimeStatus>, endpoint: &str) -> (String, bool) {
    let Some(status) = status else {
        return (format!("{} stopped {endpoint}", glyph::RING), false);
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(rss) = status.rss_kb {
        parts.push(format_kb(rss));
    }
    if status.over_budget {
        parts.push(format!("{} over budget", glyph::WARN));
    }
    if status.managed {
        parts.push("started here".to_string());
    }
    parts.push(endpoint.to_string());
    let mut line = String::from(if status.running {
        "● running"
    } else {
        "○ stopped"
    });
    let _ = write!(line, " {}", parts.join(" · "));
    (line, status.over_budget)
}

#[allow(clippy::too_many_lines)] // runtime metadata and lifecycle frame
pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "local", chrome::SMALL);
    let running = app
        .local
        .status
        .iter()
        .filter(|status| status.running)
        .count();
    let count = if running == 0 {
        String::new()
    } else {
        format!("{running} running")
    };
    let hints = [
        hint(keys::MOVE, ""),
        hint(keys::ENTER, "pick"),
        hint("s", "start"),
        hint("x", "stop"),
        hint("r", "refresh"),
        hint(
            keys::ESC,
            if app.local_from_login {
                "back"
            } else {
                "close"
            },
        ),
    ];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Local models",
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );

    let dim = Style::default().fg(app.theme.fg_dim);
    let current = app.core.saved.local.as_ref();
    let items: Vec<ListItem> = (0..ROW_COUNT)
        .map(|i| {
            let (name, hint, endpoint, active) = row(i, current);
            let chip = if active {
                Span::styled(
                    format!("{} active", glyph::OK),
                    Style::default().fg(app.theme.success),
                )
            } else {
                Span::styled(glyph::RING, dim)
            };
            let width = area.width.saturating_sub(6 + 2 * chrome::PAD) as usize;
            let name = chrome::truncate(
                &name,
                width.saturating_sub(crate::ui::style::width(&chip.content) + 2),
            );
            let pad = width.saturating_sub(
                crate::ui::style::width(&name) + crate::ui::style::width(&chip.content) + 2,
            );
            let top = Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::raw(" ".repeat(pad)),
                chip,
            ]);
            let status = LocalRuntime::ALL.get(i).and_then(|runtime| {
                app.core
                    .local
                    .status
                    .iter()
                    .find(|status| status.runtime == *runtime)
            });
            let (server, warn) = if endpoint.is_empty() {
                (String::new(), false)
            } else {
                server_line(status, &endpoint)
            };
            let status_len = if status.is_some_and(|status| status.running) {
                "● running".len()
            } else {
                "○ stopped".len()
            };
            let (word, metadata) = server.split_at(status_len.min(server.len()));
            let server_style = if status.is_some_and(|status| status.running) {
                Style::default().fg(app.theme.success)
            } else {
                dim
            };
            ListItem::new(vec![
                top,
                Line::from(Span::styled(
                    format!("  {}", chrome::truncate(&hint, width.saturating_sub(2))),
                    dim,
                )),
                chrome::fit_line(
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(word.to_string(), server_style),
                        Span::styled(
                            metadata.to_string(),
                            if warn {
                                Style::default().fg(app.theme.warning)
                            } else {
                                dim
                            },
                        ),
                    ]),
                    width,
                ),
            ])
        })
        .collect();

    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    state.select(Some(app.local_selected));
    chrome::render_list(f, list, &mut state, inner, ROW_COUNT, 3, app);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match key.code {
        KeyCode::Esc if app.local_from_login => app.popup = dugix_core::app::Popup::Login,
        KeyCode::Esc => app.popup = dugix_core::app::Popup::None,
        KeyCode::Up => app.move_local_selection(-1),
        KeyCode::Down => app.move_local_selection(1),
        KeyCode::Char('k') if !super::is_chord(key) => app.move_local_selection(-1),
        KeyCode::Char('j') if !super::is_chord(key) => app.move_local_selection(1),
        KeyCode::PageUp => app.move_local_selection(-10),
        KeyCode::PageDown => app.move_local_selection(10),
        KeyCode::Home => app.move_local_selection(i32::MIN / 2),
        KeyCode::End => app.move_local_selection(i32::MAX / 2),
        KeyCode::Enter => app.confirm_local_selection(),
        // Server lifecycle on the row under the cursor; the "off" row has no
        // server, so these are no-ops there.
        KeyCode::Char('s') => app.local_server_key("start"),
        KeyCode::Char('x') => app.local_server_key("stop"),
        KeyCode::Char('r') => app.local_server_key("status"),
        _ => {}
    }
}
