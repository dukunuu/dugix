// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::Engine;
use futures_util::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::harmony::{self, Piece};
use super::{
    ChatMessage, ChatParams, Completion, Model, ModelPricing, PromptConvention, ReasoningEffort,
    StreamEvent, ToolCall, ToolDef, Usage,
};
use crate::tools::ToolExecutor;
use crate::trace::{TraceResponse, TraceStep, TraceToolResult};

const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const CODEX_BASE: &str = "https://chatgpt.com/backend-api";
/// The Codex CLI release whose catalog shape `parse_codex_catalog` reads; the
/// backend filters its model list by this version, so newer models stay
/// hidden from an older one. A newer installed CLI's version wins
/// (`codex_client_version`).
const CODEX_CLIENT_VERSION: &str = "0.163.0";
/// The general Zen catalog: free-tier and pay-per-token models, plus
/// whatever a Go subscription adds. This is the default/fallback base.
const OPENCODE_ZEN_BASE: &str = "https://opencode.ai/zen/v1";
/// The flat-fee Go-subscription bundle — a *different* endpoint from Zen's
/// general catalog, even though it's the same account key. Requests for a
/// Go-bundled model must go here, not to Zen general, or they'd be billed
/// per-token instead of covered by the flat $10/mo.
const OPENCODE_GO_BASE: &str = "https://opencode.ai/zen/go/v1";
/// Prefix on a Model id (from `list_models`) marking it as a flat-fee Go
/// model rather than a general Zen one — stripped before it's ever sent to
/// the API; only used to pick which of the two bases above to hit.
const OPENCODE_GO_PREFIX: &str = "go:";
/// Mistral's API. A Vibe plan key (Code > Vibe CLI) and a console key both
/// authenticate here; the plan budget meters the `mistral-vibe-cli-*` names.
const MISTRAL_BASE: &str = "https://api.mistral.ai/v1";
/// The names Mistral meters against a Vibe plan, as the Vibe CLI sends them:
/// `-latest` is the main coding model (Mistral Medium 3.5 as of 2026-11),
/// `-fast` the small utility one. `/v1/models` may not list them, so
/// `parse_mistral_catalog` adds whichever is missing.
const MISTRAL_VIBE_MODEL: &str = "mistral-vibe-cli-latest";
const MISTRAL_VIBE_FAST_MODEL: &str = "mistral-vibe-cli-fast";

/// The OpenAI-wire base URL used by a backend gateway route.
pub fn base_url(tag: crate::provider::BackendTag) -> &'static str {
    match tag {
        crate::provider::BackendTag::OpenRouter => OPENROUTER_BASE,
        crate::provider::BackendTag::OpenAi => OPENAI_BASE,
        crate::provider::BackendTag::OpencodeGo => OPENCODE_ZEN_BASE,
        crate::provider::BackendTag::Codex => CODEX_BASE,
        crate::provider::BackendTag::Mistral => MISTRAL_BASE,
        // Local routing requires instance configuration; never fall back to a cloud URL.
        crate::provider::BackendTag::Local => "http://localhost:11434/v1",
    }
}

/// Select the base URL and raw model id for a gateway request.
pub fn gateway_route(tag: crate::provider::BackendTag, model: &str) -> (&'static str, String) {
    if tag == crate::provider::BackendTag::OpencodeGo
        && let Some(raw) = model.strip_prefix(OPENCODE_GO_PREFIX)
    {
        return (OPENCODE_GO_BASE, raw.to_string());
    }
    (base_url(tag), model.to_string())
}

