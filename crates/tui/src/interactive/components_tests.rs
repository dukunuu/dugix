use super::*;
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

const RPS: &str = include_str!("../../../core/tests/fixtures/interactive/rps.json");

fn source(view: Value, state: Value) -> String {
    json!({"version":1,"title":"Panel","state":state,"view":view,"actions":{}}).to_string()
}
fn entry(view: Value, state: Value) -> Entry {
    Entry::new(Ok(Session::new(
        Document::parse(&source(view, state)).unwrap(),
    )
    .unwrap()))
}
fn row_text(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect::<String>()
        .trim_end()
        .into()
}
fn snapshot(view: Value, state: Value, expected: &[String]) -> Buffer {
    let entry = entry(view, state);
    let (canvas, _) = entry.canvas(36, &Theme::default(), true, true);
    let mut terminal = Terminal::new(TestBackend::new(36, canvas.area.height)).unwrap();
    terminal
        .draw(|f| f.buffer_mut().clone_from(&canvas))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 0).starts_with("  Panel"));
    // Component expectations describe the content, excluding the new card
    // padding and blank section separators. Full-card snapshots live below.
    let regions = node_regions(entry.session.as_ref().unwrap(), 36);
    let mut content = Buffer::empty(Rect::new(
        0,
        0,
        32,
        2 + regions.iter().map(|r| r.height).sum::<u16>(),
    ));
    let mut y = 2;
    for region in regions {
        for row in region.y..region.bottom() {
            for x in 0..32 {
                content[(x, y)] = buffer[(x + 2, row)].clone();
            }
            y += 1;
        }
    }
    let rows: Vec<_> = (2..content.area.height)
        .map(|y| row_text(&content, y))
        .collect();
    assert_eq!(rows, expected);
    content
}

#[test]
fn kv_snapshot_aligns_dim_labels_and_normal_values() {
    let buffer = snapshot(
        json!([{"type":"kv","items":[["Name","{{name}}"],["Job","Ready"]]}]),
        json!({"name":"Ada"}),
        &["Name Ada".into(), "Job  Ready".into()],
    );
    assert_eq!(buffer[(0, 2)].fg, Theme::default().fg_dim);
    assert_eq!(buffer[(5, 2)].fg, Theme::default().fg);
}

#[test]
fn table_snapshot_shows_header_selection_and_truncated_cells() {
    let buffer = snapshot(
        json!([{"type":"table","columns":["Name","Status"],"rows_data":[["Ada","Ready"],["Longer than the column, and more","Waiting"]],"select":"row"}]),
        json!({"row":0}),
        // Status keeps its natural width; Name takes the rest and clips.
        &[
            format!(" {:<24}Status", "Name"),
            format!("▸{:<24}Ready", "Ada"),
            format!(" {:<24}Waiting", "Longer than the column…"),
        ],
    );
    assert_eq!(buffer[(1, 2)].fg, Theme::default().fg_dim);
    assert_eq!(buffer[(1, 3)].fg, Theme::default().accent);
}

#[test]
fn progress_snapshot_clamps_and_uses_accent_bar() {
    // Label column, eighth-cell bar on a dim track, value at the right.
    let buffer = snapshot(
        json!([{"type":"progress","label":"Done","value":5,"max":10}]),
        json!({}),
        &[format!(
            "{:<6}{}{}  5 / 10",
            "Done",
            "━".repeat(9),
            "─".repeat(9)
        )],
    );
    assert_eq!(buffer[(6, 2)].fg, Theme::default().accent2);
    assert_eq!(buffer[(15, 2)].fg, Theme::default().border_dim);
    snapshot(
        json!([{"type":"progress","label":"Done","value":99,"max":10}]),
        json!({}),
        &[format!("{:<6}{}  10 / 10", "Done", "━".repeat(17))],
    );
}

