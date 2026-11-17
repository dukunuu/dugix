use super::*;
use crate::{deck::history::Who, design::Design};
use serde_json::{Value as Json, json};

fn blank() -> Sheet {
    Sheet::from_design("Budget", &Design::default())
}

fn checked(sheet: &Sheet) -> Sheet {
    Sheet::check(&serde_json::to_string(sheet).unwrap()).unwrap()
}

fn edits(sheet: &Sheet, ops: Json) -> Sheet {
    let ops: Vec<Op> = serde_json::from_value(ops).unwrap();
    let result = apply(sheet, &ops).unwrap();
    assert_eq!(checked(&result), result);
    result
}

fn source(sheet: &Sheet, tab: usize, key: &str) -> String {
    sheet.tabs[tab]
        .cells
        .get(key)
        .map_or_else(String::new, |c| c.input.clone())
}

#[test]
fn validation_accepts_design_and_rejects_bad_files() {
    let sheet = blank();
    assert_eq!(checked(&sheet), sheet);
    assert_eq!(sheet.tabs[0].id, "t1");
    assert_eq!(
        sheet.theme,
        crate::doc::Theme::from_design(&Design::default())
    );
    let raw = serde_json::to_value(&sheet).unwrap();
    let mut duplicate = raw.clone();
    duplicate["tabs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"sHeEt1"}));
    assert!(
        Sheet::check(&duplicate.to_string())
            .unwrap_err()
            .iter()
            .any(|e| e.message.contains("duplicate"))
    );
    for (key, value) in [
        ("XFE1", json!({"input":"bad"})),
        ("A0", json!({"input":"bad"})),
        ("A1", json!({"input":"x".repeat(32768)})),
        ("A1", json!({"input":"x","unknown":true})),
        ("A1", json!({"fmt":{"color":"not-a-token"}})),
        ("A1", json!({"fmt":{"num":""}})),
    ] {
        let mut bad = raw.clone();
        bad["tabs"][0]["cells"] = json!({key: value});
        let errors = Sheet::check(&bad.to_string()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.path.contains("Sheet1") && e.path.contains(key)),
            "{errors:?}"
        );
    }
    let mut bad = raw.clone();
    bad["tabs"][0]["merges"] = json!(["A1:B2", "B2:C3"]);
    assert!(
        Sheet::check(&bad.to_string())
            .unwrap_err()
            .iter()
            .any(|e| e.message.contains("overlapping"))
    );
    let mut bad = raw;
    bad["unexpected"] = json!(true);
    assert!(Sheet::check(&bad.to_string()).is_err());
    assert!(Sheet::check(&" ".repeat(validate::MAX_SOURCE + 1)).is_err());
}

#[test]
fn stable_ids_and_sparse_cells() {
    let mut sheet = blank();
    sheet.tabs.push(Tab::new("Data"));
    sheet.tabs.push(Tab::new("Other"));
    sheet.tabs[2].id = "t1".into();
    sheet.tabs[0].cells.insert("A1".into(), Cell::default());
    let sheet = checked(&sheet);
    assert!(sheet.tabs[0].cells.is_empty());
    assert_ne!(sheet.tabs[0].id, sheet.tabs[1].id);
    assert_ne!(sheet.tabs[0].id, sheet.tabs[2].id);
    assert_eq!(checked(&sheet), sheet);
    assert_eq!(sheet.revision(), sheet.clone().revision());
    for name in [
        "",
        " ",
        "bad/name",
        "bad:name",
        "bad[tab]",
        "12345678901234567890123456789012",
    ] {
        let mut bad = sheet.clone();
        bad.tabs[0].name = name.into();
        assert!(Sheet::check(&serde_json::to_string(&bad).unwrap()).is_err());
    }
}

