//! Clip timebases and exact cross-rate positions.
//!
//! Keyframes are authored in whole frames of the clip's own [`Timebase`]. A
//! project running at a different rate samples the clip at a
//! [`ClipPosition`]: the exact rational clip frame
//! `project_frame * clip_rate / project_rate`, carried in `u128` without
//! rounding. When the rates match the position is a whole frame and sampling
//! is identical to sampling at that frame.

use fm_types::FrameRate;

/// The frame rate a clip's keyframes are authored in.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Timebase(pub FrameRate);

impl Timebase {
    #[must_use]
    pub const fn new(rate: FrameRate) -> Self {
        Self(rate)
    }

    #[must_use]
    pub const fn rate(self) -> FrameRate {
        self.0
    }

    /// The exact clip position shown at `project_frame` of a project running
    /// at `project_rate`, both counted from the same instant.
    #[must_use]
    pub fn position_at(self, project_frame: u64, project_rate: FrameRate) -> ClipPosition {
        // clip frame = project_frame * (cn / cd) / (pn / pd)
        let numerator = u128::from(project_frame)
            * u128::from(self.0.numerator())
            * u128::from(project_rate.denominator());
        let denominator = u128::from(self.0.denominator()) * u128::from(project_rate.numerator());
        ClipPosition::new(numerator, denominator)
    }

    /// How many frames a project running at `project_rate` takes to play
    /// `clip_frames` frames of this timebase from its project frame zero:
    /// the first project frame whose [`Self::position_at`] is at or past
    /// `clip_frames`, `ceil(clip_frames * project_rate / clip_rate)`, exact.
    /// Zero clip frames take zero project frames.
    #[must_use]
    pub fn project_frames(self, clip_frames: u32, project_rate: FrameRate) -> u64 {
        // position_at(k) >= n  <=>  k >= n * cd * pn / (cn * pd)
        let numerator = u128::from(clip_frames)
            * u128::from(self.0.denominator())
            * u128::from(project_rate.numerator());
        let denominator =
            (u128::from(self.0.numerator()) * u128::from(project_rate.denominator())).max(1);
        u64::try_from(numerator.div_ceil(denominator)).unwrap_or(u64::MAX)
    }

    /// The first clip frame at or after `seconds` whole seconds:
    /// `ceil(seconds * rate)`.
    #[must_use]
    pub fn frame_at_second(self, seconds: u64) -> u64 {
        let numerator = u128::from(seconds) * u128::from(self.0.numerator());
        let denominator = u128::from(self.0.denominator());
        u64::try_from(numerator.div_ceil(denominator)).unwrap_or(u64::MAX)
    }
}

/// An exact, non-negative rational clip frame `numerator / denominator`,
/// kept in lowest terms.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClipPosition {
    numerator: u128,
    denominator: u128,
}

impl ClipPosition {
    /// A position; a zero denominator is treated as one.
    #[must_use]
    pub fn new(numerator: u128, denominator: u128) -> Self {
        let denominator = denominator.max(1);
        let divisor = gcd(numerator, denominator);
        Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        }
    }

    /// Exactly clip frame `frame`.
    #[must_use]
    pub const fn frame(frame: u32) -> Self {
        Self {
            numerator: frame as u128,
            denominator: 1,
        }
    }

    #[must_use]
    pub const fn numerator(self) -> u128 {
        self.numerator
    }

    #[must_use]
    pub const fn denominator(self) -> u128 {
        self.denominator
    }

    /// The whole frame at or before this position, saturating at `u32::MAX`.
    #[must_use]
    pub fn floor_frame(self) -> u32 {
        u32::try_from(self.numerator / self.denominator).unwrap_or(u32::MAX)
    }

    /// Whether this position is at or before whole frame `frame`.
    #[must_use]
    pub fn at_or_before(self, frame: u32) -> bool {
        self.numerator <= u128::from(frame) * self.denominator
    }

    /// Whether this position is strictly before whole frame `frame`.
    #[must_use]
    pub fn before(self, frame: u32) -> bool {
        self.numerator < u128::from(frame) * self.denominator
    }

    /// The position clamped to at most whole frame `frame`.
    #[must_use]
    pub fn min_frame(self, frame: u32) -> Self {
        if self.at_or_before(frame) {
            self
        } else {
            Self::frame(frame)
        }
    }
}

const fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    if left == 0 { 1 } else { left }
}
