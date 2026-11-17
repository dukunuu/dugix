use super::*;

#[test]
fn ui_failures_are_replay_only_and_stable_across_turns() {
    fn message(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        }
    }
    let bad =
        "```dugix-ui\n{\"version\":1,\"title\":\"Quiz\",\"view\":[{\"type\":\"radio\"}]}\n```";
    let good = "```dugix-ui\n{\"version\":1,\"title\":\"Quiz\",\"view\":[{\"type\":\"text\",\"text\":\"Hi\"}]}\n```";
    let mut app = super::super::tests::app_with_key();
    app.messages = vec![
        message("assistant", bad),
        message("user", "again"),
        message("assistant", bad),
        message("user", "fix it"),
    ];
    let history = app.build_history();
    let assistants: Vec<_> = history
        .iter()
        .filter(|message| message.role == "assistant")
        .collect();
    let noted = format!(
        "{bad}\n[dugix: the dugix-ui block \"Quiz\" failed to render — view[0]: unknown node type \"radio\" — valid: {}. Correct the reported errors and resend it if the user still needs it.]",
        crate::interactive::NODE_TYPES
    );
    assert_eq!(assistants[0].content, noted);
    assert_eq!(assistants[1].content, noted);
    assert_eq!(app.messages[2].content, bad);
    assert_eq!(
        app.build_history()[history.len() - 2].content,
        assistants[1].content
    );
    app.messages[0].content = good.into();
    app.messages[2].content = good.into();
    let history = app.build_history();
    assert!(
        history
            .iter()
            .all(|message| !message.content.contains("[dugix: the dugix-ui block"))
    );
    app.messages.push(message("assistant", "No UI needed."));
    assert!(
        app.build_history()
            .iter()
            .all(|message| !message.content.contains("failed to render"))
    );
}

#[test]
fn ui_replay_notes_ignore_open_and_literal_fences_and_number_untitled_blocks() {
    for source in [
        "```dugix-ui\n{",
        "~~~dugix-ui\n{",
        "````markdown\n```dugix-ui\n{\n```\n````",
        "    ```dugix-ui\n    {\n    ```",
    ] {
        assert!(ui_failure_notes(source).is_empty(), "{source}");
    }
    let source = "~~~DuGiX-Ui extra\n{\"version\":1,\"title\":\"Fine\",\"view\":[{\"type\":\"text\",\"text\":\"OK\"}]}\n~~~\n```dugix-ui\n{\n```";
    let note = ui_failure_notes(source);
    assert!(
        note.contains("block \"#2\" failed to render — $:"),
        "{note}"
    );
    assert_eq!(note.lines().count(), 2);
    let note = ui_failure_notes(
        "> ```dugix-ui\n> {\"version\":1,\"title\":\"Quoted\",\"view\":[{\"type\":\"radio\"}]}\n> ```",
    );
    assert!(note.contains("block \"Quoted\""), "{note}");
    assert!(ui_failure_notes("> > ```dugix-ui\n> > {\"version\":1,\"title\":\"Fine\",\"view\":[{\"type\":\"text\",\"text\":\"OK\"}]}\n> > ```").is_empty());
    assert!(!ui_failure_notes("```dugix-ui\n> {\"version\":1,\"title\":\"Fine\",\"view\":[{\"type\":\"text\",\"text\":\"OK\"}]}\n```" ).is_empty());
}

#[test]
fn a_panel_shown_without_its_broken_piece_is_replayed_as_partly_rendered() {
    let note = ui_failure_notes(
        "```dugix-ui\nroot = Panel(\"Trip\", [Text(\"Kept\"), PieChart([1])])\n```",
    );
    assert!(
        note.contains("block \"Trip\" rendered with parts left out — "),
        "{note}"
    );
    assert!(note.contains("unknown component PieChart"), "{note}");
}

