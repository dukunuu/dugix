//! Bounded, revision-and-range cached spreadsheet previews.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
use super::{
    Align, Format, Sheet, Tab, ValueKind, Workbook,
    address::{Address, Range, Rect, column_name},
};
use crate::deck::text::{color, escape};
use anyhow::{Context as _, Result, ensure};
use resvg::{tiny_skia, usvg};
use std::{fmt::Write as _, path::Path};

/// Render a range, clipping it to 60 rows by 20 columns. An omitted range uses
/// the selected tab's used cells, or A1 for an empty tab.
pub fn view(
    sheet: &Sheet,
    path: &Path,
    files: &Path,
    tab: &Tab,
    range: Option<Range>,
) -> Result<String> {
    let requested = range.map_or_else(
        || {
            tab.used_range().unwrap_or(Rect {
                start: Address { row: 1, col: 1 },
                end: Address { row: 1, col: 1 },
            })
        },
        Range::bounds,
    );
    let rect = Rect {
        start: requested.start,
        end: Address {
            row: requested.end.row.min(requested.start.row + 59),
            col: requested.end.col.min(requested.start.col + 19),
        },
    };
    let directory = crate::deck::render::render_dir(path, files)?;
    let revision = sheet.revision();
    let key = crate::deck::hash(&(tab.id.as_str(), rect.to_string()));
    let name = format!("range-{}-{}.png", &revision[..8], &key[..12]);
    let output = directory.join(&name);
    ensure!(
        !output.is_symlink(),
        "render cache must not contain symlinks"
    );
    if !output.is_file() {
        let assets = path.parent().context("sheet folder")?.join("assets");
        let bytes = png(sheet, tab, rect, Some(&assets))?;
        crate::deck::write_atomic(&output, &bytes)?;
    }
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        let old = entry.file_name().to_string_lossy().into_owned();
        if old.starts_with("range-")
            && Path::new(&old)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
            && !old.starts_with(&format!("range-{}-", &revision[..8]))
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    let note = if rect == requested {
        String::new()
    } else {
        format!(
            "clipped to {}!{rect} (maximum 60 rows × 20 columns)\n",
            tab.name
        )
    };
    Ok(format!(
        "{note}![{}!{rect}]({})\n",
        tab.name,
        output
            .strip_prefix(files)?
            .to_string_lossy()
            .replace('\\', "/")
    ))
}

fn theme(sheet: &Sheet) -> crate::deck::Theme {
    crate::deck::Theme {
        colors: sheet.theme.colors.clone(),
        fonts: sheet.theme.fonts.clone(),
        font_files: sheet.theme.font_files.clone(),
        text_styles: sheet.theme.text_styles.clone(),
        ..Default::default()
    }
}

fn cell_svg(
    out: &mut String,
    bounds: [f64; 4],
    text: &str,
    fmt: &Format,
    theme: &crate::deck::Theme,
) {
    let [x, y, w, h] = bounds;
    let fill = color(theme, fmt.fill.as_deref().unwrap_or("bg"));
    let ink = color(theme, fmt.color.as_deref().unwrap_or("fg"));
    let family = theme.fonts.get("body").map_or("sans-serif", String::as_str);
    let (tx, anchor) = match fmt.align.unwrap_or(Align::Left) {
        Align::Left => (x + 5.0, "start"),
        Align::Center => (x + w / 2.0, "middle"),
        Align::Right => (x + w - 5.0, "end"),
    };
    let _ = write!(
        out,
        "<svg x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" overflow=\"hidden\"><rect width=\"100%\" height=\"100%\" fill=\"{fill}\" stroke=\"#ccc\"/><text x=\"{}\" y=\"{}\" text-anchor=\"{anchor}\" font-family=\"{}\" font-size=\"14\" font-weight=\"{}\" font-style=\"{}\" text-decoration=\"{}\" fill=\"{ink}\">{}</text></svg>",
        tx - x,
        h / 2.0 + 5.0,
        escape(family),
        if fmt.bold { "bold" } else { "normal" },
        if fmt.italic { "italic" } else { "normal" },
        if fmt.underline { "underline" } else { "none" },
        escape(text)
    );
}

