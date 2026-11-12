//! The sole `IronCalc` boundary: evaluation, reference-aware edits and XLSX.
use super::{
    Cell, Format, Frozen, Sheet, Tab,
    address::{Address, MAX_COLS, MAX_ROWS, Range, Rect, column_index, column_name},
    model::{Align, VAlign, Wrap},
    ops::Op,
};
use anyhow::{Result, anyhow, bail, ensure};
use ironcalc::base::{
    ClipboardData, Model, UserModel,
    cell::CellValue,
    expressions::{
        parser::{Node as Formula, new_parser_english, stringify::to_english_string},
        types::{Area, CellReferenceRC},
    },
    formatter::parser::Parser,
    types::{
        Alignment, CellType, Color, HorizontalAlignment, MergedCell, Style, VerticalAlignment,
    },
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
    io::Cursor,
};

/// Evaluated cell type, independent of the spreadsheet engine.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueKind {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(String),
}

/// Both the typed result and its locale/number-format-aware display.
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    pub kind: ValueKind,
    pub display: String,
}

/// A recalculated workbook. The source sidecar preserves exact user-entered
/// literals (`IronCalc` normalizes numbers) and stable tab ids, not computed data.
pub struct Workbook {
    model: Model<'static>,
    source: Vec<Tab>,
}

fn ic<T>(result: Result<T, String>) -> Result<T> {
    result.map_err(anyhow::Error::msg)
}

fn int(n: u32) -> i32 {
    i32::try_from(n).expect("validated Excel coordinate")
}

fn uint(n: i32) -> u32 {
    u32::try_from(n).expect("IronCalc grid coordinate")
}

fn idx(n: usize) -> u32 {
    u32::try_from(n).expect("at most 100 tabs")
}

pub(crate) fn valid_number_format(code: &str) -> bool {
    if code.is_empty() || code.chars().count() > 255 {
        return false;
    }
    let mut parser = Parser::new(code);
    parser.parse();
    !parser
        .parts
        .iter()
        .any(ironcalc::base::formatter::parser::ParsePart::is_error)
}

fn style(fmt: &Format) -> Style {
    let mut style = Style::default();
    if let Some(num) = &fmt.num {
        style.num_fmt.clone_from(num);
    }
    style.font.b = fmt.bold;
    style.font.i = fmt.italic;
    style.font.u = fmt.underline;
    // Token names remain opaque RGB strings inside the model. Only XLSX needs
    // actual hex; export resolves them against the sheet's copied theme.
    style.fill.color = fmt.fill.clone().map_or(Color::None, Color::Rgb);
    style.font.color = fmt.color.clone().map_or(Color::None, Color::Rgb);
    // XLSX has no "clip": wrap and clip both export as no wrap, and import as absent.
    style.alignment =
        (fmt.align.is_some() || fmt.wrap.is_some() || fmt.valign.is_some()).then(|| Alignment {
            horizontal: match fmt.align {
                Some(Align::Left) => HorizontalAlignment::Left,
                Some(Align::Center) => HorizontalAlignment::Center,
                Some(Align::Right) => HorizontalAlignment::Right,
                None => HorizontalAlignment::General,
            },
            vertical: match fmt.valign {
                Some(VAlign::Top) => VerticalAlignment::Top,
                Some(VAlign::Middle) => VerticalAlignment::Center,
                Some(VAlign::Bottom) | None => VerticalAlignment::Bottom,
            },
            wrap_text: fmt.wrap == Some(Wrap::Wrap),
        });
    style
}

fn format(style: &Style, theme: &ironcalc::base::types::Theme) -> Format {
    let color = |value: &Color| (!value.is_none()).then(|| value.to_rgb(theme));
    Format {
        num: (!style.num_fmt.eq_ignore_ascii_case("general")).then(|| style.num_fmt.clone()),
        bold: style.font.b,
        italic: style.font.i,
        underline: style.font.u,
        align: style.alignment.as_ref().and_then(|a| match a.horizontal {
            HorizontalAlignment::Left => Some(Align::Left),
            HorizontalAlignment::Center => Some(Align::Center),
            HorizontalAlignment::Right => Some(Align::Right),
            _ => None,
        }),
        wrap: style
            .alignment
            .as_ref()
            .and_then(|a| a.wrap_text.then_some(Wrap::Wrap)),
        // Bottom is the default, so it is not written back.
        valign: style.alignment.as_ref().and_then(|a| match a.vertical {
            VerticalAlignment::Top => Some(VAlign::Top),
            VerticalAlignment::Center => Some(VAlign::Middle),
            _ => None,
        }),
        fill: color(&style.fill.color),
        color: color(&style.font.color),
    }
}

