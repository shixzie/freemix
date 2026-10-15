//! Rational, frame-accurate transport for decoded media files.
//!
//! [`MediaPlayback`] is the backend-independent half of file playback. It owns
//! the transport (play, pause, stop, seek), trim points, looping, end actions,
//! playback rate, audio-track selection, and the growing-file live edge. Each
//! [`MediaPlayback::tick`] advances exactly one *output* frame and returns a
//! [`MediaTick`] naming the source frame to present and the source audio span
//! to play. A decoder (for example the `FFmpeg` file player in
//! `fm-codec-ffmpeg`) turns those instructions into pixels and samples.
//!
//! # Time model
//!
//! The playhead is an exact rational source-frame position. Each output tick
//! advances it by `rate × source_rate / output_rate` source frames; the frame
//! presented on a tick is the floor of the playhead *before* the advance. This
//! single rule defines frame dropping and duplication:
//!
//! * a step above one frame skips source frames (reported as
//!   [`MediaTick::dropped_frames`]);
//! * a step below one frame presents the same source frame on consecutive
//!   ticks (reported as [`MediaTick::repeated`]).
//!
//! Changing the rate requantizes the sub-frame phase *down* onto the new step
//! grid, so a rate change never moves the playhead across a frame boundary.
//!
//! # Audio policy
//!
//! Audio sample `s` of a track sits at `s / sample_rate` seconds after source
//! frame zero. At the normal rate audio is contiguous: every tick continues
//! from the previous tick's last sample, re-synchronizing to the playhead only
//! after a discontinuity (seek, loop, track switch, rate change), so source and
//! output sample counts never drift. At any other rate the
//! [`AudioSpeedPolicy`] decides: [`AudioSpeedPolicy::Mute`] outputs silence,
//! and [`AudioSpeedPolicy::Resample`] maps exactly the source span covered by
//! the tick onto the tick's output samples with [`resample_linear`]
//! (varispeed: pitch follows speed; there is no time-stretching).
//!
//! # Growing files
//!
//! A source that is still being written reports `complete == false`. Its
//! playable end is `available_frames - live_edge_offset`; reaching it is not an
//! end of media: the transport enters [`MediaState::Waiting`], holds the last
//! playable frame, and resumes on the next tick after
//! [`MediaPlayback::update_source`] reports more frames. A source can only
//! grow; a shrinking frame count is refused as truncation.

use core::fmt;

use fm_types::FrameRate;

use crate::FrameIndex;

/// Smallest supported playback rate numerator/denominator pair (0.25x).
pub const MIN_PLAYBACK_RATE: (u32, u32) = (1, 4);
/// Largest supported playback rate numerator/denominator pair (4x).
pub const MAX_PLAYBACK_RATE: (u32, u32) = (4, 1);

/// A forward playback rate in `0.25..=4`, held as a reduced fraction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PlaybackRate {
    numerator: u32,
    denominator: u32,
}

impl PlaybackRate {
    /// Real-time playback.
    pub const NORMAL: Self = Self {
        numerator: 1,
        denominator: 1,
    };
    /// Largest accepted reduced denominator; keeps the phase grid bounded.
    pub const MAX_DENOMINATOR: u32 = 10_000;

    /// Creates a reduced rate in `0.25..=4`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaControlError::RateOutOfRange`] for a zero denominator,
    /// a rate outside `0.25..=4`, or a reduced denominator above
    /// [`Self::MAX_DENOMINATOR`].
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, MediaControlError> {
        let error = MediaControlError::RateOutOfRange {
            numerator,
            denominator,
        };
        if numerator == 0 || denominator == 0 {
            return Err(error);
        }
        let divisor = gcd(u128::from(numerator), u128::from(denominator));
        // Dividing a u32 by one of its divisors always fits a u32.
        let numerator_reduced = u32::try_from(u128::from(numerator) / divisor).unwrap_or(numerator);
        let denominator_reduced =
            u32::try_from(u128::from(denominator) / divisor).unwrap_or(denominator);
        let (min_n, min_d) = MIN_PLAYBACK_RATE;
        let (max_n, max_d) = MAX_PLAYBACK_RATE;
        let value = u64::from(numerator_reduced);
        let scale = u64::from(denominator_reduced);
        if value * u64::from(min_d) < u64::from(min_n) * scale
            || value * u64::from(max_d) > u64::from(max_n) * scale
            || denominator_reduced > Self::MAX_DENOMINATOR
        {
            return Err(error);
        }
        Ok(Self {
            numerator: numerator_reduced,
            denominator: denominator_reduced,
        })
    }

