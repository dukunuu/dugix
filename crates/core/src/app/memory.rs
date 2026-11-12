//! Memory snapshots, recall hints, editing, and tidying for conversations.

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::App;
use crate::db::Message;
use crate::memory::{self, book, recall};

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
    /// Freeze memory for a new session or cache epoch.
    pub fn refresh_memory_snapshot(&mut self) {
        let conn = self.db.conn();
        let space = &self.active_space.id;
        let chapters = memory::visible_chapters(conn, space).unwrap_or_default();
        let facts = memory::visible_facts(conn, space).unwrap_or_default();
        let labels = memory::share_labels(conn, space, &chapters).unwrap_or_default();
        self.memory_docs = book::docs(&self.space.files_dir(&self.active_space.name));
        self.memory_snapshot = book::toc_with_labels(&chapters, &facts, &self.memory_docs, &labels);
        self.memory_tidy_due = chapters
            .iter()
            .filter(|c| {
                facts.iter().filter(|f| f.chapter_id == c.id).count() > memory::TIDY_HINT_FACTS
            })
            .count();
        self.bump_cache_epoch();
    }

    #[must_use]
    pub fn memory_snapshot(&self) -> &str {
        &self.memory_snapshot
    }

    #[must_use]
    pub fn memory_tidy_due(&self) -> usize {
        self.memory_tidy_due
    }

    pub(crate) fn bump_cache_epoch(&mut self) {
        self.cache_epoch = self.cache_epoch.wrapping_add(1);
        self.meter.warned = false;
        self.prompt_datetime = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M UTC, %A")
            .to_string();
        // Exact usage from a different prompt/model lane cannot measure this epoch.
        self.meter.total = None;
        self.meter.last_cache_rate = None;
        self.meter.turn_cache = super::CacheTally::default();
    }

    #[must_use]
    pub(crate) fn prompt_cache_key_for(&self, session_id: &str) -> String {
        format!("{session_id}:{}", self.cache_epoch)
    }

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
            &memory::visible_chapters(conn, space).ok()?,
            &memory::visible_facts(conn, space).ok()?,
            &self.memory_docs,
            &already,
        )
    }

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

    pub fn finish_memory_edit(&mut self, space_name: &str, path: &std::path::Path) -> Result<()> {
        let space = self.space_id_by_name(space_name)?;
        let text = std::fs::read_to_string(path)?;
        let summary = book::apply_edit(
            self.db.conn(),
            &space,
            &book::parse_edit(&text),
            book::opened_at(&text),
        )?;
        let _ = std::fs::remove_file(path);
        if space == self.active_space.id {
            self.refresh_memory_snapshot();
            self.refresh_toolbox();
        }
        self.push_status(summary.to_string());
        Ok(())
    }

    pub fn memory_markdown(&self) -> Result<String> {
        let conn = self.db.conn();
        let space = &self.active_space.id;
        Ok(book::render_edit(
            &memory::chapters(conn, space)?,
            &memory::facts(conn, space)?,
        ))
    }

    pub fn save_memory_markdown(&mut self, text: &str) -> Result<book::EditSummary> {
        let summary = book::apply_edit(
            self.db.conn(),
            &self.active_space.id,
            &book::parse_edit(text),
            book::opened_at(text),
        )?;
        self.refresh_memory_snapshot();
        self.refresh_toolbox();
        Ok(summary)
    }

    pub(super) fn memory_share(
        &mut self,
        title: &str,
        target: Option<&memory::ShareTarget>,
    ) -> Result<()> {
        let conn = self.db.conn();
        let space = &self.active_space.id;
        let Some(target) = target else {
            let listing = memory::sharing_list(conn, space)?;
            self.push_status(listing);
            return Ok(());
        };
        let chapter = memory::find_chapter(conn, space, title)?
            .with_context(|| format!("no chapter titled {title:?} in this space"))?;
        let chapter = memory::share_chapter(conn, &chapter.id, target.resolve(conn)?)?;
        let status = if chapter.shared_with == memory::Share::Private {
            format!("{:?} is private", chapter.title)
        } else {
            let labels = memory::share_labels(conn, space, std::slice::from_ref(&chapter))?;
            format!(
                "{:?} {}",
                chapter.title,
                labels[&chapter.id]
                    .trim()
                    .trim_start_matches('(')
                    .trim_end_matches(')')
            )
        };
        self.refresh_memory_snapshot();
        self.push_status(status);
        Ok(())
    }

    pub fn memory_tidy(&mut self) -> Result<()> {
        if self.incognito {
            self.push_status("memory is off in incognito");
            return Ok(());
        }
        self.send_message(crate::prompts::render(crate::prompts::MEMORY_TIDY, &[]))
    }
}
