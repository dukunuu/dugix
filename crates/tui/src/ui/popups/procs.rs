use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{ListItem, ListState, Paragraph};

use crate::app_view::AppView;
use dugix_core::app::Popup;

use super::chrome::{self, PopupFrame, ctrl, hint, keys};

#[allow(clippy::too_many_lines)] // list, output and kill-confirmation chrome
pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "procs", chrome::WIDE);
    let session = app.session.as_ref().map_or("", |s| s.id.as_str());
    let rows = app.procs.list(session);
    let confirming = app.procs_kill_confirm.is_some();
    let output = app
        .procs_output
        .as_ref()
        .and_then(|id| rows.iter().find(|p| &p.id == id));
    let (title, count, hints) = if confirming {
        (
            "Kill process?".to_string(),
            String::new(),
            vec![hint(ctrl('d'), "kill"), hint(keys::ESC, "cancel")],
        )
    } else if let Some(process) = output {
        (
            format!("{} output", process.name),
            String::new(),
            vec![hint(keys::MOVE, "scroll"), hint(keys::ESC, "back")],
        )
    } else {
        (
            "Processes".to_string(),
            format!("{} running", app.procs.running_count()),
            vec![
                hint(keys::MOVE, ""),
                hint(keys::ENTER, "output"),
                hint(ctrl('d'), "kill"),
                hint(keys::ESC, "close"),
            ],
        )
    };
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: &title,
            count: &count,
            hints: &hints,
            tone: if confirming {
                chrome::Tone::Danger
            } else {
                chrome::Tone::Normal
            },
            focused: true,
        },
        app,
    );
    if let Some(id) = &app.procs_output
        && !confirming
    {
        let text = app.procs.last_output(id).unwrap_or_else(|e| e.to_string());
        let max = text
            .lines()
            .count()
            .saturating_sub(usize::from(inner.height));
        let scroll = app.procs_scroll.min(u16::try_from(max).unwrap_or(u16::MAX));
        f.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(app.theme.fg))
                .scroll((scroll, 0)),
            inner,
        );
        return;
    }
    let detail = app
        .procs_kill_confirm
        .as_ref()
        .and_then(|id| {
            rows.iter()
                .find(|p| &p.id == id)
                .map(|p| format!("“{}” will be stopped.", p.name))
        })
        .unwrap_or_else(|| "Processes stop when dugix exits.".to_string());
    let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
    chrome::render_detail(f, detail_area, &detail, &app.theme);
    let items = rows
        .iter()
        .map(|p| {
            ListItem::new(chrome::fit_line(
                Line::from(format!("{} · {}", p.header(), p.last_line)),
                usize::from(list_area.width.saturating_sub(3)),
            ))
        })
        .collect();
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.procs_selected.min(rows.len() - 1)));
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
    if rows.is_empty() {
        chrome::render_empty(f, list_area, "No background processes.", &app.theme);
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    let session = app.session.as_ref().map_or("", |s| s.id.as_str());
    let rows = app.procs.list(session);
    let selected = app.procs_output.clone().or_else(|| {
        rows.get(app.procs_selected.min(rows.len().saturating_sub(1)))
            .map(|p| p.id.clone())
    });
    if let Some(id) = app.procs_kill_confirm.clone() {
        match super::classify_confirm_delete_key(key) {
            Some(super::ConfirmDeleteAction::Yes) => {
                app.procs.kill(&id)?;
                app.procs_kill_confirm = None;
            }
            Some(super::ConfirmDeleteAction::No) => app.procs_kill_confirm = None,
            None => {}
        }
        return Ok(());
    }
    if let Some(id) = &app.procs_output
        && key.code != KeyCode::Char('d')
    {
        let lines = app.procs.last_output(id)?.lines().count();
        output_key(app, key, lines);
        return Ok(());
    }
    match key.code {
        KeyCode::Esc => app.popup = Popup::None,
        KeyCode::Up | KeyCode::Char('k') => {
            app.procs_selected = app.procs_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.procs_selected = app
                .procs_selected
                .saturating_add(1)
                .min(rows.len().saturating_sub(1));
        }
        KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
            if let Some(action) = super::classify_browse_key(key, false, false) {
                super::apply_page(
                    |delta| {
                        app.procs_selected =
                            dugix_core::app::clamp_cursor(app.procs_selected, rows.len(), delta);
                    },
                    action,
                    10,
                );
            }
        }
        KeyCode::Enter if app.procs_output.is_none() => {
            app.procs_output = selected;
            app.procs_scroll = 0;
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.procs_kill_confirm = selected;
        }
        _ => {}
    }
    Ok(())
}

