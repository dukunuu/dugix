//! The `memory` tool: read, add, update and delete facts in the space's memory
//! book (`crate::memory`), and — when the user asks — share chapters between
//! spaces or merge same-titled chapters into one.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Result, bail};
use rusqlite::Connection;
use serde_json::Value;

use crate::memory::{self, Chapter, Share, book};

/// Where memory calls read and write: the durable db and the active space.
#[derive(Clone)]
pub struct MemoryCtx {
    pub db_path: std::path::PathBuf,
    pub space_id: String,
    /// More than one space exists, so sharing between spaces means something;
    /// only then are the sharing actions offered (they cost prompt tokens).
    pub sharing: bool,
}

/// Run one memory action; errors come back as text for the model.
pub(super) fn run(ctx: &MemoryCtx, internal: &str, args: &str) -> String {
    call(ctx, internal, args).unwrap_or_else(|error| format!("memory: {error}"))
}

fn call(ctx: &MemoryCtx, internal: &str, args: &str) -> Result<String> {
    let value: Value = serde_json::from_str(args).unwrap_or_default();
    let text = |key: &str| value[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    let pinned = value["pinned"].as_bool();
    let conn = crate::db::open_conn(&ctx.db_path)?;
    let space = ctx.space_id.as_str();
    Ok(match internal {
        "memory_read" => {
            let chapters = memory::visible_chapters(&conn, space)?;
            let facts = memory::visible_facts(&conn, space)?;
            let labels = memory::share_labels(&conn, space, &chapters)?;
            let offset = value["offset"]
                .as_u64()
                .map_or(1, |n| usize::try_from(n).unwrap_or(usize::MAX));
            match text("chapter") {
                None => book::chapter_list_with_labels(&chapters, &facts, offset, &labels),
                Some(title) => match memory::find_visible_chapter(&conn, space, title)? {
                    Some(chapter) => {
                        book::chapter_page_with_labels(&chapter, &facts, offset, &labels)
                    }
                    None => format!(
                        "no chapter titled {title:?}; chapters:\n{}",
                        book::chapter_list_with_labels(&chapters, &facts, 1, &labels)
                    ),
                },
            }
        }
        "memory_add" => {
            let (Some(chapter), Some(fact)) = (text("chapter"), text("text")) else {
                bail!("add needs chapter and text");
            };
            let added = memory::add(&conn, space, chapter, fact, text("description"), pinned)?;
            let mut result = if added.duplicate {
                format!("already saved as [{}]", added.fact.short_id())
            } else {
                let mut out = format!(
                    "saved [{}] in \"{}\" ({} facts)",
                    added.fact.short_id(),
                    added.chapter.title,
                    added.count
                );
                if added.count > memory::TIDY_HINT_FACTS {
                    out.push_str(
                        " — the chapter is large: merge duplicates with update/delete when convenient",
                    );
                }
                out
            };
            if added.chapter.space_id != space
                && (text("description").is_some() || pinned.is_some())
            {
                result.push_str(
                    " (description and pin of a shared chapter change only in its home space)",
                );
            }
            result
        }
        "memory_update" => {
            let Some(id) = text("id") else {
                bail!("update needs id");
            };
            let fact = memory::update_fact(&conn, space, id, text("text"), text("chapter"))?;
            format!("updated [{}]: {}", fact.short_id(), fact.text)
        }
        "memory_delete" => {
            let Some(id) = text("id") else {
                bail!("delete needs id");
            };
            let fact = memory::delete_fact(&conn, space, id)?;
            format!("forgot [{}]: {}", fact.short_id(), fact.text)
        }
        "memory_spaces" => spaces_overview(&conn, space)?,
        "memory_share" => share(
            &conn,
            space,
            text("chapter"),
            text("space"),
            &value["spaces"],
        )?,
        "memory_merge" => merge(&conn, space, text("chapter"), text("space"))?,
        other => bail!("unknown memory action {other}"),
    })
}

/// Set which spaces see a chapter: the exact set the model names, which it reads
/// from `spaces` first. `home` names the chapter's space when it is not this one.
fn share(
    conn: &Connection,
    space: &str,
    title: Option<&str>,
    home: Option<&str>,
    list: &Value,
) -> Result<String> {
    let Some(title) = title else {
        bail!("share needs chapter");
    };
    let names = string_list(list);
    let spaces = Spaces::load(conn)?;
    let home = home.map_or_else(|| Ok(space.to_string()), |name| spaces.id(name))?;
    let chapter = home_chapter(conn, &spaces, &home, title)?;
    let is = |word: &str| names.iter().any(|n| n.eq_ignore_ascii_case(word));
    let share = if is("all") {
        Share::All
    } else if is("none") || names.is_empty() {
        Share::Private
    } else {
        let ids = names
            .iter()
            .map(|n| spaces.id(n))
            .collect::<Result<Vec<_>>>()?;
        memory::widen(&Share::Private, &Share::Spaces(ids), &home)
    };
    let chapter = memory::share_chapter(conn, &chapter.id, share)?;
    Ok(sharing_sentence(&chapter, &spaces))
}

/// Fold every same-titled chapter into the one in `home` (this space unless
/// named), creating it there when only other spaces have it.
fn merge(
    conn: &Connection,
    space: &str,
    title: Option<&str>,
    home: Option<&str>,
) -> Result<String> {
    let Some(title) = title else {
        bail!("merge needs chapter");
    };
    let spaces = Spaces::load(conn)?;
    let home = home.map_or_else(|| Ok(space.to_string()), |name| spaces.id(name))?;
    let into = if let Some(chapter) = memory::find_chapter(conn, &home, title)? {
        chapter
    } else {
        let elsewhere = chapters_titled(conn, title)?;
        let Some(first) = elsewhere.first() else {
            bail!("no chapter titled {title:?} in any space — `spaces` lists every chapter");
        };
        memory::ensure_chapter(conn, &home, &first.title, None, None)?
    };
    let merged = memory::merge_same_titled(conn, &into)?;
    if merged.from.is_empty() {
        return Ok(format!(
            "nothing to merge: no other space has a chapter titled {:?}",
            merged.chapter.title
        ));
    }
    let from: Vec<&str> = merged.from.iter().map(|id| spaces.name(id)).collect();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    Ok(format!(
        "merged {:?} from {} into {}: moved {} fact{}, dropped {} duplicate{}. {}",
        merged.chapter.title,
        from.join(", "),
        spaces.name(&merged.chapter.space_id),
        merged.moved,
        plural(merged.moved),
        merged.duplicates,
        plural(merged.duplicates),
        sharing_sentence(&merged.chapter, &spaces)
    ))
}

/// Every space by id and name; names resolve ignoring case, and a wrong one
/// lists the right ones (the model's stand-in for autocomplete).
struct Spaces(BTreeMap<String, String>);

impl Spaces {
    fn load(conn: &Connection) -> Result<Self> {
        let mut stmt = conn.prepare("SELECT id, name FROM spaces")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(Self(rows.collect::<rusqlite::Result<_>>()?))
    }

    fn id(&self, name: &str) -> Result<String> {
        let name = name.trim();
        self.0
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|(id, _)| id.clone())
            .ok_or_else(|| anyhow::anyhow!("no space named {name:?}; spaces: {}", self.names()))
    }

    fn name<'a>(&'a self, id: &'a str) -> &'a str {
        self.0.get(id).map_or(id, String::as_str)
    }

    fn names(&self) -> String {
        let mut names: Vec<&str> = self.0.values().map(String::as_str).collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.join(", ")
    }
}

