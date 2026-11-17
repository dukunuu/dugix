//! `/fleet`: the board of external agent CLIs — who is working, who is blocked
//! on you, who is done — and, on Enter, an agent's own terminal to type into.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dugix_core::app::{AppCommand, FleetAction, Popup};
use dugix_core::fleet::{AgentInfo, Color as TermColor, Kind, Screen, State};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::style::{SEP, glyph};

use super::chrome::{self, PopupFrame, ctrl, hint, keys};

mod review;

/// The board is a list; an attached agent is its own app and gets the window.
const TERMINAL: (u16, u16) = (100, 100);
/// Rows one Shift+PageUp looks back.
const SCROLL_STEP: usize = 10;

#[derive(Default)]
pub struct FleetPopup {
    pub selected: usize,
    /// The agent whose terminal is open; keys go to it.
    pub attached: Option<u64>,
    /// A follow-up being typed for the selected agent.
    pub steer: Option<String>,
    pub confirm_stop: Option<u64>,
    /// Rows scrolled back in the attached terminal; 0 follows its output.
    pub scroll: usize,
    /// The new-agent form, when it is open.
    pub launcher: Option<Launcher>,
    /// An agent's diff and check, when one is being reviewed.
    pub review: Option<review::Review>,
}

/// The new-agent form: which CLI, and optionally what to start it on, on
/// which model, and whether it gets its own worktree.
pub struct Launcher {
    /// Index into [`Kind::ALL`].
    pub kind: usize,
    pub job: String,
    /// In the CLI's own naming; empty is its default.
    pub model: String,
    /// Typing goes to the model field instead of the job.
    pub on_model: bool,
    /// Its own git worktree and branch, where there is a repository.
    pub isolate: bool,
    /// Why the last start failed.
    pub error: Option<String>,
}

impl Default for Launcher {
    fn default() -> Self {
        Self {
            kind: 0,
            job: String::new(),
            model: String::new(),
            on_model: false,
            isolate: true,
            error: None,
        }
    }
}

/// Open the board; with no agents yet, straight on the new-agent form.
pub fn open(app: &mut AppView) {
    app.fleet_view = FleetPopup {
        selected: app.fleet_view.selected,
        launcher: app.fleet.list(None).is_empty().then(Launcher::default),
        ..FleetPopup::default()
    };
    app.popup = Popup::Fleet;
}

/// Open the newest agent's terminal: what starting one leads to.
pub fn attach_newest(app: &mut AppView) {
    open(app);
    app.fleet_view.launcher = None;
    app.fleet_view.attached = app.fleet.list(None).last().map(|agent| agent.id);
}

/// Newest first, with the agents waiting on the user on top.
fn rows(app: &AppView) -> Vec<AgentInfo> {
    let mut rows = app.fleet.list(None);
    rows.reverse();
    rows.sort_by_key(|agent| agent.state != State::Blocked);
    rows
}

fn selected(app: &AppView, rows: &[AgentInfo]) -> Option<AgentInfo> {
    rows.get(app.fleet_view.selected.min(rows.len().saturating_sub(1)))
        .cloned()
}

fn mark(state: State, theme: &Theme) -> (&'static str, Color) {
    match state {
        State::Working => (glyph::RUNNING, theme.accent2),
        State::Blocked => (glyph::WARN, theme.warning),
        State::Idle => (glyph::RING, theme.fg_dim),
        State::Exited(0) => (glyph::OK, theme.success),
        State::Exited(_) => (glyph::FAIL, theme.error),
    }
}

fn age(agent: &AgentInfo) -> String {
    let seconds = agent.uptime.as_secs();
    format!("{}m{:02}s", seconds / 60, seconds % 60)
}

