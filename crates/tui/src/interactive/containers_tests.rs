use super::*;

fn session(source: &str) -> Session {
    Session::new(Document::parse(source).unwrap()).unwrap()
}

const ROW: &str = r#"root = Panel("Row", [Row([Text("left"), Text("right")])])"#;

#[test]
fn rows_sit_side_by_side_until_columns_get_too_narrow() {
    let session = session(ROW);
    let row = &session.document().nodes()[0];
    let wide = components::container_regions(row, &session, Rect::new(2, 2, 76, 10));
    assert_eq!(wide, [Rect::new(2, 2, 37, 1), Rect::new(41, 2, 37, 1)]);
    let narrow = components::container_regions(row, &session, Rect::new(2, 2, 40, 10));
    assert_eq!(narrow, [Rect::new(2, 2, 40, 1), Rect::new(2, 3, 40, 1)]);
}

#[test]
fn grids_fill_rows_and_drop_columns_on_narrow_terminals() {
    let session = session(r#"root = Panel("Grid", [Grid([Text("a"), Text("b"), Text("c")], 3)])"#);
    let grid = &session.document().nodes()[0];
    let wide = components::container_regions(grid, &session, Rect::new(0, 0, 76, 10));
    assert_eq!(wide.iter().map(|r| r.y).collect::<Vec<_>>(), [0, 0, 0]);
    let narrow = components::container_regions(grid, &session, Rect::new(0, 0, 40, 10));
    assert_eq!(
        narrow,
        [
            Rect::new(0, 0, 19, 1),
            Rect::new(21, 0, 19, 1),
            Rect::new(0, 2, 19, 1)
        ]
    );
}

#[test]
fn container_panels_reserve_exactly_what_they_draw_at_every_width() {
    let dashboard = gallery_tests::PANELS
        .iter()
        .find(|source| source.contains("Ops dashboard"))
        .unwrap();
    for width in [30u16, 40, 60, 72, 100] {
        let entry = Entry::new(Ok(session(dashboard)));
        let (canvas, hits) = entry.canvas(width, &Theme::default(), false, true);
        assert_eq!(
            source_height(dashboard, width),
            canvas.area.height,
            "@{width}"
        );
        assert!(
            hits.iter()
                .all(|(rect, _)| rect.intersection(canvas.area) == *rect),
            "@{width}"
        );
    }
}

#[test]
fn controls_inside_rows_focus_in_reading_order_and_take_clicks() {
    let source = r#"root = Panel("Controls", [Row([Slider($load, "Load", 0, 10, 1), Buttons([Button("Reset", @Set($load, 0))])])])
$load = 4"#;
    let mut widgets = Widgets::default();
    let placement = tests::placement(source, 1);
    let _ = tests::draw(&mut widgets, &[placement], 0);
    let entry = widgets.entries.values().next().unwrap();
    assert!(matches!(
        entry.controls()[..],
        [Control::Slider(_), Control::Action(_)]
    ));
    let reset = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(1))
        .unwrap()
        .rect;
    assert!(reset.x > 30, "{reset:?}");
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: reset.x + 1,
        row: reset.y,
        modifiers: KeyModifiers::NONE,
    };
    let (handled, _) = widgets.click(click, Rect::new(0, 0, 80, 32), true);
    assert!(handled);
    let session = widgets
        .entries
        .values()
        .next()
        .unwrap()
        .session
        .as_ref()
        .unwrap();
    assert_eq!(session.value("load"), "0");
}
