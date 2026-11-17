use anyhow::{Context, Result, ensure};
use chrono::Utc;
use tokio::sync::watch;

use super::{Job, JobRun, JobSpec};
use crate::app::{App, AppEvent, ResearchUpdate};

pub(crate) fn spec(topic: &str, interval_minutes: Option<i64>) -> JobSpec {
    let topic = topic.trim();
    JobSpec {
        kind: "research".into(),
        topic: Some(topic.into()),
        name: topic.into(),
        prompt: topic.into(),
        trigger_kind: if interval_minutes.is_some() {
            "interval"
        } else {
            "manual"
        }
        .into(),
        interval_minutes: interval_minutes.unwrap_or(1440),
        notify: "on_change".into(),
        ..JobSpec::default()
    }
}

pub(crate) fn started(app: &App, job_id: &str, session_id: &str) -> Result<()> {
    let Some(job) = app.db.list_jobs()?.into_iter().find(|job| job.id == job_id) else {
        return Ok(());
    };
    let now = Utc::now().to_rfc3339();
    app.db.save_job_run(&JobRun {
        id: session_id.into(),
        job_id: job.id,
        device_id: job.device_id,
        started_at: now.clone(),
        finished_at: None,
        status: "running".into(),
        iters: 1,
        cost_usd: 0.0,
        branch: None,
        summary: String::new(),
        updated_at: now.clone(),
    })?;
    app.db.conn().execute(
        "UPDATE jobs SET session_id = ?2, last_run_at = ?3, updated_at = ?3 WHERE id = ?1",
        (job_id, session_id, now),
    )?;
    Ok(())
}

pub(crate) async fn completed(
    app: &mut App,
    session: &str,
    result: &Result<String, String>,
) -> Result<()> {
    let Some(mut run) = app
        .db
        .list_job_runs()?
        .into_iter()
        .find(|run| run.id == session && run.finished_at.is_none())
    else {
        return Ok(());
    };
    match result {
        Ok(report) => {
            run.status = "done".into();
            run.summary.clone_from(report);
            if app
                .db
                .list_jobs()?
                .iter()
                .any(|job| job.id == run.job_id && job.spec.notify == "on_change")
            {
                super::judging::on_change(app, &run, "").await?;
            }
        }
        Err(error) => {
            run.status = "failed".into();
            run.summary.clone_from(error);
        }
    }
    finish(app, &mut run)
}

fn finish(app: &App, run: &mut JobRun) -> Result<()> {
    run.cost_usd = app.db.conn().query_row(
        "SELECT COALESCE((SELECT SUM(cost) FROM usage_log WHERE session_id = ?1), 0)
         + COALESCE((SELECT SUM(cost) FROM cache.decisions WHERE session_id = ?1 AND tool = 'job'), 0)",
        [&run.id], |row| row.get(0),
    )?;
    run.finished_at = Some(Utc::now().to_rfc3339());
    run.updated_at = run.finished_at.clone().unwrap_or_default();
    app.db.save_job_run(run)
}

pub(super) async fn run(
    app: &mut App,
    job: &Job,
    cancelled: &mut watch::Receiver<bool>,
) -> Result<JobRun> {
    // Research uses the ordinary space toolbox, never an agent worktree.
    app.workspace = None;
    app.work_mode = crate::app::WorkMode::Chat;
    app.approvals.job_approver = None;
    app.refresh_toolbox();
    ensure!(app.run_research_job(job), "research job could not start");
    let mut run = app
        .db
        .list_job_runs()?
        .into_iter()
        .find(|run| run.job_id == job.id && run.finished_at.is_none())
        .context("research run missing")?;
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(u64::from(job.spec.max_wall_minutes) * 60);
    loop {
        tokio::select! {
            event = app.next_event() => match event {
                AppEvent::Research(Some((session_id, space_id, space_name, update))) => {
                    app.on_research_done(Some((session_id, space_id, space_name, update)));
                }
                AppEvent::Research(None) => {
                    app.on_research_done(None);
                    break;
                },
                _ => {},
            },
            () = tokio::time::sleep_until(deadline) => {
                run.status = "budget".into();
                break;
            }
            _ = cancelled.changed() => {
                run.status = "cancelled".into();
                break;
            }
        }
        if let Some(saved) = app
            .db
            .list_job_runs()?
            .into_iter()
            .find(|saved| saved.id == run.id && saved.finished_at.is_some())
        {
            app.on_research_done(None);
            return Ok(saved);
        }
    }
    app.stop_research();
    if run.status == "running" {
        run.status = "failed".into();
        run.summary = "research ended without a report".into();
    }
    finish(app, &mut run)?;
    Ok(run)
}

pub(crate) async fn event(app: &mut App, event: &crate::app::ResearchMsg) {
    if let (session, _, _, ResearchUpdate::Done(result)) = event
        && let Err(error) = completed(app, session, result).await
    {
        app.push_status(format!("could not finish research job: {error}"));
    }
}
