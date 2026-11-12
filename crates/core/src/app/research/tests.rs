use super::*;
use crate::app::App;
use crate::db::Db;
use crate::space::Space;

fn test_app() -> App {
    let db = Db::open_in_memory().unwrap();
    let root = std::env::temp_dir().join(format!("nexus-research-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    let space = Space { root };
    let mut a = App::new(db, Some("k"), space);
    a.current_model = Some("openai/gpt-5-mini".to_string());
    a
}

#[test]
fn research_params_pass_the_cache_ttl_experiment() {
    for enabled in [false, true] {
        let usage = ResearchUsage {
            db_path: std::path::PathBuf::new(),
            session_id: "s".to_string(),
            prompt_cache_key: "research:s:0".to_string(),
            cache_ttl_1h: enabled,
            space_id: "sp".to_string(),
            backend: "OpenRouter",
        };
        let params = usage.chat_params();
        assert_eq!(params.cache_ttl_1h, enabled);
        assert_eq!(params.prompt_cache_key.as_deref(), Some("research:s:0"));
    }
}

#[tokio::test]
async fn drain_steers_collects_all_queued_without_blocking() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send("look into X".to_string()).unwrap();
    tx.send("also Y".to_string()).unwrap();
    let drained = drain_steers(&mut rx);
    assert_eq!(
        drained,
        vec!["look into X".to_string(), "also Y".to_string()]
    );
    let empty = drain_steers(&mut rx);
    assert!(empty.is_empty());
}

#[test]
fn planner_messages_with_context_includes_known_chunks_as_gap_guidance() {
    let msgs = planner_messages_with_context(
        "rust async runtimes",
        &[],
        &["Rust's async model uses a Future trait.".to_string()],
    );
    assert_eq!(msgs[0].role, "system");
    assert!(msgs[1].content.contains("rust async runtimes"));
    assert!(msgs[1].content.contains("Already known"));
    assert!(msgs[1].content.contains("Future trait"));
}

#[test]
fn planner_messages_with_context_falls_back_to_plain_prompt_when_empty() {
    let msgs = planner_messages_with_context("topic", &[], &[]);
    assert!(!msgs[1].content.contains("Already known"));
    assert_eq!(msgs[1].content, "topic");
}

#[test]
fn planner_messages_with_context_folds_user_answers_into_the_prompt() {
    let msgs = planner_messages_with_context(
        "topic",
        &[
            ("q1".to_string(), "depth first".to_string()),
            ("q2".to_string(), "current state only".to_string()),
        ],
        &[],
    );
    let user = &msgs[1].content;
    assert!(user.contains("answered clarifying questions"));
    assert!(user.contains("depth first"));
    assert!(user.contains("current state only"));
    assert!(user.contains("topic"));
}

#[test]
fn verifier_prompt_mentions_quote_checking() {
    assert!(VERIFIER_PROMPT.to_lowercase().contains("quote"));
}

#[test]
fn parse_plan_blocks_reads_json_objects_with_all_fields() {
    let qs = parse_plan_blocks(
        r#"[{"question":"what is X","why":"definitions matter","angles":["a1","a2"],"sources":["s1","s2"]}]"#,
    );
    assert_eq!(qs.len(), 1);
    assert_eq!(qs[0].question, "what is X");
    assert_eq!(qs[0].why, "definitions matter");
    assert_eq!(qs[0].angles, vec!["a1".to_string(), "a2".to_string()]);
    assert_eq!(qs[0].sources, vec!["s1".to_string(), "s2".to_string()]);
}

#[test]
fn parse_plan_blocks_defaults_missing_fields_and_strips_fences() {
    let qs = parse_plan_blocks("```json\n[{\"question\": \"what is X\"}]\n```");
    assert_eq!(qs.len(), 1);
    assert_eq!(qs[0].question, "what is X");
    assert!(qs[0].why.is_empty());
    assert!(qs[0].angles.is_empty());
    assert!(qs[0].sources.is_empty());
}

#[test]
fn parse_plan_blocks_falls_back_to_bare_questions_on_non_json() {
    let qs = parse_plan_blocks("- what is X\n2. how does Y work");
    assert_eq!(qs.len(), 2);
    assert_eq!(qs[0].question, "what is X");
    assert!(qs[0].why.is_empty());
    assert_eq!(qs[1].question, "how does Y work");
}

#[test]
fn parse_plan_blocks_filters_empty_questions_and_caps_at_max() {
    let qs = parse_plan_blocks(
        r#"[{"question":""},{"question":"q1"},{"question":"q2"},{"question":"q3"}]"#,
    );
    assert_eq!(qs.len(), 3);
    assert_eq!(qs[0].question, "q1");
    let lines: Vec<String> = (0..10).map(|i| format!("q{i}")).collect();
    assert_eq!(parse_plan_blocks(&lines.join("\n")).len(), MAX_SUBQUESTIONS);
}

#[test]
fn parse_plan_blocks_rejects_malformed_json_without_line_fallback() {
    assert!(parse_plan_blocks("[{}]").is_empty(), "[{{}}] must fail");
    assert!(
        parse_plan_blocks(r#"[{"question":""}]"#).is_empty(),
        "empty questions must fail"
    );
    assert!(
        parse_plan_blocks(r#"[{"question": 5}]"#).is_empty(),
        "wrong field types must fail"
    );
    assert!(
        parse_plan_blocks(r#"{"question":"q1"}"#).is_empty(),
        "a bare object is not the required array and must fail"
    );
    assert_eq!(parse_plan_blocks("- what is X").len(), 1);
    let prose = parse_plan_blocks("Here is the plan:\n[{\"question\":\"q1\"}]");
    assert_eq!(prose.len(), 1, "prose-prefixed JSON must parse as JSON");
    assert_eq!(prose[0].question, "q1");
    let legacy = parse_plan_blocks(r#"["what is X", "how does Y work"]"#);
    assert_eq!(legacy.len(), 2);
    assert_eq!(legacy[0].question, "what is X");
}

#[test]
fn parse_survey_reply_recognizes_complete_markers() {
    assert_eq!(parse_survey_reply("COMPLETE"), SurveyReply::Complete);
    assert_eq!(parse_survey_reply("  complete  "), SurveyReply::Complete);
    assert_eq!(
        parse_survey_reply("COMPLETE: I have enough"),
        SurveyReply::Complete
    );
    assert_eq!(
        parse_survey_reply("COMPLETE — proceed"),
        SurveyReply::Complete
    );
    assert_eq!(parse_survey_reply("COMPLETE."), SurveyReply::Complete);
    assert_eq!(parse_survey_reply("COMPLETE!"), SurveyReply::Complete);
}

#[test]
fn parse_survey_reply_reads_numbered_questions() {
    assert_eq!(
        parse_survey_reply("1. Depth or breadth?\n2. History too?"),
        SurveyReply::Questions(vec![
            "Depth or breadth?".to_string(),
            "History too?".to_string()
        ])
    );
    assert_eq!(
        parse_survey_reply("- just one angle"),
        SurveyReply::Questions(vec!["just one angle".to_string()])
    );
}

#[test]
fn parse_survey_reply_marks_output_contract_violations_and_caps_questions() {
    assert_eq!(parse_survey_reply(""), SurveyReply::Malformed);
    assert_eq!(parse_survey_reply("\n\n"), SurveyReply::Malformed);
    assert_eq!(
        parse_survey_reply("I couldn't understand your last answer, please retry"),
        SurveyReply::Malformed
    );
    assert_eq!(
        parse_survey_reply("The model encountered an error processing the request."),
        SurveyReply::Malformed
    );
    assert_eq!(
        parse_survey_reply("No further questions are needed"),
        SurveyReply::Malformed
    );
    assert_eq!(
        parse_survey_reply("Depth or breadth?"),
        SurveyReply::Questions(vec!["Depth or breadth?".to_string()])
    );
    let mixed = "1. Depth or breadth?\nPlease be specific.\n2. History too?";
    assert_eq!(
        parse_survey_reply(mixed),
        SurveyReply::Questions(vec![
            "Depth or breadth?".to_string(),
            "History too?".to_string()
        ])
    );
    let lines: Vec<String> = (0..8).map(|i| format!("{}. q{i}?", i + 1)).collect();
    match parse_survey_reply(&lines.join("\n")) {
        SurveyReply::Questions(qs) => assert_eq!(qs.len(), MAX_SURVEY_QUESTIONS),
        _ => panic!("expected questions"),
    }
}

#[test]
fn parse_approval_recognizes_approved_and_revised_plans() {
    assert_eq!(parse_approval("APPROVED"), Approval::Approved);
    assert_eq!(parse_approval("  approved  "), Approval::Approved);
    assert_eq!(parse_approval("APPROVED: run it"), Approval::Approved);
    let revised = parse_approval("[{\"question\": \"revised q\"}]");
    assert_eq!(
        revised,
        Approval::Revised(vec![PlanQuestion::bare("revised q".to_string())])
    );
    assert_eq!(parse_approval("huh?"), Approval::Malformed);
    assert_eq!(parse_approval(""), Approval::Malformed);
    assert_eq!(
        parse_approval("Here is the revised plan I prepared for you"),
        Approval::Malformed
    );
    assert_eq!(parse_approval("[{}]"), Approval::Malformed);
    assert_eq!(parse_approval("[{\"question\": 5}]"), Approval::Malformed);
    assert_eq!(
        parse_approval("Here is my revised plan:\n[{\"question\":\"q\"}]\n"),
        Approval::Revised(vec![PlanQuestion::bare("q".to_string())])
    );
    assert_eq!(
        parse_approval("- drop q2"),
        Approval::Revised(vec![PlanQuestion::bare("drop q2".to_string())])
    );
}

#[test]
fn plan_question_prompt_includes_topic_and_full_brief() {
    let q = PlanQuestion {
        question: "how does X work".to_string(),
        why: "mechanism matters".to_string(),
        angles: vec!["internals".to_string(), "benchmarks".to_string()],
        sources: vec!["papers".to_string()],
    };
    let p = q.prompt("rust async");
    assert!(p.contains("rust async"));
    assert!(p.contains("how does X work"));
    assert!(p.contains("mechanism matters"));
    assert!(p.contains("internals; benchmarks"));
    assert!(p.contains("papers"));
    assert!(PlanQuestion::bare("q".into()).prompt("t").contains('q'));
}

#[test]
fn plan_text_renders_numbered_questions_with_indented_briefs() {
    let qs = vec![
        PlanQuestion::bare("q1".to_string()),
        PlanQuestion {
            question: "q2".to_string(),
            why: "why2".to_string(),
            angles: vec!["a".to_string()],
            sources: vec!["s".to_string()],
        },
    ];
    let t = plan_text(&qs);
    assert!(t.contains("1. q1"));
    assert!(t.contains("2. q2"));
    assert!(t.contains("\n   Why: why2"));
    assert!(t.contains("\n   Angles: a"));
    assert!(t.contains("\n   Sources: s"));
}

#[test]
fn research_stage_prompts_share_a_stable_cache_prefix() {
    let planner = research_stage_prompt(PLANNER_PROMPT);
    let writer = research_stage_prompt(WRITER_PROMPT);
    assert!(planner.starts_with(RESEARCH_CACHE_PREFIX));
    assert!(writer.starts_with(RESEARCH_CACHE_PREFIX));
    assert_ne!(planner, writer);
}

#[test]
fn survey_messages_include_topic_and_rounds() {
    let msgs = survey_messages("t", &[]);
    assert_eq!(msgs[0].role, "system");
    assert!(msgs[1].content.contains('t'));
    let msgs = survey_messages("t", &[("q".to_string(), "a".to_string())]);
    assert!(msgs[1].content.contains('q'));
    assert!(msgs[1].content.contains('a'));
}

#[test]
fn plan_approval_messages_include_plan_and_user_reply() {
    let msgs = plan_approval_messages("topic", &[PlanQuestion::bare("q1".to_string())], "drop q2");
    assert!(msgs[1].content.contains("topic"));
    assert!(msgs[1].content.contains("1. q1"));
    assert!(msgs[1].content.contains("drop q2"));
}

#[tokio::test]
async fn on_research_done_final_report_populates_citation_index() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();

    a.on_research_done(Some((
        session_id,
        space_id.clone(),
        space_name,
        ResearchUpdate::Done(Ok(
            "# Report\n\nBody [1].\n\n## Sources\n1. https://example.com/a\n".to_string(),
        )),
    )));

    let hits =
        a.db.search_citations(&space_id, Some("example.com"))
            .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].1, "https://example.com/a");
    assert!(
        a.db.search_citations(&space_id, Some("nope.example"))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn plan_ready_arms_the_gate_and_reply_routes_into_the_pipeline() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();

    let (tx, mut rx) = mpsc::unbounded_channel();
    a.research.survey_reply_tx = Some(tx);
    let q1 = PlanQuestion::bare("q1".to_string());
    let q2 = PlanQuestion::bare("q2".to_string());

    a.on_research_done(Some((
        session_id.clone(),
        space_id,
        space_name,
        ResearchUpdate::PlanReady {
            questions: vec![q1.clone(), q2.clone()],
            rework: false,
        },
    )));
    assert!(a.research.survey_gate.is_some());
    assert!(a.survey_gate_targets_current_session());
    assert!(a.messages.iter().any(|m| m.role == "research_plan"));
    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(stored.iter().any(|m| m.role == "research_plan"));
    let plan_msg = a
        .messages
        .iter()
        .find(|m| m.role == "research_plan")
        .unwrap();
    assert!(plan_msg.content.contains("1. q1"));

    a.reply_to_survey_gate("drop q2");
    assert!(a.research.survey_gate.is_none());
    assert!(!a.survey_gate_targets_current_session());
    assert_eq!(rx.recv().await.unwrap(), "drop q2");
    assert!(
        a.messages
            .iter()
            .any(|m| m.role == "gate_reply" && m.content == "drop q2")
    );
    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(
        stored
            .iter()
            .any(|m| m.role == "gate_reply" && m.content == "drop q2")
    );
    let history = a.build_history();
    assert!(
        !history.iter().any(|m| m.content == "drop q2"),
        "gate replies must be excluded from model history"
    );
}

#[tokio::test]
async fn plan_ready_saves_a_plan_file_record_in_the_space() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();
    let (tx, _rx) = mpsc::unbounded_channel();
    a.research.survey_reply_tx = Some(tx);

    a.on_research_done(Some((
        session_id,
        space_id,
        space_name.clone(),
        ResearchUpdate::PlanReady {
            questions: vec![PlanQuestion::bare("q1".to_string())],
            rework: false,
        },
    )));

    let dir = a.space.files_dir(&space_name);
    let saved: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.starts_with("plan-")
                && std::path::Path::new(n)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .collect();
    assert_eq!(saved.len(), 1, "expected one plan file in {dir:?}");
    let body = std::fs::read_to_string(dir.join(&saved[0])).unwrap();
    assert!(body.contains("Research plan: rust async runtimes"));
    assert!(body.contains("1. q1"));
}

#[tokio::test]
async fn survey_ready_arms_the_gate_and_renders_a_survey_section() {
    let mut a = test_app();
    a.start_research("fine-tuning LLMs");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    a.research.survey_reply_tx = Some(tx);

    a.on_research_done(Some((
        session_id.clone(),
        space_id,
        space_name,
        ResearchUpdate::SurveyReady {
            questions: vec!["Depth or breadth?".to_string()],
            round: 1,
        },
    )));
    assert!(a.survey_gate_targets_current_session());
    let survey = a.messages.iter().find(|m| m.role == "survey").unwrap();
    assert!(survey.content.contains("For \"fine-tuning LLMs\":"));
    assert!(survey.content.contains("1. Depth or breadth?"));
    assert!(survey.content.contains("I approve"));
    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(stored.iter().any(|m| m.role == "survey"));

    a.reply_to_survey_gate("depth first");
    assert!(a.research.survey_gate.is_none());
    assert_eq!(rx.recv().await.unwrap(), "depth first");
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("follow-ups"));
}

#[tokio::test]
async fn gate_only_targets_the_viewed_gated_session() {
    let mut a = test_app();
    a.start_research("topic one");
    let gated_session = a.session.as_ref().unwrap().id.clone();
    let (tx, _rx) = mpsc::unbounded_channel();
    a.research.survey_reply_tx = Some(tx);
    a.on_research_done(Some((
        gated_session.clone(),
        a.active_space.id.clone(),
        a.active_space.name.clone(),
        ResearchUpdate::SurveyReady {
            questions: vec!["q?".to_string()],
            round: 1,
        },
    )));
    assert!(a.survey_gate_targets_current_session());

    let other =
        a.db.create_session("other", "m", &a.active_space.id, "chat")
            .unwrap();
    a.session = Some(other);
    a.messages.clear();
    assert!(!a.survey_gate_targets_current_session());
    assert!(
        a.research.survey_gate.is_some(),
        "gate stays armed for its session"
    );
}

#[tokio::test]
async fn closed_reply_channel_fails_plan_approval_closed() {
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();
    drop(reply_tx);
    let (tx, _rx) = mpsc::unbounded_channel::<ResearchMsg>();
    let ids = ("s".to_string(), "sp".to_string(), "sn".to_string());
    let mut questions = vec![PlanQuestion::bare("q1".to_string())];
    let provider = OpenRouter::openrouter_flavor("test-key".to_string());
    let usage = ResearchUsage {
        db_path: std::env::temp_dir().join("nexus-research-test.db"),
        session_id: ids.0.clone(),
        prompt_cache_key: "s:0".to_string(),
        cache_ttl_1h: false,
        space_id: ids.1.clone(),
        backend: provider.backend_tag().name(),
    };
    let result = await_plan_approval(
        PlanApprovalContext {
            provider: &provider,
            model: "a/b",
            topic: "topic",
            usage: &usage,
            tx: &tx,
            ids: &ids,
        },
        &mut questions,
        &mut reply_rx,
    )
    .await;
    let err = result.expect_err("closed channel must fail closed, not approve");
    assert!(err.contains("cancelled"), "{err}");
}

#[test]
fn start_research_rejects_blank_topic_and_missing_model() {
    let mut a = test_app();
    a.start_research("  ");
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("usage:"));
    assert!(a.research.rx.is_none());

    a.current_model = None;
    a.start_research("rust async runtimes");
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("no model configured"));
    assert!(a.research.rx.is_none());
}

