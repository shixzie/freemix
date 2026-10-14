//! Synchronized Quad View and Replay Multi-View tile planning.
//!
//! The planner never decodes media. It resolves, for one replay channel
//! position, the exact source frame and segment each angle presents by reusing
//! [`ReplayDecks::resolve_frame`], then lays the results out as an ordered,
//! row-major tile plan with live/recorded labels.

use core::fmt;
use std::collections::BTreeSet;

use fm_frame::NormalizedTimestamp;

use crate::{
    CameraId, ChannelFrame, ChannelMode, EventId, MAX_REPLAY_SOURCES, ReplayChannelId, ReplayDecks,
    ReplayError, RollingSegmentCatalog, SourceTimeline,
};

/// Angle tiles in a Quad View.
pub const QUAD_VIEW_ANGLES: usize = 4;
/// Maximum angle tiles in a Replay Multi-View.
pub const MAX_MULTIVIEW_ANGLES: usize = MAX_REPLAY_SOURCES;
/// Maximum tiles in a Replay Multi-View: eight angles plus channels A and B.
pub const MAX_MULTIVIEW_TILES: usize = MAX_MULTIVIEW_ANGLES + 2;

/// Live/recorded label shown on a tile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileMode {
    Live,
    Recorded { event_id: EventId },
}

impl TileMode {
    #[must_use]
    pub const fn from_channel_mode(mode: ChannelMode) -> Self {
        match mode {
            ChannelMode::Live => Self::Live,
            ChannelMode::Recorded { event_id, .. } => Self::Recorded { event_id },
        }
    }

    /// Short operator-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Live => "LIVE",
            Self::Recorded { .. } => "REPLAY",
        }
    }
}

/// What a tile presents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TileSource {
    /// One camera angle at the driving channel's synchronized position.
    Angle(CameraId),
    /// A replay channel's own output (its own angle and position).
    Channel(ReplayChannelId),
}

/// One placed, resolved tile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultiviewTile {
    /// Zero-based position in row-major order.
    pub index: usize,
    pub row: usize,
    pub column: usize,
    pub source: TileSource,
    pub mode: TileMode,
    /// Exact frame and segment resolved for the tile.
    pub frame: ChannelFrame,
}

/// Ordered tile plan for one synchronized instant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiviewPlan {
    /// Channel whose position drives the angle tiles.
    pub driver: ReplayChannelId,
    pub driver_mode: TileMode,
    /// Synchronized time requested for every angle tile.
    pub target: NormalizedTimestamp,
    pub rows: usize,
    pub columns: usize,
    /// Tiles in row-major order.
    pub tiles: Vec<MultiviewTile>,
    /// Spread between the earliest and latest displayed angle frame.
    pub skew_nanos: u64,
    pub tolerance_nanos: u64,
    /// `skew_nanos <= tolerance_nanos`.
    pub in_sync: bool,
}

impl MultiviewPlan {
    /// Angle tiles in plan order.
    pub fn angle_tiles(&self) -> impl Iterator<Item = &MultiviewTile> {
        self.tiles
            .iter()
            .filter(|tile| matches!(tile.source, TileSource::Angle(_)))
    }
}

/// Validated Replay Multi-View configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiviewLayout {
    channels: Vec<ReplayChannelId>,
    angles: Vec<CameraId>,
}

impl MultiviewLayout {
    /// Creates a layout: channel tiles first (in the given order), then angle
    /// tiles in the given order.
    ///
    /// # Errors
    ///
    /// At least one angle and at most [`MAX_MULTIVIEW_ANGLES`] are allowed;
    /// angles and channels must each be distinct.
    pub fn new(
        channels: Vec<ReplayChannelId>,
        angles: Vec<CameraId>,
    ) -> Result<Self, MultiviewError> {
        if angles.is_empty() {
            return Err(MultiviewError::NoAngles);
        }
        if angles.len() > MAX_MULTIVIEW_ANGLES {
            return Err(MultiviewError::TooManyAngles(angles.len()));
        }
        let mut seen = BTreeSet::new();
        for angle in &angles {
            if !seen.insert(*angle) {
                return Err(MultiviewError::DuplicateAngle(*angle));
            }
        }
        for (index, channel) in channels.iter().enumerate() {
            if channels[..index].contains(channel) {
                return Err(MultiviewError::DuplicateChannel(*channel));
            }
        }
        Ok(Self { channels, angles })
    }

