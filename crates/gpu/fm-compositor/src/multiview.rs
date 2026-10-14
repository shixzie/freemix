//! Bounded reference model for independent multiview outputs.
//!
//! A multiview definition selects a rectangular grid, one source per populated
//! cell, an optional confidence view, and overlay intent flags. Planning turns
//! that definition into exact integer cell rectangles; compilation turns a plan
//! into a [`Scene`] whose layers reproduce each cell with an optional inset
//! border. Labels, tally, clocks, and audio meters are modeled but not
//! rasterized by this CPU reference path.
//!
//! Every definition is independent: callers may plan and compile two or more
//! multiviews with different layouts and sources without shared state.

use core::fmt;

use fm_types::InputId;
use fm_video::{Rgba8, Rotation, Transform};

use crate::scene::{Scene, SceneError, SourceId, SourceLayer};

/// Maximum number of cells in one multiview grid.
pub const MAX_MULTIVIEW_CELLS: usize = 16;
/// Maximum number of columns in one multiview grid.
pub const MAX_MULTIVIEW_COLUMNS: u8 = 4;
/// Maximum number of rows in one multiview grid.
pub const MAX_MULTIVIEW_ROWS: u8 = 4;
/// Maximum multiview name length in bytes.
pub const MAX_MULTIVIEW_NAME_BYTES: usize = 128;

/// One selectable multiview cell source.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MultiviewSource {
    /// The current Program output.
    Program,
    /// The current Preview output.
    Preview,
    /// One exact input identifier.
    Input(InputId),
}

/// A validated multiview grid shape.
///
/// Valid shapes have 1 to [`MAX_MULTIVIEW_COLUMNS`] columns and 1 to
/// [`MAX_MULTIVIEW_ROWS`] rows, with at most [`MAX_MULTIVIEW_CELLS`] cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GridLayout {
    columns: u8,
    rows: u8,
}

impl GridLayout {
    /// Validates a bounded grid shape.
    ///
    /// # Errors
    ///
    /// Returns a typed error for zero or oversized dimensions, or a shape that
    /// requests more than [`MAX_MULTIVIEW_CELLS`] cells.
    pub fn new(columns: u8, rows: u8) -> Result<Self, MultiviewError> {
        if columns == 0 {
            return Err(MultiviewError::ZeroGridColumns);
        }
        if rows == 0 {
            return Err(MultiviewError::ZeroGridRows);
        }
        if columns > MAX_MULTIVIEW_COLUMNS {
            return Err(MultiviewError::GridColumnsExceeded {
                columns,
                maximum: MAX_MULTIVIEW_COLUMNS,
            });
        }
        if rows > MAX_MULTIVIEW_ROWS {
            return Err(MultiviewError::GridRowsExceeded {
                rows,
                maximum: MAX_MULTIVIEW_ROWS,
            });
        }
        let cells = usize::from(columns) * usize::from(rows);
        if cells > MAX_MULTIVIEW_CELLS {
            return Err(MultiviewError::GridCapacityExceeded {
                cells,
                maximum: MAX_MULTIVIEW_CELLS,
            });
        }
        Ok(Self { columns, rows })
    }

    /// The standard single-cell grid.
    #[must_use]
    pub const fn one_by_one() -> Self {
        Self {
            columns: 1,
            rows: 1,
        }
    }

    /// The standard 2x2 grid.
    #[must_use]
    pub const fn two_by_two() -> Self {
        Self {
            columns: 2,
            rows: 2,
        }
    }

    /// The standard 3x3 grid.
    #[must_use]
    pub const fn three_by_three() -> Self {
        Self {
            columns: 3,
            rows: 3,
        }
    }

    /// The standard 4x4 grid.
    #[must_use]
    pub const fn four_by_four() -> Self {
        Self {
            columns: 4,
            rows: 4,
        }
    }

    #[must_use]
    pub const fn columns(self) -> u8 {
        self.columns
    }

    #[must_use]
    pub const fn rows(self) -> u8 {
        self.rows
    }

