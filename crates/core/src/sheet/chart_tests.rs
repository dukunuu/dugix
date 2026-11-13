use super::*;
use crate::design::Design;
use serde_json::{Value as Json, json};

fn edits(sheet: &Sheet, ops: Json) -> Sheet {
    apply(sheet, &serde_json::from_value::<Vec<Op>>(ops).unwrap()).unwrap()
}
fn sample() -> Sheet {
    edits(
        &Sheet::from_design("Charts", &Design::default()),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["Month","Sales","Costs"],["Jan","10","-2"],["Feb","=B2*2","3"]]},
            {"op":"add_chart","tab":"Sheet1","data":"A1:C3"}
        ]),
    )
}
fn data(sheet: &Sheet) -> chart::Data {
    chart::extract(
        sheet,
        &Workbook::load(sheet).unwrap(),
        &sheet.tabs[0],
        &sheet.tabs[0].charts[0],
    )
}

#[test]
fn sheet_chart_validation_round_trip_and_atomic_edits() {
    let sheet = sample();
    let chart = &sheet.tabs[0].charts[0];
    assert_eq!(
        (chart.id.as_str(), chart.at.as_str(), chart.kind),
        ("c1", "E1", ChartKind::Column)
    );
    assert_eq!((chart.w, chart.h), (480.0, 296.0));
    assert_eq!(
        Sheet::check(&serde_json::to_string(&sheet).unwrap()).unwrap(),
        sheet
    );
    for patch in [
        json!({"kind":"bad"}),
        json!({"data":"A0"}),
        json!({"data":"Missing!A1"}),
        json!({"w":159}),
        json!({"h":2001}),
        json!({"dx":-1}),
        json!({"kind":"line","stacked":true}),
        json!({"y_min":4,"y_max":2}),
        json!({"unknown":true}),
    ] {
        assert!(
            apply(
                &sheet,
                &serde_json::from_value::<Vec<Op>>(
                    json!([{"op":"edit_chart","tab":"Sheet1","id":"c1","patch":patch}])
                )
                .unwrap()
            )
            .is_err()
        );
    }
    let mut duplicate = sheet.clone();
    duplicate.tabs[0].charts.push(chart.clone());
    assert!(Sheet::check(&serde_json::to_string(&duplicate).unwrap()).is_err());
    let changed = edits(
        &sheet,
        json!([{"op":"edit_chart","tab":"Sheet1","id":"c1","patch":{"title":"Revenue","kind":"bar","w":600,"dx":8}}]),
    );
    assert_eq!(changed.tabs[0].charts[0].title.as_deref(), Some("Revenue"));
    assert_eq!(changed.tabs[0].charts[0].kind, ChartKind::Bar);
    let reset = edits(
        &changed,
        json!([{"op":"edit_chart","tab":"Sheet1","id":"c1","patch":{"title":null,"w":null}}]),
    );
    assert_eq!(reset.tabs[0].charts[0].title, None);
    assert_eq!(reset.tabs[0].charts[0].w, 480.0);
    let deleted = edits(
        &sheet,
        json!([{"op":"delete_chart","tab":"Sheet1","id":"c1"}]),
    );
    assert!(deleted.tabs[0].charts.is_empty());
    assert!(!serde_json::to_string(&deleted).unwrap().contains("charts"));
    assert!(interchange::export_xlsx(&sheet).unwrap().starts_with(b"PK"));
}

#[test]
fn sheet_chart_defaults_dates_and_many_categories() {
    let base = Sheet::from_design("Charts", &Design::default());
    let dates = edits(
        &base,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"B4","values":[["Date","N"],["45000","1"],["45001","2"]]},
            {"op":"format","tab":"Sheet1","range":"B5:B6","patch":{"num":"yyyy-mm-dd"}},
            {"op":"add_chart","tab":"Sheet1","data":"B4:C6"}
        ]),
    );
    assert_eq!(dates.tabs[0].charts[0].kind, ChartKind::Line);
    assert_eq!(dates.tabs[0].charts[0].at, "E4");
    assert_eq!(data(&dates).labels, ["2023-03-15", "2023-03-16"]);
    let long = edits(
        &base,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":(1..=13).map(|n| vec![n.to_string()]).collect::<Vec<_>>()},
            {"op":"add_chart","tab":"Sheet1","data":"A1:A13"}
        ]),
    );
    assert_eq!(long.tabs[0].charts[0].kind, ChartKind::Line);
    assert_eq!(data(&long).labels[0], "1");
    assert_eq!(data(&long).series[0].values[12], Some(13.0));
}

