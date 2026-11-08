//! What each `task` sub-agent did, for the sub-agent view: its prompt, every
//! round's reasoning and tool calls, its answer, and what it cost. Runs live
//! in memory while they stream and are saved in the device-local cache
//! (never in incognito), so a finished session's sub-agents can be read back.

use serde::{Deserialize, Serialize};

use super::App;
use crate::provider::{SubagentEvent, SubagentUpdate};

/// Runs kept in memory across sessions; older ones are read from the cache.
const LIVE_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStatus {
    Running,
    Done,
    Failed,
}

impl SubagentStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Self {
        // A run saved as running belonged to a process that is gone.
        if text == "done" {
            Self::Done
        } else {
            Self::Failed
        }
    }
}

/// One transcript row: a `tool_call` (the same JSON as a chat tool row), an
/// `assistant` answer with its reasoning, or an `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubagentRow {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubagentRun {
    pub id: String,
    /// The conversation whose turn started it.
    pub session_id: String,
    pub number: u64,
    pub prompt: String,
    pub model: String,
    pub writable: bool,
    pub max_steps: usize,
    pub status: SubagentStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// Model rounds finished so far.
    pub steps: usize,
    pub tokens: u64,
    /// USD, provider-reported or estimated from the catalog; `None` while unknown.
    pub cost: Option<f64>,
    /// Time spent in model rounds.
    pub secs: f64,
    /// A workspace task's branch summary and diff.
    pub review: Option<String>,
    pub rows: Vec<SubagentRow>,
    /// Text of the round in flight, cleared when the round lands.
    pub live_reasoning: String,
    pub live_content: String,
}

impl SubagentRun {
    /// The prompt's first line, for lists.
    #[must_use]
    pub fn title(&self) -> &str {
        self.prompt.lines().next().unwrap_or_default().trim()
    }

    /// A finished round's rows: its answer, or one row per tool call with the
    /// round's reasoning and words leading the first, as in a chat transcript.
    fn push_round(
        &mut self,
        reasoning: Option<String>,
        content: String,
        calls: Vec<(crate::provider::ToolCall, String)>,
    ) {
        let reasoning = reasoning.filter(|text| !text.trim().is_empty());
        if calls.is_empty() {
            self.rows.push(SubagentRow {
                role: "assistant".into(),
                content,
                reasoning,
            });
            return;
        }
        let mut lead = Some((reasoning, content));
        for (call, result) in calls {
            let (reasoning, said) = lead.take().unwrap_or_default();
            self.rows.push(SubagentRow {
                role: "tool_call".into(),
                content: serde_json::json!({
                    "id": call.id,
                    "reasoning_content": reasoning,
                    "assistant_content": (!said.trim().is_empty()).then_some(said),
                    "name": call.name,
                    "arguments": call.arguments,
                    "result": result,
                })
                .to_string(),
                reasoning: None,
            });
        }
    }
}

/// Add a round's usage to a running total, keeping "unknown" only while
/// nothing is known.
fn add_cost(total: Option<f64>, cost: Option<f64>) -> Option<f64> {
    match (total, cost) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    }
}