    /// Number of cells in the grid.
    #[must_use]
    pub fn capacity(self) -> usize {
        usize::from(self.columns) * usize::from(self.rows)
    }
}

/// Reference model for the confidence monitor choice of one multiview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfidenceMode {
    /// No confidence view is selected.
    None,
    /// A confidence view of all inputs only.
    InputsOnly,
    /// A confidence view locked to one input that must also be a cell source.
    SingleInput(InputId),
}

/// Overlay intent flags for one multiview.
///
/// The flags record placement intent for the multiview definition. The CPU
/// reference scene compiler does not rasterize labels, tally, clocks, or audio
/// meters. `meters` records that the operator chose audio meters for this
/// multiview; it is not a meter layout and does not describe which channels or
/// meters are shown.
// Each flag is an independent operator choice for the same multiview.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultiviewOverlays {
    /// Label overlay intent.
    pub labels: bool,
    /// Tally overlay intent.
    pub tally: bool,
    /// Clock overlay intent.
    pub clocks: bool,
    /// Chosen audio meters: placement intent only, not rasterized.
    pub meters: bool,
}

impl MultiviewOverlays {
    /// No overlays selected.
    pub const NONE: Self = Self {
        labels: false,
        tally: false,
        clocks: false,
        meters: false,
    };

    /// Every modeled overlay selected.
    pub const ALL: Self = Self {
        labels: true,
        tally: true,
        clocks: true,
        meters: true,
    };

    /// No overlays selected.
    #[must_use]
    pub const fn none() -> Self {
        Self::NONE
    }

    #[must_use]
    pub const fn with_labels(mut self, labels: bool) -> Self {
        self.labels = labels;
        self
    }

    #[must_use]
    pub const fn with_tally(mut self, tally: bool) -> Self {
        self.tally = tally;
        self
    }

    #[must_use]
    pub const fn with_clocks(mut self, clocks: bool) -> Self {
        self.clocks = clocks;
        self
    }

    #[must_use]
    pub const fn with_meters(mut self, meters: bool) -> Self {
        self.meters = meters;
        self
    }
}

/// One independent multiview selection.
///
/// Fields are public so a definition can be assembled directly; callers should
/// prefer [`MultiviewDefinition::new`] and [`MultiviewDefinition::validate`] to
/// reject malformed definitions. Plan and compile entry points re-validate the
/// definition before using it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiviewDefinition {
    pub name: String,
    pub layout: GridLayout,
    pub sources: Vec<MultiviewSource>,
    pub confidence: ConfidenceMode,
    pub overlays: MultiviewOverlays,
}

impl MultiviewDefinition {
    /// Creates and validates one multiview definition.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a malformed name, source count, duplicate cell
    /// source, or confidence selection.
    pub fn new(
        name: impl Into<String>,
        layout: GridLayout,
        sources: Vec<MultiviewSource>,
        confidence: ConfidenceMode,
        overlays: MultiviewOverlays,
    ) -> Result<Self, MultiviewError> {
        let definition = Self {
            name: name.into(),
            layout,
            sources,
            confidence,
            overlays,
        };
        definition.validate()?;
        Ok(definition)
    }

