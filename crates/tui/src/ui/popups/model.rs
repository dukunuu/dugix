use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, ListState, Paragraph};

use crate::app_view::AppView;
use crate::ui::popups::chrome::{self, PopupFrame, ctrl, hint, keys};
use crate::ui::style::glyph;
use nexus_core::app::{ModelPanel, Popup};
use nexus_core::provider::Model;

#[allow(clippy::too_many_lines)] // shared frame + two model columns
pub fn render(f: &mut Frame, app: &mut AppView) {
    let area = chrome::popup_area(app, f.area(), "model", MODEL_SIZE);
    let (fav_outer, avail_outer) = model_popup_areas(app, f.area());
    let picking = match app.model_pick_target {
        nexus_core::app::ModelPickTarget::Session => "chat model",
        nexus_core::app::ModelPickTarget::SwarmPersona(_) => "drafter model",
    };
    let mut count = format!("{} · {picking}", app.model_backend_filter_label());
    if let Some(efforts) = app.focused_reasoning_hint() {
        count = format!("{count} · {efforts}");
    }
    let hints = [
        hint(keys::MOVE, ""),
        hint(keys::ENTER, "pick"),
        hint(keys::TAB, "pane"),
        hint(ctrl('s'), "star"),
        hint(ctrl('t'), "effort"),
        hint(ctrl('p'), "backend"),
        hint(
            keys::ESC,
            if app.model_filter.is_empty() {
                "close"
            } else {
                "clear"
            },
        ),
    ];
    let inner = chrome::render_popup(
        f,
        area,
        &PopupFrame {
            title: "Models",
            count: &count,
            hints: &hints,
            tone: chrome::Tone::Normal,
            focused: true,
        },
        app,
    );
    let theme = app.theme;
    chrome::render_field(
        f,
        inner,
        "⌕",
        &app.model_filter,
        "Type to filter",
        false,
        &theme,
    );
    let avail_total = app.available_models().len();
    let unfiltered_total = app
        .models
        .iter()
        .filter(|model| {
            app.model_backend_filter
                .is_none_or(|backend| model.backend == backend)
                && !app
                    .favorites
                    .contains(&nexus_core::app::composite_id(model))
        })
        .count();
    for (area, panel, label) in [
        (fav_outer, ModelPanel::Favorites, "Favorites".to_string()),
        (
            avail_outer,
            ModelPanel::Available,
            format!("Available · {avail_total} of {unfiltered_total}"),
        ),
    ] {
        let style = if app.model_focus == panel {
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg_dim)
        };
        f.render_widget(
            Paragraph::new(format!("{} {label}", glyph::SECTION)).style(style),
            Rect {
                height: area.height.min(1),
                ..area
            },
        );
    }
    f.render_widget(
        Paragraph::new(vec![Line::from("│"); usize::from(fav_outer.height)])
            .style(Style::default().fg(theme.border_dim)),
        Rect::new(fav_outer.right(), fav_outer.y, 1, fav_outer.height),
    );
    let fav_inner = list_inner(fav_outer);
    let fav_items = model_items(app, &app.favorite_models(), row_width(fav_inner), true);
    let fav_total = app.favorite_models().len();
    let mut fav_state = ListState::default();
    if fav_total > 0 {
        fav_state.select(Some(app.fav_selected));
    }
    chrome::render_list(
        f,
        chrome::standard_list(fav_items, &theme),
        &mut fav_state,
        fav_inner,
        fav_total,
        1,
        app,
    );
    // Stash the scroll offset, which is render state, for click mapping.
    app.fav_offset = fav_state.offset();
    if fav_total == 0 {
        chrome::render_empty(f, fav_inner, "No favorites yet. ^S stars a model.", &theme);
    }
    let avail_inner = list_inner(avail_outer);
    let avail_items = model_items(app, &app.available_models(), row_width(avail_inner), false);
    let mut avail_state = ListState::default();
    if avail_total > 0 {
        avail_state.select(Some(app.avail_selected));
    }
    chrome::render_list(
        f,
        chrome::standard_list(avail_items, &theme),
        &mut avail_state,
        avail_inner,
        avail_total,
        1,
        app,
    );
    app.avail_offset = avail_state.offset();
    if avail_total == 0 {
        chrome::render_empty(
            f,
            avail_inner,
            &format!(
                "No model matches “{}”. Esc clears the filter, ^P switches backend.",
                &*app.model_filter
            ),
            &theme,
        );
    }
}

/// Row text width inside a panel: the inner rect minus the scrollbar gutter, the
/// `▸ ` highlight symbol, and a column of air.
fn row_width(inner: Rect) -> usize {
    inner.width.saturating_sub(4) as usize
}

