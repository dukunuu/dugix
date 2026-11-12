//! Strict, one-based Excel addresses. Stored addresses never contain `$`.
use anyhow::{Result, anyhow, ensure};
use std::{fmt, str::FromStr};

pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLS: u32 = 16_384;

/// Convert uppercase column letters to a one-based index.
pub fn column_index(text: &str) -> Result<u32> {
    ensure!(!text.is_empty(), "empty column");
    let mut index = 0_u32;
    for b in text.bytes() {
        ensure!(b.is_ascii_uppercase(), "invalid column {text:?}");
        index = index
            .checked_mul(26)
            .and_then(|n| n.checked_add(u32::from(b - b'A') + 1))
            .ok_or_else(|| anyhow!("column outside Excel limits: {text}"))?;
        ensure!(index <= MAX_COLS, "column outside Excel limits: {text}");
    }
    Ok(index)
}

/// Convert a one-based index to uppercase column letters.
pub fn column_name(mut index: u32) -> Result<String> {
    ensure!((1..=MAX_COLS).contains(&index), "invalid column {index}");
    let mut letters = Vec::new();
    while index > 0 {
        index -= 1;
        letters.push(char::from(b'A' + u8::try_from(index % 26)?));
        index /= 26;
    }
    Ok(letters.into_iter().rev().collect())
}

fn row_index(text: &str) -> Result<u32> {
    ensure!(
        !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit()),
        "invalid row {text:?}"
    );
    let row = text.parse()?;
    ensure!((1..=MAX_ROWS).contains(&row), "invalid row {text:?}");
    Ok(row)
}

/// A single cell, with one-based row and column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address {
    pub row: u32,
    pub col: u32,
}

impl FromStr for Address {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let split = text
            .find(|c: char| !c.is_ascii_uppercase())
            .unwrap_or(text.len());
        let (col, row) = text.split_at(split);
        Ok(Self {
            row: row_index(row)?,
            col: column_index(col)?,
        })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let col = column_name(self.col).map_err(|_| fmt::Error)?;
        write!(f, "{col}{}", self.row)
    }
}

/// Inclusive rectangle, used to intersect sparse cells without expanding ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub start: Address,
    pub end: Address,
}

impl Rect {
    #[must_use]
    pub fn contains(self, cell: Address) -> bool {
        (self.start.row..=self.end.row).contains(&cell.row)
            && (self.start.col..=self.end.col).contains(&cell.col)
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.start.row <= other.end.row
            && self.end.row >= other.start.row
            && self.start.col <= other.end.col
            && self.end.col >= other.start.col
    }

    #[must_use]
    pub fn cell_count(self) -> u64 {
        u64::from(self.end.row - self.start.row + 1) * u64::from(self.end.col - self.start.col + 1)
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.start, self.end)
    }
}

/// A cell rectangle, complete columns, or complete rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Cells(Rect),
    Columns(u32, u32),
    Rows(u32, u32),
}

impl Range {
    #[must_use]
    pub fn bounds(self) -> Rect {
        match self {
            Self::Cells(rect) => rect,
            Self::Columns(first, last) => Rect {
                start: Address { row: 1, col: first },
                end: Address {
                    row: MAX_ROWS,
                    col: last,
                },
            },
            Self::Rows(first, last) => Rect {
                start: Address { row: first, col: 1 },
                end: Address {
                    row: last,
                    col: MAX_COLS,
                },
            },
        }
    }
}

impl FromStr for Range {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let Some((first, last)) = text.split_once(':') else {
            let cell = text.parse()?;
            return Ok(Self::Cells(Rect {
                start: cell,
                end: cell,
            }));
        };
        let range = if first.bytes().all(|b| b.is_ascii_uppercase()) {
            Self::Columns(column_index(first)?, column_index(last)?)
        } else if first.bytes().all(|b| b.is_ascii_digit()) {
            Self::Rows(row_index(first)?, row_index(last)?)
        } else {
            Self::Cells(Rect {
                start: first.parse()?,
                end: last.parse()?,
            })
        };
        let rect = range.bounds();
        ensure!(
            rect.start.row <= rect.end.row && rect.start.col <= rect.end.col,
            "reversed range {text:?}"
        );
        Ok(range)
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cells(rect) if rect.start == rect.end => write!(f, "{}", rect.start),
            Self::Cells(rect) => write!(f, "{rect}"),
            Self::Columns(first, last) => write!(
                f,
                "{}:{}",
                column_name(*first).map_err(|_| fmt::Error)?,
                column_name(*last).map_err(|_| fmt::Error)?
            ),
            Self::Rows(first, last) => write!(f, "{first}:{last}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_limits() {
        for col in 1..=MAX_COLS {
            assert_eq!(column_index(&column_name(col).unwrap()).unwrap(), col);
        }
        for text in ["A1", "B12", "XFD1048576"] {
            assert_eq!(text.parse::<Address>().unwrap().to_string(), text);
        }
        for text in ["XFE1", "A0", "A01", "a1", "$A1", "A1048577", "A", "1"] {
            assert!(text.parse::<Address>().is_err(), "{text}");
        }
        for text in ["A1:C3", "B:B", "2:4", "B12"] {
            assert_eq!(text.parse::<Range>().unwrap().to_string(), text);
        }
        for text in ["C3:A1", "C:A", "4:2", "A:3", "0:2", "A1:B2:C3"] {
            assert!(text.parse::<Range>().is_err(), "{text}");
        }
        assert_eq!("B:B".parse::<Range>().unwrap().bounds().end.row, MAX_ROWS);
        assert_eq!("2:4".parse::<Range>().unwrap().bounds().end.col, MAX_COLS);
    }
}
