use std::sync::Arc;

use tokio::sync::mpsc;

use crate::prompts;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatMessage, ChatParams, StreamEvent, SubagentEvent};
use crate::subagent::{self, Role};
use crate::tools::{NoTools, ToolBox, ToolExecutor};

use super::prompts::{
    critic_messages, plan_approval_messages, planner_messages_with_context, research_stage_prompt,
    resolver_messages, survey_messages, synthesizer_messages, verifier_messages, writer_messages,
};
use super::{
    Approval, Critique, MAX_SURVEY_ROUNDS, PlanQuestion, RESEARCH_SEARCHER_MAX_ITERS, ResearchMsg,
    ResearchUpdate, SEARCHER_PROMPT, SurveyReply, parse_approval, parse_critique,
    parse_plan_blocks, parse_survey_reply,
};

/// Agents research runs at once; its searchers fan out wider than the
/// global sub-agent limit allows.
const RESEARCH_AGENTS: usize = 8;

/// What every research agent shares: usage accounting (which must never
/// persist prompt text), the channel its runs report on, and their slots.
#[derive(Clone)]
pub(super) struct ResearchUsage {
    pub(super) db_path: std::path::PathBuf,
    pub(super) session_id: String,
    pub(super) prompt_cache_key: String,
    pub(super) cache_ttl_1h: bool,
    pub(super) space_id: String,
    pub(super) topic: String,
    pub(super) events: mpsc::UnboundedSender<StreamEvent>,
    pub(super) slots: Arc<tokio::sync::Semaphore>,
}

impl ResearchUsage {
    pub(super) fn chat_params(&self) -> ChatParams {
        ChatParams {
            prompt_cache_key: Some(self.prompt_cache_key.clone()),
            cache_ttl_1h: self.cache_ttl_1h,
            ..ChatParams::default()
        }
    }

    fn log(&self) -> Option<crate::db::UsageLog> {
        (!self.session_id.is_empty()).then(|| crate::db::UsageLog {
            path: self.db_path.clone(),
            session_id: Some(self.session_id.clone()),
            space_id: self.space_id.clone(),
        })
    }

