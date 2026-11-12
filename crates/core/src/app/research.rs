//! Background multi-agent research with conversational approval gates.
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

#[derive(Clone)]
pub enum ResearchUpdate {
    /// Labels identify stage rows for in-place progress updates.
    Stage {
        label: String,
        detail: String,
    },
    /// Park for clarification with a one-based round number.
    SurveyReady {
        questions: Vec<String>,
        round: u8,
    },
    /// Park for plan approval, with `rework` marking a revised presentation.
    PlanReady {
        questions: Vec<PlanQuestion>,
        rework: bool,
    },
    Done(std::result::Result<String, String>),
}

const MAX_SUBQUESTIONS: usize = 6;
// Bound retained steer text and the otherwise unbounded channel.
const MAX_QUEUED_STEERS: usize = 64;
const MAX_SURVEY_QUESTIONS: usize = 4;
pub const MAX_SURVEY_ROUNDS: u8 = 3;
pub const RESEARCH_SEARCHER_MAX_ITERS: usize = 6;

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
    pub const fn bare(question: String) -> Self {
        Self {
            question,
            why: String::new(),
            angles: Vec::new(),
            sources: Vec::new(),
        }
    }

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
    pub fn start_research(&mut self, topic: &str) {
        self.start_research_with_gate(topic, true);
    }

    pub fn steer_research(&mut self, text: &str) {
        if text.is_empty() {
            self.push_status("usage: /steer <what to also look into>".to_string());
            return;
        }
        match &self.research.steer_tx {
            Some(_) if self.research.steer_log.len() >= MAX_QUEUED_STEERS => {
                self.push_status(format!(
                    "steer queue full ({MAX_QUEUED_STEERS} pending) — wait for the next round"
                ));
            }
            Some(tx) if tx.send(text.to_string()).is_ok() => {
                // Stable positions let acknowledged entries be pruned without changing row identity.
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
        self.set_survey_gate(None);
        self.research.survey_reply_tx = None;

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
        // Persistence must follow the job's initial privacy mode, not later UI toggles.
        self.research.incognito = self.incognito;

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
            self.research.steer_log.clear();
            self.research.steer_acked.clear();
            self.push_status("research stopped".to_string());
        } else {
            self.push_status("no research job is running".to_string());
        }
    }

    /// Intercept input only for a gate belonging to the viewed session.
    pub fn survey_gate_targets_current_session(&self) -> bool {
        self.research
            .survey_gate
            .as_ref()
            .is_some_and(|g| self.session.as_ref().is_some_and(|s| s.id == g.session_id))
    }

    // Incognito gate prompts must survive session switches without being written to disk.
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
        // Persist before acknowledging so database failure cannot lose an accepted reply.
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
            // Roll back failed delivery so retrying cannot duplicate the persisted reply.
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

    fn mirror_stage(&mut self, session_id: &str, label: &str, detail: &str) {
        let _ = self
            .db
            .upsert_research_stage_message(session_id, label, detail);
        let text = crate::db::stage_content(label, detail);
        let prefix = format!("{label}:");
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
                // In-place edits leave message counts unchanged, so invalidate the render cache explicitly.
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

    #[allow(clippy::too_many_lines)]
    pub fn on_research_done(&mut self, r: Option<ResearchMsg>) {
        let Some((session_id, space_id, space_name, update)) = r else {
            self.research.rx = None;
            self.research.abort = None;
            self.research.running = None;
            self.set_survey_gate(None);
            self.research.survey_reply_tx = None;
            self.research.steer_tx = None;
            self.research.steer_log.clear();
            self.research.steer_acked.clear();
            self.research.stage_rows.clear();
            self.research.incognito = false;
            self.research.live_input.clear();
            return;
        };
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        match update {
            ResearchUpdate::Stage { label, detail } => {
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

                // Make the prompt recoverable before intercepting input; incognito recovery is memory-only.
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

                // Make the prompt recoverable before arming; incognito recovery is memory-only.
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

                // Plans contain private survey replies and must never be written in incognito.
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
                    self.unread.insert(session_id.clone());
                    self.push_status(format!(
                        "research is waiting on you — plan approval for \"{topic}\": \
                         open that session and reply \"approve\""
                    ));
                }
            }
            ResearchUpdate::Done(Ok(report)) => {
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

    // Background artifacts belong to the job's origin space, not the currently viewed one.
    fn save_space_artifact(
        &mut self,
        space_id: &str,
        space_name: &str,
        topic: &str,
        prefix: &str,
        body: &str,
    ) -> std::io::Result<Option<std::path::PathBuf>> {
        // The job's initial privacy mode forbids persisting plans as well as finished reports.
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

    fn save_research_report(
        &mut self,
        space_id: &str,
        space_name: &str,
        topic: &str,
        report: &str,
    ) -> std::io::Result<Option<std::path::PathBuf>> {
        let saved = self.save_space_artifact(space_id, space_name, topic, "research", report)?;
        if let Some(path) = &saved {
            let citations = crate::citations::parse_citations(report);
            if !citations.is_empty() {
                // The Sources-list format carries URLs but no titles.
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

    // Retain the Markdown report because watches and citation lookup depend on its filename.
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
