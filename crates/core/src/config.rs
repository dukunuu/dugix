use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::provider::BackendTag;

/// Never passed to children that run model-authored code.
pub const SECRET_ENV_VARS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "OPENAI_API_KEY",
    "OPENCODE_API_KEY",
    "MISTRAL_API_KEY",
    "CF_API_TOKEN",
    "REDDIT_CLIENT_ID",
    "REDDIT_CLIENT_SECRET",
];

pub const BASE_SYSTEM_PROMPT: &str = include_str!("../assets/prompts/base.md");

pub fn scrub_secret_env(cmd: &mut std::process::Command) {
    for key in SECRET_ENV_VARS {
        cmd.env_remove(key);
    }
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    #[serde(default)]
    provider: Provider,
    #[serde(default)]
    host: Option<HostSettings>,
    #[serde(default)]
    local: Option<crate::provider::local::LocalConfig>,
    #[serde(default)]
    experiments: Experiments,
    #[serde(default)]
    agent: AgentConfig,
    /// Raw; `connectors::config` parses it so a malformed connector can't stop credentials loading.
    #[serde(default)]
    #[allow(dead_code)] // parsed so the key is known; read through `read_table_at`
    connectors: Option<toml::Value>,
    #[serde(default)]
    #[allow(dead_code)] // parsed so the key is known
    extract: Option<toml::Value>,
}

/// Machine-local rules and model thresholds for agent-auto policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub allow: Vec<String>,
    pub nexus_repo: Option<String>,
    pub decider: Option<ModelDecider>,
    pub decider_model: String,
    pub scope_threshold: f64,
    pub risk_threshold: f64,
    pub exec_risk_threshold: f64,
}

/// Model policy following the deterministic rules.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModelDecider {
    Jev,
    Llm,
    Rules,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            nexus_repo: None,
            decider: None,
            decider_model: "typesafe/jev-1.14".into(),
            scope_threshold: 0.90,
            risk_threshold: 1.5,
            exec_risk_threshold: 2.2,
        }
    }
}

impl AgentConfig {
    pub(crate) fn selected_decider(&self, has_openrouter_key: bool) -> ModelDecider {
        self.decider.unwrap_or(if has_openrouter_key {
            ModelDecider::Jev
        } else {
            ModelDecider::Llm
        })
    }
}

/// Opt-in context experiments; defaults preserve the current behavior.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Experiments {
    pub mask_tool_results: bool,
    pub mask_keep_rounds: u32,
    pub mask_trigger_tokens: u64,
    pub compaction_tail_turns: u32,
    pub compaction_itemized: bool,
    pub compaction_budget_tokens: u64,
    pub cache_ttl_1h: bool,
}

impl Default for Experiments {
    fn default() -> Self {
        Self {
            mask_tool_results: false,
            mask_keep_rounds: 10,
            mask_trigger_tokens: 0,
            compaction_tail_turns: 0,
            compaction_itemized: false,
            compaction_budget_tokens: 0,
            cache_ttl_1h: false,
        }
    }
}

impl Experiments {
    /// Set one experiment by its unqualified field name.
    ///
    /// # Errors
    /// Rejects unknown keys and values that do not parse as the field's type.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        const KEYS: &str = "mask_tool_results, mask_keep_rounds, mask_trigger_tokens, compaction_tail_turns, compaction_itemized, compaction_budget_tokens, cache_ttl_1h";
        let invalid = || anyhow::anyhow!("invalid experiment {key}={value}; valid keys: {KEYS}");
        match key {
            "mask_tool_results" => self.mask_tool_results = value.parse().map_err(|_| invalid())?,
            "mask_keep_rounds" => self.mask_keep_rounds = value.parse().map_err(|_| invalid())?,
            "mask_trigger_tokens" => {
                self.mask_trigger_tokens = value.parse().map_err(|_| invalid())?;
            }
            "compaction_tail_turns" => {
                self.compaction_tail_turns = value.parse().map_err(|_| invalid())?;
            }
            "compaction_itemized" => {
                self.compaction_itemized = value.parse().map_err(|_| invalid())?;
            }
            "compaction_budget_tokens" => {
                self.compaction_budget_tokens = value.parse().map_err(|_| invalid())?;
            }
            "cache_ttl_1h" => self.cache_ttl_1h = value.parse().map_err(|_| invalid())?,
            _ => return Err(invalid()),
        }
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
struct Provider {
    #[serde(default)]
    openrouter_key: String,
    #[serde(default)]
    openai_key: String,
    #[serde(default)]
    opencode_key: String,
    #[serde(default)]
    mistral_key: String,
    #[serde(default)]
    host_token: Option<String>,
    #[serde(default)]
    openai_codex: Option<CodexCredentials>,
}

impl Provider {
    fn key(&self, tag: BackendTag) -> Option<&str> {
        match tag {
            BackendTag::OpenRouter => Some(&self.openrouter_key),
            BackendTag::OpenAi => Some(&self.openai_key),
            BackendTag::OpencodeGo => Some(&self.opencode_key),
            BackendTag::Mistral => Some(&self.mistral_key),
            BackendTag::Codex | BackendTag::Local => None,
        }
    }

