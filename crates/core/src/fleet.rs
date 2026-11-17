//! The fleet: external agent CLIs (Claude Code, Codex, pi, `OpenCode`) each
//! running in its own pseudo-terminal, owned by the app and shared across
//! toolbox refreshes. An agent's screen is kept as a terminal model, so its
//! state can be read off it, a frontend can draw it and type into it, and the
//! model can read it as text. Agents stop when Dugix exits.

use std::collections::BTreeMap;
use std::io::{Read as _, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

mod check;
mod detect;
#[cfg(all(test, unix))]
mod tests;
mod worktree;

pub use check::Check;
pub use vt100::{Cell, Color, Screen};
pub use worktree::Changes;
use worktree::Worktree;

/// Agents alive at once, across every conversation.
pub const MAX_LIVE: usize = 8;
/// Longest one `fleet wait` blocks; the model waits again for longer work.
const MAX_WAIT_S: u64 = 600;
/// The terminal an agent starts in, until a frontend attaches and resizes it.
const START_SIZE: (u16, u16) = (30, 110);
const SCROLLBACK: usize = 2_000;
/// How often every agent's state is read off its screen.
const TICK: Duration = Duration::from_millis(200);
/// Between a steering message and the Enter that submits it: sent together,
/// the CLIs take the Enter as part of a paste and only add a line.
const SUBMIT_AFTER: Duration = Duration::from_millis(150);
/// How long a CLI may take to draw its first screen and still count as starting.
const STARTUP: Duration = Duration::from_secs(20);
/// Ticks between measuring what each worktree agent has changed.
const MEASURE_EVERY: u32 = 10;
/// Screen text the model is shown per agent.
const READ_CAP: usize = 6_000;
/// Patch text the model is shown per agent.
const DIFF_CAP: usize = 24_000;

/// An agent CLI the fleet knows how to launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Claude,
    Codex,
    Pi,
    OpenCode,
}

/// Whether an agent started in `dir` gets its own branch and worktree.
#[must_use]
pub fn isolates(dir: &std::path::Path) -> bool {
    worktree::is_repo(dir)
}

impl Kind {
    pub const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Pi, Self::OpenCode];
    pub const NAMES: &[&str] = &["claude", "codex", "pi", "opencode"];

    /// The program's name, which is also what the user and the model call it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::OpenCode => "opencode",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The CLI's own supervised mode for working without asking: its
    /// classifier, sandbox or reviewer still stands between it and risky
    /// actions. Used only on its own worktree; pi has no permissions to set.
    #[must_use]
    pub const fn auto_flags(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["--permission-mode", "auto"],
            Self::Codex => &["--sandbox", "workspace-write", "--approve-for-me"],
            Self::OpenCode => &["--auto"],
            Self::Pi => &[],
        }
    }

    /// The interactive CLI on `model` (its default without one), opened on `prompt`.
    fn argv(self, prompt: &str, model: Option<&str>) -> Vec<String> {
        let mut argv = vec![self.name().to_string()];
        if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
            argv.extend(["--model".to_string(), model.to_string()]);
        }
        if prompt.is_empty() {
            return argv;
        }
        if self == Self::OpenCode {
            argv.push("--prompt".into());
        }
        argv.push(prompt.to_string());
        argv
    }
}

/// What to run for one agent.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The short name shown beside the agent's number.
    pub label: String,
    /// What the agent was asked; its first line titles it.
    pub title: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Give it its own git worktree and branch when `cwd` is in a repository.
    pub isolate: bool,
    /// Flags that let it work without asking, added after the program only
    /// when it gets its own worktree; in the user's tree it keeps asking.
    pub auto: Vec<String>,
}