#[tokio::test]
async fn start_research_creates_and_switches_into_a_new_session() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    assert!(a.research.rx.is_some());
    assert!(a.research.running.is_some());
    let session = a
        .session
        .as_ref()
        .expect("switched into the research session");
    assert!(session.title.contains("rust async runtimes"));
    assert!(
        a.messages
            .iter()
            .any(|m| m.content.contains("/research rust async runtimes"))
    );
}

#[tokio::test]
async fn start_research_refuses_a_second_concurrent_job() {
    let mut a = test_app();
    a.start_research("topic one");
    assert!(a.research.rx.is_some());
    a.start_research("topic two");
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("already running"));
    assert!(a.session.as_ref().unwrap().title.contains("topic one"));
}

#[tokio::test]
async fn on_research_done_stage_update_persists_and_shows_when_viewing() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();

    a.on_research_done(Some((
        session_id.clone(),
        space_id,
        space_name,
        ResearchUpdate::Stage {
            label: "planning".to_string(),
            detail: String::new(),
        },
    )));

    assert!(
        a.messages
            .iter()
            .any(|m| m.role == "research_stage" && m.content == "planning")
    );
    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(
        stored
            .iter()
            .any(|m| m.role == "research_stage" && m.content == "planning")
    );
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("planning"));

    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();
    a.on_research_done(Some((
        session_id.clone(),
        space_id,
        space_name,
        ResearchUpdate::Stage {
            label: "planning".to_string(),
            detail: "revised".to_string(),
        },
    )));
    let stored = a.db.load_messages(&session_id).unwrap();
    let rows: Vec<_> = stored
        .iter()
        .filter(|m| m.role == "research_stage")
        .collect();
    assert_eq!(rows.len(), 1, "one row per label, updated in place");
    assert_eq!(rows[0].content, "planning: revised");
    let visible_rows: Vec<_> = a
        .messages
        .iter()
        .filter(|m| m.role == "research_stage")
        .collect();
    assert_eq!(visible_rows.len(), 1);
    assert_eq!(visible_rows[0].content, "planning: revised");
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("Ctrl+↑ agents"));
}