    fn key_mut(&mut self, tag: BackendTag) -> Option<&mut String> {
        match tag {
            BackendTag::OpenRouter => Some(&mut self.openrouter_key),
            BackendTag::OpenAi => Some(&mut self.openai_key),
            BackendTag::OpencodeGo => Some(&mut self.opencode_key),
            BackendTag::Mistral => Some(&mut self.mistral_key),
            BackendTag::Codex | BackendTag::Local => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CodexCredentials {
    pub access: String,
    pub refresh: String,
    pub expires: i64,
    pub account_id: String,
}

/// Persisted, non-secret configuration for a named Cloudflare tunnel.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct NamedTunnelConfig {
    pub tunnel_id: String,
    pub hostname: String,
    pub credentials_path: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct HostSettings {
    #[serde(default)]
    named_tunnel: Option<NamedTunnelConfig>,
}

/// Every credential the app knows about at once — every configured backend is simultaneously usable.
#[derive(Debug, Clone, Default)]
pub struct SavedCreds {
    /// Machine-local, never synced between machines.
    pub local: Option<crate::provider::local::LocalConfig>,
    pub experiments: Experiments,
    pub agent: AgentConfig,
    pub keys: HashMap<BackendTag, String>,
    pub codex: Option<CodexCredentials>,
    /// Protects the local host API; never synced with space data.
    pub host_token: Option<String>,
}

impl SavedCreds {
    /// The saved API key for a backend, excluding OAuth credentials.
    #[must_use]
    pub fn key(&self, tag: BackendTag) -> Option<&str> {
        self.keys.get(&tag).map(String::as_str)
    }

    /// Store an API key for a key-login backend.
    pub fn set_key(&mut self, tag: BackendTag, key: String) {
        if tag.info().key.is_some() {
            self.keys.insert(tag, key);
        }
    }

    /// Guess a single key's backend for tests and single-key bootstraps.
    pub fn from_key(key: Option<&str>) -> Self {
        use crate::provider::openrouter::OpenRouter;

        let mut saved = Self::default();
        if let Some(key) = key {
            let tag = OpenRouter::from_key_auto(key.to_string()).backend_tag();
            saved.set_key(tag, key.to_string());
        }
        saved
    }
}

/// `config.toml` holds API keys, OAuth tokens and the host bearer token, so it is
/// written at 0600 rather than the process umask.
fn write_secret_file(path: &std::path::Path, body: &str) -> Result<()> {
    // Written beside the target and renamed over it, so a crash mid-write can't
    // leave a truncated file that loses every key.
    let temp = path.with_extension(format!("toml.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temp, body).with_context(|| format!("writing {}", temp.display()))?;
    restrict_to_owner(&temp);
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    restrict_to_owner(path);
    Ok(())
}

/// Read `path` as a TOML table (an absent file is an empty table).
///
/// # Errors
/// The file exists but cannot be read or does not parse.
pub(crate) fn read_table_at(path: &Path) -> Result<toml::Table> {
    if !path.exists() {
        return Ok(toml::Table::new());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    // TOML diagnostics include source lines, which may contain credentials.
    toml::from_str::<toml::Table>(&text)
        .map_err(|_| anyhow::anyhow!("invalid TOML in {}", path.display()))
}

/// Read-modify-write the config, keeping every key `edit` does not touch; a file
/// that does not parse is left alone rather than replaced.
///
/// # Errors
/// Propagates read, parse, `edit` and write failures.
pub(crate) fn edit_config_at(
    path: &Path,
    edit: impl FnOnce(&mut toml::Table) -> Result<()>,
) -> Result<()> {
    // Several connectors can persist their first definition pins at startup.
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let _guard = WRITER.lock().expect("config writer");
    let mut root = read_table_at(path)?;
    edit(&mut root)?;
    let body = toml::to_string_pretty(&root).context("serializing config")?;
    write_secret_file(path, &body)
}

#[cfg(unix)]
fn restrict_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &std::path::Path) {}

/// The app's dirs: `~/.config/nexus-chat` and `~/.local/share/nexus-chat` on
/// Linux, both `~/Library/Application Support/nexus-chat` on macOS.
pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", "nexus-chat").context("could not resolve home directory")
}

pub fn config_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.toml"))
}

/// Optional custom start-screen banner: paste any ASCII art into
/// `banner.txt` in the config dir and it replaces the built-in one.
pub fn load_banner() -> Option<String> {
    let path = project_dirs().ok()?.config_dir().join("banner.txt");
    let art = std::fs::read_to_string(path).ok()?;
    (!art.trim().is_empty()).then(|| art.trim_end().to_string())
}

/// Optional user additions to the bundled base prompt: app-level, beside config.toml.
pub fn system_prompt_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("system_prompt.md"))
}

pub fn load_system_prompt_overlay() -> String {
    system_prompt_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim_end().to_string())
        .unwrap_or_default()
}

/// Resolve credentials from disk and env with no network access, so read-only CLI
/// commands never touch the network.
pub fn load_creds_offline() -> SavedCreds {
    let stored = load_config_all().unwrap_or_default();
    let host_token = load_host_token().ok().flatten();
    let resolve = |stored: &str, env_key: &str| {
        Some(stored.to_string())
            .filter(|key| !key.is_empty())
            .or_else(|| {
                std::env::var(env_key)
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
    };
    let keys = BackendTag::ALL
        .into_iter()
        .filter_map(|tag| {
            let login = tag.info().key.as_ref()?;
            let value = stored.key(tag)?;
            resolve(value, login.env_var).map(|key| (tag, key))
        })
        .collect();
    SavedCreds {
        local: load_local_config().ok().flatten(),
        experiments: load_experiments().unwrap_or_default(),
        agent: load_agent_config().unwrap_or_default(),
        keys,
        codex: stored.openai_codex,
        host_token,
    }
}

/// Resolve every configured credential, scaffolding an empty config on first run;
/// missing credentials never fail because `/login` can set them later.
pub fn load_all_providers() -> Result<SavedCreds> {
    let path = config_path()?;
    if !path.exists() {
        write_provider_config(&Provider::default())?;
    }
    let mut saved = load_creds_offline();
    saved.local = load_local_config()?;
    saved.experiments = load_experiments()?;
    saved.agent = load_agent_config()?;
    if let Some(local) = &saved.local {
        local.validate()?;
    }
    Ok(saved)
}

pub(crate) fn load_reddit_credentials() -> Option<(String, String)> {
    let text = config_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    reddit_credentials(&text, |key| std::env::var(key).ok())
}

fn reddit_credentials(
    text: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Option<(String, String)> {
    let config = toml::from_str::<toml::Value>(text).ok();
    let read = |key: &str, env_key: &str| {
        config
            .as_ref()
            .and_then(|config| config.get("search")?.get(key)?.as_str())
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env(env_key))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    Some((
        read("reddit_client_id", "REDDIT_CLIENT_ID")?,
        read("reddit_client_secret", "REDDIT_CLIENT_SECRET")?,
    ))
}

fn load_agent_config() -> Result<AgentConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AgentConfig::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: Config = toml::from_str(&text).context("parsing agent configuration")?;
    Ok(config.agent)
}

fn load_experiments() -> Result<Experiments> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Experiments::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: Config = toml::from_str(&text).context("parsing experiments configuration")?;
    Ok(config.experiments)
}

/// The user's `[extract.converters]` hooks: `"<ext or mime>" = ["cmd", "{in}"]`,
/// run with the file path substituted and read as Markdown from stdout.
pub fn load_extract_converters() -> Vec<(String, Vec<String>)> {
    config_path()
        .ok()
        .and_then(|path| read_table_at(&path).ok())
        .map(|root| converters_from_table(&root))
        .unwrap_or_default()
}

pub(crate) fn converters_from_table(root: &toml::Table) -> Vec<(String, Vec<String>)> {
    let Some(table) = root
        .get("extract")
        .and_then(|e| e.get("converters"))
        .and_then(toml::Value::as_table)
    else {
        return Vec::new();
    };
    table
        .iter()
        .filter_map(|(key, value)| {
            let argv: Vec<String> = value
                .as_array()?
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<_>>()?;
            let key = key.trim().trim_start_matches('.').to_lowercase();
            (!key.is_empty() && argv.first().is_some_and(|c| !c.trim().is_empty()))
                .then_some((key, argv))
        })
        .collect()
}

fn load_local_config() -> Result<Option<crate::provider::local::LocalConfig>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: Config = toml::from_str(&text).context("parsing local runtime configuration")?;
    Ok(config.local)
}

pub fn first_configured(saved: &SavedCreds) -> Option<(&'static str, String)> {
    BackendTag::ALL
        .into_iter()
        .find_map(|tag| {
            let login = tag.info().key.as_ref()?;
            saved
                .key(tag)
                .map(|key| (login.config_name, key.to_string()))
        })
        .or_else(|| saved.codex.as_ref().map(|c| ("codex", c.access.clone())))
}

pub fn codex_account_id(access_token: &str) -> Result<String> {
    let payload = access_token
        .split('.')
        .nth(1)
        .context("invalid Codex access token")?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .context("decoding Codex access token")?;
    let v: serde_json::Value = serde_json::from_slice(&payload).context("parsing Codex token")?;
    let account = v
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|a| a.as_str())
        .context("Codex token missing ChatGPT account id")?;
    Ok(account.to_string())
}

