use super::*;

#[test]
fn chat_body_disables_parallel_calls_only_with_tools_and_explicit_false() {
    let tools = [ToolDef {
        name: "test".into(),
        description: "Test.".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }];
    let messages = [ChatMessage::text("user", "Hi")];
    for provider in [
        OpenRouter::openrouter_flavor(String::new()),
        OpenRouter::openai(String::new()),
        OpenRouter::local("http://127.0.0.1:9/v1").unwrap(),
    ] {
        for tools in [&[][..], tools.as_slice()] {
            for disable_tool_choice in [false, true] {
                let baseline = provider.chat_completions_body(
                    "test",
                    &messages,
                    &ChatParams::default(),
                    tools,
                    disable_tool_choice,
                );
                for parallel_tool_calls in [None, Some(true), Some(false)] {
                    let params = ChatParams {
                        parallel_tool_calls,
                        tool_result_cap: Some(4_000),
                        ..ChatParams::default()
                    };
                    let mut body = provider.chat_completions_body(
                        "test",
                        &messages,
                        &params,
                        tools,
                        disable_tool_choice,
                    );
                    if parallel_tool_calls == Some(false) && !tools.is_empty() {
                        assert_eq!(body["parallel_tool_calls"], false);
                        body.as_object_mut().unwrap().remove("parallel_tool_calls");
                    } else {
                        assert!(body.get("parallel_tool_calls").is_none());
                    }
                    assert_eq!(body, baseline);
                }
            }
        }
    }
}

struct ProfileToolExecutor {
    result: String,
}

impl ToolExecutor for ProfileToolExecutor {
    fn defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "test".into(),
            description: "Test.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }

    fn is_read_only(&self, _name: &str, _args: &str) -> bool {
        true
    }

    fn run<'a>(
        &'a self,
        _name: &'a str,
        _args: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (String, String)> + Send + 'a>> {
        Box::pin(async move { (self.result.clone(), "Done".into()) })
    }

    fn supports_images(&self) -> bool {
        false
    }

    fn space_files_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
}

#[tokio::test]
async fn tool_loop_caps_only_model_visible_results_and_dedups_capped_replay() {
    for (cap, replay) in [(None, false), (Some(4_000), false), (Some(4_000), true)] {
        let result = format!("HEAD:{}:TAIL", "界".repeat(7_990));
        let executor = Arc::new(ProfileToolExecutor {
            result: result.clone(),
        });
        let visible = cap.map_or_else(
            || result.clone(),
            |cap| crate::tools::cap_head_tail(&result, cap),
        );
        let call = ToolCall {
            id: "old".into(),
            name: "test".into(),
            arguments: "{}".into(),
        };
        let mut history = vec![ChatMessage::text("user", "Read")];
        if replay {
            history.push(ChatMessage {
                role: "assistant".into(),
                tool_calls: Some(vec![call]),
                ..ChatMessage::default()
            });
            history.push(ChatMessage {
                role: "tool".into(),
                content: visible.clone(),
                tool_call_id: Some("old".into()),
                ..ChatMessage::default()
            });
            history.push(ChatMessage::text("user", "Read again"));
        }
        let first = fake_chat_frames(&[serde_json::json!({"choices": [{"delta": {
            "tool_calls": [{"index": 0, "id": "new", "type": "function", "function": {
                "name": "test", "arguments": "{}"
            }}]
        }}]})]);
        let second =
            fake_chat_frames(&[serde_json::json!({"choices": [{"delta": {"content": "Done"}}]})]);
        let (provider, server) = fake_chat_server(vec![first, second]).await;
        let (mut rx, _) = provider.stream_chat(
            "test".into(),
            history,
            ChatParams {
                tool_result_cap: cap,
                ..ChatParams::default()
            },
            executor.defs(),
            executor,
            2,
        );
        let mut stored = None;
        while let Some(event) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
        {
            match event {
                StreamEvent::Error(error) => panic!("stream failed: {error}"),
                StreamEvent::ToolCall { result, .. } => stored = Some(result),
                _ => {}
            }
        }
        assert_eq!(stored.as_deref(), Some(result.as_str()));
        let requests = server.await.unwrap();
        let messages = requests[1]["messages"].as_array().unwrap();
        let content = messages.last().unwrap()["content"].as_str().unwrap();
        if replay {
            assert_eq!(
                content,
                crate::tools::tool_result_unchanged_note("test", "{}")
            );
            assert_eq!(
                &requests[0]["messages"],
                &serde_json::json!(&messages[..messages.len() - 2])
            );
        } else {
            assert_eq!(content, visible);
            assert!(content.starts_with("HEAD:"));
            assert!(content.ends_with(":TAIL"));
            if let Some(cap) = cap {
                assert!(content.chars().count() <= cap);
            }
        }
    }
}

#[tokio::test]
async fn openrouter_requests_carry_app_attribution_and_others_do_not() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    async fn headers_of(provider: OpenRouter) -> Vec<String> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut headers = Vec::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
                headers.push(line.trim_end().to_ascii_lowercase());
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            let reply = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
            headers
        });
        let mut provider = provider;
        provider.endpoint = Some(endpoint);
        let _ = provider
            .complete_with_params(
                "test-model",
                vec![ChatMessage::text("user", "hi")],
                &ChatParams::default(),
            )
            .await;
        server.await.unwrap()
    }

    let headers = headers_of(OpenRouter::openrouter_flavor("sk-or-test".into())).await;
    for (name, value) in OPENROUTER_ATTRIBUTION {
        let line = format!(
            "{}: {}",
            name.to_ascii_lowercase(),
            value.to_ascii_lowercase()
        );
        assert!(headers.contains(&line), "missing {line} in {headers:?}");
    }
    assert!(
        headers
            .iter()
            .any(|line| line.starts_with("http-referer: https://")),
        "{headers:?}"
    );
    // Other backends never get OpenRouter's attribution headers.
    let headers = headers_of(OpenRouter::openai("sk-test".into())).await;
    assert!(
        !headers
            .iter()
            .any(|line| line.starts_with("http-referer") || line.contains("x-openrouter")),
        "{headers:?}"
    );
}

async fn fake_chat_server(
    responses: Vec<Vec<u8>>,
) -> (OpenRouter, tokio::task::JoinHandle<Vec<serde_json::Value>>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(stream.read_line(&mut line).await.unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            requests.push(serde_json::from_slice(&body).unwrap());
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            stream
                .get_mut()
                .write_all(headers.as_bytes())
                .await
                .unwrap();
            stream.get_mut().write_all(&response).await.unwrap();
            stream.get_mut().shutdown().await.unwrap();
        }
        requests
    });
    (OpenRouter::local(&endpoint).unwrap(), server)
}

fn fake_chat_frames(chunks: &[serde_json::Value]) -> Vec<u8> {
    let mut frames: Vec<_> = chunks.iter().flat_map(chat_sse_frame).collect();
    frames.extend_from_slice(b"data: [DONE]\n\n");
    frames
}

#[tokio::test]
async fn done_round_returns_accumulated_content_and_usage() {
    let response = fake_chat_frames(&[
        serde_json::json!({"choices": [{"delta": {"reasoning_content": "Think."}}]}),
        serde_json::json!({"choices": [{"delta": {"content": "Hello "}}]}),
        serde_json::json!({"choices": [{"delta": {"content": "world"}}]}),
        serde_json::json!({"choices": [], "usage": {
            "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12
        }}),
    ]);
    let (provider, server) = fake_chat_server(vec![response]).await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let finish = provider
        .run_stream(
            "test-model",
            &[ChatMessage::text("user", "Hi")],
            &ChatParams::default(),
            &[],
            false,
            &tx,
        )
        .await
        .unwrap();
    let Finish::Done(output) = finish else {
        panic!("expected a completed round");
    };
    assert_eq!(output.content, "Hello world");
    assert_eq!(output.reasoning.as_deref(), Some("Think."));
    let usage = output.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 2);
    assert_eq!(usage.total_tokens, 12);
    assert!(matches!(rx.try_recv().unwrap(), StreamEvent::Reasoning(text) if text == "Think."));
    assert!(matches!(rx.try_recv().unwrap(), StreamEvent::Token(text) if text == "Hello "));
    assert!(matches!(rx.try_recv().unwrap(), StreamEvent::Token(text) if text == "world"));
    assert!(
        matches!(rx.try_recv().unwrap(), StreamEvent::Usage(u) if u.total_tokens == usage.total_tokens)
    );
    assert!(rx.try_recv().is_err());
    server.await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // End-to-end request, tool and trace assertions.
