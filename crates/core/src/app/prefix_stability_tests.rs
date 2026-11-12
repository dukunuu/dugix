//! Regression guard for provider prompt caching: a session's request prefix
//! (system prompt, leading images, tool definitions, cache key) must stay
//! byte-identical across turns, tool loops, session switches and restarts, and
//! every request must extend the previous one, until a declared rebuild point.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use super::{App, AppCommand, AppEvent};
use crate::db::Db;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{BackendTag, Model};
use crate::space::Space;

const MODEL: &str = "mock/model";
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R', 0, 0, 0,
    1, 0, 0, 0, 1, 8, 6, 0, 0, 0,
];

#[derive(Clone)]
enum Reply {
    Text(&'static str),
    Tool(&'static str, &'static str),
}

/// A fake chat-completions provider: records every streamed request body and
/// answers from a script, reporting whatever it saw last as cached.
struct Mock {
    requests: Arc<Mutex<Vec<Value>>>,
    script: Arc<Mutex<VecDeque<Reply>>>,
}

impl Mock {
    async fn start() -> (OpenRouter, Self) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let provider =
            OpenRouter::local(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap();
        let mock = Self {
            requests: Arc::default(),
            script: Arc::default(),
        };
        let requests = Arc::clone(&mock.requests);
        let script = Arc::clone(&mock.script);
        let last_prompt = Arc::new(Mutex::new(0_u64));
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let (requests, script, last_prompt) = (
                    Arc::clone(&requests),
                    Arc::clone(&script),
                    Arc::clone(&last_prompt),
                );
                tokio::spawn(async move {
                    let mut socket = BufReader::new(socket);
                    let mut request_line = String::new();
                    socket.read_line(&mut request_line).await.unwrap();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        assert!(socket.read_line(&mut line).await.unwrap() > 0);
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
                    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let streaming = request["stream"] == true;
                    let (mime, text) = if request_line.contains("/chat/completions") && streaming {
                        let prompt = request.to_string().len() as u64 / 4;
                        let cached = {
                            let mut last = last_prompt.lock().unwrap();
                            let cached = (*last).min(prompt);
                            *last = prompt;
                            cached
                        };
                        requests.lock().unwrap().push(request);
                        let usage = serde_json::json!({
                            "prompt_tokens": prompt,
                            "completion_tokens": 10,
                            "prompt_tokens_details": { "cached_tokens": cached },
                        });
                        let reply = script
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or(Reply::Text("ok"));
                        let delta = match reply {
                            Reply::Text(text) => serde_json::json!({ "content": text }),
                            Reply::Tool(name, arguments) => serde_json::json!({
                                "tool_calls": [{
                                    "index": 0,
                                    "id": format!("call-{prompt}"),
                                    "type": "function",
                                    "function": { "name": name, "arguments": arguments },
                                }],
                            }),
                        };
                        let frame = serde_json::json!({
                            "choices": [{ "delta": delta, "finish_reason": "stop" }],
                            "usage": usage,
                        });
                        (
                            "text/event-stream",
                            format!("data: {frame}\n\ndata: [DONE]\n\n"),
                        )
                    } else {
                        // Titles and other background calls.
                        (
                            "application/json",
                            serde_json::json!({
                                "choices": [{ "message": { "content": "Title" } }],
                                "usage": { "prompt_tokens": 5, "completion_tokens": 1 },
                            })
                            .to_string(),
                        )
                    };
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        text.len()
                    );
                    let socket = socket.get_mut();
                    socket.write_all(headers.as_bytes()).await.unwrap();
                    socket.write_all(text.as_bytes()).await.unwrap();
                });
            }
        });
        (provider, mock)
    }

    fn script(&self, replies: impl IntoIterator<Item = Reply>) {
        self.script.lock().unwrap().extend(replies);
    }

    fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

struct Rig {
    root: std::path::PathBuf,
    app: App,
    mock: Mock,
    /// The previous request of each session, for prefix comparison.
    last: HashMap<String, Value>,
    seen: Vec<Value>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn model_catalog() -> Vec<Model> {
    ["mock/model", "mock/other"]
        .into_iter()
        .map(|id| Model {
            id: id.into(),
            name: id.into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: true,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
            aliases: Vec::new(),
        })
        .collect()
}

fn open_app(root: &std::path::Path, provider: OpenRouter) -> App {
    let space = Space {
        root: root.to_path_buf(),
    };
    let db = Db::open(&space.db_path()).unwrap();
    let mut app = App::new(db, None, space);
    app.models = model_catalog();
    app.backends.set(BackendTag::OpenRouter, provider);
    app.current_model = Some(MODEL.into());
    app
}

impl Rig {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nexus-prefix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let (provider, mock) = Mock::start().await;
        let app = open_app(&root, provider);
        Self {
            root,
            app,
            mock,
            last: HashMap::new(),
            seen: Vec::new(),
        }
    }

