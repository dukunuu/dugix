//! Imported files, chunks, citations and embeddings.

use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;
use uuid::Uuid;

use super::{Db, FileRow};

impl Db {
    /// Insert or replace a file row (unique per space+name), returning its id; an
    /// existing row keeps its id so its chunks can be replaced.
    pub fn upsert_file(
        &self,
        space_id: &str,
        name: &str,
        hash: &str,
        size: i64,
        status: &str,
    ) -> Result<String> {
        let now = Utc::now().to_rfc3339();
        if let Ok(existing) = self.conn.query_row(
            "SELECT id FROM files WHERE space_id = ?1 AND name = ?2",
            (space_id, name),
            |r| r.get::<_, String>(0),
        ) {
            self.conn.execute(
                "UPDATE files SET hash = ?2, size = ?3, updated_at = ?4 WHERE id = ?1",
                (&existing, hash, size, &now),
            )?;
            self.conn.execute(
                "INSERT INTO cache.file_index_state (file_id, mtime, status, updated_at)
                 VALUES (?1, 0, ?2, ?3)
                 ON CONFLICT(file_id) DO UPDATE SET status = excluded.status,
                     updated_at = excluded.updated_at",
                (&existing, status, &now),
            )?;
            return Ok(existing);
        }
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO files (id, space_id, name, hash, size, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (&id, space_id, name, hash, size, &now, &now),
        )?;
        self.conn.execute(
            "INSERT INTO cache.file_index_state (file_id, mtime, status, updated_at)
             VALUES (?1, 0, ?2, ?3)",
            (&id, status, &now),
        )?;
        Ok(id)
    }

