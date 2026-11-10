use super::*;
use nexus_core::app::Settings;
use serde_json::json;

/// `push_tool_call` with no studio window and no image files.
fn push_card(
    out: &mut Vec<Line<'static>>,
    content: &str,
    expanded: bool,
    settings: &Settings,
    width: usize,
    theme: &crate::theme::Theme,
) {
    let mut at_line = Vec::new();
    let mut cache = HashMap::new();
    push_tool_call(
        out,
        content,
        expanded,
        settings,
        width,
        theme,
        CardEnv {
            files_dir: std::path::Path::new("."),
            studio_linked: false,
            image_at_line: &mut at_line,
            image_cache: &mut cache,
        },
    );
}

fn card(
    name: &str,
    args: serde_json::Value,
    result: &str,
    expanded: bool,
    hide_hints: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let settings = Settings {
        hide_hints,
        ..Settings::default()
    };
    push_card(
        &mut lines,
        &json!({"name":name,"arguments":args.to_string(),"result":result}).to_string(),
        expanded,
        &settings,
        80,
        &crate::theme::Theme::default(),
    );
    lines
}

fn text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[test]
fn doc_cards_hint_at_studio_like_deck_cards() {
    let hint = |rows: &[String]| {
        rows.iter()
            .any(|row| row.contains("open in studio · /studio"))
    };
    for (action, result, path) in [
        (
            "edit",
            "edited doc · docs/q3.doc.json · revision 0123456789ab (was fedcba987654)\n- update_block b2\n[b2] We grew 18%",
            "docs/q3.doc.json",
        ),
        (
            "create",
            "created doc · docs/q3.doc.json · 4 blocks · revision 0123456789ab\n[b1] # Q3 strategy",
            "docs/q3.doc.json",
        ),
    ] {
        let args = json!({"action": action, "path": path});
        let rows = text(&card("canvas", args.clone(), result, false, false));
        assert!(rows[0].contains("doc"), "{rows:?}");
        assert!(hint(&rows), "{action}: {rows:?}");
        assert!(
            !hint(&text(&card("canvas", args.clone(), result, false, true))),
            "hints off"
        );
        // Reading, viewing and failing do not invite opening anything.
        let failed = text(&card(
            "canvas",
            args,
            "Nothing was written. Problems to fix",
            false,
            false,
        ));
        assert!(!hint(&failed), "{failed:?}");
    }
    let read = text(&card(
        "canvas",
        json!({"action": "read", "path": "docs/q3.doc.json"}),
        "doc · docs/q3.doc.json · 4 blocks · revision 0123456789ab\n[b1] # Q3",
        false,
        false,
    ));
    assert!(!hint(&read), "{read:?}");
}

#[test]
fn deck_cards_name_the_change_hint_at_studio_and_show_slides_when_opened() {
    let files = std::env::temp_dir().join(format!("nexus-deck-card-{}", uuid::Uuid::new_v4()));
    let deck = nexus_core::deck::Deck::new_from_layouts("Card");
    std::fs::create_dir_all(files.join("decks/.renders/a")).unwrap();
    let png = nexus_core::deck::render::render_slide(&deck, "s1", 400).unwrap();
    std::fs::write(files.join("decks/.renders/a/s1.png"), png).unwrap();
    let result = "edited slide 1 · decks/a.deck.json · revision 0123456789ab\n- update_block b1 on slide 1: text\n![slide 1](decks/.renders/a/s1.png)";
    let content = json!({
        "name": "deck",
        "arguments": json!({"action": "edit", "path": "decks/a.deck.json", "ops": []}).to_string(),
        "result": result,
    })
    .to_string();
    let theme = crate::theme::Theme::default();
    let render = |expanded: bool, linked: bool, hide_hints: bool| {
        let mut rows = Vec::new();
        let mut at_line = Vec::new();
        let mut cache = HashMap::new();
        push_tool_call(
            &mut rows,
            &content,
            expanded,
            &Settings {
                hide_hints,
                ..Settings::default()
            },
            60,
            &theme,
            CardEnv {
                files_dir: &files,
                studio_linked: linked,
                image_at_line: &mut at_line,
                image_cache: &mut cache,
            },
        );
        (text(&rows), at_line)
    };

    let (collapsed, _) = render(false, false, false);
    assert!(
        collapsed[0].contains("edited slide 1") && collapsed[0].contains("decks/a.deck.json"),
        "{collapsed:?}"
    );
    assert!(
        collapsed
            .iter()
            .any(|row| row.contains("open in studio · /studio")),
        "{collapsed:?}"
    );
    assert!(
        !render(false, true, false)
            .0
            .iter()
            .any(|row| row.contains("open in studio")),
        "linked"
    );
    assert!(
        !render(false, false, true)
            .0
            .iter()
            .any(|row| row.contains("open in studio")),
        "hints off"
    );

    let (opened, at_line) = render(true, true, false);
    assert!(
        opened
            .iter()
            .any(|row| row.contains("update_block b1 on slide 1: text")),
        "{opened:?}"
    );
    assert!(
        opened.iter().any(|row| row.contains('▀')),
        "the slide is drawn inline"
    );
    assert!(
        at_line.iter().any(Option::is_some),
        "the slide is clickable"
    );
    std::fs::remove_dir_all(files).unwrap();
}

