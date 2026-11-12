//! Tool-call and batch cards: the call row, its expanded body per tool kind,
//! and batch digests.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashMap;

use super::{md_colors, render_markdown_images, strip_markdown_images, wrap_plain};
use crate::ui::dim;
use crate::ui::style::{self, glyph};

/// What a tool card needs beyond its text: where result images live and whether
/// a studio window is attached.
pub(in crate::ui) struct CardEnv<'a> {
    pub files_dir: &'a std::path::Path,
    pub studio_linked: bool,
    pub image_at_line: &'a mut Vec<Option<String>>,
    pub image_cache: &'a mut HashMap<(String, usize), Vec<Line<'static>>>,
}

/// Per-row folds and hit regions; shared by every transcript tool card.
pub(in crate::ui) struct BatchRows<'a> {
    pub owner: usize,
    pub open: &'a std::collections::HashSet<(usize, usize)>,
    pub focus: Option<(usize, usize)>,
    pub hits: &'a mut Vec<crate::history_cache::BatchRow>,
}

#[allow(clippy::too_many_arguments)]
pub(in crate::ui) fn push_tool_call_with_rows(
    out: &mut Vec<Line<'static>>,
    content: &str,
    expanded: bool,
    settings: &nexus_core::app::Settings,
    width: usize,
    theme: &crate::theme::Theme,
    mut env: CardEnv<'_>,
    rows: Option<BatchRows<'_>>,
) {
    let v: serde_json::Value = serde_json::from_str(content).unwrap_or_default();
    let field = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let (name, args, result) = (field("name"), field("arguments"), field("result"));
    push_round_preamble(
        out,
        &[field("reasoning_content"), field("thinking")],
        &field("assistant_content"),
        settings,
        width,
        theme,
    );
    let view = nexus_core::app::tool_call_view(&name, &args, &result);
    let header = out.len();
    push_workspace_call(out, &view, expanded, settings, width, theme);
    if let Some(panel) = v.get("panel").and_then(serde_json::Value::as_str)
        && let Some(line) = out.get_mut(header)
    {
        line.spans.insert(
            0,
            Span::styled(format!("{panel} · "), Style::default().fg(theme.fg_dim)),
        );
    }
    if !view.batch.is_empty() {
        if expanded {
            push_batch_body(out, &view, width, theme, &mut env, rows);
            out.push(Line::default());
        } else {
            push_batch_digest(out, &view, width, theme);
        }
        return;
    }
    if expanded {
        let args = serde_json::from_str(&args).unwrap_or_default();
        let body_width = width.saturating_sub(2).max(1);
        let body = expanded_tool_body(&name, &args, &view, &result, body_width, theme);
        let mut body = body
            .into_iter()
            .flat_map(|row| {
                super::diff::styled_rows(&row, body_width, Style::default().fg(theme.fg_dim))
            })
            .collect();
        cap_tool_body(&mut body, body_width, theme);
        let rail = Span::styled(
            format!("{} ", glyph::RAIL),
            Style::default().fg(if view.outcome == nexus_core::app::ToolOutcome::Ok {
                theme.accent2
            } else {
                theme.error
            }),
        );
        for mut row in body {
            row.spans.insert(0, rail.clone());
            out.push(row);
        }
        push_slide_thumbnails(out, &view, width, theme, &rail, env);
        out.push(Line::default());
    } else if is_deck_change(&view) && !env.studio_linked && !settings.hide_hints {
        out.push(Line::from(Span::styled(
            "   open in studio · /studio",
            Style::default().fg(theme.fg_dim),
        )));
    }
}

