//! Backend identity and the shared catalog of provider facts.

use serde::{Deserialize, Serialize};

/// Which configured backend a `Model` came from, so a pick routes back to it.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum BackendTag {
    OpenRouter,
    OpenAi,
    OpencodeGo,
    Codex,
    /// Mistral's API: a Vibe plan key draws on the plan's Vibe budget, a
    /// console key bills per token. Both hit the same endpoint.
    Mistral,
    Local,
}

/// API-key storage and interactive login facts.
#[derive(Debug)]
pub struct KeyLogin {
    pub env_var: &'static str,
    pub config_field: &'static str,
    pub config_name: &'static str,
    pub label: &'static str,
}

/// Facts shared by configuration, clients, the gateway and the UI.
#[derive(Debug)]
pub struct BackendSpec {
    pub tag: BackendTag,
    pub name: &'static str,
    pub key_prefix: &'static str,
    pub wire_prefix: &'static str,
    pub owned_by: &'static str,
    pub aliases: &'static [&'static str],
    pub base_url: &'static str,
    pub key: Option<KeyLogin>,
    pub default_helper_model: &'static str,
    pub default_research_model: &'static str,
    pub default_embedding_model: &'static str,
    pub default_image_gen_model: &'static str,
    pub default_video_gen_model: &'static str,
}

const BACKENDS: [BackendSpec; BackendTag::ALL.len()] = [
    BackendSpec {
        tag: BackendTag::OpenRouter,
        name: "OpenRouter",
        key_prefix: "",
        wire_prefix: "openrouter:",
        owned_by: "openrouter",
        aliases: &["openrouter", "router"],
        base_url: "https://openrouter.ai/api/v1",
        key: Some(KeyLogin {
            env_var: "OPENROUTER_API_KEY",
            config_field: "openrouter_key",
            config_name: "openrouter",
            label: "OpenRouter",
        }),
        default_helper_model: "openai/gpt-6-luna",
        default_research_model: "openai/gpt-6-sol",
        default_embedding_model: "google/gemini-embedding-2",
        default_image_gen_model: "openai/gpt-image-2",
        default_video_gen_model: "google/veo-3.1",
    },
    BackendSpec {
        tag: BackendTag::OpenAi,
        name: "OpenAI",
        key_prefix: "openai:",
        wire_prefix: "openai:",
        owned_by: "openai",
        aliases: &["openai"],
        base_url: "https://api.openai.com/v1",
        key: Some(KeyLogin {
            env_var: "OPENAI_API_KEY",
            config_field: "openai_key",
            config_name: "openai",
            label: "OpenAI",
        }),
        default_helper_model: "gpt-6-luna",
        default_research_model: "gpt-6-sol",
        default_embedding_model: "text-embedding-3-small",
        default_image_gen_model: "dall-e-3",
        default_video_gen_model: "",
    },
    BackendSpec {
        tag: BackendTag::OpencodeGo,
        name: "OpenCode Go",
        key_prefix: "opencode:",
        wire_prefix: "opencode:",
        owned_by: "opencode-go",
        aliases: &["opencode", "opencode-go", "opencode_go", "go"],
        base_url: "https://opencode.ai/zen/v1",
        key: Some(KeyLogin {
            env_var: "OPENCODE_API_KEY",
            config_field: "opencode_key",
            config_name: "opencode",
            label: "OpenCode Go",
        }),
        default_helper_model: "gpt-6-luna",
        default_research_model: "glm-5.3",
        default_embedding_model: "",
        default_image_gen_model: "",
        default_video_gen_model: "",
    },
    BackendSpec {
        tag: BackendTag::Codex,
        name: "Codex",
        key_prefix: "codex:",
        wire_prefix: "codex:",
        owned_by: "codex",
        aliases: &["codex"],
        base_url: "https://chatgpt.com/backend-api",
        key: None,
        default_helper_model: "gpt-6-luna",
        default_research_model: "gpt-6-sol",
        default_embedding_model: "",
        default_image_gen_model: "",
        default_video_gen_model: "",
    },
    BackendSpec {
        tag: BackendTag::Mistral,
        name: "Mistral",
        key_prefix: "mistral:",
        wire_prefix: "mistral:",
        owned_by: "mistral",
        aliases: &["mistral", "vibe"],
        base_url: "https://api.mistral.ai/v1",
        key: Some(KeyLogin {
            env_var: "MISTRAL_API_KEY",
            config_field: "mistral_key",
            config_name: "mistral",
            label: "Mistral Vibe",
        }),
        default_helper_model: "mistral-vibe-cli-fast",
        default_research_model: "mistral-vibe-cli-latest",
        default_embedding_model: "",
        default_image_gen_model: "",
        default_video_gen_model: "",
    },
    BackendSpec {
        tag: BackendTag::Local,
        name: "Local",
        key_prefix: "local:",
        wire_prefix: "local:",
        owned_by: "local",
        aliases: &["local"],
        base_url: "http://localhost:11434/v1",
        key: None,
        default_helper_model: "",
        default_research_model: "gpt-6-sol",
        default_embedding_model: "text-embedding-3-small",
        default_image_gen_model: "dall-e-3",
        default_video_gen_model: "",
    },
];