/// Context windows for `OpenCode` Zen's general catalog (`/zen/v1/models`),
/// keyed by raw model id. The Zen `/models` endpoint returns no context
/// metadata (only `id/created/owned_by`), so without this table every `OpenCode`
/// model would show no context size. Values mirror the models.dev catalog
/// that opencode itself uses for these ids.
const OPENCODE_ZEN_CONTEXT: &[(&str, u64)] = &[
    ("big-pickle", 200_000),
    ("claude-fable-5", 1_000_000),
    ("claude-haiku-4-5", 200_000),
    ("claude-opus-4-1", 200_000),
    ("claude-opus-4-5", 200_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-5", 1_000_000),
    ("claude-sonnet-4", 1_000_000),
    ("claude-sonnet-4-5", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("deepseek-v4-flash", 1_000_000),
    ("deepseek-v4-flash-free", 200_000),
    ("deepseek-v4-pro", 1_000_000),
    ("gemini-3-flash", 1_048_576),
    ("gemini-3.1-pro", 1_048_576),
    ("gemini-3.5-flash", 1_048_576),
    ("gemini-3.5-flash-lite", 1_048_576),
    ("gemini-3.6-flash", 1_048_576),
    ("glm-5", 204_800),
    ("glm-5.1", 204_800),
    ("glm-5.2", 1_000_000),
    ("gpt-5", 400_000),
    ("gpt-5-codex", 400_000),
    ("gpt-5-nano", 400_000),
    ("gpt-5.1", 400_000),
    ("gpt-5.1-codex", 400_000),
    ("gpt-5.1-codex-max", 400_000),
    ("gpt-5.1-codex-mini", 400_000),
    ("gpt-5.2", 400_000),
    ("gpt-5.2-codex", 400_000),
    ("gpt-5.3-codex", 400_000),
    ("gpt-5.3-codex-spark", 128_000),
    ("gpt-5.4", 1_050_000),
    ("gpt-5.4-mini", 400_000),
    ("gpt-5.4-nano", 400_000),
    ("gpt-5.4-pro", 1_050_000),
    ("gpt-5.5", 1_050_000),
    ("gpt-5.5-pro", 1_050_000),
    ("gpt-5.6-luna", 1_050_000),
    ("gpt-5.6-sol", 1_050_000),
    ("gpt-5.6-terra", 1_050_000),
    ("grok-4.5", 500_000),
    ("grok-build-0.1", 256_000),
    ("kimi-k2.5", 262_144),
    ("kimi-k2.6", 262_144),
    ("kimi-k2.7-code", 262_144),
    ("kimi-k3", 1_048_576),
    ("laguna-s-2.1-free", 256_000),
    ("ling-3.0-flash-free", 262_144),
    ("longcat-2.0-free", 1_000_000),
    ("minimax-m2.5", 204_800),
    ("minimax-m2.7", 204_800),
    ("minimax-m3", 512_000),
    ("mimo-v2.5-free", 200_000),
    ("nemotron-3-ultra-free", 1_000_000),
    ("north-mini-code-free", 256_000),
    ("qwen3.5-plus", 262_144),
    ("qwen3.6-plus", 262_144),
];

/// Context windows for the flat-fee Go bundle (`/zen/go/v1/models`), keyed by
/// raw model id. Kept separate from `OPENCODE_ZEN_CONTEXT` because the *same
/// id* can have a different window on each endpoint (e.g. qwen3.6-plus:
/// 262k on Zen general vs 1M on Go; minimax-m3: 512k vs 1M).
const OPENCODE_GO_CONTEXT: &[(&str, u64)] = &[
    ("deepseek-v4-flash", 1_000_000),
    ("deepseek-v4-pro", 1_000_000),
    ("glm-5", 202_752),
    ("glm-5.1", 202_752),
    ("glm-5.2", 1_000_000),
    ("gpt-5.6-luna", 1_050_000),
    ("grok-4.5", 500_000),
    ("hy3", 256_000),
    ("hy3-preview", 256_000),
    ("kimi-k2.5", 262_144),
    ("kimi-k2.6", 262_144),
    ("kimi-k2.7-code", 262_144),
    ("kimi-k3", 1_048_576),
    ("mimo-v2-omni", 262_144),
    ("mimo-v2-pro", 1_048_576),
    ("mimo-v2.5", 1_000_000),
    ("mimo-v2.5-pro", 1_048_576),
    ("minimax-m2.5", 204_800),
    ("minimax-m2.7", 204_800),
    ("minimax-m3", 1_000_000),
    ("qwen3.5-plus", 262_144),
    ("qwen3.6-plus", 1_000_000),
    ("qwen3.7-max", 1_000_000),
    ("qwen3.7-plus", 1_000_000),
    ("qwen3.8-max", 1_000_000),
];
/// Hard cap on tool round-trips per response, so a model that keeps calling
/// tools can't loop forever. The default for interactive chat; background
/// jobs (e.g. deep-research searcher agents) pass their own smaller budget.
pub const MAX_TOOL_ITERS: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderFlavor {
    OpenRouter,
    OpenAi,
    OpenAiCodex,
    /// `OpenCode` Go: a low-cost subscription bundling several open coding
    /// models behind a fully OpenAI-compatible endpoint — no special
    /// request/response handling needed, unlike Codex.
    OpencodeGo,
    /// Mistral: OpenAI-shaped chat completions, but strict about request
    /// fields and tool-call ids (`mistral_body`), with its own model cards
    /// (`parse_mistral_catalog`) and thinking as typed content chunks.
    Mistral,
}

mod codex;
mod mistral;
mod search;
mod stream;
mod video;

pub use codex::{CodexChatStreamAdapter, chat_body_to_codex_body, codex_response_to_chat};
pub use mistral::mistral_gateway_body;
pub(crate) use stream::{merge_usage, parse_usage_value};
pub use video::normalize_video_params;

use codex::{
    accumulate_codex_tool_calls, codex_cache_headers, codex_client_version, codex_fallback_models,
    codex_input, codex_instructions, codex_reasoning_delta, codex_response_text, codex_text_delta,
    codex_tools, codex_usage, parse_codex_catalog, read_codex_cache, remember_turn_state,
};
use mistral::{MistralModelsResponse, mistral_body, parse_mistral_catalog};
use stream::{
    accumulate_tool_calls, parse_delta, parse_stream_cost, parse_usage, sse_event_data, wire_text,
};
use video::check_video_params;

// Test modules reach moved helpers through `use super::*`.
#[cfg(test)]
use codex::CODEX_TURN_STATE;
#[cfg(test)]
use mistral::{MISTRAL_MAX_IMAGES, mistral_tool_id};
#[cfg(test)]
use stream::chat_sse_frame;

#[derive(Clone)]
pub struct OpenRouter {
    client: reqwest::Client,
    #[cfg(test)]
    search_endpoint: Option<String>,
    key: String,
    flavor: ProviderFlavor,
    endpoint: Option<String>,
    local_config: Option<super::local::LocalConfig>,
    usage_log: Option<Box<(crate::db::UsageLog, String)>>,
}

/// Non-generative Decisions API answers, retaining optional provider cost.
#[derive(Debug, Deserialize)]
pub struct DecisionsResponse {
    pub answers: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub usage: DecisionsUsage,
}

/// Usage reported by the Decisions API.
#[derive(Debug, Default, Deserialize)]
pub struct DecisionsUsage {
    pub cost: Option<f64>,
}

/// Everything needed to generate a video: model, prompt, params, optional
/// frame/reference images, and provider-specific options keyed by provider
/// slug (e.g. `{"alibaba": {"parameters": {"video": "data:..."}}}`),
/// passed through as `provider.options` in the request body.
pub struct VideoRequest {
    pub model: String,
    pub prompt: String,
    pub duration: u32,
    pub resolution: String,
    pub aspect_ratio: String,
    pub generate_audio: bool,
    pub first_frame: Option<Vec<u8>>,
    pub last_frame: Option<Vec<u8>>,
    pub input_references: Vec<Vec<u8>>,
    pub seed: Option<i32>,
    pub provider_options: Option<serde_json::Value>,
}

impl ProviderFlavor {
    const fn base(self) -> &'static str {
        match self {
            Self::OpenRouter => OPENROUTER_BASE,
            Self::OpenAi => OPENAI_BASE,
            Self::OpenAiCodex => CODEX_BASE,
            Self::OpencodeGo => OPENCODE_ZEN_BASE,
            Self::Mistral => MISTRAL_BASE,
        }
    }

    /// The reasoning effort values this model accepts, in cycle order.
    /// Empty = the model has no reasoning/thinking mode.
    ///
    /// A catalog-provided `reasoning.supported_efforts` list is authoritative
    /// and wins for every backend that exposes it. Family rules are only
    /// fallbacks for catalogs that report the `reasoning` parameter without
    /// enumerating its accepted values.
    fn reasoning_efforts(self, m: &ModelEntry) -> Vec<ReasoningEffort> {
        use ReasoningEffort as E;
        if let Some(supported) = m
            .reasoning
            .as_ref()
            .map(|r| r.supported_efforts.as_slice())
            .filter(|values| !values.is_empty())
        {
            return E::CYCLE_ORDER
                .iter()
                .copied()
                .filter(|effort| supported.iter().any(|value| value == effort.as_str()))
                .collect();
        }
        match self {
            Self::OpenRouter => {
                if !m.supported_parameters.iter().any(|p| p == "reasoning") {
                    return Vec::new();
                }
                if m.id.starts_with("anthropic/") {
                    E::WITH_MINIMAL.to_vec()
                } else {
                    E::STANDARD.to_vec()
                }
            }
            Self::OpenAi => {
                let id = m.id.as_str();
                if id == "gpt-5-pro" || id.starts_with("gpt-5-pro-") {
                    E::HIGH_ONLY.to_vec()
                } else if id == "gpt-5"
                    || id.starts_with("gpt-5-20")
                    || id.starts_with("gpt-5-mini")
                    || id.starts_with("gpt-5-nano")
                {
                    E::WITH_MINIMAL.to_vec()
                } else if ["gpt-5.2", "gpt-5.3", "gpt-5.4", "gpt-5.5"]
                    .iter()
                    .any(|prefix| id.starts_with(prefix))
                {
                    E::WITH_XHIGH_AND_NONE.to_vec()
                } else if id.starts_with("gpt-5.6") {
                    E::WITH_MAX_XHIGH_AND_NONE.to_vec()
                } else if id.starts_with("gpt-6") {
                    E::WITH_MAX_XHIGH.to_vec()
                } else if id
                    .strip_prefix('o')
                    .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
                    || id.starts_with("gpt-5")
                {
                    E::STANDARD.to_vec()
                } else {
                    Vec::new()
                }
            }
            // Codex and the Go bundle are reasoning-capable chat catalogs.
            // Their catalog metadata wins above when present; this standard
            // set is only for entries that expose no per-model detail.
            Self::OpenAiCodex | Self::OpencodeGo => E::STANDARD.to_vec(),
            // Mistral cards go through `parse_mistral_catalog`, not here.
            Self::Mistral => Vec::new(),
        }
    }

    fn supports_images(self, m: &ModelEntry) -> Option<bool> {
        match self {
            Self::OpenRouter | Self::OpencodeGo | Self::Mistral => None,
            Self::OpenAi => {
                let id = m.id.as_str();
                Some(
                    id.contains("gpt-4o")
                        || id.contains("gpt-4.1")
                        || id.starts_with("gpt-5")
                        || id.starts_with("o3")
                        || id.starts_with("o4"),
                )
            }
            Self::OpenAiCodex => Some(true),
        }
    }

    fn supports_image_generation(self, m: &ModelEntry) -> Option<bool> {
        match self {
            // OpenRouter reports output_modalities in the catalog — use that.
            Self::OpenRouter => None,
            // Only dall-e models on OpenAI.
            Self::OpenAi => Some(m.id.contains("dall-e")),
            // Codex, Go and Mistral's chat models don't generate images.
            Self::OpenAiCodex | Self::OpencodeGo | Self::Mistral => Some(false),
        }
    }

    fn add_stream_usage(self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        match self {
            // Ask OpenRouter for exact token accounting in the final chunk.
            Self::OpenRouter => {
                obj.insert("usage".into(), serde_json::json!({ "include": true }));
            }
            // OpenAI (and OpenCode Go, which mirrors OpenAI's API shape)
            // put the equivalent switch under stream_options.
            Self::OpenAi | Self::OpencodeGo => {
                obj.insert(
                    "stream_options".into(),
                    serde_json::json!({ "include_usage": true }),
                );
            }
            // Mistral sends usage in the final chunk unasked, and rejects
            // `stream_options` as an unknown field.
            Self::OpenAiCodex | Self::Mistral => {}
        }
    }

    /// Add the cache-lane key for backends that route on it.
    ///
    /// `OpenAI`, `OpenCode` Zen/Go and Mistral accept `prompt_cache_key` to
    /// keep one conversation's requests on a single cache lane (Mistral's
    /// chat completions cache only reliably hits with it). `OpenRouter` uses
    /// `session_id` for sticky upstream routing with a 10-minute inactivity
    /// window. Codex carries it too: the Responses body keeps it, and the
    /// request sends it as the `session-id` header the `ChatGPT` backend routes on.
    fn add_prompt_cache_key(self, obj: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
        match self {
            Self::OpenRouter => {
                obj.insert("session_id".into(), serde_json::json!(key));
            }
            Self::OpenAi | Self::OpencodeGo | Self::OpenAiCodex | Self::Mistral => {
                obj.insert("prompt_cache_key".into(), serde_json::json!(key));
            }
        }
    }

    fn add_auto_cache_control(
        self,
        obj: &mut serde_json::Map<String, serde_json::Value>,
        model: &str,
    ) {
        if self == Self::OpenRouter && crate::provider::model_needs_cache_breakpoints(model) {
            obj.insert(
                "cache_control".into(),
                serde_json::json!({ "type": "ephemeral" }),
            );
        }
    }

    fn add_reasoning_effort(
        self,
        obj: &mut serde_json::Map<String, serde_json::Value>,
        effort: &str,
    ) {
        match self {
            Self::OpenRouter => {
                obj.insert("reasoning".into(), serde_json::json!({ "effort": effort }));
            }
            Self::OpenAi | Self::OpencodeGo | Self::Mistral => {
                obj.insert("reasoning_effort".into(), serde_json::json!(effort));
            }
            Self::OpenAiCodex => {
                obj.insert(
                    "reasoning".into(),
                    serde_json::json!({ "effort": effort, "summary": "auto" }),
                );
            }
        }
    }
}

/// Apply one TTL to every existing marker without changing cache boundaries.
fn add_cache_ttl_1h(body: &mut serde_json::Value, model: &str, enabled: bool) {
    if !enabled || !crate::provider::model_needs_cache_breakpoints(model) {
        return;
    }
    if let Some(cache_control) = body.get_mut("cache_control") {
        cache_control["ttl"] = serde_json::json!("1h");
    }
    for message in body["messages"].as_array_mut().into_iter().flatten() {
        for part in message["content"].as_array_mut().into_iter().flatten() {
            if let Some(cache_control) = part.get_mut("cache_control") {
                cache_control["ttl"] = serde_json::json!("1h");
            }
        }
    }
}

fn looks_like_openrouter_key(key: &str) -> bool {
    key.trim_start().starts_with("sk-or-")
}

fn looks_like_codex_token(key: &str) -> bool {
    crate::config::codex_account_id(key).is_ok()
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ImageModelsResponse {
    data: Vec<ImageModelEntry>,
}

#[derive(Deserialize)]
struct ImageModelEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    architecture: Option<Architecture>,
}

#[derive(Deserialize)]
struct VideoModelsResponse {
    data: Vec<VideoModelEntry>,
}

#[derive(Deserialize)]
struct VideoModelEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, Clone)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    #[serde(default)]
    reasoning: Option<ModelReasoningEntry>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    architecture: Option<Architecture>,
    #[serde(default)]
    pricing: Option<CatalogPricing>,
}

/// `OpenRouter` catalog pricing, in USD **per token** (stringly-typed in the
/// API: gpt-5 reports `1.25e-06`, i.e. $1.25/M). Only the `OpenRouter` flavor
/// reports it; others default to absent.
#[derive(Deserialize, Clone)]
struct CatalogPricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
    #[serde(default)]
    input_cache_read: Option<String>,
    #[serde(default)]
    input_cache_write: Option<String>,
}

impl CatalogPricing {
    /// Prices in USD per 1M tokens — the unit every cost calculation in this
    /// codebase uses (see `Db::request_cost`) — or `None` when either base
    /// token rate is absent or unparsable. Cache rates are optional: when a
    /// catalog omits one, the regular prompt rate applies. The API reports
    /// per-token values, so scale to per-1M once here.
    fn usd_per_million(&self) -> Option<ModelPricing> {
        let parse = |value: Option<&str>| value?.parse::<f64>().ok().map(|v| v * 1e6);
        Some(ModelPricing {
            prompt: parse(self.prompt.as_deref())?,
            completion: parse(self.completion.as_deref())?,
            cache_read: parse(self.input_cache_read.as_deref()),
            cache_write: parse(self.input_cache_write.as_deref()),
        })
    }
}

#[derive(Deserialize, Clone)]
struct ModelReasoningEntry {
    #[serde(default)]
    supported_efforts: Vec<String>,
}

#[derive(Deserialize, Clone)]
struct Architecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

/// Whether the catalog entry accepts image input.
fn entry_supports_images(e: &ModelEntry) -> bool {
    e.architecture
        .as_ref()
        .is_some_and(|a| a.input_modalities.iter().any(|m| m == "image"))
}

/// Whether the catalog entry generates image output.
fn entry_supports_image_gen(e: &ModelEntry) -> bool {
    // The API's output_modalities field is authoritative when populated.
    if e.architecture
        .as_ref()
        .is_some_and(|a| a.output_modalities.iter().any(|m| m == "image"))
    {
        return true;
    }
    // Fallback: match known image-generation model name patterns.
    // These are specific enough to avoid catching chat models from the same
    // provider, unlike broad provider-prefix matching.
    let id = &e.id;
    id.contains("/flux")
        || id.contains("dall-e")
        || id.contains("/stable-diffusion")
        || id == "recraft-20b"
        || id.starts_with("recraft-v")
        || id.contains("/imagen")
        || id.contains("/pixart")
        || id.contains("/playground-v")
        || id == "luma-photon"
        || id.starts_with("luma/")
        || id.starts_with("ideogram/")
        || id.contains("/sdxl")
        || id.contains("hyper-sd")
}

