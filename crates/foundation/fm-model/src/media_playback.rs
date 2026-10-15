//! Persisted playback settings of a media-file input (schema 36, IN-003).
//!
//! Every [`crate::InputKind::Media`] input carries one
//! [`MediaPlaybackSettings`]: the trim range, what happens at the end, the
//! end-trigger lead, the playback speed, the audio policy at non-normal
//! speeds, the selected audio stream, whether the file is still being
//! written (a growing file), and the frame a restored show cues. The
//! transport itself (playing, paused, position) is runtime state and is not
//! persisted.
//!
//! All positions are *source* frames of the file, counted from its first
//! video frame. Trim-in is inclusive and trim-out exclusive.

use core::fmt;

/// Real-time speed in basis points.
pub const MEDIA_SPEED_NORMAL_BASIS_POINTS: u32 = 10_000;
/// Slowest playback speed (0.25x) in basis points.
pub const MIN_MEDIA_SPEED_BASIS_POINTS: u32 = 2_500;
/// Fastest playback speed (4x) in basis points.
pub const MAX_MEDIA_SPEED_BASIS_POINTS: u32 = 40_000;
/// Largest persisted source frame (trim points and start frame).
pub const MAX_MEDIA_FRAME: u64 = u32::MAX as u64;
/// Largest end-trigger lead in frames.
pub const MAX_MEDIA_END_TRIGGER_LEAD_FRAMES: u32 = 3_600;
/// Smallest live-edge offset: the newest frame of a growing file may still be
/// partially written, so at least one is always withheld.
pub const MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES: u32 = 1;
/// Largest live-edge offset in frames.
pub const MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES: u32 = 600;
/// Default live-edge offset in frames.
pub const DEFAULT_MEDIA_LIVE_EDGE_OFFSET_FRAMES: u32 = 2;
/// Largest idle-refresh count before a growing file completes on its own.
pub const MAX_MEDIA_IDLE_COMPLETION_REFRESHES: u32 = 100_000;

/// What happens when the playhead crosses the final playable frame.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MediaEndAction {
    /// Rewind to trim-in and stop.
    Stop,
    /// Keep showing the final frame.
    #[default]
    Hold,
    /// Continue from trim-in seamlessly.
    Loop,
}

impl MediaEndAction {
    pub const ALL: [Self; 3] = [Self::Stop, Self::Hold, Self::Loop];

    /// The persisted and wire token.
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
        Self::ALL.into_iter().find(|action| action.token() == token)
    }
}

/// Audio behaviour while playing at a speed other than real time.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MediaAudioSpeedPolicy {
    /// Silence at any non-normal speed.
    #[default]
    Mute,
    /// Linear varispeed: pitch follows speed.
    Resample,
}

impl MediaAudioSpeedPolicy {
    pub const ALL: [Self; 2] = [Self::Mute, Self::Resample];

    /// The persisted and wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Mute => "mute",
            Self::Resample => "resample",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|policy| policy.token() == token)
    }
}

/// How a file that is still being written is followed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MediaGrowing {
    /// Newest frames withheld from playback, in
    /// [`MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES`]`..=`[`MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES`].
    pub live_edge_offset_frames: u32,
    /// The file is treated as finished after this many consecutive refreshes
    /// observe no growth; zero waits for an explicit completion.
    pub idle_completion_refreshes: u32,
}

impl Default for MediaGrowing {
    fn default() -> Self {
        Self {
            live_edge_offset_frames: DEFAULT_MEDIA_LIVE_EDGE_OFFSET_FRAMES,
            idle_completion_refreshes: 0,
        }
    }
}

/// The persisted playback settings of one media-file input.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MediaPlaybackSettings {
    /// Inclusive first playable source frame.
    pub trim_in: u64,
    /// Exclusive end of the playable range; `None` plays to the file's end
    /// (or a growing file's live edge).
    pub trim_out: Option<u64>,
    pub end_action: MediaEndAction,
    /// Fire an end trigger this many frames before the final playable frame
    /// is crossed; `None` fires none.
    pub end_trigger_lead: Option<u32>,
    /// Playback speed in basis points, `2_500..=40_000` (0.25x to 4x).
    pub speed_basis_points: u32,
    pub audio_policy: MediaAudioSpeedPolicy,
    /// The container stream index of the selected audio track; `None`
    /// selects the file's default track.
    pub audio_stream: Option<u32>,
    /// `Some` while the file is still being written.
    pub growing: Option<MediaGrowing>,
    /// The source frame a restored show cues, inside the trim range.
    pub start_frame: u64,
}

impl Default for MediaPlaybackSettings {
    fn default() -> Self {
        Self {
            trim_in: 0,
            trim_out: None,
            end_action: MediaEndAction::default(),
            end_trigger_lead: None,
            speed_basis_points: MEDIA_SPEED_NORMAL_BASIS_POINTS,
            audio_policy: MediaAudioSpeedPolicy::default(),
            audio_stream: None,
            growing: None,
            start_frame: 0,
        }
    }
}

