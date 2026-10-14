use core::{fmt, num::NonZeroU128};

use fm_frame::NormalizedTimestamp;

macro_rules! stable_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU128);

        impl $name {
            #[must_use]
            pub const fn new(value: NonZeroU128) -> Self {
                Self(value)
            }

            #[must_use]
            pub const fn get(self) -> NonZeroU128 {
                self.0
            }
        }

        impl From<NonZeroU128> for $name {
            fn from(value: NonZeroU128) -> Self {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

stable_id!(CameraId);
stable_id!(StorageRootId);
stable_id!(SegmentId);
stable_id!(EventId);
stable_id!(ListId);
stable_id!(FolderId);
stable_id!(HighlightId);
stable_id!(MusicTrackId);
stable_id!(ExportJobId);

/// Maximum events retained by one [`crate::EventDatabase`].
pub const MAX_EVENTS: usize = 4_096;
/// Maximum named event lists (the product contract requires at least 20).
pub const MAX_LISTS: usize = 64;
/// Maximum entries in one event list.
pub const MAX_EVENTS_PER_LIST: usize = MAX_EVENTS;
/// Maximum event folders.
pub const MAX_FOLDERS: usize = 256;
/// Maximum distinct tags on one event.
pub const MAX_TAGS_PER_EVENT: usize = 32;
/// Maximum UTF-8 bytes in an event, list, folder, or tag name.
pub const MAX_NAME_BYTES: usize = 256;
/// Maximum UTF-8 bytes in an event note.
pub const MAX_NOTE_BYTES: usize = 4_096;
/// Default per-camera observation bound: four hours at 240 frames per second.
pub const MAX_OBSERVATIONS_PER_CAMERA: usize = 3_456_000;

/// Identifies which explicit replay bound was exceeded.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReplayLimit {
    Events,
    Lists,
    EventsPerList,
    Folders,
    TagsPerEvent,
    NameBytes,
    NoteBytes,
    Observations,
}

impl fmt::Display for ReplayLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Events => "events",
            Self::Lists => "event lists",
            Self::EventsPerList => "events per list",
            Self::Folders => "event folders",
            Self::TagsPerEvent => "tags per event",
            Self::NameBytes => "name bytes",
            Self::NoteBytes => "note bytes",
            Self::Observations => "observations per camera",
        })
    }
}

/// Half-open range on the shared replay timeline.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TimelineRange {
    pub start: NormalizedTimestamp,
    pub end: NormalizedTimestamp,
}

impl TimelineRange {
    /// Creates a non-empty half-open range.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError::InvalidRange`] unless `start < end`.
    pub fn new(start: NormalizedTimestamp, end: NormalizedTimestamp) -> Result<Self, ReplayError> {
        if start >= end {
            return Err(ReplayError::InvalidRange);
        }
        Ok(Self { start, end })
    }

    #[must_use]
    pub fn duration_nanos(self) -> u64 {
        let duration = i128::from(self.end.as_nanos()) - i128::from(self.start.as_nanos());
        u64::try_from(duration).unwrap_or(u64::MAX)
    }

