use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use dugix_core::app::SettingsRow;

use super::chrome::{self, PopupFrame, hint, keys};
use crate::ui::style::glyph;

/// Split "label (description)" into the two display columns.
fn split_label(label: &'static str) -> (String, String) {
    match label.split_once(" (") {
        Some((name, rest)) => (name.to_string(), rest.trim_end_matches(')').to_string()),
        None => (label.to_string(), String::new()),
    }
}

// Long by design (settings renderer).
#[allow(clippy::too_many_lines)]
pub fn render(f: &mut Frame, app: &AppView) {
    use dugix_core::app::{SETTINGS_GROUPS, SettingsField};
    let area = chrome::popup_area(app, f.area(), "settings", chrome::STANDARD);

    let dim = Style::default().fg(app.theme.fg_dim);
    let name_w = SettingsField::ALL
        .iter()
        .map(|f| split_label(f.label()).0.chars().count())
        .max()
        .unwrap_or(0);

    let toggle = |b: bool| -> Span<'static> {
        if b {
            Span::styled(
                format!("{} on ", glyph::DOT),
                Style::default().fg(app.theme.success),
            )
        } else {
            Span::styled(format!("{} off", glyph::RING), dim)
        }
    };
    let numeric = |s: &str| -> Span<'static> {
        if s.trim().is_empty() {
            Span::styled("default", dim.add_modifier(Modifier::ITALIC))
        } else {
            Span::styled(s.to_string(), Style::default().fg(app.theme.accent))
        }
    };
    let choice = |s: &str| -> Span<'static> {
        Span::styled(
            format!("{} {s} {}", glyph::PREV, glyph::NEXT),
            Style::default().fg(app.theme.accent),
        )
    };
    let value = |field: SettingsField| -> Span<'static> {
        match field {
            SettingsField::ReplyStyle => choice(&app.settings.reply_style),
            SettingsField::ImageViewer => choice(&app.settings.image_viewer),
            SettingsField::CacheLifetime => choice(&app.settings.cache_lifetime),
            SettingsField::CacheLongOpenai => toggle(app.settings.cache_long_openai),
            SettingsField::ColumnWidth => choice(&app.settings.column_width),
            SettingsField::RemoteImages => toggle(app.settings.remote_images),
            SettingsField::ShowStats => toggle(app.settings.show_stats),
            SettingsField::ShowReasoning => toggle(app.settings.show_reasoning),
            SettingsField::HideHints => toggle(app.settings.hide_hints),
            SettingsField::Temperature => numeric(&app.settings_inputs[0]),
            SettingsField::TopP => numeric(&app.settings_inputs[1]),
            SettingsField::MaxTokens => numeric(&app.settings_inputs[2]),
            SettingsField::CompactThreshold => numeric(&app.settings_inputs[3]),
            SettingsField::SearxngUrl => numeric(&app.settings_inputs[4]),
            SettingsField::Verbosity => choice(&app.verbosity),
            SettingsField::LangsearchKey => numeric(&mask_secret(&app.settings_inputs[5])),
            SettingsField::SearchProvider => choice(&app.search.provider),
            SettingsField::BlockedDomains => numeric(&app.settings_inputs[6]),
        }
    };

    let rows = app.settings_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            SettingsRow::Group(i) => {
                let g = &SETTINGS_GROUPS[*i];
                let arrow = if app.settings_collapsed.contains(i) {
                    glyph::SELECTED
                } else {
                    glyph::EXPANDED
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{arrow} {}", g.name),
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD),
                )))
            }
            SettingsRow::Field(field) => {
                let (name, _) = split_label(field.label());
                let name = chrome::truncate(&name, name_w.max(1));
                let v = value(*field);
                let v_max = (area.width.saturating_sub(8 + 2 * chrome::PAD) as usize)
                    .saturating_sub(name_w + 3); // border + scrollbar + highlight + label column
                let v_span = Span::styled(chrome::truncate(&v.content, v_max.max(1)), v.style);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("  {name:<name_w$}")),
                    Span::raw("   "),
                    v_span,
                ]))
            }
        })
        .collect();

    let desc = match app.settings_row() {
        SettingsRow::Field(f) => split_label(f.label()).1.replace("Ctrl+", "^"),
        SettingsRow::Group(_) => String::new(),
    };

    let count = format!("{} settings", SettingsField::ALL.len());
    let hints = if editing(app) {
        vec![hint(keys::ENTER, "save"), hint(keys::ESC, "cancel")]
    } else {
        vec![
            hint(keys::MOVE, ""),
            hint(keys::SPACE, "toggle"),
            hint(keys::SIDE, "change"),
            hint(keys::ESC, "close"),
        ]
    };
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Settings",
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let inner = if let Some(i) = app.text_index() {
        let text = if i == 5 {
            mask_secret(&app.settings_inputs[i])
        } else {
            app.settings_inputs[i].clone()
        };
        chrome::render_field(f, inner, "✎", &text, "", true, &app.theme)
    } else {
        inner
    };
    let (list_area, detail_area) = chrome::split_with_detail(inner, &desc);

    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.settings_selected.min(rows.len() - 1)));
    }
    chrome::render_list(f, list, &mut state, list_area, rows.len(), 1, app);
    chrome::render_detail(f, detail_area, &desc, &app.theme);
}

