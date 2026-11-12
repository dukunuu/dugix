use super::*;

#[test]
fn constant_time_compare_checks_length_and_bytes() {
    assert!(constant_time_eq(b"token", b"token"));
    assert!(!constant_time_eq(b"token", b"wrong"));
    assert!(!constant_time_eq(b"token", b"token-long"));
}

#[test]
fn backend_override_accepts_supported_spellings() {
    assert_eq!(parse_backend_tag("openrouter"), Ok(BackendTag::OpenRouter));
    assert_eq!(parse_backend_tag("OpenCode-Go"), Ok(BackendTag::OpencodeGo));
    assert_eq!(parse_backend_tag("Mistral"), Ok(BackendTag::Mistral));
    assert_eq!(parse_backend_tag("vibe"), Ok(BackendTag::Mistral));
    assert!(parse_backend_tag("wat").is_err());
}

#[test]
fn query_token_is_removed_before_app_proxy() {
    assert_eq!(query_without_token("token=a&x=1&token=b"), "x=1");
}

#[test]
fn tool_rpc_uses_standard_request_and_response_envelopes() {
    let request = parse_tool_rpc_request(
        br#"{"jsonrpc":"2.0","id":7,"method":"tools/run","params":{"name":"read_file","arguments":{"path":"notes.md"}}}"#,
    )
    .expect("valid tool JSON-RPC request");
    assert_eq!(request.id, serde_json::json!(7));
    assert_eq!(request.name, "read_file");
    assert_eq!(request.args, serde_json::json!({"path": "notes.md"}));

    let response = serde_json::to_value(ToolRpcResponse::success(
        request.id,
        ToolRunResult {
            result: "ok".into(),
            label: "read_file".into(),
        },
    ))
    .expect("serializes tool JSON-RPC response");
    assert_eq!(
        response,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {"result": "ok", "label": "read_file"}
        })
    );
}

#[test]
fn tool_rpc_errors_keep_request_ids() {
    let error =
        parse_tool_rpc_request(br#"{"jsonrpc":"2.0","id":"abc","method":"unknown","params":{}}"#)
            .expect_err("unknown method must fail");
    assert_eq!(error.id, serde_json::json!("abc"));
    assert_eq!(error.error.code, -32601);
    let response = serde_json::to_value(ToolRpcResponse::error(error.id, error.error))
        .expect("serializes tool JSON-RPC error");
    assert_eq!(response["id"], "abc");
    assert_eq!(response["error"]["code"], -32601);
    assert!(response.get("result").is_none());
}

#[test]
fn usage_observer_reads_openai_and_cache_fields() {
    let mut usage = GatewayUsage::default();
    usage.observe(
        br#"{"usage":{"prompt_tokens":10,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":3},"cost":"0.01"}}"#,
        false,
    );
    usage.finish(false);
    let usage = usage.into_usage();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 4);
    assert_eq!(usage.cache_read_tokens, 3);
    assert_eq!(usage.cost, Some(0.01));
    assert_eq!(usage.cache_hit_rate(), Some(0.3));
}

#[test]
fn usage_survives_a_chunk_boundary_inside_the_sse_frame() {
    let frame = br#"data: {"usage":{"prompt_tokens":10,"completion_tokens":4}}"#;
    let (head, tail) = frame.split_at(20);
    let mut usage = GatewayUsage::default();
    usage.observe(head, true);
    usage.observe(tail, true);
    usage.finish(true);
    let usage = usage.into_usage();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 4);
}

#[test]
fn wire_event_redacts_provider_and_host_credentials() {
    let mut app = test_app();
    app.saved.openai_key = Some("provider-secret".into());
    app.saved.mistral_key = Some("mistral-secret".into());
    let event = redact_wire_event(
        WireEvent::Status("provider-secret mistral-secret host-secret".into()),
        &app,
        "host-secret",
    );
    let json = serde_json::to_string(&event).unwrap();
    assert!(!json.contains("provider-secret"));
    assert!(!json.contains("mistral-secret"));
    assert!(!json.contains("host-secret"));
    assert!(json.contains("[redacted]"));
}

