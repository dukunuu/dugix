//! Headless commands and launch flags for the terminal frontend.

#![allow(clippy::cast_precision_loss)]

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Parser, Subcommand};

use nexus_core::db;
use nexus_core::space;

mod ask;
mod canvas;
mod data;
mod eval;
mod host;
mod providers;
mod research;
mod selection;
mod sync;
mod system;
mod usage;

pub(crate) use selection::{
    clear_primary_selection, primary_selection_prompt, send_selection_request,
    start_selection_server,
};

use self::eval::{eval, parse_eval_setting};
use ask::{ask, ask_options};
use canvas::{deck_export, design_files, doc_files, launch_studio};
use data::{backup, export, files, instructions, memory, restore, sessions, spaces};
use host::{HostServiceAction, host};
use providers::{login, models, skills};
use research::{chat, jobs, research};
use selection::clipboard;
use sync::sync_cmd;
use system::{doctor, open, status, train_export, update};
use usage::usage;

// Test modules reach moved helpers through `use super::*`.
#[cfg(test)]
use self::eval::{
    EvalRun, EvalTaskResult, apply_eval_settings, eval_deltas, eval_totals, experiments_header,
    format_eval_compare, format_eval_compare_labeled,
};
#[cfg(test)]
use data::{prune_targets, transcript_markdown, unzip_into, zip_dir};
#[cfg(test)]
use nexus_core::config;
#[cfg(test)]
use selection::{DEFAULT_CLIPBOARD_INSTRUCTION, build_clipboard_prompt, build_selection_prompt};
#[cfg(test)]
use usage::session_usage_output;

