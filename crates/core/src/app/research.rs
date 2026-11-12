//! Deep research: a background multi-agent pipeline triggered by `/research`.
//! Every stage but the Searcher fan-out is a single `Provider::complete`
//! call; parsing/prompt-building here is pure and unit tested. The async
//! orchestration calls real network endpoints and is exercised
//! manually, like every other network-calling background job in this
//! codebase (`maybe_generate_title`, image description, embedding).

// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

mod parse;
mod pipeline;
mod prompts;

pub use parse::*;
pub use pipeline::*;
pub use prompts::SEARCHER_PROMPT;

#[cfg(test)]
use prompts::*;

use crate::provider::ChatMessage;
#[cfg(test)]
use crate::provider::openrouter::OpenRouter;
use crate::tools::ToolBox;
use std::fmt::Write as _;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::{AbortOnDrop, ResearchMsg, SurveyGate, SurveyPhase};

/// A background research pipeline update: a phase label (+ progress detail),
/// the survey's clarifying questions awaiting a chat reply, the Planner's
/// sub-questions awaiting approval, or the final report/error.
#[derive(Clone)]
pub enum ResearchUpdate {
    /// Successive updates within one stage share a `label` so the UI/db
    /// replace one row in place instead of appending per tick.
    Stage {
        label: String,
        detail: String,
    },
    /// The scoping agent's clarifying questions; the pipeline is parked
    /// awaiting a chat reply (`reply_to_survey_gate`). `round` is 1-based
    /// (max `MAX_SURVEY_ROUNDS`).
    SurveyReady {
        questions: Vec<String>,
        round: u8,
    },
    /// The Planner finished; the pipeline is parked awaiting a chat reply:
    /// "approve" runs the questions, edits get folded in by the approval
    /// agent and re-presented (`rework = true`) once, capped.
    PlanReady {
        questions: Vec<PlanQuestion>,
        rework: bool,
    },
    Done(std::result::Result<String, String>),
}

/// Hard cap on Planner-generated sub-questions per outer round.
const MAX_SUBQUESTIONS: usize = 6;
/// Hard bound on queued `/steer` instructions (and thus on retained steer
/// text and the unbounded channel) — beyond this, new steers are refused
/// with a status message until the next round boundary drains the queue.
const MAX_QUEUED_STEERS: usize = 64;
/// Cap on the scoping agent's clarifying questions per round.
const MAX_SURVEY_QUESTIONS: usize = 4;
/// Max survey rounds (initial + follow-ups) before the survey force-completes.
pub const MAX_SURVEY_ROUNDS: u8 = 3;
/// Tool-call budget for a single Searcher agent — a few search→fetch hops,
/// not a whole interactive conversation's worth.
pub const RESEARCH_SEARCHER_MAX_ITERS: usize = 6;

/// One Planner sub-question with its supporting brief: why it matters, the
/// angles to cover, and promising source types/leads. The whole block is
/// handed to its Searcher agent as the prompt — detail is functional.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct PlanQuestion {
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub angles: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

