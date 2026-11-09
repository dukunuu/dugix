//! Long-term memory for a space, organized like a book: chapters of short
//! facts behind a generated table of contents (`docs/memory.md`).
//!
//! Both tables live in the durable `nexus.db` and sync as last-writer-wins
//! rows (`sync.rs`). A delete sets `deleted_at` and bumps `updated_at`, so it
//! travels like any edit and a tidy can be undone. Functions take a plain
//! connection so the TUI, the CLI, the host and per-call tool connections
//! share one implementation.

pub mod book;
pub mod recall;
pub mod tidy;

use anyhow::{Result, bail, ensure};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Longest fact, in characters.
pub const MAX_FACT_CHARS: usize = 300;
/// Longest chapter title, in characters.
pub const MAX_TITLE_CHARS: usize = 60;
/// Longest chapter description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 160;
/// Most live facts one space keeps.
pub const MAX_FACTS: usize = 2000;
/// Chapter size at which the tool and the status bar suggest a tidy.
pub const TIDY_HINT_FACTS: usize = 40;
/// Characters of a fact id shown to the model and the user.
pub const SHORT_ID: usize = 8;
/// The chapter a legacy `memory.md` is imported into.
pub const ABOUT_YOU: &str = "About you";

/// One chapter of the book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub id: String,
    pub space_id: String,
    pub title: String,
    /// One line on what the chapter holds; written by whoever creates or
    /// tidies it.
    pub description: String,
    /// Pinned chapters are listed in full in every prompt.
    pub pinned: bool,
    /// When `/memory tidy` last reviewed this chapter.
    pub tidied_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

/// One fact, filed under a chapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub id: String,
    pub space_id: String,
    pub chapter_id: String,
    pub text: String,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

impl Fact {
    /// The id prefix shown to the model and in previews.
    #[must_use]
    pub fn short_id(&self) -> &str {
        short(&self.id)
    }
}

/// The first [`SHORT_ID`] characters of an id.
#[must_use]
pub fn short(id: &str) -> &str {
    id.get(..SHORT_ID).unwrap_or(id)
}

/// Create the tables on an existing or fresh durable db.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_chapters (
            id          TEXT PRIMARY KEY,
            space_id    TEXT NOT NULL,
            title       TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            pinned      INTEGER NOT NULL DEFAULT 0,
            tidied_at   TEXT,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL,
            deleted_at  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_memory_chapters_space
            ON memory_chapters(space_id);
        CREATE TABLE IF NOT EXISTS memories (
            id          TEXT PRIMARY KEY,
            space_id    TEXT NOT NULL,
            chapter_id  TEXT NOT NULL,
            text        TEXT NOT NULL,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL,
            deleted_at  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_memories_space ON memories(space_id);
        CREATE INDEX IF NOT EXISTS idx_memories_chapter ON memories(chapter_id);",
    )?;
    Ok(())
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Trim to one line and check the length.
fn clean(text: &str, max: usize, what: &str) -> Result<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!text.is_empty(), "{what} is empty");
    ensure!(
        text.chars().count() <= max,
        "{what} is longer than {max} characters"
    );
    Ok(text)
}

const CHAPTER_COLUMNS: &str =
    "id, space_id, title, description, pinned, tidied_at, created_at, updated_at, deleted_at";
const FACT_COLUMNS: &str = "id, space_id, chapter_id, text, created_at, updated_at, deleted_at";

fn chapter_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Chapter> {
    Ok(Chapter {
        id: r.get(0)?,
        space_id: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        pinned: r.get::<_, i64>(4)? != 0,
        tidied_at: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
        deleted_at: r.get(8)?,
    })
}

fn fact_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Fact> {
    Ok(Fact {
        id: r.get(0)?,
        space_id: r.get(1)?,
        chapter_id: r.get(2)?,
        text: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        deleted_at: r.get(6)?,
    })
}

