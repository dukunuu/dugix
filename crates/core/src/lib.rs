//! nexus-chat-core (lib crate `nexus_core`): the engine behind the `nexus` TUI,
//! CLI and `nexus host` API, owning all domain logic and no terminal UI.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

pub mod app;
pub mod app_templates;
pub mod appserver;
pub mod canvas;
pub mod citations;
pub mod config;
pub mod connectors;
pub mod db;
pub mod decider;
pub mod deck;
pub mod design;
pub mod doc;
pub mod eval;
pub mod extract;
pub mod filesearch;
pub mod highlight;
pub mod host;
pub mod interactive;
pub mod jobs;
pub mod markdown;
pub mod memory;
pub mod panels;
pub mod prompts;
pub mod provider;
pub mod remote_image;
pub mod sheet;
pub mod skills;
pub mod space;
pub mod subagent;
pub mod sync;
pub mod theme_colors;
pub mod tools;
pub mod trace;
pub mod train;
pub mod update;
pub mod usershell;
pub mod workspace;

use anyhow::Result;

/// One bootstrap for every frontend (TUI, CLI, host).
pub async fn boot(saved: config::SavedCreds) -> Result<app::App> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    if space.traces_dir().join("index.db").exists() {
        let _ = (|| -> Result<()> {
            let mut statement = db.conn().prepare("SELECT id FROM sessions")?;
            let existing = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
            let store =
                trace::TraceStore::open(&space.traces_dir(), trace::Scrubber::new(Vec::new()))?;
            store.prune_missing_sessions(&existing)?;
            Ok(())
        })();
    }
    let mut app = app::App::with_creds(db, space, saved);
    for note in app.db.take_upgrade_notes() {
        app.push_status(note);
    }
    app.system_prompt_overlay = config::load_system_prompt_overlay();
    // Connectors connect in the background; the TUI never waits for them.
    app.connectors =
        connectors::Registry::system(&app.space.root, Some(app.connector_notes_tx.clone()), true);
    match app.connectors.load() {
        Ok(problems) => {
            for problem in problems {
                app.push_status(format!("connector config: {problem}"));
            }
        }
        Err(error) => app.push_status(format!("connectors not loaded: {error:#}")),
    }
    app.discover_embeddings();
    app.app_server = appserver::AppServer::start(app.space.spaces_root()).await;
    app.refresh_toolbox();
    Ok(app)
}
