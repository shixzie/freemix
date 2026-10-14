//! Deterministic recorder coordination with crash-repairable segment files.
//!
//! The on-disk format is intentionally private. Segment and manifest records
//! are append-only, length-delimited, and protected by CRC32 checksums. Recovery
//! truncates an incomplete final record, but never hides checksum corruption.
//!
//! Container-level fault tolerance for fragmented MP4 program recordings lives
//! in [`scan_fmp4_file`]/[`repair_fmp4_file`] (torn-tail validation and
//! truncation), [`RecordingSession`]/[`recover_session`] (atomic multi-root
//! session manifest, failure policy, and crash recovery),
//! [`ResilientFmp4Writer`] (fragment-granular failover writer), and
//! [`estimate_recording_space`]/[`SpaceMonitor`] (remaining-time estimates).

mod coordinator;
mod fmp4;
mod format;
mod iso;
mod resilient;
mod session;
mod space;
mod track_map;
mod track_wav;
mod types;
mod wav;

pub use coordinator::{
    DurableWriter, ReconciliationFailure, ReconciliationReport, RecorderCoordinator,
    StdWriterFactory, WriterFactory, repair_recording,
};
pub use fmp4::{
    Fmp4Error, Fmp4Fragment, Fmp4Integrity, Fmp4Movie, Fmp4Repair, Fmp4Scan, Fmp4Splitter,
    Fmp4Track, Fmp4TrackRun, Fmp4TrackSpan, Fmp4Unit, MAX_FMP4_INIT_BYTES, MAX_FMP4_MOOF_BYTES,
    MAX_FMP4_TRACKS, SplitError, repair_fmp4_file, scan_fmp4, scan_fmp4_file,
};
pub use iso::{
    ISO_MANIFEST_NAME, IsoManifestEntry, IsoManifestError, IsoTapConfig, IsoTapError,
    IsoTapSession, IsoTapSnapshot, MAX_ISO_TAPS, parse_iso_manifest, render_iso_manifest_entries,
};
pub use resilient::{ResilientFmp4Writer, ResilientWriterConfig, WriterError, WriterStats};
pub use session::{
    FailureDecision, FailureKind, FailureOp, FileStorage, MAX_SESSION_ERRORS,
    MAX_SESSION_NAME_BYTES, MAX_SESSION_ROOTS, MAX_SESSION_SEGMENTS, ManifestRead, OpenSegment,
    RecordingSession, RecoveredSessionSegment, SESSION_MANIFEST_EXTENSION,
    SESSION_SEGMENT_EXTENSION, SegmentEntry, SegmentOutcome, SegmentSink, SegmentState,
    SegmentStorage, SessionConfig, SessionError, SessionErrorRecord, SessionManifest,
    SessionRecovery, SessionSnapshot, SessionState, SpaceProbe, UnknownSpace, recover_session,
};
pub use space::{
    RootPreflight, SpaceEstimate, SpaceLevel, SpaceMonitor, SpacePreflight, SpaceThresholds,
    SpaceTransition, estimate_recording_space, observed_bitrate, preflight_recording_space,
};
pub use track_map::{
    BusInventory, MAX_BUS_CHANNELS, MAX_INVENTORY_BUSES, MAX_RECORDER_CHANNELS,
    MAX_RECORDER_TRACKS, MAX_ROUTE_FRAMES, MAX_TRACK_CHANNELS, MAX_TRACK_NAME_BYTES, RecordBus,
    RecordTrack, RecorderTrackMap, TrackLayout, TrackMapError, TrackSource, split_track_file_name,
};
pub use track_wav::{TrackFileMode, TrackWavError, TrackWavOptions, TrackWavRecorder};
pub use types::{
    ActionOutcome, ActionReceiptId, AppendError, AudioPacketMetadata, Discontinuity, EnqueueError,
    IsoTapMetadata, PacketCommon, QueueLimits, RecordEvent, RecorderConfig, RecorderError,
    RecorderId, RecorderSnapshot, RecorderState, RecoveredSegment, RecoveryReport, SegmentPolicy,
    TimecodeSource, TimedPacketMetadata, VideoPacketMetadata,
};
pub use wav::{
    BEXT_FIXED_BYTES, BextField, BextMetadata, DS64_PAYLOAD_BYTES, FinalizedWav, LargeFilePolicy,
    MAX_CODING_HISTORY_BYTES, MAX_RIFF_SIZE, MAX_WAV_BUFFER_BYTES, MAX_WAV_CHANNELS, WavError,
    WavInfo, WavRecovery, WavRecoveryTarget, WavSampleFormat, WavSink, WavSource, WavSpec,
    WavSummary, WavWriter, encode_sample, encode_wav_header, read_wav_info, recover_wav,
    recover_wav_file,
};

#[cfg(test)]
mod tests;
