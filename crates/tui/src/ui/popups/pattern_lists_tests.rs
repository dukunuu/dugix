//! The shared popup pattern, as Spaces, Skills, Apps, Files, Library and Models follow it.
#![allow(unused_imports)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::Popup;

use super::tests::{render_to_string, test_app};

fn assert_frame(screen: &str, title: &str, count: &str) {
    let top = screen.lines().find(|line| line.contains('╭')).unwrap();
    let bottom = screen.lines().find(|line| line.contains('╰')).unwrap();
    assert!(top.contains(&format!("╭ {title} ")), "{screen}");
    assert!(top.contains(&format!(" {count} ╮")), "{screen}");
    assert!(!bottom.contains(count), "{screen}");
    assert!(bottom.trim_end().ends_with("Esc close ╯"), "{screen}");
    assert_key_spelling(screen);
}

fn assert_key_spelling(screen: &str) {
    assert!(!screen.contains("Ctrl+"), "{screen}");
    assert!(!screen.contains("esc"), "{screen}");
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn library_app() -> crate::app_view::AppView {
    let mut app = test_app();
    let files = app.space.files_dir(&app.active_space.name);
    std::fs::create_dir_all(files.join("decks")).unwrap();
    for (name, title) in [("plan", "Quarterly Plan"), ("launch", "Launch Notes")] {
        std::fs::write(
            files.join(format!("decks/{name}.deck.json")),
            serde_json::to_vec(&nexus_core::deck::Deck::titled(title)).unwrap(),
        )
        .unwrap();
    }
    super::library::open(&mut app);
    app
}

#[test]
fn spaces_follow_the_list_pattern() {
    let mut app = test_app();
    app.spaces_cache = vec![app.db.create_space("Work").unwrap()];
    app.popup = Popup::Space;
    let screen = render_to_string(110, 30, |f| super::space::render(f, &app));
    assert_frame(&screen, "Spaces", "1 space");
    assert!(screen.contains("⌕ Type to filter"), "{screen}");
    assert!(screen.contains("▸   Work"), "{screen}");
    for (mode, title) in [
        (nexus_core::app::SpaceMode::Create, "New space"),
        (nexus_core::app::SpaceMode::Rename, "Rename space"),
    ] {
        app.space_mode = mode;
        app.space_edit = "Project".into();
        let screen = render_to_string(110, 30, |f| super::space::render(f, &app));
        assert!(screen.contains(&format!("╭ {title} ")), "{screen}");
        assert!(screen.contains("✎ Project▏"), "{screen}");
        assert_key_spelling(&screen);
    }
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn skills_follow_the_list_pattern() {
    let mut app = test_app();
    app.core.skills = vec![nexus_core::skills::Skill {
        name: "plan".into(),
        description: "Plan a project.".into(),
        dir: app.space.root.join("skills/plan"),
    }];
    let screen = render_to_string(110, 30, |f| super::skills::render(f, &app));
    assert_frame(&screen, "Skills", "1 skill");
    assert!(screen.contains("Plan a project."), "{screen}");
    assert!(screen.contains("⌕ Type to filter"), "{screen}");
    app.skills_mode = nexus_core::app::SkillsMode::Install;
    let screen = render_to_string(110, 30, |f| super::skills::render(f, &app));
    assert!(screen.contains("✎ owner/repo/path"), "{screen}");
    assert!(!screen.contains('▏'), "{screen}");
    app.skills_edit = "owner/repo/path".into();
    let screen = render_to_string(110, 30, |f| super::skills::render(f, &app));
    assert!(screen.contains("╭ Install skill "), "{screen}");
    assert!(screen.contains("✎ owner/repo/path▏"), "{screen}");
    assert!(screen.contains("Enter install · Esc cancel"), "{screen}");
    assert_key_spelling(&screen);
    app.skills_mode = nexus_core::app::SkillsMode::ConfirmRemove;
    let screen = render_to_string(110, 30, |f| super::skills::render(f, &app));
    assert!(screen.contains("╭ Remove skill? "), "{screen}");
    assert!(screen.contains("“plan” will be removed."), "{screen}");
    assert!(screen.contains("^D remove · Esc cancel"), "{screen}");
    assert!(!screen.contains("Plan a project."), "{screen}");
    assert_key_spelling(&screen);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn apps_follow_the_list_pattern() {
    let mut app = test_app();
    app.core.caches.apps = vec!["dashboard".into()];
    let screen = render_to_string(110, 30, |f| super::apps::render(f, &app));
    assert_frame(&screen, "Apps", "1 app");
    assert!(screen.contains("^E edit"), "{screen}");
    assert!(!screen.contains("^R rename"), "{screen}");
    for (mode, title, verb) in [
        (nexus_core::app::AppsMode::EditFile, "Edit app file", "open"),
        (
            nexus_core::app::AppsMode::RenamePanel,
            "Rename panel",
            "save",
        ),
    ] {
        app.apps_mode = mode;
        app.apps_edit = "index.html".into();
        let screen = render_to_string(110, 30, |f| super::apps::render(f, &app));
        assert!(screen.contains(&format!("╭ {title} ")), "{screen}");
        assert!(screen.contains("✎ index.html▏"), "{screen}");
        assert!(
            screen.contains(&format!("Enter {verb} · Esc cancel")),
            "{screen}"
        );
        assert_key_spelling(&screen);
    }
    app.apps_mode = nexus_core::app::AppsMode::ConfirmDelete;
    let screen = render_to_string(110, 30, |f| super::apps::render(f, &app));
    assert!(screen.contains("╭ Delete app? "), "{screen}");
    assert!(
        screen.contains("App “dashboard” and all its files will be removed."),
        "{screen}"
    );
    assert!(screen.contains("^D delete · Esc cancel"), "{screen}");
    assert_key_spelling(&screen);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn files_follow_the_list_pattern() {
    let mut app = test_app();
    app.core.caches.files = vec![nexus_core::db::FileRow {
        id: "file".into(),
        name: "report.txt".into(),
        hash: String::new(),
        size: 100,
        status: "ok".into(),
        mtime: 0,
        extractor: 0,
    }];
    let screen = render_to_string(110, 30, |f| super::files::render(f, &app));
    assert_frame(&screen, "Files", "1 file");
    assert!(screen.contains("Files   Images   Scripts"), "{screen}");
    assert!(screen.contains("report.txt"), "{screen}");
    for (mode, title, verb) in [
        (nexus_core::app::FilesMode::Add, "Import file", "import"),
        (nexus_core::app::FilesMode::Rename, "Rename file", "save"),
    ] {
        app.files_mode = mode;
        app.files_edit = "report.txt".into();
        let screen = render_to_string(110, 30, |f| super::files::render(f, &app));
        assert!(screen.contains(&format!("╭ {title} ")), "{screen}");
        assert!(screen.contains("✎ report.txt▏"), "{screen}");
        assert!(
            screen.contains(&format!("Enter {verb} · Esc cancel")),
            "{screen}"
        );
        assert_key_spelling(&screen);
    }
    app.files_mode = nexus_core::app::FilesMode::ConfirmDelete;
    let screen = render_to_string(110, 30, |f| super::files::render(f, &app));
    assert!(screen.contains("╭ Delete file? "), "{screen}");
    assert!(screen.contains("“report.txt” will be removed."), "{screen}");
    assert!(screen.contains("^D delete · Esc cancel"), "{screen}");
    assert_key_spelling(&screen);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn library_follows_the_list_pattern() {
    let app = library_app();
    let screen = render_to_string(110, 30, |f| super::library::render(f, &app));
    assert_frame(&screen, "Library", "2 documents");
    assert!(screen.contains("⌕ Type to filter"), "{screen}");
    assert!(screen.contains("Quarterly Plan"), "{screen}");
    assert!(screen.contains("Launch Notes"), "{screen}");
    assert!(!screen.contains("^N"), "{screen}");
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn models_follow_the_list_pattern_with_one_frame() {
    let mut app = test_app();
    app.core.models = ["a/favorite", "b/available"]
        .into_iter()
        .map(|id| nexus_core::provider::Model {
            id: id.into(),
            name: id.into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: nexus_core::provider::BackendTag::OpenRouter,
            pricing: None,
            aliases: Vec::new(),
        })
        .collect();
    app.core.favorites.insert("a/favorite".into());
    app.popup = Popup::Model;
    let screen = render_to_string(110, 30, |f| super::model::render(f, &mut app));
    assert_frame(&screen, "Models", "all backends · chat model");
    assert_eq!(screen.matches('╭').count(), 1, "{screen}");
    assert!(screen.contains("▍ Favorites"), "{screen}");
    assert!(screen.contains("▍ Available · 1 of 1"), "{screen}");
    assert!(screen.contains("⌕ Type to filter"), "{screen}");
    assert!(screen.contains("b/available"), "{screen}");
    super::model::handle_key(&mut app, KeyCode::Char('z').into()).unwrap();
    let screen = render_to_string(110, 30, |f| super::model::render(f, &mut app));
    assert!(screen.contains("Esc clear"), "{screen}");
    assert!(screen.contains("▍ Available · 0 of 1"), "{screen}");
    super::model::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert!(app.model_filter.is_empty());
    assert_eq!(app.popup, Popup::Model);
    super::model::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.popup, Popup::None);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn library_filters_typed_text_and_deletes_only_with_ctrl_d_twice() {
    let mut app = library_app();
    for c in "pLaN".chars() {
        super::library::handle_key(&mut app, KeyCode::Char(c).into());
    }
    assert_eq!(app.library.filter, "pLaN");
    assert_eq!(app.library.selected, 0);
    let screen = render_to_string(110, 30, |f| super::library::render(f, &app));
    let top = screen.lines().find(|line| line.contains('╭')).unwrap();
    assert!(top.contains("1 of 2"), "{screen}");
    assert!(screen.contains("Quarterly Plan"), "{screen}");
    assert!(!screen.contains("Launch Notes"), "{screen}");
    assert!(screen.contains("⌕ pLaN▏"), "{screen}");
    assert!(screen.contains("Esc clear"), "{screen}");
    assert_eq!(app.list_hit.get().unwrap().total, 1);
    let path = app
        .space
        .files_dir(&app.active_space.name)
        .join("decks/plan.deck.json");
    super::library::handle_key(&mut app, ctrl('d'));
    assert_eq!(app.library.mode, super::library::Mode::ConfirmDelete);
    let screen = render_to_string(110, 30, |f| super::library::render(f, &app));
    assert!(screen.contains("╭ Delete document? "), "{screen}");
    assert!(
        screen.contains("“decks/plan.deck.json” will be moved to the trash folder."),
        "{screen}"
    );
    assert!(screen.contains("^D delete · Esc cancel"), "{screen}");
    for c in ['y', 'Y', 'n'] {
        super::library::handle_key(&mut app, KeyCode::Char(c).into());
        assert!(path.is_file());
        assert_eq!(app.library.mode, super::library::Mode::ConfirmDelete);
    }
    super::library::handle_key(&mut app, ctrl('d'));
    assert!(!path.exists());
    assert_eq!(app.library.entries.len(), 1);
    assert_eq!(app.library.entries[0].title, "Launch Notes");
    assert_eq!(app.library.selected, 0);
    assert_eq!(app.library.mode, super::library::Mode::Browse);
    super::library::handle_key(&mut app, KeyCode::Esc.into());
    assert!(app.library.filter.is_empty());
    assert_eq!(app.popup, Popup::Library);
    super::library::handle_key(&mut app, KeyCode::Char('j').into());
    assert_eq!(app.library.filter, "j");
    super::library::open(&mut app);
    assert!(app.library.filter.is_empty());
    super::library::handle_key(&mut app, KeyCode::Esc.into());
    assert_eq!(app.popup, Popup::None);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn spaces_confirm_delete_uses_the_question_and_filter_escape_clears_first() {
    let mut app = test_app();
    app.spaces_cache = vec![app.db.create_space("Work").unwrap()];
    app.popup = Popup::Space;
    super::space::handle_key(&mut app, ctrl('d')).unwrap();
    assert!(app.space_mode == nexus_core::app::SpaceMode::ConfirmDelete);
    let screen = render_to_string(110, 30, |f| super::space::render(f, &app));
    assert!(screen.contains("╭ Delete space? "), "{screen}");
    assert!(screen.contains("^D delete · Esc cancel"), "{screen}");
    assert!(
        screen.contains("“Work” will be deleted. Its chats move to the default space."),
        "{screen}"
    );
    assert_key_spelling(&screen);
    super::space::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    super::space::handle_key(&mut app, KeyCode::Char('w').into()).unwrap();
    let screen = render_to_string(110, 30, |f| super::space::render(f, &app));
    assert!(screen.contains("1 of 1"), "{screen}");
    assert!(screen.contains("Esc clear"), "{screen}");
    super::space::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert!(app.space_filter.is_empty());
    assert_eq!(app.popup, Popup::Space);
    super::space::handle_key(&mut app, KeyCode::Esc.into()).unwrap();
    assert_eq!(app.popup, Popup::None);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

fn press(app: &mut crate::app_view::AppView, popup: &Popup, key: KeyEvent) {
    match popup {
        Popup::Skills => super::skills::handle_key(app, key),
        Popup::Apps => super::apps::handle_key(app, key).unwrap(),
        _ => super::files::handle_key(app, key).unwrap(),
    }
}

fn type_text(app: &mut crate::app_view::AppView, popup: &Popup, text: &str) {
    for c in text.chars() {
        press(app, popup, KeyCode::Char(c).into());
    }
}

fn bottom(screen: &str) -> &str {
    screen.lines().find(|line| line.contains('╰')).unwrap()
}

#[test]
fn skills_filter_as_you_type_and_esc_clears_before_closing() {
    let mut app = test_app();
    app.core.skills = ["plan", "pdf", "deck"]
        .into_iter()
        .map(|name| nexus_core::skills::Skill {
            name: name.into(),
            description: format!("About {name}."),
            dir: app.space.root.join("skills").join(name),
        })
        .collect();
    app.open_skills_popup();
    type_text(&mut app, &Popup::Skills, "pd");
    assert_eq!(
        app.skills_selected, 1,
        "the selection lands on the first match"
    );
    let screen = render_to_string(110, 30, |f| super::skills::render(f, &app));
    assert!(screen.contains(" 1 of 3 ╮"), "{screen}");
    assert!(screen.contains("⌕ pd"), "{screen}");
    assert!(screen.contains("About pdf."), "{screen}");
    assert!(!screen.contains("deck"), "{screen}");
    assert!(
        bottom(&screen).trim_end().ends_with("Esc clear ╯"),
        "{screen}"
    );

    // j is text here, and with nothing matching no action fires.
    type_text(&mut app, &Popup::Skills, "j");
    let screen = render_to_string(110, 30, |f| super::skills::render(f, &app));
    assert!(
        screen.contains("No skill matches “pdj”. Esc clears the filter."),
        "{screen}"
    );
    press(&mut app, &Popup::Skills, ctrl('d'));
    assert!(app.skills_mode == nexus_core::app::SkillsMode::Browse);
    press(&mut app, &Popup::Skills, KeyCode::Enter.into());
    assert_eq!(app.popup, Popup::Skills);

    press(&mut app, &Popup::Skills, KeyCode::Backspace.into());
    assert_eq!(app.skills_filter, "pd");
    press(&mut app, &Popup::Skills, KeyCode::Esc.into());
    assert!(app.skills_filter.is_empty());
    assert_eq!(app.popup, Popup::Skills);
    assert_eq!(app.skills_selected, 1, "clearing keeps the selection");
    press(&mut app, &Popup::Skills, KeyCode::Esc.into());
    assert_eq!(app.popup, Popup::None);
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn apps_filter_moves_only_among_matches() {
    let mut app = test_app();
    app.core.caches.apps = vec!["dashboard".into(), "notes".into(), "notebook".into()];
    app.popup = Popup::Apps;
    type_text(&mut app, &Popup::Apps, "note");
    assert_eq!(app.apps_selected, 1);
    press(&mut app, &Popup::Apps, KeyCode::Down.into());
    assert_eq!(app.apps_selected, 2);
    press(&mut app, &Popup::Apps, KeyCode::Down.into());
    assert_eq!(app.apps_selected, 2, "stops at the last match");
    press(&mut app, &Popup::Apps, KeyCode::Home.into());
    assert_eq!(app.apps_selected, 1, "Home is the first match, not row 0");
    let screen = render_to_string(110, 30, |f| super::apps::render(f, &app));
    assert!(screen.contains(" 2 of 3 ╮"), "{screen}");
    assert!(!screen.contains("dashboard"), "{screen}");
    press(&mut app, &Popup::Apps, ctrl('d'));
    assert!(app.apps_mode == nexus_core::app::AppsMode::ConfirmDelete);
    let screen = render_to_string(110, 30, |f| super::apps::render(f, &app));
    assert!(
        screen.contains("App “notes”"),
        "deletes what is highlighted: {screen}"
    );
    std::fs::remove_dir_all(&app.space.root).unwrap();
}

#[test]
fn files_filter_after_slash_finds_and_letters_filter() {
    let mut app = test_app();
    app.core.caches.files = ["report.txt", "notes/plan.md", "data.csv"]
        .into_iter()
        .map(|name| nexus_core::db::FileRow {
            id: name.into(),
            name: name.into(),
            hash: String::new(),
            size: 100,
            status: "ok".into(),
            mtime: 0,
            extractor: 0,
        })
        .collect();
    app.popup = Popup::Files;
    let screen = render_to_string(110, 30, |f| super::files::render(f, &app));
    let tabs = screen
        .lines()
        .position(|l| l.contains("Files   Images"))
        .unwrap();
    let field = screen
        .lines()
        .position(|l| l.contains("⌕ Type to filter"))
        .unwrap();
    assert!(
        tabs < field,
        "the tab strip sits above the field row: {screen}"
    );
    press(&mut app, &Popup::Files, KeyCode::Char('/').into());
    assert!(app.files_mode == nexus_core::app::FilesMode::Pick);
    press(&mut app, &Popup::Files, KeyCode::Esc.into());
    assert!(app.files_mode == nexus_core::app::FilesMode::Browse);

    type_text(&mut app, &Popup::Files, "notes/");
    assert_eq!(app.files_filter, "notes/", "once filtering, / is text");
    assert_eq!(app.files_selected, 1);
    let screen = render_to_string(110, 30, |f| super::files::render(f, &app));
    assert!(screen.contains(" 1 of 3 ╮"), "{screen}");
    assert!(!screen.contains("report.txt"), "{screen}");
    assert!(!bottom(&screen).contains("/ find"), "{screen}");
    press(&mut app, &Popup::Files, KeyCode::Esc.into());
    press(&mut app, &Popup::Files, KeyCode::Char('j').into());
    assert_eq!(app.files_filter, "j", "j filters the Files list");
    std::fs::remove_dir_all(&app.space.root).unwrap();
}
