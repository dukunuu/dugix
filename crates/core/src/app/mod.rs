#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::Utc;
use tokio::sync::{mpsc, oneshot};

use crate::tools::{ApprovalRequest, Approver, Decision, WaveDecision};

use crate::config;
use crate::db::{DEFAULT_SPACE, Db, Message, Session, Space as SpaceRow};
pub use crate::provider::model_needs_cache_breakpoints;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{BackendTag, Model, StreamEvent, Usage};
use crate::space::Space;

mod apps;
mod backends;
mod chat;
mod chats;
mod commands;
mod compaction;
mod embeddings;
pub mod export;
mod files;
pub mod headless;
mod images;
#[cfg(test)]
mod improve_tests;
#[cfg(test)]
mod masking_tests;
mod memory;
mod models;
mod panels;
#[cfg(test)]
mod prompt_snapshots;
#[cfg(test)]
mod reply_style_tests;
#[cfg(test)]
mod request_usage_tests;
mod research;
mod scripts;
mod sessions;
mod settings;
pub use settings::{
    COLUMN_WIDTH_MAX, COLUMN_WIDTH_MIN, POPUP_IDS, POPUP_SIZE_MAX, POPUP_SIZE_MIN, popup_size,
    with_popup_size,
};
mod connectors;
mod skills_popup;
mod snapshot;
mod spaces;
mod state;
pub use state::{
    ApprovalState, CompactionState, ContextMeter, IndexState, ListCaches, LocalRuntimeState,
    ResearchState, SearchConfig, SwarmState, UiRepairState, UserCommandState,
};
mod subagents;
mod swarm;
#[cfg(test)]
mod tests;
mod tool_view;
mod transcribe;
pub use tool_view::{BatchEntry, Source, ToolOutcome, ToolView, tool_call_view};
mod research_jobs;
pub mod usage;
pub use backends::{Backends, composite_id};
pub use chat::{
    MERMAID_PREVIEW_EXAMPLES, code_blocks, human_size, pick_greeting, user_command_outcome,
    user_command_outcome_label,
};
pub use commands::{
    AppCommand, COMMANDS, Command, Match, MemoryAction, command_score, fuzzy_score,
};
pub use files::ConvertUpdate;
pub use research::{PlanQuestion, ResearchUpdate};
pub use sessions::session_score;
pub use snapshot::{
    CoreSnapshot, MessageSnapshot, ModelSnapshot, SessionSnapshot, SettingsSnapshot, TaskSnapshot,
};
pub use subagents::{SubagentRow, SubagentRun, SubagentStatus};
pub use swarm::{SwarmUpdate, parse_persona_editor};

#[cfg(test)]
use chat::split_inline_reasoning;
use sessions::parse_topic;

#[derive(Default)]
pub struct EvalApprover {
    pub allow_shell: Vec<String>,
}

impl Approver for EvalApprover {
    fn request(
        &self,
        req: ApprovalRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Decision> + Send + '_>> {
        Box::pin(async move {
            if req.tool == "fs" {
                return Decision::Allow;
            }
            if matches!(
                (req.tool.as_str(), req.action.as_str()),
                ("shell", "run") | ("proc", "start")
            ) && let Some(summary) = req
                .summary
                .strip_prefix("background: ")
                .unwrap_or(&req.summary)
                .strip_prefix("$ ")
            {
                // The toolbox adds a cwd annotation outside the command text being authorized.
                let command = summary.rsplit_once("\nin ").map_or(summary, |(cmd, _)| cmd);
                if crate::workspace::escalation::check_command(command).is_ok()
                    && !command.contains([';', '&', '|', '$', '`', '>', '<', '\n', '\r'])
                    && self
                        .allow_shell
                        .iter()
                        .any(|prefix| command.starts_with(prefix))
                {
                    return Decision::Allow;
                }
            }
            Decision::Deny
        })
    }
}

/// Interactive permission requests with session-lifetime grants.
pub struct ChannelApprover {
    sender: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<Decision>)>,
    allowed: Arc<Mutex<HashSet<(String, String)>>>,
}

impl ChannelApprover {
    #[must_use]
    pub fn new(
        sender: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<Decision>)>,
    ) -> Self {
        Self {
            sender,
            allowed: Arc::default(),
        }
    }
}

impl Approver for ChannelApprover {
    fn request(
        &self,
        req: ApprovalRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Decision> + Send + '_>> {
        Box::pin(async move {
            let key = (req.tool.clone(), req.action.clone());
            let allow_session = req.allow_session;
            // Name-only grants must not bypass the registry's definition-hash approval checks.
            let connector = req.class.as_deref() == Some(crate::connectors::exec::CLASS);
            if allow_session
                && !connector
                && !req.summary.starts_with("could not parse safely:")
                && self
                    .allowed
                    .lock()
                    .expect("approval permissions")
                    .contains(&key)
            {
                return Decision::Allow;
            }
            let (sender, receiver) = oneshot::channel();
            if self.sender.send((req, sender)).is_err() {
                return Decision::Deny;
            }
            let decision = receiver.await.unwrap_or(Decision::Deny);
            if decision == Decision::AllowAlways && !connector {
                return Decision::Allow;
            }
            if decision == Decision::AllowForSession && !allow_session {
                return Decision::Allow;
            }
            if decision == Decision::AllowForSession && !connector {
                self.allowed
                    .lock()
                    .expect("approval permissions")
                    .insert(key);
            }
            decision
        })
    }

    fn request_many(
        &self,
        mut reqs: Vec<ApprovalRequest>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = WaveDecision> + Send + '_>> {
        Box::pin(async move {
            {
                let allowed = self.allowed.lock().expect("approval permissions");
                reqs.retain(|req| {
                    !req.allow_session || !allowed.contains(&(req.tool.clone(), req.action.clone()))
                });
            }
            match reqs.len() {
                0 => return WaveDecision::AllowAll,
                1 => return WaveDecision::ReviewEach,
                _ => {}
            }
            let mut counts = [0; 4];
            for req in &reqs {
                let index = match (req.tool.as_str(), req.action.as_str()) {
                    ("fs", "edit" | "write") => 0,
                    ("fs", "move") => 1,
                    ("fs", "delete") => 2,
                    ("shell", _) => 3,
                    _ => continue,
                };
                counts[index] += 1;
            }
            let mut summary = counts
                .into_iter()
                .zip(["edit", "move", "delete", "command"])
                .filter(|(count, _)| *count > 0)
                .map(|(count, kind)| format!("{count} {kind}{}", if count == 1 { "" } else { "s" }))
                .collect::<Vec<_>>()
                .join(", ");
            for (index, req) in reqs.iter().enumerate() {
                let _ = write!(summary, "\n{}. {} {}", index + 1, req.tool, req.action);
                for line in req.detail.as_ref().unwrap_or(&req.summary).split('\n') {
                    let _ = write!(summary, "\n  {line}");
                }
            }
            let request = ApprovalRequest {
                tool: "wave".to_string(),
                class: None,
                detail: None,
                action: format!("{} calls", reqs.len()),
                summary,
                allow_session: reqs.iter().all(|req| req.allow_session),
            };
            let allow_session = request.allow_session;
            let (sender, receiver) = oneshot::channel();
            if self.sender.send((request, sender)).is_err() {
                return WaveDecision::DenyAll;
            }
            match receiver.await.unwrap_or(Decision::Deny) {
                Decision::Allow | Decision::AllowAlways => WaveDecision::AllowAll,
                Decision::AllowForSession if !allow_session => WaveDecision::AllowAll,
                Decision::AllowForSession => {
                    self.allowed
                        .lock()
                        .expect("approval permissions")
                        .extend(reqs.into_iter().map(|req| (req.tool, req.action)));
                    WaveDecision::AllowAllForSession
                }
                Decision::ReviewEach => WaveDecision::ReviewEach,
                Decision::Deny => WaveDecision::DenyAll,
            }
        })
    }
}

pub fn clamp_cursor(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    (current as i32 + delta).clamp(0, len as i32 - 1) as usize
}

