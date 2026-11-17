use std::time::Duration;

use super::*;

/// A stand-in agent: a shell script in a real pseudo-terminal.
fn script(body: &str) -> Launch {
    Launch {
        label: "sh".into(),
        title: "scripted agent".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), body.into()],
        cwd: std::env::temp_dir(),
        isolate: false,
        auto: Vec::new(),
    }
}

async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn state_is(fleet: &Fleet, id: u64, state: State) {
    until(&format!("{state:?}"), || {
        fleet.info(id).is_some_and(|agent| agent.state == state)
    })
    .await;
}

#[test]
fn each_kind_opens_its_cli_on_the_prompt() {
    assert_eq!(Kind::Claude.argv("fix it", None), ["claude", "fix it"]);
    assert_eq!(
        Kind::OpenCode.argv("fix it", None),
        ["opencode", "--prompt", "fix it"]
    );
    assert_eq!(Kind::Pi.argv("", None), ["pi"]);
    for (kind, name) in Kind::ALL.into_iter().zip(Kind::NAMES) {
        assert_eq!(kind.name(), *name);
        assert_eq!(Kind::parse(name), Some(kind));
    }
    assert_eq!(Kind::parse("vim"), None);
}

#[tokio::test]
async fn an_agent_goes_idle_takes_a_follow_up_and_exits() {
    let (fleet, mut events) = Fleet::new();
    let id = fleet
        .spawn(
            "s1",
            script("echo ready; read line; echo \"got $line\"; read gone"),
        )
        .unwrap();
    assert_eq!(fleet.info(id).unwrap().state, State::Working);
    state_is(&fleet, id, State::Idle).await;
    let moved = events.recv().await.unwrap();
    assert_eq!(
        (moved.previous, moved.agent.state),
        (State::Working, State::Idle)
    );
    assert_eq!(fleet.info(id).unwrap().last_line, "ready");

    fleet.send(id, "hello fleet").unwrap();
    assert_eq!(
        fleet.info(id).unwrap().state,
        State::Working,
        "a steered agent has work at once"
    );
    until("the follow-up", || {
        fleet.read("s1", id).unwrap().contains("got hello fleet")
    })
    .await;
    let report = fleet.wait("s1", Some(id), 30).await;
    assert!(
        report.starts_with("sh 1 (scripted agent) · idle"),
        "{report}"
    );
    assert!(report.contains("got hello fleet"), "{report}");

    fleet.write(id, b"bye\r").unwrap();
    state_is(&fleet, id, State::Exited(0)).await;
    assert!(fleet.send(id, "too late").is_err());
    assert_eq!(fleet.live(), 0);
    assert_eq!(fleet.stop(id).unwrap(), "stopped sh 1");
    assert!(fleet.list(None).is_empty());
}

#[tokio::test]
async fn a_permission_prompt_blocks_until_the_user_answers() {
    let (fleet, _events) = Fleet::new();
    let id = fleet
        .spawn(
            "s1",
            script("echo 'Do you want to proceed?'; echo '> 1. Yes'; read answer; echo \"answered $answer\"; read gone"),
        )
        .unwrap();
    state_is(&fleet, id, State::Blocked).await;
    assert_eq!(fleet.blocked(), 1);
    let refused = fleet
        .send_from_model("s1", id, "1")
        .unwrap_err()
        .to_string();
    assert!(refused.contains("only the user can give"), "{refused}");
    // Waiting holds while it is blocked, until the time runs out.
    let report = fleet.wait("s1", None, 1).await;
    assert!(
        report.contains("· blocked · waiting on the user's permission"),
        "{report}"
    );

    // The user, attached, answers it.
    fleet.write(id, b"1\r").unwrap();
    until("the answer", || {
        fleet.read("s1", id).unwrap().contains("answered 1")
    })
    .await;
    fleet.shutdown();
}