#[test]
fn bars_snapshot_scales_to_largest_value() {
    snapshot(
        json!([{"type":"bars","items":[{"label":"A","value":2},{"label":"B","value":4},{"label":"C","value":-1}]}]),
        json!({}),
        &[
            // Solid data bars: no track behind them, zero draws nothing.
            format!("{:<6}{}{}  2", "A", "▆".repeat(12), " ".repeat(11)),
            format!("{:<6}{}  4", "B", "▆".repeat(23)),
            format!("{:<6}{}  0", "C", " ".repeat(23)),
        ],
    );
}

#[test]
fn bars_keep_a_shared_scale_when_value_widths_differ() {
    snapshot(
        json!([{"type":"bars","items":[{"label":"A","value":9},{"label":"B","value":10}]}]),
        json!({}),
        &[
            format!("{:<6}{}     9", "A", "▆".repeat(20)),
            format!("{:<6}{}  10", "B", "▆".repeat(22)),
        ],
    );
}

#[test]
fn sparkline_snapshot_uses_declared_height_and_theme_glyphs() {
    for values in [json!([0, 1, 2, 3]), json!([0, 1e-9, 2e-9, 3e-9])] {
        snapshot(
            json!([{"type":"sparkline","values":values,"rows":2}]),
            json!({}),
            &["  ▂█".into(), " ▅██".into()],
        );
    }
}

#[test]
fn checklist_snapshot_shows_independent_boolean_values() {
    let buffer = snapshot(
        json!([{"type":"checklist","items":[{"label":"{{name}}","key":"ready"},{"label":"Done","key":"done"}]}]),
        json!({"name":"Ready","ready":false,"done":true}),
        &["    Ready".into(), " ✓  Done".into()],
    );
    // An empty box sits on the control surface; a checked one is
    // success-filled with a bright label. The first item has focus.
    assert_eq!(buffer[(1, 2)].bg, Theme::default().control);
    assert_eq!(buffer[(4, 2)].fg, Theme::default().accent);
    assert_eq!(buffer[(1, 3)].bg, Theme::default().success);
    assert_eq!(buffer[(4, 3)].fg, Theme::default().fg);
}

#[test]
fn select_snapshot_displays_label_and_current_option() {
    snapshot(
        json!([{"type":"select","key":"mode","label":"Mode","options":["Fast","Full"]}]),
        json!({"mode":"Fast"}),
        &["Mode    Fast ▕ Full".into()],
    );
}

#[test]
fn toggle_snapshot_displays_label_and_boolean() {
    snapshot(
        json!([{"type":"toggle","key":"ready","label":"{{name}}"}]),
        json!({"ready":true,"name":"Ready"}),
        &["  ●  Ready".into()],
    );
}

#[test]
fn mixed_document_snapshot_keeps_all_components_in_fixed_slots() {
    snapshot(
        json!([
            {"type":"kv","items":[["Name","{{name}}"]]},
            {"type":"table","columns":["Name"],"rows_data":[["Ada"]]},
            {"type":"progress","label":"Done","value":1},
            {"type":"bars","items":[{"label":"A","value":1}]},
            {"type":"sparkline","values":[0,1]},
            {"type":"checklist","items":[{"label":"Ready","key":"ready"}]},
            {"type":"select","key":"mode","label":"Mode","options":["Fast","Full"]},
            {"type":"toggle","key":"ready","label":"Ready"}
        ]),
        json!({"name":"Ada","ready":false,"mode":"Fast"}),
        &[
            "Name Ada".into(),
            "Name".into(),
            "Ada".into(),
            format!("{:<6}{}  100%", "Done", "━".repeat(20)),
            format!("{:<6}{}  1", "A", "▆".repeat(23)),
            " █".into(),
            "    Ready".into(),
            "Mode    Fast ▕ Full".into(),
            " ●   Ready".into(),
        ],
    );
}

