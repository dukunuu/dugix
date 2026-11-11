//! Repairing panel blocks and image links in a finished reply.

use std::fmt::Write as _;

use crate::db::Message;
use crate::provider::{ChatMessage, ChatParams, Usage};

use super::App;

pub(super) struct UiRepairTarget {
    pub(super) session_id: String,
    pub(super) message_id: String,
    pub(super) original: String,
    pub(super) incognito: bool,
    pub(super) model_id: String,
    pub(super) backend: crate::provider::BackendTag,
    pub(super) space_id: String,
}

pub(in crate::app) struct UiRepairResult {
    target: UiRepairTarget,
    result: std::result::Result<String, String>,
    usage: Option<Usage>,
}

impl App {
    pub(super) fn start_ui_repair(&mut self, target: UiRepairTarget) {
        let blocks = failing_ui_blocks(&target.original);
        if blocks.is_empty() {
            return;
        }
        let Some(provider) = self.backends.get(target.backend).cloned() else {
            self.push_status("UI repair failed — model backend unavailable");
            return;
        };
        let messages = ui_repair_history(&blocks);
        self.ui_repair_messages
            .insert(target.session_id.clone(), target.message_id.clone());
        let tx = self.ui_repair_tx.clone();
        self.push_status("fixing the UI…");
        tokio::spawn(async move {
            let completion = provider
                .complete_with_params(&target.model_id, messages, &ChatParams::default())
                .await;
            let (result, usage) = match completion {
                Ok(completion) => (
                    repaired_ui_reply(&target.original, &blocks, &completion.text),
                    completion.usage,
                ),
                Err(error) => (Err(error.to_string()), None),
            };
            let _ = tx.send(UiRepairResult {
                target,
                result,
                usage,
            });
        });
    }

    pub(in crate::app) fn on_ui_repair_result(
        &mut self,
        repair: UiRepairResult,
    ) -> super::AppEvent {
        let UiRepairResult {
            target,
            result,
            usage,
        } = repair;
        if !target.incognito
            && let Some(usage) = usage
        {
            let _ = self.db.record_usage(
                target.backend.name(),
                &target.model_id,
                usage,
                &crate::db::UsageLog {
                    path: self.space.db_path(),
                    session_id: Some(target.session_id.clone()),
                    space_id: target.space_id.clone(),
                },
                "ui repair",
            );
        }
        if self.ui_repair_messages.get(&target.session_id) != Some(&target.message_id) {
            return super::AppEvent::Status(String::new());
        }
        self.ui_repair_messages.remove(&target.session_id);
        let viewing = self
            .session
            .as_ref()
            .is_some_and(|s| s.id == target.session_id);
        let index = viewing
            .then(|| {
                self.messages.iter().rposition(|message| {
                    message.role == "assistant" && message.content == target.original
                })
            })
            .flatten();
        if (viewing && index.is_none()) || (target.incognito && !viewing) {
            return super::AppEvent::Status(String::new());
        }
        let mut fixed_images = 0;
        let applied = result.and_then(|content| {
            let stored = if target.incognito {
                Vec::new()
            } else {
                self.db
                    .load_messages(&target.session_id)
                    .map_err(|error| error.to_string())?
            };
            let (content, fixed) = repair_image_links(
                &content,
                if target.incognito {
                    &self.messages
                } else {
                    &stored
                },
            );
            fixed_images = fixed;
            if !target.incognito
                && !self
                    .db
                    .update_ui_reply(
                        &target.session_id,
                        &target.message_id,
                        &target.original,
                        &content,
                    )
                    .map_err(|error| error.to_string())?
            {
                return Ok(false);
            }
            if let Some(index) = index {
                // Only the latest reply changed; the prefix before it is the
                // same, so the prompt-cache lane (and its key) stays put.
                self.messages[index].content = content;
                self.push_history_invalidated();
            }
            Ok(true)
        });
        super::AppEvent::Status(match applied {
            Ok(true) if fixed_images > 0 => format!(
                "UI fixed · fixed {fixed_images} image link{}",
                if fixed_images == 1 { "" } else { "s" }
            ),
            Ok(true) => "UI fixed".into(),
            Ok(false) => String::new(),
            Err(error) => format!(
                "UI repair failed — {}",
                error.lines().next().unwrap_or("invalid UI")
            ),
        })
    }
}