#[test]
fn calculates_formulas_errors_cycles_and_formats() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"add_tab","name":"Data 2","after":null},
            {"op":"set_cells","tab":"Data 2","at":"B2","values":[["17"]]},
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["1"],["2"],["3"]]},
            {"op":"set_cells","tab":"Sheet1","at":"B1","values":[["=SUM(A1:A3)","='Data 2'!B2","=1/0","0.5"]]},
            {"op":"set_cells","tab":"Sheet1","at":"F1","values":[["=G1","=F1"]]},
            {"op":"format","tab":"Sheet1","range":"E1","patch":{"num":"0%"}}
        ]),
    );
    let workbook = Workbook::load(&sheet).unwrap();
    assert_eq!(workbook.value("Sheet1", "B1").kind, ValueKind::Number(6.0));
    assert_eq!(workbook.value("Sheet1", "C1").kind, ValueKind::Number(17.0));
    assert_eq!(
        workbook.value("Sheet1", "D1").kind,
        ValueKind::Error("#DIV/0!".into())
    );
    assert!(matches!(
        workbook.value("Sheet1", "F1").kind,
        ValueKind::Error(_)
    ));
    assert!(matches!(
        workbook.value("Sheet1", "G1").kind,
        ValueKind::Error(_)
    ));
    assert_eq!(workbook.value("Sheet1", "E1").display, "50%");
    assert_eq!(workbook.value("Sheet1", "H1").kind, ValueKind::Empty);
    assert_eq!(workbook.to_sheet(&sheet), sheet);
}

#[test]
fn inputs_stay_typed_and_computed_spills_are_not_serialized() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["00042","'001","TRUE","=SEQUENCE(2)"]]},
            {"op":"format","tab":"Sheet1","range":"A1","patch":{"num":"yyyy-mm-dd"}}
        ]),
    );
    let workbook = Workbook::load(&sheet).unwrap();
    let result = workbook.to_sheet(&sheet);
    assert_eq!(source(&result, 0, "A1"), "00042");
    assert_eq!(source(&result, 0, "B1"), "'001");
    assert_eq!(source(&result, 0, "C1"), "TRUE");
    assert!(!result.tabs[0].cells.contains_key("D2"));
    assert_eq!(workbook.value("Sheet1", "C1").kind, ValueKind::Bool(true));
    assert_eq!(
        workbook.value("Sheet1", "B1").kind,
        ValueKind::Text("001".into())
    );
}

#[test]
fn block_placement_clear_and_merge_patches() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"B2","values":[["1","2"],["3","4"]]},
            {"op":"format","tab":"Sheet1","range":"B2:C3","patch":{"bold":true,"num":"0.00"}},
            {"op":"set_cells","tab":"Sheet1","at":"B2","values":[[""]]},
            {"op":"format","tab":"Sheet1","range":"C2","patch":{"num":null}},
            {"op":"clear","tab":"Sheet1","range":"3:3","what":"inputs"}
        ]),
    );
    assert_eq!(source(&sheet, 0, "C2"), "2");
    assert_eq!(source(&sheet, 0, "B2"), "");
    assert!(sheet.tabs[0].cells["B2"].fmt.bold);
    assert_eq!(sheet.tabs[0].cells["C2"].fmt.num, None);
    assert!(sheet.tabs[0].cells["B3"].fmt.bold);
    let cleared = edits(
        &sheet,
        json!([
            {"op":"clear","tab":"Sheet1","range":"B:B","what":"formats"},
            {"op":"clear","tab":"Sheet1","range":"C2:C3","what":"all"}
        ]),
    );
    assert!(cleared.tabs[0].cells.is_empty());
}