fn panel() -> (Widgets, Placement) {
    let source = source(
        json!([
            {"type":"buttons","items":[{"label":"Action","action":"noop"}]},
            {"type":"input","key":"name","label":"Name"},
            {"type":"checklist","items":[{"label":"Ready","key":"ready"},{"label":"Done","key":"done"}]},
            {"type":"table","columns":["Name"],"rows_data":[["A"],["B"],["C"]],"select":"row"},
            {"type":"select","key":"mode","label":"Mode","options":["Fast","Full"]},
            {"type":"toggle","key":"ready","label":"Ready"}
        ]),
        json!({"name":"Ada","ready":false,"done":false,"mode":"Fast","row":0}),
    );
    let mut data: Value = serde_json::from_str(&source).unwrap();
    data["actions"]["noop"] = json!([]);
    let source = data.to_string();
    let p = Placement {
        row: 0,
        width: 60,
        owner: 0,
        ordinal: 0,
        height: source_height(&source, 60),
        source,
        complete: true,
    };
    (Widgets::default(), p)
}
fn draw(widgets: &mut Widgets, p: &Placement, top: usize) {
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    terminal
        .draw(|f| {
            widgets.begin_frame(true);
            widgets.paint(
                f,
                f.area(),
                top,
                std::slice::from_ref(p),
                &Theme::default(),
                true,
            );
        })
        .unwrap();
}
fn click_control(widgets: &mut Widgets, control: usize, row: u16) {
    let hit = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(control))
        .unwrap();
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x,
        row: hit.rect.y + row,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        widgets.click(mouse, Rect::new(0, 0, 80, 32), true),
        (true, None)
    );
}
fn value(widgets: &Widgets, p: &Placement, key: &str) -> String {
    widgets.entries[&p.key()]
        .session
        .as_ref()
        .unwrap()
        .value(key)
}

#[test]
fn focus_order_and_keys_include_new_controls_without_stealing_input() {
    let (mut widgets, p) = panel();
    draw(&mut widgets, &p, 0);
    assert!(widgets.focus_latest());
    let controls = widgets.entries[&p.key()].controls();
    assert!(matches!(
        &controls[..],
        [
            Control::Action(_),
            Control::Input(_),
            Control::Toggle(_),
            Control::Toggle(_),
            Control::Table(..),
            Control::Select(_),
            Control::Toggle(_)
        ]
    ));
    for expected in 1..7 {
        widgets.key(KeyCode::Tab.into(), true);
        assert_eq!(widgets.entries[&p.key()].focus, expected);
        match expected {
            1 => {
                widgets.key(KeyCode::Char('!').into(), true);
            }
            2 => {
                widgets.key(KeyCode::Char(' ').into(), true);
            }
            3 => {
                widgets.key(KeyCode::Enter.into(), true);
            }
            4 => {
                for _ in 0..5 {
                    widgets.key(KeyCode::Down.into(), true);
                }
                assert_eq!(value(&widgets, &p, "row"), "2");
                for _ in 0..5 {
                    widgets.key(KeyCode::Up.into(), true);
                }
                assert_eq!(value(&widgets, &p, "row"), "0");
            }
            5 => {
                widgets.key(KeyCode::Left.into(), true);
                assert_eq!(value(&widgets, &p, "mode"), "Full");
                widgets.key(KeyCode::Right.into(), true);
                assert_eq!(value(&widgets, &p, "mode"), "Fast");
            }
            6 => {
                widgets.key(KeyCode::Enter.into(), true);
            }
            _ => unreachable!(),
        }
    }
    assert_eq!(value(&widgets, &p, "name"), "Ada!");
    assert_eq!(value(&widgets, &p, "ready"), "false");
    assert_eq!(value(&widgets, &p, "done"), "true");
    widgets.key(KeyCode::Tab.into(), true);
    assert_eq!(widgets.entries[&p.key()].focus, 7); // Expand is keyboard reachable.
    widgets.key(KeyCode::Tab.into(), true);
    assert_eq!(widgets.entries[&p.key()].focus, 0);
}

