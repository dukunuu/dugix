//! `/agents` (or Ctrl+↑ while a turn's sub-agents run): the conversation's
//! `task` sub-agents, each with a live transcript.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::{Popup, SubagentRun, SubagentStatus};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{ListItem, ListState, Paragraph};

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::style::{SEP, glyph};

use super::chrome;

#[derive(Default)]
pub struct SubagentsPopup {
    pub session_id: String,
    /// Runs read from the cache when the popup opened.
    pub saved: Vec<SubagentRun>,
    pub selected: usize,
    pub open: Option<String>,
    /// Transcript lines scrolled up from the bottom; 0 follows the end.
    pub scroll: usize,
    pub tools_open: bool,
    pub hide_reasoning: bool,
}

/// Open the view for the current conversation, on its newest run.
pub fn open(app: &mut AppView) {
    let Some(session) = app.session.as_ref().map(|s| s.id.clone()) else {
        app.push_status("no conversation open".to_string());
        return;
    };
    let saved = app.db.subagent_runs(&session).unwrap_or_default();
    app.subagents = SubagentsPopup {
        session_id: session,
        saved,
        ..SubagentsPopup::default()
    };
    app.popup = Popup::Subagents;
}

/// Whether the open conversation has sub-agents this process ran.
#[must_use]
pub fn has_live(app: &AppView) -> bool {
    app.session.as_ref().is_some_and(|session| {
        app.subagent_runs
            .iter()
            .any(|run| run.session_id == session.id)
    })
}

fn runs(app: &AppView) -> Vec<SubagentRun> {
    let mut runs = app.with_live_subagents(app.subagents.saved.clone(), &app.subagents.session_id);
    runs.reverse();
    runs
}

fn tokens_label(tokens: u64) -> String {
    if tokens >= 1000 {
        #[allow(clippy::cast_precision_loss)] // display only
        let k = tokens as f64 / 1000.0;
        format!("{k:.1}k tokens")
    } else {
        format!("{tokens} tokens")
    }
}

fn cost_label(cost: Option<f64>) -> String {
    cost.map_or_else(|| "cost unknown".into(), |cost| format!("${cost:.4}"))
}

fn status_mark(run: &SubagentRun, theme: &Theme) -> (&'static str, ratatui::style::Color) {
    match run.status {
        SubagentStatus::Running => (glyph::RUNNING, theme.accent2),
        SubagentStatus::Done => (glyph::OK, theme.success),
        SubagentStatus::Failed => (glyph::FAIL, theme.error),
    }
}

/// `helper/fast · read-only · 4/20 steps · 12.3k tokens · $0.0123 · 18s`
fn stats(run: &SubagentRun) -> String {
    [
        run.model.clone(),
        if run.writable {
            "workspace"
        } else {
            "read-only"
        }
        .to_string(),
        format!("{}/{} steps", run.steps, run.max_steps),
        tokens_label(run.tokens),
        cost_label(run.cost),
        format!("{:.0}s", run.secs),
    ]
    .join(SEP)
}

fn list_rows(runs: &[SubagentRun], theme: &Theme, width: usize) -> Vec<ListItem<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    runs.iter()
        .map(|run| {
            let (mark, color) = status_mark(run, theme);
            ListItem::new(Text::from(vec![
                Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::styled(format!("#{} ", run.number), dim),
                    Span::styled(
                        chrome::truncate(run.title(), width.saturating_sub(8)),
                        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        "  {}",
                        chrome::truncate(&stats(run), width.saturating_sub(2))
                    ),
                    dim,
                )),
            ]))
        })
        .collect()
}

/// The whole transcript of one run, wrapped to `width`.
pub fn transcript(
    run: &SubagentRun,
    view: &SubagentsPopup,
    settings: &nexus_core::app::Settings,
    files_dir: &std::path::Path,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let settings = nexus_core::app::Settings {
        show_reasoning: !view.hide_reasoning,
        ..settings.clone()
    };
    let mut out = vec![crate::ui::style::section(theme, "task", width)];
    out.extend(crate::ui::cards::tool_markdown(&run.prompt, width, theme));
    out.push(Line::default());
    let mut image_at_line = Vec::new();
    let mut image_cache = HashMap::new();
    for row in &run.rows {
        match row.role.as_str() {
            "tool_call" => crate::ui::cards::push_tool_call_with_rows(
                &mut out,
                &row.content,
                view.tools_open,
                &settings,
                width,
                theme,
                crate::ui::cards::CardEnv {
                    files_dir,
                    studio_linked: false,
                    image_at_line: &mut image_at_line,
                    image_cache: &mut image_cache,
                },
                None,
            ),
            "assistant" => {
                crate::ui::cards::push_round_preamble(
                    &mut out,
                    &[row.reasoning.clone().unwrap_or_default()],
                    "",
                    &settings,
                    width,
                    theme,
                );
                out.push(Line::from(Span::styled(
                    format!("{} answer", glyph::ASSISTANT),
                    Style::default()
                        .fg(theme.accent2)
                        .add_modifier(Modifier::BOLD),
                )));
                out.extend(crate::ui::cards::tool_markdown(&row.content, width, theme));
                out.push(Line::default());
            }
            _ => {
                out.push(Line::from(Span::styled(
                    format!("{} {}", glyph::FAIL, row.content),
                    Style::default().fg(theme.error),
                )));
                out.push(Line::default());
            }
        }
    }
    if run.status == SubagentStatus::Running {
        crate::ui::cards::push_round_preamble(
            &mut out,
            std::slice::from_ref(&run.live_reasoning),
            "",
            &settings,
            width,
            theme,
        );
        if !run.live_content.trim().is_empty() {
            out.extend(crate::ui::cards::tool_markdown(
                &run.live_content,
                width,
                theme,
            ));
        }
        out.push(Line::from(Span::styled(
            format!(
                "{} working{SEP}step {}/{}",
                glyph::RUNNING,
                (run.steps + 1).min(run.max_steps.max(1)),
                run.max_steps
            ),
            Style::default().fg(theme.accent2),
        )));
    }
    if let Some(review) = &run.review {
        out.push(crate::ui::style::section(theme, "review", width));
        let (head, diff) = review.split_once('\n').unwrap_or((review, ""));
        out.extend(crate::ui::diff::escaped_rows(head, width, dim, dim));
        if !diff.trim().is_empty() {
            out.extend(crate::ui::diff::expanded(diff, width, theme));
        }
    }
    out.push(Line::from(Span::styled(stats(run), dim)));
    out
}

pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "agents", (chrome::WIDE.0, chrome::TALL.1));
    let runs = runs(app);
    let open = app
        .subagents
        .open
        .as_ref()
        .and_then(|id| runs.iter().find(|run| &run.id == id));
    if let Some(run) = open {
        let title = chrome::popup_title(
            app,
            format!(
                "sub-agent #{}{SEP}{}",
                run.number,
                chrome::truncate(run.title(), 40)
            ),
        );
        let hint = format!("↑↓ scroll{SEP}t tools{SEP}r reasoning{SEP}←→ runs{SEP}esc back");
        let inner = chrome::render_hinted(f, area, title, &hint, app, true, chrome::Tone::Normal);
        let lines = transcript(
            run,
            &app.subagents,
            &app.settings,
            &app.space.files_dir(&app.active_space.name),
            usize::from(inner.width.max(1)),
            &app.theme,
        );
        let height = usize::from(inner.height);
        let bottom = lines.len().saturating_sub(height);
        let top = bottom.saturating_sub(app.subagents.scroll);
        f.render_widget(
            Paragraph::new(lines).scroll((u16::try_from(top).unwrap_or(u16::MAX), 0)),
            inner,
        );
        return;
    }
    let total = runs.len();
    let spent: Option<f64> = runs.iter().filter_map(|run| run.cost).reduce(|a, b| a + b);
    let hint = if total == 0 {
        "esc close".to_string()
    } else {
        format!(
            "{}{}↑↓{SEP}enter open transcript{SEP}esc close",
            chrome::count_hint(total, "sub-agent"),
            spent.map_or_else(String::new, |cost| format!("${cost:.4} total{SEP}")),
        )
    };
    let inner = chrome::render_hinted(
        f,
        area,
        chrome::popup_title(app, "sub-agents"),
        &hint,
        app,
        true,
        chrome::Tone::Normal,
    );
    if total == 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No sub-agents in this conversation yet. In agent mode the model starts them with its task tool.",
                Style::default().fg(app.theme.fg_dim),
            )))
            .wrap(ratatui::widgets::Wrap { trim: true }),
            inner,
        );
        return;
    }
    let width = usize::from(inner.width.saturating_sub(2));
    let list = chrome::standard_list(list_rows(&runs, &app.theme, width), &app.theme);
    let mut state = ListState::default();
    state.select(Some(app.subagents.selected.min(total - 1)));
    chrome::render_list(f, list, &mut state, inner, total, 2, app);
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    let runs = runs(app);
    let total = runs.len();
    let view = &mut app.subagents;
    if view.open.is_some() {
        let page = 10;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => view.open = None,
            KeyCode::Up | KeyCode::Char('k') => view.scroll += 1,
            KeyCode::Down | KeyCode::Char('j') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::PageUp => view.scroll += page,
            KeyCode::PageDown => view.scroll = view.scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => view.scroll = usize::MAX / 2,
            KeyCode::End | KeyCode::Char('G') => view.scroll = 0,
            KeyCode::Char('t') => view.tools_open = !view.tools_open,
            KeyCode::Char('r') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                view.hide_reasoning = !view.hide_reasoning;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') if total > 0 => {
                // The list is newest first.
                let newer = matches!(key.code, KeyCode::Right | KeyCode::Char('l'));
                view.selected = if newer {
                    view.selected.saturating_sub(1)
                } else {
                    (view.selected + 1).min(total - 1)
                };
                view.open = Some(runs[view.selected].id.clone());
                view.scroll = 0;
            }
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.popup = Popup::None,
        KeyCode::Up | KeyCode::Char('k') => view.selected = view.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') if total > 0 => {
            view.selected = (view.selected + 1).min(total - 1);
        }
        KeyCode::Home => view.selected = 0,
        KeyCode::End => view.selected = total.saturating_sub(1),
        KeyCode::Enter if total > 0 => {
            view.open = Some(runs[view.selected.min(total - 1)].id.clone());
            view.scroll = 0;
        }
        _ => {}
    }
}
