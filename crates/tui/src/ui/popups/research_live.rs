use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::ListItem;

use crate::app_view::AppView;

use super::chrome;
use crate::ui::style::glyph;

/// Live per-searcher activity: the transcript's `research_stage` rows plus a
/// steer input line and the still-queued steers.
pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "research", chrome::WIDE);

    let dim = Style::default().fg(app.theme.fg_dim);
    // The rows come from the job-level mirror, so opening the popup from another
    // session still shows the job's searchers.
    let rows = &app.research.stage_rows;
    let mut items: Vec<ListItem> = Vec::new();
    // The pipeline acknowledges drained steers as `steer #N` stage rows and the
    // App records those positions; the retained log drops acked entries, so what
    // is left is exactly the still-queued steers.
    let pending: Vec<&(usize, String)> = app
        .research
        .steer_log
        .iter()
        .filter(|(pos, _)| !app.research.steer_acked.contains(pos))
        .collect();
    let has_pending = !pending.is_empty();
    if has_pending {
        items.push(ListItem::new(Line::from(Span::styled(
            "queued steers — picked up at the next round boundary:",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))));
        for (_, s) in pending {
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", glyph::DOT),
                    Style::default().fg(app.theme.accent),
                ),
                Span::styled(s.clone(), dim),
            ])));
        }
        items.push(ListItem::new(Line::from("")));
    }
    if rows.is_empty() && !has_pending {
        items.push(ListItem::new(Line::from(Span::styled(
            "waiting for the first update…",
            dim,
        ))));
    } else {
        // Newest agents first, so the active synthesis/verifier/writer stays
        // visible after a full six-searcher fan-out.
        for content in rows.iter().rev() {
            let (label, detail) = content.split_once(':').unwrap_or((content.as_str(), ""));
            let detail = detail.trim();
            let (mark, color, detail) = if let Some(rest) = detail.strip_prefix("done —") {
                (glyph::OK, app.theme.success, rest.trim())
            } else if let Some(rest) = detail.strip_prefix("error —") {
                (glyph::FAIL, app.theme.error, rest.trim())
            } else if let Some(rest) = detail.strip_prefix("working —") {
                (glyph::RUNNING, app.theme.accent2, rest.trim())
            } else {
                (glyph::RING, app.theme.fg_dim, detail)
            };
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("{mark} "), Style::default().fg(color)),
                Span::styled(
                    label.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ])];
            // Multi-line details, e.g. critic gaps listed as questions, get one
            // indented row per line.
            for detail_line in detail.lines() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(detail_line.to_string(), dim),
                ]));
            }
            items.push(ListItem::new(Text::from(lines)));
        }
    }

    let inner = chrome::render_frame(
        f,
        area,
        chrome::input_title(
            app,
            "research agents · steer",
            &app.research.live_input,
            "type instruction · Enter send · Ctrl+X stop · Esc close",
        ),
        &app.theme,
        true,
        chrome::Tone::Normal,
    );
    let list = chrome::standard_list(items, &app.theme);
    f.render_widget(list, inner);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    let ctrl = key
        .modifiers
        .contains(crossterm::event::KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('x') if ctrl => {
            app.stop_research();
            app.popup = nexus_core::app::Popup::None;
        }
        KeyCode::Esc => app.popup = nexus_core::app::Popup::None,
        KeyCode::Enter => {
            let text = app.research.live_input.trim().to_string();
            app.research.live_input.clear();
            app.steer_research(&text);
        }
        KeyCode::Backspace => {
            app.research.live_input.pop();
        }
        // Unbound Ctrl/Alt chords must not type their letter into the steer.
        KeyCode::Char(c) if !super::is_chord(key) => app.research.live_input.push(c),
        _ => {}
    }
}
