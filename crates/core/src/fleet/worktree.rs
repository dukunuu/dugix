//! An agent's own checkout: a git worktree on its own branch inside the
//! repository, so several agents can edit at once without touching each other
//! or the user's tree. The user reads the diff and merges it when it is good.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context as _, Result, bail, ensure};

use crate::jobs::service::git;

/// Where worktrees live inside a repository; kept out of `git status` through
/// the repository's local exclude file.
const DIR: &str = ".dugix/worktrees";

/// What an agent has changed against where it started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub files: usize,
    pub added: u64,
    pub deleted: u64,
}

impl Changes {
    /// `3 files +40 −12`, or `no changes`.
    #[must_use]
    pub fn label(self) -> String {
        if self.files == 0 {
            return "no changes".into();
        }
        format!(
            "{} file{} +{} −{}",
            self.files,
            if self.files == 1 { "" } else { "s" },
            self.added,
            self.deleted
        )
    }
}

pub(super) struct Worktree {
    /// The user's checkout the worktree was made from.
    root: PathBuf,
    path: PathBuf,
    pub branch: String,
    /// The commit the next merge is taken against: where the agent started,
    /// then each merged state.
    base: Mutex<String>,
}

fn text(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git(root, args)?.stdout)
        .trim()
        .to_string())
}

/// Whether `dir` is inside a git checkout an agent can branch from.
pub(super) fn is_repo(dir: &Path) -> bool {
    text(dir, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|inside| inside == "true")
        && text(dir, &["rev-parse", "--verify", "HEAD"]).is_ok()
}