#[test]
fn connector_calls_show_the_tool_the_connector_arguments_and_a_result_preview() {
    let args = json!({"title": "Fix the login bug", "priority": 2, "labels": ["a"]});
    let ok = card(
        "linear__create_issue",
        args.clone(),
        "created LIN-42\nhttps://linear.app/x/issue/LIN-42",
        false,
        true,
    );
    let head = text(&ok).remove(0);
    assert!(
        head.starts_with(&format!(
            "{} create_issue linear · labels=… priority=2 title=\"Fix the login bug\"",
            glyph::CONNECTOR
        )),
        "{head}"
    );
    assert!(head.contains("created LIN-42"), "{head}");
    let failed = card(
        "linear__create_issue",
        args.clone(),
        "linear create_issue error: no such team\nthe server reported that the tool failed",
        false,
        true,
    );
    let head = text(&failed).remove(0);
    assert!(head.starts_with(glyph::FAIL), "{head}");
    assert!(
        head.contains("create_issue linear") && head.contains("failed"),
        "{head}"
    );
    let denied = text(&card(
        "linear__create_issue",
        args.clone(),
        "denied by the user · ask what they want instead of retrying",
        false,
        true,
    ))
    .remove(0);
    assert!(denied.contains("denied"), "{denied}");
    // Through the meta-tool the inner tool is what the card names.
    let meta = card(
        "connectors",
        json!({"action": "call", "tool": "linear__get_issue", "arguments": {"id": "LIN-1"}}),
        "LIN-1: Fix it",
        false,
        true,
    );
    let head = text(&meta).remove(0);
    assert!(head.starts_with(glyph::CONNECTOR), "{head}");
    assert!(
        head.contains("get_issue linear · id=\"LIN-1\" · LIN-1: Fix it"),
        "{head}"
    );
    let search = text(&card(
        "connectors",
        json!({"action": "search", "query": "issues"}),
        "linear__a · read-only",
        false,
        true,
    ))
    .remove(0);
    assert!(search.contains("search connectors"), "{search}");
    // Expanded: the generic argument table and the full result.
    let expanded = text(&card(
        "linear__create_issue",
        args,
        "created LIN-42",
        true,
        true,
    ))
    .join("\n");
    assert!(
        expanded.contains("title") && expanded.contains("created LIN-42"),
        "{expanded}"
    );
    // Built-in cards are untouched.
    assert!(
        text(&card(
            "search",
            json!({"query": "rust", "mode": "web"}),
            "no results",
            false,
            true
        ))[0]
            .starts_with(glyph::WEB)
    );
}

#[test]
fn expanded_diff_tints_full_rows_but_not_previews() {
    let result = "edited a (+1 −1)\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\x1b";
    let args = json!({"action":"edit","path":"a"});
    let theme = crate::theme::Theme::default();
    let expanded = card("fs", args.clone(), result, true, false);
    for (needle, bg) in [("− old", theme.diff_del_bg), ("+ new", theme.diff_add_bg)] {
        let row = expanded
            .iter()
            .find(|row| row.to_string().contains(needle))
            .unwrap();
        assert_eq!(row.width(), 80);
        assert_eq!(row.style.bg, Some(bg));
        assert_eq!(row.spans[0].style.fg, Some(theme.accent2));
        assert!(row.spans[1..].iter().all(|s| s.style.bg == Some(bg)));
    }
    assert!(
        card("fs", args, result, false, false)
            .iter()
            .all(|row| row.style.bg.is_none() && row.spans.iter().all(|s| s.style.bg.is_none()))
    );
}

