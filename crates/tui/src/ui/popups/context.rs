use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app_view::AppView;
use dugix_core::app::Popup;

use super::chrome::{self, PopupFrame, hint, keys};

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;

/// Context breakdown popup (Ctrl+G): estimated tokens per bucket, plus what the
/// session has spent so far.
pub fn render(f: &mut Frame, app: &AppView) {
    let b = app.context_breakdown();
    let dim = Style::default().fg(app.theme.fg_dim);
    let total = b.system_tokens
        + b.memory_tokens
        + b.skills_tokens
        + b.tools_tokens
        + b.conversation_tokens;
    // Bars scale against the context window, or the total when it's unknown.
    let scale = b.limit.filter(|&l| l > 0).unwrap_or(total).max(1);

    let pct_of = |tok: u64| -> String {
        match b.limit.filter(|&l| l > 0) {
            Some(l) => format!(" ({}%)", tok * 100 / l),
            None => String::new(),
        }
    };
    let row = |label: &'static str, tok: u64, color: Color| -> Line<'static> {
        let filled = usize::try_from(tok * BAR_W as u64 / scale)
            .unwrap_or(BAR_W)
            .min(BAR_W);
        let filled = if tok > 0 { filled.max(1) } else { 0 };
        Line::from(vec![
            Span::styled(format!("{label:<13}"), Style::default().fg(app.theme.fg)),
            Span::styled("█".repeat(filled), Style::default().fg(color)),
            Span::styled(
                "░".repeat(BAR_W - filled),
                Style::default().fg(app.theme.border_dim),
            ),
            Span::styled(
                format!(" {:>6}", crate::ui::humanize(tok)),
                Style::default().fg(color),
            ),
            Span::styled(pct_of(tok), dim),
        ])
    };

    let mut lines = vec![
        row("System", b.system_tokens, app.theme.accent),
        row("Memory", b.memory_tokens, app.theme.accent2),
        row("Skills", b.skills_tokens, app.theme.warning),
        row("Tools", b.tools_tokens, app.theme.accent2),
        row("Conversation", b.conversation_tokens, app.theme.success),
    ];
    if b.compacted {
        lines.push(Line::from(Span::styled(
            "  this session has been compacted · v views/edits the digest",
            dim,
        )));
    }
    let limit_s = b
        .limit
        .map_or_else(|| "unknown window".to_string(), crate::ui::humanize);
    let count = format!("{} of {limit_s}", crate::ui::humanize(total));

    lines.extend(cache_lines(app));
    lines.extend(session_lines(app));

    let mut hints = Vec::new();
    if b.compacted {
        hints.push(hint("v", "digest"));
    }
    hints.push(hint(keys::ESC, "close"));
    let area = chrome::popup_area_fit(app, f.area(), "context", chrome::SMALL, lines.len());
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Context",
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    f.render_widget(Paragraph::new(lines), inner);
}

/// What the session has spent so far: cost and requests, then tokens.
fn session_lines(app: &AppView) -> Vec<Line<'static>> {
    let dim = Style::default().fg(app.theme.fg_dim);
    let label = Span::styled(
        "Session      ",
        Style::default()
            .fg(app.theme.fg)
            .add_modifier(Modifier::BOLD),
    );
    let Some(session) = app.session.as_ref().filter(|_| !app.incognito) else {
        return Vec::new();
    };
    let rows = match app.db.usage_for_session(&session.id) {
        Ok(rows) => rows,
        // A failed query says so rather than passing for an empty session.
        Err(e) => {
            return vec![
                Line::from(""),
                Line::from(vec![
                    label,
                    Span::styled(
                        format!("could not load usage: {e}"),
                        Style::default().fg(app.theme.error),
                    ),
                ]),
            ];
        }
    };
    let t = dugix_core::app::usage::SessionUsageTotals::from_rows(&rows);
    let cost = match t.cost {
        Some(c) if t.unknown_cost_requests > 0 => {
            format!("${c:.4} + {} unpriced", t.unknown_cost_requests)
        }
        Some(c) => format!("${c:.4}"),
        None if t.requests > 0 => "no cost reported".to_string(),
        None => "$0".to_string(),
    };
    let requests = match t.requests {
        1 => "1 request".to_string(),
        n => format!("{n} requests"),
    };
    let h = crate::ui::humanize;
    vec![
        Line::from(""),
        Line::from(vec![
            label,
            Span::styled(cost, Style::default().fg(app.theme.warning)),
            Span::styled(format!(" · {requests}"), dim),
        ]),
        Line::from(Span::styled(
            format!(
                "             {} in · {} cached · {} out",
                h(t.prompt_tokens),
                h(t.cache_read_tokens),
                h(t.completion_tokens)
            ),
            dim,
        )),
    ]
}

/// The turn's and the last request's cache hit rates, when either is known.
fn cache_lines(app: &AppView) -> Vec<Line<'static>> {
    let dim = Style::default().fg(app.theme.fg_dim);
    // The two figures can be labelled apart here; the status line shows only
    // the turn.
    let turn = app.meter.turn_cache;
    let mut lines = Vec::new();
    if turn.rate().is_some() || app.meter.last_cache_rate.is_some() {
        lines.push(Line::from(""));
        let pct = |rate: Option<f64>| {
            rate.map_or_else(|| "—".to_string(), |r| format!("{:.0}%", r * 100.0))
        };
        lines.push(Line::from(vec![
            Span::styled(
                "Cache        ",
                Style::default()
                    .fg(app.theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} this turn", pct(turn.rate())),
                Style::default().fg(app.theme.success),
            ),
            Span::styled(
                format!(" · {} last request", pct(app.meter.last_cache_rate)),
                dim,
            ),
        ]));
        if turn.is_partial() {
            lines.push(Line::from(Span::styled(
                format!(
                    "  {} request(s) this turn reported no cache accounting · excluded",
                    turn.unrated_requests
                ),
                dim,
            )));
        }
    }
    lines
}

/// Width of each bucket's bar, in cells.
const BAR_W: usize = 16;

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    let ctrl_g = key.code == KeyCode::Char('g')
        && key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL);
    if key.code == KeyCode::Esc || ctrl_g {
        app.popup = Popup::None;
    }
}
