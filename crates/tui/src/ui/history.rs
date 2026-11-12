// Casts here are on terminal-bounded values (dims, byte colors, glyph counts),
// never on unbounded user data.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::cards::{
    push_assistant_stored, push_assistant_streaming, push_compaction, push_compaction_pending,
    push_error, push_recall, push_research_plan, push_research_stage, push_session_link,
    push_skill, push_survey_section, push_tool_call_with_rows, push_user_card, push_user_command,
};
use super::welcome::render_welcome;
use crate::app_view::AppView;
use crate::history_cache::HistoryCache;
use crate::ui::markdown::line_text;

#[allow(clippy::too_many_lines)] // whole conversation view: header, day dividers, cards, stats
pub(super) fn render_history(f: &mut Frame, app: &mut AppView, area: Rect) {
    // The rightmost column is the scrollbar gutter, always reserved so lines
    // don't shift when the scrollbar appears.
    let inner = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(1).max(1),
        height: area.height,
    };
    if app.is_welcome() {
        app.max_scroll = 0;
        // Record an empty layout so a click here can't map onto the previous
        // session's stale snapshot and open one of its URLs.
        app.sel.record_render(
            Rect::default(),
            0,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        render_welcome(f, app, area);
        return;
    }
    app.welcome_targets.clear();
    let width = inner.width.max(1) as usize;
    let old_image_generation = app.history_cache.image_generation;
    sync_cache(app, width);
    let images_changed = old_image_generation != app.history_cache.image_generation;

    // The in-progress reply changes every frame, so it renders fresh after the
    // cached prefix.
    let mut tail: Vec<Line<'static>> = Vec::new();
    let mut tail_code: Vec<Option<usize>> = Vec::new();
    let mut tail_blocks: Vec<String> = Vec::new();
    let mut tail_diagrams = Vec::new();
    let mut tail_widgets = Vec::new();
    if app.viewing_stream() {
        push_assistant_streaming(
            &mut tail,
            app,
            width,
            &mut tail_code,
            &mut tail_blocks,
            super::cards::Regions {
                diagrams: &mut tail_diagrams,
                widgets: &mut tail_widgets,
            },
        );
    }
    if app.is_compacting_current_session() {
        push_compaction_pending(&mut tail, width, &app.theme);
    }
    tail_code.resize(tail.len(), None);

    let estimated_top = (app.history_cache.lines.len() + tail.len())
        .saturating_sub(usize::from(inner.height))
        .saturating_sub(app.scroll);
    app.interactive
        .reflow(&mut app.history_cache, estimated_top, inner.height);

    let cache = &app.history_cache;
    let cached_lines = cache.lines.len();
    let total = cached_lines + tail.len();

    // `app.scroll` counts lines scrolled up from the bottom (0 = follow).
    let height = inner.height as usize;
    app.history_height = height;
    let max_top = total.saturating_sub(height);
    app.max_scroll = max_top; // let the event loop clamp scrolling

    // The line count can change under the viewport, and a raw delta only pins
    // when all growth landed below it. Growth above shifts the content without
    // shifting the line index, so instead re-find the previous top line by
    // prefix (only the frontier lines grow) and pin the viewport back onto it.
    let prev_tail = std::mem::take(&mut app.prev_tail);
    let prev_top = app
        .prev_total
        .saturating_sub(height)
        .saturating_sub(app.scroll);
    let anchor: Option<&str> = if app.prev_total > 0 && !prev_tail.is_empty() {
        let prev_cached = app.prev_total.saturating_sub(prev_tail.len());
        prev_top
            .checked_sub(prev_cached)
            .and_then(|offset| prev_tail.get(offset).map(String::as_str))
    } else {
        None
    };

    let explicit_toggle = app.pin_viewport_top;
    let clicked = app.tool_click_anchor.take();
    app.pin_viewport_top = false;
    // While following the bottom the viewport tracks the newest lines, even
    // across the tail→cache transition, where a stored card renders taller
    // than the streaming tail.
    let pin_top = app.prev_total > 0 && (explicit_toggle || app.scroll > 0);
    // A toggle while following the bottom keeps following it: the re-wrap's
    // growth belongs above the viewport.
    let pin = pin_top && !(explicit_toggle && app.scroll == 0);
    if pin
        && !explicit_toggle
        && let Some(anchor) = anchor
        && anchor.chars().count() >= 12
    {
        // Scrolled up inside the streaming tail: pin to the content line, not
        // the line index. A short anchor is a frontier line still being typed,
        // where the delta path below is exact and a prefix could match wrongly.
        let prefix: String = anchor.chars().take(24).collect();
        // The anchor can only have moved into lines rendered since last frame.
        let search_from = app.prev_total.saturating_sub(prev_tail.len()).min(total);
        let mut hit: Option<usize> = None;
        let mut i = total;
        while i > search_from && hit.is_none() {
            i -= 1;
            let text = if i < cached_lines {
                line_text(&cache.lines[i])
            } else {
                line_text(&tail[i - cached_lines])
            };
            if text.starts_with(&prefix) {
                hit = Some(i);
                break;
            }
        }
        if let Some(hit) = hit {
            app.scroll = max_top.saturating_sub(hit);
        } else {
            let delta = total as i64 - app.prev_total as i64;
            if delta != 0 {
                app.scroll = (app.scroll as i64 + delta).clamp(0, app.max_scroll as i64) as usize;
            }
        }
    } else if pin && explicit_toggle {
        // A display-flag toggle re-wraps in place, so pin the viewport to the
        // message the previous top line belonged to, at the same offset within
        // it; the absolute line index would land on content shifted by all the
        // growth above.
        let mut pinned = clicked.and_then(|(owner, row, old_start)| {
            let new_start = row.map_or_else(
                || cache.owner.iter().position(|o| *o == Some(owner)),
                |index| {
                    cache
                        .batch_rows
                        .iter()
                        .find(|row| row.owner == owner && row.index == index)
                        .map(|row| row.lines.start)
                },
            );
            new_start.map(|new_start| (prev_top + new_start).saturating_sub(old_start))
        });
        if pinned.is_none() && images_changed {
            // Images can grow above the answer, so prefer surviving text over
            // the old offset within the message.
            for offset in 0..6 {
                if let Some(text) = app.sel.line_at(prev_top + offset)
                    && text.chars().count() >= 12
                    && let Some(hit) = find_line(&cache.lines, &tail, |line| line == text)
                {
                    pinned = Some(hit.saturating_sub(offset));
                    break;
                }
            }
        }
        if pinned.is_none()
            && let Some(owner) = app.sel.owner_at(prev_top)
        {
            // Walk up to the previous render's block start.
            let mut old_start = prev_top;
            while old_start > 0 && app.sel.owner_at(old_start - 1) == Some(owner) {
                old_start -= 1;
            }
            let offset = prev_top - old_start;
            // The in-flight reply's owner id is the message count at record
            // time, so its block lives in the tail.
            let (new_start, block_len) = if owner == app.messages.len() {
                (Some(cached_lines), tail.len())
            } else {
                let start = cache.owner.iter().position(|o| *o == Some(owner));
                let len = start.map_or(0, |s| {
                    cache.owner[s..]
                        .iter()
                        .take_while(|o| **o == Some(owner))
                        .count()
                });
                (start, len)
            };
            if let Some(new_start) = new_start {
                pinned = Some(new_start + offset.min(block_len.saturating_sub(1)));
            }
        }
        if pinned.is_none() {
            // Fallback: re-find the top line's text nearby — the top is often a
            // blank row or a header the toggle itself rewrites.
            'candidates: for off in [0, 1, 2, 3, 4, 5, -1, -2, -3, -4, -5] {
                let li = if off >= 0 {
                    prev_top + off as usize
                } else {
                    prev_top.saturating_sub((-off) as usize)
                };
                let Some(text) = app.sel.line_at(li) else {
                    continue;
                };
                if text.chars().count() < 12 {
                    continue;
                }
                // Exact text survives the re-wrap; a frontier line kept growing.
                if let Some(hit) = find_line(&cache.lines, &tail, |l| l == text) {
                    pinned = Some((hit as i64 - off).max(0) as usize);
                    break 'candidates;
                }
                let prefix: String = text.chars().take(24).collect();
                if let Some(hit) = find_line(&cache.lines, &tail, |l| l.starts_with(&prefix)) {
                    pinned = Some((hit as i64 - off).max(0) as usize);
                    break 'candidates;
                }
            }
        }
        if let Some(pinned) = pinned {
            app.scroll = max_top.saturating_sub(pinned);
        } else {
            let delta = total as i64 - app.prev_total as i64;
            if delta != 0 {
                app.scroll = (app.scroll as i64 + delta).clamp(0, app.max_scroll as i64) as usize;
            }
        }
    } else if pin {
        let delta = total as i64 - app.prev_total as i64;
        if delta != 0 {
            app.scroll = (app.scroll as i64 + delta).clamp(0, app.max_scroll as i64) as usize;
        }
    }
    app.prev_total = total;
    app.prev_tail = tail.iter().map(line_text).collect();

    app.scroll = app.scroll.min(app.max_scroll);
    let top = max_top.saturating_sub(app.scroll);

    // Snapshot layout, plain text and per-line owner so mouse selection can
    // map screen cells to a message.
    let cache = &app.history_cache;
    let mut plain = cache.plain.clone();
    plain.extend(tail.iter().map(line_text));
    let mut owner = cache.owner.clone();
    owner.resize(total, Some(app.messages.len()));
    let base = cache.blocks.len();
    let mut code = cache.code.clone();
    code.extend(tail_code.iter().map(|c| c.map(|id| id + base)));
    let mut blocks = cache.blocks.clone();
    blocks.append(&mut tail_blocks);
    app.sel
        .record_render(inner, top, plain, owner, code, blocks);

    let cache = &app.history_cache;
    let line_at = |i: usize| {
        if i < cached_lines {
            &cache.lines[i]
        } else {
            &tail[i - cached_lines]
        }
    };
    let hl = Style::default().bg(app.theme.selection).fg(app.theme.fg);
    let visible: Vec<Line> = (top..total.min(top + height))
        .map(|li| {
            app.sel
                .highlight(li, line_at(li), hl)
                .unwrap_or_else(|| line_at(li).clone())
        })
        .collect();

    f.render_widget(Paragraph::new(visible), inner);
    if app.popup == nexus_core::app::Popup::None {
        let regions = crate::pictures::regions(
            &app.history_cache.lines,
            &app.history_cache.image_at_line,
            top..(top + height).min(cached_lines),
        );
        app.pictures
            .paint(f.buffer_mut(), inner, top, &regions, |target| {
                if target.contains("://") {
                    match crate::app_view::remote_image(target) {
                        nexus_core::remote_image::Status::Ready(path) => Some(path),
                        _ => None,
                    }
                } else {
                    Some(std::path::PathBuf::from(target))
                }
            });
    }
    app.favicons.paint(
        f.buffer_mut(),
        inner,
        crate::app_view::favicon,
        app.popup == nexus_core::app::Popup::None,
    );
    if app.popup == nexus_core::app::Popup::None {
        let mut placements = app.history_cache.diagrams.clone();
        placements.extend(tail_diagrams.into_iter().map(|mut placement| {
            placement.row += cached_lines;
            placement
        }));
        app.diagrams.paint(f, inner, top, &placements, app.theme.bg);
    }

    let mut widgets = app.history_cache.widgets.clone();
    widgets.extend(tail_widgets.into_iter().map(|mut placement| {
        placement.row += cached_lines;
        placement.owner = app.messages.len();
        // Bind mutable state only to stored replies, never to a live task's
        // shifting message index.
        placement.complete = false;
        placement
    }));
    app.interactive.paint(
        f,
        inner,
        top,
        &widgets,
        &app.theme,
        matches!(
            app.popup,
            nexus_core::app::Popup::None | nexus_core::app::Popup::InteractiveUi
        ),
    );
    // Diagrams inside panels paint after the panels, over their placeholders.
    if app.popup == nexus_core::app::Popup::None {
        let placements = app.interactive.diagram_placements(&widgets, inner.width);
        if !placements.is_empty() {
            app.diagrams.paint(f, inner, top, &placements, app.theme.bg);
        }
    }

    // Hand-drawn: ratatui's `Scrollbar` thumb jumps on every streamed line and
    // never sits flush at the track bottom.
    if total > height {
        render_scrollbar(f, area, total, top, &app.theme);
    }
}