async fn tool_loop_records_one_step_per_round() {
    use crate::trace::{Scrubber, TraceContext, TraceStore};

    let first = fake_chat_frames(&[serde_json::json!({"choices": [{"delta": {
        "tool_calls": [{"index": 0, "id": "call-1", "type": "function", "function": {
            "name": "scripts", "arguments": "{\"action\":\"list\"}"
        }}]
    }}]})]);
    let second = fake_chat_frames(&[serde_json::json!({"choices": [{"delta": {
        "content": "No scripts found."
    }, "finish_reason": "stop"}]})]);
    let (provider, server) = fake_chat_server(vec![first, second]).await;
    let dir = std::env::temp_dir().join(format!("nexus-chat-trace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    let store = Arc::new(TraceStore::open(&dir.join("traces"), Scrubber::new(Vec::new())).unwrap());
    let mut toolbox = crate::tools::ToolBox::new(
        dir.join("skills"),
        None,
        None,
        "auto".into(),
        Vec::new(),
        None,
        None,
        None,
    );
    toolbox.space_scripts_dir = dir.join("scripts");
    let tools: Vec<_> = toolbox
        .defs()
        .into_iter()
        .filter(|tool| tool.name == "scripts")
        .collect();
    let params = ChatParams {
        reasoning_effort: Some("low".into()),
        temperature: Some(0.5),
        max_tokens: Some(100),
        trace: Some(TraceContext {
            store: store.clone(),
            session_id: "session".into(),
            space_id: "space".into(),
            turn_id: "turn".into(),
            backend: "Local".into(),
        }),
        ..ChatParams::default()
    };
    let (mut rx, _) = provider.stream_chat(
        "test-model".into(),
        vec![
            ChatMessage::text("system", "Be helpful."),
            ChatMessage::text("user", "List scripts"),
        ],
        params,
        tools.clone(),
        Arc::new(toolbox),
        3,
    );
    let mut tool_output = None;
    let mut done = 0;
    while let Some(event) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        match event {
            StreamEvent::Error(error) => panic!("stream failed: {error}"),
            StreamEvent::ToolCall { result, .. } => tool_output = Some(result),
            StreamEvent::Done => done += 1,
            _ => {}
        }
    }
    assert_eq!(done, 1);
    let requests = server.await.unwrap();
    let mut steps = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let steps = store.steps_for_session("session").unwrap();
            if steps.len() == 2 {
                break steps
                    .into_iter()
                    .map(|id| store.load_step(&id).unwrap())
                    .collect::<Vec<_>>();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    steps.sort_by_key(|step| step["round"].as_u64().unwrap());
    assert_eq!(steps[0]["round"], 0);
    assert_eq!(steps[1]["round"], 1);
    assert_eq!(steps[0]["response"]["finish"], "tool_calls");
    assert_eq!(steps[0]["response"]["tool_calls"][0]["name"], "scripts");
    assert_eq!(steps[0]["tool_results"].as_array().unwrap().len(), 1);
    let tool_message = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    assert_eq!(tool_message["content"], "[]");
    assert_eq!(steps[0]["tool_results"][0]["seen"], tool_message["content"]);
    assert_eq!(steps[0]["tool_results"][0]["full"], tool_output.unwrap());
    assert_eq!(steps[1]["response"]["finish"], "stop");
    assert_eq!(steps[1]["response"]["content"], "No scripts found.");
    assert!(steps[1]["tool_results"].as_array().unwrap().is_empty());
    for (step, request) in steps.iter().zip(&requests) {
        assert_eq!(step["system"], "Be helpful.");
        assert_eq!(
            step["messages"],
            serde_json::json!(&request["messages"].as_array().unwrap()[1..])
        );
        assert_eq!(step["tools"], serde_json::to_value(&tools).unwrap());
        assert_eq!(
            step["params"],
            serde_json::json!({"reasoning_effort": "low", "temperature": 0.5, "max_tokens": 100})
        );
        assert_eq!(step["model"], "test-model");
        assert_eq!(step["turn_id"], "turn");
    }
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn codex_adapter_survives_multibyte_split_across_chunks() {
    let mut adapter = CodexChatStreamAdapter::new("gpt-5-codex");
    let event = concat!(
        r#"data: {"type":"response.output_text.delta","delta":"héllo — 世界"}"#,
        "\n\n"
    )
    .as_bytes()
    .to_vec();
    // Split inside the three-byte 世, the worst case for per-chunk decoding.
    let split = event
        .iter()
        .position(|byte| *byte == 0xe4)
        .expect("multi-byte character present")
        + 1;
    let mut frames = adapter.push_bytes(&event[..split]);
    frames.extend(adapter.push_bytes(&event[split..]));
    let text: String = frames
        .iter()
        .map(|frame| String::from_utf8_lossy(frame).into_owned())
        .collect();
    assert!(text.contains("héllo — 世界"), "corrupted delta: {text}");
    assert!(!text.contains('\u{fffd}'), "replacement char in: {text}");
}

#[test]
fn codex_adapter_reports_a_truncated_stream_as_an_error() {
    let mut adapter = CodexChatStreamAdapter::new("gpt-5-codex");
    adapter.push_bytes(
        concat!(
            r#"data: {"type":"response.output_text.delta","delta":"half an ans"}"#,
            "\n\n"
        )
        .as_bytes(),
    );
    adapter.fail("Codex stream stalled");
    let frames = adapter.finish();
    let text: String = frames
        .iter()
        .map(|frame| String::from_utf8_lossy(frame).into_owned())
        .collect();
    assert!(text.contains("Codex stream stalled"), "frames: {text}");
    assert!(
        !text.contains(r#""finish_reason":"stop""#),
        "a cut-off stream must not claim a normal stop: {text}"
    );
    assert!(text.contains("[DONE]"));
}

#[test]
fn codex_tool_call_response_reports_null_content() {
    let response = serde_json::json!({
        "id": "resp_1",
        "model": "gpt-5-codex",
        "output": [{
            "type": "function_call",
            "call_id": "call_1",
            "name": "read_file",
            "arguments": "{\"path\":\"a.md\"}",
        }],
    });
    let chat = codex_response_to_chat(&response);
    let message = &chat["choices"][0]["message"];
    assert!(message["content"].is_null(), "message: {message}");
    assert_eq!(message["tool_calls"][0]["function"]["name"], "read_file");
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
}

/// Shaped like a real `/codex/models` response (client 0.158.0).
#[test]
fn codex_catalog_parses_visible_models_in_priority_order() {
    let body = serde_json::json!({ "models": [
        { "slug": "gpt-5.5", "display_name": "GPT-5.5", "visibility": "list",
          "priority": 12, "context_window": 272_000, "input_modalities": ["text", "image"],
          "supported_reasoning_levels": [
              {"effort": "low"}, {"effort": "medium"}, {"effort": "high"}, {"effort": "xhigh"}] },
        { "slug": "codex-auto-review", "display_name": "Codex Auto Review",
          "visibility": "hide", "priority": 43, "supported_reasoning_levels": [] },
        { "slug": "gpt-reserve", "display_name": "GPT-Reserve", "visibility": "hide",
          "priority": 3, "context_window": 272_000,
          "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}] },
        { "slug": "gpt-6-astra", "display_name": "GPT-6-Astra", "visibility": "list",
          "priority": 1, "context_window": 272_000, "input_modalities": ["text", "image"],
          "supported_reasoning_levels": [
              {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
              {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}] }
    ]});
    let models = parse_codex_catalog(&body);
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["gpt-6-astra", "gpt-reserve", "gpt-5.5"]);
    let astra = &models[0];
    assert_eq!(astra.name, "GPT-6-Astra");
    assert_eq!(astra.context_length, Some(272_000));
    assert!(astra.supports_images);
    // `ultra` needs the Codex CLI's own delegation tools, so it's dropped.
    assert_eq!(
        astra.reasoning_efforts,
        [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max
        ]
    );
    assert!(parse_codex_catalog(&serde_json::json!({})).is_empty());
}

#[test]
fn local_requests_carry_one_leading_system_message() {
    let history = vec![
        ChatMessage::text("system", "Prompt"),
        ChatMessage::text("system", "Summary of earlier conversation"),
        ChatMessage::text("user", "hi"),
        ChatMessage::text("assistant", "hello"),
        ChatMessage::text("system", "nexus-ui errors: []"),
    ];
    let local = OpenRouter::local("http://127.0.0.1:9931/v1").unwrap();
    let body = local.chat_completions_body("m", &history, &ChatParams::default(), &[], false);
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert_eq!(
        body["messages"][0]["content"],
        "Prompt\n\nSummary of earlier conversation"
    );
    assert_eq!(
        body["messages"][3]["content"],
        "[nexus] nexus-ui errors: []"
    );
    // Already well-formed: untouched. Hosted backends: untouched.
    assert!(single_leading_system(&history[2..4]).is_none());
    let hosted = OpenRouter::from_key_auto("sk-or-test".into());
    let body = hosted.chat_completions_body("m", &history, &ChatParams::default(), &[], false);
    assert_eq!(body["messages"].as_array().unwrap().len(), 5);
}

#[test]
fn codex_catalog_asks_for_the_newer_installed_client_version() {
    let cache = |v: &str| serde_json::json!({ "client_version": v, "models": [] });
    assert_eq!(codex_client_version(None), CODEX_CLIENT_VERSION);
    assert_eq!(codex_client_version(Some(&cache("0.170.2"))), "0.170.2");
    assert_eq!(
        codex_client_version(Some(&cache("0.9.0"))),
        CODEX_CLIENT_VERSION
    );
    assert_eq!(
        codex_client_version(Some(&cache("garbage"))),
        CODEX_CLIENT_VERSION
    );
    assert_eq!(
        codex_client_version(Some(&serde_json::json!({}))),
        CODEX_CLIENT_VERSION
    );
}

#[test]
fn codex_fallback_lists_the_current_models() {
    let models = codex_fallback_models();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-reserve",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ]
    );
    assert_eq!(models[0].context_length, Some(500_000));
    assert!(models.iter().all(|m| {
        m.backend == crate::provider::BackendTag::Codex && !m.reasoning_efforts.is_empty()
    }));
}

#[test]
// One row per live catalog id — the table is the point.
#[allow(clippy::too_many_lines)]
fn opencode_context_fallback_covers_every_live_catalog_id() {
    // The live endpoints serve exactly these ids today; each must have a
    // known context window. When opencode adds a model, add it to the table first.
    let zen_ids = [
        "big-pickle",
        "claude-fable-5",
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-opus-4-5",
        "claude-opus-4-1",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-sonnet-4",
        "claude-haiku-4-5",
        "gemini-3.6-flash",
        "gemini-3.5-flash-lite",
        "gemini-3.5-flash",
        "gemini-3.1-pro",
        "gemini-3-flash",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-5.5-pro",
        "gpt-5.4",
        "gpt-5.4-pro",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.3-codex-spark",
        "gpt-5.3-codex",
        "gpt-5.2",
        "gpt-5.2-codex",
        "gpt-5.1",
        "gpt-5.1-codex-max",
        "gpt-5.1-codex",
        "gpt-5.1-codex-mini",
        "gpt-5",
        "gpt-5-codex",
        "gpt-5-nano",
        "grok-build-0.1",
        "grok-4.5",
        "deepseek-v4-pro",
        "deepseek-v4-flash",
        "glm-5.2",
        "glm-5.1",
        "glm-5",
        "minimax-m3",
        "minimax-m2.7",
        "minimax-m2.5",
        "kimi-k3",
        "kimi-k2.7-code",
        "kimi-k2.6",
        "kimi-k2.5",
        "qwen3.6-plus",
        "qwen3.5-plus",
        "deepseek-v4-flash-free",
        "mimo-v2.5-free",
        "ling-3.0-flash-free",
        "nemotron-3-ultra-free",
        "north-mini-code-free",
        "laguna-s-2.1-free",
        "longcat-2.0-free",
    ];
    let go_ids = [
        "minimax-m3",
        "minimax-m2.7",
        "minimax-m2.5",
        "kimi-k3",
        "kimi-k2.7-code",
        "kimi-k2.6",
        "kimi-k2.5",
        "glm-5.2",
        "glm-5.1",
        "glm-5",
        "deepseek-v4-pro",
        "deepseek-v4-flash",
        "qwen3.7-max",
        "qwen3.8-max",
        "qwen3.7-plus",
        "qwen3.6-plus",
        "qwen3.5-plus",
        "mimo-v2-pro",
        "mimo-v2-omni",
        "mimo-v2.5-pro",
        "mimo-v2.5",
        "hy3",
        "hy3-preview",
        "gpt-5.6-luna",
        "grok-4.5",
    ];
    for id in zen_ids {
        assert!(
            OpenRouter::opencode_context_fallback(OPENCODE_ZEN_BASE, id).is_some(),
            "no context window for Zen general model {id}"
        );
    }
    for id in go_ids {
        assert!(
            OpenRouter::opencode_context_fallback(OPENCODE_GO_BASE, id).is_some(),
            "no context window for Go bundle model {id}"
        );
    }
}

#[test]
fn opencode_context_fallback_differs_per_endpoint_and_ignores_other_flavors() {
    // Same id, different window on each endpoint — the tables must stay
    // separate or Go models get the wrong (smaller) limit.
    assert_eq!(
        OpenRouter::opencode_context_fallback(OPENCODE_ZEN_BASE, "qwen3.6-plus"),
        Some(262_144)
    );
    assert_eq!(
        OpenRouter::opencode_context_fallback(OPENCODE_GO_BASE, "qwen3.6-plus"),
        Some(1_000_000)
    );
    assert_eq!(
        OpenRouter::opencode_context_fallback(OPENCODE_ZEN_BASE, "minimax-m3"),
        Some(512_000)
    );
    assert_eq!(
        OpenRouter::opencode_context_fallback(OPENCODE_GO_BASE, "minimax-m3"),
        Some(1_000_000)
    );
    // Fallback is OpenCode-only: no table for other bases, unknown ids
    // yield nothing, and a go: tag is never part of the lookup.
    assert_eq!(
        OpenRouter::opencode_context_fallback(OPENROUTER_BASE, "deepseek-v4-pro"),
        None
    );
    assert_eq!(
        OpenRouter::opencode_context_fallback(OPENCODE_ZEN_BASE, "nope"),
        None
    );
    assert_eq!(
        OpenRouter::opencode_context_fallback(OPENCODE_GO_BASE, "go:hy3"),
        None
    );
}

#[test]
fn reasoning_efforts_prefer_catalog_metadata_then_use_backend_fallbacks() {
    let entry = |id: &str, parameters: &[&str], efforts: &[&str]| ModelEntry {
        id: id.to_string(),
        name: None,
        supported_parameters: parameters
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
        reasoning: (!efforts.is_empty()).then(|| ModelReasoningEntry {
            supported_efforts: efforts
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        }),
        context_length: None,
        architecture: None,
        pricing: None,
    };

    let or = OpenRouter::openrouter_flavor("k".into());
    // Catalog metadata is authoritative, normalized into UI cycle order,
    // and can expose sparse sets plus explicit disable/max tiers.
    let sparse = entry("google/gemini", &[], &["high", "minimal"]);
    assert_eq!(
        or.tag.reasoning_efforts(&sparse),
        vec![ReasoningEffort::Minimal, ReasoningEffort::High]
    );
    let full = entry(
        "openai/gpt-next",
        &["reasoning"],
        &["max", "xhigh", "high", "medium", "low", "none"],
    );
    assert_eq!(
        or.tag.reasoning_efforts(&full),
        ReasoningEffort::WITH_MAX_XHIGH_AND_NONE.to_vec()
    );

    // Without an enumerated set, OpenRouter still uses the catalog's
    // supported-parameter gate and the conservative family fallback.
    let claude = entry("anthropic/claude-sonnet-4.5", &["reasoning"], &[]);
    assert_eq!(
        or.tag.reasoning_efforts(&claude),
        ReasoningEffort::WITH_MINIMAL.to_vec()
    );
    let claude_no_param = entry("anthropic/claude-sonnet-4.5", &[], &[]);
    assert!(or.tag.reasoning_efforts(&claude_no_param).is_empty());
    let generic = entry("deepseek/reasoner", &["reasoning"], &[]);
    assert_eq!(
        or.tag.reasoning_efforts(&generic),
        ReasoningEffort::STANDARD.to_vec()
    );

    // Direct OpenAI has no rich /models metadata, so known families use
    // concrete fallbacks rather than assigning every GPT-5 the same set.
    let oa = OpenRouter::openai("k".into());
    assert_eq!(
        oa.tag.reasoning_efforts(&entry("gpt-5", &[], &[])),
        ReasoningEffort::WITH_MINIMAL.to_vec()
    );
    assert_eq!(
        oa.tag.reasoning_efforts(&entry("gpt-5-pro", &[], &[])),
        ReasoningEffort::HIGH_ONLY.to_vec()
    );
    assert_eq!(
        oa.tag.reasoning_efforts(&entry("gpt-5.4", &[], &[])),
        ReasoningEffort::WITH_XHIGH_AND_NONE.to_vec()
    );
    let o3 = entry("o3", &[], &[]);
    assert_eq!(
        oa.tag.reasoning_efforts(&o3),
        ReasoningEffort::STANDARD.to_vec()
    );
    let gpt41 = entry("gpt-4.1", &[], &[]);
    assert!(oa.tag.reasoning_efforts(&gpt41).is_empty());

    // Metadata also wins on OpenCode; absent metadata retains its broad
    // compatibility fallback because that catalog is not authoritative.
    let go = OpenRouter::opencode_go("k".into());
    assert_eq!(
        go.tag.reasoning_efforts(&sparse),
        vec![ReasoningEffort::Minimal, ReasoningEffort::High]
    );
    assert_eq!(
        go.tag.reasoning_efforts(&gpt41),
        ReasoningEffort::STANDARD.to_vec()
    );
}

#[test]
fn opencode_go_reports_its_own_backend_base_and_defaults() {
    let p = OpenRouter::opencode_go("k".into());
    assert_eq!(p.backend_tag().name(), "OpenCode Go");
    assert_eq!(p.tag.base(), "https://opencode.ai/zen/v1");
    assert!(!p.default_utility_model().is_empty());
    assert!(!p.default_research_model().is_empty());
    // No embedding models on Go — feature stays disabled by default.
    assert_eq!(p.default_embedding_model(), "");
}

#[tokio::test]
async fn mistral_stream_delivers_typed_thinking_answer_and_usage() {
    let response = fake_chat_frames(&[
        serde_json::json!({"choices": [{"delta": {"content": [
            {"type": "thinking", "thinking": [{"type": "text", "text": "Think."}]},
            {"type": "text", "text": "Hello"}
        ]}}]}),
        serde_json::json!({"choices": [], "usage": {
            "prompt_tokens": 100, "completion_tokens": 12, "total_tokens": 112,
            "prompt_tokens_details": {"cached_tokens": 80},
            "completion_tokens_details": {"reasoning_tokens": 10}
        }}),
    ]);
    let (mock, server) = fake_chat_server(vec![response]).await;
    let mut provider = OpenRouter::mistral("mock-mistral-key".into());
    provider.endpoint = mock.endpoint;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let finish = provider
        .run_stream(
            MISTRAL_VIBE_MODEL,
            &[ChatMessage::text("user", "Hi")],
            &ChatParams {
                prompt_cache_key: Some("session".into()),
                reasoning_effort: Some("high".into()),
                ..ChatParams::default()
            },
            &[],
            false,
            &tx,
        )
        .await
        .unwrap();
    let Finish::Done(output) = finish else {
        panic!("expected a completed Mistral round");
    };
    assert_eq!(output.content, "Hello");
    assert_eq!(output.reasoning.as_deref(), Some("Think."));
    let usage = output.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.completion_tokens, 12);
    assert_eq!(usage.cache_read_tokens, 80);
    assert!(matches!(rx.try_recv().unwrap(), StreamEvent::Reasoning(s) if s == "Think."));
    assert!(matches!(rx.try_recv().unwrap(), StreamEvent::Token(s) if s == "Hello"));
    assert!(matches!(rx.try_recv().unwrap(), StreamEvent::Usage(u) if u.total_tokens == 112));
    let requests = server.await.unwrap();
    assert_eq!(requests[0]["model"], MISTRAL_VIBE_MODEL);
    assert_eq!(requests[0]["reasoning_effort"], "high");
    assert!(requests[0].get("stream_options").is_none());
    assert_eq!(requests[0]["prompt_cache_key"], "session");
}

#[tokio::test]
async fn mistral_helper_completion_keeps_answer_and_cached_usage() {
    let response = serde_json::json!({
        "choices": [{"message": {"content": [
            {"type": "thinking", "thinking": [{"type": "text", "text": "Private thought"}]},
            {"type": "text", "text": "A short title"}
        ]}}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 12, "total_tokens": 112,
                  "prompt_tokens_details": {"cached_tokens": 80}}
    });
    let (mock, server) =
        decider_mock::server(200, response.to_string(), std::time::Duration::ZERO, 1).await;
    let mut provider = OpenRouter::mistral("mock-mistral-key".into());
    provider.endpoint = mock.endpoint;
    let completion = provider
        .post_completion_with_usage(serde_json::json!({
            "model": MISTRAL_VIBE_FAST_MODEL,
            "stream": false,
            "messages": [{"role": "assistant", "content": "Hi", "reasoning_content": "Old"}]
        }))
        .await
        .unwrap();
    assert_eq!(completion.text, "A short title");
    let usage = completion.usage.unwrap();
    assert_eq!(usage.total_tokens, 112);
    assert_eq!(usage.cache_read_tokens, 80);
    let requests = server.await.unwrap();
    assert!(requests[0].0.starts_with("POST /api/v1/chat/completions "));
    assert!(requests[0].0.contains("Bearer mock-mistral-key"));
    assert_eq!(requests[0].1["model"], MISTRAL_VIBE_FAST_MODEL);
    assert!(
        requests[0].1["messages"][0]
            .get("reasoning_content")
            .is_none()
    );
}

#[test]
fn mistral_reports_its_backend_and_vibe_defaults() {
    let p = OpenRouter::mistral("k".into());
    assert_eq!(p.backend_tag(), crate::provider::BackendTag::Mistral);
    assert_eq!(p.tag.base(), "https://api.mistral.ai/v1");
    assert_eq!(p.default_utility_model(), "mistral-vibe-cli-fast");
    assert_eq!(p.default_research_model(), "mistral-vibe-cli-latest");
    assert_eq!(p.default_embedding_model(), "");
    assert_eq!(p.default_image_gen_model(), "");
}

#[test]
fn mistral_catalog_keeps_tool_chat_models_once_and_adds_vibe_names() {
    // Alias groups as the live catalog reported them on 2026-11-09.
    let medium = [
        "mistral-medium-latest",
        "mistral-medium-3-5",
        "magistral-medium-latest",
        "mistral-vibe-cli-latest",
        "mistral-vibe-cli-with-tools",
    ];
    let card = |id: &str, group: &[&str], ctx: u64, reasoning: bool| {
        serde_json::json!({
            "id": id, "max_context_length": ctx,
            "aliases": group.iter().filter(|a| **a != id).collect::<Vec<_>>(),
            "capabilities": {"completion_chat": true, "function_calling": true,
                             "vision": true, "reasoning": reasoning}
        })
    };
    let mut data: Vec<serde_json::Value> = medium
        .iter()
        .map(|id| card(id, &medium, 262_144, true))
        .collect();
    let large = ["mistral-large-latest", "mistral-large-2512"];
    data.extend(large.iter().map(|id| card(id, &large, 262_144, false)));
    let large4 = ["mistral-large-4-0", "mistral-large-4"];
    data.extend(large4.iter().map(|id| card(id, &large4, 1_048_576, true)));
    data.extend([
        serde_json::json!({"id": "codestral-2508", "aliases": [],
         "capabilities": {"completion_chat": true, "function_calling": true}}),
        serde_json::json!({"id": "mistral-embed", "capabilities": {"completion_chat": false}}),
        serde_json::json!({"id": "mistral-ocr-latest",
         "capabilities": {"completion_chat": false, "function_calling": true}}),
        serde_json::json!({"id": "open-mistral-7b", "deprecation": "2026-01-01T00:00:00Z",
         "capabilities": {"completion_chat": true, "function_calling": true}}),
    ]);
    let catalog: MistralModelsResponse =
        serde_json::from_value(serde_json::json!({"object": "list", "data": data})).unwrap();
    let models = parse_mistral_catalog(catalog);
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "codestral-2508",
            "mistral-large-4",
            "mistral-large-latest",
            "mistral-vibe-cli-fast",
            "mistral-vibe-cli-latest"
        ]
    );
    let vibe = &models[4];
    assert_eq!(vibe.context_length, Some(262_144));
    assert!(vibe.supports_images);
    assert_eq!(vibe.reasoning_efforts, ReasoningEffort::HIGH_AND_NONE);
    assert!(models[0].reasoning_efforts.is_empty());
    assert!(models[2].reasoning_efforts.is_empty());
    assert_eq!(models[1].context_length, Some(1_048_576));
    // Missing from this catalog, so the built-in facts fill it in.
    let fast = &models[3];
    assert!(fast.supports_images);
    assert_eq!(fast.reasoning_efforts, ReasoningEffort::HIGH_AND_NONE);
    assert!(
        models
            .iter()
            .all(|m| m.backend == crate::provider::BackendTag::Mistral)
    );
}

#[test]
fn mistral_models_take_the_price_of_their_most_specific_openrouter_twin() {
    use crate::provider::{BackendTag, ModelPricing};
    let price = |prompt: f64| ModelPricing {
        prompt,
        completion: prompt * 2.0,
        cache_read: None,
        cache_write: None,
    };
    let model = |backend, id: &str, aliases: &[&str], pricing| Model {
        id: id.into(),
        name: id.into(),
        reasoning_efforts: Vec::new(),
        context_length: None,
        supports_images: false,
        supports_image_generation: false,
        supports_video_generation: false,
        backend,
        pricing,
        aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
    };
    let router = |id: &str, prompt| model(BackendTag::OpenRouter, id, &[], Some(price(prompt)));
    let mistral = |id: &str, aliases: &[&str]| model(BackendTag::Mistral, id, aliases, None);
    // Live shapes from 2026-11-12: the Medium 3.5 group also answers to the
    // older `mistral-medium-3`, and hosted GLM/DeepSeek spell dots as dashes.
    let mut models = vec![
        router("mistralai/mistral-medium-3", 0.4),
        router("mistralai/mistral-medium-3-5", 1.5),
        router("mistralai/mistral-medium-3-5:batch", 0.75),
        router("z-ai/glm-5.2", 0.7),
        router("deepseek/deepseek-v4.1-flash-1017", 0.02),
        router("~deepseek/deepseek-v4-flash-latest", 9.0),
        mistral(
            "mistral-vibe-cli-latest",
            &[
                "mistral-medium-3",
                "mistral-medium-3-5",
                "mistral-medium-latest",
            ],
        ),
        mistral("glm-5-2", &["zai-glm-5-2"]),
        mistral("deepseek-v4-1-flash-1017", &[]),
        mistral("labs-leanstral-1-5", &[]),
    ];
    price_mistral_models(&mut models);
    let prompt = |id: &str| {
        models
            .iter()
            .find(|m| m.backend == BackendTag::Mistral && m.id == id)
            .and_then(|m| m.pricing)
            .map(|p| p.prompt)
    };
    assert_eq!(prompt("mistral-vibe-cli-latest"), Some(1.5));
    assert_eq!(prompt("glm-5-2"), Some(0.7));
    assert_eq!(prompt("deepseek-v4-1-flash-1017"), Some(0.02));
    assert_eq!(prompt("labs-leanstral-1-5"), None);
}

#[test]
fn mistral_catalog_keeps_each_groups_other_names_as_aliases() {
    let group = ["mistral-large-latest", "mistral-large-2512"];
    let data: Vec<serde_json::Value> = group
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "aliases": group.iter().filter(|a| *a != id).collect::<Vec<_>>(),
                "capabilities": {"completion_chat": true, "function_calling": true}
            })
        })
        .collect();
    let catalog: MistralModelsResponse =
        serde_json::from_value(serde_json::json!({"object": "list", "data": data})).unwrap();
    let models = parse_mistral_catalog(catalog);
    let large = models
        .iter()
        .find(|m| m.id == "mistral-large-latest")
        .unwrap();
    assert_eq!(large.aliases, ["mistral-large-2512"]);
}

