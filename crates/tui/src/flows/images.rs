#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::Result;

use nexus_core::app::ImagesMode;

use crate::app_view::AppView;

impl AppView {
    pub fn move_images_selection(&mut self, delta: i32) {
        self.images_selected = nexus_core::app::clamp_cursor(
            self.images_selected,
            self.core.caches.images.len(),
            delta,
        );
    }

    pub fn open_selected_image(&mut self) {
        let Some(img) = self.core.caches.images.get(self.images_selected).cloned() else {
            return;
        };
        let path = self
            .core
            .space
            .files_dir(&self.core.active_space.name)
            .join(&img.name);
        self.open_image_target(
            &path.to_string_lossy(),
            &crate::image_viewer::Anchor::ByTarget,
            crate::image_viewer::Return::Files,
        );
        if self.popup != nexus_core::app::Popup::ImageViewer {
            self.push_status(format!("opened {}", img.name));
        }
    }

    pub fn confirm_images_delete(&mut self) -> Result<()> {
        if let Some(img) = self.core.caches.images.get(self.images_selected).cloned() {
            self.core.delete_image_file(&img.name)?;
        }
        self.images_mode = ImagesMode::Browse;
        Ok(())
    }
}