#[tokio::test]
async fn multiple_drained_steers_each_keep_their_own_stage_row() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();

    for (i, steer) in ["look into X", "also Y"].iter().enumerate() {
        a.on_research_done(Some((
            session_id.clone(),
            space_id.clone(),
            space_name.clone(),
            ResearchUpdate::Stage {
                label: format!("steer #{}", i + 1),
                detail: steer.to_string(),
            },
        )));
    }
    assert_eq!(
        a.research.steer_acked,
        std::collections::HashSet::from([1, 2])
    );
    let stored = a.db.load_messages(&session_id).unwrap();
    let steer_rows: Vec<_> = stored
        .iter()
        .filter(|m| m.role == "research_stage" && m.content.starts_with("steer #"))
        .collect();
    assert_eq!(steer_rows.len(), 2, "one persisted row per drained steer");
    let visible: Vec<_> = a
        .messages
        .iter()
        .filter(|m| m.role == "research_stage" && m.content.starts_with("steer #"))
        .collect();
    assert_eq!(visible.len(), 2, "both steers visible in the transcript");
}

#[tokio::test]
async fn steer_rows_do_not_collide_on_duplicate_prefix_or_wildcard_text() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();

    let steers = ["a: b", "a", "same", "same", "100% done"];
    for (i, steer) in steers.iter().enumerate() {
        a.on_research_done(Some((
            session_id.clone(),
            space_id.clone(),
            space_name.clone(),
            ResearchUpdate::Stage {
                label: format!("steer #{}", i + 1),
                detail: steer.to_string(),
            },
        )));
    }
    let stored = a.db.load_messages(&session_id).unwrap();
    let rows: Vec<_> = stored
        .iter()
        .filter(|m| m.role == "research_stage" && m.content.starts_with("steer #"))
        .collect();
    assert_eq!(
        rows.len(),
        steers.len(),
        "one row per drained steer — no collapse on duplicate/prefix/wildcard text"
    );
    for (i, steer) in steers.iter().enumerate() {
        let want = format!("steer #{}: {steer}", i + 1);
        assert!(
            rows.iter().any(|m| m.content == want),
            "missing row for steer #{}: {want}",
            i + 1
        );
    }
    assert_eq!(
        a.research.steer_acked,
        (1..=steers.len()).collect::<std::collections::HashSet<usize>>(),
        "every steer position picked up"
    );
}