/// Live chapters of a space: pinned first, then in creation order.
pub fn chapters(conn: &Connection, space: &str) -> Result<Vec<Chapter>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHAPTER_COLUMNS} FROM memory_chapters
         WHERE space_id = ?1 AND deleted_at IS NULL
         ORDER BY pinned DESC, created_at, id"
    ))?;
    let rows = stmt.query_map([space], chapter_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Live facts of a space, oldest first. Facts whose chapter is gone are
/// left out.
pub fn facts(conn: &Connection, space: &str) -> Result<Vec<Fact>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM memories m
         JOIN memory_chapters c ON c.id = m.chapter_id AND c.deleted_at IS NULL
         WHERE m.space_id = ?1 AND m.deleted_at IS NULL
         ORDER BY m.created_at, m.id",
        FACT_COLUMNS
            .split(", ")
            .map(|column| format!("m.{column}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))?;
    let rows = stmt.query_map([space], fact_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A live chapter by title, ignoring case.
pub fn find_chapter(conn: &Connection, space: &str, title: &str) -> Result<Option<Chapter>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {CHAPTER_COLUMNS} FROM memory_chapters
                 WHERE space_id = ?1 AND deleted_at IS NULL AND lower(title) = lower(?2)
                 ORDER BY created_at LIMIT 1"
            ),
            params![space, title.trim()],
            chapter_row,
        )
        .optional()?)
}

/// The live chapter with this title, created when missing. A given
/// description or pin state is applied to an existing chapter too.
pub fn ensure_chapter(
    conn: &Connection,
    space: &str,
    title: &str,
    description: Option<&str>,
    pinned: Option<bool>,
) -> Result<Chapter> {
    let title = clean(title, MAX_TITLE_CHARS, "chapter title")?;
    let description = description
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| !d.is_empty())
        .map(|d| clean(&d, MAX_DESCRIPTION_CHARS, "description"))
        .transpose()?;
    if let Some(mut chapter) = find_chapter(conn, space, &title)? {
        let changed_description = description
            .as_ref()
            .is_some_and(|d| *d != chapter.description);
        let changed_pin = pinned.is_some_and(|p| p != chapter.pinned);
        if changed_description || changed_pin {
            if let Some(d) = description {
                chapter.description = d;
            }
            if let Some(p) = pinned {
                chapter.pinned = p;
            }
            chapter.updated_at = now();
            conn.execute(
                "UPDATE memory_chapters SET description = ?2, pinned = ?3, updated_at = ?4
                 WHERE id = ?1",
                params![
                    chapter.id,
                    chapter.description,
                    i64::from(chapter.pinned),
                    chapter.updated_at
                ],
            )?;
        }
        return Ok(chapter);
    }
    let stamp = now();
    let chapter = Chapter {
        id: Uuid::new_v4().to_string(),
        space_id: space.to_string(),
        title,
        description: description.unwrap_or_default(),
        pinned: pinned.unwrap_or(false),
        tidied_at: None,
        created_at: stamp.clone(),
        updated_at: stamp,
        deleted_at: None,
    };
    conn.execute(
        &format!(
            "INSERT INTO memory_chapters ({CHAPTER_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, NULL)"
        ),
        params![
            chapter.id,
            chapter.space_id,
            chapter.title,
            chapter.description,
            i64::from(chapter.pinned),
            chapter.created_at,
            chapter.updated_at
        ],
    )?;
    Ok(chapter)
}

/// What [`add`] did.
#[derive(Debug)]
pub struct Added {
    pub fact: Fact,
    pub chapter: Chapter,
    /// Live facts in the chapter afterwards.
    pub count: usize,
    /// The same text was already saved; nothing new was written.
    pub duplicate: bool,
}

/// Save a fact under a chapter (created when missing). Saving text that is
/// already in the space returns the existing fact.
pub fn add(
    conn: &Connection,
    space: &str,
    chapter_title: &str,
    text: &str,
    description: Option<&str>,
    pinned: Option<bool>,
) -> Result<Added> {
    let text = clean(text, MAX_FACT_CHARS, "fact")?;
    let chapter = ensure_chapter(conn, space, chapter_title, description, pinned)?;
    let existing = facts(conn, space)?
        .into_iter()
        .find(|f| f.text.eq_ignore_ascii_case(&text));
    if let Some(fact) = existing {
        let count = chapter_count(conn, &fact.chapter_id)?;
        return Ok(Added {
            fact,
            chapter,
            count,
            duplicate: true,
        });
    }
    let live: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE space_id = ?1 AND deleted_at IS NULL",
        [space],
        |r| r.get(0),
    )?;
    ensure!(
        usize::try_from(live).unwrap_or(usize::MAX) < MAX_FACTS,
        "memory is full ({MAX_FACTS} facts) — merge or delete facts first"
    );
    let stamp = now();
    let fact = Fact {
        id: Uuid::new_v4().to_string(),
        space_id: space.to_string(),
        chapter_id: chapter.id.clone(),
        text,
        created_at: stamp.clone(),
        updated_at: stamp,
        deleted_at: None,
    };
    conn.execute(
        &format!("INSERT INTO memories ({FACT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)"),
        params![
            fact.id,
            fact.space_id,
            fact.chapter_id,
            fact.text,
            fact.created_at,
            fact.updated_at
        ],
    )?;
    let count = chapter_count(conn, &chapter.id)?;
    Ok(Added {
        fact,
        chapter,
        count,
        duplicate: false,
    })
}

