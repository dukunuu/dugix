//! Device-local, per-session prompt prefixes. Only declared refreshes replace them.
//!
//! A provider prompt cache hits only when a request starts with exactly the
//! bytes of an earlier one, so the system prompt, the leading space images and
//! the tool definitions are built once per session and reused byte for byte,
//! across turns, session switches and restarts.

use std::collections::HashMap;

use chrono::Utc;
use rusqlite::OptionalExtension as _;
use sha2::{Digest as _, Sha256};

use super::App;
use crate::provider::{ChatMessage, ToolDef};

const DATETIME_FORMAT: &str = "%Y-%m-%d %H:%M UTC, %A";

#[derive(Clone)]
pub(super) struct Prefix {
    pub generation: u64,
    pub system: String,
    pub lead_images: Option<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub made_at: String,
    /// Empty once the prefix is declared stale, which forces a rebuild.
    identity_hash: String,
    /// The change first announced; later ones stay quiet until the next
    /// rebuild, since the model writes memory on most turns.
    seen_change: String,
}

pub(super) type Prefixes = HashMap<String, Prefix>;

pub(crate) fn hash(text: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn images_json(images: Option<&ChatMessage>) -> Option<String> {
    images.map(|m| serde_json::json!({"text": m.content, "images": m.images}).to_string())
}

fn images_from_json(text: &str) -> Option<ChatMessage> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut message = ChatMessage::text("system", value["text"].as_str()?);
    message.images = serde_json::from_value(value["images"].clone()).ok()?;
    Some(message)
}

fn tools_json(tools: &[ToolDef]) -> String {
    serde_json::to_string(tools).unwrap_or_default()
}

impl Prefix {
    /// Hash of the system prompt and the leading images.
    pub(crate) fn system_hash(&self) -> String {
        hash(&format!(
            "{}\n{}",
            self.system,
            images_json(self.lead_images.as_ref()).unwrap_or_default()
        ))
    }

    pub(crate) fn tools_hash(&self) -> String {
        hash(&tools_json(&self.tools))
    }

    fn content_hash(&self) -> String {
        hash(&format!("{}\n{}", self.system_hash(), self.tools_hash()))
    }
}

impl App {
    /// Whether this conversation's requests carry the workspace tools: from
    /// its first agent turn on, in every mode. Tool definitions come first in
    /// a request, so adding them costs the cache once; keeping them afterwards
    /// lets later mode changes cost nothing.
    pub(super) fn offers_workspace_tools(&self) -> bool {
        if self.work_mode.is_agent() {
            return true;
        }
        let Some(session) = &self.session else {
            return false;
        };
        let has = |prefix: &Prefix| prefix.tools.iter().any(|tool| tool.name == "fs");
        match self.prefixes.get(&session.id) {
            Some(prefix) => has(prefix),
            None => !self.incognito && self.load_prefix(&session.id).is_some_and(|p| has(&p)),
        }
    }

    /// What must change the prefix immediately: the model, the instructions
    /// the user chose, and the first use of agent mode. The mode itself and
    /// the workspace are not here — a mode note in the transcript carries them.
    fn prefix_identity(&self) -> String {
        hash(
            &serde_json::json!({
                "model": self.current_model,
                "agent_tools": self.offers_workspace_tools(),
                "style": self.settings.reply_style,
                "verbosity": self.verbosity,
                "interactive": self.interactive_ui,
                "incognito": self.incognito,
                "overlay": self.system_prompt_overlay,
                "compact_tools": self.model_profile().compact_tools,
            })
            .to_string(),
        )
    }

