//! Cached syntax highlighting and vim selection styling for text fields.

use crate::ui::style::{SyntaxColors, syntax_style};
use crate::vim::Vim;
use dugix_core::highlight::Span;
use ratatui::style::Style;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tui_textarea::TextArea;

const CACHE_LIMIT: usize = 64;

type SpanCache = HashMap<(String, String), Rc<Vec<Span>>>;

thread_local! {
    static CACHE: RefCell<SpanCache> = RefCell::default();
}

fn spans(source: &str, lang: &str) -> Rc<Vec<Span>> {
    CACHE.with_borrow_mut(|cache| {
        let key = (lang.to_string(), source.to_string());
        if let Some(hit) = cache.get(&key) {
            return Rc::clone(hit);
        }
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        let spans = Rc::new(dugix_core::highlight::highlight(source, lang));
        cache.insert(key, Rc::clone(&spans));
        spans
    })
}

// TextArea custom highlights use byte offsets rather than character columns.
fn byte_col(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

#[derive(Clone, Copy)]
pub struct Decor<'a> {
    /// Syntax language and leading byte count to leave unhighlighted.
    pub syntax: Option<(&'a str, usize)>,
    pub colors: SyntaxColors,
    pub vim: Option<&'a Vim>,
    pub selection: Style,
}

pub fn decorate(ta: &mut TextArea<'_>, decor: Decor<'_>) {
    ta.clear_custom_highlight();
    if let Some((lang, skip)) = decor.syntax {
        paint_syntax(ta, lang, skip, decor.colors);
    }
    if let Some(((r0, c0), (r1, c1))) = decor.vim.and_then(|vim| vim.visual_range(ta)) {
        let start = (r0, byte_col(&ta.lines()[r0], c0));
        let end = (r1, byte_col(&ta.lines()[r1], c1));
        ta.custom_highlight((start, end), decor.selection, u8::MAX);
    }
}

fn paint_syntax(ta: &mut TextArea<'_>, lang: &str, skip: usize, colors: SyntaxColors) {
    let source = ta.lines().join("\n");
    let Some(body) = source.get(skip..) else {
        return;
    };
    let spans = spans(body, lang);
    if spans.is_empty() {
        return;
    }
    let starts: Vec<usize> = std::iter::once(0)
        .chain(ta.lines().iter().scan(0, |at, line| {
            *at += line.len() + 1;
            Some(*at)
        }))
        .collect();
    let row_of = |at: usize| starts.partition_point(|&start| start <= at) - 1;
    for span in spans.iter() {
        let (from, to) = (span.range.start + skip, span.range.end + skip);
        let style = syntax_style(colors, span.role);
        let (first, last) = (row_of(from), row_of(to.saturating_sub(1)));
        for (row, &line_start) in starts.iter().enumerate().take(last + 1).skip(first) {
            let line_end = line_start + ta.lines()[row].len();
            let (a, b) = (from.max(line_start), to.min(line_end));
            if a < b {
                ta.custom_highlight(((row, a - line_start), (row, b - line_start)), style, 1);
            }
        }
    }
}

#[must_use]
pub fn composer_syntax(text: &str) -> (&'static str, usize) {
    if let Some(rest) = text.strip_prefix("!tty ") {
        ("bash", text.len() - rest.len())
    } else if text.starts_with('!') {
        ("bash", 1)
    } else {
        ("markdown", 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dugix_core::highlight::Role;

    fn highlighted(text: &str, syntax: (&str, usize)) -> Vec<(usize, String, Style)> {
        let mut ta = TextArea::new(text.split('\n').map(str::to_owned).collect());
        decorate(
            &mut ta,
            Decor {
                syntax: Some(syntax),
                colors: SyntaxColors::default(),
                vim: None,
                selection: Style::default(),
            },
        );
        let mut buffer = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 4));
        ratatui::widgets::Widget::render(&ta, buffer.area, &mut buffer);
        let mut out = Vec::new();
        for y in 0..4 {
            let mut run = String::new();
            let mut style = None;
            for x in 0..40 {
                let cell = &buffer[(x, y)];
                let cell_style = Style::default().fg(cell.fg).add_modifier(cell.modifier);
                if style != Some(cell_style) && !run.is_empty() {
                    out.push((usize::from(y), std::mem::take(&mut run), style.unwrap()));
                }
                style = Some(cell_style);
                run.push_str(cell.symbol());
            }
            out.push((usize::from(y), run, style.unwrap()));
        }
        out
    }

    fn style_of(runs: &[(usize, String, Style)], row: usize, text: &str) -> Style {
        runs.iter()
            .find(|(r, run, _)| *r == row && run.trim() == text)
            .unwrap_or_else(|| panic!("{text:?} not a run on row {row}: {runs:?}"))
            .2
    }

    #[test]
    fn markdown_and_fenced_code_color_across_lines() {
        let colors = SyntaxColors::default();
        let runs = highlighted("# Title\n```rust\nfn é() {}\n```", ("markdown", 0));
        assert_eq!(
            style_of(&runs, 0, "Title").fg,
            syntax_style(colors, Role::Heading).fg
        );
        assert_eq!(
            style_of(&runs, 2, "fn").fg,
            syntax_style(colors, Role::Keyword).fg
        );
    }

    #[test]
    fn shell_drafts_highlight_after_the_bang() {
        assert_eq!(composer_syntax("!ls -la"), ("bash", 1));
        assert_eq!(composer_syntax("!tty vim x"), ("bash", 5));
        assert_eq!(composer_syntax("hi"), ("markdown", 0));
        let runs = highlighted("!echo \"hi\"", composer_syntax("!echo \"hi\""));
        assert_eq!(
            style_of(&runs, 0, "\"hi\"").fg,
            syntax_style(SyntaxColors::default(), Role::String).fg
        );
    }

    #[test]
    fn byte_columns_count_utf8_widths() {
        assert_eq!(byte_col("héllo", 2), 3);
        assert_eq!(byte_col("ab", 5), 2);
    }
}
