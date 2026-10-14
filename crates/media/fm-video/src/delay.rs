//! Deterministic per-input video delay line (IN-022 reference slice).
//!
//! The line is keyed by media timestamps, never by wall clock. Every call
//! carries the caller's current output time `now` on the same normalized
//! nanosecond timeline as frame presentation timestamps (for example
//! `fm_frame::NormalizedTimestamp` values after input clock mapping).
//!
//! # Timeline
//!
//! Each pushed frame receives a *line timestamp* `line_pts = pts + rebase`.
//! The rebase is zero while source timestamps stay within
//! [`VideoDelayConfig::max_skew`] of `now`; it is re-anchored to `now` at
//! every discontinuity (see below), so frame spacing inside one segment is
//! taken exactly from the source timestamps.
//!
//! # Frame selection
//!
//! [`VideoDelayLine::output`] computes `target = now - effective_delay` and
//! presents the frame that covers the target: the newest buffered frame whose
//! start is not later than `target + slack`, where the slack is
//! [`SELECTION_SLACK_NANOS`] (capped at half the frame duration). This is
//! sample-and-hold semantics, so a delay that is not a whole number of frames
//! is effectively rounded up to the next frame start. The slack absorbs the
//! one-nanosecond rounding of integer-nanosecond frame starts and of a delay
//! expressed in frames at fractional rates such as 30000/1001 and 60000/1001,
//! which makes whole-frame delays exact; it is far smaller than any frame, so
//! output ticks at mid-frame (for example 59.94 output of a 29.97 source) are
//! never ambiguous.
//!
//! Output never moves backwards in source time: buffered frames older than
//! the presented frame are discarded, so a frame that has been passed over
//! can never be selected again.
//!
//! # Start-up policy
//!
//! Until the first frame becomes due, the line is [`DelayLineState::Filling`]
//! and each output returns [`DelayOutput::Nothing`] or [`DelayOutput::Black`]
//! according to [`StartupPolicy`]. The same applies after a reset or
//! discontinuity. If capacity eviction happens while filling (the configured
//! delay cannot be held in memory), the line starts presenting its oldest
//! frame immediately instead of staying black forever.
//!
//! # Live delay changes
//!
//! With [`DelayChangePolicy::Immediate`], an increase by `D` repeats the
//! current frame for `D` (a bounded freeze) and a decrease by `D` skips
//! forward by `D` in one output. With [`DelayChangePolicy::Slew`] the
//! effective delay moves toward the requested delay by at most
//! `elapsed_now * per_mille / 1000` per output, so an increase plays at
//! `1 - per_mille/1000` speed (repeating frames) and a decrease at
//! `1 + per_mille/1000` speed (skipping frames). While filling, changes apply
//! immediately. The requested delay is always bounded by
//! [`VideoDelayConfig::max_delay`].
//!
//! # Discontinuities
//!
//! A pushed frame whose source timestamp is equal to or earlier than the
//! newest buffered frame's, but by no more than `max_skew`, is a late or
//! duplicate frame: it is dropped and counted as `late_drops`, and the buffer,
//! state, and rebase are untouched. A single stray frame therefore never
//! blanks the output for the whole delay.
//!
//! A pushed frame is a discontinuity when it carries the discontinuity flag,
//! its source timestamp jumps backwards by more than `max_skew` from the
//! newest buffered frame, or its line timestamp is more than `max_skew` away
//! from `now` (which catches forward jumps). A discontinuity clears the buffer
//! (counting unpresented frames as `discontinuity_drops`), returns the line to
//! filling, and re-anchors the rebase so the new frame's line timestamp equals
//! `now`.
//!
//! # Memory bounds
//!
//! The buffer never holds more than [`VideoDelayConfig::max_frames`] frames
//! or [`VideoDelayConfig::max_bytes`] payload bytes (as reported by the
//! caller for each frame). When a push would exceed either bound the oldest
//! frames are evicted first. A presented frame evicted this way leaves the
//! line presenting its oldest remaining frame early
//! ([`DelayedFrameKind::Early`]), which caps the achievable delay at what fits;
//! [`VideoDelayConfig::capacity_for`] estimates that cap.

use core::fmt;
use std::collections::VecDeque;