impl PlanQuestion {
    /// A question with no brief — the fallback shape when the Planner didn't
    /// follow the JSON-object instructions.
    pub const fn bare(question: String) -> Self {
        Self {
            question,
            why: String::new(),
            angles: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// The Searcher's prompt: the topic plus this question's full block
    /// (why/angles/sources), so one focused agent answers one focused brief.
    pub fn prompt(&self, topic: &str) -> String {
        let mut p = format!(
            "Research topic: {topic}\n\nSub-question: {}\n",
            self.question
        );
        if !self.why.is_empty() {
            let _ = writeln!(p, "\nWhy this angle matters: {}", self.why);
        }
        if !self.angles.is_empty() {
            let _ = write!(p, "\nAngles to cover: {}\n", self.angles.join("; "));
        }
        if !self.sources.is_empty() {
            let _ = write!(p, "\nSource leads: {}\n", self.sources.join("; "));
        }
        p
    }
}

/// A plan rendered for the transcript / plan file: numbered questions, each
/// with its Why/Angles/Sources brief indented under it.
pub fn plan_text(questions: &[PlanQuestion]) -> String {
    questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let mut s = format!("{}. {}", i + 1, q.question);
            if !q.why.is_empty() {
                let _ = write!(s, "\n   Why: {}", q.why);
            }
            if !q.angles.is_empty() {
                let _ = write!(s, "\n   Angles: {}", q.angles.join("; "));
            }
            if !q.sources.is_empty() {
                let _ = write!(s, "\n   Sources: {}", q.sources.join("; "));
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl super::App {
    /// `/research <topic>`: run the multi-agent research pipeline in a new
    /// background session. One job at a time. `/research! <topic>` skips the
    /// plan-approval gate.
    pub fn start_research(&mut self, topic: &str) {
        self.start_research_with_gate(topic, true);
    }

    /// `/steer <text>`: queue an extra instruction for the running research
    /// job, picked up at the next round boundary. No-op with a status message
    /// if no research job is running.
    pub fn steer_research(&mut self, text: &str) {
        if text.is_empty() {
            self.push_status("usage: /steer <what to also look into>".to_string());
            return;
        }
        match &self.research.steer_tx {
            // Hard bound: refuse once the queue is full — this also bounds
            // the unbounded channel and the retained log.
            Some(_) if self.research.steer_log.len() >= MAX_QUEUED_STEERS => {
                self.push_status(format!(
                    "steer queue full ({MAX_QUEUED_STEERS} pending) — wait for the next round"
                ));
            }
            Some(tx) if tx.send(text.to_string()).is_ok() => {
                // Keep a log so the live popup can show what's queued vs.
                // already picked up by the pipeline. Entries carry their
                // 1-based queue position (positions are never renumbered, so
                // acknowledged entries can be dropped without shifting the
                // popup's view), and entries the pipeline has drained are
                // removed here — a long job can't retain unbounded steer
                // text without backpressure.
                let pos = self
                    .research
                    .steer_acked
                    .iter()
                    .chain(self.research.steer_log.iter().map(|(p, _)| p))
                    .max()
                    .map_or(0, |&p| p)
                    + 1;
                self.research.steer_log.push((pos, text.to_string()));
                self.research
                    .steer_log
                    .retain(|(p, _)| !self.research.steer_acked.contains(p));
                self.push_status(format!("queued steer: {text}"));
            }
            _ => self.push_status("no research job is running".to_string()),
        }
    }

    /// `/research` with no topic: distill one from the last ~20 chat turns
    /// (one cheap completion, same background-channel shape as
    /// `maybe_generate_title`) then hand it to `start_research_with_gate`
    /// exactly as if it had been typed — the existing plan-approval gate
    /// still lets you bail or edit before searchers run.
    pub fn start_research_from_chat(&mut self) {
        if self.research.topic_rx.is_some() {
            self.push_status("already scoping a topic from this chat…".to_string());
            return;
        }
        if self.research.rx.is_some() {
            self.push_status("a research job is already running".to_string());
            return;
        }
        let Some(model) = self.current_model.clone() else {
            self.push_status("no model configured — set one in /login or /model".to_string());
            return;
        };
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            self.push_status(format!(
                "model backend unavailable: {model} — pick another with /model"
            ));
            return;
        };
        let convo: String = self
            .messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|m| {
                format!(
                    "{}: {}",
                    m.role,
                    m.content.chars().take(500).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if convo.trim().is_empty() {
            self.push_status(
                "nothing to scope yet — chat first, or use /research <topic>".to_string(),
            );
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.research.topic_rx = Some(rx);
        let provider = self.usage_provider(provider, "research:topic");
        self.push_status("scoping a research topic from this chat…".to_string());
        tokio::spawn(async move {
            let prompt = prompts::render(
                prompts::RESEARCH_TOPIC_FROM_CHAT,
                &[("conversation", &convo)],
            );
            let msgs = vec![ChatMessage::text("user", prompt)];
            let result = provider
                .complete(&raw_model, msgs)
                .await
                .map(|s| s.trim().to_string())
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    /// The topic-distillation job finished: start research with it, or
    /// report the failure. `None` = channel closed without a result.
    pub fn on_research_topic_derived(&mut self, r: Option<Result<String, String>>) {
        self.research.topic_rx = None;
        let Some(result) = r else { return };
        match result {
            Ok(topic) if !topic.is_empty() => self.start_research_with_gate(&topic, true),
            Ok(_) => {
                self.push_status("couldn't derive a topic — try /research <topic>".to_string());
            }
            Err(e) => self.push_status(format!("topic scoping failed: {e}")),
        }
    }

    // Long by design (gate setup + mirroring).
    #[allow(clippy::too_many_lines)]
    pub fn start_research_with_gate(&mut self, topic: &str, gated: bool) {
        let topic = topic.trim().to_string();
        if topic.is_empty() {
            self.push_status("usage: /research <topic>".to_string());
            return;
        }
        if self.research.rx.is_some() {
            self.push_status("a research job is already running".to_string());
            return;
        }
        // Research inherits the session's model (or the global default) —
        // there is no separate researcher-model setting.
        let Some(model) = self.current_model.clone() else {
            self.push_status("no model configured — set one in /login or /model".to_string());
            return;
        };
        let Some((provider, raw_research_model)) = self.resolve_model_backend(&model) else {
            self.push_status(format!(
                "model backend unavailable: {model} — pick another with /model"
            ));
            return;
        };
        let title = super::chat::title_from(&topic);
        // Hygiene: no gate or reply channel from a previous job may linger.
        self.set_survey_gate(None);
        self.research.survey_reply_tx = None;

        // Check if there's a conversation to migrate to the research session.
        let parent_id = self.session.as_ref().and_then(|s| {
            let has_content = self
                .messages
                .iter()
                .any(|m| m.role == "user" || m.role == "assistant");
            if has_content {
                Some(s.id.clone())
            } else {
                None
            }
        });
        let parent_title = parent_id
            .as_ref()
            .and_then(|pid| self.db.get_session(pid).ok()?.map(|s| s.title));

        let session =
            match self
                .db
                .create_session(&title, &model, &self.active_space.id, "research")
            {
                Ok(s) => s,
                Err(e) => {
                    self.push_status(format!("could not start research session: {e}"));
                    return;
                }
            };

        if let Some(ref pid) = parent_id {
            let _ = self.db.set_research_parent(&session.id, pid);

            // Build compacted context from the original conversation
            let compact_summary = self
                .session
                .as_ref()
                .and_then(|s| s.compact_summary.clone());
            let compact_through = self
                .session
                .as_ref()
                .map_or(0, |s| s.compact_through as usize);
            let mut ctx = String::new();
            if let Some(ref summary) = compact_summary {
                ctx.push_str("Previous conversation summary:\n");
                ctx.push_str(summary);
            }
            let tail: Vec<&crate::db::Message> = self.messages[compact_through..]
                .iter()
                .filter(|m| m.role == "user" || m.role == "assistant")
                .collect();
            if !tail.is_empty() {
                if !ctx.is_empty() {
                    ctx.push_str("\n\n");
                }
                ctx.push_str("Recent messages:\n");
                for m in tail {
                    let t = m.content.chars().take(300).collect::<String>();
                    let _ = writeln!(ctx, "{}: {t}", m.role);
                }
            }
            let msg = if ctx.is_empty() {
                format!("/research {topic}")
            } else {
                format!("/research {topic}\n\n{ctx}")
            };
            let _ = self.db.add_user_message(&session.id, &msg);

            // Link message in the original session
            let _ = self.db.insert_message(
                pid,
                "session_link",
                &format!("{}\n🔗 Research session started for: {topic}", session.id),
                None,
                None,
                None,
                None,
                None,
                None,
            );

            // Link message in the research session back to the original
            let back_title = parent_title.as_deref().unwrap_or("previous chat");
            let _ = self.db.insert_message(
                &session.id,
                "session_link",
                &format!("{pid}\n↩ Originally from: {back_title}"),
                None,
                None,
                None,
                None,
                None,
                None,
            );

            self.messages = self.db.load_messages(&session.id).unwrap_or_default();
        } else {
            let _ = self
                .db
                .add_user_message(&session.id, &format!("/research {topic}"));
            self.messages = self.db.load_messages(&session.id).unwrap_or_default();
        }

        let searxng_url = (!self.search.searxng_url.trim().is_empty())
            .then(|| self.search.searxng_url.trim().to_string());
        let langsearch_key = (!self.search.langsearch_key.trim().is_empty())
            .then(|| self.search.langsearch_key.trim().to_string());
        let mut toolbox = ToolBox::research(
            searxng_url,
            langsearch_key,
            self.search.provider.clone(),
            self.blocked_domains(),
            Some(self.space.db_path()),
        );
        toolbox.search_backend = self.session_backend();
        toolbox.search_space_id.clone_from(&self.active_space.id);
        toolbox.session_id.clone_from(&session.id);
        toolbox.record_events = !self.incognito;
        let toolbox = Arc::new(toolbox);

        let (tx, rx) = mpsc::unbounded_channel();
        self.research.rx = Some(rx);
        self.research.running = Some((session.id.clone(), topic.clone()));
        self.push_status(format!("researching: {topic} · Ctrl+↑ agents"));

        let (steer_tx, steer_rx) = mpsc::unbounded_channel();
        self.research.steer_tx = Some(steer_tx);
        self.research.steer_log.clear();
        self.research.steer_acked.clear();
        self.research.stage_rows.clear();
        // Capture the mode at job start: plan-message and artifact
        // persistence follow this, never a mid-job incognito toggle.
        self.research.incognito = self.incognito;

        // The conversational gates (survey + plan approval) ride one reply
        // channel: the pipeline parks on `reply_rx`, the App arms a gate on
        // each SurveyReady/PlanReady. Ungated (`/research!`, watches) skips
        // both phases entirely.
        let reply_rx = if gated {
            let (reply_tx, reply_rx) = mpsc::unbounded_channel();
            self.research.survey_reply_tx = Some(reply_tx);
            Some(reply_rx)
        } else {
            None
        };

        let space_id = self.active_space.id.clone();
        let space_name = self.active_space.name.clone();
        self.session = Some(session.clone());
        self.mask_through = 0;
        self.reset_work_mode();
        self.refresh_memory_snapshot();
        let prompt_cache_key = format!("research:{}", self.prompt_cache_key_for(&session.id));
        self.meter.total = None;
        // Selection + scroll point into the previous session's lines; the
        // view resets them on `ViewportReset`.
        self.push_viewport_reset();
        self.refresh_toolbox();

        let embedding_model = self.index.embedding_model.trim().to_string();
        let (embedding_provider, raw_embedding_model) = self
            .resolve_model_backend(&embedding_model)
            .unwrap_or_else(|| (provider.clone(), embedding_model.clone()));

        let task = tokio::spawn(run_research(crate::app::research::ResearchOptions {
            research_provider: provider,
            research_model: raw_research_model,
            embedding_provider,
            embedding_model: raw_embedding_model,
            db_path: self.space.db_path(),
            incognito: self.incognito,
            topic,
            reply_rx,
            steer_rx,
            toolbox,
            tx,
            session_id: session.id,
            prompt_cache_key,
            cache_ttl_1h: self.experiments.cache_ttl_1h,
            space_id,
            space_name,
        }));
        self.research.abort = Some(task.abort_handle());
    }

    /// Abort the active research pipeline, including survey/searcher/tool
    /// streams spawned under its orchestration task.
    pub fn stop_research(&mut self) {
        self.set_survey_gate(None);
        if let Some(abort) = self.research.abort.take() {
            abort.abort();
        }
        if self.research.rx.take().is_some() {
            if let Some((session_id, _)) = self.research.running.take() {
                let _ = self.db.upsert_research_stage_message(
                    &session_id,
                    "research",
                    "stopped by user",
                );
            }
            self.set_survey_gate(None);
            self.research.survey_reply_tx = None;
            self.research.steer_tx = None;
            // Retained steer state belongs to the job: drop it so a long
            // session can't keep unbounded text after research stops.
            self.research.steer_log.clear();
            self.research.steer_acked.clear();
            self.push_status("research stopped".to_string());
        } else {
            self.push_status("no research job is running".to_string());
        }
    }

    /// Whether the survey gate (clarifying questions or plan approval) is
    /// armed for the currently viewed session — the only case where Enter is
    /// intercepted and routed to the pipeline instead of a normal chat send.
    /// A gate in another session must never swallow typing.
    pub fn survey_gate_targets_current_session(&self) -> bool {
        self.research
            .survey_gate
            .as_ref()
            .is_some_and(|g| self.session.as_ref().is_some_and(|s| s.id == g.session_id))
    }

    /// Restore an actionable gate row after loading its session. Normal jobs
    /// already load the persisted row, while incognito jobs recover it from
    /// `SurveyGate` without writing private content to the database.
    pub fn restore_survey_gate_prompt(&mut self) {
        let pending = self.research.survey_gate.as_ref().and_then(|gate| {
            self.session
                .as_ref()
                .filter(|session| session.id == gate.session_id)
                .map(|_| (gate.prompt_role.clone(), gate.prompt_content.clone()))
        });
        let Some((role, content)) = pending else {
            return;
        };
        if self
            .messages
            .iter()
            .any(|message| message.role == role && message.content == content)
        {
            return;
        }
        self.messages.push(crate::db::Message {
            role,
            content,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });
    }

    /// Route a chat reply into the parked survey gate (survey answer or plan
    /// approval/edit). Records the reply as a `gate_reply` in the session —
    /// it renders in the transcript like a user message but is never replayed
    /// to the model, since the survey/plan rows it answers are excluded too:
    /// a bare "the second option" or "drop Q2" must not leak into model
    /// history without its context.
    /// Arm or clear the parked gate, emitting a `Gate` event either way.
    /// The event carries the session id so consumers can compare it against
    /// the viewed session — a gate in another session must never swallow
    /// typing.
    pub fn set_survey_gate(&mut self, gate: Option<SurveyGate>) {
        let state = gate.as_ref().map(|g| super::GateState {
            session_id: g.session_id.clone(),
            phase: g.phase.clone(),
        });
        self.research.survey_gate = gate;
        self.pending_events.push_back(super::AppEvent::Gate(state));
    }

    pub fn reply_to_survey_gate(&mut self, text: &str) {
        let Some(gate) = self.research.survey_gate.take() else {
            return;
        };
        self.pending_events.push_back(super::AppEvent::Gate(None));
        // Persist before acknowledging: the pipeline and the transcript must
        // never incorporate a reply the database didn't record (a locked or
        // full db would otherwise silently lose the answer on reload). On a
        // persistence failure the gate stays armed and the composer is
        // restored so the user can retry.
        let saved_id = if !text.trim().is_empty() && !self.research.incognito {
            match self.db.add_gate_reply_message(&gate.session_id, text) {
                Ok(id) => Some(id),
                Err(e) => {
                    self.set_survey_gate(Some(gate));
                    self.push_composer_set(text);
                    self.push_status(format!("couldn't save your reply — {e}"));
                    return;
                }
            }
        } else {
            None
        };
        if gate.reply_tx.send(text.to_string()).is_err() {
            // Delivery failed: roll back the persisted reply so a retry
            // can't duplicate it in the transcript, then put the text back
            // in the composer rather than eating the user's typing.
            let rollback_error = saved_id.and_then(|id| self.db.delete_message(&id).err());
            self.push_composer_set(text);
            self.push_status(match rollback_error {
                Some(e) => format!(
                    "the job stopped waiting and the saved reply could not be rolled back: {e} — text restored to the composer"
                ),
                None => "the job is no longer waiting for a reply — text restored to the composer"
                    .to_string(),
            });
            return;
        }
        if !text.trim().is_empty()
            && self
                .session
                .as_ref()
                .is_some_and(|s| s.id == gate.session_id)
        {
            self.messages.push(crate::db::Message {
                role: "gate_reply".to_string(),
                content: text.to_string(),
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                cost: None,
                phrase: None,
                persona: None,
                created_at: None,
            });
        }
        match gate.phase {
            SurveyPhase::Clarify { round } => {
                self.push_status(format!(
                    "answer noted (round {round}) — checking for follow-ups… · Ctrl+↑ agents"
                ));
            }
            SurveyPhase::Approve { rework } => self.push_status(if rework {
                "revision folded in — continuing… · Ctrl+↑ agents".to_string()
            } else {
                "plan reply sent — continuing… · Ctrl+↑ agents".to_string()
            }),
        }
    }

    /// Persist a stage row and keep both in-memory views in sync: the
    /// viewed transcript (`self.messages`, only when the job's session is
    /// viewed) and the job's stage-row mirror (`research_stage_rows`, always
    /// — the live popup renders from it without a db read per frame). One
    /// row per label, updated in place. Also used for error rows (plan
    /// record / report file) so a save failure is visible immediately,
    /// not only after a reload.
    fn mirror_stage(&mut self, session_id: &str, label: &str, detail: &str) {
        let _ = self
            .db
            .upsert_research_stage_message(session_id, label, detail);
        let text = crate::db::stage_content(label, detail);
        let prefix = format!("{label}:");
        // Job-level mirror: the live popup's single source of truth.
        if let Some(row) = self
            .research
            .stage_rows
            .iter_mut()
            .rev()
            .find(|c| c.as_str() == label || c.starts_with(prefix.as_str()))
        {
            row.clone_from(&text);
        } else {
            self.research.stage_rows.push(text.clone());
        }
        if self.session.as_ref().is_some_and(|s| s.id == session_id) {
            if let Some(row) = self.messages.iter_mut().rev().find(|m| {
                m.role == "research_stage" && (m.content == label || m.content.starts_with(&prefix))
            }) {
                row.content = text;
                // Stage rows update in place, so message count does not
                // change and the wrapped transcript cache would otherwise
                // keep rendering stale progress.
                self.push_history_invalidated();
            } else {
                self.messages.push(crate::db::Message {
                    role: "research_stage".to_string(),
                    content: text,
                    model: None,
                    reasoning: None,
                    tokens: None,
                    secs: None,
                    cost: None,
                    phrase: None,
                    persona: None,
                    created_at: None,
                });
            }
        }
    }

    /// A research pipeline update: a stage label, or the final report/error.
    /// `None` = the job's channel closed (fires once, right after `Done`).
    // Long by design (event dispatch).
    #[allow(clippy::too_many_lines)]
    pub fn on_research_done(&mut self, r: Option<ResearchMsg>) {
        let Some((session_id, space_id, space_name, update)) = r else {
            self.research.rx = None;
            self.research.abort = None;
            self.research.running = None;
            self.set_survey_gate(None);
            self.research.survey_reply_tx = None;
            self.research.steer_tx = None;
            // Retained steer state belongs to the job: drop it when the job
            // ends so the next job starts from an empty queue view.
            self.research.steer_log.clear();
            self.research.steer_acked.clear();
            self.research.stage_rows.clear();
            self.research.incognito = false;
            self.research.live_input.clear();
            // The view closes its live popup when the job's channel closes.
            return;
        };
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        match update {
            ResearchUpdate::Stage { label, detail } => {
                // A `steer #N` stage row means the pipeline drained that
                // steer: record the acknowledgment and prune the retained
                // log immediately — acknowledged text must not linger in
                // memory while the job is parked.
                if let Some(n) = label.strip_prefix("steer #").and_then(|n| n.parse().ok()) {
                    self.research.steer_acked.insert(n);
                    self.research
                        .steer_log
                        .retain(|(p, _)| !self.research.steer_acked.contains(p));
                }
                self.mirror_stage(&session_id, &label, &detail);
                if viewing {
                    self.push_status(format!(
                        "research: {} · Ctrl+↑ agents",
                        crate::db::stage_content(&label, &detail)
                    ));
                }
            }
            ResearchUpdate::SurveyReady { questions, round } => {
                let topic = self
                    .research
                    .running
                    .as_ref()
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                let header = if round <= 1 {
                    format!("For \"{topic}\":")
                } else {
                    format!("Follow-up (round {round} of {MAX_SURVEY_ROUNDS}) for \"{topic}\":")
                };
                let qs = questions
                    .iter()
                    .enumerate()
                    .map(|(i, q)| format!(" {}. {q}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                let content = format!(
                    "{header}\n{qs}\n\nAnswer in chat — I may ask follow-ups (up to {MAX_SURVEY_ROUNDS} rounds), \
                     then say \"I approve\". (Enter on an empty input skips ahead.)"
                );

                // A normal job must make the prompt durable before it can
                // intercept Enter. Incognito deliberately keeps it only in
                // SurveyGate, where session loading can restore it in memory.
                if !self.research.incognito
                    && let Err(e) = self.db.add_survey_message(&session_id, &content)
                {
                    self.stop_research();
                    self.push_status(format!(
                        "couldn't persist the survey — research stopped: {e}"
                    ));
                    return;
                }
                let Some(tx) = self.research.survey_reply_tx.clone() else {
                    self.stop_research();
                    self.push_status(
                        "survey reply channel unavailable — research stopped".to_string(),
                    );
                    return;
                };
                self.set_survey_gate(Some(SurveyGate {
                    session_id: session_id.clone(),
                    reply_tx: tx,
                    phase: SurveyPhase::Clarify { round },
                    prompt_role: "survey".to_string(),
                    prompt_content: content.clone(),
                }));

                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "survey".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        cost: None,
                        phrase: None,
                        persona: None,
                        created_at: None,
                    });
                    self.push_status(format!(
                        "survey round {round} — answer in chat · Ctrl+↑ agents"
                    ));
                } else {
                    // The gate is parked off-screen: mark the job's session
                    // unread and say where input is needed. In incognito the
                    // prompt will be restored from SurveyGate when opened.
                    self.unread.insert(session_id.clone());
                    self.push_status(format!(
                        "research is waiting on you — survey round {round} for \"{topic}\": \
                         open that session and answer in chat"
                    ));
                }
            }
            ResearchUpdate::PlanReady { questions, rework } => {
                let topic = self
                    .research
                    .running
                    .as_ref()
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                let plan = plan_text(&questions);
                let heading = if rework {
                    "Research plan (revised with your feedback) — reply \"approve\" to continue:"
                } else {
                    "Research plan — reply to approve, or tell me what to change (\"drop Q2\", \"also look into X\"):"
                };
                let content = format!("{heading}\n{plan}");

                // As with survey prompts, normal jobs persist before arming;
                // incognito jobs retain the actionable row only in the gate.
                if !self.research.incognito
                    && let Err(e) = self.db.add_research_plan_message(&session_id, &content)
                {
                    self.stop_research();
                    self.push_status(format!("couldn't persist the plan — research stopped: {e}"));
                    return;
                }
                let Some(tx) = self.research.survey_reply_tx.clone() else {
                    self.stop_research();
                    self.push_status(
                        "plan approval channel unavailable — research stopped".to_string(),
                    );
                    return;
                };
                self.set_survey_gate(Some(SurveyGate {
                    session_id: session_id.clone(),
                    reply_tx: tx,
                    phase: SurveyPhase::Approve { rework },
                    prompt_role: "research_plan".to_string(),
                    prompt_content: content.clone(),
                }));

                // A byproduct record in the space's files, like the report:
                // the conversation is the edit surface, the file is history.
                // Skipped entirely in incognito — the plan folds in the user's
                // survey replies, so "nothing persists" must not leave it on
                // disk even if the job stops before any report. Failures
                // surface as a transcript stage row instead of vanishing.
                if let Err(e) = self.save_space_artifact(
                    &space_id,
                    &space_name,
                    &topic,
                    "plan",
                    &format!("# Research plan: {topic}\n\n{plan}\n"),
                ) {
                    self.mirror_stage(
                        &session_id,
                        "plan record",
                        &format!("error — could not save plan record: {e}"),
                    );
                }
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "research_plan".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        cost: None,
                        phrase: None,
                        persona: None,
                        created_at: None,
                    });
                    self.push_status(if rework {
                        "revised plan ready — reply to approve".to_string()
                    } else {
                        "research plan ready — reply to approve or change · Ctrl+↑ agents"
                            .to_string()
                    });
                } else {
                    // The gate is parked off-screen: mark the job's session
                    // unread and say where input is needed. An incognito plan
                    // is restored from SurveyGate rather than the database.
                    self.unread.insert(session_id.clone());
                    self.push_status(format!(
                        "research is waiting on you — plan approval for \"{topic}\": \
                         open that session and reply \"approve\""
                    ));
                }
            }
            ResearchUpdate::Done(Ok(report)) => {
                // A watch session with a prior run gets a "what changed"
                // section prepended, listing sources not cited last time.
                let report = if let Ok(Some(prev_citations)) =
                    self.previous_citations_for_watch_session(&session_id, &space_id)
                {
                    let new_sources =
                        crate::app::watches::new_sources_since(&report, &prev_citations);
                    format!(
                        "{}\n\n{}",
                        crate::app::watches::diff_section("", &report, &new_sources),
                        report
                    )
                } else {
                    report
                };
                let _ = self.db.add_assistant_message(
                    &session_id,
                    &report,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                let topic = self
                    .research
                    .running
                    .as_ref()
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                // Failures surface as a transcript stage row (mirrored into
                // the in-memory transcript and the live popup) instead of
                // vanishing (incognito skips the write by design).
                let mut in_library = None;
                match self.save_research_report(&space_id, &space_name, &topic, &report) {
                    Ok(Some(path)) => {
                        match self.save_research_doc(&space_name, &topic, &path, &report) {
                            Ok(relative) => in_library = Some(relative),
                            Err(e) => self.mirror_stage(
                                &session_id,
                                "report doc",
                                &format!("error — could not add the report to the library: {e}"),
                            ),
                        }
                    }
                    Ok(None) => {}
                    Err(e) => self.mirror_stage(
                        &session_id,
                        "report file",
                        &format!("error — could not save report file: {e}"),
                    ),
                }
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "assistant".to_string(),
                        content: report,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        cost: None,
                        phrase: Some("Researched".to_string()),
                        persona: None,
                        created_at: None,
                    });
                    self.push_status(in_library.map_or_else(
                        || "research complete".to_string(),
                        |doc| format!("research complete · in your library as {doc} (/library)"),
                    ));
                } else {
                    self.unread.insert(session_id);
                    if let Some((_, topic)) = &self.research.running {
                        self.push_status(format!("✓ research ready: {topic}"));
                    }
                }
            }
            ResearchUpdate::Done(Err(e)) => {
                let msg = format!("research failed: {e}");
                let _ = self.db.add_assistant_message(
                    &session_id,
                    &msg,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "assistant".to_string(),
                        content: msg.clone(),
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        cost: None,
                        phrase: None,
                        persona: None,
                        created_at: None,
                    });
                }
                self.push_status(msg);
            }
        }
    }

