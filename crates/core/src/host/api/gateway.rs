//! The `OpenAI`-wire gateway: routing, upstream forwarding, and usage logging.

use std::io;

use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::oneshot;

use crate::app::App;
use crate::provider::openrouter;
use crate::provider::{BackendTag, Usage};

use super::actor::{HostRequest, ask_actor};
use super::http::{
    Request, is_hop_by_hop, reason, respond_error, respond_full, respond_json, respond_text,
};
use super::{
    CORS_HEADERS, HostState, MAX_GATEWAY_BODY_BYTES, MAX_USAGE_BUFFER_BYTES,
    UPSTREAM_CHUNK_TIMEOUT, UPSTREAM_HEADERS_TIMEOUT,
};
use crate::host::wire::{WireModel, public_model_id};

/// A backend route for one gateway request; kept private so its upstream key
/// cannot leak.
#[derive(Clone)]
pub(super) struct GatewayRoute {
    pub(super) tag: BackendTag,
    pub(super) model: String,
    pub(super) key: String,
    pub(super) account_id: Option<String>,
}

/// Usage observed while forwarding one provider response, reassembling `SSE`
/// frames from socket chunks.
#[derive(Debug, Default)]
pub(super) struct GatewayUsage {
    usage: Option<Usage>,
    /// Bytes not yet forming a complete `SSE` line, or the whole non-streaming
    /// body.
    buffer: Vec<u8>,
}

impl GatewayUsage {
    pub(super) fn observe(&mut self, bytes: &[u8], streaming: bool) {
        if self.buffer.len() + bytes.len() > MAX_USAGE_BUFFER_BYTES {
            return;
        }
        self.buffer.extend_from_slice(bytes);
        if !streaming {
            // One JSON document; it can only be parsed once it is complete.
            return;
        }
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=position).collect();
            self.observe_line(&String::from_utf8_lossy(&line));
        }
    }

    /// Parse whatever is left once the upstream body ends.
    pub(super) fn finish(&mut self, streaming: bool) {
        let rest = std::mem::take(&mut self.buffer);
        if rest.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&rest).into_owned();
        if streaming {
            self.observe_line(&text);
        } else {
            self.observe_json(text.trim());
        }
    }

    fn observe_line(&mut self, line: &str) {
        let Some(data) = line.trim_end().strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data != "[DONE]" {
            self.observe_json(data);
        }
    }

    fn observe_json(&mut self, text: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let Some(usage) = value.get("usage") else {
            return;
        };
        if let Some(next) = openrouter::parse_usage_value(usage) {
            openrouter::merge_usage(&mut self.usage, next);
        }
    }

    /// The accounting to log, or an all-zero record when the upstream never
    /// reported any.
    pub(super) fn into_usage(self) -> Usage {
        self.usage.unwrap_or_default()
    }
}

#[derive(Debug, Serialize)]
pub(super) struct DiscoveryResponse {
    pub(super) host_id: String,
    pub(super) product: &'static str,
    pub(super) version: &'static str,
    pub(super) api: &'static str,
    pub(super) authentication: &'static str,
}

#[derive(Debug, Serialize)]
pub(super) struct ModelsResponse {
    pub(super) object: &'static str,
    pub(super) data: Vec<OpenAiModel>,
}

/// `OpenAI` `/v1/models` fields, with Dugix routing metadata retained as
/// additive fields for clients that want backend-aware pickers.
#[derive(Debug, Serialize)]
pub(super) struct OpenAiModel {
    pub(super) id: String,
    object: &'static str,
    created: u64,
    pub(super) owned_by: String,
    backend: BackendTag,
    name: String,
    reasoning_efforts: Vec<crate::host::wire::WireReasoningEffort>,
    context_length: Option<u64>,
    supports_images: bool,
    supports_image_generation: bool,
    supports_video_generation: bool,
    pricing: Option<crate::host::wire::WireModelPricing>,
}

