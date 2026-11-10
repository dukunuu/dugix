//! Tools the model can call mid-response, advertised as nine consolidated
//! names (`batch`, `skills`, `scripts`, `search`, `fetch_url`,
//! `research_lookup`, `files`, `app`, `media`) that dispatch onto a larger
//! set of specialized implementations below, behind the `ToolExecutor` seam.

mod apps;
mod canvas;
mod chats;
mod effect;
mod files;
mod fs;
mod hashline;
#[cfg(test)]
mod image_tests;
#[cfg(test)]
mod keyless_search_tests;
mod media;
mod memory_tool;
mod scripts;
#[cfg(test)]
mod search_tests;
mod spec;
mod web;
mod web_search;

pub use effect::{Effect, plan_waves};
#[cfg(test)]
pub(crate) use files::unreadable_file_message_for_test;
pub(crate) use hashline::{apply_hashline_edits, number_lines_with_hash};
pub use web::{cited_url_norms, dedup_source_lines, normalize_url, rewrite_query_with_domains};

use fs::{APPROVAL_DETAIL_LINES, fs_approval_summary, fs_diff, fs_relative, fs_replacement};
use hashline::number_lines;
use web::{
    DUCKDUCKGO_HTML, DiscussionHit, DiscussionSearch, ImageSearch, REDDIT_RATE_LIMIT, SearchHit,
    brave_search, duckduckgo_search, fetch_url_text, fetch_youtube_transcript, format_web_results,
    is_youtube_url, langsearch_search, searxng_search,
};

// Test modules reach moved helpers through `use super::*`.
#[cfg(test)]
use files::*;
#[cfg(test)]
use fs::*;
#[cfg(test)]
use hashline::line_hash;
#[cfg(test)]
use scripts::run_cmd_env;
#[cfg(test)]
use web::*;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(test)]
use base64::Engine;
#[cfg(test)]
use sha2::{Digest, Sha256};

use crate::decider::{Chain, Class, Decider, DecisionRequest, Verdict, Verdicts};
use crate::provider::ToolDef;
use crate::provider::openrouter::OpenRouter;
use crate::skills::{load_skills_from_dirs, resolve_skill_file, skill_body};
use crate::workspace::{Access, Workspace};

const MAX_TOOL_RESULT_CHARS: usize = 8_000;
/// Maximum sub-operations in one `batch` call — bounds execution time and
/// the size of the combined result.
const MAX_BATCH_CALLS: usize = 8;
/// Calls run concurrently within one wave of `run_scheduled`.
const MAX_PARALLEL_CALLS: usize = 8;
/// Combined-result cap for a `batch` call. Each sub-result is already capped
/// at `MAX_TOOL_RESULT_CHARS`; this lets a few of them through together
/// while still bounding what a runaway batch pushes into the conversation.
const MAX_BATCH_RESULT_CHARS: usize = MAX_TOOL_RESULT_CHARS * 4;

const TOOL_ERROR_PREFIXES: &[(&str, &str)] = &[
    ("missing required field", "invalid_args"),
    ("invalid action for", "invalid_args"),
    ("invalid mode for", "invalid_args"),
    ("invalid scope for", "invalid_args"),
    ("invalid tool arguments", "invalid_args"),
    ("invalid path", "invalid_args"),
    ("privilege escalation is blocked", "blocked"),
    ("could not parse this command", "invalid_args"),
    ("timed out after", "upstream"),
    ("unknown tool:", "unknown_tool"),
    ("path escapes", "confinement"),
    ("unknown file:", "not_found"),
    ("unknown skill:", "not_found"),
    ("no such script:", "not_found"),
    ("unknown app", "not_found"),
    ("cannot read ", "not_found"),
    ("write failed:", "io"),
    ("db error:", "io"),
    ("store error:", "io"),
    ("fs.", "stale_edit"),
    ("changed since your last read", "stale_edit"),
];

pub(crate) struct ToolOutcome {
    ok: bool,
    error_kind: Option<&'static str>,
    latency: Duration,
    result_chars: usize,
}

pub fn classify_tool_result(result: &str, status: &str) -> Option<&'static str> {
    let line = result.lines().next().unwrap_or_default().trim();
    let prefix_kind = TOOL_ERROR_PREFIXES.iter().find_map(|(prefix, kind)| {
        let matches = match *prefix {
            "fs." => line.starts_with(prefix) && line.contains("needs a fresh read"),
            "changed since your last read" => line.contains(prefix),
            _ => line.starts_with(prefix),
        };
        matches.then_some(*kind)
    });
    if status == "invalid arguments" || prefix_kind == Some("invalid_args") {
        return Some("invalid_args");
    }
    if status == "blocked" {
        return Some("blocked");
    }
    if line.starts_with("hash ")
        && [
            "not found",
            "matches multiple",
            "targets a line already edited",
        ]
        .iter()
        .any(|suffix| line.contains(suffix))
    {
        return Some("stale_edit");
    }
    if prefix_kind.is_some() {
        return prefix_kind;
    }
    let (prefix, _) = line.split_once(" failed:")?;
    (prefix.starts_with(|c: char| c.is_ascii_lowercase())
        && prefix.bytes().all(|b| b.is_ascii_lowercase() || b == b' '))
    .then_some("upstream")
}

/// A workspace action waiting for the user's permission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub tool: String,
    pub action: String,
    pub summary: String,
    /// Human-readable policy class for workspace actions.
    pub class: Option<String>,
    /// The full preview shown to the person approving (a large write's
    /// whole diff). `summary` stays short because deciders, the decision
    /// log and parked jobs read it; `detail` only ever reaches the popup.
    pub detail: Option<String>,
    /// Whether this action may reuse or create a session-wide grant.
    pub allow_session: bool,
}

/// Permission for one action, its kind for this session, or neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Decision {
    Allow,
    AllowForSession,
    /// Allow now and from here on (a connector tool: policy `allow` in
    /// config, with its definition pinned). Only connector requests offer it.
    AllowAlways,
    Deny,
    /// Fall back to individual prompts for a combined request.
    ReviewEach,
}

/// Permission for every call in a response, or a return to individual prompts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveDecision {
    AllowAll,
    AllowAllForSession,
    ReviewEach,
    DenyAll,
}

/// Pre-approval outcome for one scheduled call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    NotNeeded,
    Approved,
    Denied,
    AskEach,
    /// A per-call policy ticket, consumed by this toolbox at execution.
    Policy(u64),
}

struct PreparedDecision {
    request: ApprovalRequest,
    answer: Verdicts,
    row_id: Option<i64>,
    user_answer: Option<Decision>,
}

/// Interactive permission provider; absent in headless toolboxes.
pub trait Approver: Send + Sync {
    fn request(&self, req: ApprovalRequest) -> Pin<Box<dyn Future<Output = Decision> + Send + '_>>;

    /// Ask once for several actions; providers without a combined prompt review each.
    fn request_many(
        &self,
        _reqs: Vec<ApprovalRequest>,
    ) -> Pin<Box<dyn Future<Output = WaveDecision> + Send + '_>> {
        Box::pin(std::future::ready(WaveDecision::ReviewEach))
    }
}

// Independent capability and privacy switches, not mutually exclusive states.
#[allow(clippy::struct_excessive_bools)]
pub struct ToolBox {
    /// Confined agent filesystem access; absent outside agent mode.
    pub workspace: Option<Arc<Workspace>>,
    /// App-owned background commands, shared across toolbox refreshes.
    pub procs: Arc<crate::workspace::procs::Procs>,
    /// Eval write confinement; auto-approved commands also confine per call.
    pub confine_shell_writes: Option<PathBuf>,
    /// Present only in agent-auto mode, with rules first.
    pub auto_decider: Option<Arc<Chain>>,
    /// Whether unattended shell commands can be write-confined on this device.
    pub sandbox_available: bool,
    /// Session id and last user message for the current turn.
    pub decision_context: Arc<Mutex<(String, String)>>,
    /// Last five completed call summaries, scoped to the live session.
    pub(crate) decision_steps: Arc<Mutex<(String, Vec<String>)>>,
    /// Saved credentials used to redact decision summaries.
    pub decision_scrubber: crate::trace::Scrubber,
    prepared_decisions: Arc<Mutex<HashMap<u64, PreparedDecision>>>,
    next_decision: Arc<AtomicU64>,
    subagents: Option<Arc<crate::subagent::Context>>,
    subagent_workspace: Option<bool>,
    pub(crate) subagent_environment: String,
    decision_parent: Option<Arc<Self>>,
    /// Require permission before mutating workspace calls.
    pub require_approval: bool,
    /// Interactive permission provider; absent on headless paths.
    pub approver: Option<Arc<dyn Approver>>,
    /// Offer `propose_command`: only a terminal UI has a composer to put it in.
    pub interactive_ui: bool,
    /// Session-local studio focus and pending tool edit notifications.
    pub deck_link: Arc<Mutex<crate::deck::link::Context>>,
    /// App-managed destination for newly created or downloaded skills.
    pub skills_dir: PathBuf,
    /// Skill roots in precedence order. These include the app-managed root and
    /// read-only project/user Agent Skills roots discovered at startup.
    skill_dirs: Vec<PathBuf>,
    /// Base URL of a `SearXNG` instance (e.g. `http://localhost:8080`), no
    /// trailing slash. Free and self-hosted — no API key needed.
    pub searxng_url: Option<String>,
    /// `LangSearch` API key (free tier, no card): <https://langsearch.com/dashboard>
    pub langsearch_key: Option<String>,
    /// Preferred web search backend; auto tries configured services, Exa,
    /// the session provider, then HTML scrapers.
    pub search_provider: String,
    web_search: Arc<web_search::WebSearch>,
    /// Auto helper resolved on the session's backend, never another provider.
    pub(crate) search_backend: Option<(OpenRouter, String)>,
    pub(crate) search_space_id: String,
    /// When true, `defs()`/`run()` restrict to `search`/`fetch_url` only —
    /// used for deep-research searcher agents, which must never reach
    /// `scripts`/`app`/`media` tools even if hallucinated.
    research_only: bool,
    /// Domains a per-space setting always excludes from `search(mode=web)` results
    /// (appended to any `exclude_domains` the model passes).
    pub blocked_domains: Vec<String>,
    /// Main db path for tool connections. Connections open the db with its
    /// sibling `cache.db` attached (`open_attached`), so both durable tables
    /// (`session_sources`, `citations`, `files`) and device-local ones
    /// (`web_cache`, `file_chunks`, `model_prices`) resolve on one
    /// connection. `None` disables the cache-backed tools (some tests).
    db_path: Option<PathBuf>,
    /// Opt in to device-local tool outcome recording. Disabled in incognito.
    pub record_events: bool,
    /// Set for follow-up turns inside a `/research` session: enables the
    /// `research_lookup(scope=session_sources)` over that session's gathered source bundle.
    research_session_id: Option<String>,
    client: reqwest::Client,
    discussions: Arc<DiscussionSearch>,
    image_search: ImageSearch,
    /// Remote-image preference shared with image-consuming tools.
    pub remote_images: Option<Arc<crate::remote_image::Fetcher>>,
    /// Test seam: the downloader the `canvas` tool uses for https images. Normally
    /// absent, and each deck gets one rooted at its own `assets/` folder.
    #[cfg(test)]
    pub(crate) deck_images: Option<Arc<crate::remote_image::Fetcher>>,
    /// Test seam: where the `canvas` tool gets Google Fonts. Tests without one
    /// never reach the network.
    #[cfg(test)]
    pub(crate) deck_fonts: Option<Arc<dyn crate::deck::google_fonts::Source>>,
    files: Option<FilesCtx>,
    apps: Option<AppsCtx>,
    /// Whether the current model supports image inputs. When false, tool
    /// image results are returned as text references instead of being
    /// injected as vision content (which would cause a 400).
    pub supports_images: bool,
    /// When true, `fetch_cached` never hits the network on a cache miss —
    /// used for the Verifier stage's quote-checking pass, which must only
    /// ever see pages the searchers actually gathered, never fresh fetches.
    cache_only: bool,
    /// Provider + model for AI image generation. `None` = tool disabled.
    pub image_gen_backend: Option<(OpenRouter, String)>,
    /// Directory to save generated images into / search for reference images.
    pub space_files_dir: PathBuf,
    /// The space's memory book; `None` hides the `memory` tool (incognito,
    /// evals).
    pub memory: Option<MemoryCtx>,
    pub space_apps_dir: PathBuf,
    /// Directory holding space-local scripts (created by the model via the
    /// `scripts` tool).
    pub space_scripts_dir: PathBuf,
    /// Current session id — for attaching generated images to a message.
    pub session_id: String,
    /// Provider + model for video generation. `None` = tools hidden.
    pub video_gen_backend: Option<(OpenRouter, String)>,
    /// Connected MCP connectors; their tools follow the built-in ones.
    pub connectors: Option<crate::connectors::Registry>,
}

