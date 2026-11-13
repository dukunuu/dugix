//! Spreadsheet mode: calculated windows, atomic edits, file history and cell threads.
use anyhow::{Context as _, Result, bail, ensure};
use nexus_core::canvas::files;
use nexus_core::deck::{
    history::{self, Who},
    link::Message as LinkMessage,
};
use nexus_core::sheet::{self, Sheet, ValueKind, Workbook, address::Range, feedback as comments};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{Backend, Command, Event, asset_root, atomic_write, stamp};

/// One non-empty computed cell, independent of the calculation engine.
#[derive(Debug, Serialize)]
pub struct CellValue {
    pub a1: String,
    pub display: String,
    pub kind: &'static str,
    pub error: Option<String>,
}

/// A chart's stable identity and freshly calculated SVG.
#[derive(Debug, Serialize)]
pub struct ChartSvg {
    pub id: String,
    pub svg: String,
}

fn charts(sheet: &Sheet, workbook: &Workbook) -> BTreeMap<String, Vec<ChartSvg>> {
    sheet
        .tabs
        .iter()
        .map(|tab| {
            (
                tab.id.clone(),
                tab.charts
                    .iter()
                    .map(|chart| ChartSvg {
                        id: chart.id.clone(),
                        svg: sheet::chart_svg::svg(sheet, workbook, tab, chart),
                    })
                    .collect(),
            )
        })
        .collect()
}

/// The open workbook and whole-sheet undo steps.
pub struct SheetSession {
    pub sheet: Sheet,
    /// `sheet.revision()`, cached; `replace` is the only way to change `sheet`.
    pub revision: String,
    pub workbook: Workbook,
    pub undo: Vec<Sheet>,
    pub redo: Vec<Sheet>,
    pub threads: Vec<comments::Thread>,
    undo_threads: Vec<Vec<comments::Thread>>,
    redo_threads: Vec<Vec<comments::Thread>>,
}

pub(super) fn is_sheet_path(path: &Path) -> bool {
    nexus_core::deck::link::Kind::of_path(path) == nexus_core::deck::link::Kind::Sheet
}

fn tab_name<'a>(sheet: &'a Sheet, tab: &str) -> Result<&'a str> {
    sheet
        .tabs
        .iter()
        .find(|t| t.id == tab || t.name.eq_ignore_ascii_case(tab))
        .map(|t| t.name.as_str())
        .context("unknown tab")
}

/// The height each row needs for its wrapped cells, for every tab.
fn auto_rows(sheet: &Sheet, workbook: &Workbook) -> BTreeMap<String, BTreeMap<u32, f64>> {
    sheet
        .tabs
        .iter()
        .map(|tab| {
            (
                tab.id.clone(),
                sheet::layout::auto_row_heights(sheet, workbook, tab),
            )
        })
        .collect()
}

fn values(workbook: &Workbook, tab: &str, range: Range) -> Vec<CellValue> {
    let rect = range.bounds();
    let mut values = Vec::new();
    for row in rect.start.row..=rect.end.row {
        for col in rect.start.col..=rect.end.col {
            let a1 = sheet::address::Address { row, col }.to_string();
            let value = workbook.value(tab, &a1);
            let (kind, error) = match value.kind {
                ValueKind::Empty => continue,
                ValueKind::Number(_) => ("number", None),
                ValueKind::Text(_) => ("text", None),
                ValueKind::Bool(_) => ("bool", None),
                ValueKind::Error(error) => ("error", Some(error)),
            };
            values.push(CellValue {
                a1,
                display: value.display,
                kind,
                error,
            });
        }
    }
    values
}

impl SheetSession {
    fn replace(&mut self, next: Sheet, revision: String) -> Sheet {
        debug_assert_eq!(revision, next.revision());
        self.revision = revision;
        std::mem::replace(&mut self.sheet, next)
    }
}

impl Backend {
    fn sheet_session(&self) -> Result<&SheetSession> {
        self.sheet.as_ref().context("open a sheet first")
    }
    fn sheet_session_mut(&mut self) -> Result<&mut SheetSession> {
        self.sheet.as_mut().context("open a sheet first")
    }

