//! Import, extract, and index files in the active space.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use std::fmt::Write as _;
use std::path::Path;

use crate::db::FileRow;
use crate::extract::{PAGE_DPI, SCANNED_PDF_STATUS};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::App;

#[derive(Clone)]
pub enum ConvertUpdate {
    Stage(String),
    /// (pages done, total pages, pages failed so far).
    Progress(usize, usize, usize),
    /// Extracted text and per-page errors, or a whole-document error.
    Done(std::result::Result<(String, Vec<(usize, String)>), String>),
}

fn is_uuid_like(stem: &str) -> bool {
    let hex = |s: &str| s.chars().all(|c| c.is_ascii_hexdigit());
    let parts: Vec<&str> = stem.split('-').collect();
    parts.len() == 5
        && parts[0].len() == 8
        && hex(parts[0])
        && parts[1].len() == 4
        && hex(parts[1])
        && parts[2].len() == 4
        && hex(parts[2])
        && parts[3].len() == 4
        && hex(parts[3])
        && parts[4].len() == 12
        && hex(parts[4])
}

fn clip_err(e: &str) -> String {
    let mut s: String = e.chars().take(90).collect();
    if s.len() < e.len() {
        s.push('…');
    }
    s
}

const MAX_EMBEDDED_PAGES: u32 = 30;

const EMBEDDING_STATUS: &str = "embedding…";

fn embedding_status(prev: &str) -> String {
    if prev == "ok" {
        EMBEDDING_STATUS.to_string()
    } else {
        format!("{EMBEDDING_STATUS} · {prev}")
    }
}

fn restored_status(status: &str) -> String {
    status
        .strip_prefix(EMBEDDING_STATUS)
        .and_then(|rest| rest.strip_prefix(" · "))
        .map_or_else(|| "ok".to_string(), str::to_string)
}

fn read_label(previous_status: &str) -> String {
    previous_status
        .trim_start_matches("convert: ")
        .trim_end_matches('…')
        .trim()
        .to_string()
}

async fn convert_flow(
    path: std::path::PathBuf,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, String, ConvertUpdate)>,
    space_id: &str,
    name: &str,
) -> std::result::Result<(String, Vec<(usize, String)>), String> {
    let result = tokio::task::spawn_blocking(move || {
        crate::extract::convert_file(&path, &crate::extract::ExtractConfig::load())
    })
    .await
    .map_err(|e| format!("conversion failed: {e}"))?;
    match result {
        Ok(done) => {
            let _ = tx.send((
                space_id.to_string(),
                name.to_string(),
                ConvertUpdate::Stage(format!("convert: {}", done.via)),
            ));
            Ok((done.text, Vec::new()))
        }
        Err(e) => Err(format!("conversion failed: {}", clip_err(&e))),
    }
}

fn read_and_hash(path: &Path, size: i64) -> Option<(String, Option<Vec<u8>>)> {
    use std::io::Read as _;
    let hex = |digest: &[u8]| {
        digest.iter().fold(String::new(), |mut h, b| {
            let _ = write!(h, "{b:02x}");
            h
        })
    };
    if size <= 64 * 1024 * 1024 {
        let bytes = std::fs::read(path).ok()?;
        return Some((hex(&Sha256::digest(&bytes)), Some(bytes)));
    }
    // Stream large files to bound memory use while hashing.
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some((hex(&hasher.finalize()), None))
}

impl App {
    /// Reconcile the active space's files with its searchable index.
    pub fn rescan_files(&mut self) {
        if self.index.embed_rx.is_none()
            && self
                .index
                .embedding_backend
                .as_ref()
                .is_some_and(crate::provider::embedding::Embedder::is_ollama)
        {
            self.index.embedding_backend = None;
        }
        self.rescan_files_with(&crate::extract::ExtractConfig::load());
    }

