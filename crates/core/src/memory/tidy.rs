//! `/memory tidy`: the helper model reviews changed chapters one at a time,
//! then the chapter titles as a whole. The result is a [`Plan`] the user
//! previews and applies or discards; nothing changes before that.

use std::fmt::Write as _;

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Deserialize;

use super::{Chapter, Fact};

/// A change to the facts of one chapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactOp {
    Delete {
        id: String,
        why: String,
    },
    Update {
        id: String,
        text: String,
    },
    /// Replace several facts with one: the first is rewritten, the rest go.
    Merge {
        ids: Vec<String>,
        text: String,
    },
    /// Facts that disagree; shown to the user, never applied.
    Conflict {
        ids: Vec<String>,
        note: String,
    },
}

/// A change to the chapter list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChapterOp {
    Merge { from: String, into: String },
    Rename { from: String, to: String },
}

/// The review of one chapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChapterPlan {
    pub chapter_id: String,
    pub title: String,
    pub description: Option<String>,
    pub ops: Vec<FactOp>,
}

/// Everything a tidy would change in one space.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub space_id: String,
    pub chapters: Vec<ChapterPlan>,
    pub book: Vec<ChapterOp>,
}

impl Plan {
    /// Whether applying would change anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.book.is_empty()
            && self
                .chapters
                .iter()
                .all(|c| c.description.is_none() && c.ops.is_empty())
    }
}

/// Chapters worth a review: two or more facts and a change since the last
/// tidy.
#[must_use]
pub fn changed<'a>(chapters: &'a [Chapter], facts: &[Fact]) -> Vec<&'a Chapter> {
    chapters
        .iter()
        .filter(|chapter| facts.iter().filter(|f| f.chapter_id == chapter.id).count() >= 2)
        .filter(|chapter| {
            chapter
                .tidied_at
                .as_deref()
                .is_none_or(|tidied| super::last_change(chapter, facts) > tidied)
        })
        .collect()
}

/// The review prompt for one chapter.
#[must_use]
pub fn chapter_prompt(chapter: &Chapter, facts: &[Fact]) -> String {
    let mut listed = String::new();
    for fact in facts.iter().filter(|f| f.chapter_id == chapter.id) {
        let _ = writeln!(listed, "{} {}", fact.short_id(), fact.text);
    }
    crate::prompts::render(
        crate::prompts::MEMORY_TIDY,
        &[
            ("title", &chapter.title),
            (
                "description",
                if chapter.description.is_empty() {
                    "(none)"
                } else {
                    &chapter.description
                },
            ),
            ("facts", listed.trim_end()),
        ],
    )
}

/// The prompt that reviews chapter titles.
#[must_use]
pub fn book_prompt(chapters: &[Chapter], facts: &[Fact]) -> String {
    let mut listed = String::new();
    for chapter in chapters {
        let count = facts.iter().filter(|f| f.chapter_id == chapter.id).count();
        let _ = writeln!(
            listed,
            "- {} ({count}) — {}",
            chapter.title, chapter.description
        );
    }
    crate::prompts::render(
        crate::prompts::MEMORY_BOOK,
        &[("chapters", listed.trim_end())],
    )
}

