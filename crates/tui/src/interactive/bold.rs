//! Shared panel drawing: aligned labels, slim tracks, solid bars and compact
//! filled controls. Glyphs come from `ui/style.rs`.

use super::{Session, accent_color, display, glyph, tone_color};
use crate::theme::Theme;
use nexus_core::interactive::{Node, Tone};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

/// Width of the shared label column, capped at a third of the panel so values keep most of the row.
pub(super) fn label_column(session: &Session, width: u16) -> u16 {
    let widest = session
        .document()
        .all_nodes()
        .into_iter()
        .flat_map(|node| match node {
            Node::Stepper { label, .. }
            | Node::Select { label, .. }
            | Node::Slider { label, .. }
            | Node::Rating { label, .. }
            | Node::Progress { label, .. }
            | Node::Meter { label, .. } => vec![display(session, label).width()],
            Node::Bars { items } => items
                .iter()
                .map(|item| display(session, &item.label).width())
                .collect(),
            _ => Vec::new(),
        })
        .max()
        .unwrap_or(0);
    u16::try_from(widest + 2)
        .unwrap_or(u16::MAX)
        .clamp(6, (width / 3).max(6))
        .min(width)
}

/// A control label in the label column, accented when focused.
pub(super) fn label(buffer: &mut Buffer, area: Rect, text: &str, focused: bool, theme: &Theme) {
    let style = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg_dim)
    };
    let text = super::clipped(text, area.width.saturating_sub(1));
    buffer.set_stringn(area.x, area.y, text, usize::from(area.width), style);
}

/// A slim one-row bar with a partial end cap and a dim track.
pub(super) fn bar(buffer: &mut Buffer, area: Rect, ratio: f64, fill: Color, theme: &Theme) {
    if area.width == 0 {
        return;
    }
    let scaled = (ratio.clamp(0.0, 1.0) * f64::from(area.width) * 8.0).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let eighth_cells = scaled as usize;
    let (whole, part) = (eighth_cells / 8, eighth_cells % 8);
    let track = Style::default().fg(theme.border_dim);
    for x in 0..area.width {
        let cell = usize::from(x);
        let (symbol, style) = if cell < whole {
            (glyph::BAR_FILL, Style::default().fg(fill))
        } else if cell == whole && part > 0 {
            (glyph::BAR_END, Style::default().fg(fill))
        } else {
            (glyph::BAR_TRACK, track)
        };
        buffer.set_string(area.x + x, area.y, symbol, style);
    }
}

/// Fill and ink for a tone; normal and muted stay on the theme's control surfaces.
pub(super) fn tone_fill(tone: Tone, session: &Session, theme: &Theme) -> (Color, Color) {
    match tone {
        Tone::Normal => (theme.control, theme.fg),
        Tone::Muted => (theme.raised, theme.fg_dim),
        tone => {
            let fill = tone_color(Some(tone), accent_color(session.document(), theme), theme);
            (fill, theme.on(fill))
        }
    }
}

/// One end of a filled shape, one cell wide whatever the font draws.
pub(super) fn cap(buffer: &mut Buffer, (x, y): (u16, u16), left: bool, fill: Color, theme: &Theme) {
    if theme.round_caps {
        let under = buffer.cell((x, y)).map_or(theme.surface, |cell| cell.bg);
        buffer.set_string(
            x,
            y,
            if left {
                glyph::CAP_LEFT
            } else {
                glyph::CAP_RIGHT
            },
            Style::default().fg(fill).bg(under),
        );
    } else {
        buffer.set_string(x, y, " ", Style::default().bg(fill));
    }
}

/// A padded label on a filled surface, clipped to `limit` columns; returns the width drawn.
pub(super) fn pill(
    buffer: &mut Buffer,
    (x, y): (u16, u16),
    limit: u16,
    text: &str,
    (fill, ink): (Color, Color),
    (bold, theme): (bool, &Theme),
) -> u16 {
    if limit < 5 {
        return 0;
    }
    let text = super::clipped(text, limit.saturating_sub(4));
    let inner = Style::default().fg(ink).bg(fill);
    let inner = if bold {
        inner.add_modifier(Modifier::BOLD)
    } else {
        inner
    };
    let inner_width = u16::try_from(text.width()).unwrap_or(0) + 2;
    cap(buffer, (x, y), true, fill, theme);
    buffer.set_string(x + 1, y, format!(" {text} "), inner);
    cap(buffer, (x + 1 + inner_width, y), false, fill, theme);
    inner_width + 2
}

