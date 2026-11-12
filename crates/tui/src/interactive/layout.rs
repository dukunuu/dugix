use super::lines::{card_head, card_lines, clipped, display, layout_lines};
use super::widgets::safe;
use super::{Entry, bold, components, image_lines};
use crate::theme::Theme;
use crate::ui::popups::chrome;
use crate::ui::style::glyph;
use nexus_core::interactive::{
    Accent, Button, Card, Document, Frame as DocumentFrame, Node, Session, Tone,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

/// One image drawn inside a document, in document order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocImage {
    pub url: String,
    pub button: Option<(usize, usize)>,
}
/// Every image a document draws, in the order `image_targets` lays them out.
pub(crate) fn document_images(document: &Document) -> Vec<DocImage> {
    let plain = |url: &String| DocImage {
        url: url.clone(),
        button: None,
    };
    let mut out = Vec::new();
    for (index, node) in document.all_nodes().into_iter().enumerate() {
        match node {
            Node::Image { src, .. } => out.push(plain(src)),
            Node::Hero {
                image: Some(url), ..
            } => out.push(plain(url)),
            Node::Cards { items } => {
                out.extend(items.iter().filter_map(|c| c.image.as_ref().map(plain)));
            }
            Node::Buttons { items, .. } => {
                out.extend(items.iter().enumerate().filter_map(|(item, button)| {
                    button.image.as_ref().map(|url| DocImage {
                        url: url.clone(),
                        button: Some((index, item)),
                    })
                }));
            }
            _ => {}
        }
    }
    out
}
/// Where one image sits on a document canvas; an image button exposes only its corner tag.
#[derive(Clone, Debug)]
pub(crate) struct ImageTarget {
    pub rect: Rect,
    pub url: String,
    pub index: usize,
}
fn card_image_rects(items: &[Card], session: &Session, region: Rect) -> Vec<(Rect, String, bool)> {
    let theme = Theme::default();
    let fit = usize::from((region.width.saturating_add(2) / 26).clamp(1, 3)).min(items.len());
    let count = items.len().div_ceil(items.len().div_ceil(fit));
    let columns = u16::try_from(count).unwrap_or(1);
    let cell = region.width.saturating_sub(2 * (columns - 1)) / columns;
    let mut out = Vec::new();
    let mut y = region.y;
    for (row, chunk) in items.chunks(count).enumerate() {
        if row > 0 {
            y = y.saturating_add(1);
        }
        let rows = chunk
            .iter()
            .map(|item| {
                card_lines(item, session, cell, &theme, 0)
                    .len()
                    .saturating_sub(2)
            })
            .max()
            .unwrap_or(0);
        for (column, item) in chunk.iter().enumerate() {
            if let Some(url) = &item.image {
                let head = card_head(item, session, cell, &theme).len();
                out.push((
                    Rect::new(
                        // Past the tile's rail and its two-column padding.
                        region.x + u16::try_from(column).unwrap_or(0) * (cell + 2) + 3,
                        y.saturating_add(1 + u16::try_from(head).unwrap_or(0)),
                        cell.saturating_sub(4),
                        6,
                    ),
                    url.clone(),
                    false,
                ));
            }
        }
        y = y.saturating_add(u16::try_from(rows + 2).unwrap_or(u16::MAX));
    }
    out
}
pub(crate) fn image_targets(session: &Session, width: u16) -> Vec<ImageTarget> {
    let mut targets = Vec::new();
    for (node, region) in components::visible_regions(session, width) {
        let base: usize = session
            .document()
            .all_nodes()
            .into_iter()
            .take(session.document().node_index(node))
            .map(components::image_count)
            .sum();
        let mut found = Vec::new();
        match node {
            Node::Image { src, .. } => found.push((region, src.clone(), false)),
            Node::Hero {
                image: Some(url), ..
            } => found.push((
                Rect::new(region.x, region.y, region.width, 6.min(region.height)),
                url.clone(),
                false,
            )),
            Node::Cards { items } => found.extend(card_image_rects(items, session, region)),
            Node::Buttons { items, .. } => {
                for (item, rect) in button_regions(session, items, region) {
                    if let Some(url) = &items[item].image {
                        let area =
                            Rect::new(rect.x + 1, rect.y + 1, rect.width.saturating_sub(2), 6);
                        // A button too narrow to draw the tag still counts in
                        // the order; it just has nothing to click.
                        let tag = if area.width >= VIEW_TAG_WIDTH + 5 {
                            view_tag(area)
                        } else {
                            Rect::new(area.x, area.y, 0, 0)
                        };
                        found.push((tag, url.clone(), true));
                    }
                }
            }
            _ => {}
        }
        targets.extend(
            found
                .into_iter()
                .enumerate()
                .map(|(index, (rect, url, _))| ImageTarget {
                    rect,
                    url,
                    index: base + index,
                }),
        );
    }
    targets
}
/// Render validated nodes into their measured, padded regions.
pub(super) fn render_nodes(
    session: &Session,
    regions: &[Rect],
    theme: &Theme,
    focus: Option<usize>,
    can_send: bool,
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    Entry::nodes(session, regions, theme, focus, can_send, buffer)
}
pub(super) fn text_lines(session: &Session, text: &str, width: u16) -> Vec<Line<'static>> {
    safe(&session.text(text).unwrap_or_else(|error| error.to_string()))
        .split('\n')
        .flat_map(|line| {
            crate::ui::markdown::wrap_styled_line(&Line::from(line), usize::from(width))
        })
        .collect()
}
pub(super) fn primary_button(document: &Document) -> Option<(usize, usize)> {
    let buttons = || {
        document
            .all_nodes()
            .into_iter()
            .enumerate()
            .flat_map(|(node, value)| {
                let items = match value {
                    Node::Buttons { items, .. } => items.as_slice(),
                    _ => &[],
                };
                let all_answers = items.iter().all(|button| document.answers(&button.action));
                items
                    .iter()
                    .enumerate()
                    .map(move |(item, button)| ((node, item), button, all_answers))
            })
    };
    buttons()
        .find(|(_, button, _)| button.primary)
        .or_else(|| {
            buttons()
                .find(|(_, button, all_answers)| !all_answers && document.sends(&button.action))
        })
        .map(|(index, _, _)| index)
}
pub(super) fn session_controls(session: &Session) -> usize {
    session
        .visible_nodes()
        .iter()
        .map(|(_, node)| components::control_count(node, session))
        .sum::<usize>()
        + usize::from(session.needs_data_load())
}
pub(super) fn button_label(session: &Session, button: &Button) -> String {
    format!(
        "{}{}",
        display(session, &button.label),
        if session.document().answers(&button.action) {
            " →"
        } else if session.document().sends(&button.action) {
            " ↵"
        } else {
            ""
        }
    )
}
pub(super) fn choice_list(document: &Document, items: &[Button]) -> bool {
    !items.is_empty()
        && items.iter().all(|button| {
            button.body.is_empty() && button.image.is_none() && document.answers(&button.action)
        })
}
pub(super) fn render_button_group(
    session: &Session,
    (index, items): (usize, &[Button]),
    area: Rect,
    theme: &Theme,
    (focus, control, can_send): (Option<usize>, usize, bool),
    buffer: &mut Buffer,
) -> Vec<(Rect, usize)> {
    let mut hits = Vec::new();
    let primary = primary_button(session.document());
    let choices = choice_list(session.document(), items);
    let choice: usize = session
        .visible_nodes()
        .into_iter()
        .take_while(|(node_index, _)| *node_index < index)
        .filter_map(|(_, node)| match node {
            Node::Buttons { items, .. } if choice_list(session.document(), items) => {
                Some(items.len())
            }
            _ => None,
        })
        .sum();
    let tiles = items
        .iter()
        .any(|button| !button.body.is_empty() || button.image.is_some());
    for (item, rect) in button_regions(session, items, area) {
        let button = &items[item];
        let selected = focus == Some(control + item);
        if choices {
            render_choice(
                &display(session, &button.label),
                choice + item + 1,
                rect,
                theme,
                selected,
                buffer,
            );
            hits.push((rect, control + item));
            continue;
        }
        let enabled = can_send
            || session.document().answers(&button.action)
            || !session.document().sends(&button.action);
        let label = button_label(session, button);
        if !tiles {
            let kind = if !enabled {
                bold::Kind::Disabled
            } else if primary == Some((index, item)) {
                bold::Kind::Primary
            } else {
                bold::Kind::Secondary
            };
            let rect = bold::button(
                buffer,
                (rect.x, rect.y),
                rect.width,
                &label,
                (kind, selected),
                accent_color(session.document(), theme),
                theme,
            );
            hits.push((rect, control + item));
            continue;
        }
        let mut body: Vec<_> = button.body.iter().map(String::as_str).collect();
        if button.image.is_some() {
            if body.is_empty() {
                body.push("");
            }
            body.splice(0..0, [""; 6]);
        }
        hits.extend(chrome::render_buttons(
            buffer,
            rect,
            &[chrome::Button {
                action: control + item,
                label: &label,
                body: &body,
                selected,
                primary: primary == Some((index, item)),
                enabled,
            }],
            theme,
        ));
        if let Some(image) = &button.image {
            let area = Rect::new(rect.x + 1, rect.y + 1, rect.width.saturating_sub(2), 6);
            Paragraph::new(image_lines(
                image,
                button.alt.as_deref().unwrap_or_default(),
                area.width,
                area.height,
                theme,
            ))
            .render(area, buffer);
            // The button keeps its click for the action; this tag (and the
            // `v` key) opens the image in the viewer instead.
            if area.width >= VIEW_TAG_WIDTH + 5 {
                Paragraph::new(VIEW_TAG)
                    .style(Style::default().fg(theme.fg).bg(theme.control))
                    .render(view_tag(area), buffer);
            }
        }
    }
    hits
}
const VIEW_TAG: &str = " v ";
const VIEW_TAG_WIDTH: u16 = 3;
fn view_tag(image_area: Rect) -> Rect {
    let width = VIEW_TAG_WIDTH.min(image_area.width);
    Rect::new(image_area.right() - width, image_area.y, width, 1)
}
fn choice_lines(label: &str, width: u16) -> Vec<Line<'static>> {
    wrapped_text(label, width.saturating_sub(6), Style::default())
}
fn render_choice(
    label: &str,
    number: usize,
    area: Rect,
    theme: &Theme,
    selected: bool,
    buffer: &mut Buffer,
) {
    if area.is_empty() {
        return;
    }
    let mut style = Style::default().fg(if selected { theme.accent } else { theme.fg });
    if selected {
        style = style.bg(theme.selection);
    }
    buffer.set_style(area, style);
    let indent = area.width.min(4);
    Paragraph::new(choice_lines(label, area.width))
        .style(style)
        .render(
            Rect::new(
                area.x + indent,
                area.y,
                area.width.saturating_sub(6),
                area.height,
            ),
            buffer,
        );
    if number <= 9 {
        Paragraph::new(format!(" {number} "))
            .style(Style::default().fg(theme.fg).bg(theme.control))
            .render(Rect::new(area.x, area.y, area.width.min(3), 1), buffer);
    }
    Paragraph::new("→")
        .style(Style::default().fg(theme.fg_dim))
        .render(Rect::new(area.right() - 1, area.y, 1, 1), buffer);
}
fn button_regions(session: &Session, items: &[Button], area: Rect) -> Vec<(usize, Rect)> {
    let mut regions = Vec::new();
    if choice_list(session.document(), items) {
        let mut y = area.y;
        for (item, button) in items.iter().enumerate() {
            let height =
                u16::try_from(choice_lines(&display(session, &button.label), area.width).len())
                    .unwrap_or(u16::MAX)
                    .max(1);
            regions.push((item, Rect::new(area.x, y, area.width, height)));
            y = y.saturating_add(height);
        }
    } else if items
        .iter()
        .any(|button| !button.body.is_empty() || button.image.is_some())
    {
        let content_width = items
            .iter()
            .flat_map(|button| {
                std::iter::once(button_label(session, button).width())
                    .chain(button.body.iter().map(|line| safe(line).width()))
            })
            .max()
            .unwrap_or(0);
        let minimum_width = u16::try_from(content_width)
            .unwrap_or(u16::MAX)
            .saturating_add(4)
            .max(if items.iter().any(|button| button.image.is_some()) {
                20
            } else {
                0
            });
        let columns =
            usize::from((area.width.saturating_add(2) / minimum_width.saturating_add(2)).max(1))
                .min(items.len());
        let count = u16::try_from(columns).unwrap_or(1);
        let width = area.width.saturating_sub((count - 1) * 2) / count;
        let mut y = area.y;
        for (row, chunk) in items.chunks(columns).enumerate() {
            let height = chunk
                .iter()
                .map(|button| {
                    u16::try_from(button.body.len()).unwrap_or(0)
                        + 3
                        + if button.image.is_some() { 6 } else { 0 }
                })
                .max()
                .unwrap_or(3);
            for item in 0..chunk.len() {
                let x = area.x + u16::try_from(item).unwrap_or(0) * (width + 2);
                regions.push((row * columns + item, Rect::new(x, y, width, height)));
            }
            y = y.saturating_add(height).saturating_add(1);
        }
    } else {
        let mut x = area.x;
        let mut y = area.y;
        // Keep action order predictable: drawing, tabbing and the source all agree.
        for (item, button) in items.iter().enumerate() {
            let width = bold::pill_width(&button_label(session, button)).min(area.width);
            let fits = x > area.x && x.saturating_add(width) <= area.right();
            if x > area.x && !fits {
                x = area.x;
                y = y.saturating_add(1);
            }
            regions.push((item, Rect::new(x, y, width, 1)));
            x = x.saturating_add(width).saturating_add(1);
        }
    }
    regions
}
/// Column where panel content starts, leaving a gutter for the focus marker.
pub(super) const CONTENT_X: u16 = 2;
/// Measure actual content at the available width; declared rows remain core caps.
pub(super) fn node_regions(session: &Session, width: u16) -> Vec<Rect> {
    branch_regions(
        session,
        session.document().nodes(),
        Rect::new(CONTENT_X, 2, width.saturating_sub(4), u16::MAX),
    )
}
pub(super) fn branch_regions(session: &Session, nodes: &[Node], area: Rect) -> Vec<Rect> {
    let width = area.width;
    let mut y = area.y;
    let mut previous = None;
    // A field's message row (hint or error) already spaces it from the next
    // node, so no extra blank row follows it.
    let mut spaced = false;
    nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            // Runs of text, and runs of hairline readings, read as one block:
            // no blank row inside a run, one between different kinds.
            let run = run_kind(node);
            if index > 0 && !spaced && (run.is_none() || run != previous) {
                y = y.saturating_add(1);
            }
            previous = run;
            spaced = matches!(node, Node::Input { rules, hint, .. } if !rules.is_empty() || !hint.is_empty());
            let height = components::height(node, session, width).unwrap_or_else(|| match node {
                Node::Text { text, .. } => u16::try_from(text_lines(session, text, width).len())
                    .unwrap_or(u16::MAX)
                    .max(1),
                Node::Buttons { items, .. } => {
                    button_regions(session, items, Rect::new(2, 0, width, 0))
                        .iter()
                        .map(|(_, rect)| rect.bottom())
                        .max()
                        .unwrap_or(1)
                }
                Node::Input { rules, hint, .. } => {
                    node.rows() + u16::from(!rules.is_empty() || !hint.is_empty())
                }
                _ => layout_lines(node, session, width, &Theme::default()).map_or_else(
                    || node.rows(),
                    |lines| u16::try_from(lines.len()).unwrap_or(u16::MAX).max(1),
                ),
            });
            let region = Rect::new(
                area.x,
                y,
                width,
                height.min(u16::MAX.saturating_sub(y.saturating_add(2))),
            );
            y = region.bottom();
            region
        })
        .collect()
}
/// Nodes that stack without a blank row when they follow one of their own kind.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Run {
    Text,
    Reading,
}
pub(super) fn run_kind(node: &Node) -> Option<Run> {
    match node {
        Node::Text { .. } => Some(Run::Text),
        Node::Progress { .. } | Node::Meter { .. } => Some(Run::Reading),
        _ => None,
    }
}
pub(super) fn card_height(regions: &[Rect]) -> u16 {
    regions
        .last()
        .map_or(4, |rect| rect.bottom().saturating_add(2))
}
pub(crate) fn source_height(source: &str, width: u16) -> u16 {
    document_height(Document::preview(source), width)
}
/// Rows a block reserves: settled when complete, else the streamed preview.
pub(crate) fn block_height(source: &str, complete: bool, width: u16) -> u16 {
    if complete {
        document_height(Document::settled(source), width)
    } else {
        source_height(source, width)
    }
}
fn document_height(document: Option<Document>, width: u16) -> u16 {
    document
        .and_then(|document| Session::new(document).ok())
        .map_or(4, |session| card_height(&node_regions(&session, width)))
}

