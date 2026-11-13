//! `/connectors`: the MCP connectors and their tools. Domain changes go through
//! `AppCommand`; only the bearer token bypasses it, so a secret never rides one.

use std::fmt::Write as _;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::{AppCommand, Popup};
use nexus_core::connectors::import::{self, ImportPaths, Proposal};
use nexus_core::connectors::{Access, ConnectorInfo, Policy, Status, ToolRow, config};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState, Paragraph};

use crate::app_view::AppView;
use crate::ui::style::{self, glyph};

use super::chrome::{self, PopupFrame, ctrl, hint, keys};

/// Which screen of the popup is showing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    #[default]
    List,
    Tools,
    Add,
    ConfirmRemove,
    Import,
    Log,
}

/// The add form's current step.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AddStep {
    #[default]
    Target,
    Name,
    Auth,
    Token,
}

/// What the add form has collected so far.
#[derive(Default)]
pub struct AddForm {
    pub step: AddStep,
    pub target: String,
    pub name: String,
    pub bearer: bool,
    pub token: String,
}

/// One server found in another tool's config.
pub struct ImportRow {
    pub proposal: Proposal,
    pub checked: bool,
}

#[derive(Default)]
pub struct ConnectorsPopup {
    pub mode: Mode,
    pub selected: usize,
    pub tool_selected: usize,
    pub add: AddForm,
    pub import: Vec<ImportRow>,
    pub import_selected: usize,
    pub import_skipped: Vec<String>,
    pub log: String,
    pub log_scroll: usize,
}

/// Open the popup on its list.
pub fn open(app: &mut AppView) {
    app.connectors_popup.mode = Mode::List;
    let count = app.connectors.infos().len();
    app.connectors_popup.selected = app.connectors_popup.selected.min(count.saturating_sub(1));
    app.popup = Popup::Connectors;
}

fn status_word(status: &Status) -> &'static str {
    match status {
        Status::Connecting => "connecting",
        Status::Connected { .. } => "connected",
        Status::NeedsAuth { .. } => "sign-in needed",
        Status::Error(_) => "error",
        Status::Disabled => "off",
    }
}

fn status_dot(status: &Status, theme: &crate::theme::Theme) -> Span<'static> {
    let (glyph, color) = match status {
        Status::Connecting => (glyph::RUNNING, theme.fg_dim),
        Status::Connected { .. } => (glyph::DOT, theme.success),
        Status::NeedsAuth { .. } => (glyph::WARN, theme.warning),
        Status::Error(_) => (glyph::FAIL, theme.error),
        Status::Disabled => (glyph::RING, theme.fg_dim),
    };
    Span::styled(glyph, Style::default().fg(color))
}

