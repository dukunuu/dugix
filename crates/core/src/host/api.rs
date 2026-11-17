//! The raw `tokio` HTTP/SSE server for `dugix host`, deliberately framework-free
//! so authentication and body limits are easy to audit.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::app::{App, AppCommand};
use crate::appserver::AppRegistry;
use crate::db::UsageRange;
use crate::sync::Changeset;

use super::wire::WireEvent;

mod actor;
mod gateway;
mod http;
mod tool_rpc;

use actor::{HostRequest, app_actor, ask_actor};
use gateway::{BackendsResponse, DiscoveryResponse, ModelsResponse, OpenAiModel, handle_gateway};
use http::{
    Request, RequestHead, constant_time_eq, hex, is_hop_by_hop, query_without_token, read_body,
    read_request_head, respond_empty, respond_error, respond_full, respond_json, respond_text,
    split_target,
};
use tool_rpc::{ToolRpcError, ToolRpcResponse, ToolRunResult, parse_tool_rpc_request};

#[cfg(test)]
use super::wire::WireModel;
#[cfg(test)]
use crate::app::AppEvent;
#[cfg(test)]
use crate::provider::BackendTag;
#[cfg(test)]
use crate::provider::openrouter;
#[cfg(test)]
use actor::redact_wire_event;
#[cfg(test)]
use gateway::{GatewayUsage, gateway_route, parse_backend_tag};
#[cfg(test)]
use http::respond_with_content_type;
#[cfg(test)]
use tokio::io::AsyncReadExt;

const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_API_BODY_BYTES: usize = 10 * 1024 * 1024;
const MAX_GATEWAY_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;
/// How much of a request body is read (and reserved) at a time.
const BODY_CHUNK_BYTES: usize = 64 * 1024;
/// Ceiling on the bytes held back while looking for a provider usage frame.
const MAX_USAGE_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(15);
const ACTOR_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const UPSTREAM_HEADERS_TIMEOUT: Duration = Duration::from_secs(20);
const UPSTREAM_CHUNK_TIMEOUT: Duration = Duration::from_mins(2);
const MAX_APP_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 128;
/// CORS block shared by every response; `Access-Control-Allow-Methods` is
/// required for browser preflights.
const CORS_HEADERS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS\r\nAccess-Control-Allow-Headers: Authorization, Content-Type, X-Dugix-Backend, X-Dugix-Blob-Hash, X-Content-SHA256\r\nAccess-Control-Max-Age: 600\r\n";

/// Configuration for a local host listener.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// TCP port on loopback; `0` asks the OS for an ephemeral port.
    pub port: u16,
    /// Bearer token required by `/v1/*`; public app UUID routes do not use it.
    pub token: String,
    /// Optional upstream override for hermetic gateway tests.
    gateway_base: Option<String>,
    web_dir: Option<std::path::PathBuf>,
}

impl HostConfig {
    /// Construct a listener configuration.
    pub fn new(port: u16, token: impl Into<String>) -> Self {
        Self {
            port,
            token: token.into(),
            gateway_base: None,
            web_dir: None,
        }
    }

    /// Serve a built web client from this directory at the host origin.
    #[must_use]
    pub fn with_web_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.web_dir = Some(path.into());
        self
    }

    /// Route all gateway flavors to a local mock upstream, for network-free tests.
    #[must_use]
    pub fn with_gateway_base(mut self, base: impl Into<String>) -> Self {
        self.gateway_base = Some(base.into());
        self
    }
}

/// A running host server; dropping it aborts its tasks, while [`Self::shutdown`]
/// cancels them cleanly.
pub struct HostServer {
    state: Arc<HostState>,
    shutdown: Arc<Notify>,
    accept_task: Option<JoinHandle<()>>,
    actor_task: Option<JoinHandle<()>>,
    addr: SocketAddr,
}

