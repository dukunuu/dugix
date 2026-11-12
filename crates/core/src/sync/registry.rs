//! The per-table sync rules: cursor kind, columns, keys, and cursor encoding.

/// How a table's rows are ordered and resumed across changesets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// Mutable rows: an opaque JSON `(updated_at, pk…)` tuple. Rows that never got
    /// a version are never exported; the primary key keeps a row created in the same
    /// timestamp as the last acknowledged row from being skipped forever.
    UpdatedAt,
    /// Append-only rows: a `(col1, col2)` row-value tuple, encoded as `"a|b"`.
    Tuple(&'static [&'static str]),
    /// Append-only rows cursor on their device-local AUTOINCREMENT id; the id rides
    /// along in the row JSON for bookkeeping but is never applied.
    AutoId,
    /// Rows travel attached to a parent row (`swarm_personas` → sessions).
    None,
}

pub struct TableSpec {
    pub name: &'static str,
    /// Exported in the row JSON; order is the SELECT list.
    pub columns: &'static [&'static str],
    /// A subset of `columns`: AUTOINCREMENT ids are dropped where they would
    /// collide across devices and kept where the id is the sync identity.
    pub apply_columns: &'static [&'static str],
    /// The INSERT OR IGNORE target and the LWW tiebreak.
    pub pk: &'static [&'static str],
    pub cursor: Cursor,
}

