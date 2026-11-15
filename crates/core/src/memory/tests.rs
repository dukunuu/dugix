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
    let parsed = book::parse_edit(&rendered);
    assert_eq!(
        book::apply_edit(&conn, SPACE, &parsed, book::opened_at(&rendered)).unwrap(),
        book::EditSummary::default()
    );

    let edited = rendered
        .replace("- deploys from dev\n", "- deploys from master\n")
        .replace("- uses Neovim\n", "")
        .replace("- tags before publishing\n", "")
        + "\n## Rust\n> conventions\n- uses thiserror\n- tags before publishing\n";
    let summary = book::apply_edit(
        &conn,
        SPACE,
        &book::parse_edit(&edited),
        book::opened_at(&edited),
    )
    .unwrap();
    assert_eq!(summary.added, 2); // "deploys from master", "uses thiserror"
    assert_eq!(summary.removed, 2); // "deploys from dev", "uses Neovim"
    assert_eq!(summary.moved, 1); // "tags before publishing" -> Rust
    let rust = find_chapter(&conn, SPACE, "Rust").unwrap().unwrap();
    assert_eq!(rust.description, "conventions");
    let moved = resolve_fact(&conn, SPACE, &kept.id).unwrap();
    assert_eq!(moved.chapter_id, rust.id);

    let summary = book::apply_edit(
        &conn,
        SPACE,
        &book::parse_edit("## Rust\n- uses thiserror\n- tags before publishing\n"),
        None,
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
    let summary = book::apply_edit(
        &conn,
        SPACE,
        &book::parse_edit(&rendered),
        book::opened_at(&rendered),
    )
    .unwrap();
    assert_eq!(summary.added, 0);
    assert_eq!(summary.removed, 0);
    let saved = facts(&conn, SPACE).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, fact.id);
    assert_eq!(saved[0].text, text);
}

#[test]
fn editor_save_keeps_facts_added_while_it_was_open() {
    let conn = conn();
    add(&conn, SPACE, ABOUT_YOU, "uses Neovim", None, Some(true)).unwrap();
    let rendered = book::render_edit(
        &chapters(&conn, SPACE).unwrap(),
        &facts(&conn, SPACE).unwrap(),
    );
    let late = add(&conn, SPACE, "nexus", "deploys from master", None, None)
        .unwrap()
        .fact;
    let summary = book::apply_edit(
        &conn,
        SPACE,
        &book::parse_edit(&rendered),
        book::opened_at(&rendered),
    )
    .unwrap();
    assert_eq!(summary.removed, 0);
    assert!(resolve_fact(&conn, SPACE, &late.id).is_ok());
}

#[test]
fn editor_save_still_deletes_a_line_removed_from_the_text() {
    let conn = conn();
    let gone = add(&conn, SPACE, "nexus", "deploys from dev", None, None)
        .unwrap()
        .fact;
    let rendered = book::render_edit(
        &chapters(&conn, SPACE).unwrap(),
        &facts(&conn, SPACE).unwrap(),
    );
    let edited = rendered.replace("- deploys from dev\n", "");
    let summary = book::apply_edit(
        &conn,
        SPACE,
        &book::parse_edit(&edited),
        book::opened_at(&edited),
    )
    .unwrap();
    assert_eq!(summary.removed, 1);
    assert!(resolve_fact(&conn, SPACE, &gone.id).is_err());
}

