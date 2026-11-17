use anyhow::{Context as _, Result};
use chrono::Utc;

use crate::provider::BackendTag;
use crate::provider::local::{LocalConfig, LocalRuntime};
use crate::provider::openrouter::OpenRouter;
use crate::provider::serve::{LaunchOptions, RuntimeStatus};

use super::{App, ModelPickTarget, Popup};

const ROUTER_MODEL_IDS: &[&str] = &["typesafe/jev-router", "openrouter/auto"];

fn split_version(id: &str) -> Option<(&str, &str, &str)> {
    let start = id.find(|c: char| c.is_ascii_digit())?;
    let len = id[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(id.len() - start);
    let version = id[start..start + len].trim_end_matches('.');
    let end = start + version.len();
    Some((&id[..start], version, &id[end..]))
}

fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

impl App {
    pub fn rebuild_all_backends(&mut self) {
        self.experiments = self.saved.experiments.clone();
        self.backends = super::Backends::default();
        for tag in BackendTag::ALL {
            if let Some(key) = self.saved.key(tag) {
                self.backends
                    .set(tag, OpenRouter::with_key(tag, key.to_string()));
            }
        }
        if let Some(config) = self.saved.local.clone() {
            match OpenRouter::configured_local(config) {
                Ok(provider) => self.backends.set(BackendTag::Local, provider),
                Err(error) => self.push_status(format!("local inference: {error}")),
            }
        }
        if let Some(c) = self.saved.codex.clone() {
            self.backends
                .set(BackendTag::Codex, OpenRouter::openai_codex(c.access));
        }
        if self.backends.any() {
            self.push_status("loading models…  (/model to pick, /help for commands)".to_string());
        }
    }

    pub fn configure_local(&mut self, spec: &str) {
        if spec.trim().is_empty() {
            let status = match &self.saved.local {
                Some(config) => format!(
                    "local inference: {} at {}",
                    config.provider.label(),
                    config.endpoint()
                ),
                None => "local inference is off — /local <ollama|llama-cpp|mlx|mlx-serve|lmstudio|edge0>"
                    .to_string(),
            };
            self.push_status(status);
            return;
        }
        let applied = LocalConfig::from_spec(spec, self.saved.local.as_ref())
            .and_then(|config| self.set_local_runtime(config));
        if let Err(error) = applied {
            self.push_status(format!("local inference: {error}"));
        }
    }

    pub fn manage_local_server(&mut self, verb: &str, runtime: &str) {
        if let Err(error) = self.local_server_action(verb, runtime) {
            self.push_status(format!("local server: {error}"));
            return;
        }
        self.local.status_announce = true;
        self.refresh_local_status();
    }

    fn local_server_action(&mut self, verb: &str, runtime: &str) -> Result<()> {
        if verb == "status" {
            return Ok(());
        }
        let runtime = if runtime.trim().is_empty() {
            self.saved
                .local
                .as_ref()
                .map(|config| config.provider)
                .context(
                    "local inference is off — /local <ollama|llama-cpp|mlx|mlx-serve|lmstudio|edge0> first",
                )?
        } else {
            LocalRuntime::parse(runtime.trim())
                .with_context(|| format!("unknown local runtime {:?}", runtime.trim()))?
        };
        let config = LocalConfig::for_runtime(runtime, self.saved.local.as_ref());
        let port = config
            .port()
            .context("the configured endpoint has no port to serve on")?;
        if matches!(verb, "stop" | "restart") {
            self.local.servers.stop(runtime, port, verb == "restart")?;
            self.push_status(format!("{} server stopped", runtime.label()));
            if verb == "stop" {
                return Ok(());
            }
        }
        // mlx-lm and edge0 require a single model at server launch.
        let model = self
            .current_model
            .as_deref()
            .and_then(|id| id.strip_prefix("local:"))
            .map(str::to_owned);
        let options = LaunchOptions {
            context_length: config.effective_context(),
            kv_cache_q8: config.kv_cache_q8,
        };
        let argv = self
            .local
            .servers
            .start(runtime, model.as_deref(), port, options)?;
        self.push_status(format!(
            "{} starting: {argv} — /local status when it answers",
            runtime.label()
        ));
        Ok(())
    }

    pub fn refresh_local_status(&mut self) {
        if self.local.status_rx.is_some() {
            return;
        }
        self.local.servers.reap();
        let configured = self.saved.local.clone();
        let managed: Vec<(LocalRuntime, Option<u32>)> = self
            .local
            .servers
            .managed_runtimes()
            .into_iter()
            .map(|runtime| (runtime, self.local.servers.pid(runtime)))
            .collect();
        // Synchronous frontends may lack the reactor required by the survey.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.local.status_rx = Some(rx);
        handle.spawn(async move {
            let _ = tx.send(crate::provider::serve::survey(configured.as_ref(), &managed).await);
        });
    }

    /// Report local runtime health without interrupting servers over their advisory budget.
    pub fn on_local_status(&mut self, result: Option<Vec<RuntimeStatus>>) {
        self.local.status_rx = None;
        let Some(statuses) = result else {
            return;
        };
        let announce = std::mem::take(&mut self.local.status_announce);
        self.local.status = statuses;
        if announce {
            let line = self
                .local
                .status
                .iter()
                .map(RuntimeStatus::summary)
                .collect::<Vec<_>>()
                .join("  ·  ");
            self.push_status(format!("local: {line}"));
        }
        if let Some(over) = self.local.status.iter().find(|status| status.over_budget) {
            let budget = over
                .budget_kb
                .map(crate::provider::serve::format_kb)
                .unwrap_or_default();
            self.push_status(format!(
                "⚠ {} is using {} — over the {budget} budget; /local stop {} to reclaim it",
                over.runtime.label(),
                over.rss_kb
                    .map(crate::provider::serve::format_kb)
                    .unwrap_or_default(),
                over.runtime.token()
            ));
        }
    }

    pub async fn stop_local_servers(&mut self) {
        self.local.servers.stop_all().await;
    }

    pub fn set_local_runtime(&mut self, config: Option<LocalConfig>) -> Result<()> {
        if let Some(config) = &config {
            config.validate()?;
        }
        crate::config::save_local_config(config.as_ref())?;
        let status = match &config {
            Some(config) => format!(
                "local inference: {} at {} — loading models…",
                config.provider.label(),
                config.endpoint()
            ),
            None => "local inference disabled".to_string(),
        };
        self.saved.local = config;
        if self.saved.local.is_none() {
            // With no providers, `fetch_models` returns before clearing stale local selections.
            self.models
                .retain(|model| model.backend != BackendTag::Local);
            if self
                .current_model
                .as_deref()
                .is_some_and(|id| id.starts_with(BackendTag::Local.key_prefix()))
            {
                self.current_model = None;
            }
        }
        self.rebuild_all_backends();
        self.fetch_models();
        self.push_status(status);
        Ok(())
    }

    pub fn resolve_model_backend(&self, id: &str) -> Option<(OpenRouter, String)> {
        self.backends.resolve(id)
    }

    /// Use the session's own model for background work.
    pub fn session_backend(&self) -> Option<(OpenRouter, String)> {
        self.current_model
            .as_deref()
            .and_then(|id| self.resolve_model_backend(id))
            .map(|(p, model)| (self.usage_provider(p, "background"), model))
    }

    /// Resolve media generation on the session's backend without a cross-provider fallback.
    pub fn generation_backend(
        &self,
        default: fn(&OpenRouter) -> &'static str,
    ) -> Option<(OpenRouter, String)> {
        let provider = self.active_provider()?;
        if provider.backend_tag() == BackendTag::Local {
            return None;
        }
        let model = default(&provider);
        (!model.is_empty()).then(|| {
            (
                self.usage_provider(provider.clone(), "media"),
                model.to_string(),
            )
        })
    }

    fn active_provider(&self) -> Option<OpenRouter> {
        self.current_model
            .as_deref()
            .and_then(|id| self.resolve_model_backend(id).map(|(provider, _)| provider))
            .or_else(|| {
                self.backends
                    .configured_tags()
                    .first()
                    .and_then(|tag| self.backends.get(*tag).cloned())
            })
    }

    pub fn resolve_feature_model_backend(
        &self,
        configured_id: &str,
        default: fn(&OpenRouter) -> &'static str,
    ) -> Option<(OpenRouter, String)> {
        let configured_id = configured_id.trim();
        if !configured_id.is_empty()
            && let Some((provider, raw)) = self.resolve_model_backend(configured_id)
            && self.resolved_model_looks_valid(configured_id, provider.backend_tag(), &raw)
        {
            return Some((provider, raw));
        }

        let provider = self.active_provider()?;
        if provider.backend_tag() == BackendTag::Local {
            // Local runtimes require installed models, not cloud defaults.
            let raw = self
                .current_model
                .as_deref()
                .and_then(|id| self.resolve_model_backend(id))
                .filter(|(backend, _)| backend.backend_tag() == BackendTag::Local)
                .map(|(_, raw)| raw)
                .or_else(|| {
                    self.models
                        .iter()
                        .find(|model| model.backend == BackendTag::Local)
                        .map(|model| model.id.clone())
                })?;
            return Some((provider, raw));
        }
        let tag = provider.backend_tag();
        let auto = self.newest_listed(tag, default(&provider));
        let auto = auto.as_str();
        let listed =
            self.models.is_empty() || self.models.iter().any(|m| m.backend == tag && m.id == auto);
        if auto.is_empty() || !listed {
            // Model ids are provider-specific; fallback must stay on this backend.
            if let Some(raw) = self
                .current_model
                .as_deref()
                .and_then(|id| self.resolve_model_backend(id))
                .filter(|(backend, _)| backend.backend_tag() == tag)
                .map(|(_, raw)| raw)
            {
                return Some((provider, raw));
            }
        }
        (!auto.is_empty()).then(|| (provider, auto.to_string()))
    }

    /// Upgrade a default model within its version family using the live catalog.
    pub fn newest_listed(&self, tag: BackendTag, base: &str) -> String {
        let Some((prefix, version, suffix)) = split_version(base) else {
            return base.to_string();
        };
        let floor = version_key(version);
        self.models
            .iter()
            .filter(|m| m.backend == tag)
            .filter_map(|m| {
                let (p, v, s) = split_version(&m.id)?;
                (p == prefix && s == suffix).then(|| (version_key(v), &m.id))
            })
            .filter(|(key, _)| *key > floor)
            .max_by(|a, b| a.0.cmp(&b.0))
            .map_or_else(|| base.to_string(), |(_, id)| id.clone())
    }

    fn resolved_model_looks_valid(
        &self,
        original_id: &str,
        backend: BackendTag,
        raw: &str,
    ) -> bool {
        if self.models.is_empty() {
            // Slashed bare ids are OpenRouter defaults, not ids for other cloud backends.
            return backend != BackendTag::Local
                && (backend == BackendTag::OpenRouter || !original_id.contains('/'));
        }
        self.models
            .iter()
            .any(|m| m.backend == backend && m.id == raw)
    }

    /// Merge backend catalogs, retaining successful results when others fail.
    pub fn fetch_models(&mut self) {
        let providers: Vec<OpenRouter> = self
            .backends
            .configured_tags()
            .into_iter()
            .filter_map(|tag| self.backends.get(tag).cloned())
            .collect();
        if providers.is_empty() {
            return;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.models_rx = Some(rx);
        tokio::spawn(async move {
            let mut set = tokio::task::JoinSet::new();
            for p in providers {
                set.spawn(async move { p.list_models().await });
            }
            let mut merged = Vec::new();
            let mut errors = Vec::new();
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(Ok(models)) => merged.extend(models),
                    Ok(Err(e)) => errors.push(e.to_string()),
                    Err(e) => errors.push(e.to_string()),
                }
            }
            let result = if merged.is_empty() && !errors.is_empty() {
                Err(errors.join("; "))
            } else {
                merged.sort_by(|a, b| a.id.cmp(&b.id));
                crate::provider::openrouter::price_mistral_models(&mut merged);
                Ok(merged)
            };
            let _ = tx.send(result);
        });
    }

    pub fn context_limit(&self) -> Option<u64> {
        let id = self.current_model.as_deref()?;
        self.models
            .iter()
            .find(|m| super::composite_id(m) == id)
            .and_then(|m| m.context_length)
    }

    /// Estimate context growth relative to the provider's latest exact count.
    pub fn context_used(&self) -> u64 {
        let streaming = self.is_streaming();
        if let (Some(total), Some(basis)) = (self.meter.total, self.meter.basis)
            && total == basis.exact
        {
            if !streaming && self.messages.len() == basis.messages {
                return total;
            }
            let delta = self.context_chars() as f64 - basis.chars as f64;
            let tokens = total as f64 + self.calibrated(delta / 4.0);
            return tokens.max(0.0).round() as u64;
        }
        if !streaming && let Some(total) = self.meter.total {
            return total;
        }
        let estimate = self.calibrated(self.context_chars() as f64 / 4.0) as u64;
        // An estimate must not shrink the bar before the provider reports exact usage.
        if streaming {
            self.meter
                .total
                .map_or(estimate, |total| total.max(estimate))
        } else {
            estimate
        }
    }

    fn calibrated(&self, tokens: f64) -> f64 {
        tokens * self.meter.ratio.unwrap_or(1.0)
    }

    pub(crate) fn context_chars(&self) -> usize {
        let mut chars = self.system_prompt().chars().count();
        if let Some(s) = self
            .session
            .as_ref()
            .and_then(|s| s.compact_summary.as_deref())
        {
            chars += s.chars().count();
        }
        if let Some(name) = &self.forced_skill
            && let Some(skill) = self.skills.iter().find(|s| &s.name == name)
        {
            chars += std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map_or(0, |md| crate::skills::skill_body(&md).chars().count());
        }
        chars += self.effective_conversation_chars();
        if let Some(buf) = self.active_streaming_text() {
            chars += buf.chars().count();
        }
        chars
    }

    /// Record context usage without letting a nested sub-agent's smaller prompt move the bar.
    pub(crate) fn observe_context(&mut self, task: super::ChatTaskId, total: u64) {
        if self
            .meter
            .basis
            .is_some_and(|basis| basis.task == task && total < basis.exact)
        {
            return;
        }
        let chars = self.context_chars();
        if chars > 0 && total > 0 {
            self.meter.ratio = Some((total as f64 / (chars as f64 / 4.0)).clamp(0.5, 2.0));
        }
        self.meter.total = Some(total);
        self.meter.basis = Some(super::ContextBasis {
            exact: total,
            chars,
            messages: self.messages.len(),
            task,
        });
    }

    pub fn current_model_supports_images(&self) -> bool {
        self.current_model.as_deref().is_some_and(|id| {
            self.models
                .iter()
                .any(|m| super::composite_id(m) == id && m.supports_images)
        })
    }

    pub fn reasoning_of(&self, id: &str) -> Option<&str> {
        let effort = self.reasoning.get(id)?.as_str();
        self.effort_accepted(id, effort).then_some(effort)
    }

    /// Validate reasoning effort when the model catalog is available.
    pub fn effort_accepted(&self, model: &str, effort: &str) -> bool {
        self.models
            .iter()
            .find(|m| super::composite_id(m) == model)
            .is_none_or(|m| m.reasoning_efforts.iter().any(|e| e.as_str() == effort))
    }

    pub fn pick_model(&mut self, id: &str) -> Result<()> {
        match self.model_pick_target {
            ModelPickTarget::Session => {
                let model_changed = self.current_model.as_deref() != Some(id);
                if model_changed {
                    self.bump_cache_epoch();
                }
                self.current_model = Some(id.to_string());
                if model_changed && !self.is_streaming() {
                    self.refresh_toolbox();
                }
                if let Some(session) = &self.session {
                    self.db.set_session_model(&session.id, id)?;
                }
                self.db.mark_model_used(id)?;
                self.last_used
                    .insert(id.to_string(), Utc::now().to_rfc3339());
                self.push_status(format!("model: {id}"));
                if !self.router_note_shown && ROUTER_MODEL_IDS.contains(&id) {
                    self.router_note_shown = true;
                    self.push_status("router models pick a model per request · prompt caching is less effective · compare with dugix eval --compare-model first");
                }
            }
            ModelPickTarget::SwarmPersona(row) => {
                if let Some(p) = self.swarm.cache.get_mut(row) {
                    p.model = id.to_string();
                }
                if let Some(session) = &self.session {
                    let _ = self.db.save_swarm_personas(&session.id, &self.swarm.cache);
                }
                self.push_status(format!("drafter model: {id}"));
            }
        }
        if self.work_mode == super::WorkMode::AgentAuto
            && self.model_pick_target == ModelPickTarget::Session
        {
            self.refresh_toolbox();
        }
        Ok(())
    }

    pub fn popup_after_pick(target: ModelPickTarget) -> Popup {
        match target {
            ModelPickTarget::Session => Popup::None,
            ModelPickTarget::SwarmPersona(_) => Popup::Swarm,
        }
    }

    pub fn start_codex_login(&mut self) {
        // Cancelled or timed-out device logins cannot be resumed through the UI.
        self.login_rx = None;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.login_rx = Some(rx);
        self.push_status("starting OpenAI Codex login…".to_string());
        tokio::spawn(async move {
            let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let forward = tx.clone();
            tokio::spawn(async move {
                while let Some(s) = status_rx.recv().await {
                    let _ = forward.send(super::LoginMsg::Status(s));
                }
            });
            let result = crate::config::login_openai_codex_device(status_tx)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(super::LoginMsg::Done(result));
        });
    }

    pub fn on_login_result(&mut self, msg: Option<super::LoginMsg>) {
        match msg {
            Some(super::LoginMsg::Status(s)) => self.push_status(s),
            Some(super::LoginMsg::Done(Ok(creds))) => {
                self.login_rx = None;
                self.backends.set(
                    BackendTag::Codex,
                    OpenRouter::openai_codex(creds.access.clone()),
                );
                self.saved.codex = Some(creds);
                self.push_status("OpenAI Codex login saved, loading models…".to_string());
                self.fetch_models();
                self.refresh_toolbox();
            }
            Some(super::LoginMsg::Done(Err(e))) => {
                self.login_rx = None;
                self.push_status(format!("OpenAI Codex login failed: {e}"));
            }
            None => self.login_rx = None,
        }
    }
}

