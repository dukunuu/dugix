//! Focused nested agent loops with isolated histories and workspace branches.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::jobs::service::{Tree, git};
use crate::provider::openrouter::OpenRouter;
use crate::provider::{
    ChatMessage, ChatParams, StreamEvent, SubagentEvent, SubagentUpdate, ToolCall, ToolDef, Usage,
};
use crate::tools::{ToolBox, ToolExecutor, cap_head_tail, run_scheduled};
use crate::trace::{TraceResponse, TraceStep, TraceToolResult};
use crate::workspace::Workspace;

static NEXT_TASK: AtomicU64 = AtomicU64::new(1);
const REPORT_CAP: usize = 6_000;
/// Sub-agents running at once; more wait for a slot. Parallel helpers mostly buy
/// token spend, and a burst of them trips provider rate limits.
const MAX_CONCURRENT: usize = 3;
/// How often streamed sub-agent text reaches the view, at most.
const DELTA_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

fn slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT))
}

#[cfg(test)]
mod tests;

pub(crate) struct Request<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub params: &'a ChatParams,
    pub tools: &'a [ToolDef],
    pub final_round: bool,
    /// Streamed `(reasoning, content)` chunks of this round, as they arrive.
    pub live: &'a (dyn Fn(&str, &str) + Send + Sync),
}

pub(crate) struct Round {
    pub content: String,
    pub reasoning: Option<String>,
    pub calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

pub(crate) trait Provider: Send + Sync {
    fn round<'a>(
        &'a self,
        request: Request<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Round>> + Send + 'a>>;
}

impl Provider for OpenRouter {
    fn round<'a>(
        &'a self,
        request: Request<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Round>> + Send + 'a>> {
        Box::pin(self.subagent_round(request))
    }
}

/// The session model, request settings and event sink captured for one parent turn.
pub struct Context {
    provider: Arc<dyn Provider>,
    model: String,
    params: ChatParams,
    events: mpsc::UnboundedSender<StreamEvent>,
    /// The run's own concurrency limit; `None` shares the global one.
    slots: Option<Arc<tokio::sync::Semaphore>>,
}

impl Context {
    fn send(&self, id: &str, number: u64, event: SubagentEvent) {
        let _ = self
            .events
            .send(StreamEvent::Subagent(Box::new(SubagentUpdate {
                id: id.to_string(),
                number,
                event,
            })));
    }

    pub(crate) fn new(
        provider: OpenRouter,
        model: String,
        params: ChatParams,
        events: mpsc::UnboundedSender<StreamEvent>,
    ) -> Self {
        Self {
            provider: Arc::new(provider),
            model,
            params,
            events,
            slots: None,
        }
    }

    /// Limit these runs by their own semaphore instead of the global slots, for
    /// a pipeline that fans out wider than chat sub-agents may.
    pub(crate) fn with_slots(mut self, slots: Arc<tokio::sync::Semaphore>) -> Self {
        self.slots = Some(slots);
        self
    }

    fn slots(&self) -> &tokio::sync::Semaphore {
        self.slots.as_deref().unwrap_or_else(|| slots())
    }
}

/// Streamed text batched into `Delta` events, at most one per [`DELTA_EVERY`].
struct Live<'a> {
    context: &'a Context,
    id: &'a str,
    number: u64,
    pending: std::sync::Mutex<(String, String, Instant)>,
}

impl<'a> Live<'a> {
    fn new(context: &'a Context, id: &'a str, number: u64) -> Self {
        Self {
            context,
            id,
            number,
            pending: std::sync::Mutex::new((String::new(), String::new(), Instant::now())),
        }
    }

    fn push(&self, reasoning: &str, content: &str) {
        let mut pending = self.pending.lock().expect("live text");
        pending.0.push_str(reasoning);
        pending.1.push_str(content);
        if pending.2.elapsed() >= DELTA_EVERY {
            self.flush_locked(&mut pending);
        }
    }

    fn flush(&self) {
        self.flush_locked(&mut self.pending.lock().expect("live text"));
    }