fn detail_for(info: &ConnectorInfo) -> String {
    let mut lines = vec![info.target.clone()];
    let mut facts = Vec::new();
    if let Some(protocol) = &info.protocol {
        facts.push(format!("protocol {protocol}"));
    }
    if let Some(server) = &info.server_name {
        facts.push(format!("server {server}"));
    }
    if !facts.is_empty() {
        lines.push(facts.join(" · "));
    }
    match &info.status {
        Status::Error(why) => lines.push(format!("error: {why}")),
        Status::NeedsAuth { .. } => lines.push(match info.auth {
            config::AuthKind::Bearer => {
                "the bearer token is missing or was rejected · remove and add the connector again with a token".to_string()
            }
            _ => "this server wants you to sign in · OAuth sign-in arrives in the next release".to_string(),
        }),
        _ => {}
    }
    if !info.skipped.is_empty() {
        lines.push(format!(
            "skipped {}: {}",
            info.skipped.len(),
            info.skipped
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if let Some(path) = &info.log_path {
        lines.push(format!("log: {}", path.display()));
    }
    lines.join("\n")
}

pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "connectors", chrome::WIDE);
    match app.connectors_popup.mode {
        Mode::List | Mode::ConfirmRemove => render_list(f, app, area),
        Mode::Tools => render_tools(f, app, area),
        Mode::Add => render_add(f, app, area),
        Mode::Import => render_import(f, app, area),
        Mode::Log => render_log(f, app, area),
    }
}

#[allow(clippy::too_many_lines)] // connector rows and removal frame
fn render_list(f: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    let infos = app.connectors.infos();
    let popup = &app.connectors_popup;
    let confirm = popup.mode == Mode::ConfirmRemove;
    let selected = popup.selected.min(infos.len().saturating_sub(1));
    let tools: usize = infos
        .iter()
        .map(|info| match info.status {
            Status::Connected { tools } => tools,
            _ => 0,
        })
        .sum();
    let count = if confirm {
        String::new()
    } else {
        format!(
            "{} connector{} · {tools} tool{}",
            infos.len(),
            if infos.len() == 1 { "" } else { "s" },
            if tools == 1 { "" } else { "s" }
        )
    };
    let hints = if confirm {
        vec![hint(ctrl('d'), "remove"), hint(keys::ESC, "cancel")]
    } else {
        vec![
            hint(keys::MOVE, ""),
            hint(keys::ENTER, "tools"),
            hint("a", "add"),
            hint("e", "on/off"),
            hint("i", "import"),
            hint("l", "log"),
            hint("r", "retry"),
            hint(ctrl('d'), "remove"),
            hint(keys::ESC, "close"),
        ]
    };
    let tone = if confirm {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: if confirm {
                "Remove connector?"
            } else {
                "Connectors"
            },
            count: &count,
            hints: &hints,
            tone,
            focused: true,
        },
        app,
    );
    let width = usize::from(inner.width.saturating_sub(4));
    let name_width = infos
        .iter()
        .map(|info| crate::ui::style::width(&info.name))
        .max()
        .unwrap_or(0)
        .min(20);
    let items: Vec<ListItem> = infos
        .iter()
        .map(|info| {
            let tools = match &info.status {
                Status::Connected { tools } => {
                    format!("{tools} tool{}", if *tools == 1 { "" } else { "s" })
                }
                _ => String::new(),
            };
            let kind = if info.remote { "hosted" } else { "local" };
            let mut spans = vec![
                status_dot(&info.status, &app.theme),
                Span::raw(" "),
                Span::styled(
                    format!("{:<name_width$}", chrome::truncate(&info.name, name_width)),
                    Style::default().fg(app.theme.fg),
                ),
                Span::styled(
                    format!("{}{kind}", style::SEP),
                    Style::default().fg(app.theme.fg_dim),
                ),
            ];
            if !tools.is_empty() {
                spans.push(Span::styled(
                    format!("{}{tools}", style::SEP),
                    Style::default().fg(app.theme.fg_dim),
                ));
            }
            let status_style = match &info.status {
                Status::Error(_) => Style::default().fg(app.theme.error),
                Status::NeedsAuth { .. } => Style::default().fg(app.theme.warning),
                _ => Style::default().fg(app.theme.fg_dim),
            };
            spans.push(Span::styled(
                format!("{}{}", style::SEP, status_word(&info.status)),
                status_style,
            ));
            ListItem::new(chrome::fit_line(Line::from(spans), width))
        })
        .collect();
    let detail = if confirm {
        let name = infos.get(selected).map_or("", |info| info.name.as_str());
        format!("“{name}” and its keychain entries will be removed.")
    } else {
        infos.get(selected).map(detail_for).unwrap_or_default()
    };
    let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
    let mut state = ListState::default();
    if !infos.is_empty() {
        state.select(Some(selected));
    }
    chrome::render_list(
        f,
        chrome::standard_list(items, &app.theme),
        &mut state,
        list_area,
        infos.len(),
        1,
        app,
    );
    chrome::render_detail(f, detail_area, &detail, &app.theme);
    if infos.is_empty() {
        chrome::render_empty(
            f,
            list_area,
            "No connectors yet. a adds one, i imports from Claude Desktop, Claude Code or Cursor.",
            &app.theme,
        );
    }
}

fn policy_cell(row: &ToolRow, theme: &crate::theme::Theme) -> Span<'static> {
    let (text, color) = match (row.policy, row.access) {
        (Some(Policy::Allow), Access::Ask { .. }) => ("allow?", theme.warning),
        (Some(Policy::Allow), _) => ("allow", theme.success),
        (Some(Policy::Ask), _) => ("ask", theme.accent),
        (Some(Policy::Deny), _) => ("deny", theme.error),
        (None, Access::Free) => ("free", theme.fg_dim),
        (None, Access::Ask { changed: true }) => ("changed", theme.warning),
        (None, _) => ("ask", theme.fg_dim),
    };
    Span::styled(format!("{text:<7}"), Style::default().fg(color))
}

