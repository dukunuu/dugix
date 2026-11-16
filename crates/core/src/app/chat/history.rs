//! The message history sent to the model: masking, budget, cache breakpoints, tool-call replay.

use crate::db::Message;
use crate::provider::profile::{ModelProfile, profile_for};
use crate::provider::{ChatMessage, ToolCall};

use super::App;
use super::ui_repair::{ui_failure_notes, ui_program_history};
use super::user_command::user_command_replay;

impl App {
    pub(in crate::app) fn model_profile(&self) -> ModelProfile {
        profile_for(
            self.models.iter().find(|model| {
                self.current_model.as_deref() == Some(super::composite_id(model).as_str())
            }),
            self.saved.local.as_ref(),
        )
    }

    pub fn maybe_mask(&mut self) {
        if !self.experiments.mask_tool_results || self.is_streaming() {
            return;
        }
        let trigger = if self.experiments.mask_trigger_tokens > 0 {
            self.experiments.mask_trigger_tokens
        } else if self.experiments.compaction_budget_tokens > 0 {
            self.experiments.compaction_budget_tokens / 2
        } else if let Some(limit) = self.context_limit() {
            limit / 2
        } else {
            return;
        };
        if self.context_used() < trigger {
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let keep = self.experiments.mask_keep_rounds as usize;
        let through = if keep == 0 {
            self.messages.len()
        } else {
            let Some((index, _)) = self
                .messages
                .iter()
                .enumerate()
                .filter(|(i, m)| {
                    m.role == "tool_call" && (*i == 0 || self.messages[*i - 1].role != "tool_call")
                })
                .rev()
                .nth(keep - 1)
            else {
                return;
            };
            index
        };
        if through <= self.mask_through {
            return;
        }
        let start = self
            .mask_through
            .max(self.messages.len() - self.effective_messages().len());
        let mut count = 0;
        let mut chars = 0;
        for message in &self.messages[start.min(through)..through] {
            if message.role == "tool_call"
                && !Self::excluded_from_model_history(message)
                && let Some(row) = parse_tool_call_row(&message.content)
                && crate::tools::classify_tool_result(&row.result, "").is_none()
            {
                count += 1;
                chars += row.result.chars().count();
            }
        }
        let tokens = chars / 4;
        if tokens < 2_000 {
            return;
        }
        if !self.incognito {
            let _ = self.db.set_session_mask(&session.id, through);
        }
        self.mask_through = through;
        self.bump_cache_epoch();
        self.push_status(format!(
            "masked {count} old tool results (~{tokens} tokens) · experiment"
        ));
    }

    pub(in crate::app) fn effective_conversation_chars(&self) -> usize {
        let effective = self.effective_messages();
        let offset = self.messages.len() - effective.len();
        effective
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role != "compaction")
            .map(|(index, m)| {
                let chars = m.content.chars().count();
                if self.experiments.mask_tool_results
                    && offset + index < self.mask_through
                    && m.role == "tool_call"
                    && let Some(row) = parse_tool_call_row(&m.content)
                    && crate::tools::classify_tool_result(&row.result, "").is_none()
                {
                    let note =
                        crate::tools::tool_result_masked_note(&row.call.name, &row.call.arguments);
                    return chars.saturating_sub(
                        serde_json::Value::String(row.result)
                            .to_string()
                            .chars()
                            .count(),
                    ) + serde_json::Value::String(note).to_string().chars().count();
                }
                chars
            })
            .sum()
    }

