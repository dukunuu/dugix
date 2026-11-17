// Casts here are on terminal-bounded values (dims, byte colors, glyph counts),
// never on unbounded user data.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use dugix_core::app::{FilesMode, FilesTab, ImagesMode, ScriptsMode};

use super::chrome::{self, PopupFrame, ctrl, hint, keys};
use super::list_filter;

pub fn render(f: &mut Frame, app: &AppView) {
    match app.files_tab {
        FilesTab::Files => render_files(f, app),
        FilesTab::Images => render_images(f, app),
        FilesTab::Scripts => render_scripts(f, app),
    }
}

#[allow(clippy::too_many_lines)] // tabs + picker + browse rows
fn render_files(f: &mut Frame, app: &AppView) {
    use dugix_core::app::FilesMode;
    let area = chrome::popup_area(app, f.area(), "files", chrome::STANDARD);

    if app.files_mode == FilesMode::Pick {
        render_picker(f, app, area);
        return;
    }

    let dim = Style::default().fg(app.theme.fg_dim);
    let content_w = (area.width.saturating_sub(5 + 2 * chrome::PAD)) as usize; // border + scrollbar + highlight
    let visible = app.files_rows();
    let filtering = !app.files_filter.is_empty();
    let shown = list_filter::shown(&visible, app.files_selected);
    let items: Vec<ListItem> = visible
        .iter()
        .map(|&index| &app.caches.files[index])
        .map(|file| {
            let ok = dugix_core::extract::status_is_ok(&file.status);
            let status_style = if ok {
                dim
            } else {
                Style::default().fg(app.theme.warning)
            };
            // Status can carry long error text, so cap the meta column.
            let meta = chrome::truncate(
                &format!(
                    "  {}  {}",
                    dugix_core::app::human_size(file.size.unsigned_abs()),
                    file.status
                ),
                36,
            );
            let name = chrome::truncate_middle(
                &file.name,
                content_w.saturating_sub(crate::ui::style::width(&meta) + 1),
            );
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(meta, status_style),
            ]))
        })
        .collect();

    let total = visible.len();
    let count = if app.files_mode == FilesMode::Browse {
        list_filter::count(total, app.caches.files.len(), filtering, "file")
    } else {
        String::new()
    };
    let (title, hints) = match app.files_mode {
        FilesMode::Add => (
            "Import file",
            vec![hint(keys::ENTER, "import"), hint(keys::ESC, "cancel")],
        ),
        FilesMode::Rename => (
            "Rename file",
            vec![hint(keys::ENTER, "save"), hint(keys::ESC, "cancel")],
        ),
        FilesMode::ConfirmDelete => (
            "Delete file?",
            vec![hint(ctrl('d'), "delete"), hint(keys::ESC, "cancel")],
        ),
        FilesMode::Browse => {
            let mut hints = Vec::new();
            if shown.is_some() {
                hints.extend([hint(keys::MOVE, ""), hint(keys::ENTER, "open")]);
            }
            hints.push(hint(keys::TAB, "tab"));
            if !filtering {
                hints.push(hint("/", "find"));
            }
            hints.push(hint(ctrl('n'), "add"));
            if shown.is_some() {
                hints.extend([hint(ctrl('r'), "rename"), hint(ctrl('d'), "delete")]);
            }
            hints.push(hint(keys::ESC, if filtering { "clear" } else { "close" }));
            ("Files", hints)
        }
        FilesMode::Pick => unreachable!(),
    };
    let tone = if app.files_mode == FilesMode::ConfirmDelete {
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
    inner = chrome::render_tabs(f, inner, &["Files", "Images", "Scripts"], 0, &app.theme);
    if matches!(app.files_mode, FilesMode::Add | FilesMode::Rename) {
        inner = chrome::render_field(f, inner, "✎", &app.files_edit, "", true, &app.theme);
    } else if app.files_mode == FilesMode::Browse {
        inner = chrome::render_field(
            f,
            inner,
            "⌕",
            &app.files_filter,
            "Type to filter, / to find",
            false,
            &app.theme,
        );
    }
    if app.files_mode == FilesMode::ConfirmDelete {
        let name = app
            .caches
            .files
            .get(app.files_selected)
            .map_or("", |file| file.name.as_str());
        let detail = format!("“{name}” will be removed.");
        let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
        chrome::render_detail(f, detail_area, &detail, &app.theme);
        inner = list_area;
    }
    // Embedding health and re-index progress, one dim line above the list.
    for text in [&app.index.embedding_status, &app.index.chat_index_status] {
        if app.files_mode == FilesMode::Browse && !text.is_empty() && inner.height > 3 {
            let status = Rect { height: 1, ..inner };
            f.render_widget(
                ratatui::widgets::Paragraph::new(Span::styled(
                    chrome::truncate(text, inner.width.saturating_sub(2) as usize),
                    dim,
                )),
                status,
            );
            inner = Rect {
                y: inner.y + 1,
                height: inner.height - 1,
                ..inner
            };
        }
    }
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    state.select(shown);
    chrome::render_list(f, list, &mut state, inner, total, 1, app);
    if total == 0 {
        let message = if filtering {
            format!(
                "No file matches “{}”. Esc clears the filter.",
                app.files_filter
            )
        } else {
            "No files yet. ^N adds one, or drop files here.".to_string()
        };
        chrome::render_empty(f, inner, &message, &app.theme);
    }
}

