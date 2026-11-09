//! The Bold panel look's shared drawing pieces: the label column that lines
//! labelled controls and bars up, eighth-cell bars on a track, and filled
//! pills for buttons, badges and tabs. Glyphs come from `ui/style.rs`.

use super::{Session, accent_color, display, glyph, tone_color};
use crate::theme::Theme;
use nexus_core::interactive::{Node, Tone};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

/// Width of the shared label column: the widest label among the panel's
/// labelled one-row controls and bars, plus a gap, capped at a third of the
/// width so values keep most of the row.
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

/// A control label in the label column: dim capitals, your accent when the
/// control has focus.
pub(super) fn label(buffer: &mut Buffer, area: Rect, text: &str, focused: bool, theme: &Theme) {
    let style = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg_dim)
    };
    let text = super::clipped(&text.to_uppercase(), area.width.saturating_sub(1));
    buffer.set_stringn(area.x, area.y, text, usize::from(area.width), style);
}

/// A one-row bar: `ratio` of `area` in `fill`, eighth-cell precise, on a
/// dim track.
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
            ("█", Style::default().fg(fill))
        } else if cell == whole && part > 0 {
            (glyph::EIGHTHS[part - 1], Style::default().fg(fill))
        } else {
            (glyph::TRACK, track)
        };
        buffer.set_string(area.x + x, area.y, symbol, style);
    }
}

/// Fill and ink for a tone: semantic tones fill with their color; normal and
/// muted fall back to the theme's control surfaces so they stay quiet.
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

/// A filled pill `▐ text ▌` at `(x, y)`, clipped to `limit` columns; returns
/// the width drawn. The caps take the fill color over the panel background,
/// so the ends read as rounded.
pub(super) fn pill(
    buffer: &mut Buffer,
    (x, y): (u16, u16),
    limit: u16,
    text: &str,
    (fill, ink): (Color, Color),
    bold: bool,
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
    // An unpainted fill (transparent backgrounds) gets no caps: they would
    // draw in the terminal's default foreground.
    let (left, right) = if fill == Color::Reset {
        (" ", " ")
    } else {
        (glyph::CAP_LEFT, glyph::CAP_RIGHT)
    };
    buffer.set_string(x, y, left, Style::default().fg(fill));
    buffer.set_string(x + 1, y, format!(" {text} "), inner);
    buffer.set_string(x + 1 + inner_width, y, right, Style::default().fg(fill));
    inner_width + 2
}

/// Width `pill` will take for `text`.
pub(super) fn pill_width(text: &str) -> u16 {
    u16::try_from(text.width())
        .unwrap_or(u16::MAX)
        .saturating_add(4)
}

/// A value right-aligned at the end of a row, in bold foreground.
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

/// A pill as spans for line-based layouts; `bg` is the surface it sits on,
/// which the caps keep as their background.
pub(super) fn pill_spans(
    text: &str,
    (fill, ink): (Color, Color),
    bg: Color,
) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::text::Span;
    let cap = Style::default().fg(fill).bg(bg);
    let (left, right) = if fill == Color::Reset {
        (" ", " ")
    } else {
        (glyph::CAP_LEFT, glyph::CAP_RIGHT)
    };
    vec![
        Span::styled(left, cap),
        Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(ink)
                .bg(fill)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(right, cap),
    ]
}

/// What a button does, which picks its fill.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// The panel's main action: filled with the panel accent.
    Primary,
    /// Any other action: the theme's control surface.
    Secondary,
    /// Not available now.
    Disabled,
}

/// A button pill. Focus fills it with your accent and marks it `▸`, so "you
/// are here" never depends on the panel's own color.
pub(super) fn button(
    buffer: &mut Buffer,
    (x, y): (u16, u16),
    limit: u16,
    text: &str,
    (kind, focused): (Kind, bool),
    panel: Color,
    theme: &Theme,
) -> Rect {
    let colors = if focused {
        (theme.accent, theme.on(theme.accent))
    } else {
        match kind {
            Kind::Primary => (panel, theme.on(panel)),
            Kind::Secondary => (theme.control, theme.fg),
            Kind::Disabled => (theme.raised, theme.fg_dim),
        }
    };
    let width = pill(
        buffer,
        (x, y),
        limit,
        text,
        colors,
        kind == Kind::Primary || focused,
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

/// Where each tab's pill sits on the strip; shared by drawing and clicks.
pub(super) fn tab_rects(labels: &[String], area: Rect) -> Vec<Rect> {
    let mut x = area.x;
    labels
        .iter()
        .map(|label| {
            let width = pill_width(label).min(area.right().saturating_sub(x));
            let rect = Rect::new(x, area.y, width, 1);
            x = x.saturating_add(width).saturating_add(1).min(area.right());
            rect
        })
        .collect()
}
