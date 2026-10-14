//! Bounded, deterministic scope monitoring sessions.
//!
//! [`ScopeMonitor`] models the operator-facing state of color monitoring: the
//! monitored point, the selected scopes, an optional split between the live
//! picture and the scope surface, and the analysis cadence. The module never
//! renders; it produces bounded reports for callers that do.
//!
//! Analysis is deterministic: the same configuration, frame index, and pixel
//! data always produce the same report.

use core::fmt;
use core::ops::RangeInclusive;

use fm_types::InputId;
use fm_video::{ImageFrame, rgb_to_yuv};

use crate::{
    BinDimension, ColorMatrix, ColorMetadata, ColorRange, Histogram, HistogramConfig, LumaWaveform,
    MAX_HISTOGRAM_BINS, MAX_HORIZONTAL_BINS, MAX_LEVEL_BINS, MAX_VECTOR_BINS, ParadeConfig,
    RgbParade, ScopeError, Vectorscope, VectorscopeConfig, WaveformConfig, frame_pixels, histogram,
    luma_waveform, rgb_parade, validate_bins, validate_cells, vectorscope,
};

/// Maximum number of scope kinds a monitoring selection can hold.
pub const MAX_MONITOR_SCOPES: usize = 4;

/// Minimum analysis cadence, in frames.
pub const MIN_CADENCE_FRAMES: u32 = 1;

/// Maximum analysis cadence, in frames.
pub const MAX_CADENCE_FRAMES: u32 = 3_600;

/// The video point a monitoring session measures.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MonitoringPoint {
    /// The program output.
    Program,
    /// The preview output.
    Preview,
    /// A captured input, identified by its full-width [`InputId`].
    Input(InputId),
}

/// A scope kind that can be selected for monitoring.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MonitorScopeKind {
    /// Luma waveform.
    Waveform,
    /// RGB parade.
    Parade,
    /// U/V vectorscope.
    Vectorscope,
    /// Luma and RGB histograms.
    Histogram,
}

impl MonitorScopeKind {
    /// Every kind in canonical order.
    pub const ALL: [Self; MAX_MONITOR_SCOPES] = [
        Self::Waveform,
        Self::Parade,
        Self::Vectorscope,
        Self::Histogram,
    ];
}

/// A bounded, ordered, deduplicated set of scope kinds.
///
/// The set holds at most [`MAX_MONITOR_SCOPES`] kinds. Kinds are kept in
/// insertion order, so iteration is deterministic and stable for a given
/// sequence of operations. [`MonitorScopeSelection::DEFAULT`] and
/// [`MonitorScopeSelection::all`] contain every kind in canonical order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MonitorScopeSelection {
    kinds: [Option<MonitorScopeKind>; MAX_MONITOR_SCOPES],
    len: usize,
}

impl MonitorScopeSelection {
    /// Every scope kind in canonical order.
    pub const DEFAULT: Self = Self::all();

    /// Returns every scope kind in canonical order.
    #[must_use]
    pub const fn all() -> Self {
        let mut kinds: [Option<MonitorScopeKind>; MAX_MONITOR_SCOPES] = [None; MAX_MONITOR_SCOPES];
        let mut index = 0;
        while index < MonitorScopeKind::ALL.len() {
            kinds[index] = Some(MonitorScopeKind::ALL[index]);
            index += 1;
        }
        Self {
            kinds,
            len: MAX_MONITOR_SCOPES,
        }
    }

    /// Returns an empty selection.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            kinds: [None; MAX_MONITOR_SCOPES],
            len: 0,
        }
    }

    /// Inserts `kind` at the end of the selection order.
    ///
    /// Returns `true` when the kind was inserted, and `false` when the kind was
    /// already selected or the selection is full. Duplicate inserts are
    /// idempotent and never change order or length.
    #[must_use]
    pub fn insert(&mut self, kind: MonitorScopeKind) -> bool {
        if self.contains(kind) || self.len == MAX_MONITOR_SCOPES {
            return false;
        }
        self.kinds[self.len] = Some(kind);
        self.len += 1;
        true
    }

    /// Returns whether `kind` is selected.
    #[must_use]
    pub fn contains(&self, kind: MonitorScopeKind) -> bool {
        self.iter().any(|selected| selected == kind)
    }

    /// Iterates over the selected kinds in selection order.
    pub fn iter(&self) -> impl Iterator<Item = MonitorScopeKind> + '_ {
        self.kinds[..self.len].iter().flatten().copied()
    }

    /// Returns the number of selected kinds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no kind is selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for MonitorScopeSelection {
    /// Returns every scope kind, the same as [`MonitorScopeSelection::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl<'a> IntoIterator for &'a MonitorScopeSelection {
    type Item = MonitorScopeKind;
    type IntoIter =
        core::iter::Copied<core::iter::Flatten<core::slice::Iter<'a, Option<MonitorScopeKind>>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.kinds[..self.len].iter().flatten().copied()
    }
}

