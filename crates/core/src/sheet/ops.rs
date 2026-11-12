//! Ordered, atomic sheet edits. Tabs are addressed by their displayed names.
use super::{
    Cell, Format, Sheet, Tab,
    address::{Address, MAX_COLS, MAX_ROWS, Range, Rect, column_name},
};
use crate::design::{Resolver, resolve};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which part of a cell to clear.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearWhat {
    Inputs,
    Formats,
    All,
}

/// The sheet edit language, shared by future tool and studio callers.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    SetCells {
        tab: String,
        at: String,
        values: Vec<Vec<String>>,
    },
    Clear {
        tab: String,
        range: String,
        what: ClearWhat,
    },
    Format {
        tab: String,
        range: String,
        patch: Value,
    },
    InsertRows {
        tab: String,
        at: u32,
        count: u32,
    },
    DeleteRows {
        tab: String,
        at: u32,
        count: u32,
    },
    InsertCols {
        tab: String,
        at: String,
        count: u32,
    },
    DeleteCols {
        tab: String,
        at: String,
        count: u32,
    },
    Sort {
        tab: String,
        range: String,
        by: String,
        desc: bool,
        header: bool,
    },
    SetColWidth {
        tab: String,
        cols: String,
        width: f64,
    },
    SetRowHeight {
        tab: String,
        rows: String,
        height: f64,
    },
    Freeze {
        tab: String,
        rows: u32,
        cols: u32,
    },
    Merge {
        tab: String,
        range: String,
    },
    Unmerge {
        tab: String,
        range: String,
    },
    AddTab {
        name: String,
        after: Option<String>,
    },
    RenameTab {
        tab: String,
        name: String,
    },
    DeleteTab {
        tab: String,
    },
    MoveTab {
        tab: String,
        after: Option<String>,
    },
    SetTitle {
        title: String,
    },
    ApplyDesign {
        design: Option<String>,
    },
}

/// Apply all edits on a copy, using the built-in design resolver.
pub fn apply(base: &Sheet, ops: &[Op]) -> Result<Sheet> {
    apply_with_resolver(base, ops, None)
}

/// Apply atomically with a space's design resolver. Every intermediate result
/// is validated; failures never mutate the caller's sheet.
pub fn apply_with_resolver(
    base: &Sheet,
    ops: &[Op],
    resolver: Option<&Resolver<'_>>,
) -> Result<Sheet> {
    let mut working = super::validate::checked(base)?;
    for (index, op) in ops.iter().enumerate() {
        step(&mut working, op, resolver).map_err(|e| anyhow!("ops[{index}]: {e}"))?;
        working = super::validate::checked(&working).map_err(|e| anyhow!("ops[{index}]: {e}"))?;
    }
    Ok(working)
}

fn tab_index(sheet: &Sheet, name: &str) -> Result<usize> {
    sheet
        .tabs
        .iter()
        .position(|t| t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow!("no tab {name:?}"))
}

fn tab_mut<'a>(sheet: &'a mut Sheet, name: &str) -> Result<&'a mut Tab> {
    let index = tab_index(sheet, name)?;
    Ok(&mut sheet.tabs[index])
}

fn set_cells(tab: &mut Tab, at: &str, values: &[Vec<String>]) -> Result<()> {
    let at: Address = at.parse()?;
    let width = values.first().map_or(0, Vec::len);
    ensure!(
        width > 0 && values.iter().all(|row| row.len() == width),
        "values must be a nonempty rectangle"
    );
    ensure!(
        values.len() <= (MAX_ROWS - at.row + 1) as usize
            && width <= (MAX_COLS - at.col + 1) as usize,
        "block extends beyond Excel limits"
    );
    ensure!(
        values.len().saturating_mul(width) <= super::validate::MAX_CELLS,
        "block exceeds stored cell limit"
    );
    for (r, row) in values.iter().enumerate() {
        for (c, input) in row.iter().enumerate() {
            let key = Address {
                row: at.row + u32::try_from(r)?,
                col: at.col + u32::try_from(c)?,
            }
            .to_string();
            if input.is_empty() && !tab.cells.contains_key(&key) {
                continue;
            }
            tab.cells.entry(key).or_default().input.clone_from(input);
        }
    }
    Ok(())
}

