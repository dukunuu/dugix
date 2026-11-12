use crate::provider::openrouter::OpenRouter;
use crate::provider::{BackendTag, Model};

#[derive(Clone, Default)]
pub struct Backends {
    pub openrouter: Option<OpenRouter>,
    pub openai: Option<OpenRouter>,
    pub opencode: Option<OpenRouter>,
    pub codex: Option<OpenRouter>,
    pub mistral: Option<OpenRouter>,
    pub local: Option<OpenRouter>,
}

impl Backends {
    pub const fn any(&self) -> bool {
        self.openrouter.is_some()
            || self.openai.is_some()
            || self.opencode.is_some()
            || self.codex.is_some()
            || self.mistral.is_some()
            || self.local.is_some()
    }

    pub const fn configured(&self, tag: BackendTag) -> bool {
        self.get(tag).is_some()
    }

    pub const fn get(&self, tag: BackendTag) -> Option<&OpenRouter> {
        match tag {
            BackendTag::OpenRouter => self.openrouter.as_ref(),
            BackendTag::OpenAi => self.openai.as_ref(),
            BackendTag::OpencodeGo => self.opencode.as_ref(),
            BackendTag::Codex => self.codex.as_ref(),
            BackendTag::Mistral => self.mistral.as_ref(),
            BackendTag::Local => self.local.as_ref(),
        }
    }

    pub fn set(&mut self, tag: BackendTag, provider: OpenRouter) {
        match tag {
            BackendTag::OpenRouter => self.openrouter = Some(provider),
            BackendTag::OpenAi => self.openai = Some(provider),
            BackendTag::OpencodeGo => self.opencode = Some(provider),
            BackendTag::Codex => self.codex = Some(provider),
            BackendTag::Mistral => self.mistral = Some(provider),
            BackendTag::Local => self.local = Some(provider),
        }
    }

    pub fn configured_tags(&self) -> Vec<BackendTag> {
        [
            BackendTag::OpenRouter,
            BackendTag::OpenAi,
            BackendTag::OpencodeGo,
            BackendTag::Codex,
            BackendTag::Mistral,
            BackendTag::Local,
        ]
        .into_iter()
        .filter(|t| self.get(*t).is_some())
        .collect()
    }

    /// Resolve a composite model id to its provider and wire id.
    pub fn resolve(&self, composite_id: &str) -> Option<(OpenRouter, String)> {
        for tag in [
            BackendTag::OpenAi,
            BackendTag::OpencodeGo,
            BackendTag::Codex,
            BackendTag::Mistral,
            BackendTag::Local,
        ] {
            if let Some(raw) = composite_id.strip_prefix(tag.key_prefix()) {
                return self.get(tag).cloned().map(|p| (p, raw.to_string()));
            }
        }
        // Persisted bare model ids belong to OpenRouter when it is configured.
        self.get(BackendTag::OpenRouter)
            .or_else(|| self.configured_tags().first().and_then(|t| self.get(*t)))
            .cloned()
            .map(|p| (p, composite_id.to_string()))
    }
}

/// A persistent model key, backend-prefixed except for `OpenRouter`.
pub fn composite_id(m: &Model) -> String {
    format!("{}{}", m.backend.key_prefix(), m.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_routes_prefixed_models_to_their_backend_even_with_openrouter_configured() {
        let mut backends = Backends::default();
        backends.set(
            BackendTag::OpenRouter,
            OpenRouter::openrouter_flavor("or".into()),
        );
        backends.set(BackendTag::OpenAi, OpenRouter::openai("oa".into()));
        backends.set(BackendTag::OpencodeGo, OpenRouter::opencode_go("go".into()));
        backends.set(BackendTag::Codex, OpenRouter::openai_codex("codex".into()));
        backends.set(BackendTag::Mistral, OpenRouter::mistral("vibe".into()));

        let (provider, raw) = backends.resolve("openai:gpt-4.1-mini").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::OpenAi);
        assert_eq!(raw, "gpt-4.1-mini");

        let (provider, raw) = backends.resolve("opencode:deepseek-v4-flash").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::OpencodeGo);
        assert_eq!(raw, "deepseek-v4-flash");

        let (provider, raw) = backends.resolve("codex:gpt-5.4-mini").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::Codex);
        assert_eq!(raw, "gpt-5.4-mini");

        let (provider, raw) = backends.resolve("mistral:mistral-vibe-cli-latest").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::Mistral);
        assert_eq!(raw, "mistral-vibe-cli-latest");
    }

    #[test]
    fn local_and_openai_are_independent_backends() {
        let mut backends = Backends::default();
        backends.set(BackendTag::OpenAi, OpenRouter::openai("cloud-key".into()));
        backends.set(
            BackendTag::Local,
            OpenRouter::local("http://localhost:11434/v1").unwrap(),
        );
        let (provider, raw) = backends.resolve("local:org/model:latest").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::Local);
        assert_eq!(raw, "org/model:latest");
        assert_eq!(
            backends
                .resolve("openai:org/model:latest")
                .unwrap()
                .0
                .backend_tag(),
            BackendTag::OpenAi
        );
        assert_eq!(
            backends.configured_tags(),
            [BackendTag::OpenAi, BackendTag::Local]
        );
        backends.local = None;
        assert!(backends.resolve("local:org/model:latest").is_none());
    }

    #[test]
    fn resolve_keeps_bare_ids_on_openrouter_for_legacy_settings() {
        let mut backends = Backends::default();
        backends.set(
            BackendTag::OpenRouter,
            OpenRouter::openrouter_flavor("or".into()),
        );
        backends.set(BackendTag::OpenAi, OpenRouter::openai("oa".into()));

        let (provider, raw) = backends.resolve("google/gemini-2.5-flash-lite").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::OpenRouter);
        assert_eq!(raw, "google/gemini-2.5-flash-lite");
    }
}