use fm_types::FrameRate;

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Hard upper bound for [`VideoDelayConfig::max_delay`]: two minutes.
pub const MAX_VIDEO_DELAY_NANOS: u64 = 120 * 1_000_000_000;
/// Hard upper bound for [`VideoDelayConfig::max_frames`].
pub const MAX_VIDEO_DELAY_FRAMES: usize = 1 << 16;
/// Timestamp rounding slack used by frame selection (one microsecond).
pub const SELECTION_SLACK_NANOS: u64 = 1_000;
/// Default [`VideoDelayConfig::max_skew`]: one second.
pub const DEFAULT_VIDEO_DELAY_MAX_SKEW_NANOS: u64 = 1_000_000_000;

/// A non-negative delay or duration on the normalized nanosecond timeline.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DelayDuration(u64);

impl DelayDuration {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Creates a delay from whole milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::ArithmeticOverflow`] if the nanosecond value
    /// is not representable.
    pub const fn from_millis(millis: u64) -> Result<Self, VideoDelayError> {
        match millis.checked_mul(1_000_000) {
            Some(nanos) => Ok(Self(nanos)),
            None => Err(VideoDelayError::ArithmeticOverflow),
        }
    }

    /// Creates a delay of `frames` frame periods at `rate`, rounded to the
    /// nearest nanosecond (ties up).
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::ArithmeticOverflow`] if the result is not
    /// representable.
    pub fn from_frames(frames: u64, rate: FrameRate) -> Result<Self, VideoDelayError> {
        let numerator = u128::from(frames)
            .checked_mul(u128::from(rate.denominator()))
            .and_then(|value| value.checked_mul(NANOS_PER_SECOND))
            .ok_or(VideoDelayError::ArithmeticOverflow)?;
        let nanos = rounded_div(numerator, u128::from(rate.numerator()));
        u64::try_from(nanos)
            .map(Self)
            .map_err(|_| VideoDelayError::ArithmeticOverflow)
    }

    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Returns the nearest whole number of frame periods at `rate`.
    #[must_use]
    pub fn frames_at(self, rate: FrameRate) -> u64 {
        let numerator = u128::from(self.0) * u128::from(rate.numerator());
        let denominator = u128::from(rate.denominator()) * NANOS_PER_SECOND;
        u64::try_from(rounded_div(numerator, denominator)).unwrap_or(u64::MAX)
    }

    /// Returns the nearest whole number of audio samples at
    /// `sample_rate_hz`, so an audio path can match this video delay.
    #[must_use]
    pub fn audio_samples(self, sample_rate_hz: u32) -> u64 {
        let numerator = u128::from(self.0) * u128::from(sample_rate_hz);
        u64::try_from(rounded_div(numerator, NANOS_PER_SECOND)).unwrap_or(u64::MAX)
    }
}

/// What the line outputs before its first frame becomes due.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StartupPolicy {
    /// Output nothing; downstream keeps its own previous or empty state.
    Nothing,
    /// Ask downstream to present black.
    #[default]
    Black,
}

/// How a live change of the requested delay reaches the effective delay.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum DelayChangePolicy {
    /// Apply the new delay at the next output (bounded freeze or skip).
    #[default]
    Immediate,
    /// Converge by at most `per_mille / 1000` of the elapsed output time per
    /// output; `per_mille` must be in `1..=1000`.
    Slew { per_mille: u16 },
}

/// Validated delay-line bounds and policies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoDelayConfig {
    max_delay: DelayDuration,
    max_bytes: usize,
    max_frames: usize,
    max_skew: DelayDuration,
    startup: StartupPolicy,
    change: DelayChangePolicy,
}

/// Estimated delay capacity for a uniform frame size and rate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DelayCapacity {
    /// Frames the buffer can hold at once.
    pub frames_held: usize,
    /// Largest delay that runs without capacity eviction when every output
    /// follows the push of the frame at the same timestamp.
    pub max_delay: DelayDuration,
}