#[test]
fn mouse_toggles_checklist_and_toggle_cycles_select_and_selects_table_rows() {
    let (mut widgets, p) = panel();
    draw(&mut widgets, &p, 0);
    click_control(&mut widgets, 3, 0);
    assert_eq!(value(&widgets, &p, "done"), "true");
    click_control(&mut widgets, 5, 0);
    assert_eq!(value(&widgets, &p, "mode"), "Full");
    click_control(&mut widgets, 5, 0);
    assert_eq!(value(&widgets, &p, "mode"), "Fast");
    click_control(&mut widgets, 6, 0);
    assert_eq!(value(&widgets, &p, "ready"), "true");
    click_control(&mut widgets, 4, 3);
    assert_eq!(value(&widgets, &p, "row"), "2");
    // Clip the header and first data row without depending on earlier fields' height.
    let table_top = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(4))
        .unwrap()
        .rect
        .y;
    draw(&mut widgets, &p, usize::from(table_top + 2));
    click_control(&mut widgets, 4, 0);
    assert_eq!(value(&widgets, &p, "row"), "1");
}

#[test]
fn labels_are_sanitized_and_narrow_component_views_are_safe() {
    assert_eq!(safe("A\u{1b}\t\rB\u{7f}\u{fe0f}"), "A\\u{1b}\\t\\rB\\u{7f}");
    snapshot(
        json!([{"type":"toggle","key":"ready","label":"Ready\u{fe0f}"}]),
        json!({"ready":true}),
        &["  ●  Ready".into()],
    );
    assert_eq!(clipped("宽宽宽", 5), "宽宽…");
    let entry = entry(
        json!([
            {"type":"kv","items":[["Long label","Value"]]},
            {"type":"table","columns":["A","B"],"rows_data":[["Wide","Cell"]],"select":"row"},
            {"type":"progress","label":"Progress","value":1},
            {"type":"bars","items":[{"label":"A","value":1}]},
            {"type":"sparkline","values":[0,0,0]},
            {"type":"checklist","items":[{"label":"A","key":"ready"}]},
            {"type":"select","key":"mode","label":"Mode","options":["A","B"]},
            {"type":"toggle","key":"ready","label":"A"}
        ]),
        json!({"row":0,"ready":false,"mode":"A"}),
    );
    for width in [0, 1, 2, 4, 8] {
        entry.canvas(width, &Theme::default(), true, true);
    }
}

#[test]
fn game_card_snapshots_70_and_80_columns() {
    let document = Document::parse(RPS).unwrap();
    let entry = Entry::new(Ok(Session::new(document).unwrap()));
    let theme = Theme::default();
    let bodies: [&[&str]; 3] = [
        &[
            "   _______",
            "--'   ____)",
            "     (_____)",
            "     (____)",
            "--.__(___)",
            "beats scissors",
        ],
        &[
            "   _______",
            "--'   ____)____",
            "         ______)",
            "        _______)",
            "--.__________)",
            "beats rock",
        ],
        &[
            "   _______",
            "--'   ____)____",
            "      _________)",
            "     (____)",
            "--.__(___)",
            "beats paper",
        ],
    ];
    for (width, inner_width) in [(70, 18), (80, 22)] {
        let tile_top = vec![format!("┌{}┐", "─".repeat(inner_width)); 3].join("  ");
        let mut expected = vec![
            top_border("Rock · Paper · Scissors", width),
            card_row("", width),
            card_row(" You: Pick a move · Computer: ?", width),
            card_row(" Ready", width),
            card_row(" Wins 0 · Losses 0 · Draws 0", width),
            card_row("", width),
            card_row(&format!(" {tile_top}"), width),
        ];
        for row in 0..5 {
            let tiles = bodies
                .iter()
                .map(|body| {
                    let padding = inner_width - body[row].width();
                    format!(
                        "│{}{}{}│",
                        " ".repeat(padding.div_ceil(2)),
                        body[row],
                        " ".repeat(padding / 2)
                    )
                })
                .collect::<Vec<_>>()
                .join("  ");
            expected.push(card_row(&format!(" {tiles}"), width));
        }
        let text_width = inner_width - 2;
        let labels = ["Rock", "Paper", "Scissors"]
            .map(|label| format!("│ {label:<text_width$} │"))
            .join("  ");
        expected.push(card_row(&format!(" {labels}"), width));
        let metadata = bodies
            .map(|body| format!("│ {:<text_width$} │", body[5]))
            .join("  ");
        expected.push(card_row(&format!(" {metadata}"), width));
        let tile_bottom = vec![format!("└{}┘", "─".repeat(inner_width)); 3].join("  ");
        expected.push(card_row(&format!(" {tile_bottom}"), width));
        expected.extend([
            card_row("", width),
            card_row("   Reset score", width),
            card_row("", width),
            bottom_border(width),
        ]);
        assert_card_snapshot(&entry, width, &expected);
        let (actual, _) = entry.canvas(width, &theme, false, true);
        assert_eq!(actual[(4, 12)].fg, theme.fg);
        assert!(actual[(4, 12)].modifier.contains(Modifier::BOLD));
        assert_eq!(actual[(4, 13)].fg, theme.fg_dim);
    }
}

