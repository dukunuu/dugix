//! Bounded parsing, located validation, and deterministic stable tab ids.
use super::{
    Sheet,
    address::{Address, MAX_COLS, MAX_ROWS, Range, Rect, column_index},
    model::Cell,
};
use crate::deck::CheckError;
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_SOURCE: usize = 32 * 1024 * 1024;
pub const MAX_CELLS: usize = 500_000;

fn issue(errors: &mut Vec<CheckError>, path: impl Into<String>, message: impl Into<String>) {
    errors.push(CheckError {
        path: path.into(),
        message: message.into(),
    });
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= 31
        && !name.contains(['[', ']', ':', '*', '?', '/', '\\'])
}

pub(crate) fn merge_range(text: &str) -> anyhow::Result<Rect> {
    let Range::Cells(rect) = text.parse()? else {
        anyhow::bail!("merge must be a cell rectangle");
    };
    anyhow::ensure!(text.contains(':'), "merge must look like A1:B2");
    Ok(rect)
}

impl Sheet {
    /// Parse and validate a bounded source file, assigning missing/duplicate tab
    /// ids and removing empty, unformatted cells. Errors locate tabs by name.
    #[allow(clippy::too_many_lines)]
    pub fn check(source: &str) -> Result<Self, Vec<CheckError>> {
        let fail = |message: String| {
            vec![CheckError {
                path: "$".into(),
                message,
            }]
        };
        if source.len() > MAX_SOURCE {
            return Err(fail("sheet exceeds 32 MiB".into()));
        }
        let mut sheet: Self = match serde_json::from_str(source) {
            Ok(sheet) => sheet,
            Err(error) => {
                // Locate serde errors inside a cell instead of reporting only the root.
                let raw: Value = serde_json::from_str(source).map_err(|e| fail(e.to_string()))?;
                for tab in raw["tabs"].as_array().into_iter().flatten() {
                    for (key, cell) in tab["cells"].as_object().into_iter().flatten() {
                        if let Err(e) = serde_json::from_value::<Cell>(cell.clone()) {
                            return Err(vec![CheckError {
                                path: format!("$.tabs[{:?}].cells[{key:?}]", tab["name"]),
                                message: e.to_string(),
                            }]);
                        }
                    }
                }
                return Err(match serde_json::from_value::<Self>(raw) {
                    Err(e) => fail(e.to_string()),
                    Ok(_) => fail(error.to_string()),
                });
            }
        };
        let mut errors = crate::doc::validate::check_theme(&sheet.theme);
        if sheet.version != 1 {
            issue(&mut errors, "$.version", "expected sheet version 1");
        }
        if !(1..=100).contains(&sheet.tabs.len()) {
            issue(&mut errors, "$.tabs", "expected 1 to 100 tabs");
        }
        let count: usize = sheet.tabs.iter().map(|t| t.cells.len()).sum();
        if count > MAX_CELLS {
            issue(&mut errors, "$.tabs", "at most 500000 stored cells");
        }
        let reserved: BTreeSet<_> = sheet.tabs.iter().map(|t| t.id.clone()).collect();
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        let tab_names: BTreeSet<_> = sheet.tabs.iter().map(|t| t.name.to_lowercase()).collect();
        let mut chart_ids = BTreeSet::new();
        for (index, tab) in sheet.tabs.iter_mut().enumerate() {
            let path = format!("$.tabs[{:?}]", tab.name);
            if tab.id.is_empty() || ids.contains(&tab.id) {
                let stem = format!("t{}", index + 1);
                tab.id.clone_from(&stem);
                let mut suffix = 1;
                while reserved.contains(&tab.id) || ids.contains(&tab.id) {
                    tab.id = format!("{stem}_{suffix}");
                    suffix += 1;
                }
            }
            ids.insert(tab.id.clone());
            if !valid_name(&tab.name) {
                issue(
                    &mut errors,
                    format!("{path}.name"),
                    "invalid Excel tab name",
                );
            }
            if !names.insert(tab.name.to_lowercase()) {
                issue(&mut errors, format!("{path}.name"), "duplicate tab name");
            }
            for (key, cell) in &tab.cells {
                let path = format!("{path}.cells[{key:?}]");
                if key.parse::<Address>().is_err() {
                    issue(&mut errors, &path, "invalid uppercase A1 cell address");
                }
                if cell.input.chars().count() > 32_767 {
                    issue(&mut errors, &path, "input exceeds 32767 characters");
                }
                for color in [&cell.fmt.fill, &cell.fmt.color].into_iter().flatten() {
                    let hex = color.len() == 7
                        && color.starts_with('#')
                        && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit);
                    if !hex && !sheet.theme.colors.contains_key(color) {
                        issue(&mut errors, &path, format!("invalid color {color:?}"));
                    }
                }
                if let Some(num) = &cell.fmt.num
                    && !super::engine::valid_number_format(num)
                {
                    issue(&mut errors, &path, format!("invalid number format {num:?}"));
                }
            }
            tab.cells.retain(|_, cell| cell != &Cell::default());
            for (col, width) in &tab.cols {
                if column_index(col).is_err() || !width.is_finite() || *width < 0.0 {
                    issue(
                        &mut errors,
                        format!("{path}.cols[{col:?}]"),
                        "invalid column width",
                    );
                }
            }
            for (row, height) in &tab.rows {
                if !(1..=MAX_ROWS).contains(row) || !height.is_finite() || *height < 0.0 {
                    issue(
                        &mut errors,
                        format!("{path}.rows[{row}]"),
                        "invalid row height",
                    );
                }
            }
            if tab.frozen.rows >= MAX_ROWS || tab.frozen.cols >= MAX_COLS {
                issue(
                    &mut errors,
                    format!("{path}.frozen"),
                    "frozen panes exceed grid",
                );
            }
            for chart in &tab.charts {
                let path = format!("{path}.charts[{:?}]", chart.id);
                if chart.id.is_empty() || !chart_ids.insert(chart.id.clone()) {
                    issue(&mut errors, &path, "empty or duplicate chart id");
                }
                if chart.data != "#REF!"
                    && !chart
                        .data
                        .parse::<super::address::TabRange>()
                        .is_ok_and(|r| {
                            r.tab
                                .as_ref()
                                .is_none_or(|name| tab_names.contains(&name.to_lowercase()))
                        })
                {
                    issue(&mut errors, &path, "invalid chart data range or tab prefix");
                }
                if chart.at.parse::<Address>().is_err() {
                    issue(&mut errors, &path, "invalid chart placement");
                }
                if !chart.w.is_finite()
                    || !chart.h.is_finite()
                    || !(160.0..=2000.0).contains(&chart.w)
                    || !(120.0..=2000.0).contains(&chart.h)
                    || !chart.dx.is_finite()
                    || chart.dx < 0.0
                    || !chart.dy.is_finite()
                    || chart.dy < 0.0
                {
                    issue(&mut errors, &path, "invalid chart size or offset");
                }
                if chart.stacked
                    && !matches!(
                        chart.kind,
                        super::ChartKind::Column | super::ChartKind::Bar | super::ChartKind::Area
                    )
                {
                    issue(&mut errors, &path, "stacked requires column, bar or area");
                }
                if chart.y_min.is_some_and(|v| !v.is_finite())
                    || chart.y_max.is_some_and(|v| !v.is_finite())
                    || chart
                        .y_min
                        .zip(chart.y_max)
                        .is_some_and(|(min, max)| min >= max)
                {
                    issue(&mut errors, &path, "invalid value axis bounds");
                }
                if chart.kind == super::ChartKind::Pie
                    && (chart.x_title.is_some()
                        || chart.y_title.is_some()
                        || chart.y_min.is_some()
                        || chart.y_max.is_some())
                {
                    issue(&mut errors, &path, "pie has no axes");
                }
            }
            let mut merges: Vec<Rect> = Vec::new();
            for text in &tab.merges {
                let path = format!("{path}.merges[{text:?}]");
                match merge_range(text) {
                    Ok(rect) => {
                        if merges.iter().any(|other| rect.overlaps(*other)) {
                            issue(&mut errors, &path, "overlapping merge");
                        }
                        merges.push(rect);
                    }
                    Err(e) => issue(&mut errors, path, e.to_string()),
                }
            }
        }
        if errors.is_empty() {
            Ok(sheet)
        } else {
            Err(errors)
        }
    }
}

pub(crate) fn checked(sheet: &Sheet) -> anyhow::Result<Sheet> {
    Sheet::check(&serde_json::to_string(sheet)?).map_err(|errors| {
        anyhow::anyhow!(
            "{}",
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        )
    })
}
