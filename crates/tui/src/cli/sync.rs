//! `nexus sync` — changeset exchange over a shared directory or ssh.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use nexus_core::db;
use nexus_core::space;

use super::{SyncCmd, out, short_id};

// --- sync commands ---

/// Temporary workspace for bundle extraction and SSH replies. Cleanup runs
/// on every return path, including malformed bundles and failed SSH calls.
struct SyncTempDir {
    path: PathBuf,
}

impl SyncTempDir {
    fn new(prefix: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("creating temporary sync directory {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SyncTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `nexus sync` dispatch: the export/import subcommands, or the positional
/// target — a shared dir (mailbox sync) or `ssh://user@host`.
pub(super) fn sync_cmd(cmd: Option<SyncCmd>, target: Option<&Path>) -> Result<()> {
    match cmd {
        Some(SyncCmd::Export { peer, dir }) => sync_export(peer.as_deref(), dir.as_deref()),
        Some(SyncCmd::Import { path, bundle }) => sync_import(path.as_deref(), bundle),
        None => match target {
            Some(t) => {
                let spec = t.to_string_lossy();
                if let Some(host) = spec.strip_prefix("ssh://") {
                    sync_ssh(host)
                } else if spec.contains('@') && !spec.contains('/') && !spec.contains('\\') {
                    sync_ssh(&spec)
                } else {
                    sync_dir(t)
                }
            }
            None => bail!(
                "pass a sync directory, `ssh://user@host`, or a subcommand — `nexus sync --help`"
            ),
        },
    }
}

/// `nexus sync export`: the changeset JSON on stdout; with `--dir`, the
/// blobs go into `DIR/blobs/` too. The cursor advance waits for the peer's
/// ack (see the roadmap's ack design).
fn sync_export(peer: Option<&str>, dir: Option<&Path>) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let cs = nexus_core::sync::build_changeset(&db, peer, &nexus_core::sync::device_name())?;
    if let Some(dir) = dir {
        let n = nexus_core::sync::export_blobs(&db, &space, &cs, dir)?;
        eprintln!("wrote {n} blob(s) to {}", dir.join("blobs").display());
    }
    eprintln!(
        "exported {} rows, {} tombstones, {} files{}",
        cs.rows.len(),
        cs.tombstones.len(),
        cs.files.len(),
        match peer {
            Some(p) => format!(" for peer {p}"),
            None => " (full export — first sync?)".to_string(),
        }
    );
    out(serde_json::to_string(&cs)?);
    Ok(())
}

/// `nexus sync import`: apply a changeset (or zip bundle) from a file or
/// stdin, then print the reply — this device's export + ack — on stdout.
/// The ssh transport pipes a bundle in and expects a bundle out
/// (`--bundle`); plain JSON is the default for interactive use.
fn sync_import(path: Option<&Path>, bundle_reply: bool) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let bytes = if let Some(p) = path {
        std::fs::read(p).with_context(|| format!("reading {}", p.display()))?
    } else {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        buf
    };
    let tmp = SyncTempDir::new("nexus-import")?;
    let (cs, blob_source) = if bytes.starts_with(b"PK") {
        let bundle = tmp.path().join("in.bundle");
        std::fs::write(&bundle, &bytes)?;
        let unpacked = tmp.path().join("unpacked");
        let cs = nexus_core::sync::unpack_bundle(&bundle, &unpacked)?;
        (cs, Some(unpacked))
    } else {
        (
            serde_json::from_slice(&bytes).context("parsing changeset JSON")?,
            None,
        )
    };
    let (summary, cursors) =
        nexus_core::sync::apply_changeset(&db, &space, &cs, blob_source.as_deref())?;
    print_sync_summary(&cs, &summary);
    let mut reply = nexus_core::sync::build_changeset(
        &db,
        Some(&cs.device_id),
        &nexus_core::sync::device_name(),
    )?;
    reply.ack = Some(cursors);
    if bundle_reply {
        // Zip writers need a seekable sink — write the reply bundle to a
        // temp file, then stream it out.
        let reply_path = tmp.path().join("reply.bundle");
        nexus_core::sync::write_bundle(&db, &space, &reply, std::fs::File::create(&reply_path)?)?;
        std::io::copy(
            &mut std::fs::File::open(&reply_path)?,
            &mut std::io::stdout(),
        )?;
    } else {
        out(serde_json::to_string(&reply)?);
    }
    Ok(())
}

/// The import's log line — stderr, because stdout carries the reply.
fn print_sync_summary(cs: &nexus_core::sync::Changeset, summary: &nexus_core::sync::ApplySummary) {
    eprintln!(
        "imported from {} ({}) — {} rows applied, {} skipped, {} tombstones, {} acks, {} files kept, {} pulled, {} missing",
        short_id(&cs.device_id),
        cs.device_name,
        summary.rows_applied,
        summary.rows_skipped,
        summary.tombstones_applied,
        summary.acks_applied,
        summary.files_kept,
        summary.files_pulled,
        summary.files_missing.len(),
    );
    for w in &summary.warnings {
        eprintln!("  warning: {w}");
    }
}

/// Write a mailbox file through a temporary sibling and rename it into place.
/// Readers either see the previous complete changeset or the new complete
/// one, never a half-written JSON document.
fn write_sync_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("changeset.json");
    let temporary = path.with_file_name(format!(".{name}.nexus-write-{}", uuid::Uuid::new_v4()));
    if let Err(error) = std::fs::write(&temporary, bytes) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("writing {}", temporary.display()));
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("installing {}", path.display()));
    }
    Ok(())
}