#[test]
fn structural_edits_rewrite_references_and_shift_styles_and_geometry() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["=A5"]]},
            {"op":"set_cells","tab":"Sheet1","at":"A5","values":[["0005"]]},
            {"op":"format","tab":"Sheet1","range":"A5","patch":{"bold":true,"fill":"accent"}},
            {"op":"set_row_height","tab":"Sheet1","rows":"5:5","height":33},
            {"op":"freeze","tab":"Sheet1","rows":5,"cols":1},
            {"op":"merge","tab":"Sheet1","range":"B5:C5"},
            {"op":"insert_rows","tab":"Sheet1","at":3,"count":1}
        ]),
    );
    assert_eq!(source(&sheet, 0, "A1"), "=A6");
    assert_eq!(source(&sheet, 0, "A6"), "0005");
    assert!(sheet.tabs[0].cells["A6"].fmt.bold);
    assert_eq!(
        sheet.tabs[0].cells["A6"].fmt.fill.as_deref(),
        Some("accent")
    );
    assert_eq!(sheet.tabs[0].rows.get(&6), Some(&33.0));
    assert_eq!(sheet.tabs[0].frozen.rows, 6);
    assert_eq!(sheet.tabs[0].merges, ["B6:C6"]);
    let result = edits(
        &sheet,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"D1","values":[["=B6"]]},
            {"op":"delete_cols","tab":"Sheet1","at":"B","count":1}
        ]),
    );
    assert!(source(&result, 0, "C1").contains("#REF!"));
    assert!(matches!(
        Workbook::load(&result).unwrap().value("Sheet1", "C1").kind,
        ValueKind::Error(_)
    ));
}

#[test]
fn column_insertion_and_row_deletion_follow_references() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["=C3"]]},
            {"op":"set_cells","tab":"Sheet1","at":"C3","values":[["9"]]},
            {"op":"set_col_width","tab":"Sheet1","cols":"C:C","width":120},
            {"op":"insert_cols","tab":"Sheet1","at":"B","count":1}
        ]),
    );
    assert_eq!(source(&sheet, 0, "A1"), "=D3");
    assert_eq!(sheet.tabs[0].cols.get("D"), Some(&120.0));
    let sheet = edits(
        &sheet,
        json!([{"op":"delete_rows","tab":"Sheet1","at":3,"count":1}]),
    );
    assert!(source(&sheet, 0, "A1").contains("#REF!"));
}

#[test]
fn renaming_moving_and_deleting_tabs_preserve_identity_and_references() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"add_tab","name":"Data 2","after":null},
            {"op":"set_cells","tab":"Data 2","at":"B2","values":[["7"]]},
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["='Data 2'!B2"]]}
        ]),
    );
    let id = sheet.tabs[1].id.clone();
    let sheet = edits(
        &sheet,
        json!([
            {"op":"rename_tab","tab":"Data 2","name":"Renamed"},
            {"op":"move_tab","tab":"Sheet1","after":"Renamed"}
        ]),
    );
    assert_eq!(sheet.tabs[0].id, id);
    assert!(source(&sheet, 1, "A1").contains("Renamed!B2"));
    assert_eq!(
        Workbook::load(&sheet).unwrap().value("Sheet1", "A1").kind,
        ValueKind::Number(7.0)
    );
    let sheet = edits(&sheet, json!([{"op":"delete_tab","tab":"Renamed"}]));
    assert!(matches!(
        Workbook::load(&sheet).unwrap().value("Sheet1", "A1").kind,
        ValueKind::Error(_)
    ));
    assert!(
        apply(
            &sheet,
            &[Op::DeleteTab {
                tab: "Sheet1".into()
            }]
        )
        .is_err()
    );
}

#[test]
fn sort_is_stable_with_header_mixed_values_and_copied_formulas() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[
                ["id","key","calc"],
                ["three","3","=B2*10"],
                ["zee","z",""],
                ["one-a","1",""],
                ["blank-a","",""],
                ["alpha","a",""],
                ["one-b","1",""],
                ["blank-b","",""],
                ["two","=1+1",""]
            ]},
            {"op":"set_cells","tab":"Sheet1","at":"E1","values":[["=B2"]]},
            {"op":"format","tab":"Sheet1","range":"A2","patch":{"italic":true}},
            {"op":"sort","tab":"Sheet1","range":"A1:C9","by":"B","desc":false,"header":true}
        ]),
    );
    let ids: Vec<_> = (1..=9)
        .map(|row| source(&sheet, 0, &format!("A{row}")))
        .collect();
    assert_eq!(
        ids,
        [
            "id", "one-a", "one-b", "two", "three", "alpha", "zee", "blank-a", "blank-b"
        ]
    );
    assert!(sheet.tabs[0].cells["A5"].fmt.italic);
    assert_eq!(source(&sheet, 0, "C5"), "=B5*10");
    assert_eq!(source(&sheet, 0, "E1"), "=B2");
    assert_eq!(
        Workbook::load(&sheet).unwrap().value("Sheet1", "C5").kind,
        ValueKind::Number(30.0)
    );
    let desc = edits(
        &sheet,
        json!([{"op":"sort","tab":"Sheet1","range":"A1:C9","by":"B","desc":true,"header":true}]),
    );
    assert_eq!(source(&desc, 0, "A2"), "three");
    assert_eq!(source(&desc, 0, "A5"), "one-b");
    assert_eq!(source(&desc, 0, "A8"), "blank-a");
}