fn string_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        Value::String(one) if !one.trim().is_empty() => vec![one.trim().to_string()],
        _ => Vec::new(),
    }
}

fn chapters_titled(conn: &Connection, title: &str) -> Result<Vec<Chapter>> {
    let mut out = Vec::new();
    for space in Spaces::load(conn)?.0.keys() {
        if let Some(chapter) = memory::find_chapter(conn, space, title)? {
            out.push(chapter);
        }
    }
    Ok(out)
}

/// The chapter whose sharing changes: it must live in `home`. A chapter shared
/// into `home` from elsewhere names its own home so the next call can use it.
fn home_chapter(conn: &Connection, spaces: &Spaces, home: &str, title: &str) -> Result<Chapter> {
    if let Some(chapter) = memory::find_chapter(conn, home, title)? {
        return Ok(chapter);
    }
    let elsewhere = chapters_titled(conn, title)?;
    if elsewhere.is_empty() {
        bail!(
            "no chapter titled {title:?} in {} — `spaces` lists every chapter",
            spaces.name(home)
        );
    }
    let homes: Vec<&str> = elsewhere.iter().map(|c| spaces.name(&c.space_id)).collect();
    bail!(
        "{title:?} is not a chapter of {}; it lives in {} — pass space to name its home",
        spaces.name(home),
        homes.join(", ")
    )
}

