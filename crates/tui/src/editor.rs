//! Decorations painted onto a `TextArea` each frame: the vim Visual
//! selection (and, with `docs/editor.md` phase E4, syntax colors).

use crate::vim::Vim;
use ratatui::style::Style;
use tui_textarea::TextArea;

/// Byte offset of char column `col` in `line` (custom highlights use bytes).
fn byte_col(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

/// Repaint `ta`'s custom highlights: the Visual selection when vim is in a
/// Visual mode.
pub fn decorate(ta: &mut TextArea<'_>, vim: Option<&Vim>, selection: Style) {
    ta.clear_custom_highlight();
    if let Some(((r0, c0), (r1, c1))) = vim.and_then(|vim| vim.visual_range(ta)) {
        let start = (r0, byte_col(&ta.lines()[r0], c0));
        let end = (r1, byte_col(&ta.lines()[r1], c1));
        ta.custom_highlight((start, end), selection, u8::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::byte_col;

    #[test]
    fn byte_columns_count_utf8_widths() {
        assert_eq!(byte_col("héllo", 2), 3);
        assert_eq!(byte_col("ab", 5), 2);
    }
}
