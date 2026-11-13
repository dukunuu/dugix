//! The shared popup pattern, as Settings, Login, Local models, Copy, Connectors and Swarm follow it.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::Popup;

use super::tests::{render_to_string, test_app};

fn assert_frame(screen: &str, title: &str, count: &str, escape: &str) {
    let top = screen.lines().find(|line| line.contains('╭')).unwrap();
    let bottom = screen.lines().find(|line| line.contains('╰')).unwrap();
    assert!(top.contains(&format!(" {title} ")), "{screen}");
    if !count.is_empty() {
        assert!(top.contains(&format!(" {count} ")), "{screen}");
        assert!(!bottom.contains(count), "{screen}");
    }
    assert!(
        bottom
            .split('╯')
            .next()
            .unwrap()
            .trim_end()
            .ends_with(escape),
        "{screen}"
    );
    for old in ["Ctrl+", "⏎", "esc"] {
        assert!(!screen.contains(old), "{old}: {screen}");
    }
}

#[test]
fn settings_uses_the_shared_frame_and_never_nerd_config() {
    let mut app = test_app();
    app.settings.show_stats = true;
    app.core.verbosity = "normal".into();
    app.open_settings();
    let screen = render_to_string(110, 30, |f| super::settings::render(f, &app));
    let count = format!("{} settings", nexus_core::app::SettingsField::ALL.len());
    assert_frame(&screen, "Settings", &count, "Esc close");
    assert!(!screen.contains("nerd config"), "{screen}");
    assert!(screen.contains("● on"), "{screen}");
    assert!(screen.contains("‹ normal ›"), "{screen}");
}

#[test]
fn login_uses_the_shared_frame_and_j_k_move_the_selection() {
    let mut app = test_app();
    app.open_login_popup();
    let screen = render_to_string(110, 30, |f| super::login::render(f, &app));
    assert_frame(&screen, "Login", "1 of 6 set", "Esc close");
    super::login::handle_key(&mut app, KeyCode::Char('j').into());
    assert_eq!(app.login_selected, 1);
    super::login::handle_key(&mut app, KeyCode::Char('k').into());
    assert_eq!(app.login_selected, 0);
    super::login::handle_key(&mut app, KeyCode::End.into());
    assert_eq!(app.login_selected, 5);
    super::login::handle_key(&mut app, KeyCode::PageUp.into());
    assert_eq!(app.login_selected, 0);
    super::login::handle_key(&mut app, KeyCode::PageDown.into());
    assert_eq!(app.login_selected, 5);
    super::login::handle_key(&mut app, KeyCode::Home.into());
    assert_eq!(app.login_selected, 0);
}

#[test]
fn local_models_uses_the_shared_frame_and_names_runtime_statuses() {
    use nexus_core::provider::local::LocalRuntime;
    use nexus_core::provider::serve::RuntimeStatus;
    let mut app = test_app();
    app.popup = Popup::Local;
    app.local.status = vec![RuntimeStatus {
        runtime: LocalRuntime::Ollama,
        endpoint: "http://localhost:11434/v1".into(),
        port: Some(11434),
        running: true,
        managed: false,
        rss_kb: None,
        over_budget: false,
        budget_kb: None,
    }];
    let screen = render_to_string(110, 30, |f| super::local::render(f, &app));
    assert_frame(&screen, "Local models", "1 running", "Esc close");
    assert!(screen.contains("● running"), "{screen}");
    assert!(screen.contains("○ stopped"), "{screen}");
    super::local::handle_key(&mut app, KeyCode::Char('j').into());
    assert_eq!(app.local_selected, 1);
    super::local::handle_key(&mut app, KeyCode::Char('k').into());
    assert_eq!(app.local_selected, 0);
    app.local.status.clear();
    app.local_from_login = true;
    let screen = render_to_string(110, 30, |f| super::local::render(f, &app));
    assert_frame(&screen, "Local models", "", "Esc back");
    assert!(
        !screen
            .lines()
            .find(|line| line.contains('╭'))
            .unwrap()
            .contains("running")
    );
}

#[test]
fn copy_uses_the_shared_frame_and_the_new_empty_sentence() {
    let mut app = test_app();
    app.copy_options = vec![nexus_core::app::CopyOption {
        label: "Copy message".into(),
        text: "message".into(),
    }];
    let screen = render_to_string(110, 30, |f| super::copy::render(f, &app));
    assert_frame(&screen, "Copy", "1 item", "Esc close");
    assert!(screen.contains("↑↓ · Enter copy · Esc close"), "{screen}");
    app.copy_options.clear();
    let screen = render_to_string(110, 30, |f| super::copy::render(f, &app));
    assert_frame(&screen, "Copy", "0 items", "Esc close");
    assert!(screen.contains("Nothing to copy here yet."), "{screen}");
    assert!(!screen.contains("Enter copy"), "{screen}");
}

