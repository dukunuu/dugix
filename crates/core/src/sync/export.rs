//! Building outgoing changesets and acks.

use anyhow::{Result, bail};
use rusqlite::params_from_iter;

use crate::db::{Db, migrations::durable_schema_version};

use super::registry::{
    Cursor, SYNC_TOMBSTONES, TABLES, TableSpec, decode_updated_cursor, valid_cursor,
};
use super::{Changeset, FileChange, PeerCursor, RowChange, Tombstone};

/// Build this device's export for `peer_id`: every row past the peer's acked
/// cursor per table (no cursor means a full export, the first-run bootstrap), the
/// un-acked tombstones and the file manifest. Advances no cursor.
pub fn build_changeset(db: &Db, peer_id: Option<&str>, name: &str) -> Result<Changeset> {
    let device_id = db.device_id()?;
    let mut cs = Changeset {
        schema: durable_schema_version(),
        device_id,
        device_name: name.to_string(),
        ack: None,
        rows: Vec::new(),
        tombstones: Vec::new(),
        files: Vec::new(),
        generated_at: chrono::Utc::now().to_rfc3339(),
    };
    let states = db.load_sync_state()?;
    let push_cursor = |table: &str| {
        peer_id.and_then(|p| {
            states
                .iter()
                .find(|s| s.peer_id == p && s.table_name == table)
                .and_then(|s| s.push_cursor.clone())
        })
    };
    for spec in TABLES {
        if spec.cursor == Cursor::None {
            continue;
        }
        let rows = select_rows(db, spec, push_cursor(spec.name).as_deref())?;
        for row in rows {
            if spec.name == "sessions" {
                for persona in select_personas(db, &row)? {
                    cs.rows.push(RowChange {
                        table: "swarm_personas".to_string(),
                        row: persona,
                    });
                }
            }
            if spec.name == "files" {
                cs.files.push(FileChange {
                    space_id: row
                        .get("space_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: row
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    hash: row
                        .get("hash")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    size: row
                        .get("size")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                });
            }
            cs.rows.push(RowChange {
                table: spec.name.to_string(),
                row,
            });
        }
    }
    let cursor = push_cursor(SYNC_TOMBSTONES);
    let c = cursor
        .as_deref()
        .filter(|c| valid_cursor(SYNC_TOMBSTONES, c))
        .unwrap_or("0");
    let mut stmt = db.conn().prepare(&format!(
        "SELECT id, table_name, row_id, deleted_at FROM {SYNC_TOMBSTONES} WHERE id > ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([c], |r| {
        Ok(Tombstone {
            origin_id: r.get(0)?,
            table_name: r.get(1)?,
            row_id: r.get(2)?,
            deleted_at: r.get(3)?,
        })
    })?;
    cs.tombstones = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cs)
}

/// This device's pull cursors for `peer_id`'s data: "I imported your rows up to
/// here". Transports embed it in their next changeset; without it the peer would
/// re-export everything forever.
pub fn build_ack(db: &Db, peer_id: &str) -> Result<Vec<PeerCursor>> {
    Ok(db
        .load_sync_state()?
        .iter()
        .filter(|s| s.peer_id == peer_id)
        .filter_map(|s| {
            s.pull_cursor
                .as_deref()
                .filter(|cursor| valid_cursor(&s.table_name, cursor))
                .map(|cursor| PeerCursor {
                    peer_id: peer_id.to_string(),
                    table_name: s.table_name.clone(),
                    cursor: cursor.to_string(),
                })
        })
        .collect())
}