#[test]
fn fresh_codex_login_never_reaches_host_redaction_with_credentials() {
    // The event is converted before the actor applies it to `App`, so the
    // newly-issued credentials are not present in `app.saved` yet. The
    // wire conversion must therefore remove them structurally rather than
    // relying on the later string-redaction backstop.
    let app = test_app();
    let event = AppEvent::Login(Some(crate::app::LoginMsg::Done(Ok(
        crate::config::CodexCredentials {
            access: "fresh-access-secret".into(),
            refresh: "fresh-refresh-secret".into(),
            expires: 123,
            account_id: "account".into(),
        },
    ))));
    let wire = redact_wire_event(WireEvent::from(event), &app, "host-secret");
    let json = serde_json::to_string(&wire).expect("serializes login event");
    assert!(!json.contains("fresh-access-secret"));
    assert!(!json.contains("fresh-refresh-secret"));
    assert_eq!(
        wire,
        WireEvent::Login(Some(crate::host::wire::WireLoginMsg::Done(Ok(()))))
    );
}

#[test]
fn mistral_routes_through_the_gateway_with_its_own_key() {
    let mut app = test_app();
    app.saved.mistral_key = Some("mock-mistral-key".into());
    app.backends.set(
        BackendTag::Mistral,
        openrouter::OpenRouter::mistral("mock-mistral-key".into()),
    );
    app.models = vec![test_model("mistral-vibe-cli-latest", BackendTag::Mistral)];
    let wire = OpenAiModel::from(WireModel::from(app.models[0].clone()));
    assert_eq!(wire.id, "mistral:mistral-vibe-cli-latest");
    assert_eq!(wire.owned_by, "mistral");
    for (model, override_tag) in [
        ("mistral-vibe-cli-latest", None),
        ("mistral:mistral-vibe-cli-latest", None),
        ("mistral:mistral-vibe-cli-latest", Some(BackendTag::Mistral)),
    ] {
        let route = gateway_route(&app, model, override_tag).expect("Mistral model routes");
        assert_eq!(route.tag, BackendTag::Mistral);
        assert_eq!(route.model, "mistral-vibe-cli-latest");
        assert_eq!(route.key, "mock-mistral-key");
    }
    app.saved.mistral_key = None;
    assert!(gateway_route(&app, "mistral-vibe-cli-latest", None).is_err());
}

#[test]
fn codex_routes_through_the_gateway_like_every_other_backend() {
    let mut app = test_app();
    app.saved.codex = Some(crate::config::CodexCredentials {
        access: "codex-access".into(),
        refresh: "codex-refresh".into(),
        expires: 0,
        account_id: "codex-account".into(),
    });
    app.backends.set(
        BackendTag::Codex,
        openrouter::OpenRouter::openai_codex("codex-access".into()),
    );
    app.models = vec![test_model("gpt-5-codex", BackendTag::Codex)];

    // Resolvable by raw id, by composite id, and by the public
    // `codex:`-prefixed id `/v1/models` advertises.
    for model in ["gpt-5-codex", "codex:gpt-5-codex"] {
        let route = gateway_route(&app, model, None).expect("codex model routes");
        assert_eq!(route.tag, BackendTag::Codex);
        assert_eq!(route.model, "gpt-5-codex");
        assert_eq!(route.account_id.as_deref(), Some("codex-account"));
    }

    // An explicit `x-nexus-backend: codex` override routes too.
    let route =
        gateway_route(&app, "codex:gpt-5-codex", Some(BackendTag::Codex)).expect("override");
    assert_eq!(route.model, "gpt-5-codex");
}

fn test_model(id: &str, backend: BackendTag) -> crate::provider::Model {
    crate::provider::Model {
        id: id.to_string(),
        name: id.to_string(),
        reasoning_efforts: Vec::new(),
        context_length: None,
        supports_images: false,
        supports_image_generation: false,
        supports_video_generation: false,
        backend,
        pricing: None,
    }
}

fn test_app() -> App {
    let root = std::env::temp_dir().join(format!("nexus-host-test-{}", uuid::Uuid::new_v4()));
    App::new(
        crate::db::Db::open_in_memory().expect("in-memory db"),
        Some("sk-host-test"),
        crate::space::Space { root },
    )
}