#[test]
fn steer_log_drops_acknowledged_entries_and_clears_on_stop() {
    let mut a = test_app();
    let (tx, _rx) = mpsc::unbounded_channel::<String>();
    a.research.steer_tx = Some(tx);

    a.steer_research("first");
    a.steer_research("second");
    a.steer_research("third");
    assert_eq!(
        a.research.steer_log,
        vec![
            (1, "first".into()),
            (2, "second".into()),
            (3, "third".into())
        ]
    );

    a.research.steer_acked = std::collections::HashSet::from([1, 2]);
    a.steer_research("fourth");
    assert_eq!(
        a.research.steer_log,
        vec![(3, "third".into()), (4, "fourth".into())]
    );

    a.research.steer_acked.insert(3);
    a.on_research_done(Some((
        "s".to_string(),
        "sp".to_string(),
        "sn".to_string(),
        ResearchUpdate::Stage {
            label: "steer #3".to_string(),
            detail: "third".to_string(),
        },
    )));
    assert_eq!(a.research.steer_log, vec![(4, "fourth".into())]);

    let (_tx, rx) = mpsc::unbounded_channel::<ResearchMsg>();
    a.research.rx = Some(rx);
    a.research.running = Some(("s".to_string(), "t".to_string()));
    a.stop_research();
    assert!(a.research.steer_log.is_empty());
    assert!(a.research.steer_acked.is_empty());
}