#[test]
fn editor_text_without_an_opened_line_still_deletes() {
    let conn = conn();
    let gone = add(&conn, SPACE, "nexus", "deploys from dev", None, None)
        .unwrap()
        .fact;
    let text = "## nexus\n";
    let summary =
        book::apply_edit(&conn, SPACE, &book::parse_edit(text), book::opened_at(text)).unwrap();
    assert_eq!(summary.removed, 1);
    assert!(resolve_fact(&conn, SPACE, &gone.id).is_err());
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

#[test]
fn reading_a_large_chapter_pages_by_offset() {
    let conn = conn();
    for n in 1..=40 {
        let fact = format!("fact {n:02} {}", "x".repeat(280));
        add(&conn, SPACE, "Notes", &fact, None, None).unwrap();
    }
    let chapter = find_chapter(&conn, SPACE, "Notes").unwrap().unwrap();
    let all = facts(&conn, SPACE).unwrap();

    let first = book::chapter_page(&chapter, &all, 1);
    assert!(first.chars().count() <= 6_200, "{}", first.len());
    assert!(first.contains("fact 01 "));
    let (_, tail) = first.rsplit_once("continue with offset=").expect(&first);
    let next: usize = tail.trim().parse().unwrap();
    assert!(next > 1 && next <= 40, "{next}");
    assert!(
        first.contains(&format!("facts 1–{} of 40", next - 1)),
        "{first}"
    );

    let mut seen = next - 1;
    let mut offset = next;
    while seen < 40 {
        let page = book::chapter_page(&chapter, &all, offset);
        assert!(page.contains(&format!("fact {offset:02} ")), "{page}");
        let shown = page.lines().filter(|l| l.starts_with("- [")).count();
        seen += shown;
        offset += shown;
        assert_eq!(page.contains("continue with offset="), seen < 40, "{page}");
    }
    assert_eq!(seen, 40);

    let past = book::chapter_page(&chapter, &all, 41);
    assert!(
        past.contains("offset 41 is past the last of 40 facts"),
        "{past}"
    );
    let list = book::chapter_list(&chapters(&conn, SPACE).unwrap(), &all, 1);
    assert_eq!(list, "- Notes (40)");
}

fn sharing_spaces(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO spaces (id, name, created_at) VALUES
         ('home', 'learning', 'now'), ('guest', 'job-search', 'now'), ('third', 'third', 'now')",
    )
    .unwrap();
}

#[test]
fn shared_memory_visibility_labels_and_private_toc_compatibility() {
    let conn = conn();
    sharing_spaces(&conn);
    let added = add(
        &conn,
        "home",
        "Career profile",
        "Rust engineer",
        None,
        Some(true),
    )
    .unwrap();
    assert_eq!(
        visible_chapters(&conn, "guest").unwrap(),
        Vec::<Chapter>::new()
    );
    assert_eq!(visible_facts(&conn, "guest").unwrap(), Vec::<Fact>::new());
    let own = chapters(&conn, "home").unwrap();
    let own_facts = facts(&conn, "home").unwrap();
    let labels = share_labels(&conn, "home", &own).unwrap();
    let expected = format!(
        "Saved across chats in this space. Pinned chapters are listed in full. Open any other \
         chapter with `memory` (action=read, chapter=…) and a doc with `canvas` (action=read, \
         path=…) before relying on it.\n\n### Career profile\n- [{}] Rust engineer",
        added.fact.short_id()
    );
    assert_eq!(book::toc(&own, &own_facts, &[]), expected);
    assert_eq!(
        book::toc_with_labels(&own, &own_facts, &[], &labels),
        expected
    );
    let chapter = share_chapter(&conn, &added.chapter.id, Share::All).unwrap();
    assert!(chapter.updated_at > added.chapter.updated_at);
    for space in ["guest", "third"] {
        assert_eq!(
            visible_chapters(&conn, space).unwrap(),
            std::slice::from_ref(&chapter)
        );
        assert_eq!(
            visible_facts(&conn, space).unwrap(),
            std::slice::from_ref(&added.fact)
        );
    }
    let home_labels = share_labels(&conn, "home", std::slice::from_ref(&chapter)).unwrap();
    assert_eq!(home_labels[&chapter.id], " (shared with all spaces)");
    share_chapter(&conn, &chapter.id, Share::Spaces(vec!["guest".into()])).unwrap();
    assert_eq!(
        visible_chapters(&conn, "third").unwrap(),
        Vec::<Chapter>::new()
    );
    assert_eq!(visible_facts(&conn, "third").unwrap(), Vec::<Fact>::new());
    for (space, suffix) in [
        ("guest", "shared from learning"),
        ("home", "shared with job-search"),
    ] {
        let chapters = visible_chapters(&conn, space).unwrap();
        let facts = visible_facts(&conn, space).unwrap();
        let labels = share_labels(&conn, space, &chapters).unwrap();
        let toc = book::toc_with_labels(&chapters, &facts, &[], &labels);
        assert!(
            toc.contains(&format!("### Career profile ({suffix})")),
            "{toc}"
        );
        assert!(toc.contains("Rust engineer"));
        assert!(toc.contains("Chapters marked shared are seen in other spaces too: keep them to facts true in all of them."));
        assert!(book::chapter_page_with_labels(&chapters[0], &facts, 1, &labels).contains(suffix));
        assert!(book::chapter_list_with_labels(&chapters, &facts, 1, &labels).contains(suffix));
    }
    conn.execute("UPDATE spaces SET name = 'renamed' WHERE id = 'home'", [])
        .unwrap();
    assert!(
        sharing_list(&conn, "guest")
            .unwrap()
            .contains("shared from renamed")
    );
    share_chapter(&conn, &chapter.id, Share::Private).unwrap();
    assert_eq!(
        visible_chapters(&conn, "guest").unwrap(),
        Vec::<Chapter>::new()
    );
}

