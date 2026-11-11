use super::*;

#[test]
fn harness_summary_groups_ranks_and_filters_the_window() {
    let db = Db::open_in_memory().unwrap();
    let since = "2026-07-07T12:00:00+00:00";
    assert_eq!(
        db.harness_summary(since).unwrap(),
        HarnessSummary::default()
    );
    db.conn
        .execute_batch(
            "INSERT INTO cache.tool_events
             (at, session_id, tool, action, error_kind, ok, latency_ms, result_chars) VALUES
             ('2026-07-07T11:59:59+00:00', 'old', 'scripts', 'edit', 'stale_edit', 0, 1, 1),
             ('2026-07-07T12:00:00+00:00', 'boundary', 'scripts', 'edit', 'stale_edit', 0, 1, 1),
             ('2026-07-13T12:00:00+00:00', 'latest', 'scripts', 'edit', 'stale_edit', 0, 1, 1),
             ('2026-07-12T12:00:00+00:00', 'middle', 'scripts', 'edit', 'stale_edit', 0, 1, 1),
             ('2026-07-14T11:00:00+00:00', 'invalid-latest', 'scripts', 'edit', 'invalid_args', 0, 1, 1),
             ('2026-07-07T12:00:00+00:00', 'invalid-old', 'scripts', 'edit', 'invalid_args', 0, 1, 1),
             ('2026-07-14T10:00:00+00:00', 'run-latest', 'scripts', 'run', 'upstream', 0, 1, 1),
             ('2026-07-07T12:00:00+00:00', 'run-old', 'scripts', 'run', 'upstream', 0, 1, 1),
             ('2026-07-14T12:00:00+00:00', 'edit', 'scripts', 'edit', 'upstream', 0, 1, 1),
             ('2026-07-14T09:00:00+00:00', 'fs', 'fs', 'edit', 'stale_edit', 0, 1, 1),
             ('2026-07-14T13:00:00+00:00', 'ok', 'scripts', 'edit', NULL, 1, 1, 1);",
        )
        .unwrap();
    let summary = db.harness_summary(since).unwrap();
    assert_eq!((summary.calls, summary.failures), (10, 9));
    let modes: Vec<_> = summary
        .rows
        .iter()
        .map(|row| {
            (
                row.tool.as_str(),
                row.action.as_str(),
                row.error_kind.as_str(),
                row.count,
                row.last_session_id.as_str(),
            )
        })
        .collect();
    assert_eq!(
        modes,
        [
            ("scripts", "edit", "stale_edit", 3, "latest"),
            ("scripts", "edit", "invalid_args", 2, "invalid-latest"),
            ("scripts", "run", "upstream", 2, "run-latest"),
            ("scripts", "edit", "upstream", 1, "edit"),
            ("fs", "edit", "stale_edit", 1, "fs"),
        ]
    );
    assert_eq!(summary.rows[0].last_at, "2026-07-13T12:00:00+00:00");
}

#[test]
fn harness_summary_caps_modes_but_not_totals() {
    let db = Db::open_in_memory().unwrap();
    for count in 1..=55 {
        for _ in 0..count {
            db.conn
                .execute(
                    "INSERT INTO cache.tool_events
                     (at, session_id, tool, action, ok, error_kind, latency_ms, result_chars)
                     VALUES ('2026-07-14T12:00:00+00:00', 'session', ?1, '', 0, 'io', 1, 1)",
                    [format!("tool-{count}")],
                )
                .unwrap();
        }
    }
    let summary = db.harness_summary("2026-07-07T12:00:00+00:00").unwrap();
    assert_eq!((summary.calls, summary.failures), (1540, 1540));
    assert_eq!(summary.rows.len(), 50);
    assert_eq!(summary.rows.first().unwrap().count, 55);
    assert_eq!(summary.rows.last().unwrap().count, 6);
}

#[test]
fn cache_migration_adds_workspace_mode_to_legacy_bindings() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("ATTACH DATABASE ':memory:' AS cache; CREATE TABLE cache.session_workspaces (session_id TEXT PRIMARY KEY, root TEXT NOT NULL, updated_at TEXT NOT NULL);
        INSERT INTO cache.session_workspaces VALUES ('old', '/workspace', 'now');").unwrap();
    migrate_cache(&conn, "cache").unwrap();
    migrate_cache(&conn, "cache").unwrap();
    let mode: Option<String> = conn
        .query_row(
            "SELECT mode FROM cache.session_workspaces WHERE session_id = 'old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mode, None);
    let db = Db { conn };
    db.set_session_workspace_mode("old", "auto").unwrap();
    assert_eq!(db.session_workspace_mode("old").as_deref(), Some("auto"));
}

#[test]
fn decision_cost_migration_is_nullable_guarded_and_preserves_existing_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "ATTACH DATABASE ':memory:' AS cache;
         CREATE TABLE cache.decisions (id INTEGER PRIMARY KEY, summary TEXT);
         INSERT INTO cache.decisions VALUES (1, 'legacy decision');",
    )
    .unwrap();
    migrate_cache(&conn, "cache").unwrap();
    migrate_cache(&conn, "cache").unwrap();
    let row: (String, Option<f64>) = conn
        .query_row(
            "SELECT summary, cost FROM cache.decisions WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("legacy decision".into(), None));
    set_decision_cost(&conn, 1, 0.06).unwrap();
    migrate_cache(&conn, "cache").unwrap();
    let cost: Option<f64> = conn
        .query_row("SELECT cost FROM cache.decisions WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(cost, Some(0.06));
    conn.execute(
        "INSERT INTO cache.decisions (id, summary) VALUES (2, 'new decision')",
        [],
    )
    .unwrap();
    let cost: Option<f64> = conn
        .query_row("SELECT cost FROM cache.decisions WHERE id = 2", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(cost, None);
}

#[test]
fn decision_log_caps_scrubbed_unicode_summaries_and_records_answers() {
    let db = Db::open_in_memory().unwrap();
    let req = crate::decider::DecisionRequest {
        tool: "fs".into(),
        action: "write".into(),
        summary: format!("saved-secret-value {}", "é".repeat(2_100)),
        task: String::new(),
        root: "/workspace".into(),
        effect: crate::tools::Effect::default(),
        class: crate::decider::Class::WriteInRoot,
    };
    let answer = crate::decider::Verdicts {
        verdict: crate::decider::Verdict::Allow,
        p_in_scope: None,
        risk: None,
        by: "rules",
    };
    let id = insert_decision(
        &db.conn,
        "session",
        &req,
        &answer,
        &crate::trace::Scrubber::new(vec!["saved-secret-value".into()]),
    )
    .unwrap();
    answer_decision(&db.conn, id, "allow_session").unwrap();
    let rows = db.decisions("session").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].summary.chars().count(), 2_000);
    assert!(rows[0].summary.starts_with("[REDACTED]"));
    assert_eq!(rows[0].verdict, "allow");
    assert_eq!(rows[0].by, "rules");
    assert_eq!(rows[0].user_answer.as_deref(), Some("allow_session"));
}