/// Collapsed batches have a grouped digest and, if needed, one error line.
fn push_batch_digest(
    out: &mut Vec<Line<'static>>,
    view: &nexus_core::app::ToolView,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let dim = Style::default().fg(theme.fg_dim);
    let error = Style::default().fg(theme.error);
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for entry in &view.batch {
        let tool = match entry.tool.as_str() {
            "fetch_url" => "fetch",
            other => other,
        };
        let target = if entry.view.verb.is_empty() && !entry.action.is_empty() {
            format!("{} {}", entry.action, entry.target)
        } else {
            entry.target.clone()
        };
        if let Some((_, targets)) = groups.iter_mut().find(|(name, _)| *name == tool) {
            targets.push(target);
        } else {
            groups.push((tool, vec![target]));
        }
    }
    let digest = groups
        .into_iter()
        .map(|(tool, targets)| {
            let mut shown = targets
                .iter()
                .take(2)
                .map(|target| super::popups::chrome::truncate(target, 29))
                .collect::<Vec<_>>()
                .join(", ");
            if targets.len() > 2 {
                shown.push_str(" …");
            }
            format!("{tool} ×{} {shown}", targets.len())
        })
        .collect::<Vec<_>>()
        .join(style::SEP);
    out.push(batch_line(&format!("   {digest}"), width, dim, theme));
    let errors = view
        .batch
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry
                .status
                .is_some_and(|status| status != nexus_core::app::ToolOutcome::Ok)
        })
        .map(|(index, entry)| format!("{} {}: {}", index + 1, entry.tool, entry.outcome))
        .collect::<Vec<_>>()
        .join(style::SEP);
    let error_text = view
        .output
        .as_deref()
        .unwrap_or(&errors)
        .lines()
        .next()
        .unwrap_or_default();
    if !error_text.is_empty() {
        out.push(batch_line(&format!("   {error_text}"), width, error, theme));
    }
}

fn batch_line(text: &str, width: usize, role: Style, theme: &crate::theme::Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for line in super::diff::escaped_rows(text, usize::MAX, role, Style::default().fg(theme.fg_dim))
    {
        spans.extend(line.spans);
    }
    super::popups::chrome::fit_line(Line::from(spans), width)
}

fn batch_row_header(
    entry: &nexus_core::app::BatchEntry,
    index: usize,
    width: usize,
    theme: &crate::theme::Theme,
    open: bool,
    focused: bool,
) -> Line<'static> {
    use nexus_core::app::ToolOutcome;
    let dim = Style::default().fg(theme.fg_dim);
    let failed = entry.status.is_some_and(|status| status != ToolOutcome::Ok);
    let role = Style::default().fg(if failed { theme.error } else { theme.accent2 });
    let status = match entry.status {
        Some(ToolOutcome::Ok) => "ok",
        Some(ToolOutcome::Failed) => "failed",
        Some(ToolOutcome::Denied | ToolOutcome::Blocked) => "blocked",
        None => "not shown",
    };
    let detail = if failed {
        format!("{status}: {}", entry.outcome)
    } else {
        status.into()
    };
    let detail = if entry.truncated {
        format!("{detail} · truncated")
    } else {
        detail
    };
    let summary = if entry.result.is_some() {
        entry.view.summary()
    } else {
        format!("{} {} {}", entry.tool, entry.action, entry.target)
            .trim()
            .to_string()
    };
    let summary = if entry.view.verb.is_empty()
        || entry.view.verb.starts_with(&entry.tool)
        || (entry.tool == "fetch_url" && entry.view.verb == "fetch")
    {
        summary
    } else {
        format!("{}{}{}", entry.tool, style::SEP, summary)
    };
    let mut head = vec![
        Span::styled(format!("{:>2}  ", index + 1), dim),
        Span::styled(format!("{} ", call_glyph(&entry.view, failed)), role),
    ];
    let text = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg)
    };
    head.extend(batch_line(&summary, usize::MAX, text, theme).spans);
    // Leave room for the status and the row's fold affordance.
    let detail = batch_line(
        &detail,
        width.saturating_sub(24).max(8),
        if failed { role } else { dim },
        theme,
    );
    let mut header =
        super::popups::chrome::fit_line(Line::from(head), width.saturating_sub(detail.width() + 7));
    header
        .spans
        .insert(0, Span::styled(format!("{} ", glyph::RAIL), role));
    header.spans.push(Span::styled(style::SEP, dim));
    header.spans.extend(detail.spans);
    let padding = width.saturating_sub(header.width() + 1);
    header.spans.push(Span::raw(" ".repeat(padding)));
    header.spans.push(Span::styled(
        if open {
            glyph::EXPANDED
        } else {
            glyph::SELECTED
        },
        dim,
    ));
    super::popups::chrome::fit_line(header, width)
}

