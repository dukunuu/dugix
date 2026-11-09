//! Drawing for Number, Gauge, Heatmap, Md, Tree, Board and Diagram.

use super::{Session, accent_color, bold, canvas::Dots, clipped, control_style, display, glyph};
use crate::theme::Theme;
use nexus_core::interactive::{Node, Stat, Tone, tree_branches};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

/// Shade glyphs for heatmap quartiles, lightest first.
const SHADES: [&str; 4] = ["░", "▒", "▓", "█"];

fn fill(tone: Tone, session: &Session, theme: &Theme) -> Color {
    match tone {
        Tone::Normal => accent_color(session.document(), theme),
        tone => super::tone_color(Some(tone), accent_color(session.document(), theme), theme),
    }
}
fn number_lines(node: &Node, session: &Session, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let Node::Number {
        value,
        label,
        delta,
        tone,
    } = node
    else {
        return Vec::new();
    };
    let stat = Stat {
        value: value.clone(),
        label: label.clone(),
        delta: delta.clone(),
        tone: *tone,
    };
    super::stat_lines(&stat, session, (width, true), theme)
}
fn md_lines(
    text: &str,
    rows: u16,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = crate::ui::markdown::render(
        &session.text(text).unwrap_or_default(),
        usize::from(width),
        crate::ui::markdown::MdColors {
            heading: accent_color(session.document(), theme),
            rule: theme.border_dim,
            code: theme.fg,
            syntax: crate::ui::style::SyntaxColors::from_theme(theme),
        },
    )
    .lines;
    lines.truncate(usize::from(rows));
    lines
}

/// Height at `width`, for the components drawn here.
pub(super) fn height(node: &Node, session: &Session, width: u16) -> Option<u16> {
    let rows = |count: usize| u16::try_from(count).unwrap_or(u16::MAX);
    Some(match node {
        Node::Number { .. } => rows(number_lines(node, session, width, &Theme::default()).len()),
        Node::Md { text, rows: limit } => {
            rows(md_lines(text, *limit, session, width, &Theme::default()).len()).max(1)
        }
        Node::Gauge { .. } | Node::Heatmap { .. } | Node::Board { .. } | Node::Diagram { .. } => {
            node.rows()
        }
        // Reserve the fully expanded tree so expanding never moves the
        // transcript.
        Node::Tree { items } => rows(tree_branches(items).len()),
        _ => return None,
    })
}

/// Draw one of these components; `None` for anything else.
pub(super) fn render(
    node: &Node,
    session: &Session,
    area: Rect,
    theme: &Theme,
    (focus, control): (Option<usize>, usize),
    buffer: &mut Buffer,
) -> Option<Vec<(Rect, usize)>> {
    let mut hits = Vec::new();
    match node {
        Node::Number { .. } => {
            Paragraph::new(number_lines(node, session, area.width, theme)).render(area, buffer);
        }
        Node::Md { text, rows } => {
            Paragraph::new(md_lines(text, *rows, session, area.width, theme)).render(area, buffer);
        }
        Node::Gauge {
            label,
            value,
            max,
            tone,
        } => gauge(
            (
                session.document().node_index(node),
                &display(session, label),
                value,
                max,
            ),
            fill(*tone, session, theme),
            session,
            area,
            theme,
            buffer,
        ),
        Node::Heatmap {
            values,
            labels,
            tone,
        } => heatmap(
            values,
            labels,
            fill(*tone, session, theme),
            session,
            area,
            theme,
            buffer,
        ),
        Node::Tree { items } => {
            hits = tree(
                (session.document().node_index(node), items),
                session,
                area,
                theme,
                (focus, control),
                buffer,
            );
        }
        Node::Board { items, rows } => {
            let count = u16::try_from(items.len()).unwrap_or(1).max(1);
            let lane_width = area.width.saturating_sub(2 * (count - 1)) / count;
            for (column, lane) in items.iter().enumerate() {
                let x = area.x + u16::try_from(column).unwrap_or(0) * (lane_width + 2);
                let lane_area = Rect::new(x, area.y, lane_width, rows + 2);
                board_lane(lane, *rows, session, lane_area, theme, buffer);
            }
        }
        Node::Diagram { alt, .. } => {
            // Painted over with the rendered image where the terminal can
            // show one; otherwise this placeholder stays.
            let text = if alt.is_empty() {
                "diagram".to_string()
            } else {
                alt.clone()
            };
            Paragraph::new(format!(
                "{} {}",
                glyph::RESEARCH,
                clipped(&text, area.width.saturating_sub(2))
            ))
            .style(Style::default().fg(theme.fg_dim))
            .render(Rect::new(area.x, area.y, area.width, 1), buffer);
        }
        _ => return None,
    }
    Some(hits)
}

