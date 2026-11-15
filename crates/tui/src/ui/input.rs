//! The composer frame and the menus that float above it: slash commands
//! and `@` files.

// Casts here are on terminal-bounded values (dims, byte colors, glyph counts),
// never on unbounded user data.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, ListItem, ListState, Padding};

use super::style::glyph;
use super::{popups, to_color};
use crate::app_view::AppView;
use crate::composer::AtMatch;

/// The composer's vim mode in its border: dim while inserting, accent otherwise.
fn vim_mode(app: &AppView) -> Option<Span<'static>> {
    app.vim_enabled.then(|| {
        let style = if app.vim.mode() == crate::vim::Mode::Insert {
            Style::default().fg(app.theme.fg_dim)
        } else {
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD)
        };
        Span::styled(format!("{} ", app.vim.label()), style)
    })
}

/// Waiting messages shown at once; the rest are a count.
const QUEUE_ROWS: usize = 3;

/// Rows the queued-message strip takes: a header and up to [`QUEUE_ROWS`] messages.
pub(super) fn queue_height(app: &AppView) -> u16 {
    match app.queued_messages().len() {
        0 => 0,
        n => 1 + n.min(QUEUE_ROWS) as u16,
    }
}

/// The messages waiting for the running reply, above the composer.
pub(super) fn render_queue(f: &mut Frame, app: &AppView, area: Rect) {
    use super::style::SEP;
    let queued = app.queued_messages();
    if queued.is_empty() || area.height == 0 {
        return;
    }
    let dim = Style::default().fg(app.theme.fg_dim);
    let held = !app.viewing_stream();
    let mut header = format!("  {} {} queued", glyph::RING, queued.len());
    if held {
        header.push_str(SEP);
        header.push_str("held");
    }
    if !app.settings.hide_hints {
        header.push_str(SEP);
        header.push_str(if held {
            "Enter sends the next"
        } else {
            "sends after this reply"
        });
        header.push_str(SEP);
        header.push_str("Up edits the newest");
        header.push_str(SEP);
        header.push_str("/queue");
    }
    let width = usize::from(area.width);
    let mut lines = vec![Line::from(Span::styled(
        popups::chrome::truncate(&header, width),
        dim,
    ))];
    // The newest are the ones Up reaches, so they are the ones shown.
    let skipped = queued.len().saturating_sub(QUEUE_ROWS);
    for (index, text) in queued.iter().enumerate().skip(skipped) {
        let row = format!(
            "    {}. {}",
            index + 1,
            nexus_core::app::one_line(text, 400)
        );
        lines.push(Line::from(Span::styled(
            popups::chrome::truncate(&row, width),
            dim,
        )));
    }
    f.render_widget(ratatui::widgets::Paragraph::new(lines), area);
}

/// What is running, for the composer's border: the reply, compaction, other
/// chats, research, and background agents beside any of them.
fn activity_label(app: &AppView, hints: bool) -> Option<String> {
    // What's running shows even with hints hidden: it's state, not key help.
    let activity = if app.viewing_stream() {
        Some(if hints {
            "working · Esc to stop".to_string()
        } else {
            "working".into()
        })
    } else if app.is_compacting_current_session() {
        Some(format!("{} compacting…", glyph::RUNNING))
    } else if app.is_streaming() {
        let n = app.chat_task_count();
        Some(format!(
            "{} {n} chat{} running",
            glyph::RUNNING,
            if n == 1 { "" } else { "s" }
        ))
    } else {
        app.research
            .running
            .as_ref()
            .filter(|(id, _)| app.session.as_ref().is_none_or(|s| &s.id != id))
            .map(|(_, topic)| format!("{} researching: {topic}", glyph::RESEARCH))
    };
    // Background agents outlive the reply that started them, so they show
    // beside whatever else is running.
    match app.background_running() {
        0 => activity,
        n => {
            let agents = format!(
                "{} {n} background agent{}",
                glyph::RUNNING,
                if n == 1 { "" } else { "s" }
            );
            Some(activity.map_or(agents.clone(), |activity| {
                format!("{activity}{}{agents}", super::style::SEP)
            }))
        }
    }
}

