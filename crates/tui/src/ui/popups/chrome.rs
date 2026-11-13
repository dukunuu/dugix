//! Shared popup chrome: rounded blocks, consistent list selection, and small
//! title helpers for browse / edit / confirm modal states.

// Casts here are on terminal-bounded values (dims, byte colors, glyph counts),
// never on unbounded user data.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Widget, Wrap,
};

use crate::app_view::AppView;
use crate::theme::Theme;
use crate::ui::style::{SEP, width};

/// Canonical popup sizes, so the whole family reads as one design.
pub const SMALL: (u16, u16) = (56, 40);
pub const STANDARD: (u16, u16) = (64, 60);
pub const TALL: (u16, u16) = (64, 74);
pub const WIDE: (u16, u16) = (78, 66);

/// Where a centered popup was drawn last frame, for mouse resizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupHit {
    pub rect: Rect,
    pub id: &'static str,
    pub default: (u16, u16),
}

/// Smallest a resized popup gets, in cells (or the whole screen if smaller).
pub const MIN_POPUP_W: u16 = 40;
pub const MIN_POPUP_H: u16 = 10;

/// The rect popup `id` occupies on `screen`: its persisted size, else the
/// `default` percentages. Pure, so hit-testing can ask without drawing.
pub fn popup_rect(app: &AppView, screen: Rect, id: &str, default: (u16, u16)) -> Rect {
    let custom = nexus_core::app::popup_size(&app.settings.popup_sizes, id);
    let (pct_w, pct_h) = custom.unwrap_or(default);
    let rect = crate::ui::centered(screen, pct_w, pct_h);
    if custom.is_none() {
        return rect;
    }
    // A saved percentage must stay usable on a small terminal.
    let w = rect.width.max(MIN_POPUP_W.min(screen.width));
    let h = rect.height.max(MIN_POPUP_H.min(screen.height));
    Rect {
        x: screen.x + (screen.width - w) / 2,
        y: screen.y + (screen.height - h) / 2,
        width: w,
        height: h,
    }
}

/// [`popup_rect`] for a popup about to draw, also recording the rect so a border
/// press can start a resize.
pub fn popup_area(app: &AppView, screen: Rect, id: &'static str, default: (u16, u16)) -> Rect {
    let rect = popup_rect(app, screen, id, default);
    app.popup_hit.set(Some(PopupHit { rect, id, default }));
    rect
}

/// [`popup_area`] that grows to `rows` lines plus the border when the sized
/// rect would clip a report, still centered and within the screen.
pub fn popup_area_fit(
    app: &AppView,
    screen: Rect,
    id: &'static str,
    default: (u16, u16),
    rows: usize,
) -> Rect {
    let rect = popup_rect(app, screen, id, default);
    let want = u16::try_from(rows.saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(screen.height);
    let rect = if want > rect.height {
        Rect {
            y: screen.y + (screen.height - want) / 2,
            height: want,
            ..rect
        }
    } else {
        rect
    };
    app.popup_hit.set(Some(PopupHit { rect, id, default }));
    rect
}

/// Horizontal padding inside every popup frame, in cells per side.
pub const PAD: u16 = 1;

/// Frame tone: `Danger` colors the border for destructive confirm states.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Danger,
}

/// Canonical spellings for popup keyboard hints.
#[allow(dead_code)] // Shared vocabulary for the remaining popup conversions.
pub mod keys {
    pub const MOVE: &str = "↑↓";
    pub const SIDE: &str = "←→";
    pub const ENTER: &str = "Enter";
    pub const ESC: &str = "Esc";
    pub const TAB: &str = "Tab";
    pub const SPACE: &str = "Space";
}

/// A keyboard shortcut and its optional one-word action.
pub struct Hint {
    pub key: Cow<'static, str>,
    pub verb: Cow<'static, str>,
}

/// Build a hint; an empty verb displays just the key.
#[must_use]
pub fn hint(key: impl Into<Cow<'static, str>>, verb: impl Into<Cow<'static, str>>) -> Hint {
    Hint {
        key: key.into(),
        verb: verb.into(),
    }
}

/// Format a Ctrl shortcut in caret notation.
#[must_use]
pub fn ctrl(letter: char) -> String {
    format!("^{}", letter.to_uppercase())
}

