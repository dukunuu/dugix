use super::*;
use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};
use serde_json::json;
use std::collections::HashSet;

fn content() -> String {
    json!({
        "name":"batch",
        "arguments":json!({"calls":[
            {"tool":"search","arguments":{"mode":"web","query":"jev decision api"}},
            {"tool":"search","arguments":{"mode":"web","query":"luna vs jev"}},
            {"tool":"search","arguments":{"mode":"web","query":"pricing"}},
            {"tool":"fetch_url","arguments":{"url":"https://arxiv.org/abs/123"}},
            {"tool":"files","arguments":{"action":"read","name":"us_1.jpg"}},
            {"tool":"deck","arguments":{"action":"edit","path":"decks/a.deck.json"}}
        ]}).to_string(),
        "result":"[1/6] search web \"jev decision api\"\n[1] Decision API\n    https://example.org/api\n    A decision endpoint.\n[2/6] search web \"luna vs jev\"\n[1] Comparison\n    https://example.org/compare\n    The comparison.\n[3/6] search web \"pricing\"\n[1] Pricing\n    https://example.org/pricing\n    The prices.\n[4/6] fetch_url https://arxiv.org/abs/123\nPaper preview\n[5/6] files/read us_1.jpg\nimage ready\n[6/6] deck\ndeck failed: no block \"x\""
    }).to_string()
}

fn render(
    expanded: bool,
    open: &[usize],
) -> (Vec<Line<'static>>, Vec<crate::history_cache::BatchRow>) {
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    let mut images = Vec::new();
    let mut cache = HashMap::new();
    let folds: HashSet<_> = open.iter().map(|index| (0, *index)).collect();
    push_tool_call_with_rows(
        &mut lines,
        &content(),
        expanded,
        &dugix_core::app::Settings::default(),
        140,
        &crate::theme::Theme::default(),
        CardEnv {
            files_dir: std::path::Path::new("."),
            studio_linked: false,
            image_at_line: &mut images,
            image_cache: &mut cache,
        },
        Some(BatchRows {
            owner: 0,
            open: &folds,
            focus: None,
            hits: &mut hits,
        }),
    );
    (lines, hits)
}

fn snapshot(lines: &[Line<'static>]) -> Vec<String> {
    let mut terminal =
        Terminal::new(TestBackend::new(140, u16::try_from(lines.len()).unwrap())).unwrap();
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(lines.to_vec()), frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text: Vec<_> = (0..buffer.area.height)
        .map(|y| {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            let row = row.trim_end();
            if let Some(head) = row.strip_suffix('▸').or_else(|| row.strip_suffix('▾')) {
                format!("{} {}", head.trim_end(), row.chars().last().unwrap())
            } else {
                row.to_string()
            }
        })
        .collect();
    while text.last().is_some_and(String::is_empty) {
        text.pop();
    }
    text
}

fn header() -> &'static str {
    "✗ batch · 6 calls · 5 ok · 1 failed"
}

fn opened() -> Vec<String> {
    [
        format!("{} ▾", header()),
        "▎  1  ◎ search \"jev decision api\" · 1 result · ok ▸".into(),
        "▎  2  ◎ search \"luna vs jev\" · 1 result · ok ▸".into(),
        "▎  3  ◎ search \"pricing\" · 1 result · ok ▸".into(),
        "▎  4  ◎ fetch arxiv.org/abs/123 · 13 chars · ok ▸".into(),
        "▎  5  ⚒ files/read us_1.jpg → image ready · ok ▸".into(),
        "▎  6  ✗ deck · edit decks/a.deck.json · failed · failed: deck failed: no block \"x\" ▸"
            .into(),
    ]
    .into()
}