impl VideoDelayConfig {
    /// Creates a configuration with black start-up, immediate delay changes,
    /// and the default one-second skew tolerance.
    ///
    /// # Errors
    ///
    /// Returns an error when `max_delay` exceeds [`MAX_VIDEO_DELAY_NANOS`],
    /// `max_bytes` is zero, or `max_frames` is outside
    /// `2..=`[`MAX_VIDEO_DELAY_FRAMES`].
    pub const fn new(
        max_delay: DelayDuration,
        max_bytes: usize,
        max_frames: usize,
    ) -> Result<Self, VideoDelayError> {
        if max_delay.0 > MAX_VIDEO_DELAY_NANOS {
            return Err(VideoDelayError::DelayOutOfRange {
                requested: max_delay,
                maximum: DelayDuration(MAX_VIDEO_DELAY_NANOS),
            });
        }
        if max_bytes == 0 {
            return Err(VideoDelayError::ZeroByteBudget);
        }
        if max_frames < 2 || max_frames > MAX_VIDEO_DELAY_FRAMES {
            return Err(VideoDelayError::FrameBudgetOutOfRange {
                requested: max_frames,
                maximum: MAX_VIDEO_DELAY_FRAMES,
            });
        }
        Ok(Self {
            max_delay,
            max_bytes,
            max_frames,
            max_skew: DelayDuration(DEFAULT_VIDEO_DELAY_MAX_SKEW_NANOS),
            startup: StartupPolicy::Black,
            change: DelayChangePolicy::Immediate,
        })
    }

    #[must_use]
    pub const fn with_startup(mut self, startup: StartupPolicy) -> Self {
        self.startup = startup;
        self
    }

    /// Sets the live delay-change policy.
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::InvalidSlew`] for a slew outside `1..=1000`.
    pub const fn with_change_policy(
        mut self,
        change: DelayChangePolicy,
    ) -> Result<Self, VideoDelayError> {
        if let DelayChangePolicy::Slew { per_mille } = change
            && (per_mille == 0 || per_mille > 1000)
        {
            return Err(VideoDelayError::InvalidSlew { per_mille });
        }
        self.change = change;
        Ok(self)
    }

    /// Sets how far a line timestamp may be from `now` before the frame is
    /// treated as a discontinuity.
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::ZeroSkew`] for a zero tolerance.
    pub const fn with_max_skew(mut self, max_skew: DelayDuration) -> Result<Self, VideoDelayError> {
        if max_skew.0 == 0 {
            return Err(VideoDelayError::ZeroSkew);
        }
        self.max_skew = max_skew;
        Ok(self)
    }

    #[must_use]
    pub const fn max_delay(&self) -> DelayDuration {
        self.max_delay
    }

    #[must_use]
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    #[must_use]
    pub const fn max_frames(&self) -> usize {
        self.max_frames
    }

    #[must_use]
    pub const fn max_skew(&self) -> DelayDuration {
        self.max_skew
    }

    #[must_use]
    pub const fn startup(&self) -> StartupPolicy {
        self.startup
    }

    #[must_use]
    pub const fn change_policy(&self) -> DelayChangePolicy {
        self.change
    }

    /// Estimates how much delay fits for frames of `frame_bytes` at `rate`.
    ///
    /// Steady state at a delay of `N` frames holds `N + 1` frames (the
    /// presented frame and the `N` newer ones), so the frame-based limit is
    /// one less than the number of frames that fit. The result is also capped
    /// by [`Self::max_delay`].
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::ArithmeticOverflow`] if the frame duration
    /// arithmetic overflows.
    pub fn capacity_for(
        &self,
        frame_bytes: usize,
        rate: FrameRate,
    ) -> Result<DelayCapacity, VideoDelayError> {
        let by_bytes = self
            .max_bytes
            .checked_div(frame_bytes)
            .unwrap_or(usize::MAX);
        let frames_held = by_bytes.min(self.max_frames);
        let delay_frames = u64::try_from(frames_held.saturating_sub(1))
            .map_err(|_| VideoDelayError::ArithmeticOverflow)?;
        let by_frames = DelayDuration::from_frames(delay_frames, rate)?;
        Ok(DelayCapacity {
            frames_held,
            max_delay: by_frames.min(self.max_delay),
        })
    }
}

/// One source frame offered to the delay line.
///
/// `payload` is opaque; it is expected to be a cheap handle (for example an
/// `Arc` of a CPU frame or a GPU texture lease). `bytes` is the memory the
/// payload pins and is what the byte budget accounts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelayInputFrame<H> {
    pts_nanos: i64,
    duration_nanos: u64,
    bytes: usize,
    discontinuity: bool,
    payload: H,
}

impl<H> DelayInputFrame<H> {
    /// Creates a frame description.
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::ZeroFrameDuration`] for a zero duration.
    pub fn new(
        pts_nanos: i64,
        duration_nanos: u64,
        bytes: usize,
        payload: H,
    ) -> Result<Self, VideoDelayError> {
        if duration_nanos == 0 {
            return Err(VideoDelayError::ZeroFrameDuration);
        }
        Ok(Self {
            pts_nanos,
            duration_nanos,
            bytes,
            discontinuity: false,
            payload,
        })
    }

    /// Marks this frame as the first of a new, unrelated timestamp segment.
    #[must_use]
    pub fn with_discontinuity(mut self) -> Self {
        self.discontinuity = true;
        self
    }

    #[must_use]
    pub const fn pts_nanos(&self) -> i64 {
        self.pts_nanos
    }

    #[must_use]
    pub const fn duration_nanos(&self) -> u64 {
        self.duration_nanos
    }

    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub const fn is_discontinuity(&self) -> bool {
        self.discontinuity
    }
}