/// Each sub-call reuses its standalone body, capped independently of its peers.
fn push_batch_body(
    out: &mut Vec<Line<'static>>,
    view: &nexus_core::app::ToolView,
    width: usize,
    theme: &crate::theme::Theme,
    env: &mut CardEnv<'_>,
    mut rows: Option<BatchRows<'_>>,
) {
    use nexus_core::app::ToolOutcome;
    let dim = Style::default().fg(theme.fg_dim);
    let body_width = width.saturating_sub(4).max(1);
    if let Some(error) = &view.output {
        out.push(batch_line(
            error.lines().next().unwrap_or_default(),
            width,
            Style::default().fg(theme.error),
            theme,
        ));
    }
    for (index, entry) in view.batch.iter().enumerate() {
        let start = out.len();
        let open = rows
            .as_ref()
            .is_some_and(|rows| rows.open.contains(&(rows.owner, index)));
        let focused = rows
            .as_ref()
            .is_some_and(|rows| rows.focus == Some((rows.owner, index)));
        let failed = entry.status.is_some_and(|status| status != ToolOutcome::Ok);
        let role = Style::default().fg(if failed { theme.error } else { theme.accent2 });
        let rail = Span::styled(format!("{}   ", glyph::RAIL), role);
        out.push(batch_row_header(entry, index, width, theme, open, focused));
        if open {
            let result = entry
                .result
                .as_deref()
                .unwrap_or("result not shown (combined result cap)");
            let visible_result = strip_markdown_images(result);
            let body = if entry.result.is_some() {
                expanded_tool_body(
                    &entry.tool,
                    &entry.arguments,
                    &entry.view,
                    &visible_result,
                    body_width,
                    theme,
                )
            } else {
                vec![batch_line(result, body_width, dim, theme)]
            };
            let mut body = body
                .into_iter()
                .flat_map(|row| super::diff::styled_rows(&row, body_width, dim))
                .collect();
            cap_tool_body(&mut body, body_width, theme);
            for mut row in body {
                row.spans.insert(0, rail.clone());
                out.push(row);
            }
            // Deck images are already parsed; other tools return Markdown refs.
            push_slide_thumbnails(
                out,
                &entry.view,
                width.saturating_sub(2),
                theme,
                &rail,
                CardEnv {
                    files_dir: env.files_dir,
                    studio_linked: env.studio_linked,
                    image_at_line: env.image_at_line,
                    image_cache: env.image_cache,
                },
            );
            if entry.view.images.is_empty() {
                render_markdown_images(
                    out,
                    result,
                    body_width,
                    theme,
                    env.files_dir,
                    env.image_at_line,
                    env.image_cache,
                    Some(&rail),
                );
            }
        }
        if let Some(rows) = &mut rows {
            rows.hits.push(crate::history_cache::BatchRow {
                owner: rows.owner,
                index,
                lines: start..out.len(),
            });
        }
    }
}

/// Whether the call created or edited a deck or doc that studio can open.
fn is_deck_change(view: &nexus_core::app::ToolView) -> bool {
    view.outcome == nexus_core::app::ToolOutcome::Ok
        && (view.verb.starts_with("created deck")
            || view.verb.starts_with("created doc")
            || view.verb.starts_with("edited "))
}

/// The slides a `canvas` call rendered, drawn inline in the opened card.
fn push_slide_thumbnails(
    out: &mut Vec<Line<'static>>,
    view: &nexus_core::app::ToolView,
    width: usize,
    theme: &crate::theme::Theme,
    rail: &Span<'static>,
    env: CardEnv<'_>,
) {
    let CardEnv {
        files_dir,
        image_at_line,
        image_cache,
        ..
    } = env;
    for (alt, file) in &view.images {
        // Contact sheets hold several slides; give them more columns.
        let columns = if alt.ends_with(" slides") { 60 } else { 40 };
        render_markdown_images(
            out,
            &format!("![{alt}]({file})"),
            width.saturating_sub(2).clamp(8, columns),
            theme,
            files_dir,
            image_at_line,
            image_cache,
            Some(rail),
        );
    }
}