pub fn fuzzy_filter_sorted<T>(items: &[T], score_fn: impl Fn(&T) -> Option<i32>) -> Vec<&T> {
    let mut scored: Vec<(i32, &T)> = items
        .iter()
        .filter_map(|item| score_fn(item).map(|sc| (sc, item)))
        .collect();
    scored.sort_by_key(|(sc, _)| std::cmp::Reverse(*sc));
    scored.into_iter().map(|(_, item)| item).collect()
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum FilesTab {
    Files,
    Images,
    Scripts,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Popup {
    None,
    Model,
    Session,
    Key,
    Settings,
    Copy,
    Approval,
    Space,
    Context,
    Skills,
    Files,
    Apps,
    InteractiveUi,
    ResearchLive,
    Swarm,
    Connectors,
    Library,
    Subagents,
    Usage,
    Harness,
    Procs,
    Jobs,
    Login,
    Local,
    Help,
    ImageViewer,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SwarmPopupMode {
    Browse,
    ConfirmDelete,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum AppsMode {
    Browse,
    ConfirmDelete,
    EditFile,
    RenamePanel,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SkillsMode {
    Browse,
    Install,
    ConfirmRemove,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum FilesMode {
    Browse,
    Add,
    Rename,
    ConfirmDelete,
    Pick,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ImagesMode {
    Browse,
    ConfirmDelete,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ScriptsMode {
    Browse,
    Create,
    Rename,
    ConfirmDelete,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SpaceMode {
    Browse,
    Create,
    Rename,
    ConfirmDelete,
}

pub struct CopyOption {
    pub label: String,
    pub text: String,
}

pub struct ContextBreakdown {
    pub system_tokens: u64,
    pub memory_tokens: u64,
    pub skills_tokens: u64,
    pub tools_tokens: u64,
    pub conversation_tokens: u64,
    pub limit: Option<u64>,
    pub compacted: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum WorkMode {
    Chat,
    Agent,
    AgentAuto,
}

impl WorkMode {
    pub fn is_agent(self) -> bool {
        self != Self::Chat
    }

    pub(super) fn binding_mode(self) -> &'static str {
        if self == Self::AgentAuto {
            "auto"
        } else {
            "agent"
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SessionMode {
    Browse,
    Rename,
    ConfirmDelete,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum MouseTarget {
    None,
    Input,
    History,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ModelPanel {
    Favorites,
    Available,
}

/// Cache accounting across every API request in a user turn.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheTally {
    pub prompt_tokens: u64,
    pub cache_read_tokens: u64,
    /// Requests without cache accounting, excluded from the reported rate.
    pub unrated_requests: u64,
}

impl CacheTally {
    pub fn observe(&mut self, usage: &crate::provider::Usage) {
        if usage.prompt_convention.ratio_is_meaningful() {
            self.prompt_tokens += usage.prompt_tokens;
            self.cache_read_tokens += usage.cache_read_tokens;
        } else {
            self.unrated_requests += 1;
        }
    }

    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn rate(&self) -> Option<f64> {
        (self.prompt_tokens > 0).then(|| self.cache_read_tokens as f64 / self.prompt_tokens as f64)
    }

    #[must_use]
    pub const fn is_partial(&self) -> bool {
        self.unrated_requests > 0
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Default)]
pub enum ModelPickTarget {
    #[default]
    Session,
    SwarmPersona(usize),
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SettingsField {
    RemoteImages,
    ShowStats,
    ShowReasoning,
    HideHints,
    Temperature,
    TopP,
    MaxTokens,
    CompactThreshold,
    SearxngUrl,
    Verbosity,
    LangsearchKey,
    SearchProvider,
    BlockedDomains,
    ReplyStyle,
    ColumnWidth,
    ImageViewer,
}

impl SettingsField {
    pub const ALL: [Self; 16] = [
        Self::RemoteImages,
        Self::ShowStats,
        Self::ShowReasoning,
        Self::HideHints,
        Self::Temperature,
        Self::TopP,
        Self::MaxTokens,
        Self::CompactThreshold,
        Self::SearxngUrl,
        Self::Verbosity,
        Self::LangsearchKey,
        Self::SearchProvider,
        Self::BlockedDomains,
        Self::ReplyStyle,
        Self::ColumnWidth,
        Self::ImageViewer,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::RemoteImages => "remote images and site icons (Space toggles on/off)",
            Self::ReplyStyle => "reply style (Space cycles interactive/auto/rich/plain)",
            Self::ColumnWidth => {
                "column width (Space cycles auto/full/100/140/180/220; or drag the column edge)"
            }
            Self::ImageViewer => {
                "image viewer (Space cycles auto/terminal/system; auto = in-terminal when graphics are available)"
            }
            Self::ShowStats => "show stats (model · TPS footer)",
            Self::ShowReasoning => "expand reasoning (Ctrl+R)",
            Self::HideHints => "hide hints (keybind labels)",
            Self::Temperature => "temperature",
            Self::TopP => "top_p",
            Self::MaxTokens => "max_tokens",
            Self::CompactThreshold => "auto-compact at (% of context, 0 disables)",
            Self::SearxngUrl => "optional SearXNG URL (auto search needs no setup)",
            Self::Verbosity => "answer length (Space cycles normal/concise/caveman)",
            Self::LangsearchKey => "optional LangSearch key (langsearch.com/dashboard, free)",
            Self::SearchProvider => {
                "search provider (auto works without setup; Space cycles auto/langsearch/searxng/exa/provider/duckduckgo)"
            }
            Self::BlockedDomains => "blocked domains (comma-separated, always excluded; per-space)",
        }
    }
}

pub struct SettingsGroup {
    pub name: &'static str,
    pub fields: &'static [SettingsField],
}

pub const SETTINGS_GROUPS: &[SettingsGroup] = &[
    SettingsGroup {
        name: "Interface",
        fields: &[
            SettingsField::ShowStats,
            SettingsField::ShowReasoning,
            SettingsField::HideHints,
            SettingsField::Verbosity,
        ],
    },
    SettingsGroup {
        name: "Generation",
        fields: &[
            SettingsField::Temperature,
            SettingsField::TopP,
            SettingsField::MaxTokens,
        ],
    },
    SettingsGroup {
        name: "Context",
        fields: &[SettingsField::CompactThreshold],
    },
    SettingsGroup {
        name: "Web Search",
        fields: &[
            SettingsField::SearchProvider,
            SettingsField::SearxngUrl,
            SettingsField::LangsearchKey,
            SettingsField::BlockedDomains,
        ],
    },
    SettingsGroup {
        name: "Reply Layouts",
        fields: &[
            SettingsField::ReplyStyle,
            SettingsField::RemoteImages,
            SettingsField::ColumnWidth,
            SettingsField::ImageViewer,
        ],
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    Group(usize),
    Field(SettingsField),
}

pub const REPLY_STYLES: [&str; 4] = ["interactive", "auto", "rich", "plain"];
pub const IMAGE_VIEWERS: [&str; 3] = ["auto", "terminal", "system"];
pub const VERBOSITY_LEVELS: [&str; 3] = ["normal", "concise", "caveman"];
pub const SEARCH_PROVIDERS: [&str; 6] = [
    "auto",
    "langsearch",
    "searxng",
    "exa",
    "provider",
    "duckduckgo",
];

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct Settings {
    /// Fetch untrusted remote images through the device-local safety cache.
    pub remote_images: bool,
    pub reply_style: String,
    pub image_viewer: String,
    pub show_stats: bool,
    pub show_reasoning: bool,
    pub hide_hints: bool,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Context-window percentage that triggers compaction, or zero to disable it.
    pub compact_threshold: u8,
    /// Conversation width as `auto`, `full`, or a cell count.
    pub column_width: String,
    /// Comma-separated `id:WWxHH` overrides in screen percentages.
    pub popup_sizes: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            remote_images: true,
            reply_style: "interactive".into(),
            image_viewer: "auto".into(),
            show_stats: false,
            show_reasoning: false,
            hide_hints: false,
            temperature: None,
            top_p: None,
            max_tokens: None,
            compact_threshold: 60,
            column_width: "auto".into(),
            popup_sizes: String::new(),
        }
    }
}

fn verbosity_clause(level: &str) -> &'static str {
    match level {
        "normal" => {
            "Answer at whatever length the question deserves — don't optimize for brevity over completeness."
        }
        "caveman" => {
            "Talk caveman-terse. Drop articles (a/an/the) and filler words. Short fragments over full sentences. No politeness, no hedging, no restating the question. Symbols over words where clear (→, =, vs). Full explanations ONLY when explicitly asked — otherwise state the fact/answer and stop. Preserve numbers, code, names, and technical terms exactly."
        }
        _ => {
            "Answer style: default to short. Say the answer, then stop — don't restate the question, don't add a summary, don't hedge (\"it's worth noting...\", \"generally speaking...\"). No preamble (\"Great question!\", \"I'd be happy to help with that\"), no postamble (\"Let me know if you have questions!\"). One clear sentence beats three vague ones.\nThis is a floor, not a ceiling: if the user asks to explain, teach, or go deep, or the topic genuinely needs multiple steps to be correct (debugging, multi-part instructions, tradeoffs), give it the room it needs. Brevity for its own sake that omits a needed step is wrong, not concise.\nKeep full grammar — this isn't telegraphic shorthand. Drop filler, not clarity."
        }
    }
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const GREETINGS: [&str; 8] = [
    "What are we building today?",
    "Ask me anything.",
    "Fresh session, fresh ideas.",
    "The terminal is your canvas.",
    "Ready when you are.",
    "Type / to see commands.",
    "Let's get to work.",
    "How can I help?",
];

const THINKING: [(&str, &str); 16] = [
    ("Thinking", "Thought"),
    ("Pondering", "Pondered"),
    ("Noodling", "Noodled"),
    ("Ruminating", "Ruminated"),
    ("Cogitating", "Cogitated"),
    ("Marinating", "Marinated"),
    ("Percolating", "Percolated"),
    ("Mulling", "Mulled"),
    ("Conjuring", "Conjured"),
    ("Vibing", "Vibed"),
    ("Scheming", "Schemed"),
    ("Wrangling tokens", "Wrangled tokens"),
    ("Brewing", "Brewed"),
    ("Musing", "Mused"),
    ("Galaxy-braining", "Galaxy-brained"),
    ("Doing the thing", "Did the thing"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SpinnerColor {
    Green,
    Cyan,
    Magenta,
}

const SPINNER_COLORS: [SpinnerColor; 3] = [
    SpinnerColor::Green,
    SpinnerColor::Cyan,
    SpinnerColor::Magenta,
];

type ModelsResult = std::result::Result<Vec<Model>, String>;

#[derive(Clone)]
pub struct ImageMeta {
    pub name: String,
    pub size: u64,
    pub modified: String,
}

#[derive(Clone)]
pub struct ScriptMeta {
    pub name: String,
    pub size: u64,
    pub modified: String,
}

pub const MAX_CHAT_TASKS: usize = 10;

pub type ChatTaskId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextBasis {
    pub exact: u64,
    pub chars: usize,
    pub messages: usize,
    pub task: ChatTaskId,
}

#[derive(Clone)]
pub struct GateState {
    pub session_id: String,
    pub phase: SurveyPhase,
}

pub struct ChatEvent {
    pub task_id: ChatTaskId,
    pub event: StreamEvent,
}

/// UI and persistence state for one in-flight chat response.
pub struct ChatTask {
    pub id: ChatTaskId,
    pub session_id: String,
    pub session_title: String,
    pub space_id: String,
    pub model: String,
    /// Wire model id without the backend prefix, used for pricing.
    pub model_id: String,
    pub backend: BackendTag,
    pub incognito: bool,
    pub buffer: String,
    pub thinking: String,
    pub tool_status: Option<String>,
    pub usage: Option<Usage>,
    /// Latest usage row, revised in place by trailing `OpenCode` Zen cost events.
    pub usage_row_id: Option<i64>,
    pub started: std::time::Instant,
    pub thinking_idx: usize,
    pub spinner_color: SpinnerColor,
    pub abort: tokio::task::AbortHandle,
}

/// Token and cost totals across a turn's requests, including cost-only revisions.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TurnTally {
    pub output_tokens: u64,
    settled_cost: Option<f64>,
    last_cost: Option<f64>,
}

impl TurnTally {
    /// Record a request or, with no token count, revise the latest request's cost.
    pub fn observe(&mut self, output_tokens: Option<u64>, cost: Option<f64>) {
        if let Some(tokens) = output_tokens {
            self.settled_cost = add_cost(self.settled_cost, self.last_cost.take());
            self.output_tokens += tokens;
            self.last_cost = cost;
        } else if cost.is_some() {
            self.last_cost = cost;
        }
    }

    #[must_use]
    pub fn cost(&self) -> Option<f64> {
        add_cost(self.settled_cost, self.last_cost)
    }
}

fn add_cost(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    }
}

pub struct ChatNotification {
    pub session_id: String,
    pub title: String,
    pub text: String,
    pub success: bool,
}

/// Embedding result keyed by space and file, with an empty file id denoting a probe.
pub type EmbedMsg = (
    String,
    String,
    std::result::Result<Vec<(i64, Vec<f32>)>, String>,
);

pub type ResearchMsg = (String, String, String, research::ResearchUpdate);

pub struct SurveyGate {
    pub session_id: String,
    pub reply_tx: mpsc::UnboundedSender<String>,
    pub phase: SurveyPhase,
    /// Retain the prompt in memory so incognito gates survive session switches without persistence.
    pub prompt_role: String,
    pub prompt_content: String,
}

#[derive(Clone)]
pub enum SurveyPhase {
    /// One-based clarification round.
    Clarify { round: u8 },
    /// `rework` marks an artifact revised after user edits.
    Approve { rework: bool },
}

#[derive(Clone)]
pub enum LoginMsg {
    Status(String),
    Done(Result<crate::config::CodexCredentials, String>),
}

/// A request for the frontend to suspend its terminal and run an external editor or command.
pub enum PendingEditor {
    AppFile(std::path::PathBuf),
    Persona(std::path::PathBuf),
    ScriptFile(std::path::PathBuf),
    /// A terminal command recorded without capturing its output.
    UserCommand(String),
    Memory {
        space: String,
        path: std::path::PathBuf,
    },
}

// Dropping a JoinHandle detaches its task; this guard must abort child streams instead.
pub struct AbortOnDrop(pub tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Frontend feedback and background results, with `None` marking a closed source channel.
#[derive(Clone)]
pub enum AppEvent {
    Status(String),
    ComposerSet(String),
    ComposerClear,
    ViewportReset,
    HistoryInvalidated,
    OpenLoginPopup,
    /// Consumers must match the gate's session before intercepting input.
    Gate(Option<GateState>),
    Stream(Option<(ChatTaskId, StreamEvent)>),
    Models(Option<ModelsResult>),
    Title(Option<(String, String, String)>),
    Compact(Option<(String, String, i64, u64)>),
    SkillInstall(Option<Result<String, String>>),

    Convert(Option<(String, String, files::ConvertUpdate)>),
    Embed(Option<EmbedMsg>),
    Research(Option<ResearchMsg>),
    ResearchTopic(Option<Result<String, String>>),
    UpdateCheck(Option<String>),
    Login(Option<LoginMsg>),
    Swarm(Option<swarm::SwarmMsg>),
    LocalServers(Option<Vec<crate::provider::serve::RuntimeStatus>>),
}

#[allow(clippy::struct_field_names, clippy::struct_excessive_bools)]
pub struct App {
    pub db: Db,
    pub space: Space,
    pub backends: Backends,
    pub saved: crate::config::SavedCreds,
    pub experiments: config::Experiments,
    pub trace_store: Option<std::sync::Arc<crate::trace::TraceStore>>,
    pub last_turn: Option<(String, String)>,

    pub active_space: SpaceRow,
    /// Memory frozen for the cache epoch; mid-session facts remain in the transcript.
    pub(crate) memory_snapshot: String,
    pub(crate) memory_docs: Vec<crate::memory::book::DocEntry>,
    pub(crate) memory_tidy_due: usize,
    /// Advance when the serialized prefix changes to select a fresh provider-cache lane.
    pub(crate) cache_epoch: u64,
    /// Frozen timestamp preventing clock ticks from invalidating the provider-cache prefix.
    pub(crate) prompt_datetime: String,
    pub index: IndexState,
    pub search: SearchConfig,
    pub base_system_prompt: String,
    /// User additions from `system_prompt.md`, loaded at boot.
    pub system_prompt_overlay: String,
    pub interactive_ui: bool,
    pub interactive_approvals: bool,
    pub approvals: ApprovalState,
    pub deck_link: Arc<std::sync::Mutex<crate::deck::link::Context>>,
    /// Approved workspace root deferred until the current chat task finishes.
    pub(crate) queued_workspace: Option<(ChatTaskId, String)>,
    pub user_command: UserCommandState,
    pub verbosity: String,
    pub ui_repair: UiRepairState,
    pub compaction: CompactionState,

    pub skills: Vec<crate::skills::Skill>,
    pub connectors: crate::connectors::Registry,
    /// Override connector import locations, or `None` for the device's real paths.
    pub import_paths: Option<crate::connectors::import::ImportPaths>,
    pub connector_notes_tx: mpsc::UnboundedSender<String>,
    connector_notes_rx: mpsc::UnboundedReceiver<String>,
    pub forced_skill: Option<String>,
    pub work_mode: WorkMode,
    pub auto_sandbox_available: bool,
    decision_context: Arc<Mutex<(String, String)>>,
    decider_availability: Arc<crate::decider::Availability>,
    decider_status_rx: mpsc::UnboundedReceiver<String>,
    pub workspace: Option<std::sync::Arc<crate::workspace::Workspace>>,
    pub incognito: bool,
    pub incognito_img_dir: Option<std::path::PathBuf>,
    pub research: ResearchState,
    pub subagent_runs: Vec<SubagentRun>,
    pub subagent_turn_cost: HashMap<ChatTaskId, f64>,
    pub turn_usage: HashMap<ChatTaskId, TurnTally>,
    pub toolbox: std::sync::Arc<dyn crate::tools::ToolExecutor>,
    image_fetcher: Arc<crate::remote_image::Fetcher>,
    pub procs: Arc<crate::workspace::procs::Procs>,
    pub app_server: Option<crate::appserver::AppServer>,
    pub skills_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,
    pub local: LocalRuntimeState,
    pub login_rx: Option<mpsc::UnboundedReceiver<LoginMsg>>,
    pub update_rx: Option<mpsc::UnboundedReceiver<Option<String>>>,
    /// Handle refreshed Codex tokens internally so they never enter frontend events.
    codex_refresh_rx: Option<mpsc::UnboundedReceiver<Result<config::CodexCredentials, String>>>,
    pub swarm: SwarmState,
    pub caches: ListCaches,
    pub usage_range: crate::db::UsageRange,
    pub harness: Option<crate::db::HarnessSummary>,

    pub models: Vec<Model>,
    pub current_model: Option<String>,
    router_note_shown: bool,
    pub models_rx: Option<mpsc::UnboundedReceiver<ModelsResult>>,
    pub favorites: HashSet<String>,
    pub last_used: HashMap<String, String>,
    pub reasoning: HashMap<String, String>,

    pub session: Option<Session>,
    pub messages: Vec<Message>,
    pub mask_through: usize,

    pub chat_event_tx: mpsc::UnboundedSender<ChatEvent>,
    pub chat_event_rx: mpsc::UnboundedReceiver<ChatEvent>,
    pub chat_tasks: HashMap<ChatTaskId, ChatTask>,
    pub next_chat_task_id: ChatTaskId,
    pub notifications: VecDeque<ChatNotification>,
    /// Locally queued events take priority over background channels.
    pub(crate) pending_events: VecDeque<AppEvent>,
    pub unread: std::collections::HashSet<String>,
    pub meter: ContextMeter,

    pub settings: Settings,
    pub model_pick_target: ModelPickTarget,
    pub spinner_frame: usize,
    pub thinking_idx: usize,
    pub spinner_color: SpinnerColor,

    pub(crate) title_rx: Option<mpsc::UnboundedReceiver<(String, String, String)>>,
}

impl App {
    pub fn new(db: Db, key: Option<&str>, space: Space) -> Self {
        Self::with_creds(db, space, config::SavedCreds::from_key(key))
    }

    /// Construct app state from saved credentials without network access.
    #[allow(clippy::too_many_lines)]
    pub fn with_creds(db: Db, space: Space, saved: config::SavedCreds) -> Self {
        let active_space = db
            .default_space_id()
            .ok()
            .and_then(|id| {
                db.list_spaces()
                    .ok()
                    .and_then(|s| s.into_iter().find(|s| s.id == id))
            })
            .unwrap_or_else(|| SpaceRow {
                id: String::new(),
                name: DEFAULT_SPACE.to_string(),
                created_at: Utc::now().to_rfc3339(),
            });
        let _ = space.ensure_space_dir(&active_space.name);
        let skills_dir = crate::skills::skills_dir(&space.root);
        let skill_dirs = crate::skills::app_skill_roots(&space.root);
        let skills = crate::skills::load_skills_from_dirs(&skill_dirs);
        let (chat_event_tx, chat_event_rx) = mpsc::unbounded_channel();
        let (decider_status_tx, decider_status_rx) = mpsc::unbounded_channel();
        let (connector_notes_tx, connector_notes_rx) = mpsc::unbounded_channel();
        let image_fetcher =
            crate::remote_image::Fetcher::new(space.db_path().with_file_name("cache"));
        let toolbox = std::sync::Arc::new(
            crate::tools::ToolBox::new(
                skills_dir,
                None,
                None,
                "auto".to_string(),
                Vec::new(),
                Some(space.db_path()),
                Some(crate::tools::FilesCtx {
                    db_path: space.db_path(),
                    space_id: active_space.id.clone(),
                    embedder: None,
                }),
                None,
            )
            .with_skill_dirs(skill_dirs),
        );
        let mut app = Self {
            db,
            space,
            backends: Backends::default(),
            saved,
            experiments: config::Experiments::default(),
            trace_store: None,
            last_turn: None,
            skills,
            connectors: crate::connectors::Registry::empty(),
            import_paths: None,
            connector_notes_tx,
            connector_notes_rx,
            search: SearchConfig::new(),
            forced_skill: None,
            work_mode: WorkMode::Chat,
            auto_sandbox_available: crate::workspace::shell::write_confinement_available(),
            decision_context: Arc::default(),
            decider_availability: Arc::new(crate::decider::Availability::new(decider_status_tx)),
            decider_status_rx,
            workspace: None,
            incognito: false,
            incognito_img_dir: None,
            research: ResearchState::default(),
            subagent_runs: Vec::new(),
            subagent_turn_cost: HashMap::new(),
            turn_usage: HashMap::new(),
            toolbox,
            image_fetcher,
            procs: Arc::default(),
            app_server: None,
            skills_rx: None,
            local: LocalRuntimeState::default(),
            login_rx: None,
            update_rx: None,
            codex_refresh_rx: None,
            swarm: SwarmState::default(),
            caches: ListCaches::default(),
            usage_range: crate::db::UsageRange::default(),
            harness: None,
            active_space,
            memory_snapshot: String::new(),
            memory_docs: Vec::new(),
            memory_tidy_due: 0,
            cache_epoch: 0,
            prompt_datetime: Utc::now().format("%Y-%m-%d %H:%M UTC, %A").to_string(),
            index: IndexState::new(),
            base_system_prompt: config::BASE_SYSTEM_PROMPT.to_string(),
            system_prompt_overlay: String::new(),
            interactive_ui: false,
            interactive_approvals: false,
            approvals: ApprovalState::default(),
            deck_link: Arc::default(),
            queued_workspace: None,
            user_command: UserCommandState::default(),
            verbosity: "concise".to_string(),
            ui_repair: UiRepairState::new(),
            compaction: CompactionState::default(),
            models: Vec::new(),
            current_model: None,
            router_note_shown: false,
            models_rx: None,
            favorites: HashSet::new(),
            last_used: HashMap::new(),
            reasoning: HashMap::new(),
            session: None,
            messages: Vec::new(),
            mask_through: 0,
            chat_event_tx,
            chat_event_rx,
            chat_tasks: HashMap::new(),
            next_chat_task_id: 0,
            notifications: VecDeque::new(),
            pending_events: VecDeque::new(),
            unread: std::collections::HashSet::new(),
            meter: ContextMeter::default(),
            settings: Settings::default(),
            model_pick_target: ModelPickTarget::Session,
            spinner_frame: 0,
            thinking_idx: 0,
            spinner_color: SpinnerColor::Green,
            title_rx: None,
        };
        app.rebuild_all_backends();
        app.index.embedding_model = app
            .backends
            .configured_tags()
            .first()
            .and_then(|tag| app.backends.get(*tag))
            .map_or_else(
                || "openai/text-embedding-3-small".to_string(),
                |provider| {
                    let model = provider.default_embedding_model();
                    if model.is_empty() {
                        String::new()
                    } else {
                        format!("{}{}", provider.backend_tag().key_prefix(), model)
                    }
                },
            );
        if !app.backends.any() {
            app.push_status("no API key — run /login to add OpenRouter, OpenAI, OpenCode, Codex, Mistral or a local model".to_string());
        }
        app.refresh_memory_snapshot();
        app.load_prefs();
        app.load_settings();
        app
    }

    fn open_workspace(
        &self,
        root: Option<&str>,
    ) -> Result<std::sync::Arc<crate::workspace::Workspace>> {
        let cwd = std::env::current_dir()?;
        let root = match root {
            Some(path) if path == "~" || path.starts_with("~/") => {
                let home =
                    std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
                std::path::PathBuf::from(home).join(path.strip_prefix("~/").unwrap_or(""))
            }
            Some(path) => cwd.join(path),
            None => cwd,
        }
        .canonicalize()?;
        Ok(Arc::new(
            self.open_workspace_root(&root)?
                .with_checkpoints(self.space.root.join("checkpoints")),
        ))
    }

    pub fn validate_job_root(&self, root: &str) -> Result<String> {
        Ok(self
            .open_workspace(Some(root))?
            .root()
            .to_string_lossy()
            .into_owned())
    }

    pub(crate) fn enter_job_workspace(
        &mut self,
        root: &std::path::Path,
        mode: WorkMode,
        approver: Arc<crate::jobs::JobApprover>,
    ) -> Result<()> {
        let root = root.canonicalize()?;
        let parent = self.space.root.join("job-worktrees").canonicalize()?;
        anyhow::ensure!(
            root.parent() == Some(parent.as_path()),
            "invalid job run directory"
        );
        let mut deny = Vec::new();
        let config_dir = config::project_dirs()?.config_dir().to_path_buf();
        if config_dir.exists() {
            deny.push(config_dir);
        }
        let workspace = crate::workspace::Workspace::open(&root, deny)?;
        workspace.resolve(".", crate::workspace::Access::Read)?;
        self.reset_user_command_dir();
        self.workspace = Some(Arc::new(workspace));
        self.work_mode = mode;
        self.approvals.job_approver = Some(approver);
        self.refresh_toolbox();
        Ok(())
    }

    pub fn collect_job_notifications(&mut self) -> Result<()> {
        let seen = self.db.setting("jobs_seen").unwrap_or_default();
        let jobs = self.db.list_jobs()?;
        let mut newest = seen.clone();
        let runs = self.db.list_job_runs()?;
        for run in &runs {
            let Some(finished) = &run.finished_at else {
                continue;
            };
            if finished <= &seen {
                continue;
            }
            newest = newest.max(finished.clone());
            let Some(job) = jobs.iter().find(|job| job.id == run.job_id) else {
                continue;
            };
            if run.status == "done"
                && (job.spec.notify == "on_failure"
                    || (job.spec.notify == "on_change"
                        && !crate::jobs::notification_visible(&self.db, run, &runs)?))
            {
                continue;
            }
            self.unread.insert(run.id.clone());
            self.notifications.push_back(ChatNotification {
                session_id: run.id.clone(),
                title: format!("job: {}", job.spec.name),
                text: format!("{} · {}", run.status, run.summary),
                success: run.status == "done",
            });
        }
        if newest != seen {
            self.db.set_setting("jobs_seen", &newest)?;
        }
        Ok(())
    }

    fn open_workspace_root(&self, root: &std::path::Path) -> Result<crate::workspace::Workspace> {
        let root = root.canonicalize()?;
        let data = self.space.root.canonicalize()?;
        if data.starts_with(&root) || root.starts_with(&data) {
            anyhow::bail!(
                "workspace {} is or contains nexus's data root, or is inside it · choose another directory",
                root.display()
            );
        }
        let mut deny = vec![data];
        let config_dir = config::project_dirs()?.config_dir().to_path_buf();
        if config_dir.exists() {
            deny.push(config_dir.canonicalize()?);
        }
        let workspace = crate::workspace::Workspace::open(&root, deny)?;
        workspace.resolve(".", crate::workspace::Access::Read)?;
        Ok(workspace)
    }

    /// Enter an incognito evaluation workspace without session bindings or durable checkpoints.
    pub fn enter_eval_workspace(&mut self, root: &std::path::Path) -> Result<()> {
        let workspace = self.open_workspace_root(root)?;
        if !self.incognito {
            self.execute(AppCommand::Incognito { on: true })?;
        }
        self.reset_user_command_dir();
        self.workspace = Some(Arc::new(workspace));
        self.work_mode = WorkMode::Agent;
        if self.approvals.eval_approver.is_none() {
            self.approvals.eval_approver = Some(Arc::new(EvalApprover::default()));
        }
        self.refresh_toolbox();
        self.bump_cache_epoch();
        Ok(())
    }

    fn improve_nexus(&mut self, what: &str) -> Result<()> {
        use crate::jobs::service::{Tree, git};

        let Some(repo) = self
            .saved
            .agent
            .nexus_repo
            .as_deref()
            .filter(|path| !path.trim().is_empty())
        else {
            self.push_status(
                "/improve needs the nexus checkout · set [agent] nexus_repo = \"~/work/nexus-chat\" in config.toml",
            );
            return Ok(());
        };
        if self.incognito {
            self.push_status("agent mode is not available in incognito mode");
            return Ok(());
        }
        if !self.backends.any() {
            self.push_status("set your API key first with /login");
            return Ok(());
        }
        if self.current_model.is_none() {
            self.push_status("pick a model first with /model");
            return Ok(());
        }
        if self.chat_task_count() >= MAX_CHAT_TASKS {
            self.push_status(format!("chat task limit reached ({MAX_CHAT_TASKS})"));
            return Ok(());
        }
        let repo = self.open_workspace(Some(repo))?;
        git(repo.root(), &["rev-parse", "--verify", "HEAD"])?;
        let since = (Utc::now() - chrono::Duration::days(7)).to_rfc3339();
        let failures = self
            .db
            .harness_summary(&since)?
            .rows
            .iter()
            .take(5)
            .map(|row| {
                format!(
                    "{}.{}: {} ×{}",
                    row.tool, row.action, row.error_kind, row.count
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!(
            "Improve nexus: {what}.\nRelevant recent tool failures:\n{failures}\nAcceptance: scripts/check.sh must pass. Work only in this worktree; finish with a summary of the change."
        );
        let id = uuid::Uuid::new_v4().to_string();
        let slug: String = sessions::slugify(what).chars().take(60).collect();
        let branch = format!("nexus/improve/{slug}-{}", &id[..8]);
        let tree = Tree::prepare(
            repo.root(),
            &std::env::temp_dir().join("nexus-improve"),
            &slug,
            &id,
        )?;
        git(&tree.path, &["branch", "-m", &branch])?;
        let root = tree.path.to_string_lossy();
        self.open_workspace(Some(&root))?;
        if !self.messages.is_empty() {
            self.new_session();
        }
        self.set_work_mode(WorkMode::Agent, Some(&root))?;
        self.send_message(text)?;
        self.push_status(format!(
            "improve branch: {branch} · review and merge yourself · remove with git worktree remove {}",
            tree.path.display()
        ));
        Ok(())
    }

    fn set_work_mode(&mut self, mode: WorkMode, root: Option<&str>) -> Result<()> {
        if mode.is_agent() && self.incognito {
            self.push_status("agent mode is not available in incognito mode");
            return Ok(());
        }
        let workspace = if mode.is_agent() {
            match self.open_workspace(root) {
                Ok(workspace) => Some(workspace),
                Err(error) => {
                    self.push_status(format!("workspace error: {error}"));
                    return Ok(());
                }
            }
        } else {
            self.workspace.clone()
        };
        if !self.incognito
            && let Some(session) = &self.session
        {
            self.db.set_session_workspace(
                &session.id,
                workspace
                    .as_ref()
                    .filter(|_| mode.is_agent())
                    .map(|workspace| workspace.root()),
            )?;
            if mode.is_agent() {
                self.db
                    .set_session_workspace_mode(&session.id, mode.binding_mode())?;
            }
        }
        self.reset_user_command_dir();
        self.work_mode = mode;
        self.workspace = workspace;
        self.refresh_toolbox();
        self.bump_cache_epoch();
        self.push_status(if mode.is_agent() {
            format!(
                "{} mode · workspace {}",
                if mode == WorkMode::AgentAuto {
                    "agent·auto"
                } else {
                    "agent"
                },
                self.workspace
                    .as_ref()
                    .expect("opened workspace")
                    .root()
                    .display()
            )
        } else {
            "chat mode — no workspace".to_string()
        });
        Ok(())
    }

    pub fn training_enabled(&self) -> bool {
        !self.incognito
            && self
                .db
                .setting(&format!("training:{}", self.active_space.id))
                .as_deref()
                == Some("on")
    }

    pub(crate) fn saved_secrets(&self) -> Vec<String> {
        BackendTag::ALL
            .into_iter()
            .filter_map(|tag| self.saved.key(tag))
            .chain(
                [
                    self.saved.codex.as_ref().map(|creds| creds.access.as_str()),
                    self.saved
                        .codex
                        .as_ref()
                        .map(|creds| creds.refresh.as_str()),
                    self.saved.host_token.as_deref(),
                ]
                .into_iter()
                .flatten(),
            )
            .filter(|secret| !secret.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn open_trace_store(&mut self) -> Option<std::sync::Arc<crate::trace::TraceStore>> {
        if let Some(store) = &self.trace_store {
            return Some(store.clone());
        }
        match crate::trace::TraceStore::open(
            &self.space.traces_dir(),
            crate::trace::Scrubber::new(self.saved_secrets()),
        ) {
            Ok(store) => {
                let store = std::sync::Arc::new(store);
                self.trace_store = Some(store.clone());
                Some(store)
            }
            Err(error) => {
                self.push_status(format!("training trace store unavailable: {error}"));
                None
            }
        }
    }

    fn load_prefs(&mut self) {
        if let Ok(prefs) = self.db.load_model_prefs() {
            for p in prefs {
                if p.favorite {
                    self.favorites.insert(p.id.clone());
                }
                if let Some(t) = p.last_used {
                    self.last_used.insert(p.id.clone(), t);
                }
                if let Some(r) = p.reasoning {
                    self.reasoning.insert(p.id, r);
                }
            }
        }
        if let Some((id, _)) = self.last_used.iter().max_by(|a, b| a.1.cmp(b.1)) {
            let id = id.clone();
            if self.backends.any() {
                self.push_status(format!("model: {id} — type a message, /model to change"));
            }
            self.current_model = Some(id);
        }
    }

    fn load_settings(&mut self) {
        let Ok(kv) = self.db.load_settings() else {
            return;
        };
        for (k, v) in kv {
            self.apply_setting(&k, &v);
        }
        self.refresh_toolbox();
    }

    fn apply_setting(&mut self, k: &str, v: &str) {
        match k {
            "remote_images" => {
                self.settings.remote_images = v == "on";
                self.image_fetcher.set_enabled(self.settings.remote_images);
            }
            "show_stats" => self.settings.show_stats = v == "1",
            "show_reasoning" => self.settings.show_reasoning = v == "1",
            "hide_hints" => self.settings.hide_hints = v == "1",
            "usage_range" => self.usage_range = crate::db::UsageRange::from_key(v),
            "temperature" => self.settings.temperature = v.parse().ok(),
            "top_p" => self.settings.top_p = v.parse().ok(),
            "max_tokens" => self.settings.max_tokens = v.parse().ok(),
            "compact_threshold" => {
                if let Ok(t) = v.parse() {
                    self.settings.compact_threshold = t;
                }
            }
            "searxng_url" => self.search.searxng_url = v.to_string(),
            "column_width" if settings::valid_column_width(v) => {
                self.settings.column_width = v.trim().to_string();
            }
            // Settings written by newer builds must remain readable by older clients.
            "popup_sizes" => self.settings.popup_sizes = settings::sanitize_popup_sizes(v),
            "reply_style" if REPLY_STYLES.contains(&v) => {
                if self.settings.reply_style != v {
                    self.settings.reply_style = v.to_string();
                    self.bump_cache_epoch();
                }
            }
            "image_viewer" if IMAGE_VIEWERS.contains(&v) => {
                self.settings.image_viewer = v.to_string();
            }
            "verbosity" if VERBOSITY_LEVELS.contains(&v) => {
                if self.verbosity != v {
                    self.verbosity = v.to_string();
                    self.bump_cache_epoch();
                }
            }
            "langsearch_key" => self.search.langsearch_key = v.to_string(),
            "search_provider" if SEARCH_PROVIDERS.contains(&v) => {
                self.search.provider = v.to_string();
            }
            _ => {}
        }
    }

    fn configure_auto_policy(&mut self, toolbox: &mut crate::tools::ToolBox) {
        use crate::config::ModelDecider;
        use crate::decider::{Chain, Decider, JevDecider, LlmDecider, ModelContext};

        toolbox.require_approval = self.work_mode.is_agent();
        toolbox.sandbox_available = self.auto_sandbox_available;
        self.decision_context = Arc::default();
        toolbox.decision_context = self.decision_context.clone();
        toolbox.decision_scrubber = crate::trace::Scrubber::new(self.saved_secrets());
        if self.work_mode == WorkMode::AgentAuto {
            let context = ModelContext {
                session: self.decision_context.clone(),
                recent_steps: toolbox.decision_steps.clone(),
                scrubber: crate::trace::Scrubber::new(self.saved_secrets()),
                sandbox_available: self.auto_sandbox_available,
                config: self.saved.agent.clone(),
                availability: self.decider_availability.clone(),
            };
            let has_key = self
                .saved
                .key(BackendTag::OpenRouter)
                .is_some_and(|key| !key.trim().is_empty());
            let following: Vec<Arc<dyn Decider>> = match self.saved.agent.selected_decider(has_key)
            {
                ModelDecider::Jev => vec![Arc::new(JevDecider {
                    provider: self
                        .backends
                        .get(BackendTag::OpenRouter)
                        .cloned()
                        .map(|p| self.usage_provider(p, "decider")),
                    context,
                })],
                ModelDecider::Llm => {
                    let backend = self
                        .session_backend()
                        .map(|(p, m)| (p.usage_kind("decider"), m));
                    vec![Arc::new(LlmDecider { backend, context })]
                }
                ModelDecider::Rules => Vec::new(),
            };
            toolbox.auto_decider = Some(Arc::new(Chain::new(
                self.saved.agent.allow.clone(),
                following,
            )));
        }
    }

    pub fn refresh_toolbox(&mut self) {
        self.rebuild_toolbox();
        self.reload_skills();
    }

    #[allow(clippy::too_many_lines)]
    fn rebuild_toolbox(&mut self) {
        let url = (!self.search.searxng_url.trim().is_empty())
            .then(|| self.search.searxng_url.trim().to_string());
        let key = (!self.search.langsearch_key.trim().is_empty())
            .then(|| self.search.langsearch_key.trim().to_string());
        // Skill discovery may read external roots, but writes must stay in the managed root.
        let skills_dir = crate::skills::skills_dir(&self.space.root);
        crate::skills::install_builtin(&skills_dir);
        let skill_dirs = crate::skills::app_skill_roots(&self.space.root);
        let mut toolbox = crate::tools::ToolBox::new(
            skills_dir.clone(),
            url,
            key,
            self.search.provider.clone(),
            self.blocked_domains(),
            Some(
                self.approvals
                    .eval_approver
                    .as_ref()
                    .and(self.workspace.as_ref())
                    .map_or_else(
                        || self.space.db_path(),
                        |workspace| {
                            workspace
                                .root()
                                .parent()
                                .expect("eval workspace parent")
                                .join("runtime.db")
                        },
                    ),
            ),
            Some(crate::tools::FilesCtx {
                db_path: self.space.db_path(),
                space_id: self.active_space.id.clone(),
                embedder: self.file_embedder(),
            }),
            self.app_server
                .as_ref()
                .filter(|_| !self.incognito)
                .map(|s| crate::tools::AppsCtx {
                    dir: self.space.apps_dir(&self.active_space.name),
                    server_port: s.port(),
                    public_base: s.public_base().map(str::to_string),
                    registry: s.registry().clone(),
                    space_name: self.active_space.name.clone(),
                    space_id: self.active_space.id.clone(),
                    space_db_path: self.space.db_path(),
                    files_dir: self.space.files_dir(&self.active_space.name),
                    session_id: self
                        .session
                        .as_ref()
                        .map(|s| s.id.clone())
                        .unwrap_or_default(),
                }),
        )
        .with_skill_dirs(skill_dirs);
        if self.is_research_session()
            && let Some(session_id) = self.session.as_ref().map(|s| s.id.clone())
        {
            toolbox = toolbox.with_research_session(session_id);
        }
        toolbox.search_backend = self.session_backend();
        toolbox.search_space_id.clone_from(&self.active_space.id);
        toolbox.image_gen_backend = self.generation_backend(OpenRouter::default_image_gen_model);
        toolbox.video_gen_backend = self.generation_backend(OpenRouter::default_video_gen_model);
        toolbox.space_files_dir = self.space.files_dir(&self.active_space.name);
        toolbox.memory = (!self.incognito && self.approvals.eval_approver.is_none()).then(|| {
            crate::tools::MemoryCtx {
                db_path: self.space.db_path(),
                space_id: self.active_space.id.clone(),
            }
        });
        toolbox.space_apps_dir = self.space.apps_dir(&self.active_space.name);
        toolbox.space_scripts_dir = self.space.scripts_dir(&self.active_space.name);
        toolbox.supports_images = self.current_model_supports_images();
        // Evaluation workspaces must not access outside services.
        toolbox.connectors = self
            .approvals
            .eval_approver
            .is_none()
            .then(|| self.connectors.clone());
        toolbox.record_events = !self.incognito;
        self.configure_auto_policy(&mut toolbox);
        toolbox.remote_images = Some(Arc::clone(&self.image_fetcher));
        toolbox.interactive_ui = self.interactive_ui;
        toolbox.deck_link = Arc::clone(&self.deck_link);
        self.approvals.pending = None;
        self.approvals.pending_request = None;
        self.approvals.queue.clear();
        self.approvals.rx = None;
        self.approvals.panel_approver = None;
        if let Some(approver) = &self.approvals.job_approver {
            toolbox.approver = Some(approver.clone());
        } else if let Some(approver) = &self.approvals.eval_approver {
            toolbox.approver = Some(approver.clone());
        } else if self.interactive_approvals {
            let (sender, receiver) = mpsc::unbounded_channel();
            let approver: Arc<dyn Approver> = Arc::new(ChannelApprover::new(sender));
            toolbox.approver = Some(Arc::clone(&approver));
            self.approvals.panel_approver = Some(approver);
            self.approvals.rx = Some(receiver);
        }
        toolbox.workspace = self
            .workspace
            .as_ref()
            .filter(|_| self.work_mode.is_agent())
            .cloned();
        if self.approvals.eval_approver.is_some() || self.approvals.job_approver.is_some() {
            toolbox.confine_shell_writes =
                toolbox.workspace.as_ref().map(|w| w.root().to_path_buf());
        }
        toolbox.subagent_environment = self.environment_section();
        toolbox.procs = Arc::clone(&self.procs);
        toolbox.session_id = self
            .session
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_default();
        self.toolbox = if let Some(approver) = &self.approvals.job_approver {
            Arc::new(crate::jobs::JobExecutor {
                inner: Arc::new(toolbox),
                approver: Arc::clone(approver),
            })
        } else {
            Arc::new(toolbox)
        };
    }

    pub fn is_research_session(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.kind == "research")
    }

    pub fn blocked_domains(&self) -> Vec<String> {
        std::fs::read_to_string(self.space.blocked_domains_path(&self.active_space.name))
            .unwrap_or_default()
            .split(',')
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect()
    }

    pub fn init(&mut self) {
        if self.backends.any() {
            self.fetch_models();
        }
        self.rescan_files();
    }

    pub fn spawn_update_check(&mut self) {
        if !self.db.update_check_due() {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.update_rx = Some(rx);
        tokio::spawn(async move {
            let latest = crate::update::latest_version().await;
            let _ = tx.send(latest);
        });
    }

    pub fn refresh_codex_in_background(&mut self) {
        if self.codex_refresh_rx.is_some() {
            return;
        }
        let Some(creds) = self.saved.codex.as_ref() else {
            return;
        };
        if Utc::now().timestamp_millis() < creds.expires - 60_000 {
            return;
        }
        let creds = creds.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.codex_refresh_rx = Some(rx);
        tokio::spawn(async move {
            let result = config::refresh_codex_if_needed(creds)
                .await
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
    }

    pub fn on_update_check(&mut self, latest: Option<String>) {
        let Some(latest) = latest else {
            return;
        };
        if !crate::update::version_gt(&latest, crate::update::CURRENT) {
            return;
        }
        self.push_status(format!(
            "nexus v{latest} is out (you have {}) — run `nexus update`",
            crate::update::CURRENT
        ));
        self.notifications.push_back(ChatNotification {
            session_id: String::new(),
            title: "update available".to_string(),
            text: format!("v{latest} is out — run `nexus update`"),
            success: true,
        });
    }

    pub fn is_streaming(&self) -> bool {
        !self.chat_tasks.is_empty()
    }

    pub fn chat_task_count(&self) -> usize {
        self.chat_tasks.len()
    }

    pub fn chat_task_for_session(&self, session_id: &str) -> Option<&ChatTask> {
        self.chat_tasks
            .values()
            .find(|task| task.session_id == session_id)
    }

    pub fn active_chat_task(&self) -> Option<&ChatTask> {
        self.session
            .as_ref()
            .and_then(|session| self.chat_task_for_session(&session.id))
    }

    pub fn active_streaming_text(&self) -> Option<&str> {
        self.active_chat_task().map(|task| task.buffer.as_str())
    }

    pub fn viewing_stream(&self) -> bool {
        self.active_chat_task().is_some()
    }

    pub fn is_welcome(&self) -> bool {
        self.messages.is_empty() && !self.viewing_stream()
    }

    pub const fn tick_spinner(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    pub const fn spinner_char(&self) -> &'static str {
        SPINNER[self.spinner_frame % SPINNER.len()]
    }

    pub fn spinner_color(&self) -> SpinnerColor {
        self.active_chat_task()
            .map_or(self.spinner_color, |task| task.spinner_color)
    }

    pub fn thinking_phrase(&self) -> &'static str {
        self.active_chat_task()
            .map_or(THINKING[self.thinking_idx].0, |task| {
                THINKING[task.thinking_idx].0
            })
    }

    pub fn thinking_text(&self) -> Option<&str> {
        self.active_chat_task()
            .and_then(|task| (!task.thinking.is_empty()).then_some(task.thinking.as_str()))
    }

    pub fn push_status(&mut self, s: impl Into<String>) {
        self.pending_events.push_back(AppEvent::Status(s.into()));
    }

    pub fn push_composer_set(&mut self, text: impl Into<String>) {
        self.pending_events
            .push_back(AppEvent::ComposerSet(text.into()));
    }

    pub fn push_composer_clear(&mut self) {
        self.pending_events.push_back(AppEvent::ComposerClear);
    }

    pub fn push_viewport_reset(&mut self) {
        if self
            .user_command
            .context
            .as_ref()
            .is_some_and(|context| *context != self.user_command_context())
        {
            self.reset_user_command_dir();
        }
        self.pending_events.push_back(AppEvent::ViewportReset);
    }

    pub fn push_history_invalidated(&mut self) {
        self.pending_events.push_back(AppEvent::HistoryInvalidated);
    }

    /// Drain immediate frontend feedback before drawing or waiting on background channels.
    pub fn pop_pending_event(&mut self) -> Option<AppEvent> {
        self.pending_events.pop_front()
    }

    #[doc(hidden)]
    pub fn drain_ui_events(&mut self) -> (Vec<String>, String) {
        let mut sets = Vec::new();
        let mut last = String::new();
        while let Some(ev) = self.pop_pending_event() {
            match ev {
                AppEvent::ComposerSet(s) => sets.push(s),
                AppEvent::Status(s) => last = s,
                _ => {}
            }
        }
        (sets, last)
    }

    #[doc(hidden)]
    pub fn last_status(&mut self) -> String {
        self.drain_ui_events().1
    }

    #[doc(hidden)]
    pub fn drain_composer_sets(&mut self) -> Vec<String> {
        self.drain_ui_events().0
    }

    #[must_use]
    pub fn panel_scrubber(&self) -> crate::trace::Scrubber {
        crate::trace::Scrubber::new(self.saved_secrets())
    }

    pub fn record_panel_call(
        &mut self,
        session_id: &str,
        incognito: bool,
        panel: &str,
        call: &crate::interactive::live::Call,
        outcome: &crate::interactive::live::Outcome,
    ) -> Result<()> {
        let scrubber = self.panel_scrubber();
        let content = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "panel": panel,
            "name": scrubber.scrub(&call.tool),
            "arguments": crate::interactive::live::redacted_arguments(&call.arguments, &scrubber),
            "result": scrubber.scrub(&outcome.text),
        })
        .to_string();
        if !incognito {
            self.db.add_tool_call_message(session_id, &content)?;
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.id == session_id)
        {
            self.messages.push(Message {
                role: "tool_call".into(),
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
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub async fn next_event(&mut self) -> AppEvent {
        if let Some(ev) = self.pending_events.pop_front() {
            return ev;
        }
        tokio::select! {
            () = tokio::time::sleep_until(self.index.next_chat_index.into()), if self.index.embed_rx.is_none() => {
                self.start_embedding();
                AppEvent::Status(self.index.embedding_status.clone())
            },
            request = async {
                match self.approvals.rx.as_mut() {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some((request, reply)) = request {
                    if reply.is_closed() {
                        return AppEvent::Status("approval cancelled".to_string());
                    }
                    let status = format!("approval needed · {} {}", request.tool, request.action);
                    self.approvals.queue.push_back((request, reply));
                    self.show_next_approval();
                    AppEvent::Status(status)
                } else {
                    self.approvals.rx = None;
                    AppEvent::Status(String::new())
                }
            },
            () = async {
                match self.approvals.pending.as_mut() {
                    Some(reply) => reply.closed().await,
                    None => std::future::pending().await,
                }
            } => {
                self.approvals.pending = None;
                self.approvals.pending_request = None;
                self.show_next_approval();
                AppEvent::Status("approval cancelled".to_string())
            },
            Some(status) = self.decider_status_rx.recv() => AppEvent::Status(status),
            Some(note) = self.connector_notes_rx.recv() => AppEvent::Status(note),
            Some(repair) = self.ui_repair.rx.recv() => self.on_ui_repair_result(repair),
            ev = self.chat_event_rx.recv() => {
                AppEvent::Stream(ev.map(|event| (event.task_id, event.event)))
            },
            run = async {
                match self.user_command.rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                self.user_command.rx = None;
                if let Some(run) = run
                    && let Err(error) = self.finish_user_command(&run)
                {
                    self.push_status(format!("could not record the command: {error}"));
                }
                AppEvent::HistoryInvalidated
            },
            res = async {
                match self.models_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Models(res),
            t = async {
                match self.title_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Title(t),
            c = async {
                match self.compaction.rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Compact(c),
            r = async {
                match self.skills_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::SkillInstall(r),
            r = async {
                match self.index.convert_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Convert(r),
            r = async {
                match self.index.embed_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Embed(r),
            r = async {
                match self.local.status_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::LocalServers(r),
            r = async {
                match self.research.rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(event) = &r {
                    crate::jobs::research::event(self, event).await;
                }
                AppEvent::Research(r)
            },
            r = async {
                match self.research.topic_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::ResearchTopic(r),
            r = async {
                match self.login_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Login(r),
            r = async {
                match self.update_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::UpdateCheck(r.flatten()),
            r = async {
                match self.codex_refresh_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                self.codex_refresh_rx = None;
                if let Some(result) = r {
                    match result.and_then(|creds| {
                        config::save_codex_credentials(&creds).map_err(|error| error.to_string())?;
                        Ok(creds)
                    }) {
                        Ok(creds) => {
                            self.saved.codex = Some(creds);
                            self.rebuild_all_backends();
                        }
                        Err(error) => self.push_status(format!(
                            "Codex sign-in could not refresh ({error}) — run /login and pick Codex"
                        )),
                    }
                }
                self.pending_events.pop_front().unwrap_or_else(|| AppEvent::Status(String::new()))
            },
            r = async {
                match self.swarm.rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Swarm(r),
        }
    }

    pub fn on_models_result(&mut self, result: Option<ModelsResult>) {
        self.models_rx = None;
        let result = match result {
            Some(r) => r,
            None => Err("model fetch cancelled".to_string()),
        };
        match result {
            Ok(models) => {
                let n = models.len();
                // Batch pricing writes into one transaction to avoid stalling the UI task.
                let prices: Vec<_> = models
                    .iter()
                    .filter_map(|m| {
                        m.pricing
                            .map(|price| (m.id.clone(), m.backend.name().to_string(), price))
                    })
                    .collect();
                let _ = self.db.upsert_model_prices(&prices);
                let _ = self.db.backfill_usage_costs();
                self.models = models;
                self.push_status(format!("loaded {n} models"));
                // Catalog arrival may complete the prerequisites for a pending compaction.
                self.maybe_compact();
            }
            Err(e) => self.push_status(format!("model fetch failed: {e}")),
        }
    }
}

impl App {
    pub fn cancel_chat_tasks(&mut self) {
        for task in self.chat_tasks.values() {
            task.abort.abort();
        }
        self.chat_tasks.clear();
        self.approvals.pending = None;
        self.approvals.pending_request = None;
        self.approvals.queue.clear();
    }

    pub fn show_next_approval(&mut self) -> bool {
        if self.approvals.pending.is_some() {
            return false;
        }
        while let Some((request, reply)) = self.approvals.queue.pop_front() {
            if reply.is_closed() {
                continue;
            }
            self.approvals.pending = Some(reply);
            self.approvals.pending_request = Some(request);
            self.approvals.shown += 1;
            return true;
        }
        false
    }

    #[must_use]
    pub fn approval_count(&self) -> usize {
        usize::from(self.approvals.pending.is_some())
            + self
                .approvals
                .queue
                .iter()
                .filter(|(_, reply)| !reply.is_closed())
                .count()
    }

    pub(crate) fn answer_approval(&mut self, decision: Decision) {
        let request = self.approvals.pending_request.take();
        let connector = request.as_ref().is_some_and(|request| {
            request.class.as_deref() == Some(crate::connectors::exec::CLASS)
        });
        // Only connector requests may persist an "Always" policy.
        let decision = match decision {
            Decision::AllowAlways if !connector => Decision::Allow,
            Decision::AllowForSession
                if request
                    .as_ref()
                    .is_some_and(|request| !request.allow_session) =>
            {
                Decision::Allow
            }
            other => other,
        };
        if let Some(sender) = self.approvals.pending.take() {
            let _ = sender.send(decision);
        }
        if decision == Decision::AllowForSession
            && let Some(request) = request.filter(|request| request.tool != "wave")
            && !connector
        {
            if let Some(receiver) = self.approvals.rx.as_mut() {
                while let Ok(item) = receiver.try_recv() {
                    self.approvals.queue.push_back(item);
                }
            }
            for (queued, reply) in std::mem::take(&mut self.approvals.queue) {
                if queued.tool == request.tool
                    && queued.action == request.action
                    && queued.allow_session
                    && !queued.summary.starts_with("could not parse safely:")
                {
                    let _ = reply.send(Decision::Allow);
                } else {
                    self.approvals.queue.push_back((queued, reply));
                }
            }
        }
        self.show_next_approval();
    }
}

// notify-send cannot route clicks back without a long-lived D-Bus action listener.
pub fn send_system_notification(title: &str, body: &str) {
    #[cfg(all(target_os = "linux", not(test)))]
    {
        let _ = std::process::Command::new("notify-send")
            .args([
                "--app-name=nexus-chat",
                "--urgency=normal",
                "--expire-time=5000",
                "--hint=int:transient:1",
                title,
                body,
            ])
            .spawn();
    }
    #[cfg(any(not(target_os = "linux"), test))]
    let _ = (title, body);
}

#[allow(clippy::too_many_lines)]
pub fn tool_call_summary(name: &str, args: &str, result: &str) -> String {
    if let Some(view) = tool_view::parse(name, args, result) {
        return view.summary();
    }
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let f = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let f_or = |first: &str, second: &str| {
        let value = f(first);
        if value.is_empty() { f(second) } else { value }
    };
    match name {
        "skills" => {
            let target = if f("action") == "install" {
                f("source")
            } else {
                f("name")
            };
            format!("skills/{} {}", f("action"), target)
        }
        "scripts" => {
            let target = match f("action").as_str() {
                "install" => {
                    let n = v
                        .get("packages")
                        .and_then(|a| a.as_array())
                        .map_or(0, Vec::len);
                    format!("{n} packages")
                }
                "python" => f("name"),
                _ => f("path"),
            };
            format!("scripts/{} {}", f("action"), target)
        }
        "app" => format!("app/{} {}", f("action"), f("app")),
        "media" => {
            let target = [f("video_id"), f("name"), f("image_id")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_else(|| f("prompt").chars().take(30).collect());
            format!("media/{} {}", f("action"), target)
        }
        "skill" => format!("skill {}", f("name")),
        "skill_admin" => format!("skill_admin {} → {}", f("action"), first_line(result)),
        "search" => {
            let failed = result.starts_with("no results") || result.contains("failed");
            let hits = if failed { "no hits" } else { "hits" };
            format!("search/{} \"{}\" → {hits}", f("mode"), f("query"))
        }
        "research_lookup" => format!("research_lookup/{} \"{}\"", f("scope"), f("query")),
        "batch" => {
            let calls = v.get("calls").and_then(|c| c.as_array());
            let n = calls.map_or(0, Vec::len);
            let tools: Vec<&str> = calls
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| item.get("tool").and_then(|t| t.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            format!("batch [{n} ops: {}]", tools.join(", "))
        }
        "fetch_url" => format!("fetch_url {} → {}", f("url"), first_line(result)),
        "files" => format!(
            "files/{} {} → {}",
            f("action"),
            f_or("name", "query"),
            first_line(result)
        ),
        "app_inspect" => format!(
            "app_inspect/{} {}/{}",
            f("action"),
            f("app"),
            f_or("path", "pattern")
        ),
        "app_modify" => format!("app_modify/{} {}/{}", f("action"), f("app"), f("path")),
        "app_assets" => format!("app_assets/{} {}", f("action"), f("app")),
        "script_files" => format!("script_files/{} {}", f("action"), f("path")),
        "video_transform" => format!("video_transform/{} {}", f("action"), f("video_id")),
        "video_references" => format!("video_references/{} {}", f("action"), f("name")),
        "install_skill" => format!("install_skill {} → {}", f("source"), first_line(result)),
        "create_skill" => format!("create_skill {} → {}", f("name"), first_line(result)),
        "run_script" => {
            let path = f_or("path", "script");
            if v.get("space")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                format!("run_script space/{path}")
            } else {
                format!("run_script {}/{}", f("skill"), path)
            }
        }
        "run_python" => format!("run_python ({} lines)", f("code").lines().count().max(1)),
        "grep_app" => {
            let hits = if result.starts_with("no matches") {
                "no hits".to_string()
            } else {
                format!(
                    "{} files",
                    result.lines().filter(|line| !line.starts_with('…')).count()
                )
            };
            format!("grep_app {} \"{}\" → {hits}", f("app"), f("pattern"))
        }
        "install_packages" => {
            let pkgs = v
                .get("packages")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let target = [f("skill"), f("app")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_default();
            format!("install_packages {pkgs} → {target}")
        }
        "web_search" | "search_files" => {
            let failed = result.starts_with("no results")
                || result.starts_with("no matches")
                || result.contains("failed");
            let hits = if failed {
                "no hits".to_string()
            } else {
                format!("{} hits", result.lines().count())
            };
            format!("{name} \"{}\" → {hits}", f("query"))
        }
        "read_file" => format!("read_file {} → {}", f("name"), first_line(result)),
        "read_app_file" => format!("read_app_file {}/{}", f("app"), f("path")),
        "diff_app" => format!("diff_app {}/{}", f("app"), f("path")),
        "write_file" => {
            format!(
                "write_file {}/{} ({} bytes)",
                f("app"),
                f("path"),
                f("content").len()
            )
        }
        "edit_file" => format!("edit_file {}/{}", f("app"), f("path")),
        "generate_video" => format!("generate_video \"{}\"", f("prompt")),
        "edit_video" => format!("edit_video {} {}", f("video_id"), f("lighting")),
        "extract_frame" => format!("extract_frame {} @ {:.1}s", f("video_id"), f("time_sec")),
        "stitch_videos" => format!("stitch_videos {}", f("video_ids")),
        "save_reference" => format!("save_reference {}", f("name")),
        "list_references" => "list_references".to_string(),
        "delete_reference" => format!("delete_reference {}", f("name")),
        "generate_image" => format!("generate_image \"{}\"", f("prompt")),
        _ => {
            let mut a: String = args.chars().take(60).collect();
            if args.chars().count() > 60 {
                a.push('…');
            }
            format!("{name} {a}")
        }
    }
}

fn first_line(result: &str) -> String {
    result
        .lines()
        .next()
        .unwrap_or("")
        .trim_end_matches(':')
        .to_string()
}

#[cfg(test)]
mod model_decider_tests {
    use super::*;
    use crate::provider::openrouter::decider_mock::server;
    use std::time::Duration;

    #[tokio::test]
    async fn auto_uses_jev_in_incognito_without_logging_and_passes_recent_calls() {
        let dir = std::env::temp_dir().join(format!("nexus-decider-{}", uuid::Uuid::new_v4()));
        let root = dir.join("workspace");
        let data = dir.join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let body = serde_json::json!({"answers": {
            "c0_scope": {"noul": 0.99},
            "c0_risk": {"score": 1.0, "confidence": 0.95}
        }});
        let (provider, handle) = server(200, body.to_string(), Duration::ZERO, 1).await;
        let space = Space { root: data };
        let mut app = App::new(Db::open(&space.db_path()).unwrap(), None, space);
        app.saved
            .set_key(BackendTag::OpenRouter, "mock-decider-key".into());
        app.backends.set(BackendTag::OpenRouter, provider);
        app.work_mode = WorkMode::AgentAuto;
        app.incognito = true;
        app.workspace = Some(Arc::new(
            crate::workspace::Workspace::open(&root, Vec::new()).unwrap(),
        ));
        app.refresh_toolbox();
        *app.decision_context.lock().unwrap() = ("private-session".into(), "create new.rs".into());
        app.toolbox.run("fs", r#"{"action":"list"}"#).await;
        let (result, _) = app
            .toolbox
            .run("fs", r#"{"action":"write","path":"new.rs","content":"ok"}"#)
            .await;
        assert!(result.starts_with("wrote new.rs"), "{result}");
        assert_eq!(std::fs::read_to_string(root.join("new.rs")).unwrap(), "ok");
        assert!(app.db.decisions("private-session").unwrap().is_empty());
        let requests = handle.await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1["state"]["task"], "create new.rs");
        assert_eq!(requests[0].1["session_id"], "private-session");
        assert!(
            requests[0].1["state"]["recent_steps"][0]
                .as_str()
                .unwrap()
                .contains("list")
        );
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn llm_wiring_follows_session_model_changes() {
        let body = serde_json::json!({"choices": [{"message": {"content":
            r#"{"calls":[{"id":"c0","in_scope":0.99,"risk":1}]}"#
        }}]});
        let (provider, handle) = server(200, body.to_string(), Duration::ZERO, 3).await;
        let mut app = tests::app_with_key();
        let dir = app.space.root.clone();
        let root = dir.join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        app.saved.keys.remove(&BackendTag::OpenRouter);
        app.backends = Backends::default();
        app.backends.set(BackendTag::Local, provider);
        app.models[0].id = "utility".into();
        app.models[1].id = "session".into();
        for model in &mut app.models {
            model.backend = BackendTag::Local;
        }
        app.current_model = Some("local:session".into());
        app.work_mode = WorkMode::AgentAuto;
        app.incognito = true;
        app.workspace = Some(Arc::new(
            crate::workspace::Workspace::open(&root, Vec::new()).unwrap(),
        ));
        app.refresh_toolbox();
        app.model_pick_target = ModelPickTarget::Session;
        app.pick_model("local:utility").unwrap();
        for path in ["one", "two", "three"] {
            let args = serde_json::json!({"action": "write", "path": path, "content": "ok"});
            let (result, _) = app.toolbox.run("fs", &args.to_string()).await;
            assert!(result.starts_with("wrote "), "{result}");
            if path == "one" {
                app.pick_model("local:session").unwrap();
            } else if path == "two" {
                app.pick_model("local:utility").unwrap();
            }
        }
        let requests = handle.await.unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].1["model"], "utility");
        assert_eq!(requests[1].1["model"], "session");
        assert_eq!(requests[2].1["model"], "utility");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod eval_tests {
    use super::*;

    #[tokio::test]
    async fn eval_permissions_are_confined_to_fs_and_allowlisted_shell() {
        let approver = EvalApprover {
            allow_shell: vec!["cargo test".into(), "ls".into()],
        };
        for action in ["read", "write", "edit", "delete"] {
            assert_eq!(
                approver
                    .request(ApprovalRequest {
                        tool: "fs".into(),
                        class: None,
                        detail: None,
                        action: action.into(),
                        summary: "file".into(),
                        allow_session: true,
                    })
                    .await,
                Decision::Allow
            );
        }
        for (summary, allowed) in [
            ("$ cargo test", true),
            ("$ cargo test --offline", true),
            ("$ ls\nin src", true),
            ("$ rm file", false),
            ("could not parse safely: bad\n$ cargo test", false),
            ("$ ls\nrm file", false),
            ("$ ls 'quoted\nin name; bad'", false),
            ("$ ls 'quoted\nin name'\nin src", false),
        ] {
            assert_eq!(
                approver
                    .request(ApprovalRequest {
                        tool: "shell".into(),
                        class: None,
                        detail: None,
                        action: "run".into(),
                        summary: summary.into(),
                        allow_session: true,
                    })
                    .await,
                if allowed {
                    Decision::Allow
                } else {
                    Decision::Deny
                },
                "{summary}"
            );
        }
        for forbidden in [';', '&', '|', '$', '`', '>', '<'] {
            let request = ApprovalRequest {
                tool: "shell".into(),
                class: None,
                detail: None,
                action: "run".into(),
                summary: format!("$ cargo test {forbidden} echo bad"),
                allow_session: true,
            };
            assert_eq!(approver.request(request).await, Decision::Deny);
        }
        for (tool, action) in [("shell", "log"), ("request_workspace", "open")] {
            assert_eq!(
                approver
                    .request(ApprovalRequest {
                        tool: tool.into(),
                        class: None,
                        detail: None,
                        action: action.into(),
                        summary: "$ ls".into(),
                        allow_session: true,
                    })
                    .await,
                Decision::Deny
            );
        }
    }

    #[tokio::test]
    async fn eval_workspace_is_incognito_and_has_no_binding_or_durable_checkpoints() {
        let dir = std::env::temp_dir().join(format!("nexus-eval-app-{}", uuid::Uuid::new_v4()));
        let data = dir.join("data");
        let root = dir.join("copy/workspace");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(
            Db::open_in_memory().unwrap(),
            None,
            Space { root: data.clone() },
        );
        app.execute(AppCommand::Incognito { on: true }).unwrap();
        app.approvals.eval_approver = Some(Arc::new(EvalApprover {
            allow_shell: vec!["printf ok".into()],
        }));
        let session = app
            .db
            .create_session("test", "model", &app.active_space.id, "chat")
            .unwrap();
        let session_id = session.id.clone();
        app.session = Some(session);
        app.enter_eval_workspace(&root).unwrap();
        assert!(app.incognito);
        assert_eq!(app.work_mode, WorkMode::Agent);
        assert_eq!(
            app.workspace.as_ref().unwrap().root(),
            root.canonicalize().unwrap()
        );
        assert_eq!(app.db.session_workspace(&session_id), None);
        app.refresh_toolbox();
        app.workspace.as_ref().unwrap().begin_turn(&session_id);
        let (result, _) = app
            .toolbox
            .run("fs", r#"{"action":"write","path":"new","content":"yes"}"#)
            .await;
        assert!(root.join("new").exists(), "{result}");
        let (result, _) = app
            .toolbox
            .run("shell", r#"{"action":"run","cmd":"printf ok"}"#)
            .await;
        #[cfg(target_os = "macos")]
        {
            assert!(result.starts_with("exit 0"), "{result}");
            assert!(root.parent().unwrap().join("runs").exists());
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            result,
            "shell write confinement unavailable · this eval task needs macOS sandbox-exec"
        );
        assert!(!data.join("runs").exists());
        assert!(!data.join("checkpoints").exists());
        let rows: i64 = app
            .db
            .conn_for_test()
            .query_row("SELECT count(*) FROM cache.session_workspaces", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0);
        assert!(app.enter_eval_workspace(&data).is_err());
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn boot_backend_wiring_carries_loaded_experiments() {
        let mut app = tests::app_with_key();
        app.saved.experiments.cache_ttl_1h = true;
        app.rebuild_all_backends();
        assert!(app.experiments.cache_ttl_1h);
    }
}