    /// Write a space artifact (finished report or presented plan) into the
    /// job's own space — not necessarily the currently active one, since the
    /// user may have switched spaces while the job ran — named
    /// `{prefix}-<slug>-<timestamp>.md`. Only refreshes the files cache /
    /// triggers a rescan if that space is still active; otherwise the file
    /// sits on disk and gets picked up next time that space's /files is
    /// opened, same as any externally-dropped file. Returns the written path
    /// so callers can surface failures at the update boundary instead of
    /// dropping them. Never writes in incognito mode: the plan folds in the
    /// user's survey replies, so "nothing persists" must not leave it on
    /// disk even if the job stops before any report.
    fn save_space_artifact(
        &mut self,
        space_id: &str,
        space_name: &str,
        topic: &str,
        prefix: &str,
        body: &str,
    ) -> std::io::Result<Option<std::path::PathBuf>> {
        // "Nothing persists" mode: no plan/report records on disk at all —
        // plan files incorporate survey replies, so even a job stopped
        // before its report must not leak user details. The mode is the one
        // captured when the job started, not a mid-job toggle.
        if self.research.incognito {
            return Ok(None);
        }
        let dir = self.space.files_dir(space_name);
        std::fs::create_dir_all(&dir)?;
        let slug = super::sessions::slugify(topic);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let name = format!("{prefix}-{slug}-{stamp}.md");
        let path = dir.join(&name);
        std::fs::write(&path, body)?;
        if space_id == self.active_space.id {
            self.rescan_files();
        }
        Ok(Some(path))
    }