/// Live facts in one chapter.
pub fn chapter_count(conn: &Connection, chapter_id: &str) -> Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE chapter_id = ?1 AND deleted_at IS NULL",
        [chapter_id],
        |r| r.get(0),
    )?;
    Ok(usize::try_from(n).unwrap_or(0))
}

/// A live fact by full id or a unique prefix of at least four characters.
pub fn resolve_fact(conn: &Connection, space: &str, id: &str) -> Result<Fact> {
    let id = id.trim().trim_start_matches('#');
    ensure!(id.len() >= 4, "fact id {id:?} is too short");
    let matches: Vec<Fact> = facts(conn, space)?
        .into_iter()
        .filter(|f| f.id.starts_with(id))
        .collect();
    match matches.as_slice() {
        [fact] => Ok(fact.clone()),
        [] => bail!("no fact with id {id}"),
        _ => bail!("fact id {id} is ambiguous — use more characters"),
    }
}

/// Change a fact's text and/or move it to another chapter (created when
/// missing).
pub fn update_fact(
    conn: &Connection,
    space: &str,
    id: &str,
    text: Option<&str>,
    chapter_title: Option<&str>,
) -> Result<Fact> {
    let mut fact = resolve_fact(conn, space, id)?;
    ensure!(
        text.is_some() || chapter_title.is_some(),
        "nothing to update — give text or chapter"
    );
    if let Some(text) = text {
        fact.text = clean(text, MAX_FACT_CHARS, "fact")?;
    }
    if let Some(title) = chapter_title {
        fact.chapter_id = ensure_chapter(conn, space, title, None, None)?.id;
    }
    fact.updated_at = now();
    conn.execute(
        "UPDATE memories SET text = ?2, chapter_id = ?3, updated_at = ?4 WHERE id = ?1",
        params![fact.id, fact.text, fact.chapter_id, fact.updated_at],
    )?;
    Ok(fact)
}

/// Mark one fact deleted.
pub fn delete_fact(conn: &Connection, space: &str, id: &str) -> Result<Fact> {
    let fact = resolve_fact(conn, space, id)?;
    tombstone_fact(conn, &fact.id)?;
    Ok(fact)
}

fn tombstone_fact(conn: &Connection, id: &str) -> Result<()> {
    let stamp = now();
    conn.execute(
        "UPDATE memories SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
        params![id, stamp],
    )?;
    Ok(())
}

/// Rename a chapter, change its description or pin state.
pub fn update_chapter(
    conn: &Connection,
    space: &str,
    title: &str,
    new_title: Option<&str>,
    description: Option<&str>,
    pinned: Option<bool>,
) -> Result<Chapter> {
    let Some(mut chapter) = find_chapter(conn, space, title)? else {
        bail!("no chapter titled {title:?}");
    };
    if let Some(new_title) = new_title {
        let new_title = clean(new_title, MAX_TITLE_CHARS, "chapter title")?;
        if let Some(other) = find_chapter(conn, space, &new_title)?
            && other.id != chapter.id
        {
            bail!("a chapter titled {new_title:?} already exists — merge instead");
        }
        chapter.title = new_title;
    }
    if let Some(description) = description {
        chapter.description = if description.trim().is_empty() {
            String::new()
        } else {
            clean(description, MAX_DESCRIPTION_CHARS, "description")?
        };
    }
    if let Some(pinned) = pinned {
        chapter.pinned = pinned;
    }
    chapter.updated_at = now();
    conn.execute(
        "UPDATE memory_chapters SET title = ?2, description = ?3, pinned = ?4, updated_at = ?5
         WHERE id = ?1",
        params![
            chapter.id,
            chapter.title,
            chapter.description,
            i64::from(chapter.pinned),
            chapter.updated_at
        ],
    )?;
    Ok(chapter)
}

/// Mark a chapter and every fact in it deleted.
pub fn delete_chapter(conn: &Connection, chapter_id: &str) -> Result<()> {
    let stamp = now();
    conn.execute(
        "UPDATE memories SET deleted_at = ?2, updated_at = ?2
         WHERE chapter_id = ?1 AND deleted_at IS NULL",
        params![chapter_id, stamp],
    )?;
    conn.execute(
        "UPDATE memory_chapters SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
        params![chapter_id, stamp],
    )?;
    Ok(())
}