#[tokio::test]
async fn the_model_reaches_only_its_own_conversation() {
    let (fleet, _events) = Fleet::new();
    let id = fleet.spawn("s1", script("echo hi; read gone")).unwrap();
    assert!(fleet.read("s2", id).is_err());
    assert!(fleet.send_from_model("s2", id, "x").is_err());
    assert!(fleet.stop_from_model("s2", id).is_err());
    assert_eq!(fleet.list_text("s2"), "no fleet agents");
    assert_eq!(
        fleet.wait("s2", None, 1).await,
        "no fleet agents to wait for"
    );
    assert!(fleet.list_text("s1").starts_with("1 · sh · "));
    assert_eq!(fleet.stop_from_model("s1", id).unwrap(), "stopped sh 1");
}

#[tokio::test]
async fn the_screen_follows_a_resize_and_answers_cursor_queries() {
    let (fleet, _events) = Fleet::new();
    // A CLI that asks where the cursor is and prints the terminal's answer.
    let id = fleet
        .spawn(
            "s1",
            script("stty raw -echo; printf 'ab\\033[6n'; dd bs=1 count=6 2>/dev/null | od -c | head -1; stty sane; read gone"),
        )
        .unwrap();
    until("the cursor report", || {
        fleet.read("s1", id).unwrap().contains("1   ;   3   R")
    })
    .await;
    fleet.resize(id, 12, 40);
    assert_eq!(
        fleet
            .with_screen(id, |screen| {
                let (rows, cols) = screen.size();
                (rows.get(), cols.get())
            })
            .unwrap(),
        (12, 40)
    );
    fleet.shutdown();
}

#[tokio::test]
async fn a_missing_program_says_so_and_the_fleet_has_a_ceiling() {
    let (fleet, _events) = Fleet::new();
    let mut missing = script("");
    missing.argv = vec!["dugix-no-such-agent-cli".into()];
    let error = format!("{:#}", fleet.spawn("s1", missing).unwrap_err());
    assert!(error.contains("is it installed and on PATH?"), "{error}");
    for _ in 0..MAX_LIVE {
        fleet.spawn("s1", script("read gone")).unwrap();
    }
    let error = fleet.spawn("s1", script("read gone")).unwrap_err();
    assert!(error.to_string().contains("already running"), "{error}");
    fleet.shutdown();
}

/// A git repository with one commit, removed when dropped.
struct Repo(std::path::PathBuf);

