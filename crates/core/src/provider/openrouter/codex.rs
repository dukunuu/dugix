//! The Codex Responses API: request translation, response and SSE translation, catalog
//! parsing.

use std::collections::BTreeMap;

use super::CODEX_CLIENT_VERSION;
use super::stream::{
    chat_sse_frame, json_u64, parse_usage_value, sse_block_end, sse_event_data, wire_text,
};
use crate::provider::{ChatMessage, ChatParams, Model, ReasoningEffort, ToolCall, ToolDef, Usage};

/// Convert a native Codex Responses JSON object into a Chat Completions
/// response for gateway clients.
pub fn codex_response_to_chat(response: &serde_json::Value) -> serde_json::Value {
    let id = response
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("chatcmpl-nexus-codex");
    let model = response
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let text = codex_response_text(response);
    let mut tool_calls = Vec::new();
    if let Some(output) = response.get("output").and_then(|value| value.as_array()) {
        for item in output {
            if item.get("type").and_then(|value| value.as_str()) != Some("function_call") {
                continue;
            }
            tool_calls.push(serde_json::json!({
                "id": item.get("call_id").and_then(|value| value.as_str()).unwrap_or_default(),
                "type": "function",
                "function": {
                    "name": item.get("name").and_then(|value| value.as_str()).unwrap_or_default(),
                    "arguments": item.get("arguments").and_then(|value| value.as_str()).unwrap_or_default(),
                },
            }));
        }
    }
    let finish_reason = if tool_calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    // A tool-call response carries `content: null`, matching what an
    // OpenAI-wire client expects to branch on.
    let mut message = if tool_calls.is_empty() {
        serde_json::json!({ "role": "assistant", "content": text })
    } else {
        serde_json::json!({ "role": "assistant", "content": serde_json::Value::Null })
    };
    if !tool_calls.is_empty() {
        message
            .as_object_mut()
            .expect("message is a JSON object")
            .insert("tool_calls".into(), serde_json::Value::Array(tool_calls));
    }
    let mut out = serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": response.get("created_at").and_then(json_u64).unwrap_or(0),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
    });
    if let Some(usage) = response.get("usage").map(response_usage_to_chat) {
        out.as_object_mut()
            .expect("response is a JSON object")
            .insert("usage".into(), usage);
    }
    out
}

fn response_usage_to_chat(usage: &serde_json::Value) -> serde_json::Value {
    let input = usage.get("input_tokens").and_then(json_u64).unwrap_or(0);
    let output = usage.get("output_tokens").and_then(json_u64).unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(json_u64)
        .unwrap_or(input + output);
    serde_json::json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": total,
        "prompt_tokens_details": {
            "cached_tokens": usage.get("input_tokens_details").and_then(|value| value.get("cached_tokens")).and_then(json_u64).unwrap_or(0)
        }
    })
}

/// Translate a native Codex Responses SSE stream into Chat Completions SSE
/// `data:` frames for gateway clients.
pub struct CodexChatStreamAdapter {
    buffer: Vec<u8>,
    id: String,
    model: String,
    created: u64,
    started: bool,
    finished: bool,
    tool_calls: BTreeMap<usize, ToolCall>,
    usage: Option<Usage>,
    failed: Option<String>,
}