    #[allow(clippy::too_many_lines)]
    pub fn rescan_files_with(&mut self, cfg: &crate::extract::ExtractConfig) {
        let dir = self.space.files_dir(&self.active_space.name);
        let known = self
            .db
            .list_files(&self.active_space.id)
            .unwrap_or_default();
        let mut seen: Vec<String> = Vec::new();
        let mut convert_jobs: Vec<(String, String, std::path::PathBuf)> = Vec::new();
        let version = crate::extract::EXTRACTOR_VERSION;

        let entries = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        for entry in entries {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            seen.push(name.clone());
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            let disk_size = entry.metadata().map_or(0, |m| m.len() as i64);
            let existing = known.iter().find(|f| f.name == name);
            // Avoid reading and hashing unchanged files on the UI thread.
            if let Some(f) = existing
                && f.size == disk_size
                && f.mtime == mtime
                && mtime != 0
            {
                // Matching stats do not guarantee complete or current extraction state.
                if crate::extract::status_is_queued(&f.status) {
                    if self.index.convert_rx.is_none() {
                        convert_jobs.push((
                            self.active_space.id.clone(),
                            name.clone(),
                            path.clone(),
                        ));
                    }
                    continue;
                }
                if f.extractor >= version && self.db.file_has_chunks(&f.id).unwrap_or(false) {
                    continue;
                }
            }
            let Some((hash, bytes)) = read_and_hash(&path, disk_size) else {
                continue;
            };
            if let Some(f) = existing.filter(|f| f.hash == hash)
                && f.extractor >= version
                && self.db.file_indexed(&f.id).unwrap_or(false)
                && self.db.file_has_chunks(&f.id).unwrap_or(false)
            {
                let _ = self.db.set_file_mtime(&f.id, mtime);
                if crate::extract::status_is_queued(&f.status) && self.index.convert_rx.is_none() {
                    convert_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
                }
                continue;
            }
            // A durable file row can survive loss of the device-local extraction cache.
            let size = disk_size;
            let extracted = match &bytes {
                Some(b) => crate::extract::to_text_bytes(b, &name, cfg),
                None => crate::extract::to_text_with(&path, cfg),
            };
            let (status, chunks) = extracted.index(&name, size.unsigned_abs());
            match self
                .db
                .upsert_file(&self.active_space.id, &name, &hash, size, &status)
            {
                Ok(id) => {
                    let _ = self.db.set_file_chunks(&id, &chunks);
                    let _ = self.db.set_file_mtime(&id, mtime);
                    let _ = self.db.set_file_extractor(&id, version);
                    // Without a durable row, results cannot be recorded and rescans would requeue forever.
                    if crate::extract::status_is_queued(&status) {
                        convert_jobs.push((
                            self.active_space.id.clone(),
                            name.clone(),
                            path.clone(),
                        ));
                    }
                }
                Err(e) => self.push_status(format!("could not index {name}: {e:#}")),
            }
        }
        for gone in known.iter().filter(|f| !seen.contains(&f.name)) {
            let _ = self.db.delete_file(&gone.id);
        }
        self.start_convert(convert_jobs);
        self.start_embedding();
        self.caches.files = self
            .db
            .list_files(&self.active_space.id)
            .unwrap_or_default();
    }

