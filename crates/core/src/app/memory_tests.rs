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