    /// One research agent, listed in `/agents` as `title` and logged under `kind`.
    /// `on_step` hears a one-line summary of each tool call, for its stage row.
    /// An agent that ends with no text answers with an empty string.
    async fn agent(
        &self,
        (provider, model, kind): (&OpenRouter, &str, &str),
        title: String,
        tools: &dyn ToolExecutor,
        (messages, max_steps): (Vec<ChatMessage>, usize),
        on_step: &(dyn Fn(String) + Sync),
    ) -> Result<String, String> {
        let (events, mut steps) = mpsc::unbounded_channel::<StreamEvent>();
        let context = subagent::Context::new(
            provider.clone().logged(self.log(), kind),
            model.to_string(),
            self.chat_params(),
            events,
        )
        .with_slots(self.slots.clone());
        let run = async move {
            let outcome = subagent::run_role(
                &context,
                tools,
                Role {
                    title,
                    messages,
                    max_steps,
                },
            )
            .await;
            drop(context);
            outcome
        };
        let relay = async {
            while let Some(event) = steps.recv().await {
                if let StreamEvent::Subagent(update) = &event
                    && let SubagentEvent::Round { calls, .. } = &update.event
                    && let Some((call, result)) = calls.last()
                {
                    on_step(crate::app::tool_call_summary(
                        &call.name,
                        &call.arguments,
                        result,
                    ));
                }
                let _ = self.events.send(event);
            }
        };
        match tokio::join!(run, relay).0 {
            Ok(text) => Ok(text.trim().to_string()),
            Err(e) if e.is::<subagent::EmptyAnswer>() => Ok(String::new()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn send_stage(
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    label: impl Into<String>,
    detail: impl Into<String>,
) {
    let _ = tx.send((
        ids.0.clone(),
        ids.1.clone(),
        ids.2.clone(),
        ResearchUpdate::Stage {
            label: label.into(),
            detail: detail.into(),
        },
    ));
}

pub fn drain_steers(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(s) = rx.try_recv() {
        out.push(s);
    }
    out
}

async fn complete_text(
    provider: &OpenRouter,
    model: &str,
    messages: Vec<ChatMessage>,
    usage: &ResearchUsage,
    kind: &str,
) -> Result<String, String> {
    let mut stage = kind.trim_start_matches("research:").chars();
    let title: String = stage
        .next()
        .map(|first| first.to_uppercase().chain(stage).collect())
        .unwrap_or_default();
    let text = usage
        .agent(
            (provider, model, kind),
            format!("{title} · {}", usage.topic),
            &NoTools,
            (messages, 0),
            &|_| {},
        )
        .await?;
    if text.is_empty() {
        return Err(subagent::EmptyAnswer.to_string());
    }
    Ok(text)
}

async fn complete_agent(
    provider: &OpenRouter,
    model: &str,
    messages: Vec<ChatMessage>,
    usage: &ResearchUsage,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    label: &str,
) -> Result<String, String> {
    match complete_text(
        provider,
        model,
        messages,
        usage,
        &format!("research:{label}"),
    )
    .await
    {
        Ok(text) => Ok(text),
        Err(e) => {
            send_stage(tx, ids, label, format!("error — {e}"));
            Err(e)
        }
    }
}

async fn plan(
    provider: &OpenRouter,
    model: &str,
    topic: &str,
    answers: &[(String, String)],
    known: &[String],
    usage: &ResearchUsage,
) -> Result<Vec<PlanQuestion>, String> {
    let text = complete_text(
        provider,
        model,
        planner_messages_with_context(topic, answers, known),
        usage,
        "research:planner",
    )
    .await?;
    let qs = parse_plan_blocks(&text);
    if qs.is_empty() {
        return Err(format!(
            "planner returned no usable sub-questions (raw reply: {text:.200})"
        ));
    }
    Ok(qs)
}

#[allow(clippy::too_many_arguments)]
pub struct SearcherCtx<'a> {
    pub toolbox: Arc<dyn ToolExecutor>,
    usage: Arc<ResearchUsage>,
    pub tx: &'a mpsc::UnboundedSender<ResearchMsg>,
    pub ids: &'a (String, String, String),
}

pub struct SearcherSlot {
    pub batch: String,
    pub idx: usize,
    pub total: usize,
}

async fn run_searcher(
    provider: &OpenRouter,
    model: &str,
    prompt: &str,
    display: &str,
    ctx: SearcherCtx<'_>,
    slot: SearcherSlot,
) -> String {
    // Batch identity prevents follow-up searchers from overwriting earlier activity rows.
    let label = format!("searcher {} {}/{}", slot.batch, slot.idx + 1, slot.total);
    send_stage(
        ctx.tx,
        ctx.ids,
        &label,
        format!("working — investigating \"{display}\""),
    );
    let messages = vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(SEARCHER_PROMPT, &[])),
        ),
        ChatMessage::text("user", prompt),
    ];
    let kind = if slot.batch == "web survey" {
        "research:web survey"
    } else {
        "research:searcher"
    };
    let buf = match ctx
        .usage
        .agent(
            (provider, model, kind),
            format!(
                "Searcher {} {}/{} · {display}",
                slot.batch,
                slot.idx + 1,
                slot.total
            ),
            ctx.toolbox.as_ref(),
            (messages, RESEARCH_SEARCHER_MAX_ITERS),
            &|step| send_stage(ctx.tx, ctx.ids, &label, format!("working — {step}")),
        )
        .await
    {
        Ok(text) => text,
        Err(e) => {
            send_stage(ctx.tx, ctx.ids, &label, format!("error — {e}"));
            return format!("[search agent error on \"{display}\": {e}]");
        }
    };
    let text = buf.trim();
    if text.is_empty() {
        send_stage(ctx.tx, ctx.ids, &label, "error — no findings returned");
        format!("[no findings for \"{display}\"]")
    } else {
        send_stage(
            ctx.tx,
            ctx.ids,
            &label,
            format!("done — answered \"{display}\""),
        );
        text.to_string()
    }
}

