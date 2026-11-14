//! One numbering for every source a research run cites.
//!
//! Each searcher numbers its own sources from `[1]`, so the same page is `[1]`
//! for one searcher and `[3]` for the next. The ledger gives every distinct URL
//! one run-wide number as findings arrive and rewrites each searcher's `[n]`
//! onto it, so a citation always names the page its searcher read. The final
//! `## Sources` list is built from the ledger, never written by a model, so a
//! report cannot cite a URL that no searcher found.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::tools::{normalize_url, split_sources};

/// Characters of cached page text the verifier sees per source.
const EXCERPT_CHARS_PER_SOURCE: usize = 1_600;
/// Characters of excerpts the verifier sees in all.
const EXCERPT_CHARS_TOTAL: usize = 48_000;

#[derive(Debug, Default)]
pub(super) struct Ledger {
    /// Run-wide number `n` is `urls[n - 1]`.
    urls: Vec<String>,
    by_norm: HashMap<String, usize>,
    /// Citations that named no listed source, across every absorbed finding.
    pub(super) unlisted: usize,
}

impl Ledger {
    pub(super) fn len(&self) -> usize {
        self.urls.len()
    }

    fn number(&mut self, url: &str) -> usize {
        let norm = normalize_url(url);
        if let Some(&n) = self.by_norm.get(&norm) {
            return n;
        }
        self.urls.push(url.to_string());
        self.by_norm.insert(norm, self.urls.len());
        self.urls.len()
    }

    /// A searcher's findings with its sources list removed and its `[n]` renumbered
    /// onto the ledger. A citation its list does not name is dropped.
    pub(super) fn absorb(&mut self, finding: &str) -> String {
        let split = split_sources(finding);
        let local: HashMap<usize, usize> = split
            .sources
            .iter()
            .filter_map(|(n, url)| Some((n.as_ref().copied()?, self.number(url))))
            .collect();
        let (body, dropped) = rewrite_citations(split.body, |n| local.get(&n).copied());
        self.unlisted += dropped;
        body
    }

    /// `Sources:` lines for run-wide numbers `from + 1` onwards.
    pub(super) fn list_from(&self, from: usize) -> String {
        let mut out = String::from("Sources:\n");
        for (i, url) in self.urls.iter().enumerate().skip(from) {
            let _ = writeln!(out, "{}. {url}", i + 1);
        }
        out
    }

    /// The text each source's page holds about the draft's claims citing it, from
    /// the pages the searchers fetched (cache only; nothing is fetched here).
    pub(super) fn excerpts(&self, draft: &str, page: impl Fn(&str) -> Option<String>) -> Excerpts {
        let mut claims: HashMap<usize, Vec<&str>> = HashMap::new();
        for sentence in sentences(draft) {
            for n in cited_numbers(sentence) {
                claims.entry(n).or_default().push(sentence);
            }
        }
        let mut cited: Vec<usize> = claims.keys().copied().collect();
        cited.sort_unstable();
        let mut out = Excerpts::default();
        for n in cited {
            let Some(url) = self.urls.get(n.wrapping_sub(1)) else {
                continue;
            };
            if out.text.len() >= EXCERPT_CHARS_TOTAL {
                out.skipped += 1;
                continue;
            }
            match page(&normalize_url(url)) {
                Some(text) if !text.trim().is_empty() => {
                    let excerpt = best_passages(&text, &claims[&n], EXCERPT_CHARS_PER_SOURCE);
                    let _ = write!(out.text, "[{n}] {url}\n{excerpt}\n\n");
                }
                _ => {
                    out.unopened += 1;
                    let _ = write!(
                        out.text,
                        "[{n}] {url}\n(never opened by a searcher — cited from a search result only)\n\n"
                    );
                }
            }
        }
        out
    }

