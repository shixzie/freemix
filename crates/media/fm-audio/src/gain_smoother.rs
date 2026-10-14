//! Zipper-free gain smoothing for high-rate fader and controller moves.
//!
//! Control surfaces deliver gain targets at control rate (one coalesced value
//! per tick). Applying each target as a step produces audible "zipper"
//! discontinuities. [`GainSmoother`] instead ramps linearly, per sample, from
//! wherever it currently is to the newest target:
//!
//! - the per-sample change never exceeds [`GainSmoother::max_step_bound`];
//! - every ramp lasts at most `max_ramp_samples`, so settling time is bounded;
//! - the final ramp sample lands exactly on the target (no float drift);
//! - retargeting mid-ramp starts from the current value, so there is never a
//!   discontinuity between blocks or between consecutive targets.
//!
//! Sample positions are computed from the ramp start rather than accumulated,
//! so a ramp is identical however it is split across blocks.

use crate::{Gain, MAX_CHANNELS, MAX_GAIN_DB, MAX_SAMPLES_PER_BLOCK};
use core::fmt;

/// Largest `max_ramp_samples` accepted (one second at 48 kHz).
pub const MAX_GAIN_SMOOTHER_RAMP_SAMPLES: u32 = 48_000;

/// Smoothing limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainSmootherConfig {
    /// Largest linear-gain change per sample for a ramp that fits in
    /// `max_ramp_samples` (`0 < max_step <= 1`).
    pub max_step: f32,
    /// Shortest ramp, so even small changes are interpolated (`>= 1`).
    pub min_ramp_samples: u32,
    /// Longest ramp; a change too large to respect `max_step` within it is
    /// spread evenly over exactly this many samples.
    pub max_ramp_samples: u32,
}

