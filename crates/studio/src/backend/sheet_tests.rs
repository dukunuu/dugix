//! Sheet mode through the same JSON command boundary the grid will use.
use super::*;
use dugix_core::{
    canvas::dialogs::Scripted,
    sheet::{self, Sheet},
};
use serde_json::{Value, json};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("dugix-studio-sheet-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn files(&self) -> PathBuf {
        self.0.join("files")
    }
    fn path(&self) -> PathBuf {
        self.files().join("sheets/grid.sheet.json")
    }
    fn backend(&self) -> Backend {
        let sheet = Sheet::from_design("Grid", &dugix_core::design::Design::default());
        sheet::storage::save(
            &self.files(),
            "sheets/grid.sheet.json",
            &sheet,
            deck::history::Who::User,
        )
        .unwrap();
        Backend::start(Some(&self.path()), &self.files(), &self.0.join("data")).unwrap()
    }
    fn disk(&self) -> Sheet {
        dugix_core::canvas::files::read_sheet(&self.path()).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn send(backend: &mut Backend, command: Value) -> Event {
    backend.dispatch(&command.to_string())
}
fn apply(backend: &mut Backend, input: &str) -> Event {
    send(
        backend,
        json!({"command":"SheetApply","ops":[{"op":"set_cells","tab":"Sheet1","at":"B4","values":[[input]]}]}),
    )
}
fn assert_sheet(event: Event) {
    assert!(matches!(event, Event::Sheet { .. }), "{event:?}");
}

#[test]
fn sheet_chart_events_recalculate_undo_reload_and_warn_on_xlsx() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let event = send(
        &mut backend,
        json!({"command":"SheetApply","ops":[
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[["Label","N"],["A","2"],["B","4"]]},
            {"op":"add_tab","name":"Other"},
            {"op":"add_chart","tab":"Sheet1","data":"A1:B3"}
        ]}),
    );
    let Event::Sheet { charts, .. } = event else {
        panic!("{event:?}")
    };
    assert_eq!(charts.len(), 2);
    assert!(charts["t2"].is_empty());
    let original = charts["t1"][0].svg.clone();
    assert_eq!(charts["t1"][0].id, "c1");
    let event = send(
        &mut backend,
        json!({"command":"SheetApply","ops":[{"op":"set_cells","tab":"Sheet1","at":"B2","values":[["9"]]}]}),
    );
    let Event::Sheet { charts, .. } = event else {
        panic!("{event:?}")
    };
    assert_ne!(charts["t1"][0].svg, original);
    let event = send(&mut backend, json!({"command":"Undo"}));
    let Event::Sheet { charts, .. } = event else {
        panic!("{event:?}")
    };
    assert_eq!(charts["t1"][0].svg, original);
    let event = send(
        &mut backend,
        json!({"command":"SheetExport","format":"xlsx"}),
    );
    let Event::Exported { warnings, .. } = event else {
        panic!("{event:?}")
    };
    assert!(
        warnings
            .iter()
            .any(|w| w == "charts aren't included in XLSX exports")
    );
    let next = sheet::apply(
        &temp.disk(),
        &serde_json::from_value::<Vec<sheet::Op>>(
            json!([{"op":"set_cells","tab":"Sheet1","at":"B2","values":[["12"]]}]),
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(temp.path(), serde_json::to_vec(&next).unwrap()).unwrap();
    let Event::Sheet { charts, .. } = backend.poll().unwrap().unwrap() else {
        panic!("sheet reload")
    };
    assert_ne!(charts["t1"][0].svg, original);
}

#[test]
fn sheet_open_apply_atomic_failure_and_undo_redo_restore_the_file() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    assert_sheet(backend.event());
    let initial = temp.disk();
    assert_sheet(apply(&mut backend, "=2+3"));
    let saved = temp.disk();
    assert_eq!(saved.tabs[0].cells["B4"].input, "=2+3");
    assert_eq!(backend.sheet.as_ref().unwrap().undo.len(), 1);
    let bytes = fs::read(temp.path()).unwrap();
    let failed = send(
        &mut backend,
        json!({"command":"SheetApply","ops":[
            {"op":"set_cells","tab":"Sheet1","at":"B4","values":[["changed"]]},
            {"op":"delete_tab","tab":"Sheet1"}
        ]}),
    );
    assert!(matches!(failed, Event::Error { .. }), "{failed:?}");
    assert_eq!(fs::read(temp.path()).unwrap(), bytes);
    assert_eq!(backend.sheet.as_ref().unwrap().sheet, saved);
    assert_eq!(backend.sheet.as_ref().unwrap().undo.len(), 1);
    assert_sheet(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(temp.disk(), initial);
    assert_sheet(send(&mut backend, json!({"command":"Redo"})));
    assert_eq!(temp.disk(), saved);
    let event = send(&mut backend, json!({"command":"NewDeck","title":"Deck"}));
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
}

#[test]
fn sheet_library_switches_modes_and_cross_mode_commands_are_refused() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let event = send(&mut backend, json!({"command":"NewDoc","title":"Doc"}));
    assert!(matches!(event, Event::Doc { .. }), "{event:?}");
    let event = send(
        &mut backend,
        json!({"command":"SheetValues","tab":"Sheet1","range":"A1"}),
    );
    assert!(matches!(event, Event::Error { message } if message.contains("open a sheet")));
    assert_sheet(send(
        &mut backend,
        json!({"command":"LibraryOpen","path":"sheets/grid.sheet.json"}),
    ));
    for command in [
        json!({"command":"DocApply","ops":[]}),
        json!({"command":"Apply","ops":[]}),
        json!({"command":"Save","deck":Deck::new_from_layouts("No")}),
        json!({"command":"ImportTemplate","path":"no.pptx"}),
        json!({"command":"ApplyDesign","design":"dugix"}),
    ] {
        let event = send(&mut backend, command);
        assert!(matches!(event, Event::Error { .. }), "{event:?}");
    }
    let event = send(&mut backend, json!({"command":"NewDeck","title":"Deck"}));
    assert!(matches!(event, Event::Deck { .. }), "{event:?}");
    let event = send(&mut backend, json!({"command":"SheetApply","ops":[]}));
    assert!(matches!(event, Event::Error { .. }));
    assert_sheet(send(
        &mut backend,
        json!({"command":"NewSheet","title":"New budget"}),
    ));
    assert!(backend.path.ends_with("sheets/new-budget.sheet.json"));
}

