//! Per-source sample-rate conversion into the engine clock with buffer-fill
//! drift compensation, discontinuity handling, and sync telemetry.
//!
//! A [`SourceSync`] accepts planar `f32` blocks stamped with the source's own
//! sample index (its clock) and renders any number of output samples at the
//! engine rate ([`crate::ENGINE_SAMPLE_RATE`] by default). The read position
//! advances by an exact rational step
//! `input_hz * (1e9 + correction_ppb) / (output_hz * 1e9)`, so with zero
//! correction the consumed input count is exactly
//! `floor(rendered * input_hz / output_hz)` over arbitrarily long runs.
//!
//! Drift compensation uses only buffer-fill observations (no paired clock
//! samples, in contrast with [`crate::ClockRecalibrationPolicy`]): a
//! [`DriftController`] low-pass filters the fill, and a PI loop trims the step
//! within a bounded ppm range and slew rate. The integral term is the
//! estimated source-vs-engine drift. The fill is sampled at each pull request
//! before rendering, the same quantity the priming threshold uses; while
//! silent (priming or recovering), input above the target is discarded so
//! playback always starts at the target latency.
//!
//! Discontinuity policy, applied to source timestamps on push:
//!
//! * contiguous index: appended;
//! * forward gap up to `max_gap_fill_samples`: concealed with silence, with
//!   declick ramps around the hole, so the timeline is preserved;
//! * backward overlap up to `max_overlap_trim_samples`: duplicate samples are
//!   trimmed from the head of the block;
//! * larger jumps, or blocks pushed via [`SourceSync::push_discontinuity`]:
//!   [`JumpAction::Splice`] declick-joins the new audio after the buffered
//!   audio (latency preserved), while [`JumpAction::Flush`] fades out and
//!   discards buffered audio beyond the kernel lookahead and pads with silence
//!   so the fill returns to the target (latency reset).
//!
//! Buffer faults: an underrun renders what is available with a fade-out,
//! outputs silence, and re-primes to the target fill before fading back in;
//! an overrun drops audio so the fill returns to the target, with declick
//! ramps at the cut.
//!
//! All storage is allocated in [`SourceSync::new`]; `push`, `pull`, and
//! `telemetry` never allocate.

use core::fmt;

use fm_types::SampleRate;

use crate::resampler::{
    ENGINE_SAMPLE_RATE, ResamplerError, SincKernel, f64_to_f32, u128_to_f64, usize_to_f64,
};

const PPB_SCALE: u64 = 1_000_000_000;

/// Largest bound accepted for the drift correction range.
pub const MAX_DRIFT_CORRECTION_PPM: f64 = 5_000.0;
/// Largest per-source buffer capacity, in input samples.
pub const MAX_SOURCE_SYNC_CAPACITY_SAMPLES: usize = 524_288;
/// Largest block accepted by one push, in input samples.
pub const MAX_SOURCE_SYNC_PUSH_SAMPLES: usize = 192_000;
/// Largest request accepted by one pull, in output samples.
pub const MAX_SOURCE_SYNC_PULL_SAMPLES: usize = 48_000;
/// Largest declick ramp, in samples.
pub const MAX_DECLICK_SAMPLES: usize = 4_800;

/// Errors from configuration, push, or pull.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SourceSyncError {
    Resampler(ResamplerError),
    ChannelCountOutOfRange(usize),
    InvalidDriftControl,
    DeclickTooLong(usize),
    TargetFillTooSmall {
        target: usize,
        minimum: usize,
    },
    CapacityOutOfRange {
        capacity: usize,
        minimum: usize,
    },
    PlaneCountMismatch {
        expected: usize,
        actual: usize,
    },
    PlaneLengthMismatch {
        plane: usize,
        expected: usize,
        actual: usize,
    },
    PushTooLarge(usize),
    PullTooLarge(usize),
    NonFiniteSample {
        channel: usize,
        sample: usize,
    },
    TimestampOverflow,
}

impl fmt::Display for SourceSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resampler(error) => write!(formatter, "{error}"),
            Self::ChannelCountOutOfRange(count) => {
                write!(
                    formatter,
                    "channel count {count} is outside 1..={}",
                    crate::MAX_CHANNELS
                )
            }
            Self::InvalidDriftControl => formatter.write_str("invalid drift control parameters"),
            Self::DeclickTooLong(samples) => write!(
                formatter,
                "declick ramp {samples} exceeds {MAX_DECLICK_SAMPLES} samples"
            ),
            Self::TargetFillTooSmall { target, minimum } => write!(
                formatter,
                "target fill {target} is below the kernel minimum {minimum}"
            ),
            Self::CapacityOutOfRange { capacity, minimum } => write!(
                formatter,
                "capacity {capacity} is outside {minimum}..={MAX_SOURCE_SYNC_CAPACITY_SAMPLES}"
            ),
            Self::PlaneCountMismatch { expected, actual } => {
                write!(formatter, "expected {expected} planes, got {actual}")
            }
            Self::PlaneLengthMismatch {
                plane,
                expected,
                actual,
            } => write!(
                formatter,
                "plane {plane} has {actual} samples, expected {expected}"
            ),
            Self::PushTooLarge(samples) => write!(
                formatter,
                "push of {samples} samples exceeds {MAX_SOURCE_SYNC_PUSH_SAMPLES}"
            ),
            Self::PullTooLarge(samples) => write!(
                formatter,
                "pull of {samples} samples exceeds {MAX_SOURCE_SYNC_PULL_SAMPLES}"
            ),
            Self::NonFiniteSample { channel, sample } => {
                write!(
                    formatter,
                    "non-finite sample at channel {channel}, index {sample}"
                )
            }
            Self::TimestampOverflow => formatter.write_str("source sample index overflow"),
        }
    }
}

