//! Space files as deck images: a checked image is copied into the deck's `assets/`
//! folder under a content-addressed name.

use std::fmt::Write as _;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};
use image::ImageFormat;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::write_atomic;

pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SIDE: u32 = 16_384;

/// Call `f` on every image `src` a value carries.
pub fn visit_srcs(value: &mut Value, f: &mut dyn FnMut(&mut String) -> Result<()>) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                visit_srcs(item, f)?;
            }
        }
        Value::Object(map) => {
            if let Some(Value::String(image)) = map
                .get_mut("background")
                .and_then(|background| background.get_mut("image"))
            {
                f(image)?;
            }
            let is_image = map.get("kind").and_then(Value::as_str) == Some("image");
            let is_patch = map.get("op").and_then(Value::as_str) == Some("update_node");
            if is_image && let Some(Value::String(src)) = map.get_mut("src") {
                f(src)?;
            }
            if is_patch
                && let Some(Value::String(src)) =
                    map.get_mut("fields").and_then(|f| f.get_mut("src"))
            {
                f(src)?;
            }
            for inner in map.values_mut() {
                if inner.is_array() || inner.is_object() {
                    visit_srcs(inner, f)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn sanitized_stem(stem: &str) -> String {
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    let clean = clean.trim_matches('-').to_string();
    if clean.is_empty() {
        "image".into()
    } else {
        clean
    }
}

fn extension(format: ImageFormat) -> Option<&'static str> {
    match format {
        ImageFormat::Png => Some("png"),
        ImageFormat::Jpeg => Some("jpg"),
        ImageFormat::WebP => Some("webp"),
        _ => None,
    }
}

/// The space file `src` names; symlinks and `..` must not leave the space files.
#[must_use]
pub fn find_space_file(src: &str, space_files: &Path) -> Option<std::path::PathBuf> {
    let cleaned = src.trim().trim_start_matches("./");
    let root = space_files.canonicalize().ok()?;
    let candidates = [
        Some(cleaned),
        cleaned.strip_prefix("files/"),
        cleaned.strip_prefix(&format!("{}/", root.display())),
    ];
    for candidate in candidates.into_iter().flatten() {
        let joined = if Path::new(candidate).is_absolute() {
            Path::new(candidate).to_path_buf()
        } else {
            root.join(candidate)
        };
        if let Ok(real) = joined.canonicalize()
            && real.starts_with(&root)
            && real.is_file()
        {
            return Some(real);
        }
    }
    None
}

/// Resolve an image `src` to something a deck can store; `Ok(None)` keeps it as
/// written, `Ok(Some(path))` is the `assets/…` copy of an imported space file.
///
/// # Errors
/// When the file exists but is not an acceptable image: not an image by
/// content, too large, or outside the space files.
pub fn import_space_image(src: &str, space_files: &Path, assets: &Path) -> Result<Option<String>> {
    if src.starts_with("https://") || src.starts_with("data:") || src.contains('\0') {
        return Ok(None);
    }
    let cleaned = src.trim().trim_start_matches("./");
    if let Some(inside) = cleaned.strip_prefix("assets/")
        && assets.join(inside).is_file()
    {
        return Ok(None);
    }
    let Some(file) = find_space_file(cleaned, space_files) else {
        return Ok(None);
    };
    if let Ok(assets_real) = assets.canonicalize()
        && let Ok(inside) = file.strip_prefix(&assets_real)
    {
        return Ok(Some(format!(
            "assets/{}",
            inside.to_string_lossy().replace('\\', "/")
        )));
    }
    let length = std::fs::metadata(&file)?.len();
    ensure!(
        length <= MAX_IMAGE_BYTES,
        "{src} is {} MB; images from the space files can be at most {} MB",
        length.div_ceil(1024 * 1024),
        MAX_IMAGE_BYTES / (1024 * 1024)
    );
    let bytes = std::fs::read(&file).with_context(|| format!("could not read {src}"))?;
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    store_image_bytes(&bytes, src, &stem, assets).map(Some)
}

/// Store `bytes` in the deck's `assets/` under a content-addressed name; the same
/// bytes always give the same path. `label` names the source in errors.
///
/// # Errors
/// When the bytes are not an acceptable image, or cannot be written.
pub fn store_image_bytes(bytes: &[u8], label: &str, stem: &str, assets: &Path) -> Result<String> {
    ensure!(
        bytes.len() as u64 <= MAX_IMAGE_BYTES,
        "{label} is {} MB; images can be at most {} MB",
        (bytes.len() as u64).div_ceil(1024 * 1024),
        MAX_IMAGE_BYTES / (1024 * 1024)
    );
    let format = image::guess_format(bytes).ok();
    let Some(extension) = format.and_then(extension) else {
        bail!("{label} is not a PNG, JPEG or WebP image (checked by content, not by name)");
    };
    let (w, h) = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_dimensions()
        .with_context(|| format!("{label} is not a readable image"))?;
    ensure!(
        w.max(h) <= MAX_SIDE && w > 0 && h > 0,
        "{label} is {w}x{h} px; at most {MAX_SIDE} px on a side"
    );
    let digest = Sha256::digest(bytes);
    let sha8 = digest.iter().take(4).fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    let name = format!("{sha8}-{}.{extension}", sanitized_stem(stem));
    ensure!(!assets.is_symlink(), "assets folder must not be a symlink");
    std::fs::create_dir_all(assets)?;
    let target = assets.join(&name);
    if !target.is_file() {
        write_atomic(&target, bytes)?;
    }
    Ok(format!("assets/{name}"))
}

#[cfg(test)]
mod tests;