/// A named band of the false-color luma palette.
///
/// The palette partitions every luma code `0..=255` into exactly one band,
/// anchored at the limited-range legal code endpoints:
///
/// | Band | Luma codes | Meaning |
/// | ---- | ---------- | ------- |
/// | [`FalseColorBand::Purple`] | `0..=15` | below legal black |
/// | [`FalseColorBand::Blue`] | `16..=63` | shadows |
/// | [`FalseColorBand::Green`] | `64..=127` | lower midtones |
/// | [`FalseColorBand::Yellow`] | `128..=191` | upper midtones |
/// | [`FalseColorBand::Orange`] | `192..=234` | highlights below legal white |
/// | [`FalseColorBand::Red`] | `235..=255` | legal white and above |
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FalseColorBand {
    Purple,
    Blue,
    Green,
    Yellow,
    Orange,
    Red,
}

impl FalseColorBand {
    /// Number of bands in the palette.
    pub const COUNT: usize = 6;

    /// Every band in palette order, from black to white.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Purple,
        Self::Blue,
        Self::Green,
        Self::Yellow,
        Self::Orange,
        Self::Red,
    ];

    /// Returns the inclusive luma-code range of the band.
    #[must_use]
    pub const fn range(self) -> RangeInclusive<u8> {
        match self {
            Self::Purple => 0..=15,
            Self::Blue => 16..=63,
            Self::Green => 64..=127,
            Self::Yellow => 128..=191,
            Self::Orange => 192..=234,
            Self::Red => 235..=255,
        }
    }

    /// Returns the human-readable band name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Purple => "purple",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Orange => "orange",
            Self::Red => "red",
        }
    }

    /// Returns the palette index `0..=5`, ordered from black to white.
    #[must_use]
    pub const fn code(self) -> usize {
        match self {
            Self::Purple => 0,
            Self::Blue => 1,
            Self::Green => 2,
            Self::Yellow => 3,
            Self::Orange => 4,
            Self::Red => 5,
        }
    }
}

/// Maps every luma code to exactly one [`FalseColorBand`].
#[must_use]
pub const fn false_color_band(luma: u8) -> FalseColorBand {
    match luma {
        0..=15 => FalseColorBand::Purple,
        16..=63 => FalseColorBand::Blue,
        64..=127 => FalseColorBand::Green,
        128..=191 => FalseColorBand::Yellow,
        192..=234 => FalseColorBand::Orange,
        235..=255 => FalseColorBand::Red,
    }
}

/// Per-band pixel counts derived from one analysed frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FalseColorCounts {
    counts: [u64; FalseColorBand::COUNT],
}

impl FalseColorCounts {
    /// Returns the count of every band in palette order.
    #[must_use]
    pub const fn counts(&self) -> &[u64; FalseColorBand::COUNT] {
        &self.counts
    }

    /// Returns the number of pixels in `band`.
    #[must_use]
    pub const fn count(&self, band: FalseColorBand) -> u64 {
        self.counts[band.code()]
    }

    /// Returns the total number of counted pixels.
    #[must_use]
    pub const fn total(&self) -> u64 {
        let mut total = 0;
        let mut index = 0;
        while index < self.counts.len() {
            total += self.counts[index];
            index += 1;
        }
        total
    }
}

/// Orientation of the boundary between the live picture and the scope surface.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SplitOrientation {
    /// A horizontal boundary: the video sits above the scope.
    Horizontal,
    /// A vertical boundary: the video sits left of the scope.
    Vertical,
}

/// How a live picture and a scope share an operator surface.
///
/// Both parts are measured in basis points (1/100 of a percent) of the shared
/// surface and must sum to [`ScopeSplitView::FULL_BASIS_POINTS`]. The model
/// describes the split; it does not render it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ScopeSplitView {
    orientation: SplitOrientation,
    video_basis_points: u16,
    scope_basis_points: u16,
}

