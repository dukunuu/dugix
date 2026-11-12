//! The library and its native dialogs; the page never names a path of its own:
//! a library file is addressed by its path inside the space files.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nexus_core::canvas::dialogs::{self, Dialogs};
use nexus_core::canvas::{self, Entry, files, settings};
use nexus_core::deck::link::Kind as FileKind;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{Backend, Command, Deck, Event, Result, absolute, bail, doc, ensure, read_deck_noted};

#[derive(Debug, Serialize)]
pub struct Item {
    pub kind: FileKind,
    /// `decks/q3.deck.json`: the file's path inside the space.
    pub relative: String,
    pub title: String,
    /// RFC 3339, rendered as "3 hours ago" by the page.
    pub modified: String,
    pub count: usize,
    pub design: Option<String>,
    /// `/library/<key>.png`; `None` when there is nothing to draw yet.
    pub thumb: Option<String>,
    pub current: bool,
}

/// The URL name of an entry's thumbnail: stable for a file and revision.
fn thumb_key(entry: &Entry) -> String {
    let digest = Sha256::digest(entry.relative.as_bytes());
    let hex = digest.iter().take(6).fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!("{hex}-{}.png", &entry.revision[..8])
}

fn deck_format(word: &str) -> Result<nexus_core::deck::export::Format> {
    word.parse()
}