    /// The final report: citations renumbered in order of first use, any sources
    /// list the writer added replaced by the ledger's, limitations above it.
    pub(super) fn finish(&self, report: &str, limitations: &[String]) -> Finished {
        let body = split_sources(report).body.trim_end();
        let mut order: Vec<usize> = Vec::new();
        let (body, dropped) = rewrite_citations(body, |n| {
            self.urls.get(n.wrapping_sub(1))?;
            let at = order.iter().position(|&seen| seen == n).unwrap_or_else(|| {
                order.push(n);
                order.len() - 1
            });
            Some(at + 1)
        });
        let mut notes = limitations.to_vec();
        if dropped > 0 {
            notes.push(format!(
                "{dropped} citation{} named no source the searchers found and {} removed.",
                plural(dropped),
                if dropped == 1 { "was" } else { "were" }
            ));
        }
        let mut out = body.trim_end().to_string();
        if !notes.is_empty() {
            out.push_str("\n\n## Limitations\n\n");
            for note in &notes {
                let _ = writeln!(out, "- {note}");
            }
        }
        if !order.is_empty() {
            out.truncate(out.trim_end().len());
            out.push_str("\n\n## Sources\n\n");
            for (i, n) in order.iter().enumerate() {
                let _ = writeln!(out, "{}. {}", i + 1, self.urls[n - 1]);
            }
        }
        Finished {
            report: out.trim_end().to_string() + "\n",
            dropped,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Excerpts {
    pub(super) text: String,
    /// Cited sources no searcher fetched: only a search snippet backs them.
    pub(super) unopened: usize,
    /// Cited sources left out once the excerpt budget ran out.
    pub(super) skipped: usize,
}

#[derive(Debug)]
pub(super) struct Finished {
    pub(super) report: String,
    pub(super) dropped: usize,
}

pub(super) const fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Rewrite every citation bracket — `[3]`, `[1, 4]`, `[2-3]`, `[1][2]` — through
/// `map`; numbers it rejects are dropped, and a bracket left empty goes with the
/// space before it. Returns the text and how many numbers were dropped. Markdown
/// links (`[text](url)`) and non-numeric brackets are left alone.
pub(super) fn rewrite_citations(
    text: &str,
    mut map: impl FnMut(usize) -> Option<usize>,
) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut dropped = 0;
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            break;
        };
        let inner = &after[..close];
        let tail = &after[close + 1..];
        let Some(numbers) = citation_numbers(inner).filter(|_| !tail.starts_with('(')) else {
            out.push_str(&rest[..=open]);
            rest = after;
            continue;
        };
        let mut kept: Vec<usize> = Vec::new();
        for n in numbers {
            match map(n) {
                Some(m) if !kept.contains(&m) => kept.push(m),
                Some(_) => {}
                None => dropped += 1,
            }
        }
        out.push_str(&rest[..open]);
        if kept.is_empty() {
            let trimmed = out.trim_end_matches([' ', '\t']).len();
            out.truncate(trimmed);
        } else {
            let list = kept.iter().map(usize::to_string).collect::<Vec<_>>();
            let _ = write!(out, "[{}]", list.join(", "));
        }
        rest = tail;
    }
    out.push_str(rest);
    (out, dropped)
}

/// The numbers of a citation bracket's inside, or `None` when it is not one.
fn citation_numbers(inner: &str) -> Option<Vec<usize>> {
    let inner = inner.trim();
    if inner.is_empty() || !inner.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let mut out = Vec::new();
    for part in inner.split([',', ';']) {
        let part = part.trim();
        if let Some((a, b)) = part.split_once(['-', '–']) {
            let (a, b): (usize, usize) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
            if b < a || b - a > 20 {
                return None;
            }
            out.extend(a..=b);
        } else {
            out.push(part.parse().ok()?);
        }
    }
    (!out.is_empty() && out.iter().all(|&n| n > 0)).then_some(out)
}

fn cited_numbers(sentence: &str) -> Vec<usize> {
    let mut found = Vec::new();
    rewrite_citations(sentence, |n| {
        found.push(n);
        Some(n)
    });
    found
}

/// Rough sentences: enough to pair a claim with the citations at its end.
fn sentences(text: &str) -> impl Iterator<Item = &str> {
    text.split_inclusive(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|s| s.len() > 12)
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 3)
        .map(str::to_lowercase)
        .collect()
}

