//! Saving and restoring a panel's state, plus the pure helpers the saved-panel
//! and `OpenUrl` surfaces share; nothing here touches a database or the screen.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use super::{Session, ViewSnapshot, https_url};

/// Largest encoded record that is saved or accepted back. Records can arrive
/// from a synced peer, so the bound applies on both sides.
pub const MAX_RECORD: usize = 48 * 1024;
/// The only record format written; newer formats are rejected, not guessed at.
const FORMAT: u8 = 1;
const MAX_HELD: usize = 32;

/// Everything needed to bring a panel back exactly as the person left it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelSnapshot {
    /// Record format version.
    pub v: u8,
    /// Scalar and structured UI state, by declared name.
    #[serde(default)]
    pub state: Map<String, Value>,
    /// Last successful query results by statement id; never mutation status.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub queries: BTreeMap<String, Value>,
    /// Selected tabs, open sections, table sort/page/filter.
    #[serde(default)]
    pub view: ViewSnapshot,
}

impl PanelSnapshot {
    /// Serialize within [`MAX_RECORD`], dropping the query cache first.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        let full = serde_json::to_string(self).ok()?;
        if full.len() <= MAX_RECORD {
            return Some(full);
        }
        let lean = Self {
            queries: BTreeMap::new(),
            ..self.clone()
        };
        serde_json::to_string(&lean)
            .ok()
            .filter(|text| text.len() <= MAX_RECORD)
    }

    /// Parse a stored record, rejecting oversized, malformed and newer formats.
    pub fn decode(text: &str) -> Result<Self> {
        ensure!(text.len() <= MAX_RECORD, "saved panel state is too large");
        let snapshot: Self = serde_json::from_str(text)?;
        match snapshot.v {
            FORMAT => Ok(snapshot),
            newer if newer > FORMAT => bail!("saved panel state is from a newer version"),
            _ => bail!("unknown saved panel state version"),
        }
    }
}

impl Session {
    #[must_use]
    pub fn snapshot(&self) -> PanelSnapshot {
        let mut state = self.state.clone();
        for (key, value) in &self.held {
            state.entry(key.clone()).or_insert_with(|| value.clone());
        }
        PanelSnapshot {
            v: FORMAT,
            state,
            queries: self.live.query_data(&self.document.calls),
            view: self.view_snapshot(),
        }
    }

    /// Re-apply a saved state; undeclared names are held for a replayed patch,
    /// and a validation failure leaves the panel exactly as it was.
    pub fn restore(&mut self, snapshot: &PanelSnapshot) -> Result<()> {
        ensure!(snapshot.v == FORMAT, "unsupported saved panel state");
        let mut state = self.state.clone();
        let mut held = Map::new();
        for (key, value) in &snapshot.state {
            if let Some(default) = self.defaults.get(key) {
                ensure!(
                    same_kind(default, value),
                    "saved value for {key} no longer fits the program"
                );
                state.insert(key.clone(), value.clone());
            } else {
                held.insert(key.clone(), value.clone());
            }
        }
        ensure!(
            state.len() + held.len() <= MAX_HELD,
            "too many saved UI state fields — maximum: {MAX_HELD}"
        );
        self.bindings_ok(&mut state)?;
        let before = self.live.query_data(&self.document.calls);
        self.live
            .set_query_data(&self.document.calls, &snapshot.queries);
        if let Err(error) = self.commit(state) {
            self.live.set_query_data(&self.document.calls, &before);
            return Err(error);
        }
        self.held = held;
        self.restore_view(&snapshot.view);
        Ok(())
    }

    /// Counter that changes whenever state or presentation changes.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The current program text (after any patches); `None` for legacy JSON.
    #[must_use]
    pub fn program(&self) -> Option<&str> {
        self.document.program.as_deref()
    }

    /// Host (with a non-default port) of the URL awaiting confirmation.
    #[must_use]
    pub fn pending_host(&self) -> Option<String> {
        self.pending_url().and_then(|url| url_host(url).ok())
    }
}

/// Whether a saved value is the same kind of thing the program declares. A
/// `null` default is untyped and accepts anything.
fn same_kind(default: &Value, saved: &Value) -> bool {
    matches!(
        (default, saved),
        (Value::Null, _)
            | (Value::Bool(_), Value::Bool(_))
            | (Value::Number(_), Value::Number(_))
            | (Value::String(_), Value::String(_))
            | (Value::Array(_), Value::Array(_))
            | (Value::Object(_), Value::Object(_))
    )
}

/// Stable identity of a panel within one conversation: a hash of its source
/// plus its occurrence. It survives compaction and is the same on every device.
#[must_use]
pub fn panel_key(source: &str, occurrence: usize) -> String {
    let digest = Sha256::digest(source.as_bytes());
    let mut hex = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{hex}-{occurrence}")
}

/// The host a validated HTTPS URL points at, lowercased and punycoded, with
/// the port appended when it is not 443; this is what "always allow for this host" keys on.
pub fn url_host(url: &str) -> Result<String> {
    https_url(url)?;
    let parsed = reqwest::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    Ok(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}
