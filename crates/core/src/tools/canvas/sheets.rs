//! Spreadsheet actions sharing canvas confinement, history and export helpers.
use super::*;
use crate::sheet::{
    self, Sheet, Show, Tab, Workbook,
    address::{Address, MAX_COLS, MAX_ROWS, Range, Rect, TabRange, column_index},
};

pub(super) const SHEET_SCHEMA: &str = "\nsheet schema\n\
set_cells{tab,at,values:[[input]]}\n\
clear{tab,range,what:inputs|formats|all}\n\
format{tab,range,patch:fmt}\n\
insert_rows{tab,at:row,count}\n\
delete_rows{tab,at:row,count}\n\
insert_cols{tab,at:column,count}\n\
delete_cols{tab,at:column,count}\n\
sort{tab,range,by:column,desc,header}\n\
set_col_width{tab,cols:A:C,width}\n\
set_row_height{tab,rows:1:3,height}\n\
freeze{tab,rows,cols}\n\
merge{tab,range}\n\
unmerge{tab,range}\n\
add_tab{name,after?}\n\
rename_tab{tab,name}\n\
delete_tab{tab}\n\
move_tab{tab,after?}\n\
set_title{title}\n\
apply_design{design?}\n\
Each op has op:name; atomic. fmt: num,bold,italic,underline,align:left|center|right,fill,color (theme token or #RRGGBB). Range: A1:F20,B:B,2:4; optional Data! or 'Q3 plan'! prefix (apostrophe escaped ''). Without tab: first tab. Formulas start with =.\n";

pub(in crate::tools) fn is_sheet(action: &str, v: &Value) -> bool {
    v["path"]
        .as_str()
        .map_or(action == "deck_create" && v["kind"] == "sheet", |p| {
            p.starts_with("sheets/") || p.ends_with(".sheet.json")
        })
}

fn selection<'a>(sheet: &'a Sheet, v: &Value) -> Result<(&'a Tab, Option<Range>)> {
    let parsed = v["range"]
        .as_str()
        .map(str::parse::<TabRange>)
        .transpose()?;
    let tab = if let Some(name) = parsed.as_ref().and_then(|r| r.tab.as_deref()) {
        sheet
            .tabs
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .with_context(|| format!("no tab {name:?}"))?
    } else {
        &sheet.tabs[0]
    };
    Ok((tab, parsed.map(|r| r.range)))
}

