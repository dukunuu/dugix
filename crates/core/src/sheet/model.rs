//! Version-one sheets store editable inputs, never calculated values.
use crate::doc::Theme;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

const fn version() -> u32 {
    1
}

/// Horizontal cell alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Center,
    Right,
}

/// How text that does not fit its cell is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wrap {
    /// Spill into the empty cells beside it (the default).
    Overflow,
    /// Break into lines inside the cell; the row grows to fit.
    Wrap,
    /// Cut at the cell border.
    Clip,
}

/// Vertical alignment of a cell's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

/// Sparse formatting overrides; colors are theme tokens or six-digit hex.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Format {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num: Option<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub bold: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub italic: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub underline: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap: Option<Wrap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valign: Option<VAlign>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Exactly the editable cell source and its formatting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cell {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub input: String,
    #[serde(skip_serializing_if = "is_default")]
    pub fmt: Format,
}

/// Counts of frozen leading rows and columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Frozen {
    #[serde(skip_serializing_if = "is_default")]
    pub rows: u32,
    #[serde(skip_serializing_if = "is_default")]
    pub cols: u32,
}

/// A named grid; the stable id is independent of its displayed name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tab {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cells: BTreeMap<String, Cell>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cols: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rows: BTreeMap<u32, f64>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub frozen: Frozen,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merges: Vec<String>,
}

impl Tab {
    /// An empty tab; validation assigns its stable id in workbook order.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            id: String::new(),
            name: name.into(),
            cells: BTreeMap::new(),
            cols: BTreeMap::new(),
            rows: BTreeMap::new(),
            frozen: Frozen::default(),
            merges: Vec::new(),
        }
    }

    /// Smallest rectangle containing stored inputs and formatting.
    #[must_use]
    pub fn used_range(&self) -> Option<super::address::Rect> {
        use super::address::{Address, Rect};
        self.cells
            .keys()
            .filter_map(|key| key.parse::<Address>().ok())
            .fold(None, |rect: Option<Rect>, cell| {
                Some(rect.map_or(
                    Rect {
                        start: cell,
                        end: cell,
                    },
                    |rect| Rect {
                        start: Address {
                            row: rect.start.row.min(cell.row),
                            col: rect.start.col.min(cell.col),
                        },
                        end: Address {
                            row: rect.end.row.max(cell.row),
                            col: rect.end.col.max(cell.col),
                        },
                    },
                ))
            })
    }
}

/// A complete, self-contained spreadsheet file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sheet {
    #[serde(default = "version")]
    pub version: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_rev: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub theme: Theme,
    pub tabs: Vec<Tab>,
}
