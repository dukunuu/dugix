use super::*;
use crate::db::Db;
use std::io::Write as _;

/// A fresh device pair: in-memory dbs plus temp roots for the spaces layout.
struct Pair {
    a: Db,
    b: Db,
    a_root: PathBuf,
    b_root: PathBuf,
    dir: PathBuf,
}

impl Pair {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("nexus-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            a: Db::open_in_memory().unwrap(),
            b: Db::open_in_memory().unwrap(),
            a_root: dir.join("a"),
            b_root: dir.join("b"),
            dir,
        }
    }

    fn space_a(&self) -> Space {
        Space {
            root: self.a_root.clone(),
        }
    }

    fn space_b(&self) -> Space {
        Space {
            root: self.b_root.clone(),
        }
    }

    /// One full A→B exchange, blobs and ack included.
    fn exchange_ab(&self) -> (ApplySummary, ApplySummary) {
        let a_space = self.space_a();
        let b_space = self.space_b();
        let cs = build_changeset(&self.a, None, "a").unwrap();
        export_blobs(&self.a, &a_space, &cs, &self.dir).unwrap();
        let (sa, cursors) = apply_changeset(&self.b, &b_space, &cs, Some(&self.dir)).unwrap();
        let mut reply = build_changeset(&self.b, Some(&cs.device_id), "b").unwrap();
        reply.ack = Some(cursors);
        export_blobs(&self.b, &b_space, &reply, &self.dir).unwrap();
        let (sb, _) = apply_changeset(&self.a, &a_space, &reply, Some(&self.dir)).unwrap();
        (sa, sb)
    }

    fn exchange_ba(&self) -> (ApplySummary, ApplySummary) {
        let a_space = self.space_a();
        let b_space = self.space_b();
        let cs = build_changeset(&self.b, None, "b").unwrap();
        export_blobs(&self.b, &b_space, &cs, &self.dir).unwrap();
        let (sb, cursors) = apply_changeset(&self.a, &a_space, &cs, Some(&self.dir)).unwrap();
        let mut reply = build_changeset(&self.a, Some(&cs.device_id), "a").unwrap();
        reply.ack = Some(cursors);
        export_blobs(&self.a, &a_space, &reply, &self.dir).unwrap();
        let (sa, _) = apply_changeset(&self.b, &b_space, &reply, Some(&self.dir)).unwrap();
        (sb, sa)
    }

    fn exchange(&self) -> (ApplySummary, ApplySummary) {
        let (_, _) = self.exchange_ab();
        self.exchange_ba()
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Every syncable row on a device as (table, pk, json): two devices are converged
/// iff their dumps are equal. The default space is excluded because it never
/// exports.
fn dump(db: &Db) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for spec in TABLES {
        if spec.cursor == Cursor::None {
            continue;
        }
        let rows = select_rows(db, spec, None).unwrap();
        for row in rows {
            let pk = spec
                .pk
                .iter()
                .map(|c| {
                    row.get(*c)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(":");
            out.push((spec.name.to_string(), pk, row.to_string()));
        }
    }
    let mut stmt = db
        .conn()
        .prepare("SELECT table_name, row_id, deleted_at FROM sync_tombstones ORDER BY row_id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for (t, rid, _) in rows {
        out.push(("tombstone".to_string(), format!("{t}:{rid}"), String::new()));
    }
    out.sort();
    out
}

fn space(db: &Db, name: &str) -> String {
    db.list_spaces()
        .unwrap()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap()
        .id
}

fn session(db: &Db, title: &str) -> String {
    let sid = space(db, "default");
    let s = db.create_session(title, "m", &sid, "chat").unwrap();
    s.id
}

fn set_updated(db: &Db, table: &str, id: &str, at: &str) {
    db.conn()
        .execute(
            &format!("UPDATE {table} SET updated_at = ?1 WHERE id = ?2"),
            (at, id),
        )
        .unwrap();
}

fn session_updated(db: &Db, id: &str) -> String {
    db.conn()
        .query_row("SELECT updated_at FROM sessions WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

fn minus_one_second(rfc3339: &str) -> String {
    let dt = chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap();
    (dt - chrono::Duration::seconds(1)).to_rfc3339()
}

fn write_blob(space: &Space, space_name: &str, name: &str, bytes: &[u8]) {
    let dir = space.files_dir(space_name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

fn add_session_sources(db: &Db, session_id: &str, url_norms: &[String]) {
    crate::db::add_session_sources(db.conn(), session_id, url_norms).unwrap();
}

fn db_state(db: &Db) -> Vec<crate::db::SyncState> {
    db.load_sync_state().unwrap()
}

#[test]
fn changeset_serde_roundtrip() {
    let cs = Changeset {
        schema: crate::db::migrations::durable_schema_version(),
        device_id: "dev-1".to_string(),
        device_name: "laptop".to_string(),
        ack: Some(vec![PeerCursor {
            peer_id: "dev-2".to_string(),
            table_name: "sessions".to_string(),
            cursor: "2026-01-01T00:00:00Z|id".to_string(),
        }]),
        rows: vec![RowChange {
            table: "sessions".to_string(),
            row: serde_json::json!({"id": "s1", "title": "t"}),
        }],
        tombstones: vec![Tombstone {
            origin_id: 3,
            table_name: "sessions".to_string(),
            row_id: "s1".to_string(),
            deleted_at: "2026-01-02T00:00:00Z".to_string(),
        }],
        files: vec![FileChange {
            space_id: "sp".to_string(),
            name: "f.txt".to_string(),
            hash: "abc".to_string(),
            size: 3,
        }],
        generated_at: "2026-01-03T00:00:00Z".to_string(),
    };
    let json = serde_json::to_string(&cs).unwrap();
    let back: Changeset = serde_json::from_str(&json).unwrap();
    assert_eq!(cs, back);
}

#[test]
fn cursor_positions_compare_table_aware() {
    assert!(position_gt("citations", "10", "9"));
    assert!(position_gt("sync_tombstones", "10", "9"));
    assert!(position_gt(
        "sessions",
        r#"["2026-02-01T00:00:00Z",["a"]]"#,
        r#"["2026-01-01T00:00:00Z",["a"]]"#
    ));
    assert!(!position_gt(
        "sessions",
        r#"["2026-01-01T00:00:00Z",["a"]]"#,
        r#"["2026-01-01T00:00:00Z",["a"]]"#
    ));
}

#[test]
fn unsafe_components_are_rejected() {
    assert!(valid_component("notes.txt"));
    assert!(!valid_component(""));
    assert!(!valid_component("."));
    assert!(!valid_component(".."));
    assert!(!valid_component("a/b"));
    assert!(!valid_component("a\\b"));
    assert!(!valid_component("a\0b"));
}

#[test]
fn warned_rows_do_not_advance_their_table_cursor() {
    let pair = Pair::new();
    let changeset = Changeset {
        schema: crate::db::migrations::durable_schema_version(),
        device_id: "peer".to_string(),
        device_name: "peer".to_string(),
        ack: None,
        rows: vec![RowChange {
            table: "files".to_string(),
            row: serde_json::json!({
                "id": "file-1",
                "space_id": "default",
                "name": "../escape.txt",
                "hash": "hash",
                "size": 4,
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z"
            }),
        }],
        tombstones: Vec::new(),
        files: Vec::new(),
        generated_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let (summary, cursors) = apply_changeset(&pair.b, &pair.space_b(), &changeset, None).unwrap();
    assert!(!summary.warnings.is_empty());
    assert!(!cursors.iter().any(|cursor| cursor.table_name == "files"));
}

#[test]
fn cold_start_full_export_covers_every_table() {
    let pair = Pair::new();
    let a = &pair.a;
    let s = session(a, "hello");
    a.add_user_message(&s, "hi").unwrap();
    a.log_usage(
        "openrouter",
        "m",
        1,
        2,
        0,
        0,
        Default::default(),
        Some(0.1),
        true,
        Some(&s),
        None,
    )
    .unwrap();
    a.add_citations(&space(a, "default"), "r.md", &[("https://x".into(), None)])
        .unwrap();
    add_session_sources(a, &s, &["https://x".to_string()]);
    a.create_watch(&space(a, "default"), "topic", 24, &s)
        .unwrap();
    a.set_reasoning("m", Some("high")).unwrap();
    a.set_setting("theme", "dark").unwrap();
    a.create_space("work").unwrap();
    let persona = crate::db::Persona {
        name: "p".to_string(),
        model: "m".to_string(),
        blurb: "b".to_string(),
    };
    a.save_swarm_personas(&s, &[persona]).unwrap();
    a.upsert_file(&space(a, "default"), "f.txt", "deadbeef", 3, "ok")
        .unwrap();

    let cs = build_changeset(a, None, "a").unwrap();
    let tables: HashSet<&str> = cs.rows.iter().map(|r| r.table.as_str()).collect();
    for expected in [
        "sessions",
        "swarm_personas",
        "model_prefs",
        "spaces",
        "files",
        "watches",
        "app_settings",
        "session_sources",
        "messages",
        "usage_log",
        "citations",
    ] {
        assert!(tables.contains(expected), "missing {expected}");
    }
    assert_eq!(cs.files.len(), 1);
    assert_eq!(cs.files[0].space_id, space(a, "default"));
    assert_eq!(cs.files[0].name, "f.txt");
    assert_eq!(cs.files[0].hash, "deadbeef");
    assert_eq!(cs.files[0].size, 3);
    let personas: Vec<_> = cs
        .rows
        .iter()
        .filter(|r| r.table == "swarm_personas")
        .collect();
    assert_eq!(personas.len(), 1);
    assert_eq!(personas[0].row["session_id"], s);
    assert_eq!(personas[0].row["name"], "p");
    assert!(cs.ack.is_none());
    assert!(
        !cs.rows
            .iter()
            .any(|r| r.table == "app_settings" && r.row["scope"] == "local")
    );
}

#[test]
fn export_resumes_past_acked_cursor_only() {
    let pair = Pair::new();
    let a = &pair.a;
    let s1 = session(a, "one");
    let cs1 = build_changeset(a, None, "a").unwrap();
    assert!(!cs1.rows.is_empty());

    let cs_again = build_changeset(a, Some("peer-x"), "a").unwrap();
    assert_eq!(cs_again.rows.len(), cs1.rows.len());

    let sess_pos = cs1
        .rows
        .iter()
        .find(|r| r.table == "sessions" && r.row["id"] == s1)
        .map(|r| row_position(spec_for("sessions").unwrap(), &r.row))
        .unwrap();
    a.set_sync_state("peer-x", "sessions", None, Some(&sess_pos))
        .unwrap();
    let cs2 = build_changeset(a, Some("peer-x"), "a").unwrap();
    assert!(!cs2.rows.iter().any(|r| r.table == "sessions"));
    let _s2 = session(a, "two");
    let cs3 = build_changeset(a, Some("peer-x"), "a").unwrap();
    assert!(
        cs3.rows
            .iter()
            .any(|r| r.table == "sessions" && r.row["title"] == "two")
    );
    let _ = s1;
}

#[test]
fn updated_cursor_keeps_rows_sharing_a_timestamp() {
    let pair = Pair::new();
    let a = &pair.a;
    let first = session(a, "first");
    let second = session(a, "second");
    let timestamp = "2026-02-01T00:00:00Z";
    set_session_updated(a, &first, timestamp);
    set_session_updated(a, &second, timestamp);

    let full = build_changeset(a, None, "a").unwrap();
    let sessions: Vec<&RowChange> = full
        .rows
        .iter()
        .filter(|row| row.table == "sessions")
        .collect();
    assert!(sessions.iter().any(|row| row.row["id"] == first));
    assert!(sessions.iter().any(|row| row.row["id"] == second));

    let first_row = sessions.first().expect("session export is non-empty");
    let cursor = row_position(spec_for("sessions").unwrap(), &first_row.row);
    a.set_sync_state("peer-x", "sessions", None, Some(&cursor))
        .unwrap();
    let resumed = build_changeset(a, Some("peer-x"), "a").unwrap();
    let resumed_ids: Vec<&str> = resumed
        .rows
        .iter()
        .filter(|row| row.table == "sessions")
        .map(|row| row.row["id"].as_str().unwrap())
        .collect();
    assert_eq!(resumed_ids.len(), 1);
    assert_ne!(resumed_ids[0], first_row.row["id"].as_str().unwrap());
}

#[test]
fn lww_newer_wins_older_loses() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let sa = session(a, "from a");
    let sb = session(b, "from b");
    b.conn()
        .execute("UPDATE sessions SET id = ?1 WHERE id = ?2", (&sa, &sb))
        .unwrap();
    set_session_updated(a, &sa, "2026-02-01T00:00:00Z");
    set_session_updated(b, &sa, "2026-01-01T00:00:00Z");
    pair.exchange_ab();
    let title = |db: &Db| db.get_session(&sa).unwrap().unwrap().title.clone();
    assert_eq!(title(a), "from a");
    assert_eq!(title(b), "from a");
    let cs = build_changeset(b, None, "b").unwrap();
    let _ = apply_changeset(&pair.b, &pair.space_b(), &cs, None).unwrap();
    assert_eq!(title(a), "from a");
}

fn set_session_updated(db: &Db, id: &str, at: &str) {
    set_updated(db, "sessions", id, at);
}

#[test]
fn equal_timestamp_keeps_local_and_is_stable() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let sa = session(a, "a's title");
    let sb = session(b, "b's title");
    let sid = sa.clone();
    b.conn()
        .execute("UPDATE sessions SET id = ?1 WHERE id = ?2", (&sid, &sb))
        .unwrap();
    set_session_updated(a, &sid, "2026-01-01T00:00:00Z");
    set_session_updated(b, &sid, "2026-01-01T00:00:00Z");
    let title_a = a.get_session(&sid).unwrap().unwrap().title.clone();
    let title_b = b.get_session(&sid).unwrap().unwrap().title.clone();
    pair.exchange();
    assert_eq!(a.get_session(&sid).unwrap().unwrap().title, title_a);
    assert_eq!(b.get_session(&sid).unwrap().unwrap().title, title_b);
    pair.exchange();
    assert_eq!(a.get_session(&sid).unwrap().unwrap().title, title_a);
    assert_eq!(b.get_session(&sid).unwrap().unwrap().title, title_b);
}

#[test]
fn clock_skew_loser_converges() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let sa = session(a, "fast clock");
    let sb = session(b, "slow clock");
    let sid = sa.clone();
    b.conn()
        .execute("UPDATE sessions SET id = ?1 WHERE id = ?2", (&sid, &sb))
        .unwrap();
    set_session_updated(a, &sid, "2026-02-01T00:00:00Z");
    set_session_updated(b, &sid, "2026-01-01T00:00:00Z");
    pair.exchange();
    assert_eq!(a.get_session(&sid).unwrap().unwrap().title, "fast clock");
    assert_eq!(b.get_session(&sid).unwrap().unwrap().title, "fast clock");
}

#[test]
fn scope_local_setting_never_syncs_or_applies() {
    let pair = Pair::new();
    let a = &pair.a;
    a.set_setting("searxng_url", "http://localhost:8888")
        .unwrap();
    a.set_setting("theme", "dark").unwrap();
    let cs = build_changeset(a, None, "a").unwrap();
    let keys: Vec<&str> = cs
        .rows
        .iter()
        .filter(|r| r.table == "app_settings")
        .map(|r| r.row["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["theme"]);
    let mut evil = cs.clone();
    evil.rows.push(RowChange {
        table: "app_settings".to_string(),
        row: serde_json::json!({
            "key": "searxng_url", "value": "http://evil", "scope": "local",
            "updated_at": "2099-01-01T00:00:00Z",
        }),
    });
    let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &evil, None).unwrap();
    assert!(summary.rows_skipped >= 1);
    let settings = pair.b.load_settings().unwrap();
    assert!(!settings.iter().any(|(k, _)| k == "searxng_url"));
}

#[test]
fn messages_union_dedupes_by_id() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let sa = session(a, "shared");
    let cs = build_changeset(a, None, "a").unwrap();
    let _ = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
    a.add_user_message(&sa, "from a").unwrap();
    let sb = b.get_session(&sa).unwrap().unwrap().id;
    b.add_user_message(&sb, "from b").unwrap();
    pair.exchange();
    let count = |db: &Db| db.load_messages(&sa).unwrap().len();
    assert_eq!(count(a), 2);
    assert_eq!(count(b), 2);
    let (sa2, _) = pair.exchange_ab();
    assert_eq!(count(a), 2);
    assert_eq!(count(b), 2);
    assert_eq!(sa2.rows_applied, 0);
}

#[test]
fn usage_and_citations_dedupe_by_sync_id() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let s = session(a, "shared");
    a.log_usage(
        "openrouter",
        "m",
        1,
        2,
        0,
        0,
        Default::default(),
        None,
        false,
        Some(&s),
        None,
    )
    .unwrap();
    let cs = build_changeset(a, None, "a").unwrap();
    let _ = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
    b.log_usage(
        "openrouter",
        "m",
        3,
        4,
        0,
        0,
        Default::default(),
        None,
        false,
        Some(&s),
        None,
    )
    .unwrap();
    a.add_citations(&space(a, "default"), "r.md", &[("https://a".into(), None)])
        .unwrap();
    let cs2 = build_changeset(a, None, "a").unwrap();
    let _ = apply_changeset(b, &pair.space_b(), &cs2, None).unwrap();
    b.add_citations(&space(b, "default"), "r2.md", &[("https://b".into(), None)])
        .unwrap();
    pair.exchange();
    let usage = |db: &Db| {
        db.conn()
            .query_row("SELECT COUNT(*) FROM usage_log", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let cites = |db: &Db| {
        db.conn()
            .query_row("SELECT COUNT(*) FROM citations", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(usage(a), 2);
    assert_eq!(usage(b), 2);
    assert_eq!(cites(a), 2);
    assert_eq!(cites(b), 2);
    for db in [a, b] {
        let dupes: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM (SELECT sync_id FROM usage_log UNION ALL \
                 SELECT sync_id FROM citations) GROUP BY sync_id HAVING COUNT(*) > 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        assert_eq!(dupes, 0);
    }
}

#[test]
fn two_way_convergence_and_then_silence() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let sa = session(a, "a's chat");
    a.add_user_message(&sa, "from a").unwrap();
    a.set_setting("theme", "dark").unwrap();
    a.set_reasoning("m1", Some("high")).unwrap();
    a.log_usage(
        "openrouter",
        "m1",
        1,
        2,
        0,
        0,
        Default::default(),
        None,
        false,
        Some(&sa),
        None,
    )
    .unwrap();
    let sb = session(b, "b's chat");
    b.add_user_message(&sb, "from b").unwrap();
    b.add_user_message(&sb, "and another").unwrap();
    b.create_watch(&space(b, "default"), "topic", 24, &sb)
        .unwrap();

    pair.exchange();
    assert_eq!(dump(a), dump(b), "devices must converge");

    let (sa2, sb2) = pair.exchange_ab();
    assert_eq!(sa2.rows_applied, 0);
    assert_eq!(sb2.rows_applied, 0);
    assert_eq!(dump(a), dump(b));
}

#[test]
fn idempotent_reimport_is_a_noop() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let s = session(a, "hello");
    a.add_user_message(&s, "hi").unwrap();
    a.log_usage(
        "openrouter",
        "m",
        1,
        2,
        0,
        0,
        Default::default(),
        None,
        false,
        Some(&s),
        None,
    )
    .unwrap();
    let cs = build_changeset(a, None, "a").unwrap();
    let (first, _) = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
    assert!(first.rows_applied > 0);
    let (again, _) = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
    assert_eq!(again.rows_applied, 0);
    assert_eq!(again.tombstones_applied, 0);
    assert!(again.warnings.is_empty());
}

#[test]
fn session_sources_lww_flag_propagates() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let s = session(a, "s");
    add_session_sources(a, &s, &["https://x".to_string()]);
    let cs = build_changeset(a, None, "a").unwrap();
    let _ = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
    a.set_source_flag(&s, "https://x", Some("pinned")).unwrap();
    pair.exchange_ab();
    let flags: Vec<(String, String)> = b
        .conn()
        .prepare("SELECT session_id, flag FROM session_sources")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(flags, vec![(s, "pinned".to_string())]);
}

#[test]
fn session_tombstone_cascades_messages_and_sources() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let s = session(a, "doomed");
    a.add_user_message(&s, "one").unwrap();
    add_session_sources(a, &s, &["https://x".to_string()]);
    pair.exchange_ab();
    assert_eq!(dump(a), dump(b));
    a.delete_session(&s).unwrap();
    pair.exchange_ab();
    assert_eq!(dump(a), dump(b));
    let sessions = b.list_sessions(&space(b, "default")).unwrap();
    assert!(sessions.iter().all(|s| s.title != "doomed"));
    let messages: i64 = b
        .conn()
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(messages, 0);
    let sources: i64 = b
        .conn()
        .query_row("SELECT COUNT(*) FROM session_sources", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sources, 0);
}

#[test]
fn space_tombstone_removes_row_and_dir() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let sp = a.create_space("work").unwrap();
    pair.space_a().ensure_space_dir("work").unwrap();
    pair.space_b().ensure_space_dir("work").unwrap();
    write_blob(&pair.space_b(), "work", "f.txt", b"content");
    pair.exchange_ab();
    assert!(pair.space_b().files_dir("work").join("f.txt").exists());
    a.delete_space(&sp.id).unwrap();
    pair.space_a().remove_space_dir("work").unwrap();
    pair.exchange_ab();
    assert!(b.list_spaces().unwrap().iter().all(|s| s.name != "work"));
    assert!(!pair.space_b().space_dir("work").exists());
}

#[test]
fn file_tombstone_removes_row_and_blob() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let sid = space(a, "default");
    write_blob(&pair.space_a(), "default", "f.txt", b"content");
    let hash = sha256_hex(b"content");
    let fid = a.upsert_file(&sid, "f.txt", &hash, 7, "ok").unwrap();
    pair.exchange_ab();
    assert!(pair.space_b().files_dir("default").join("f.txt").exists());
    a.delete_file(&fid).unwrap();
    std::fs::remove_file(pair.space_a().files_dir("default").join("f.txt")).unwrap();
    pair.exchange_ab();
    assert_eq!(dump(a), dump(b));
    let files = b.list_files(&sid).unwrap();
    assert!(files.is_empty());
    assert!(!pair.space_b().files_dir("default").join("f.txt").exists());
}

#[test]
fn default_space_tombstone_is_ignored() {
    let pair = Pair::new();
    let a = &pair.a;
    let mut evil = build_changeset(a, None, "a").unwrap();
    evil.tombstones = vec![Tombstone {
        origin_id: 1,
        table_name: "spaces".to_string(),
        row_id: DEFAULT_SPACE.to_string(),
        deleted_at: "2026-01-01T00:00:00Z".to_string(),
    }];
    let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &evil, None).unwrap();
    assert_eq!(summary.tombstones_applied, 0);
    assert!(pair.b.default_space_id().is_ok());
}