fn saved_input(app: &AppView) -> Option<String> {
    use dugix_core::app::SettingsField;
    Some(match app.settings_field()? {
        SettingsField::Temperature => app
            .settings
            .temperature
            .map(|v| v.to_string())
            .unwrap_or_default(),
        SettingsField::TopP => app
            .settings
            .top_p
            .map(|v| v.to_string())
            .unwrap_or_default(),
        SettingsField::MaxTokens => app
            .settings
            .max_tokens
            .map(|v| v.to_string())
            .unwrap_or_default(),
        SettingsField::CompactThreshold => app.settings.compact_threshold.to_string(),
        SettingsField::SearxngUrl => app.search.searxng_url.clone(),
        SettingsField::LangsearchKey => app.search.langsearch_key.clone(),
        SettingsField::BlockedDomains => {
            std::fs::read_to_string(app.space.blocked_domains_path(&app.active_space.name))
                .unwrap_or_default()
        }
        _ => return None,
    })
}

fn editing(app: &AppView) -> bool {
    app.text_index()
        .zip(saved_input(app))
        .is_some_and(|(i, saved)| app.settings_inputs[i] != saved)
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use dugix_core::app::{
        IMAGE_VIEWERS, REPLY_STYLES, SEARCH_PROVIDERS, SettingsField, VERBOSITY_LEVELS,
    };
    // The column width has no text to type: the arrows and Enter cycle it.
    if app.settings_field() == Some(SettingsField::ColumnWidth) {
        match key.code {
            KeyCode::Enter | KeyCode::Right => return app.cycle_column_width(1),
            KeyCode::Left => return app.cycle_column_width(-1),
            _ => {}
        }
    }
    if matches!(key.code, KeyCode::Left | KeyCode::Right) {
        let choices = match app.settings_field() {
            Some(SettingsField::ReplyStyle) => REPLY_STYLES.len(),
            Some(SettingsField::ImageViewer) => IMAGE_VIEWERS.len(),
            Some(SettingsField::Verbosity) => VERBOSITY_LEVELS.len(),
            Some(SettingsField::SearchProvider) => SEARCH_PROVIDERS.len(),
            Some(SettingsField::CacheLifetime) => 2,
            _ => 0,
        };
        let steps = if key.code == KeyCode::Left {
            choices.saturating_sub(1)
        } else {
            usize::from(choices > 0)
        };
        for _ in 0..steps {
            app.toggle_settings_field();
        }
        return Ok(());
    }
    match key.code {
        // Enter on a group header toggles it; fields with an Enter action were
        // handled above.
        KeyCode::Enter if matches!(app.settings_row(), SettingsRow::Group(_)) => {
            app.toggle_settings_field();
        }
        KeyCode::Esc if editing(app) => {
            if let Some((i, saved)) = app.text_index().zip(saved_input(app)) {
                app.settings_inputs[i] = saved;
            }
        }
        KeyCode::Esc => app.save_settings()?,
        KeyCode::Enter => {
            app.save_settings()?;
            app.popup = dugix_core::app::Popup::Settings;
            if let Some((i, saved)) = app.text_index().zip(saved_input(app)) {
                app.settings_inputs[i] = saved;
            }
        }
        KeyCode::Char(c @ ('j' | 'k'))
            if !super::is_chord(key) && !editing(app) && app.text_index().is_none_or(|i| i < 4) =>
        {
            app.move_settings_selection(if c == 'j' { 1 } else { -1 });
        }
        KeyCode::Up => app.move_settings_selection(-1),
        KeyCode::Down | KeyCode::Tab => app.move_settings_selection(1),
        KeyCode::PageUp => app.move_settings_selection(-10),
        KeyCode::PageDown => app.move_settings_selection(10),
        KeyCode::Home => app.settings_selected = 0,
        KeyCode::End => app.settings_selected = app.settings_rows().len().saturating_sub(1),
        KeyCode::Char(' ') => app.toggle_settings_field(),
        KeyCode::Char(c) if !super::is_chord(key) => app.settings_input_char(c),
        KeyCode::Backspace => app.settings_input_backspace(),
        _ => {}
    }
    Ok(())
}

/// `••••••3814`: enough to recognize which key is set, never the key. Short
/// values mask fully so the tail can't be most of the secret.
fn mask_secret(s: &str) -> String {
    let s = s.trim();
    let n = s.chars().count();
    if n == 0 {
        return String::new();
    }
    if n < 12 {
        return "•".repeat(6);
    }
    let tail: String = s.chars().skip(n - 4).collect();
    format!("••••••{tail}")
}
