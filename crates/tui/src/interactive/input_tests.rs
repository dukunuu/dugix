use super::{tests::*, *};

fn focused_input(source: &str) -> (Widgets, Placement) {
    let placement = placement(source, 0);
    let mut widgets = Widgets::default();
    draw(&mut widgets, &[placement.clone()], 0);
    assert!(widgets.focus_latest());
    (widgets, placement)
}

fn value(widgets: &Widgets, placement: &Placement, key: &str) -> String {
    widgets.entries[&placement.key()]
        .session
        .as_ref()
        .unwrap()
        .value(key)
}

#[test]
fn input_arrows_edit_at_the_caret_without_changing_focus() {
    let (mut widgets, placement) = focused_input(
        "root = Panel('Contact', [Input($name, 'Name'), Input($email, 'Email')])\n$name = 'Ada'\n$email = ''",
    );
    widgets.key(KeyCode::Left.into(), false);
    widgets.key(KeyCode::Char('X').into(), false);
    assert_eq!(value(&widgets, &placement, "name"), "AdXa");
    assert_eq!(value(&widgets, &placement, "email"), "");
    widgets.key(KeyCode::Delete.into(), false);
    assert_eq!(value(&widgets, &placement, "name"), "AdX");
}

#[test]
fn input_selection_replaces_text_and_owns_modified_edit_keys() {
    let (mut widgets, placement) =
        focused_input("root = Panel('Contact', [Input($name, 'Name')])\n$name = 'Ada'");
    widgets.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT), false);
    widgets.paste("èle");
    assert_eq!(value(&widgets, &placement, "name"), "Adèle");
    widgets.key(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        false,
    );
    widgets.key(KeyCode::Char('李').into(), false);
    assert_eq!(value(&widgets, &placement, "name"), "李");
}

#[test]
fn input_digits_do_not_activate_answer_shortcuts() {
    let (mut widgets, placement) = focused_input(
        "root = Panel('Question', [Input($answer, 'Answer'), Buttons([Button('Yes', @Answer()), Button('No', @Answer())])])\n$answer = ''",
    );
    assert_eq!(widgets.key(KeyCode::Char('1').into(), true).1, None);
    assert_eq!(value(&widgets, &placement, "answer"), "1");
}

fn pointer(widgets: &mut Widgets, column: u16, row: u16, dragging: bool) {
    let hit = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap()
        .clone();
    assert!(
        widgets
            .click(
                MouseEvent {
                    kind: if dragging {
                        MouseEventKind::Drag(MouseButton::Left)
                    } else {
                        MouseEventKind::Down(MouseButton::Left)
                    },
                    column: hit.rect.x + column,
                    row: hit.rect.y + row,
                    modifiers: KeyModifiers::NONE,
                },
                Rect::new(0, 0, 80, 32),
                false
            )
            .0
    );
}

#[test]
fn modified_edit_keys_cannot_confirm_or_trust_a_pending_url() {
    let (mut widgets, placement) = focused_input(
        "root = Panel('Visit', [Buttons([Button('Visit', @OpenUrl('https://example.com'))])])",
    );
    widgets
        .entries
        .get_mut(&placement.key())
        .unwrap()
        .activate(0, true, true);
    assert!(widgets.pending_url_focused());
    for code in [KeyCode::Char('a'), KeyCode::Enter] {
        widgets.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL), false);
        assert!(widgets.pending_url_focused());
    }
    assert!(
        widgets.entries[&placement.key()]
            .session
            .as_ref()
            .unwrap()
            .pending_url()
            .is_some()
    );
}

#[test]
fn only_text_fields_show_a_terminal_cursor_not_highlighted_primary_buttons() {
    let (mut widgets, placement) = focused_input(
        "root = Panel('Contact', [Input($name, 'Name'), Buttons([Button('Save', @ToAssistant('Save'), true)])])\n$name = 'Ada'",
    );
    assert!(
        draw(&mut widgets, &[placement.clone()], 0)
            .backend()
            .cursor_visible()
    );
    widgets.key(KeyCode::Tab.into(), false);
    assert!(
        !draw(&mut widgets, &[placement], 0)
            .backend()
            .cursor_visible()
    );
}

#[test]
fn word_navigation_document_selection_and_key_repeats_stay_in_the_field() {
    let (mut widgets, placement) = focused_input(
        "root = Panel('Notes', [Textarea($notes, 'Notes')])\n$notes = 'one two\\nthree'",
    );
    widgets.key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL), false);
    widgets.key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT), false);
    widgets.key(
        KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        false,
    );
    widgets.paste("TWO");
    assert_eq!(value(&widgets, &placement, "notes"), "one TWO");
    widgets.key(
        KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        false,
    );
    widgets.paste("Z");
    assert_eq!(value(&widgets, &placement, "notes"), "Z");
    let mut repeated = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
    repeated.kind = KeyEventKind::Repeat;
    widgets.key(repeated, false);
    widgets.key(KeyCode::Char('A').into(), false);
    assert_eq!(value(&widgets, &placement, "notes"), "AZ");
}

