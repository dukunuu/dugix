//! `/library`: every deck and doc of the space, newest first. The listing, file
//! operations and dialogs are `dugix_core::canvas`'s; this module only draws.

use std::path::PathBuf;

use anyhow::Result;
use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dugix_core::app::{Popup, clamp_cursor};
use dugix_core::canvas::dialogs::{self, Dialogs};
use dugix_core::canvas::{self, Entry, files, settings};
use dugix_core::deck::link::Kind;
use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListItem, ListState};

use crate::app_view::AppView;
use crate::ui::style::SEP;

use super::chrome::{self, PopupFrame, ctrl, hint, keys};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Browse,
    ExportAs,
    ConfirmDelete,
}

#[derive(Default)]
pub struct LibraryPopup {
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub mode: Mode,
    pub filter: String,
}

/// The formats `^E` offers, as `(key, word, label)`.
const DECK_FORMATS: &[(char, &str, &str)] = &[
    ('p', "pptx", "pptx"),
    ('f', "pdf", "pdf"),
    ('g', "png", "png zip"),
    ('h', "html", "html"),
    ('j', "json", "json"),
];
const DOC_FORMATS: &[(char, &str, &str)] = &[
    ('w', "docx", "docx"),
    ('f', "pdf", "pdf"),
    ('m', "md", "md"),
    ('h', "html", "html"),
];

const SHEET_FORMATS: &[(char, &str, &str)] = &[('c', "csv", "csv"), ('x', "xlsx", "xlsx")];

fn formats(kind: Kind) -> &'static [(char, &'static str, &'static str)] {
    match kind {
        Kind::Deck => DECK_FORMATS,
        Kind::Doc => DOC_FORMATS,
        Kind::Sheet => SHEET_FORMATS,
    }
}

fn files_dir(app: &AppView) -> PathBuf {
    app.space.files_dir(&app.active_space.name)
}

/// `/library`: list the space and open the popup.
pub fn open(app: &mut AppView) {
    if app.incognito {
        app.push_status("library isn't available in incognito mode");
        return;
    }
    app.library.filter.clear();
    refresh(app);
    app.library.selected = 0;
    app.library.mode = Mode::Browse;
    app.popup = Popup::Library;
}

fn refresh(app: &mut AppView) {
    app.library.entries = canvas::library(&files_dir(app));
    app.library.selected = app
        .library
        .selected
        .min(filtered_entries(app).count().saturating_sub(1));
}

fn filtered_entries(app: &AppView) -> impl Iterator<Item = &Entry> {
    let filter = app.library.filter.to_lowercase();
    app.library
        .entries
        .iter()
        .filter(move |entry| entry.title.to_lowercase().contains(&filter))
}

fn selected(app: &AppView) -> Option<&Entry> {
    filtered_entries(app).nth(app.library.selected)
}

fn rows(app: &AppView, width: usize) -> Vec<ListItem<'static>> {
    let dim = Style::default().fg(app.theme.fg_dim);
    let now = Utc::now();
    filtered_entries(app)
        .map(|entry| {
            let tag = match entry.kind {
                Kind::Deck => "deck ",
                Kind::Doc => "doc  ",
                Kind::Sheet => "sheet",
            };
            let title = Line::from(vec![
                Span::styled(format!("{tag}  "), dim),
                Span::styled(
                    chrome::truncate(&entry.title, width.saturating_sub(7)),
                    Style::default().fg(app.theme.fg),
                ),
            ]);
            let mut meta = vec![
                entry.count_label(),
                canvas::relative_time(entry.modified, now),
            ];
            if let Some(design) = &entry.design {
                meta.push(design.clone());
            }
            let meta = Line::from(Span::styled(format!("      {}", meta.join(SEP)), dim));
            ListItem::new(vec![
                chrome::fit_line(title, width),
                chrome::fit_line(meta, width),
                Line::from(""),
            ])
        })
        .collect()
}

