//! Deterministic replay domain metadata.
//!
//! Media payloads and recorder durability remain owned by `fm-record`; this
//! crate catalogs rolling segments, synchronized source time, replay events,
//! playback intent, highlights, exports, and storage health.

mod capacity;
mod channel;
mod event;
mod highlight;
mod resolve;
mod source;
mod storage;
mod types;

pub use capacity::{
    CapacityPrediction, CapacitySample, LowSpaceAction, LowSpaceDecision, LowSpacePolicy,
    ReplayRecoveryReport, inspect_recovery,
};
pub use channel::{
    AudioPolicy, ChannelMode, ChannelTransport, PlaybackRate, ReplayAudioSource, ReplayChannel,
    ReplayChannelId, ReplayDecks, VariableSpeedAudio,
};
pub use event::{EventDatabase, EventFolder, EventList, ReplayEvent, ReplayMarks};
pub use highlight::{
    CaptureState, ExportJob, ExportJobState, HighlightItem, HighlightTimeline, MusicBed,
    ReplaySession, Transition,
};
pub use resolve::{
    AngleSpan, CapturedEvent, ChannelFrame, FrameSpan, LastNPreset, SegmentCoverage,
};
pub use source::{
    CameraSource, MAX_AUDIO_CHANNELS, MAX_REPLAY_SOURCES, QuadCell, QuadViewDescription,
    SourceCatalog, SourceObservation, SourceTimeline, TimelineDiscontinuity,
};
pub use storage::{
    ProtectionReference, RetentionReport, RollingSegmentCatalog, SegmentMetadata, SegmentState,
    StorageRoot,
};
pub use types::{
    CameraId, EventId, ExportJobId, FolderId, HighlightId, ListId, MAX_EVENTS, MAX_EVENTS_PER_LIST,
    MAX_FOLDERS, MAX_LISTS, MAX_NAME_BYTES, MAX_NOTE_BYTES, MAX_OBSERVATIONS_PER_CAMERA,
    MAX_TAGS_PER_EVENT, MusicTrackId, ReplayError, ReplayLimit, SegmentId, StorageRootId,
    TimelineRange,
};

#[cfg(test)]
mod tests;