fn entry_supports_video_gen(e: &ModelEntry) -> bool {
    e.architecture
        .as_ref()
        .is_some_and(|a| a.output_modalities.iter().any(|m| m == "video"))
}

fn merge_generation_models(models: &mut Vec<Model>, additions: Vec<Model>) {
    for addition in additions {
        if let Some(existing) = models
            .iter_mut()
            .find(|m| m.backend == addition.backend && m.id == addition.id)
        {
            existing.supports_images |= addition.supports_images;
            existing.supports_image_generation |= addition.supports_image_generation;
            existing.supports_video_generation |= addition.supports_video_generation;
            if existing.name == existing.id && addition.name != addition.id {
                existing.name = addition.name;
            }
        } else {
            models.push(addition);
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
}

#[cfg(test)]
mod local_tests {
    use super::OpenRouter;

    #[test]
    fn local_routes_preserve_model_ids_and_omit_credentials() {
        for endpoint in [
            "http://localhost:11434/v1/",
            "http://localhost:1234/v1",
            "http://127.0.0.1:8080/v1",
        ] {
            let provider = OpenRouter::local(endpoint).expect("valid endpoint");
            let (base, model) = provider.opencode_route("mlx-community/model:latest");
            assert_eq!(base, endpoint.trim_end_matches('/'));
            assert_eq!(model, "mlx-community/model:latest");
            let request = provider
                .authenticate(provider.client.get(format!("{base}/models")))
                .build()
                .expect("valid request");
            assert!(!request.headers().contains_key("authorization"));
        }
    }

    #[test]
    fn local_rejects_unsafe_or_ambiguous_endpoints() {
        for endpoint in [
            "file:///tmp/model",
            "not a URL",
            "http://user:secret@localhost/v1",
            "http://localhost/v1?key=secret",
            "http://localhost/v1#fragment",
        ] {
            assert!(OpenRouter::local(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn cloud_authentication_is_unchanged() {
        let provider = OpenRouter::openai("test-key".into());
        let request = provider
            .authenticate(provider.client.get(provider.api_base()))
            .build()
            .expect("valid request");
        assert_eq!(request.headers()["authorization"], "Bearer test-key");
        assert_eq!(provider.api_base(), super::OPENAI_BASE);
    }
}

/// `OpenRouter` app attribution (openrouter.ai/docs/app-attribution): every
/// request names the app so its usage is registered under nexus-chat.
/// `HTTP-Referer` is the required identifier; the title (current header plus
/// the legacy `X-Title`) names it and the category files it. Sent only to
/// `OpenRouter`, never to `OpenAI`, Codex or local servers.
pub(crate) const OPENROUTER_ATTRIBUTION: [(&str, &str); 4] = [
    ("HTTP-Referer", env!("CARGO_PKG_REPOSITORY")),
    ("X-OpenRouter-Title", "nexus-chat"),
    ("X-Title", "nexus-chat"),
    ("X-OpenRouter-Categories", "cli-agent"),
];

fn openrouter_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in OPENROUTER_ATTRIBUTION {
        headers.insert(name, reqwest::header::HeaderValue::from_static(value));
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap_or_default()
}

impl OpenRouter {
    pub(crate) fn logged(mut self, log: Option<crate::db::UsageLog>, kind: &str) -> Self {
        self.usage_log = log.map(|log| Box::new((log, kind.to_string())));
        self
    }

    pub(crate) fn for_usage_session(mut self, session: &str) -> Self {
        if let Some((log, _)) = self.usage_log.as_deref_mut() {
            log.session_id = Some(session.to_string());
        }
        self
    }

    pub(crate) fn usage_kind(mut self, kind: &str) -> Self {
        if let Some((_, label)) = self.usage_log.as_deref_mut() {
            *label = kind.to_string();
        }
        self
    }

    /// Whether the interactive chat task records this client's usage itself.
    fn app_logs_usage(&self) -> bool {
        self.usage_log
            .as_deref()
            .is_some_and(|(_, kind)| kind == "chat")
    }

    /// The client a turn's sub-agents run on, logging their requests as
    /// `subagent` under the model they actually used.
    pub(crate) fn subagent_client(&self) -> Self {
        self.clone().usage_kind("subagent")
    }

    #[cfg(test)]
    pub(crate) fn usage_label(&self) -> Option<&str> {
        self.usage_log.as_deref().map(|(_, kind)| kind.as_str())
    }

    fn record_usage(&self, model: &str, usage: Option<Usage>) {
        if let Some((log, kind)) = self.usage_log.as_deref() {
            log.record(
                self.backend_tag().name(),
                model,
                usage.unwrap_or_default(),
                kind,
            );
        }
    }

    pub fn from_key_auto(key: String) -> Self {
        if looks_like_openrouter_key(&key) {
            Self::openrouter_flavor(key)
        } else if looks_like_codex_token(&key) {
            Self::openai_codex(key)
        } else {
            Self::openai(key)
        }
    }

    pub fn openrouter_flavor(key: String) -> Self {
        Self {
            client: openrouter_client(),
            #[cfg(test)]
            search_endpoint: None,
            key,
            flavor: ProviderFlavor::OpenRouter,
            endpoint: None,
            local_config: None,
            usage_log: None,
        }
    }

    pub fn openai(key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            key,
            flavor: ProviderFlavor::OpenAi,
            #[cfg(test)]
            search_endpoint: None,
            endpoint: None,
            local_config: None,
            usage_log: None,
        }
    }

    /// Connect to an explicitly configured OpenAI-compatible inference server.
    /// Cloud credentials are never reused for this endpoint.
    ///
    /// # Errors
    /// Returns an error for non-HTTP URLs, embedded credentials, queries or fragments.
    pub fn local(endpoint: &str) -> Result<Self> {
        let url = reqwest::Url::parse(endpoint).context("invalid local inference URL")?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "local inference URL must be HTTP(S) without credentials, query or fragment"
        );
        let mut provider = Self::openai(String::new());
        provider.endpoint = Some(url.as_str().trim_end_matches('/').to_string());
        Ok(provider)
    }

    /// Build a separate local backend with command-based model discovery.
    ///
    /// # Errors
    /// Returns an error if the runtime configuration is invalid.
    pub fn configured_local(config: super::local::LocalConfig) -> Result<Self> {
        config.validate()?;
        let mut provider = Self::local(config.endpoint())?;
        provider.local_config = Some(config);
        Ok(provider)
    }

    /// Non-secret backend/endpoint component of an embedding cache identity.
    pub(super) fn embedding_backend_id(&self) -> String {
        format!("{}:{}", self.backend_tag().key_prefix(), self.api_base())
    }

    fn api_base(&self) -> &str {
        self.endpoint
            .as_deref()
            .unwrap_or_else(|| self.flavor.base())
    }

    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.key.is_empty() {
            request
        } else {
            request.bearer_auth(&self.key)
        }
    }

    pub fn opencode_go(key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            key,
            flavor: ProviderFlavor::OpencodeGo,
            #[cfg(test)]
            search_endpoint: None,
            endpoint: None,
            local_config: None,
            usage_log: None,
        }
    }

    /// Mistral's API with a Vibe plan key or a console key.
    pub fn mistral(key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            key,
            flavor: ProviderFlavor::Mistral,
            #[cfg(test)]
            search_endpoint: None,
            endpoint: None,
            local_config: None,
            usage_log: None,
        }
    }

    pub fn openai_codex(key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            key,
            flavor: ProviderFlavor::OpenAiCodex,
            #[cfg(test)]
            search_endpoint: None,
            endpoint: None,
            local_config: None,
            usage_log: None,
        }
    }

    /// True when this client points at a user-configured local inference
    /// server rather than a hosted backend. Local runtimes are the ones
    /// that may leave Harmony framing in `content` (see `provider::harmony`).
    const fn is_local(&self) -> bool {
        self.endpoint.is_some()
    }

    pub const fn backend_tag(&self) -> crate::provider::BackendTag {
        if self.is_local() {
            return crate::provider::BackendTag::Local;
        }
        match self.flavor {
            ProviderFlavor::OpenRouter => crate::provider::BackendTag::OpenRouter,
            ProviderFlavor::OpenAi => crate::provider::BackendTag::OpenAi,
            ProviderFlavor::OpenAiCodex => crate::provider::BackendTag::Codex,
            ProviderFlavor::OpencodeGo => crate::provider::BackendTag::OpencodeGo,
            ProviderFlavor::Mistral => crate::provider::BackendTag::Mistral,
        }
    }

    /// Submit one non-streaming wave to the `OpenRouter` Decisions API.
    ///
    /// # Errors
    /// Returns provider, timeout, HTTP or response parsing errors.
    pub async fn decide(&self, body: &serde_json::Value) -> Result<DecisionsResponse> {
        anyhow::ensure!(self.is_openrouter(), "decisions require OpenRouter");
        let base = self.api_base().trim_end_matches("/v1");
        let response: DecisionsResponse = self
            .authenticate(self.client.post(format!("{base}/alpha/decisions")))
            .timeout(std::time::Duration::from_secs(10))
            .json(body)
            .send()
            .await
            .context("decisions request")?
            .error_for_status()
            .context("decisions failed")?
            .json()
            .await
            .context("parsing decisions")?;
        self.record_usage(
            body["model"].as_str().unwrap_or_default(),
            Some(Usage {
                cost: response.usage.cost,
                ..Usage::default()
            }),
        );
        Ok(response)
    }

    /// Video gen is OpenRouter-only (not OpenAI/Codex/OpenCode).
    pub fn is_openrouter(&self) -> bool {
        self.flavor == ProviderFlavor::OpenRouter
    }

    /// Generate a video via `OpenRouter`'s `/api/v1/videos` endpoint.
    /// Submits the job, polls every 10s (up to 6 min), downloads on completion.
    /// Returns `(mp4_bytes, cost_in_usd)`.
    // Long by design (request assembly + polling).
    #[allow(clippy::too_many_lines)]
    pub async fn generate_video(&self, req: VideoRequest) -> Result<(Vec<u8>, f64)> {
        let VideoRequest {
            model,
            prompt,
            duration,
            resolution,
            aspect_ratio,
            generate_audio,
            first_frame,
            last_frame,
            input_references,
            seed,
            provider_options,
        } = req;
        if !self.is_openrouter() {
            anyhow::bail!("video generation only available on the OpenRouter backend");
        }
        let (base, model_id) = self.opencode_route(&model);
        let (duration, resolution, aspect_ratio) =
            normalize_video_params(&model_id, duration, &resolution, &aspect_ratio);
        check_video_params(&model_id, duration, &resolution, &aspect_ratio)?;

        let mut body = serde_json::json!({
            "model": model_id,
            "prompt": prompt,
            "duration": duration,
            "resolution": resolution,
            "aspect_ratio": aspect_ratio,
            "generate_audio": generate_audio,
        });

        let mut frames = Vec::new();
        if let Some(data) = first_frame {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            let mime = Self::detect_image_mime(&data);
            frames.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{b64}") },
                "frame_type": "first_frame",
            }));
        }
        if let Some(data) = last_frame {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            let mime = Self::detect_image_mime(&data);
            frames.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{b64}") },
                "frame_type": "last_frame",
            }));
        }
        if !frames.is_empty() {
            body["frame_images"] = serde_json::Value::Array(frames);
        }

        if !input_references.is_empty() {
            let refs: Vec<serde_json::Value> = input_references
                .iter()
                .map(|data| {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                    let mime = Self::detect_image_mime(data);
                    serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{mime};base64,{b64}") }
                    })
                })
                .collect();
            body["input_references"] = serde_json::Value::Array(refs);
        }

        if let Some(s) = seed {
            body["seed"] = serde_json::json!(s);
        }

        if let Some(po) = provider_options {
            body["provider"] = serde_json::json!({ "options": po });
        }

        let raw_resp = self
            .client
            .post(format!("{base}/videos"))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .context("video generation request")?;
        let resp: serde_json::Value = if raw_resp.status().is_success() {
            raw_resp
                .json::<serde_json::Value>()
                .await
                .context("parsing video submission response")?
        } else {
            let status = raw_resp.status();
            let body_text = raw_resp.text().await.unwrap_or_default();
            anyhow::bail!("video generation submission failed (HTTP {status}): {body_text}");
        };

        let job_id = resp
            .get("id")
            .and_then(|i| i.as_str())
            .context("no job id in video generation response")?
            .to_string();

        let poll_url = format!("{base}/videos/{job_id}");
        for _attempt in 0..36 {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;

            let status = self
                .client
                .get(&poll_url)
                .bearer_auth(&self.key)
                .send()
                .await
                .context("video status poll")?
                .error_for_status()
                .context("video status poll failed")?
                .json::<serde_json::Value>()
                .await
                .context("parsing video status response")?;

            match status.get("status").and_then(|s| s.as_str()) {
                Some("completed") => {
                    let cost = status
                        .pointer("/usage/cost")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0);

                    self.clone()
                        .usage_kind("generate video")
                        .record_usage(&model, status.get("usage").and_then(parse_usage_value));
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                    let content = self
                        .client
                        .get(format!("{base}/videos/{job_id}/content?index=0"))
                        .bearer_auth(&self.key)
                        .send()
                        .await
                        .context("video download request")?
                        .error_for_status()
                        .context("video download failed")?
                        .bytes()
                        .await
                        .context("reading video content")?;

                    return Ok((content.to_vec(), cost));
                }
                Some("failed") => {
                    let err = status
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown error");
                    anyhow::bail!("video generation failed: {err}");
                }
                Some("cancelled" | "expired") => {
                    anyhow::bail!("video generation was cancelled or expired");
                }
                _ => {}
            }
        }

        anyhow::bail!("video generation timed out after 6 minutes");
    }

    /// The "auto" helper model for `tag`: the cheap model every background job
    /// (memory, compaction, image descriptions, vision OCR, the auto-approval
    /// judge) uses when the user hasn't picked one. This is the one place the
    /// per-backend choice lives. Empty means the backend has no cheap option
    /// of its own (local runtimes serve whatever the user installed), so the
    /// caller falls back to the session's chat model on that backend. These
    /// are floors: `App::newest_listed` upgrades each to the newest version
    /// of the same family the live catalog lists (`gpt-6-luna` → `gpt-6.1-luna`).
    #[must_use]
    pub const fn default_helper_for(tag: crate::provider::BackendTag) -> &'static str {
        use crate::provider::BackendTag;
        match tag {
            BackendTag::OpenRouter => "openai/gpt-6-luna",
            BackendTag::OpenAi | BackendTag::Codex | BackendTag::OpencodeGo => "gpt-6-luna",
            BackendTag::Mistral => MISTRAL_VIBE_FAST_MODEL,
            BackendTag::Local => "",
        }
    }

    pub const fn default_utility_model(&self) -> &'static str {
        Self::default_helper_for(self.backend_tag())
    }

    pub const fn default_research_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "openai/gpt-6-sol",
            ProviderFlavor::OpenAi | ProviderFlavor::OpenAiCodex => "gpt-6-sol",
            ProviderFlavor::OpencodeGo => "glm-5.3",
            ProviderFlavor::Mistral => MISTRAL_VIBE_MODEL,
        }
    }

    pub const fn default_embedding_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "google/gemini-embedding-2",
            ProviderFlavor::OpenAi => "text-embedding-3-small",
            // Codex, Go and a Vibe plan cover chat/coding models, no embeddings.
            ProviderFlavor::OpenAiCodex | ProviderFlavor::OpencodeGo | ProviderFlavor::Mistral => {
                ""
            }
        }
    }

    pub const fn default_video_gen_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "google/veo-3.1",
            _ => "",
        }
    }

    pub const fn default_image_gen_model(&self) -> &'static str {
        match self.flavor {
            ProviderFlavor::OpenRouter => "openai/gpt-image-2",
            ProviderFlavor::OpenAi => "dall-e-3",
            ProviderFlavor::OpenAiCodex | ProviderFlavor::OpencodeGo | ProviderFlavor::Mistral => {
                ""
            }
        }
    }

    /// Fetch the live model catalog. No hardcoded list except Codex, whose
    /// subscription endpoint does not expose the normal `OpenAI` models catalog.
    // Long by design (catalog merge).
    #[allow(clippy::too_many_lines)]
    pub async fn list_models(&self) -> Result<Vec<Model>> {
        if let Some(config) = &self.local_config {
            return config.list_models().await;
        }
        if self.flavor == ProviderFlavor::OpenAiCodex {
            // The live catalog, like the Codex CLI's; the built-in list only
            // covers a failed fetch. Codex models stay unmerged with
            // OpenRouter's (Ctrl+P switches backends): a few hundred entries
            // would bury them alphabetically.
            let cache = read_codex_cache();
            return Ok(
                match self
                    .fetch_codex_catalog(&codex_client_version(cache.as_ref()))
                    .await
                {
                    Ok(models) if !models.is_empty() => models,
                    _ => cache
                        .as_ref()
                        .map(parse_codex_catalog)
                        .filter(|models| !models.is_empty())
                        .unwrap_or_else(codex_fallback_models),
                },
            );
        }
        if self.flavor == ProviderFlavor::OpencodeGo {
            // Two distinct catalogs behind the same account key: Zen
            // general (free + pay-per-token) and the flat-fee Go bundle.
            // Fetch both and show them together — Go entries tagged so
            // requests route (and bill) correctly; see `opencode_route`.
            let (zen, go) = tokio::join!(
                self.fetch_models_from(OPENCODE_ZEN_BASE),
                self.fetch_models_from(OPENCODE_GO_BASE),
            );
            // Zen general is the primary catalog — a failure there is a
            // real problem (bad key, network) and should surface. Go's
            // bundle can legitimately 403 for an account without that
            // subscription; treat it as "no Go models" rather than an error.
            let zen = zen?;
            let go = go.unwrap_or_default();
            let go_ids: std::collections::HashSet<&str> =
                go.iter().map(|m| m.id.as_str()).collect();
            let mut models: Vec<Model> = zen
                .into_iter()
                // A model covered by the flat-fee Go bundle is strictly
                // better than its metered Zen-general twin — don't show both.
                .filter(|m| !go_ids.contains(m.id.as_str()))
                .collect();
            models.extend(go.into_iter().map(|mut m| {
                m.id = format!("{OPENCODE_GO_PREFIX}{}", m.id);
                m
            }));
            models.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(models);
        }
        if self.flavor == ProviderFlavor::Mistral {
            let catalog = self
                .authenticate(self.client.get(format!("{}/models", self.api_base())))
                .send()
                .await
                .context("requesting Mistral model list")?
                .error_for_status()
                .context("Mistral model list request failed")?
                .json::<MistralModelsResponse>()
                .await
                .context("parsing Mistral model list")?;
            return Ok(parse_mistral_catalog(catalog));
        }
        let mut models = self.fetch_models_from(self.api_base()).await?;
        if self.flavor == ProviderFlavor::OpenRouter {
            // The general catalog is not authoritative for generation models.
            // Merge OpenRouter's dedicated catalogs into the existing picker.
            let (images, videos) =
                tokio::join!(self.fetch_image_models(), self.fetch_video_models(),);
            merge_generation_models(&mut models, images.unwrap_or_default());
            merge_generation_models(&mut models, videos.unwrap_or_default());
        }
        Ok(models)
    }

    async fn fetch_image_models(&self) -> Result<Vec<Model>> {
        let response = self
            .client
            .get(format!("{OPENROUTER_BASE}/images/models"))
            .bearer_auth(&self.key)
            .send()
            .await
            .context("requesting image model list")?
            .error_for_status()
            .context("image model list request failed")?
            .json::<ImageModelsResponse>()
            .await
            .context("parsing image model list")?;

        Ok(response
            .data
            .into_iter()
            .map(|m| {
                let id = m.id;
                Model {
                    name: m.name.unwrap_or_else(|| id.clone()),
                    reasoning_efforts: Vec::new(),
                    context_length: None,
                    supports_images: m
                        .architecture
                        .as_ref()
                        .is_some_and(|a| a.input_modalities.iter().any(|v| v == "image")),
                    supports_image_generation: true,
                    supports_video_generation: false,
                    backend: crate::provider::BackendTag::OpenRouter,
                    id,
                    pricing: None,
                }
            })
            .collect())
    }

    async fn fetch_video_models(&self) -> Result<Vec<Model>> {
        let response = self
            .client
            .get(format!("{OPENROUTER_BASE}/videos/models"))
            .bearer_auth(&self.key)
            .send()
            .await
            .context("requesting video model list")?
            .error_for_status()
            .context("video model list request failed")?
            .json::<VideoModelsResponse>()
            .await
            .context("parsing video model list")?;

        Ok(response
            .data
            .into_iter()
            .map(|m| {
                let id = m.id;
                Model {
                    name: m.name.unwrap_or_else(|| id.clone()),
                    reasoning_efforts: Vec::new(),
                    context_length: None,
                    supports_images: false,
                    supports_image_generation: false,
                    supports_video_generation: true,
                    backend: crate::provider::BackendTag::OpenRouter,
                    id,
                    pricing: None,
                }
            })
            .collect())
    }

    /// GET `{base}/models` and map the response into our `Model` type using
    /// this instance's flavor for reasoning/image-support inference.
    async fn fetch_models_from(&self, base: &str) -> Result<Vec<Model>> {
        let resp = self
            .authenticate(self.client.get(format!("{base}/models")))
            .send()
            .await
            .context("requesting model list")?
            .error_for_status()
            .context("model list request failed")?
            .json::<ModelsResponse>()
            .await
            .context("parsing model list")?;

        let mut models: Vec<Model> = resp
            .data
            .into_iter()
            .map(|m| {
                let reasoning_efforts = self.flavor.reasoning_efforts(&m);
                let supports_images = self
                    .flavor
                    .supports_images(&m)
                    .unwrap_or_else(|| entry_supports_images(&m));
                let supports_image_generation = self
                    .flavor
                    .supports_image_generation(&m)
                    .unwrap_or_else(|| entry_supports_image_gen(&m));
                let supports_video_generation =
                    self.flavor == ProviderFlavor::OpenRouter && entry_supports_video_gen(&m);
                Model {
                    name: m.name.unwrap_or_else(|| m.id.clone()),
                    reasoning_efforts,
                    context_length: m.context_length,
                    id: m.id,
                    supports_images,
                    supports_image_generation,
                    supports_video_generation,
                    backend: self.backend_tag(),
                    pricing: m.pricing.and_then(|p| p.usd_per_million()),
                }
            })
            .collect();
        // The OpenCode Zen endpoints return no context metadata at all; fill
        // in the known windows per endpoint (the same id can differ between
        // the general catalog and the Go bundle — see the table comments).
        // An API-provided value always wins over the fallback table.
        for m in &mut models {
            if m.context_length.is_none() {
                m.context_length = Self::opencode_context_fallback(base, &m.id);
            }
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    /// Fallback context window for an `OpenCode` model id, keyed by endpoint
    /// (Zen general vs Go bundle). `None` for non-OpenCode bases and for ids
    /// not in the table.
    fn opencode_context_fallback(base: &str, id: &str) -> Option<u64> {
        let table = match base {
            OPENCODE_ZEN_BASE => OPENCODE_ZEN_CONTEXT,
            OPENCODE_GO_BASE => OPENCODE_GO_CONTEXT,
            _ => return None,
        };
        table.iter().find(|(tid, _)| *tid == id).map(|(_, c)| *c)
    }

    /// One-shot, non-streaming completion. Used for short utility calls like
    /// generating a session topic/slug. Returns the assistant's message text.
    pub async fn complete(&self, model: &str, messages: Vec<ChatMessage>) -> Result<String> {
        self.complete_with_usage(model, messages)
            .await
            .map(|completion| completion.text)
    }

    /// One-shot completion with the provider's usage object when available.
    /// Keeping usage attached to the text lets background workflow stages use
    /// the same cache accounting as the interactive stream.
    pub async fn complete_with_usage(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<Completion> {
        self.complete_with_params(model, messages, &ChatParams::default())
            .await
    }

    /// One-shot completion with request parameters and usage accounting.
    pub async fn complete_with_params(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        params: &ChatParams,
    ) -> Result<Completion> {
        let completion = self.complete_unlogged(model, messages, params).await?;
        self.record_usage(model, completion.usage);
        Ok(completion)
    }

    async fn complete_unlogged(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        params: &ChatParams,
    ) -> Result<Completion> {
        // ChatGPT's Codex endpoint is stream-oriented: non-streaming requests
        // are rejected or can return no usable output. Utility jobs (titles,
        // memory, compaction, research metadata) still need a one-shot string,
        // so collect the same proven SSE path used by interactive chat.
        if self.flavor == ProviderFlavor::OpenAiCodex {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let finish = self
                .run_codex_stream(model, &messages, params, &[], false, &tx)
                .await?;
            drop(tx);

            let mut text = String::new();
            let mut error = None;
            let mut usage = None;
            while let Ok(event) = rx.try_recv() {
                match event {
                    StreamEvent::Token(token) => text.push_str(&token),
                    StreamEvent::Error(message) => error = Some(message),
                    StreamEvent::Usage(next) => merge_usage(&mut usage, next),
                    _ => {}
                }
            }
            if matches!(finish, Finish::Errored) {
                anyhow::bail!(error.unwrap_or_else(|| "Codex completion failed".to_string()));
            }
            return Ok(Completion { text, usage });
        }

        // Same Harmony repair the streaming path does: utility jobs
        // (titles, memory, compaction) replay recorded assistant turns too.
        let messages = if self.is_local() {
            harmony::sanitize_history(&messages).unwrap_or(messages)
        } else {
            messages
        };
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });
        if let Some(key) = &params.prompt_cache_key
            && let Some(obj) = body.as_object_mut()
        {
            self.flavor.add_prompt_cache_key(obj, key);
        }
        add_cache_ttl_1h(&mut body, model, params.cache_ttl_1h);
        self.post_completion_with_usage(body).await
    }

    /// POST a non-streaming completions body while retaining usage accounting.
    async fn post_completion_with_usage(&self, body: serde_json::Value) -> Result<Completion> {
        if let Some(delegate) = self.openrouter_delegate_for_body(&body) {
            return Box::pin(delegate.post_completion_with_usage(body)).await;
        }
        if self.flavor == ProviderFlavor::OpenAiCodex {
            let body = chat_body_to_codex_body(&body, false, &[]);
            let key = body.get("prompt_cache_key").and_then(|key| key.as_str());
            let v = self
                .client
                .post(format!("{}/codex/responses", self.flavor.base()))
                .headers(codex_cache_headers(self.codex_headers(false)?, key, None))
                .json(&body)
                .send()
                .await
                .context("Codex completion request")?
                .error_for_status()
                .context("Codex completion failed")?
                .json::<serde_json::Value>()
                .await
                .context("parsing Codex completion")?;
            return Ok(Completion {
                text: codex_response_text(&v),
                usage: v.get("usage").and_then(parse_usage_value),
            });
        }
        let mut body = body;
        if self.flavor == ProviderFlavor::Mistral {
            mistral_body(&mut body);
        }
        let base = if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
            let (base, real_model) = self.opencode_route(model);
            if let Some(obj) = body.as_object_mut() {
                obj.insert("model".into(), serde_json::json!(real_model));
            }
            base
        } else {
            self.api_base()
        };
        let v = self
            .authenticate(self.client.post(format!("{base}/chat/completions")))
            .json(&body)
            .send()
            .await
            .context("completion request")?
            .error_for_status()
            .context("completion failed")?
            .json::<serde_json::Value>()
            .await
            .context("parsing completion")?;
        let text = v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(wire_text)
            .unwrap_or_default();
        // A local runtime may hand back a whole Harmony turn here; the
        // caller wants the answer, not the framing around it.
        let text = match self.is_local().then(|| harmony::sanitize(&text)).flatten() {
            Some(visible) => visible,
            None => text,
        };
        Ok(Completion {
            text,
            usage: v.get("usage").and_then(parse_usage_value),
        })
    }

    /// Generate an image via `OpenRouter`'s dedicated `/api/v1/images` endpoint.
    /// Returns `(decoded_bytes, file_extension)` — the caller should use the
    /// returned extension (e.g. `"png"`, `"jpg"`, `"webp"`) for the saved file.
    pub async fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        size: &str,
        image_data: Option<&[u8]>,
    ) -> Result<(Vec<u8>, String)> {
        if let Some(delegate) = self.openrouter_delegate_for_model(model) {
            return Box::pin(delegate.generate_image(model, prompt, size, image_data)).await;
        }
        if self.flavor == ProviderFlavor::OpenAiCodex {
            anyhow::bail!("image generation not supported on Codex");
        }
        let (base, model) = self.opencode_route(model);

        let mut body = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "n": 1,
        });
        if self.flavor == ProviderFlavor::OpenAi {
            // DALL-E uses OpenAI's legacy pixel-size parameter.
            body["size"] = serde_json::json!(size);
        } else {
            // OpenRouter's image catalog uses normalized capabilities; a
            // fixed 1024x1024 size isn't accepted by several newer models.
            body["aspect_ratio"] = serde_json::json!(Self::image_aspect_ratio(size));
            if model.starts_with("google/gemini-3") {
                body["resolution"] = serde_json::json!(Self::image_resolution(size));
            }
            if model.starts_with("openai/gpt-image") || model.starts_with("openai/gpt-5-image") {
                body["quality"] = serde_json::json!("high");
            }
        }
        if let Some(img) = image_data {
            let b64 = base64::engine::general_purpose::STANDARD.encode(img);
            let mime = Self::detect_image_mime(img);
            body["input_references"] = serde_json::json!([{
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{b64}") }
            }]);
        }
        let v = self
            .client
            .post(format!("{base}/images"))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .context("image generation request")?
            .error_for_status()
            .context("image generation failed")?
            .json::<serde_json::Value>()
            .await
            .context("parsing image generation response")?;
        self.clone()
            .usage_kind("generate image")
            .record_usage(model.as_str(), v.get("usage").and_then(parse_usage_value));
        let data = v
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .context("no image data in response")?;
        let b64 = data
            .get("b64_json")
            .and_then(|b| b.as_str())
            .context("no b64_json field")?;
        let media_type = data
            .get("media_type")
            .and_then(|m| m.as_str())
            .unwrap_or("image/png");
        let ext = match media_type {
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            "image/gif" => "gif",
            "image/svg+xml" => "svg",
            _ => "png",
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .context("base64 decode")?;
        Ok((bytes, ext.to_string()))
    }

    /// Detect image MIME type from magic bytes.
    fn detect_image_mime(data: &[u8]) -> &'static str {
        if data.len() < 4 {
            return "image/png";
        }
        if data[0] == 0x89 && data[1] == b'P' && data[2] == b'N' && data[3] == b'G' {
            "image/png"
        } else if data[0] == 0xFF && data[1] == 0xD8 {
            "image/jpeg"
        } else if data[0] == b'G' && data[1] == b'I' && data[2] == b'F' {
            "image/gif"
        } else if data[0] == b'R' && data[1] == b'I' && data[2] == b'F' && data[3] == b'F' {
            "image/webp"
        } else {
            "image/png"
        }
    }

    fn image_aspect_ratio(size: &str) -> &'static str {
        match size {
            "1024x1792" => "9:16",
            "1792x1024" => "16:9",
            _ => "1:1",
        }
    }

    fn image_resolution(size: &str) -> &'static str {
        let max_dimension = size
            .split('x')
            .filter_map(|value| value.parse::<u32>().ok())
            .max()
            .unwrap_or(1024);
        if max_dimension >= 3000 {
            "4K"
        } else if max_dimension >= 1800 {
            "2K"
        } else {
            "1K"
        }
    }

    /// Embed `inputs` with `model` (OpenAI-format /embeddings endpoint);
    /// returns one vector per input, in order.
    pub async fn embed(&self, model: &str, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.embed_values(
            model,
            inputs.into_iter().map(serde_json::Value::String).collect(),
        )
        .await
    }

    /// Embed text or multimodal `inputs` with `model` (OpenAI-format /embeddings endpoint);
    /// returns one vector per input, in order.
    pub async fn embed_values(
        &self,
        model: &str,
        inputs: Vec<serde_json::Value>,
    ) -> Result<Vec<Vec<f32>>> {
        if let Some(delegate) = self.openrouter_delegate_for_model(model) {
            return Box::pin(delegate.embed_values(model, inputs)).await;
        }
        let (base, model) = self.opencode_route(model);
        let count = inputs.len();
        let v = self
            .authenticate(self.client.post(format!("{base}/embeddings")))
            .timeout(std::time::Duration::from_secs(120))
            .json(&serde_json::json!({ "model": model, "input": inputs }))
            .send()
            .await
            .context("embeddings request")?
            .error_for_status()
            .context("embeddings failed")?
            .json::<serde_json::Value>()
            .await
            .context("parsing embeddings")?;
        if let Some((log, _)) = self.usage_log.as_deref()
            && let Some(usage) = v.get("usage").and_then(parse_usage_value)
        {
            log.record(self.backend_tag().name(), &model, usage, "embed");
        }
        super::embedding::indexed_vectors(&v, count, None)
    }

    /// Start a streaming completion. Spawns a task that pushes tokens over the
    /// returned channel; the UI loop drains it alongside keypresses. If the
    /// model calls a tool, the task runs it via `toolbox` and continues the
    /// conversation, bounded by `max_tool_iters` round-trips.
    pub fn stream_chat(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        params: ChatParams,
        tools: Vec<ToolDef>,
        toolbox: Arc<dyn ToolExecutor>,
        max_tool_iters: usize,
    ) -> (
        mpsc::UnboundedReceiver<StreamEvent>,
        tokio::task::AbortHandle,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let this = self.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = this
                .run_chat_loop(model, messages, params, tools, toolbox, max_tool_iters, &tx)
                .await
            {
                let _ = tx.send(StreamEvent::Error(e.to_string()));
            }
        });
        (rx, task.abort_handle())
    }

    /// One chat round-trip plus tool iterations. Args mix request data
    /// (model/messages/params/tools) with execution plumbing (toolbox,
    /// budget, event sink) — a struct would group unrelated concerns.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn run_chat_loop(
        &self,
        model: String,
        mut messages: Vec<ChatMessage>,
        params: ChatParams,
        tools: Vec<ToolDef>,
        toolbox: Arc<dyn ToolExecutor>,
        max_tool_iters: usize,
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<()> {
        // Interactive chat persists accounting in App (including its two-event
        // compatibility path and the usage sub-agents forward on `tx`), so
        // neither its rounds nor its sub-agents log here; other callers'
        // sub-agents log independently.
        let app_logs = self.app_logs_usage();
        let toolbox = toolbox
            .clone()
            .with_subagents(Arc::new(crate::subagent::Context::new(
                self.subagent_client(),
                model.clone(),
                params.clone(),
                tx.clone(),
            )))
            .unwrap_or(toolbox);
        // Dedup state for tool results: (tool, arguments) → latest full
        // result. Seeded from the incoming history (which `build_history`
        // already deduped with the same rule), so a result that duplicates
        // anything the model has seen — this turn or earlier turns — is
        // compressed before it ever enters the context, and the wire
        // history stays byte-identical to what the next turn's replay will
        // rebuild (prompt-cache continuity).
        let mut seen_results = super::seed_tool_result_dedup(&messages);
        let stream_provider = if app_logs {
            self.clone().logged(None, "")
        } else {
            self.clone()
        };
        for iter in 0..=max_tool_iters {
            // Keep the tool schema stable while the loop grows. On the final
            // request, disable selection without removing the schemas or
            // appending a transient budget prompt; the next turn can still
            // reconstruct the same message prefix.
            let request_len = messages.len();
            let started = std::time::Instant::now();
            let finish = stream_provider
                .run_stream(
                    &model,
                    &messages,
                    &params,
                    &tools,
                    iter == max_tool_iters,
                    tx,
                )
                .await?;
            let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (output, calls, tool_results) = match finish {
                Finish::Errored => return Ok(()),
                Finish::Done(output) => (output, Vec::new(), Vec::new()),
                Finish::ToolCalls(calls, output) => {
                    let content = &output.content;
                    let reasoning = &output.reasoning;
                    let mut tool_results = Vec::new();
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: content.clone(),
                        reasoning_content: reasoning.clone(),
                        tool_calls: Some(calls.clone()),
                        ..Default::default()
                    });
                    // Parallel calls the model issued in one response run in
                    // waves of non-conflicting effects: reads, fetches and
                    // writes to different files overlap instead of stacking,
                    // while writes to the same file and program runs keep
                    // their order. Results are zipped back into call order.
                    let pending: Vec<(String, String)> = calls
                        .iter()
                        .map(|call| (call.name.clone(), call.arguments.clone()))
                        .collect();
                    let results = crate::tools::run_scheduled(&*toolbox, &pending).await;
                    for (index, (call, (result, status))) in calls.iter().zip(results).enumerate() {
                        let _ = tx.send(StreamEvent::Status("Running tool…".to_string()));
                        let _ = tx.send(StreamEvent::Status(status));
                        let _ = tx.send(StreamEvent::ToolCall {
                            id: call.id.clone(),
                            reasoning: (index == 0).then(|| reasoning.clone()).flatten(),
                            assistant_content: (index == 0 && !content.is_empty())
                                .then(|| content.clone()),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                            result: result.clone(),
                        });
                        // A result byte-identical to an earlier call with
                        // the same tool and arguments (e.g. re-reading an
                        // unchanged file) is replaced by a one-line note so
                        // the duplicate never re-enters the context. The
                        // transcript above still carries the full result and
                        // the db stores it; `build_history` applies the same
                        // rule on replay, keeping the cache prefix intact.
                        let key = (call.name.clone(), call.arguments.clone());
                        // Compare capped content, as seeded from replayed history.
                        let visible = params.tool_result_cap.map_or_else(
                            || result.clone(),
                            |cap| crate::tools::cap_head_tail(&result, cap),
                        );
                        let content = match seen_results.get(&key) {
                            Some(prev) if *prev == visible => {
                                crate::tools::tool_result_unchanged_note(
                                    &call.name,
                                    &call.arguments,
                                )
                            }
                            _ => {
                                seen_results.insert(key, visible.clone());
                                visible
                            }
                        };
                        if params.trace.is_some() {
                            tool_results.push(TraceToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                full: result.clone(),
                                seen: content.clone(),
                            });
                        }
                        messages.push(ChatMessage {
                            role: "tool".to_string(),
                            content,
                            tool_call_id: Some(call.id.clone()),
                            ..Default::default()
                        });
                        // If the model supports images, load image references
                        // from the tool result and inject them as a user
                        // message so the vision model can see them on the
                        // next stream request.
                        if toolbox.supports_images()
                            && let Some(files_dir) = toolbox.space_files_dir()
                            && let Some(imgs) = extract_tool_images(&result, &files_dir)
                        {
                            messages.push(ChatMessage {
                                role: "user".to_string(),
                                content: imgs.description,
                                images: imgs.urls,
                                ..Default::default()
                            });
                        }
                    }
                    (output, calls, tool_results)
                }
            };
            let done = calls.is_empty();
            if let Some(context) = &params.trace {
                let request = &messages[..request_len];
                let repaired = self
                    .is_local()
                    .then(|| harmony::sanitize_history(request))
                    .flatten();
                let request = repaired.as_deref().unwrap_or(request);
                let (system, request) = match request.split_first() {
                    Some((first, rest)) if first.role == "system" => (first.content.clone(), rest),
                    _ => (String::new(), request),
                };
                let mut sampling = serde_json::json!({
                    "reasoning_effort": params.reasoning_effort,
                    "temperature": params.temperature,
                    "top_p": params.top_p,
                    "max_tokens": params.max_tokens,
                });
                sampling
                    .as_object_mut()
                    .unwrap()
                    .retain(|_, value| !value.is_null());
                let step = TraceStep {
                    session_id: context.session_id.clone(),
                    space_id: context.space_id.clone(),
                    turn_id: context.turn_id.clone(),
                    round: u32::try_from(iter).unwrap_or(u32::MAX),
                    model: self.opencode_route(&model).1,
                    backend: context.backend.clone(),
                    system,
                    tools: tools.clone(),
                    messages: request.to_vec(),
                    params: sampling,
                    response: TraceResponse {
                        content: output.content,
                        reasoning: output.reasoning,
                        tool_calls: calls,
                        finish: if done { "stop" } else { "tool_calls" }.to_string(),
                    },
                    usage: output.usage,
                    latency_ms,
                    tool_results,
                };
                let store = context.store.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = store.record_step(&step);
                });
            }
            if done {
                let _ = tx.send(StreamEvent::Done);
                return Ok(());
            }
        }
        let _ = tx.send(StreamEvent::Done);
        Ok(())
    }

    pub(crate) async fn subagent_round(
        &self,
        request: crate::subagent::Request<'_>,
    ) -> Result<crate::subagent::Round> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run = async {
            let tx = tx;
            self.run_stream(
                request.model,
                request.messages,
                request.params,
                request.tools,
                request.final_round,
                &tx,
            )
            .await
        };
        // Stream the round's text to the live view while it runs; the
        // channel closes when `run` drops its sender.
        let forward = async {
            let mut error = None;
            while let Some(event) = rx.recv().await {
                match event {
                    StreamEvent::Reasoning(text) => (request.live)(&text, ""),
                    StreamEvent::Token(text) => (request.live)("", &text),
                    StreamEvent::Error(text) => error = Some(text),
                    _ => {}
                }
            }
            error
        };
        let (finish, error) = tokio::join!(run, forward);
        let (calls, output) = match finish? {
            Finish::Done(output) => (Vec::new(), output),
            Finish::ToolCalls(calls, output) => (calls, output),
            Finish::Errored => {
                anyhow::bail!(
                    "{}",
                    error.as_deref().unwrap_or("sub-agent provider failed")
                );
            }
        };
        Ok(crate::subagent::Round {
            content: output.content,
            reasoning: output.reasoning,
            calls,
            usage: output.usage,
        })
    }

    fn chat_completions_body(
        &self,
        model: &str,
        messages: &[ChatMessage],
        params: &ChatParams,
        tools: &[ToolDef],
        disable_tool_choice: bool,
    ) -> serde_json::Value {
        // A local runtime that leaves Harmony framing in `content` also
        // refuses to take it back ("you have passed a message containing
        // <|channel|> tags"), which wedges every later turn of a session
        // recorded before the demux existed. Repair the replay.
        let repaired = self
            .is_local()
            .then(|| harmony::sanitize_history(messages))
            .flatten();
        let messages = repaired.as_deref().unwrap_or(messages);
        // Local chat templates (Qwen, Llama, Mistral) accept one system
        // message and only as the first; llama.cpp's Jinja raises "System
        // message must be at the beginning" on any other.
        let single_system = self
            .is_local()
            .then(|| single_leading_system(messages))
            .flatten();
        let messages = single_system.as_deref().unwrap_or(messages);
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });
        let obj = body.as_object_mut().expect("body is a json object");
        self.flavor.add_stream_usage(obj);
        if let Some(key) = &params.prompt_cache_key {
            self.flavor.add_prompt_cache_key(obj, key);
        }
        // mark_cache_breakpoints marks the system block and pre-tail; this
        // automatic marker makes 3 total, below Anthropic's limit of 4.
        self.flavor.add_auto_cache_control(obj, model);
        if let Some(effort) = &params.reasoning_effort {
            self.flavor.add_reasoning_effort(obj, effort);
        }
        if let Some(t) = params.temperature {
            obj.insert("temperature".into(), serde_json::json!(t));
        }
        if let Some(p) = params.top_p {
            obj.insert("top_p".into(), serde_json::json!(p));
        }
        if let Some(m) = params.max_tokens {
            obj.insert("max_tokens".into(), serde_json::json!(m));
        }
        if !tools.is_empty() {
            let wire: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
                    })
                })
                .collect();
            obj.insert("tools".into(), serde_json::json!(wire));
            if disable_tool_choice {
                obj.insert("tool_choice".into(), serde_json::json!("none"));
            }
            if params.parallel_tool_calls == Some(false) {
                obj.insert("parallel_tool_calls".into(), serde_json::json!(false));
            }
        }
        add_cache_ttl_1h(&mut body, model, params.cache_ttl_1h);
        if self.flavor == ProviderFlavor::Mistral {
            mistral_body(&mut body);
        }
        body
    }

    /// One request/response over SSE. Doesn't send `Done` itself — the caller
    /// decides whether another tool round-trip follows.
    // Long by design (stream processing).
    #[allow(clippy::too_many_lines)]
    async fn run_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        params: &ChatParams,
        tools: &[ToolDef],
        disable_tool_choice: bool,
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<Finish> {
        if let Some(delegate) = self.openrouter_delegate_for_model(model) {
            return Box::pin(delegate.run_stream(
                model,
                messages,
                params,
                tools,
                disable_tool_choice,
                tx,
            ))
            .await;
        }
        let finish = self
            .run_stream_unlogged(model, messages, params, tools, disable_tool_choice, tx)
            .await?;
        match &finish {
            Finish::Done(output) | Finish::ToolCalls(_, output) => {
                self.record_usage(model, output.usage);
            }
            Finish::Errored => {}
        }
        Ok(finish)
    }

    #[allow(clippy::too_many_lines)]
    async fn run_stream_unlogged(
        &self,
        model: &str,
        messages: &[ChatMessage],
        params: &ChatParams,
        tools: &[ToolDef],
        disable_tool_choice: bool,
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<Finish> {
        if self.flavor == ProviderFlavor::OpenAiCodex {
            return self
                .run_codex_stream(model, messages, params, tools, disable_tool_choice, tx)
                .await;
        }
        let (base, model) = self.opencode_route(model);
        let body = self.chat_completions_body(&model, messages, params, tools, disable_tool_choice);
        let request = self
            .authenticate(self.client.post(format!("{base}/chat/completions")))
            .json(&body);

        let mut es = EventSource::new(request).context("opening SSE stream")?;
        let mut tool_calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
        let mut content_acc = String::new();
        let mut reasoning_acc = String::new();
        // Only local endpoints get the splitter: a hosted model that writes
        // `<|channel|>` is talking about Harmony, not speaking it.
        let mut demux = self.is_local().then(harmony::Demux::default);
        let mut usage = None;
        let mut done = false;
        while let Some(event) = es.next().await {
            match event {
                Ok(Event::Open) => {}
                Ok(Event::Message(msg)) => {
                    if msg.data == "[DONE]" {
                        done = true;
                        break; // main phase over; the cost drain runs below
                    }
                    let (content, reasoning) = parse_delta(&msg.data);
                    if let Some(r) = reasoning
                        && !r.is_empty()
                    {
                        reasoning_acc.push_str(&r);
                        let _ = tx.send(StreamEvent::Reasoning(r));
                    }
                    if let Some(token) = content
                        && !token.is_empty()
                    {
                        if let Some(demux) = demux.as_mut() {
                            let pieces = demux.push(&token);
                            route_harmony(
                                pieces,
                                &mut content_acc,
                                &mut reasoning_acc,
                                &mut tool_calls,
                                tx,
                            );
                        } else {
                            content_acc.push_str(&token);
                            let _ = tx.send(StreamEvent::Token(token));
                        }
                    }
                    accumulate_tool_calls(&mut tool_calls, &msg.data);
                    if let Some(next) = parse_usage(&msg.data) {
                        merge_usage(&mut usage, next);
                    } else if let Some(cost) = parse_stream_cost(&msg.data) {
                        // OpenCode Zen: models on the general route never
                        // report usage (every chunk carries `usage:null`);
                        // the trailing cost chunk is the only accounting.
                        merge_usage(
                            &mut usage,
                            Usage {
                                cost: Some(cost),
                                prompt_convention: PromptConvention::Unknown,
                                ..Default::default()
                            },
                        );
                    }
                }
                Err(reqwest_eventsource::Error::StreamEnded) => break,
                // These two variants carry the actual HTTP response, but
                // their `Display` is close to useless ("Invalid header
                // value: \"\"" when Content-Type is simply absent — no
                // status, no body shown). Read the response through instead
                // so the real reason (an auth/entitlement error page, a
                // rate limit, etc.) reaches the user rather than a
                // header-parsing artifact.
                Err(
                    reqwest_eventsource::Error::InvalidStatusCode(_, response)
                    | reqwest_eventsource::Error::InvalidContentType(_, response),
                ) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    let msg = format!("request failed ({status}): {}", truncate_error_body(&body));
                    let _ = tx.send(StreamEvent::Error(msg));
                    es.close();
                    return Ok(Finish::Errored);
                }
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(e.to_string()));
                    es.close();
                    return Ok(Finish::Errored);
                }
            }
        }
        // Whatever the last delta held back (a control token split across
        // chunks, or a truncated tool call) is resolved here.
        if let Some(demux) = demux.as_mut() {
            let pieces = demux.finish();
            route_harmony(
                pieces,
                &mut content_acc,
                &mut reasoning_acc,
                &mut tool_calls,
                tx,
            );
        }
        // The stream isn't finished at [DONE]: OpenCode sends the trailing
        // cost chunk ({"choices":[],"cost":"…"}) AFTER it, and for
        // no-usage (zen free) models that chunk is the entire accounting.
        // Drain it with a cap so a peer that keeps the connection open
        // after [DONE] can't hang the turn.
        if done {
            loop {
                let next = tokio::time::timeout(std::time::Duration::from_secs(3), es.next()).await;
                match next {
                    Ok(Some(Ok(Event::Message(msg)))) if msg.data != "[DONE]" => {
                        if let Some(cost) = parse_stream_cost(&msg.data) {
                            merge_usage(
                                &mut usage,
                                Usage {
                                    cost: Some(cost),
                                    prompt_convention: PromptConvention::Unknown,
                                    ..Default::default()
                                },
                            );
                        }
                    }
                    // StreamEnded, transport error, a second [DONE], or the
                    // 3-second cap — either way the accounting is complete.
                    _ => break,
                }
            }
        }
        // Surface exactly one accounting event for this provider request.
        // Keeping it at the request boundary prevents the app from treating
        // several partial/cost chunks as separate requests.
        if let Some(usage) = usage {
            let _ = tx.send(StreamEvent::Usage(usage));
        }
        // Trust accumulated tool calls, not finish_reason: some providers
        // stream tool_calls but finish with "stop" (or no finish chunk at
        // all); dropping the calls there kills the turn silently.
        let output = RoundOutput {
            content: content_acc,
            reasoning: (!reasoning_acc.is_empty()).then_some(reasoning_acc),
            usage,
        };
        if tool_calls.is_empty() {
            Ok(Finish::Done(output))
        } else {
            Ok(Finish::ToolCalls(
                tool_calls.into_values().collect(),
                output,
            ))
        }
    }

    /// For `OpenCode` Go: pick the base to hit and the raw model id to send,
    /// stripping the `go:` tag `list_models` adds to flat-fee Go models. A
    /// no-op (returns the flavor's default base, id unchanged) for every
    /// other flavor and for untagged (general Zen) `OpenCode` ids.
    fn opencode_route(&self, model: &str) -> (&str, String) {
        if self.flavor == ProviderFlavor::OpencodeGo
            && let Some(stripped) = model.strip_prefix(OPENCODE_GO_PREFIX)
        {
            return (OPENCODE_GO_BASE, stripped.to_string());
        }
        (self.api_base(), model.to_string())
    }

    fn openrouter_delegate_for_model(&self, model: &str) -> Option<Self> {
        if self.flavor == ProviderFlavor::OpenAiCodex && model.contains('/') {
            crate::config::load_openrouter_key_only().map(|key| {
                let mut delegate = Self::openrouter_flavor(key);
                delegate.usage_log.clone_from(&self.usage_log);
                delegate
            })
        } else {
            None
        }
    }

    fn openrouter_delegate_for_body(&self, body: &serde_json::Value) -> Option<Self> {
        let model = body.get("model").and_then(|m| m.as_str())?;
        self.openrouter_delegate_for_model(model)
    }

    /// GET the ChatGPT-sign-in model catalog the Codex CLI reads
    /// (`/codex/models`), filtered for this client version.
    async fn fetch_codex_catalog(&self, client_version: &str) -> Result<Vec<Model>> {
        let body: serde_json::Value = self
            .client
            .get(format!(
                "{}/codex/models?client_version={client_version}",
                self.flavor.base()
            ))
            .headers(self.codex_headers(false)?)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .context("fetching the Codex model catalog")?
            .error_for_status()
            .context("Codex model catalog")?
            .json()
            .await
            .context("parsing the Codex model catalog")?;
        Ok(parse_codex_catalog(&body))
    }

    fn codex_headers(&self, sse: bool) -> Result<reqwest::header::HeaderMap> {
        let account_id = crate::config::codex_account_id(&self.key)?;
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.key).parse()?,
        );
        h.insert("chatgpt-account-id", account_id.parse()?);
        h.insert("originator", "nexus-chat".parse()?);
        h.insert(reqwest::header::USER_AGENT, "nexus-chat".parse()?);
        h.insert("OpenAI-Beta", "responses=experimental".parse()?);
        h.insert(reqwest::header::CONTENT_TYPE, "application/json".parse()?);
        if sse {
            h.insert(reqwest::header::ACCEPT, "text/event-stream".parse()?);
        }
        Ok(h)
    }

    async fn run_codex_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        params: &ChatParams,
        tools: &[ToolDef],
        disable_tool_choice: bool,
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<Finish> {
        let mut body = serde_json::json!({
            "model": model,
            "store": false,
            "stream": true,
            "instructions": codex_instructions(messages),
            "input": codex_input(messages),
            "text": { "verbosity": "low" },
            "include": ["reasoning.encrypted_content"],
            "tool_choice": if disable_tool_choice { "none" } else { "auto" },
            "parallel_tool_calls": true,
        });
        let obj = body.as_object_mut().unwrap();
        if let Some(key) = &params.prompt_cache_key {
            // The Codex Responses route uses the public Responses API cache
            // key field when it is available. Without it, GPT-5.6+ can route
            // identical prefixes to different cache workers and report no
            // reuse even though the full input is stable.
            obj.insert("prompt_cache_key".into(), serde_json::json!(key));
        }
        if let Some(t) = params.temperature {
            obj.insert("temperature".into(), serde_json::json!(t));
        }
        if let Some(effort) = &params.reasoning_effort {
            obj.insert(
                "reasoning".into(),
                serde_json::json!({ "effort": effort, "summary": "auto" }),
            );
        }
        if !tools.is_empty() {
            obj.insert("tools".into(), serde_json::json!(codex_tools(tools)));
        }
        // Not reqwest_eventsource here: it gates the whole response on a
        // strict Content-Type == "text/event-stream" check, and ChatGPT's
        // backend has been observed sending a genuine SSE body (starting
        // with a real "event: response.created" line) under a Content-Type
        // that check doesn't accept — the crate then reports the situation
        // as an opaque "invalid header value" and discards the (valid) body
        // entirely. Reading the response as a raw byte stream and splitting
        // on blank lines ourselves doesn't care what Content-Type says.
        let response = self
            .client
            .post(format!("{}/codex/responses", self.flavor.base()))
            .headers(codex_cache_headers(
                self.codex_headers(true)?,
                params.prompt_cache_key.as_deref(),
                params.codex_turn_state.get().map(String::as_str),
            ))
            .json(&body)
            .send()
            .await
            .context("sending Codex request")?;
        remember_turn_state(&response, params);
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            let msg = format!("request failed ({status}): {}", truncate_error_body(&text));
            let _ = tx.send(StreamEvent::Error(msg));
            return Ok(Finish::Errored);
        }

        let mut tool_calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
        let mut content_acc = String::new();
        let mut usage = None;
        let mut buf = String::new();
        let mut stream = response.bytes_stream();
        'stream: while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading Codex stream")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // SSE events are separated by a blank line; a "data:" line's
            // content (possibly spread across several, joined by \n per the
            // spec — Codex never does this in practice, but handle it) is
            // the payload we care about.
            while let Some(end) = buf.find("\n\n") {
                let event_block: String = buf.drain(..end + 2).collect();
                let data = sse_event_data(&event_block);
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    break 'stream;
                }
                if let Some(token) = codex_text_delta(&data)
                    && !token.is_empty()
                {
                    content_acc.push_str(&token);
                    let _ = tx.send(StreamEvent::Token(token));
                }
                if let Some(r) = codex_reasoning_delta(&data)
                    && !r.is_empty()
                {
                    let _ = tx.send(StreamEvent::Reasoning(r));
                }
                accumulate_codex_tool_calls(&mut tool_calls, &data);
                if let Some(next) = codex_usage(&data) {
                    merge_usage(&mut usage, next);
                }
            }
        }
        if let Some(usage) = usage {
            let _ = tx.send(StreamEvent::Usage(usage));
        }
        let output = RoundOutput {
            content: content_acc,
            reasoning: None,
            usage,
        };
        if tool_calls.is_empty() {
            Ok(Finish::Done(output))
        } else {
            Ok(Finish::ToolCalls(
                tool_calls.into_values().collect(),
                output,
            ))
        }
    }
}