impl Launch {
    #[must_use]
    pub fn agent(kind: Kind, prompt: &str, model: Option<&str>, cwd: PathBuf) -> Self {
        Self {
            label: kind.name().into(),
            title: match prompt.lines().next().unwrap_or_default().trim() {
                "" => "interactive session".into(),
                title => title.to_string(),
            },
            argv: kind.argv(prompt, model),
            cwd,
            isolate: true,
            auto: kind
                .auto_flags()
                .iter()
                .map(|flag| (*flag).to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// A turn is running.
    Working,
    /// It asked for a permission or trust decision and waits for the user.
    Blocked,
    /// Its turn ended; it waits for the next instruction.
    Idle,
    /// The CLI exited with this code.
    Exited(u32),
}

impl State {
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Working => "working".into(),
            Self::Blocked => "blocked".into(),
            Self::Idle => "idle".into(),
            Self::Exited(code) => format!("exited {code}"),
        }
    }

    #[must_use]
    pub const fn live(self) -> bool {
        !matches!(self, Self::Exited(_))
    }
}

/// One agent as a list row.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub id: u64,
    pub session_id: String,
    pub label: String,
    pub title: String,
    pub state: State,
    pub uptime: Duration,
    /// The last line on its screen.
    pub last_line: String,
    /// Its own branch, when it works in a worktree.
    pub branch: Option<String>,
    /// What it has changed there since it started or was last merged.
    pub changes: Option<Changes>,
    /// The last check of its work, until it works again.
    pub check: Option<Check>,
}

impl AgentInfo {
    /// `claude 2`: how the agent is named everywhere.
    #[must_use]
    pub fn name(&self) -> String {
        format!("{} {}", self.label, self.id)
    }
}

/// An agent moved from one state to another.
#[derive(Debug, Clone)]
pub struct FleetMsg {
    pub agent: AgentInfo,
    pub previous: State,
    /// How long it was in `previous`.
    pub lasted: Duration,
    /// A check of its work that just finished; the state did not move.
    pub checked: Option<Check>,
}

/// Answers to the terminal queries CLIs make at startup; without a cursor
/// position some of them refuse to draw.
#[derive(Default)]
struct Replies(Vec<u8>);

impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|param| param.first()).copied();
        match (i1, c, first) {
            (None, 'n', Some(6)) => {
                let (row, col) = screen.cursor_position();
                self.0
                    .extend(format!("\x1b[{};{}R", row + 1, col + 1).bytes());
            }
            (None, 'n', Some(5)) => self.0.extend(b"\x1b[0n"),
            (None, 'c', None | Some(0)) => self.0.extend(b"\x1b[?1;2c"),
            (Some(b'>'), 'c', _) => self.0.extend(b"\x1b[>0;0;0c"),
            _ => {}
        }
    }
}

struct Term {
    parser: vt100::Parser<Replies>,
    last_output: Instant,
    /// The size a frontend last asked for.
    size: (u16, u16),
}

struct Agent {
    session_id: String,
    label: String,
    title: String,
    started: Instant,
    term: Mutex<Term>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    exit: Mutex<Option<u32>>,
    /// The state last announced, and since when.
    state: Mutex<(State, Instant)>,
    worktree: Option<Worktree>,
    /// The worktree's changes as last measured.
    changes: Mutex<Changes>,
    check: Mutex<Option<Check>>,
}

impl Agent {
    fn screen_text(&self) -> String {
        let term = self.term.lock().expect("agent terminal");
        term.parser.screen().contents().trim_end().to_string()
    }

    fn current(&self) -> State {
        if let Some(code) = *self.exit.lock().expect("agent exit") {
            return State::Exited(code);
        }
        let term = self.term.lock().expect("agent terminal");
        let screen = term.parser.screen().contents();
        // Some CLIs load for seconds before their first paint.
        if screen.trim().is_empty() && self.started.elapsed() < STARTUP {
            return State::Working;
        }
        detect::classify(&screen, term.last_output.elapsed())
    }

