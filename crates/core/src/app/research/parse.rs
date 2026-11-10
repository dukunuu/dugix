//! Parsers for each research stage's reply.

use super::{MAX_SUBQUESTIONS, MAX_SURVEY_QUESTIONS, PlanQuestion};

/// The scoping agent's reply: the single-word COMPLETE marker, a numbered
/// list of clarifying questions, or output that violates the contract
/// (`Malformed` — empty, explanatory prose, error text) which fails the
/// survey visibly instead of silently dropping it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurveyReply {
    Complete,
    Questions(Vec<String>),
    Malformed,
}

/// The approval agent's verdict on a user reply to the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    Approved,
    Revised(Vec<PlanQuestion>),
    /// The agent produced output that is neither an approval nor a usable
    /// revision (bare prose, empty reply). Fails the phase visibly —
    /// malformed output must never be mistaken for approval.
    Malformed,
}

/// The Critic stage's structured decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Critique {
    Satisfied,
    Gaps(Vec<String>),
    Contradiction(String),
}

/// Parse the Planner's raw reply into sub-questions: a JSON string array, or
/// (if the model didn't follow instructions) a best-effort line-by-line
/// fallback stripping bullet/number prefixes. Always capped at
/// `MAX_SUBQUESTIONS`.
pub fn parse_subquestions(text: &str) -> Vec<String> {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Vec<String>>(trimmed) {
        return v
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .take(MAX_SUBQUESTIONS)
            .collect();
    }
    trimmed
        .lines()
        .map(strip_list_prefix)
        .filter(|l| !l.is_empty())
        .take(MAX_SUBQUESTIONS)
        .collect()
}

/// Strip a leading `-`, `*`, or `N.`/`N)` list-item marker, if present.
fn strip_list_prefix(line: &str) -> String {
    let s = line.trim().trim_start_matches(['-', '*']).trim();
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits_end > 0
        && let Some(rest) = s[digits_end..].strip_prefix(['.', ')'])
    {
        return rest.trim().to_string();
    }
    s.to_string()
}

/// Parse the scoping agent's reply: `COMPLETE` (case-insensitive, optionally
/// with trailing punctuation or prose) ends the survey. Otherwise only lines
/// that look like questions — numbered/bulleted, or ending in `?` — are read
/// as clarifying questions. Anything else (empty output, explanatory or
/// error prose) is `Malformed`: the agent's output contract says COMPLETE or
/// questions, so a violation must fail the survey visibly — never be
/// mistaken for completion, and never park the pipeline awaiting an answer
/// for a non-question.
pub fn parse_survey_reply(text: &str) -> SurveyReply {
    let t = text.trim();
    // First word COMPLETE ends the survey, tolerating trailing punctuation
    // and prose: "COMPLETE", "COMPLETE.", "COMPLETE: proceed", "COMPLETE —".
    let head = t
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    if head.eq_ignore_ascii_case("COMPLETE") {
        return SurveyReply::Complete;
    }
    let mut qs: Vec<String> = Vec::new();
    for line in t.lines() {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        // Only list-marked or question-shaped lines count as questions;
        // bare prose (explanations, error messages) is a contract violation.
        let marked =
            s.starts_with(['-', '*']) || s.chars().next().is_some_and(|c| c.is_ascii_digit());
        let q = strip_list_prefix(s);
        if q.is_empty() {
            continue;
        }
        if !marked && !q.ends_with('?') {
            continue;
        }
        qs.push(q);
        if qs.len() >= MAX_SURVEY_QUESTIONS {
            break;
        }
    }
    if qs.is_empty() {
        SurveyReply::Malformed
    } else {
        SurveyReply::Questions(qs)
    }
}

/// Byte offset of the first `[` or `{` in `s`, if any. Structured JSON the
/// model wrapped in prose ("Here is the plan:\n[{\"question\":…}]") is
/// still JSON and must be parsed as such — never re-read as bare lines.
fn json_start(s: &str) -> Option<usize> {
    s.find(['[', '{'])
}