pub(super) struct UiBlock {
    range: std::ops::Range<usize>,
    prefix: String,
    source: String,
    title: String,
}

// Only complete UI fences count. Track other fences too, so literal examples
// inside longer Markdown fences never become replay diagnostics or repairs.
fn ui_blocks(content: &str) -> Vec<UiBlock> {
    let mut fence: Option<(char, usize, bool, usize)> = None;
    let mut source = String::new();
    let mut blocks = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    let mut prefix = String::new();
    for raw in content.split_inclusive('\n') {
        let line = raw.trim_end_matches('\n').trim_end_matches('\r');
        if let Some((marker, length, ui, quotes)) = fence {
            let mut body = line;
            for _ in 0..quotes {
                if let Some(rest) = body.trim_start().strip_prefix('>') {
                    body = rest.strip_prefix(' ').unwrap_or(rest);
                }
            }
            let trimmed = body.trim_start();
            let count = trimmed.chars().take_while(|ch| *ch == marker).count();
            if count >= length && trimmed[count..].trim().is_empty() {
                if ui {
                    let title = crate::interactive::Document::preview(&source)
                        .map(|document| document.title().to_string())
                        .or_else(|| {
                            serde_json::from_str::<serde_json::Value>(&source)
                                .ok()
                                .and_then(|value| {
                                    value
                                        .get("title")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_string)
                                })
                        })
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| format!("#{}", blocks.len() + 1));
                    blocks.push(UiBlock {
                        range: start..offset + line.len(),
                        prefix: prefix.clone(),
                        source: source.clone(),
                        title,
                    });
                }
                fence = None;
                source.clear();
            } else if ui {
                // Markdown removes blockquote prefixes before handing off JSON.
                source.push_str(body);
                source.push('\n');
            }
        } else {
            if line.starts_with("    ") || line.starts_with('\t') {
                offset += raw.len();
                continue;
            }
            let trimmed = line.trim_start_matches([' ', '\t', '>']);
            let marker = trimmed.chars().next().unwrap_or_default();
            let length = trimmed.chars().take_while(|ch| *ch == marker).count();
            if matches!(marker, '`' | '~') && length >= 3 {
                let ui = trimmed[length..]
                    .split_whitespace()
                    .next()
                    .is_some_and(|lang| lang.eq_ignore_ascii_case("nexus-ui"));
                let quotes = line[..line.len() - trimmed.len()]
                    .chars()
                    .filter(|ch| *ch == '>')
                    .count();
                fence = Some((marker, length, ui, quotes));
                start = offset;
                prefix = line[..line.len() - trimmed.len()].into();
            }
        }
        offset += raw.len();
    }
    blocks
}

fn image_distance(left: &str, right: &str) -> usize {
    let left = left.split('?').next().unwrap_or(left).as_bytes();
    let right = right.split('?').next().unwrap_or(right).as_bytes();
    if left.len().abs_diff(right.len()) > 4 {
        return 5;
    }
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (diagonal + usize::from(a != b))
                .min(row[j] + 1)
                .min(old + 1);
            diagonal = old;
        }
        if row.iter().copied().min().unwrap_or(5) > 4 {
            return 5;
        }
    }
    row[right.len()]
}

fn result_urls(result: &str) -> Vec<&str> {
    result
        .match_indices("https://")
        .map(|(start, _)| {
            result[start..]
                .split(|ch: char| {
                    ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | '\\' | '`')
                })
                .next()
                .unwrap_or_default()
                .trim_end_matches(['.', ',', ';', ')', ']', '}'])
        })
        .collect()
}

