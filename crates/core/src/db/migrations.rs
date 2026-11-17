//! Ordered upgrades for the durable database and its disposable cache.
//! Append new steps; never change a shipped step.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, Transaction, TransactionBehavior};

const DURABLE_SQL: &str = include_str!("migrations/durable_v1.sql");
const CACHE_SQL: &str = include_str!("migrations/cache_v1.sql");

struct Step {
    target: i64,
    name: &'static str,
    run: fn(&Transaction<'_>, &mut Ctx<'_>) -> Result<()>,
}

struct Ctx<'a> {
    schema: &'a str,
    file: &'a str,
    root: Option<&'a Path>,
    notes: Vec<String>,
}

const DURABLE: &[Step] = &[
    Step {
        target: 4,
        name: "durable v1 baseline",
        run: durable_v1,
    },
    Step {
        target: 5,
        name: "drop columns kept for older peers",
        run: drop_older_peer_columns,
    },
    Step {
        target: 6,
        name: "research jobs replace watches",
        run: research_jobs_replace_watches,
    },
    Step {
        target: 7,
        name: "share memory chapters between spaces",
        run: share_memory_chapters,
    },
];
const CACHE: &[Step] = &[
    Step {
        target: 1,
        name: "cache v1 baseline",
        run: cache_v1,
    },
    Step {
        target: 2,
        name: "drop watch_jobs",
        run: drop_watch_jobs,
    },
    Step {
        target: 3,
        name: "session prompt prefixes and request cache accounting",
        run: prompt_cache,
    },
];

fn prompt_cache(tx: &Transaction<'_>, ctx: &mut Ctx<'_>) -> Result<()> {
    let schema = ctx.schema;
    tx.execute_batch(&format!(
        "CREATE TABLE {schema}.session_prefixes (
            session_id TEXT PRIMARY KEY, generation INTEGER NOT NULL,
            system TEXT NOT NULL, lead_images_json TEXT, tools_json TEXT NOT NULL,
            made_at TEXT NOT NULL, identity_hash TEXT NOT NULL
        );
        CREATE TABLE {schema}.request_cache (
            usage_id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, cache_key TEXT NOT NULL,
            prefix_generation INTEGER NOT NULL, reusable_tokens INTEGER NOT NULL,
            system_hash TEXT NOT NULL, tools_hash TEXT NOT NULL,
            gap_secs INTEGER NOT NULL, verdict TEXT NOT NULL
        );
        CREATE INDEX {schema}.request_cache_lane ON request_cache (session_id, cache_key, usage_id);"
    ))?;
    Ok(())
}

/// The current version of the durable database schema.
#[must_use]
pub fn durable_schema_version() -> i64 {
    DURABLE.last().map_or(0, |step| step.target)
}

fn version(conn: &Connection, schema: &str) -> Result<i64> {
    Ok(
        conn.query_row(&format!("PRAGMA {schema}.user_version"), [], |row| {
            row.get(0)
        })?,
    )
}

fn validate_version(conn: &Connection, ctx: &Ctx<'_>, v: i64, latest: i64) -> Result<()> {
    if v > latest {
        bail!(
            "{} was written by a newer dugix (schema {v}, this build knows up to {latest}); update dugix",
            ctx.file
        );
    }
    if ctx.file == "dugix.db"
        && (v == 1
            || v == 2
            || (v == 0
                && conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name = 'sessions' AND type = 'table')",
                    [],
                    |row| row.get::<_, bool>(0),
                )?))
    {
        bail!(
            "dugix.db is from a version older than 0.1.42 (schema {v}); install nexus-chat 0.1.42 once to upgrade it, then this version"
        );
    }
    Ok(())
}

/// Return the number of steps committed by this connection.
fn run(conn: &Connection, steps: &[Step], ctx: &mut Ctx<'_>) -> Result<usize> {
    let latest = steps.last().map_or(0, |step| step.target);
    let mut current = version(conn, ctx.schema)?;
    validate_version(conn, ctx, current, latest)?;
    let mut ran = 0;
    for step in steps {
        if current >= step.target {
            continue;
        }
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        current = version(&tx, ctx.schema)?;
        validate_version(&tx, ctx, current, latest)?;
        if current < step.target {
            (step.run)(&tx, ctx).with_context(|| format!("running {}", step.name))?;
            tx.pragma_update(Some(ctx.schema), "user_version", step.target)?;
            tx.commit()?;
            current = step.target;
            ran += 1;
        }
    }
    Ok(ran)
}

pub(crate) fn migrate_durable(conn: &Connection) -> Result<Vec<String>> {
    let root = conn
        .path()
        .filter(|path| !path.is_empty())
        .and_then(|path| Path::new(path).parent());
    let mut ctx = Ctx {
        schema: "main",
        file: "dugix.db",
        root,
        notes: Vec::new(),
    };
    run(conn, DURABLE, &mut ctx)?;
    Ok(ctx.notes)
}

/// Upgrade the cache, attached as `cache` or opened on its own as `main`.
pub fn migrate_cache(conn: &Connection, schema: &str) -> Result<()> {
    run(
        conn,
        CACHE,
        &mut Ctx {
            schema,
            file: "cache.db",
            root: None,
            notes: Vec::new(),
        },
    )?;
    Ok(())
}

pub(super) fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    schema_has_column(conn, "main", table, column)
}

