use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use nexus_core::app::Popup;
use nexus_core::db::HarnessSummary;

use super::chrome::{self, PopupFrame, hint, keys};

pub fn render(f: &mut Frame, app: &AppView) {
    let empty = HarnessSummary::default();
    let summary = app.harness.as_ref().unwrap_or(&empty);
    let area = chrome::popup_area(app, f.area(), "harness", chrome::WIDE);
    let count = format!(
        "{} call{} · {} failed · 7 days",
        summary.calls,
        if summary.calls == 1 { "" } else { "s" },
        summary.failures,
    );
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Tool health",
            count: &count,
            hints: &[
                hint(keys::MOVE, ""),
                hint(keys::ENTER, "transcript"),
                hint(keys::ESC, "close"),
            ],
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let dim = Style::default().fg(app.theme.fg_dim);
    let now = Utc::now();
    let items = summary
        .rows
        .iter()
        .map(|row| {
            let label = if row.action.is_empty() {
                row.tool.clone()
            } else {
                format!("{} {}", row.tool, row.action)
            };
            let line = Line::from(vec![
                Span::styled(label, Style::default().fg(app.theme.fg)),
                crate::ui::style::sep(&app.theme),
                Span::styled(format!("{} ", row.error_kind), dim),
                Span::styled(
                    format!("×{}", row.count),
                    Style::default().fg(app.theme.accent),
                ),
                crate::ui::style::sep(&app.theme),
                Span::styled(relative_time(&row.last_at, now), dim),
            ]);
            ListItem::new(chrome::fit_line(
                line,
                usize::from(inner.width.saturating_sub(3)),
            ))
        })
        .collect();
    let mut state = ListState::default();
    if !summary.rows.is_empty() {
        state.select(Some(app.harness_selected.min(summary.rows.len() - 1)));
    }
    chrome::render_list(
        f,
        chrome::standard_list(items, &app.theme),
        &mut state,
        inner,
        summary.rows.len(),
        1,
        app,
    );
    if summary.rows.is_empty() {
        let message = if app.incognito && summary.calls == 0 {
            "Nothing is recorded in incognito."
        } else {
            "No tool failures in the last 7 days."
        };
        chrome::render_empty(f, inner, message, &app.theme);
    }
}

fn relative_time(at: &str, now: DateTime<Utc>) -> String {
    DateTime::parse_from_rfc3339(at).map_or_else(
        |_| at.to_string(),
        |at| {
            let seconds = (now - at.with_timezone(&Utc)).num_seconds();
            if seconds < 60 {
                "just now".to_string()
            } else if seconds < 3600 {
                format!("{}m ago", seconds / 60)
            } else if seconds < 86400 {
                format!("{}h ago", seconds / 3600)
            } else {
                format!("{}d ago", seconds / 86400)
            }
        },
    )
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    let len = app.harness.as_ref().map_or(0, |summary| summary.rows.len());
    match key.code {
        KeyCode::Esc => app.popup = Popup::None,
        KeyCode::Up | KeyCode::Char('k') => {
            app.harness_selected = app.harness_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.harness_selected = app
                .harness_selected
                .saturating_add(1)
                .min(len.saturating_sub(1));
        }
        KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
            if let Some(action) = super::classify_browse_key(key, false, false) {
                super::apply_page(
                    |delta| {
                        app.harness_selected =
                            nexus_core::app::clamp_cursor(app.harness_selected, len, delta);
                    },
                    action,
                    10,
                );
            }
        }
        KeyCode::Enter => {
            let Some(id) = app
                .harness
                .as_ref()
                .and_then(|summary| summary.rows.get(app.harness_selected))
                .map(|row| row.last_session_id.clone())
            else {
                return Ok(());
            };
            if app.db.get_session(&id)?.is_none() {
                app.push_status("that session was deleted");
            } else {
                app.core.switch_to_session_by_id(&id)?;
                app.popup = Popup::None;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::{Db, FailureRow};
    use nexus_core::space::Space;

    #[test]
    fn relative_time_uses_minutes_hours_and_days() {
        let now = DateTime::parse_from_rfc3339("2026-07-14T12:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        for (seconds, expected) in [
            (-60, "just now"),
            (0, "just now"),
            (59, "just now"),
            (60, "1m ago"),
            (300, "5m ago"),
            (3600, "1h ago"),
            (10800, "3h ago"),
            (86400, "1d ago"),
            (172800, "2d ago"),
        ] {
            let at = (now - chrono::Duration::seconds(seconds)).to_rfc3339();
            assert_eq!(relative_time(&at, now), expected);
        }
    }

    #[test]
    fn harness_popup_selects_and_opens_the_latest_transcript() {
        let root = std::env::temp_dir().join(format!("nexus-harness-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let db = Db::open_in_memory().unwrap();
        let mut app = AppView::new(App::new(db, Some("k"), Space { root: root.clone() }));
        app.core
            .execute(nexus_core::app::AppCommand::Harness)
            .unwrap();
        app.popup = Popup::Harness;
        assert_eq!(app.harness.as_ref().unwrap(), &HarnessSummary::default());
        handle_key(&mut app, KeyCode::Enter.into()).unwrap();
        assert_eq!(app.popup, Popup::Harness);
        handle_key(&mut app, KeyCode::Down.into()).unwrap();
        assert_eq!(app.harness_selected, 0);

        let first = app
            .db
            .create_session("first", "model", &app.active_space.id, "chat")
            .unwrap();
        let latest = app
            .db
            .create_session("latest", "model", &app.active_space.id, "chat")
            .unwrap();
        app.db
            .add_user_message(&latest.id, "latest transcript")
            .unwrap();
        app.harness = Some(HarnessSummary {
            calls: 2,
            failures: 2,
            rows: [first.id.clone(), latest.id.clone()]
                .into_iter()
                .map(|id| FailureRow {
                    tool: "scripts".into(),
                    action: "edit".into(),
                    error_kind: "stale_edit".into(),
                    count: 1,
                    last_at: Utc::now().to_rfc3339(),
                    last_session_id: id,
                })
                .collect(),
        });
        for (key, selected) in [
            (KeyCode::Up, 0),
            (KeyCode::Down, 1),
            (KeyCode::Char('j'), 1),
            (KeyCode::Char('k'), 0),
            (KeyCode::Down, 1),
        ] {
            handle_key(&mut app, key.into()).unwrap();
            assert_eq!(app.harness_selected, selected);
        }
        handle_key(&mut app, KeyCode::Enter.into()).unwrap();
        assert_eq!(app.popup, Popup::None);
        assert_eq!(app.session.as_ref().unwrap().id, latest.id);
        assert_eq!(app.messages[0].content, "latest transcript");

        app.popup = Popup::Harness;
        app.harness_selected = 0;
        app.caches.sessions.push(first.clone());
        app.db.delete_session(&first.id).unwrap();
        handle_key(&mut app, KeyCode::Enter.into()).unwrap();
        assert_eq!(app.popup, Popup::Harness);
        assert_eq!(app.session.as_ref().unwrap().id, latest.id);
        let mut status = None;
        while let Some(event) = app.pop_pending_event() {
            if let nexus_core::app::AppEvent::Status(message) = event {
                status = Some(message);
            }
        }
        assert_eq!(status.as_deref(), Some("that session was deleted"));
        handle_key(&mut app, KeyCode::Esc.into()).unwrap();
        assert_eq!(app.popup, Popup::None);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
