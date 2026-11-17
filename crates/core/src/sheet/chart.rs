//! Range-backed charts: bounded computed data and shared structural anchors.
use super::{
    Chart, ChartKind, Headers, Op, SeriesIn, Sheet, Tab, ValueKind, Workbook,
    address::{Address, Range, TabRange},
};
use anyhow::{Context as _, Result, ensure};
use serde_json::Value;

/// A named numeric series; missing values are gaps, not zeroes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    pub name: String,
    pub values: Vec<Option<f64>>,
}

/// Computed chart data, independent of drawing and of the calculation engine.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    pub labels: Vec<String>,
    pub series: Vec<Series>,
    pub x_values: Vec<Option<f64>>,
    pub number_format: Option<String>,
    pub capped: bool,
    pub dates: bool,
}

/// Resolve a range against its containing tab. Deleted sources have no range.
#[must_use]
pub fn source<'a>(sheet: &'a Sheet, owner: &'a Tab, text: &str) -> Option<(&'a Tab, Range)> {
    let reference: TabRange = text.parse().ok()?;
    let tab = reference.tab.as_ref().map_or(Some(owner), |name| {
        sheet
            .tabs
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    })?;
    Some((tab, reference.range))
}

fn number(kind: &ValueKind) -> Option<f64> {
    match kind {
        ValueKind::Number(n) if n.is_finite() => Some(*n),
        _ => None,
    }
}

/// Extract at most 2,000 categories and 24 series from calculated values.
#[must_use]
pub fn extract(sheet: &Sheet, workbook: &Workbook, owner: &Tab, chart: &Chart) -> Data {
    let Some((tab, range)) = source(sheet, owner, &chart.data) else {
        return Data::default();
    };
    let mut rect = range.bounds();
    // Whole rows/columns stop at the used cells, rather than producing empty millions.
    if !matches!(range, Range::Cells(_)) {
        let Some(used) = tab.used_range() else {
            return Data::default();
        };
        rect.end.row = rect.end.row.min(used.end.row);
        rect.end.col = rect.end.col.min(used.end.col);
        if rect.start.row > rect.end.row || rect.start.col > rect.end.col {
            return Data::default();
        }
    }
    let rows = chart.series_in == SeriesIn::Rows;
    let (length, width) = if rows {
        (
            rect.end.col - rect.start.col + 1,
            rect.end.row - rect.start.row + 1,
        )
    } else {
        (
            rect.end.row - rect.start.row + 1,
            rect.end.col - rect.start.col + 1,
        )
    };
    let address = |point, series| {
        Address {
            row: rect.start.row + if rows { series } else { point },
            col: rect.start.col + if rows { point } else { series },
        }
        .to_string()
    };
    let value = |point, series| workbook.value(&tab.name, &address(point, series));
    let first_series = u32::from(width >= 2);
    let header = match chart.headers {
        Headers::Yes => true,
        Headers::No => false,
        Headers::Auto => (first_series..width.min(first_series + 24)).any(|s| {
            matches!(value(0, s).kind, ValueKind::Text(_))
                && (1..length.min(2001)).any(|p| number(&value(p, s).kind).is_some())
        }),
    };
    let start = u32::from(header);
    let end = length.min(start + 2000);
    let mut data = Data {
        capped: length > end || width - first_series > 24,
        ..Data::default()
    };
    for p in start..end {
        data.labels.push(if first_series == 1 {
            value(p, 0).display
        } else {
            (p - start + 1).to_string()
        });
        data.x_values.push(if first_series == 1 {
            number(&value(p, 0).kind)
        } else {
            Some(f64::from(p - start + 1))
        });
        if first_series == 1
            && let Some(fmt) = tab
                .cells
                .get(&address(p, 0))
                .and_then(|c| c.fmt.num.as_deref())
        {
            data.dates |= super::engine::date_format(fmt);
        }
    }
    for s in first_series..width.min(first_series + 24) {
        let mut series = Series {
            name: if header {
                value(0, s).display
            } else {
                format!("Series {}", s - first_series + 1)
            },
            values: Vec::new(),
        };
        for p in start..end {
            let n = number(&value(p, s).kind);
            if n.is_some() && data.number_format.is_none() {
                // General is a format too: do not pick a later cell's override.
                data.number_format = Some(
                    tab.cells
                        .get(&address(p, s))
                        .and_then(|c| c.fmt.num.clone())
                        .unwrap_or_else(|| "General".into()),
                );
            }
            series.values.push(n);
        }
        data.series.push(series);
    }
    data
}

