//! Real small tools from the model prompt, not decorative component demos.

use super::*;

const TITLES: [&str; 3] = ["Trip planner", "Split the bill", "Python quick check"];

fn source(title: &str) -> &'static str {
    nexus_core::interactive::INSTRUCTIONS
        .split("```nexus-ui\n")
        .skip(1)
        .map(|example| example.split("\n```").next().unwrap())
        .find(|example| example.contains(&format!("Panel(\"{title}\"")))
        .unwrap()
}

fn entry(title: &str) -> Entry {
    Entry::new(Ok(
        Session::new(Document::parse(source(title)).unwrap()).unwrap()
    ))
}

fn text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn reference_tools_have_narrow_and_wide_visual_snapshots() {
    let snapshots = TITLES
        .iter()
        .flat_map(|title| {
            [32, 72].map(|width| {
                let (canvas, _) = entry(title).canvas(width, &Theme::default(), false, true);
                format!("{title} / {width}\n{}", text(&canvas))
            })
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if std::env::var_os("NEXUS_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/interactive/reference_snapshots.txt"
            ),
            format!("{snapshots}\n"),
        )
        .unwrap();
        return;
    }
    assert_eq!(
        snapshots,
        include_str!("reference_snapshots.txt").trim_end()
    );
}

#[test]
fn reference_tools_stay_legible_and_clickable_across_sizes_and_themes() {
    let dark = Theme::default();
    let mut light = Theme::default();
    light.bg = Color::Rgb(250, 250, 250);
    light.surface = light.bg;
    light.fg = Color::Rgb(24, 24, 27);
    light.fg_dim = Color::Rgb(82, 82, 91);
    light.raised = Color::Rgb(244, 244, 245);
    light.control = Color::Rgb(228, 228, 231);
    light.accent = Color::Rgb(29, 78, 216);
    light.accent2 = Color::Rgb(109, 40, 217);
    let mut transparent = dark;
    transparent.set_background_mode(crate::theme::BackgroundMode::Transparent);
    for title in TITLES {
        let entry = entry(title);
        for width in [24, 32, 48, 72, 100] {
            for theme in [&dark, &light, &transparent] {
                for focused in [false, true] {
                    let (canvas, hits) = entry.canvas(width, theme, focused, true);
                    assert_eq!(canvas.area.height, source_height(source(title), width));
                    assert!(
                        hits.iter()
                            .all(|(rect, _)| rect.intersection(canvas.area) == *rect)
                    );
                    assert!(
                        canvas
                            .content
                            .iter()
                            .all(|cell| !cell.symbol().contains(['█', '▀', '▄']))
                    );
                    assert_eq!(
                        canvas[(3, 0)].bg,
                        theme.surface,
                        "header must not be a color slab"
                    );
                    assert_eq!(canvas[(3, 0)].fg, theme.fg);
                }
            }
        }
    }
}

#[test]
fn calculator_updates_its_result_locally_and_reset_restores_the_defaults() {
    let mut entry = entry("Split the bill");
    let render = |entry: &Entry| text(&entry.canvas(48, &Theme::default(), false, true).0);
    assert!(render(&entry).contains("$46"));
    assert!(entry.navigate(KeyCode::Right));
    assert_eq!(entry.session.as_ref().unwrap().value("bill"), "85");
    assert!(render(&entry).contains("$48.88"), "{}", render(&entry));
    entry.focus = 2;
    assert!(entry.navigate(KeyCode::Right));
    assert!(render(&entry).contains("$32.58"));
    assert!(
        entry.activate(3, true, true).is_none(),
        "reset never sends a message"
    );
    assert_eq!(entry.session.as_ref().unwrap().value("bill"), "80");
    assert!(render(&entry).contains("$46"));
}

#[test]
fn quiz_feedback_and_choices_do_not_move_the_following_transcript() {
    let mut entry = entry("Python quick check");
    let (before, _) = entry.canvas(48, &Theme::default(), false, true);
    assert!(!text(&before).contains("A hash table maps"));
    assert!(entry.activate(2, true, true).is_none());
    assert_eq!(entry.session.as_ref().unwrap().value("pick"), "Hash map");
    assert!(entry.activate(1, true, true).is_none());
    let (after, _) = entry.canvas(48, &Theme::default(), false, true);
    assert!(text(&after).contains("A hash table maps"));
    assert_eq!(before.area.height, after.area.height);
}