#[test]
fn session_workspace_binding_is_cache_only_and_round_trips() {
    let db = Db::open_in_memory().unwrap();
    let root = std::env::temp_dir().join("workspace");
    let replacement = std::env::temp_dir().join("another workspace");
    assert_eq!(db.session_workspace("session"), None);
    db.set_session_workspace("session", Some(&root)).unwrap();
    assert_eq!(db.session_workspace("session"), Some(root));
    assert_eq!(db.session_workspace("other"), None);
    db.set_session_workspace("session", Some(&replacement))
        .unwrap();
    assert_eq!(db.session_workspace("session"), Some(replacement));
    for (schema, expected) in [("main", 0), ("cache", 1)] {
        let count: i64 = db
            .conn()
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {schema}.sqlite_schema
                     WHERE type = 'table' AND name = 'session_workspaces'"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, expected);
    }
    let updated_at: String = db
        .conn()
        .query_row(
            "SELECT updated_at FROM cache.session_workspaces WHERE session_id = 'session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(&updated_at).is_ok());
    db.set_session_workspace("session", None).unwrap();
    assert_eq!(db.session_workspace("session"), None);
}

#[test]
fn session_workspace_binding_survives_reopening_the_database() {
    let dir = std::env::temp_dir().join(format!("nexus-workspace-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("nexus.db");
    let root = dir.join("workspace");
    {
        let db = Db::open(&path).unwrap();
        db.set_session_workspace("session", Some(&root)).unwrap();
    }
    {
        let db = Db::open(&path).unwrap();
        assert_eq!(db.session_workspace("session"), Some(root));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

fn price(prompt: f64, completion: f64) -> ModelPricing {
    ModelPricing {
        prompt,
        completion,
        cache_read: None,
        cache_write: None,
    }
}

fn cache_price(prompt: f64, completion: f64, cache_read: f64, cache_write: f64) -> ModelPricing {
    ModelPricing {
        prompt,
        completion,
        cache_read: Some(cache_read),
        cache_write: Some(cache_write),
    }
}

#[test]
fn is_fresh_true_under_24h_false_over() {
    let now = Utc::now();
    let recent = (now - chrono::Duration::hours(1)).to_rfc3339();
    let stale = (now - chrono::Duration::hours(25)).to_rfc3339();
    assert!(is_fresh(&recent, now));
    assert!(!is_fresh(&stale, now));
    assert!(!is_fresh("not a timestamp", now)); // unparseable = not fresh
}

#[test]
fn usage_log_round_trips_and_aggregates() {
    let mut db = Db::open_in_memory().unwrap();
    db.upsert_model_prices(&[(
        "anthropic/claude-3.5-sonnet".to_string(),
        "OpenRouter".to_string(),
        price(3.0, 15.0),
    )])
    .unwrap();
    assert_eq!(
        db.model_price("anthropic/claude-3.5-sonnet"),
        Some(price(3.0, 15.0))
    );
    // Re-upsert refreshes every rate rather than duplicating.
    db.upsert_model_prices(&[(
        "anthropic/claude-3.5-sonnet".to_string(),
        "OpenRouter".to_string(),
        cache_price(4.0, 16.0, 0.4, 5.0),
    )])
    .unwrap();
    assert_eq!(
        db.model_price("anthropic/claude-3.5-sonnet"),
        Some(cache_price(4.0, 16.0, 0.4, 5.0))
    );
    assert_eq!(db.model_price("unknown/model"), None);

    // 100 prompt @ $4/1M + 10 completion @ $16/1M = $0.0004 + $0.00016.
    db.log_usage(
        "OpenRouter",
        "anthropic/claude-3.5-sonnet",
        100,
        10,
        70,
        20,
        Default::default(),
        Some(0.00056),
        true,
        Some("s1"),
        Some("space-a"),
    )
    .unwrap();
    db.log_usage(
        "Codex",
        "gpt-5.1-codex",
        50,
        5,
        0,
        0,
        Default::default(),
        None,
        false,
        None,
        None,
    )
    .unwrap();

    let totals = db.usage_totals(None).unwrap();
    assert_eq!(totals.requests, 2);
    assert_eq!(totals.prompt_tokens, 150);
    assert_eq!(totals.completion_tokens, 15);
    assert_eq!(totals.cache_read_tokens, 70);
    assert_eq!(totals.cache_creation_tokens, 20);
    assert!((totals.cost - 0.00056).abs() < 1e-9);

    let by_backend = db.usage_by_backend(None).unwrap();
    assert_eq!(by_backend.len(), 2);
    assert_eq!(by_backend[0].backend, "OpenRouter"); // most-used first
    assert_eq!(by_backend[0].requests, 1);

    let by_model = db.usage_by_model(5, None).unwrap();
    assert_eq!(by_model.len(), 2);
    assert!(by_model.iter().any(|m| m.model == "gpt-5.1-codex"));

    let recent = db.usage_recent(10, None).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].model, "gpt-5.1-codex"); // newest first
    assert_eq!(recent[0].cost, None);
    assert_eq!(recent[1].cache_read_tokens, 70);
}

#[test]
fn usage_queries_filter_by_since_window() {
    let db = Db::open_in_memory().unwrap();
    // Rows with explicit timestamps (raw insert: log_usage stamps now).
    let insert = |created: &str| {
        db.raw().execute(
            "INSERT INTO usage_log (sync_id, created_at, session_id, backend, model,
                prompt_tokens, completion_tokens, cache_read_tokens, cache_creation_tokens, cost)
             VALUES (?1, ?2, NULL, 'OpenRouter', 'a/model', 100, 10, 0, 0, 0.001)",
            (uuid::Uuid::new_v4().to_string(), created),
        )
    };
    insert("2026-01-01T00:00:00+00:00").unwrap();
    insert("2026-01-02T00:00:00+00:00").unwrap();
    insert("2026-01-03T00:00:00+00:00").unwrap();

    let since = Some("2026-01-02T00:00:00+00:00");
    let totals = db.usage_totals(since).unwrap();
    assert_eq!(totals.requests, 2);
    assert_eq!(totals.prompt_tokens, 200);
    assert!((totals.cost - 0.002).abs() < 1e-12);
    assert_eq!(db.usage_totals(None).unwrap().requests, 3);
    assert_eq!(db.usage_by_backend(since).unwrap()[0].requests, 2);
    assert_eq!(db.usage_by_model(5, since).unwrap()[0].requests, 2);
    let recent = db.usage_recent(10, since).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].created_at, "2026-01-03T00:00:00+00:00");
    // Boundary is inclusive: the exact-cutoff row is included.
    assert!(
        recent
            .iter()
            .any(|r| r.created_at == "2026-01-02T00:00:00+00:00")
    );
}

#[test]
fn usage_range_cycles_and_persists() {
    use crate::db::UsageRange;
    assert_eq!(UsageRange::Day.next(), UsageRange::Week);
    assert_eq!(UsageRange::All.next(), UsageRange::Day);
    assert_eq!(UsageRange::Day.prev(), UsageRange::All);
    assert_eq!(UsageRange::from_key("month"), UsageRange::Month);
    assert_eq!(UsageRange::from_key("bogus"), UsageRange::All);
    assert_eq!(UsageRange::Week.key(), "week");
    assert_eq!(UsageRange::Day.title(), "last 24 hours");
    assert!(UsageRange::All.since().is_none());
    assert!(UsageRange::Day.since().is_some());
}

#[test]
fn backfill_usage_costs_recomputes_history_from_catalog() {
    let mut db = Db::open_in_memory().unwrap();
    db.upsert_model_prices(&[(
        "anthropic/claude-3.5-sonnet".to_string(),
        "OpenRouter".to_string(),
        cache_price(3.0, 15.0, 0.3, 3.75),
    )])
    .unwrap();
    // Rows logged before pricing existed: one priced model (NULL cost),
    // one model with no catalog entry.
    db.log_usage(
        "OpenRouter",
        "anthropic/claude-3.5-sonnet",
        100,
        10,
        70,
        20,
        Default::default(),
        None,
        false,
        None,
        None,
    )
    .unwrap();
    db.log_usage(
        "Codex",
        "gpt-5.1-codex",
        50,
        5,
        0,
        0,
        Default::default(),
        None,
        false,
        None,
        None,
    )
    .unwrap();

    let visited = db.backfill_usage_costs().unwrap();
    assert_eq!(visited, 2);
    // 10 ordinary @ $3/M + 70 reads @ $0.30/M + 20 writes @ $3.75/M,
    // plus 10 completion @ $15/M = $0.000276.
    let totals = db.usage_totals(None).unwrap();
    assert!((totals.cost - 0.000_276).abs() < 1e-12);
    let recent = db.usage_recent(10, None).unwrap();
    assert!((recent[1].cost.unwrap() - 0.000_276).abs() < 1e-12); // priced row filled in
    assert_eq!(recent[0].cost, None); // unknown price stays unknown

    // Idempotent: a second pass leaves the values untouched.
    db.backfill_usage_costs().unwrap();
    assert!((db.usage_totals(None).unwrap().cost - 0.000_276).abs() < 1e-12);
}

#[test]
fn backfill_preserves_provider_reported_cost() {
    let mut db = Db::open_in_memory().unwrap();
    db.upsert_model_prices(&[(
        "anthropic/claude-3.5-sonnet".to_string(),
        "OpenRouter".to_string(),
        cache_price(3.0, 15.0, 0.3, 3.75),
    )])
    .unwrap();
    db.log_usage(
        "OpenRouter",
        "anthropic/claude-3.5-sonnet",
        100,
        10,
        70,
        20,
        Default::default(),
        Some(0.000_321),
        true,
        None,
        None,
    )
    .unwrap();

    db.backfill_usage_costs().unwrap();

    assert_eq!(db.usage_recent(1, None).unwrap()[0].cost, Some(0.000_321));
}