/// The page's paragraphs sharing the most words with the claims, best first,
/// up to `budget` characters.
fn best_passages(page: &str, claims: &[&str], budget: usize) -> String {
    let wanted: std::collections::HashSet<String> = claims.iter().flat_map(|c| words(c)).collect();
    let mut scored: Vec<(usize, usize, &str)> = page
        .split("\n\n")
        .flat_map(|block| {
            // Very long blocks are windowed so one wall of text cannot take the whole budget.
            let mut pieces = Vec::new();
            let mut start = 0;
            while start < block.len() {
                let mut end = (start + 700).min(block.len());
                while !block.is_char_boundary(end) {
                    end += 1;
                }
                pieces.push(&block[start..end]);
                start = end;
            }
            pieces
        })
        .map(str::trim)
        .filter(|p| p.len() > 40)
        .enumerate()
        .map(|(i, p)| {
            let score = words(p).iter().filter(|w| wanted.contains(*w)).count();
            (score, i, p)
        })
        .filter(|(score, ..)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out = String::new();
    for (_, _, passage) in scored {
        if out.len() + passage.len() > budget {
            if out.is_empty() {
                let mut end = budget.min(passage.len());
                while !passage.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&passage[..end]);
                out.push('…');
            }
            break;
        }
        if !out.is_empty() {
            out.push_str("\n…\n");
        }
        out.push_str(passage);
    }
    if out.is_empty() {
        "(the cached page shares no wording with the claims citing it)".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_source_keeps_one_number_and_every_citation_keeps_its_page() {
        let mut ledger = Ledger::default();
        let a = ledger.absorb("Rust is fast [1].\nSources:\n1. https://www.rust-lang.org/");
        let b = ledger.absorb(
            "Rust is safe [1] and loved [2].\n**Sources:**\n1. https://www.rust-lang.org\n2. https://survey.example/2024",
        );
        assert_eq!(a, "Rust is fast [1].");
        // The page both cited is [1] in both; the new one is [2].
        assert_eq!(b, "Rust is safe [1] and loved [2].");
        assert_eq!(ledger.len(), 2);
        assert_eq!(
            ledger.list_from(0),
            "Sources:\n1. https://www.rust-lang.org/\n2. https://survey.example/2024\n"
        );
        assert_eq!(
            ledger.list_from(1),
            "Sources:\n2. https://survey.example/2024\n"
        );
    }

    #[test]
    fn local_numbers_move_onto_the_ledger_and_unlisted_ones_go() {
        let mut ledger = Ledger::default();
        ledger.absorb("X [1].\nSources:\n1. https://a.example");
        let body = ledger.absorb(
            "Y [1, 2] and [3-4]; Z [9]. A [link](https://x.example) and [note].\nSources:\n1. https://b.example\n2. https://a.example\n3. https://c.example\n4. https://d.example",
        );
        assert_eq!(
            body,
            "Y [2, 1] and [3, 4]; Z. A [link](https://x.example) and [note]."
        );
        assert_eq!(ledger.unlisted, 1);
    }

    #[test]
    fn the_finished_report_lists_only_what_it_cites_from_the_ledger() {
        let mut ledger = Ledger::default();
        ledger.absorb(
            "a [1] b [2] c [3]\nSources:\n1. https://a.example\n2. https://b.example\n3. https://c.example",
        );
        let report = "# Title\n\nFirst [3]. Then [1][3]. Invented [7].\n\n## Sources\n1. https://made-up.example\n";
        let done = ledger.finish(report, &["Not covered: pricing.".to_string()]);
        assert_eq!(
            done.report,
            "# Title\n\nFirst [1]. Then [2][1]. Invented.\n\n## Limitations\n\n- Not covered: pricing.\n- 1 citation named no source the searchers found and was removed.\n\n## Sources\n\n1. https://c.example\n2. https://a.example\n"
        );
        assert_eq!(done.dropped, 1);
        // Downstream readers (citations table, run diffs) parse this list.
        assert_eq!(
            crate::citations::parse_citations(&done.report),
            vec![
                (1, "https://c.example".into()),
                (2, "https://a.example".into())
            ]
        );
    }

    #[test]
    fn excerpts_quote_the_cached_page_near_each_claim() {
        let mut ledger = Ledger::default();
        ledger.absorb("x [1] y [2]\nSources:\n1. https://a.example/page\n2. https://b.example");
        let page = "Unrelated navigation text that says nothing useful here at all.\n\nThe Tokio runtime schedules tasks with a work-stealing scheduler across threads.";
        let excerpts = ledger.excerpts(
            "Tokio uses a work-stealing scheduler [1]. Something else entirely [2].",
            |norm| (norm == "https://a.example/page").then(|| page.to_string()),
        );
        assert!(
            excerpts
                .text
                .contains("[1] https://a.example/page\nThe Tokio runtime")
        );
        assert!(!excerpts.text.contains("Unrelated navigation"));
        assert!(
            excerpts
                .text
                .contains("[2] https://b.example\n(never opened")
        );
        assert_eq!(excerpts.unopened, 1);
    }
}