#[test]
fn newer_offline_write_survives_an_older_tombstone() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let session_id = session(a, "before delete");
    pair.exchange_ab();

    a.delete_session(&session_id).unwrap();
    b.conn()
        .execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            ("offline revival", "2099-01-01T00:00:00Z", &session_id),
        )
        .unwrap();

    let delete = build_changeset(a, None, "a").unwrap();
    let (_, cursors) = apply_changeset(b, &pair.space_b(), &delete, None).unwrap();
    assert_eq!(
        b.get_session(&session_id).unwrap().unwrap().title,
        "offline revival"
    );
    let mut reply = build_changeset(b, Some(&delete.device_id), "b").unwrap();
    reply.ack = Some(cursors);
    apply_changeset(a, &pair.space_a(), &reply, None).unwrap();
    assert_eq!(
        a.get_session(&session_id).unwrap().unwrap().title,
        "offline revival"
    );
}

#[test]
fn tombstone_cursor_advances_and_no_resends() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let s = session(a, "doomed");
    pair.exchange_ab();
    a.delete_session(&s).unwrap();
    pair.exchange_ab();
    let cs = build_changeset(a, Some(&b.device_id().unwrap()), "a").unwrap();
    assert!(cs.tombstones.is_empty());
    let state = db_state(b);
    assert!(state.iter().any(|st| {
        st.peer_id == a.device_id().unwrap()
            && st.table_name == "sync_tombstones"
            && st.pull_cursor.is_some()
    }));
}

