//! The project's own check, run in an agent's worktree: what tells the user
//! and the model whether an agent's work is ready to merge.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Longest a check may run before it is stopped and counted as failed.
const LIMIT: Duration = Duration::from_mins(30);
/// Output kept from a finished check: its end, where failures are reported.
const TAIL_LINES: usize = 80;
const TAIL_CHARS: usize = 8_000;

/// A check of one agent's work: running, or finished with its output's end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Running {
        command: String,
        started: Instant,
        /// Its process group, once started, so stopping the agent stops it.
        pid: Option<u32>,
    },
    Done {
        command: String,
        took: Duration,
        /// `None` when it was stopped or could not start.
        code: Option<i32>,
        tail: String,
    },
}

impl Check {
    /// Whether it passed; `None` while it runs.
    #[must_use]
    pub fn passed(&self) -> Option<bool> {
        match self {
            Self::Running { .. } => None,
            Self::Done { code, .. } => Some(*code == Some(0)),
        }
    }

    #[must_use]
    pub fn command(&self) -> &str {
        match self {
            Self::Running { command, .. } | Self::Done { command, .. } => command,
        }
    }

    /// `checking 1m02s`, `check passed in 3m10s` or `check failed (exit 101)`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Running { started, .. } => format!("checking {}", clock(started.elapsed())),
            Self::Done {
                took,
                code: Some(0),
                ..
            } => format!("check passed in {}", clock(*took)),
            Self::Done {
                code: Some(code), ..
            } => format!("check failed (exit {code})"),
            Self::Done { code: None, .. } => "check did not finish".into(),
        }
    }

    /// The result and the end of its output, for the model.
    #[must_use]
    pub fn report(&self) -> String {
        match self {
            Self::Running { .. } => format!("{} · `{}`", self.label(), self.command()),
            Self::Done { tail, .. } => {
                format!("{} · `{}`\n{tail}", self.label(), self.command())
            }
        }
    }
}

fn clock(span: Duration) -> String {
    let seconds = span.as_secs();
    format!("{}m{:02}s", seconds / 60, seconds % 60)
}

/// The command that checks the project at `root`, by its own conventions.
pub(super) fn command(root: &Path) -> Option<String> {
    let file = |name: &str| std::fs::read_to_string(root.join(name)).ok();
    if root.join("scripts/check.sh").is_file() {
        return Some("sh scripts/check.sh".into());
    }
    if let Some(makefile) = file("Makefile") {
        for target in ["check", "test"] {
            if makefile
                .lines()
                .any(|line| line.starts_with(&format!("{target}:")))
            {
                return Some(format!("make {target}"));
            }
        }
    }
    if let Some(package) = file("package.json")
        && serde_json::from_str::<serde_json::Value>(&package).is_ok_and(|package| {
            package["scripts"]["test"]
                .as_str()
                .is_some_and(|test| !test.contains("no test specified"))
        })
    {
        return Some("npm test".into());
    }
    if root.join("Cargo.toml").is_file() {
        return Some("cargo test --workspace".into());
    }
    if root.join("go.mod").is_file() {
        return Some("go test ./...".into());
    }
    if ["pyproject.toml", "pytest.ini", "setup.cfg", "tox.ini"]
        .iter()
        .any(|name| root.join(name).is_file())
    {
        return Some("python3 -m pytest -q".into());
    }
    None
}

/// Where cargo builds for the fleet: one directory per repository, shared by
/// every agent and check, so each worktree does not grow its own target.
pub(super) fn target_dir(root: &Path) -> PathBuf {
    root.join(".dugix/target")
}