// Verify against cached pages only so quotes are checked against the searchers' exact sources.
async fn verify_with_quote_check(
    provider: &OpenRouter,
    model: &str,
    messages: Vec<ChatMessage>,
    cache_only_toolbox: Arc<dyn ToolExecutor>,
    usage: &ResearchUsage,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> String {
    let (buf, failed) = match usage
        .agent(
            (provider, model, "research:verifier"),
            format!("Verifier · {}", usage.topic),
            cache_only_toolbox.as_ref(),
            (messages, RESEARCH_SEARCHER_MAX_ITERS),
            &|step| send_stage(tx, ids, "verifier", format!("working — {step}")),
        )
        .await
    {
        Ok(text) => (text, false),
        Err(e) => {
            send_stage(tx, ids, "verifier", format!("error — {e}"));
            (String::new(), true)
        }
    };
    if !failed {
        if buf.trim().is_empty() {
            send_stage(
                tx,
                ids,
                "verifier",
                "error — no verification output returned",
            );
        } else {
            send_stage(tx, ids, "verifier", "done — source checks complete");
        }
    }
    buf
}

struct SearcherBatch<'a> {
    provider: &'a OpenRouter,
    model: &'a str,
    toolbox: &'a Arc<dyn ToolExecutor>,
    usage: &'a Arc<ResearchUsage>,
    tx: &'a mpsc::UnboundedSender<ResearchMsg>,
    ids: &'a (String, String, String),
    batch: &'a str,
}

struct Run<'a> {
    provider: &'a OpenRouter,
    model: &'a str,
    toolbox: &'a Arc<dyn ToolExecutor>,
    usage: &'a Arc<ResearchUsage>,
    tx: &'a mpsc::UnboundedSender<ResearchMsg>,
    ids: &'a (String, String, String),
    db_path: &'a std::path::Path,
    topic: &'a str,
}

impl Run<'_> {
    fn stage(&self, label: impl Into<String>, detail: impl Into<String>) {
        send_stage(self.tx, self.ids, label, detail);
    }

    async fn agent(&self, messages: Vec<ChatMessage>, label: &str) -> Result<String, String> {
        complete_agent(
            self.provider,
            self.model,
            messages,
            self.usage,
            self.tx,
            self.ids,
            label,
        )
        .await
    }

    async fn search(&self, batch: &str, items: &[(String, String)]) -> Vec<String> {
        let found = run_searchers(
            SearcherBatch {
                provider: self.provider,
                model: self.model,
                toolbox: self.toolbox,
                usage: self.usage,
                tx: self.tx,
                ids: self.ids,
                batch,
            },
            items,
        )
        .await;
        persist_session_sources(self.db_path, &self.ids.0, &found);
        found
    }

    // Use sequence keys: user text can collide through duplicates, prefixes, or SQL LIKE wildcards.
    async fn steer(
        &self,
        steer_rx: &mut mpsc::UnboundedReceiver<String>,
        seq: &mut usize,
        batch: &str,
    ) -> Vec<String> {
        let steers = drain_steers(steer_rx);
        if steers.is_empty() {
            return Vec::new();
        }
        let items: Vec<(String, String)> = steers.iter().map(|s| (s.clone(), s.clone())).collect();
        for s in &steers {
            *seq += 1;
            self.stage(format!("steer #{seq}"), s.clone());
        }
        self.search(batch, &items).await
    }

    async fn critic(&self, draft: &str, second: bool) -> Result<Critique, String> {
        let (label, working) = if second {
            ("critic r2", "working — reviewing the revised draft")
        } else {
            ("critic", "working — checking coverage and contradictions")
        };
        self.stage(label, working);
        let critique = parse_critique(
            &self
                .agent(critic_messages(self.topic, draft), label)
                .await?,
        );
        let detail = match &critique {
            Critique::Satisfied => if second {
                "done — revised draft is complete"
            } else {
                "done — draft is sufficiently complete"
            }
            .to_string(),
            Critique::Gaps(gaps) => {
                let list = gaps
                    .iter()
                    .enumerate()
                    .map(|(i, g)| format!("{}. {g}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                if second {
                    format!("done — {} gaps remain:\n{list}", gaps.len())
                } else {
                    format!("done — found {} coverage gaps:\n{list}", gaps.len())
                }
            }
            Critique::Contradiction(_) => if second {
                "done — contradiction remains"
            } else {
                "done — found a source contradiction"
            }
            .to_string(),
        };
        self.stage(label, detail);
        Ok(critique)
    }
}