pub(crate) async fn refresh_codex_if_needed(creds: CodexCredentials) -> Result<CodexCredentials> {
    if chrono::Utc::now().timestamp_millis() < creds.expires - 60_000 {
        return Ok(creds);
    }
    let resp = reqwest::Client::new()
        .post("https://auth.openai.com/oauth/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", creds.refresh.as_str()),
            ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"),
        ])
        .send()
        .await
        .context("refreshing OpenAI Codex token")?
        .error_for_status()
        .context("OpenAI Codex token refresh failed")?
        .json::<serde_json::Value>()
        .await
        .context("parsing OpenAI Codex token refresh")?;
    let access = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .context("missing access_token")?
        .to_string();
    let refresh = resp
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or(&creds.refresh)
        .to_string();
    let expires_in = resp
        .get("expires_in")
        .and_then(serde_json::Value::as_i64)
        .context("missing expires_in")?;
    Ok(CodexCredentials {
        account_id: codex_account_id(&access)?,
        access,
        refresh,
        expires: chrono::Utc::now().timestamp_millis() + expires_in * 1000,
    })
}

pub fn save_codex_credentials(creds: &CodexCredentials) -> Result<()> {
    let mut stored = load_config_all().unwrap_or_default();
    stored.openai_codex = Some(creds.clone());
    write_provider_config(&stored)
}

