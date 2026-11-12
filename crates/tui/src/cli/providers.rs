//! `nexus models`, `nexus login` and `nexus skills`.

use anyhow::{Result, bail};

use nexus_core::app;
use nexus_core::config;
use nexus_core::provider::Model;
use nexus_core::provider::openrouter::OpenRouter;
use nexus_core::space;

use super::ask::build_app;
use super::{SkillsCmd, out, truncate};

pub(super) async fn models(backend: Option<&str>) -> Result<()> {
    let mut app = build_app(None, None).await?;
    if !app.backends.any() {
        bail!("no API keys configured — `nexus login <provider> <key>` or set the env vars");
    }
    app.init();
    let deadline = tokio::time::sleep(std::time::Duration::from_mins(1));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => bail!("timed out fetching model catalogs"),
            ev = app.next_event() => {
                if let app::AppEvent::Models(r) = ev {
                    app.on_models_result(r);
                    break;
                }
            },
        }
    }
    let mut models: Vec<&Model> = app
        .models
        .iter()
        .filter(|m| {
            let prefix = m.backend.key_prefix().trim_end_matches(':');
            backend.is_none_or(|b| {
                prefix.eq_ignore_ascii_case(b) || m.backend.name().eq_ignore_ascii_case(b)
            })
        })
        .collect();
    if app.models.is_empty() {
        let mut status = String::new();
        while let Some(ev) = app.pop_pending_event() {
            if let app::AppEvent::Status(s) = ev {
                status = s;
            }
        }
        bail!("{status}");
    }
    if models.is_empty() {
        bail!("no models from backend {backend:?} — `nexus models` lists them");
    }
    models.sort_by(|a, b| a.backend.name().cmp(b.backend.name()).then(a.id.cmp(&b.id)));
    for m in models {
        let ctx = m
            .context_length
            .map_or_else(|| "—".to_string(), |c| format!("{}k", c / 1000));
        out(format!(
            "{:<44} {:<10} {:>7}  {}",
            truncate(&m.id, 44),
            m.backend.name(),
            ctx,
            if m.supports_images { "images" } else { "" },
        ));
    }
    Ok(())
}

pub(super) async fn login(provider: &str, key: &str, check: bool) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        bail!("empty key");
    }
    if !matches!(provider, "openrouter" | "openai" | "opencode" | "mistral") {
        bail!("unknown provider {provider:?} — use openrouter, openai, opencode, or mistral");
    }
    config::save_provider_key(provider, key)?;
    out(format!("saved {provider} key"));
    if check {
        let client = client_for(provider, key);
        match client.list_models().await {
            Ok(_) => out("key check: ok"),
            Err(e) => bail!("key check failed: {e}"),
        }
    }
    Ok(())
}

/// Uses the configured provider because most API keys have no identifying prefix.
pub(super) fn client_for(provider: &str, key: &str) -> OpenRouter {
    let key = key.to_string();
    match provider {
        "openai" => OpenRouter::openai(key),
        "opencode" => OpenRouter::opencode_go(key),
        "mistral" => OpenRouter::mistral(key),
        _ => OpenRouter::from_key_auto(key),
    }
}

pub(super) async fn skills(cmd: SkillsCmd) -> Result<()> {
    match cmd {
        SkillsCmd::List => skills_list(),
        SkillsCmd::Install { skill } => skills_install(&skill).await,
    }
}

fn skills_list() -> Result<()> {
    let space = space::Space::open()?;
    let local = nexus_core::skills::skills_dir(&space.root);
    nexus_core::skills::install_builtin(&local);
    let skills = nexus_core::skills::load_skills_from_dirs(&nexus_core::skills::app_skill_roots(
        &space.root,
    ));
    if skills.is_empty() {
        out("no skills installed — `nexus skills install <owner/repo[/path]>`");
        return Ok(());
    }
    for s in skills {
        out(format!(
            "{:<32} {}",
            truncate(&s.name, 32),
            truncate(&s.description, 60),
        ));
    }
    Ok(())
}

async fn skills_install(spec: &str) -> Result<()> {
    let Some((owner, repo, path)) = nexus_core::skills::parse_gh_shorthand(spec) else {
        bail!("expected owner/repo[/path] shorthand, got {spec:?}");
    };
    let space = space::Space::open()?;
    let dest = nexus_core::skills::skills_dir(&space.root);
    let name = nexus_core::skills::install_from_github(
        &reqwest::Client::new(),
        &owner,
        &repo,
        &path,
        &dest,
    )
    .await?;
    out(format!("installed skill {name}"));
    Ok(())
}
