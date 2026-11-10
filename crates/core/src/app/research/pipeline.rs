//! The background pipeline: usage accounting, searcher fan-out, survey/plan gates and orchestration.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::prompts;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatMessage, ChatParams, StreamEvent, Usage};
use crate::tools::{ToolBox, ToolExecutor};

use super::prompts::{
    critic_messages, plan_approval_messages, planner_messages_with_context, research_stage_prompt,
    resolver_messages, survey_messages, synthesizer_messages, verifier_messages, writer_messages,
};
use super::{
    Approval, Critique, MAX_SURVEY_ROUNDS, PlanQuestion, RESEARCH_SEARCHER_MAX_ITERS, ResearchMsg,
    ResearchUpdate, SEARCHER_PROMPT, SurveyReply, parse_approval, parse_critique,
    parse_plan_blocks, parse_survey_reply,
};

/// Content-free usage sink for background research requests. Each stage uses
/// the same provider accounting as interactive chat without putting prompt
/// text into the database.
#[derive(Clone)]
pub(super) struct ResearchUsage {
    pub(super) db_path: std::path::PathBuf,
    pub(super) session_id: String,
    pub(super) prompt_cache_key: String,
    pub(super) cache_ttl_1h: bool,
    pub(super) space_id: String,
    pub(super) backend: &'static str,
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

    fn record(&self, model: &str, usage: Usage, kind: &str) {
        if let Some(log) = self.log() {
            log.record(self.backend, model, usage, kind);
        }
    }
}

/// Send the `(session_id, space_id, space_name)` triple's stage update.
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

/// Every steer instruction queued since the last drain, without blocking —
/// `try_recv` until the channel is empty. Called at each round boundary so
/// a user's mid-flight `/steer` gets picked up as an extra searcher round.
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
    let params = usage.chat_params();
    provider
        .complete_with_params(model, messages, &params)
        .await
        .map(|completion| {
            usage.record(model, completion.usage.unwrap_or_default(), kind);
            completion.text.trim().to_string()
        })
        .map_err(|e| e.to_string())
}

/// Run a non-streaming pipeline agent and terminalize its activity row on
/// failure. The caller owns the stage-specific success detail.
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

/// One Searcher agent: given one focused sub-question prompt, runs the normal
/// tool-loop (restricted to `search/fetch_url`) and returns its final prose
/// findings (including its own "Sources:" citation list). Never returns an
/// `Err` — a dead search/fetch/model call becomes a placeholder finding
/// string so one bad sub-question can't sink the whole pipeline.
///
/// `prompt` is the full block handed to the model (topic + why/angles/sources
/// brief — detail is functional); `display` is the short label used in the
/// live activity rows, so a searcher's status never leaks the whole brief.
///
/// Every `Status`/`ToolCall` event along the way is forwarded as a live
/// stage update under this searcher's own label (`searcher N/total`), so the
/// UI shows what it's actually doing (searching, fetching a URL, etc.) in
/// real time instead of going silent until it finishes.
#[allow(clippy::too_many_arguments)]
/// The execution plumbing a searcher agent shares with the rest of the
/// research pipeline: the tool box, the job's stage-update channel, and the
/// job's session/space identity.
pub struct SearcherCtx<'a> {
    pub toolbox: Arc<dyn ToolExecutor>,
    usage: Arc<ResearchUsage>,
    pub tx: &'a mpsc::UnboundedSender<ResearchMsg>,
    pub ids: &'a (String, String, String),
}

/// Which slot in which batch a searcher agent occupies — its stage-row
/// identity (`searcher {batch} {idx}/{total}`) in the live activity view.
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
    // Include the batch in the identity so follow-up and steered agents never
    // overwrite earlier activity rows that happen to have the same index.
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
    let tools = ctx.toolbox.defs();
    let kind = if slot.batch == "web survey" {
        "research:web survey"
    } else {
        "research:searcher"
    };
    let (mut rx, abort) = provider.clone().logged(ctx.usage.log(), kind).stream_chat(
        model.to_string(),
        messages,
        ctx.usage.chat_params(),
        tools,
        ctx.toolbox,
        RESEARCH_SEARCHER_MAX_ITERS,
    );
    let _abort = super::AbortOnDrop(abort);
    let mut buf = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Token(t) => buf.push_str(&t),
            StreamEvent::Status(s) => {
                send_stage(ctx.tx, ctx.ids, &label, format!("working — {s}"));
            }
            StreamEvent::ToolCall {
                name,
                arguments,
                result,
                ..
            } => {
                let summary = crate::app::tool_call_summary(&name, &arguments, &result);
                send_stage(ctx.tx, ctx.ids, &label, format!("working — {summary}"));
            }
            StreamEvent::Error(e) => {
                send_stage(ctx.tx, ctx.ids, &label, format!("error — {e}"));
                return format!("[search agent error on \"{display}\": {e}]");
            }
            StreamEvent::Done => break,
            StreamEvent::Usage(_) | StreamEvent::Reasoning(_) | StreamEvent::Subagent(_) => {}
        }
    }
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