fn area(sheet: u32, rect: Rect) -> Area {
    Area {
        sheet,
        row: int(rect.start.row),
        column: int(rect.start.col),
        width: int(rect.end.col - rect.start.col + 1),
        height: int(rect.end.row - rect.start.row + 1),
    }
}

impl Workbook {
    /// Build all named tabs before setting inputs, then calculate once.
    pub fn load(sheet: &Sheet) -> Result<Self> {
        let sheet = super::validate::checked(sheet)?;
        #[cfg(test)]
        ironcalc::base::mock_time::set_mock_time(1_700_000_000_000);
        let mut model = ic(Model::new_empty("Sheet", "en", "UTC", "en"))?;
        model.workbook.name.clone_from(&sheet.title);
        // Use a fresh temporary name to avoid collisions with another tab's name.
        let temporary = (0..=sheet.tabs.len())
            .map(|n| format!("_nexus_{n}"))
            .find(|name| !sheet.tabs.iter().any(|t| t.name.eq_ignore_ascii_case(name)))
            .expect("finite tab names");
        ic(model.rename_sheet_by_index(0, &temporary))?;
        for tab in &sheet.tabs {
            ic(model.add_sheet(&tab.name))?;
        }
        ic(model.delete_sheet(0))?;
        for (index, tab) in sheet.tabs.iter().enumerate() {
            let index = idx(index);
            for (key, cell) in &tab.cells {
                let a: Address = key.parse()?;
                ic(model.set_user_input(index, int(a.row), int(a.col), cell.input.clone()))?;
                let mut fmt = style(&cell.fmt);
                // Keep IronCalc's quote-prefix interpretation of literal text.
                fmt.quote_prefix =
                    ic(model.get_style_for_cell(index, int(a.row), int(a.col)))?.quote_prefix;
                ic(model.set_cell_style(index, int(a.row), int(a.col), &fmt))?;
            }
            for (col, width) in &tab.cols {
                ic(model.set_column_width(index, int(column_index(col)?), *width))?;
            }
            for (row, height) in &tab.rows {
                ic(model.set_row_height(index, int(*row), *height))?;
            }
            ic(model.set_frozen_rows(index, int(tab.frozen.rows)))?;
            ic(model.set_frozen_columns(index, int(tab.frozen.cols)))?;
            // Loading does not discard covered inputs or restyle a merge.
            model.workbook.worksheets[index as usize].merged_cells = tab
                .merges
                .iter()
                .map(|text| {
                    let rect = super::validate::merge_range(text)?;
                    let a = area(index, rect);
                    Ok(MergedCell {
                        row: a.row,
                        column: a.column,
                        width: a.width,
                        height: a.height,
                    })
                })
                .collect::<Result<_>>()?;
        }
        model.evaluate();
        Ok(Self {
            model,
            source: sheet.tabs,
        })
    }

    /// Read a computed value. Invalid tab names or addresses yield `#REF!`.
    #[must_use]
    pub fn value(&self, tab: &str, address: &str) -> Value {
        let result = (|| {
            let index = self.tab_index(tab)?;
            let a: Address = address.parse()?;
            self.value_at(index, a)
        })();
        result.unwrap_or_else(|_: anyhow::Error| Value {
            kind: ValueKind::Error("#REF!".into()),
            display: "#REF!".into(),
        })
    }

    /// Error values in the selected rectangles, including calculated spill cells.
    pub(crate) fn errors_in(&self, tab: &str, ranges: &[Rect]) -> Result<Vec<(Address, String)>> {
        let index = self.tab_index(tab)?;
        let mut errors = Vec::new();
        for cell in self.model.get_all_cells() {
            if cell.index != index {
                continue;
            }
            let address = Address {
                row: uint(cell.row),
                col: uint(cell.column),
            };
            if ranges.iter().any(|rect| rect.contains(address))
                && let ValueKind::Error(error) = self.value_at(index, address)?.kind
            {
                errors.push((address, error));
            }
        }
        Ok(errors)
    }

    fn tab_index(&self, name: &str) -> Result<u32> {
        self.source
            .iter()
            .position(|t| t.name.eq_ignore_ascii_case(name))
            .map(idx)
            .ok_or_else(|| anyhow!("no tab {name:?}"))
    }

