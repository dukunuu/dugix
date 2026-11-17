//! Background `task` runs: started by one turn, they keep working after it
//! ends. The registry is app-owned and shared across toolbox refreshes; a
//! finished run's report waits here until `agents wait` collects it or the app
//! delivers it to the conversation, whichever comes first — never both.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{Context, Reserved, run_reserved};
use crate::provider::{StreamEvent, SubagentUpdate};
use crate::tools::ToolBox;

/// Background runs one conversation may have going at once.
pub const MAX_RUNNING: usize = 6;
/// Longest one `agents wait` blocks; the model waits again for longer work.
const MAX_WAIT_S: u64 = 600;

/// What a background run tells the app.
pub enum BackgroundMsg {
    /// Progress for `/agents`, as a turn's own sub-agents report it.
    Update {
        session_id: String,
        update: Box<SubagentUpdate>,
    },
    /// A run of this conversation ended; its report can be taken.
    Finished { session_id: String },
    /// A run was stopped before it finished, so no `Finished` update follows.
    Stopped { session_id: String, number: u64 },
}

/// A finished run's result, taken exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub number: u64,
    pub title: String,
    pub text: String,
}

struct Entry {
    session_id: String,
    title: String,
    /// `None` while it runs.
    report: Option<String>,
    abort: Option<tokio::task::AbortHandle>,
}

pub struct Background {
    runs: Mutex<BTreeMap<u64, Entry>>,
    events: mpsc::UnboundedSender<BackgroundMsg>,
    changed: tokio::sync::Notify,
}