struct RoundOutput {
    content: String,
    reasoning: Option<String>,
    usage: Option<Usage>,
}

enum Finish {
    Done(RoundOutput),
    ToolCalls(Vec<ToolCall>, RoundOutput),
    Errored,
}

/// Cap an error response body shown to the user — a WAF/error page can be
/// arbitrarily large HTML, and the status line has no room for it anyway.
fn truncate_error_body(body: &str) -> String {
    const MAX: usize = 300;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(empty body)".to_string();
    }
    let mut truncated: String = trimmed.chars().take(MAX).collect();
    if trimmed.chars().count() > MAX {
        truncated.push('…');
    }
    truncated
}

/// Pull `(content, reasoning)` deltas out of one SSE data chunk. ``OpenRouter`` puts
/// thinking tokens in `delta.reasoning`, separate from the visible `delta.content`.
/// Dispatch demultiplexed Harmony spans the way the provider should have
/// sent them: visible text as tokens, chain-of-thought as reasoning, and an
/// unparsed `commentary` call as a real tool call. Calls are appended after
/// any the runtime *did* parse, so a server that handles Harmony itself
/// never sees a duplicate.
fn route_harmony(
    pieces: Vec<Piece>,
    content_acc: &mut String,
    reasoning_acc: &mut String,
    tool_calls: &mut BTreeMap<usize, ToolCall>,
    tx: &mpsc::UnboundedSender<StreamEvent>,
) {
    for piece in pieces {
        match piece {
            Piece::Content(text) => {
                content_acc.push_str(&text);
                let _ = tx.send(StreamEvent::Token(text));
            }
            Piece::Reasoning(text) => {
                reasoning_acc.push_str(&text);
                let _ = tx.send(StreamEvent::Reasoning(text));
            }
            Piece::ToolCall { name, arguments } => {
                let index = tool_calls.keys().last().map_or(0, |last| last + 1);
                tool_calls.insert(index, harmony::synthetic_call(index, name, arguments));
            }
        }
    }
}

