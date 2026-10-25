//! Deterministic replay domain metadata.
//!
//! Media payloads and recorder durability remain owned by `fm-record`; this
//! crate catalogs rolling segments, synchronized source time, replay events,
//! playback intent, highlights, exports, and storage health.

mod capacity;
mod channel;
mod controller;
mod disk_health;
mod event;
mod highlight;
mod multiview;
mod music;
mod record_export;
mod reel;
mod resolve;
mod segment_writer;
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
pub use controller::{
    AngleSlot, BindingConflict, ControlInput, ControllerCommand, ControllerContext,
    ControllerError, ControllerProfile, ControllerSlot, ControllerSurface, JogStep,
    MAX_BINDINGS_PER_PROFILE, MAX_JOG_FRAMES, Modifiers, ReplayAction, binding_conflicts,
};
pub use disk_health::{
    CameraBitrate, DiskHealthError, DiskHealthLevel, DiskHealthMonitor, DiskHealthReport,
    DiskSample, DiskThresholds, MAX_CAMERA_BITS_PER_SECOND, MAX_OVERHEAD_PERMILLE, RecordingLoad,
};
pub use event::{EventDatabase, EventFolder, EventList, ReplayEvent, ReplayMarks};
pub use highlight::{
    CaptureState, ExportJob, ExportJobState, HighlightItem, HighlightTimeline, MusicBed,
    ReplaySession, Transition,
};
pub use multiview::{
    MAX_MULTIVIEW_ANGLES, MAX_MULTIVIEW_TILES, MultiviewError, MultiviewLayout, MultiviewPlan,
    MultiviewTile, QUAD_VIEW_ANGLES, TileMode, TileSource,
};
pub use music::{
    BedEntry, BedGain, BedRepeat, BedVoice, DuckingSpec, GainPoint, MAX_BED_ENTRIES,
    MAX_BED_SAMPLE_RATE, MAX_DUCK_INTERVALS, MAX_MUSIC_TRACKS, MIN_BED_SAMPLE_RATE, MusicBedError,
    MusicBedPlan, MusicBedSpec, MusicTrack,
};
pub use record_export::{
    ExportChunk, MAX_EXPORT_CHUNK_FRAMES, MAX_RECORD_EXPORT_JOBS, MAX_SEGMENTS_PER_EXPORT,
    RecordExportError, RecordExportFailure, RecordExportJob, RecordExportProgress,
    RecordExportQueue, RecordExportState,
};
pub use reel::{
    CompiledClip, CompiledReel, HighlightReel, MAX_RATIONAL_TERM, MAX_REEL_CLIPS, MAX_REEL_FPS,
    MAX_REEL_FRAMES, MAX_REEL_SPEED_RATIO, MAX_TRANSITION_FRAMES, MixWeight, ReelClipSpec,
    ReelError, ReelFrame, ReelFrameRate, ReelLayer, ReelLayerPlan, ReelSpeed, ReelTransition,
};
pub use resolve::{
    AngleSpan, CapturedEvent, ChannelFrame, FrameSpan, LastNPreset, SegmentCoverage,
};
pub use segment_writer::{
    DEFAULT_SEGMENT_UNIT_BYTES, ReplaySegmentWriter, SegmentFileEvent, SegmentWriterConfig,
    SegmentWriterError, segment_path,
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