impl ScopeSplitView {
    /// Basis points in a complete surface (100%).
    pub const FULL_BASIS_POINTS: u16 = 10_000;

    /// Creates a split view for a shared surface.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::InvalidSplit`] when either part is zero or when
    /// the parts do not sum to [`Self::FULL_BASIS_POINTS`].
    pub fn new(
        orientation: SplitOrientation,
        video_basis_points: u16,
        scope_basis_points: u16,
    ) -> Result<Self, MonitorError> {
        let view = Self {
            orientation,
            video_basis_points,
            scope_basis_points,
        };
        view.validate()?;
        Ok(view)
    }

    /// Returns the orientation of the boundary.
    #[must_use]
    pub const fn orientation(self) -> SplitOrientation {
        self.orientation
    }

    /// Returns the video part of the surface in basis points.
    #[must_use]
    pub const fn video_basis_points(self) -> u16 {
        self.video_basis_points
    }

    /// Returns the scope part of the surface in basis points.
    #[must_use]
    pub const fn scope_basis_points(self) -> u16 {
        self.scope_basis_points
    }

    fn validate(self) -> Result<(), MonitorError> {
        let total = u32::from(self.video_basis_points) + u32::from(self.scope_basis_points);
        if self.video_basis_points == 0
            || self.scope_basis_points == 0
            || total != u32::from(Self::FULL_BASIS_POINTS)
        {
            return Err(MonitorError::InvalidSplit {
                video_basis_points: self.video_basis_points,
                scope_basis_points: self.scope_basis_points,
                expected: Self::FULL_BASIS_POINTS,
            });
        }
        Ok(())
    }
}

/// Errors returned by monitoring configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorError {
    /// The analysis cadence is outside the accepted range.
    CadenceOutOfRange {
        requested: u32,
        minimum: u32,
        maximum: u32,
    },
    /// A split view part is zero or the parts do not fill the shared surface.
    InvalidSplit {
        video_basis_points: u16,
        scope_basis_points: u16,
        expected: u16,
    },
    /// An embedded scope configuration is invalid.
    Scope(ScopeError),
}

impl fmt::Display for MonitorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CadenceOutOfRange {
                requested,
                minimum,
                maximum,
            } => write!(
                formatter,
                "analysis cadence {requested} is outside {minimum}..={maximum} frames"
            ),
            Self::InvalidSplit {
                video_basis_points,
                scope_basis_points,
                expected,
            } => write!(
                formatter,
                "split view parts {video_basis_points} + {scope_basis_points} must both be \
                 nonzero and sum to {expected} basis points"
            ),
            Self::Scope(error) => write!(formatter, "invalid scope configuration: {error}"),
        }
    }
}

impl std::error::Error for MonitorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Scope(error) => Some(error),
            Self::CadenceOutOfRange { .. } | Self::InvalidSplit { .. } => None,
        }
    }
}

/// Complete configuration for a scope monitoring session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeMonitorConfig {
    /// The video point measured by every selected scope.
    pub point: MonitoringPoint,
    /// The selected scopes, in stable order.
    pub scopes: MonitorScopeSelection,
    /// Optional split between the live picture and the scope surface.
    pub split: Option<ScopeSplitView>,
    /// Analyse one frame out of every `cadence_frames` frames.
    pub cadence_frames: u32,
    /// Luma waveform configuration.
    pub waveform: WaveformConfig,
    /// RGB parade configuration.
    pub parade: ParadeConfig,
    /// Vectorscope configuration.
    pub vectorscope: VectorscopeConfig,
    /// Histogram configuration.
    pub histogram: HistogramConfig,
}

impl ScopeMonitorConfig {
    /// Validates the cadence, the optional split view, and every embedded scope
    /// configuration.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::CadenceOutOfRange`] when the cadence is outside
    /// `MIN_CADENCE_FRAMES..=MAX_CADENCE_FRAMES`, [`MonitorError::InvalidSplit`]
    /// when the split view does not fill the shared surface, and
    /// [`MonitorError::Scope`] when an embedded configuration is rejected by
    /// the corresponding scope function.
    pub fn validate(&self) -> Result<(), MonitorError> {
        validate_cadence(self.cadence_frames)?;
        if let Some(split) = self.split {
            split.validate()?;
        }
        validate_waveform(self.waveform).map_err(MonitorError::Scope)?;
        validate_parade(self.parade).map_err(MonitorError::Scope)?;
        validate_vectorscope(self.vectorscope).map_err(MonitorError::Scope)?;
        validate_histogram(self.histogram).map_err(MonitorError::Scope)?;
        Ok(())
    }
}

