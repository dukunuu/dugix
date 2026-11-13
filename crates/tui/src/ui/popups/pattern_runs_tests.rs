//! The shared popup pattern, as Jobs, Processes, Sub-agents and Tool health follow it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::{Popup, SubagentRun, SubagentStatus};
use nexus_core::jobs::{Job, JobRun, JobSpec};

use super::tests::{render_to_string, test_app};
use super::{harness, jobs, procs, subagents};
use crate::app_view::AppView;

fn assert_frame(screen: &str, title: &str, count: &str) {
    let top = screen.lines().find(|line| line.contains('╭')).unwrap();
    let bottom = screen.lines().find(|line| line.contains('╰')).unwrap();
    assert!(top.contains(&format!(" {title} ")), "{screen}");
    assert!(top.contains(&format!(" {count} ")), "{screen}");
    assert!(!bottom.contains(count), "{screen}");
    assert!(bottom.trim_end().ends_with("Esc close ╯"), "{screen}");
    for old in ["Ctrl+", "esc", "enter"] {
        assert!(!bottom.contains(old), "{screen}");
    }
}

fn cleanup(app: AppView) {
    let root = app.space.root.clone();
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

fn add_job(app: &AppView, name: &str) -> Job {
    let job = Job {
        id: name.into(),
        space_id: app.active_space.id.clone(),
        session_id: String::new(),
        device_id: app.db.device_id().unwrap(),
        spec: JobSpec {
            name: name.into(),
            prompt: "Review the tests".into(),
            ..JobSpec::default()
        },
        enabled: 1,
        last_run_at: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    app.db.save_job(&job).unwrap();
    job
}

fn add_run(app: &AppView, job: &Job) {
    let session = app
        .db
        .create_session("job run", "model", &app.active_space.id, "chat")
        .unwrap();
    app.db
        .add_user_message(&session.id, "Stored job transcript")
        .unwrap();
    app.db
        .save_job_run(&JobRun {
            id: session.id,
            job_id: job.id.clone(),
            device_id: job.device_id.clone(),
            started_at: job.updated_at.clone(),
            finished_at: None,
            status: "parked".into(),
            iters: 1,
            cost_usd: 0.01,
            branch: Some("nexus/job/tests".into()),
            summary: "Needs approval.".into(),
            updated_at: job.updated_at.clone(),
        })
        .unwrap();
}

fn add_subagent(app: &mut AppView) {
    let session = app
        .db
        .create_session("helpers", "model", &app.active_space.id, "chat")
        .unwrap();
    app.subagent_runs.push(SubagentRun {
        id: "helper".into(),
        session_id: session.id.clone(),
        number: 1,
        prompt: "Find callers".into(),
        model: "helper/fast".into(),
        writable: false,
        max_steps: 20,
        status: SubagentStatus::Done,
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        steps: 1,
        tokens: 321,
        cost: Some(0.0123),
        secs: 4.0,
        review: None,
        rows: Vec::new(),
        live_reasoning: String::new(),
        live_content: String::new(),
    });
    app.session = Some(session);
}

#[test]
fn jobs_frame_keeps_counts_above_and_keys_below() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    let job = add_job(&app, "review");
    add_run(&app, &job);
    add_job(&app, "tests");
    jobs::open(&mut app, false).unwrap();
    let screen = render_to_string(110, 30, |f| jobs::render(f, &app));
    assert_frame(&screen, "Jobs", "2 jobs · 1 parked");
    assert!(screen.contains("Needs approval."), "{screen}");
    cleanup(app);
}

#[test]
fn processes_frame_keeps_counts_above_and_keys_below() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    let screen = render_to_string(110, 30, |f| procs::render(f, &app));
    assert_frame(&screen, "Processes", "0 running");
    assert!(screen.contains("No background processes."), "{screen}");
    assert!(
        screen.contains("Processes stop when nexus exits."),
        "{screen}"
    );
    cleanup(app);
}

#[test]
fn subagents_frame_keeps_counts_above_and_keys_below() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    add_subagent(&mut app);
    subagents::open(&mut app);
    let screen = render_to_string(110, 30, |f| subagents::render(f, &app));
    assert_frame(&screen, "Sub-agents", "1 run · $0.0123 total");
    cleanup(app);
}

#[test]
fn tool_health_frame_keeps_counts_above_and_keys_below() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    app.harness = Some(nexus_core::db::HarnessSummary {
        calls: 12,
        failures: 3,
        rows: Vec::new(),
    });
    let screen = render_to_string(110, 30, |f| harness::render(f, &app));
    assert_frame(&screen, "Tool health", "12 calls · 3 failed · 7 days");
    assert!(
        screen.contains("No tool failures in the last 7 days."),
        "{screen}"
    );
    cleanup(app);
}