/// Drop whole hints until they fit, preserving the last hint even at tiny widths.
#[must_use]
pub fn fit_hints(hints: &[Hint], max: usize) -> Vec<&Hint> {
    let mut fitted: Vec<_> = hints.iter().collect();
    while fitted.len() > 1 {
        let used = fitted
            .iter()
            .map(|hint| {
                width(&hint.key)
                    + if hint.verb.is_empty() {
                        0
                    } else {
                        1 + width(&hint.verb)
                    }
            })
            .sum::<usize>()
            + (fitted.len() - 1) * width(SEP);
        if used <= max {
            break;
        }
        let drop = if fitted.len() > 3 {
            fitted.len() - 3
        } else {
            fitted.len() - 2
        };
        fitted.remove(drop);
    }
    fitted
}

/// The title, status and actions around a popup's content.
pub struct PopupFrame<'a> {
    pub title: &'a str,
    pub count: &'a str,
    pub hints: &'a [Hint],
    pub tone: Tone,
    pub focused: bool,
}

/// Clear and draw the shared popup frame, returning its padded content area.
pub fn render_popup(f: &mut Frame, area: Rect, frame: &PopupFrame<'_>, app: &AppView) -> Rect {
    let theme = &app.theme;
    let title = format!(" {} ", frame.title);
    let color = if frame.tone == Tone::Danger {
        theme.error
    } else {
        theme.accent
    };
    let mut block = popup_block_focused(
        Line::from(Span::styled(
            title.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
        .left_aligned(),
        theme,
        frame.focused,
        frame.tone,
    );
    if !frame.count.is_empty()
        && width(&title) + width(frame.count) + 4 <= usize::from(area.width.saturating_sub(2))
    {
        block = block.title_top(
            Line::from(Span::styled(
                format!(" {} ", frame.count),
                Style::default().fg(theme.fg_dim),
            ))
            .right_aligned(),
        );
    }
    if !app.settings.hide_hints && !frame.hints.is_empty() {
        let fitted = fit_hints(frame.hints, usize::from(area.width.saturating_sub(4)));
        let dim = Style::default().fg(theme.fg_dim);
        let mut spans = vec![Span::raw(" ")];
        for (i, hint) in fitted.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(SEP, dim));
            }
            spans.push(Span::styled(
                hint.key.clone(),
                Style::default().fg(theme.fg),
            ));
            if !hint.verb.is_empty() {
                spans.push(Span::styled(format!(" {}", hint.verb), dim));
            }
        }
        spans.push(Span::raw(" "));
        block = block.title_bottom(Line::from(spans).right_aligned());
    }
    f.render_widget(Clear, area);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// Draw a field and divider above content when there is room for both.
pub fn render_field(
    f: &mut Frame,
    area: Rect,
    icon: &str,
    text: &str,
    placeholder: &str,
    editing: bool,
    theme: &Theme,
) -> Rect {
    if area.height < 4 {
        return area;
    }
    let dim = Style::default().fg(theme.fg_dim);
    let mut spans = vec![Span::styled(format!("{icon} "), dim)];
    if editing || !text.is_empty() {
        spans.push(Span::styled(text, Style::default().fg(theme.fg)));
        spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
    } else {
        spans.push(Span::styled(placeholder, dim));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );
    f.render_widget(
        Paragraph::new("─".repeat(usize::from(area.width)))
            .style(Style::default().fg(theme.border_dim)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    Rect {
        y: area.y + 2,
        height: area.height - 2,
        ..area
    }
}

/// Draw a tab strip and divider above content when there is room for both.
#[allow(dead_code)] // Used by the tabbed popup conversions that follow Sessions.
pub fn render_tabs(f: &mut Frame, area: Rect, tabs: &[&str], active: usize, theme: &Theme) -> Rect {
    if area.height < 4 {
        return area;
    }
    let mut spans = Vec::new();
    for (i, name) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        let style = if i == active {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg_dim)
        };
        spans.push(Span::styled(*name, style));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );
    f.render_widget(
        Paragraph::new("─".repeat(usize::from(area.width)))
            .style(Style::default().fg(theme.border_dim)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    Rect {
        y: area.y + 2,
        height: area.height - 2,
        ..area
    }
}

/// Draw a wrapped, dim italic empty-state sentence at the top of the list area.
pub fn render_empty(f: &mut Frame, area: Rect, message: &str, theme: &Theme) {
    f.render_widget(
        Paragraph::new(message)
            .style(
                Style::default()
                    .fg(theme.fg_dim)
                    .add_modifier(Modifier::ITALIC),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// The rounded border + title style shared by every popup.
pub fn popup_block_focused<'a>(
    title: impl Into<Line<'a>>,
    theme: &Theme,
    focused: bool,
    tone: Tone,
) -> Block<'a> {
    let border = match tone {
        Tone::Normal if focused => theme.border,
        Tone::Normal => theme.border_dim,
        Tone::Danger => theme.error,
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(theme.background_style())
        .padding(ratatui::widgets::Padding::horizontal(PAD))
        .title(title)
}

/// The popup frame plus the standard bottom-right hint bar, hidden by
/// `hide_hints` and truncated with `…` to the frame's inner width.
pub fn hinted_block<'a>(
    title: impl Into<Line<'a>>,
    hint: &str,
    app: &AppView,
    focused: bool,
    tone: Tone,
    width: u16,
) -> Block<'a> {
    let mut block = popup_block_focused(title, &app.theme, focused, tone);
    if !app.settings.hide_hints && !hint.trim().is_empty() {
        let max = (width.saturating_sub(4)) as usize; // +2 for the border, +2 for the hint padding
        let text = truncate(hint, max);
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {text} "),
                Style::default().fg(app.theme.fg_dim),
            ))
            .right_aligned(),
        );
    }
    block
}