    fn info(&self, id: u64) -> AgentInfo {
        AgentInfo {
            id,
            session_id: self.session_id.clone(),
            label: self.label.clone(),
            title: self.title.clone(),
            state: self.state.lock().expect("agent state").0,
            uptime: self.started.elapsed(),
            last_line: self
                .screen_text()
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .to_string(),
            branch: self.worktree.as_ref().map(|tree| tree.branch.clone()),
            changes: self
                .worktree
                .as_ref()
                .map(|_| *self.changes.lock().expect("agent changes")),
            check: self.check.lock().expect("agent check").clone(),
        }
    }

    fn measure(&self) {
        if let Some(Ok(changes)) = self.worktree.as_ref().map(Worktree::changes) {
            *self.changes.lock().expect("agent changes") = changes;
        }
    }

    /// The commit message for work the agent left uncommitted.
    fn check_command(&self) -> String {
        self.check
            .lock()
            .expect("agent check")
            .as_ref()
            .map(|check| check.command().to_string())
            .unwrap_or_default()
    }

    fn commit_message(&self) -> String {
        format!("{}: {}", self.label, self.title)
    }

    fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().expect("agent input");
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    fn kill(&self) {
        let _ = self.killer.lock().expect("agent killer").kill();
        if let Some(Check::Running { pid: Some(pid), .. }) =
            *self.check.lock().expect("agent check")
        {
            check::stop(pid);
        }
    }
}

pub struct Fleet {
    agents: Mutex<BTreeMap<u64, Arc<Agent>>>,
    next: AtomicU64,
    events: mpsc::UnboundedSender<FleetMsg>,
    changed: tokio::sync::Notify,
    ticking: AtomicBool,
}

