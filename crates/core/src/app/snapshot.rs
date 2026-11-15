//! App state exposed through the host API.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::App;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreSnapshot {
    /// The selected session, or `None` until the next message creates one.
    pub active_session_id: Option<String>,
    pub active_space_id: String,
    pub active_space_name: String,
    pub sessions: Vec<SessionSnapshot>,
    pub models: Vec<ModelSnapshot>,
    pub settings: SettingsSnapshot,
    pub tasks: Vec<TaskSnapshot>,
    /// Messages of the active session waiting for its running reply.
    #[serde(default)]
    pub queued: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub title: String,
    pub slug: Option<String>,
    pub model: String,
    pub kind: String,
    pub created_at: String,
}

/// A transcript row without provider credentials or internal database identifiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSnapshot {
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub tokens: Option<i64>,
    pub secs: Option<f64>,
    pub cost: Option<f64>,
    pub phrase: Option<String>,
    pub persona: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSnapshot {
    /// Persistent model key combining backend and wire id.
    pub id: String,
    pub name: String,
    pub context_length: Option<u64>,
    pub favorite: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SettingsSnapshot {
    #[serde(default)]
    pub show_stats: bool,
    #[serde(default)]
    pub show_reasoning: bool,
    #[serde(default)]
    pub hide_hints: bool,
    pub model: Option<String>,
    pub verbosity: String,
    pub incognito: bool,
    /// Sanitized `SearXNG` endpoint (credentials/query strings are removed).
    pub searxng_url: String,
    /// Credential availability only; the key must never cross the host API boundary.
    #[serde(default)]
    pub langsearch_configured: bool,
    pub search_provider: String,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub compact_threshold: u8,
    pub blocked_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: u64,
    pub session_id: String,
    pub session_title: String,
    pub model: String,
    pub backend: String,
    /// Active tasks are either `tool` or `streaming`, never idle.
    pub status: String,
    pub buffer_chars: usize,
    /// Partial model output for recovery after an SSE reconnect.
    #[serde(default)]
    pub buffer: String,
}

// Endpoint URLs can contain credentials in userinfo, query, or fragment components.
fn sanitized_endpoint(value: &str) -> String {
    let value = value.trim();
    let value = value.split_once('#').map_or(value, |(prefix, _)| prefix);
    let value = value.split_once('?').map_or(value, |(prefix, _)| prefix);
    if let Some((scheme, authority)) = value.split_once("://")
        && let Some((_, host)) = authority.rsplit_once('@')
    {
        return format!("{scheme}://{host}");
    }
    value.to_string()
}

impl App {
    pub fn session_messages(&self, session_id: &str) -> Result<Vec<MessageSnapshot>> {
        self.db
            .load_messages(session_id)
            .context("reading session messages")
            .map(|messages| {
                messages
                    .into_iter()
                    .map(|message| MessageSnapshot {
                        role: message.role,
                        content: message.content,
                        model: message.model,
                        reasoning: message.reasoning,
                        tokens: message.tokens,
                        secs: message.secs,
                        cost: message.cost,
                        phrase: message.phrase,
                        persona: message.persona,
                        created_at: message.created_at,
                    })
                    .collect()
            })
    }

    pub fn snapshot(&self) -> Result<CoreSnapshot> {
        let sessions = if self.caches.sessions.is_empty() {
            self.db
                .list_sessions(&self.active_space.id)
                .context("reading sessions for snapshot")?
        } else {
            self.caches.sessions.clone()
        };
        let favorite_ids = &self.favorites;
        Ok(CoreSnapshot {
            active_session_id: self.session.as_ref().map(|session| session.id.clone()),
            active_space_id: self.active_space.id.clone(),
            active_space_name: self.active_space.name.clone(),
            sessions: sessions
                .into_iter()
                .map(|s| SessionSnapshot {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    slug: s.slug.clone(),
                    model: s.model.clone(),
                    kind: s.kind.clone(),
                    created_at: s.created_at.clone(),
                })
                .collect(),
            models: self
                .models
                .iter()
                .map(|m| ModelSnapshot {
                    id: super::composite_id(m),
                    name: m.name.clone(),
                    context_length: m.context_length,
                    favorite: favorite_ids.contains(&super::composite_id(m)),
                })
                .collect(),
            settings: SettingsSnapshot {
                show_stats: self.settings.show_stats,
                show_reasoning: self.settings.show_reasoning,
                hide_hints: self.settings.hide_hints,
                model: self.current_model.clone(),
                verbosity: self.verbosity.clone(),
                incognito: self.incognito,
                searxng_url: sanitized_endpoint(&self.search.searxng_url),
                langsearch_configured: !self.search.langsearch_key.trim().is_empty(),
                search_provider: self.search.provider.clone(),
                temperature: self.settings.temperature,
                top_p: self.settings.top_p,
                max_tokens: self.settings.max_tokens,
                compact_threshold: self.settings.compact_threshold,
                blocked_domains: self.blocked_domains(),
            },
            tasks: self
                .chat_tasks
                .values()
                .map(|t| TaskSnapshot {
                    id: t.id,
                    session_id: t.session_id.clone(),
                    session_title: t.session_title.clone(),
                    model: t.model.clone(),
                    backend: t.backend.name().to_string(),
                    status: if t.tool_status.is_some() {
                        "tool".to_string()
                    } else {
                        "streaming".to_string()
                    },
                    buffer_chars: t.buffer.chars().count(),
                    buffer: t.buffer.clone(),
                })
                .collect(),
            queued: self.queued_messages().to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_json_locks_wire_shape() {
        let snap = CoreSnapshot {
            active_session_id: Some("s1".into()),
            active_space_id: "sp1".into(),
            active_space_name: "Default".into(),
            sessions: vec![SessionSnapshot {
                id: "s1".into(),
                title: "hello".into(),
                slug: Some("hello".into()),
                model: "openrouter:anthropic/claude-sonnet-4".into(),
                kind: "chat".into(),
                created_at: "2025-01-01T00:00:00Z".into(),
            }],
            models: vec![ModelSnapshot {
                id: "openrouter:anthropic/claude-sonnet-4".into(),
                name: "Claude Sonnet 4".into(),
                context_length: Some(200_000),
                favorite: true,
            }],
            settings: SettingsSnapshot {
                show_stats: false,
                show_reasoning: false,
                hide_hints: false,
                model: Some("openrouter:anthropic/claude-sonnet-4".into()),
                verbosity: "high".into(),
                incognito: false,
                searxng_url: String::new(),
                langsearch_configured: false,
                search_provider: "searxng".into(),
                temperature: Some(0.7),
                top_p: None,
                max_tokens: None,
                compact_threshold: 40,
                blocked_domains: Vec::new(),
            },
            tasks: vec![TaskSnapshot {
                id: 1,
                session_id: "s1".into(),
                session_title: "hello".into(),
                model: "openrouter:anthropic/claude-sonnet-4".into(),
                backend: "OpenRouter".into(),
                status: "streaming".into(),
                buffer_chars: 12,
                buffer: "".into(),
            }],
            queued: vec!["and then this".into()],
        };
        let json = serde_json::to_string(&snap).expect("snapshot serializes");
        assert_eq!(
            json,
            r#"{"active_session_id":"s1","active_space_id":"sp1","active_space_name":"Default","sessions":[{"id":"s1","title":"hello","slug":"hello","model":"openrouter:anthropic/claude-sonnet-4","kind":"chat","created_at":"2025-01-01T00:00:00Z"}],"models":[{"id":"openrouter:anthropic/claude-sonnet-4","name":"Claude Sonnet 4","context_length":200000,"favorite":true}],"settings":{"show_stats":false,"show_reasoning":false,"hide_hints":false,"model":"openrouter:anthropic/claude-sonnet-4","verbosity":"high","incognito":false,"searxng_url":"","langsearch_configured":false,"search_provider":"searxng","temperature":0.7,"top_p":null,"max_tokens":null,"compact_threshold":40,"blocked_domains":[]},"tasks":[{"id":1,"session_id":"s1","session_title":"hello","model":"openrouter:anthropic/claude-sonnet-4","backend":"OpenRouter","status":"streaming","buffer_chars":12,"buffer":""}],"queued":["and then this"]}"#
        );
        let back: CoreSnapshot = serde_json::from_str(&json).expect("golden parses");
        assert_eq!(back.sessions[0].title, "hello");
        assert_eq!(back.sessions[0].slug.as_deref(), Some("hello"));
        assert_eq!(back.models[0].context_length, Some(200_000));
        assert_eq!(back.settings.temperature, Some(0.7));
        assert!(!back.settings.langsearch_configured);
        assert_eq!(back.settings.blocked_domains, Vec::<String>::new());
        assert_eq!(back.tasks[0].status, "streaming");
        assert_eq!(back.tasks[0].buffer_chars, 12);
    }

    #[test]
    fn sanitized_endpoint_drops_credentials_and_query() {
        assert_eq!(
            sanitized_endpoint("https://user:pass@example.test/search?token=secret#frag"),
            "https://example.test/search"
        );
        assert_eq!(
            sanitized_endpoint("http://localhost:8080"),
            "http://localhost:8080"
        );
    }
}
