use crate::app::tests::app_with_key;
use crate::memory;

#[test]
fn every_space_memory_file_is_left_alone_at_refresh() {
    let mut app = app_with_key();
    let root = app.space.root.clone();
    let active = app.active_space.id.clone();
    let other = app.db.create_space("other").unwrap();
    app.space.ensure_space_dir(&other.name).unwrap();
    let path = app.space.memory_path(&other.name);
    std::fs::write(&path, "1. prefers Rust\n").unwrap();

    app.refresh_memory_snapshot();

    assert_eq!(app.active_space.id, active);
    let facts = memory::facts(app.db.conn(), &other.id).unwrap();
    assert_eq!(facts.len(), 0);
    assert!(memory::facts(app.db.conn(), &active).unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "1. prefers Rust\n");
    assert!(!path.with_file_name("memory.md.imported").exists());
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn memory_share_commands_parse_last_with_and_list_both_directions() {
    use crate::app::{AppCommand, AppEvent, MemoryAction};
    use memory::{Share, ShareTarget};
    let mut app = app_with_key();
    let root = app.space.root.clone();
    let home = app.active_space.id.clone();
    let a = app.db.create_space("a").unwrap();
    let b = app.db.create_space("b").unwrap();
    assert_eq!(
        app.parse_command("memory share X with a, b"),
        Ok(AppCommand::Memory {
            action: MemoryAction::Share {
                chapter: "X".into(),
                with: Some(ShareTarget::Spaces(vec!["a".into(), "b".into()]))
            },
        })
    );
    assert_eq!(
        app.parse_command("memory share Work with people with a, b"),
        Ok(AppCommand::Memory {
            action: MemoryAction::Share {
                chapter: "Work with people".into(),
                with: Some(ShareTarget::Spaces(vec!["a".into(), "b".into()]))
            },
        })
    );
    let own = memory::add(app.db.conn(), &home, "X", "local", None, None).unwrap();
    let incoming = memory::add(app.db.conn(), &a.id, "Incoming", "remote", None, None).unwrap();
    memory::share_chapter(
        app.db.conn(),
        &incoming.chapter.id,
        Share::Spaces(vec![home.clone()]),
    )
    .unwrap();
    let command = app.parse_command("memory share X with a, b").unwrap();
    app.execute(command).unwrap();
    assert_eq!(
        memory::chapters(app.db.conn(), &home).unwrap()[0].shared_with,
        Share::Spaces({
            let mut ids = vec![a.id.clone(), b.id];
            ids.sort();
            ids
        })
    );
    assert!(app.memory_snapshot().contains("X (shared with a, b)"));
    assert!(app.memory_snapshot().contains("Incoming (shared from a)"));
    let mut statuses = Vec::new();
    while let Some(event) = app.pop_pending_event() {
        if let AppEvent::Status(status) = event {
            statuses.push(status);
        }
    }
    assert!(
        statuses
            .iter()
            .any(|status| status == "\"X\" shared with a, b")
    );
    let listing = app.parse_command("memory share").unwrap();
    assert_eq!(
        listing,
        AppCommand::Memory {
            action: MemoryAction::Share {
                chapter: String::new(),
                with: None
            }
        }
    );
    app.execute(listing).unwrap();
    let Some(AppEvent::Status(listing)) = app.pop_pending_event() else {
        panic!("expected sharing listing");
    };
    assert!(listing.contains("X (shared with a, b)"));
    assert!(listing.contains("Incoming (shared from a)"));
    let before = memory::snapshot(app.db.conn(), &home).unwrap();
    let command = app.parse_command("memory share X with unknown").unwrap();
    assert!(
        app.execute(command)
            .unwrap_err()
            .to_string()
            .contains("no space named \"unknown\"")
    );
    assert_eq!(memory::snapshot(app.db.conn(), &home).unwrap(), before);
    let command = app.parse_command("memory share Incoming with all").unwrap();
    assert!(app.execute(command).is_err());
    for target in ["all", "none"] {
        let command = app
            .parse_command(&format!("memory share X with {target}"))
            .unwrap();
        app.execute(command).unwrap();
    }
    assert_eq!(
        memory::chapters(app.db.conn(), &home).unwrap()[0].shared_with,
        Share::Private
    );
    assert_eq!(
        memory::chapters(app.db.conn(), &home).unwrap()[0].id,
        own.chapter.id
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shared_memory_refresh_and_hint_include_incoming_chapters() {
    let mut app = app_with_key();
    let root = app.space.root.clone();
    let home = app.db.create_space("learning").unwrap();
    let added = memory::add(
        app.db.conn(),
        &home.id,
        "Career",
        "Rust compiler engineer",
        None,
        None,
    )
    .unwrap();
    memory::share_chapter(app.db.conn(), &added.chapter.id, memory::Share::All).unwrap();
    app.refresh_memory_snapshot();
    assert!(
        app.memory_snapshot()
            .contains("Career (shared from learning)")
    );
    assert_eq!(
        app.memory_hint("Find Rust compiler positions for me")
            .unwrap()
            .targets[0]
            .label,
        "Career"
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}