/// What the model thought and said before a round of tool calls, drawn above
/// the round's first call.
pub(in crate::ui) fn push_round_preamble(
    out: &mut Vec<Line<'static>>,
    reasoning: &[String],
    said: &str,
    settings: &nexus_core::app::Settings,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let reasoning = reasoning
        .iter()
        .map(|text| text.trim())
        .find(|text| !text.is_empty());
    if let Some(reasoning) = reasoning {
        // The same rail as a reply's reasoning, so live thinking keeps its look.
        let rail = Span::styled(
            format!("{} ", glyph::RAIL),
            Style::default().fg(theme.border_dim),
        );
        if settings.show_reasoning {
            out.push(Line::from(vec![
                rail.clone(),
                dim(format!("{} reasoning", glyph::EXPANDED), theme),
            ]));
            for line in wrap_plain(reasoning, width.saturating_sub(2)) {
                out.push(Line::from(vec![rail.clone(), dim(line, theme)]));
            }
        } else {
            let hint = if settings.hide_hints {
                ""
            } else {
                " — Ctrl+R to expand"
            };
            out.push(Line::from(vec![
                rail,
                dim(
                    format!(
                        "{} reasoning ({} chars){hint}",
                        glyph::SELECTED,
                        reasoning.chars().count()
                    ),
                    theme,
                ),
            ]));
        }
    }
    let said = said.trim();
    if !said.is_empty() {
        for line in wrap_plain(said, width) {
            out.push(Line::from(Span::styled(
                line,
                Style::default().fg(theme.fg),
            )));
        }
    }
}

#[allow(clippy::too_many_lines)] // shared header roles and per-tool collapsed previews
fn push_workspace_call(
    out: &mut Vec<Line<'static>>,
    view: &nexus_core::app::ToolView,
    expanded: bool,
    settings: &nexus_core::app::Settings,
    width: usize,
    theme: &crate::theme::Theme,
) {
    use nexus_core::app::ToolOutcome;
    let failed = view.outcome != ToolOutcome::Ok;
    let dim = Style::default().fg(theme.fg_dim);
    let mut head = vec![Span::styled(
        format!("{} ", call_glyph(view, failed)),
        Style::default().fg(if failed { theme.error } else { theme.accent2 }),
    )];
    let mut append = |text: String, style: Style| {
        for line in super::diff::escaped_rows(&text, usize::MAX, style, dim) {
            head.extend(line.spans);
        }
    };
    append(
        if view.batch.is_empty() {
            format!("{} {}", view.verb, view.target)
                .trim_start()
                .to_string()
        } else {
            view.verb.clone()
        },
        if view.verb.is_empty() {
            dim
        } else {
            Style::default().fg(theme.fg)
        },
    );
    if view.verb.is_empty() && !expanded && !settings.hide_hints {
        append(" — Ctrl+T for detail".to_string(), dim);
    }
    if let Some(added) = view.added {
        append(format!("  +{added}"), Style::default().fg(theme.success));
    }
    if let Some(deleted) = view.deleted {
        append(format!(" −{deleted}"), Style::default().fg(theme.error));
    }
    for stat in &view.stats {
        append(
            format!("{}{stat}", style::SEP),
            if failed
                && (stat.starts_with("exit")
                    || matches!(stat.as_str(), "failed" | "denied" | "blocked")
                    || (!view.batch.is_empty()
                        && (stat.ends_with(" failed") || stat.ends_with(" blocked"))))
            {
                Style::default().fg(theme.error)
            } else {
                dim
            },
        );
    }
    let mut header = Line::from(head);
    if header.width() + 2 <= width {
        header
            .spans
            .push(Span::raw(" ".repeat(width - header.width() - 1)));
        header.spans.push(Span::styled(
            if expanded {
                glyph::EXPANDED
            } else {
                glyph::SELECTED
            },
            dim,
        ));
    }
    out.push(header);
    if expanded {
        return;
    }
    let body_width = width.saturating_sub(3);
    let body = if let Some(diff) = &view.diff {
        super::diff::render(
            diff,
            body_width,
            theme,
            super::diff::Options {
                max_rows: Some(8),
                context: 1,
                hint: if settings.hide_hints {
                    ""
                } else {
                    " · Ctrl+T"
                },
            },
        )
    } else if let Some(output) = &view.output {
        let rows: Vec<_> = output
            .split('\n')
            .flat_map(|line| super::diff::escaped_rows(line, body_width, dim, dim))
            .collect();
        if view.verb == "$" && failed {
            let skip = rows.len().saturating_sub(5);
            rows.into_iter().skip(skip).collect()
        } else {
            Vec::new()
        }
    } else if !view.sources.is_empty() {
        source_domains(view, body_width, theme)
    } else {
        Vec::new()
    };
    for mut row in body {
        row.spans.insert(0, Span::raw("   "));
        out.push(row);
    }
}