#[test]
fn counter_card_snapshots_70_and_80_columns() {
    let document = Document::parse(super::tests::COUNTER).unwrap();
    let entry = Entry::new(Ok(Session::new(document).unwrap()));
    let theme = Theme::default();
    for width in [70, 80] {
        let expected = [
            top_border("Counter", width),
            card_row("", width),
            card_row(" Count: 0", width),
            card_row("", width),
            card_row("   Add one     Reset", width),
            card_row("", width),
            card_row(" Name", width),
            card_row("", width),
            card_row(&format!(" {}", "▔".repeat(usize::from(width - 4))), width),
            card_row("", width),
            card_row("   Submit result ↵", width),
            card_row("", width),
            bottom_border(width),
        ];
        assert_card_snapshot(&entry, width, &expected);
        let (actual, _) = entry.canvas(width, &theme, false, true);
        // The primary pill fills with the panel accent.
        assert_eq!(actual[(3, 10)].bg, theme.accent2);
        assert_eq!(actual[(3, 10)].fg, theme.on(theme.accent2));
        assert!(actual[(4, 4)].modifier.contains(Modifier::UNDERLINED));
        assert_eq!(actual[(2, 2)].fg, theme.fg);
    }
}

/// A content row: content starts two columns in, past the focus gutter.
/// Rows are compared with trailing spaces trimmed.
fn card_row(content: &str, _width: u16) -> String {
    format!(" {content}").trim_end().to_string()
}
/// Quiet sentence-case title, with a discreet expand affordance.
fn top_border(title: &str, width: u16) -> String {
    format!("  {title:<inner$}⤢", inner = usize::from(width) - 5)
}
/// The bottom rule with the hint right-aligned two columns from the end.
fn bottom_border(width: u16) -> String {
    format!(
        "{} F2 keyboard ──",
        "─".repeat(usize::from(width) - " F2 keyboard ──".width())
    )
}
fn assert_card_snapshot(entry: &Entry, width: u16, expected: &[String]) {
    let (actual, _) = entry.canvas(width, &Theme::default(), false, true);
    let mut terminal = Terminal::new(TestBackend::new(width, actual.area.height)).unwrap();
    terminal
        .draw(|f| f.buffer_mut().clone_from(&actual))
        .unwrap();
    let rows: Vec<_> = (0..actual.area.height)
        .map(|y| row_text(terminal.backend().buffer(), y))
        .collect();
    assert_eq!(rows.join("\n"), expected.join("\n"));
}

