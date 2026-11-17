//! Shared command catalog, parsing, and domain dispatch for frontends.

use anyhow::Result;

use super::{App, FilesTab, WorkMode};
use crate::memory::ShareTarget;

pub struct Command {
    pub name: &'static str,
    pub desc: &'static str,
    pub aliases: &'static [&'static str],
}

pub enum Match {
    Builtin(&'static Command),
}

impl Match {
    pub const fn name(&self) -> &str {
        match self {
            Self::Builtin(c) => c.name,
        }
    }

    pub const fn desc(&self) -> &str {
        match self {
            Self::Builtin(c) => c.desc,
        }
    }
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        desc: "keys & commands",
        aliases: &["keys", "shortcuts", "?"],
    },
    Command {
        name: "new",
        desc: "start new chat",
        aliases: &["clear"],
    },
    Command {
        name: "refresh",
        desc: "refresh this chat's frozen prompt (one cache miss)",
        aliases: &[],
    },
    Command {
        name: "compact",
        desc: "summarize old messages",
        aliases: &["compaction", "summarize"],
    },
    Command {
        name: "session",
        desc: "switch sessions",
        aliases: &["sessions", "history", "resume", "continue", "switch"],
    },
    Command {
        name: "space",
        desc: "switch spaces",
        aliases: &["spaces", "project", "workspace"],
    },
    Command {
        name: "model",
        desc: "pick a model",
        aliases: &["models", "llm"],
    },
    Command {
        name: "login",
        desc: "pick a backend to log into",
        aliases: &[
            "key",
            "apikey",
            "token",
            "auth",
            "codex",
            "subscription",
            "oauth",
            "chatgpt",
            "opencode",
            "mistral",
            "vibe",
        ],
    },
    Command {
        name: "local",
        desc: "local inference runtime",
        aliases: &[
            "ollama",
            "mlx",
            "mlx-serve",
            "lmstudio",
            "edge0",
            "llama-cpp",
            "llamacpp",
            "offline",
            "runtime",
            "serve",
        ],
    },
    Command {
        name: "swarm",
        desc: "independent drafts, checked, then one reply",
        aliases: &["swarms", "personas", "panel"],
    },
    Command {
        name: "config",
        desc: "settings & stats",
        aliases: &["settings", "stats", "nerd", "params"],
    },
    Command {
        name: "style",
        desc: "reply layouts: /style interactive|auto|rich|plain",
        aliases: &[],
    },
    Command {
        name: "theme",
        desc: "set UI background",
        aliases: &["appearance", "colors"],
    },
    Command {
        name: "vim",
        desc: "vim keys in text inputs: /vim on|off",
        aliases: &["keymap"],
    },
    Command {
        name: "motion",
        desc: "animation in panels: /motion on|off",
        aliases: &["animations"],
    },
    Command {
        name: "skills",
        desc: "manage skills",
        aliases: &["addskill"],
    },
    Command {
        name: "files",
        desc: "browse space files / images / scripts",
        aliases: &[
            "file", "attach", "upload", "docs", "image", "images", "img", "pictures", "script",
            "scripts",
        ],
    },
    Command {
        name: "view",
        desc: "view an image in the terminal: /view <path|url>",
        aliases: &["viewer"],
    },
    Command {
        name: "connectors",
        desc: "connect outside services (MCP): add, tools, policy — /connectors add <name> <url|command>",
        aliases: &["connector", "mcp", "integrations", "services"],
    },
    Command {
        name: "apps",
        desc: "view space apps",
        aliases: &["app", "webapps"],
    },
    Command {
        name: "studio",
        desc: "open the slide canvas for this conversation",
        aliases: &["deck", "slides"],
    },
    Command {
        name: "queue",
        desc: "messages waiting for the reply: send, edit [n], drop <n>, clear",
        aliases: &["queued"],
    },
    Command {
        name: "agents",
        desc: "this conversation's sub-agents: live transcripts, steps, tokens and cost",
        aliases: &["subagents", "sub-agents"],
    },
    Command {
        name: "fleet",
        desc: "run Claude Code, Codex, pi or OpenCode inside Dugix: start, watch, steer and stop them from one board",
        aliases: &["herd"],
    },
    Command {
        name: "library",
        desc: "your decks and docs: open in studio, export, delete",
        aliases: &["decks", "lib"],
    },
    Command {
        name: "pin-panel",
        desc: "pin the focused panel as a mini-app: [name] | unpin <name> | rename <name> <new>",
        aliases: &["pin", "unpin", "unpin-panel", "rename-panel"],
    },
    Command {
        name: "research",
        desc: "create a research job (blank = scope topic from this chat)",
        aliases: &["deep-research"],
    },
    Command {
        name: "export",
        desc: "write session's report + sources to a file",
        aliases: &["save-report"],
    },
    Command {
        name: "memory",
        desc: "what dugix remembers — /memory [edit|tidy|share [<chapter> with all|none|<space>[, <space>…]]]",
        aliases: &["remember"],
    },
    Command {
        name: "watch",
        desc: "daily research job (blank = /jobs)",
        aliases: &["watches"],
    },
    Command {
        name: "jobs",
        desc: "agent and research jobs — /job new to create",
        aliases: &[],
    },
    Command {
        name: "job",
        desc: "create a scheduled agent job: /job new",
        aliases: &[],
    },
    Command {
        name: "usage",
        desc: "token/cache/cost analytics by backend and model",
        aliases: &["analytics", "costs", "billing"],
    },
    Command {
        name: "harness",
        desc: "where the tools fail — last 7 days",
        aliases: &[],
    },
    Command {
        name: "procs",
        desc: "background processes started by the agent",
        aliases: &[],
    },
    Command {
        name: "agent",
        desc: "work in a directory: read, edit, run — /agent [path]",
        aliases: &[],
    },
    Command {
        name: "improve",
        desc: "improve dugix in a review worktree — /improve <what>",
        aliases: &[],
    },
    Command {
        name: "chat",
        desc: "back to plain chat (no workspace)",
        aliases: &[],
    },
    Command {
        name: "auto",
        desc: "toggle agent / agent·auto approval policy",
        aliases: &[],
    },
    Command {
        name: "incognito",
        desc: "toggle incognito (no persistence, no apps)",
        aliases: &["private", "anon"],
    },
    Command {
        name: "training",
        desc: "record training data for this space (local, scrubbed)",
        aliases: &[],
    },
    Command {
        name: "retry",
        desc: "regenerate the last reply",
        aliases: &["regen"],
    },
    Command {
        name: "undo",
        desc: "restore the files the agent changed in its last turn",
        aliases: &[],
    },
    Command {
        name: "copy",
        desc: "copy last reply",
        aliases: &["yank", "clip"],
    },
    Command {
        name: "quit",
        desc: "exit the app",
        aliases: &["q", "exit"],
    },
];