/// Draw the history scrollbar into the 1-column gutter at `area`'s right edge;
/// `total` is the rendered line count and `top` the first visible line.
fn render_scrollbar(
    f: &mut Frame,
    area: Rect,
    total: usize,
    top: usize,
    theme: &crate::theme::Theme,
) {
    let track = area.height as usize;
    if track == 0 {
        return;
    }
    let thumb_len = (track * track / total).clamp(1, track);
    // Map `top` onto the thumb's travel range so both scroll extremes land flush.
    let room = total.saturating_sub(track);
    let thumb_top = (top * (track - thumb_len)).checked_div(room).unwrap_or(0);
    let gutter = Rect {
        x: area.x + area.width.saturating_sub(1),
        y: area.y,
        width: 1,
        height: area.height,
    };
    let thumb_style = Style::default().fg(theme.accent);
    let track_style = Style::default().fg(theme.border_dim);
    let lines: Vec<Line> = (0..track)
        .map(|y| {
            let (symbol, style) = if y >= thumb_top && y < thumb_top + thumb_len {
                ("█", thumb_style)
            } else {
                ("║", track_style)
            };
            Line::from(Span::styled(symbol, style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), gutter);
}

/// Scan the rendered lines from the bottom for the last one matching `pred`.
fn find_line(
    cache: &[Line<'static>],
    tail: &[Line<'static>],
    pred: impl Fn(&str) -> bool,
) -> Option<usize> {
    let total = cache.len() + tail.len();
    let mut i = total;
    while i > 0 {
        i -= 1;
        let text = if i < cache.len() {
            line_text(&cache[i])
        } else {
            line_text(&tail[i - cache.len()])
        };
        if pred(&text) {
            return Some(i);
        }
    }
    None
}

// Long by design (cache sync).
#[allow(clippy::too_many_lines)]
/// Bring the cached wrapped prefix up to date, wrapping only messages not yet
/// cached: stored messages are append-only, so this is O(new) per frame.
fn sync_cache(app: &mut AppView, width: usize) {
    let image_generation = app.activate_image_context();
    if app.history_cache.msg_count != app.messages.len() {
        app.stream_image_cache.borrow_mut().clear();
    }
    if app.history_cache.msg_count > 0 && app.history_cache.image_generation != image_generation {
        app.pin_viewport_top = true;
    }
    let session = app.session.as_ref().map(|s| s.id.clone());
    if app.tool_session != session {
        app.tool_session.clone_from(&session);
        app.batch_focus = None;
        if !app.tool_open.is_empty() || !app.batch_open.is_empty() {
            app.tool_open.clear();
            app.batch_open.clear();
            app.tool_open_gen = app.tool_open_gen.wrapping_add(1);
        }
        app.tool_click_anchor = None;
    }
    let key = (
        session,
        width,
        app.show_tool_detail,
        app.settings.show_reasoning,
        app.settings.hide_hints,
        app.settings.show_stats,
        // Both generations only advance; either change invalidates the key.
        app.theme_gen
            .wrapping_add(app.tool_open_gen)
            .wrapping_add(image_generation),
    );
    // Detach the cache: the wrap pass reads the domain half through the
    // `AppView` deref, which borrows the whole view.
    let mut cache = std::mem::take(&mut app.history_cache);
    let c = &mut cache;
    if c.key != key || app.messages.len() < c.msg_count {
        if c.key.0 != key.0 {
            if let Some(sid) = &c.key.0 {
                app.session_caches.insert(sid.clone(), std::mem::take(c));
            }
            if let Some(sid) = &key.0
                && let Some(cached) = app.session_caches.remove(sid)
                && cached.key == key
                && cached.msg_count == app.messages.len()
            {
                *c = cached;
                // Reattach before returning, or the view renders empty for a frame.
                app.history_cache = cache;
                return;
            }
            *c = HistoryCache {
                key,
                ..Default::default()
            };
        } else if app.messages.len() < c.msg_count || c.key.1 != key.1 || c.key.6 != key.6 {
            *c = HistoryCache {
                key,
                ..Default::default()
            };
        } else {
            // Only display flags changed, so keep the image cache.
            let ic = std::mem::take(&mut c.image_cache);
            *c = HistoryCache {
                key,
                ..Default::default()
            };
            c.image_cache = ic;
        }
    }
    let theme = app.theme;
    for (i, m) in app.messages.iter().enumerate().skip(c.msg_count) {
        let start = c.lines.len();
        let widget_start = c.widgets.len();
        if let Some(day) = m.created_at.as_deref().map(day_of)
            && c.last_day.as_deref() != Some(day)
        {
            if c.last_day.is_some() {
                rule_line(&mut c.lines, &day_label(day), width, &theme);
            }
            c.last_day = Some(day.to_string());
        }
        if m.role == "user" || m.role == "gate_reply" {
            let images_dir = app.space.files_dir(&app.active_space.name);
            push_user_card(
                &mut c.lines,
                &mut c.image_at_line,
                &m.content,
                width,
                &theme,
                m.created_at.as_deref(),
                &images_dir,
                &mut c.image_cache,
            );
        } else if m.role == "skill" {
            push_skill(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "recall" {
            push_recall(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "user_command" {
            push_user_command(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "compaction" {
            push_compaction(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "research_stage" {
            push_research_stage(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "error" {
            push_error(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "survey" {
            push_survey_section(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "research_plan" {
            push_research_plan(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "session_link" {
            push_session_link(&mut c.lines, &m.content, width, &theme);
        } else if m.role == "tool_call" {
            push_tool_call_with_rows(
                &mut c.lines,
                &m.content,
                app.tool_open
                    .get(&i)
                    .copied()
                    .unwrap_or(app.show_tool_detail),
                &app.settings,
                width,
                &theme,
                super::cards::CardEnv {
                    files_dir: &app.space.files_dir(&app.active_space.name),
                    studio_linked: app.studio.linked(),
                    image_at_line: &mut c.image_at_line,
                    image_cache: &mut c.image_cache,
                },
                Some(super::cards::BatchRows {
                    owner: i,
                    open: &app.batch_open,
                    focus: app.batch_focus,
                    hits: &mut c.batch_rows,
                }),
            );
        } else {
            push_assistant_stored(
                &mut c.lines,
                &mut c.image_at_line,
                &m.content,
                m,
                &app.settings,
                width,
                &mut c.code,
                &mut c.blocks,
                &theme,
                &app.space.files_dir(&app.active_space.name),
                &mut c.image_cache,
                super::cards::Regions {
                    diagrams: &mut c.diagrams,
                    widgets: &mut c.widgets,
                },
            );
        }
        for widget in &mut c.widgets[widget_start..] {
            widget.owner = i;
        }
        c.owner.resize(c.lines.len(), Some(i));
        c.code.resize(c.lines.len(), None);
        c.image_at_line.resize(c.lines.len(), None);
        let new_plain: Vec<String> = c.lines[start..].iter().map(line_text).collect();
        c.plain.extend(new_plain);
    }
    c.image_generation = image_generation;
    c.msg_count = app.messages.len();
    app.history_cache = cache;
}

/// Toggle only the tool card owning a rendered line, including its preamble.
pub(crate) fn toggle_tool_at_line(app: &mut AppView, line: usize) {
    let Some(owner) = app.sel.owner_at(line) else {
        return;
    };
    if app
        .messages
        .get(owner)
        .is_none_or(|m| m.role != "tool_call")
    {
        return;
    }
    if let Some(row) = app
        .history_cache
        .batch_rows
        .iter()
        .find(|row| row.lines.contains(&line))
    {
        let key = (owner, row.index);
        app.batch_focus = Some(key);
        if !app.batch_open.insert(key) {
            app.batch_open.remove(&key);
        }
        app.tool_click_anchor = Some((owner, Some(row.index), row.lines.start));
    } else {
        app.batch_focus = None;
        let open = app
            .tool_open
            .get(&owner)
            .copied()
            .unwrap_or(app.show_tool_detail);
        app.tool_open.insert(owner, !open);
        app.tool_click_anchor = app
            .history_cache
            .owner
            .iter()
            .position(|o| *o == Some(owner))
            .map(|start| (owner, None, start));
    }
    app.tool_open_gen = app.tool_open_gen.wrapping_add(1);
    app.pin_viewport_top = true;
    // A consumed card click must not turn its next click into word selection.
    app.sel.clear();
}

/// Enter folds the focused batch row without submitting an empty composer.
pub(crate) fn toggle_focused_batch_row(app: &mut AppView) {
    if let Some((owner, index)) = app.batch_focus
        && let Some(line) = app
            .history_cache
            .batch_rows
            .iter()
            .find(|row| row.owner == owner && row.index == index)
            .map(|row| row.lines.start)
    {
        toggle_tool_at_line(app, line);
    }
}

/// Arrow keys move between the opened batch's rows after a row is focused.
pub(crate) fn move_batch_focus(app: &mut AppView, down: bool) {
    let Some((owner, index)) = app.batch_focus else {
        return;
    };
    let next = if down {
        index.saturating_add(1)
    } else {
        index.saturating_sub(1)
    };
    if app
        .history_cache
        .batch_rows
        .iter()
        .any(|row| row.owner == owner && row.index == next)
    {
        app.batch_focus = Some((owner, next));
        app.tool_open_gen = app.tool_open_gen.wrapping_add(1);
    }
}

/// A centered `── label ──` rule.
pub(super) fn rule_line(
    out: &mut Vec<Line<'static>>,
    label: &str,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let label = format!(" {label} ");
    let dashes = width.saturating_sub(crate::ui::style::width(&label));
    let line = format!(
        "{}{}{}",
        "─".repeat(dashes / 2),
        label,
        "─".repeat(dashes - dashes / 2)
    );
    out.push(Line::from(Span::styled(
        line,
        Style::default().fg(theme.border_dim),
    )));
}

/// `2026-08-08T14:32:00Z` -> `2026-08-08`.
pub(super) fn day_of(rfc3339: &str) -> &str {
    rfc3339.get(0..10).unwrap_or(rfc3339)
}

/// Human label for a "YYYY-MM-DD" day: Today / Yesterday / weekday+date.
pub(super) fn day_label(day: &str) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if day == today {
        return "Today".into();
    }
    let yesterday = (chrono::Local::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    if day == yesterday {
        return "Yesterday".into();
    }
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_or_else(|_| day.to_string(), |d| d.format("%A, %B %-d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::{Db, Message};

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        }
    }

    fn test_app() -> AppView {
        let db = Db::open_in_memory().unwrap();
        let space = nexus_core::space::Space {
            root: std::env::temp_dir().join(format!("nexus-hist-{}", uuid::Uuid::new_v4())),
        };
        AppView::new(App::new(db, Some("k"), space))
    }

    /// Build an app with a session and a manually-inserted streaming task.
    fn streaming_app(thinking: &str, buffer: &str) -> AppView {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let s =
            a.db.create_session("stream test", "a/one", &space, "chat")
                .unwrap();
        let session = a.db.get_session(&s.id).unwrap().unwrap();
        a.session = Some(session);
        a.settings.show_reasoning = true;
        a.show_tool_detail = true;
        // Stored history so the transcript overflows the pane.
        for i in 0..8 {
            a.messages.push(msg(
                "user",
                &format!("earlier user message number {i} with a decent amount of body text"),
            ));
            a.messages.push(msg(
                "assistant",
                &format!(
                    "earlier assistant reply {i} with a longer body so it wraps over a few lines"
                ),
            ));
        }
        let abort = tokio::spawn(async {}).abort_handle();
        let task = nexus_core::app::ChatTask {
            id: 1,
            session_id: s.id.clone(),
            session_title: "stream test".into(),
            space_id: space,
            model: "a/one".into(),
            model_id: "one".into(),
            backend: nexus_core::provider::BackendTag::OpenRouter,
            incognito: false,
            buffer: buffer.into(),
            thinking: thinking.into(),
            tool_status: None,
            usage: None,
            usage_row_id: None,
            started: std::time::Instant::now(),
            thinking_idx: 0,
            spinner_color: nexus_core::app::SpinnerColor::Green,
            abort,
        };
        a.chat_tasks.insert(1, task);
        a
    }

    /// The rendered text of the first three rows of the history pane.
    fn visible_snapshot(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..3 {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                out.push_str(cell.symbol());
            }
            out.push('|');
        }
        out
    }

    #[tokio::test]
    async fn diagram_preview_renders_in_place_and_preserves_copy_metadata() {
        let mut a = test_app();
        let source = "flowchart LR\nA[Start] --> B[End]";
        a.messages.push(msg(
            "assistant",
            &format!("Before\n\n```mermaid\n{source}\n```\n\nAfter"),
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 40)).unwrap();
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        let rows = a.history_cache.lines.len();
        let preview = a.history_cache.diagrams[0].clone();
        let block = a.history_cache.code[preview.row].expect("copyable preview");
        assert_eq!(a.history_cache.blocks[block], source);
        let completion = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            a.diagrams.next_completion(),
        )
        .await
        .expect("renderer completed")
        .expect("completion");
        a.diagrams.complete(completion);
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        assert_eq!(a.history_cache.lines.len(), rows);
        assert_eq!(a.history_cache.blocks[block], source);
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol() == "▀")
        );
    }

    #[tokio::test]
    async fn late_diagram_completion_above_the_viewport_does_not_jump() {
        let mut a = test_app();
        let later = (0..40)
            .map(|i| format!("Later paragraph number {i} remains in the reading viewport.\n\n"))
            .collect::<String>();
        a.messages.push(msg(
            "assistant",
            &format!("```mermaid\nflowchart LR\nA-->B\n```\n\n{later}"),
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        a.scroll = usize::MAX;
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        a.scroll = 5;
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        let before = visible_snapshot(&terminal);
        assert!(before.contains("Later paragraph"), "{before}");
        let completion = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            a.diagrams.next_completion(),
        )
        .await
        .expect("renderer completed")
        .expect("completion");
        a.diagrams.complete(completion);
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        assert_eq!(visible_snapshot(&terminal), before);
    }

    #[tokio::test]
    async fn closing_a_streamed_diagram_keeps_source_and_viewport_stable() {
        let partial = "```mermaid\nflowchart LR\nA[Start] --> B[End]";
        let mut a = streaming_app("", partial);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        a.scroll = 2;
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        let before = visible_snapshot(&terminal);
        let total = a.prev_total;
        a.chat_tasks.get_mut(&1).unwrap().buffer.push_str("\n```");
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        assert_eq!(a.prev_total, total);
        assert_eq!(visible_snapshot(&terminal), before);
    }

    #[test]
    fn cache_appends_new_messages_and_resets_on_width_or_flag_change() {
        let mut a = test_app();
        a.messages.push(msg("user", "hello"));
        a.messages.push(msg("assistant", "**bold** answer"));
        sync_cache(&mut a, 80);
        let n = a.history_cache.lines.len();
        assert!(n > 0);
        assert_eq!(a.history_cache.msg_count, 2);
        assert_eq!(a.history_cache.plain.len(), n);

        a.messages.push(msg("user", "again"));
        sync_cache(&mut a, 80);
        assert!(a.history_cache.lines.len() > n);
        assert_eq!(a.history_cache.msg_count, 3);

        sync_cache(&mut a, 20);
        assert_eq!(a.history_cache.msg_count, 3);
        assert_eq!(a.history_cache.key.1, 20);

        a.show_tool_detail = true;
        let before = a.history_cache.lines.len();
        a.messages.push(msg(
            "tool_call",
            r#"{"name":"skill","arguments":"{}","result":"ok"}"#,
        ));
        sync_cache(&mut a, 20);
        assert!(a.history_cache.lines.len() > before);

        a.messages.clear();
        sync_cache(&mut a, 20);
        assert_eq!(a.history_cache.lines.len(), 0);
        assert_eq!(a.history_cache.msg_count, 0);
    }

    #[test]
    fn survey_section_renders_marker_header_and_dimmed_footer() {
        let mut a = test_app();
        a.messages.push(msg(
            "survey",
            "For \"fine-tuning LLMs\":\n 1. Depth or breadth?\n\nAnswer in chat — then say \"I approve\".",
        ));
        sync_cache(&mut a, 80);
        let text = a
            .history_cache
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("? "), "{text}");
        assert!(text.contains("fine-tuning LLMs"), "{text}");
        assert!(text.contains("1. Depth or breadth?"), "{text}");
        assert!(text.contains("Answer in chat"), "{text}");
        assert!(
            a.history_cache
                .plain
                .iter()
                .any(|l| l.contains("Depth or breadth?"))
        );
    }

    #[test]
    fn compaction_digest_renders_with_header_and_body() {
        let mut a = test_app();
        a.messages.push(msg("user", "long conversation…"));
        a.messages
            .push(msg("compaction", "digest line one\ndigest line two"));
        sync_cache(&mut a, 80);
        let text = a
            .history_cache
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("≡ "), "{text}");
        assert!(text.contains("earlier messages, summarized"), "{text}");
        assert!(text.contains("digest line one"), "{text}");
        assert!(text.contains("digest line two"), "{text}");
        assert!(
            a.history_cache
                .plain
                .iter()
                .any(|l| l.contains("digest line one")),
            "{text}"
        );
    }

    #[test]
    fn skill_row_renders_name_not_body() {
        let mut a = test_app();
        a.messages.push(msg("skill", "web-search\nSECRET BODY"));
        sync_cache(&mut a, 80);
        let text = a
            .history_cache
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("skill web-search"), "{text}");
        assert!(text.contains("active for this session"), "{text}");
        assert!(!text.contains("SECRET BODY"), "{text}");
    }

    #[test]
    fn user_command_card_shows_the_command_exit_code_and_at_most_twelve_lines() {
        let mut a = test_app();
        let output: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let content = serde_json::json!({
            "cmd": "cargo test",
            "cwd": "/w",
            "exit": 101,
            "output": output,
            "interactive": false,
        })
        .to_string();
        a.messages.push(msg("user_command", &content));
        sync_cache(&mut a, 80);
        let text = a
            .history_cache
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("❯ $ cargo test · exit 101"), "{text}");
        assert!(text.contains("  line 12\n"), "{text}");
        assert!(!text.contains("line 13"), "{text}");
        assert!(text.contains("… 8 more lines"), "{text}");
    }

    #[test]
    fn running_compaction_is_visible_in_the_history_pane() {
        let mut a = test_app();
        let session =
            a.db.create_session("compacting", "a/one", &a.active_space.id, "chat")
                .unwrap();
        a.session = Some(session.clone());
        a.messages.push(msg("user", "old conversation"));
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        a.compaction.rx = Some(rx);
        a.compaction.session_id = Some(session.id);

        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_history(f, &mut a, f.area()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect::<String>();
        assert!(text.contains("summarizing earlier messages"), "{text}");
    }

    /// A conversation tall enough to overflow the history pane, with one
    /// tool-call block whose detail toggle produces a large line delta.
    fn tall_tool_call_app() -> AppView {
        let mut a = test_app();
        for i in 0..60 {
            a.messages.push(msg(
                "user",
                &format!("message number {i} with some body text"),
            ));
        }
        let tool_content = serde_json::json!({
            "name": "search",
            "arguments": r#"{"mode":"web","query":"q"}"#,
            "result": "line one\n".repeat(40),
        })
        .to_string();
        a.messages.push(msg("tool_call", &tool_content));
        a
    }

    #[tokio::test]
    async fn streaming_reasoning_growth_keeps_viewport_on_reasoning() {
        use std::fmt::Write as _;
        let reasoning: String = (0..60)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, "The answer body.\nSecond answer line.");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        a.scroll = 8;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("reasoning line"),
            "viewport should be in the reasoning, got: {before}"
        );

        let task = a.chat_tasks.get_mut(&1).unwrap();
        for i in 0..20 {
            write!(
                task.thinking,
                "\nfresh reasoning token {i} some more words to wrap"
            )
            .unwrap();
        }
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "viewport content jumped while reading reasoning: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn streaming_reasoning_growth_keeps_viewport_on_answer() {
        use std::fmt::Write as _;
        let reasoning: String = (0..50)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let answer: String = (0..40)
            .map(|i| format!("answer paragraph line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, &answer);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        assert!(a.max_scroll > 0, "needs an overflowing transcript");
        a.scroll = 12;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("answer"),
            "viewport should be in the answer, got: {before}"
        );

        let task = a.chat_tasks.get_mut(&1).unwrap();
        for i in 0..30 {
            write!(
                task.thinking,
                "\nmore reasoning token {i} with some words to wrap"
            )
            .unwrap();
        }
        task.buffer.push_str(" more answer tokens here.");
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "viewport content jumped: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn streaming_tail_to_cache_transition_does_not_jump() {
        let mut a = streaming_app(
            "some reasoning text here\nmore of it",
            &"A final answer body with enough text to wrap over several lines. ".repeat(6),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        a.scroll = 3;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);

        a.on_chat_event(1, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "tail->cache transition jumped: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn streaming_tail_to_cache_transition_pins_inside_the_tail() {
        let reasoning: String = (0..40)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, "A final answer body line one.\nBody line two.");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        a.scroll = 5;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("reasoning line"),
            "viewport should be in the reasoning, got: {before}"
        );

        a.on_chat_event(1, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "tail->cache transition inside the tail jumped: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn tool_call_landing_above_viewport_keeps_position() {
        let reasoning: String = (0..60)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, "answer body line one\nanswer body line two");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        a.scroll = 10;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("reasoning line"),
            "viewport should be in the reasoning, got: {before}"
        );

        a.on_chat_event(
            1,
            nexus_core::provider::StreamEvent::ToolCall {
                id: "call_0".into(),
                reasoning: None,
                assistant_content: None,
                name: "web_search".into(),
                arguments: r#"{"query":"some search query text"}"#.into(),
                result: "result line one\nresult line two\nresult line three".into(),
            },
        )
        .unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();

        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "tool call landing above the viewport jumped: before={before:?} after={after:?}"
        );
    }

    #[tokio::test]
    async fn opened_batch_completion_above_a_streaming_viewport_does_not_jump() {
        let reasoning = (0..60)
            .map(|index| format!("reasoning line {index} with enough words to identify it"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut app = streaming_app(&reasoning, "answer line one\nanswer line two");
        app.show_tool_detail = true;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        app.scroll = 10;
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let before = visible_snapshot(&terminal);
        let args = serde_json::json!({"calls": (0..6).map(|index| serde_json::json!({"tool":"files","arguments":{"action":"read","name":format!("{index}.txt")}})).collect::<Vec<_>>()}).to_string();
        let result = (0..6)
            .map(|index| {
                format!(
                    "[{}/6] files/read {index}.txt\ncontents of file {index}",
                    index + 1
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        app.on_chat_event(
            1,
            nexus_core::provider::StreamEvent::ToolCall {
                id: "batch-complete".into(),
                reasoning: None,
                assistant_content: None,
                name: "batch".into(),
                arguments: args,
                result,
            },
        )
        .unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..24]),
            "batch completion jumped: before={before:?}, after={after:?}"
        );
        assert_eq!(app.history_cache.batch_rows.len(), 6);
    }

    #[test]
    fn opening_a_batch_row_keeps_its_screen_position_and_other_rows_closed() {
        let mut app = test_app();
        for index in 0..20 {
            app.messages
                .push(msg("assistant", &format!("Before batch message {index}.")));
        }
        app.messages.push(msg("tool_call", &serde_json::json!({
            "name":"batch", "arguments":serde_json::json!({"calls":[
                {"tool":"search","arguments":{"query":"needle"}},
                {"tool":"fetch_url","arguments":{"url":"https://example.test"}}
            ]}).to_string(),
            "result":"[1/2] search  \"needle\"\n[1] A result\n    https://example.test\n    A result snippet.\n[2/2] fetch_url https://example.test\nA page preview"
        }).to_string()));
        for index in 0..30 {
            app.messages
                .push(msg("assistant", &format!("After batch message {index}.")));
        }
        app.show_tool_detail = true;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render_history(frame, &mut app, frame.area()))
            .unwrap();
        let row = app.history_cache.batch_rows[0].lines.start;
        app.scroll = app.max_scroll - (row - 5);
        terminal
            .draw(|frame| render_history(frame, &mut app, frame.area()))
            .unwrap();
        for open in [true, false] {
            let line = app.sel.pos_at(4, 5).unwrap().0;
            toggle_tool_at_line(&mut app, line);
            terminal
                .draw(|frame| render_history(frame, &mut app, frame.area()))
                .unwrap();
            let screen: String = (0..79)
                .map(|x| terminal.backend().buffer()[(x, 5)].symbol())
                .collect();
            assert!(screen.contains("1  ◎ search \"needle\""), "{screen}");
            assert!(screen.contains(if open { '▾' } else { '▸' }));
            assert_eq!(app.batch_open.contains(&(20, 0)), open);
            assert!(!app.batch_open.contains(&(20, 1)));
            assert!(app.scroll > 0);
        }
        app.batch_open.insert((20, 0));
        let space = app.active_space.id.clone();
        app.session = Some(app.db.create_session("other", "m", &space, "chat").unwrap());
        sync_cache(&mut app, 79);
        assert!(app.batch_open.is_empty());
        assert_eq!(app.batch_focus, None);
    }

    #[test]
    fn switching_back_to_a_session_reuses_its_wrapped_history() {
        let mut a = tall_tool_call_app();
        let first =
            a.db.create_session("first", "m", &a.active_space.id, "chat")
                .unwrap();
        let second =
            a.db.create_session("second", "m", &a.active_space.id, "chat")
                .unwrap();
        a.session = Some(first.clone());
        sync_cache(&mut a, 80);
        let wrapped = a.history_cache.lines.len();
        assert!(wrapped > 0);
        let messages = std::mem::take(&mut a.messages);
        a.session = Some(second);
        sync_cache(&mut a, 80);
        a.messages = messages;
        a.session = Some(first);
        sync_cache(&mut a, 80);
        assert_eq!(
            a.history_cache.lines.len(),
            wrapped,
            "restored, not dropped"
        );
    }

    #[test]
    fn clicked_card_header_stays_on_its_screen_row_when_scrolled_up() {
        let mut app = tall_tool_call_app();
        for n in 0..40 {
            app.messages.push(msg(
                "assistant",
                &format!("Later message {n} after the clicked card."),
            ));
        }
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render_history(f, &mut app, f.area()))
            .unwrap();
        let header = app
            .history_cache
            .owner
            .iter()
            .position(|o| *o == Some(60))
            .unwrap();
        app.scroll = app.max_scroll - (header - 5);
        terminal
            .draw(|f| render_history(f, &mut app, f.area()))
            .unwrap();
        assert!(app.scroll > 0);
        for open in [true, false] {
            let line = app.sel.pos_at(3, 5).unwrap().0;
            toggle_tool_at_line(&mut app, line);
            terminal
                .draw(|f| render_history(f, &mut app, f.area()))
                .unwrap();
            let row: String = (0..79)
                .map(|x| terminal.backend().buffer()[(x, 5)].symbol())
                .collect();
            assert!(row.contains("search \"q\""), "{row}");
            assert!(row.contains(if open { '▾' } else { '▸' }), "{row}");
            assert!(app.scroll > 0);
        }
    }

    #[test]
    fn per_card_cache_generation_and_session_reset_do_not_leak_overrides() {
        let mut app = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render_history(f, &mut app, f.area()))
            .unwrap();
        let header = app
            .history_cache
            .owner
            .iter()
            .position(|o| *o == Some(60))
            .unwrap();
        let collapsed = app.history_cache.lines.len();
        toggle_tool_at_line(&mut app, header);
        terminal
            .draw(|f| render_history(f, &mut app, f.area()))
            .unwrap();
        assert!(app.history_cache.lines.len() > collapsed);
        assert_eq!(app.scroll, 0, "clicks at the bottom keep following it");
        app.invalidate_history_cache();
        terminal
            .draw(|f| render_history(f, &mut app, f.area()))
            .unwrap();
        assert_eq!(app.tool_open.get(&60), Some(&true));
        app.apply_event(&nexus_core::app::AppEvent::ViewportReset);
        assert!(app.tool_open.is_empty());
        terminal
            .draw(|f| render_history(f, &mut app, f.area()))
            .unwrap();
        assert_eq!(app.history_cache.lines.len(), collapsed);
        toggle_tool_at_line(&mut app, header);
        let space = app.active_space.id.clone();
        let session = app
            .db
            .create_session("other", "a/one", &space, "chat")
            .unwrap();
        app.session = Some(session);
        sync_cache(&mut app, 79);
        assert!(app.tool_open.is_empty());
    }

    #[test]
    fn flag_toggle_pins_viewport_when_scrolled_up() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total1 = a.history_cache.lines.len();
        assert!(a.max_scroll > 10, "conversation should overflow the pane");

        a.scroll = 5;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 5);
        a.show_tool_detail = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total2 = a.history_cache.lines.len();
        let delta = total2 as i64 - total1 as i64;
        assert!(delta > 20, "tool detail should add many lines");
        assert_eq!(
            a.scroll,
            5 + delta as usize,
            "viewport top must stay pinned"
        );

        a.show_tool_detail = false;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 5, "collapse must restore the pinned position");
    }

    #[test]
    fn flag_toggle_while_following_bottom_stays_at_bottom() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total1 = a.history_cache.lines.len();
        assert_eq!(a.scroll, 0, "starts following the bottom");

        a.show_tool_detail = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total2 = a.history_cache.lines.len();
        assert!(total2 > total1, "tool detail should add lines");
        assert_eq!(a.scroll, 0, "must keep following the bottom");
    }

    /// A tall conversation with reasoning traces on many assistant messages.
    fn tall_reasoning_app() -> AppView {
        let mut a = test_app();
        for i in 0..40 {
            a.messages.push(msg(
                "user",
                &format!("question number {i} with some body text"),
            ));
            let mut m = msg(
                "assistant",
                &format!("answer number {i} with a decent amount of body text"),
            );
            m.reasoning = Some(format!(
                "thinking trace {i} line one\nthinking trace {i} line two\nthinking trace {i} line three"
            ));
            m.model = Some("a/one".into());
            m.phrase = Some("working".into());
            a.messages.push(m);
        }
        a.settings.show_reasoning = false;
        a
    }

    #[test]
    fn flag_toggle_reasoning_while_scrolled_up_pins_content() {
        let mut a = tall_reasoning_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total1 = a.history_cache.lines.len();
        assert!(a.max_scroll > 10, "conversation should overflow the pane");

        a.scroll = 25;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("answer number 34"),
            "viewport should be in the middle of the conversation, got: {before}"
        );

        a.settings.show_reasoning = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total2 = a.history_cache.lines.len();
        assert!(total2 > total1, "reasoning should add lines");
        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "viewport content jumped on reasoning toggle: before={before:?} after={after:?}"
        );

        a.settings.show_reasoning = false;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let after2 = visible_snapshot(&terminal);
        assert!(
            after2.starts_with(&before[..before.len().min(24)]),
            "viewport content jumped on reasoning collapse: before={before:?} after={after2:?}"
        );
    }

    #[test]
    fn flag_toggle_reasoning_while_following_bottom_stays_at_bottom() {
        let mut a = tall_reasoning_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0, "starts following the bottom");

        a.settings.show_reasoning = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0, "must keep following the bottom");
        let buf = terminal.backend().buffer();
        let all: String = (0..HISTORY_H)
            .flat_map(|y| {
                (0..buf.area.width - 1).map(move |x| buf[(x, y as u16)].symbol().to_string())
            })
            .collect();
        assert!(
            all.contains("thinking trace 39"),
            "the last message's reasoning must be visible at the bottom"
        );
    }

    #[tokio::test]
    async fn flag_toggle_while_streaming_and_scrolled_into_tail_stays_pinned() {
        let reasoning: String = (0..60)
            .map(|i| format!("reasoning line {i} with a bunch of words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut a = streaming_app(&reasoning, "The answer body.\nSecond answer line.");
        a.messages.push(msg(
            "tool_call",
            r#"{"name":"search","arguments":"{}","result":"line one\nline two"}"#,
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        a.scroll = 8;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        assert!(
            before.contains("reasoning line"),
            "viewport should be in the tail, got: {before}"
        );

        a.show_tool_detail = true;
        a.pin_viewport_top = true;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let after = visible_snapshot(&terminal);
        assert!(
            after.starts_with(&before[..before.len().min(24)]),
            "viewport jumped on toggle while streaming: before={before:?} after={after:?}"
        );
    }

    /// The scrollbar gutter column (x = width - 1): (symbol, fg) per row.
    fn gutter_snapshot(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
    ) -> Vec<(String, ratatui::style::Color)> {
        let buf = terminal.backend().buffer();
        let x = crate::ui::reading_column(buf.area, "auto").right() - 1;
        (0..buf.area.height)
            .map(|y| (buf[(x, y)].symbol().to_string(), buf[(x, y)].fg))
            .collect()
    }

    /// Height of the history pane on an 80x24 terminal with an empty composer.
    const HISTORY_H: usize = 20;

    #[test]
    fn scrollbar_thumb_sits_flush_at_bottom_when_following() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0, "starts following the bottom");
        let gutter = gutter_snapshot(&terminal);
        assert_eq!(
            gutter[HISTORY_H - 1].1,
            a.theme.accent,
            "thumb must sit flush at the track bottom — no dead gap"
        );
        assert_eq!(gutter[0].1, a.theme.border_dim);
    }

    #[test]
    fn scrollbar_thumb_sits_flush_at_top_when_scrolled_to_start() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        a.scroll = a.max_scroll;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let gutter = gutter_snapshot(&terminal);
        assert_eq!(
            gutter[0].1, a.theme.accent,
            "thumb must sit flush at the track top when scrolled to the start"
        );
    }

    #[test]
    fn scrollbar_thumb_tracks_viewport_proportionally() {
        let mut a = tall_tool_call_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let total = a.history_cache.lines.len();
        assert!(total > HISTORY_H, "conversation should overflow the pane");
        a.scroll = a.max_scroll / 2;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let top = a.max_scroll - a.scroll;
        let thumb_len = (HISTORY_H * HISTORY_H / total).clamp(1, HISTORY_H);
        let expect_top = top * (HISTORY_H - thumb_len) / (total - HISTORY_H);
        let gutter = gutter_snapshot(&terminal);
        let accent_rows: Vec<usize> = (0..HISTORY_H)
            .filter(|&y| gutter[y].1 == a.theme.accent)
            .collect();
        assert_eq!(
            accent_rows.len(),
            thumb_len,
            "thumb length is the viewport fraction"
        );
        assert_eq!(
            accent_rows.first(),
            Some(&expect_top),
            "thumb top maps to the viewport"
        );
    }

    #[tokio::test]
    async fn stream_end_while_following_bottom_stays_at_bottom() {
        let mut a = streaming_app(
            "some reasoning text here\nmore of it",
            "A short final answer.",
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0, "follows the bottom while streaming");

        a.on_chat_event(1, nexus_core::provider::StreamEvent::Done)
            .unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(
            a.scroll, 0,
            "must keep following the bottom after the stream ends"
        );
        let buf = terminal.backend().buffer();
        let col = crate::ui::reading_column(buf.area, "auto");
        let bottom_row: String = (col.x..col.right() - 1)
            .map(|x| buf[(x, HISTORY_H as u16 - 1)].symbol().to_string())
            .collect();
        assert!(
            bottom_row.trim().is_empty(),
            "the message's trailing blank row must sit at the pane bottom, got: {bottom_row:?}"
        );
    }

    // ---- remote images in replies ----

    const PIC: &str = "https://img.example.invalid/pic.png";

    fn tiny_png(size: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(size, size, |x, y| {
            image::Rgb([x as u8 * 6, y as u8 * 6, 80])
        });
        let mut out = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn screen(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// 40 question/answer pairs; the answer at pair 10 carries a remote image.
    fn remote_image_app() -> AppView {
        let mut a = test_app();
        for i in 0..40 {
            a.messages.push(msg(
                "user",
                &format!("question number {i} with some body text"),
            ));
            let body = if i == 10 {
                format!(
                    "answer number {i} with a decent amount of text\n\n![the diagram]({PIC})\n\nmore words after the picture"
                )
            } else {
                format!("answer number {i} with a decent amount of body text")
            };
            let mut m = msg("assistant", &body);
            m.model = Some("a/one".into());
            m.phrase = Some("working".into());
            a.messages.push(m);
        }
        a
    }

    fn history_text(a: &AppView) -> String {
        a.history_cache
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn remote_image_placeholder_swaps_for_pixels_without_moving_the_viewport() {
        let mut a = remote_image_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let text = history_text(&a);
        assert!(
            text.contains("\u{25a3} the diagram \u{b7} loading"),
            "{text}"
        );
        let before_lines = a.history_cache.lines.len();

        a.scroll = 25;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let before = visible_snapshot(&terminal);
        let before_screen = screen(&terminal);
        assert!(before.contains("answer number"), "{before}");

        a.remote_images.seed(PIC, &tiny_png(40)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert!(
            a.history_cache.lines.len() > before_lines + 5,
            "the image should add rows once it is known"
        );
        assert!(!history_text(&a).contains("loading"));
        assert_eq!(
            screen(&terminal),
            before_screen,
            "late image rows must not move the viewport"
        );

        a.scroll = 0;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let bottom = screen(&terminal);
        a.remote_images.seed_failure(PIC);
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        assert_eq!(a.scroll, 0);
        assert_eq!(screen(&terminal), bottom);
    }

    #[test]
    fn remote_image_shows_failure_and_disabled_text() {
        let mut a = remote_image_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        a.remote_images.seed_failure(PIC);
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let text = history_text(&a);
        assert!(
            text.contains("\u{25a3} the diagram \u{b7} img.example.invalid (couldn't load)"),
            "{text}"
        );

        a.core.settings.remote_images = false;
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let text = history_text(&a);
        assert!(
            text.contains("\u{25a3} the diagram \u{b7} img.example.invalid"),
            "{text}"
        );
        assert!(
            !text.contains("couldn't load") && !text.contains("loading"),
            "{text}"
        );
        assert!(!a.remote_images.enabled());
    }

    #[test]
    fn clicking_a_remote_image_row_targets_its_url() {
        let mut a = remote_image_app();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        a.remote_images.seed(PIC, &tiny_png(40)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut a)).unwrap();
        let opened: Vec<_> = a.history_cache.image_at_line.iter().flatten().collect();
        assert!(opened.len() > 3);
        assert!(opened.iter().all(|target| target.as_str() == PIC));
    }
}

#[cfg(test)]
mod card_background_tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::{Db, Message};
    use nexus_core::space::Space;

    #[test]
    fn user_card_uses_configured_bg_and_stays_right_aligned() {
        let db = Db::open_in_memory().unwrap();
        let space = Space {
            root: std::env::temp_dir().join(format!("nexus-tint-{}", uuid::Uuid::new_v4())),
        };
        let mut app = AppView::new(App::new(db, Some("k"), space));
        app.messages.push(Message {
            role: "user".into(),
            content: "a short note".into(),
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: Some("2026-08-08T10:00:00Z".into()),
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 10)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut body_start = None;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "a" {
                    body_start = Some((x, cell.bg));
                    break;
                }
            }
            if body_start.is_some() {
                break;
            }
        }
        let (body_start, body_bg) = body_start.expect("user message should be rendered");
        assert_eq!(
            body_bg, app.theme.raised,
            "card should use the raised shade (the terminal's own bg when transparent)"
        );
        assert!(
            body_start >= 30,
            "card should be right-aligned, got body start at {body_start}"
        );
    }
}
