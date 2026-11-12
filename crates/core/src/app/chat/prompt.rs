//! The system prompt and its sections.

use std::fmt::Write as _;

use crate::app::{App, WorkMode, verbosity_clause};
use crate::prompts;
use crate::provider::ChatMessage;

impl App {
    /// Instructions + the active session's memory snapshot, combined into one
    /// system message. Stable base, rendering and environment instructions
    /// precede space instructions, user additions, skills and the frozen
    /// memory snapshot; the date comes last. Space files, apps and scripts are
    /// not listed here — the model lists them with their tools, so importing a
    /// file never changes the cached prefix.
    pub fn system_prompt(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let base = self.resolved_base_system_prompt();
        if !base.is_empty() {
            parts.push(base);
        }
        if self.interactive_ui {
            parts.push(Self::rendering_section());
            parts.push(crate::interactive::instructions());
            if let Some(layouts) = self.layout_guidance() {
                parts.push(layouts);
            }
        }
        parts.push(self.environment_section());
        if self.work_mode.is_agent() {
            parts.push(prompts::render(prompts::AGENT, &[]));
        }
        if !self.incognito {
            let instructions =
                std::fs::read_to_string(self.space.instructions_path(&self.active_space.name))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            if let Some(i) = instructions {
                parts.push(i);
            }
        }
        let overlay = self.resolved_system_prompt_overlay();
        if !overlay.is_empty() {
            parts.push(overlay);
        }
        if let Some(skills) = self.skills_section() {
            parts.push(skills);
        }
        if !self.incognito {
            let memory = self.memory_snapshot();
            if !memory.trim().is_empty() {
                parts.push(format!("## Memory\n{memory}"));
            }
        }
        if self.is_research_session() {
            parts.push(
                "This session came from /research — prefer research_lookup with scope=session_sources over search with mode=web for \
                 follow-ups; only use web search on a miss."
                    .to_string(),
            );
        }
        parts.push(self.now_section());
        parts.join("\n\n")
    }

    pub(in crate::app) fn environment_section(&self) -> String {
        let mut section = "## Environment\n".to_string();
        if let Some((provider, raw_model)) = self
            .current_model
            .as_deref()
            .and_then(|model| self.resolve_model_backend(model))
        {
            let _ = writeln!(
                section,
                "- Backend: {} · model {raw_model}",
                provider.backend_tag().name()
            );
        }
        let _ = write!(
            section,
            "- OS: {} ({})",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        if self.work_mode.is_agent()
            && let Some(workspace) = &self.workspace
        {
            let _ = write!(
                section,
                "\n- Mode: {} · workspace {}",
                if self.work_mode == WorkMode::AgentAuto {
                    "agent·auto"
                } else {
                    "agent"
                },
                workspace.root().display()
            );
        }
        section
    }

    pub(in crate::app) fn layout_guidance(&self) -> Option<String> {
        if self.settings.reply_style == "plain" {
            return None;
        }
        let mut guidance = crate::interactive::LAYOUTS.to_string();
        match self.settings.reply_style.as_str() {
            "rich" => guidance.push_str("\nPrefer a layout for most substantive answers.\n"),
            // Coding replies stay Markdown: in agent mode the interactive
            // style falls back to auto.
            "interactive" if !self.work_mode.is_agent() => {
                guidance.push('\n');
                guidance.push_str(crate::interactive::INTERACTIVE_FIRST);
            }
            _ => {}
        }
        Some(guidance)
    }

    pub(in crate::app) fn rendering_section() -> String {
        "## Rendering\nFenced ```mermaid blocks render as inline diagrams for flowchart, sequenceDiagram and stateDiagram-v2 sources. Other Mermaid diagram types are shown as source text, so prefer the supported kinds or a plain list."
            .to_string()
    }

    pub(in crate::app) fn now_section(&self) -> String {
        format!(
            "## Now\nCurrent date/time: {}. Your training data has a cutoff well before this — trust this timestamp over any date assumption from training.",
            self.prompt_datetime
        )
    }

    /// Bundled base prompt with `{{verbosity}}` and `{{datetime}}` resolved.
    pub fn resolved_base_system_prompt(&self) -> String {
        self.base_system_prompt
            .replace("{{verbosity}}", verbosity_clause(&self.verbosity))
            .replace("{{datetime}}", &self.prompt_datetime)
    }

    /// User additions with the same runtime placeholders as the bundled base.
    pub(in crate::app) fn resolved_system_prompt_overlay(&self) -> String {
        self.system_prompt_overlay
            .trim()
            .replace("{{verbosity}}", verbosity_clause(&self.verbosity))
            .replace("{{datetime}}", &self.prompt_datetime)
    }

    /// Re-read the user additions in `system_prompt.md` after a Ctrl+E hand-edit.
    pub fn reload_base_system_prompt(&mut self) {
        let text = crate::config::load_system_prompt_overlay();
        if text != self.system_prompt_overlay {
            self.system_prompt_overlay = text;
            self.bump_cache_epoch();
        }
        self.push_status("system prompt reloaded".to_string());
    }

    /// Names + descriptions of installed skills and how to invoke one — full
    /// bodies stay off the wire until the model calls the `skill` tool.
    fn skills_section(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut s = "## Skills\nYou have skills available. Their descriptions are metadata only: when a skill \
                     matches the user's task, call the `skills` tool with action=load and its name before \
                     acting; the full instructions will be returned. Load bundled resources by their relative \
                     path only, and do not load every skill spec up front.\n"
            .to_string();
        for skill in &self.skills {
            let _ = writeln!(s, "- {}: {}", skill.name, skill.description);
        }
        Some(s.trim_end().to_string())
    }

    /// A system message with space-file images inline, for vision models.
    /// Returns None when there are no image-type space files.
    pub(super) fn space_images_message(&self) -> Option<ChatMessage> {
        let files_dir = self.space.files_dir(&self.active_space.name);
        let mut urls: Vec<String> = Vec::new();
        let mut names: Vec<&str> = Vec::new();
        for f in &self.caches.files {
            // Judged by content: a renamed PDF is never sent as an image, and
            // a PNG with a wrong extension still is one.
            let Ok(bytes) = std::fs::read(files_dir.join(&f.name)) else {
                continue;
            };
            let Some(mime) = crate::extract::vision_mime(&bytes) else {
                continue;
            };
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
            urls.push(format!("data:{mime};base64,{b64}"));
            names.push(f.name.as_str());
        }
        if urls.is_empty() {
            return None;
        }
        let text = format!(
            "The user has these image files in this space which you can see below: {}",
            names.join(", ")
        );
        Some(ChatMessage {
            role: "system".to_string(),
            content: text,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            images: urls,
            cache_breakpoint: false,
        })
    }

    /// Names of the active space's existing apps (directory listing).
    pub fn list_apps(&self) -> Vec<String> {
        let dir = self.space.apps_dir(&self.active_space.name);
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut apps: Vec<String> = rd
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        apps.sort();
        apps
    }
}
