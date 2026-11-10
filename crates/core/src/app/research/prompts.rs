//! Stage prompts and the message lists sent to each research stage.

use std::fmt::Write as _;

use crate::prompts;
pub(super) use crate::prompts::{RESEARCH_TOPIC_FROM_CHAT, render};
use crate::provider::ChatMessage;

use super::{PlanQuestion, plan_text};

#[cfg(test)]
pub(super) const PLANNER_PROMPT: &str = prompts::RESEARCH_PLANNER.trim_ascii_end();

pub const SEARCHER_PROMPT: &str = prompts::RESEARCH_SEARCHER.trim_ascii_end();

/// Fixed protocol text placed before every research-stage instruction. Keeping
/// this prefix byte-identical gives independent stages and agent branches a
/// common provider-cache anchor; stage-specific instructions follow it.
#[cfg(test)]
pub(super) const RESEARCH_CACHE_PREFIX: &str = prompts::RESEARCH_CACHE_PREFIX.trim_ascii_end();

pub(super) fn research_stage_prompt(stage: &str) -> String {
    let prefix = prompts::render(prompts::RESEARCH_CACHE_PREFIX, &[]);
    format!("{prefix}\n\n{stage}")
}

#[cfg(test)]
pub(super) const VERIFIER_PROMPT: &str = prompts::RESEARCH_VERIFIER.trim_ascii_end();

#[cfg(test)]
pub(super) const WRITER_PROMPT: &str = prompts::RESEARCH_WRITER.trim_ascii_end();

/// The Planner's request: the topic, the user's survey answers ("what they
/// said they want"), and any locally-known context (chunks from the space's
/// own files plus a preliminary web survey, semantically matched to the
/// topic) framed as "already known — plan sub-questions for the gaps".
pub(super) fn planner_messages_with_context(
    topic: &str,
    answers: &[(String, String)],
    known: &[String],
) -> Vec<ChatMessage> {
    let mut user = String::new();
    if !answers.is_empty() {
        user.push_str("The user answered clarifying questions before planning:\n");
        for (i, (qs, reply)) in answers.iter().enumerate() {
            let _ = write!(user, "Round {} — asked: {}\nAnswered: {reply}\n", i + 1, qs);
        }
        user.push('\n');
    }
    if known.is_empty() {
        user.push_str(topic);
    } else {
        let _ = write!(
            user,
            "Topic: {topic}\n\nAlready known (from local files and/or a preliminary web survey) — \
             plan sub-questions for the gaps, not what's already covered:\n{}",
            known.join("\n\n")
        );
    }
    vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(prompts::RESEARCH_PLANNER, &[])),
        ),
        ChatMessage::text("user", user),
    ]
}

/// The scoping agent's request: the topic, and on later rounds the questions
/// asked + answers given so far. One prompt serves both the initial questions
/// (empty rounds) and each follow-up round — the agent replies COMPLETE when
/// it has enough.
pub(super) fn survey_messages(topic: &str, rounds: &[(String, String)]) -> Vec<ChatMessage> {
    let mut user = format!("Research topic: {topic}\n");
    if !rounds.is_empty() {
        user.push_str("\nSo far:\n");
        for (i, (qs, reply)) in rounds.iter().enumerate() {
            let _ = write!(
                user,
                "Round {} — I asked:\n{qs}\nThe user answered: {reply}\n",
                i + 1
            );
        }
    }
    vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(prompts::RESEARCH_SURVEY, &[])),
        ),
        ChatMessage::text("user", user),
    ]
}

/// Fold the user's reply to the presented plan back into the pipeline: the
/// approval agent either recognizes an approval (APPROVED) or returns a
/// revised plan.
pub(super) fn plan_approval_messages(
    topic: &str,
    questions: &[PlanQuestion],
    user_reply: &str,
) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(prompts::RESEARCH_PLAN_APPROVAL, &[])),
        ),
        ChatMessage::text(
            "user",
            format!(
                "Topic: {topic}\n\nPlan:\n{}\n\nUser reply: {user_reply}",
                plan_text(questions)
            ),
        ),
    ]
}

pub(super) fn synthesizer_messages(
    topic: &str,
    findings: &[String],
    pinned: &[String],
) -> Vec<ChatMessage> {
    let body = findings
        .iter()
        .enumerate()
        .map(|(i, f)| format!("--- Searcher {} findings ---\n{f}", i + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut user = format!("Topic: {topic}\n\n");
    if !pinned.is_empty() {
        let _ = write!(
            user,
            "Prioritize these pinned sources in the synthesis if their content is present in the findings below:\n{}\n\n",
            pinned.join("\n")
        );
    }
    user.push_str(&body);
    vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(prompts::RESEARCH_SYNTHESIZER, &[])),
        ),
        ChatMessage::text("user", user),
    ]
}

pub(super) fn critic_messages(topic: &str, draft: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(prompts::RESEARCH_CRITIC, &[])),
        ),
        ChatMessage::text("user", format!("Topic: {topic}\n\nDraft:\n{draft}")),
    ]
}

pub(super) fn resolver_messages(
    topic: &str,
    draft: &str,
    findings: &[String],
    contradiction: &str,
) -> Vec<ChatMessage> {
    let body = findings.join("\n\n");
    vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(prompts::RESEARCH_RESOLVER, &[])),
        ),
        ChatMessage::text(
            "user",
            format!(
                "Topic: {topic}\n\nContradiction: {contradiction}\n\nDraft:\n{draft}\n\nSource findings:\n{body}"
            ),
        ),
    ]
}

pub(super) fn verifier_messages(topic: &str, draft: &str, findings: &[String]) -> Vec<ChatMessage> {
    let body = findings.join("\n\n");
    vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(prompts::RESEARCH_VERIFIER, &[])),
        ),
        ChatMessage::text(
            "user",
            format!("Topic: {topic}\n\nSource findings:\n{body}\n\nDraft:\n{draft}"),
        ),
    ]
}

pub(super) fn writer_messages(
    topic: &str,
    verified_draft: &str,
    pinned: &[String],
) -> Vec<ChatMessage> {
    let mut user = format!("Topic: {topic}\n\n");
    if !pinned.is_empty() {
        let _ = write!(
            user,
            "Prioritize these pinned sources in the final report if their content is present in the verified draft below:\n{}\n\n",
            pinned.join("\n")
        );
    }
    let _ = write!(user, "Verified draft:\n{verified_draft}");
    vec![
        ChatMessage::text(
            "system",
            research_stage_prompt(&prompts::render(prompts::RESEARCH_WRITER, &[])),
        ),
        ChatMessage::text("user", user),
    ]
}