impl Repo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("dugix-fleet-repo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // The worktree's reported paths are canonical (/private/var on macOS).
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("calc.py"), "def add(a, b):\n    return a - b\n").unwrap();
        let repo = Self(root);
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["add", "-A"]);
        repo.git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// An agent that edits one file, adds another, then waits.
    fn editing_agent(&self) -> Launch {
        let mut launch = script(
            "printf 'def add(a, b):\\n    return a + b\\n' > calc.py; echo notes > NOTES.md; echo done; read gone",
        );
        launch.cwd = self.0.clone();
        launch.isolate = true;
        launch
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn a_worktree_agent_edits_on_its_own_branch_until_merged() {
    let repo = Repo::new();
    let (fleet, _events) = Fleet::new();
    let id = fleet.spawn("s1", repo.editing_agent()).unwrap();
    let branch = fleet.info(id).unwrap().branch.expect("its own branch");
    assert!(branch.starts_with("dugix/fleet/sh-1-"), "{branch}");
    state_is(&fleet, id, State::Idle).await;
    until("its changes to be measured", || {
        fleet
            .info(id)
            .unwrap()
            .changes
            .is_some_and(|c| c.files == 2)
    })
    .await;
    assert_eq!(
        fleet.info(id).unwrap().changes.unwrap().label(),
        "2 files +2 −1"
    );

    // The user's tree is untouched, and the worktree is hidden from status.
    assert_eq!(
        std::fs::read_to_string(repo.0.join("calc.py")).unwrap(),
        "def add(a, b):\n    return a - b\n"
    );
    assert_eq!(repo.git(&["status", "--porcelain"]), "");
    let diff = fleet.diff(id).unwrap();
    assert!(
        diff.contains("-    return a - b") && diff.contains("+    return a + b"),
        "{diff}"
    );
    assert!(
        diff.contains("NOTES.md") && diff.contains("+notes"),
        "{diff}"
    );
    assert!(
        fleet
            .read("s1", id)
            .unwrap()
            .contains(&format!("branch {branch} · 2 files +2 −1")),
        "the model sees the branch"
    );

    assert_eq!(
        fleet.merge(id).unwrap(),
        "merged sh 1: 2 files +2 −1 staged in your tree · review and commit"
    );
    assert_eq!(
        std::fs::read_to_string(repo.0.join("calc.py")).unwrap(),
        "def add(a, b):\n    return a + b\n"
    );
    assert_eq!(
        repo.git(&["status", "--porcelain"]),
        "A  NOTES.md\nM  calc.py\n"
    );
    assert_eq!(fleet.merge(id).unwrap(), "sh 1 has nothing new to merge");
    assert_eq!(
        fleet.info(id).unwrap().changes.unwrap().label(),
        "no changes"
    );

    // Merged and stopped: neither the checkout nor the branch is left behind.
    assert_eq!(fleet.stop(id).unwrap(), "stopped sh 1");
    assert_eq!(repo.git(&["branch", "--list", "dugix/fleet/*"]), "");
    assert!(
        !repo
            .0
            .join(".dugix/worktrees")
            .read_dir()
            .unwrap()
            .any(|_| true)
    );
}

#[tokio::test]
async fn stopping_an_unmerged_agent_keeps_its_work_on_the_branch() {
    let repo = Repo::new();
    let (fleet, _events) = Fleet::new();
    let id = fleet.spawn("s1", repo.editing_agent()).unwrap();
    let branch = fleet.info(id).unwrap().branch.unwrap();
    state_is(&fleet, id, State::Idle).await;
    assert_eq!(
        fleet.stop(id).unwrap(),
        format!("stopped sh 1 · its unmerged work is on branch {branch}")
    );
    assert!(repo.git(&["show", "--stat", &branch]).contains("NOTES.md"));
    assert_eq!(repo.git(&["status", "--porcelain"]), "");

    // Outside a repository, or when asked not to, an agent works in place.
    let mut plain = script("echo hi; read gone");
    plain.isolate = true;
    let id = fleet.spawn("s1", plain).unwrap();
    assert_eq!(fleet.info(id).unwrap().branch, None);
    assert!(
        fleet
            .merge(id)
            .unwrap_err()
            .to_string()
            .contains("works directly in your tree")
    );
    fleet.shutdown();
}

#[tokio::test]
async fn a_check_runs_in_the_worktree_and_new_work_clears_it() {
    let repo = Repo::new();
    // The project's check passes only on the agent's fixed calc.py.
    std::fs::create_dir_all(repo.0.join("scripts")).unwrap();
    std::fs::write(
        repo.0.join("scripts/check.sh"),
        "grep -q 'a + b' calc.py && echo 'all green'\n",
    )
    .unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "check",
    ]);
    let (fleet, mut events) = Fleet::new();
    let mut launch = script(
        "printf 'def add(a, b):\\n    return a + b\\n' > calc.py; echo done; read again; echo working; sleep 1; read gone",
    );
    launch.cwd = repo.0.clone();
    launch.isolate = true;
    let id = fleet.spawn("s1", launch).unwrap();
    state_is(&fleet, id, State::Idle).await;

    let report = fleet.check_from_model("s1", id, 60).await.unwrap();
    assert!(report.starts_with("sh 1 check passed in"), "{report}");
    assert!(
        report.contains("`sh scripts/check.sh`\nall green"),
        "{report}"
    );
    let checked = loop {
        if let Some(checked) = events.recv().await.unwrap().checked {
            break checked;
        }
    };
    assert_eq!(checked.passed(), Some(true), "the user hears the verdict");
    assert!(fleet.list_text("s1").contains("· check passed in"));
    assert!(
        fleet.check_from_model("s2", id, 1).await.is_err(),
        "another conversation's agent"
    );

    fleet.send(id, "go").unwrap();
    until("new work to clear the check", || {
        fleet.info(id).unwrap().check.is_none()
    })
    .await;
    fleet.shutdown();
}