#[test]
fn mistral_moves_system_images_to_user_messages_without_losing_context() {
    let image = "data:image/png;base64,AA==";
    let history = [
        ChatMessage::text("system", "Follow the instructions."),
        ChatMessage {
            role: "system".into(),
            content: "The user has these image files in this space: photo.png".into(),
            images: vec![image.into()],
            ..Default::default()
        },
        ChatMessage::text("user", "Describe the photo."),
    ];
    let provider = OpenRouter::mistral("mock-key".into());
    let chat = provider.chat_completions_body(
        MISTRAL_VIBE_MODEL,
        &history,
        &ChatParams::default(),
        &[],
        false,
    );
    let mut gateway = serde_json::json!({"messages": history});
    mistral_gateway_body(&mut gateway);
    for body in [chat, gateway] {
        let messages = body["messages"].as_array().unwrap();
        for message in messages.iter().filter(|m| m["role"] == "system") {
            assert!(
                message["content"]
                    .as_array()
                    .is_none_or(|parts| parts.iter().all(|part| part["type"] != "image_url")),
                "ImageURLChunk cannot be used in system message: {message}"
            );
        }
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["content"], "Follow the instructions.");
        assert_eq!(messages[1]["role"], "system");
        assert_eq!(messages[1]["content"][0]["text"], history[1].content);
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["image_url"]["url"], image);
        assert_eq!(messages[3]["content"], "Describe the photo.");
    }
}