#[test]
fn sort_keeps_external_addresses_and_copies_relative_and_absolute_references() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[
                ["3","=A1*10"],
                ["1","=A2*10"],
                ["2","=A3*10"]
            ]},
            {"op":"set_cells","tab":"Sheet1","at":"D1","values":[["=A1"]]},
            {"op":"sort","tab":"Sheet1","range":"A1:B3","by":"A","desc":false,"header":false}
        ]),
    );
    let workbook = Workbook::load(&sheet).unwrap();
    assert_eq!(source(&sheet, 0, "D1"), "=A1");
    assert_eq!(workbook.value("Sheet1", "D1").kind, ValueKind::Number(1.0));
    for row in 1..=3 {
        assert_eq!(source(&sheet, 0, &format!("A{row}")), row.to_string());
        assert_eq!(source(&sheet, 0, &format!("B{row}")), format!("=A{row}*10"));
        assert_eq!(
            workbook.value("Sheet1", &format!("B{row}")).kind,
            ValueKind::Number(f64::from(row) * 10.0)
        );
    }

    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[
                ["0003","=$A$3"],
                ["01","=A2*10"],
                ["002","=A4"],
                ["4",""]
            ]},
            {"op":"sort","tab":"Sheet1","range":"A1:B4","by":"A","desc":false,"header":false}
        ]),
    );
    let inputs: Vec<_> = (1..=4)
        .map(|row| source(&sheet, 0, &format!("A{row}")))
        .collect();
    assert_eq!(inputs, ["01", "002", "0003", "4"]);
    assert_eq!(source(&sheet, 0, "B1"), "=A1*10");
    assert_eq!(source(&sheet, 0, "B2"), "=A3");
    assert_eq!(source(&sheet, 0, "B3"), "=$A$3");
    let workbook = Workbook::load(&sheet).unwrap();
    assert_eq!(workbook.value("Sheet1", "B2").kind, ValueKind::Number(3.0));
    assert_eq!(workbook.value("Sheet1", "B3").kind, ValueKind::Number(3.0));
}

#[test]
fn edits_are_atomic_and_strict() {
    let sheet = blank();
    let before = sheet.revision();
    let ops: Vec<Op> = serde_json::from_value(json!([
        {"op":"set_title","title":"Changed"},
        {"op":"set_cells","tab":"Sheet1","at":"A0","values":[["1"]]}
    ]))
    .unwrap();
    assert!(apply(&sheet, &ops).is_err());
    assert_eq!(sheet.revision(), before);
    assert_eq!(sheet.title, "Budget");
    assert!(
        serde_json::from_value::<Op>(json!({"op":"set_title","title":"x","extra":true})).is_err()
    );
    let changed = edits(
        &sheet,
        json!([
            {"op":"apply_design","design":null},
            {"op":"merge","tab":"Sheet1","range":"A1:B2"},
            {"op":"unmerge","tab":"Sheet1","range":"B2"}
        ]),
    );
    assert!(changed.tabs[0].merges.is_empty());
}

