use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;

/// Resolves the shared data directory (holding `nexus.db`) and per-space
/// directories (`spaces/<name>/`, each with `memory.md` + `instructions.md`).
pub struct Space {
    pub root: PathBuf,
}

impl Space {
    /// Resolve the data dir and create its `spaces/` directory.
    pub fn open() -> Result<Self> {
        let root = config::project_dirs()?.data_dir().to_path_buf();
        let space = Self { root };
        std::fs::create_dir_all(space.root.join("spaces"))
            .with_context(|| format!("creating {}", space.root.join("spaces").display()))?;
        Ok(space)
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("nexus.db")
    }

    /// Device-local traces beside the shared database, never synced.
    pub fn traces_dir(&self) -> PathBuf {
        self.root.join("traces")
    }

    /// Ensure `spaces/<name>/` exists, for a space just created in the db.
    pub fn ensure_space_dir(&self, name: &str) -> Result<()> {
        let dir = self.space_dir(name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))
    }

    /// `spaces/<name>/` — crate-internal (the sync engine renames space
    /// dirs when a rename syncs across).
    pub(crate) fn space_dir(&self, name: &str) -> PathBuf {
        self.root.join("spaces").join(name)
    }

    pub fn memory_path(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("memory.md")
    }

    pub fn instructions_path(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("instructions.md")
    }

    /// Comma-separated domains `search(mode=web)` always excludes in this space.
    pub fn blocked_domains_path(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("blocked_domains.txt")
    }

    /// Directory holding a space's imported fileset (created on demand).
    pub fn files_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("files")
    }

    /// Directory holding a space's model-created apps (created on demand).
    pub fn apps_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("apps")
    }

    /// Directory holding a space's reusable scripts.
    pub fn scripts_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("scripts")
    }

    /// Root of all spaces — what the app server serves from.
    pub fn spaces_root(&self) -> PathBuf {
        self.root.join("spaces")
    }

    /// Rename a space's directory (its db row is renamed separately).
    pub fn rename_space_dir(&self, old: &str, new: &str) -> Result<()> {
        let from = self.space_dir(old);
        let to = self.space_dir(new);
        if from.exists() {
            std::fs::rename(&from, &to)
                .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        }
        Ok(())
    }

    /// Remove a space's directory (memory + instructions gone with it).
    pub fn remove_space_dir(&self, name: &str) -> Result<()> {
        let dir = self.space_dir(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
    }
}