impl ToolBox {
    /// The sheet op reference, once per session and only when it fits.
    pub(in crate::tools) fn sheet_schema_if_fits(&self, used: usize) -> &'static str {
        let mut context = self.deck_link.lock().expect("deck link");
        if context.sheet_schema_shown || used + SHEET_SCHEMA.chars().count() > read::RESULT_BUDGET {
            return "";
        }
        context.sheet_schema_shown = true;
        SHEET_SCHEMA
    }

    pub(super) fn sheet_action(&self, action: &str, v: &Value) -> Result<String> {
        if matches!(action, "deck_comment" | "deck_resolve" | "deck_fonts") {
            bail!("not available for sheets yet");
        }
        if action == "deck_create" {
            return self.sheet_create(v);
        }
        let relative = v["path"]
            .as_str()
            .context("path is required: sheets/name.sheet.json")?;
        let path = sheet::storage::confined(&self.space_files_dir, relative)?;
        let sheet = sheet::storage::load(&self.space_files_dir, relative)?;
        match action {
            "deck_read" => {
                let mut out = format!(
                    "sheet · {relative} · revision {}\n",
                    short(&sheet.revision())
                );
                let (tab, range) = selection(&sheet, v)?;
                if let Some(range) = range {
                    out.push_str(&sheet::read_range(
                        &Workbook::load(&sheet)?,
                        &sheet,
                        &tab.name,
                        &range.to_string(),
                        Show::Both,
                        read::RESULT_BUDGET.saturating_sub(out.chars().count()),
                    ));
                } else {
                    out.push_str(&sheet::outline(&sheet, 4500));
                    out.push_str(self.sheet_schema_if_fits(out.chars().count()));
                }
                Ok(out)
            }
            "deck_edit" => self.sheet_edit(relative, &path, &sheet, v),
            "deck_view" => {
                let (tab, range) = selection(&sheet, v)?;
                sheet::render::view(&sheet, &path, &self.space_files_dir, tab, range)
            }
            "deck_history" => self.sheet_history(relative, &path, &sheet, v),
            "deck_export" => self.sheet_export(relative, &sheet, v),
            _ => bail!("unknown sheet action"),
        }
    }

    fn sheet_create(&self, v: &Value) -> Result<String> {
        let default = crate::design::default_slug(&self.space_files_dir)?;
        let design_slug = v["design"].as_str().unwrap_or(&default);
        let design = crate::design::load(&self.space_files_dir, design_slug)?;
        let title = v["title"].as_str().unwrap_or("Untitled");
        let mut sheet = if let Some(from) = v["from"].as_str() {
            let source = assets::find_space_file(from, &self.space_files_dir)
                .context("no import file in the space files")?;
            let bytes = std::fs::read(&source)?;
            match source
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("csv") => sheet::interchange::import_csv(&bytes, from, &design)?,
                Some("xlsx") => {
                    let mut imported = sheet::interchange::import_xlsx(&bytes, title)?;
                    imported.theme = crate::doc::Theme::from_design(&design);
                    imported.design = Some(design_slug.into());
                    imported.design_rev = Some(design.revision());
                    imported
                }
                _ => bail!("sheet create from requires .csv or .xlsx"),
            }
        } else {
            Sheet::from_design(title, &design)
        };
        if v["title"].as_str().is_some() {
            sheet.title = title.into();
        }
        let relative = v["path"].as_str().map_or_else(
            || format!("sheets/{}.sheet.json", slug(&sheet.title)),
            str::to_string,
        );
        let path = sheet::storage::confined(&self.space_files_dir, &relative)?;
        ensure!(
            !path.exists(),
            "{relative} already exists; read it, then edit it"
        );
        sheet::storage::save(&self.space_files_dir, &relative, &sheet, Who::Model)?;
        self.deck_changed(&path, Some(String::new()));
        let mut out = format!(
            "created sheet · {relative} · revision {}\n{}",
            short(&sheet.revision()),
            sheet::outline(&sheet, 4500)
        );
        out.push_str(self.sheet_schema_if_fits(out.chars().count()));
        Ok(out)
    }

    fn sheet_edit(&self, relative: &str, path: &Path, before: &Sheet, v: &Value) -> Result<String> {
        let ops: Vec<sheet::Op> = serde_json::from_value(v["ops"].clone())
            .context("Nothing was written. ops must be an array of sheet operations")?;
        ensure!(
            !ops.is_empty(),
            "Nothing was written. ops must contain at least one operation"
        );
        let rebased = sheet_revision(path, before, v["revision"].as_str(), &ops)?;
        let resolver = |slug: &str| crate::design::load(&self.space_files_dir, slug);
        let mut next = before.clone();
        let mut lines = String::new();
        let mut affected = Vec::new();
        for (index, op) in ops.iter().enumerate() {
            let after =
                sheet::apply_with_resolver(&next, std::slice::from_ref(op), Some(&resolver))
                    .map_err(|e| anyhow::anyhow!("Nothing was written. ops[{index}]: {e}"))?;
            let value = serde_json::to_value(op)?;
            let name = value["op"].as_str().unwrap_or_default();
            let target = edited_range(op)?;
            let label = target.as_ref().map_or_else(
                || {
                    value["tab"]
                        .as_str()
                        .or(value["name"].as_str())
                        .unwrap_or_default()
                        .to_string()
                },
                |(tab, rect)| format!("{tab}!{rect}"),
            );
            let _ = writeln!(lines, "- {name} {label}");
            if let Some((tab, rect)) = target
                && let Some(tab) = next
                    .tabs
                    .iter()
                    .chain(&after.tabs)
                    .find(|t| t.name.eq_ignore_ascii_case(&tab))
            {
                affected.push((tab.id.clone(), rect));
            }
            next = after;
        }
        let mut out = format!(
            "edited sheet · {relative} · revision {} (was {})\n",
            short(&next.revision()),
            short(&before.revision())
        );
        if let Some(note) = rebased {
            let _ = writeln!(out, "{note}");
        }
        out.push_str(&lines);
        let workbook = Workbook::load(&next)?;
        let mut errors = Vec::new();
        for tab in &next.tabs {
            let ranges = affected
                .iter()
                .filter_map(|(id, rect)| (id == &tab.id).then_some(*rect))
                .collect::<Vec<_>>();
            for (address, error) in workbook.errors_in(&tab.name, &ranges)? {
                errors.push(format!("{}!{address} {error}", tab.name));
            }
        }
        if !errors.is_empty() {
            let _ = write!(out, "errors: {}", errors[..errors.len().min(20)].join(", "));
            if errors.len() > 20 {
                let _ = write!(out, ", … {} more", errors.len() - 20);
            }
            out.push('\n');
        }
        sheet::storage::save(&self.space_files_dir, relative, &next, Who::Model)?;
        self.deck_changed(path, Some(String::new()));
        Ok(out)
    }

    fn sheet_history(
        &self,
        relative: &str,
        path: &Path,
        sheet: &Sheet,
        v: &Value,
    ) -> Result<String> {
        if let Some(revision) = v["revision"].as_str() {
            let entry = history::find(path, revision)?;
            let snapshot: Sheet = entry.load_for()?;
            sheet::storage::save(&self.space_files_dir, relative, &snapshot, Who::Model)?;
            self.deck_changed(path, Some(String::new()));
            return Ok(format!(
                "restored {relative} to version {} · revision {} (was {})\nThis is a new write; the replaced version is in history.\n",
                entry.rev8,
                short(&snapshot.revision()),
                short(&sheet.revision())
            ));
        }
        let entries = history::list(path)?;
        let mut out = format!(
            "history · {relative} · current revision {}\n",
            short(&sheet.revision())
        );
        for entry in entries.iter().rev().take(HISTORY_LINES) {
            let snapshot: Sheet = entry.load_for()?;
            let _ = writeln!(
                out,
                "{} · {} · replaced by {} · {} tabs",
                entry.rev8,
                entry.time(),
                entry.who,
                snapshot.tabs.len()
            );
        }
        if entries.is_empty() {
            out.push_str("no earlier versions kept yet\n");
        }
        out.push_str("restore with history and revision\n");
        Ok(out)
    }

    fn sheet_export(&self, relative: &str, sheet: &Sheet, v: &Value) -> Result<String> {
        let (format, destination) = self.export_request(v)?;
        let bytes = match format.as_str() {
            "csv" => {
                let (tab, _) = selection(sheet, v)?;
                sheet::interchange::export_csv(sheet, &tab.name)?
            }
            "xlsx" => sheet::interchange::export_xlsx(sheet)?,
            _ => bail!("sheet export format is csv or xlsx"),
        };
        let output = crate::deck::export::Export {
            bytes,
            file_name: format!("{}.{format}", slug(&sheet.title)),
            warnings: Vec::new(),
        };
        let written = crate::deck::export::write_export(
            &output,
            &self.space_files_dir,
            destination.as_deref(),
        )?;
        Ok(format!(
            "exported {relative} → {}\n",
            written
                .strip_prefix(self.space_files_dir.canonicalize()?)
                .unwrap_or(&written)
                .display()
        ))
    }
}