impl From<WireModel> for OpenAiModel {
    fn from(model: WireModel) -> Self {
        let owned_by = model.backend.info().owned_by;
        Self {
            id: model.id,
            object: "model",
            created: 0,
            owned_by: owned_by.to_string(),
            backend: model.backend,
            name: model.name,
            reasoning_efforts: model.reasoning_efforts,
            context_length: model.context_length,
            supports_images: model.supports_images,
            supports_image_generation: model.supports_image_generation,
            supports_video_generation: model.supports_video_generation,
            pricing: model.pricing,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct BackendsResponse {
    pub(super) backends: Vec<BackendInfo>,
}

#[derive(Debug, Serialize)]
pub(super) struct BackendInfo {
    pub(super) tag: BackendTag,
    pub(super) name: &'static str,
    pub(super) configured: bool,
    /// Whether `/v1/chat/completions` can route this backend; always true.
    pub(super) gateway_supported: bool,
    /// Stable explanation when a backend cannot be routed; always `None` today.
    pub(super) gateway_error: Option<&'static str>,
    pub(super) default_model: String,
    pub(super) model_count: usize,
}

#[allow(clippy::too_many_lines)]
pub(super) async fn handle_gateway(
    stream: &mut TcpStream,
    request: &Request,
    state: &HostState,
) -> io::Result<()> {
    if request.method != "POST" {
        return respond_text(stream, 405, "method not allowed").await;
    }
    if request.body.len() > MAX_GATEWAY_BODY_BYTES {
        return respond_text(stream, 413, "request entity too large").await;
    }
    let body: serde_json::Value = match serde_json::from_slice(&request.body) {
        Ok(body) => body,
        Err(error) => {
            return respond_error(stream, 400, &format!("invalid completion request: {error}"))
                .await;
        }
    };
    let model = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if model.trim().is_empty() {
        return respond_error(stream, 400, "completion request is missing model").await;
    }
    let override_tag = request
        .headers
        .get("x-dugix-backend")
        .map(|value| parse_backend_tag(value))
        .transpose();
    let override_tag = match override_tag {
        Ok(tag) => tag,
        Err(error) => return respond_error(stream, 400, &error).await,
    };
    let route = ask_actor(&state.requests, |reply| HostRequest::GatewayRoute {
        model: model.to_string(),
        override_tag,
        reply,
    })
    .await;
    let route = match route {
        Ok(route) => route,
        Err(error) => {
            let models = ask_actor(&state.requests, |reply| HostRequest::Models { reply })
                .await
                .unwrap_or_default();
            return respond_json(
                stream,
                400,
                &serde_json::json!({
                    "error": { "message": error, "type": "invalid_request_error" },
                    "models": models,
                }),
            )
            .await;
        }
    };
    let (default_base, raw_model) = openrouter::gateway_route(route.tag, &route.model);
    let base = state.gateway_base.as_deref().unwrap_or(default_base);
    let codex = route.tag == BackendTag::Codex;
    let streaming_requested = body
        .get("stream")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let url = if codex {
        format!("{base}/codex/responses")
    } else {
        format!("{base}/chat/completions")
    };
    // `/v1/models` publishes composite ids (`openai:`/`codex:`/`opencode:`
    // prefixes, plus the `go:` tag `list_models` adds), which no upstream
    // recognizes. Forward the raw id the route resolved to, not the client's.
    let payload = if codex {
        serde_json::to_vec(&openrouter::chat_body_to_codex_body(
            &body,
            streaming_requested,
            &[],
        ))
        .unwrap_or_else(|_| request.body.clone())
    } else {
        let mut forwarded = body.clone();
        if let Some(object) = forwarded.as_object_mut() {
            object.insert(
                "model".to_string(),
                serde_json::Value::String(raw_model.clone()),
            );
        }
        if route.tag == BackendTag::Mistral {
            openrouter::mistral_gateway_body(&mut forwarded);
        }
        serde_json::to_vec(&forwarded).unwrap_or_else(|_| request.body.clone())
    };
    let mut builder = state
        .client
        .post(url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", route.key),
        )
        .body(payload);
    for (name, value) in &request.headers {
        if is_hop_by_hop(name) || name.starts_with("x-dugix-") {
            continue;
        }
        let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) else {
            continue;
        };
        builder = builder.header(header_name, header_value);
    }
    if codex {
        if let Some(account_id) = &route.account_id {
            builder = builder.header("chatgpt-account-id", account_id);
        }
        builder = builder
            .header("originator", "dugix-host")
            .header("OpenAI-Beta", "responses=experimental")
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if streaming_requested {
            builder = builder.header(reqwest::header::ACCEPT, "text/event-stream");
        }
    }
    let response = match tokio::time::timeout(UPSTREAM_HEADERS_TIMEOUT, builder.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return respond_error(stream, 502, &format!("upstream request failed: {error}")).await;
        }
        Err(_) => return respond_error(stream, 504, "upstream request timed out").await,
    };
    if codex && response.status().is_success() {
        return handle_codex_gateway_response(
            stream,
            response,
            state,
            route,
            &raw_model,
            streaming_requested,
        )
        .await;
    }
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let streaming = content_type
        .to_ascii_lowercase()
        .contains("text/event-stream")
        || body
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
    let mut upstream = response.bytes_stream();
    let mut usage = GatewayUsage::default();
    let reason = reason(status);
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nCache-Control: no-cache\r\nConnection: close\r\n{CORS_HEADERS}\r\n"
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await?;
    loop {
        let Ok(next) = tokio::time::timeout(UPSTREAM_CHUNK_TIMEOUT, upstream.next()).await else {
            break;
        };
        let Some(chunk) = next else {
            break;
        };
        let Ok(chunk) = chunk else {
            break;
        };
        usage.observe(&chunk, streaming);
        if stream.write_all(&chunk).await.is_err() {
            return Ok(());
        }
        if stream.flush().await.is_err() {
            return Ok(());
        }
    }
    usage.finish(streaming);
    log_gateway_usage(state, route, usage).await;
    stream.shutdown().await
}

async fn handle_codex_gateway_response(
    stream: &mut TcpStream,
    response: reqwest::Response,
    state: &HostState,
    route: GatewayRoute,
    model: &str,
    streaming: bool,
) -> io::Result<()> {
    if !streaming {
        let status = response.status().as_u16();
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                return respond_error(
                    stream,
                    502,
                    &format!("reading Codex response failed: {error}"),
                )
                .await;
            }
        };
        let response_json: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                return respond_error(stream, 502, &format!("invalid Codex response: {error}"))
                    .await;
            }
        };
        let translated = openrouter::codex_response_to_chat(&response_json);
        let payload = match serde_json::to_vec(&translated) {
            Ok(payload) => payload,
            Err(error) => {
                return respond_error(
                    stream,
                    502,
                    &format!("encoding Codex response failed: {error}"),
                )
                .await;
            }
        };
        let mut usage = GatewayUsage::default();
        usage.observe(&payload, false);
        usage.finish(false);
        log_gateway_usage(state, route, usage).await;
        return respond_full(stream, status, "application/json", "", &payload, false).await;
    }

    let mut adapter = openrouter::CodexChatStreamAdapter::new(model.to_string());
    let mut usage = GatewayUsage::default();
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: close\r\n{CORS_HEADERS}\r\n"
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await?;
    let mut upstream = response.bytes_stream();
    loop {
        // A stream that stops early must not be reported as a clean stop: the
        // adapter's terminal chunk would otherwise tell the client the answer
        // finished normally when it was cut off mid-response.
        let Ok(next) = tokio::time::timeout(UPSTREAM_CHUNK_TIMEOUT, upstream.next()).await else {
            adapter.fail("Codex stream stalled");
            break;
        };
        let Some(chunk) = next else { break };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                adapter.fail(format!("Codex stream failed: {error}"));
                break;
            }
        };
        for frame in adapter.push_bytes(&chunk) {
            usage.observe(&frame, true);
            if stream.write_all(&frame).await.is_err() {
                return Ok(());
            }
            if stream.flush().await.is_err() {
                return Ok(());
            }
        }
    }
    for frame in adapter.finish() {
        usage.observe(&frame, true);
        if stream.write_all(&frame).await.is_err() {
            return Ok(());
        }
        if stream.flush().await.is_err() {
            return Ok(());
        }
    }
    usage.finish(true);
    log_gateway_usage(state, route, usage).await;
    stream.shutdown().await
}

