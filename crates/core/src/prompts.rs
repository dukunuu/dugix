//! Model-facing prompt text bundled from `assets/prompts/`.
//! Edits change model behavior.

pub const AGENT: &str = include_str!("../assets/prompts/agent.md");
pub const SUBAGENT: &str = include_str!("../assets/prompts/subagent.md");
pub const RESEARCH_PLANNER: &str = include_str!("../assets/prompts/research/planner.md");
pub const RESEARCH_SURVEY: &str = include_str!("../assets/prompts/research/survey.md");
pub const RESEARCH_PLAN_APPROVAL: &str =
    include_str!("../assets/prompts/research/plan-approval.md");
pub const RESEARCH_SEARCHER: &str = include_str!("../assets/prompts/research/searcher.md");
pub const RESEARCH_CACHE_PREFIX: &str = include_str!("../assets/prompts/research/cache-prefix.md");
pub const RESEARCH_SYNTHESIZER: &str = include_str!("../assets/prompts/research/synthesizer.md");
pub const RESEARCH_CRITIC: &str = include_str!("../assets/prompts/research/critic.md");
pub const RESEARCH_RESOLVER: &str = include_str!("../assets/prompts/research/resolver.md");
pub const RESEARCH_VERIFIER: &str = include_str!("../assets/prompts/research/verifier.md");
pub const RESEARCH_WRITER: &str = include_str!("../assets/prompts/research/writer.md");
pub const RESEARCH_TOPIC_FROM_CHAT: &str =
    include_str!("../assets/prompts/research/topic-from-chat.md");
pub const COMPACTION: &str = include_str!("../assets/prompts/compaction.md");
pub const COMPACTION_ITEMIZED: &str = include_str!("../assets/prompts/compaction-itemized.md");
pub const MEMORY_TIDY: &str = include_str!("../assets/prompts/memory-tidy.md");
pub const TITLE: &str = include_str!("../assets/prompts/title.md");
pub const SWARM_DRAFT: &str = include_str!("../assets/prompts/swarm/draft.md");
pub const SWARM_VERIFY: &str = include_str!("../assets/prompts/swarm/verify.md");
pub const SWARM_SYNTHESIS: &str = include_str!("../assets/prompts/swarm/synthesis.md");

/// Unknown keys panic in debug builds and remain unchanged in release builds.
#[must_use]
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut rest = template.strip_suffix('\n').unwrap_or(template);
    let mut rendered = String::with_capacity(rest.len());
    while let Some((before, after)) = rest.split_once("{{") {
        let Some((key, tail)) = after.split_once("}}") else {
            break;
        };
        rendered.push_str(before);
        let value = vars.iter().find(|(name, _)| *name == key);
        debug_assert!(value.is_some(), "unknown prompt placeholder: {key}");
        if let Some((_, value)) = value {
            rendered.push_str(value);
        } else {
            rendered.push_str("{{");
            rendered.push_str(key);
            rendered.push_str("}}");
        }
        rest = tail;
    }
    rendered.push_str(rest);
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const TEMPLATES: &[(&str, &[&str])] = &[
        (RESEARCH_PLANNER, &[]),
        (RESEARCH_SURVEY, &[]),
        (RESEARCH_PLAN_APPROVAL, &[]),
        (RESEARCH_SEARCHER, &[]),
        (RESEARCH_CACHE_PREFIX, &[]),
        (RESEARCH_SYNTHESIZER, &[]),
        (RESEARCH_CRITIC, &[]),
        (RESEARCH_RESOLVER, &[]),
        (RESEARCH_VERIFIER, &[]),
        (RESEARCH_WRITER, &[]),
        (RESEARCH_TOPIC_FROM_CHAT, &["conversation"]),
        (COMPACTION, &[]),
        (MEMORY_TIDY, &[]),
        (TITLE, &["conversation"]),
        (SWARM_DRAFT, &["drafter", "focus"]),
        (SWARM_VERIFY, &["topic", "drafts"]),
        (SWARM_SYNTHESIS, &["topic", "drafts", "review"]),
    ];

    #[test]
    fn itemized_compaction_template_renders_without_placeholders() {
        let rendered = render(COMPACTION_ITEMIZED, &[]);
        assert!(!rendered.contains("{{"));
        assert!(COMPACTION_ITEMIZED.ends_with('\n'));
        assert!(!COMPACTION_ITEMIZED.ends_with("\n\n"));
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("## "))
                .collect::<Vec<_>>(),
            [
                "## Goal",
                "## Decisions",
                "## Files and code",
                "## Facts learned",
                "## Open threads",
            ]
        );
    }

    #[test]
    fn agent_addendum_renders_without_placeholders() {
        let rendered = render(AGENT, &[]);
        assert!(rendered.starts_with("## Agent mode\n"));
        assert!(rendered.contains("The user approves every change and command."));
        assert!(AGENT.ends_with('\n'));
        assert!(!AGENT.ends_with("\n\n"));
        assert!(!rendered.contains("{{"));
    }

    #[test]
    fn render_fills_placeholders_in_one_pass() {
        assert_eq!(render("a {{x}} b\n", &[("x", "{{x}}")]), "a {{x}} b");
    }

    #[test]
    fn every_template_declares_exactly_its_placeholders() {
        for &(template, keys) in TEMPLATES {
            let mut found = BTreeSet::new();
            let mut rest = template;
            while let Some((_, after)) = rest.split_once("{{") {
                let (key, tail) = after.split_once("}}").expect("closed placeholder");
                found.insert(key);
                rest = tail;
            }
            assert_eq!(found, keys.iter().copied().collect());
            let vars: Vec<_> = keys.iter().map(|&key| (key, "value")).collect();
            assert!(!render(template, &vars).contains("{{"));
        }
    }

    #[test]
    fn prompt_files_end_with_one_newline() {
        for &(template, _) in TEMPLATES {
            assert!(template.ends_with('\n'));
            assert!(!template.ends_with("\n\n"));
        }
    }

    #[test]
    fn representative_prompts_match_their_previous_text() {
        assert_eq!(
            render(RESEARCH_CRITIC, &[]),
            "You are the critic stage of a research pipeline. Given the original topic and a draft report, decide if it's ready. Respond with nothing but your verdict, in one of these forms:\n- the single word SATISFIED, if the draft thoroughly covers the topic with no notable gaps or contradictions.\n- GAPS: followed by a newline-separated bullet list (each line starting with '- ') of specific missing sub-topics or unanswered angles, each phrased as a searchable question.\n- CONTRADICTION: followed by one line describing a specific factual contradiction between sources in the draft that isn't resolved.\nIf the draft has both gaps and a contradiction, give the GAPS block first, then the CONTRADICTION line. Use CONTRADICTION only for an actual conflict between sources, not a missing angle — missing angles are always GAPS."
        );
    }
}
