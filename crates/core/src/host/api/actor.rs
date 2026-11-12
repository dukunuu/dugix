//! The app actor: one task owns `App`, serves requests, and fans out events.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};

use crate::app::{App, AppCommand, AppEvent, CoreSnapshot};
use crate::db::UsageRange;
use crate::provider::BackendTag;
use crate::sync::Changeset;
use crate::tools::ToolExecutor;

use super::ACTOR_REQUEST_TIMEOUT;
use super::gateway::{
    BackendInfo, GatewayRoute, GatewayUsage, gateway_route, public_model_id_for_backend,
};
use super::hex_string;
use crate::host::wire::{WireEvent, WireModel};

pub(super) fn redact_wire_event(event: WireEvent, app: &App, host_token: &str) -> WireEvent {
    let mut secrets = vec![
        host_token,
        app.saved.host_token.as_deref().unwrap_or_default(),
        app.saved.openrouter_key.as_deref().unwrap_or_default(),
        app.saved.openai_key.as_deref().unwrap_or_default(),
        app.saved.opencode_key.as_deref().unwrap_or_default(),
        app.saved.mistral_key.as_deref().unwrap_or_default(),
        app.saved
            .codex
            .as_ref()
            .map_or("", |credentials| credentials.access.as_str()),
        app.saved
            .codex
            .as_ref()
            .map_or("", |credentials| credentials.refresh.as_str()),
        app.search.langsearch_key.as_str(),
        app.search.searxng_url.as_str(),
    ];
    secrets.retain(|secret| !secret.is_empty());
    let Ok(mut value) = serde_json::to_value(event.clone()) else {
        return event;
    };
    redact_json_secrets(&mut value, &secrets);
    serde_json::from_value(value).unwrap_or(event)
}

