use super::*;

const CHART: &str =
    r#"root = Panel("Trend", [Chart("line", [Series("Load", [1, 4, 2, 8])], ["Mon", "Thu"], 6)])"#;
const PROGRESS: &str = r#"root = Panel("Goal", [Progress("Done", $n, 10), Buttons([Button("Finish", @Set($n, 10))])])
$n = 2"#;

fn entry(source: &str) -> Entry {
    Entry::new(Ok(Session::new(Document::parse(source).unwrap()).unwrap()))
}
/// Braille cells drawn in a buffer: the chart's ink.
fn ink(buffer: &Buffer) -> usize {
    buffer
        .content
        .iter()
        .filter(|cell| {
            cell.symbol()
                .chars()
                .next()
                .is_some_and(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
        })
        .count()
}
/// Heavy track cells: a progress bar's filled part.
fn filled(buffer: &Buffer) -> usize {
    buffer
        .content
        .iter()
        .filter(|cell| cell.symbol() == glyph::BAR_FILL)
        .count()
}

#[test]
fn charts_draw_in_with_motion_and_draw_whole_without_it() {
    let still = entry(CHART);
    let whole = ink(&still.canvas(60, &Theme::default(), false, true).0);
    assert!(whole > 0);
    let mut moving = entry(CHART);
    let session = moving.session.as_mut().unwrap();
    session.set_motion(true);
    session.set_clock(0.0);
    assert_eq!(ink(&moving.canvas(60, &Theme::default(), false, true).0), 0);
    assert!(moving.session.as_ref().unwrap().animating());
    moving.session.as_mut().unwrap().set_clock(0.15);
    let partway = ink(&moving.canvas(60, &Theme::default(), false, true).0);
    assert!(partway > 0 && partway < whole, "{partway} of {whole}");
    moving.session.as_mut().unwrap().set_clock(1.0);
    assert_eq!(
        ink(&moving.canvas(60, &Theme::default(), false, true).0),
        whole
    );
    assert!(!moving.session.as_ref().unwrap().animating());
}

#[test]
fn bound_bars_glide_to_new_values() {
    let mut entry = entry(PROGRESS);
    let session = entry.session.as_mut().unwrap();
    session.set_motion(true);
    session.set_clock(1.0);
    let before = filled(&entry.canvas(60, &Theme::default(), false, true).0);
    entry.activate(0, true, true);
    // Same instant: the bar still shows where it was.
    assert_eq!(
        filled(&entry.canvas(60, &Theme::default(), false, true).0),
        before
    );
    entry.session.as_mut().unwrap().set_clock(1.1);
    let gliding = filled(&entry.canvas(60, &Theme::default(), false, true).0);
    entry.session.as_mut().unwrap().set_clock(1.5);
    let landed = filled(&entry.canvas(60, &Theme::default(), false, true).0);
    assert!(
        before < gliding && gliding < landed,
        "{before} {gliding} {landed}"
    );
}

#[test]
fn a_pressed_pill_flashes_only_with_motion_on() {
    let mut entry = entry(PROGRESS);
    entry.activate(0, true, true);
    let pill = |entry: &Entry| {
        let (buffer, hits) = entry.canvas(60, &Theme::default(), false, true);
        let rect = hits.iter().find(|(_, control)| *control == 0).unwrap().0;
        buffer[(rect.x + 2, rect.y)].modifier
    };
    assert!(!pill(&entry).contains(Modifier::REVERSED));
    entry.session.as_mut().unwrap().set_motion(true);
    assert!(pill(&entry).contains(Modifier::REVERSED));
    assert!(
        entry.advance_clock(true, false),
        "the flash keeps frames coming"
    );
}

#[test]
fn motion_off_freezes_the_clock_and_settings_survive_a_reset() {
    let mut widgets = Widgets::default();
    let placement = tests::placement(CHART, 1);
    let _ = tests::draw(&mut widgets, &[placement.clone()], 0);
    assert!(!widgets.animating(), "motion is off by default");
    widgets.motion = true;
    widgets.vim = true;
    widgets.clear();
    assert!(widgets.motion && widgets.vim);
    let _ = tests::draw(&mut widgets, &[placement], 0);
    assert!(widgets.animating(), "a fresh chart draws in");
}
