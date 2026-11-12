//! Provider prompt-cache lifetimes: which request fields lengthen a cache
//! entry's life, how long a lane survives, and what to do when a provider
//! refuses a field.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use super::{BackendTag, ChatParams, model_needs_cache_breakpoints};

/// Extended retention for models before GPT-5.6; free.
pub(crate) const RETENTION: &str = "prompt_cache_retention";
/// Paid twelve-hour lifetime for GPT-5.6 and later on the Responses API.
pub(crate) const OPTIONS: &str = "prompt_cache_options";

const MINUTES_5: u64 = 300;
const MINUTES_30: u64 = 1_800;
const HOURS_1: u64 = 3_600;
const HOURS_12: u64 = 43_200;
const HOURS_24: u64 = 86_400;

/// Fields a provider refused, per backend and model, for the rest of the process.
static REFUSED: LazyLock<Mutex<HashSet<Refusal>>> = LazyLock::new(Mutex::default);

type Refusal = (BackendTag, String, &'static str);

/// A request failed because the provider rejected one lifetime field.
#[derive(Debug)]
pub(crate) struct FieldRejected(pub &'static str);

impl std::fmt::Display for FieldRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "provider rejected {}", self.0)
    }
}

impl std::error::Error for FieldRejected {}

/// The `(major, minor)` of a `gpt-N[.M]…` model id, ignoring any vendor or
/// backend prefix. Other families have no version here.
#[must_use]
pub(crate) fn gpt_version(model: &str) -> Option<(u32, u32)> {
    let name = model.rsplit(['/', ':']).next()?.to_ascii_lowercase();
    let rest = name.strip_prefix("gpt-")?;
    let digits = |text: &str| -> (Option<u32>, usize) {
        let len = text.chars().take_while(char::is_ascii_digit).count();
        (text[..len].parse().ok(), len)
    };
    let (major, len) = digits(rest);
    let major = major?;
    let minor = rest[len..]
        .strip_prefix('.')
        .and_then(|after| digits(after).0)
        .unwrap_or(0);
    Some((major, minor))
}

/// Models whose extended retention is a free `24h` switch: GPT-4.1 and GPT-5.x
/// before GPT-5.6.
fn supports_free_retention(version: (u32, u32)) -> bool {
    version == (4, 1) || (version.0 == 5 && version.1 < 6)
}

fn is_openai_wire(tag: BackendTag) -> bool {
    matches!(tag, BackendTag::OpenAi | BackendTag::Codex)
}

/// Whether the provider already refused `field` for this backend and model.
#[must_use]
pub(crate) fn is_refused(tag: BackendTag, model: &str, field: &'static str) -> bool {
    REFUSED
        .lock()
        .is_ok_and(|set| set.contains(&(tag, model.to_string(), field)))
}

/// Stop sending `field` to this backend and model for the rest of the process.
pub(crate) fn refuse(tag: BackendTag, model: &str, field: &'static str) {
    if let Ok(mut set) = REFUSED.lock() {
        set.insert((tag, model.to_string(), field));
    }
}

/// The lifetime fields to add to a request body for this backend and model.
/// `Codex` talks the Responses API; `OpenAI` is chat completions, which has no
/// paid-lifetime option.
#[must_use]
pub(crate) fn lifetime_fields(
    tag: BackendTag,
    model: &str,
    params: &ChatParams,
) -> Vec<(&'static str, serde_json::Value)> {
    let Some(version) = is_openai_wire(tag).then(|| gpt_version(model)).flatten() else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    if supports_free_retention(version) {
        if params.cache_ttl_1h {
            fields.push((RETENTION, serde_json::json!("24h")));
        }
    } else if version >= (5, 6) && tag == BackendTag::Codex && params.cache_long_openai {
        fields.push((OPTIONS, serde_json::json!({ "ttl": "12h" })));
    }
    fields.retain(|(field, _)| !is_refused(tag, model, field));
    fields
}

/// Add this backend's lifetime fields to a request body.
pub(crate) fn apply_lifetime(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    tag: BackendTag,
    model: &str,
    params: &ChatParams,
) {
    for (field, value) in lifetime_fields(tag, model, params) {
        obj.insert(field.into(), value);
    }
}

/// Which lifetime field a 400 response names, if the request carried it.
#[must_use]
pub(crate) fn rejected_field(sent: &serde_json::Value, response: &str) -> Option<&'static str> {
    [RETENTION, OPTIONS]
        .into_iter()
        .find(|field| sent.get(*field).is_some() && response.contains(field))
}