#[test]
fn expanded_commands_color_output_and_align_cwd() {
    let theme = crate::theme::Theme::default();
    let lines = card(
        "shell",
        json!({"action":"run","cmd":"cargo build","cwd":"sub/dir"}),
        "exit 101 · 9.8s\nnormal\n  error[E0425]: missing\nwarning: unused\nfatal: broken\nFAILED test\npanicked here",
        true,
        false,
    );
    assert!(lines[1].to_string().starts_with("▎ $ cargo build"));
    assert!(lines[1].to_string().ends_with("in sub/dir"));
    assert_eq!(lines[1].width(), 80);
    assert!(lines[2].to_string().contains("output · 6 lines"));
    for line in &lines[1..lines.len() - 1] {
        assert_eq!(line.spans[0].style.fg, Some(theme.error));
    }
    for (needle, color) in [
        ("normal", theme.fg_dim),
        ("error[E", theme.error),
        ("warning:", theme.warning),
        ("fatal:", theme.error),
        ("FAILED", theme.error),
        ("panicked", theme.error),
    ] {
        let row = lines
            .iter()
            .find(|l| l.to_string().contains(needle))
            .unwrap();
        assert!(row.spans[1..].iter().any(|s| s.style.fg == Some(color)));
    }
}

#[test]
fn grep_highlights_literal_case_sensitive_matches_and_paths() {
    let lines = card(
        "fs",
        json!({"action":"grep","path":"crates","pattern":"fn parse"}),
        "crates/a.rs:\n  12: pub fn parse() { fn parse(); FN PARSE(); }",
        true,
        false,
    );
    let theme = crate::theme::Theme::default();
    assert_eq!(lines[1].to_string(), "▎ crates/a.rs");
    assert_eq!(lines[1].spans[1].style.fg, Some(theme.accent));
    let matched: Vec<_> = lines[2]
        .spans
        .iter()
        .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
        .collect();
    assert_eq!(matched.len(), 2);
    assert!(
        matched
            .iter()
            .all(|s| s.content == "fn parse" && s.style.fg == Some(theme.accent))
    );
    assert!(lines[2].to_string().contains("  12  pub"));
}

#[test]
fn reads_highlight_code_with_dim_number_gutters_and_language() {
    let lines = card(
        "fs",
        json!({"action":"read","path":"src/a.rs"}),
        "src/a.rs (lines 40-42 of 42):\n   40:0123abcd\tlet value = 1;\n   41:abcdef01\t\n   42:abcdef02\tvalue + 2",
        true,
        false,
    );
    let theme = crate::theme::Theme::default();
    assert!(lines[1].to_string().contains("── src/a.rs ── rust ──"));
    assert!(
        lines[2].to_string().contains("40: let value = 1;"),
        "{:?}",
        text(&lines)
    );
    assert!(!text(&lines).join("\n").contains("0123abcd"));
    assert_eq!(lines[2].spans[1].style.fg, Some(theme.fg_dim));
    assert!(
        lines[2].spans[2..]
            .iter()
            .any(|s| s.style.fg.is_some_and(|c| c != theme.fg_dim))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.to_string().contains("42: value + 2"))
    );
}

#[test]
fn read_wrapping_preserves_tabs_fences_and_later_line_numbers() {
    let theme = crate::theme::Theme::default();
    let rows = read_body(
        "notes.txt",
        "notes.txt (lines 1-3 of 3):\n1:     ````\n2: \tlong content that wraps\n3: end",
        16,
        &theme,
    );
    let shown = text(&rows).join("\n");
    assert!(shown.contains("plain"));
    assert!(shown.contains("1:     ````"), "{shown}");
    assert!(shown.contains("2:     long"), "{shown}");
    assert!(shown.contains("3: end"), "{shown}");
    assert!(rows.iter().all(|row| row.width() <= 16));
}

#[test]
fn markdown_controls_stay_dim_even_when_the_escape_wraps() {
    let theme = crate::theme::Theme::default();
    for source in [
        "xxxxxxxxxxxxxxxxxxxxxxxx\x1b",
        "```text\nxxxxxxxxxxxxxxxxxxxxxxxx\x1b\n```",
    ] {
        let rows = tool_markdown(source, 10, &theme);
        assert!(rows.iter().all(|row| row.width() <= 10));
        let escapes: Vec<_> = rows
            .iter()
            .flat_map(|row| &row.spans)
            .filter(|s| s.content.chars().any(|c| "\\u{1b}".contains(c)))
            .collect();
        assert!(!escapes.is_empty());
        assert!(escapes.iter().all(|s| s.style.fg == Some(theme.fg_dim)));
    }
}