    /// Creates a rate from basis points, where `10_000` is real time.
    ///
    /// # Errors
    ///
    /// Returns [`MediaControlError::RateOutOfRange`] outside `2_500..=40_000`.
    pub fn from_basis_points(basis_points: u32) -> Result<Self, MediaControlError> {
        Self::new(basis_points, 10_000)
    }

    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    #[must_use]
    pub const fn is_normal(self) -> bool {
        self.numerator == self.denominator
    }
}

impl Default for PlaybackRate {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// An exact, reduced non-negative number of seconds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RationalSeconds {
    numerator: u128,
    denominator: u128,
}

impl RationalSeconds {
    /// Creates a reduced value; returns `None` for a zero denominator.
    #[must_use]
    pub fn new(numerator: u128, denominator: u128) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        let divisor = gcd(numerator, denominator).max(1);
        Some(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    #[must_use]
    pub const fn numerator(self) -> u128 {
        self.numerator
    }

    #[must_use]
    pub const fn denominator(self) -> u128 {
        self.denominator
    }
}

/// What happens when the playhead crosses the final playable frame.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MediaEndAction {
    /// Rewind to trim-in and stop; the trim-in frame is presented as a cue.
    Stop,
    /// Keep presenting the final frame in [`MediaState::Ended`].
    #[default]
    HoldLastFrame,
    /// Continue from trim-in (or frame zero without trim) seamlessly.
    Loop,
}

/// Audio behaviour while playing at a rate other than real time.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum AudioSpeedPolicy {
    /// Output silence at any non-normal rate.
    #[default]
    Mute,
    /// Linear varispeed resample of exactly the covered source span.
    Resample,
}

/// Transport state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaState {
    /// Cued at trim-in, not advancing.
    Stopped,
    Playing,
    Paused,
    /// Playing, but the playhead reached the live edge of a growing source.
    Waiting,
    /// The final frame was reached with [`MediaEndAction::HoldLastFrame`].
    Ended,
}

/// One selectable audio stream of the source.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AudioTrack {
    /// Absolute container stream index; the stable selection key.
    pub stream_index: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub language: Option<String>,
    pub title: Option<String>,
    pub default: bool,
}

/// Static description of a source when a transport is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaSourceConfig {
    pub source_rate: FrameRate,
    pub output_rate: FrameRate,
    /// Frames currently known to exist (decodable) in the source.
    pub available_frames: u64,
    /// `false` while the source is still being written.
    pub complete: bool,
    pub audio_tracks: Vec<AudioTrack>,
    /// Initially selected track (absolute stream index), if any.
    pub selected_audio: Option<u32>,
}

/// Something observable that happened since the previous tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaEvent {
    Seeked {
        frame: FrameIndex,
    },
    /// Fired once per pass when the playhead comes within the configured lead
    /// of the final playable frame.
    EndTrigger {
        remaining_frames: u64,
    },
    /// The final playable frame was crossed; `action` was applied.
    Ended {
        action: MediaEndAction,
    },
    Looped {
        count: u64,
    },
    /// A growing source starved playback at its live edge.
    LiveEdgeReached {
        frame: FrameIndex,
    },
    /// More data arrived and playback continued from the live edge.
    LiveEdgeResumed {
        frame: FrameIndex,
    },
    SourceGrew {
        available_frames: u64,
    },
    SourceCompleted {
        frames: u64,
    },
    AudioTrackChanged {
        from: Option<u32>,
        to: Option<u32>,
    },
}

