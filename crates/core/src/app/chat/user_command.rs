//! `!cmd` user shell commands: run, stream, record and replay.

use std::fmt::Write as _;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::db::Message;

use super::App;

impl App {
    #[must_use]
    pub fn user_command_running(&self) -> bool {
        self.user_command.rx.is_some()
    }

    pub fn user_command_cwd(&self) -> std::path::PathBuf {
        if let Some(dir) = &self.user_command.dir {
            return dir.clone();
        }
        let default = self.default_user_command_cwd();
        default.canonicalize().unwrap_or(default)
    }

    fn default_user_command_cwd(&self) -> std::path::PathBuf {
        if let Some(workspace) = self
            .workspace
            .as_ref()
            .filter(|_| self.work_mode.is_agent())
        {
            return workspace.root().to_path_buf();
        }
        let _ = self.space.ensure_space_dir(&self.active_space.name);
        self.space.space_dir(&self.active_space.name)
    }

    pub(in crate::app) fn reset_user_command_dir(&mut self) {
        self.user_command.dir = None;
        self.user_command.context = None;
        self.user_command.epoch += 1;
    }

    pub(in crate::app) fn user_command_context(&self) -> UserCommandContext {
        UserCommandContext {
            session: self.session.as_ref().map(|session| session.id.clone()),
            space: self.active_space.id.clone(),
            mode: self.work_mode,
            root: self
                .workspace
                .as_ref()
                .map(|workspace| workspace.root().to_path_buf()),
        }
    }

    pub fn report_user_shell_warning(&mut self) {
        if !self.user_command.shell_warning_shown
            && let Some(reason) = self
                .user_command
                .shell
                .get()
                .and_then(|setup| setup.warning.clone())
        {
            self.user_command.shell_warning_shown = true;
            self.push_status(format!("! runs without your shell setup · {reason}"));
        }
    }

    /// Run an unsandboxed user command and capture its output for the conversation.
    pub fn run_user_command(&mut self, cmd: String) -> Result<()> {
        if !self.work_mode.is_agent() {
            self.push_status(CHAT_MODE_BANG_STATUS);
            return Ok(());
        }
        if !self.interactive_ui {
            self.push_status("!commands run only in the terminal UI");
            return Ok(());
        }
        if self.is_streaming() || self.user_command.rx.is_some() {
            self.push_status(if self.is_streaming() {
                "wait — response still streaming"
            } else {
                "wait — a command is still running"
            });
            self.push_composer_set(format!("!{cmd}"));
            return Ok(());
        }
        if let Some(word) = crate::usershell::terminal_program(&cmd) {
            self.push_status(format!("!{word} needs a terminal · run it outside nexus"));
            return Ok(());
        }
        let cwd = self.user_command_cwd();
        let session_id =
            self.ensure_session(&cmd, self.current_model.clone().unwrap_or_default())?;
        self.user_command.context = Some(self.user_command_context());
        let epoch = self.user_command.epoch;
        let setup = std::sync::Arc::clone(&self.user_command.shell);
        let data = self.space.root.clone();
        let scrubber = crate::trace::Scrubber::new(self.saved_secrets());
        let incognito = self.incognito;
        let (tx, rx) = mpsc::unbounded_channel();
        self.user_command.rx = Some(rx);
        self.push_status(format!("running: {cmd}"));
        tokio::spawn(async move {
            let setup = setup
                .get_or_init(|| crate::usershell::UserShell::load(&data))
                .await;
            let cwd_file = data.join(format!("shell-cwd-{}", uuid::Uuid::new_v4()));
            let (exit, started, output) =
                capture_with_shell(&cmd, &cwd, USER_COMMAND_TIMEOUT, setup, &cwd_file).await;
            let next_dir = std::fs::read_to_string(&cwd_file)
                .ok()
                .map(|text| std::path::PathBuf::from(text.trim_end_matches('\n')))
                .filter(|path| path.is_absolute() && path.is_dir());
            let _ = std::fs::remove_file(cwd_file);
            let _ = tx.send(UserCommandRun {
                session_id,
                incognito,
                cmd,
                cwd: crate::usershell::display_dir(&cwd),
                next_dir,
                epoch,
                exit,
                started,
                output: crate::tools::cap_head_tail(&scrubber.scrub(&output), USER_COMMAND_CHARS),
            });
        });
        Ok(())
    }

