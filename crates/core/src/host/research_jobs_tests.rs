use super::*;
use crate::db::Db;
use crate::provider::openrouter::OpenRouter;
use crate::space::Space;
use serde_json::json;

#[tokio::test]
async fn watches_api_keeps_its_json_shape_using_only_interval_research_jobs() {
    let root = std::env::temp_dir().join(format!("nexus-watch-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut app = App::new(
        Db::open_in_memory().unwrap(),
        None,
        Space { root: root.clone() },
    );
    app.current_model = Some("local:fake".into());
    app.backends.local =
        Some(OpenRouter::local(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap());
    let space = app.active_space.id.clone();
    let created = dispatch(
        &mut app,
        "POST",
        "/v1/watches",
        "",
        &serde_json::to_vec(&json!({
            "space_id": space,
            "topic": "topic",
            "interval_hours": 12,
        }))
        .unwrap(),
    )
    .unwrap();
    let job = app.db.list_jobs().unwrap().pop().unwrap();
    assert_eq!(
        created,
        json!({
            "id": job.id,
            "space_id": space,
            "topic": "topic",
            "interval_hours": 12,
            "session_id": job.session_id,
            "last_run_at": job.last_run_at,
        })
    );
    assert_eq!(job.spec.interval_minutes, 720);
    app.stop_research();
    app.run_command("research! one off").unwrap();
    assert_eq!(
        dispatch(&mut app, "GET", "/v1/watches", "", &[]).unwrap(),
        json!({ "watches": [created] })
    );
    let deleted = dispatch(
        &mut app,
        "DELETE",
        &format!("/v1/watches/{}", job.id),
        "",
        &serde_json::to_vec(&json!({ "space_id": space })).unwrap(),
    )
    .unwrap();
    assert_eq!(deleted, json!({ "deleted": true }));
    assert_eq!(
        dispatch(&mut app, "GET", "/v1/watches", "", &[]).unwrap(),
        json!({ "watches": [] })
    );
    assert_eq!(app.db.list_jobs().unwrap().len(), 1);
    app.stop_research();
    std::fs::remove_dir_all(root).unwrap();
}
