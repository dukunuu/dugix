//! The `memory` tool: read, add, update and delete facts in the space's memory
//! book (`crate::memory`).

use anyhow::{Result, bail};
use serde_json::Value;

use crate::memory::{self, book};

/// Where memory calls read and write: the durable db and the active space.
#[derive(Clone)]
pub struct MemoryCtx {
    pub db_path: std::path::PathBuf,
    pub space_id: String,
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
            let chapters = memory::chapters(&conn, space)?;
            let facts = memory::facts(&conn, space)?;
            let offset = value["offset"]
                .as_u64()
                .map_or(1, |n| usize::try_from(n).unwrap_or(usize::MAX));
            match text("chapter") {
                None => book::chapter_list(&chapters, &facts, offset),
                Some(title) => match memory::find_chapter(&conn, space, title)? {
                    Some(chapter) => book::chapter_page(&chapter, &facts, offset),
                    None => format!(
                        "no chapter titled {title:?}; chapters:\n{}",
                        book::chapter_list(&chapters, &facts, 1)
                    ),
                },
            }
        }
        "memory_add" => {
            let (Some(chapter), Some(fact)) = (text("chapter"), text("text")) else {
                bail!("add needs chapter and text");
            };
            let added = memory::add(&conn, space, chapter, fact, text("description"), pinned)?;
            if added.duplicate {
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
            }
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
        other => bail!("unknown memory action {other}"),
    })
}
