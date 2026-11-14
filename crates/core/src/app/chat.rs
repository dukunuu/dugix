#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
mod history;
mod prompt;
mod ui_repair;
mod user_command;

pub use history::{human_size, split_inline_reasoning};
pub(super) use ui_repair::UiRepairResult;
pub use ui_repair::code_blocks;
use ui_repair::{UiRepairTarget, repair_image_links};
pub(super) use user_command::UserCommandContext;
pub(crate) use user_command::UserCommandRun;
pub use user_command::{user_command_outcome, user_command_outcome_label};

#[cfg(test)]
use ui_repair::{failing_ui_blocks, ui_failure_notes, ui_program_history};
#[cfg(test)]
use user_command::{
    HeadTail, USER_COMMAND_INTERACTIVE_NOTE, USER_COMMAND_KEEP_BYTES,
    USER_COMMAND_STREAM_OPEN_NOTE, USER_COMMAND_TIMEOUT, capture_user_command, user_command_replay,
    user_command_row,
};

use anyhow::Result;
use chrono::Utc;
use tokio::sync::mpsc;

use crate::db::Message;
use crate::prompts;
use crate::provider::{ChatMessage, ChatParams, StreamEvent, Usage};

use super::{
    App, AppEvent, SPINNER_COLORS, SpinnerColor, THINKING, WorkMode, composite_id,
    model_needs_cache_breakpoints, parse_topic,
};

pub const MERMAID_PREVIEW_EXAMPLES: &[&str] = &[
    "flowchart LR\nA-->B",
    "sequenceDiagram\nAlice->>Bob: Hello",
    "stateDiagram-v2\n[*] --> Idle",
];

impl App {
    #[allow(clippy::too_many_lines)]
    pub fn send_message(&mut self, text: String) -> Result<()> {
        if let Some(session) = self.session.as_ref()
            && let Some(task) = self.chat_task_for_session(&session.id)
        {
            self.push_status(format!(
                "wait — response still streaming in: {}",
                task.session_title
            ));
            self.push_composer_set(&text);
            return Ok(());
        }
        if self.chat_task_count() >= super::MAX_CHAT_TASKS {
            self.push_status(format!(
                "chat task limit reached ({})",
                super::MAX_CHAT_TASKS
            ));
            self.push_composer_set(&text);
            return Ok(());
        }
        if !self.backends.any() {
            self.push_status("set your API key first with /login".to_string());
            self.push_composer_set(&text);
            return Ok(());
        }
        self.meter.turn_cache = super::CacheTally::default();
        let Some(model) = self.current_model.clone() else {
            self.push_status("pick a model first with /model".to_string());
            self.push_composer_set(&text);
            return Ok(());
        };

        let session_id = self.ensure_session(&text, model)?;
        self.ensure_prompt_prefix();

        if let Some(name) = self.forced_skill.take()
            && let Some(skill) = self.skills.iter().find(|s| s.name == name)
        {
            let body = std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map(|md| crate::skills::skill_body(&md).to_string())
                .unwrap_or_default();
            let content = format!("{name}\n{body}");
            if !self.incognito {
                self.db.add_skill_message(&session_id, &content)?;
            }
            self.messages.push(Message {
                role: "skill".to_string(),
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
        if let Some(hint) = self.memory_hint(&text) {
            self.push_note(&session_id, "recall", hint.to_row())?;
        }
        let text = self.dated_user_message(text, Utc::now());
        if !self.incognito {
            self.db.add_user_message(&session_id, &text)?;
        }
        self.ui_repair.messages.remove(&session_id);
        self.messages.push(Message {
            role: "user".to_string(),
            content: text,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });

        if self.session.as_ref().is_some_and(|s| s.swarm_mode) {
            self.start_swarm_turn();
            return Ok(());
        }
        self.start_stream()
    }

    pub(super) fn ensure_session(&mut self, text: &str, model: String) -> Result<String> {
        if let Some(session) = &self.session {
            return Ok(session.id.clone());
        }
        self.mask_through = 0;
        self.refresh_memory_snapshot();
        let title = title_from(text);
        let s = if self.incognito {
            crate::db::Session {
                id: uuid::Uuid::new_v4().to_string(),
                title,
                model,
                slug: None,
                created_at: Utc::now().to_rfc3339(),
                compact_summary: None,
                compact_through: 0,
                swarm_mode: false,
                kind: "chat".to_string(),
                research_parent_id: None,
            }
        } else {
            self.db
                .create_session(&title, &model, &self.active_space.id, "chat")?
        };
        if !self.incognito && self.work_mode.is_agent() {
            self.db.set_session_workspace(
                &s.id,
                self.workspace.as_ref().map(|workspace| workspace.root()),
            )?;
            self.db
                .set_session_workspace_mode(&s.id, self.work_mode.binding_mode())?;
        }
        let id = s.id.clone();
        self.session = Some(s);
        self.rebuild_toolbox();
        Ok(id)
    }

    pub fn retry_last_reply(&mut self) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            self.push_status("nothing to retry — no active session");
            return Ok(());
        };
        if self.is_streaming() {
            self.push_status("wait — response still streaming");
            return Ok(());
        }
        if session.swarm_mode || session.kind == "research" {
            self.push_status("/retry is not available in swarm or research sessions");
            return Ok(());
        }
        let Some(user_index) = self
            .messages
            .iter()
            .rposition(|message| matches!(message.role.as_str(), "user" | "user_command"))
        else {
            self.push_status("nothing to retry — no user message");
            return Ok(());
        };
        if user_index + 1 == self.messages.len() {
            self.push_status("nothing to retry — no reply after the last user message");
            return Ok(());
        }
        let session_id = session.id.clone();
        let changed_files = self.work_mode.is_agent()
            && self
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.turn_changed_files(&session_id));
        if !self.incognito {
            for id in self.db.reply_ids_after_last_user(&session_id)? {
                self.db.delete_message(&id)?;
            }
        }
        self.messages.truncate(user_index + 1);
        if self.mask_through > self.messages.len() {
            self.mask_through = self.messages.len();
            if !self.incognito {
                let _ = self.db.set_session_mask(&session_id, self.mask_through);
            }
        }
        self.push_history_invalidated();
        if let Some((turn_session, turn_id)) = &self.last_turn
            && turn_session == &session_id
            && let Some(store) = &self.trace_store
            && let Ok(steps) = store.steps_for_turn(turn_id)
        {
            for step in steps {
                let _ = store.add_label(&step, "rejected", "retry");
            }
        }
        self.start_stream()?;
        if changed_files {
            self.push_status(
                "retrying · the replaced turn's file changes are kept · /undo walks back one agent turn at a time",
            );
        }
        Ok(())
    }