pub(super) fn render_input(f: &mut Frame, app: &mut AppView, area: Rect) {
    let hints = !app.settings.hide_hints;
    let busy = app.is_streaming() || app.is_compacting_current_session();
    let activity = activity_label(app, hints);
    // The border takes the spinner's color while something runs.
    let border_color = if busy {
        to_color(app.spinner_color())
    } else {
        app.theme.border_dim
    };
    let mut title = vec![Span::styled(
        format!(" {} ", glyph::YOU),
        Style::default()
            .fg(if busy { border_color } else { app.theme.accent })
            .add_modifier(Modifier::BOLD),
    )];
    if app.shell_draft() {
        title.push(Span::styled(
            format!(
                "{} ",
                nexus_core::usershell::display_dir(&app.user_command_cwd())
            ),
            Style::default().fg(app.theme.fg_dim),
        ));
    }
    if let Some(activity) = activity {
        title.push(Span::styled(
            format!("{activity} "),
            Style::default().fg(app.theme.fg_dim),
        ));
    }
    title.extend(vim_mode(app));
    let name = app.session.as_ref().map(|s| s.title.clone());
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::horizontal(1))
        .title_top(Line::from(title));
    if let Some(name) = name {
        block = block.title_top(
            Line::from(Span::styled(
                format!(" {} ", popups::chrome::truncate(&name, 40)),
                Style::default().fg(app.theme.fg_dim),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    app.input_inner = inner; // mouse click -> cursor mapping
    f.render_widget(block, area);
    app.input
        .set_selection_style(Style::default().bg(app.theme.selection).fg(app.theme.fg));
    app.input
        .set_placeholder_style(Style::default().fg(app.theme.fg_dim));
    app.input.set_placeholder_text(if app.settings.hide_hints {
        "Ask anything"
    } else {
        "Ask anything · / commands · @ files · F1 help"
    });
    let syntax = crate::editor::composer_syntax(&app.input_text());
    let decor = crate::editor::Decor {
        syntax: Some(syntax),
        colors: crate::ui::style::SyntaxColors::from_theme(&app.theme),
        vim: app.vim_enabled.then_some(&app.vim),
        selection: Style::default().bg(app.theme.selection).fg(app.theme.fg),
    };
    crate::editor::decorate(&mut app.input, decor);
    f.render_widget(&app.input, inner);
    // Terminal IMEs draw preedit text at the real cursor, which otherwise
    // trails the last changed cell; park it on the caret the textarea drew.
    if app.popup == nexus_core::app::Popup::None
        && let Some(caret) = take_caret(f, inner)
    {
        f.set_cursor_position(caret);
    }
}

/// The textarea's caret cell inside `area` (it draws the caret `REVERSED`),
/// with that styling removed so the terminal cursor alone marks it.
fn take_caret(f: &mut Frame, area: Rect) -> Option<ratatui::layout::Position> {
    let buffer = f.buffer_mut();
    let caret = area
        .positions()
        .find(|&p| buffer[p].modifier.contains(Modifier::REVERSED))?;
    buffer[caret].modifier.remove(Modifier::REVERSED);
    Some(caret)
}

/// Slash-command autocomplete: a fuzzy-ranked list floating just above the
/// input box.
pub(super) fn render_command_popup(f: &mut Frame, app: &AppView, input_area: Rect) {
    if let Some(state) = &app.shell_state {
        let items = state
            .candidates
            .iter()
            .map(|candidate| {
                ListItem::new(Span::styled(
                    candidate.clone(),
                    Style::default().fg(app.theme.accent),
                ))
            })
            .collect();
        floating_menu(
            f,
            app,
            input_area,
            ("shell", "Tab/Enter insert · Esc cancel"),
            items,
            state.selected,
        );
        return;
    }
    let matches = app.command_matches();
    if matches.is_empty() {
        return;
    }
    let name_w = matches
        .iter()
        .map(|c| crate::ui::style::width(c.name()))
        .max()
        .unwrap_or(0)
        + 1;
    let desc_w = usize::from(MENU_W).saturating_sub(name_w + 8);
    let items: Vec<ListItem> = matches
        .iter()
        .map(|c| {
            let name = format!("/{}", c.name());
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{name:<name_w$}"),
                    Style::default().fg(app.theme.accent),
                ),
                Span::raw("  "),
                Span::styled(
                    popups::chrome::truncate(c.desc(), desc_w),
                    Style::default().fg(app.theme.fg_dim),
                ),
            ]))
        })
        .collect();
    floating_menu(
        f,
        app,
        input_area,
        ("commands", "Tab fill · Enter run"),
        items,
        app.command_selected(),
    );
}

/// Widest a floating composer menu gets.
const MENU_W: u16 = 76;

/// A rounded menu floating just above the composer's left edge, at most ten
/// rows tall.
fn floating_menu(
    f: &mut Frame,
    app: &AppView,
    input_area: Rect,
    (title, hint): (&str, &str),
    items: Vec<ListItem<'static>>,
    selected: usize,
) {
    let rows = u16::try_from(items.len()).unwrap_or(u16::MAX).min(10);
    let h = (rows + 2).min(input_area.y);
    if h < 3 {
        return;
    }
    let area = Rect {
        x: input_area.x,
        y: input_area.y - h,
        width: input_area.width.min(MENU_W),
        height: h,
    };
    let hint = if app.settings.hide_hints { "" } else { hint };
    let block = popups::chrome::hinted_block(
        Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(app.theme.fg_dim),
        )),
        hint,
        app,
        false,
        popups::chrome::Tone::Normal,
        area.width,
    );
    let mut state = ListState::default();
    state.select(Some(selected.min(items.len().saturating_sub(1))));
    let list = popups::chrome::standard_list(items, &app.theme).block(block);
    f.render_widget(Clear, area);
    f.render_stateful_widget(list, area, &mut state);
}

/// `@` file autocomplete over agent-workspace paths and space files.
pub(super) fn render_at_popup(f: &mut Frame, app: &AppView, input_area: Rect) {
    let Some((ref matches, selected, _)) = app.at_state else {
        return;
    };
    let row_w = usize::from(input_area.width.min(MENU_W)).saturating_sub(5);
    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = matches
        .iter()
        .map(|m| {
            let (name, meta) = match m {
                AtMatch::Project(path) => match path.rsplit_once('/') {
                    Some((dir, name)) => (name, format!("  {dir}")),
                    None => (path.as_str(), String::new()),
                },
                AtMatch::Space(file) => (
                    file.name.as_str(),
                    format!(
                        "  {}  {}",
                        nexus_core::app::human_size(file.size.unsigned_abs()),
                        file.status
                    ),
                ),
            };
            let name = popups::chrome::truncate_middle(name, row_w);
            let meta = popups::chrome::truncate_middle(
                &meta,
                row_w.saturating_sub(crate::ui::style::width(&name)),
            );
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(meta, dim),
            ]))
        })
        .collect();
    floating_menu(
        f,
        app,
        input_area,
        ("files", "Tab insert · Esc cancel"),
        items,
        selected,
    );
}
