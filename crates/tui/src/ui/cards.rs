//! Transcript card builders: user bubbles, assistant replies, tool calls,
//! research/compaction/error rows, session links, and inline images.

// Casts here are on terminal-bounded values (dims, byte colors, glyph counts),
// never on unbounded user data.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
mod tools;

pub(super) use tools::{
    BatchRows, CardEnv, push_round_preamble, push_tool_call_with_rows, tool_markdown,
};

#[cfg(test)]
use tools::read_body;

use image::GenericImageView;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;

use super::style::{self, glyph};
use super::{diff, dim, fmt_cost, popups, to_color};
use crate::app_view::AppView;
use nexus_core::db::Message;

/// Non-text regions travel together through the card renderer.
pub(super) struct Regions<'a> {
    pub diagrams: &'a mut Vec<crate::diagrams::Placement>,
    pub widgets: &'a mut Vec<crate::interactive::Placement>,
}

/// Drop `![alt](file)` refs from body text — the images render inline instead.
/// Unterminated refs (mid-stream) are kept verbatim.
pub(super) fn strip_markdown_images(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    for (range, _, _) in markdown_images(content) {
        out.push_str(&content[cursor..range.start]);
        cursor = range.end;
    }
    out.push_str(&content[cursor..]);
    out
}

/// Actual Markdown images only, never image-looking text inside a code fence.
fn markdown_images(content: &str) -> Vec<(std::ops::Range<usize>, String, String)> {
    let mut images = Vec::new();
    let mut current = None;
    for (event, range) in Parser::new(content).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                current = Some((range, dest_url.into_string(), String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, _, alt)) = &mut current {
                    alt.push_str(&text);
                }
            }
            Event::End(TagEnd::Image) => {
                if let Some(image) = current.take() {
                    images.push(image);
                }
            }
            _ => {}
        }
    }
    images
}

