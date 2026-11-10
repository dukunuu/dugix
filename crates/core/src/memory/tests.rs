use std::collections::HashSet;

use super::book::{self, DocEntry};
use super::recall;
use super::*;

fn conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    conn
}

const SPACE: &str = "space-1";

#[test]
fn add_files_facts_under_chapters_and_skips_duplicates() {
    let conn = conn();
    let first = add(
        &conn,
        SPACE,
        "Tools",
        "uses Neovim",
        Some("editor setup"),
        None,
    )
    .unwrap();
    assert!(!first.duplicate);
    assert_eq!(first.chapter.description, "editor setup");
    assert_eq!(first.count, 1);
    let again = add(&conn, SPACE, "Tools", "Uses  neovim ", None, None).unwrap();
    assert!(again.duplicate);
    assert_eq!(again.fact.id, first.fact.id);
    // Chapter titles match without regard to case.
    let second = add(&conn, SPACE, "tools", "prefers Rust", None, None).unwrap();
    assert_eq!(second.chapter.id, first.chapter.id);
    assert_eq!(second.count, 2);
    assert_eq!(chapters(&conn, SPACE).unwrap().len(), 1);
    assert!(add(&conn, SPACE, "Tools", "   ", None, None).is_err());
    assert!(
        add(
            &conn,
            SPACE,
            "Tools",
            &"x".repeat(MAX_FACT_CHARS + 1),
            None,
            None
        )
        .is_err()
    );
}

#[test]
fn model_facts_keep_the_300_cap() {
    let conn = conn();
    assert!(add(&conn, SPACE, "Tools", &"x".repeat(301), None, None).is_err());
}

#[test]
fn short_ids_resolve_update_move_and_delete() {
    let conn = conn();
    let fact = add(&conn, SPACE, "Tools", "uses vim", None, None)
        .unwrap()
        .fact;
    let updated = update_fact(&conn, SPACE, fact.short_id(), Some("uses Neovim"), None).unwrap();
    assert_eq!(updated.text, "uses Neovim");
    let moved = update_fact(&conn, SPACE, &fact.id, None, Some("Editor")).unwrap();
    let editor = find_chapter(&conn, SPACE, "editor").unwrap().unwrap();
    assert_eq!(moved.chapter_id, editor.id);
    delete_fact(&conn, SPACE, fact.short_id()).unwrap();
    assert!(facts(&conn, SPACE).unwrap().is_empty());
    assert!(resolve_fact(&conn, SPACE, fact.short_id()).is_err());
    assert!(resolve_fact(&conn, SPACE, "ab").is_err());
}

