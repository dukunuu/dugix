use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::*;
use crate::db::{Db, cache_get, cache_path_for};

const DEV: &str = include_str!("../../tests/fixtures/db/durable-dev.sql");
const RELEASED: &str = include_str!("../../tests/fixtures/db/durable-0.1.42.sql");
const CACHE_DEV: &str = include_str!("../../tests/fixtures/db/cache-dev.sql");

// Fresh databases mark these NOT NULL; upgraded ones left them nullable.
const FIXTURE_NULLABILITY_VARIANTS: &[(&str, &str)] = &[
    ("files", "updated_at"),
    ("citations", "sync_id"),
    ("usage_log", "sync_id"),
];

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexus-migrations-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn db_path(&self) -> PathBuf {
        self.0.join("nexus.db")
    }

    fn fixture(&self, sql: &str) {
        Connection::open(self.db_path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn context<'a>(schema: &'a str, file: &'static str) -> Ctx<'a> {
    Ctx {
        schema,
        file,
        root: None,
        notes: Vec::new(),
    }
}

fn schema(conn: &Connection, name: &str) -> String {
    schema_ignoring_nullability(conn, name, &[])
}

fn schema_ignoring_nullability(
    conn: &Connection,
    name: &str,
    ignored_not_null: &[(&str, &str)],
) -> String {
    let mut stmt = conn.prepare(&format!("PRAGMA {name}.table_list")).unwrap();
    let mut tables = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .filter(|(table, kind)| !table.starts_with("sqlite_") && kind != "shadow")
        .collect::<Vec<_>>();
    tables.sort();
    let mut text = String::new();
    for (table, kind) in tables {
        if kind == "virtual" {
            let sql: String = conn
                .query_row(
                    &format!("SELECT sql FROM {name}.sqlite_schema WHERE name = ?1"),
                    [&table],
                    |row| row.get(0),
                )
                .unwrap();
            writeln!(
                text,
                "{};",
                sql.split_whitespace().collect::<Vec<_>>().join(" ")
            )
            .unwrap();
            continue;
        }
        writeln!(text, "table {table}").unwrap();
        let mut columns = conn
            .prepare(&format!("PRAGMA {name}.table_info({table})"))
            .unwrap();
        let mut columns = columns
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        columns.sort();
        for (column, ty, required, default, pk) in columns {
            let required =
                required && !ignored_not_null.contains(&(table.as_str(), column.as_str()));
            writeln!(
                text,
                "  {column}: {ty} not_null={required} default={default:?} pk={pk}"
            )
            .unwrap();
        }
        let mut indexes = conn
            .prepare(&format!("PRAGMA {name}.index_list({table})"))
            .unwrap();
        let mut indexes = indexes
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, bool>(2)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .filter(|(index, _)| !index.starts_with("sqlite_"))
            .collect::<Vec<_>>();
        indexes.sort();
        for (index, unique) in indexes {
            let mut info = conn
                .prepare(&format!("PRAGMA {name}.index_info({index})"))
                .unwrap();
            let columns = info
                .query_map([], |row| row.get::<_, String>(2))
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                text,
                "  index {index} on {table}({columns}) unique={unique}"
            )
            .unwrap();
        }
    }
    text
}

fn assert_snapshot(file: &str, actual: &str) {
    let directory = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/schema-snapshots/"
    ));
    let path = directory.join(file);
    if std::env::var("NEXUS_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(path, actual).unwrap();
        return;
    }
    let message = format!(
        "snapshot {}: rerun with NEXUS_UPDATE_SNAPSHOTS=1 cargo test -p nexus-chat-core db:: and review the diff",
        path.display()
    );
    let expected =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{message}: {error}"));
    assert_eq!(expected, actual, "{message}");
}

