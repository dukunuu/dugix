//! One SVG renderer for studio charts and model-visible PNG previews.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
use super::{
    Chart, ChartKind, Headers, Legend, SeriesIn, Sheet, Tab, ValueKind, Workbook,
    chart::Data,
    layout::{CellText, FONT_SIZE, LINE_HEIGHT},
};
use crate::deck::text::{color, escape};
use std::fmt::Write as _;

#[cfg(test)]
#[path = "chart_layout_tests.rs"]
mod tests;

/// Human-friendly ticks at multiples of 1, 2 or 5 × 10ⁿ.
#[must_use]
pub fn nice_ticks(min: f64, max: f64) -> Vec<f64> {
    let span = (max - min).max(f64::MIN_POSITIVE);
    let unit = 10.0_f64.powf((span / 5.0).log10().floor());
    let fraction = span / 5.0 / unit;
    let step = unit
        * if fraction <= 1.0 {
            1.0
        } else if fraction <= 2.0 {
            2.0
        } else if fraction <= 5.0 {
            5.0
        } else {
            10.0
        };
    let first = (min / step).floor();
    let last = (max / step).ceil();
    (0..=((last - first) as usize).min(12))
        .map(|i| {
            let value = (first + i as f64) * step;
            if value.abs() < step * 1e-10 {
                0.0
            } else {
                value
            }
        })
        .collect()
}

/// Numeric bounds, including signed stack totals rather than individual maxima.
#[must_use]
pub fn value_bounds(data: &Data, chart: &Chart) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    if chart.stacked {
        for p in 0..data.labels.len() {
            let mut positive = 0.0;
            let mut negative = 0.0;
            for series in &data.series {
                if let Some(n) = series.values[p] {
                    if n >= 0.0 {
                        positive += n;
                    } else {
                        negative += n;
                    }
                }
            }
            min = min.min(negative);
            max = max.max(positive);
        }
    } else {
        for n in data.series.iter().flat_map(|s| s.values.iter().flatten()) {
            min = min.min(*n);
            max = max.max(*n);
        }
    }
    if !min.is_finite() {
        min = 0.0;
        max = 1.0;
    }
    if matches!(
        chart.kind,
        ChartKind::Column | ChartKind::Bar | ChartKind::Area
    ) {
        min = min.min(0.0);
        max = max.max(0.0);
    }
    if min >= max {
        let pad = min.abs().max(1.0) * 0.1;
        min -= pad;
        max += pad;
    }
    let ticks = nice_ticks(min, max);
    min = chart.y_min.unwrap_or(ticks[0]);
    max = chart.y_max.unwrap_or(*ticks.last().expect("ticks"));
    if min >= max {
        if chart.y_max.is_some() {
            min = max - max.abs().max(1.0);
        } else {
            max = min + min.abs().max(1.0);
        }
    }
    (min, max)
}

const PAD: f64 = 12.0;
const GAP: f64 = 6.0;
const SWATCH: f64 = 9.0;
const MARKER: f64 = 3.0;

fn ellipsis(text: &str, width: f64, measure: &mut CellText<'_>) -> String {
    if measure.width(text, false) <= width {
        return text.into();
    }
    let mut short = String::new();
    let suffix = measure.width("…", false);
    if suffix > width {
        return short;
    }
    for ch in text.chars() {
        let next = format!("{short}{ch}");
        if measure.width(&next, false) + suffix > width {
            break;
        }
        short.push(ch);
    }
    short.push('…');
    short
}