#[test]
fn swarm_roster_follows_winning_session() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let s = session(a, "roundtable");
    let p1 = crate::db::Persona {
        name: "p1".to_string(),
        model: "m".to_string(),
        blurb: "b".to_string(),
    };
    a.save_swarm_personas(&s, &[p1]).unwrap();
    let cs = build_changeset(a, None, "a").unwrap();
    let _ = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
    let p2 = crate::db::Persona {
        name: "p2".to_string(),
        model: "m".to_string(),
        blurb: "b".to_string(),
    };
    let p3 = crate::db::Persona {
        name: "p3".to_string(),
        model: "m".to_string(),
        blurb: "b".to_string(),
    };
    b.save_swarm_personas(&s, &[p2, p3]).unwrap();
    let t2 = session_updated(b, &s);
    let p1b = crate::db::Persona {
        name: "p1".to_string(),
        model: "m".to_string(),
        blurb: "b".to_string(),
    };
    a.save_swarm_personas(&s, &[p1b]).unwrap();
    set_session_updated(a, &s, &minus_one_second(&t2));
    let cs_a = build_changeset(a, None, "a").unwrap();
    let (_, cursors) = apply_changeset(b, &pair.space_b(), &cs_a, None).unwrap();
    let mut reply = build_changeset(b, Some(&cs_a.device_id), "b").unwrap();
    reply.ack = Some(cursors);
    let _ = apply_changeset(a, &pair.space_a(), &reply, None).unwrap();
    let names = |db: &Db| -> Vec<String> {
        db.list_swarm_personas(&s)
            .unwrap()
            .iter()
            .map(|p| p.name.clone())
            .collect()
    };
    assert_eq!(names(a), vec!["p2", "p3"]);
    assert_eq!(names(b), vec!["p2", "p3"]);
}