impl std::error::Error for SourceSyncError {}

impl From<ResamplerError> for SourceSyncError {
    fn from(error: ResamplerError) -> Self {
        Self::Resampler(error)
    }
}

/// Parameters of the buffer-fill drift control loop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DriftControlConfig {
    max_correction_ppm: f64,
    loop_bandwidth_hz: f64,
    damping: f64,
    fill_smoothing_seconds: f64,
    max_slew_ppm_per_second: f64,
}

impl DriftControlConfig {
    /// Creates a validated loop configuration.
    ///
    /// `loop_bandwidth_hz` is the PI loop's natural frequency, `damping` its
    /// damping ratio, `fill_smoothing_seconds` the time constant of the
    /// one-pole fill filter, and `max_slew_ppm_per_second` bounds how fast the
    /// applied ratio may change.
    ///
    /// # Errors
    ///
    /// Returns [`SourceSyncError::InvalidDriftControl`] for non-finite,
    /// negative, or out-of-range values.
    pub fn new(
        max_correction_ppm: f64,
        loop_bandwidth_hz: f64,
        damping: f64,
        fill_smoothing_seconds: f64,
        max_slew_ppm_per_second: f64,
    ) -> Result<Self, SourceSyncError> {
        let valid = (0.0..=MAX_DRIFT_CORRECTION_PPM).contains(&max_correction_ppm)
            && loop_bandwidth_hz > 0.0
            && loop_bandwidth_hz <= 10.0
            && damping > 0.0
            && damping <= 10.0
            && (0.0..=60.0).contains(&fill_smoothing_seconds)
            && max_slew_ppm_per_second > 0.0
            && max_slew_ppm_per_second <= 1.0e6;
        if !valid {
            return Err(SourceSyncError::InvalidDriftControl);
        }
        Ok(Self {
            max_correction_ppm,
            loop_bandwidth_hz,
            damping,
            fill_smoothing_seconds,
            max_slew_ppm_per_second,
        })
    }

    /// A fixed nominal ratio: no correction is ever applied.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            max_correction_ppm: 0.0,
            loop_bandwidth_hz: 0.05,
            damping: 1.0,
            fill_smoothing_seconds: 0.5,
            max_slew_ppm_per_second: 200.0,
        }
    }

    #[must_use]
    pub const fn max_correction_ppm(self) -> f64 {
        self.max_correction_ppm
    }

    #[must_use]
    pub const fn loop_bandwidth_hz(self) -> f64 {
        self.loop_bandwidth_hz
    }

    #[must_use]
    pub const fn damping(self) -> f64 {
        self.damping
    }

    #[must_use]
    pub const fn fill_smoothing_seconds(self) -> f64 {
        self.fill_smoothing_seconds
    }

    #[must_use]
    pub const fn max_slew_ppm_per_second(self) -> f64 {
        self.max_slew_ppm_per_second
    }
}

impl Default for DriftControlConfig {
    /// ±500 ppm range, 0.05 Hz critically damped loop, 0.5 s fill smoothing,
    /// and at most 200 ppm/s of ratio change.
    fn default() -> Self {
        Self {
            max_correction_ppm: 500.0,
            ..Self::disabled()
        }
    }
}

/// Buffer-fill driven PI drift controller.
///
/// The controlled quantity is the fill error in seconds of source audio,
/// `e = (smoothed_fill - target) / input_hz`. Its output is a signed
/// fractional ratio correction: a positive value consumes input faster.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DriftController {
    config: DriftControlConfig,
    input_hz: f64,
    target_fill: f64,
    smoothed_fill: Option<f64>,
    integral: f64,
    applied: f64,
}

impl DriftController {
    #[must_use]
    pub fn new(
        config: DriftControlConfig,
        input_rate: SampleRate,
        target_fill_samples: f64,
    ) -> Self {
        Self {
            config,
            input_hz: f64::from(input_rate.hertz()),
            target_fill: target_fill_samples,
            smoothed_fill: None,
            integral: 0.0,
            applied: 0.0,
        }
    }

