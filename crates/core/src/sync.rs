//! The sync merge engine: changeset build/apply, per-table sync rules, cursor and
//! ack bookkeeping, and the file blob channel.

// Casts here are on bounded values: byte sizes, row counts, ordinals — the
// same justification as db.rs.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

mod apply;
mod blobs;
mod export;
mod registry;

pub use apply::apply_changeset;
pub use blobs::{export_blobs, put_blob, read_blob, unpack_bundle, write_bundle};
pub use export::{build_ack, build_changeset};
pub use registry::{Cursor, TABLES, TableSpec, device_name};

#[cfg(test)]
use blobs::sha256_hex;
#[cfg(test)]
use export::{select_rows, short_id};
#[cfg(test)]
use registry::{position_gt, row_position, spec_for, valid_component};

#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::db::DEFAULT_SPACE;
#[cfg(test)]
use crate::space::Space;

/// One device's export, possibly carrying an ack for the device it was sent to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Changeset {
    /// The sender's durable database schema version; must match the receiver's.
    #[serde(default)]
    pub schema: i64,
    pub device_id: String,
    pub device_name: String,
    /// "I imported up to here" — entries are addressed by the receiver's device id.
    pub ack: Option<Vec<PeerCursor>>,
    pub rows: Vec<RowChange>,
    pub tombstones: Vec<Tombstone>,
    /// File metadata; the blobs follow via the transport's blob channel.
    pub files: Vec<FileChange>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RowChange {
    pub table: String,
    pub row: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tombstone {
    /// The sender's `sync_tombstones` AUTOINCREMENT id: cursor bookkeeping only,
    /// never applied.
    pub origin_id: i64,
    pub table_name: String,
    pub row_id: String,
    pub deleted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileChange {
    pub space_id: String,
    pub name: String,
    pub hash: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerCursor {
    /// The device whose data was imported up to `cursor`, matched against the
    /// receiver's own device id.
    pub peer_id: String,
    pub table_name: String,
    pub cursor: String,
}

/// The receiver's log line, plus the blobs the transport still has to fetch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApplySummary {
    pub rows_applied: usize,
    pub rows_skipped: usize,
    pub tombstones_applied: usize,
    pub acks_applied: usize,
    pub files_kept: usize,
    pub files_pulled: usize,
    pub files_missing: Vec<FileChange>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests;