fn luminance(ink: &str) -> f64 {
    let hex = ink.trim_start_matches('#');
    let short = hex.len() <= 4;
    let channel = |index: usize| {
        let start = index * if short { 1 } else { 2 };
        let n = u8::from_str_radix(&hex[start..start + if short { 1 } else { 2 }], 16)
            .expect("validated theme color");
        let value = f64::from(n) * if short { 17.0 } else { 1.0 } / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(0) + 0.7152 * channel(1) + 0.0722 * channel(2)
}

fn pie_label_fits(width: f64, radius: f64, mid: f64, half_angle: f64) -> bool {
    let label_radius = radius * 0.68;
    let chord = 2.0 * label_radius * half_angle.min(std::f64::consts::FRAC_PI_2).sin();
    width + GAP <= chord
        && [-width / 2.0, width / 2.0].iter().all(|dx| {
            [-LINE_HEIGHT / 2.0, LINE_HEIGHT / 2.0].iter().all(|dy| {
                let x = label_radius * mid.cos() + dx;
                let y = label_radius * mid.sin() + dy;
                let angle = (y.atan2(x) - mid + std::f64::consts::PI)
                    .rem_euclid(std::f64::consts::TAU)
                    - std::f64::consts::PI;
                x.hypot(y) < radius && angle.abs() < half_angle
            })
        })
}

fn widest(labels: &[String], measure: &mut CellText<'_>) -> f64 {
    labels
        .iter()
        .map(|text| measure.width(text, false))
        .fold(0.0, f64::max)
}

fn tick_labels(ticks: &[f64], format: Option<&str>) -> Vec<String> {
    ticks
        .iter()
        .map(|n| super::engine::format_number(*n, format))
        .collect()
}

fn horizontal_ticks(
    bounds: (f64, f64),
    format: Option<&str>,
    width: f64,
    measure: &mut CellText<'_>,
) -> Vec<f64> {
    let initial = nice_ticks(bounds.0, bounds.1);
    let mut step = initial[1] - initial[0];
    loop {
        let first = (bounds.0 / step).ceil();
        let last = (bounds.1 / step).floor();
        let ticks: Vec<_> = (0..=((last - first).max(0.0) as usize))
            .map(|i| (first + i as f64) * step)
            .filter(|n| *n >= bounds.0 && *n <= bounds.1)
            .collect();
        let label_width = widest(&tick_labels(&ticks, format), measure);
        if ticks.len() < 2 || step / (bounds.1 - bounds.0) * width >= label_width + GAP {
            return ticks;
        }
        let unit = 10.0_f64.powf(step.log10().floor());
        step = unit
            * if step / unit < 1.5 {
                2.0
            } else if step / unit < 3.5 {
                5.0
            } else {
                10.0
            };
    }
}

fn x_bounds(data: &Data) -> (f64, f64) {
    let mut min = data
        .x_values
        .iter()
        .flatten()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let mut max = data
        .x_values
        .iter()
        .flatten()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() {
        return (0.0, 1.0);
    }
    if min >= max {
        min -= 1.0;
        max += 1.0;
    }
    let ticks = nice_ticks(min, max);
    (ticks[0], *ticks.last().expect("ticks"))
}

// Recover the x cell's format without adding renderer metadata to the data model.
fn x_format(
    sheet: &Sheet,
    workbook: &Workbook,
    owner: &Tab,
    chart: &Chart,
    data: &Data,
) -> Option<String> {
    let (tab, range) = super::chart::source(sheet, owner, &chart.data)?;
    let mut rect = range.bounds();
    if !matches!(range, super::address::Range::Cells(_)) {
        let used = tab.used_range()?;
        rect.end.row = rect.end.row.min(used.end.row);
        rect.end.col = rect.end.col.min(used.end.col);
    }
    let rows = chart.series_in == SeriesIn::Rows;
    if (rows && rect.end.row == rect.start.row) || (!rows && rect.end.col == rect.start.col) {
        return None;
    }
    let address = |point, series| {
        super::address::Address {
            row: rect.start.row + if rows { series } else { point },
            col: rect.start.col + if rows { point } else { series },
        }
        .to_string()
    };
    let header = match chart.headers {
        Headers::Yes => true,
        Headers::No => false,
        Headers::Auto => data.series.iter().enumerate().any(|(index, series)| {
            series.values.iter().any(Option::is_some)
                && matches!(
                    workbook
                        .value(
                            &tab.name,
                            &address(0, u32::try_from(index).expect("bounded series") + 1)
                        )
                        .kind,
                    ValueKind::Text(_)
                )
        }),
    };
    let point = u32::try_from(data.x_values.iter().position(Option::is_some)?)
        .expect("bounded points")
        + u32::from(header);
    tab.cells.get(&address(point, 0))?.fmt.num.clone()
}

struct LegendItem {
    index: usize,
    label: String,
    at: [f64; 2],
}

#[derive(Default)]
struct LegendBlock {
    items: Vec<LegendItem>,
    width: f64,
    height: f64,
}

impl LegendBlock {
    fn new(chart: &Chart, data: &Data, width: f64, measure: &mut CellText<'_>) -> Self {
        let mut block = Self::default();
        if chart.legend == Legend::None {
            return block;
        }
        let labels: Vec<_> = if chart.kind == ChartKind::Pie {
            data.labels.iter().map(String::as_str).collect()
        } else {
            data.series.iter().map(|s| s.name.as_str()).collect()
        };
        let mut x = 0.0;
        let mut y = 0.0;
        for (index, text) in labels.iter().enumerate() {
            let label = ellipsis(text, (width - SWATCH - GAP).max(0.0), measure);
            let item_width = SWATCH + GAP + measure.width(&label, false);
            if chart.legend == Legend::Bottom && x > 0.0 && x + item_width > width {
                x = 0.0;
                y += LINE_HEIGHT + GAP;
            }
            block.items.push(LegendItem {
                index,
                label,
                at: [x, y + FONT_SIZE],
            });
            block.width = block.width.max(x + item_width);
            block.height = y + LINE_HEIGHT;
            if chart.legend == Legend::Right {
                y += LINE_HEIGHT + GAP;
            } else {
                x += item_width + 2.0 * GAP;
            }
        }
        block
    }
}

struct Layout {
    plot: [f64; 4],
    bounds: (f64, f64),
    x_bounds: (f64, f64),
    y_ticks: Vec<f64>,
    x_ticks: Vec<f64>,
    x_format: Option<String>,
    categories: Vec<String>,
    rotated: bool,
    category_step: usize,
    category_height: f64,
    title: Vec<String>,
    x_title: Vec<String>,
    y_title: Option<String>,
    legend: LegendBlock,
}

impl Layout {
    #[allow(clippy::too_many_lines)]
    fn new(
        chart: &Chart,
        data: &Data,
        theme: &crate::deck::Theme,
        x_format: Option<String>,
    ) -> Self {
        let mut measure = CellText::new(theme);
        let pie = chart.kind == ChartKind::Pie;
        let bar = chart.kind == ChartKind::Bar;
        let scatter = chart.kind == ChartKind::Scatter;
        let bounds = value_bounds(data, chart);
        let mut y_ticks: Vec<_> = nice_ticks(bounds.0, bounds.1)
            .into_iter()
            .filter(|n| *n >= bounds.0 && *n <= bounds.1)
            .collect();
        let value_width = widest(
            &tick_labels(&y_ticks, data.number_format.as_deref()),
            &mut measure,
        );
        let x_bounds = x_bounds(data);
        let x_width = if scatter {
            widest(
                &tick_labels(&nice_ticks(x_bounds.0, x_bounds.1), x_format.as_deref()),
                &mut measure,
            )
        } else if bar {
            value_width
        } else {
            0.0
        };
        let title = chart
            .title
            .as_ref()
            .filter(|s| !s.is_empty())
            .map_or_else(Vec::new, |text| {
                measure.lines(text, chart.w - 2.0 * PAD, false)
            });
        let top = PAD
            + if title.is_empty() {
                0.0
            } else {
                title.len() as f64 * LINE_HEIGHT + GAP + if pie { 0.0 } else { LINE_HEIGHT / 2.0 }
            };
        let mut categories: Vec<_> = data
            .labels
            .iter()
            .map(|s| ellipsis(s, chart.w * 0.4, &mut measure))
            .collect();
        let category_width = widest(&categories, &mut measure);
        let y_title_width = if !pie && chart.y_title.as_ref().is_some_and(|s| !s.is_empty()) {
            LINE_HEIGHT + GAP
        } else {
            0.0
        };
        let left = PAD
            + if pie {
                0.0
            } else {
                (if bar { category_width } else { value_width } + GAP + y_title_width)
                    .max(x_width / 2.0)
            };
        let mut legend = LegendBlock::new(chart, data, chart.w * 0.35, &mut measure);
        let right = (PAD
            + if chart.legend == Legend::Right && !legend.items.is_empty() {
                legend.width + GAP
            } else {
                0.0
            })
        .max(PAD + x_width / 2.0);
        let width = (chart.w - left - right).max(24.0);
        let count = data.labels.len().max(1) as f64;
        let rotated = !pie && !bar && !scatter && category_width + GAP > width / count;
        let mut category_height = if pie { 0.0 } else { LINE_HEIGHT };
        let mut category_step = 1;
        if rotated {
            let diagonal = std::f64::consts::FRAC_1_SQRT_2;
            let label_limit = ((chart.h * 0.45) / diagonal - LINE_HEIGHT).max(0.0);
            for (index, label) in categories.iter_mut().enumerate() {
                let x = left + (index as f64 + 0.5) * width / count;
                let room = (chart.w - PAD - x) / diagonal - LINE_HEIGHT;
                *label = ellipsis(label, label_limit.min(room).max(0.0), &mut measure);
            }
            category_height = (widest(&categories, &mut measure) + LINE_HEIGHT) * diagonal;
            category_step = ((category_height + GAP) * count / width).ceil().max(1.0) as usize;
        }
        let x_title = chart
            .x_title
            .as_ref()
            .filter(|s| !pie && !s.is_empty())
            .map_or_else(Vec::new, |s| measure.lines(s, width, false));
        if chart.legend == Legend::Bottom {
            legend = LegendBlock::new(chart, data, width, &mut measure);
        }
        let bottom = PAD
            + if pie { 0.0 } else { GAP + category_height }
            + if x_title.is_empty() {
                0.0
            } else {
                GAP + x_title.len() as f64 * LINE_HEIGHT
            }
            + if chart.legend == Legend::Bottom && !legend.items.is_empty() {
                GAP + legend.height
            } else {
                0.0
            }
            + if data.capped { GAP + LINE_HEIGHT } else { 0.0 };
        let height = (chart.h - top - bottom).max(24.0);
        if bar {
            y_ticks = horizontal_ticks(bounds, data.number_format.as_deref(), width, &mut measure);
            category_step = (count * (LINE_HEIGHT + GAP) / height).ceil().max(1.0) as usize;
        }
        let x_ticks = if scatter {
            horizontal_ticks(x_bounds, x_format.as_deref(), width, &mut measure)
        } else {
            Vec::new()
        };
        let y_title = chart
            .y_title
            .as_ref()
            .filter(|s| !pie && !s.is_empty())
            .map(|s| ellipsis(s, height, &mut measure));
        let legend_at = if chart.legend == Legend::Right {
            [left + width + GAP, top]
        } else {
            [
                left,
                top + height
                    + if pie { 0.0 } else { GAP + category_height }
                    + if x_title.is_empty() {
                        0.0
                    } else {
                        GAP + x_title.len() as f64 * LINE_HEIGHT
                    }
                    + GAP,
            ]
        };
        for item in &mut legend.items {
            item.at[0] += legend_at[0];
            item.at[1] += legend_at[1];
        }
        Self {
            plot: [left, top, width, height],
            bounds,
            x_bounds,
            y_ticks,
            x_ticks,
            x_format,
            categories,
            rotated,
            category_step,
            category_height,
            title,
            x_title,
            y_title,
            legend,
        }
    }
}

struct Drawing<'a> {
    out: String,
    chart: &'a Chart,
    data: &'a Data,
    theme: crate::deck::Theme,
    palette: Vec<String>,
    layout: Layout,
}