/// Local-first terminal chat for deep research and multi-agent work.
#[derive(Parser)]
#[command(name = "nexus", version, about, max_term_width = 100)]
pub struct Cli {
    /// Open the most recently updated session instead of starting a blank chat.
    #[arg(long = "continue")]
    pub continue_session: bool,
    /// Start the TUI and analyze Wayland's primary selection when it has text;
    /// otherwise behave like `--continue`.
    #[arg(long = "selection-chat")]
    pub selection_chat: bool,
    /// Send a selection request to an already-running TUI and exit.
    #[arg(long = "selection-chat-send-only", hide = true)]
    pub selection_chat_send_only: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// User-only memory sharing operations.
#[derive(Subcommand)]
pub enum MemoryCommand {
    /// Share one of the home space's chapters.
    Share {
        chapter: String,
        /// Space names that can read and write this chapter's facts.
        #[arg(long, num_args = 1.., required_unless_present_any = ["all", "none"], conflicts_with_all = ["all", "none"])]
        with: Vec<String>,
        /// Share with every space, including future spaces.
        #[arg(long, conflicts_with = "none")]
        all: bool,
        /// Make the chapter private again.
        #[arg(long)]
        none: bool,
    },
    /// List chapters shared out of or into this space.
    Shares,
}

#[derive(Subcommand)]
pub enum Command {
    /// One-shot question from the shell — the answer streams to stdout and
    /// is saved as a normal session you can reopen in the TUI. With no
    /// prompt argument, the question is read from stdin (pipe-friendly).
    Ask {
        /// The question to ask (omit to read stdin).
        prompt: Option<String>,
        /// Model id as shown in the TUI's /model picker (defaults to your
        /// most recently used model).
        #[arg(long)]
        model: Option<String>,
        /// Run in this space instead of your default space; `new:NAME`
        /// creates the space on the fly.
        #[arg(long)]
        space: Option<String>,
        /// Emit the answer as JSON (answer, session, usage) instead of
        /// streaming it.
        #[arg(long)]
        json: bool,
        /// Suppress stderr chatter (tool status, token summary).
        #[arg(long)]
        quiet: bool,
    },
    /// Run golden tasks against a real backend in incognito (costs real tokens).
    Eval {
        /// Directory containing one golden task per TOML file.
        #[arg(default_value = "./evals")]
        dir: PathBuf,
        /// Model id (defaults to your most recently used model).
        #[arg(long)]
        model: Option<String>,
        /// Only tasks whose names contain this substring.
        #[arg(long)]
        filter: Option<String>,
        /// Override experiments.FIELD=VALUE (repeatable).
        #[arg(long, value_name = "KEY=VALUE", value_parser = parse_eval_setting)]
        set: Vec<EvalSetting>,
        /// Run baseline first, then the overrides, and compare results.
        #[arg(long, requires = "set")]
        compare: bool,
        /// Run the same tasks and experiment settings on two models.
        #[arg(
            long,
            num_args = 2,
            value_names = ["A", "B"],
            action = clap::ArgAction::Set,
            conflicts_with = "compare"
        )]
        compare_model: Vec<String>,
        /// Emit task results and totals as a JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Export recorded traces as training data (offline; never runs training).
    Train {
        #[command(subcommand)]
        cmd: TrainCmd,
    },
    /// Analyze text from the OS clipboard with one headless turn. The
    /// optional instruction is applied to the copied text; when omitted,
    /// Nexus explains it and points out interesting facts and context.
    #[command(name = "clip", visible_alias = "clipboard")]
    Clip {
        /// Custom instruction for the copied text.
        instruction: Option<String>,
        /// Named form of the custom instruction, useful in shortcut configs.
        #[arg(
            long = "instruction",
            value_name = "TEXT",
            conflicts_with = "instruction"
        )]
        instruction_option: Option<String>,
        /// Model id as shown in the TUI's /model picker.
        #[arg(long)]
        model: Option<String>,
        /// Run in this space instead of your default space; `new:NAME`
        /// creates the space on the fly.
        #[arg(long)]
        space: Option<String>,
        /// Copy the final answer back to the OS clipboard.
        #[arg(long)]
        copy: bool,
        /// Emit the answer as JSON (answer, session, usage) instead of
        /// streaming it.
        #[arg(long)]
        json: bool,
        /// Suppress stderr chatter (tool status, token summary).
        #[arg(long)]
        quiet: bool,
    },
    /// A bare REPL: one turn per line, all turns in one session.
    Chat {
        /// Model id as shown in the TUI's /model picker.
        #[arg(long)]
        model: Option<String>,
        /// Run in this space instead of your default space.
        #[arg(long)]
        space: Option<String>,
        /// Suppress stderr chatter.
        #[arg(long)]
        quiet: bool,
    },
    /// Deep research from the shell: survey → plan → searchers → synthesis
    /// → critic → verifier → writer. Without --approve, a gated run parks
    /// at the survey/plan checkpoints (interactive when stdin is a
    /// terminal, an error otherwise).
    Research {
        /// What to research.
        topic: String,
        /// Model id to run the pipeline with.
        #[arg(long)]
        model: Option<String>,
        /// Run in this space instead of your default space.
        #[arg(long)]
        space: Option<String>,
        /// Skip the survey and plan-approval gates entirely (like /research!).
        #[arg(long)]
        approve: bool,
        /// Emit the report as JSON (report, session) instead of raw text.
        #[arg(long)]
        json: bool,
        /// Suppress stderr chatter (stage progress).
        #[arg(long)]
        quiet: bool,
    },
    /// MCP connectors: list, add, remove, show tools, import from other apps.
    Connectors {
        #[command(subcommand)]
        cmd: crate::connectors_cli::ConnectorsCmd,
    },
    /// Durable agent jobs, independent of the host and terminal UI.
    Jobs {
        #[command(subcommand)]
        cmd: JobsCmd,
    },
    /// Token/cache/cost analytics, like /usage.
    Usage {
        /// Show every request for this session id, oldest first.
        #[arg(long)]
        session: Option<String>,
        /// Window: 24h, 7d, 30d, or all.
        #[arg(long, default_value = "all")]
        range: String,
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
        /// Rows in the per-model and per-day sections.
        #[arg(long, default_value = "10")]
        top: u64,
        /// Add a per-day breakdown, newest day first.
        #[arg(long)]
        by_day: bool,
    },
    /// List sessions, newest first.
    Sessions {
        /// Only this space's sessions.
        #[arg(long)]
        space: Option<String>,
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<SessionsCmd>,
    },
    /// List spaces.
    Spaces {
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Print a session's latest report + sources (or its full transcript),
    /// like /export.
    Export {
        /// Session id, slug, or id prefix.
        session: String,
        /// Export the whole conversation instead of just the latest report.
        #[arg(long)]
        transcript: bool,
        /// Write to this path instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Zip the local data (db + spaces + skills) into one file.
    Backup {
        /// Destination path (default: `nexus-backup-<date>.zip` next to the
        /// data dir).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Restore a backup into the data dir, overwriting current data.
    Restore {
        /// The backup zip to restore.
        file: PathBuf,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Print (or edit) a space's memory: chapters of facts as Markdown.
    Memory {
        /// Which space (default: your default space).
        #[arg(long, global = true)]
        space: Option<String>,
        /// Open it in $EDITOR; the saved file is applied as a diff.
        #[arg(long)]
        edit: bool,
        #[command(subcommand)]
        command: Option<MemoryCommand>,
    },
    /// Print (or edit) a space's instructions file.
    Instructions {
        /// Which space (default: your default space).
        #[arg(long)]
        space: Option<String>,
        /// Open the file in $EDITOR instead of printing it.
        #[arg(long)]
        edit: bool,
    },
    /// List a space's imported files.
    Files {
        /// Which space (default: your default space).
        #[arg(long)]
        space: Option<String>,
    },
    /// Fetch and list the model catalogs (network).
    Models {
        /// Only this backend's models (openrouter / openai / opencode / codex / mistral).
        #[arg(long)]
        backend: Option<String>,
    },
    /// Save a provider API key to the config file.
    Login {
        /// openrouter, openai, opencode, or mistral (a Vibe plan key).
        provider: String,
        /// The API key.
        key: String,
        /// Verify the key with a live catalog fetch after saving.
        #[arg(long)]
        check: bool,
    },
    /// List or install skills.
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },
    /// Jump into a session: launches the TUI, already in that conversation.
    Open {
        /// Session id, slug, or id prefix.
        session: String,
    },
    /// Convert a local deck to `PowerPoint`, PDF, PNG images, HTML or JSON, or
    /// import a `PowerPoint` file as a deck.
    Deck {
        #[command(subcommand)]
        cmd: DeckCmd,
    },
    /// Export a document or import a Word/Markdown file, entirely offline.
    Doc {
        #[command(subcommand)]
        cmd: DocCmd,
    },
    /// Import Office templates as shared designs, or list space designs.
    Design {
        #[command(subcommand)]
        cmd: DesignCmd,
    },
    /// List the decks and docs of the active space, newest first.
    Library {
        /// Only decks or only docs.
        #[arg(long, value_parser = ["deck", "doc"])]
        kind: Option<String>,
        /// Emit JSON instead of the human-readable list.
        #[arg(long)]
        json: bool,
    },
    /// Open the local studio (decks and docs) in a separate desktop window.
    Studio {
        /// File to open: a deck, doc, .pptx, .docx or .md (otherwise the Library).
        path: Option<PathBuf>,
    },
    /// Update to the latest release from crates.io — runs
    /// `cargo install nexus-chat` when a newer version exists.
    Update,
    /// Sync with another device: export/import changesets, a shared-dir
    /// mailbox, or over ssh. `nexus sync <dir>` and `nexus sync ssh://host`
    /// are the two transports — see `nexus sync --help`.
    Sync {
        #[command(subcommand)]
        cmd: Option<SyncCmd>,
        /// A shared sync directory, or `ssh://user@host`.
        target: Option<PathBuf>,
    },
    /// Run the local HTTP/SSE daemon and provider gateway.
    Host {
        /// Loopback port for the host API.
        #[arg(long, default_value_t = 8643)]
        port: u16,
        /// Start a quick `cloudflared` tunnel and print its public URL.
        #[arg(long)]
        tunnel: bool,
        /// Do not hold the machine awake while hosting.
        #[arg(long)]
        no_sleep_guard: bool,
        /// Provision a named Cloudflare tunnel using `CF_API_TOKEN`.
        #[arg(long)]
        setup: bool,
        /// Install a per-user systemd/launchd service and exit.
        #[arg(long, conflicts_with = "uninstall_service")]
        install_service: bool,
        /// Remove the per-user systemd/launchd service and exit.
        #[arg(long, conflicts_with = "install_service")]
        uninstall_service: bool,
    },
    /// Show data/config paths and which providers are configured.
    Status,
    /// Deeper diagnostics: db integrity, config, optional tools,
    /// `--network` also pings your providers. Exits non-zero on problems.
    Doctor {
        /// Also verify configured provider keys with live calls.
        #[arg(long)]
        network: bool,
    },
}

#[derive(Subcommand)]
pub enum DesignCmd {
    /// Store a PPTX/POTX/DOCX/DOTX template and extract its design.
    Import {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// List designs and their source templates.
    List,
}

#[derive(Subcommand)]
pub enum DeckCmd {
    /// Export offline; default output is <space files>/exports/<deck-slug>.<ext>.
    Export {
        /// Deck path, absolute or relative to the active space's files.
        path: PathBuf,
        #[arg(long, value_parser = ["pptx", "pdf", "png", "html", "json"])]
        format: String,
        /// Explicit output path; existing files are never overwritten.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Use a plain package instead of the design's template.
        #[arg(long, value_parser = ["none"])]
        template: Option<String>,
    },
    /// Import a `PowerPoint` file as an editable deck (<space files>/decks by default).
    Import {
        /// The .pptx file: a path, or a name in the active space's files.
        path: PathBuf,
        /// Deck to write, absolute or relative to the space files, ending in
        /// .deck.json; an existing deck is never overwritten.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum DocCmd {
    /// Export to <space files>/exports; existing files are never overwritten.
    Export {
        path: PathBuf,
        #[arg(long, value_parser = ["docx", "pdf", "md", "html"])]
        format: String,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Use a plain package instead of the design's template.
        #[arg(long, value_parser = ["none"])]
        template: Option<String>,
    },
    /// Import a .docx, .dotx or .md as a new docs/*.doc.json file.
    Import {
        path: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum TrainCmd {
    /// Filter, decontaminate and rescrub recorded turns into JSONL.
    Export {
        #[arg(long, value_parser = ["chat-jsonl", "sharegpt", "dpo"])]
        format: String,
        #[arg(long, default_value = "success", value_parser = ["success", "all"])]
        filter: String,
        /// Comma-separated model globs, qualified by backend when needed.
        #[arg(long, default_value = "local:*,distillable", value_delimiter = ',')]
        models: Vec<String>,
        /// Inclusive UTC date (YYYY-MM-DD); complete turns are retained.
        #[arg(long)]
        since: Option<chrono::NaiveDate>,
        /// Only this space; omitted means all spaces.
        #[arg(long)]
        space: Option<String>,
        #[arg(long, default_value = "./evals")]
        evals: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Also write train.jsonl / valid.jsonl (90/10, chat-jsonl only).
        #[arg(long)]
        split: bool,
    },
}

#[derive(Subcommand)]
pub enum JobsCmd {
    /// List jobs, including read-only jobs owned by other devices.
    List,
    /// Run one job immediately (also used by the TUI).
    Run { id: String },
    /// Run each due job once, then exit.
    RunDue,
    /// Check every 60 seconds, running one job at a time.
    Daemon,
    /// Write a per-user service definition, without activating it.
    InstallService,
    /// Remove the service definition, without stopping a loaded service.
    UninstallService,
}

#[derive(Subcommand)]
pub enum SessionsCmd {
    /// Delete one session (by id, slug, or id prefix).
    Rm {
        /// Session id, slug, or id prefix.
        session: String,
    },
    /// Delete old sessions.
    Prune {
        /// Keep the N most recent sessions per space.
        #[arg(long)]
        keep: Option<u64>,
        /// Delete sessions older than this many days.
        #[arg(long)]
        days: Option<u64>,
        /// Print what would be deleted without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum SkillsCmd {
    /// List installed skills.
    List,
    /// Install a skill from GitHub (`owner/repo` or `owner/repo/path`).
    Install {
        /// `owner/repo[/path]` shorthand.
        skill: String,
    },
}

#[derive(Subcommand)]
pub enum SyncCmd {
    /// Export a changeset (JSON on stdout). `--dir` also writes the file
    /// blobs into `DIR/blobs/`. Without a peer, exports everything.
    Export {
        /// Which peer to export for (the other device's id, from its
        /// changesets). Full export when unknown.
        #[arg(long)]
        peer: Option<String>,
        /// Also write the blob payloads into this directory.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Import a changeset or bundle (stdin when no file is given) and
    /// print the reply — your own export + ack — on stdout. Zip bundles
    /// (the ssh transport) are detected by magic bytes.
    Import {
        /// Changeset JSON or bundle zip; stdin when omitted.
        path: Option<PathBuf>,
        /// Reply as a bundle (zip with blobs) instead of plain JSON.
        #[arg(long)]
        bundle: bool,
    },
}

/// Parses launch flags and an optional headless command, exiting on help or invalid arguments.
pub fn parse() -> (bool, bool, bool, Option<Command>) {
    let cli = Cli::parse();
    (
        cli.continue_session,
        cli.selection_chat,
        cli.selection_chat_send_only,
        cli.command,
    )
}

#[allow(clippy::too_many_lines)]
pub async fn run(cmd: Command) -> Result<()> {
    match cmd {
        Command::Ask {
            prompt,
            model,
            space,
            json,
            quiet,
        } => ask(prompt, model, space, json, quiet).await,
        Command::Eval {
            dir,
            model,
            filter,
            set,
            compare,
            compare_model,
            json,
        } => {
            eval(
                &dir,
                model.as_deref(),
                filter.as_deref(),
                &set,
                compare,
                &compare_model,
                json,
            )
            .await
        }
        Command::Train { cmd } => train_export(cmd),
        Command::Clip {
            instruction,
            instruction_option,
            model,
            space,
            copy,
            json,
            quiet,
        } => {
            clipboard(
                instruction.or(instruction_option),
                ask_options(model, space, json, quiet),
                copy,
            )
            .await
        }
        Command::Chat {
            model,
            space,
            quiet,
        } => chat(model, space, quiet).await,
        Command::Research {
            topic,
            model,
            space,
            approve,
            json,
            quiet,
        } => research(topic, model, space, approve, json, quiet).await,
        Command::Connectors { cmd } => crate::connectors_cli::run(cmd).await,
        Command::Jobs { cmd } => jobs(cmd).await,
        Command::Usage {
            session,
            range,
            json,
            top,
            by_day,
        } => usage(&range, json, top, by_day, session.as_deref()),
        Command::Sessions { space, json, cmd } => sessions(space.as_deref(), json, cmd),
        Command::Spaces { json } => spaces(json),
        Command::Export {
            session,
            transcript,
            output,
        } => export(&session, transcript, output.as_deref()),
        Command::Backup { output } => backup(output.as_deref()),
        Command::Restore { file, yes } => restore(&file, yes),
        Command::Memory {
            space,
            edit,
            command,
        } => memory(space.as_deref(), edit, command),
        Command::Instructions { space, edit } => instructions(space.as_deref(), edit),
        Command::Files { space } => files(space.as_deref()),
        Command::Models { backend } => models(backend.as_deref()).await,
        Command::Login {
            provider,
            key,
            check,
        } => login(&provider, &key, check).await,
        Command::Skills { cmd } => skills(cmd).await,
        Command::Open { session } => open(&session),
        Command::Sync { cmd, target } => sync_cmd(cmd, target.as_deref()),
        Command::Host {
            port,
            tunnel,
            no_sleep_guard,
            setup,
            install_service,
            uninstall_service,
        } => {
            host(HostOptions {
                port,
                tunnel,
                no_sleep_guard,
                setup,
                service_action: if install_service {
                    Some(HostServiceAction::Install)
                } else if uninstall_service {
                    Some(HostServiceAction::Uninstall)
                } else {
                    None
                },
            })
            .await
        }
        Command::Deck { cmd } => {
            let space = space::Space::open()?;
            let db = db::Db::open(&space.db_path())?;
            let files = space.files_dir(&resolve_space_name(&db, None)?);
            deck_export(cmd, &files)
        }
        Command::Doc { cmd } => {
            let space = space::Space::open()?;
            let db = db::Db::open(&space.db_path())?;
            let files = space.files_dir(&resolve_space_name(&db, None)?);
            doc_files(cmd, &files)
        }
        Command::Design { cmd } => {
            let space = space::Space::open()?;
            let db = db::Db::open(&space.db_path())?;
            let files = space.files_dir(&resolve_space_name(&db, None)?);
            design_files(cmd, &files)
        }
        Command::Library { kind, json } => {
            let space = space::Space::open()?;
            let db = db::Db::open(&space.db_path())?;
            let files = space.files_dir(&resolve_space_name(&db, None)?);
            crate::cli_library::run(kind.as_deref(), json, &files)
        }
        Command::Studio { path } => launch_studio(path.as_deref()),
        Command::Update => update().await,
        Command::Status => status(),
        Command::Doctor { network } => doctor(network).await,
    }
}
/// A validated, non-persistent experiment override for an eval run.
#[derive(Clone)]
pub struct EvalSetting {
    key: String,
    value: String,
}

pub(super) struct HostOptions {
    port: u16,
    tunnel: bool,
    no_sleep_guard: bool,
    setup: bool,
    service_action: Option<HostServiceAction>,
}

/// Resolves an ID, slug or UUID prefix to a session and its owning space.
pub(crate) fn resolve_session(db: &db::Db, reference: &str) -> Result<(String, db::Session)> {
    for sp in db.list_spaces()? {
        for s in db.list_sessions(&sp.id)? {
            if s.id == reference
                || s.slug.as_deref() == Some(reference)
                || s.id.starts_with(reference)
            {
                return Ok((sp.id.clone(), s));
            }
        }
    }
    bail!("no session matching {reference:?} — `nexus sessions` lists them")
}

fn resolve_space_name(db: &db::Db, space_name: Option<&str>) -> Result<String> {
    if let Some(name) = space_name {
        db.list_spaces()?
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.name)
            .ok_or_else(|| anyhow!("no space named {name:?} — `nexus spaces` lists them"))
    } else {
        let id = db.default_space_id()?;
        db.list_spaces()?
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.name)
            .context("default space missing")
    }
}

/// Writes stdout without panicking when a pipe reader exits early.
pub(crate) fn out(line: impl std::fmt::Display) {
    let mut stdout = std::io::stdout();
    if writeln!(stdout, "{line}").is_err() {
        std::process::exit(0);
    }
}

fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

fn fmt_req(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!(
        "{}…",
        s.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

fn fmt_ts(rfc3339: &str) -> String {
    rfc3339.chars().take(16).collect()
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = bytes as f64 / KB;
    if kb < KB {
        return format!("{kb:.1} KiB");
    }
    let mb = kb / KB;
    if mb < KB {
        return format!("{mb:.1} MiB");
    }
    format!("{:.1} GiB", mb / KB)
}

#[cfg(test)]
#[path = "cli_deck_export_tests.rs"]
mod deck_export_tests;
#[cfg(test)]
#[path = "cli_doc_tests.rs"]
mod doc_tests;

#[cfg(test)]
#[path = "cli_design_tests.rs"]
mod design_tests;
#[cfg(test)]
#[path = "cli_usage_tests.rs"]
mod usage_tests;

#[cfg(test)]
mod tests;