/// Score case-insensitive subsequence matches, preferring contiguous and leading matches.
pub fn fuzzy_score(hay: &str, needle: &str) -> Option<i32> {
    let hay = hay.to_lowercase();
    let needle = needle.to_lowercase();
    let mut chars = hay.chars();
    let mut score = 0i32;
    let mut prev_matched = false;
    let mut pos = 0i32;
    for nc in needle.chars() {
        loop {
            let hc = chars.next()?;
            if hc == nc {
                score += 1;
                if prev_matched {
                    score += 2;
                }
                if pos == 0 {
                    score += 3;
                }
                prev_matched = true;
                pos += 1;
                break;
            }
            prev_matched = false;
            pos += 1;
        }
    }
    Some(score)
}

/// Score command matches with names weighted above aliases and descriptions.
pub fn command_score(c: &Command, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let mut best: Option<i32> = None;
    let mut upd = |s: &str, bonus: i32| {
        if let Some(sc) = fuzzy_score(s, needle) {
            let v = sc + bonus;
            best = Some(best.map_or(v, |b| b.max(v)));
        }
    };
    upd(c.name, 100);
    for a in c.aliases {
        upd(a, 50);
    }
    upd(c.desc, 0);
    best
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MemoryAction {
    Edit,
    Tidy,
    Share {
        chapter: String,
        with: Option<ShareTarget>,
    },
}

fn parse_memory(args: &str) -> Result<AppCommand, String> {
    let action = match args {
        "" | "edit" => MemoryAction::Edit,
        "tidy" => MemoryAction::Tidy,
        "share" => MemoryAction::Share {
            chapter: String::new(),
            with: None,
        },
        _ => {
            let usage = "usage: /memory share <chapter> with all|none|<space>[, <space>…]";
            let rest = args
                .strip_prefix("share ")
                .ok_or_else(|| "usage: /memory [edit|tidy]".to_string())?;
            let (chapter, target) = rest.rsplit_once(" with ").ok_or(usage)?;
            if chapter.trim().is_empty() || target.trim().is_empty() {
                return Err(usage.to_string());
            }
            let with = match target.trim() {
                "all" => ShareTarget::All,
                "none" => ShareTarget::None,
                names => {
                    let names: Vec<String> = names
                        .split(',')
                        .map(|name| name.trim().to_string())
                        .collect();
                    if names.iter().any(String::is_empty) {
                        return Err(usage.to_string());
                    }
                    ShareTarget::Spaces(names)
                }
            };
            MemoryAction::Share {
                chapter: chapter.trim().to_string(),
                with: Some(with),
            }
        }
    };
    Ok(AppCommand::Memory { action })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum AppCommand {
    Quit,
    Send {
        text: String,
    },
    /// Cancel the active response when no task id is supplied.
    Cancel {
        task: Option<u64>,
    },
    Approve {
        decision: crate::tools::Decision,
    },
    Steer {
        text: String,
    },
    /// Act on the messages waiting for the running reply.
    Queue {
        action: super::QueueAction,
    },
    /// Open the fleet board, or start, steer or stop an external agent.
    Fleet {
        action: super::FleetAction,
    },
    AnswerGate {
        text: String,
    },
    NewSession,
    RefreshPrompt,
    Compact,
    OpenSessionPicker,
    OpenSpacePicker,
    OpenModelPicker,
    OpenLogin,
    /// An empty spec opens the runtime picker or reports its headless status.
    ConfigureLocal {
        spec: String,
    },
    /// An empty runtime targets the selected local provider.
    LocalServer {
        verb: String,
        runtime: String,
    },
    OpenSwarm,
    OpenSettings,
    SetTheme {
        mode: String,
    },
    SetKeymap {
        mode: String,
    },
    SetMotion {
        mode: String,
    },
    Retry,
    Undo,
    RunUserCommand {
        cmd: String,
    },
    /// Record a terminal command without capturing its output.
    RecordUserCommand {
        cmd: String,
        exit: Option<i32>,
        started: bool,
    },
    OpenCopyMenu,
    OpenSkills,
    OpenFiles {
        tab: FilesTab,
    },
    OpenApps,
    OpenConnectors,
    /// Bearer tokens must be stored separately in the keychain, never in this command.
    AddConnector {
        name: String,
        url: Option<String>,
        command: Vec<String>,
        bearer: bool,
    },
    RemoveConnector {
        name: String,
    },
    SetConnectorEnabled {
        name: String,
        on: bool,
    },
    SetConnectorPolicy {
        name: String,
        tool: String,
        policy: String,
    },
    /// Import all new connectors when `names` is empty.
    ImportConnectors {
        source: String,
        names: Vec<String>,
    },
    PinPanel {
        name: Option<String>,
    },
    UnpinPanel {
        name: String,
    },
    RenamePanel {
        from: String,
        to: String,
    },
    OpenStudio {
        deck: Option<String>,
    },
    OpenLibrary,
    OpenSubagents,
    OpenImage {
        target: String,
    },
    /// An empty topic is distilled from recent chat; ungated runs skip approval.
    RunResearch {
        topic: String,
        gated: bool,
    },
    Export,
    Memory {
        action: MemoryAction,
    },
    ToggleAuto,
    SetWorkMode {
        mode: WorkMode,
        root: Option<String>,
    },
    Improve {
        what: String,
    },
    Incognito {
        on: bool,
    },
    SetTraining {
        on: Option<bool>,
    },
    Watch {
        topic: Option<String>,
    },
    OpenJobs,
    OpenJobForm,
    CreateJob {
        spec: crate::jobs::JobSpec,
    },
    SetJobEnabled {
        id: String,
        on: bool,
    },
    ToggleResearchRepeat {
        id: String,
    },
    RunJobNow {
        id: String,
    },
    /// Soft-delete the job while retaining review branches and transcripts.
    DeleteJob {
        id: String,
    },
    OpenUsage,
    Harness,
    OpenProcs,
    OpenHelp,
    ArmSkill {
        name: String,
        rest: Option<String>,
    },
    SwitchSpace {
        name: String,
    },
    ResolveSession {
        id: String,
    },
    SetModel {
        id: String,
    },
    SetSetting {
        key: String,
        value: String,
    },
}

impl App {
    fn owned_job(&self, id: &str) -> Result<crate::jobs::Job> {
        let job = self
            .db
            .list_jobs()?
            .into_iter()
            .find(|job| job.id == id)
            .ok_or_else(|| anyhow::anyhow!("job not found"))?;
        anyhow::ensure!(
            job.device_id == self.db.device_id()?,
            "job belongs to another device; read-only here"
        );
        Ok(job)
    }

    fn parse_style(value: &str) -> std::result::Result<AppCommand, String> {
        if super::REPLY_STYLES.contains(&value) {
            Ok(AppCommand::SetSetting {
                key: "reply_style".into(),
                value: value.into(),
            })
        } else {
            Err("usage: /style interactive|auto|rich|plain".into())
        }
    }

    /// Parse a command line without its leading slash, resolving aliases and skills.
    #[allow(clippy::too_many_lines)]
    pub fn parse_command(&self, cmd: &str) -> std::result::Result<AppCommand, String> {
        if let Some(rest) = cmd.strip_prefix("research!") {
            return Ok(AppCommand::RunResearch {
                topic: rest.trim().to_string(),
                gated: false,
            });
        }
        let token = cmd.split_whitespace().next().unwrap_or("");
        let canonical = COMMANDS
            .iter()
            .find(|c| c.name == token || c.aliases.contains(&token))
            .map_or(token, |c| c.name);
        let rest = |cmd: &str, token: &str| cmd[token.len()..].trim().to_string();
        match canonical {
            "quit" => Ok(AppCommand::Quit),
            "new" => Ok(AppCommand::NewSession),
            "compact" => Ok(AppCommand::Compact),
            "refresh" => Ok(AppCommand::RefreshPrompt),
            "session" => Ok(AppCommand::OpenSessionPicker),
            "space" => Ok(AppCommand::OpenSpacePicker),
            "model" => Ok(AppCommand::OpenModelPicker),
            "login" => Ok(AppCommand::OpenLogin),
            "local" => Ok(local_command(&match (token, rest(cmd, token)) {
                ("local", arg) => arg,
                (runtime, arg) if arg.is_empty() => runtime.to_string(),
                (runtime, arg) => format!("{runtime} {arg}"),
            })),
            "swarm" => Ok(AppCommand::OpenSwarm),
            "config" => Ok(AppCommand::OpenSettings),
            "style" => Self::parse_style(&rest(cmd, token)),
            "theme" => Ok(AppCommand::SetTheme {
                mode: rest(cmd, token),
            }),
            "vim" => Ok(AppCommand::SetKeymap {
                mode: rest(cmd, token),
            }),
            "motion" => Ok(AppCommand::SetMotion {
                mode: rest(cmd, token),
            }),
            "retry" => Ok(AppCommand::Retry),
            "undo" => Ok(AppCommand::Undo),
            "copy" => Ok(AppCommand::OpenCopyMenu),
            "skills" => Ok(AppCommand::OpenSkills),
            "files" => Ok(AppCommand::OpenFiles {
                tab: match token {
                    t if t == "image" || t == "images" || t == "img" || t == "pictures" => {
                        FilesTab::Images
                    }
                    t if t == "script" || t == "scripts" => FilesTab::Scripts,
                    _ => FilesTab::Files,
                },
            }),
            "apps" => Ok(AppCommand::OpenApps),
            "view" => match rest(cmd, token).as_str() {
                "" => Err("usage: /view <path|url>".into()),
                target => Ok(AppCommand::OpenImage {
                    target: target.to_string(),
                }),
            },
            "studio" => Ok(AppCommand::OpenStudio {
                deck: Some(rest(cmd, token)).filter(|deck| !deck.is_empty()),
            }),
            "library" => Ok(AppCommand::OpenLibrary),
            "agents" => Ok(AppCommand::OpenSubagents),
            "queue" => {
                super::queue::parse(&rest(cmd, token)).map(|action| AppCommand::Queue { action })
            }
            "fleet" => {
                super::fleet::parse(&rest(cmd, token)).map(|action| AppCommand::Fleet { action })
            }
            "connectors" => parse_connectors(&rest(cmd, token)),
            "pin-panel" => parse_pin_panel(token, &rest(cmd, token)),
            "research" => Ok(AppCommand::RunResearch {
                topic: rest(cmd, token),
                gated: true,
            }),
            "export" => Ok(AppCommand::Export),
            "memory" => parse_memory(&rest(cmd, token)),
            "agent" => {
                let root = rest(cmd, token);
                Ok(AppCommand::SetWorkMode {
                    mode: WorkMode::Agent,
                    root: (!root.is_empty()).then_some(root),
                })
            }
            "improve" => improve_command(rest(cmd, token)),
            "auto" => Ok(AppCommand::ToggleAuto),
            "chat" => Ok(AppCommand::SetWorkMode {
                mode: WorkMode::Chat,
                root: None,
            }),
            "incognito" => Ok(AppCommand::Incognito {
                on: !self.incognito,
            }),
            "training" => Ok(AppCommand::SetTraining {
                on: match rest(cmd, token).as_str() {
                    "" => None,
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => return Err("usage: /training [on|off]".to_string()),
                },
            }),
            "watch" => {
                let arg = rest(cmd, token);
                Ok(AppCommand::Watch {
                    topic: (!arg.is_empty()).then_some(arg),
                })
            }
            "jobs" => Ok(AppCommand::OpenJobs),
            "job" if rest(cmd, token) == "new" => Ok(AppCommand::OpenJobForm),
            "job" => Err("usage: /job new".into()),
            "usage" => Ok(AppCommand::OpenUsage),
            "harness" => Ok(AppCommand::Harness),
            "procs" => Ok(AppCommand::OpenProcs),
            "help" => Ok(AppCommand::OpenHelp),
            other => {
                if self.skills.iter().any(|s| s.name == other) {
                    let text = rest(cmd, token);
                    Ok(AppCommand::ArmSkill {
                        name: other.to_string(),
                        rest: (!text.is_empty()).then_some(text),
                    })
                } else {
                    Err(format!("unknown command: /{other}"))
                }
            }
        }
    }

    /// Execute domain commands, leaving frontend-only actions to the caller.
    #[allow(clippy::too_many_lines)]
    pub fn execute(&mut self, cmd: AppCommand) -> Result<()> {
        match cmd {
            AppCommand::Quit
            | AppCommand::OpenSessionPicker
            | AppCommand::OpenSpacePicker
            | AppCommand::OpenModelPicker
            | AppCommand::OpenLogin
            | AppCommand::OpenSwarm
            | AppCommand::OpenSettings
            | AppCommand::SetTheme { .. }
            | AppCommand::SetKeymap { .. }
            | AppCommand::SetMotion { .. }
            | AppCommand::OpenCopyMenu
            | AppCommand::OpenSkills
            | AppCommand::OpenFiles { .. }
            | AppCommand::OpenApps
            | AppCommand::OpenConnectors
            | AppCommand::PinPanel { .. }
            | AppCommand::OpenStudio { .. }
            | AppCommand::OpenLibrary
            | AppCommand::OpenSubagents
            | AppCommand::OpenImage { .. }
            | AppCommand::OpenUsage
            | AppCommand::OpenJobForm
            | AppCommand::OpenProcs
            | AppCommand::OpenHelp
            | AppCommand::Watch { topic: None } => {}
            AppCommand::Watch { topic: Some(topic) } => self.repeat_research(&topic),
            AppCommand::UnpinPanel { name } => self.unpin_panel(&name)?,
            AppCommand::RenamePanel { from, to } => self.rename_panel(&from, &to)?,
            AppCommand::AddConnector {
                name,
                url,
                command,
                bearer,
            } => self.add_connector(&name, url, command, bearer)?,
            AppCommand::RemoveConnector { name } => self.remove_connector(&name)?,
            AppCommand::SetConnectorEnabled { name, on } => {
                self.set_connector_enabled(&name, on)?;
            }
            AppCommand::SetConnectorPolicy { name, tool, policy } => {
                self.set_connector_policy(&name, &tool, &policy)?;
            }
            AppCommand::ImportConnectors { source, names } => {
                self.import_connectors(&source, &names)?;
            }
            AppCommand::OpenJobs => self.collect_job_notifications()?,
            AppCommand::CreateJob { mut spec } => {
                spec.validate()?;
                if spec.kind == "agent" {
                    spec.root = self.validate_job_root(&spec.root)?;
                }
                let job = crate::jobs::Job {
                    id: uuid::Uuid::new_v4().to_string(),
                    space_id: self.active_space.id.clone(),
                    session_id: self
                        .session
                        .as_ref()
                        .map(|session| session.id.clone())
                        .unwrap_or_default(),
                    device_id: self.db.device_id()?,
                    spec,
                    enabled: 1,
                    last_run_at: None,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                };
                self.db.save_job(&job)?;
                self.push_status("job created · run `dugix jobs daemon` to schedule it");
            }
            AppCommand::SetJobEnabled { id, on } => {
                let mut job = self.owned_job(&id)?;
                job.enabled = i64::from(on);
                job.updated_at = chrono::Utc::now().to_rfc3339();
                self.db.save_job(&job)?;
            }
            AppCommand::ToggleResearchRepeat { id } => {
                let job = self.owned_job(&id)?;
                if job.spec.kind == "research" {
                    let trigger = if job.spec.trigger_kind == "interval" {
                        "manual"
                    } else {
                        "interval"
                    };
                    self.db.conn().execute(
                        "UPDATE jobs SET trigger_kind = ?2, interval_minutes = 1440,
                         updated_at = ?3 WHERE id = ?1",
                        (&id, trigger, chrono::Utc::now().to_rfc3339()),
                    )?;
                }
            }
            AppCommand::DeleteJob { id } => {
                let mut job = self.owned_job(&id)?;
                job.enabled = -1;
                job.updated_at = chrono::Utc::now().to_rfc3339();
                self.db.save_job(&job)?;
            }
            AppCommand::RunJobNow { id } => {
                self.owned_job(&id)?;
                let mut command = std::process::Command::new(std::env::current_exe()?);
                command
                    .args(["jobs", "run", &id])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt as _;
                    command.process_group(0);
                }
                command.spawn()?;
                self.push_status("job launched · /jobs to review the result");
            }
            AppCommand::Harness => {
                let since = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
                self.harness = Some(self.db.harness_summary(&since)?);
            }
            AppCommand::ConfigureLocal { spec } => self.configure_local(&spec),
            AppCommand::LocalServer { verb, runtime } => self.manage_local_server(&verb, &runtime),
            AppCommand::Send { text } => self.send_message(text)?,
            AppCommand::Retry => self.retry_last_reply()?,
            AppCommand::Undo => self.undo_last_turn()?,
            AppCommand::RunUserCommand { cmd } => self.run_user_command(cmd)?,
            AppCommand::RecordUserCommand { cmd, exit, started } => {
                self.record_user_command(&cmd, exit, started)?;
            }
            AppCommand::Cancel { task } => match task {
                Some(id) => self.cancel_chat_task(id)?,
                None => self.stop_stream()?,
            },
            AppCommand::Approve { decision } => self.answer_approval(decision),
            AppCommand::Steer { text } => self.steer_research(&text),
            AppCommand::Queue { action } => self.run_queue_action(action)?,
            AppCommand::Fleet { action } => self.run_fleet_action(action)?,
            AppCommand::AnswerGate { text } => self.reply_to_survey_gate(&text),
            AppCommand::NewSession => self.new_session(),
            AppCommand::Compact => self.force_compact(),
            AppCommand::RefreshPrompt => self.refresh_prompt(),
            AppCommand::RunResearch { topic, gated } => {
                if !gated {
                    self.start_research_with_gate(&topic, false);
                } else if topic.is_empty() {
                    self.start_research_from_chat();
                } else {
                    self.start_research(&topic);
                }
            }
            AppCommand::Memory { action } => match action {
                MemoryAction::Edit => self.push_status(
                    "edit memory in the terminal UI with /memory, or run dugix memory --edit",
                ),
                MemoryAction::Tidy => self.memory_tidy()?,
                MemoryAction::Share { chapter, with } => {
                    self.memory_share(&chapter, with.as_ref())?;
                }
            },
            AppCommand::Export => {
                self.export_report()?;
            }
            AppCommand::ToggleAuto => {
                if self.work_mode.is_agent() {
                    let mode = if self.work_mode == WorkMode::AgentAuto {
                        WorkMode::Agent
                    } else {
                        WorkMode::AgentAuto
                    };
                    let root = self
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.root().to_string_lossy().into_owned());
                    self.set_work_mode(mode, root.as_deref())?;
                } else {
                    self.push_status("/auto needs agent mode · run /agent first");
                }
            }
            AppCommand::SetWorkMode { mode, root } => {
                self.set_work_mode(mode, root.as_deref())?;
            }
            AppCommand::Improve { what } => self.improve_dugix(&what)?,
            AppCommand::Incognito { on } => {
                if on != self.incognito {
                    self.toggle_incognito()?;
                }
            }
            AppCommand::SetTraining { on } => {
                if self.incognito {
                    self.push_status("training is not available in incognito mode");
                } else {
                    let on = on.unwrap_or_else(|| !self.training_enabled());
                    self.db.set_setting(
                        &format!("training:{}", self.active_space.id),
                        if on { "on" } else { "off" },
                    )?;
                    if on {
                        self.push_status(format!(
                            "training on — local data in {}; secrets are scrubbed",
                            self.space.traces_dir().display()
                        ));
                    } else {
                        self.push_status("training off for this space");
                    }
                }
            }
            AppCommand::ArmSkill { name, rest } => {
                self.forced_skill = Some(name.clone());
                if let Some(text) = rest {
                    self.send_message(text)?;
                } else {
                    self.push_status(format!("skill {name} armed for next message"));
                }
            }
            AppCommand::SwitchSpace { name } => {
                self.switch_space_cli(&name)?;
            }
            AppCommand::ResolveSession { id } => {
                self.switch_to_session_by_id(&id)?;
            }
            AppCommand::SetModel { id } => {
                self.pick_model(&id)?;
            }
            AppCommand::SetSetting { key, value } => {
                self.set_setting(&key, &value)?;
            }
        }
        Ok(())
    }

    pub fn run_command(&mut self, cmd: &str) -> Result<()> {
        match self.parse_command(cmd) {
            Ok(cmd) => self.execute(cmd),
            Err(message) => {
                self.push_status(message);
                Ok(())
            }
        }
    }
}