/// Clear a popup area and render its rounded frame, returning the inner rect.
pub fn render_frame<'a>(
    f: &mut Frame,
    area: Rect,
    title: impl Into<Line<'a>>,
    theme: &Theme,
    focused: bool,
    tone: Tone,
) -> Rect {
    f.render_widget(Clear, area);
    let block = popup_block_focused(title, theme, focused, tone);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// Like [`render_frame`] but with the standard bottom hint bar.
pub fn render_hinted<'a>(
    f: &mut Frame,
    area: Rect,
    title: impl Into<Line<'a>>,
    hint: &str,
    app: &AppView,
    focused: bool,
    tone: Tone,
) -> Rect {
    f.render_widget(Clear, area);
    let block = hinted_block(title, hint, app, focused, tone, area.width);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// One keyboard-labelled action card.
pub struct Button<'a, A> {
    /// Opaque caller-owned action returned by hit-testing.
    pub action: A,
    /// Visible label, including the keyboard shortcut.
    pub label: &'a str,
    /// Optional artwork; its last line is the compact description.
    pub body: &'a [&'a str],
    pub selected: bool,
    pub enabled: bool,
    /// The document's resolved primary action.
    pub primary: bool,
}

/// Render compact pills or artwork tiles in measured regions, returning hit
/// areas that correspond exactly to this frame.
pub fn render_buttons<A: Copy>(
    buffer: &mut ratatui::buffer::Buffer,
    area: Rect,
    buttons: &[Button<'_, A>],
    theme: &Theme,
) -> Vec<(Rect, A)> {
    if buttons.is_empty() || area.is_empty() {
        return Vec::new();
    }
    let columns = usize::from((area.width / 16).max(1)).min(buttons.len());
    let row_count = buttons.len().div_ceil(columns);
    let rows = Layout::vertical(vec![
        Constraint::Ratio(
            1,
            u32::try_from(row_count).unwrap_or(u32::MAX)
        );
        row_count
    ])
    .split(area);
    let areas: Vec<_> = rows
        .iter()
        .zip(buttons.chunks(columns))
        .flat_map(|(row, items)| {
            let count = u32::try_from(items.len()).unwrap_or(u32::MAX);
            Layout::horizontal(vec![Constraint::Ratio(1, count); items.len()])
                .spacing(1)
                .split(*row)
                .to_vec()
        })
        .collect();
    let mut hits = Vec::new();
    for (button, rect) in buttons.iter().zip(areas.iter().copied()) {
        if rect.is_empty() {
            continue;
        }
        let label = crate::interactive::safe(button.label);
        if rect.height > 1 && buttons.len() == 1 || !button.body.is_empty() {
            render_tile(buffer, rect, &label, button.body, button.selected, theme);
        } else {
            let mut style = Style::default()
                .fg(if !button.enabled {
                    theme.fg_dim
                } else if button.primary {
                    theme.bg
                } else {
                    theme.fg
                })
                .bg(if button.primary && button.enabled {
                    theme.accent
                } else {
                    theme.control
                });
            if button.primary {
                style = style.add_modifier(Modifier::BOLD);
            }
            if button.selected {
                style = if button.primary {
                    style.add_modifier(Modifier::REVERSED)
                } else {
                    style.fg(theme.accent)
                }
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(theme.accent);
            }
            Paragraph::new(fit_line(
                Line::from(format!(" {label} ")),
                usize::from(rect.width),
            ))
            .style(style)
            .render(Rect::new(rect.x, rect.y, rect.width, 1), buffer);
        }
        if button.enabled {
            hits.push((rect, button.action));
        }
    }
    hits
}

fn render_tile(
    buffer: &mut ratatui::buffer::Buffer,
    area: Rect,
    label: &str,
    body: &[&str],
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered().border_style(Style::default().fg(if focused {
        theme.accent
    } else {
        theme.border_dim
    }));
    let inner = block.inner(area);
    block.render(area, buffer);
    let art = body.len().saturating_sub(1);
    for (row, text) in body.iter().take(art).enumerate() {
        Paragraph::new(crate::interactive::safe(text))
            .centered()
            .style(Style::default().fg(theme.fg))
            .render(
                Rect::new(
                    inner.x,
                    inner.y + u16::try_from(row).unwrap_or(0),
                    inner.width,
                    1,
                ),
                buffer,
            );
    }
    let text_width = inner.width.saturating_sub(2);
    Paragraph::new(fit_line(Line::from(label), usize::from(text_width)))
        .style(Style::default().fg(theme.fg).add_modifier(Modifier::BOLD))
        .render(
            Rect::new(
                inner.x + 1,
                inner.y + u16::try_from(art).unwrap_or(0),
                text_width,
                1,
            ),
            buffer,
        );
    if let Some(metadata) = body.last() {
        Paragraph::new(fit_line(
            Line::from(crate::interactive::safe(metadata)),
            usize::from(text_width),
        ))
        .style(Style::default().fg(theme.fg_dim))
        .render(
            Rect::new(
                inner.x + 1,
                inner.y + u16::try_from(art).unwrap_or(0) + 1,
                text_width,
                1,
            ),
            buffer,
        );
    }
}

/// Standard popup list selection: accent `▸ ` marker, bold text, raised shade.
pub fn standard_list<'a>(items: Vec<ListItem<'a>>, theme: &Theme) -> List<'a> {
    List::new(items)
        .highlight_symbol(Span::styled(
            format!("{} ", crate::ui::style::glyph::SELECTED),
            Style::default().fg(theme.accent),
        ))
        .highlight_style(
            Style::default()
                .bg(theme.raised)
                .add_modifier(Modifier::BOLD),
        )
}

