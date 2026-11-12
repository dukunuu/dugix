use super::layout::{accent_color, tone_color, wrapped_text};
use super::widgets::safe;
use super::{bold, image_lines};
use crate::theme::Theme;
use crate::ui::style::glyph;
use nexus_core::interactive::{
    CalloutTone, Card, Column, Mark, Node, Session, Stat, StepState, Tone,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Sparkline, Widget};
use unicode_width::UnicodeWidthStr;

fn styled_text(session: &Session, text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    wrapped_text(&session.text(text).unwrap_or_default(), width, style)
}
fn join_columns(columns: &[Vec<Line<'static>>], widths: &[u16], gap: u16) -> Vec<Line<'static>> {
    let height = columns.iter().map(Vec::len).max().unwrap_or(0);
    (0..height)
        .map(|row| {
            let mut spans = Vec::new();
            for (index, column) in columns.iter().enumerate() {
                let line = column.get(row).cloned().unwrap_or_default();
                let used = line.width();
                let style = line.style;
                spans.extend(
                    line.spans
                        .into_iter()
                        .map(|span| Span::styled(span.content, style.patch(span.style))),
                );
                if index + 1 < columns.len() {
                    let cell = usize::from(widths.get(index).copied().unwrap_or(0) + gap);
                    spans.push(Span::raw(" ".repeat(cell.saturating_sub(used))));
                }
            }
            Line::from(spans)
        })
        .collect()
}
fn prefixed(lines: Vec<Line<'static>>, prefix: &str, style: Style) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let line_style = line.style;
            let mut spans = vec![Span::styled(prefix.to_string(), style)];
            spans.extend(
                line.spans
                    .into_iter()
                    .map(|span| Span::styled(span.content, line_style.patch(span.style))),
            );
            Line::from(spans)
        })
        .collect()
}
fn columns_lines(
    items: &[Column],
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let stacked = width < 56;
    let count = u16::try_from(items.len()).unwrap_or(1);
    let cell = if stacked {
        width
    } else {
        width.saturating_sub(3 * (count - 1)) / count
    };
    let accent = accent_color(session.document(), theme);
    let columns: Vec<_> = items
        .iter()
        .map(|column| {
            let mut lines = styled_text(
                session,
                &column.title,
                cell,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            );
            for item in &column.items {
                let (mark, color) = match item.mark {
                    Mark::Ok => ("✓ ", theme.success),
                    Mark::No => ("✗ ", theme.error),
                    Mark::Dot => ("● ", accent),
                    Mark::None => ("", theme.fg),
                };
                let body = styled_text(
                    session,
                    &item.text,
                    cell.saturating_sub(u16::try_from(mark.width()).unwrap_or(0)),
                    Style::default().fg(theme.fg),
                );
                for (index, line) in body.into_iter().enumerate() {
                    let prefix = if index == 0 {
                        mark.to_string()
                    } else {
                        " ".repeat(mark.width())
                    };
                    lines.extend(prefixed(vec![line], &prefix, Style::default().fg(color)));
                }
            }
            lines
        })
        .collect();
    if stacked {
        let mut lines = Vec::new();
        for column in columns {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(column);
        }
        lines
    } else {
        join_columns(&columns, &vec![cell; columns.len()], 3)
    }
}
fn hero_lines(
    title: &str,
    subtitle: Option<&str>,
    badge: Option<&str>,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let accent = accent_color(session.document(), theme);
    let badge = badge.map(|s| display(session, s));
    let badge_width = badge
        .as_ref()
        .map_or(0, |s| bold::pill_width(s).saturating_add(2));
    let mut lines = styled_text(
        session,
        title,
        width.saturating_sub(badge_width).max(1),
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    );
    if let Some(badge) = badge
        && let Some(last) = lines.last_mut()
    {
        last.spans.push(Span::raw("  "));
        last.spans.extend(bold::pill_spans(
            &clipped(&badge, width.saturating_sub(6)),
            (accent, theme.on(accent)),
            (theme.surface, theme.round_caps),
        ));
    }
    if let Some(subtitle) = subtitle {
        lines.extend(styled_text(
            session,
            subtitle,
            width,
            Style::default().fg(theme.fg_dim),
        ));
    }
    lines
}
/// A card's title and badge rows, above its image.
pub(super) fn card_head(
    item: &Card,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(4);
    let accent = accent_color(session.document(), theme);
    let color = tone_color(item.tone, accent, theme);
    let mut content = styled_text(
        session,
        &item.title,
        inner,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    );
    if let Some(badge) = &item.badge {
        let badge = display(session, badge);
        let badge_width = bold::pill_width(&badge);
        let pill = bold::pill_spans(
            &clipped(&badge, inner.saturating_sub(4)),
            (color, theme.on(color)),
            (theme.raised, theme.round_caps),
        );
        // On the title row when both fit, so a badge costs no height.
        let single_title = content.len() == 1;
        match content.first_mut() {
            Some(title)
                if single_title
                    && title.width() + usize::from(badge_width) < usize::from(inner) =>
            {
                let gap = usize::from(inner) - title.width() - usize::from(badge_width);
                title.spans.push(Span::raw(" ".repeat(gap)));
                title.spans.extend(pill);
            }
            _ => content.push(Line::from(pill)),
        }
    }
    content
}
/// One bordered card, padded to at least `rows` content rows so a grid row lines up.
pub(super) fn card_lines(
    item: &Card,
    session: &Session,
    width: u16,
    theme: &Theme,
    rows: usize,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(4);
    let mut content = card_head(item, session, width, theme);
    if let Some(image) = &item.image {
        content.extend(image_lines(
            image,
            item.alt.as_deref().unwrap_or_default(),
            inner,
            6,
            theme,
        ));
    }
    content.push(Line::default());
    content.extend(styled_text(
        session,
        &item.text,
        inner,
        Style::default().fg(theme.fg),
    ));
    content.resize(content.len().max(rows), Line::default());
    // A raised tile with a tone rail: the rail still groups the card when a
    // transparent background leaves `raised` unpainted.
    let rail_color = tone_color(item.tone, accent_color(session.document(), theme), theme);
    let tile = Style::default().bg(theme.raised);
    let rail = Style::default().fg(rail_color).bg(theme.raised);
    let blank = || {
        Line::from(vec![
            Span::styled(glyph::RAIL, rail),
            Span::styled(" ".repeat(usize::from(width.saturating_sub(1))), tile),
        ])
    };
    let mut lines = vec![blank()];
    for line in content {
        let padding = usize::from(width.saturating_sub(3)).saturating_sub(line.width());
        let mut row = prefixed(vec![line], glyph::RAIL, rail).remove(0);
        row.spans.insert(1, Span::styled("  ", tile));
        row.spans.push(Span::styled(" ".repeat(padding), tile));
        lines.push(row.style(tile));
    }
    lines.push(blank());
    lines
}
pub(super) fn cards_lines(
    items: &[Card],
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let fit = usize::from((width.saturating_add(2) / 26).clamp(1, 3)).min(items.len());
    // Balance the rows: four cards form 2x2 rather than 3 + 1.
    let count = items.len().div_ceil(items.len().div_ceil(fit));
    let columns = u16::try_from(count).unwrap_or(1);
    let cell = width.saturating_sub(2 * (columns - 1)) / columns;
    let mut lines = Vec::new();
    for chunk in items.chunks(count) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        // Cards in a row share the tallest card's height; empty cells keep a
        // short last grid row left-aligned at the same width.
        let rows = chunk
            .iter()
            .map(|item| {
                card_lines(item, session, cell, theme, 0)
                    .len()
                    .saturating_sub(2)
            })
            .max()
            .unwrap_or(0);
        let mut cards: Vec<_> = chunk
            .iter()
            .map(|item| card_lines(item, session, cell, theme, rows))
            .collect();
        cards.resize(count, Vec::new());
        lines.extend(join_columns(&cards, &vec![cell; count], 2));
    }
    lines
}
pub(super) fn stat_lines(
    item: &Stat,
    session: &Session,
    (width, big): (u16, bool),
    theme: &Theme,
) -> Vec<Line<'static>> {
    let color = tone_color(item.tone.or(Some(Tone::Normal)), theme.fg, theme);
    let value_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
    let value = display(session, &item.value);
    let (number, unit) = crate::ui::style::display_split(&value);
    let mut lines = match crate::ui::style::display_font(number)
        .filter(|rows| big && rows[0].width() + unit.width() <= usize::from(width))
    {
        Some([top, middle, bottom]) => vec![
            Line::styled(top, value_style),
            Line::styled(middle, value_style),
            Line::from(vec![
                Span::styled(bottom, value_style),
                Span::styled(unit.to_string(), value_style),
            ]),
        ],
        None => styled_text(session, &item.value, width, value_style),
    };
    lines.extend(styled_text(
        session,
        &item.label,
        width,
        Style::default().fg(theme.fg_dim),
    ));
    if let Some(delta) = &item.delta {
        let delta = display(session, delta);
        let negative = delta.trim_start().starts_with('-');
        let arrow = if negative { "▼" } else { "▲" };
        lines.extend(wrapped_text(
            &format!("{arrow} {}", delta.trim_start_matches(['+', '-'])),
            width,
            Style::default().fg(if negative { theme.error } else { theme.success }),
        ));
    }
    lines
}
fn stats_lines(
    items: &[Stat],
    session: &Session,
    (width, big): (u16, bool),
    theme: &Theme,
) -> Vec<Line<'static>> {
    const GAP: u16 = 4;
    // Size each metric to its content when they all fit on one row; otherwise
    // fall back to an equal-width grid that wraps onto further rows.
    let natural: Vec<_> = items
        .iter()
        .map(|item| stat_lines(item, session, (width, big), theme))
        .collect();
    let widths: Vec<_> = natural
        .iter()
        .map(|lines| {
            u16::try_from(lines.iter().map(Line::width).max().unwrap_or(0)).unwrap_or(u16::MAX)
        })
        .collect();
    let total = widths
        .iter()
        .fold(0u16, |sum, w| sum.saturating_add(*w))
        .saturating_add(GAP * u16::try_from(items.len().saturating_sub(1)).unwrap_or(0));
    if total <= width {
        return join_columns(&natural, &widths, GAP);
    }
    let count = usize::from((width.saturating_add(3) / 15).max(1)).min(items.len());
    let columns = u16::try_from(count).unwrap_or(1);
    let cell = width.saturating_sub(3 * (columns - 1)) / columns;
    let mut lines = Vec::new();
    for chunk in items.chunks(count) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let mut stats: Vec<_> = chunk
            .iter()
            .map(|item| stat_lines(item, session, (cell, big), theme))
            .collect();
        stats.resize(count, Vec::new());
        lines.extend(join_columns(&stats, &vec![cell; count], 3));
    }
    lines
}
fn step_marker(state: StepState, accent: Color, theme: &Theme) -> (&'static str, Color) {
    match state {
        StepState::Done => ("●", theme.success),
        StepState::Current => ("◉", accent),
        StepState::Todo => ("○", theme.fg_dim),
    }
}
fn step_columns(
    items: &[nexus_core::interactive::DisplayStep],
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Option<Vec<Line<'static>>> {
    let accent = accent_color(session.document(), theme);
    let count = u16::try_from(items.len()).unwrap_or(u16::MAX).max(1);
    let column = usize::from(width / count);
    let fits = items.iter().all(|item| {
        display(session, &item.title).width() + 4 <= column
            && item
                .text
                .as_ref()
                .is_none_or(|text| display(session, text).width() + 3 <= column)
    });
    if !fits || items.is_empty() {
        return None;
    }
    let mut titles = Vec::new();
    let mut details = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let (mark, color) = step_marker(item.state, accent, theme);
        let title = display(session, &item.title);
        titles.push(Span::styled(format!("{mark} "), Style::default().fg(color)));
        titles.push(Span::styled(
            title.clone(),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ));
        let rest = column.saturating_sub(title.width() + 2);
        let joint = if index + 1 < items.len() {
            format!(" {}", "─".repeat(rest.saturating_sub(2)))
        } else {
            String::new()
        };
        titles.push(Span::styled(
            format!("{joint:<rest$}"),
            Style::default().fg(theme.border_dim),
        ));
        let text = item
            .text
            .as_ref()
            .map(|text| display(session, text))
            .unwrap_or_default();
        details.push(Span::styled(
            format!("  {text:<width$}", width = column.saturating_sub(2)),
            Style::default().fg(theme.fg_dim),
        ));
    }
    let mut lines = vec![Line::from(titles)];
    if items.iter().any(|item| item.text.is_some()) {
        lines.push(Line::from(details));
    }
    Some(lines)
}
fn steps_lines(
    items: &[nexus_core::interactive::DisplayStep],
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let accent = accent_color(session.document(), theme);
    let marker = |state| step_marker(state, accent, theme);
    let mut horizontal = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            horizontal.push(Span::raw("   "));
        }
        let (mark, color) = marker(item.state);
        horizontal.push(Span::styled(format!("{mark} "), Style::default().fg(color)));
        horizontal.push(Span::styled(
            display(session, &item.title),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ));
        if let Some(text) = &item.text {
            horizontal.push(Span::styled(
                format!(" — {}", display(session, text)),
                Style::default().fg(theme.fg_dim),
            ));
        }
    }
    let horizontal = Line::from(horizontal);
    if horizontal.width() <= usize::from(width) {
        return vec![horizontal];
    }
    if let Some(lines) = step_columns(items, session, width, theme) {
        return lines;
    }
    let mut lines = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let (mark, color) = marker(item.state);
        let titles = styled_text(
            session,
            &item.title,
            width.saturating_sub(2),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        );
        for (row, title) in titles.into_iter().enumerate() {
            lines.extend(prefixed(
                vec![title],
                &format!("{} ", if row == 0 { mark } else { "│" }),
                Style::default().fg(color),
            ));
        }
        if let Some(text) = &item.text {
            let body = styled_text(
                session,
                text,
                width.saturating_sub(2),
                Style::default().fg(theme.fg_dim),
            );
            lines.extend(prefixed(
                body,
                if index + 1 < items.len() {
                    "│ "
                } else {
                    "  "
                },
                Style::default().fg(theme.border_dim),
            ));
        }
        if index + 1 < items.len() {
            lines.push(Line::from(Span::styled(
                "│",
                Style::default().fg(theme.border_dim),
            )));
        }
    }
    lines
}
fn callout_lines(
    tone: CalloutTone,
    title: Option<&str>,
    text: &str,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let color = match tone {
        CalloutTone::Info => theme.accent,
        CalloutTone::Tip => accent_color(session.document(), theme),
        CalloutTone::Warning => theme.warning,
        CalloutTone::Danger => theme.error,
    };
    let mut lines = Vec::new();
    if let Some(title) = title {
        lines.extend(styled_text(
            session,
            title,
            width.saturating_sub(3),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    lines.extend(styled_text(
        session,
        text,
        width.saturating_sub(3),
        Style::default().fg(theme.fg),
    ));
    let band = Style::default().bg(theme.raised);
    prefixed(lines, "▌ ", Style::default().fg(color).bg(theme.raised))
        .into_iter()
        .map(|mut line| {
            let padding = usize::from(width).saturating_sub(line.width());
            line.spans.push(Span::styled(" ".repeat(padding), band));
            line.style(band)
        })
        .collect()
}
pub(super) fn layout_lines(
    node: &Node,
    session: &Session,
    width: u16,
    theme: &Theme,
) -> Option<Vec<Line<'static>>> {
    let _accent = accent_color(session.document(), theme);
    let lines = match node {
        Node::Image { src, alt, rows } => image_lines(src, alt, width, *rows, theme),
        Node::Hero {
            title,
            subtitle,
            badge,
            image,
            alt,
        } => {
            let mut lines = Vec::new();
            if let Some(image) = image {
                lines.extend(image_lines(
                    image,
                    alt.as_deref().unwrap_or_default(),
                    width,
                    6,
                    theme,
                ));
            }
            lines.extend(hero_lines(
                title,
                subtitle.as_deref(),
                badge.as_deref(),
                session,
                width,
                theme,
            ));
            lines
        }
        Node::Columns { items } => columns_lines(items, session, width, theme),
        Node::Cards { items } => cards_lines(items, session, width, theme),
        Node::Stats { items, big } => stats_lines(items, session, (width, *big), theme),
        Node::Steps { items } => steps_lines(items, session, width, theme),
        Node::Callout { tone, title, text } => {
            callout_lines(*tone, title.as_deref(), text, session, width, theme)
        }
        Node::Quote { text, by } => {
            let mut lines = prefixed(
                styled_text(
                    session,
                    text,
                    width.saturating_sub(2),
                    Style::default().fg(theme.fg).add_modifier(Modifier::ITALIC),
                ),
                "│ ",
                Style::default().fg(theme.border_dim),
            );
            if let Some(by) = by {
                lines.extend(wrapped_text(
                    &format!("— {}", display(session, by)),
                    width,
                    Style::default().fg(theme.fg_dim),
                ));
            }
            lines
        }
        Node::Divider { label } => {
            let label = label
                .as_ref()
                .map_or_else(String::new, |s| format!(" {} ", display(session, s)));
            let label = clipped(&label, width);
            let remaining = usize::from(width).saturating_sub(label.width());
            vec![Line::from(Span::styled(
                format!(
                    "{}{label}{}",
                    "╌".repeat(remaining / 2),
                    "╌".repeat(remaining.div_ceil(2))
                ),
                Style::default().fg(theme.border_dim),
            ))]
        }
        _ => return None,
    };
    Some(lines)
}
pub(super) fn display(session: &Session, template: &str) -> String {
    safe(&session.text(template).unwrap_or_default()).replace('\n', " ")
}
/// One focus treatment for unfilled controls: your accent, bold.
pub(super) fn control_style(theme: &Theme, selected: bool) -> Style {
    let style = Style::default().fg(if selected { theme.accent } else { theme.fg });
    if selected {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}
fn render_toggle(
    session: &Session,
    (key, label, boxed): (&str, &str, bool),
    area: Rect,
    (theme, focused): (&Theme, bool),
    buffer: &mut Buffer,
) {
    let mark = if boxed {
        bold::CHECK_WIDTH
    } else {
        bold::SWITCH_WIDTH
    };
    if area.width < mark + 2 {
        return;
    }
    let on = session.value(key) == "true";
    if boxed {
        bold::check(buffer, (area.x, area.y), on, theme);
    } else {
        bold::switch(buffer, (area.x, area.y), on, theme);
    }
    let offset = mark + 1;
    let style = if on || focused {
        control_style(theme, focused)
    } else {
        Style::default().fg(theme.fg_dim)
    };
    buffer.set_stringn(
        area.x + offset,
        area.y,
        display(session, label),
        usize::from(area.width - offset),
        style,
    );
}
fn stepper_offset(label: &str, value: &str, session: &Session, area: Rect) -> u16 {
    let chip = u16::try_from(value.width())
        .unwrap_or(u16::MAX)
        .saturating_add(6);
    if label.is_empty() {
        0
    } else {
        bold::label_column(session, area.width).min(area.width.saturating_sub(chip))
    }
}
/// Minus and plus hit regions of an inline stepper chip, ` − value + `.
pub(super) fn stepper_arrows(node: &Node, session: &Session, area: Rect) -> Option<[Rect; 2]> {
    let Node::Stepper { key, label, .. } = node else {
        return None;
    };
    let value = session.value(key);
    let value_width = u16::try_from(value.width()).unwrap_or(u16::MAX);
    if area.width < value_width.saturating_add(6) {
        return None;
    }
    let offset = stepper_offset(&display(session, label), &value, session, area);
    Some([
        Rect::new(area.x + offset, area.y, 2, 1),
        Rect::new(area.x + offset + value_width + 4, area.y, 2, 1),
    ])
}
fn render_stepper(
    node: &Node,
    session: &Session,
    region: Rect,
    theme: &Theme,
    selected: bool,
    buffer: &mut Buffer,
) {
    let Node::Stepper {
        key,
        label,
        min,
        max,
        ..
    } = node
    else {
        return;
    };
    let value = session.value(key);
    let label = display(session, label);
    if stepper_arrows(node, session, region).is_none() {
        Paragraph::new(value)
            .style(control_style(theme, selected).add_modifier(Modifier::BOLD))
            .render(region, buffer);
        return;
    }
    let offset = stepper_offset(&label, &value, session, region);
    bold::label(
        buffer,
        Rect::new(region.x, region.y, offset, 1),
        &label,
        selected,
        theme,
    );
    let number = value.parse::<f64>().unwrap_or_default();
    let surface = if selected {
        theme.selection
    } else {
        theme.control
    };
    let live = Style::default()
        .fg(if selected { theme.accent } else { theme.fg_dim })
        .bg(surface);
    let spent = Style::default().fg(theme.border_dim).bg(surface);
    let x = region.x + offset;
    let value_width = u16::try_from(value.width()).unwrap_or(0);
    bold::cap(buffer, (x, region.y), true, surface, theme);
    buffer.set_string(
        x + 1,
        region.y,
        "−",
        if number <= *min { spent } else { live },
    );
    bold::chip(
        buffer,
        (x + 2, region.y),
        &value,
        Style::default()
            .fg(theme.fg)
            .bg(surface)
            .add_modifier(Modifier::BOLD),
    );
    buffer.set_string(
        x + value_width + 4,
        region.y,
        "+",
        if number >= *max { spent } else { live },
    );
    bold::cap(
        buffer,
        (x + value_width + 5, region.y),
        false,
        surface,
        theme,
    );
}
fn render_select(
    node: &Node,
    session: &Session,
    region: Rect,
    theme: &Theme,
    selected: bool,
    buffer: &mut Buffer,
) {
    let Node::Select {
        key,
        label,
        options,
    } = node
    else {
        return;
    };
    let label = display(session, label);
    let value = display(session, &session.value(key));
    let column = if label.is_empty() {
        0
    } else {
        bold::label_column(session, region.width)
    };
    bold::label(
        buffer,
        Rect::new(region.x, region.y, column, 1),
        &label,
        selected,
        theme,
    );
    let area = Rect::new(
        region.x + column,
        region.y,
        region.width.saturating_sub(column),
        1,
    );
    let labels: Vec<_> = options.iter().map(|option| safe(option)).collect();
    let fill = if selected {
        theme.accent
    } else {
        accent_color(session.document(), theme)
    };
    if bold::segments_width(&labels) <= area.width {
        let active = options
            .iter()
            .position(|option| *option == session.value(key))
            .unwrap_or(0);
        bold::segmented(buffer, area, &labels, (active, fill), theme);
    } else {
        let surface = if selected {
            theme.selection
        } else {
            theme.control
        };
        let arrows = Style::default()
            .fg(if selected { theme.accent } else { theme.fg_dim })
            .bg(surface);
        let text = clipped(&value, area.width.saturating_sub(6));
        bold::cap(buffer, (area.x, area.y), true, surface, theme);
        buffer.set_string(area.x + 1, area.y, format!("{} ", glyph::PREV), arrows);
        let width = bold::chip(
            buffer,
            (area.x + 2, area.y),
            &text,
            Style::default()
                .fg(theme.fg)
                .bg(surface)
                .add_modifier(Modifier::BOLD),
        );
        buffer.set_string(area.x + 2 + width, area.y, glyph::NEXT, arrows);
        bold::cap(buffer, (area.x + 3 + width, area.y), false, surface, theme);
    }
}
/// The option of a select under column `x`, if its strip is showing.
pub(super) fn select_option_at(
    node: &Node,
    session: &Session,
    region: Rect,
    x: u16,
) -> Option<usize> {
    let Node::Select { label, options, .. } = node else {
        return None;
    };
    let column = if display(session, label).is_empty() {
        0
    } else {
        bold::label_column(session, region.width)
    };
    let area = Rect::new(
        region.x + column,
        region.y,
        region.width.saturating_sub(column),
        1,
    );
    let labels: Vec<_> = options.iter().map(|option| safe(option)).collect();
    if bold::segments_width(&labels) > area.width {
        return None;
    }
    bold::segment_rects(&labels, area)
        .iter()
        .position(|rect| x >= rect.x && x < rect.right().saturating_add(1))
}
pub(super) fn render_component(
    node: &Node,
    session: &Session,
    region: Rect,
    theme: &Theme,
    focus: Option<usize>,
    mut control: usize,
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    let mut hits = Vec::new();
    match node {
        Node::Checklist { items } => {
            for (index, item) in items.iter().enumerate() {
                let row = Rect::new(
                    region.x,
                    region.y + u16::try_from(index).unwrap_or(0),
                    region.width,
                    1,
                );
                render_toggle(
                    session,
                    (&item.key, &item.label, true),
                    row,
                    (theme, focus == Some(control)),
                    buffer,
                );
                hits.push((row, control));
                control += 1;
            }
        }
        Node::Toggle { key, label } => {
            render_toggle(
                session,
                (key, label, false),
                region,
                (theme, focus == Some(control)),
                buffer,
            );
            hits.push((region, control));
        }
        Node::Select { .. } => {
            render_select(node, session, region, theme, focus == Some(control), buffer);
            hits.push((region, control));
        }
        Node::Stepper { .. } => {
            render_stepper(node, session, region, theme, focus == Some(control), buffer);
            hits.push((region, control));
        }
        Node::Table {
            select,
            sortable,
            page_size,
            search,
            ..
        } => {
            render_table(
                node,
                session,
                region,
                theme,
                focus == Some(control)
                    && (select.is_some() || *sortable || page_size.is_some() || *search),
                buffer,
            );
            if select.is_some() || *sortable || page_size.is_some() || *search {
                hits.push((region, control));
            }
        }
        _ => {
            if let Some(lines) = layout_lines(node, session, region.width, theme) {
                Paragraph::new(lines).render(region, buffer);
            } else {
                render_data(node, session, region, theme, buffer);
            }
        }
    }
    hits
}
pub(super) fn clipped(text: &str, width: u16) -> String {
    if text.width() <= usize::from(width) {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let size = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + size >= usize::from(width) {
            break;
        }
        result.push(ch);
        used += size;
    }
    result.push('…');
    result
}
/// Each table column's `(x, width)` within `width` cells, one gap between columns.
pub(super) fn table_columns(session: &Session, node: &Node, width: u16) -> Vec<(u16, u16)> {
    let Node::Table {
        columns,
        rows_data,
        sortable,
        ..
    } = node
    else {
        return Vec::new();
    };
    let count = columns.len();
    if count == 0 {
        return Vec::new();
    }
    let arrow = if *sortable { 2 } else { 0 };
    let mut natural: Vec<usize> = columns
        .iter()
        .map(|header| display(session, header).width() + arrow)
        .collect();
    for row in rows_data {
        for (column, cell) in row.iter().enumerate().take(count) {
            natural[column] = natural[column].max(display(session, cell).width());
        }
    }
    let gaps = count - 1;
    let available = usize::from(width).saturating_sub(gaps);
    let mut widths = vec![0; count];
    let total: usize = natural.iter().sum();
    if total <= available {
        // Everything fits: share the slack evenly so the table fills its row.
        let slack = available - total;
        for (column, width) in widths.iter_mut().enumerate() {
            *width = natural[column] + slack / count + usize::from(column < slack % count);
        }
    } else {
        // Water-fill: columns narrower than a fair share keep their width.
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by_key(|&column| natural[column]);
        let mut remaining = available;
        for (placed, &column) in order.iter().enumerate() {
            let share = remaining / (count - placed);
            widths[column] = natural[column].min(share);
            remaining -= widths[column];
        }
        // Rounding leftovers go to the widest columns, which were clipped.
        for &column in order.iter().rev().take(remaining) {
            widths[column] += 1;
        }
    }
    let mut x = 0u16;
    widths
        .into_iter()
        .map(|width| {
            let width = u16::try_from(width).unwrap_or(u16::MAX);
            let column = (x, width);
            x = x.saturating_add(width).saturating_add(1);
            column
        })
        .collect()
}

pub(super) fn render_table(
    node: &Node,
    session: &Session,
    area: Rect,
    theme: &Theme,
    focused: bool,
    buffer: &mut Buffer,
) {
    let Node::Table {
        columns,
        rows_data,
        select,
        sortable,
        page_size,
        search,
    } = node
    else {
        return;
    };
    let selected = select.as_ref().map(|key| session.selected_row(key));
    let index = session.document().node_index(node);
    let state = session.table_state(index);
    let rows = session.table_rows(index);
    let size = page_size.unwrap_or(rows_data.len()).max(1);
    let page = state.page.min(rows.len().div_ceil(size).saturating_sub(1));
    let mut data = vec![(None, columns)];
    data.extend(
        rows.iter()
            .skip(page * size)
            .take(size)
            .map(|row| (Some(*row), &rows_data[*row])),
    );
    if *search {
        Paragraph::new(format!(
            "/ Search: {}{}",
            state.query,
            if state.searching { "▁" } else { "" }
        ))
        .style(control_style(theme, focused))
        .render(Rect::new(area.x, area.y, area.width, 1), buffer);
    }
    for (row, (original, cells)) in data.into_iter().enumerate() {
        let is_selected = original.is_some() && selected == original;
        let style = if row == 0 {
            Style::default()
                .fg(theme.fg_dim)
                .bg(theme.control)
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            control_style(theme, focused).bg(theme.selection)
        } else if row % 2 == 0 {
            Style::default().fg(theme.fg).bg(theme.raised)
        } else {
            Style::default().fg(theme.fg)
        };
        let marker = if is_selected { glyph::SELECTED } else { " " };
        let y = area.y + u16::from(*search) + u16::try_from(row).unwrap_or(0);
        buffer.set_style(Rect::new(area.x, y, area.width, 1), style);
        let lead = u16::from(select.is_some()).min(area.width);
        if lead > 0 {
            buffer.set_string(area.x, y, marker, style);
        }
        let layout = table_columns(session, node, area.width.saturating_sub(lead));
        for (column, (cell, &(x, cell_width))) in cells.iter().zip(&layout).enumerate() {
            let mut text = display(session, cell);
            if row == 0
                && *sortable
                && let Some((sorted, descending)) = state.sort
                && sorted == column
            {
                text.push_str(if descending { " ▾" } else { " ▴" });
            }
            Paragraph::new(clipped(&text, cell_width))
                .style(style)
                .render(Rect::new(area.x + lead + x, y, cell_width, 1), buffer);
        }
    }
    if page_size.is_some() {
        Paragraph::new(format!(
            "‹ {}/{} › · PgUp/PgDn",
            page + 1,
            rows.len().div_ceil(size).max(1)
        ))
        .style(Style::default().fg(theme.fg_dim))
        .render(
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
            buffer,
        );
    }
}
/// One compact data row: label column, slim track, value.
pub(super) struct BarRow<'a> {
    pub(super) label: &'a str,
    /// A dim control label rather than a foreground data category.
    pub(super) control_label: bool,
    pub(super) value: &'a str,
    /// Width reserved for values, shared by sibling rows so bars line up.
    pub(super) value_width: u16,
    pub(super) ratio: f64,
    pub(super) fill: Color,
}
pub(super) fn render_bar(
    row: &BarRow,
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    let column = bold::label_column(session, area.width);
    let label_area = Rect::new(area.x, area.y, column, 1);
    if row.control_label {
        bold::label(buffer, label_area, row.label, false, theme);
    } else {
        Paragraph::new(clipped(row.label, column.saturating_sub(1)))
            .style(Style::default().fg(theme.fg))
            .render(label_area, buffer);
    }
    let value_width = row.value_width.min(area.width.saturating_sub(column));
    let bar_width = area.width.saturating_sub(column + value_width + 2);
    let track = Rect::new(area.x + column, area.y, bar_width, 1);
    // Data bars are solid, their length is the reading; progress-like
    // controls keep the slim track that shows what remains.
    if row.control_label {
        bold::bar(buffer, track, row.ratio, row.fill, theme);
    } else {
        bold::solid_bar(buffer, track, row.ratio, row.fill);
    }
    bold::value(
        buffer,
        Rect::new(area.right() - value_width, area.y, value_width, 1),
        row.value,
        theme,
    );
}
/// "2 / 5", or a percentage when the maximum is one.
pub(super) fn ratio_text(value: f64, max: f64) -> String {
    if (max - 1.0).abs() < f64::EPSILON {
        format!("{}%", (value * 100.0).round())
    } else {
        format!("{value} / {max}")
    }
}
fn render_bars(
    (node, items): (usize, &[nexus_core::interactive::BarItem]),
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    let values: Vec<_> = items
        .iter()
        .map(|item| session.number(&item.value).unwrap_or(0.0).max(0.0))
        .collect();
    let max = values.iter().copied().fold(0.0, f64::max);
    let value_width = values
        .iter()
        .map(|value| value.to_string().width())
        .max()
        .unwrap_or(0);
    for (index, (item, value)) in items.iter().zip(values).enumerate() {
        render_bar(
            &BarRow {
                label: &display(session, &item.label),
                control_label: false,
                value: &value.to_string(),
                value_width: u16::try_from(value_width).unwrap_or(0),
                ratio: session.eased((node, index), if max > 0.0 { value / max } else { 0.0 }),
                fill: accent_color(session.document(), theme),
            },
            session,
            Rect::new(
                area.x,
                area.y + u16::try_from(index).unwrap_or(0),
                area.width,
                1,
            ),
            theme,
            buffer,
        );
    }
}
fn render_data(node: &Node, session: &Session, area: Rect, theme: &Theme, buffer: &mut Buffer) {
    match node {
        Node::Kv { items } => {
            let labels: Vec<_> = items
                .iter()
                .map(|pair| display(session, &pair[0]))
                .collect();
            let label_width = u16::try_from(labels.iter().map(|s| s.width()).max().unwrap_or(0))
                .unwrap_or(u16::MAX)
                .min(area.width / 2);
            for (index, (pair, label)) in items.iter().zip(&labels).enumerate() {
                let y = area.y + u16::try_from(index).unwrap_or(0);
                Paragraph::new(clipped(label, label_width))
                    .style(Style::default().fg(theme.fg_dim))
                    .render(Rect::new(area.x, y, label_width, 1), buffer);
                let offset = (label_width + 1).min(area.width);
                Paragraph::new(display(session, &pair[1]))
                    .style(Style::default().fg(theme.fg))
                    .render(
                        Rect::new(area.x + offset, y, area.width - offset, 1),
                        buffer,
                    );
            }
        }
        Node::Progress { label, value, max } => {
            let (value, max) = session.progress(value, max).unwrap_or_default();
            let text = ratio_text(value, max);
            render_bar(
                &BarRow {
                    label: &display(session, label),
                    control_label: true,
                    value: &text,
                    value_width: u16::try_from(text.width()).unwrap_or(0),
                    ratio: session.eased(
                        (session.document().node_index(node), 0),
                        if max > 0.0 { value / max } else { 0.0 },
                    ),
                    fill: accent_color(session.document(), theme),
                },
                session,
                area,
                theme,
                buffer,
            );
        }
        Node::Bars { items } => render_bars(
            (session.document().node_index(node), items),
            session,
            area,
            theme,
            buffer,
        ),
        Node::Sparkline { values, .. } => {
            let values: Vec<_> = values
                .iter()
                .map(|value| session.number(value).unwrap_or(0.0).max(0.0))
                .collect();
            // Draw in left to right the first time the panel shows.
            let area = Rect::new(
                area.x,
                area.y,
                bold::revealed(area.width, session),
                area.height,
            );
            let max = values.iter().copied().fold(0.0, f64::max);
            // Normalize before integer conversion to preserve even tiny fractional samples.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let values: Vec<_> = values
                .iter()
                .map(|value| {
                    if max > 0.0 {
                        (value / max * 1000.0) as u64
                    } else {
                        0
                    }
                })
                .collect();
            let levels = glyph::LEVELS;
            let bars = ratatui::symbols::bar::Set {
                empty: " ",
                one_eighth: levels[0],
                one_quarter: levels[1],
                three_eighths: levels[2],
                half: levels[3],
                five_eighths: levels[4],
                three_quarters: levels[5],
                seven_eighths: levels[6],
                full: levels[7],
            };
            Sparkline::default()
                .data(values)
                .bar_set(bars)
                .style(Style::default().fg(accent_color(session.document(), theme)))
                .render(area, buffer);
        }
        _ => {}
    }
}