fn list_rows(rows: &[AgentInfo], theme: &Theme, width: usize) -> Vec<ListItem<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    rows.iter()
        .map(|agent| {
            let (glyph, color) = mark(agent.state, theme);
            let name = agent.name();
            let mut detail = vec![agent.state.label(), age(agent)];
            if let Some(branch) = &agent.branch {
                detail.push(branch.clone());
            }
            if let Some(changes) = agent.changes {
                detail.push(changes.label());
            }
            if let Some(check) = &agent.check {
                detail.push(check.label());
            }
            detail.push(agent.last_line.clone());
            let detail = detail.join(SEP);
            ListItem::new(Text::from(vec![
                Line::from(vec![
                    Span::styled(format!("{glyph} "), Style::default().fg(color)),
                    Span::styled(format!("{name} "), Style::default().fg(theme.accent2)),
                    Span::styled(
                        chrome::truncate(&agent.title, width.saturating_sub(name.len() + 4)),
                        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    format!("  {}", chrome::truncate(&detail, width.saturating_sub(2))),
                    if agent.state == State::Blocked {
                        Style::default().fg(theme.warning)
                    } else {
                        dim
                    },
                )),
            ]))
        })
        .collect()
}

pub fn render(f: &mut Frame, app: &AppView) {
    if let Some(agent) = app.fleet_view.attached.and_then(|id| app.fleet.info(id)) {
        render_terminal(f, app, &agent);
    } else if let Some((review, agent)) = app
        .fleet_view
        .review
        .as_ref()
        .and_then(|review| Some((review, app.fleet.info(review.id)?)))
    {
        review::render(f, app, review, &agent);
    } else if let Some(launcher) = &app.fleet_view.launcher {
        render_launcher(f, app, launcher);
    } else {
        render_board(f, app);
    }
}

fn render_launcher(f: &mut Frame, app: &AppView, launcher: &Launcher) {
    let area = chrome::popup_area(app, f.area(), "fleet", chrome::WIDE);
    let hints = [
        hint(keys::SIDE, "agent"),
        hint(keys::MOVE, if launcher.on_model { "job" } else { "model" }),
        hint(ctrl('t'), "worktree"),
        hint(keys::ENTER, "start"),
        hint(keys::ESC, "cancel"),
    ];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "New agent",
            count: "",
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let inner = chrome::render_tabs(f, inner, Kind::NAMES, launcher.kind, &app.theme);
    let inner = chrome::render_field(
        f,
        inner,
        glyph::YOU,
        &launcher.job,
        "what should it do? (Enter alone just opens it)",
        !launcher.on_model,
        &app.theme,
    );
    let inner = chrome::render_field(
        f,
        inner,
        glyph::YOU,
        &launcher.model,
        "model (empty: its default)",
        launcher.on_model,
        &app.theme,
    );
    let place = if launcher.isolate {
        "on its own branch and worktree when this is a git repository, working without asking in its CLI's safe auto mode; its edits stay apart from yours until you review them with d and merge with m"
    } else {
        "directly in your tree, asking before it acts, where its edits land as it makes them"
    };
    let (text, color) = match &launcher.error {
        Some(error) => (error.clone(), app.theme.error),
        None => (
            format!(
                "Opens {} in its own terminal, {place}. You type straight into it, and Ctrl+] brings you back to the board. It works under its own login and permissions.",
                Kind::NAMES[launcher.kind]
            ),
            app.theme.fg_dim,
        ),
    };
    f.render_widget(
        ratatui::widgets::Paragraph::new(text)
            .style(Style::default().fg(color))
            .wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

fn render_board(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "fleet", chrome::WIDE);
    let rows = rows(app);
    let view = &app.fleet_view;
    let stopping = view
        .confirm_stop
        .and_then(|id| rows.iter().find(|agent| agent.id == id));
    let (title, hints) = if stopping.is_some() {
        (
            "Stop agent?",
            vec![hint(ctrl('d'), "stop"), hint(keys::ESC, "cancel")],
        )
    } else if view.steer.is_some() {
        (
            "Fleet",
            vec![hint(keys::ENTER, "send"), hint(keys::ESC, "cancel")],
        )
    } else {
        (
            "Fleet",
            vec![
                hint(keys::MOVE, ""),
                hint(keys::ENTER, "open"),
                hint("d", "review"),
                hint("n", "new"),
                hint("s", "steer"),
                hint(ctrl('d'), "stop"),
                hint(keys::ESC, "close"),
            ],
        )
    };
    let blocked = app.fleet.blocked();
    let count = if blocked > 0 {
        format!("{} running{SEP}{blocked} need you", app.fleet.live())
    } else {
        format!("{} running", app.fleet.live())
    };
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title,
            count: &count,
            hints: &hints,
            tone: if stopping.is_some() {
                chrome::Tone::Danger
            } else {
                chrome::Tone::Normal
            },
            focused: true,
        },
        app,
    );
    let chosen = selected(app, &rows);
    let detail = if let Some(agent) = stopping {
        format!("{} will be stopped and its terminal closed.", agent.name())
    } else if let (Some(text), Some(agent)) = (&view.steer, &chosen) {
        format!("tell {} {} {text}▏", agent.name(), glyph::YOU)
    } else if rows.is_empty() {
        String::new()
    } else {
        "Agents run under their own permissions and stop when dugix exits; an unmerged branch keeps its work.".into()
    };
    let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
    chrome::render_detail(f, detail_area, &detail, &app.theme);
    let width = usize::from(list_area.width.saturating_sub(3));
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(view.selected.min(rows.len() - 1)));
    }
    chrome::render_list(
        f,
        chrome::standard_list(list_rows(&rows, &app.theme, width), &app.theme),
        &mut state,
        list_area,
        rows.len(),
        2,
        app,
    );
    if rows.is_empty() {
        chrome::render_empty(f, list_area, "No agents. n starts one.", &app.theme);
    }
}