/// Scan a tool result string for `![...](file)` image references, load the
/// files from `files_dir`, resize large images to avoid hitting API size
/// limits, and return data URLs for the synthetic user message.
/// Returns `None` when no references found.
pub(crate) fn extract_tool_images(
    result: &str,
    files_dir: &std::path::Path,
) -> Option<ImagesForTool> {
    use base64::Engine;
    let mut urls: Vec<String> = Vec::new();
    let mut descs: Vec<String> = Vec::new();
    let mut rest = result;
    while let Some(start) = rest.find("![") {
        if let Some(end) = rest[start..].find(')') {
            let inner = &rest[start + 2..start + end];
            if let Some((desc, file)) = inner.split_once("](") {
                let rel = std::path::Path::new(file);
                let safe = !rel.is_absolute()
                    && !rel
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir));
                // Only real images, judged by content: a PDF or text file
                // named in an image reference is never sent as image data.
                let image = safe
                    .then(|| std::fs::read(files_dir.join(rel)).ok())
                    .flatten()
                    .and_then(|bytes| crate::extract::vision_mime(&bytes).map(|m| (bytes, m)));
                if let Some((bytes, original_mime)) = image {
                    let resized = resize_for_api(&bytes, files_dir);
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&resized);
                    let mime = if resized.len() < bytes.len() {
                        "image/jpeg"
                    } else {
                        original_mime
                    };
                    urls.push(format!("data:{mime};base64,{b64}"));
                    descs.push(desc.to_string());
                }
            }
            rest = &rest[start + end + 1..];
        } else {
            break;
        }
    }
    if urls.is_empty() {
        return None;
    }
    let description = format!("The tool returned this image: {}", descs.join(", "));
    Some(ImagesForTool { urls, description })
}