#[tokio::test]
async fn an_agent_in_your_tree_has_no_separate_check() {
    let (fleet, _events) = Fleet::new();
    let id = fleet.spawn("s1", script("echo hi; read gone")).unwrap();
    state_is(&fleet, id, State::Idle).await;
    assert!(
        fleet
            .check(id)
            .unwrap_err()
            .to_string()
            .contains("works directly in your tree")
    );
    fleet.shutdown();
}

#[tokio::test]
async fn auto_flags_apply_only_on_its_own_worktree() {
    let repo = Repo::new();
    let (fleet, _events) = Fleet::new();
    // `sh -e` shows up in `$-`, so the screen tells whether the flag came through.
    let launch = |isolate| {
        let mut launch = script("echo \"flags=$-\"; read gone");
        launch.cwd = repo.0.clone();
        launch.isolate = isolate;
        launch.auto = vec!["-e".into()];
        launch
    };
    let isolated = fleet.spawn("s1", launch(true)).unwrap();
    let in_place = fleet.spawn("s1", launch(false)).unwrap();
    state_is(&fleet, isolated, State::Idle).await;
    state_is(&fleet, in_place, State::Idle).await;
    let flags = |id| {
        let screen = fleet.read("s1", id).unwrap();
        screen
            .lines()
            .find_map(|line| line.trim().strip_prefix("flags=").map(str::to_string))
            .unwrap()
    };
    assert!(flags(isolated).contains('e'), "{}", flags(isolated));
    assert!(!flags(in_place).contains('e'), "{}", flags(in_place));
    assert_eq!(
        Kind::Claude.auto_flags(),
        ["--permission-mode", "auto"],
        "the CLI's own supervised mode, never a bypass"
    );
    assert!(Kind::ALL.iter().all(|kind| {
        !kind
            .auto_flags()
            .iter()
            .any(|flag| flag.contains("dangerous") || flag.contains("bypass"))
    }));
    fleet.shutdown();
}

#[tokio::test]
async fn waiting_holds_through_a_blocked_agent_until_the_user_answers() {
    let (fleet, _events) = Fleet::new();
    let id = fleet
        .spawn(
            "s1",
            // Like the CLIs, it redraws once answered, and the question goes.
            script(
                "echo 'Do you want to proceed? (y/n)'; read a; printf '\\033[2J\\033[H'; echo \"got $a\"; read b",
            ),
        )
        .unwrap();
    state_is(&fleet, id, State::Blocked).await;
    let fleet_ = Arc::clone(&fleet);
    let waiting = tokio::spawn(async move { fleet_.wait("s1", Some(id), 30).await });
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!waiting.is_finished(), "a blocked agent is not done");
    fleet.write(id, b"y\r").unwrap();
    let screens = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("returns once it is answered")
        .unwrap();
    assert!(
        screens.contains("got y") && screens.contains("idle"),
        "{screens}"
    );
    fleet.shutdown();
}

#[tokio::test]
async fn a_merge_reports_conflicts_and_refuses_over_uncommitted_edits() {
    let repo = Repo::new();
    let (fleet, _events) = Fleet::new();
    let id = fleet.spawn("s1", repo.editing_agent()).unwrap();
    state_is(&fleet, id, State::Idle).await;

    // Uncommitted edits to the same file: nothing is applied.
    std::fs::write(repo.0.join("calc.py"), "def add(a, b):\n    return b - a\n").unwrap();
    let refused = fleet.merge(id).unwrap_err().to_string();
    assert!(
        refused.contains("your uncommitted edits touch the same files (calc.py)"),
        "{refused}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.0.join("calc.py")).unwrap(),
        "def add(a, b):\n    return b - a\n"
    );

    // Committed: it lands with markers, and says where.
    repo.git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qam",
        "mine",
    ]);
    let merged = fleet.merge(id).unwrap();
    assert!(
        merged.contains("conflicts with your commits in calc.py · resolve the markers"),
        "{merged}"
    );
    assert!(
        std::fs::read_to_string(repo.0.join("calc.py"))
            .unwrap()
            .contains("<<<<<<<")
    );
    assert!(
        repo.0.join("NOTES.md").is_file(),
        "the clean file landed too"
    );
    assert_eq!(fleet.merge(id).unwrap(), "sh 1 has nothing new to merge");
    fleet.shutdown();
}