impl Default for ScopeMonitorConfig {
    /// Returns the program point, every scope, no split, a one-frame cadence,
    /// and 64-bin BT.709 limited-range scope configurations.
    fn default() -> Self {
        Self {
            point: MonitoringPoint::Program,
            scopes: MonitorScopeSelection::DEFAULT,
            split: None,
            cadence_frames: MIN_CADENCE_FRAMES,
            waveform: WaveformConfig {
                horizontal_bins: DEFAULT_BINS,
                level_bins: DEFAULT_BINS,
                color: DEFAULT_COLOR,
            },
            parade: ParadeConfig {
                horizontal_bins: DEFAULT_BINS,
                level_bins: DEFAULT_BINS,
                color: DEFAULT_COLOR,
            },
            vectorscope: VectorscopeConfig {
                bins: DEFAULT_BINS,
                color: DEFAULT_COLOR,
            },
            histogram: HistogramConfig {
                bins: DEFAULT_BINS,
                color: DEFAULT_COLOR,
            },
        }
    }
}

/// A validated, deterministic scope monitoring session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeMonitor {
    config: ScopeMonitorConfig,
}

impl ScopeMonitor {
    /// Creates a session from a validated configuration.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`ScopeMonitorConfig::validate`].
    pub fn new(config: ScopeMonitorConfig) -> Result<Self, MonitorError> {
        config.validate()?;
        Ok(Self { config })
    }

    /// Returns the session configuration.
    #[must_use]
    pub const fn config(&self) -> &ScopeMonitorConfig {
        &self.config
    }

    /// Returns the monitored point.
    #[must_use]
    pub const fn point(&self) -> MonitoringPoint {
        self.config.point
    }

    /// Returns the selected scope kinds.
    #[must_use]
    pub const fn scopes(&self) -> MonitorScopeSelection {
        self.config.scopes
    }

    /// Returns the split between the live picture and the scope surface.
    #[must_use]
    pub const fn split(&self) -> Option<ScopeSplitView> {
        self.config.split
    }

    /// Returns the analysis cadence in frames.
    #[must_use]
    pub const fn cadence_frames(&self) -> u32 {
        self.config.cadence_frames
    }

    /// Replaces the monitored point.
    pub fn set_point(&mut self, point: MonitoringPoint) {
        self.config.point = point;
    }

    /// Replaces the analysis cadence.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::CadenceOutOfRange`] when `cadence_frames` is
    /// outside `MIN_CADENCE_FRAMES..=MAX_CADENCE_FRAMES`; the previous cadence
    /// is kept on error.
    pub fn set_cadence_frames(&mut self, cadence_frames: u32) -> Result<(), MonitorError> {
        validate_cadence(cadence_frames)?;
        self.config.cadence_frames = cadence_frames;
        Ok(())
    }

    /// Analyses `frame` when the cadence selects `frame_index`.
    ///
    /// Only frame indices where `frame_index % cadence_frames == 0` are
    /// analysed; frame zero always analyses. Other indices return [`None`]
    /// without reading the frame.
    ///
    /// The configuration is validated when the session is created, so selected
    /// scope computations cannot fail; a result that still fails is reported as
    /// [`None`] for that field. False-color tallies always interpret luma with
    /// the waveform configuration's color metadata.
    #[must_use]
    pub fn analyse(&self, frame_index: u64, frame: &ImageFrame) -> Option<ScopeMonitorReport> {
        if !frame_index.is_multiple_of(u64::from(self.config.cadence_frames)) {
            return None;
        }
        let scopes = self.config.scopes;
        let waveform = if scopes.contains(MonitorScopeKind::Waveform) {
            luma_waveform(frame, self.config.waveform).ok()
        } else {
            None
        };
        let parade = if scopes.contains(MonitorScopeKind::Parade) {
            rgb_parade(frame, self.config.parade).ok()
        } else {
            None
        };
        let vectorscope = if scopes.contains(MonitorScopeKind::Vectorscope) {
            vectorscope(frame, self.config.vectorscope).ok()
        } else {
            None
        };
        let histogram = if scopes.contains(MonitorScopeKind::Histogram) {
            histogram(frame, self.config.histogram).ok()
        } else {
            None
        };
        Some(ScopeMonitorReport {
            frame_index,
            point: self.config.point,
            split: self.config.split,
            false_color: Some(false_color_counts(frame, self.config.waveform.color)),
            waveform,
            parade,
            vectorscope,
            histogram,
        })
    }
}