#[test]
fn memory_sharing_migration_defaults_existing_chapters_and_only_adds_its_column() {
    let conn = Connection::open_in_memory().unwrap();
    run(&conn, &DURABLE[..3], &mut context("main", "nexus.db")).unwrap();
    conn.execute_batch(
        "INSERT INTO memory_chapters (id, space_id, title, created_at, updated_at)
         VALUES ('existing', 'home', 'Career', 'created', 'updated')",
    )
    .unwrap();
    let before = schema(&conn, "main");
    run(&conn, DURABLE, &mut context("main", "nexus.db")).unwrap();
    let shared: String = conn
        .query_row(
            "SELECT shared_with FROM memory_chapters WHERE id = 'existing'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(shared, "[]");
    let after = schema(&conn, "main");
    let column = "  shared_with: TEXT not_null=true default=Some(\"'[]'\") pk=0\n";
    assert!(after.contains(column));
    assert_eq!(after.replace(column, ""), before);
    assert_snapshot("durable.txt", &after);
}

#[test]
fn fresh_schemas_match_snapshots() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(version(db.conn(), "main").unwrap(), 7);
    assert_eq!(version(db.conn(), "cache").unwrap(), 3);
    assert_snapshot("durable.txt", &schema(db.conn(), "main"));
    assert_snapshot("cache.txt", &schema(db.conn(), "cache"));
}

#[test]
fn durable_fixtures_reach_the_fresh_schema_and_preserve_rows() {
    let fresh = Db::open_in_memory().unwrap();
    let expected = schema_ignoring_nullability(fresh.conn(), "main", FIXTURE_NULLABILITY_VARIANTS);
    for fixture in [DEV, RELEASED] {
        let root = TempRoot::new();
        root.fixture(fixture);
        let db = Db::open(&root.db_path()).unwrap();
        assert_eq!(version(db.conn(), "main").unwrap(), 7);
        let actual = schema_ignoring_nullability(db.conn(), "main", FIXTURE_NULLABILITY_VARIANTS);
        assert_eq!(actual, expected);
        assert_snapshot("durable.txt", &actual);
        assert_eq!(db.list_sessions("space-1").unwrap()[0].id, "session-1");
        assert_eq!(
            db.load_messages("session-1").unwrap()[0].content,
            "Synthetic message"
        );
        assert_eq!(db.setting("vim").as_deref(), Some("true"));
        assert_eq!(db.setting("helper_model"), None);
        let usage: (String, i64, i64) = db
            .conn()
            .query_row(
                "SELECT sync_id, prompt_tokens, completion_tokens FROM usage_log WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(usage, ("usage-1".into(), 10, 2));
        drop(db);
        let db = Db::open(&root.db_path()).unwrap();
        assert_eq!(
            run(db.conn(), DURABLE, &mut context("main", "nexus.db")).unwrap(),
            0
        );
        assert_eq!(
            run(db.conn(), CACHE, &mut context("cache", "cache.db")).unwrap(),
            0
        );
    }
}

#[test]
fn cache_fixture_preserves_embeddings_in_attached_and_standalone_schemas() {
    let fresh = Db::open_in_memory().unwrap();
    let expected = schema(fresh.conn(), "cache");
    for attached in [false, true] {
        let root = TempRoot::new();
        let path = root.db_path();
        let conn = Connection::open(cache_path_for(&path)).unwrap();
        conn.execute_batch(CACHE_DEV).unwrap();
        drop(conn);
        let db;
        let standalone;
        let (conn, name) = if attached {
            db = Db::open(&path).unwrap();
            (db.conn(), "cache")
        } else {
            standalone = Connection::open(cache_path_for(&path)).unwrap();
            migrate_cache(&standalone, "main").unwrap();
            (&standalone, "main")
        };
        let actual = schema(conn, name);
        assert_eq!(actual, expected);
        assert_snapshot("cache.txt", &actual);
        assert_eq!(version(conn, name).unwrap(), 3);
        let embedding: (String, Vec<u8>, String) = conn
            .query_row(
                &format!("SELECT file_id, vec, text FROM {name}.chunk_embeddings"),
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            embedding,
            (
                "file-1".into(),
                vec![0, 0, 128, 63],
                "Synthetic embedding".into()
            )
        );
        assert_eq!(run(conn, CACHE, &mut context(name, "cache.db")).unwrap(), 0);
    }
}

#[test]
fn cache_baseline_adds_all_missing_columns() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE chunk_embeddings (file_id TEXT NOT NULL, seq INTEGER NOT NULL,
            vec BLOB NOT NULL, PRIMARY KEY(file_id, seq));
         INSERT INTO chunk_embeddings VALUES ('file-1', 0, X'00');
         CREATE TABLE session_workspaces (session_id TEXT PRIMARY KEY, root TEXT NOT NULL,
            updated_at TEXT NOT NULL);
         CREATE TABLE file_index_state (file_id TEXT PRIMARY KEY,
            mtime INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL);
         CREATE TABLE decisions (id INTEGER PRIMARY KEY, at TEXT, session_id TEXT, tool TEXT,
            action TEXT, class TEXT, summary TEXT, by TEXT, verdict TEXT, p_in_scope REAL,
            risk TEXT, user_answer TEXT);"
    ).unwrap();
    assert_eq!(
        run(&conn, CACHE, &mut context("main", "cache.db")).unwrap(),
        3
    );
    assert_snapshot("cache.txt", &schema(&conn, "main"));
    let identity: String = conn
        .query_row("SELECT identity FROM chunk_embeddings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(identity, "");
}

