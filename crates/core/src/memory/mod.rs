//! Long-term memory for a space, organized like a book: chapters of short facts
//! behind a generated table of contents.

pub mod book;
pub mod recall;

use std::collections::BTreeMap;

use anyhow::{Result, bail, ensure};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_FACT_CHARS: usize = 300;
/// The user's own facts (editor, legacy `memory.md`) may be longer than the model's.
pub const MAX_NOTE_CHARS: usize = 2_000;
pub const MAX_TITLE_CHARS: usize = 60;
pub const MAX_DESCRIPTION_CHARS: usize = 160;
pub const MAX_FACTS: usize = 2000;
/// Chapter size at which the tool and the status bar suggest a tidy.
pub const TIDY_HINT_FACTS: usize = 40;
pub const SHORT_ID: usize = 8;
/// The chapter a legacy `memory.md` is imported into.
pub const ABOUT_YOU: &str = "About you";

/// Which other spaces can read and write a chapter's facts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<String>", into = "Vec<String>")]
pub enum Share {
    #[default]
    Private,
    All,
    Spaces(Vec<String>),
}

impl From<Vec<String>> for Share {
    fn from(ids: Vec<String>) -> Self {
        if ids.iter().any(|id| id == "*") {
            Self::All
        } else if ids.is_empty() {
            Self::Private
        } else {
            Self::Spaces(ids)
        }
    }
}

impl From<Share> for Vec<String> {
    fn from(share: Share) -> Self {
        match share {
            Share::Private => Self::new(),
            Share::All => vec!["*".to_string()],
            Share::Spaces(ids) => ids,
        }
    }
}

/// User-selected sharing targets, using names rather than stored ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShareTarget {
    All,
    None,
    Spaces(Vec<String>),
}

impl ShareTarget {
    /// Resolve user-facing names to stable space ids.
    pub fn resolve(&self, conn: &Connection) -> Result<Share> {
        match self {
            Self::All => Ok(Share::All),
            Self::None => Ok(Share::Private),
            Self::Spaces(names) => {
                let mut ids = Vec::new();
                for name in names {
                    let id = conn
                        .query_row("SELECT id FROM spaces WHERE name = ?1", [name], |row| {
                            row.get::<_, String>(0)
                        })
                        .optional()?;
                    let Some(id) = id else {
                        bail!("no space named {name:?}");
                    };
                    ids.push(id);
                }
                Ok(Share::Spaces(ids))
            }
        }
    }
}

/// One chapter of the book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub id: String,
    pub space_id: String,
    pub title: String,
    /// One line on what the chapter holds; written by whoever creates or tidies it.
    pub description: String,
    /// Pinned chapters are listed in full in every prompt.
    pub pinned: bool,
    #[serde(default)]
    pub shared_with: Share,
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
    #[must_use]
    pub fn short_id(&self) -> &str {
        short(&self.id)
    }
}