    /// Creates the 2x2 Quad View layout of four distinct angles.
    ///
    /// # Errors
    ///
    /// Angles must be distinct.
    pub fn quad(angles: [CameraId; QUAD_VIEW_ANGLES]) -> Result<Self, MultiviewError> {
        Self::new(Vec::new(), angles.to_vec())
    }

    #[must_use]
    pub fn channels(&self) -> &[ReplayChannelId] {
        &self.channels
    }

    #[must_use]
    pub fn angles(&self) -> &[CameraId] {
        &self.angles
    }

    #[must_use]
    pub fn tile_count(&self) -> usize {
        self.channels.len() + self.angles.len()
    }

    /// Smallest near-square grid: `columns = ceil(sqrt(n))`,
    /// `rows = ceil(n / columns)`. Four tiles produce 2x2.
    #[must_use]
    pub fn grid(&self) -> (usize, usize) {
        let count = self.tile_count();
        let mut columns = 1;
        while columns * columns < count {
            columns += 1;
        }
        (count.div_ceil(columns), columns)
    }

    /// Resolves every tile at the driving channel's position: its cursor in
    /// recorded mode or `live_edge` in live mode. Channel tiles resolve their
    /// own channel's angle and position.
    ///
    /// # Errors
    ///
    /// Any tile whose frame or segment cannot be resolved fails the whole plan
    /// with the tile source identified.
    pub fn plan(
        &self,
        decks: &ReplayDecks,
        driver: ReplayChannelId,
        live_edge: NormalizedTimestamp,
        timeline: &SourceTimeline,
        catalog: &RollingSegmentCatalog,
        tolerance_nanos: u64,
    ) -> Result<MultiviewPlan, MultiviewError> {
        let (rows, columns) = self.grid();
        let driver_mode = TileMode::from_channel_mode(decks.channel(driver).mode);
        let mut tiles = Vec::with_capacity(self.tile_count());
        let sources = self
            .channels
            .iter()
            .map(|channel| TileSource::Channel(*channel))
            .chain(self.angles.iter().map(|angle| TileSource::Angle(*angle)));
        let mut probe = decks.clone();
        for (index, source) in sources.enumerate() {
            let (frame, mode) = match source {
                TileSource::Channel(channel) => (
                    decks.resolve_frame(channel, live_edge, timeline, catalog),
                    TileMode::from_channel_mode(decks.channel(channel).mode),
                ),
                TileSource::Angle(camera_id) => {
                    probe.select_angle(driver, camera_id);
                    (
                        probe.resolve_frame(driver, live_edge, timeline, catalog),
                        driver_mode,
                    )
                }
            };
            let frame = frame.map_err(|error| MultiviewError::Unresolved { source, error })?;
            tiles.push(MultiviewTile {
                index,
                row: index / columns,
                column: index % columns,
                source,
                mode,
                frame,
            });
        }
        let target = match driver_mode {
            TileMode::Live => live_edge,
            TileMode::Recorded { .. } => decks.channel(driver).cursor,
        };
        let angle_times = tiles
            .iter()
            .filter(|tile| matches!(tile.source, TileSource::Angle(_)))
            .map(|tile| i128::from(tile.frame.observation.synchronized_at.as_nanos()));
        let (minimum, maximum) = angle_times.fold((i128::MAX, i128::MIN), |(low, high), time| {
            (low.min(time), high.max(time))
        });
        let skew_nanos = u64::try_from(maximum - minimum).unwrap_or(u64::MAX);
        Ok(MultiviewPlan {
            driver,
            driver_mode,
            target,
            rows,
            columns,
            tiles,
            skew_nanos,
            tolerance_nanos,
            in_sync: skew_nanos <= tolerance_nanos,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MultiviewError {
    NoAngles,
    TooManyAngles(usize),
    DuplicateAngle(CameraId),
    DuplicateChannel(ReplayChannelId),
    Unresolved {
        source: TileSource,
        error: ReplayError,
    },
}

impl fmt::Display for MultiviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAngles => formatter.write_str("multiview requires at least one angle"),
            Self::TooManyAngles(count) => write!(
                formatter,
                "{count} multiview angles exceed the limit of {MAX_MULTIVIEW_ANGLES}"
            ),
            Self::DuplicateAngle(id) => write!(formatter, "camera {id} occurs more than once"),
            Self::DuplicateChannel(id) => {
                write!(formatter, "replay channel {id:?} occurs more than once")
            }
            Self::Unresolved { source, error } => {
                write!(formatter, "multiview tile {source:?} unresolved: {error}")
            }
        }
    }
}

impl std::error::Error for MultiviewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unresolved { error, .. } => Some(error),
            _ => None,
        }
    }
}
