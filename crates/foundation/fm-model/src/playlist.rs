//! The project's persisted playlist: an ordered list of whole inputs, each
//! held on Program for a fixed number of frames and brought on air with a
//! typed transition, plus the automation state that drives it.
//!
//! Time is measured in engine frames. A playlist is driven by the engine's
//! frame cursor, so every scheduled boundary is an exact frame number rather
//! than a wall-clock instant. The state is persisted alongside the definition
//! so a restarted authority resumes the same airing at the same frame.

use fm_types::InputId;

/// Largest number of entries one project playlist may hold. Equals the
/// protocol's list bound so a complete playlist always fits one record.
pub const MAX_PLAYLIST_ENTRIES: usize = 256;
/// Longest entry duration in frames.
pub const MAX_PLAYLIST_ENTRY_FRAMES: u64 = 7 * 24 * 60 * 60 * 1000;
/// Longest playlist transition in frames; equals the engine's program
/// transition bound.
pub const MAX_PLAYLIST_TRANSITION_FRAMES: u32 = 3_600;

/// What happens when the on-air entry ends.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PlaylistLoop {
    /// Advance to the next enabled entry and finish after the last one.
    #[default]
    Off,
    /// Wrap from the last enabled entry to the first.
    All,
    /// Restart the on-air entry.
    One,
}

/// Program transitions a playlist entry may request. Stingers are excluded
/// because a playlist entry does not name a Stinger slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PlaylistTransitionKind {
    Cut,
    Fade,
    AlphaFade,
    Wipe,
    Slide,
    Zoom,
}

/// The transition that brings an entry on air. A cut has zero frames; every
/// other kind has between one and [`MAX_PLAYLIST_TRANSITION_FRAMES`] frames
/// and no more than the entry's duration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaylistTransition {
    pub kind: PlaylistTransitionKind,
    pub duration_frames: u32,
}

impl PlaylistTransition {
    pub const CUT: Self = Self {
        kind: PlaylistTransitionKind::Cut,
        duration_frames: 0,
    };
}

/// One playlist row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaylistEntry {
    /// Stable identity, unique within the playlist.
    pub id: u64,
    pub input: InputId,
    /// Frames the entry stays on Program, including its incoming transition.
    pub duration_frames: u64,
    pub transition: PlaylistTransition,
    pub enabled: bool,
}

/// The persisted automation state of a playlist.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaylistRunState {
    #[default]
    Stopped,
    /// Waiting for `start_frame`; starts from `from` or the first entry.
    Armed { start_frame: u64, from: Option<u64> },
    /// `started_frame` is the frame the airing would have started at had it
    /// never been paused.
    Playing {
        entry: u64,
        take: u64,
        cycle: u64,
        started_frame: u64,
    },
    Paused {
        entry: u64,
        take: u64,
        cycle: u64,
        elapsed_frames: u64,
    },
    /// Ran off the end; the last input stays on Program.
    Finished { last: Option<u64> },
}

/// The project's playlist definition and automation state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Playlist {
    pub loop_mode: PlaylistLoop,
    pub entries: Vec<PlaylistEntry>,
    pub state: PlaylistRunState,
    /// The take number the next airing receives; takes start at 1.
    pub next_take: u64,
    /// The last frame the playlist observed.
    pub observed_frame: u64,
}

impl Default for Playlist {
    fn default() -> Self {
        Self {
            loop_mode: PlaylistLoop::Off,
            entries: Vec::new(),
            state: PlaylistRunState::Stopped,
            next_take: 1,
            observed_frame: 0,
        }
    }
}

/// Why a playlist definition is structurally invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistEntryError {
    TooManyEntries { count: usize },
    DuplicateEntry(u64),
    InvalidDuration(u64),
    InvalidTransition(u64),
}

impl std::fmt::Display for PlaylistEntryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyEntries { count } => write!(
                formatter,
                "playlist has {count} entries; the limit is {MAX_PLAYLIST_ENTRIES}"
            ),
            Self::DuplicateEntry(id) => write!(formatter, "playlist entry {id} is duplicated"),
            Self::InvalidDuration(id) => write!(
                formatter,
                "playlist entry {id} must last between 1 and {MAX_PLAYLIST_ENTRY_FRAMES} frames"
            ),
            Self::InvalidTransition(id) => write!(
                formatter,
                "playlist entry {id} transition must be a zero-frame cut or last between 1 and \
                 {MAX_PLAYLIST_TRANSITION_FRAMES} frames within the entry"
            ),
        }
    }
}

impl std::error::Error for PlaylistEntryError {}

/// Checks the structural bounds every persisted or submitted playlist
/// definition must meet. Whether each input exists is checked against the
/// owning project or show.
///
/// # Errors
///
/// Returns the first [`PlaylistEntryError`] in entry order.
pub fn validate_playlist_entries(entries: &[PlaylistEntry]) -> Result<(), PlaylistEntryError> {
    if entries.len() > MAX_PLAYLIST_ENTRIES {
        return Err(PlaylistEntryError::TooManyEntries {
            count: entries.len(),
        });
    }
    for (index, entry) in entries.iter().enumerate() {
        if entries[..index].iter().any(|other| other.id == entry.id) {
            return Err(PlaylistEntryError::DuplicateEntry(entry.id));
        }
        if entry.duration_frames == 0 || entry.duration_frames > MAX_PLAYLIST_ENTRY_FRAMES {
            return Err(PlaylistEntryError::InvalidDuration(entry.id));
        }
        let frames = entry.transition.duration_frames;
        let valid = match entry.transition.kind {
            PlaylistTransitionKind::Cut => frames == 0,
            _ => {
                frames != 0
                    && frames <= MAX_PLAYLIST_TRANSITION_FRAMES
                    && u64::from(frames) <= entry.duration_frames
            }
        };
        if !valid {
            return Err(PlaylistEntryError::InvalidTransition(entry.id));
        }
    }
    Ok(())
}