/// Truncate `s` to `max` display columns, appending `…` when it overflows.
pub fn truncate(s: &str, max: usize) -> String {
    let max = max.max(1);
    if width(s) <= max {
        return s.to_string();
    }
    format!(
        "{}…",
        take_cols(s.chars(), max - 1)
            .into_iter()
            .collect::<String>()
    )
}

/// Truncate `s` to `max` display columns by cutting the middle: for file names
/// the start tells files apart and the end carries the extension.
pub fn truncate_middle(s: &str, max: usize) -> String {
    let max = max.max(1);
    if width(s) <= max {
        return s.to_string();
    }
    let keep = max - 1;
    let head = keep / 3;
    let start: String = take_cols(s.chars(), head).into_iter().collect();
    let end: String = take_cols(s.chars().rev(), keep - width(&start))
        .into_iter()
        .rev()
        .collect();
    format!("{start}…{end}")
}

/// The leading chars of `chars` that fit in `cols` display columns.
fn take_cols(chars: impl Iterator<Item = char>, cols: usize) -> Vec<char> {
    let mut used = 0;
    chars
        .take_while(|&c| {
            used += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            used <= cols
        })
        .collect()
}

/// Fit a styled line to `cols` columns, cutting across spans with a trailing `…`
/// in the style of the span it lands in.
pub fn fit_line(line: Line<'_>, cols: usize) -> Line<'_> {
    let total: usize = line.spans.iter().map(|s| width(&s.content)).sum();
    if total <= cols {
        return line;
    }
    let mut spans = Vec::with_capacity(line.spans.len());
    if cols > 0 {
        let mut left = cols - 1;
        for span in line.spans {
            let n = width(&span.content);
            if n <= left {
                left -= n;
                spans.push(span);
            } else {
                let kept: String = take_cols(span.content.chars(), left).into_iter().collect();
                spans.push(Span::styled(format!("{kept}…"), span.style));
                break;
            }
        }
    }
    Line { spans, ..line }
}

/// [`fit_line`] over every line.
pub fn fit_lines(lines: Vec<Line<'_>>, width: u16) -> Vec<Line<'_>> {
    lines
        .into_iter()
        .map(|l| fit_line(l, width as usize))
        .collect()
}

/// Where a popup list was drawn last frame, for mouse clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListHit {
    pub area: Rect,
    pub item_lines: u16,
    pub offset: usize,
    pub selected: Option<usize>,
    pub total: usize,
}

impl ListHit {
    /// The item index under a screen cell, if the cell is on a list row.
    pub fn index_at(&self, column: u16, row: u16) -> Option<usize> {
        if !self
            .area
            .contains(ratatui::layout::Position::new(column, row))
        {
            return None;
        }
        let index = self.offset + usize::from((row - self.area.y) / self.item_lines.max(1));
        (index < self.total).then_some(index)
    }
}

/// Render a popup list with a right-edge scrollbar when the content overflows;
/// `item_lines` is how many rows each item occupies, for the thumb's length.
pub fn render_list(
    f: &mut Frame,
    list: List<'_>,
    state: &mut ListState,
    area: Rect,
    total: usize,
    item_lines: u16,
    app: &AppView,
) {
    let theme = &app.theme;
    let list_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(1).max(1),
        height: area.height,
    };
    f.render_stateful_widget(list, list_area, state);
    app.list_hit.set(Some(ListHit {
        area: list_area,
        item_lines: item_lines.max(1),
        offset: state.offset(),
        selected: state.selected(),
        total,
    }));
    let viewport = (area.height / item_lines.max(1)) as usize;
    if total > viewport {
        let gutter = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y,
            width: 1,
            height: area.height,
        };
        let mut sb_state = ratatui::widgets::ScrollbarState::new(total)
            .viewport_content_length(viewport)
            .position(state.offset());
        let sb =
            ratatui::widgets::Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(theme.accent))
                .track_style(Style::default().fg(theme.border_dim));
        f.render_stateful_widget(sb, gutter, &mut sb_state);
    }
}