    /// Re-validates this definition.
    ///
    /// The name must be nonempty, at most [`MAX_MULTIVIEW_NAME_BYTES`] bytes,
    /// and free of control characters. The source list must hold between 1 and
    /// [`GridLayout::capacity`] distinct cell sources. A
    /// [`ConfidenceMode::SingleInput`] must name one of the cell inputs.
    ///
    /// # Errors
    ///
    /// Returns a typed error for the first violated rule.
    pub fn validate(&self) -> Result<(), MultiviewError> {
        validate_name(&self.name)?;
        if self.sources.is_empty() {
            return Err(MultiviewError::NoSources);
        }
        if self.sources.len() > MAX_MULTIVIEW_CELLS {
            return Err(MultiviewError::SourceLimitExceeded {
                actual: self.sources.len(),
                maximum: MAX_MULTIVIEW_CELLS,
            });
        }
        let capacity = self.layout.capacity();
        if self.sources.len() > capacity {
            return Err(MultiviewError::SourceCapacityExceeded {
                actual: self.sources.len(),
                capacity,
            });
        }
        for (index, source) in self.sources.iter().enumerate() {
            if self.sources[..index].contains(source) {
                return Err(MultiviewError::DuplicateSource { source: *source });
            }
        }
        if let ConfidenceMode::SingleInput(input) = self.confidence
            && !self.sources.contains(&MultiviewSource::Input(input))
        {
            return Err(MultiviewError::SingleInputNotPresent { input });
        }
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), MultiviewError> {
    if name.is_empty() {
        return Err(MultiviewError::EmptyName);
    }
    if name.len() > MAX_MULTIVIEW_NAME_BYTES {
        return Err(MultiviewError::NameTooLong {
            actual: name.len(),
            maximum: MAX_MULTIVIEW_NAME_BYTES,
        });
    }
    if let Some((index, _)) = name
        .char_indices()
        .find(|(_, character)| character.is_control())
    {
        return Err(MultiviewError::ControlCharacterInName { index });
    }
    Ok(())
}

/// One populated multiview cell with exact integer canvas geometry.
///
/// Cell bounds are half-open: the cell covers `x..x + width` and
/// `y..y + height`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultiviewCell {
    pub source: MultiviewSource,
    pub row: u8,
    pub column: u8,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl MultiviewCell {
    /// Reports whether the half-open cell rectangle contains `(x, y)`.
    #[must_use]
    pub const fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
}

/// One deterministic multiview plan with cells in row-major order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiviewPlan {
    cells: Vec<MultiviewCell>,
}

impl MultiviewPlan {
    #[must_use]
    pub fn cells(&self) -> &[MultiviewCell] {
        &self.cells
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    #[must_use]
    pub fn cell(&self, index: usize) -> Option<&MultiviewCell> {
        self.cells.get(index)
    }

    /// Returns the source of the cell containing the exact pixel `(x, y)`.
    ///
    /// Cell bounds are half-open, so a pixel on an internal cell border belongs
    /// to exactly one cell. Cells are searched in their stored row-major order
    /// and the first match wins; canvas pixels outside every populated cell
    /// return `None`.
    #[must_use]
    pub fn hit_test(&self, x: u32, y: u32) -> Option<MultiviewSource> {
        self.cells
            .iter()
            .find(|cell| cell.contains(x, y))
            .map(|cell| cell.source)
    }
}

/// Plans one multiview definition into exact integer cell rectangles.
///
/// Each column receives `canvas_width / columns` pixels, and each of the first
/// `canvas_width % columns` columns receives one extra pixel; rows follow the
/// same rule topmost-first. Cells are emitted in row-major order for the first
/// `sources.len()` grid positions. A fully populated grid tiles the canvas
/// exactly with no gaps or overlaps; remaining positions stay empty and show
/// the scene background when the compiled scene executes.
///
/// # Errors
///
/// Returns a typed error for an invalid definition, a zero canvas dimension, or
/// a canvas smaller than the grid.
pub fn plan_multiview(
    canvas_width: u32,
    canvas_height: u32,
    definition: &MultiviewDefinition,
) -> Result<MultiviewPlan, MultiviewError> {
    definition.validate()?;
    if canvas_width == 0 {
        return Err(MultiviewError::ZeroCanvasWidth);
    }
    if canvas_height == 0 {
        return Err(MultiviewError::ZeroCanvasHeight);
    }
    let columns = definition.layout.columns();
    let rows = definition.layout.rows();
    if canvas_width < u32::from(columns) || canvas_height < u32::from(rows) {
        return Err(MultiviewError::CanvasSmallerThanGrid {
            width: canvas_width,
            height: canvas_height,
            columns,
            rows,
        });
    }

    let column_base = canvas_width / u32::from(columns);
    let column_remainder = canvas_width % u32::from(columns);
    let row_base = canvas_height / u32::from(rows);
    let row_remainder = canvas_height % u32::from(rows);

    let mut cells = Vec::with_capacity(definition.sources.len());
    let mut y = 0_u32;
    'cells: for row in 0..rows {
        let height = row_base + u32::from(u32::from(row) < row_remainder);
        let mut x = 0_u32;
        for column in 0..columns {
            let index = usize::from(row) * usize::from(columns) + usize::from(column);
            if index >= definition.sources.len() {
                break 'cells;
            }
            let width = column_base + u32::from(u32::from(column) < column_remainder);
            cells.push(MultiviewCell {
                source: definition.sources[index],
                row,
                column,
                x,
                y,
                width,
                height,
            });
            x += width;
        }
        y += height;
    }
    Ok(MultiviewPlan { cells })
}