fn json_object(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

#[derive(Deserialize)]
struct RawOp {
    op: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    into: String,
    #[serde(default)]
    to: String,
}

/// Parse one chapter's review. Short ids are mapped back to the chapter's
/// facts; ops naming anything else are dropped, as is a malformed reply.
#[must_use]
pub fn parse_chapter(reply: &str, chapter: &Chapter, facts: &[Fact]) -> ChapterPlan {
    let in_chapter: Vec<&Fact> = facts
        .iter()
        .filter(|f| f.chapter_id == chapter.id)
        .collect();
    let full = |id: &str| {
        let id = id.trim().trim_start_matches('#');
        (id.len() >= 4)
            .then(|| {
                in_chapter
                    .iter()
                    .find(|f| f.id.starts_with(id))
                    .map(|f| f.id.clone())
            })
            .flatten()
    };
    let mut plan = ChapterPlan {
        chapter_id: chapter.id.clone(),
        title: chapter.title.clone(),
        description: None,
        ops: Vec::new(),
    };
    let Some(value) = json_object(reply) else {
        return plan;
    };
    plan.description = value["description"]
        .as_str()
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| {
            !d.is_empty()
                && *d != chapter.description
                && d.chars().count() <= super::MAX_DESCRIPTION_CHARS
        });
    let raw: Vec<RawOp> = serde_json::from_value(value["ops"].clone()).unwrap_or_default();
    let mut touched = std::collections::HashSet::new();
    for op in raw {
        let text = op.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let ids: Vec<String> = op.ids.iter().filter_map(|id| full(id)).collect();
        let parsed = match op.op.as_str() {
            "delete" => full(&op.id).map(|id| FactOp::Delete {
                id,
                why: op.why.trim().to_string(),
            }),
            "update" if !text.is_empty() && text.chars().count() <= super::MAX_FACT_CHARS => {
                full(&op.id).map(|id| FactOp::Update { id, text })
            }
            "merge"
                if ids.len() >= 2
                    && !text.is_empty()
                    && text.chars().count() <= super::MAX_FACT_CHARS =>
            {
                Some(FactOp::Merge { ids, text })
            }
            "conflict" if ids.len() >= 2 => Some(FactOp::Conflict {
                ids,
                note: op.note.trim().to_string(),
            }),
            _ => None,
        };
        // One change per fact: a later op touching the same fact is dropped.
        if let Some(parsed) = parsed {
            let ids: Vec<&String> = match &parsed {
                FactOp::Delete { id, .. } | FactOp::Update { id, .. } => vec![id],
                FactOp::Merge { ids, .. } => ids.iter().collect(),
                FactOp::Conflict { .. } => Vec::new(),
            };
            if ids.iter().all(|id| !touched.contains(*id)) {
                touched.extend(ids.into_iter().cloned());
                plan.ops.push(parsed);
            }
        }
    }
    plan
}

/// Parse the chapter-title review; ops naming unknown chapters are dropped.
#[must_use]
pub fn parse_book(reply: &str, chapters: &[Chapter]) -> Vec<ChapterOp> {
    let known = |title: &str| {
        chapters
            .iter()
            .find(|c| c.title.eq_ignore_ascii_case(title.trim()))
            .map(|c| c.title.clone())
    };
    let Some(value) = json_object(reply) else {
        return Vec::new();
    };
    let raw: Vec<RawOp> = serde_json::from_value(value["ops"].clone()).unwrap_or_default();
    let mut used = std::collections::HashSet::new();
    let mut ops = Vec::new();
    for op in raw {
        let parsed = match op.op.as_str() {
            "merge" => known(&op.from)
                .zip(known(&op.into))
                .filter(|(from, into)| from != into)
                .map(|(from, into)| ChapterOp::Merge { from, into }),
            "rename" => {
                let to = op.to.split_whitespace().collect::<Vec<_>>().join(" ");
                known(&op.from)
                    .filter(|_| {
                        !to.is_empty()
                            && to.chars().count() <= super::MAX_TITLE_CHARS
                            && known(&to).is_none()
                    })
                    .map(|from| ChapterOp::Rename { from, to })
            }
            _ => None,
        };
        if let Some(parsed) = parsed {
            let from = match &parsed {
                ChapterOp::Merge { from, .. } | ChapterOp::Rename { from, .. } => from.clone(),
            };
            if used.insert(from) {
                ops.push(parsed);
            }
        }
    }
    ops
}