fn markdown_image_ranges(content: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    let mut fence = None;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start_matches([' ', '>']);
        let marker = trimmed.chars().next().unwrap_or_default();
        let count = trimmed.chars().take_while(|ch| *ch == marker).count();
        if matches!(marker, '`' | '~') && count >= 3 {
            if fence.is_none() {
                fence = Some((marker, count));
            } else if fence.is_some_and(|(open, length)| open == marker && count >= length) {
                fence = None;
            }
        } else if fence.is_none() && !line.starts_with("    ") {
            let mut rest = line;
            let mut consumed = 0;
            while let Some(start) = rest.find("![") {
                let Some(close) = rest[start + 2..].find("](") else {
                    break;
                };
                let begin = start + 2 + close + 2;
                // Inline code is literal, not a rendered image.
                let prefix = &line[..consumed + start];
                if prefix.chars().filter(|ch| *ch == '`').count() % 2 == 0
                    && prefix.chars().rev().take_while(|ch| *ch == '\\').count() % 2 == 0
                {
                    let tail = &rest[begin..];
                    let angle = tail.starts_with('<');
                    let mut depth = 0usize;
                    let end = tail.char_indices().find_map(|(index, ch)| {
                        if angle {
                            return (ch == '>').then_some(index);
                        }
                        match ch {
                            '(' => depth += 1,
                            ')' if depth > 0 => depth -= 1,
                            ')' => return Some(index),
                            ch if ch.is_whitespace() => return Some(index),
                            _ => {}
                        }
                        None
                    });
                    if let Some(end) = end {
                        ranges.push(
                            offset + consumed + begin + usize::from(angle)
                                ..offset + consumed + begin + end,
                        );
                    }
                }
                consumed += begin;
                rest = &rest[begin..];
            }
        }
        offset += line.len();
    }
    ranges
}

pub(super) fn repair_image_links(content: &str, messages: &[Message]) -> (String, usize) {
    let mut results = Vec::new();
    for message in messages {
        if message.role == "tool" {
            results.push((message.content.clone(), false));
        } else if message.role == "tool_call"
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content)
            && let Some(result) = value["result"].as_str()
        {
            let images = value["name"] == "image_search"
                || (value["name"] == "search"
                    && value["arguments"]
                        .as_str()
                        .and_then(|args| serde_json::from_str::<serde_json::Value>(args).ok())
                        .is_some_and(|args| args["mode"] == "images"))
                || result.contains("\n    image · ");
            results.push((result.into(), images));
        }
    }
    let mut candidates = std::collections::BTreeSet::new();
    for (result, images) in &results {
        for url in result_urls(result) {
            let lower = url.to_ascii_lowercase();
            if *images
                || [".png", ".jpg", ".jpeg", ".webp", ".gif"]
                    .iter()
                    .any(|ext| lower.contains(ext))
            {
                candidates.insert(url);
            }
        }
    }
    let replacement = |url: &str| -> Option<&str> {
        if results.iter().any(|(result, _)| result.contains(url)) {
            return None;
        }
        let parsed = crate::remote_image::validate_url(url).ok()?;
        let matches: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|candidate| {
                crate::remote_image::validate_url(candidate)
                    .is_ok_and(|candidate| candidate.host_str() == parsed.host_str())
                    && image_distance(url, candidate) <= 4
            })
            .collect();
        (matches.len() == 1).then(|| matches[0])
    };
    let mut edits = Vec::new();
    for range in markdown_image_ranges(content) {
        if let Some(url) = replacement(&content[range.clone()]) {
            edits.push((range, url.to_string()));
        }
    }
    for block in ui_blocks(content) {
        // Read the image fields even when another node needs schema repair.
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&block.source) else {
            continue;
        };
        for node in value["view"].as_array().into_iter().flatten() {
            let mut urls = Vec::new();
            match node["type"].as_str() {
                Some("image") => urls.extend(node["src"].as_str()),
                Some("hero") => urls.extend(node["image"].as_str()),
                Some("cards" | "buttons") => {
                    urls.extend(
                        node["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|item| item["image"].as_str()),
                    );
                }
                _ => {}
            }
            for url in urls {
                if let Some(candidate) = replacement(url) {
                    let encoded = serde_json::to_string(url).expect("image URL string");
                    let fixed = serde_json::to_string(candidate).expect("image URL string");
                    for (index, _) in content[block.range.clone()].match_indices(&encoded) {
                        let prefix =
                            content[block.range.start..block.range.start + index].trim_end();
                        let key = prefix.strip_suffix(':').unwrap_or_default().trim_end();
                        if !key.ends_with("\"image\"") && !key.ends_with("\"src\"") {
                            continue;
                        }
                        edits.push((
                            block.range.start + index..block.range.start + index + encoded.len(),
                            fixed.clone(),
                        ));
                    }
                }
            }
        }
    }
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    edits.dedup_by(|left, right| left.0 == right.0);
    let mut repaired = content.to_string();
    for (range, url) in edits.iter().rev() {
        repaired.replace_range(range.clone(), url);
    }
    (repaired, edits.len())
}

