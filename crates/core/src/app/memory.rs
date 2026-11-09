//! The App side of memory (`docs/memory.md`): the frozen table of contents
//! in the system prompt, per-message hints, editing, and `/memory tidy`.
//! Storage and text forms live in `crate::memory`.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use tokio::sync::mpsc;

use super::App;
use crate::db::Message;
use crate::memory::{self, book, recall, tidy};
use crate::provider::{ChatMessage, ChatParams};

/// A finished `/memory tidy` review, or why it could not run.
pub type MemoryTidyResult = std::result::Result<tidy::Plan, String>;

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

    /// Move a pre-db `memory.md` of the active space into the book, once.
    fn import_legacy_memory(&mut self) {
        let path = self.space.memory_path(&self.active_space.name);
        if !path.exists() {
            return;
        }
        match memory::import_legacy(self.db.conn(), &self.active_space.id, &path) {
            Ok(0) => {}
            Ok(n) => self.push_status(format!(
                "memory moved into the database · {n} facts in \"{}\"",
                memory::ABOUT_YOU
            )),
            Err(error) => self.push_status(format!("memory import failed: {error}")),
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

    /// `/memory tidy`: review every changed chapter with the helper model,
    /// then the chapter titles. The plan arrives as `AppEvent::Memory` and
    /// changes nothing until `/memory apply`.
    pub fn memory_tidy(&mut self) {
        if self.incognito {
            self.push_status("memory is off in incognito");
            return;
        }
        if self.memory_rx.is_some() {
            self.push_status("memory tidy is already running");
            return;
        }
        let Some((provider, model)) = self.helper_backend() else {
            self.push_status("memory tidy needs a signed-in backend · /login");
            return;
        };
        let conn = self.db.conn();
        let space = self.active_space.id.clone();
        let (Ok(chapters), Ok(facts)) =
            (memory::chapters(conn, &space), memory::facts(conn, &space))
        else {
            self.push_status("memory could not be read");
            return;
        };
        let changed: Vec<memory::Chapter> = tidy::changed(&chapters, &facts)
            .into_iter()
            .cloned()
            .collect();
        if changed.is_empty() && chapters.len() < 2 {
            self.push_status("memory is tidy · nothing changed since the last review");
            return;
        }
        let provider = provider.usage_kind("memory");
        let (tx, rx) = mpsc::unbounded_channel();
        self.memory_rx = Some(rx);
        self.push_status(format!(
            "tidying memory · {} chapter{} to review",
            changed.len(),
            if changed.len() == 1 { "" } else { "s" }
        ));
        tokio::spawn(async move {
            let ask = |prompt: String| {
                let provider = provider.clone();
                let model = model.clone();
                async move {
                    provider
                        .complete_with_params(
                            &model,
                            vec![ChatMessage::text("user", prompt)],
                            &ChatParams::default(),
                        )
                        .await
                        .map(|completion| completion.text)
                }
            };
            let reviews = futures_util::future::join_all(
                changed
                    .iter()
                    .map(|chapter| ask(tidy::chapter_prompt(chapter, &facts))),
            )
            .await;
            let mut plan = tidy::Plan {
                space_id: space,
                ..tidy::Plan::default()
            };
            for (chapter, review) in changed.iter().zip(reviews) {
                match review {
                    Ok(reply) => plan
                        .chapters
                        .push(tidy::parse_chapter(&reply, chapter, &facts)),
                    Err(error) => {
                        let _ = tx.send(Err(format!("memory tidy failed: {error}")));
                        return;
                    }
                }
            }
            if chapters.len() >= 2 {
                match ask(tidy::book_prompt(&chapters, &facts)).await {
                    Ok(reply) => plan.book = tidy::parse_book(&reply, &chapters),
                    Err(error) => {
                        let _ = tx.send(Err(format!("memory tidy failed: {error}")));
                        return;
                    }
                }
            }
            let _ = tx.send(Ok(plan));
        });
    }

    /// A tidy review landed: show its preview in the open session (one is
    /// started when none is open) and hold the plan for `/memory apply`.
    pub fn on_memory_result(&mut self, result: Option<MemoryTidyResult>) {
        self.memory_rx = None;
        let plan = match result {
            Some(Ok(plan)) => plan,
            Some(Err(error)) => {
                self.push_status(error);
                return;
            }
            None => return,
        };
        if plan.space_id != self.active_space.id {
            self.push_status("memory tidy discarded · the space changed");
            return;
        }
        if plan.is_empty() {
            self.push_status("memory is tidy · nothing to change");
            return;
        }
        let facts = memory::facts(self.db.conn(), &plan.space_id).unwrap_or_default();
        let preview = tidy::preview(&plan, &facts);
        let model = self.current_model.clone().unwrap_or_default();
        let shown = self
            .ensure_session("memory tidy", model)
            .and_then(|session| self.push_note(&session, "memory_tidy", preview));
        if let Err(error) = shown {
            self.push_status(format!("memory tidy preview failed: {error}"));
            return;
        }
        self.memory_plan = Some(plan);
        self.push_history_invalidated();
        self.push_status("review the memory changes · /memory apply · /memory discard");
    }

    /// `/memory apply`: save an undo point, then apply the pending plan.
    pub fn memory_apply(&mut self) -> Result<()> {
        let Some(plan) = self.memory_plan.take() else {
            self.push_status("no memory changes to apply · run /memory tidy first");
            return Ok(());
        };
        if plan.space_id != self.active_space.id {
            self.push_status("memory changes discarded · the space changed");
            return Ok(());
        }
        let snapshot = memory::snapshot(self.db.conn(), &plan.space_id)?;
        self.space.ensure_space_dir(&self.active_space.name)?;
        std::fs::write(self.memory_undo_path(), serde_json::to_vec(&snapshot)?)?;
        let applied = tidy::apply(self.db.conn(), &plan)?;
        self.refresh_memory_snapshot();
        self.refresh_toolbox();
        self.push_status(applied.to_string());
        Ok(())
    }

    /// `/memory discard`: drop the pending plan.
    pub fn memory_discard(&mut self) {
        let message = if self.memory_plan.take().is_some() {
            "memory changes discarded"
        } else {
            "no memory changes pending"
        };
        self.push_status(message);
    }

    /// `/memory undo`: restore memory to just before the last applied tidy.
    pub fn memory_undo(&mut self) -> Result<()> {
        let path = self.memory_undo_path();
        let Ok(bytes) = std::fs::read(&path) else {
            self.push_status("nothing to undo · no tidy applied in this space");
            return Ok(());
        };
        let snapshot: memory::Snapshot = serde_json::from_slice(&bytes)?;
        memory::restore(self.db.conn(), &snapshot)?;
        std::fs::remove_file(&path)?;
        self.refresh_memory_snapshot();
        self.refresh_toolbox();
        self.push_status("memory restored to before the last tidy");
        Ok(())
    }

    fn memory_undo_path(&self) -> PathBuf {
        self.space
            .space_dir(&self.active_space.name)
            .join("memory-undo.json")
    }
}
