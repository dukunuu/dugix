//! Syntax colors for code blocks: the page sends each block's text and
//! language, the backend answers with tree-sitter spans in UTF-16 offsets
//! (what JS strings and DOM ranges count), each tagged with the design color
//! key it paints in.

use serde::{Deserialize, Serialize};

/// Most code blocks one request may carry, and the most text in all of them.
const MAX_BLOCKS: usize = 512;
const MAX_TEXT: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    id: String,
    lang: String,
    text: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Reply {
    id: String,
    /// `[start, end, color key]`, UTF-16 offsets into the block's text.
    spans: Vec<(usize, usize, &'static str)>,
}

/// Spans for each block; blocks past the limits get none.
pub fn highlight(blocks: &[Request]) -> Vec<Reply> {
    let mut budget = MAX_TEXT;
    blocks
        .iter()
        .take(MAX_BLOCKS)
        .map(|block| {
            let spans = if block.text.len() <= budget {
                budget -= block.text.len();
                utf16_spans(&block.text, &block.lang)
            } else {
                Vec::new()
            };
            Reply {
                id: block.id.clone(),
                spans,
            }
        })
        .collect()
}

fn utf16_spans(text: &str, lang: &str) -> Vec<(usize, usize, &'static str)> {
    let spans = nexus_core::highlight::highlight(text, lang);
    if spans.is_empty() {
        return Vec::new();
    }
    // UTF-16 offset of every byte boundary we may be asked about.
    let mut units = vec![0; text.len() + 1];
    let mut at = 0;
    for (index, ch) in text.char_indices() {
        units[index] = at;
        at += ch.len_utf16();
    }
    units[text.len()] = at;
    spans
        .into_iter()
        .map(|span| {
            (
                units[span.range.start],
                units[span.range.end],
                span.role.color_key(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_count_utf16_units_and_name_design_colors() {
        let replies = highlight(&[
            Request {
                id: "a".into(),
                lang: "rust".into(),
                text: "let s = \"🙂\"; let t = 1;".into(),
            },
            Request {
                id: "b".into(),
                lang: "nope".into(),
                text: "x".into(),
            },
        ]);
        assert_eq!(
            replies[1],
            Reply {
                id: "b".into(),
                spans: vec![]
            }
        );
        let spans = &replies[0].spans;
        assert!(spans.contains(&(0, 3, "accent")), "{spans:?}");
        // The emoji is two UTF-16 units, so the string spans 8..12.
        assert!(spans.contains(&(8, 12, "accent2")), "{spans:?}");
        assert!(spans.contains(&(14, 17, "accent")), "{spans:?}");
    }
}