#[allow(clippy::too_many_lines)] // browse / export / confirm chrome
pub fn render(f: &mut Frame, app: &AppView) {
    let area = chrome::popup_area(app, f.area(), "library", chrome::WIDE);
    let content_w = usize::from(area.width.saturating_sub(5 + 2 * chrome::PAD));
    let total = filtered_entries(app).count();
    let filtering = !app.library.filter.is_empty();
    let count = if app.library.mode != Mode::Browse {
        String::new()
    } else if filtering {
        format!("{total} of {}", app.library.entries.len())
    } else {
        format!("{total} document{}", if total == 1 { "" } else { "s" })
    };
    let (title, hints, tone) = match (app.library.mode, selected(app)) {
        (Mode::ConfirmDelete, Some(_)) => (
            "Delete document?".to_string(),
            vec![hint(ctrl('d'), "delete"), hint(keys::ESC, "cancel")],
            chrome::Tone::Danger,
        ),
        (Mode::ExportAs, Some(entry)) => {
            let mut hints: Vec<_> = formats(entry.kind)
                .iter()
                .map(|(key, word, _)| hint(key.to_string(), *word))
                .collect();
            hints.push(hint(keys::ESC, "back"));
            (
                format!("Export “{}”", entry.title),
                hints,
                chrome::Tone::Normal,
            )
        }
        _ => (
            "Library".to_string(),
            vec![
                hint(keys::MOVE, ""),
                hint(keys::ENTER, "open"),
                hint(ctrl('e'), "export"),
                hint(ctrl('d'), "delete"),
                hint(keys::ESC, if filtering { "clear" } else { "close" }),
            ],
            chrome::Tone::Normal,
        ),
    };
    let mut inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: &title,
            count: &count,
            hints: &hints,
            tone,
            focused: true,
        },
        app,
    );
    if app.library.mode == Mode::Browse {
        inner = chrome::render_field(
            f,
            inner,
            "⌕",
            &app.library.filter,
            "Type to filter",
            false,
            &app.theme,
        );
    } else if app.library.mode == Mode::ConfirmDelete {
        let name = selected(app).map_or_else(String::new, |entry| entry.relative.clone());
        let detail = format!("“{name}” will be moved to the trash folder.");
        let (list_area, detail_area) = chrome::split_with_detail(inner, &detail);
        chrome::render_detail(f, detail_area, &detail, &app.theme);
        inner = list_area;
    }
    let list = chrome::standard_list(rows(app, content_w), &app.theme);
    let mut state = ListState::default();
    if total > 0 {
        state.select(Some(app.library.selected.min(total - 1)));
    }
    chrome::render_list(f, list, &mut state, inner, total, 3, app);
    if total == 0 {
        let message = if filtering {
            format!(
                "No document matches “{}”. Esc clears the filter.",
                app.library.filter
            )
        } else {
            "No decks or docs yet. Ask the model to draft one.".to_string()
        };
        chrome::render_empty(f, inner, &message, &app.theme);
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) {
    match app.library.mode {
        Mode::ConfirmDelete => confirm_key(app, key),
        Mode::ExportAs => export_key(app, key),
        Mode::Browse => browse_key(app, key),
    }
}

fn browse_key(app: &mut AppView, key: KeyEvent) {
    let total = filtered_entries(app).count();
    let page = 10;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            if app.library.filter.is_empty() {
                app.popup = Popup::None;
            } else {
                app.library.filter.clear();
                app.library.selected = 0;
            }
        }
        KeyCode::Up => {
            app.library.selected = clamp_cursor(app.library.selected, total, -1);
        }
        KeyCode::Down => {
            app.library.selected = clamp_cursor(app.library.selected, total, 1);
        }
        KeyCode::PageUp => app.library.selected = clamp_cursor(app.library.selected, total, -page),
        KeyCode::PageDown => app.library.selected = clamp_cursor(app.library.selected, total, page),
        KeyCode::Home => app.library.selected = 0,
        KeyCode::End => app.library.selected = total.saturating_sub(1),
        KeyCode::Enter => open_selected(app),
        KeyCode::Char('e') if ctrl && selected(app).is_some() => app.library.mode = Mode::ExportAs,
        KeyCode::Char('d') if ctrl && selected(app).is_some() => {
            app.library.mode = Mode::ConfirmDelete;
        }
        KeyCode::Backspace => {
            app.library.filter.pop();
            app.library.selected = 0;
        }
        KeyCode::Char(c) if !super::is_chord(key) => {
            app.library.filter.push(c);
            app.library.selected = 0;
        }
        _ => {}
    }
}

