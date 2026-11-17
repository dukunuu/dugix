#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::{Context, Result};

use super::{App, ImageMeta};

/// Image formats supported by the inline or system viewer.
pub fn is_image_name(name: &str) -> bool {
    let ext = std::path::Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tiff" | "tif" | "avif" | "svg"
    )
}

impl App {
    pub fn refresh_images(&mut self) {
        let dir = self.space.files_dir(&self.active_space.name);
        let _ = std::fs::create_dir_all(&dir);
        self.caches.images = match std::fs::read_dir(&dir) {
            Err(_) => Vec::new(),
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.path().is_file())
                .filter(|e| is_image_name(&e.file_name().to_string_lossy()))
                .filter_map(|e| {
                    let meta = e.metadata().ok()?;
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| {
                            chrono::DateTime::from_timestamp(
                                t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64,
                                0,
                            )
                        })
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default();
                    Some(ImageMeta {
                        name: e.file_name().to_string_lossy().to_string(),
                        size: meta.len(),
                        modified,
                    })
                })
                .collect(),
        };
        self.caches.images.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn delete_image_file(&mut self, name: &str) -> Result<bool> {
        let dir = self.space.files_dir(&self.active_space.name);
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            self.push_status(format!("removed {name}"));
            self.refresh_images();
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_image_name;

    #[test]
    fn image_names_match_only_raster_formats() {
        assert!(is_image_name("paste.png"));
        assert!(is_image_name("photo.JPG"));
        assert!(is_image_name("a.b.webp"));
        assert!(is_image_name("diagram.svg"));
        assert!(!is_image_name("notes.md"));
        assert!(!is_image_name("script.py"));
        assert!(!is_image_name("archive.tar.gz"));
        assert!(!is_image_name("README"));
        assert!(!is_image_name(".hidden"));
    }
}