    /// Feeds one fill observation taken `elapsed_seconds` after the previous
    /// one and returns the applied correction in parts per billion.
    pub fn update(&mut self, fill_samples: f64, elapsed_seconds: f64) -> i64 {
        let limit = self.config.max_correction_ppm * 1e-6;
        if limit == 0.0
            || !fill_samples.is_finite()
            || !elapsed_seconds.is_finite()
            || elapsed_seconds <= 0.0
        {
            return self.applied_ppb();
        }
        let smoothed = match self.smoothed_fill {
            None => fill_samples,
            Some(previous) => {
                let tau = self.config.fill_smoothing_seconds;
                let alpha = elapsed_seconds / (tau + elapsed_seconds);
                previous + alpha * (fill_samples - previous)
            }
        };
        self.smoothed_fill = Some(smoothed);
        let error = (smoothed - self.target_fill) / self.input_hz;
        let omega = core::f64::consts::TAU * self.config.loop_bandwidth_hz;
        let ki = omega * omega;
        let kp = 2.0 * self.config.damping * omega;
        let integral = (self.integral + ki * error * elapsed_seconds).clamp(-limit, limit);
        let desired = (kp * error + integral).clamp(-limit, limit);
        let slew = self.config.max_slew_ppm_per_second * 1e-6 * elapsed_seconds;
        let wanted = desired - self.applied;
        let step = wanted.clamp(-slew, slew);
        // Conditional integration: while slew-limited, only let the integral
        // move in the direction that reduces the pending demand (anti-windup).
        if wanted.abs() <= slew || (integral - self.integral) * wanted <= 0.0 {
            self.integral = integral;
        }
        self.applied += step;
        self.applied_ppb()
    }

    /// Restarts fill smoothing from the next observation, keeping the drift
    /// estimate. Used after splices, overruns, and re-priming.
    pub fn reseed_fill(&mut self) {
        self.smoothed_fill = None;
    }

    /// Forgets the drift estimate and the applied correction.
    pub fn reset(&mut self) {
        self.smoothed_fill = None;
        self.integral = 0.0;
        self.applied = 0.0;
    }

    /// Estimated source-vs-engine clock drift (the loop integral).
    #[must_use]
    pub fn estimated_drift_ppm(&self) -> f64 {
        self.integral * 1e6
    }

    /// Correction currently applied to the conversion ratio.
    #[must_use]
    pub fn applied_correction_ppm(&self) -> f64 {
        self.applied * 1e6
    }

    #[must_use]
    pub fn applied_ppb(&self) -> i64 {
        round_to_i64(self.applied * 1e9)
    }

    #[must_use]
    pub fn smoothed_fill(&self) -> Option<f64> {
        self.smoothed_fill
    }

    #[must_use]
    pub const fn config(&self) -> DriftControlConfig {
        self.config
    }
}

/// What to do on a timestamp jump outside the gap/overlap tolerances or on an
/// explicitly flagged discontinuity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JumpAction {
    /// Keep buffered audio and declick-join the new audio after it.
    Splice,
    /// Fade out and drop buffered audio, then pad silence back to the target
    /// fill before the new audio. Counted as a reset.
    Flush,
}

/// Timestamp discontinuity handling for one source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscontinuityPolicy {
    max_gap_fill_samples: usize,
    max_overlap_trim_samples: usize,
    jump_action: JumpAction,
    declick_samples: usize,
}

impl DiscontinuityPolicy {
    /// # Errors
    ///
    /// Returns [`SourceSyncError::DeclickTooLong`] above
    /// [`MAX_DECLICK_SAMPLES`].
    pub const fn new(
        max_gap_fill_samples: usize,
        max_overlap_trim_samples: usize,
        jump_action: JumpAction,
        declick_samples: usize,
    ) -> Result<Self, SourceSyncError> {
        if declick_samples > MAX_DECLICK_SAMPLES {
            return Err(SourceSyncError::DeclickTooLong(declick_samples));
        }
        Ok(Self {
            max_gap_fill_samples,
            max_overlap_trim_samples,
            jump_action,
            declick_samples,
        })
    }

    /// Defaults for `rate`: conceal gaps and trim overlaps up to 100 ms,
    /// splice larger jumps, and use 1 ms declick ramps.
    #[must_use]
    pub const fn for_rate(rate: SampleRate) -> Self {
        let hz = rate.hertz() as usize;
        Self {
            max_gap_fill_samples: hz / 10,
            max_overlap_trim_samples: hz / 10,
            jump_action: JumpAction::Splice,
            declick_samples: hz / 1_000,
        }
    }

    #[must_use]
    pub const fn max_gap_fill_samples(self) -> usize {
        self.max_gap_fill_samples
    }

    #[must_use]
    pub const fn max_overlap_trim_samples(self) -> usize {
        self.max_overlap_trim_samples
    }

    #[must_use]
    pub const fn jump_action(self) -> JumpAction {
        self.jump_action
    }

    #[must_use]
    pub const fn declick_samples(self) -> usize {
        self.declick_samples
    }
}

/// Construction parameters for one [`SourceSync`]. Fill and capacity are in
/// input (source) samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceSyncConfig {
    input_rate: SampleRate,
    output_rate: SampleRate,
    channels: usize,
    target_fill_samples: usize,
    capacity_samples: usize,
    drift_control: DriftControlConfig,
    discontinuity: DiscontinuityPolicy,
}