/// A bounded analysis result for one selected frame index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeMonitorReport {
    frame_index: u64,
    point: MonitoringPoint,
    split: Option<ScopeSplitView>,
    false_color: Option<FalseColorCounts>,
    waveform: Option<LumaWaveform>,
    parade: Option<RgbParade>,
    vectorscope: Option<Vectorscope>,
    histogram: Option<Histogram>,
}

impl ScopeMonitorReport {
    /// Returns the analysed frame index.
    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Returns the monitored point.
    #[must_use]
    pub const fn point(&self) -> MonitoringPoint {
        self.point
    }

    /// Returns the split between the live picture and the scope surface.
    #[must_use]
    pub const fn split(&self) -> Option<ScopeSplitView> {
        self.split
    }

    /// Returns the false-color per-band pixel tally.
    ///
    /// Always present in a report produced by [`ScopeMonitor::analyse`].
    #[must_use]
    pub const fn false_color(&self) -> Option<FalseColorCounts> {
        self.false_color
    }

    /// Returns the luma waveform when it was selected.
    #[must_use]
    pub const fn waveform(&self) -> Option<&LumaWaveform> {
        self.waveform.as_ref()
    }

    /// Returns the RGB parade when it was selected.
    #[must_use]
    pub const fn parade(&self) -> Option<&RgbParade> {
        self.parade.as_ref()
    }

    /// Returns the vectorscope when it was selected.
    #[must_use]
    pub const fn vectorscope(&self) -> Option<&Vectorscope> {
        self.vectorscope.as_ref()
    }

    /// Returns the histogram when it was selected.
    #[must_use]
    pub const fn histogram(&self) -> Option<&Histogram> {
        self.histogram.as_ref()
    }
}

/// Default bucket count for each scope axis.
const DEFAULT_BINS: usize = 64;

/// Default color interpretation: BT.709 limited range.
const DEFAULT_COLOR: ColorMetadata = ColorMetadata::new(ColorMatrix::Bt709, ColorRange::Limited);

fn validate_cadence(cadence_frames: u32) -> Result<(), MonitorError> {
    if !(MIN_CADENCE_FRAMES..=MAX_CADENCE_FRAMES).contains(&cadence_frames) {
        return Err(MonitorError::CadenceOutOfRange {
            requested: cadence_frames,
            minimum: MIN_CADENCE_FRAMES,
            maximum: MAX_CADENCE_FRAMES,
        });
    }
    Ok(())
}

fn validate_waveform(config: WaveformConfig) -> Result<(), ScopeError> {
    validate_bins(
        config.horizontal_bins,
        MAX_HORIZONTAL_BINS,
        BinDimension::Horizontal,
    )?;
    validate_bins(config.level_bins, MAX_LEVEL_BINS, BinDimension::Level)?;
    validate_cells(config.horizontal_bins, config.level_bins, 1)?;
    Ok(())
}

fn validate_parade(config: ParadeConfig) -> Result<(), ScopeError> {
    validate_bins(
        config.horizontal_bins,
        MAX_HORIZONTAL_BINS,
        BinDimension::Horizontal,
    )?;
    validate_bins(config.level_bins, MAX_LEVEL_BINS, BinDimension::Level)?;
    validate_cells(config.horizontal_bins, config.level_bins, 3)?;
    Ok(())
}

fn validate_vectorscope(config: VectorscopeConfig) -> Result<(), ScopeError> {
    validate_bins(config.bins, MAX_VECTOR_BINS, BinDimension::Vector)?;
    validate_cells(config.bins, config.bins, 1)?;
    Ok(())
}

fn validate_histogram(config: HistogramConfig) -> Result<(), ScopeError> {
    validate_bins(config.bins, MAX_HISTOGRAM_BINS, BinDimension::Histogram)?;
    validate_cells(config.bins, 4, 1)?;
    Ok(())
}

fn false_color_counts(frame: &ImageFrame, color: ColorMetadata) -> FalseColorCounts {
    let mut counts = [0_u64; FalseColorBand::COUNT];
    for (_, pixel) in frame_pixels(frame) {
        let luma = rgb_to_yuv(pixel, color.matrix, color.range).y;
        counts[false_color_band(luma).code()] += 1;
    }
    FalseColorCounts { counts }
}
