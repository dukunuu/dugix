use super::*;
use crate::app::tests::app_with_key;
use crate::app::{ChatTask, SpinnerColor};
use crate::provider::{BackendTag, StreamEvent, ToolCall, Usage};

fn update(id: &str, event: SubagentEvent) -> StreamEvent {
    StreamEvent::Subagent(Box::new(SubagentUpdate {
        id: id.into(),
        number: 7,
        event,
    }))
}

fn round(step: usize, calls: Vec<(ToolCall, String)>, cost: f64) -> SubagentEvent {
    SubagentEvent::Round {
        step,
        reasoning: Some(format!("why {step}")),
        content: format!("said {step}"),
        calls,
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            cost: Some(cost),
            ..Usage::default()
        }),
        secs: 1.5,
    }
}

async fn streaming(incognito: bool) -> (App, String) {
    let mut app = app_with_key();
    let session = app
        .db
        .create_session("helpers", "a/one", &app.active_space.id, "chat")
        .unwrap();
    app.session = Some(session.clone());
    app.chat_tasks.insert(
        1,
        ChatTask {
            id: 1,
            session_id: session.id.clone(),
            session_title: "helpers".into(),
            space_id: app.active_space.id.clone(),
            model: "a/one".into(),
            model_id: "a/one".into(),
            backend: BackendTag::OpenRouter,
            incognito,
            buffer: String::new(),
            thinking: String::new(),
            tool_status: None,
            usage: None,
            usage_row_id: None,
            started: std::time::Instant::now(),
            thinking_idx: 0,
            spinner_color: SpinnerColor::Green,
            abort: tokio::spawn(async {}).abort_handle(),
        },
    );
    (app, session.id)
}

fn started() -> SubagentEvent {
    SubagentEvent::Started {
        prompt: "find callers\nin core".into(),
        model: "helper/fast".into(),
        writable: false,
        max_steps: 20,
    }
}

#[tokio::test]
async fn a_run_streams_saves_its_transcript_and_adds_its_cost_to_the_reply() {
    let (mut app, session) = streaming(false).await;
    app.on_chat_event(1, update("run", started())).unwrap();
    app.on_chat_event(
        1,
        update(
            "run",
            SubagentEvent::Delta {
                reasoning: "looking".into(),
                content: "so far".into(),
            },
        ),
    )
    .unwrap();
    let live = app.live_subagent("run").unwrap();
    assert_eq!(
        (live.live_reasoning.as_str(), live.live_content.as_str()),
        ("looking", "so far")
    );
    assert_eq!(live.title(), "find callers");

    let call = ToolCall {
        id: "c1".into(),
        name: "fs".into(),
        arguments: r#"{"action":"grep","pattern":"run"}"#.into(),
    };
    let second = ToolCall {
        id: "c2".into(),
        ..call.clone()
    };
    app.on_chat_event(
        1,
        update(
            "run",
            round(
                0,
                vec![(call, "a.rs:1".into()), (second, "b.rs:2".into())],
                0.25,
            ),
        ),
    )
    .unwrap();
    app.on_chat_event(1, update("run", round(1, Vec::new(), 0.5)))
        .unwrap();
    app.on_chat_event(
        1,
        update(
            "run",
            SubagentEvent::Finished {
                error: None,
                review: None,
            },
        ),
    )
    .unwrap();

    let saved = app.db.subagent_runs(&session).unwrap();
    assert_eq!(saved.len(), 1);
    let run = &saved[0];
    assert_eq!(run.status, SubagentStatus::Done);
    assert_eq!((run.steps, run.tokens, run.cost), (2, 30, Some(0.75)));
    assert!(run.live_reasoning.is_empty() && run.live_content.is_empty());
    let roles: Vec<_> = run.rows.iter().map(|row| row.role.as_str()).collect();
    assert_eq!(roles, ["tool_call", "tool_call", "assistant"]);
    let first: serde_json::Value = serde_json::from_str(&run.rows[0].content).unwrap();
    assert_eq!(first["reasoning_content"], "why 0");
    assert_eq!(first["assistant_content"], "said 0");
    assert_eq!(first["result"], "a.rs:1");
    let next: serde_json::Value = serde_json::from_str(&run.rows[1].content).unwrap();
    assert!(next["reasoning_content"].is_null() && next["assistant_content"].is_null());
    assert_eq!(run.rows[2].content, "said 1");
    assert_eq!(run.rows[2].reasoning.as_deref(), Some("why 1"));
    assert_eq!(app.subagents_for(&session), saved);

    app.on_chat_event(
        1,
        StreamEvent::Usage(Usage {
            prompt_tokens: 100,
            completion_tokens: 10,
            total_tokens: 110,
            cost: Some(1.0),
            ..Usage::default()
        }),
    )
    .unwrap();
    app.on_chat_event(1, StreamEvent::Token("done".into()))
        .unwrap();
    app.on_chat_event(1, StreamEvent::Done).unwrap();
    let reply = app
        .db
        .load_messages(&session)
        .unwrap()
        .into_iter()
        .find(|m| m.role == "assistant")
        .unwrap();
    assert_eq!(reply.cost, Some(1.75));
    assert!(app.subagent_turn_cost.is_empty());
}

#[tokio::test]
async fn a_stopped_reply_ends_its_running_subagents_and_incognito_saves_nothing() {
    let (mut app, session) = streaming(true).await;
    app.on_chat_event(1, update("run", started())).unwrap();
    app.on_chat_event(1, update("run", round(0, Vec::new(), 0.1)))
        .unwrap();
    app.cancel_chat_task(1).unwrap();
    let run = app.live_subagent("run").unwrap();
    assert_eq!(run.status, SubagentStatus::Failed);
    assert_eq!(run.rows.last().unwrap().content, "stopped with the reply");
    assert!(app.db.subagent_runs(&session).unwrap().is_empty());
    assert_eq!(app.subagents_for(&session).len(), 1);
}

#[tokio::test]
async fn deleting_a_session_drops_its_saved_runs() {
    let (mut app, session) = streaming(false).await;
    app.on_chat_event(1, update("run", started())).unwrap();
    app.on_chat_event(
        1,
        update(
            "run",
            SubagentEvent::Finished {
                error: Some("provider failed".into()),
                review: None,
            },
        ),
    )
    .unwrap();
    let saved = app.db.subagent_runs(&session).unwrap();
    assert_eq!(saved[0].status, SubagentStatus::Failed);
    assert_eq!(saved[0].rows[0].role, "error");
    app.discard_chat_task(&session);
    app.db.delete_session(&session).unwrap();
    assert!(app.db.subagent_runs(&session).unwrap().is_empty());
}