#[test]
fn rps_tiles_share_a_row_and_pad_their_labels_and_metadata() {
    let entry = Entry::new(Ok(Session::new(Document::parse(RPS).unwrap()).unwrap()));
    for (width, tile_width, positions) in [(70, 20, [2, 24, 46]), (80, 24, [2, 28, 54])] {
        let (canvas, hits) = entry.canvas(width, &Theme::default(), false, true);
        let tiles: Vec<_> = hits.iter().take(3).map(|(rect, _)| *rect).collect();
        assert_eq!(tiles, positions.map(|x| Rect::new(x, 6, tile_width, 9)));
        for tile in tiles {
            for y in [12, 13] {
                assert_eq!(canvas[(tile.x + 1, y)].symbol(), " ");
                assert_eq!(canvas[(tile.right() - 2, y)].symbol(), " ");
            }
        }
        assert!(row_text(&canvas, 12).contains("│ Rock"));
        assert!(row_text(&canvas, 13).contains("│ beats scissors"));
    }
}

#[test]
fn short_last_tile_row_stays_left_aligned_without_stretching() {
    let mut data: Value = serde_json::from_str(RPS).unwrap();
    let mut fourth = data["view"][3]["items"][0].clone();
    fourth["label"] = json!("Fourth");
    data["view"][3]["items"]
        .as_array_mut()
        .unwrap()
        .push(fourth);
    let entry = Entry::new(Ok(Session::new(
        Document::parse(&data.to_string()).unwrap(),
    )
    .unwrap()));
    let (canvas, hits) = entry.canvas(80, &Theme::default(), false, true);
    let tiles: Vec<_> = hits.iter().take(4).map(|(rect, _)| *rect).collect();
    assert_eq!(
        tiles,
        [
            Rect::new(2, 6, 24, 9),
            Rect::new(28, 6, 24, 9),
            Rect::new(54, 6, 24, 9),
            Rect::new(2, 16, 24, 9),
        ]
    );
    assert_eq!(canvas[(25, 16)].symbol(), "┐");
    assert!((26..78).all(|x| canvas[(x, 16)].symbol() == " "));
    assert!(row_text(&canvas, 22).contains("│ Fourth"));
}

#[test]
fn input_fields_are_underlined_lines_with_visible_focus() {
    let theme = Theme::default();
    for (width, label) in [
        (70, "Name"),
        (80, "Name"),
        (40, "An unusually long input label"),
        (24, "Name"),
    ] {
        let entry = entry(
            json!([{"type":"input","key":"name","label":label}]),
            json!({"name":""}),
        );
        let (quiet, _) = entry.canvas(width, &theme, false, true);
        let (focused, _) = entry.canvas(width, &theme, true, true);
        // Label above, a text row on the panel surface, an underline below
        // that spans the content width.
        assert_eq!(quiet[(2, 2)].fg, theme.fg_dim);
        assert_eq!(focused[(2, 2)].fg, theme.accent);
        for x in 2..width - 2 {
            assert_eq!(quiet[(x, 3)].bg, theme.bg);
            assert_eq!(quiet[(x, 4)].symbol(), "▔");
            assert_eq!(quiet[(x, 4)].fg, theme.border_dim);
            assert_eq!(focused[(x, 4)].fg, theme.accent);
        }
        assert!(focused[(2, 3)].modifier.contains(Modifier::REVERSED));
        assert!(inputs::caret(&quiet, quiet.area).is_none());
    }
}

#[test]
fn primary_resolution_prefers_explicit_then_first_send_otherwise_none() {
    let mut value: Value = serde_json::from_str(super::tests::COUNTER).unwrap();
    let document = Document::parse(&value.to_string()).unwrap();
    assert_eq!(primary_button(&document), Some((3, 0)));
    value["view"][1]["items"][1]["primary"] = json!(true);
    let document = Document::parse(&value.to_string()).unwrap();
    assert_eq!(primary_button(&document), Some((1, 1)));
    value["view"][1]["items"][1]["primary"] = json!(false);
    value["actions"]["submit"] = json!([]);
    assert_eq!(
        primary_button(&Document::parse(&value.to_string()).unwrap()),
        None
    );
}

#[test]
fn input_labels_remain_literal_for_legacy_documents() {
    let entry = entry(
        json!([{"type":"input","key":"name","label":"Name {{literal}}"}]),
        json!({"name":"Ada"}),
    );
    let (buffer, _) = entry.canvas(80, &Theme::default(), false, true);
    assert!(row_text(&buffer, 2).contains("Name {{literal}}"));
    assert!(row_text(&buffer, 3).contains("Ada"));
}

