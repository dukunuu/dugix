use super::*;

fn entry(source: &str) -> Entry {
    Entry::new(Ok(Session::new(Document::parse(source).unwrap()).unwrap()))
}
fn text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn new_components_reserve_exactly_what_they_draw() {
    for name in ["Team pulse", "Planning"] {
        let source = gallery_tests::PANELS
            .iter()
            .find(|source| source.contains(name))
            .unwrap();
        for width in [36u16, 60, 72, 100] {
            let (canvas, hits) = entry(source).canvas(width, &Theme::default(), false, true);
            assert_eq!(
                source_height(source, width),
                canvas.area.height,
                "{name} @{width}"
            );
            assert!(
                hits.iter()
                    .all(|(rect, _)| rect.intersection(canvas.area) == *rect)
            );
        }
    }
}

#[test]
fn tree_branches_expand_into_their_reserved_rows() {
    let source = r#"root = Panel("Files", [Tree([Branch("src", [Branch("app", [Branch("mod.rs")])], true), Branch("README")])])"#;
    let mut entry = entry(source);
    let before = entry.canvas(40, &Theme::default(), false, true).0;
    assert!(text(&before).contains("▸ app"));
    assert!(!text(&before).contains("mod.rs"));
    // Two expandable branches are visible: src and app.
    assert_eq!(entry.controls().len(), 2);
    entry.activate(1, true, true);
    let after = entry.canvas(40, &Theme::default(), false, true).0;
    assert!(text(&after).contains("▾ app"));
    assert!(text(&after).contains("· mod.rs"));
    assert_eq!(
        before.area.height, after.area.height,
        "the tree reserved its rows"
    );
}

#[test]
fn board_lanes_follow_state_lists() {
    let source = r#"root = Panel("Kanban", [Board([Lane("Todo", $todo), Lane("Done", $done, "success")], 3), Buttons([Button("Finish first", Action([@Set($done, @Append($done, @First($todo))), @Set($todo, @Remove($todo, 0))]))])])
$todo = ["Docs", "Tests"]
$done = []"#;
    let mut entry = entry(source);
    let before = text(&entry.canvas(60, &Theme::default(), false, true).0);
    assert!(before.contains("TODO   2"), "{before}");
    assert!(before.contains("DONE   0"), "{before}");
    entry.activate(0, true, true);
    let after = text(&entry.canvas(60, &Theme::default(), false, true).0);
    assert!(after.contains("TODO   1"), "{after}");
    assert!(after.contains("DONE   1"), "{after}");
    let done_row = after.lines().find(|row| row.contains("Docs")).unwrap();
    assert!(
        done_row.contains("Tests"),
        "Docs moved beside Tests: {done_row}"
    );
}

#[test]
fn diagrams_at_the_content_edge_get_image_placements_others_keep_placeholders() {
    // The second diagram sits in a row's right column, off the edge the
    // diagram painter insets to.
    let source = r#"root = Panel("Flow", [Text("Intro"), Diagram("flowchart LR\nA-->B", "A to B"), Row([Text("side"), Diagram("flowchart LR\nC-->D")])])"#;
    let mut widgets = Widgets::default();
    let placement = tests::placement(source, 1);
    let _ = tests::draw(&mut widgets, std::slice::from_ref(&placement), 0);
    let diagrams = widgets.diagram_placements(std::slice::from_ref(&placement), 80);
    assert_eq!(diagrams.len(), 1, "only the diagram at the content edge");
    assert_eq!(diagrams[0].source, "flowchart LR\nA-->B");
    assert_eq!(diagrams[0].width, 66, "the panel's content width");
    assert_eq!(
        diagrams[0].row,
        placement.row + 4,
        "title bar, padding, intro, gap"
    );
}