impl SourceSyncConfig {
    /// Defaults: 48 kHz engine output, 20 ms target fill, 200 ms capacity,
    /// default drift control, and [`DiscontinuityPolicy::for_rate`].
    #[must_use]
    pub const fn new(input_rate: SampleRate, channels: usize) -> Self {
        let hz = input_rate.hertz() as usize;
        Self {
            input_rate,
            output_rate: ENGINE_SAMPLE_RATE,
            channels,
            target_fill_samples: hz / 50,
            capacity_samples: hz / 5,
            drift_control: DriftControlConfig {
                max_correction_ppm: 500.0,
                ..DriftControlConfig::disabled()
            },
            discontinuity: DiscontinuityPolicy::for_rate(input_rate),
        }
    }

    #[must_use]
    pub const fn with_output_rate(mut self, rate: SampleRate) -> Self {
        self.output_rate = rate;
        self
    }

    #[must_use]
    pub const fn with_target_fill_samples(mut self, samples: usize) -> Self {
        self.target_fill_samples = samples;
        self
    }

    #[must_use]
    pub const fn with_capacity_samples(mut self, samples: usize) -> Self {
        self.capacity_samples = samples;
        self
    }

    #[must_use]
    pub const fn with_drift_control(mut self, config: DriftControlConfig) -> Self {
        self.drift_control = config;
        self
    }

    #[must_use]
    pub const fn with_discontinuity_policy(mut self, policy: DiscontinuityPolicy) -> Self {
        self.discontinuity = policy;
        self
    }

    #[must_use]
    pub const fn input_rate(&self) -> SampleRate {
        self.input_rate
    }

    #[must_use]
    pub const fn output_rate(&self) -> SampleRate {
        self.output_rate
    }

    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    #[must_use]
    pub const fn target_fill_samples(&self) -> usize {
        self.target_fill_samples
    }

    #[must_use]
    pub const fn capacity_samples(&self) -> usize {
        self.capacity_samples
    }

    #[must_use]
    pub const fn drift_control(&self) -> DriftControlConfig {
        self.drift_control
    }

    #[must_use]
    pub const fn discontinuity(&self) -> DiscontinuityPolicy {
        self.discontinuity
    }
}

/// Output state of a source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceSyncState {
    /// Never started (or reset): silent until the target fill is reached.
    Priming,
    /// Rendering converted audio.
    Running,
    /// Silent after an underrun until the target fill is reached again.
    Recovering,
}

/// How one push was classified against the expected source sample index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushContinuity {
    /// First block after construction or [`SourceSync::reset`].
    Anchored,
    Contiguous,
    GapFilled {
        samples: u64,
    },
    OverlapTrimmed {
        samples: u64,
    },
    Spliced,
    Flushed,
}

/// Result of one push.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PushReport {
    pub continuity: PushContinuity,
    /// Input samples appended (excluding concealment silence).
    pub accepted_samples: usize,
    /// Samples dropped to resolve an overrun (buffered or incoming).
    pub overrun_dropped_samples: usize,
}

/// Result of one pull.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PullReport {
    /// Output samples converted from source audio.
    pub rendered_samples: usize,
    /// Output samples that are silence (priming, recovery, or underrun).
    pub silent_samples: usize,
    pub underrun: bool,
}

/// Per-source synchronization telemetry snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceSyncTelemetry {
    pub state: SourceSyncState,
    pub input_rate_hz: u32,
    pub output_rate_hz: u32,
    /// Estimated source-vs-engine clock drift.
    pub estimated_drift_ppm: f64,
    /// Ratio correction currently applied.
    pub applied_correction_ppm: f64,
    /// Unread input samples ahead of the fractional read position.
    pub buffer_fill_samples: f64,
    pub smoothed_fill_samples: Option<f64>,
    pub target_fill_samples: usize,
    pub capacity_samples: usize,
    pub peak_fill_samples: usize,
    pub pushed_samples: u64,
    pub concealed_gap_samples: u64,
    pub trimmed_overlap_samples: u64,
    /// Input discarded while silent so playback starts at the target fill.
    pub priming_trimmed_samples: u64,
    /// Whole input samples the read position has advanced past.
    pub consumed_input_samples: u64,
    pub rendered_output_samples: u64,
    pub silent_output_samples: u64,
    pub underruns: u64,
    pub underrun_samples: u64,
    pub overruns: u64,
    pub overrun_dropped_samples: u64,
    pub gaps_filled: u64,
    pub overlaps_trimmed: u64,
    /// Jumps beyond tolerance plus explicitly flagged discontinuities.
    pub discontinuities: u64,
    pub splices: u64,
    /// Flushes plus explicit [`SourceSync::reset`] calls.
    pub resets: u64,
}

/// Bounded, preallocated per-source converter into the engine clock.
#[derive(Debug)]
pub struct SourceSync {
    config: SourceSyncConfig,
    kernel: SincKernel,
    planes: Vec<Vec<f32>>,
    coefficients: Vec<f32>,
    len: usize,
    read_index: usize,
    frac: u64,
    denominator: u64,
    step: u64,
    state: SourceSyncState,
    fade_in_done: usize,
    expected_next: Option<u64>,
    controller: DriftController,
    telemetry: SourceSyncTelemetry,
}