/// Where the file tools read from: the shared db plus the space to scope to.
pub use memory_tool::MemoryCtx;

/// The toolbox opens its own short-lived connection per call — the app's
/// `Db` handle stays on the UI task and is never shared with the stream task.
#[derive(Clone)]
pub struct FilesCtx {
    pub db_path: std::path::PathBuf,
    pub space_id: String,
    /// Identity-aware embedding backend for semantic search; None = keyword only.
    pub embedder: Option<crate::provider::embedding::Embedder>,
}

/// Where the app tools write: the active space's apps dir, plus the
/// registry, server port and space metadata. Only present while the server
/// runs.
#[derive(Clone)]
pub struct AppsCtx {
    pub dir: PathBuf,
    pub server_port: u16,
    /// Public host base, when the daemon is exposed through a tunnel.
    pub public_base: Option<String>,
    pub registry: crate::appserver::AppRegistry,
    pub space_name: String,
    pub space_id: String,
    pub space_db_path: PathBuf,
    pub files_dir: PathBuf,
    pub session_id: String,
}

fn tool_def(name: &str, description: &str, parameters: serde_json::Value) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
    }
}

fn required_arg(v: &serde_json::Value, key: &str) -> Result<(), String> {
    match v.get(key).and_then(|value| value.as_str()) {
        Some(value) if !value.trim().is_empty() => Ok(()),
        _ => Err(format!("missing required field: {key}")),
    }
}

fn required_array(v: &serde_json::Value, key: &str) -> Result<(), String> {
    match v.get(key).and_then(|value| value.as_array()) {
        Some(values) if !values.is_empty() => Ok(()),
        _ => Err(format!("missing required field: {key}")),
    }
}

const APP_ACTIONS: &[&str] = &[
    "read",
    "search",
    "write",
    "patch",
    "diff",
    "list",
    "copy_file",
    "copy_images",
    "init",
    "build",
];

/// All public names and actions, including tools hidden by runtime capabilities.
pub(crate) fn current_tools() -> Vec<(&'static str, Vec<&'static str>)> {
    spec::current_tools()
        .chain([("app", APP_ACTIONS.to_vec()), ("batch", Vec::new())])
        .collect()
}

/// Map a retired call to its public name, selector, and rewritten arguments.
/// Action selection shares dispatch routing; required fields are not checked,
/// so recorded definitions can be migrated using only their selector values.
pub(crate) fn migrate_tool_call(
    name: &str,
    mut args: serde_json::Value,
) -> Option<(&'static str, &'static str, serde_json::Value)> {
    let (internal, _) = public_call_impl(name, &args.to_string(), false).ok()?;
    if let Some((tool, selector, action)) = spec::public_alias(&internal) {
        args.as_object_mut()?
            .insert(selector.into(), serde_json::json!(action));
        return Some((tool, selector, args));
    }
    for action in APP_ACTIONS {
        let candidate = serde_json::json!({"action": action}).to_string();
        if public_call_impl("app", &candidate, false).ok()?.0 == internal {
            args.as_object_mut()?
                .insert("action".into(), serde_json::json!(action));
            return Some(("app", "action", args));
        }
    }
    None
}

/// Normalize advertised consolidated calls onto the specialized implementations below.
/// The retired names (`skill_admin`, `app_inspect`, `run_python`, …) are intentionally
/// still accepted by `run()` for persisted/replayed calls, but never returned by `defs()`.
fn public_call(name: &str, args: &str) -> Result<(String, String), String> {
    public_call_impl(name, args, true)
}

// Long by design (tool dispatch).
#[allow(clippy::too_many_lines)]
fn public_call_impl(name: &str, args: &str, validate: bool) -> Result<(String, String), String> {
    if let Some(tool) = spec::find(name) {
        let value: serde_json::Value =
            serde_json::from_str(args).map_err(|e| format!("invalid tool arguments: {e}"))?;
        return Ok((tool.validate(&value)?.to_string(), args.to_string()));
    }
    let public = matches!(
        name,
        "app"
            | "skill_admin"
            | "app_inspect"
            | "app_modify"
            | "app_assets"
            | "script_files"
            | "video_transform"
            | "video_references"
    );
    if !public {
        return Ok((name.to_string(), args.to_string()));
    }
    let value: serde_json::Value =
        serde_json::from_str(args).map_err(|e| format!("invalid tool arguments: {e}"))?;
    let action = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("missing required field: {key}"))
    };
    let required_arg = |value: &serde_json::Value, key: &str| {
        if validate {
            required_arg(value, key)
        } else {
            Ok(())
        }
    };
    let required_array = |value: &serde_json::Value, key: &str| {
        if validate {
            required_array(value, key)
        } else {
            Ok(())
        }
    };
    let mapped = match name {
        "skill_admin" => match action("action")? {
            "create" => {
                required_arg(&value, "name")?;
                required_arg(&value, "description")?;
                "create_skill"
            }
            "install" => {
                required_arg(&value, "source")?;
                "install_skill"
            }
            other => return Err(format!("invalid action for skill_admin: {other}")),
        },
        "app" => match action("action")? {
            "read" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                "read_app_file"
            }
            "search" => {
                required_arg(&value, "app")?;
                required_arg(&value, "pattern")?;
                "grep_app"
            }
            "write" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                required_arg(&value, "content")?;
                "write_file"
            }
            "patch" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                required_array(&value, "edits")?;
                "edit_file"
            }
            "diff" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                if validate && value.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err("missing required field: content".to_string());
                }
                "diff_app"
            }
            "list" => "list_images",
            "copy_file" => {
                required_arg(&value, "app")?;
                required_arg(&value, "file_name")?;
                "copy_file_to_app"
            }
            "copy_images" => {
                required_arg(&value, "app")?;
                required_array(&value, "image_ids")?;
                "copy_images_to_app"
            }
            "init" => {
                required_arg(&value, "app")?;
                "init_app"
            }
            "build" => {
                required_arg(&value, "app")?;
                "build_app"
            }
            other => return Err(format!("invalid action for app: {other}")),
        },
        "app_inspect" => match action("action")? {
            "read" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                "read_app_file"
            }
            "search" => {
                required_arg(&value, "app")?;
                required_arg(&value, "pattern")?;
                "grep_app"
            }
            other => return Err(format!("invalid action for app_inspect: {other}")),
        },
        "app_modify" => match action("action")? {
            "write" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                required_arg(&value, "content")?;
                "write_file"
            }
            "patch" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                required_array(&value, "edits")?;
                "edit_file"
            }
            "diff" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                if validate && value.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err("missing required field: content".to_string());
                }
                "diff_app"
            }
            other => return Err(format!("invalid action for app_modify: {other}")),
        },
        "app_assets" => match action("action")? {
            "list" => "list_images",
            "copy_file" => {
                required_arg(&value, "app")?;
                required_arg(&value, "file_name")?;
                "copy_file_to_app"
            }
            "copy_images" => {
                required_arg(&value, "app")?;
                required_array(&value, "image_ids")?;
                "copy_images_to_app"
            }
            other => return Err(format!("invalid action for app_assets: {other}")),
        },
        "script_files" => match action("action")? {
            "list" => "list_scripts",
            "write" => {
                required_arg(&value, "path")?;
                if validate && value.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err("missing required field: content".to_string());
                }
                "write_script"
            }
            "read" => {
                required_arg(&value, "path")?;
                "read_script"
            }
            "edit" => {
                required_arg(&value, "path")?;
                required_array(&value, "edits")?;
                "edit_script"
            }
            other => return Err(format!("invalid action for script_files: {other}")),
        },
        "video_transform" => match action("action")? {
            "edit" => {
                required_arg(&value, "video_id")?;
                "edit_video"
            }
            "extract_frame" => {
                required_arg(&value, "video_id")?;
                "extract_frame"
            }
            "stitch" => {
                required_array(&value, "video_ids")?;
                "stitch_videos"
            }
            other => return Err(format!("invalid action for video_transform: {other}")),
        },
        "video_references" => match action("action")? {
            "save" => {
                required_arg(&value, "name")?;
                required_arg(&value, "image_id")?;
                required_arg(&value, "description")?;
                "save_reference"
            }
            "list" => "list_references",
            "delete" => {
                required_arg(&value, "name")?;
                "delete_reference"
            }
            other => return Err(format!("invalid action for video_references: {other}")),
        },
        _ => unreachable!(),
    };
    Ok((mapped.to_string(), args.to_string()))
}

