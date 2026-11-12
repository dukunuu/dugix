//! Hermetic sheet workflows through the public canvas tool.
use super::tests::Fixture;
use super::*;
use crate::sheet::{self, Sheet, ValueKind, Workbook};

#[tokio::test]
async fn sheets_comments_suggestions_replies_and_resolve_are_listed_on_read() {
    let f = Fixture::new();
    create(&f).await;
    edit(
        &f,
        json!([{"op":"set_cells","tab":"Sheet1","at":"B4","values":[["=2+3"]]}]),
    )
    .await;
    let result = call(&f, "comment", json!({"path":PATH,"range":"B4","text":"Check this\nMore detail", "ops":[{"op":"set_cells","tab":"Sheet1","at":"B4","values":[["6"]]}]})).await;
    assert!(result.contains("commented"), "{result}");
    let path = f.file(PATH);
    let threads = sheet::feedback::load(&path).unwrap();
    let thread = &threads[0];
    assert_eq!(thread.anchor.quote, "5");
    assert_eq!(thread.anchor.range, "B4");
    assert_eq!(thread.messages[0].edits.len(), 1);
    assert_eq!(load(&f).tabs[0].cells["B4"].input, "=2+3");
    let read = call(&f, "read", json!({"path":PATH})).await;
    assert!(
        read.contains(&thread.id) && read.contains("Sheet1!B4") && read.contains("Check this"),
        "{read}"
    );
    let reply = call(
        &f,
        "comment",
        json!({"path":PATH,"thread":thread.id,"text":"Reviewed"}),
    )
    .await;
    assert!(reply.contains("commented"), "{reply}");
    assert_eq!(sheet::feedback::load(&path).unwrap()[0].messages.len(), 2);
    let resolved = call(&f, "resolve", json!({"path":PATH,"thread":thread.id})).await;
    assert!(resolved.contains("resolved thread"), "{resolved}");
    assert!(sheet::feedback::load(&path).unwrap()[0].resolved.is_some());
    assert!(
        !call(&f, "read", json!({"path":PATH}))
            .await
            .contains(&thread.id)
    );
}

const PATH: &str = "sheets/q3-budget.sheet.json";

async fn call(f: &Fixture, action: &str, mut v: Value) -> String {
    v["action"] = json!(action);
    f.tools.run("canvas", &v.to_string()).await.0
}

fn load(f: &Fixture) -> Sheet {
    sheet::storage::load(&f.tools.space_files_dir, PATH).unwrap()
}

async fn create(f: &Fixture) -> String {
    call(f, "create", json!({"kind":"sheet","title":"Q3 Budget!"})).await
}

async fn edit(f: &Fixture, ops: Value) -> String {
    call(f, "edit", json!({"path":PATH,"ops":ops})).await
}

#[tokio::test]
async fn sheets_create_import_and_path_routing() {
    let f = Fixture::new();
    let created = create(&f).await;
    assert!(
        created.starts_with("created sheet · sheets/q3-budget.sheet.json"),
        "{created}"
    );
    assert_eq!(load(&f).title, "Q3 Budget!");
    assert!(created.contains("Sheet1 · empty"));
    assert!(created.contains("sheet schema"));
    assert!(super::sheets::SHEET_SCHEMA.chars().count() <= 900);
    std::fs::write(f.file("data.csv"), "Label,Value\nSales,3\n").unwrap();
    let csv = call(
        &f,
        "create",
        json!({"path":"sheets/csv.sheet.json","from":"data.csv","title":"CSV"}),
    )
    .await;
    assert!(csv.starts_with("created sheet"), "{csv}");
    let csv_sheet =
        sheet::storage::load(&f.tools.space_files_dir, "sheets/csv.sheet.json").unwrap();
    assert_eq!(csv_sheet.tabs[0].cells["B2"].input, "3");
    std::fs::write(
        f.file("data.xlsx"),
        sheet::interchange::export_xlsx(&csv_sheet).unwrap(),
    )
    .unwrap();
    let xlsx = call(
        &f,
        "create",
        json!({"kind":"sheet","title":"Imported","from":"data.xlsx"}),
    )
    .await;
    assert!(
        xlsx.starts_with("created sheet · sheets/imported.sheet.json"),
        "{xlsx}"
    );
    let imported =
        sheet::storage::load(&f.tools.space_files_dir, "sheets/imported.sheet.json").unwrap();
    assert_eq!(imported.tabs[0].cells["B2"].input, "3");
    let doc = call(
        &f,
        "create",
        json!({"path":"docs/still.doc.json","title":"Still"}),
    )
    .await;
    assert!(doc.starts_with("created doc"), "{doc}");
    let deck = call(
        &f,
        "create",
        json!({"path":"decks/still.deck.json","title":"Still"}),
    )
    .await;
    assert!(deck.starts_with("created deck"), "{deck}");
    assert!(!super::sheets::is_sheet(
        "deck_read",
        &json!({"kind":"sheet"})
    ));
}