#[test]
fn sheet_chart_extraction_headers_directions_gaps_formats_and_caps() {
    let sheet = sample();
    let extracted = data(&sheet);
    assert_eq!(extracted.labels, ["Jan", "Feb"]);
    assert_eq!(extracted.series[0].name, "Sales");
    assert_eq!(extracted.series[0].values, [Some(10.0), Some(20.0)]);
    for headers in ["yes", "no"] {
        let next = edits(
            &sheet,
            json!([{"op":"edit_chart","tab":"Sheet1","id":"c1","patch":{"headers":headers}}]),
        );
        assert_eq!(
            data(&next).labels.len(),
            if headers == "yes" { 2 } else { 3 }
        );
    }
    let rows = edits(
        &Sheet::from_design("Rows", &Design::default()),
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["Month","Jan","Feb"],["Sales","10","20"],["Costs","-2","3"]]},
            {"op":"add_chart","tab":"Sheet1","data":"A1:C3","series_in":"rows"}
        ]),
    );
    assert_eq!(data(&rows), extracted);
    let gaps = edits(
        &sheet,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"B2","values":[["word"],[""],["TRUE"],["=1/0"],["0.25"]]},
            {"op":"format","tab":"Sheet1","range":"B6","patch":{"num":"0.0%"}},
            {"op":"edit_chart","tab":"Sheet1","id":"c1","patch":{"data":"A1:B6"}}
        ]),
    );
    assert_eq!(
        data(&gaps).series[0].values,
        [None, None, None, None, Some(0.25)]
    );
    assert_eq!(data(&gaps).number_format.as_deref(), Some("0.0%"));
    let caps = edits(
        &sheet,
        json!([{"op":"edit_chart","tab":"Sheet1","id":"c1","patch":{"data":"A1:AZ3000"}}]),
    );
    let extracted = data(&caps);
    assert!(extracted.capped);
    assert_eq!(extracted.labels.len(), 2000);
    assert_eq!(extracted.series.len(), 24);
}

#[test]
fn sheet_chart_ranges_and_placements_follow_structural_edits() {
    let base = edits(
        &sample(),
        json!([{"op":"edit_chart","tab":"Sheet1","id":"c1","patch":{"data":"B3:D6","at":"F8"}}]),
    );
    for (op, range, at) in [
        (json!({"op":"insert_rows","at":2,"count":2}), "B5:D8", "F10"),
        (json!({"op":"insert_rows","at":4,"count":2}), "B3:D8", "F10"),
        (json!({"op":"insert_rows","at":10,"count":2}), "B3:D6", "F8"),
        (json!({"op":"delete_rows","at":1,"count":1}), "B2:D5", "F7"),
        (json!({"op":"delete_rows","at":4,"count":1}), "B3:D5", "F7"),
        (json!({"op":"delete_rows","at":10,"count":1}), "B3:D6", "F8"),
        (
            json!({"op":"insert_cols","at":"A","count":1}),
            "C3:E6",
            "G8",
        ),
        (
            json!({"op":"insert_cols","at":"C","count":1}),
            "B3:E6",
            "G8",
        ),
        (
            json!({"op":"insert_cols","at":"H","count":1}),
            "B3:D6",
            "F8",
        ),
        (
            json!({"op":"delete_cols","at":"A","count":1}),
            "A3:C6",
            "E8",
        ),
        (
            json!({"op":"delete_cols","at":"C","count":1}),
            "B3:C6",
            "E8",
        ),
        (
            json!({"op":"delete_cols","at":"H","count":1}),
            "B3:D6",
            "F8",
        ),
        (json!({"op":"delete_rows","at":3,"count":4}), "#REF!", "F4"),
        (
            json!({"op":"delete_cols","at":"B","count":3}),
            "#REF!",
            "C8",
        ),
        (json!({"op":"delete_rows","at":8,"count":2}), "B3:D6", "F8"),
    ] {
        let mut op = op;
        op["tab"] = json!("Sheet1");
        let next = edits(&base, json!([op]));
        assert_eq!(next.tabs[0].charts[0].data, range, "{op}");
        assert_eq!(next.tabs[0].charts[0].at, at, "{op}");
    }
}

