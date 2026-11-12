//! Scheduled jobs.

use anyhow::Result;

use super::Db;

impl Db {
    /// All visible jobs, including read-only jobs owned by other devices.
    pub fn list_jobs(&self) -> Result<Vec<crate::jobs::Job>> {
        let mut statement = self.conn.prepare(
            "SELECT id, space_id, session_id, device_id, name, prompt, root, mode,
             trigger_kind, interval_minutes, run_at, stop_command, max_iters,
             max_cost_usd, max_wall_minutes, notify, enabled, last_run_at, updated_at, kind, topic
             FROM jobs WHERE enabled >= 0 ORDER BY name, id",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(crate::jobs::Job {
                    id: row.get(0)?,
                    space_id: row.get(1)?,
                    session_id: row.get(2)?,
                    device_id: row.get(3)?,
                    spec: crate::jobs::JobSpec {
                        kind: row.get(19)?,
                        topic: row.get(20)?,
                        name: row.get(4)?,
                        prompt: row.get(5)?,
                        root: row.get(6)?,
                        mode: row.get(7)?,
                        trigger_kind: row.get(8)?,
                        interval_minutes: row.get(9)?,
                        run_at: row.get(10)?,
                        stop_command: row.get(11)?,
                        max_iters: row.get(12)?,
                        max_cost_usd: row.get(13)?,
                        max_wall_minutes: row.get(14)?,
                        notify: row.get(15)?,
                    },
                    enabled: row.get(16)?,
                    last_run_at: row.get(17)?,
                    updated_at: row.get(18)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Persist a validated job or update its scheduling state.
    pub fn save_job(&self, job: &crate::jobs::Job) -> Result<()> {
        let spec = &job.spec;
        self.conn.execute(
            "INSERT INTO jobs (id, space_id, session_id, device_id, name, prompt, root, mode,
             trigger_kind, interval_minutes, run_at, stop_command, max_iters, max_cost_usd,
             max_wall_minutes, notify, enabled, last_run_at, updated_at, kind, topic)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
             ON CONFLICT(id) DO UPDATE SET session_id=?3, enabled=?17,
             last_run_at=?18, updated_at=?19",
            rusqlite::params![
                job.id,
                job.space_id,
                job.session_id,
                job.device_id,
                spec.name,
                spec.prompt,
                spec.root,
                spec.mode,
                spec.trigger_kind,
                spec.interval_minutes,
                spec.run_at,
                spec.stop_command,
                spec.max_iters,
                spec.max_cost_usd,
                spec.max_wall_minutes,
                spec.notify,
                job.enabled,
                job.last_run_at,
                job.updated_at,
                spec.kind,
                spec.topic
            ],
        )?;
        Ok(())
    }

    /// Runs newest first; their ids identify the corresponding sessions.
    pub fn list_job_runs(&self) -> Result<Vec<crate::jobs::JobRun>> {
        let mut statement = self.conn.prepare(
            "SELECT id, job_id, device_id, started_at, finished_at, status, iters,
             cost_usd, branch, summary, updated_at FROM job_runs ORDER BY started_at DESC, id DESC",
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(crate::jobs::JobRun {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    device_id: row.get(2)?,
                    started_at: row.get(3)?,
                    finished_at: row.get(4)?,
                    status: row.get(5)?,
                    iters: row.get(6)?,
                    cost_usd: row.get(7)?,
                    branch: row.get(8)?,
                    summary: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Persist the current run state and final review result.
    pub fn save_job_run(&self, run: &crate::jobs::JobRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO job_runs VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(id) DO UPDATE SET finished_at=?5, status=?6, iters=?7,
             cost_usd=?8, branch=?9, summary=?10, updated_at=?11",
            rusqlite::params![
                run.id,
                run.job_id,
                run.device_id,
                run.started_at,
                run.finished_at,
                run.status,
                run.iters,
                run.cost_usd,
                run.branch,
                run.summary,
                run.updated_at
            ],
        )?;
        Ok(())
    }
}