#[test]
fn mistral_system_image_relocation_is_idempotent_and_leaves_other_roles_alone() {
    let image = serde_json::json!({"type": "image_url", "image_url": {"url": "image.png"}});
    let user = serde_json::json!({"role": "user", "content": [image.clone()]});
    let assistant = serde_json::json!({"role": "assistant", "content": "I see it."});
    let mut body = serde_json::json!({"messages": [
        {"role": "system", "content": [image]}, user, assistant
    ]});
    mistral_body(&mut body);
    assert_eq!(body["messages"], serde_json::json!([user, user, assistant]));
    let normalized = body.clone();
    mistral_body(&mut body);
    assert_eq!(body, normalized);
}

#[test]
fn mistral_keeps_only_the_newest_images_within_its_limit() {
    let history: Vec<ChatMessage> = (0..25)
        .map(|i| ChatMessage {
            role: "user".into(),
            content: format!("turn {i}"),
            images: vec![format!("data:image/png;base64,{i}")],
            ..Default::default()
        })
        .collect();
    let provider = OpenRouter::mistral("mock-key".into());
    let mut body = provider.chat_completions_body(
        MISTRAL_VIBE_MODEL,
        &history,
        &ChatParams::default(),
        &[],
        false,
    );
    let images = |body: &serde_json::Value| -> Vec<String> {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .filter(|p| p["type"] == "image_url")
            .map(|p| p["image_url"]["url"].as_str().unwrap().to_string())
            .collect()
    };
    let kept = images(&body);
    // Five over the limit: the oldest block of ten leaves together.
    assert_eq!(kept.len(), 15);
    assert_eq!(kept[0], "data:image/png;base64,10");
    assert_eq!(kept[14], "data:image/png;base64,24");
    let first = &body["messages"][0]["content"];
    assert_eq!(first[0]["text"], "turn 0");
    assert!(first[1]["text"].as_str().unwrap().contains("omitted"));
    let normalized = body.clone();
    mistral_body(&mut body);
    assert_eq!(body, normalized);
}

#[test]
fn mistral_image_eviction_keeps_the_earlier_prefix_stable_between_requests() {
    let provider = OpenRouter::mistral("mock-key".into());
    let body = |count: usize| {
        let history: Vec<ChatMessage> = (0..count)
            .map(|i| ChatMessage {
                role: "user".into(),
                content: format!("turn {i}"),
                images: vec![format!("data:image/png;base64,{i}")],
                ..Default::default()
            })
            .collect();
        provider.chat_completions_body(
            MISTRAL_VIBE_MODEL,
            &history,
            &ChatParams::default(),
            &[],
            false,
        )
    };
    // From 21 to 30 images the same ten leave, so every earlier message is
    // byte-identical from one request to the next (the provider's cached prefix).
    for count in 22..=30 {
        let (before, after) = (body(count - 1), body(count));
        assert_eq!(
            before["messages"].as_array().unwrap()[..count - 1],
            after["messages"].as_array().unwrap()[..count - 1],
            "{count} images"
        );
    }
    let omitted = |count: usize| {
        body(count)["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["content"].to_string().contains("older image omitted"))
            .count()
    };
    assert_eq!(omitted(20), 0);
    assert_eq!(omitted(21), 10);
    assert_eq!(omitted(30), 10);
    assert_eq!(omitted(31), 20);
}

#[test]
fn mistral_body_drops_foreign_fields_and_fits_tool_ids() {
    let tools = [ToolDef {
        name: "read".into(),
        description: "Read.".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }];
    let call = |id: &str| crate::provider::ToolCall {
        id: id.into(),
        name: "read".into(),
        arguments: "{}".into(),
    };
    let messages = [
        ChatMessage::text("user", "Hi"),
        ChatMessage {
            role: "assistant".into(),
            reasoning_content: Some("thought".into()),
            tool_calls: Some(vec![call("call_9f8e7d6c5b4a"), call("Ab3dE6gH9")]),
            ..Default::default()
        },
        ChatMessage {
            role: "tool".into(),
            content: "ok".into(),
            tool_call_id: Some("call_9f8e7d6c5b4a".into()),
            ..Default::default()
        },
        ChatMessage {
            role: "user".into(),
            content: "look".into(),
            images: vec!["data:image/png;base64,AA==".into()],
            cache_breakpoint: true,
            ..Default::default()
        },
    ];
    let params = ChatParams {
        prompt_cache_key: Some("session-1".into()),
        reasoning_effort: Some("high".into()),
        ..ChatParams::default()
    };
    let body = OpenRouter::mistral("k".into()).chat_completions_body(
        "mistral-vibe-cli-latest",
        &messages,
        &params,
        &tools,
        false,
    );
    for key in ["stream_options", "session_id", "usage"] {
        assert!(body.get(key).is_none(), "{key}");
    }
    assert_eq!(body["prompt_cache_key"], "session-1");
    assert_eq!(body["reasoning_effort"], "high");
    let messages = body["messages"].as_array().unwrap();
    assert!(messages[1].get("reasoning_content").is_none());
    let foreign = messages[1]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(foreign.len(), 9);
    assert!(foreign.bytes().all(|b| b.is_ascii_alphanumeric()));
    assert_eq!(messages[2]["tool_call_id"], foreign);
    assert_eq!(messages[1]["tool_calls"][1]["id"], "Ab3dE6gH9");
    assert!(
        messages[3]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|part| part.get("cache_control").is_none())
    );
    assert_eq!(mistral_tool_id("call_9f8e7d6c5b4a"), foreign);
    assert_ne!(mistral_tool_id("call_other"), foreign);
}