/// Compiles one multiview plan into a [`Scene`].
///
/// Every cell becomes one enabled [`SourceLayer`] at the cell's exact position
/// and size with `z` equal to its row-major index, so later cells composite on
/// top. Each layer receives an inset white border when `border_width` is
/// nonzero. `background` must be premultiplied RGBA8. A cell whose source
/// cannot be resolved is rejected instead of skipped.
///
/// The definition overlay flags are modeled but not rasterized: this reference
/// scene contains no labels, tally, clocks, or audio meters.
///
/// # Errors
///
/// Returns a typed error for a zero or out-of-bounds canvas, a malformed plan,
/// an unresolved cell source, or an invalid scene background.
pub fn compile_multiview_scene(
    plan: &MultiviewPlan,
    canvas_width: u32,
    canvas_height: u32,
    background: Rgba8,
    border_width: u32,
    resolve: impl Fn(MultiviewSource) -> Option<SourceId>,
) -> Result<Scene, MultiviewError> {
    if canvas_width == 0 {
        return Err(MultiviewError::ZeroCanvasWidth);
    }
    if canvas_height == 0 {
        return Err(MultiviewError::ZeroCanvasHeight);
    }
    if plan.cells.is_empty() {
        return Err(MultiviewError::EmptyPlan);
    }
    if plan.cells.len() > MAX_MULTIVIEW_CELLS {
        return Err(MultiviewError::TooManyCells {
            actual: plan.cells.len(),
            maximum: MAX_MULTIVIEW_CELLS,
        });
    }
    for (index, cell) in plan.cells.iter().enumerate() {
        let fits = cell.width > 0
            && cell.height > 0
            && cell
                .x
                .checked_add(cell.width)
                .is_some_and(|right| right <= canvas_width)
            && cell
                .y
                .checked_add(cell.height)
                .is_some_and(|bottom| bottom <= canvas_height);
        if !fits {
            return Err(MultiviewError::PlanExceedsCanvas { cell: index });
        }
    }

    let mut scene = Scene::new(canvas_width, canvas_height, background)?;
    for (index, cell) in plan.cells.iter().enumerate() {
        let source = resolve(cell.source).ok_or(MultiviewError::UnresolvedSource {
            source: cell.source,
        })?;
        let x = i32::try_from(cell.x)
            .map_err(|_| MultiviewError::CellPositionOutOfRange { cell: index })?;
        let y = i32::try_from(cell.y)
            .map_err(|_| MultiviewError::CellPositionOutOfRange { cell: index })?;
        let z = i32::try_from(index).map_err(|_| MultiviewError::TooManyCells {
            actual: plan.cells.len(),
            maximum: MAX_MULTIVIEW_CELLS,
        })?;
        let layer = SourceLayer::new(
            source,
            z,
            Transform::new(x, y, cell.width, cell.height, Rotation::Deg0),
        );
        scene.push_layer(if border_width == 0 {
            layer
        } else {
            layer.with_inset_border(border_width)
        });
    }
    Ok(scene)
}