async fn run_searchers(batch_ctx: SearcherBatch<'_>, items: &[(String, String)]) -> Vec<String> {
    let SearcherBatch {
        provider,
        model,
        toolbox,
        usage,
        tx,
        ids,
        batch,
    } = batch_ctx;
    let total = items.len();
    send_stage(
        tx,
        ids,
        format!("search {batch}"),
        format!("working — 0/{total} agents complete"),
    );
    let mut set = tokio::task::JoinSet::new();
    for (idx, (prompt, display)) in items.iter().cloned().enumerate() {
        let provider = provider.clone();
        let model = model.to_string();
        let toolbox = toolbox.clone();
        let usage = usage.clone();
        let tx = tx.clone();
        let ids = ids.clone();
        let batch = batch.to_string();
        set.spawn(async move {
            let ctx = SearcherCtx {
                toolbox,
                usage,
                tx: &tx,
                ids: &ids,
            };
            let slot = SearcherSlot { batch, idx, total };
            run_searcher(&provider, &model, &prompt, &display, ctx, slot).await
        });
    }
    let mut done = 0usize;
    let mut findings = Vec::with_capacity(total);
    while let Some(res) = set.join_next().await {
        done += 1;
        send_stage(
            tx,
            ids,
            format!("search {batch}"),
            format!("working — {done}/{total} agents complete"),
        );
        findings.push(res.unwrap_or_else(|e| format!("[search agent panicked: {e}]")));
    }
    send_stage(
        tx,
        ids,
        format!("search {batch}"),
        format!("done — {done}/{total} agents complete"),
    );
    findings
}

pub struct ResearchOptions {
    pub research_provider: OpenRouter,
    pub research_model: String,
    pub embedding_provider: OpenRouter,
    pub embedding_model: String,
    pub db_path: std::path::PathBuf,
    pub incognito: bool,
    pub topic: String,
    pub reply_rx: Option<mpsc::UnboundedReceiver<String>>,
    pub steer_rx: mpsc::UnboundedReceiver<String>,
    pub toolbox: Arc<dyn ToolExecutor>,
    pub tx: mpsc::UnboundedSender<ResearchMsg>,
    pub session_id: String,
    pub prompt_cache_key: String,
    pub cache_ttl_1h: bool,
    pub space_id: String,
    pub space_name: String,
}

pub async fn run_research(mut opts: ResearchOptions) {
    // Every agent reports on one channel, forwarded as research updates; the
    // report waits for the forwarder so no agent's last step trails it.
    let (events, mut agent_rx) = mpsc::unbounded_channel::<StreamEvent>();
    let forward = {
        let tx = opts.tx.clone();
        let ids = (
            opts.session_id.clone(),
            opts.space_id.clone(),
            opts.space_name.clone(),
        );
        tokio::spawn(async move {
            while let Some(event) = agent_rx.recv().await {
                if let StreamEvent::Subagent(update) = event {
                    let _ = tx.send((
                        ids.0.clone(),
                        ids.1.clone(),
                        ids.2.clone(),
                        ResearchUpdate::Subagent(update),
                    ));
                }
            }
        })
    };
    let result = run_research_inner(&mut opts, events).await;
    // An aborted context-gathering task may still hold a sender briefly.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), forward).await;
    let _ = opts.tx.send((
        opts.session_id,
        opts.space_id,
        opts.space_name,
        ResearchUpdate::Done(result),
    ));
}

async fn local_known_chunks(
    provider: &OpenRouter,
    embedding_model: &str,
    db_path: &std::path::Path,
    space_id: &str,
    topic: &str,
) -> Vec<String> {
    if embedding_model.trim().is_empty() {
        return Vec::new();
    }
    let Ok(mut vecs) = provider
        .embed(embedding_model, vec![topic.to_string()])
        .await
    else {
        return Vec::new();
    };
    if vecs.is_empty() {
        return Vec::new();
    }
    let query = vecs.remove(0);
    let Ok(conn) = crate::db::open_attached(db_path) else {
        return Vec::new();
    };
    crate::db::semantic_chunks(&conn, space_id, &query, 5)
        .map(|hits| {
            hits.into_iter()
                .map(|(name, loc, text, _)| format!("{name} ({loc}): {text}"))
                .collect()
        })
        .unwrap_or_default()
}

async fn await_survey_reply(
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    reply_rx: &mut mpsc::UnboundedReceiver<String>,
    questions: &[String],
    round: u8,
) -> Option<String> {
    let _ = tx.send((
        ids.0.clone(),
        ids.1.clone(),
        ids.2.clone(),
        ResearchUpdate::SurveyReady {
            questions: questions.to_vec(),
            round,
        },
    ));
    reply_rx.recv().await.map(|r| r.trim().to_string())
}

