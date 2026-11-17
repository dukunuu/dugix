//! One agent's work under review: its patch, and the project's check of it.
//! What the user reads before merging.

use std::cell::Cell;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use dugix_core::app::{AppCommand, FleetAction};
use dugix_core::fleet::{AgentInfo, Check};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::diff;
use crate::ui::style::SEP;

use super::super::chrome::{self, PopupFrame, hint, keys};

pub const DIFF: usize = 0;
pub const CHECK: usize = 1;
const TABS: &[&str] = &["Diff", "Check"];
/// Rows one `PageUp` or `PageDown` moves.
const PAGE: usize = 20;

pub struct Review {
    pub id: u64,
    pub tab: usize,
    pub scroll: usize,
    /// The patch as of opening, or the last reload.
    pub diff: String,
    /// The furthest the last frame could scroll.
    max_scroll: Cell<usize>,
}

/// Open agent `id`'s work on `tab`; an agent in the user's tree has none.
pub fn open(app: &mut AppView, id: u64, tab: usize) {
    match app.fleet.diff(id) {
        Ok(diff) => {
            app.fleet_view.review = Some(Review {
                id,
                tab,
                scroll: 0,
                diff,
                max_scroll: Cell::new(0),
            });
        }
        Err(error) => app.push_status(format!("fleet: {error:#}")),
    }
}

fn reload(app: &mut AppView) {
    let Some(id) = app.fleet_view.review.as_ref().map(|review| review.id) else {
        return;
    };
    match app.fleet.diff(id) {
        Ok(diff) => {
            if let Some(review) = app.fleet_view.review.as_mut() {
                review.diff = diff;
            }
        }
        Err(error) => app.push_status(format!("fleet: {error:#}")),
    }
}

pub fn render(f: &mut Frame, app: &AppView, review: &Review, agent: &AgentInfo) {
    let area = chrome::popup_area(app, f.area(), "fleet-review", super::TERMINAL);
    let hints = [
        hint(keys::TAB, if review.tab == DIFF { "check" } else { "diff" }),
        hint(keys::MOVE, "scroll"),
        hint("c", "check"),
        hint("m", "merge"),
        hint("r", "reload"),
        hint(keys::ESC, "back"),
    ];
    let mut count = vec![agent.state.label()];
    count.extend(agent.changes.map(dugix_core::fleet::Changes::label));
    count.extend(agent.check.as_ref().map(Check::label));
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: &format!("Review {}", agent.name()),
            count: &count.join(SEP),
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let inner = chrome::render_tabs(f, inner, TABS, review.tab, &app.theme);
    let width = usize::from(inner.width);
    let lines = if review.tab == DIFF {
        patch(&review.diff, width, &app.theme)
    } else {
        check(agent, width, &app.theme)
    };
    let height = usize::from(inner.height);
    let max_scroll = lines.len().saturating_sub(height);
    review.max_scroll.set(max_scroll);
    let scroll = review.scroll.min(max_scroll);
    f.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(scroll)
                .take(height)
                .collect::<Vec<_>>(),
        ),
        inner,
    );
}

/// The file summary, then each file's changes under its name.
fn patch(text: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let mut lines = Vec::new();
    // A leading newline lets the first file split like the rest.
    let text = format!("\n{text}");
    let mut files = text.split("\ndiff --git ");
    for line in files.next().unwrap_or_default().lines() {
        lines.extend(diff::escaped_rows(line, width, dim, dim));
    }
    for file in files {
        let (header, body) = file.split_once('\n').unwrap_or((file, ""));
        // `a/x b/x`, or `/dev/null x` for a new file.
        let name = header.rsplit_once(' ').map_or(header, |(_, new)| new);
        let name = name.strip_prefix("b/").unwrap_or(name);
        lines.push(Line::default());
        lines.extend(diff::escaped_rows(
            name,
            width,
            Style::default()
                .fg(theme.accent2)
                .add_modifier(Modifier::BOLD),
            dim,
        ));
        lines.extend(diff::expanded(body, width, theme));
    }
    lines
}

/// The last check's verdict and the end of its output.
fn check(agent: &AgentInfo, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let Some(check) = &agent.check else {
        return vec![Line::from(Span::styled(
            "Not checked since its last change. c runs the project's tests in its worktree.",
            dim,
        ))];
    };
    let verdict = match check.passed() {
        Some(true) => theme.success,
        Some(false) => theme.error,
        None => theme.accent2,
    };
    let mut lines = vec![
        Line::from(Span::styled(
            check.label(),
            Style::default().fg(verdict).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(format!("$ {}", check.command()), dim)),
        Line::default(),
    ];
    match check {
        Check::Running { .. } => lines.push(Line::from(Span::styled(
            "Its output shows here when it finishes.",
            dim,
        ))),
        Check::Done { tail, .. } => {
            for line in tail.lines() {
                lines.extend(diff::escaped_rows(
                    line,
                    width,
                    Style::default().fg(theme.fg),
                    dim,
                ));
            }
        }
    }
    lines
}

/// A key in the review. Commands go straight to the core: through the
/// composer they would reopen the board.
pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    let Some(id) = app.fleet_view.review.as_ref().map(|review| review.id) else {
        return Ok(());
    };
    if app.fleet.info(id).is_none() {
        // Its agent was stopped elsewhere.
        app.fleet_view.review = None;
        return Ok(());
    }
    let Some(review) = app.fleet_view.review.as_mut() else {
        return Ok(());
    };
    let max = review.max_scroll.get();
    match key.code {
        KeyCode::Esc => app.fleet_view.review = None,
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
            review.tab = (review.tab + 1) % TABS.len();
            review.scroll = 0;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            review.scroll = review.scroll.min(max).saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => review.scroll = (review.scroll + 1).min(max),
        KeyCode::PageUp => review.scroll = review.scroll.min(max).saturating_sub(PAGE),
        KeyCode::PageDown | KeyCode::Char(' ') => review.scroll = (review.scroll + PAGE).min(max),
        KeyCode::Home | KeyCode::Char('g') => review.scroll = 0,
        KeyCode::End | KeyCode::Char('G') => review.scroll = max,
        KeyCode::Char('r') => reload(app),
        KeyCode::Char('c') => {
            review.tab = CHECK;
            review.scroll = 0;
            app.core.execute(AppCommand::Fleet {
                action: FleetAction::Check { id },
            })?;
        }
        KeyCode::Char('m') => {
            app.core.execute(AppCommand::Fleet {
                action: FleetAction::Merge { id },
            })?;
            reload(app);
        }
        _ => {}
    }
    Ok(())
}