/// Rows of one table past a cursor, in cursor order. Rows without an `updated_at`
/// stay home: they could never win LWW anyway.
pub(super) fn select_rows(
    db: &Db,
    spec: &TableSpec,
    cursor: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let cursor = cursor.filter(|c| valid_cursor(spec.name, c));
    let cols = spec.columns.join(", ");
    let (sql, params): (String, Vec<rusqlite::types::Value>) = match spec.cursor {
        Cursor::UpdatedAt => {
            // Device-local settings are structurally invisible to sync. The cursor is
            // `(updated_at, pk…)`, not only a timestamp: two rows can legitimately
            // share an RFC3339 timestamp.
            let (updated_at, keys) = decode_updated_cursor(cursor.unwrap_or(""), spec.pk.len());
            let ordered_columns = std::iter::once("COALESCE(updated_at, '')")
                .chain(spec.pk.iter().copied())
                .collect::<Vec<_>>();
            let placeholders = (1..=ordered_columns.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let scope_filter = if spec.name == "app_settings" {
                " AND scope = 'sync'"
            } else {
                ""
            };
            let mut values = vec![rusqlite::types::Value::Text(updated_at)];
            values.extend(keys.into_iter().map(rusqlite::types::Value::Text));
            (
                format!(
                    "SELECT {cols} FROM {} WHERE COALESCE(updated_at, '') <> '' \
                     AND ({}) > ({}){scope_filter} \
                     ORDER BY COALESCE(updated_at, ''), {}",
                    spec.name,
                    ordered_columns.join(", "),
                    placeholders,
                    spec.pk.join(", ")
                ),
                values,
            )
        }
        Cursor::Tuple(tuple_cols) => {
            let (a, b) = (tuple_cols[0], tuple_cols[1]);
            let (ca, cb) = cursor.and_then(|c| c.split_once('|')).unwrap_or(("", ""));
            (
                format!(
                    "SELECT {cols} FROM {} WHERE ({a}, {b}) > (?1, ?2) ORDER BY {a}, {b}",
                    spec.name
                ),
                vec![
                    rusqlite::types::Value::Text(ca.to_string()),
                    rusqlite::types::Value::Text(cb.to_string()),
                ],
            )
        }
        Cursor::AutoId => (
            format!("SELECT {cols} FROM {} WHERE id > ?1 ORDER BY id", spec.name),
            vec![rusqlite::types::Value::Text(
                cursor.unwrap_or("0").to_string(),
            )],
        ),
        Cursor::None => bail!("{} has no cursor", spec.name),
    };
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), |r| {
        row_to_json(r, spec.columns)
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A session row's persona roster, attached to the session row because personas
/// have no cursor of their own.
fn select_personas(db: &Db, session: &serde_json::Value) -> Result<Vec<serde_json::Value>> {
    let Some(session_id) = session.get("id").and_then(serde_json::Value::as_str) else {
        return Ok(Vec::new());
    };
    let mut stmt = db.conn().prepare(
        "SELECT session_id, ord, name, model, persona FROM swarm_personas WHERE session_id = ?1",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        row_to_json(r, &["session_id", "ord", "name", "model", "persona"])
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn row_to_json(row: &rusqlite::Row, columns: &[&str]) -> rusqlite::Result<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    for (i, column) in columns.iter().enumerate() {
        let v: rusqlite::types::Value = row.get(i)?;
        obj.insert((*column).to_string(), sqlite_to_json(v));
    }
    Ok(serde_json::Value::Object(obj))
}

fn sqlite_to_json(v: rusqlite::types::Value) -> serde_json::Value {
    match v {
        rusqlite::types::Value::Null | rusqlite::types::Value::Blob(_) => serde_json::Value::Null,
        rusqlite::types::Value::Integer(i) => serde_json::Value::from(i),
        rusqlite::types::Value::Real(f) => serde_json::Number::from_f64(f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        rusqlite::types::Value::Text(s) => serde_json::Value::from(s),
    }
}

pub(super) fn json_to_value(v: &serde_json::Value) -> rusqlite::types::Value {
    match v {
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            rusqlite::types::Value::Null
        }
        serde_json::Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)),
            rusqlite::types::Value::Integer,
        ),
        serde_json::Value::String(s) => rusqlite::types::Value::Text(s.clone()),
    }
}

/// The first 8 chars of a uuid: enough to disambiguate in log lines, and the
/// suffix space-name collision resolution renames with.
pub(super) fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
