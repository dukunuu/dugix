//! Bounded DOCX/DOTX and Markdown file imports, with content-addressed media.
pub mod docx;
use super::{Doc, Kind, feedback::Thread};
use anyhow::{Context as _, Result, bail, ensure};
use std::path::Path;
/// Checked document, imported comment threads and every approximation.
#[derive(Debug)]
pub struct Imported {
    pub doc: Doc,
    pub threads: Vec<Thread>,
    pub warnings: Vec<String>,
}
/// The same package byte limit as deck import.
pub const MAX_BYTES: u64 = crate::deck::import::MAX_PPTX_BYTES;
/// Read a local Markdown, DOCX or DOTX file, copying relative media into assets.
pub fn file(source: &Path, assets: &Path) -> Result<Imported> {
    read_file(source, assets, None)
}
/// Tool import: every Markdown image must remain within the space files.
pub fn file_confined(source: &Path, assets: &Path, files: &Path) -> Result<Imported> {
    read_file(source, assets, Some(&files.canonicalize()?))
}
fn read_file(source: &Path, assets: &Path, files: Option<&Path>) -> Result<Imported> {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    ensure!(
        !matches!(ext.as_str(), "docm" | "dotm"),
        "macro-enabled Word files are refused (.docm/.dotm)"
    );
    ensure!(
        std::fs::metadata(source)?.len() <= MAX_BYTES,
        "document exceeds the 250 MB import limit"
    );
    let bytes = std::fs::read(source)?;
    match ext.as_str() {
        "docx" | "dotx" => docx::import(&bytes, assets),
        "md" => {
            let text = std::str::from_utf8(&bytes).context("Markdown must be UTF-8")?;
            let parsed = super::markdown::parse_document(text, true);
            let title = parsed
                .blocks
                .iter()
                .find_map(|b| {
                    if let Kind::Title { text } = &b.kind {
                        Some(super::markdown::plain(text))
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| {
                    source
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            let mut doc = Doc::from_design(&title, &crate::design::Design::default());
            doc.blocks = parsed.blocks;
            doc.footnotes = parsed.footnotes;
            let mut warnings = Vec::new();
            for block in &mut doc.blocks {
                if let Kind::Image { src, .. } = &mut block.kind {
                    if src.starts_with("https://") {
                        continue;
                    }
                    let image = source.parent().context("Markdown folder")?.join(&*src);
                    if !image.is_file() {
                        warnings.push(format!("{}: image {src} is missing.", block.id));
                        continue;
                    }
                    if let Some(files) = files {
                        ensure!(
                            image.canonicalize()?.starts_with(files),
                            "Markdown image is outside the space files"
                        );
                    }
                    ensure!(
                        std::fs::metadata(&image)?.len() <= crate::deck::assets::MAX_IMAGE_BYTES,
                        "image exceeds 16 MB"
                    );
                    *src = crate::deck::assets::store_image_bytes(
                        &std::fs::read(&image)?,
                        src,
                        image
                            .file_stem()
                            .unwrap_or_default()
                            .to_str()
                            .unwrap_or("image"),
                        assets,
                    )?;
                }
            }
            let doc =
                Doc::check(&serde_json::to_string(&doc)?).map_err(|e| anyhow::anyhow!("{e:?}"))?;
            Ok(Imported {
                doc,
                threads: Vec::new(),
                warnings,
            })
        }
        _ => bail!("expected .docx, .dotx or .md"),
    }
}
/// A doc from generated Markdown: the first level-one heading becomes the title
/// (else `fallback_title`), and an image that is not a usable HTTPS address stays
/// as a link.
pub fn from_markdown(files: &Path, markdown: &str, fallback_title: &str) -> Result<Doc> {
    let mut parsed = super::markdown::parse_document(markdown, false);
    let title = super::markdown::take_title(&mut parsed.blocks)
        .unwrap_or_else(|| fallback_title.to_string());
    let slug = crate::design::default_slug(files)?;
    let design = crate::design::load(files, &slug)?;
    let mut doc = Doc::from_design(&title, &design);
    doc.design = Some(slug);
    doc.blocks = parsed.blocks;
    doc.footnotes = parsed.footnotes;
    for block in &mut doc.blocks {
        if let Kind::Image { src, alt, .. } = &block.kind
            && crate::remote_image::validate_url(src).is_err()
        {
            let label = if alt.is_empty() {
                src.clone()
            } else {
                alt.clone()
            };
            block.kind = Kind::Paragraph {
                text: format!("[{label}](<{src}>)"),
            };
        }
    }
    Doc::check(&serde_json::to_string(&doc)?).map_err(|e| anyhow::anyhow!("{e:?}"))
}

/// Write a new document and its comments, never replacing an existing file.
pub fn write_new(imported: &Imported, target: &Path) -> Result<()> {
    ensure!(
        target.to_string_lossy().ends_with(".doc.json"),
        "output must end in .doc.json"
    );
    let parent = target.parent().context("doc folder")?;
    ensure!(
        !parent.is_symlink() && !parent.join("assets").is_symlink(),
        "doc folders must not be symlinks"
    );
    std::fs::create_dir_all(parent)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(&imported.doc)?)?;
    if !imported.threads.is_empty() {
        super::feedback::save(target, &imported.threads)?;
    }
    Ok(())
}