    /// Replace the app with a fresh one on the same database, as after a restart.
    fn restart(&mut self) {
        let provider = self
            .app
            .backends
            .get(BackendTag::OpenRouter)
            .unwrap()
            .clone();
        self.app = open_app(&self.root, provider);
    }

    fn files_dir(&self) -> std::path::PathBuf {
        let dir = self.app.space.files_dir(&self.app.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Send a message, run the turn to completion, and return the status lines
    /// it produced. Each new request is checked against its session's previous
    /// one; `rebuild` says the first of them is a declared rebuild point.
    async fn turn(&mut self, text: &str, rebuild: bool) -> Vec<String> {
        self.app
            .execute(AppCommand::Send { text: text.into() })
            .unwrap();
        let mut statuses = Vec::new();
        let app = &mut self.app;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match app.next_event().await {
                    AppEvent::Stream(Some((task, event))) => {
                        app.on_chat_event(task, event).unwrap();
                        if app.chat_tasks.is_empty() {
                            break;
                        }
                    }
                    AppEvent::Stream(None) => break,
                    AppEvent::Title(title) => app.on_title_result(title),
                    AppEvent::Status(status) if !status.is_empty() => statuses.push(status),
                    _ => {}
                }
            }
        })
        .await
        .expect("turn finished");
        let requests = self.mock.take();
        assert!(!requests.is_empty(), "the turn sent no request");
        for (index, request) in requests.into_iter().enumerate() {
            self.check(request, rebuild && index == 0);
        }
        statuses
    }

    fn check(&mut self, request: Value, rebuild: bool) {
        let key = request["prompt_cache_key"]
            .as_str()
            .expect("cache key")
            .to_string();
        let (session, generation) = key.rsplit_once(':').unwrap();
        let generation: u64 = generation.parse().unwrap();
        if let Some(previous) = self.last.get(session) {
            let previous_key = previous["prompt_cache_key"].as_str().unwrap();
            let previous_generation: u64 =
                previous_key.rsplit_once(':').unwrap().1.parse().unwrap();
            if rebuild {
                assert_eq!(
                    generation,
                    previous_generation + 1,
                    "one rebuild, one new lane"
                );
            } else {
                assert_eq!(key, previous_key, "the cache key moved");
                assert_eq!(
                    leading(&request),
                    leading(previous),
                    "system/images changed"
                );
                assert_eq!(
                    request["tools"], previous["tools"],
                    "tool definitions changed"
                );
                let (old, new) = (messages(previous), messages(&request));
                assert!(new.len() >= old.len(), "history shrank");
                assert_eq!(
                    &new[..old.len()],
                    &old[..],
                    "the history is not an extension"
                );
            }
        } else {
            assert!(!rebuild, "nothing to rebuild");
        }
        self.last.insert(session.to_string(), request.clone());
        self.seen.push(request);
    }

    fn session_id(&self) -> String {
        self.app.session.as_ref().unwrap().id.clone()
    }
}

fn messages(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::to_string)
        .collect()
}

/// Everything before the first non-system message: the prompt, the leading
/// space images and any compaction digest.
fn leading(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .take_while(|m| m["role"] == "system")
        .map(Value::to_string)
        .collect()
}

fn system_text(request: &Value) -> String {
    request["messages"][0].to_string()
}

fn staleness_hints(statuses: &[String]) -> usize {
    statuses
        .iter()
        .filter(|s| s.starts_with("prompt frozen"))
        .count()
}