/// Run `command` in `dir` to the end, or to [`LIMIT`]. `started` hears its
/// process group as soon as it runs.
pub(super) fn run(command: &str, dir: &Path, target: &Path, started: impl FnOnce(u32)) -> Check {
    let begun = Instant::now();
    let done = |code, tail| Check::Done {
        command: command.to_string(),
        took: begun.elapsed(),
        code,
        tail,
    };
    let mut shell = Command::new("sh");
    shell
        .arg("-c")
        .arg(format!("exec 2>&1; {command}"))
        .current_dir(dir)
        .env("CARGO_TERM_COLOR", "never")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if std::env::var_os("CARGO_TARGET_DIR").is_none() {
        shell.env("CARGO_TARGET_DIR", target);
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut shell, 0);
    let mut child = match shell.spawn() {
        Ok(child) => child,
        Err(error) => return done(None, format!("could not start the check: {error}")),
    };
    started(child.id());
    let mut stdout = child.stdout.take().expect("piped output");
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.read_to_end(&mut output);
        output
    });
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if begun.elapsed() < LIMIT => std::thread::sleep(Duration::from_millis(200)),
            Ok(None) | Err(_) => {
                stop(child.id());
                let _ = child.wait();
                break None;
            }
        }
    };
    let output = reader.join().unwrap_or_default();
    let mut tail = tail(&String::from_utf8_lossy(&output));
    if code.is_none() && begun.elapsed() >= LIMIT {
        tail = format!("{tail}\n(stopped after {})", clock(LIMIT));
    }
    done(code, tail)
}

/// Stop a running check and everything it started.
pub(super) fn stop(pid: u32) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: signals the process group the check leads; no memory is shared.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// The end of a check's output, where the verdict and failures are.
fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.trim_end().lines().collect();
    let kept = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    match kept.char_indices().rev().nth(TAIL_CHARS) {
        Some((cut, _)) => format!("…{}", &kept[cut..]),
        None => kept,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dugix-check-{}", uuid::Uuid::new_v4()));
        for (name, body) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn the_check_follows_the_project_conventions() {
        let cases: &[(&[(&str, &str)], Option<&str>)] = &[
            (
                &[("scripts/check.sh", ""), ("Cargo.toml", "")],
                Some("sh scripts/check.sh"),
            ),
            (
                &[("Makefile", "build:\n\ntest:\n\tgo test\n")],
                Some("make test"),
            ),
            (
                &[("package.json", r#"{"scripts":{"test":"vitest run"}}"#)],
                Some("npm test"),
            ),
            (
                &[
                    (
                        "package.json",
                        r#"{"scripts":{"test":"echo \"Error: no test specified\""}}"#,
                    ),
                    ("pyproject.toml", ""),
                ],
                Some("python3 -m pytest -q"),
            ),
            (&[("Cargo.toml", "")], Some("cargo test --workspace")),
            (&[("README.md", "")], None),
        ];
        for (files, expected) in cases {
            let root = dir(files);
            assert_eq!(command(&root).as_deref(), *expected, "{files:?}");
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_check_keeps_its_verdict_and_the_end_of_its_output() {
        let root = dir(&[]);
        let target = root.join("target");
        let mut pid = None;
        let passed = run("echo ok; echo warn >&2", &root, &target, |id| {
            pid = Some(id)
        });
        assert!(pid.is_some());
        assert_eq!(passed.passed(), Some(true));
        assert!(
            matches!(&passed, Check::Done { tail, .. } if tail == "ok\nwarn"),
            "{passed:?}"
        );
        assert!(passed.label().starts_with("check passed in 0m0"));
        let failed = run(
            "seq 1 200; echo \"target=$CARGO_TARGET_DIR\"; exit 3",
            &root,
            &target,
            |_| {},
        );
        assert_eq!(failed.label(), "check failed (exit 3)");
        let Check::Done { tail, .. } = &failed else {
            panic!("{failed:?}")
        };
        assert_eq!(tail.lines().count(), TAIL_LINES);
        if std::env::var_os("CARGO_TARGET_DIR").is_none() {
            assert!(
                tail.ends_with(&format!("target={}", target.display())),
                "{tail}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
