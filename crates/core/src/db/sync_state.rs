//! Sync identity and per-peer cursors.

use anyhow::Result;
use chrono::Utc;
use rusqlite::OptionalExtension;
use uuid::Uuid;

use super::Db;

/// One peer's sync cursor for one table; cursors are opaque strings,
/// `(created_at, id)` for append-only tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    pub peer_id: String,
    pub table_name: String,
    pub pull_cursor: Option<String>,
    pub push_cursor: Option<String>,
    pub last_synced_at: Option<String>,
}

impl Db {
    /// This device's stable sync id, created on first use.
    pub fn device_id(&self) -> Result<String> {
        if let Some(id) = self
            .conn
            .query_row("SELECT device_id FROM device_meta LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()?
        {
            return Ok(id);
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO device_meta (device_id, created_at) VALUES (?1, ?2)",
            (&id, &now),
        )?;
        Ok(id)
    }

    /// Store a peer's cursors for one table; `None` leaves an existing cursor
    /// untouched.
    pub fn set_sync_state(
        &self,
        peer_id: &str,
        table_name: &str,
        pull_cursor: Option<&str>,
        push_cursor: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_state (peer_id, table_name, pull_cursor, push_cursor, last_synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(peer_id, table_name) DO UPDATE SET
                pull_cursor = COALESCE(?3, pull_cursor),
                push_cursor = COALESCE(?4, push_cursor),
                last_synced_at = ?5",
            (
                peer_id,
                table_name,
                pull_cursor,
                push_cursor,
                Utc::now().to_rfc3339(),
            ),
        )?;
        Ok(())
    }

    pub fn load_sync_state(&self) -> Result<Vec<SyncState>> {
        let mut stmt = self.conn.prepare(
            "SELECT peer_id, table_name, pull_cursor, push_cursor, last_synced_at
             FROM sync_state ORDER BY peer_id, table_name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SyncState {
                peer_id: r.get(0)?,
                table_name: r.get(1)?,
                pull_cursor: r.get(2)?,
                push_cursor: r.get(3)?,
                last_synced_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