/// The tool-calling seam: everything an agent loop needs from the tool
/// layer — definitions for the request, read-only classification for
/// parallelizing independent calls, and execution. `ToolBox` is the local
/// implementation; a remote implementation (nexus host) speaks the
/// same methods over the wire, and the loops don't know the difference.
/// `supports_images`/`space_files_dir` are required because the tool loop
/// injects image references from tool results as vision content (the host
/// that runs the tools is the only one who knows the model's vision
/// support and where the space's files live).
pub trait ToolExecutor: Send + Sync + 'static {
    /// Tool definitions to attach to the request (see `ToolBox::defs`).
    fn defs(&self) -> Vec<ToolDef>;
    /// Compact tool definitions for small models; custom executors may keep full definitions.
    fn defs_compact(&self) -> Vec<ToolDef> {
        self.defs()
    }
    /// Whether a call is read-only (see `is_read_only_tool`); the default
    /// `effect` builds on it.
    fn is_read_only(&self, name: &str, args: &str) -> bool;
    /// What a call touches, declared before it runs — `run_scheduled` lets
    /// calls whose effects don't conflict run together. Without path
    /// knowledge, a read-only call touches nothing and any other call is a
    /// barrier.
    fn effect(&self, name: &str, args: &str) -> Effect {
        if self.is_read_only(name, args) {
            Effect::read_only()
        } else {
            Effect::barrier()
        }
    }
    /// Run one tool call, returning `(result, status-label)`. Boxed future
    /// so the trait stays dyn-compatible (the loops hold `Arc<dyn
    /// ToolExecutor>`).
    fn run<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (String, String)> + Send + 'a>>;
    /// Collect permission for a response before any of its calls run.
    fn preapprove<'a>(
        &'a self,
        calls: &'a [(String, String)],
    ) -> Pin<Box<dyn Future<Output = Vec<Gate>> + Send + 'a>> {
        Box::pin(std::future::ready(vec![Gate::NotNeeded; calls.len()]))
    }
    /// Run a call with its pre-approval outcome; executors without a gate use `run`.
    fn run_gated<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
        _gate: Gate,
    ) -> Pin<Box<dyn Future<Output = (String, String)> + Send + 'a>> {
        self.run(name, args)
    }
    /// Bind nested tasks to this turn; remote and restricted executors opt out.
    fn with_subagents(
        self: Arc<Self>,
        _context: Arc<crate::subagent::Context>,
    ) -> Option<Arc<dyn ToolExecutor>> {
        None
    }
    /// Whether the current model accepts image inputs.
    fn supports_images(&self) -> bool;
    /// The active space's files dir, for resolving image references in tool
    /// results.
    fn space_files_dir(&self) -> Option<std::path::PathBuf>;
}

impl ToolExecutor for ToolBox {
    fn with_subagents(
        self: Arc<Self>,
        context: Arc<crate::subagent::Context>,
    ) -> Option<Arc<dyn ToolExecutor>> {
        if self.workspace.is_none() || self.research_only || self.subagent_workspace.is_some() {
            return None;
        }
        let mut toolbox = self.copy(Arc::clone(&self));
        toolbox.subagents = Some(context);
        Some(Arc::new(toolbox))
    }

    fn defs(&self) -> Vec<ToolDef> {
        ToolBox::defs(self)
    }

    fn defs_compact(&self) -> Vec<ToolDef> {
        ToolBox::defs_compact(self)
    }

    fn is_read_only(&self, name: &str, args: &str) -> bool {
        if let Some(read_only) = self.connector_read_only(name, args) {
            return read_only;
        }
        is_read_only_tool(name, args)
    }

    /// Workspace calls declare the paths they resolve to; `shell run` is a
    /// barrier, as are the user bridges, which must stay in call order. A
    /// path that fails to resolve declares nothing: that call fails without
    /// touching anything.
    fn effect(&self, name: &str, args: &str) -> Effect {
        if matches!(name, "propose_command" | "request_workspace") {
            return Effect::barrier();
        }
        if let Some(read_only) = self.connector_read_only(name, args) {
            return if read_only {
                Effect {
                    network: true,
                    ..Effect::read_only()
                }
            } else {
                Effect::barrier()
            };
        }
        if name == "batch" {
            let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
            let mut union = Effect::read_only();
            for item in v
                .get("calls")
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
            {
                let tool = item
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default();
                let arguments = item
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let effect = self.effect(tool, &arguments.to_string());
                union.reads.extend(effect.reads);
                union.writes.extend(effect.writes);
                union.exec |= effect.exec;
                union.barrier |= effect.barrier;
                union.network |= effect.network;
            }
            return union;
        }
        let internal = public_call(name, args).map(|(internal, _)| internal);
        match internal.as_deref() {
            Ok("shell_run") => Effect {
                exec: true,
                ..Effect::read_only()
            },
            Ok("proc_start") => Effect {
                exec: true,
                ..Effect::barrier()
            },
            Ok("shell_log" | "proc_read" | "proc_list") => Effect::read_only(),
            Ok("proc_kill") => Effect::barrier(),
            Ok(
                action @ ("fs_list" | "fs_glob" | "fs_grep" | "fs_read" | "fs_edit" | "fs_write"
                | "fs_move" | "fs_delete"),
            ) => {
                let Some(workspace) = &self.workspace else {
                    return Effect::read_only();
                };
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let arg = |key: &str| v.get(key).and_then(serde_json::Value::as_str);
                let resolved = match action {
                    "fs_list" | "fs_glob" | "fs_grep" | "fs_read" => workspace
                        .resolve(arg("path").unwrap_or("."), Access::Read)
                        .map(|path| Effect {
                            reads: vec![path],
                            ..Effect::read_only()
                        }),
                    "fs_move" => workspace
                        .resolve(arg("path").unwrap_or_default(), Access::Write)
                        .and_then(|from| {
                            let to =
                                workspace.resolve(arg("to").unwrap_or_default(), Access::Write)?;
                            Ok(Effect {
                                writes: vec![from, to],
                                ..Effect::read_only()
                            })
                        }),
                    _ => workspace
                        .resolve(arg("path").unwrap_or_default(), Access::Write)
                        .map(|path| Effect {
                            writes: vec![path],
                            ..Effect::read_only()
                        }),
                };
                resolved.unwrap_or_default()
            }
            _ if is_read_only_tool(name, args) => Effect {
                network: matches!(name, "search" | "fetch_url" | "research_lookup"),
                ..Effect::read_only()
            },
            _ => Effect::barrier(),
        }
    }

    fn run<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (String, String)> + Send + 'a>> {
        Box::pin(ToolBox::run(self, name, args))
    }

    fn preapprove<'a>(
        &'a self,
        calls: &'a [(String, String)],
    ) -> Pin<Box<dyn Future<Output = Vec<Gate>> + Send + 'a>> {
        Box::pin(async move {
            if self.auto_decider.is_some() && self.require_approval && !self.research_only {
                return self.preapprove_auto(calls).await;
            }
            let mut gates = vec![Gate::NotNeeded; calls.len()];
            if !self.require_approval || self.research_only {
                return gates;
            }
            let mut requests = Vec::new();
            let mut indices = Vec::new();
            for (index, (name, args)) in calls.iter().enumerate() {
                let Ok((name, args)) = public_call(name, args) else {
                    continue;
                };
                if !(name.starts_with("fs_") || matches!(name.as_str(), "shell_run" | "proc_start"))
                    || is_read_only_tool(&name, &args)
                {
                    continue;
                }
                let Ok(request) = self.approval_request(&name, &args) else {
                    continue;
                };
                gates[index] = Gate::AskEach;
                if request
                    .summary
                    .trim_start_matches("background: ")
                    .starts_with("could not parse safely:")
                {
                    continue;
                }
                requests.push(request);
                indices.push(index);
            }
            if requests.len() >= 2
                && let Some(approver) = &self.approver
            {
                let gate = match approver.request_many(requests).await {
                    WaveDecision::AllowAll | WaveDecision::AllowAllForSession => Gate::Approved,
                    WaveDecision::ReviewEach => Gate::AskEach,
                    WaveDecision::DenyAll => Gate::Denied,
                };
                for index in indices {
                    gates[index] = gate;
                }
            }
            gates
        })
    }

    fn run_gated<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
        gate: Gate,
    ) -> Pin<Box<dyn Future<Output = (String, String)> + Send + 'a>> {
        Box::pin(self.run_with_gate(name, args, gate))
    }

    fn supports_images(&self) -> bool {
        self.supports_images
    }

    fn space_files_dir(&self) -> Option<std::path::PathBuf> {
        Some(self.space_files_dir.clone())
    }
}

/// Run calls in waves of non-conflicting effects (see `plan_waves`): the
/// calls in a wave run concurrently, at most `MAX_PARALLEL_CALLS` at a time,
/// and each wave waits for the one before. Results come back in call order,
/// whatever order the calls finish in.
pub(crate) async fn run_scheduled(
    exec: &dyn ToolExecutor,
    calls: &[(String, String)],
) -> Vec<(String, String)> {
    use futures_util::StreamExt as _;

    let gates = exec.preapprove(calls).await;
    if let [(name, args)] = calls {
        return vec![exec.run_gated(name, args, gates[0]).await];
    }
    let effects: Vec<Effect> = calls
        .iter()
        .map(|(name, args)| exec.effect(name, args))
        .collect();
    let mut results = vec![None; calls.len()];
    for wave in plan_waves(&effects) {
        let done: Vec<(String, String)> = futures_util::stream::iter(
            wave.iter()
                .copied()
                .map(|index| exec.run_gated(&calls[index].0, &calls[index].1, gates[index])),
        )
        .buffered(MAX_PARALLEL_CALLS)
        .collect()
        .await;
        for (index, result) in wave.into_iter().zip(done) {
            results[index] = Some(result);
        }
    }
    results
        .into_iter()
        .map(|result| result.expect("every call runs in one wave"))
        .collect()
}