#[test]
fn connectors_uses_the_shared_frame_and_requires_ctrl_d_twice() {
    use super::connectors::Mode;
    use nexus_core::connectors::{ConnectorConfig, Status};
    let mut app = test_app();
    app.connectors
        .add("notes", ConnectorConfig::local(vec!["notes".into()]))
        .unwrap();
    app.connectors
        .force_state("notes", Status::Connected { tools: 1 }, Vec::new());
    super::connectors::open(&mut app);
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Connectors", "1 connector · 1 tool", "Esc close");
    assert!(screen.contains("Enter tools · a add"), "{screen}");
    assert!(screen.contains("^D remove · Esc close"), "{screen}");
    super::connectors::handle_key(&mut app, KeyCode::Char('d').into());
    assert_eq!(app.connectors_popup.mode, Mode::List);
    let remove = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    super::connectors::handle_key(&mut app, remove);
    assert_eq!(app.connectors_popup.mode, Mode::ConfirmRemove);
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Remove connector?", "", "Esc cancel");
    assert!(
        screen.contains("“notes” and its keychain entries will be removed."),
        "{screen}"
    );
    assert!(screen.contains("^D remove · Esc cancel"), "{screen}");
    super::connectors::handle_key(&mut app, KeyCode::Char('y').into());
    assert_eq!(app.connectors_popup.mode, Mode::ConfirmRemove);
    assert_eq!(app.connectors.infos().len(), 1);
    super::connectors::handle_key(&mut app, KeyCode::Esc.into());
    assert_eq!(app.connectors_popup.mode, Mode::List);
    super::connectors::handle_key(&mut app, remove);
    super::connectors::handle_key(&mut app, remove);
    assert!(app.connectors.infos().is_empty());
    assert_eq!(app.connectors_popup.mode, Mode::List);
}

#[test]
fn swarm_uses_the_shared_frame_and_plain_n_starts_a_persona() {
    use nexus_core::app::{PendingEditor, SwarmPopupMode};
    let mut app = test_app();
    app.popup = Popup::Swarm;
    app.swarm.cache.push(nexus_core::db::Persona {
        name: "Reviewer".into(),
        model: "test-model".into(),
        blurb: "Reviews work".into(),
    });
    let screen = render_to_string(110, 30, |f| super::swarm::render(f, &app));
    assert_frame(&screen, "Swarm", "off · 1 persona", "Esc close");
    assert!(screen.contains("Enter edit · n add"), "{screen}");
    let remove = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    super::swarm::handle_key(&mut app, remove).unwrap();
    let screen = render_to_string(110, 30, |f| super::swarm::render(f, &app));
    assert_frame(&screen, "Remove persona?", "", "Esc cancel");
    assert!(
        screen.contains("“Reviewer” will be removed from the swarm."),
        "{screen}"
    );
    super::swarm::handle_key(&mut app, KeyCode::Char('y').into()).unwrap();
    assert!(app.swarm_popup_mode == SwarmPopupMode::ConfirmDelete);
    super::swarm::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    app.swarm.cache.clear();
    let screen = render_to_string(110, 30, |f| super::swarm::render(f, &app));
    assert_frame(&screen, "Swarm", "off · 0 personas", "Esc close");
    assert!(
        screen.contains(
            "No personas yet. n adds one, or send a message and three will be suggested."
        ),
        "{screen}"
    );
    super::swarm::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert!(app.pending_editor.is_none());
    assert!(app.swarm.cache.is_empty());
    super::swarm::handle_key(&mut app, KeyCode::Char('n').into()).unwrap();
    assert_eq!(app.swarm.cache.len(), 1);
    assert_eq!(app.swarm_selected, 0);
    let Some(PendingEditor::Persona(path)) = app.pending_editor.take() else {
        panic!("n must queue the persona editor");
    };
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .starts_with("name: \n")
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn settings_numeric_drafts_have_a_field_and_save_or_cancel_without_closing() {
    use nexus_core::app::{SettingsField, SettingsRow};
    let mut app = test_app();
    app.open_settings();
    app.settings_selected = app
        .settings_rows()
        .iter()
        .position(|row| *row == SettingsRow::Field(SettingsField::Temperature))
        .unwrap();
    super::settings::handle_key(&mut app, KeyCode::Char('1').into()).unwrap();
    let screen = render_to_string(110, 30, |f| super::settings::render(f, &app));
    assert_frame(&screen, "Settings", "18 settings", "Esc cancel");
    assert!(screen.contains("✎ 1▏"), "{screen}");
    assert!(screen.contains("Enter save · Esc cancel"), "{screen}");
    let selected = app.settings_selected;
    super::settings::handle_key(&mut app, KeyCode::Char('j').into()).unwrap();
    assert_eq!(app.settings_selected, selected);
    assert_eq!(app.settings_inputs[0], "1");
    super::settings::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.settings_inputs[0], "");
    assert_eq!(app.popup, Popup::Settings);
    super::settings::handle_key(&mut app, KeyCode::Char('1').into()).unwrap();
    super::settings::handle_key(&mut app, KeyCode::Enter.into()).unwrap();
    assert_eq!(app.settings.temperature, Some(1.0));
    assert_eq!(app.popup, Popup::Settings);
    super::settings::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.popup, Popup::None);
}

