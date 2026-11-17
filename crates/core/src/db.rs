// Casts here are on bounded values (token counts, byte sizes, selection
// indices); JSON-derived indices go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::Connection;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::provider::ModelPricing;

mod cache_accounting;
pub(crate) mod chats;
mod files;
mod jobs;
pub(crate) mod migrations;
mod sessions;
mod subagents;
mod sync_state;
mod usage;

pub use cache_accounting::{Classified, LaneState, RequestFacts, Verdict, classify};
use files::cosine;
pub use files::{
    add_session_sources, blob_to_vec, cache_get, cache_put, count_files, discarded_domains,
    file_status, file_text, files_missing_embeddings, fts_quote, is_fresh, pinned_urls,
    search_chunks, search_citations, search_session_sources, semantic_chunks,
    semantic_chunks_for_identity, stage_content, vec_to_blob,
};
use migrations::has_column;
pub use migrations::migrate_cache;
pub use sync_state::SyncState;
pub(crate) use usage::UsageLog;
pub use usage::{
    SessionUsageRow, UsageByBackend, UsageByModel, UsageDay, UsageRange, UsageRow, UsageTotals,
    price_name,
};

#[derive(Debug, Clone)]
pub struct ModelPref {
    pub id: String,
    pub favorite: bool,
    pub last_used: Option<String>,
    pub reasoning: Option<String>,
}

/// An isolated collection of sessions with its own memory and instructions;
/// `name` is also its directory name.
#[derive(Debug, Clone)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub created_at: String,
}

/// One chat session within a space.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub model: String,
    /// Short model-generated slug; display falls back to a uuid prefix until set.
    pub slug: Option<String>,
    pub created_at: String,
    /// Compressed digest of earlier messages, set by auto-compaction.
    pub compact_summary: Option<String>,
    /// Number of raw messages folded into `compact_summary`; later messages are
    /// still sent verbatim.
    pub compact_through: i64,
    /// `/swarm` mode: each turn is independent drafts, a review, then one reply.
    pub swarm_mode: bool,
    /// `"chat"` or `"research"`.
    pub kind: String,
    /// Original session id when this research session was spawned from a chat.
    pub research_parent_id: Option<String>,
}

/// Tool-call totals and frequent failure modes in a time window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessSummary {
    pub calls: u64,
    pub failures: u64,
    pub rows: Vec<FailureRow>,
}

/// One failure mode and the latest transcript where it occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRow {
    pub tool: String,
    pub action: String,
    pub error_kind: String,
    pub count: u64,
    pub last_at: String,
    pub last_session_id: String,
}

/// One row of a session's `/swarm` roster: a model + a personality blurb.
#[derive(Debug, Clone)]
pub struct Persona {
    pub name: String,
    pub model: String,
    pub blurb: String,
}

/// An imported file; `status` describes extraction and `status`/`mtime` are
/// this device's derived index state.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: String,
    pub name: String,
    pub hash: String,
    pub size: i64,
    pub status: String,
    /// Unix mtime when last indexed; rescans skip files whose size and mtime are
    /// unchanged.
    pub mtime: i64,
    /// `extract::EXTRACTOR_VERSION` that produced the index; 0 means unversioned.
    pub extractor: i64,
}

/// One message; assistant-only fields are `None` for user and system messages.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub tokens: Option<i64>,
    pub secs: Option<f64>,
    /// USD cost of the reply: provider-reported, else a catalog estimate.
    pub cost: Option<f64>,
    /// Past-tense flavour phrase for the completion line, e.g. "Vibed".
    pub phrase: Option<String>,
    /// `/swarm` persona that produced this reply, if any.
    pub persona: Option<String>,
    /// RFC3339 timestamp; `None` for in-memory-only messages.
    pub created_at: Option<String>,
}

/// Name of the always-present space that sessions default into.
pub const DEFAULT_SPACE: &str = "default";

/// Path to the disposable device-local cache db (`cache.db`) beside the durable db.
pub fn cache_path_for(db_path: &std::path::Path) -> std::path::PathBuf {
    db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(
            || std::path::PathBuf::from("cache.db"),
            |p| p.join("cache.db"),
        )
}

/// Open a durable db with its sibling `cache.db` attached as schema `cache` for
/// cross-db queries.
pub fn open_attached(db_path: &std::path::Path) -> Result<Connection> {
    let conn = open_conn(db_path)?;
    let cache = cache_path_for(db_path);
    let escaped = cache.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!("ATTACH DATABASE '{escaped}' AS cache"))
        .with_context(|| format!("attaching cache db {}", cache.display()))?;
    // Journal mode is per-database, so the attached cache needs its own switch.
    let _ = conn.pragma_update(Some("cache"), "journal_mode", "WAL");
    migrate_cache(&conn, "cache")?;
    chats::install_cleanup(&conn)?;
    Ok(conn)
}