/// Bidirectional mailbox sync over a shared directory: consume every
/// foreign `*.changeset.json` (rows, tombstones, embedded acks, blobs),
/// then leave this device's own changeset + blobs + ack behind for the
/// peer to pick up. Works on a shared/USB/scp'd dir.
fn sync_dir(dir: &Path) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let my_id = db.device_id()?;
    let own_path = dir.join(format!("{my_id}.changeset.json"));
    // Our previous reply is still an outbound message until a peer imports
    // it. Preserve its ack when this device is run again before that peer;
    // dropping the file would silently lose the only copy of the ack.
    let pending_ack = if own_path.is_file() {
        let bytes = std::fs::read(&own_path)
            .with_context(|| format!("reading pending {}", own_path.display()))?;
        let pending: nexus_core::sync::Changeset = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing pending {}", own_path.display()))?;
        if pending.device_id != my_id {
            bail!(
                "mailbox file {} belongs to another device",
                own_path.display()
            );
        }
        pending.ack
    } else {
        None
    };
    let mut reply_cursors = pending_ack.unwrap_or_default();
    let mut senders: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !name.ends_with(".changeset.json") {
            continue;
        }
        let cs: nexus_core::sync::Changeset = serde_json::from_slice(&std::fs::read(entry.path())?)
            .with_context(|| format!("parsing {}", entry.path().display()))?;
        if cs.device_id == my_id {
            // Keep our outbound file until a peer consumes it. It is read
            // above only to preserve its ack; it is replaced atomically below.
            continue;
        }
        let (summary, cursors) = nexus_core::sync::apply_changeset(&db, &space, &cs, Some(dir))?;
        print_sync_summary(&cs, &summary);
        reply_cursors.extend(cursors);
        if !senders.contains(&cs.device_id) {
            senders.push(cs.device_id);
        }
        // Consume the changeset. Blob files are retained as a bounded
        // per-name mailbox cache: deleting them here races with another
        // device's outbound export when peers share one directory, while
        // hash verification makes stale payloads harmless.
        std::fs::remove_file(entry.path())?;
    }
    // Our reply: an export for whoever we just imported from (a full
    // export on a cold dir) plus the ack of what we took in. The peer is
    // the mail's sender, not this run's reply cursors — on an ack-only
    // round the cursors are empty but the push cursors were just advanced.
    // With no mail at all (a solo re-run), fall back to the peer we most
    // recently synced with so we don't blast a full export.
    // A single changeset can carry an ack for many senders, but its row
    // export can only be delta-filtered for one peer. When several peers
    // posted mail in the same pass, use a full export so none of them gets
    // its push cursor acknowledged without receiving this device's rows.
    let peer: Option<String> = match senders.as_slice() {
        [peer] if reply_cursors.is_empty() => Some(peer.clone()),
        [] if reply_cursors.is_empty() => db
            .load_sync_state()?
            .iter()
            .filter(|s| s.push_cursor.is_some())
            .max_by(|a, b| a.last_synced_at.cmp(&b.last_synced_at))
            .map(|s| s.peer_id.clone()),
        // A pending ack means another outbound exchange is still in flight;
        // use a full export so replacing the mailbox file cannot drop rows
        // associated with that pending message.
        _ => None,
    };
    let mut cs =
        nexus_core::sync::build_changeset(&db, peer.as_deref(), &nexus_core::sync::device_name())?;
    cs.ack = Some(reply_cursors);
    let path = dir.join(format!("{my_id}.changeset.json"));
    let encoded = serde_json::to_vec(&cs)?;
    let blobs = nexus_core::sync::export_blobs(&db, &space, &cs, dir)?;
    write_sync_file(&path, &encoded)?;
    eprintln!(
        "exported {} rows, {} tombstones, {blobs} blob(s) → {}",
        cs.rows.len(),
        cs.tombstones.len(),
        path.display()
    );
    Ok(())
}