/// Return a JPEG-compressed version of the image if it exceeds 1 MB, so the
/// API request doesn't get rejected for being too large. Keeps the original
/// if it's already small enough.
fn resize_for_api(bytes: &[u8], _files_dir: &std::path::Path) -> Vec<u8> {
    // ponytail: 1 MB threshold — large enough for readable text, small
    // enough to avoid 400s from provider byte limits.
    if bytes.len() < 1_000_000 {
        return bytes.to_vec();
    }
    if let Ok(img) = image::load_from_memory(bytes) {
        let (w, h) = (img.width(), img.height());
        let max_dim = 1024u32;
        if w <= max_dim && h <= max_dim {
            return bytes.to_vec();
        }
        let (nw, nh) = if w > h {
            (max_dim, (h * max_dim / w).max(1))
        } else {
            ((w * max_dim / h).max(1), max_dim)
        };
        let small = img.resize(nw, nh, image::imageops::FilterType::Lanczos3);
        let mut out = Vec::new();
        if small
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .is_ok()
        {
            return out;
        }
    }
    bytes.to_vec()
}

/// The history with one system message, first: the leading system block
/// (prompt, space images, compaction digest) merged into it, and any later
/// system note (nexus-ui errors) sent as a `[nexus]` user message instead.
/// The merged prefix is the same every turn, so a local prompt cache still
/// reuses it. `None` when the history already has that shape.
fn single_leading_system(messages: &[ChatMessage]) -> Option<Vec<ChatMessage>> {
    let leading = messages.iter().take_while(|m| m.role == "system").count();
    let later = messages[leading..].iter().any(|m| m.role == "system");
    if leading <= 1 && !later {
        return None;
    }
    let mut out = Vec::with_capacity(messages.len());
    if leading > 0 {
        let mut first = messages[0].clone();
        for extra in &messages[1..leading] {
            first.content.push_str("\n\n");
            first.content.push_str(&extra.content);
            first.images.extend(extra.images.iter().cloned());
            first.cache_breakpoint |= extra.cache_breakpoint;
        }
        out.push(first);
    }
    out.extend(messages[leading..].iter().map(|m| {
        if m.role == "system" {
            ChatMessage {
                role: "user".into(),
                content: format!("[nexus] {}", m.content),
                ..m.clone()
            }
        } else {
            m.clone()
        }
    }));
    Some(out)
}

pub(crate) struct ImagesForTool {
    pub(crate) urls: Vec<String>,
    pub(crate) description: String,
}

#[cfg(test)]
pub(crate) mod decider_mock {
    use super::OpenRouter;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    pub(crate) async fn server(
        status: u16,
        body: String,
        delay: Duration,
        count: usize,
    ) -> (
        OpenRouter,
        tokio::task::JoinHandle<Vec<(String, serde_json::Value)>>,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let mut provider = OpenRouter::openrouter_flavor("mock-decider-key".into());
        provider.endpoint = Some(format!("http://{}/api/v1", listener.local_addr().unwrap()));
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..count {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(stream.read_line(&mut line).await.unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    headers.push_str(&line);
                }
                let mut request = vec![0; length];
                stream.read_exact(&mut request).await.unwrap();
                requests.push((headers, serde_json::from_slice(&request).unwrap()));
                tokio::time::sleep(delay).await;
                let headers = format!(
                    "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .get_mut()
                    .write_all(headers.as_bytes())
                    .await
                    .unwrap();
                stream.get_mut().write_all(body.as_bytes()).await.unwrap();
            }
            requests
        });
        (provider, handle)
    }
}

#[cfg(test)]
mod tests;
