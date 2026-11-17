//! The work mode, told to the model where it changes instead of in the system
//! prompt. A provider's prompt cache holds only while a request starts with
//! the bytes of an earlier one, so rewriting the system prompt on a mode
//! switch would discard the cache of the whole conversation. A `mode` row in
//! the transcript is sent as a message at its own position: everything before
//! it stays cached.

use anyhow::Result;

use super::{App, WorkMode};
use crate::db::Message;
use crate::prompts;

/// The mode and workspace a `mode` row recorded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModeNote {
    /// `chat`, `agent` or `agent·auto`.
    pub mode: String,
    pub workspace: Option<String>,
    /// What the model is sent for this row, kept so a replay is byte-identical
    /// even after the bundled agent prompt changes.
    pub text: String,
}

impl ModeNote {
    #[must_use]
    pub fn from_row(content: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(content).ok()?;
        Some(Self {
            mode: value["mode"].as_str()?.to_string(),
            workspace: value["workspace"].as_str().map(str::to_string),
            text: value["text"].as_str()?.to_string(),
        })
    }

    fn to_row(&self) -> String {
        serde_json::json!({
            "mode": self.mode,
            "workspace": self.workspace,
            "text": self.text,
        })
        .to_string()
    }
}

fn mode_name(mode: WorkMode) -> &'static str {
    match mode {
        WorkMode::Chat => "chat",
        WorkMode::Agent => "agent",
        WorkMode::AgentAuto => "agent·auto",
    }
}

impl App {
    /// `- Mode: agent · workspace <root>` in agent mode.
    pub(super) fn mode_line(&self) -> Option<String> {
        let workspace = self
            .workspace
            .as_ref()
            .filter(|_| self.work_mode.is_agent())?;
        Some(format!(
            "- Mode: {} · workspace {}",
            mode_name(self.work_mode),
            workspace.root().display()
        ))
    }

    /// What the model is told when the conversation enters the current mode.
    #[must_use]
    pub fn mode_note_text(&self) -> String {
        let Some(line) = self.mode_line() else {
            return "[The user switched this conversation to chat mode. Agent mode is off and there is no workspace: fs, shell, proc, task and agents refuse until the user turns it on again with /agent <path>. The agent-mode instructions above no longer apply.]".to_string();
        };
        let mut text = format!(
            "[The user switched this conversation to agent mode. These instructions apply until the mode changes again.]\n\n{}\n{line}",
            prompts::render(prompts::AGENT, &[])
        );
        if self.interactive_ui && self.settings.reply_style == "interactive" {
            text.push_str(
                "\n- While working in the workspace, answer in plain text; interactive layouts are for chat answers.",
            );
        }
        text
    }

    fn noted_mode(&self) -> (String, Option<String>) {
        self.effective_messages()
            .iter()
            .rev()
            .filter(|message| message.role == "mode")
            .find_map(|message| ModeNote::from_row(&message.content))
            .map_or_else(
                || ("chat".to_string(), None),
                |note| (note.mode, note.workspace),
            )
    }

    /// Record the mode in the transcript when it differs from what the model
    /// was last told, so the coming request carries it. Switching back and
    /// forth without sending anything leaves no trace.
    pub(super) fn note_mode_change(&mut self, session_id: &str) -> Result<()> {
        let workspace = self
            .workspace
            .as_ref()
            .filter(|_| self.work_mode.is_agent())
            .map(|workspace| workspace.root().display().to_string());
        let mode = mode_name(self.work_mode).to_string();
        if (mode.clone(), workspace.clone()) == self.noted_mode() {
            return Ok(());
        }
        let note = ModeNote {
            mode,
            workspace,
            text: self.mode_note_text(),
        };
        self.push_note(session_id, "mode", note.to_row())
    }

    /// The message a `mode` row is replayed as.
    pub(super) fn mode_note_message(row: &Message) -> Option<String> {
        ModeNote::from_row(&row.content).map(|note| note.text)
    }
}