pub(super) fn ui_program_history(
    messages: &[Message],
    first_owner: usize,
) -> (
    std::collections::BTreeMap<usize, String>,
    Vec<serde_json::Value>,
) {
    use crate::interactive::{Document, edit_patch, merge_program, panel_id};
    let mut panels = std::collections::BTreeMap::<String, String>::new();
    let mut replay = std::collections::BTreeMap::new();
    let mut errors = Vec::new();
    for (owner, message) in messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "assistant")
    {
        errors.clear();
        let mut content = message.content.clone();
        let blocks = ui_blocks(&content);
        let mut replacements = Vec::new();
        for (ordinal, block) in blocks.into_iter().enumerate() {
            let id = panel_id(first_owner + owner, ordinal);
            let result = if let Some((target, patch)) = edit_patch(&block.source) {
                let result = panels
                    .get(target)
                    .ok_or_else(|| anyhow::anyhow!("unknown panel {target}"))
                    .and_then(|source| merge_program(source, patch));
                if let Ok(source) = &result {
                    panels.insert(target.into(), source.clone());
                }
                replacements.push((
                    block.range,
                    format!("Patch for nexus-ui panel {target}\n```nexus-ui\n{patch}```"),
                ));
                result.map(|_| ()).map_err(|error| {
                    vec![crate::interactive::CheckError {
                        path: "edit".into(),
                        message: error.to_string(),
                    }]
                })
            } else {
                let result = Document::check(&block.source);
                if result.is_ok() && !block.source.trim_start().starts_with('{') {
                    panels.insert(id.clone(), block.source.clone());
                    replacements.push((
                        block.range,
                        format!("nexus-ui panel {id}\n```nexus-ui\n{}```", block.source),
                    ));
                }
                result.map(|_| ())
            };
            if let Err(failures) = result {
                for error in failures.into_iter().take(10 - errors.len().min(10)) {
                    errors.push(serde_json::json!({
                        "source":"parser", "code": if error.path == "source" { "parse-failed" } else { "type-mismatch" },
                        "statementId":id, "path":error.path, "message":format!("failed to render: {}", error.message.chars().take(220).collect::<String>()),
                        "hint":"Use the terminal signatures and bounded nexus-ui syntax."
                    }));
                }
            }
        }
        for (range, replacement) in replacements.into_iter().rev() {
            content.replace_range(range, &replacement);
        }
        replay.insert(owner, content);
    }
    for (owner, content) in &mut replay {
        for (ordinal, block) in ui_blocks(content).into_iter().enumerate().rev() {
            let id = panel_id(first_owner + owner, ordinal);
            if let Some(program) = panels.get(&id) {
                content.replace_range(block.range, &format!("```nexus-ui\n{program}\n```"));
            }
        }
    }
    (replay, errors)
}