#[test]
fn file_blobs_transfer_keep_and_report_missing() {
    let pair = Pair::new();
    let a = &pair.a;
    let sid = space(a, "default");
    write_blob(&pair.space_a(), "default", "notes.txt", b"hello sync");
    let hash = sha256_hex(b"hello sync");
    a.upsert_file(&sid, "notes.txt", &hash, 11, "ok").unwrap();

    let cs = build_changeset(a, None, "a").unwrap();
    let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &cs, None).unwrap();
    assert_eq!(summary.files_missing.len(), 1);
    assert_eq!(summary.files_missing[0].name, "notes.txt");
    assert!(
        !pair
            .space_b()
            .files_dir("default")
            .join("notes.txt")
            .exists()
    );

    export_blobs(a, &pair.space_a(), &cs, &pair.dir).unwrap();
    let (summary2, _) = apply_changeset(&pair.b, &pair.space_b(), &cs, Some(&pair.dir)).unwrap();
    assert_eq!(summary2.files_pulled, 1);
    assert!(summary2.files_missing.is_empty());
    assert_eq!(
        std::fs::read(pair.space_b().files_dir("default").join("notes.txt")).unwrap(),
        b"hello sync"
    );

    write_blob(&pair.space_a(), "default", "notes.txt", b"hello sync v2");
    let hash2 = sha256_hex(b"hello sync v2");
    a.upsert_file(&sid, "notes.txt", &hash2, 13, "ok").unwrap();
    let cs2 = build_changeset(a, None, "a").unwrap();
    export_blobs(a, &pair.space_a(), &cs2, &pair.dir).unwrap();
    let (summary3, _) = apply_changeset(&pair.b, &pair.space_b(), &cs2, Some(&pair.dir)).unwrap();
    assert_eq!(summary3.files_pulled, 1);
    assert!(summary3.files_missing.is_empty());
    assert_eq!(
        std::fs::read(pair.space_b().files_dir("default").join("notes.txt")).unwrap(),
        b"hello sync v2"
    );
}

