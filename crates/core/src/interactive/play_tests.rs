use super::*;

fn session(source: &str) -> Session {
    Session::new(Document::parse(source).unwrap()).unwrap()
}
fn error(source: &str) -> String {
    Document::parse(source)
        .err()
        .unwrap_or_else(|| panic!("{source} should fail"))
        .to_string()
}

#[test]
fn timers_run_on_their_interval_and_skip_a_backlog() {
    let mut session = session(
        r#"root = Panel("Clock", [Text("Ticks: " + $n)])
$n = 0
tick = @Every(0.5, @Set($n, $n + 1))"#,
    );
    assert!(session.has_timers());
    // The first tick only schedules; then every half second runs once.
    assert!(!session.tick(0.0).unwrap());
    assert!(!session.tick(0.4).unwrap());
    assert!(session.tick(0.5).unwrap());
    assert_eq!(session.value("n"), "1");
    assert!(!session.tick(0.7).unwrap());
    assert!(session.tick(1.0).unwrap());
    assert_eq!(session.value("n"), "2");
    // Off screen for a while: one catch-up run, not twenty.
    assert!(session.tick(10.0).unwrap());
    assert_eq!(session.value("n"), "3");
    assert!(!session.tick(10.2).unwrap());
    assert!(session.tick(10.5).unwrap());
    assert_eq!(session.value("n"), "4");
}

#[test]
fn timer_and_key_actions_stay_local() {
    let base = "root = Panel(\"P\", [Text(\"x\")])\n$n = 0\n";
    for (statement, message) in [
        (
            r#"tick = @Every(1, @ToAssistant("hi"))"#,
            "may only @Set or @Reset",
        ),
        (
            r#"tick = @Every(1, Action([@Set($n, 1), @OpenUrl("https://example.com")]))"#,
            "may only @Set or @Reset",
        ),
        ("tick = @Every(0.01, @Set($n, 1))", "0.1–3600 seconds"),
        (r"keys = @Keys({enter: @Set($n, 1)})", "unbindable key"),
        (
            r"keys = @Keys({left: @Answer()})",
            "may only @Set or @Reset",
        ),
    ] {
        let error = error(&format!("{base}{statement}"));
        assert!(error.contains(message), "{statement}: {error}");
    }
    let error = error(
        r#"root = Panel("P", [@Every(1, @Set($n, 1))])
$n = 0"#,
    );
    assert!(error.contains("is a statement"), "{error}");
}

#[test]
fn key_bindings_run_their_actions() {
    let mut session = session(
        r#"root = Panel("Pad", [Text("x = " + $x)])
$x = 5
keys = @Keys({left: @Set($x, $x - 1), right: @Set($x, $x + 1), space: @Reset($x)})"#,
    );
    assert!(session.press_key("right").unwrap());
    assert!(session.press_key("right").unwrap());
    assert!(session.press_key("left").unwrap());
    assert_eq!(session.value("x"), "6");
    assert!(session.press_key("space").unwrap());
    assert_eq!(session.value("x"), "5");
    assert!(
        !session.press_key("q").unwrap(),
        "unbound keys pass through"
    );
}

#[test]
fn streaming_previews_keep_timers_and_keys_inert() {
    let source = r#"root = Panel("Live", [Text("t")])
$n = 0
tick = @Every(1, @Set($n, $n + 1))
keys = @Keys({up: @Set($n, 0)})
"#;
    let preview = StreamParser::default().set(source).unwrap();
    assert!(preview.timers().is_empty());
    assert!(preview.key_action("up").is_none());
    let complete = Document::parse(source).unwrap();
    assert_eq!(complete.timers().len(), 1);
    assert!(complete.key_action("up").is_some());
}

#[test]
fn clickable_shapes_are_controls_with_their_own_actions() {
    let source = r#"root = Panel("Target", [Canvas([Circle(50, 50, 10, "warning", true, null, @Set($hits, $hits + 1)), Rect(0, 0, 100, 100, "muted")])])
$hits = 0"#;
    let mut session = session(source);
    assert_eq!(session.document().control_count(), 1);
    assert_eq!(session.activate_shape(0, 0).unwrap(), None);
    assert_eq!(session.value("hits"), "1");
    assert!(
        session.activate_shape(0, 1).is_err(),
        "the frame isn't clickable"
    );
    let error = error(r#"root = Panel("T", [Canvas([Dot(1, 1, null, null, @Answer())])])"#);
    assert!(error.contains("can't answer"), "{error}");
}

#[test]
fn array_builtins_edit_lists_without_going_out_of_bounds() {
    let list = json!(["a", "b", "c"]);
    let run = |op: &str, args: &[Value]| data::evaluate(op, args).unwrap();
    assert_eq!(
        run("append", &[list.clone(), json!("d")]),
        json!(["a", "b", "c", "d"])
    );
    assert_eq!(run("remove", &[list.clone(), json!(0)]), json!(["b", "c"]));
    assert_eq!(run("remove", &[list.clone(), json!(-1)]), json!(["a", "b"]));
    assert_eq!(run("remove", &[list.clone(), json!(9)]), list);
    assert_eq!(
        run("replace", &[list.clone(), json!(1), json!("z")]),
        json!(["a", "z", "c"])
    );
    assert_eq!(run("slice", &[list.clone(), json!(1)]), json!(["b", "c"]));
    assert_eq!(
        run("slice", &[list.clone(), json!(0), json!(-1)]),
        json!(["a", "b"])
    );
    assert_eq!(run("slice", &[list, json!(5), json!(2)]), json!([]));
    let mut session = session(
        r#"root = Panel("Todo", [Text("Items: " + @Count($todos)), Buttons([Button("Add", @Set($todos, @Append($todos, "new"))), Button("Drop first", @Set($todos, @Remove($todos, 0)))])])
$todos = ["milk", "eggs"]"#,
    );
    session.activate_button(1, 0).unwrap();
    session.activate_button(1, 1).unwrap();
    session.activate_button(1, 1).unwrap();
    let text = session.document().plain_text();
    assert!(text.contains("Items: 1"), "{text}");
}