/// How long an unused cache entry survives on this backend and model.
#[must_use]
pub fn lifetime_secs(tag: BackendTag, model: &str, extended: bool, long_openai: bool) -> u64 {
    match tag {
        BackendTag::OpenAi | BackendTag::Codex => match gpt_version(model) {
            Some(version) if supports_free_retention(version) && extended => HOURS_24,
            Some(version) if version >= (5, 6) && tag == BackendTag::Codex && long_openai => {
                HOURS_12
            }
            _ => MINUTES_30,
        },
        BackendTag::OpenRouter if model_needs_cache_breakpoints(model) && extended => HOURS_1,
        _ => MINUTES_5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(extended: bool, long: bool) -> ChatParams {
        ChatParams {
            cache_ttl_1h: extended,
            cache_long_openai: long,
            ..ChatParams::default()
        }
    }

    fn names(tag: BackendTag, model: &str, params: &ChatParams) -> Vec<&'static str> {
        lifetime_fields(tag, model, params)
            .into_iter()
            .map(|(field, _)| field)
            .collect()
    }

    #[test]
    fn versions_parse_from_model_ids() {
        for (id, expected) in [
            ("gpt-5.5", Some((5, 5))),
            ("openai/gpt-5.6-pro", Some((5, 6))),
            ("codex:gpt-5-codex", Some((5, 0))),
            ("gpt-6.1-sol", Some((6, 1))),
            ("gpt-4.1-mini", Some((4, 1))),
            ("gpt-4o", Some((4, 0))),
            ("claude-sonnet-4", None),
            ("o3", None),
            ("gpt-", None),
        ] {
            assert_eq!(gpt_version(id), expected, "{id}");
        }
    }

    #[test]
    fn older_openai_models_get_free_retention_only_when_extended() {
        for tag in [BackendTag::OpenAi, BackendTag::Codex] {
            assert_eq!(names(tag, "gpt-5.5", &params(true, false)), [RETENTION]);
            assert_eq!(
                names(tag, "gpt-5.1-codex", &params(true, true)),
                [RETENTION]
            );
            assert!(names(tag, "gpt-5.5", &params(false, false)).is_empty());
        }
        let value = &lifetime_fields(BackendTag::OpenAi, "gpt-5.5", &params(true, false))[0].1;
        assert_eq!(value, "24h");
    }

    #[test]
    fn newer_openai_models_get_nothing_unless_the_paid_option_is_on_for_responses() {
        for model in ["gpt-5.6", "gpt-6.1-sol"] {
            for tag in [BackendTag::OpenAi, BackendTag::Codex] {
                assert!(
                    names(tag, model, &params(true, false)).is_empty(),
                    "{model}"
                );
            }
            assert!(names(BackendTag::OpenAi, model, &params(true, true)).is_empty());
            let fields = lifetime_fields(BackendTag::Codex, model, &params(false, true));
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].0, OPTIONS);
            assert_eq!(fields[0].1, serde_json::json!({ "ttl": "12h" }));
        }
    }

    #[test]
    fn unknown_models_and_other_backends_get_nothing() {
        for model in ["o3", "mystery-model", "gpt-4o"] {
            assert!(names(BackendTag::OpenAi, model, &params(true, true)).is_empty());
            assert!(names(BackendTag::Codex, model, &params(true, true)).is_empty());
        }
        for tag in [
            BackendTag::Mistral,
            BackendTag::OpencodeGo,
            BackendTag::Local,
            BackendTag::OpenRouter,
        ] {
            assert!(
                names(tag, "gpt-5.5", &params(true, true)).is_empty(),
                "{tag:?}"
            );
        }
    }

    #[test]
    fn refused_fields_are_dropped_for_that_backend_and_model_only() {
        let model = "gpt-5.4-refusal-test";
        assert_eq!(
            names(BackendTag::OpenAi, model, &params(true, false)),
            [RETENTION]
        );
        refuse(BackendTag::OpenAi, model, RETENTION);
        assert!(names(BackendTag::OpenAi, model, &params(true, false)).is_empty());
        assert_eq!(
            names(BackendTag::Codex, model, &params(true, false)),
            [RETENTION]
        );
        assert_eq!(
            names(BackendTag::OpenAi, "gpt-5.4-other", &params(true, false)),
            [RETENTION]
        );
    }

    #[test]
    fn a_400_must_name_a_field_the_request_carried() {
        let sent = serde_json::json!({ "prompt_cache_retention": "24h" });
        let body = r#"{"error":{"message":"Unknown parameter: 'prompt_cache_retention'."}}"#;
        assert_eq!(rejected_field(&sent, body), Some(RETENTION));
        assert_eq!(rejected_field(&sent, "bad request"), None);
        assert_eq!(rejected_field(&serde_json::json!({}), body), None);
    }

    #[test]
    fn lifetimes_follow_the_provider_and_the_setting() {
        use BackendTag::{Codex, Local, Mistral, OpenAi, OpenRouter};
        for (tag, model, extended, long, secs) in [
            (Mistral, "mistral-large", true, true, 300),
            (OpenAi, "gpt-5.5", false, false, 1_800),
            (OpenAi, "gpt-5.5", true, false, 86_400),
            (Codex, "gpt-5.6", true, false, 1_800),
            (Codex, "gpt-5.6", true, true, 43_200),
            (OpenAi, "gpt-5.6", true, true, 1_800),
            (OpenRouter, "anthropic/claude-sonnet-4", false, false, 300),
            (OpenRouter, "anthropic/claude-sonnet-4", true, false, 3_600),
            (OpenRouter, "openai/gpt-5.5", true, false, 300),
            (Local, "qwen", true, true, 300),
        ] {
            assert_eq!(
                lifetime_secs(tag, model, extended, long),
                secs,
                "{tag:?} {model}"
            );
        }
    }
}