impl Background {
    #[must_use]
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<BackgroundMsg>) {
        let (events, receiver) = mpsc::unbounded_channel();
        let background = Arc::new(Self {
            runs: Mutex::default(),
            events,
            changed: tokio::sync::Notify::new(),
        });
        (background, receiver)
    }

    /// Runs of this conversation still working.
    #[must_use]
    pub fn running(&self, session_id: &str) -> usize {
        self.runs
            .lock()
            .expect("background runs")
            .values()
            .filter(|run| run.session_id == session_id && run.report.is_none())
            .count()
    }

    pub(crate) fn check_room(&self, session_id: &str) -> Result<()> {
        if self.running(session_id) >= MAX_RUNNING {
            bail!(
                "{MAX_RUNNING} background agents already running · wait for one with agents action=wait"
            );
        }
        Ok(())
    }

    /// Start `args` as a detached run and return its number.
    pub(crate) fn start(
        self: &Arc<Self>,
        context: &Context,
        toolbox: Arc<ToolBox>,
        args: Value,
        reserved: Reserved,
    ) -> u64 {
        let session_id = toolbox.session_id.clone();
        let number = reserved.number;
        let title = args["prompt"]
            .as_str()
            .and_then(|prompt| prompt.lines().next())
            .unwrap_or_default()
            .trim()
            .to_string();
        // The turn's event channel closes with the turn; this one is the app's.
        let (events, mut updates) = mpsc::unbounded_channel();
        let context = context.reporting_to(events);
        let sink = self.events.clone();
        let forward_session = session_id.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(event) = updates.recv().await {
                if let StreamEvent::Subagent(update) = event {
                    let _ = sink.send(BackgroundMsg::Update {
                        session_id: forward_session.clone(),
                        update,
                    });
                }
            }
        });
        // Registered before the run starts, so a run that ends at once still finds its entry.
        self.runs.lock().expect("background runs").insert(
            number,
            Entry {
                session_id: session_id.clone(),
                title,
                report: None,
                abort: None,
            },
        );
        let registry = Arc::clone(self);
        let handle = tokio::spawn(async move {
            let text = run_reserved(&context, &toolbox, &args, reserved)
                .await
                .unwrap_or_else(|error| format!("sub-agent failed: {error}"));
            // Closing the run's channel lets its last progress reach the app
            // before the app hears that the report is ready.
            drop(context);
            let _ = forwarder.await;
            registry.finish(number, text);
        });
        if let Some(run) = self.runs.lock().expect("background runs").get_mut(&number)
            && run.report.is_none()
        {
            run.abort = Some(handle.abort_handle());
        }
        number
    }

    fn finish(&self, number: u64, text: String) {
        let session_id = {
            let mut runs = self.runs.lock().expect("background runs");
            let Some(run) = runs.get_mut(&number) else {
                return;
            };
            run.report = Some(text);
            run.abort = None;
            run.session_id.clone()
        };
        self.changed.notify_waiters();
        let _ = self.events.send(BackgroundMsg::Finished { session_id });
    }

    /// A finished run nobody has read yet, without running anything.
    #[cfg(test)]
    pub(crate) fn insert_finished(&self, session_id: &str, number: u64, title: &str, text: &str) {
        self.runs.lock().expect("background runs").insert(
            number,
            Entry {
                session_id: session_id.into(),
                title: title.into(),
                report: Some(text.into()),
                abort: None,
            },
        );
    }

    /// Take every finished, uncollected report of this conversation.
    pub fn take_reports(&self, session_id: &str) -> Vec<Report> {
        let mut runs = self.runs.lock().expect("background runs");
        let done: Vec<u64> = runs
            .iter()
            .filter(|(_, run)| run.session_id == session_id && run.report.is_some())
            .map(|(number, _)| *number)
            .collect();
        done.into_iter()
            .filter_map(|number| {
                let run = runs.remove(&number)?;
                Some(Report {
                    number,
                    title: run.title,
                    text: run.report?,
                })
            })
            .collect()
    }

    /// Block until run `number` — or, without one, every run of the
    /// conversation — has finished, then return the reports. On a timeout the
    /// finished ones are returned and the rest are named as still running.
    pub(crate) async fn wait(&self, session_id: &str, args: &Value) -> String {
        let seconds = args["wait_s"].as_u64().unwrap_or(300).clamp(1, MAX_WAIT_S);
        let only = args["id"].as_u64();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        loop {
            let notified = self.changed.notified();
            let (known, pending) = {
                let runs = self.runs.lock().expect("background runs");
                let mine = || {
                    runs.iter().filter(|(number, run)| {
                        run.session_id == session_id && only.is_none_or(|id| id == **number)
                    })
                };
                (
                    mine().count(),
                    mine().filter(|(_, run)| run.report.is_none()).count(),
                )
            };
            if known == 0 {
                return match only {
                    Some(id) => format!(
                        "no background agent {id} to wait for · its report was already delivered, or it never started"
                    ),
                    None => "no background agents to wait for".into(),
                };
            }
            let timed_out = tokio::time::Instant::now() >= deadline;
            if pending == 0 || timed_out {
                let mut out = Vec::new();
                let mut runs = self.runs.lock().expect("background runs");
                let numbers: Vec<u64> = runs
                    .iter()
                    .filter(|(number, run)| {
                        run.session_id == session_id && only.is_none_or(|id| id == **number)
                    })
                    .map(|(number, _)| *number)
                    .collect();
                for number in numbers {
                    if runs[&number].report.is_some() {
                        let run = runs.remove(&number).expect("listed run");
                        out.push(format!(
                            "agent {number} ({}) finished:\n{}",
                            run.title,
                            run.report.unwrap_or_default()
                        ));
                    } else {
                        out.push(format!(
                            "agent {number} ({}) is still running after {seconds}s · wait again",
                            runs[&number].title
                        ));
                    }
                }
                return out.join("\n\n");
            }
            let _ = tokio::time::timeout_at(deadline, notified).await;
        }
    }

    /// This conversation's background runs, one per line.
    #[must_use]
    pub fn list(&self, session_id: &str) -> String {
        let runs = self.runs.lock().expect("background runs");
        let lines: Vec<String> = runs
            .iter()
            .filter(|(_, run)| run.session_id == session_id)
            .map(|(number, run)| {
                let state = if run.report.is_some() {
                    "finished, report not yet read"
                } else {
                    "running"
                };
                format!("{number} · {state} · {}", run.title)
            })
            .collect();
        if lines.is_empty() {
            "no background agents".into()
        } else {
            lines.join("\n")
        }
    }

    /// Stop one running background agent of this conversation.
    pub fn stop(&self, session_id: &str, number: u64) -> Result<String> {
        let mut runs = self.runs.lock().expect("background runs");
        let Some(run) = runs.get(&number).filter(|run| run.session_id == session_id) else {
            bail!("no background agent {number}");
        };
        if run.report.is_some() {
            bail!("background agent {number} already finished · read it with agents action=wait");
        }
        let run = runs.remove(&number).expect("checked run");
        if let Some(abort) = run.abort {
            abort.abort();
        }
        drop(runs);
        self.changed.notify_waiters();
        let _ = self.events.send(BackgroundMsg::Stopped {
            session_id: session_id.to_string(),
            number,
        });
        Ok(format!("stopped background agent {number} ({})", run.title))
    }

    /// Stop and forget every run of a conversation that is going away.
    pub fn stop_session(&self, session_id: &str) {
        let mut runs = self.runs.lock().expect("background runs");
        runs.retain(|_, run| {
            if run.session_id != session_id {
                return true;
            }
            if let Some(abort) = &run.abort {
                abort.abort();
            }
            false
        });
        drop(runs);
        self.changed.notify_waiters();
    }
}