#[test]
fn chat_params_pass_the_cache_lifetime_settings() {
    let mut app = super::super::tests::app_with_key();
    assert_eq!(app.settings.cache_lifetime, "extended");
    assert!(!app.settings.cache_long_openai);
    for (lifetime, enabled) in [("default", false), ("extended", true)] {
        app.settings.cache_lifetime = lifetime.into();
        app.settings.cache_long_openai = enabled;
        let params = app.chat_params(None, crate::provider::BackendTag::OpenRouter);
        assert_eq!(params.cache_ttl_1h, enabled);
        assert_eq!(params.cache_long_openai, enabled);
    }
}

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dugix-usercmd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn captured_command_timeout_kills_its_process_group() {
    let dir = temp_dir();
    let started = std::time::Instant::now();
    let (exit, ran, output) = capture_user_command(
        "echo started; (sleep 1; touch late) & sleep 5",
        &dir,
        std::time::Duration::from_millis(300),
    )
    .await;
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!((exit, ran), (None, true));
    assert_eq!(user_command_outcome_label(exit, ran), "killed");
    assert!(output.starts_with("started\n"), "{output}");
    assert!(
        output.ends_with("[timed out after 300ms · killed]"),
        "{output}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert!(
        !dir.join("late").exists(),
        "background job outlived the kill"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn an_escaped_descendant_holding_the_pipe_does_not_stall_the_command() {
    let dir = temp_dir();
    let started = std::time::Instant::now();
    // setsid escapes the group kill while retaining the output pipe.
    let (exit, ran, output) = capture_user_command(
        "perl -MPOSIX -e 'POSIX::setsid(); open my $f, \">\", \"escaped\"; close $f; sleep 10' & \
         while [ ! -e escaped ]; do sleep 0.05; done; echo hi",
        &dir,
        USER_COMMAND_TIMEOUT,
    )
    .await;
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!((exit, ran), (Some(0), true));
    assert_eq!(output, format!("hi\n\n{USER_COMMAND_STREAM_OPEN_NOTE}"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_command_that_cannot_start_says_so() {
    let missing = std::env::temp_dir().join(format!("dugix-missing-{}", uuid::Uuid::new_v4()));
    let (exit, ran, output) = capture_user_command("true", &missing, USER_COMMAND_TIMEOUT).await;
    assert_eq!((exit, ran), (None, false));
    assert_eq!(user_command_outcome_label(exit, ran), "failed to start");
    assert!(output.starts_with("could not run: "), "{output}");
}

#[test]
fn runaway_output_keeps_only_its_head_and_tail() {
    let mut buffer = HeadTail::default();
    buffer.push("H".repeat(USER_COMMAND_KEEP_BYTES - 1).as_bytes());
    buffer.push(b"HMMMMMMMMMM");
    buffer.push("T".repeat(USER_COMMAND_KEEP_BYTES).as_bytes());
    let text = buffer.text();
    assert!(!text.contains('M'));
    assert!(text.contains("\n… [10 bytes omitted] …\n"));
    assert_eq!(text.matches('H').count(), USER_COMMAND_KEEP_BYTES);
    assert_eq!(text.matches('T').count(), USER_COMMAND_KEEP_BYTES);
    let mut short = HeadTail::default();
    short.push(b"short");
    assert_eq!(short.text(), "short");
}

#[test]
fn replay_fences_output_longer_than_its_own_backticks() {
    let row = user_command_row(
        "cat README.md",
        "/w",
        Some(0),
        true,
        "```rust\nfn x() {}\n```\n",
        false,
    );
    assert_eq!(
        user_command_replay(&row),
        "I ran `cat README.md` in /w (exit 0):\n````\n```rust\nfn x() {}\n```\n````"
    );
    for (exit, started, outcome) in [(None, true, "killed"), (None, false, "failed to start")] {
        let row = user_command_row(
            "sudo true",
            "/w",
            exit,
            started,
            USER_COMMAND_INTERACTIVE_NOTE,
            true,
        );
        assert_eq!(
            user_command_replay(&row),
            format!("I ran `sudo true` in /w ({outcome}):\n{USER_COMMAND_INTERACTIVE_NOTE}")
        );
    }
}