#[test]
fn unsupported_durable_versions_have_exact_errors() {
    for (v, sessions) in [(1, false), (2, false), (0, true), (99, false)] {
        let root = TempRoot::new();
        let conn = Connection::open(root.db_path()).unwrap();
        conn.pragma_update(None, "user_version", v).unwrap();
        if sessions {
            conn.execute_batch("CREATE TABLE sessions (id TEXT)")
                .unwrap();
        }
        drop(conn);
        let error = Db::open(&root.db_path()).err().unwrap().to_string();
        let expected = if v == 99 {
            "nexus.db was written by a newer nexus (schema 99, this build knows up to 7); update nexus".into()
        } else {
            format!(
                "nexus.db is from a nexus older than 0.1.42 (schema {v}); install nexus-chat 0.1.42 once to upgrade it, then this version"
            )
        };
        assert_eq!(error, expected);
    }
}

#[test]
fn newer_cache_versions_have_exact_errors() {
    let root = TempRoot::new();
    let conn = Connection::open(cache_path_for(&root.db_path())).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    let expected =
        "cache.db was written by a newer nexus (schema 99, this build knows up to 3); update nexus";
    assert_eq!(
        migrate_cache(&conn, "main").unwrap_err().to_string(),
        expected
    );
    drop(conn);
    assert_eq!(
        Db::open(&root.db_path()).err().unwrap().to_string(),
        expected
    );
}

#[test]
fn failed_step_rolls_back_without_undoing_committed_steps() {
    let steps = [
        Step {
            target: 1,
            name: "first",
            run: |tx, _| {
                tx.execute_batch("CREATE TABLE kept (id INTEGER); INSERT INTO kept VALUES (1);")?;
                Ok(())
            },
        },
        Step {
            target: 2,
            name: "failure",
            run: |tx, _| {
                tx.execute_batch("CREATE TABLE absent (id INTEGER); INSERT INTO kept VALUES (2);")?;
                bail!("intentional failure")
            },
        },
    ];
    let conn = Connection::open_in_memory().unwrap();
    let error = run(&conn, &steps, &mut context("main", "test.db")).unwrap_err();
    assert!(format!("{error:#}").contains("intentional failure"));
    assert_eq!(version(&conn, "main").unwrap(), 1);
    assert!(!has_column(&conn, "absent", "id").unwrap());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM kept", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn legacy_memory_is_imported_only_by_the_durable_baseline() {
    let root = TempRoot::new();
    root.fixture(DEV);
    let directory = root.0.join("spaces/work");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("memory.md");
    std::fs::write(&path, "1. likes rust\n2. uses vim\n").unwrap();
    let mut db = Db::open(&root.db_path()).unwrap();
    let facts = crate::memory::facts(db.conn(), "space-1").unwrap();
    let mut texts = facts
        .iter()
        .map(|fact| fact.text.as_str())
        .collect::<Vec<_>>();
    texts.sort_unstable();
    assert_eq!(texts, ["likes rust", "uses vim"]);
    assert!(!path.exists());
    assert!(directory.join("memory.md.imported").is_file());
    assert_eq!(
        db.take_upgrade_notes(),
        ["memory moved into the database · 2 facts in \"About you\""]
    );
    assert_eq!(db.take_upgrade_notes(), Vec::<String>::new());
    drop(db);
    std::fs::write(&path, "1. not imported\n").unwrap();
    let mut db = Db::open(&root.db_path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "1. not imported\n");
    assert_eq!(crate::memory::facts(db.conn(), "space-1").unwrap().len(), 2);
    assert_eq!(db.take_upgrade_notes(), Vec::<String>::new());
}