#[test]
fn deletes_are_tombstones_that_bump_updated_at() {
    let conn = conn();
    let fact = add(&conn, SPACE, "Tools", "uses vim", None, None)
        .unwrap()
        .fact;
    delete_fact(&conn, SPACE, &fact.id).unwrap();
    let (deleted_at, updated_at): (Option<String>, String) = conn
        .query_row(
            "SELECT deleted_at, updated_at FROM memories WHERE id = ?1",
            [&fact.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(deleted_at.as_deref(), Some(updated_at.as_str()));
    assert!(updated_at > fact.updated_at);
}

#[test]
fn toc_lists_pinned_in_full_and_others_by_title() {
    let conn = conn();
    add(&conn, SPACE, ABOUT_YOU, "uses Neovim", None, Some(true)).unwrap();
    add(
        &conn,
        SPACE,
        "nexus",
        "deploys from master",
        Some("release plan"),
        None,
    )
    .unwrap();
    add(&conn, SPACE, "nexus", "tags before publishing", None, None).unwrap();
    let docs = vec![DocEntry {
        relative: "docs/eu.doc.json".into(),
        title: "EU AI Act".into(),
        headings: vec!["Summary".into(), "Sources".into()],
    }];
    let toc = book::toc(
        &chapters(&conn, SPACE).unwrap(),
        &facts(&conn, SPACE).unwrap(),
        &docs,
    );
    assert!(toc.contains("### About you\n- ["));
    assert!(toc.contains("] uses Neovim"));
    assert!(toc.contains("- nexus (2) — release plan"));
    assert!(!toc.contains("deploys from master"));
    assert!(toc.contains("- docs/eu.doc.json — EU AI Act: Summary · Sources"));
    assert!(book::toc(&[], &[], &[]).is_empty());
}

#[test]
fn editor_round_trip_applies_a_diff() {
    let conn = conn();
    add(&conn, SPACE, ABOUT_YOU, "uses Neovim", None, Some(true)).unwrap();
    add(&conn, SPACE, "nexus", "deploys from dev", None, None).unwrap();
    let kept = add(&conn, SPACE, "nexus", "tags before publishing", None, None)
        .unwrap()
        .fact;
    let rendered = book::render_edit(
        &chapters(&conn, SPACE).unwrap(),
        &facts(&conn, SPACE).unwrap(),
    );
    assert!(rendered.contains("## About you [pinned]\n- uses Neovim"));
    // Unchanged text keeps its fact.
    let parsed = book::parse_edit(&rendered);
    assert_eq!(
        book::apply_edit(&conn, SPACE, &parsed).unwrap(),
        book::EditSummary::default()
    );

    let edited = rendered
        .replace("- deploys from dev\n", "- deploys from master\n")
        .replace("- uses Neovim\n", "")
        .replace("- tags before publishing\n", "")
        + "\n## Rust\n> conventions\n- uses thiserror\n- tags before publishing\n";
    let summary = book::apply_edit(&conn, SPACE, &book::parse_edit(&edited)).unwrap();
    assert_eq!(summary.added, 2); // "deploys from master", "uses thiserror"
    assert_eq!(summary.removed, 2); // "deploys from dev", "uses Neovim"
    assert_eq!(summary.moved, 1); // "tags before publishing" -> Rust
    let rust = find_chapter(&conn, SPACE, "Rust").unwrap().unwrap();
    assert_eq!(rust.description, "conventions");
    let moved = resolve_fact(&conn, SPACE, &kept.id).unwrap();
    assert_eq!(moved.chapter_id, rust.id);

    // Dropping a chapter heading forgets the chapter.
    let summary = book::apply_edit(
        &conn,
        SPACE,
        &book::parse_edit("## Rust\n- uses thiserror\n- tags before publishing\n"),
    )
    .unwrap();
    assert_eq!(summary.chapters_removed, 2);
    assert_eq!(chapters(&conn, SPACE).unwrap().len(), 1);
}

#[test]
fn editor_keeps_long_facts_whole() {
    let conn = conn();
    let text = "x".repeat(1_000);
    let fact = add_note(&conn, SPACE, ABOUT_YOU, &text, None, Some(true))
        .unwrap()
        .fact;
    let rendered = book::render_edit(
        &chapters(&conn, SPACE).unwrap(),
        &facts(&conn, SPACE).unwrap(),
    );
    let summary = book::apply_edit(&conn, SPACE, &book::parse_edit(&rendered)).unwrap();
    assert_eq!(summary.added, 0);
    assert_eq!(summary.removed, 0);
    let saved = facts(&conn, SPACE).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, fact.id);
    assert_eq!(saved[0].text, text);
}

#[test]
fn parse_edit_skips_comments_and_files_loose_lines() {
    let parsed = book::parse_edit(
        "<!-- header\n spanning lines -->\nloose fact\n## Tools [pinned]\n> one\n> two\n* starred\n",
    );
    assert_eq!(parsed[0].title, book::UNFILED);
    assert_eq!(parsed[0].facts, ["loose fact"]);
    assert_eq!(parsed[1].title, "Tools");
    assert!(parsed[1].pinned);
    assert_eq!(parsed[1].description, "one two");
    assert_eq!(parsed[1].facts, ["starred"]);
}

#[test]
fn legacy_memory_file_is_imported_once() {
    let conn = conn();
    let dir = std::env::temp_dir().join(format!("nexus-memory-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("memory.md");
    std::fs::write(
        &path,
        "<!-- memory -->\n1. prefers Rust\n2. uses Neovim\n\n",
    )
    .unwrap();
    assert_eq!(import_legacy(&conn, SPACE, &path).unwrap(), 2);
    assert!(!path.exists());
    assert!(dir.join("memory.md.imported").exists());
    assert_eq!(import_legacy(&conn, SPACE, &path).unwrap(), 0);
    let about = find_chapter(&conn, SPACE, ABOUT_YOU).unwrap().unwrap();
    assert!(about.pinned);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_import_keeps_long_lines_whole() {
    let conn = conn();
    let dir = std::env::temp_dir().join(format!("nexus-memory-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("memory.md");
    let text = "x".repeat(1_000);
    std::fs::write(&path, format!("1. {text}\n")).unwrap();
    assert_eq!(import_legacy(&conn, SPACE, &path).unwrap(), 1);
    let saved = facts(&conn, SPACE).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].text, text);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hints_name_matching_chapters_and_docs_once() {
    let conn = conn();
    add(&conn, SPACE, ABOUT_YOU, "release manager", None, Some(true)).unwrap();
    add(
        &conn,
        SPACE,
        "Release process",
        "tag before publishing to crates.io",
        None,
        None,
    )
    .unwrap();
    add(&conn, SPACE, "Cooking", "likes ramen", None, None).unwrap();
    let docs = vec![DocEntry {
        relative: "docs/release.doc.json".into(),
        title: "Release checklist".into(),
        headings: vec![],
    }];
    let chapters = chapters(&conn, SPACE).unwrap();
    let facts = facts(&conn, SPACE).unwrap();
    let hint = recall::hint(
        "how should I release the new crate version?",
        &chapters,
        &facts,
        &docs,
        &HashSet::new(),
    )
    .unwrap();
    let keys: Vec<&str> = hint.targets.iter().map(|t| t.key.as_str()).collect();
    assert_eq!(
        keys,
        ["chapter:release process", "doc:docs/release.doc.json"]
    );
    assert!(
        hint.text
            .contains("memory action=read chapter=\"Release process\"")
    );
    assert_eq!(recall::Recall::from_row(&hint.to_row()), Some(hint.clone()));

    let already: HashSet<String> = keys.iter().map(ToString::to_string).collect();
    assert!(
        recall::hint(
            "how should I release the new crate version?",
            &chapters,
            &facts,
            &docs,
            &already
        )
        .is_none()
    );
    assert!(recall::hint("release?", &chapters, &facts, &docs, &HashSet::new()).is_none());
    assert!(
        recall::hint(
            "/memory tidy release process",
            &chapters,
            &facts,
            &docs,
            &HashSet::new()
        )
        .is_none()
    );
}

#[test]
fn deleting_a_space_tombstones_its_memory() {
    let conn = conn();
    add(&conn, SPACE, "Tools", "uses vim", None, None).unwrap();
    add(&conn, "other", "Tools", "uses emacs", None, None).unwrap();
    delete_space(&conn, SPACE).unwrap();
    assert!(chapters(&conn, SPACE).unwrap().is_empty());
    assert_eq!(facts(&conn, "other").unwrap().len(), 1);
}

#[test]
fn chapters_and_facts_sync_with_their_deletes() {
    use crate::db::Db;
    use crate::space::Space;
    use crate::sync::{TABLES, apply_changeset, build_changeset};

    for table in ["memory_chapters", "memories"] {
        assert!(TABLES.iter().any(|spec| spec.name == table), "{table}");
    }
    let root = std::env::temp_dir().join(format!("nexus-memory-sync-{}", Uuid::new_v4()));
    let space = Space { root: root.clone() };
    let (a, b) = (Db::open_in_memory().unwrap(), Db::open_in_memory().unwrap());
    let kept = add(a.conn(), SPACE, "Tools", "uses vim", Some("editor"), None)
        .unwrap()
        .fact;
    let gone = add(a.conn(), SPACE, "Tools", "uses a mac", None, None)
        .unwrap()
        .fact;
    apply_changeset(&b, &space, &build_changeset(&a, None, "a").unwrap(), None).unwrap();
    assert_eq!(facts(b.conn(), SPACE).unwrap().len(), 2);
    assert_eq!(chapters(b.conn(), SPACE).unwrap()[0].description, "editor");

    // A delete is a newer row, so it travels like any edit.
    delete_fact(a.conn(), SPACE, &gone.id).unwrap();
    update_fact(a.conn(), SPACE, &kept.id, Some("uses Neovim"), None).unwrap();
    apply_changeset(&b, &space, &build_changeset(&a, None, "a").unwrap(), None).unwrap();
    let texts: Vec<String> = facts(b.conn(), SPACE)
        .unwrap()
        .into_iter()
        .map(|f| f.text)
        .collect();
    assert_eq!(texts, ["uses Neovim"]);
    let _ = std::fs::remove_dir_all(root);
}
