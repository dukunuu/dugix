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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Critique {
    Satisfied,
    Gaps(Vec<String>),
    Contradiction(String),
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
    let upper = text.trim().to_ascii_uppercase();
    if upper == "APPROVED"
        || upper.starts_with("APPROVED:")
        || upper.starts_with("APPROVED —")
        || upper.starts_with("APPROVED\n")
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

// Unparseable critiques must not keep the pipeline looping indefinitely.
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