    /// Reconstruct the wire history with compaction, images, and tool-call replay.
    #[allow(clippy::too_many_lines)]
    pub fn build_history(&mut self) -> Vec<ChatMessage> {
        let mut history: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 3);
        self.ensure_prompt_prefix();
        if let Some(prefix) = self.active_prefix() {
            history.push(ChatMessage::text("system", prefix.system.clone()));
            if let Some(images) = &prefix.lead_images {
                history.push(images.clone());
            }
        } else {
            history.push(ChatMessage::text("system", self.system_prompt()));
            if self.current_model_supports_images()
                && let Some(images) = self.space_images_message()
            {
                history.push(images);
            }
        }
        if let Some(summary) = self
            .session
            .as_ref()
            .and_then(|s| s.compact_summary.clone())
        {
            history.push(ChatMessage::text(
                "system",
                format!("Summary of earlier conversation (auto-compacted for length):\n{summary}"),
            ));
        }
        let vision = self.current_model_supports_images();
        let result_cap = self.model_profile().result_cap;
        // Replay and the live tool loop must deduplicate identically to preserve cached prefixes.
        let mut seen_results: std::collections::HashMap<(String, String), String> =
            std::collections::HashMap::new();
        let effective = self.effective_messages();
        let first_owner = self.messages.len() - effective.len();
        let (ui_replay, ui_errors) = if self.interactive_ui {
            ui_program_history(effective, first_owner)
        } else {
            (std::collections::BTreeMap::new(), Vec::new())
        };
        let ui_feedback_key = effective
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.role == "assistant")
            .map(|(index, m)| {
                use std::hash::{Hash, Hasher};
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                m.content.hash(&mut hash);
                format!(
                    "{}:{}:{}",
                    self.session.as_ref().map_or("", |s| s.id.as_str()),
                    first_owner + index,
                    hash.finish()
                )
            });
        let mask_through = if self.experiments.mask_tool_results {
            self.mask_through
                .saturating_sub(self.messages.len() - effective.len())
        } else {
            0
        };
        let tool_images_dir = self
            .toolbox
            .supports_images()
            .then(|| self.toolbox.space_files_dir())
            .flatten();
        let mut index = 0;
        // Recall prefixes must remain byte-identical across requests for provider caching.
        let mut recall: Option<String> = None;
        while index < effective.len() {
            let m = &effective[index];
            if m.role == "recall" {
                recall = crate::memory::recall::Recall::from_row(&m.content).map(|r| r.text);
                index += 1;
                continue;
            }
            if Self::excluded_from_model_history(m) {
                index += 1;
                continue;
            }
            if m.role == "tool_call" {
                append_tool_call_history(
                    &mut history,
                    effective,
                    &mut index,
                    &mut seen_results,
                    tool_images_dir.as_deref(),
                    result_cap,
                    mask_through,
                );
                continue;
            }
            if m.role == "mode" {
                // Sent where the mode changed, so the turns before it stay cached.
                if let Some(text) = Self::mode_note_message(m) {
                    history.push(ChatMessage::text("user", text));
                }
                index += 1;
                continue;
            }
            let mut cm = if m.role == "user_command" {
                ChatMessage::text("user", user_command_replay(&m.content))
            } else if m.role == "skill" {
                let (name, body) = m
                    .content
                    .split_once('\n')
                    .unwrap_or((m.content.as_str(), ""));
                ChatMessage::text(
                    "user",
                    format!(
                        "The user invoked the skill '{name}'. Follow these instructions for the rest of this conversation:\n{body}"
                    ),
                )
            } else if m.role == "user"
                && let Some(hint) = recall.take()
            {
                ChatMessage::text("user", format!("{hint}\n\n{}", m.content))
            } else {
                ChatMessage::text(m.role.clone(), m.content.clone())
            };
            // Removing an older turn's diagnostic would invalidate the cached prefix.
            if m.role == "assistant" {
                if let Some(content) = ui_replay.get(&index) {
                    cm.content.clone_from(content);
                }
                cm.content.push_str(&ui_failure_notes(&m.content));
            }
            if m.role == "user" && vision {
                let images_dir = self.space.files_dir(&self.active_space.name);
                let mut images = Vec::new();
                let mut rest = m.content.as_str();
                while let Some(start) = rest.find("![") {
                    if let Some(end) = rest[start..].find(')') {
                        let inner = &rest[start + 2..start + end];
                        if let Some((_alt, file)) = inner.split_once("](")
                            && let Some(url) =
                                crate::app::transcribe::image_ref_data_url(&images_dir, file)
                        {
                            images.push(url);
                        }
                        rest = &rest[start + end + 1..];
                    } else {
                        break;
                    }
                }
                cm.images = images;
            }
            history.push(cm);
            index += 1;
        }
        if !ui_errors.is_empty()
            && let Some(key) = ui_feedback_key
            && self.ui_repair.feedback_seen.insert(key)
        {
            history.push(ChatMessage::text(
                "system",
                format!("nexus-ui errors: {}", serde_json::Value::Array(ui_errors)),
            ));
        }
        self.mark_cache_breakpoints(&mut history);
        history
    }

    // Breakpoints must precede the live tail so each turn preserves the cached prefix.
    fn mark_cache_breakpoints(&self, history: &mut [ChatMessage]) {
        if !self.wants_explicit_cache_breakpoints() {
            return;
        }
        let last_system = history
            .iter()
            .position(|message| message.role != "system")
            .unwrap_or(history.len())
            .saturating_sub(1);
        if let Some(message) = history.get_mut(last_system) {
            message.cache_breakpoint = true;
        }
        if history.len() >= last_system + 3
            && let Some(message) = history.get_mut(history.len() - 2)
        {
            message.cache_breakpoint = true;
        }
    }

    // Anthropic requires explicit markers; auto-caching providers may reject them.
    #[must_use]
    pub fn wants_explicit_cache_breakpoints(&self) -> bool {
        self.current_model
            .as_deref()
            .is_some_and(super::model_needs_cache_breakpoints)
    }
}