#[test]
fn read_helpers_show_formulas_values_and_bounded_omissions() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["Name","Total"],["Ada","=2+3"],["Bo","7"],["Cy","9"]]}
        ]),
    );
    let workbook = Workbook::load(&sheet).unwrap();
    let values = read_range(&workbook, &sheet, "Sheet1", "A1:B4", Show::Values, 1000);
    assert_eq!(
        values,
        "\tA\tB\n1\tName\tTotal\n2\tAda\t5\n3\tBo\t7\n4\tCy\t9\n"
    );
    let formulas = read_range(&workbook, &sheet, "Sheet1", "A1:B4", Show::Formulas, 1000);
    assert!(formulas.contains("2\tAda\t=2+3"));
    let short = read_range(&workbook, &sheet, "Sheet1", "A1:B4", Show::Values, 43);
    assert!(short.contains("more: rows "));
    assert!(short.chars().count() <= 43);
    let text = outline(&sheet, 1000);
    assert!(text.contains("Sheet1") && text.contains("A1:B4") && text.contains("Name | Total"));
    for cap in 0..50 {
        assert!(outline(&sheet, cap).chars().count() <= cap);
        assert!(
            read_range(&workbook, &sheet, "Sheet1", "A1:B4", Show::Values, cap)
                .chars()
                .count()
                <= cap
        );
    }
}

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("dugix-sheet-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn confined_storage_and_shared_history() {
    let dir = Temp::new();
    for path in [
        "docs/x.doc.json",
        "sheets/../x.sheet.json",
        "sheets/.hidden/x.sheet.json",
    ] {
        assert!(storage::confined(&dir.0, path).is_err());
    }
    let sheet = blank();
    storage::save(&dir.0, "sheets/x.sheet.json", &sheet, Who::Model).unwrap();
    assert_eq!(storage::load(&dir.0, "sheets/x.sheet.json").unwrap(), sheet);
    let changed = edits(&sheet, json!([{"op":"set_title","title":"New"}]));
    storage::save(&dir.0, "sheets/x.sheet.json", &changed, Who::User).unwrap();
    let path = dir.0.join("sheets/x.sheet.json");
    let history = history::list(&path).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].load_for::<Sheet>().unwrap(), sheet);
    assert_eq!(
        storage::load(&dir.0, "sheets/x.sheet.json").unwrap(),
        changed
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_sheets_directory() {
    let dir = Temp::new();
    let target = Temp::new();
    std::os::unix::fs::symlink(&target.0, dir.0.join("sheets")).unwrap();
    assert!(storage::confined(&dir.0, "sheets/x.sheet.json").is_err());
}

#[test]
fn csv_quotes_commas_newlines_and_formulas() {
    let csv = b"name,notes,value\n\"A, B\",\"said \"\"hi\"\"\",42\nC,\"first\nsecond\",0.5\n";
    let sheet = interchange::import_csv(csv, "[data].csv", &Design::default()).unwrap();
    assert_eq!(sheet.tabs[0].name, "_data_");
    assert_eq!(source(&sheet, 0, "B3"), "first\nsecond");
    assert_eq!(interchange::export_csv(&sheet, "_data_").unwrap(), csv);
    let formula =
        interchange::import_csv(b"1,2,=SUM(A1:B1)\n", "calc.csv", &Design::default()).unwrap();
    assert_eq!(source(&formula, 0, "C1"), "=SUM(A1:B1)");
    assert_eq!(
        interchange::export_csv(&formula, "calc").unwrap(),
        b"1,2,3\n"
    );
}

#[test]
fn xlsx_roundtrip_keeps_inputs_formulas_styles_and_geometry() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["0.5","=A1*2","hello"]]},
            {"op":"format","tab":"Sheet1","range":"A1","patch":{"num":"0%","bold":true,"italic":true,"underline":true,"fill":"accent","color":"#123456","align":"right"}},
            {"op":"set_col_width","tab":"Sheet1","cols":"A:A","width":144},
            {"op":"set_row_height","tab":"Sheet1","rows":"1:1","height":32},
            {"op":"freeze","tab":"Sheet1","rows":1,"cols":1},
            {"op":"merge","tab":"Sheet1","range":"A3:B3"},
            {"op":"add_tab","name":"Data 2","after":null},
            {"op":"set_cells","tab":"Data 2","at":"A1","values":[["=Sheet1!B1"]]}
        ]),
    );
    let bytes = interchange::export_xlsx(&sheet).unwrap();
    assert!(bytes.starts_with(b"PK"));
    let imported = interchange::import_xlsx(&bytes, "Imported").unwrap();
    assert_eq!(
        imported
            .tabs
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["Sheet1", "Data 2"]
    );
    assert_eq!(source(&imported, 0, "A1"), "0.5");
    assert_eq!(source(&imported, 0, "B1"), "=A1*2");
    assert_eq!(source(&imported, 0, "C1"), "hello");
    let fmt = &imported.tabs[0].cells["A1"].fmt;
    assert_eq!(fmt.num.as_deref(), Some("0%"));
    assert!(fmt.bold && fmt.italic && fmt.underline);
    assert_eq!(fmt.align, Some(Align::Right));
    assert_eq!(
        fmt.fill.as_deref(),
        sheet.theme.colors.get("accent").map(String::as_str)
    );
    assert_eq!(fmt.color.as_deref(), Some("#123456"));
    assert!((imported.tabs[0].cols["A"] - 144.0).abs() < 0.01);
    assert!((imported.tabs[0].rows[&1] - 32.0).abs() < 0.01);
    assert_eq!(imported.tabs[0].frozen, Frozen { rows: 1, cols: 1 });
    assert_eq!(imported.tabs[0].merges, ["A3:B3"]);
    assert_eq!(
        Workbook::load(&imported)
            .unwrap()
            .value("Data 2", "A1")
            .kind,
        ValueKind::Number(1.0)
    );
    assert_eq!(checked(&imported), imported);
}

