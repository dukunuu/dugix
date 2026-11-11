//! The space's imported files: list, keyword/semantic search, read, PDF pages.

use std::path::Path;

use super::hashline::number_lines;
use super::{FilesCtx, ToolBox};

impl ToolBox {
    /// The imported-file tools: list, search, read and PDF pages.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn run_files(&self, name: &str, args: &str) -> (String, String) {
        match name {
            "list_files" => {
                let status = "Listing files…".to_string();
                let result = match &self.files {
                    None => "no files imported".to_string(),
                    Some(ctx) => list_files_impl(ctx),
                };
                (result, status)
            }
            "search_files" => {
                let query = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let status = "Searching files…".to_string();
                let result = match &self.files {
                    None => "no files imported".to_string(),
                    Some(ctx) => search_files_impl(ctx, &query).await,
                };
                (result, status)
            }
            "read_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let offset = usize::try_from(
                    v.get("offset")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1)
                        .max(1),
                )
                .unwrap_or(1);
                let limit = v
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let status = format!("Reading {name}…");
                let result = match &self.files {
                    None => "no files imported".to_string(),
                    Some(ctx) => match crate::db::open_attached(&ctx.db_path)
                        .and_then(|conn| crate::db::file_text(&conn, &ctx.space_id, &name))
                    {
                        Ok(Some(text)) => {
                            let lines: Vec<&str> = text.lines().collect();
                            let total = lines.len();
                            let start = (offset - 1).min(total);
                            let slice = &lines[start..(start + limit).min(total)];
                            if slice.is_empty() {
                                format!("{name}: offset {offset} is past the end ({total} lines)")
                            } else {
                                format!(
                                    "{name} (lines {}-{} of {total}):\n{}",
                                    start + 1,
                                    start + slice.len(),
                                    number_lines(slice, start),
                                )
                            }
                        }
                        Ok(None) => unreadable_file_message(&ctx.db_path, &ctx.space_id, &name),
                        Err(e) => format!("file read failed: {e}"),
                    },
                };
                (result, status)
            }
            "read_pdf_page" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let page = u32::try_from(
                    v.get("page")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1)
                        .max(1),
                )
                .unwrap_or(u32::MAX);
                let status = format!("Reading {name} page {page}…");
                let result = match &self.files {
                    None => "no files imported".to_string(),
                    Some(ctx) => {
                        read_pdf_page(ctx, &self.space_files_dir, &name, page, "pdftoppm").await
                    }
                };
                (result, status)
            }
            other => (
                format!("unknown tool: {other}"),
                "Running tool…".to_string(),
            ),
        }
    }
}

/// Read a cached PDF page, rendering it on demand when missing.
pub(super) async fn read_pdf_page(
    ctx: &FilesCtx,
    files_dir: &Path,
    name: &str,
    page: u32,
    pdftoppm: &str,
) -> String {
    let known = crate::db::open_attached(&ctx.db_path)
        .and_then(|conn| crate::db::file_status(&conn, &ctx.space_id, name))
        .ok()
        .flatten()
        .is_some();
    if !known {
        return format!("unknown file: {name}");
    }
    if !name.to_lowercase().ends_with(".pdf") {
        return format!("not a PDF: {name}");
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let page_dir = files_dir.join(stem);
    let png_path = page_dir.join(format!("page-{page}.png"));
    if !png_path.exists() {
        let pdf = files_dir.join(name);
        let pdftoppm = pdftoppm.to_string();
        let result = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&page_dir)
                .map_err(|e| crate::extract::ToolError::Failed(e.to_string()))?;
            crate::extract::render_pdf_page(&pdftoppm, &pdf, page, 150, &png_path)
        })
        .await
        .unwrap_or_else(|e| Err(crate::extract::ToolError::Failed(e.to_string())));
        match result {
            Ok(()) => {}
            Err(crate::extract::ToolError::MissingTools) => {
                return "pdftoppm is not installed — install poppler to view PDF pages".to_string();
            }
            Err(crate::extract::ToolError::Failed(error)) => {
                return format!("cannot render page {page}: {error}");
            }
        }
    }
    format!("![page {page}]({stem}/page-{page}.png)")
}

