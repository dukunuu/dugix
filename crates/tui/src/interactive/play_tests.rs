use super::*;
use dugix_core::app::App;
use dugix_core::db::Db;
use dugix_core::space::Space;

const PAD: &str = r#"root = Panel("Pad", [Text("x = " + $x), Input($name, "Name")])
$x = 5
$name = ""
keys = @Keys({left: @Set($x, $x - 1), right: @Set($x, $x + 1)})"#;
const CLOCK: &str = r#"root = Panel("Clock", [Text("Ticks: " + $n)])
$n = 0
tick = @Every(0.5, @Set($n, $n + 1))"#;
const TARGET: &str = r#"root = Panel("Target", [Canvas([Rect(0, 0, 100, 100, "muted"), Circle(50, 50, 30, "warning", true, null, @Set($hits, $hits + 1))], 6)])
$hits = 0"#;

fn value(widgets: &Widgets, name: &str) -> String {
    widgets
        .entries
        .values()
        .next()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
        .value(name)
}
fn press(widgets: &mut Widgets, code: KeyCode) {
    widgets.key(KeyEvent::new(code, KeyModifiers::NONE), true);
}

#[test]
fn bound_keys_drive_the_focused_panel_but_never_typing() {
    let mut widgets = Widgets::default();
    let _ = tests::draw(&mut widgets, &[tests::placement(PAD, 1)], 0);
    assert!(widgets.focus_latest());
    press(&mut widgets, KeyCode::Right);
    assert_eq!(value(&widgets, "x"), "5");
    press(&mut widgets, KeyCode::Tab);
    press(&mut widgets, KeyCode::Right);
    press(&mut widgets, KeyCode::Right);
    press(&mut widgets, KeyCode::Left);
    assert_eq!(value(&widgets, "x"), "6");
}

#[test]
fn timers_tick_on_wall_time_only_while_live() {
    let mut entry = Entry::new(Ok(Session::new(Document::parse(CLOCK).unwrap()).unwrap()));
    let back = |entry: &mut Entry, seconds: f64| {
        entry.born -= std::time::Duration::from_secs_f64(seconds);
    };
    assert!(entry.advance_clock(false, true));
    back(&mut entry, 0.6);
    entry.advance_clock(false, true);
    back(&mut entry, 0.5);
    entry.advance_clock(false, true);
    assert_eq!(entry.session.as_ref().unwrap().value("n"), "2");
    back(&mut entry, 5.0);
    assert!(!entry.advance_clock(false, false));
    assert_eq!(entry.session.as_ref().unwrap().value("n"), "2");
    let mut widgets = Widgets::default();
    assert!(widgets.timers_live());
    widgets.window_focus(false);
    assert!(!widgets.timers_live());
}

#[test]
fn clicking_a_clickable_shape_runs_its_action() {
    let mut widgets = Widgets::default();
    let _ = tests::draw(&mut widgets, &[tests::placement(TARGET, 1)], 0);
    let entry = widgets.entries.values().next().unwrap();
    assert!(matches!(entry.controls()[..], [Control::Shape(_, 1)]));
    let target = widgets
        .hits
        .iter()
        .find(|hit| hit.control == Some(0))
        .unwrap()
        .rect;
    assert!(target.width < 40 && target.height < 6, "{target:?}");
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: target.x + target.width / 2,
        row: target.y + target.height / 2,
        modifiers: KeyModifiers::NONE,
    };
    assert!(widgets.click(click, Rect::new(0, 0, 80, 32), true).0);
    assert_eq!(value(&widgets, "hits"), "1");
}

#[test]
fn the_loop_saves_panel_state_at_most_once_a_second() {
    let root = std::env::temp_dir().join(format!("dugix-play-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("spaces")).unwrap();
    let mut app = AppView::new(App::new(
        Db::open_in_memory().unwrap(),
        Some("k"),
        Space { root },
    ));
    let session = app
        .core
        .db
        .create_session("chat", "model", &app.core.active_space.id, "chat")
        .unwrap();
    let id = session.id.clone();
    app.core.session = Some(session);
    let placement = tests::placement(CLOCK, 0);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 32)).unwrap();
    let mut frame = |app: &mut AppView| {
        terminal
            .draw(|f| {
                crate::interactive::sync_saved(app);
                app.interactive.begin_frame(true);
                app.interactive.paint(
                    f,
                    f.area(),
                    0,
                    std::slice::from_ref(&placement),
                    &Theme::default(),
                    true,
                );
            })
            .unwrap();
    };
    frame(&mut app);
    let run_timer = |app: &mut AppView| {
        let entry = app.interactive.entries.values_mut().next().unwrap();
        assert!(entry.session.as_mut().unwrap().tick(0.6).unwrap());
    };
    run_timer(&mut app);
    flush_settled(&mut app);
    let saved = |app: &AppView| app.core.panel_states(&id).unwrap().len();
    assert_eq!(saved(&app), 1);
    let entry = app.interactive.entries.values_mut().next().unwrap();
    assert!(entry.session.as_mut().unwrap().tick(1.2).unwrap());
    let before = app.core.panel_states(&id).unwrap();
    flush_settled(&mut app);
    assert_eq!(app.core.panel_states(&id).unwrap(), before);
    flush(&mut app);
    assert_ne!(app.core.panel_states(&id).unwrap(), before);
}