impl SourceSync {
    /// Validates `config`, builds the kernel, and preallocates all storage.
    ///
    /// The target fill must be at least `half_width + 1` input samples and
    /// should exceed the input equivalent of the largest pull; the capacity
    /// must be at least twice the target.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration error.
    pub fn new(config: SourceSyncConfig) -> Result<Self, SourceSyncError> {
        if !(1..=crate::MAX_CHANNELS).contains(&config.channels) {
            return Err(SourceSyncError::ChannelCountOutOfRange(config.channels));
        }
        let kernel = SincKernel::new(config.input_rate, config.output_rate)?;
        let half = kernel.half_width();
        let minimum_target = half + 1;
        if config.target_fill_samples < minimum_target {
            return Err(SourceSyncError::TargetFillTooSmall {
                target: config.target_fill_samples,
                minimum: minimum_target,
            });
        }
        let minimum_capacity = config.target_fill_samples.saturating_mul(2);
        if !(minimum_capacity..=MAX_SOURCE_SYNC_CAPACITY_SAMPLES).contains(&config.capacity_samples)
        {
            return Err(SourceSyncError::CapacityOutOfRange {
                capacity: config.capacity_samples,
                minimum: minimum_capacity,
            });
        }
        if config.discontinuity.declick_samples > MAX_DECLICK_SAMPLES {
            return Err(SourceSyncError::DeclickTooLong(
                config.discontinuity.declick_samples,
            ));
        }
        let buffer_len = half - 1 + config.capacity_samples;
        let planes = (0..config.channels)
            .map(|_| vec![0.0; buffer_len])
            .collect();
        let denominator = u64::from(config.output_rate.hertz()) * PPB_SCALE;
        let step = u64::from(config.input_rate.hertz()) * PPB_SCALE;
        let telemetry = SourceSyncTelemetry {
            state: SourceSyncState::Priming,
            input_rate_hz: config.input_rate.hertz(),
            output_rate_hz: config.output_rate.hertz(),
            estimated_drift_ppm: 0.0,
            applied_correction_ppm: 0.0,
            buffer_fill_samples: 0.0,
            smoothed_fill_samples: None,
            target_fill_samples: config.target_fill_samples,
            capacity_samples: config.capacity_samples,
            peak_fill_samples: 0,
            pushed_samples: 0,
            concealed_gap_samples: 0,
            trimmed_overlap_samples: 0,
            priming_trimmed_samples: 0,
            consumed_input_samples: 0,
            rendered_output_samples: 0,
            silent_output_samples: 0,
            underruns: 0,
            underrun_samples: 0,
            overruns: 0,
            overrun_dropped_samples: 0,
            gaps_filled: 0,
            overlaps_trimmed: 0,
            discontinuities: 0,
            splices: 0,
            resets: 0,
        };
        Ok(Self {
            controller: DriftController::new(
                config.drift_control,
                config.input_rate,
                usize_to_f64(config.target_fill_samples),
            ),
            coefficients: vec![0.0; kernel.taps()],
            config,
            kernel,
            planes,
            len: half - 1,
            read_index: half - 1,
            frac: 0,
            denominator,
            step,
            state: SourceSyncState::Priming,
            fade_in_done: 0,
            expected_next: None,
            telemetry,
        })
    }

    #[must_use]
    pub const fn config(&self) -> &SourceSyncConfig {
        &self.config
    }

    #[must_use]
    pub const fn kernel(&self) -> &SincKernel {
        &self.kernel
    }

    #[must_use]
    pub const fn state(&self) -> SourceSyncState {
        self.state
    }

    /// Unread input samples ahead of the fractional read position.
    #[must_use]
    pub fn buffer_fill(&self) -> f64 {
        usize_to_f64(self.len - self.read_index)
            - u128_to_f64(u128::from(self.frac)) / u128_to_f64(u128::from(self.denominator))
    }

    /// Returns a telemetry snapshot. Never allocates.
    #[must_use]
    pub fn telemetry(&self) -> SourceSyncTelemetry {
        let mut telemetry = self.telemetry;
        telemetry.state = self.state;
        telemetry.estimated_drift_ppm = self.controller.estimated_drift_ppm();
        telemetry.applied_correction_ppm = self.controller.applied_correction_ppm();
        telemetry.buffer_fill_samples = self.buffer_fill();
        telemetry.smoothed_fill_samples = self.controller.smoothed_fill();
        telemetry
    }

    /// Clears buffered audio and timestamps and re-primes; the drift estimate
    /// is kept because the source clock is unchanged. Counted as a reset.
    pub fn reset(&mut self) {
        self.hard_reset();
        self.expected_next = None;
        self.telemetry.resets = self.telemetry.resets.saturating_add(1);
    }

    /// Forgets the drift estimate, for example after a device change.
    pub fn reset_drift_estimate(&mut self) {
        self.controller.reset();
        self.step = self.step_for(0);
    }

    /// Pushes a block whose first sample has source index `first_sample`.
    ///
    /// # Errors
    ///
    /// Returns a typed shape, size, finiteness, or overflow error; on error no
    /// state changes.
    pub fn push(
        &mut self,
        first_sample: u64,
        planes: &[&[f32]],
    ) -> Result<PushReport, SourceSyncError> {
        self.push_inner(first_sample, planes, false)
    }