fn parse_pin_panel(token: &str, rest: &str) -> std::result::Result<AppCommand, String> {
    let mut words = rest.split_whitespace().collect::<Vec<_>>();
    let verb = match token {
        "unpin-panel" | "unpin" => Some("unpin"),
        "rename-panel" => Some("rename"),
        _ => words
            .first()
            .copied()
            .filter(|word| matches!(*word, "unpin" | "rename"))
            .inspect(|_| {
                words.remove(0);
            }),
    };
    match (verb, words.as_slice()) {
        (Some("unpin"), [name]) => Ok(AppCommand::UnpinPanel {
            name: (*name).into(),
        }),
        (Some("rename"), [from, to]) => Ok(AppCommand::RenamePanel {
            from: (*from).into(),
            to: (*to).into(),
        }),
        (Some("unpin"), _) => Err("usage: /pin-panel unpin <name>".into()),
        (Some(_), _) => Err("usage: /pin-panel rename <name> <new name>".into()),
        (None, []) => Ok(AppCommand::PinPanel { name: None }),
        (None, [name]) => Ok(AppCommand::PinPanel {
            name: Some((*name).into()),
        }),
        (None, _) => Err("usage: /pin-panel [name]".into()),
    }
}

fn parse_connectors(rest: &str) -> std::result::Result<AppCommand, String> {
    const USAGE: &str = "usage: /connectors [add <name> <url|command…> | remove <name> | enable|disable <name> | allow|ask|deny|default <name> <tool> | import [source]]";
    let mut words = rest.split_whitespace();
    let Some(verb) = words.next() else {
        return Ok(AppCommand::OpenConnectors);
    };
    let tail = |count: usize| {
        let mut skipped = rest.trim_start();
        for _ in 0..count {
            skipped = skipped
                .trim_start_matches(|c: char| !c.is_whitespace())
                .trim_start();
        }
        skipped.to_string()
    };
    let name = words.next().map(str::to_string);
    match (verb, name) {
        ("add", Some(name)) => {
            let target = tail(2);
            if target.is_empty() {
                return Err(USAGE.into());
            }
            if target.starts_with("http://") || target.starts_with("https://") {
                Ok(AppCommand::AddConnector {
                    name,
                    url: Some(
                        target
                            .split_whitespace()
                            .next()
                            .unwrap_or_default()
                            .to_string(),
                    ),
                    command: Vec::new(),
                    bearer: target.split_whitespace().any(|word| word == "--bearer"),
                })
            } else {
                Ok(AppCommand::AddConnector {
                    name,
                    url: None,
                    command: crate::connectors::split_argv(&target)?,
                    bearer: false,
                })
            }
        }
        ("remove" | "rm" | "delete", Some(name)) => Ok(AppCommand::RemoveConnector { name }),
        ("enable" | "disable", Some(name)) => Ok(AppCommand::SetConnectorEnabled {
            name,
            on: verb == "enable",
        }),
        ("allow" | "ask" | "deny" | "default", Some(name)) => match words.next() {
            Some(tool) => Ok(AppCommand::SetConnectorPolicy {
                name,
                tool: tool.to_string(),
                policy: verb.to_string(),
            }),
            None => Err(USAGE.into()),
        },
        ("import", source) => Ok(AppCommand::ImportConnectors {
            source: source.unwrap_or_default(),
            names: Vec::new(),
        }),
        _ => Err(USAGE.into()),
    }
}

