use super::{MAX_SUBQUESTIONS, MAX_SURVEY_QUESTIONS, PlanQuestion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurveyReply {
    Complete,
    Questions(Vec<String>),
    Malformed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    Approved,
    Revised(Vec<PlanQuestion>),
    Malformed,
}

/// The critic's verdict. Gaps and a contradiction can come together; neither
/// is "satisfied". `unreadable` is a reply in none of the agreed forms, which
/// must not pass as satisfied: the gap round would be skipped unseen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Critique {
    pub gaps: Vec<String>,
    pub contradiction: Option<String>,
    pub unreadable: bool,
}

impl Critique {
    #[must_use]
    pub fn satisfied() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.gaps.is_empty() && self.contradiction.is_none() && !self.unreadable
    }
}

/// A verdict line without the markdown a model wraps it in: `**GAPS:**`, `## Satisfied`, `> APPROVED.`
fn bare_line(line: &str) -> &str {
    line.trim()
        .trim_start_matches(['#', '*', '_', '>', '`'])
        .trim_end_matches(['*', '_', '`'])
        .trim()
}

/// The text after `word` when `line` starts with it as a whole word, any case.
fn after_word<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let head = line.get(..word.len())?;
    if !head.eq_ignore_ascii_case(word) {
        return None;
    }
    let rest = &line[word.len()..];
    if rest.starts_with(|c: char| c.is_alphanumeric()) {
        return None;
    }
    Some(rest.trim_start_matches(['*', '_']).trim())
}

/// A whole reply that is just `word`, give or take markdown and closing punctuation.
fn is_only(text: &str, word: &str) -> bool {
    let t = bare_line(text)
        .trim_end_matches(['.', '!', '*', '_'])
        .trim();
    t.eq_ignore_ascii_case(word)
}

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

// The scoping protocol requires COMPLETE or questions; arbitrary prose is not completion.
pub fn parse_survey_reply(text: &str) -> SurveyReply {
    let t = text.trim();
    let head = bare_line(t)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if head.eq_ignore_ascii_case("COMPLETE") {
        return SurveyReply::Complete;
    }
    let mut qs: Vec<String> = Vec::new();
    for line in t.lines() {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
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

fn json_start(s: &str) -> Option<usize> {
    s.find(['[', '{'])
}

// Malformed JSON must fail planning, not become literal questions through line fallback.
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
        return Vec::new();
    }
    parse_subquestions(text)
        .into_iter()
        .map(PlanQuestion::bare)
        .collect()
}

// Approval must fail closed when the agent returns neither APPROVED nor a usable revision.
pub fn parse_approval(text: &str) -> Approval {
    // `APPROVED.`, `**APPROVED**`, `Approved — looks good`: the verdict leads the reply.
    let first = text
        .trim()
        .lines()
        .next()
        .map(bare_line)
        .unwrap_or_default();
    if is_only(first, "APPROVED")
        || after_word(first, "APPROVED")
            .is_some_and(|rest| rest.starts_with([':', '—', '-', '.', '!']))
    {
        return Approval::Approved;
    }
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

/// Read the critic's reply, wherever its verdict sits and however it is dressed:
/// a preamble line, `**GAPS:**`, `Gaps:`, numbered gap lists, or a GAPS and a
/// CONTRADICTION together. Gaps are capped so the pipeline cannot fan out unbounded.
pub fn parse_critique(text: &str) -> Critique {
    if is_only(text, "SATISFIED") {
        return Critique::satisfied();
    }
    let mut critique = Critique::default();
    let mut satisfied = false;
    let mut in_gaps = false;
    let mut expect_contradiction = false;
    for raw in text.lines() {
        let line = bare_line(raw);
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = after_word(line, "GAPS") {
            in_gaps = true;
            expect_contradiction = false;
            let first = rest.trim_start_matches(':').trim();
            if !first.is_empty() {
                critique.gaps.push(strip_list_prefix(first));
            }
            continue;
        }
        if let Some(rest) = after_word(line, "CONTRADICTION") {
            in_gaps = false;
            let desc = rest.trim_start_matches(':').trim();
            expect_contradiction = desc.is_empty();
            if !desc.is_empty() {
                critique.contradiction = Some(desc.to_string());
            }
            continue;
        }
        if expect_contradiction {
            critique.contradiction = Some(line.to_string());
            expect_contradiction = false;
            continue;
        }
        if in_gaps {
            let listed = raw.trim_start().starts_with(['-', '*', '•'])
                || raw.trim_start().starts_with(|c: char| c.is_ascii_digit());
            if listed {
                let gap = strip_list_prefix(raw.trim_start().trim_start_matches('•'));
                if !gap.is_empty() {
                    critique.gaps.push(gap);
                }
                continue;
            }
        }
        if after_word(line, "SATISFIED").is_some() {
            satisfied = true;
        }
    }
    critique.gaps.retain(|g| !g.is_empty());
    critique.gaps.truncate(MAX_SUBQUESTIONS);
    if critique.gaps.is_empty() && critique.contradiction.is_none() && !satisfied {
        critique.unreadable = true;
    }
    critique
}
