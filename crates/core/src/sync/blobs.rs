//! The file blob channel and the zip bundles the ssh transport carries it in.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use sha2::{Digest as _, Sha256};

use crate::db::Db;
use crate::space::Space;

use super::apply::space_name_for;
use super::registry::valid_component;
use super::{Changeset, FileChange};

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().fold(String::new(), |mut h, b| {
        let _ = std::fmt::Write::write_fmt(&mut h, format_args!("{b:02x}"));
        h
    })
}

/// Store one transport blob after checking that its manifest row exists, the
/// declared size matches and the content hash is exact: an upload can never create
/// a new file row or escape its space.
pub fn put_blob(
    db: &Db,
    space: &Space,
    space_id: &str,
    name: &str,
    hash: &str,
    bytes: &[u8],
) -> Result<()> {
    if !valid_component(space_id) || !valid_component(name) {
        bail!("unsafe blob path");
    }
    let Some(space_name) = space_name_for(db, space_id)? else {
        bail!("unknown space");
    };
    let row = db
        .list_files(space_id)?
        .into_iter()
        .find(|file| file.name == name)
        .ok_or_else(|| anyhow!("unknown file manifest"))?;
    if row.hash != hash {
        bail!("blob hash does not match the current file manifest");
    }
    if row.size < 0 || row.size as usize != bytes.len() {
        bail!("blob size does not match the current file manifest");
    }
    if sha256_hex(bytes) != hash {
        bail!("blob content hash mismatch");
    }
    let target = space.files_dir(&space_name).join(name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let temporary = target.with_extension(format!("nexus-upload-{}", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, bytes)
        .with_context(|| format!("writing blob {}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("installing blob {}", target.display()));
    }
    Ok(())
}

/// A missing or stale local file is `None`, never an unverified byte stream.
pub fn read_blob(db: &Db, space: &Space, space_id: &str, name: &str) -> Result<Option<Vec<u8>>> {
    if !valid_component(space_id) || !valid_component(name) {
        return Ok(None);
    }
    let Some(space_name) = space_name_for(db, space_id)? else {
        return Ok(None);
    };
    let Some(row) = db
        .list_files(space_id)?
        .into_iter()
        .find(|file| file.name == name)
    else {
        return Ok(None);
    };
    let path = space.files_dir(&space_name).join(name);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(
        (row.size >= 0 && row.size as usize == bytes.len() && sha256_hex(&bytes) == row.hash)
            .then_some(bytes),
    )
}

pub(super) fn file_matches(path: &Path, hash: &str) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    sha256_hex(&bytes) == hash
}

/// The blob's local path, when the space still exists and the name is a valid
/// path component.
fn blob_source_path(db: &Db, space: &Space, fc: &FileChange) -> Result<Option<PathBuf>> {
    if !valid_component(&fc.name) || !valid_component(&fc.space_id) {
        return Ok(None);
    }
    let Some(space_name) = space_name_for(db, &fc.space_id)? else {
        return Ok(None);
    };
    let path = space.files_dir(&space_name).join(&fc.name);
    // Never publish a disk payload that does not match the durable manifest: it
    // only guarantees a failed import and can overwrite a valid mailbox blob.
    Ok((path.is_file() && file_matches(&path, &fc.hash)).then_some(path))
}

/// Install with a rename so a reader never observes a partial blob, which is what
/// makes a mailbox changeset safe to read right after its blobs are exported.
fn copy_atomic(source: &Path, target: &Path) -> Result<()> {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("blob");
    let temporary = target.with_file_name(format!(".{name}.nexus-copy-{}", uuid::Uuid::new_v4()));
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if let Err(error) = std::fs::copy(source, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| {
            format!(
                "copying blob {} to {}",
                source.display(),
                temporary.display()
            )
        });
    }
    let result = match std::fs::rename(&temporary, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(target).and_then(|()| std::fs::rename(&temporary, target))
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("installing blob {}", target.display()));
    }
    Ok(())
}

/// Copy the manifest's local blobs into `dest/blobs/by-hash/`, the transport's
/// channel; identical content is never re-sent because the receiver hash-checks.
pub fn export_blobs(db: &Db, space: &Space, cs: &Changeset, dest: &Path) -> Result<usize> {
    let mut n = 0usize;
    for fc in &cs.files {
        let Some(src) = blob_source_path(db, space, fc)? else {
            continue;
        };
        if valid_component(&fc.hash) {
            let by_hash = dest.join("blobs").join("by-hash").join(&fc.hash);
            copy_atomic(&src, &by_hash)
                .with_context(|| format!("exporting content-addressed blob {}", src.display()))?;
        }
        n += 1;
    }
    Ok(n)
}

/// Pull one blob from `src/blobs/by-hash/<hash>` into `target` after verifying its
/// content hash: a stale or mismatched payload is never applied.
pub(super) fn pull_blob(src: &Path, hash: &str, target: &Path) -> Result<bool> {
    let candidate = valid_component(hash)
        .then(|| src.join("blobs").join("by-hash").join(hash))
        .filter(|path| file_matches(path, hash));
    let Some(candidate) = candidate else {
        return Ok(false);
    };
    copy_atomic(&candidate, target)?;
    Ok(true)
}

/// Write a changeset plus its blobs as a zip (`changeset.json` and
/// `blobs/by-hash/<hash>` entries); returns how many blobs were included.
pub fn write_bundle(
    db: &Db,
    space: &Space,
    cs: &Changeset,
    writer: impl std::io::Write + std::io::Seek,
) -> Result<usize> {
    let mut zip = zip::ZipWriter::new(writer);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("changeset.json", opts)?;
    serde_json::to_writer(&mut zip, cs)?;
    let mut n = 0usize;
    let mut content_hashes = HashSet::new();
    for fc in &cs.files {
        let Some(src) = blob_source_path(db, space, fc)? else {
            continue;
        };
        if valid_component(&fc.hash) && content_hashes.insert(fc.hash.clone()) {
            zip.start_file(format!("blobs/by-hash/{}", fc.hash), opts)?;
            let mut content = std::fs::File::open(&src)?;
            std::io::copy(&mut content, &mut zip)?;
        }
        n += 1;
    }
    zip.finish()?;
    Ok(n)
}

/// Unpack a bundle into `dest_dir` (as `blobs/…`), returning the changeset. Entry
/// paths are validated: a bundle must never write outside `dest_dir`.
pub fn unpack_bundle(src: &Path, dest_dir: &Path) -> Result<Changeset> {
    let file = std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| format!("reading {}", src.display()))?;
    let mut changeset: Option<Changeset> = None;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.into_owned();
        if name == "changeset.json" {
            changeset = Some(serde_json::from_reader(&mut entry)?);
            continue;
        }
        let Some(rel) = name.strip_prefix("blobs/") else {
            continue;
        };
        let components: Vec<&str> = rel.split('/').collect();
        if components.len() != 2 || !components.iter().all(|c| valid_component(c)) {
            bail!("unsafe path in bundle: {name}");
        }
        if components[0] != "by-hash" {
            continue;
        }
        let dest = dest_dir.join("blobs").join("by-hash").join(components[1]);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut out =
            std::fs::File::create(&dest).with_context(|| format!("creating {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)?;
    }
    changeset.ok_or_else(|| anyhow!("bundle has no changeset.json"))
}