    pub(super) fn sheet_event(&self) -> Event {
        let s = self.sheet.as_ref().expect("a sheet is open");
        let relative = self
            .files_dir
            .canonicalize()
            .ok()
            .and_then(|root| self.path.strip_prefix(root).ok().map(Path::to_path_buf))
            .unwrap_or_else(|| self.path.file_name().map(PathBuf::from).unwrap_or_default())
            .to_string_lossy()
            .replace('\\', "/");
        Event::Sheet {
            sheet: s.sheet.clone(),
            revision: s.revision.clone(),
            relative,
            auto_rows: auto_rows(&s.sheet, &s.workbook),
            charts: charts(&s.sheet, &s.workbook),
            values: values(
                &s.workbook,
                &s.sheet.tabs[0].name,
                "A1:AZ200".parse().expect("initial window"),
            ),
            threads: s.threads.clone(),
            linked: self.linked,
            activities: self.thread_activities(),
            changed: vec![],
            notice: self.doc_notice.borrow_mut().take(),
            colors: self.colors.clone(),
            can_undo: !s.undo.is_empty(),
            can_redo: !s.redo.is_empty(),
            home: self.home.get(),
        }
    }

    pub(super) fn sheet_feedback_event(&self, notes: Vec<String>) -> Event {
        Event::SheetFeedback {
            threads: self
                .sheet
                .as_ref()
                .map(|s| s.threads.clone())
                .unwrap_or_default(),
            activities: self.thread_activities(),
            notes,
        }
    }

    pub(super) fn open_sheet_file(&mut self, path: PathBuf) -> Result<()> {
        let sheet = if path.exists() {
            files::read_sheet(&path)?
        } else {
            let sheet = self.blank_sheet("Untitled")?;
            atomic_write(&path, &serde_json::to_vec_pretty(&sheet)?)?;
            sheet
        };
        self.enter_sheet(path, sheet)
    }

    pub(super) fn enter_sheet(&mut self, path: PathBuf, sheet: Sheet) -> Result<()> {
        let workbook = Workbook::load(&sheet)?;
        let mut threads = comments::load(&path)?;
        comments::reconcile(&mut threads, &sheet);
        let root = asset_root(&path)?;
        atomic_write(&self.recent, &serde_json::to_vec(&path)?)?;
        self.home.set(false);
        self.path = path;
        self.doc = None;
        let revision = sheet.revision();
        self.known_revision.clone_from(&revision);
        self.sheet = Some(SheetSession {
            sheet,
            revision,
            workbook,
            undo: vec![],
            redo: vec![],
            threads,
            undo_threads: vec![],
            redo_threads: vec![],
        });
        self.fetcher = nexus_core::remote_image::Fetcher::new(root);
        self.deck_stamp = stamp(&self.path);
        self.comments_stamp = stamp(&comments::sidecar(&self.path));
        self.load_notes.clear();
        self.import_warnings.clear();
        self.hello();
        Ok(())
    }

    pub(super) fn blank_sheet(&self, title: &str) -> Result<Sheet> {
        let slug = nexus_core::design::default_slug(&self.files_dir)?;
        let design = nexus_core::design::load(&self.files_dir, &slug)?;
        let mut sheet = Sheet::from_design(title, &design);
        sheet.design = Some(slug);
        Sheet::check(&serde_json::to_string(&sheet)?).map_err(|e| anyhow::anyhow!("{e:?}"))
    }

    pub(super) fn new_sheet_path(&self, title: &str) -> Result<PathBuf> {
        let slug = files::file_stem_for(title)
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let slug = if slug.is_empty() { "untitled" } else { &slug };
        for index in 1.. {
            let name = if index == 1 {
                slug.to_string()
            } else {
                format!("{slug}-{index}")
            };
            let path =
                sheet::storage::confined(&self.files_dir, &format!("sheets/{name}.sheet.json"))?;
            if !path.exists() && !path.is_symlink() {
                return Ok(path);
            }
        }
        unreachable!("unbounded file names")
    }

    pub(super) fn new_sheet(&mut self, title: &str) -> Result<()> {
        let sheet = self.blank_sheet(title)?;
        let path = self.new_sheet_path(title)?;
        atomic_write(&path, &serde_json::to_vec_pretty(&sheet)?)?;
        self.enter_sheet(path, sheet)
    }