/// Why a buffered frame was presented.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DelayedFrameKind {
    /// First presentation, at its delayed time.
    Fresh,
    /// The same frame as the previous output.
    Repeat,
    /// First presentation before its delayed time because capacity eviction
    /// removed the frames that were due.
    Early,
}

/// A presented frame borrowed from the line.
#[derive(Debug, Eq, PartialEq)]
pub struct DelayedFrame<'a, H> {
    pub payload: &'a H,
    pub source_pts_nanos: i64,
    pub line_pts_nanos: i64,
    pub duration_nanos: u64,
    pub kind: DelayedFrameKind,
    /// Unpresented frames passed over to reach this one.
    pub skipped: u64,
}

/// The result of one output slot.
#[derive(Debug, Eq, PartialEq)]
pub enum DelayOutput<'a, H> {
    /// Filling with [`StartupPolicy::Nothing`].
    Nothing,
    /// Filling with [`StartupPolicy::Black`].
    Black,
    Frame(DelayedFrame<'a, H>),
}

impl<H> DelayOutput<'_, H> {
    #[must_use]
    pub const fn frame(&self) -> Option<&DelayedFrame<'_, H>> {
        match self {
            Self::Frame(frame) => Some(frame),
            Self::Nothing | Self::Black => None,
        }
    }
}

/// Observable line state.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum DelayLineState {
    #[default]
    Filling,
    Running,
}

/// What one push did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PushReport {
    /// The frame started a new segment and the buffer was cleared.
    pub discontinuity: bool,
    /// Unpresented frames evicted to stay within the memory bounds.
    pub capacity_drops: u64,
    /// The frame was late or a duplicate (its timestamp is not newer than the
    /// newest buffered frame, but within `max_skew` of it) and was dropped
    /// without changing the line.
    pub late_drop: bool,
}

/// Point-in-time line telemetry. Counters are cumulative since creation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VideoDelayTelemetry {
    pub state: DelayLineState,
    pub requested_delay: DelayDuration,
    pub effective_delay: DelayDuration,
    pub frames_held: usize,
    pub bytes_held: usize,
    /// Span from the oldest held frame start to the newest held frame end.
    pub buffered: DelayDuration,
    /// `buffered / requested_delay`, capped at 1000 (1000 for a zero delay).
    pub fill_per_mille: u16,
    /// Frames accepted into the buffer (late drops are not counted).
    pub frames_pushed: u64,
    /// Late or duplicate frames dropped on push without a reset.
    pub late_drops: u64,
    /// First presentations (fresh and early).
    pub frames_presented: u64,
    pub repeats: u64,
    /// Unpresented frames passed over by selection.
    pub skips: u64,
    /// Unpresented frames evicted by the frame or byte bound.
    pub capacity_drops: u64,
    /// Unpresented frames discarded by a discontinuity or reset.
    pub discontinuity_drops: u64,
    pub early_presentations: u64,
    /// Outputs answered by the start-up policy.
    pub startup_outputs: u64,
    pub discontinuities: u64,
    pub resets: u64,
}

/// Delay-line failures. Failed calls leave the line unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoDelayError {
    DelayOutOfRange {
        requested: DelayDuration,
        maximum: DelayDuration,
    },
    ZeroByteBudget,
    FrameBudgetOutOfRange {
        requested: usize,
        maximum: usize,
    },
    InvalidSlew {
        per_mille: u16,
    },
    ZeroSkew,
    ZeroFrameDuration,
    FrameTooLarge {
        bytes: usize,
        maximum: usize,
    },
    NowRegressed {
        previous: i64,
        actual: i64,
    },
    ArithmeticOverflow,
}