    fn flush_locked(&self, pending: &mut (String, String, Instant)) {
        pending.2 = Instant::now();
        if pending.0.is_empty() && pending.1.is_empty() {
            return;
        }
        self.context.send(
            self.id,
            self.number,
            SubagentEvent::Delta {
                reasoning: std::mem::take(&mut pending.0),
                content: std::mem::take(&mut pending.1),
            },
        );
    }
}

fn branch_name(session: &str, number: u64) -> String {
    let short: String = session
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    format!("nexus/sub/{short}/{number}")
}

fn prepare_tree(toolbox: &ToolBox, number: &mut u64) -> Result<(Tree, Arc<Workspace>)> {
    let root = toolbox
        .workspace
        .as_ref()
        .context("task needs an agent workspace")?
        .root();
    let prefix = git(root, &["rev-parse", "--show-prefix"])?;
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/nexus/sub/",
        ],
    )?;
    let refs = String::from_utf8_lossy(&refs.stdout);
    while refs
        .lines()
        .any(|name| name == branch_name(&toolbox.session_id, *number))
    {
        *number = NEXT_TASK.fetch_add(1, Ordering::Relaxed);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut tree = Tree::prepare(
        root,
        &std::env::temp_dir().join("nexus-subagents"),
        "subagent",
        &id,
    )?;
    let branch = branch_name(&toolbox.session_id, *number);
    git(&tree.path, &["branch", "-m", &branch])?;
    tree.branch = Some(branch);
    let path = tree
        .path
        .join(String::from_utf8_lossy(&prefix.stdout).trim());
    let workspace = Arc::new(Workspace::open(&path, Vec::new())?);
    Ok((tree, workspace))
}

fn finish_tree(tree: &Tree, number: u64) -> Result<String> {
    // Stage before collecting the patch so new files appear in the diff too.
    git(&tree.path, &["add", "-A"])?;
    let diff = tree.changes()?;
    let stats = git(&tree.path, &["diff", "HEAD", "--numstat"])?;
    let stats = String::from_utf8_lossy(&stats.stdout);
    let mut added = 0_u64;
    let mut deleted = 0_u64;
    let mut files = Vec::new();
    for line in stats.lines() {
        let mut fields = line.splitn(3, '\t');
        added += fields
            .next()
            .unwrap_or_default()
            .parse::<u64>()
            .unwrap_or(0);
        deleted += fields
            .next()
            .unwrap_or_default()
            .parse::<u64>()
            .unwrap_or(0);
        files.push(fields.next().unwrap_or_default());
    }
    tree.finish("subagent", &number.to_string())?;
    if diff.is_empty() {
        return Ok("no changes; worktree removed".into());
    }
    Ok(format!(
        "changes on branch {}: {} (+{added} −{deleted})\n{}",
        tree.branch.as_deref().context("sub-agent branch")?,
        files.join(", "),
        cap_head_tail(&diff, REPORT_CAP),
    ))
}

