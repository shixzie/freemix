//! Deterministic recorder coordination with crash-repairable segment files.
//!
//! The on-disk format is intentionally private. Segment and manifest records
//! are append-only, length-delimited, and protected by CRC32 checksums. Recovery
//! truncates an incomplete final record, but never hides checksum corruption.

mod coordinator;
mod format;
mod iso;
mod track_map;
mod track_wav;
mod types;
mod wav;

pub use coordinator::{
    DurableWriter, ReconciliationFailure, ReconciliationReport, RecorderCoordinator,
    StdWriterFactory, WriterFactory, repair_recording,
};
pub use iso::{
    ISO_MANIFEST_NAME, IsoManifestEntry, IsoManifestError, IsoTapConfig, IsoTapError,
    IsoTapSession, IsoTapSnapshot, MAX_ISO_TAPS, parse_iso_manifest, render_iso_manifest_entries,
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
