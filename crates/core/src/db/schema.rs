//! Schema creation and migrations of the durable and cache dbs.

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use super::{DEFAULT_SPACE, Db, chats};

/// Schema version of the durable db, tracked via `PRAGMA user_version`.
/// Legacy dbs (never versioned) are 0 and get one-time column adds plus the
/// device-local table move into `cache.db`; fresh dbs are created complete
/// and stamped 2 immediately. v2: the default space's sync identity becomes
/// deterministic (id `default`) so two devices' default spaces merge as one
/// LWW row instead of colliding by name. v3: the dead legacy `files.status`
/// column is dropped — it is `NOT NULL` with no default, so it rejected every
/// new `files` row once status moved to `cache.file_index_state`.
pub(super) const SCHEMA_VERSION: i64 = 3;

/// Columns added since the v1 schema. Fresh dbs declare them inline; legacy
/// dbs get them via `user_version`-gated `ALTER TABLE` adds guarded by
/// `PRAGMA table_info` (the only tolerated "duplicate" is an existing
/// column — real errors propagate). `files.mtime` is deliberately absent:
/// it moved to `cache.file_index_state` and stays a dead column on legacy
/// dbs.
const LEGACY_COLUMN_ADDS: &[(&str, &str, &str)] = &[
    (
        "messages",
        "model",
        "ALTER TABLE messages ADD COLUMN model TEXT",
    ),
    (
        "messages",
        "reasoning",
        "ALTER TABLE messages ADD COLUMN reasoning TEXT",
    ),
    (
        "messages",
        "tokens",
        "ALTER TABLE messages ADD COLUMN tokens INTEGER",
    ),
    (
        "messages",
        "secs",
        "ALTER TABLE messages ADD COLUMN secs REAL",
    ),
    (
        "messages",
        "cost",
        "ALTER TABLE messages ADD COLUMN cost REAL",
    ),
    (
        "messages",
        "phrase",
        "ALTER TABLE messages ADD COLUMN phrase TEXT",
    ),
    (
        "messages",
        "persona",
        "ALTER TABLE messages ADD COLUMN persona TEXT",
    ),
    (
        "model_prefs",
        "reasoning",
        "ALTER TABLE model_prefs ADD COLUMN reasoning TEXT",
    ),
    (
        "model_prefs",
        "updated_at",
        "ALTER TABLE model_prefs ADD COLUMN updated_at TEXT",
    ),
    (
        "sessions",
        "slug",
        "ALTER TABLE sessions ADD COLUMN slug TEXT",
    ),
    (
        "sessions",
        "space_id",
        "ALTER TABLE sessions ADD COLUMN space_id TEXT",
    ),
    (
        "sessions",
        "compact_summary",
        "ALTER TABLE sessions ADD COLUMN compact_summary TEXT",
    ),
    (
        "sessions",
        "compact_through",
        "ALTER TABLE sessions ADD COLUMN compact_through INTEGER NOT NULL DEFAULT 0",
    ),
    // Retired `/web` flag. Nothing reads it; the column stays so sync rows
    // from older peers (which require it) still apply both ways.
    (
        "sessions",
        "web_mode",
        "ALTER TABLE sessions ADD COLUMN web_mode INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "sessions",
        "swarm_mode",
        "ALTER TABLE sessions ADD COLUMN swarm_mode INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "sessions",
        "kind",
        "ALTER TABLE sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'chat'",
    ),
    (
        "sessions",
        "research_parent_id",
        "ALTER TABLE sessions ADD COLUMN research_parent_id TEXT",
    ),
    (
        "session_sources",
        "flag",
        "ALTER TABLE session_sources ADD COLUMN flag TEXT",
    ),
    (
        "session_sources",
        "updated_at",
        "ALTER TABLE session_sources ADD COLUMN updated_at TEXT",
    ),
    (
        "usage_log",
        "cost_is_provider",
        "ALTER TABLE usage_log ADD COLUMN cost_is_provider INTEGER",
    ),
    (
        "usage_log",
        "sync_id",
        "ALTER TABLE usage_log ADD COLUMN sync_id TEXT",
    ),
    (
        "usage_log",
        "updated_at",
        "ALTER TABLE usage_log ADD COLUMN updated_at TEXT",
    ),
    (
        "app_settings",
        "scope",
        "ALTER TABLE app_settings ADD COLUMN scope TEXT NOT NULL DEFAULT 'sync'",
    ),
    (
        "app_settings",
        "updated_at",
        "ALTER TABLE app_settings ADD COLUMN updated_at TEXT",
    ),
    (
        "spaces",
        "updated_at",
        "ALTER TABLE spaces ADD COLUMN updated_at TEXT",
    ),
    (
        "files",
        "updated_at",
        "ALTER TABLE files ADD COLUMN updated_at TEXT",
    ),
    (
        "citations",
        "sync_id",
        "ALTER TABLE citations ADD COLUMN sync_id TEXT",
    ),
    (
        "watches",
        "updated_at",
        "ALTER TABLE watches ADD COLUMN updated_at TEXT",
    ),
    // NULL on pre-normalization rows: they were logged when the prompt-token
    // convention was ambiguous, so their cache ratio cannot be reconstructed.
    // Token and cost totals stay usable; only the ratio is withheld.
    (
        "usage_log",
        "prompt_convention",
        "ALTER TABLE usage_log ADD COLUMN prompt_convention TEXT",
    ),
];