/// The space's imported files as JSON `{name, size}` rows, sorted by name.
fn list_files_impl(ctx: &FilesCtx) -> String {
    let rows = crate::db::open_conn(&ctx.db_path).and_then(|conn| -> anyhow::Result<Vec<_>> {
        let mut stmt =
            conn.prepare("SELECT name, size FROM files WHERE space_id = ?1 ORDER BY name")?;
        let rows = stmt
            .query_map([&ctx.space_id], |r| {
                Ok(serde_json::json!({
                    "name": r.get::<_, String>(0)?,
                    "size": r.get::<_, i64>(1)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    });
    match rows {
        Ok(rows) => serde_json::Value::Array(rows).to_string(),
        Err(error) => format!("db error: {error}"),
    }
}

/// Search imported files: embed the query and rank chunks by cosine when an
/// embedder is configured; otherwise (or when embedding fails / no vectors
/// are stored yet) fall back to FTS keywords, tagged so the model knows the
/// weaker path answered.
pub(super) async fn search_files_impl(ctx: &FilesCtx, query: &str) -> String {
    let conn = match crate::db::open_attached(&ctx.db_path) {
        Ok(c) => c,
        Err(e) => return format!("file search failed: {e}"),
    };
    let mut fell_back = false;
    let mut embedding_error = None;
    if let Some(embedder) = &ctx.embedder {
        match embedder.query(query).await {
            Ok(vector) => {
                if let Some(identity) = embedder.identity()
                    && let Ok(hits) = crate::db::semantic_chunks_for_identity(
                        &conn,
                        &ctx.space_id,
                        &identity.key(),
                        &vector,
                        8,
                    )
                    && !hits.is_empty()
                {
                    return format_semantic_hits(&hits);
                }
                fell_back = true;
            }
            Err(error) => {
                fell_back = true;
                embedding_error = Some(error.to_string());
            }
        }
    }
    match crate::db::search_chunks(&conn, &ctx.space_id, query, 8) {
        Ok(hits) if hits.is_empty() => embedding_error.map_or_else(
            || "no matches".to_string(),
            |error| format!("(keyword fallback · embeddings: {error})\nno matches"),
        ),
        Ok(hits) => {
            let body = hits
                .iter()
                .map(|(name, loc, snip)| format!("{name} ({loc}): {snip}"))
                .collect::<Vec<_>>()
                .join("\n");
            if fell_back {
                embedding_error.map_or_else(
                    || format!("(keyword fallback)\n{body}"),
                    |error| format!("(keyword fallback · embeddings: {error})\n{body}"),
                )
            } else {
                body
            }
        }
        Err(e) => format!("file search failed: {e}"),
    }
}

/// Top cosine-ranked chunks formatted as `name (location): text` (truncated),
/// or None when the space has no usable vectors.
#[cfg(test)]
pub(super) fn semantic_snippets(
    conn: &rusqlite::Connection,
    space_id: &str,
    query: &[f32],
) -> Option<String> {
    let hits = crate::db::semantic_chunks(conn, space_id, query, 8).ok()?;
    if hits.is_empty() {
        return None;
    }
    Some(format_semantic_hits(&hits))
}

fn format_semantic_hits(hits: &[(String, String, String, f32)]) -> String {
    hits.iter()
        .map(|(name, loc, text, score)| {
            if loc == "image" {
                return format!("image · {name} · {score:.2}\n![{name}]({name})");
            }
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let cut: String = flat.chars().take(300).collect();
            let ellipsis = if cut.len() < flat.len() { "…" } else { "" };
            format!("text · {name} ({loc}) · {score:.2}: {cut}{ellipsis}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Why `files read` has no text for `name`: unknown, still being processed,
/// or imported but without readable text (with the status that says why).
fn unreadable_file_message(db_path: &Path, space_id: &str, name: &str) -> String {
    let status = crate::db::open_attached(db_path)
        .and_then(|conn| crate::db::file_status(&conn, space_id, name))
        .ok()
        .flatten();
    unreadable_message(status, name)
}

fn unreadable_message(status: Option<String>, name: &str) -> String {
    match status {
        None => format!("unknown file: {name}"),
        Some(s) if crate::extract::status_is_queued(&s) => {
            format!("{name} is still being processed ({s}) · try again shortly")
        }
        Some(s) => format!("{name} has no readable text · {s}"),
    }
}

#[cfg(test)]
pub(crate) fn unreadable_file_message_for_test(
    conn: &rusqlite::Connection,
    space_id: &str,
    name: &str,
) -> String {
    let status = crate::db::file_status(conn, space_id, name).ok().flatten();
    unreadable_message(status, name)
}
