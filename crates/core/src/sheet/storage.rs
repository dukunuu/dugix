//! Confined sheet JSON files and shared, content-addressed undo history.
use super::Sheet;
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

/// Resolve a non-hidden `sheets/…/*.sheet.json` path without following symlinks.
pub fn confined(files: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(
        relative.starts_with("sheets/") && relative.ends_with(".sheet.json"),
        "path must look like sheets/name.sheet.json"
    );
    crate::design::storage::confined(files, relative)
}

/// Load and validate a sheet inside a space's files directory.
pub fn load(files: &Path, relative: &str) -> Result<Sheet> {
    let path = confined(files, relative)?;
    ensure!(
        std::fs::metadata(&path)?.len() <= super::validate::MAX_SOURCE as u64,
        "sheet exceeds 32 MiB"
    );
    Sheet::check(&std::fs::read_to_string(path)?).map_err(|e| anyhow::anyhow!("{e:?}"))
}

/// Validate, keep the previous revision in history, then write atomically.
pub fn save(
    files: &Path,
    relative: &str,
    sheet: &Sheet,
    who: crate::deck::history::Who,
) -> Result<()> {
    let path = confined(files, relative)?;
    let sheet = super::validate::checked(sheet)?;
    let bytes = serde_json::to_vec_pretty(&sheet)?;
    ensure!(
        bytes.len() <= super::validate::MAX_SOURCE,
        "sheet exceeds 32 MiB"
    );
    crate::deck::history::record_before_write_for::<Sheet>(&path, &sheet.revision(), who)?;
    crate::deck::write_atomic(&path, &bytes)
}
