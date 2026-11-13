use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;

use super::chrome::{self, PopupFrame, ctrl, hint, keys};
use crate::ui::style::glyph;

#[allow(clippy::too_many_lines)] // row design + preview strip
pub fn render(f: &mut Frame, app: &mut AppView) {
    use nexus_core::app::SessionMode;
    let area = chrome::popup_area(app, f.area(), "session", chrome::TALL);

    // Preview strip: the selected session's last exchange. Re-queried only when
    // the selection changes, and before `filtered_sessions` borrows the cache.
    let sid = app.selected_session().map(|s| s.id).unwrap_or_default();
    if app.session_preview.as_ref().map(|(id, _)| id) != Some(&sid) {
        app.session_preview = app
            .db
            .last_message_preview(&sid)
            .map(|c| (sid.clone(), chrome::truncate(&c, 260)));
    }
    let preview = app
        .session_preview
        .as_ref()
        .map(|(_, p)| p.clone())
        .unwrap_or_default();

    let sessions = app.filtered_sessions();
    // Inside the border, minus the scrollbar gutter, the `▸ ` highlight, and a
    // column of air.
    let width = area.width.saturating_sub(6 + 2 * chrome::PAD) as usize;
    let dim = Style::default().fg(app.theme.fg_dim);
    let active_id = app.session.as_ref().map(|s| s.id.clone());

    let items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            // Running work is the assistant busy; a finished-while-away reply
            // waits on you; otherwise the session's kind.
            let compacting_here = app.is_compacting_session(&s.id);
            let streaming_here = app.chat_task_for_session(&s.id).is_some();
            let researching_here = app
                .research
                .running
                .as_ref()
                .is_some_and(|(id, _)| *id == s.id);
            let agent = Style::default().fg(app.theme.accent2);
            let marker = if compacting_here || streaming_here || researching_here {
                Some(Span::styled(format!("{} ", glyph::RUNNING), agent))
            } else if app.unread.contains(&s.id) {
                Some(Span::styled(
                    format!("{} ", glyph::DOT),
                    Style::default().fg(app.theme.accent),
                ))
            } else if s.research_parent_id.is_some() {
                Some(Span::styled(format!("{} ", glyph::LINK), dim))
            } else if s.kind == "research" {
                Some(Span::styled(format!("{} ", glyph::RESEARCH), agent))
            } else {
                None
            };
            // Widths are display cells, so a two-column marker still lines up.
            let when = crate::ui::fmt_created(&s.created_at);
            let marker_w = marker.as_ref().map_or(0, Span::width);
            let title_max = width.saturating_sub(marker_w + crate::ui::style::width(&when) + 2);
            let title = chrome::truncate(&s.title, title_max);
            let gap = width.saturating_sub(
                marker_w + crate::ui::style::width(&title) + crate::ui::style::width(&when),
            );
            let mut top_spans: Vec<Span> = marker.into_iter().collect();
            top_spans.extend([
                Span::styled(
                    title,
                    Style::default()
                        .fg(app.theme.fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" ".repeat(gap)),
                Span::styled(when, dim),
            ]);
            // An explicit label while compaction runs, so ⟳ never needs guessing.
            let slug = s
                .slug
                .clone()
                .unwrap_or_else(|| format!("{}…", &s.id[..8.min(s.id.len())]));
            let mut detail = if compacting_here {
                format!("#{slug} · {} compacting…", glyph::RUNNING)
            } else {
                format!("#{slug} · {}", crate::ui::short_model_label(&s.model))
            };
            if active_id.as_deref() == Some(s.id.as_str()) {
                detail.push_str(" · open");
            }
            let body = Line::from(Span::styled(chrome::truncate(&detail, width), dim));
            ListItem::new(vec![Line::from(top_spans), body, Line::from("")])
        })
        .collect();

    let filtering = !app.session_filter.is_empty();
    let count = if app.session_mode != SessionMode::Browse {
        String::new()
    } else if filtering {
        format!("{} of {}", sessions.len(), app.caches.sessions.len())
    } else {
        format!(
            "{} chat{}",
            sessions.len(),
            if sessions.len() == 1 { "" } else { "s" }
        )
    };
    let (title, hints) = match app.session_mode {
        SessionMode::Rename => (
            "Rename chat",
            vec![hint(keys::ENTER, "save"), hint(keys::ESC, "cancel")],
        ),
        SessionMode::ConfirmDelete => (
            "Delete chat?",
            vec![hint(ctrl('d'), "delete"), hint(keys::ESC, "cancel")],
        ),
        SessionMode::Browse => {
            let mut hints = if sessions.is_empty() {
                Vec::new()
            } else {
                vec![
                    hint(keys::MOVE, ""),
                    hint(keys::ENTER, "open"),
                    hint(ctrl('r'), "rename"),
                    hint(ctrl('d'), "delete"),
                ]
            };
            hints.push(hint(keys::ESC, if filtering { "clear" } else { "close" }));
            ("Sessions", hints)
        }
    };
    let tone = if app.session_mode == SessionMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title,
            count: &count,
            hints: &hints,
            tone,
            focused: true,
        },
        app,
    );
    let renaming = app.session_mode == SessionMode::Rename;
    let inner = chrome::render_field(
        f,
        inner,
        if renaming { "✎" } else { "⌕" },
        if renaming {
            &app.session_edit
        } else {
            &app.session_filter
        },
        "Type to filter",
        renaming,
        &app.theme,
    );
    let detail = if app.session_mode == SessionMode::ConfirmDelete {
        let name = app.selected_session().map(|s| s.title).unwrap_or_default();
        format!("“{name}” and its messages will be deleted.")
    } else {
        preview
    };
    let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
    chrome::render_detail(f, detail_area, &detail, &app.theme);
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !sessions.is_empty() {
        state.select(Some(app.session_selected.min(sessions.len() - 1)));
    }
    chrome::render_list(f, list, &mut state, list_area, sessions.len(), 3, app);
    if sessions.is_empty() {
        let message = if filtering {
            format!(
                "No chat matches “{}”. Esc clears the filter.",
                &*app.session_filter
            )
        } else {
            "No chats yet.".to_string()
        };
        chrome::render_empty(f, list_area, &message, &app.theme);
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use super::{
        ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key,
        classify_edit_key,
    };
    use nexus_core::app::SessionMode;
    match app.session_mode {
        SessionMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.session_mode = SessionMode::Browse,
            Some(EditAction::Save) => app.confirm_rename()?,
            Some(EditAction::Backspace) => {
                app.session_edit.pop();
            }
            Some(EditAction::Push(c)) => app.session_edit.push(c),
            None => {}
        },
        SessionMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_delete()?,
            Some(ConfirmDeleteAction::No) => app.session_mode = SessionMode::Browse,
            None => {}
        },
        SessionMode::Browse => {
            if key.code == KeyCode::Enter {
                return app.confirm_session();
            }
            // The search filter's own cursor, select and clipboard keys come
            // before the rest of Browse mode's bindings.
            if app.session_filter.key(key, &mut app.clipboard) {
                return Ok(());
            }
            match classify_browse_key(key, false, true) {
                Some(super::BrowseAction::Close) => {
                    if app.session_filter.is_empty() {
                        app.popup = nexus_core::app::Popup::None;
                    } else {
                        app.session_filter.clear();
                        app.session_selected = 0;
                    }
                }
                Some(super::BrowseAction::MoveUp) => app.move_session_selection(-1),
                Some(super::BrowseAction::MoveDown) => app.move_session_selection(1),
                Some(
                    a @ (super::BrowseAction::PageUp
                    | super::BrowseAction::PageDown
                    | super::BrowseAction::Top
                    | super::BrowseAction::Bottom),
                ) => {
                    super::apply_page(|d| app.move_session_selection(d), a, 10);
                }
                Some(super::BrowseAction::Rename) => app.start_rename(),
                Some(super::BrowseAction::ConfirmDelete) => {
                    app.session_mode = SessionMode::ConfirmDelete;
                }
                Some(super::BrowseAction::Backspace) => app.session_filter_pop(),
                Some(super::BrowseAction::Filter(c)) => app.session_filter_push(c),
                Some(super::BrowseAction::Create) | None => {}
            }
        }
    }
    Ok(())
}
