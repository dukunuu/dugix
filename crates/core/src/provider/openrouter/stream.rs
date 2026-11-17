//! Chat-completions SSE stream parsing: frames, deltas, tool calls, and usage.

use std::collections::BTreeMap;

use crate::provider::{PromptConvention, ToolCall, Usage};

/// End of the first complete SSE block in `buffer`, as `(offset, separator
/// length)`. Both `\n\n` and `\r\n\r\n` terminate a block; whichever starts
/// first wins.
pub(super) fn sse_block_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = find_bytes(buffer, b"\n\n").map(|index| (index, 2));
    let crlf = find_bytes(buffer, b"\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 < right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(super) fn chat_sse_frame(value: &serde_json::Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

pub(super) fn parse_delta(data: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return (None, None);
    };
    let Some(delta) = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
    else {
        return (None, None);
    };
    let field = |name: &str| delta.get(name).and_then(|x| x.as_str()).map(str::to_string);
    // Mistral's reasoning models stream content as typed chunks: `text`
    // parts are the answer, `thinking` parts carry the reasoning.
    let (content, thinking) = match delta.get("content") {
        Some(serde_json::Value::Array(parts)) => split_content_chunks(parts),
        _ => (field("content"), None),
    };
    (
        content,
        field("reasoning_content")
            .or_else(|| field("reasoning"))
            .or(thinking),
    )
}

/// Split typed content chunks into (answer text, thinking text); either is
/// `None` when the chunk list carries none of it.
fn split_content_chunks(parts: &[serde_json::Value]) -> (Option<String>, Option<String>) {
    let text_of = |part: &serde_json::Value| {
        part.get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let mut content = String::new();
    let mut thinking = String::new();
    for part in parts {
        match part.get("type").and_then(|t| t.as_str()) {
            Some("thinking") => match part.get("thinking") {
                Some(serde_json::Value::Array(inner)) => {
                    for p in inner {
                        thinking.push_str(&text_of(p));
                    }
                }
                Some(serde_json::Value::String(text)) => thinking.push_str(text),
                _ => {}
            },
            Some("text") | None => content.push_str(&text_of(part)),
            Some(_) => {}
        }
    }
    let some = |s: String| (!s.is_empty()).then_some(s);
    (some(content), some(thinking))
}

/// Merge one SSE chunk's `delta.tool_calls` fragments into the running
/// per-call accumulator, keyed by the call's `index` (ids/names arrive once,
/// `arguments` streams in pieces that must be concatenated in order).
pub(super) fn accumulate_tool_calls(acc: &mut BTreeMap<usize, ToolCall>, data: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let Some(calls) = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("tool_calls"))
        .and_then(|t| t.as_array())
    else {
        return;
    };
    for call in calls {
        let idx = usize::try_from(
            call.get("index")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        )
        .unwrap_or(0);
        let entry = acc.entry(idx).or_default();
        if let Some(id) = call.get("id").and_then(|i| i.as_str())
            && !id.is_empty()
        {
            entry.id = id.to_string();
        }
        if let Some(func) = call.get("function") {
            if let Some(name) = func.get("name").and_then(|n| n.as_str())
                && !name.is_empty()
            {
                entry.name = name.to_string();
            }
            if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                entry.arguments.push_str(args);
            }
        }
    }
}

/// Pull the `usage` object from an SSE chunk, if present; `None` for absent,
/// `null` or all-zero usage (providers echo nulls until the final chunk).
pub(super) fn parse_usage(data: &str) -> Option<Usage> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    v.get("usage").and_then(parse_usage_value)
}