#[test]
fn sheet_values_are_computed_sparse_windowed_and_bounded() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    assert_sheet(apply(&mut backend, "=2+3"));
    let event = send(
        &mut backend,
        json!({"command":"SheetValues","tab":"Sheet1","range":"B4:C5"}),
    );
    let Event::SheetValues { values, .. } = event else {
        panic!("{event:?}");
    };
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].a1, "B4");
    assert_eq!(values[0].display, "5");
    assert_eq!(values[0].kind, "number");
    let event = send(
        &mut backend,
        json!({"command":"SheetValues","tab":"Sheet1","range":"A1:CV100"}),
    );
    assert!(matches!(event, Event::SheetValues { .. }), "{event:?}");
    let event = send(
        &mut backend,
        json!({"command":"SheetValues","tab":"Sheet1","range":"A1:CV101"}),
    );
    assert!(matches!(event, Event::Error { message } if message.contains("10,000")));
    let event = send(
        &mut backend,
        json!({"command":"SheetValues","tab":"Missing","range":"A1"}),
    );
    assert!(matches!(event, Event::Error { .. }));
}

#[test]
fn sheet_external_reload_is_one_undo_step_with_changed_cells_and_history_preview() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let before = temp.disk();
    let next = sheet::apply(
        &before,
        &[sheet::Op::SetCells {
            tab: "Sheet1".into(),
            at: "B4".into(),
            values: vec![vec!["42".into()]],
        }],
    )
    .unwrap();
    sheet::storage::save(
        &temp.files(),
        "sheets/grid.sheet.json",
        &next,
        deck::history::Who::Model,
    )
    .unwrap();
    let event = backend.poll().unwrap().unwrap();
    let Event::Sheet {
        changed, notice, ..
    } = event
    else {
        panic!("{event:?}");
    };
    assert_eq!(changed, ["Sheet1!B4"]);
    assert!(notice.unwrap().contains("Undo"));
    assert_eq!(backend.sheet.as_ref().unwrap().undo.len(), 1);
    assert!(backend.poll().unwrap().is_none());
    let event = send(&mut backend, json!({"command":"History"}));
    let Event::History { versions } = event else {
        panic!("{event:?}");
    };
    assert!(!versions.is_empty());
    let id = &versions[0].id;
    let event = send(&mut backend, json!({"command":"HistoryPreview","id":id}));
    let Event::HistoryPreview {
        sheet: Some(kept),
        deck: None,
        doc: None,
        ..
    } = event
    else {
        panic!("{event:?}");
    };
    assert_eq!(kept, before);
    assert_eq!(temp.disk(), next);
    assert_sheet(send(&mut backend, json!({"command":"Undo"})));
    assert_eq!(temp.disk(), before);
    assert_sheet(send(&mut backend, json!({"command":"Redo"})));
    assert_eq!(temp.disk(), next);
    assert_sheet(send(
        &mut backend,
        json!({"command":"HistoryRestore","id":id}),
    ));
    assert_eq!(temp.disk(), before);
}

