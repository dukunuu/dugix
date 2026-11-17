//! `dugix status`, `doctor`, `update`, `open` and `train`.

use anyhow::{Context as _, Result, bail};

use dugix_core::config;
use dugix_core::db;
use dugix_core::provider::BackendTag;
use dugix_core::space;
use dugix_core::train;

use super::providers::client_for;
use super::{TrainCmd, out, resolve_session};

pub(super) fn status() -> Result<()> {
    let creds = config::load_creds_offline();
    let dirs = config::project_dirs()?;
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let spaces = db.list_spaces()?;
    let sessions: u64 = spaces
        .iter()
        .map(|s| db.count_sessions(&s.id).unwrap_or(0))
        .sum();
    out(format!("dugix {}", env!("CARGO_PKG_VERSION")));
    out(format!("data dir:  {}", dirs.data_dir().display()));
    out(format!("config:    {}", config::config_path()?.display()));
    out(format!("db:        {}", space.db_path().display()));
    out(format!("device id: {}", db.device_id()?));
    let mark = |on: bool| if on { "✓" } else { "✗" };
    out(format!(
        "providers: openrouter {}  openai {}  opencode {}  codex {}  mistral {}",
        mark(creds.key(BackendTag::OpenRouter).is_some()),
        mark(creds.key(BackendTag::OpenAi).is_some()),
        mark(creds.key(BackendTag::OpencodeGo).is_some()),
        mark(creds.codex.is_some()),
        mark(creds.key(BackendTag::Mistral).is_some()),
    ));
    out(format!("spaces: {}   sessions: {sessions}", spaces.len()));
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(super) async fn doctor(network: bool) -> Result<()> {
    let dirs = config::project_dirs()?;
    let mut problems = 0usize;
    let mut check = |label: &str, ok: bool, detail: &str| {
        out(format!("{} {label}: {detail}", if ok { "✓" } else { "✗" }));
        if !ok {
            problems += 1;
        }
    };

    let data_dir = dirs.data_dir();
    let db_path = data_dir.join("dugix.db");
    check(
        "data dir",
        data_dir.is_dir(),
        &data_dir.display().to_string(),
    );
    check("db file", db_path.is_file(), &db_path.display().to_string());
    let integrity = db::Db::open(&db_path)
        .ok()
        .and_then(|db| db.integrity_check().ok());
    check(
        "db integrity",
        integrity.as_deref() == Some("ok"),
        integrity.as_deref().unwrap_or("unreadable"),
    );

    // A missing or empty cache is healthy because it rebuilds on demand.
    let cache_path = data_dir.join("cache.db");
    match std::fs::metadata(&cache_path) {
        Err(_) => check("cache db", true, "absent (created on next launch)"),
        Ok(meta) if meta.len() == 0 => check("cache db", true, "empty (rebuilds on demand)"),
        Ok(_) => {
            let ok = db::open_attached(&db_path)
                .ok()
                .and_then(|conn| {
                    conn.query_row("PRAGMA cache.integrity_check", [], |r| {
                        r.get::<_, String>(0)
                    })
                    .ok()
                })
                .as_deref()
                == Some("ok");
            check(
                "cache db",
                ok,
                if ok {
                    "integrity ok"
                } else {
                    "unreadable (deleting it is safe — rebuilds on demand)"
                },
            );
        }
    }

    let cfg_path = config::config_path()?;
    let cfg_ok = match std::fs::read_to_string(&cfg_path) {
        Ok(text) => toml::from_str::<toml::Value>(&text).is_ok(),
        Err(_) => false,
    };
    check(
        "config",
        cfg_ok,
        if cfg_path.exists() {
            "parse ok"
        } else {
            "missing (scaffolded on first launch)"
        },
    );

    for tool in ["ffmpeg", "ollama", "node", "npm", "cloudflared"] {
        let present = std::process::Command::new(tool)
            .arg("--version")
            .output()
            .is_ok();
        check(
            tool,
            present,
            if present {
                "found"
            } else {
                "not in PATH (optional)"
            },
        );
    }

    if network {
        let creds = config::load_creds_offline();
        for tag in BackendTag::ALL {
            let Some(login) = &tag.info().key else {
                continue;
            };
            let name = login.config_name;
            let key = creds.key(tag);
            if let Some(key) = key {
                let client = client_for(name, key);
                let ok = client.list_models().await.is_ok();
                check(name, ok, if ok { "reachable" } else { "request failed" });
            } else {
                check(name, true, "no key configured");
            }
        }
    }

    if problems > 0 {
        bail!("{problems} problem(s) found");
    }
    out("all good");
    Ok(())
}

pub(super) async fn update() -> Result<()> {
    match dugix_core::update::latest_version().await {
        Some(latest) if dugix_core::update::version_gt(&latest, dugix_core::update::CURRENT) => {
            out(format!(
                "updating v{} → v{latest} — `cargo install dugix` (this can take a few minutes)",
                dugix_core::update::CURRENT
            ));
            let status = dugix_core::update::install_now()?;
            if !status.success() {
                bail!("cargo install failed ({status}) — see the output above");
            }
            out(format!("updated to v{latest} — relaunch dugix"));
        }
        _ => out(format!("up to date — v{}", dugix_core::update::CURRENT)),
    }
    Ok(())
}

pub(super) fn open(reference: &str) -> Result<()> {
    let space = space::Space::open()?;
    let db = db::Db::open(&space.db_path())?;
    let (_, session) = resolve_session(&db, reference)?;
    std::fs::write(space.root.join("pending-open"), &session.id)?;
    let exe = std::env::current_exe().context("resolving this binary")?;
    std::process::Command::new(exe)
        .spawn()
        .context("launching the TUI")?;
    out(format!("opening \"{}\" in the TUI…", session.title));
    Ok(())
}

pub(super) fn train_export(cmd: TrainCmd) -> Result<()> {
    let TrainCmd::Export {
        format,
        filter,
        models,
        since,
        space: space_name,
        evals,
        out: output,
        split,
    } = cmd;
    let space = space::Space::open()?;
    let space_id = space_name
        .map(|name| -> Result<String> {
            let db = db::Db::open(&space.db_path())?;
            db.list_spaces()?
                .into_iter()
                .find(|space| space.name == name)
                .map(|space| space.id)
                .with_context(|| format!("no space named {name:?} — `dugix spaces` lists them"))
        })
        .transpose()?;
    let saved = config::load_creds_offline();
    let mut secrets: Vec<_> = BackendTag::ALL
        .into_iter()
        .filter_map(|tag| saved.key(tag).map(str::to_string))
        .chain(saved.host_token.clone())
        .collect();
    if let Some(codex) = saved.codex {
        secrets.extend([codex.access, codex.refresh]);
    }
    // Environment credentials remain secret even when an on-disk key takes precedence.
    secrets.extend(
        config::SECRET_ENV_VARS
            .iter()
            .filter_map(|key| std::env::var(key).ok()),
    );
    let scrubber = dugix_core::trace::Scrubber::new(secrets);
    let store = dugix_core::trace::TraceStore::open(
        &space.traces_dir(),
        dugix_core::trace::Scrubber::new(Vec::new()),
    )?;
    let options = train::ExportOptions {
        format: match format.as_str() {
            "chat-jsonl" => train::Format::ChatJsonl,
            "sharegpt" => train::Format::Sharegpt,
            "dpo" => train::Format::Dpo,
            _ => unreachable!("clap validates the format"),
        },
        filter: if filter == "success" {
            train::Filter::Success
        } else {
            train::Filter::All
        },
        models,
        since,
        space_id,
        evals,
        out: output,
        split,
    };
    // Offline export must not fetch a catalog or assume a license without persisted metadata.
    let report = train::export(
        &store,
        &options,
        &scrubber,
        &std::collections::BTreeMap::new(),
    )?;
    out(serde_json::to_string_pretty(&report)?);
    Ok(())
}
