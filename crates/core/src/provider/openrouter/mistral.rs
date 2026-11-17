//! Mistral: catalog parsing, request shaping, and tool-call id fitting.

use serde::Deserialize;

use super::{MISTRAL_VIBE_FAST_MODEL, MISTRAL_VIBE_MODEL};
use crate::provider::{BackendTag, Model, ModelPricing, ReasoningEffort};

/// Mistral's `/v1/models`: one card per id *and* per alias, with the context
/// window in `max_context_length` and capabilities as flags.
#[derive(Deserialize)]
pub(super) struct MistralModelsResponse {
    data: Vec<MistralModelEntry>,
}

#[derive(Deserialize)]
struct MistralModelEntry {
    id: String,
    #[serde(default)]
    max_context_length: Option<u64>,
    #[serde(default)]
    capabilities: MistralCapabilities,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    deprecation: Option<serde_json::Value>,
}

// Mirrors the wire object's independent flags.
#[allow(clippy::struct_excessive_bools)]
#[derive(Deserialize, Default)]
struct MistralCapabilities {
    #[serde(default)]
    completion_chat: bool,
    #[serde(default)]
    function_calling: bool,
    #[serde(default)]
    vision: bool,
    #[serde(default)]
    reasoning: bool,
}

/// Mistral models usable in a tool-calling chat, one per alias group; the two
/// Vibe-metered names are added when the catalog lacks them.
pub(super) fn parse_mistral_catalog(catalog: MistralModelsResponse) -> Vec<Model> {
    // Show each alias group once, preferring the Vibe name (it bills against
    // the plan), then a `-latest` name, then the shortest.
    let rank = |id: &str| {
        let vibe = id == MISTRAL_VIBE_MODEL || id == MISTRAL_VIBE_FAST_MODEL;
        (!vibe, !id.ends_with("-latest"), id.len(), id.to_string())
    };
    let mut groups: Vec<(std::collections::HashSet<String>, MistralModelEntry)> = Vec::new();
    for m in catalog.data.into_iter().filter(|m| {
        m.capabilities.completion_chat && m.capabilities.function_calling && m.deprecation.is_none()
    }) {
        let names: std::collections::HashSet<String> =
            m.aliases.iter().cloned().chain([m.id.clone()]).collect();
        match groups
            .iter_mut()
            .find(|(seen, _)| !seen.is_disjoint(&names))
        {
            Some((seen, best)) => {
                seen.extend(names);
                if rank(&m.id) < rank(&best.id) {
                    *best = m;
                }
            }
            None => groups.push((names, m)),
        }
    }
    let mut models: Vec<Model> = groups
        .into_iter()
        .map(|(names, m)| Model {
            aliases: {
                let mut aliases: Vec<String> =
                    names.into_iter().filter(|name| *name != m.id).collect();
                aliases.sort();
                aliases
            },
            name: m.id.clone(),
            reasoning_efforts: if m.capabilities.reasoning {
                ReasoningEffort::HIGH_AND_NONE.to_vec()
            } else {
                Vec::new()
            },
            context_length: m.max_context_length,
            supports_images: m.capabilities.vision,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::Mistral,
            id: m.id,
            pricing: None,
        })
        .collect();
    // In case a key's catalog leaves them out: both are reasoning, vision
    // models as of 2026-11 (Medium 3.5 and Small 2603).
    for (id, name) in [
        (MISTRAL_VIBE_MODEL, "Mistral Vibe"),
        (MISTRAL_VIBE_FAST_MODEL, "Mistral Vibe fast"),
    ] {
        if !models.iter().any(|m| m.id == id) {
            models.push(Model {
                id: id.to_string(),
                name: name.to_string(),
                reasoning_efforts: ReasoningEffort::HIGH_AND_NONE.to_vec(),
                context_length: Some(262_144),
                supports_images: true,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: BackendTag::Mistral,
                pricing: None,
                aliases: Vec::new(),
            });
        }
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

/// Give Mistral models their `OpenRouter` twin's price. Mistral serves
/// floating names (`mistral-vibe-cli-latest`) and spells versions with dashes
/// (`glm-5-2`), so match its dated and versioned aliases against `OpenRouter`
/// ids with punctuation ignored, most specific alias first.
pub fn price_mistral_models(models: &mut [Model]) {
    let squash = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    // `:batch`/`:free` variants and `~…-latest` routers aren't the plain price.
    let twins: Vec<(String, String, ModelPricing)> = models
        .iter()
        .filter(|m| {
            m.backend == BackendTag::OpenRouter && !m.id.contains(':') && !m.id.starts_with('~')
        })
        .filter_map(|m| {
            let price = m.pricing?;
            let name = m.id.rsplit('/').next().unwrap_or(&m.id);
            Some((squash(name), squash(&m.id), price))
        })
        .collect();
    let twin_price = |name: &str| {
        let key = squash(name);
        twins
            .iter()
            .filter(|(bare, full, _)| *bare == key || *full == key)
            .min_by_key(|(_, full, _)| full.len())
            .map(|(_, _, price)| *price)
    };
    for model in models
        .iter_mut()
        .filter(|m| m.backend == BackendTag::Mistral && m.pricing.is_none())
    {
        // `-latest` names float between versions; longer names are more specific
        // (`mistral-medium-3-5` over `mistral-medium-3` in the same group).
        let mut names: Vec<&str> = std::iter::once(model.id.as_str())
            .chain(model.aliases.iter().map(String::as_str))
            .filter(|name| !name.ends_with("-latest"))
            .collect();
        names.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        model.pricing = names.into_iter().find_map(twin_price);
    }
}

/// Fit a chat-completions body to Mistral's validation, which rejects unknown
/// message fields and tool-call ids that are not 9 ASCII alphanumerics. System
/// image chunks move to adjacent user messages, where Mistral accepts images.
pub(super) fn mistral_body(body: &mut serde_json::Value) {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    mistral_system_images(messages);
    mistral_image_cap(messages);
    for message in messages.iter_mut().filter_map(|m| m.as_object_mut()) {
        message.remove("reasoning_content");
        if let Some(parts) = message
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        {
            for part in parts.iter_mut().filter_map(|p| p.as_object_mut()) {
                part.remove("cache_control");
            }
        }
        if let Some(id) = message.get_mut("tool_call_id") {
            *id = serde_json::json!(mistral_tool_id(id.as_str().unwrap_or_default()));
        }
        for call in message
            .get_mut("tool_calls")
            .and_then(serde_json::Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            if let Some(id) = call.get_mut("id") {
                *id = serde_json::json!(mistral_tool_id(id.as_str().unwrap_or_default()));
            }
        }
    }
}

/// Preserve system text at its original priority while placing its images
/// in a role Mistral allows. This also covers image-only system messages.
fn mistral_system_images(messages: &mut Vec<serde_json::Value>) {
    let mut normalized = Vec::with_capacity(messages.len());
    for mut message in std::mem::take(messages) {
        let mut images = Vec::new();
        let mut image_only = false;
        if message["role"] == "system"
            && let Some(parts) = message
                .get_mut("content")
                .and_then(serde_json::Value::as_array_mut)
            && parts.iter().any(|part| part["type"] == "image_url")
        {
            let (image_parts, text_parts): (Vec<_>, Vec<_>) = std::mem::take(parts)
                .into_iter()
                .partition(|part| part["type"] == "image_url");
            images = image_parts;
            *parts = text_parts;
            image_only = parts.is_empty();
        }
        if !image_only {
            normalized.push(message);
        }
        if !images.is_empty() {
            normalized.push(serde_json::json!({"role": "user", "content": images}));
        }
    }
    *messages = normalized;
}

/// Mistral rejects a request carrying more than this many images
/// ("Total number of images exceeds the maximum allowed of 20").
pub(super) const MISTRAL_MAX_IMAGES: usize = 20;

/// Older images leave in blocks of this many, so the cut point moves once per
/// block instead of on every new image.
const MISTRAL_IMAGE_EVICTION_STEP: usize = 10;

/// Replace the oldest image chunks with a text note so at most
/// `MISTRAL_MAX_IMAGES` remain, keeping long sessions with deck renders or
/// screenshots working instead of failing every turn with a 400. Images are
/// dropped in blocks: dropping one per new image would change an early message
/// on every request and miss the prompt cache from there on.
fn mistral_image_cap(messages: &mut [serde_json::Value]) {
    let is_image = |part: &serde_json::Value| part["type"] == "image_url";
    let total = messages
        .iter()
        .filter_map(|m| m.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .filter(|part| is_image(part))
        .count();
    let over = total.saturating_sub(MISTRAL_MAX_IMAGES);
    let mut omit = over.div_ceil(MISTRAL_IMAGE_EVICTION_STEP) * MISTRAL_IMAGE_EVICTION_STEP;
    for parts in messages.iter_mut().filter_map(|m| {
        m.get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
    }) {
        for part in parts.iter_mut().filter(|part| is_image(part)) {
            if omit == 0 {
                return;
            }
            omit -= 1;
            *part = serde_json::json!({
                "type": "text",
                "text": "[older image omitted: Mistral accepts at most 20 images per request]",
            });
        }
    }
}

/// `mistral_body` for a request an OpenAI-wire client built (the host
/// gateway): also drops the `OpenAI`-only top-level fields SDKs send by
/// default, and renames `max_completion_tokens` to Mistral's `max_tokens`.
pub fn mistral_gateway_body(body: &mut serde_json::Value) {
    if let Some(object) = body.as_object_mut() {
        for key in ["stream_options", "store", "service_tier"] {
            object.remove(key);
        }
        if let Some(max) = object.remove("max_completion_tokens") {
            object.entry("max_tokens").or_insert(max);
        }
    }
    mistral_body(body);
}

/// A tool-call id Mistral accepts: unchanged if already 9 ASCII
/// alphanumerics, else 9 characters derived from a hash of the original.
pub(super) fn mistral_tool_id(id: &str) -> String {
    use sha2::{Digest, Sha256};
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    if id.len() == 9 && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return id.to_string();
    }
    Sha256::digest(id.as_bytes())
        .iter()
        .take(9)
        .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
        .collect()
}