fn append_tool_call_history(
    history: &mut Vec<ChatMessage>,
    effective: &[Message],
    index: &mut usize,
    seen_results: &mut std::collections::HashMap<(String, String), String>,
    tool_images_dir: Option<&std::path::Path>,
    result_cap: Option<usize>,
    mask_through: usize,
) {
    let start = *index;
    let mut rows = Vec::new();
    while *index < effective.len() && effective[*index].role == "tool_call" {
        if let Some(row) = parse_tool_call_row(&effective[*index].content) {
            let masked = *index < mask_through
                && crate::tools::classify_tool_result(&row.result, "").is_none();
            rows.push((row, masked));
        }
        *index += 1;
    }
    if rows.is_empty() {
        debug_assert!(*index > start);
        return;
    }
    // One assistant message per step, each followed by its own results, exactly
    // as the live loop sent them; merging steps would change the replayed
    // prefix and miss the provider's prompt cache on the next turn.
    let mut steps: Vec<Vec<(StoredToolCall, bool)>> = Vec::new();
    for (row, masked) in rows {
        match steps.last_mut() {
            Some(step) if !row.starts_step() => step.push((row, masked)),
            _ => steps.push(vec![(row, masked)]),
        }
    }
    for step in steps {
        append_tool_step(history, step, seen_results, tool_images_dir, result_cap);
    }
}

fn append_tool_step(
    history: &mut Vec<ChatMessage>,
    rows: Vec<(StoredToolCall, bool)>,
    seen_results: &mut std::collections::HashMap<(String, String), String>,
    tool_images_dir: Option<&std::path::Path>,
    result_cap: Option<usize>,
) {
    let calls = rows
        .iter()
        .map(|(row, _)| row.call.clone())
        .collect::<Vec<_>>();
    let reasoning_content = rows
        .iter()
        .find_map(|(row, _)| row.reasoning_content.clone());
    let assistant_content = rows
        .iter()
        .find_map(|(row, _)| row.assistant_content.clone());
    history.push(ChatMessage {
        role: "assistant".to_string(),
        content: assistant_content.unwrap_or_default(),
        reasoning_content,
        tool_calls: Some(calls),
        tool_call_id: None,
        images: Vec::new(),
        cache_breakpoint: false,
    });
    // Image messages follow the last tool result; providers reject a user message between results.
    let mut image_messages = Vec::new();
    for (row, masked) in rows {
        let call = row.call;
        let images = tool_images_dir
            .filter(|_| !masked)
            .and_then(|dir| crate::provider::openrouter::extract_tool_images(&row.result, dir));
        let key = (call.name.clone(), call.arguments.clone());
        let visible = result_cap.map_or_else(
            || row.result.clone(),
            |cap| crate::tools::cap_head_tail(&row.result, cap),
        );
        let content = if masked {
            crate::tools::tool_result_masked_note(&call.name, &call.arguments)
        } else {
            match seen_results.get(&key) {
                Some(prev) if *prev == visible => {
                    crate::tools::tool_result_unchanged_note(&call.name, &call.arguments)
                }
                _ => {
                    seen_results.insert(key, visible.clone());
                    visible
                }
            }
        };
        history.push(ChatMessage {
            role: "tool".to_string(),
            content,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call.id),
            images: Vec::new(),
            cache_breakpoint: false,
        });
        if let Some(images) = images {
            image_messages.push(ChatMessage {
                role: "user".to_string(),
                content: images.description,
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                images: images.urls,
                cache_breakpoint: false,
            });
        }
    }
    history.extend(image_messages);
}

struct StoredToolCall {
    call: ToolCall,
    result: String,
    reasoning_content: Option<String>,
    assistant_content: Option<String>,
    /// `None` on rows written before steps were recorded.
    step_start: Option<bool>,
}

impl StoredToolCall {
    /// Whether this row opens a new assistant step. Older rows carry no flag:
    /// a step's reasoning and visible text ride on its first call only, so
    /// either one marks a boundary.
    fn starts_step(&self) -> bool {
        self.step_start
            .unwrap_or_else(|| self.reasoning_content.is_some() || self.assistant_content.is_some())
    }
}

fn parse_tool_call_row(content: &str) -> Option<StoredToolCall> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    let arguments = v.get("arguments")?.as_str()?.to_string();
    let result = v.get("result")?.as_str()?.to_string();
    Some(StoredToolCall {
        call: ToolCall {
            id: v
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .unwrap_or("call_0")
                .to_string(),
            name,
            arguments,
        },
        result,
        reasoning_content: v
            .get("reasoning_content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        assistant_content: v
            .get("assistant_content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        step_start: v.get("step_start").and_then(serde_json::Value::as_bool),
    })
}

pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// Separate inline reasoning tags from answer content, including truncated reasoning blocks.
pub fn split_inline_reasoning(text: &str) -> (String, Option<String>) {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut content = String::with_capacity(text.len());
    let mut reasoning = String::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find(OPEN) else {
            content.push_str(rest);
            break;
        };
        content.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];
        let (block, remainder) = match after_open.find(CLOSE) {
            Some(end) => (&after_open[..end], &after_open[end + CLOSE.len()..]),
            None => (after_open, ""),
        };
        if !reasoning.is_empty() {
            reasoning.push('\n');
        }
        reasoning.push_str(block.trim());
        rest = remainder;
    }
    let content = content.trim().to_string();
    (content, (!reasoning.is_empty()).then_some(reasoning))
}
