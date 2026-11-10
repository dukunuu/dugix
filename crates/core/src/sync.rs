//! The sync merge engine: changeset build/apply, per-table sync rules,
//! cursor + ack bookkeeping, and the file blob channel.
//!
//! Every table that syncs declares itself in the registry below — its
//! cursor rule (how exports resume) and its apply rule (how imports land).
//! The rules are deliberately small:
//!
//! - **LWW** (`updated_at` cursor): the incoming row wins iff
//!   `(updated_at, pk…) > (local updated_at, pk…)`. RFC3339 timestamps
//!   compare lexically, so both devices compute the same winner with no
//!   origin columns. A tie (same row written in the same nanosecond on two
//!   clocks — the clock-skew residual) keeps the local row on both sides;
//!   it is stable and never flip-flops.
//! - **Append** (`(created_at, id)` tuple or AUTOINCREMENT cursor):
//!   INSERT OR IGNORE by the row's sync identity (uuid id, or `sync_id`
//!   for the AUTOINCREMENT tables). Idempotent union — the exactly-once
//!   backstop when a changeset is re-imported after a crash.
//! - **Tombstones** (AUTOINCREMENT cursor): applied destructively — a
//!   session tombstone cascades its messages and sources, a space
//!   tombstone removes the space row and its directory, a file tombstone
//!   removes the row and the local blob.
//! - **`swarm_personas`** has no cursor of its own: it is versioned by the
//!   owning session. Persona rows travel attached to
//!   their session row and are applied only when that session row wins
//!   LWW, as a wholesale roster replace.
//!
//! Cursor lifecycle (the ack design): an export does not advance
//! `push_cursor` — the receiver imports, then replies with an ack
//! carrying its new pull cursors, and only then does the sender advance
//! `push_cursor`. Idempotent apply is the backstop for crashes mid-
//! exchange.

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

// ── changeset types ──

/// One device's export, possibly carrying an ack for the device it was
/// sent to. Serde JSON over the wire (or inside a zip bundle with blobs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Changeset {
    /// The sender (from `device_meta`, created on first sync).
    pub device_id: String,
    /// "laptop" — for the receiver's log/status.
    pub device_name: String,
    /// "I imported up to here" — the sender's reply acks the *receiver*'s
    /// data, so entries are addressed by the receiver's device id.
    pub ack: Option<Vec<PeerCursor>>,
    /// Full rows as JSON objects keyed by column name.
    pub rows: Vec<RowChange>,
    /// Deletes that actually happened on the sender.
    pub tombstones: Vec<Tombstone>,
    /// File metadata — the blobs themselves follow via the transport's
    /// blob channel (`blobs/<space_id>/<name>` plus the content-addressed
    /// `blobs/by-hash/<hash>` path under a sync dir or inside a zip bundle).
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
    /// The sender's `sync_tombstones` AUTOINCREMENT id — cursor
    /// bookkeeping only, never applied.
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
    /// The device whose data was imported up to `cursor` — the ack's
    /// target, matched against the receiver's own device id.
    pub peer_id: String,
    pub table_name: String,
    pub cursor: String,
}

/// What applying a changeset did — the receiver's log line, plus the
/// blobs the transport still has to fetch.
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