impl fmt::Display for VideoDelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DelayOutOfRange { requested, maximum } => write!(
                formatter,
                "video delay {} ns exceeds the maximum of {} ns",
                requested.0, maximum.0
            ),
            Self::ZeroByteBudget => formatter.write_str("video delay byte budget must be nonzero"),
            Self::FrameBudgetOutOfRange { requested, maximum } => write!(
                formatter,
                "video delay frame budget {requested} is outside 2..={maximum}"
            ),
            Self::InvalidSlew { per_mille } => write!(
                formatter,
                "video delay slew {per_mille} per mille is outside 1..=1000"
            ),
            Self::ZeroSkew => formatter.write_str("video delay skew tolerance must be nonzero"),
            Self::ZeroFrameDuration => {
                formatter.write_str("video delay frame duration must be nonzero")
            }
            Self::FrameTooLarge { bytes, maximum } => write!(
                formatter,
                "video delay frame of {bytes} bytes exceeds the {maximum}-byte budget"
            ),
            Self::NowRegressed { previous, actual } => write!(
                formatter,
                "video delay time {actual} ns precedes {previous} ns"
            ),
            Self::ArithmeticOverflow => formatter.write_str("video delay arithmetic overflow"),
        }
    }
}

impl std::error::Error for VideoDelayError {}

#[derive(Clone, Debug)]
struct Entry<H> {
    source_pts: i64,
    line_pts: i64,
    duration: u64,
    bytes: usize,
    presented: bool,
    payload: H,
}

/// Bounded per-input video delay line. See the module documentation.
#[derive(Clone, Debug)]
pub struct VideoDelayLine<H> {
    config: VideoDelayConfig,
    requested: DelayDuration,
    effective: u64,
    ring: VecDeque<Entry<H>>,
    bytes_held: usize,
    state: DelayLineState,
    rebase: i64,
    last_source_pts: Option<i64>,
    last_now: Option<i64>,
    last_output_now: Option<i64>,
    capacity_pressure: bool,
    telemetry: VideoDelayTelemetry,
}