#[test]
fn stale_blob_in_channel_is_never_applied() {
    let pair = Pair::new();
    let a = &pair.a;
    let sid = space(a, "default");
    write_blob(&pair.space_a(), "default", "f.txt", b"real");
    let hash = sha256_hex(b"real");
    a.upsert_file(&sid, "f.txt", &hash, 4, "ok").unwrap();
    let cs = build_changeset(a, None, "a").unwrap();
    let blob_dir = pair.dir.join("blobs").join(&sid);
    std::fs::create_dir_all(&blob_dir).unwrap();
    std::fs::write(blob_dir.join("f.txt"), b"stale").unwrap();
    let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &cs, Some(&pair.dir)).unwrap();
    assert_eq!(summary.files_pulled, 0);
    assert_eq!(summary.files_missing.len(), 1);
    assert!(!pair.space_b().files_dir("default").join("f.txt").exists());
}

#[test]
fn content_addressed_blob_survives_mailbox_name_collision() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let sid = space(a, "default");
    write_blob(&pair.space_a(), "default", "same.txt", b"from-a");
    write_blob(&pair.space_b(), "default", "same.txt", b"from-b");
    let hash_a = sha256_hex(b"from-a");
    let hash_b = sha256_hex(b"from-b");
    a.upsert_file(&sid, "same.txt", &hash_a, 6, "ok").unwrap();
    b.upsert_file(&sid, "same.txt", &hash_b, 6, "ok").unwrap();
    set_updated(
        a,
        "files",
        &a.list_files(&sid).unwrap()[0].id,
        "2099-01-01T00:00:00Z",
    );
    set_updated(
        b,
        "files",
        &b.list_files(&sid).unwrap()[0].id,
        "2020-01-01T00:00:00Z",
    );

    let changeset_a = build_changeset(a, None, "a").unwrap();
    let changeset_b = build_changeset(b, None, "b").unwrap();
    export_blobs(a, &pair.space_a(), &changeset_a, &pair.dir).unwrap();
    export_blobs(b, &pair.space_b(), &changeset_b, &pair.dir).unwrap();
    let (summary, _) = apply_changeset(b, &pair.space_b(), &changeset_a, Some(&pair.dir)).unwrap();
    assert_eq!(summary.files_pulled, 1);
    assert_eq!(
        std::fs::read(pair.space_b().files_dir("default").join("same.txt")).unwrap(),
        b"from-a"
    );
    assert_eq!(hash_a.len(), 64);
    assert_ne!(hash_a, hash_b);
}