impl CodexChatStreamAdapter {
    /// Create an adapter for one gateway request.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            buffer: Vec::new(),
            id: "chatcmpl-nexus-codex".to_string(),
            model: model.into(),
            created: 0,
            started: false,
            finished: false,
            tool_calls: BTreeMap::new(),
            usage: None,
            failed: None,
        }
    }

    /// Feed raw upstream bytes and return complete translated SSE frames.
    /// Bytes are buffered because a chunk boundary can split a multi-byte character.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some((end, separator_len)) = sse_block_end(&self.buffer) {
            let block: Vec<u8> = self.buffer.drain(..end + separator_len).collect();
            let data = sse_event_data(&String::from_utf8_lossy(&block));
            if !data.is_empty() {
                frames.extend(self.push_data(&data));
            }
        }
        frames
    }

    /// Record that the upstream stream ended early (timeout, transport error).
    /// Without this the terminal chunk would claim a normal `stop`, turning a
    /// truncated answer into an apparently complete one.
    pub fn fail(&mut self, message: impl Into<String>) {
        if self.failed.is_none() {
            self.failed = Some(message.into());
        }
    }

    /// Finish the upstream stream and emit the terminal chunk plus `[DONE]`.
    pub fn finish(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        if !self.buffer.is_empty() {
            let block = std::mem::take(&mut self.buffer);
            let data = sse_event_data(&String::from_utf8_lossy(&block));
            if !data.is_empty() {
                frames.extend(self.push_data(&data));
            }
        }
        if self.finished {
            return frames;
        }
        self.finished = true;
        if let Some(message) = self.failed.take() {
            frames.push(chat_sse_frame(&serde_json::json!({
                "error": { "message": message, "type": "server_error" }
            })));
        } else {
            let finish_reason = if self.tool_calls.is_empty() {
                "stop"
            } else {
                "tool_calls"
            };
            frames.push(self.chunk(&serde_json::json!({}), Some(finish_reason)));
            if let Some(usage) = self.usage {
                frames.push(self.usage_chunk(usage));
            }
        }
        frames.push(b"data: [DONE]\n\n".to_vec());
        frames
    }

    #[allow(clippy::too_many_lines)] // one match arm per Responses event type
    fn push_data(&mut self, data: &str) -> Vec<Vec<u8>> {
        if data == "[DONE]" {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            return Vec::new();
        };
        match value.get("type").and_then(|value| value.as_str()) {
            Some("response.created") => {
                if let Some(response) = value.get("response") {
                    if let Some(id) = response.get("id").and_then(|value| value.as_str()) {
                        self.id = id.to_string();
                    }
                    if let Some(model) = response.get("model").and_then(|value| value.as_str()) {
                        self.model = model.to_string();
                    }
                    self.created = response
                        .get("created_at")
                        .and_then(json_u64)
                        .unwrap_or(self.created);
                }
                Vec::new()
            }
            Some("response.output_text.delta" | "response.refusal.delta") => value
                .get("delta")
                .and_then(|value| value.as_str())
                .filter(|delta| !delta.is_empty())
                .map(|delta| {
                    let mut object = serde_json::Map::new();
                    if !self.started {
                        object.insert("role".into(), serde_json::json!("assistant"));
                        self.started = true;
                    }
                    object.insert("content".into(), serde_json::json!(delta));
                    vec![self.chunk(&serde_json::Value::Object(object), None)]
                })
                .unwrap_or_default(),
            Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
                value
                    .get("delta")
                    .and_then(|value| value.as_str())
                    .filter(|delta| !delta.is_empty())
                    .map(|delta| {
                        let mut object = serde_json::Map::new();
                        if !self.started {
                            object.insert("role".into(), serde_json::json!("assistant"));
                            self.started = true;
                        }
                        object.insert("reasoning_content".into(), serde_json::json!(delta));
                        vec![self.chunk(&serde_json::Value::Object(object), None)]
                    })
                    .unwrap_or_default()
            }
            Some("response.output_item.added") => {
                let Some(item) = value.get("item") else {
                    return Vec::new();
                };
                if item.get("type").and_then(|value| value.as_str()) != Some("function_call") {
                    return Vec::new();
                }
                let index = output_index(&value);
                let call = self.tool_calls.entry(index).or_default();
                call.id = item
                    .get("call_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                call.name = item
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let mut function = serde_json::Map::new();
                function.insert("name".into(), serde_json::json!(call.name));
                function.insert("arguments".into(), serde_json::json!(""));
                let delta = serde_json::json!({
                    "role": "assistant",
                    "tool_calls": [{ "index": index, "id": call.id, "type": "function", "function": function }]
                });
                self.started = true;
                vec![self.chunk(&delta, None)]
            }
            Some("response.function_call_arguments.delta") => {
                let index = output_index(&value);
                let delta = value
                    .get("delta")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.tool_calls
                    .entry(index)
                    .or_default()
                    .arguments
                    .push_str(delta);
                if delta.is_empty() {
                    Vec::new()
                } else {
                    vec![self.chunk(
                        &serde_json::json!({ "tool_calls": [{ "index": index, "function": { "arguments": delta } }] }),
                        None,
                    )]
                }
            }
            Some("response.function_call_arguments.done" | "response.output_item.done") => {
                if value.get("type").and_then(|value| value.as_str())
                    == Some("response.function_call_arguments.done")
                {
                    let index = output_index(&value);
                    if let Some(arguments) = value.get("arguments").and_then(|value| value.as_str())
                    {
                        self.tool_calls.entry(index).or_default().arguments = arguments.to_string();
                    }
                }
                Vec::new()
            }
            Some("response.completed") => {
                self.usage = value
                    .get("response")
                    .and_then(|response| response.get("usage"))
                    .and_then(parse_usage_value);
                Vec::new()
            }
            Some("response.failed") => {
                self.failed = Some(
                    value
                        .get("response")
                        .and_then(|response| response.get("error"))
                        .and_then(|error| error.get("message"))
                        .and_then(|message| message.as_str())
                        .unwrap_or("Codex response failed")
                        .to_string(),
                );
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn chunk(&self, delta: &serde_json::Value, finish_reason: Option<&str>) -> Vec<u8> {
        chat_sse_frame(&serde_json::json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
        }))
    }

    fn usage_chunk(&self, usage: Usage) -> Vec<u8> {
        chat_sse_frame(&serde_json::json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens,
                "prompt_tokens_details": { "cached_tokens": usage.cache_read_tokens },
            },
        }))
    }
}

