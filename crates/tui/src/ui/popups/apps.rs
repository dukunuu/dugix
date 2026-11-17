use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use crossterm::event::KeyModifiers;
use dugix_core::app::AppsMode;

use super::chrome::{self, PopupFrame, ctrl, hint, keys};
use super::list_filter;

/// The file-name and rename prompts: an input line over an empty list.
fn render_prompt(f: &mut Frame, app: &AppView, area: ratatui::layout::Rect) -> bool {
    let (title, verb) = match app.apps_mode {
        AppsMode::EditFile => ("Edit app file", "open"),
        AppsMode::RenamePanel => ("Rename panel", "save"),
        _ => return false,
    };
    let hints = [hint(keys::ENTER, verb), hint(keys::ESC, "cancel")];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title,
            count: "",
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    chrome::render_field(f, inner, "✎", &app.apps_edit, "", true, &app.theme);
    true
}

/// Web apps first, then pinned panels; only the indices in `keep`.
fn rows(app: &AppView, content_w: usize, keep: &[usize]) -> Vec<ListItem<'static>> {
    let dim = Style::default().fg(app.theme.fg_dim);
    let row = |name: &str, meta: &str| {
        ListItem::new(vec![
            Line::from(Span::styled(
                chrome::truncate(name, content_w.saturating_sub(1)),
                Style::default().fg(app.theme.fg),
            )),
            Line::from(Span::styled(
                format!("  {}", chrome::truncate(meta, content_w.saturating_sub(3))),
                dim,
            )),
            Line::from(""),
        ])
    };
    let web = app.caches.apps.iter().map(|name| {
        let n = app.app_file_count(name);
        let url = app
            .app_url(name)
            .unwrap_or_else(|| "server not running".to_string());
        row(
            name,
            &format!("{n} file{} · {url}", if n == 1 { "" } else { "s" }),
        )
    });
    let panels = app
        .pinned_apps
        .iter()
        .map(|pinned| row(&pinned.name, &format!("panel · {}", pinned.title)));
    let all: Vec<_> = web.chain(panels).collect();
    keep.iter()
        .filter_map(|&index| all.get(index).cloned())
        .collect()
}

/// Title and footer keys for the list and its delete prompt.
fn frame_text(app: &AppView, shown: bool, filtering: bool) -> (&'static str, Vec<chrome::Hint>) {
    // A pinned panel is only unpinned, so its prompt says so.
    let verb = if app.selected_pinned().is_some() {
        "unpin"
    } else {
        "delete"
    };
    if app.apps_mode == AppsMode::ConfirmDelete {
        (
            if verb == "unpin" {
                "Unpin panel?"
            } else {
                "Delete app?"
            },
            vec![hint(ctrl('d'), verb), hint(keys::ESC, "cancel")],
        )
    } else {
        let mut hints = Vec::new();
        if shown {
            hints.extend([hint(keys::MOVE, ""), hint(keys::ENTER, "open")]);
            hints.push(if app.selected_pinned().is_some() {
                hint(ctrl('r'), "rename")
            } else {
                hint(ctrl('e'), "edit")
            });
            hints.push(hint(ctrl('d'), verb));
        }
        hints.push(hint(keys::ESC, if filtering { "clear" } else { "close" }));
        ("Apps", hints)
    }
}

pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "apps", chrome::STANDARD);
    if render_prompt(f, app, area) {
        return;
    }
    let visible = app.apps_rows();
    let filtering = !app.apps_filter.is_empty();
    let shown = list_filter::shown(&visible, app.apps_selected);
    let total = visible.len();
    let content_w = (area.width.saturating_sub(5 + 2 * chrome::PAD)) as usize;
    let items = rows(app, content_w, &visible);

    let count = if app.apps_mode == AppsMode::Browse {
        list_filter::count(total, app.apps_total(), filtering, "app")
    } else {
        String::new()
    };
    let (title, hints) = frame_text(app, shown.is_some(), filtering);
    let tone = if app.apps_mode == AppsMode::ConfirmDelete {
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
    if app.apps_mode == AppsMode::Browse {
        inner = chrome::render_field(
            f,
            inner,
            "⌕",
            &app.apps_filter,
            "Type to filter",
            false,
            &app.theme,
        );
    }
    if app.apps_mode == AppsMode::ConfirmDelete {
        let detail = app.selected_pinned().map_or_else(
            || {
                let name = app
                    .caches
                    .apps
                    .get(app.apps_selected)
                    .cloned()
                    .unwrap_or_default();
                format!("App “{name}” and all its files will be removed.")
            },
            |pinned| format!("Panel app “{}” will be unpinned.", pinned.name),
        );
        let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
        chrome::render_detail(f, detail_area, &detail, &app.theme);
        inner = list_area;
    }
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    state.select(shown);
    chrome::render_list(f, list, &mut state, inner, total, 3, app);
    if total == 0 {
        let message = if filtering {
            format!(
                "No app matches “{}”. Esc clears the filter.",
                app.apps_filter
            )
        } else {
            "No apps yet. Ask the model to build one.".to_string()
        };
        chrome::render_empty(f, inner, &message, &app.theme);
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key,
        classify_confirm_delete_key, classify_edit_key,
    };
    match app.apps_mode {
        AppsMode::EditFile => match classify_edit_key(key) {
            Some(EditAction::Cancel) => {
                app.apps_mode = AppsMode::Browse;
            }
            Some(EditAction::Save) => app.confirm_app_edit(),
            Some(EditAction::Backspace) => {
                app.apps_edit.pop();
            }
            Some(EditAction::Push(c)) => app.apps_edit.push(c),
            None => {}
        },
        AppsMode::RenamePanel => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.apps_mode = AppsMode::Browse,
            Some(EditAction::Save) => app.confirm_panel_rename(),
            Some(EditAction::Backspace) => {
                app.apps_edit.pop();
            }
            Some(EditAction::Push(c)) => app.apps_edit.push(c),
            None => {}
        },
        AppsMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_app_delete()?,
            Some(ConfirmDeleteAction::No) => app.apps_mode = AppsMode::Browse,
            None => {}
        },
        AppsMode::Browse => {
            // Letters filter; an action needs a highlighted row.
            let selected = list_filter::shown(&app.apps_rows(), app.apps_selected).is_some();
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            if !selected
                && (key.code == KeyCode::Enter
                    || (ctrl && matches!(key.code, KeyCode::Char('e' | 'r' | 'd'))))
            {
                return Ok(());
            }
            if key.code == KeyCode::Enter {
                app.open_selected_app();
                return Ok(());
            }
            if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.start_app_edit();
                return Ok(());
            }
            if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.start_panel_rename();
                return Ok(());
            }
            match classify_browse_key(key, false, false) {
                Some(BrowseAction::Close) if !app.apps_filter.is_empty() => app.apps_filter.clear(),
                Some(BrowseAction::Close) => app.popup = dugix_core::app::Popup::None,
                Some(BrowseAction::Filter(c)) => {
                    app.apps_filter.push(c);
                    app.apps_selected = list_filter::first(&app.apps_rows(), app.apps_selected);
                }
                Some(BrowseAction::Backspace) => {
                    app.apps_filter.pop();
                    app.apps_selected = list_filter::first(&app.apps_rows(), app.apps_selected);
                }
                Some(BrowseAction::MoveUp) => app.move_apps_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_apps_selection(1),
                Some(
                    a @ (BrowseAction::PageUp
                    | BrowseAction::PageDown
                    | BrowseAction::Top
                    | BrowseAction::Bottom),
                ) => {
                    super::apply_page(|d| app.move_apps_selection(d), a, 10);
                }
                Some(BrowseAction::ConfirmDelete) if app.apps_total() > 0 => {
                    app.apps_mode = AppsMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