/// A dim italic placeholder row for empty lists.
pub fn empty_placeholder(message: &str, theme: &Theme) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        message.to_string(),
        Style::default()
            .fg(theme.fg_dim)
            .add_modifier(Modifier::ITALIC),
    )))
}

/// `"12 items · "`-style prefix for footer hints (singular-aware).
pub fn count_hint(n: usize, label: &str) -> String {
    format!("{n} {label}{} · ", if n == 1 { "" } else { "s" })
}

fn titled_line(app: &AppView, text: impl Into<String>, color: Color, hint: &str) -> Line<'static> {
    let text = text.into();
    let mut spans = vec![Span::styled(
        format!(" {text} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    if !app.settings.hide_hints && !hint.trim().is_empty() {
        spans.push(Span::styled(
            format!("— {hint} "),
            Style::default().fg(app.theme.fg_dim),
        ));
    }
    Line::from(spans)
}

/// The standard popup title: the bold accent name.
pub fn popup_title(app: &AppView, name: impl Into<String>) -> Line<'static> {
    Line::from(crate::ui::style::title(&app.theme, name))
}

/// A title for text-entry popups: label + live value + trailing cursor.
pub fn input_title(
    app: &AppView,
    label: impl Into<String>,
    value: impl AsRef<str>,
    hint: &str,
) -> Line<'static> {
    let label = label.into();
    let mut spans = vec![Span::styled(
        format!(" {label}: "),
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(
        format!("{}▏", value.as_ref()),
        Style::default().fg(app.theme.fg),
    ));
    if !app.settings.hide_hints && !hint.trim().is_empty() {
        spans.push(Span::styled(
            format!("  ({hint}) "),
            Style::default().fg(app.theme.fg_dim),
        ));
    }
    Line::from(spans)
}

fn confirmish_title(
    app: &AppView,
    text: impl Into<String>,
    color: Color,
    hint: &str,
) -> Line<'static> {
    titled_line(app, text, color, hint)
}

/// Destructive prompt title (error color).
pub fn danger_title(app: &AppView, text: impl Into<String>, hint: &str) -> Line<'static> {
    confirmish_title(app, text, app.theme.error, hint)
}

/// Max rows the detail strip will grow to before truncating.
const MAX_DETAIL_ROWS: u16 = 4;

/// Split a popup's inner area into (list area, detail-strip area), the strip
/// sized to `desc`'s wrapped height.
pub fn split_with_detail(area: Rect, desc: &str) -> (Rect, Rect) {
    if desc.trim().is_empty() {
        return (area, Rect { height: 0, ..area });
    }
    let inner_w = area.width.max(1) as usize;
    let wrapped = textwrap::wrap(desc, inner_w).len().max(1) as u16;
    let desc_h = wrapped.min(MAX_DETAIL_ROWS) + 1; // +1 for the divider
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(desc_h)]).split(area);
    (chunks[0], chunks[1])
}

/// Render the wrapped description strip: a dim divider rule, then plain text.
pub fn render_detail(f: &mut Frame, area: Rect, desc: &str, theme: &Theme) {
    if area.height == 0 || desc.trim().is_empty() {
        return;
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border_dim));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let p = Paragraph::new(desc.to_string())
        .style(Style::default().fg(theme.fg_dim))
        .wrap(Wrap { trim: true });
    f.render_widget(p, inner);
}