pub fn load_openrouter_key_only() -> Option<String> {
    let login = BackendTag::OpenRouter.info().key.as_ref()?;
    if let Ok(v) = std::env::var(login.env_var) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    load_config_all()
        .ok()
        .map(|stored| stored.openrouter_key)
        .filter(|key| !key.is_empty())
}

/// The `/login` selector always knows the flavor of a pasted key, so nothing is shape-sniffed.
pub fn save_provider_key(flavor: &str, key: &str) -> Result<()> {
    let mut stored = load_config_all().unwrap_or_default();
    for tag in BackendTag::ALL {
        if tag
            .info()
            .key
            .as_ref()
            .is_some_and(|login| login.config_name == flavor)
            && let Some(slot) = stored.key_mut(tag)
        {
            key.clone_into(slot);
            return write_provider_config(&stored);
        }
    }
    Ok(())
}

fn load_config_all() -> Result<Provider> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Provider::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg.provider)
}

/// Read the host bearer token without exposing it in provider selection or diagnostics.
pub fn load_host_token() -> Result<Option<String>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg
        .provider
        .host_token
        .filter(|token| !token.trim().is_empty()))
}

pub fn save_host_token(token: &str) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str::<toml::Value>(&text)
            .with_context(|| format!("parsing {}", path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = value
        .as_table_mut()
        .context("config root is not a TOML table")?;
    let provider = table
        .entry("provider".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let provider = provider
        .as_table_mut()
        .context("config provider is not a TOML table")?;
    provider.insert(
        "host_token".to_string(),
        toml::Value::String(token.to_string()),
    );
    let body = toml::to_string_pretty(&value).context("serializing config")?;
    write_secret_file(&path, &body)
}

pub fn ensure_host_token() -> Result<String> {
    if let Some(token) = load_host_token()? {
        return Ok(token);
    }
    let token = uuid::Uuid::new_v4().to_string();
    save_host_token(&token)?;
    Ok(token)
}

/// Load a provisioned named tunnel without contacting Cloudflare.
pub fn load_named_tunnel() -> Result<Option<NamedTunnelConfig>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg.host.and_then(|host| host.named_tunnel))
}