/// Columns of a `width`-wide plot drawn so far while it draws in.
pub(super) fn revealed(width: u16, session: &Session) -> u16 {
    let shown = (f64::from(width) * session.drawn_fraction()).ceil();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let shown = shown as u16;
    shown.min(width)
}

pub(super) fn pill_width(text: &str) -> u16 {
    u16::try_from(text.width())
        .unwrap_or(u16::MAX)
        .saturating_add(4)
}

/// A value right-aligned at the end of a row.
pub(super) fn value(buffer: &mut Buffer, area: Rect, text: &str, theme: &Theme) -> u16 {
    let width = u16::try_from(text.width())
        .unwrap_or(u16::MAX)
        .min(area.width);
    buffer.set_stringn(
        area.right() - width,
        area.y,
        text,
        usize::from(width),
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    );
    width
}

/// A padded label as spans for line-based layouts, on the surface `bg`.
pub(super) fn pill_spans(
    text: &str,
    (fill, ink): (Color, Color),
    (bg, round): (Color, bool),
) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::text::Span;
    let fill_or_bg = if fill == Color::Reset { bg } else { fill };
    let end = |symbol: &'static str| {
        if round {
            Span::styled(symbol, Style::default().fg(fill_or_bg).bg(bg))
        } else {
            Span::styled(" ", Style::default().bg(fill_or_bg))
        }
    };
    vec![
        end(glyph::CAP_LEFT),
        Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(ink)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        ),
        end(glyph::CAP_RIGHT),
    ]
}

/// What a button does, which picks its fill.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// The panel's main action: filled with the panel accent.
    Primary,
    /// Any other action: the theme's control surface.
    Secondary,
    Disabled,
}

/// A button pill; focus fills it with your accent and marks it `▸`.
pub(super) fn button(
    buffer: &mut Buffer,
    (x, y): (u16, u16),
    limit: u16,
    text: &str,
    (kind, focused): (Kind, bool),
    panel: Color,
    theme: &Theme,
) -> Rect {
    // Secondary and unavailable actions are quiet text links, so the one
    // filled shape in a row is the action the panel wants.
    if !focused && kind != Kind::Primary {
        if limit < 5 {
            return Rect::new(x, y, 0, 1);
        }
        let text = super::clipped(text, limit.saturating_sub(4));
        let style = if kind == Kind::Disabled {
            Style::default().fg(theme.border_dim)
        } else {
            Style::default()
                .fg(theme.fg_dim)
                .add_modifier(Modifier::UNDERLINED)
        };
        buffer.set_string(x + 2, y, &text, style);
        return Rect::new(x, y, u16::try_from(text.width()).unwrap_or(0) + 4, 1);
    }
    let colors = if focused {
        (theme.accent, theme.on(theme.accent))
    } else {
        (panel, theme.on(panel))
    };
    let width = pill(
        buffer,
        (x, y),
        limit,
        text,
        colors,
        (kind == Kind::Primary || focused, theme),
    );
    if focused && width > 0 {
        buffer.set_string(
            x + 1,
            y,
            glyph::SELECTED,
            Style::default()
                .fg(colors.1)
                .bg(colors.0)
                .add_modifier(Modifier::BOLD),
        );
    }
    Rect::new(x, y, width, 1)
}

/// Where each segment of a segmented strip sits; drawing and clicks share it.
pub(super) fn segment_rects(labels: &[String], area: Rect) -> Vec<Rect> {
    let mut x = area.x.saturating_add(1).min(area.right());
    labels
        .iter()
        .map(|label| {
            let width = u16::try_from(label.width())
                .unwrap_or(u16::MAX)
                .saturating_add(2)
                .min(area.right().saturating_sub(x).saturating_sub(1));
            let rect = Rect::new(x, area.y, width, 1);
            x = x.saturating_add(width).saturating_add(1).min(area.right());
            rect
        })
        .collect()
}