    pub fn undo_last_turn(&mut self) -> Result<()> {
        if self.is_streaming() {
            self.push_status("wait — response still streaming");
            return Ok(());
        }
        let Some(workspace) = self.workspace.clone().filter(|_| self.work_mode.is_agent()) else {
            self.push_status("/undo works in agent mode · switch with /agent <path>");
            return Ok(());
        };
        let report = match &self.session {
            Some(session) => workspace.undo_last(&session.id),
            None => Ok(None),
        };
        match report {
            Ok(Some(report)) => self.push_status(report.summary()),
            Ok(None) => self.push_status("nothing to undo — no agent file changes in this session"),
            Err(error) => self.push_status(format!("undo failed: {error}")),
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub fn start_stream(&mut self) -> Result<()> {
        if let Some(session) = &self.session {
            self.ui_repair.messages.remove(&session.id);
        }
        let Some(model) = self.current_model.clone() else {
            return Ok(());
        };
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            self.push_status(format!(
                "model backend unavailable: {model} — pick another with /model"
            ));
            return Ok(());
        };
        let history = self.build_history();
        // Catalog changes can invalidate persisted reasoning preferences.
        let stored_effort = self.reasoning.get(&model).cloned();
        let (reasoning_effort, reasoning_warning) = match stored_effort {
            Some(effort) if self.effort_accepted(&model, &effort) => (Some(effort), None),
            Some(effort) => {
                self.db.set_reasoning(&model, None)?;
                self.reasoning.remove(&model);
                (
                    None,
                    Some(format!("cleared unsupported reasoning {effort}: {model}")),
                )
            }
            None => (None, None),
        };
        self.push_status(reasoning_warning.unwrap_or_default());
        self.warn_stale_prefix();
        self.note_turn_lane();
        let params = self.chat_params(reasoning_effort, provider.backend_tag());
        let tools = self
            .active_prefix()
            .map_or_else(|| self.chat_tool_defs(), |prefix| prefix.tools.clone());
        if provider.backend_tag() == crate::provider::BackendTag::Local
            && !self.meter.warned
            && let Some(limit) = self.context_limit()
        {
            let defs = serde_json::to_string(&tools)?;
            let tokens = (self.system_prompt().chars().count() + defs.chars().count()) as u64 / 4;
            if tokens > limit / 2 {
                self.push_status(format!(
                    "prompt + tool definitions use ~{tokens} of {limit} tokens — raise [local] context_length"
                ));
                self.meter.warned = true;
            }
        }
        if self.work_mode.is_agent()
            && let (Some(workspace), Some(session)) = (&self.workspace, &self.session)
        {
            workspace.begin_turn(&session.id);
        }
        if let Some(session) = &self.session {
            let task = self
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content.chars().take(2_000).collect())
                .unwrap_or_default();
            *self.decision_context.lock().expect("decision context") = (session.id.clone(), task);
        }
        let backend = provider.backend_tag();
        let (rx, abort) = self.usage_provider(provider, "chat").stream_chat(
            raw_model.clone(),
            history,
            params,
            tools,
            self.toolbox.clone(),
            crate::provider::openrouter::MAX_TOOL_ITERS,
        );
        let Some(session) = self.session.clone() else {
            return Ok(());
        };
        let (thinking_idx, spinner_color) = pick_flavor();
        let task_id = self.next_chat_task_id;
        self.next_chat_task_id = self.next_chat_task_id.wrapping_add(1);
        let tx = self.chat_event_tx.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            while let Some(event) = rx.recv().await {
                if tx.send(super::ChatEvent { task_id, event }).is_err() {
                    break;
                }
            }
            let _ = tx.send(super::ChatEvent {
                task_id,
                event: StreamEvent::Done,
            });
        });
        self.chat_tasks.insert(
            task_id,
            super::ChatTask {
                id: task_id,
                session_id: session.id,
                session_title: session.title,
                space_id: self.active_space.id.clone(),
                model,
                model_id: raw_model,
                backend,
                incognito: self.incognito,
                buffer: String::new(),
                thinking: String::new(),
                tool_status: None,
                usage: None,
                usage_row_id: None,
                started: std::time::Instant::now(),
                thinking_idx,
                spinner_color,
                abort,
            },
        );
        self.spinner_frame = 0;
        self.thinking_idx = thinking_idx;
        self.spinner_color = spinner_color;
        self.push_viewport_reset();
        Ok(())
    }

    #[must_use]
    pub fn chat_tool_defs(&self) -> Vec<crate::provider::ToolDef> {
        if self.model_profile().compact_tools {
            self.toolbox.defs_compact()
        } else {
            self.toolbox.defs()
        }
    }

    /// A chat turn's request parameters; compaction sends the same to reuse its cache.
    /// The parameters and tool list a request needs to share the chat's
    /// cached prefix: the turn's own, with the effort the catalog still accepts.
    pub(super) fn turn_shape(&self) -> (ChatParams, Vec<crate::provider::ToolDef>) {
        let effort = self.current_model.clone().and_then(|model| {
            let effort = self.reasoning.get(&model).cloned()?;
            self.effort_accepted(&model, &effort).then_some(effort)
        });
        let tools = self
            .active_prefix()
            .map_or_else(|| self.chat_tool_defs(), |prefix| prefix.tools.clone());
        (self.turn_params(effort), tools)
    }

    pub(super) fn turn_params(&self, reasoning_effort: Option<String>) -> ChatParams {
        let profile = self.model_profile();
        ChatParams {
            reasoning_effort,
            prompt_cache_key: self
                .session
                .as_ref()
                .map(|session| self.prompt_cache_key_for(&session.id)),
            cache_ttl_1h: self.settings.cache_lifetime == "extended",
            cache_long_openai: self.settings.cache_long_openai,
            temperature: self.settings.temperature,
            top_p: self.settings.top_p,
            max_tokens: self.settings.max_tokens,
            parallel_tool_calls: (!profile.parallel_calls).then_some(false),
            tool_result_cap: profile.result_cap,
            ..ChatParams::default()
        }
    }

    pub(super) fn chat_params(
        &mut self,
        reasoning_effort: Option<String>,
        backend: crate::provider::BackendTag,
    ) -> ChatParams {
        let mut params = self.turn_params(reasoning_effort);
        if self.training_enabled()
            && let Some(session_id) = self.session.as_ref().map(|session| session.id.clone())
            && let Some(store) = self.open_trace_store()
        {
            let turn_id = uuid::Uuid::new_v4().to_string();
            self.last_turn = Some((session_id.clone(), turn_id.clone()));
            params.trace = Some(crate::trace::TraceContext {
                store,
                session_id,
                space_id: self.active_space.id.clone(),
                turn_id,
                backend: backend.name().to_string(),
            });
        }
        params
    }

    #[cfg(test)]
    pub fn on_stream_event(&mut self, ev: StreamEvent) -> Result<()> {
        let task_id = self
            .active_chat_task()
            .map(|task| task.id)
            .or_else(|| self.chat_tasks.keys().next().copied());
        if let Some(task_id) = task_id {
            return self.on_chat_event(task_id, ev);
        }
        if let StreamEvent::ToolCall {
            id,
            reasoning,
            assistant_content,
            step_start,
            name,
            arguments,
            result,
        } = ev
        {
            let content = serde_json::json!({
                "id": id,
                "reasoning_content": reasoning,
                "assistant_content": assistant_content,
                "step_start": step_start,
                "name": name,
                "arguments": arguments,
                "result": result,
            })
            .to_string();
            if let Some(session) = self.session.as_ref()
                && !self.incognito
            {
                let _ = self.db.add_tool_call_message(&session.id, &content);
            }
            self.messages.push(Message {
                role: "tool_call".to_string(),
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
    pub fn on_chat_event(&mut self, task_id: super::ChatTaskId, ev: StreamEvent) -> Result<()> {
        match ev {
            StreamEvent::Token(t) => {
                if let Some(task) = self.chat_tasks.get_mut(&task_id) {
                    task.buffer.push_str(&t);
                }
            }
            StreamEvent::Reasoning(t) => {
                if let Some(task) = self.chat_tasks.get_mut(&task_id) {
                    task.thinking.push_str(&t);
                }
            }
            StreamEvent::Usage(u) => {
                // OpenCode Zen's trailing cost-only event revises the latest request's usage row.
                let has_tokens = u.prompt_tokens + u.completion_tokens > 0;
                if has_tokens {
                    self.meter.last_cache_rate = u.cache_hit_rate();
                    self.meter.turn_cache.observe(&u);
                }
                let Some(task) = self.chat_tasks.get_mut(&task_id) else {
                    return Ok(());
                };
                let accounted = has_tokens
                    .then(|| {
                        (
                            task.session_id.clone(),
                            task.backend,
                            task.model_id.clone(),
                            task.incognito,
                        )
                    })
                    .and_then(|(session_id, backend, model_id, incognito)| {
                        self.account_request(&session_id, backend, &model_id, incognito, &u)
                    });
                let Some(task) = self.chat_tasks.get_mut(&task_id) else {
                    return Ok(());
                };
                let merged = if has_tokens {
                    // A new API request must not inherit the previous request's provider cost.
                    u
                } else {
                    task.usage.map_or(u, |prev| Usage {
                        cost: u.cost.or(prev.cost),
                        ..prev
                    })
                };
                task.usage = Some(merged);
                let session_id = task.session_id.clone();
                if has_tokens
                    && self
                        .session
                        .as_ref()
                        .is_some_and(|session| session.id == session_id)
                {
                    let total = if u.total_tokens > 0 {
                        u.total_tokens
                    } else {
                        u.prompt_tokens + u.completion_tokens
                    };
                    self.observe_context(task_id, total);
                }
                let Some(task) = self.chat_tasks.get_mut(&task_id) else {
                    return Ok(());
                };
                let space_id = task.space_id.clone();
                let model_id = task.model_id.clone();
                let backend = task.backend;
                let incognito = task.incognito;
                let row_id = task.usage_row_id;
                let billable = merged.prompt_tokens > 0
                    || merged.completion_tokens > 0
                    || merged.cost.is_some();
                // Other backends use their OpenRouter catalog twin when provider cost is absent.
                let cost_is_provider = merged.cost.is_some();
                let cost = merged.cost.or_else(|| {
                    billable
                        .then(|| {
                            self.db.request_cost(
                                &model_id,
                                merged.prompt_tokens,
                                merged.completion_tokens,
                                merged.cache_read_tokens,
                                merged.cache_creation_tokens,
                            )
                        })
                        .flatten()
                });
                self.turn_usage
                    .entry(task_id)
                    .or_default()
                    .observe(has_tokens.then_some(merged.completion_tokens), cost);
                if !incognito && billable {
                    if has_tokens || row_id.is_none() {
                        let id = self.db.log_usage(
                            backend.name(),
                            &model_id,
                            merged.prompt_tokens,
                            merged.completion_tokens,
                            merged.cache_read_tokens,
                            merged.cache_creation_tokens,
                            merged.prompt_convention,
                            cost,
                            cost_is_provider,
                            Some(&session_id),
                            Some(&space_id),
                        );
                        if let Ok(id) = id
                            && let Some(task) = self.chat_tasks.get_mut(&task_id)
                        {
                            let _ = self.db.set_usage_kind(id, "chat");
                            task.usage_row_id = Some(id);
                            if let Some(request) = &accounted {
                                self.store_accounted_request(id, request);
                            }
                        }
                    } else if let Some(row_id) = row_id {
                        let _ = self.db.update_usage(
                            row_id,
                            merged.prompt_tokens,
                            merged.completion_tokens,
                            merged.cache_read_tokens,
                            merged.cache_creation_tokens,
                            cost,
                            cost_is_provider,
                        );
                    }
                }
            }
            StreamEvent::Status(s) => {
                if let Some(task) = self.chat_tasks.get_mut(&task_id) {
                    task.tool_status = Some(s);
                }
            }
            StreamEvent::Notice(message) => self.push_status(message),
            StreamEvent::Subagent(update) => {
                let Some(task) = self.chat_tasks.get(&task_id) else {
                    return Ok(());
                };
                let (session_id, incognito) = (task.session_id.clone(), task.incognito);
                if let Some(cost) = self.on_subagent(&session_id, incognito, *update) {
                    *self.subagent_turn_cost.entry(task_id).or_default() += cost;
                }
            }
            StreamEvent::ToolCall {
                id,
                reasoning,
                assistant_content,
                step_start,
                name,
                arguments,
                result,
            } => {
                if ((name == "skills")
                    || (name == "skill_admin")
                    || (name == "install_skill" && result.starts_with("installed"))
                    || (name == "create_skill"
                        && (result.starts_with("created") || result.starts_with("updated"))))
                    && (result.starts_with("installed")
                        || result.starts_with("created")
                        || result.starts_with("updated"))
                {
                    self.reload_skills();
                }
                let Some(task) = self.chat_tasks.get_mut(&task_id) else {
                    return Ok(());
                };
                if !task.incognito
                    && let Some(id) = task.usage_row_id
                {
                    let _ = self.db.add_usage_tool(id, &name);
                }
                // Unattached provider reasoning is display-only and must not be replayed.
                let streamed_thinking = std::mem::take(&mut task.thinking);
                task.buffer.clear();
                let thinking = (reasoning.is_none() && !streamed_thinking.trim().is_empty())
                    .then_some(streamed_thinking);
                let content = serde_json::json!({
                    "id": id,
                    "reasoning_content": reasoning,
                    "thinking": thinking,
                    "assistant_content": assistant_content,
                    "step_start": step_start,
                    "name": name,
                    "arguments": arguments,
                    "result": result,
                })
                .to_string();
                let target = task.session_id.clone();
                let incognito = task.incognito;
                let in_active_space = task.space_id == self.active_space.id;
                let viewing = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.id == target);
                if !incognito {
                    let _ = self.db.add_tool_call_message(&target, &content);
                }
                if viewing {
                    self.messages.push(Message {
                        role: "tool_call".to_string(),
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
                if viewing
                    && ((name == "propose_command" && result.starts_with("proposed to the user"))
                        || (name == "request_workspace" && result.starts_with("approved:")))
                {
                    let args =
                        serde_json::from_str::<serde_json::Value>(&arguments).unwrap_or_default();
                    let arg = |key: &str| args[key].as_str().unwrap_or_default().trim().to_string();
                    if name == "propose_command" {
                        self.approvals.proposed_command = Some(format!("!{}", arg("cmd")));
                        self.push_status(arg("reason"));
                    } else {
                        // Never swap the toolbox mid-turn.
                        self.queued_workspace = Some((task_id, arg("root")));
                    }
                }
                let media_generated = matches!(
                    name.as_str(),
                    "generate_image" | "generate_video" | "video_transform"
                ) || matches!(
                    name.as_str(),
                    "edit_video" | "extract_frame" | "stitch_videos"
                ) || (name == "media"
                    && serde_json::from_str::<serde_json::Value>(&arguments)
                        .ok()
                        .and_then(|v| v.get("action").and_then(|a| a.as_str()).map(str::to_string))
                        .is_some_and(|a| {
                            matches!(
                                a.as_str(),
                                "generate_image"
                                    | "generate_video"
                                    | "edit"
                                    | "extract_frame"
                                    | "stitch"
                            )
                        }));
                if in_active_space && media_generated {
                    self.rescan_files();
                }
            }
            StreamEvent::Done => self.finish_chat_task(task_id, None)?,
            StreamEvent::Error(e) => {
                self.finish_chat_task(task_id, Some(e))?;
            }
        }
        Ok(())
    }

    pub fn cancel_chat_task(&mut self, task_id: super::ChatTaskId) -> Result<()> {
        if let Some(task) = self.chat_tasks.remove(&task_id) {
            task.abort.abort();
            self.finish_chat_task_state(task, None, true)?;
        }
        Ok(())
    }

    pub fn stop_stream(&mut self) -> Result<()> {
        let Some(task_id) = self.active_chat_task().map(|task| task.id) else {
            return Ok(());
        };
        self.cancel_chat_task(task_id)
    }

    pub fn discard_chat_task(&mut self, session_id: &str) {
        if let Some(task) = self
            .chat_tasks
            .iter()
            .find(|(_, task)| task.session_id == session_id)
            .map(|(id, _)| *id)
            .and_then(|id| self.chat_tasks.remove(&id))
        {
            task.abort.abort();
            if self
                .session
                .as_ref()
                .is_some_and(|session| session.id == session_id)
            {
                self.approvals.pending = None;
                self.approvals.pending_request = None;
            }
        }
    }

    fn finish_chat_task(
        &mut self,
        task_id: super::ChatTaskId,
        error: Option<String>,
    ) -> Result<()> {
        let Some(task) = self.chat_tasks.remove(&task_id) else {
            return Ok(());
        };
        self.finish_chat_task_state(task, error, false)
    }

    fn finish_chat_task_state(
        &mut self,
        task: super::ChatTask,
        error: Option<String>,
        stopped: bool,
    ) -> Result<()> {
        use crate::deck::link::{ActivityState, TurnOutcome};

        let id = task.id;
        self.end_subagents(&task.session_id, task.incognito);
        let watching = self
            .deck_link
            .lock()
            .expect("deck link")
            .turns
            .contains_key(&id);
        let outcome = watching.then(|| {
            let scrubber = crate::trace::Scrubber::new(self.saved_secrets());
            let (answer, _) = split_inline_reasoning(&task.buffer);
            let (state, detail) = if let Some(error) = &error {
                (ActivityState::Failed, scrubber.scrub(error))
            } else if stopped {
                (ActivityState::Stopped, "Response stopped".to_string())
            } else {
                (
                    ActivityState::Done,
                    scrubber.scrub(&crate::markdown::to_plain(&answer)),
                )
            };
            TurnOutcome {
                state,
                detail: detail.chars().take(240).collect(),
            }
        });
        let result = self.persist_chat_task_state(task, error, stopped);
        if let Some(mut outcome) = outcome {
            if result.is_err() {
                outcome.state = ActivityState::Failed;
                outcome.detail = "Could not save the response; see nexus for details".into();
            }
            let mut link = self.deck_link.lock().expect("deck link");
            link.turns.insert(id, Some(outcome));
            link.notify.notify_one();
        }
        result
    }

    #[allow(clippy::too_many_lines)]
    fn persist_chat_task_state(
        &mut self,
        task: super::ChatTask,
        error: Option<String>,
        stopped: bool,
    ) -> Result<()> {
        let helpers_cost = self.subagent_turn_cost.remove(&task.id);
        let turn = self.turn_usage.remove(&task.id).unwrap_or_default();
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.id == task.session_id)
        {
            self.approvals.pending = None;
            self.approvals.pending_request = None;
        }
        if let Some((_, root)) = self.queued_workspace.take_if(|(id, _)| *id == task.id)
            && self
                .session
                .as_ref()
                .is_some_and(|session| session.id == task.session_id)
        {
            self.execute(super::AppCommand::SetWorkMode {
                mode: WorkMode::Agent,
                root: Some(root),
            })?;
        }
        let mut reasoning = task.thinking;
        let (buf, inline) = split_inline_reasoning(&task.buffer);
        if let Some(inline) = inline {
            if !reasoning.is_empty() {
                reasoning.push('\n');
            }
            reasoning.push_str(&inline);
        }
        if buf.is_empty() && error.is_none() {
            self.push_status(if stopped {
                "response stopped".to_string()
            } else {
                "response finished without text".to_string()
            });
            return Ok(());
        }
        // Some providers inline reasoning tags in content instead of sending reasoning deltas.
        let viewing = self
            .session
            .as_ref()
            .is_some_and(|session| session.id == task.session_id);
        let model = Some(task.model.clone());
        let usage = task.usage;
        let tokens = Some(if usage.is_some() {
            turn.output_tokens as i64
        } else {
            buf.chars().count().div_ceil(4) as i64
        });
        if viewing && let Some(u) = usage {
            // Some providers omit the total token count.
            let total = if u.total_tokens > 0 {
                u.total_tokens
            } else {
                u.prompt_tokens + u.completion_tokens
            };
            if self.meter.basis.is_none_or(|basis| basis.task != task.id) {
                self.meter.total = Some(total);
            }
        }
        let secs = Some(task.started.elapsed().as_secs_f64());
        let cost = turn.cost();
        let cost = match (cost, helpers_cost) {
            (Some(own), Some(helpers)) => Some(own + helpers),
            (own, helpers) => own.or(helpers),
        };
        let reasoning = (!reasoning.is_empty()).then_some(reasoning);
        let phrase = Some(THINKING[task.thinking_idx].1.to_string());
        let created_at = Some(chrono::Utc::now().to_rfc3339());
        let stored_messages = if task.incognito {
            Vec::new()
        } else {
            self.db.load_messages(&task.session_id)?
        };
        let tool_messages = if task.incognito && viewing {
            &self.messages
        } else {
            &stored_messages
        };
        let (buf, fixed_images) = repair_image_links(&buf, tool_messages);

        let message_id = if !task.incognito && !buf.is_empty() {
            self.db.add_assistant_message(
                &task.session_id,
                &buf,
                model.as_deref(),
                reasoning.as_deref(),
                tokens,
                secs,
                cost,
                phrase.as_deref(),
            )?
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        let repair = (viewing && self.interactive_ui).then(|| UiRepairTarget {
            session_id: task.session_id.clone(),
            message_id,
            original: buf.clone(),
            incognito: task.incognito,
            model_id: task.model_id.clone(),
            backend: task.backend,
            space_id: task.space_id.clone(),
        });
        if viewing && !buf.is_empty() {
            self.messages.push(Message {
                role: "assistant".to_string(),
                content: buf,
                model,
                reasoning,
                tokens,
                secs,
                cost,
                phrase,
                persona: None,
                created_at,
            });
            // These jobs read active-session state, not the completed task's origin.
            if !task.incognito {
                self.maybe_generate_title();
            }
            self.maybe_mask();
            if !task.incognito {
                self.maybe_compact();
            }
        }
        if let Some(error) = error {
            if !task.incognito {
                self.db.add_error_message(&task.session_id, &error)?;
            }
            let msg = format!("stream error: {error}");
            if viewing {
                self.messages.push(Message {
                    role: "error".to_string(),
                    content: error.clone(),
                    model: None,
                    reasoning: None,
                    tokens: None,
                    secs: None,
                    cost: None,
                    phrase: None,
                    persona: None,
                    created_at: None,
                });
            } else if !task.incognito {
                self.unread.insert(task.session_id.clone());
            }
            self.push_status(if viewing {
                msg.clone()
            } else {
                format!("stream error in {}: {error}", task.session_title)
            });
            if !viewing && !task.incognito {
                super::send_system_notification(
                    &format!("Chat failed: {}", task.session_title),
                    &msg,
                );
            }
            if !viewing && !task.incognito {
                self.notifications.push_back(super::ChatNotification {
                    session_id: task.session_id,
                    title: task.session_title,
                    text: msg,
                    success: false,
                });
            }
        } else if stopped {
            self.push_status("response stopped".to_string());
        } else {
            if !viewing && !task.incognito {
                self.unread.insert(task.session_id.clone());
                self.push_status(format!("✓ response ready in: {}", task.session_title));
                super::send_system_notification(
                    &format!("Chat ready: {}", task.session_title),
                    "response complete",
                );
            } else if viewing {
                self.push_status("response complete".to_string());
            }
            if !viewing && !task.incognito {
                self.notifications.push_back(super::ChatNotification {
                    session_id: task.session_id,
                    title: task.session_title,
                    text: "response complete".to_string(),
                    success: true,
                });
            }
        }
        if fixed_images > 0 {
            self.push_status(format!(
                "fixed {fixed_images} image link{}",
                if fixed_images == 1 { "" } else { "s" }
            ));
        }
        if let Some(target) = repair {
            self.start_ui_repair(target);
        }
        Ok(())
    }

    pub fn maybe_generate_title(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Some((provider, raw_model)) = self
            .current_model
            .as_deref()
            .and_then(|model| self.resolve_model_backend(model))
        else {
            return;
        };
        if session.slug.is_some() {
            return;
        }
        let convo: String = self
            .messages
            .iter()
            .filter(|m| m.role != "tool_call")
            .map(|m| {
                format!(
                    "{}: {}",
                    m.role,
                    m.content.chars().take(500).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let session_id = session.id.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.title_rx = Some(rx);
        let provider = self.usage_provider(provider, "title");
        tokio::spawn(async move {
            let prompt = prompts::render(prompts::TITLE, &[("conversation", &convo)]);
            let msgs = vec![ChatMessage::text("user", prompt)];
            if let Ok(text) = provider.complete(&raw_model, msgs).await
                && let Some((topic, slug)) = parse_topic(&text)
            {
                let _ = tx.send((session_id, topic, slug));
            }
        });
    }

    pub fn on_title_result(&mut self, result: Option<(String, String, String)>) {
        self.title_rx = None;
        let Some((id, topic, slug)) = result else {
            return;
        };
        let _ = self.db.set_session_title(&id, &topic, Some(&slug));
        if let Some(s) = self.session.as_mut().filter(|s| s.id == id) {
            s.title.clone_from(&topic);
            s.slug = Some(slug.clone());
        }
        if let Some(s) = self.caches.sessions.iter_mut().find(|s| s.id == id) {
            s.title = topic;
            s.slug = Some(slug);
        }
    }

    /// Follow a session link at the selected transcript message index.
    pub fn open_session_link(&mut self, owner: Option<usize>) {
        let Some(msg) = owner.and_then(|i| self.messages.get(i)) else {
            self.push_status(
                "select text on a session link message, then press Ctrl+O".to_string(),
            );
            return;
        };
        if msg.role != "session_link" {
            self.push_status(
                "select text on a session link message, then press Ctrl+O".to_string(),
            );
            return;
        }
        let Some(target) = msg
            .content
            .split_once('\n')
            .map(|(s, _)| s.trim().to_string())
        else {
            self.push_status("malformed session link".to_string());
            return;
        };
        if let Err(e) = self.switch_to_session_by_id(&target) {
            self.push_status(format!("session switch failed: {e}"));
        }
    }

    /// Pin or discard a citation selected in the transcript.
    pub fn flag_source_under_selection(
        &mut self,
        flag: Option<&str>,
        selected: Option<String>,
        owner: Option<usize>,
    ) {
        let Some(selected) = selected else {
            self.push_status("select a [n] citation first".to_string());
            return;
        };
        let Some(n) = crate::citations::citation_number_in(&selected) else {
            self.push_status("no [n] citation in the current selection".to_string());
            return;
        };
        let Some(msg) = owner.and_then(|i| self.messages.get(i)) else {
            self.push_status("no [n] citation in the current selection".to_string());
            return;
        };
        let citations = crate::citations::parse_citations(&msg.content);
        let Some((_, url)) = citations.iter().find(|(num, _)| *num == n) else {
            self.push_status(format!("no source [{n}] in this message"));
            return;
        };
        let Some(session) = &self.session else {
            self.push_status("no active session".to_string());
            return;
        };
        let url_norm = crate::tools::normalize_url(url);
        let verb = match flag {
            Some("discarded") => "discarded",
            Some(_) => "pinned",
            None => "cleared",
        };
        match self.db.set_source_flag(&session.id, &url_norm, flag) {
            Ok(()) => {
                self.push_status(format!("{verb} [{n}]: {url}"));
                self.refresh_toolbox();
            }
            Err(e) => self.push_status(format!("flag failed: {e}")),
        }
    }

    pub fn toggle_incognito(&mut self) -> Result<()> {
        if self.is_streaming() {
            self.stop_stream()?;
        }
        self.session = None;
        self.messages.clear();
        self.mask_through = 0;
        self.refresh_memory_snapshot();
        self.meter.total = None;
        self.push_composer_clear();
        self.push_viewport_reset();
        self.cleanup_incognito_images();
        self.incognito = !self.incognito;
        self.work_mode = WorkMode::Chat;
        self.workspace = None;
        self.refresh_toolbox();
        self.push_status(if self.incognito {
            "incognito mode — nothing persists, no apps".to_string()
        } else {
            "incognito mode off".to_string()
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "ui_language_tests.rs"]
mod ui_language_tests;

pub fn pick_greeting() -> &'static str {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    super::GREETINGS[n % super::GREETINGS.len()]
}

pub fn pick_flavor() -> (usize, SpinnerColor) {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    (n % THINKING.len(), SPINNER_COLORS[n % SPINNER_COLORS.len()])
}

pub fn title_from(text: &str) -> String {
    let t: String = text.chars().take(40).collect();
    if t.trim().is_empty() {
        "new chat".to_string()
    } else {
        t
    }
}

#[cfg(test)]
#[path = "chat/image_tests.rs"]
mod image_source_tests;

#[cfg(test)]
mod tests;