/// Enter: open the file in studio, linking the window to this conversation.
fn open_selected(app: &mut AppView) {
    let Some(path) = selected(app).map(|entry| entry.path.display().to_string()) else {
        return;
    };
    app.popup = Popup::None;
    if let Err(error) = app.open_studio(Some(&path)) {
        app.push_status(format!("{error:#}"));
    }
}

fn confirm_key(app: &mut AppView, key: KeyEvent) {
    match super::classify_confirm_delete_key(key) {
        Some(super::ConfirmDeleteAction::Yes) => delete_selected(app),
        Some(super::ConfirmDeleteAction::No) => app.library.mode = Mode::Browse,
        _ => {}
    }
}

fn delete_selected(app: &mut AppView) {
    app.library.mode = Mode::Browse;
    let Some((relative, title)) = selected(app).map(|e| (e.relative.clone(), e.title.clone()))
    else {
        return;
    };
    match files::delete(&files_dir(app), &relative) {
        Ok(_) => app.push_status(format!("moved \"{title}\" to the trash folder (.trash)")),
        Err(error) => app.push_status(format!("delete failed: {error:#}")),
    }
    refresh(app);
}

fn export_key(app: &mut AppView, key: KeyEvent) {
    let Some(kind) = selected(app).map(|e| e.kind) else {
        app.library.mode = Mode::Browse;
        return;
    };
    match key.code {
        KeyCode::Esc => app.library.mode = Mode::Browse,
        KeyCode::Char(c) => {
            if let Some((_, word, _)) = formats(kind).iter().find(|(key, _, _)| *key == c) {
                app.library.mode = Mode::Browse;
                export_selected(app, word);
            }
        }
        _ => {}
    }
}

/// Export the selected file in `word` format: through the Save dialog when a
/// desktop session is there, otherwise into the space's `exports/` folder.
fn export_selected(app: &mut AppView, word: &str) {
    let Some(entry) = selected(app).cloned() else {
        return;
    };
    let files = files_dir(app);
    let result = (|| -> Result<Option<(PathBuf, bool)>> {
        let extension = files::export_extension(entry.kind, word)?;
        let dialog = app.dialogs.available();
        let chosen = if dialog {
            let start = settings::export_dir(&app.space.root, &files);
            let name = files::export_file_name(&entry.title, extension);
            let filters = dialogs::save_filter(extension.rsplit('.').next().unwrap_or(extension));
            let Some(chosen) = app.dialogs.save_file(
                &format!("Export {}", entry.title),
                start.as_deref(),
                &name,
                &filters,
            ) else {
                return Ok(None);
            };
            Some(files::with_extension(chosen, extension))
        } else {
            None
        };
        // Markdown's images go beside the file, or into exports/assets.
        let images = chosen
            .as_deref()
            .and_then(std::path::Path::parent)
            .map_or_else(|| files.join("exports/assets"), |dir| dir.join("assets"));
        let output = files::export_file(&files, &entry.relative, word, None, &images)?;
        let written = match &chosen {
            Some(path) => {
                let written = files::write_chosen(&output.bytes, path)?;
                let _ = settings::remember_export(&app.space.root, &files, &written);
                written
            }
            None => dugix_core::deck::export::write_export(&output, &files, None)?,
        };
        Ok(Some((written, dialog)))
    })();
    match result {
        Ok(Some((path, true))) => app.push_status(format!("exported to {}", path.display())),
        Ok(Some((path, false))) => app.push_status(format!(
            "no desktop session for a Save dialog — exported to {}",
            path.display()
        )),
        Ok(None) => app.push_status("export cancelled"),
        Err(error) => app.push_status(format!("export failed: {error:#}")),
    }
}

/// The dialogs the TUI asks: the operating system's, except in tests.
#[must_use]
pub fn default_dialogs() -> Box<dyn Dialogs> {
    #[cfg(test)]
    {
        Box::new(dialogs::Scripted::new([]))
    }
    #[cfg(not(test))]
    {
        Box::new(dialogs::Native)
    }
}