#[tokio::test]
async fn sheets_read_ranges_and_schema_budget() {
    let f = Fixture::new();
    create(&f).await;
    edit(
        &f,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A2","values":[["=B2*10","3"]]},
            {"op":"add_tab","name":"Q3 plan","after":null},
            {"op":"set_cells","tab":"Q3 plan","at":"C9","values":[["quoted tab"]]},
            {"op":"add_tab","name":"O'Brien","after":null},
            {"op":"set_cells","tab":"O'Brien","at":"A1","values":[["escaped quote"]]}
        ]),
    )
    .await;
    let outline = call(&f, "read", json!({"path":PATH})).await;
    assert!(outline.contains("Sheet1 · A2:B2"), "{outline}");
    // create already sent the reference; it goes out once per session.
    assert!(!outline.contains("sheet schema"), "{outline}");
    f.tools.deck_link.lock().unwrap().sheet_schema_shown = false;
    let fresh = call(&f, "read", json!({"path":PATH})).await;
    assert!(fresh.contains("sheet schema"), "{fresh}");
    let range = call(&f, "read", json!({"path":PATH,"range":"A2:B2"})).await;
    assert!(range.contains("=B2*10 → 30"), "{range}");
    assert!(!range.contains("sheet schema"));
    for (range, expected) in [
        ("Sheet1!A2:B2", "=B2*10 → 30"),
        ("'Q3 plan'!C9", "quoted tab"),
        ("'O''Brien'!A1", "escaped quote"),
        ("B:B", "3"),
        ("2:4", "more: rows"),
    ] {
        let read = call(&f, "read", json!({"path":PATH,"range":range})).await;
        assert!(read.contains(expected), "{read}");
        assert!(read.chars().count() <= read::RESULT_BUDGET);
    }
    let bad = call(&f, "read", json!({"path":PATH,"range":"B2:A1"})).await;
    assert!(
        bad.contains("invalid range") && bad.contains("reversed"),
        "{bad}"
    );
    let missing = call(&f, "read", json!({"path":PATH,"range":"Missing!A1"})).await;
    assert!(missing.contains("no tab"), "{missing}");
}

#[tokio::test]
async fn sheets_edit_atomic_rebase_and_errors() {
    let f = Fixture::new();
    create(&f).await;
    let base = load(&f);
    let edited = call(
        &f,
        "edit",
        json!({"path":PATH,"revision":base.revision(),"ops":[
            {"op":"set_cells","tab":"Sheet1","at":"C5","values":[["=1/0"]]}
        ]}),
    )
    .await;
    assert!(edited.starts_with("edited sheet"), "{edited}");
    assert!(edited.contains("- set_cells Sheet1!C5:C5"), "{edited}");
    assert!(edited.contains("errors: Sheet1!C5 #DIV/0!"), "{edited}");
    assert!(f.tools.deck_link.lock().unwrap().changed.len() >= 2);
    let rebased = call(
        &f,
        "edit",
        json!({"path":PATH,"revision":base.revision(),"ops":[
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["safe"]]}
        ]}),
    )
    .await;
    assert!(rebased.contains("rebased:"), "{rebased}");
    let current = load(&f);
    let refused = call(
        &f,
        "edit",
        json!({"path":PATH,"revision":base.revision(),"ops":[
            {"op":"set_cells","tab":"Sheet1","at":"C5","values":[["2"]]}
        ]}),
    )
    .await;
    assert!(
        refused.contains("stale revision") && refused.contains("Nothing was written"),
        "{refused}"
    );
    assert_eq!(load(&f), current);
    let bytes = std::fs::read(f.file(PATH)).unwrap();
    let failed = edit(
        &f,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"B2","values":[["changed"]]},
            {"op":"delete_tab","tab":"Sheet1"}
        ]),
    )
    .await;
    assert!(failed.contains("Nothing was written"), "{failed}");
    assert_eq!(std::fs::read(f.file(PATH)).unwrap(), bytes);
    let many = edit(
        &f,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A10","values":vec![vec!["=1/0"]; 22]}
        ]),
    )
    .await;
    assert!(many.contains("… 2 more"), "{many}");
    assert!(
        !many.contains("Sheet1!C5 #DIV/0!"),
        "only errors in edited ranges: {many}"
    );
    let formatting = edit(
        &f,
        json!([
            {"op":"format","tab":"Sheet1","range":"C1:C9","patch":{"bold":true}}
        ]),
    )
    .await;
    assert!(formatting.contains("Sheet1!C5 #DIV/0!"), "{formatting}");
}

