use super::*;
use serde_json::{Value as Json, json};

fn ops(sheet: &Sheet, raw: Json) -> Sheet {
    apply(sheet, &serde_json::from_value::<Vec<Op>>(raw).unwrap()).unwrap()
}

fn sample() -> Sheet {
    ops(
        &Sheet::from_design("Grid", &crate::design::Design::default()),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["1","=A1+1","=A1"]]},
            {"op":"format","tab":"Sheet1","range":"A1:B1","patch":{"bold":true}},
            {"op":"add_tab","name":"Other"}
        ]),
    )
}

#[test]
fn sheet_copy_and_cut_shift_formulas_but_only_cut_moves_observers() {
    for cut in [false, true] {
        let sheet = ops(
            &sample(),
            json!([{"op":"copy","tab":"Sheet1","from":"A1:B1","to":"A3","cut":cut}]),
        );
        let book = Workbook::load(&sheet).unwrap();
        assert_eq!(book.value("Sheet1", "A3").display, "1");
        assert_eq!(book.value("Sheet1", "B3").display, "2");
        assert!(sheet.tabs[0].cells["A3"].fmt.bold);
        assert_eq!(sheet.tabs[0].cells["B3"].input, "=A3+1");
        assert_eq!(
            sheet.tabs[0].cells["C1"].input,
            if cut { "=A3" } else { "=A1" }
        );
        assert_eq!(sheet.tabs[0].cells.contains_key("A1"), !cut);
    }
}

#[test]
fn sheet_copy_and_cut_across_tabs_keep_formats_and_reference_semantics() {
    for cut in [false, true] {
        let sheet = ops(
            &sample(),
            json!([{"op":"copy","tab":"Sheet1","from":"A1:B1","to_tab":"Other","to":"D4","cut":cut}]),
        );
        let book = Workbook::load(&sheet).unwrap();
        assert_eq!(book.value("Other", "D4").display, "1");
        assert_eq!(book.value("Other", "E4").display, "2");
        assert_eq!(book.value("Sheet1", "C1").display, "1");
        assert!(sheet.tabs[1].cells["D4"].fmt.bold);
        assert_eq!(sheet.tabs[0].cells.contains_key("A1"), !cut);
        assert_eq!(sheet.tabs[0].cells["C1"].input.contains("Other!D4"), cut);
    }
}

#[test]
fn sheet_fill_shifts_formulas_down_and_right_and_repeats_text() {
    let sheet = ops(
        &sample(),
        json!([
            {"op":"fill","tab":"Sheet1","from":"B1","to":"B1:B4"},
            {"op":"fill","tab":"Sheet1","from":"B4","to":"B4:D4"},
            {"op":"set_cells","tab":"Other","at":"A1","values":[["Monday"],["hello"]]},
            {"op":"fill","tab":"Other","from":"A1:A2","to":"A1:A5"}
        ]),
    );
    assert_eq!(sheet.tabs[0].cells["B4"].input, "=A4+1");
    assert_eq!(sheet.tabs[0].cells["D4"].input, "=C4+1");
    assert_eq!(sheet.tabs[1].cells["A3"].input, "Monday");
    assert_eq!(sheet.tabs[1].cells["A4"].input, "hello");
    assert_eq!(sheet.tabs[1].cells["A5"].input, "Monday");
}

#[test]
fn sheet_fill_continues_constant_numeric_series() {
    let sheet = ops(
        &sample(),
        json!([
            {"op":"set_cells","tab":"Other","at":"A1","values":[["1","10","20"],["2","",""]]},
            {"op":"fill","tab":"Other","from":"A1:A2","to":"A1:A5"},
            {"op":"fill","tab":"Other","from":"B1:C1","to":"B1:F1"}
        ]),
    );
    let book = Workbook::load(&sheet).unwrap();
    assert_eq!(book.value("Other", "A5").display, "5");
    assert_eq!(book.value("Other", "F1").display, "50");
    assert!(
        apply(
            &sheet,
            &serde_json::from_value::<Vec<Op>>(json!([
                {"op":"fill","tab":"Other","from":"B2","to":"A1:C3"}
            ]))
            .unwrap()
        )
        .is_err()
    );
}

fn thread(sheet: &Sheet, range: &str) -> feedback::Thread {
    feedback::Thread {
        id: "thread".into(),
        anchor: feedback::Anchor::new(sheet, "Sheet1", range).unwrap(),
        orphaned: false,
        severity: feedback::Severity::Consider,
        created: 0,
        resolved: None,
        messages: vec![],
    }
}

#[test]
fn sheet_anchors_shift_shrink_orphan_and_move_on_cut() {
    let before = sample();
    let mut threads = vec![thread(&before, "B4:C9")];
    let edits: Vec<Op> = serde_json::from_value(json!([
        {"op":"insert_rows","tab":"Sheet1","at":2,"count":2},
        {"op":"insert_cols","tab":"Sheet1","at":"A","count":1},
        {"op":"delete_rows","tab":"Sheet1","at":8,"count":2},
        {"op":"delete_cols","tab":"Sheet1","at":"C","count":1}
    ]))
    .unwrap();
    apply(&before, &edits).unwrap();
    feedback::reanchor(&mut threads, &before, &edits);
    assert_eq!(threads[0].anchor.range, "C6:C9");
    assert!(!threads[0].orphaned);
    let anchor = threads[0].anchor.clone();
    feedback::reanchor(
        &mut threads,
        &before,
        &[Op::DeleteRows {
            tab: "Sheet1".into(),
            at: 6,
            count: 4,
        }],
    );
    assert!(threads[0].orphaned);
    assert_eq!(threads[0].anchor, anchor);
    let mut threads = vec![thread(&before, "B4")];
    feedback::reanchor(
        &mut threads,
        &before,
        &[Op::Copy {
            tab: "Sheet1".into(),
            from: "A3:C5".into(),
            to_tab: Some("Other".into()),
            to: "D7".into(),
            cut: true,
        }],
    );
    assert_eq!(threads[0].anchor.tab, before.tabs[1].id);
    assert_eq!(threads[0].anchor.range, "E8");
}