/// Trailing columns every model row reserves: the vision glyph and the context
/// window (`humanize` yields up to six chars, e.g. `131.1k`).
const VISION_W: usize = 2;
const CTX_W: usize = 7;

/// `short` shows the compact `name · backend` label instead of the full
/// searchable id.
fn model_items(
    app: &AppView,
    models: &[&Model],
    width: usize,
    short: bool,
) -> Vec<ListItem<'static>> {
    models
        .iter()
        .map(|m| {
            let key = nexus_core::app::composite_id(m);
            let mut id = if short {
                crate::ui::short_model_label(&key)
            } else {
                key.clone()
            };
            let marker = if app.favorites.contains(&key) {
                format!("{} ", glyph::FAVORITE)
            } else if app.last_used.contains_key(&key) {
                format!("{} ", glyph::DOT)
            } else {
                "  ".to_string()
            };
            // [r:high] if set, [r] if supported but off.
            let badge = match app.reasoning_of(&key) {
                Some("none") => "  [r:off]".to_string(),
                Some(effort) => format!("  [r:{effort}]"),
                None if !m.reasoning_efforts.is_empty() => "  [r]".to_string(),
                None => String::new(),
            };
            // Every row reserves the same trailing columns, so they line up
            // right-aligned.
            let name_w = width.saturating_sub(VISION_W + CTX_W);
            id = chrome::truncate(&id, name_w.saturating_sub(2 + badge.chars().count()));
            let name = format!("{marker}{id}{badge}");
            let pad = name_w.saturating_sub(name.chars().count());
            let dim = Style::default().fg(app.theme.fg_dim);
            let vision = if m.supports_images {
                format!(" {}", glyph::VISION)
            } else {
                "  ".to_string()
            };
            let ctx = m.context_length.map_or_else(
                || " ".repeat(CTX_W),
                |c| format!(" {:>6}", crate::ui::humanize(c)),
            );
            let spans = vec![
                Span::raw(format!("{name}{}", " ".repeat(pad))),
                Span::styled(vision, dim),
                Span::styled(ctx, dim),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect()
}

/// The model picker's default size, in percent of the screen.
const MODEL_SIZE: (u16, u16) = chrome::WIDE;

/// Column rects below the filter, shared by the renderer and the mouse
/// hit-tester so they always agree.
pub fn model_popup_areas(app: &AppView, screen: Rect) -> (Rect, Rect) {
    let outer = chrome::popup_rect(app, screen, "model", MODEL_SIZE);
    let mut inner = Block::default()
        .borders(Borders::ALL)
        .padding(ratatui::widgets::Padding::horizontal(chrome::PAD))
        .inner(outer);
    if inner.height >= 4 {
        inner.y += 2;
        inner.height -= 2;
    }
    let cols = Layout::horizontal([Constraint::Ratio(2, 5), Constraint::Ratio(3, 5)])
        .spacing(1)
        .split(inner);
    (cols[0], cols[1])
}

/// The clickable list area below a column's heading.
pub fn list_inner(outer: Rect) -> Rect {
    Rect {
        y: outer.y + outer.height.min(1),
        height: outer.height.saturating_sub(1),
        ..outer
    }
}

pub fn handle_key(app: &mut AppView, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('p') if ctrl => app.cycle_model_backend_filter(),
        KeyCode::Char('s') if ctrl => app.toggle_favorite_focused()?,
        KeyCode::Char('t') if ctrl => app.cycle_reasoning_focused()?,
        KeyCode::Esc => {
            if app.model_filter.is_empty() {
                app.popup = match app.model_pick_target {
                    nexus_core::app::ModelPickTarget::SwarmPersona(_) => Popup::Swarm,
                    nexus_core::app::ModelPickTarget::Session => Popup::None,
                };
            } else {
                app.model_filter.clear();
                app.reset_model_selection();
            }
        }
        KeyCode::Enter => app.confirm_model()?,
        KeyCode::Tab => app.toggle_model_focus(),
        KeyCode::Up => app.move_model_selection(-1),
        KeyCode::Down => app.move_model_selection(1),
        KeyCode::PageUp => app.move_model_selection(-10),
        KeyCode::PageDown => app.move_model_selection(10),
        KeyCode::Home => app.move_model_selection(i32::MIN / 2),
        KeyCode::End => app.move_model_selection(i32::MAX / 2),
        KeyCode::Backspace => {
            app.model_filter.backspace();
            app.reset_model_selection();
        }
        KeyCode::Char(c) if !super::is_chord(key) => {
            app.model_filter.insert_char(c);
            app.reset_model_selection();
        }
        _ if app.model_filter.key(key, &mut app.clipboard) => {}
        _ => {}
    }
    Ok(())
}