    fn value_at(&self, index: u32, a: Address) -> Result<Value> {
        let display = ic(self
            .model
            .get_formatted_cell_value(index, int(a.row), int(a.col)))?;
        let kind = if ic(self.model.get_cell_type(index, int(a.row), int(a.col)))?
            == CellType::ErrorValue
        {
            ValueKind::Error(display.clone())
        } else {
            match ic(self
                .model
                .get_cell_value_by_index(index, int(a.row), int(a.col)))?
            {
                CellValue::None => ValueKind::Empty,
                CellValue::Number(n) => ValueKind::Number(n),
                CellValue::String(s) => ValueKind::Text(s),
                CellValue::Boolean(b) => ValueKind::Bool(b),
            }
        };
        Ok(Value { kind, display })
    }

    fn input(&self, index: u32, row: i32, col: i32) -> String {
        if let Some(formula) = self
            .model
            .get_cell_formula(index, row, col)
            .expect("existing tab")
        {
            return formula;
        }
        let key = Address {
            row: uint(row),
            col: uint(col),
        }
        .to_string();
        if let Some(cell) = self.source[index as usize].cells.get(&key) {
            return cell.input.clone();
        }
        // Imported numbers stay raw inputs even when formatted as dates.
        match self
            .model
            .get_cell_value_by_index(index, row, col)
            .expect("existing tab")
        {
            CellValue::None => String::new(),
            CellValue::Number(n) => n.to_string(),
            _ => self
                .model
                .get_localized_cell_content(index, row, col)
                .expect("existing cell"),
        }
    }

    /// Read rewritten inputs and engine-tracked formatting/geometry. Non-grid
    /// metadata (title, design and copied theme) comes from `base`.
    #[must_use]
    pub fn to_sheet(&self, base: &Sheet) -> Sheet {
        let mut sheet = base.clone();
        sheet.tabs.clone_from(&self.source);
        for (index, tab) in sheet.tabs.iter_mut().enumerate() {
            let ws = &self.model.workbook.worksheets[index];
            tab.name.clone_from(&ws.name);
            tab.cells.clear();
            tab.cols.clear();
            tab.rows.clear();
            for col in &ws.cols {
                for c in col.min..=col.max {
                    tab.cols.insert(
                        column_name(uint(c)).expect("engine column"),
                        ws.get_column_width(c).expect("engine width"),
                    );
                }
            }
            for row in &ws.rows {
                if row.custom_height {
                    tab.rows
                        .insert(uint(row.r), ws.row_height(row.r).expect("engine height"));
                }
            }
            tab.frozen = Frozen {
                rows: uint(ws.frozen_rows),
                cols: uint(ws.frozen_columns),
            };
            tab.merges = ws
                .merged_cells
                .iter()
                .map(|m| {
                    Rect {
                        start: Address {
                            row: uint(m.row),
                            col: uint(m.column),
                        },
                        end: Address {
                            row: uint(m.last_row()),
                            col: uint(m.last_column()),
                        },
                    }
                    .to_string()
                })
                .collect();
        }
        for a in self.model.get_all_cells() {
            let fmt = self
                .model
                .get_style_for_cell(a.index, a.row, a.column)
                .expect("engine cell style");
            let spill = self.model.workbook.worksheets[a.index as usize]
                .cell(a.row, a.column)
                .is_some_and(|c| matches!(*c, ironcalc::base::types::Cell::SpillCell { .. }));
            let cell = Cell {
                input: if spill {
                    String::new()
                } else {
                    self.input(a.index, a.row, a.column)
                },
                fmt: format(&fmt, &self.model.workbook.theme),
            };
            if cell != Cell::default() {
                sheet.tabs[a.index as usize].cells.insert(
                    Address {
                        row: uint(a.row),
                        col: uint(a.column),
                    }
                    .to_string(),
                    cell,
                );
            }
        }
        sheet
    }