    pub(in crate::app) fn finish_user_command(&mut self, run: &UserCommandRun) -> Result<()> {
        if run.epoch == self.user_command.epoch
            && let Some(dir) = &run.next_dir
        {
            self.user_command.dir = Some(dir.clone());
        }
        self.push_status(format!(
            "ran: {} · {}",
            run.cmd,
            user_command_outcome_label(run.exit, run.started)
        ));
        let content = user_command_row(
            &run.cmd,
            &run.cwd,
            run.exit,
            run.started,
            &run.output,
            false,
        );
        self.store_user_command(&run.session_id, run.incognito, content)?;
        self.report_user_shell_warning();
        Ok(())
    }

    /// Record a terminal command whose output was not captured.
    pub fn record_user_command(
        &mut self,
        cmd: &str,
        exit: Option<i32>,
        started: bool,
    ) -> Result<()> {
        if !self.work_mode.is_agent() {
            self.push_status(CHAT_MODE_BANG_STATUS);
            return Ok(());
        }
        if !self.interactive_ui {
            self.push_status("!commands run only in the terminal UI");
            return Ok(());
        }
        let cwd = crate::usershell::display_dir(&self.user_command_cwd());
        let session_id =
            self.ensure_session(cmd, self.current_model.clone().unwrap_or_default())?;
        self.push_status(format!(
            "ran: {cmd} · {}",
            user_command_outcome_label(exit, started)
        ));
        let content = user_command_row(
            cmd,
            &cwd,
            exit,
            started,
            USER_COMMAND_INTERACTIVE_NOTE,
            true,
        );
        self.store_user_command(&session_id, self.incognito, content)?;
        self.push_history_invalidated();
        Ok(())
    }