fn render_terminal(f: &mut Frame, app: &AppView, agent: &AgentInfo) {
    let area = chrome::popup_area(app, f.area(), "fleet-term", TERMINAL);
    let live = agent.state.live();
    let hints = if live {
        vec![hint("Shift+PgUp", "scroll"), hint(ctrl(']'), "detach")]
    } else {
        vec![hint(keys::ESC, "back")]
    };
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: &format!("{}{SEP}{}", agent.name(), agent.state.label()),
            count: &agent.title,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    app.fleet.resize(agent.id, inner.height, inner.width);
    let cursor = app
        .fleet
        .with_screen(agent.id, |screen| draw_screen(f, inner, screen, &app.theme))
        .flatten();
    if let Some(cursor) = cursor.filter(|_| live && app.fleet_view.scroll == 0) {
        f.set_cursor_position(cursor);
    }
}

const fn color(color: TermColor, default: Color) -> Color {
    match color {
        TermColor::Default => default,
        TermColor::Idx(index) => Color::Indexed(index),
        TermColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Copy the agent's screen into `area`, returning where its cursor shows.
fn draw_screen(f: &mut Frame, area: Rect, screen: &Screen, theme: &Theme) -> Option<Position> {
    let buffer = f.buffer_mut();
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::default()
                .fg(color(cell.fgcolor(), theme.fg))
                .bg(color(cell.bgcolor(), Color::Reset));
            for (on, modifier) in [
                (cell.bold(), Modifier::BOLD),
                (cell.dim(), Modifier::DIM),
                (cell.italic(), Modifier::ITALIC),
                (cell.underline(), Modifier::UNDERLINED),
                (cell.inverse(), Modifier::REVERSED),
            ] {
                if on {
                    style = style.add_modifier(modifier);
                }
            }
            if let Some(target) = buffer.cell_mut((area.x + col, area.y + row)) {
                target.set_symbol(if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                });
                target.set_style(style);
            }
        }
    }
    let (row, col) = screen.cursor_position();
    (!screen.hide_cursor() && row < area.height && col < area.width)
        .then(|| Position::new(area.x + col, area.y + row))
}

/// Whether keys and pastes belong to an attached, running agent.
#[must_use]
pub fn captures(app: &AppView) -> bool {
    app.popup == Popup::Fleet
        && app
            .fleet_view
            .attached
            .and_then(|id| app.fleet.info(id))
            .is_some_and(|agent| agent.state.live())
}

/// Forward a paste to the attached agent; `false` when none is attached.
pub fn paste(app: &mut AppView, text: &str) -> bool {
    if !captures(app) {
        return false;
    }
    if let Some(id) = app.fleet_view.attached
        && let Err(error) = app.fleet.paste(id, text)
    {
        app.push_status(format!("fleet: {error:#}"));
    }
    true
}

