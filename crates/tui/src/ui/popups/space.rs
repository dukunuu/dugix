use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;

use super::chrome::{self, PopupFrame, ctrl, hint, keys};

#[allow(clippy::too_many_lines)] // rows + browse / edit / confirm chrome
pub fn render(f: &mut Frame, app: &AppView) {
    use nexus_core::app::SpaceMode;
    use nexus_core::db::DEFAULT_SPACE;
    let area = chrome::popup_area(app, f.area(), "space", chrome::STANDARD);

    let dim = Style::default().fg(app.theme.fg_dim);
    let spaces = app.filtered_spaces();
    let items: Vec<ListItem> = spaces
        .iter()
        .map(|s| {
            let n = app.db.count_sessions(&s.id).unwrap_or(0);
            let mark = if s.name == app.active_space.name {
                format!("{} ", crate::ui::style::glyph::DOT)
            } else {
                "  ".to_string()
            };
            let base = if s.name == DEFAULT_SPACE {
                format!("{} (default)", s.name)
            } else {
                s.name.clone()
            };
            let meta = format!(
                "  {n} session{}  · {}",
                if n == 1 { "" } else { "s" },
                crate::ui::fmt_created(&s.created_at)
            );
            // border 2 + scrollbar 1 + highlight 2
            let content_w = area.width.saturating_sub(5 + 2 * chrome::PAD) as usize;
            let name = chrome::truncate(&base, content_w.saturating_sub(meta.chars().count() + 1));
            let line = Line::from(vec![
                Span::styled(format!("{mark}{name}"), Style::default().fg(app.theme.fg)),
                Span::styled(meta, dim),
            ]);
            ListItem::new(line)
        })
        .collect();

    let filtering = !app.space_filter.is_empty();
    let count = if app.space_mode != SpaceMode::Browse {
        String::new()
    } else if filtering {
        format!("{} of {}", spaces.len(), app.spaces_cache.len())
    } else {
        format!(
            "{} space{}",
            spaces.len(),
            if spaces.len() == 1 { "" } else { "s" }
        )
    };
    let (title, hints) = match app.space_mode {
        SpaceMode::Create => (
            "New space",
            vec![hint(keys::ENTER, "create"), hint(keys::ESC, "cancel")],
        ),
        SpaceMode::Rename => (
            "Rename space",
            vec![hint(keys::ENTER, "save"), hint(keys::ESC, "cancel")],
        ),
        SpaceMode::ConfirmDelete => (
            "Delete space?",
            vec![hint(ctrl('d'), "delete"), hint(keys::ESC, "cancel")],
        ),
        SpaceMode::Browse => (
            "Spaces",
            vec![
                hint(keys::MOVE, ""),
                hint(keys::ENTER, "switch"),
                hint(ctrl('n'), "new"),
                hint(ctrl('r'), "rename"),
                hint(ctrl('d'), "delete"),
                hint(keys::ESC, if filtering { "clear" } else { "close" }),
            ],
        ),
    };
    let tone = if app.space_mode == SpaceMode::ConfirmDelete {
        chrome::Tone::Danger
    } else {
        chrome::Tone::Normal
    };
    let mut inner = chrome::render_popup(
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
    if app.space_mode == SpaceMode::ConfirmDelete {
        let name = app.selected_space().map(|s| s.name).unwrap_or_default();
        let detail = format!("“{name}” will be deleted. Its chats move to the default space.");
        let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
        chrome::render_detail(f, detail_area, &detail, &app.theme);
        inner = list_area;
    } else {
        let editing = app.space_mode != SpaceMode::Browse;
        inner = chrome::render_field(
            f,
            inner,
            if editing { "✎" } else { "⌕" },
            if editing {
                &app.space_edit
            } else {
                &app.space_filter
            },
            "Type to filter",
            editing,
            &app.theme,
        );
    }
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !spaces.is_empty() {
        state.select(Some(app.space_selected.min(spaces.len() - 1)));
    }
    chrome::render_list(f, list, &mut state, inner, spaces.len(), 1, app);
    if spaces.is_empty() {
        chrome::render_empty(
            f,
            inner,
            &format!(
                "No space matches “{}”. Esc clears the filter.",
                &*app.space_filter
            ),
            &app.theme,
        );
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use super::{
        ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key,
        classify_edit_key,
    };
    use nexus_core::app::SpaceMode;
    use nexus_core::db::DEFAULT_SPACE;
    match app.space_mode {
        SpaceMode::Create | SpaceMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.space_mode = SpaceMode::Browse,
            Some(EditAction::Save) => match app.space_mode {
                SpaceMode::Create => app.confirm_space_create()?,
                _ => app.confirm_space_rename()?,
            },
            Some(EditAction::Backspace) => {
                app.space_edit.pop();
            }
            Some(EditAction::Push(c)) => app.space_edit.push(c),
            None => {}
        },
        SpaceMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_space_delete()?,
            Some(ConfirmDeleteAction::No) => app.space_mode = SpaceMode::Browse,
            None => {}
        },
        SpaceMode::Browse => {
            if key.code == KeyCode::Enter {
                app.confirm_space();
                return Ok(());
            }
            if !matches!(key.code, KeyCode::Home | KeyCode::End)
                && app.space_filter.key(key, &mut app.clipboard)
            {
                return Ok(());
            }
            match classify_browse_key(key, true, true) {
                Some(super::BrowseAction::Close) => {
                    if app.space_filter.is_empty() {
                        app.popup = nexus_core::app::Popup::None;
                    } else {
                        app.space_filter.clear();
                        app.space_selected = 0;
                    }
                }
                Some(super::BrowseAction::MoveUp) => app.move_space_selection(-1),
                Some(super::BrowseAction::MoveDown) => app.move_space_selection(1),
                Some(
                    a @ (super::BrowseAction::PageUp
                    | super::BrowseAction::PageDown
                    | super::BrowseAction::Top
                    | super::BrowseAction::Bottom),
                ) => {
                    super::apply_page(|d| app.move_space_selection(d), a, 10);
                }
                Some(super::BrowseAction::Create) => app.start_space_create(),
                Some(super::BrowseAction::Rename) => app.start_space_rename(),
                // The default space is never deletable.
                Some(super::BrowseAction::ConfirmDelete)
                    if app
                        .selected_space()
                        .is_some_and(|s| s.name != DEFAULT_SPACE) =>
                {
                    app.space_mode = SpaceMode::ConfirmDelete;
                }
                Some(super::BrowseAction::ConfirmDelete) | None => {}
                Some(super::BrowseAction::Backspace) => app.space_filter_pop(),
                Some(super::BrowseAction::Filter(c)) => app.space_filter_push(c),
            }
        }
    }
    Ok(())
}
