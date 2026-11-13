//! The shared popup pattern, as Help, Context, Usage, Research, Image viewer, API key, Tool approval and the interactive panel follow it.
#![allow(unused_imports)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::Popup;

use super::tests::{render_to_string, test_app};

fn assert_frame(screen: &str, title: &str, escape: &str) {
    let top = screen.lines().find(|line| line.contains('╭')).unwrap();
    let bottom = screen.lines().find(|line| line.contains('╰')).unwrap();
    assert!(top.contains(&format!(" {title} ")), "{screen}");
    assert!(
        bottom.trim_end().ends_with(&format!("{escape} ╯")),
        "{screen}"
    );
}

#[test]
fn viewer_frames_put_names_and_escape_hints_in_the_borders() {
    let mut app = test_app();
    let root = app.space.root.clone();
    app.popup = Popup::Help;
    let screen = render_to_string(110, 30, |frame| super::help::render(frame, &mut app));
    assert_frame(&screen, "Help", "Esc close");
    app.popup = Popup::Context;
    let screen = render_to_string(110, 30, |frame| super::context::render(frame, &app));
    assert_frame(&screen, "Context", "Esc close");
    assert!(!screen.contains("Total"), "{screen}");
    assert!(
        screen
            .lines()
            .find(|line| line.contains('╭'))
            .unwrap()
            .contains("of unknown window"),
        "{screen}"
    );
    app.open_usage_popup();
    let screen = render_to_string(110, 30, |frame| super::usage::render(frame, &app));
    assert_frame(&screen, "Usage", "Esc close");
    assert!(screen.contains("24h   7d   30d   All"), "{screen}");
    app.popup = Popup::ResearchLive;
    let screen = render_to_string(110, 30, |frame| super::research_live::render(frame, &app));
    assert_frame(&screen, "Research", "Esc close");
    assert!(screen.contains("Waiting for the first update…"), "{screen}");
    app.popup = Popup::Key;
    let screen = render_to_string(110, 30, |frame| super::key::render(frame, &app));
    assert_frame(&screen, "OpenRouter key", "Esc back");
    assert!(screen.contains("✎ Paste or type the key"), "{screen}");
    assert!(
        screen.contains("Keys stay on this machine, in your config file."),
        "{screen}"
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn help_only_escape_closes_and_letters_still_scroll() {
    let mut app = test_app();
    let root = app.space.root.clone();
    app.popup = Popup::Help;
    for key in [KeyCode::Char('q'), KeyCode::Enter, KeyCode::F(1)] {
        super::help::handle_key(&mut app, key.into());
        assert_eq!(app.popup, Popup::Help);
    }
    super::help::handle_key(&mut app, KeyCode::Char('j').into());
    assert_eq!(app.help_scroll, 1);
    super::help::handle_key(&mut app, KeyCode::Char('k').into());
    assert_eq!(app.help_scroll, 0);
    super::help::handle_key(&mut app, KeyCode::Esc.into());
    assert_eq!(app.popup, Popup::None);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn api_key_field_masks_every_typed_character() {
    let mut app = test_app();
    let root = app.space.root.clone();
    app.popup = Popup::Key;
    for character in "sk-secret".chars() {
        super::key::handle_key(&mut app, KeyCode::Char(character).into());
    }
    let screen = render_to_string(110, 30, |frame| super::key::render(frame, &app));
    assert_frame(&screen, "OpenRouter key", "Esc back");
    assert!(!screen.contains("secret"), "{screen}");
    assert!(!screen.contains("sk-"), "{screen}");
    assert!(screen.contains("✎ •••••••••▏"), "{screen}");
    assert_eq!(screen.matches('•').count(), 9);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn research_steer_is_a_field_and_never_part_of_the_title() {
    let mut app = test_app();
    let root = app.space.root.clone();
    app.popup = Popup::ResearchLive;
    let screen = render_to_string(110, 30, |frame| super::research_live::render(frame, &app));
    assert!(
        screen
            .lines()
            .find(|line| line.contains("Steer the run…"))
            .unwrap()
            .contains("│ ❯ Steer the run…"),
        "{screen}"
    );
    let top = screen.lines().find(|line| line.contains('╭')).unwrap();
    assert!(!top.to_lowercase().contains("steer"), "{top}");
    app.research.stage_rows = vec!["writer: working — drafting".into()];
    app.research.live_input = "focus on evidence".into();
    let screen = render_to_string(110, 30, |frame| super::research_live::render(frame, &app));
    let top = screen.lines().find(|line| line.contains('╭')).unwrap();
    assert!(top.contains("writer"), "{screen}");
    assert!(!top.contains("focus on evidence"), "{top}");
    assert!(screen.contains("❯ focus on evidence▏"), "{screen}");
    assert!(
        screen.contains("Enter send · ^X stop · Esc close"),
        "{screen}"
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn usage_refresh_moves_to_plain_r() {
    let mut app = test_app();
    let root = app.space.root.clone();
    app.open_usage_popup();
    app.db
        .log_usage(
            "OpenRouter",
            "test-model",
            100,
            10,
            0,
            0,
            Default::default(),
            None,
            false,
            None,
            None,
        )
        .unwrap();
    super::usage::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.usage_data.as_ref().unwrap().totals.requests, 0);
    super::usage::handle_key(&mut app, KeyCode::Char('r').into());
    assert_eq!(app.usage_data.as_ref().unwrap().totals.requests, 1);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn image_viewer_and_actions_keep_q_open_and_use_escape_to_go_back() {
    use crate::image_viewer::{Anchor, Item, Return};

    let mut app = test_app();
    let root = app.space.root.clone();
    app.image_viewer.start(
        vec![Item {
            target: root.join("missing.png").to_string_lossy().into_owned(),
            anchor: Anchor::ByTarget,
        }],
        0,
        Return::Nothing,
    );
    app.popup = Popup::ImageViewer;
    super::image_viewer::handle_key(&mut app, KeyCode::Char('q').into());
    assert_eq!(app.popup, Popup::ImageViewer);
    let screen = render_to_string(110, 30, |frame| {
        super::image_viewer::render(frame, &mut app)
    });
    assert_frame(&screen, "missing.png", "Esc close");
    super::image_viewer::handle_key(&mut app, KeyCode::Char('.').into());
    super::image_viewer::handle_key(&mut app, KeyCode::Char('q').into());
    assert!(app.image_viewer.pointer.menu.is_some());
    let screen = render_to_string(110, 30, |frame| {
        super::image_viewer::render(frame, &mut app)
    });
    assert!(screen.contains("╭ Actions "), "{screen}");
    assert!(screen.contains("↑↓ · Enter run · Esc back"), "{screen}");
    super::image_viewer::handle_key(&mut app, KeyCode::Esc.into());
    assert_eq!(app.popup, Popup::ImageViewer);
    assert!(app.image_viewer.pointer.menu.is_none());
    super::image_viewer::handle_key(&mut app, KeyCode::Esc.into());
    assert_eq!(app.popup, Popup::None);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn expanded_interactive_panel_uses_the_shared_name_and_footer() {
    let mut app = test_app();
    let root = app.space.root.clone();
    app.messages.push(nexus_core::db::Message {
        role: "assistant".into(),
        content: r#"```nexus-ui
{"version":1,"title":"Counter","state":{"count":0},"view":[{"type":"text","text":"Count: {{count}}"},{"type":"buttons","items":[{"label":"Add one","action":"add"}]}],"actions":{"add":[{"op":"set","key":"count","value":1}]}}
```"#.into(),
        model: None,
        reasoning: None,
        tokens: None,
        secs: None,
        cost: None,
        phrase: None,
        persona: None,
        created_at: None,
    });
    render_to_string(110, 30, |frame| crate::ui::render(frame, &mut app));
    crate::interactive::focus(&mut app);
    crate::interactive::key(&mut app, KeyCode::Tab.into());
    crate::interactive::key(&mut app, KeyCode::Enter.into());
    let screen = render_to_string(110, 30, |frame| crate::ui::render(frame, &mut app));
    assert_frame(&screen, "Counter", "Esc close");
    assert!(
        screen.contains("Tab next · Enter choose · Esc close"),
        "{screen}"
    );
    assert!(screen.contains("Count: 0"), "{screen}");
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}