/// How a decoder should fill a tick's audio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioPlan {
    /// No selected track: produce nothing.
    None,
    /// Output `output_samples` of silence on the selected track's format.
    Silence { track: u32, output_samples: u32 },
    /// Play source samples `start_sample..end_sample` of `track`.
    ///
    /// When `resample` is false the span is copied and any shortfall up to
    /// `output_samples` (a trimmed end or missing data) is padded with silence.
    /// When true the span is linearly resampled to exactly `output_samples`.
    Source {
        track: u32,
        start_sample: u64,
        end_sample: u64,
        output_samples: u32,
        resample: bool,
    },
}

/// One output frame's worth of transport decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaTick {
    /// Zero-based output tick number since the transport was created.
    pub output_tick: u64,
    /// Source frame to present; `None` while no frame is playable yet.
    pub frame: Option<FrameIndex>,
    /// The same source frame as the previous tick, without a discontinuity.
    pub repeated: bool,
    /// Source frames skipped since the previous tick by a fast rate.
    pub dropped_frames: u64,
    /// The decoder must reposition (seek, loop, first frame, trim change).
    pub discontinuity: bool,
    /// State after this tick's advance.
    pub state: MediaState,
    pub audio: AudioPlan,
    pub events: Vec<MediaEvent>,
}

/// Typed control failure. Failed operations leave the transport unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaControlError {
    RateOutOfRange {
        numerator: u32,
        denominator: u32,
    },
    InvalidTrim {
        in_frame: FrameIndex,
        out_frame: FrameIndex,
    },
    TrimBeyondSource {
        out_frame: FrameIndex,
        frames: u64,
    },
    SeekOutOfRange {
        requested: FrameIndex,
        start: FrameIndex,
        end: FrameIndex,
    },
    NothingPlayable,
    UnknownAudioTrack(u32),
    SourceTruncated {
        previous_frames: u64,
        frames: u64,
    },
    CompletedSourceChanged {
        frames: u64,
        reported: u64,
    },
    TimelineOverflow,
}

impl fmt::Display for MediaControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateOutOfRange {
                numerator,
                denominator,
            } => write!(
                formatter,
                "playback rate {numerator}/{denominator} is outside 0.25..=4"
            ),
            Self::InvalidTrim {
                in_frame,
                out_frame,
            } => write!(
                formatter,
                "trim-in {in_frame} is not before trim-out {out_frame}"
            ),
            Self::TrimBeyondSource { out_frame, frames } => write!(
                formatter,
                "trim-out {out_frame} is beyond a {frames}-frame source"
            ),
            Self::SeekOutOfRange {
                requested,
                start,
                end,
            } => write!(
                formatter,
                "seek {requested} is outside the playable range {start}..{end}"
            ),
            Self::NothingPlayable => formatter.write_str("no frame is playable yet"),
            Self::UnknownAudioTrack(index) => {
                write!(formatter, "audio stream {index} does not exist")
            }
            Self::SourceTruncated {
                previous_frames,
                frames,
            } => write!(
                formatter,
                "source shrank from {previous_frames} to {frames} frames"
            ),
            Self::CompletedSourceChanged { frames, reported } => write!(
                formatter,
                "completed {frames}-frame source reported {reported} frames"
            ),
            Self::TimelineOverflow => formatter.write_str("playback timeline overflowed"),
        }
    }
}

impl std::error::Error for MediaControlError {}

/// Backend-independent media-file transport. See the module documentation.
#[derive(Clone, Debug)]
pub struct MediaPlayback {
    source_rate: FrameRate,
    output_rate: FrameRate,
    available: u64,
    complete: bool,
    live_edge_offset: u64,
    trim_in: u64,
    trim_out: Option<u64>,
    end_action: MediaEndAction,
    end_trigger_lead: Option<u64>,
    rate: PlaybackRate,
    audio_policy: AudioSpeedPolicy,
    state: MediaState,
    /// Playhead = `units / denominator` source frames.
    units: u128,
    /// Step per output tick = `step_units / denominator` source frames.
    step_units: u128,
    denominator: u128,
    output_tick: u64,
    audio_cursor: Option<u64>,
    audio_tracks: Vec<AudioTrack>,
    selected_audio: Option<u32>,
    last_presented: Option<u64>,
    pending_discontinuity: bool,
    lead_fired: bool,
    loops: u64,
    pending_events: Vec<MediaEvent>,
}