#[test]
fn mistral_gateway_body_strips_openai_sdk_defaults() {
    let mut body = serde_json::json!({
        "model": "mistral-vibe-cli-latest",
        "stream": true,
        "stream_options": {"include_usage": true},
        "store": false,
        "max_completion_tokens": 512,
        "messages": [{"role": "tool", "tool_call_id": "call_abc", "content": "ok"}],
    });
    mistral_gateway_body(&mut body);
    assert!(body.get("stream_options").is_none());
    assert!(body.get("store").is_none());
    assert!(body.get("max_completion_tokens").is_none());
    assert_eq!(body["max_tokens"], 512);
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["messages"][0]["tool_call_id"],
        mistral_tool_id("call_abc")
    );
}

#[test]
fn typed_content_chunks_split_into_answer_and_thinking() {
    let data = r#"{"choices":[{"delta":{"content":[
        {"type":"thinking","thinking":[{"type":"text","text":"Let me "},{"type":"text","text":"see."}]},
        {"type":"text","text":"Answer"}]}}]}"#;
    assert_eq!(
        parse_delta(data),
        (Some("Answer".into()), Some("Let me see.".into()))
    );
    let only_thinking =
        r#"{"choices":[{"delta":{"content":[{"type":"thinking","thinking":"hm"}]}}]}"#;
    assert_eq!(parse_delta(only_thinking), (None, Some("hm".into())));
}

#[test]
fn opencode_route_sends_go_tagged_models_to_the_go_base_untagged() {
    let p = OpenRouter::opencode_go("k".into());
    let (base, model) = p.opencode_route("go:deepseek-v4-pro");
    assert_eq!(base, "https://opencode.ai/zen/go/v1");
    assert_eq!(model, "deepseek-v4-pro");
}

#[test]
fn opencode_route_sends_untagged_models_to_zen_general() {
    let p = OpenRouter::opencode_go("k".into());
    let (base, model) = p.opencode_route("deepseek-v4-flash-free");
    assert_eq!(base, "https://opencode.ai/zen/v1");
    assert_eq!(model, "deepseek-v4-flash-free");
}

#[test]
fn opencode_route_is_a_no_op_for_other_flavors() {
    // A "go:"-prefixed id is meaningless outside OpenCode Go — must not
    // be stripped or rerouted for another flavor.
    let p = OpenRouter::openrouter_flavor("k".into());
    let (base, model) = p.opencode_route("go:whatever");
    assert_eq!(base, "https://openrouter.ai/api/v1");
    assert_eq!(model, "go:whatever");
}

#[test]
fn sse_event_data_joins_multiple_data_lines() {
    let block = "event: response.created\ndata: {\"a\":1}\ndata: more\n\n";
    assert_eq!(sse_event_data(block), "{\"a\":1}\nmore");
}

#[test]
fn sse_event_data_ignores_non_data_lines() {
    let block = "id: 5\nevent: ping\n\n";
    assert_eq!(sse_event_data(block), "");
}

#[test]
fn sse_event_data_handles_done_sentinel() {
    let block = "event: done\ndata: [DONE]\n\n";
    assert_eq!(sse_event_data(block), "[DONE]");
}

#[test]
fn truncate_error_body_reports_empty_body_explicitly() {
    assert_eq!(truncate_error_body(""), "(empty body)");
    assert_eq!(truncate_error_body("   \n  "), "(empty body)");
}

#[test]
fn truncate_error_body_passes_short_text_through() {
    assert_eq!(truncate_error_body("  access denied  "), "access denied");
}

#[test]
fn truncate_error_body_caps_long_text_with_ellipsis() {
    let long = "x".repeat(1000);
    let out = truncate_error_body(&long);
    assert!(out.ends_with('…'));
    assert_eq!(out.chars().count(), 301); // 300 chars + the ellipsis marker
}

/// One golden usage payload per provider family, all asserting the same
/// normalized invariant.
#[test]
fn every_provider_family_normalizes_to_one_prompt_convention() {
    struct Case {
        name: &'static str,
        payload: &'static str,
        prompt: u64,
        read: u64,
        write: u64,
        convention: PromptConvention,
        rate: Option<f64>,
    }
    let cases = [
        Case {
            name: "OpenAI / OpenRouter (prompt_tokens includes cached)",
            payload: r#"{"prompt_tokens":1000,"completion_tokens":50,
                "prompt_tokens_details":{"cached_tokens":800}}"#,
            prompt: 1000,
            read: 800,
            write: 0,
            convention: PromptConvention::Inclusive,
            rate: Some(0.8),
        },
        Case {
            name: "Codex Responses (input_tokens includes cached)",
            payload: r#"{"input_tokens":1000,"output_tokens":50,
                "input_tokens_details":{"cached_tokens":250}}"#,
            prompt: 1000,
            read: 250,
            write: 0,
            convention: PromptConvention::Inclusive,
            rate: Some(0.25),
        },
        Case {
            name: "DeepSeek (prompt_tokens = hit + miss)",
            payload: r#"{"prompt_tokens":400,"completion_tokens":10,
                "prompt_cache_hit_tokens":300,"prompt_cache_miss_tokens":100}"#,
            prompt: 400,
            read: 300,
            write: 0,
            convention: PromptConvention::Inclusive,
            rate: Some(0.75),
        },
        Case {
            name: "Anthropic-shaped (input_tokens excludes both buckets)",
            payload: r#"{"input_tokens":100,"output_tokens":20,
                "cache_read_input_tokens":700,"cache_creation_input_tokens":200}"#,
            // The provider's 100 is the fresh remainder; the real prompt
            // is 100 + 700 + 200.
            prompt: 1000,
            read: 700,
            write: 200,
            convention: PromptConvention::NormalizedFromExclusive,
            rate: Some(0.7),
        },
        Case {
            name: "no cache accounting at all",
            payload: r#"{"prompt_tokens":500,"completion_tokens":25}"#,
            prompt: 500,
            read: 0,
            write: 0,
            convention: PromptConvention::Unknown,
            rate: None,
        },
    ];
    for case in cases {
        let value: serde_json::Value =
            serde_json::from_str(case.payload).expect("valid golden payload");
        let usage = parse_usage_value(&value).expect("payload reports usage");
        assert_eq!(usage.prompt_tokens, case.prompt, "prompt: {}", case.name);
        assert_eq!(usage.cache_read_tokens, case.read, "read: {}", case.name);
        assert_eq!(
            usage.cache_creation_tokens, case.write,
            "write: {}",
            case.name
        );
        assert_eq!(
            usage.prompt_convention, case.convention,
            "convention: {}",
            case.name
        );
        assert_eq!(usage.cache_hit_rate(), case.rate, "rate: {}", case.name);
        // The invariant every consumer relies on.
        assert!(
            usage.cache_read_tokens + usage.cache_creation_tokens <= usage.prompt_tokens,
            "cache buckets must be subsets of the prompt total: {}",
            case.name
        );
    }
}

#[test]
fn an_unrecognized_exclusive_shape_is_normalized_not_clamped() {
    // A hypothetical backend using inclusive-looking field names with an
    // exclusive count. The arithmetic contradiction is the giveaway.
    let value = serde_json::json!({
        "prompt_tokens": 10,
        "completion_tokens": 5,
        "cache_read_tokens": 990,
    });
    let usage = parse_usage_value(&value).expect("reports usage");
    assert_eq!(usage.prompt_tokens, 1000);
    assert_eq!(usage.cache_read_tokens, 990);
    assert_eq!(
        usage.prompt_convention,
        PromptConvention::NormalizedFromExclusive
    );
    assert_eq!(usage.cache_hit_rate(), Some(0.99));
}

#[test]
fn merged_usage_keeps_fields_from_one_response() {
    // A later, less complete frame must not donate one field into an
    // earlier frame's numbers and invent a ratio out of the pair.
    let mut slot = None;
    merge_usage(
        &mut slot,
        parse_usage_value(&serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 40,
            "prompt_tokens_details": {"cached_tokens": 900},
        }))
        .unwrap(),
    );
    merge_usage(
        &mut slot,
        parse_usage_value(&serde_json::json!({
            "prompt_tokens": 200,
            "completion_tokens": 5,
            "prompt_tokens_details": {"cached_tokens": 0},
        }))
        .unwrap(),
    );
    let merged = slot.expect("merged usage");
    assert_eq!(merged.prompt_tokens, 1000);
    assert_eq!(merged.cache_read_tokens, 900);
    assert_eq!(merged.cache_hit_rate(), Some(0.9));

    // A trailing cost-only frame annotates the winner without disturbing
    // its counts.
    merge_usage(
        &mut slot,
        parse_usage_value(&serde_json::json!({"cost": "0.02"})).unwrap(),
    );
    let merged = slot.expect("merged usage");
    assert_eq!(merged.prompt_tokens, 1000);
    assert_eq!(merged.cost, Some(0.02));
}