/// Persist named-tunnel identifiers and paths; credentials stay in cloudflared's private file.
pub fn save_named_tunnel(tunnel: &NamedTunnelConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str::<toml::Value>(&text)
            .with_context(|| format!("parsing {}", path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let root = value
        .as_table_mut()
        .context("config root is not a TOML table")?;
    let host = root
        .entry("host".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("config host is not a TOML table")?;
    host.insert(
        "named_tunnel".to_string(),
        toml::Value::try_from(tunnel).context("serializing named tunnel config")?,
    );
    let body = toml::to_string_pretty(&value).context("serializing config")?;
    write_secret_file(&path, &body)
}

pub async fn login_openai_codex_device(
    status: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<CodexCredentials> {
    login_openai_codex_device_with_browser(status, true).await
}

/// Device login for a host client: no browser launch, no host clipboard.
pub async fn login_openai_codex_remote(
    status: tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<CodexCredentials> {
    login_openai_codex_device_with_browser(status, false).await
}

// Long by design (device-flow state machine).
#[allow(clippy::too_many_lines)]
async fn login_openai_codex_device_with_browser(
    status: tokio::sync::mpsc::UnboundedSender<String>,
    local_browser: bool,
) -> Result<CodexCredentials> {
    let client = reqwest::Client::new();
    let device = client
        .post("https://auth.openai.com/api/accounts/deviceauth/usercode")
        .json(&serde_json::json!({ "client_id": "app_EMoamEEZ73f0CkXaXp7hrann" }))
        .send()
        .await
        .context("starting OpenAI Codex device login")?
        .error_for_status()
        .context("OpenAI Codex device login failed")?
        .json::<serde_json::Value>()
        .await
        .context("parsing OpenAI Codex device login")?;
    let device_auth_id = device
        .get("device_auth_id")
        .and_then(|v| v.as_str())
        .context("missing device_auth_id")?
        .to_string();
    let user_code = device
        .get("user_code")
        .and_then(|v| v.as_str())
        .context("missing user_code")?
        .to_string();
    let interval = device
        .get("interval")
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse::<f64>().ok()))
        .unwrap_or(5.0)
        .max(1.0);
    let url = "https://auth.openai.com/codex/device";
    let prefilled_url = format!("{url}?user_code={user_code}");
    if local_browser {
        let _ = arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(user_code.clone()));
        let _ = status.send(format!(
            "{user_code}  ← copied to clipboard; enter at {url}"
        ));
        let _ = open::that(&prefilled_url);
    } else {
        let _ = status.send(format!("device-code:{user_code}"));
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_mins(15);
    let code = loop {
        if std::time::Instant::now() >= deadline {
            bail!("OpenAI Codex device login timed out");
        }
        tokio::time::sleep(std::time::Duration::from_secs_f64(interval)).await;
        let resp = client
            .post("https://auth.openai.com/api/accounts/deviceauth/token")
            .json(&serde_json::json!({ "device_auth_id": device_auth_id, "user_code": user_code }))
            .send()
            .await
            .context("polling OpenAI Codex device login")?;
        if resp.status().is_success() {
            let v = resp
                .json::<serde_json::Value>()
                .await
                .context("parsing OpenAI Codex device token")?;
            let authorization_code = v
                .get("authorization_code")
                .and_then(|v| v.as_str())
                .context("missing authorization_code")?
                .to_string();
            let code_verifier = v
                .get("code_verifier")
                .and_then(|v| v.as_str())
                .context("missing code_verifier")?
                .to_string();
            break (authorization_code, code_verifier);
        }
        if resp.status().as_u16() != 403 && resp.status().as_u16() != 404 {
            let status_code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("OpenAI Codex device login failed ({status_code}): {body}");
        }
    };

    let token = client
        .post("https://auth.openai.com/oauth/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"),
            ("code", code.0.as_str()),
            ("code_verifier", code.1.as_str()),
            (
                "redirect_uri",
                "https://auth.openai.com/deviceauth/callback",
            ),
        ])
        .send()
        .await
        .context("exchanging OpenAI Codex device code")?
        .error_for_status()
        .context("OpenAI Codex device code exchange failed")?
        .json::<serde_json::Value>()
        .await
        .context("parsing OpenAI Codex token")?;
    let access = token
        .get("access_token")
        .and_then(|v| v.as_str())
        .context("missing access_token")?
        .to_string();
    let refresh = token
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .context("missing refresh_token")?
        .to_string();
    let expires_in = token
        .get("expires_in")
        .and_then(serde_json::Value::as_i64)
        .context("missing expires_in")?;
    let creds = CodexCredentials {
        account_id: codex_account_id(&access)?,
        access,
        refresh,
        expires: chrono::Utc::now().timestamp_millis() + expires_in * 1000,
    };
    save_codex_credentials(&creds)?;
    Ok(creds)
}

/// Persist (or clear) the machine-local `[local]` block, leaving credentials untouched.
///
/// # Errors
/// Propagates read/serialize/write failures on the config file.
pub fn save_local_config(local: Option<&crate::provider::local::LocalConfig>) -> Result<()> {
    let stored = load_config_all().unwrap_or_default();
    write_config_at(&config_path()?, &stored, local)
}

fn write_provider_config(stored: &Provider) -> Result<()> {
    let local = load_local_config()?;
    write_config_at(&config_path()?, stored, local.as_ref())
}

/// Update credentials, the Codex login and `[local]`; every other table is kept and
/// a file that doesn't parse is left alone rather than replaced.
fn write_config_at(
    path: &std::path::Path,
    stored: &Provider,
    local: Option<&crate::provider::local::LocalConfig>,
) -> Result<()> {
    use toml::Value;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut root = if path.exists() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str::<toml::Table>(&text)
            .with_context(|| format!("parsing {} (left unchanged)", path.display()))?
    } else {
        toml::Table::new()
    };
    let provider = root
        .entry("provider".to_string())
        .or_insert_with(|| Value::Table(toml::Table::new()))
        .as_table_mut()
        .context("config provider is not a TOML table")?;
    for tag in BackendTag::ALL {
        let Some(login) = &tag.info().key else {
            continue;
        };
        let Some(value) = stored.key(tag) else {
            continue;
        };
        // Removed rather than written empty, so configs that never used Mistral stay as they were.
        if tag == BackendTag::Mistral && value.is_empty() {
            provider.remove(login.config_field);
        } else {
            provider.insert(
                login.config_field.to_string(),
                Value::String(value.to_string()),
            );
        }
    }
    match &stored.openai_codex {
        Some(c) => {
            provider.insert(
                "openai_codex".to_string(),
                Value::try_from(c).context("serializing Codex credentials")?,
            );
        }
        None => {
            provider.remove("openai_codex");
        }
    }
    match local {
        Some(local) => {
            root.insert(
                "local".to_string(),
                Value::try_from(local).context("serializing local runtime configuration")?,
            );
        }
        None => {
            root.remove("local");
        }
    }
    let body = toml::to_string_pretty(&root).context("serializing config")?;
    write_secret_file(path, &body)
}