#[derive(Clone)]
struct HostState {
    requests: mpsc::Sender<HostRequest>,
    events: broadcast::Sender<WireEvent>,
    token: Arc<String>,
    app_server_port: Option<u16>,
    app_registry: Option<AppRegistry>,
    client: reqwest::Client,
    connections: Arc<tokio::sync::Semaphore>,
    gateway_base: Option<String>,
    web_dir: Option<std::path::PathBuf>,
}

impl HostServer {
    /// Bind a loopback host listener and start the app actor.
    pub async fn bind(app: App, config: HostConfig) -> Result<Self> {
        if config.token.trim().is_empty() {
            bail!("host token must not be empty");
        }
        let listener = TcpListener::bind(("127.0.0.1", config.port))
            .await
            .with_context(|| format!("binding host listener on 127.0.0.1:{}", config.port))?;
        let addr = listener
            .local_addr()
            .context("reading host listener address")?;
        let app_server_port = app
            .app_server
            .as_ref()
            .map(crate::appserver::AppServer::port);
        let app_registry = app
            .app_server
            .as_ref()
            .map(|server| server.registry().clone());
        let (request_tx, request_rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(256);
        let shutdown = Arc::new(Notify::new());
        let host_token = Arc::new(config.token);
        let state = Arc::new(HostState {
            requests: request_tx,
            events: events.clone(),
            token: host_token.clone(),
            app_server_port,
            app_registry,
            client: reqwest::Client::builder()
                .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
                .pool_max_idle_per_host(8)
                .pool_idle_timeout(Duration::from_secs(30))
                .build()
                .context("building host upstream client")?,
            connections: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
            gateway_base: config.gateway_base,
            web_dir: config.web_dir,
        });
        let actor_events = events;
        let actor_shutdown = shutdown.clone();
        let actor_task = tokio::spawn(async move {
            app_actor(app, request_rx, actor_events, actor_shutdown, host_token).await;
        });
        let accept_state = state.clone();
        let accept_shutdown = shutdown.clone();
        let accept_task = tokio::spawn(async move {
            accept_loop(listener, accept_state, accept_shutdown).await;
        });
        Ok(Self {
            state,
            shutdown,
            accept_task: Some(accept_task),
            actor_task: Some(actor_task),
            addr,
        })
    }

    /// The loopback address selected by the OS.
    pub const fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Subscribe to the global app event stream; slow subscribers may lag and
    /// should refresh.
    pub fn subscribe(&self) -> broadcast::Receiver<WireEvent> {
        self.state.events.subscribe()
    }

    /// Set or clear the public app-link base used by the app actor.
    pub async fn set_public_base(&self, base: Option<String>) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.state
            .requests
            .send(HostRequest::SetPublicBase { base, reply })
            .await
            .map_err(|_| anyhow!("host actor stopped"))?;
        let result = rx.await.map_err(|_| anyhow!("host actor stopped"))?;
        result.map_err(|error| anyhow!(error))?;
        Ok(())
    }

    /// Stop accepting connections and abort the app actor.
    pub async fn shutdown(&mut self) {
        self.shutdown.notify_waiters();
        if let Some(task) = self.accept_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.actor_task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for HostServer {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        if let Some(task) = self.actor_task.take() {
            task.abort();
        }
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<HostState>, shutdown: Arc<Notify>) {
    loop {
        tokio::select! {
            () = shutdown.notified() => break,
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue; };
                let Ok(permit) = state.connections.clone().try_acquire_owned() else {
                    let mut stream = stream;
                    let _ = respond_text(&mut stream, 503, "host connection limit reached").await;
                    continue;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = handle_connection(stream, state).await;
                });
            }
        }
    }
}