pub(crate) fn add(sheet: &mut Sheet, op: &Op) -> Result<()> {
    let mut raw = serde_json::to_value(op)?;
    let object = raw.as_object_mut().expect("op object");
    let name = object
        .remove("tab")
        .and_then(|v| v.as_str().map(str::to_string))
        .context("tab")?;
    object.remove("op");
    object.retain(|_, v| !v.is_null());
    let automatic_kind = !object.contains_key("kind");
    let automatic_at = !object.contains_key("at");
    let mut chart: Chart = serde_json::from_value(raw)?;
    let index = sheet
        .tabs
        .iter()
        .position(|t| t.name.eq_ignore_ascii_case(&name))
        .context("unknown tab")?;
    let owner = &sheet.tabs[index];
    let (tab, range) =
        source(sheet, owner, &chart.data).context("invalid chart data range or tab")?;
    if automatic_at {
        let rect = range.bounds();
        let right = if matches!(range, Range::Rows(_, _)) {
            tab.used_range().map_or(1, |used| used.end.col)
        } else {
            tab.used_range()
                .map_or(rect.end.col, |used| used.end.col.max(rect.end.col))
        };
        chart.at = Address {
            row: rect.start.row,
            col: (right + 2).min(super::address::MAX_COLS),
        }
        .to_string();
    }
    if automatic_kind {
        let data = extract(sheet, &Workbook::load(sheet)?, owner, &chart);
        chart.kind = if data.dates || data.labels.len() > 12 {
            ChartKind::Line
        } else {
            ChartKind::Column
        };
    }
    for n in 1.. {
        let id = format!("c{n}");
        if !sheet
            .tabs
            .iter()
            .flat_map(|t| &t.charts)
            .any(|c| c.id == id)
        {
            chart.id = id;
            break;
        }
    }
    sheet.tabs[index].charts.push(chart);
    Ok(())
}

pub(crate) fn edit(tab: &mut Tab, id: &str, patch: &Value) -> Result<()> {
    ensure!(patch.is_object(), "chart patch must be an object");
    ensure!(patch.get("id").is_none(), "chart ids are stable");
    let chart = tab
        .charts
        .iter_mut()
        .find(|c| c.id == id)
        .context("unknown chart")?;
    let mut raw = serde_json::to_value(&*chart)?;
    crate::doc::ops::merge_patch(&mut raw, patch);
    *chart = serde_json::from_value(raw)?;
    Ok(())
}

fn qualified(tab: &str, range: Range) -> String {
    format!("'{}'!{range}", tab.replace('\'', "''"))
}

/// Follow a validated structural operation, using exactly the comment anchor rules.
pub(crate) fn follow(sheet: &mut Sheet, before: &Sheet, op: &Op) {
    if !matches!(
        op,
        Op::InsertRows { .. }
            | Op::DeleteRows { .. }
            | Op::InsertCols { .. }
            | Op::DeleteCols { .. }
            | Op::RenameTab { .. }
            | Op::DeleteTab { .. }
            | Op::Copy { cut: true, .. }
    ) {
        return;
    }
    for tab in &mut sheet.tabs {
        let old = before
            .tabs
            .iter()
            .find(|t| t.id == tab.id)
            .expect("existing tab");
        for chart in &mut tab.charts {
            if let Some((source, range)) = source(before, old, &chart.data) {
                let mut rect = range.bounds();
                let orthogonal = matches!(
                    (range, op),
                    (
                        Range::Columns(_, _),
                        Op::InsertRows { .. } | Op::DeleteRows { .. }
                    ) | (
                        Range::Rows(_, _),
                        Op::InsertCols { .. } | Op::DeleteCols { .. }
                    )
                );
                let (alive, moved) = if orthogonal {
                    (true, None)
                } else {
                    super::feedback::follow(&mut rect, &source.name, op)
                };
                if alive {
                    let range = match range {
                        Range::Columns(_, _) => Range::Columns(rect.start.col, rect.end.col),
                        Range::Rows(_, _) => Range::Rows(rect.start.row, rect.end.row),
                        Range::Cells(_) => Range::Cells(rect),
                    };
                    let mut name = moved.unwrap_or(&source.name);
                    if let Op::RenameTab { tab, name: next } = op
                        && name.eq_ignore_ascii_case(tab)
                    {
                        name = next;
                    }
                    let prefixed = chart
                        .data
                        .parse::<TabRange>()
                        .is_ok_and(|r| r.tab.is_some());
                    chart.data = if prefixed || !name.eq_ignore_ascii_case(&tab.name) {
                        qualified(name, range)
                    } else {
                        range.to_string()
                    };
                } else {
                    chart.data = "#REF!".into();
                }
            }
            // Placement follows only insert/delete, never cut or sort. A removed anchor
            // lands on the first surviving cell at the deletion boundary.
            if matches!(
                op,
                Op::InsertRows { .. }
                    | Op::DeleteRows { .. }
                    | Op::InsertCols { .. }
                    | Op::DeleteCols { .. }
            ) {
                let at: Address = chart.at.parse().expect("validated chart");
                let mut rect = super::address::Rect { start: at, end: at };
                let (alive, _) = super::feedback::follow(&mut rect, &old.name, op);
                rect.start.row = rect.start.row.min(super::address::MAX_ROWS);
                rect.start.col = rect.start.col.min(super::address::MAX_COLS);
                // `follow` leaves a wholly deleted point unchanged.
                if !alive {
                    match op {
                        Op::DeleteRows { at, .. } => rect.start.row = *at,
                        Op::DeleteCols { at, .. } => {
                            rect.start.col =
                                super::address::column_index(at).expect("validated op");
                        }
                        _ => {}
                    }
                }
                chart.at = rect.start.to_string();
            }
        }
    }
}

/// Loss notice shared by the tool and the studio's explicit sheet export.
#[must_use]
pub fn export_notice(sheet: &Sheet, format: &str) -> Option<String> {
    (format == "xlsx" && sheet.tabs.iter().any(|t| !t.charts.is_empty()))
        .then(|| "charts aren't included in XLSX exports".into())
}
