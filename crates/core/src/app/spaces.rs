use anyhow::Result;

use super::App;
use crate::db::Space as SpaceRow;

impl App {
    pub fn switch_to_default_space(&mut self) -> Result<()> {
        let default_id = self.db.default_space_id()?;
        let row = self
            .db
            .list_spaces()?
            .into_iter()
            .find(|s| s.id == default_id)
            .ok_or_else(|| anyhow::anyhow!("default space {default_id:?} no longer exists"))?;
        self.set_active_space(row);
        Ok(())
    }

    pub fn set_active_space(&mut self, row: SpaceRow) {
        self.active_space = row;
        self.session = None;
        self.reset_work_mode();
        self.messages.clear();
        self.mask_through = 0;
        self.refresh_memory_snapshot();
        self.meter.total = None;
        self.push_viewport_reset();
        self.cleanup_incognito_images();
        self.rescan_files();
        self.refresh_toolbox();
        self.push_status(format!("space: {}", self.active_space.name));
    }

    /// Create an instructions stub if needed and return its path for editing.
    pub fn instructions_path_for_space(&self, name: &str) -> Option<std::path::PathBuf> {
        let path = self.space.instructions_path(name);
        if !path.exists() {
            let _ = std::fs::write(
                &path,
                format!("<!-- instructions for the \"{name}\" space -->\n"),
            );
        }
        Some(path)
    }
}