/// Visible branches packed from the top; expandable ones are controls.
fn tree(
    (index, items): (usize, &[nexus_core::interactive::Branch]),
    session: &Session,
    area: Rect,
    theme: &Theme,
    (focus, control): (Option<usize>, usize),
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    let mut hits = Vec::new();
    let mut y = area.y;
    let mut rank = 0;
    for (branch, (depth, item)) in tree_branches(items).into_iter().enumerate() {
        if !session.branch_visible(index, branch) {
            continue;
        }
        if y >= area.bottom() {
            break;
        }
        let row = Rect::new(area.x, y, area.width, 1);
        let indent = "  ".repeat(depth);
        let parent = !item.items.is_empty();
        let focused = parent && focus == Some(control + rank);
        let (marker, marker_style) = if parent {
            (
                if session.section_open(index, branch) {
                    glyph::EXPANDED
                } else {
                    glyph::SELECTED
                },
                Style::default().fg(accent_color(session.document(), theme)),
            )
        } else {
            ("·", Style::default().fg(theme.fg_dim))
        };
        buffer.set_stringn(
            row.x,
            y,
            format!("{indent}{marker} "),
            usize::from(row.width),
            marker_style,
        );
        let offset = u16::try_from(indent.width() + 2)
            .unwrap_or(u16::MAX)
            .min(row.width);
        Paragraph::new(clipped(&display(session, &item.label), row.width - offset))
            .style(control_style(theme, focused))
            .render(Rect::new(row.x + offset, y, row.width - offset, 1), buffer);
        if parent {
            hits.push((row, control + rank));
            rank += 1;
        }
        y += 1;
    }

    hits
}

/// A half-ring in braille with the value in its middle and the label below.
fn gauge(
    (index, label, value, max): (usize, &str, &serde_json::Value, &serde_json::Value),
    fill: Color,
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    if area.height < 2 || area.width < 4 {
        return;
    }
    let (value, max) = session.progress(value, max).unwrap_or_default();
    let ratio = session.eased((index, 0), if max > 0.0 { value / max } else { 0.0 });
    let ring_rows = area.height - 1;
    // Square dots: a ring as wide as it is tall needs twice the rows in cells.
    let cols = (ring_rows * 4).min(area.width);
    let ring = Rect::new(area.x + (area.width - cols) / 2, area.y, cols, ring_rows);
    let mut dots = Dots::new(ring.width, ring.height);
    let center = ((dots.width() - 1.0) / 2.0, dots.height() - 1.0);
    let radius = (dots.height() - 1.0).min((dots.width() - 1.0) / 2.0);
    for thickness in [0.0, 1.0, 2.0] {
        let r = radius - thickness;
        dots.arc(center, r, 270.0, 180.0, theme.border_dim);
        dots.arc(center, r, 270.0, 180.0 * ratio.clamp(0.0, 1.0), fill);
    }
    dots.render(ring, buffer);
    let text = super::ratio_text(value, max);
    let width = u16::try_from(text.width()).unwrap_or(0).min(ring.width);
    buffer.set_stringn(
        ring.x + (ring.width - width) / 2,
        ring.bottom() - 1,
        &text,
        usize::from(width),
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    );
    let label = clipped(&label.to_uppercase(), area.width);
    let label_width = u16::try_from(label.width()).unwrap_or(0);
    buffer.set_string(
        area.x + (area.width.saturating_sub(label_width)) / 2,
        area.bottom() - 1,
        label,
        Style::default().fg(theme.fg_dim),
    );
}

