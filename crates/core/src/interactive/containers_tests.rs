use super::*;

fn parse(source: &str) -> Result<Document> {
    Document::parse(source)
}

const DASHBOARD: &str = r#"root = Panel("Ops", [top, grid, controls])
$load = 10
top = Row([Stats([Stat("99.9%", "uptime")], true), Stack([Meter("CPU", 0.7), Meter("Memory", 0.4)], 0)])
grid = Grid([Callout("tip", "Healthy", "API"), Callout("warning", "Slow", "Queue"), Callout("tip", "Healthy", "DB")], 3)
controls = Row([Slider($load, "Load", 0, 100, 1), Buttons([Button("Reset", @Set($load, 0))])], 4)"#;

#[test]
fn containers_hold_any_components_and_walk_in_reading_order() {
    let document = parse(DASHBOARD).unwrap();
    let kinds: Vec<_> = document
        .all_nodes()
        .into_iter()
        .map(|node| match node {
            Node::Row { .. } => "row",
            Node::Grid { .. } => "grid",
            Node::Stack { .. } => "stack",
            Node::Stats { .. } => "stats",
            Node::Meter { .. } => "meter",
            Node::Callout { .. } => "callout",
            Node::Slider { .. } => "slider",
            Node::Buttons { .. } => "buttons",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "row", "stats", "stack", "meter", "meter", "grid", "callout", "callout", "callout",
            "row", "slider", "buttons"
        ]
    );
    // Controls inside containers are real controls with working actions.
    assert_eq!(document.control_count(), 2);
    let mut session = Session::new(document).unwrap();
    assert_eq!(session.value("load"), "10");
    let buttons = session
        .visible_nodes()
        .into_iter()
        .find(|(_, node)| matches!(node, Node::Buttons { .. }))
        .unwrap()
        .0;
    session.activate_button(buttons, 0).unwrap();
    assert_eq!(session.value("load"), "0");
    assert!(session.document().plain_text().contains("Healthy"));
}

#[test]
fn containers_budget_their_stacked_height_and_skip_the_node_cap() {
    let document = parse(DASHBOARD).unwrap();
    let [top, grid, controls] = document.nodes() else {
        panic!("three containers")
    };
    // Stacked worst case: children plus one gap row between each.
    assert_eq!(top.rows(), 5 + (1 + 1) + 1);
    assert_eq!(grid.rows(), 2 * 3 + 2);
    assert_eq!(controls.rows(), 1 + 3 + 1);
    let stack =
        parse(r#"root = Panel("S", [Stack([Text("a"), Text("b"), Text("c")], 2)])"#).unwrap();
    assert_eq!(stack.nodes()[0].rows(), 3 + 2 * 2);
    // Sixteen visible components fit even when wrapped in containers: eight
    // rows of two badges, each row budgeting 1 + 1 + 1 stacked rows.
    let rows = (0..8)
        .map(|n| format!("Row([Badge(\"{n}a\"), Badge(\"{n}b\")])"))
        .collect::<Vec<_>>()
        .join(", ");
    let fits = parse(&format!("root = Panel(\"Cap\", [{rows}])"));
    assert!(fits.is_ok(), "{fits:?}");
    let too_many = format!("root = Panel(\"Cap\", [{rows}, Badge(\"x\")])");
    assert!(
        parse(&too_many)
            .unwrap_err()
            .to_string()
            .contains("1–16 view nodes")
    );
}

#[test]
fn container_limits_are_checked() {
    for (source, message) in [
        (r#"Row([Text("only")])"#, "2–4 items"),
        (r#"Row([Text("a"), Text("b")], 9)"#, "row gap must be 0–4"),
        (
            r#"Grid([Text("a"), Text("b")], 5)"#,
            "grid cols must be 2–4",
        ),
        (r#"Stack([Text("a")], 4)"#, "stack gap must be 0–3"),
        ("Spacer(6)", "spacer rows must be 1–4"),
        (
            r#"Grid([Card("API", "ok"), Card("DB", "ok")])"#,
            "expected type string",
        ),
    ] {
        let error = parse(&format!(
            "root = Panel(\"Bad\", [Text(\"t\"), node])\nnode = {source}"
        ))
        .err()
        .unwrap_or_else(|| panic!("{source} should fail"))
        .to_string();
        assert!(error.contains(message), "{source}: {error}");
    }
}

#[test]
fn streaming_keeps_actions_of_buttons_inside_containers() {
    let source = r#"root = Panel("Live", [Row([Text("Count"), Buttons([Button("Add", @Set($n, $n + 1))])])])
$n = 0
"#;
    let document = StreamParser::default().set(source).unwrap();
    assert_eq!(document.nodes().len(), 1);
    let mut session = Session::new(document).unwrap();
    let buttons = session
        .visible_nodes()
        .into_iter()
        .find(|(_, node)| matches!(node, Node::Buttons { .. }))
        .unwrap()
        .0;
    session.activate_button(buttons, 0).unwrap();
    assert_eq!(session.value("n"), "1");
}