#[test]
fn focus_is_only_visible_on_the_focused_control() {
    let mut entry = entry(
        json!([{"type":"input","key":"name","label":"Name"}]),
        json!({"name":"Ada"}),
    );
    let theme = Theme::default();
    let (quiet, _) = entry.canvas(80, &theme, false, true);
    assert!(inputs::caret(&quiet, quiet.area).is_none());
    assert_eq!(quiet[(2, 2)].fg, theme.fg_dim);
    let (focused, _) = entry.canvas(80, &theme, true, true);
    assert!(row_text(&focused, 3).contains("Ada"));
    assert_eq!(
        inputs::caret(&focused, focused.area),
        Some(Position::new(5, 3))
    );
    assert_eq!(focused[(2, 2)].fg, theme.accent);
    assert!(row_text(&focused, focused.area.height - 1).contains("Tab next"));
    entry.focus = 1;
    let (expanded, _) = entry.canvas(80, &theme, true, true);
    assert_eq!(expanded[(77, 0)].fg, theme.accent);
}

#[test]
fn pills_use_contrasting_surfaces_and_distinct_primary_and_focus_styles() {
    let entry = Entry::new(Ok(Session::new(
        Document::parse(super::tests::COUNTER).unwrap(),
    )
    .unwrap()));
    let mut theme = Theme::default();
    theme.raised = theme.bg;
    let (quiet, _) = entry.canvas(80, &theme, false, true);
    // Secondary actions are quiet underlined text on the panel surface; the
    // primary action is the one filled pill.
    assert_eq!(quiet[(4, 4)].bg, theme.bg);
    assert_eq!(quiet[(4, 4)].fg, theme.fg_dim);
    assert!(quiet[(4, 4)].modifier.contains(Modifier::UNDERLINED));
    assert_eq!(quiet[(3, 10)].bg, theme.accent2);
    assert_eq!(quiet[(3, 10)].fg, theme.on(theme.accent2));
    assert!(quiet[(3, 10)].modifier.contains(Modifier::BOLD));
    assert_eq!(quiet[(2, 10)].symbol(), " ");
    assert_eq!(quiet[(2, 10)].bg, theme.accent2);
    // Focus fills a pill with your accent and marks it, with no underline.
    let (focused, _) = entry.canvas(80, &theme, true, true);
    assert_eq!(focused[(3, 4)].bg, theme.accent);
    assert_eq!(focused[(3, 4)].symbol(), "▸");
    assert!(!focused[(3, 4)].modifier.contains(Modifier::UNDERLINED));
    let mut entry = entry;
    entry.focus = 3;
    let (focused, _) = entry.canvas(80, &theme, true, true);
    assert_eq!(focused[(3, 10)].bg, theme.accent);
    assert_eq!(focused[(3, 10)].symbol(), "▸");
    assert!(focused[(3, 10)].modifier.contains(Modifier::BOLD));
    theme.set_background_mode(crate::theme::BackgroundMode::Transparent);
    let (transparent, _) = entry.canvas(80, &theme, false, true);
    assert_eq!(transparent[(3, 10)].bg, theme.accent2);
    assert_eq!(transparent[(1, 10)].bg, Color::Reset);
}

