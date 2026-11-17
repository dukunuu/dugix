//! The fleet from the app's side: `/fleet` starts, steers and stops external
//! agent CLIs, and an agent that needs the user — a permission prompt, a
//! finished turn, an exit — says so in the status line.

use std::time::Duration;

use anyhow::Result;

use super::App;
use crate::fleet::{FleetMsg, Kind, Launch, State};

/// A turn shorter than this ends without a notice: typing into an attached
/// agent makes it "work" for a moment too.
const NOTABLE_TURN: Duration = Duration::from_secs(4);

/// What `/fleet` does.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FleetAction {
    /// Show the board; a frontend's job.
    Open,
    Start {
        kind: Kind,
        prompt: String,
        /// In the CLI's own naming; `None` is its default.
        model: Option<String>,
        /// Its own git worktree and branch, where there is a repository.
        isolate: bool,
    },
    /// Type a follow-up into an agent and submit it.
    Send {
        id: u64,
        text: String,
    },
    Stop {
        id: u64,
    },
    /// Bring a worktree agent's changes into the user's tree, staged.
    Merge {
        id: u64,
    },
    /// Run the project's check in a worktree agent's checkout.
    Check {
        id: u64,
    },
}

impl App {
    /// Where a user-started agent works: the agent workspace, else the
    /// directory Dugix was started in.
    fn fleet_cwd(&self) -> std::path::PathBuf {
        self.workspace
            .as_ref()
            .filter(|_| self.work_mode.is_agent())
            .map(|workspace| workspace.root().to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(std::env::temp_dir)
    }

    pub fn run_fleet_action(&mut self, action: FleetAction) -> Result<()> {
        match action {
            FleetAction::Open => {}
            FleetAction::Start {
                kind,
                prompt,
                model,
                isolate,
            } => {
                let cwd = self.fleet_cwd();
                let session = self
                    .session
                    .as_ref()
                    .map(|session| session.id.clone())
                    .unwrap_or_default();
                let place = crate::usershell::display_dir(&cwd);
                let mut launch = Launch::agent(kind, &prompt, model.as_deref(), cwd);
                launch.isolate = isolate;
                let id = self.fleet.spawn(&session, launch)?;
                let home = match self.fleet.info(id).and_then(|agent| agent.branch) {
                    Some(branch) => format!("on branch {branch}"),
                    None => format!("in {place}"),
                };
                self.push_status(format!(
                    "{} {id} started {home} — Ctrl+] returns to the board",
                    kind.name()
                ));
            }
            FleetAction::Send { id, text } => {
                self.fleet.send(id, &text)?;
                self.push_status(format!("sent to agent {id}"));
            }
            FleetAction::Stop { id } => {
                let stopped = self.fleet.stop(id)?;
                self.push_status(stopped);
            }
            FleetAction::Merge { id } => {
                let merged = self.fleet.merge(id)?;
                self.push_status(merged);
            }
            FleetAction::Check { id } => {
                let checking = self.fleet.check(id)?;
                self.push_status(checking);
            }
        }
        Ok(())
    }

    /// An agent changed state; say so when it now needs the user.
    pub(crate) fn on_fleet(&mut self, message: &FleetMsg) {
        let agent = &message.agent;
        let name = agent.name();
        if let Some(check) = &message.checked {
            self.push_status(format!("{name} {} — /fleet", check.label()));
            return;
        }
        match agent.state {
            State::Blocked => {
                self.push_status(format!("{name} needs you: {} — /fleet", agent.last_line));
            }
            State::Idle if message.previous == State::Working && message.lasted >= NOTABLE_TURN => {
                self.push_status(format!("{name} finished its turn — /fleet"));
            }
            State::Exited(code) => self.push_status(format!("{name} exited ({code})")),
            State::Working | State::Idle => {}
        }
    }
}

/// `/fleet [<agent> [job] | send <n> <text> | check <n> | merge <n> | stop <n>]`.
pub(super) fn parse(arg: &str) -> Result<FleetAction, String> {
    let usage = || {
        format!(
            "usage: /fleet [{} [job] | send <n> <text> | check <n> | merge <n> | stop <n>]",
            Kind::NAMES.join("|")
        )
    };
    let arg = arg.trim();
    let (verb, rest) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
    let rest = rest.trim();
    match verb {
        "" | "list" if rest.is_empty() => Ok(FleetAction::Open),
        "send" | "tell" => {
            let (id, text) = rest.split_once(char::is_whitespace).ok_or_else(usage)?;
            let id = id.parse().map_err(|_| usage())?;
            let text = text.trim();
            if text.is_empty() {
                return Err(usage());
            }
            Ok(FleetAction::Send {
                id,
                text: text.to_string(),
            })
        }
        "merge" => rest
            .parse()
            .map(|id| FleetAction::Merge { id })
            .map_err(|_| usage()),
        "check" | "test" => rest
            .parse()
            .map(|id| FleetAction::Check { id })
            .map_err(|_| usage()),
        "stop" | "kill" => rest
            .parse()
            .map(|id| FleetAction::Stop { id })
            .map_err(|_| usage()),
        name => Kind::parse(name)
            .map(|kind| FleetAction::Start {
                kind,
                prompt: rest.to_string(),
                model: None,
                isolate: true,
            })
            .ok_or_else(usage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_command_parses_every_form() {
        assert_eq!(parse(""), Ok(FleetAction::Open));
        assert_eq!(
            parse("claude fix the  flaky test"),
            Ok(FleetAction::Start {
                kind: Kind::Claude,
                prompt: "fix the  flaky test".into(),
                model: None,
                isolate: true,
            })
        );
        assert_eq!(
            parse("opencode"),
            Ok(FleetAction::Start {
                kind: Kind::OpenCode,
                prompt: String::new(),
                model: None,
                isolate: true,
            })
        );
        assert_eq!(
            parse("send 2 also run the tests"),
            Ok(FleetAction::Send {
                id: 2,
                text: "also run the tests".into()
            })
        );
        assert_eq!(parse("stop 3"), Ok(FleetAction::Stop { id: 3 }));
        assert_eq!(parse("merge 3"), Ok(FleetAction::Merge { id: 3 }));
        assert_eq!(parse("check 3"), Ok(FleetAction::Check { id: 3 }));
        for bad in ["send 2", "send x hi", "stop", "vim fix it", "list now"] {
            assert!(
                parse(bad).unwrap_err().starts_with("usage: /fleet"),
                "{bad}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_blocked_agent_tells_the_user_and_a_follow_up_reaches_it() {
        let mut app = crate::app::tests::app_with_key();
        let id = app
            .fleet
            .spawn(
                "s1",
                Launch {
                    label: "sh".into(),
                    title: "scripted".into(),
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "echo 'Do you want to proceed? (y/n)'; read a; echo \"got $a\"; read b"
                            .into(),
                    ],
                    cwd: std::env::temp_dir(),
                    isolate: false,
                    auto: Vec::new(),
                },
            )
            .unwrap();
        let status = loop {
            match tokio::time::timeout(Duration::from_secs(10), app.next_event())
                .await
                .expect("a fleet notice")
            {
                crate::app::AppEvent::Status(status) if status.contains("needs you") => {
                    break status;
                }
                _ => {}
            }
        };
        assert_eq!(
            status,
            "sh 1 needs you: Do you want to proceed? (y/n) — /fleet"
        );
        app.execute(crate::app::AppCommand::Fleet {
            action: FleetAction::Send {
                id,
                text: "y".into(),
            },
        })
        .unwrap();
        for _ in 0..200 {
            if app.fleet.read("s1", id).unwrap().contains("got y") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(app.fleet.read("s1", id).unwrap().contains("got y"));
        app.execute(crate::app::AppCommand::Fleet {
            action: FleetAction::Stop { id },
        })
        .unwrap();
        assert!(app.fleet.list(None).is_empty());
    }
}
