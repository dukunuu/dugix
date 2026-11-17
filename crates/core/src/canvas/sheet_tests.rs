use super::*;
use crate::{
    deck::history::Who,
    sheet::{self, Sheet},
};

#[test]
fn canvas_sheets_list_thumbnail_rename_duplicate_delete_and_copy() {
    let root = std::env::temp_dir().join(format!("dugix-sheet-library-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut sheet = Sheet::from_design("Budget", &crate::design::Design::default());
        sheet.tabs[0].cells.insert(
            "B4".into(),
            sheet::Cell {
                input: "3".into(),
                ..Default::default()
            },
        );
        let relative = "sheets/budget.sheet.json";
        sheet::storage::save(&root, relative, &sheet, Who::User)?;
        let entries = library(&root);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.kind, Kind::Sheet);
        assert_eq!(entry.count, 1);
        assert_eq!(entry.count_label(), "1 cell");
        let thumbnail = ensure_thumbnail(&root, entry)?;
        assert!(
            thumbnail
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(&sheet.revision()[..8])
        );
        assert!(std::fs::read(thumbnail)?.starts_with(b"\x89PNG"));
        files::rename(&root, relative, "Renamed")?;
        assert_eq!(sheet::storage::load(&root, relative)?.title, "Renamed");
        let copy = files::duplicate(&root, relative)?;
        assert_eq!(sheet::storage::load(&root, &copy)?.title, "Renamed copy");
        let output = root.join("outside");
        std::fs::create_dir_all(&output)?;
        let saved = files::save_copy(&entry.path, &output.join("another"))?;
        assert!(saved.path.ends_with("another.sheet.json"));
        assert_eq!(files::read_sheet(&saved.path)?.title, "Renamed");
        let trash = files::delete(&root, relative)?;
        assert!(!root.join(relative).exists());
        assert!(trash.join("budget.sheet.json").is_file());
        assert_eq!(library(&root).len(), 1);
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(root);
    result.unwrap();
}