/// Move every fact of `from` into `into`, then delete `from`.
pub fn merge_chapters(conn: &Connection, from: &str, into: &str) -> Result<()> {
    ensure!(from != into, "cannot merge a chapter into itself");
    let stamp = now();
    conn.execute(
        "UPDATE memories SET chapter_id = ?2, updated_at = ?3
         WHERE chapter_id = ?1 AND deleted_at IS NULL",
        params![from, into, stamp],
    )?;
    delete_chapter(conn, from)
}

/// Mark every memory row of a deleted space deleted, so the delete syncs.
pub fn delete_space(conn: &Connection, space: &str) -> Result<()> {
    let stamp = now();
    for table in ["memories", "memory_chapters"] {
        conn.execute(
            &format!(
                "UPDATE {table} SET deleted_at = ?2, updated_at = ?2
                 WHERE space_id = ?1 AND deleted_at IS NULL"
            ),
            params![space, stamp],
        )?;
    }
    Ok(())
}

/// Every memory row of a space, deleted ones included: what `/memory undo`
/// restores.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub chapters: Vec<Chapter>,
    pub facts: Vec<Fact>,
}

/// All rows of a space, deleted ones included.
pub fn snapshot(conn: &Connection, space: &str) -> Result<Snapshot> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHAPTER_COLUMNS} FROM memory_chapters WHERE space_id = ?1 ORDER BY created_at, id"
    ))?;
    let chapters = stmt
        .query_map([space], chapter_row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {FACT_COLUMNS} FROM memories WHERE space_id = ?1 ORDER BY created_at, id"
    ))?;
    let facts = stmt
        .query_map([space], fact_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Snapshot { chapters, facts })
}

/// Put every row of a snapshot back as it was. Rows are re-stamped so the
/// restore wins over the change it undoes on every synced device; rows
/// created after the snapshot are left alone.
pub fn restore(conn: &Connection, snapshot: &Snapshot) -> Result<()> {
    let stamp = now();
    for c in &snapshot.chapters {
        conn.execute(
            &format!(
                "INSERT INTO memory_chapters ({CHAPTER_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET title = excluded.title,
                     description = excluded.description, pinned = excluded.pinned,
                     tidied_at = excluded.tidied_at, updated_at = excluded.updated_at,
                     deleted_at = excluded.deleted_at"
            ),
            params![
                c.id,
                c.space_id,
                c.title,
                c.description,
                i64::from(c.pinned),
                c.tidied_at,
                c.created_at,
                stamp,
                c.deleted_at
            ],
        )?;
    }
    for f in &snapshot.facts {
        conn.execute(
            &format!(
                "INSERT INTO memories ({FACT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET chapter_id = excluded.chapter_id,
                     text = excluded.text, updated_at = excluded.updated_at,
                     deleted_at = excluded.deleted_at"
            ),
            params![
                f.id,
                f.space_id,
                f.chapter_id,
                f.text,
                f.created_at,
                stamp,
                f.deleted_at
            ],
        )?;
    }
    Ok(())
}

/// Import a legacy numbered `memory.md` (`1. fact` lines) into a pinned
/// "About you" chapter, once: the file is renamed to `memory.md.imported`
/// afterwards. Returns how many facts were saved.
pub fn import_legacy(conn: &Connection, space: &str, path: &std::path::Path) -> Result<usize> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(0);
    };
    let mut saved = 0;
    for line in text.lines() {
        let line = line.trim();
        let fact = line
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .map_or(line, |(_, rest)| rest)
            .trim();
        if fact.is_empty() || fact.starts_with("<!--") {
            continue;
        }
        let fact: String = fact.chars().take(MAX_FACT_CHARS).collect();
        if add(conn, space, ABOUT_YOU, &fact, None, Some(true)).is_ok_and(|a| !a.duplicate) {
            saved += 1;
        }
    }
    let mut imported = path.as_os_str().to_owned();
    imported.push(".imported");
    std::fs::rename(path, imported)?;
    Ok(saved)
}

/// When a chapter or any of its facts last changed.
#[must_use]
pub fn last_change<'a>(chapter: &'a Chapter, facts: &'a [Fact]) -> &'a str {
    facts
        .iter()
        .filter(|f| f.chapter_id == chapter.id)
        .map(|f| f.updated_at.as_str())
        .chain(std::iter::once(chapter.updated_at.as_str()))
        .max()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