#[test]
fn cache_key_goes_to_the_flavors_that_route_on_it() {
    for flavor in [
        BackendTag::OpencodeGo,
        BackendTag::OpenAi,
        BackendTag::Mistral,
    ] {
        let mut body = serde_json::Map::new();
        flavor.add_prompt_cache_key(&mut body, "session-1");
        assert_eq!(
            body.get("prompt_cache_key"),
            Some(&serde_json::json!("session-1")),
            "{flavor:?} should carry the cache lane key"
        );
    }
    let mut body = serde_json::Map::new();
    BackendTag::OpenRouter.add_prompt_cache_key(&mut body, "session-1");
    assert!(!body.contains_key("prompt_cache_key"));
    assert_eq!(
        serde_json::Value::Object(body),
        serde_json::json!({ "session_id": "session-1" })
    );
    // Codex keeps it in the body and sends it as the session-id header.
    let mut body = serde_json::Map::new();
    BackendTag::Codex.add_prompt_cache_key(&mut body, "session-1");
    assert_eq!(
        body.get("prompt_cache_key"),
        Some(&serde_json::json!("session-1"))
    );
}

#[test]
fn codex_requests_carry_cache_affinity_headers_and_the_turn_state() {
    let headers = codex_cache_headers(
        reqwest::header::HeaderMap::new(),
        Some("session-1:epoch-0"),
        None,
    );
    assert_eq!(headers["session-id"], "session-1:epoch-0");
    assert_eq!(headers["thread-id"], "session-1:epoch-0");
    assert!(
        !headers.contains_key(CODEX_TURN_STATE),
        "none before the first response"
    );
    let headers = codex_cache_headers(
        reqwest::header::HeaderMap::new(),
        Some("session-1:epoch-0"),
        Some("sticky-worker-7"),
    );
    assert_eq!(headers[CODEX_TURN_STATE], "sticky-worker-7");
    // No key, no affinity headers (and a key that isn't a valid header value is dropped).
    assert!(codex_cache_headers(reqwest::header::HeaderMap::new(), None, None).is_empty());
    assert!(
        codex_cache_headers(reqwest::header::HeaderMap::new(), Some("bad\nkey"), None).is_empty()
    );
    // The turn state is set once per turn and shared by its requests.
    let params = ChatParams::default();
    assert!(params.codex_turn_state.set("first".into()).is_ok());
    assert!(
        params
            .clone()
            .codex_turn_state
            .set("second".into())
            .is_err()
    );
    assert_eq!(
        params.codex_turn_state.get().map(String::as_str),
        Some("first")
    );
}

#[test]
fn non_streaming_codex_bodies_keep_the_cache_key() {
    let body = serde_json::json!({
        "model": "gpt-6-luna",
        "messages": [{"role": "user", "content": "summarize"}],
        "prompt_cache_key": "compaction:session-1",
    });
    assert_eq!(
        chat_body_to_codex_body(&body, false, &[])["prompt_cache_key"],
        "compaction:session-1"
    );
}

fn cache_ttl_messages() -> [ChatMessage; 3] {
    [
        ChatMessage {
            cache_breakpoint: true,
            ..ChatMessage::text("system", "Stable")
        },
        ChatMessage {
            cache_breakpoint: true,
            images: vec![
                "data:image/png;base64,AA==".into(),
                "data:image/png;base64,BB==".into(),
            ],
            ..ChatMessage::text("user", "Look")
        },
        ChatMessage::text("assistant", "Answer"),
    ]
}

#[test]
fn cache_ttl_is_opt_in_and_uniform_for_anthropic_requests() {
    assert!(!ChatParams::default().cache_ttl_1h);
    for (provider, model, automatic) in [
        (
            OpenRouter::openrouter_flavor(String::new()),
            "anthropic/claude-sonnet-4",
            true,
        ),
        (
            OpenRouter::opencode_go(String::new()),
            "claude-sonnet-4",
            false,
        ),
    ] {
        for enabled in [false, true] {
            let params = ChatParams {
                cache_ttl_1h: enabled,
                ..ChatParams::default()
            };
            let marker = if enabled {
                serde_json::json!({"type": "ephemeral", "ttl": "1h"})
            } else {
                serde_json::json!({"type": "ephemeral"})
            };
            let mut expected = serde_json::json!({
                "model": model,
                "stream": true,
                "messages": [
                    {"role": "system", "content": [
                        {"type": "text", "text": "Stable", "cache_control": marker}
                    ]},
                    {"role": "user", "content": [
                        {"type": "text", "text": "Look"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA=="}},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,BB=="}, "cache_control": marker}
                    ]},
                    {"role": "assistant", "content": "Answer"}
                ]
            });
            if automatic {
                expected["usage"] = serde_json::json!({"include": true});
                expected["cache_control"] = marker;
            } else {
                expected["stream_options"] = serde_json::json!({"include_usage": true});
            }
            let body =
                provider.chat_completions_body(model, &cache_ttl_messages(), &params, &[], false);
            assert_eq!(
                serde_json::to_vec(&body).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
            assert_eq!(
                serde_json::to_string(&body).unwrap().contains("\"ttl\""),
                enabled
            );
        }
    }
}

#[test]
fn cache_ttl_does_not_add_markers_to_non_anthropic_requests() {
    for provider in [
        OpenRouter::openrouter_flavor(String::new()),
        OpenRouter::openai(String::new()),
        OpenRouter::opencode_go(String::new()),
    ] {
        for model in [
            "openai/gpt-5",
            "gpt-5",
            "google/gemini-2.5-pro",
            "deepseek/deepseek-chat",
        ] {
            let messages = [
                ChatMessage::text("system", "Stable"),
                ChatMessage::text("user", "Hi"),
            ];
            let baseline = provider.chat_completions_body(
                model,
                &messages,
                &ChatParams::default(),
                &[],
                false,
            );
            let params = ChatParams {
                cache_ttl_1h: true,
                ..ChatParams::default()
            };
            let body = provider.chat_completions_body(model, &messages, &params, &[], false);
            // The only extension is OpenAI's own free retention switch.
            let mut expected = baseline;
            if provider.backend_tag() == BackendTag::OpenAi && model.ends_with("gpt-5") {
                expected["prompt_cache_retention"] = serde_json::json!("24h");
            }
            assert_eq!(body, expected);
            let wire = serde_json::to_string(&body).unwrap();
            assert!(!wire.contains("\"cache_control\""));
            assert!(!wire.contains("\"ttl\""));
        }
    }
}

#[tokio::test]
async fn one_shot_requests_apply_the_same_ttl_to_existing_markers() {
    let response = serde_json::to_vec(&serde_json::json!({
        "choices": [{"message": {"content": "Done"}}]
    }))
    .unwrap();
    let (provider, server) = fake_chat_server(vec![response.clone(), response]).await;
    let model = "claude-sonnet-4";
    for enabled in [false, true] {
        let params = ChatParams {
            cache_ttl_1h: enabled,
            ..ChatParams::default()
        };
        let completion = provider
            .complete_with_params(model, cache_ttl_messages().to_vec(), &params)
            .await
            .unwrap();
        assert_eq!(completion.text, "Done");
    }
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    for (request, enabled) in requests.iter().zip([false, true]) {
        let mut expected = serde_json::json!({
            "model": model,
            "messages": cache_ttl_messages(),
            "stream": false,
        });
        if enabled {
            expected["messages"][0]["content"][0]["cache_control"]["ttl"] = serde_json::json!("1h");
            expected["messages"][1]["content"][2]["cache_control"]["ttl"] = serde_json::json!("1h");
        }
        assert_eq!(request, &expected);
        assert_eq!(
            serde_json::to_string(request).unwrap().contains("\"ttl\""),
            enabled
        );
    }
}

#[test]
fn auto_cache_control_goes_only_to_openrouter_anthropic() {
    for model in [
        "anthropic/claude-sonnet-4",
        "anthropic/claude-3.7-sonnet:thinking",
    ] {
        let mut body = serde_json::Map::new();
        BackendTag::OpenRouter.add_auto_cache_control(&mut body, model);
        assert_eq!(
            body.get("cache_control"),
            Some(&serde_json::json!({ "type": "ephemeral" })),
            "{model} should carry the automatic breakpoint"
        );
    }
    let mut body = serde_json::Map::new();
    BackendTag::OpenRouter.add_auto_cache_control(&mut body, "openai/gpt-5");
    assert!(body.is_empty());
    for flavor in [
        BackendTag::OpenAi,
        BackendTag::OpencodeGo,
        BackendTag::Codex,
    ] {
        let mut body = serde_json::Map::new();
        flavor.add_auto_cache_control(&mut body, "claude-sonnet-4");
        assert!(body.is_empty(), "{flavor:?} should not carry it");
    }
}

#[test]
fn empty_breakpoint_message_serializes_as_plain_string() {
    let mut message = ChatMessage {
        role: "assistant".into(),
        cache_breakpoint: true,
        tool_calls: Some(vec![ToolCall {
            id: "call-1".into(),
            name: "lookup".into(),
            arguments: "{}".into(),
        }]),
        ..Default::default()
    };
    let empty_wire = serde_json::to_value(&message).expect("serializes");
    assert_eq!(empty_wire["content"], serde_json::json!(""));
    assert_eq!(
        empty_wire["tool_calls"],
        serde_json::json!([{
            "id": "call-1",
            "type": "function",
            "function": { "name": "lookup", "arguments": "{}" },
        }])
    );

    message.content = "hi".into();
    let wire = serde_json::to_value(&message).expect("serializes");
    assert_eq!(
        wire["content"],
        serde_json::json!([{
            "type": "text",
            "text": "hi",
            "cache_control": { "type": "ephemeral" },
        }])
    );
    assert_eq!(wire["tool_calls"], empty_wire["tool_calls"]);
}

#[test]
fn extracts_content_delta() {
    let data = r#"{"choices":[{"delta":{"content":"Hel"}}]}"#;
    let (content, reasoning) = parse_delta(data);
    assert_eq!(content.as_deref(), Some("Hel"));
    assert_eq!(reasoning, None);
}

#[test]
fn extracts_reasoning_delta() {
    let data = r#"{"choices":[{"delta":{"reasoning":"Let me think"}}]}"#;
    let (content, reasoning) = parse_delta(data);
    assert_eq!(content, None);
    assert_eq!(reasoning.as_deref(), Some("Let me think"));
}

#[test]
fn extracts_reasoning_content_delta() {
    let data = r#"{"choices":[{"delta":{"reasoning_content":"Keep this on replay"}}]}"#;
    let (content, reasoning) = parse_delta(data);
    assert_eq!(content, None);
    assert_eq!(reasoning.as_deref(), Some("Keep this on replay"));
}

#[test]
fn empty_delta_yields_none() {
    let data = r#"{"choices":[{"delta":{"role":"assistant"}}]}"#;
    assert_eq!(parse_delta(data), (None, None));
}

#[test]
fn junk_yields_none() {
    assert_eq!(parse_delta("not json"), (None, None));
}