#[test]
fn dropping_the_old_web_cache_exposes_the_device_cache() {
    let root = TempRoot::new();
    root.fixture(DEV);
    let conn = Connection::open(root.db_path()).unwrap();
    conn.execute_batch(
        "INSERT INTO main.web_cache VALUES ('url', 'url', NULL, 'stale main', 'now')",
    )
    .unwrap();
    drop(conn);
    let conn = Connection::open(cache_path_for(&root.db_path())).unwrap();
    conn.execute_batch(CACHE_DEV).unwrap();
    conn.execute_batch("INSERT INTO web_cache VALUES ('url', 'url', NULL, 'device cache', 'now')")
        .unwrap();
    drop(conn);
    let db = Db::open(&root.db_path()).unwrap();
    assert_eq!(
        cache_get(db.conn(), "url").unwrap().unwrap().1,
        "device cache"
    );
}

#[test]
fn durable_upgrades_preserve_sessions_and_chapters() {
    let fresh = Db::open_in_memory().unwrap();
    assert_eq!(durable_schema_version(), 7);
    assert_eq!(version(fresh.conn(), "main").unwrap(), 7);
    let expected = schema_ignoring_nullability(fresh.conn(), "main", FIXTURE_NULLABILITY_VARIANTS);
    assert_snapshot("durable.txt", &expected);
    for fixture in [DEV, RELEASED] {
        let root = TempRoot::new();
        root.fixture(fixture);
        let conn = Connection::open(root.db_path()).unwrap();
        run(&conn, &DURABLE[..1], &mut context("main", "nexus.db")).unwrap();
        assert_eq!(version(&conn, "main").unwrap(), 4);
        conn.execute_batch(
            "INSERT INTO sessions (id, title, model, space_id, created_at, updated_at)
             VALUES ('kept-session', 'Keep this session', 'm', 'space-1', 'created', 'updated');
             INSERT INTO memory_chapters (id, space_id, title, description, pinned, created_at, updated_at)
             VALUES ('kept-chapter', 'space-1', 'Tools', 'Keep this chapter', 1, 'created', 'updated');",
        )
        .unwrap();
        assert_eq!(
            run(&conn, DURABLE, &mut context("main", "nexus.db")).unwrap(),
            3
        );
        assert_eq!(version(&conn, "main").unwrap(), 7);
        assert_eq!(
            schema_ignoring_nullability(&conn, "main", FIXTURE_NULLABILITY_VARIANTS),
            expected
        );
        let session: (String, String, String, String, String) = conn
            .query_row(
                "SELECT title, model, space_id, created_at, updated_at FROM sessions WHERE id = 'kept-session'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            session,
            (
                "Keep this session".into(),
                "m".into(),
                "space-1".into(),
                "created".into(),
                "updated".into()
            )
        );
        let chapter = crate::memory::find_chapter(&conn, "space-1", "Tools")
            .unwrap()
            .unwrap();
        assert_eq!(chapter.id, "kept-chapter");
        assert_eq!(chapter.description, "Keep this chapter");
        assert!(chapter.pinned);
        assert_eq!(chapter.created_at, "created");
        assert_eq!(chapter.updated_at, "updated");
        assert_eq!(chapter.deleted_at, None);
    }
}