impl MediaPlayback {
    /// Creates a stopped transport cued at frame zero.
    ///
    /// # Errors
    ///
    /// Returns [`MediaControlError::UnknownAudioTrack`] when the initial
    /// selection is not one of `audio_tracks`, or
    /// [`MediaControlError::TimelineOverflow`] for unrepresentable rates.
    pub fn new(config: MediaSourceConfig) -> Result<Self, MediaControlError> {
        if let Some(index) = config.selected_audio
            && !config
                .audio_tracks
                .iter()
                .any(|track| track.stream_index == index)
        {
            return Err(MediaControlError::UnknownAudioTrack(index));
        }
        let (step_units, denominator) =
            step_fraction(PlaybackRate::NORMAL, config.source_rate, config.output_rate)?;
        Ok(Self {
            source_rate: config.source_rate,
            output_rate: config.output_rate,
            available: config.available_frames,
            complete: config.complete,
            live_edge_offset: 0,
            trim_in: 0,
            trim_out: None,
            end_action: MediaEndAction::default(),
            end_trigger_lead: None,
            rate: PlaybackRate::NORMAL,
            audio_policy: AudioSpeedPolicy::default(),
            state: MediaState::Stopped,
            units: 0,
            step_units,
            denominator,
            output_tick: 0,
            audio_cursor: None,
            audio_tracks: config.audio_tracks,
            selected_audio: config.selected_audio,
            last_presented: None,
            pending_discontinuity: true,
            lead_fired: false,
            loops: 0,
            pending_events: Vec::new(),
        })
    }

    #[must_use]
    pub const fn state(&self) -> MediaState {
        self.state
    }

    #[must_use]
    pub const fn source_rate(&self) -> FrameRate {
        self.source_rate
    }

    #[must_use]
    pub const fn output_rate(&self) -> FrameRate {
        self.output_rate
    }

    #[must_use]
    pub const fn rate(&self) -> PlaybackRate {
        self.rate
    }

    #[must_use]
    pub const fn audio_policy(&self) -> AudioSpeedPolicy {
        self.audio_policy
    }

    #[must_use]
    pub const fn end_action(&self) -> MediaEndAction {
        self.end_action
    }

    #[must_use]
    pub const fn end_trigger_lead(&self) -> Option<u64> {
        self.end_trigger_lead
    }

    #[must_use]
    pub const fn live_edge_offset(&self) -> u64 {
        self.live_edge_offset
    }