fn render_tools(f: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    let infos = app.connectors.infos();
    let popup = &app.connectors_popup;
    let name = infos
        .get(popup.selected)
        .map(|info| info.name.clone())
        .unwrap_or_default();
    let rows = app.connectors.tool_rows(&name);
    let selected = popup.tool_selected.min(rows.len().saturating_sub(1));
    let count = format!(
        "{} tool{}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    );
    let hints = [
        hint(keys::MOVE, ""),
        hint("a", "allow"),
        hint("s", "ask"),
        hint("d", "deny"),
        hint("Backspace", "default"),
        hint(keys::ESC, "back"),
    ];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: &format!("{name} tools"),
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let width = usize::from(inner.width.saturating_sub(4));
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let label = row.title.as_deref().unwrap_or(&row.name);
            ListItem::new(chrome::fit_line(
                Line::from(vec![
                    policy_cell(row, &app.theme),
                    Span::styled(label.to_string(), Style::default().fg(app.theme.fg)),
                    Span::styled(
                        format!("{}{}", style::SEP, row.words),
                        Style::default().fg(app.theme.fg_dim),
                    ),
                ]),
                width,
            ))
        })
        .collect();
    let detail = rows
        .get(selected)
        .map(|row| {
            let mut text = row.description.chars().take(300).collect::<String>();
            if matches!(row.access, Access::Ask { changed: true }) {
                text.push_str("\nthis tool's definition changed since you allowed it");
            }
            if !row.exposed.is_empty() {
                let _ = write!(text, "\nthe model sees it as {}", row.exposed);
            }
            text
        })
        .unwrap_or_default();
    let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(selected));
    }
    chrome::render_list(
        f,
        chrome::standard_list(items, &app.theme),
        &mut state,
        list_area,
        rows.len(),
        1,
        app,
    );
    chrome::render_detail(f, detail_area, &detail, &app.theme);
    if rows.is_empty() {
        chrome::render_empty(
            f,
            list_area,
            "No tools yet. Connect the connector or retry from the list.",
            &app.theme,
        );
    }
}

