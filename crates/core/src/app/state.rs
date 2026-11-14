use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use super::{CacheTally, ContextBasis, EmbedMsg, ImageMeta, ResearchMsg, ScriptMeta, SurveyGate};
use super::{chat, chats, files, swarm};
use crate::tools::{ApprovalRequest, Approver, Decision};

#[derive(Default)]
pub struct ResearchState {
    pub survey_gate: Option<SurveyGate>,
    pub survey_reply_tx: Option<mpsc::UnboundedSender<String>>,
    /// Queued `/steer` positions and text, dropped on acknowledgement or when the job stops.
    pub steer_log: Vec<(usize, String)>,
    /// Steer positions drained and persisted by the pipeline, parsed from `Stage` updates.
    pub steer_acked: HashSet<usize>,
    /// Running job's stage rows, kept in sync by `mirror_stage` regardless of the viewed session.
    pub stage_rows: Vec<String>,
    /// Incognito mode at job start, governing artifact persistence even if the mode changes.
    pub incognito: bool,
    pub steer_tx: Option<mpsc::UnboundedSender<String>>,
    pub topic_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,
    /// Composer buffer for the live research-activity view's steer input.
    pub live_input: String,
    pub rx: Option<mpsc::UnboundedReceiver<ResearchMsg>>,
    pub abort: Option<tokio::task::AbortHandle>,
    /// (session id, topic) of the running `/research` job, cleared when its channel closes.
    pub running: Option<(String, String)>,
}

pub struct IndexState {
    /// Automatically chosen embedding model: local `EmbeddingGemma` 2, backend default, or empty.
    pub embedding_model: String,
    /// Probed runtime shared with file search; never contains cloud credentials in its identity.
    pub embedding_backend: Option<crate::provider::embedding::Embedder>,
    pub embedding_status: String,
    pub(super) embedding_chunks: Vec<(String, String)>,
    pub(super) embedding_chat_job: Option<chats::ChatEmbedJob>,
    pub(super) next_chat_index: Instant,
    pub chat_index_status: String,
    pub(super) embedding_discovery:
        Option<Arc<std::sync::OnceLock<(String, crate::provider::embedding::Embedder)>>>,
    /// One in-flight file/chat embedding job, or embedder capability probe.
    pub embed_rx: Option<mpsc::UnboundedReceiver<EmbedMsg>>,
    /// Background converter updates: (`space_id`, file name, progress or final result).
    pub convert_rx: Option<mpsc::UnboundedReceiver<(String, String, files::ConvertUpdate)>>,
}

impl IndexState {
    pub(super) fn new() -> Self {
        Self {
            embedding_model: String::new(),
            embedding_backend: None,
            embedding_status: String::new(),
            embedding_chunks: Vec::new(),
            embedding_chat_job: None,
            next_chat_index: Instant::now() + Duration::from_secs(5),
            chat_index_status: String::new(),
            embedding_discovery: None,
            embed_rx: None,
            convert_rx: None,
        }
    }
}

#[derive(Default)]
pub struct ApprovalState {
    /// Shared interactive channel for panel mutations, independent of work mode.
    pub panel_approver: Option<Arc<dyn Approver>>,
    /// Eval permissions survive toolbox refreshes without interactive prompts.
    pub eval_approver: Option<Arc<dyn Approver>>,
    /// Unattended job gate, retained across toolbox refreshes.
    pub job_approver: Option<Arc<crate::jobs::JobApprover>>,
    /// Reply channel for the currently displayed approval request.
    pub pending: Option<oneshot::Sender<Decision>>,
    pub pending_request: Option<ApprovalRequest>,
    /// Concurrent approval requests in arrival order.
    pub queue: VecDeque<(ApprovalRequest, oneshot::Sender<Decision>)>,
    /// Counts displayed requests so a view can distinguish consecutive identical requests.
    pub shown: u64,
    /// Requests from the interactive toolbox (agent edits/commands or chat `request_workspace`).
    pub rx: Option<mpsc::UnboundedReceiver<(ApprovalRequest, oneshot::Sender<Decision>)>>,
    /// A `propose_command` (`!<cmd>`) the view moves into an empty composer.
    pub proposed_command: Option<String>,
}