async fn run_user_survey(
    provider: &OpenRouter,
    model: &str,
    topic: &str,
    reply_rx: &mut mpsc::UnboundedReceiver<String>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    usage: &ResearchUsage,
) -> Result<Vec<(String, String)>, String> {
    let mut rounds: Vec<(String, String)> = Vec::new();
    let mut history = survey_messages(topic, &[]);
    let initial = complete_text(provider, model, history.clone(), usage, "research:survey")
        .await
        .map_err(|e| format!("survey agent failed: {e}"))?;
    history.push(ChatMessage::text("assistant", initial.clone()));
    let mut questions = parse_survey_reply(&initial);
    let mut raw = initial;
    let mut round: u8 = 1;
    loop {
        match questions {
            SurveyReply::Complete => return Ok(rounds),
            SurveyReply::Malformed => {
                return Err(format!(
                    "survey agent returned unusable output (raw reply: {raw:.200})"
                ));
            }
            SurveyReply::Questions(qs) if qs.is_empty() || round > MAX_SURVEY_ROUNDS => {
                return Ok(rounds);
            }
            SurveyReply::Questions(qs) => {
                let Some(reply) = await_survey_reply(tx, ids, reply_rx, &qs, round).await else {
                    return Err("survey cancelled — the reply channel closed".to_string());
                };
                if reply.is_empty() {
                    return Ok(rounds);
                }
                let asked = qs.join("\n");
                rounds.push((asked.clone(), reply.clone()));
                round += 1;
                if round > MAX_SURVEY_ROUNDS {
                    return Ok(rounds);
                }
                // Append to the exact prior request to preserve the provider's cached prefix.
                history.push(ChatMessage::text(
                    "user",
                    format!("Questions asked:\n{asked}\nUser answer: {reply}"),
                ));
                raw = complete_text(provider, model, history.clone(), usage, "research:survey")
                    .await
                    .map_err(|e| format!("survey follow-up failed: {e}"))?;
                history.push(ChatMessage::text("assistant", raw.clone()));
                questions = parse_survey_reply(&raw);
            }
        }
    }
}

pub(super) struct PlanApprovalContext<'a> {
    pub(super) provider: &'a OpenRouter,
    pub(super) model: &'a str,
    pub(super) topic: &'a str,
    pub(super) usage: &'a ResearchUsage,
    pub(super) tx: &'a mpsc::UnboundedSender<ResearchMsg>,
    pub(super) ids: &'a (String, String, String),
}

