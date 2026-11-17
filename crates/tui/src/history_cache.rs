//! Cached transcript layout for redraws and session switches.

use std::collections::HashMap;

use ratatui::text::Line;

#[derive(Clone, Debug)]
pub struct BatchRow {
    pub owner: usize,
    pub index: usize,
    pub lines: std::ops::Range<usize>,
}

#[derive(Default)]
pub struct HistoryCache {
    pub key: (Option<String>, usize, bool, bool, bool, bool, usize),
    pub msg_count: usize,
    pub image_generation: usize,
    pub lines: Vec<Line<'static>>,
    pub owner: Vec<Option<usize>>,
    pub batch_rows: Vec<BatchRow>,
    pub code: Vec<Option<usize>>,
    pub blocks: Vec<String>,
    pub diagrams: Vec<crate::diagrams::Placement>,
    pub widgets: Vec<crate::interactive::Placement>,
    pub plain: Vec<String>,
    pub image_at_line: Vec<Option<String>>,
    pub image_cache: HashMap<(String, usize), Vec<Line<'static>>>,
    pub last_day: Option<String>,
}