impl BackendTag {
    /// Backend routing and fallback order.
    pub const ALL: [Self; 6] = [
        Self::OpenRouter,
        Self::OpenAi,
        Self::OpencodeGo,
        Self::Codex,
        Self::Mistral,
        Self::Local,
    ];

    /// Shared facts for this backend.
    #[must_use]
    pub const fn info(self) -> &'static BackendSpec {
        &BACKENDS[self as usize]
    }

    /// Human-readable backend name, used in usage analytics.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.info().name
    }

    /// Prefix used to key per-backend model data; bare for `OpenRouter`.
    #[must_use]
    pub const fn key_prefix(self) -> &'static str {
        self.info().key_prefix
    }

    /// Prefix used for model ids crossing an OpenAI-wire API boundary.
    #[must_use]
    pub const fn wire_prefix(self) -> &'static str {
        self.info().wire_prefix
    }

    /// Parse the gateway's case-insensitive backend aliases.
    #[must_use]
    pub fn from_alias(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL.into_iter().find(|tag| {
            tag.info()
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(value))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn table_contains_each_tag_once_in_all_order() {
        assert_eq!(BACKENDS.map(|row| row.tag), BackendTag::ALL);
        let tags: HashSet<_> = BACKENDS.iter().map(|row| row.tag).collect();
        assert_eq!(tags.len(), BackendTag::ALL.len());
        for tag in BackendTag::ALL {
            assert_eq!(tag.info().tag, tag);
        }
    }

    #[test]
    fn prefixes_and_aliases_are_unique_and_aliases_round_trip() {
        let mut key_prefixes = HashSet::new();
        let mut wire_prefixes = HashSet::new();
        let mut aliases = HashSet::new();
        for row in &BACKENDS {
            assert!(key_prefixes.insert(row.key_prefix));
            assert!(wire_prefixes.insert(row.wire_prefix));
            for alias in row.aliases {
                assert!(aliases.insert(alias.to_ascii_lowercase()));
                assert_eq!(BackendTag::from_alias(alias), Some(row.tag));
            }
        }
    }

    #[test]
    fn default_models_preserve_each_backends_values() {
        use BackendTag::*;
        let expected = [
            (
                OpenRouter,
                [
                    "openai/gpt-6-luna",
                    "openai/gpt-6-sol",
                    "google/gemini-embedding-2",
                    "openai/gpt-image-2",
                    "google/veo-3.1",
                ],
            ),
            (
                OpenAi,
                [
                    "gpt-6-luna",
                    "gpt-6-sol",
                    "text-embedding-3-small",
                    "dall-e-3",
                    "",
                ],
            ),
            (OpencodeGo, ["gpt-6-luna", "glm-5.3", "", "", ""]),
            (Codex, ["gpt-6-luna", "gpt-6-sol", "", "", ""]),
            (
                Mistral,
                [
                    "mistral-vibe-cli-fast",
                    "mistral-vibe-cli-latest",
                    "",
                    "",
                    "",
                ],
            ),
            (
                Local,
                ["", "gpt-6-sol", "text-embedding-3-small", "dall-e-3", ""],
            ),
        ];
        for (tag, models) in expected {
            let row = tag.info();
            assert_eq!(
                [
                    row.default_helper_model,
                    row.default_research_model,
                    row.default_embedding_model,
                    row.default_image_gen_model,
                    row.default_video_gen_model
                ],
                models,
                "{tag:?}"
            );
        }
    }
}