#[test]
fn jobs_delete_requires_two_control_d_keys_and_escape_cancels() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    add_job(&app, "review");
    jobs::open(&mut app, false).unwrap();
    jobs::handle_key(&mut app, KeyCode::Char('d').into()).unwrap();
    assert!(!app.jobs_popup.confirm_delete);
    assert_eq!(app.db.list_jobs().unwrap().len(), 1);
    let delete = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    jobs::handle_key(&mut app, delete).unwrap();
    assert!(app.jobs_popup.confirm_delete);
    assert_eq!(app.db.list_jobs().unwrap().len(), 1);
    let screen = render_to_string(110, 30, |f| jobs::render(f, &app));
    assert!(screen.contains(" Delete job? "), "{screen}");
    assert!(screen.contains("^D delete · Esc cancel"), "{screen}");
    assert!(
        screen.contains("“review” and its run history will be deleted."),
        "{screen}"
    );
    jobs::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.popup, Popup::Jobs);
    assert!(!app.jobs_popup.confirm_delete);
    assert_eq!(app.db.list_jobs().unwrap().len(), 1);
    jobs::handle_key(&mut app, delete).unwrap();
    jobs::handle_key(&mut app, delete).unwrap();
    assert!(app.db.list_jobs().unwrap().is_empty());
    assert!(app.jobs_popup.jobs.is_empty());
    assert!(!app.jobs_popup.confirm_delete);
    cleanup(app);
}

#[test]
fn jobs_logs_and_form_follow_the_pattern_and_escape_returns_to_list() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    let job = add_job(&app, "review");
    add_run(&app, &job);
    jobs::open(&mut app, false).unwrap();
    jobs::handle_key(&mut app, KeyCode::Enter.into()).unwrap();
    let screen = render_to_string(110, 30, |f| jobs::render(f, &app));
    assert!(screen.contains(" review logs "), "{screen}");
    assert!(screen.contains("Stored job transcript"), "{screen}");
    assert!(screen.contains("↑↓ scroll · Esc back"), "{screen}");
    jobs::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.popup, Popup::Jobs);
    assert!(app.jobs_popup.logs.is_none());
    jobs::handle_key(&mut app, KeyCode::Char('n').into()).unwrap();
    jobs::handle_key(&mut app, KeyCode::Char('j').into()).unwrap();
    jobs::handle_key(&mut app, KeyCode::Char('k').into()).unwrap();
    let screen = render_to_string(110, 30, |f| jobs::render(f, &app));
    assert!(screen.contains(" New job "), "{screen}");
    assert!(screen.contains("✎ jk▏"), "{screen}");
    assert!(
        screen.contains("Tab field · Enter next · ^S save · Esc cancel"),
        "{screen}"
    );
    assert_eq!(app.jobs_popup.form.as_ref().unwrap().field, 0);
    jobs::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.popup, Popup::Jobs);
    assert!(app.jobs_popup.form.is_none());
    cleanup(app);
}

#[test]
fn jobs_owned_by_another_device_keep_the_read_only_note_and_no_mutating_hints() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    let mut job = add_job(&app, "remote");
    job.device_id = "another-device".into();
    app.jobs_popup.jobs.push(job);
    app.popup = Popup::Jobs;
    let screen = render_to_string(110, 30, |f| jobs::render(f, &app));
    assert_frame(&screen, "Jobs", "1 job");
    assert!(screen.contains("other device (read-only)"), "{screen}");
    let footer = screen.lines().find(|line| line.contains('╰')).unwrap();
    assert!(
        footer.contains("↑↓ · Enter logs · n new · Esc close"),
        "{screen}"
    );
    for action in ["Space", "w daily", "r run", "^D"] {
        assert!(!footer.contains(action), "{screen}");
    }
    jobs::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert!(!app.jobs_popup.confirm_delete);
    cleanup(app);
}

