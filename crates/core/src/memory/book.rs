//! The book's two text forms: the table of contents the model reads at the
//! start of a session, and the Markdown the user edits in `$EDITOR`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use super::{Chapter, Fact};

/// Most pinned facts listed in full; the rest are reachable with a read.
const MAX_PINNED_FACTS: usize = 60;
/// Most chapters listed.
const MAX_CHAPTERS: usize = 60;
/// Most docs listed.
const MAX_DOCS: usize = 30;
/// Most headings listed per doc.
const MAX_HEADINGS: usize = 6;

/// A doc of the space, as the table of contents lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocEntry {
    /// `docs/…/x.doc.json`, what `canvas` reads.
    pub relative: String,
    pub title: String,
    /// Level 1–2 headings, in order.
    pub headings: Vec<String>,
}

/// The space's docs, newest first, with their top headings.
#[must_use]
pub fn docs(files_dir: &Path) -> Vec<DocEntry> {
    crate::canvas::library(files_dir)
        .into_iter()
        .filter(|entry| entry.relative.starts_with("docs/"))
        .take(MAX_DOCS)
        .map(|entry| {
            let headings = crate::doc::storage::load(files_dir, &entry.relative)
                .map(|doc| {
                    doc.blocks
                        .iter()
                        .filter_map(|block| match &block.kind {
                            crate::doc::Kind::Heading { level, text } if *level <= 2 => {
                                Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
                            }
                            _ => None,
                        })
                        .filter(|text| !text.is_empty())
                        .take(MAX_HEADINGS)
                        .collect()
                })
                .unwrap_or_default();
            DocEntry {
                relative: entry.relative,
                title: entry.title,
                headings,
            }
        })
        .collect()
}

/// The `## Memory` section body: pinned chapters in full, other chapters by
/// title, count and description, then the docs. Empty when there is nothing
/// to list.
#[must_use]
pub fn toc(chapters: &[Chapter], facts: &[Fact], docs: &[DocEntry]) -> String {
    if chapters.is_empty() && docs.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Saved across chats in this space. Pinned chapters are listed in full. Open any other \
         chapter with `memory` (action=read, chapter=…) and a doc with `canvas` (action=read, \
         path=…) before relying on it.\n",
    );
    let mut budget = MAX_PINNED_FACTS;
    for chapter in chapters.iter().filter(|c| c.pinned) {
        let _ = write!(out, "\n### {}\n", chapter.title);
        let in_chapter: Vec<&Fact> = facts
            .iter()
            .filter(|f| f.chapter_id == chapter.id)
            .collect();
        let take = in_chapter.len().min(budget);
        for fact in in_chapter.iter().take(take) {
            let _ = writeln!(out, "- [{}] {}", fact.short_id(), fact.text);
        }
        budget -= take;
        let hidden = in_chapter.len() - take;
        if hidden > 0 {
            let _ = writeln!(out, "- … {hidden} more: open the chapter");
        }
    }
    let others: Vec<&Chapter> = chapters.iter().filter(|c| !c.pinned).collect();
    if !others.is_empty() {
        out.push_str("\nChapters:\n");
        for chapter in others.iter().take(MAX_CHAPTERS) {
            let count = facts.iter().filter(|f| f.chapter_id == chapter.id).count();
            let _ = write!(out, "- {} ({count})", chapter.title);
            if !chapter.description.is_empty() {
                let _ = write!(out, " — {}", chapter.description);
            }
            out.push('\n');
        }
        if others.len() > MAX_CHAPTERS {
            let _ = writeln!(
                out,
                "- … {} more: memory action=read lists them all",
                others.len() - MAX_CHAPTERS
            );
        }
    }
    if !docs.is_empty() {
        out.push_str("\nDocs:\n");
        for doc in docs {
            let _ = write!(out, "- {} — {}", doc.relative, doc.title);
            if !doc.headings.is_empty() {
                let _ = write!(out, ": {}", doc.headings.join(" · "));
            }
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// One chapter with its facts, as `memory action=read` returns it.
#[must_use]
pub fn chapter_page(chapter: &Chapter, facts: &[Fact]) -> String {
    let mut out = format!("## {}", chapter.title);
    if chapter.pinned {
        out.push_str(" [pinned]");
    }
    out.push('\n');
    if !chapter.description.is_empty() {
        let _ = writeln!(out, "{}", chapter.description);
    }
    let in_chapter: Vec<&Fact> = facts
        .iter()
        .filter(|f| f.chapter_id == chapter.id)
        .collect();
    if in_chapter.is_empty() {
        out.push_str("(no facts)\n");
    }
    for fact in in_chapter {
        let _ = writeln!(out, "- [{}] {}", fact.short_id(), fact.text);
    }
    out.trim_end().to_string()
}

/// Every chapter title with its count and description, as `memory
/// action=read` without a chapter returns it.
#[must_use]
pub fn chapter_list(chapters: &[Chapter], facts: &[Fact]) -> String {
    if chapters.is_empty() {
        return "memory is empty — save a fact with action=add".to_string();
    }
    let mut out = String::new();
    for chapter in chapters {
        let count = facts.iter().filter(|f| f.chapter_id == chapter.id).count();
        let _ = write!(out, "- {} ({count})", chapter.title);
        if chapter.pinned {
            out.push_str(" [pinned]");
        }
        if !chapter.description.is_empty() {
            let _ = write!(out, " — {}", chapter.description);
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

const EDIT_HEADER: &str =
    "<!-- Memory for this space. One fact per \"- \" line under a \"## chapter\".
     \"> \" sets the chapter's description; \"[pinned]\" after a title lists the
     chapter in full in every chat. Delete a line to forget that fact, or a
     chapter heading to forget the whole chapter. -->";

/// The book as Markdown for `$EDITOR`.
#[must_use]
pub fn render_edit(chapters: &[Chapter], facts: &[Fact]) -> String {
    let mut out = format!("{EDIT_HEADER}\n");
    for chapter in chapters {
        let _ = write!(out, "\n## {}", chapter.title);
        if chapter.pinned {
            out.push_str(" [pinned]");
        }
        out.push('\n');
        if !chapter.description.is_empty() {
            let _ = writeln!(out, "> {}", chapter.description);
        }
        for fact in facts.iter().filter(|f| f.chapter_id == chapter.id) {
            let _ = writeln!(out, "- {}", fact.text);
        }
    }
    out
}

/// One chapter as the user left it in the editor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditedChapter {
    pub title: String,
    pub description: String,
    pub pinned: bool,
    pub facts: Vec<String>,
}

/// Facts written above the first chapter heading land here.
pub const UNFILED: &str = "General";

/// Parse the editor Markdown. Comments are skipped; any other non-empty line
/// is a fact.
#[must_use]
pub fn parse_edit(text: &str) -> Vec<EditedChapter> {
    let mut out: Vec<EditedChapter> = Vec::new();
    for line in strip_comments(text).lines() {
        let line = line.trim();
        if !line.is_empty() {
            push_line(&mut out, line);
        }
    }
    out
}

/// `text` without its `<!-- … -->` comments; an unclosed one runs to the end.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn push_line(out: &mut Vec<EditedChapter>, line: &str) {
    if let Some(title) = line.strip_prefix("## ") {
        let title = title.trim();
        let (title, pinned) = match title.strip_suffix("[pinned]") {
            Some(rest) => (rest.trim(), true),
            None => (title, false),
        };
        if !title.is_empty() {
            out.push(EditedChapter {
                title: title.to_string(),
                pinned,
                ..EditedChapter::default()
            });
        }
        return;
    }
    if out.is_empty() {
        out.push(EditedChapter {
            title: UNFILED.to_string(),
            ..EditedChapter::default()
        });
    }
    let chapter = out.last_mut().expect("a chapter exists");
    if let Some(description) = line.strip_prefix('>') {
        if !chapter.description.is_empty() {
            chapter.description.push(' ');
        }
        chapter.description.push_str(description.trim());
        return;
    }
    let fact = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line)
        .trim();
    if !fact.is_empty() && !line.starts_with('#') {
        chapter.facts.push(fact.to_string());
    }
}

/// What an editor save changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditSummary {
    pub added: usize,
    pub removed: usize,
    pub moved: usize,
    pub chapters_removed: usize,
}

impl std::fmt::Display for EditSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "memory saved · {} added · {} removed · {} moved",
            self.added, self.removed, self.moved
        )?;
        if self.chapters_removed > 0 {
            write!(f, " · {} chapters removed", self.chapters_removed)?;
        }
        Ok(())
    }
}

