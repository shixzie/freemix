//! Frame-accurate resolution results shared by the event database, the rolling
//! segment catalog, the synchronized source timeline, and the replay decks.

use fm_frame::NormalizedTimestamp;

use crate::{
    CameraId, EventId, ReplayChannelId, ReplayError, ReplayMarks, SegmentId, SourceObservation,
    TimelineRange,
};

/// Operator last-N presets surfaced as one-touch replay buttons.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LastNPreset {
    Last5,
    Last10,
    Last20,
}

impl LastNPreset {
    pub const ALL: [Self; 3] = [Self::Last5, Self::Last10, Self::Last20];

    #[must_use]
    pub const fn seconds(self) -> u64 {
        match self {
            Self::Last5 => 5,
            Self::Last10 => 10,
            Self::Last20 => 20,
        }
    }

    /// Returns the preset range ending exactly at `live_edge`.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError::InvalidLastN`] if the range would underflow the
    /// normalized timeline.
    pub fn range(self, live_edge: NormalizedTimestamp) -> Result<TimelineRange, ReplayError> {
        ReplayMarks::last_n(live_edge, self.seconds())
    }
}

/// Recorded-segment coverage for one camera over one timeline range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmentCoverage {
    pub camera_id: CameraId,
    pub range: TimelineRange,
    /// Overlapping segments ordered by timeline start, recorder index, then ID.
    pub segments: Vec<SegmentId>,
    /// Uncovered sub-ranges in ascending order.
    pub gaps: Vec<TimelineRange>,
}

impl SegmentCoverage {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }
}

/// Source frames observed for one camera inside a half-open range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameSpan {
    pub camera_id: CameraId,
    pub first: SourceObservation,
    pub last: SourceObservation,
    pub frame_count: usize,
    /// Number of discontinuity epochs crossed between `first` and `last`.
    pub discontinuities: u64,
}

/// One synchronized angle of a captured event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AngleSpan {
    pub frames: FrameSpan,
    pub segments: Vec<SegmentId>,
}

/// Result of atomically capturing and protecting a multi-angle event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedEvent {
    pub event_id: EventId,
    pub timeline: TimelineRange,
    /// Angles in ascending camera-ID order.
    pub angles: Vec<AngleSpan>,
}

/// The exact source frame a replay channel presents at its current position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelFrame {
    pub channel: ReplayChannelId,
    pub camera_id: CameraId,
    pub live: bool,
    /// Synchronized time requested (cursor, or live edge in live mode).
    pub target: NormalizedTimestamp,
    /// Frame displayed at `target`: the latest observation at or before it.
    pub observation: SourceObservation,
    pub segment_id: SegmentId,
    /// Distance from the displayed frame's synchronized time to `target`.
    pub offset_nanos: u64,
}