    /// Pushes a block the source flagged as discontinuous; it is always
    /// handled by the policy's [`JumpAction`].
    ///
    /// # Errors
    ///
    /// As [`Self::push`].
    pub fn push_discontinuity(
        &mut self,
        first_sample: u64,
        planes: &[&[f32]],
    ) -> Result<PushReport, SourceSyncError> {
        self.push_inner(first_sample, planes, true)
    }

    /// Renders `output[channel].len()` engine-rate samples into every plane.
    ///
    /// # Errors
    ///
    /// Returns a typed output-shape or size error; on error no state changes.
    pub fn pull(&mut self, output: &mut [&mut [f32]]) -> Result<PullReport, SourceSyncError> {
        let requested = validate_output(output, self.config.channels)?;
        if requested == 0 {
            return Ok(PullReport {
                rendered_samples: 0,
                silent_samples: 0,
                underrun: false,
            });
        }
        if self.state != SourceSyncState::Running {
            self.trim_to_target();
            let ready = self.buffer_fill() >= usize_to_f64(self.config.target_fill_samples)
                && self.renderable(requested) == requested;
            if !ready {
                for plane in output.iter_mut() {
                    plane.fill(0.0);
                }
                if self.state == SourceSyncState::Recovering {
                    self.telemetry.underrun_samples = self
                        .telemetry
                        .underrun_samples
                        .saturating_add(as_u64(requested));
                }
                self.telemetry.silent_output_samples = self
                    .telemetry
                    .silent_output_samples
                    .saturating_add(as_u64(requested));
                return Ok(PullReport {
                    rendered_samples: 0,
                    silent_samples: requested,
                    underrun: false,
                });
            }
            self.state = SourceSyncState::Running;
            self.fade_in_done = 0;
            self.controller.reseed_fill();
        }

        // The loop regulates the fill seen by each request, before rendering,
        // which is also the quantity the priming threshold uses.
        let fill_at_request = self.buffer_fill();
        let renderable = self.renderable(requested);
        let consumed_before = self.read_index;
        self.render(output, renderable, renderable < requested);
        for plane in output.iter_mut() {
            plane[renderable..].fill(0.0);
        }
        self.telemetry.consumed_input_samples = self
            .telemetry
            .consumed_input_samples
            .saturating_add(as_u64(self.read_index - consumed_before));
        self.telemetry.rendered_output_samples = self
            .telemetry
            .rendered_output_samples
            .saturating_add(as_u64(renderable));
        let silent = requested - renderable;
        let underrun = silent > 0;
        if underrun {
            self.state = SourceSyncState::Recovering;
            self.telemetry.underruns = self.telemetry.underruns.saturating_add(1);
            self.telemetry.underrun_samples = self
                .telemetry
                .underrun_samples
                .saturating_add(as_u64(silent));
            self.telemetry.silent_output_samples = self
                .telemetry
                .silent_output_samples
                .saturating_add(as_u64(silent));
        } else {
            let elapsed = usize_to_f64(requested) / f64::from(self.config.output_rate.hertz());
            let ppb = self.controller.update(fill_at_request, elapsed);
            self.step = self.step_for(ppb);
        }
        Ok(PullReport {
            rendered_samples: renderable,
            silent_samples: silent,
            underrun,
        })
    }

    /// Converts `renderable` samples into the head of each output plane,
    /// applying any pending fade-in and, if `fade_out`, a closing fade-out.
    fn render(&mut self, output: &mut [&mut [f32]], renderable: usize, fade_out: bool) {
        let declick = self.config.discontinuity.declick_samples;
        let fade_out = if fade_out { declick.min(renderable) } else { 0 };
        for index in 0..renderable {
            let mut gain = 1.0_f32;
            if self.fade_in_done < declick {
                self.fade_in_done += 1;
                gain = ramp_gain(self.fade_in_done, declick);
            }
            if index >= renderable - fade_out {
                gain *= ramp_gain(renderable - index, fade_out);
            }
            self.kernel
                .coefficients_into(self.frac, self.denominator, &mut self.coefficients);
            let base = self.read_index + 1 - self.kernel.half_width();
            for (channel, plane) in output.iter_mut().enumerate() {
                let history = &self.planes[channel][base..base + self.coefficients.len()];
                let mut sum = 0.0_f64;
                for (coefficient, sample) in self.coefficients.iter().zip(history) {
                    sum += f64::from(*coefficient) * f64::from(*sample);
                }
                plane[index] = f64_to_f32(sum) * gain;
            }
            let advanced = self.frac + self.step;
            self.read_index += usize::try_from(advanced / self.denominator).unwrap_or(usize::MAX);
            self.frac = advanced % self.denominator;
        }
    }

    fn step_for(&self, correction_ppb: i64) -> u64 {
        let scale = PPB_SCALE.saturating_add_signed(correction_ppb);
        u64::from(self.config.input_rate.hertz()) * scale
    }

    /// Output samples (up to `requested`) whose kernel window is fully buffered.
    fn renderable(&self, requested: usize) -> usize {
        let half = self.kernel.half_width();
        if self.len < self.read_index + half + 1 {
            return 0;
        }
        let room = self.len - 1 - half - self.read_index;
        let limit = (as_u128(room) + 1) * u128::from(self.denominator) - u128::from(self.frac);
        let step = u128::from(self.step);
        let count = limit.div_ceil(step);
        usize::try_from(count).map_or(requested, |count| count.min(requested))
    }