/// Apply an editor save as a diff: matching lines keep their fact, new lines
/// are added, a line moved under another heading moves its fact, and
/// anything no longer present is deleted.
pub fn apply_edit(conn: &Connection, space: &str, edited: &[EditedChapter]) -> Result<EditSummary> {
    let mut summary = EditSummary::default();
    let mut keep_facts: HashSet<String> = HashSet::new();
    let mut keep_chapters: HashSet<String> = HashSet::new();
    for edited_chapter in edited {
        let chapter = super::ensure_chapter(
            conn,
            space,
            &edited_chapter.title,
            None,
            Some(edited_chapter.pinned),
        )?;
        let chapter = if chapter.description == edited_chapter.description.trim() {
            chapter
        } else {
            super::update_chapter(
                conn,
                space,
                &chapter.title,
                None,
                Some(&edited_chapter.description),
                None,
            )?
        };
        keep_chapters.insert(chapter.id.clone());
        for text in &edited_chapter.facts {
            let text: String = text.chars().take(super::MAX_NOTE_CHARS).collect();
            let existing = super::facts(conn, space)?
                .into_iter()
                .find(|f| f.text.eq_ignore_ascii_case(text.trim()));
            match existing {
                Some(fact) if fact.chapter_id == chapter.id => {
                    keep_facts.insert(fact.id);
                }
                Some(fact) if !keep_facts.contains(&fact.id) => {
                    super::update_fact(conn, space, &fact.id, None, Some(&chapter.title))?;
                    keep_facts.insert(fact.id);
                    summary.moved += 1;
                }
                // The same text twice in the edit: keep one.
                Some(_) => {}
                None => {
                    let added = super::add_note(conn, space, &chapter.title, &text, None, None)?;
                    keep_facts.insert(added.fact.id);
                    summary.added += 1;
                }
            }
        }
    }
    for fact in super::facts(conn, space)? {
        if !keep_facts.contains(&fact.id) {
            super::delete_fact(conn, space, &fact.id)?;
            summary.removed += 1;
        }
    }
    for chapter in super::chapters(conn, space)? {
        if !keep_chapters.contains(&chapter.id) {
            super::delete_chapter(conn, &chapter.id)?;
            summary.chapters_removed += 1;
        }
    }
    Ok(summary)
}