fn output_key(app: &mut AppView, key: KeyEvent, lines: usize) {
    let height = app
        .popup_hit
        .get()
        .map_or(1, |hit| hit.rect.height.saturating_sub(2));
    let max = u16::try_from(lines.saturating_sub(usize::from(height))).unwrap_or(u16::MAX);
    let scroll = app.procs_scroll.min(max);
    match key.code {
        KeyCode::Esc => app.procs_output = None,
        KeyCode::Up | KeyCode::Char('k') => app.procs_scroll = scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => app.procs_scroll = scroll.saturating_add(1).min(max),
        KeyCode::PageUp => app.procs_scroll = scroll.saturating_sub(10),
        KeyCode::PageDown => app.procs_scroll = scroll.saturating_add(10).min(max),
        KeyCode::Home => app.procs_scroll = 0,
        KeyCode::End => app.procs_scroll = max,
        _ => {}
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use dugix_core::app::{App, AppCommand};
    use dugix_core::db::Db;
    use dugix_core::space::Space;
    use dugix_core::tools::ToolBox;
    use dugix_core::workspace::Workspace;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::Arc;

    #[tokio::test]
    async fn output_scroll_status_count_and_two_key_kill() {
        let root = std::env::temp_dir().join(format!("dugix-procs-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            None,
            Space { root: root.clone() },
        ));
        assert_eq!(app.parse_command("procs").unwrap(), AppCommand::OpenProcs);
        let mut toolbox = ToolBox::new(
            std::path::PathBuf::new(),
            None,
            None,
            "auto".into(),
            vec![],
            Some(root.join("dugix.db")),
            None,
            None,
        );
        toolbox.workspace = Some(Arc::new(Workspace::open(&root, vec![]).unwrap()));
        toolbox.procs = Arc::clone(&app.procs);
        let lines = (0..205)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let args = serde_json::json!({"action":"start", "cmd":format!("printf '%s\\n' {lines}; echo ready; sleep 30"), "name":"server", "ready_when":"ready"});
        let (result, _) = toolbox.run("proc", &args.to_string()).await;
        assert!(result.contains(" · ready ·"), "{result}");
        let id = result.split_whitespace().next().unwrap();
        let mut terminal = Terminal::new(TestBackend::new(180, 1)).unwrap();
        terminal
            .draw(|f| crate::ui::status::render_status(f, &app, f.area()))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("1 procs"), "{screen}");
        let count = terminal
            .backend()
            .buffer()
            .content
            .windows(7)
            .find(|cells| cells.iter().map(|c| c.symbol()).collect::<String>() == "1 procs")
            .unwrap();
        assert!(count.iter().all(|c| c.fg == app.theme.fg_dim));
        app.popup = Popup::Procs;
        handle_key(&mut app, KeyCode::Enter.into()).unwrap();
        assert_eq!(app.procs_output.as_deref(), Some(id));
        assert!(app.procs.last_output(id).unwrap().starts_with("line6\n"));
        handle_key(&mut app, KeyCode::Down.into()).unwrap();
        assert_eq!(app.procs_scroll, 1);
        handle_key(&mut app, KeyCode::Up.into()).unwrap();
        assert_eq!(app.procs_scroll, 0);
        handle_key(&mut app, KeyCode::Char('k').into()).unwrap();
        assert!(
            app.procs_kill_confirm.is_none(),
            "k scrolls, it never kills"
        );
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        handle_key(&mut app, ctrl_d).unwrap();
        assert_eq!(app.procs_kill_confirm.as_deref(), Some(id));
        assert_eq!(app.procs.list("")[0].state, "running");
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("Kill process?") && screen.contains("^D kill"));
        handle_key(&mut app, ctrl_d).unwrap();
        assert_eq!(app.procs.list("")[0].state, "killed");
        assert!(app.procs_kill_confirm.is_none());
        handle_key(&mut app, KeyCode::Esc.into()).unwrap();
        assert!(
            app.procs_output.is_none(),
            "Esc leaves the output view first"
        );
        handle_key(&mut app, KeyCode::Esc.into()).unwrap();
        assert_eq!(app.popup, Popup::None);
        app.procs.shutdown().await;
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