/// The find picker: recent files, search results or path completions.
fn render_picker(f: &mut Frame, app: &AppView, area: Rect) {
    let home = &app.picker_home;
    let max = (area.width.saturating_sub(5 + 2 * chrome::PAD)) as usize;
    let dim = Style::default().fg(app.theme.fg_dim);
    let items: Vec<ListItem> = app
        .picker_results
        .iter()
        .map(|found| {
            let name = found
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (name, style) = if found.is_dir {
                (format!("{name}/"), Style::default().fg(app.theme.accent))
            } else {
                (name, Style::default().fg(app.theme.fg))
            };
            let name = chrome::truncate_middle(&name, max);
            let folder = found
                .path
                .parent()
                .map(|dir| dugix_core::filesearch::tilde(dir, home))
                .unwrap_or_default();
            let folder = chrome::truncate_middle(
                &format!("  {folder}"),
                max.saturating_sub(crate::ui::style::width(&name)),
            );
            ListItem::new(Line::from(vec![
                Span::styled(name, style),
                Span::styled(folder, dim),
            ]))
        })
        .collect();
    let hints = [
        hint(keys::MOVE, ""),
        hint(keys::ENTER, "import"),
        hint(keys::TAB, "complete"),
        hint(keys::ESC, "back"),
    ];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Find a file to import",
            count: if app.picker_rx.is_some() {
                "Searching…"
            } else {
                ""
            },
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let inner = chrome::render_tabs(f, inner, &["Files", "Images", "Scripts"], 0, &app.theme);
    let inner = chrome::render_field(
        f,
        inner,
        "⌕",
        &app.picker_query,
        "Type to search, ~/ or / for a path",
        false,
        &app.theme,
    );
    let list = chrome::standard_list(items, &app.theme);
    let total = app.picker_results.len();
    let mut state = ListState::default();
    if total > 0 {
        state.select(Some(app.picker_selected.min(total - 1)));
    }
    chrome::render_list(f, list, &mut state, inner, total, 1, app);
    if total == 0 {
        let empty = if app.picker_rx.is_some() {
            "Searching for files."
        } else if app.picker_query.trim().is_empty() {
            "Nothing recent in Downloads, Desktop or Documents. Type to search."
        } else {
            "No matches."
        };
        chrome::render_empty(f, inner, empty, &app.theme);
    }
}