    fn shift(&mut self, index: u32, at: u32, count: u32, rows: bool, delete: bool) -> Result<()> {
        let limit = if rows { MAX_ROWS } else { MAX_COLS };
        ensure!(
            count > 0 && at > 0 && at <= limit && count <= limit - at + 1,
            "invalid structural edit"
        );
        let map = |n: u32| {
            if n < at {
                Some(n)
            } else if delete {
                (n >= at + count).then(|| n - count)
            } else {
                n.checked_add(count).filter(|n| *n <= limit)
            }
        };
        let result = match (rows, delete) {
            (true, false) => self.model.insert_rows(index, int(at), int(count)),
            (true, true) => self.model.delete_rows(index, int(at), int(count)),
            (false, false) => self.model.insert_columns(index, int(at), int(count)),
            (false, true) => self.model.delete_columns(index, int(at), int(count)),
        };
        ic(result)?;
        let tab = &mut self.source[index as usize];
        tab.cells = std::mem::take(&mut tab.cells)
            .into_iter()
            .filter_map(|(key, cell)| {
                let mut a: Address = key.parse().expect("validated address");
                if rows {
                    a.row = map(a.row)?;
                } else {
                    a.col = map(a.col)?;
                }
                Some((a.to_string(), cell))
            })
            .collect();
        // IronCalc leaves frozen counts fixed during insert/delete. A frozen
        // boundary follows its original last frozen row/column, clipped at zero.
        let frozen = if rows {
            tab.frozen.rows
        } else {
            tab.frozen.cols
        };
        let next = if frozen < at {
            frozen
        } else if delete {
            frozen.saturating_sub(count.min(frozen - at + 1))
        } else {
            frozen.saturating_add(count).min(limit - 1)
        };
        if rows {
            tab.frozen.rows = next;
            ic(self.model.set_frozen_rows(index, int(next)))?;
        } else {
            tab.frozen.cols = next;
            ic(self.model.set_frozen_columns(index, int(next)))?;
        }
        Ok(())
    }