    /// While silent, discards whole input samples above the target so playback
    /// starts at the target latency regardless of delivery granularity.
    fn trim_to_target(&mut self) {
        let ahead = self.len - self.read_index;
        let partial = usize::from(self.frac > 0);
        let excess = ahead
            .saturating_sub(self.config.target_fill_samples)
            .saturating_sub(partial);
        if excess > 0 {
            self.read_index += excess;
            self.telemetry.priming_trimmed_samples = self
                .telemetry
                .priming_trimmed_samples
                .saturating_add(as_u64(excess));
        }
    }

    fn hard_reset(&mut self) {
        let history = self.kernel.half_width() - 1;
        for plane in &mut self.planes {
            plane[..history].fill(0.0);
        }
        self.len = history;
        self.read_index = history;
        self.frac = 0;
        self.state = SourceSyncState::Priming;
        self.fade_in_done = 0;
        self.controller.reseed_fill();
    }

    /// Drops already-consumed history beyond the kernel's look-behind.
    fn compact(&mut self) {
        let keep_from = self.read_index + 1 - self.kernel.half_width();
        if keep_from == 0 {
            return;
        }
        for plane in &mut self.planes {
            plane.copy_within(keep_from..self.len, 0);
        }
        self.len -= keep_from;
        self.read_index -= keep_from;
    }

    /// First buffer index not yet inside any rendered kernel window.
    fn protected_end(&self) -> usize {
        self.len.min(self.read_index + self.kernel.half_width() + 1)
    }

