//! Component sheet: one realistic program per component family, rendered by
//! the real panel renderer and exported as terminal cells.

use super::*;
use serde_json::{Value, json};

fn sheet() -> Value {
    serde_json::from_str(include_str!(
        "../../../../web/prototypes/interactive/component-sheet.json"
    ))
    .unwrap()
}

/// Tokyo Night, a common dark terminal theme with distinct data hues.
pub(super) fn preview_theme() -> Theme {
    let mut theme = Theme::default();
    theme.bg = Color::Rgb(26, 27, 38);
    theme.surface = theme.bg;
    theme.fg = Color::Rgb(192, 202, 245);
    theme.fg_dim = Color::Rgb(120, 127, 168);
    theme.accent = Color::Rgb(122, 162, 247);
    theme.accent2 = Color::Rgb(187, 154, 247);
    theme.border = theme.accent;
    theme.border_dim = Color::Rgb(59, 66, 97);
    theme.success = Color::Rgb(158, 206, 106);
    theme.warning = Color::Rgb(224, 175, 104);
    theme.error = Color::Rgb(247, 118, 142);
    theme.raised = Color::Rgb(36, 40, 59);
    theme.control = Color::Rgb(47, 53, 73);
    theme.selection = Color::Rgb(40, 52, 87);
    theme.round_caps = true;
    theme
}

fn render(source: &str, width: u16, focused: bool) -> Buffer {
    let document = Document::parse(source)
        .unwrap_or_else(|error| panic!("component sheet program failed: {error}\n{source}"));
    let entry = Entry::new(Ok(Session::new(document).unwrap()));
    entry.canvas(width, &preview_theme(), focused, true).0
}

fn encoded(canvas: &Buffer) -> Value {
    let mut rows = Vec::new();
    for y in 0..canvas.area.height {
        let mut runs = Vec::new();
        let mut x = 0;
        while x < canvas.area.width {
            let start = x;
            let style = canvas[(x, y)].style();
            let mut text = String::new();
            while x < canvas.area.width && canvas[(x, y)].style() == style {
                text.push_str(canvas[(x, y)].symbol());
                x += 1;
            }
            let modifier = style.add_modifier;
            runs.push(json!({"x":start,"text":text,
                "fg":format!("{:?}",style.fg.unwrap_or(Color::Reset)),
                "bg":format!("{:?}",style.bg.unwrap_or(Color::Reset)),
                "bold":modifier.contains(Modifier::BOLD),
                "italic":modifier.contains(Modifier::ITALIC),
                "underline":modifier.contains(Modifier::UNDERLINED),
                "dim":modifier.contains(Modifier::DIM),
                "reverse":modifier.contains(Modifier::REVERSED)}));
        }
        rows.push(runs);
    }
    let theme = preview_theme();
    json!({"width":canvas.area.width,"height":canvas.area.height,"rows":rows,
        "foreground":format!("{:?}",theme.fg),"background":format!("{:?}",theme.bg)})
}

#[test]
fn component_sheet_covers_every_contract_with_valid_programs() {
    let sheet = sheet();
    let cases = sheet["cases"].as_array().unwrap();
    let signatures: Vec<_> = sheet["contracts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|contract| contract["signature"].as_str().unwrap())
        .collect();
    assert_eq!(
        signatures.join("\n"),
        dugix_core::interactive::component_signatures()
    );
    for contract in sheet["contracts"].as_array().unwrap() {
        let name = contract["name"].as_str().unwrap();
        assert!(
            cases.iter().any(|case| case["names"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == name)),
            "missing {name}"
        );
    }
    for case in cases {
        let source = case["source"].as_str().unwrap();
        for width in [44, 76] {
            let canvas = render(source, width, false);
            assert_eq!(canvas.area.width, width);
            assert!(canvas.area.height <= 40, "{} became too tall", case["id"]);
        }
    }
}

#[test]
#[ignore = "opt-in component sheet export; scripts/preview-terminal-panels.sh"]
fn export_component_sheet() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("DUGIX_TERMINAL_PREVIEW_DIR").expect("set DUGIX_TERMINAL_PREVIEW_DIR"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let mut sheet = sheet();
    for case in sheet["cases"].as_array_mut().unwrap() {
        let source = case["source"].as_str().unwrap().to_string();
        let mut frames = serde_json::Map::new();
        for (size, width) in [("wide", 76), ("narrow", 44)] {
            for (state, focused) in [("default", false), ("focused", true)] {
                frames.insert(
                    format!("{size}/{state}"),
                    encoded(&render(&source, width, focused)),
                );
            }
        }
        case["frames"] = Value::Object(frames);
    }
    let path = directory.join("component-sheet-frames.json");
    std::fs::write(&path, serde_json::to_vec(&sheet).unwrap()).unwrap();
    eprintln!("Component sheet: {}", path.display());
}