fn call_glyph(view: &nexus_core::app::ToolView, failed: bool) -> &'static str {
    if failed {
        glyph::FAIL
    } else if view.connector {
        glyph::CONNECTOR
    } else if is_web(view) {
        glyph::WEB
    } else {
        glyph::TOOL
    }
}

fn is_web(view: &nexus_core::app::ToolView) -> bool {
    matches!(
        view.verb.as_str(),
        "search" | "news" | "papers" | "discussions" | "images" | "fetch"
    )
}

/// Collapsed search card: the distinct source domains, in result order.
fn source_domains(
    view: &nexus_core::app::ToolView,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let mut domains: Vec<&str> = Vec::new();
    for source in &view.sources {
        if !source.domain.is_empty() && !domains.contains(&source.domain.as_str()) {
            domains.push(&source.domain);
        }
    }
    let shown = domains.len().min(4);
    let mut text = domains[..shown].join(style::SEP);
    if domains.len() > shown {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("{}+{}", style::SEP, domains.len() - shown),
        );
    }
    let dim = Style::default().fg(theme.fg_dim);
    let mut rows = super::diff::escaped_rows(&text, usize::MAX, dim, dim);
    rows.truncate(1);
    if let Some(row) = rows.first_mut() {
        *row = super::popups::chrome::fit_line(row.clone(), width);
    }
    rows
}

/// Opened search card: each hit's title, domain and the start of its text.
fn sources_body(
    view: &nexus_core::app::ToolView,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let mut rows = tool_rule("results", width, theme);
    for (index, source) in view.sources.iter().enumerate() {
        let start = rows.len();
        let thumb_width = if source.image.is_some() {
            width.saturating_sub(6).min(12)
        } else {
            0
        };
        let gutter = if source.image.is_some() {
            thumb_width + 2
        } else {
            0
        };
        let text_width = width.saturating_sub(5 + gutter).max(1);
        let number = format!("{:>3}  ", index + 1);
        let pad = " ".repeat(number.chars().count());
        for (row, line) in wrap_plain(&source.title, text_width)
            .into_iter()
            .enumerate()
        {
            let lead = if row == 0 {
                number.clone()
            } else {
                pad.clone()
            };
            let mut spans = vec![Span::styled(lead, dim)];
            for part in
                super::diff::escaped_rows(&line, usize::MAX, Style::default().fg(theme.fg), dim)
            {
                spans.extend(part.spans);
            }
            rows.push(Line::from(spans));
        }
        if !source.domain.is_empty() {
            let mut spans = vec![Span::raw(pad.clone())];
            for part in super::diff::escaped_rows(
                &source.domain,
                usize::MAX,
                Style::default().fg(theme.accent2),
                dim,
            ) {
                spans.extend(part.spans);
            }
            rows.push(Line::from(spans));
        }
        for line in wrap_plain(&source.snippet, text_width).into_iter().take(2) {
            let mut spans = vec![Span::raw(pad.clone())];
            for part in super::diff::escaped_rows(&line, usize::MAX, dim, dim) {
                spans.extend(part.spans);
            }
            rows.push(Line::from(spans));
        }
        if let Some(image) = &source.image {
            let thumbnails =
                crate::interactive::image_lines(image, "", thumb_width as u16, 3, theme);
            rows.resize(rows.len().max(start + 3), Line::default());
            for (row, line) in rows[start..].iter_mut().enumerate() {
                let mut thumbnail = thumbnails.get(row).cloned().unwrap_or_default();
                // A text marker can outgrow a thumbnail; clip so it cannot
                // push the result out of its column.
                if thumbnail.width() > thumb_width {
                    thumbnail = Line::from(glyph::MEDIA);
                }
                let padding = gutter.saturating_sub(thumbnail.width());
                thumbnail.spans.push(Span::raw(" ".repeat(padding)));
                thumbnail.spans.append(&mut line.spans);
                *line = thumbnail;
            }
        }
        if index + 1 < view.sources.len() {
            rows.push(Line::default());
        }
    }
    rows
}