    fn fade_out_tail(&mut self) {
        let start = self.protected_end().max(
            self.len
                .saturating_sub(self.config.discontinuity.declick_samples),
        );
        let count = self.len - start;
        for plane in &mut self.planes {
            for (offset, sample) in plane[start..self.len].iter_mut().enumerate() {
                *sample *= ramp_gain(count - offset, count);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn push_inner(
        &mut self,
        first_sample: u64,
        input: &[&[f32]],
        flagged: bool,
    ) -> Result<PushReport, SourceSyncError> {
        let incoming = validate_input(input, self.config.channels)?;
        let next_expected = first_sample
            .checked_add(as_u64(incoming))
            .ok_or(SourceSyncError::TimestampOverflow)?;
        if incoming == 0 {
            return Ok(PushReport {
                continuity: PushContinuity::Contiguous,
                accepted_samples: 0,
                overrun_dropped_samples: 0,
            });
        }
        let policy = self.config.discontinuity;
        let mut gap = 0_usize;
        let mut skip = 0_usize;
        let mut edge = false;
        let continuity = match self.expected_next {
            None => PushContinuity::Anchored,
            Some(_) if flagged => self.jump(&mut gap, &mut edge, incoming),
            Some(expected) if first_sample == expected => PushContinuity::Contiguous,
            Some(expected)
                if first_sample > expected
                    && first_sample - expected <= as_u64(policy.max_gap_fill_samples) =>
            {
                let samples = first_sample - expected;
                gap = usize::try_from(samples).unwrap_or(usize::MAX);
                edge = true;
                self.telemetry.gaps_filled = self.telemetry.gaps_filled.saturating_add(1);
                self.telemetry.concealed_gap_samples =
                    self.telemetry.concealed_gap_samples.saturating_add(samples);
                PushContinuity::GapFilled { samples }
            }
            Some(expected)
                if first_sample < expected
                    && expected - first_sample <= as_u64(policy.max_overlap_trim_samples) =>
            {
                let overlap = usize::try_from(expected - first_sample).unwrap_or(usize::MAX);
                skip = overlap.min(incoming);
                self.telemetry.overlaps_trimmed = self.telemetry.overlaps_trimmed.saturating_add(1);
                self.telemetry.trimmed_overlap_samples = self
                    .telemetry
                    .trimmed_overlap_samples
                    .saturating_add(as_u64(skip));
                if skip == incoming {
                    return Ok(PushReport {
                        continuity: PushContinuity::OverlapTrimmed {
                            samples: as_u64(skip),
                        },
                        accepted_samples: 0,
                        overrun_dropped_samples: 0,
                    });
                }
                PushContinuity::OverlapTrimmed {
                    samples: as_u64(skip),
                }
            }
            Some(_) => self.jump(&mut gap, &mut edge, incoming),
        };
        self.expected_next = Some(next_expected);
        self.compact();
        // Concealment beyond the capacity is always dropped by the overrun
        // below; cut it here so the fill arithmetic cannot overflow.
        let concealment_cut = gap.saturating_sub(self.config.capacity_samples);
        gap -= concealment_cut;

        // Overrun: drop audio so the fill returns to the target.
        let mut dropped = 0;
        let ahead = self.len - self.read_index;
        let appended = gap + (incoming - skip);
        if ahead + appended > self.config.capacity_samples {
            let excess = ahead + appended - self.config.target_fill_samples;
            // Keep one declick ramp beyond the kernel lookahead so the cut
            // can fade out rather than step.
            let keep = self.len.min(self.protected_end() + policy.declick_samples);
            let removable = self.len - keep;
            let from_buffer = excess.min(removable);
            self.len -= from_buffer;
            let mut rest = excess - from_buffer;
            let from_gap = rest.min(gap);
            gap -= from_gap;
            rest -= from_gap;
            let from_incoming = rest.min(incoming - skip);
            skip += from_incoming;
            dropped = concealment_cut.saturating_add(from_buffer + from_gap + from_incoming);
            edge = true;
            self.controller.reseed_fill();
            self.telemetry.overruns = self.telemetry.overruns.saturating_add(1);
            self.telemetry.overrun_dropped_samples = self
                .telemetry
                .overrun_dropped_samples
                .saturating_add(as_u64(dropped));
        }

        if edge {
            self.fade_out_tail();
        }
        let declick = policy.declick_samples;
        let accepted = incoming - skip;
        for (plane, source) in self.planes.iter_mut().zip(input) {
            plane[self.len..self.len + gap].fill(0.0);
            let destination = &mut plane[self.len + gap..self.len + gap + accepted];
            destination.copy_from_slice(&source[skip..]);
            if edge {
                let ramp = declick.min(accepted);
                for (offset, sample) in destination[..ramp].iter_mut().enumerate() {
                    *sample *= ramp_gain(offset + 1, declick);
                }
            }
        }
        self.len += gap + accepted;
        self.telemetry.pushed_samples = self
            .telemetry
            .pushed_samples
            .saturating_add(as_u64(accepted));
        self.telemetry.peak_fill_samples = self
            .telemetry
            .peak_fill_samples
            .max(self.len - self.read_index);
        Ok(PushReport {
            continuity,
            accepted_samples: accepted,
            overrun_dropped_samples: dropped,
        })
    }

    /// Applies the policy's jump action and counts the discontinuity.
    fn jump(&mut self, gap: &mut usize, edge: &mut bool, incoming: usize) -> PushContinuity {
        self.telemetry.discontinuities = self.telemetry.discontinuities.saturating_add(1);
        match self.config.discontinuity.jump_action {
            JumpAction::Splice => {
                *edge = true;
                self.telemetry.splices = self.telemetry.splices.saturating_add(1);
                self.controller.reseed_fill();
                PushContinuity::Spliced
            }
            JumpAction::Flush => {
                self.telemetry.resets = self.telemetry.resets.saturating_add(1);
                if self.state == SourceSyncState::Running {
                    self.compact();
                    let keep = self
                        .len
                        .min(self.protected_end() + self.config.discontinuity.declick_samples);
                    self.len = keep;
                    self.fade_out_tail();
                    let ahead = self.len - self.read_index;
                    *gap = self
                        .config
                        .target_fill_samples
                        .saturating_sub(ahead + incoming);
                    *edge = true;
                    self.controller.reseed_fill();
                } else {
                    self.hard_reset();
                }
                PushContinuity::Flushed
            }
        }
    }
}

fn validate_input(input: &[&[f32]], channels: usize) -> Result<usize, SourceSyncError> {
    if input.len() != channels {
        return Err(SourceSyncError::PlaneCountMismatch {
            expected: channels,
            actual: input.len(),
        });
    }
    let samples = input.first().map_or(0, |plane| plane.len());
    if samples > MAX_SOURCE_SYNC_PUSH_SAMPLES {
        return Err(SourceSyncError::PushTooLarge(samples));
    }
    for (channel, plane) in input.iter().enumerate() {
        if plane.len() != samples {
            return Err(SourceSyncError::PlaneLengthMismatch {
                plane: channel,
                expected: samples,
                actual: plane.len(),
            });
        }
        if let Some(sample) = plane.iter().position(|value| !value.is_finite()) {
            return Err(SourceSyncError::NonFiniteSample { channel, sample });
        }
    }
    Ok(samples)
}

fn validate_output(output: &[&mut [f32]], channels: usize) -> Result<usize, SourceSyncError> {
    if output.len() != channels {
        return Err(SourceSyncError::PlaneCountMismatch {
            expected: channels,
            actual: output.len(),
        });
    }
    let samples = output.first().map_or(0, |plane| plane.len());
    if samples > MAX_SOURCE_SYNC_PULL_SAMPLES {
        return Err(SourceSyncError::PullTooLarge(samples));
    }
    for (channel, plane) in output.iter().enumerate() {
        if plane.len() != samples {
            return Err(SourceSyncError::PlaneLengthMismatch {
                plane: channel,
                expected: samples,
                actual: plane.len(),
            });
        }
    }
    Ok(samples)
}

/// Linear ramp gain `position / (length + 1)` for `position` in `1..=length`.
fn ramp_gain(position: usize, length: usize) -> f32 {
    f64_to_f32(usize_to_f64(position) / usize_to_f64(length + 1))
}

const fn as_u64(value: usize) -> u64 {
    value as u64
}

const fn as_u128(value: usize) -> u128 {
    value as u128
}

#[allow(clippy::cast_possible_truncation)]
fn round_to_i64(value: f64) -> i64 {
    value.round() as i64
}
