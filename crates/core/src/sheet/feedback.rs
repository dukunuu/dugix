//! Cell anchors and suggested fixes on the shared comments sidecar.
use super::{
    Op, Sheet, Workbook,
    address::{Range, Rect, column_index},
};
pub use crate::deck::feedback::{EditStatus, Role, Severity, now, sidecar};
use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A stable tab id and an inclusive A1 selection, with its original display text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub tab: String,
    pub range: String,
    pub quote: String,
}
impl Anchor {
    /// Validate a selection and capture the top-left cell's computed display.
    pub fn new(sheet: &Sheet, tab: &str, range: &str) -> Result<Self> {
        let tab = sheet
            .tabs
            .iter()
            .find(|t| t.id == tab || t.name.eq_ignore_ascii_case(tab))
            .context("unknown tab")?;
        let range: Range = range.parse()?;
        ensure!(matches!(range, Range::Cells(_)), "select a cell range");
        Ok(Self {
            tab: tab.id.clone(),
            range: range.to_string(),
            quote: Workbook::load(sheet)?
                .value(&tab.name, &range.bounds().start.to_string())
                .display,
        })
    }
}

/// A reviewed batch, guarded against intervening workbook edits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edit {
    pub id: String,
    pub ops: Vec<Op>,
    pub status: EditStatus,
    pub revision: String,
}
impl Edit {
    /// Capture the workbook revision against which the fix was prepared.
    #[must_use]
    pub fn new(sheet: &Sheet, ops: Vec<Op>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            ops,
            status: EditStatus::Pending,
            revision: sheet.revision(),
        }
    }
}
pub type Message = crate::deck::feedback::Message<Edit>;

/// The shared thread lifecycle, with a sheet-specific anchor and deletion state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub anchor: Anchor,
    #[serde(default)]
    pub orphaned: bool,
    pub severity: Severity,
    pub created: u64,
    pub resolved: Option<u64>,
    pub messages: Vec<Message>,
}

/// Load the same atomic sidecar as docs and decks.
pub fn load(path: &Path) -> Result<Vec<Thread>> {
    let path = sidecar(path);
    ensure!(!path.is_symlink(), "comments file must not be a symlink");
    if !path.exists() {
        return Ok(vec![]);
    }
    serde_json::from_slice(&std::fs::read(path)?).context("invalid comments file")
}
/// Save cell-anchored threads beside the workbook.
pub fn save(path: &Path, threads: &[Thread]) -> Result<()> {
    crate::deck::write_atomic(&sidecar(path), &serde_json::to_vec_pretty(threads)?)
}

fn shift(rect: &mut Rect, at: u32, count: u32, rows: bool, delete: bool) -> bool {
    let (start, end) = if rows {
        (&mut rect.start.row, &mut rect.end.row)
    } else {
        (&mut rect.start.col, &mut rect.end.col)
    };
    if delete {
        if *start >= at && *end < at + count {
            return false;
        }
        let map = |n: u32| {
            if n < at {
                n
            } else if n < at + count {
                at
            } else {
                n - count
            }
        };
        let next_end = if *end >= at && *end < at + count {
            at - 1
        } else {
            map(*end)
        };
        *start = map(*start);
        *end = next_end;
    } else {
        if *start >= at {
            *start += count;
        }
        if *end >= at {
            *end += count;
        }
    }
    *start <= *end
        && rect.end.row <= super::address::MAX_ROWS
        && rect.end.col <= super::address::MAX_COLS
}

/// Follow structural edits in order; deleted selections retain their old anchor.
pub fn reanchor(threads: &mut [Thread], before: &Sheet, after_ops: &[Op]) {
    let mut tabs: Vec<_> = before
        .tabs
        .iter()
        .map(|t| (t.id.clone(), t.name.clone()))
        .collect();
    for op in after_ops {
        for thread in threads.iter_mut().filter(|t| !t.orphaned) {
            let Some((_, name)) = tabs.iter().find(|(id, _)| id == &thread.anchor.tab) else {
                continue;
            };
            let Ok(range) = thread.anchor.range.parse::<Range>() else {
                continue;
            };
            let mut rect = range.bounds();
            let same = |tab: &str| name.eq_ignore_ascii_case(tab);
            let alive = match op {
                Op::InsertRows { tab, at, count } if same(tab) => {
                    shift(&mut rect, *at, *count, true, false)
                }
                Op::DeleteRows { tab, at, count } if same(tab) => {
                    shift(&mut rect, *at, *count, true, true)
                }
                Op::InsertCols { tab, at, count } if same(tab) => shift(
                    &mut rect,
                    column_index(at).expect("validated op"),
                    *count,
                    false,
                    false,
                ),
                Op::DeleteCols { tab, at, count } if same(tab) => shift(
                    &mut rect,
                    column_index(at).expect("validated op"),
                    *count,
                    false,
                    true,
                ),
                Op::DeleteTab { tab } if same(tab) => false,
                Op::Copy {
                    tab,
                    from,
                    to_tab,
                    to,
                    cut: true,
                } if same(tab) => {
                    let source = from.parse::<Range>().expect("validated op").bounds();
                    if source.contains(rect.start) && source.contains(rect.end) {
                        let target: super::address::Address = to.parse().expect("validated op");
                        rect.start.row = target.row + rect.start.row - source.start.row;
                        rect.end.row = target.row + rect.end.row - source.start.row;
                        rect.start.col = target.col + rect.start.col - source.start.col;
                        rect.end.col = target.col + rect.end.col - source.start.col;
                        if let Some(to_tab) = to_tab
                            && let Some((id, _)) =
                                tabs.iter().find(|(_, n)| n.eq_ignore_ascii_case(to_tab))
                        {
                            thread.anchor.tab.clone_from(id);
                        }
                    }
                    true
                }
                _ => true,
            };
            if alive {
                thread.anchor.range = Range::Cells(rect).to_string();
            } else {
                thread.orphaned = true;
            }
        }
        if let Op::RenameTab { tab, name } = op
            && let Some((_, old)) = tabs.iter_mut().find(|(_, n)| n.eq_ignore_ascii_case(tab))
        {
            old.clone_from(name);
        }
    }
}

/// Recheck deletion and proposal guards after a reload or history restore.
pub fn reconcile(threads: &mut [Thread], sheet: &Sheet) {
    let revision = sheet.revision();
    for thread in threads {
        if !sheet.tabs.iter().any(|t| t.id == thread.anchor.tab) {
            thread.orphaned = true;
        }
        for edit in thread.messages.iter_mut().flat_map(|m| &mut m.edits) {
            if matches!(edit.status, EditStatus::Pending | EditStatus::Outdated) {
                edit.status = if edit.revision == revision {
                    EditStatus::Pending
                } else {
                    EditStatus::Outdated
                };
            }
        }
    }
}