/// Run the Verifier stage with a cache-only toolbox so it can check direct
/// quotes against exactly the pages searchers already gathered (never a
/// fresh fetch). Never returns an `Err` — returns whatever text accumulated
/// before the stream ended, or an empty string if it errored before
/// producing any. The caller falls back to the unverified draft when this
/// comes back empty, so verification failing must never blank out an
/// otherwise-good report.
async fn verify_with_quote_check(
    provider: &OpenRouter,
    model: &str,
    messages: Vec<ChatMessage>,
    cache_only_toolbox: Arc<dyn ToolExecutor>,
    usage: &ResearchUsage,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> String {
    let tools = cache_only_toolbox.defs();
    let (mut rx, abort) = provider
        .clone()
        .logged(usage.log(), "research:verifier")
        .stream_chat(
            model.to_string(),
            messages,
            usage.chat_params(),
            tools,
            cache_only_toolbox,
            RESEARCH_SEARCHER_MAX_ITERS,
        );
    let _abort = super::AbortOnDrop(abort);
    let mut buf = String::new();
    let mut failed = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Token(t) => buf.push_str(&t),
            StreamEvent::Status(s) => send_stage(tx, ids, "verifier", format!("working — {s}")),
            StreamEvent::ToolCall {
                name,
                arguments,
                result,
                ..
            } => {
                let summary = crate::app::tool_call_summary(&name, &arguments, &result);
                send_stage(tx, ids, "verifier", format!("working — {summary}"));
            }
            StreamEvent::Error(e) => {
                failed = true;
                send_stage(tx, ids, "verifier", format!("error — {e}"));
                break;
            }
            StreamEvent::Done => break,
            StreamEvent::Usage(_) | StreamEvent::Reasoning(_) | StreamEvent::Subagent(_) => {}
        }
    }
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

/// The shared plumbing for one searcher fan-out batch.
struct SearcherBatch<'a> {
    provider: &'a OpenRouter,
    model: &'a str,
    toolbox: &'a Arc<dyn ToolExecutor>,
    usage: &'a Arc<ResearchUsage>,
    tx: &'a mpsc::UnboundedSender<ResearchMsg>,
    ids: &'a (String, String, String),
    batch: &'a str,
}

/// What every stage of one research run shares.
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
    /// Send a stage update for this run.
    fn stage(&self, label: impl Into<String>, detail: impl Into<String>) {
        send_stage(self.tx, self.ids, label, detail);
    }

    /// Run an agent and terminalize its activity row on failure.
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

    /// Run a searcher batch and persist its sources.
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

    // One stage row per drained steer, keyed by a job-global sequence number
    // (`steer #N` — N = the steer's 1-based queue position, which equals its
    // drain order). The stage upsert matches rows by label, so the key must
    // never be user text: duplicate, prefix-of-each-other, or LIKE-wildcard
    // (`%`/`_`) steer text would collapse or hijack rows and leave picked-up
    // steers looking queued (the live popup derives picked-up from the same
    // numbered keys).
    /// Drain queued steers and search them with distinct stage rows.
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

    /// Critique a draft and report its coverage and contradiction verdict.
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
            // Surface the actual gap questions, not just a count — the
            // follow-up searchers are about to investigate exactly these.
            Critique::Gaps(gaps) => {
                let list = gaps
                    .iter()
                    .enumerate()
                    .map(|(i, g)| format!("{}. {g}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                // Same shape as round 1: the remaining gap questions, not a count.
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

/// Fan out one Searcher per question in parallel, sending a running
/// `{done}/{total}` stage update as each finishes (in addition to each
/// searcher's own live per-tool-call feed). Order of the returned findings
/// doesn't matter (synthesis treats them as an unordered set). Each item is
/// `(prompt, display)`: the full prompt goes to the agent, the short display
/// label goes into the activity rows.
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

/// Run the full pipeline and send exactly one final `Done` on `tx` (the
/// caller's channel then closes naturally when this function returns and
/// `tx` is dropped).
/// Everything `run_research` needs to start a gated or ungated research job.
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
    let result = run_research_inner(&mut opts).await;
    let _ = opts.tx.send((
        opts.session_id,
        opts.space_id,
        opts.space_name,
        ResearchUpdate::Done(result),
    ));
}

/// Top-k chunks from the space's files already relevant to `topic`, for the
/// Planner's "already known" context — silently empty when embeddings are
/// unconfigured, embedding fails, or the space has no files (never blocks
/// `/research` on any of those).
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

/// One survey round's questions, sent to the UI and awaited: park on
/// `reply_rx` until the user answers (or the job is stopped and the channel
/// drops). Returns the user's trimmed reply, or `None` when the channel
/// closed.
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

/// The conversational survey: the scoping agent asks what the user wants
/// (1–3 rounds), the user answers in chat, and the agent declares the survey
/// complete once it has enough — no phrase-matching in app code. An empty
/// reply (Enter on an empty input) skips ahead. Request failures (auth,
/// rate limits, network), malformed agent output, and a closed reply channel
/// all propagate as `Err` — the survey is a promised phase of the
/// conversational flow, not an optional garnish, so it must not silently
/// skip and still report success. Returns the (questions, answer) rounds
/// for the planner's context.
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
                    // The reply channel closed — the job is being torn down
                    // (or a stop raced the parked gate). Don't keep planning
                    // as if the scoping happened.
                    return Err("survey cancelled — the reply channel closed".to_string());
                };
                if reply.is_empty() {
                    return Ok(rounds); // Empty Enter = skip the rest.
                }
                let asked = qs.join("\n");
                rounds.push((asked.clone(), reply.clone()));
                round += 1;
                if round > MAX_SURVEY_ROUNDS {
                    return Ok(rounds);
                }
                // Preserve the exact prior request and append only the
                // user's answer. Rebuilding one growing user string changes
                // the serialized history and forfeits the provider's cached
                // prefix on every survey round.
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

/// The shared plumbing for the plan-approval agent.
pub(super) struct PlanApprovalContext<'a> {
    pub(super) provider: &'a OpenRouter,
    pub(super) model: &'a str,
    pub(super) topic: &'a str,
    pub(super) usage: &'a ResearchUsage,
    pub(super) tx: &'a mpsc::UnboundedSender<ResearchMsg>,
    pub(super) ids: &'a (String, String, String),
}