/// Parse a provider usage object into a [`Usage`], normalizing every backend to
/// one invariant: `prompt_tokens` is the total prompt, cache buckets are subsets.
pub(crate) fn parse_usage_value(value: &serde_json::Value) -> Option<Usage> {
    let u = value.as_object()?;
    let get = |k: &str| u.get(k).and_then(json_u64).unwrap_or(0);
    let has = |k: &str| u.contains_key(k);
    let detail = |group: &str, field: &str| {
        u.get(group)
            .and_then(|d| d.get(field))
            .and_then(json_u64)
            .unwrap_or(0)
    };
    let detail_present = |group: &str, field: &str| {
        u.get(group)
            .is_some_and(|d| d.get(field).is_some_and(|v| json_u64(v).is_some()))
    };

    // Cache fields whose provider reports a prompt total that already covers
    // them (OpenAI, Codex Responses, DeepSeek-compatible). Take the largest so
    // duplicated compatibility spellings count once, not twice.
    let inclusive_read = [
        detail("prompt_tokens_details", "cached_tokens"),
        detail("input_tokens_details", "cached_tokens"),
        get("prompt_cache_hit_tokens"),
        get("prompt_cache_read_tokens"),
        get("cache_read_tokens"),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let inclusive_write = [
        detail("prompt_tokens_details", "cache_write_tokens"),
        detail("input_tokens_details", "cache_write_tokens"),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let inclusive_present = detail_present("prompt_tokens_details", "cached_tokens")
        || detail_present("input_tokens_details", "cached_tokens")
        || detail_present("prompt_tokens_details", "cache_write_tokens")
        || detail_present("input_tokens_details", "cache_write_tokens")
        || has("prompt_cache_hit_tokens")
        || has("prompt_cache_read_tokens")
        || has("cache_read_tokens");

    // Anthropic-shaped accounting: `input_tokens` counts only the fresh
    // prompt, with reads and writes reported beside it.
    let exclusive_read = get("cache_read_input_tokens");
    let exclusive_write = get("cache_creation_input_tokens");
    let exclusive_present = has("cache_read_input_tokens") || has("cache_creation_input_tokens");

    let names_prompt_tokens = has("prompt_tokens");
    let reported_prompt = if names_prompt_tokens {
        get("prompt_tokens")
    } else {
        get("input_tokens")
    };
    let completion = if has("completion_tokens") {
        get("completion_tokens")
    } else {
        get("output_tokens")
    };

    let (prompt_tokens, cache_read, cache_write, convention) =
        if exclusive_present && !names_prompt_tokens {
            // The unambiguous Anthropic shape: no `prompt_tokens` key at all,
            // and the exclusive cache fields present. The total is the sum.
            (
                reported_prompt + exclusive_read + exclusive_write,
                exclusive_read,
                exclusive_write,
                PromptConvention::NormalizedFromExclusive,
            )
        } else {
            let read = inclusive_read.max(exclusive_read);
            let write = inclusive_write.max(exclusive_write);
            if read + write > reported_prompt {
                // Backstop for an unrecognized shape: buckets larger than the
                // total prove the count excluded them.
                (
                    reported_prompt + read + write,
                    read,
                    write,
                    PromptConvention::NormalizedFromExclusive,
                )
            } else if inclusive_present || exclusive_present {
                (reported_prompt, read, write, PromptConvention::Inclusive)
            } else {
                // No cache accounting at all. Zero reads is not the same claim
                // as "this provider reported zero reads", so no ratio is
                // offered rather than reporting a confident 0%.
                (reported_prompt, 0, 0, PromptConvention::Unknown)
            }
        };

    let usage = Usage {
        prompt_tokens,
        completion_tokens: completion,
        total_tokens: get("total_tokens").max(prompt_tokens + completion),
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
        prompt_convention: convention,
        cost: u.get("cost").and_then(json_f64),
    };
    (usage.prompt_tokens > 0 || usage.completion_tokens > 0 || usage.cost.is_some())
        .then_some(usage)
}

/// Merge a usage-only event with earlier accounting from the same request.
/// `OpenCode` may send cost-only or partial/cumulative objects after token
/// usage; counts must never move backwards within one API request.
pub(crate) fn merge_usage(slot: &mut Option<Usage>, next: Usage) {
    *slot = Some(match slot.take() {
        // A cost-only trailing object annotates the request it follows.
        Some(previous) if next.prompt_tokens + next.completion_tokens == 0 => Usage {
            cost: next.cost.or(previous.cost),
            ..previous
        },
        // Otherwise take whichever object accounts for more of the request,
        // whole; field-by-field merging could pair a cache count with a prompt
        // total from another frame and produce a ratio no response reported.
        Some(previous) => {
            let keep_next = next.prompt_tokens + next.completion_tokens
                >= previous.prompt_tokens + previous.completion_tokens;
            let winner = if keep_next { next } else { previous };
            Usage {
                cost: next.cost.or(previous.cost),
                ..winner
            }
        }
        None => next,
    });
}

/// Parse an unsigned usage count from either JSON's numeric representation or
/// a provider's stringly-typed equivalent.
pub(super) fn json_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse::<u64>().ok())
}

/// Parse either JSON's numeric representation or a provider's stringly-typed
/// equivalent, rejecting non-finite values before they reach `SQLite`.
fn json_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
        .filter(|n| n.is_finite())
}

/// The trailing chunk of an `OpenCode` Zen/Go stream carries the request's
/// billed cost as a string with no usage object; other providers omit it.
pub(super) fn parse_stream_cost(data: &str) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    v.get("cost").and_then(json_f64)
}

pub(super) fn wire_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// Extract an SSE event block's `data:` payload, joining multiple `data:` lines
/// with `\n` per the spec and ignoring `event:`/`id:`/comment lines.
pub(super) fn sse_event_data(event_block: &str) -> String {
    event_block
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n")
}
