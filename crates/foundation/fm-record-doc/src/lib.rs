//! Client-safe recorder document formats (AU-010): the per-recorder track
//! map, the track-file rollover limits document, and the track loudness
//! document carried into each WAV `bext` chunk.
//!
//! The daemon's recorder crate depends on these formats; Studio reads and
//! rewrites the same files in the supervised project bundle, so they live
//! in a foundation crate with no media dependencies.

pub mod track_limits;
pub mod track_loudness;
pub mod track_map;

pub use track_limits::{
    MAX_TRACK_SEGMENT_BYTES, MAX_TRACK_SEGMENT_SECONDS, TRACK_LIMITS_FILE_NAME,
    TRACK_LIMITS_HEADER, TrackLimits, TrackLimitsError,
};
pub use track_loudness::{
    TRACK_LOUDNESS_FILE_NAME, TRACK_LOUDNESS_HEADER, TrackLoudnessError, decode_track_loudness,
    encode_track_loudness,
};
pub use track_map::{
    BusInventory, MAX_BUS_CHANNELS, MAX_INVENTORY_BUSES, MAX_RECORDER_CHANNELS,
    MAX_RECORDER_TRACKS, MAX_ROUTE_FRAMES, MAX_TRACK_CHANNELS, MAX_TRACK_NAME_BYTES, RecordBus,
    RecordTrack, RecorderTrackMap, TrackLayout, TrackMapError, TrackMapLayout, TrackSource,
    split_track_file_name,
};

/// Broadcast Wave Format `bext` loudness fields (version 2), all in
/// hundredths: LUFS for integrated, momentary, and short-term loudness, LU
/// for range, and dBTP for the maximum true peak.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct BextLoudness {
    pub integrated_lufs: i16,
    pub range_lu: i16,
    pub max_true_peak_dbtp: i16,
    pub max_momentary_lufs: i16,
    pub max_short_term_lufs: i16,
}
