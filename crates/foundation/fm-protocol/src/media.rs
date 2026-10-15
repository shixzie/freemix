//! Media-file playback DTOs (Protocol 2.37, IN-003).
//!
//! A media input's replicated state is one [`MediaStatus`]: its persisted
//! [`MediaSettingsStatus`] and its runtime [`MediaTransportStatus`]. The same
//! status rides the `media_state` follow-on record of a snapshot, the
//! durable `media_changed` event, and the `runtime_event` with
//! `event=media`, so a client replaces the whole status of one input on
//! every record that names it.

use fm_types::FrameRate;

use crate::WireInputId;

/// Largest persisted source frame (trim points, start frame).
pub const MAX_MEDIA_FRAME: u64 = u32::MAX as u64;
/// Slowest speed (0.25x) in basis points.
pub const MIN_MEDIA_SPEED_BASIS_POINTS: u32 = 2_500;
/// Fastest speed (4x) in basis points.
pub const MAX_MEDIA_SPEED_BASIS_POINTS: u32 = 40_000;
/// Largest end-trigger lead in frames.
pub const MAX_MEDIA_END_TRIGGER_LEAD_FRAMES: u32 = 3_600;
/// Live-edge offset bounds in frames.
pub const MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES: u32 = 1;
pub const MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES: u32 = 600;
/// Largest idle-refresh completion count.
pub const MAX_MEDIA_IDLE_COMPLETION_REFRESHES: u32 = 100_000;
/// Most audio tracks one media status lists.
pub const MAX_MEDIA_AUDIO_TRACKS: usize = 32;
/// Longest audio-track language tag in bytes.
pub const MAX_MEDIA_TRACK_LANGUAGE_BYTES: usize = 32;
/// Longest audio-track title in bytes.
pub const MAX_MEDIA_TRACK_TITLE_BYTES: usize = 128;
/// Most media events one runtime record carries.
pub const MAX_MEDIA_EVENTS: usize = 16;
/// Most `media_state` records that follow one snapshot.
pub const MAX_MEDIA_STATES: u16 = 256;

/// What happens when the playhead crosses the final playable frame.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MediaEndAction {
    Stop,
    #[default]
    Hold,
    Loop,
}

impl MediaEndAction {
    pub const ALL: [Self; 3] = [Self::Stop, Self::Hold, Self::Loop];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Hold => "hold",
            Self::Loop => "loop",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// Audio behaviour at a speed other than real time.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MediaAudioPolicy {
    #[default]
    Mute,
    Resample,
}

impl MediaAudioPolicy {
    pub const ALL: [Self; 2] = [Self::Mute, Self::Resample];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Mute => "mute",
            Self::Resample => "resample",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// Transport state.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MediaTransportState {
    /// Cued, not advancing.
    #[default]
    Stopped,
    Playing,
    Paused,
    /// Playing, but starved at a growing file's live edge.
    Waiting,
    /// Holding the final frame after the end.
    Ended,
}

impl MediaTransportState {
    pub const ALL: [Self; 5] = [
        Self::Stopped,
        Self::Playing,
        Self::Paused,
        Self::Waiting,
        Self::Ended,
    ];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Waiting => "waiting",
            Self::Ended => "ended",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }

    /// Whether the playhead advances with the program clock.
    #[must_use]
    pub const fn is_advancing(self) -> bool {
        matches!(self, Self::Playing | Self::Waiting)
    }
}

/// Why a media input's source cannot play (or stopped playing).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaFault {
    /// The file is missing or unreadable.
    SourceMissing,
    /// The file shrank.
    SourceTruncated,
    /// A different file replaced the source (rotation or replacement).
    SourceReplaced,
    /// Previously read content changed in place.
    SourceRewritten,
    /// A finished file changed.
    SourceChanged,
    /// The file's container, codec, or timing is not supported (variable
    /// frame rate, unknown frame rate, invalid index, no video).
    Unsupported,
    /// A decode could not be verified or failed.
    DecodeFailed,
    /// The decoding tools are not available.
    ToolUnavailable,
}

impl MediaFault {
    pub const ALL: [Self; 8] = [
        Self::SourceMissing,
        Self::SourceTruncated,
        Self::SourceReplaced,
        Self::SourceRewritten,
        Self::SourceChanged,
        Self::Unsupported,
        Self::DecodeFailed,
        Self::ToolUnavailable,
    ];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::SourceMissing => "source_missing",
            Self::SourceTruncated => "source_truncated",
            Self::SourceReplaced => "source_replaced",
            Self::SourceRewritten => "source_rewritten",
            Self::SourceChanged => "source_changed",
            Self::Unsupported => "unsupported",
            Self::DecodeFailed => "decode_failed",
            Self::ToolUnavailable => "tool_unavailable",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.token() == token)
    }
}

/// How a file that is still being written is followed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MediaGrowingStatus {
    pub live_edge_offset_frames: u32,
    /// Zero waits for `media_complete_source`.
    pub idle_completion_refreshes: u32,
}

/// A media input's persisted playback settings.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MediaSettingsStatus {
    pub trim_in: u64,
    pub trim_out: Option<u64>,
    pub end_action: MediaEndAction,
    pub end_trigger_lead: Option<u32>,
    pub speed_basis_points: u32,
    pub audio_policy: MediaAudioPolicy,
    /// `None` selects the file's default track.
    pub audio_stream: Option<u32>,
    pub growing: Option<MediaGrowingStatus>,
    pub start_frame: u64,
}

