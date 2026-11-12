//! Bounded, tab-separated read views for tool and studio callers.
use super::{
    Sheet, Workbook,
    address::{Address, Range, column_name},
};
use serde::{Deserialize, Serialize};

/// Whether a formula cell shows its editable formula or calculated display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    Values,
    Formulas,
}

fn flat(text: &str) -> String {
    text.replace(['\t', '\r', '\n'], " ")
}

fn finish(out: &mut String, more: &str, max_chars: usize) {
    out.extend(
        more.chars()
            .take(max_chars.saturating_sub(out.chars().count())),
    );
}

/// Per-tab used range and the first used row as a header guess. The final
/// `more:` line names omitted tabs; even that line respects the character cap.
#[must_use]
pub fn outline(sheet: &Sheet, max_chars: usize) -> String {
    let workbook = Workbook::load(sheet).ok();
    let mut out = String::new();
    for (index, tab) in sheet.tabs.iter().enumerate() {
        let range = tab.used_range();
        let header = range.map_or_else(String::new, |rect| {
            (rect.start.col..=rect.end.col)
                .take(12)
                .map(|col| {
                    let key = Address {
                        row: rect.start.row,
                        col,
                    }
                    .to_string();
                    let text = workbook.as_ref().map_or_else(
                        || {
                            tab.cells
                                .get(&key)
                                .map_or_else(String::new, |c| c.input.clone())
                        },
                        |w| w.value(&tab.name, &key).display,
                    );
                    flat(&text).chars().take(60).collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" | ")
        });
        let used = range.map_or_else(|| "empty".into(), |r| r.to_string());
        let line = format!("{} · {used} · {header}\n", tab.name);
        let more = format!("more: {} tabs\n", sheet.tabs.len() - index);
        let reserve = if index + 1 < sheet.tabs.len() {
            more.chars().count()
        } else {
            0
        };
        if out.chars().count() + line.chars().count() + reserve > max_chars {
            finish(&mut out, &more, max_chars);
            break;
        }
        out.push_str(&line);
    }
    out
}

/// A TSV grid with column letters and a leading row-number column. Complete
/// rows only; the final `more: rows N:M` names the rows that did not fit.
#[must_use]
pub fn read_range(
    workbook: &Workbook,
    sheet: &Sheet,
    tab: &str,
    range: &str,
    show: Show,
    max_chars: usize,
) -> String {
    let result = (|| -> anyhow::Result<String> {
        let tab = sheet
            .tabs
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(tab))
            .ok_or_else(|| anyhow::anyhow!("no tab {tab:?}"))?;
        let rect = range.parse::<Range>()?.bounds();
        let mut out = String::new();
        let header = (rect.start.col..=rect.end.col)
            .map(column_name)
            .collect::<anyhow::Result<Vec<_>>>()?
            .join("\t");
        let header = format!("\t{header}\n");
        let more = format!("more: rows {}:{}\n", rect.start.row, rect.end.row);
        if header.chars().count() + more.chars().count() > max_chars {
            finish(&mut out, &more, max_chars);
            return Ok(out);
        }
        out.push_str(&header);
        for row in rect.start.row..=rect.end.row {
            let mut line = row.to_string();
            for col in rect.start.col..=rect.end.col {
                let key = Address { row, col }.to_string();
                let formula = tab
                    .cells
                    .get(&key)
                    .filter(|c| show == Show::Formulas && c.input.starts_with('='));
                let value = formula.map_or_else(
                    || workbook.value(&tab.name, &key).display,
                    |c| c.input.clone(),
                );
                line.push('\t');
                // Do not build a giant line merely to decide it cannot fit.
                line.extend(flat(&value).chars().take(max_chars.saturating_add(1)));
                if line.chars().count() > max_chars {
                    break;
                }
            }
            line.push('\n');
            let more = format!("more: rows {row}:{}\n", rect.end.row);
            let reserve = if row < rect.end.row {
                more.chars().count()
            } else {
                0
            };
            if out.chars().count() + line.chars().count() + reserve > max_chars {
                finish(&mut out, &more, max_chars);
                break;
            }
            out.push_str(&line);
        }
        Ok(out)
    })();
    result.unwrap_or_else(|e| format!("error: {e}").chars().take(max_chars).collect())
}