// Approval must fail closed: searchers must never execute an unapproved plan.
pub(super) async fn await_plan_approval(
    ctx: PlanApprovalContext<'_>,
    questions: &mut Vec<PlanQuestion>,
    reply_rx: &mut mpsc::UnboundedReceiver<String>,
) -> Result<(), String> {
    let PlanApprovalContext {
        provider,
        model,
        topic,
        usage,
        tx,
        ids,
    } = ctx;
    let mut rework = false;
    loop {
        let _ = tx.send((
            ids.0.clone(),
            ids.1.clone(),
            ids.2.clone(),
            ResearchUpdate::PlanReady {
                questions: questions.clone(),
                rework,
            },
        ));
        let Some(reply) = reply_rx.recv().await else {
            return Err(
                "plan approval cancelled — the reply channel closed before the plan was approved"
                    .to_string(),
            );
        };
        if reply.trim().is_empty() {
            return Ok(());
        }
        let text = complete_text(
            provider,
            model,
            plan_approval_messages(topic, questions, &reply),
            usage,
            "research:plan approval",
        )
        .await
        .map_err(|e| format!("plan approval agent failed: {e}"))?;
        match parse_approval(&text) {
            Approval::Approved => return Ok(()),
            Approval::Revised(revised) if !revised.is_empty() => {
                if rework {
                    return Err(
                        "plan was revised twice — rework cap reached; re-run /research \
                         with the final plan"
                            .to_string(),
                    );
                }
                *questions = revised;
                rework = true;
            }
            Approval::Revised(_) | Approval::Malformed => {
                return Err(format!(
                    "plan approval agent returned an unusable verdict (raw reply: {text:.200})"
                ));
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn run_research_inner(
    opts: &mut ResearchOptions,
    events: mpsc::UnboundedSender<StreamEvent>,
) -> Result<String, String> {
    let ids = &(
        opts.session_id.clone(),
        opts.space_id.clone(),
        opts.space_name.clone(),
    );
    let prompt_cache_key = opts.prompt_cache_key.clone();
    let cache_ttl_1h = opts.cache_ttl_1h;
    let ResearchOptions {
        research_provider,
        research_model,
        embedding_provider,
        embedding_model,
        db_path,
        incognito,
        topic,
        reply_rx,
        steer_rx,
        toolbox,
        tx,
        ..
    } = &mut *opts;
    let db_path = db_path.as_path();
    let usage = Arc::new(ResearchUsage {
        db_path: db_path.to_path_buf(),
        session_id: if *incognito {
            String::new()
        } else {
            ids.0.clone()
        },
        prompt_cache_key,
        cache_ttl_1h,
        space_id: ids.1.clone(),
        topic: topic.clone(),
        events,
        slots: Arc::new(tokio::sync::Semaphore::new(RESEARCH_AGENTS)),
    });
    let run = Run {
        provider: research_provider,
        model: research_model,
        toolbox,
        usage: &usage,
        tx,
        ids,
        db_path,
        topic,
    };
    let gather_task = {
        let provider = embedding_provider.clone().logged(usage.log(), "embed");
        let model = embedding_model.clone();
        let db_path = db_path.to_path_buf();
        let space_id = ids.1.clone();
        let topic = topic.clone();
        let research_provider = research_provider.clone();
        let research_model = research_model.clone();
        let toolbox = toolbox.clone();
        let usage = usage.clone();
        let tx = tx.clone();
        let ids = ids.clone();
        tokio::spawn(async move {
            let known =
                async { local_known_chunks(&provider, &model, &db_path, &space_id, &topic).await };
            let survey = async {
                send_stage(
                    &tx,
                    &ids,
                    "web survey",
                    "working — mapping the topic, debates, evidence, and source landscape",
                );
                let survey_question = format!(
                    "Conduct a broad preliminary survey of this research topic before planning: {topic}. \
                     Identify the major concepts, current debates, useful source types, and important \
                     evidence gaps."
                );
                let ctx = SearcherCtx {
                    toolbox,
                    usage,
                    tx: &tx,
                    ids: &ids,
                };
                let slot = SearcherSlot {
                    batch: "web survey".to_string(),
                    idx: 0,
                    total: 1,
                };
                let survey = run_searcher(
                    &research_provider,
                    &research_model,
                    &survey_question,
                    &survey_question,
                    ctx,
                    slot,
                )
                .await;
                persist_session_sources(&db_path, &ids.0, std::slice::from_ref(&survey));
                survey
            };
            let (known, survey) = tokio::join!(known, survey);
            (known, survey)
        })
    };
    // Dropping a JoinHandle detaches it; abort the gather task when research is cancelled.
    let gather_guard = super::AbortOnDrop(gather_task.abort_handle());

    let answers: Vec<(String, String)> = if let Some(rx) = reply_rx.as_mut() {
        run_user_survey(
            research_provider,
            research_model,
            topic,
            rx,
            tx,
            ids,
            &usage,
        )
        .await
        .map_err(|e| {
            run.stage("survey", format!("error — {e}"));
            e
        })?
    } else {
        Vec::new()
    };

    let (known, web_survey) = match gather_task.await {
        Ok(v) => v,
        Err(e) if e.is_panic() => return Err(format!("context gathering panicked: {e}")),
        Err(e) => return Err(format!("context gathering was cancelled: {e}")),
    };
    drop(gather_guard);
    let mut planning_context = known;
    if !web_survey.is_empty() && !web_survey.starts_with('[') {
        planning_context.push(format!("Preliminary web survey:\n{web_survey}"));
        run.stage("web survey", "done — landscape mapped for planning");
    } else {
        run.stage(
            "web survey",
            "error — survey failed; planning from local context only",
        );
    }

    run.stage(
        "planner",
        "working — decomposing the surveyed landscape into focused questions",
    );
    let mut questions = match plan(
        research_provider,
        research_model,
        topic,
        &answers,
        &planning_context,
        &usage,
    )
    .await
    {
        Ok(questions) => questions,
        Err(e) => {
            run.stage("planner", format!("error — {e}"));
            return Err(e);
        }
    };
    run.stage(
        "planner",
        format!("done — proposed {} questions", questions.len()),
    );

    if let Some(rx) = reply_rx.as_mut() {
        await_plan_approval(
            PlanApprovalContext {
                provider: research_provider,
                model: research_model,
                topic,
                usage: &usage,
                tx,
                ids,
            },
            &mut questions,
            rx,
        )
        .await?;
    }

    let pinned = crate::db::open_conn(db_path)
        .ok()
        .and_then(|conn| crate::db::pinned_urls(&conn, &ids.0).ok())
        .unwrap_or_default();

    let mut findings: Vec<String> = if !web_survey.is_empty() && !web_survey.starts_with('[') {
        vec![format!("--- Survey overview ---\n{web_survey}")]
    } else {
        Vec::new()
    };
    let searcher_items: Vec<(String, String)> = questions
        .iter()
        .map(|q| (q.prompt(topic), q.question.clone()))
        .collect();
    findings.extend(run.search("round 1", &searcher_items).await);

    let mut steer_seq: usize = 0;
    findings.extend(run.steer(steer_rx, &mut steer_seq, "round 1 steer").await);

    run.stage(
        "synthesizer",
        format!("working — combining {} agent findings", findings.len()),
    );
    let mut synthesizer_history =
        synthesizer_messages(topic, &crate::tools::dedup_source_lines(&findings), &pinned);
    let mut draft = run
        .agent(synthesizer_history.clone(), "synthesizer")
        .await?;
    run.stage("synthesizer", "done — draft assembled");

    let mut critique = run.critic(&draft, false).await?;

    if let Critique::Gaps(gaps) = &critique {
        let mut follow_up_findings = run
            .search(
                "round 2",
                &gaps
                    .iter()
                    .map(|g| (g.clone(), g.clone()))
                    .collect::<Vec<_>>(),
            )
            .await;
        follow_up_findings.extend(run.steer(steer_rx, &mut steer_seq, "round 2 steer").await);
        findings.extend(follow_up_findings.clone());

        run.stage("synthesizer r2", "working — merging follow-up findings");
        // Append findings without rebuilding history to preserve the provider's cached prefix.
        synthesizer_history.push(ChatMessage::text("assistant", draft.clone()));
        synthesizer_history.push(ChatMessage::text(
            "user",
            format!(
                "Additional searcher findings to incorporate:\n{}",
                crate::tools::dedup_source_lines(&follow_up_findings).join("\n\n")
            ),
        ));
        draft = run
            .agent(synthesizer_history.clone(), "synthesizer r2")
            .await?;
        run.stage("synthesizer r2", "done — revised draft assembled");
        critique = run.critic(&draft, true).await?;
    }

    if let Critique::Contradiction(desc) = &critique {
        run.stage(
            "resolver",
            "working — reconciling conflicting source claims",
        );
        let resolution = run
            .agent(
                resolver_messages(topic, &draft, &findings, desc),
                "resolver",
            )
            .await?;
        draft.push_str("\n\n");
        draft.push_str(&resolution);
        run.stage("resolver", "done — contradiction reconciled");
    }

    run.stage(
        "verifier",
        "working — checking claims, citations, and direct quotes",
    );
    let verify_toolbox = Arc::new(
        ToolBox::research(
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            Some(db_path.to_path_buf()),
        )
        .cache_only(),
    );
    let verified_raw = verify_with_quote_check(
        research_provider,
        research_model,
        verifier_messages(topic, &draft, &findings),
        verify_toolbox,
        &usage,
        tx,
        ids,
    )
    .await;
    let verified = if verified_raw.trim().is_empty() {
        draft.clone()
    } else {
        verified_raw
    };

    run.stage("writer", "working — polishing structure and citations");
    match complete_text(
        research_provider,
        research_model,
        writer_messages(topic, &verified, &pinned),
        &usage,
        "research:writer",
    )
    .await
    {
        Ok(report) => {
            run.stage("writer", "done — final report ready");
            Ok(report)
        }
        Err(e) => {
            run.stage("writer", format!("error — {e}"));
            Err(e)
        }
    }
}

fn persist_session_sources(db_path: &std::path::Path, session_id: &str, findings: &[String]) {
    let url_norms = crate::tools::cited_url_norms(findings);
    if url_norms.is_empty() {
        return;
    }
    if let Ok(conn) = crate::db::open_conn(db_path) {
        let _ = crate::db::add_session_sources(&conn, session_id, &url_norms);
    }
}