async fn handle_connection(mut stream: TcpStream, state: Arc<HostState>) -> io::Result<()> {
    let head =
        match tokio::time::timeout(REQUEST_READ_TIMEOUT, read_request_head(&mut stream)).await {
            Ok(Ok(head)) => head,
            Ok(Err(error)) => return respond_text(&mut stream, error.status, error.message).await,
            Err(_) => return respond_text(&mut stream, 408, "request timed out").await,
        };
    let (path, query) = split_target(&head.target);
    let path = path.to_string();
    let query = query.to_string();
    if head.method.eq_ignore_ascii_case("OPTIONS") {
        return respond_empty(&mut stream, 204).await;
    }
    let is_apps = path == "/apps" || path.starts_with("/apps/");
    let is_v1 = path == "/v1" || path.starts_with("/v1/");
    if (is_apps || is_v1)
        && !authorized(
            &head,
            &state.token,
            is_apps,
            state.app_registry.as_ref(),
            &path,
        )
    {
        return respond_json(
            &mut stream,
            401,
            &serde_json::json!({
                "error": { "message": "unauthorized", "type": "authentication_error" }
            }),
        )
        .await;
    }
    if head.content_length > MAX_GATEWAY_BODY_BYTES {
        return respond_text(&mut stream, 413, "request entity too large").await;
    }
    let body = match tokio::time::timeout(
        REQUEST_READ_TIMEOUT,
        read_body(&mut stream, &head.prefix, head.content_length),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(error)) => return respond_text(&mut stream, error.status, error.message).await,
        Err(_) => return respond_text(&mut stream, 408, "request timed out").await,
    };
    let request = Request {
        method: head.method,
        target: head.target,
        headers: head.headers,
        body,
    };

    if path == "/v1/events" {
        return handle_sse(&mut stream, &request, &state).await;
    }
    if path == "/v1/chat/completions" {
        return handle_gateway(&mut stream, &request, &state).await;
    }
    if is_apps {
        // Public app URLs are capabilities: the registry UUID is high entropy and
        // the host bearer token never appears in the URL or logs.
        return proxy_app(&mut stream, &request, &path, &query, &state).await;
    }
    if !is_v1 && path != "/.well-known/dugix" {
        return serve_web(&mut stream, &request, &path, state.web_dir.as_deref()).await;
    }
    handle_api_route(&mut stream, &request, &path, &state).await
}

async fn serve_web(
    stream: &mut TcpStream,
    request: &Request,
    path: &str,
    root: Option<&std::path::Path>,
) -> io::Result<()> {
    if !matches!(request.method.as_str(), "GET" | "HEAD") {
        return respond_text(stream, 405, "method not allowed").await;
    }
    let Some(root) = root else {
        return respond_text(
            stream,
            404,
            "Web client unavailable. Build web/ and set DUGIX_WEB_DIR to its dist directory.",
        )
        .await;
    };
    let decoded = percent_decode(path);
    let relative = decoded.trim_start_matches('/');
    if relative
        .split('/')
        .any(|part| part == ".." || part.starts_with('.'))
        || relative.contains('\\')
    {
        return respond_text(stream, 404, "not found").await;
    }
    let Ok(root) = tokio::fs::canonicalize(root).await else {
        return respond_text(stream, 404, "web build not found").await;
    };
    let target = root.join(if relative.is_empty() {
        "index.html"
    } else {
        relative
    });
    let Ok(target) = tokio::fs::canonicalize(target).await else {
        return respond_text(stream, 404, "not found").await;
    };
    if !target.starts_with(&root) {
        return respond_text(stream, 404, "not found").await;
    }
    let Ok(metadata) = tokio::fs::metadata(&target).await else {
        return respond_text(stream, 404, "not found").await;
    };
    if !metadata.is_file() || metadata.len() > MAX_APP_RESPONSE_BYTES as u64 {
        return respond_text(stream, 404, "not found").await;
    }
    match tokio::fs::read(&target).await {
        Ok(bytes) => {
            respond_full(
                stream,
                200,
                crate::appserver::mime_for(&target),
                "X-Content-Type-Options: nosniff\r\n",
                &bytes,
                request.method == "HEAD",
            )
            .await
        }
        Err(_) => respond_text(stream, 404, "not found").await,
    }
}

