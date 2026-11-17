//! `dugix ask` — one-shot headless turns.

use std::io::Read as _;

use anyhow::{Context as _, Result, bail};

use dugix_core::app;
use dugix_core::config;
use dugix_core::db;
use dugix_core::space;

use super::{out, short_id};

pub(super) async fn build_app(model: Option<&str>, space_name: Option<&str>) -> Result<app::App> {
    let saved = config::load_all_providers()?;
    let mut app = dugix_core::boot(saved).await?;
    // Headless requests must wait for background connectors before building their tool list.
    app.connectors
        .wait_ready(std::time::Duration::from_secs(12))
        .await;
    if let Some(name) = space_name {
        app.execute(dugix_core::app::AppCommand::SwitchSpace {
            name: name.to_string(),
        })?;
    }
    if let Some(m) = model {
        // Headless mode has no catalog for SetModel resolution; the next turn validates the backend.
        app.current_model = Some(m.to_string());
    }
    Ok(app)
}

fn ensure_space(name: &str) -> Result<()> {
    let name = name
        .strip_prefix("new:")
        .context("space must be an existing name or new:NAME")?;
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    if db.list_spaces()?.iter().any(|s| s.name == name) {
        return Ok(());
    }
    db.create_space(name)?;
    space.ensure_space_dir(name)?;
    Ok(())
}

pub(super) struct AskOptions {
    pub(super) model: Option<String>,
    pub(super) space_name: Option<String>,
    pub(super) json: bool,
    pub(super) quiet: bool,
}

pub(super) fn ask_options(
    model: Option<String>,
    space_name: Option<String>,
    json: bool,
    quiet: bool,
) -> AskOptions {
    AskOptions {
        model,
        space_name,
        json,
        quiet,
    }
}

pub(super) async fn ask(
    prompt: Option<String>,
    model: Option<String>,
    space_name: Option<String>,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let prompt = if let Some(p) = prompt {
        p
    } else {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    };
    let options = AskOptions {
        model,
        space_name,
        json,
        quiet,
    };
    let outcome = ask_prompt(prompt, options).await?;
    print_ask_outcome(&outcome, json, quiet)
}

pub(super) async fn ask_prompt(
    prompt: String,
    options: AskOptions,
) -> Result<app::headless::AskOutcome> {
    if prompt.trim().is_empty() {
        bail!("empty prompt — pass the question as an argument or pipe it on stdin");
    }
    let space_name = match options.space_name {
        Some(n) if n.starts_with("new:") => {
            ensure_space(&n)?;
            Some(n["new:".len()..].to_string())
        }
        n => n,
    };
    let mut app = build_app(options.model.as_deref(), space_name.as_deref()).await?;
    app.ask_headless(
        prompt,
        app::headless::TurnOpts {
            stream: !options.json,
            quiet: options.quiet,
        },
    )
    .await
}

pub(super) fn print_ask_outcome(
    outcome: &app::headless::AskOutcome,
    json: bool,
    quiet: bool,
) -> Result<()> {
    if json {
        let v = serde_json::json!({
            "answer": outcome.answer,
            "session_id": outcome.session_id,
            "session_title": outcome.session_title,
            "usage": outcome.usage.as_ref().map(|u| serde_json::json!({
                "prompt_tokens": u.prompt_tokens,
                "completion_tokens": u.completion_tokens,
                "cache_read_tokens": u.cache_read_tokens,
                "cache_creation_tokens": u.cache_creation_tokens,
                "cost": u.cost,
            })),
        });
        out(serde_json::to_string_pretty(&v)?);
    } else if !quiet {
        eprintln!(
            "saved as \"{}\" — reopen it in the TUI with /sessions (id {})",
            outcome.session_title,
            short_id(&outcome.session_id)
        );
    }
    Ok(())
}