#[test]
fn shared_memory_validates_canonicalizes_snapshots_and_cleans_deleted_targets() {
    let conn = conn();
    sharing_spaces(&conn);
    let chapter = ensure_chapter(&conn, "home", "Career", None, None).unwrap();
    for id in ["missing", "home"] {
        assert!(share_chapter(&conn, &chapter.id, Share::Spaces(vec![id.into()])).is_err());
    }
    let chapter = share_chapter(
        &conn,
        &chapter.id,
        Share::Spaces(vec!["third".into(), "guest".into(), "third".into()]),
    )
    .unwrap();
    assert_eq!(
        chapter.shared_with,
        Share::Spaces(vec!["guest".into(), "third".into()])
    );
    let stored: String = conn
        .query_row(
            "SELECT shared_with FROM memory_chapters WHERE id = ?1",
            [&chapter.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, r#"["guest","third"]"#);
    let saved = snapshot(&conn, "home").unwrap();
    share_chapter(&conn, &chapter.id, Share::Private).unwrap();
    restore(&conn, &saved).unwrap();
    assert_eq!(
        chapters(&conn, "home").unwrap()[0].shared_with,
        chapter.shared_with
    );
    delete_space(&conn, "guest").unwrap();
    let changed = &chapters(&conn, "home").unwrap()[0];
    assert_eq!(changed.shared_with, Share::Spaces(vec!["third".into()]));
    assert!(changed.updated_at > chapter.updated_at);
    delete_space(&conn, "third").unwrap();
    assert_eq!(
        chapters(&conn, "home").unwrap()[0].shared_with,
        Share::Private
    );
}

#[test]
fn guest_editor_and_chapter_metadata_stay_private_but_recall_sees_shared_facts() {
    let conn = conn();
    sharing_spaces(&conn);
    let added = add(
        &conn,
        "home",
        "Career",
        "Rust engineer applying for compiler positions",
        None,
        None,
    )
    .unwrap();
    share_chapter(&conn, &added.chapter.id, Share::All).unwrap();
    add(&conn, "guest", "Private", "only in job-search", None, None).unwrap();
    let rendered = book::render_edit(
        &chapters(&conn, "guest").unwrap(),
        &facts(&conn, "guest").unwrap(),
    );
    assert!(!rendered.contains("Career"));
    assert!(!rendered.contains("compiler"));
    book::apply_edit(
        &conn,
        "guest",
        &book::parse_edit(&rendered),
        book::opened_at(&rendered),
    )
    .unwrap();
    assert_eq!(facts(&conn, "home").unwrap(), [added.fact]);
    assert!(update_chapter(&conn, "guest", "Career", None, None, Some(true)).is_err());
    let hint = recall::hint(
        "Find Rust compiler positions for me",
        &visible_chapters(&conn, "guest").unwrap(),
        &visible_facts(&conn, "guest").unwrap(),
        &[],
        &HashSet::new(),
    )
    .unwrap();
    assert_eq!(hint.targets[0].label, "Career");
}

#[tokio::test]
async fn memory_tool_guest_reads_writes_and_prefers_own_titles() {
    use crate::db::Db;
    use crate::tools::{MemoryCtx, ToolBox};
    let root = std::env::temp_dir().join(format!("nexus-memory-shared-tool-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("nexus.db");
    let db = Db::open(&path).unwrap();
    sharing_spaces(db.conn());
    let added = add(
        db.conn(),
        "home",
        "Career",
        "Rust engineer",
        Some("profile"),
        Some(true),
    )
    .unwrap();
    share_chapter(db.conn(), &added.chapter.id, Share::All).unwrap();
    let mut tools = ToolBox::new(
        root.join("skills"),
        None,
        None,
        "auto".into(),
        vec![],
        None,
        None,
        None,
    );
    tools.memory = Some(MemoryCtx {
        db_path: path,
        space_id: "guest".into(),
        sharing: true,
    });
    let (read, _) = tools
        .run("memory", r#"{"action":"read","chapter":"Career"}"#)
        .await;
    assert!(read.contains("Career (shared from learning)"), "{read}");
    assert!(read.contains("Rust engineer"));
    let (saved, _) = tools.run("memory", r#"{"action":"add","chapter":"Career","text":"likes compilers","description":"changed","pinned":false}"#).await;
    assert!(saved.contains("saved ["), "{saved}");
    assert!(
        saved.contains("description and pin of a shared chapter change only in its home space")
    );
    let chapter = find_chapter(db.conn(), "home", "Career").unwrap().unwrap();
    assert_eq!(chapter.description, "profile");
    assert!(chapter.pinned);
    assert_eq!(chapters(db.conn(), "guest").unwrap(), Vec::<Chapter>::new());
    let fact = facts(db.conn(), "home")
        .unwrap()
        .into_iter()
        .find(|f| f.text == "likes compilers")
        .unwrap();
    assert_eq!(fact.space_id, "home");
    let args = serde_json::json!({"action":"update", "id":fact.id, "text":"loves compilers"});
    let (updated, _) = tools.run("memory", &args.to_string()).await;
    assert!(updated.contains("loves compilers"), "{updated}");
    assert_eq!(
        resolve_fact(db.conn(), "guest", &fact.id).unwrap().text,
        "loves compilers"
    );
    let args = serde_json::json!({"action":"delete", "id":fact.id});
    let (deleted, _) = tools.run("memory", &args.to_string()).await;
    assert!(deleted.contains("forgot ["), "{deleted}");
    assert!(resolve_fact(db.conn(), "guest", &fact.id).is_err());
    let own = ensure_chapter(db.conn(), "guest", "Career", None, None).unwrap();
    let (saved, _) = tools
        .run(
            "memory",
            r#"{"action":"add","chapter":"Career","text":"private job search"}"#,
        )
        .await;
    assert!(saved.contains("saved ["), "{saved}");
    assert_eq!(facts(db.conn(), "guest").unwrap()[0].chapter_id, own.id);
    let (read, _) = tools
        .run("memory", r#"{"action":"read","chapter":"Career"}"#)
        .await;
    assert!(read.contains("private job search"));
    assert!(!read.contains("Rust engineer"));
    drop(tools);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shared_memory_moves_facts_to_home_and_caps_that_space() {
    let conn = conn();
    sharing_spaces(&conn);
    let chapter = ensure_chapter(&conn, "home", "Career", None, None).unwrap();
    share_chapter(&conn, &chapter.id, Share::All).unwrap();
    let fact = add(&conn, "guest", "Local", "compiler engineer", None, None)
        .unwrap()
        .fact;
    let moved = update_fact(&conn, "guest", &fact.id, None, Some("Career")).unwrap();
    assert_eq!(moved.space_id, "home");
    assert_eq!(moved.chapter_id, chapter.id);
    conn.execute(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < ?1)
         INSERT INTO memories (id, space_id, chapter_id, text, created_at, updated_at)
         SELECT 'cap-' || x, 'home', ?2, 'fact ' || x, 'now', 'now' FROM n",
        params![i64::try_from(MAX_FACTS - 1).unwrap(), chapter.id],
    )
    .unwrap();
    assert!(add(&conn, "guest", "Career", "one more", None, None).is_err());
    let other = add(&conn, "guest", "Local", "another engineer", None, None)
        .unwrap()
        .fact;
    assert!(update_fact(&conn, "guest", &other.id, None, Some("Career")).is_err());
    assert_eq!(
        resolve_fact(&conn, "guest", &other.id).unwrap().space_id,
        "guest"
    );
}

#[test]
fn merge_folds_same_titled_chapters_into_one_shared_chapter() {
    let conn = conn();
    sharing_spaces(&conn);
    let keep = add(
        &conn,
        "home",
        "About you",
        "Lives in Ulaanbaatar",
        None,
        None,
    )
    .unwrap();
    add(
        &conn,
        "guest",
        "about you",
        "lives in ulaanbaatar",
        Some("who you are"),
        Some(true),
    )
    .unwrap();
    add(
        &conn,
        "guest",
        "About you",
        "Prefers short answers",
        None,
        None,
    )
    .unwrap();
    let third = add(&conn, "third", "About You", "Studies Japanese", None, None).unwrap();
    share_chapter(
        &conn,
        &third.chapter.id,
        Share::Spaces(vec!["guest".into()]),
    )
    .unwrap();
    add(&conn, "third", "Unrelated", "keeps its chapter", None, None).unwrap();

    let merged = merge_same_titled(&conn, &keep.chapter).unwrap();

    assert_eq!((merged.moved, merged.duplicates), (2, 1));
    assert_eq!(merged.from, vec!["guest".to_string(), "third".to_string()]);
    // The spaces that had a copy, and anyone those copies were shared with, still see it.
    assert_eq!(
        merged.chapter.shared_with,
        Share::Spaces(vec!["guest".into(), "third".into()])
    );
    assert!(merged.chapter.pinned);
    assert_eq!(merged.chapter.description, "who you are");
    // Moved facts live in the chapter's home space, so each space's editor shows the right ones.
    let home: Vec<String> = facts(&conn, "home")
        .unwrap()
        .into_iter()
        .map(|f| f.text)
        .collect();
    assert_eq!(
        home,
        [
            "Lives in Ulaanbaatar",
            "Prefers short answers",
            "Studies Japanese"
        ]
    );
    assert!(facts(&conn, "guest").unwrap().is_empty());
    assert!(chapters(&conn, "guest").unwrap().is_empty());
    let left: Vec<String> = chapters(&conn, "third")
        .unwrap()
        .into_iter()
        .map(|c| c.title)
        .collect();
    assert_eq!(left, ["Unrelated"]);
    for space in ["guest", "third"] {
        assert!(
            find_visible_chapter(&conn, space, "About you")
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn widen_keeps_sharing_canonical() {
    let some = |ids: &[&str]| Share::Spaces(ids.iter().map(ToString::to_string).collect());
    assert_eq!(
        widen(&some(&["b"]), &some(&["a", "home", "b"]), "home"),
        some(&["a", "b"])
    );
    assert_eq!(widen(&Share::Private, &Share::All, "home"), Share::All);
    assert_eq!(
        widen(&Share::Private, &some(&["home"]), "home"),
        Share::Private
    );
}

#[tokio::test]
async fn memory_tool_shares_and_merges_by_name() {
    use crate::db::Db;
    use crate::tools::{MemoryCtx, ToolBox};
    let root = std::env::temp_dir().join(format!("nexus-memory-share-tool-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("nexus.db");
    let db = Db::open(&path).unwrap();
    sharing_spaces(db.conn());
    add(
        db.conn(),
        "home",
        "About you",
        "Lives in Ulaanbaatar",
        None,
        Some(true),
    )
    .unwrap();
    add(
        db.conn(),
        "third",
        "About you",
        "Studies Japanese",
        None,
        None,
    )
    .unwrap();
    let mut tools = ToolBox::new(
        root.join("skills"),
        None,
        None,
        "auto".into(),
        vec![],
        None,
        None,
        None,
    );
    tools.memory = Some(MemoryCtx {
        db_path: path,
        space_id: "guest".into(),
        sharing: true,
    });
    let run = |args: serde_json::Value| {
        let tools = &tools;
        async move { tools.run("memory", &args.to_string()).await.0 }
    };

    let overview = run(serde_json::json!({"action":"spaces"})).await;
    assert!(
        overview.starts_with("job-search (this space)"),
        "{overview}"
    );
    assert!(
        overview.contains("  - About you (1 facts, pinned)"),
        "{overview}"
    );
    assert!(
        overview.contains("\"About you\" in learning, third"),
        "{overview}"
    );

    // From another space: the chapter's home must be named, and the error says where it is.
    let missing =
        run(serde_json::json!({"action":"share","chapter":"About you","spaces":["all"]})).await;
    assert!(missing.contains("it lives in learning, third"), "{missing}");
    let wrong = run(serde_json::json!({"action":"share","chapter":"About you","spaces":["nope"],"space":"learning"})).await;
    assert!(
        wrong.contains("no space named \"nope\"; spaces: default, job-search, learning, third"),
        "{wrong}"
    );

    let no_list =
        run(serde_json::json!({"action":"share","chapter":"About you","space":"learning"})).await;
    assert!(
        no_list.contains("missing required field: spaces"),
        "{no_list}"
    );

    // Names match ignoring case; the list given is the exact new set.
    let shared = run(serde_json::json!({"action":"share","chapter":"about you","spaces":["Job-Search"],"space":"LEARNING"})).await;
    assert_eq!(
        shared,
        "\"About you\" (home: learning) is now shared with job-search"
    );
    let both = run(serde_json::json!({"action":"share","chapter":"About you","spaces":["job-search","third","learning"],"space":"learning"})).await;
    assert_eq!(
        both,
        "\"About you\" (home: learning) is now shared with job-search, third"
    );
    let back = run(serde_json::json!({"action":"share","chapter":"About you","spaces":["job-search"],"space":"learning"})).await;
    assert_eq!(
        back,
        "\"About you\" (home: learning) is now shared with job-search"
    );

    let merged =
        run(serde_json::json!({"action":"merge","chapter":"About you","space":"learning"})).await;
    assert_eq!(
        merged,
        "merged \"About you\" from third into learning: moved 1 fact, dropped 0 duplicates. \"About you\" (home: learning) is now shared with job-search, third"
    );
    let everyone = run(serde_json::json!({"action":"share","chapter":"About you","spaces":["all"],"space":"learning"})).await;
    assert_eq!(
        everyone,
        "\"About you\" (home: learning) is now shared with all spaces, including new ones"
    );
    let private = run(serde_json::json!({"action":"share","chapter":"About you","spaces":["none"],"space":"learning"})).await;
    assert_eq!(
        private,
        "\"About you\" (home: learning) is now private to learning"
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