fn tool_rule(label: &str, width: usize, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let rule = Style::default().fg(theme.border_dim);
    let dim = Style::default().fg(theme.fg_dim);
    let mut rows = super::diff::escaped_rows(&format!("── {label} ──"), width, rule, dim);
    if let Some(last) = rows.last_mut() {
        last.spans.push(Span::styled(
            "─".repeat(width.saturating_sub(last.width())),
            rule,
        ));
    }
    rows
}

#[allow(clippy::too_many_lines)] // one dispatch for the emitted tool formats
fn expanded_tool_body(
    name: &str,
    args: &serde_json::Value,
    view: &nexus_core::app::ToolView,
    result: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    if let Some(diff) = &view.diff {
        let mut rows = tool_rule("diff", width, theme);
        rows.extend(super::diff::expanded(diff, width, theme));
        return rows;
    }
    if !view.sources.is_empty() {
        return sources_body(view, width, theme);
    }
    let tool = name.split('_').next().unwrap_or(name);
    if matches!(tool, "canvas" | "deck") && !view.verb.is_empty() {
        return view
            .output
            .as_deref()
            .unwrap_or_default()
            .split('\n')
            .flat_map(|line| super::diff::escaped_rows(line, width, dim, dim))
            .collect();
    }
    if matches!(tool, "shell" | "proc") && !view.verb.is_empty() {
        return command_body(args, view, result, width, theme);
    }
    if tool == "fs" && view.verb == "read" {
        return read_body(args["path"].as_str().unwrap_or(""), result, width, theme);
    }
    if tool == "fs" && view.verb == "grep" {
        return grep_body(args["pattern"].as_str().unwrap_or(""), result, width, theme);
    }
    if tool == "fs" && matches!(view.verb.as_str(), "glob" | "list") {
        return result
            .split('\n')
            .flat_map(|line| {
                let style = if line.ends_with('/') {
                    Style::default().fg(theme.accent)
                } else {
                    dim
                };
                super::diff::escaped_rows(line, width, style, dim)
            })
            .collect();
    }
    if name == "task" {
        let report = if result.starts_with("sub-agent report (") {
            result.split_once('\n').map_or("", |(_, report)| report)
        } else {
            result
        };
        let mut rows = tool_markdown(report, width, theme);
        rows.extend(super::diff::escaped_rows(
            "/agents — its reasoning, tool calls and cost",
            width,
            dim,
            dim,
        ));
        return rows;
    }
    let mut rows = Vec::new();
    if let Some(args) = args.as_object() {
        let keys: Vec<_> = args
            .keys()
            .map(|key| super::diff::escaped_rows(key, usize::MAX, dim, dim).remove(0))
            .collect();
        let key_width = keys.iter().map(Line::width).max().unwrap_or(0);
        for ((_, value), mut key) in args.iter().zip(keys) {
            let padding = key_width.saturating_sub(key.width()) + 2;
            key.spans.push(Span::raw(" ".repeat(padding)));
            let value = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            key.spans
                .push(Span::styled(value, Style::default().fg(theme.fg)));
            rows.extend(super::diff::styled_rows(&key, width, dim));
        }
    }
    rows.extend(tool_rule("result", width, theme));
    rows.extend(
        result
            .split('\n')
            .flat_map(|line| super::diff::escaped_rows(line, width, dim, dim)),
    );
    rows
}