/// A playback setting outside its bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaPlaybackSettingsError {
    /// Trim-in is not before trim-out, or a trim point is beyond
    /// [`MAX_MEDIA_FRAME`].
    InvalidTrim,
    /// The start frame is outside the trim range or beyond [`MAX_MEDIA_FRAME`].
    StartOutsideTrim,
    SpeedOutOfRange,
    EndTriggerLeadOutOfRange,
    LiveEdgeOffsetOutOfRange,
    IdleCompletionOutOfRange,
}

impl MediaPlaybackSettingsError {
    /// The settings field the error names.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::InvalidTrim => "trim",
            Self::StartOutsideTrim => "start_frame",
            Self::SpeedOutOfRange => "speed_basis_points",
            Self::EndTriggerLeadOutOfRange => "end_trigger_lead",
            Self::LiveEdgeOffsetOutOfRange => "live_edge_offset_frames",
            Self::IdleCompletionOutOfRange => "idle_completion_refreshes",
        }
    }
}

impl fmt::Display for MediaPlaybackSettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidTrim => "media trim-in must be before trim-out and within the frame bound",
            Self::StartOutsideTrim => "media start frame must be inside the trim range",
            Self::SpeedOutOfRange => "media speed must be 2500..=40000 basis points",
            Self::EndTriggerLeadOutOfRange => "media end-trigger lead must be 0..=3600 frames",
            Self::LiveEdgeOffsetOutOfRange => "media live-edge offset must be 1..=600 frames",
            Self::IdleCompletionOutOfRange => "media idle completion count must be 0..=100000",
        })
    }
}

impl std::error::Error for MediaPlaybackSettingsError {}

impl MediaPlaybackSettings {
    /// Checks every bound.
    ///
    /// # Errors
    ///
    /// Returns the first violated bound.
    pub fn validate(&self) -> Result<(), MediaPlaybackSettingsError> {
        validate_trim(self.trim_in, self.trim_out)?;
        if self.start_frame > MAX_MEDIA_FRAME
            || self.start_frame < self.trim_in
            || self.trim_out.is_some_and(|out| self.start_frame >= out)
        {
            return Err(MediaPlaybackSettingsError::StartOutsideTrim);
        }
        if !(MIN_MEDIA_SPEED_BASIS_POINTS..=MAX_MEDIA_SPEED_BASIS_POINTS)
            .contains(&self.speed_basis_points)
        {
            return Err(MediaPlaybackSettingsError::SpeedOutOfRange);
        }
        if self
            .end_trigger_lead
            .is_some_and(|lead| lead > MAX_MEDIA_END_TRIGGER_LEAD_FRAMES)
        {
            return Err(MediaPlaybackSettingsError::EndTriggerLeadOutOfRange);
        }
        if let Some(growing) = self.growing {
            if !(MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES..=MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES)
                .contains(&growing.live_edge_offset_frames)
            {
                return Err(MediaPlaybackSettingsError::LiveEdgeOffsetOutOfRange);
            }
            if growing.idle_completion_refreshes > MAX_MEDIA_IDLE_COMPLETION_REFRESHES {
                return Err(MediaPlaybackSettingsError::IdleCompletionOutOfRange);
            }
        }
        Ok(())
    }

    /// The settings with a new trim range. The start frame moves into the
    /// range when the new range excludes it.
    ///
    /// # Errors
    ///
    /// Returns [`MediaPlaybackSettingsError::InvalidTrim`].
    pub fn with_trim(
        mut self,
        trim_in: u64,
        trim_out: Option<u64>,
    ) -> Result<Self, MediaPlaybackSettingsError> {
        validate_trim(trim_in, trim_out)?;
        self.trim_in = trim_in;
        self.trim_out = trim_out;
        if self.start_frame < trim_in || trim_out.is_some_and(|out| self.start_frame >= out) {
            self.start_frame = trim_in;
        }
        Ok(self)
    }
}

fn validate_trim(trim_in: u64, trim_out: Option<u64>) -> Result<(), MediaPlaybackSettingsError> {
    if trim_in > MAX_MEDIA_FRAME
        || trim_out.is_some_and(|out| out > MAX_MEDIA_FRAME || out <= trim_in)
    {
        return Err(MediaPlaybackSettingsError::InvalidTrim);
    }
    Ok(())
}

/// Why [`crate::Project::set_media_playback`] refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetMediaPlaybackError {
    UnknownInput(fm_types::InputId),
    NotMediaInput(fm_types::InputId),
    Invalid(MediaPlaybackSettingsError),
}

impl fmt::Display for SetMediaPlaybackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::NotMediaInput(input) => write!(formatter, "input {input} is not a media input"),
            Self::Invalid(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SetMediaPlaybackError {}