    fn store_user_command(
        &mut self,
        session_id: &str,
        incognito: bool,
        content: String,
    ) -> Result<()> {
        if !incognito {
            self.db.add_user_command_message(session_id, &content)?;
        }
        self.ui_repair.messages.remove(session_id);
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.id == session_id)
        {
            self.messages.push(Message {
                role: "user_command".to_string(),
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
}

const CHAT_MODE_BANG_STATUS: &str = "! commands run in agent mode";

pub(super) const USER_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);
const USER_COMMAND_CHARS: usize = 8_000;
pub(super) const USER_COMMAND_KEEP_BYTES: usize = 64 * 1024;
pub(super) const USER_COMMAND_INTERACTIVE_NOTE: &str =
    "(interactive — output not captured; paste anything relevant)";

// Escaped descendants can hold the output pipe open after the process group exits.
const USER_COMMAND_DRAIN: std::time::Duration = std::time::Duration::from_secs(2);
pub(super) const USER_COMMAND_STREAM_OPEN_NOTE: &str =
    "(output stream still open — a background process kept it; not waited for)";

#[derive(PartialEq, Eq)]
pub(in crate::app) struct UserCommandContext {
    session: Option<String>,
    space: String,
    mode: super::WorkMode,
    root: Option<std::path::PathBuf>,
}

pub(crate) struct UserCommandRun {
    session_id: String,
    incognito: bool,
    cmd: String,
    cwd: String,
    next_dir: Option<std::path::PathBuf>,
    epoch: u64,
    exit: Option<i32>,
    started: bool,
    output: String,
}

pub fn user_command_outcome_label(exit: Option<i32>, started: bool) -> String {
    match exit {
        Some(code) => format!("exit {code}"),
        None if started => "killed".to_string(),
        None => "failed to start".to_string(),
    }
}

pub fn user_command_outcome(row: &serde_json::Value) -> String {
    user_command_outcome_label(
        row["exit"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok()),
        row["started"].as_bool().unwrap_or(true),
    )
}

pub(super) fn user_command_row(
    cmd: &str,
    cwd: &str,
    exit: Option<i32>,
    started: bool,
    output: &str,
    interactive: bool,
) -> String {
    serde_json::json!({
        "cmd": cmd,
        "cwd": cwd,
        "exit": exit,
        "started": started,
        "output": output,
        "interactive": interactive,
    })
    .to_string()
}

pub(super) fn user_command_replay(content: &str) -> String {
    let Ok(row) = serde_json::from_str::<serde_json::Value>(content) else {
        return content.to_string();
    };
    let field = |key: &str| row[key].as_str().unwrap_or_default();
    let mut text = format!(
        "I ran `{}` in {} ({}):\n",
        field("cmd"),
        field("cwd"),
        user_command_outcome(&row)
    );
    let output = field("output");
    if row["interactive"].as_bool() == Some(true) {
        text.push_str(output);
    } else {
        let longest = output
            .split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or_default();
        let fence = "`".repeat(longest.max(2) + 1);
        let _ = write!(text, "{fence}\n{}\n{fence}", output.trim_end());
    }
    text
}

#[cfg(test)]
pub(super) async fn capture_user_command(
    cmd: &str,
    cwd: &std::path::Path,
    timeout: std::time::Duration,
) -> (Option<i32>, bool, String) {
    let setup = crate::usershell::UserShell {
        shell: "/bin/sh".into(),
        snapshot: None,
        path: String::new(),
        names: Vec::new(),
        warning: None,
    };
    capture_with_shell(cmd, cwd, timeout, &setup, std::path::Path::new("/dev/null")).await
}

async fn capture_with_shell(
    cmd: &str,
    cwd: &std::path::Path,
    timeout: std::time::Duration,
    setup: &crate::usershell::UserShell,
    cwd_file: &std::path::Path,
) -> (Option<i32>, bool, String) {
    let (child, reader) = match spawn_captured(cmd, cwd, setup, cwd_file) {
        Ok(spawned) => spawned,
        Err(error) => return (None, false, format!("could not run: {error}")),
    };
    match collect_captured(child, reader, timeout).await {
        Ok((exit, output)) => (exit, true, output),
        Err(error) => (None, true, format!("could not read its output: {error}")),
    }
}

fn spawn_captured(
    cmd: &str,
    cwd: &std::path::Path,
    setup: &crate::usershell::UserShell,
    cwd_file: &std::path::Path,
) -> std::io::Result<(tokio::process::Child, std::io::PipeReader)> {
    // One pipe behind both descriptors keeps stdout and stderr in write order.
    let (reader, writer) = std::io::pipe()?;
    let mut command = tokio::process::Command::new(&setup.shell);
    command
        .args(["-c", &setup.wrapper(cmd, cwd)])
        .env("NEXUS_CWD_FILE", cwd_file)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn()?;
    // Dropping `command` closes the parent's write ends so the reader can reach EOF.
    Ok((child, reader))
}

async fn collect_captured(
    mut child: tokio::process::Child,
    reader: std::io::PipeReader,
    timeout: std::time::Duration,
) -> std::io::Result<(Option<i32>, String)> {
    let group = child.id();
    let sink = std::sync::Arc::new(std::sync::Mutex::new(HeadTail::default()));
    let reader_sink = std::sync::Arc::clone(&sink);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    // A reader stuck on an escaped descendant must not block runtime shutdown.
    std::thread::spawn(move || {
        let _ = done_tx.send(read_into(reader, &reader_sink));
    });
    let (exit, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => (status?.code(), false),
        Err(_) => (None, true),
    };
    kill_group(group);
    if timed_out {
        child.wait().await?;
    }
    let stream_open = match tokio::time::timeout(USER_COMMAND_DRAIN, done_rx).await {
        Ok(Ok(read)) => {
            read?;
            false
        }
        Ok(Err(closed)) => return Err(std::io::Error::other(closed)),
        Err(_) => true,
    };
    let mut output = sink.lock().expect("output buffer").text();
    if timed_out {
        let _ = write!(output, "\n[timed out after {timeout:?} · killed]");
    }
    if stream_open {
        let _ = write!(output, "\n{USER_COMMAND_STREAM_OPEN_NOTE}");
    }
    Ok((exit, output))
}

#[cfg(unix)]
fn kill_group(group: Option<u32>) {
    if let Some(pid) = group.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: the pid came from our child, spawned as its own process
        // group leader; negating it signals only that group.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}

#[cfg(not(unix))]
fn kill_group(_group: Option<u32>) {}

/// Bounded output capture that preserves the start and end of a stream.
#[derive(Default)]
pub(super) struct HeadTail {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: usize,
}

impl HeadTail {
    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len();
        let split = (USER_COMMAND_KEEP_BYTES - self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..split]);
        self.tail.extend(&bytes[split..]);
        if self.tail.len() > USER_COMMAND_KEEP_BYTES {
            self.tail.drain(..self.tail.len() - USER_COMMAND_KEEP_BYTES);
        }
    }

    pub(super) fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        let omitted = self.total - self.head.len() - self.tail.len();
        if omitted > 0 {
            let _ = write!(text, "\n… [{omitted} bytes omitted] …\n");
        }
        let (front, back) = self.tail.as_slices();
        text.push_str(&String::from_utf8_lossy(&[front, back].concat()));
        text
    }
}

fn read_into(
    mut reader: impl std::io::Read,
    sink: &std::sync::Mutex<HeadTail>,
) -> std::io::Result<()> {
    let mut chunk = [0; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => sink.lock().expect("output buffer").push(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}
