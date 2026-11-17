//! Embedding selection and capability probing.

use crate::provider::embedding::{Embedder, discover, is_gemma};
use crate::provider::local::LocalRuntime;

use super::App;

impl App {
    pub(super) fn configured_embedder(&self) -> Option<Embedder> {
        let model = self.index.embedding_model.trim();
        if model.is_empty() {
            return None;
        }
        // A local selection must never send content to a cloud fallback.
        if let Some(raw) = model.strip_prefix("local:")
            && is_gemma(raw)
            && self
                .saved
                .local
                .as_ref()
                .is_none_or(|c| c.provider == LocalRuntime::Ollama)
        {
            return Embedder::ollama(self.embedding_endpoint(), raw.to_string()).ok();
        }
        self.backends.resolve(model).map(|(p, model)| {
            Embedder::openai(p.logged(self.background_usage_log(), "embed"), model)
        })
    }

    pub(super) fn file_embedder(&self) -> Option<Embedder> {
        let configured = self.configured_embedder()?;
        Some(
            self.index
                .embedding_backend
                .as_ref()
                .filter(|b| b.same_route(&configured))
                .cloned()
                .unwrap_or(configured)
                .logged(self.background_usage_log()),
        )
    }

    /// Invalidate the runtime probe without discarding cached vectors.
    pub fn reset_embedding_backend(&mut self) {
        self.index.embed_rx = None;
        self.index.embedding_backend = None;
        self.index.embedding_discovery = None;
        self.index.embedding_chunks.clear();
        self.index.embedding_chat_job = None;
        self.index.embedding_status.clear();
    }

    fn embedding_endpoint(&self) -> &str {
        self.saved
            .local
            .as_ref()
            .map_or("http://127.0.0.1:11434", |c| c.endpoint())
    }

    /// Discover local embeddings in the background without sending file content.
    pub fn discover_embeddings(&mut self) {
        if self.index.embed_rx.is_some()
            || self
                .saved
                .local
                .as_ref()
                .is_some_and(|c| c.provider != LocalRuntime::Ollama)
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let endpoint = self.embedding_endpoint().to_string();
        let selected = std::sync::Arc::new(std::sync::OnceLock::new());
        self.index.embedding_discovery = Some(selected.clone());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.index.embed_rx = Some(rx);
        let space = self.active_space.id.clone();
        handle.spawn(async move {
            if let Some(result) = discover(&endpoint).await {
                let _ = selected.set(result);
            }
            let _ = tx.send((space, String::new(), Ok(Vec::new())));
        });
    }

    pub(super) fn probe_embeddings(&mut self, embedder: Embedder) {
        let embedder = embedder.logged(self.background_usage_log());
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.index.embed_rx = Some(rx);
        self.index.embedding_status = "embeddings: probing…".to_string();
        let space = self.active_space.id.clone();
        handle.spawn(async move {
            let result = embedder
                .prepare()
                .await
                .map(|()| Vec::new())
                .map_err(|e| e.to_string());
            let _ = tx.send((space, String::new(), result));
        });
    }

    pub(super) fn on_embedding_probe(&mut self, result: Result<Vec<(i64, Vec<f32>)>, String>) {
        if let Some(selection) = self.index.embedding_discovery.take() {
            if let Some((model, embedder)) = selection.get() {
                self.index.embedding_model.clone_from(model);
                self.index.embedding_backend = Some(embedder.clone());
            } else {
                self.start_embedding();
                return;
            }
        }
        match result {
            Ok(_) => {
                if let Some(backend) = &self.index.embedding_backend {
                    self.index.embedding_status = backend.status();
                    if let Some(identity) = backend.identity() {
                        let _ = self
                            .db
                            .invalidate_embedding_identity(&self.active_space.id, &identity.key());
                    }
                }
                self.push_status(self.index.embedding_status.clone());
                self.refresh_toolbox();
                self.index.next_chat_index = std::time::Instant::now();
                self.rescan_files_with(&crate::extract::ExtractConfig::load());
            }
            Err(error) => {
                self.index.embedding_status = format!("embeddings: {error}");
                self.push_status(self.index.embedding_status.clone());
            }
        }
    }
}

#[cfg(test)]
#[path = "embedding_tests.rs"]
mod tests;