#[allow(clippy::too_many_lines)] // image rows + browse / confirm chrome
fn render_images(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "files", chrome::STANDARD);
    let dim = Style::default().fg(app.theme.fg_dim);

    let items: Vec<ListItem> = app
        .caches
        .images
        .iter()
        .map(|img| {
            let created = crate::ui::fmt_created(&img.modified);
            let meta = format!("  {}  {created}", dugix_core::app::human_size(img.size));
            let name = chrome::truncate_middle(
                &img.name,
                (area.width.saturating_sub(5 + 2 * chrome::PAD) as usize)
                    .saturating_sub(crate::ui::style::width(&meta) + 1),
            );
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(meta, dim),
            ]))
        })
        .collect();

    let total = app.caches.images.len();
    let count = if app.images_mode == ImagesMode::Browse {
        format!("{total} image{}", if total == 1 { "" } else { "s" })
    } else {
        String::new()
    };
    let (title, hints) = match app.images_mode {
        ImagesMode::ConfirmDelete => (
            "Delete image?",
            vec![hint(ctrl('d'), "delete"), hint(keys::ESC, "cancel")],
        ),
        ImagesMode::Browse => (
            "Files",
            vec![
                hint(keys::MOVE, ""),
                hint(keys::ENTER, "open"),
                hint(keys::TAB, "tab"),
                hint(ctrl('d'), "delete"),
                hint(keys::ESC, "close"),
            ],
        ),
    };
    let tone = if app.images_mode == ImagesMode::ConfirmDelete {
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
    let mut inner = chrome::render_tabs(f, inner, &["Files", "Images", "Scripts"], 1, &app.theme);
    if app.images_mode == ImagesMode::ConfirmDelete {
        let name = app
            .caches
            .images
            .get(app.images_selected)
            .map_or("", |image| image.name.as_str());
        let detail = format!("“{name}” will be removed.");
        let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
        chrome::render_detail(f, detail_area, &detail, &app.theme);
        inner = list_area;
    }
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.caches.images.is_empty() {
        state.select(Some(app.images_selected.min(app.caches.images.len() - 1)));
    }
    chrome::render_list(f, list, &mut state, inner, app.caches.images.len(), 1, app);
    if total == 0 {
        chrome::render_empty(
            f,
            inner,
            "No images yet. Paste one, or have the model generate one.",
            &app.theme,
        );
    }
}