#[test]
fn mouse_places_the_caret_by_display_cells_including_wide_text() {
    let (mut widgets, placement) =
        focused_input("root = Panel('Contact', [Input($name, 'Name')])\n$name = 'A李B'");
    // Content starts two columns into the field. Click the second cell of 李.
    pointer(&mut widgets, 4, 1, false);
    widgets.key(KeyCode::Char('X').into(), false);
    assert_eq!(value(&widgets, &placement, "name"), "AX李B");
    draw(&mut widgets, &[placement.clone()], 0);
    pointer(&mut widgets, 2, 1, false);
    pointer(&mut widgets, 3, 1, true);
    widgets.paste("Z");
    assert_eq!(value(&widgets, &placement, "name"), "ZX李B");
}

#[test]
fn mouse_drag_keeps_its_original_selection_anchor() {
    let (mut widgets, placement) =
        focused_input("root = Panel('Contact', [Input($name, 'Name')])\n$name = 'hello world'");
    pointer(&mut widgets, 2, 1, false);
    pointer(&mut widgets, 4, 1, true);
    pointer(&mut widgets, 7, 1, true);
    widgets.paste("Hi");
    assert_eq!(value(&widgets, &placement, "name"), "Hi world");
}

#[test]
fn clipped_and_expanded_fields_keep_mouse_and_terminal_carets_aligned() {
    let (mut widgets, placement) =
        focused_input("root = Panel('Contact', [Input($name, 'Name')])\n$name = 'Ada'");
    let mut terminal = draw(&mut widgets, &[placement.clone()], 3);
    let hit = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap();
    assert_eq!(hit.row_offset, 1);
    assert_eq!(
        terminal.get_cursor_position().unwrap(),
        Position::new(hit.rect.x + 5, hit.rect.y)
    );
    pointer(&mut widgets, 3, 0, false);
    widgets.paste("X");
    assert_eq!(value(&widgets, &placement, "name"), "AXda");
    terminal
        .draw(|frame| widgets.popup(frame, &Theme::default(), Rect::new(4, 4, 50, 20), false))
        .unwrap();
    pointer(&mut widgets, 2, 1, false);
    widgets.paste("Y");
    assert_eq!(value(&widgets, &placement, "name"), "YAXda");
}

#[test]
fn expanded_textarea_scroll_follows_the_caret_in_a_short_viewport() {
    let (mut widgets, placement) = focused_input(
        "root = Panel('Notes', [Textarea($notes, 'Notes', 8)])\n$notes = '1\\n2\\n3\\n4\\n5\\n6\\n7\\n8\\n9'",
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 32)).unwrap();
    let area = Rect::new(2, 2, 50, 3);
    terminal
        .draw(|frame| widgets.popup(frame, &Theme::default(), area, false))
        .unwrap();
    assert!(area.contains(terminal.get_cursor_position().unwrap()));
    assert!(widgets.entries[&placement.key()].scroll > 0);
}

#[test]
fn horizontally_scrolled_input_clicks_use_the_visible_text_not_the_start() {
    let initial = "abcdefghijklmnopqrstuvwxyz".repeat(4);
    let source = format!("root = Panel('Long', [Input($name, 'Name')])\n$name = '{initial}'");
    let (mut widgets, placement) = focused_input(&source);
    draw(&mut widgets, &[placement.clone()], 0);
    // Field inner width is 62; the end caret reserves one visible cell.
    pointer(&mut widgets, 4, 1, false);
    widgets.paste("!");
    let mut expected = initial;
    expected.insert(45, '!');
    assert_eq!(value(&widgets, &placement, "name"), expected);
}

#[test]
fn textarea_wraps_and_supports_mouse_editing_newlines_and_tab_navigation() {
    let source = "root = Panel('Notes', [Textarea($notes, 'Notes', 3), Input($name, 'Name')])\n$notes = 'abcdefghijklmnopqrs'\n$name = ''";
    let mut placement = placement(source, 0);
    placement.width = 24;
    placement.height = source_height(source, placement.width);
    let mut widgets = Widgets::default();
    draw(&mut widgets, &[placement.clone()], 0);
    assert!(widgets.focus_latest());
    draw(&mut widgets, &[placement.clone()], 0);
    pointer(&mut widgets, 6, 1, false);
    widgets.key(KeyCode::Char('X').into(), false);
    assert_eq!(value(&widgets, &placement, "notes"), "abcdXefghijklmnopqrs");
    widgets.key(KeyCode::Enter.into(), true);
    widgets.paste("one\r\ntwo\u{1b}");
    assert_eq!(
        value(&widgets, &placement, "notes"),
        "abcdX\none\ntwoefghijklmnopqrs"
    );
    widgets.key(KeyCode::Tab.into(), false);
    widgets.key(KeyCode::Char('A').into(), false);
    assert_eq!(value(&widgets, &placement, "name"), "A");
}