    /// Save the finished report into the job's own space, named
    /// `research-<slug>-<timestamp>.md`, via the shared artifact writer,
    /// then index the report's cited sources for
    /// `research_lookup(scope=citations)`. Failures propagate to the caller
    /// (the `Done` handler surfaces them as a transcript stage row).
    fn save_research_report(
        &mut self,
        space_id: &str,
        space_name: &str,
        topic: &str,
        report: &str,
    ) -> std::io::Result<Option<std::path::PathBuf>> {
        let saved = self.save_space_artifact(space_id, space_name, topic, "research", report)?;
        if let Some(path) = &saved {
            // Index the report's cited sources for research_lookup(scope=citations).
            let citations = crate::citations::parse_citations(report);
            if !citations.is_empty() {
                // Titles aren't in the Sources-list format; index url only.
                let rows: Vec<(String, Option<String>)> =
                    citations.into_iter().map(|(_, url)| (url, None)).collect();
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let _ = self.db.add_citations(space_id, &name, &rows);
            }
        }
        Ok(saved)
    }

    /// The report as a doc in the space's library, beside its `.md` file
    /// (`research-x-<stamp>.md` gives `docs/research-x-<stamp>.doc.json`), so
    /// it opens in studio and exports in the space's design. The `.md` stays:
    /// watches and citations find reports by that name. Returns the doc's
    /// library path.
    fn save_research_doc(
        &mut self,
        space_name: &str,
        topic: &str,
        report_path: &std::path::Path,
        report: &str,
    ) -> anyhow::Result<String> {
        let files = self.space.files_dir(space_name);
        let stem = report_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("research");
        let relative = format!("docs/{stem}.doc.json");
        let doc = crate::doc::import::from_markdown(&files, report, topic)?;
        crate::doc::storage::save(&files, &relative, &doc, crate::doc::history::Who::Model)?;
        Ok(relative)
    }
}

#[cfg(test)]
mod tests;
