//! Watches and scheduled jobs.

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use super::{Db, Watch};

impl Db {
    pub fn create_watch(
        &self,
        space_id: &str,
        topic: &str,
        interval_hours: i64,
        session_id: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO watches (id, space_id, topic, interval_hours, session_id, last_run_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            (&id, space_id, topic, interval_hours, session_id, &now),
        )?;
        self.migrate_watch_jobs()?;
        Ok(id)
    }

    /// Keep the legacy watch sync surface unchanged. Reusing the watch UUID
    /// makes migration idempotent even after cache loss or a sync import.
    pub fn migrate_watch_jobs(&self) -> Result<()> {
        let device = self.device_id()?;
        self.conn.execute(
            "INSERT OR IGNORE INTO jobs
             (id, space_id, session_id, device_id, name, prompt, root, mode,
              trigger_kind, interval_minutes, max_iters, max_cost_usd, max_wall_minutes,
              notify, enabled, last_run_at, updated_at, kind, topic)
             SELECT id, space_id, session_id, ?1, topic, topic, '', 'agent',
              'interval', interval_hours * 60, 5, 1.0, 30, 'on_change', 1,
              last_run_at, COALESCE(updated_at, ?2), 'research', topic FROM watches",
            (&device, Utc::now().to_rfc3339()),
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO cache.watch_jobs (watch_id, job_id) SELECT id, id FROM watches",
            [],
        )?;
        Ok(())
    }

    pub fn list_watches(&self, space_id: &str) -> Result<Vec<Watch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, space_id, topic, interval_hours, session_id, last_run_at
             FROM watches WHERE space_id = ?1 ORDER BY topic",
        )?;
        let rows = stmt.query_map([space_id], |r| {
            Ok(Watch {
                id: r.get(0)?,
                space_id: r.get(1)?,
                topic: r.get(2)?,
                interval_hours: r.get(3)?,
                session_id: r.get(4)?,
                last_run_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// All visible jobs, including read-only jobs owned by other devices.
    pub fn list_jobs(&self) -> Result<Vec<crate::jobs::Job>> {
        self.migrate_watch_jobs()?;
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

    /// Every watch across all spaces — used by the startup due-check, which
    /// runs before any space is necessarily "active".
    pub fn list_all_watches(&self) -> Result<Vec<Watch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, space_id, topic, interval_hours, session_id, last_run_at FROM watches",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Watch {
                id: r.get(0)?,
                space_id: r.get(1)?,
                topic: r.get(2)?,
                interval_hours: r.get(3)?,
                session_id: r.get(4)?,
                last_run_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn touch_watch(&self, id: &str, now_rfc3339: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE watches SET last_run_at = ?2, updated_at = ?3 WHERE id = ?1",
            (id, now_rfc3339, Utc::now().to_rfc3339()),
        )?;
        self.conn.execute(
            "UPDATE jobs SET last_run_at = ?2, updated_at = ?2 WHERE id = ?1 AND kind = 'research'",
            (id, now_rfc3339),
        )?;
        Ok(())
    }

    /// Change a watch cadence while preserving its current run timestamp.
    pub fn set_watch_interval(&self, id: &str, interval_hours: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE watches SET interval_hours = ?2, updated_at = ?3 WHERE id = ?1",
            (id, interval_hours, Utc::now().to_rfc3339()),
        )?;
        self.conn.execute(
            "UPDATE jobs SET interval_minutes = ?2 * 60, updated_at = ?3 WHERE id = ?1 AND kind = 'research'",
            (id, interval_hours, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    /// Repoint a watch at the session its most recent re-run actually used,
    /// so the next due-check's diff-section lookup
    /// (`previous_citations_for_watch_session`) can match against it.
    pub fn set_watch_session(&self, id: &str, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE watches SET session_id = ?2, updated_at = ?3 WHERE id = ?1",
            (id, session_id, Utc::now().to_rfc3339()),
        )?;
        self.conn.execute(
            "UPDATE jobs SET session_id = ?2, updated_at = ?3 WHERE id = ?1 AND kind = 'research'",
            (id, session_id, Utc::now().to_rfc3339()),
        )?;
        Ok(())
    }

    pub fn delete_watch(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET enabled = -1, updated_at = ?2 WHERE id = ?1 AND kind = 'research'",
            (id, Utc::now().to_rfc3339()),
        )?;
        self.conn
            .execute("DELETE FROM watches WHERE id = ?1", [id])?;
        self.tombstone("watches", id)?;
        Ok(())
    }
}