/// Sync over ssh: bundle this device's export (+ blobs) into a zip, pipe
/// it into `ssh host "nexus sync import --bundle"`, and apply the reply
/// bundle (the peer's export + blobs + ack). The peer's device id is
/// remembered in the local `sync_ssh_peer` setting — per-peer cursors
/// live under it.
fn sync_ssh(host: &str) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let remote_peer: Option<String> = db
        .load_settings()?
        .into_iter()
        .find(|(k, _)| k == "sync_ssh_peer")
        .map(|(_, v)| v);
    let mut cs = nexus_core::sync::build_changeset(
        &db,
        remote_peer.as_deref(),
        &nexus_core::sync::device_name(),
    )?;
    // Carry our pull cursors for the remote — without the ack it would
    // re-export everything on every round.
    if let Some(peer) = &remote_peer {
        cs.ack = Some(nexus_core::sync::build_ack(&db, peer)?);
    }
    let tmp = SyncTempDir::new("nexus-ssh")?;
    let bundle = tmp.path().join("out.bundle");
    let n = nexus_core::sync::write_bundle(&db, &space, &cs, std::fs::File::create(&bundle)?)?;
    eprintln!(
        "exported {} rows, {} tombstones, {n} blob(s)",
        cs.rows.len(),
        cs.tombstones.len()
    );
    let output = std::process::Command::new("ssh")
        .arg(host)
        .args(["nexus", "sync", "import", "--bundle"])
        .stdin(std::process::Stdio::from(std::fs::File::open(&bundle)?))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("running ssh {host}"))?;
    if !output.status.success() {
        bail!(
            "ssh {host} failed ({}) — is `nexus` installed there?",
            output.status
        );
    }
    let reply_path = tmp.path().join("reply.bundle");
    std::fs::write(&reply_path, &output.stdout)?;
    let unpacked = tmp.path().join("unpacked");
    let reply = nexus_core::sync::unpack_bundle(&reply_path, &unpacked)?;
    let (summary, _) = nexus_core::sync::apply_changeset(&db, &space, &reply, Some(&unpacked))?;
    print_sync_summary(&reply, &summary);
    db.set_setting("sync_ssh_peer", &reply.device_id)?;
    Ok(())
}