/// The plan-approval phase: present the plan, park for a chat reply, and fold
/// edits back in via the approval agent. An empty reply (Enter) or an
/// agent-recognized "approve" runs the questions as-is; edits are re-presented
/// once (`rework` cap) for a final approval. A second edit, a failed approval
/// call, malformed agent output, or a closed reply channel (job teardown
/// racing the parked gate) fails visibly (`Err`) — searchers never run on a
/// plan the user hasn't approved, and approval never fails open.
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
            // The reply channel closed without an approval (job teardown, or
            // a stop racing the parked gate). Fail visibly rather than
            // running searchers on a plan the user hasn't approved.
            return Err(
                "plan approval cancelled — the reply channel closed before the plan was approved"
                    .to_string(),
            );
        };
        if reply.trim().is_empty() {
            return Ok(()); // Enter on an empty input = approve.
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
                    // Second edit: never silently folded in and run. The
                    // rework cap is one re-presentation — fail visibly
                    // rather than execute an unapproved revision.
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

// Long by design (pipeline orchestration).
#[allow(clippy::too_many_lines)]
async fn run_research_inner(opts: &mut ResearchOptions) -> Result<String, String> {
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
        backend: research_provider.backend_tag().name(),
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
    // Gather the planning context (local chunks + a web landscape survey)
    // concurrently with the conversational survey — the user answers while
    // the ground truth arrives, so the plan targets real gaps. The two
    // gatherers also run concurrently with each other.
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
    // Abort-on-drop guard: dropping the JoinHandle alone would detach the
    // gather task instead of cancelling it, so web calls and DB writes could
    // keep running after the user stops research. Aborting the outer task
    // drops this handle, which cancels the gather task.
    let gather_guard = super::AbortOnDrop(gather_task.abort_handle());

    // Phase 1: the conversational survey (skipped entirely for `/research!`).
    // Failures propagate visibly — the survey is a promised phase, and a
    // silent skip would report success without the user's scoping input.
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

    // Join the concurrent gathering and fold it into planning context. A
    // panic (or cancellation) inside the gather task terminates the job with
    // context — defaulting to empty context would mask a programming failure
    // and let the pipeline report success on invented ground truth.
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

    // Phase 2: plan approval — reply in chat to approve or change it. The
    // gate parks with no timeout; Ctrl+↑ then Ctrl+X (the live view's stop)
    // is the escape hatch. Skipped entirely for `/research!`. Approval is
    // fail-closed: any failure here returns `Err` and research stops rather
    // than running searchers on an unapproved plan.
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
        // Successful web survey: seed it into the findings so synthesis can
        // cite the landscape overview alongside each answer's own citations.
        vec![format!("--- Survey overview ---\n{web_survey}")]
    } else {
        Vec::new()
    };
    // Each searcher gets the full question block as its prompt (detail is
    // functional) but only the bare question as its live display label, so
    // the activity rows stay short.
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
        // Continue the original synthesis conversation instead of
        // rebuilding one giant user message. The earlier request remains an
        // exact prefix and only the new findings are appended.
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

/// Link every URL cited in `findings` into the session's source bundle
/// (they're already in `web_cache` from `fetch_url`'s write-through). Best
/// effort — a failed write never disturbs the pipeline.
fn persist_session_sources(db_path: &std::path::Path, session_id: &str, findings: &[String]) {
    let url_norms = crate::tools::cited_url_norms(findings);
    if url_norms.is_empty() {
        return;
    }
    if let Ok(conn) = crate::db::open_conn(db_path) {
        let _ = crate::db::add_session_sources(&conn, session_id, &url_norms);
    }
}