impl ToolBox {
    /// All config knobs; kept flat because ~17 test call sites construct
    /// this with inline `None`/default args — a config struct would churn
    /// every one of them for no readability gain.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        skills_dir: PathBuf,
        searxng_url: Option<String>,
        langsearch_key: Option<String>,
        search_provider: String,
        blocked_domains: Vec<String>,
        db_path: Option<PathBuf>,
        files: Option<FilesCtx>,
        apps: Option<AppsCtx>,
    ) -> Self {
        Self {
            workspace: None,
            procs: Arc::default(),
            confine_shell_writes: None,
            auto_decider: None,
            sandbox_available: crate::workspace::shell::write_confinement_available(),
            decision_context: Arc::default(),
            decision_steps: Arc::default(),
            decision_scrubber: crate::trace::Scrubber::new(Vec::new()),
            prepared_decisions: Arc::default(),
            next_decision: Arc::new(AtomicU64::new(0)),
            subagents: None,
            subagent_workspace: None,
            subagent_environment: String::new(),
            decision_parent: None,
            require_approval: false,
            approver: None,
            interactive_ui: false,
            deck_link: Arc::default(),
            skill_dirs: vec![skills_dir.clone()],
            skills_dir,
            searxng_url,
            langsearch_key,
            search_provider,
            web_search: Arc::default(),
            search_backend: None,
            search_space_id: String::new(),
            research_only: false,
            blocked_domains,
            db_path,
            record_events: false,
            research_session_id: None,
            client: reqwest::Client::new(),
            discussions: Arc::new(DiscussionSearch::default()),
            image_search: ImageSearch::default(),
            remote_images: None,
            #[cfg(test)]
            deck_images: None,
            #[cfg(test)]
            deck_fonts: None,
            files,
            apps,
            cache_only: false,
            image_gen_backend: None,
            supports_images: false,
            space_files_dir: PathBuf::new(),
            memory: None,
            space_apps_dir: PathBuf::new(),
            space_scripts_dir: PathBuf::new(),
            session_id: String::new(),
            video_gen_backend: None,
            connectors: None,
        }
    }

    /// Whether a connector call is read-only, or `None` when `name` is not
    /// a connector tool (so the built-in classification applies).
    fn connector_read_only(&self, name: &str, args: &str) -> Option<bool> {
        let connectors = self.connectors.as_ref().filter(|c| !c.is_empty())?;
        if name == crate::connectors::exec::META_TOOL {
            let value: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
            return Some(
                match value.get("action").and_then(serde_json::Value::as_str) {
                    Some("search") => true,
                    Some("call") => value
                        .get("tool")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|tool| connectors.resolve(tool))
                        .is_some_and(|target| connectors.is_read_only(&target)),
                    _ => false,
                },
            );
        }
        let target = connectors.resolve(name)?;
        Some(connectors.is_read_only(&target))
    }

    fn scrubber(&self) -> &crate::trace::Scrubber {
        self.decision_parent
            .as_ref()
            .map_or(&self.decision_scrubber, |parent| parent.scrubber())
    }

    fn copy(&self, parent: Arc<Self>) -> Self {
        Self {
            workspace: self.workspace.clone(),
            procs: self.procs.clone(),
            confine_shell_writes: self.confine_shell_writes.clone(),
            auto_decider: self.auto_decider.clone(),
            sandbox_available: self.sandbox_available,
            decision_context: self.decision_context.clone(),
            decision_steps: self.decision_steps.clone(),
            decision_scrubber: crate::trace::Scrubber::new(Vec::new()),
            prepared_decisions: self.prepared_decisions.clone(),
            next_decision: self.next_decision.clone(),
            subagents: self.subagents.clone(),
            subagent_workspace: self.subagent_workspace,
            subagent_environment: self.subagent_environment.clone(),
            decision_parent: Some(parent),
            require_approval: self.require_approval,
            approver: self.approver.clone(),
            interactive_ui: self.interactive_ui,
            deck_link: self.deck_link.clone(),
            skills_dir: self.skills_dir.clone(),
            skill_dirs: self.skill_dirs.clone(),
            searxng_url: self.searxng_url.clone(),
            langsearch_key: self.langsearch_key.clone(),
            search_provider: self.search_provider.clone(),
            web_search: self.web_search.clone(),
            search_backend: self.search_backend.clone(),
            search_space_id: self.search_space_id.clone(),
            research_only: self.research_only,
            blocked_domains: self.blocked_domains.clone(),
            db_path: self.db_path.clone(),
            record_events: self.record_events,
            research_session_id: self.research_session_id.clone(),
            client: self.client.clone(),
            discussions: self.discussions.clone(),
            image_search: self.image_search.clone(),
            remote_images: self.remote_images.clone(),
            #[cfg(test)]
            deck_images: self.deck_images.clone(),
            #[cfg(test)]
            deck_fonts: self.deck_fonts.clone(),
            files: self.files.clone(),
            apps: self.apps.clone(),
            supports_images: self.supports_images,
            cache_only: self.cache_only,
            image_gen_backend: self.image_gen_backend.clone(),
            space_files_dir: self.space_files_dir.clone(),
            memory: self.memory.clone(),
            space_apps_dir: self.space_apps_dir.clone(),
            space_scripts_dir: self.space_scripts_dir.clone(),
            session_id: self.session_id.clone(),
            video_gen_backend: self.video_gen_backend.clone(),
            connectors: self.connectors.clone(),
        }
    }

    pub(crate) fn for_subagent(&self, workspace: Option<Arc<Workspace>>) -> Self {
        let mut toolbox = self.copy(self.decision_parent.clone().expect("bound toolbox"));
        toolbox.subagents = None;
        toolbox.subagent_workspace = Some(workspace.is_some());
        if let Some(workspace) = workspace {
            toolbox.confine_shell_writes = Some(workspace.root().to_path_buf());
            toolbox.workspace = Some(workspace);
            toolbox.procs = Arc::default();
        }
        toolbox.require_approval = toolbox.auto_decider.is_some();
        toolbox.approver = None;
        toolbox.prepared_decisions = Arc::default();
        toolbox.next_decision = Arc::new(AtomicU64::new(0));
        toolbox.decision_steps = Arc::default();
        toolbox
    }

    /// Replace the skill roots used for reads while retaining `skills_dir` as
    /// the app-managed install/create destination.
    #[must_use]
    pub fn with_skill_dirs(mut self, skill_dirs: Vec<PathBuf>) -> Self {
        self.skill_dirs = skill_dirs;
        self
    }

    /// A toolbox restricted to `search`/`fetch_url` — for deep-research
    /// searcher agents, which get no filesystem/app/script access.
    pub fn research(
        searxng_url: Option<String>,
        langsearch_key: Option<String>,
        search_provider: String,
        blocked_domains: Vec<String>,
        db_path: Option<PathBuf>,
    ) -> Self {
        let mut tb = Self::new(
            PathBuf::new(),
            searxng_url,
            langsearch_key,
            search_provider,
            blocked_domains,
            db_path,
            None,
            None,
        );
        tb.research_only = true;
        tb
    }

    /// Attach a research session id, enabling `research_lookup` for follow-up
    /// turns in that session's chat. Also merges any domains the user has
    /// discarded in this session into `blocked_domains`, so a later
    /// `search`/`fetch_url` call excludes them the same way the global
    /// setting does.
    #[must_use]
    pub fn with_research_session(mut self, session_id: String) -> Self {
        if let Some(db_path) = &self.db_path
            && let Ok(conn) = crate::db::open_conn(db_path)
            && let Ok(hosts) = crate::db::discarded_domains(&conn, &session_id)
        {
            self.blocked_domains.extend(hosts);
        }
        self.research_session_id = Some(session_id);
        self
    }

    /// Restrict `fetch_url` to serving from `web_cache` only — a cache miss
    /// returns `[not cached]` instead of fetching. Used for the Verifier's
    /// quote-checking pass.
    #[must_use]
    pub const fn cache_only(mut self) -> Self {
        self.cache_only = true;
        self
    }

    fn files_count(&self) -> u64 {
        let Some(ctx) = &self.files else { return 0 };
        crate::db::open_conn(&ctx.db_path)
            .ok()
            .and_then(|conn| crate::db::count_files(&conn, &ctx.space_id).ok())
            .unwrap_or(0)
    }

    fn citation_count(&self) -> u64 {
        let Some(ctx) = &self.files else { return 0 };
        crate::db::open_conn(&ctx.db_path)
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM citations WHERE space_id = ?1",
                    [&ctx.space_id],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
            })
            .unwrap_or(0)
            .max(0)
            .unsigned_abs()
    }

    /// Fetch through the cache: serve a fresh (<24h) cached copy unless
    /// `force_fresh`, else live-fetch and write through. Cache read/write
    /// failures degrade to a live fetch — a broken db must never block a
    /// tool call.
    async fn fetch_cached(&self, url: &str, force_fresh: bool) -> anyhow::Result<String> {
        let url_norm = normalize_url(url);
        if !force_fresh
            && let Some(db_path) = &self.db_path
            && let Ok(conn) = crate::db::open_attached(db_path)
            && let Ok(Some((_, text, fetched_at))) = crate::db::cache_get(&conn, &url_norm)
            && crate::db::is_fresh(&fetched_at, chrono::Utc::now())
        {
            return Ok(text);
        }
        if self.cache_only {
            return Ok("[not cached]".to_string());
        }
        let text = if is_youtube_url(url) {
            fetch_youtube_transcript(&self.client, url).await?
        } else {
            fetch_url_text(&self.client, url).await?
        };
        if text != REDDIT_RATE_LIMIT
            && let Some(db_path) = &self.db_path
            && let Ok(conn) = crate::db::open_attached(db_path)
        {
            let _ = crate::db::cache_put(&conn, &url_norm, url, None, &text);
        }
        Ok(text)
    }

    /// Tool definitions to attach to the request, or empty to send a plain
    /// request (keeps models that don't support tools working). `search` always works —
    /// it prefers configured API backends, then Exa and session-provider search
    /// when those are unavailable, so it needs no setup.
    pub fn defs(&self) -> Vec<ToolDef> {
        self.tool_definitions(false)
    }

    /// The same live tools and schemas, with concise descriptions for small models.
    pub fn defs_compact(&self) -> Vec<ToolDef> {
        self.tool_definitions(true)
    }

    // Long by design (tool-definition table).
    #[allow(clippy::too_many_lines)]
    fn tool_definitions(&self, compact: bool) -> Vec<ToolDef> {
        let def = |spec: &spec::ToolSpec, live: &[&str]| {
            if compact {
                spec.def_compact(live)
            } else {
                spec.def(live)
            }
        };
        if let Some(writable) = self.subagent_workspace {
            let actions: &[&str] = if writable {
                &[
                    "list", "glob", "grep", "read", "edit", "write", "move", "delete",
                ]
            } else {
                &["list", "glob", "grep", "read"]
            };
            return vec![
                def(&spec::FS, actions),
                def(
                    &spec::SHELL,
                    if writable { &["run", "log"] } else { &["log"] },
                ),
            ];
        }
        let mut defs = Vec::new();
        defs.push(tool_def(
            "batch",
            "Run several independent tool operations in ONE call — multiple searches, multiple file \
             searches/reads, or multiple app/script writes. Every result comes back in a single \
             round-trip, each labeled [n/N]. Prefer this over calling tools one by one whenever you \
             need several operations at once. Sub-calls use the same public tool names and \
             parameters as normal calls. Never nest batch inside batch; keep dependent steps (e.g. \
             write then edit the same file) as separate calls.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "calls": {
                        "type": "array",
                        "description": "up to 8 operations, run in order",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": { "type": "string", "description": "public tool name, e.g. search, canvas, scripts" },
                                "arguments": { "type": "object", "description": "that tool's parameters, same shape as a normal call" }
                            },
                            "required": ["tool"]
                        }
                    }
                },
                "required": ["calls"]
            }),
        ));
        let has_skills = !load_skills_from_dirs(&self.skill_dirs).is_empty();
        let mut skills_actions = vec!["create", "install"];
        if has_skills {
            skills_actions.insert(0, "load");
        }
        defs.push(def(&spec::SKILLS, &skills_actions));
        defs.push(def(
            &spec::SCRIPTS,
            &["list", "read", "write", "edit", "run", "python", "install"],
        ));
        defs.push(def(
            &spec::CANVAS,
            &[
                "create", "read", "view", "fonts", "edit", "comment", "resolve", "history",
                "export", "design",
            ],
        ));
        let mut search_modes = vec!["web", "academic", "discussion", "images"];
        if self.files.is_some() {
            search_modes.push("chats");
        }
        defs.push(def(&spec::SEARCH, &search_modes));
        defs.push(def(&spec::FETCH_URL, &["fetch_url"]));
        if self.workspace.is_some() {
            defs.push(def(
                &spec::FS,
                &[
                    "list", "glob", "grep", "read", "edit", "write", "move", "delete",
                ],
            ));
            defs.push(def(&spec::SHELL, &["run", "log"]));
            defs.push(def(&spec::PROC, &["start", "read", "list", "kill"]));
            defs.push(def(&spec::TASK, &["task"]));
        }
        if self.interactive_ui {
            // `!` lines run only in agent mode, which is when a workspace is open.
            if self.workspace.is_some() {
                defs.push(def(&spec::PROPOSE_COMMAND, &["propose_command"]));
            }
            if self.files.is_some() {
                defs.push(def(&spec::PANEL_APP, &["list", "read", "patch"]));
            }
        }
        if self.workspace.is_none() && self.approver.is_some() {
            defs.push(def(&spec::REQUEST_WORKSPACE, &["request_workspace"]));
        }
        let has_session_sources = self.research_session_id.is_some();
        let has_citations = self.citation_count() > 0;
        if has_session_sources || has_citations {
            let mut scopes = Vec::new();
            if has_session_sources {
                scopes.push("session_sources");
            }
            if has_citations {
                scopes.push("citations");
            }
            defs.push(def(&spec::RESEARCH_LOOKUP, &scopes));
        }
        if self.files_count() > 0 {
            defs.push(def(&spec::FILES, &["list", "search", "read", "pdf_page"]));
        }
        if self.memory.is_some() {
            defs.push(def(&spec::MEMORY, &["read", "add", "update", "delete"]));
        }
        if self.apps.is_some() {
            defs.push(tool_def(
                "app",
                "Build and manage locally served web apps. action=read returns stable hash-lines for safe editing and action=search greps non-ignored files; action=write replaces complete content, action=patch applies hash-line edits (include the line number when identical lines repeat), and action=diff previews a complete candidate without writing; action=list shows the space's apps (name, uuid, url) and the conversation/space images, and action=copy_file/copy_images bring user data into an app (images go to _images/, text files to the app KV store at <app_url>/_api/kv/_file:<name>); action=init scaffolds a React starter (framework astro = Astro + React islands, vite-react = Vite SPA; default astro) and action=build compiles the app — it installs missing deps from package.json, runs the framework's static build with --base=/<app-uuid>/ (so asset links resolve under the app's URL), and serves the result from dist/ (build errors come back here for you to fix; requires node/npm on this machine). App URLs are UUID-based: copy the UUID from a write or list result, never invent one. Persist app data in the per-app KV store, not localStorage: PUT/GET/DELETE <app_url>/_api/kv/<key> (body = raw text) and GET <app_url>/_api/kv lists keys; POST multipart/form-data to <app_url>/_api/upload stores a file and returns {name, url}.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": APP_ACTIONS },
                        "app": { "type": "string", "description": "app name or UUID" },
                        "path": { "type": "string", "description": "file path within the app" },
                        "pattern": { "type": "string", "description": "case-insensitive search text for search" },
                        "content": { "type": "string", "description": "complete content for write or diff" },
                        "edits": { "type": "array", "description": "hash-line edits from the latest read; hashes remain valid when earlier lines move, and include line when identical lines repeat", "items": { "type": "object", "properties": { "hash": { "type": "string", "description": "hash from the latest read" }, "line": { "type": "integer", "description": "optional 1-based line number to disambiguate repeated lines" }, "new": { "type": ["string", "null"] } }, "required": ["hash"] } },
                        "file_name": { "type": "string", "description": "imported file name for copy_file" },
                        "image_ids": { "type": "array", "items": { "type": "string" }, "description": "image IDs for copy_images" },
                        "framework": { "type": "string", "enum": ["astro", "vite-react"], "description": "starter template for init (default astro)" },
                        "offset": { "type": "integer" },
                        "limit": { "type": "integer", "description": "maximum 200 lines" },
                        "compact": { "type": "boolean", "description": "return locations only for search (default true)" }
                    },
                    "required": ["action"]
                }),
            ));
        }
        let mut media_actions = Vec::new();
        if self.image_gen_backend.is_some() {
            media_actions.push("generate_image");
        }
        if self.video_gen_backend.is_some() {
            media_actions.extend([
                "generate_video",
                "edit",
                "extract_frame",
                "stitch",
                "save_reference",
                "list_references",
                "delete_reference",
            ]);
        }
        if !media_actions.is_empty() {
            defs.push(def(&spec::MEDIA, &media_actions));
        }
        if self.research_only {
            defs.retain(|d| matches!(d.name.as_str(), "search" | "fetch_url"));
        }
        if compact {
            for tool in &mut defs {
                if matches!(tool.name.as_str(), "batch" | "app") {
                    if let Some(end) = tool.description.find(". ") {
                        tool.description.truncate(end + 2);
                    }
                    strip_param_descriptions(&mut tool.parameters);
                }
            }
        }
        // Connector tools come last, in a stable order, so adding or
        // removing one never reorders (and so never re-caches) the built-ins.
        // With no connectors this adds nothing: the list is byte-identical.
        if let Some(connectors) = &self.connectors
            && !self.research_only
        {
            defs.extend(connectors.exposure().defs.iter().cloned());
        }
        defs
    }

    /// Run a tool by name. Returns `(result text sent back to the model,
    /// status label shown in the UI while it runs)`.
    pub async fn run(&self, name: &str, args: &str) -> (String, String) {
        self.run_with_gate(name, args, Gate::NotNeeded).await
    }

    async fn run_with_gate(&self, name: &str, args: &str, gate: Gate) -> (String, String) {
        let started = Instant::now();
        let (result, status) = self.run_inner(name, args, gate).await;
        let latency = started.elapsed();
        let error_kind = classify_tool_result(&result, &status);
        let outcome = ToolOutcome {
            ok: error_kind.is_none(),
            error_kind,
            latency,
            result_chars: result.chars().count(),
        };
        self.record_tool_event(name, args, &outcome);
        if self.auto_decider.is_some() {
            let session = self
                .decision_context
                .lock()
                .expect("decision context")
                .0
                .clone();
            let mut recent = self.decision_steps.lock().expect("decision steps");
            if recent.0 != session {
                *recent = (session, Vec::new());
            }
            let summary = self.scrubber().scrub(&format!("{name} {args}"));
            recent.1.push(summary.chars().take(300).collect());
            if recent.1.len() > 5 {
                recent.1.remove(0);
            }
        }
        (result, status)
    }

    fn record_tool_event(&self, name: &str, args: &str, outcome: &ToolOutcome) {
        if !self.record_events {
            return;
        }
        let Some(db_path) = &self.db_path else {
            return;
        };
        let Ok(conn) = crate::db::open_attached(db_path) else {
            return;
        };
        let args: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
        let action = ["action", "mode", "scope"]
            .iter()
            .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))
            .unwrap_or_default();
        let now = chrono::Utc::now();
        let _ = conn.execute(
            "INSERT INTO cache.tool_events
                (at, session_id, tool, action, ok, error_kind, latency_ms, result_chars)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                now.to_rfc3339(),
                self.session_id,
                name,
                action,
                outcome.ok,
                outcome.error_kind,
                i64::try_from(outcome.latency.as_millis()).unwrap_or(i64::MAX),
                i64::try_from(outcome.result_chars).unwrap_or(i64::MAX),
            ],
        );
        let _ = conn.execute(
            "DELETE FROM cache.tool_events WHERE at < ?1",
            [(now - chrono::Duration::days(90)).to_rfc3339()],
        );
    }

    async fn preapprove_auto(&self, calls: &[(String, String)]) -> Vec<Gate> {
        let mut gates = vec![Gate::NotNeeded; calls.len()];
        let mut pending = Vec::new();
        let mut reqs = Vec::new();
        let (session_id, task) = self
            .decision_context
            .lock()
            .expect("decision context")
            .clone();
        for (index, (name, args)) in calls.iter().enumerate() {
            let Ok((name, args)) = public_call(name, args) else {
                continue;
            };
            if !(name.starts_with("fs_") || matches!(name.as_str(), "shell_run" | "proc_start"))
                || is_read_only_tool(&name, &args)
            {
                continue;
            }
            let Ok(mut request) = self.approval_request(&name, &args) else {
                continue;
            };
            let root = self
                .workspace
                .as_ref()
                .expect("validated workspace")
                .root()
                .to_path_buf();
            let effect = self.effect(&name, &args);
            let summary = if matches!(request.tool.as_str(), "shell" | "proc") {
                let args: serde_json::Value =
                    serde_json::from_str(&args).expect("validated arguments");
                format!("$ {}", args["cmd"].as_str().expect("validated command"))
            } else {
                request.summary.clone()
            };
            // Background starts use shell policy, including its hard denies and
            // configured command allow rules; the approval still names proc.
            let (policy_tool, policy_action) = if request.tool == "proc" {
                ("shell", "run")
            } else {
                (request.tool.as_str(), request.action.as_str())
            };
            let class =
                crate::decider::classify(policy_tool, policy_action, &root, &effect, &summary);
            request.allow_session = !matches!(class, Class::Destructive | Class::Outside);
            reqs.push(DecisionRequest {
                tool: policy_tool.to_string(),
                action: policy_action.to_string(),
                summary,
                task: self.scrubber().scrub(&task).chars().take(2_000).collect(),
                root,
                effect,
                class,
            });
            pending.push((index, request));
        }
        let (answers, cost) = self
            .auto_decider
            .as_ref()
            .expect("auto policy")
            .decide_with_cost(&reqs)
            .await;
        let conn = self
            .db_path
            .as_ref()
            .filter(|_| self.record_events)
            .and_then(|path| crate::db::open_attached(path).ok());
        let mut prepared: Vec<_> = pending
            .into_iter()
            .zip(reqs.iter().zip(answers))
            .map(|((index, request), (req, answer))| {
                (
                    index,
                    self.prepare_decision(req, request, answer, conn.as_ref(), &session_id),
                )
            })
            .collect();
        // Attribute the entire wave's reported cost to its first row, even if rules resolved it.
        if let Some(cost) = cost
            && let Some(conn) = &conn
            && let Some(id) = prepared.first().and_then(|(_, decision)| decision.row_id)
        {
            let _ = crate::db::set_decision_cost(conn, id, cost);
        }
        self.approve_auto_wave(&mut prepared).await;
        let mut tickets = self.prepared_decisions.lock().expect("prepared decisions");
        for (index, decision) in prepared {
            let ticket = self.next_decision.fetch_add(1, Ordering::Relaxed);
            tickets.insert(ticket, decision);
            gates[index] = Gate::Policy(ticket);
        }
        gates
    }

    fn prepare_decision(
        &self,
        req: &DecisionRequest,
        mut request: ApprovalRequest,
        mut answer: Verdicts,
        conn: Option<&rusqlite::Connection>,
        session_id: &str,
    ) -> PreparedDecision {
        let unparseable = request
            .summary
            .trim_start_matches("background: ")
            .starts_with("could not parse safely:");
        let hard_deny = answer.by == "rules" && matches!(answer.verdict, Verdict::Deny(_));
        if !hard_deny
            && (matches!(req.class, Class::Destructive | Class::Outside)
                || unparseable
                || (req.tool == "shell" && !self.sandbox_available))
        {
            answer.verdict = Verdict::Ask;
        }
        let row_id = conn.and_then(|conn| {
            crate::db::insert_decision(conn, session_id, req, &answer, self.scrubber()).ok()
        });
        if answer.verdict == Verdict::Ask {
            let mut evidence = Vec::new();
            if let Some(p) = answer.p_in_scope {
                evidence.push(format!("in_scope {p:.2}"));
            }
            if let Some(risk) = &answer.risk {
                evidence.push(format!("risk: {risk}"));
            }
            if !evidence.is_empty() {
                let evidence = evidence.join(" · ");
                request.summary = format!("{evidence}\n{}", request.summary);
                if let Some(detail) = &mut request.detail {
                    *detail = format!("{evidence}\n{detail}");
                }
            }
        }
        PreparedDecision {
            request,
            answer,
            row_id,
            user_answer: None,
        }
    }

    async fn approve_auto_wave(&self, prepared: &mut [(usize, PreparedDecision)]) {
        let ask: Vec<_> = prepared
            .iter()
            .enumerate()
            .filter_map(|(index, (_, decision))| {
                (decision.answer.verdict == Verdict::Ask
                    && !decision.request.summary.contains("could not parse safely:"))
                .then_some(index)
            })
            .collect();
        if ask.len() >= 2
            && let Some(approver) = &self.approver
        {
            let requests = ask
                .iter()
                .map(|&index| prepared[index].1.request.clone())
                .collect();
            let answer = match approver.request_many(requests).await {
                WaveDecision::AllowAll => Some(Decision::Allow),
                WaveDecision::AllowAllForSession => Some(Decision::AllowForSession),
                WaveDecision::DenyAll => Some(Decision::Deny),
                WaveDecision::ReviewEach => None,
            };
            if let Some(answer) = answer {
                for index in ask {
                    let decision = &mut prepared[index].1;
                    self.record_decision_answer(decision.row_id, answer);
                    decision.user_answer = Some(answer);
                }
            }
        }
    }

    fn record_decision_answer(&self, row_id: Option<i64>, answer: Decision) {
        let Some(id) = row_id else {
            return;
        };
        let answer = match answer {
            Decision::Allow => "allow",
            Decision::AllowForSession => "allow_session",
            Decision::AllowAlways => "allow_always",
            Decision::Deny => "deny",
            Decision::ReviewEach => return,
        };
        if let Some(path) = &self.db_path
            && let Ok(conn) = crate::db::open_attached(path)
        {
            let _ = crate::db::answer_decision(&conn, id, answer);
        }
    }

    fn approval_request(&self, name: &str, args: &str) -> Result<ApprovalRequest, String> {
        use crate::workspace::escalation::{Escalation, check_command};

        let effect = self.effect(name, args);
        let (tool, action) = name.split_once('_').expect("workspace tool action");
        let mut args: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("invalid tool arguments: {error}"))?;
        args.as_object_mut()
            .ok_or_else(|| "invalid tool arguments: expected an object".to_string())?
            .insert("action".to_string(), action.into());
        spec::find(tool)
            .expect("workspace tool spec")
            .validate(&args)?;
        let workspace = self.workspace.as_ref().ok_or_else(|| {
            format!("{tool} needs an agent workspace · switch with /agent <path>")
        })?;
        let (summary, detail) = if matches!(tool, "shell" | "proc") {
            let cmd = args["cmd"].as_str().expect("validated command");
            let mut summary = match check_command(cmd) {
                Ok(()) => String::new(),
                Err(Escalation::Blocked(word)) => {
                    return Err(format!(
                        "privilege escalation is blocked: {word} · ask the user to run it themselves (propose the command; they can run it with !)"
                    ));
                }
                Err(Escalation::Unparseable(reason)) => {
                    format!("could not parse safely: {reason}\n")
                }
            };
            let cwd = workspace
                .resolve(args["cwd"].as_str().unwrap_or("."), Access::Read)
                .map_err(|error| error.to_string())?;
            if !cwd.is_dir() {
                return Err("invalid path: shell cwd must be a directory".to_string());
            }
            let _ = write!(summary, "$ {cmd}");
            if tool == "proc" {
                summary = format!("background: {summary}");
            }
            if cwd != workspace.root() {
                let _ = write!(summary, "\nin {}", fs_relative(workspace, &cwd));
            }
            (summary, None)
        } else {
            let mut summary = fs_approval_summary(workspace, name, &args)?;
            let mut detail = None;
            if matches!(action, "edit" | "write" | "delete") {
                let path = workspace
                    .resolve(
                        args["path"].as_str().expect("validated path"),
                        Access::Write,
                    )
                    .map_err(|error| error.to_string())?;
                let bytes = if path.exists() {
                    std::fs::read(&path).map_err(|error| error.to_string())?
                } else {
                    Vec::new()
                };
                let current = String::from_utf8_lossy(&bytes);
                let candidate = if action == "delete" {
                    String::new()
                } else {
                    fs_replacement(name, &args, &current)?
                };
                let rel = fs_relative(workspace, &path);
                let (diff, added, deleted) = fs_diff(&rel, &current, &candidate, 40);
                summary = format!("{action} {rel} (+{added} −{deleted})\n{diff}");
                if diff.lines().count() >= 40 {
                    let (full, _, _) = fs_diff(&rel, &current, &candidate, APPROVAL_DETAIL_LINES);
                    detail = Some(format!("{action} {rel} (+{added} −{deleted})\n{full}"));
                }
            }
            (summary, detail)
        };
        let policy_tool = if tool == "proc" { "shell" } else { tool };
        let command = format!("$ {}", args["cmd"].as_str().unwrap_or_default());
        let class =
            crate::decider::classify(policy_tool, action, workspace.root(), &effect, &command);
        let class = match class {
            Class::WriteInRoot => "write in workspace",
            Class::Exec => "runs a command",
            Class::Destructive => "destructive",
            Class::Outside => "reaches outside",
        };
        Ok(ApprovalRequest {
            tool: tool.to_string(),
            action: action.to_string(),
            summary,
            class: Some(class.to_string()),
            detail,
            allow_session: true,
        })
    }

    // Long by design: the model tool dispatch (each arm is one tool).
    #[allow(clippy::too_many_lines)]
    async fn run_inner(&self, name: &str, args: &str, gate: Gate) -> (String, String) {
        if gate == Gate::Denied {
            return (
                "denied by the user · ask what they want instead of retrying".to_string(),
                "blocked".to_string(),
            );
        }
        if self.research_only
            && !matches!(
                name,
                "search"
                    | "fetch_url"
                    | "web_search"
                    | "academic_search"
                    | "discussion_search"
                    | "image_search"
            )
        {
            return (
                format!("tool '{name}' is not available in research mode"),
                "blocked".to_string(),
            );
        }
        let (dispatch_name, dispatch_args) = match public_call(name, args) {
            Ok(call) => call,
            Err(error) => return (cap_tool_result(error), "invalid arguments".to_string()),
        };
        let name = dispatch_name.as_str();
        let args = dispatch_args.as_str();
        if let Some(writable) = self.subagent_workspace {
            let allowed = matches!(
                name,
                "fs_list" | "fs_glob" | "fs_grep" | "fs_read" | "shell_log"
            ) || (writable
                && matches!(
                    name,
                    "fs_edit" | "fs_write" | "fs_move" | "fs_delete" | "shell_run"
                ));
            if !allowed {
                return (
                    format!("tool '{name}' is not available in this sub-agent"),
                    "blocked".into(),
                );
            }
        }
        if let Some(connectors) = &self.connectors
            && crate::connectors::exec::handles(connectors, name)
        {
            // Connector tools ask for approval themselves -- in chat mode
            // too -- so they never reach the workspace policy below.
            let (result, status) = crate::connectors::exec::run(
                crate::connectors::exec::Exec {
                    registry: connectors,
                    approver: self.approver.as_ref(),
                    files_dir: &self.space_files_dir,
                },
                name,
                args,
            )
            .await;
            return (cap_tool_result(result), status);
        }
        let mut allow_unparseable = false;
        let mut auto_allowed = false;
        let mut gate = gate;
        if self.require_approval
            && self.auto_decider.is_some()
            && (name.starts_with("fs_") || matches!(name, "shell_run" | "proc_start"))
            && !is_read_only_tool(name, args)
        {
            if !matches!(gate, Gate::Policy(_)) {
                gate = self
                    .preapprove_auto(&[(name.to_string(), args.to_string())])
                    .await[0];
            }
            if let Gate::Policy(ticket) = gate {
                let prepared = self
                    .prepared_decisions
                    .lock()
                    .expect("prepared decisions")
                    .remove(&ticket)
                    .expect("policy ticket belongs to this call");
                if let Verdict::Deny(reason) = &prepared.answer.verdict {
                    return (
                        format!("denied by policy: {reason} · ask the user"),
                        "blocked".to_string(),
                    );
                }
                auto_allowed = prepared.answer.verdict == Verdict::Allow;
                let decision = if auto_allowed {
                    Decision::Allow
                } else if let Some(answer) = prepared.user_answer {
                    answer
                } else if let Some(approver) = &self.approver {
                    let answer = approver.request(prepared.request.clone()).await;
                    self.record_decision_answer(prepared.row_id, answer);
                    answer
                } else {
                    return (
                        if self.subagent_workspace.is_some() {
                            "needs approval · sub-agents cannot prompt; report what you would have done".to_string()
                        } else {
                            "needs approval · not available here (headless); describe the change and let the user apply it".to_string()
                        },
                        "blocked".to_string(),
                    );
                };
                if !matches!(
                    decision,
                    Decision::Allow | Decision::AllowForSession | Decision::AllowAlways
                ) {
                    return (
                        "denied by the user · ask what they want instead of retrying".to_string(),
                        "blocked".to_string(),
                    );
                }
                allow_unparseable = prepared.request.summary.contains("could not parse safely:");
                gate = Gate::Approved;
            }
        }
        if self.require_approval
            && (name.starts_with("fs_") || matches!(name, "shell_run" | "proc_start"))
            && !is_read_only_tool(name, args)
        {
            let request = match self.approval_request(name, args) {
                Ok(request) => request,
                Err(error) => return (error, "blocked".to_string()),
            };
            let unparseable = request
                .summary
                .trim_start_matches("background: ")
                .starts_with("could not parse safely:");
            if gate != Gate::Approved || (unparseable && !allow_unparseable) {
                let Some(approver) = &self.approver else {
                    return (
                        "needs approval · not available here (headless); describe the change and let the user apply it".to_string(),
                        "blocked".to_string(),
                    );
                };
                // Only an explicit allow runs the call; anything else
                // (deny, or a review answer meant for a combined prompt) is
                // a denial.
                if !matches!(
                    approver.request(request).await,
                    Decision::Allow | Decision::AllowForSession | Decision::AllowAlways
                ) {
                    return (
                        "denied by the user · ask what they want instead of retrying".to_string(),
                        "blocked".to_string(),
                    );
                }
                allow_unparseable = unparseable;
            }
        }
        let (result, status) = match name {
            "deck_create" | "deck_read" | "deck_view" | "deck_edit" | "deck_comment"
            | "deck_resolve" | "deck_fonts" | "deck_export" | "deck_history" | "canvas_design" => {
                let value = serde_json::from_str(args).expect("validated canvas arguments");
                match self.canvas_action(name, &value).await {
                    Ok(result) => (result, "done".into()),
                    Err(error) => {
                        let kind = if Self::is_doc(&value) {
                            "doc"
                        } else if name == "canvas_design" {
                            "design"
                        } else {
                            "deck"
                        };
                        let message = format!("{kind} failed: {error:#}");
                        // A first create usually fails before any result could
                        // carry the field reference; send it with the error.
                        let schema = if Self::is_doc(&value) {
                            self.doc_schema_if_fits(message.chars().count())
                        } else {
                            self.deck_schema_if_fits(message.chars().count())
                        };
                        (message + schema, "invalid arguments".into())
                    }
                }
            }
            "task" => {
                let Some(context) = &self.subagents else {
                    return ("task needs an agent turn".into(), "blocked".into());
                };
                let value: serde_json::Value = serde_json::from_str(args).expect("validated task");
                if value["tools"] == "workspace" && gate != Gate::Approved {
                    let Some(approver) = &self.approver else {
                        return (
                            "needs approval · workspace sub-agent cannot run unattended".into(),
                            "blocked".into(),
                        );
                    };
                    let request = ApprovalRequest {
                        tool: "task".into(),
                        class: None,
                        detail: None,
                        action: "workspace".into(),
                        summary: format!(
                            "sub-agent (workspace): {}",
                            value["prompt"]
                                .as_str()
                                .expect("validated prompt")
                                .lines()
                                .next()
                                .unwrap_or_default()
                        ),
                        allow_session: false,
                    };
                    if !matches!(
                        approver.request(request).await,
                        Decision::Allow | Decision::AllowForSession | Decision::AllowAlways
                    ) {
                        return (
                            "denied by the user · workspace sub-agent not started".into(),
                            "blocked".into(),
                        );
                    }
                }
                return match crate::subagent::run(context, self, &value).await {
                    Ok(report) => (report, "Sub-agent finished".into()),
                    Err(error) => (format!("sub-agent failed: {error}"), "blocked".into()),
                };
            }
            "panel_app_list" | "panel_app_read" | "panel_app_patch" => {
                let Some(files) = self.files.as_ref().filter(|_| self.interactive_ui) else {
                    return ("panel_app needs the terminal UI".into(), "blocked".into());
                };
                let args = serde_json::from_str(args).unwrap_or_default();
                let action = name.strip_prefix("panel_app_").expect("panel app action");
                let result = crate::panels::tool(&files.db_path, &files.space_id, action, &args)
                    .unwrap_or_else(|error| format!("panel_app error: {error}"));
                (result, format!("Pinned panel {action}…"))
            }
            "propose_command" => {
                if !self.interactive_ui {
                    return (
                        "propose_command needs the terminal UI · tell the user the exact command and why instead".to_string(),
                        "blocked".to_string(),
                    );
                }
                if self.workspace.is_none() {
                    return (
                        "propose_command runs in agent mode · tell the user the exact command and why instead".to_string(),
                        "blocked".to_string(),
                    );
                }
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let cmd = v["cmd"].as_str().unwrap_or_default().trim();
                // The command lands in the user's composer: one visible line,
                // so nothing can hide below or behind what they read.
                if cmd.is_empty() || cmd.chars().any(char::is_control) {
                    return (
                        "invalid tool arguments: propose_command takes one non-empty line without control characters · join steps with && or propose them one at a time".to_string(),
                        "invalid arguments".to_string(),
                    );
                }
                (
                    format!(
                        "proposed to the user: !{cmd} · wait for them to run it; its output will appear in the conversation"
                    ),
                    "Proposing a command…".to_string(),
                )
            }
            "request_workspace" => {
                let Some(approver) = self.approver.as_ref().filter(|_| self.workspace.is_none())
                else {
                    return (
                        "request_workspace is only available in chat mode in the terminal UI · ask the user to run /agent <path>".to_string(),
                        "blocked".to_string(),
                    );
                };
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let root = v["root"].as_str().unwrap_or_default().trim();
                let reason = v["reason"].as_str().unwrap_or_default().trim();
                let request = ApprovalRequest {
                    tool: "request_workspace".to_string(),
                    class: None,
                    detail: None,
                    action: "agent".to_string(),
                    summary: format!("switch this session to agent mode in {root}\n{reason}"),
                    allow_session: true,
                };
                if !matches!(
                    approver.request(request).await,
                    Decision::Allow | Decision::AllowForSession | Decision::AllowAlways
                ) {
                    return (
                        "denied by the user · ask what they want instead of retrying".to_string(),
                        "blocked".to_string(),
                    );
                }
                (
                    format!(
                        "approved: agent mode in {root} · workspace tools are available from your next turn"
                    ),
                    "Requesting a workspace…".to_string(),
                )
            }
            "proc_start" | "proc_read" | "proc_list" | "proc_kill" => match &self.workspace {
                None => (
                    "proc needs an agent workspace · switch with /agent <path>".to_string(),
                    "blocked".to_string(),
                ),
                Some(workspace) => (
                    self.procs
                        .action(
                            crate::workspace::procs::Context {
                                workspace,
                                db_path: self.db_path.as_deref(),
                                confinement: self
                                    .confine_shell_writes
                                    .as_deref()
                                    .or_else(|| auto_allowed.then(|| workspace.root())),
                                session_id: &self.session_id,
                                allow_unparseable,
                            },
                            name,
                            args,
                        )
                        .await,
                    format!("Running {name}…"),
                ),
            },
            "shell_run" | "shell_log" => match &self.workspace {
                None => (
                    "shell needs an agent workspace · switch with /agent <path>".to_string(),
                    "blocked".to_string(),
                ),
                Some(workspace) => (
                    crate::workspace::shell::action(
                        workspace,
                        self.db_path.as_deref(),
                        self.confine_shell_writes
                            .as_deref()
                            .or_else(|| auto_allowed.then(|| workspace.root())),
                        name,
                        args,
                        allow_unparseable,
                    )
                    .await,
                    format!("Running {name}…"),
                ),
            },
            "fs_list" | "fs_glob" | "fs_grep" | "fs_read" | "fs_edit" | "fs_write" | "fs_move"
            | "fs_delete" => self.run_fs(name, args).await,
            "skill" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let skill_name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let file = v
                    .get("file")
                    .and_then(|f| f.as_str())
                    .filter(|f| !f.is_empty())
                    .unwrap_or("SKILL.md");
                let status = format!("Reading {skill_name}/{file}…");
                let result = match resolve_skill_file(&self.skill_dirs, &skill_name, file) {
                    Err(e) => e,
                    Ok(path) if !path.is_file() => format!("no such file: {skill_name}/{file}"),
                    Ok(path) => {
                        let text = match std::fs::read_to_string(&path) {
                            Ok(t) => t,
                            Err(e) => format!("error reading {skill_name}/{file}: {e}"),
                        };
                        if file == "SKILL.md" {
                            skill_body(&text).to_string()
                        } else {
                            text
                        }
                    }
                };
                (result, status)
            }
            "install_skill" => {
                let source = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("source").and_then(|s| s.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let status = format!("Installing skill {source}…");
                let result = match crate::skills::parse_gh_shorthand(&source) {
                    None => format!("invalid source {source:?} — expected owner/repo/path"),
                    Some((owner, repo, path)) => {
                        match crate::skills::install_from_github(
                            &self.client,
                            &owner,
                            &repo,
                            &path,
                            &self.skills_dir,
                        )
                        .await
                        {
                            Ok(name) => {
                                format!("installed skill '{name}' — load it with the skill tool")
                            }
                            Err(e) => format!("install failed: {e}"),
                        }
                    }
                };
                (result, status)
            }
            "create_skill" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let description = v
                    .get("description")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let body = v
                    .get("body")
                    .and_then(|x| x.as_str())
                    .filter(|b| !b.is_empty());
                let overwrite = v
                    .get("overwrite")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let status = format!("Creating skill {name}…");
                let result = if !crate::skills::valid_skill_name(&name) {
                    "name must be 1-64 lowercase letters, digits, and single hyphens".to_string()
                } else if description.is_empty()
                    || description.chars().count() > 1_024
                    || description.contains(['\n', '\r'])
                {
                    "description must be 1-1024 characters on one line".to_string()
                } else {
                    let dir = self.skills_dir.join(&name);
                    let existed = dir.exists();
                    if existed && !overwrite {
                        format!("skill '{name}' already exists — set overwrite=true to replace")
                    } else if let Err(e) = std::fs::create_dir_all(&dir) {
                        format!("cannot create skill dir: {e}")
                    } else {
                        let body = body.unwrap_or("Write the skill instructions here. The model sees this text when it loads the skill.");
                        let md = format!(
                            "---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"
                        );
                        match std::fs::write(dir.join("SKILL.md"), &md) {
                            Ok(()) => {
                                let verb = if existed { "updated" } else { "created" };
                                format!("{verb} skill '{name}' — load it with the skill tool")
                            }
                            Err(e) => format!("cannot write SKILL.md: {e}"),
                        }
                    }
                };
                (result, status)
            }
            "run_script" | "install_packages" | "run_python" | "list_scripts" | "write_script"
            | "read_script" | "edit_script" => self.run_scripts(name, args).await,
            "web_search" | "fetch_url" | "image_search" | "academic_search"
            | "discussion_search" => self.run_web(name, args).await,
            "search_sources" => {
                let query = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let status = "Searching session sources…".to_string();
                let result = match (&self.research_session_id, &self.db_path) {
                    (Some(session_id), Some(db_path)) => match crate::db::open_attached(db_path) {
                        Err(e) => format!("source search failed: {e}"),
                        Ok(conn) => {
                            match crate::db::search_session_sources(&conn, session_id, &query) {
                                Ok(hits) if hits.is_empty() => {
                                    "no matches in this session's sources".to_string()
                                }
                                Ok(hits) => hits
                                    .iter()
                                    .map(|(url, text)| {
                                        let cut: String = text.chars().take(500).collect();
                                        format!("{url}:\n{cut}")
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n"),
                                Err(e) => format!("source search failed: {e}"),
                            }
                        }
                    },
                    _ => "no session source bundle available".to_string(),
                };
                (result, status)
            }
            "list_citations" => {
                let query = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string));
                let status = "Listing citations…".to_string();
                let result = match &self.files {
                    None => "no space context available".to_string(),
                    Some(ctx) => match rusqlite::Connection::open(&ctx.db_path) {
                        Err(e) => format!("citation lookup failed: {e}"),
                        Ok(conn) => match crate::db::search_citations(
                            &conn,
                            &ctx.space_id,
                            query.as_deref(),
                        ) {
                            Ok(rows) if rows.is_empty() => "no citations recorded yet".to_string(),
                            Ok(rows) => rows
                                .iter()
                                .map(|(report, url, title)| {
                                    if title.is_empty() {
                                        format!("{report}: {url}")
                                    } else {
                                        format!("{report}: {url} ({title})")
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Err(e) => format!("citation lookup failed: {e}"),
                        },
                    },
                };
                (result, status)
            }
            "batch" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let calls: Vec<(String, serde_json::Value)> = v
                    .get("calls")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                let tool = item
                                    .get("tool")
                                    .and_then(|t| t.as_str())
                                    .filter(|t| !t.is_empty())?
                                    .to_string();
                                let arguments = item
                                    .get("arguments")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}));
                                Some((tool, arguments))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if calls.is_empty() {
                    return (
                        "batch requires a non-empty calls array".to_string(),
                        "Running batch…".to_string(),
                    );
                }
                if calls.len() > MAX_BATCH_CALLS {
                    return (
                        format!(
                            "batch accepts at most {MAX_BATCH_CALLS} calls, got {}",
                            calls.len()
                        ),
                        "Running batch…".to_string(),
                    );
                }
                if calls.iter().any(|(tool, _)| tool == "batch") {
                    return (
                        "nested batch calls are not allowed — flatten them into one list"
                            .to_string(),
                        "Running batch…".to_string(),
                    );
                }
                if calls.iter().any(|(tool, _)| {
                    matches!(tool.as_str(), "propose_command" | "request_workspace")
                }) {
                    return (
                        "propose_command and request_workspace cannot run inside batch — call them directly"
                            .to_string(),
                        "Running batch…".to_string(),
                    );
                }
                let status = format!("Running {} batched operations…", calls.len());
                // Serialize each sub-call once. Sub-calls run in waves of
                // non-conflicting effects (`run_scheduled`): reads and writes
                // to different files overlap, while writes to the same file
                // or a program run keep their order. Results are zipped back
                // into call order.
                let items: Vec<(String, String)> = calls
                    .iter()
                    .map(|(tool, arguments)| {
                        (
                            tool.clone(),
                            serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
                        )
                    })
                    .collect();
                // Sub-calls go through `ToolExecutor::run`, whose Box::pin
                // keeps the recursive `run` future finitely sized.
                let results: Vec<(String, String)> = run_scheduled(self, &items).await;
                let mut out = String::new();
                for (i, ((tool, _), (_, arguments))) in items.iter().zip(calls.iter()).enumerate() {
                    if i > 0 {
                        out.push('\n');
                    }
                    let label: String = batch_call_label(tool, arguments)
                        .chars()
                        .take(120)
                        .collect();
                    let _ = write!(
                        out,
                        "[batch-result: {} bytes]\n[{}/{}] {}\n{}\n[/batch-result]",
                        results[i].0.len(),
                        i + 1,
                        items.len(),
                        label,
                        results[i].0,
                    );
                }
                (out, status)
            }
            "search_chats" => {
                let value = serde_json::from_str(args).unwrap_or_default();
                let result = match &self.files {
                    Some(ctx) if !self.research_only => {
                        chats::search(ctx, &self.session_id, &value).await
                    }
                    _ => Err(anyhow::anyhow!("chat history unavailable")),
                };
                (
                    result.unwrap_or_else(|error| format!("chat lookup failed: {error}")),
                    "Reading past chats".to_string(),
                )
            }
            "memory_read" | "memory_add" | "memory_update" | "memory_delete" => {
                let status = if name == "memory_read" {
                    "Reading memory…"
                } else {
                    "Updating memory…"
                };
                let result = match &self.memory {
                    Some(ctx) => memory_tool::run(ctx, name, args),
                    None => "memory is off in this chat".to_string(),
                };
                (result, status.to_string())
            }
            "list_files" | "search_files" | "read_file" | "read_pdf_page" => {
                self.run_files(name, args).await
            }
            "copy_file_to_app" | "list_images" | "copy_images_to_app" | "init_app"
            | "build_app" | "write_file" | "edit_file" | "diff_app" | "grep_app"
            | "read_app_file" => self.run_apps(name, args).await,
            "generate_image" | "generate_video" | "edit_video" | "extract_frame"
            | "stitch_videos" | "save_reference" | "list_references" | "delete_reference" => {
                self.run_media(name, args).await
            }
            other => (
                format!("unknown tool: {other}"),
                "Running tool…".to_string(),
            ),
        };
        let result = if name == "batch" {
            // The batch arm already bounded each sub-result; apply the larger
            // combined cap here so several packed results survive intact.
            cap_result(result, MAX_BATCH_RESULT_CHARS)
        } else {
            cap_tool_result(result)
        };
        (result, status)
    }
}