#[cfg(test)]
mod reddit_tests {
    use super::reddit_credentials;

    #[test]
    fn credentials_use_config_then_env_and_require_both_fields() {
        let env = |key: &str| match key {
            "REDDIT_CLIENT_ID" => Some("env-id".into()),
            "REDDIT_CLIENT_SECRET" => Some("env-secret".into()),
            _ => None,
        };
        assert_eq!(
            reddit_credentials(
                "[search]\nreddit_client_id='config-id'\nreddit_client_secret='config-secret'",
                env
            ),
            Some(("config-id".into(), "config-secret".into()))
        );
        assert_eq!(
            reddit_credentials("", env),
            Some(("env-id".into(), "env-secret".into()))
        );
        assert_eq!(
            reddit_credentials("[search]\nreddit_client_id='config-id'", env),
            Some(("config-id".into(), "env-secret".into()))
        );
        assert_eq!(
            reddit_credentials("[search]\nreddit_client_id='config-id'", |_| None),
            None
        );
        assert_eq!(
            reddit_credentials(
                "[search]\nreddit_client_id=' '\nreddit_client_secret=' '",
                |_| None
            ),
            None
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connectors_section_never_blocks_provider_loading() {
        for body in [
            "[provider]\nopenrouter_key = \"k\"\n[connectors]\nlinear = 5\n",
            "connectors = \"nonsense\"\n[provider]\nopenrouter_key = \"k\"\n",
        ] {
            let config: super::Config = toml::from_str(body).unwrap();
            assert_eq!(config.provider.openrouter_key, "k");
        }
    }

    #[test]
    fn experiment_defaults_round_trip_and_set() {
        let defaults = Experiments::default();
        assert_eq!(defaults.mask_keep_rounds, 10);
        assert_eq!(toml::from_str::<Config>("").unwrap().experiments, defaults);
        let encoded = toml::to_string(&defaults).unwrap();
        assert_eq!(toml::from_str::<Experiments>(&encoded).unwrap(), defaults);
        let mut experiments = defaults;
        for (key, value) in [
            ("mask_tool_results", "true"),
            ("mask_keep_rounds", "12"),
            ("mask_trigger_tokens", "12345"),
            ("compaction_tail_turns", "3"),
            ("compaction_itemized", "true"),
            ("compaction_budget_tokens", "40000"),
            ("cache_ttl_1h", "true"),
        ] {
            experiments.set(key, value).unwrap();
            let error = experiments.set(key, "invalid").unwrap_err().to_string();
            assert!(error.contains("valid keys:"));
            assert!(error.contains(key));
        }
        assert_eq!(
            experiments,
            Experiments {
                mask_tool_results: true,
                mask_keep_rounds: 12,
                mask_trigger_tokens: 12345,
                compaction_tail_turns: 3,
                compaction_itemized: true,
                compaction_budget_tokens: 40000,
                cache_ttl_1h: true,
            }
        );
        let section = format!("[experiments]\n{}", toml::to_string(&experiments).unwrap());
        assert_eq!(
            toml::from_str::<Config>(&section).unwrap().experiments,
            experiments
        );
        assert_eq!(
            toml::from_str::<Config>("[experiments]\ncache_ttl_1h = true")
                .unwrap()
                .experiments
                .mask_keep_rounds,
            10
        );
        for (key, value) in [
            ("unknown", "true"),
            ("mask_keep_rounds", "-1"),
            ("mask_trigger_tokens", "-1"),
            ("cache_ttl_1h", "1"),
        ] {
            assert!(experiments.set(key, value).is_err());
        }
        assert!(toml::from_str::<Config>("[experiments]\nunknown = true").is_err());
    }

    #[test]
    fn secret_env_vars_include_every_backend_key() {
        for tag in BackendTag::ALL {
            if let Some(key) = &tag.info().key {
                assert!(SECRET_ENV_VARS.contains(&key.env_var), "{}", key.env_var);
            }
        }
    }

    #[test]
    fn scrub_secret_env_removes_every_secret() {
        let mut cmd = std::process::Command::new("env");
        scrub_secret_env(&mut cmd);
        for key in SECRET_ENV_VARS {
            assert!(
                cmd.get_envs()
                    .any(|entry| entry == (std::ffi::OsStr::new(key), None))
            );
        }
    }

    #[test]
    fn local_runtime_config_round_trips_alongside_cloud_credentials() {
        let cfg: Config = toml::from_str(
            "[provider]\nopenai_key = 'test-key'\n[local]\nprovider = 'mlx'\nlist_command = ['my-list', '--installed']\n",
        ).unwrap();
        assert_eq!(cfg.provider.openai_key, "test-key");
        let local = cfg.local.unwrap();
        local.validate().unwrap();
        let encoded = toml::to_string(&local).unwrap();
        let decoded: crate::provider::local::LocalConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.provider, local.provider);
        assert_eq!(decoded.list_command, local.list_command);
        assert_eq!(decoded.endpoint(), "http://localhost:8080/v1");
        assert!(toml::from_str::<Config>("[local]\nprovider = 'unknown'").is_err());
    }

    #[test]
    fn local_context_settings_parse_and_round_trip_with_defaults_and_overrides() {
        for (settings, context_length, kv_cache_q8) in [
            ("", None, false),
            (
                "context_length = 65536\nkv_cache_q8 = true\n",
                Some(65_536),
                true,
            ),
            (
                "context_length = 2048\nkv_cache_q8 = false\n",
                Some(2048),
                false,
            ),
        ] {
            let cfg: Config = toml::from_str(&format!(
                "[provider]\nopenai_key = 'test-key'\n[local]\nprovider = 'ollama'\n{settings}"
            ))
            .unwrap();
            assert_eq!(cfg.provider.openai_key, "test-key");
            let local = cfg.local.unwrap();
            local.validate().unwrap();
            assert_eq!(local.context_length, context_length);
            assert_eq!(local.kv_cache_q8, kv_cache_q8);
            let encoded = toml::to_string(&local).unwrap();
            assert_eq!(encoded.contains("context_length"), context_length.is_some());
            assert_eq!(encoded.contains("kv_cache_q8"), kv_cache_q8);
            let decoded: crate::provider::local::LocalConfig = toml::from_str(&encoded).unwrap();
            assert_eq!(decoded.context_length, context_length);
            assert_eq!(decoded.kv_cache_q8, kv_cache_q8);
        }
    }

    #[test]
    fn credential_writes_keep_every_other_table_and_key() {
        let dir = std::env::temp_dir().join(format!("nexus-config-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "[provider]\nopenrouter_key = 'old'\nhost_token = 'tok'\nfuture_key = 'kept'\n\
             [search]\nreddit_client_id = 'rid'\nreddit_client_secret = 'rsecret'\n\
             [agent]\nsandbox = 'off'\n[studio]\nhost_url = 'http://127.0.0.1:9'\n\
             [host.named_tunnel]\ntunnel_id = 't'\n[local]\nprovider = 'ollama'\n",
        )
        .unwrap();
        let codex = CodexCredentials {
            access: "a".into(),
            refresh: "r".into(),
            expires: 1,
            account_id: "acct".into(),
        };
        let local = toml::from_str::<Config>("[local]\nprovider = 'ollama'\n")
            .unwrap()
            .local;
        let stored = Provider {
            openrouter_key: "new".into(),
            mistral_key: "vibe".into(),
            openai_codex: Some(codex),
            ..Provider::default()
        };
        write_config_at(&path, &stored, local.as_ref()).unwrap();
        let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["provider"]["openrouter_key"].as_str(), Some("new"));
        assert_eq!(saved["provider"]["mistral_key"].as_str(), Some("vibe"));
        assert_eq!(saved["provider"]["host_token"].as_str(), Some("tok"));
        assert_eq!(saved["provider"]["future_key"].as_str(), Some("kept"));
        assert_eq!(
            saved["provider"]["openai_codex"]["account_id"].as_str(),
            Some("acct")
        );
        assert_eq!(
            saved["search"]["reddit_client_secret"].as_str(),
            Some("rsecret")
        );
        assert_eq!(saved["agent"]["sandbox"].as_str(), Some("off"));
        assert_eq!(
            saved["studio"]["host_url"].as_str(),
            Some("http://127.0.0.1:9")
        );
        assert_eq!(
            saved["host"]["named_tunnel"]["tunnel_id"].as_str(),
            Some("t")
        );
        assert_eq!(saved["local"]["provider"].as_str(), Some("ollama"));
        let codex: CodexCredentials = saved["provider"]["openai_codex"]
            .clone()
            .try_into()
            .unwrap();
        assert_eq!(codex.refresh, "r");

        let only = |key: &str| Provider {
            openrouter_key: key.into(),
            ..Provider::default()
        };
        write_config_at(&path, &only("new"), None).unwrap();
        let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved["provider"].get("openai_codex").is_none());
        assert!(saved["provider"].get("mistral_key").is_none());
        assert!(saved.get("local").is_none());
        assert_eq!(saved["search"]["reddit_client_id"].as_str(), Some("rid"));

        std::fs::write(&path, "[provider\nopenrouter_key = 'precious'").unwrap();
        assert!(write_config_at(&path, &only("x"), None).is_err());
        assert!(std::fs::read_to_string(&path).unwrap().contains("precious"));
        #[cfg(unix)]
        {
            write_config_at(&dir.join("fresh.toml"), &only("k"), None).unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join("fresh.toml"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_creds_from_key_guesses_openrouter() {
        let saved = SavedCreds::from_key(Some("sk-or-test"));
        assert_eq!(saved.key(BackendTag::OpenRouter), Some("sk-or-test"));
        assert!(saved.key(BackendTag::OpenAi).is_none());
        assert!(saved.key(BackendTag::OpencodeGo).is_none());
        assert!(saved.key(BackendTag::Mistral).is_none());
        assert!(saved.codex.is_none());
        assert!(saved.local.is_none());
        assert!(saved.host_token.is_none());
    }

    #[test]
    fn saved_creds_from_no_key_is_empty() {
        let saved = SavedCreds::from_key(None);
        assert!(saved.key(BackendTag::OpenRouter).is_none());
        assert!(saved.key(BackendTag::OpenAi).is_none());
        assert!(saved.key(BackendTag::OpencodeGo).is_none());
        assert!(saved.key(BackendTag::Mistral).is_none());
        assert!(saved.codex.is_none());
        assert!(saved.local.is_none());
        assert!(saved.host_token.is_none());
    }

    #[test]
    fn parses_keys() {
        let cfg: Config = toml::from_str(
            "[provider]\nopenrouter_key = \"sk-or-abc\"\nopenai_key = \"sk-proj-abc\"\n",
        )
        .unwrap();
        assert_eq!(cfg.provider.openrouter_key, "sk-or-abc");
        assert_eq!(cfg.provider.openai_key, "sk-proj-abc");
    }

    #[test]
    fn missing_keys_default_empty() {
        let cfg: Config = toml::from_str("[provider]\n").unwrap();
        assert!(cfg.provider.openrouter_key.is_empty());
        assert!(cfg.provider.openai_key.is_empty());
    }

    fn codex_creds(access: &str) -> CodexCredentials {
        CodexCredentials {
            access: access.to_string(),
            refresh: "r".to_string(),
            expires: 0,
            account_id: "a".to_string(),
        }
    }

    #[test]
    fn first_configured_follows_fixed_priority() {
        let saved = SavedCreds {
            keys: HashMap::from([
                (BackendTag::OpenRouter, "sk-or-abc".into()),
                (BackendTag::OpenAi, "sk-proj-abc".into()),
                (BackendTag::OpencodeGo, "oc-token".into()),
            ]),
            codex: Some(codex_creds("codex-token")),
            ..Default::default()
        };
        assert_eq!(
            first_configured(&saved),
            Some(("openrouter", "sk-or-abc".to_string()))
        );
    }

    #[test]
    fn first_configured_falls_through_to_whatever_is_set() {
        let saved = SavedCreds {
            keys: HashMap::from([(BackendTag::OpencodeGo, "oc-token".into())]),
            codex: Some(codex_creds("codex-token")),
            ..Default::default()
        };
        assert_eq!(
            first_configured(&saved),
            Some(("opencode", "oc-token".to_string()))
        );
    }

    #[test]
    fn first_configured_none_when_nothing_saved() {
        assert_eq!(first_configured(&SavedCreds::default()), None);
    }

    #[test]
    fn extract_converters_come_from_the_config_table() {
        let root: toml::Table = toml::from_str(
            r#"
            [extract.converters]
            "RTF" = ["unrtf", "--text", "{in}"]
            ".key" = ["keynote2md", "{in}"]
            "application/x-thing" = ["thing"]
            empty = []
            wrong = "not an array"
            mixed = ["ok", 3]
            blank = [""]
            "#,
        )
        .unwrap();
        let mut got = converters_from_table(&root);
        got.sort();
        assert_eq!(
            got,
            vec![
                ("application/x-thing".to_string(), vec!["thing".to_string()]),
                (
                    "key".to_string(),
                    vec!["keynote2md".to_string(), "{in}".to_string()]
                ),
                (
                    "rtf".to_string(),
                    vec![
                        "unrtf".to_string(),
                        "--text".to_string(),
                        "{in}".to_string()
                    ]
                ),
            ]
        );
        assert!(converters_from_table(&toml::Table::new()).is_empty());
    }

    #[test]
    fn extract_converters_survive_a_credential_save() {
        let dir = std::env::temp_dir().join(format!("nexus-config-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "[extract.converters]\n\"rtf\" = [\"unrtf\", \"{in}\"]\n",
        )
        .unwrap();
        write_config_at(
            &path,
            &Provider {
                openrouter_key: "key".into(),
                ..Provider::default()
            },
            None,
        )
        .unwrap();
        let saved = read_table_at(&path).unwrap();
        assert_eq!(
            converters_from_table(&saved),
            vec![(
                "rtf".to_string(),
                vec!["unrtf".to_string(), "{in}".to_string()]
            )]
        );
        assert_eq!(saved["provider"]["openrouter_key"].as_str(), Some("key"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(toml::from_str::<Config>(&text).is_ok());
    }
}
