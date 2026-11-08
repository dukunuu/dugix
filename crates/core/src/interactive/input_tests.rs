use super::*;

fn session(source: &str) -> Session {
    Session::new(Document::parse(source).unwrap()).unwrap()
}

#[test]
fn existing_input_arguments_and_new_textarea_contracts_are_compatible() {
    let session = session(
        "root = Panel('Contact', [Input($email, 'Email', {email: true}, 'you@example.com', 'Work email'), Textarea($notes, 'Notes')])\n$email = ''\n$notes = 'First\\nSecond'",
    );
    assert!(
        matches!(session.document().node(0), Some(Node::Input { rows: 1, placeholder, hint, .. }) if placeholder == "you@example.com" && hint == "Work email")
    );
    assert!(matches!(
        session.document().node(1),
        Some(Node::Input { rows: 4, .. })
    ));
    assert_eq!(session.value("notes"), "First\nSecond");
    let legacy = Document::parse(r#"{"version":1,"title":"Legacy","state":{"name":"Ada"},"view":[{"type":"input","key":"name","label":"Name"}]}"#).unwrap();
    assert!(
        matches!(legacy.node(0), Some(Node::Input { rows: 1, placeholder, hint, .. }) if placeholder.is_empty() && hint.is_empty())
    );
}

#[test]
fn textareas_validate_rows_controls_and_byte_limits_transactionally() {
    let mut session = session(
        "root = Panel('Contact', [Input($name, 'Name'), Textarea($notes, 'Notes', 3)])\n$name = ''\n$notes = ''",
    );
    assert!(session.input("name", "one\ntwo".into()).is_err());
    assert!(session.input("name", "x".repeat(1025)).is_err());
    session.input("notes", "李\n".repeat(1024)).unwrap();
    assert_eq!(session.value("notes").len(), 4096);
    for invalid in ["x".repeat(4097), "bad\u{1b}".into(), "tab\t".into()] {
        assert!(session.input("notes", invalid).is_err());
        assert_eq!(session.value("notes").len(), 4096);
    }
    for rows in [2, 4, 6, 8] {
        let source =
            format!("root = Panel('Notes', [Textarea($notes, 'Notes', {rows})])\n$notes = ''");
        assert!(Document::parse(&source).is_ok());
    }
    for rows in [0, 1, 9, u16::MAX] {
        let source =
            format!("root = Panel('Invalid', [Textarea($notes, 'Notes', {rows})])\n$notes = ''");
        assert!(Document::parse(&source).is_err());
    }
    for invalid in ["bad\\nplaceholder", "bad\\u001bplaceholder"] {
        let source = format!(
            "root = Panel('Invalid', [Input($name, 'Name', null, '{invalid}')])\n$name = ''"
        );
        assert!(Document::parse(&source).is_err());
    }
}

#[test]
fn nested_textarea_validation_blocks_sends_and_preserves_multiline_payloads() {
    let mut session = session(
        "root = Panel('Feedback', [Form('Details', [Textarea($notes, 'Notes', 3, 'Your feedback', {required:true, maxLength:4096}, 'Be specific')], 'Send', @ToAssistant('Feedback'))])\n$notes = ''",
    );
    assert_eq!(session.input_error("notes").as_deref(), Some("Required"));
    assert!(!session.form_valid(0));
    assert!(session.submit_form(0).is_err());
    session.input("notes", "First\nSecond".into()).unwrap();
    assert!(session.form_valid(0));
    assert!(session.input_error("notes").is_none());
    session.submit_form(0).unwrap();
    assert_eq!(
        session.assistant_payload().unwrap()["state"]["notes"],
        "First\nSecond"
    );
}

#[test]
fn length_rules_cannot_exceed_the_fields_own_byte_budget() {
    let input = |max: usize| {
        format!("root = Panel('Name', [Input($name, 'Name', {{maxLength: {max}}})])\n$name = ''")
    };
    assert!(Document::parse(&input(1024)).is_ok());
    assert!(Document::parse(&input(1025)).is_err());
    let textarea = |max: usize| {
        format!(
            "root = Panel('Notes', [Textarea($notes, 'Notes', 3, null, {{maxLength: {max}}})])\n$notes = ''"
        )
    };
    assert!(Document::parse(&textarea(4096)).is_ok());
    assert!(Document::parse(&textarea(4097)).is_err());
}

#[test]
fn placeholders_and_hints_share_the_label_length_cap() {
    for (placeholder, hint) in [
        ("x".repeat(121), String::new()),
        (String::new(), "x".repeat(121)),
    ] {
        let source = format!(
            "root = Panel('Name', [Input($name, 'Name', null, '{placeholder}', '{hint}')])\n$name = ''"
        );
        assert!(Document::parse(&source).is_err());
    }
}