impl Default for GainSmootherConfig {
    /// Unity in 10 ms, at least 1 ms, at most 50 ms at 48 kHz.
    fn default() -> Self {
        Self {
            max_step: 1.0 / 480.0,
            min_ramp_samples: 48,
            max_ramp_samples: 2_400,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GainSmootherError {
    InvalidMaxStep(f32),
    InvalidRampBounds {
        minimum: u32,
        maximum: u32,
    },
    ChannelCountOutOfRange(usize),
    SampleCountOutOfRange(usize),
    PlaneLengthMismatch {
        plane: usize,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for GainSmootherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMaxStep(step) => {
                write!(formatter, "gain smoother step {step} is outside (0, 1]")
            }
            Self::InvalidRampBounds { minimum, maximum } => write!(
                formatter,
                "gain smoother ramp bounds {minimum}..={maximum} are invalid \
                 (1 <= min <= max <= {MAX_GAIN_SMOOTHER_RAMP_SAMPLES})"
            ),
            Self::ChannelCountOutOfRange(channels) => {
                write!(
                    formatter,
                    "gain smoother channel count {channels} is out of range"
                )
            }
            Self::SampleCountOutOfRange(samples) => {
                write!(
                    formatter,
                    "gain smoother sample count {samples} is out of range"
                )
            }
            Self::PlaneLengthMismatch {
                plane,
                expected,
                actual,
            } => write!(
                formatter,
                "plane {plane} has {actual} samples, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for GainSmootherError {}

/// Running counters since construction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GainSmootherTelemetry {
    /// Targets that started a new ramp.
    pub retargets: u64,
    /// Ramps stretched to `max_ramp_samples` (their step exceeds `max_step`
    /// but stays within [`GainSmoother::max_step_bound`]).
    pub clamped_ramps: u64,
    /// Explicit discontinuous jumps via [`GainSmoother::jump_to`].
    pub jumps: u64,
}

/// A per-sample linear gain ramp toward the latest target.
#[derive(Clone, Debug)]
pub struct GainSmoother {
    config: GainSmootherConfig,
    start: f64,
    target: f32,
    current: f32,
    position: u32,
    length: u32,
    telemetry: GainSmootherTelemetry,
}

fn max_linear_gain() -> f64 {
    10.0_f64.powf(f64::from(MAX_GAIN_DB) / 20.0)
}

impl GainSmoother {
    /// Creates a settled smoother at `initial`.
    ///
    /// # Errors
    ///
    /// Returns an error for a step outside `(0, 1]` or ramp bounds outside
    /// `1 <= min <= max <= MAX_GAIN_SMOOTHER_RAMP_SAMPLES`.
    pub fn new(config: GainSmootherConfig, initial: Gain) -> Result<Self, GainSmootherError> {
        if !(config.max_step.is_finite() && config.max_step > 0.0 && config.max_step <= 1.0) {
            return Err(GainSmootherError::InvalidMaxStep(config.max_step));
        }
        if config.min_ramp_samples == 0
            || config.min_ramp_samples > config.max_ramp_samples
            || config.max_ramp_samples > MAX_GAIN_SMOOTHER_RAMP_SAMPLES
        {
            return Err(GainSmootherError::InvalidRampBounds {
                minimum: config.min_ramp_samples,
                maximum: config.max_ramp_samples,
            });
        }
        Ok(Self {
            config,
            start: f64::from(initial.linear()),
            target: initial.linear(),
            current: initial.linear(),
            position: 0,
            length: 0,
            telemetry: GainSmootherTelemetry::default(),
        })
    }

    #[must_use]
    pub const fn config(&self) -> GainSmootherConfig {
        self.config
    }

    /// Guaranteed upper bound on the linear change between two consecutive
    /// output samples, for any sequence of targets in the valid [`Gain`]
    /// range: `max(max_step, max_linear_gain / max_ramp_samples)`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn max_step_bound(&self) -> f32 {
        let clamped = max_linear_gain() / f64::from(self.config.max_ramp_samples);
        f64::from(self.config.max_step).max(clamped) as f32
    }

    /// The gain of the most recently produced sample.
    #[must_use]
    pub const fn current(&self) -> f32 {
        self.current
    }

    /// The linear gain being approached.
    #[must_use]
    pub const fn target(&self) -> f32 {
        self.target
    }

    /// Samples left until the target is reached exactly.
    #[must_use]
    pub const fn remaining(&self) -> u32 {
        self.length - self.position
    }

    #[must_use]
    pub const fn is_settled(&self) -> bool {
        self.position == self.length
    }

    #[must_use]
    pub const fn telemetry(&self) -> GainSmootherTelemetry {
        self.telemetry
    }

    /// Ramps from the current value toward `target`; returns the ramp length
    /// in samples (`0` when already there). Re-sending the pending target does
    /// not restart its ramp.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn set_target(&mut self, target: Gain) -> u32 {
        let target = target.linear();
        if target.to_bits() == self.target.to_bits() {
            return self.remaining();
        }
        self.target = target;
        let distance = (f64::from(target) - f64::from(self.current)).abs();
        if distance == 0.0 {
            self.start = f64::from(target);
            self.position = 0;
            self.length = 0;
            return 0;
        }
        let ideal = (distance / f64::from(self.config.max_step)).ceil();
        let maximum = self.config.max_ramp_samples;
        let length = if ideal > f64::from(maximum) {
            self.telemetry.clamped_ramps += 1;
            maximum
        } else {
            // `ideal` is a positive integer no larger than `maximum` (u32).
            (ideal as u32).max(self.config.min_ramp_samples)
        };
        self.start = f64::from(self.current);
        self.position = 0;
        self.length = length;
        self.telemetry.retargets += 1;
        length
    }

    /// Moves to `gain` immediately, cancelling any ramp. This is a deliberate
    /// discontinuity, for use only while the signal is silent (reset, load).
    pub fn jump_to(&mut self, gain: Gain) {
        self.current = gain.linear();
        self.target = gain.linear();
        self.start = f64::from(gain.linear());
        self.position = 0;
        self.length = 0;
        self.telemetry.jumps += 1;
    }

    #[allow(clippy::cast_possible_truncation)]
    fn value_at(&self, position: u32) -> f32 {
        if position >= self.length {
            return self.target;
        }
        let fraction = f64::from(position) / f64::from(self.length);
        (self.start + (f64::from(self.target) - self.start) * fraction) as f32
    }

    /// Produces the gain for the next sample.
    pub fn next_gain(&mut self) -> f32 {
        if self.position < self.length {
            self.position += 1;
            self.current = self.value_at(self.position);
        }
        self.current
    }

    /// Advances `samples` without producing output (an inaudible strip),
    /// leaving the ramp exactly where per-sample processing would.
    pub fn skip(&mut self, samples: usize) {
        let samples = u32::try_from(samples).unwrap_or(u32::MAX);
        if self.position < self.length {
            self.position = self.position.saturating_add(samples).min(self.length);
            self.current = self.value_at(self.position);
        }
    }

    /// Writes one gain per sample into `gains`.
    ///
    /// # Errors
    ///
    /// Returns an error if `gains` exceeds [`MAX_SAMPLES_PER_BLOCK`]; the
    /// smoother is unchanged.
    pub fn fill(&mut self, gains: &mut [f32]) -> Result<(), GainSmootherError> {
        if gains.len() > MAX_SAMPLES_PER_BLOCK {
            return Err(GainSmootherError::SampleCountOutOfRange(gains.len()));
        }
        for gain in gains {
            *gain = self.next_gain();
        }
        Ok(())
    }

    /// Multiplies equal-length planar channels in place, advancing one gain
    /// step per sample frame (all channels share each step).
    ///
    /// # Errors
    ///
    /// Returns an error for `0` or more than [`MAX_CHANNELS`] planes, more
    /// than [`MAX_SAMPLES_PER_BLOCK`] samples, or unequal plane lengths. All
    /// validation happens before any sample or ramp state changes.
    pub fn apply(&mut self, planes: &mut [&mut [f32]]) -> Result<(), GainSmootherError> {
        if planes.is_empty() || planes.len() > MAX_CHANNELS {
            return Err(GainSmootherError::ChannelCountOutOfRange(planes.len()));
        }
        let samples = planes[0].len();
        if samples > MAX_SAMPLES_PER_BLOCK {
            return Err(GainSmootherError::SampleCountOutOfRange(samples));
        }
        for (plane, data) in planes.iter().enumerate() {
            if data.len() != samples {
                return Err(GainSmootherError::PlaneLengthMismatch {
                    plane,
                    expected: samples,
                    actual: data.len(),
                });
            }
        }
        for sample in 0..samples {
            let gain = self.next_gain();
            for plane in planes.iter_mut() {
                plane[sample] *= gain;
            }
        }
        Ok(())
    }
}