/// Tools safe to run concurrently in one round-trip: they read state or hit
/// the network but never mutate files/db in ways that could race. Used to
/// parallelize both model-issued parallel tool calls and `batch` sub-calls.
/// Consolidated names (`search`, `files`, `skills`, `scripts`, `app`, `media`,
/// `research_lookup`) count as read-only only for their read-only actions —
/// the `action` argument decides. Legacy names (`skill`, `web_search`, …)
/// are still classified; mutating consolidations (`batch`) are never read-only.
pub fn is_read_only_tool(name: &str, args: &str) -> bool {
    if let Some(read_only) = spec::read_only(name, args) {
        return read_only;
    }
    let action_is = |actions: &[&str]| {
        serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| v.get("action").and_then(|a| a.as_str()).map(str::to_string))
            .is_some_and(|a| actions.contains(&a.as_str()))
    };
    match name {
        "app" => action_is(&["read", "search", "diff", "list"]),
        _ => matches!(
            name,
            "app_inspect" | "read_app_file" | "grep_app" | "diff_app" | "list_images"
        ),
    }
}

/// Compact label for one `batch` sub-call, shown above its result so the
/// model can tell which output belongs to which operation.
pub(crate) fn batch_call_label(name: &str, v: &serde_json::Value) -> String {
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let quoted = |k: &str| format!("{:?}", s(k));
    match name {
        "skills" => format!("skills/{} {}", s("action"), s("name")),
        "scripts" => {
            let target = if s("action") == "install" {
                "packages".to_string()
            } else {
                s("path")
            };
            format!("scripts/{} {}", s("action"), target)
        }
        "app" => {
            let path = s("path");
            if path.is_empty() {
                format!("app/{} {}", s("action"), s("app"))
            } else {
                format!("app/{} {}/{}", s("action"), s("app"), path)
            }
        }
        "media" => {
            let target = [s("video_id"), s("name"), s("image_id")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_else(|| s("prompt").chars().take(40).collect());
            format!("media/{} {}", s("action"), target)
        }
        "search" => format!("search {} {}", s("mode"), quoted("query")),
        "fetch_url" => format!("fetch_url {}", s("url")),
        "files" => {
            let target = if s("name").is_empty() {
                quoted("query")
            } else {
                s("name")
            };
            format!("files/{} {target}", s("action"))
        }
        "app_inspect" => format!("app_inspect/{} {}/{}", s("action"), s("app"), s("path")),
        "app_modify" => format!("app_modify/{} {}/{}", s("action"), s("app"), s("path")),
        "app_assets" => format!("app_assets/{} {}", s("action"), s("app")),
        "script_files" => format!("script_files/{} {}", s("action"), s("path")),
        "research_lookup" => format!("research_lookup/{} {}", s("scope"), quoted("query")),
        "web_search" => format!("web_search {:?}", s("query")),
        "academic_search" => format!("academic_search {:?}", s("query")),
        "discussion_search" => format!("discussion_search {:?}", s("query")),
        "search_files" => format!("search_files {:?}", s("query")),
        "search_sources" => format!("search_sources {:?}", s("query")),
        "list_citations" => "list_citations".to_string(),
        "read_file" => format!("read_file {}", s("name")),
        "read_pdf_page" => format!("read_pdf_page {} page {}", s("name"), s("page")),
        "read_app_file" => format!("read_app_file {}/{}", s("app"), s("path")),
        "grep_app" => format!("grep_app {} {:?}", s("app"), s("pattern")),
        "read_script" => format!("read_script {}", s("path")),
        "skill" => format!("skill {}", s("name")),
        "skill_admin" => format!("skill_admin {}", s("action")),
        "run_python" => format!("run_python {}", s("name")),
        "run_script" => format!("run_script {}", s("path")),
        "install_packages" => {
            let target = [s("skill"), s("app")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_default();
            format!("install_packages {target}")
        }
        "generate_image" | "generate_video" => {
            let prompt: String = s("prompt").chars().take(60).collect();
            format!("{name} {prompt:?}")
        }
        _ => name.to_string(),
    }
}

/// Marker prefix of `tool_result_unchanged_note` — a tool result omitted
/// because it is byte-identical to an earlier call with the same tool and
/// arguments. Both the live tool loop and `build_history` emit this exact
/// text for duplicates, which lets the loop recognize replayed notes when
/// seeding its dedup map. A real tool result beginning with this string
/// would at worst miss one dedup (and cause a single prompt-cache break),
/// never corrupt model-visible content.
pub const TOOL_RESULT_OMITTED_PREFIX: &str = "[result omitted: ";

/// One-line replacement for a tool result that duplicates an earlier call
/// with the same tool and arguments. The first full copy stays in the
/// conversation, so nothing is lost; the note keeps the duplicate from
/// re-entering the context on every subsequent request.
pub fn tool_result_unchanged_note(name: &str, args: &str) -> String {
    let label = serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .map_or_else(|| name.to_string(), |v| batch_call_label(name, &v));
    format!(
        "{TOOL_RESULT_OMITTED_PREFIX}{label} returned exactly the same result as an earlier \
         call — content unchanged; the earlier result is above]"
    )
}

/// Marker for an old tool result cleared by the observation-masking experiment.
pub const TOOL_RESULT_MASKED_PREFIX: &str = "[result cleared to save context — call ";

/// One-line replacement for an old result; its call and arguments stay in history.
pub fn tool_result_masked_note(name: &str, _args: &str) -> String {
    format!("{TOOL_RESULT_MASKED_PREFIX}{name} again if you need it]")
}

/// Bound every tool result before it is sent back to the model and persisted.
/// This is especially important for fetched web pages, which can otherwise
/// consume the conversation context one tool call at a time.
pub(crate) fn cap_tool_result(result: String) -> String {
    cap_result(result, MAX_TOOL_RESULT_CHARS)
}

fn cap_result(result: String, max_chars: usize) -> String {
    const SUFFIX: &str = "\n... (tool result truncated)";
    if result.chars().count() <= max_chars {
        return result;
    }
    let keep = max_chars.saturating_sub(SUFFIX.chars().count());
    let mut capped: String = result.chars().take(keep).collect();
    capped.push_str(SUFFIX);
    capped
}

/// Cap context-visible text while retaining roughly 70% head and 30% tail.
/// Counts Unicode characters, including the omission marker, not UTF-8 bytes.
#[must_use]
pub fn cap_head_tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    // Reserve enough marker space even if the omitted count has fewer digits.
    let marker = format!("\n… [{count} characters omitted] …\n");
    let keep = max_chars.saturating_sub(marker.chars().count());
    let head = keep * 7 / 10;
    let tail = keep - head;
    let omitted = count - keep;
    let marker = format!("\n… [{omitted} characters omitted] …\n");
    let mut result: String = text.chars().take(head).collect();
    result.extend(marker.chars().take(max_chars - keep));
    result.extend(text.chars().skip(count - tail));
    result
}

/// Remove schema annotations without removing parameters named `description`.
fn strip_param_descriptions(schema: &mut serde_json::Value) {
    if let Some(object) = schema.as_object_mut() {
        object.remove("description");
    }
    if let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        for property in properties.values_mut() {
            strip_param_descriptions(property);
        }
    }
    if let Some(items) = schema.get_mut("items") {
        strip_param_descriptions(items);
    }
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && std::path::Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

pub(crate) fn confined_join(
    root: &std::path::Path,
    rel: &str,
) -> Result<std::path::PathBuf, String> {
    if !valid_relative_path(rel) {
        return Err(format!("invalid path: {rel:?}"));
    }
    let root_ancestor = root
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor).is_ok())
        .unwrap_or(root);
    let canonical_root = std::fs::canonicalize(root_ancestor)
        .map_err(|e| format!("cannot resolve scripts directory: {e}"))?;
    let path = root.join(rel);
    // This includes the file itself when it exists, or its nearest existing parent otherwise.
    let ancestor = path
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor).is_ok())
        .unwrap_or(&path);
    let canonical_path = std::fs::canonicalize(ancestor)
        .map_err(|_| format!("path escapes the scripts directory: {rel:?}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(format!("path escapes the scripts directory: {rel:?}"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