impl Default for MediaSettingsStatus {
    fn default() -> Self {
        Self {
            trim_in: 0,
            trim_out: None,
            end_action: MediaEndAction::Hold,
            end_trigger_lead: None,
            speed_basis_points: 10_000,
            audio_policy: MediaAudioPolicy::Mute,
            audio_stream: None,
            growing: None,
            start_frame: 0,
        }
    }
}

/// One selectable audio stream of a media file.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MediaAudioTrack {
    /// The container stream index; the selection key.
    pub stream_index: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// A language tag such as `eng`, at most
    /// [`MAX_MEDIA_TRACK_LANGUAGE_BYTES`].
    pub language: Option<String>,
    /// At most [`MAX_MEDIA_TRACK_TITLE_BYTES`].
    pub title: Option<String>,
    pub default: bool,
}

/// An exact non-negative time in seconds, `numerator / denominator`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MediaTime {
    pub numerator: u128,
    /// Nonzero.
    pub denominator: u128,
}

impl MediaTime {
    pub const ZERO: Self = Self {
        numerator: 0,
        denominator: 1,
    };

    /// The same time in lowest terms (a zero denominator becomes
    /// [`Self::ZERO`]).
    #[must_use]
    pub const fn reduced(self) -> Self {
        if self.denominator == 0 || self.numerator == 0 {
            return Self::ZERO;
        }
        let (mut left, mut right) = (self.numerator, self.denominator);
        while right != 0 {
            let next = left % right;
            left = right;
            right = next;
        }
        Self {
            numerator: self.numerator / left,
            denominator: self.denominator / left,
        }
    }

    /// When source frame `frame` starts at `rate`, in lowest terms.
    #[must_use]
    pub const fn of_frame(frame: u64, rate: FrameRate) -> Self {
        Self {
            numerator: frame as u128 * rate.denominator() as u128,
            denominator: rate.numerator() as u128,
        }
        .reduced()
    }
}

/// A media input's runtime transport.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MediaTransportStatus {
    /// Whether the source is open: `false` while the file is unknown,
    /// missing, or faulted before it could be read.
    pub available: bool,
    pub state: MediaTransportState,
    /// The source frame under the playhead.
    pub position: u64,
    /// The playhead's exact time since source frame zero.
    pub position_time: MediaTime,
    /// The exclusive end of the currently playable range.
    pub playable_end: u64,
    /// Frames known to exist in the source.
    pub frames: u64,
    /// `false` while a growing file is still being written.
    pub complete: bool,
    pub loops: u64,
    /// The selected audio track's stream index.
    pub audio_track: Option<u32>,
    pub tracks: Vec<MediaAudioTrack>,
    /// The source's own frame rate, when available.
    pub source_rate: Option<FrameRate>,
    pub fault: Option<MediaFault>,
}

impl Default for MediaTime {
    fn default() -> Self {
        Self::ZERO
    }
}

/// The complete replicated state of one media input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaStatus {
    pub input: WireInputId,
    pub settings: MediaSettingsStatus,
    pub transport: MediaTransportStatus,
}

/// One `media_state` record after a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaStateMessage {
    /// The snapshot's revision.
    pub revision: u64,
    /// The engine's frame cursor when the record was written.
    pub frame: u64,
    pub status: MediaStatus,
}

/// Where a seek lands.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaSeekTarget {
    /// An exact source frame.
    Frame(u64),
    /// The frame displayed at `numerator / denominator` seconds.
    Time { numerator: u64, denominator: u64 },
}

/// One media command (Protocol 2.37).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaOperation {
    Play,
    Pause,
    Stop,
    Seek(MediaSeekTarget),
    /// Declares a growing file finished.
    CompleteSource,
    Trim {
        trim_in: u64,
        trim_out: Option<u64>,
    },
    Rate {
        speed_basis_points: u32,
    },
    EndAction(MediaEndAction),
    EndTriggerLead(Option<u32>),
    AudioTrack(Option<u32>),
    AudioPolicy(MediaAudioPolicy),
}

impl MediaOperation {
    /// The operation's token in `media_changed` (`operation=`); the command
    /// payload is `media_<token>`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Stop => "stop",
            Self::Seek(_) => "seek",
            Self::CompleteSource => "complete_source",
            Self::Trim { .. } => "trim",
            Self::Rate { .. } => "rate",
            Self::EndAction(_) => "end_action",
            Self::EndTriggerLead(_) => "end_trigger_lead",
            Self::AudioTrack(_) => "audio_track",
            Self::AudioPolicy(_) => "audio_policy",
        }
    }

    /// Transport operations are operator controls; the others edit the
    /// project's persisted settings.
    #[must_use]
    pub const fn is_transport(self) -> bool {
        matches!(
            self,
            Self::Play | Self::Pause | Self::Stop | Self::Seek(_) | Self::CompleteSource
        )
    }
}

/// Something that happened to a media input's transport.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaEvent {
    Seeked {
        frame: u64,
    },
    /// At most `remaining_frames` remain before the end.
    EndTrigger {
        remaining_frames: u64,
    },
    Ended {
        action: MediaEndAction,
    },
    Looped {
        count: u64,
    },
    LiveEdgeReached {
        frame: u64,
    },
    Resumed {
        frame: u64,
    },
    SourceGrew {
        frames: u64,
    },
    SourceCompleted {
        frames: u64,
    },
    AudioTrackChanged {
        from: Option<u32>,
        to: Option<u32>,
    },
    Faulted {
        fault: MediaFault,
    },
}