#[test]
fn reports_use_markdown_and_generic_arguments_form_a_table() {
    let report = card(
        "task",
        json!({"prompt":"report"}),
        "sub-agent report (1 steps, 10 tokens):\n# Findings\n\n**important** text",
        true,
        false,
    );
    assert!(report.iter().any(|l| {
        l.spans.iter().any(|s| {
            s.content.contains("important") && s.style.add_modifier.contains(Modifier::BOLD)
        })
    }));
    assert!(
        !report[1..]
            .iter()
            .any(|l| l.to_string().contains("sub-agent report"))
    );
    let lines = card(
        "search",
        json!({"query":"needle","limit":3}),
        "a result",
        true,
        false,
    );
    assert!(lines.iter().any(|l| l.to_string() == "▎ limit  3"));
    assert!(lines.iter().any(|l| l.to_string() == "▎ query  needle"));
    assert!(lines.iter().any(|l| l.to_string().contains("── result ──")));
    for action in ["glob", "list"] {
        let listing = card(
            "fs",
            json!({"action":action,"path":".","pattern":"*"}),
            "dir/\nfile",
            true,
            false,
        );
        assert_eq!(
            listing[1].spans[1].style.fg,
            Some(crate::theme::Theme::default().accent)
        );
    }
}

#[test]
fn expanded_bodies_are_capped_and_controls_are_always_dim_escapes() {
    let result = (0..450)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let lines = card("search", json!({}), &result, true, false);
    assert_eq!(lines.len(), 402); // header + 400 body rows + blank
    assert_eq!(lines[400].to_string(), "▎ … 52 more lines");
    let theme = crate::theme::Theme::default();
    for (name, args, result) in [
        (
            "fs",
            json!({"action":"edit","path":"a"}),
            "edited a (+1 −0)\n--- a/a\n+++ b/a\n@@ -0,0 +1 @@\n+x\x1b",
        ),
        (
            "shell",
            json!({"action":"run","cmd":"x\x1b","cwd":"a\x07"}),
            "exit 1\nerror: x\r",
        ),
        (
            "proc",
            json!({"action":"read","id":"x"}),
            "x · exited 1\npanicked\x1b",
        ),
        (
            "fs",
            json!({"action":"read","path":"a.rs"}),
            "a.rs (lines 1-1 of 1):\n1: let x = \"a\x1b\";\r",
        ),
        (
            "fs",
            json!({"action":"grep","pattern":"x","path":"."}),
            "a\x07:\n  1: x\x1b",
        ),
        (
            "fs",
            json!({"action":"glob","pattern":"*","path":"."}),
            "a\x1b/",
        ),
        ("fs", json!({"action":"list","path":"."}), "a\x1b/"),
        (
            "task",
            json!({"prompt":"report"}),
            "sub-agent report (1 steps, 1 tokens):\n**x\x1b**",
        ),
        ("search", json!({"query":"a\x07"}), "x\x1b"),
    ] {
        let lines = card(name, args, result, true, false);
        let body = text(&lines[1..]).join("\n");
        assert!(
            !body.chars().any(|c| c.is_control() && c != '\n'),
            "{name}: {body}"
        );
        assert!(body.contains("\\u{"), "{name}: {body}");
        for row in lines.iter().skip(1).take(lines.len() - 2) {
            assert!(row.to_string().starts_with("▎ "), "{name}: {row}");
            assert!(row.width() <= 80, "{name}: {row}");
            for span in &row.spans {
                if span.content.contains("\\u{") || span.content == "\\" {
                    assert_eq!(span.style.fg, Some(theme.fg_dim), "{name}: {span:?}");
                }
            }
        }
    }
}

#[test]
fn narrow_expanded_cards_wrap_gutters_without_losing_the_rail() {
    let theme = crate::theme::Theme::default();
    for (name, args, result) in [
        (
            "fs",
            json!({"action":"edit","path":"a"}),
            "edited a (+1 −1)\n--- a/a\n+++ b/a\n@@ -100 +100 @@\n-old\n+new",
        ),
        (
            "fs",
            json!({"action":"grep","path":".","pattern":"x"}),
            "a:\n  100: xxxxx",
        ),
    ] {
        let mut rows = Vec::new();
        push_card(
            &mut rows,
            &json!({"name":name,"arguments":args.to_string(),"result":result}).to_string(),
            true,
            &Settings::default(),
            8,
            &theme,
        );
        for row in &rows[1..rows.len() - 1] {
            assert!(row.width() <= 8, "{row}");
            assert_eq!(row.spans[0].content, "▎ ");
        }
    }
}