pub(super) fn failing_ui_blocks(
    content: &str,
) -> Vec<(UiBlock, Vec<crate::interactive::CheckError>)> {
    ui_blocks(content)
        .into_iter()
        .filter(|block| crate::interactive::edit_patch(&block.source).is_none())
        .filter_map(|block| {
            crate::interactive::Document::check(&block.source)
                .err()
                .map(|errors| (block, errors))
        })
        .collect()
}

pub(super) fn ui_failure_notes(content: &str) -> String {
    let mut notes = String::new();
    for (block, errors) in failing_ui_blocks(content) {
        let title = block.title;
        let error = &errors[0];
        let message = error.message.lines().collect::<Vec<_>>().join(" ");
        let path = error.path.replace(['\n', '\r'], " ");
        // A program with a valid root shows without its broken pieces.
        let outcome = if crate::interactive::Document::recover(&block.source).is_ok() {
            "rendered with parts left out"
        } else {
            "failed to render"
        };
        let _ = write!(
            notes,
            "\n[nexus: the nexus-ui block {title:?} {outcome} — {path}: {message}. Correct the reported errors and resend it if the user still needs it.]"
        );
    }
    notes
}

fn ui_repair_history(
    blocks: &[(UiBlock, Vec<crate::interactive::CheckError>)],
) -> Vec<ChatMessage> {
    let mut source = String::new();
    let mut request = String::new();
    for (block, errors) in blocks {
        let _ = writeln!(source, "```nexus-ui\n{}```", block.source);
        let diagnostics = errors
            .iter()
            .take(10)
            .map(|error| {
                format!(
                    "{}: {}",
                    error.path.replace(['\n', '\r'], " "),
                    error.message.replace(['\n', '\r'], " ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let _ = writeln!(
            request,
            "Your nexus-ui block {:?} failed validation:\n{diagnostics}\nReply with ONLY the corrected nexus-ui fenced block, nothing else.",
            block.title,
        );
    }
    vec![
        ChatMessage::text(
            "system",
            format!(
                "{}\nIf multiple blocks need repair, return one corrected fenced block per input block, in the same order.",
                crate::interactive::instructions(),
            ),
        ),
        ChatMessage::text("assistant", source),
        ChatMessage::text("user", request),
    ]
}

fn repaired_ui_reply(
    original: &str,
    failures: &[(UiBlock, Vec<crate::interactive::CheckError>)],
    response: &str,
) -> std::result::Result<String, String> {
    let corrected = ui_blocks(response);
    if corrected.len() != failures.len() {
        return Err(format!(
            "expected {} corrected nexus-ui block(s), got {}",
            failures.len(),
            corrected.len()
        ));
    }
    for block in &corrected {
        crate::interactive::Document::check(&block.source)
            .map_err(|errors| format!("{}: {}", errors[0].path, errors[0].message))?;
    }
    let mut content = original.to_string();
    for ((block, _), corrected) in failures.iter().zip(&corrected).rev() {
        let fence = format!("```nexus-ui\n{}\n```", corrected.source.trim_end());
        let replacement = fence
            .lines()
            .map(|line| format!("{}{line}", block.prefix))
            .collect::<Vec<_>>()
            .join("\n");
        content.replace_range(block.range.clone(), &replacement);
    }
    Ok(content)
}

/// Each fenced code block in `md` as `(language, code)`.
pub fn code_blocks(md: &str) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut lang: Option<String> = None;
    let mut buf = String::new();
    for line in md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if inside {
                out.push((lang.take(), std::mem::take(&mut buf)));
            } else {
                let l = trimmed.trim_start_matches('`').trim();
                lang = (!l.is_empty()).then(|| l.to_string());
            }
            inside = !inside;
            continue;
        }
        if inside {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if inside && !buf.is_empty() {
        out.push((lang.take(), buf)); // unterminated (e.g. mid-stream)
    }
    out
}