#[test]
fn backfill_usage_costs_heals_per_token_catalog() {
    // A legacy catalog holding the endpoint's raw per-token values
    // (deepseek-v4-flash at $0.08/M stores 8e-08) must be scaled to the
    // per-1M convention before costs are computed against it.
    let mut db = Db::open_in_memory().unwrap();
    db.upsert_model_prices(&[(
        "deepseek/deepseek-v4-flash-0731".to_string(),
        "OpenRouter".to_string(),
        price(8e-08, 1.8e-07),
    )])
    .unwrap();
    db.log_usage(
        "OpenRouter",
        "deepseek/deepseek-v4-flash-0731",
        122_221,
        672,
        118_784,
        0,
        Default::default(),
        Some(9.89864e-09), // the old, 1e6×-too-small value
        false,
        None,
        None,
    )
    .unwrap();

    db.backfill_usage_costs().unwrap();

    assert_eq!(
        db.model_price("deepseek/deepseek-v4-flash-0731"),
        Some(price(0.08, 0.18))
    );
    // 122221/1e6 × 0.08 + 672/1e6 × 0.18 ≈ $0.00989.
    let recent = db.usage_recent(10, None).unwrap();
    let cost = recent[0].cost.unwrap();
    assert!((cost - 0.009_898_6).abs() < 1e-6, "cost was {cost}");
    assert!(cost > 0.009, "cost was {cost}");
}

#[test]
fn request_cost_prices_tokens_against_catalog() {
    let mut db = Db::open_in_memory().unwrap();
    assert_eq!(db.request_cost("unknown/model", 100, 10, 0, 0), None);
    db.upsert_model_prices(&[(
        "anthropic/claude-3.5-sonnet".to_string(),
        "OpenRouter".to_string(),
        cache_price(3.0, 15.0, 0.3, 3.75),
    )])
    .unwrap();
    let cost = db
        .request_cost("anthropic/claude-3.5-sonnet", 100, 10, 70, 20)
        .unwrap();
    assert!((cost - 0.000_276).abs() < 1e-12);
}

#[test]
fn model_price_cross_references_openrouter_catalog_twins() {
    let mut db = Db::open_in_memory().unwrap();
    db.upsert_model_prices(&[
        (
            "deepseek/deepseek-v4-flash".to_string(),
            "OpenRouter".to_string(),
            cache_price(0.08, 0.18, 0.016, 0.08),
        ),
        (
            "openai/gpt-5".to_string(),
            "OpenRouter".to_string(),
            cache_price(1.25, 10.0, 0.125, 1.25),
        ),
    ])
    .unwrap();
    // Exact ids hit directly; other backends' prefixed/bare ids resolve
    // through the vendor/name twin.
    assert_eq!(
        db.model_price("deepseek/deepseek-v4-flash"),
        Some(cache_price(0.08, 0.18, 0.016, 0.08))
    );
    assert_eq!(
        db.model_price("go:deepseek-v4-flash"),
        Some(cache_price(0.08, 0.18, 0.016, 0.08))
    );
    assert_eq!(
        db.model_price("deepseek-v4-flash"),
        Some(cache_price(0.08, 0.18, 0.016, 0.08))
    );
    assert_eq!(
        db.model_price("openai:gpt-5"),
        Some(cache_price(1.25, 10.0, 0.125, 1.25))
    );
    assert_eq!(
        db.model_price("codex:gpt-5"),
        Some(cache_price(1.25, 10.0, 0.125, 1.25))
    );
    // No twin anywhere: unknown.
    assert_eq!(db.model_price("no-such-model-anywhere"), None);
    // The price flows into per-request costs for the other backend.
    let cost = db
        .request_cost("go:deepseek-v4-flash", 100, 10, 70, 0)
        .unwrap();
    assert!((cost - 0.000_005_32).abs() < 1e-15);
}

#[test]
fn price_name_strips_backend_prefixes_and_vendors() {
    assert_eq!(price_name("go:deepseek-v4-flash"), "deepseek-v4-flash");
    assert_eq!(price_name("openai:gpt-5"), "gpt-5");
    assert_eq!(price_name("codex:gpt-5.1-codex"), "gpt-5.1-codex");
    assert_eq!(price_name("opencode:qwen3.6-plus"), "qwen3.6-plus");
    assert_eq!(
        price_name("deepseek/deepseek-v4-flash"),
        "deepseek-v4-flash"
    );
    assert_eq!(price_name("gpt-5"), "gpt-5");
}

#[test]
fn backfill_prices_non_openrouter_models_via_catalog_twins() {
    let mut db = Db::open_in_memory().unwrap();
    db.upsert_model_prices(&[(
        "deepseek/deepseek-v4-flash".to_string(),
        "OpenRouter".to_string(),
        cache_price(0.08, 0.18, 0.016, 0.08),
    )])
    .unwrap();
    // OpenCode Go rows logged with no cost — the flat-fee backend has
    // no pricing of its own, so the twin's list price is the estimate.
    db.log_usage(
        "OpenCode Go",
        "go:deepseek-v4-flash",
        100,
        10,
        0,
        0,
        Default::default(),
        None,
        false,
        None,
        None,
    )
    .unwrap();

    db.backfill_usage_costs().unwrap();

    let recent = db.usage_recent(10, None).unwrap();
    let cost = recent[0].cost.unwrap();
    assert!((cost - 0.000_009_8).abs() < 1e-15, "cost was {cost}");
    assert!((db.usage_totals(None).unwrap().cost - 0.000_009_8).abs() < 1e-15);
}

#[test]
fn web_cache_roundtrips_and_updates_on_rewrite() {
    let db = Db::open_in_memory().unwrap();
    assert!(cache_get(db.raw(), "example.com/a").unwrap().is_none());
    cache_put(
        db.raw(),
        "example.com/a",
        "https://example.com/a",
        Some("Title"),
        "body text",
    )
    .unwrap();
    let (title, text, fetched_at) = cache_get(db.raw(), "example.com/a").unwrap().unwrap();
    assert_eq!(title, "Title");
    assert_eq!(text, "body text");
    assert!(!fetched_at.is_empty());

    // Re-fetching overwrites the row, not duplicates it.
    cache_put(
        db.raw(),
        "example.com/a",
        "https://example.com/a",
        None,
        "new body",
    )
    .unwrap();
    let (title, text, _) = cache_get(db.raw(), "example.com/a").unwrap().unwrap();
    assert_eq!(title, "");
    assert_eq!(text, "new body");
}

#[test]
fn swarm_mode_defaults_off_and_toggles() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    assert!(!s.swarm_mode);
    db.set_session_swarm_mode(&s.id, true).unwrap();
    assert!(db.list_sessions(&space).unwrap()[0].swarm_mode);
    assert!(db.get_session(&s.id).unwrap().unwrap().swarm_mode);
}

#[test]
fn swarm_personas_roundtrip_and_replace_all_on_save() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    assert!(db.list_swarm_personas(&s.id).unwrap().is_empty());

    let roster = vec![
        Persona {
            name: "Skeptic".into(),
            model: "a/one".into(),
            blurb: "pokes holes".into(),
        },
        Persona {
            name: "Advocate".into(),
            model: "b/two".into(),
            blurb: "user-first".into(),
        },
    ];
    db.save_swarm_personas(&s.id, &roster).unwrap();
    let loaded = db.list_swarm_personas(&s.id).unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].name, "Skeptic");
    assert_eq!(loaded[1].name, "Advocate");

    // A second save fully replaces the roster, not appends.
    db.save_swarm_personas(&s.id, &roster[..1]).unwrap();
    assert_eq!(db.list_swarm_personas(&s.id).unwrap().len(), 1);
}

#[test]
fn persona_message_tags_role_assistant_with_persona_and_model() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    db.add_persona_message(&s.id, "reply text", "Skeptic", "a/one")
        .unwrap();
    let msgs = db.load_messages(&s.id).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, "assistant");
    assert_eq!(msgs[0].persona.as_deref(), Some("Skeptic"));
    assert_eq!(msgs[0].model.as_deref(), Some("a/one"));

    // An ordinary assistant message has no persona tag.
    db.add_assistant_message(&s.id, "final answer", None, None, None, None, None, None)
        .unwrap();
    let msgs = db.load_messages(&s.id).unwrap();
    assert_eq!(msgs[1].persona, None);
}