#[cfg(unix)]
#[tokio::test]
async fn processes_k_moves_up_and_control_d_confirms_or_cancels_a_kill() {
    use nexus_core::tools::ToolBox;
    use nexus_core::workspace::Workspace;
    use std::sync::Arc;

    let mut app = test_app();
    app.settings.hide_hints = false;
    let mut toolbox = ToolBox::new(
        std::path::PathBuf::new(),
        None,
        None,
        "auto".into(),
        vec![],
        Some(app.space.root.join("nexus.db")),
        None,
        None,
    );
    toolbox.workspace = Some(Arc::new(Workspace::open(&app.space.root, vec![]).unwrap()));
    toolbox.procs = Arc::clone(&app.procs);
    for name in ["first", "second"] {
        let args = serde_json::json!({
            "action": "start", "cmd": "echo ready; sleep 30", "name": name, "ready_when": "ready",
        });
        let (result, _) = toolbox.run("proc", &args.to_string()).await;
        assert!(result.contains(" · ready ·"), "{result}");
    }
    app.popup = Popup::Procs;
    procs::handle_key(&mut app, KeyCode::Char('j').into()).unwrap();
    assert_eq!(app.procs_selected, 1);
    procs::handle_key(&mut app, KeyCode::Char('k').into()).unwrap();
    assert_eq!(app.procs_selected, 0);
    assert!(app.procs_kill_confirm.is_none());
    procs::handle_key(&mut app, KeyCode::Char('k').into()).unwrap();
    assert!(app.procs_kill_confirm.is_none());
    assert_eq!(app.procs.running_count(), 2);
    let rows = app.procs.list("");
    let id = rows[0].id.clone();
    let name = rows[0].name.clone();
    procs::handle_key(&mut app, KeyCode::Enter.into()).unwrap();
    let screen = render_to_string(110, 30, |f| procs::render(f, &app));
    assert!(screen.contains(&format!(" {name} output ")), "{screen}");
    assert!(screen.contains("↑↓ scroll · Esc back"), "{screen}");
    procs::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.popup, Popup::Procs);
    assert!(app.procs_output.is_none());
    let kill = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    procs::handle_key(&mut app, kill).unwrap();
    assert_eq!(app.procs_kill_confirm.as_deref(), Some(id.as_str()));
    let screen = render_to_string(110, 30, |f| procs::render(f, &app));
    assert!(screen.contains(" Kill process? "), "{screen}");
    assert!(screen.contains("^D kill · Esc cancel"), "{screen}");
    assert!(
        screen.contains(&format!("“{name}” will be stopped.")),
        "{screen}"
    );
    assert_eq!(app.procs.list("")[0].state, "running");
    procs::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.popup, Popup::Procs);
    assert!(app.procs_kill_confirm.is_none());
    procs::handle_key(&mut app, kill).unwrap();
    procs::handle_key(&mut app, kill).unwrap();
    assert_eq!(app.procs.list("")[0].state, "killed");
    assert!(app.procs_kill_confirm.is_none());
    app.procs.shutdown().await;
    cleanup(app);
}

#[test]
fn subagents_q_does_not_close_list_or_transcript() {
    let mut app = test_app();
    app.settings.hide_hints = false;
    add_subagent(&mut app);
    subagents::open(&mut app);
    subagents::handle_key(&mut app, KeyCode::Char('q').into());
    assert_eq!(app.popup, Popup::Subagents);
    subagents::handle_key(&mut app, KeyCode::Enter.into());
    subagents::handle_key(&mut app, KeyCode::Char('q').into());
    assert_eq!(app.popup, Popup::Subagents);
    assert_eq!(app.subagents.open.as_deref(), Some("helper"));
    let screen = render_to_string(110, 30, |f| subagents::render(f, &app));
    let top = screen.lines().find(|line| line.contains('╭')).unwrap();
    assert!(top.contains(" Sub-agent #1 "), "{screen}");
    assert!(top.contains(" read-only · 321 tokens · 4s "), "{screen}");
    assert!(
        screen.contains("↑↓ scroll · t tools · r reasoning · ←→ runs · Esc back"),
        "{screen}"
    );
    subagents::handle_key(&mut app, KeyCode::Char('k').into());
    assert_eq!(app.subagents.scroll, 1);
    subagents::handle_key(&mut app, KeyCode::Char('j').into());
    assert_eq!(app.subagents.scroll, 0);
    subagents::handle_key(&mut app, KeyCode::Esc.into());
    assert!(app.subagents.open.is_none());
    assert_eq!(app.popup, Popup::Subagents);
    subagents::handle_key(&mut app, KeyCode::Esc.into());
    assert_eq!(app.popup, Popup::None);
    cleanup(app);
}

#[test]
fn subagents_without_a_conversation_show_the_empty_state() {
    let mut app = test_app();
    subagents::open(&mut app);
    assert_eq!(app.popup, Popup::Subagents);
    let screen = render_to_string(110, 30, |f| subagents::render(f, &app));
    assert!(screen.contains("No conversation open."), "{screen}");
    cleanup(app);
}