impl Drawing<'_> {
    fn text(&mut self, x: f64, y: f64, text: &str, anchor: &str) {
        let _ = write!(
            self.out,
            "<text x=\"{x}\" y=\"{y}\" text-anchor=\"{anchor}\">{}</text>",
            escape(text)
        );
    }

    fn line(&mut self, x: f64, y: f64, x2: f64, y2: f64, token: &str) {
        let _ = write!(
            self.out,
            "<path d=\"M{x} {y}L{x2} {y2}\" fill=\"none\" stroke=\"{}\"/>",
            color(&self.theme, token)
        );
    }

    fn title(&self, s: usize, p: usize, n: f64) -> String {
        format!(
            "<title>{}</title>",
            escape(&format!(
                "{} · {}: {}",
                self.data.series[s].name,
                self.data.labels[p],
                super::engine::format_number(n, self.data.number_format.as_deref())
            ))
        )
    }

    fn value(&self, n: f64) -> f64 {
        let fraction = (n - self.layout.bounds.0) / (self.layout.bounds.1 - self.layout.bounds.0);
        if self.chart.kind == ChartKind::Bar {
            fraction * self.layout.plot[2]
        } else {
            (1.0 - fraction) * self.layout.plot[3]
        }
    }

    fn category(&self, p: usize) -> f64 {
        let length = if self.chart.kind == ChartKind::Bar {
            self.layout.plot[3]
        } else {
            self.layout.plot[2]
        };
        (p as f64 + 0.5) * length / self.data.labels.len().max(1) as f64
    }

    fn axes(&mut self) {
        let [x, y, w, h] = self.layout.plot;
        let horizontal = self.chart.kind == ChartKind::Bar;
        for index in 0..self.layout.y_ticks.len() {
            let tick = self.layout.y_ticks[index];
            let v = self.value(tick);
            let label = super::engine::format_number(tick, self.data.number_format.as_deref());
            if horizontal {
                self.line(x + v, y, x + v, y + h, "border");
                self.text(x + v, y + h + GAP + FONT_SIZE, &label, "middle");
            } else {
                self.line(x, y + v, x + w, y + v, "border");
                self.text(x - GAP, y + v + FONT_SIZE / 3.0, &label, "end");
            }
        }
        self.line(x, y + h, x + w, y + h, "muted");
        self.line(x, y, x, y + h, "muted");
        if self.chart.kind == ChartKind::Scatter {
            let (min, max) = self.layout.x_bounds;
            for index in 0..self.layout.x_ticks.len() {
                let tick = self.layout.x_ticks[index];
                let tx = x + (tick - min) / (max - min) * w;
                self.text(
                    tx,
                    y + h + GAP + FONT_SIZE,
                    &super::engine::format_number(tick, self.layout.x_format.as_deref()),
                    "middle",
                );
            }
        } else {
            for p in (0..self.layout.categories.len()).step_by(self.layout.category_step) {
                let label = self.layout.categories[p].clone();
                let c = self.category(p);
                if horizontal {
                    self.text(x - GAP, y + c + FONT_SIZE / 3.0, &label, "end");
                } else if self.layout.rotated {
                    let _ = write!(
                        self.out,
                        "<text transform=\"translate({} {}) rotate(45)\">{}</text>",
                        x + c,
                        y + h + GAP + FONT_SIZE * std::f64::consts::FRAC_1_SQRT_2,
                        escape(&label)
                    );
                } else {
                    self.text(x + c, y + h + GAP + FONT_SIZE, &label, "middle");
                }
            }
        }
        for index in 0..self.layout.x_title.len() {
            let title = self.layout.x_title[index].clone();
            self.text(
                x + w / 2.0,
                y + h
                    + GAP
                    + self.layout.category_height
                    + GAP
                    + FONT_SIZE
                    + index as f64 * LINE_HEIGHT,
                &title,
                "middle",
            );
        }
        if let Some(title) = &self.layout.y_title {
            let _ = write!(
                self.out,
                "<text transform=\"translate({} {}) rotate(-90)\" text-anchor=\"middle\">{}</text>",
                PAD + FONT_SIZE,
                y + h / 2.0,
                escape(title)
            );
        }
    }

    fn legend(&mut self) {
        for index in 0..self.layout.legend.items.len() {
            let item = &self.layout.legend.items[index];
            let [x, y] = item.at;
            if y > self.chart.h - PAD {
                break;
            }
            let ink = &self.palette[item.index % self.palette.len()];
            let _ = write!(
                self.out,
                "<rect x=\"{x}\" y=\"{}\" width=\"{SWATCH}\" height=\"{SWATCH}\" fill=\"{ink}\"/>",
                y - SWATCH
            );
            let label = item.label.clone();
            self.text(x + SWATCH + GAP, y, &label, "start");
        }
    }

    fn bars(&mut self) {
        let horizontal = self.chart.kind == ChartKind::Bar;
        let band = if horizontal {
            self.layout.plot[3]
        } else {
            self.layout.plot[2]
        } / self.data.labels.len().max(1) as f64;
        let width = band * 0.76
            / if self.chart.stacked {
                1.0
            } else {
                self.data.series.len() as f64
            };
        let mut positive = vec![0.0; self.data.labels.len()];
        let mut negative = positive.clone();
        for (s, series) in self.data.series.iter().enumerate() {
            for (p, n) in series.values.iter().enumerate() {
                let Some(n) = n else { continue };
                let base = if self.chart.stacked {
                    if *n >= 0.0 {
                        &mut positive[p]
                    } else {
                        &mut negative[p]
                    }
                } else {
                    &mut 0.0
                };
                let v0 = self.value(*base);
                let v1 = self.value(*base + n);
                *base += n;
                let c = self.category(p) - band * 0.38
                    + if self.chart.stacked {
                        0.0
                    } else {
                        s as f64 * width
                    };
                let (x, y, w, h) = if horizontal {
                    (v0.min(v1), c, (v1 - v0).abs(), width)
                } else {
                    (c, v0.min(v1), width, (v1 - v0).abs())
                };
                let _ = write!(
                    self.out,
                    "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"{}\">{}</rect>",
                    self.palette[s % 8],
                    self.title(s, p, *n)
                );
            }
        }
    }

    fn paths(&mut self) {
        let (xmin, xmax) = self.layout.x_bounds;
        let mut positive = vec![0.0; self.data.labels.len()];
        let mut negative = positive.clone();
        for (s, series) in self.data.series.iter().enumerate() {
            let mut points: Vec<(usize, f64, f64, f64, f64)> = Vec::new();
            for p in 0..=series.values.len() {
                let n = series.values.get(p).copied().flatten();
                let x = if self.chart.kind == ChartKind::Scatter {
                    self.data
                        .x_values
                        .get(p)
                        .copied()
                        .flatten()
                        .map(|x| (x - xmin) / (xmax - xmin) * self.layout.plot[2])
                } else {
                    Some(self.category(p))
                };
                if let Some(n) = n
                    && let Some(x) = x
                {
                    let base = if self.chart.stacked {
                        if n >= 0.0 {
                            &mut positive[p]
                        } else {
                            &mut negative[p]
                        }
                    } else {
                        &mut 0.0
                    };
                    let bottom = self.value(*base);
                    let top = self.value(*base + n);
                    *base += n;
                    points.push((p, x, top, bottom, n));
                    continue;
                }
                if points.is_empty() {
                    continue;
                }
                let mut path = String::new();
                for (i, (_, x, y, _, _)) in points.iter().enumerate() {
                    let _ = write!(path, "{} {x} {y}", if i == 0 { "M" } else { "L" });
                }
                let ink = &self.palette[s % 8];
                if self.chart.kind == ChartKind::Area {
                    for (_, x, _, bottom, _) in points.iter().rev() {
                        let _ = write!(path, "L {x} {bottom}");
                    }
                    path.push('Z');
                    let _ = write!(
                        self.out,
                        "<path d=\"{path}\" fill=\"{ink}\" fill-opacity=\"0.35\"><title>{}</title></path>",
                        escape(&series.name)
                    );
                } else if self.chart.kind != ChartKind::Scatter {
                    let _ = write!(
                        self.out,
                        "<path d=\"{path}\" fill=\"none\" stroke=\"{ink}\" stroke-width=\"2\"><title>{}</title></path>",
                        escape(&series.name)
                    );
                }
                for (p, x, y, _, n) in &points {
                    let visible =
                        self.chart.kind == ChartKind::Scatter || self.data.labels.len() <= 30;
                    let _ = write!(
                        self.out,
                        "<circle cx=\"{x}\" cy=\"{y}\" r=\"{MARKER}\" fill=\"{ink}\" fill-opacity=\"{}\">{}</circle>",
                        u8::from(visible),
                        self.title(s, *p, *n)
                    );
                }
                points.clear();
            }
        }
    }

    fn pie(&mut self) {
        let Some(series) = self.data.series.first() else {
            return;
        };
        let total: f64 = series.values.iter().flatten().filter(|n| **n > 0.0).sum();
        if total <= 0.0 {
            self.text(self.chart.w / 2.0, self.chart.h / 2.0, "No data", "middle");
            return;
        }
        let [x, y, w, h] = self.layout.plot;
        let (cx, cy, r) = (x + w / 2.0, y + h / 2.0, (w.min(h) / 2.0 - 4.0).max(1.0));
        let mut measure = CellText::new(&self.theme);
        let bg = color(&self.theme, "bg");
        let fg = color(&self.theme, "fg");
        let mut angle = -std::f64::consts::FRAC_PI_2;
        for (p, n) in series.values.iter().enumerate() {
            let Some(n) = n.filter(|n| *n > 0.0) else {
                continue;
            };
            let fraction = n / total;
            let end = angle + fraction * std::f64::consts::TAU;
            let ink = &self.palette[p % 8];
            if fraction >= 1.0 - f64::EPSILON {
                let _ = write!(
                    self.out,
                    "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\" fill=\"{ink}\">{}</circle>",
                    self.title(0, p, n)
                );
            } else {
                let _ = write!(
                    self.out,
                    "<path d=\"M{cx} {cy}L{} {}A{r} {r} 0 {} 1 {} {}Z\" fill=\"{ink}\">{}</path>",
                    cx + r * angle.cos(),
                    cy + r * angle.sin(),
                    u8::from(fraction > 0.5),
                    cx + r * end.cos(),
                    cy + r * end.sin(),
                    self.title(0, p, n)
                );
            }
            let mid = angle.midpoint(end);
            let label = format!("{:.0}%", fraction * 100.0);
            if fraction >= 0.05
                && pie_label_fits(measure.width(&label, false), r, mid, (end - angle) / 2.0)
            {
                let light = luminance(ink);
                let contrast = |candidate: &str| {
                    let other = luminance(candidate);
                    (light.max(other) + 0.05) / (light.min(other) + 0.05)
                };
                let label_color = if contrast(&fg) > contrast(&bg) {
                    &fg
                } else {
                    &bg
                };
                let _ = write!(
                    self.out,
                    "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" fill=\"{label_color}\">{label}</text>",
                    cx + r * 0.68 * mid.cos(),
                    cy + r * 0.68 * mid.sin() + FONT_SIZE / 3.0
                );
            }
            angle = end;
        }
    }
}

