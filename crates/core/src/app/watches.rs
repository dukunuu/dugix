//! Repeating research jobs and report diffs.

use std::fmt::Write as _;

use crate::jobs::Job;

pub fn diff_section(previous_report: &str, new_report: &str, new_sources: &[String]) -> String {
    let _ = (previous_report, new_report);
    let mut out = String::from("## What changed since last run\n\n");
    if new_sources.is_empty() {
        out.push_str("No new sources since the last run.\n");
    } else {
        out.push_str("New sources:\n");
        for s in new_sources {
            let _ = writeln!(out, "- {s}");
        }
    }
    out
}

pub fn new_sources_since(new_report: &str, previous_citations: &[String]) -> Vec<String> {
    let previous: std::collections::HashSet<String> = previous_citations
        .iter()
        .map(|u| crate::tools::normalize_url(u))
        .collect();
    crate::citations::parse_citations(new_report)
        .into_iter()
        .map(|(_, url)| url)
        .filter(|url| !previous.contains(&crate::tools::normalize_url(url)))
        .collect()
}

impl super::App {
    pub fn create_watch(&mut self, topic: &str) {
        if topic.is_empty() {
            self.push_status("usage: /watch <topic>".to_string());
            return;
        }
        self.create_watch_with_interval(topic, 1440);
    }

    pub(crate) fn create_watch_with_interval(&mut self, topic: &str, interval_minutes: i64) {
        if self.start_research_job(
            crate::jobs::research::spec(topic, Some(interval_minutes)),
            false,
            None,
        ) {
            self.push_status(format!(
                "watching: {} (every {}h)",
                topic.trim(),
                interval_minutes / 60
            ));
        }
    }

    /// Start a research job in its own space, whether due or manually requested.
    pub fn run_research_job(&mut self, job: &Job) -> bool {
        let Ok(spaces) = self.db.list_spaces() else {
            return false;
        };
        let Some(space_row) = spaces.into_iter().find(|s| s.id == job.space_id) else {
            return false;
        };
        let restore_space = self.active_space.clone();
        let restore_session = self.session.clone();
        let restore_memory_snapshot = self.memory_snapshot.clone();
        let restore_cache_epoch = self.cache_epoch;
        let restore_messages = std::mem::take(&mut self.messages);
        let restore_work_mode = self.work_mode;
        let restore_workspace = self.workspace.clone();
        self.active_space = space_row;
        self.session = self.db.get_session(&job.session_id).ok().flatten();
        let started = self.start_research_job(job.spec.clone(), false, Some(&job.id));
        self.active_space = restore_space;
        self.session = restore_session;
        self.memory_snapshot = restore_memory_snapshot;
        self.cache_epoch = restore_cache_epoch;
        self.messages = restore_messages;
        self.work_mode = restore_work_mode;
        self.workspace = restore_workspace;
        self.refresh_toolbox();
        started
    }

    // Completion may arrive after a space switch; use the report's space, not the active space.
    pub fn previous_citations_for_watch_session(
        &self,
        session_id: &str,
        space_id: &str,
    ) -> anyhow::Result<Option<Vec<String>>> {
        let Some(w) = self
            .db
            .list_jobs()?
            .into_iter()
            .find(|job| job.spec.kind == "research" && job.session_id == session_id)
        else {
            return Ok(None);
        };
        // Unrelated reports in the same space must not suppress this watch's new sources.
        let slug = super::sessions::slugify(w.spec.topic.as_deref().unwrap_or(&w.spec.prompt));
        let prefix = format!("research-{slug}-");
        let rows = self.db.search_citations(space_id, Some(&prefix))?;
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|(report_file, _, _)| {
                report_file.starts_with(&prefix)
                    && report_file
                        .chars()
                        .nth(prefix.len())
                        .is_some_and(|c| c.is_ascii_digit())
            })
            .collect();
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(rows.into_iter().map(|(_, url, _)| url).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root =
            std::env::temp_dir().join(format!("nexus-watches-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k"), space)
    }