/// The bytes a terminal sends for `key`; `application_cursor` is the mode the
/// program asked for, which changes what the arrows send.
#[must_use]
pub fn encode_key(key: KeyEvent, application_cursor: bool) -> Vec<u8> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let arrow = |letter: char| {
        if application_cursor {
            format!("\x1bO{letter}")
        } else {
            format!("\x1b[{letter}")
        }
    };
    let mut bytes = match key.code {
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            ' ' | '@' => vec![0],
            c @ 'a'..='z' => vec![c as u8 & 0x1f],
            '[' => vec![0x1b],
            '\\' => vec![0x1c],
            ']' => vec![0x1d],
            '^' => vec![0x1e],
            '_' | '/' => vec![0x1f],
            _ => Vec::new(),
        },
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // A newline without submitting, as the agent CLIs read it.
        KeyCode::Enter if shift || alt => return b"\x1b\r".to_vec(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => arrow('A').into_bytes(),
        KeyCode::Down => arrow('B').into_bytes(),
        KeyCode::Right => arrow('C').into_bytes(),
        KeyCode::Left => arrow('D').into_bytes(),
        KeyCode::Home => arrow('H').into_bytes(),
        KeyCode::End => arrow('F').into_bytes(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(n @ 1..=4) => vec![0x1b, b'O', b'P' + n - 1],
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            format!("\x1b[{code}~").into_bytes()
        }
        _ => Vec::new(),
    };
    if alt && !bytes.is_empty() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    if let Some(id) = app.fleet_view.attached {
        terminal_key(app, id, key);
        return Ok(());
    }
    if app.fleet_view.launcher.is_some() {
        launcher_key(app, key);
        return Ok(());
    }
    if app.fleet_view.review.is_some() {
        return review::handle_key(app, key);
    }
    let rows = rows(app);
    let chosen = selected(app, &rows);
    if let Some(id) = app.fleet_view.confirm_stop {
        match super::classify_confirm_delete_key(key) {
            Some(super::ConfirmDeleteAction::Yes) => {
                app.fleet_view.confirm_stop = None;
                app.execute(AppCommand::Fleet {
                    action: FleetAction::Stop { id },
                })?;
            }
            Some(super::ConfirmDeleteAction::No) => app.fleet_view.confirm_stop = None,
            None => {}
        }
        return Ok(());
    }
    if let Some(mut text) = app.fleet_view.steer.take() {
        match super::classify_edit_key(key) {
            Some(super::EditAction::Cancel) => {}
            Some(super::EditAction::Save) => {
                if let Some(agent) = chosen.filter(|_| !text.trim().is_empty()) {
                    app.execute(AppCommand::Fleet {
                        action: FleetAction::Send { id: agent.id, text },
                    })?;
                }
            }
            Some(super::EditAction::Backspace) => {
                text.pop();
                app.fleet_view.steer = Some(text);
            }
            Some(super::EditAction::Push(c)) => {
                text.push(c);
                app.fleet_view.steer = Some(text);
            }
            None => app.fleet_view.steer = Some(text),
        }
        return Ok(());
    }
    let last = rows.len().saturating_sub(1);
    match key.code {
        KeyCode::Esc => app.popup = Popup::None,
        KeyCode::Up | KeyCode::Char('k') => {
            app.fleet_view.selected = app.fleet_view.selected.min(last).saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.fleet_view.selected = app.fleet_view.selected.saturating_add(1).min(last);
        }
        KeyCode::Home => app.fleet_view.selected = 0,
        KeyCode::End => app.fleet_view.selected = last,
        KeyCode::Enter => {
            app.fleet_view.attached = chosen.map(|agent| agent.id);
            app.fleet_view.scroll = 0;
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.fleet_view.confirm_stop = chosen.map(|agent| agent.id);
        }
        KeyCode::Char('n') => app.fleet_view.launcher = Some(Launcher::default()),
        KeyCode::Char('s') if chosen.as_ref().is_some_and(|agent| agent.state.live()) => {
            app.fleet_view.steer = Some(String::new());
        }
        KeyCode::Char('d') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(agent) = chosen {
                review::open(app, agent.id, review::DIFF);
            }
        }
        KeyCode::Char('c') => {
            if let Some(agent) = chosen {
                app.execute(AppCommand::Fleet {
                    action: FleetAction::Check { id: agent.id },
                })?;
            }
        }
        KeyCode::Char('m') => {
            if let Some(agent) = chosen {
                app.execute(AppCommand::Fleet {
                    action: FleetAction::Merge { id: agent.id },
                })?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// A key in the new-agent form: arrows pick the CLI, text is the job, Enter
/// starts it and opens its terminal.
fn launcher_key(app: &mut AppView, key: KeyEvent) {
    let Some(launcher) = app.fleet_view.launcher.as_mut() else {
        return;
    };
    let kinds = Kind::ALL.len();
    match key.code {
        KeyCode::Left | KeyCode::BackTab => launcher.kind = (launcher.kind + kinds - 1) % kinds,
        KeyCode::Right | KeyCode::Tab => launcher.kind = (launcher.kind + 1) % kinds,
        KeyCode::Esc => {
            app.fleet_view.launcher = None;
            // Nothing to go back to: the form was the whole view.
            if app.fleet.list(None).is_empty() {
                app.popup = Popup::None;
            }
        }
        KeyCode::Up | KeyCode::Down => launcher.on_model = !launcher.on_model,
        KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            launcher.isolate = !launcher.isolate;
        }
        KeyCode::Enter => {
            let model = launcher.model.trim();
            let action = FleetAction::Start {
                kind: Kind::ALL[launcher.kind],
                prompt: launcher.job.trim().to_string(),
                model: (!model.is_empty()).then(|| model.to_string()),
                isolate: launcher.isolate,
            };
            match app.core.execute(AppCommand::Fleet { action }) {
                Ok(()) => attach_newest(app),
                Err(error) => {
                    if let Some(launcher) = app.fleet_view.launcher.as_mut() {
                        launcher.error = Some(format!("{error:#}"));
                    }
                }
            }
        }
        KeyCode::Backspace => {
            if launcher.on_model {
                launcher.model.pop();
            } else {
                launcher.job.pop();
            }
        }
        KeyCode::Char(c) if !super::is_chord(key) => {
            if launcher.on_model {
                launcher.model.push(c);
            } else {
                launcher.job.push(c);
            }
        }
        _ => {}
    }
}

/// A key while an agent's terminal is open: everything is the agent's except
/// the detach chord and the scrollback keys.
fn terminal_key(app: &mut AppView, id: u64, key: KeyEvent) {
    let live = app.fleet.info(id).is_some_and(|agent| agent.state.live());
    let detach = key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(']' | '5'));
    if detach || (!live && key.code == KeyCode::Esc) {
        app.fleet.scroll(id, 0);
        app.fleet_view.attached = None;
        app.fleet_view.scroll = 0;
        return;
    }
    let scroll = app.fleet_view.scroll;
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let scrolled = match key.code {
        KeyCode::PageUp if shift || !live => Some(scroll.saturating_add(SCROLL_STEP)),
        KeyCode::PageDown if shift || !live => Some(scroll.saturating_sub(SCROLL_STEP)),
        _ => None,
    };
    if let Some(rows) = scrolled {
        app.fleet.scroll(id, rows);
        // The terminal clamps to the scrollback it has.
        app.fleet_view.scroll = app
            .fleet
            .with_screen(id, Screen::scrollback)
            .unwrap_or_default();
        return;
    }
    if !live {
        return;
    }
    if scroll > 0 {
        app.fleet.scroll(id, 0);
        app.fleet_view.scroll = 0;
    }
    let application_cursor = app
        .fleet
        .with_screen(id, Screen::application_cursor)
        .unwrap_or_default();
    let bytes = encode_key(key, application_cursor);
    if !bytes.is_empty()
        && let Err(error) = app.fleet.write(id, &bytes)
    {
        app.push_status(format!("fleet: {error:#}"));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use dugix_core::fleet::Launch;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Duration;

    fn script(title: &str, body: &str) -> Launch {
        Launch {
            label: "sh".into(),
            title: title.into(),
            argv: vec!["/bin/sh".into(), "-c".into(), body.into()],
            cwd: std::env::temp_dir(),
            isolate: false,
            auto: Vec::new(),
        }
    }

    fn screen(app: &AppView, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content
            .chunks(usize::from(width))
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn until(app: &AppView, what: &str, done: impl Fn(&AppView) -> bool) {
        for _ in 0..200 {
            if done(app) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn keys_encode_as_a_terminal_sends_them() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(encode_key(key(KeyCode::Char('é')), false), "é".as_bytes());
        assert_eq!(encode_key(ctrl('c'), false), [3]);
        assert_eq!(encode_key(key(KeyCode::Enter), false), b"\r");
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), false),
            b"\x1b\r"
        );
        assert_eq!(encode_key(key(KeyCode::Up), false), b"\x1b[A");
        assert_eq!(encode_key(key(KeyCode::Up), true), b"\x1bOA");
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT), false),
            b"\x1bb"
        );
        assert_eq!(encode_key(key(KeyCode::F(5)), false), b"\x1b[15~");
        assert_eq!(encode_key(key(KeyCode::BackTab), false), b"\x1b[Z");
    }

    #[tokio::test]
    async fn the_board_lists_agents_blocked_first_and_attaching_types_into_one() {
        let mut app = crate::ui::popups::tests::test_app();
        assert_eq!(
            app.parse_command("fleet").unwrap(),
            AppCommand::Fleet {
                action: FleetAction::Open
            }
        );
        // With no agents the board opens on the new-agent form.
        open(&mut app);
        let form = screen(&app, 110, 30);
        assert!(
            form.contains("New agent") && form.contains("opencode"),
            "{form}"
        );
        assert!(form.contains("Opens claude in its own terminal"), "{form}");
        assert!(form.contains("own branch and worktree"), "{form}");
        handle_key(&mut app, key(KeyCode::Right)).unwrap();
        for c in "fix it".chars() {
            handle_key(&mut app, key(KeyCode::Char(c))).unwrap();
        }
        // Down moves typing to the model field; Ctrl+T works in your tree instead.
        handle_key(&mut app, key(KeyCode::Down)).unwrap();
        for c in "haiku".chars() {
            handle_key(&mut app, key(KeyCode::Char(c))).unwrap();
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        )
        .unwrap();
        let launcher = app.fleet_view.launcher.as_ref().unwrap();
        assert_eq!(
            (
                launcher.job.as_str(),
                launcher.model.as_str(),
                launcher.isolate
            ),
            ("fix it", "haiku", false)
        );
        let form = screen(&app, 110, 30);
        assert!(
            form.contains("Opens codex in its own terminal")
                && form.contains("❯ fix it")
                && form.contains("❯ haiku")
                && form.contains("directly in your tree"),
            "{form}"
        );
        handle_key(&mut app, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.popup, Popup::None, "nothing to go back to");

        let asking = app
            .fleet
            .spawn(
                "",
                script(
                    "delete the build dir",
                    "echo 'Do you want to proceed? (y/n)'; read a; echo \"answer=$a\"; read b",
                ),
            )
            .unwrap();
        app.fleet
            .spawn("", script("write the docs", "echo drafting; read a"))
            .unwrap();
        open(&mut app);
        assert!(app.fleet_view.launcher.is_none(), "agents exist: the board");
        handle_key(&mut app, key(KeyCode::Char('n'))).unwrap();
        assert!(screen(&app, 110, 30).contains("New agent"));
        handle_key(&mut app, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.popup, Popup::Fleet, "Esc returns to the board");
        until(&app, "both agents to settle", |app| {
            app.fleet.blocked() == 1
                && app
                    .fleet
                    .list(None)
                    .iter()
                    .all(|agent| agent.state != State::Working)
        })
        .await;
        let board = screen(&app, 110, 30);
        assert!(board.contains("2 running · 1 need you"), "{board}");
        let blocked = board.find("sh 1 delete the build dir").expect(&board);
        let idle = board.find("sh 2 write the docs").expect(&board);
        assert!(blocked < idle, "the blocked agent leads:\n{board}");
        assert!(board.contains("blocked · 0m0"), "{board}");
        assert!(board.contains("Do you want to proceed? (y/n)"), "{board}");

        // `s` steers from the board without attaching.
        handle_key(&mut app, key(KeyCode::Down)).unwrap();
        handle_key(&mut app, key(KeyCode::Char('s'))).unwrap();
        for c in "go".chars() {
            handle_key(&mut app, key(KeyCode::Char(c))).unwrap();
        }
        assert!(screen(&app, 110, 30).contains("tell sh 2 ❯ go"));
        handle_key(&mut app, key(KeyCode::Esc)).unwrap();
        assert_eq!(app.popup, Popup::Fleet, "Esc leaves the steer line first");

        // Enter attaches to the blocked agent; its terminal takes the keys.
        handle_key(&mut app, key(KeyCode::Up)).unwrap();
        handle_key(&mut app, key(KeyCode::Enter)).unwrap();
        assert_eq!(app.fleet_view.attached, Some(asking));
        assert!(captures(&app));
        let terminal = screen(&app, 110, 30);
        assert!(terminal.contains("sh 1 · blocked"), "{terminal}");
        assert!(terminal.contains("^] detach"), "{terminal}");
        assert!(
            terminal.contains("Do you want to proceed? (y/n)"),
            "{terminal}"
        );
        handle_key(&mut app, key(KeyCode::Char('y'))).unwrap();
        handle_key(&mut app, key(KeyCode::Enter)).unwrap();
        until(&app, "the answer", |app| {
            screen(app, 110, 30).contains("answer=y")
        })
        .await;
        assert!(paste(&mut app, "pasted"));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
        )
        .unwrap();
        assert_eq!(app.fleet_view.attached, None);
        assert!(!paste(&mut app, "ignored"));

        // Two keys stop an agent.
        let stop = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        handle_key(&mut app, stop).unwrap();
        assert!(screen(&app, 110, 30).contains("Stop agent?"));
        handle_key(&mut app, stop).unwrap();
        assert_eq!(app.fleet.list(None).len(), 1);

        // Starting an agent lands in its terminal, not on the board.
        attach_newest(&mut app);
        assert_eq!(
            app.fleet_view.attached,
            app.fleet.list(None).last().map(|a| a.id)
        );
        assert!(screen(&app, 110, 30).contains("^] detach"));
        app.fleet.shutdown();
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    #[tokio::test]
    async fn reviewing_an_agent_shows_its_diff_and_check_before_merging() {
        let repo = std::env::temp_dir().join(format!("dugix-review-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        let repo = repo.canonicalize().unwrap();
        std::fs::write(repo.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        std::fs::write(
            repo.join("scripts/check.sh"),
            "grep -q 'a + b' calc.py && echo 'all green'\n",
        )
        .unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);

        let mut app = crate::ui::popups::tests::test_app();
        let mut launch = script(
            "fix add",
            "printf 'def add(a, b):\\n    return a + b\\n' > calc.py; echo fixed; read gone",
        );
        launch.cwd = repo.clone();
        launch.isolate = true;
        let id = app.fleet.spawn("", launch).unwrap();
        open(&mut app);
        until(&app, "the fix to be measured", |app| {
            app.fleet.info(id).is_some_and(|agent| {
                agent.state == State::Idle && agent.changes.is_some_and(|c| c.files == 1)
            })
        })
        .await;
        assert!(screen(&app, 110, 30).contains("1 file +1 −1"));

        handle_key(&mut app, key(KeyCode::Char('d'))).unwrap();
        let diff = screen(&app, 110, 30);
        assert!(
            diff.contains("Review sh 1") && diff.contains("calc.py"),
            "{diff}"
        );
        assert!(
            diff.contains("return a + b") && diff.contains("return a - b"),
            "{diff}"
        );

        handle_key(&mut app, key(KeyCode::Tab)).unwrap();
        assert!(screen(&app, 110, 30).contains("Not checked since its last change"));
        handle_key(&mut app, key(KeyCode::Char('c'))).unwrap();
        until(&app, "the check to pass", |app| {
            app.fleet
                .info(id)
                .and_then(|agent| agent.check)
                .is_some_and(|check| check.passed() == Some(true))
        })
        .await;
        let check = screen(&app, 110, 30);
        assert!(
            check.contains("check passed in") && check.contains("$ sh scripts/check.sh"),
            "{check}"
        );
        assert!(check.contains("all green"), "{check}");

        handle_key(&mut app, key(KeyCode::Char('m'))).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join("calc.py")).unwrap(),
            "def add(a, b):\n    return a + b\n"
        );
        handle_key(&mut app, key(KeyCode::Tab)).unwrap();
        assert!(screen(&app, 110, 30).contains("no changes yet"));
        handle_key(&mut app, key(KeyCode::Esc)).unwrap();
        assert!(app.fleet_view.review.is_none(), "back on the board");
        assert_eq!(app.popup, Popup::Fleet);
        app.fleet.shutdown();
        let _ = std::fs::remove_dir_all(repo);
    }
}