#[test]
fn steer_queue_is_hard_bound() {
    let mut a = test_app();
    let (tx, _rx) = mpsc::unbounded_channel::<String>();
    a.research.steer_tx = Some(tx);

    for i in 0..MAX_QUEUED_STEERS {
        a.steer_research(&format!("steer {i}"));
    }
    assert_eq!(a.research.steer_log.len(), MAX_QUEUED_STEERS);
    a.steer_research("overflow");
    assert_eq!(a.research.steer_log.len(), MAX_QUEUED_STEERS);
    assert!(
        a.last_status().contains("steer queue full"),
        "{}",
        a.last_status()
    );
}

#[tokio::test]
async fn plan_ready_in_incognito_mode_writes_no_plan_file() {
    let mut a = test_app();
    a.incognito = true;
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();
    let (tx, _rx) = mpsc::unbounded_channel();
    a.research.survey_reply_tx = Some(tx);

    a.on_research_done(Some((
        session_id.clone(),
        space_id,
        space_name.clone(),
        ResearchUpdate::PlanReady {
            questions: vec![PlanQuestion::bare("q1".to_string())],
            rework: false,
        },
    )));

    let dir = a.space.files_dir(&space_name);
    let _ = std::fs::create_dir_all(&dir);
    let saved: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("plan-"))
        .collect();
    assert!(saved.is_empty(), "no plan files in incognito: {saved:?}");
    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(
        stored.iter().all(|m| m.role != "research_plan"),
        "the plan must not be persisted to the message db in incognito"
    );
    assert!(
        a.messages.iter().any(|m| m.role == "research_plan"),
        "the in-memory transcript still shows the plan while viewed"
    );
}