fn format_cells(tab: &mut Tab, rect: Rect, patch: &Value) -> Result<()> {
    ensure!(patch.is_object(), "format patch must be an object");
    // Validate the patch even when it only clears default fields.
    let merged = |fmt: &Format| -> Result<Format> {
        let mut value = serde_json::to_value(fmt)?;
        crate::doc::ops::merge_patch(&mut value, patch);
        Ok(serde_json::from_value(value)?)
    };
    let default = merged(&Format::default())?;
    if default == Format::default() {
        for (key, cell) in &mut tab.cells {
            if rect.contains(key.parse()?) {
                cell.fmt = merged(&cell.fmt)?;
            }
        }
    } else {
        ensure!(
            rect.cell_count() <= super::validate::MAX_CELLS as u64,
            "format range exceeds stored cell limit"
        );
        for row in rect.start.row..=rect.end.row {
            for col in rect.start.col..=rect.end.col {
                let cell = tab
                    .cells
                    .entry(Address { row, col }.to_string())
                    .or_default();
                cell.fmt = merged(&cell.fmt)?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn step(sheet: &mut Sheet, op: &Op, resolver: Option<&Resolver<'_>>) -> Result<()> {
    match op {
        Op::SetCells { tab, at, values } => set_cells(tab_mut(sheet, tab)?, at, values)?,
        Op::Clear { tab, range, what } => {
            let rect = range.parse::<Range>()?.bounds();
            for (key, cell) in &mut tab_mut(sheet, tab)?.cells {
                if !rect.contains(key.parse()?) {
                    continue;
                }
                match what {
                    ClearWhat::Inputs => cell.input.clear(),
                    ClearWhat::Formats => cell.fmt = Format::default(),
                    ClearWhat::All => *cell = Cell::default(),
                }
            }
        }
        Op::Format { tab, range, patch } => {
            format_cells(
                tab_mut(sheet, tab)?,
                range.parse::<Range>()?.bounds(),
                patch,
            )?;
        }
        Op::InsertRows { .. }
        | Op::DeleteRows { .. }
        | Op::InsertCols { .. }
        | Op::DeleteCols { .. }
        | Op::Sort { .. }
        | Op::RenameTab { .. }
        | Op::DeleteTab { .. }
        | Op::MoveTab { .. } => *sheet = super::engine::transform(sheet, op)?,
        Op::SetColWidth { tab, cols, width } => {
            ensure!(
                width.is_finite() && *width >= 0.0,
                "width must be finite and nonnegative"
            );
            let Range::Columns(first, last) = cols.parse()? else {
                anyhow::bail!("cols must look like A:C");
            };
            let tab = tab_mut(sheet, tab)?;
            for col in first..=last {
                tab.cols.insert(column_name(col)?, *width);
            }
        }
        Op::SetRowHeight { tab, rows, height } => {
            ensure!(
                height.is_finite() && *height >= 0.0,
                "height must be finite and nonnegative"
            );
            let Range::Rows(first, last) = rows.parse()? else {
                anyhow::bail!("rows must look like 1:3");
            };
            let tab = tab_mut(sheet, tab)?;
            for row in first..=last {
                tab.rows.insert(row, *height);
            }
        }
        Op::Freeze { tab, rows, cols } => {
            tab_mut(sheet, tab)?.frozen = super::Frozen {
                rows: *rows,
                cols: *cols,
            };
        }
        Op::Merge { tab, range } => {
            let rect = super::validate::merge_range(range)?;
            tab_mut(sheet, tab)?.merges.push(rect.to_string());
        }
        Op::Unmerge { tab, range } => {
            let rect = range.parse::<Range>()?.bounds();
            tab_mut(sheet, tab)?.merges.retain(|text| {
                !rect.overlaps(super::validate::merge_range(text).expect("validated merge"))
            });
        }
        Op::AddTab { name, after } => {
            let at = after.as_ref().map_or(Ok(sheet.tabs.len()), |name| {
                tab_index(sheet, name).map(|n| n + 1)
            })?;
            sheet.tabs.insert(at, Tab::new(name));
        }
        Op::SetTitle { title } => sheet.title.clone_from(title),
        Op::ApplyDesign { design } => {
            let slug = design
                .as_deref()
                .or(sheet.design.as_deref())
                .unwrap_or("nexus")
                .to_string();
            let design = resolve(&slug, resolver)?;
            sheet.theme = crate::doc::Theme::from_design(&design);
            sheet.design = Some(slug);
            sheet.design_rev = Some(design.revision());
        }
    }
    Ok(())
}