fn output_index(value: &serde_json::Value) -> usize {
    value
        .get("output_index")
        .and_then(json_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

pub(super) fn codex_instructions(messages: &[ChatMessage]) -> String {
    let s = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if s.is_empty() {
        "You are a helpful assistant.".to_string()
    } else {
        s
    }
}

pub(super) fn codex_input(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter(|m| m.role != "system")
        .flat_map(|m| match m.role.as_str() {
            // A model may request several tools in parallel. Every subsequent
            // function_call_output must have a matching function_call item;
            // emitting only calls[0] makes Codex reject the other outputs.
            "assistant" => match m.tool_calls.as_ref().filter(|calls| !calls.is_empty()) {
                Some(calls) => {
                    let mut items = Vec::with_capacity(calls.len() + usize::from(!m.content.is_empty()));
                    if !m.content.is_empty() {
                        // Keep visible assistant text next to tool calls so a later
                        // Codex request matches the conversation the user saw.
                        items.push(serde_json::json!({
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": m.content, "annotations": [] }],
                        }));
                    }
                    items.extend(calls.iter().map(|call| {
                        serde_json::json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments,
                        })
                    }));
                    items
                }
                None => vec![serde_json::json!({
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": m.content, "annotations": [] }],
                })],
            },
            "tool" => vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.as_deref().unwrap_or_default(),
                "output": m.content,
            })],
            _ => {
                let mut content = Vec::new();
                if !m.content.is_empty() {
                    content.push(serde_json::json!({ "type": "input_text", "text": m.content }));
                }
                for image_url in &m.images {
                    content.push(serde_json::json!({ "type": "input_image", "detail": "auto", "image_url": image_url }));
                }
                vec![serde_json::json!({ "role": "user", "content": content })]
            }
        })
        .collect()
}

pub(super) fn codex_tools(tools: &[ToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
                "strict": false,
            })
        })
        .collect()
}

/// Sticky-routing token the `ChatGPT` backend returns and expects back within a turn.
pub(super) const CODEX_TURN_STATE: &str = "x-codex-turn-state";

/// Keep the turn's sticky-routing token from the first response that sets it.
pub(super) fn remember_turn_state(response: &reqwest::Response, params: &ChatParams) {
    if let Some(state) = response
        .headers()
        .get(CODEX_TURN_STATE)
        .and_then(|value| value.to_str().ok())
    {
        let _ = params.codex_turn_state.set(state.to_string());
    }
}

