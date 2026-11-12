//! Derived sheet geometry: the height a wrapped row needs and the width a column
//! needs for its contents, measured with the very font `render.rs` draws with.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
use super::{
    Sheet, Tab, Workbook,
    address::{Address, column_name},
    model::{Align, Wrap},
};
use crate::deck::text::{Face, Typesetter};
use std::collections::BTreeMap;

/// The font size `render.rs` draws cell text at, with its line height and padding.
pub const FONT_SIZE: f64 = 14.0;
pub const LINE_HEIGHT: f64 = 17.0;
pub const PAD_X: f64 = 5.0;
pub const PAD_Y: f64 = 2.0;
/// The height of a row nobody resized.
pub const DEFAULT_ROW: f64 = 24.0;
/// The width of a column nobody resized, and the limits of a fitted one.
pub const DEFAULT_COL: f64 = 100.0;
pub const MIN_FIT: f64 = 40.0;
pub const MAX_FIT: f64 = 480.0;

/// Cell text measured with the sheet's body font, as the PNG and the grid draw it.
pub struct CellText<'a> {
    typesetter: Typesetter<'a>,
    family: String,
}

impl<'a> CellText<'a> {
    #[must_use]
    pub fn new(theme: &'a crate::deck::Theme) -> Self {
        Self {
            typesetter: Typesetter::new(theme, None),
            family: theme
                .fonts
                .get("body")
                .cloned()
                .unwrap_or_else(|| "sans-serif".into()),
        }
    }

    fn face(&self, bold: bool) -> Face {
        Face {
            family: self.family.clone(),
            weight: if bold { 700 } else { 400 },
            italic: false,
            spacing: 0.0,
        }
    }

    /// Pixel width of one line of `text`.
    pub fn width(&mut self, text: &str, bold: bool) -> f64 {
        let face = self.face(bold);
        self.typesetter.advance(&face, FONT_SIZE, text)
    }

    /// The widest word, for a wrapped cell that only needs its longest word to fit.
    pub fn longest_word(&mut self, text: &str, bold: bool) -> f64 {
        let mut widest = 0.0_f64;
        for word in text.split_whitespace() {
            widest = widest.max(self.width(word, bold));
        }
        widest
    }

    /// Greedy word wrap into `width`; overlong words break anywhere, like a browser.
    /// Explicit newlines start a new line.
    pub fn lines(&mut self, text: &str, width: f64, bold: bool) -> Vec<String> {
        let width = width.max(1.0);
        let space = self.width(" ", bold);
        let mut out = Vec::new();
        for paragraph in text.split('\n') {
            let mut line = String::new();
            let mut used = 0.0;
            for word in paragraph.split_whitespace() {
                let word_width = self.width(word, bold);
                if !line.is_empty() && used + space + word_width > width {
                    out.push(std::mem::take(&mut line));
                    used = 0.0;
                }
                if word_width <= width {
                    if !line.is_empty() {
                        line.push(' ');
                        used += space;
                    }
                    line.push_str(word);
                    used += word_width;
                    continue;
                }
                for character in word.chars() {
                    let part = self.width(&character.to_string(), bold);
                    if !line.is_empty() && used + part > width {
                        out.push(std::mem::take(&mut line));
                        used = 0.0;
                    }
                    line.push(character);
                    used += part;
                }
            }
            out.push(line);
        }
        out
    }
}

/// The columns a single-line cell's text may spill over. `empty` reports whether a
/// column in this row holds no value. Wrapped, clipped and numeric cells never
/// spill; left-aligned text grows right, right-aligned left, centered both ways.
#[must_use]
pub fn overflow_span(
    align: Align,
    wrap: Option<Wrap>,
    numeric: bool,
    col: u32,
    last: u32,
    empty: impl Fn(u32) -> bool,
) -> (u32, u32) {
    if wrap.is_some() || numeric {
        return (col, col);
    }
    let (mut first, mut end) = (col, col);
    if matches!(align, Align::Left | Align::Center) {
        while end < last && empty(end + 1) {
            end += 1;
        }
    }
    if matches!(align, Align::Right | Align::Center) {
        while first > 1 && empty(first - 1) {
            first -= 1;
        }
    }
    (first, end)
}

