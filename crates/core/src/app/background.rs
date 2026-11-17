//! Background sub-agents (`task background=true`) from the app's side: their
//! progress feeds `/agents` like any sub-agent, and a finished run's report
//! comes back into its conversation as a message — at once when the
//! conversation is idle, otherwise behind the reply and any queued messages.

use anyhow::Result;

use super::{App, SubagentRow, SubagentStatus};
use crate::provider::SubagentEvent;
use crate::subagent::{BackgroundMsg, Report};

impl App {
    /// Background agents of the open conversation still working.
    #[must_use]
    pub fn background_running(&self) -> usize {
        self.session
            .as_ref()
            .map_or(0, |session| self.background.running(&session.id))
    }

    pub(crate) fn on_background(&mut self, message: BackgroundMsg) {
        match message {
            BackgroundMsg::Update { session_id, update } => {
                match &update.event {
                    SubagentEvent::Started { .. } => {
                        self.background_agents.insert(update.id.clone());
                    }
                    SubagentEvent::Finished { .. } => {
                        self.background_agents.remove(&update.id);
                    }
                    _ => {}
                }
                // Its cost is logged by its own requests; no reply is left to carry it.
                let _ = self.on_subagent(&session_id, false, *update);
            }
            BackgroundMsg::Stopped { session_id, number } => {
                self.mark_background_stopped(&session_id, number);
            }
            BackgroundMsg::Finished { session_id } => {
                if let Err(error) = self.deliver_reports(&session_id) {
                    self.push_status(format!("background agent report failed: {error:#}"));
                }
            }
        }
    }

    fn mark_background_stopped(&mut self, session_id: &str, number: u64) {
        let Some(run) = self.subagent_runs.iter_mut().find(|run| {
            run.session_id == session_id
                && run.number == number
                && run.status == SubagentStatus::Running
        }) else {
            return;
        };
        run.status = SubagentStatus::Failed;
        run.finished_at = Some(chrono::Utc::now().to_rfc3339());
        run.live_reasoning.clear();
        run.live_content.clear();
        run.rows.push(SubagentRow {
            role: "error".into(),
            content: "stopped".into(),
            reasoning: None,
        });
        let run = run.clone();
        self.background_agents.remove(&run.id);
        let _ = self.db.save_subagent_run(&run);
    }

    /// Move the conversation's finished background reports into its queue as
    /// one message. `false` when there were none.
    pub(super) fn queue_reports(&mut self, session_id: &str) -> bool {
        let reports = self.background.take_reports(session_id);
        if reports.is_empty() {
            return false;
        }
        self.queued
            .entry(session_id.to_string())
            .or_default()
            .push(report_message(&reports));
        true
    }

    /// A background agent of `session_id` finished. A busy conversation gets
    /// the report when its turn ends; an idle one gets it now, unless the user
    /// is holding a queue there — then it waits its turn in that queue.
    fn deliver_reports(&mut self, session_id: &str) -> Result<()> {
        if self.session_busy(session_id) {
            return Ok(());
        }
        let held = self.queued.get(session_id).is_some_and(|q| !q.is_empty());
        if !self.queue_reports(session_id) {
            return Ok(());
        }
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        if viewing && !held {
            self.send_next_queued()?;
        } else if viewing {
            self.push_status("background agent finished — its report is queued");
        } else {
            self.unread.insert(session_id.to_string());
            let title = self
                .db
                .get_session(session_id)
                .ok()
                .flatten()
                .map_or_else(|| "another chat".to_string(), |session| session.title);
            self.push_status(format!(
                "background agent finished in: {title} — open it and press Enter for the report"
            ));
        }
        Ok(())
    }
}

/// The message a conversation receives for its finished background agents.
fn report_message(reports: &[Report]) -> String {
    reports
        .iter()
        .map(|report| {
            format!(
                "[background agent {} finished: {}]\n{}",
                report.number, report.title, report.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