#[tokio::test]
async fn incognito_gate_rows_follow_the_mode_captured_at_job_start() {
    let mut a = test_app();
    a.incognito = true;
    a.start_research("private topic");
    let session_id = a.session.as_ref().unwrap().id.clone();
    assert!(a.research.incognito);

    a.incognito = false;
    let (tx, mut rx) = mpsc::unbounded_channel();
    a.research.survey_reply_tx = Some(tx);
    a.on_research_done(Some((
        session_id.clone(),
        a.active_space.id.clone(),
        a.active_space.name.clone(),
        ResearchUpdate::SurveyReady {
            questions: vec!["Which confidential product?".to_string()],
            round: 1,
        },
    )));
    a.reply_to_survey_gate("Project Juniper");
    assert_eq!(rx.recv().await.unwrap(), "Project Juniper");

    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(stored.iter().all(|m| m.role != "survey"));
    assert!(stored.iter().all(|m| m.role != "gate_reply"));
}

#[tokio::test]
async fn off_screen_incognito_plan_is_restored_when_its_session_opens() {
    let mut a = test_app();
    a.incognito = true;
    a.start_research("private topic");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let other =
        a.db.create_session("other", "m", &a.active_space.id, "chat")
            .unwrap();
    a.session = Some(other);
    a.messages.clear();
    let (tx, _rx) = mpsc::unbounded_channel();
    a.research.survey_reply_tx = Some(tx);

    a.on_research_done(Some((
        session_id.clone(),
        a.active_space.id.clone(),
        a.active_space.name.clone(),
        ResearchUpdate::PlanReady {
            questions: vec![PlanQuestion::bare("private question".to_string())],
            rework: true,
        },
    )));
    assert!(!a.survey_gate_targets_current_session());
    assert!(
        a.db.load_messages(&session_id)
            .unwrap()
            .iter()
            .all(|m| m.role != "research_plan")
    );

    a.switch_to_session_by_id(&session_id).unwrap();

    assert!(a.survey_gate_targets_current_session());
    let plan = a
        .messages
        .iter()
        .find(|m| m.role == "research_plan")
        .expect("pending incognito plan restored in memory");
    assert!(plan.content.contains("private question"));
    assert!(plan.content.contains("reply \"approve\""));
    assert!(!plan.content.contains("tell me what to change"));
}

#[tokio::test]
async fn undelivered_gate_reply_is_rolled_back_and_restored_to_composer() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let (reply_tx, rx) = mpsc::unbounded_channel::<String>();
    drop(rx);
    a.research.survey_reply_tx = Some(reply_tx.clone());
    a.on_research_done(Some((
        session_id.clone(),
        a.active_space.id.clone(),
        a.active_space.name.clone(),
        ResearchUpdate::PlanReady {
            questions: vec![PlanQuestion::bare("q1".to_string())],
            rework: false,
        },
    )));
    assert!(a.research.survey_gate.is_some());

    a.reply_to_survey_gate("drop q2");

    assert!(a.research.survey_gate.is_none());
    let (sets, _) = a.drain_ui_events();
    assert_eq!(sets, vec!["drop q2".to_string()]);
    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(
        stored.iter().all(|m| m.role != "gate_reply"),
        "undelivered reply must not remain persisted"
    );
    assert!(!a.messages.iter().any(|m| m.role == "gate_reply"));
}

#[tokio::test]
async fn off_screen_gate_marks_the_session_unread_and_notifies() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let other =
        a.db.create_session("other", "m", &a.active_space.id, "chat")
            .unwrap();
    a.session = Some(other);
    a.messages.clear();
    let (tx, _rx) = mpsc::unbounded_channel();
    a.research.survey_reply_tx = Some(tx);

    a.on_research_done(Some((
        session_id.clone(),
        a.active_space.id.clone(),
        a.active_space.name.clone(),
        ResearchUpdate::SurveyReady {
            questions: vec!["Depth or breadth?".to_string()],
            round: 1,
        },
    )));

    assert!(a.research.survey_gate.is_some());
    assert!(!a.survey_gate_targets_current_session());
    assert!(
        a.unread.contains(&session_id),
        "session must be marked unread"
    );
    let (_, status) = a.drain_ui_events();
    assert!(status.contains("waiting on you"), "{status}");
    assert!(status.contains("survey round 1"), "{status}");
}