    fn create_research_job(a: &App, topic: &str, session_id: &str) -> String {
        let job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            space_id: a.active_space.id.clone(),
            session_id: session_id.to_string(),
            device_id: a.db.device_id().unwrap(),
            spec: crate::jobs::research::spec(topic, Some(1440)),
            enabled: 1,
            last_run_at: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        a.db.save_job(&job).unwrap();
        job.id
    }

    fn research_job(a: &App, id: &str) -> Job {
        a.db.list_jobs()
            .unwrap()
            .into_iter()
            .find(|job| job.id == id)
            .unwrap()
    }

    #[tokio::test]
    async fn a_watch_run_repoints_the_watch_at_its_new_session() {
        let mut a = test_app();
        a.current_model = Some("openai/gpt-5-mini".to_string());
        let space_id = a.active_space.id.clone();

        let first_session =
            a.db.create_session("first run", "openai/gpt-5-mini", &space_id, "chat")
                .unwrap();
        let watch_id = create_research_job(&a, "rust async runtimes", &first_session.id);

        assert!(a.run_research_job(&research_job(&a, &watch_id)));

        let updated = research_job(&a, &watch_id);
        assert_ne!(
            updated.session_id, first_session.id,
            "a watch run should repoint the watch at the session its re-run actually used, \
             not leave it pinned to the first run's session forever"
        );
        assert!(updated.last_run_at.is_some());
    }

    #[tokio::test]
    async fn a_watch_run_only_touches_the_watch_whose_job_actually_started() {
        let mut a = test_app();
        a.current_model = Some("openai/gpt-5-mini".to_string());
        let space_id = a.active_space.id.clone();

        let first_session =
            a.db.create_session("first run", "openai/gpt-5-mini", &space_id, "chat")
                .unwrap();
        let second_session =
            a.db.create_session("second run", "openai/gpt-5-mini", &space_id, "chat")
                .unwrap();
        let watch_a = create_research_job(&a, "rust async runtimes", &first_session.id);
        let watch_b = create_research_job(&a, "wasm gc proposal", &second_session.id);

        assert!(a.run_research_job(&research_job(&a, &watch_a)));
        assert!(!a.run_research_job(&research_job(&a, &watch_b)));

        let updated_a = research_job(&a, &watch_a);
        let updated_b = research_job(&a, &watch_b);

        assert_ne!(
            updated_a.session_id, first_session.id,
            "watch_a's job actually started, so it should be repointed at its new session"
        );
        assert!(
            updated_a.last_run_at.is_some(),
            "watch_a's job actually started, so it should be touched"
        );

        assert_eq!(
            updated_b.session_id, second_session.id,
            "watch_b's job never started (guard fired) — it must not be repointed"
        );
        assert!(
            updated_b.last_run_at.is_none(),
            "watch_b's job never started (guard fired) — touching it would falsely mark it caught up \
             and make it silently skip a full interval"
        );
    }

    fn watch(topic: &str, interval_hours: i64, last_run_at: Option<&str>) -> Job {
        Job {
            id: "w1".to_string(),
            space_id: "space-1".to_string(),
            session_id: "sess-1".to_string(),
            device_id: "device".to_string(),
            spec: crate::jobs::research::spec(topic, Some(interval_hours * 60)),
            enabled: 1,
            last_run_at: last_run_at.map(str::to_string),
            updated_at: String::new(),
        }
    }

    #[test]
    fn never_run_watch_is_always_due() {
        let w = watch("topic", 24, None);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T00:00:00+00:00")
            .unwrap()
            .to_utc();
        assert_eq!(crate::jobs::due(&[w], now, "device").len(), 1);
    }

