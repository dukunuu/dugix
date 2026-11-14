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

use super::{
    Backend, Command, Deck, Event, LibraryStamp, Result, absolute, bail, doc, ensure,
    read_deck_noted,
};

#[derive(Debug, Serialize)]
pub struct Item {
    pub kind: FileKind,
    /// The space the file belongs to, by name.
    pub space: String,
    /// `decks/q3.deck.json`: the file's path inside its space.
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

/// One library file with the space it was listed from.
#[derive(Clone, Debug)]
pub struct Listed {
    pub space: String,
    pub files: PathBuf,
    pub entry: Entry,
}

/// The space files folder `path` lies in, when it is under `<data>/spaces/<name>/files`.
pub(super) fn space_files_of(data_dir: &Path, path: &Path) -> Option<PathBuf> {
    let spaces = data_dir.join("spaces").canonicalize().ok()?;
    let path = absolute(path).ok()?;
    // A file not written yet is placed by its folder.
    let path = path
        .canonicalize()
        .ok()
        .or_else(|| Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?)))?;
    let name = path.strip_prefix(&spaces).ok()?.components().next()?;
    let files = spaces.join(name).join("files");
    path.starts_with(&files).then_some(files)
}

/// The URL name of an entry's thumbnail: stable for a file and revision.
fn thumb_key(listed: &Listed) -> String {
    let mut hasher = Sha256::new();
    hasher.update(listed.space.as_bytes());
    hasher.update([0]);
    hasher.update(listed.entry.relative.as_bytes());
    let hex = hasher
        .finalize()
        .iter()
        .take(6)
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    format!("{hex}-{}.png", &listed.entry.revision[..8])
}

fn deck_format(word: &str) -> Result<nexus_core::deck::export::Format> {
    word.parse()
}

impl Backend {
    /// The open file's space follows it, so its designs and new files are that space's.
    pub(super) fn follow_space(&mut self) {
        if let Some(files) = space_files_of(&self.data_dir, &self.path) {
            self.files_dir = files;
        }
    }

