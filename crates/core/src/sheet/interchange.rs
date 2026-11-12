//! Byte-oriented CSV and XLSX interchange. Callers choose paths and dialogs.
use super::{
    Cell, Sheet, Workbook,
    address::{Address, MAX_COLS, MAX_ROWS},
};
use crate::design::Design;
use anyhow::{Result, anyhow, ensure};

/// Import UTF-8 RFC 4180 CSV. Every field is editable input, including formulas.
/// The filename's sanitized stem names the single tab.
pub fn import_csv(bytes: &[u8], filename: &str, design: &Design) -> Result<Sheet> {
    let stem = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Sheet1");
    let mut name: String = stem
        .chars()
        .map(|c| {
            if matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .take(31)
        .collect();
    if name.trim().is_empty() {
        name = "Sheet1".into();
    }
    let mut sheet = Sheet::from_design(stem, design);
    sheet.tabs[0].name = name;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    for (row, record) in reader.records().enumerate() {
        let row = u32::try_from(row)? + 1;
        ensure!(row <= MAX_ROWS, "CSV exceeds Excel row limit");
        let record = record?;
        ensure!(
            record.len() <= MAX_COLS as usize,
            "CSV exceeds Excel column limit"
        );
        for (col, input) in record.iter().enumerate() {
            ensure!(
                input.chars().count() <= 32_767,
                "CSV {row}: input exceeds 32767 characters"
            );
            if !input.is_empty() {
                let key = Address {
                    row,
                    col: u32::try_from(col)? + 1,
                }
                .to_string();
                sheet.tabs[0].cells.insert(
                    key,
                    Cell {
                        input: input.into(),
                        ..Cell::default()
                    },
                );
                ensure!(
                    sheet.tabs[0].cells.len() <= super::validate::MAX_CELLS,
                    "CSV exceeds stored cell limit"
                );
            }
        }
    }
    super::validate::checked(&sheet)
}

/// Export one tab's computed displays as RFC 4180 CSV with LF line endings.
/// The rectangle starts at A1 so leading empty rows/columns retain positions.
pub fn export_csv(sheet: &Sheet, tab: &str) -> Result<Vec<u8>> {
    let workbook = Workbook::load(sheet)?;
    let tab = sheet
        .tabs
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(tab))
        .ok_or_else(|| anyhow!("no tab {tab:?}"))?;
    let Some(rect) = tab.used_range() else {
        return Ok(Vec::new());
    };
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    for row in 1..=rect.end.row {
        writer.write_record((1..=rect.end.col).map(|col| {
            workbook
                .value(&tab.name, &Address { row, col }.to_string())
                .display
        }))?;
    }
    Ok(writer.into_inner()?)
}

/// Import XLSX inputs and supported formatting through `IronCalc`. Unsupported
/// features are silently dropped: charts, images, comments, links, named ranges,
/// tables, conditional formatting, validation, print settings, hidden state,
/// rich text, font family/size, borders, and alignment beyond left/center/right.
/// Array/spill results are recalculated, not saved as inputs.
pub fn import_xlsx(bytes: &[u8], title: &str) -> Result<Sheet> {
    super::engine::Workbook::import_xlsx(bytes, title)
}

/// Export calculated XLSX through `IronCalc`, resolving theme colors to hex.
pub fn export_xlsx(sheet: &Sheet) -> Result<Vec<u8>> {
    super::engine::Workbook::export_xlsx(sheet)
}