/// Typed multiview validation, planning, and compilation errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MultiviewError {
    ZeroGridColumns,
    ZeroGridRows,
    GridColumnsExceeded {
        columns: u8,
        maximum: u8,
    },
    GridRowsExceeded {
        rows: u8,
        maximum: u8,
    },
    GridCapacityExceeded {
        cells: usize,
        maximum: usize,
    },
    EmptyName,
    NameTooLong {
        actual: usize,
        maximum: usize,
    },
    ControlCharacterInName {
        index: usize,
    },
    NoSources,
    SourceLimitExceeded {
        actual: usize,
        maximum: usize,
    },
    SourceCapacityExceeded {
        actual: usize,
        capacity: usize,
    },
    DuplicateSource {
        source: MultiviewSource,
    },
    SingleInputNotPresent {
        input: InputId,
    },
    ZeroCanvasWidth,
    ZeroCanvasHeight,
    CanvasSmallerThanGrid {
        width: u32,
        height: u32,
        columns: u8,
        rows: u8,
    },
    TooManyCells {
        actual: usize,
        maximum: usize,
    },
    EmptyPlan,
    PlanExceedsCanvas {
        cell: usize,
    },
    CellPositionOutOfRange {
        cell: usize,
    },
    UnresolvedSource {
        source: MultiviewSource,
    },
    Scene(SceneError),
}

impl fmt::Display for MultiviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroGridColumns => formatter.write_str("multiview grid columns must be nonzero"),
            Self::ZeroGridRows => formatter.write_str("multiview grid rows must be nonzero"),
            Self::GridColumnsExceeded { columns, maximum } => write!(
                formatter,
                "multiview grid has {columns} columns; maximum is {maximum}"
            ),
            Self::GridRowsExceeded { rows, maximum } => {
                write!(
                    formatter,
                    "multiview grid has {rows} rows; maximum is {maximum}"
                )
            }
            Self::GridCapacityExceeded { cells, maximum } => write!(
                formatter,
                "multiview grid requests {cells} cells; maximum is {maximum}"
            ),
            Self::EmptyName => formatter.write_str("multiview name must not be empty"),
            Self::NameTooLong { actual, maximum } => write!(
                formatter,
                "multiview name has {actual} bytes; maximum is {maximum}"
            ),
            Self::ControlCharacterInName { index } => write!(
                formatter,
                "multiview name contains a control character at byte {index}"
            ),
            Self::NoSources => formatter.write_str("multiview must select at least one source"),
            Self::SourceLimitExceeded { actual, maximum } => write!(
                formatter,
                "multiview selects {actual} sources; maximum is {maximum}"
            ),
            Self::SourceCapacityExceeded { actual, capacity } => write!(
                formatter,
                "multiview selects {actual} sources; the grid holds {capacity} cells"
            ),
            Self::DuplicateSource { source } => write!(
                formatter,
                "multiview cell source {source:?} appears more than once"
            ),
            Self::SingleInputNotPresent { input } => write!(
                formatter,
                "confidence input {input} is not a multiview cell source"
            ),
            Self::ZeroCanvasWidth => formatter.write_str("multiview canvas width must be nonzero"),
            Self::ZeroCanvasHeight => {
                formatter.write_str("multiview canvas height must be nonzero")
            }
            Self::CanvasSmallerThanGrid {
                width,
                height,
                columns,
                rows,
            } => write!(
                formatter,
                "multiview canvas {width}x{height} is smaller than the {columns}x{rows} grid"
            ),
            Self::TooManyCells { actual, maximum } => write!(
                formatter,
                "multiview plan has {actual} cells; maximum is {maximum}"
            ),
            Self::EmptyPlan => formatter.write_str("multiview plan has no cells"),
            Self::PlanExceedsCanvas { cell } => write!(
                formatter,
                "multiview plan cell {cell} does not fit inside the canvas"
            ),
            Self::CellPositionOutOfRange { cell } => write!(
                formatter,
                "multiview plan cell {cell} cannot be positioned in a scene transform"
            ),
            Self::UnresolvedSource { source } => write!(
                formatter,
                "multiview cell source {source:?} could not be resolved"
            ),
            Self::Scene(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MultiviewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Scene(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SceneError> for MultiviewError {
    fn from(value: SceneError) -> Self {
        Self::Scene(value)
    }
}