#[test]
fn connector_wizard_uses_step_counts_fields_and_contextual_actions() {
    use super::connectors::{AddStep, Mode};
    let mut app = test_app();
    super::connectors::open(&mut app);
    super::connectors::handle_key(&mut app, KeyCode::Char('a').into());
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Add connector", "step 1 of 2", "Esc cancel");
    super::connectors::paste(&mut app, "https://notes.example/mcp");
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Add connector", "step 1 of 3", "Esc cancel");
    assert!(screen.contains("✎ https://notes.example/mcp▏"), "{screen}");
    super::connectors::handle_key(&mut app, KeyCode::Enter.into());
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Add connector", "step 2 of 3", "Esc back");
    super::connectors::handle_key(&mut app, KeyCode::Char('j').into());
    assert_eq!(app.connectors_popup.add.name, "notesj");
    super::connectors::handle_key(&mut app, KeyCode::Enter.into());
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Add connector", "step 3 of 3", "Esc back");
    assert!(
        screen.contains("Enter save · Tab switch · Esc back"),
        "{screen}"
    );
    super::connectors::handle_key(&mut app, KeyCode::Tab.into());
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Add connector", "step 3 of 4", "Esc back");
    super::connectors::handle_key(&mut app, KeyCode::Enter.into());
    assert_eq!(app.connectors_popup.add.step, AddStep::Token);
    super::connectors::paste(&mut app, "fake-secret");
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Add connector", "step 4 of 4", "Esc back");
    assert!(screen.contains("✎ ***********▏"), "{screen}");
    assert!(!screen.contains("fake-secret"), "{screen}");
    for _ in 0..4 {
        super::connectors::handle_key(&mut app, KeyCode::Esc.into());
    }
    assert_eq!(app.connectors_popup.mode, Mode::List);
}

#[test]
fn connector_log_and_import_use_the_shared_frame_and_navigation() {
    use super::connectors::Mode;
    let mut app = test_app();
    app.connectors
        .add(
            "notes",
            nexus_core::connectors::ConnectorConfig::local(vec!["notes".into()]),
        )
        .unwrap();
    app.connectors_popup.mode = Mode::Log;
    app.connectors_popup.log = "First line\nSecond line\nLast line".into();
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "notes log", "", "Esc back");
    assert!(screen.contains("↑↓ scroll · Esc back"), "{screen}");
    super::connectors::handle_key(&mut app, KeyCode::Char('j').into());
    assert_eq!(app.connectors_popup.log_scroll, 1);
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert!(!screen.contains("First line"), "{screen}");
    assert!(screen.contains("Second line"), "{screen}");
    super::connectors::handle_key(&mut app, KeyCode::Char('k').into());
    assert_eq!(app.connectors_popup.log_scroll, 0);
    super::connectors::handle_key(&mut app, KeyCode::End.into());
    assert_eq!(app.connectors_popup.log_scroll, 2);
    super::connectors::handle_key(&mut app, KeyCode::Home.into());
    assert_eq!(app.connectors_popup.log_scroll, 0);
    super::connectors::handle_key(&mut app, KeyCode::Esc.into());
    assert_eq!(app.connectors_popup.mode, Mode::List);
    app.connectors_popup.mode = Mode::Import;
    let screen = render_to_string(110, 30, |f| super::connectors::render(f, &app));
    assert_frame(&screen, "Import connectors", "0 selected", "Esc back");
    assert!(
        screen.contains("Space toggle · a all · Enter import · Esc back"),
        "{screen}"
    );
}