    pub fn list_files(&self, space_id: &str) -> Result<Vec<FileRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT files.id, files.name, files.hash, files.size,
                    COALESCE(cache.file_index_state.status, 'not indexed'),
                    COALESCE(cache.file_index_state.mtime, 0),
                    COALESCE(cache.file_index_state.extractor, 0)
             FROM files
             LEFT JOIN cache.file_index_state
                 ON cache.file_index_state.file_id = files.id
             WHERE files.space_id = ?1 ORDER BY files.name ASC",
        )?;
        let rows = stmt.query_map([space_id], |r| {
            Ok(FileRow {
                id: r.get(0)?,
                name: r.get(1)?,
                hash: r.get(2)?,
                size: r.get(3)?,
                status: r.get(4)?,
                mtime: r.get(5)?,
                extractor: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Whether this device has derived index state for the file; a missing row
    /// means a cold cache that must re-extract.
    pub fn file_indexed(&self, file_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM cache.file_index_state WHERE file_id = ?1)",
            [file_id],
            |r| r.get(0),
        )?)
    }

    /// Whether the file has extracted chunks; separate from `file_indexed`, since
    /// a cache can have a status row without chunks.
    pub fn file_has_chunks(&self, file_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM cache.file_chunks WHERE file_id = ?1)",
            [file_id],
            |r| r.get(0),
        )?)
    }

    pub fn delete_file(&self, file_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM cache.file_chunks WHERE file_id = ?1",
            [file_id],
        )?;
        self.conn.execute(
            "DELETE FROM cache.chunk_embeddings WHERE file_id = ?1",
            [file_id],
        )?;
        self.conn.execute(
            "DELETE FROM cache.file_index_state WHERE file_id = ?1",
            [file_id],
        )?;
        self.conn
            .execute("DELETE FROM files WHERE id = ?1", [file_id])?;
        self.tombstone("files", file_id)?;
        Ok(())
    }

    /// Record the disk mtime a file was indexed at.
    pub fn set_file_mtime(&self, file_id: &str, mtime: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO cache.file_index_state (file_id, mtime, status, updated_at)
             VALUES (?1, ?2, '', ?3)
             ON CONFLICT(file_id) DO UPDATE SET mtime = excluded.mtime,
                 updated_at = excluded.updated_at",
            (file_id, mtime, &now),
        )?;
        Ok(())
    }

    /// Record which extractor version indexed a file.
    pub fn set_file_extractor(&self, file_id: &str, version: i64) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO cache.file_index_state (file_id, mtime, status, updated_at, extractor)
             VALUES (?1, 0, '', ?3, ?2)
             ON CONFLICT(file_id) DO UPDATE SET extractor = excluded.extractor,
                 updated_at = excluded.updated_at",
            (file_id, version, &now),
        )?;
        Ok(())
    }

    /// Update a file's derived status.
    pub fn set_file_status(&self, file_id: &str, status: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO cache.file_index_state (file_id, mtime, status, updated_at)
             VALUES (?1, 0, ?2, ?3)
             ON CONFLICT(file_id) DO UPDATE SET status = excluded.status,
                 updated_at = excluded.updated_at",
            (file_id, status, &now),
        )?;
        Ok(())
    }

    pub fn rename_file(&self, file_id: &str, new_name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET name = ?2, updated_at = ?3 WHERE id = ?1",
            (file_id, new_name, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Replace a file name in message content within a space, e.g. after OCR
    /// renames an image.
    pub fn replace_file_ref_in_messages(
        &self,
        space_id: &str,
        old_name: &str,
        new_name: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET content = REPLACE(content, ?1, ?2)
             WHERE session_id IN (SELECT id FROM sessions WHERE space_id = ?3)",
            (old_name, new_name, space_id),
        )?;
        Ok(())
    }

    /// Replace a file's indexed chunks; identical chunks are left alone and stale
    /// embeddings are dropped.
    pub fn set_file_chunks(&self, file_id: &str, chunks: &[(String, String)]) -> Result<()> {
        let current: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT location, text FROM cache.file_chunks
                 WHERE file_id = ?1 ORDER BY CAST(seq AS INTEGER) ASC",
            )?;
            stmt.query_map([file_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        if current == chunks {
            return Ok(());
        }
        self.conn.execute(
            "DELETE FROM cache.file_chunks WHERE file_id = ?1",
            [file_id],
        )?;
        self.conn.execute(
            "DELETE FROM cache.chunk_embeddings WHERE file_id = ?1",
            [file_id],
        )?;
        for (seq, (location, text)) in chunks.iter().enumerate() {
            self.conn.execute(
                "INSERT INTO cache.file_chunks (file_id, seq, location, text) VALUES (?1, ?2, ?3, ?4)",
                (file_id, seq as i64, location, text),
            )?;
        }
        Ok(())
    }

    /// A file's chunk texts as `(seq, text)`, in order — the embedder's input.
    pub fn file_chunk_texts(&self, file_id: &str) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT CAST(seq AS INTEGER), text FROM cache.file_chunks
             WHERE file_id = ?1 ORDER BY CAST(seq AS INTEGER) ASC",
        )?;
        let rows = stmt.query_map([file_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// See the free function of the same name.
    pub fn files_missing_embeddings(&self, space_id: &str) -> Result<Vec<String>> {
        files_missing_embeddings(&self.conn, space_id)
    }

    /// Record a research report's cited sources; each row gets a UUID `sync_id`
    /// for sync.
    pub fn add_citations(
        &self,
        space_id: &str,
        report_file: &str,
        citations: &[(String, Option<String>)],
    ) -> Result<()> {
        for (url, title) in citations {
            self.conn.execute(
                "INSERT INTO citations (sync_id, space_id, report_file, url, title)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    Uuid::new_v4().to_string(),
                    space_id,
                    report_file,
                    url,
                    title,
                ),
            )?;
        }
        Ok(())
    }

    /// See the free function of the same name.
    pub fn search_citations(
        &self,
        space_id: &str,
        query: Option<&str>,
    ) -> Result<Vec<(String, String, String)>> {
        search_citations(&self.conn, space_id, query)
    }

    /// Store embedding vectors for a file's chunks as `(seq, vector)` pairs.
    pub fn set_chunk_embeddings(&self, file_id: &str, vecs: &[(i64, Vec<f32>)]) -> Result<()> {
        for (seq, v) in vecs {
            self.conn.execute(
                "INSERT OR REPLACE INTO cache.chunk_embeddings (file_id, seq, vec) VALUES (?1, ?2, ?3)",
                (file_id, seq, vec_to_blob(v)),
            )?;
        }
        Ok(())
    }

    /// Remove stale vectors lazily, scoped to the space being indexed.
    pub fn invalidate_embedding_identity(&self, space_id: &str, identity: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM cache.chunk_embeddings WHERE identity != ?2
             AND file_id IN (SELECT id FROM files WHERE space_id = ?1)",
            (space_id, identity),
        )?;
        Ok(())
    }

    /// Files without a complete, atomically stored set for this identity.
    pub fn files_missing_embedding_identity(
        &self,
        space_id: &str,
        identity: &str,
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM files WHERE space_id = ?1 AND NOT EXISTS (
                 SELECT 1 FROM cache.chunk_embeddings WHERE file_id = files.id AND identity = ?2
             ) ORDER BY name",
        )?;
        Ok(stmt
            .query_map((space_id, identity), |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Store a complete file's vectors with their identity and source locators;
    /// embedding chunks are independent of readable chunks.
    pub fn set_file_embeddings(
        &self,
        file_id: &str,
        identity: &str,
        chunks: &[(String, String)],
        vectors: &[(i64, Vec<f32>)],
    ) -> Result<()> {
        anyhow::ensure!(
            chunks.len() == vectors.len(),
            "embedding chunk count mismatch"
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM cache.chunk_embeddings WHERE file_id = ?1",
            [file_id],
        )?;
        for ((location, text), (seq, vector)) in chunks.iter().zip(vectors) {
            tx.execute(
                "INSERT INTO cache.chunk_embeddings (file_id, seq, vec, identity, location, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (file_id, seq, vec_to_blob(vector), identity, location, text),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop all device-local vectors, e.g. when the embedding model changes.
    pub fn clear_chunk_embeddings(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM cache.chunk_embeddings", [])?;
        Ok(())
    }
}

/// Encode an embedding as little-endian f32 bytes for a BLOB column.
pub fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Decode a BLOB back into an embedding (inverse of `vec_to_blob`).
pub fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

/// Citations in a space matching `query` by substring, as
/// `(report_file, url, title)`, newest first.
pub fn search_citations(
    conn: &Connection,
    space_id: &str,
    query: Option<&str>,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT report_file, url, COALESCE(title, '') FROM citations
         WHERE space_id = ?1
           AND (?2 IS NULL OR url LIKE ?2 OR title LIKE ?2 OR report_file LIKE ?2)
         ORDER BY id DESC",
    )?;
    let pattern = query.map(|q| format!("%{q}%"));
    let rows = stmt.query_map((space_id, pattern), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One transcript line for a research stage: bare label, or `label: detail`.
pub fn stage_content(label: &str, detail: &str) -> String {
    if detail.is_empty() {
        label.to_string()
    } else {
        format!("{label}: {detail}")
    }
}

/// See `Db::add_session_sources`; free so the research pipeline can use its own
/// connection.
pub fn add_session_sources(
    conn: &Connection,
    session_id: &str,
    url_norms: &[String],
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    for u in url_norms {
        conn.execute(
            "INSERT OR IGNORE INTO session_sources (session_id, url_norm, updated_at)
             VALUES (?1, ?2, ?3)",
            (session_id, u, &now),
        )?;
    }
    Ok(())
}

/// Substring-search a session's cached source bundle for `(url, text)` pairs; a
/// bundle is a handful of pages, not a corpus.
pub fn search_session_sources(
    conn: &Connection,
    session_id: &str,
    query: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT cache.web_cache.url, cache.web_cache.text FROM session_sources
         JOIN cache.web_cache ON cache.web_cache.url_norm = session_sources.url_norm
         WHERE session_sources.session_id = ?1",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let needle = query.to_lowercase();
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, text)| text.to_lowercase().contains(&needle))
        .collect())
}

/// URLs pinned in a session's source bundle.
pub fn pinned_urls(conn: &Connection, session_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT url_norm FROM session_sources WHERE session_id = ?1 AND flag = 'pinned'",
    )?;
    let rows = stmt.query_map([session_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Distinct hostnames discarded in a session, excluded from later searcher rounds.
pub fn discarded_domains(conn: &Connection, session_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT url_norm FROM session_sources WHERE session_id = ?1 AND flag = 'discarded'",
    )?;
    let rows: Vec<String> = stmt
        .query_map([session_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut hosts: Vec<String> = rows
        .iter()
        .filter_map(|u| {
            reqwest::Url::parse(u)
                .ok()
                .and_then(|p| p.host_str().map(str::to_string))
        })
        .collect();
    hosts.sort();
    hosts.dedup();
    Ok(hosts)
}

/// Whether a cached fetch is under 24h old; unparseable timestamps count as stale.
pub fn is_fresh(fetched_at: &str, now: chrono::DateTime<Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(fetched_at)
        .is_ok_and(|dt| now.signed_duration_since(dt) < chrono::Duration::hours(24))
}

/// A cached page as `(title, text, fetched_at)`, or `None` on a miss; `web_cache`
/// is unqualified so it resolves on either connection.
pub fn cache_get(conn: &Connection, url_norm: &str) -> Result<Option<(String, String, String)>> {
    let row = conn.query_row(
        "SELECT COALESCE(title, ''), text, fetched_at FROM web_cache WHERE url_norm = ?1",
        [url_norm],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    );
    match row {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write (or overwrite) a fetched page into the cache, stamped now.
pub fn cache_put(
    conn: &Connection,
    url_norm: &str,
    url: &str,
    title: Option<&str>,
    text: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO web_cache (url_norm, url, title, text, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(url_norm) DO UPDATE SET url = ?2, title = ?3, text = ?4, fetched_at = ?5",
        (url_norm, url, title, text, &now),
    )?;
    Ok(())
}

/// Ids of files missing a vector for at least one chunk, including files with no
/// chunks.
pub fn files_missing_embeddings(conn: &Connection, space_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT files.id
         FROM files
         WHERE files.space_id = ?1
           AND (
               NOT EXISTS (
                   SELECT 1 FROM cache.file_chunks
                   WHERE cache.file_chunks.file_id = files.id
               )
               OR EXISTS (
                   SELECT 1
                   FROM cache.file_chunks
                   LEFT JOIN cache.chunk_embeddings
                       ON cache.chunk_embeddings.file_id = cache.file_chunks.file_id
                      AND cache.chunk_embeddings.seq = CAST(cache.file_chunks.seq AS INTEGER)
                   WHERE cache.file_chunks.file_id = files.id
                     AND cache.chunk_embeddings.file_id IS NULL
               )
           )
         ORDER BY files.name",
    )?;
    let rows = stmt.query_map([space_id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Cosine-ranked chunk search within one space, best first; mismatched dimensions
/// are skipped.
pub fn semantic_chunks(
    conn: &Connection,
    space_id: &str,
    query: &[f32],
    limit: usize,
) -> Result<Vec<(String, String, String, f32)>> {
    let mut stmt = conn.prepare(
        "SELECT files.name, cache.file_chunks.location, cache.file_chunks.text,
                cache.chunk_embeddings.vec
         FROM cache.chunk_embeddings
         JOIN files ON files.id = cache.chunk_embeddings.file_id
         JOIN cache.file_chunks
             ON cache.file_chunks.file_id = cache.chunk_embeddings.file_id
            AND CAST(cache.file_chunks.seq AS INTEGER) = cache.chunk_embeddings.seq
         WHERE files.space_id = ?1",
    )?;
    let rows = stmt.query_map([space_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Vec<u8>>(3)?,
        ))
    })?;
    let mut hits: Vec<(String, String, String, f32)> = Vec::new();
    for row in rows {
        let (name, loc, text, blob) = row?;
        let v = blob_to_vec(&blob);
        if v.len() != query.len() {
            continue;
        }
        let score = cosine(query, &v);
        hits.push((name, loc, text, score));
    }
    hits.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);
    Ok(hits)
}

/// Rank text and image locators together, accepting only vectors with the query's
/// identity.
pub fn semantic_chunks_for_identity(
    conn: &Connection,
    space_id: &str,
    identity: &str,
    query: &[f32],
    limit: usize,
) -> Result<Vec<(String, String, String, f32)>> {
    let mut stmt = conn.prepare(
        "SELECT files.name, e.location, e.text, e.vec FROM cache.chunk_embeddings e
         JOIN files ON files.id = e.file_id WHERE files.space_id = ?1 AND e.identity = ?2",
    )?;
    let rows = stmt.query_map((space_id, identity), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    })?;
    let mut hits = Vec::new();
    for row in rows {
        let (name, location, text, bytes) = row?;
        let vector = blob_to_vec(&bytes);
        if vector.len() == query.len() && vector.iter().all(|x| x.is_finite()) {
            let score = cosine(query, &vector);
            if score.is_finite() {
                hits.push((name, location, text, score));
            }
        }
    }
    hits.sort_by(|a, b| b.3.total_cmp(&a.3));
    hits.truncate(limit);
    Ok(hits)
}

pub(super) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

/// Quote a query for FTS5 MATCH so model-supplied text cannot be a syntax error.
pub fn fts_quote(query: &str) -> String {
    query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// BM25-ranked chunk search within one space: `(file name, location, snippet)`.
pub fn search_chunks(
    conn: &Connection,
    space_id: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String, String)>> {
    let q = fts_quote(query);
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT files.name, cache.file_chunks.location,
                snippet(file_chunks, 3, '', '', '…', 24)
         FROM cache.file_chunks JOIN files ON files.id = cache.file_chunks.file_id
         WHERE file_chunks MATCH ?1 AND files.space_id = ?2
         ORDER BY bm25(file_chunks) LIMIT ?3",
    )?;
    let rows = stmt.query_map((q, space_id, limit as i64), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A file's full extracted text (chunks re-joined in order), by display name.
pub fn file_text(conn: &Connection, space_id: &str, name: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT cache.file_chunks.text
         FROM cache.file_chunks JOIN files ON files.id = cache.file_chunks.file_id
         WHERE files.space_id = ?1 AND files.name = ?2
           AND cache.file_chunks.location != 'file metadata'
         ORDER BY CAST(cache.file_chunks.seq AS INTEGER) ASC",
    )?;
    let rows = stmt.query_map((space_id, name), |r| r.get::<_, String>(0))?;
    let parts = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((!parts.is_empty()).then(|| parts.join("\n")))
}

/// The indexing status of a named file, or `None` when the space has no such file.
pub fn file_status(conn: &Connection, space_id: &str, name: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(cache.file_index_state.status, 'not indexed')
         FROM files
         LEFT JOIN cache.file_index_state ON cache.file_index_state.file_id = files.id
         WHERE files.space_id = ?1 AND files.name = ?2",
    )?;
    let mut rows = stmt.query_map((space_id, name), |r| r.get::<_, String>(0))?;
    Ok(rows.next().transpose()?)
}

pub fn count_files(conn: &Connection, space_id: &str) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE space_id = ?1",
        [space_id],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}