#[test]
fn tab_ranges_parse_excel_quotes_and_reject_bad_ranges() {
    use address::{Range, TabRange};
    for (text, tab, range) in [
        ("A1:F20", None, "A1:F20"),
        ("B:B", None, "B:B"),
        ("2:4", None, "2:4"),
        ("Data!A1:C9", Some("Data"), "A1:C9"),
        ("'Q3 plan'!A1:C9", Some("Q3 plan"), "A1:C9"),
        ("'O''Brien!'!B2", Some("O'Brien!"), "B2"),
    ] {
        let parsed: TabRange = text.parse().unwrap();
        assert_eq!(parsed.tab.as_deref(), tab);
        assert_eq!(parsed.range, range.parse::<Range>().unwrap());
    }
    for text in [
        "",
        "!A1",
        "''!A1",
        "'Plan!A1",
        "'Plan'A1",
        "Data!",
        "Q3 plan!A1",
        "O'Brien!A1",
        "Data!C9:A1",
        "Data!A1!B2",
    ] {
        assert!(text.parse::<TabRange>().is_err(), "{text}");
    }
}

#[test]
fn sheet_both_read_shows_formula_and_display_within_budget() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A2","values":[["=B2*10","3","plain"]]}
        ]),
    );
    let workbook = Workbook::load(&sheet).unwrap();
    let text = read_range(&workbook, &sheet, "Sheet1", "A2:C2", Show::Both, 200);
    assert!(text.contains("=B2*10 → 30\t3\tplain"), "{text}");
    let small = read_range(&workbook, &sheet, "Sheet1", "A1:C100", Show::Both, 60);
    assert!(small.chars().count() <= 60);
    assert!(small.contains("more: rows"));
}

