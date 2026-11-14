//! Swarm turns: several agents answer the message independently, a reviewer
//! checks their work with tools, and a writer turns what held up into the reply.
//! Each step is a read-only sub-agent run, listed in `/agents` like a `task`.
//!
//! Independent drafts and a checking pass, not a persona debate: debating agents
//! drift toward agreement and drop correct minority answers, while answers
//! sampled apart and then verified keep the gains.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use super::App;
use crate::db::Persona;
use crate::prompts;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatMessage, ChatParams, StreamEvent, SubagentUpdate};
use crate::subagent::{self, Role};
use crate::tools::{ReadOnly, ToolExecutor};

/// Drafts when the session has no roster.
const DEFAULT_DRAFTS: usize = 3;
/// Drafts at most, whatever the roster holds.
const MAX_DRAFTS: usize = 6;
const DRAFT_STEPS: usize = 8;
const REVIEW_STEPS: usize = 10;

pub type SwarmMsg = (String, SwarmUpdate);

#[derive(Clone)]
pub enum SwarmUpdate {
    Progress(String),
    /// One of the turn's agents moved; it lands in `/agents` like a `task` run.
    Subagent(Box<SubagentUpdate>),
    Synthesis(String),
    Error(String),
}

/// One independent answer: who writes it, what it looks at, and on which model.
pub struct Drafter {
    pub label: String,
    pub focus: String,
    pub provider: OpenRouter,
    pub model: String,
}

impl App {
    pub fn toggle_swarm_mode(&mut self) -> Result<()> {
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        let on = !session.swarm_mode;
        session.swarm_mode = on;
        self.db.set_session_swarm_mode(&session.id, on)?;
        self.push_status(format!("swarm mode: {}", if on { "ON" } else { "OFF" }));
        Ok(())
    }

    pub fn start_swarm_turn(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if self.swarm.rx.is_some() {
            self.push_status("a swarm turn is already running".to_string());
            return;
        }
        let default_model = self.current_model.clone().unwrap_or_default();
        let Some((default_provider, raw_default_model)) =
            self.resolve_model_backend(&default_model)
        else {
            self.pending_events
                .push_back(super::AppEvent::OpenLoginPopup);
            return;
        };
        let default_provider = self.usage_provider(default_provider, "swarm");
        let personas = self.db.list_swarm_personas(&session.id).unwrap_or_default();
        let drafters = self.drafters(&personas, &default_provider, &raw_default_model);
        let user_message = self
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let base_history = self.build_history();

        let (tx, rx) = mpsc::unbounded_channel();
        self.swarm.rx = Some(rx);
        self.swarm.incognito = self.incognito;
        self.swarm.cost = None;
        self.push_status("swarm: starting…".to_string());

        let swarm_session_id = session.id.clone();
        let task = tokio::spawn(run_swarm_turn(SwarmTurnOptions {
            drafters,
            provider: default_provider,
            model: raw_default_model,
            base_history,
            tools: Arc::new(ReadOnly(self.toolbox.clone())),
            user_message,
            prompt_cache_key: format!("swarm:{}", self.prompt_cache_key_for(&session.id)),
            cache_ttl_1h: self.settings.cache_lifetime == "extended",
            session_id: session.id,
            tx,
        }));
        self.swarm.abort = Some(task.abort_handle());
        self.swarm.session = Some(swarm_session_id);
    }

    /// The roster's personas as drafters, or plain drafts on the session model;
    /// a persona whose model is unavailable drafts on the session model.
    fn drafters(
        &mut self,
        personas: &[Persona],
        default_provider: &OpenRouter,
        default_model: &str,
    ) -> Vec<Drafter> {
        if personas.is_empty() {
            return (1..=DEFAULT_DRAFTS)
                .map(|n| Drafter {
                    label: format!("drafter {n}"),
                    focus: String::new(),
                    provider: default_provider.clone(),
                    model: default_model.to_string(),
                })
                .collect();
        }
        personas
            .iter()
            .take(MAX_DRAFTS)
            .map(|persona| {
                let (provider, model) =
                    if let Some((provider, model)) = self.resolve_model_backend(&persona.model) {
                        (self.usage_provider(provider, "swarm"), model)
                    } else {
                        self.push_status(format!(
                            "swarm: {} model unavailable ({}); using the session model",
                            persona.name, persona.model
                        ));
                        (default_provider.clone(), default_model.to_string())
                    };
                Drafter {
                    label: persona.name.clone(),
                    focus: persona.blurb.trim().to_string(),
                    provider,
                    model,
                }
            })
            .collect()
    }

    pub fn save_swarm_roster(&mut self) -> Result<()> {
        self.swarm.cache.retain(|p| !p.name.trim().is_empty());
        if let Some(session) = &self.session {
            self.db
                .save_swarm_personas(&session.id, &self.swarm.cache)?;
        }
        Ok(())
    }