#[allow(clippy::too_many_lines)] // script rows + browse / edit / confirm chrome
fn render_scripts(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "files", chrome::STANDARD);
    let dim = Style::default().fg(app.theme.fg_dim);

    let items: Vec<ListItem> = app
        .caches
        .scripts
        .iter()
        .map(|s| {
            let created = crate::ui::fmt_created(&s.modified);
            let meta = format!("  {}  {created}", dugix_core::app::human_size(s.size));
            let name = chrome::truncate_middle(
                &s.name,
                (area.width.saturating_sub(5 + 2 * chrome::PAD) as usize)
                    .saturating_sub(crate::ui::style::width(&meta) + 1),
            );
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(app.theme.fg)),
                Span::styled(meta, dim),
            ]))
        })
        .collect();

    let total = app.caches.scripts.len();
    let count = if app.scripts_mode == ScriptsMode::Browse {
        format!("{total} script{}", if total == 1 { "" } else { "s" })
    } else {
        String::new()
    };
    let (title, hints) = match app.scripts_mode {
        ScriptsMode::Create => (
            "New script",
            vec![hint(keys::ENTER, "create"), hint(keys::ESC, "cancel")],
        ),
        ScriptsMode::Rename => (
            "Rename script",
            vec![hint(keys::ENTER, "save"), hint(keys::ESC, "cancel")],
        ),
        ScriptsMode::ConfirmDelete => (
            "Delete script?",
            vec![hint(ctrl('d'), "delete"), hint(keys::ESC, "cancel")],
        ),
        ScriptsMode::Browse => (
            "Files",
            vec![
                hint(keys::MOVE, ""),
                hint(keys::ENTER, "open"),
                hint(keys::TAB, "tab"),
                hint(ctrl('n'), "new"),
                hint(ctrl('r'), "rename"),
                hint(ctrl('d'), "delete"),
                hint(keys::ESC, "close"),
            ],
        ),
    };
    let tone = if app.scripts_mode == ScriptsMode::ConfirmDelete {
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
    let mut inner = chrome::render_tabs(f, inner, &["Files", "Images", "Scripts"], 2, &app.theme);
    if matches!(app.scripts_mode, ScriptsMode::Create | ScriptsMode::Rename) {
        chrome::render_field(f, inner, "✎", &app.scripts_edit, "", true, &app.theme);
        return;
    }
    if app.scripts_mode == ScriptsMode::ConfirmDelete {
        let name = app
            .caches
            .scripts
            .get(app.scripts_selected)
            .map_or("", |script| script.name.as_str());
        let detail = format!("“{name}” will be removed.");
        let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
        chrome::render_detail(f, detail_area, &detail, &app.theme);
        inner = list_area;
    }
    let list = chrome::standard_list(items, &app.theme);
    let mut state = ListState::default();
    if !app.caches.scripts.is_empty() {
        state.select(Some(app.scripts_selected.min(app.caches.scripts.len() - 1)));
    }
    chrome::render_list(f, list, &mut state, inner, app.caches.scripts.len(), 1, app);
    if total == 0 {
        chrome::render_empty(f, inner, "No scripts yet. ^N creates one.", &app.theme);
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    let picking = app.files_tab == FilesTab::Files && app.files_mode == FilesMode::Pick;
    if key.code == KeyCode::Tab && !picking {
        app.files_tab = match app.files_tab {
            FilesTab::Files => FilesTab::Images,
            FilesTab::Images => FilesTab::Scripts,
            FilesTab::Scripts => FilesTab::Files,
        };
        match app.files_tab {
            FilesTab::Files => app.rescan_files(),
            FilesTab::Images => app.refresh_images(),
            FilesTab::Scripts => app.refresh_scripts(),
        }
        app.files_mode = FilesMode::Browse;
        return Ok(());
    }

    // The Files list filters as you type, so j/k move only on the other tabs.
    let browsing = match app.files_tab {
        FilesTab::Files => false,
        FilesTab::Images => app.images_mode == ImagesMode::Browse,
        FilesTab::Scripts => app.scripts_mode == ScriptsMode::Browse,
    };
    let key = if browsing && !super::is_chord(key) {
        match key.code {
            KeyCode::Char('j') => KeyCode::Down.into(),
            KeyCode::Char('k') => KeyCode::Up.into(),
            _ => key,
        }
    } else {
        key
    };
    match app.files_tab {
        FilesTab::Files => handle_files_key(app, key),
        FilesTab::Images => handle_images_key(app, key),
        FilesTab::Scripts => handle_scripts_key(app, key),
    }
}

fn handle_files_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key,
        classify_confirm_delete_key, classify_edit_key,
    };
    match app.files_mode {
        FilesMode::Add | FilesMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.files_mode = FilesMode::Browse,
            Some(EditAction::Save) if app.files_mode == FilesMode::Add => {
                app.confirm_files_add();
            }
            Some(EditAction::Save) => {
                app.confirm_files_rename();
            }
            Some(EditAction::Backspace) => {
                app.files_edit.pop();
            }
            Some(EditAction::Push(c)) => app.files_edit.push(c),
            None => {}
        },
        FilesMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_files_delete()?,
            Some(ConfirmDeleteAction::No) => app.files_mode = FilesMode::Browse,
            None => {}
        },
        FilesMode::Browse => {
            // Letters filter; an action on a file needs a highlighted row.
            let selected = list_filter::shown(&app.files_rows(), app.files_selected).is_some();
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            if !selected
                && (key.code == KeyCode::Enter
                    || (ctrl && matches!(key.code, KeyCode::Char('o' | 'r' | 'd'))))
            {
                return Ok(());
            }
            if key.code == KeyCode::Enter {
                app.open_selected_file();
                return Ok(());
            }
            if key.code == KeyCode::Char('o') && ctrl {
                app.reextract_selected_file();
                return Ok(());
            }
            // `/` finds a file to import until a filter is typed, then it is text.
            if key.code == KeyCode::Char('/')
                && !super::is_chord(key)
                && app.files_filter.is_empty()
            {
                app.open_file_picker();
                return Ok(());
            }
            match classify_browse_key(key, true, true) {
                Some(BrowseAction::Close) if !app.files_filter.is_empty() => {
                    app.files_filter.clear();
                }
                Some(BrowseAction::Close) => app.popup = dugix_core::app::Popup::None,
                Some(BrowseAction::Filter(c)) => {
                    app.files_filter.push(c);
                    app.files_selected = list_filter::first(&app.files_rows(), app.files_selected);
                }
                Some(BrowseAction::Backspace) => {
                    app.files_filter.pop();
                    app.files_selected = list_filter::first(&app.files_rows(), app.files_selected);
                }
                Some(BrowseAction::MoveUp) => app.move_files_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_files_selection(1),
                Some(
                    a @ (BrowseAction::PageUp
                    | BrowseAction::PageDown
                    | BrowseAction::Top
                    | BrowseAction::Bottom),
                ) => super::apply_page(|d| app.move_files_selection(d), a, 10),
                Some(BrowseAction::Create) => app.import_files(),
                Some(BrowseAction::Rename) => app.start_files_rename(),
                Some(BrowseAction::ConfirmDelete) if !app.caches.files.is_empty() => {
                    app.files_mode = FilesMode::ConfirmDelete;
                }
                _ => {}
            }
        }
        FilesMode::Pick => match key.code {
            KeyCode::Esc => {
                app.picker_rx = None;
                app.files_mode = FilesMode::Browse;
            }
            KeyCode::Enter => app.picker_enter(),
            KeyCode::Tab => app.picker_complete(),
            KeyCode::Backspace => app.picker_backspace(),
            KeyCode::Up => app.move_picker_selection(-1),
            KeyCode::Down => app.move_picker_selection(1),
            KeyCode::PageUp => app.move_picker_selection(-10),
            KeyCode::PageDown => app.move_picker_selection(10),
            KeyCode::Home => app.move_picker_selection(i32::MIN / 2),
            KeyCode::End => app.move_picker_selection(i32::MAX / 2),
            KeyCode::Char(c) if !super::is_chord(key) => app.picker_push(c),
            _ => {}
        },
    }
    Ok(())
}