/// One task, returning only its final report and optional branch review.
pub(crate) async fn run(context: &Context, toolbox: &ToolBox, args: &Value) -> Result<String> {
    let mut number = NEXT_TASK.fetch_add(1, Ordering::Relaxed);
    let writable = args["tools"] == "workspace";
    let prepared = if writable {
        Some(prepare_tree(toolbox, &mut number)?)
    } else {
        None
    };
    let mut child = toolbox.for_subagent(prepared.as_ref().map(|(_, workspace)| workspace.clone()));
    child.decision_context = Arc::new(std::sync::Mutex::new((
        toolbox.session_id.clone(),
        args["prompt"].as_str().expect("validated prompt").into(),
    )));
    let (model, mut params) = (context.model.clone(), context.params.clone());
    params.prompt_cache_key = params
        .prompt_cache_key
        .map(|key| format!("{key}:sub:{number}"));
    let max_steps = usize::try_from(args["max_steps"].as_u64().unwrap_or(20))?;
    let workspace = child
        .workspace
        .as_ref()
        .context("task needs an agent workspace")?;
    let environment = toolbox.subagent_environment.replace(
        &toolbox
            .workspace
            .as_ref()
            .expect("agent workspace")
            .root()
            .display()
            .to_string(),
        &workspace.root().display().to_string(),
    );
    let mut system = ChatMessage::text(
        "system",
        format!(
            "{}\n\n{environment}\n\n{}\n\nSub-agent capabilities: {}. No task, proc, or user bridge tools are available. Workspace changes stay on a review branch; never try to merge them yourself.",
            crate::prompts::render(crate::prompts::SUBAGENT, &[]),
            crate::prompts::render(crate::prompts::AGENT, &[]),
            if writable {
                "fs and write-confined shell in an isolated worktree"
            } else {
                "read-only fs and shell log only"
            },
        ),
    );
    system.cache_breakpoint = crate::provider::model_needs_cache_breakpoints(&model);
    let prompt = args["prompt"].as_str().expect("validated prompt");
    let messages = vec![system, ChatMessage::text("user", prompt)];
    let id = uuid::Uuid::new_v4().to_string();
    context.send(
        &id,
        number,
        SubagentEvent::Started {
            prompt: prompt.to_string(),
            model: model.clone(),
            writable,
            max_steps,
        },
    );
    let slot = if let Ok(permit) = context.slots().try_acquire() {
        permit
    } else {
        let _ = context.events.send(StreamEvent::Status(format!(
            "sub-agent waiting: {MAX_CONCURRENT} already running"
        )));
        context
            .slots()
            .acquire()
            .await
            .context("sub-agent slots closed")?
    };
    let outcome = run_loop(
        context,
        &child,
        &model,
        &params,
        messages,
        max_steps,
        (number, &id),
    )
    .await
    .map(|done| {
        format!(
            "sub-agent report ({model}, {} steps, {} tokens):\n{}",
            done.steps,
            done.tokens,
            cap_head_tail(&done.content, REPORT_CAP)
        )
    });
    drop(slot);
    let review = prepared
        .as_ref()
        .map(|(tree, _)| finish_tree(tree, number))
        .transpose();
    conclude(context, (number, &id), outcome, review)
}

/// A nested agent outside the `task` tool, such as one of the swarm's drafters:
/// the caller supplies the whole conversation and the tools, and gets the final
/// answer back uncapped. It shows in `/agents` like any sub-agent.
pub(crate) struct Role {
    /// What `/agents` lists the run as; its first line is the title.
    pub title: String,
    pub messages: Vec<ChatMessage>,
    pub max_steps: usize,
}

pub(crate) async fn run_role(
    context: &Context,
    tools: &dyn ToolExecutor,
    role: Role,
) -> Result<String> {
    let number = NEXT_TASK.fetch_add(1, Ordering::Relaxed);
    let id = uuid::Uuid::new_v4().to_string();
    // Unlike a task, a role usually shares its caller's history, so it keeps the
    // caller's cache key and reuses that prefix.
    let params = context.params.clone();
    context.send(
        &id,
        number,
        SubagentEvent::Started {
            prompt: role.title,
            model: context.model.clone(),
            writable: false,
            max_steps: role.max_steps,
        },
    );
    let slot = context
        .slots()
        .acquire()
        .await
        .context("sub-agent slots closed")?;
    let outcome = run_loop(
        context,
        tools,
        &context.model,
        &params,
        role.messages,
        role.max_steps,
        (number, &id),
    )
    .await
    .and_then(|done| {
        anyhow::ensure!(!done.content.trim().is_empty(), "returned an empty answer");
        Ok(done.content)
    });
    drop(slot);
    context.send(
        &id,
        number,
        SubagentEvent::Finished {
            error: outcome.as_ref().err().map(ToString::to_string),
            review: None,
        },
    );
    outcome
}

/// The last round's text and what the run took.
struct Done {
    content: String,
    steps: usize,
    tokens: u64,
}

fn conclude(
    context: &Context,
    (number, id): (u64, &str),
    outcome: Result<String>,
    review: Result<Option<String>>,
) -> Result<String> {
    let error = match (&outcome, &review) {
        (Err(error), _) => Some(error.to_string()),
        (_, Err(error)) => Some(format!("review branch failed: {error}")),
        _ => None,
    };
    context.send(
        id,
        number,
        SubagentEvent::Finished {
            error,
            review: review.as_ref().ok().cloned().flatten(),
        },
    );
    let review = review?;
    let mut report = outcome.unwrap_or_else(|error| format!("sub-agent failed: {error}"));
    if let Some(review) = review {
        report.push_str("\n\n");
        report.push_str(&review);
    }
    Ok(report)
}