/// Parse the Planner's reply into plan blocks: a JSON array of objects with
/// `question`/`why`/`angles`/`sources` (missing fields default to empty).
/// Structured JSON is unambiguous — malformed JSON (parse failure, wrong
/// field types) or JSON with no usable questions yields an empty result and
/// fails planning; the raw JSON lines are never reinterpreted as bare
/// questions (`[{}]` must not become a plan whose question is literally
/// `[{}]`, and prose-prefixed JSON like "Here is the plan:\n[…]" is still
/// parsed as JSON, not as two raw lines). Only output with no JSON shape at
/// all falls back to one bare question per line (the legacy line format),
/// which also still accepts a legacy JSON array of strings. Always capped at
/// `MAX_SUBQUESTIONS`.
pub fn parse_plan_blocks(text: &str) -> Vec<PlanQuestion> {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Some(start) = json_start(trimmed) {
        let candidate = &trimmed[start..];
        if let Ok(v) = serde_json::from_str::<Vec<PlanQuestion>>(candidate) {
            let qs: Vec<PlanQuestion> = v
                .into_iter()
                .map(|mut q| {
                    q.question = q.question.trim().to_string();
                    q
                })
                .filter(|q| !q.question.is_empty())
                .take(MAX_SUBQUESTIONS)
                .collect();
            if !qs.is_empty() {
                return qs;
            }
        }
        // Legacy structured format: a JSON array of plain strings.
        if let Ok(v) = serde_json::from_str::<Vec<String>>(candidate) {
            let qs: Vec<PlanQuestion> = v
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PlanQuestion::bare)
                .take(MAX_SUBQUESTIONS)
                .collect();
            if !qs.is_empty() {
                return qs;
            }
        }
        // Malformed or unusable structured output: fail, no line fallback.
        return Vec::new();
    }
    parse_subquestions(text)
        .into_iter()
        .map(PlanQuestion::bare)
        .collect()
}

/// Parse the approval agent's reply: `APPROVED` (case-insensitive, optionally
/// with trailing prose) accepts the plan; a JSON plan array is a revision;
/// line-formatted revisions are only accepted when they look like a list
/// (bullets or `N.`/`N)` prefixes). Anything else is `Malformed` — a garbled
/// verdict must fail the phase visibly, never silently count as approval.
pub fn parse_approval(text: &str) -> Approval {
    let upper = text.trim().to_ascii_uppercase();
    if upper == "APPROVED"
        || upper.starts_with("APPROVED:")
        || upper.starts_with("APPROVED —")
        || upper.starts_with("APPROVED\n")
    {
        return Approval::Approved;
    }
    // Structured JSON (possibly wrapped in prose) is unambiguous: parse it
    // strictly through `parse_plan_blocks`, and treat malformed or unusable
    // output as `Malformed` — never fall back to reading the raw JSON lines
    // as bare plan questions.
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if json_start(trimmed).is_some() {
        let qs = parse_plan_blocks(text);
        return if qs.is_empty() {
            Approval::Malformed
        } else {
            Approval::Revised(qs)
        };
    }
    // Line fallback: only when the output is recognizably a list.
    let has_markers = trimmed.lines().any(|l| {
        let s = l.trim();
        s.starts_with(['-', '*']) || s.chars().next().is_some_and(|c| c.is_ascii_digit())
    });
    if !has_markers {
        return Approval::Malformed;
    }
    let qs = parse_plan_blocks(text);
    if qs.is_empty() {
        Approval::Malformed
    } else {
        Approval::Revised(qs)
    }
}

/// Parse the Critic's raw reply into a `Critique`. Anything that doesn't
/// match one of the three expected shapes is treated as `Satisfied` — an
/// unparseable critique shouldn't loop the pipeline forever on garbage.
pub fn parse_critique(text: &str) -> Critique {
    let t = text.trim();
    if t.eq_ignore_ascii_case("SATISFIED") {
        return Critique::Satisfied;
    }
    if let Some(rest) = t.strip_prefix("CONTRADICTION:") {
        let desc = rest.trim();
        if !desc.is_empty() {
            return Critique::Contradiction(desc.to_string());
        }
    }
    if let Some(rest) = t.strip_prefix("GAPS:") {
        let gaps: Vec<String> = rest
            .lines()
            .map(str::trim)
            .filter_map(|l| l.strip_prefix('-'))
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .take(MAX_SUBQUESTIONS)
            .collect();
        if !gaps.is_empty() {
            return Critique::Gaps(gaps);
        }
    }
    Critique::Satisfied
}