fn merges(tab: &Tab) -> Vec<super::address::Rect> {
    tab.merges
        .iter()
        .filter_map(|text| super::validate::merge_range(text).ok())
        .collect()
}

/// The width a cell's text has to fit: its column, or the whole merge it anchors.
fn text_width(tab: &Tab, merges: &[super::address::Rect], address: Address) -> f64 {
    let columns = merges
        .iter()
        .find(|rect| rect.start == address)
        .map_or(address.col..=address.col, |rect| {
            rect.start.col..=rect.end.col
        });
    columns
        .filter_map(|col| column_name(col).ok())
        .map(|name| tab.cols.get(&name).copied().unwrap_or(DEFAULT_COL))
        .sum()
}

/// The height each row needs for its wrapped cells, for rows with no user-set
/// height. Only rows taller than the default are returned.
#[must_use]
pub fn auto_row_heights(sheet: &Sheet, workbook: &Workbook, tab: &Tab) -> BTreeMap<u32, f64> {
    let mut heights = BTreeMap::new();
    let wrapped = tab
        .cells
        .iter()
        .filter(|(_, cell)| cell.fmt.wrap == Some(Wrap::Wrap))
        .filter_map(|(key, cell)| Some((key.parse::<Address>().ok()?, &cell.fmt)))
        .collect::<Vec<_>>();
    if wrapped.is_empty() {
        return heights;
    }
    let theme = super::render::theme(sheet);
    let mut measure = CellText::new(&theme);
    let merges = merges(tab);
    for (address, fmt) in wrapped {
        if tab.rows.contains_key(&address.row) {
            continue;
        }
        let value = workbook.value(&tab.name, &address.to_string());
        if value.display.is_empty() {
            continue;
        }
        let width = text_width(tab, &merges, address) - 2.0 * PAD_X;
        let lines = measure.lines(&value.display, width, fmt.bold).len();
        let height = lines as f64 * LINE_HEIGHT + 2.0 * PAD_Y;
        if height > DEFAULT_ROW {
            let entry = heights.entry(address.row).or_insert(height);
            *entry = entry.max(height);
        }
    }
    heights
}

/// The width a column needs for every value in it: the widest display value over
/// the tab's used range, plus padding, clamped. An empty column keeps the default.
#[must_use]
pub fn column_fit_width(sheet: &Sheet, workbook: &Workbook, tab: &Tab, col: u32) -> f64 {
    let theme = super::render::theme(sheet);
    let mut measure = CellText::new(&theme);
    let merges = merges(tab);
    let mut widest: Option<f64> = None;
    for (key, cell) in &tab.cells {
        let Ok(address) = key.parse::<Address>() else {
            continue;
        };
        // A merged cell's text belongs to the merge, not to one column.
        if address.col != col || merges.iter().any(|rect| rect.contains(address)) {
            continue;
        }
        let value = workbook.value(&tab.name, key);
        if value.display.is_empty() {
            continue;
        }
        // A wrapped cell only has to fit its longest word.
        let width = if cell.fmt.wrap == Some(Wrap::Wrap) {
            measure.longest_word(&value.display, cell.fmt.bold)
        } else {
            measure.width(&value.display, cell.fmt.bold)
        };
        widest = Some(widest.map_or(width, |best: f64| best.max(width)));
    }
    widest.map_or(DEFAULT_COL, |width| {
        (width + 2.0 * PAD_X).clamp(MIN_FIT, MAX_FIT)
    })
}

/// The columns a tab's used range covers, as `A:C` text.
#[must_use]
pub fn used_columns(tab: &Tab) -> Option<String> {
    let used = tab.used_range()?;
    Some(format!(
        "{}:{}",
        column_name(used.start.col).ok()?,
        column_name(used.end.col).ok()?
    ))
}