#[tokio::test]
async fn on_research_done_final_report_posts_message_saves_file_and_notifies_when_away() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();

    a.session = None;
    a.messages.clear();

    a.on_research_done(Some((
        session_id.clone(),
        space_id,
        space_name.clone(),
        ResearchUpdate::Done(Ok(
            "# Rust Async Runtimes\n\nBody text. [1]\n\n## Sources\n1. https://a".to_string(),
        )),
    )));

    assert!(a.unread.contains(&session_id));
    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(
        stored
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("Rust Async Runtimes"))
    );

    let dir = a.space.files_dir(&space_name);
    let saved = report_files(&dir);
    assert_eq!(
        saved.len(),
        1,
        "expected exactly one saved report file in {dir:?}"
    );
    let library = crate::canvas::library(&dir);
    assert_eq!(library.len(), 1, "{library:?}");
    assert_eq!(library[0].title, "Rust Async Runtimes");
    assert_eq!(
        library[0].relative,
        format!("docs/{}.doc.json", saved[0].strip_suffix(".md").unwrap())
    );
}

fn report_files(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |read| {
            read.filter_map(std::result::Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".md"))
                .collect()
        },
    )
}

#[tokio::test]
async fn research_doc_keeps_watch_notes_and_links_unusable_images() {
    let mut a = test_app();
    a.start_research("tide tables");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();
    a.on_research_done(Some((
        session_id,
        space_id,
        space_name.clone(),
        ResearchUpdate::Done(Ok("## What changed\n\nNew data.\n\n# Tides\n\n\
             ![chart](file:///etc/x.png)\n\n![map](https://example.com/m.png)\n"
            .to_string())),
    )));
    let dir = a.space.files_dir(&space_name);
    let entry = &crate::canvas::library(&dir)[0];
    let doc = crate::doc::storage::load(&dir, &entry.relative).unwrap();
    assert_eq!(doc.title, "Tides");
    let kinds: Vec<_> = doc.blocks.iter().map(|b| &b.kind).collect();
    assert!(matches!(
        kinds[0],
        crate::doc::Kind::Heading { level: 2, .. }
    ));
    assert!(matches!(kinds[2], crate::doc::Kind::Title { .. }));
    assert!(
        matches!(kinds[3], crate::doc::Kind::Paragraph { text } if text.contains("file:///etc/x.png")),
        "{kinds:?}"
    );
    assert!(matches!(kinds[4], crate::doc::Kind::Image { .. }));
}

#[tokio::test]
async fn on_research_done_saves_report_to_original_space_even_if_user_switched() {
    let mut a = test_app();

    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let original_space_id = a.active_space.id.clone();
    let original_space_name = a.active_space.name.clone();

    let second_space = a.db.create_space("research-test-space-2").unwrap();
    a.space.ensure_space_dir(&second_space.name).unwrap();
    a.active_space = second_space.clone();
    a.session = None;
    a.messages.clear();
    a.caches.files.clear();

    assert_eq!(a.active_space.id, second_space.id);
    assert_ne!(a.active_space.id, original_space_id);

    a.on_research_done(Some((
        session_id.clone(),
        original_space_id.clone(),
        original_space_name.clone(),
        ResearchUpdate::Done(Ok(
            "# Rust Async Runtimes\n\nBody text. [1]\n\n## Sources\n1. https://a".to_string(),
        )),
    )));

    let original_dir = a.space.files_dir(&original_space_name);
    let original_files = report_files(&original_dir).len();
    assert_eq!(crate::canvas::library(&original_dir).len(), 1);
    assert_eq!(
        original_files, 1,
        "expected exactly one report file in original space {original_dir:?}"
    );

    let second_dir = a.space.files_dir(&second_space.name);
    let second_files =
        std::fs::read_dir(&second_dir).map_or(0, |d| d.filter_map(std::result::Result::ok).count());
    assert_eq!(
        second_files, 0,
        "expected no files in second (active) space {second_dir:?}"
    );

    assert_eq!(
        a.caches.files.len(),
        0,
        "files_cache should be empty since rescan was not triggered"
    );
}

#[tokio::test]
async fn on_research_done_failure_posts_error_message() {
    let mut a = test_app();
    a.start_research("rust async runtimes");
    let session_id = a.session.as_ref().unwrap().id.clone();
    let space_id = a.active_space.id.clone();
    let space_name = a.active_space.name.clone();

    a.on_research_done(Some((
        session_id.clone(),
        space_id,
        space_name,
        ResearchUpdate::Done(Err("planner: network down".to_string())),
    )));

    let (_, status) = a.drain_ui_events();
    assert!(status.contains("network down"));
    let stored = a.db.load_messages(&session_id).unwrap();
    assert!(
        stored
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("network down"))
    );
}