fn schema_has_column(conn: &Connection, schema: &str, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info({table})"))?;
    for name in stmt.query_map([], |row| row.get::<_, String>(1))? {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn durable_v1(tx: &Transaction<'_>, ctx: &mut Ctx<'_>) -> Result<()> {
    tx.execute_batch(DURABLE_SQL)?;
    for (table, column, definition) in [
        ("jobs", "kind", "TEXT NOT NULL DEFAULT 'agent'"),
        ("jobs", "topic", "TEXT"),
        ("usage_log", "kind", "TEXT"),
        ("usage_log", "tools", "TEXT"),
    ] {
        if !has_column(tx, table, column)? {
            tx.execute_batch(&format!(
                "ALTER TABLE main.{table} ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    if has_column(tx, "files", "mtime")? {
        tx.execute_batch("ALTER TABLE main.files DROP COLUMN mtime")?;
    }
    tx.execute_batch(
        "DROP TABLE IF EXISTS main.message_images;
         DROP TABLE IF EXISTS main.web_cache;
         DROP TABLE IF EXISTS main.file_chunks;
         DROP TABLE IF EXISTS main.chunk_embeddings;
         DROP TABLE IF EXISTS main.model_prices;
         DELETE FROM main.app_settings WHERE key IN (
             'helper_model', 'memory_model', 'transcriber_model', 'ocr_model',
             'ocr_engine', 'local_ocr_model', 'ocr_images', 'embedding_model',
             'image_gen_model', 'video_gen_model'
         );",
    )?;
    import_memory(tx, ctx)?;
    Ok(())
}

fn drop_older_peer_columns(tx: &Transaction<'_>, _ctx: &mut Ctx<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE main.sessions DROP COLUMN web_mode;
         ALTER TABLE main.memory_chapters DROP COLUMN tidied_at;",
    )?;
    Ok(())
}

fn research_jobs_replace_watches(tx: &Transaction<'_>, _ctx: &mut Ctx<'_>) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO main.device_meta (device_id, created_at)
         SELECT ?1, ?2 WHERE NOT EXISTS (SELECT 1 FROM main.device_meta)",
        (uuid::Uuid::new_v4().to_string(), &now),
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO main.jobs
         (id, space_id, session_id, device_id, name, prompt, root, mode,
          trigger_kind, interval_minutes, max_iters, max_cost_usd, max_wall_minutes,
          notify, enabled, last_run_at, updated_at, kind, topic)
         SELECT id, space_id, session_id, (SELECT device_id FROM main.device_meta LIMIT 1),
          topic, topic, '', 'agent', 'interval', interval_hours * 60, 5, 1.0, 30,
          'on_change', 1, last_run_at, COALESCE(updated_at, ?1), 'research', topic
         FROM main.watches",
        [&now],
    )?;
    tx.execute_batch("DROP TABLE main.watches")?;
    Ok(())
}

fn share_memory_chapters(tx: &Transaction<'_>, _ctx: &mut Ctx<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE main.memory_chapters ADD COLUMN shared_with TEXT NOT NULL DEFAULT '[]'",
    )?;
    Ok(())
}

fn drop_watch_jobs(tx: &Transaction<'_>, ctx: &mut Ctx<'_>) -> Result<()> {
    tx.execute_batch(&format!("DROP TABLE IF EXISTS {}.watch_jobs", ctx.schema))?;
    Ok(())
}

fn import_memory(tx: &Transaction<'_>, ctx: &mut Ctx<'_>) -> Result<()> {
    let Some(root) = ctx.root else {
        return Ok(());
    };
    let mut stmt = tx.prepare("SELECT id, name FROM main.spaces ORDER BY name")?;
    let spaces = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut total = 0;
    let mut imported_spaces = 0;
    for space in spaces {
        let (id, name) = space?;
        let path = root.join("spaces").join(&name).join("memory.md");
        if !path.exists() {
            continue;
        }
        match crate::memory::import_legacy(tx, &id, &path) {
            Ok(0) => {}
            Ok(n) => {
                total += n;
                imported_spaces += 1;
            }
            Err(error) => ctx
                .notes
                .push(format!("memory import failed for {name}: {error}")),
        }
    }
    if total > 0 {
        ctx.notes.push(if imported_spaces == 1 {
            format!(
                "memory moved into the database · {total} facts in \"{}\"",
                crate::memory::ABOUT_YOU
            )
        } else {
            format!(
                "memory moved into the database · {total} facts across {imported_spaces} spaces"
            )
        });
    }
    Ok(())
}

fn cache_v1(tx: &Transaction<'_>, ctx: &mut Ctx<'_>) -> Result<()> {
    let schema = ctx.schema;
    tx.execute_batch(&CACHE_SQL.replace("{schema}", schema))?;
    for (table, column, definition) in [
        ("chunk_embeddings", "identity", "TEXT NOT NULL DEFAULT ''"),
        ("chunk_embeddings", "location", "TEXT NOT NULL DEFAULT ''"),
        ("chunk_embeddings", "text", "TEXT NOT NULL DEFAULT ''"),
        ("session_workspaces", "mode", "TEXT"),
        (
            "file_index_state",
            "extractor",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        ("decisions", "cost", "REAL"),
    ] {
        if !schema_has_column(tx, schema, table, column)? {
            tx.execute_batch(&format!(
                "ALTER TABLE {schema}.{table} ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "research_jobs_migration_tests.rs"]
mod research_jobs_tests;