/// In apply order: sessions before their children, spaces and files before
/// messages so foreign rows land after their parents.
pub const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "sessions",
        columns: &[
            "id",
            "title",
            "model",
            "slug",
            "space_id",
            "compact_summary",
            "compact_through",
            "swarm_mode",
            "kind",
            "research_parent_id",
            "created_at",
            "updated_at",
        ],
        apply_columns: &[
            "id",
            "title",
            "model",
            "slug",
            "space_id",
            "compact_summary",
            "compact_through",
            "swarm_mode",
            "kind",
            "research_parent_id",
            "created_at",
            "updated_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "swarm_personas",
        columns: &["session_id", "ord", "name", "model", "persona"],
        apply_columns: &["session_id", "ord", "name", "model", "persona"],
        pk: &["session_id", "ord"],
        cursor: Cursor::None,
    },
    TableSpec {
        name: "model_prefs",
        columns: &["id", "favorite", "last_used", "reasoning", "updated_at"],
        apply_columns: &["id", "favorite", "last_used", "reasoning", "updated_at"],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "spaces",
        columns: &["id", "name", "created_at", "updated_at"],
        apply_columns: &["id", "name", "created_at", "updated_at"],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "files",
        columns: &[
            "id",
            "space_id",
            "name",
            "hash",
            "size",
            "created_at",
            "updated_at",
        ],
        apply_columns: &[
            "id",
            "space_id",
            "name",
            "hash",
            "size",
            "created_at",
            "updated_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "watches",
        columns: &[
            "id",
            "space_id",
            "topic",
            "interval_hours",
            "session_id",
            "last_run_at",
            "updated_at",
        ],
        apply_columns: &[
            "id",
            "space_id",
            "topic",
            "interval_hours",
            "session_id",
            "last_run_at",
            "updated_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "jobs",
        columns: &[
            "id",
            "space_id",
            "session_id",
            "device_id",
            "name",
            "prompt",
            "root",
            "mode",
            "trigger_kind",
            "interval_minutes",
            "run_at",
            "stop_command",
            "max_iters",
            "max_cost_usd",
            "max_wall_minutes",
            "notify",
            "enabled",
            "last_run_at",
            "updated_at",
            "kind",
            "topic",
        ],
        apply_columns: &[
            "id",
            "space_id",
            "session_id",
            "device_id",
            "name",
            "prompt",
            "root",
            "mode",
            "trigger_kind",
            "interval_minutes",
            "run_at",
            "stop_command",
            "max_iters",
            "max_cost_usd",
            "max_wall_minutes",
            "notify",
            "enabled",
            "last_run_at",
            "updated_at",
            "kind",
            "topic",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "job_runs",
        columns: &[
            "id",
            "job_id",
            "device_id",
            "started_at",
            "finished_at",
            "status",
            "iters",
            "cost_usd",
            "branch",
            "summary",
            "updated_at",
        ],
        apply_columns: &[
            "id",
            "job_id",
            "device_id",
            "started_at",
            "finished_at",
            "status",
            "iters",
            "cost_usd",
            "branch",
            "summary",
            "updated_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "app_settings",
        columns: &["key", "value", "scope", "updated_at"],
        apply_columns: &["key", "value", "scope", "updated_at"],
        pk: &["key"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "session_sources",
        columns: &["session_id", "url_norm", "flag", "updated_at"],
        apply_columns: &["session_id", "url_norm", "flag", "updated_at"],
        pk: &["session_id", "url_norm"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "panel_states",
        columns: &["session_id", "panel_key", "record", "updated_at"],
        apply_columns: &["session_id", "panel_key", "record", "updated_at"],
        pk: &["session_id", "panel_key"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "pinned_panels",
        columns: &[
            "id",
            "space_id",
            "name",
            "title",
            "program",
            "record",
            "created_at",
            "updated_at",
        ],
        apply_columns: &[
            "id",
            "space_id",
            "name",
            "title",
            "program",
            "record",
            "created_at",
            "updated_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "memory_chapters",
        columns: &[
            "id",
            "space_id",
            "title",
            "description",
            "pinned",
            "created_at",
            "updated_at",
            "deleted_at",
        ],
        apply_columns: &[
            "id",
            "space_id",
            "title",
            "description",
            "pinned",
            "created_at",
            "updated_at",
            "deleted_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "memories",
        columns: &[
            "id",
            "space_id",
            "chapter_id",
            "text",
            "created_at",
            "updated_at",
            "deleted_at",
        ],
        apply_columns: &[
            "id",
            "space_id",
            "chapter_id",
            "text",
            "created_at",
            "updated_at",
            "deleted_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "messages",
        columns: &[
            "id",
            "session_id",
            "role",
            "content",
            "model",
            "reasoning",
            "tokens",
            "secs",
            "cost",
            "phrase",
            "persona",
            "created_at",
        ],
        apply_columns: &[
            "id",
            "session_id",
            "role",
            "content",
            "model",
            "reasoning",
            "tokens",
            "secs",
            "cost",
            "phrase",
            "persona",
            "created_at",
        ],
        pk: &["id"],
        cursor: Cursor::Tuple(&["created_at", "id"]),
    },
    TableSpec {
        name: "usage_log",
        columns: &[
            "sync_id",
            "created_at",
            "session_id",
            "space_id",
            "backend",
            "model",
            "prompt_tokens",
            "completion_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "cost",
            "cost_is_provider",
            "prompt_convention",
            "kind",
            "tools",
            "updated_at",
        ],
        apply_columns: &[
            "sync_id",
            "created_at",
            "session_id",
            "space_id",
            "backend",
            "model",
            "prompt_tokens",
            "completion_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "cost",
            "cost_is_provider",
            "prompt_convention",
            "kind",
            "tools",
            "updated_at",
        ],
        pk: &["sync_id"],
        cursor: Cursor::Tuple(&["created_at", "sync_id"]),
    },
    TableSpec {
        name: "citations",
        // `id` rides along for the cursor but is never applied.
        columns: &["id", "sync_id", "space_id", "report_file", "url", "title"],
        apply_columns: &["sync_id", "space_id", "report_file", "url", "title"],
        pk: &["sync_id"],
        cursor: Cursor::AutoId,
    },
];

pub(super) const SYNC_TOMBSTONES: &str = "sync_tombstones";

pub(super) fn spec_for(table: &str) -> Option<&'static TableSpec> {
    TABLES.iter().find(|s| s.name == table)
}

pub(super) fn known_table(table: &str) -> bool {
    table == SYNC_TOMBSTONES || spec_for(table).is_some()
}

/// JSON is opaque to transports and avoids delimiter collisions in keys such as URLs.
fn encode_updated_cursor(updated_at: &str, keys: &[String]) -> String {
    serde_json::Value::Array(vec![
        serde_json::Value::String(updated_at.to_string()),
        serde_json::Value::Array(
            keys.iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    ])
    .to_string()
}

/// Invalid cursors restart the export.
pub(super) fn decode_updated_cursor(cursor: &str, key_count: usize) -> (String, Vec<String>) {
    if let Ok(serde_json::Value::Array(parts)) = serde_json::from_str(cursor)
        && let (Some(serde_json::Value::String(updated_at)), Some(serde_json::Value::Array(keys))) =
            (parts.first(), parts.get(1))
        && parts.len() == 2
        && !updated_at.is_empty()
        && keys.len() == key_count
        && keys.iter().all(serde_json::Value::is_string)
    {
        let keys = keys
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect();
        return (updated_at.clone(), keys);
    }
    (String::new(), vec![String::new(); key_count])
}

/// Table-aware: AUTOINCREMENT tables compare numerically (string order puts "9"
/// above "10"), mutable rows compare `(updated_at, pk…)`, the rest lexically.
pub(super) fn position_gt(table: &str, a: &str, b: &str) -> bool {
    if table == "citations" || table == SYNC_TOMBSTONES {
        let n = |s: &str| s.parse::<i64>().unwrap_or(0);
        n(a) > n(b)
    } else if let Some(spec) = spec_for(table)
        && let Cursor::UpdatedAt = spec.cursor
    {
        let left = decode_updated_cursor(a, spec.pk.len());
        let right = decode_updated_cursor(b, spec.pk.len());
        left > right
    } else {
        a > b
    }
}

/// A malformed ack must not poison future exports with an impossible SQL
/// comparison or a cursor that can never be advanced.
pub(super) fn valid_cursor(table: &str, cursor: &str) -> bool {
    if table == "citations" || table == SYNC_TOMBSTONES {
        return cursor.parse::<i64>().is_ok_and(|value| value >= 0);
    }
    let Some(spec) = spec_for(table) else {
        return false;
    };
    match spec.cursor {
        Cursor::UpdatedAt => {
            if let Ok(serde_json::Value::Array(parts)) = serde_json::from_str(cursor) {
                let Some(serde_json::Value::String(updated_at)) = parts.first() else {
                    return false;
                };
                let Some(serde_json::Value::Array(keys)) = parts.get(1) else {
                    return false;
                };
                parts.len() == 2
                    && !updated_at.is_empty()
                    && keys.len() == spec.pk.len()
                    && keys.iter().all(serde_json::Value::is_string)
            } else {
                false
            }
        }
        Cursor::Tuple(_) => {
            let Some((left, right)) = cursor.split_once('|') else {
                return false;
            };
            !left.is_empty() && !right.is_empty()
        }
        Cursor::AutoId | Cursor::None => false,
    }
}

/// The pull cursor advances to the max position among received rows, win or lose
/// (a losing row was still imported).
pub(super) fn row_position(spec: &TableSpec, row: &serde_json::Value) -> String {
    match spec.cursor {
        Cursor::UpdatedAt => {
            let updated_at = row
                .get("updated_at")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let keys = spec
                .pk
                .iter()
                .map(|key| {
                    row.get(*key)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect::<Vec<_>>();
            encode_updated_cursor(updated_at, &keys)
        }
        Cursor::Tuple(cols) => cols
            .iter()
            .map(|c| {
                row.get(*c)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("|"),
        Cursor::AutoId => row
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .map_or_else(|| "0".to_string(), |i| i.to_string()),
        Cursor::None => String::new(),
    }
}

/// Names flow into `spaces/<name>/files/<name>` and hashes into
/// `blobs/by-hash/<hash>`, so they must not smuggle separators or `..`.
pub(super) fn valid_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

pub fn device_name() -> String {
    if let Ok(n) = std::env::var("NEXUS_DEVICE_NAME")
        && !n.trim().is_empty()
    {
        return n;
    }
    std::fs::read_to_string("/etc/hostname")
        .map_or_else(|_| "nexus-device".to_string(), |s| s.trim().to_string())
}