/// "\"About you\" (home: default) is now shared with all spaces" — who can see it now.
fn sharing_sentence(chapter: &Chapter, spaces: &Spaces) -> String {
    let home = spaces.name(&chapter.space_id);
    let who = match &chapter.shared_with {
        Share::Private => {
            return format!(
                "{:?} (home: {home}) is now private to {home}",
                chapter.title
            );
        }
        Share::All => "all spaces, including new ones".to_string(),
        Share::Spaces(ids) => {
            let mut names: Vec<&str> = ids.iter().map(|id| spaces.name(id)).collect();
            names.sort_by_key(|n| n.to_lowercase());
            names.join(", ")
        }
    };
    format!(
        "{:?} (home: {home}) is now shared with {who}",
        chapter.title
    )
}

/// Every space with its own chapters, fact counts and sharing, and the titles
/// several spaces repeat — the candidates for sharing one chapter instead.
fn spaces_overview(conn: &Connection, current: &str) -> Result<String> {
    let spaces = Spaces::load(conn)?;
    let mut order: Vec<(&String, &String)> = spaces.0.iter().collect();
    order.sort_by_key(|(id, name)| (*id != current, name.to_lowercase()));
    let mut out = String::new();
    // Keyed by lowercase title; the first spelling seen is the one shown.
    let mut titles: BTreeMap<String, (String, Vec<&str>)> = BTreeMap::new();
    for (id, name) in order {
        let chapters = memory::chapters(conn, id)?;
        let labels = memory::share_labels(conn, id, &chapters)?;
        let here = if id == current { " (this space)" } else { "" };
        let _ = writeln!(out, "{name}{here}");
        if chapters.is_empty() {
            out.push_str("  (no chapters)\n");
        }
        for chapter in &chapters {
            let count = memory::chapter_count(conn, &chapter.id)?;
            let pin = if chapter.pinned { ", pinned" } else { "" };
            let label = labels.get(&chapter.id).map_or("", String::as_str);
            let _ = writeln!(out, "  - {} ({count} facts{pin}){label}", chapter.title);
            titles
                .entry(chapter.title.to_lowercase())
                .or_insert_with(|| (chapter.title.clone(), Vec::new()))
                .1
                .push(name.as_str());
        }
    }
    let repeated: Vec<String> = titles
        .into_values()
        .filter(|(_, names)| names.len() > 1)
        .map(|(title, names)| format!("  - {title:?} in {}", names.join(", ")))
        .collect();
    if !repeated.is_empty() {
        out.push_str(
            "\nSame title in several spaces (merge folds them into one shared chapter):\n",
        );
        out.push_str(&repeated.join("\n"));
        out.push('\n');
    }
    Ok(out.trim_end().to_string())
}