    #[test]
    fn watch_run_recently_is_not_due() {
        let w = watch("topic", 24, Some("2026-07-07T00:00:00+00:00"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T05:00:00+00:00")
            .unwrap()
            .to_utc();
        assert!(crate::jobs::due(&[w], now, "device").is_empty());
    }

    #[test]
    fn watch_past_its_interval_is_due() {
        let w = watch("topic", 24, Some("2026-07-06T00:00:00+00:00"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T01:00:00+00:00")
            .unwrap()
            .to_utc();
        assert_eq!(crate::jobs::due(&[w], now, "device").len(), 1);
    }

    #[test]
    fn diff_section_lists_new_sources_when_present() {
        let section = diff_section(
            "# Old Report\nOld body.",
            "# New Report\nNew body.",
            &["https://new-source.example".to_string()],
        );
        assert!(
            section.contains("What changed since last run"),
            "{section:?}"
        );
        assert!(
            section.contains("https://new-source.example"),
            "{section:?}"
        );
    }

    #[test]
    fn diff_section_empty_new_sources_still_produces_a_header() {
        let section = diff_section("old", "new", &[]);
        assert!(section.contains("What changed since last run"));
        assert!(!section.contains("New sources"));
    }

    #[test]
    fn new_sources_since_filters_out_previously_cited_urls() {
        let new_report =
            "Body [1][2].\n\n## Sources\n1. https://old.example/a\n2. https://fresh.example/b\n";
        let previous = vec!["https://old.example/a".to_string()];
        let new_sources = new_sources_since(new_report, &previous);
        assert_eq!(new_sources, vec!["https://fresh.example/b".to_string()]);
    }

    #[test]
    fn previous_citations_for_watch_session_is_scoped_to_the_watchs_own_reports() {
        let a = test_app();
        let space_id = a.active_space.id.clone();
        let session =
            a.db.create_session(
                "rust async runtimes",
                "openai/gpt-5-mini",
                &space_id,
                "chat",
            )
            .unwrap();
        create_research_job(&a, "rust async runtimes", &session.id);

        let slug = super::super::sessions::slugify("rust async runtimes");
        a.db.add_citations(
            &space_id,
            &format!("research-{slug}-20260101-000000.md"),
            &[("https://own-report.example".to_string(), None)],
        )
        .unwrap();

        a.db.add_citations(
            &space_id,
            "research-some-other-topic-20260101-000000.md",
            &[("https://unrelated.example".to_string(), None)],
        )
        .unwrap();

        let prev = a
            .previous_citations_for_watch_session(&session.id, &space_id)
            .unwrap();
        let prev = prev.expect("watch session with prior citations should yield Some");
        assert_eq!(prev, vec!["https://own-report.example".to_string()]);
        assert!(
            !prev.contains(&"https://unrelated.example".to_string()),
            "an unrelated citation elsewhere in the space must not suppress a source as \
             already-cited for this watch: {prev:?}"
        );
    }

    #[test]
    fn new_sources_since_normalizes_urls_before_comparing() {
        let new_report = "Body [1].\n\n## Sources\n1. https://Old.example/a/\n";
        let previous = vec!["https://old.example/a".to_string()];
        assert!(new_sources_since(new_report, &previous).is_empty());
    }

    #[test]
    fn previous_citations_for_watch_session_prefix_collision_doesnt_match_longer_slugs() {
        let a = test_app();
        let space_id = a.active_space.id.clone();

        let session_rust =
            a.db.create_session("rust", "openai/gpt-5-mini", &space_id, "chat")
                .unwrap();
        create_research_job(&a, "rust", &session_rust.id);

        let session_rust_async =
            a.db.create_session("rust async", "openai/gpt-5-mini", &space_id, "chat")
                .unwrap();
        create_research_job(&a, "rust async", &session_rust_async.id);

        let slug_rust_async = super::super::sessions::slugify("rust async");
        a.db.add_citations(
            &space_id,
            &format!("research-{slug_rust_async}-20260101-000000.md"),
            &[("https://rust-async-report.example".to_string(), None)],
        )
        .unwrap();

        let prev_rust = a
            .previous_citations_for_watch_session(&session_rust.id, &space_id)
            .unwrap();
        assert!(
            prev_rust.is_none(),
            "rust watch should return None (no prior citations for itself), \
             not see rust-async watch's citations (would show prefix collision bug): {prev_rust:?}"
        );
    }
}
