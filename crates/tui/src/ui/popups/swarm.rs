use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use nexus_core::app::SwarmPopupMode;

use super::chrome::{self, PopupFrame, ctrl, hint, keys};

#[allow(clippy::too_many_lines)] // roster rows and mode-specific frame
pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "swarm", chrome::WIDE);

    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = app
        .swarm
        .cache
        .iter()
        .map(|p| {
            let content_w = (area.width.saturating_sub(5 + 2 * chrome::PAD)) as usize;
            let name = chrome::truncate(
                &p.name,
                content_w.saturating_sub(crate::ui::style::width(&p.model) + 3),
            );
            let blurb = chrome::truncate(&p.blurb, content_w.saturating_sub(3));
            let top = Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(format!("  {}", p.model), dim),
            ]);
            ListItem::new(vec![
                top,
                Line::from(Span::styled(format!("  {blurb}"), dim)),
                Line::from(""),
            ])
        })
        .collect();

    let on = app.session.as_ref().is_some_and(|s| s.swarm_mode);
    let n = app.swarm.cache.len();
    let confirm = app.swarm_popup_mode == SwarmPopupMode::ConfirmDelete;
    let count = if confirm {
        String::new()
    } else {
        format!(
            "{} · {n} persona{}",
            if on { "on" } else { "off" },
            if n == 1 { "" } else { "s" }
        )
    };
    let (title, hints) = if confirm {
        (
            "Remove persona?",
            vec![hint(ctrl('d'), "remove"), hint(keys::ESC, "cancel")],
        )
    } else {
        let mut hints = Vec::new();
        if n > 0 {
            hints.extend([hint(keys::MOVE, ""), hint(keys::ENTER, "edit")]);
        }
        hints.extend([hint("n", "add"), hint("g", "on/off")]);
        if app.swarm.rx.is_some() {
            hints.push(hint("x", "stop"));
        }
        if n > 0 {
            hints.extend([
                hint(ctrl('e'), "edit"),
                hint("m", "model"),
                hint(ctrl('d'), "remove"),
            ]);
        }
        hints.push(hint(keys::ESC, "close"));
        ("Swarm", hints)
    };
    let tone = if app.swarm_popup_mode == SwarmPopupMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title,
            count: &count,
            hints: &hints,
            tone,
            focused: true,
        },
        app,
    );
    let detail = if confirm {
        let name = app
            .swarm
            .cache
            .get(app.swarm_selected)
            .map_or("", |p| p.name.as_str());
        format!("“{name}” will be removed from the swarm.")
    } else {
        String::new()
    };
    let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
    chrome::render_detail(f, detail_area, &detail, &app.theme);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.swarm.cache.is_empty() {
        state.select(Some(app.swarm_selected.min(app.swarm.cache.len() - 1)));
    }
    chrome::render_list(
        f,
        list,
        &mut state,
        list_area,
        app.swarm.cache.len(),
        3,
        app,
    );
    if app.swarm.cache.is_empty() {
        chrome::render_empty(
            f,
            list_area,
            "No personas yet. n adds one, or send a message and three will be suggested.",
            &app.theme,
        );
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use super::{ConfirmDeleteAction, classify_confirm_delete_key};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match app.swarm_popup_mode {
        SwarmPopupMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.swarm_remove_row()?,
            Some(ConfirmDeleteAction::No) => app.swarm_popup_mode = SwarmPopupMode::Browse,
            None => {}
        },
        SwarmPopupMode::Browse => {
            // Ctrl+M is the same byte as Enter on terminals without the kitty
            // keyboard protocol, so only plain `m` can open the picker.
            if key.code == KeyCode::Char('m') && !super::is_chord(key) {
                if !app.swarm.cache.is_empty() {
                    app.open_model_picker_for_swarm_persona(app.swarm_selected);
                }
                return Ok(());
            }
            match key.code {
                KeyCode::Esc => app.popup = nexus_core::app::Popup::None,
                KeyCode::Up => app.move_swarm_selection(-1),
                KeyCode::Down => app.move_swarm_selection(1),
                KeyCode::Char('k') if !super::is_chord(key) => app.move_swarm_selection(-1),
                KeyCode::Char('j') if !super::is_chord(key) => app.move_swarm_selection(1),
                KeyCode::PageUp => app.move_swarm_selection(-10),
                KeyCode::PageDown => app.move_swarm_selection(10),
                KeyCode::Home => app.move_swarm_selection(i32::MIN / 2),
                KeyCode::End => app.move_swarm_selection(i32::MAX / 2),
                KeyCode::Char('g') if !super::is_chord(key) => app.toggle_swarm_mode()?,
                KeyCode::Char('n') if !super::is_chord(key) => {
                    app.queue_swarm_persona_editor(true)?;
                }
                KeyCode::Enter if !app.swarm.cache.is_empty() => {
                    app.queue_swarm_persona_editor(false)?;
                }
                KeyCode::Char('e') if ctrl && !app.swarm.cache.is_empty() => {
                    app.queue_swarm_persona_editor(false)?;
                }
                KeyCode::Char('x') if !super::is_chord(key) && app.swarm.rx.is_some() => {
                    app.stop_swarm();
                    app.popup = nexus_core::app::Popup::None;
                }
                KeyCode::Char('d') if ctrl && !app.swarm.cache.is_empty() => {
                    app.swarm_popup_mode = SwarmPopupMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
