//! `dugix chat`, `dugix research` and `dugix jobs`.

use anyhow::{Context as _, Result, bail};

use dugix_core::app;
use dugix_core::db;
use dugix_core::space;

use super::ask::build_app;
use super::{JobsCmd, out, short_id};

pub(super) async fn chat(
    model: Option<String>,
    space_name: Option<String>,
    quiet: bool,
) -> Result<()> {
    let mut app = build_app(model.as_deref(), space_name.as_deref()).await?;
    app.chat_headless(quiet).await
}

pub(super) async fn research(
    topic: String,
    model: Option<String>,
    space_name: Option<String>,
    approve: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    if topic.trim().is_empty() {
        bail!("empty topic — pass what to research as the first argument");
    }
    let mut app = build_app(model.as_deref(), space_name.as_deref()).await?;
    let outcome = app
        .research_headless(
            topic,
            approve,
            app::headless::TurnOpts {
                stream: false,
                quiet,
            },
        )
        .await?;
    if json {
        let v = serde_json::json!({
            "report": outcome.report,
            "session_id": outcome.session_id,
            "session_title": outcome.session_title,
        });
        out(serde_json::to_string_pretty(&v)?);
    } else {
        out(outcome.report);
        if !quiet {
            eprintln!(
                "saved as \"{}\" — reopen it in the TUI with /sessions (id {})",
                outcome.session_title,
                short_id(&outcome.session_id)
            );
        }
    }
    Ok(())
}

pub(super) async fn jobs(cmd: JobsCmd) -> Result<()> {
    use dugix_core::jobs;

    match cmd {
        JobsCmd::InstallService => {
            let path = jobs::service::install_service()?;
            out(format!("wrote {}", path.display()));
            if cfg!(target_os = "macos") {
                out(format!(
                    "activate: launchctl bootstrap gui/$(id -u) '{}'",
                    path.display()
                ));
            } else {
                out(
                    "activate: systemctl --user daemon-reload && systemctl --user enable --now dugix-jobs.service",
                );
            }
        }
        JobsCmd::UninstallService => {
            out(
                "stop the loaded service first: launchctl bootout gui/$(id -u)/chat.dugix.jobs (macOS), or systemctl --user disable --now dugix-jobs.service (Linux)",
            );
            out(format!(
                "service file removed: {}",
                jobs::service::uninstall_service()?
            ));
        }
        JobsCmd::List => {
            let space = space::Space::open()?;
            let db = db::Db::open(&space.db_path())?;
            let device = db.device_id()?;
            for job in db.list_jobs()? {
                out(format!(
                    "{} · {} · {} · {}{}",
                    job.id,
                    job.spec.name,
                    job.spec.trigger_kind,
                    if job.enabled == 1 {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    if job.device_id == device {
                        ""
                    } else {
                        " · other device (read-only)"
                    }
                ));
            }
        }
        JobsCmd::Run { id } => {
            let mut app = build_app(None, None).await?;
            let job = app
                .db
                .list_jobs()?
                .into_iter()
                .find(|job| job.id == id)
                .context("job not found")?;
            anyhow::ensure!(
                job.device_id == app.db.device_id()?,
                "job belongs to another device"
            );
            let Ok(_lock) = jobs::DaemonLock::acquire(&app.space.root) else {
                jobs::request_run(&app.space.root, &job)?;
                out("job queued for the active daemon");
                return Ok(());
            };
            let run = jobs::run_with_app(&mut app, &job).await?;
            out(format!("{} · {}\n{}", run.id, run.status, run.summary));
        }
        JobsCmd::RunDue => {
            let mut app = build_app(None, None).await?;
            for run in jobs::run_due(&mut app).await? {
                out(format!("{} · {}\n{}", run.id, run.status, run.summary));
            }
        }
        JobsCmd::Daemon => jobs::daemon(&mut build_app(None, None).await?).await?,
    }
    Ok(())
}