/// The edited area, kept sparse even for whole-row/column operations.
fn edited_range(op: &sheet::Op) -> Result<Option<(String, Rect)>> {
    use sheet::Op;
    let (tab, rect) = match op {
        Op::SetCells { tab, at, values } => {
            let start: Address = at.parse()?;
            (
                tab,
                Rect {
                    start,
                    end: Address {
                        row: start.row + u32::try_from(values.len())?.saturating_sub(1),
                        col: start.col
                            + u32::try_from(values.first().map_or(0, Vec::len))?.saturating_sub(1),
                    },
                },
            )
        }
        Op::Clear { tab, range, .. }
        | Op::Format { tab, range, .. }
        | Op::Sort { tab, range, .. }
        | Op::Merge { tab, range }
        | Op::Unmerge { tab, range } => (tab, range.parse::<Range>()?.bounds()),
        Op::InsertRows { tab, at, .. } | Op::DeleteRows { tab, at, .. } => {
            (tab, Range::Rows(*at, MAX_ROWS).bounds())
        }
        Op::InsertCols { tab, at, .. } | Op::DeleteCols { tab, at, .. } => {
            (tab, Range::Columns(column_index(at)?, MAX_COLS).bounds())
        }
        Op::SetColWidth { tab, cols, .. } => (tab, cols.parse::<Range>()?.bounds()),
        Op::SetRowHeight { tab, rows, .. } => (tab, rows.parse::<Range>()?.bounds()),
        Op::Freeze { tab, .. } | Op::RenameTab { tab, .. } | Op::MoveTab { tab, .. } => {
            (tab, Range::Rows(1, MAX_ROWS).bounds())
        }
        _ => return Ok(None),
    };
    Ok(Some((tab.clone(), rect)))
}