#[test]
fn parses_usage() {
    let data =
        r#"{"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":40,"total_tokens":160}}"#;
    let u = parse_usage(data).unwrap();
    assert_eq!(u.prompt_tokens, 120);
    assert_eq!(u.completion_tokens, 40);
    assert_eq!(u.total_tokens, 160);
    assert_eq!(u.cache_read_tokens, 0);
    assert_eq!(u.cache_creation_tokens, 0);
    assert!(parse_usage(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).is_none());
}

#[test]
fn merging_usage_never_reduces_counts_within_a_request() {
    let mut slot = Some(Usage {
        prompt_tokens: 120,
        completion_tokens: 40,
        total_tokens: 160,
        cache_read_tokens: 100,
        cache_creation_tokens: 5,
        prompt_convention: Default::default(),
        cost: None,
    });
    merge_usage(
        &mut slot,
        Usage {
            prompt_tokens: 80,
            completion_tokens: 20,
            total_tokens: 100,
            cache_read_tokens: 60,
            cache_creation_tokens: 2,
            prompt_convention: Default::default(),
            cost: Some(0.01),
        },
    );
    let u = slot.unwrap();
    assert_eq!(u.prompt_tokens, 120);
    assert_eq!(u.completion_tokens, 40);
    assert_eq!(u.total_tokens, 160);
    assert_eq!(u.cache_read_tokens, 100);
    assert_eq!(u.cache_creation_tokens, 5);
    assert_eq!(u.cost, Some(0.01));
}

#[test]
fn parses_usage_cache_tokens_both_styles() {
    // OpenRouter/OpenAI style: cache reads and writes nested under
    // prompt_tokens_details, alongside the provider's exact request cost.
    let openai = r#"{"usage":{"prompt_tokens":100,"prompt_tokens_details":{"cached_tokens":70,"cache_write_tokens":20},"completion_tokens":10,"total_tokens":110,"cost":0.00024}}"#;
    let u = parse_usage(openai).unwrap();
    assert_eq!(u.cache_read_tokens, 70);
    assert_eq!(u.cache_creation_tokens, 20);
    assert_eq!(u.cache_hit_rate(), Some(0.7));
    assert_eq!(u.cost, Some(0.00024));
    // Anthropic-style: flat cache fields (OpenRouter passthrough).
    let anthropic = r#"{"usage":{"prompt_tokens":100,"cache_read_input_tokens":40,"cache_creation_input_tokens":10,"completion_tokens":10,"total_tokens":110}}"#;
    let u = parse_usage(anthropic).unwrap();
    assert_eq!(u.cache_read_tokens, 40);
    assert_eq!(u.cache_creation_tokens, 10);
    assert_eq!(u.cache_hit_rate(), Some(0.4));
}

#[test]
fn deepseek_style_cache_hit_tokens_parsed() {
    // OpenCode Zen reports cached input as flat prompt_cache_hit_tokens
    // (DeepSeek-style) plus the OpenAI-style nested field — captured from
    // the real zen/go/v1 finish chunk.
    let data = r#"{"choices":[],"usage":{"prompt_tokens":6,"completion_tokens":28,"total_tokens":34,"prompt_cache_hit_tokens":2,"prompt_cache_miss_tokens":4,"prompt_tokens_details":{"cached_tokens":1},"completion_tokens_details":{"reasoning_tokens":26}}}"#;
    let u = parse_usage(data).unwrap();
    assert_eq!(u.cache_read_tokens, 2);
    assert_eq!(u.cache_hit_rate(), Some(2.0 / 6.0));
}

#[test]
fn null_or_zero_usage_is_not_an_event() {
    // OpenCode Zen echoes `"usage":null` on every chunk; the app must
    // not emit (and log) a zero-usage event per chunk.
    let null_usage = r#"{"id":"x","choices":[{"index":0,"delta":{"content":null}}],"usage":null}"#;
    assert!(parse_usage(null_usage).is_none());
    // All-zero usage objects are equally uninformative.
    let zero =
        r#"{"choices":[],"usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}}"#;
    assert!(parse_usage(zero).is_none());
}

#[test]
fn stream_cost_parses_string_dollars() {
    // The trailing OpenCode Zen chunk: `{"choices":[],"cost":"0"}`.
    assert_eq!(parse_stream_cost(r#"{"choices":[],"cost":"0"}"#), Some(0.0));
    assert_eq!(
        parse_stream_cost(r#"{"choices":[],"cost":"0.0012"}"#),
        Some(0.0012)
    );
    // No cost field -> None (OpenRouter/OpenAI/Codex trailing chunks).
    assert!(parse_stream_cost(r#"{"choices":[]}"#).is_none());
    assert!(parse_stream_cost(r#"{"choices":[],"usage":{"prompt_tokens":1}}"#).is_none());
}

#[test]
fn codex_usage_reads_cached_input_and_cache_writes() {
    let data = r#"{"response":{"usage":{"input_tokens":50,"output_tokens":5,"input_tokens_details":{"cached_tokens":30,"cache_write_tokens":12}}}}"#;
    let u = codex_usage(data).unwrap();
    assert_eq!(u.prompt_tokens, 50);
    assert_eq!(u.cache_read_tokens, 30);
    assert_eq!(u.cache_creation_tokens, 12);
    assert_eq!(u.cache_hit_rate(), Some(0.6));
}

#[test]
fn accumulates_tool_call_fragments_across_chunks() {
    let mut acc = BTreeMap::new();
    accumulate_tool_calls(
        &mut acc,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"web_search","arguments":""}}]}}]}"#,
    );
    accumulate_tool_calls(
        &mut acc,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\""}}]}}]}"#,
    );
    accumulate_tool_calls(
        &mut acc,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"rust\"}"}}]}}]}"#,
    );
    let call = acc.get(&0).unwrap();
    assert_eq!(call.id, "call_1");
    assert_eq!(call.name, "web_search");
    assert_eq!(call.arguments, r#"{"query":"rust"}"#);
}

#[test]
fn accumulates_multiple_parallel_tool_calls_by_index() {
    let mut acc = BTreeMap::new();
    accumulate_tool_calls(
        &mut acc,
        r#"{"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"a","function":{"name":"skill","arguments":"{}"}},
            {"index":1,"id":"b","function":{"name":"web_search","arguments":"{}"}}
        ]}}]}"#,
    );
    assert_eq!(acc.len(), 2);
    assert_eq!(acc[&0].name, "skill");
    assert_eq!(acc[&1].name, "web_search");
}

#[test]
fn codex_input_preserves_assistant_content_with_parallel_function_calls() {
    let messages = vec![
        ChatMessage {
            role: "assistant".into(),
            content: "I will check both sources.".into(),
            tool_calls: Some(vec![
                ToolCall {
                    id: "call_a".into(),
                    name: "first".into(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "call_b".into(),
                    name: "second".into(),
                    arguments: r#"{"value":2}"#.into(),
                },
            ]),
            ..Default::default()
        },
        ChatMessage {
            role: "tool".into(),
            content: "first result".into(),
            tool_call_id: Some("call_a".into()),
            ..Default::default()
        },
        ChatMessage {
            role: "tool".into(),
            content: "second result".into(),
            tool_call_id: Some("call_b".into()),
            ..Default::default()
        },
    ];

    let input = codex_input(&messages);
    assert_eq!(input.len(), 5);
    assert_eq!(input[0]["role"], "assistant");
    assert_eq!(input[0]["content"][0]["text"], "I will check both sources.");
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "call_a");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_b");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_a");
    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[4]["call_id"], "call_b");
}

#[test]
fn request_body_omits_tools_key_when_empty() {
    let body =
        serde_json::json!({ "model": "m", "messages": Vec::<ChatMessage>::new(), "stream": true });
    assert!(body.get("tools").is_none());
}

#[test]
fn harmony_spans_route_to_tokens_reasoning_and_tool_calls() {
    // Only local endpoints demux; hosted ones stream content verbatim.
    assert!(
        OpenRouter::local("http://localhost:8080/v1")
            .unwrap()
            .is_local()
    );
    assert!(!OpenRouter::openai("k".into()).is_local());

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut content = String::new();
    let mut reasoning = String::new();
    // A call the runtime *did* parse: the synthetic one lands after it.
    let mut calls = BTreeMap::from([(
        0,
        ToolCall {
            id: "parsed".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        },
    )]);
    route_harmony(
        vec![
            Piece::Reasoning("thinking".into()),
            Piece::Content("Hello.".into()),
            Piece::ToolCall {
                name: "grep".into(),
                arguments: r#"{"q":"x"}"#.into(),
            },
        ],
        &mut content,
        &mut reasoning,
        &mut calls,
        &tx,
    );
    assert_eq!(content, "Hello.");
    assert_eq!(reasoning, "thinking");
    let names: Vec<&str> = calls.values().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["read_file", "grep"]);
    assert_eq!(calls[&1].id, "harmony-1");
    assert!(matches!(rx.try_recv(), Ok(StreamEvent::Reasoning(t)) if t == "thinking"));
    assert!(matches!(rx.try_recv(), Ok(StreamEvent::Token(t)) if t == "Hello."));
    assert!(rx.try_recv().is_err());
}

#[test]
fn parses_input_modalities_into_supports_images() {
    let json = r#"{"data":[
        {"id":"a/vision","architecture":{"input_modalities":["text","image"]}},
        {"id":"b/text","architecture":{"input_modalities":["text"]}},
        {"id":"c/legacy"}
    ]}"#;
    let resp: ModelsResponse = serde_json::from_str(json).unwrap();
    let flags: Vec<bool> = resp.data.iter().map(entry_supports_images).collect();
    assert_eq!(flags, vec![true, false, false]);
}

#[test]
fn catalog_pricing_scales_per_token_to_per_million() {
    // The API reports USD per token (gpt-5 → 1.25e-06 = $1.25/M); the
    // rest of the codebase prices in USD per 1M tokens, so parsing must
    // scale. Cache rates are preserved and free-tier entries stay zero.
    let p = CatalogPricing {
        prompt: Some("8e-08".into()),
        completion: Some("1.8e-07".into()),
        input_cache_read: Some("1.6e-08".into()),
        input_cache_write: Some("1e-07".into()),
    };
    let scaled = p.usd_per_million().unwrap();
    assert!((scaled.prompt - 0.08).abs() < 1e-12);
    assert!((scaled.completion - 0.18).abs() < 1e-12);
    assert!((scaled.cache_read.unwrap() - 0.016).abs() < 1e-12);
    assert!((scaled.cache_write.unwrap() - 0.1).abs() < 1e-12);
    let free = CatalogPricing {
        prompt: Some("0".into()),
        completion: Some("0".into()),
        input_cache_read: None,
        input_cache_write: None,
    };
    assert_eq!(
        free.usd_per_million(),
        Some(ModelPricing {
            prompt: 0.0,
            completion: 0.0,
            cache_read: None,
            cache_write: None,
        })
    );
    let missing = CatalogPricing {
        prompt: None,
        completion: None,
        input_cache_read: None,
        input_cache_write: None,
    };
    assert_eq!(missing.usd_per_million(), None);
}