impl<H> VideoDelayLine<H> {
    /// Creates an empty, filling line.
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::DelayOutOfRange`] if `delay` exceeds the
    /// configured maximum.
    pub fn new(config: VideoDelayConfig, delay: DelayDuration) -> Result<Self, VideoDelayError> {
        check_delay(&config, delay)?;
        Ok(Self {
            config,
            requested: delay,
            effective: delay.0,
            ring: VecDeque::new(),
            bytes_held: 0,
            state: DelayLineState::Filling,
            rebase: 0,
            last_source_pts: None,
            last_now: None,
            last_output_now: None,
            capacity_pressure: false,
            telemetry: VideoDelayTelemetry::default(),
        })
    }

    #[must_use]
    pub const fn config(&self) -> &VideoDelayConfig {
        &self.config
    }

    #[must_use]
    pub const fn state(&self) -> DelayLineState {
        self.state
    }

    #[must_use]
    pub const fn requested_delay(&self) -> DelayDuration {
        self.requested
    }

    /// The delay currently applied to frame selection. An audio path matching
    /// this input should use this value (see [`DelayDuration::audio_samples`]).
    #[must_use]
    pub const fn effective_delay(&self) -> DelayDuration {
        DelayDuration(self.effective)
    }

    /// Changes the requested delay; see the module documentation for how the
    /// effective delay converges.
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::DelayOutOfRange`] above the configured
    /// maximum; the line is unchanged.
    pub fn set_delay(&mut self, delay: DelayDuration) -> Result<(), VideoDelayError> {
        check_delay(&self.config, delay)?;
        self.requested = delay;
        if self.state == DelayLineState::Filling
            || self.config.change == DelayChangePolicy::Immediate
        {
            self.effective = delay.0;
        }
        Ok(())
    }

    /// Accepts one source frame at output time `now`.
    ///
    /// A late or duplicate frame is dropped and reported as
    /// [`PushReport::late_drop`]; see the module documentation.
    ///
    /// # Errors
    ///
    /// Returns an error for a frame above the byte budget, a `now` earlier than
    /// a previous call, or timestamp arithmetic overflow. Failure leaves the
    /// line unchanged.
    pub fn push(
        &mut self,
        now: i64,
        frame: DelayInputFrame<H>,
    ) -> Result<PushReport, VideoDelayError> {
        if frame.bytes > self.config.max_bytes {
            return Err(VideoDelayError::FrameTooLarge {
                bytes: frame.bytes,
                maximum: self.config.max_bytes,
            });
        }
        self.check_now(now)?;

        let pts = frame.pts_nanos;
        let max_skew = u128::from(self.config.max_skew.0);
        // A frame at or slightly before the newest buffered frame is late or
        // a duplicate: it carries nothing newer, so it is dropped rather than
        // allowed to discard the whole buffer.
        let late = !frame.discontinuity
            && self.last_source_pts.is_some_and(|newest| {
                pts <= newest && (i128::from(newest) - i128::from(pts)).unsigned_abs() <= max_skew
            });
        if late {
            self.telemetry.late_drops += 1;
            self.last_now = Some(now);
            return Ok(PushReport {
                late_drop: true,
                ..PushReport::default()
            });
        }
        let line_with_current = pts.checked_add(self.rebase);
        let skewed = line_with_current
            .is_none_or(|line| (i128::from(line) - i128::from(now)).unsigned_abs() > max_skew);
        // Any remaining non-increasing timestamp is a backward jump beyond
        // `max_skew`.
        let discontinuity = self.last_source_pts.is_some()
            && (frame.discontinuity
                || self.last_source_pts.is_some_and(|newest| pts <= newest)
                || skewed);
        let (rebase, line_pts) = if discontinuity || (self.last_source_pts.is_none() && skewed) {
            let rebase = i64::try_from(i128::from(now) - i128::from(pts))
                .map_err(|_| VideoDelayError::ArithmeticOverflow)?;
            (rebase, now)
        } else {
            let line = line_with_current.ok_or(VideoDelayError::ArithmeticOverflow)?;
            (self.rebase, line)
        };
        line_pts
            .checked_add_unsigned(frame.duration_nanos)
            .ok_or(VideoDelayError::ArithmeticOverflow)?;

        if discontinuity {
            self.telemetry.discontinuities += 1;
            self.clear_segment();
        }
        self.rebase = rebase;
        let mut report = PushReport {
            discontinuity,
            ..PushReport::default()
        };
        while !self.ring.is_empty()
            && (self.ring.len() >= self.config.max_frames
                || self.bytes_held.saturating_add(frame.bytes) > self.config.max_bytes)
        {
            if let Some(evicted) = self.pop_front()
                && !evicted.presented
            {
                report.capacity_drops += 1;
            }
            if self.state == DelayLineState::Filling {
                self.capacity_pressure = true;
            }
        }
        self.telemetry.capacity_drops += report.capacity_drops;
        self.bytes_held += frame.bytes;
        self.ring.push_back(Entry {
            source_pts: pts,
            line_pts,
            duration: frame.duration_nanos,
            bytes: frame.bytes,
            presented: false,
            payload: frame.payload,
        });
        self.telemetry.frames_pushed += 1;
        self.last_source_pts = Some(pts);
        self.last_now = Some(now);
        Ok(report)
    }

    /// Produces the output for the slot at time `now`.
    ///
    /// # Errors
    ///
    /// Returns [`VideoDelayError::NowRegressed`] if `now` is earlier than a
    /// previous call; the line is unchanged.
    pub fn output(&mut self, now: i64) -> Result<DelayOutput<'_, H>, VideoDelayError> {
        self.check_now(now)?;
        self.advance_effective_delay(now);
        self.last_now = Some(now);
        self.last_output_now = Some(now);

        let target = i128::from(now) - i128::from(self.effective);
        let candidate = self.ring.iter().rposition(|entry| {
            let slack = SELECTION_SLACK_NANOS.min(entry.duration / 2);
            i128::from(entry.line_pts) <= target + i128::from(slack)
        });
        let mut skipped = 0;
        let kind = match candidate {
            Some(index) => {
                for _ in 0..index {
                    if let Some(entry) = self.pop_front()
                        && !entry.presented
                    {
                        skipped += 1;
                    }
                }
                if self.ring[0].presented {
                    DelayedFrameKind::Repeat
                } else {
                    DelayedFrameKind::Fresh
                }
            }
            None if !self.ring.is_empty()
                && (self.state == DelayLineState::Running || self.capacity_pressure) =>
            {
                if self.ring[0].presented {
                    DelayedFrameKind::Repeat
                } else {
                    DelayedFrameKind::Early
                }
            }
            None => {
                self.telemetry.startup_outputs += 1;
                return Ok(match self.config.startup {
                    StartupPolicy::Nothing => DelayOutput::Nothing,
                    StartupPolicy::Black => DelayOutput::Black,
                });
            }
        };

        self.state = DelayLineState::Running;
        self.capacity_pressure = false;
        self.telemetry.skips += skipped;
        match kind {
            DelayedFrameKind::Repeat => self.telemetry.repeats += 1,
            DelayedFrameKind::Fresh => self.telemetry.frames_presented += 1,
            DelayedFrameKind::Early => {
                self.telemetry.frames_presented += 1;
                self.telemetry.early_presentations += 1;
            }
        }
        let entry = &mut self.ring[0];
        entry.presented = true;
        Ok(DelayOutput::Frame(DelayedFrame {
            payload: &entry.payload,
            source_pts_nanos: entry.source_pts,
            line_pts_nanos: entry.line_pts,
            duration_nanos: entry.duration,
            kind,
            skipped,
        }))
    }

    /// Discards all buffered frames and returns to filling. Output time stays
    /// monotonic across a reset.
    pub fn reset(&mut self) {
        self.telemetry.resets += 1;
        self.clear_segment();
    }

    #[must_use]
    pub fn telemetry(&self) -> VideoDelayTelemetry {
        let buffered = match (self.ring.front(), self.ring.back()) {
            (Some(front), Some(back)) => {
                let end = i128::from(back.line_pts) + i128::from(back.duration);
                u64::try_from(end - i128::from(front.line_pts)).unwrap_or(u64::MAX)
            }
            _ => 0,
        };
        let fill_per_mille = if self.requested.0 == 0 {
            1000
        } else {
            let ratio = u128::from(buffered) * 1000 / u128::from(self.requested.0);
            u16::try_from(ratio.min(1000)).unwrap_or(1000)
        };
        VideoDelayTelemetry {
            state: self.state,
            requested_delay: self.requested,
            effective_delay: DelayDuration(self.effective),
            frames_held: self.ring.len(),
            bytes_held: self.bytes_held,
            buffered: DelayDuration(buffered),
            fill_per_mille,
            ..self.telemetry
        }
    }

    fn check_now(&self, now: i64) -> Result<(), VideoDelayError> {
        match self.last_now {
            Some(previous) if now < previous => Err(VideoDelayError::NowRegressed {
                previous,
                actual: now,
            }),
            _ => Ok(()),
        }
    }

    fn advance_effective_delay(&mut self, now: i64) {
        let requested = self.requested.0;
        if self.state == DelayLineState::Filling {
            self.effective = requested;
            return;
        }
        let DelayChangePolicy::Slew { per_mille } = self.config.change else {
            self.effective = requested;
            return;
        };
        let elapsed = self.last_output_now.map_or(0, |previous| {
            (i128::from(now) - i128::from(previous)).unsigned_abs()
        });
        let step = u64::try_from(elapsed * u128::from(per_mille) / 1000).unwrap_or(u64::MAX);
        self.effective = if self.effective < requested {
            self.effective.saturating_add(step).min(requested)
        } else {
            self.effective.saturating_sub(step).max(requested)
        };
    }

    fn pop_front(&mut self) -> Option<Entry<H>> {
        let entry = self.ring.pop_front()?;
        self.bytes_held -= entry.bytes;
        Some(entry)
    }

    fn clear_segment(&mut self) {
        let unpresented = self.ring.iter().filter(|entry| !entry.presented).count();
        self.telemetry.discontinuity_drops += u64::try_from(unpresented).unwrap_or(u64::MAX);
        self.ring.clear();
        self.bytes_held = 0;
        self.state = DelayLineState::Filling;
        self.effective = self.requested.0;
        self.last_source_pts = None;
        self.capacity_pressure = false;
    }
}

const fn check_delay(
    config: &VideoDelayConfig,
    delay: DelayDuration,
) -> Result<(), VideoDelayError> {
    if delay.0 > config.max_delay.0 {
        Err(VideoDelayError::DelayOutOfRange {
            requested: delay,
            maximum: config.max_delay,
        })
    } else {
        Ok(())
    }
}

fn rounded_div(numerator: u128, denominator: u128) -> u128 {
    numerator / denominator + u128::from(numerator % denominator * 2 >= denominator)
}