impl App {
    /// Fold one sub-agent event into its run, saving it unless the turn is
    /// incognito. Returns the round's cost, which belongs to the parent turn.
    pub(crate) fn on_subagent(
        &mut self,
        session_id: &str,
        incognito: bool,
        update: SubagentUpdate,
    ) -> Option<f64> {
        let SubagentUpdate { id, number, event } = update;
        let now = || chrono::Utc::now().to_rfc3339();
        if let SubagentEvent::Started {
            prompt,
            model,
            writable,
            max_steps,
        } = &event
        {
            self.start_subagent(SubagentRun {
                id: id.clone(),
                session_id: session_id.to_string(),
                number,
                prompt: prompt.clone(),
                model: model.clone(),
                writable: *writable,
                max_steps: *max_steps,
                status: SubagentStatus::Running,
                started_at: now(),
                finished_at: None,
                steps: 0,
                tokens: 0,
                cost: None,
                secs: 0.0,
                review: None,
                rows: Vec::new(),
                live_reasoning: String::new(),
                live_content: String::new(),
            });
        }
        let run = self.subagent_runs.iter_mut().find(|run| run.id == id)?;
        let mut round_cost = None;
        match event {
            SubagentEvent::Started { .. } => {}
            SubagentEvent::Delta { reasoning, content } => {
                run.live_reasoning.push_str(&reasoning);
                run.live_content.push_str(&content);
                // Streamed text is not saved; the round that follows is.
                return None;
            }
            SubagentEvent::Round {
                step,
                reasoning,
                content,
                calls,
                usage,
                secs,
            } => {
                run.live_reasoning.clear();
                run.live_content.clear();
                run.steps = step + 1;
                run.secs += secs;
                if let Some(usage) = usage {
                    run.tokens += if usage.total_tokens > 0 {
                        usage.total_tokens
                    } else {
                        usage.prompt_tokens + usage.completion_tokens
                    };
                    round_cost = usage.cost.or_else(|| {
                        self.db.request_cost(
                            &run.model,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            usage.cache_read_tokens,
                            usage.cache_creation_tokens,
                        )
                    });
                    run.cost = add_cost(run.cost, round_cost);
                }
                run.push_round(reasoning, content, calls);
            }
            SubagentEvent::Finished { error, review } => {
                run.live_reasoning.clear();
                run.live_content.clear();
                run.finished_at = Some(now());
                run.review = review;
                run.status = if let Some(error) = error {
                    run.rows.push(SubagentRow {
                        role: "error".into(),
                        content: error,
                        reasoning: None,
                    });
                    SubagentStatus::Failed
                } else {
                    SubagentStatus::Done
                };
            }
        }
        if !incognito {
            let run = run.clone();
            let _ = self.db.save_subagent_run(&run);
        }
        round_cost
    }

    /// Keep a new run, dropping the oldest finished one past [`LIVE_LIMIT`].
    fn start_subagent(&mut self, run: SubagentRun) {
        if self.subagent_runs.len() >= LIVE_LIMIT
            && let Some(done) = self
                .subagent_runs
                .iter()
                .position(|run| run.status != SubagentStatus::Running)
        {
            self.subagent_runs.remove(done);
        }
        self.subagent_runs.push(run);
    }

    /// A turn ended: any of its sub-agents still marked running were cut off
    /// with it (a stopped reply aborts its tool calls).
    pub(crate) fn end_subagents(&mut self, session_id: &str, incognito: bool) {
        for run in self
            .subagent_runs
            .iter_mut()
            .filter(|run| run.session_id == session_id && run.status == SubagentStatus::Running)
        {
            run.status = SubagentStatus::Failed;
            run.finished_at = Some(chrono::Utc::now().to_rfc3339());
            run.live_reasoning.clear();
            run.live_content.clear();
            run.rows.push(SubagentRow {
                role: "error".into(),
                content: "stopped with the reply".into(),
                reasoning: None,
            });
            if !incognito {
                let _ = self.db.save_subagent_run(run);
            }
        }
    }

    /// The session's sub-agent runs, oldest first: saved ones from the cache
    /// with this process's live state laid over them.
    #[must_use]
    pub fn subagents_for(&self, session_id: &str) -> Vec<SubagentRun> {
        self.with_live_subagents(
            self.db.subagent_runs(session_id).unwrap_or_default(),
            session_id,
        )
    }

    /// `saved` runs of a session with this process's live state laid over
    /// them and its unsaved runs added, oldest first. No db read, so a view
    /// can call it every frame.
    #[must_use]
    pub fn with_live_subagents(
        &self,
        mut runs: Vec<SubagentRun>,
        session_id: &str,
    ) -> Vec<SubagentRun> {
        for live in self
            .subagent_runs
            .iter()
            .filter(|run| run.session_id == session_id)
        {
            match runs.iter_mut().find(|run| run.id == live.id) {
                Some(saved) => *saved = live.clone(),
                None => runs.push(live.clone()),
            }
        }
        runs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        runs
    }

    /// The live state of one run, when this process has it.
    #[must_use]
    pub fn live_subagent(&self, id: &str) -> Option<&SubagentRun> {
        self.subagent_runs.iter().find(|run| run.id == id)
    }
}

#[cfg(test)]
#[path = "subagents_tests.rs"]
mod tests;