    #[must_use]
    pub const fn contains(self, timestamp: NormalizedTimestamp) -> bool {
        timestamp.as_nanos() >= self.start.as_nanos() && timestamp.as_nanos() < self.end.as_nanos()
    }

    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start.as_nanos() < other.end.as_nanos() && other.start.as_nanos() < self.end.as_nanos()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayError {
    InvalidRange,
    EmptyName,
    TooManySources(usize),
    TooManyAudioChannels(usize),
    DuplicateCamera(CameraId),
    DuplicateStorageRoot(StorageRootId),
    DuplicateSegment(SegmentId),
    DuplicateEvent(EventId),
    DuplicateList(ListId),
    DuplicateFolder(FolderId),
    DuplicateHighlight(HighlightId),
    DuplicateExport(ExportJobId),
    UnknownCamera(CameraId),
    UnknownStorageRoot(StorageRootId),
    UnknownSegment(SegmentId),
    UnknownEvent(EventId),
    UnknownList(ListId),
    UnknownFolder(FolderId),
    UnknownExport(ExportJobId),
    CameraRootMismatch,
    TimelineRegression(CameraId),
    SequenceRegression(CameraId),
    DuplicateQuadCamera(CameraId),
    ObservationUnavailable(CameraId),
    MarkIncomplete,
    InvalidLastN,
    PreferredAngleUnavailable(CameraId),
    EventAlreadyInList {
        event: EventId,
        list: ListId,
    },
    InvalidPlaybackRate(i32),
    FrameDurationZero,
    ExportStateTransition,
    LimitExceeded {
        limit: ReplayLimit,
        maximum: usize,
    },
    AngleNotInEvent {
        event: EventId,
        camera: CameraId,
    },
    CoverageGap {
        camera: CameraId,
        gap: TimelineRange,
    },
    NoFramesInRange(CameraId),
    SegmentUnavailable {
        camera: CameraId,
        at: NormalizedTimestamp,
    },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange => {
                formatter.write_str("timeline range must be non-empty and ordered")
            }
            Self::EmptyName => formatter.write_str("name must not be empty"),
            Self::TooManySources(count) => {
                write!(formatter, "{count} replay sources exceed the limit")
            }
            Self::TooManyAudioChannels(count) => write!(
                formatter,
                "{count} audio channels exceed the replay source limit"
            ),
            Self::DuplicateCamera(id) => write!(formatter, "duplicate replay camera {id}"),
            Self::DuplicateStorageRoot(id) => write!(formatter, "duplicate storage root {id}"),
            Self::DuplicateSegment(id) => write!(formatter, "duplicate replay segment {id}"),
            Self::DuplicateEvent(id) => write!(formatter, "duplicate replay event {id}"),
            Self::DuplicateList(id) => write!(formatter, "duplicate replay list {id}"),
            Self::DuplicateFolder(id) => write!(formatter, "duplicate event folder {id}"),
            Self::DuplicateHighlight(id) => write!(formatter, "duplicate highlight item {id}"),
            Self::DuplicateExport(id) => write!(formatter, "duplicate export job {id}"),
            Self::UnknownCamera(id) => write!(formatter, "unknown replay camera {id}"),
            Self::UnknownStorageRoot(id) => write!(formatter, "unknown storage root {id}"),
            Self::UnknownSegment(id) => write!(formatter, "unknown replay segment {id}"),
            Self::UnknownEvent(id) => write!(formatter, "unknown replay event {id}"),
            Self::UnknownList(id) => write!(formatter, "unknown replay list {id}"),
            Self::UnknownFolder(id) => write!(formatter, "unknown event folder {id}"),
            Self::UnknownExport(id) => write!(formatter, "unknown export job {id}"),
            Self::CameraRootMismatch => {
                formatter.write_str("segment storage root does not match its camera")
            }
            Self::TimelineRegression(id) => write!(formatter, "timeline regressed for camera {id}"),
            Self::SequenceRegression(id) => {
                write!(formatter, "source sequence did not advance for camera {id}")
            }
            Self::DuplicateQuadCamera(id) => {
                write!(formatter, "camera {id} occurs more than once in quad view")
            }
            Self::ObservationUnavailable(id) => write!(
                formatter,
                "camera {id} has no observation at the requested time"
            ),
            Self::MarkIncomplete => formatter.write_str("mark-in and mark-out are both required"),
            Self::InvalidLastN => {
                formatter.write_str("last-N duration must be positive and fit the timeline")
            }
            Self::PreferredAngleUnavailable(id) => {
                write!(formatter, "preferred camera {id} is not an event angle")
            }
            Self::EventAlreadyInList { event, list } => {
                write!(formatter, "event {event} is already in list {list}")
            }
            Self::InvalidPlaybackRate(rate) => write!(
                formatter,
                "playback rate {rate} milli-x is outside -16000..=16000"
            ),
            Self::FrameDurationZero => formatter.write_str("frame duration must be nonzero"),
            Self::ExportStateTransition => {
                formatter.write_str("invalid export job state transition")
            }
            Self::LimitExceeded { limit, maximum } => {
                write!(
                    formatter,
                    "replay limit exceeded: at most {maximum} {limit}"
                )
            }
            Self::AngleNotInEvent { event, camera } => {
                write!(
                    formatter,
                    "camera {camera} is not an angle of event {event}"
                )
            }
            Self::CoverageGap { camera, gap } => write!(
                formatter,
                "camera {camera} has no recorded segment for {}..{} ns",
                gap.start.as_nanos(),
                gap.end.as_nanos()
            ),
            Self::NoFramesInRange(id) => {
                write!(
                    formatter,
                    "camera {id} has no frames in the requested range"
                )
            }
            Self::SegmentUnavailable { camera, at } => write!(
                formatter,
                "camera {camera} has no recorded segment at {} ns",
                at.as_nanos()
            ),
        }
    }
}

impl std::error::Error for ReplayError {}

pub(crate) fn check_name(name: &str) -> Result<(), ReplayError> {
    if name.trim().is_empty() {
        return Err(ReplayError::EmptyName);
    }
    check_bytes(name, ReplayLimit::NameBytes, MAX_NAME_BYTES)
}

pub(crate) const fn check_bytes(
    text: &str,
    limit: ReplayLimit,
    maximum: usize,
) -> Result<(), ReplayError> {
    if text.len() > maximum {
        return Err(ReplayError::LimitExceeded { limit, maximum });
    }
    Ok(())
}

pub(crate) const fn check_count(
    count: usize,
    limit: ReplayLimit,
    maximum: usize,
) -> Result<(), ReplayError> {
    if count > maximum {
        return Err(ReplayError::LimitExceeded { limit, maximum });
    }
    Ok(())
}