fn request(method: &str, target: &str, token: Option<&str>, body: &[u8]) -> Vec<u8> {
    let mut request = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\n").into_bytes();
    if let Some(token) = token {
        request.extend_from_slice(format!("Authorization: Bearer {token}\r\n").as_bytes());
    }
    request.extend_from_slice(
        format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    request.extend_from_slice(body);
    request
}

async fn raw_request(addr: SocketAddr, request: Vec<u8>) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).await.expect("connect host");
    stream.write_all(&request).await.expect("write request");
    stream.shutdown().await.expect("shutdown request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    response
}

fn response_body(response: &[u8]) -> &[u8] {
    response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(&[], |position| &response[position + 4..])
}

#[tokio::test]
async fn host_http_auth_models_command_and_snapshot_are_hermetic() {
    let mut app = test_app();
    app.models = vec![crate::provider::Model {
        id: "gpt-test".into(),
        name: "Test model".into(),
        reasoning_efforts: Vec::new(),
        context_length: Some(8_192),
        supports_images: false,
        supports_image_generation: false,
        supports_video_generation: false,
        backend: BackendTag::OpenAi,
        pricing: None,
    }];
    let mut server = HostServer::bind(app, HostConfig::new(0, "host-secret"))
        .await
        .unwrap();
    let addr = server.local_addr();

    let discovery = raw_request(addr, request("GET", "/.well-known/nexus", None, &[])).await;
    assert!(String::from_utf8_lossy(&discovery).starts_with("HTTP/1.1 200"));
    let discovery_body: serde_json::Value =
        serde_json::from_slice(response_body(&discovery)).unwrap();
    assert_eq!(discovery_body["product"], "nexus-chat");
    assert_eq!(discovery_body["api"], "/v1");
    assert_eq!(discovery_body["authentication"], "bearer");
    assert!(
        !discovery_body["host_id"]
            .as_str()
            .unwrap()
            .contains("host-secret")
    );

    let unauthorized = raw_request(addr, request("GET", "/v1/snapshot", None, &[])).await;
    assert!(String::from_utf8_lossy(&unauthorized).starts_with("HTTP/1.1 401"));

    // Authentication must happen before waiting for or allocating a
    // declared large body. An attacker that does not know the token gets
    // 401 immediately instead of tying up the request-read timeout.
    let oversized_unauthorized = raw_request(
        addr,
        format!(
            "POST /v1/command HTTP/1.1\r\nHost: localhost\r\nContent-Length: {MAX_GATEWAY_BODY_BYTES}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
    )
    .await;
    assert!(String::from_utf8_lossy(&oversized_unauthorized).starts_with("HTTP/1.1 401"));

    let models = raw_request(addr, request("GET", "/v1/models", Some("host-secret"), &[])).await;
    assert!(String::from_utf8_lossy(&models).starts_with("HTTP/1.1 200"));

    let tool_rpc = br#"{"jsonrpc":"2.0","id":"tool-1","method":"tools/run","params":{"name":"not_a_real_tool","arguments":{}}}"#;
    let tool_response = raw_request(
        addr,
        request("POST", "/v1/tools/run", Some("host-secret"), tool_rpc),
    )
    .await;
    assert!(String::from_utf8_lossy(&tool_response).starts_with("HTTP/1.1 200"));
    let tool_response: serde_json::Value =
        serde_json::from_slice(response_body(&tool_response)).unwrap();
    assert_eq!(tool_response["jsonrpc"], "2.0");
    assert_eq!(tool_response["id"], "tool-1");
    assert!(
        tool_response["result"]["result"]
            .as_str()
            .is_some_and(|result| result.contains("unknown tool"))
    );
    let models: serde_json::Value = serde_json::from_slice(response_body(&models)).unwrap();
    assert_eq!(models["object"], "list");
    assert_eq!(models["data"][0]["object"], "model");
    assert_eq!(models["data"][0]["id"], "openai:gpt-test");
    assert_eq!(models["data"][0]["owned_by"], "openai");

    let command = serde_json::to_vec(&AppCommand::SetSetting {
        key: "langsearch_key".into(),
        value: "do-not-leak".into(),
    })
    .unwrap();
    let command_response = raw_request(
        addr,
        request("POST", "/v1/command", Some("host-secret"), &command),
    )
    .await;
    assert!(String::from_utf8_lossy(&command_response).starts_with("HTTP/1.1 202"));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let snapshot = raw_request(
        addr,
        request("GET", "/v1/snapshot", Some("host-secret"), &[]),
    )
    .await;
    let snapshot_text = String::from_utf8_lossy(response_body(&snapshot));
    assert!(!snapshot_text.contains("do-not-leak"));
    assert!(snapshot_text.contains("langsearch_configured"));

    server.shutdown().await;
}

#[tokio::test]
async fn generated_apps_are_sandboxed_from_host_browser_storage() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = upstream.local_addr().unwrap().port();
    let mock = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let _ = read_request_head(&mut stream).await.unwrap();
        respond_with_content_type(&mut stream, 200, "text/html", b"<h1>App</h1>", false)
            .await
            .unwrap();
    });
    let mut server = HostServer::bind(test_app(), HostConfig::new(0, "secret"))
        .await
        .unwrap();
    let mut state = (*server.state).clone();
    state.app_server_port = Some(port);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let proxy = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_connection(stream, Arc::new(state)).await.unwrap();
    });
    let response = raw_request(addr, request("GET", "/apps/test/", Some("secret"), &[])).await;
    let headers = String::from_utf8_lossy(&response);
    assert!(headers.contains("Content-Security-Policy: sandbox allow-scripts allow-forms allow-downloads allow-popups\r\n"));
    assert!(!headers.contains("allow-same-origin"));
    assert_eq!(response_body(&response), b"<h1>App</h1>");
    proxy.await.unwrap();
    mock.await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn web_build_and_workspace_routes_are_hermetic() {
    let mut app = test_app();
    app.index.embedding_model.clear();
    let root = app.space.root.clone();
    let web = root.join("web");
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), "<h1>Nexus</h1>").unwrap();
    std::fs::write(web.join("assets/app.js"), "export {}").unwrap();
    std::fs::write(root.join("secret.txt"), "private").unwrap();
    let first_space = app.active_space.id.clone();
    let second = app.db.create_space("other").unwrap();
    let mut server = HostServer::bind(app, HostConfig::new(0, "secret").with_web_dir(&web))
        .await
        .unwrap();
    let addr = server.local_addr();
    let index = raw_request(addr, request("GET", "/", None, &[])).await;
    assert_eq!(response_body(&index), b"<h1>Nexus</h1>");
    let asset = raw_request(addr, request("GET", "/assets/app.js", None, &[])).await;
    assert!(String::from_utf8_lossy(&asset).contains("Content-Type: text/javascript"));
    let head = raw_request(addr, request("HEAD", "/", None, &[])).await;
    assert!(response_body(&head).is_empty());
    for path in ["/%2e%2e/secret.txt", "/missing.js"] {
        let result = raw_request(addr, request("GET", path, None, &[])).await;
        assert!(String::from_utf8_lossy(&result).starts_with("HTTP/1.1 404"));
    }
    for path in ["/v1/spaces", "/v1/files"] {
        let result = raw_request(addr, request("GET", path, None, &[])).await;
        assert!(String::from_utf8_lossy(&result).starts_with("HTTP/1.1 401"));
    }
    let target = format!("/v1/files?space_id={first_space}&name=hello.txt");
    let uploaded = raw_request(addr, request("PUT", &target, Some("secret"), b"hello")).await;
    assert!(String::from_utf8_lossy(&uploaded).starts_with("HTTP/1.1 201"));
    let duplicate = raw_request(addr, request("PUT", &target, Some("secret"), b"overwrite")).await;
    assert!(String::from_utf8_lossy(&duplicate).starts_with("HTTP/1.1 400"));
    let downloaded = raw_request(
        addr,
        request(
            "GET",
            &format!("/v1/sync/blob?space_id={first_space}&name=hello.txt"),
            Some("secret"),
            &[],
        ),
    )
    .await;
    assert_eq!(response_body(&downloaded), b"hello");
    let files = raw_request(
        addr,
        request(
            "GET",
            &format!("/v1/files?space_id={}", second.id),
            Some("secret"),
            &[],
        ),
    )
    .await;
    let value: serde_json::Value = serde_json::from_slice(response_body(&files)).unwrap();
    assert_eq!(value["files"], serde_json::json!([]));
    let unsafe_upload = raw_request(
        addr,
        request(
            "PUT",
            &format!("/v1/files?space_id={first_space}&name=..%2Fescape"),
            Some("secret"),
            b"bad",
        ),
    )
    .await;
    assert!(String::from_utf8_lossy(&unsafe_upload).starts_with("HTTP/1.1 400"));
    let created = raw_request(
        addr,
        request("POST", "/v1/spaces?name=research", Some("secret"), &[]),
    )
    .await;
    assert!(String::from_utf8_lossy(&created).starts_with("HTTP/1.1 201"));
    let switched = serde_json::to_vec(&AppCommand::SwitchSpace {
        name: "research".into(),
    })
    .unwrap();
    raw_request(
        addr,
        request("POST", "/v1/command", Some("secret"), &switched),
    )
    .await;
    let snapshot = raw_request(addr, request("GET", "/v1/snapshot", Some("secret"), &[])).await;
    let value: serde_json::Value = serde_json::from_slice(response_body(&snapshot)).unwrap();
    assert_eq!(value["active_space_name"], "research");
    server.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn host_session_messages_are_available_to_thin_clients() {
    let app = test_app();
    let session = app
        .db
        .create_session("web", "gpt-test", &app.active_space.id, "chat")
        .unwrap();
    app.db
        .insert_message(
            &session.id,
            "user",
            "hello from the browser",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let mut server = HostServer::bind(app, HostConfig::new(0, "host-secret"))
        .await
        .unwrap();
    let response = raw_request(
        server.local_addr(),
        request(
            "GET",
            &format!("/v1/sessions/{}/messages", session.id),
            Some("host-secret"),
            &[],
        ),
    )
    .await;
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
    let body: serde_json::Value = serde_json::from_slice(response_body(&response)).unwrap();
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hello from the browser");
    server.shutdown().await;
}

#[test]
fn session_messages_path_decodes_ids_without_matching_partial_paths() {
    assert_eq!(
        session_messages_path("/v1/sessions/session%2Fid/messages"),
        Some("session/id".into())
    );
    assert!(session_messages_path("/v1/sessions/session/messages/extra").is_none());
}

#[tokio::test]
async fn host_gateway_translates_codex_responses_stream() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    let upstream = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5-codex\",\"created_at\":123}}\n\n",
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from Codex\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":3,\"total_tokens\":13}}}\n\n",
    );
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut chunk = [0u8; 1024];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&chunk[..n]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        let request_text = String::from_utf8_lossy(&request);
        assert!(request_text.starts_with("POST /codex/responses HTTP/1.1"));
        assert!(request_text.contains("authorization: Bearer codex-access"));
        assert!(request_text.contains("chatgpt-account-id: codex-account"));
        assert!(request_text.contains("\"instructions\":\"be helpful\""));
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
            upstream.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        stream.write_all(upstream.as_bytes()).await.unwrap();
    });

    let mut app = test_app();
    app.saved.codex = Some(crate::config::CodexCredentials {
        access: "codex-access".into(),
        refresh: "codex-refresh".into(),
        expires: 0,
        account_id: "codex-account".into(),
    });
    app.backends.set(
        BackendTag::Codex,
        openrouter::OpenRouter::openai_codex("codex-access".into()),
    );
    app.models = vec![test_model("gpt-5-codex", BackendTag::Codex)];
    let mut server = HostServer::bind(
        app,
        HostConfig::new(0, "host-secret").with_gateway_base(format!("http://{mock_addr}")),
    )
    .await
    .unwrap();
    let body = br#"{"model":"codex:gpt-5-codex","stream":true,"messages":[{"role":"system","content":"be helpful"},{"role":"user","content":"hi"}]}"#;
    let response = raw_request(
        server.local_addr(),
        request("POST", "/v1/chat/completions", Some("host-secret"), body),
    )
    .await;
    let text = String::from_utf8_lossy(response_body(&response));
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
    assert!(text.contains("chat.completion.chunk"));
    assert!(text.contains("hello from Codex"));
    assert!(text.contains("\"prompt_tokens\":10"));
    assert!(text.contains("data: [DONE]"));
    assert!(!text.contains("response.output_text.delta"));
    server.shutdown().await;
    mock.await.unwrap();
}