fn improve_command(what: String) -> std::result::Result<AppCommand, String> {
    if what.is_empty() {
        Err("usage: /improve <what>".into())
    } else {
        Ok(AppCommand::Improve { what })
    }
}

fn local_command(spec: &str) -> AppCommand {
    let words: Vec<&str> = spec.split_whitespace().collect();
    let verb = words.iter().position(|word| {
        matches!(
            word.to_ascii_lowercase().as_str(),
            "start" | "serve" | "stop" | "restart" | "status"
        )
    });
    match verb {
        Some(index) => AppCommand::LocalServer {
            verb: match words[index].to_ascii_lowercase().as_str() {
                "serve" => "start".to_string(),
                other => other.to_string(),
            },
            runtime: words
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index)
                .map(|(_, word)| *word)
                .collect::<Vec<_>>()
                .join(" "),
        },
        None => AppCommand::ConfigureLocal {
            spec: spec.to_string(),
        },
    }
}

#[cfg(test)]
mod harness_tests {
    use super::*;

    #[test]
    fn harness_command_parses_and_loads_seven_days_including_incognito() {
        let mut app = crate::app::tests::app_with_key();
        let now = chrono::Utc::now();
        for days in [0, 6, 8] {
            app.db
                .conn_for_test()
                .execute(
                    "INSERT INTO cache.tool_events
                     (at, session_id, tool, action, ok, error_kind, latency_ms, result_chars)
                     VALUES (?1, 'session', 'scripts', 'edit', 0, 'stale_edit', 1, 1)",
                    [(now - chrono::Duration::days(days)).to_rfc3339()],
                )
                .unwrap();
        }
        assert_eq!(app.parse_command("harness"), Ok(AppCommand::Harness));
        for incognito in [false, true] {
            app.incognito = incognito;
            app.run_command("harness").unwrap();
            let summary = app.harness.as_ref().unwrap();
            assert_eq!((summary.calls, summary.failures), (2, 2));
            assert_eq!(summary.rows.len(), 1);
            assert_eq!(summary.rows[0].count, 2);
        }
        let root = app.space.root.clone();
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