#[test]
fn collapsed_batch_snapshot_is_a_digest_not_concatenated_results() {
    let (lines, hits) = render(false, &[]);
    assert_eq!(snapshot(&lines), [
        format!("{} ▸", header()),
        "   search ×3 \"jev decision api\", \"luna vs jev\" … · fetch ×1 arxiv.org/abs/123 · files ×1 read us_1.jpg · deck ×1 decks/a.deck.json".into(),
        "   6 deck: deck failed: no block \"x\"".into(),
    ]);
    assert!(hits.is_empty());
    let theme = crate::theme::Theme::default();
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content.contains("1 failed") && span.style.fg == Some(theme.error))
    );
    assert!(
        lines[2]
            .spans
            .iter()
            .all(|span| span.style.fg == Some(theme.error))
    );
}

#[test]
fn opened_batch_snapshot_is_one_numbered_row_per_call() {
    let (lines, hits) = render(true, &[]);
    assert_eq!(snapshot(&lines), opened());
    assert_eq!(hits.len(), 6);
    assert!(hits.iter().all(|hit| hit.lines.len() == 1));
}

#[test]
fn expanded_search_row_snapshot_uses_the_search_results_body() {
    let (lines, hits) = render(true, &[0]);
    let mut expected = opened();
    expected[1] = expected[1].replace('▸', "▾");
    expected.splice(
        2..2,
        [
            format!("▎   ── results ──{}", "─".repeat(123)),
            "▎     1  Decision API".into(),
            "▎        example.org".into(),
            "▎        A decision endpoint.".into(),
        ],
    );
    assert_eq!(snapshot(&lines), expected);
    assert_eq!(hits[0].lines.len(), 5);
    assert_eq!(hits[1].lines.len(), 1);
}

#[test]
fn expanded_failed_row_snapshot_keeps_the_short_error() {
    let (lines, hits) = render(true, &[5]);
    let mut expected = opened();
    expected[6] = expected[6].replace('▸', "▾");
    expected.push("▎   deck failed: no block \"x\"".into());
    assert_eq!(snapshot(&lines), expected);
    assert_eq!(hits[5].lines.len(), 2);
    let theme = crate::theme::Theme::default();
    assert!(
        lines[hits[5].lines.start]
            .spans
            .iter()
            .any(|span| span.content.contains("failed:") && span.style.fg == Some(theme.error))
    );
}

#[test]
fn batch_deck_and_media_images_belong_only_to_their_open_row() {
    let files = std::env::temp_dir().join(format!("dugix-batch-images-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&files).unwrap();
    image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]))
        .save(files.join("slide.png"))
        .unwrap();
    let content = json!({"name":"batch", "arguments":json!({"calls":[
        {"tool":"deck", "arguments":{"action":"view","path":"a.deck.json"}},
        {"tool":"media", "arguments":{"action":"generate_image","prompt":"a"}}
    ]}).to_string(), "result":"[1/2] deck\nviewed 1 slides · a.deck.json\n![slide 1](slide.png)\n[2/2] media/generate_image a\n![image](slide.png)"}).to_string();
    for open in [
        HashSet::new(),
        HashSet::from([(0, 0)]),
        HashSet::from([(0, 1)]),
    ] {
        let mut lines = Vec::new();
        let mut images = Vec::new();
        let mut cache = HashMap::new();
        let mut hits = Vec::new();
        push_tool_call_with_rows(
            &mut lines,
            &content,
            true,
            &dugix_core::app::Settings::default(),
            80,
            &crate::theme::Theme::default(),
            CardEnv {
                files_dir: &files,
                studio_linked: false,
                image_at_line: &mut images,
                image_cache: &mut cache,
            },
            Some(BatchRows {
                owner: 0,
                open: &open,
                focus: None,
                hits: &mut hits,
            }),
        );
        assert_eq!(images.iter().any(Option::is_some), !open.is_empty());
        for (line, image) in images
            .iter()
            .enumerate()
            .filter(|(_, image)| image.is_some())
        {
            let hit = hits.iter().find(|hit| hit.lines.contains(&line)).unwrap();
            assert!(open.contains(&(0, hit.index)));
            assert!(image.as_deref().unwrap().ends_with("slide.png"));
        }
    }
    std::fs::remove_dir_all(files).unwrap();
}