    fn edit(&mut self, op: &Op) -> Result<()> {
        match op {
            Op::InsertRows { tab, at, count } => {
                self.shift(self.tab_index(tab)?, *at, *count, true, false)?;
            }
            Op::DeleteRows { tab, at, count } => {
                self.shift(self.tab_index(tab)?, *at, *count, true, true)?;
            }
            Op::InsertCols { tab, at, count } => self.shift(
                self.tab_index(tab)?,
                column_index(at)?,
                *count,
                false,
                false,
            )?,
            Op::DeleteCols { tab, at, count } => {
                self.shift(self.tab_index(tab)?, column_index(at)?, *count, false, true)?;
            }
            Op::RenameTab { tab, name } => {
                let index = self.tab_index(tab)?;
                ic(self.model.rename_sheet_by_index(index, name))?;
                self.source[index as usize].name.clone_from(name);
            }
            Op::DeleteTab { tab } => {
                let index = self.tab_index(tab)?;
                ic(self.model.delete_sheet(index))?;
                self.source.remove(index as usize);
            }
            Op::MoveTab { tab, after } => {
                let index = self.tab_index(tab)?;
                ensure!(
                    !after.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(tab)),
                    "cannot move a tab after itself"
                );
                let mut next = after.as_ref().map_or(Ok(idx(self.source.len())), |a| {
                    self.tab_index(a).map(|n| n + 1)
                })?;
                if next > index {
                    next -= 1;
                }
                ic(self.model.move_sheet(index, next))?;
                let tab = self.source.remove(index as usize);
                self.source.insert(next as usize, tab);
            }
            Op::Copy {
                tab,
                from,
                to_tab,
                to,
                cut,
            } => {
                self.copy_cells(tab, from, to_tab.as_deref().unwrap_or(tab), to, *cut)?;
            }
            Op::Fill { tab, from, to } => self.fill_cells(tab, from, to)?,
            Op::Sort {
                tab,
                range,
                by,
                desc,
                header,
            } => self.sort(tab, range, by, *desc, *header)?,
            _ => bail!("not an engine edit"),
        }
        self.model.evaluate();
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn copy_cells(
        &mut self,
        tab: &str,
        from: &str,
        to_tab: &str,
        to: &str,
        cut: bool,
    ) -> Result<()> {
        let source = self.tab_index(tab)?;
        let target = self.tab_index(to_tab)?;
        let rect = from.parse::<Range>()?.bounds();
        let to: Address = to.parse()?;
        ensure!(
            rect.cell_count() <= super::validate::MAX_CELLS as u64,
            "copy exceeds stored cell limit"
        );
        ensure!(
            to.row + rect.end.row - rect.start.row <= MAX_ROWS
                && to.col + rect.end.col - rect.start.col <= MAX_COLS,
            "paste extends beyond Excel limits"
        );
        let mut data = BTreeMap::new();
        for row in rect.start.row..=rect.end.row {
            let row_data = copy_row(&self.model, source, rect, row)?;
            data.extend(row_data);
        }
        let mut user = UserModel::from_model(ic(Model::from_bytes(&self.model.to_bytes(), "en"))?);
        ic(user.set_selected_sheet(target))?;
        ic(user.set_selected_cell(int(to.row), int(to.col)))?;
        ic(user.paste_from_clipboard(
            source,
            (
                int(rect.start.row),
                int(rect.start.col),
                int(rect.end.row),
                int(rect.end.col),
            ),
            &data.into_iter().collect(),
            cut,
        ))?;
        let original = self.source[source as usize].cells.clone();
        if cut {
            self.source[source as usize]
                .cells
                .retain(|key, _| !rect.contains(key.parse().expect("validated address")));
        }
        for row in rect.start.row..=rect.end.row {
            for col in rect.start.col..=rect.end.col {
                let from = Address { row, col }.to_string();
                let key = Address {
                    row: to.row + row - rect.start.row,
                    col: to.col + col - rect.start.col,
                }
                .to_string();
                let cells = &mut self.source[target as usize].cells;
                if let Some(cell) = original.get(&from) {
                    cells.insert(key, cell.clone());
                } else {
                    cells.remove(&key);
                }
            }
        }
        let mut next = ic(Model::from_bytes(&user.to_bytes(), "en"))?;
        if cut && source != target {
            let moved = Cut {
                source,
                target,
                rect,
                to,
                names: self.source.iter().map(|t| t.name.clone()).collect(),
            };
            let mut parser = new_parser_english(moved.names.clone(), vec![], HashMap::default());
            let target_rect = Rect {
                start: to,
                end: Address {
                    row: to.row + rect.end.row - rect.start.row,
                    col: to.col + rect.end.col - rect.start.col,
                },
            };
            for cell in self.model.get_all_cells() {
                let a = Address {
                    row: uint(cell.row),
                    col: uint(cell.column),
                };
                let inside = cell.index == source && rect.contains(a);
                if !inside && cell.index == target && target_rect.contains(a) {
                    continue;
                }
                let Some(formula) =
                    ic(self
                        .model
                        .get_cell_formula(cell.index, cell.row, cell.column))?
                else {
                    continue;
                };
                let old = CellReferenceRC {
                    sheet: moved.names[cell.index as usize].clone(),
                    row: cell.row,
                    column: cell.column,
                };
                let (index, row, column) = if inside {
                    (
                        target,
                        int(to.row + a.row - rect.start.row),
                        int(to.col + a.col - rect.start.col),
                    )
                } else {
                    (cell.index, cell.row, cell.column)
                };
                let new = CellReferenceRC {
                    sheet: moved.names[index as usize].clone(),
                    row,
                    column,
                };
                let mut ast = parser.parse(formula.trim_start_matches('='), &old);
                moved.rewrite(&mut ast, &old, &new);
                ic(next.set_user_input(
                    index,
                    row,
                    column,
                    format!("={}", to_english_string(&ast, &new)),
                ))?;
            }
        }
        self.model = next;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn fill_cells(&mut self, tab: &str, from: &str, to: &str) -> Result<()> {
        let index = self.tab_index(tab)?;
        let from = from.parse::<Range>()?.bounds();
        let to = to.parse::<Range>()?.bounds();
        let down =
            from.start == to.start && from.end.col == to.end.col && to.end.row > from.end.row;
        let right =
            from.start == to.start && from.end.row == to.end.row && to.end.col > from.end.col;
        ensure!(down || right, "fill must extend the source down or right");
        ensure!(
            to.cell_count() <= super::validate::MAX_CELLS as u64,
            "fill exceeds stored cell limit"
        );
        let seeds = if down && from.start.col == from.end.col {
            (from.start.row..=from.end.row)
                .map(|row| Address {
                    row,
                    col: from.start.col,
                })
                .collect::<Vec<_>>()
        } else if right && from.start.row == from.end.row {
            (from.start.col..=from.end.col)
                .map(|col| Address {
                    row: from.start.row,
                    col,
                })
                .collect()
        } else {
            Vec::new()
        };
        let numbers: Option<Vec<f64>> = seeds
            .iter()
            .map(|a| {
                let cell = self.source[index as usize].cells.get(&a.to_string())?;
                cell.input.parse::<f64>().ok()
            })
            .collect();
        let series = numbers.as_ref().is_some_and(|n| {
            n.len() >= 2
                && n.windows(2)
                    .all(|w| (w[1] - w[0] - (n[1] - n[0])).abs() < 1e-10)
        });
        if series {
            let mut user =
                UserModel::from_model(ic(Model::from_bytes(&self.model.to_bytes(), "en"))?);
            if down {
                ic(user.auto_fill_rows(&area(index, from), int(to.end.row)))?;
            } else {
                ic(user.auto_fill_columns(&area(index, from), int(to.end.col)))?;
            }
            self.source[index as usize].cells.retain(|key, _| {
                let a = key.parse().expect("validated address");
                !to.contains(a) || from.contains(a)
            });
            self.model = ic(Model::from_bytes(&user.to_bytes(), "en"))?;
        } else {
            let mut user =
                UserModel::from_model(ic(Model::from_bytes(&self.model.to_bytes(), "en"))?);
            user.pause_evaluation();
            ic(user.set_selected_sheet(index))?;
            let original = self.source[index as usize].cells.clone();
            for row in to.start.row..=to.end.row {
                for col in to.start.col..=to.end.col {
                    let target = Address { row, col };
                    if from.contains(target) {
                        continue;
                    }
                    let source = Address {
                        row: from.start.row
                            + (row - from.start.row) % (from.end.row - from.start.row + 1),
                        col: from.start.col
                            + (col - from.start.col) % (from.end.col - from.start.col + 1),
                    };
                    let data = copy_row(
                        &self.model,
                        index,
                        Rect {
                            start: source,
                            end: source,
                        },
                        source.row,
                    )?;
                    ic(user.set_selected_cell(int(row), int(col)))?;
                    ic(user.paste_from_clipboard(
                        index,
                        (
                            int(source.row),
                            int(source.col),
                            int(source.row),
                            int(source.col),
                        ),
                        &data,
                        false,
                    ))?;
                    let cells = &mut self.source[index as usize].cells;
                    if let Some(cell) = original.get(&source.to_string()) {
                        cells.insert(target.to_string(), cell.clone());
                    } else {
                        cells.remove(&target.to_string());
                    }
                }
            }
            self.model = ic(Model::from_bytes(&user.to_bytes(), "en"))?;
        }
        Ok(())
    }

    fn sort(&mut self, tab: &str, range: &str, by: &str, desc: bool, header: bool) -> Result<()> {
        let index = self.tab_index(tab)?;
        let mut rect = range.parse::<Range>()?.bounds();
        // Whole-row/column ranges only need their used part: trailing empty
        // rows remain empty, and empties are always the final category.
        if let Some(used) = self.source[index as usize].used_range() {
            rect.end.row = rect.end.row.min(used.end.row);
        } else {
            return Ok(());
        }
        let col = column_index(by)?;
        ensure!(
            (rect.start.col..=rect.end.col).contains(&col),
            "sort column outside range"
        );
        rect.start.row += u32::from(header);
        if rect.start.row >= rect.end.row {
            return Ok(());
        }
        let mut order = (rect.start.row..=rect.end.row)
            .map(|row| {
                self.value_at(index, Address { row, col })
                    .map(|v| (row, v.kind))
            })
            .collect::<Result<Vec<_>>>()?;
        order.sort_by(|(_, a), (_, b)| compare(a, b, desc));
        if order
            .iter()
            .enumerate()
            .all(|(i, (row, _))| *row == rect.start.row + u32::try_from(i).expect("row offset"))
        {
            return Ok(());
        }
        // Snapshot before any paste so each formula is copied from its original
        // position, without rewriting references outside the sorted range.
        let rows = order
            .iter()
            .map(|(row, _)| copy_row(&self.model, index, rect, *row))
            .collect::<Result<Vec<_>>>()?;
        let mut user = UserModel::from_model(ic(Model::from_bytes(&self.model.to_bytes(), "en"))?);
        user.pause_evaluation();
        ic(user.set_selected_sheet(index))?;
        for (offset, ((source, _), data)) in order.iter().zip(&rows).enumerate() {
            let target = rect.start.row + u32::try_from(offset)?;
            ic(user.set_selected_cell(int(target), int(rect.start.col)))?;
            ic(user.paste_from_clipboard(
                index,
                (
                    int(*source),
                    int(rect.start.col),
                    int(*source),
                    int(rect.end.col),
                ),
                data,
                false,
            ))?;
        }
        self.reorder_source(index, rect, &order);
        self.model = ic(Model::from_bytes(&user.to_bytes(), "en"))?;
        Ok(())
    }

    fn reorder_source(&mut self, index: u32, rect: Rect, order: &[(u32, ValueKind)]) {
        let cells = &mut self.source[index as usize].cells;
        let original = cells.clone();
        for (offset, (source, _)) in order.iter().enumerate() {
            let target = rect.start.row + u32::try_from(offset).expect("row offset");
            for col in rect.start.col..=rect.end.col {
                let from = Address { row: *source, col }.to_string();
                let to = Address { row: target, col }.to_string();
                if let Some(cell) = original.get(&from) {
                    cells.insert(to, cell.clone());
                } else {
                    cells.remove(&to);
                }
            }
        }
    }

    pub(crate) fn import_xlsx(bytes: &[u8], title: &str) -> Result<Sheet> {
        let data = ironcalc::import::load_from_xlsx_bytes(bytes, title, "en", "UTC")?;
        let mut model = ic(Model::from_workbook(data, "en"))?;
        model.evaluate();
        let source = model
            .workbook
            .worksheets
            .iter()
            .map(|ws| Tab::new(&ws.name))
            .collect();
        let workbook = Self { model, source };
        let base = Sheet::from_design(title, &crate::design::Design::default());
        let mut sheet = workbook.to_sheet(&base);
        // OOXML uses uppercase RGB; the file model uses canonical #rrggbb.
        for tab in &mut sheet.tabs {
            for cell in tab.cells.values_mut() {
                for color in [&mut cell.fmt.fill, &mut cell.fmt.color]
                    .into_iter()
                    .flatten()
                {
                    color.make_ascii_lowercase();
                }
            }
        }
        super::validate::checked(&sheet)
    }

    pub(crate) fn export_xlsx(sheet: &Sheet) -> Result<Vec<u8>> {
        let mut workbook = Self::load(sheet)?;
        // Resolve tokens only in the export model, leaving the source untouched.
        let resolve = |color: &mut Color| {
            if let Color::Rgb(name) = color
                && let Some(hex) = sheet.theme.colors.get(name)
            {
                name.clone_from(hex);
            }
        };
        for a in workbook.model.get_all_cells() {
            let mut fmt = ic(workbook.model.get_style_for_cell(a.index, a.row, a.column))?;
            resolve(&mut fmt.font.color);
            resolve(&mut fmt.fill.color);
            ic(workbook
                .model
                .set_cell_style(a.index, a.row, a.column, &fmt))?;
        }
        Ok(
            ironcalc::export::save_xlsx_to_writer(&workbook.model, Cursor::new(Vec::new()))?
                .into_inner(),
        )
    }
}

// IronCalc's clipboard API moves observers within a sheet. For a cross-tab
// cut, re-express references in the original AST against the new tab as well.
struct Cut {
    source: u32,
    target: u32,
    rect: Rect,
    to: Address,
    names: Vec<String>,
}

impl Cut {
    fn point(&self, sheet: u32, row: i32, col: i32) -> (u32, i32, i32) {
        if sheet == self.source
            && row >= int(self.rect.start.row)
            && row <= int(self.rect.end.row)
            && col >= int(self.rect.start.col)
            && col <= int(self.rect.end.col)
        {
            (
                self.target,
                row + int(self.to.row) - int(self.rect.start.row),
                col + int(self.to.col) - int(self.rect.start.col),
            )
        } else {
            (sheet, row, col)
        }
    }

