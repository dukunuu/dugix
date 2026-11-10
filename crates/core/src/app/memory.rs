//! The App side of memory (`docs/memory.md`): the frozen table of contents
//! in the system prompt, per-message hints, editing, and `/memory tidy`.
//! Storage and text forms live in `crate::memory`.

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::App;
use crate::db::Message;
use crate::memory::{self, book, recall};

/// A transcript row the app writes itself (no model, tokens or cost).
pub(super) fn note(role: &str, content: String) -> Message {
    Message {
        role: role.to_string(),
        content,
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    }
}

impl App {
    /// Rebuild the frozen `## Memory` section from the db and the space's
    /// docs, and begin a new prompt-cache epoch. Runs when a session starts
    /// or switches, never mid-session: a fact saved during a chat is already
    /// in its transcript.
    pub fn refresh_memory_snapshot(&mut self) {
        self.import_legacy_memory();
        let conn = self.db.conn();
        let space = &self.active_space.id;
        let chapters = memory::chapters(conn, space).unwrap_or_default();
        let facts = memory::facts(conn, space).unwrap_or_default();
        self.memory_docs = book::docs(&self.space.files_dir(&self.active_space.name));
        self.memory_snapshot = book::toc(&chapters, &facts, &self.memory_docs);
        self.memory_tidy_due = chapters
            .iter()
            .filter(|c| {
                facts.iter().filter(|f| f.chapter_id == c.id).count() > memory::TIDY_HINT_FACTS
            })
            .count();
        self.bump_cache_epoch();
    }

    /// Move every space's pre-db `memory.md` into the book, once.
    fn import_legacy_memory(&mut self) {
        let Ok(spaces) = self.db.list_spaces() else {
            return;
        };
        let mut total = 0;
        let mut imported_spaces = 0;
        for space in spaces {
            let path = self.space.memory_path(&space.name);
            if !path.exists() {
                continue;
            }
            match memory::import_legacy(self.db.conn(), &space.id, &path) {
                Ok(0) => {}
                Ok(n) => {
                    total += n;
                    imported_spaces += 1;
                }
                Err(error) => {
                    self.push_status(format!("memory import failed for {}: {error}", space.name));
                }
            }
        }
        if total > 0 {
            self.push_status(if imported_spaces == 1 {
                format!(
                    "memory moved into the database · {total} facts in \"{}\"",
                    memory::ABOUT_YOU
                )
            } else {
                format!(
                    "memory moved into the database · {total} facts across {imported_spaces} spaces"
                )
            });
        }
    }

    /// The table of contents frozen for the active cache epoch.
    #[must_use]
    pub fn memory_snapshot(&self) -> &str {
        &self.memory_snapshot
    }

    /// Chapters over the tidy threshold, for the status bar.
    #[must_use]
    pub fn memory_tidy_due(&self) -> usize {
        self.memory_tidy_due
    }

    /// Advance the app-local prompt-cache epoch after a serialized-prefix
    /// boundary such as a memory or prompt refresh.
    pub(crate) fn bump_cache_epoch(&mut self) {
        self.cache_epoch = self.cache_epoch.wrapping_add(1);
        self.context_warned = false;
        self.prompt_datetime = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M UTC, %A")
            .to_string();
        // The exact total belongs to the previous serialized prompt/model
        // lane. Let the next request establish a fresh value rather than
        // pinning the context bar to an unrelated boundary.
        self.context_total = None;
        self.last_cache_rate = None;
        self.turn_cache = super::CacheTally::default();
    }

    /// Build a non-sensitive cache-lane key for one session and epoch.
    #[must_use]
    pub(crate) fn prompt_cache_key_for(&self, session_id: &str) -> String {
        format!("{session_id}:{}", self.cache_epoch)
    }

    /// The chapters and docs worth naming before `text` is sent, minus any
    /// already named in this session. `None` in incognito.
    pub(super) fn memory_hint(&self, text: &str) -> Option<recall::Recall> {
        if self.incognito {
            return None;
        }
        let already: HashSet<String> = self
            .messages
            .iter()
            .filter(|m| m.role == "recall")
            .filter_map(|m| recall::Recall::from_row(&m.content))
            .flat_map(|r| r.targets.into_iter().map(|t| t.key))
            .collect();
        let conn = self.db.conn();
        let space = &self.active_space.id;
        recall::hint(
            text,
            &memory::chapters(conn, space).ok()?,
            &memory::facts(conn, space).ok()?,
            &self.memory_docs,
            &already,
        )
    }

    /// Append an app-written row to the open session (persisted unless
    /// incognito).
    pub(super) fn push_note(
        &mut self,
        session_id: &str,
        role: &str,
        content: String,
    ) -> Result<()> {
        if !self.incognito {
            self.db.insert_message(
                session_id, role, &content, None, None, None, None, None, None,
            )?;
        }
        self.messages.push(note(role, content));
        Ok(())
    }

    fn space_id_by_name(&self, name: &str) -> Result<String> {
        self.db
            .list_spaces()?
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.id)
            .with_context(|| format!("no space named {name:?}"))
    }

    /// Render a space's memory to a Markdown file for `$EDITOR`; read it back
    /// with [`App::finish_memory_edit`].
    pub fn memory_edit_file(&self, space_name: &str) -> Result<PathBuf> {
        let space = self.space_id_by_name(space_name)?;
        let conn = self.db.conn();
        let text = book::render_edit(
            &memory::chapters(conn, &space)?,
            &memory::facts(conn, &space)?,
        );
        self.space.ensure_space_dir(space_name)?;
        let path = self.space.space_dir(space_name).join("memory.edit.md");
        std::fs::write(&path, text)?;
        Ok(path)
    }

    /// Apply an edited memory file as a diff, remove it, and refresh the
    /// table of contents when it belongs to the active space.
    pub fn finish_memory_edit(&mut self, space_name: &str, path: &std::path::Path) -> Result<()> {
        let space = self.space_id_by_name(space_name)?;
        let text = std::fs::read_to_string(path)?;
        let summary = book::apply_edit(self.db.conn(), &space, &book::parse_edit(&text))?;
        let _ = std::fs::remove_file(path);
        if space == self.active_space.id {
            self.refresh_memory_snapshot();
            self.refresh_toolbox();
        }
        self.push_status(summary.to_string());
        Ok(())
    }

    /// The book as editable Markdown (the host's memory document).
    pub fn memory_markdown(&self) -> Result<String> {
        let conn = self.db.conn();
        let space = &self.active_space.id;
        Ok(book::render_edit(
            &memory::chapters(conn, space)?,
            &memory::facts(conn, space)?,
        ))
    }

    /// Apply edited Markdown to the active space's memory (the host's memory
    /// document).
    pub fn save_memory_markdown(&mut self, text: &str) -> Result<book::EditSummary> {
        let summary = book::apply_edit(
            self.db.conn(),
            &self.active_space.id,
            &book::parse_edit(text),
        )?;
        self.refresh_memory_snapshot();
        self.refresh_toolbox();
        Ok(summary)
    }

    /// `/memory tidy`: ask the session model to tidy memory in the conversation.
    pub fn memory_tidy(&mut self) -> Result<()> {
        if self.incognito {
            self.push_status("memory is off in incognito");
            return Ok(());
        }
        self.send_message(crate::prompts::render(crate::prompts::MEMORY_TIDY, &[]))
    }
}