#[test]
fn session_sources_link_to_the_web_cache_and_are_keyword_searchable() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    cache_put(
        db.raw(),
        "https://example.com/a",
        "https://example.com/a",
        Some("A"),
        "rust borrow checker deep dive",
    )
    .unwrap();
    cache_put(
        db.raw(),
        "https://example.com/b",
        "https://example.com/b",
        Some("B"),
        "cooking pasta recipes",
    )
    .unwrap();
    db.add_session_sources(
        &s.id,
        &[
            "https://example.com/a".to_string(),
            "https://example.com/b".to_string(),
        ],
    )
    .unwrap();

    let hits = db.search_session_sources(&s.id, "borrow checker").unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].1.contains("borrow checker"));

    assert!(
        db.search_session_sources(&s.id, "quantum")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn set_source_flag_pins_and_discards_then_clears() {
    let db = Db::open_in_memory().unwrap();
    let session_id = "sess-1";
    add_session_sources(&db.conn, session_id, &["https://a.example/x".to_string()]).unwrap();
    db.set_source_flag(session_id, "https://a.example/x", Some("pinned"))
        .unwrap();
    assert_eq!(
        pinned_urls(&db.conn, session_id).unwrap(),
        vec!["https://a.example/x".to_string()]
    );
    assert!(discarded_domains(&db.conn, session_id).unwrap().is_empty());

    db.set_source_flag(session_id, "https://a.example/x", Some("discarded"))
        .unwrap();
    assert!(pinned_urls(&db.conn, session_id).unwrap().is_empty());
    assert_eq!(
        discarded_domains(&db.conn, session_id).unwrap(),
        vec!["a.example".to_string()]
    );

    db.set_source_flag(session_id, "https://a.example/x", None)
        .unwrap();
    assert!(discarded_domains(&db.conn, session_id).unwrap().is_empty());
}

#[test]
fn upsert_research_stage_message_replaces_the_same_labels_row() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    db.upsert_research_stage_message(&s.id, "searching", "round 1, 1/3")
        .unwrap();
    db.upsert_research_stage_message(&s.id, "searching", "round 1, 2/3")
        .unwrap();
    db.upsert_research_stage_message(&s.id, "planning", "")
        .unwrap();

    let msgs = db.load_messages(&s.id).unwrap();
    let searching: Vec<_> = msgs
        .iter()
        .filter(|m| m.content.starts_with("searching:"))
        .collect();
    assert_eq!(searching.len(), 1, "expected one row, updated in place");
    assert!(searching[0].content.contains("2/3"));
    assert_eq!(msgs.iter().filter(|m| m.content == "planning").count(), 1);
}

#[test]
fn reply_ids_after_last_user_returns_only_the_last_reply() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let session = db.create_session("chat", "a/one", &space, "chat").unwrap();
    db.add_user_message(&session.id, "first question").unwrap();
    db.add_assistant_message(
        &session.id,
        "first reply",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    db.add_user_message(&session.id, "second question").unwrap();
    let tool = db.add_tool_call_message(&session.id, "{}").unwrap();
    let reply = db
        .add_assistant_message(
            &session.id,
            "second reply",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let other = db.create_session("other", "a/one", &space, "chat").unwrap();
    db.add_user_message(&other.id, "unrelated").unwrap();
    db.add_error_message(&other.id, "unrelated failure")
        .unwrap();
    db.conn
        .execute(
            "UPDATE messages SET created_at = '2026-01-01T00:00:00Z'",
            [],
        )
        .unwrap();

    assert_eq!(
        db.reply_ids_after_last_user(&session.id).unwrap(),
        vec![tool, reply]
    );
}

#[test]
fn reply_ids_after_last_user_uses_timestamps_before_insertion_order() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let session = db.create_session("chat", "a/one", &space, "chat").unwrap();
    let reply = db
        .add_assistant_message(
            &session.id,
            "last reply",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let user = db.add_user_message(&session.id, "last question").unwrap();
    let previous = db
        .add_error_message(&session.id, "earlier failure")
        .unwrap();
    let first_user = db.add_user_message(&session.id, "first question").unwrap();
    for (id, at) in [
        (&reply, "2026-01-04T00:00:00Z"),
        (&user, "2026-01-03T00:00:00Z"),
        (&previous, "2026-01-02T00:00:00Z"),
        (&first_user, "2026-01-01T00:00:00Z"),
    ] {
        db.conn
            .execute(
                "UPDATE messages SET created_at = ?2 WHERE id = ?1",
                (id, at),
            )
            .unwrap();
    }

    assert_eq!(
        db.reply_ids_after_last_user(&session.id).unwrap(),
        vec![reply]
    );
}

#[test]
fn reply_ids_after_last_user_is_empty_without_a_user_or_reply() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let session = db.create_session("chat", "a/one", &space, "chat").unwrap();
    assert!(
        db.reply_ids_after_last_user(&session.id)
            .unwrap()
            .is_empty()
    );
    db.add_error_message(&session.id, "no user yet").unwrap();
    assert!(
        db.reply_ids_after_last_user(&session.id)
            .unwrap()
            .is_empty()
    );
    db.add_user_message(&session.id, "no reply yet").unwrap();
    assert!(
        db.reply_ids_after_last_user(&session.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn session_and_message_roundtrip() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db
        .create_session("hello", "openai/gpt-4o", &space, "chat")
        .unwrap();

    db.add_user_message(&s.id, "hi").unwrap();
    db.add_assistant_message(
        &s.id,
        "hello there",
        Some("openai/gpt-4o"),
        Some("let me think"),
        Some(3),
        Some(1.5),
        Some(0.0042),
        Some("Vibed"),
    )
    .unwrap();

    let msgs = db.load_messages(&s.id).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[1].content, "hello there");
    assert_eq!(msgs[1].model.as_deref(), Some("openai/gpt-4o"));
    assert_eq!(msgs[1].reasoning.as_deref(), Some("let me think"));
    assert_eq!(msgs[1].tokens, Some(3));
    assert_eq!(msgs[1].cost, Some(0.0042));

    let sessions = db.list_sessions(&space).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, s.id);
}

#[test]
fn latest_session_returns_the_newest_session_across_spaces() {
    let db = Db::open_in_memory().unwrap();
    let default_id = db.default_space_id().unwrap();
    let other = db.create_space("other").unwrap();
    let older = db
        .create_session("older", "a/one", &default_id, "chat")
        .unwrap();
    let newer = db
        .create_session("newer", "b/two", &other.id, "chat")
        .unwrap();

    // Use explicit timestamps so the assertion is independent of clock
    // resolution on the test runner.
    db.raw()
        .execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            ("2026-01-01T00:00:00Z", &older.id),
        )
        .unwrap();
    db.raw()
        .execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            ("2026-01-02T00:00:00Z", &newer.id),
        )
        .unwrap();

    let (space_id, session) = db.latest_session().unwrap().unwrap();
    assert_eq!(space_id, other.id);
    assert_eq!(session.id, newer.id);
}

#[test]
fn latest_session_is_empty_without_sessions() {
    let db = Db::open_in_memory().unwrap();
    assert!(db.latest_session().unwrap().is_none());
}

#[test]
fn markdown_images_in_content_roundtrip() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    let content = "look at ![this](img.png) and ![that](other.png)";
    db.add_user_message(&s.id, content).unwrap();
    let msgs = db.load_messages(&s.id).unwrap();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].content.contains("![this](img.png)"));
    assert!(msgs[0].content.contains("![that](other.png)"));
}

#[test]
fn model_prefs_toggle_and_used() {
    let db = Db::open_in_memory().unwrap();
    assert!(db.toggle_favorite("a/one").unwrap()); // now favorite
    assert!(!db.toggle_favorite("a/one").unwrap()); // toggled off
    db.mark_model_used("a/one").unwrap();
    db.set_reasoning("a/one", Some("high")).unwrap();

    let prefs = db.load_model_prefs().unwrap();
    let p = &prefs[0];
    assert_eq!(p.id, "a/one");
    assert!(!p.favorite);
    assert!(p.last_used.is_some());
    assert_eq!(p.reasoning.as_deref(), Some("high"));
}