#[test]
fn fold_markers_fit_without_wrapping_long_headers() {
    for expanded in [false, true] {
        let short = card("search", json!({"query":"x"}), "ok", expanded, false);
        assert_eq!(short[0].width(), 80);
        assert!(short[0].to_string().ends_with(if expanded {
            glyph::EXPANDED
        } else {
            glyph::SELECTED
        }));
        let long = card(
            "shell",
            json!({"action":"run","cmd":"x".repeat(100)}),
            "exit 0",
            expanded,
            false,
        );
        assert!(!long[0].to_string().contains(['▸', '▾']));
    }
}

#[test]
fn round_thinking_and_interim_text_sit_above_the_first_call() {
    let row = json!({
        "name": "fs",
        "arguments": json!({"action":"read","path":"src/a.rs"}).to_string(),
        "result": "src/a.rs (lines 1-2 of 2):\n1: fn a() {}\n2: fn b() {}",
        "reasoning_content": "check a first",
        "assistant_content": "Let me read a.rs.",
    })
    .to_string();
    for (show, expected) in [
        (false, "▎ ▸ reasoning (13 chars) — Ctrl+R to expand"),
        (true, "▎ ▾ reasoning"),
    ] {
        let mut lines = Vec::new();
        let settings = Settings {
            show_reasoning: show,
            ..Settings::default()
        };
        push_card(
            &mut lines,
            &row,
            false,
            &settings,
            80,
            &crate::theme::Theme::default(),
        );
        let lines = text(&lines);
        assert_eq!(lines[0], expected);
        let said = lines.iter().position(|l| l == "Let me read a.rs.").unwrap();
        let call = lines
            .iter()
            .position(|l| l.contains("read src/a.rs"))
            .unwrap();
        assert!(said < call, "{lines:?}");
    }
}

#[test]
fn ctrl_t_shows_the_result_of_reads_and_searches() {
    let result = "src/a.rs (lines 1-2 of 2):\n1: fn a() {}\n2: fn b() {}";
    let collapsed = text(&card(
        "fs",
        json!({"action":"read","path":"src/a.rs"}),
        result,
        false,
        false,
    ));
    assert_eq!(collapsed.len(), 1, "{collapsed:?}");
    let expanded = text(&card(
        "fs",
        json!({"action":"read","path":"src/a.rs"}),
        result,
        true,
        false,
    ));
    assert!(
        expanded.iter().any(|l| l.contains("2: fn b() {}")),
        "{expanded:?}"
    );
}

#[test]
fn web_searches_show_their_query_sources_and_results() {
    let result = "[1] Budget passes\n    https://www.mongolia.substack.com/p/b\n    The 2026 budget passed on time.\n[2] World population\n    https://en.wikipedia.org/wiki/World_population\n    The total number of people.";
    let args = json!({"mode":"web","query":"Mongolia budget"});
    let collapsed = text(&card("search", args.clone(), result, false, false));
    assert!(
        collapsed[0].starts_with("◎ search \"Mongolia budget\" · 2 results"),
        "{collapsed:?}"
    );
    assert_eq!(
        collapsed[1].trim(),
        "mongolia.substack.com · en.wikipedia.org"
    );
    let open = text(&card("search", args, result, true, false));
    assert!(
        open.iter().any(|l| l.contains("1  Budget passes")),
        "{open:?}"
    );
    assert!(
        open.iter().any(|l| l
            .trim_start_matches(['▎', ' '])
            .starts_with("en.wikipedia.org")),
        "{open:?}"
    );
    assert!(
        open.iter()
            .any(|l| l.contains("The total number of people.")),
        "{open:?}"
    );
}

