use super::*;
use dugix_core::app::App;
use dugix_core::db::Db;
use dugix_core::space::Space;

#[test]
fn watch_without_a_topic_opens_jobs_and_w_toggles_daily_repeats() {
    let root = std::env::temp_dir().join(format!("dugix-research-popup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    let mut app = AppView::new(App::new(
        Db::open_in_memory().unwrap(),
        None,
        Space { root: root.clone() },
    ));
    app.core
        .execute(AppCommand::CreateJob {
            spec: JobSpec {
                kind: "research".into(),
                topic: Some("topic".into()),
                name: "topic".into(),
                prompt: "topic".into(),
                trigger_kind: "manual".into(),
                notify: "on_change".into(),
                ..JobSpec::default()
            },
        })
        .unwrap();
    app.run_command("watch").unwrap();
    assert_eq!(app.popup, Popup::Jobs);
    assert_eq!(app.jobs_popup.jobs.len(), 1);
    let key = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE);
    handle_key(&mut app, key).unwrap();
    let job = app.db.list_jobs().unwrap().pop().unwrap();
    assert_eq!(job.spec.trigger_kind, "interval");
    assert_eq!(job.spec.interval_minutes, 1440);
    assert_eq!(app.jobs_popup.jobs[0].spec.trigger_kind, "interval");
    handle_key(&mut app, key).unwrap();
    assert_eq!(app.db.list_jobs().unwrap()[0].spec.trigger_kind, "manual");
    assert!(app.db.list_job_runs().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