impl Fleet {
    #[must_use]
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<FleetMsg>) {
        let (events, receiver) = mpsc::unbounded_channel();
        let fleet = Arc::new(Self {
            agents: Mutex::default(),
            next: AtomicU64::new(1),
            events,
            changed: tokio::sync::Notify::new(),
            ticking: AtomicBool::new(false),
        });
        (fleet, receiver)
    }

    fn agent(&self, id: u64) -> Result<Arc<Agent>> {
        self.agents
            .lock()
            .expect("fleet")
            .get(&id)
            .cloned()
            .with_context(|| format!("no fleet agent {id}"))
    }

    /// An agent that is still running, for input.
    fn live_agent(&self, id: u64) -> Result<Arc<Agent>> {
        let agent = self.agent(id)?;
        if let Some(code) = *agent.exit.lock().expect("agent exit") {
            bail!("fleet agent {id} has exited ({code})");
        }
        Ok(agent)
    }

    /// Agents still running, across every conversation.
    #[must_use]
    pub fn live(&self) -> usize {
        self.count(State::live)
    }

    /// Agents waiting on a decision from the user.
    #[must_use]
    pub fn blocked(&self) -> usize {
        self.count(|state| state == State::Blocked)
    }

    fn count(&self, matching: impl Fn(State) -> bool) -> usize {
        self.agents
            .lock()
            .expect("fleet")
            .values()
            .filter(|agent| matching(agent.state.lock().expect("agent state").0))
            .count()
    }

    /// Start `launch` in its own terminal and return the agent's number.
    pub fn spawn(self: &Arc<Self>, session_id: &str, launch: Launch) -> Result<u64> {
        if self.live() >= MAX_LIVE {
            bail!("{MAX_LIVE} fleet agents already running · stop one first");
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (worktree, cwd) = if launch.isolate && worktree::is_repo(&launch.cwd) {
            let tag = uuid::Uuid::new_v4().simple().to_string();
            let name = format!("{}-{id}-{}", launch.label, &tag[..4]);
            let (tree, cwd) = Worktree::create(&launch.cwd, &name)?;
            (Some(tree), cwd)
        } else {
            (None, launch.cwd.clone())
        };
        let (rows, cols) = START_SIZE;
        let pty = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let (program, command) = command(&launch, &cwd, worktree.as_ref())?;
        let mut child = match pty.slave.spawn_command(command) {
            Ok(child) => child,
            Err(error) => {
                if let Some(tree) = &worktree {
                    let _ = tree.remove("");
                }
                return Err(error.context(format!(
                    "could not start {program} · is it installed and on PATH?"
                )));
            }
        };
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader()?;
        let now = Instant::now();
        let agent = Arc::new(Agent {
            session_id: session_id.to_string(),
            label: launch.label,
            title: launch.title,
            started: now,
            term: Mutex::new(Term {
                parser: vt100::Parser::new_with_callbacks(
                    cells(rows),
                    cells(cols),
                    SCROLLBACK,
                    Replies::default(),
                ),
                last_output: now,
                size: START_SIZE,
            }),
            writer: Arc::new(Mutex::new(pty.master.take_writer()?)),
            killer: Mutex::new(child.clone_killer()),
            master: Mutex::new(pty.master),
            exit: Mutex::new(None),
            state: Mutex::new((State::Working, now)),
            worktree,
            changes: Mutex::default(),
            check: Mutex::default(),
        });
        self.agents
            .lock()
            .expect("fleet")
            .insert(id, Arc::clone(&agent));

        let output = Arc::clone(&agent);
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            while let Ok(read @ 1..) = reader.read(&mut buffer) {
                let replies = {
                    let mut term = output.term.lock().expect("agent terminal");
                    term.parser.process(&buffer[..read]);
                    term.last_output = Instant::now();
                    std::mem::take(&mut term.parser.callbacks_mut().0)
                };
                if !replies.is_empty() {
                    let _ = output.write(&replies);
                }
            }
        });
        let fleet = Arc::downgrade(self);
        std::thread::spawn(move || {
            let code = child.wait().map_or(1, |status| status.exit_code());
            *agent.exit.lock().expect("agent exit") = Some(code);
            agent.measure();
            if let Some(fleet) = fleet.upgrade() {
                fleet.refresh();
            }
        });
        self.tick();
        Ok(id)
    }

    /// Keep states fresh while any agent runs: most transitions come from
    /// time passing without output, which no event announces.
    fn tick(self: &Arc<Self>) {
        if self.ticking.swap(true, Ordering::AcqRel) {
            return;
        }
        let fleet: Weak<Self> = Arc::downgrade(self);
        std::thread::spawn(move || {
            let mut ticks = 0_u32;
            loop {
                std::thread::sleep(TICK);
                let Some(fleet) = fleet.upgrade() else { return };
                fleet.refresh();
                ticks = ticks.wrapping_add(1);
                if ticks.is_multiple_of(MEASURE_EVERY) {
                    fleet.measure();
                }
                if fleet.live() == 0 {
                    fleet.ticking.store(false, Ordering::Release);
                    // An agent started between the count and the store has no ticker.
                    if fleet.live() == 0 || fleet.ticking.swap(true, Ordering::AcqRel) {
                        return;
                    }
                }
            }
        });
    }

    /// Measure what every running worktree agent has changed.
    fn measure(&self) {
        let agents: Vec<Arc<Agent>> = self
            .agents
            .lock()
            .expect("fleet")
            .values()
            .cloned()
            .collect();
        for agent in agents {
            if agent.exit.lock().expect("agent exit").is_none() {
                agent.measure();
            }
        }
    }

    /// Read every agent's state and announce the ones that moved.
    fn refresh(&self) {
        let agents: Vec<(u64, Arc<Agent>)> = self
            .agents
            .lock()
            .expect("fleet")
            .iter()
            .map(|(id, agent)| (*id, Arc::clone(agent)))
            .collect();
        let mut moved = false;
        for (id, agent) in agents {
            let current = agent.current();
            let (previous, lasted) = {
                let mut state = agent.state.lock().expect("agent state");
                if state.0 == current {
                    continue;
                }
                let was = (state.0, state.1.elapsed());
                *state = (current, Instant::now());
                was
            };
            if current == State::Working {
                // New work makes a finished check stale.
                let mut check = agent.check.lock().expect("agent check");
                if check.as_ref().is_some_and(|check| check.passed().is_some()) {
                    *check = None;
                }
            }
            moved = true;
            let _ = self.events.send(FleetMsg {
                agent: agent.info(id),
                previous,
                lasted,
                checked: None,
            });
        }
        if moved {
            self.changed.notify_waiters();
        }
    }

    /// Every agent, oldest first; `session_id` narrows to one conversation.
    #[must_use]
    pub fn list(&self, session_id: Option<&str>) -> Vec<AgentInfo> {
        self.agents
            .lock()
            .expect("fleet")
            .iter()
            .filter(|(_, agent)| session_id.is_none_or(|id| agent.session_id == id))
            .map(|(id, agent)| agent.info(*id))
            .collect()
    }

    #[must_use]
    pub fn info(&self, id: u64) -> Option<AgentInfo> {
        self.agent(id).ok().map(|agent| agent.info(id))
    }

    /// Steer an agent: type `text` into it and submit.
    pub fn send(&self, id: u64, text: &str) -> Result<()> {
        let agent = self.live_agent(id)?;
        let pasted = {
            let mut term = agent.term.lock().expect("agent terminal");
            // It has work now, whatever the screen still shows.
            term.last_output = Instant::now();
            term.parser.screen().bracketed_paste()
        };
        if pasted {
            agent.write(format!("\x1b[200~{text}\x1b[201~").as_bytes())?;
        } else {
            agent.write(text.as_bytes())?;
        }
        self.refresh();
        std::thread::spawn(move || {
            std::thread::sleep(SUBMIT_AFTER);
            let _ = agent.write(b"\r");
        });
        Ok(())
    }

    /// Raw terminal input, as an attached frontend types it.
    pub fn write(&self, id: u64, bytes: &[u8]) -> Result<()> {
        self.live_agent(id)?.write(bytes)
    }

    /// Pasted text, bracketed when the agent asked for that.
    pub fn paste(&self, id: u64, text: &str) -> Result<()> {
        let agent = self.live_agent(id)?;
        let bracketed = agent
            .term
            .lock()
            .expect("agent terminal")
            .parser
            .screen()
            .bracketed_paste();
        if bracketed {
            agent.write(format!("\x1b[200~{text}\x1b[201~").as_bytes())
        } else {
            agent.write(text.as_bytes())
        }
    }

    /// Fit the agent's terminal to the area a frontend draws it in.
    pub fn resize(&self, id: u64, rows: u16, cols: u16) {
        let Ok(agent) = self.agent(id) else { return };
        if rows == 0 || cols == 0 {
            return;
        }
        let mut term = agent.term.lock().expect("agent terminal");
        if term.size == (rows, cols) {
            return;
        }
        term.size = (rows, cols);
        term.parser.screen_mut().set_size(cells(rows), cells(cols));
        let _ = agent.master.lock().expect("agent pty").resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Look `rows` back into the agent's scrollback; 0 follows its output.
    pub fn scroll(&self, id: u64, rows: usize) {
        if let Ok(agent) = self.agent(id) {
            agent
                .term
                .lock()
                .expect("agent terminal")
                .parser
                .screen_mut()
                .set_scrollback(rows);
        }
    }

    /// Borrow the agent's screen, to draw it.
    pub fn with_screen<R>(&self, id: u64, draw: impl FnOnce(&Screen) -> R) -> Option<R> {
        let agent = self.agent(id).ok()?;
        let term = agent.term.lock().expect("agent terminal");
        Some(draw(term.parser.screen()))
    }

    /// The agent's state and screen as text, for the model.
    pub fn read(&self, session_id: &str, id: u64) -> Result<String> {
        let agent = self.owned(session_id, id)?;
        Ok(describe(&agent.info(id), &agent.screen_text()))
    }

    /// An agent of this conversation: the model reaches no others.
    fn owned(&self, session_id: &str, id: u64) -> Result<Arc<Agent>> {
        self.agent(id)
            .ok()
            .filter(|agent| agent.session_id == session_id)
            .with_context(|| format!("no fleet agent {id} in this conversation"))
    }

    /// Steering from the model: its own conversation's agents, and never an
    /// answer to a permission prompt — that decision is the user's.
    pub fn send_from_model(&self, session_id: &str, id: u64, text: &str) -> Result<String> {
        let agent = self.owned(session_id, id)?;
        if agent.current() == State::Blocked {
            bail!(
                "fleet agent {id} is blocked on a permission only the user can give · they answer it in /fleet"
            );
        }
        self.send(id, text)?;
        Ok(format!(
            "sent to {} {id} · fleet action=wait id={id} for its answer",
            agent.label
        ))
    }

    /// Stop one of this conversation's agents, for the model.
    pub fn stop_from_model(&self, session_id: &str, id: u64) -> Result<String> {
        self.owned(session_id, id)?;
        self.stop(id)
    }

    /// This conversation's agents, one per line, for the model.
    #[must_use]
    pub fn list_text(&self, session_id: &str) -> String {
        let lines: Vec<String> = self
            .list(Some(session_id))
            .iter()
            .map(|agent| {
                let mut line = vec![
                    agent.id.to_string(),
                    agent.label.clone(),
                    agent.state.label(),
                    agent.title.clone(),
                ];
                if let (Some(branch), Some(changes)) = (&agent.branch, agent.changes) {
                    line.extend([branch.clone(), changes.label()]);
                }
                if let Some(check) = &agent.check {
                    line.push(check.label());
                }
                line.join(" · ")
            })
            .collect();
        if lines.is_empty() {
            "no fleet agents".into()
        } else {
            lines.join("\n")
        }
    }

    /// Block until agent `id` — or, without one, every agent of the
    /// conversation — has finished its turn, then return their screens. A
    /// blocked agent keeps it waiting: the user answers it in /fleet meanwhile.
    pub async fn wait(&self, session_id: &str, id: Option<u64>, seconds: u64) -> String {
        let seconds = seconds.clamp(1, MAX_WAIT_S);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        loop {
            let notified = self.changed.notified();
            let agents: Vec<AgentInfo> = self
                .list(Some(session_id))
                .into_iter()
                .filter(|agent| id.is_none_or(|id| agent.id == id))
                .collect();
            if agents.is_empty() {
                return match id {
                    Some(id) => format!("no fleet agent {id} in this conversation"),
                    None => "no fleet agents to wait for".into(),
                };
            }
            let pending = agents
                .iter()
                .any(|agent| matches!(agent.state, State::Working | State::Blocked));
            if !pending || tokio::time::Instant::now() >= deadline {
                return agents
                    .iter()
                    .map(|agent| {
                        if agent.state == State::Working {
                            format!(
                                "{} is still working after {seconds}s · wait again",
                                agent.name()
                            )
                        } else {
                            let screen = self
                                .agent(agent.id)
                                .map(|agent| agent.screen_text())
                                .unwrap_or_default();
                            describe(agent, &screen)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
            }
            let _ = tokio::time::timeout_at(deadline, notified).await;
        }
    }

    /// Stop a running agent, or clear one that already exited.
    pub fn stop(&self, id: u64) -> Result<String> {
        let agent = self
            .agents
            .lock()
            .expect("fleet")
            .remove(&id)
            .with_context(|| format!("no fleet agent {id}"))?;
        agent.kill();
        self.changed.notify_waiters();
        let kept = match agent
            .worktree
            .as_ref()
            .map(|tree| tree.remove(&agent.commit_message()))
        {
            Some(Ok(Some(branch))) => format!(" · its unmerged work is on branch {branch}"),
            Some(Err(error)) => format!(" · its worktree could not be removed: {error:#}"),
            Some(Ok(None)) | None => String::new(),
        };
        Ok(format!("stopped {} {id}{kept}", agent.label))
    }

    /// The agent's changes in its worktree, as a patch to read.
    pub fn diff(&self, id: u64) -> Result<String> {
        let agent = self.agent(id)?;
        let tree = agent.worktree.as_ref().with_context(|| {
            format!("fleet agent {id} works directly in your tree · there is no separate diff")
        })?;
        let diff = tree.diff()?;
        Ok(if diff.trim().is_empty() {
            "no changes yet".into()
        } else {
            diff
        })
    }

    /// The patch of one of this conversation's agents, for the model.
    pub fn diff_from_model(&self, session_id: &str, id: u64) -> Result<String> {
        self.owned(session_id, id)?;
        Ok(crate::tools::cap_head_tail(&self.diff(id)?, DIFF_CAP))
    }

    /// Run the project's check in the agent's worktree, in the background;
    /// the result arrives as a [`FleetMsg`] with `checked` set.
    pub fn check(self: &Arc<Self>, id: u64) -> Result<String> {
        let agent = self.agent(id)?;
        let name = format!("{} {id}", agent.label);
        let tree = agent.worktree.as_ref().with_context(|| {
            format!("{name} works directly in your tree · run the checks there yourself")
        })?;
        if agent.current() == State::Working {
            bail!("{name} is still working · check when its turn ends");
        }
        let command = check::command(tree.path()).context(
            "no check found for this project · looked for scripts/check.sh, make check or test, npm test, cargo, go and pytest",
        )?;
        {
            let mut slot = agent.check.lock().expect("agent check");
            if matches!(*slot, Some(Check::Running { .. })) {
                return Ok(format!("{name} is already being checked"));
            }
            *slot = Some(Check::Running {
                command: command.clone(),
                started: Instant::now(),
                pid: None,
            });
        }
        let dir = tree.path().to_path_buf();
        let target = check::target_dir(tree.root());
        let fleet = Arc::downgrade(self);
        let checked = Arc::clone(&agent);
        std::thread::spawn(move || {
            let done = check::run(&command, &dir, &target, |pid| {
                if let Some(Check::Running { pid: slot, .. }) =
                    checked.check.lock().expect("agent check").as_mut()
                {
                    *slot = Some(pid);
                }
            });
            *checked.check.lock().expect("agent check") = Some(done.clone());
            let Some(fleet) = fleet.upgrade() else { return };
            // A stopped agent's check has no one left to tell.
            if fleet.agent(id).is_ok() {
                let state = checked.state.lock().expect("agent state").0;
                let _ = fleet.events.send(FleetMsg {
                    agent: checked.info(id),
                    previous: state,
                    lasted: Duration::ZERO,
                    checked: Some(done),
                });
            }
            fleet.changed.notify_waiters();
        });
        self.changed.notify_waiters();
        Ok(format!("checking {name}: {}", agent.check_command()))
    }

    /// Check one of this conversation's agents and wait for the verdict, for
    /// the model.
    pub async fn check_from_model(
        self: &Arc<Self>,
        session_id: &str,
        id: u64,
        seconds: u64,
    ) -> Result<String> {
        self.owned(session_id, id)?;
        self.check(id)?;
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(seconds.clamp(1, MAX_WAIT_S));
        loop {
            let notified = self.changed.notified();
            let agent = self.owned(session_id, id)?;
            let check = agent.check.lock().expect("agent check").clone();
            match check {
                Some(check @ Check::Done { .. }) => {
                    return Ok(format!("{} {id} {}", agent.label, check.report()));
                }
                Some(check) if tokio::time::Instant::now() >= deadline => {
                    return Ok(format!(
                        "{} {id} is still {} · fleet action=check id={id} again waits for the same run",
                        agent.label,
                        check.label()
                    ));
                }
                None => bail!(
                    "{} {id} started working again before its check finished",
                    agent.label
                ),
                Some(_) => {
                    let _ = tokio::time::timeout_at(deadline, notified).await;
                }
            }
        }
    }

    /// Bring the agent's changes into the user's checkout, staged and
    /// uncommitted. It keeps its worktree and can go on working.
    pub fn merge(&self, id: u64) -> Result<String> {
        let agent = self.agent(id)?;
        let tree = agent.worktree.as_ref().with_context(|| {
            format!("fleet agent {id} works directly in your tree · nothing to merge")
        })?;
        if agent.current() == State::Working {
            bail!("fleet agent {id} is still working · merge when its turn ends");
        }
        let (merged, conflicts) = tree.merge(&agent.commit_message())?;
        agent.measure();
        Ok(if merged.files == 0 {
            format!("{} {id} has nothing new to merge", agent.label)
        } else if conflicts.is_empty() {
            format!(
                "merged {} {id}: {} staged in your tree · review and commit",
                agent.label,
                merged.label()
            )
        } else {
            format!(
                "merged {} {id}: {} · conflicts with your commits in {} · resolve the markers, then commit",
                agent.label,
                merged.label(),
                conflicts.join(", ")
            )
        })
    }

    /// Stop every agent; the app is going away.
    pub fn shutdown(&self) {
        let agents = std::mem::take(&mut *self.agents.lock().expect("fleet"));
        for agent in agents.values() {
            agent.kill();
            // Unmerged work is committed to the agent's branch before its checkout goes.
            if let Some(tree) = &agent.worktree {
                let _ = tree.remove(&agent.commit_message());
            }
        }
        self.changed.notify_waiters();
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The agent's process, set up to run in `cwd`: on its own worktree it
/// works without asking and builds into the shared target.
fn command<'a>(
    launch: &'a Launch,
    cwd: &std::path::Path,
    worktree: Option<&Worktree>,
) -> Result<(&'a String, CommandBuilder)> {
    let (program, args) = launch.argv.split_first().context("empty agent command")?;
    let mut command = CommandBuilder::new(program);
    if worktree.is_some() {
        command.args(&launch.auto);
    }
    command.args(args);
    command.cwd(cwd);
    command.env("TERM", "xterm-256color");
    // This terminal is ours, whatever multiplexer Dugix itself runs in.
    command.env_remove("TMUX");
    command.env_remove("TMUX_PANE");
    // Worktrees share one build directory instead of each growing its own.
    if let Some(tree) = worktree
        && std::env::var_os("CARGO_TARGET_DIR").is_none()
    {
        command.env("CARGO_TARGET_DIR", check::target_dir(tree.root()));
    }
    Ok((program, command))
}

/// A terminal dimension; a zero becomes one cell.
fn cells(count: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(count).unwrap_or(std::num::NonZeroU16::MIN)
}

/// An agent's state line and screen, as the model reads them.
fn describe(agent: &AgentInfo, screen: &str) -> String {
    let note = match agent.state {
        State::Blocked => " · waiting on the user's permission; they answer it in /fleet",
        State::Idle => " · its turn ended; send a follow-up or stop it",
        State::Working | State::Exited(_) => "",
    };
    let work = match (&agent.branch, agent.changes) {
        (Some(branch), Some(changes)) => format!(
            "\nbranch {branch} · {} · the user reviews and merges it in /fleet",
            changes.label()
        ),
        _ => String::new(),
    };
    let work = match &agent.check {
        Some(check) => format!("{work} · {}", check.label()),
        None => work,
    };
    format!(
        "{} ({}) · {}{note}{work}\n{}",
        agent.name(),
        agent.title,
        agent.state.label(),
        crate::tools::cap_head_tail(screen, READ_CAP)
    )
}