fn thumbnail_png() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(24, 24, image::Rgb([20, 200, 90]));
    let mut out = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn opened_search_card_shows_a_thumbnail_only_beside_results_that_have_one() {
    let url = "https://cdn.example.invalid/thumb.png";
    let plain = "[1] First\n    https://a.example/p\n    Text one.\n\n[2] Second\n    https://b.example/p\n    Text two.";
    let with = format!(
        "[1] First\n    https://a.example/p\n    Text one.\n    image: {url}\n\n[2] Second\n    https://b.example/p\n    Text two."
    );
    let args = json!({"mode":"web","query":"q"});
    let fetcher = nexus_core::remote_image::Fetcher::new(
        std::env::temp_dir().join(format!("nexus-card-{}", uuid::Uuid::new_v4())),
    );
    crate::app_view::use_fetcher_for_tests(&fetcher);
    let baseline = text(&card("search", args.clone(), plain, true, false));

    // Not fetched yet (and no runtime in this test): a compact marker.
    let loading = text(&card("search", args.clone(), &with, true, false));
    assert!(
        loading
            .iter()
            .any(|l| l.contains("1  First") && l.contains(glyph::MEDIA)),
        "{loading:?}"
    );

    fetcher.seed(url, &thumbnail_png()).unwrap();
    let ready = card("search", args.clone(), &with, true, false);
    let ready_text = text(&ready);
    let pixel_rows = ready_text.iter().filter(|l| l.contains('▀')).count();
    assert_eq!(pixel_rows, 3, "{ready_text:?}");
    // Same rows as a result list without images (the text is 3 rows tall
    // already); the second, image-less result is untouched.
    assert_eq!(ready_text.len(), baseline.len(), "{ready_text:?}");
    assert!(
        ready_text
            .iter()
            .any(|l| l.trim_end().ends_with("Second") && !l.contains('▀'))
    );
    assert!(ready.iter().all(|l| l.width() <= 80));

    // Collapsed card is unchanged by thumbnails.
    let collapsed_with = text(&card("search", args.clone(), &with, false, false));
    let collapsed_plain = text(&card("search", args, plain, false, false));
    assert_eq!(collapsed_with, collapsed_plain);
    crate::app_view::use_fetcher_for_tests(&nexus_core::remote_image::Fetcher::new(
        std::env::temp_dir(),
    ));
}

#[test]
fn compact_edit_is_capped_and_expanded_diff_never_shows_arguments() {
    let diff = (1..=20)
        .map(|n| format!("+new line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let result =
        format!("edited src/a (+20 −1)\n--- a/src/a\n+++ b/src/a\n@@ -1 +1,20 @@\n-old\n{diff}");
    let args = json!({"action":"edit","path":"src/a","edits":[{"hash":"secret hash","new":"new"}]});
    let compact = card("fs", args.clone(), &result, false, false);
    assert_eq!(
        compact[0].to_string().trim_end_matches([' ', '▸']),
        "⚒ edit src/a  +20 −1"
    );
    assert_eq!(compact.len(), 9);
    assert!(
        compact
            .last()
            .unwrap()
            .to_string()
            .contains("more lines · Ctrl+T")
    );
    assert!(
        compact[1..]
            .iter()
            .all(|line| line.to_string().starts_with("   "))
    );
    let expanded = card("fs", args.clone(), &result, true, false);
    let text = expanded
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("new line 20"));
    assert!(!text.contains("secret hash"));
    assert!(!text.contains("\"action\""));
    assert_eq!(expanded.last().unwrap().to_string(), "");
    assert!(
        !card("fs", args, &result, false, true)
            .last()
            .unwrap()
            .to_string()
            .contains("Ctrl+T")
    );
}

#[test]
fn failed_command_shows_only_output_tail_until_expanded() {
    let args = json!({"action":"run","cmd":"cargo build"});
    let result = "exit 101 · 9.8s\nfirst\nsecond\nthird\nfourth\nfifth\nsixth\nlast\x1b[2J";
    let compact = card("shell", args.clone(), result, false, false);
    assert_eq!(
        compact[0].to_string().trim_end_matches([' ', '▸']),
        "✗ $ cargo build · exit 101 · 9.8s"
    );
    assert_eq!(compact.len(), 6);
    assert_eq!(compact[1].to_string(), "   third");
    assert!(compact[5].to_string().contains("last\\u{1b}[2J"));
    let expanded = card("shell", args, result, true, false);
    assert!(expanded.iter().any(|line| line.to_string() == "▎ first"));
    assert!(
        !expanded
            .iter()
            .any(|line| line.to_string().contains("\"cmd\""))
    );
    assert_eq!(
        card(
            "shell",
            json!({"action":"run","cmd":"true"}),
            "exit 0\nok",
            false,
            false
        )
        .len(),
        1
    );
}