#[test]
fn unsafe_blob_names_cannot_escape() {
    let pair = Pair::new();
    let b = &pair.b;
    let mut cs = build_changeset(b, None, "b").unwrap();
    cs.rows.push(RowChange {
        table: "files".to_string(),
        row: serde_json::json!({
            "id": "f1", "space_id": "sp", "name": "../escape.txt",
            "hash": "x", "size": 1,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
        }),
    });
    cs.files.push(FileChange {
        space_id: "sp".to_string(),
        name: "../escape.txt".to_string(),
        hash: "x".to_string(),
        size: 1,
    });
    let (summary, _) = apply_changeset(&pair.a, &pair.space_a(), &cs, Some(&pair.dir)).unwrap();
    assert!(summary.warnings.iter().any(|w| w.contains("unsafe")));
    assert!(!pair.dir.join("blobs").join("sp").join("..").exists());
    assert!(!pair.a_root.join("escape.txt").exists());
}

#[test]
fn default_space_is_the_same_row_on_both_devices() {
    let pair = Pair::new();
    assert_eq!(pair.a.default_space_id().unwrap(), DEFAULT_SPACE);
    assert_eq!(pair.b.default_space_id().unwrap(), DEFAULT_SPACE);
    pair.exchange_ab();
    let spaces = pair.b.list_spaces().unwrap();
    assert_eq!(spaces.len(), 1, "no name collision for the default space");
}

#[test]
fn space_name_collision_renames_loser_deterministically() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let wa = a.create_space("work").unwrap();
    let wb = b.create_space("work").unwrap();
    set_updated(a, "spaces", &wa.id, "2026-02-01T00:00:00Z");
    set_updated(b, "spaces", &wb.id, "2026-01-01T00:00:00Z");
    pair.exchange();
    let names_a: Vec<String> = a
        .list_spaces()
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let names_b: Vec<String> = b
        .list_spaces()
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let expected = format!("work-{}", short_id(&wb.id));
    assert!(names_a.contains(&"work".to_string()));
    assert!(names_a.contains(&expected));
    assert_eq!(names_a, names_b);
    assert_eq!(dump(a), dump(b));
    pair.exchange();
    let names_a2: Vec<String> = a
        .list_spaces()
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(names_a2, names_a);
}

#[test]
fn space_rename_moves_the_dir() {
    let pair = Pair::new();
    let a = &pair.a;
    let sp = a.create_space("work").unwrap();
    pair.space_a().ensure_space_dir("work").unwrap();
    pair.space_b().ensure_space_dir("work").unwrap();
    write_blob(&pair.space_b(), "work", "f.txt", b"content");
    pair.exchange_ab();
    assert!(pair.space_b().files_dir("work").join("f.txt").exists());
    a.rename_space(&sp.id, "work-2").unwrap();
    pair.space_a().rename_space_dir("work", "work-2").unwrap();
    pair.exchange_ab();
    assert!(
        pair.b
            .list_spaces()
            .unwrap()
            .iter()
            .any(|s| s.name == "work-2")
    );
    assert!(pair.space_b().files_dir("work-2").join("f.txt").exists());
    assert!(!pair.space_b().space_dir("work").exists());
}