#[test]
fn sheet_chart_whole_axes_track_only_their_own_dimension() {
    let base = edits(
        &sample(),
        json!([{"op":"edit_chart","tab":"Sheet1","id":"c1","patch":{"data":"A:C"}}]),
    );
    let next = edits(
        &base,
        json!([
            {"op":"insert_rows","tab":"Sheet1","at":2,"count":1},
            {"op":"insert_cols","tab":"Sheet1","at":"B","count":1}
        ]),
    );
    assert_eq!(next.tabs[0].charts[0].data, "A:D");
    let base = edits(
        &base,
        json!([{"op":"edit_chart","tab":"Sheet1","id":"c1","patch":{"data":"1:3"}}]),
    );
    let next = edits(
        &base,
        json!([
            {"op":"insert_cols","tab":"Sheet1","at":"B","count":1},
            {"op":"insert_rows","tab":"Sheet1","at":2,"count":1}
        ]),
    );
    assert_eq!(next.tabs[0].charts[0].data, "1:4");
}

#[test]
fn sheet_chart_cross_tab_rename_delete_cut_and_sort() {
    let base = edits(
        &sample(),
        json!([
            {"op":"add_tab","name":"Dashboard"},
            {"op":"add_chart","tab":"Dashboard","data":"Sheet1!A1:C3"}
        ]),
    );
    let inserted = edits(
        &base,
        json!([{"op":"insert_rows","tab":"Sheet1","at":2,"count":1}]),
    );
    assert!(inserted.tabs[1].charts[0].data.ends_with("!A1:C4"));
    let renamed = edits(
        &base,
        json!([{"op":"rename_tab","tab":"Sheet1","name":"It's data"}]),
    );
    assert_eq!(renamed.tabs[1].charts[0].data, "'It''s data'!A1:C3");
    let removed = edits(&base, json!([{"op":"delete_tab","tab":"Sheet1"}]));
    assert_eq!(removed.tabs[0].charts.len(), 1);
    assert_eq!(removed.tabs[0].charts[0].data, "#REF!");
    let moved = edits(
        &base,
        json!([{"op":"copy","tab":"Sheet1","from":"A1:C3","to_tab":"Dashboard","to":"D5","cut":true}]),
    );
    assert_eq!(moved.tabs[0].charts[0].data, "'Dashboard'!D5:F7");
    assert_eq!(moved.tabs[1].charts[0].data, "'Dashboard'!D5:F7");
    let copied = edits(
        &base,
        json!([{"op":"copy","tab":"Sheet1","from":"A1:C3","to":"D5","cut":false}]),
    );
    assert_eq!(copied.tabs[0].charts, base.tabs[0].charts);
    let sorted = edits(
        &base,
        json!([{"op":"sort","tab":"Sheet1","range":"A1:C3","by":"B","desc":true,"header":true}]),
    );
    assert_eq!(sorted.tabs[0].charts, base.tabs[0].charts);
}

#[test]
fn sheet_chart_svg_all_kinds_titles_negative_stacks_and_nice_ticks() {
    let sheet = sample();
    let workbook = Workbook::load(&sheet).unwrap();
    for kind in [
        ChartKind::Column,
        ChartKind::Bar,
        ChartKind::Line,
        ChartKind::Area,
        ChartKind::Pie,
        ChartKind::Scatter,
    ] {
        let mut chart = sheet.tabs[0].charts[0].clone();
        chart.kind = kind;
        // Scatter needs numeric x values.
        if kind == ChartKind::Scatter {
            chart.data = "B2:C3".into();
            chart.headers = Headers::No;
        }
        let svg = chart_svg::svg(&sheet, &workbook, &sheet.tabs[0], &chart);
        assert!(
            resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).is_ok(),
            "{kind:?}: {svg}"
        );
        assert!(svg.contains("<title>"), "{kind:?}");
        if kind != ChartKind::Pie {
            assert!(svg.contains("-2"), "negative value: {kind:?}");
        }
    }
    let mut chart = sheet.tabs[0].charts[0].clone();
    chart.stacked = true;
    assert_eq!(chart_svg::value_bounds(&data(&sheet), &chart), (-5.0, 25.0));
    for kind in [ChartKind::Column, ChartKind::Bar, ChartKind::Area] {
        chart.kind = kind;
        let svg = chart_svg::svg(&sheet, &workbook, &sheet.tabs[0], &chart);
        assert!(resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).is_ok());
        assert!(svg.matches("<title>").count() >= 4);
    }
    chart.data = "#REF!".into();
    assert!(chart_svg::svg(&sheet, &workbook, &sheet.tabs[0], &chart).contains("No data"));
    assert_eq!(
        chart_svg::nice_ticks(0.0, 9.0),
        [0.0, 2.0, 4.0, 6.0, 8.0, 10.0]
    );
    assert_eq!(
        chart_svg::nice_ticks(-3.0, 3.0),
        [-4.0, -2.0, 0.0, 2.0, 4.0]
    );
    assert_eq!(chart_svg::nice_ticks(0.0, 0.09).len(), 6);
}