#[tokio::test]
async fn the_prefix_holds_until_a_declared_rebuild() {
    let mut rig = Rig::new().await;
    std::fs::write(rig.files_dir().join("first.png"), PNG).unwrap();
    rig.app.rescan_files();

    // Turn 1: a multi-step tool loop that writes memory through the tool.
    rig.mock.script([
        Reply::Tool(
            "memory",
            r#"{"action":"add","chapter":"About you","text":"likes tea"}"#,
        ),
        Reply::Tool("memory", r#"{"action":"read"}"#),
        Reply::Text("noted"),
    ]);
    let statuses = rig.turn("remember that I like tea", false).await;
    assert_eq!(
        staleness_hints(&statuses),
        0,
        "nothing was stale yet: {statuses:?}"
    );
    assert!(rig.seen.len() >= 3);
    let first = rig.seen[0].clone();
    assert!(
        first["messages"][0]["content"]
            .to_string()
            .contains("image_url"),
        "the space image leads the prompt"
    );
    assert!(
        !system_text(&first).contains("About you"),
        "memory did not exist yet"
    );
    let session = rig.session_id();

    // Turn 2: more tool steps.
    rig.mock.script([
        Reply::Tool("memory", r#"{"action":"read"}"#),
        Reply::Text("done"),
    ]);
    let statuses = rig.turn("what do you know about me?", false).await;
    assert_eq!(
        staleness_hints(&statuses),
        1,
        "the memory write is announced: {statuses:?}"
    );
    assert!(
        statuses.iter().any(|s| s.contains("/refresh")),
        "the hint names the way out"
    );

    // Things that change under the frozen prefix: a search that stored
    // citations, a new file, and a memory editor save.
    rig.app
        .db
        .add_citations(
            &rig.app.active_space.id,
            "report.md",
            &[("https://example.com/a".into(), Some("A".into()))],
        )
        .unwrap();
    std::fs::write(rig.files_dir().join("notes.txt"), "fresh notes").unwrap();
    rig.app.rescan_files();
    let edited = rig
        .app
        .memory_markdown()
        .unwrap()
        .replace("likes tea", "likes green tea");
    rig.app.save_memory_markdown(&edited).unwrap();
    let statuses = rig.turn("third question", false).await;
    assert_eq!(
        staleness_hints(&statuses),
        0,
        "announced once per frozen prefix, not per change: {statuses:?}"
    );
    rig.turn("fourth question", false).await;

    // Switch to another conversation and back.
    rig.app.new_session();
    rig.turn("an unrelated chat", false).await;
    assert_ne!(rig.session_id(), session);
    rig.app.switch_to_session_by_id(&session).unwrap();
    rig.turn("back again", false).await;

    // Restart the app on the same database.
    rig.restart();
    rig.app.switch_to_session_by_id(&session).unwrap();
    rig.turn("after the restart", false).await;

    // An image message.
    rig.mock.script([Reply::Text("a pixel")]);
    rig.turn("look at this ![pic](first.png)", false).await;
    let last = rig.seen.last().unwrap();
    assert!(
        last["messages"]
            .to_string()
            .contains("data:image/png;base64")
    );

    // /refresh rebuilds once, and the new files and tools arrive with it.
    let before = rig.seen.last().unwrap().clone();
    rig.app.run_command("refresh").unwrap();
    let statuses = rig.turn("after refresh", true).await;
    assert_eq!(staleness_hints(&statuses), 0, "{statuses:?}");
    let after = rig.seen.last().unwrap();
    assert_ne!(after["tools"], before["tools"], "the new tools are in");
    assert!(after["tools"].to_string().contains("citations"));
    assert!(
        system_text(after).contains("About you (1)"),
        "the memory is in"
    );
    rig.turn("steady again", false).await;
    rig.turn("and again", false).await;

    // Compaction is a declared rebuild point.
    let through = rig
        .app
        .messages
        .iter()
        .rposition(|m| m.role == "user")
        .unwrap();
    rig.app.on_compact_result(Some((
        session.clone(),
        "digest of the earlier chat".into(),
        i64::try_from(through).unwrap(),
        50,
    )));
    rig.turn("after compaction", true).await;
    assert!(system_text(rig.seen.last().unwrap()).contains("digest of the earlier chat"));
    rig.turn("and once more", false).await;

    // A model change is a declared rebuild point.
    rig.app.pick_model("mock/other").unwrap();
    rig.turn("another model", true).await;
    rig.turn("same model", false).await;

    // The reuse accounting never blames nexus for a change it did not make.
    let verdicts = rig.app.db.cache_verdict_counts(None).unwrap();
    assert!(
        !verdicts.iter().any(|(name, _)| name == "prefix_changed"),
        "{verdicts:?}"
    );
    assert!(
        verdicts
            .iter()
            .any(|(name, count)| name == "hit" && *count > 5),
        "{verdicts:?}"
    );
}