fn cells_svg(
    out: &mut String,
    sheet: &Sheet,
    tab: &Tab,
    rect: Rect,
    xs: &[f64],
    ys: &[f64],
) -> Result<()> {
    let theme = theme(sheet);
    let workbook = Workbook::load(sheet)?;
    let merges = tab
        .merges
        .iter()
        .map(|s| s.parse::<Range>().map(Range::bounds))
        .collect::<Result<Vec<_>>>()?;
    for (r, row) in (rect.start.row..=rect.end.row).enumerate() {
        for (c, col) in (rect.start.col..=rect.end.col).enumerate() {
            let address = Address { row, col };
            let area = merges.iter().find(|m| m.contains(address));
            let (source, end) = if let Some(area) = area {
                if row != area.start.row.max(rect.start.row)
                    || col != area.start.col.max(rect.start.col)
                {
                    continue;
                }
                (
                    area.start,
                    Address {
                        row: area.end.row.min(rect.end.row),
                        col: area.end.col.min(rect.end.col),
                    },
                )
            } else {
                (address, address)
            };
            let key = source.to_string();
            let value = workbook.value(&tab.name, &key);
            let mut fmt = tab
                .cells
                .get(&key)
                .map_or_else(Format::default, |cell| cell.fmt.clone());
            if fmt.align.is_none() && matches!(value.kind, ValueKind::Number(_)) {
                fmt.align = Some(Align::Right);
            }
            let right = (end.col - rect.start.col + 1) as usize;
            let bottom = (end.row - rect.start.row + 1) as usize;
            cell_svg(
                out,
                [xs[c], ys[r], xs[right] - xs[c], ys[bottom] - ys[r]],
                &value.display,
                &fmt,
                &theme,
            );
        }
    }
    Ok(())
}

/// Draw a bounded grid window as PNG, using the workbook's copied theme.
pub fn png(sheet: &Sheet, tab: &Tab, rect: Rect, assets: Option<&Path>) -> Result<Vec<u8>> {
    let theme = theme(sheet);
    let mut xs = vec![44.0];
    for col in rect.start.col..=rect.end.col {
        xs.push(
            xs.last().copied().unwrap_or_default()
                + tab.cols.get(&column_name(col)?).copied().unwrap_or(100.0),
        );
    }
    let mut ys = vec![24.0];
    for row in rect.start.row..=rect.end.row {
        ys.push(
            ys.last().copied().unwrap_or_default() + tab.rows.get(&row).copied().unwrap_or(24.0),
        );
    }
    let width = *xs.last().context("column bounds")?;
    let height = *ys.last().context("row bounds")?;
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\"><rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        color(&theme, "bg")
    );
    let header = Format {
        bold: true,
        align: Some(Align::Center),
        ..Format::default()
    };
    for (index, col) in (rect.start.col..=rect.end.col).enumerate() {
        cell_svg(
            &mut out,
            [xs[index], 0.0, xs[index + 1] - xs[index], 24.0],
            &column_name(col)?,
            &header,
            &theme,
        );
    }
    for (index, row) in (rect.start.row..=rect.end.row).enumerate() {
        cell_svg(
            &mut out,
            [0.0, ys[index], 44.0, ys[index + 1] - ys[index]],
            &row.to_string(),
            &header,
            &theme,
        );
    }
    cells_svg(&mut out, sheet, tab, rect, &xs, &ys)?;
    if (rect.start.col..=rect.end.col).contains(&tab.frozen.cols) {
        let x = xs[(tab.frozen.cols - rect.start.col + 1) as usize];
        let _ = write!(
            out,
            "<path d=\"M{x} 0V{height}\" stroke=\"#555\" stroke-width=\"3\"/>"
        );
    }
    if (rect.start.row..=rect.end.row).contains(&tab.frozen.rows) {
        let y = ys[(tab.frozen.rows - rect.start.row + 1) as usize];
        let _ = write!(
            out,
            "<path d=\"M0 {y}H{width}\" stroke=\"#555\" stroke-width=\"3\"/>"
        );
    }
    out.push_str("</svg>");
    let options = usvg::Options {
        fontdb: crate::deck::text::deck_fonts(&theme, assets),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&out, &options)?;
    let scale = (1200.0 / tree.size().width()).min(1.0);
    let mut pixmap = tiny_skia::Pixmap::new(
        (tree.size().width() * scale).ceil() as u32,
        (tree.size().height() * scale).ceil() as u32,
    )
    .context("invalid sheet render size")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap.encode_png()?)
}