async fn log_gateway_usage(state: &HostState, route: GatewayRoute, usage: GatewayUsage) {
    let (reply, result) = oneshot::channel();
    let _ = state
        .requests
        .send(HostRequest::LogGatewayUsage {
            route,
            usage,
            reply,
        })
        .await;
    let _ = result.await;
}

pub(super) fn public_model_id_for_backend(tag: BackendTag, model: &str) -> String {
    format!("{}{}", tag.wire_prefix(), raw_model_for_backend(tag, model))
}

fn raw_model_for_backend(tag: BackendTag, model: &str) -> String {
    model
        .strip_prefix(tag.wire_prefix())
        .or_else(|| {
            let prefix = tag.key_prefix();
            (!prefix.is_empty())
                .then(|| model.strip_prefix(prefix))
                .flatten()
        })
        .unwrap_or(model)
        .to_string()
}

pub(super) fn gateway_route(
    app: &App,
    model: &str,
    override_tag: Option<BackendTag>,
) -> Result<GatewayRoute, String> {
    let selected = if let Some(tag) = override_tag {
        if !app.backends.configured(tag) {
            return Err(format!("backend {} is not configured", tag.name()));
        }
        Some((tag, raw_model_for_backend(tag, model)))
    } else {
        app.models.iter().find_map(|entry| {
            let composite = crate::app::composite_id(entry);
            let public_id = public_model_id(entry);
            (entry.id == model || composite == model || public_id == model)
                .then_some((entry.backend, entry.id.clone()))
        })
    };
    let Some((tag, raw_model)) = selected else {
        return Err(format!("unknown model {model:?}"));
    };
    let key = match tag {
        BackendTag::Local => {
            return Err("local inference is not yet supported by the host gateway".into());
        }
        BackendTag::Codex => app
            .saved
            .codex
            .as_ref()
            .map(|credentials| credentials.access.clone()),
        _ => app.saved.key(tag).map(str::to_string),
    }
    .ok_or_else(|| format!("backend {} is not configured", tag.name()))?;
    let account_id = app
        .saved
        .codex
        .as_ref()
        .map(|credentials| credentials.account_id.clone());
    Ok(GatewayRoute {
        tag,
        model: raw_model,
        key,
        account_id,
    })
}

pub(super) fn parse_backend_tag(value: &str) -> Result<BackendTag, String> {
    BackendTag::from_alias(value).ok_or_else(|| format!("unknown x-dugix-backend {value:?}"))
}
