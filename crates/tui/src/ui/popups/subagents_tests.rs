use crossterm::event::{KeyCode, KeyEvent};
use nexus_core::app::{App, Popup, SubagentRow, SubagentRun, SubagentStatus};
use nexus_core::db::Db;
use nexus_core::space::Space;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app_view::AppView;

fn app_with_session() -> AppView {
    let db = Db::open_in_memory().unwrap();
    let root = std::env::temp_dir().join(format!("nexus-agents-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    let mut app = AppView::new(App::new(db, Some("k"), Space { root }));
    let session = app
        .db
        .create_session("helpers", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.session = Some(session);
    app
}

fn run(app: &AppView, id: &str, number: u64, status: SubagentStatus) -> SubagentRun {
    SubagentRun {
        id: id.into(),
        session_id: app.session.as_ref().unwrap().id.clone(),
        number,
        prompt: format!("find callers of run {number}\nthen report"),
        model: "helper/fast".into(),
        writable: false,
        max_steps: 20,
        status,
        started_at: format!("2026-11-08T00:00:0{number}Z"),
        finished_at: None,
        steps: 1,
        tokens: 12_345,
        cost: Some(0.0123),
        secs: 4.0,
        review: None,
        rows: vec![
            SubagentRow {
                role: "tool_call".into(),
                content: serde_json::json!({
                    "id": "c1",
                    "reasoning_content": "grep first",
                    "name": "fs",
                    "arguments": r#"{"action":"grep","pattern":"run("}"#,
                    "result": "core/src/a.rs:10: run(",
                })
                .to_string(),
                reasoning: None,
            },
            SubagentRow {
                role: "assistant".into(),
                content: "Two callers found.".into(),
                reasoning: Some("checked both".into()),
            },
        ],
        live_reasoning: String::new(),
        live_content: String::new(),
    }
}

fn screen(app: &mut AppView) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal
        .draw(|f| crate::ui::popups::subagents::render(f, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn key(app: &mut AppView, code: KeyCode) {
    crate::ui::popups::subagents::handle_key(app, KeyEvent::from(code));
}

#[test]
fn agents_lists_runs_newest_first_with_their_usage() {
    let mut app = app_with_session();
    let saved = run(&app, "old", 1, SubagentStatus::Done);
    app.db.save_subagent_run(&saved).unwrap();
    let live = run(&app, "new", 2, SubagentStatus::Running);
    app.subagent_runs.push(live);
    app.run_command("agents").unwrap();
    assert_eq!(app.popup, Popup::Subagents);
    let text = screen(&mut app);
    let newer = text.find("find callers of run 2").expect(&text);
    let older = text.find("find callers of run 1").expect(&text);
    assert!(newer < older, "newest first:\n{text}");
    assert!(text.contains("12.3k tokens"), "{text}");
    assert!(text.contains("$0.0123"), "{text}");
    assert!(text.contains("2 sub-agents"), "{text}");
    assert!(text.contains("$0.0246 total"), "{text}");
}

#[test]
fn a_transcript_shows_reasoning_calls_answer_and_live_text() {
    let mut app = app_with_session();
    let mut live = run(&app, "live", 3, SubagentStatus::Running);
    live.live_reasoning = "now reading a.rs".into();
    live.live_content = "Partial answer".into();
    app.subagent_runs.push(live);
    crate::ui::popups::subagents::open(&mut app);
    key(&mut app, KeyCode::Enter);
    let text = screen(&mut app);
    for expected in [
        "sub-agent #3",
        "find callers of run 3",
        "grep first",
        "grep",
        "checked both",
        "Two callers found.",
        "now reading a.rs",
        "Partial answer",
        "working · step 2/20",
    ] {
        assert!(text.contains(expected), "{expected:?} missing:\n{text}");
    }
    // r folds the reasoning to its one-line summary.
    key(&mut app, KeyCode::Char('r'));
    let folded = screen(&mut app);
    assert!(!folded.contains("checked both"), "{folded}");
    // Esc goes back to the list, then closes.
    key(&mut app, KeyCode::Esc);
    assert!(app.subagents.open.is_none());
    assert_eq!(app.popup, Popup::Subagents);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.popup, Popup::None);
}

#[test]
fn an_empty_conversation_says_how_sub_agents_start() {
    let mut app = app_with_session();
    crate::ui::popups::subagents::open(&mut app);
    let text = screen(&mut app);
    assert!(
        text.contains("No sub-agents in this conversation yet"),
        "{text}"
    );
}
