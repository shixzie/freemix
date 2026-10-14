use core::fmt;

use fm_types::FrameRate;

use super::ClockValue;

const NANOS_PER_SECOND: u128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaProgressError {
    /// A value exceeds [`ClockValue::MAX`].
    OutOfRange,
    /// The play range is empty or reversed, or ends after the duration.
    InvalidRange,
}

impl fmt::Display for MediaProgressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OutOfRange => "media time exceeds the supported clock range",
            Self::InvalidRange => "media play range must be non-empty and within the duration",
        })
    }
}

impl std::error::Error for MediaProgressError {}

/// A media position within an optional mark-in/mark-out play range, in
/// non-negative nanoseconds.
///
/// The position is clamped into the play range for every derived value, so a
/// decoder that reports a position slightly past the end (or before a
/// mark-in) reads as finished (or not started) rather than negative.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MediaProgress {
    position: u64,
    range_start: u64,
    range_end: u64,
}

impl MediaProgress {
    /// Position within the whole media.
    ///
    /// # Errors
    ///
    /// Returns [`MediaProgressError::OutOfRange`] above [`ClockValue::MAX`].
    pub fn new(position_nanos: u64, duration_nanos: u64) -> Result<Self, MediaProgressError> {
        Self::with_range(position_nanos, duration_nanos, 0, duration_nanos)
    }

    /// Position within a mark-in/mark-out range of the media.
    ///
    /// # Errors
    ///
    /// Returns [`MediaProgressError::OutOfRange`] above [`ClockValue::MAX`],
    /// or [`MediaProgressError::InvalidRange`] unless
    /// `mark_in < mark_out <= duration`. A zero-length media item with the
    /// whole-media range (`0, 0, 0`) is accepted and reads as finished.
    pub fn with_range(
        position_nanos: u64,
        duration_nanos: u64,
        mark_in_nanos: u64,
        mark_out_nanos: u64,
    ) -> Result<Self, MediaProgressError> {
        let max = ClockValue::MAX.as_nanos().unsigned_abs();
        if [position_nanos, duration_nanos]
            .iter()
            .any(|value| *value > max)
        {
            return Err(MediaProgressError::OutOfRange);
        }
        let whole_empty = duration_nanos == 0 && mark_in_nanos == 0 && mark_out_nanos == 0;
        if !whole_empty && (mark_in_nanos >= mark_out_nanos || mark_out_nanos > duration_nanos) {
            return Err(MediaProgressError::InvalidRange);
        }
        Ok(Self {
            position: position_nanos,
            range_start: mark_in_nanos,
            range_end: mark_out_nanos,
        })
    }

    /// Converts frame indexes at `rate` to exact nanosecond positions, using
    /// the start time of each frame (`floor(frame / rate)`).
    ///
    /// # Errors
    ///
    /// Returns [`MediaProgressError::OutOfRange`] above [`ClockValue::MAX`].
    pub fn from_frames(
        position_frame: u64,
        duration_frames: u64,
        rate: FrameRate,
    ) -> Result<Self, MediaProgressError> {
        Self::new(
            frame_to_nanos(position_frame, rate)?,
            frame_to_nanos(duration_frames, rate)?,
        )
    }

    fn clamped_position(self) -> u64 {
        self.position.clamp(self.range_start, self.range_end)
    }

    /// Time left until mark-out (or the end).
    #[must_use]
    pub fn remaining(self) -> ClockValue {
        to_value(self.range_end - self.clamped_position())
    }

    /// Time played since mark-in (or the start).
    #[must_use]
    pub fn elapsed(self) -> ClockValue {
        to_value(self.clamped_position() - self.range_start)
    }

    /// Length of the play range.
    #[must_use]
    pub fn length(self) -> ClockValue {
        to_value(self.range_end - self.range_start)
    }

    #[must_use]
    pub fn is_finished(self) -> bool {
        self.clamped_position() == self.range_end
    }
}

fn to_value(nanos: u64) -> ClockValue {
    // Construction bounds every stored value by ClockValue::MAX.
    ClockValue::from_nanos(i64::try_from(nanos).unwrap_or(i64::MAX)).unwrap_or(ClockValue::MAX)
}

fn frame_to_nanos(frame: u64, rate: FrameRate) -> Result<u64, MediaProgressError> {
    let nanos = u128::from(frame) * u128::from(rate.denominator()) * NANOS_PER_SECOND
        / u128::from(rate.numerator());
    u64::try_from(nanos).map_err(|_| MediaProgressError::OutOfRange)
}