/// In-flight panels are disposable drawings, never registered UI instances.
pub(super) fn streaming_buffer(document: Option<Document>, width: u16, theme: &Theme) -> Buffer {
    let session = document.and_then(|document| Session::new(document).ok());
    let Some(session) = session else {
        let mut buffer = card_buffer(width, 4, "Designing your interface", theme, "");
        Paragraph::new("◌ Waiting for the first element…")
            .style(Style::default().fg(theme.fg_dim))
            .render(Rect::new(2, 2, width.saturating_sub(4), 1), &mut buffer);
        return buffer;
    };
    let regions = node_regions(&session, width);
    let height = card_height(&regions);
    let mut buffer = document_buffer(width, height, &session, theme, "");
    render_nodes(&session, &regions, theme, None, false, &mut buffer);
    if width >= 8 {
        buffer.set_string(width - 3, 0, "◌", Style::default().fg(theme.fg_dim));
    }
    let count = session.document().nodes().len();
    let status = format!(
        "◌ composing · {count} element{}",
        if count == 1 { "" } else { "s" }
    );
    let status = clipped(&status, width.saturating_sub(6));
    Paragraph::new(format!(" {status} "))
        .style(Style::default().fg(theme.fg_dim))
        .render(
            Rect::new(2, height - 1, width.saturating_sub(4), 1),
            &mut buffer,
        );
    buffer
}
/// Scroll a pinned panel so its focused control stays in view; returns the rows scrolled.
pub(super) fn follow_focus(
    entry: &mut Entry,
    canvas: &Buffer,
    hits: &[(Rect, usize)],
    caret: Option<Position>,
    viewport: u16,
) -> u16 {
    let rows = viewport.saturating_sub(1).max(1);
    let target = caret
        .map(|caret| Rect::new(caret.x, caret.y, 1, 1))
        .or_else(|| {
            hits.iter()
                .find(|(_, control)| *control == entry.focus)
                .map(|(rect, _)| *rect)
        });
    if let Some(rect) = target {
        if rect.bottom() > entry.scroll + rows {
            entry.scroll = rect.bottom() - rows;
        }
        if rect.y < entry.scroll {
            entry.scroll = rect.y;
        }
    }
    entry.scroll = entry.scroll.min(canvas.area.height.saturating_sub(rows));
    entry.scroll
}
pub(super) fn scrolled(canvas: &Buffer, scroll: u16) -> Buffer {
    let area = canvas.area;
    let height = area.height.saturating_sub(scroll);
    let mut buffer = Buffer::empty(Rect::new(0, 0, area.width, height));
    for y in 0..height {
        for x in 0..area.width {
            buffer[(x, y)] = canvas[(x, y + scroll)].clone();
        }
    }
    buffer
}
/// One line on why part of a panel is missing, without the list of alternatives.
pub(super) fn left_out_note(error: &nexus_core::interactive::CheckError) -> String {
    let line = error.message.lines().next().unwrap_or_default();
    let line = line.split(';').next().unwrap_or(line).trim_end_matches(':');
    safe(line.trim())
}
/// Mark a panel that renders without its broken pieces, left of the key hints.
pub(super) fn draw_left_out(buffer: &mut Buffer, note: &str, hint: &str, theme: &Theme) {
    let area = buffer.area;
    if area.width < 12 || area.height < 2 {
        return;
    }
    let reserved = if hint.is_empty() {
        0
    } else {
        u16::try_from(clipped(hint, area.width.saturating_sub(6)).width())
            .unwrap_or(area.width)
            .saturating_add(3)
    };
    let room = area.width.saturating_sub(6 + reserved);
    let text = if room >= 24 {
        clipped(&format!("{} left out: {note}", glyph::WARN), room)
    } else {
        clipped(&format!("{} part left out", glyph::WARN), room)
    };
    if !text.is_empty() {
        buffer.set_string(
            2,
            area.height - 1,
            format!(" {text} "),
            Style::default().fg(theme.warning),
        );
    }
}
pub(super) fn card_buffer(
    width: u16,
    height: u16,
    title: &str,
    theme: &Theme,
    hint: &str,
) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    buffer.set_style(area, theme.background_style());
    panel_chrome(
        &mut buffer,
        &Chrome {
            title: Some(title),
            frame: DocumentFrame::Rounded,
            expand: true,
            hint,
        },
        theme,
    );
    buffer
}
/// Quiet, shared chrome around a panel's content.
struct Chrome<'a> {
    /// Title text; `None` leaves the bar empty (a Hero already says it).
    title: Option<&'a str>,
    frame: DocumentFrame,
    expand: bool,
    hint: &'a str,
}
fn panel_chrome(buffer: &mut Buffer, chrome: &Chrome, theme: &Theme) {
    let area = buffer.area;
    if area.width < 4 || area.height < 2 {
        return;
    }
    let width = area.width;
    let title = chrome
        .title
        .map(|title| {
            clipped(
                &safe(title),
                width.saturating_sub(if chrome.expand { 6 } else { 4 }),
            )
        })
        .unwrap_or_default();
    let rule = match chrome.frame {
        DocumentFrame::Double => "═",
        DocumentFrame::Heavy => "━",
        DocumentFrame::Dashed => "╌",
        DocumentFrame::Rounded | DocumentFrame::Plain => "─",
    };
    let dim = Style::default().fg(theme.border_dim);
    if matches!(chrome.frame, DocumentFrame::Plain) {
        buffer.set_string(0, 0, rule.repeat(usize::from(width)), dim);
        if !title.is_empty() {
            buffer.set_string(
                0,
                0,
                format!("{title} "),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            );
        }
    } else if !title.is_empty() {
        buffer.set_string(
            2,
            0,
            &title,
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        );
    }
    if chrome.expand {
        buffer.set_string(width - 3, 0, "⤢", Style::default().fg(theme.fg_dim));
    }
    let bottom = area.height - 1;
    buffer.set_string(0, bottom, rule.repeat(usize::from(width)), dim);
    if !chrome.hint.is_empty() {
        let hint = format!(" {} ", clipped(chrome.hint, width.saturating_sub(6)));
        let x = width.saturating_sub(2 + u16::try_from(hint.width()).unwrap_or(width));
        buffer.set_string(x, bottom, hint, Style::default().fg(theme.fg_dim));
    }
}
pub(super) fn accent_color(document: &Document, theme: &Theme) -> Color {
    match document.accent() {
        Accent::Accent => theme.accent,
        Accent::Accent2 => theme.accent2,
        Accent::Success => theme.success,
        Accent::Warning => theme.warning,
        Accent::Error => theme.error,
        Accent::Muted => theme.fg_dim,
    }
}
pub(super) fn tone_color(tone: Option<Tone>, accent: Color, theme: &Theme) -> Color {
    match tone {
        None => accent,
        Some(Tone::Normal) => theme.fg,
        Some(Tone::Muted) => theme.fg_dim,
        Some(Tone::Accent) => theme.accent,
        Some(Tone::Success) => theme.success,
        Some(Tone::Warning) => theme.warning,
        Some(Tone::Error) => theme.error,
    }
}
pub(super) fn document_buffer(
    width: u16,
    height: u16,
    session: &Session,
    theme: &Theme,
    hint: &str,
) -> Buffer {
    let doc = session.document();
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    buffer.set_style(area, theme.background_style());
    let duplicate = matches!(doc.nodes().first(), Some(Node::Hero { title, .. })
        if safe(&session.text(title).unwrap_or_default()).trim().to_lowercase()
            == safe(doc.title()).trim().to_lowercase());
    panel_chrome(
        &mut buffer,
        &Chrome {
            title: (!duplicate).then(|| doc.title()),
            frame: doc.frame(),
            expand: !doc.is_static(),
            hint: if doc.is_static() { "" } else { hint },
        },
        theme,
    );
    buffer
}
pub(super) fn wrapped_text(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    safe(text)
        .split('\n')
        .flat_map(|line| {
            crate::ui::markdown::wrap_styled_line(&Line::from(line), usize::from(width.max(1)))
        })
        .map(|line| line.style(style))
        .collect()
}
