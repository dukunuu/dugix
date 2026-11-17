//! A whole research run against a scripted provider: what each stage is handed
//! and what report comes out, with citations followed from searcher to report.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::mpsc;

use super::*;
use crate::app::research::ResearchUpdate;

/// Every request as `(system prompt, user message)`, in arrival order.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// A chat endpoint that answers each request by the stage its system prompt names.
async fn scripted(reply: fn(&str, &str) -> String) -> (OpenRouter, Seen) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let provider =
        OpenRouter::local(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut socket = BufReader::new(socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if socket.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let text_of = |role: &str| {
                    request["messages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|m| m["role"] == role)
                        .map(|m| match &m["content"] {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let (system, user) = (text_of("system"), text_of("user"));
                let text = reply(&system, &user);
                log.lock().unwrap().push((system, user));
                let usage = serde_json::json!({"prompt_tokens": 10, "completion_tokens": 5});
                let (mime, body) = if request["stream"] == true {
                    let chunk = serde_json::json!({"choices":[{"delta":{"content":text},"finish_reason":"stop"}],"usage":usage});
                    (
                        "text/event-stream",
                        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                    )
                } else {
                    let whole = serde_json::json!({"choices":[{"message":{"content":text},"finish_reason":"stop"}],"usage":usage});
                    ("application/json", whole.to_string())
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let socket = socket.get_mut();
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
            });
        }
    });
    (provider, seen)
}

/// Two searchers cite one page under their own `[1]`; the writer invents a citation.
fn stage_reply(system: &str, user: &str) -> String {
    if system.contains("planning stage") {
        r#"[{"question":"Q-alpha"},{"question":"Q-beta"}]"#.into()
    } else if system.contains("searcher agent") {
        if user.contains("preliminary survey") {
            "The landscape overview [1].\nSources:\n1. https://shared.example".into()
        } else if user.contains("Q-alpha") {
            "Alpha fact [1].\nSources:\n1. https://shared.example/".into()
        } else {
            "Beta fact [1] and beta detail [2].\n**Sources:**\n1. https://SHARED.example\n2. https://beta.example/page".into()
        }
    } else if system.contains("synthesis stage") {
        "Alpha fact [1]. Beta detail [2].".into()
    } else if system.contains("critic stage") {
        "**Satisfied.**".into()
    } else if system.contains("verifier stage") {
        "Alpha fact [1]. Beta detail [2] ‹med›.".into()
    } else if system.contains("final writer stage") {
        "# Report\n\nAlpha fact [1]. Beta detail [2] ‹med›. Invented claim [7].\n\n## Sources\n1. https://made-up.example\n".into()
    } else {
        format!("unscripted stage: {system:.80}")
    }
}

/// Run research end to end against `reply`; the report and every request seen.
async fn run_scripted(reply: fn(&str, &str) -> String) -> (String, Vec<(String, String)>) {
    let root = std::env::temp_dir().join(format!("dugix-research-run-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db_path = root.join("dugix.db");
    drop(crate::db::Db::open(&db_path).unwrap());
    {
        // A page one searcher opened: the verifier must be handed its text.
        let conn = crate::db::open_attached(&db_path).unwrap();
        crate::db::cache_put(
            &conn,
            &crate::tools::normalize_url("https://beta.example/page"),
            "https://beta.example/page",
            None,
            "Navigation and cookie banner text that is unrelated.\n\nThe beta detail is documented here with measured results.",
        )
        .unwrap();
    }
    let (provider, seen) = scripted(reply).await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (_steer_tx, steer_rx) = mpsc::unbounded_channel();
    run_research(ResearchOptions {
        research_provider: provider.clone(),
        research_model: "m".into(),
        embedding_provider: provider,
        embedding_model: String::new(),
        db_path: db_path.clone(),
        incognito: false,
        topic: "the topic".into(),
        reply_rx: None,
        steer_rx,
        toolbox: Arc::new(crate::tools::NoTools),
        tx,
        session_id: "s1".into(),
        prompt_cache_key: "k".into(),
        cache_ttl_1h: false,
        space_id: "space".into(),
        space_name: "default".into(),
    })
    .await;
    let mut report = None;
    while let Ok((.., update)) = rx.try_recv() {
        if let ResearchUpdate::Done(result) = update {
            report = Some(result.unwrap());
        }
    }
    let report = report.expect("the run finished");
    let seen = seen.lock().unwrap().clone();
    std::fs::remove_dir_all(root).unwrap();
    (report, seen)
}

fn user_of(seen: &[(String, String)], stage: &str) -> String {
    seen.iter()
        .find(|(system, _)| system.contains(stage))
        .map(|(_, user)| user.clone())
        .unwrap_or_else(|| panic!("no {stage} request"))
}

#[tokio::test]
async fn citations_survive_from_searchers_to_the_report() {
    let (report, seen) = run_scripted(stage_reply).await;
    let user_of = |stage: &str| user_of(&seen, stage);

    // One page, one number, for every searcher that cited it.
    let synth = user_of("synthesis stage");
    assert!(synth.contains("Alpha fact [1]."), "{synth}");
    assert!(
        synth.contains("Beta fact [1] and beta detail [2]."),
        "{synth}"
    );
    assert!(
        synth.contains("Sources:\n1. https://shared.example\n2. https://beta.example/page"),
        "{synth}"
    );
    // The verifier reads the opened page, not just the searcher's summary of it.
    let verify = user_of("verifier stage");
    assert!(
        verify.contains("[2] https://beta.example/page\nThe beta detail is documented here"),
        "{verify}"
    );
    assert!(!verify.contains("cookie banner"), "{verify}");
    assert!(
        verify.contains("[1] https://shared.example\n(never opened"),
        "{verify}"
    );

    assert_eq!(
        report,
        "# Report\n\nAlpha fact [1]. Beta detail [2] ‹med›. Invented claim.\n\n\
         ## Limitations\n\n\
         - 1 cited source was never opened, only seen in search results; claims resting on it alone are marked ‹snippet›.\n\
         - 1 citation named no source the searchers found and was removed.\n\n\
         ## Sources\n\n1. https://shared.example\n2. https://beta.example/page\n"
    );
}

static CRITIC_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The critic finds a gap behind a preamble, then one that stays open.
fn gap_reply(system: &str, user: &str) -> String {
    if system.contains("critic stage") {
        return if CRITIC_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            "Here is my review.\n**GAPS:**\n- Q-gamma?".into()
        } else {
            "Gaps:\n1. Q-delta?".into()
        };
    }
    if system.contains("searcher agent") && user.contains("Q-gamma") {
        return "Gamma fact [1].\nSources:\n1. https://gamma.example".into();
    }
    if system.contains("final writer stage") {
        return "# Report\n\nGamma fact [3]. Alpha fact [1].".into();
    }
    stage_reply(system, user)
}

#[tokio::test]
async fn a_gap_round_extends_the_shared_numbering_and_open_gaps_are_reported() {
    let (report, seen) = run_scripted(gap_reply).await;
    let merge = seen
        .iter()
        .rev()
        .find(|(system, _)| system.contains("synthesis stage"))
        .map(|(_, user)| user.clone())
        .unwrap();
    assert!(merge.contains("Gamma fact [3]."), "{merge}");
    assert!(
        merge.contains(
            "New entries in the shared sources list:\nSources:\n3. https://gamma.example"
        ),
        "{merge}"
    );
    assert!(
        report.contains("- Still not covered after a second search round: Q-delta?\n"),
        "{report}"
    );
    assert!(
        report.starts_with("# Report\n\nGamma fact [1]. Alpha fact [2]."),
        "{report}"
    );
    assert!(
        report.ends_with("## Sources\n\n1. https://gamma.example\n2. https://shared.example\n"),
        "{report}"
    );
}