fn exclude(root: &Path) -> Result<()> {
    let common = PathBuf::from(text(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let file = common.join("info/exclude");
    let current = std::fs::read_to_string(&file).unwrap_or_default();
    if current.lines().any(|line| line.trim() == ".dugix/") {
        return Ok(());
    }
    std::fs::create_dir_all(common.join("info"))?;
    let mut lines = current;
    if !lines.is_empty() && !lines.ends_with('\n') {
        lines.push('\n');
    }
    lines.push_str(".dugix/\n");
    std::fs::write(file, lines)?;
    Ok(())
}

impl Worktree {
    /// A new worktree named `name`, branched from the HEAD of the checkout
    /// holding `dir`. Returns it with the directory inside it that matches `dir`.
    pub(super) fn create(dir: &Path, name: &str) -> Result<(Self, PathBuf)> {
        let root = PathBuf::from(text(dir, &["rev-parse", "--show-toplevel"])?);
        let prefix = text(dir, &["rev-parse", "--show-prefix"])?;
        let base = text(&root, &["rev-parse", "HEAD"])?;
        exclude(&root)?;
        let path = root.join(DIR).join(name);
        let branch = format!("dugix/fleet/{name}");
        git(
            &root,
            &[
                "worktree",
                "add",
                path.to_str().context("non-UTF-8 worktree path")?,
                "-b",
                &branch,
                "HEAD",
            ],
        )?;
        let cwd = path.join(prefix);
        Ok((
            Self {
                root,
                path,
                branch,
                base: Mutex::new(base),
            },
            cwd,
        ))
    }

    /// The user's checkout the worktree was made from.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// The worktree's own top directory.
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn base(&self) -> String {
        self.base.lock().expect("worktree base").clone()
    }

    fn untracked(&self) -> Result<Vec<String>> {
        Ok(
            text(&self.path, &["ls-files", "--others", "--exclude-standard"])?
                .lines()
                .map(str::to_string)
                .collect(),
        )
    }

    /// What the agent changed since its base, without touching its index.
    pub(super) fn changes(&self) -> Result<Changes> {
        let mut changes = Changes::default();
        for line in text(&self.path, &["diff", &self.base(), "--numstat"])?.lines() {
            let mut fields = line.split('\t');
            changes.added += fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            changes.deleted += fields.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            changes.files += 1;
        }
        for file in self.untracked()? {
            changes.files += 1;
            changes.added += std::fs::read_to_string(self.path.join(file))
                .map_or(0, |text| text.lines().count() as u64);
        }
        Ok(changes)
    }

    /// The agent's changes as a patch to read, new files included.
    pub(super) fn diff(&self) -> Result<String> {
        let mut diff = text(&self.path, &["diff", &self.base(), "--stat", "--patch"])?;
        for file in self.untracked()? {
            // `--no-index` exits 1 when the files differ, which is every time.
            let output = Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .args(["diff", "--no-index", "--", "/dev/null", &file])
                .output()?;
            if !diff.is_empty() {
                diff.push('\n');
            }
            diff.push_str(String::from_utf8_lossy(&output.stdout).trim_end());
        }
        Ok(diff)
    }

    /// Commit whatever the agent left uncommitted, on its own branch.
    fn commit(&self, message: &str) -> Result<()> {
        git(&self.path, &["add", "-A"])?;
        let staged = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["diff", "--cached", "--quiet"])
            .status()?;
        ensure!(matches!(staged.code(), Some(0 | 1)), "git diff failed");
        if !staged.success() {
            // The branch is reviewed before it lands; hooks and signing prompts
            // must not stop it from being recorded.
            git(
                &self.path,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "user.name=dugix fleet",
                    "-c",
                    "user.email=fleet@dugix.invalid",
                    "commit",
                    "--no-verify",
                    "-m",
                    message,
                ],
            )?;
        }
        Ok(())
    }

    /// Bring the agent's changes since the last merge into the user's
    /// checkout, staged and uncommitted, for them to review and commit.
    /// Returns them with the files left with conflict markers, where the
    /// user's own commits changed the same lines.
    pub(super) fn merge(&self, message: &str) -> Result<(Changes, Vec<String>)> {
        let changes = self.changes()?;
        self.commit(message)?;
        let head = text(&self.path, &["rev-parse", "HEAD"])?;
        let base = self.base();
        if head == base {
            return Ok((Changes::default(), Vec::new()));
        }
        let patch = git(&self.path, &["diff", "--binary", &base, &head])?.stdout;
        let mut apply = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["apply", "--3way", "--whitespace=nowarn", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        apply
            .stdin
            .take()
            .context("git apply input")?
            .write_all(&patch)?;
        let output = apply.wait_with_output()?;
        let errors = String::from_utf8_lossy(&output.stderr);
        // A three-way apply that conflicts still lands, markers and all.
        let landed = output.status.success() || errors.contains("with conflicts");
        if !landed {
            let overlapping: Vec<&str> = errors
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("error: ")?
                        .strip_suffix(": does not match index")
                })
                .collect();
            if overlapping.is_empty() {
                bail!("the changes did not apply to your tree: {}", errors.trim());
            }
            bail!(
                "your uncommitted edits touch the same files ({}) · commit or stash them, then merge again",
                overlapping.join(", ")
            );
        }
        *self.base.lock().expect("worktree base") = head;
        let conflicts = text(&self.root, &["diff", "--name-only", "--diff-filter=U"])?
            .lines()
            .map(str::to_string)
            .collect();
        Ok((changes, conflicts))
    }

    /// Remove the checkout. Unmerged work is committed and stays on the
    /// branch, which is then returned; an agent that left nothing behind
    /// loses its branch too.
    pub(super) fn remove(&self, message: &str) -> Result<Option<String>> {
        let pending = self.changes().is_ok_and(|changes| changes.files > 0);
        if pending {
            self.commit(message)?;
        }
        git(
            &self.root,
            &[
                "worktree",
                "remove",
                "--force",
                self.path.to_str().context("non-UTF-8 worktree path")?,
            ],
        )?;
        if pending {
            return Ok(Some(self.branch.clone()));
        }
        // Merged or untouched: nothing on the branch the user's tree lacks.
        let _ = git(&self.root, &["branch", "-D", &self.branch]);
        Ok(None)
    }
}