/// Cache affinity for the `ChatGPT` backend: the `session-id` header, plus the
/// turn's sticky-routing token once the first response has set it.
pub(super) fn codex_cache_headers(
    mut headers: reqwest::header::HeaderMap,
    key: Option<&str>,
    turn_state: Option<&str>,
) -> reqwest::header::HeaderMap {
    if let Some(key) = key.and_then(|key| reqwest::header::HeaderValue::from_str(key).ok()) {
        headers.insert("session-id", key.clone());
        headers.insert("thread-id", key);
    }
    if let Some(state) =
        turn_state.and_then(|state| reqwest::header::HeaderValue::from_str(state).ok())
    {
        headers.insert(CODEX_TURN_STATE, state);
    }
    headers
}

/// Convert an `OpenAI` Chat Completions request into the native Codex
/// Responses request shape. `tools` is used by the in-process client; the
/// host gateway reads the same definitions directly from the JSON body.
#[allow(clippy::too_many_lines)] // one flat pass over the chat-body fields
pub fn chat_body_to_codex_body(
    body: &serde_json::Value,
    stream: bool,
    tools: &[ToolDef],
) -> serde_json::Value {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gpt-5.1-codex-mini");
    let messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let instructions = messages
        .iter()
        .filter(|m| {
            matches!(
                m.get("role").and_then(|r| r.as_str()),
                Some("system" | "developer")
            )
        })
        .filter_map(|m| wire_text(m.get("content")?))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut input = Vec::new();
    for message in &messages {
        let role = message
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("user");
        if matches!(role, "system" | "developer") {
            continue;
        }
        if role == "tool" {
            input.push(serde_json::json!({
                "type": "function_call_output",
                "call_id": message.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or_default(),
                "output": wire_text(message.get("content").unwrap_or(&serde_json::Value::Null)).unwrap_or_default(),
            }));
            continue;
        }
        if role == "assistant"
            && let Some(calls) = message.get("tool_calls").and_then(|v| v.as_array())
        {
            let text = wire_text(message.get("content").unwrap_or(&serde_json::Value::Null))
                .unwrap_or_default();
            if !text.is_empty() {
                input.push(serde_json::json!({
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": text, "annotations": [] }],
                }));
            }
            for call in calls {
                let function = call.get("function").unwrap_or(call);
                input.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": call.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                    "name": function.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                    "arguments": function.get("arguments").and_then(|v| v.as_str()).unwrap_or_default(),
                }));
            }
            continue;
        }
        let mut content = Vec::new();
        match message.get("content") {
            Some(serde_json::Value::String(s)) => {
                content.push(serde_json::json!({ "type": "input_text", "text": s }));
            }
            Some(serde_json::Value::Array(parts)) => {
                for part in parts {
                    match part.get("type").and_then(|t| t.as_str()) {
                        Some("text") => content.push(serde_json::json!({
                            "type": "input_text",
                            "text": part.get("text").and_then(|t| t.as_str()).unwrap_or_default()
                        })),
                        Some("image_url") => content.push(serde_json::json!({
                            "type": "input_image",
                            "detail": "auto",
                            "image_url": part.get("image_url").and_then(|i| i.get("url")).and_then(|u| u.as_str()).unwrap_or_default()
                        })),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        input.push(serde_json::json!({ "role": role, "content": content }));
    }
    let mut out = serde_json::json!({
        "model": model,
        "store": false,
        "stream": stream,
        "instructions": if instructions.is_empty() { "You are a helpful assistant." } else { &instructions },
        "input": input,
        "text": { "verbosity": "low" },
    });
    let object = out.as_object_mut().expect("body is a JSON object");
    if let Some(key) = body.get("prompt_cache_key").filter(|key| key.is_string()) {
        object.insert("prompt_cache_key".into(), key.clone());
    }
    if tools.is_empty() {
        if let Some(chat_tools) = body.get("tools").and_then(|v| v.as_array()) {
            let response_tools = chat_tools
                .iter()
                .filter_map(chat_tool_to_codex_tool)
                .collect::<Vec<_>>();
            if !response_tools.is_empty() {
                object.insert("tools".into(), serde_json::Value::Array(response_tools));
            }
        }
    } else {
        object.insert("tools".into(), serde_json::json!(codex_tools(tools)));
    }
    for key in ["tool_choice", "parallel_tool_calls"] {
        if let Some(value) = body.get(key) {
            object.insert(key.to_string(), value.clone());
        }
    }
    if let Some(effort) = body
        .get("reasoning_effort")
        .and_then(|value| value.as_str())
    {
        object.insert(
            "reasoning".into(),
            serde_json::json!({ "effort": effort, "summary": "auto" }),
        );
    }
    if let Some(max_tokens) = body.get("max_tokens") {
        object.insert("max_output_tokens".into(), max_tokens.clone());
    }
    out
}

fn chat_tool_to_codex_tool(tool: &serde_json::Value) -> Option<serde_json::Value> {
    let function = tool.get("function").unwrap_or(tool);
    let name = function.get("name")?.as_str()?;
    Some(serde_json::json!({
        "type": "function",
        "name": name,
        "description": function.get("description").and_then(|v| v.as_str()).unwrap_or_default(),
        "parameters": function.get("parameters").cloned().unwrap_or(serde_json::json!({})),
        "strict": false,
    }))
}

pub(super) fn codex_response_text(v: &serde_json::Value) -> String {
    if let Some(s) = v.get("output_text").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    v.get("output")
        .and_then(|o| o.as_array())
        .into_iter()
        .flatten()
        .flat_map(|item| {
            item.get("content")
                .and_then(|c| c.as_array())
                .into_iter()
                .flatten()
        })
        .filter_map(|c| {
            c.get("text")
                .or_else(|| c.get("refusal"))
                .and_then(|t| t.as_str())
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(super) fn codex_text_delta(data: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    match v.get("type")?.as_str()? {
        "response.output_text.delta" | "response.refusal.delta" => {
            v.get("delta")?.as_str().map(str::to_string)
        }
        _ => None,
    }
}

pub(super) fn codex_reasoning_delta(data: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    match v.get("type")?.as_str()? {
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            v.get("delta")?.as_str().map(str::to_string)
        }
        _ => None,
    }
}

pub(super) fn accumulate_codex_tool_calls(acc: &mut BTreeMap<usize, ToolCall>, data: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("response.output_item.added") => {
            let Some(item) = v.get("item") else { return };
            if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                return;
            }
            let idx = usize::try_from(
                v.get("output_index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(0);
            let entry = acc.entry(idx).or_default();
            entry.id = item
                .get("call_id")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            entry.name = item
                .get("name")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string();
            entry.arguments.push_str(
                item.get("arguments")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default(),
            );
        }
        Some("response.function_call_arguments.delta") => {
            let idx = usize::try_from(
                v.get("output_index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(0);
            acc.entry(idx)
                .or_default()
                .arguments
                .push_str(v.get("delta").and_then(|s| s.as_str()).unwrap_or_default());
        }
        Some("response.function_call_arguments.done") => {
            let idx = usize::try_from(
                v.get("output_index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(0);
            if let Some(args) = v.get("arguments").and_then(|s| s.as_str()) {
                acc.entry(idx).or_default().arguments = args.to_string();
            }
        }
        Some("response.output_item.done") => {
            let Some(item) = v.get("item") else { return };
            if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                return;
            }
            let idx = usize::try_from(
                v.get("output_index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(0);
            let entry = acc.entry(idx).or_default();
            entry.id = item
                .get("call_id")
                .and_then(|s| s.as_str())
                .unwrap_or(&entry.id)
                .to_string();
            entry.name = item
                .get("name")
                .and_then(|s| s.as_str())
                .unwrap_or(&entry.name)
                .to_string();
            entry.arguments = item
                .get("arguments")
                .and_then(|s| s.as_str())
                .unwrap_or(&entry.arguments)
                .to_string();
        }
        _ => {}
    }
}

/// Pull usage out of a Codex Responses `response.completed` event.
pub(super) fn codex_usage(data: &str) -> Option<Usage> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    v.get("response")?.get("usage").and_then(parse_usage_value)
}

/// Models from a `/codex/models` response: picker-visible entries plus
/// `gpt-reserve`, leaving out efforts this app cannot drive.
pub(super) fn parse_codex_catalog(body: &serde_json::Value) -> Vec<Model> {
    let Some(entries) = body.get("models").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut entries: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|m| {
            m.get("visibility").and_then(serde_json::Value::as_str) == Some("list")
                || m.get("slug").and_then(serde_json::Value::as_str) == Some("gpt-reserve")
        })
        .collect();
    entries.sort_by_key(|m| {
        m.get("priority")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(i64::MAX)
    });
    entries
        .into_iter()
        .filter_map(|m| {
            let id = m.get("slug")?.as_str()?.to_string();
            let levels: Vec<&str> = m
                .get("supported_reasoning_levels")
                .and_then(serde_json::Value::as_array)
                .map(|ls| {
                    ls.iter()
                        .filter_map(|l| l.get("effort").and_then(serde_json::Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            let reasoning_efforts = ReasoningEffort::CYCLE_ORDER
                .iter()
                .copied()
                .filter(|e| levels.contains(&e.as_str()))
                .collect();
            let supports_images = m
                .get("input_modalities")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ms| ms.iter().any(|x| x.as_str() == Some("image")));
            Some(Model {
                name: m
                    .get("display_name")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| id.clone(), str::to_string),
                id,
                reasoning_efforts,
                context_length: m.get("context_window").and_then(serde_json::Value::as_u64),
                supports_images,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: crate::provider::BackendTag::Codex,
                pricing: None,
                aliases: Vec::new(),
            })
        })
        .collect()
}

/// The Codex CLI's own catalog cache (`$CODEX_HOME/models_cache.json`, by
/// default under `~/.codex`), which the CLI keeps fresh. None in tests, so
/// they never read the developer's real cache.
fn codex_cache_path() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".codex")))
        .map(|dir| dir.join("models_cache.json"))
}

pub(super) fn read_codex_cache() -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(codex_cache_path()?).ok()?).ok()
}

/// The client version to ask the catalog for: ours, or the installed Codex
/// CLI's (from its cache) when that is newer.
pub(super) fn codex_client_version(cache: Option<&serde_json::Value>) -> String {
    let parts = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    cache
        .and_then(|c| c.get("client_version")?.as_str())
        .filter(|v| parts(v) > parts(CODEX_CLIENT_VERSION))
        .unwrap_or(CODEX_CLIENT_VERSION)
        .to_string()
}

/// The Codex catalog as of client 0.163.0 (2026-10-26), for when neither the
/// live fetch nor the Codex CLI's cache is available.
pub(super) fn codex_fallback_models() -> Vec<Model> {
    use ReasoningEffort as E;
    let model = |id: &str, name: &str, efforts: &[E]| Model {
        id: id.into(),
        name: name.into(),
        reasoning_efforts: efforts.to_vec(),
        context_length: Some(if id == "gpt-6.1-sol" {
            500_000
        } else {
            272_000
        }),
        supports_images: true,
        supports_image_generation: false,
        supports_video_generation: false,
        backend: crate::provider::BackendTag::Codex,
        pricing: None,
        aliases: Vec::new(),
    };
    let full = [E::Low, E::Medium, E::High, E::XHigh, E::Max];
    vec![
        model("gpt-6.1-sol", "GPT-6.1-Sol", &full),
        model("gpt-6-astra", "GPT-6-Astra", &full),
        model("gpt-6-sol", "GPT-6-Sol", &full),
        model("gpt-reserve", "GPT-Reserve", &full),
        model("gpt-6-luna", "GPT-6-Luna", &full),
        model("gpt-5.6-sol", "GPT-5.6-Sol", &full),
        model("gpt-5.6-terra", "GPT-5.6-Terra", &full),
        model("gpt-5.6-luna", "GPT-5.6-Luna", &full),
    ]
}