#[test]
fn sheet_comments_send_link_messages_and_sidecar_reloads_are_undoable() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    assert_sheet(apply(&mut backend, "42"));
    backend.linked = true;
    let event = send(
        &mut backend,
        json!({"command":"SheetCommentCreate","tab":"Sheet1","range":"B4","text":"Why?","send":true}),
    );
    let Event::SheetFeedback { threads, .. } = event else {
        panic!("{event:?}");
    };
    assert_eq!(threads[0].anchor.quote, "42");
    assert!(
        matches!(backend.outgoing.last(), Some(LinkMessage::SheetComment { path, tab, range, quote, text, .. }) if path == &temp.path() && tab == "Sheet1" && range == "B4" && quote == "42" && text == "Why?")
    );
    let id = &threads[0].id;
    let event = send(
        &mut backend,
        json!({"command":"CommentReply","thread":id,"text":"More","send":true}),
    );
    assert!(matches!(event, Event::SheetFeedback { .. }));
    let event = send(
        &mut backend,
        json!({"command":"CommentResolve","thread":id}),
    );
    assert!(matches!(event, Event::SheetFeedback { threads, .. } if threads[0].resolved.is_some()));
    let mut threads = sheet::feedback::load(&temp.path()).unwrap();
    threads[0].resolved = None;
    threads[0].messages.push(sheet::feedback::Message::new_for(
        sheet::feedback::Role::Model,
        "Reply from the model".into(),
        vec![],
    ));
    sheet::feedback::save(&temp.path(), &threads).unwrap();
    assert!(matches!(backend.poll().unwrap(), Some(Event::Sheet { .. })));
    assert_sheet(send(&mut backend, json!({"command":"Undo"})));
    assert!(
        sheet::feedback::load(&temp.path()).unwrap()[0]
            .resolved
            .is_some()
    );
    let event = send(
        &mut backend,
        json!({"command":"SheetFocus","tab":"Sheet1","range":"B2:C5"}),
    );
    assert!(matches!(event, Event::SheetFeedback { .. }));
    assert!(
        matches!(backend.outgoing.last(), Some(LinkMessage::SheetFocus { range, .. }) if range == "B2:C5")
    );
    let event = backend
        .receive_link(LinkMessage::SheetFocus {
            path: temp.path(),
            tab: "Sheet1".into(),
            range: "C9".into(),
        })
        .unwrap();
    assert!(matches!(event, Event::SheetFocus { range, .. } if range == "C9"));
}

#[test]
fn sheet_csv_xlsx_exports_and_dialog_imports_use_scripted_dialogs() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    assert_sheet(apply(&mut backend, "=2+3"));
    for format in ["csv", "xlsx"] {
        let event = send(
            &mut backend,
            json!({"command":"SheetExport","format":format}),
        );
        let Event::Exported { path, .. } = event else {
            panic!("{event:?}");
        };
        assert!(path.is_file());
        if format == "csv" {
            assert!(fs::read_to_string(&path).unwrap().contains('5'));
        } else {
            let imported =
                sheet::interchange::import_xlsx(&fs::read(&path).unwrap(), "Imported").unwrap();
            assert_eq!(
                sheet::Workbook::load(&imported)
                    .unwrap()
                    .value(&imported.tabs[0].name, "B4")
                    .display,
                "5"
            );
        }
        let chosen = temp.0.join(format!("chosen.{format}"));
        let fake = Scripted::new([Some(chosen.clone()), Some(chosen.clone())]);
        backend.dialogs = Arc::new(fake.clone());
        let event = send(&mut backend, json!({"command":"ExportAs","format":format}));
        assert!(matches!(event, Event::Exported { path, .. } if path == chosen));
        assert_sheet(send(&mut backend, json!({"command":"OpenDialog"})));
        assert!(backend.path.starts_with(temp.files().join("sheets")));
        assert_eq!(
            backend.sheet.as_ref().unwrap().sheet.tabs[0].cells["B4"].input,
            "5"
        );
        assert_eq!(fake.requests().len(), 2);
        assert!(!fake.requests()[1].save);
    }
    let chosen = temp.0.join("library.csv");
    backend.dialogs = Arc::new(Scripted::new([Some(chosen.clone())]));
    let event = send(
        &mut backend,
        json!({"command":"LibraryExport","path":"sheets/grid.sheet.json","format":"csv"}),
    );
    assert!(matches!(event, Event::Exported { path, .. } if path == chosen));
}

#[test]
fn sheet_event_carries_derived_row_heights_for_wrapped_cells() {
    let temp = Temp::new();
    let mut backend = temp.backend();
    let Event::Sheet { auto_rows, .. } = backend.event() else {
        panic!("no sheet event");
    };
    assert!(auto_rows["t1"].is_empty(), "nothing is wrapped yet");
    let long = "a sentence long enough to need several lines inside a narrow cell";
    let event = send(
        &mut backend,
        json!({"command":"SheetApply","ops":[
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[[long]]},
            {"op":"format","tab":"Sheet1","range":"A1","patch":{"wrap":"wrap"}}
        ]}),
    );
    let Event::Sheet { auto_rows, .. } = event else {
        panic!("{event:?}");
    };
    assert!(
        auto_rows["t1"][&1] > 24.0,
        "a wrapped row grows: {:?}",
        auto_rows["t1"]
    );
    // A user-set height always wins.
    let event = send(
        &mut backend,
        json!({"command":"SheetApply","ops":[
            {"op":"set_row_height","tab":"Sheet1","rows":"1:1","height":80}
        ]}),
    );
    let Event::Sheet { auto_rows, .. } = event else {
        panic!("{event:?}");
    };
    assert!(!auto_rows["t1"].contains_key(&1));
}