fn command_body(
    args: &serde_json::Value,
    view: &nexus_core::app::ToolView,
    result: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let command = args["cmd"].as_str().map_or_else(
        || format!("{} {}", view.verb, view.target),
        |cmd| format!("$ {cmd}"),
    );
    let mut rows = super::diff::escaped_rows(
        &command,
        width,
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        dim,
    );
    if let Some(cwd) = args["cwd"].as_str() {
        let dir = super::diff::escaped_rows(&format!("in {cwd}"), width, dim, dim);
        let last = rows.last_mut().expect("command has a row");
        if dir.len() == 1 && last.width() + dir[0].width() < width {
            last.spans
                .push(Span::raw(" ".repeat(width - last.width() - dir[0].width())));
            last.spans.extend(dir[0].spans.clone());
        } else {
            rows.extend(dir);
        }
    }
    let output = view
        .output
        .as_deref()
        .unwrap_or(result)
        .trim_end_matches('\n');
    let count = if output.is_empty() {
        0
    } else {
        output.split('\n').count()
    };
    rows.extend(tool_rule(
        &format!("output{}{} lines", style::SEP, count),
        width,
        theme,
    ));
    if count > 0 {
        for line in output.split('\n') {
            let start = line.trim_start();
            let color = if ["error", "fatal", "FAILED", "panicked"]
                .iter()
                .any(|s| start.starts_with(s))
            {
                theme.error
            } else if start.starts_with("warning") {
                theme.warning
            } else {
                theme.fg_dim
            };
            rows.extend(super::diff::escaped_rows(
                line,
                width,
                Style::default().fg(color),
                dim,
            ));
        }
    }
    rows
}

fn grep_body(
    pattern: &str,
    result: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let matched = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut rows = Vec::new();
    for line in result.trim_end().split('\n') {
        if !line.starts_with(' ') && line.ends_with(':') {
            rows.extend(super::diff::escaped_rows(
                &line[..line.len() - 1],
                width,
                Style::default().fg(theme.accent),
                dim,
            ));
        } else if let Some((number, text)) = line.trim_start().split_once(": ")
            && number.parse::<usize>().is_ok()
        {
            let mut spans = vec![Span::styled(format!("{number:>4}  "), dim)];
            let mut cursor = 0;
            if !pattern.is_empty() {
                for (index, found) in text.match_indices(pattern) {
                    spans.push(Span::styled(text[cursor..index].to_string(), dim));
                    spans.push(Span::styled(found.to_string(), matched));
                    cursor = index + found.len();
                }
            }
            spans.push(Span::styled(text[cursor..].to_string(), dim));
            rows.extend(super::diff::styled_rows(&Line::from(spans), width, dim));
        } else {
            rows.extend(super::diff::escaped_rows(line, width, dim, dim));
        }
    }
    rows
}

pub(super) fn read_body(
    path: &str,
    result: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");
    let lang = match extension {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "sh" | "bash" => "bash",
        "json" | "toml" | "yaml" | "yml" | "html" | "css" | "c" | "cpp" | "go" | "java" | "rb"
        | "sql" | "md" => extension,
        _ => "plain",
    };
    let mut rows = tool_rule(&format!("{path} ── {lang}"), width, theme);
    if !result
        .split('\n')
        .next()
        .unwrap_or_default()
        .contains(" (lines ")
    {
        let dim = Style::default().fg(theme.fg_dim);
        rows.extend(
            result
                .split('\n')
                .flat_map(|line| super::diff::escaped_rows(line, width, dim, dim)),
        );
        return rows;
    }
    let entries: Vec<_> = result
        .split('\n')
        .skip(1)
        .map(|line| {
            if let Some((number, text)) = line.trim_start().split_once(':')
                && number.parse::<usize>().is_ok()
            {
                let text = text
                    .split_once('\t')
                    .filter(|(hash, _)| {
                        hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit())
                    })
                    .map_or_else(|| text.strip_prefix(' ').unwrap_or(text), |(_, text)| text);
                (number, text)
            } else {
                ("", line)
            }
        })
        .collect();
    let source = entries
        .iter()
        .map(|(_, text)| *text)
        .collect::<Vec<_>>()
        .join("\n");
    let highlighted = highlight_read(&source, lang, theme);
    let digits = entries.iter().map(|(n, _)| n.len()).max().unwrap_or(1);
    let dim = Style::default().fg(theme.fg_dim);
    for ((number, _), mut source) in entries.iter().zip(highlighted) {
        dim_tool_escapes(std::slice::from_mut(&mut source), dim, theme.border_dim);
        for (index, mut line) in
            super::diff::styled_rows(&source, width.saturating_sub(digits + 2), dim)
                .into_iter()
                .enumerate()
        {
            line.spans.insert(
                0,
                Span::styled(
                    if index == 0 {
                        format!("{number:>digits$}: ")
                    } else {
                        " ".repeat(digits + 2)
                    },
                    dim,
                ),
            );
            rows.extend(super::diff::styled_rows(&line, width, dim));
        }
    }
    rows
}