    fn write_sheet(&self, sheet: &Sheet, revision: &str) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(sheet)?;
        ensure!(
            bytes.len() <= sheet::validate::MAX_SOURCE,
            "sheet exceeds 32 MiB"
        );
        // History is a safety net: failing to keep a version must not block a save.
        let _ = match &self.sheet {
            // The file is still the one this window wrote.
            Some(s) if stamp(&self.path) == self.deck_stamp => {
                history::record_known_for(&self.path, &s.sheet, &s.revision, revision, Who::User)
            }
            _ => history::record_before_write_for::<Sheet>(&self.path, revision, Who::User),
        };
        atomic_write(&self.path, &bytes)?;
        self.home.set(false);
        Ok(())
    }

    fn replace_sheet(&mut self, next: Sheet, ops: &[sheet::Op]) -> Result<()> {
        let s = self.sheet_session()?;
        if s.sheet == next {
            return Ok(());
        }
        let workbook = Workbook::load(&next)?;
        let mut threads = s.threads.clone();
        comments::reanchor(&mut threads, &s.sheet, ops);
        comments::reconcile(&mut threads, &next);
        let revision = next.revision();
        self.write_sheet(&next, &revision)?;
        self.deck_stamp = stamp(&self.path);
        let s = self.sheet_session_mut()?;
        let previous = s.replace(next, revision);
        s.undo.push(previous);
        s.redo.clear();
        s.redo_threads.clear();
        s.undo_threads
            .push(std::mem::replace(&mut s.threads, threads));
        s.workbook = workbook;
        Ok(())
    }

    fn sheet_apply(&mut self, raw: Vec<serde_json::Value>) -> Result<()> {
        let ops: Vec<sheet::Op> = serde_json::from_value(serde_json::Value::Array(raw))?;
        let resolver = self.doc_resolver();
        let next = sheet::apply_with_resolver(&self.sheet_session()?.sheet, &ops, Some(&resolver))?;
        self.replace_sheet(next, &ops)
    }

    fn sheet_undo(&mut self, redo: bool) -> Result<()> {
        let s = self.sheet_session()?;
        let stack = if redo { &s.redo } else { &s.undo };
        let Some(next) = stack.last().cloned() else {
            return Ok(());
        };
        let workbook = Workbook::load(&next)?;
        let revision = next.revision();
        self.write_sheet(&next, &revision)?;
        self.deck_stamp = stamp(&self.path);
        let s = self.sheet_session_mut()?;
        let previous = s.replace(next, revision);
        if redo {
            s.redo.pop();
            s.undo.push(previous);
            let threads = s.redo_threads.pop().expect("redo threads");
            s.undo_threads
                .push(std::mem::replace(&mut s.threads, threads));
        } else {
            s.undo.pop();
            s.redo.push(previous);
            let threads = s.undo_threads.pop().expect("undo threads");
            s.redo_threads
                .push(std::mem::replace(&mut s.threads, threads));
        }
        s.workbook = workbook;
        comments::reconcile(&mut s.threads, &s.sheet);
        Ok(())
    }

    fn persist_sheet_feedback(&mut self) -> Result<()> {
        let threads = &self.sheet_session()?.threads;
        if !threads.is_empty() || comments::sidecar(&self.path).exists() {
            comments::save(&self.path, threads)?;
        }
        self.comments_stamp = stamp(&comments::sidecar(&self.path));
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn sheet_execute(&mut self, command: Command) -> Result<Event> {
        match command {
            Command::Load {} => {}
            Command::SheetApply { ops } => self.sheet_apply(ops)?,
            Command::SheetValues { tab, range } => {
                let s = self.sheet_session()?;
                let name = tab_name(&s.sheet, &tab)?;
                let parsed: Range = range.parse()?;
                ensure!(
                    parsed.bounds().cell_count() <= 10_000,
                    "SheetValues is limited to 10,000 cells"
                );
                return Ok(Event::SheetValues {
                    values: values(&s.workbook, name, parsed),
                    tab,
                    range,
                });
            }
            Command::SheetFocus { tab, range } => {
                let tab = tab_name(&self.sheet_session()?.sheet, &tab)?.to_string();
                let range = range.parse::<Range>()?.to_string();
                if self.linked {
                    self.outgoing.push(LinkMessage::SheetFocus {
                        path: self.path.clone(),
                        tab,
                        range,
                    });
                }
                return Ok(self.sheet_feedback_event(vec![]));
            }
            Command::SheetCommentCreate {
                tab,
                range,
                text,
                send,
            } => return self.sheet_comment_create(&tab, &range, text, send),
            Command::CommentReply { thread, text, send } => {
                return self.sheet_comment_reply(&thread, text, send);
            }
            Command::CommentResolve { thread } => return self.sheet_comment_resolve(&thread, true),
            Command::CommentReopen { thread } => return self.sheet_comment_resolve(&thread, false),
            Command::PreviewEdit { owner, edit } => {
                let edit = self.sheet_edit(&owner, &edit)?;
                ensure!(
                    edit.status == comments::EditStatus::Pending,
                    "Outdated — ask again in nexus"
                );
                let resolver = self.doc_resolver();
                let next = sheet::apply_with_resolver(
                    &self.sheet_session()?.sheet,
                    &edit.ops,
                    Some(&resolver),
                )?;
                let workbook = Workbook::load(&next)?;
                let mut event = self.sheet_event();
                if let Event::Sheet {
                    sheet,
                    revision,
                    values: window,
                    auto_rows: rows,
                    charts: rendered,
                    ..
                } = &mut event
                {
                    *revision = next.revision();
                    *window = values(&workbook, &next.tabs[0].name, "A1:AZ200".parse()?);
                    *rows = auto_rows(&next, &workbook);
                    *rendered = charts(&next, &workbook);
                    *sheet = next;
                }
                return Ok(event);
            }
            Command::ApplyEdit { owner, edit } => {
                let proposal = self.sheet_edit(&owner, &edit)?.clone();
                ensure!(
                    proposal.status == comments::EditStatus::Pending,
                    "Outdated — ask again in nexus"
                );
                let resolver = self.doc_resolver();
                let next = sheet::apply_with_resolver(
                    &self.sheet_session()?.sheet,
                    &proposal.ops,
                    Some(&resolver),
                )?;
                self.replace_sheet(next, &proposal.ops)?;
                self.sheet_edit_mut(&owner, &edit)?.status = comments::EditStatus::Applied;
            }
            Command::DismissEdit { owner, edit } => {
                self.sheet_edit_mut(&owner, &edit)?.status = comments::EditStatus::Dismissed;
                self.persist_sheet_feedback()?;
                return Ok(self.sheet_feedback_event(vec![]));
            }
            Command::Undo {} => self.sheet_undo(false)?,
            Command::Redo {} => self.sheet_undo(true)?,
            Command::HistoryRestore { id } => {
                let next = self.history_sheet(&id)?;
                self.replace_sheet(next, &[])?;
            }
            Command::SheetExport { format, path, tab } => {
                let mut output =
                    files::sheet_output(&self.sheet_session()?.sheet, &format, tab.as_deref())?;
                if let Some(notice) =
                    sheet::chart::export_notice(&self.sheet_session()?.sheet, &format)
                {
                    output.warnings.push(notice);
                }
                let path = nexus_core::deck::export::write_export(
                    &output,
                    &self.files_dir,
                    path.as_deref(),
                )?;
                self.exports.push(path.clone());
                return Ok(Event::Exported {
                    path,
                    warnings: output.warnings,
                });
            }
            Command::Reveal { path } => return self.reveal(&path),
            _ => bail!("that action is for docs or decks; open a doc or deck to use it"),
        }
        self.known_revision = self.sheet_session()?.revision.clone();
        self.deck_stamp = stamp(&self.path);
        self.persist_sheet_feedback()?;
        Ok(self.sheet_event())
    }

    fn sheet_comment_resolve(&mut self, id: &str, resolved: bool) -> Result<Event> {
        self.sheet_session_mut()?
            .threads
            .iter_mut()
            .find(|t| t.id == id)
            .context("Unknown thread")?
            .resolved = resolved.then(comments::now);
        self.persist_sheet_feedback()?;
        Ok(self.sheet_feedback_event(vec![]))
    }

    fn sheet_edit(&self, owner: &str, id: &str) -> Result<&comments::Edit> {
        self.sheet_session()?
            .threads
            .iter()
            .filter(|t| t.id == owner)
            .flat_map(|t| &t.messages)
            .flat_map(|m| &m.edits)
            .find(|e| e.id == id)
            .context("Unknown edit")
    }
    fn sheet_edit_mut(&mut self, owner: &str, id: &str) -> Result<&mut comments::Edit> {
        self.sheet_session_mut()?
            .threads
            .iter_mut()
            .filter(|t| t.id == owner)
            .flat_map(|t| &mut t.messages)
            .flat_map(|m| &mut m.edits)
            .find(|e| e.id == id)
            .context("Unknown edit")
    }

    fn check_sheet_comment(&self, text: &str, send: bool) -> Result<()> {
        ensure!(
            !text.trim().is_empty() && text.chars().count() <= 2000,
            "Write a comment (at most 2000 characters)"
        );
        ensure!(
            !send || self.linked,
            "Not linked to a conversation — open it from nexus with /studio"
        );
        Ok(())
    }

    fn sheet_comment_create(
        &mut self,
        tab: &str,
        range: &str,
        text: String,
        send: bool,
    ) -> Result<Event> {
        self.check_sheet_comment(&text, send)?;
        let s = self.sheet_session()?;
        ensure!(
            s.threads.len() < 200,
            "This sheet has 200 threads; resolve older threads first"
        );
        let anchor = comments::Anchor::new(&s.sheet, tab, range)?;
        let tab = tab_name(&s.sheet, &anchor.tab)?.to_string();
        let id = uuid::Uuid::new_v4().to_string();
        self.sheet_session_mut()?.threads.push(comments::Thread {
            id: id.clone(),
            anchor: anchor.clone(),
            orphaned: false,
            severity: comments::Severity::Consider,
            created: comments::now(),
            resolved: None,
            messages: vec![comments::Message::new_for(
                comments::Role::You,
                text.clone(),
                vec![],
            )],
        });
        self.persist_sheet_feedback()?;
        if send {
            self.outgoing.push(LinkMessage::SheetComment {
                path: self.path.clone(),
                thread: id,
                tab,
                range: anchor.range,
                quote: anchor.quote,
                text,
            });
        }
        Ok(self.sheet_feedback_event(vec![]))
    }

    fn sheet_comment_reply(&mut self, id: &str, text: String, send: bool) -> Result<Event> {
        self.check_sheet_comment(&text, send)?;
        let thread = self
            .sheet_session_mut()?
            .threads
            .iter_mut()
            .find(|t| t.id == id)
            .context("Unknown thread")?;
        ensure!(
            thread.messages.len() < 100,
            "Thread has reached 100 messages"
        );
        thread.messages.push(comments::Message::new_for(
            comments::Role::You,
            text.clone(),
            vec![],
        ));
        self.persist_sheet_feedback()?;
        if send {
            self.outgoing.push(LinkMessage::Reply {
                deck_path: self.path.clone(),
                thread_id: id.into(),
                text,
            });
        }
        Ok(self.sheet_feedback_event(vec![]))
    }

    pub(super) fn sheet_sync(&mut self) -> Result<(Option<Event>, bool)> {
        let next_stamp = stamp(&self.path);
        let comments_stamp = stamp(&comments::sidecar(&self.path));
        let mut changed = vec![];
        let mut reloaded = false;
        if next_stamp != self.deck_stamp {
            self.deck_stamp = next_stamp;
            let next = files::read_sheet(&self.path)?;
            let revision = next.revision();
            if revision != self.known_revision {
                let workbook = Workbook::load(&next)?;
                let s = self.sheet_session_mut()?;
                changed = sheet::read::changed_cells(&s.sheet, &next);
                let previous = s.replace(next, revision.clone());
                s.undo.push(previous);
                s.undo_threads.push(s.threads.clone());
                s.redo.clear();
                s.redo_threads.clear();
                s.workbook = workbook;
                self.known_revision = revision;
                reloaded = true;
            }
        }
        let comments_changed = comments_stamp != self.comments_stamp;
        if comments_changed {
            let threads = comments::load(&self.path)?;
            let s = self.sheet_session_mut()?;
            if !reloaded {
                s.undo.push(s.sheet.clone());
                s.undo_threads.push(s.threads.clone());
                s.redo.clear();
                s.redo_threads.clear();
            }
            s.threads = threads;
            self.comments_stamp = comments_stamp;
        }
        let s = self.sheet_session_mut()?;
        comments::reconcile(&mut s.threads, &s.sheet);
        if !reloaded && !comments_changed {
            return Ok((None, false));
        }
        let mut event = self.sheet_event();
        if let Event::Sheet {
            changed: flash,
            notice,
            ..
        } = &mut event
        {
            *flash = changed;
            if reloaded {
                *notice = Some(
                    "The sheet changed on disk and was reloaded. Undo takes the change back."
                        .into(),
                );
            }
        }
        Ok((Some(event), reloaded))
    }

    pub(super) fn refuse_sheet_command(command: &Command) -> Result<()> {
        ensure!(
            !matches!(
                command,
                Command::NewSheet { .. }
                    | Command::SheetApply { .. }
                    | Command::SheetValues { .. }
                    | Command::SheetCommentCreate { .. }
                    | Command::SheetFocus { .. }
                    | Command::SheetExport { .. }
            ),
            "open a sheet first"
        );
        Ok(())
    }

    pub(super) fn sheet_mutates(command: &Command) -> bool {
        matches!(
            command,
            Command::SheetApply { .. }
                | Command::Undo {}
                | Command::Redo {}
                | Command::HistoryRestore { .. }
                | Command::ApplyEdit { .. }
        )
    }
}