/// Render one chart from calculated workbook values. All user text is XML escaped.
#[must_use]
pub fn svg(sheet: &Sheet, workbook: &Workbook, tab: &Tab, chart: &Chart) -> String {
    let data = super::chart::extract(sheet, workbook, tab, chart);
    let theme = super::render::theme(sheet);
    let palette = [
        color(&theme, "accent"),
        "#e69f00".into(),
        "#009e73".into(),
        "#cc79a7".into(),
        "#56b4e9".into(),
        "#d55e00".into(),
        "#b3a332".into(),
        "#8b78c5".into(),
    ]
    .to_vec();
    let font = theme.fonts.get("body").map_or("sans-serif", String::as_str);
    let font_size = super::layout::FONT_SIZE;
    let out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\"><rect width=\"100%\" height=\"100%\" fill=\"{}\"/><g font-family=\"{}\" font-size=\"{font_size}\" fill=\"{}\">",
        chart.w,
        chart.h,
        chart.w,
        chart.h,
        color(&theme, "bg"),
        escape(font),
        color(&theme, "fg")
    );
    let layout = Layout::new(
        chart,
        &data,
        &theme,
        x_format(sheet, workbook, tab, chart, &data),
    );
    let mut draw = Drawing {
        out,
        chart,
        data: &data,
        theme,
        palette,
        layout,
    };
    for index in 0..draw.layout.title.len() {
        let title = draw.layout.title[index].clone();
        draw.text(
            chart.w / 2.0,
            PAD + FONT_SIZE + index as f64 * LINE_HEIGHT,
            &title,
            "middle",
        );
    }
    if !data
        .series
        .iter()
        .any(|s| s.values.iter().any(Option::is_some))
    {
        draw.text(chart.w / 2.0, chart.h / 2.0, "No data", "middle");
    } else if chart.kind == ChartKind::Pie {
        draw.pie();
        draw.legend();
    } else {
        draw.axes();
        let [x, y, w, h] = draw.layout.plot;
        if chart.kind == ChartKind::Scatter {
            let _ = write!(
                draw.out,
                "<svg x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" viewBox=\"-{MARKER} -{MARKER} {} {}\" overflow=\"hidden\">",
                x - MARKER,
                y - MARKER,
                w + 2.0 * MARKER,
                h + 2.0 * MARKER,
                w + 2.0 * MARKER,
                h + 2.0 * MARKER
            );
        } else {
            let _ = write!(
                draw.out,
                "<svg x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" overflow=\"hidden\">"
            );
        }
        match chart.kind {
            ChartKind::Column | ChartKind::Bar => draw.bars(),
            _ => draw.paths(),
        }
        draw.out.push_str("</svg>");
        draw.legend();
    }
    if data.capped {
        draw.text(
            8.0,
            chart.h - 3.0,
            "Limited to 2,000 categories / 24 series",
            "start",
        );
    }
    draw.out.push_str("</g></svg>");
    draw.out
}