#[allow(clippy::too_many_arguments)]
fn heatmap(
    values: &[Vec<serde_json::Value>],
    labels: &[String],
    fill: Color,
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    let numbers: Vec<Vec<f64>> = values
        .iter()
        .map(|row| {
            row.iter()
                .map(|v| session.number(v).unwrap_or(0.0))
                .collect()
        })
        .collect();
    let max = numbers.iter().flatten().copied().fold(0.0, f64::max);
    let labels: Vec<_> = labels.iter().map(|label| display(session, label)).collect();
    let label_width = labels
        .iter()
        .map(|label| u16::try_from(label.width()).unwrap_or(0))
        .max()
        .map_or(0, |width| (width + 1).min(area.width / 3));
    let shown = bold::revealed(area.width.saturating_sub(label_width), session);
    for (row, cells) in numbers.iter().enumerate() {
        let y = area.y + u16::try_from(row).unwrap_or(0);
        if y >= area.bottom() {
            break;
        }
        if let Some(label) = labels.get(row) {
            buffer.set_stringn(
                area.x,
                y,
                label,
                usize::from(label_width),
                Style::default().fg(theme.fg_dim),
            );
        }
        for (column, value) in cells.iter().enumerate() {
            let x = area.x + label_width + u16::try_from(column * 2).unwrap_or(u16::MAX);
            if x + 2 > area.x + label_width + shown {
                break;
            }
            let (shade, color) = if *value <= 0.0 || max <= 0.0 {
                (glyph::TRACK, theme.border_dim)
            } else {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let level = ((value / max) * 4.0).ceil().clamp(1.0, 4.0) as usize - 1;
                (SHADES[level], fill)
            };
            buffer.set_string(x, y, shade.repeat(2), Style::default().fg(color));
        }
    }
}

fn board_lane(
    lane: &nexus_core::interactive::Lane,
    rows: u16,
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    let cards: Vec<String> = session
        .evaluate(&lane.items)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|card| super::safe(&nexus_core::interactive::scalar_text(card)))
        .collect();
    let color = lane.tone.map_or_else(
        || accent_color(session.document(), theme),
        |tone| fill(tone, session, theme),
    );
    let title = clipped(
        &display(session, &lane.title).to_uppercase(),
        area.width.saturating_sub(6),
    );
    buffer.set_string(
        area.x,
        area.y,
        &title,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    );
    let count = cards.len().to_string();
    let title_width = u16::try_from(title.width()).unwrap_or(0);
    bold::pill(
        buffer,
        (area.x + title_width + 1, area.y),
        area.width.saturating_sub(title_width + 1),
        &count,
        (theme.control, theme.fg),
        false,
    );
    buffer.set_string(
        area.x,
        area.y + 1,
        "─".repeat(usize::from(area.width)),
        Style::default().fg(theme.border_dim),
    );
    let room = usize::from(rows);
    for (index, card) in cards.iter().enumerate().take(room) {
        let y = area.y + 2 + u16::try_from(index).unwrap_or(0);
        let overflow = index + 1 == room && cards.len() > room;
        let text = if overflow {
            format!("+{} more", cards.len() - room + 1)
        } else {
            card.clone()
        };
        let surface = Style::default().bg(theme.raised);
        buffer.set_style(Rect::new(area.x, y, area.width, 1), surface);
        buffer.set_string(
            area.x,
            y,
            glyph::RAIL,
            Style::default().fg(color).bg(theme.raised),
        );
        buffer.set_stringn(
            area.x + 2,
            y,
            clipped(&text, area.width.saturating_sub(3)),
            usize::from(area.width.saturating_sub(3)),
            if overflow {
                surface.fg(theme.fg_dim)
            } else {
                surface.fg(theme.fg)
            },
        );
    }
}