#[test]
fn wrap_and_valign_validate_round_trip_and_export_as_xlsx_alignment() {
    let sheet = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["long text here","clip me"]]},
            {"op":"format","tab":"Sheet1","range":"A1","patch":{"wrap":"wrap","valign":"top"}},
            {"op":"format","tab":"Sheet1","range":"B1","patch":{"wrap":"clip","valign":"middle"}}
        ]),
    );
    assert_eq!(sheet.tabs[0].cells["A1"].fmt.wrap, Some(Wrap::Wrap));
    assert_eq!(sheet.tabs[0].cells["A1"].fmt.valign, Some(VAlign::Top));
    assert_eq!(sheet.tabs[0].cells["B1"].fmt.wrap, Some(Wrap::Clip));
    assert_eq!(sheet.tabs[0].cells["B1"].fmt.valign, Some(VAlign::Middle));
    assert_eq!(checked(&sheet), sheet);
    for (field, value) in [("wrap", "sideways"), ("valign", "sideways")] {
        let mut bad = serde_json::to_value(&sheet).unwrap();
        bad["tabs"][0]["cells"]["A1"]["fmt"][field] = json!(value);
        let errors = Sheet::check(&bad.to_string()).unwrap_err();
        assert!(errors.iter().any(|e| e.path.contains("A1")), "{errors:?}");
    }
    // XLSX: wrap becomes wrapText, and clip exports as no wrap.
    let imported =
        interchange::import_xlsx(&interchange::export_xlsx(&sheet).unwrap(), "Imported").unwrap();
    assert_eq!(imported.tabs[0].cells["A1"].fmt.wrap, Some(Wrap::Wrap));
    assert_eq!(imported.tabs[0].cells["A1"].fmt.valign, Some(VAlign::Top));
    assert_eq!(imported.tabs[0].cells["B1"].fmt.wrap, None);
    assert_eq!(
        imported.tabs[0].cells["B1"].fmt.valign,
        Some(VAlign::Middle)
    );
}

#[test]
fn fit_cols_measures_values_bold_and_wrapped_words_within_limits() {
    let base = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["mmmm"]]},
            {"op":"set_cells","tab":"Sheet1","at":"B1","values":[["mmmm"]]},
            {"op":"format","tab":"Sheet1","range":"B1","patch":{"bold":true}},
            {"op":"set_cells","tab":"Sheet1","at":"C1","values":[[
                "a value far too wide to fit any column that a sheet would ever show, \
                even on a very large display with plenty of room to spare for it"
            ]]},
            {"op":"set_cells","tab":"Sheet1","at":"E1","values":[["wrap this phrase please"]]},
            {"op":"format","tab":"Sheet1","range":"E1","patch":{"wrap":"wrap"}},
            {"op":"set_cells","tab":"Sheet1","at":"F1","values":[["wrap this phrase please"]]}
        ]),
    );
    let fitted = edits(
        &base,
        json!([{"op":"fit_cols","tab":"Sheet1","cols":"A:F"}]),
    );
    let width = |col: &str| fitted.tabs[0].cols[col];
    assert!(width("B") > width("A"), "bold is measured as bold");
    assert_eq!(width("C"), layout::MAX_FIT, "clamped to the widest");
    assert_eq!(
        width("D"),
        layout::DEFAULT_COL,
        "a column with no data keeps the default"
    );
    assert_eq!(
        width("E"),
        width("F"),
        "a wrapped cell fits its whole text unwrapped, so fitting makes it readable"
    );
    assert!((layout::MIN_FIT..layout::MAX_FIT).contains(&width("A")));
}

#[test]
fn fit_rows_drops_set_heights_so_wrapped_rows_grow_again() {
    let wrapped = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["a sentence long enough to need several lines in a narrow cell"],["short"],["x"]]},
            {"op":"format","tab":"Sheet1","range":"A1:A2","patch":{"wrap":"wrap"}},
            {"op":"set_row_height","tab":"Sheet1","rows":"1:3","height":30}
        ]),
    );
    let workbook = |sheet: &Sheet| Workbook::load(sheet).unwrap();
    assert!(
        layout::auto_row_heights(&wrapped, &workbook(&wrapped), &wrapped.tabs[0]).is_empty(),
        "a set height wins over wrapping"
    );
    let fitted = edits(
        &wrapped,
        json!([{"op":"fit_rows","tab":"Sheet1","rows":"1:2"}]),
    );
    assert_eq!(
        fitted.tabs[0].rows.keys().copied().collect::<Vec<_>>(),
        vec![3],
        "only the fitted rows lose their set height"
    );
    let auto = layout::auto_row_heights(&fitted, &workbook(&fitted), &fitted.tabs[0]);
    assert!(
        auto[&1] > layout::DEFAULT_ROW,
        "the wrapped row grows to its lines"
    );
    assert!(
        !auto.contains_key(&2),
        "one short line keeps the default height"
    );
    assert!(
        apply(
            &wrapped,
            &serde_json::from_value::<Vec<Op>>(
                json!([{"op":"fit_rows","tab":"Sheet1","rows":"A:B"}])
            )
            .unwrap()
        )
        .is_err(),
        "rows must be a row range"
    );
}