/// A user message card: a right-aligned bubble capped at ~60% of the pane.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_user_card(
    out: &mut Vec<Line<'static>>,
    image_at_line: &mut Vec<Option<String>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
    created_at: Option<&str>,
    images_dir: &std::path::Path,
    image_cache: &mut HashMap<(String, usize), Vec<Line<'static>>>,
) {
    let card_w = (width * 3 / 5)
        .clamp(24, 64)
        .min(width.saturating_sub(6).max(24));
    let inner = card_w.saturating_sub(4);
    let bg_style = Style::default().bg(theme.raised);

    let mut card: Vec<Line<'static>> = Vec::new();
    let mut card_img: Vec<Option<String>> = Vec::new();

    let mut head = vec![Span::styled(
        format!("{} you", glyph::YOU),
        Style::default()
            .fg(theme.user_msg)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(t) = created_at {
        let time = super::fmt_created(t);
        let used: usize = head
            .iter()
            .map(|s| crate::ui::style::width(&s.content))
            .sum();
        let pad = inner.saturating_sub(used + 1 + crate::ui::style::width(&time));
        head.push(Span::raw(" ".repeat(pad)));
        head.push(Span::styled(time, Style::default().fg(theme.fg_dim)));
    }
    card.push(Line::from(head));
    card_img.push(None);

    render_markdown_images(
        &mut card,
        content,
        inner,
        theme,
        images_dir,
        &mut card_img,
        image_cache,
        None,
    );
    for line in wrap_plain(&strip_markdown_images(content), inner) {
        card.push(Line::from(Span::raw(line)));
        card_img.push(None);
    }

    // The rail keeps the bubble reading as a card even when the raised shade
    // is the terminal's own (transparent) background.
    let lead = width.saturating_sub(card_w);
    let rail = Style::default().fg(theme.user_msg).patch(bg_style);
    for (li, line) in card.into_iter().enumerate() {
        let len: usize = line
            .spans
            .iter()
            .map(|s| crate::ui::style::width(&s.content))
            .sum();
        let is_image = card_img[li].is_some();
        let mut spans = Vec::with_capacity(4);
        if lead > 0 {
            spans.push(Span::raw(" ".repeat(lead)));
        }
        spans.push(Span::styled(format!("{} ", glyph::RAIL), rail));
        for sp in line.spans {
            // Image rows carry their own per-pixel backgrounds — the card
            // tint must not override them.
            if is_image {
                spans.push(sp);
            } else {
                spans.push(Span::styled(sp.content, sp.style.patch(bg_style)));
            }
        }
        spans.push(Span::styled(
            " ".repeat(card_w.saturating_sub(len + 2)),
            bg_style,
        ));
        out.push(Line::from(spans));
        image_at_line.push(card_img[li].clone());
    }
    out.push(Line::from(""));
    image_at_line.push(None);
}

#[cfg(test)]
pub(super) fn push_tool_call(
    out: &mut Vec<Line<'static>>,
    content: &str,
    expanded: bool,
    settings: &nexus_core::app::Settings,
    width: usize,
    theme: &crate::theme::Theme,
    env: CardEnv<'_>,
) {
    push_tool_call_with_rows(out, content, expanded, settings, width, theme, env, None);
}

/// A compaction digest at the compaction boundary, dimmed so the live
/// conversation leads.
pub(super) fn push_compaction(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let agent = Style::default().fg(theme.accent2);
    out.push(style::event_head(
        glyph::DIGEST,
        agent,
        "earlier messages, summarized".into(),
        agent.add_modifier(Modifier::BOLD),
    ));
    for line in wrap_plain(content, width.saturating_sub(2)) {
        out.push(style::event_body(&line, Style::default().fg(theme.fg_dim)));
    }
    out.push(Line::from(""));
}

/// Memory named for the message below it: one dim line of chapter and doc
/// titles.
pub(super) fn push_recall(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let Some(recall) = nexus_core::memory::recall::Recall::from_row(content) else {
        return;
    };
    let names = recall
        .targets
        .iter()
        .map(|t| t.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let dim = Style::default().fg(theme.fg_dim);
    let label: String = format!("memory{}{names}", style::SEP)
        .chars()
        .take(width.saturating_sub(2).max(8))
        .collect();
    out.push(style::event_head(glyph::DIGEST, dim, label, dim));
}

/// A user-invoked skill: show its name and scope, never its instruction body.
pub(super) fn push_skill(
    out: &mut Vec<Line<'static>>,
    content: &str,
    _width: usize,
    theme: &crate::theme::Theme,
) {
    let name = content.lines().next().unwrap_or_default();
    let you = Style::default().fg(theme.accent);
    let label = format!("skill {name}{}active for this session", style::SEP);
    out.push(style::event_head(glyph::TOOL, you, label, you));
    out.push(Line::from(""));
}

/// A command the user ran with `!`: the command, how it ended, and the head of
/// its output.
pub(super) fn push_user_command(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    const MAX_OUTPUT_LINES: usize = 12;
    let row: serde_json::Value = serde_json::from_str(content).unwrap_or_default();
    let cmd = row["cmd"].as_str().unwrap_or_default();
    let outcome = nexus_core::app::user_command_outcome(&row);
    let you = Style::default().fg(theme.accent);
    out.push(style::event_head(
        glyph::YOU,
        you,
        format!("$ {cmd}{}{outcome}", style::SEP),
        you,
    ));
    let output = row["output"].as_str().unwrap_or_default().trim_end();
    if !output.is_empty() {
        let meta = Style::default().fg(theme.fg_dim);
        let lines = wrap_plain(output, width.saturating_sub(2));
        for line in lines.iter().take(MAX_OUTPUT_LINES) {
            out.push(style::event_body(line, meta));
        }
        if lines.len() > MAX_OUTPUT_LINES {
            out.push(style::event_body(
                &format!("… {} more lines", lines.len() - MAX_OUTPUT_LINES),
                meta,
            ));
        }
    }
    out.push(Line::from(""));
}

/// A transient row while a compaction request runs, deliberately not a
/// `Message`: a failed job must leave no fake turn behind.
pub(super) fn push_compaction_pending(
    out: &mut Vec<Line<'static>>,
    _width: usize,
    theme: &crate::theme::Theme,
) {
    let agent = Style::default().fg(theme.accent2);
    out.push(style::event_head(
        glyph::RUNNING,
        agent,
        "summarizing earlier messages…".into(),
        agent.add_modifier(Modifier::BOLD),
    ));
    out.push(Line::from(""));
}

/// A research progress row: the stage and its detail, dimmed.
pub(super) fn push_research_stage(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let meta = Style::default().fg(theme.fg_dim);
    let mut lines = wrap_plain(content, width.saturating_sub(2)).into_iter();
    let head = lines.next().unwrap_or_default();
    out.push(style::event_head(
        glyph::RESEARCH,
        Style::default().fg(theme.accent2),
        head,
        meta,
    ));
    for line in lines {
        out.push(style::event_body(&line, meta));
    }
    out.push(Line::from(""));
}

/// A persistent request failure, kept in the transcript after the status bar
/// moves on.
pub(super) fn push_error(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let err = Style::default().fg(theme.error);
    let mut lines = wrap_plain(content, width.saturating_sub(2)).into_iter();
    let head = lines.next().unwrap_or_else(|| "request failed".into());
    out.push(style::event_head(
        glyph::FAIL,
        err.add_modifier(Modifier::BOLD),
        head,
        err,
    ));
    for line in lines {
        out.push(style::event_body(&line, err));
    }
    out.push(Line::from(""));
}

/// Clarifying questions waiting on your chat answer.
pub(super) fn push_survey_section(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let you = Style::default().fg(theme.accent);
    let mut lines = wrap_plain(content, width.saturating_sub(2)).into_iter();
    let head = lines.next().unwrap_or_default();
    out.push(style::event_head(
        glyph::QUESTION,
        you.add_modifier(Modifier::BOLD),
        head,
        you.add_modifier(Modifier::BOLD),
    ));
    // The trailing guidance ("Answer in chat…") stays dim across its lines.
    let mut footer = false;
    for line in lines {
        footer |= line.starts_with("Answer in chat");
        let style = if footer {
            Style::default().fg(theme.fg_dim)
        } else {
            Style::default().fg(theme.fg)
        };
        out.push(style::event_body(&line, style));
    }
    out.push(Line::from(""));
}

/// A plan waiting on your approval, in the same accent as a question round.
pub(super) fn push_research_plan(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let you = Style::default().fg(theme.accent);
    let mut lines = wrap_plain(content, width.saturating_sub(2)).into_iter();
    let head = lines.next().unwrap_or_default();
    out.push(style::event_head(
        glyph::PLAN,
        you.add_modifier(Modifier::BOLD),
        head,
        you.add_modifier(Modifier::BOLD),
    ));
    for line in lines {
        out.push(style::event_body(&line, Style::default().fg(theme.fg)));
    }
    out.push(Line::from(""));
}

/// A stored assistant reply: header, reasoning, images, markdown answer, then a
/// dim stats/phrase footer.
#[allow(clippy::too_many_lines)] // one card shape per content kind
#[allow(clippy::too_many_arguments)]
pub(super) fn push_assistant_stored(
    out: &mut Vec<Line<'static>>,
    image_at_line: &mut Vec<Option<String>>,
    content: &str,
    msg: &Message,
    settings: &nexus_core::app::Settings,
    width: usize,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
    theme: &crate::theme::Theme,
    images_dir: &std::path::Path,
    image_cache: &mut HashMap<(String, usize), Vec<Line<'static>>>,
    regions: Regions<'_>,
) {
    // A persona overrides the model name and takes the agent color.
    let mut head = vec![Span::styled(
        format!("{} ", glyph::ASSISTANT),
        Style::default()
            .fg(theme.accent2)
            .add_modifier(Modifier::BOLD),
    )];
    match (&msg.persona, msg.model.as_deref()) {
        (Some(p), Some(m)) => {
            head.push(Span::styled(
                p.clone(),
                Style::default()
                    .fg(theme.accent2)
                    .add_modifier(Modifier::BOLD),
            ));
            head.push(dim(
                format!(" · {}", crate::ui::short_model_label(m)),
                theme,
            ));
        }
        (Some(p), None) => {
            head.push(Span::styled(
                p.clone(),
                Style::default()
                    .fg(theme.accent2)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        (None, Some(m)) => {
            head.push(Span::styled(
                crate::ui::short_model_label(m),
                Style::default()
                    .fg(theme.assistant_msg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        (None, None) => {
            head.push(Span::styled(
                "assistant",
                Style::default()
                    .fg(theme.assistant_msg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    if let Some(t) = &msg.created_at {
        let time = super::fmt_created(t);
        let head_len: usize = head
            .iter()
            .map(|s| crate::ui::style::width(&s.content))
            .sum();
        let pad = width.saturating_sub(head_len + 1 + crate::ui::style::width(&time));
        head.push(Span::raw(" ".repeat(pad)));
        head.push(Span::styled(time, Style::default().fg(theme.fg_dim)));
    }
    out.push(Line::from(head));

    // A quiet rail for stored replies; only the live reply's rail is bright.
    let rail = Span::styled(
        format!("{} ", glyph::RAIL),
        Style::default().fg(theme.border_dim),
    );
    if let Some(r) = &msg.reasoning {
        if settings.show_reasoning {
            out.push(Line::from(vec![
                rail.clone(),
                dim(format!("{} reasoning", glyph::EXPANDED), theme),
            ]));
            for line in wrap_plain(r, width.saturating_sub(2)) {
                out.push(Line::from(vec![rail.clone(), dim(line, theme)]));
            }
        } else {
            let n = r.chars().count();
            let hint = if settings.hide_hints {
                ""
            } else {
                " — Ctrl+R to expand"
            };
            out.push(Line::from(vec![
                rail.clone(),
                dim(
                    format!("{} reasoning ({n} chars){hint}", glyph::SELECTED),
                    theme,
                ),
            ]));
        }
    }

    render_markdown_images(
        out,
        content,
        width.saturating_sub(2),
        theme,
        images_dir,
        image_at_line,
        image_cache,
        Some(&rail),
    );

    let mut rendered = crate::ui::markdown::render(
        &strip_markdown_images(content),
        // Rail (2) plus a one-column margin before the scrollbar gutter.
        width.saturating_sub(3),
        md_colors(theme),
    );
    rendered.lines = crate::ui::citations_style::style_citations(rendered.lines, theme.accent);
    rendered.lines = crate::ui::citations_style::style_confidence_tags(rendered.lines);
    push_rendered(out, code, blocks, rendered, Some(rail), regions);

    let mut stats: Vec<String> = Vec::new();
    if settings.show_stats
        && let (Some(tok), Some(secs)) = (msg.tokens, msg.secs)
    {
        let tps = if secs > 0.0 { tok as f64 / secs } else { 0.0 };
        stats.push(format!("{tps:.1} tok/s · ~{tok} tok · {secs:.1}s"));
    }
    if settings.show_stats
        && let Some(cost) = msg.cost.filter(|c| *c > 0.0)
    {
        stats.push(fmt_cost(Some(cost)));
    }
    let mut footer: Vec<Span> = Vec::new();
    if let Some(p) = &msg.phrase {
        footer.push(Span::styled(
            p.clone(),
            Style::default()
                .fg(theme.fg_dim)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    if !stats.is_empty() {
        let sep = if footer.is_empty() { "" } else { " · " };
        footer.push(dim(format!("{sep}{}", stats.join(" · ")), theme));
    }
    if !footer.is_empty() {
        footer.insert(0, dim("· ", theme));
        out.push(Line::from(footer));
    }
    out.push(Line::from(""));
}

/// A link to another session; content is `<target_sid>\n<label>`.
pub(super) fn push_session_link(
    out: &mut Vec<Line<'static>>,
    content: &str,
    _width: usize,
    theme: &crate::theme::Theme,
) {
    let (sid, label) = match content.split_once('\n') {
        Some((sid, rest)) => (sid.to_string(), rest.trim().to_string()),
        None => (String::new(), content.to_string()),
    };
    // Stored link rows may lead with an emoji instead of the glyph.
    let label = label
        .trim_start_matches(['\u{1F517}', '↩', ' '])
        .to_string();
    let you = Style::default().fg(theme.accent);
    out.push(style::event_head(glyph::LINK, you, label, you));
    if !sid.is_empty() {
        out.push(style::event_body(
            "select this line + Ctrl+O to open",
            Style::default().fg(theme.fg_dim),
        ));
    }
    out.push(Line::from(""));
}

/// The in-progress reply: spinner header, thinking block, then the live
/// markdown stream.
pub(super) fn push_assistant_streaming(
    out: &mut Vec<Line<'static>>,
    app: &AppView,
    width: usize,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
    regions: Regions<'_>,
) {
    let color = to_color(app.spinner_color());
    let name = app.active_chat_task().map_or_else(
        || "assistant".to_string(),
        |t| crate::ui::short_model_label(&t.model),
    );
    let mut head = vec![
        Span::styled(
            format!("{} ", app.spinner_char()),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{name} — {}", app.thinking_phrase()),
            Style::default().fg(color),
        ),
    ];
    if let Some(t) = app.active_chat_task() {
        let secs = t.started.elapsed().as_secs();
        let time = format!("{}:{:02}", secs / 60, secs % 60);
        let used: usize = head
            .iter()
            .map(|s| crate::ui::style::width(&s.content))
            .sum();
        let pad = width.saturating_sub(used + 1 + crate::ui::style::width(&time));
        head.push(Span::raw(" ".repeat(pad)));
        head.push(Span::styled(time, Style::default().fg(app.theme.fg_dim)));
    }
    out.push(Line::from(head));

    let rail = Span::styled(format!("{} ", glyph::RAIL), Style::default().fg(color));
    if let Some(t) = app.thinking_text() {
        for line in wrap_plain(t, width.saturating_sub(2)) {
            out.push(Line::from(vec![rail.clone(), dim(line, &app.theme)]));
        }
    }

    let buf = app.active_streaming_text().unwrap_or("");
    render_markdown_images(
        out,
        buf,
        width.saturating_sub(3),
        &app.theme,
        &app.space.files_dir(&app.active_space.name),
        &mut Vec::new(),
        &mut app.stream_image_cache.borrow_mut(),
        Some(&rail),
    );
    let mut rendered = crate::ui::markdown::render(
        &strip_markdown_images(buf),
        width.saturating_sub(3),
        md_colors(&app.theme),
    );
    rendered.lines = crate::ui::citations_style::style_citations(rendered.lines, app.theme.accent);
    rendered.lines = crate::ui::citations_style::style_confidence_tags(rendered.lines);
    push_rendered(out, code, blocks, rendered, Some(rail), regions);
    out.push(Line::from(""));
}

/// Splice a `markdown::Rendered` into the running line/code/block vecs,
/// offsetting local block ids and widget rows to global ones.
pub(super) fn push_rendered(
    out: &mut Vec<Line<'static>>,
    code: &mut Vec<Option<usize>>,
    blocks: &mut Vec<String>,
    r: crate::ui::markdown::Rendered,
    rail: Option<Span<'static>>,
    regions: Regions<'_>,
) {
    let Regions { diagrams, widgets } = regions;
    widgets.extend(r.widgets.into_iter().map(|mut placement| {
        placement.row += out.len();
        placement
    }));
    diagrams.extend(r.diagrams.into_iter().map(|mut placement| {
        placement.row += out.len();
        placement
    }));
    code.resize(out.len(), None); // align past any dot/reasoning lines
    let base = blocks.len();
    code.extend(r.code.iter().map(|c| c.map(|id| id + base)));
    blocks.extend(r.blocks);
    if let Some(rail) = rail {
        for line in r.lines {
            let mut line = line;
            line.spans.insert(0, rail.clone());
            out.push(line);
        }
    } else {
        out.extend(r.lines);
    }
}

/// Render `![alt](file)` refs inline, resolving each against `images_dir` and
/// caching by (path, width). `prefix` (the assistant rail) is carried by every
/// pushed line so the gutter stays continuous.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_markdown_images(
    out: &mut Vec<Line<'static>>,
    content: &str,
    width: usize,
    theme: &crate::theme::Theme,
    images_dir: &std::path::Path,
    image_at_line: &mut Vec<Option<String>>,
    image_cache: &mut HashMap<(String, usize), Vec<Line<'static>>>,
    prefix: Option<&Span<'static>>,
) {
    image_at_line.resize(out.len(), None);
    for (_, file, alt) in markdown_images(content) {
        let remote = file.contains("://");
        let path = if remote {
            match crate::app_view::remote_image(&file) {
                nexus_core::remote_image::Status::Ready(path) => path,
                status => {
                    let mut line =
                        crate::app_view::image_placeholder(&file, &alt, &status, theme, width);
                    if let Some(p) = prefix {
                        line.spans.insert(0, p.clone());
                    }
                    out.push(line);
                    image_at_line.push(Some(file));
                    continue;
                }
            }
        } else {
            images_dir.join(&file)
        };
        let path_str = path.to_string_lossy().to_string();
        let open_target = if remote { file } else { path_str.clone() };
        let key = (path_str.clone(), width);
        let half = image_cache
            .entry(key)
            .or_insert_with(|| image_to_halfblock_lines(&path_str, width));
        if half.len() <= 1
            && half
                .first()
                .map(std::string::ToString::to_string)
                .unwrap_or_default()
                .contains("[image]")
        {
            let mut line = Line::from(dim(format!("{} {alt}", glyph::MEDIA), theme));
            if let Some(p) = prefix {
                line.spans.insert(0, p.clone());
            }
            out.push(line);
            image_at_line.push(Some(open_target));
        } else {
            let img_start = out.len();
            for l in half.clone() {
                let mut l = l;
                if let Some(p) = prefix {
                    l.spans.insert(0, p.clone());
                }
                out.push(l);
            }
            let img_end = out.len();
            let mut blank = Line::from("");
            if let Some(p) = prefix {
                blank.spans.insert(0, p.clone());
            }
            out.push(blank);
            for _ in img_start..img_end {
                image_at_line.push(Some(open_target.clone()));
            }
            image_at_line.push(None);
        }
    }
}

/// Max cell-rows a rendered image occupies, so it can't eat the whole terminal.
pub(crate) const MAX_IMAGE_ROWS: usize = 20;

/// Render a PNG as half-block lines, or a text marker when it can't be loaded.
pub(super) fn image_to_halfblock_lines(path: &str, max_width: usize) -> Vec<Line<'static>> {
    let Ok(img) = image::open(path) else {
        return vec![Line::from(Span::raw(format!("{} [image]", glyph::MEDIA)))];
    };
    if max_width < 4 {
        return vec![Line::from(Span::raw(glyph::MEDIA))];
    }
    let mut cell_w = max_width.min(img.width() as usize);
    let aspect = f64::from(img.width()) / f64::from(img.height());
    let mut cell_h = (cell_w as f64 / aspect / 2.0).round().max(1.0) as usize;
    let truncated = cell_h > MAX_IMAGE_ROWS;
    if truncated {
        cell_h = MAX_IMAGE_ROWS - 1;
        let capped_w = (cell_h as f64 * 2.0 * aspect).round().max(1.0) as usize;
        cell_w = cell_w.min(capped_w);
    }
    let pixel_w = cell_w;
    let pixel_h = (cell_h * 2).max(2);
    let resized = img.resize_exact(
        pixel_w as u32,
        pixel_h as u32,
        image::imageops::FilterType::Lanczos3,
    );
    let mut lines: Vec<Line<'static>> = Vec::new();
    for y in (0..pixel_h).step_by(2) {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(pixel_w);
        for x in 0..pixel_w {
            let top = resized.get_pixel(x as u32, y as u32);
            let bottom = if y + 1 < pixel_h {
                resized.get_pixel(x as u32, (y + 1) as u32)
            } else {
                image::Rgba([0, 0, 0, 0])
            };
            let fg = ratatui::style::Color::Rgb(top[0], top[1], top[2]);
            let bg = ratatui::style::Color::Rgb(bottom[0], bottom[1], bottom[2]);
            spans.push(Span::styled("▀", Style::default().fg(fg).bg(bg)));
        }
        lines.push(Line::from(spans));
    }
    if truncated {
        lines.push(Line::from(Span::styled(
            format!("{} click to open in viewer", glyph::MEDIA),
            ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
        )));
    }
    lines
}

pub(super) fn wrap_plain(content: &str, width: usize) -> Vec<String> {
    let w = width.max(1);
    let mut out = Vec::new();
    let content = crate::ui::markdown::terminal_safe(content);
    for raw in content.split('\n') {
        // Continuation lines sit under the item's text, not back at the margin.
        let hang = " ".repeat(list_hang(raw));
        let opts = textwrap::Options::new(w).subsequent_indent(&hang);
        for piece in textwrap::wrap(raw, opts) {
            out.push(piece.into_owned());
        }
    }
    out
}

/// Columns before a list item's text, or the plain indent for a non-list line.
fn list_hang(line: &str) -> usize {
    let body = line.trim_start();
    let indent = line.len() - body.len();
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    let marker = if digits > 0 && matches!(body[digits..].chars().next(), Some('.' | ')')) {
        digits + 1
    } else if body.starts_with(['-', '*', '+', '•']) {
        body.chars().next().map_or(0, char::len_utf8)
    } else {
        0
    };
    if marker > 0 && body[marker..].starts_with(' ') {
        // `•` is multi-byte but one column wide.
        indent + body[..marker].chars().count() + 1
    } else {
        indent
    }
}

/// The markdown palette for the transcript, from the theme.
fn md_colors(theme: &crate::theme::Theme) -> crate::ui::markdown::MdColors {
    crate::ui::markdown::MdColors {
        heading: theme.accent,
        rule: theme.border_dim,
        code: theme.warning,
        syntax: crate::ui::style::SyntaxColors::from_theme(theme),
    }
}

#[cfg(test)]
mod batch_card_tests;

#[cfg(test)]
mod tool_card_tests;

#[cfg(test)]
mod image_reference_tests;