#[test]
fn undo_redo_rejected_edits_and_external_reset_preserve_consistent_state() {
    let (mut widgets, placement) = focused_input(
        "root = Panel('Contact', [Input($name, 'Name'), Buttons([Button('Reset', @Reset($name))])])\n$name = 'Ada'",
    );
    widgets.key(KeyCode::Left.into(), false);
    widgets.paste(&"x".repeat(1025));
    assert_eq!(value(&widgets, &placement, "name"), "Ada");
    assert!(!widgets.entries[&placement.key()].error.is_empty());
    widgets.key(KeyCode::Char('X').into(), false);
    assert_eq!(value(&widgets, &placement, "name"), "AdXa");
    widgets.key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        false,
    );
    assert_eq!(value(&widgets, &placement, "name"), "Ada");
    widgets.key(
        KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        false,
    );
    assert_eq!(value(&widgets, &placement, "name"), "AdXa");
    widgets.key(KeyCode::Tab.into(), false);
    widgets.key(KeyCode::Enter.into(), false);
    widgets.key(KeyCode::BackTab.into(), false);
    widgets.key(KeyCode::Char('!').into(), false);
    assert_eq!(value(&widgets, &placement, "name"), "Ada!");
}

#[test]
fn fields_use_theme_borders_placeholders_help_and_inline_errors_at_every_width() {
    let source = "root = Panel('Contact', [Input($email, 'Email', {required: true, email: true}, 'you@example.com', 'Use your work email'), Textarea($notes, 'Notes', 3, 'What should we know?')])\n$email = ''\n$notes = ''";
    for width in [0, 1, 2, 4, 8, 24, 70, 100] {
        let entry = Entry::new(Ok(Session::new(Document::parse(source).unwrap()).unwrap()));
        let (buffer, hits) = entry.canvas(width, &Theme::default(), true, true);
        assert!(
            hits.iter()
                .all(|(rect, _)| rect.intersection(buffer.area) == *rect)
        );
        if width >= 70 {
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("Email *"));
            assert!(text.contains("you@example.com"));
            assert!(text.contains("Required"));
            assert!(text.contains("What should we know?"));
        }
    }
}

#[test]
fn single_line_paste_folds_newlines_into_spaces() {
    let (mut widgets, placement) =
        focused_input("root = Panel('Contact', [Input($name, 'Name')])\n$name = ''");
    widgets.paste("Ada\r\nLovelace\nKing");
    assert_eq!(value(&widgets, &placement, "name"), "Ada Lovelace King");
}

#[test]
fn single_line_inputs_leave_page_keys_to_the_panel_but_textareas_keep_them() {
    let (mut widgets, placement) = focused_input(
        "root = Panel('Contact', [Input($name, 'Name'), Textarea($notes, 'Notes')])\n$name = ''\n$notes = ''",
    );
    assert!(widgets.input_focused());
    assert!(!widgets.textarea_focused());
    let entry = widgets.entries.get_mut(&placement.key()).unwrap();
    let node = entry.focused_input().unwrap();
    let session = entry.session.as_mut().unwrap();
    assert!(
        !entry
            .inputs
            .get_mut()
            .key(session, node, KeyCode::PageUp.into())
            .unwrap()
    );
    widgets.key(KeyCode::Tab.into(), false);
    assert!(widgets.textarea_focused());
}

#[test]
fn focused_fields_keep_the_focus_border_even_while_invalid() {
    let source = "root = Panel('Contact', [Input($email, 'Email', {required: true})])\n$email = ''";
    let entry = Entry::new(Ok(Session::new(Document::parse(source).unwrap()).unwrap()));
    let theme = Theme::default();
    let border = |focused: bool| {
        let (buffer, hits) = entry.canvas(60, &theme, focused, true);
        let (rect, _) = hits.first().copied().unwrap();
        buffer[(rect.x, rect.y)].fg
    };
    assert_eq!(border(true), theme.accent);
    assert_eq!(border(false), theme.error);
}

#[test]
fn a_drag_past_the_field_edge_keeps_extending_the_selection() {
    let (mut widgets, placement) =
        focused_input("root = Panel('Contact', [Input($name, 'Name')])\n$name = 'hello world'");
    pointer(&mut widgets, 2, 1, false);
    pointer(&mut widgets, 70, 1, true);
    widgets.paste("Hi");
    assert_eq!(value(&widgets, &placement, "name"), "Hi");
}
