use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;

/// The shared data directory (holding `dugix.db`) and per-space directories.
pub struct Space {
    pub root: PathBuf,
}

impl Space {
    pub fn open() -> Result<Self> {
        let root = config::project_dirs()?.data_dir().to_path_buf();
        let space = Self { root };
        std::fs::create_dir_all(space.root.join("spaces"))
            .with_context(|| format!("creating {}", space.root.join("spaces").display()))?;
        Ok(space)
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join("dugix.db")
    }

    /// Device-local traces beside the shared database, never synced.
    pub fn traces_dir(&self) -> PathBuf {
        self.root.join("traces")
    }

    pub fn ensure_space_dir(&self, name: &str) -> Result<()> {
        let dir = self.space_dir(name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))
    }

    /// Crate-internal: the sync engine renames space dirs when a rename syncs across.
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

    pub fn files_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("files")
    }

    pub fn apps_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("apps")
    }

    pub fn scripts_dir(&self, name: &str) -> PathBuf {
        self.space_dir(name).join("scripts")
    }

    /// What the app server serves from.
    pub fn spaces_root(&self) -> PathBuf {
        self.root.join("spaces")
    }

    /// The db row is renamed separately.
    pub fn rename_space_dir(&self, old: &str, new: &str) -> Result<()> {
        let from = self.space_dir(old);
        let to = self.space_dir(new);
        if from.exists() {
            std::fs::rename(&from, &to)
                .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        }
        Ok(())
    }

    pub fn remove_space_dir(&self, name: &str) -> Result<()> {
        let dir = self.space_dir(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        Ok(())
    }
}