#[tokio::test]
async fn host_gateway_preserves_mocked_stream_bytes() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    let expected = br#"data: {"choices":[{"delta":{"content":"hi"}}]}

data: [DONE]

"#;
    let expected_for_task = expected.to_vec();
    let mock = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut chunk = [0u8; 1024];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&chunk[..n]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request_text = String::from_utf8_lossy(&request);
        assert!(request_text.starts_with("POST /chat/completions HTTP/1.1"));
        assert!(request_text.contains("authorization: Bearer sk-host-test"));
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
            expected_for_task.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        stream.write_all(&expected_for_task).await.unwrap();
    });

    let mut app = test_app();
    app.models = vec![crate::provider::Model {
        id: "gpt-test".into(),
        name: "Test model".into(),
        reasoning_efforts: Vec::new(),
        context_length: None,
        supports_images: false,
        supports_image_generation: false,
        supports_video_generation: false,
        backend: BackendTag::OpenAi,
        pricing: None,
    }];
    let mut server = HostServer::bind(
        app,
        HostConfig::new(0, "host-secret").with_gateway_base(format!("http://{mock_addr}")),
    )
    .await
    .unwrap();
    let body =
        br#"{"model":"openai:gpt-test","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
    let response = raw_request(
        server.local_addr(),
        request("POST", "/v1/chat/completions", Some("host-secret"), body),
    )
    .await;
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
    assert!(response.ends_with(expected));
    mock.await.unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn host_sse_and_http_blob_transfer_are_hermetic() {
    let mut app = test_app();
    // The upload path rescans and starts semantic backfill; keep this
    // transport-only test independent of the fake provider key.
    app.index.embedding_model.clear();
    let space_id = app.active_space.id.clone();
    let name = "remote.txt";
    let bytes = b"blob over http";
    let digest = sha2::Sha256::digest(bytes);
    let hash = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    app.db
        .upsert_file(&space_id, name, &hash, bytes.len() as i64, "ok")
        .unwrap();
    let mut server = HostServer::bind(app, HostConfig::new(0, "host-secret"))
        .await
        .unwrap();
    let addr = server.local_addr();

    let mut events = TcpStream::connect(addr).await.unwrap();
    events
        .write_all(&request("GET", "/v1/events", Some("host-secret"), &[]))
        .await
        .unwrap();
    let mut initial = Vec::new();
    while !initial.windows(4).any(|window| window == b"\r\n\r\n") {
        let mut chunk = [0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(2), events.read(&mut chunk))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0);
        initial.extend_from_slice(&chunk[..n]);
    }

    let setting = serde_json::to_vec(&AppCommand::Send {
        text: "event-secret".into(),
    })
    .unwrap();
    let _ = raw_request(
        addr,
        request("POST", "/v1/command", Some("host-secret"), &setting),
    )
    .await;
    let mut frame_bytes = initial;
    while !String::from_utf8_lossy(&frame_bytes).contains("composer_set") {
        let mut chunk = [0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(2), events.read(&mut chunk))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0);
        frame_bytes.extend_from_slice(&chunk[..n]);
    }
    assert!(!String::from_utf8_lossy(&frame_bytes).contains("host-secret"));
    events.shutdown().await.unwrap();

    let target = format!("/v1/sync/blob?space_id={space_id}&name={name}&hash={hash}");
    let uploaded = raw_request(addr, request("PUT", &target, Some("host-secret"), bytes)).await;
    assert!(String::from_utf8_lossy(&uploaded).starts_with("HTTP/1.1 201"));
    let downloaded = raw_request(
        addr,
        request(
            "GET",
            &format!("/v1/sync/blob?space_id={space_id}&name={name}"),
            Some("host-secret"),
            &[],
        ),
    )
    .await;
    assert_eq!(response_body(&downloaded), bytes);
    server.shutdown().await;
}
