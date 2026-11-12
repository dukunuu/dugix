use super::*;
use crate::app::App;
use crate::db::Db;
use crate::provider::openrouter::OpenRouter;
use crate::space::Space;

struct Fixture {
    app: App,
    _listener: std::net::TcpListener,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("nexus-research-job-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut app = App::new(Db::open_in_memory().unwrap(), None, Space { root });
        app.current_model = Some("local:fake".into());
        app.backends.set(
            crate::provider::BackendTag::Local,
            OpenRouter::local(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap(),
        );
        Self {
            app,
            _listener: listener,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.stop_research();
        std::fs::remove_dir_all(&self.app.space.root).unwrap();
    }
}

#[tokio::test]
async fn research_commands_persist_a_job_and_a_run_with_the_requested_gate() {
    for (command, trigger, gated) in [
        ("research topic", "manual", true),
        ("research! topic", "manual", false),
        ("watch topic", "interval", false),
    ] {
        let mut fixture = Fixture::new();
        fixture.app.run_command(command).unwrap();
        let jobs = fixture.app.db.list_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.space_id, fixture.app.active_space.id);
        assert_eq!(job.spec.kind, "research");
        assert_eq!(job.spec.topic.as_deref(), Some("topic"));
        assert_eq!(job.spec.name, "topic");
        assert_eq!(job.spec.prompt, "topic");
        assert!(job.spec.root.is_empty());
        assert_eq!(job.spec.mode, "agent");
        assert_eq!(job.spec.trigger_kind, trigger);
        assert_eq!(job.spec.interval_minutes, 1440);
        assert_eq!(job.spec.notify, "on_change");
        assert_eq!(job.spec.max_iters, 5);
        assert_eq!(job.spec.max_cost_usd, 1.0);
        assert_eq!(job.spec.max_wall_minutes, 30);
        let session = fixture.app.session.as_ref().unwrap();
        assert_eq!(job.session_id, session.id);
        let runs = fixture.app.db.list_job_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, session.id);
        assert_eq!(runs[0].job_id, job.id);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].finished_at, None);
        assert_eq!(
            job.last_run_at.as_deref(),
            Some(runs[0].started_at.as_str())
        );
        assert_eq!(fixture.app.research.survey_reply_tx.is_some(), gated);
        fixture.app.run_command("watch another topic").unwrap();
        assert_eq!(fixture.app.db.list_jobs().unwrap().len(), 1);
        assert_eq!(fixture.app.db.list_job_runs().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn derived_topics_create_manual_jobs_and_repeat_runs_reuse_the_job() {
    let mut fixture = Fixture::new();
    fixture
        .app
        .on_research_topic_derived(Some(Ok("topic".into())));
    let job = fixture.app.db.list_jobs().unwrap().pop().unwrap();
    assert_eq!(job.spec.trigger_kind, "manual");
    assert!(fixture.app.research.survey_reply_tx.is_some());
    fixture.app.stop_research();
    let other = fixture.app.db.create_space("other").unwrap();
    fixture.app.set_active_space(other.clone());
    let session = fixture
        .app
        .db
        .create_session("active", "local:fake", &other.id, "chat")
        .unwrap();
    let session_before = session.id.clone();
    fixture.app.session = Some(session);
    assert!(fixture.app.run_research_job(&job));
    assert_eq!(fixture.app.active_space.id, other.id);
    assert_eq!(fixture.app.session.as_ref().unwrap().id, session_before);
    assert!(fixture.app.research.survey_reply_tx.is_none());
    let jobs = fixture.app.db.list_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, job.id);
    assert_ne!(jobs[0].session_id, job.session_id);
    assert_eq!(fixture.app.db.list_job_runs().unwrap().len(), 2);
    assert!(!fixture.app.run_research_job(&jobs[0]));
    assert_eq!(fixture.app.db.list_job_runs().unwrap().len(), 2);
}

#[test]
fn manual_research_is_never_due_and_daily_research_waits_1440_minutes() {
    let mut job = Job {
        id: "job".into(),
        space_id: "default".into(),
        session_id: String::new(),
        device_id: "device".into(),
        spec: research::spec("topic", None),
        enabled: 1,
        last_run_at: None,
        updated_at: String::new(),
    };
    job.spec.validate().unwrap();
    let now = parse_time("2026-01-02T00:00:00Z").unwrap();
    assert!(due(&[job.clone()], now, "device").is_empty());
    job.last_run_at = Some("2026-01-01T00:00:00Z".into());
    assert!(due(&[job.clone()], now, "device").is_empty());
    job.spec = research::spec("topic", Some(1440));
    assert!(due(&[job.clone()], now - chrono::Duration::minutes(1), "device").is_empty());
    assert_eq!(due(&[job], now, "device").len(), 1);
}