#[tokio::test]
async fn sheets_view_cache_png_aspect_and_clipping() {
    let f = Fixture::new();
    create(&f).await;
    edit(
        &f,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["Title","","3"],["text","=C1*10",""]]},
            {"op":"merge","tab":"Sheet1","range":"A1:B1"},
            {"op":"format","tab":"Sheet1","range":"A1:B1","patch":{"bold":true,"italic":true,"underline":true,"fill":"accent","color":"#ffffff"}},
            {"op":"freeze","tab":"Sheet1","rows":1,"cols":1}
        ]),
    )
    .await;
    let first = call(&f, "view", json!({"path":PATH,"range":"A1:C2"})).await;
    let images = f.images(&first);
    assert_eq!(images.len(), 1, "{first}");
    let image = image::open(&images[0]).unwrap();
    assert_eq!((image.width(), image.height()), (344, 72));
    let modified = std::fs::metadata(&images[0]).unwrap().modified().unwrap();
    let second = call(&f, "view", json!({"path":PATH,"range":"Sheet1!A1:C2"})).await;
    assert_eq!(first, second);
    assert_eq!(
        std::fs::metadata(&images[0]).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(
        std::fs::read_dir(images[0].parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    let clipped = call(&f, "view", json!({"path":PATH,"range":"A1:Z100"})).await;
    assert!(clipped.contains("clipped to Sheet1!A1:T60"), "{clipped}");
    assert!(image::open(&f.images(&clipped)[0]).unwrap().width() <= 1200);
    edit(&f, json!([{"op":"set_title","title":"New revision"}])).await;
    let next = call(&f, "view", json!({"path":PATH})).await;
    assert_eq!(f.images(&next).len(), 1);
    assert!(!images[0].exists(), "stale revisions are removed");
}

#[tokio::test]
async fn sheets_history_export_and_unavailable_actions() {
    let f = Fixture::new();
    create(&f).await;
    let base = load(&f);
    edit(
        &f,
        json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["=2*3"]]},
            {"op":"add_tab","name":"Export tab","after":null},
            {"op":"set_cells","tab":"Export tab","at":"A1","values":[["other"]]}
        ]),
    )
    .await;
    for (format, range, expected) in [
        ("csv", "Sheet1!A1", "6\n"),
        ("csv", "'Export tab'!A1", "other\n"),
        ("xlsx", "", ""),
    ] {
        let exported = call(&f, "export", json!({"path":PATH,"format":format,"range":if range.is_empty() { Value::Null } else { json!(range) }})).await;
        assert!(exported.contains("→ exports/"), "{exported}");
        let relative = exported.trim().split_once("→ ").unwrap().1;
        let bytes = std::fs::read(f.file(relative)).unwrap();
        if format == "csv" {
            assert_eq!(String::from_utf8(bytes).unwrap(), expected);
        } else {
            let imported = sheet::interchange::import_xlsx(&bytes, "Exported").unwrap();
            assert_eq!(
                Workbook::load(&imported)
                    .unwrap()
                    .value("Sheet1", "A1")
                    .kind,
                ValueKind::Number(6.0)
            );
        }
    }
    let history = call(&f, "history", json!({"path":PATH})).await;
    assert!(history.contains(&base.revision()[..8]), "{history}");
    let restored = call(
        &f,
        "history",
        json!({"path":PATH,"revision":base.revision()}),
    )
    .await;
    assert!(restored.starts_with("restored"), "{restored}");
    assert_eq!(load(&f), base);
    let error = call(
        &f,
        "fonts",
        json!({"path":PATH,"text":"review","thread":"t1"}),
    )
    .await;
    assert!(error.contains("not available for sheets yet"), "{error}");
}