#[test]
fn defaults_use_current_quality_generation_models() {
    let provider = OpenRouter::openrouter_flavor("key".into());
    assert_eq!(provider.default_image_gen_model(), "openai/gpt-image-2");
    assert_eq!(provider.default_video_gen_model(), "google/veo-3.1");
}

#[test]
fn image_size_maps_to_normalized_generation_capabilities() {
    assert_eq!(OpenRouter::image_aspect_ratio("1024x1024"), "1:1");
    assert_eq!(OpenRouter::image_aspect_ratio("1024x1792"), "9:16");
    assert_eq!(OpenRouter::image_aspect_ratio("1792x1024"), "16:9");
    assert_eq!(OpenRouter::image_resolution("1792x1024"), "1K");
    assert_eq!(OpenRouter::image_resolution("2048x2048"), "2K");
}

#[test]
fn normalizes_model_specific_video_defaults() {
    let (_, resolution, aspect_ratio) =
        normalize_video_params("minimax/hailuo-3", 8, "720p", "16:9");
    assert_eq!(resolution, "2K");
    assert_eq!(aspect_ratio, "16:9");

    let (duration, resolution, _) = normalize_video_params("openai/sora-2-pro", 6, "720p", "16:9");
    assert_eq!(duration, 4);
    assert_eq!(resolution, "720p");
}

/// `write`/`read` calls on the `path` argument; `ms` sets how long one
/// takes, so calls can finish out of order.
struct PathToolExecutor;

impl ToolExecutor for PathToolExecutor {
    fn defs(&self) -> Vec<ToolDef> {
        Vec::new()
    }

    fn is_read_only(&self, name: &str, _args: &str) -> bool {
        name == "read"
    }

    fn effect(&self, name: &str, args: &str) -> crate::tools::Effect {
        let v: serde_json::Value = serde_json::from_str(args).unwrap();
        let path = std::path::PathBuf::from(v["path"].as_str().unwrap());
        if name == "read" {
            crate::tools::Effect {
                reads: vec![path],
                ..crate::tools::Effect::read_only()
            }
        } else {
            crate::tools::Effect {
                writes: vec![path],
                ..crate::tools::Effect::read_only()
            }
        }
    }

    fn run<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (String, String)> + Send + 'a>> {
        Box::pin(async move {
            let v: serde_json::Value = serde_json::from_str(args).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(v["ms"].as_u64().unwrap())).await;
            (
                format!("{name} {}", v["path"].as_str().unwrap()),
                format!("Running {name}…"),
            )
        })
    }

    fn supports_images(&self) -> bool {
        false
    }

    fn space_files_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
}

#[tokio::test]
async fn scheduled_tool_calls_report_and_reply_in_call_order() {
    let calls = [
        ("c0", "write", r#"{"path":"/w/a","ms":60}"#),
        ("c1", "read", r#"{"path":"/w/b","ms":1}"#),
        ("c2", "write", r#"{"path":"/w/a","ms":1}"#),
        ("c3", "read", r#"{"path":"/w/c","ms":30}"#),
    ];
    let tool_calls: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (id, name, arguments))| {
            serde_json::json!({"index": index, "id": id, "type": "function", "function": {
                "name": name, "arguments": arguments
            }})
        })
        .collect();
    let first = fake_chat_frames(&[
        serde_json::json!({"choices": [{"delta": {"tool_calls": tool_calls}}]}),
    ]);
    let second =
        fake_chat_frames(&[serde_json::json!({"choices": [{"delta": {"content": "Done"}}]})]);
    let (provider, server) = fake_chat_server(vec![first, second]).await;
    let executor = Arc::new(PathToolExecutor);
    let (mut rx, _) = provider.stream_chat(
        "test".into(),
        vec![ChatMessage::text("user", "Go")],
        ChatParams::default(),
        Vec::new(),
        executor,
        2,
    );
    let mut reported = Vec::new();
    while let Some(event) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        match event {
            StreamEvent::Error(error) => panic!("stream failed: {error}"),
            StreamEvent::ToolCall {
                id, name, result, ..
            } => reported.push((id, name, result)),
            _ => {}
        }
    }
    let expected: Vec<_> = calls
        .iter()
        .map(|(id, name, arguments)| {
            let path = serde_json::from_str::<serde_json::Value>(arguments).unwrap()["path"]
                .as_str()
                .unwrap()
                .to_string();
            (
                (*id).to_string(),
                (*name).to_string(),
                format!("{name} {path}"),
            )
        })
        .collect();
    assert_eq!(reported, expected);
    let requests = server.await.unwrap();
    let replies: Vec<_> = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| {
            (
                message["tool_call_id"].as_str().unwrap().to_string(),
                message["content"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        replies,
        expected
            .into_iter()
            .map(|(id, _, result)| (id, result))
            .collect::<Vec<_>>()
    );
}

fn lifetime_fields_in(body: &serde_json::Value) -> Vec<(&'static str, serde_json::Value)> {
    ["prompt_cache_retention", "prompt_cache_options"]
        .into_iter()
        .filter_map(|field| body.get(field).map(|value| (field, value.clone())))
        .collect()
}

#[test]
fn cache_lifetime_fields_follow_backend_and_model_family() {
    let messages = [ChatMessage::text("user", "hi")];
    let params = |extended, long| ChatParams {
        cache_ttl_1h: extended,
        cache_long_openai: long,
        ..ChatParams::default()
    };
    let chat = |provider: &OpenRouter, model: &str, params: &ChatParams| {
        lifetime_fields_in(&provider.chat_completions_body(model, &messages, params, &[], false))
    };
    let responses = |provider: &OpenRouter, model: &str, params: &ChatParams| {
        lifetime_fields_in(&provider.codex_stream_body(model, &messages, params, &[], false))
    };
    let retention = vec![("prompt_cache_retention", serde_json::json!("24h"))];
    let long = vec![("prompt_cache_options", serde_json::json!({ "ttl": "12h" }))];

    let openai = OpenRouter::openai("k".into());
    assert_eq!(chat(&openai, "gpt-5.5", &params(true, false)), retention);
    assert!(chat(&openai, "gpt-5.5", &params(false, false)).is_empty());
    // GPT-5.6 and later keep the 30 minute default, even with the paid option:
    // chat completions has no such field.
    for model in ["gpt-5.6", "gpt-6.1-sol"] {
        assert!(
            chat(&openai, model, &params(true, true)).is_empty(),
            "{model}"
        );
    }
    assert!(chat(&openai, "mystery-model", &params(true, true)).is_empty());

    let codex = OpenRouter::openai_codex("k".into());
    assert_eq!(
        responses(&codex, "gpt-5.5", &params(true, false)),
        retention
    );
    assert!(responses(&codex, "gpt-5.5", &params(false, true)).is_empty());
    assert!(responses(&codex, "gpt-5.6", &params(true, false)).is_empty());
    assert_eq!(responses(&codex, "gpt-5.6", &params(false, true)), long);
    assert_eq!(responses(&codex, "gpt-6.1-sol", &params(true, true)), long);
    assert!(responses(&codex, "mystery-model", &params(true, true)).is_empty());

    for provider in [
        OpenRouter::with_key(BackendTag::Mistral, "k".into()),
        OpenRouter::opencode_go("k".into()),
        OpenRouter::openrouter_flavor("k".into()),
        OpenRouter::local("http://127.0.0.1:1/v1").unwrap(),
    ] {
        assert!(chat(&provider, "gpt-5.5", &params(true, true)).is_empty());
    }
}

#[tokio::test]
async fn a_400_naming_a_lifetime_field_is_retried_once_without_it_for_good() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let ok = fake_chat_frames(&[serde_json::json!({"choices": [{"delta": {"content": "hi"}}]})]);
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for index in 0..3 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(stream.read_line(&mut line).await.unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.unwrap();
            requests.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap());
            let (status, mime, payload) = if index == 0 {
                (
                    "400 Bad Request",
                    "application/json",
                    br#"{"error":{"message":"Unknown parameter: 'prompt_cache_retention'."}}"#
                        .to_vec(),
                )
            } else {
                ("200 OK", "text/event-stream", ok.clone())
            };
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            stream
                .get_mut()
                .write_all(headers.as_bytes())
                .await
                .unwrap();
            stream.get_mut().write_all(&payload).await.unwrap();
            stream.get_mut().shutdown().await.unwrap();
        }
        requests
    });
    let mut provider = OpenRouter::openai("k".into());
    provider.endpoint = Some(endpoint);
    let params = ChatParams {
        cache_ttl_1h: true,
        ..ChatParams::default()
    };
    let model = "gpt-5.4-retry-drop-test";
    let mut notices = Vec::new();
    for _ in 0..2 {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let finish = provider
            .run_stream(
                model,
                &[ChatMessage::text("user", "Hi")],
                &params,
                &[],
                false,
                &tx,
            )
            .await
            .unwrap();
        assert!(matches!(finish, Finish::Done(ref output) if output.content == "hi"));
        drop(tx);
        while let Some(event) = rx.recv().await {
            if let StreamEvent::Notice(text) = event {
                notices.push(text);
            }
        }
    }
    let requests = server.await.unwrap();
    assert_eq!(requests[0]["prompt_cache_retention"], "24h");
    assert!(requests[1].get("prompt_cache_retention").is_none());
    assert!(requests[2].get("prompt_cache_retention").is_none());
    assert_eq!(
        notices,
        ["OpenAI rejected prompt_cache_retention; continuing without it"],
        "announced once"
    );
}

#[test]
fn opencode_names_the_session_in_a_header_and_other_backends_do_not() {
    let body = serde_json::json!({"model": "m", "prompt_cache_key": "session-1:0"});
    assert_eq!(
        opencode_session(BackendTag::OpencodeGo, &body),
        Some("session-1:0")
    );
    assert_eq!(opencode_session(BackendTag::Mistral, &body), None);
    assert_eq!(
        opencode_session(BackendTag::OpencodeGo, &serde_json::json!({"model": "m"})),
        None
    );
    let provider = OpenRouter::opencode_go("mock-key".into());
    let request = provider
        .chat_request("http://127.0.0.1:9/chat/completions".into(), &body)
        .build()
        .unwrap();
    assert_eq!(request.headers()["x-opencode-session"], "session-1:0");
}
