//! Bounded component drawings. Child regions reserve every branch's height so
//! local navigation never moves following transcript content.

use super::{
    Control, Entry, accent_color, bold, branch_regions, clipped, control_style, display, glyph,
    tone_color,
};
use crate::theme::Theme;
use nexus_core::interactive::{ChartKind, ChartSeries};
use nexus_core::interactive::{Node, Session, Tone};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

pub(super) fn control_count(node: &Node) -> usize {
    match node {
        Node::Buttons { items, .. } => items.len(),
        Node::Checklist { items } => items.len(),
        Node::Accordion { items } => items.len(),
        Node::Input { .. }
        | Node::Toggle { .. }
        | Node::Select { .. }
        | Node::Stepper { .. }
        | Node::Tabs { .. }
        | Node::Form { .. }
        | Node::Rating { .. }
        | Node::Slider { .. }
        | Node::Code { .. }
        | Node::Editor { .. } => 1,
        Node::Choice { options, .. } => options.len(),
        Node::Quiz { .. } => 2,
        Node::Table {
            select,
            sortable,
            page_size,
            search,
            ..
        } => usize::from(select.is_some() || *sortable || page_size.is_some() || *search),
        _ => 0,
    }
}
fn branch_height(nodes: &[Node], session: &Session, width: u16) -> u16 {
    branch_regions(session, nodes, Rect::new(0, 0, width, u16::MAX))
        .last()
        .map_or(0, |area| area.bottom())
}
pub(super) fn height(node: &Node, session: &Session, width: u16) -> Option<u16> {
    if let Some(height) = super::practice::height(node, session, width) {
        return Some(height);
    }
    Some(match node {
        Node::Tabs { items, .. } => {
            1 + items
                .iter()
                .map(|item| branch_height(&item.view, session, width))
                .max()
                .unwrap_or(0)
        }
        Node::Accordion { items } => items
            .iter()
            .map(|item| 1 + branch_height(&item.view, session, width))
            .sum(),
        Node::Form { fields, .. } => 2 + branch_height(fields, session, width),
        Node::Chart { rows, .. } | Node::Canvas { rows, .. } => *rows,
        Node::Timeline { .. }
        | Node::Code { .. }
        | Node::Badge { .. }
        | Node::Meter { .. }
        | Node::Slider { .. }
        | Node::Rating { .. } => node.rows(),
        _ => return None,
    })
}
pub(super) fn image_count(node: &Node) -> usize {
    match node {
        Node::Image { .. } | Node::Hero { image: Some(_), .. } => 1,
        Node::Cards { items } => items.iter().filter(|item| item.image.is_some()).count(),
        Node::Buttons { items, .. } => items.iter().filter(|item| item.image.is_some()).count(),
        _ => 0,
    }
}
pub(super) fn visible_regions(session: &Session, width: u16) -> Vec<(&Node, Rect)> {
    fn visit<'a>(
        session: &'a Session,
        nodes: &'a [Node],
        area: Rect,
        out: &mut Vec<(&'a Node, Rect)>,
    ) {
        for (node, region) in nodes.iter().zip(branch_regions(session, nodes, area)) {
            out.push((node, region));
            let index = session.document().node_index(node);
            match node {
                Node::Tabs { items, .. } => {
                    if let Some(item) = items.get(session.tab(index)) {
                        visit(
                            session,
                            &item.view,
                            body(region, 1, region.height.saturating_sub(1)),
                            out,
                        );
                    }
                }
                Node::Accordion { items } => {
                    let mut offset = 0;
                    for (section, item) in items.iter().enumerate() {
                        let height = branch_height(&item.view, session, region.width);
                        if session.section_open(index, section) {
                            visit(session, &item.view, body(region, offset + 1, height), out);
                        }
                        offset += 1 + height;
                    }
                }
                Node::Form { fields, .. } => visit(
                    session,
                    fields,
                    body(region, 1, region.height.saturating_sub(2)),
                    out,
                ),
                Node::Quiz { .. } => {
                    if let Some((view, area)) = super::practice::quiz_view(node, session, region) {
                        visit(session, view, area, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    visit(
        session,
        session.document().nodes(),
        Rect::new(2, 2, width.saturating_sub(4), u16::MAX),
        &mut out,
    );
    out
}
fn row(area: Rect, offset: u16) -> Rect {
    Rect::new(area.x, area.y + offset, area.width, 1)
}
pub(super) fn color(tone: Tone, session: &Session, theme: &Theme) -> Color {
    tone_color(Some(tone), accent_color(session.document(), theme), theme)
}
fn body(area: Rect, offset: u16, height: u16) -> Rect {
    Rect::new(area.x, area.y + offset, area.width, height)
}

#[allow(clippy::too_many_lines)]
pub(super) fn render(
    node: &Node,
    session: &Session,
    area: Rect,
    theme: &Theme,
    (focus, control, can_send): (Option<usize>, usize, bool),
    buffer: &mut Buffer,
) -> Option<(usize, Vec<(Rect, usize)>)> {
    if let Some(hits) = super::practice::render(
        node,
        session,
        area,
        theme,
        (focus, control, can_send),
        buffer,
    ) {
        return Some((control_count(node), hits));
    }
    let index = session.document().node_index(node);
    let mut next = control + control_count(node);
    let mut hits = Vec::new();
    match node {
        Node::Tabs { items, .. } => {
            let labels: Vec<_> = items
                .iter()
                .map(|item| display(session, &item.label))
                .collect();
            let focused = focus == Some(control);
            let active = session.tab(index);
            for (tab, rect) in bold::tab_rects(&labels, row(area, 0))
                .into_iter()
                .enumerate()
            {
                if tab == active {
                    let fill = if focused {
                        theme.accent
                    } else {
                        accent_color(session.document(), theme)
                    };
                    bold::pill(
                        buffer,
                        (rect.x, rect.y),
                        rect.width,
                        &labels[tab],
                        (fill, theme.on(fill)),
                        true,
                    );
                } else {
                    Paragraph::new(clipped(&format!("  {}", labels[tab]), rect.width))
                        .style(Style::default().fg(theme.fg_dim))
                        .render(rect, buffer);
                }
            }
            hits.push((row(area, 0), control));
            if let Some(item) = items.get(session.tab(index)) {
                let regions = branch_regions(
                    session,
                    &item.view,
                    body(area, 1, area.height.saturating_sub(1)),
                );
                hits.extend(Entry::branch(
                    &item.view, session, &regions, theme, focus, can_send, buffer, &mut next,
                ));
            }
        }
        Node::Accordion { items } => {
            let mut offset = 0;
            for (section, item) in items.iter().enumerate() {
                let open = session.section_open(index, section);
                let header = row(area, offset);
                Paragraph::new(format!(
                    "{} {}",
                    if open {
                        glyph::EXPANDED
                    } else {
                        glyph::SELECTED
                    },
                    display(session, &item.title)
                ))
                .style(control_style(theme, focus == Some(control + section)))
                .render(header, buffer);
                hits.push((header, control + section));
                let height = branch_height(&item.view, session, area.width);
                if open {
                    let regions =
                        branch_regions(session, &item.view, body(area, offset + 1, height));
                    hits.extend(Entry::branch(
                        &item.view, session, &regions, theme, focus, can_send, buffer, &mut next,
                    ));
                }
                offset += 1 + height;
            }
        }
        Node::Form {
            title,
            fields,
            submit_label,
            action,
        } => {
            Paragraph::new(display(session, title))
                .style(Style::default().fg(theme.fg).add_modifier(Modifier::BOLD))
                .render(row(area, 0), buffer);
            let regions = branch_regions(
                session,
                fields,
                body(area, 1, area.height.saturating_sub(2)),
            );
            hits.extend(Entry::branch(
                fields, session, &regions, theme, focus, can_send, buffer, &mut next,
            ));
            let submit = row(area, area.height.saturating_sub(1));
            let enabled =
                session.form_valid(index) && (can_send || !session.document().sends(action));
            let label = format!(
                "{}{}",
                display(session, submit_label),
                if session.document().sends(action) {
                    " ↵"
                } else {
                    ""
                }
            );
            let kind = if enabled {
                bold::Kind::Primary
            } else {
                bold::Kind::Disabled
            };
            let submit = bold::button(
                buffer,
                (submit.x, submit.y),
                submit.width,
                &label,
                (kind, focus == Some(control)),
                accent_color(session.document(), theme),
                theme,
            );
            hits.push((submit, control));
        }
        Node::Chart {
            kind,
            series,
            labels,
            ..
        } => chart(*kind, series, labels, session, area, theme, buffer),
        Node::Canvas { shapes, .. } => super::canvas::draw(shapes, session, area, theme, buffer),
        Node::Timeline { items } => {
            let mut y = 0;
            for item in items {
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("{} {} ", glyph::DOT, display(session, &item.time)),
                        Style::default().fg(color(item.tone, session, theme)),
                    ),
                    Span::styled(display(session, &item.title), Style::default().fg(theme.fg)),
                ]))
                .render(row(area, y), buffer);
                y += 1;
                if let Some(text) = &item.text {
                    Paragraph::new(format!("│ {}", display(session, text)))
                        .style(Style::default().fg(theme.fg_dim))
                        .render(row(area, y), buffer);
                    y += 1;
                }
            }
        }
        Node::Rating { key, label, max } => {
            let value = session.selected_row(key);
            let stars = rating_rect(label, *max, session, area);
            bold::label(
                buffer,
                Rect::new(area.x, area.y, stars.x - area.x, 1),
                &display(session, label),
                focus == Some(control),
                theme,
            );
            for star in 0..stars.width {
                let lit = usize::from(star) < value;
                buffer.set_string(
                    stars.x + star,
                    stars.y,
                    if lit { glyph::FAVORITE } else { "☆" },
                    Style::default().fg(if lit { theme.warning } else { theme.fg_dim }),
                );
            }
            hits.push((stars, control));
        }
        Node::Code { source, lang } => {
            let lang = lang.as_deref().unwrap_or("plain");
            bold::label(buffer, row(area, 0), lang, false, theme);
            let copy = bold::pill_width("Copy");
            bold::button(
                buffer,
                (area.right().saturating_sub(copy), area.y),
                copy,
                "Copy",
                (bold::Kind::Secondary, focus == Some(control)),
                accent_color(session.document(), theme),
                theme,
            );
            let fence = "`".repeat(
                source
                    .split(|ch| ch != '`')
                    .map(str::len)
                    .max()
                    .unwrap_or(0)
                    .max(3)
                    + 1,
            );
            let highlight_lang =
                if lang.eq_ignore_ascii_case("nexus-ui") || lang.eq_ignore_ascii_case("mermaid") {
                    "plain"
                } else {
                    lang
                };
            let rendered = crate::ui::markdown::render(
                &format!("{fence}{highlight_lang}\n{source}\n{fence}"),
                source
                    .lines()
                    .map(UnicodeWidthStr::width)
                    .max()
                    .unwrap_or(1)
                    .max(1)
                    .saturating_add(4),
                crate::ui::markdown::MdColors {
                    heading: theme.accent,
                    rule: theme.border_dim,
                    code: theme.fg,
                    syntax: crate::ui::style::SyntaxColors::from_theme(theme),
                },
            );
            let lines: Vec<_> = rendered
                .lines
                .into_iter()
                .skip(1)
                .take(source.lines().count().max(1))
                .map(|line| {
                    let count = line.spans.len().saturating_sub(2);
                    Line::from(
                        line.spans
                            .into_iter()
                            .skip(1)
                            .take(count)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            Paragraph::new(lines).render(body(area, 1, area.height.saturating_sub(1)), buffer);
            hits.push((row(area, 0), control));
        }
        Node::Badge { text, tone } => {
            bold::pill(
                buffer,
                (area.x, area.y),
                area.width,
                &display(session, text),
                bold::tone_fill(*tone, session, theme),
                true,
            );
        }
        Node::Meter {
            label,
            value,
            max,
            tone,
        } => {
            let (value, max) = session.progress(value, max).unwrap_or_default();
            let text = super::ratio_text(value, max);
            let fill = match tone {
                Tone::Normal => super::accent_color(session.document(), theme),
                tone => color(*tone, session, theme),
            };
            super::render_bar(
                &super::BarRow {
                    label: &display(session, label),
                    caps: true,
                    value: &text,
                    value_width: u16::try_from(text.width()).unwrap_or(0),
                    ratio: if max > 0.0 { value / max } else { 0.0 },
                    fill,
                },
                session,
                area,
                theme,
                buffer,
            );
        }
        Node::Slider {
            key,
            label,
            min,
            max,
            ..
        } => {
            let value = session.value(key);
            let ratio = (value.parse::<f64>().unwrap_or(*min) - min) / (max - min);
            let focused = focus == Some(control);
            let track = slider_track(label, &value, session, area);
            bold::label(
                buffer,
                Rect::new(area.x, area.y, track.x - area.x, 1),
                &display(session, label),
                focused,
                theme,
            );
            let fill = if focused {
                theme.accent
            } else {
                accent_color(session.document(), theme)
            };
            bold::bar(buffer, track, ratio, fill, theme);
            bold::value(
                buffer,
                Rect::new(track.right(), area.y, area.right() - track.right(), 1),
                &value,
                theme,
            );
            hits.push((track, control));
        }
        _ => return None,
    }
    Some((next - control, hits))
}
/// The slider's bar, between the label column and its value.
fn slider_track(label: &str, value: &str, session: &Session, area: Rect) -> Rect {
    let column = if display(session, label).is_empty() {
        0
    } else {
        bold::label_column(session, area.width)
    };
    let value_width = u16::try_from(value.width()).unwrap_or(0).saturating_add(2);
    Rect::new(
        area.x + column,
        area.y,
        area.width.saturating_sub(column + value_width),
        1,
    )
}
/// The stars of a rating, after the label column.
fn rating_rect(label: &str, max: u16, session: &Session, area: Rect) -> Rect {
    let column = if display(session, label).is_empty() {
        0
    } else {
        bold::label_column(session, area.width)
    };
    Rect::new(
        area.x + column.min(area.width),
        area.y,
        max.min(area.width.saturating_sub(column)),
        1,
    )
}

pub(super) fn click(entry: &mut Entry, control: &Control, pos: Position, area: Rect) -> bool {
    let Ok(session) = &mut entry.session else {
        return true;
    };
    let result = match control {
        Control::Tabs(node) => {
            let Some(Node::Tabs { items, .. }) = session.document().node(*node) else {
                return true;
            };
            let labels: Vec<_> = items
                .iter()
                .map(|item| display(session, &item.label))
                .collect();
            let rects = bold::tab_rects(&labels, area);
            let tab = rects
                .iter()
                .position(|rect| pos.x < rect.right())
                .unwrap_or(rects.len().saturating_sub(1));
            session.select_tab(*node, tab)
        }
        Control::Rating(node) => {
            let Some(Node::Rating { max, .. }) = session.document().node(*node) else {
                return true;
            };
            // `area` is the stars' own hit rect.
            let value = pos.x.saturating_sub(area.x).saturating_add(1).min(*max);
            session.rate(*node, value)
        }
        Control::Slider(node) => session.slide(
            *node,
            f64::from(pos.x - area.x) / f64::from(area.width.saturating_sub(1).max(1)),
        ),
        _ => return false,
    };
    entry.error = result
        .err()
        .map_or_else(String::new, |error| error.to_string());
    true
}

pub(super) fn click_table(
    session: &mut Session,
    node: usize,
    row: u16,
    x: u16,
    area: Rect,
) -> anyhow::Result<()> {
    let Some(Node::Table {
        columns,
        sortable,
        page_size,
        search,
        select,
        ..
    }) = session.document().node(node)
    else {
        return Ok(());
    };
    if *search && row == 0 {
        return session.search_table(node, session.table_state(node).query, true);
    }
    let header = u16::from(*search);
    if row == header && *sortable {
        let lead = u16::from(select.is_some());
        let column = usize::from(x.saturating_sub(lead)) * columns.len()
            / usize::from(area.width.saturating_sub(lead).max(1));
        return session.sort_table(node, column.min(columns.len() - 1));
    }
    if page_size.is_some() && row + 1 == session.document().node(node).expect("table node").rows() {
        return session.page_table(node, x >= area.width / 2);
    }
    if row > header && select.is_some() {
        let rows = session.table_rows(node);
        let size = page_size.unwrap_or(rows.len()).max(1);
        let page = session
            .table_state(node)
            .page
            .min(rows.len().div_ceil(size).saturating_sub(1));
        if let Some(selected) = rows.get(page * size + usize::from(row - header - 1)) {
            return session.select_row(node, *selected);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn chart(
    kind: ChartKind,
    series: &[ChartSeries],
    labels: &[String],
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let values: Vec<Vec<f64>> = series
        .iter()
        .map(|series| {
            series
                .values
                .iter()
                .map(|value| session.number(value).unwrap_or_default())
                .collect()
        })
        .collect();
    let tones: Vec<_> = series
        .iter()
        .map(|item| color(item.tone, session, theme))
        .collect();
    let legend = Line::from(
        series
            .iter()
            .enumerate()
            .map(|(index, item)| {
                Span::styled(
                    format!(
                        "{} {}{} ",
                        glyph::DOT,
                        display(session, &item.name),
                        if kind == ChartKind::Pie {
                            format!(
                                " {}",
                                values[index]
                                    .iter()
                                    .map(|value| value.max(0.0))
                                    .sum::<f64>()
                            )
                        } else {
                            String::new()
                        }
                    ),
                    Style::default().fg(tones[index]),
                )
            })
            .collect::<Vec<_>>(),
    );
    Paragraph::new(legend).render(row(area, 0), buffer);
    if area.width < 16 || area.height < 4 {
        for (index, (item, values)) in series
            .iter()
            .zip(&values)
            .enumerate()
            .take(usize::from(area.height.saturating_sub(1)))
        {
            Paragraph::new(format!(
                "{}: {}",
                display(session, &item.name),
                values.iter().sum::<f64>()
            ))
            .style(Style::default().fg(tones[index]))
            .render(row(area, u16::try_from(index).unwrap_or(0) + 1), buffer);
        }
        return;
    }
    let plot = Rect::new(area.x + 6, area.y + 1, area.width - 6, area.height - 2);
    if kind == ChartKind::Pie {
        ring(&values, &tones, plot, buffer);
    } else {
        let min = values.iter().flatten().copied().fold(0.0, f64::min);
        let max = values
            .iter()
            .flatten()
            .copied()
            .fold(0.0, f64::max)
            .max(min + f64::EPSILON);
        Paragraph::new(clipped(&format!("{max}"), 5))
            .style(Style::default().fg(theme.fg_dim))
            .render(Rect::new(area.x, plot.y, 5, 1), buffer);
        Paragraph::new(clipped(&format!("{min}"), 5))
            .style(Style::default().fg(theme.fg_dim))
            .render(Rect::new(area.x, plot.bottom() - 1, 5, 1), buffer);
        let samples = values.iter().map(Vec::len).max().unwrap_or(1);
        // Lines and points go on one braille grid shared by every series.
        let mut dots = super::canvas::Dots::new(plot.width, plot.height);
        for (index, values) in values.iter().enumerate() {
            if kind != ChartKind::Bar {
                let points = braille_points(values, samples, (min, max), &dots);
                if kind == ChartKind::Scatter {
                    // A 2×2 block that never straddles cells stays crisp.
                    for &(x, y) in &points {
                        let x = ((x / 2.0).floor() * 2.0).min(dots.width() - 2.0);
                        let y = ((y / 2.0).floor() * 2.0).min(dots.height() - 2.0);
                        for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                            dots.set(x + dx, y + dy, tones[index]);
                        }
                    }
                    continue;
                }
                for pair in points.windows(2) {
                    dots.line(pair[0], pair[1], tones[index]);
                }
                if let [point] = points[..] {
                    dots.set(point.0, point.1, tones[index]);
                }
                if kind == ChartKind::Area
                    && let (Some(first), Some(last)) = (points.first(), points.last())
                {
                    let zero = (max / (max - min)) * (dots.height() - 1.0);
                    let mut outline = points.clone();
                    outline.extend([(last.0, zero), (first.0, zero)]);
                    dots.shade(&outline, tones[index]);
                }
                continue;
            }
            for x in 0..plot.width {
                let sample = f64::from(x)
                    * f64::from(u16::try_from(samples.saturating_sub(1)).unwrap_or(0))
                    / f64::from(plot.width.saturating_sub(1).max(1));
                let nearest = (0..samples)
                    .min_by(|a, b| {
                        (f64::from(u16::try_from(*a).unwrap_or(0)) - sample)
                            .abs()
                            .total_cmp(&(f64::from(u16::try_from(*b).unwrap_or(0)) - sample).abs())
                    })
                    .unwrap_or(0);
                if nearest >= values.len() {
                    continue;
                }
                let value = values[nearest];
                let level = (value - min) / (max - min) * f64::from(plot.height.saturating_sub(1));
                let baseline = (-min / (max - min)) * f64::from(plot.height.saturating_sub(1));
                for y in 0..plot.height {
                    let pos = f64::from(plot.height - 1 - y);
                    let fill = pos >= baseline.min(level) && pos <= baseline.max(level);
                    if fill && usize::from(x) % series.len().max(1) == index {
                        buffer.set_string(
                            plot.x + x,
                            plot.y + y,
                            glyph::SECTION,
                            Style::default().fg(tones[index]),
                        );
                    }
                }
            }
        }
        dots.render(plot, buffer);
    }
    let first = labels
        .first()
        .map_or_else(|| "0".into(), |s| display(session, s));
    let last = labels.last().map_or_else(
        || {
            values
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(1)
                .saturating_sub(1)
                .to_string()
        },
        |s| display(session, s),
    );
    let axis = body(area, area.height - 1, 1);
    Paragraph::new(format!("      {}", clipped(&first, plot.width / 2)))
        .style(Style::default().fg(theme.fg_dim))
        .render(axis, buffer);
    Paragraph::new(clipped(&last, plot.width / 2))
        .right_aligned()
        .style(Style::default().fg(theme.fg_dim))
        .render(axis, buffer);
}
/// Samples as braille dot positions, spread evenly across the plot.
fn braille_points(
    values: &[f64],
    samples: usize,
    (min, max): (f64, f64),
    dots: &super::canvas::Dots,
) -> Vec<(f64, f64)> {
    let span = f64::from(u16::try_from(samples.saturating_sub(1)).unwrap_or(u16::MAX)).max(1.0);
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let x = f64::from(u16::try_from(index).unwrap_or(u16::MAX)) / span;
            let y = 1.0 - (value - min) / (max - min);
            (x * (dots.width() - 1.0), y * (dots.height() - 1.0))
        })
        .collect()
}
fn ring(values: &[Vec<f64>], colors: &[Color], area: Rect, buffer: &mut Buffer) {
    let width = area.width.min(area.height.saturating_mul(2));
    let area = Rect::new(
        area.x + (area.width - width) / 2,
        area.y,
        width,
        area.height,
    );
    let totals: Vec<_> = values
        .iter()
        .map(|values| values.iter().map(|v| v.max(0.0)).sum::<f64>())
        .collect();
    let total: f64 = totals.iter().sum();
    if total <= 0.0 {
        return;
    }
    for y in 0..area.height {
        for x in 0..area.width {
            let dx = (2.0 * f64::from(x) + 1.0 - f64::from(area.width)) / f64::from(area.width);
            let dy = (2.0 * f64::from(y) + 1.0 - f64::from(area.height)) / f64::from(area.height);
            let radius = dx.hypot(dy);
            if !(0.45..=1.0).contains(&radius) {
                continue;
            }
            let angle = (dy.atan2(dx) + std::f64::consts::PI) / std::f64::consts::TAU * total;
            let mut cumulative = 0.0;
            let slice = totals
                .iter()
                .position(|value| {
                    cumulative += value;
                    angle < cumulative
                })
                .unwrap_or(totals.len() - 1);
            buffer.set_string(
                area.x + x,
                area.y + y,
                glyph::SECTION,
                Style::default().fg(colors[slice]),
            );
        }
    }
}