    #[allow(clippy::too_many_lines)]
    fn rewrite(&self, node: &mut Formula, old: &CellReferenceRC, new: &CellReferenceRC) {
        match node {
            Formula::ReferenceKind {
                sheet_name,
                sheet_index,
                absolute_row,
                absolute_column,
                row,
                column,
            } => {
                let r = *row + if *absolute_row { 0 } else { old.row };
                let c = *column + if *absolute_column { 0 } else { old.column };
                let (sheet, r, c) = self.point(*sheet_index, r, c);
                *sheet_index = sheet;
                *sheet_name = (sheet_name.is_some() || self.names[sheet as usize] != new.sheet)
                    .then(|| self.names[sheet as usize].clone());
                *row = r - if *absolute_row { 0 } else { new.row };
                *column = c - if *absolute_column { 0 } else { new.column };
            }
            Formula::RangeKind {
                sheet_name,
                sheet_index,
                absolute_row1,
                absolute_column1,
                row1,
                column1,
                absolute_row2,
                absolute_column2,
                row2,
                column2,
            } => {
                let r1 = *row1 + if *absolute_row1 { 0 } else { old.row };
                let c1 = *column1 + if *absolute_column1 { 0 } else { old.column };
                let r2 = *row2 + if *absolute_row2 { 0 } else { old.row };
                let c2 = *column2 + if *absolute_column2 { 0 } else { old.column };
                let a = self.point(*sheet_index, r1, c1);
                let b = self.point(*sheet_index, r2, c2);
                let (sheet, r1, c1, r2, c2) =
                    if a.0 == self.target && b.0 == self.target && *sheet_index == self.source {
                        (self.target, a.1, a.2, b.1, b.2)
                    } else {
                        (*sheet_index, r1, c1, r2, c2)
                    };
                *sheet_index = sheet;
                *sheet_name = (sheet_name.is_some() || self.names[sheet as usize] != new.sheet)
                    .then(|| self.names[sheet as usize].clone());
                *row1 = r1 - if *absolute_row1 { 0 } else { new.row };
                *column1 = c1 - if *absolute_column1 { 0 } else { new.column };
                *row2 = r2 - if *absolute_row2 { 0 } else { new.row };
                *column2 = c2 - if *absolute_column2 { 0 } else { new.column };
            }
            Formula::OpRangeKind { left, right }
            | Formula::OpConcatenateKind { left, right }
            | Formula::OpSumKind { left, right, .. }
            | Formula::OpProductKind { left, right, .. }
            | Formula::OpPowerKind { left, right }
            | Formula::CompareKind { left, right, .. } => {
                self.rewrite(left, old, new);
                self.rewrite(right, old, new);
            }
            Formula::FunctionKind { args, .. } | Formula::NamedFunctionKind { args, .. } => {
                for arg in args {
                    self.rewrite(arg, old, new);
                }
            }
            Formula::LambdaDefKind { body, .. } => self.rewrite(body, old, new),
            Formula::LambdaCallKind { lambda, args } => {
                self.rewrite(lambda, old, new);
                for arg in args {
                    self.rewrite(arg, old, new);
                }
            }
            Formula::ImplicitIntersection { child, .. }
            | Formula::SpillRangeOperator { child }
            | Formula::UnaryKind { right: child, .. } => self.rewrite(child, old, new),
            _ => {}
        }
    }
}

fn compare(a: &ValueKind, b: &ValueKind, desc: bool) -> Ordering {
    let rank = |v: &ValueKind| match v {
        ValueKind::Number(_) => 0,
        ValueKind::Text(_) | ValueKind::Bool(_) | ValueKind::Error(_) => 1,
        ValueKind::Empty => 2,
    };
    let category = rank(a).cmp(&rank(b));
    if category != Ordering::Equal {
        return category;
    }
    let text = |v: &ValueKind| match v {
        ValueKind::Text(s) | ValueKind::Error(s) => s.clone(),
        ValueKind::Bool(b) => b.to_string(),
        _ => String::new(),
    };
    let ordering = match (a, b) {
        (ValueKind::Number(a), ValueKind::Number(b)) => a.total_cmp(b),
        (ValueKind::Empty, ValueKind::Empty) => Ordering::Equal,
        _ => text(a).cmp(&text(b)),
    };
    if desc { ordering.reverse() } else { ordering }
}

fn copy_row(model: &Model<'_>, index: u32, rect: Rect, from: u32) -> Result<ClipboardData> {
    let mut row = BTreeMap::new();
    for col in rect.start.col..=rect.end.col {
        row.insert(
            int(col),
            serde_json::json!({
                "text": ic(model.get_localized_cell_content(index, int(from), int(col)))?,
                "style": ic(model.get_style_for_cell(index, int(from), int(col)))?,
                "is_spill": false,
            }),
        );
    }
    Ok(serde_json::from_value(serde_json::to_value(
        BTreeMap::from([(int(from), row)]),
    )?)?)
}

pub(crate) fn transform(base: &Sheet, op: &Op) -> Result<Sheet> {
    let mut workbook = Workbook::load(base)?;
    workbook.edit(op)?;
    Ok(workbook.to_sheet(base))
}