/// The plan as the user reviews it.
#[must_use]
pub fn preview(plan: &Plan, facts: &[Fact]) -> String {
    let text = |id: &str| {
        facts
            .iter()
            .find(|f| f.id == id)
            .map_or_else(String::new, |f| f.text.clone())
    };
    let mut out = String::new();
    let reviewed = plan.chapters.len();
    let _ = writeln!(
        out,
        "memory tidy · {reviewed} chapter{} reviewed",
        if reviewed == 1 { "" } else { "s" }
    );
    for chapter in &plan.chapters {
        if chapter.description.is_none() && chapter.ops.is_empty() {
            continue;
        }
        let before = facts
            .iter()
            .filter(|f| f.chapter_id == chapter.chapter_id)
            .count();
        let removed: usize = chapter
            .ops
            .iter()
            .map(|op| match op {
                FactOp::Delete { .. } => 1,
                FactOp::Merge { ids, .. } => ids.len() - 1,
                _ => 0,
            })
            .sum();
        let _ = writeln!(
            out,
            "\n{} ({before} → {} facts)",
            chapter.title,
            before - removed
        );
        if let Some(description) = &chapter.description {
            let _ = writeln!(out, "  description: {description}");
        }
        for op in &chapter.ops {
            let _ = match op {
                FactOp::Delete { id, why } => writeln!(
                    out,
                    "  - {}{}",
                    text(id),
                    if why.is_empty() {
                        String::new()
                    } else {
                        format!("  ({why})")
                    }
                ),
                FactOp::Update { id, text: new } => {
                    writeln!(out, "  ~ {}  →  {new}", text(id))
                }
                FactOp::Merge { ids, text: new } => writeln!(
                    out,
                    "  ~ {}  →  {new}",
                    ids.iter()
                        .map(|id| text(id))
                        .collect::<Vec<_>>()
                        .join(" + ")
                ),
                FactOp::Conflict { ids, note } => writeln!(
                    out,
                    "  ? {}{}  — decide with /memory edit",
                    ids.iter()
                        .map(|id| format!("\"{}\"", text(id)))
                        .collect::<Vec<_>>()
                        .join(" vs "),
                    if note.is_empty() {
                        String::new()
                    } else {
                        format!(": {note}")
                    }
                ),
            };
        }
    }
    if !plan.book.is_empty() {
        out.push_str("\nchapters\n");
        for op in &plan.book {
            let _ = match op {
                ChapterOp::Merge { from, into } => writeln!(out, "  ~ {from} + {into}  →  {into}"),
                ChapterOp::Rename { from, to } => writeln!(out, "  ~ rename {from}  →  {to}"),
            };
        }
    }
    if plan.is_empty() {
        out.push_str("\nnothing to change");
    }
    out.trim_end().to_string()
}

/// What applying a plan did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub removed: usize,
    pub rewritten: usize,
    pub chapters_changed: usize,
}

/// Apply a plan. Facts deleted since the plan was made, and chapters
/// renamed or removed since, are skipped. Reviewed chapters are stamped so the
/// next tidy skips them until they change again.
pub fn apply(conn: &Connection, plan: &Plan) -> Result<Applied> {
    let space = plan.space_id.as_str();
    let mut applied = Applied::default();
    let live =
        |id: &str| -> Result<bool> { Ok(super::facts(conn, space)?.iter().any(|f| f.id == id)) };
    for chapter in &plan.chapters {
        for op in &chapter.ops {
            match op {
                FactOp::Delete { id, .. } if live(id)? => {
                    super::delete_fact(conn, space, id)?;
                    applied.removed += 1;
                }
                FactOp::Update { id, text } if live(id)? => {
                    super::update_fact(conn, space, id, Some(text), None)?;
                    applied.rewritten += 1;
                }
                FactOp::Merge { ids, text } => {
                    let live_ids: Vec<&String> =
                        ids.iter().filter(|id| live(id).unwrap_or(false)).collect();
                    if let Some((first, rest)) = live_ids.split_first() {
                        super::update_fact(conn, space, first, Some(text), None)?;
                        applied.rewritten += 1;
                        for id in rest {
                            super::delete_fact(conn, space, id)?;
                            applied.removed += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        let stamp = chrono::Utc::now().to_rfc3339();
        if let Some(description) = &chapter.description {
            conn.execute(
                "UPDATE memory_chapters SET description = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                params![chapter.chapter_id, description],
            )?;
        }
        conn.execute(
            "UPDATE memory_chapters SET tidied_at = ?2, updated_at = ?2
             WHERE id = ?1 AND deleted_at IS NULL",
            params![chapter.chapter_id, stamp],
        )?;
    }
    for op in &plan.book {
        match op {
            ChapterOp::Merge { from, into } => {
                if let (Some(from), Some(into)) = (
                    super::find_chapter(conn, space, from)?,
                    super::find_chapter(conn, space, into)?,
                ) {
                    super::merge_chapters(conn, &from.id, &into.id)?;
                    applied.chapters_changed += 1;
                }
            }
            ChapterOp::Rename { from, to } => {
                if super::find_chapter(conn, space, from)?.is_some()
                    && super::update_chapter(conn, space, from, Some(to), None, None).is_ok()
                {
                    applied.chapters_changed += 1;
                }
            }
        }
    }
    Ok(applied)
}

impl std::fmt::Display for Applied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "memory tidied · {} removed · {} rewritten · {} chapters changed · /memory undo restores",
            self.removed, self.rewritten, self.chapters_changed
        )
    }
}