#[test]
fn related_actions_stay_together_in_source_order_at_every_width() {
    let mut data: Value = serde_json::from_str(super::tests::COUNTER).unwrap();
    data["view"] = json!([{"type":"buttons","items":[
        {"label":"Add one","action":"add"},
        {"label":"Reset","action":"reset"},
        {"label":"Submit result","action":"submit"}
    ]}]);
    let make_entry = |data: &Value| {
        Entry::new(Ok(Session::new(
            Document::parse(&data.to_string()).unwrap(),
        )
        .unwrap()))
    };
    let entry = make_entry(&data);
    let (_, wide) = entry.canvas(80, &Theme::default(), false, true);
    // Buttons keep two padding columns per side and source reading order.
    assert_eq!(wide[0].0, Rect::new(2, 2, 11, 1));
    assert_eq!(wide[1].0, Rect::new(14, 2, 9, 1));
    assert_eq!(wide[2].0, Rect::new(24, 2, 19, 1));
    let (_, narrow) = entry.canvas(30, &Theme::default(), false, true);
    assert_eq!(narrow[2].0, Rect::new(2, 3, 19, 1));
    // Once anything wraps, the send stays in the left-aligned flow.
    data["view"][0]["items"][0]["label"] = json!("A very long local action");
    let (_, wrapped) = make_entry(&data).canvas(40, &Theme::default(), false, true);
    assert_eq!(wrapped[1].0, Rect::new(2, 3, 9, 1));
    assert_eq!(wrapped[2].0, Rect::new(12, 3, 19, 1));
    data["view"][0]["items"][0]["label"] =
        json!("An action long enough to occupy the entire first row");
    let (_, wrapped) = make_entry(&data).canvas(40, &Theme::default(), false, true);
    assert_eq!(wrapped[2].0, Rect::new(12, 3, 19, 1));
    data["view"][0]["items"][0]["label"] = json!("Add one");
    data["view"][0]["items"][1]["action"] = json!("submit");
    let (_, multiple) = make_entry(&data).canvas(80, &Theme::default(), false, true);
    assert!(multiple[2].0.right() < 78);
}

#[test]
fn wrapped_secondary_pill_rows_and_mixed_answer_buttons_stay_left_aligned() {
    let data = json!({
        "version":1,"title":"Options","view":[{"type":"buttons","items":[
            {"label":"First answer","action":"answer"},
            {"label":"Local action","action":"local"},
            {"label":"Last answer","action":"answer"}
        ]}],
        "actions":{"answer":[{"op":"answer"}],"local":[]}
    });
    let entry = Entry::new(Ok(Session::new(
        Document::parse(&data.to_string()).unwrap(),
    )
    .unwrap()));
    let (_, hits) = entry.canvas(30, &Theme::default(), false, true);
    assert_eq!(hits[0], (Rect::new(2, 2, 18, 1), 0));
    assert_eq!(hits[1], (Rect::new(2, 3, 16, 1), 1));
    assert_eq!(hits[2], (Rect::new(2, 4, 17, 1), 2));
}

const FLAGS: &str = r#"root = Panel("Flags", [flags])
flags = Table(["Flag", "Meaning"], [["Z · Zero", "1 when the difference is zero, so the inputs were equal"], ["N · Negative", "1 when the highest bit of the difference is set"]], null, true)"#;

#[test]
fn a_short_label_column_leaves_room_for_the_long_one() {
    let entry = Entry::new(Ok(Session::new(Document::parse(FLAGS).unwrap()).unwrap()));
    let (buffer, _) = entry.canvas(72, &Theme::default(), false, true);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(
        text.contains("1 when the difference is zero, so the inputs were equal"),
        "{text}"
    );
}

#[test]
fn a_header_click_sorts_the_column_drawn_under_the_pointer() {
    let mut session = Session::new(Document::parse(FLAGS).unwrap()).unwrap();
    let area = Rect::new(0, 0, 68, 3);
    let mut buffer = Buffer::empty(area);
    let document = session.document();
    let index = document.node_index(&document.nodes()[0]);
    render_table(
        &document.nodes()[0],
        &session,
        area,
        &Theme::default(),
        false,
        &mut buffer,
    );
    let header: String = (0..area.width).map(|x| buffer[(x, 0)].symbol()).collect();
    let meaning = u16::try_from(header.find("Meaning").unwrap()).unwrap();
    components::click_table(&mut session, index, 0, meaning, area).unwrap();
    assert_eq!(
        session.table_state(index).sort.map(|(column, _)| column),
        Some(1)
    );
}