    fn current_relative(&self) -> Option<String> {
        let root = self.files_dir.canonicalize().ok()?;
        let path = self.path.canonicalize().ok()?;
        path.strip_prefix(root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    /// The name the open file's space goes by in the listing.
    fn current_space(&self) -> String {
        let name = |dir: &Path| {
            dir.parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
        };
        name(&self.files_dir)
            .or_else(|| self.files_dir.canonicalize().ok().as_deref().and_then(name))
            .unwrap_or_else(|| "this space".into())
    }

    /// Every space by name with its files folder: those under `<data>/spaces`,
    /// and the open file's space when it lives elsewhere.
    fn spaces(&self) -> Vec<(String, PathBuf)> {
        let mut spaces: Vec<(String, PathBuf)> = std::fs::read_dir(self.data_dir.join("spaces"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|child| child.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|child| {
                let name = child.file_name().to_string_lossy().into_owned();
                let files = child.path().join("files");
                (!name.starts_with('.') && files.is_dir()).then_some((name, files))
            })
            .collect();
        let current = self.files_dir.canonicalize().ok();
        let listed = spaces
            .iter()
            .any(|(_, files)| files.canonicalize().ok() == current);
        if !listed {
            let mut name = self.current_space();
            while spaces.iter().any(|(other, _)| *other == name) {
                name.push('\'');
            }
            spaces.push((name, self.files_dir.clone()));
        }
        spaces.sort_by_key(|a| a.0.to_lowercase());
        spaces
    }

    /// The files folder of the space a library command names.
    fn space_files(&self, space: Option<&str>) -> Result<PathBuf> {
        let Some(space) = space else {
            return Ok(self.files_dir.clone());
        };
        self.spaces()
            .into_iter()
            .find(|(name, _)| name == space)
            .map(|(_, files)| files)
            .ok_or_else(|| anyhow::anyhow!("there is no space named {space}"))
    }

    /// List every space and remember the listing for thumbnail requests.
    pub(super) fn library_event(
        &self,
        notice: Option<String>,
        selected: Option<(String, String)>,
    ) -> Event {
        let spaces = self.spaces();
        let stamp: LibraryStamp = spaces
            .iter()
            .flat_map(|(_, files)| canvas::library_stamp(files))
            .collect();
        let unchanged = self.listing_stamp.borrow().as_ref() == Some(&stamp);
        let mut listed = if unchanged {
            self.listing.borrow().clone()
        } else {
            spaces
                .iter()
                .flat_map(|(space, files)| {
                    canvas::library(files).into_iter().map(|entry| Listed {
                        space: space.clone(),
                        files: files.clone(),
                        entry,
                    })
                })
                .collect()
        };
        if !unchanged {
            listed.sort_by(|a: &Listed, b: &Listed| {
                b.entry
                    .modified
                    .cmp(&a.entry.modified)
                    .then_with(|| a.space.cmp(&b.space))
                    .then_with(|| a.entry.relative.cmp(&b.entry.relative))
            });
        }
        *self.listing_stamp.borrow_mut() = Some(stamp);
        let current = self.current_relative();
        let current_files = self.files_dir.canonicalize().ok();
        let items = listed
            .iter()
            .map(|listed| {
                let entry = &listed.entry;
                Item {
                    kind: entry.kind,
                    space: listed.space.clone(),
                    relative: entry.relative.clone(),
                    title: entry.title.clone(),
                    modified: entry.modified.to_rfc3339(),
                    count: entry.count,
                    design: entry.design.clone(),
                    thumb: entry
                        .thumbnail
                        .as_ref()
                        .map(|_| format!("/library/{}", thumb_key(listed))),
                    current: current.as_deref() == Some(entry.relative.as_str())
                        && listed.files.canonicalize().ok() == current_files,
                }
            })
            .collect();
        *self.listing.borrow_mut() = listed;
        let space = spaces
            .iter()
            .find(|(_, files)| files.canonicalize().ok() == current_files)
            .map_or_else(|| self.current_space(), |(name, _)| name.clone());
        let (selected_space, selected) = selected.unzip();
        Event::Library {
            items,
            current,
            space,
            spaces: spaces.into_iter().map(|(name, _)| name).collect(),
            home: self.home.get(),
            selected,
            selected_space,
            notice,
        }
    }

    fn thumbnail_entry(&self, name: &str) -> Result<Listed> {
        self.listing
            .borrow()
            .iter()
            .find(|listed| thumb_key(listed) == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown thumbnail"))
    }

    /// `/library/<key>.png`, drawn on first request.
    pub(super) fn serve_thumbnail(&self, name: &str) -> Result<(&'static str, Vec<u8>)> {
        let listed = self.thumbnail_entry(name)?;
        let path = canvas::ensure_thumbnail(&listed.files, &listed.entry)?;
        Ok(("image/png", std::fs::read(path)?))
    }

    /// What drawing a thumbnail takes, so the caller can do it off the backend's lock.
    pub fn thumbnail_job(&self, uri: &str) -> Result<(PathBuf, Entry)> {
        let path = super::protocol_path(uri)?;
        let name = path
            .strip_prefix("/library/")
            .ok_or_else(|| anyhow::anyhow!("unknown resource"))?;
        let listed = self.thumbnail_entry(name)?;
        Ok((listed.files, listed.entry))
    }

    /// The window's own library commands. Returns `None` for any other.
    pub(super) fn library_command(&mut self, command: &Command) -> Result<Option<Event>> {
        Ok(Some(match command {
            Command::Library {} => self.library_event(None, None),
            Command::LibraryOpen { path, space } => self.library_open(space.as_deref(), path)?,
            Command::LibraryReveal { path, space } => {
                let files = self.space_files(space.as_deref())?;
                let (_, file) = files::resolve(&files, path)?;
                Self::show_in_folder(&file)?
            }
            Command::LibraryRename { path, title, space } => {
                self.library_rename(space.as_deref(), path, title)?
            }
            Command::LibraryDuplicate { path, space } => {
                let files = self.space_files(space.as_deref())?;
                let copy = files::duplicate(&files, path)?;
                let space = space.clone().unwrap_or_else(|| self.current_space());
                self.library_event(Some("Duplicated".into()), Some((space, copy)))
            }
            Command::LibraryDelete { path, space } => {
                self.library_delete(space.as_deref(), path)?
            }
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
    fn library_open(&mut self, space: Option<&str>, relative: &str) -> Result<Event> {
        let files = self.space_files(space)?;
        let (_, file) = files::resolve(&files, relative)?;
        if nexus_core::deck::same_file(&file, &self.path) && !self.home.get() {
            return Ok(self.event());
        }
        let event = self.execute(Command::Open { path: file })?;
        // A space outside `<data>/spaces` is not found from the path alone.
        self.files_dir = files;
        Ok(event)
    }

    fn library_rename(
        &mut self,
        space: Option<&str>,
        relative: &str,
        title: &str,
    ) -> Result<Event> {
        let files = self.space_files(space)?;
        let (_, file) = files::resolve(&files, relative)?;
        if !nexus_core::deck::same_file(&file, &self.path) {
            files::rename(&files, relative, title)?;
            let space = space.map_or_else(|| self.current_space(), Into::into);
            return Ok(self.library_event(None, Some((space, relative.into()))));
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

    fn library_delete(&mut self, space: Option<&str>, relative: &str) -> Result<Event> {
        let files = self.space_files(space)?;
        let (_, file) = files::resolve(&files, relative)?;
        let was_open = nexus_core::deck::same_file(&file, &self.path);
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        files::delete(&files, relative)?;
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
            Command::LibraryExport {
                path,
                format,
                space,
            } => {
                let files = self.space_files(space.as_deref())?;
                let (kind, file) = files::resolve(&files, path)?;
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