#[test]
fn auto_row_heights_grow_with_text_and_narrow_columns_and_never_override_user_heights() {
    let text = "one two three four five six seven eight nine ten eleven twelve";
    let base = edits(
        &blank(),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[[text]]},
            {"op":"format","tab":"Sheet1","range":"A1","patch":{"wrap":"wrap"}},
            {"op":"set_cells","tab":"Sheet1","at":"A2","values":[[text]]},
            {"op":"format","tab":"Sheet1","range":"A2","patch":{"wrap":"wrap"}},
            {"op":"set_row_height","tab":"Sheet1","rows":"2:2","height":90},
            {"op":"set_cells","tab":"Sheet1","at":"A3","values":[["plain"]]}
        ]),
    );
    let workbook = Workbook::load(&base).unwrap();
    let tall = layout::auto_row_heights(&base, &workbook, &base.tabs[0]);
    assert!(tall[&1] > layout::DEFAULT_ROW);
    assert!(!tall.contains_key(&2), "a user-set height wins");
    assert!(!tall.contains_key(&3), "an unwrapped row keeps the default");
    let narrow = edits(
        &base,
        json!([{"op":"set_col_width","tab":"Sheet1","cols":"A:A","width":60}]),
    );
    let workbook = Workbook::load(&narrow).unwrap();
    let taller = layout::auto_row_heights(&narrow, &workbook, &narrow.tabs[0]);
    assert!(
        taller[&1] > tall[&1],
        "a narrower column wraps into more lines"
    );
    let plain = edits(
        &base,
        json!([{"op":"format","tab":"Sheet1","range":"A1","patch":{"wrap":null}}]),
    );
    let workbook = Workbook::load(&plain).unwrap();
    assert!(
        !layout::auto_row_heights(&plain, &workbook, &plain.tabs[0]).contains_key(&1),
        "no longer wrapped: no derived height"
    );
}

#[test]
fn overflow_spans_stop_at_filled_cells_and_follow_alignment() {
    assert_eq!(
        layout::overflow_span(Align::Left, None, false, 1, 5, |c| c < 3),
        (1, 2)
    );
    assert_eq!(
        layout::overflow_span(Align::Left, None, false, 1, 5, |_| true),
        (1, 5)
    );
    assert_eq!(
        layout::overflow_span(Align::Right, None, false, 5, 5, |c| c < 4),
        (5, 5),
        "a filled cell stops the spill"
    );
    assert_eq!(
        layout::overflow_span(Align::Right, None, false, 5, 5, |_| true),
        (1, 5),
        "right-aligned text spills left"
    );
    assert_eq!(
        layout::overflow_span(Align::Center, None, false, 3, 5, |_| true),
        (1, 5),
        "centered text spills both ways"
    );
    assert_eq!(
        layout::overflow_span(Align::Center, None, false, 3, 5, |c| c != 2),
        (3, 5)
    );
    assert_eq!(
        layout::overflow_span(Align::Left, None, true, 1, 5, |_| true),
        (1, 1),
        "numbers never overflow"
    );
    for wrap in [Wrap::Wrap, Wrap::Clip] {
        assert_eq!(
            layout::overflow_span(Align::Left, Some(wrap), false, 1, 5, |_| true),
            (1, 1),
            "{wrap:?} never overflows"
        );
    }
}

#[test]
fn csv_import_fits_every_column_with_data() {
    let sheet =
        interchange::import_csv(b"a,bb\nlonger value,x\n", "fit.csv", &Design::default()).unwrap();
    let cols = &sheet.tabs[0].cols;
    assert!(cols["A"] > cols["B"], "the widest value wins");
    assert!(cols["B"] >= layout::MIN_FIT);
    assert!(
        !cols.contains_key("C"),
        "a column with no data keeps the default"
    );
}
