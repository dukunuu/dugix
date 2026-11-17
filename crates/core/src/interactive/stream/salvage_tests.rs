use super::*;
use crate::interactive::{Node, Session};

const BROKEN: &str = r#"root = Panel("Trip planner", [heading, chart, days, save])
$days = 5
heading = Hero("Make room for adventure")
chart = PieChart([1, 2, 3])
days = Stepper($days, "Days away", 1, 30, 1)
save = Buttons([Button("Save plan", @ToAssistant("Plan " + $days + " days"), true)])"#;

#[test]
fn a_broken_component_is_left_out_and_the_rest_renders() {
    assert!(Document::check(BROKEN).is_err());
    let (document, dropped) = Document::recover(BROKEN).unwrap();
    assert_eq!(document.nodes().len(), 3);
    assert!(
        dropped
            .iter()
            .any(|error| error.message.contains("unknown component PieChart")),
        "{dropped:?}"
    );
}

#[test]
fn a_salvaged_panel_keeps_working_after_state_changes() {
    let (document, _) = Document::recover(BROKEN).unwrap();
    let mut session = Session::new(document).unwrap();
    session.step(1, true).unwrap();
    assert_eq!(session.document().nodes().len(), 3);
    assert_eq!(
        session.activate_button(2, 0).unwrap(),
        Some("UI response — Trip planner\nPlan 6 days".into())
    );
}

#[test]
fn a_statement_that_does_not_parse_is_left_out() {
    let source = "root = Panel(\"Notes\", [intro, status, outro])\nintro = Text(\"Before\")\nstatus = Badge(= \"Handoff\")\noutro = Text(\"After\")";
    let (document, dropped) = Document::recover(source).unwrap();
    assert_eq!(document.nodes().len(), 2);
    assert!(dropped[0].message.contains("line 3"), "{dropped:?}");
    let session = Session::new(document).unwrap();
    assert!(!session.program().unwrap().contains("Badge"));
}

#[test]
fn a_broken_inline_child_is_left_out_of_its_list() {
    let source = "root = Panel(\"Inline\", [Text(\"Kept\"), Text(\"Too\", \"muted\", 1, 2), Text(\"Also kept\")])";
    let (document, dropped) = Document::recover(source).unwrap();
    assert_eq!(document.nodes().len(), 2);
    assert!(!dropped.is_empty());
}

#[test]
fn a_panel_with_nothing_left_or_no_valid_root_is_still_rejected() {
    for source in [
        "root = Panel(\"Bad\", [missing])",
        "root = Pane(\"Typo\", [Text(\"Hi\")])",
        "root = Panel(\"Bad\", [Canvas(4)])",
        "{\"version\": 1, \"title\": \"Legacy\", \"view\": [{\"type\": \"nope\"}]}",
    ] {
        assert!(Document::recover(source).is_err(), "{source}");
    }
}

#[test]
fn a_valid_panel_recovers_unchanged_with_nothing_dropped() {
    let (document, dropped) = Document::recover(
        BROKEN
            .replace("PieChart([1, 2, 3])", "Text(\"Chart\")")
            .as_str(),
    )
    .unwrap();
    assert!(dropped.is_empty());
    assert_eq!(document.nodes().len(), 4);
}

#[test]
fn patching_the_broken_statement_restores_the_whole_panel() {
    let (document, _) = Document::recover(BROKEN).unwrap();
    let mut session = Session::new(document).unwrap();
    session.patch("chart = Text(\"Chart\")").unwrap();
    assert_eq!(session.document().nodes().len(), 4);
    let Node::Text { text, .. } = &session.document().nodes()[1] else {
        panic!("text")
    };
    assert_eq!(text, "Chart");
}

#[test]
fn an_unrelated_patch_keeps_the_salvaged_panel() {
    let (document, _) = Document::recover(BROKEN).unwrap();
    let mut session = Session::new(document).unwrap();
    session.patch("heading = Hero(\"Renamed\")").unwrap();
    assert_eq!(session.document().nodes().len(), 3);
}

#[test]
fn timers_survive_when_a_sibling_is_left_out() {
    let source = "root = Panel(\"Clock\", [Text(\"{{$n}}\"), PieChart([1])])\n$n = 0\ntick = @Every(1, @Set($n, $n + 1))";
    let (document, _) = Document::recover(source).unwrap();
    assert_eq!(document.nodes().len(), 1);
    assert_eq!(document.timers.len(), 1);
}