impl Backend {
    fn current_relative(&self) -> Option<String> {
        let root = self.files_dir.canonicalize().ok()?;
        let path = self.path.canonicalize().ok()?;
        path.strip_prefix(root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    /// List the space and remember the listing for thumbnail requests.
    pub(super) fn library_event(&self, notice: Option<String>, selected: Option<String>) -> Event {
        let entries = canvas::library(&self.files_dir);
        let current = self.current_relative();
        let items = entries
            .iter()
            .map(|entry| Item {
                kind: entry.kind,
                relative: entry.relative.clone(),
                title: entry.title.clone(),
                modified: entry.modified.to_rfc3339(),
                count: entry.count,
                design: entry.design.clone(),
                thumb: entry
                    .thumbnail
                    .as_ref()
                    .map(|_| format!("/library/{}", thumb_key(entry))),
                current: current.as_deref() == Some(entry.relative.as_str()),
            })
            .collect();
        *self.listing.borrow_mut() = entries;
        Event::Library {
            items,
            current,
            home: self.home.get(),
            selected,
            notice,
        }
    }

    fn thumbnail_entry(&self, name: &str) -> Result<Entry> {
        self.listing
            .borrow()
            .iter()
            .find(|entry| thumb_key(entry) == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown thumbnail"))
    }

    /// `/library/<key>.png`, drawn on first request.
    pub(super) fn serve_thumbnail(&self, name: &str) -> Result<(&'static str, Vec<u8>)> {
        let entry = self.thumbnail_entry(name)?;
        let path = canvas::ensure_thumbnail(&self.files_dir, &entry)?;
        Ok(("image/png", std::fs::read(path)?))
    }

    /// What drawing a thumbnail takes, so the caller can do it off the backend's lock.
    pub fn thumbnail_job(&self, uri: &str) -> Result<(PathBuf, Entry)> {
        let path = super::protocol_path(uri)?;
        let name = path
            .strip_prefix("/library/")
            .ok_or_else(|| anyhow::anyhow!("unknown resource"))?;
        Ok((self.files_dir.clone(), self.thumbnail_entry(name)?))
    }

    /// The window's own library commands. Returns `None` for any other.
    pub(super) fn library_command(&mut self, command: &Command) -> Result<Option<Event>> {
        Ok(Some(match command {
            Command::Library {} => self.library_event(None, None),
            Command::LibraryOpen { path } => self.library_open(path)?,
            Command::LibraryReveal { path } => {
                let (_, file) = files::resolve(&self.files_dir, path)?;
                Self::show_in_folder(&file)?
            }
            Command::LibraryRename { path, title } => self.library_rename(path, title)?,
            Command::LibraryDuplicate { path } => {
                let copy = files::duplicate(&self.files_dir, path)?;
                self.library_event(Some("Duplicated".into()), Some(copy))
            }
            Command::LibraryDelete { path } => self.library_delete(path)?,
            // The window shows these off the backend's lock (`prepare_dialog`);
            // everything else, tests included, asks and finishes in place.
            _ => match self.dialog_job(command)? {
                Some(job) => {
                    let answer = job.ask();
                    self.finish(answer)?
                }
                None => return Ok(None),
            },
        }))
    }

    /// The file already open stays as it is, with its undo history.
    fn library_open(&mut self, relative: &str) -> Result<Event> {
        let (_, file) = files::resolve(&self.files_dir, relative)?;
        if nexus_core::deck::same_file(&file, &self.path) && !self.home.get() {
            return Ok(self.event());
        }
        self.execute(Command::Open { path: file })
    }

    fn library_rename(&mut self, relative: &str, title: &str) -> Result<Event> {
        let (_, file) = files::resolve(&self.files_dir, relative)?;
        if !nexus_core::deck::same_file(&file, &self.path) {
            files::rename(&self.files_dir, relative, title)?;
            return Ok(self.library_event(None, Some(relative.into())));
        }
        // The open file changes through the window's own undo history.
        let title = title.trim();
        ensure!(!title.is_empty(), "the title is empty");
        if self.sheet.is_some() {
            let ops = vec![serde_json::json!({"op": "set_title", "title": title})];
            self.execute(Command::SheetApply { ops })
        } else if self.doc.is_some() {
            let ops = vec![serde_json::json!({"op": "set_title", "title": title})];
            self.execute(Command::DocApply { ops })
        } else {
            let mut deck = self.deck.clone();
            deck.title = title.into();
            self.execute(Command::Save { deck })
        }
    }

    fn library_delete(&mut self, relative: &str) -> Result<Event> {
        let (_, file) = files::resolve(&self.files_dir, relative)?;
        let was_open = nexus_core::deck::same_file(&file, &self.path);
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        files::delete(&self.files_dir, relative)?;
        if was_open {
            self.reset_to_home()?;
        }
        let event = self.library_event(
            Some(format!("Moved {name} to the trash folder (.trash)")),
            None,
        );
        Ok(event)
    }

    /// The open file was deleted: back to an empty, not-yet-saved deck behind the library.
    fn reset_to_home(&mut self) -> Result<()> {
        self.doc = None;
        self.sheet = None;
        self.path = super::new_path(&self.files_dir)?;
        self.deck = Deck::new_from_layouts("Untitled");
        self.fetcher = nexus_core::remote_image::Fetcher::new(super::asset_root(&self.path)?);
        self.known_revision = self.deck.revision();
        self.undo.clear();
        self.redo.clear();
        self.feedback.clear();
        self.deck_stamp = None;
        self.comments_stamp = None;
        self.home.set(true);
        Ok(())
    }

    // -- dialogs ------------------------------------------------------------
    //
    // A dialog is two steps with the dialog in between, so the window never holds
    // the backend (or the event loop's callback) while one is open: `dialog_job`
    // works out what to ask, `DialogJob::ask` shows it on a helper thread (macOS
    // needs the panel on the main thread), and `finish_dialog` finishes.

    fn dialog_start(&self) -> Option<PathBuf> {
        settings::export_dir(&self.data_dir, &self.files_dir)
            .or_else(|| Some(self.files_dir.join("exports")).filter(|p| p.is_dir()))
    }

    fn remember_export(&self, exported: &Path) {
        // The next dialog starting somewhere else is no reason to fail an export.
        let _ = settings::remember_export(&self.data_dir, &self.files_dir, exported);
    }

    fn current_file(&self) -> (FileKind, PathBuf) {
        let kind = if self.sheet.is_some() {
            FileKind::Sheet
        } else if self.doc.is_some() {
            FileKind::Doc
        } else {
            FileKind::Deck
        };
        (kind, self.path.clone())
    }

    pub fn dialog_job(&mut self, command: &Command) -> Result<Option<DialogJob>> {
        let start_in_space = self.files_dir.canonicalize().ok();
        let (ask, then) = match command {
            Command::OpenDialog {} => (
                Ask::Open {
                    title: "Open".into(),
                    start: start_in_space,
                    filters: dialogs::OPEN,
                },
                Then::Open,
            ),
            Command::PickTemplate {} => (
                Ask::Open {
                    title: "Add template".into(),
                    start: start_in_space,
                    filters: dialogs::TEMPLATES,
                },
                Then::Template,
            ),
            Command::PickImage {} => (
                Ask::Open {
                    title: "Choose an image".into(),
                    start: start_in_space,
                    filters: dialogs::IMAGES,
                },
                Then::Image,
            ),
            Command::ExportAs { format, template } => {
                self.export_job(self.current_file(), format, template.clone())?
            }
            Command::LibraryExport { path, format } => {
                let (kind, file) = files::resolve(&self.files_dir, path)?;
                self.export_job((kind, file), format, None)?
            }
            Command::SaveCopy {} => self.save_copy_job()?,
            _ => return Ok(None),
        };
        Ok(Some(DialogJob {
            dialogs: Arc::clone(&self.dialogs),
            ask,
            then,
        }))
    }

    /// A dialog job for a raw IPC message, if it is one; an error is the reply.
    pub fn prepare_dialog(&mut self, source: &str) -> Option<Result<DialogJob>> {
        let command: Command = serde_json::from_str(source).ok()?;
        self.dialog_job(&command).transpose()
    }

    pub fn finish_dialog(&mut self, answer: Answer) -> Event {
        self.finish(answer).unwrap_or_else(|error| Event::Error {
            message: error.to_string(),
        })
    }

    fn finish(&mut self, answer: Answer) -> Result<Event> {
        let Some(chosen) = answer.chosen else {
            return Ok(Event::Idle {});
        };
        match answer.then {
            Then::Open => self.open_chosen(&chosen),
            Then::Template => self.import_template(&absolute(&chosen)?),
            Then::Image => self.import_image(&absolute(&chosen)?),
            Then::Export {
                file,
                word,
                template,
                extension,
            } => self.export_to(&file, &word, template.as_deref(), extension, chosen),
            Then::SaveCopy => self.save_copy_to(&chosen),
        }
    }

    fn import_image(&self, chosen: &Path) -> Result<Event> {
        let src = if self.doc.is_some() {
            let bytes = super::read_bounded(chosen, 8 * 1024 * 1024)?;
            self.doc_store_image(&bytes, &chosen.to_string_lossy())?
        } else {
            self.import_asset(chosen)?
        };
        Ok(Event::Asset { src })
    }

    /// nexus files open as they are; `PowerPoint`, Word and Markdown files import.
    fn open_chosen(&mut self, chosen: &Path) -> Result<Event> {
        let chosen = absolute(chosen)?;
        let name = chosen
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let extension = chosen
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        if name.ends_with(".deck.json")
            || name.ends_with(".sheet.json")
            || name.ends_with(".doc.json")
            || extension.as_deref() == Some("pptx")
        {
            let event = self.execute(Command::Open { path: chosen })?;
            // The approximations of an import ride on this one event.
            self.import_warnings.clear();
            return Ok(event);
        }
        if matches!(extension.as_deref(), Some("docx" | "dotx" | "md")) {
            return self.import_document(&chosen);
        }
        if matches!(extension.as_deref(), Some("csv" | "xlsx")) {
            return self.import_sheet(&chosen);
        }
        bail!("choose a .deck.json, .doc.json, .sheet.json, .pptx, .docx, .md, .csv or .xlsx file")
    }

    fn import_sheet(&mut self, source: &Path) -> Result<Event> {
        let title = source
            .file_stem()
            .map_or_else(|| "Imported".into(), |s| s.to_string_lossy().into_owned());
        let base = self.blank_sheet(&title)?;
        let bytes = super::read_bounded(source, nexus_core::sheet::validate::MAX_SOURCE)?;
        let mut sheet = if source
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("csv"))
        {
            let slug = base.design.as_deref().unwrap_or("nexus");
            let design = nexus_core::design::load(&self.files_dir, slug)?;
            nexus_core::sheet::interchange::import_csv(&bytes, &source.to_string_lossy(), &design)?
        } else {
            nexus_core::sheet::interchange::import_xlsx(&bytes, &title)?
        };
        sheet.title.clone_from(&title);
        sheet.theme = base.theme;
        sheet.design = base.design;
        sheet.design_rev = base.design_rev;
        let path = self.new_sheet_path(&title)?;
        super::atomic_write(&path, &serde_json::to_vec_pretty(&sheet)?)?;
        self.enter_sheet(path, sheet)?;
        *self.doc_notice.borrow_mut() = Some(format!(
            "Imported {}",
            source.file_name().unwrap_or_default().to_string_lossy()
        ));
        Ok(self.sheet_event())
    }

    fn import_document(&mut self, source: &Path) -> Result<Event> {
        let docs = doc::docs_dir(&self.files_dir)?;
        let imported = nexus_core::doc::import::file(source, &docs.join("assets"))?;
        let stem = {
            let stem = nexus_core::canvas::files::file_stem_for(&imported.doc.title);
            let slug: String = stem
                .to_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            if slug.is_empty() {
                "imported".into()
            } else {
                slug
            }
        };
        let mut target = docs.join(format!("{stem}.doc.json"));
        for index in 2.. {
            match nexus_core::doc::import::write_new(&imported, &target) {
                Ok(()) => break,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
                {
                    target = docs.join(format!("{stem}-{index}.doc.json"));
                }
                Err(error) => return Err(error),
            }
        }
        let file = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let notice = if imported.warnings.is_empty() {
            format!("Imported {file}")
        } else {
            format!(
                "Imported {file} with {} approximation{}: {}",
                imported.warnings.len(),
                if imported.warnings.len() == 1 {
                    ""
                } else {
                    "s"
                },
                imported
                    .warnings
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        };
        // The doc event the open builds carries the notice.
        *self.doc_notice.borrow_mut() = Some(notice);
        self.execute(Command::Open { path: target })
    }

    /// What to ask for an export of `source` in `word` format.
    fn export_job(
        &self,
        source: (FileKind, PathBuf),
        word: &str,
        template: Option<String>,
    ) -> Result<(Ask, Then)> {
        let (kind, file) = source;
        let open = nexus_core::deck::same_file(&file, &self.path);
        let extension = files::export_extension(kind, word)?;
        // The open file's title is read from memory (a not-yet-saved placeholder
        // only exists there); another file's from disk.
        let title = match kind {
            FileKind::Deck if open => self.deck.title.clone(),
            FileKind::Deck => read_deck_noted(&file)?.0.title,
            FileKind::Sheet => match self.sheet.as_ref().filter(|_| open) {
                Some(session) => session.sheet.title.clone(),
                None => files::read_sheet(&file)?.title,
            },
            FileKind::Doc => match self.doc.as_ref().filter(|_| open) {
                Some(session) => session.doc.title.clone(),
                None => doc::read_doc(&file)?.title,
            },
        };
        Ok((
            Ask::Save {
                name: files::export_file_name(&title, extension),
                title: format!("Export {title}"),
                start: self.dialog_start(),
                filters: dialogs::save_filter(extension.rsplit('.').next().unwrap_or(extension)),
            },
            Then::Export {
                file: (kind, file),
                word: word.into(),
                template,
                extension,
            },
        ))
    }

    fn export_to(
        &mut self,
        source: &(FileKind, PathBuf),
        word: &str,
        template: Option<&str>,
        extension: &str,
        chosen: PathBuf,
    ) -> Result<Event> {
        let (kind, file) = source;
        let open = nexus_core::deck::same_file(file, &self.path);
        let chosen = files::with_extension(chosen, extension);
        let output = match kind {
            FileKind::Sheet => {
                let sheet = match self.sheet.as_ref().filter(|_| open) {
                    Some(session) => session.sheet.clone(),
                    None => files::read_sheet(file)?,
                };
                files::sheet_output(&sheet, word, None)?
            }
            FileKind::Deck => {
                let deck = if open {
                    self.deck.clone()
                } else {
                    read_deck_noted(file)?.0
                };
                self.deck_output(&deck, file, deck_format(word)?, template)?
            }
            FileKind::Doc => {
                let (doc, threads) = match self.doc.as_ref().filter(|_| open) {
                    Some(session) => (session.doc.clone(), session.threads.clone()),
                    None => (doc::read_doc(file)?, nexus_core::doc::feedback::load(file)?),
                };
                let images = chosen
                    .parent()
                    .map_or_else(|| PathBuf::from("assets"), |p| p.join("assets"));
                self.doc_output(&doc, &threads, file, word.parse()?, template, &images)?
            }
        };
        let written = files::write_chosen(&output.bytes, &chosen)?;
        self.remember_export(&written);
        self.exports.push(written.clone());
        Ok(Event::Exported {
            path: written,
            warnings: output.warnings,
        })
    }

    fn save_copy_job(&mut self) -> Result<(Ask, Then)> {
        // A deck that was never written exists only in memory.
        if !self.path.exists() {
            if self.doc.is_some() {
                bail!("save the doc first: make an edit");
            }
            self.save()?;
        }
        let (kind, title) = if let Some(session) = &self.sheet {
            (FileKind::Sheet, session.sheet.title.clone())
        } else {
            match self.doc.as_ref() {
                Some(session) => (FileKind::Doc, session.doc.title.clone()),
                None => (FileKind::Deck, self.deck.title.clone()),
            }
        };
        let suffix = match kind {
            FileKind::Deck => "deck.json",
            FileKind::Doc => "doc.json",
            FileKind::Sheet => "sheet.json",
        };
        Ok((
            Ask::Save {
                title: "Save a copy".into(),
                start: self.dialog_start(),
                name: files::export_file_name(&format!("{title} copy"), suffix),
                filters: dialogs::save_filter("json"),
            },
            Then::SaveCopy,
        ))
    }

    fn save_copy_to(&mut self, chosen: &Path) -> Result<Event> {
        let saved = files::save_copy(&self.path, chosen)?;
        self.remember_export(&saved.path);
        self.exports.push(saved.path.clone());
        Ok(Event::Saved {
            path: saved.path,
            assets: saved.assets,
        })
    }
}

enum Ask {
    Open {
        title: String,
        start: Option<PathBuf>,
        filters: &'static [dialogs::Filter],
    },
    Save {
        title: String,
        start: Option<PathBuf>,
        name: String,
        filters: Vec<dialogs::Filter>,
    },
}

enum Then {
    Open,
    Template,
    Image,
    Export {
        file: (FileKind, PathBuf),
        word: String,
        template: Option<String>,
        extension: &'static str,
    },
    SaveCopy,
}

/// A native dialog ready to be shown, owning everything it needs so it can run on
/// any thread while the backend stays free.
pub struct DialogJob {
    dialogs: Arc<dyn Dialogs>,
    ask: Ask,
    then: Then,
}

impl DialogJob {
    /// Show the dialog and wait; `None` is a cancel.
    #[must_use]
    pub fn ask(self) -> Answer {
        let chosen = match &self.ask {
            Ask::Open {
                title,
                start,
                filters,
            } => self.dialogs.open_file(title, start.as_deref(), filters),
            Ask::Save {
                title,
                start,
                name,
                filters,
            } => self
                .dialogs
                .save_file(title, start.as_deref(), name, filters),
        };
        Answer {
            then: self.then,
            chosen,
        }
    }
}

pub struct Answer {
    then: Then,
    chosen: Option<PathBuf>,
}
