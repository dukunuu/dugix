use super::*;

fn context<'a>(schema: &'a str, file: &'static str) -> Ctx<'a> {
    Ctx {
        schema,
        file,
        root: None,
        notes: Vec::new(),
    }
}

fn has_table(conn: &Connection, schema: &str, table: &str) -> bool {
    conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {schema}.sqlite_schema WHERE name = ?1 AND type = 'table')"
        ),
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn step_six_moves_legacy_watches_once_without_resurrecting_existing_jobs() {
    let conn = Connection::open_in_memory().unwrap();
    let mut ctx = context("main", "nexus.db");
    run(&conn, &DURABLE[..2], &mut ctx).unwrap();
    assert_eq!(version(&conn, "main").unwrap(), 5);
    conn.execute_batch(
        "INSERT INTO device_meta VALUES ('owner', 'created');
         INSERT INTO watches VALUES ('watch', 'space', 'topic', 12, 'session', 'last', 'updated');
         INSERT INTO watches VALUES ('deleted', 'space', 'old', 24, 'old-session', NULL, 'old');
         INSERT INTO jobs (id, space_id, session_id, device_id, name, prompt, root, mode,
          trigger_kind, interval_minutes, max_iters, max_cost_usd, max_wall_minutes, notify,
          enabled, updated_at, kind, topic)
         VALUES ('deleted', 'space', 'new-session', 'owner', 'old', 'old', '', 'agent',
          'manual', 1440, 5, 1.0, 30, 'on_change', -1, 'new', 'research', 'old');",
    )
    .unwrap();
    assert_eq!(run(&conn, &DURABLE[..3], &mut ctx).unwrap(), 1);
    assert_eq!(version(&conn, "main").unwrap(), 6);
    assert!(!has_table(&conn, "main", "watches"));
    let migrated: (String, String, String, i64, String, String, String, String) = conn
        .query_row(
            "SELECT id, kind, topic, interval_minutes, session_id, last_run_at, device_id,
             trigger_kind FROM jobs WHERE id = 'watch'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        migrated,
        (
            "watch".into(),
            "research".into(),
            "topic".into(),
            720,
            "session".into(),
            "last".into(),
            "owner".into(),
            "interval".into(),
        )
    );
    let preserved: (i64, String, String) = conn
        .query_row(
            "SELECT enabled, session_id, trigger_kind FROM jobs WHERE id = 'deleted'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(preserved, (-1, "new-session".into(), "manual".into()));
    assert_eq!(run(&conn, &DURABLE[..3], &mut ctx).unwrap(), 0);
}

#[test]
fn cache_step_two_drops_the_watch_mapping_attached_or_standalone() {
    for schema in ["main", "cache"] {
        let conn = Connection::open_in_memory().unwrap();
        if schema == "cache" {
            conn.execute_batch("ATTACH DATABASE ':memory:' AS cache")
                .unwrap();
        }
        let mut ctx = context(schema, "cache.db");
        run(&conn, &CACHE[..1], &mut ctx).unwrap();
        assert!(has_table(&conn, schema, "watch_jobs"));
        conn.execute_batch(&format!(
            "INSERT INTO {schema}.watch_jobs VALUES ('watch', 'job')"
        ))
        .unwrap();
        assert_eq!(run(&conn, CACHE, &mut ctx).unwrap(), 1);
        assert_eq!(version(&conn, schema).unwrap(), 2);
        assert!(!has_table(&conn, schema, "watch_jobs"));
        assert_eq!(run(&conn, CACHE, &mut ctx).unwrap(), 0);
    }
}