#[allow(clippy::too_many_lines)] // wizard fields and step-specific frame
fn render_add(f: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    let form = &app.connectors_popup.add;
    let remote = form.target.trim().starts_with("http");
    let total = if !remote {
        2
    } else if form.bearer {
        4
    } else {
        3
    };
    let (step, label, value) = match form.step {
        AddStep::Target => (1, "URL or command", form.target.clone()),
        AddStep::Name => (2, "Name", form.name.clone()),
        AddStep::Auth => (
            3,
            "Auth",
            if form.bearer { "bearer token" } else { "none" }.to_string(),
        ),
        AddStep::Token => (4, "Token", "*".repeat(form.token.chars().count())),
    };
    let count = format!("step {step} of {total}");
    let mut hints = vec![hint(
        keys::ENTER,
        if step == total { "save" } else { "next" },
    )];
    if form.step == AddStep::Auth {
        hints.push(hint(keys::TAB, "switch"));
    }
    hints.push(hint(keys::ESC, if step == 1 { "cancel" } else { "back" }));
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Add connector",
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let inner = chrome::render_field(
        f,
        inner,
        "✎",
        &value,
        label,
        form.step != AddStep::Auth,
        &app.theme,
    );
    let dim = Style::default().fg(app.theme.fg_dim);
    let normal = Style::default().fg(app.theme.fg);
    let current = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    let steps = [
        (AddStep::Target, "url or command", form.target.clone()),
        (AddStep::Name, "name", form.name.clone()),
        (
            AddStep::Auth,
            "auth",
            if form.bearer { "bearer token" } else { "none" }.to_string(),
        ),
        (
            AddStep::Token,
            "token",
            "*".repeat(form.token.chars().count()),
        ),
    ];
    let mut lines = vec![Line::from("")];
    for (step, label, value) in steps {
        let skipped = (!remote && matches!(step, AddStep::Auth | AddStep::Token))
            || (step == AddStep::Token && !form.bearer);
        let style = if skipped {
            dim
        } else if step == form.step {
            current
        } else {
            normal
        };
        lines.push(chrome::fit_line(
            Line::from(vec![
                Span::styled(format!("{label:<15}"), dim),
                Span::styled(if skipped { "—".to_string() } else { value }, style),
            ]),
            usize::from(inner.width),
        ));
    }
    lines.push(Line::from(""));
    for text in [
        "a hosted server is an https:// URL; a local one is a command line, e.g.",
        "npx -y @modelcontextprotocol/server-filesystem ~/notes",
        "tokens use your keychain, or owner-only config.toml when none is available",
    ] {
        lines.push(Line::from(Span::styled(
            if app.settings.hide_hints {
                String::new()
            } else {
                text.to_string()
            },
            dim,
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// One import row: checkbox, name, where it came from, what it points at.
fn import_item(row: &ImportRow, width: usize, theme: &crate::theme::Theme) -> ListItem<'static> {
    let proposal = &row.proposal;
    let mark = if proposal.already {
        "[=]"
    } else if row.checked {
        "[x]"
    } else {
        "[ ]"
    };
    let fg = if proposal.already {
        theme.fg_dim
    } else {
        theme.fg
    };
    let dim = Style::default().fg(theme.fg_dim);
    let mut spans = vec![
        Span::styled(format!("{mark} "), Style::default().fg(theme.accent)),
        Span::styled(proposal.name.clone(), Style::default().fg(fg)),
        Span::styled(
            format!(
                "{}{}{}{}",
                style::SEP,
                proposal.source.label(),
                style::SEP,
                proposal.config.target_label()
            ),
            dim,
        ),
    ];
    if proposal.already {
        spans.push(Span::styled(format!("{}already added", style::SEP), dim));
    }
    ListItem::new(chrome::fit_line(Line::from(spans), width))
}

/// The strip under the import list: source file, keychain moves, caveats.
fn import_detail(proposal: &Proposal) -> String {
    let file = proposal
        .origin
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut text = format!("from {file}");
    let secrets = proposal.secrets.len();
    if secrets > 0 {
        let _ = write!(
            text,
            "\n{secrets} secret{} will move to your keychain",
            if secrets == 1 { "" } else { "s" }
        );
    }
    for note in &proposal.notes {
        let _ = write!(text, "\n{note}");
    }
    text
}

fn render_import(f: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    let popup = &app.connectors_popup;
    let chosen = popup.import.iter().filter(|row| row.checked).count();
    let count = format!("{chosen} selected");
    let hints = [
        hint(keys::MOVE, ""),
        hint(keys::SPACE, "toggle"),
        hint("a", "all"),
        hint(keys::ENTER, "import"),
        hint(keys::ESC, "back"),
    ];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Import connectors",
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let width = usize::from(inner.width.saturating_sub(4));
    let items: Vec<ListItem> = popup
        .import
        .iter()
        .map(|row| import_item(row, width, &app.theme))
        .collect();
    let detail = popup.import.get(popup.import_selected).map_or_else(
        || popup.import_skipped.join("\n"),
        |row| import_detail(&row.proposal),
    );
    let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
    let mut state = ListState::default();
    if !popup.import.is_empty() {
        state.select(Some(popup.import_selected.min(popup.import.len() - 1)));
    }
    chrome::render_list(
        f,
        chrome::standard_list(items, &app.theme),
        &mut state,
        list_area,
        popup.import.len(),
        1,
        app,
    );
    chrome::render_detail(f, detail_area, &detail, &app.theme);
    if popup.import.is_empty() {
        chrome::render_empty(
            f,
            list_area,
            "Nothing found in Claude Desktop, Claude Code or Cursor.",
            &app.theme,
        );
    }
}

fn render_log(f: &mut Frame, app: &AppView, area: ratatui::layout::Rect) {
    let name = selected_connector(app)
        .map(|info| info.name)
        .unwrap_or_default();
    let hints = [hint(keys::MOVE, "scroll"), hint(keys::ESC, "back")];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: &format!("{name} log"),
            count: "",
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    if app.connectors_popup.log.is_empty() {
        chrome::render_empty(f, inner, "No log entries yet.", &app.theme);
    } else {
        let lines: Vec<_> = app
            .connectors_popup
            .log
            .lines()
            .skip(app.connectors_popup.log_scroll)
            .map(Line::raw)
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().fg(app.theme.fg)),
            inner,
        );
    }
}

fn run(app: &mut AppView, command: AppCommand) {
    if let Err(error) = app.execute(command) {
        app.push_status(format!("error: {error:#}"));
    }
}

fn move_by(value: usize, len: usize, delta: i32) -> usize {
    nexus_core::app::clamp_cursor(value, len, delta)
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match app.connectors_popup.mode {
        Mode::List => list_key(app, key),
        Mode::ConfirmRemove => confirm_key(app, key),
        Mode::Tools => tools_key(app, key),
        Mode::Add => add_key(app, key),
        Mode::Import => import_key(app, key),
        Mode::Log => log_key(app, key),
    }
}

fn selected_connector(app: &AppView) -> Option<ConnectorInfo> {
    let infos = app.connectors.infos();
    let index = app
        .connectors_popup
        .selected
        .min(infos.len().saturating_sub(1));
    infos.into_iter().nth(index)
}

fn list_key(app: &mut AppView, key: KeyEvent) {
    let count = app.connectors.infos().len();
    let popup = &mut app.connectors_popup;
    match key.code {
        KeyCode::Esc => app.popup = Popup::None,
        KeyCode::Up => popup.selected = move_by(popup.selected, count, -1),
        KeyCode::Down => popup.selected = move_by(popup.selected, count, 1),
        KeyCode::Char('k') if !super::is_chord(key) => {
            popup.selected = move_by(popup.selected, count, -1);
        }
        KeyCode::Char('j') if !super::is_chord(key) => {
            popup.selected = move_by(popup.selected, count, 1);
        }
        KeyCode::PageUp => popup.selected = move_by(popup.selected, count, -10),
        KeyCode::PageDown => popup.selected = move_by(popup.selected, count, 10),
        KeyCode::Home => popup.selected = 0,
        KeyCode::End => popup.selected = count.saturating_sub(1),
        KeyCode::Enter => {
            if count > 0 {
                popup.mode = Mode::Tools;
                popup.tool_selected = 0;
            }
        }
        KeyCode::Char('a') if !super::is_chord(key) => {
            popup.add = AddForm::default();
            popup.mode = Mode::Add;
        }
        KeyCode::Char('e') if !super::is_chord(key) => {
            if let Some(info) = selected_connector(app) {
                run(
                    app,
                    AppCommand::SetConnectorEnabled {
                        name: info.name,
                        on: !info.enabled,
                    },
                );
            }
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if count > 0 {
                popup.mode = Mode::ConfirmRemove;
            }
        }
        KeyCode::Char('r') if !super::is_chord(key) => {
            // Pick up hand edits to config.toml, then reconnect the selection.
            match app.connectors.load() {
                Ok(problems) => {
                    for problem in problems {
                        app.push_status(format!("connector config: {problem}"));
                    }
                }
                Err(error) => app.push_status(format!("could not reload config: {error:#}")),
            }
            if let Some(info) = selected_connector(app) {
                app.connectors.restart(&info.name);
                app.push_status(format!("reconnecting {}…", info.name));
            }
        }
        KeyCode::Char('l') if !super::is_chord(key) => {
            if let Some(path) = selected_connector(app).and_then(|info| info.log_path) {
                if path.exists() {
                    match std::fs::read_to_string(&path) {
                        Ok(log) => {
                            app.connectors_popup.log = log;
                            app.connectors_popup.log_scroll = 0;
                            app.connectors_popup.mode = Mode::Log;
                        }
                        Err(error) => app.push_status(format!("could not read log: {error}")),
                    }
                } else {
                    app.push_status(format!("no log yet · it will be {}", path.display()));
                }
            }
        }
        KeyCode::Char('i') if !super::is_chord(key) => start_import(app),
        _ => {}
    }
}

fn confirm_key(app: &mut AppView, key: KeyEvent) {
    let yes = super::classify_confirm_delete_key(key) == Some(super::ConfirmDeleteAction::Yes);
    if yes {
        if let Some(info) = selected_connector(app) {
            run(app, AppCommand::RemoveConnector { name: info.name });
        }
        let count = app.connectors.infos().len();
        let popup = &mut app.connectors_popup;
        popup.selected = popup.selected.min(count.saturating_sub(1));
        popup.mode = Mode::List;
    } else if key.code == KeyCode::Esc {
        app.connectors_popup.mode = Mode::List;
    }
}

fn tools_key(app: &mut AppView, key: KeyEvent) {
    let Some(info) = selected_connector(app) else {
        app.connectors_popup.mode = Mode::List;
        return;
    };
    let rows = app.connectors.tool_rows(&info.name);
    let selected = app
        .connectors_popup
        .tool_selected
        .min(rows.len().saturating_sub(1));
    let set = |app: &mut AppView, policy: &str| {
        if let Some(row) = rows.get(selected) {
            run(
                app,
                AppCommand::SetConnectorPolicy {
                    name: info.name.clone(),
                    tool: row.name.clone(),
                    policy: policy.to_string(),
                },
            );
        }
    };
    let popup = &mut app.connectors_popup;
    match key.code {
        KeyCode::Esc => popup.mode = Mode::List,
        KeyCode::Up => popup.tool_selected = move_by(selected, rows.len(), -1),
        KeyCode::Down => popup.tool_selected = move_by(selected, rows.len(), 1),
        KeyCode::Char('k') if !super::is_chord(key) => {
            popup.tool_selected = move_by(selected, rows.len(), -1);
        }
        KeyCode::Char('j') if !super::is_chord(key) => {
            popup.tool_selected = move_by(selected, rows.len(), 1);
        }
        KeyCode::PageUp => popup.tool_selected = move_by(selected, rows.len(), -10),
        KeyCode::PageDown => popup.tool_selected = move_by(selected, rows.len(), 10),
        KeyCode::Home => popup.tool_selected = 0,
        KeyCode::End => popup.tool_selected = rows.len().saturating_sub(1),
        KeyCode::Char('a') if !super::is_chord(key) => set(app, "allow"),
        KeyCode::Char('s') if !super::is_chord(key) => set(app, "ask"),
        KeyCode::Char('d') if !super::is_chord(key) => set(app, "deny"),
        KeyCode::Backspace | KeyCode::Delete => set(app, "default"),
        _ => {}
    }
}

fn field_mut(form: &mut AddForm) -> Option<&mut String> {
    match form.step {
        AddStep::Target => Some(&mut form.target),
        AddStep::Name => Some(&mut form.name),
        AddStep::Token => Some(&mut form.token),
        AddStep::Auth => None,
    }
}

/// Type or paste into whichever add-form field has focus.
pub fn paste(app: &mut AppView, text: &str) {
    if app.connectors_popup.mode == Mode::Add
        && let Some(field) = field_mut(&mut app.connectors_popup.add)
    {
        field.push_str(&text.replace(['\r', '\n'], ""));
    }
}

fn add_key(app: &mut AppView, key: KeyEvent) {
    let form = &mut app.connectors_popup.add;
    let remote = form.target.trim().starts_with("http");
    match key.code {
        KeyCode::Esc => match form.step {
            AddStep::Target => app.connectors_popup.mode = Mode::List,
            AddStep::Name => form.step = AddStep::Target,
            AddStep::Auth => form.step = AddStep::Name,
            AddStep::Token => form.step = AddStep::Auth,
        },
        KeyCode::Enter => match form.step {
            AddStep::Target => {
                if form.target.trim().is_empty() {
                    return;
                }
                if form.name.is_empty() {
                    form.name = config::suggest_name(&form.target);
                }
                form.step = AddStep::Name;
            }
            AddStep::Name => {
                if !config::valid_name(form.name.trim()) {
                    app.push_status("names are 1-32 characters of a-z, 0-9, - and _".to_string());
                    return;
                }
                form.step = if remote {
                    AddStep::Auth
                } else {
                    AddStep::Token
                };
                if !remote {
                    save_add(app);
                }
            }
            AddStep::Auth => {
                if form.bearer {
                    form.step = AddStep::Token;
                } else {
                    save_add(app);
                }
            }
            AddStep::Token => save_add(app),
        },
        KeyCode::Tab | KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
            if form.step == AddStep::Auth =>
        {
            form.bearer = !form.bearer;
        }
        KeyCode::Backspace => {
            if let Some(field) = field_mut(form) {
                field.pop();
            }
        }
        KeyCode::Char(c) if !super::is_chord(key) => {
            if let Some(field) = field_mut(form) {
                field.push(c);
            }
        }
        _ => {}
    }
}

/// Add the connector the form describes; a bearer token goes to the keychain.
fn save_add(app: &mut AppView) {
    let form = std::mem::take(&mut app.connectors_popup.add);
    let name = form.name.trim().to_string();
    let target = form.target.trim().to_string();
    app.connectors_popup.mode = Mode::List;
    let result = if target.starts_with("http") {
        app.execute(AppCommand::AddConnector {
            name: name.clone(),
            url: Some(target),
            command: Vec::new(),
            bearer: form.bearer,
        })
        .and_then(|()| {
            if form.bearer && !form.token.is_empty() {
                if let Err(error) = app.connectors.vault().set(&name, "bearer", &form.token) {
                    app.execute(AppCommand::RemoveConnector { name: name.clone() })?;
                    return Err(anyhow::anyhow!(error));
                }
                app.connectors.restart(&name);
                app.push_status(format!("connector {name} added · token stored"));
            }
            Ok(())
        })
    } else {
        match nexus_core::connectors::split_argv(&target) {
            Ok(command) => app.execute(AppCommand::AddConnector {
                name: name.clone(),
                url: None,
                command,
                bearer: false,
            }),
            Err(why) => Err(anyhow::anyhow!(why)),
        }
    };
    match result {
        Ok(()) => {
            let infos = app.connectors.infos();
            app.connectors_popup.selected =
                infos.iter().position(|info| info.name == name).unwrap_or(0);
        }
        Err(error) => app.push_status(format!("error: {error:#}")),
    }
}

fn start_import(app: &mut AppView) {
    let paths = app
        .import_paths
        .clone()
        .unwrap_or_else(|| ImportPaths::system(std::env::current_dir().ok().as_deref()));
    open_import(app, &paths);
}

/// List what other tools have configured (paths injectable for tests).
pub fn open_import(app: &mut AppView, paths: &ImportPaths) {
    app.import_paths = Some(paths.clone());
    let found = import::discover(paths, &app.connectors.configs());
    let popup = &mut app.connectors_popup;
    popup.import = found
        .proposals
        .into_iter()
        .map(|proposal| ImportRow {
            checked: !proposal.already,
            proposal,
        })
        .collect();
    popup.import_skipped = found.skipped;
    popup.import_selected = 0;
    popup.mode = Mode::Import;
}

fn log_key(app: &mut AppView, key: KeyEvent) {
    let popup = &mut app.connectors_popup;
    let count = popup.log.lines().count();
    let delta = match key.code {
        KeyCode::Esc => {
            popup.mode = Mode::List;
            return;
        }
        KeyCode::Up => -1,
        KeyCode::Down => 1,
        KeyCode::Char('k') if !super::is_chord(key) => -1,
        KeyCode::Char('j') if !super::is_chord(key) => 1,
        KeyCode::PageUp => -10,
        KeyCode::PageDown => 10,
        KeyCode::Home => i32::MIN / 2,
        KeyCode::End => i32::MAX / 2,
        _ => return,
    };
    popup.log_scroll = move_by(popup.log_scroll, count, delta);
}

fn import_key(app: &mut AppView, key: KeyEvent) {
    let popup = &mut app.connectors_popup;
    let count = popup.import.len();
    match key.code {
        KeyCode::Esc => popup.mode = Mode::List,
        KeyCode::Up => popup.import_selected = move_by(popup.import_selected, count, -1),
        KeyCode::Down => popup.import_selected = move_by(popup.import_selected, count, 1),
        KeyCode::Char('k') if !super::is_chord(key) => {
            popup.import_selected = move_by(popup.import_selected, count, -1);
        }
        KeyCode::Char('j') if !super::is_chord(key) => {
            popup.import_selected = move_by(popup.import_selected, count, 1);
        }
        KeyCode::PageUp => popup.import_selected = move_by(popup.import_selected, count, -10),
        KeyCode::PageDown => popup.import_selected = move_by(popup.import_selected, count, 10),
        KeyCode::Home => popup.import_selected = 0,
        KeyCode::End => popup.import_selected = count.saturating_sub(1),
        KeyCode::Char(' ') => {
            if let Some(row) = popup.import.get_mut(popup.import_selected)
                && !row.proposal.already
            {
                row.checked = !row.checked;
            }
        }
        KeyCode::Char('a') if !super::is_chord(key) => {
            let all = popup
                .import
                .iter()
                .filter(|row| !row.proposal.already)
                .all(|row| row.checked);
            for row in popup.import.iter_mut().filter(|row| !row.proposal.already) {
                row.checked = !all;
            }
        }
        KeyCode::Enter => {
            let names: Vec<String> = popup
                .import
                .iter()
                .filter(|row| row.checked && !row.proposal.already)
                .map(|row| row.proposal.name.clone())
                .collect();
            popup.mode = Mode::List;
            if names.is_empty() {
                app.push_status("nothing selected");
                return;
            }
            // Re-reads the same files, so the names line up with the rows.
            run(
                app,
                AppCommand::ImportConnectors {
                    source: "all".to_string(),
                    names,
                },
            );
        }
        _ => {}
    }
}