/// Create the device-local cache schema on `conn` under the given schema
/// name — `cache` on a main-db connection from `open_attached`, or `main`
/// on a standalone cache-only connection.
#[allow(clippy::too_many_lines)]
pub fn migrate_cache(conn: &Connection, schema: &str) -> Result<()> {
    chats::migrate(conn, schema)?;
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {schema}.web_cache (
            url_norm   TEXT PRIMARY KEY,
            url        TEXT NOT NULL,
            title      TEXT,
            text       TEXT NOT NULL,
            fetched_at TEXT NOT NULL
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS {schema}.file_chunks USING fts5(
            file_id UNINDEXED,
            seq UNINDEXED,
            location UNINDEXED,
            text
        );
        CREATE TABLE IF NOT EXISTS {schema}.chunk_embeddings (
            file_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            vec BLOB NOT NULL,
            PRIMARY KEY (file_id, seq)
        );
        CREATE TABLE IF NOT EXISTS {schema}.model_prices (
            model_id TEXT PRIMARY KEY,
            backend TEXT NOT NULL,
            prompt_price REAL NOT NULL,
            completion_price REAL NOT NULL,
            cache_read_price REAL,
            cache_write_price REAL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS {schema}.file_index_state (
            file_id TEXT PRIMARY KEY,
            mtime INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL,
            extractor INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS {schema}.session_workspaces (
            session_id TEXT PRIMARY KEY,
            root TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS {schema}.session_masks (
            session_id TEXT PRIMARY KEY,
            mask_through INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS {schema}.tool_events (
            id INTEGER PRIMARY KEY,
            at TEXT NOT NULL,
            session_id TEXT NOT NULL,
            tool TEXT NOT NULL,
            action TEXT NOT NULL,
            ok INTEGER NOT NULL,
            error_kind TEXT,
            latency_ms INTEGER NOT NULL,
            result_chars INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS {schema}.tool_events_at ON tool_events(at);
        CREATE TABLE IF NOT EXISTS {schema}.subagent_runs (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            number INTEGER NOT NULL,
            prompt TEXT NOT NULL,
            model TEXT NOT NULL,
            writable INTEGER NOT NULL,
            max_steps INTEGER NOT NULL,
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            steps INTEGER NOT NULL,
            tokens INTEGER NOT NULL,
            cost REAL,
            secs REAL NOT NULL,
            review TEXT,
            rows TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS {schema}.subagent_runs_session
            ON subagent_runs(session_id, started_at);
        CREATE TABLE IF NOT EXISTS {schema}.watch_jobs (
            watch_id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS {schema}.decisions (
            id INTEGER PRIMARY KEY,
            at TEXT,
            session_id TEXT,
            tool TEXT,
            action TEXT,
            class TEXT,
            summary TEXT,
            by TEXT,
            verdict TEXT,
            p_in_scope REAL,
            risk TEXT,
            user_answer TEXT,
            cost REAL
        );",
    ))?;
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info(chunk_embeddings)"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for column in ["identity", "location", "text"] {
        if !columns.iter().any(|existing| existing == column) {
            conn.execute_batch(&format!(
                "ALTER TABLE {schema}.chunk_embeddings ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"
            ))?;
        }
    }
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info(session_workspaces)"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "mode") {
        conn.execute_batch(&format!(
            "ALTER TABLE {schema}.session_workspaces ADD COLUMN mode TEXT"
        ))?;
    }
    // Which extractor version indexed each file: older ones are re-extracted
    // once (see `extract::EXTRACTOR_VERSION`).
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info(file_index_state)"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "extractor") {
        conn.execute_batch(&format!(
            "ALTER TABLE {schema}.file_index_state ADD COLUMN extractor INTEGER NOT NULL DEFAULT 0"
        ))?;
    }
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info(decisions)"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "cost") {
        conn.execute_batch(&format!(
            "ALTER TABLE {schema}.decisions ADD COLUMN cost REAL"
        ))?;
    }
    Ok(())
}

/// Whether `table` in the **main** schema has `column` — the guard for
/// legacy column adds. Explicitly main-scoped: `PRAGMA table_info` would
/// otherwise resolve names across the attached `cache` schema too.
pub(super) fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA main.table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Db {
    // Long by design (schema migrations).
    #[allow(clippy::too_many_lines)]
    pub(super) fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                model TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id, created_at);
            CREATE TABLE IF NOT EXISTS model_prefs (
                id TEXT PRIMARY KEY,
                favorite INTEGER NOT NULL DEFAULT 0,
                last_used TEXT,
                reasoning TEXT,
                updated_at TEXT
            );
            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                scope TEXT NOT NULL DEFAULT 'sync',
                updated_at TEXT
            );
            CREATE TABLE IF NOT EXISTS spaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL,
                updated_at TEXT
            );
            CREATE TABLE IF NOT EXISTS files (
                id TEXT PRIMARY KEY,
                space_id TEXT NOT NULL,
                name TEXT NOT NULL,
                hash TEXT NOT NULL,
                size INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(space_id, name)
            );
            CREATE TABLE IF NOT EXISTS citations (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                sync_id     TEXT NOT NULL,
                space_id    TEXT NOT NULL,
                report_file TEXT NOT NULL,
                url         TEXT NOT NULL,
                title       TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_citations_space ON citations(space_id);
            CREATE TABLE IF NOT EXISTS session_sources (
                session_id TEXT NOT NULL,
                url_norm   TEXT NOT NULL,
                flag TEXT,
                updated_at TEXT,
                PRIMARY KEY (session_id, url_norm)
            );
            CREATE TABLE IF NOT EXISTS watches (
                id             TEXT PRIMARY KEY,
                space_id       TEXT NOT NULL,
                topic          TEXT NOT NULL,
                interval_hours INTEGER NOT NULL,
                session_id     TEXT NOT NULL,
                last_run_at    TEXT,
                updated_at     TEXT
            );
            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY, space_id TEXT NOT NULL, session_id TEXT NOT NULL,
                device_id TEXT NOT NULL, name TEXT NOT NULL, prompt TEXT NOT NULL,
                root TEXT NOT NULL, mode TEXT NOT NULL, trigger_kind TEXT NOT NULL,
                interval_minutes INTEGER NOT NULL, run_at TEXT, stop_command TEXT,
                max_iters INTEGER NOT NULL, max_cost_usd REAL NOT NULL,
                max_wall_minutes INTEGER NOT NULL, notify TEXT NOT NULL,
                enabled INTEGER NOT NULL, last_run_at TEXT, updated_at TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'agent', topic TEXT
            );
            CREATE TABLE IF NOT EXISTS job_runs (
                id TEXT PRIMARY KEY, job_id TEXT NOT NULL, device_id TEXT NOT NULL,
                started_at TEXT NOT NULL, finished_at TEXT, status TEXT NOT NULL,
                iters INTEGER NOT NULL, cost_usd REAL NOT NULL, branch TEXT,
                summary TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS swarm_personas (
                session_id TEXT NOT NULL,
                ord        INTEGER NOT NULL,
                name       TEXT NOT NULL,
                model      TEXT NOT NULL,
                persona    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_swarm_personas_session
                ON swarm_personas(session_id, ord);
            CREATE TABLE IF NOT EXISTS usage_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sync_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                session_id TEXT,
                space_id TEXT,
                backend TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL,
                completion_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                prompt_convention TEXT,
                cost REAL,
                cost_is_provider INTEGER,
                updated_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_usage_log_created ON usage_log(created_at);
            CREATE INDEX IF NOT EXISTS idx_usage_log_model ON usage_log(model);
            CREATE INDEX IF NOT EXISTS idx_usage_log_session ON usage_log(session_id, created_at);
            CREATE TABLE IF NOT EXISTS sync_tombstones (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                table_name TEXT NOT NULL,
                row_id TEXT NOT NULL,
                deleted_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sync_tombstones_table ON sync_tombstones(table_name, row_id);
            CREATE TABLE IF NOT EXISTS device_meta (
                device_id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sync_state (
                peer_id TEXT NOT NULL,
                table_name TEXT NOT NULL,
                pull_cursor TEXT,
                push_cursor TEXT,
                last_synced_at TEXT,
                PRIMARY KEY (peer_id, table_name)
            );",
        )?;
        crate::panels::migrate(&self.conn)?;
        crate::memory::migrate(&self.conn)?;
        for (column, ddl) in [
            (
                "kind",
                "ALTER TABLE jobs ADD COLUMN kind TEXT NOT NULL DEFAULT 'agent'",
            ),
            ("topic", "ALTER TABLE jobs ADD COLUMN topic TEXT"),
        ] {
            if !has_column(&self.conn, "jobs", column)? {
                self.conn.execute(ddl, [])?;
            }
        }
        // Added after SCHEMA_VERSION 3, so dbs already at that version need
        // them too: the gated LEGACY_COLUMN_ADDS below never run for them.
        for (column, ddl) in [
            ("kind", "ALTER TABLE usage_log ADD COLUMN kind TEXT"),
            ("tools", "ALTER TABLE usage_log ADD COLUMN tools TEXT"),
        ] {
            if !has_column(&self.conn, "usage_log", column)? {
                self.conn.execute(ddl, [])?;
            }
        }
        // user_version-gated migrations. Legacy dbs (version 0) get the
        // column adds they may still lack — guarded by `PRAGMA table_info`,
        // the only tolerated "duplicate" — plus a one-time move of the
        // device-local tables into cache.db. Real errors propagate; nothing
        // is swallowed.
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            for (table, column, ddl) in LEGACY_COLUMN_ADDS {
                if !has_column(&self.conn, table, column)? {
                    self.conn.execute(ddl, []).with_context(|| {
                        format!("migrating column {table}.{column} (user_version {version})")
                    })?;
                }
            }
            // Device-local ids for rows whose AUTOINCREMENT ids can't be
            // sync identity. One transaction: a large usage_log must not
            // pay a per-row fsync (minutes on a file db). Idempotent — rows
            // already stamped are skipped by the IS NULL filter.
            let backfill_tx = self.conn.transaction()?;
            for table in ["citations", "usage_log"] {
                let ids: Vec<i64> = {
                    let mut stmt = backfill_tx
                        .prepare(&format!("SELECT id FROM {table} WHERE sync_id IS NULL"))?;
                    let rows = stmt.query_map([], |r| r.get(0))?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                };
                let mut update = backfill_tx
                    .prepare(&format!("UPDATE {table} SET sync_id = ?1 WHERE id = ?2"))?;
                for id in ids {
                    update.execute((Uuid::new_v4().to_string(), id))?;
                }
            }
            backfill_tx.commit()?;
            // Unique indexes on the backfilled ids (fresh dbs already have
            // the columns inline; the indexes must wait until legacy dbs
            // have theirs).
            self.conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_citations_sync_id ON citations(sync_id);
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_log_sync_id ON usage_log(sync_id);",
            )?;
            // One-time move of the device-local tables into cache.db (they
            // were the same device's local store before the split, so the
            // copy preserves behavior exactly). Each copy is guarded by
            // table existence — fresh dbs have nothing to move.
            let now = Utc::now().to_rfc3339();
            if has_column(&self.conn, "files", "mtime")?
                && has_column(&self.conn, "files", "status")?
            {
                self.conn.execute(
                    "INSERT OR IGNORE INTO cache.file_index_state (file_id, mtime, status, updated_at)
                     SELECT id, mtime, status, ?1 FROM files",
                    [&now],
                )?;
            }
            // Once seeded into cache, the legacy status column is dead — and
            // harmful: it is NOT NULL with no default, so inserts that omit
            // it fail. `files.mtime` stays (it has a default).
            if has_column(&self.conn, "files", "status")? {
                self.conn
                    .execute("ALTER TABLE files DROP COLUMN status", [])
                    .context("dropping legacy files.status")?;
            }
            if has_column(&self.conn, "web_cache", "url_norm")? {
                self.conn.execute(
                    "INSERT OR IGNORE INTO cache.web_cache (url_norm, url, title, text, fetched_at)
                     SELECT url_norm, url, title, text, fetched_at FROM web_cache",
                    [],
                )?;
            }
            if has_column(&self.conn, "chunk_embeddings", "file_id")? {
                self.conn.execute(
                    "INSERT OR IGNORE INTO cache.chunk_embeddings (file_id, seq, vec)
                     SELECT file_id, seq, vec FROM chunk_embeddings",
                    [],
                )?;
            }
            if has_column(&self.conn, "file_chunks", "file_id")? {
                self.conn.execute(
                    "INSERT OR IGNORE INTO cache.file_chunks (file_id, seq, location, text)
                     SELECT file_id, seq, location, text FROM file_chunks",
                    [],
                )?;
            }
            // model_prices may predate its cache-rate columns; copy with the
            // widest shape the legacy table actually has.
            if has_column(&self.conn, "model_prices", "model_id")? {
                let cols = if has_column(&self.conn, "model_prices", "cache_read_price")? {
                    "model_id, backend, prompt_price, completion_price,\n                        cache_read_price, cache_write_price, updated_at"
                } else {
                    "model_id, backend, prompt_price, completion_price, updated_at"
                };
                self.conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO cache.model_prices ({cols}) SELECT {cols} FROM model_prices"
                    ),
                    [],
                )?;
            }
            // Sync identity for the default space: fresh dbs insert it with
            // the deterministic id `default` (see below), so two devices'
            // default spaces are the *same* sync row and merge via LWW
            // instead of colliding by name. Legacy dbs carry a random uuid
            // there — renumber it once, moving every space_id reference
            // (and any tombstone) with it.
            let old_default: Option<String> = self
                .conn
                .query_row(
                    "SELECT id FROM spaces WHERE name = ?1",
                    [DEFAULT_SPACE],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(old) = old_default.filter(|id| id != DEFAULT_SPACE) {
                let tx = self.conn.transaction()?;
                for table in ["sessions", "files", "watches", "usage_log", "citations"] {
                    tx.execute(
                        &format!("UPDATE {table} SET space_id = ?1 WHERE space_id = ?2"),
                        (DEFAULT_SPACE, &old),
                    )?;
                }
                tx.execute(
                    "UPDATE sync_tombstones SET row_id = ?1
                     WHERE table_name = 'spaces' AND row_id = ?2",
                    (DEFAULT_SPACE, &old),
                )?;
                tx.execute(
                    "UPDATE spaces SET id = ?1 WHERE id = ?2",
                    (DEFAULT_SPACE, &old),
                )?;
                tx.commit()?;
            }
            self.conn
                .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        }
        // Migration: remove the message_images table — images are now embedded
        // as markdown `![alt](file)` in message content.
        let _ = self
            .conn
            .execute_batch("DROP TABLE IF EXISTS message_images;");
        // Ensure the default space exists, then backfill any session left
        // without a space (pre-spaces db, or a space that got deleted). The
        // default space's id is the deterministic string `default` (not a
        // uuid) — it is the same sync row on every device, so the sync LWW
        // merge treats it as one row instead of a name collision.
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO spaces (id, name, created_at) VALUES (?1, ?2, ?3)",
            (DEFAULT_SPACE, DEFAULT_SPACE, &now),
        )?;
        let default_id: String = self.conn.query_row(
            "SELECT id FROM spaces WHERE name = ?1",
            [DEFAULT_SPACE],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "UPDATE sessions SET space_id = ?1 WHERE space_id IS NULL",
            [&default_id],
        )?;
        self.migrate_watch_jobs()?;
        Ok(())
    }
}