/// Open one connection with the pragmas every dugix connection needs: WAL keeps
/// concurrent readers unblocked and the busy timeout makes a writer wait.
pub fn open_conn(db_path: &std::path::Path) -> Result<Connection> {
    let conn =
        Connection::open(db_path).with_context(|| format!("opening db {}", db_path.display()))?;
    apply_pragmas(&conn);
    Ok(conn)
}

/// Best-effort pragmas; a read-only filesystem may refuse the journal switch,
/// which only slows the db.
fn apply_pragmas(conn: &Connection) {
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
}

/// One device-local policy decision, including the user's eventual answer.
#[derive(Debug)]
pub struct DecisionRow {
    pub id: i64,
    pub tool: String,
    pub action: String,
    pub class: String,
    pub summary: String,
    pub by: String,
    pub verdict: String,
    pub p_in_scope: Option<f64>,
    pub risk: Option<String>,
    pub user_answer: Option<String>,
    /// Total wave cost on the first row only; other rows and unknown costs are NULL.
    pub cost: Option<f64>,
}

/// Record a scrubbed policy decision on an attached cache connection.
pub fn insert_decision(
    conn: &Connection,
    session_id: &str,
    req: &crate::decider::DecisionRequest,
    answer: &crate::decider::Verdicts,
    scrubber: &crate::trace::Scrubber,
) -> Result<i64> {
    let verdict = match answer.verdict {
        crate::decider::Verdict::Allow => "allow",
        crate::decider::Verdict::Deny(_) => "deny",
        crate::decider::Verdict::Ask => "ask",
    };
    let summary: String = scrubber.scrub(&req.summary).chars().take(2_000).collect();
    conn.execute(
        "INSERT INTO cache.decisions
         (at, session_id, tool, action, class, summary, by, verdict, p_in_scope, risk)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            Utc::now().to_rfc3339(),
            session_id,
            req.tool,
            req.action,
            req.class.as_str(),
            summary,
            answer.by,
            verdict,
            answer.p_in_scope,
            answer.risk,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Record a wave's total cost on its first decision row; other rows stay NULL.
pub fn set_decision_cost(conn: &Connection, id: i64, cost: f64) -> Result<()> {
    conn.execute(
        "UPDATE cache.decisions SET cost = ?2 WHERE id = ?1",
        (id, cost),
    )?;
    Ok(())
}

/// Fill in the user label after a single or combined approval prompt.
pub fn answer_decision(conn: &Connection, id: i64, answer: &str) -> Result<()> {
    conn.execute(
        "UPDATE cache.decisions SET user_answer = ?2 WHERE id = ?1",
        (id, answer),
    )?;
    Ok(())
}

// rusqlite is synchronous and called inline on the UI task; writes are tiny
// local inserts, so no spawn_blocking.
pub struct Db {
    conn: Connection,
    upgrade_notes: Vec<String>,
}

impl Db {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = open_attached(path)?;
        let upgrade_notes = migrations::migrate_durable(&conn)?;
        let mut db = Self {
            conn,
            upgrade_notes,
        };
        db.ensure_invariants()?;
        chats::install_cleanup(&db.conn)?;
        Ok(db)
    }

    #[doc(hidden)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        apply_pragmas(&conn);
        conn.execute_batch("ATTACH DATABASE ':memory:' AS cache")?;
        migrate_cache(&conn, "cache")?;
        let upgrade_notes = migrations::migrate_durable(&conn)?;
        let mut db = Db {
            conn,
            upgrade_notes,
        };
        db.ensure_invariants()?;
        chats::install_cleanup(&db.conn)?;
        Ok(db)
    }

    /// Drain user-facing notes produced by database upgrades.
    pub fn take_upgrade_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.upgrade_notes)
    }

    fn ensure_invariants(&mut self) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO spaces (id, name, created_at) VALUES (?1, ?2, ?3)",
            (DEFAULT_SPACE, DEFAULT_SPACE, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// The underlying connection, for the sync engine's per-table queries.
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    #[cfg(test)]
    pub fn conn_for_test(&self) -> &Connection {
        &self.conn
    }

    /// Bump a versioned row's `updated_at`, the LWW version the sync engine
    /// compares.
    fn touch(&self, table: &str, id: &str) -> Result<()> {
        self.conn.execute(
            &format!("UPDATE {table} SET updated_at = ?1 WHERE id = ?2"),
            (Utc::now().to_rfc3339(), id),
        )?;
        Ok(())
    }

    /// Record a physically deleted syncable row; `row_id` is its sync identity.
    fn tombstone(&self, table: &str, row_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_tombstones (table_name, row_id, deleted_at)
             VALUES (?1, ?2, ?3)",
            (table, row_id, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// The underlying connection, for tests that exercise the free query functions.
    #[cfg(test)]
    pub fn raw(&self) -> &Connection {
        &self.conn
    }

    /// `PRAGMA integrity_check`: `"ok"` when the file is sound, else a list of
    /// problems.
    pub fn integrity_check(&self) -> Result<String> {
        self.conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .context("running integrity check")
    }
}

#[cfg(test)]
mod tests;