#[test]
fn bundle_roundtrip_carries_changeset_and_blobs() {
    let pair = Pair::new();
    let a = &pair.a;
    let sid = space(a, "default");
    write_blob(&pair.space_a(), "default", "f.txt", b"payload");
    let hash = sha256_hex(b"payload");
    a.upsert_file(&sid, "f.txt", &hash, 7, "ok").unwrap();
    let cs = build_changeset(a, None, "a").unwrap();
    let dest = pair.dir.join("out.bundle");
    let blobs = write_bundle(
        a,
        &pair.space_a(),
        &cs,
        std::fs::File::create(&dest).unwrap(),
    )
    .unwrap();
    assert_eq!(blobs, 1);
    let unpacked = pair.dir.join("unpacked");
    let back = unpack_bundle(&dest, &unpacked).unwrap();
    assert_eq!(back.device_id, cs.device_id);
    assert_eq!(back.rows.len(), cs.rows.len());
    assert_eq!(
        std::fs::read(
            unpacked
                .join("blobs")
                .join("by-hash")
                .join(&cs.files[0].hash)
        )
        .unwrap(),
        b"payload"
    );
}

#[test]
fn bundle_rejects_escaping_paths() {
    let pair = Pair::new();
    let dest = pair.dir.join("evil.bundle");
    let file = std::fs::File::create(&dest).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("changeset.json", opts).unwrap();
    serde_json::to_writer(
        &mut zip,
        &Changeset {
            schema: crate::db::migrations::durable_schema_version(),
            device_id: "x".to_string(),
            device_name: "x".to_string(),
            ack: None,
            rows: Vec::new(),
            tombstones: Vec::new(),
            files: Vec::new(),
            generated_at: "t".to_string(),
        },
    )
    .unwrap();
    zip.start_file("blobs/../escape.txt", opts).unwrap();
    zip.write_all(b"nope").unwrap();
    zip.finish().unwrap();
    let unpacked = pair.dir.join("evil-out");
    assert!(unpack_bundle(&dest, &unpacked).is_err());
    assert!(!pair.dir.join("escape.txt").exists());
}

#[test]
fn ack_built_from_pull_cursors_advances_peer_push() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let _s = session(a, "s");
    let cs = build_changeset(a, None, "a").unwrap();
    let (_, cursors) = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
    let mut reply = build_changeset(b, Some(&cs.device_id), "b").unwrap();
    reply.ack = Some(cursors);
    let _ = apply_changeset(a, &pair.space_a(), &reply, None).unwrap();
    let bid = b.device_id().unwrap();
    let mut next = build_changeset(a, Some(&bid), "a").unwrap();
    next.ack = Some(build_ack(a, &bid).unwrap());
    let (summary, _) = apply_changeset(b, &pair.space_b(), &next, None).unwrap();
    assert!(summary.acks_applied >= 1);
    let after = build_changeset(b, Some(&a.device_id().unwrap()), "b").unwrap();
    assert!(after.rows.is_empty());
}

#[test]
fn acks_are_only_honored_for_this_device() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let _s = session(a, "s");
    pair.exchange_ab();
    let state = db_state(a);
    assert!(state.iter().any(|st| {
        st.peer_id == b.device_id().unwrap()
            && st.table_name == "sessions"
            && st.push_cursor.is_some()
    }));
    let forged = Changeset {
        schema: crate::db::migrations::durable_schema_version(),
        device_id: b.device_id().unwrap(),
        device_name: "b".to_string(),
        ack: Some(vec![PeerCursor {
            peer_id: "someone-else".to_string(),
            table_name: "sessions".to_string(),
            cursor: "2099-01-01T00:00:00Z".to_string(),
        }]),
        rows: Vec::new(),
        tombstones: Vec::new(),
        files: Vec::new(),
        generated_at: "t".to_string(),
    };
    let (summary, _) = apply_changeset(a, &pair.space_a(), &forged, None).unwrap();
    assert_eq!(summary.acks_applied, 0);
}

#[test]
fn malformed_ack_does_not_poison_peer_cursor() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let forged = Changeset {
        schema: crate::db::migrations::durable_schema_version(),
        device_id: b.device_id().unwrap(),
        device_name: "b".to_string(),
        ack: Some(vec![PeerCursor {
            peer_id: a.device_id().unwrap(),
            table_name: "sessions".to_string(),
            cursor: "[\"missing-key-tuple\"]".to_string(),
        }]),
        rows: Vec::new(),
        tombstones: Vec::new(),
        files: Vec::new(),
        generated_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let (summary, _) = apply_changeset(a, &pair.space_a(), &forged, None).unwrap();
    assert_eq!(summary.acks_applied, 0);
    assert!(
        summary
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid cursor"))
    );
    assert!(
        !db_state(a).iter().any(|state| {
            state.peer_id == b.device_id().unwrap() && state.push_cursor.is_some()
        })
    );
}

#[test]
fn push_cursor_advances_only_on_ack() {
    let pair = Pair::new();
    let a = &pair.a;
    let b = &pair.b;
    let _s = session(a, "s");
    let bid = b.device_id().unwrap();
    let _ = build_changeset(a, Some(&bid), "a").unwrap();
    let after_export = db_state(a);
    assert!(after_export.iter().all(|st| st.peer_id != bid));
    let cs = build_changeset(a, None, "a").unwrap();
    let (_, cursors) = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
    let mut reply = build_changeset(b, Some(&cs.device_id), "b").unwrap();
    reply.ack = Some(cursors);
    let _ = apply_changeset(a, &pair.space_a(), &reply, None).unwrap();
    let state = db_state(a);
    assert!(state.iter().any(|st| {
        st.peer_id == bid && st.table_name == "sessions" && st.push_cursor.is_some()
    }));
}

