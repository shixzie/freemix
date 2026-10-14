//! Deterministic row selection and navigation over a list of data rows.

use std::fmt;

use crate::{DataPath, DataValue, PathError, ValueType};

/// What happens when navigation moves past the first or last row.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EdgeBehavior {
    /// Stay on the first/last row.
    #[default]
    Stop,
    /// Continue from the opposite end.
    Wrap,
}

/// A navigation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowNavigation {
    First,
    Last,
    /// Selects the first row when nothing is selected.
    Next,
    /// Selects the last row when nothing is selected.
    Previous,
    Index(usize),
}

/// Row navigation or lookup failed. The selection is left unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowSelectionError {
    Empty,
    IndexOutOfRange { index: usize, len: usize },
    LengthMismatch { cursor: usize, rows: usize },
    NoMatch,
    ExpectedList { actual: ValueType },
    Path(PathError),
}

impl fmt::Display for RowSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("there are no rows"),
            Self::IndexOutOfRange { index, len } => {
                write!(formatter, "row {index} is out of range for {len} rows")
            }
            Self::LengthMismatch { cursor, rows } => {
                write!(
                    formatter,
                    "cursor tracks {cursor} rows but {rows} were given"
                )
            }
            Self::NoMatch => formatter.write_str("no row matched"),
            Self::ExpectedList { actual } => {
                write!(formatter, "expected row list, found {actual:?}")
            }
            Self::Path(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RowSelectionError {}

/// Resolves the row list at `path` inside a parsed document.
///
/// # Errors
///
/// Returns a path error or [`RowSelectionError::ExpectedList`].
pub fn rows_at<'a>(
    root: &'a DataValue,
    path: &DataPath,
) -> Result<&'a [DataValue], RowSelectionError> {
    match path.extract(root).map_err(RowSelectionError::Path)? {
        DataValue::List(rows) => Ok(rows),
        other => Err(RowSelectionError::ExpectedList {
            actual: other.value_type(),
        }),
    }
}

/// A selection over `len` rows with deterministic edge handling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RowCursor {
    len: usize,
    selected: Option<usize>,
    edge: EdgeBehavior,
}

impl RowCursor {
    /// Creates a cursor with nothing selected.
    #[must_use]
    pub const fn new(len: usize, edge: EdgeBehavior) -> Self {
        Self {
            len,
            selected: None,
            edge,
        }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub const fn selected(&self) -> Option<usize> {
        self.selected
    }

    #[must_use]
    pub const fn edge(&self) -> EdgeBehavior {
        self.edge
    }

    /// Clears the selection.
    pub fn clear(&mut self) {
        self.selected = None;
    }

    /// Updates the row count after a refresh, keeping the selected index when
    /// it is still valid, clamping it to the last row otherwise, and clearing
    /// it when the list becomes empty.
    pub fn set_len(&mut self, len: usize) {
        self.len = len;
        self.selected = match (self.selected, len) {
            (_, 0) | (None, _) => None,
            (Some(index), len) => Some(index.min(len - 1)),
        };
    }

    /// Moves the selection and returns the newly selected index.
    ///
    /// # Errors
    ///
    /// Returns [`RowSelectionError::Empty`] with no rows, or
    /// [`RowSelectionError::IndexOutOfRange`] for an invalid explicit index.
    pub fn navigate(&mut self, navigation: RowNavigation) -> Result<usize, RowSelectionError> {
        if self.len == 0 {
            return Err(RowSelectionError::Empty);
        }
        let last = self.len - 1;
        let wrap = self.edge == EdgeBehavior::Wrap;
        let index = match (navigation, self.selected) {
            (RowNavigation::First, _) | (RowNavigation::Next, None) => 0,
            (RowNavigation::Last, _) | (RowNavigation::Previous, None) => last,
            (RowNavigation::Next, Some(current)) if current >= last => {
                if wrap {
                    0
                } else {
                    last
                }
            }
            (RowNavigation::Next, Some(current)) => current + 1,
            (RowNavigation::Previous, Some(0)) => {
                if wrap {
                    last
                } else {
                    0
                }
            }
            (RowNavigation::Previous, Some(current)) => current - 1,
            (RowNavigation::Index(index), _) => {
                if index > last {
                    return Err(RowSelectionError::IndexOutOfRange {
                        index,
                        len: self.len,
                    });
                }
                index
            }
        };
        self.selected = Some(index);
        Ok(index)
    }

    /// Selects the first row whose value at `key` equals `expected` exactly
    /// (no type coercion). Rows lacking the key are skipped.
    ///
    /// # Errors
    ///
    /// Returns [`RowSelectionError::LengthMismatch`] when `rows` does not match
    /// the cursor length, or [`RowSelectionError::NoMatch`].
    pub fn select_where(
        &mut self,
        rows: &[DataValue],
        key: &DataPath,
        expected: &DataValue,
    ) -> Result<usize, RowSelectionError> {
        if rows.len() != self.len {
            return Err(RowSelectionError::LengthMismatch {
                cursor: self.len,
                rows: rows.len(),
            });
        }
        let index = rows
            .iter()
            .position(|row| key.extract(row).is_ok_and(|value| value == expected))
            .ok_or(RowSelectionError::NoMatch)?;
        self.selected = Some(index);
        Ok(index)
    }

    /// Returns the selected row from `rows`, if any.
    #[must_use]
    pub fn current<'a>(&self, rows: &'a [DataValue]) -> Option<&'a DataValue> {
        rows.get(self.selected?)
    }
}
