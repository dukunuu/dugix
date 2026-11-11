//! Spaces, settings, model prefs, sessions and messages.

use anyhow::Result;
use chrono::Utc;
use rusqlite::OptionalExtension;
use uuid::Uuid;

use super::{
    DEFAULT_SPACE, Db, DecisionRow, FailureRow, HarnessSummary, Message, ModelPref, Persona,
    Session, Space, stage_content,
};
#[cfg(test)]
use super::{add_session_sources, search_session_sources};

impl Db {
    /// Summarize tool calls at or after a UTC RFC3339 cutoff, with at most
    /// 50 failure modes ranked by count, then most recent occurrence.
    pub fn harness_summary(&self, since: &str) -> Result<HarnessSummary> {
        let (calls, failures) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(ok = 0), 0)
             FROM cache.tool_events WHERE at >= ?1",
            [since],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )?;
        // SQLite's single MAX aggregate takes bare columns from the row
        // supplying that maximum, so session_id belongs to the latest event.
        let mut stmt = self.conn.prepare(
            "SELECT tool, action, error_kind, COUNT(*), MAX(at) AS last_at, session_id
             FROM cache.tool_events WHERE at >= ?1 AND ok = 0
             GROUP BY tool, action, error_kind
             ORDER BY COUNT(*) DESC, last_at DESC LIMIT 50",
        )?;
        let rows = stmt
            .query_map([since], |row| {
                Ok(FailureRow {
                    tool: row.get(0)?,
                    action: row.get(1)?,
                    error_kind: row.get(2)?,
                    count: row.get::<_, i64>(3)? as u64,
                    last_at: row.get(4)?,
                    last_session_id: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(HarnessSummary {
            calls,
            failures,
            rows,
        })
    }

    /// Set or remove a session's device-local workspace binding.
    pub fn set_session_workspace(&self, id: &str, root: Option<&std::path::Path>) -> Result<()> {
        if let Some(root) = root {
            self.conn.execute(
                "INSERT INTO cache.session_workspaces (session_id, root, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_id) DO UPDATE SET
                     root = excluded.root, updated_at = excluded.updated_at",
                (id, root.to_string_lossy(), Utc::now().to_rfc3339()),
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM cache.session_workspaces WHERE session_id = ?1",
                [id],
            )?;
        }
        Ok(())
    }

    /// Store the mode alongside an existing device-local workspace binding.
    pub fn set_session_workspace_mode(&self, id: &str, mode: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE cache.session_workspaces SET mode = ?2 WHERE session_id = ?1",
            (id, mode),
        )?;
        Ok(())
    }

    /// Missing mode values are legacy plain-agent bindings.
    pub fn session_workspace_mode(&self, id: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT mode FROM cache.session_workspaces WHERE session_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Policy decisions for one session, in execution order.
    pub fn decisions(&self, session_id: &str) -> Result<Vec<DecisionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tool, action, class, summary, by, verdict, p_in_scope, risk, user_answer, cost
             FROM cache.decisions WHERE session_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([session_id], |row| {
            Ok(DecisionRow {
                id: row.get(0)?,
                tool: row.get(1)?,
                action: row.get(2)?,
                class: row.get(3)?,
                summary: row.get(4)?,
                by: row.get(5)?,
                verdict: row.get(6)?,
                p_in_scope: row.get(7)?,
                risk: row.get(8)?,
                user_answer: row.get(9)?,
                cost: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Read a session's workspace binding on this device, if any.
    pub fn session_workspace(&self, id: &str) -> Option<std::path::PathBuf> {
        self.conn
            .query_row(
                "SELECT root FROM cache.session_workspaces WHERE session_id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .map(std::path::PathBuf::from)
    }

    /// Persist a session's device-local tool-result masking boundary.
    pub fn set_session_mask(&self, id: &str, mask_through: usize) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cache.session_masks (session_id, mask_through, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET
                 mask_through = excluded.mask_through, updated_at = excluded.updated_at",
            (id, i64::try_from(mask_through)?, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Read a session's masking boundary on this device, or zero if absent.
    pub fn session_mask(&self, id: &str) -> usize {
        self.conn
            .query_row(
                "SELECT mask_through FROM cache.session_masks WHERE session_id = ?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0)
    }

    /// The default space's id (always present after `migrate`).
    pub fn default_space_id(&self) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT id FROM spaces WHERE name = ?1",
            [DEFAULT_SPACE],
            |r| r.get(0),
        )?)
    }

    pub fn create_space(&self, name: &str) -> Result<Space> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO spaces (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            (&id, name, &now),
        )?;
        Ok(Space {
            id,
            name: name.to_string(),
            created_at: now,
        })
    }

    /// Spaces oldest-first (`default` was inserted first, so it naturally leads).
    pub fn list_spaces(&self) -> Result<Vec<Space>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at FROM spaces ORDER BY created_at ASC")?;
        let rows = stmt.query_map([], |r| {
            Ok(Space {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn rename_space(&self, id: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE spaces SET name = ?2, updated_at = ?3 WHERE id = ?1",
            (id, name, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Delete a space, reassigning its sessions to `default` rather than
    /// deleting them — only the space's own memory/instructions are lost.
    /// The reassignment bumps every moved session's version (a mutation,
    /// sync-wise); the space itself is tombstoned.
    pub fn delete_space(&self, id: &str) -> Result<()> {
        let default_id = self.default_space_id()?;
        self.conn.execute(
            "UPDATE sessions SET space_id = ?1, updated_at = ?2 WHERE space_id = ?3",
            (&default_id, Utc::now().to_rfc3339(), id),
        )?;
        self.conn
            .execute("DELETE FROM spaces WHERE id = ?1", [id])?;
        self.tombstone("spaces", id)?;
        crate::memory::delete_space(&self.conn, id)?;
        Ok(())
    }

    /// Number of sessions currently in a space (shown in the space picker).
    pub fn count_sessions(&self, space_id: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE space_id = ?1",
            [space_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// The most recent user/assistant message of a session — the session
    /// picker's preview strip, so you can see what a session is about before
    /// opening it.
    pub fn last_message_preview(&self, session_id: &str) -> Option<String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT content FROM messages WHERE session_id = ?1 \
                 AND role IN ('user','assistant') ORDER BY id DESC LIMIT 1",
            )
            .ok()?;
        let mut rows = stmt
            .query_map([session_id], |r| r.get::<_, String>(0))
            .ok()?;
        rows.next().and_then(Result::ok)
    }

    // --- key/value app settings ---

    /// Whether an `app_settings` key is device-local rather than syncable.
    /// Local keys describe this device's capabilities or per-device state
    /// (search endpoints, secrets, ui state); everything else
    /// is a user preference that should follow the user. New keys must be
    /// classified here — the default is sync, so a forgotten local key would
    /// silently sync to other devices.
    pub fn setting_is_local(key: &str) -> bool {
        key.starts_with("training:")
            || matches!(
                key,
                // Device capabilities / local services: a phone has no
                // localhost SearXNG, no ollama, and its API keys
                // are its own.
                "searxng_url"
                | "langsearch_key"
                | "search_provider"
                // Per-device ui/timing state.
                | "usage_range"
                | "ui_background"
                | "column_width"
                | "popup_sizes"
                // Whether a terminal draws graphics is a property of this device.
                | "image_viewer"
                | "last_update_check"
                | "jobs_seen"
                // Which remote device the ssh transport last synced with —
                // per-peer export cursors are device-local bookkeeping.
                | "sync_ssh_peer"
            )
    }

    /// Read one setting, returning `None` if it is missing or cannot be read.
    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let scope = if Self::setting_is_local(key) {
            "local"
        } else {
            "sync"
        };
        self.conn.execute(
            "INSERT INTO app_settings (key, value, scope, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET value = ?2, scope = ?3, updated_at = ?4",
            (key, value, scope, &now),
        )?;
        Ok(())
    }

    pub fn load_settings(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM app_settings")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Daily throttle for the startup update check: returns `true` (and
    /// records today as the check date) when no check has run today yet,
    /// `false` when one already has. The check is a courtesy, not a
    /// service — one tiny index fetch per day is plenty.
    pub fn update_check_due(&self) -> bool {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let last = self
            .conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'last_update_check'",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok();
        if last.as_deref() == Some(today.as_str()) {
            return false;
        }
        let _ = self.set_setting("last_update_check", &today);
        true
    }

    /// Set (or clear, with None) a model's reasoning effort.
    pub fn set_reasoning(&self, model_id: &str, effort: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO model_prefs (id, reasoning, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET reasoning = ?2, updated_at = ?3",
            (model_id, effort, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Flip a model's favorite flag; returns the new state.
    pub fn toggle_favorite(&self, model_id: &str) -> Result<bool> {
        self.conn.execute(
            "INSERT INTO model_prefs (id, favorite, updated_at) VALUES (?1, 1, ?2)
             ON CONFLICT(id) DO UPDATE SET favorite = 1 - favorite, updated_at = ?2",
            (model_id, Utc::now().to_rfc3339()),
        )?;
        let fav: i64 = self.conn.query_row(
            "SELECT favorite FROM model_prefs WHERE id = ?1",
            [model_id],
            |r| r.get(0),
        )?;
        Ok(fav != 0)
    }

    /// Record a model as just used (for the recents ordering).
    pub fn mark_model_used(&self, model_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO model_prefs (id, favorite, last_used, updated_at) VALUES (?1, 0, ?2, ?2)
             ON CONFLICT(id) DO UPDATE SET last_used = ?2, updated_at = ?2",
            (model_id, &now),
        )?;
        Ok(())
    }

    /// All stored prefs: (model id, favorite, last used, reasoning effort).
    pub fn load_model_prefs(&self) -> Result<Vec<ModelPref>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, favorite, last_used, reasoning FROM model_prefs")?;
        let rows = stmt.query_map([], |r| {
            Ok(ModelPref {
                id: r.get(0)?,
                favorite: r.get::<_, i64>(1)? != 0,
                last_used: r.get(2)?,
                reasoning: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_session(
        &self,
        title: &str,
        model: &str,
        space_id: &str,
        kind: &str,
    ) -> Result<Session> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (id, title, model, space_id, kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            (&id, title, model, space_id, kind, &now),
        )?;
        Ok(Session {
            id,
            title: title.to_string(),
            model: model.to_string(),
            slug: None,
            created_at: now,
            compact_summary: None,
            compact_through: 0,
            swarm_mode: false,
            kind: kind.to_string(),
            research_parent_id: None,
        })
    }

    /// A single session by id, or `None` if it doesn't exist (e.g. deleted
    /// out from under a watch).
    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        self.conn
            .query_row(
                "SELECT id, title, model, slug, created_at, compact_summary, compact_through, \
                 swarm_mode, kind, research_parent_id
                 FROM sessions WHERE id = ?1",
                [id],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        model: r.get(2)?,
                        slug: r.get(3)?,
                        created_at: r.get(4)?,
                        compact_summary: r.get(5)?,
                        compact_through: r.get(6)?,
                        swarm_mode: r.get::<_, i64>(7)? != 0,
                        kind: r.get(8)?,
                        research_parent_id: r.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Sessions in `space_id`, most-recently-updated first.
    pub fn list_sessions(&self, space_id: &str) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, model, slug, created_at, compact_summary, compact_through, \
             swarm_mode, kind, research_parent_id
             FROM sessions WHERE space_id = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([space_id], |r| {
            Ok(Session {
                id: r.get(0)?,
                title: r.get(1)?,
                model: r.get(2)?,
                slug: r.get(3)?,
                created_at: r.get(4)?,
                compact_summary: r.get(5)?,
                compact_through: r.get(6)?,
                swarm_mode: r.get::<_, i64>(7)? != 0,
                kind: r.get(8)?,
                research_parent_id: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Return the most recently updated session across all spaces, together
    /// with its owning space id. Used by the `nexus --continue` launcher.
    pub fn latest_session(&self) -> Result<Option<(String, Session)>> {
        self.conn
            .query_row(
                "SELECT space_id, id, title, model, slug, created_at, compact_summary,
                        compact_through, swarm_mode, kind, research_parent_id
                 FROM sessions
                 ORDER BY updated_at DESC, rowid DESC
                 LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        Session {
                            id: r.get(1)?,
                            title: r.get(2)?,
                            model: r.get(3)?,
                            slug: r.get(4)?,
                            created_at: r.get(5)?,
                            compact_summary: r.get(6)?,
                            compact_through: r.get(7)?,
                            swarm_mode: r.get::<_, i64>(8)? != 0,
                            kind: r.get(9)?,
                            research_parent_id: r.get(10)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Store an auto-compaction result: the digest plus how many raw messages
    /// it now covers.
    pub fn set_compaction(&self, session_id: &str, summary: &str, through: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET compact_summary = ?2, compact_through = ?3, updated_at = ?4
             WHERE id = ?1",
            (session_id, summary, through, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Persist a session's `/swarm` mode toggle.
    pub fn set_session_swarm_mode(&self, session_id: &str, on: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET swarm_mode = ?2, updated_at = ?3 WHERE id = ?1",
            (session_id, i64::from(on), Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// A session's `/swarm` roster, in display order.
    pub fn list_swarm_personas(&self, session_id: &str) -> Result<Vec<Persona>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, model, persona FROM swarm_personas
             WHERE session_id = ?1 ORDER BY ord ASC",
        )?;
        let rows = stmt.query_map([session_id], |r| {
            Ok(Persona {
                name: r.get(0)?,
                model: r.get(1)?,
                blurb: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Replace a session's whole `/swarm` roster with `personas`, in order.
    /// The roster has no per-row LWW — saving is DELETE-all + INSERT — so
    /// the collection is versioned by bumping the owning session, and each
    /// removed slot is tombstoned for the merge engine.
    pub fn save_swarm_personas(&self, session_id: &str, personas: &[Persona]) -> Result<()> {
        let old: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT ord FROM swarm_personas WHERE session_id = ?1")?;
            let rows = stmt.query_map([session_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for ord in old {
            self.tombstone("swarm_personas", &format!("{session_id}:{ord}"))?;
        }
        self.conn.execute(
            "DELETE FROM swarm_personas WHERE session_id = ?1",
            [session_id],
        )?;
        for (i, p) in personas.iter().enumerate() {
            self.conn.execute(
                "INSERT INTO swarm_personas (session_id, ord, name, model, persona)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (session_id, i as i64, &p.name, &p.model, &p.blurb),
            )?;
        }
        self.touch("sessions", session_id)
    }

    /// Set a session's `research_parent_id` after creation (e.g. when
    /// a regular chat is promoted to research and the original session is
    /// created first).
    pub fn set_research_parent(&self, id: &str, parent_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET research_parent_id = ?2, updated_at = ?3 WHERE id = ?1",
            (id, parent_id, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Set a session's title and (optionally) its generated slug.
    pub fn set_session_title(&self, id: &str, title: &str, slug: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2, slug = COALESCE(?3, slug), updated_at = ?4
             WHERE id = ?1",
            (id, title, slug, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Delete a single message row by id — used to roll back a persisted
    /// `gate_reply` whose channel delivery failed, so a retry can't
    /// duplicate it in the transcript. Tombstoned for sync.
    pub fn delete_message(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM messages WHERE id = ?1", [id])?;
        self.tombstone("messages", id)?;
        Ok(())
    }

    /// Delete a session and all its messages and swarm personas. Every
    /// removed row is tombstoned — append-only children and roster slots
    /// must not survive as orphaned durable state on this device.
    pub fn delete_session(&self, id: &str) -> Result<()> {
        let ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM messages WHERE session_id = ?1")?;
            let rows = stmt.query_map([id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let persona_ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT ord FROM swarm_personas WHERE session_id = ?1")?;
            let rows = stmt.query_map([id], |r| Ok(format!("{id}:{}", r.get::<_, i64>(0)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for mid in &ids {
            self.tombstone("messages", mid)?;
        }
        for persona_id in &persona_ids {
            self.tombstone("swarm_personas", persona_id)?;
        }
        self.conn
            .execute("DELETE FROM messages WHERE session_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM swarm_personas WHERE session_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM panel_states WHERE session_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        self.tombstone("sessions", id)?;
        Ok(())
    }

    /// IDs after the last user message or `!` command row, in load order; empty
    /// without one or a reply.
    pub fn reply_ids_after_last_user(&self, session_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM messages
             WHERE session_id = ?1 AND (created_at, rowid) > (
                 SELECT created_at, rowid FROM messages
                 WHERE session_id = ?1 AND role IN ('user', 'user_command')
                 ORDER BY created_at DESC, rowid DESC LIMIT 1
             ) ORDER BY created_at ASC, rowid ASC",
        )?;
        Ok(stmt
            .query_map([session_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content, model, reasoning, tokens, secs, cost, phrase, persona, created_at
             FROM messages WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let messages = stmt
            .query_map([session_id], |r| {
                Ok(Message {
                    role: r.get(0)?,
                    content: r.get(1)?,
                    model: r.get(2)?,
                    reasoning: r.get(3)?,
                    tokens: r.get(4)?,
                    secs: r.get(5)?,
                    cost: r.get(6)?,
                    phrase: r.get(7)?,
                    persona: r.get(8)?,
                    created_at: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(messages)
    }

    /// Recent user-typed shell commands across sessions, newest first and unique.
    pub fn recent_user_commands(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT json_extract(content, '$.cmd') AS cmd FROM messages
             WHERE role = 'user_command' AND json_valid(content)
               AND json_type(content, '$.cmd') = 'text'
             GROUP BY cmd ORDER BY MAX(created_at) DESC, MAX(rowid) DESC LIMIT 200",
        )?;
        Ok(stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Insert a user message (no model/reasoning/stats). Returns its id.
    pub fn add_user_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id, "user", content, None, None, None, None, None, None,
        )
    }

    /// A user's reply to a survey/approval gate: rendered in the transcript
    /// like a user message but never replayed to the model (`gate_reply`
    /// role) — the survey/plan rows it answers are excluded from model
    /// history too, so bare answers ("the second option", "drop Q2") must
    /// not reach the model without their context.
    pub fn add_gate_reply_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id,
            "gate_reply",
            content,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// Insert a tool-call transcript block: `content` is JSON containing the
    /// provider call id, optional reasoning/content, name, arguments, and
    /// result. It
    /// is never sent back to the model verbatim; `App::build_history` rebuilds
    /// the assistant/tool wire messages.
    pub fn add_tool_call_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id,
            "tool_call",
            content,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// Insert a failed-response line. It remains visible in the transcript
    /// after the status bar changes, but is never replayed to the model.
    pub fn add_error_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id, "error", content, None, None, None, None, None, None,
        )
    }

    /// Insert a skill invocation, replayed to the model as a user-role
    /// instruction at its position in the transcript. Returns its id.
    pub fn add_skill_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id, "skill", content, None, None, None, None, None, None,
        )
    }

    /// Insert a command the user ran with `!` (JSON: cmd, cwd, exit, output,
    /// interactive), replayed to the model as a user message. Returns its id.
    pub fn add_user_command_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id,
            "user_command",
            content,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// Insert a background-research stage/progress line: plain text, shown in
    /// the transcript but never sent back to the model (unlike `tool_call`
    /// rows, never replayed into `build_history` either — this is the job's
    /// own scratch work, not something the chat model did).
    pub fn add_research_stage_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id,
            "research_stage",
            content,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// A research pipeline's plan-approval prompt: rendered like a stage row
    /// but actionable, and (like `research_stage`) never replayed to the model.
    pub fn add_research_plan_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id,
            "research_plan",
            content,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// A research pipeline's clarifying-survey section: the scoping agent's
    /// questions awaiting a chat answer. Rendered like a stage row but
    /// actionable, and never replayed to the model.
    pub fn add_survey_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(
            session_id, "survey", content, None, None, None, None, None, None,
        )
    }

    /// Update the most recent `research_stage` row for `session_id` whose
    /// content starts with `label`, or insert one on the stage's first
    /// occurrence — keeps one transcript row per named stage instead of
    /// appending on every progress tick (e.g. every searcher finishing).
    pub fn upsert_research_stage_message(
        &self,
        session_id: &str,
        label: &str,
        detail: &str,
    ) -> Result<()> {
        let content = stage_content(label, detail);
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM messages WHERE session_id = ?1 AND role = 'research_stage'
                   AND (content = ?2 OR content LIKE ?3)
                 ORDER BY created_at DESC LIMIT 1",
                (session_id, label, format!("{label}:%")),
                |r| r.get(0),
            )
            .ok();
        match existing {
            Some(id) => {
                let now = Utc::now().to_rfc3339();
                self.conn.execute(
                    "UPDATE messages SET content = ?2, created_at = ?3 WHERE id = ?1",
                    (&id, &content, &now),
                )?;
            }
            None => {
                self.add_research_stage_message(session_id, &content)?;
            }
        }
        Ok(())
    }

    /// See the free function of the same name. Production code (the
    /// research pipeline task) writes through its own connection; this
    /// handle exists for tests.
    #[cfg(test)]
    pub fn add_session_sources(&self, session_id: &str, url_norms: &[String]) -> Result<()> {
        add_session_sources(&self.conn, session_id, url_norms)
    }

    /// See the free function of the same name.
    #[cfg(test)]
    pub fn search_session_sources(
        &self,
        session_id: &str,
        query: &str,
    ) -> Result<Vec<(String, String)>> {
        search_session_sources(&self.conn, session_id, query)
    }

    /// Pin (`Some("pinned")`), discard (`Some("discarded")`), or clear
    /// (`None`) a session source's flag. `url_norm` must already exist in
    /// `session_sources` for this session (a no-op UPDATE otherwise — the
    /// row is created by `add_session_sources` when a source is first
    /// cited, not here). Bumps the row's version — flag changes sync.
    pub fn set_source_flag(
        &self,
        session_id: &str,
        url_norm: &str,
        flag: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE session_sources SET flag = ?3, updated_at = ?4
             WHERE session_id = ?1 AND url_norm = ?2",
            (session_id, url_norm, flag, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Insert an assistant reply with its model, reasoning trace, and stats.
    /// `cost` is the provider-reported USD total or a cache-aware catalog
    /// estimate (`None` when neither is available).
    /// Args mirror the messages table columns; ~25 call sites pass inline
    /// `None`s for unused fields, so a struct would churn all of them.
    #[allow(clippy::too_many_arguments)]
    pub fn add_assistant_message(
        &self,
        session_id: &str,
        content: &str,
        model: Option<&str>,
        reasoning: Option<&str>,
        tokens: Option<i64>,
        secs: Option<f64>,
        cost: Option<f64>,
        phrase: Option<&str>,
    ) -> Result<String> {
        self.insert_message(
            session_id,
            "assistant",
            content,
            model,
            reasoning,
            tokens,
            secs,
            cost,
            phrase,
        )
    }

    /// Replace a repaired reply only while it is still the session's latest
    /// user/assistant message. Bookkeeping and errors are not another turn.
    pub(crate) fn update_ui_reply(
        &self,
        session_id: &str,
        message_id: &str,
        original: &str,
        content: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE messages SET content = ?4
             WHERE session_id = ?1 AND id = ?2 AND role = 'assistant' AND content = ?3
             AND id = (SELECT id FROM messages WHERE session_id = ?1
                       AND role IN ('user', 'user_command', 'assistant')
                       ORDER BY created_at DESC, rowid DESC LIMIT 1)",
            (session_id, message_id, original, content),
        )? == 1)
    }

    /// Insert a `/swarm` persona's round reply: an assistant message tagged
    /// with which persona (and its own model) produced it.
    pub fn add_persona_message(
        &self,
        session_id: &str,
        content: &str,
        persona_name: &str,
        model: &str,
    ) -> Result<String> {
        let id = self.insert_message(
            session_id,
            "assistant",
            content,
            Some(model),
            None,
            None,
            None,
            None,
            None,
        )?;
        self.conn.execute(
            "UPDATE messages SET persona = ?2 WHERE id = ?1",
            (&id, persona_name),
        )?;
        Ok(id)
    }

    /// Shared message-row insert; kept flat for the same reason as
    /// `add_assistant_message` — column-shaped params, many inline callers.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        model: Option<&str>,
        reasoning: Option<&str>,
        tokens: Option<i64>,
        secs: Option<f64>,
        cost: Option<f64>,
        phrase: Option<&str>,
    ) -> Result<String> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO messages
                (id, session_id, role, content, model, reasoning, tokens, secs, cost, phrase, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            (
                &id, session_id, role, content, model, reasoning, tokens, secs, cost, phrase, &now,
            ),
        )?;
        self.conn.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            (session_id, &now),
        )?;
        Ok(id)
    }

    /// `created_at` of the message at `index` (0-based, transcript order) —
    /// used to anchor a compaction row at the boundary without loading the
    /// whole session (e.g. when the job finishes after the user switched
    /// sessions). `None` when the session has fewer than `index + 1` messages.
    pub fn message_created_at(&self, session_id: &str, index: usize) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT created_at FROM messages WHERE session_id = ?1
             ORDER BY created_at ASC, rowid ASC LIMIT 1 OFFSET ?2",
        )?;
        Ok(stmt
            .query_row((session_id, index as i64), |r| r.get(0))
            .optional()?)
    }

    /// Insert a compaction-digest row at the exact `created_at` position —
    /// the timestamp of the last message the digest covers, so reloads keep
    /// the digest at the compaction boundary (right after the raw messages
    /// it summarizes) instead of at the end of the transcript. Unlike
    /// `insert_message`, this does not bump the session's `updated_at`:
    /// compacting is bookkeeping, not new activity.
    pub fn add_compaction_message(
        &self,
        session_id: &str,
        content: &str,
        at: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO messages
                (id, session_id, role, content, model, reasoning, tokens, secs, phrase, created_at)
             VALUES (?1, ?2, 'compaction', ?3, NULL, NULL, NULL, NULL, NULL, ?4)",
            (&id, session_id, content, at),
        )?;
        Ok(id)
    }

    /// Replace the session's compaction row's content in place — a later
    /// compaction folds new messages into the same digest, so there is
    /// exactly one row per session. Returns the number of rows updated
    /// (0 = the session has no compaction row yet).
    pub fn update_compaction_message(&self, session_id: &str, content: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE messages SET content = ?2
             WHERE session_id = ?1 AND role = 'compaction'",
            (session_id, content),
        )?)
    }

    pub fn set_session_model(&self, session_id: &str, model: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET model = ?2, updated_at = ?3 WHERE id = ?1",
            (session_id, model, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }
}