    #[must_use]
    pub const fn available_frames(&self) -> u64 {
        self.available
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    #[must_use]
    pub const fn loop_count(&self) -> u64 {
        self.loops
    }

    /// Inclusive trim-in and exclusive trim-out (`None` = source end).
    #[must_use]
    pub const fn trim(&self) -> (FrameIndex, Option<FrameIndex>) {
        (
            FrameIndex::new(self.trim_in),
            match self.trim_out {
                Some(out) => Some(FrameIndex::new(out)),
                None => None,
            },
        )
    }

    #[must_use]
    pub fn audio_tracks(&self) -> &[AudioTrack] {
        &self.audio_tracks
    }

    #[must_use]
    pub const fn selected_audio(&self) -> Option<u32> {
        self.selected_audio
    }

    /// The source frame under the playhead.
    #[must_use]
    pub fn position(&self) -> FrameIndex {
        FrameIndex::new(self.frame_of(self.units))
    }

    /// Exact playhead time in seconds since source frame zero.
    #[must_use]
    pub fn position_time(&self) -> RationalSeconds {
        // units / denominator frames * (den / num) seconds per frame.
        let numerator = self
            .units
            .saturating_mul(u128::from(self.source_rate.denominator()));
        let denominator = self
            .denominator
            .saturating_mul(u128::from(self.source_rate.numerator()));
        RationalSeconds::new(numerator, denominator).unwrap_or(RationalSeconds {
            numerator: 0,
            denominator: 1,
        })
    }

    /// The exclusive end of the currently playable range.
    #[must_use]
    pub fn playable_end(&self) -> FrameIndex {
        FrameIndex::new(self.end_frame())
    }

    /// Exact source frames advanced per output tick at the current rate, as a
    /// `(numerator, denominator)` pair.
    #[must_use]
    pub const fn frames_per_tick(&self) -> (u128, u128) {
        (self.step_units, self.denominator)
    }

    /// Starts or resumes playback. From [`MediaState::Stopped`] or
    /// [`MediaState::Ended`] at the final frame, playback restarts at trim-in.
    pub fn play(&mut self) {
        match self.state {
            MediaState::Playing | MediaState::Waiting => {}
            MediaState::Paused => self.state = MediaState::Playing,
            MediaState::Stopped | MediaState::Ended => {
                if self.state == MediaState::Ended || self.frame_of(self.units) >= self.end_frame()
                {
                    self.reposition(self.trim_in);
                }
                self.state = MediaState::Playing;
            }
        }
    }

    /// Pauses on the current frame.
    pub fn pause(&mut self) {
        if matches!(self.state, MediaState::Playing | MediaState::Waiting) {
            self.state = MediaState::Paused;
        }
    }

    /// Stops and cues trim-in.
    pub fn stop(&mut self) {
        self.state = MediaState::Stopped;
        self.reposition(self.trim_in);
    }

    /// Seeks to an exact source frame inside the playable range.
    ///
    /// The transport state is kept, except that [`MediaState::Ended`] becomes
    /// [`MediaState::Paused`].
    ///
    /// # Errors
    ///
    /// Returns [`MediaControlError::SeekOutOfRange`] outside
    /// `trim_in..playable_end`.
    pub fn seek(&mut self, frame: FrameIndex) -> Result<(), MediaControlError> {
        let end = self.end_frame();
        if frame.get() < self.trim_in || frame.get() >= end {
            return Err(MediaControlError::SeekOutOfRange {
                requested: frame,
                start: FrameIndex::new(self.trim_in),
                end: FrameIndex::new(end),
            });
        }
        self.reposition(frame.get());
        match self.state {
            MediaState::Ended => self.state = MediaState::Paused,
            MediaState::Waiting => self.state = MediaState::Playing,
            MediaState::Stopped | MediaState::Playing | MediaState::Paused => {}
        }
        self.pending_events.push(MediaEvent::Seeked { frame });
        Ok(())
    }

    /// Seeks to the frame displayed at `time` (floor of `time × source_rate`).
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::seek`].
    pub fn seek_time(&mut self, time: RationalSeconds) -> Result<(), MediaControlError> {
        let frame = time
            .numerator
            .checked_mul(u128::from(self.source_rate.numerator()))
            .ok_or(MediaControlError::TimelineOverflow)?
            / time
                .denominator
                .checked_mul(u128::from(self.source_rate.denominator()))
                .ok_or(MediaControlError::TimelineOverflow)?;
        self.seek(FrameIndex::new(
            u64::try_from(frame).map_err(|_| MediaControlError::TimelineOverflow)?,
        ))
    }

    /// Replaces the trim range: inclusive `in_frame`, exclusive `out_frame`
    /// (`None` plays to the source end or live edge). Looping repeats the
    /// trimmed range. A playhead outside the new range moves to trim-in.
    ///
    /// # Errors
    ///
    /// Returns [`MediaControlError::InvalidTrim`] when `in_frame >= out_frame`,
    /// or [`MediaControlError::TrimBeyondSource`] when a complete source is
    /// shorter than either point.
    pub fn set_trim(
        &mut self,
        in_frame: FrameIndex,
        out_frame: Option<FrameIndex>,
    ) -> Result<(), MediaControlError> {
        if let Some(out) = out_frame {
            if in_frame >= out {
                return Err(MediaControlError::InvalidTrim {
                    in_frame,
                    out_frame: out,
                });
            }
            if self.complete && out.get() > self.available {
                return Err(MediaControlError::TrimBeyondSource {
                    out_frame: out,
                    frames: self.available,
                });
            }
        }
        if self.complete && in_frame.get() >= self.available {
            return Err(MediaControlError::TrimBeyondSource {
                out_frame: in_frame,
                frames: self.available,
            });
        }
        self.trim_in = in_frame.get();
        self.trim_out = out_frame.map(FrameIndex::get);
        let current = self.frame_of(self.units);
        if current < self.trim_in || self.trim_out.is_some_and(|out| current >= out) {
            self.reposition(self.trim_in);
        }
        self.lead_fired = false;
        Ok(())
    }

    /// Removes trim points.
    pub fn clear_trim(&mut self) {
        self.trim_in = 0;
        self.trim_out = None;
        self.lead_fired = false;
    }

    /// Changes the playback rate, requantizing the sub-frame phase downward.
    ///
    /// # Errors
    ///
    /// Returns [`MediaControlError::TimelineOverflow`] for unrepresentable
    /// combinations of rate and frame rates.
    pub fn set_rate(&mut self, rate: PlaybackRate) -> Result<(), MediaControlError> {
        let (step_units, denominator) = step_fraction(rate, self.source_rate, self.output_rate)?;
        let units = self
            .units
            .checked_mul(denominator)
            .ok_or(MediaControlError::TimelineOverflow)?
            / self.denominator;
        self.rate = rate;
        self.step_units = step_units;
        self.denominator = denominator;
        self.units = units;
        self.audio_cursor = None;
        Ok(())
    }

    pub const fn set_end_action(&mut self, action: MediaEndAction) {
        self.end_action = action;
    }

    /// Emits [`MediaEvent::EndTrigger`] once per pass when at most `lead`
    /// frames remain before the final playable frame is crossed.
    pub const fn set_end_trigger_lead(&mut self, lead: Option<u64>) {
        self.end_trigger_lead = lead;
        self.lead_fired = false;
    }

    pub const fn set_audio_policy(&mut self, policy: AudioSpeedPolicy) {
        self.audio_policy = policy;
    }

    /// Frames withheld behind the newest available frame of a growing source.
    pub const fn set_live_edge_offset(&mut self, frames: u64) {
        self.live_edge_offset = frames;
    }

    /// Selects an audio track by absolute stream index, or none. Switching
    /// mid-play continues at the playhead's exact sample position.
    ///
    /// # Errors
    ///
    /// Returns [`MediaControlError::UnknownAudioTrack`] for an unknown index.
    pub fn select_audio_track(
        &mut self,
        stream_index: Option<u32>,
    ) -> Result<(), MediaControlError> {
        if let Some(index) = stream_index
            && !self
                .audio_tracks
                .iter()
                .any(|track| track.stream_index == index)
        {
            return Err(MediaControlError::UnknownAudioTrack(index));
        }
        if self.selected_audio != stream_index {
            self.pending_events.push(MediaEvent::AudioTrackChanged {
                from: self.selected_audio,
                to: stream_index,
            });
            self.selected_audio = stream_index;
            self.audio_cursor = None;
        }
        Ok(())
    }

    /// Reports the current decodable frame count of the source.
    ///
    /// # Errors
    ///
    /// Returns [`MediaControlError::SourceTruncated`] if the count shrank, or
    /// [`MediaControlError::CompletedSourceChanged`] if a complete source
    /// reports a different count. The transport is unchanged on error.
    pub fn update_source(
        &mut self,
        available_frames: u64,
        complete: bool,
    ) -> Result<(), MediaControlError> {
        if available_frames < self.available {
            return Err(MediaControlError::SourceTruncated {
                previous_frames: self.available,
                frames: available_frames,
            });
        }
        if self.complete && available_frames != self.available {
            return Err(MediaControlError::CompletedSourceChanged {
                frames: self.available,
                reported: available_frames,
            });
        }
        if available_frames > self.available {
            self.pending_events
                .push(MediaEvent::SourceGrew { available_frames });
        }
        if complete && !self.complete {
            self.pending_events.push(MediaEvent::SourceCompleted {
                frames: available_frames,
            });
        }
        self.available = available_frames;
        self.complete = complete;
        Ok(())
    }

    /// Advances one output frame. See the module documentation.
    pub fn tick(&mut self) -> MediaTick {
        let mut events = core::mem::take(&mut self.pending_events);
        let output_tick = self.output_tick;
        self.output_tick = self.output_tick.saturating_add(1);
        let end = self.end_frame();
        let current = self.frame_of(self.units);

        if self.state == MediaState::Waiting && current < end {
            self.state = MediaState::Playing;
            events.push(MediaEvent::LiveEdgeResumed {
                frame: FrameIndex::new(current),
            });
        }

        let presented = if end > self.trim_in {
            Some(current.clamp(self.trim_in, end - 1))
        } else {
            None
        };
        let output_samples = self.output_samples(output_tick);
        let pending_discontinuity = core::mem::take(&mut self.pending_discontinuity);

        let audio = if self.state == MediaState::Playing && presented.is_some() {
            let audio = self.audio_plan(output_samples, end);
            self.advance(end, &mut events);
            audio
        } else {
            if self.state == MediaState::Playing {
                // Nothing playable yet on a growing source.
                self.state = MediaState::Waiting;
            }
            self.audio_cursor = None;
            self.silence(output_samples)
        };

        let discontinuity = pending_discontinuity
            || match (self.last_presented, presented) {
                (Some(previous), Some(frame)) => frame < previous,
                (None, Some(_)) => true,
                _ => false,
            };
        let repeated = !discontinuity && presented.is_some() && self.last_presented == presented;
        let dropped_frames = match (discontinuity, self.last_presented, presented) {
            (false, Some(previous), Some(frame)) if frame > previous + 1 => frame - previous - 1,
            _ => 0,
        };
        if presented.is_some() {
            self.last_presented = presented;
        } else if pending_discontinuity {
            self.pending_discontinuity = true;
        }
        MediaTick {
            output_tick,
            frame: presented.map(FrameIndex::new),
            repeated,
            dropped_frames,
            discontinuity,
            state: self.state,
            audio,
            events,
        }
    }

    fn advance(&mut self, end: u64, events: &mut Vec<MediaEvent>) {
        let end_is_final = self.end_is_final();
        self.units = self.units.saturating_add(self.step_units);
        let next = self.frame_of(self.units);
        if end_is_final
            && let Some(lead) = self.end_trigger_lead
            && !self.lead_fired
            && end.saturating_sub(next) <= lead
        {
            self.lead_fired = true;
            events.push(MediaEvent::EndTrigger {
                remaining_frames: end.saturating_sub(next),
            });
        }
        if next < end {
            return;
        }
        if !end_is_final {
            self.state = MediaState::Waiting;
            events.push(MediaEvent::LiveEdgeReached {
                frame: FrameIndex::new(end),
            });
            return;
        }
        events.push(MediaEvent::Ended {
            action: self.end_action,
        });
        self.audio_cursor = None;
        self.lead_fired = false;
        match self.end_action {
            MediaEndAction::Loop => {
                let start = u128::from(self.trim_in) * self.denominator;
                let length = u128::from(end - self.trim_in) * self.denominator;
                self.units = start + (self.units - start) % length;
                self.loops = self.loops.saturating_add(1);
                self.pending_discontinuity = true;
                events.push(MediaEvent::Looped { count: self.loops });
            }
            MediaEndAction::Stop => {
                self.state = MediaState::Stopped;
                self.reposition(self.trim_in);
            }
            MediaEndAction::HoldLastFrame => {
                self.state = MediaState::Ended;
                self.units = u128::from(end - 1) * self.denominator;
            }
        }
    }

    fn audio_plan(&mut self, output_samples: Option<(u32, u32)>, end: u64) -> AudioPlan {
        let Some((track, samples, sample_rate)) = self
            .selected_audio
            .zip(output_samples)
            .map(|(track, (samples, rate))| (track, samples, rate))
        else {
            return AudioPlan::None;
        };
        let limit = self.sample_at(u128::from(end) * self.denominator, sample_rate);
        if self.rate.is_normal() {
            let start = self
                .audio_cursor
                .unwrap_or_else(|| self.sample_at(self.units, sample_rate));
            let next = start.saturating_add(u64::from(samples));
            self.audio_cursor = Some(next);
            return AudioPlan::Source {
                track,
                start_sample: start,
                end_sample: next.min(limit.max(start)),
                output_samples: samples,
                resample: false,
            };
        }
        self.audio_cursor = None;
        match self.audio_policy {
            AudioSpeedPolicy::Mute => AudioPlan::Silence {
                track,
                output_samples: samples,
            },
            AudioSpeedPolicy::Resample => {
                let start = self.sample_at(self.units, sample_rate);
                let finish =
                    self.sample_at(self.units.saturating_add(self.step_units), sample_rate);
                AudioPlan::Source {
                    track,
                    start_sample: start,
                    end_sample: finish.min(limit).max(start),
                    output_samples: samples,
                    resample: true,
                }
            }
        }
    }

    fn silence(&self, output_samples: Option<(u32, u32)>) -> AudioPlan {
        match self.selected_audio.zip(output_samples) {
            Some((track, (samples, _))) => AudioPlan::Silence {
                track,
                output_samples: samples,
            },
            None => AudioPlan::None,
        }
    }

    /// Output samples of tick `tick` at the selected track's sample rate, and
    /// that rate.
    fn output_samples(&self, tick: u64) -> Option<(u32, u32)> {
        let sample_rate = self
            .audio_tracks
            .iter()
            .find(|track| Some(track.stream_index) == self.selected_audio)?
            .sample_rate;
        let boundary = |tick: u64| {
            u128::from(tick) * u128::from(sample_rate) * u128::from(self.output_rate.denominator())
                / u128::from(self.output_rate.numerator())
        };
        let samples = boundary(tick + 1) - boundary(tick);
        Some((u32::try_from(samples).unwrap_or(u32::MAX), sample_rate))
    }

    /// First sample at or after source position `units / denominator`.
    fn sample_at(&self, units: u128, sample_rate: u32) -> u64 {
        let numerator = units
            .saturating_mul(u128::from(sample_rate))
            .saturating_mul(u128::from(self.source_rate.denominator()));
        let denominator = self.denominator * u128::from(self.source_rate.numerator());
        u64::try_from(numerator / denominator).unwrap_or(u64::MAX)
    }

    fn frame_of(&self, units: u128) -> u64 {
        u64::try_from(units / self.denominator).unwrap_or(u64::MAX)
    }

    fn reposition(&mut self, frame: u64) {
        self.units = u128::from(frame) * self.denominator;
        self.pending_discontinuity = true;
        self.audio_cursor = None;
        self.lead_fired = false;
    }

    fn live_edge(&self) -> u64 {
        if self.complete {
            self.available
        } else {
            self.available.saturating_sub(self.live_edge_offset)
        }
    }

    fn end_frame(&self) -> u64 {
        let edge = self.live_edge();
        self.trim_out.map_or(edge, |out| out.min(edge))
    }

    fn end_is_final(&self) -> bool {
        self.complete || self.trim_out.is_some_and(|out| out <= self.live_edge())
    }
}

/// Linearly resamples one channel plane to exactly `output_len` samples.
///
/// Output sample `i` reads input position `i × input_len / output_len`,
/// interpolating between the two neighbouring input samples (the last input
/// sample is held at the right edge). An empty input yields silence.
#[must_use]
pub fn resample_linear(input: &[f32], output_len: usize) -> Vec<f32> {
    if input.is_empty() {
        return vec![0.0; output_len];
    }
    if input.len() == output_len {
        return input.to_vec();
    }
    let input_len = input.len() as u128;
    let output = output_len.max(1) as u128;
    (0..output_len)
        .map(|index| {
            let scaled = index as u128 * input_len;
            let whole = usize::try_from(scaled / output).unwrap_or(usize::MAX);
            let remainder = scaled % output;
            let left = input[whole.min(input.len() - 1)];
            let right = input[(whole + 1).min(input.len() - 1)];
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let fraction = (remainder as f64 / output as f64) as f32;
            (right - left).mul_add(fraction, left)
        })
        .collect()
}

fn step_fraction(
    rate: PlaybackRate,
    source: FrameRate,
    output: FrameRate,
) -> Result<(u128, u128), MediaControlError> {
    let numerator = u128::from(rate.numerator())
        * u128::from(source.numerator())
        * u128::from(output.denominator());
    let denominator = u128::from(rate.denominator())
        * u128::from(source.denominator())
        * u128::from(output.numerator());
    let divisor = gcd(numerator, denominator);
    if divisor == 0 {
        return Err(MediaControlError::TimelineOverflow);
    }
    Ok((numerator / divisor, denominator / divisor))
}

const fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}