/// Total width of a segmented strip for `labels`, caps included.
pub(super) fn segments_width(labels: &[String]) -> u16 {
    let text: usize = labels.iter().map(|label| label.width() + 2).sum();
    u16::try_from(text + labels.len().saturating_sub(1) + 2).unwrap_or(u16::MAX)
}

/// A segmented strip with the active option filled and capped at both ends.
pub(super) fn segmented(
    buffer: &mut Buffer,
    area: Rect,
    labels: &[String],
    (active, fill): (usize, Color),
    theme: &Theme,
) -> Vec<Rect> {
    let rects = segment_rects(labels, area);
    let surface = |index: usize| if index == active { fill } else { theme.control };
    for (index, (rect, label)) in rects.iter().zip(labels).enumerate() {
        if rect.width == 0 {
            continue;
        }
        let style = if index == active {
            Style::default()
                .fg(theme.on(fill))
                .bg(fill)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg).bg(theme.control)
        };
        buffer.set_style(*rect, style);
        buffer.set_stringn(
            rect.x + 1,
            rect.y,
            label,
            usize::from(rect.width.saturating_sub(2)),
            style,
        );
        if index + 1 < labels.len() && rect.right() < area.right() {
            buffer.set_string(
                rect.right(),
                rect.y,
                "▕",
                Style::default().fg(theme.bg).bg(theme.control),
            );
        }
    }
    if let (Some(first), Some(last)) = (rects.first(), rects.last())
        && first.width > 0
        && last.right() < area.right()
    {
        cap(buffer, (first.x - 1, first.y), true, surface(0), theme);
        cap(
            buffer,
            (last.right(), last.y),
            false,
            surface(rects.len() - 1),
            theme,
        );
    }
    rects
}

pub(super) fn tab_rects(labels: &[String], area: Rect) -> Vec<Rect> {
    segment_rects(labels, area)
}

/// A compact filled chip, returning its width.
pub(super) fn chip(buffer: &mut Buffer, (x, y): (u16, u16), text: &str, style: Style) -> u16 {
    let width = u16::try_from(text.width())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    buffer.set_string(x, y, format!(" {text} "), style);
    width
}

pub(super) const SWITCH_WIDTH: u16 = 4;
pub(super) const CHECK_WIDTH: u16 = 3;

/// A four-cell switch, on with its knob at the right.
pub(super) fn switch(buffer: &mut Buffer, (x, y): (u16, u16), on: bool, theme: &Theme) {
    let (fill, knob) = if on {
        (theme.success, theme.on(theme.success))
    } else {
        (theme.control, theme.fg_dim)
    };
    let track = Style::default().fg(knob).bg(fill);
    cap(buffer, (x, y), true, fill, theme);
    if on {
        buffer.set_string(x + 1, y, " ", track);
        buffer.set_string(x + 2, y, glyph::DOT, track);
    } else {
        buffer.set_string(x + 1, y, glyph::DOT, track);
        buffer.set_string(x + 2, y, " ", track);
    }
    cap(buffer, (x + 3, y), false, fill, theme);
}

/// A three-cell check box, capped at both ends.
pub(super) fn check(buffer: &mut Buffer, (x, y): (u16, u16), on: bool, theme: &Theme) {
    let fill = if on { theme.success } else { theme.control };
    cap(buffer, (x, y), true, fill, theme);
    buffer.set_string(
        x + 1,
        y,
        if on { glyph::OK } else { " " },
        Style::default()
            .fg(theme.on(theme.success))
            .bg(fill)
            .add_modifier(Modifier::BOLD),
    );
    cap(buffer, (x + 2, y), false, fill, theme);
}

/// A solid one-row bar for data, five-eighths tall so stacked bars keep a seam.
pub(super) fn solid_bar(buffer: &mut Buffer, area: Rect, ratio: f64, fill: Color) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cells = (ratio.clamp(0.0, 1.0) * f64::from(area.width)).round() as u16;
    // Any nonzero value shows at least one cell.
    let cells = if ratio > 0.0 { cells.max(1) } else { 0 };
    buffer.set_string(
        area.x,
        area.y,
        "▆".repeat(usize::from(cells.min(area.width))),
        Style::default().fg(fill),
    );
}