/// Mirror `doc_revision`'s history-based rebase, comparing cells for local edits
/// and whole tabs for structural edits rather than document block ids.
fn sheet_revision(
    path: &Path,
    current: &Sheet,
    given: Option<&str>,
    ops: &[sheet::Op],
) -> Result<Option<String>> {
    let Some(given) = given.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if given.len() >= 8 && current.revision().starts_with(given) {
        return Ok(None);
    }
    let stale = format!(
        "stale revision {given}: sheet is now at revision {}.",
        short(&current.revision())
    );
    let entry = history::find(path, given).with_context(|| {
        format!("{stale} Nothing was written. Read the ranges you are editing; version not kept.")
    })?;
    let old: Sheet = entry.load_for()?;
    for op in ops {
        let unchanged = match op {
            sheet::Op::SetTitle { .. } => old.title == current.title,
            sheet::Op::ApplyDesign { .. } => {
                old.theme == current.theme
                    && old.design == current.design
                    && old.design_rev == current.design_rev
            }
            sheet::Op::SetCells { .. } | sheet::Op::Clear { .. } | sheet::Op::Format { .. } => {
                let (name, rect) = edited_range(op)?.context("cell edit range")?;
                let before = old.tabs.iter().find(|t| t.name.eq_ignore_ascii_case(&name));
                let after = current
                    .tabs
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(&name));
                match (before, after) {
                    (Some(a), Some(b)) if a.id == b.id => a
                        .cells
                        .keys()
                        .chain(b.cells.keys())
                        .filter(|key| rect.contains(key.parse().expect("validated address")))
                        .all(|key| a.cells.get(key) == b.cells.get(key)),
                    _ => false,
                }
            }
            _ => {
                let value = serde_json::to_value(op)?;
                if let Some(tab) = value["tab"].as_str() {
                    old.tabs.iter().find(|t| t.name.eq_ignore_ascii_case(tab))
                        == current
                            .tabs
                            .iter()
                            .find(|t| t.name.eq_ignore_ascii_case(tab))
                        && old
                            .tabs
                            .iter()
                            .map(|t| &t.name)
                            .eq(current.tabs.iter().map(|t| &t.name))
                } else {
                    old.tabs == current.tabs
                }
            }
        };
        ensure!(
            unchanged,
            "{stale} The edit touches cells or tabs changed since then. Nothing was written; read those ranges before retrying."
        );
    }
    Ok(Some(format!(
        "rebased: touched cells and tabs unchanged since {given}; applied to revision {}",
        short(&current.revision())
    )))
}