#[test]
fn setting_returns_a_single_value() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(db.setting("training:space"), None);
    db.set_setting("training:other", "off").unwrap();
    db.set_setting("training:space", "on").unwrap();
    assert_eq!(db.setting("training:space"), Some("on".to_string()));
    assert_eq!(db.setting("training:other"), Some("off".to_string()));
}

#[test]
fn settings_roundtrip() {
    let db = Db::open_in_memory().unwrap();
    db.set_setting("temperature", "0.7").unwrap();
    db.set_setting("temperature", "0.9").unwrap(); // upsert
    let s = db.load_settings().unwrap();
    assert_eq!(s, vec![("temperature".to_string(), "0.9".to_string())]);
}

#[test]
fn set_model_updates_row() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    db.set_session_model(&s.id, "c/d").unwrap();
    assert_eq!(db.list_sessions(&space).unwrap()[0].model, "c/d");
}

#[test]
fn compaction_persists_and_roundtrips() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    assert_eq!(s.compact_summary, None);
    assert_eq!(s.compact_through, 0);

    db.set_compaction(&s.id, "digest of earlier turns", 6)
        .unwrap();
    let reloaded = &db.list_sessions(&space).unwrap()[0];
    assert_eq!(
        reloaded.compact_summary.as_deref(),
        Some("digest of earlier turns")
    );
    assert_eq!(reloaded.compact_through, 6);
}

#[test]
fn spaces_crud_and_session_reassignment_on_delete() {
    let db = Db::open_in_memory().unwrap();
    let spaces = db.list_spaces().unwrap();
    assert_eq!(spaces.len(), 1);
    assert_eq!(spaces[0].name, DEFAULT_SPACE);

    let work = db.create_space("work").unwrap();
    let s = db.create_session("hi", "a/b", &work.id, "chat").unwrap();
    assert_eq!(db.count_sessions(&work.id).unwrap(), 1);

    db.rename_space(&work.id, "work-renamed").unwrap();
    assert!(
        db.list_spaces()
            .unwrap()
            .iter()
            .any(|s| s.name == "work-renamed")
    );

    db.delete_space(&work.id).unwrap();
    assert_eq!(db.list_spaces().unwrap().len(), 1); // work is gone
    let default_id = db.default_space_id().unwrap();
    let moved = db.list_sessions(&default_id).unwrap();
    assert!(moved.iter().any(|c| c.id == s.id)); // session survived, moved to default
}

#[test]
fn chunk_embeddings_store_rank_and_invalidate() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let id = db.upsert_file(&space, "book.pdf", "h1", 10, "ok").unwrap();
    db.set_file_chunks(
        &id,
        &[
            ("page 1".into(), "cooking with fire".into()),
            ("page 2".into(), "quantum entanglement".into()),
        ],
    )
    .unwrap();

    // Blob codec roundtrip.
    let v = vec![0.25f32, -1.0, 3.5];
    assert_eq!(blob_to_vec(&vec_to_blob(&v)), v);

    // No vectors yet → file needs embedding.
    assert_eq!(
        files_missing_embeddings(&db.conn, &space).unwrap(),
        vec![id.clone()]
    );

    db.set_chunk_embeddings(&id, &[(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])])
        .unwrap();
    assert!(
        files_missing_embeddings(&db.conn, &space)
            .unwrap()
            .is_empty()
    );

    // Query near the second chunk's vector ranks it first.
    let hits = semantic_chunks(&db.conn, &space, &[0.1, 0.9], 5).unwrap();
    assert_eq!(hits[0].1, "page 2");
    assert!(hits[0].2.contains("quantum"));
    assert!(hits[0].3 > hits[1].3, "scores must be descending");

    // Dimension-mismatched vectors are skipped, not an error.
    let hits = semantic_chunks(&db.conn, &space, &[1.0, 0.0, 0.0], 5).unwrap();
    assert!(hits.is_empty());

    // Rewriting chunks invalidates stale vectors.
    db.set_file_chunks(&id, &[("page 1".into(), "new text".into())])
        .unwrap();
    assert_eq!(
        files_missing_embeddings(&db.conn, &space).unwrap(),
        vec![id.clone()]
    );

    // A vector for the wrong sequence must not make the file look complete.
    db.set_chunk_embeddings(&id, &[(99, vec![1.0, 0.0])])
        .unwrap();
    assert_eq!(
        files_missing_embeddings(&db.conn, &space).unwrap(),
        vec![id.clone()]
    );
}

#[test]
fn rewriting_identical_chunks_keeps_their_embeddings() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let id = db.upsert_file(&space, "book.pdf", "h1", 10, "ok").unwrap();
    let chunks = [
        ("page 1".to_string(), "cooking with fire".to_string()),
        ("page 2".to_string(), "quantum entanglement".to_string()),
    ];
    db.set_file_chunks(&id, &chunks).unwrap();
    db.set_chunk_embeddings(&id, &[(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])])
        .unwrap();
    db.set_file_chunks(&id, &chunks).unwrap();
    assert!(
        files_missing_embeddings(&db.conn, &space)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn files_without_chunks_are_embedding_queue_entries() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let id = db
        .upsert_file(&space, "empty.py", "h", 0, "no text (scanned?)")
        .unwrap();
    assert_eq!(
        files_missing_embeddings(&db.conn, &space).unwrap(),
        vec![id]
    );

    // OCR rows stay in the queue until they report a terminal result;
    // the app-level worker skips them while they are active.
    let ocr = db.upsert_file(&space, "scan.pdf", "p", 0, "ocr…").unwrap();
    assert!(
        files_missing_embeddings(&db.conn, &space)
            .unwrap()
            .contains(&ocr)
    );
}

#[test]
fn files_upsert_list_delete_roundtrip() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let id = db.upsert_file(&space, "notes.md", "h1", 10, "ok").unwrap();
    db.set_file_chunks(&id, &[("lines 1-40".into(), "hello fts world".into())])
        .unwrap();

    let files = db.list_files(&space).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "notes.md");
    assert_eq!(files[0].hash, "h1");
    assert_eq!(files[0].status, "ok");

    // Re-import with a new hash keeps one row (same id or replaced) and replaces chunks.
    let id2 = db.upsert_file(&space, "notes.md", "h2", 12, "ok").unwrap();
    db.set_file_chunks(&id2, &[("lines 1-40".into(), "goodbye".into())])
        .unwrap();
    let files = db.list_files(&space).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].hash, "h2");

    db.delete_file(&files[0].id).unwrap();
    assert!(db.list_files(&space).unwrap().is_empty());
}

#[test]
fn chunk_search_ranks_and_scopes_by_space() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let other = db.create_space("other").unwrap();
    let a = db.upsert_file(&space, "a.md", "h", 1, "ok").unwrap();
    let b = db.upsert_file(&other.id, "b.md", "h", 1, "ok").unwrap();
    db.set_file_chunks(&a, &[("lines 1-40".into(), "rust borrow checker".into())])
        .unwrap();
    db.set_file_chunks(&b, &[("lines 1-40".into(), "rust in other space".into())])
        .unwrap();

    let hits = search_chunks(&db.conn, &space, "rust", 8).unwrap();
    assert_eq!(hits.len(), 1); // other space's chunk is excluded
    assert_eq!(hits[0].0, "a.md");
    assert_eq!(hits[0].1, "lines 1-40");
    assert!(hits[0].2.contains("rust"));

    // Special characters must not be an FTS syntax error.
    assert!(search_chunks(&db.conn, &space, "c++ \"quoted\" -dash", 8).is_ok());
}

#[test]
fn file_text_joins_chunks_in_order() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let id = db.upsert_file(&space, "doc.txt", "h", 1, "ok").unwrap();
    db.set_file_chunks(
        &id,
        &[
            ("lines 1-2".into(), "one\ntwo".into()),
            ("lines 3-4".into(), "three\nfour".into()),
        ],
    )
    .unwrap();
    let text = file_text(&db.conn, &space, "doc.txt").unwrap().unwrap();
    assert_eq!(text, "one\ntwo\nthree\nfour");
    assert!(
        file_text(&db.conn, &space, "missing.txt")
            .unwrap()
            .is_none()
    );
    assert_eq!(count_files(&db.conn, &space).unwrap(), 1);
}