fn redact_json_secrets(value: &mut serde_json::Value, secrets: &[&str]) {
    match value {
        serde_json::Value::String(text) => {
            for secret in secrets {
                *text = text.replace(secret, "[redacted]");
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json_secrets(value, secrets);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_json_secrets(value, secrets);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

pub(super) async fn app_actor(
    mut app: App,
    mut requests: mpsc::Receiver<HostRequest>,
    events: broadcast::Sender<WireEvent>,
    shutdown: Arc<Notify>,
    host_token: Arc<String>,
) {
    let mut admin = crate::host::admin::State::default();
    loop {
        tokio::select! {
            () = shutdown.notified() => break,
            request = requests.recv() => {
                let Some(request) = request else { break; };
                handle_actor_request(&mut app, &mut admin, request);
            }
            event = app.next_event() => {
                admin.observe(&event);
                let wire = redact_wire_event(WireEvent::from(event.clone()), &app, host_token.as_str());
                // The handler may clear a one-shot receiver itself; this backstop
                // prevents a closed source from returning `None` forever.
                clear_closed_source(&mut app, &event);
                apply_domain_event(&mut app, event);
                let _ = events.send(wire);
            }
        }
    }
}

fn write_workspace(
    app: &mut App,
    name: &str,
    space_id: Option<&str>,
    bytes: Option<&[u8]>,
) -> Result<()> {
    if name.trim().is_empty()
        || name.starts_with('.')
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        bail!("invalid name");
    }
    if let Some(bytes) = bytes {
        let space = app
            .db
            .list_spaces()?
            .into_iter()
            .find(|space| Some(space.id.as_str()) == space_id)
            .ok_or_else(|| anyhow!("unknown space"))?;
        let dir = app.space.files_dir(&space.name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(name);
        // Never silently overwrite a user's existing file, including symlinks.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        if let Err(error) = std::io::Write::write_all(&mut file, bytes) {
            let _ = std::fs::remove_file(&path);
            return Err(error.into());
        }
        let hash = hex_string(&Sha256::digest(bytes));
        app.db.upsert_file(
            &space.id,
            name,
            &hash,
            i64::try_from(bytes.len())?,
            "not indexed",
        )?;
        if space.id == app.active_space.id {
            app.rescan_files();
        }
    } else {
        app.space.ensure_space_dir(name)?;
        app.db.create_space(name)?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn handle_actor_request(
    app: &mut App,
    admin: &mut crate::host::admin::State,
    request: HostRequest,
) {
    match request {
        HostRequest::ScriptJob {
            space_id,
            name,
            args,
            reply,
        } => {
            let _ = reply.send(
                crate::host::media::prepare_script(app, &space_id, &name, &args)
                    .map_err(|error| error.to_string()),
            );
        }
        HostRequest::SessionEdit {
            space_id,
            id,
            title,
            reply,
        } => {
            let result = if let Some(title) = title {
                crate::host::sessions_view::rename(app, &space_id, &id, &title)
                    .and_then(|value| Ok(serde_json::to_value(value)?))
            } else {
                crate::host::sessions_view::delete(app, &space_id, &id)
            };
            let _ = reply.send(result.map_err(|error| error.to_string()));
        }
        HostRequest::Bundle { input, reply } => {
            let _ = reply.send(
                crate::host::browser_sync::bundle(app, input.as_deref())
                    .map_err(|error| error.to_string()),
            );
        }

        HostRequest::MediaBlob { query, reply } => {
            let _ = reply
                .send(crate::host::media::blob(app, &query).map_err(|error| error.to_string()));
        }

        HostRequest::Surface {
            route,
            method,
            query,
            body,
            reply,
        } => {
            let result = if route.starts_with("/v1/sync") {
                crate::host::browser_sync::dispatch(app, &method, &route, &query)
            } else if route.starts_with("/v1/research/") {
                crate::host::research_view::dispatch(app, &method, &route, &query, &body)
            } else if ["/v1/swarm", "/v1/skills", "/v1/watches"]
                .iter()
                .any(|prefix| route.starts_with(prefix))
            {
                crate::host::management::dispatch(app, &method, &route, &query, &body)
            } else if route.starts_with("/v1/media") {
                crate::host::media::dispatch(app, &method, &query, &body)
            } else {
                crate::host::admin::dispatch(app, admin, &method, &route, &query, &body)
            };
            let _ = reply.send(result.map_err(|error| error.to_string()));
        }

        HostRequest::Apps {
            route,
            method,
            space_id,
            name,
            file,
            edit,
            reply,
        } => {
            let result = match (method.as_str(), route.as_str()) {
                ("DELETE", "/v1/apps") => {
                    crate::host::apps::delete(app, space_id.as_deref(), &name)
                }
                ("POST", "/v1/apps") => {
                    crate::host::apps::register(app, space_id.as_deref(), &name)
                }
                (_, "/v1/apps") => crate::host::apps::catalog(app, space_id.as_deref()),
                (_, "/v1/apps/files") => crate::host::apps::files(app, space_id.as_deref(), &name),
                _ => crate::host::apps::source(app, space_id.as_deref(), &name, &file, edit),
            };
            let _ = reply.send(result.map_err(|error| error.to_string()));
        }

        HostRequest::WorkspaceWrite {
            name,
            space_id,
            bytes,
            reply,
        } => {
            let _ = reply.send(
                write_workspace(app, &name, space_id.as_deref(), bytes.as_deref())
                    .map_err(|error| error.to_string()),
            );
        }
        HostRequest::Workspace {
            files,
            space_id,
            reply,
        } => {
            let result = (|| -> Result<serde_json::Value> {
                let spaces = app.db.list_spaces()?;
                if files {
                    let id = space_id.as_deref().unwrap_or(&app.active_space.id);
                    if !spaces.iter().any(|space| space.id == id) {
                        bail!("unknown space");
                    }
                    let files = app.db.list_files(id)?.into_iter().map(|file| serde_json::json!({
                        "id": file.id, "name": file.name, "size": file.size, "hash": file.hash, "status": file.status
                    })).collect::<Vec<_>>();
                    Ok(serde_json::json!({ "space_id": id, "files": files }))
                } else {
                    let spaces = spaces
                        .into_iter()
                        .map(|space| serde_json::json!({ "id": space.id, "name": space.name }))
                        .collect::<Vec<_>>();
                    Ok(
                        serde_json::json!({ "active_space_id": app.active_space.id, "spaces": spaces }),
                    )
                }
            })();
            let _ = reply.send(result.map_err(|error| error.to_string()));
        }
        HostRequest::Snapshot { reply } => {
            let _ = reply.send(app.snapshot().map_err(|error| error.to_string()));
        }
        HostRequest::Models { reply } => {
            // Every configured backend has an OpenAI-wire gateway route.
            let models = app.models.iter().cloned().map(WireModel::from).collect();
            let _ = reply.send(Ok(models));
        }
        HostRequest::Usage { range, reply } => {
            app.backfill_usage_costs();
            let data = app.load_usage_for_range(range);
            let usage = serde_json::json!({
                "range": range.key(),
                "totals": {
                    "requests": data.totals.requests,
                    "prompt_tokens": data.totals.prompt_tokens,
                    "completion_tokens": data.totals.completion_tokens,
                    "cache_read_tokens": data.totals.cache_read_tokens,
                    "cache_creation_tokens": data.totals.cache_creation_tokens,
                    "rated_prompt_tokens": data.totals.rated_prompt_tokens,
                    "rated_cache_read_tokens": data.totals.rated_cache_read_tokens,
                    "cost": data.totals.cost,
                },
                "by_backend": data.by_backend.iter().map(|row| serde_json::json!({
                    "backend": row.backend,
                    "requests": row.requests,
                    "prompt_tokens": row.prompt_tokens,
                    "completion_tokens": row.completion_tokens,
                    "cache_read_tokens": row.cache_read_tokens,
                    "rated_prompt_tokens": row.rated_prompt_tokens,
                    "rated_cache_read_tokens": row.rated_cache_read_tokens,
                    "cost": row.cost,
                })).collect::<Vec<_>>(),
                "by_model": data.by_model.iter().map(|row| serde_json::json!({
                    "model": row.model,
                    "requests": row.requests,
                    "prompt_tokens": row.prompt_tokens,
                    "completion_tokens": row.completion_tokens,
                    "cache_read_tokens": row.cache_read_tokens,
                    "rated_prompt_tokens": row.rated_prompt_tokens,
                    "rated_cache_read_tokens": row.rated_cache_read_tokens,
                    "cost": row.cost,
                })).collect::<Vec<_>>(),
                "recent": data.recent.iter().map(|row| serde_json::json!({
                    "created_at": row.created_at,
                    "backend": row.backend,
                    "model": row.model,
                    "prompt_tokens": row.prompt_tokens,
                    "completion_tokens": row.completion_tokens,
                    "cache_read_tokens": row.cache_read_tokens,
                    "cache_creation_tokens": row.cache_creation_tokens,
                    "prompt_convention": row.prompt_convention.as_str(),
                    "cost": row.cost,
                })).collect::<Vec<_>>(),
            });
            let _ = reply.send(Ok(usage));
        }
        HostRequest::Backends { reply } => {
            let tags = [
                BackendTag::OpenRouter,
                BackendTag::OpenAi,
                BackendTag::OpencodeGo,
                BackendTag::Codex,
                BackendTag::Mistral,
                BackendTag::Local,
            ];
            let backends = tags
                .into_iter()
                .map(|tag| {
                    let provider = app.backends.get(tag);
                    BackendInfo {
                        tag: tag.into(),
                        name: tag.display_name(),
                        configured: app.backends.configured(tag),
                        gateway_supported: true,
                        gateway_error: None,
                        default_model: provider.map_or_else(String::new, |provider| {
                            // A local runtime has no cheap default of its own.
                            match provider.default_utility_model() {
                                "" => String::new(),
                                model => {
                                    public_model_id_for_backend(tag, &app.newest_listed(tag, model))
                                }
                            }
                        }),
                        model_count: app
                            .models
                            .iter()
                            .filter(|model| model.backend == tag)
                            .count(),
                    }
                })
                .collect();
            let _ = reply.send(Ok(backends));
        }
        HostRequest::Messages { session_id, reply } => {
            let _ = reply.send(
                app.session_messages(&session_id)
                    .map_err(|error| error.to_string()),
            );
        }
        HostRequest::Command { command, reply } => {
            let result = app.execute(command).map_err(|error| error.to_string());
            let _ = reply.send(result);
        }
        HostRequest::ToolDefs { reply } => {
            let _ = reply.send(Ok(app.toolbox.defs()));
        }
        HostRequest::Toolbox { space_id, reply } => {
            let result = if space_id.is_some_and(|id| id != app.active_space.id) {
                Err("active space changed; retry the operation".into())
            } else {
                Ok(app.toolbox.clone())
            };
            let _ = reply.send(result);
        }
        HostRequest::PutBlob {
            space_id,
            name,
            hash,
            bytes,
            reply,
        } => {
            let result =
                crate::sync::put_blob(&app.db, &app.space, &space_id, &name, &hash, &bytes)
                    .map_err(|error| error.to_string());
            if result.is_ok() {
                // Invalidate the stat shortcut too: a replacement upload can have
                // the same size and land in the same mtime second.
                if let Some(file) = app
                    .db
                    .list_files(&space_id)
                    .ok()
                    .and_then(|files| files.into_iter().find(|file| file.name == name))
                {
                    let _ = app.db.set_file_mtime(&file.id, 0);
                }
                if space_id == app.active_space.id {
                    // The blob route is also an upload path; without a rescan the
                    // file would exist on disk but never receive chunks or embeddings.
                    app.rescan_files();
                }
            }
            let _ = reply.send(result);
        }
        HostRequest::GetBlob {
            space_id,
            name,
            reply,
        } => {
            let result = crate::sync::read_blob(&app.db, &app.space, &space_id, &name)
                .map_err(|error| error.to_string());
            let _ = reply.send(result);
        }
        HostRequest::Sync { changeset, reply } => {
            let result = (|| -> Result<Changeset, String> {
                let (_summary, cursors) =
                    crate::sync::apply_changeset(&app.db, &app.space, &changeset, None)
                        .map_err(|error| error.to_string())?;
                crate::host::browser_sync::refresh_after_merge(app)
                    .map_err(|error| error.to_string())?;
                let mut reply_changeset = crate::sync::build_changeset(
                    &app.db,
                    Some(&changeset.device_id),
                    &crate::sync::device_name(),
                )
                .map_err(|error| error.to_string())?;
                reply_changeset.ack = Some(cursors);
                Ok(reply_changeset)
            })();
            let _ = reply.send(result);
        }
        HostRequest::SetPublicBase { base, reply } => {
            let result = if let Some(server) = app.app_server.as_mut() {
                server.set_public_base(base);
                app.refresh_toolbox();
                Ok(())
            } else {
                Err("app server is unavailable".to_string())
            };
            let _ = reply.send(result);
        }
        HostRequest::GatewayRoute {
            model,
            override_tag,
            reply,
        } => {
            let result = gateway_route(app, &model, override_tag);
            let _ = reply.send(result);
        }
        HostRequest::LogGatewayUsage {
            route,
            usage,
            reply,
        } => {
            let usage = usage.into_usage();
            let result = app
                .db
                .record_usage(
                    route.tag.name(),
                    &route.model,
                    usage,
                    &crate::db::UsageLog {
                        path: app.space.db_path(),
                        session_id: None,
                        space_id: app.active_space.id.clone(),
                    },
                    "gateway",
                )
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = reply.send(result);
        }
    }
}

fn apply_domain_event(app: &mut App, event: AppEvent) {
    match event {
        AppEvent::Stream(Some((task_id, event))) => {
            if let Err(error) = app.on_chat_event(task_id, event) {
                app.push_status(error.to_string());
            }
        }
        AppEvent::Models(result) => app.on_models_result(result),
        AppEvent::Title(result) => app.on_title_result(result),
        AppEvent::Compact(result) => app.on_compact_result(result),
        AppEvent::SkillInstall(result) => app.on_skill_install_result(result),
        AppEvent::Convert(result) => app.on_convert_done(result),
        AppEvent::Embed(result) => app.on_embed_done(result),
        AppEvent::Research(result) => app.on_research_done(result),
        AppEvent::ResearchTopic(result) => app.on_research_topic_derived(result),
        AppEvent::Login(result) => app.on_login_result(result),
        AppEvent::UpdateCheck(result) => app.on_update_check(result),
        AppEvent::Swarm(result) => app.on_swarm_update(result),
        AppEvent::LocalServers(result) => app.on_local_status(result),
        AppEvent::Status(_)
        | AppEvent::ComposerSet(_)
        | AppEvent::ComposerClear
        | AppEvent::ViewportReset
        | AppEvent::HistoryInvalidated
        | AppEvent::OpenLoginPopup
        | AppEvent::Gate(_)
        | AppEvent::Stream(None) => {}
    }
}

fn clear_closed_source(app: &mut App, event: &AppEvent) {
    match event {
        AppEvent::Models(None) => app.models_rx = None,
        AppEvent::Title(None) => app.title_rx = None,
        AppEvent::Compact(None) => app.compaction.rx = None,
        AppEvent::SkillInstall(None) => app.skills_rx = None,
        AppEvent::Convert(None) => app.index.convert_rx = None,
        AppEvent::Embed(None) => app.index.embed_rx = None,
        AppEvent::Research(None) => app.research.rx = None,
        AppEvent::ResearchTopic(None) => app.research.topic_rx = None,
        AppEvent::Login(None) => app.login_rx = None,
        AppEvent::Swarm(None) => app.swarm.rx = None,
        AppEvent::UpdateCheck(None) => app.update_rx = None,
        AppEvent::LocalServers(None) => app.local.status_rx = None,
        _ => {}
    }
}

pub(super) enum HostRequest {
    ScriptJob {
        space_id: String,
        name: String,
        args: Vec<String>,
        reply: oneshot::Sender<Result<crate::host::media::ScriptJob, String>>,
    },
    SessionEdit {
        space_id: String,
        id: String,
        title: Option<String>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Bundle {
        input: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    MediaBlob {
        query: String,
        reply: oneshot::Sender<Result<crate::host::media::MediaBlob, String>>,
    },
    Surface {
        route: String,
        method: String,
        query: String,
        body: Vec<u8>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Apps {
        route: String,
        method: String,
        space_id: Option<String>,
        name: String,
        file: String,
        edit: Option<crate::host::apps::Edit>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    WorkspaceWrite {
        name: String,
        space_id: Option<String>,
        bytes: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Workspace {
        files: bool,
        space_id: Option<String>,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Snapshot {
        reply: oneshot::Sender<Result<CoreSnapshot, String>>,
    },
    Models {
        reply: oneshot::Sender<Result<Vec<WireModel>, String>>,
    },
    Usage {
        range: UsageRange,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Backends {
        reply: oneshot::Sender<Result<Vec<BackendInfo>, String>>,
    },
    Messages {
        session_id: String,
        reply: oneshot::Sender<Result<Vec<crate::app::MessageSnapshot>, String>>,
    },
    Command {
        command: AppCommand,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ToolDefs {
        reply: oneshot::Sender<Result<Vec<crate::provider::ToolDef>, String>>,
    },
    Toolbox {
        space_id: Option<String>,
        reply: oneshot::Sender<Result<Arc<dyn ToolExecutor>, String>>,
    },
    Sync {
        changeset: Changeset,
        reply: oneshot::Sender<Result<Changeset, String>>,
    },
    PutBlob {
        space_id: String,
        name: String,
        hash: String,
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GetBlob {
        space_id: String,
        name: String,
        reply: oneshot::Sender<Result<Option<Vec<u8>>, String>>,
    },
    SetPublicBase {
        base: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GatewayRoute {
        model: String,
        override_tag: Option<BackendTag>,
        reply: oneshot::Sender<Result<GatewayRoute, String>>,
    },
    LogGatewayUsage {
        route: GatewayRoute,
        usage: GatewayUsage,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

pub(super) async fn ask_actor<T, F>(
    requests: &mpsc::Sender<HostRequest>,
    make: F,
) -> Result<T, String>
where
    F: FnOnce(oneshot::Sender<Result<T, String>>) -> HostRequest,
{
    let (reply, receiver) = oneshot::channel();
    requests
        .send(make(reply))
        .await
        .map_err(|_| "host actor stopped".to_string())?;
    tokio::time::timeout(ACTOR_REQUEST_TIMEOUT, receiver)
        .await
        .map_err(|_| "host actor request timed out".to_string())?
        .map_err(|_| "host actor stopped".to_string())?
}