    pub fn stop_swarm(&mut self) {
        if let Some(abort) = self.swarm.abort.take() {
            abort.abort();
        }
        if self.swarm.rx.take().is_some() {
            if let Some(id) = self.swarm.session.take() {
                let _ = self
                    .db
                    .upsert_research_stage_message(&id, "swarm", "stopped by user");
                self.end_subagents(&id, self.swarm.incognito);
            }
            self.push_status("swarm stopped".to_string());
        } else {
            self.push_status("no swarm is running".to_string());
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn on_swarm_update(&mut self, r: Option<SwarmMsg>) {
        let Some((session_id, update)) = r else {
            self.swarm.rx = None;
            self.swarm.abort = None;
            if let Some(id) = self.swarm.session.take() {
                self.end_subagents(&id, self.swarm.incognito);
            }
            return;
        };
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        match update {
            SwarmUpdate::Progress(s) => {
                let _ = self
                    .db
                    .upsert_research_stage_message(&session_id, "swarm", &s);
                if viewing {
                    let text = crate::db::stage_content("swarm", &s);
                    if let Some(row) = self.messages.iter_mut().rev().find(|m| {
                        m.role == "research_stage"
                            && (m.content == "swarm" || m.content.starts_with("swarm:"))
                    }) {
                        row.content = text;
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
                    self.push_status(format!("swarm: {s}"));
                }
            }
            SwarmUpdate::Subagent(update) => {
                if let Some(cost) = self.on_subagent(&session_id, self.swarm.incognito, *update) {
                    *self.swarm.cost.get_or_insert(0.0) += cost;
                }
            }
            SwarmUpdate::Synthesis(content) => {
                let cost = self.swarm.cost.take();
                let _ = self.db.add_assistant_message(
                    &session_id,
                    &content,
                    None,
                    None,
                    None,
                    None,
                    cost,
                    Some(DRAFTED),
                );
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "assistant".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        cost,
                        phrase: Some(DRAFTED.to_string()),
                        persona: None,
                        created_at: None,
                    });
                    self.push_status("swarm turn complete".to_string());
                    self.maybe_generate_title();
                    self.maybe_mask();
                    self.maybe_compact();
                }
            }
            SwarmUpdate::Error(e) => {
                // Progress upserts must not overwrite failure rows.
                let _ = self
                    .db
                    .add_error_message(&session_id, &format!("swarm: {e}"));
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "error".to_string(),
                        content: format!("swarm: {e}"),
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        cost: None,
                        phrase: None,
                        persona: None,
                        created_at: None,
                    });
                    self.push_status(format!("swarm error: {e}"));
                }
            }
        }
    }
}

