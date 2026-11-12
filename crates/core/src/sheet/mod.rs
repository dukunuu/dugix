//! Local-first spreadsheets: editable JSON, one calculation engine, atomic
//! edits, bounded read views, history, and byte-oriented interchange.
pub mod address;
pub mod engine;
pub mod feedback;
pub mod interchange;
pub mod model;
pub mod ops;
pub mod read;
pub mod render;
pub mod storage;
pub mod validate;

pub use crate::deck::history;
pub use engine::{Value, ValueKind, Workbook};
pub use model::{Align, Cell, Format, Frozen, Sheet, Tab};
pub use ops::{ClearWhat, Op, apply, apply_with_resolver};
pub use read::{Show, outline, read_range};

impl Sheet {
    /// One empty Sheet1 tab following the supplied design.
    #[must_use]
    pub fn from_design(title: &str, design: &crate::design::Design) -> Self {
        let mut tab = Tab::new("Sheet1");
        tab.id = "t1".into();
        Self {
            version: 1,
            title: title.into(),
            design: Some(design.slug()),
            design_rev: Some(design.revision()),
            theme: crate::doc::Theme::from_design(design),
            tabs: vec![tab],
        }
    }

    /// SHA-256 of canonical JSON; calculated values never affect revisions.
    #[must_use]
    pub fn revision(&self) -> String {
        crate::deck::hash(self)
    }
}

#[cfg(test)]
mod grid_tests;
#[cfg(test)]
mod tests;
