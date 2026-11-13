use super::*;
use crate::doc::test_support::{Temp, checked};
use serde_json::json;
fn doc(blocks: serde_json::Value, height: f64) -> Doc {
    checked(
        json!({"version":1,"title":"Pages","page":{"size":{"custom":{"w":300,"h":height}},"margins_pt":[20,20,20,20],"header":"{title}","footer":"{page}/{pages}"},"theme":{"text_styles":{"body":{"size":10,"line_height":1},"h1":{"size":12,"line_height":1}}},"blocks":blocks}),
    )
}
fn words(page: &Page, block: &str) -> String {
    page.items
        .iter()
        .filter(|i| i.block == block)
        .filter_map(|i| {
            if let Paint::Run(r) = &i.paint {
                Some(r.text.as_str())
            } else {
                None
            }
        })
        .collect()
}
fn lines(page: &Page, block: &str) -> usize {
    let mut ys = page
        .items
        .iter()
        .filter(|i| i.block == block && matches!(i.paint, Paint::Run(_)))
        .map(|i| i.y.to_bits())
        .collect::<Vec<_>>();
    ys.sort_unstable();
    ys.dedup();
    ys.len()
}
#[test]
fn heading_keeps_with_next_at_bottom() {
    let doc = doc(
        json!([{"id":"first","kind":"paragraph","text":"one  \ntwo  \nthree  \nfour"},{"id":"heading","kind":"heading","level":1,"text":"Heading"},{"id":"next","kind":"paragraph","text":"The next block."}]),
        120.0,
    );
    let pages = paginate(&doc, None);
    assert_eq!(pages.len(), 2);
    assert!(words(&pages[0], "heading").is_empty());
    assert_eq!(words(&pages[1], "heading"), "Heading");
    assert!(words(&pages[1], "next").contains("next"));
}
#[test]
fn paragraphs_and_list_items_have_no_single_line_widows_or_orphans() {
    for kind in ["paragraph", "list"] {
        for filler in ["a  \nb", "a  \nb  \nc"] {
            let text = "one  \ntwo  \nthree  \nfour  \nfive";
            let block = if kind == "list" {
                json!({"id":"p","kind":"list","items":[{"text":text}]})
            } else {
                json!({"id":"p","kind":"paragraph","text":text})
            };
            let doc = doc(
                json!([{"id":"filler","kind":"paragraph","text":filler},block]),
                100.0,
            );
            let pages = paginate(&doc, None);
            let counts = pages
                .iter()
                .map(|p| lines(p, "p"))
                .filter(|n| *n > 0)
                .collect::<Vec<_>>();
            assert_eq!(counts.iter().sum::<usize>(), 5);
            assert!(counts.iter().all(|n| *n >= 2), "{kind}: {counts:?}");
        }
    }
}
#[test]
fn table_splits_and_repeats_its_header() {
    let rows = std::iter::once(vec!["Header"])
        .chain((0..12).map(|_| vec!["Cell"]))
        .collect::<Vec<_>>();
    let doc = doc(
        json!([{"id":"table","kind":"table","header":true,"rows":rows}]),
        140.0,
    );
    let pages = paginate(&doc, None);
    assert!(pages.len() > 1);
    for page in pages {
        assert!(words(&page, "table").starts_with("Header"));
        assert!(words(&page, "table").contains("Cell"));
    }
}
#[test]
fn tall_image_is_scaled_inside_content_box() {
    let temp = Temp::new();
    temp.image();
    let doc = doc(
        json!([{"id":"image","kind":"image","src":"assets/sample.png"}]),
        120.0,
    );
    let pages = paginate(&doc, Some(&temp.0.join("assets")));
    let item = pages[0]
        .items
        .iter()
        .find(|i| matches!(i.paint, Paint::Image { .. }))
        .unwrap();
    assert!(item.h <= 80.0 && item.y + item.h <= 100.0);
    assert!((item.w / item.h - 0.5).abs() < 0.001);
}
#[test]
fn code_and_tall_callouts_split_by_line_but_small_ones_move_whole() {
    for kind in ["code", "callout"] {
        let text = (0..30)
            .map(|_| "a line")
            .collect::<Vec<_>>()
            .join(if kind == "code" { "\n" } else { "  \n" });
        let block = if kind == "code" {
            json!({"id":"long","kind":kind,"text":text})
        } else {
            json!({"id":"long","kind":kind,"tone":"note","text":text})
        };
        let doc = doc(json!([block]), 140.0);
        let pages = paginate(&doc, None);
        assert!(pages.len() > 1);
        assert_eq!(
            pages
                .iter()
                .map(|p| words(p, "long").matches("line").count())
                .sum::<usize>(),
            30
        );
    }
}
#[test]
fn explicit_page_break_and_two_pass_toc_numbers_and_fields() {
    let doc = doc(
        json!([{"id":"toc","kind":"toc"},{"kind":"page_break"},{"id":"h","kind":"heading","level":1,"text":"Chapter"},{"kind":"paragraph","text":"Body"}]),
        200.0,
    );
    let pages = paginate(&doc, None);
    assert_eq!(pages.len(), 2);
    assert_eq!(words(&pages[0], "toc"), "Chapter2");
    assert_eq!(heading_pages(&pages)["h"], 2);
    assert_eq!(words(&pages[0], "@header"), "Pages");
    assert_eq!(words(&pages[0], "@footer"), "1/2");
    assert_eq!(words(&pages[1], "@footer"), "2/2");
}
#[test]
fn a4_letter_and_custom_dimensions() {
    let mut doc = doc(json!([]), 200.0);
    for (paper, size) in [
        (super::super::model::Paper::A4, (595.28, 841.89)),
        (super::super::model::Paper::Letter, (612.0, 792.0)),
        (
            super::super::model::Paper::Custom { w: 400.0, h: 600.0 },
            (400.0, 600.0),
        ),
    ] {
        doc.page.size = paper;
        let page = &paginate(&doc, None)[0];
        assert_eq!((page.width, page.height), size);
    }
}
#[test]
fn code_runs_take_design_syntax_colors_without_changing_line_breaks() {
    let text = "fn main() { let s = \"hi\"; } // done";
    let colored = doc(
        json!([{"id":"c","kind":"code","lang":"rust","text":text}]),
        400.0,
    );
    let plain = doc(
        json!([{"id":"c","kind":"code","lang":"","text":text}]),
        400.0,
    );
    let colored_pages = paginate(&colored, None);
    let plain_pages = paginate(&plain, None);
    assert_eq!(words(&colored_pages[0], "c"), words(&plain_pages[0], "c"));
    assert_eq!(lines(&colored_pages[0], "c"), lines(&plain_pages[0], "c"));
    let theme = crate::doc::inline::theme(&colored);
    let color_of = |want: &str| {
        colored_pages[0]
            .items
            .iter()
            .find_map(|item| match &item.paint {
                Paint::Run(run) if item.block == "c" && run.text.trim() == want => {
                    Some(run.color.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no run {want:?}"))
    };
    assert_eq!(color_of("fn"), crate::deck::text::color(&theme, "accent"));
    assert_eq!(
        color_of("\"hi\""),
        crate::deck::text::color(&theme, "accent2")
    );
    assert_eq!(color_of("done"), crate::deck::text::color(&theme, "muted"));
}
#[test]
fn justified_lines_end_flush_except_last_lines_hard_breaks_and_cells() {
    let sentence = "Justified text spreads the slack of every full line over its word gaps so both edges line up";
    let blocks = json!([
        {"id":"p","kind":"paragraph","text":format!("{sentence}  \n{sentence}")},
        {"id":"t","kind":"table","header":false,"rows":[[sentence, "x"]]}
    ]);
    let set = |align: &str| {
        let mut doc = doc(blocks.clone(), 2000.0);
        doc.theme.text_styles.get_mut("body").unwrap().align =
            Some(serde_json::from_value(json!(align)).unwrap());
        paginate(&doc, None).remove(0)
    };
    let (left, justified) = (set("left"), set("justify"));
    // Run items of a block grouped by line, top to bottom.
    let rows = |page: &Page, block: &str| {
        let mut rows: BTreeMap<u64, Vec<(f64, f64)>> = BTreeMap::new();
        for item in page.items.iter().filter(|i| i.block == block) {
            if matches!(item.paint, Paint::Run(_)) {
                rows.entry(item.y.to_bits())
                    .or_default()
                    .push((item.x, item.w));
            }
        }
        rows.into_values().collect::<Vec<_>>()
    };
    let (ragged, flush) = (rows(&left, "p"), rows(&justified, "p"));
    assert_eq!(ragged.len(), flush.len(), "justifying never rewraps");
    let right = 280.0;
    let mut stretched = 0;
    for (index, (a, b)) in ragged.iter().zip(&flush).enumerate() {
        assert!(
            (a[0].0 - b[0].0).abs() < 1e-9,
            "line {index} starts at the left edge"
        );
        let (end_a, end_b) = (a.last().unwrap(), b.last().unwrap());
        if (end_b.0 - end_a.0).abs() > 1e-9 {
            stretched += 1;
            assert!(
                end_b.0 + end_b.1 >= right - 0.01,
                "line {index} reaches the right edge"
            );
            assert!(end_b.0 < right, "the last word starts inside the column");
        }
    }
    // Two wrapped sentences: every line but the one before the break and the last stretches.
    assert_eq!(stretched, flush.len() - 2, "{flush:?}");
    let before_break = ragged.len() / 2 - 1;
    assert_eq!(
        ragged[before_break], flush[before_break],
        "a hard break stays ragged"
    );
    assert_eq!(ragged.last(), flush.last(), "the last line stays ragged");
    assert_eq!(
        rows(&left, "t"),
        rows(&justified, "t"),
        "table cells stay ragged"
    );
}
