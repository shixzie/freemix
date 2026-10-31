//! Deterministic, frame-exact playback and playlist transport.
//!
//! This crate intentionally does not decode media. [`FixtureClip`] provides an
//! in-memory reference source, while [`FrameCodec`] is the boundary for a real
//! encoded-media adapter. [`MediaPlayback`] is the rational, backend-independent
//! transport for decoded media files (seek, trim, loop, rate, end actions,
//! audio-track selection, and growing-file live edges); `fm-codec-ffmpeg`
//! drives it over real files.

mod bank;
mod clip;
mod image_folder;
mod image_sequence;
mod list;
mod media;
mod playlist;
mod slideshow;
mod transport;
mod types;

pub use bank::{ListGoBank, ListGoBankError, MAX_GO_BANK_INPUTS};
pub use clip::{
    ClipError, ClipLibrary, CodecError, EncodedClip, FixtureClip, FrameCodec, LibraryError,
    PlaybackClip,
};
pub use image_folder::{
    FolderListingError, IMAGE_EXTENSIONS, ImageFolderListing, MAX_FOLDER_ENTRIES,
    MAX_FOLDER_NAME_BYTES, is_supported_image, natural_cmp,
};
pub use image_sequence::{
    CacheInsert, CacheRejection, GapPolicy, ImageSequence, ImageSequenceError, MAX_SEQUENCE_DIGITS,
    MAX_SEQUENCE_FRAMES, PrefetchBudget, PrefetchBudgetError, PrefetchCache, PrefetchStats,
    SequenceClock, SequenceClockError, SequenceEndMode, SequenceFile, SequenceName,
    SequencePadding, SequencePosition, parse_sequence_name, prefetch_order,
};
pub use list::{
    ActivationCause, ListAction, ListControlError, ListError, ListEvent, ListGo, ListGoError,
    ListItem, ListOptions, ListPlayer, ListTick, ListTickError, MAX_LIST_GO_RECORDS,
    MAX_LIST_ITEMS, MAX_PENDING_LIST_GO, MediaList, list_play_order,
};
pub use media::{
    AudioPlan, AudioSpeedPolicy, AudioTrack, MAX_PLAYBACK_RATE, MIN_PLAYBACK_RATE,
    MediaControlError, MediaEndAction, MediaEvent, MediaPlayback, MediaSourceConfig, MediaState,
    MediaTick, PlaybackRate, RationalSeconds, resample_linear,
};
pub use playlist::{
    CancelGoOutcome, EndAction, GoError, GoScheduleOutcome, GoStatus, MAX_PENDING_PLAYLIST_GO,
    MAX_PLAYLIST_GO_RECORDS, PlayerError, Playlist, PlaylistEntry, PlaylistError, PlaylistPlayer,
    ProgrammedGo,
};
pub use slideshow::{
    CropRect, DEFAULT_LARGE_SET_THRESHOLD, FULL_MIX_BASIS_POINTS, KenBurns, MAX_SLIDE_BROWSER_PAGE,
    MAX_SLIDE_DURATION_FRAMES, MAX_SLIDE_FILENAME_BYTES, MAX_SLIDE_SOURCE_DIMENSION,
    MAX_SLIDE_TRANSITION_FRAMES, MAX_SLIDES, PixelRect, RectEdge, Slide, SlideBrowserEntry,
    SlideId, SlideOrder, SlideTransition, SlideView, Slideshow, SlideshowConfig,
    SlideshowEditError, SlideshowError, SlideshowFrame,
};
pub use transport::{
    EndBehavior, MarkError, Marks, PlaybackError, PlaybackFrame, Transport, TransportState,
};
pub use types::{
    ClipId, FrameIndex, GoId, ListItemId, PlaylistEntryId, ScheduleCoordinate, Speed,
    SpeedDirection,
};

#[cfg(test)]
mod tests;