#[test]
fn different_or_missing_schema_is_refused_without_changes() {
    let ours = crate::db::migrations::durable_schema_version();
    for schema in [Some(ours - 1), Some(ours + 1), None] {
        let pair = Pair::new();
        let local = session(&pair.b, "keep local");
        let remote = session(&pair.a, "remote");
        pair.a.add_user_message(&remote, "remote message").unwrap();
        crate::memory::add(
            pair.a.conn(),
            DEFAULT_SPACE,
            "Tools",
            "uses Rust",
            None,
            None,
        )
        .unwrap();
        write_blob(&pair.space_a(), "default", "remote.txt", b"remote");
        pair.a
            .upsert_file(DEFAULT_SPACE, "remote.txt", &sha256_hex(b"remote"), 6, "ok")
            .unwrap();
        let mut changeset = build_changeset(&pair.a, None, "laptop").unwrap();
        assert_eq!(changeset.schema, ours);
        changeset.tombstones.push(Tombstone {
            origin_id: 1,
            table_name: "sessions".to_string(),
            row_id: local,
            deleted_at: "2099-01-01T00:00:00Z".to_string(),
        });
        export_blobs(&pair.a, &pair.space_a(), &changeset, &pair.dir).unwrap();
        let mut json = serde_json::to_value(&changeset).unwrap();
        if let Some(schema) = schema {
            json["schema"] = schema.into();
        } else {
            json.as_object_mut().unwrap().remove("schema");
        }
        let changeset: Changeset = serde_json::from_value(json).unwrap();
        let theirs = schema.unwrap_or(0);
        assert_eq!(changeset.schema, theirs);
        let row_counts = || {
            let mut tables = pair
                .b
                .conn()
                .prepare("SELECT name FROM main.sqlite_schema WHERE type = 'table' ORDER BY name")
                .unwrap();
            tables
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(|name| {
                    let name = name.unwrap();
                    let count = pair
                        .b
                        .conn()
                        .query_row(
                            &format!("SELECT count(*) FROM main.\"{name}\""),
                            [],
                            |row| row.get::<_, i64>(0),
                        )
                        .unwrap();
                    (name, count)
                })
                .collect::<Vec<_>>()
        };
        let counts_before = row_counts();
        let rows_before = dump(&pair.b);
        let error =
            apply_changeset(&pair.b, &pair.space_b(), &changeset, Some(&pair.dir)).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "laptop runs a different nexus version (schema {theirs}, this device {ours}) — update nexus on both devices, then sync again"
            )
        );
        assert_eq!(row_counts(), counts_before);
        assert_eq!(dump(&pair.b), rows_before);
        assert!(!pair.b_root.exists());
    }
}

#[test]
fn invalid_cursors_resend_rows_and_are_not_acked() {
    let pair = Pair::new();
    let id = session(&pair.a, "resend");
    for cursor in [
        "2099-01-01T00:00:00Z",
        "[\"2099-01-01T00:00:00Z\",[],\"extra\"]",
        "not a cursor",
    ] {
        pair.a
            .set_sync_state("peer", "sessions", Some(cursor), Some(cursor))
            .unwrap();
        assert!(build_ack(&pair.a, "peer").unwrap().is_empty());
        let changeset = build_changeset(&pair.a, Some("peer"), "a").unwrap();
        assert!(changeset.rows.iter().any(|row| row.row["id"] == id));
        let mut reply = build_changeset(&pair.b, None, "b").unwrap();
        reply.ack = Some(vec![PeerCursor {
            peer_id: pair.a.device_id().unwrap(),
            table_name: "sessions".to_string(),
            cursor: cursor.to_string(),
        }]);
        let (summary, _) = apply_changeset(&pair.a, &pair.space_a(), &reply, None).unwrap();
        assert_eq!(summary.acks_applied, 0);
        assert!(
            summary
                .warnings
                .iter()
                .any(|w| w.contains("invalid cursor"))
        );
    }
}

#[test]
fn exports_and_bundles_contain_only_content_addressed_blobs() {
    let pair = Pair::new();
    let hash = sha256_hex(b"payload");
    for name in ["first.txt", "second.txt"] {
        write_blob(&pair.space_a(), "default", name, b"payload");
        pair.a
            .upsert_file(DEFAULT_SPACE, name, &hash, 7, "ok")
            .unwrap();
    }
    let changeset = build_changeset(&pair.a, None, "a").unwrap();
    assert_eq!(
        export_blobs(&pair.a, &pair.space_a(), &changeset, &pair.dir).unwrap(),
        2
    );
    let directories = std::fs::read_dir(pair.dir.join("blobs"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(directories, [std::ffi::OsString::from("by-hash")]);
    let bundle = pair.dir.join("by-hash.zip");
    assert_eq!(
        write_bundle(
            &pair.a,
            &pair.space_a(),
            &changeset,
            std::fs::File::create(&bundle).unwrap(),
        )
        .unwrap(),
        2
    );
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&bundle).unwrap()).unwrap();
    assert_eq!(archive.len(), 2);
    assert!(archive.by_name("changeset.json").is_ok());
    assert!(archive.by_name(&format!("blobs/by-hash/{hash}")).is_ok());
    let unpacked = pair.dir.join("unpacked-by-hash");
    let back = unpack_bundle(&bundle, &unpacked).unwrap();
    assert_eq!(back, changeset);
    let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &back, Some(&unpacked)).unwrap();
    assert_eq!(summary.files_pulled, 2);
    for name in ["first.txt", "second.txt"] {
        assert_eq!(
            std::fs::read(pair.space_b().files_dir("default").join(name)).unwrap(),
            b"payload"
        );
    }
}