#[test]
fn research_stage_messages_round_trip() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    db.add_research_stage_message(&s.id, "planning…").unwrap();
    let msgs = db.load_messages(&s.id).unwrap();
    assert_eq!(msgs.last().unwrap().role, "research_stage");
    assert_eq!(msgs.last().unwrap().content, "planning…");
}

#[test]
fn survey_messages_round_trip() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    db.add_survey_message(&s.id, "For \"topic\":\n 1. Depth or breadth?")
        .unwrap();
    let msgs = db.load_messages(&s.id).unwrap();
    let last = msgs.last().unwrap();
    assert_eq!(last.role, "survey");
    assert!(last.content.contains("Depth or breadth?"));
}

#[test]
fn gate_reply_round_trip() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let s = db.create_session("t", "a/b", &space, "chat").unwrap();
    db.add_gate_reply_message(&s.id, "the second option")
        .unwrap();
    let msgs = db.load_messages(&s.id).unwrap();
    let last = msgs.last().unwrap();
    assert_eq!(last.role, "gate_reply");
    assert_eq!(last.content, "the second option");
}

#[test]
fn create_list_touch_delete_watch_roundtrip() {
    let db = Db::open_in_memory().unwrap();
    let id = db
        .create_watch("space-1", "rust async runtimes", 24, "sess-1")
        .unwrap();
    let watches = db.list_watches("space-1").unwrap();
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].topic, "rust async runtimes");
    assert_eq!(watches[0].interval_hours, 24);
    assert!(watches[0].last_run_at.is_none());

    db.touch_watch(&id, "2026-07-07T00:00:00+00:00").unwrap();
    let watches = db.list_watches("space-1").unwrap();
    assert_eq!(
        watches[0].last_run_at.as_deref(),
        Some("2026-07-07T00:00:00+00:00")
    );

    db.delete_watch(&id).unwrap();
    assert!(db.list_watches("space-1").unwrap().is_empty());
}

#[test]
// space_a_watches / space_b_watches differ only by the space label.
#[allow(clippy::similar_names)]
fn list_all_watches_returns_watches_from_all_spaces() {
    let db = Db::open_in_memory().unwrap();

    // Create watches in different spaces
    let id1 = db.create_watch("space-a", "topic-1", 24, "sess-1").unwrap();
    let id2 = db.create_watch("space-b", "topic-2", 48, "sess-2").unwrap();
    let id3 = db.create_watch("space-a", "topic-3", 12, "sess-3").unwrap();

    // list_all_watches should return watches from all spaces
    let all_watches = db.list_all_watches().unwrap();
    assert_eq!(all_watches.len(), 3);
    assert!(
        all_watches
            .iter()
            .any(|w| w.id == id1 && w.space_id == "space-a")
    );
    assert!(
        all_watches
            .iter()
            .any(|w| w.id == id2 && w.space_id == "space-b")
    );
    assert!(
        all_watches
            .iter()
            .any(|w| w.id == id3 && w.space_id == "space-a")
    );

    // list_watches for one space should only return that space's watches,
    // confirming list_all_watches is not space-scoped
    let space_a_watches = db.list_watches("space-a").unwrap();
    assert_eq!(space_a_watches.len(), 2);
    assert!(space_a_watches.iter().all(|w| w.space_id == "space-a"));

    let space_b_watches = db.list_watches("space-b").unwrap();
    assert_eq!(space_b_watches.len(), 1);
    assert!(space_b_watches.iter().all(|w| w.space_id == "space-b"));
}