fn handle_images_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, classify_browse_key, classify_confirm_delete_key,
    };
    match app.images_mode {
        ImagesMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_images_delete()?,
            Some(ConfirmDeleteAction::No) => app.images_mode = ImagesMode::Browse,
            None => {}
        },
        ImagesMode::Browse => {
            if key.code == KeyCode::Enter && !app.caches.images.is_empty() {
                app.open_selected_image();
                return Ok(());
            }
            match classify_browse_key(key, false, false) {
                Some(BrowseAction::Close) => app.popup = dugix_core::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_images_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_images_selection(1),
                Some(
                    a @ (BrowseAction::PageUp
                    | BrowseAction::PageDown
                    | BrowseAction::Top
                    | BrowseAction::Bottom),
                ) => super::apply_page(|d| app.move_images_selection(d), a, 10),
                Some(BrowseAction::ConfirmDelete) if !app.caches.images.is_empty() => {
                    app.images_mode = ImagesMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_scripts_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    use super::{
        BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key,
        classify_confirm_delete_key, classify_edit_key,
    };
    match app.scripts_mode {
        ScriptsMode::Create | ScriptsMode::Rename => match classify_edit_key(key) {
            Some(EditAction::Cancel) => {
                if app.scripts_mode == ScriptsMode::Rename {
                    app.scripts_edit.clear();
                }
                app.scripts_mode = ScriptsMode::Browse;
            }
            Some(EditAction::Save) if app.scripts_mode == ScriptsMode::Create => {
                app.confirm_script_create()?;
            }
            Some(EditAction::Save) => {
                app.confirm_script_rename();
            }
            Some(EditAction::Backspace) => {
                app.scripts_edit.pop();
            }
            Some(EditAction::Push(c)) => app.scripts_edit.push(c),
            None => {}
        },
        ScriptsMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_script_delete()?,
            Some(ConfirmDeleteAction::No) => app.scripts_mode = ScriptsMode::Browse,
            None => {}
        },
        ScriptsMode::Browse => {
            if key.code == KeyCode::Enter && !app.caches.scripts.is_empty() {
                app.open_selected_script();
                return Ok(());
            }
            match classify_browse_key(key, true, true) {
                Some(BrowseAction::Close) => app.popup = dugix_core::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_scripts_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_scripts_selection(1),
                Some(
                    a @ (BrowseAction::PageUp
                    | BrowseAction::PageDown
                    | BrowseAction::Top
                    | BrowseAction::Bottom),
                ) => super::apply_page(|d| app.move_scripts_selection(d), a, 10),
                Some(BrowseAction::Create) => app.start_script_create(),
                Some(BrowseAction::Rename) => app.start_script_rename(),
                Some(BrowseAction::ConfirmDelete) if !app.caches.scripts.is_empty() => {
                    app.scripts_mode = ScriptsMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
