//! Chapter and doc hints attached to a message before it is sent.
//!
//! A plain keyword match, no model call: the message's distinct terms are
//! scored against each unpinned chapter (title, description, facts) and each
//! doc (title, headings). A term in a title counts double. The best few
//! targets are named — not pasted — so the model opens them only when they
//! matter. Pinned chapters are already in the prompt and never hinted.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::book::DocEntry;
use super::{Chapter, Fact};

/// Most targets hinted per message.
pub const MAX_HINTS: usize = 3;
/// Messages shorter than this carry no hint.
const MIN_QUERY_CHARS: usize = 15;
/// Weighted score a target needs to be hinted.
const MIN_SCORE: usize = 2;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "you", "your", "with", "this", "that", "what", "how", "can", "are", "was",
    "were", "have", "has", "had", "not", "but", "all", "any", "about", "into", "from", "they",
    "them", "then", "than", "there", "their", "here", "when", "where", "which", "who", "why",
    "will", "would", "should", "could", "does", "did", "doing", "just", "also", "some", "more",
    "most", "very", "much", "make", "made", "like", "want", "need", "use", "using", "get", "got",
    "one", "two", "its", "it's", "i'm", "let", "lets", "let's", "please", "thanks", "okay", "yes",
    "now", "new", "out", "our", "over", "only", "other", "same", "such", "too", "way", "well",
    "my", "me", "is", "it", "to", "of", "in", "on", "a", "an", "do", "be", "so", "if", "or",
];

/// Lowercase alphanumeric words of three or more characters, minus common
/// words, each once.
#[must_use]
pub fn terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-' || c == '_'))
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| word.chars().count() >= 3 && !STOPWORDS.contains(&word.as_str()))
        .filter(|word| seen.insert(word.clone()))
        .collect()
}

/// Something worth opening.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// `chapter:<title>` or `doc:<relative>`: what "already hinted" compares.
    pub key: String,
    /// What the user sees: the chapter or doc title.
    pub label: String,
    /// How the model opens it.
    pub open: String,
}

/// The persisted `recall` row: the text replayed to the model, verbatim on
/// every later request, plus the targets for display and de-duplication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recall {
    pub text: String,
    pub targets: Vec<Target>,
}

impl Recall {
    /// Encode for the message row.
    #[must_use]
    pub fn to_row(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Decode a message row; `None` for anything malformed.
    #[must_use]
    pub fn from_row(content: &str) -> Option<Self> {
        serde_json::from_str(content).ok()
    }
}

fn score(query: &[String], title: &str, body: &[&str]) -> usize {
    let title = title.to_lowercase();
    let body: Vec<String> = body.iter().map(|text| text.to_lowercase()).collect();
    query
        .iter()
        .map(|term| {
            if title.contains(term.as_str()) {
                2
            } else {
                usize::from(body.iter().any(|text| text.contains(term.as_str())))
            }
        })
        .sum()
}

/// The hint for one message, or `None` when nothing matches well enough.
/// `already` holds keys hinted earlier in the session.
#[must_use]
#[allow(clippy::implicit_hasher)] // Callers build a plain `HashSet`.
pub fn hint(
    message: &str,
    chapters: &[Chapter],
    facts: &[Fact],
    docs: &[DocEntry],
    already: &HashSet<String>,
) -> Option<Recall> {
    let message = message.trim();
    if message.chars().count() < MIN_QUERY_CHARS || message.starts_with(['/', '!']) {
        return None;
    }
    let query = terms(message);
    if query.is_empty() {
        return None;
    }
    let mut scored: Vec<(usize, Target)> = Vec::new();
    for chapter in chapters.iter().filter(|c| !c.pinned) {
        let mut body: Vec<&str> = facts
            .iter()
            .filter(|f| f.chapter_id == chapter.id)
            .map(|f| f.text.as_str())
            .collect();
        body.push(&chapter.description);
        scored.push((
            score(&query, &chapter.title, &body),
            Target {
                key: format!("chapter:{}", chapter.title.to_lowercase()),
                label: chapter.title.clone(),
                open: format!("memory action=read chapter=\"{}\"", chapter.title),
            },
        ));
    }
    for doc in docs {
        let body: Vec<&str> = doc.headings.iter().map(String::as_str).collect();
        scored.push((
            score(&query, &doc.title, &body),
            Target {
                key: format!("doc:{}", doc.relative),
                label: doc.title.clone(),
                open: format!("canvas action=read path={}", doc.relative),
            },
        ));
    }
    scored.retain(|(score, target)| *score >= MIN_SCORE && !already.contains(&target.key));
    // Stable: equal scores keep chapter-then-doc, creation order.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    let targets: Vec<Target> = scored
        .into_iter()
        .take(MAX_HINTS)
        .map(|(_, target)| target)
        .collect();
    if targets.is_empty() {
        return None;
    }
    let list = targets
        .iter()
        .map(|t| format!("\"{}\" ({})", t.label, t.open))
        .collect::<Vec<_>>()
        .join("; ");
    Some(Recall {
        text: format!("[memory: possibly relevant — {list}. Open one only if it helps.]"),
        targets,
    })
}