#[must_use]
pub fn short(id: &str) -> &str {
    id.get(..SHORT_ID).unwrap_or(id)
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
    "id, space_id, title, description, pinned, created_at, updated_at, deleted_at, shared_with";
const FACT_COLUMNS: &str = "id, space_id, chapter_id, text, created_at, updated_at, deleted_at";

fn chapter_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Chapter> {
    Ok(Chapter {
        id: r.get("id")?,
        space_id: r.get("space_id")?,
        title: r.get("title")?,
        description: r.get("description")?,
        pinned: r.get::<_, i64>("pinned")? != 0,
        // The baseline migration imports legacy memory before the sharing step.
        shared_with: match r.as_ref().column_index("shared_with") {
            Ok(index) => serde_json::from_str(&r.get::<_, String>(index)?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            Err(_) => Share::Private,
        },
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        deleted_at: r.get("deleted_at")?,
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

/// Live facts of a space, oldest first; facts whose chapter is gone are left out.
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

/// Live chapters owned by this space, followed by chapters shared into it.
pub fn visible_chapters(conn: &Connection, space: &str) -> Result<Vec<Chapter>> {
    let mut stmt = conn.prepare(
        "SELECT c.* FROM memory_chapters c
         WHERE c.deleted_at IS NULL AND (c.space_id = ?1 OR
             EXISTS (SELECT 1 FROM json_each(c.shared_with) WHERE value IN ('*', ?1)))
         ORDER BY (c.space_id = ?1) DESC, c.pinned DESC, c.created_at, c.id",
    )?;
    Ok(stmt
        .query_map([space], chapter_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Live facts belonging to visible chapters, regardless of the fact's space id.
pub fn visible_facts(conn: &Connection, space: &str) -> Result<Vec<Fact>> {
    let mut stmt = conn.prepare(
        "SELECT m.* FROM memories m JOIN memory_chapters c ON c.id = m.chapter_id
         WHERE m.deleted_at IS NULL AND c.deleted_at IS NULL AND (c.space_id = ?1 OR
             EXISTS (SELECT 1 FROM json_each(c.shared_with) WHERE value IN ('*', ?1)))
         ORDER BY m.created_at, m.id",
    )?;
    Ok(stmt
        .query_map([space], fact_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// A visible title, preferring a chapter owned by the current space.
pub fn find_visible_chapter(
    conn: &Connection,
    space: &str,
    title: &str,
) -> Result<Option<Chapter>> {
    Ok(visible_chapters(conn, space)?
        .into_iter()
        .find(|chapter| chapter.title.eq_ignore_ascii_case(title.trim())))
}

/// Set sharing for a chapter, validating and canonicalizing target space ids.
pub fn share_chapter(conn: &Connection, chapter_id: &str, share: Share) -> Result<Chapter> {
    let mut chapter = conn.query_row(
        &format!(
            "SELECT {CHAPTER_COLUMNS} FROM memory_chapters WHERE id = ?1 AND deleted_at IS NULL"
        ),
        [chapter_id],
        chapter_row,
    )?;
    let share = match share {
        Share::Spaces(mut ids) => {
            ids.sort();
            ids.dedup();
            for id in &ids {
                ensure!(
                    *id != chapter.space_id,
                    "cannot share a chapter with its home space"
                );
                let exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM spaces WHERE id = ?1)",
                    [id],
                    |row| row.get(0),
                )?;
                ensure!(exists, "no space with id {id:?}");
            }
            Share::from(ids)
        }
        other => other,
    };
    chapter.shared_with = share;
    chapter.updated_at = now();
    conn.execute(
        "UPDATE memory_chapters SET shared_with = ?2, updated_at = ?3 WHERE id = ?1",
        params![
            chapter.id,
            serde_json::to_string(&chapter.shared_with)?,
            chapter.updated_at
        ],
    )?;
    Ok(chapter)
}

/// Display-only suffixes keyed by chapter id; space names are resolved afresh.
pub fn share_labels(
    conn: &Connection,
    space: &str,
    chapters: &[Chapter],
) -> Result<BTreeMap<String, String>> {
    let mut stmt = conn.prepare("SELECT id, name FROM spaces")?;
    let names = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let mut labels = BTreeMap::new();
    for chapter in chapters {
        let label = if chapter.space_id == space {
            let targets = match &chapter.shared_with {
                Share::Private => continue,
                Share::All => "all spaces".to_string(),
                Share::Spaces(ids) => {
                    let mut targets: Vec<&str> = ids
                        .iter()
                        .map(|id| names.get(id).unwrap_or(id).as_str())
                        .collect();
                    targets.sort_unstable();
                    targets.join(", ")
                }
            };
            format!(" (shared with {targets})")
        } else {
            format!(
                " (shared from {})",
                names.get(&chapter.space_id).unwrap_or(&chapter.space_id)
            )
        };
        labels.insert(chapter.id.clone(), label);
    }
    Ok(labels)
}

/// The user-facing list shared by `/memory share` and `nexus memory shares`.
pub fn sharing_list(conn: &Connection, space: &str) -> Result<String> {
    let chapters = visible_chapters(conn, space)?;
    let labels = share_labels(conn, space, &chapters)?;
    let lines: Vec<String> = chapters
        .iter()
        .filter_map(|chapter| {
            labels
                .get(&chapter.id)
                .map(|label| format!("- {}{label}", chapter.title))
        })
        .collect();
    Ok(if lines.is_empty() {
        "no shared memory chapters".to_string()
    } else {
        lines.join("\n")
    })
}

/// A live chapter by title, ignoring case.
pub fn find_chapter(conn: &Connection, space: &str, title: &str) -> Result<Option<Chapter>> {
    Ok(conn
        .query_row(
            "SELECT * FROM memory_chapters
             WHERE space_id = ?1 AND deleted_at IS NULL AND lower(title) = lower(?2)
             ORDER BY created_at LIMIT 1",
            params![space, title.trim()],
            chapter_row,
        )
        .optional()?)
}

/// The live chapter with this title, created when missing; a given description or
/// pin state is applied to an existing chapter too.
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
        shared_with: Share::Private,
        created_at: stamp.clone(),
        updated_at: stamp,
        deleted_at: None,
    };
    conn.execute(
        "INSERT INTO memory_chapters
         (id, space_id, title, description, pinned, created_at, updated_at, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
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

#[derive(Debug)]
pub struct Added {
    pub fact: Fact,
    pub chapter: Chapter,
    /// Live facts in the chapter afterwards.
    pub count: usize,
    /// The same text was already saved; nothing new was written.
    pub duplicate: bool,
}

/// Save a fact under a chapter (created when missing); text already in the space
/// returns the existing fact.
pub fn add(
    conn: &Connection,
    space: &str,
    chapter_title: &str,
    text: &str,
    description: Option<&str>,
    pinned: Option<bool>,
) -> Result<Added> {
    let shared = find_visible_chapter(conn, space, chapter_title)?
        .filter(|chapter| chapter.space_id != space);
    let (home, description, pinned) = shared
        .as_ref()
        .map_or((space, description, pinned), |chapter| {
            (chapter.space_id.as_str(), None, None)
        });
    add_capped(
        conn,
        home,
        chapter_title,
        text,
        description,
        pinned,
        MAX_FACT_CHARS,
    )
}

/// [`add`] for text the user wrote (editor, legacy import): up to [`MAX_NOTE_CHARS`].
pub fn add_note(
    conn: &Connection,
    space: &str,
    chapter_title: &str,
    text: &str,
    description: Option<&str>,
    pinned: Option<bool>,
) -> Result<Added> {
    add_capped(
        conn,
        space,
        chapter_title,
        text,
        description,
        pinned,
        MAX_NOTE_CHARS,
    )
}

fn add_capped(
    conn: &Connection,
    space: &str,
    chapter_title: &str,
    text: &str,
    description: Option<&str>,
    pinned: Option<bool>,
    max: usize,
) -> Result<Added> {
    let text = clean(text, max, "fact")?;
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
    ensure_capacity(conn, space)?;
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

fn ensure_capacity(conn: &Connection, space: &str) -> Result<()> {
    let live: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE space_id = ?1 AND deleted_at IS NULL",
        [space],
        |r| r.get(0),
    )?;
    ensure!(
        usize::try_from(live).unwrap_or(usize::MAX) < MAX_FACTS,
        "memory is full ({MAX_FACTS} facts) — merge or delete facts first"
    );
    Ok(())
}

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
    let matches: Vec<Fact> = visible_facts(conn, space)?
        .into_iter()
        .filter(|f| f.id.starts_with(id))
        .collect();
    match matches.as_slice() {
        [fact] => Ok(fact.clone()),
        [] => bail!("no fact with id {id}"),
        _ => bail!("fact id {id} is ambiguous — use more characters"),
    }
}

/// Change a fact's text and/or move it to another chapter (created when missing).
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
        let chapter = match find_visible_chapter(conn, space, title)? {
            Some(chapter) => chapter,
            None => ensure_chapter(conn, space, title, None, None)?,
        };
        if fact.space_id != chapter.space_id {
            ensure_capacity(conn, &chapter.space_id)?;
        }
        fact.chapter_id = chapter.id;
        fact.space_id = chapter.space_id;
    }
    fact.updated_at = now();
    conn.execute(
        "UPDATE memories SET text = ?2, chapter_id = ?3, updated_at = ?4, space_id = ?5 WHERE id = ?1",
        params![fact.id, fact.text, fact.chapter_id, fact.updated_at, fact.space_id],
    )?;
    Ok(fact)
}

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
    conn.execute(
        "UPDATE memory_chapters SET shared_with = (
             SELECT json_group_array(value) FROM json_each(memory_chapters.shared_with) WHERE value != ?1
         ), updated_at = ?2
         WHERE EXISTS (SELECT 1 FROM json_each(memory_chapters.shared_with) WHERE value = ?1)",
        params![space, stamp],
    )?;
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

/// Every memory row of a space, deleted ones included: what `/memory undo` restores.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub chapters: Vec<Chapter>,
    pub facts: Vec<Fact>,
}

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

/// Put every row of a snapshot back as it was. Rows are re-stamped so the restore
/// wins over the change it undoes on every synced device.
pub fn restore(conn: &Connection, snapshot: &Snapshot) -> Result<()> {
    let stamp = now();
    for c in &snapshot.chapters {
        conn.execute(
            &format!(
                "INSERT INTO memory_chapters ({CHAPTER_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET title = excluded.title,
                     description = excluded.description, pinned = excluded.pinned,
                     shared_with = excluded.shared_with,
                     updated_at = excluded.updated_at, deleted_at = excluded.deleted_at"
            ),
            params![
                c.id,
                c.space_id,
                c.title,
                c.description,
                i64::from(c.pinned),
                c.created_at,
                stamp,
                c.deleted_at,
                serde_json::to_string(&c.shared_with)?
            ],
        )?;
    }
    for f in &snapshot.facts {
        conn.execute(
            &format!(
                "INSERT INTO memories ({FACT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET chapter_id = excluded.chapter_id,
                     space_id = excluded.space_id,
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

/// Import a legacy numbered `memory.md` (`1. fact` lines) into a pinned "About you"
/// chapter once, renaming the file to `memory.md.imported`; returns how many facts
/// were saved.
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
        let fact: String = fact.chars().take(MAX_NOTE_CHARS).collect();
        if add_note(conn, space, ABOUT_YOU, &fact, None, Some(true)).is_ok_and(|a| !a.duplicate) {
            saved += 1;
        }
    }
    let mut imported = path.as_os_str().to_owned();
    imported.push(".imported");
    std::fs::rename(path, imported)?;
    Ok(saved)
}

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
use crate::db::migrations::migrate_durable as migrate;

#[cfg(test)]
mod tests;