#[derive(Default)]
pub struct UserCommandState {
    pub(super) rx: Option<mpsc::UnboundedReceiver<chat::UserCommandRun>>,
    /// The user's terminal directory; never used by agent tools.
    pub dir: Option<std::path::PathBuf>,
    /// Lazy shell setup, shared by command execution and composer completion.
    pub shell: Arc<tokio::sync::OnceCell<crate::usershell::UserShell>>,
    pub(super) shell_warning_shown: bool,
    pub(super) context: Option<chat::UserCommandContext>,
    pub(super) epoch: u64,
}

pub struct UiRepairState {
    /// Finished UI repairs share a channel so switching sessions cannot lose a result.
    pub(super) tx: mpsc::UnboundedSender<chat::UiRepairResult>,
    pub(super) rx: mpsc::UnboundedReceiver<chat::UiRepairResult>,
    /// Session -> reply message id; a new turn disarms that session's repair.
    pub(super) messages: HashMap<String, String>,
    pub(super) feedback_seen: HashSet<String>,
}

impl UiRepairState {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx,
            messages: HashMap::new(),
            feedback_seen: HashSet::new(),
        }
    }
}

#[derive(Default)]
pub struct CompactionState {
    /// Background compaction result: (session id, digest, messages-covered, pre-compaction %).
    pub rx: Option<mpsc::UnboundedReceiver<(String, String, i64, u64)>>,
    /// Session being compacted, kept separately from `rx` so frontends can mark its row.
    pub session_id: Option<String>,
    /// Last compaction failure, filled before the result channel drops so status names the cause.
    pub(crate) error: Arc<Mutex<Option<String>>>,
}

#[derive(Default)]
pub struct SwarmState {
    pub rx: Option<mpsc::UnboundedReceiver<swarm::SwarmMsg>>,
    pub abort: Option<tokio::task::AbortHandle>,
    pub session: Option<String>,
    /// Active session's roster, cached for the popup and kept in sync by `on_swarm_update`.
    pub cache: Vec<crate::db::Persona>,
    /// Whether the running turn's session is incognito, so its agents stay unsaved.
    pub incognito: bool,
    /// What the running turn's agents have cost so far; it lands on the reply.
    pub cost: Option<f64>,
}

#[derive(Default)]
pub struct ListCaches {
    pub files: Vec<crate::db::FileRow>,
    pub apps: Vec<String>,
    pub images: Vec<ImageMeta>,
    pub scripts: Vec<ScriptMeta>,
    pub sessions: Vec<crate::db::Session>,
}

#[derive(Default)]
pub struct LocalRuntimeState {
    /// Servers Nexus started with `/local start`, owned so stop/exit can reclaim their memory.
    pub servers: crate::provider::serve::ManagedServers,
    /// Last local-server survey, cached for the `/local` picker and shared by all frontends.
    pub status: Vec<crate::provider::serve::RuntimeStatus>,
    pub status_rx: Option<mpsc::UnboundedReceiver<Vec<crate::provider::serve::RuntimeStatus>>>,
    /// The user requested the in-flight survey, so its result should also report a status line.
    pub status_announce: bool,
}

#[derive(Default)]
pub struct ContextMeter {
    /// Exact conversation token total from the last completed response.
    pub total: Option<u64>,
    /// Latest exact count and conversation basis, used to estimate only additions since then.
    pub(crate) basis: Option<ContextBasis>,
    /// Learned exact/estimated token ratio, scaling estimates after session or epoch switches.
    pub(crate) ratio: Option<f64>,
    /// In-progress turn's cache accounting across all API requests; per-request data is in `usage_log`.
    pub turn_cache: CacheTally,
    /// Most recent completed request's cache hit rate (0..=1), shown in the context popup.
    pub last_cache_rate: Option<f64>,
    /// Whether the local prompt-budget warning has fired in this cache epoch.
    pub(crate) warned: bool,
}

pub struct SearchConfig {
    /// Empty disables `SearXNG`.
    pub searxng_url: String,
    /// Empty disables `LangSearch`.
    pub langsearch_key: String,
    /// One of `auto`, `langsearch`, `searxng`, or `duckduckgo`.
    pub provider: String,
}

impl SearchConfig {
    pub(super) fn new() -> Self {
        Self {
            searxng_url: String::new(),
            langsearch_key: String::new(),
            provider: "auto".to_string(),
        }
    }
}
