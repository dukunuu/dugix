//! Messages sent while the conversation is still answering wait here, per
//! session, and go out one by one as each reply finishes. A stopped or failed
//! reply holds the rest until the user sends again, so nothing fires after a
//! turn they cut short.

use anyhow::Result;

use super::{App, AppEvent};

/// Queued messages one conversation holds; more are refused, not dropped.
pub const MAX_QUEUED_MESSAGES: usize = 32;

/// What `/queue` does to the waiting messages; positions are 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QueueAction {
    List,
    /// Send the first waiting message now (a held queue starts again).
    Send,
    /// Move one back into the composer; `None` is the newest.
    Edit(Option<usize>),
    Drop(usize),
    Clear,
}

impl App {
    /// The messages waiting in the open conversation, oldest first.
    pub fn queued_messages(&self) -> &[String] {
        self.session
            .as_ref()
            .and_then(|session| self.queued.get(&session.id))
            .map_or(&[], Vec::as_slice)
    }

    /// A reply (or a swarm turn) is still being written in this conversation.
    pub(super) fn session_busy(&self, session_id: &str) -> bool {
        self.chat_task_for_session(session_id).is_some()
            || (self.swarm.rx.is_some() && self.swarm.session.as_deref() == Some(session_id))
    }

    /// Keep `text` for after the running reply. `false` when the queue is full.
    pub(super) fn enqueue_message(&mut self, session_id: &str, text: String) -> bool {
        let queue = self.queued.entry(session_id.to_string()).or_default();
        if queue.len() >= MAX_QUEUED_MESSAGES {
            self.push_status(format!(
                "queue full ({MAX_QUEUED_MESSAGES} waiting) — wait for a reply or /queue clear"
            ));
            self.push_composer_set(&text);
            return false;
        }
        queue.push(text);
        let waiting = queue.len();
        self.push_status(format!(
            "queued ({waiting} waiting) — sends when the reply finishes"
        ));
        true
    }

    fn pop_queued(&mut self, session_id: &str) -> Option<String> {
        let queue = self.queued.get_mut(session_id)?;
        let next = (!queue.is_empty()).then(|| queue.remove(0));
        if queue.is_empty() {
            self.queued.remove(session_id);
        }
        next
    }

    /// The text to send now when the user sends `text` into an idle
    /// conversation: waiting messages go first, so order is kept.
    pub(super) fn ahead_of_queue(&mut self, session_id: &str, text: String) -> String {
        match self.pop_queued(session_id) {
            Some(first) => {
                self.queued
                    .entry(session_id.to_string())
                    .or_default()
                    .push(text);
                first
            }
            None => text,
        }
    }

    /// Send the first waiting message of the open conversation, if it is idle.
    pub fn send_next_queued(&mut self) -> Result<bool> {
        let Some(id) = self.session.as_ref().map(|session| session.id.clone()) else {
            return Ok(false);
        };
        if self.session_busy(&id) {
            return Ok(false);
        }
        let Some(text) = self.pop_queued(&id) else {
            return Ok(false);
        };
        self.send_now(text)?;
        Ok(true)
    }

    /// A turn of `session_id` ended: send its next waiting message, or — after
    /// a stop or an error, or when another conversation is open — hold them.
    pub(super) fn after_turn(&mut self, session_id: &str, hold: bool) -> Result<()> {
        // Background agents that finished during the turn report now.
        self.queue_reports(session_id);
        let waiting = self.queued.get(session_id).map_or(0, Vec::len);
        if waiting == 0 {
            return Ok(());
        }
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        if hold || !viewing {
            if viewing {
                self.push_status(format!(
                    "{waiting} queued message{} held — Enter sends the next, /queue clear drops them",
                    if waiting == 1 { "" } else { "s" }
                ));
            }
            return Ok(());
        }
        self.send_next_queued().map(|_| ())
    }

    pub fn run_queue_action(&mut self, action: QueueAction) -> Result<()> {
        let Some(id) = self.session.as_ref().map(|session| session.id.clone()) else {
            self.push_status("no messages queued");
            return Ok(());
        };
        let waiting = self.queued.get(&id).map_or(0, Vec::len);
        if waiting == 0 {
            self.push_status("no messages queued");
            return Ok(());
        }
        match action {
            QueueAction::List => {
                let list: Vec<String> = self.queued[&id]
                    .iter()
                    .enumerate()
                    .map(|(i, text)| format!("{}. {}", i + 1, one_line(text, 60)))
                    .collect();
                self.push_status(format!("queued: {}", list.join(" · ")));
            }
            QueueAction::Send => {
                if !self.send_next_queued()? {
                    self.push_status("wait — response still streaming");
                }
            }
            QueueAction::Edit(position) => {
                let position = position.unwrap_or(waiting);
                let Some(text) = self.remove_queued(&id, position) else {
                    return Ok(());
                };
                self.pending_events.push_back(AppEvent::ComposerSet(text));
                self.push_status("editing a queued message — Enter queues it again");
            }
            QueueAction::Drop(position) => {
                if let Some(text) = self.remove_queued(&id, position) {
                    self.push_status(format!("dropped queued: {}", one_line(&text, 60)));
                }
            }
            QueueAction::Clear => {
                self.queued.remove(&id);
                self.push_status(format!(
                    "dropped {waiting} queued message{}",
                    if waiting == 1 { "" } else { "s" }
                ));
            }
        }
        Ok(())
    }

    fn remove_queued(&mut self, session_id: &str, position: usize) -> Option<String> {
        let queue = self.queued.get_mut(session_id)?;
        if position == 0 || position > queue.len() {
            let waiting = queue.len();
            self.push_status(format!(
                "no queued message {position} — there are {waiting}"
            ));
            return None;
        }
        let text = queue.remove(position - 1);
        if queue.is_empty() {
            self.queued.remove(session_id);
        }
        Some(text)
    }
}

/// `/queue [send|edit [n]|drop <n>|clear]`.
pub(super) fn parse(arg: &str) -> Result<QueueAction, String> {
    let mut words = arg.split_whitespace();
    let verb = words.next().unwrap_or_default();
    let number = words.next().map(str::parse::<usize>);
    match (verb, number) {
        ("" | "list", None) => Ok(QueueAction::List),
        ("send" | "next", None) => Ok(QueueAction::Send),
        ("clear", None) => Ok(QueueAction::Clear),
        ("edit", None) => Ok(QueueAction::Edit(None)),
        ("edit", Some(Ok(n))) => Ok(QueueAction::Edit(Some(n))),
        ("drop" | "rm" | "remove", Some(Ok(n))) => Ok(QueueAction::Drop(n)),
        _ => Err("usage: /queue [send | edit [n] | drop <n> | clear]".into()),
    }
}

/// The first line of `text`, cut to `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() > max {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}
