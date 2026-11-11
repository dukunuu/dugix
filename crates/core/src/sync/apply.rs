//! Applying an incoming changeset: LWW, append, spaces, files, tombstones.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension as _, params_from_iter};

use crate::db::{DEFAULT_SPACE, Db, migrations::durable_schema_version};
use crate::space::Space;

use super::blobs::{file_matches, pull_blob};
use super::export::{json_to_value, short_id};
use super::registry::{
    Cursor, SYNC_TOMBSTONES, TABLES, TableSpec, known_table, position_gt, row_position,
    valid_component, valid_cursor,
};
use super::{ApplySummary, Changeset, FileChange, PeerCursor, Tombstone};

// ── apply ──

enum LwwOutcome {
    Applied,
    Skipped,
    Warned(String),
}

/// Whether an incoming row wins LWW against a local opponent.
/// `local_updated`/`local_tie` are `''`/empty when no local row exists.
fn lww_wins(
    spec: &TableSpec,
    row: &serde_json::Value,
    local_updated: &str,
    local_tie: &str,
) -> bool {
    let incoming_updated = row
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let incoming_tie = spec
        .pk
        .iter()
        .map(|c| {
            row.get(*c)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\u{1f}");
    (incoming_updated, incoming_tie.as_str()) > (local_updated, local_tie)
}

/// `(updated_at, id) > (local_updated_at, local_id)` — the spaces/files
/// LWW comparison, id-tiebroken so equal timestamps stay deterministic.
fn lww_wins_against(id: &str, updated: &str, local_id: &str, local_updated: &str) -> bool {
    (updated, id) > (local_updated, local_id)
}

/// Apply one changeset: rows (per the registry), tombstones, file blobs,
/// and the embedded ack. Returns the summary plus the reply cursors — the
/// receiver's new pull cursors per table, to be acked back to the sender.
/// `blob_source`, when given, is a directory whose content-addressed
/// `blobs/by-hash/<hash>` path holds the sender's payloads; missing blobs are
/// reported instead.
// Long by design: one step per sync rule, in registry order.
#[allow(clippy::too_many_lines)]
pub fn apply_changeset(
    db: &Db,
    space: &Space,
    cs: &Changeset,
    blob_source: Option<&Path>,
) -> Result<(ApplySummary, Vec<PeerCursor>)> {
    let ours = durable_schema_version();
    if cs.schema != ours {
        bail!(
            "{} runs a different nexus version (schema {}, this device {ours}) — update nexus on both devices, then sync again",
            cs.device_name,
            cs.schema
        );
    }
    let my_id = db.device_id()?;
    let mut summary = ApplySummary::default();
    let mut by_table: HashMap<&str, Vec<&serde_json::Value>> = HashMap::new();
    for rc in &cs.rows {
        by_table.entry(rc.table.as_str()).or_default().push(&rc.row);
    }
    for table in by_table.keys() {
        if !known_table(table) {
            summary
                .warnings
                .push(format!("changeset has rows for unknown table {table:?}"));
        }
    }

    // Rows, in registry order. `max_pos` tracks the highest position
    // received per table — the reply cursor. Losing LWW rows are safe to
    // acknowledge because their data is already present locally; warned
    // rows remove the table cursor below so they are retried.
    let mut max_pos: HashMap<&'static str, String> = HashMap::new();
    let mut failed_tables: HashSet<&'static str> = HashSet::new();
    let mut won_sessions: HashSet<String> = HashSet::new();
    let mut won_files: HashSet<(String, String)> = HashSet::new();
    for spec in TABLES {
        if spec.cursor == Cursor::None {
            continue; // swarm_personas travel with their session row
        }
        let Some(rows) = by_table.get(spec.name) else {
            continue;
        };
        for row in rows {
            let pos = row_position(spec, row);
            max_pos
                .entry(spec.name)
                .and_modify(|p| {
                    if position_gt(spec.name, &pos, p) {
                        p.clone_from(&pos);
                    }
                })
                .or_insert(pos);
            let outcome = match apply_row(db, space, spec, row) {
                Ok(o) => o,
                Err(e) => LwwOutcome::Warned(format!("{} row failed: {e:#}", spec.name)),
            };
            match outcome {
                LwwOutcome::Applied => {
                    summary.rows_applied += 1;
                    if spec.name == "sessions"
                        && let Some(id) = row.get("id").and_then(serde_json::Value::as_str)
                    {
                        won_sessions.insert(id.to_string());
                    }
                    if spec.name == "files"
                        && let (Some(sid), Some(name)) = (
                            row.get("space_id").and_then(serde_json::Value::as_str),
                            row.get("name").and_then(serde_json::Value::as_str),
                        )
                    {
                        won_files.insert((sid.to_string(), name.to_string()));
                    }
                }
                LwwOutcome::Skipped => summary.rows_skipped += 1,
                LwwOutcome::Warned(w) => {
                    summary.rows_skipped += 1;
                    summary.warnings.push(w);
                    // Do not acknowledge a table past a row that was not
                    // applied. The sender will retry it; advancing here
                    // would turn a transient/local validation problem into
                    // silent permanent data loss.
                    failed_tables.insert(spec.name);
                }
            }
        }
    }
    for table in failed_tables {
        max_pos.remove(table);
    }

    // swarm_personas: applied only for sessions that won in this changeset,
    // as a wholesale roster replace (the collection has no per-row LWW).
    if let Some(personas) = by_table.get("swarm_personas") {
        let mut rosters: HashMap<&str, Vec<&serde_json::Value>> = HashMap::new();
        for p in personas {
            if let Some(sid) = p.get("session_id").and_then(serde_json::Value::as_str) {
                rosters.entry(sid).or_default().push(p);
            }
        }
        for (sid, roster) in rosters {
            if won_sessions.contains(sid) {
                let conn = db.conn();
                conn.execute("DELETE FROM swarm_personas WHERE session_id = ?1", [sid])?;
                for p in &roster {
                    let values: Vec<rusqlite::types::Value> =
                        ["session_id", "ord", "name", "model", "persona"]
                            .iter()
                            .map(|c| json_to_value(p.get(*c).unwrap_or(&serde_json::Value::Null)))
                            .collect();
                    conn.execute(
                        "INSERT INTO swarm_personas (session_id, ord, name, model, persona)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params_from_iter(values.iter()),
                    )?;
                }
                summary.rows_applied += roster.len();
            } else {
                summary.rows_skipped += roster.len();
                summary.warnings.push(format!(
                    "swarm personas for session {sid} skipped — their session row lost LWW"
                ));
            }
        }
    }

    // Tombstones, after rows: within one changeset a row and its tombstone
    // never coexist (a deleted row is not re-exported), and across
    // changesets the destructive delete is the newer intent.
    let mut tombstone_max = 0i64;
    for t in &cs.tombstones {
        tombstone_max = tombstone_max.max(t.origin_id);
        if apply_tombstone(db, space, t, &mut summary)? {
            summary.tombstones_applied += 1;
        }
    }
    if !cs.tombstones.is_empty() {
        max_pos.insert(SYNC_TOMBSTONES, tombstone_max.to_string());
    }

    // File blobs: for every file row that won, keep the local blob when it
    // matches, else pull it from the transport's blob channel. A manifest
    // may have landed during an earlier metadata-only import, so retry a
    // missing blob even when the row itself is an idempotent LWW skip.
    for fc in &cs.files {
        let manifest_matches = local_file_matches_manifest(db, fc)?;
        if !won_files.contains(&(fc.space_id.clone(), fc.name.clone())) && !manifest_matches {
            continue;
        }
        if !valid_component(&fc.name) || !valid_component(&fc.space_id) {
            summary.warnings.push(format!(
                "skipping blob for unsafe file {:?}/{:?}",
                fc.space_id, fc.name
            ));
            continue;
        }
        let Some(space_name) = space_name_for(db, &fc.space_id)? else {
            summary.warnings.push(format!(
                "skipping blob for {:?} — its space no longer exists",
                fc.name
            ));
            continue;
        };
        let target = space.files_dir(&space_name).join(&fc.name);
        if file_matches(&target, &fc.hash) {
            summary.files_kept += 1;
            continue;
        }
        let pulled = match blob_source {
            Some(src) => match pull_blob(src, &fc.hash, &target) {
                Ok(true) => {
                    summary.files_pulled += 1;
                    true
                }
                _ => false,
            },
            None => false,
        };
        if !pulled {
            summary.files_missing.push(fc.clone());
            summary.warnings.push(format!(
                "blob for {:?} unavailable — fetch it with a dir or ssh transport",
                fc.name
            ));
        }
    }

    // The embedded ack: entries addressed to this device advance the
    // sender's push cursors — the only thing that does.
    if let Some(acks) = &cs.ack {
        let states = db.load_sync_state()?;
        for a in acks {
            if a.peer_id != my_id {
                continue;
            }
            if !known_table(&a.table_name) {
                summary
                    .warnings
                    .push(format!("ack for unknown table {:?}", a.table_name));
                continue;
            }
            if !valid_cursor(&a.table_name, &a.cursor) {
                summary.warnings.push(format!(
                    "ack for {} has an invalid cursor and was ignored",
                    a.table_name
                ));
                continue;
            }
            let existing = states
                .iter()
                .find(|s| s.peer_id == cs.device_id && s.table_name == a.table_name)
                .and_then(|s| s.push_cursor.clone());
            if existing
                .as_deref()
                .is_none_or(|e| position_gt(&a.table_name, &a.cursor, e))
            {
                db.set_sync_state(&cs.device_id, &a.table_name, None, Some(&a.cursor))?;
                summary.acks_applied += 1;
            }
        }
    }

    // Advance pull cursors (monotonically) and build the reply cursors.
    let states = db.load_sync_state()?;
    let mut reply: Vec<PeerCursor> = Vec::new();
    for (table, pos) in &max_pos {
        let existing = states
            .iter()
            .find(|s| s.peer_id == cs.device_id && s.table_name == *table)
            .and_then(|s| s.pull_cursor.clone());
        let final_pos = match &existing {
            Some(e) if position_gt(table, e, pos) => e.clone(),
            _ => pos.clone(),
        };
        if existing.as_deref() != Some(final_pos.as_str()) {
            db.set_sync_state(&cs.device_id, table, Some(&final_pos), None)?;
            reply.push(PeerCursor {
                peer_id: cs.device_id.clone(),
                table_name: (*table).to_string(),
                cursor: final_pos,
            });
        }
    }
    Ok((summary, reply))
}

fn apply_row(
    db: &Db,
    space: &Space,
    spec: &TableSpec,
    row: &serde_json::Value,
) -> Result<LwwOutcome> {
    match spec.cursor {
        Cursor::UpdatedAt if spec.name == "spaces" => apply_space(db, space, row),
        Cursor::UpdatedAt if spec.name == "files" => apply_file(db, row),
        Cursor::UpdatedAt if spec.name == "jobs" && row.get("kind").is_none() => {
            let mut row = row.clone();
            row["kind"] = "agent".into();
            apply_lww(db, spec, &row)
        }
        Cursor::UpdatedAt => apply_lww(db, spec, row),
        Cursor::Tuple(_) | Cursor::AutoId => Ok(if apply_append(db, spec, row)? {
            LwwOutcome::Applied
        } else {
            LwwOutcome::Skipped
        }),
        Cursor::None => Ok(LwwOutcome::Skipped),
    }
}

/// LWW upsert for a plain row (not spaces/files — those have their own
/// name-merge rules). A `scope != 'sync'` `app_settings` row never lands.
fn apply_lww(db: &Db, spec: &TableSpec, row: &serde_json::Value) -> Result<LwwOutcome> {
    if spec.name == "app_settings"
        && row.get("scope").and_then(serde_json::Value::as_str) != Some("sync")
    {
        return Ok(LwwOutcome::Skipped);
    }
    let conn = db.conn();
    let pk_values: Vec<String> = spec
        .pk
        .iter()
        .map(|c| {
            row.get(*c)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let where_sql = spec
        .pk
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{c} = ?{}", i + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let existing: Option<String> = conn
        .query_row(
            &format!(
                "SELECT COALESCE(updated_at, '') FROM {} WHERE {where_sql}",
                spec.name
            ),
            params_from_iter(pk_values.iter()),
            |r| r.get(0),
        )
        .optional()?;
    let local_tie = pk_values.join("\u{1f}");
    if !lww_wins(spec, row, existing.as_deref().unwrap_or(""), &local_tie) {
        return Ok(LwwOutcome::Skipped);
    }
    let values: Vec<rusqlite::types::Value> = spec
        .apply_columns
        .iter()
        .map(|c| json_to_value(row.get(*c).unwrap_or(&serde_json::Value::Null)))
        .collect();
    if existing.is_some() {
        let set_sql = spec
            .apply_columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c} = ?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        // The WHERE placeholders follow the SET's (the SELECT above used
        // its own 1-based clause).
        let update_where = spec
            .pk
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c} = ?{}", spec.apply_columns.len() + i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut all = values;
        all.extend(
            pk_values
                .iter()
                .map(|v| rusqlite::types::Value::Text(v.clone())),
        );
        conn.execute(
            &format!("UPDATE {} SET {set_sql} WHERE {update_where}", spec.name),
            params_from_iter(all.iter()),
        )?;
    } else {
        let cols = spec.apply_columns.join(", ");
        let marks = (1..=spec.apply_columns.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute(
            &format!("INSERT INTO {} ({cols}) VALUES ({marks})", spec.name),
            params_from_iter(values.iter()),
        )?;
    }
    Ok(LwwOutcome::Applied)
}

/// INSERT OR IGNORE by the row's sync identity — the append-only union
/// rule. `changes()` tells an ignored duplicate apart from a real insert.
fn apply_append(db: &Db, spec: &TableSpec, row: &serde_json::Value) -> Result<bool> {
    let cols = spec.apply_columns.join(", ");
    let marks = (1..=spec.apply_columns.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let values: Vec<rusqlite::types::Value> = spec
        .apply_columns
        .iter()
        .map(|c| json_to_value(row.get(*c).unwrap_or(&serde_json::Value::Null)))
        .collect();
    let n = db.conn().execute(
        &format!(
            "INSERT OR IGNORE INTO {} ({cols}) VALUES ({marks})",
            spec.name
        ),
        params_from_iter(values.iter()),
    )?;
    Ok(n > 0)
}

/// A space row's apply: LWW like the others, plus the name-merge rules —
/// spaces are UNIQUE by name, and two devices may have independently
/// created different spaces with the same name. The LWW loser is renamed
/// to `<name>-<first8(loser_id)>` on both sides (the rename is a pure
/// function of the loser, so both devices compute the same name). Fresh
/// winners get their directory; renames move it.
fn apply_space(db: &Db, space: &Space, row: &serde_json::Value) -> Result<LwwOutcome> {
    let Some(id) = row.get("id").and_then(serde_json::Value::as_str) else {
        return Ok(LwwOutcome::Warned("space row without id".to_string()));
    };
    let Some(name) = row.get("name").and_then(serde_json::Value::as_str) else {
        return Ok(LwwOutcome::Warned(format!("space {id} without name")));
    };
    if !valid_component(name) {
        return Ok(LwwOutcome::Warned(format!(
            "skipping space {id}: unsafe name {name:?}"
        )));
    }
    let conn = db.conn();
    let incoming_updated = row
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let local: Option<(String, String)> = conn
        .query_row(
            "SELECT name, COALESCE(updated_at, '') FROM spaces WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((local_name, local_updated)) = local else {
        // New space. Its name may be taken by a different local space —
        // the loser of that pair is renamed deterministically.
        let colliding: Option<String> = conn
            .query_row(
                "SELECT id FROM spaces WHERE name = ?1 AND id != ?2",
                (name, id),
                |r| r.get(0),
            )
            .optional()?;
        let mut incoming_name = name.to_string();
        if let Some(other) = colliding {
            let other_updated: String = conn.query_row(
                "SELECT COALESCE(updated_at, '') FROM spaces WHERE id = ?1",
                [&other],
                |r| r.get(0),
            )?;
            if lww_wins_against(id, incoming_updated, &other, &other_updated) {
                let new_name = format!("{name}-{}", short_id(&other));
                conn.execute(
                    "UPDATE spaces SET name = ?1 WHERE id = ?2",
                    (new_name.as_str(), other.as_str()),
                )?;
                rename_dir(space, name, &new_name);
            } else {
                incoming_name = format!("{name}-{}", short_id(id));
            }
        }
        insert_space_row(conn, id, &incoming_name, row)?;
        if let Err(e) = space.ensure_space_dir(&incoming_name) {
            return Ok(LwwOutcome::Warned(format!(
                "space {id} applied but its dir failed: {e}"
            )));
        }
        return Ok(LwwOutcome::Applied);
    };
    if !lww_wins_against(id, incoming_updated, id, &local_updated) {
        return Ok(LwwOutcome::Skipped);
    }
    if name != local_name {
        rename_dir(space, &local_name, name);
    }
    insert_space_row(conn, id, name, row)?;
    Ok(LwwOutcome::Applied)
}

fn insert_space_row(
    conn: &Connection,
    id: &str,
    name: &str,
    row: &serde_json::Value,
) -> Result<()> {
    conn.execute(
        "INSERT INTO spaces (id, name, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET name = ?2, created_at = ?3, updated_at = ?4",
        (
            id,
            name,
            row.get("created_at")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
            row.get("updated_at")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        ),
    )?;
    Ok(())
}

/// Move a space's directory on a rename — best-effort: a missing old dir
/// is fine, a failing rename surfaces in the caller's warning path.
fn rename_dir(space: &Space, old: &str, new: &str) {
    let from = space.space_dir(old);
    let to = space.space_dir(new);
    if from.exists() && !to.exists() {
        let _ = std::fs::rename(&from, &to);
    }
}

/// A file row's apply: LWW, plus the (`space_id`, name) uniqueness merge —
/// the app keeps one row per space+name, so a synced row colliding with a
/// different id is the LWW opponent; the loser is replaced wholesale (id
/// and all) so both devices end on the same row id and deletes propagate.
fn apply_file(db: &Db, row: &serde_json::Value) -> Result<LwwOutcome> {
    let Some(id) = row.get("id").and_then(serde_json::Value::as_str) else {
        return Ok(LwwOutcome::Warned("file row without id".to_string()));
    };
    let (Some(space_id), Some(name)) = (
        row.get("space_id").and_then(serde_json::Value::as_str),
        row.get("name").and_then(serde_json::Value::as_str),
    ) else {
        return Ok(LwwOutcome::Warned(format!("file {id} without space/name")));
    };
    if !valid_component(name) || !valid_component(space_id) {
        return Ok(LwwOutcome::Warned(format!(
            "skipping file {id}: unsafe name {name:?}"
        )));
    }
    let conn = db.conn();
    let incoming_updated = row
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    // The opponent: the row with this id, or the row owning this
    // space+name (the app's uniqueness rule).
    let local: Option<(String, String)> = conn
        .query_row(
            "SELECT id, COALESCE(updated_at, '') FROM files WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .or(conn
            .query_row(
                "SELECT id, COALESCE(updated_at, '') FROM files
                 WHERE space_id = ?1 AND name = ?2",
                (space_id, name),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?);
    let Some((local_id, local_updated)) = local else {
        insert_file_row(conn, row)?;
        return Ok(LwwOutcome::Applied);
    };
    if !lww_wins_against(id, incoming_updated, &local_id, &local_updated) {
        return Ok(LwwOutcome::Skipped);
    }
    // Incoming wins — replace the local row wholesale, including its id:
    // the winner's id becomes the row's identity on both devices.
    let values: Vec<rusqlite::types::Value> = [
        "id",
        "space_id",
        "name",
        "hash",
        "size",
        "created_at",
        "updated_at",
    ]
    .iter()
    .map(|c| json_to_value(row.get(*c).unwrap_or(&serde_json::Value::Null)))
    .collect();
    let mut all = values;
    all.push(rusqlite::types::Value::Text(local_id));
    conn.execute(
        "UPDATE files SET id = ?1, space_id = ?2, name = ?3, hash = ?4, size = ?5,
            created_at = ?6, updated_at = ?7
         WHERE id = ?8",
        params_from_iter(all.iter()),
    )?;
    Ok(LwwOutcome::Applied)
}

fn insert_file_row(conn: &Connection, row: &serde_json::Value) -> Result<()> {
    let values: Vec<rusqlite::types::Value> = [
        "id",
        "space_id",
        "name",
        "hash",
        "size",
        "created_at",
        "updated_at",
    ]
    .iter()
    .map(|c| json_to_value(row.get(*c).unwrap_or(&serde_json::Value::Null)))
    .collect();
    conn.execute(
        "INSERT INTO files (id, space_id, name, hash, size, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params_from_iter(values.iter()),
    )?;
    Ok(())
}

/// Read the version that competes with a tombstone. Mutable rows use
/// `updated_at`; append-only rows use `created_at`. A newer offline write must
/// be allowed to revive a row after an older delete reaches the device.
fn row_version_for_tombstone(db: &Db, table: &str, row_id: &str) -> Result<Option<String>> {
    let conn = db.conn();
    let (table, id_column, version_column, id) = match table {
        "sessions" | "model_prefs" | "spaces" | "files" | "watches" | "pinned_panels" => {
            (table, "id", "updated_at", row_id)
        }
        "app_settings" => (table, "key", "updated_at", row_id),
        "messages" => (table, "id", "created_at", row_id),
        "usage_log" | "citations" => (table, "sync_id", "created_at", row_id),
        // Persona rows are versioned by their owning session. Their own
        // tombstones are retained for transitive propagation, but the row is
        // only removed by a winning session roster replacement.
        "swarm_personas" => {
            let Some((session_id, _)) = row_id.split_once(':') else {
                return Ok(None);
            };
            ("sessions", "id", "updated_at", session_id)
        }
        _ => return Ok(None),
    };
    Ok(conn
        .query_row(
            &format!(
                "SELECT COALESCE({version_column}, '') FROM {table} \
                 WHERE {id_column} = ?1"
            ),
            [id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Record an incoming tombstone and return the newest delete time known for
/// this `(table_name, row_id)`. Tombstones can cross several peers, so an
/// older copy must never replace a newer delete that was already recorded.
fn remember_tombstone(db: &Db, tombstone: &Tombstone) -> Result<String> {
    let conn = db.conn();
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, deleted_at FROM sync_tombstones
             WHERE table_name = ?1 AND row_id = ?2
             ORDER BY id DESC LIMIT 1",
            (&tombstone.table_name, &tombstone.row_id),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((id, deleted_at)) = existing else {
        conn.execute(
            "INSERT INTO sync_tombstones (table_name, row_id, deleted_at)
             VALUES (?1, ?2, ?3)",
            (
                &tombstone.table_name,
                &tombstone.row_id,
                &tombstone.deleted_at,
            ),
        )?;
        return Ok(tombstone.deleted_at.clone());
    };
    if tombstone.deleted_at > deleted_at {
        conn.execute(
            "UPDATE sync_tombstones SET deleted_at = ?1 WHERE id = ?2",
            (&tombstone.deleted_at, id),
        )?;
        Ok(tombstone.deleted_at.clone())
    } else {
        Ok(deleted_at)
    }
}

/// Apply one tombstone; returns whether it performed the destructive delete.
/// The incoming tombstone is recorded locally for transitive propagation,
/// even when a newer row makes the delete stale.
// Long by design: one arm per tombstonable table.
#[allow(clippy::too_many_lines)]
fn apply_tombstone(
    db: &Db,
    space: &Space,
    t: &Tombstone,
    summary: &mut ApplySummary,
) -> Result<bool> {
    if !matches!(
        t.table_name.as_str(),
        "spaces"
            | "sessions"
            | "messages"
            | "files"
            | "watches"
            | "usage_log"
            | "citations"
            | "swarm_personas"
            | "app_settings"
            | "pinned_panels"
    ) {
        summary
            .warnings
            .push(format!("tombstone for unknown table {:?}", t.table_name));
        return Ok(false);
    }
    let deleted_at = remember_tombstone(db, t)?;
    if t.table_name == "spaces" && t.row_id == DEFAULT_SPACE {
        summary
            .warnings
            .push("tombstone for the default space ignored".to_string());
        return Ok(false);
    }
    if let Some(version) = row_version_for_tombstone(db, &t.table_name, &t.row_id)?
        && version >= deleted_at
    {
        summary.warnings.push(format!(
            "stale tombstone for {} {:?} ignored; row version is newer",
            t.table_name, t.row_id
        ));
        return Ok(false);
    }

    let conn = db.conn();
    match t.table_name.as_str() {
        "spaces" => {
            let name: Option<String> = conn
                .query_row("SELECT name FROM spaces WHERE id = ?1", [&t.row_id], |r| {
                    r.get(0)
                })
                .optional()?;
            conn.execute("DELETE FROM spaces WHERE id = ?1", [&t.row_id])?;
            if let Some(name) = name
                && let Err(e) = space.remove_space_dir(&name)
            {
                summary
                    .warnings
                    .push(format!("removing space dir {name}: {e}"));
            }
            Ok(true)
        }
        "sessions" => {
            conn.execute("DELETE FROM messages WHERE session_id = ?1", [&t.row_id])?;
            conn.execute(
                "DELETE FROM panel_states WHERE session_id = ?1",
                [&t.row_id],
            )?;
            conn.execute(
                "DELETE FROM session_sources WHERE session_id = ?1",
                [&t.row_id],
            )?;
            conn.execute(
                "DELETE FROM swarm_personas WHERE session_id = ?1",
                [&t.row_id],
            )?;
            conn.execute("DELETE FROM sessions WHERE id = ?1", [&t.row_id])?;
            Ok(true)
        }
        "messages" => {
            conn.execute("DELETE FROM messages WHERE id = ?1", [&t.row_id])?;
            Ok(true)
        }
        "files" => {
            let row: Option<(String, String)> = conn
                .query_row(
                    "SELECT space_id, name FROM files WHERE id = ?1",
                    [&t.row_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            conn.execute(
                "DELETE FROM cache.file_chunks WHERE file_id = ?1",
                [&t.row_id],
            )?;
            conn.execute(
                "DELETE FROM cache.chunk_embeddings WHERE file_id = ?1",
                [&t.row_id],
            )?;
            conn.execute(
                "DELETE FROM cache.file_index_state WHERE file_id = ?1",
                [&t.row_id],
            )?;
            conn.execute("DELETE FROM files WHERE id = ?1", [&t.row_id])?;
            if let Some((space_id, name)) = row
                && let Some(space_name) = space_name_for(db, &space_id)?
                && valid_component(&name)
            {
                let blob = space.files_dir(&space_name).join(&name);
                let _ = std::fs::remove_file(blob);
            }
            Ok(true)
        }
        "watches" => {
            conn.execute("DELETE FROM watches WHERE id = ?1", [&t.row_id])?;
            Ok(true)
        }
        "pinned_panels" => {
            conn.execute("DELETE FROM pinned_panels WHERE id = ?1", [&t.row_id])?;
            Ok(true)
        }
        "usage_log" => {
            conn.execute("DELETE FROM usage_log WHERE sync_id = ?1", [&t.row_id])?;
            Ok(true)
        }
        "citations" => {
            conn.execute("DELETE FROM citations WHERE sync_id = ?1", [&t.row_id])?;
            Ok(true)
        }
        "app_settings" => {
            // Local-scope settings are never exported, but reject a forged
            // tombstone rather than allowing a peer to delete device state.
            let scope: Option<String> = conn
                .query_row(
                    "SELECT scope FROM app_settings WHERE key = ?1",
                    [&t.row_id],
                    |row| row.get(0),
                )
                .optional()?;
            if scope.as_deref() == Some("local") {
                summary
                    .warnings
                    .push(format!("local setting {:?} tombstone ignored", t.row_id));
                return Ok(false);
            }
            conn.execute("DELETE FROM app_settings WHERE key = ?1", [&t.row_id])?;
            Ok(true)
        }
        // Persona tombstones are subsumed by the roster replace that
        // accompanies a winning session row — applying them alone could
        // delete slots of a roster the session's LWW winner restored.
        "swarm_personas" => Ok(false),
        _ => unreachable!("validated tombstone table"),
    }
}

/// Whether the local durable manifest still describes the incoming file.
/// This enables a later blob-only retry after a metadata changeset was
/// imported without its transport channel.
fn local_file_matches_manifest(db: &Db, file: &FileChange) -> Result<bool> {
    if !valid_component(&file.space_id) || !valid_component(&file.name) {
        return Ok(false);
    }
    Ok(db
        .conn()
        .query_row(
            "SELECT hash, size FROM files WHERE space_id = ?1 AND name = ?2",
            (&file.space_id, &file.name),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .is_some_and(|(hash, size)| hash == file.hash && size == file.size))
}

/// A space row's name for a space id, when the space still exists.
pub(super) fn space_name_for(db: &Db, space_id: &str) -> Result<Option<String>> {
    Ok(db
        .conn()
        .query_row("SELECT name FROM spaces WHERE id = ?1", [space_id], |r| {
            r.get(0)
        })
        .optional()?)
}