async fn run_loop(
    context: &Context,
    toolbox: &dyn ToolExecutor,
    model: &str,
    params: &ChatParams,
    mut messages: Vec<ChatMessage>,
    max_steps: usize,
    (number, id): (u64, &str),
) -> Result<Done> {
    let tools = toolbox.defs();
    let mut tokens = 0;
    for step in 0..=max_steps {
        let _ = context.events.send(StreamEvent::Status(format!(
            "sub-agent {number}: step {}/{max_steps} · /agents to watch",
            (step + 1).min(max_steps)
        )));
        let started = Instant::now();
        let live = Live::new(context, id, number);
        let round = context
            .provider
            .round(Request {
                model,
                messages: &messages,
                params,
                tools: &tools,
                final_round: step == max_steps,
                live: &|reasoning, content| live.push(reasoning, content),
            })
            .await;
        live.flush();
        let round = round?;
        // Usage is recorded by the sub-agent's own client, under its own
        // model; forwarding it would price it as the session model and move
        // the parent's context bar.
        if let Some(usage) = round.usage {
            tokens += usage.total_tokens;
        }
        let done = round.calls.is_empty() || step == max_steps;
        let request = messages.clone();
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: round.content.clone(),
            reasoning_content: round.reasoning.clone(),
            tool_calls: (!round.calls.is_empty()).then(|| round.calls.clone()),
            ..Default::default()
        });
        let mut results = Vec::new();
        if !done {
            let pending: Vec<_> = round
                .calls
                .iter()
                .map(|call| (call.name.clone(), call.arguments.clone()))
                .collect();
            for (call, (full, _)) in round
                .calls
                .iter()
                .zip(run_scheduled(toolbox, &pending).await)
            {
                let seen = params
                    .tool_result_cap
                    .map_or_else(|| full.clone(), |cap| cap_head_tail(&full, cap));
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: seen.clone(),
                    tool_call_id: Some(call.id.clone()),
                    ..Default::default()
                });
                results.push(TraceToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    full,
                    seen,
                });
            }
        }
        context.send(
            id,
            number,
            SubagentEvent::Round {
                step,
                reasoning: round.reasoning.clone(),
                content: round.content.clone(),
                calls: round
                    .calls
                    .iter()
                    .cloned()
                    .zip(results.iter().map(|result| result.seen.clone()))
                    .collect(),
                usage: round.usage,
                secs: started.elapsed().as_secs_f64(),
            },
        );
        record_step(
            model, params, &tools, request, &round, results, step, number, started,
        )
        .await;
        if done {
            return Ok(Done {
                content: round.content,
                steps: step,
                tokens,
            });
        }
    }
    unreachable!("the last round always returns a report")
}

#[allow(clippy::too_many_arguments)] // One trace record, mirroring the parent loop.
async fn record_step(
    model: &str,
    params: &ChatParams,
    tools: &[ToolDef],
    messages: Vec<ChatMessage>,
    round: &Round,
    results: Vec<TraceToolResult>,
    step: usize,
    number: u64,
    started: Instant,
) {
    let Some(trace) = &params.trace else { return };
    let record = TraceStep {
        session_id: trace.session_id.clone(),
        space_id: trace.space_id.clone(),
        turn_id: trace.turn_id.clone(),
        round: u32::try_from(step).unwrap_or(u32::MAX),
        model: model.to_string(),
        backend: trace.backend.clone(),
        system: messages[0].content.clone(),
        tools: tools.to_vec(),
        messages: messages[1..].to_vec(),
        params: json!({ "reasoning_effort": params.reasoning_effort, "temperature": params.temperature, "top_p": params.top_p, "max_tokens": params.max_tokens }),
        response: TraceResponse {
            content: round.content.clone(),
            reasoning: round.reasoning.clone(),
            tool_calls: round.calls.clone(),
            finish: if round.calls.is_empty() {
                "stop"
            } else {
                "tool_calls"
            }
            .into(),
        },
        usage: round.usage,
        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        tool_results: results,
    };
    let store = trace.store.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(id) = store.record_step(&record) {
            let _ = store.add_label(&id, "subagent", &number.to_string());
        }
    })
    .await;
}