/// Build a db shaped like the pre-split schema: `files` carries
/// `status`/`mtime`, and the device-local tables live in `main` (the
/// legacy CREATE TABLEs + column adds, minus anything added later).
fn legacy_db(path: &std::path::Path, now: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL,
            model TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
         CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            role TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL);
         CREATE TABLE model_prefs (id TEXT PRIMARY KEY,
            favorite INTEGER NOT NULL DEFAULT 0, last_used TEXT);
         CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE spaces (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL);
         CREATE TABLE files (id TEXT PRIMARY KEY, space_id TEXT NOT NULL,
            name TEXT NOT NULL, hash TEXT NOT NULL, size INTEGER NOT NULL,
            status TEXT NOT NULL, created_at TEXT NOT NULL,
            UNIQUE(space_id, name));
         ALTER TABLE files ADD COLUMN mtime INTEGER NOT NULL DEFAULT 0;
         CREATE VIRTUAL TABLE file_chunks USING fts5(
            file_id UNINDEXED, seq UNINDEXED, location UNINDEXED, text);
         CREATE TABLE chunk_embeddings (file_id TEXT NOT NULL, seq INTEGER NOT NULL,
            vec BLOB NOT NULL, PRIMARY KEY (file_id, seq));
         CREATE TABLE web_cache (url_norm TEXT PRIMARY KEY, url TEXT NOT NULL,
            title TEXT, text TEXT NOT NULL, fetched_at TEXT NOT NULL);
         CREATE TABLE citations (id INTEGER PRIMARY KEY AUTOINCREMENT,
            space_id TEXT NOT NULL, report_file TEXT NOT NULL,
            url TEXT NOT NULL, title TEXT);
         CREATE TABLE session_sources (session_id TEXT NOT NULL,
            url_norm TEXT NOT NULL, PRIMARY KEY (session_id, url_norm));
         CREATE TABLE watches (id TEXT PRIMARY KEY, space_id TEXT NOT NULL,
            topic TEXT NOT NULL, interval_hours INTEGER NOT NULL,
            session_id TEXT NOT NULL, last_run_at TEXT);
         CREATE TABLE swarm_personas (session_id TEXT NOT NULL, ord INTEGER NOT NULL,
            name TEXT NOT NULL, model TEXT NOT NULL, persona TEXT NOT NULL);
         CREATE TABLE usage_log (id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at TEXT NOT NULL, session_id TEXT, space_id TEXT,
            backend TEXT NOT NULL, model TEXT NOT NULL,
            prompt_tokens INTEGER NOT NULL, completion_tokens INTEGER NOT NULL,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0, cost REAL);
         CREATE TABLE model_prices (model_id TEXT PRIMARY KEY, backend TEXT NOT NULL,
            prompt_price REAL NOT NULL, completion_price REAL NOT NULL,
            cache_read_price REAL, cache_write_price REAL, updated_at TEXT NOT NULL);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO spaces (id, name, created_at) VALUES ('sp', 'default', ?1)",
        [now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files (id, space_id, name, hash, size, status, created_at, mtime)
         VALUES ('f1', 'sp', 'a.txt', 'h1', 10, 'ok', ?1, 1234)",
        [now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_chunks (file_id, seq, location, text)
         VALUES ('f1', 0, 'l', 'hello world')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunk_embeddings (file_id, seq, vec) VALUES ('f1', 0, ?1)",
        [vec_to_blob(&[1.0, 0.0])],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO web_cache (url_norm, url, title, text, fetched_at)
         VALUES ('https://x.test/', 'https://x.test/', NULL, 'cached body', ?1)",
        [now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO model_prices (model_id, backend, prompt_price, completion_price, updated_at)
         VALUES ('a/model', 'OpenRouter', 1.0, 2.0, ?1)",
        [now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO usage_log (created_at, backend, model, prompt_tokens, completion_tokens)
         VALUES (?1, 'OpenRouter', 'a/model', 100, 10)",
        [now],
    )
    .unwrap();
    drop(conn);
}

#[test]
// The price catalog round-trips exact decimals (1.0 in, 1.0 out).
#[allow(clippy::float_cmp)]
fn legacy_db_migrates_cache_tables_and_seeds_file_index_state() {
    let dir = std::env::temp_dir().join(format!("nexus-migrate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("nexus.db");
    legacy_db(&db_path, &Utc::now().to_rfc3339());
    let mut db = Db::open(&db_path).unwrap();

    // user_version stamped; legacy columns survive as dead columns.
    let v: i64 = db
        .raw()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    assert!(has_column(db.raw(), "files", "mtime").unwrap());

    // The v2 migration renumbered the legacy default space (id 'sp')
    // to the deterministic `default` — every reference followed.
    assert_eq!(db.default_space_id().unwrap(), DEFAULT_SPACE);
    let files_space: String = db
        .raw()
        .query_row("SELECT space_id FROM files WHERE id = 'f1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(files_space, DEFAULT_SPACE);

    // Derived index state seeded from the legacy files row.
    let (mtime, status) = db
        .raw()
        .query_row(
            "SELECT mtime, status FROM cache.file_index_state WHERE file_id = 'f1'",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .unwrap();
    assert_eq!((mtime, status.as_str()), (1234, "ok"));

    // Device-local tables moved into the sibling cache.db, reachable
    // through the attached connection.
    assert!(cache_path_for(&db_path).is_file());
    let space = db.default_space_id().unwrap();
    let hits = search_chunks(db.raw(), &space, "hello", 8).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        file_text(db.raw(), &space, "a.txt").unwrap().as_deref(),
        Some("hello world")
    );
    let (_, text, _) = cache_get(db.raw(), "https://x.test/").unwrap().unwrap();
    assert_eq!(text, "cached body");
    let hits = semantic_chunks(db.raw(), &space, &[1.0, 0.0], 4).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, "a.txt");

    // The price catalog moved too — the backfill can price the legacy
    // usage row, and its sync_id was backfilled.
    assert_eq!(db.model_price("a/model").unwrap().prompt, 1.0);
    assert_eq!(db.backfill_usage_costs().unwrap(), 1);
    let sync: String = db
        .raw()
        .query_row("SELECT sync_id FROM usage_log WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!sync.is_empty());
}

/// Legacy `files` rows carried a `status TEXT NOT NULL` column with no
/// default. Once status moved to `cache.file_index_state`, inserts that
/// omit it failed on those dbs — the rescan swallowed the error, so every
/// new file stayed unindexed and was reprocessed on every rescan, forever.
#[test]
fn legacy_files_status_column_no_longer_blocks_new_files() {
    let dir = std::env::temp_dir().join(format!("nexus-status-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("nexus.db");
    legacy_db(&db_path, &Utc::now().to_rfc3339());
    let db = Db::open(&db_path).unwrap();
    assert!(!has_column(db.raw(), "files", "status").unwrap());
    let space = db.default_space_id().unwrap();
    db.upsert_file(&space, "pasted.png", "h2", 5, "ok · image")
        .unwrap();
    let files = db.list_files(&space).unwrap();
    let legacy = files.iter().find(|f| f.name == "a.txt").unwrap();
    assert_eq!(legacy.status, "ok", "index state survived the drop");
    let new = files.iter().find(|f| f.name == "pasted.png").unwrap();
    assert_eq!(new.status, "ok · image");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fresh_db_creates_sibling_cache_db_with_split_tables() {
    let dir = std::env::temp_dir().join(format!("nexus-fresh-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("nexus.db");
    let db = Db::open(&db_path).unwrap();
    let space = db.default_space_id().unwrap();
    let id = db.upsert_file(&space, "doc.txt", "h", 1, "ok").unwrap();
    db.set_file_chunks(&id, &[("l".into(), "needle text".into())])
        .unwrap();

    // The durable db holds no cache tables and no legacy file columns...
    assert!(!has_column(db.raw(), "web_cache", "url_norm").unwrap());
    assert!(!has_column(db.raw(), "files", "mtime").unwrap());
    // ...they live in the sibling cache.db: reachable through the
    // attached connection, and a standalone cache-only connection works.
    let cache_path = cache_path_for(&db_path);
    assert!(cache_path.is_file());
    let conn = rusqlite::Connection::open(&cache_path).unwrap();
    assert!(cache_get(&conn, "x").unwrap().is_none());
    drop(conn);
    assert_eq!(
        file_text(db.raw(), &space, "doc.txt").unwrap().as_deref(),
        Some("needle text")
    );
    assert_eq!(db.list_files(&space).unwrap()[0].status, "ok");
}

/// Every mutable table bumps its version (`updated_at`, RFC3339 so
/// lexical order = time order) on every mutation path — the LWW input
/// for the sync merge engine. Reads happen 2ms after each write so
/// equal-microsecond timestamps can't pass a `>` check by accident.
// Long by design (one assertion per mutation path).
#[allow(clippy::too_many_lines)]
#[test]
fn every_mutable_table_bumps_updated_at_on_mutation() {
    // One closure per table; the array needs a shared fn type.
    type Mutator<'a> = &'a dyn Fn(&Db) -> Result<()>;
    let mut db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let sid = db.create_session("t", "a/b", &space, "chat").unwrap().id;
    let read = |table: &str, id: &str| -> String {
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.raw()
            .query_row(
                &format!("SELECT updated_at FROM {table} WHERE id = ?1"),
                [id],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
    };

    // sessions: every mutation path bumps.
    let mutators: [Mutator<'_>; 5] = [
        &|db: &Db| db.set_compaction(&sid, "sum", 3),
        &|db: &Db| db.set_session_swarm_mode(&sid, true),
        &|db: &Db| db.set_session_title(&sid, "new", Some("new-slug")),
        &|db: &Db| db.set_session_model(&sid, "m/x"),
        &|db: &Db| db.set_research_parent(&sid, "parent"),
    ];
    for mutate in mutators {
        let before = read("sessions", &sid);
        mutate(&db).unwrap();
        assert!(read("sessions", &sid) > before);
    }
    // The swarm roster has no per-row LWW — saving versions the session.
    let before = read("sessions", &sid);
    db.save_swarm_personas(
        &sid,
        &[Persona {
            name: "a".into(),
            model: "m".into(),
            blurb: "b".into(),
        }],
    )
    .unwrap();
    assert!(read("sessions", &sid) > before);

    // model_prefs.
    assert!(db.toggle_favorite("a/model").unwrap());
    let before = read("model_prefs", "a/model");
    db.set_reasoning("a/model", Some("high")).unwrap();
    assert!(read("model_prefs", "a/model") > before);
    let before = read("model_prefs", "a/model");
    db.mark_model_used("a/model").unwrap();
    assert!(read("model_prefs", "a/model") > before);

    // app_settings.
    let read_key = |key: &str| -> String {
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.raw()
            .query_row(
                "SELECT updated_at FROM app_settings WHERE key = ?1",
                [key],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
    };
    db.set_setting("temperature", "0.5").unwrap();
    let before = read_key("temperature");
    db.set_setting("temperature", "0.9").unwrap();
    assert!(read_key("temperature") > before);

    // spaces.
    let sp = db.create_space("other").unwrap();
    let before = read("spaces", &sp.id);
    db.rename_space(&sp.id, "other2").unwrap();
    assert!(read("spaces", &sp.id) > before);

    // files.
    let fid = db.upsert_file(&space, "f.txt", "h", 1, "ok").unwrap();
    let before = read("files", &fid);
    db.rename_file(&fid, "g.txt").unwrap();
    assert!(read("files", &fid) > before);

    // watches.
    let wid = db.create_watch(&space, "topic", 24, &sid).unwrap();
    let before = read("watches", &wid);
    db.touch_watch(&wid, "2026-01-01T00:00:00Z").unwrap();
    assert!(read("watches", &wid) > before);
    let before = read("watches", &wid);
    db.set_watch_session(&wid, "other-session").unwrap();
    assert!(read("watches", &wid) > before);

    // session_sources: insert and flag changes both version the row.
    let url = "https://x.test/";
    add_session_sources(db.raw(), &sid, &[url.to_string()]).unwrap();
    let read_src = || -> String {
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.raw()
            .query_row(
                "SELECT updated_at FROM session_sources
                 WHERE session_id = ?1 AND url_norm = ?2",
                (&sid, url),
                |r| r.get::<_, String>(0),
            )
            .unwrap()
    };
    let before = read_src();
    db.set_source_flag(&sid, url, Some("pinned")).unwrap();
    assert!(read_src() > before);

    // usage_log: in-place updates bump; the backfill does too.
    let row = db
        .log_usage(
            "OpenRouter",
            "a/model",
            100,
            2,
            3,
            4,
            Default::default(),
            None,
            false,
            None,
            None,
        )
        .unwrap();
    let read_usage = |db: &Db, row: i64| -> String {
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.raw()
            .query_row(
                "SELECT updated_at FROM usage_log WHERE id = ?1",
                [&row],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
    };
    let before = read_usage(&db, row);
    db.update_usage(row, 200, 6, 7, 8, Some(0.1), false)
        .unwrap();
    assert!(read_usage(&db, row) > before);
    db.upsert_model_prices(&[(
        "a/model".to_string(),
        "OpenRouter".to_string(),
        price(1.0, 2.0),
    )])
    .unwrap();
    let before = read_usage(&db, row);
    assert_eq!(db.backfill_usage_costs().unwrap(), 1);
    assert!(read_usage(&db, row) > before);
}

#[test]
fn delete_paths_write_tombstones_for_sync() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let sid = db.create_session("t", "a/b", &space, "chat").unwrap().id;
    let tombstones = |table: &str| -> Vec<String> {
        let mut stmt = db
            .raw()
            .prepare("SELECT row_id FROM sync_tombstones WHERE table_name = ?1 ORDER BY row_id")
            .unwrap();
        let rows = stmt.query_map([table], |r| r.get::<_, String>(0)).unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };

    // Message delete.
    let mid = db.add_user_message(&sid, "hi").unwrap();
    db.delete_message(&mid).unwrap();
    assert_eq!(tombstones("messages"), vec![mid.clone()]);

    // Session delete tombstones the session and each of its messages.
    let sid2 = db.create_session("t2", "a/b", &space, "chat").unwrap().id;
    let m1 = db.add_user_message(&sid2, "one").unwrap();
    let m2 = db.add_user_message(&sid2, "two").unwrap();
    db.save_swarm_personas(
        &sid2,
        &[Persona {
            name: "p".into(),
            model: "m".into(),
            blurb: "b".into(),
        }],
    )
    .unwrap();
    db.delete_session(&sid2).unwrap();
    let mut expected = vec![mid, m1, m2];
    expected.sort();
    assert_eq!(tombstones("messages"), expected);
    assert_eq!(tombstones("sessions"), vec![sid2.clone()]);
    assert_eq!(tombstones("swarm_personas"), vec![format!("{sid2}:0")]);

    // File delete.
    let fid = db.upsert_file(&space, "f.txt", "h", 1, "ok").unwrap();
    db.delete_file(&fid).unwrap();
    assert_eq!(tombstones("files"), vec![fid]);

    // Watch delete.
    let wid = db.create_watch(&space, "topic", 24, &sid).unwrap();
    db.delete_watch(&wid).unwrap();
    assert_eq!(tombstones("watches"), vec![wid]);

    // Space delete (its sessions are reassigned, not deleted).
    let sp = db.create_space("doomed").unwrap();
    db.delete_space(&sp.id).unwrap();
    assert_eq!(tombstones("spaces"), vec![sp.id]);

    // Roster replace: removed slots are tombstoned as `session:ord`.
    let persona = |name: &str| Persona {
        name: name.into(),
        model: "m".into(),
        blurb: "b".into(),
    };
    db.save_swarm_personas(&sid, &[persona("a"), persona("b")])
        .unwrap();
    db.save_swarm_personas(&sid, &[persona("b")]).unwrap();
    let mut expected_persona_tombstones =
        vec![format!("{sid2}:0"), format!("{sid}:0"), format!("{sid}:1")];
    expected_persona_tombstones.sort();
    assert_eq!(tombstones("swarm_personas"), expected_persona_tombstones);
}

/// A v1 db (random uuid for the default space) gets
/// renumbered once on open: the default space becomes the deterministic
/// `default` row, and every space_id reference (and the tombstone)
/// follows — two devices' default spaces then merge as one LWW row.
#[test]
fn v1_default_space_renumbers_to_deterministic_id() {
    let dir = std::env::temp_dir().join(format!("nexus-v1-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("nexus.db");
    {
        let db = Db::open(&db_path).unwrap();
        let old = db.default_space_id().unwrap();
        assert_eq!(old, DEFAULT_SPACE);
        // Simulate a v1 db: random default id, rows pointing at it, a
        // tombstone for it.
        db.raw().execute("PRAGMA user_version = 1", []).unwrap();
        db.raw()
            .execute(
                "UPDATE spaces SET id = 'legacy-uuid' WHERE name = ?1",
                [DEFAULT_SPACE],
            )
            .unwrap();
        db.raw()
            .execute(
                "UPDATE sessions SET space_id = 'legacy-uuid' WHERE space_id = ?1",
                [&old],
            )
            .unwrap();
        db.raw()
            .execute(
                "INSERT INTO sync_tombstones (table_name, row_id, deleted_at)
                 VALUES ('spaces', 'legacy-uuid', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        let s = db.create_session("t", "m", &old, "chat").unwrap().id;
        let _ = s;
        drop(db);
    }
    // Reopen: the migration block re-runs (user_version 1 < 2) and
    // renumbers everything.
    let db = Db::open(&db_path).unwrap();
    assert_eq!(db.default_space_id().unwrap(), DEFAULT_SPACE);
    let n: i64 = db
        .raw()
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE space_id = ?1",
            [DEFAULT_SPACE],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "sessions followed the renumber");
    let tomb: i64 = db
        .raw()
        .query_row(
            "SELECT COUNT(*) FROM sync_tombstones WHERE table_name = 'spaces' AND row_id = ?1",
            [DEFAULT_SPACE],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tomb, 1, "the space tombstone followed too");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn device_id_is_stable_and_sync_state_roundtrips() {
    let db = Db::open_in_memory().unwrap();
    let a = db.device_id().unwrap();
    let b = db.device_id().unwrap();
    assert_eq!(a, b);
    assert!(!a.is_empty());

    // Cursors round-trip per (peer, table); None leaves values alone.
    db.set_sync_state(
        "peer-1",
        "sessions",
        Some("2026-01-01T00:00:00Z|id1"),
        Some("2026-01-02T00:00:00Z|id2"),
    )
    .unwrap();
    db.set_sync_state("peer-1", "sessions", None, Some("2026-01-03T00:00:00Z|id3"))
        .unwrap();
    db.set_sync_state("peer-1", "messages", Some("c1"), None)
        .unwrap();
    let states = db.load_sync_state().unwrap();
    assert_eq!(states.len(), 2);
    let s = states.iter().find(|s| s.table_name == "sessions").unwrap();
    assert_eq!(s.pull_cursor.as_deref(), Some("2026-01-01T00:00:00Z|id1"));
    assert_eq!(s.push_cursor.as_deref(), Some("2026-01-03T00:00:00Z|id3"));
    assert!(s.last_synced_at.is_some());
    let m = states.iter().find(|s| s.table_name == "messages").unwrap();
    assert_eq!(m.pull_cursor.as_deref(), Some("c1"));
}

#[test]
fn settings_scope_registry_classifies_keys_and_stores_scope() {
    // Device-local keys are classified local; user prefs default to sync.
    for local in [
        "searxng_url",
        "langsearch_key",
        "search_provider",
        "usage_range",
        "ui_background",
        "column_width",
        "popup_sizes",
        "image_viewer",
        "last_update_check",
    ] {
        assert!(Db::setting_is_local(local), "{local} should be local");
    }
    for sync in [
        "temperature",
        "top_p",
        "max_tokens",
        "show_stats",
        "show_reasoning",
        "hide_hints",
        "verbosity",
    ] {
        assert!(!Db::setting_is_local(sync), "{sync} should sync");
    }

    let db = Db::open_in_memory().unwrap();
    db.set_setting("temperature", "0.7").unwrap();
    db.set_setting("search_provider", "auto").unwrap();
    let scope = |key: &str| -> String {
        db.raw()
            .query_row(
                "SELECT scope FROM app_settings WHERE key = ?1",
                [key],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(scope("temperature"), "sync");
    assert_eq!(scope("search_provider"), "local");
}

#[test]
fn sync_ids_are_unique_for_citations_and_usage() {
    let db = Db::open_in_memory().unwrap();
    db.add_citations(
        "sp",
        "r.md",
        &[
            ("https://a.test/".to_string(), None),
            ("https://b.test/".to_string(), None),
        ],
    )
    .unwrap();
    db.log_usage(
        "OpenRouter",
        "m",
        1,
        2,
        0,
        0,
        Default::default(),
        None,
        false,
        None,
        None,
    )
    .unwrap();
    db.log_usage(
        "OpenRouter",
        "m",
        1,
        2,
        0,
        0,
        Default::default(),
        None,
        false,
        None,
        None,
    )
    .unwrap();
    let distinct: i64 = db
        .raw()
        .query_row(
            "SELECT COUNT(DISTINCT sync_id) FROM
             (SELECT sync_id FROM citations UNION ALL SELECT sync_id FROM usage_log)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let total: i64 = db
        .raw()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM citations) + (SELECT COUNT(*) FROM usage_log)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(distinct, total);
}