pub fn parse_persona_editor(text: &str) -> Result<Persona, String> {
    let mut lines = text.lines();
    let name = lines
        .next()
        .and_then(|line| line.strip_prefix("name:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "first line must be `name: <non-empty name>`".to_string())?;
    let model = lines
        .next()
        .and_then(|line| line.strip_prefix("model:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "second line must be `model: <model id>`".to_string())?;
    let remaining: Vec<&str> = lines.collect();
    let separator = remaining
        .iter()
        .position(|line| line.trim() == "---")
        .ok_or_else(|| "missing `---` before the blurb".to_string())?;
    let blurb = remaining[separator + 1..].join("\n").trim().to_string();
    Ok(Persona {
        name: name.to_string(),
        model: model.to_string(),
        blurb,
    })
}

/// The reply's footer word: what the turn did instead of "thought".
const DRAFTED: &str = "Drafted";

pub struct SwarmTurnOptions {
    pub drafters: Vec<Drafter>,
    /// The session model, which reviews the drafts and writes the reply.
    pub provider: OpenRouter,
    pub model: String,
    pub base_history: Vec<ChatMessage>,
    /// Read-only: every agent here may look, none may change anything.
    pub tools: Arc<dyn ToolExecutor>,
    pub user_message: String,
    pub prompt_cache_key: String,
    pub cache_ttl_1h: bool,
    pub session_id: String,
    pub tx: mpsc::UnboundedSender<SwarmMsg>,
}

impl SwarmTurnOptions {
    fn chat_params(&self) -> ChatParams {
        ChatParams {
            prompt_cache_key: Some(self.prompt_cache_key.clone()),
            cache_ttl_1h: self.cache_ttl_1h,
            ..ChatParams::default()
        }
    }
}

/// Draft letters, so the reviewer and writer can name answers without names
/// that hint at a role.
fn letter(index: usize) -> char {
    char::from(b'A' + u8::try_from(index % 26).unwrap_or(0))
}

fn draft_messages(base: &[ChatMessage], drafter: &Drafter) -> Vec<ChatMessage> {
    let focus = if drafter.focus.is_empty() {
        String::new()
    } else {
        format!("\nYour focus: {}\n", drafter.focus)
    };
    with_instruction(
        base,
        prompts::render(
            prompts::SWARM_DRAFT,
            &[("drafter", &drafter.label), ("focus", &focus)],
        ),
    )
}

fn render_drafts(drafts: &[(char, String, String)]) -> String {
    drafts
        .iter()
        .map(|(letter, _, content)| format!("### Answer {letter}\n\n{content}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn title_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
}

/// Every agent reports on one channel, forwarded in order; the turn awaits the
/// forwarder so the reply's cost is complete before the reply itself arrives.
fn forward_agents(
    tx: mpsc::UnboundedSender<SwarmMsg>,
    session_id: String,
) -> (
    mpsc::UnboundedSender<StreamEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (events, mut agent_rx) = mpsc::unbounded_channel::<StreamEvent>();
    let forward = tokio::spawn(async move {
        while let Some(event) = agent_rx.recv().await {
            if let StreamEvent::Subagent(update) = event {
                let _ = tx.send((session_id.clone(), SwarmUpdate::Subagent(update)));
            }
        }
    });
    (events, forward)
}

/// The conversation so far plus one instruction for this agent.
fn with_instruction(base: &[ChatMessage], instruction: String) -> Vec<ChatMessage> {
    let mut messages = base.to_vec();
    messages.push(ChatMessage::text("user", instruction));
    messages
}

/// Every drafter answers at once, none seeing another's answer.
async fn draft_apart(
    drafters: &[Drafter],
    base_history: &[ChatMessage],
    tools: &Arc<dyn ToolExecutor>,
    params: &ChatParams,
    events: &mpsc::UnboundedSender<StreamEvent>,
    topic: &str,
) -> Vec<Result<String>> {
    let runs = drafters.iter().enumerate().map(|(index, drafter)| {
        let context = subagent::Context::new(
            drafter.provider.clone(),
            drafter.model.clone(),
            params.clone(),
            events.clone(),
        );
        let role = Role {
            title: format!("Draft {} · {} · {topic}", letter(index), drafter.label),
            messages: draft_messages(base_history, drafter),
            max_steps: DRAFT_STEPS,
        };
        let tools = Arc::clone(tools);
        async move { subagent::run_role(&context, tools.as_ref(), role).await }
    });
    futures_util::future::join_all(runs).await
}

async fn run_swarm_turn(opts: SwarmTurnOptions) {
    let params = opts.chat_params();
    let SwarmTurnOptions {
        drafters,
        provider,
        model,
        base_history,
        tools,
        user_message,
        session_id,
        tx,
        ..
    } = opts;
    let send = |u: SwarmUpdate| {
        let _ = tx.send((session_id.clone(), u));
    };
    let (events, forward) = forward_agents(tx.clone(), session_id.clone());
    let topic = title_line(&user_message).to_string();
    let count = drafters.len();
    send(SwarmUpdate::Progress(format!(
        "{count} agents drafting apart · Ctrl+↑ to watch"
    )));
    let results = draft_apart(&drafters, &base_history, &tools, &params, &events, &topic).await;
    let mut drafts = Vec::new();
    for (index, (drafter, result)) in drafters.iter().zip(results).enumerate() {
        match result {
            Ok(content) => drafts.push((letter(index), drafter.label.clone(), content)),
            Err(e) => send(SwarmUpdate::Error(format!("{} failed: {e}", drafter.label))),
        }
    }
    if drafts.is_empty() {
        send(SwarmUpdate::Error("every draft failed".to_string()));
        return;
    }
    let rendered = render_drafts(&drafts);
    let context = subagent::Context::new(provider, model, params, events);

    send(SwarmUpdate::Progress(format!(
        "checking {} drafts · Ctrl+↑ to watch",
        drafts.len()
    )));
    let messages = with_instruction(
        &base_history,
        prompts::render(
            prompts::SWARM_VERIFY,
            &[("topic", &user_message), ("drafts", &rendered)],
        ),
    );
    let review = subagent::run_role(
        &context,
        tools.as_ref(),
        Role {
            title: format!("Review of {} drafts · {topic}", drafts.len()),
            messages,
            max_steps: REVIEW_STEPS,
        },
    )
    .await
    .unwrap_or_else(|e| {
        send(SwarmUpdate::Error(format!("review failed: {e}")));
        "The review failed, so no claim has been checked; treat every answer as unverified."
            .to_string()
    });

    send(SwarmUpdate::Progress("writing the reply".to_string()));
    let messages = with_instruction(
        &base_history,
        prompts::render(
            prompts::SWARM_SYNTHESIS,
            &[
                ("topic", &user_message),
                ("drafts", &rendered),
                ("review", &review),
            ],
        ),
    );
    // No steps: the writer answers from the drafts and review without tools.
    let reply = subagent::run_role(
        &context,
        tools.as_ref(),
        Role {
            title: format!("Reply · {topic}"),
            messages,
            max_steps: 0,
        },
    )
    .await;
    drop(context);
    let _ = forward.await;
    match reply {
        Ok(text) => {
            send(SwarmUpdate::Progress(format!(
                "{} drafts · checked · Ctrl+↑ shows each agent",
                drafts.len()
            )));
            send(SwarmUpdate::Synthesis(text));
        }
        Err(e) => send(SwarmUpdate::Error(format!("writing the reply failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_params_pass_the_cache_ttl_experiment() {
        let app = super::super::tests::app_with_key();
        let provider = OpenRouter::openrouter_flavor("test-key".to_string());
        for enabled in [false, true] {
            let (tx, _rx) = mpsc::unbounded_channel();
            let opts = SwarmTurnOptions {
                drafters: Vec::new(),
                provider: provider.clone(),
                model: "anthropic/claude-sonnet-4".to_string(),
                base_history: Vec::new(),
                tools: app.toolbox.clone(),
                user_message: "topic".to_string(),
                prompt_cache_key: "swarm:s:0".to_string(),
                cache_ttl_1h: enabled,
                session_id: "s".to_string(),
                tx,
            };
            let params = opts.chat_params();
            assert_eq!(params.cache_ttl_1h, enabled);
            assert_eq!(params.prompt_cache_key.as_deref(), Some("swarm:s:0"));
        }
    }

    #[test]
    fn persona_editor_round_trips_name_model_and_multiline_blurb() {
        let persona = parse_persona_editor(
            "name: Skeptic\nmodel: codex:gpt-5.4-mini\n---\npokes holes\nand checks evidence\n",
        )
        .unwrap();
        assert_eq!(persona.name, "Skeptic");
        assert_eq!(persona.model, "codex:gpt-5.4-mini");
        assert_eq!(persona.blurb, "pokes holes\nand checks evidence");
        assert!(parse_persona_editor("name: \nmodel: m\n---\n").is_err());
    }

    #[test]
    fn drafters_default_to_three_plain_drafts_and_cap_the_roster() {
        let mut app = super::super::tests::app_with_key();
        let provider = OpenRouter::openrouter_flavor("test-key".to_string());
        let plain = app.drafters(&[], &provider, "m");
        assert_eq!(plain.len(), DEFAULT_DRAFTS);
        assert!(plain.iter().all(|d| d.focus.is_empty() && d.model == "m"));
        let roster: Vec<_> = (0..9)
            .map(|n| Persona {
                name: format!("p{n}"),
                model: "mistral:not-configured".into(),
                blurb: format!(" check {n} "),
            })
            .collect();
        let drafters = app.drafters(&roster, &provider, "m");
        assert_eq!(drafters.len(), MAX_DRAFTS);
        assert_eq!(drafters[2].focus, "check 2");
        assert_eq!(drafters[2].model, "m", "an unknown model falls back");
    }

    #[test]
    fn a_draft_sees_the_conversation_and_only_its_own_focus() {
        let provider = OpenRouter::openrouter_flavor("test-key".to_string());
        let base = vec![
            ChatMessage::text("system", "sys"),
            ChatMessage::text("user", "Which cache should we use?"),
        ];
        let drafter = Drafter {
            label: "Ops".into(),
            focus: "what breaks at 3am".into(),
            provider,
            model: "m".into(),
        };
        let messages = draft_messages(&base, &drafter);
        assert_eq!(messages.len(), 3);
        let note = &messages[2].content;
        assert!(note.starts_with("You are Ops,"), "{note}");
        assert!(
            note.contains("\n\nYour focus: what breaks at 3am\n\n"),
            "{note}"
        );
        assert!(!note.contains("{{"), "{note}");
        let plain = draft_messages(
            &base,
            &Drafter {
                focus: String::new(),
                ..drafter
            },
        );
        assert!(!plain[2].content.contains("focus"), "{}", plain[2].content);
    }

    #[test]
    fn drafts_are_named_by_letter_not_by_role() {
        let drafts = vec![
            ('A', "Skeptic".to_string(), "no".to_string()),
            ('B', "Advocate".to_string(), "yes".to_string()),
        ];
        let rendered = render_drafts(&drafts);
        assert_eq!(rendered, "### Answer A\n\nno\n\n### Answer B\n\nyes");
        assert_eq!(letter(0), 'A');
        assert_eq!(letter(27), 'B');
    }
}
