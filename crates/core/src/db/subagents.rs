//! Sub-agent transcripts in the device-local cache (see `app::subagents`).

use anyhow::Result;

use super::Db;
use crate::app::{SubagentRun, SubagentStatus};

impl Db {
    /// Insert or replace a run in the device-local cache.
    pub fn save_subagent_run(&self, run: &SubagentRun) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO cache.subagent_runs
             (id, session_id, number, prompt, model, writable, max_steps, status,
              started_at, finished_at, steps, tokens, cost, secs, review, rows)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            rusqlite::params![
                run.id,
                run.session_id,
                i64::try_from(run.number).unwrap_or(i64::MAX),
                run.prompt,
                run.model,
                run.writable,
                i64::try_from(run.max_steps).unwrap_or(i64::MAX),
                run.status.as_str(),
                run.started_at,
                run.finished_at,
                i64::try_from(run.steps).unwrap_or(i64::MAX),
                i64::try_from(run.tokens).unwrap_or(i64::MAX),
                run.cost,
                run.secs,
                run.review,
                serde_json::to_string(&run.rows)?,
            ],
        )?;
        Ok(())
    }

    /// A session's saved runs, oldest first.
    pub fn subagent_runs(&self, session_id: &str) -> Result<Vec<SubagentRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, number, prompt, model, writable, max_steps, status,
                    started_at, finished_at, steps, tokens, cost, secs, review, rows
             FROM cache.subagent_runs WHERE session_id = ?1 ORDER BY started_at",
        )?;
        let runs = stmt.query_map([session_id], |r| {
            let rows: String = r.get(15)?;
            Ok(SubagentRun {
                id: r.get(0)?,
                session_id: r.get(1)?,
                number: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                prompt: r.get(3)?,
                model: r.get(4)?,
                writable: r.get(5)?,
                max_steps: usize::try_from(r.get::<_, i64>(6)?).unwrap_or(0),
                status: SubagentStatus::parse(&r.get::<_, String>(7)?),
                started_at: r.get(8)?,
                finished_at: r.get(9)?,
                steps: usize::try_from(r.get::<_, i64>(10)?).unwrap_or(0),
                tokens: u64::try_from(r.get::<_, i64>(11)?).unwrap_or(0),
                cost: r.get(12)?,
                secs: r.get(13)?,
                review: r.get(14)?,
                rows: serde_json::from_str(&rows).unwrap_or_default(),
                live_reasoning: String::new(),
                live_content: String::new(),
            })
        })?;
        Ok(runs.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