/// Highlight the whole file slice before adding gutters or wrapping its rows.
fn highlight_read(source: &str, lang: &str, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    crate::ui::style::highlighted_lines(
        &markdown_tool_text(source),
        lang,
        crate::ui::style::SyntaxColors::from_theme(theme),
        Style::default().fg(theme.fg),
    )
}

/// Sanitize before Markdown parsing, which can otherwise discard controls.
fn markdown_tool_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\t' {
                "    ".to_string()
            } else if c.is_control() && c != '\n' {
                c.escape_unicode().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Escapes may be split across spans or rows, so ignore box/table chrome when
/// locating their text.
fn dim_tool_escapes(lines: &mut [Line<'static>], dim: Style, rule: ratatui::style::Color) {
    let chrome = |span: &Span<'_>| {
        span.style.fg == Some(rule) && span.content.chars().all(|c| " │┌┐└┘─├┤┬┴┼".contains(c))
    };
    let text: String = lines
        .iter()
        .flat_map(|line| line.spans.iter().filter(|span| !chrome(span)))
        .map(|span| span.content.as_ref())
        .collect();
    let ranges: Vec<_> = text
        .match_indices("\\u{")
        .filter_map(|(start, _)| {
            let end = text[start + 3..].find('}')? + start + 3;
            text[start + 3..end]
                .chars()
                .all(|c| c.is_ascii_hexdigit())
                .then_some(start..end + 1)
        })
        .collect();
    if ranges.is_empty() {
        return;
    }
    let mut offset = 0;
    for line in lines {
        let mut spans = Vec::new();
        for span in std::mem::take(&mut line.spans) {
            if chrome(&span) {
                spans.push(span);
                continue;
            }
            for c in span.content.chars() {
                let style = if ranges.iter().any(|range| range.contains(&offset)) {
                    dim
                } else {
                    line.style.patch(span.style)
                };
                spans.push(Span::styled(c.to_string(), style));
                offset += c.len_utf8();
            }
        }
        line.spans = spans;
    }
}

pub(in crate::ui) fn tool_markdown(
    text: &str,
    width: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.fg_dim);
    let mut lines =
        crate::ui::markdown::render(&markdown_tool_text(text), width, md_colors(theme)).lines;
    dim_tool_escapes(&mut lines, dim, theme.border_dim);
    lines
        .into_iter()
        .flat_map(|line| super::diff::styled_rows(&line, width, dim))
        .collect()
}

fn cap_tool_body(rows: &mut Vec<Line<'static>>, width: usize, theme: &crate::theme::Theme) {
    const MAX_ROWS: usize = 400;
    if rows.len() > MAX_ROWS {
        let total = rows.len();
        let dim = Style::default().fg(theme.fg_dim);
        let mut keep = MAX_ROWS - 1;
        loop {
            let notice = super::diff::escaped_rows(
                &format!("… {} more lines", total - keep),
                width,
                dim,
                dim,
            );
            if keep + notice.len() <= MAX_ROWS {
                rows.truncate(keep);
                rows.extend(notice);
                break;
            }
            keep = MAX_ROWS.saturating_sub(notice.len());
        }
    }
}