    pub fn start_convert(&mut self, jobs: Vec<(String, String, std::path::PathBuf)>) {
        if jobs.is_empty() || self.index.convert_rx.is_some() {
            return;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.index.convert_rx = Some(rx);
        tokio::spawn(async move {
            for (space_id, name, path) in jobs {
                let result = convert_flow(path, &tx, &space_id, &name).await;
                if tx
                    .send((space_id, name, ConvertUpdate::Done(result)))
                    .is_err()
                {
                    return;
                }
            }
        });
    }

    pub fn reextract_file(&mut self, name: &str) {
        let Some(f) = self.caches.files.iter().find(|f| f.name == name).cloned() else {
            return;
        };
        let _ = self.db.set_file_chunks(&f.id, &[]);
        let _ = self
            .db
            .upsert_file(&self.active_space.id, &f.name, "", 0, "re-extracting");
        self.push_status(format!("re-extracting: {}", f.name));
        self.rescan_files();
    }

    #[allow(clippy::too_many_lines)]
    pub fn start_embedding(&mut self) {
        if self.index.embed_rx.is_some() {
            return;
        }
        // Timer-only chat backfill gives foreground work priority over indexing.
        let chats_due = std::time::Instant::now() >= self.index.next_chat_index;
        if chats_due {
            self.index_chats();
        }
        let model = self.index.embedding_model.trim().to_string();
        if model.is_empty() {
            return;
        }
        let Some(embedder) = self.file_embedder() else {
            return;
        };
        // Toolbox clones share the probed profile; only a new instance requires rebuilding.
        let new_instance = self
            .index
            .embedding_backend
            .as_ref()
            .is_none_or(|b| !b.same_instance(&embedder));
        self.index.embedding_backend = Some(embedder.clone());
        if new_instance {
            self.refresh_toolbox();
        }
        // Probe identity with synthetic content before sending any file text.
        let Some(identity) = embedder
            .identity()
            .map(crate::provider::embedding::EmbedIdentity::key)
        else {
            self.probe_embeddings(embedder);
            return;
        };
        let space_id = self.active_space.id.clone();
        let _ = self.db.invalidate_embedding_identity(&space_id, &identity);
        let Ok(missing) = self
            .db
            .files_missing_embedding_identity(&space_id, &identity)
        else {
            return;
        };
        let Some(file) = self.db.list_files(&space_id).ok().and_then(|files| {
            files.into_iter().find(|file| {
                missing.iter().any(|id| id == &file.id)
                    && !crate::extract::status_is_queued(&file.status)
            })
        }) else {
            self.index.embedding_status = embedder.status();
            if chats_due {
                self.start_chat_embedding(embedder, identity);
            }
            return;
        };
        // Do not install placeholder chunks while a converter owns the file.
        let file_id = file.id.clone();
        let Ok(mut chunks) = self.db.file_chunk_texts(&file_id) else {
            return;
        };
        if chunks.is_empty() {
            if self
                .db
                .set_file_chunks(&file_id, &crate::extract::metadata_chunks(&file.name))
                .is_err()
            {
                return;
            }
            chunks = match self.db.file_chunk_texts(&file_id) {
                Ok(chunks) => chunks,
                Err(_) => return,
            };
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let show_embedding_status =
            crate::extract::status_is_ok(&file.status) || file.status.starts_with(EMBEDDING_STATUS);
        if show_embedding_status && !file.status.starts_with(EMBEDDING_STATUS) {
            let _ = self
                .db
                .set_file_status(&file_id, &embedding_status(&file.status));
        }
        if space_id == self.active_space.id {
            self.caches.files = self.db.list_files(&space_id).unwrap_or_default();
        }
        let file_name = file.name;
        let image_path = self
            .space
            .files_dir(&self.active_space.name)
            .join(&file_name);
        let is_image =
            embedder.supports_images() && crate::extract::detect_path(&image_path).is_image_file();
        let settled_status = if file.status.starts_with(EMBEDDING_STATUS) {
            restored_status(&file.status)
        } else {
            file.status
        };
        let is_scanned_pdf = embedder.supports_images()
            && file_name.to_lowercase().ends_with(".pdf")
            && settled_status == SCANNED_PDF_STATUS;
        let pages = if is_scanned_pdf {
            let count = crate::extract::pdf_page_count("pdfinfo", &image_path)
                .unwrap_or_default()
                .min(MAX_EMBEDDED_PAGES);
            let page_dir = image_path.with_extension("");
            (1..=count)
                .map(|page| page_dir.join(format!("page-{page}.png")))
                .collect()
        } else {
            Vec::new()
        };
        let text = chunks
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.index.embedding_chunks = if is_image {
            vec![("image".to_string(), file_name.clone())]
        } else if is_scanned_pdf {
            if pages.is_empty() {
                crate::extract::metadata_chunks(&file_name)
            } else {
                (1..=pages.len())
                    .map(|page| (format!("page {page}"), file_name.clone()))
                    .collect()
            }
        } else if embedder.is_gemma() {
            crate::extract::embedding::chunk_text(&text, &file_name)
        } else {
            crate::extract::chunk_lines(&text)
        };
        if self.index.embedding_chunks.is_empty() {
            // A blank file still needs a vector to prevent endless requeueing.
            self.index.embedding_chunks = crate::extract::metadata_chunks(&file_name);
        }
        let pdf_path = is_scanned_pdf.then(|| image_path.clone());
        let image_paths = if is_image { vec![image_path] } else { pages };
        let chunks = self
            .index
            .embedding_chunks
            .iter()
            .enumerate()
            .map(|(seq, (_, text))| (seq as i64, text.clone()))
            .collect::<Vec<_>>();
        self.index.embedding_status = format!(
            "{} · indexing {} ({} files remaining)",
            embedder.status(),
            file_name,
            missing.len()
        );
        self.push_status(self.index.embedding_status.clone());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.index.embed_rx = Some(rx);
        handle.spawn(async move {
            let mut out: Vec<(i64, Vec<f32>)> = Vec::with_capacity(chunks.len());
            let mut err = None;
            for batch in chunks.chunks(64) {
                let inputs = if image_paths.is_empty() {
                    batch
                        .iter()
                        .map(|(_, text)| embedder.document(&file_name, text))
                        .collect()
                } else {
                    let mut inputs = Vec::with_capacity(batch.len());
                    for (seq, _) in batch {
                        let path = image_paths[*seq as usize].clone();
                        let pdf = pdf_path.clone();
                        let page = *seq as u32 + 1;
                        let prepared = tokio::task::spawn_blocking(move || {
                            if let Some(pdf) = pdf
                                && !path.exists()
                            {
                                std::fs::create_dir_all(pdf.with_extension("")).ok()?;
                                crate::extract::render_pdf_page(
                                    "pdftoppm", &pdf, page, PAGE_DPI, &path,
                                )
                                .ok()?;
                            }
                            crate::provider::embedding::image_input(&path).ok()
                        })
                        .await;
                        // Metadata keeps unsupported files searchable without stalling the queue.
                        inputs.push(match prepared {
                            Ok(Some(input)) => input,
                            _ => embedder.document(
                                &file_name,
                                &crate::extract::metadata_chunks(&file_name)[0].1,
                            ),
                        });
                    }
                    inputs
                };
                match embedder.embed(inputs).await {
                    Ok(vecs) if vecs.len() == batch.len() => out.extend(
                        batch
                            .iter()
                            .zip(vecs)
                            .map(|((seq, _), vector)| (*seq, vector)),
                    ),
                    Ok(vecs) => {
                        err = Some(format!(
                            "embedding returned {} vectors for {} chunks",
                            vecs.len(),
                            batch.len()
                        ));
                        break;
                    }
                    Err(e) => {
                        err = Some(e.to_string());
                        break;
                    }
                }
            }
            let result = match err {
                Some(e) => Err(e),
                None => Ok(out),
            };
            let _ = tx.send((space_id, file_id, result));
        });
    }

    // Stop chaining after errors to avoid hammering a dead embedding endpoint.
    pub fn on_embed_done(&mut self, r: Option<crate::app::EmbedMsg>) {
        let Some((space_id, file_id, result)) = r else {
            self.index.embed_rx = None;
            self.index.embedding_chat_job = None;
            return;
        };
        self.index.embed_rx = None;
        if self.index.embedding_chat_job.is_some() {
            self.on_chat_embed_done(result);
            return;
        }
        if file_id.is_empty() {
            self.on_embedding_probe(result);
            return;
        }
        let chunks = std::mem::take(&mut self.index.embedding_chunks);
        let restore_status = self
            .db
            .list_files(&space_id)
            .ok()
            .and_then(|files| files.into_iter().find(|file| file.id == file_id))
            .filter(|file| file.status.starts_with(EMBEDDING_STATUS))
            .map(|file| restored_status(&file.status));
        match result {
            Ok(vecs) => {
                let Some(key) = self
                    .index
                    .embedding_backend
                    .as_ref()
                    .and_then(|b| b.identity())
                    .map(crate::provider::embedding::EmbedIdentity::key)
                else {
                    self.index.embedding_status =
                        "embeddings: backend changed, will retry".to_string();
                    return;
                };
                let _ = self.db.invalidate_embedding_identity(&space_id, &key);
                if let Err(error) = self.db.set_file_embeddings(&file_id, &key, &chunks, &vecs) {
                    self.index.embedding_status = format!("embeddings: {error}");
                    self.push_status(self.index.embedding_status.clone());
                    if let Some(status) = &restore_status {
                        let _ = self.db.set_file_status(&file_id, status);
                    }
                    return;
                }
                if let Some(status) = &restore_status {
                    let _ = self.db.set_file_status(&file_id, status);
                }
                self.start_embedding();
            }
            Err(e) => {
                if let Some(status) = &restore_status {
                    let _ = self.db.set_file_status(&file_id, status);
                }
                self.index.next_chat_index =
                    std::time::Instant::now() + std::time::Duration::from_secs(60);
                self.index.embedding_status = format!("embeddings: {e} · keyword search available");
                self.push_status(format!("embedding failed: {e}"));
            }
        }
        if space_id == self.active_space.id {
            self.caches.files = self.db.list_files(&space_id).unwrap_or_default();
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn on_convert_done(&mut self, r: Option<(String, String, ConvertUpdate)>) {
        let Some((space_id, name, update)) = r else {
            self.index.convert_rx = None;
            // A rescan picks up files imported while the completed batch was running.
            self.rescan_files();
            return;
        };
        let Ok(files) = self.db.list_files(&space_id) else {
            return;
        };
        let Some(f) = files.iter().find(|f| f.name == name) else {
            return;
        };
        if !crate::extract::status_is_queued(&f.status) {
            return;
        }
        let completed = matches!(update, ConvertUpdate::Done(_));
        match update {
            ConvertUpdate::Stage(s) => {
                // The stale-result check depends on this prefix.
                let shown = if s.starts_with("convert:") {
                    s.clone()
                } else {
                    format!("convert: {s}")
                };
                let _ = self.db.set_file_status(&f.id, &shown);
                if space_id == self.active_space.id {
                    self.push_status(format!("convert {name}: {s}"));
                }
            }
            ConvertUpdate::Progress(done, total, failed) => {
                let tail = if failed > 0 {
                    format!(" ({failed} failed)")
                } else {
                    String::new()
                };
                let _ = self
                    .db
                    .set_file_status(&f.id, &format!("convert: {done}/{total}{tail}"));
                if space_id == self.active_space.id {
                    self.push_status(format!("convert {name}: {done}/{total} pages{tail}"));
                }
            }
            ConvertUpdate::Done(Ok((text, errors))) if text.trim().is_empty() => {
                let _ = self
                    .db
                    .set_file_chunks(&f.id, &crate::extract::metadata_chunks(&name));
                let status = match errors.first() {
                    Some((i, e)) => {
                        format!("all pages failed (p{}: {})", i + 1, clip_err(e))
                    }
                    None => "no text (converter found nothing)".to_string(),
                };
                let _ = self.db.set_file_status(&f.id, &status);
                if space_id == self.active_space.id {
                    self.push_status(format!("convert {name}: {status}"));
                }
            }
            ConvertUpdate::Done(Ok((text, errors))) => {
                let chunks = crate::extract::chunk_lines(&text);
                let chunks = if chunks.is_empty() {
                    crate::extract::metadata_chunks(&name)
                } else {
                    chunks
                };
                let _ = self.db.set_file_chunks(&f.id, &chunks);
                let status = match errors.first() {
                    None => format!("ok · {}", read_label(&f.status)),
                    Some((i, e)) => format!(
                        "ok — {} page{} failed (p{}: {})",
                        errors.len(),
                        if errors.len() == 1 { "" } else { "s" },
                        i + 1,
                        clip_err(e),
                    ),
                };
                let _ = self.db.set_file_status(&f.id, &status);

                if let Some(new_name) = Self::descriptive_paste_name(f, &text) {
                    let dir = self.space.files_dir(&self.active_space.name);
                    let old_path = dir.join(&f.name);
                    let new_path = dir.join(&new_name);
                    if old_path.exists() && std::fs::rename(&old_path, &new_path).is_ok() {
                        let _ = self.db.rename_file(&f.id, &new_name);
                        let _ = self
                            .db
                            .replace_file_ref_in_messages(&space_id, &f.name, &new_name);
                        if space_id == self.active_space.id {
                            self.push_status(format!("conversion done: {new_name}"));
                        }
                    } else if space_id == self.active_space.id {
                        self.push_status(format!("conversion done: {name}"));
                    }
                } else if space_id == self.active_space.id {
                    self.push_status(format!("conversion done: {name}"));
                }
            }
            ConvertUpdate::Done(Err(msg)) => {
                if self
                    .db
                    .file_chunk_texts(&f.id)
                    .unwrap_or_default()
                    .is_empty()
                {
                    let _ = self
                        .db
                        .set_file_chunks(&f.id, &crate::extract::metadata_chunks(&name));
                }
                let _ = self.db.set_file_status(&f.id, &msg);
                if space_id == self.active_space.id {
                    self.push_status(format!("convert {name}: {msg}"));
                }
            }
        }
        if completed && space_id == self.active_space.id {
            self.start_embedding();
        }
        if space_id == self.active_space.id {
            self.caches.files = self.db.list_files(&space_id).unwrap_or_default();
        }
    }

    /// Import and index a file, overwriting an existing file with the same name.
    pub fn import_file(&mut self, path: &Path) -> Result<String> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .context("path has no file name")?;
        let dir = self.space.files_dir(&self.active_space.name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::copy(path, dir.join(&name))
            .with_context(|| format!("copying {} into the space", path.display()))?;
        self.rescan_files();
        Ok(name)
    }

    pub fn delete_file(&mut self, name: &str) -> Result<()> {
        let Some(f) = self.caches.files.iter().find(|f| f.name == name).cloned() else {
            return Ok(());
        };
        let disk = self.space.files_dir(&self.active_space.name).join(&f.name);
        if disk.exists() {
            std::fs::remove_file(&disk).with_context(|| format!("removing {}", disk.display()))?;
        }
        self.db.delete_file(&f.id)?;
        self.push_status(format!("removed {}", f.name));
        self.rescan_files();
        Ok(())
    }

    pub fn rename_file(&mut self, name: &str, new: &str) -> Result<()> {
        if new.is_empty() || new == name {
            return Ok(());
        }
        if new.contains(['/', '\\']) || new == "." || new == ".." {
            anyhow::bail!("invalid name: {new}");
        }
        let dir = self.space.files_dir(&self.active_space.name);
        if dir.join(new).exists() {
            anyhow::bail!("{new} already exists");
        }
        std::fs::rename(dir.join(name), dir.join(new))
            .with_context(|| format!("renaming {name} to {new}"))?;
        self.rescan_files();
        self.push_status(format!("renamed {name} to {new}"));
        Ok(())
    }

    fn descriptive_paste_name(f: &FileRow, text: &str) -> Option<String> {
        let stem = std::path::Path::new(&f.name).file_stem()?.to_str()?;
        let ext = std::path::Path::new(&f.name).extension()?.to_str()?;
        if !is_uuid_like(stem) {
            return None;
        }
        let slug = Self::slug_from_text(text)?;
        Some(format!("{stem}-{slug}.{ext}"))
    }

    fn slug_from_text(text: &str) -> Option<String> {
        let words: Vec<&str> = text
            .split_whitespace()
            .filter(|w| {
                let w = w.trim_matches(|c: char| !c.is_alphanumeric());
                w.len() > 2 && w.chars().any(char::is_alphanumeric)
            })
            .collect();
        if words.is_empty() {
            return None;
        }
        let slug: String = words
            .iter()
            .take(5)
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .collect::<Vec<_>>()
            .join("_");
        if slug.is_empty() { None } else { Some(slug) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root = std::env::temp_dir().join(format!("nexus-files-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k"), space)
    }

    #[tokio::test]
    async fn embedder_queue_backfills_chains_and_stops_on_error() {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let id = a.db.upsert_file(&space, "b.txt", "h", 1, "ok").unwrap();
        a.db.set_file_chunks(&id, &[("l".into(), "text".into())])
            .unwrap();

        let saved = a.backends.clone();
        a.backends = crate::app::Backends::default();
        a.start_embedding();
        assert!(a.index.embed_rx.is_none());
        a.backends = saved;

        let m = std::mem::take(&mut a.index.embedding_model);
        a.start_embedding();
        assert!(a.index.embed_rx.is_none());
        a.index.embedding_model = m;

        a.start_embedding();
        assert!(a.index.embed_rx.is_some());
        let files = a.db.list_files(&space).unwrap();
        assert!(
            files[0].status.starts_with("embedding"),
            "{}",
            files[0].status
        );

        a.on_embed_done(Some((
            space.clone(),
            id.clone(),
            Ok(vec![(0, vec![1.0f32, 0.0])]),
        )));
        assert!(a.db.files_missing_embeddings(&space).unwrap().is_empty());
        assert_eq!(a.db.list_files(&space).unwrap()[0].status, "ok");

        a.db.set_file_chunks(&id, &[("l".into(), "new".into())])
            .unwrap();
        a.on_embed_done(Some((space.clone(), id.clone(), Err("offline".into()))));
        assert!(a.index.embed_rx.is_none());
        let (_, status) = a.drain_ui_events();
        assert!(status.contains("embedding failed"));
        assert_eq!(a.db.list_files(&space).unwrap()[0].status, "ok");
    }

    #[test]
    fn import_copies_extracts_and_indexes() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-src-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&src, "# quarterly report\nrevenue up").unwrap();

        let name = a.import_file(&src).unwrap();
        assert_eq!(name, src.file_name().unwrap().to_string_lossy());
        assert_eq!(a.caches.files.len(), 1);
        assert_eq!(a.caches.files[0].status, "ok");
        assert!(a.space.files_dir(&a.active_space.name).join(&name).exists());
        let hits = crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "revenue", 8)
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn generic_code_and_binary_files_get_index_chunks() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("page.html"), "<main>searchable markup</main>").unwrap();
        std::fs::write(dir.join("job.py"), "def searchable_code():\n    return 42").unwrap();
        std::fs::write(dir.join("asset.bin"), [0u8, 1, 2, 3]).unwrap();

        a.rescan_files();
        for name in ["page.html", "job.py", "asset.bin"] {
            let file = a
                .caches
                .files
                .iter()
                .find(|file| file.name == name)
                .unwrap();
            assert!(a.db.file_has_chunks(&file.id).unwrap(), "{name}");
        }
        let hits =
            crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "searchable", 8)
                .unwrap();
        assert_eq!(hits.len(), 2, "code files should be FTS indexed");
        let binary_hits =
            crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "asset.bin", 8)
                .unwrap();
        assert_eq!(
            binary_hits.len(),
            1,
            "binary upload should be name-searchable"
        );
    }

    #[test]
    fn cold_cache_after_restore_reindexes_from_disk() {
        let root = std::env::temp_dir().join(format!("nexus-cold-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        let db = Db::open(&space.db_path()).unwrap();
        let mut a = App::new(db, Some("k"), space);

        let src = std::env::temp_dir().join(format!("nexus-cold-src-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&src, "# report\nresilience is a property").unwrap();
        let name = a.import_file(&src).unwrap();
        let id = a.caches.files[0].id.clone();
        assert!(a.db.file_indexed(&id).unwrap());
        assert!(
            crate::db::file_text(a.db.conn_for_test(), &a.active_space.id, &name)
                .unwrap()
                .is_some()
        );
        let root = a.space.root.clone();
        drop(a);

        let cache_path = root.join("cache.db");
        assert!(cache_path.exists());
        std::fs::remove_file(&cache_path).unwrap();

        let mut a = App::new(
            Db::open(&root.join("nexus.db")).unwrap(),
            Some("k"),
            Space { root },
        );
        assert!(!a.db.file_indexed(&id).unwrap());

        a.rescan_files();
        assert!(a.db.file_indexed(&id).unwrap());
        assert_eq!(
            crate::db::file_text(a.db.conn_for_test(), &a.active_space.id, &name)
                .unwrap()
                .as_deref(),
            Some("# report\nresilience is a property")
        );
        assert_eq!(a.db.list_files(&a.active_space.id).unwrap()[0].status, "ok");
    }

    #[test]
    fn reextract_clears_stale_chunks_and_reindexes_from_disk() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("doc.txt"), "real content on disk").unwrap();
        a.rescan_files();
        let id = a.caches.files[0].id.clone();

        a.db.set_file_chunks(&id, &[("p1".into(), "garbage".into())])
            .unwrap();

        a.reextract_file("doc.txt");
        assert!(
            a.last_status().contains("re-extracting"),
            "{}",
            a.last_status()
        );
        let texts = a.db.file_chunk_texts(&id).unwrap();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].1.contains("real content"), "{texts:?}");
        assert_eq!(a.caches.files[0].status, "ok");
    }

    #[test]
    fn rescan_picks_up_dropped_and_deleted_files() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dropped.txt"), "hello dropped").unwrap();

        a.rescan_files();
        assert_eq!(a.caches.files.len(), 1);
        assert_eq!(a.caches.files[0].name, "dropped.txt");

        std::fs::write(dir.join("dropped.txt"), "hello again").unwrap();
        a.rescan_files();
        assert_eq!(a.caches.files.len(), 1);
        std::fs::remove_file(dir.join("dropped.txt")).unwrap();
        a.rescan_files();
        assert!(a.caches.files.is_empty());
    }

    #[test]
    fn empty_extraction_gets_no_text_status() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.txt"), "   ").unwrap();
        a.rescan_files();
        assert_eq!(a.caches.files[0].status, "no text (scanned?)");
    }

    #[test]
    fn rescan_skips_stat_unchanged_files_without_rehashing() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("book.txt"), "big content").unwrap();
        a.rescan_files();
        let f = a.caches.files[0].clone();
        assert!(f.mtime > 0, "mtime recorded on index");

        a.db.upsert_file(&a.active_space.id, "book.txt", "sentinel", f.size, "ok")
            .unwrap();
        a.rescan_files();
        assert_eq!(a.caches.files[0].hash, "sentinel");

        std::fs::write(dir.join("book.txt"), "big content grew").unwrap();
        a.rescan_files();
        assert_ne!(a.caches.files[0].hash, "sentinel");
        assert_eq!(a.caches.files[0].status, "ok");
    }

    #[test]
    fn rename_moves_disk_file_and_reindexes() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.txt"), "searchable content").unwrap();
        std::fs::write(dir.join("taken.txt"), "x").unwrap();
        a.rescan_files();

        assert!(a.rename_file("old.txt", "taken.txt").is_err());
        assert!(dir.join("old.txt").exists());

        assert!(a.rename_file("old.txt", "../evil.txt").is_err());

        a.rename_file("old.txt", "new.txt").unwrap();
        assert!(!dir.join("old.txt").exists());
        assert!(dir.join("new.txt").exists());
        assert!(a.caches.files.iter().any(|f| f.name == "new.txt"));
    }

    #[test]
    fn delete_removes_disk_file_and_row() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gone.txt"), "bye").unwrap();
        a.rescan_files();
        a.delete_file("gone.txt").unwrap();
        assert!(a.caches.files.is_empty());
        assert!(!dir.join("gone.txt").exists());
    }

    #[test]
    fn import_file_copies_and_indexes_typed_path() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-add-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&src, "typed in").unwrap();
        let name = a.import_file(&src).unwrap();
        assert_eq!(a.caches.files.len(), 1);
        assert_eq!(a.caches.files[0].name, name);

        assert!(
            a.import_file(std::path::Path::new("/definitely/not/a/file"))
                .is_err()
        );
        assert_eq!(a.caches.files.len(), 1);
    }

    #[test]
    fn importing_an_image_indexes_metadata_without_queueing() {
        let mut a = test_app();
        a.index.embedding_model.clear();
        let src = std::env::temp_dir().join(format!("nexus-image-{}.png", uuid::Uuid::new_v4()));
        image::RgbImage::new(2, 2).save(&src).unwrap();
        let name = a.import_file(&src).unwrap();
        assert_eq!(a.caches.files[0].status, "ok · image");
        assert!(a.index.convert_rx.is_none());
        let chunks = a.db.file_chunk_texts(&a.caches.files[0].id).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].1.contains(&format!("uploaded file: {name}")));
        a.rescan_files();
        assert_eq!(a.caches.files[0].status, "ok · image");
        assert!(a.index.convert_rx.is_none());
        let _ = std::fs::remove_file(src);
    }

    #[test]
    fn scanned_pdf_is_searchable_by_name_without_queueing() {
        let mut a = test_app();
        a.index.embedding_model.clear();
        let dir = files_dir_of(&a);
        std::fs::write(dir.join("scan.pdf"), crate::extract::minimal_pdf(None)).unwrap();
        a.rescan_files();
        assert_eq!(a.caches.files[0].status, "ok · scanned pdf (no text layer)");
        assert!(a.index.convert_rx.is_none());
        let chunks = a.db.file_chunk_texts(&a.caches.files[0].id).unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].1.contains("uploaded file: scan.pdf"));
    }

    fn files_dir_of(a: &App) -> std::path::PathBuf {
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn status_of(a: &App, name: &str) -> String {
        a.caches
            .files
            .iter()
            .find(|f| f.name == name)
            .unwrap()
            .status
            .clone()
    }

    #[test]
    fn html_export_is_indexed_as_clean_text_with_a_status() {
        let mut a = test_app();
        let dir = files_dir_of(&a);
        let blob = "QUJD".repeat(300);
        let html = format!(
            "<html><head><style>.a{{color:red}}</style><script>var track=1;</script></head><body>\
             <img src=\"data:image/png;base64,{blob}\">\
             <div class=\"chatlog__message-container\"><div class=\"chatlog__header\">\
             <span class=\"chatlog__author\">Ann</span><span class=\"chatlog__timestamp\">10:02 PM</span></div>\
             <div class=\"chatlog__content\">lunch at noon</div></div></body></html>"
        );
        std::fs::write(dir.join("chat.html"), html).unwrap();
        a.rescan_files();
        assert_eq!(status_of(&a, "chat.html"), "ok · html → text");
        let text = crate::db::file_text(a.db.conn_for_test(), &a.active_space.id, "chat.html")
            .unwrap()
            .unwrap();
        assert_eq!(text, "[10:02 PM] Ann: lunch at noon");
        assert!(!text.contains("QUJD") && !text.contains("track"));
    }

    #[test]
    fn a_pdf_without_a_pdf_extension_is_still_read_as_a_pdf() {
        let mut a = test_app();
        let dir = files_dir_of(&a);
        let pdf = crate::extract::pdf_with_pages(&["QUARTERLY NEXUS REPORT"]);
        std::fs::write(dir.join("download"), &pdf).unwrap();
        std::fs::write(dir.join("report.txt"), &pdf).unwrap();
        a.rescan_files();
        for name in ["download", "report.txt"] {
            assert_eq!(status_of(&a, name), "ok", "{name}");
            let text = crate::db::file_text(a.db.conn_for_test(), &a.active_space.id, name)
                .unwrap()
                .unwrap();
            assert!(text.contains("QUARTERLY NEXUS REPORT"), "{name}: {text:?}");
            assert!(!text.contains("%PDF"), "{name}: raw bytes leaked");
        }
    }

    #[test]
    fn unknown_binary_is_metadata_only_with_a_status_and_a_clear_read_message() {
        let mut a = test_app();
        let dir = files_dir_of(&a);
        std::fs::write(dir.join("asset.bin"), [0u8, 159, 146, 150, 1, 2]).unwrap();
        a.rescan_files();
        let status = status_of(&a, "asset.bin");
        assert!(
            status.starts_with("binary: unknown, no converter — install pandoc/libreoffice"),
            "{status}"
        );
        assert!(
            crate::db::file_text(a.db.conn_for_test(), &a.active_space.id, "asset.bin")
                .unwrap()
                .is_none()
        );
        let msg = crate::tools::unreadable_file_message_for_test(
            a.db.conn_for_test(),
            &a.active_space.id,
            "asset.bin",
        );
        assert!(
            msg.starts_with("asset.bin has no readable text · binary: unknown"),
            "{msg}"
        );
    }

    #[test]
    fn an_older_extractor_version_re_extracts_once() {
        let mut a = test_app();
        let dir = files_dir_of(&a);
        std::fs::write(dir.join("page.html"), "<p>fresh words</p>").unwrap();
        a.rescan_files();
        let id = a.caches.files[0].id.clone();
        assert_eq!(
            a.caches.files[0].extractor,
            crate::extract::EXTRACTOR_VERSION
        );

        a.db.set_file_chunks(&id, &[("lines 1-1".into(), "<p>stale markup</p>".into())])
            .unwrap();
        a.db.set_file_extractor(&id, 0).unwrap();
        a.rescan_files();
        let texts = a.db.file_chunk_texts(&id).unwrap();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].1, "fresh words");
        assert_eq!(
            a.caches.files[0].extractor,
            crate::extract::EXTRACTOR_VERSION
        );

        a.db.set_file_chunks(&id, &[("lines 1-1".into(), "sentinel".into())])
            .unwrap();
        a.rescan_files();
        assert_eq!(a.db.file_chunk_texts(&id).unwrap()[0].1, "sentinel");
    }

    #[test]
    fn embedding_status_carries_the_read_status_and_restores_it() {
        assert_eq!(embedding_status("ok"), "embedding…");
        assert_eq!(
            embedding_status("ok · html → text"),
            "embedding… · ok · html → text"
        );
        assert_eq!(restored_status("embedding…"), "ok");
        assert_eq!(
            restored_status("embedding… · ok · html → text"),
            "ok · html → text"
        );
    }

    #[tokio::test]
    async fn embedding_a_converted_file_keeps_how_it_was_read() {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let id =
            a.db.upsert_file(&space, "p.html", "h", 1, "ok · html → text")
                .unwrap();
        a.db.set_file_chunks(&id, &[("l".into(), "text".into())])
            .unwrap();
        a.start_embedding();
        assert_eq!(
            a.db.list_files(&space).unwrap()[0].status,
            "embedding… · ok · html → text"
        );
        a.on_embed_done(Some((space.clone(), id, Ok(vec![(0, vec![1.0f32, 0.0])]))));
        assert_eq!(
            a.db.list_files(&space).unwrap()[0].status,
            "ok · html → text"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_converter_job_marks_the_file_then_replaces_the_stand_in_text() {
        use crate::extract::external::fake_tools::Bin;
        let bin = Bin::new();
        bin.tool("pandoc", "echo '# converted by pandoc'");
        let mut a = test_app();
        a.index.embedding_model.clear();
        let dir = files_dir_of(&a);
        std::fs::write(dir.join("notes.rst"), "Title\n=====\nrst body words").unwrap();
        a.rescan_files_with(&bin.cfg());
        assert_eq!(status_of(&a, "notes.rst"), "convert: pandoc…");
        assert!(a.index.convert_rx.is_some(), "a job is queued");
        let hits =
            crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "words", 8).unwrap();
        assert_eq!(hits.len(), 1);

        let space = a.active_space.id.clone();
        a.on_convert_done(Some((
            space.clone(),
            "notes.rst".into(),
            ConvertUpdate::Stage("convert: pandoc".into()),
        )));
        assert_eq!(status_of(&a, "notes.rst"), "convert: pandoc");
        a.on_convert_done(Some((
            space.clone(),
            "notes.rst".into(),
            ConvertUpdate::Done(Ok(("# converted by pandoc".into(), Vec::new()))),
        )));
        assert_eq!(status_of(&a, "notes.rst"), "ok · pandoc");
        let text = crate::db::file_text(a.db.conn_for_test(), &space, "notes.rst")
            .unwrap()
            .unwrap();
        assert_eq!(text, "# converted by pandoc");

        std::fs::write(dir.join("other.rst"), "Other\n=====\nkept words").unwrap();
        a.index.convert_rx = None;
        a.rescan_files_with(&bin.cfg());
        a.on_convert_done(Some((
            space.clone(),
            "other.rst".into(),
            ConvertUpdate::Done(Err("conversion failed: pandoc: boom".into())),
        )));
        assert_eq!(
            status_of(&a, "other.rst"),
            "conversion failed: pandoc: boom"
        );
        assert!(!crate::extract::status_is_queued(&status_of(
            &a,
            "other.rst"
        )));
        assert!(
            crate::db::file_text(a.db.conn_for_test(), &space, "other.rst")
                .unwrap()
                .unwrap()
                .contains("kept words")
        );
    }

    #[test]
    fn read_labels_say_how_a_job_read_the_file() {
        assert_eq!(read_label("convert: soffice → pdf"), "soffice → pdf");
        assert_eq!(read_label("convert: pandoc…"), "pandoc");
    }
}