#[cfg(test)]
mod router_tests {
    use super::*;
    use crate::app::{AppCommand, AppEvent};

    #[test]
    fn router_note_is_once_per_run_and_only_for_session_selection() {
        for router in ["typesafe/jev-router", "openrouter/auto"] {
            let mut app = crate::app::tests::app_with_key();
            app.model_pick_target = ModelPickTarget::SwarmPersona(0);
            app.pick_model(router).unwrap();
            assert!(!app.router_note_shown);
            app.model_pick_target = ModelPickTarget::Session;
            let session = app
                .db
                .create_session("router", "a/one", &app.active_space.id, "chat")
                .unwrap();
            app.session = Some(session.clone());
            app.execute(AppCommand::SetModel { id: "a/one".into() })
                .unwrap();
            assert!(!app.router_note_shown);
            app.pending_events.clear();
            for id in [router, router, "a/one", "openrouter/auto"] {
                app.execute(AppCommand::SetModel { id: id.into() }).unwrap();
                assert_eq!(app.current_model.as_deref(), Some(id));
                assert_eq!(app.db.get_session(&session.id).unwrap().unwrap().model, id);
            }
            let notes: Vec<_> = app
                .pending_events
                .iter()
                .filter_map(|event| match event {
                    AppEvent::Status(text) if text.starts_with("router models") => Some(text),
                    _ => None,
                })
                .collect();
            assert_eq!(notes.len(), 1);
            assert_eq!(
                notes[0],
                "router models pick a model per request · prompt caching is less effective · compare with dugix eval --compare-model first"
            );
            let root = app.space.root.clone();
            drop(app);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