    fn load_prefix(&self, session_id: &str) -> Option<Prefix> {
        let (generation, system, images, tools, made_at, identity_hash) = self
            .db
            .conn()
            .query_row(
                "SELECT generation, system, lead_images_json, tools_json, made_at, identity_hash
                 FROM cache.session_prefixes WHERE session_id = ?1",
                [session_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten()?;
        Some(Prefix {
            generation: u64::try_from(generation).ok()?,
            system,
            lead_images: match images {
                Some(text) => Some(images_from_json(&text)?),
                None => None,
            },
            tools: serde_json::from_str(&tools).ok()?,
            made_at,
            identity_hash,
            seen_change: String::new(),
        })
    }

    fn current_lead_images(&self) -> Option<ChatMessage> {
        if self.current_model_supports_images() {
            self.space_images_message()
        } else {
            None
        }
    }

    /// Ensure the active session has a prefix. Transient inputs (memory edits,
    /// new files, new tools, the clock) never replace an existing one; only a
    /// changed identity or a declared refresh does.
    pub(super) fn ensure_prompt_prefix(&mut self) {
        let Some(id) = self.session.as_ref().map(|s| s.id.clone()) else {
            return;
        };
        if !self.prefixes.contains_key(&id)
            && !self.incognito
            && let Some(prefix) = self.load_prefix(&id)
        {
            self.prefixes.insert(id.clone(), prefix);
        }
        let identity = self.prefix_identity();
        if let Some(prefix) = self.prefixes.get(&id)
            && prefix.identity_hash == identity
        {
            self.prompt_datetime.clone_from(&prefix.made_at);
            return;
        }
        let generation = self.prefixes.get(&id).map_or(0, |p| p.generation + 1);
        self.refresh_memory_snapshot();
        self.prompt_datetime = Utc::now().format(DATETIME_FORMAT).to_string();
        let prefix = Prefix {
            generation,
            system: self.system_prompt(),
            lead_images: self.current_lead_images(),
            tools: self.chat_tool_defs(),
            made_at: self.prompt_datetime.clone(),
            identity_hash: identity,
            seen_change: String::new(),
        };
        if !self.incognito {
            let _ = self.db.conn().execute(
                "INSERT OR REPLACE INTO cache.session_prefixes
                 (session_id, generation, system, lead_images_json, tools_json, made_at, identity_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    id,
                    i64::try_from(prefix.generation).unwrap_or(i64::MAX),
                    prefix.system,
                    images_json(prefix.lead_images.as_ref()),
                    tools_json(&prefix.tools),
                    prefix.made_at,
                    prefix.identity_hash
                ],
            );
        }
        self.prefixes.insert(id, prefix);
    }

    /// Declare a session's prefix stale: the next request rebuilds it under a
    /// new generation, which also selects a fresh provider-cache lane.
    pub(super) fn invalidate_prefix(&mut self, session_id: &str) {
        if let Some(prefix) = self.prefixes.get_mut(session_id) {
            prefix.identity_hash.clear();
        }
        if !self.incognito {
            let _ = self.db.conn().execute(
                "UPDATE cache.session_prefixes SET identity_hash = '' WHERE session_id = ?1",
                [session_id],
            );
        }
    }

    pub(super) fn active_prefix(&self) -> Option<&Prefix> {
        self.session.as_ref().and_then(|s| self.prefixes.get(&s.id))
    }

    /// The provider cache lane for a session: its id plus the prefix generation.
    #[must_use]
    pub(crate) fn prompt_cache_key_for(&self, session_id: &str) -> String {
        let generation = self.prefixes.get(session_id).map_or_else(
            || {
                self.db
                    .conn()
                    .query_row(
                        "SELECT generation FROM cache.session_prefixes WHERE session_id = ?1",
                        [session_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap_or(0)
            },
            |prefix| i64::try_from(prefix.generation).unwrap_or(i64::MAX),
        );
        format!("{session_id}:{generation}")
    }

    /// Say once per frozen prefix that the live prompt no longer matches it.
    pub(super) fn warn_stale_prefix(&mut self) {
        self.ensure_prompt_prefix();
        let Some(frozen) = self.active_prefix().cloned() else {
            return;
        };
        self.refresh_memory_snapshot();
        let candidate = Prefix {
            system: self.system_prompt(),
            lead_images: self.current_lead_images(),
            tools: self.chat_tool_defs(),
            ..frozen.clone()
        };
        let change = candidate.content_hash();
        if change == frozen.content_hash() || !frozen.seen_change.is_empty() {
            return;
        }
        if let Some(id) = self.session.as_ref().map(|s| s.id.clone())
            && let Some(prefix) = self.prefixes.get_mut(&id)
        {
            prefix.seen_change = change;
        }
        self.push_status(
            "prompt frozen · memory/files/tools changed since this chat began · /refresh to include them (one cache miss)",
        );
    }

    pub(super) fn refresh_prompt(&mut self) {
        self.reload_skills();
        self.bump_cache_epoch();
        self.push_status("prompt refreshed · next request rebuilds the cache");
    }

    /// Carry today's date on a message when the frozen prompt's date is older,
    /// so the model knows the day without the prefix changing.
    pub(super) fn dated_user_message(&self, text: String, now: chrono::DateTime<Utc>) -> String {
        let stale = self
            .active_prefix()
            .is_some_and(|p| !p.made_at.starts_with(&now.format("%Y-%m-%d").to_string()));
        if stale {
            format!("[now: {}]\n\n{text}", now.format(DATETIME_FORMAT))
        } else {
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;
    use crate::app::tests::app_with_key;

    fn started() -> App {
        let mut a = app_with_key();
        a.current_model = Some("a/one".into());
        a.ensure_session("hello", "a/one".into()).unwrap();
        a.ensure_prompt_prefix();
        a
    }

    fn frozen(a: &App) -> (u64, String, String) {
        let prefix = a.active_prefix().unwrap();
        let id = a.session.as_ref().unwrap().id.clone();
        (
            prefix.generation,
            prefix.system.clone(),
            a.prompt_cache_key_for(&id),
        )
    }

    #[test]
    fn identity_changes_rebuild_and_everything_else_stays_frozen() {
        let mut a = started();
        let (generation, system, key) = frozen(&a);
        assert_eq!(generation, 0);
        assert!(key.ends_with(":0"));

        // Memory, space instructions, skills, files, the clock: none rebuild.
        let space = a.active_space.id.clone();
        crate::memory::add(a.db.conn(), &space, "About you", "a fact", None, Some(true)).unwrap();
        std::fs::create_dir_all(a.space.space_dir(&a.active_space.name)).unwrap();
        std::fs::write(
            a.space.instructions_path(&a.active_space.name),
            "Always be formal.",
        )
        .unwrap();
        a.skills.clear();
        a.prompt_datetime = "2001-01-01 00:00 UTC, Monday".into();
        a.refresh_memory_snapshot();
        assert!(
            a.system_prompt().contains("a fact"),
            "the live prompt moved on"
        );
        a.ensure_prompt_prefix();
        assert_eq!(frozen(&a), (generation, system.clone(), key.clone()));
        assert!(!system.contains("a fact") && !system.contains("formal"));
        assert_eq!(a.prompt_datetime, a.active_prefix().unwrap().made_at);

        // Each identity input rebuilds exactly once.
        let changes: [fn(&mut App); 5] = [
            |a| a.verbosity = "detailed".into(),
            |a| a.system_prompt_overlay = "Speak French.".into(),
            |a| a.interactive_ui = !a.interactive_ui,
            |a| a.settings.reply_style = "plain".into(),
            |a| a.current_model = Some("b/two".into()),
        ];
        for (index, change) in changes.into_iter().enumerate() {
            change(&mut a);
            a.ensure_prompt_prefix();
            let (now, ..) = frozen(&a);
            assert_eq!(now, index as u64 + 1, "change {index}");
            a.ensure_prompt_prefix();
            assert_eq!(frozen(&a).0, now, "change {index} rebuilt twice");
        }
        let rebuilt = frozen(&a);
        assert!(rebuilt.1.contains("a fact") && rebuilt.1.contains("formal"));
        assert!(rebuilt.2.ends_with(":5"));
    }

    #[test]
    fn refresh_rebuilds_once_and_says_so() {
        let mut a = started();
        let before = frozen(&a);
        a.last_status();
        a.run_command("refresh").unwrap();
        assert_eq!(
            a.last_status(),
            "prompt refreshed · next request rebuilds the cache"
        );
        a.ensure_prompt_prefix();
        a.ensure_prompt_prefix();
        let after = frozen(&a);
        assert_eq!(after.0, before.0 + 1);
        assert_ne!(after.2, before.2);
    }

    #[test]
    fn the_prefix_outlives_the_app() {
        let root = std::env::temp_dir().join(format!("dugix-freeze-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let open = || {
            let space = crate::space::Space { root: root.clone() };
            let db = crate::db::Db::open(&space.db_path()).unwrap();
            let mut a = App::new(db, Some("sk-or-test-key"), space);
            a.models = app_with_key().models;
            a.current_model = Some("a/one".into());
            a
        };
        let mut first = open();
        first.ensure_session("hello", "a/one".into()).unwrap();
        first.ensure_prompt_prefix();
        first.run_command("refresh").unwrap();
        first.ensure_prompt_prefix();
        let session = first.session.clone().unwrap();
        let expected = frozen(&first);
        assert_eq!(expected.0, 1);
        std::fs::write(
            first.space.instructions_path(&first.active_space.name),
            "New standing instructions.",
        )
        .unwrap();
        drop(first);

        let mut second = open();
        second.session = Some(session);
        second.ensure_prompt_prefix();
        assert_eq!(frozen(&second), expected);
        assert!(!expected.1.contains("standing instructions"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incognito_prefixes_stay_in_memory() {
        let mut a = app_with_key();
        a.current_model = Some("a/one".into());
        a.incognito = true;
        a.ensure_session("hello", "a/one".into()).unwrap();
        a.ensure_prompt_prefix();
        assert!(a.active_prefix().is_some());
        let rows: i64 =
            a.db.conn()
                .query_row("SELECT COUNT(*) FROM cache.session_prefixes", [], |r| {
                    r.get(0)
                })
                .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn deleting_a_chat_drops_its_frozen_prefix() {
        let a = started();
        let id = a.session.as_ref().unwrap().id.clone();
        let count = |a: &App| -> i64 {
            a.db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM cache.session_prefixes WHERE session_id = ?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(count(&a), 1);
        a.db.delete_session(&id).unwrap();
        assert_eq!(count(&a), 0);
    }

    #[test]
    fn the_date_rides_the_message_only_on_a_new_day() {
        let a = started();
        let made = a.active_prefix().unwrap().made_at.clone();
        let day = chrono::NaiveDate::parse_from_str(&made[..10], "%Y-%m-%d").unwrap();
        let at =
            |date: chrono::NaiveDate| Utc.from_utc_datetime(&date.and_hms_opt(23, 59, 0).unwrap());
        assert_eq!(a.dated_user_message("hi".into(), at(day)), "hi");
        let next = a.dated_user_message("hi".into(), at(day.succ_opt().unwrap()));
        assert!(
            next.starts_with(&format!("[now: {} 23:59 UTC", day.succ_opt().unwrap())),
            "{next}"
        );
        assert!(next.ends_with("\n\nhi"));
    }
}