fn authorized(
    request: &RequestHead,
    token: &str,
    apps: bool,
    registry: Option<&AppRegistry>,
    path: &str,
) -> bool {
    if let Some(value) = request.headers.get("authorization")
        && let Some((scheme, supplied)) = value.split_once(char::is_whitespace)
        && scheme.eq_ignore_ascii_case("bearer")
        && constant_time_eq(supplied.trim().as_bytes(), token.as_bytes())
    {
        return true;
    }
    apps && known_app_path(registry, path)
}

fn known_app_path(registry: Option<&AppRegistry>, path: &str) -> bool {
    let Some(registry) = registry else {
        return false;
    };
    let Some(rest) = path.strip_prefix("/apps/") else {
        return false;
    };
    let Some(uuid) = rest.split('/').next() else {
        return false;
    };
    !uuid.is_empty() && registry.lookup(uuid).is_some()
}

#[allow(clippy::too_many_lines)]
async fn handle_api_route(
    stream: &mut TcpStream,
    request: &Request,
    path: &str,
    state: &HostState,
) -> io::Result<()> {
    if path == "/v1/media/action" && request.method == "POST" {
        let value = serde_json::from_slice::<serde_json::Value>(&request.body).unwrap_or_default();
        if value["action"] == "run_script" {
            let query = split_target(&request.target).1;
            let space_id = query_param(query, "space_id").unwrap_or_default();
            let name = value["name"].as_str().unwrap_or_default().to_string();
            let Ok(args) = serde_json::from_value::<Vec<String>>(
                value
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            ) else {
                return respond_error(stream, 400, "invalid script arguments").await;
            };
            let job = ask_actor(&state.requests, |reply| HostRequest::ScriptJob {
                space_id,
                name,
                args,
                reply,
            })
            .await;
            let result = match job {
                Ok(job) => super::jobs::run(job)
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            };
            return match result {
                Ok(value) => respond_json(stream, 200, &value).await,
                Err(error) => respond_error(stream, 400, &error).await,
            };
        }
    }
    if path.starts_with("/v1/sessions/") && matches!(request.method.as_str(), "PATCH" | "DELETE") {
        let id = percent_decode(path.trim_start_matches("/v1/sessions/"));
        if id.is_empty() || id.contains('/') {
            return respond_error(stream, 400, "invalid session id").await;
        }
        let body = serde_json::from_slice::<serde_json::Value>(&request.body).unwrap_or_default();
        let space_id = body["space_id"]
            .as_str()
            .map(str::to_string)
            .or_else(|| query_param(split_target(&request.target).1, "space_id"))
            .unwrap_or_default();
        let title = (request.method == "PATCH")
            .then(|| body["title"].as_str().unwrap_or_default().to_string());
        let result = ask_actor(&state.requests, |reply| HostRequest::SessionEdit {
            space_id,
            id,
            title,
            reply,
        })
        .await;
        return match result {
            Ok(value) => respond_json(stream, 200, &value).await,
            Err(error) => respond_error(stream, 400, &error).await,
        };
    }
    if path == "/v1/sync/bundle" {
        if !matches!(request.method.as_str(), "GET" | "POST") {
            return respond_text(stream, 405, "method not allowed").await;
        }
        let input = (request.method == "POST").then(|| request.body.clone());
        let result = ask_actor(&state.requests, |reply| HostRequest::Bundle {
            input,
            reply,
        })
        .await;
        return match result {
            Ok(bytes) => {
                respond_full(
                    stream,
                    200,
                    "application/zip",
                    "Content-Disposition: attachment; filename=dugix-sync.zip\r\n",
                    &bytes,
                    false,
                )
                .await
            }
            Err(error) => respond_error(stream, 400, &error).await,
        };
    }
    if path == "/v1/media/blob" {
        if request.method != "GET" {
            return respond_text(stream, 405, "method not allowed").await;
        }
        let query = split_target(&request.target).1.to_string();
        let result = ask_actor(&state.requests, |reply| HostRequest::MediaBlob {
            query,
            reply,
        })
        .await;
        return match result {
            Ok(blob) => {
                respond_full(
                    stream,
                    200,
                    &blob.mime,
                    "Content-Security-Policy: sandbox\r\nX-Content-Type-Options: nosniff\r\n",
                    &blob.bytes,
                    false,
                )
                .await
            }
            Err(error) => respond_error(stream, 404, &error).await,
        };
    }
    if (request.method == "GET" && matches!(path, "/v1/sync" | "/v1/sync/state"))
        || matches!(
            path,
            "/v1/settings"
                | "/v1/settings/document"
                | "/v1/login"
                | "/v1/attachments"
                | "/v1/models/refresh"
                | "/v1/media"
                | "/v1/media/action"
        )
        || ["/v1/swarm", "/v1/skills", "/v1/watches", "/v1/research"]
            .iter()
            .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
    {
        if request.body.len() > MAX_API_BODY_BYTES
            && !(path == "/v1/media" && request.method == "PUT")
        {
            return respond_text(stream, 413, "request entity too large").await;
        }
        let route = path.to_string();
        let method = request.method.clone();
        let query = split_target(&request.target).1.to_string();
        let body = request.body.clone();
        let result = ask_actor(&state.requests, |reply| HostRequest::Surface {
            route,
            method,
            query,
            body,
            reply,
        })
        .await;
        return match result {
            Ok(value) => respond_json(stream, 200, &value).await,
            Err(error) => respond_error(stream, 400, &error).await,
        };
    }
    if matches!(path, "/v1/apps" | "/v1/apps/files" | "/v1/apps/source") {
        if request.method != "GET"
            && !(request.method == "PUT" && path == "/v1/apps/source")
            && !(path == "/v1/apps" && matches!(request.method.as_str(), "POST" | "DELETE"))
        {
            return respond_text(stream, 405, "method not allowed").await;
        }
        let query = split_target(&request.target).1;
        let space_id = query_param(query, "space_id");
        let name = query_param(query, "name").unwrap_or_default();
        let file = query_param(query, "path").unwrap_or_default();
        let edit = if request.method == "PUT" {
            match serde_json::from_slice::<super::apps::Edit>(&request.body) {
                Ok(edit) => Some(edit),
                Err(_) => return respond_error(stream, 400, "invalid source edit").await,
            }
        } else {
            None
        };
        let route = path.to_string();
        let method = request.method.clone();
        let result = ask_actor(&state.requests, |reply| HostRequest::Apps {
            route,
            method,
            space_id,
            name,
            file,
            edit,
            reply,
        })
        .await;
        return match result {
            Ok(value) => respond_json(stream, 200, &value).await,
            Err(error) => respond_error(stream, 400, &error).await,
        };
    }
    if path == "/v1/sync/blob"
        || path.starts_with("/v1/sync/blob/")
        || path.starts_with("/v1/sync/blobs/")
    {
        return handle_sync_blob(stream, request, path, state).await;
    }
    if let Some(session_id) = session_messages_path(path) {
        if request.method != "GET" {
            return respond_text(stream, 405, "method not allowed").await;
        }
        let messages = ask_actor(&state.requests, |reply| HostRequest::Messages {
            session_id,
            reply,
        })
        .await;
        return match messages {
            Ok(messages) => {
                respond_json(stream, 200, &serde_json::json!({ "messages": messages })).await
            }
            Err(error) => respond_error(stream, 404, &error).await,
        };
    }
    match (request.method.as_str(), path) {
        ("GET", "/.well-known/dugix") => {
            let discovery = DiscoveryResponse {
                host_id: host_discovery_id(&state.token),
                product: "dugix",
                version: env!("CARGO_PKG_VERSION"),
                api: "/v1",
                authentication: "bearer",
            };
            respond_json(stream, 200, &discovery).await
        }
        ("POST", "/v1/spaces") | ("PUT", "/v1/files") => {
            let query = split_target(&request.target).1;
            let name = query_param(query, "name").unwrap_or_default();
            let space_id = query_param(query, "space_id");
            let bytes = (path == "/v1/files").then(|| request.body.clone());
            let result = ask_actor(&state.requests, |reply| HostRequest::WorkspaceWrite {
                name,
                space_id,
                bytes,
                reply,
            })
            .await;
            match result {
                Ok(()) => respond_json(stream, 201, &serde_json::json!({ "ok": true })).await,
                Err(error) => respond_error(stream, 400, &error).await,
            }
        }
        ("GET", "/v1/spaces" | "/v1/files") => {
            let files = path == "/v1/files";
            let space_id = query_param(split_target(&request.target).1, "space_id");
            let result = ask_actor(&state.requests, |reply| HostRequest::Workspace {
                files,
                space_id,
                reply,
            })
            .await;
            match result {
                Ok(value) => respond_json(stream, 200, &value).await,
                Err(error) => respond_error(stream, 400, &error).await,
            }
        }
        ("GET", "/v1/snapshot") => {
            let snapshot =
                ask_actor(&state.requests, |reply| HostRequest::Snapshot { reply }).await;
            match snapshot {
                Ok(snapshot) => respond_json(stream, 200, &snapshot).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("GET", "/v1/usage") => {
            let range = query_param(split_target(&request.target).1, "range")
                .map(|value| UsageRange::from_key(&value))
                .unwrap_or_default();
            let usage =
                ask_actor(&state.requests, |reply| HostRequest::Usage { range, reply }).await;
            match usage {
                Ok(usage) => respond_json(stream, 200, &usage).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("GET", "/v1/models") => {
            let models = ask_actor(&state.requests, |reply| HostRequest::Models { reply }).await;
            match models {
                Ok(models) => {
                    let response = ModelsResponse {
                        object: "list",
                        data: models.into_iter().map(OpenAiModel::from).collect(),
                    };
                    respond_json(stream, 200, &response).await
                }
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("GET", "/v1/backends") => {
            let backends =
                ask_actor(&state.requests, |reply| HostRequest::Backends { reply }).await;
            match backends {
                Ok(backends) => respond_json(stream, 200, &BackendsResponse { backends }).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("POST", "/v1/command") => {
            if request.body.len() > MAX_API_BODY_BYTES {
                return respond_text(stream, 413, "request entity too large").await;
            }
            let command = match serde_json::from_slice::<AppCommand>(&request.body) {
                Ok(command) => command,
                Err(error) => {
                    return respond_error(stream, 400, &format!("invalid command: {error}")).await;
                }
            };
            let result = ask_actor(&state.requests, |reply| HostRequest::Command {
                command,
                reply,
            })
            .await;
            match result {
                Ok(()) => respond_json(stream, 202, &serde_json::json!({ "ok": true })).await,
                Err(error) => respond_error(stream, 400, &error).await,
            }
        }
        ("POST", "/v1/sync") => {
            if request.body.len() > MAX_API_BODY_BYTES {
                return respond_text(stream, 413, "request entity too large").await;
            }
            let changeset = match serde_json::from_slice::<Changeset>(&request.body) {
                Ok(changeset) => changeset,
                Err(error) => {
                    return respond_error(stream, 400, &format!("invalid changeset: {error}"))
                        .await;
                }
            };
            let result = ask_actor(&state.requests, |reply| HostRequest::Sync {
                changeset,
                reply,
            })
            .await;
            match result {
                Ok(reply) => respond_json(stream, 200, &reply).await,
                Err(error) => respond_error(stream, 400, &error).await,
            }
        }
        ("GET", "/v1/tools") => {
            let defs = ask_actor(&state.requests, |reply| HostRequest::ToolDefs { reply }).await;
            match defs {
                Ok(defs) => respond_json(stream, 200, &serde_json::json!({ "tools": defs })).await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        ("POST", "/v1/tools/run") => {
            if request.body.len() > MAX_API_BODY_BYTES {
                return respond_text(stream, 413, "request entity too large").await;
            }
            let input = match parse_tool_rpc_request(&request.body) {
                Ok(input) => input,
                Err(error) => {
                    let response = ToolRpcResponse::error(error.id, error.error);
                    return respond_json(stream, 400, &response).await;
                }
            };
            let space_id = query_param(split_target(&request.target).1, "space_id");
            let toolbox = match ask_actor(&state.requests, |reply| HostRequest::Toolbox {
                space_id,
                reply,
            })
            .await
            {
                Ok(toolbox) => toolbox,
                Err(error) => {
                    let response = ToolRpcResponse::error(input.id, ToolRpcError::internal(error));
                    return respond_json(stream, 200, &response).await;
                }
            };
            let args = match input.args {
                serde_json::Value::String(args) => args,
                value => value.to_string(),
            };
            let (result, label) = toolbox.run(&input.name, &args).await;
            respond_json(
                stream,
                200,
                &ToolRpcResponse::success(input.id, ToolRunResult { result, label }),
            )
            .await
        }
        ("GET", "/") => respond_text(stream, 200, "dugix host\n").await,
        _ => respond_text(stream, 404, "not found").await,
    }
}

async fn handle_sync_blob(
    stream: &mut TcpStream,
    request: &Request,
    path: &str,
    state: &HostState,
) -> io::Result<()> {
    let query = split_target(&request.target).1;
    let path_parts = path
        .strip_prefix("/v1/sync/blob/")
        .or_else(|| path.strip_prefix("/v1/sync/blobs/"))
        .and_then(|rest| rest.split_once('/'));
    let space_id = query_param(query, "space_id")
        .or_else(|| path_parts.map(|(space, _)| percent_decode(space)));
    let Some(space_id) = space_id else {
        return respond_error(stream, 400, "sync blob is missing space_id").await;
    };
    let name =
        query_param(query, "name").or_else(|| path_parts.map(|(_, name)| percent_decode(name)));
    let Some(name) = name else {
        return respond_error(stream, 400, "sync blob is missing name").await;
    };
    match request.method.as_str() {
        "PUT" => {
            if request.body.len() > MAX_BLOB_BYTES {
                return respond_text(stream, 413, "blob too large").await;
            }
            let hash = query_param(query, "hash")
                .or_else(|| request.headers.get("x-dugix-blob-hash").cloned())
                .or_else(|| request.headers.get("x-content-sha256").cloned());
            let Some(hash) = hash else {
                return respond_error(stream, 400, "sync blob is missing hash").await;
            };
            let result = ask_actor(&state.requests, |reply| HostRequest::PutBlob {
                space_id,
                name,
                hash,
                bytes: request.body.clone(),
                reply,
            })
            .await;
            match result {
                Ok(()) => {
                    respond_json(
                        stream,
                        201,
                        &serde_json::json!({ "ok": true, "bytes": request.body.len() }),
                    )
                    .await
                }
                Err(error) => respond_error(stream, 409, &error).await,
            }
        }
        "GET" | "HEAD" => {
            let result = ask_actor(&state.requests, |reply| HostRequest::GetBlob {
                space_id,
                name,
                reply,
            })
            .await;
            match result {
                Ok(Some(bytes)) => {
                    respond_full(
                        stream,
                        200,
                        "application/octet-stream",
                        "",
                        &bytes,
                        request.method == "HEAD",
                    )
                    .await
                }
                Ok(None) => respond_text(stream, 404, "sync blob not found").await,
                Err(error) => respond_error(stream, 500, &error).await,
            }
        }
        _ => respond_text(stream, 405, "method not allowed").await,
    }
}

async fn handle_sse(
    stream: &mut TcpStream,
    request: &Request,
    state: &HostState,
) -> io::Result<()> {
    if request.method != "GET" {
        return respond_text(stream, 405, "method not allowed").await;
    }
    let mut receiver = state.events.subscribe();
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n{CORS_HEADERS}\r\n"
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await?;
    let mut heartbeat = tokio::time::interval(SSE_HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Ok(event) => {
                    let Ok(json) = serde_json::to_string(&event) else {
                        continue;
                    };
                    if stream.write_all(format!("data: {json}\n\n").as_bytes()).await.is_err() {
                        return Ok(());
                    }
                    if stream.flush().await.is_err() {
                        return Ok(());
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if stream.write_all(b": lagged; refresh /v1/snapshot\n\n").await.is_err() {
                        return Ok(());
                    }
                    let _ = stream.flush().await;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = heartbeat.tick() => {
                if stream.write_all(b": heartbeat\n\n").await.is_err() {
                    return Ok(());
                }
                if stream.flush().await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

async fn proxy_app(
    stream: &mut TcpStream,
    request: &Request,
    path: &str,
    query: &str,
    state: &HostState,
) -> io::Result<()> {
    if !matches!(
        request.method.as_str(),
        "GET" | "HEAD" | "POST" | "PUT" | "DELETE" | "OPTIONS"
    ) {
        return respond_text(stream, 405, "method not allowed").await;
    }
    let Some(port) = state.app_server_port else {
        return respond_text(stream, 503, "app server unavailable").await;
    };
    let local_path = path.strip_prefix("/apps").unwrap_or("/");
    let query = query_without_token(query);
    let target = if query.is_empty() {
        format!("http://127.0.0.1:{port}{local_path}")
    } else {
        format!("http://127.0.0.1:{port}{local_path}?{query}")
    };
    let Ok(method) = reqwest::Method::from_bytes(request.method.as_bytes()) else {
        return respond_text(stream, 400, "invalid method").await;
    };
    let mut builder = state
        .client
        .request(method, target)
        .body(request.body.clone());
    for (name, value) in &request.headers {
        if is_hop_by_hop(name) || name == "cookie" || name == "x-dugix-token" {
            continue;
        }
        let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) else {
            continue;
        };
        builder = builder.header(header_name, header_value);
    }
    let response = match tokio::time::timeout(UPSTREAM_HEADERS_TIMEOUT, builder.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return respond_error(stream, 502, &format!("app proxy failed: {error}")).await;
        }
        Err(_) => return respond_error(stream, 504, "app proxy timed out").await,
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let mut body = Vec::new();
    let mut upstream = response.bytes_stream();
    loop {
        let Ok(next) = tokio::time::timeout(UPSTREAM_CHUNK_TIMEOUT, upstream.next()).await else {
            return respond_error(stream, 504, "app response timed out").await;
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                return respond_error(stream, 502, &format!("app response failed: {error}")).await;
            }
        };
        if body.len().saturating_add(chunk.len()) > MAX_APP_RESPONSE_BYTES {
            return respond_text(stream, 502, "app response too large").await;
        }
        body.extend_from_slice(&chunk);
    }
    respond_full(
        stream,
        status,
        &content_type,
        // Generated applications are untrusted documents; an opaque sandbox
        // origin keeps them from the web client's tokens in browser storage.
        "Content-Security-Policy: sandbox allow-scripts allow-forms allow-downloads allow-popups\r\nX-Content-Type-Options: nosniff\r\n",
        &body,
        request.method == "HEAD",
    )
    .await
}

pub(super) fn hex_string(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        },
    )
}

/// Public discovery fingerprint; compare before transmitting a host bearer token.
#[must_use]
pub fn host_discovery_id(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let suffix = hex_string(&digest[..8]);
    format!("host-{suffix}")
}

fn session_messages_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1/sessions/")?;
    let session_id = rest.strip_suffix("/messages")?;
    (!session_id.is_empty()).then(|| percent_decode(session_id))
}

pub(super) fn query_param(query: &str, wanted: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(name).eq_ignore_ascii_case(wanted)).then(|| percent_decode(value))
    })
}

pub(super) fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            output.push(high * 16 + low);
            index += 3;
        } else {
            output.push(if bytes[index] == b'+' {
                b' '
            } else {
                bytes[index]
            });
            index += 1;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests;