#[tokio::test]
async fn on_research_done_none_clears_domain_state() {
    let mut a = test_app();
    a.start_research("t");
    assert!(a.research.rx.is_some());
    a.research.live_input = "late steer".to_string();
    a.research.stage_rows = vec!["writer: done".to_string()];

    a.on_research_done(None);

    assert!(a.research.rx.is_none());
    assert!(a.research.running.is_none());
    assert!(a.research.live_input.is_empty());
    assert!(a.research.stage_rows.is_empty());
}

#[test]
fn parse_subquestions_reads_a_clean_json_array() {
    let qs = parse_subquestions(r#"["what is X", "how does Y work"]"#);
    assert_eq!(
        qs,
        vec!["what is X".to_string(), "how does Y work".to_string()]
    );
}

#[test]
fn parse_subquestions_strips_markdown_fences() {
    let qs = parse_subquestions("```json\n[\"a\", \"b\"]\n```");
    assert_eq!(qs, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn parse_subquestions_falls_back_to_bullet_lines() {
    let qs = parse_subquestions("- what is X\n- how does Y work\n* a third one");
    assert_eq!(
        qs,
        vec![
            "what is X".to_string(),
            "how does Y work".to_string(),
            "a third one".to_string()
        ]
    );
}

#[test]
fn parse_subquestions_falls_back_to_numbered_lines() {
    let qs = parse_subquestions("1. what is X\n2) how does Y work");
    assert_eq!(
        qs,
        vec!["what is X".to_string(), "how does Y work".to_string()]
    );
}

#[test]
fn parse_subquestions_caps_at_max() {
    let lines: Vec<String> = (0..10).map(|i| format!("- q{i}")).collect();
    let qs = parse_subquestions(&lines.join("\n"));
    assert_eq!(qs.len(), MAX_SUBQUESTIONS);
}

#[test]
fn parse_critique_recognizes_satisfied() {
    assert_eq!(parse_critique("SATISFIED"), Critique::Satisfied);
    assert_eq!(parse_critique("  satisfied  "), Critique::Satisfied);
}

#[test]
fn parse_critique_recognizes_gaps() {
    let c = parse_critique("GAPS:\n- what about pricing?\n- any recent incidents?");
    assert_eq!(
        c,
        Critique::Gaps(vec![
            "what about pricing?".to_string(),
            "any recent incidents?".to_string()
        ])
    );
}

#[test]
fn parse_critique_recognizes_contradiction() {
    let c = parse_critique("CONTRADICTION: source A says X, source B says not-X");
    assert_eq!(
        c,
        Critique::Contradiction("source A says X, source B says not-X".to_string())
    );
}

#[test]
fn parse_critique_falls_back_to_satisfied_on_garbage() {
    assert_eq!(
        parse_critique("uh, looks fine I guess?"),
        Critique::Satisfied
    );
    assert_eq!(parse_critique("GAPS:\n"), Critique::Satisfied);
}

#[test]
fn synthesizer_messages_includes_topic_and_all_findings() {
    let msgs = synthesizer_messages(
        "rust async runtimes",
        &["finding one".to_string(), "finding two".to_string()],
        &[],
    );
    assert_eq!(msgs[0].role, "system");
    assert!(msgs[1].content.contains("rust async runtimes"));
    assert!(msgs[1].content.contains("finding one"));
    assert!(msgs[1].content.contains("finding two"));
}

#[test]
fn synthesizer_messages_lists_pinned_sources_when_present() {
    let msgs = synthesizer_messages(
        "topic",
        &["finding one".to_string()],
        &["https://a.example".to_string()],
    );
    let user = msgs.iter().find(|m| m.role == "user").unwrap();
    assert!(
        user.content.contains("https://a.example"),
        "{}",
        user.content
    );
    assert!(
        user.content.to_lowercase().contains("prioritize"),
        "{}",
        user.content
    );
}

#[test]
fn synthesizer_messages_omits_pinned_section_when_empty() {
    let msgs = synthesizer_messages("topic", &["finding one".to_string()], &[]);
    let user = msgs.iter().find(|m| m.role == "user").unwrap();
    assert!(
        !user.content.to_lowercase().contains("prioritize"),
        "{}",
        user.content
    );
}

#[test]
fn critic_messages_includes_topic_and_draft() {
    let msgs = critic_messages("topic X", "draft text");
    assert!(msgs[1].content.contains("topic X"));
    assert!(msgs[1].content.contains("draft text"));
}

#[test]
fn resolver_messages_includes_contradiction_description() {
    let msgs = resolver_messages("t", "draft", &["f1".to_string()], "A vs B");
    assert!(msgs[1].content.contains("A vs B"));
    assert!(msgs[1].content.contains("f1"));
}

#[test]
fn writer_messages_includes_verified_draft() {
    let msgs = writer_messages("t", "verified content", &[]);
    assert!(msgs[1].content.contains("verified content"));
}
