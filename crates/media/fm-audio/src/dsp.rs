//! Bounded, deterministic native DSP for one audio channel strip.
//!
//! This module implements the AU-005 processing primitives as reference code:
//!
//! - [`Polarity`] performs exact sign inversion.
//! - [`Biquad`] and [`ParametricEq`] implement the RBJ audio-EQ cookbook
//!   filters with `f64` coefficients and transposed direct form II state.
//! - [`Gate`], [`Expander`], [`Compressor`], and [`Limiter`] implement
//!   dynamics with `f64` detectors and `f32` input/output.
//! - [`DspMeter`] accumulates sample peak and RMS over a caller-selected
//!   window.
//!
//! [`StripDsp`] composes the fixed chain
//! `polarity -> high-pass -> EQ bands -> gate -> compressor -> limiter` with
//! one independent state per channel. Constructors and setters validate every
//! argument before mutating state, all per-channel storage is allocated by
//! [`StripDsp::new`], and steady-state processing never allocates or panics.

use core::fmt;

use crate::{AudioBlock, MAX_CHANNELS, MAX_SAMPLES_PER_BLOCK};

/// Maximum number of peaking bands in one [`ParametricEq`] or [`StripDsp`].
pub const MAX_EQ_BANDS: usize = 4;

/// Level floor used by logarithmic dynamics detectors, in linear magnitude.
const ENVELOPE_FLOOR: f64 = 1.0e-12;

const MIN_COMPRESSOR_THRESHOLD_DB: f32 = -60.0;
const MAX_COMPRESSOR_THRESHOLD_DB: f32 = 0.0;
const MIN_COMPRESSOR_RATIO: f32 = 1.0;
const MAX_COMPRESSOR_RATIO: f32 = 20.0;
const MIN_COMPRESSOR_KNEE_DB: f32 = 0.0;
const MAX_COMPRESSOR_KNEE_DB: f32 = 24.0;
const MIN_COMPRESSOR_MAKEUP_GAIN_DB: f32 = 0.0;
const MAX_COMPRESSOR_MAKEUP_GAIN_DB: f32 = 24.0;

const MIN_GATE_THRESHOLD_DB: f32 = -80.0;
const MAX_GATE_THRESHOLD_DB: f32 = 0.0;
const MIN_GATE_RANGE_DB: f32 = 0.0;
const MAX_GATE_RANGE_DB: f32 = 80.0;
const MIN_GATE_HOLD_MS: f32 = 0.0;
const MAX_GATE_HOLD_MS: f32 = 1_000.0;
const MIN_GATE_RELEASE_MS: f32 = 0.1;
const MAX_GATE_RELEASE_MS: f32 = 5_000.0;

const MIN_EXPANDER_THRESHOLD_DB: f32 = -80.0;
const MAX_EXPANDER_THRESHOLD_DB: f32 = 0.0;
const MIN_EXPANDER_RATIO: f32 = 1.0;
const MAX_EXPANDER_RATIO: f32 = 20.0;
const MIN_EXPANDER_RANGE_DB: f32 = 0.0;
const MAX_EXPANDER_RANGE_DB: f32 = 80.0;

const MIN_LIMITER_CEILING_DB: f32 = -60.0;
const MAX_LIMITER_CEILING_DB: f32 = 0.0;

/// Attack-time bound shared by the gate and compressor, in milliseconds.
const MIN_ATTACK_MS: f32 = 0.1;
const MAX_ATTACK_MS: f32 = 1_000.0;
/// Release-time bound shared by the compressor and limiter, in milliseconds.
const MIN_RELEASE_MS: f32 = 0.1;
const MAX_RELEASE_MS: f32 = 1_000.0;

/// Errors returned while configuring or running native DSP.
///
/// Every message is bounded: variants carry only fixed-size numeric values.
#[derive(Clone, Debug, PartialEq)]
pub enum DspError {
    InvalidSampleRate(f32),
    InvalidFrequency(f32),
    InvalidQ(f32),
    InvalidEqGainDb(f32),
    InvalidThresholdDb(f32),
    InvalidRatio(f32),
    InvalidAttackMs(f32),
    InvalidReleaseMs(f32),
    InvalidHoldMs(f32),
    InvalidKneeDb(f32),
    InvalidMakeupGainDb(f32),
    InvalidRangeDb(f32),
    InvalidCeilingDb(f32),
    CoefficientOverflow,
    ChannelCountOutOfRange { actual: usize, maximum: usize },
    ChannelCountMismatch { expected: usize, actual: usize },
    SampleRateMismatch { expected_hz: f32, actual_hz: u32 },
    ChannelIndexOutOfRange { channel: usize, channels: usize },
    BandIndexOutOfRange { band: usize, bands: usize },
    SampleCountOutOfRange { actual: usize, maximum: usize },
    NonFiniteSample { channel: usize, sample: usize },
}

impl fmt::Display for DspError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate(sample_rate_hz) => {
                write!(formatter, "invalid DSP sample rate: {sample_rate_hz}")
            }
            Self::InvalidFrequency(frequency_hz) => {
                write!(formatter, "invalid DSP frequency: {frequency_hz} Hz")
            }
            Self::InvalidQ(q) => write!(formatter, "invalid DSP quality factor: {q}"),
            Self::InvalidEqGainDb(gain_db) => {
                write!(formatter, "invalid EQ gain: {gain_db} dB")
            }
            Self::InvalidThresholdDb(threshold_db) => {
                write!(formatter, "invalid dynamics threshold: {threshold_db} dB")
            }
            Self::InvalidRatio(ratio) => write!(formatter, "invalid dynamics ratio: {ratio}"),
            Self::InvalidAttackMs(attack_ms) => {
                write!(formatter, "invalid attack time: {attack_ms} ms")
            }
            Self::InvalidReleaseMs(release_ms) => {
                write!(formatter, "invalid release time: {release_ms} ms")
            }
            Self::InvalidHoldMs(hold_ms) => write!(formatter, "invalid hold time: {hold_ms} ms"),
            Self::InvalidKneeDb(knee_db) => {
                write!(formatter, "invalid compressor knee: {knee_db} dB")
            }
            Self::InvalidMakeupGainDb(makeup_gain_db) => {
                write!(formatter, "invalid makeup gain: {makeup_gain_db} dB")
            }
            Self::InvalidRangeDb(range_db) => {
                write!(formatter, "invalid gate or expander range: {range_db} dB")
            }
            Self::InvalidCeilingDb(ceiling_db) => {
                write!(formatter, "invalid limiter ceiling: {ceiling_db} dB")
            }
            Self::CoefficientOverflow => {
                formatter.write_str("DSP filter coefficients are not finite")
            }
            Self::ChannelCountOutOfRange { actual, maximum } => {
                write!(
                    formatter,
                    "DSP channel count {actual} is outside 1..={maximum}"
                )
            }
            Self::ChannelCountMismatch { expected, actual } => {
                write!(
                    formatter,
                    "audio block has {actual} channels; strip has {expected}"
                )
            }
            Self::SampleRateMismatch {
                expected_hz,
                actual_hz,
            } => write!(
                formatter,
                "audio block sample rate {actual_hz} Hz does not match the configured {expected_hz} Hz"
            ),
            Self::ChannelIndexOutOfRange { channel, channels } => write!(
                formatter,
                "DSP channel index {channel} is outside {channels} channels"
            ),
            Self::BandIndexOutOfRange { band, bands } => {
                write!(formatter, "EQ band index {band} is outside {bands} bands")
            }
            Self::SampleCountOutOfRange { actual, maximum } => write!(
                formatter,
                "DSP block has {actual} samples per channel; maximum is {maximum}"
            ),
            Self::NonFiniteSample { channel, sample } => write!(
                formatter,
                "DSP input sample {sample} in channel {channel} is not finite"
            ),
        }
    }
}

impl std::error::Error for DspError {}

/// Exact sample polarity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Polarity {
    /// Passes samples through unchanged.
    #[default]
    Normal,
    /// Negates samples exactly.
    Inverted,
}

impl Polarity {
    /// Applies the polarity to one sample.
    ///
    /// Inverted polarity returns the exact negation, so a positive zero
    /// becomes a negative zero (which compares equal to zero) and a negative
    /// zero becomes a positive zero.
    #[must_use]
    pub const fn apply(self, sample: f32) -> f32 {
        match self {
            Self::Normal => sample,
            Self::Inverted => -sample,
        }
    }
}

/// RBJ audio-EQ cookbook filter type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BiquadKind {
    LowPass,
    HighPass,
    PeakingEq,
    LowShelf,
    HighShelf,
}

/// Validated, normalized second-order section coefficients.
///
/// Coefficients are computed in `f64` at construction and stored with `a0`
/// normalized to one:
///
/// ```text
/// H(z) = (b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2)
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Default for BiquadCoefficients {
    fn default() -> Self {
        Self::identity()
    }
}

impl BiquadCoefficients {
    /// The coefficients `fraction` (0 to 1) of the way from `self` to
    /// `other`. Stable biquads form a convex set (the stability triangle of
    /// `a1` and `a2`), so a blend of two stable sections is stable.
    #[must_use]
    pub(crate) fn blend(self, other: Self, fraction: f64) -> Self {
        let mix = |from: f64, to: f64| from + (to - from) * fraction;
        Self {
            b0: mix(self.b0, other.b0),
            b1: mix(self.b1, other.b1),
            b2: mix(self.b2, other.b2),
            a1: mix(self.a1, other.a1),
            a2: mix(self.a2, other.a2),
        }
    }

    /// Unity passthrough coefficients.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// Computes RBJ audio-EQ cookbook coefficients with `f64` math.
    ///
    /// The intermediate values follow the cookbook exactly for a normalized
    /// angular frequency `omega = 2 * pi * frequency_hz / sample_rate_hz`:
    ///
    /// ```text
    /// alpha       = sin(omega) / (2 * q)
    /// low  pass   b0 = (1 - cos(omega)) / 2, b1 = 1 - cos(omega), b2 = b0
    /// high pass   b0 = (1 + cos(omega)) / 2, b1 = -(1 + cos(omega)), b2 = b0
    /// peaking     A = 10^(gain_db / 40), alpha applied as +/- alpha * A
    /// shelves     A = 10^(gain_db / 40), alpha = sin(omega) / (2 * q)
    /// ```
    ///
    /// Every section shares the denominator `a0 = 1 + alpha`,
    /// `a1 = -2 * cos(omega)`, `a2 = 1 - alpha` except the gain sections,
    /// which use the cookbook's `A`-dependent terms. Shelf sections use the
    /// documented Q-form (`alpha = sin(omega) / (2 * q)`) rather than the
    /// shelf-slope form.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidSampleRate`] unless the sample rate is finite
    /// and greater than zero, [`DspError::InvalidFrequency`] unless the
    /// frequency is finite, greater than zero, and strictly below the Nyquist
    /// frequency, [`DspError::InvalidQ`] unless `q` is finite and greater than
    /// zero, [`DspError::InvalidEqGainDb`] unless `gain_db` is finite and
    /// representable, and [`DspError::CoefficientOverflow`] if the normalized
    /// coefficients are not finite.
    pub fn new(
        kind: BiquadKind,
        sample_rate_hz: f32,
        frequency_hz: f32,
        q: f32,
        gain_db: f32,
    ) -> Result<Self, DspError> {
        let sample_rate = validate_sample_rate(sample_rate_hz)?;
        let frequency = validate_frequency(frequency_hz, sample_rate)?;
        let q = validate_q(q)?;
        if !gain_db.is_finite() {
            return Err(DspError::InvalidEqGainDb(gain_db));
        }
        let amplitude = match kind {
            BiquadKind::LowPass | BiquadKind::HighPass => 1.0,
            BiquadKind::PeakingEq | BiquadKind::LowShelf | BiquadKind::HighShelf => {
                eq_amplitude(gain_db)?
            }
        };
        let omega = std::f64::consts::TAU * frequency / sample_rate;
        let cos_omega = omega.cos();
        let sin_omega = omega.sin();
        let alpha = sin_omega / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::LowPass => (
                (1.0 - cos_omega) / 2.0,
                1.0 - cos_omega,
                (1.0 - cos_omega) / 2.0,
                1.0 + alpha,
                -2.0 * cos_omega,
                1.0 - alpha,
            ),
            BiquadKind::HighPass => (
                f64::midpoint(1.0, cos_omega),
                -(1.0 + cos_omega),
                f64::midpoint(1.0, cos_omega),
                1.0 + alpha,
                -2.0 * cos_omega,
                1.0 - alpha,
            ),
            BiquadKind::PeakingEq => (
                1.0 + alpha * amplitude,
                -2.0 * cos_omega,
                1.0 - alpha * amplitude,
                1.0 + alpha / amplitude,
                -2.0 * cos_omega,
                1.0 - alpha / amplitude,
            ),
            BiquadKind::LowShelf | BiquadKind::HighShelf => {
                let sqrt_amplitude = amplitude.sqrt();
                let shelf = 2.0 * sqrt_amplitude * alpha;
                let plus = (amplitude + 1.0) + (amplitude - 1.0) * cos_omega;
                let minus = (amplitude + 1.0) - (amplitude - 1.0) * cos_omega;
                if kind == BiquadKind::LowShelf {
                    (
                        amplitude * (minus + shelf),
                        2.0 * amplitude * ((amplitude - 1.0) - (amplitude + 1.0) * cos_omega),
                        amplitude * (minus - shelf),
                        plus + shelf,
                        -2.0 * ((amplitude - 1.0) + (amplitude + 1.0) * cos_omega),
                        plus - shelf,
                    )
                } else {
                    (
                        amplitude * (plus + shelf),
                        -2.0 * amplitude * ((amplitude - 1.0) + (amplitude + 1.0) * cos_omega),
                        amplitude * (plus - shelf),
                        minus + shelf,
                        2.0 * ((amplitude - 1.0) - (amplitude + 1.0) * cos_omega),
                        minus - shelf,
                    )
                }
            }
        };
        if !a0.is_finite() || a0 <= 0.0 {
            return Err(DspError::CoefficientOverflow);
        }
        let coefficients = Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        };
        if !coefficients.is_finite() {
            return Err(DspError::CoefficientOverflow);
        }
        Ok(coefficients)
    }

    /// Computes RBJ low-pass coefficients.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    pub fn low_pass(sample_rate_hz: f32, frequency_hz: f32, q: f32) -> Result<Self, DspError> {
        Self::new(BiquadKind::LowPass, sample_rate_hz, frequency_hz, q, 0.0)
    }

    /// Computes RBJ high-pass coefficients.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    pub fn high_pass(sample_rate_hz: f32, frequency_hz: f32, q: f32) -> Result<Self, DspError> {
        Self::new(BiquadKind::HighPass, sample_rate_hz, frequency_hz, q, 0.0)
    }

    /// Computes RBJ peaking-EQ coefficients.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    pub fn peaking_eq(
        sample_rate_hz: f32,
        frequency_hz: f32,
        q: f32,
        gain_db: f32,
    ) -> Result<Self, DspError> {
        Self::new(
            BiquadKind::PeakingEq,
            sample_rate_hz,
            frequency_hz,
            q,
            gain_db,
        )
    }

    /// Computes RBJ low-shelf coefficients using the cookbook Q-form.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    pub fn low_shelf(
        sample_rate_hz: f32,
        frequency_hz: f32,
        q: f32,
        gain_db: f32,
    ) -> Result<Self, DspError> {
        Self::new(
            BiquadKind::LowShelf,
            sample_rate_hz,
            frequency_hz,
            q,
            gain_db,
        )
    }

    /// Computes RBJ high-shelf coefficients using the cookbook Q-form.
    ///
    /// # Errors
    ///
    /// See [`Self::new`].
    pub fn high_shelf(
        sample_rate_hz: f32,
        frequency_hz: f32,
        q: f32,
        gain_db: f32,
    ) -> Result<Self, DspError> {
        Self::new(
            BiquadKind::HighShelf,
            sample_rate_hz,
            frequency_hz,
            q,
            gain_db,
        )
    }

    #[must_use]
    pub const fn b0(&self) -> f64 {
        self.b0
    }

    #[must_use]
    pub const fn b1(&self) -> f64 {
        self.b1
    }

    #[must_use]
    pub const fn b2(&self) -> f64 {
        self.b2
    }

    #[must_use]
    pub const fn a1(&self) -> f64 {
        self.a1
    }

    #[must_use]
    pub const fn a2(&self) -> f64 {
        self.a2
    }

    fn is_finite(&self) -> bool {
        self.b0.is_finite()
            && self.b1.is_finite()
            && self.b2.is_finite()
            && self.a1.is_finite()
            && self.a2.is_finite()
    }
}

/// Settings of one configured biquad section.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadSettings {
    pub kind: BiquadKind,
    pub frequency_hz: f32,
    pub q: f32,
    pub gain_db: f32,
}

/// Second-order section with transposed direct form II state.
///
/// State is kept in `f64` and samples enter and leave as `f32`. Processing is
/// allocation-free and deterministic.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Biquad {
    coefficients: BiquadCoefficients,
    z1: f64,
    z2: f64,
}

impl Biquad {
    /// Creates a section with cleared state.
    #[must_use]
    pub const fn new(coefficients: BiquadCoefficients) -> Self {
        Self {
            coefficients,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[must_use]
    pub const fn coefficients(&self) -> BiquadCoefficients {
        self.coefficients
    }

    /// Replaces the coefficients and clears the state.
    ///
    /// Changing coefficients always resets the section history, which keeps
    /// the response deterministic when a caller retunes a live filter.
    pub fn set_coefficients(&mut self, coefficients: BiquadCoefficients) {
        self.coefficients = coefficients;
        self.reset();
    }

    /// Replaces the coefficients and keeps the state.
    ///
    /// A live filter whose parameters glide in small steps is retuned this
    /// way so the response moves without the click a history reset causes.
    pub const fn retune(&mut self, coefficients: BiquadCoefficients) {
        self.coefficients = coefficients;
    }

    /// Clears the section history.
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Processes one sample with transposed direct form II.
    #[must_use]
    pub fn process(&mut self, sample: f32) -> f32 {
        let input = f64::from(sample);
        let output = self.coefficients.b0 * input + self.z1;
        self.z1 = self.coefficients.b1 * input - self.coefficients.a1 * output + self.z2;
        self.z2 = self.coefficients.b2 * input - self.coefficients.a2 * output;
        to_f32(output)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BiquadSection {
    settings: BiquadSettings,
    filter: Biquad,
}

/// One optional high-pass section plus up to [`MAX_EQ_BANDS`] peaking bands.
///
/// Bands are processed in index order after the high-pass section, so the
/// result is deterministic regardless of configuration history. Reconfiguring
/// a section resets only that section's filter state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParametricEq {
    sample_rate_hz: f32,
    high_pass: Option<BiquadSection>,
    bands: [Option<BiquadSection>; MAX_EQ_BANDS],
}

impl ParametricEq {
    /// Creates an empty EQ at a fixed sample rate.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidSampleRate`] unless the rate is finite and
    /// greater than zero.
    pub fn new(sample_rate_hz: f32) -> Result<Self, DspError> {
        validate_sample_rate(sample_rate_hz)?;
        Ok(Self {
            sample_rate_hz,
            high_pass: None,
            bands: [None; MAX_EQ_BANDS],
        })
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> f32 {
        self.sample_rate_hz
    }

    /// Installs a high-pass section at `frequency_hz` with quality `q`.
    ///
    /// # Errors
    ///
    /// See [`BiquadCoefficients::high_pass`].
    pub fn set_high_pass(&mut self, frequency_hz: f32, q: f32) -> Result<(), DspError> {
        let coefficients = BiquadCoefficients::high_pass(self.sample_rate_hz, frequency_hz, q)?;
        self.install_high_pass(
            BiquadSettings {
                kind: BiquadKind::HighPass,
                frequency_hz,
                q,
                gain_db: 0.0,
            },
            coefficients,
        );
        Ok(())
    }

    /// Removes the high-pass section.
    pub fn clear_high_pass(&mut self) {
        self.high_pass = None;
    }

    #[must_use]
    pub fn high_pass(&self) -> Option<BiquadSettings> {
        self.high_pass.map(|section| section.settings)
    }

    /// Installs or replaces one peaking band.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::BandIndexOutOfRange`] when `band` is at least
    /// [`MAX_EQ_BANDS`], and otherwise the errors documented by
    /// [`BiquadCoefficients::peaking_eq`]. The section is unchanged on error.
    pub fn set_band(
        &mut self,
        band: usize,
        frequency_hz: f32,
        q: f32,
        gain_db: f32,
    ) -> Result<(), DspError> {
        let coefficients =
            BiquadCoefficients::peaking_eq(self.sample_rate_hz, frequency_hz, q, gain_db)?;
        if band >= MAX_EQ_BANDS {
            return Err(DspError::BandIndexOutOfRange {
                band,
                bands: MAX_EQ_BANDS,
            });
        }
        self.install_band(
            band,
            BiquadSettings {
                kind: BiquadKind::PeakingEq,
                frequency_hz,
                q,
                gain_db,
            },
            coefficients,
        );
        Ok(())
    }

    /// Removes one peaking band.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::BandIndexOutOfRange`] when `band` is at least
    /// [`MAX_EQ_BANDS`]. The EQ is unchanged on error.
    pub fn clear_band(&mut self, band: usize) -> Result<(), DspError> {
        if band >= MAX_EQ_BANDS {
            return Err(DspError::BandIndexOutOfRange {
                band,
                bands: MAX_EQ_BANDS,
            });
        }
        self.remove_band(band);
        Ok(())
    }

    #[must_use]
    pub fn band(&self, band: usize) -> Option<BiquadSettings> {
        self.bands
            .get(band)
            .copied()
            .flatten()
            .map(|section| section.settings)
    }

    /// Clears every section's filter state.
    pub fn reset(&mut self) {
        if let Some(section) = &mut self.high_pass {
            section.filter.reset();
        }
        for section in self.bands.iter_mut().flatten() {
            section.filter.reset();
        }
    }

    /// Processes one sample through the high-pass section and all bands.
    #[must_use]
    pub fn process(&mut self, sample: f32) -> f32 {
        let mut output = sample;
        if let Some(section) = &mut self.high_pass {
            output = section.filter.process(output);
        }
        for section in self.bands.iter_mut().flatten() {
            output = section.filter.process(output);
        }
        output
    }

    fn install_high_pass(&mut self, settings: BiquadSettings, coefficients: BiquadCoefficients) {
        self.high_pass = Some(BiquadSection {
            settings,
            filter: Biquad::new(coefficients),
        });
    }

    fn install_band(
        &mut self,
        band: usize,
        settings: BiquadSettings,
        coefficients: BiquadCoefficients,
    ) {
        if let Some(section) = self.bands.get_mut(band) {
            *section = Some(BiquadSection {
                settings,
                filter: Biquad::new(coefficients),
            });
        }
    }

    fn remove_band(&mut self, band: usize) {
        if let Some(section) = self.bands.get_mut(band) {
            *section = None;
        }
    }
}

/// One peak/RMS measurement window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DspMeterReading {
    /// Largest absolute sample observed in the window.
    pub peak: f32,
    /// Root mean square of the samples observed in the window.
    pub rms: f32,
}

/// Caller-windowed peak and RMS accumulator.
///
/// The meter accumulates every sample added with [`Self::add_sample`] until
/// [`Self::reset`] clears the window. [`StripDsp`] accumulates post-chain
/// output samples into one meter per channel and resets them with
/// [`StripDsp::reset`], so callers select the measurement window.
#[derive(Clone, Copy, Debug, Default)]
pub struct DspMeter {
    samples: u64,
    peak: f32,
    squares: f64,
}

impl DspMeter {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            samples: 0,
            peak: 0.0,
            squares: 0.0,
        }
    }

    /// Clears the window.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Adds one sample to the window.
    pub fn add_sample(&mut self, sample: f32) {
        self.samples = self.samples.saturating_add(1);
        self.peak = self.peak.max(sample.abs());
        self.squares += f64::from(sample) * f64::from(sample);
    }

    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    #[must_use]
    pub const fn peak(&self) -> f32 {
        self.peak
    }

    #[must_use]
    pub fn rms(&self) -> f32 {
        if self.samples == 0 {
            return 0.0;
        }
        to_f32((self.squares / count_to_f64(self.samples)).sqrt())
    }

    #[must_use]
    pub fn reading(&self) -> DspMeterReading {
        DspMeterReading {
            peak: self.peak,
            rms: self.rms(),
        }
    }
}

/// Feed-forward peak compressor with a soft knee.
///
/// The detector is a one-pole peak follower over `|sample|`:
///
/// ```text
/// level       = |sample|
/// coefficient = 1 - exp(-1 / (time_ms / 1000 * sample_rate_hz))
/// envelope   += (level - envelope) * coefficient
/// ```
///
/// The attack coefficient applies while the level rises; the release
/// coefficient applies while it falls. The static gain computer converts the
/// envelope to `level_db = 20 * log10(max(envelope, 1e-12))` and interpolates
/// the reduction quadratically across a knee of `knee_db`:
///
/// ```text
/// over_db = level_db - threshold_db
/// slope   = 1 - 1 / ratio
/// reduction_db = 0                              over_db <= -knee_db / 2
///              = slope * over_db                over_db >=  knee_db / 2
///              = slope * (over_db + knee/2)^2   otherwise
///                          / (2 * knee_db)
/// ```
///
/// The makeup gain is applied after the reduction:
/// `output = sample * 10^((makeup_gain_db - reduction_db) / 20)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Compressor {
    sample_rate_hz: f32,
    threshold_db: f32,
    ratio: f32,
    attack_ms: f32,
    release_ms: f32,
    knee_db: f32,
    makeup_gain_db: f32,
    attack_coefficient: f64,
    release_coefficient: f64,
    envelope: f64,
    reduction_db: f64,
}

impl Compressor {
    /// Creates a compressor.
    ///
    /// Accepted bounds are:
    ///
    /// - `threshold_db` in `-60.0..=0.0`,
    /// - `ratio` in `1.0..=20.0`,
    /// - `attack_ms` and `release_ms` in `0.1..=1000.0`,
    /// - `knee_db` in `0.0..=24.0`,
    /// - `makeup_gain_db` in `0.0..=24.0`.
    ///
    /// # Errors
    ///
    /// Returns the matching [`DspError`] variant for any value outside its
    /// bound. Nothing is mutated on error.
    pub fn new(
        sample_rate_hz: f32,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        knee_db: f32,
        makeup_gain_db: f32,
    ) -> Result<Self, DspError> {
        let sample_rate = validate_sample_rate(sample_rate_hz)?;
        if !in_range(
            threshold_db,
            MIN_COMPRESSOR_THRESHOLD_DB,
            MAX_COMPRESSOR_THRESHOLD_DB,
        ) {
            return Err(DspError::InvalidThresholdDb(threshold_db));
        }
        if !in_range(ratio, MIN_COMPRESSOR_RATIO, MAX_COMPRESSOR_RATIO) {
            return Err(DspError::InvalidRatio(ratio));
        }
        if !in_range(attack_ms, MIN_ATTACK_MS, MAX_ATTACK_MS) {
            return Err(DspError::InvalidAttackMs(attack_ms));
        }
        if !in_range(release_ms, MIN_RELEASE_MS, MAX_RELEASE_MS) {
            return Err(DspError::InvalidReleaseMs(release_ms));
        }
        if !in_range(knee_db, MIN_COMPRESSOR_KNEE_DB, MAX_COMPRESSOR_KNEE_DB) {
            return Err(DspError::InvalidKneeDb(knee_db));
        }
        if !in_range(
            makeup_gain_db,
            MIN_COMPRESSOR_MAKEUP_GAIN_DB,
            MAX_COMPRESSOR_MAKEUP_GAIN_DB,
        ) {
            return Err(DspError::InvalidMakeupGainDb(makeup_gain_db));
        }
        Ok(Self {
            sample_rate_hz,
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            knee_db,
            makeup_gain_db,
            attack_coefficient: one_pole_coefficient(attack_ms, sample_rate),
            release_coefficient: one_pole_coefficient(release_ms, sample_rate),
            envelope: 0.0,
            reduction_db: 0.0,
        })
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> f32 {
        self.sample_rate_hz
    }

    #[must_use]
    pub const fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    #[must_use]
    pub const fn ratio(&self) -> f32 {
        self.ratio
    }

    #[must_use]
    pub const fn attack_ms(&self) -> f32 {
        self.attack_ms
    }

    #[must_use]
    pub const fn release_ms(&self) -> f32 {
        self.release_ms
    }

    #[must_use]
    pub const fn knee_db(&self) -> f32 {
        self.knee_db
    }

    #[must_use]
    pub const fn makeup_gain_db(&self) -> f32 {
        self.makeup_gain_db
    }

    /// Sets the threshold in dB.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidThresholdDb`] outside `-60.0..=0.0`.
    pub fn set_threshold_db(&mut self, threshold_db: f32) -> Result<(), DspError> {
        if !in_range(
            threshold_db,
            MIN_COMPRESSOR_THRESHOLD_DB,
            MAX_COMPRESSOR_THRESHOLD_DB,
        ) {
            return Err(DspError::InvalidThresholdDb(threshold_db));
        }
        self.threshold_db = threshold_db;
        Ok(())
    }

    /// Sets the compression ratio.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidRatio`] outside `1.0..=20.0`.
    pub fn set_ratio(&mut self, ratio: f32) -> Result<(), DspError> {
        if !in_range(ratio, MIN_COMPRESSOR_RATIO, MAX_COMPRESSOR_RATIO) {
            return Err(DspError::InvalidRatio(ratio));
        }
        self.ratio = ratio;
        Ok(())
    }

    /// Sets the attack time in milliseconds and recomputes its coefficient.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidAttackMs`] outside `0.1..=1000.0`.
    pub fn set_attack_ms(&mut self, attack_ms: f32) -> Result<(), DspError> {
        if !in_range(attack_ms, MIN_ATTACK_MS, MAX_ATTACK_MS) {
            return Err(DspError::InvalidAttackMs(attack_ms));
        }
        self.attack_ms = attack_ms;
        self.attack_coefficient = one_pole_coefficient(attack_ms, f64::from(self.sample_rate_hz));
        Ok(())
    }

    /// Sets the release time in milliseconds and recomputes its coefficient.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidReleaseMs`] outside `0.1..=1000.0`.
    pub fn set_release_ms(&mut self, release_ms: f32) -> Result<(), DspError> {
        if !in_range(release_ms, MIN_RELEASE_MS, MAX_RELEASE_MS) {
            return Err(DspError::InvalidReleaseMs(release_ms));
        }
        self.release_ms = release_ms;
        self.release_coefficient = one_pole_coefficient(release_ms, f64::from(self.sample_rate_hz));
        Ok(())
    }

    /// Sets the soft-knee width in dB.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidKneeDb`] outside `0.0..=24.0`.
    pub fn set_knee_db(&mut self, knee_db: f32) -> Result<(), DspError> {
        if !in_range(knee_db, MIN_COMPRESSOR_KNEE_DB, MAX_COMPRESSOR_KNEE_DB) {
            return Err(DspError::InvalidKneeDb(knee_db));
        }
        self.knee_db = knee_db;
        Ok(())
    }

    /// Sets the makeup gain in dB.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidMakeupGainDb`] outside `0.0..=24.0`.
    pub fn set_makeup_gain_db(&mut self, makeup_gain_db: f32) -> Result<(), DspError> {
        if !in_range(
            makeup_gain_db,
            MIN_COMPRESSOR_MAKEUP_GAIN_DB,
            MAX_COMPRESSOR_MAKEUP_GAIN_DB,
        ) {
            return Err(DspError::InvalidMakeupGainDb(makeup_gain_db));
        }
        self.makeup_gain_db = makeup_gain_db;
        Ok(())
    }

    /// Returns the last computed gain reduction as a non-negative dB value.
    #[must_use]
    pub fn current_gain_reduction_db(&self) -> f32 {
        to_f32(self.reduction_db.max(0.0))
    }

    /// Clears the detector envelope and gain-reduction state.
    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.reduction_db = 0.0;
    }

    /// Processes one sample.
    #[must_use]
    pub fn process(&mut self, sample: f32) -> f32 {
        let level = f64::from(sample.abs());
        let coefficient = if level > self.envelope {
            self.attack_coefficient
        } else {
            self.release_coefficient
        };
        self.envelope += (level - self.envelope) * coefficient;
        let level_db = 20.0 * self.envelope.max(ENVELOPE_FLOOR).log10();
        let over_db = level_db - f64::from(self.threshold_db);
        self.reduction_db =
            compressor_reduction_db(over_db, f64::from(self.ratio), f64::from(self.knee_db));
        let gain_db = f64::from(self.makeup_gain_db) - self.reduction_db;
        to_f32(f64::from(sample) * 10.0_f64.powf(gain_db / 20.0))
    }
}

/// Peak limiter with instant attack, smoothed release, and a hard ceiling.
///
/// The required gain for each sample is `ceiling_linear / |sample|` whenever
/// the input exceeds the ceiling, otherwise unity. The applied gain drops
/// immediately to the required value (instant attack) and otherwise moves
/// toward it with a one-pole release:
///
/// ```text
/// release_coefficient = 1 - exp(-1 / (release_ms / 1000 * sample_rate_hz))
/// ```
///
/// The output is clamped to `+/-ceiling_linear` after the gain, so no sample
/// can exceed the ceiling in linear magnitude even under floating-point
/// rounding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limiter {
    sample_rate_hz: f32,
    ceiling_db: f32,
    release_ms: f32,
    ceiling_linear: f64,
    release_coefficient: f64,
    gain: f64,
    reduction_db: f64,
}

impl Limiter {
    /// Creates a limiter.
    ///
    /// Accepted bounds are `ceiling_db` in `-60.0..=0.0` and `release_ms` in
    /// `0.1..=1000.0`.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidSampleRate`], [`DspError::InvalidCeilingDb`],
    /// or [`DspError::InvalidReleaseMs`] for an out-of-bounds value.
    pub fn new(sample_rate_hz: f32, ceiling_db: f32, release_ms: f32) -> Result<Self, DspError> {
        let sample_rate = validate_sample_rate(sample_rate_hz)?;
        if !in_range(ceiling_db, MIN_LIMITER_CEILING_DB, MAX_LIMITER_CEILING_DB) {
            return Err(DspError::InvalidCeilingDb(ceiling_db));
        }
        if !in_range(release_ms, MIN_RELEASE_MS, MAX_RELEASE_MS) {
            return Err(DspError::InvalidReleaseMs(release_ms));
        }
        Ok(Self {
            sample_rate_hz,
            ceiling_db,
            release_ms,
            ceiling_linear: 10.0_f64.powf(f64::from(ceiling_db) / 20.0),
            release_coefficient: one_pole_coefficient(release_ms, sample_rate),
            gain: 1.0,
            reduction_db: 0.0,
        })
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> f32 {
        self.sample_rate_hz
    }

    #[must_use]
    pub const fn ceiling_db(&self) -> f32 {
        self.ceiling_db
    }

    #[must_use]
    pub const fn release_ms(&self) -> f32 {
        self.release_ms
    }

    /// Returns the linear ceiling.
    ///
    /// A ceiling of 0 dB is exactly `1.0`.
    #[must_use]
    pub fn ceiling_linear(&self) -> f32 {
        to_f32(self.ceiling_linear)
    }

    /// Sets the ceiling in dB.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidCeilingDb`] outside `-60.0..=0.0`.
    pub fn set_ceiling_db(&mut self, ceiling_db: f32) -> Result<(), DspError> {
        if !in_range(ceiling_db, MIN_LIMITER_CEILING_DB, MAX_LIMITER_CEILING_DB) {
            return Err(DspError::InvalidCeilingDb(ceiling_db));
        }
        self.ceiling_db = ceiling_db;
        self.ceiling_linear = 10.0_f64.powf(f64::from(ceiling_db) / 20.0);
        Ok(())
    }

    /// Sets the release time in milliseconds and recomputes its coefficient.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidReleaseMs`] outside `0.1..=1000.0`.
    pub fn set_release_ms(&mut self, release_ms: f32) -> Result<(), DspError> {
        if !in_range(release_ms, MIN_RELEASE_MS, MAX_RELEASE_MS) {
            return Err(DspError::InvalidReleaseMs(release_ms));
        }
        self.release_ms = release_ms;
        self.release_coefficient = one_pole_coefficient(release_ms, f64::from(self.sample_rate_hz));
        Ok(())
    }

    /// Returns the current gain reduction as a non-negative dB value.
    #[must_use]
    pub fn current_gain_reduction_db(&self) -> f32 {
        to_f32(self.reduction_db.max(0.0))
    }

    /// Clears the limiter gain state to unity.
    pub fn reset(&mut self) {
        self.gain = 1.0;
        self.reduction_db = 0.0;
    }

    /// Processes one sample, clamping the result to the linear ceiling.
    #[must_use]
    pub fn process(&mut self, sample: f32) -> f32 {
        let input = f64::from(sample);
        let magnitude = input.abs();
        let required_gain = if magnitude > self.ceiling_linear {
            self.ceiling_linear / magnitude
        } else {
            1.0
        };
        if required_gain < self.gain {
            self.gain = required_gain;
        } else {
            self.gain += (required_gain - self.gain) * self.release_coefficient;
        }
        self.reduction_db = if self.gain >= 1.0 {
            0.0
        } else {
            -20.0 * self.gain.log10()
        };
        // An infinite input drives the gain to exactly zero, and `inf * 0` is
        // NaN, which `clamp` would pass through unchanged.
        let output = if magnitude.is_infinite() {
            self.ceiling_linear.copysign(input)
        } else {
            (input * self.gain).clamp(-self.ceiling_linear, self.ceiling_linear)
        };
        to_f32(output)
    }
}

/// Gate with threshold, range, attack, hold, and release.
///
/// The gate is a downward expander driven by the instantaneous absolute
/// sample value:
///
/// - A sample above the linear threshold resets the hold counter and moves the
///   gain toward unity with the one-pole attack coefficient.
/// - A sample at or below the threshold with hold time remaining keeps the
///   current gain and decrements the hold counter.
/// - Once the hold expires, the gain moves toward `10^(-range_db / 20)` with
///   the one-pole release coefficient, so the attenuation approaches at most
///   `range_db`.
///
/// Attack, hold, and release times are converted to sample counts at the
/// constructor sample rate with `samples = time_ms / 1000 * sample_rate_hz`,
/// rounded to the nearest sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gate {
    sample_rate_hz: f32,
    threshold_db: f32,
    range_db: f32,
    attack_ms: f32,
    hold_ms: f32,
    release_ms: f32,
    threshold_linear: f64,
    closed_gain: f64,
    attack_coefficient: f64,
    release_coefficient: f64,
    hold_samples: u64,
    hold_remaining: u64,
    gain: f64,
}

impl Gate {
    /// Creates a gate.
    ///
    /// Accepted bounds are:
    ///
    /// - `threshold_db` in `-80.0..=0.0`,
    /// - `range_db` in `0.0..=80.0`,
    /// - `attack_ms` in `0.1..=1000.0`,
    /// - `hold_ms` in `0.0..=1000.0`,
    /// - `release_ms` in `0.1..=5000.0`.
    ///
    /// # Errors
    ///
    /// Returns the matching [`DspError`] variant for any value outside its
    /// bound. Nothing is mutated on error.
    pub fn new(
        sample_rate_hz: f32,
        threshold_db: f32,
        range_db: f32,
        attack_ms: f32,
        hold_ms: f32,
        release_ms: f32,
    ) -> Result<Self, DspError> {
        let sample_rate = validate_sample_rate(sample_rate_hz)?;
        if !in_range(threshold_db, MIN_GATE_THRESHOLD_DB, MAX_GATE_THRESHOLD_DB) {
            return Err(DspError::InvalidThresholdDb(threshold_db));
        }
        if !in_range(range_db, MIN_GATE_RANGE_DB, MAX_GATE_RANGE_DB) {
            return Err(DspError::InvalidRangeDb(range_db));
        }
        if !in_range(attack_ms, MIN_ATTACK_MS, MAX_ATTACK_MS) {
            return Err(DspError::InvalidAttackMs(attack_ms));
        }
        if !in_range(hold_ms, MIN_GATE_HOLD_MS, MAX_GATE_HOLD_MS) {
            return Err(DspError::InvalidHoldMs(hold_ms));
        }
        if !in_range(release_ms, MIN_GATE_RELEASE_MS, MAX_GATE_RELEASE_MS) {
            return Err(DspError::InvalidReleaseMs(release_ms));
        }
        Ok(Self {
            sample_rate_hz,
            threshold_db,
            range_db,
            attack_ms,
            hold_ms,
            release_ms,
            threshold_linear: 10.0_f64.powf(f64::from(threshold_db) / 20.0),
            closed_gain: 10.0_f64.powf(-f64::from(range_db) / 20.0),
            attack_coefficient: one_pole_coefficient(attack_ms, sample_rate),
            release_coefficient: one_pole_coefficient(release_ms, sample_rate),
            hold_samples: hold_sample_count(hold_ms, sample_rate),
            hold_remaining: 0,
            gain: 1.0,
        })
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> f32 {
        self.sample_rate_hz
    }

    #[must_use]
    pub const fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    #[must_use]
    pub const fn range_db(&self) -> f32 {
        self.range_db
    }

    #[must_use]
    pub const fn attack_ms(&self) -> f32 {
        self.attack_ms
    }

    #[must_use]
    pub const fn hold_ms(&self) -> f32 {
        self.hold_ms
    }

    #[must_use]
    pub const fn release_ms(&self) -> f32 {
        self.release_ms
    }

    /// Sets the threshold in dB.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidThresholdDb`] outside `-80.0..=0.0`.
    pub fn set_threshold_db(&mut self, threshold_db: f32) -> Result<(), DspError> {
        if !in_range(threshold_db, MIN_GATE_THRESHOLD_DB, MAX_GATE_THRESHOLD_DB) {
            return Err(DspError::InvalidThresholdDb(threshold_db));
        }
        self.threshold_db = threshold_db;
        self.threshold_linear = 10.0_f64.powf(f64::from(threshold_db) / 20.0);
        Ok(())
    }

    /// Sets the attenuation range in dB.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidRangeDb`] outside `0.0..=80.0`.
    pub fn set_range_db(&mut self, range_db: f32) -> Result<(), DspError> {
        if !in_range(range_db, MIN_GATE_RANGE_DB, MAX_GATE_RANGE_DB) {
            return Err(DspError::InvalidRangeDb(range_db));
        }
        self.range_db = range_db;
        self.closed_gain = 10.0_f64.powf(-f64::from(range_db) / 20.0);
        Ok(())
    }

    /// Sets the opening attack time in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidAttackMs`] outside `0.1..=1000.0`.
    pub fn set_attack_ms(&mut self, attack_ms: f32) -> Result<(), DspError> {
        if !in_range(attack_ms, MIN_ATTACK_MS, MAX_ATTACK_MS) {
            return Err(DspError::InvalidAttackMs(attack_ms));
        }
        self.attack_ms = attack_ms;
        self.attack_coefficient = one_pole_coefficient(attack_ms, f64::from(self.sample_rate_hz));
        Ok(())
    }

    /// Sets the hold time in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidHoldMs`] outside `0.0..=1000.0`.
    pub fn set_hold_ms(&mut self, hold_ms: f32) -> Result<(), DspError> {
        if !in_range(hold_ms, MIN_GATE_HOLD_MS, MAX_GATE_HOLD_MS) {
            return Err(DspError::InvalidHoldMs(hold_ms));
        }
        self.hold_ms = hold_ms;
        self.hold_samples = hold_sample_count(hold_ms, f64::from(self.sample_rate_hz));
        Ok(())
    }

    /// Sets the closing release time in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidReleaseMs`] outside `0.1..=5000.0`.
    pub fn set_release_ms(&mut self, release_ms: f32) -> Result<(), DspError> {
        if !in_range(release_ms, MIN_GATE_RELEASE_MS, MAX_GATE_RELEASE_MS) {
            return Err(DspError::InvalidReleaseMs(release_ms));
        }
        self.release_ms = release_ms;
        self.release_coefficient = one_pole_coefficient(release_ms, f64::from(self.sample_rate_hz));
        Ok(())
    }

    /// Returns the current attenuation as a non-negative dB value.
    #[must_use]
    pub fn current_attenuation_db(&self) -> f32 {
        if self.gain >= 1.0 {
            0.0
        } else {
            to_f32((-20.0 * self.gain.log10()).max(0.0))
        }
    }

    /// Clears the gain and hold state while preserving settings.
    pub fn reset(&mut self) {
        self.gain = 1.0;
        self.hold_remaining = 0;
    }

    /// Processes one sample.
    #[must_use]
    pub fn process(&mut self, sample: f32) -> f32 {
        let level = f64::from(sample.abs());
        if level > self.threshold_linear {
            self.hold_remaining = self.hold_samples;
            self.gain += (1.0 - self.gain) * self.attack_coefficient;
        } else if self.hold_remaining > 0 {
            self.hold_remaining -= 1;
        } else {
            self.gain += (self.closed_gain - self.gain) * self.release_coefficient;
        }
        to_f32(f64::from(sample) * self.gain)
    }
}

/// Downward expander with threshold, ratio, attack, release, and range.
///
/// A one-pole peak envelope follows `|sample|` (the attack coefficient while
/// the level rises, the release coefficient while it falls). Below the
/// threshold the output level falls `ratio` times faster than the input,
/// so the attenuation is
///
/// ```text
/// under_db     = threshold_db - envelope_db
/// reduction_db = min((ratio - 1) * max(under_db, 0), range_db)
/// ```
///
/// A ratio of 1 or a range of 0 is transparent. Unlike [`Gate`], the
/// attenuation grows gradually with the distance below the threshold, so
/// quiet room tone is turned down rather than switched off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Expander {
    sample_rate_hz: f32,
    threshold_db: f32,
    ratio: f32,
    attack_ms: f32,
    release_ms: f32,
    range_db: f32,
    attack_coefficient: f64,
    release_coefficient: f64,
    envelope: f64,
    reduction_db: f64,
}

impl Expander {
    /// Creates an expander.
    ///
    /// Accepted bounds are:
    ///
    /// - `threshold_db` in `-80.0..=0.0`,
    /// - `ratio` in `1.0..=20.0`,
    /// - `attack_ms` in `0.1..=1000.0`,
    /// - `release_ms` in `0.1..=5000.0`,
    /// - `range_db` in `0.0..=80.0`.
    ///
    /// # Errors
    ///
    /// Returns the matching [`DspError`] variant for any value outside its
    /// bound. Nothing is mutated on error.
    pub fn new(
        sample_rate_hz: f32,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        range_db: f32,
    ) -> Result<Self, DspError> {
        let sample_rate = validate_sample_rate(sample_rate_hz)?;
        let mut expander = Self {
            sample_rate_hz,
            threshold_db: 0.0,
            ratio: 1.0,
            attack_ms: MIN_ATTACK_MS,
            release_ms: MIN_GATE_RELEASE_MS,
            range_db: 0.0,
            attack_coefficient: one_pole_coefficient(MIN_ATTACK_MS, sample_rate),
            release_coefficient: one_pole_coefficient(MIN_GATE_RELEASE_MS, sample_rate),
            envelope: 0.0,
            reduction_db: 0.0,
        };
        expander.set_threshold_db(threshold_db)?;
        expander.set_ratio(ratio)?;
        expander.set_attack_ms(attack_ms)?;
        expander.set_release_ms(release_ms)?;
        expander.set_range_db(range_db)?;
        Ok(expander)
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> f32 {
        self.sample_rate_hz
    }

    #[must_use]
    pub const fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    #[must_use]
    pub const fn ratio(&self) -> f32 {
        self.ratio
    }

    #[must_use]
    pub const fn attack_ms(&self) -> f32 {
        self.attack_ms
    }

    #[must_use]
    pub const fn release_ms(&self) -> f32 {
        self.release_ms
    }

    #[must_use]
    pub const fn range_db(&self) -> f32 {
        self.range_db
    }

    /// Sets the threshold in dB.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidThresholdDb`] outside `-80.0..=0.0`.
    pub fn set_threshold_db(&mut self, threshold_db: f32) -> Result<(), DspError> {
        if !in_range(
            threshold_db,
            MIN_EXPANDER_THRESHOLD_DB,
            MAX_EXPANDER_THRESHOLD_DB,
        ) {
            return Err(DspError::InvalidThresholdDb(threshold_db));
        }
        self.threshold_db = threshold_db;
        Ok(())
    }

    /// Sets the expansion ratio.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidRatio`] outside `1.0..=20.0`.
    pub fn set_ratio(&mut self, ratio: f32) -> Result<(), DspError> {
        if !in_range(ratio, MIN_EXPANDER_RATIO, MAX_EXPANDER_RATIO) {
            return Err(DspError::InvalidRatio(ratio));
        }
        self.ratio = ratio;
        Ok(())
    }

    /// Sets the attack time in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidAttackMs`] outside `0.1..=1000.0`.
    pub fn set_attack_ms(&mut self, attack_ms: f32) -> Result<(), DspError> {
        if !in_range(attack_ms, MIN_ATTACK_MS, MAX_ATTACK_MS) {
            return Err(DspError::InvalidAttackMs(attack_ms));
        }
        self.attack_ms = attack_ms;
        self.attack_coefficient = one_pole_coefficient(attack_ms, f64::from(self.sample_rate_hz));
        Ok(())
    }

    /// Sets the release time in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidReleaseMs`] outside `0.1..=5000.0`.
    pub fn set_release_ms(&mut self, release_ms: f32) -> Result<(), DspError> {
        if !in_range(release_ms, MIN_GATE_RELEASE_MS, MAX_GATE_RELEASE_MS) {
            return Err(DspError::InvalidReleaseMs(release_ms));
        }
        self.release_ms = release_ms;
        self.release_coefficient = one_pole_coefficient(release_ms, f64::from(self.sample_rate_hz));
        Ok(())
    }

    /// Sets the deepest attenuation in dB.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidRangeDb`] outside `0.0..=80.0`.
    pub fn set_range_db(&mut self, range_db: f32) -> Result<(), DspError> {
        if !in_range(range_db, MIN_EXPANDER_RANGE_DB, MAX_EXPANDER_RANGE_DB) {
            return Err(DspError::InvalidRangeDb(range_db));
        }
        self.range_db = range_db;
        Ok(())
    }

    /// Returns the last computed attenuation as a non-negative dB value.
    #[must_use]
    pub fn current_gain_reduction_db(&self) -> f32 {
        to_f32(self.reduction_db.max(0.0))
    }

    /// Clears the detector envelope and attenuation state.
    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.reduction_db = 0.0;
    }

    /// Processes one sample.
    #[must_use]
    pub fn process(&mut self, sample: f32) -> f32 {
        let level = f64::from(sample.abs());
        let coefficient = if level > self.envelope {
            self.attack_coefficient
        } else {
            self.release_coefficient
        };
        self.envelope += (level - self.envelope) * coefficient;
        let level_db = 20.0 * self.envelope.max(ENVELOPE_FLOOR).log10();
        let under_db = (f64::from(self.threshold_db) - level_db).max(0.0);
        self.reduction_db =
            ((f64::from(self.ratio) - 1.0) * under_db).min(f64::from(self.range_db));
        to_f32(f64::from(sample) * 10.0_f64.powf(-self.reduction_db / 20.0))
    }
}

#[derive(Clone, Debug)]
struct ChannelDsp {
    polarity: Polarity,
    eq: ParametricEq,
    gate: Option<Gate>,
    compressor: Option<Compressor>,
    limiter: Option<Limiter>,
    meter: DspMeter,
}

impl ChannelDsp {
    fn process(&mut self, sample: f32) -> f32 {
        let mut output = self.polarity.apply(sample);
        output = self.eq.process(output);
        if let Some(gate) = &mut self.gate {
            output = gate.process(output);
        }
        if let Some(compressor) = &mut self.compressor {
            output = compressor.process(output);
        }
        if let Some(limiter) = &mut self.limiter {
            output = limiter.process(output);
        }
        self.meter.add_sample(output);
        output
    }

    fn reset(&mut self) {
        self.eq.reset();
        if let Some(gate) = &mut self.gate {
            gate.reset();
        }
        if let Some(compressor) = &mut self.compressor {
            compressor.reset();
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.reset();
        }
        self.meter.reset();
    }
}

/// Per-channel strip DSP with a fixed processing order.
///
/// The chain is always
/// `polarity -> high-pass -> EQ bands -> gate -> compressor -> limiter`, and
/// each channel owns independent filter, detector, and meter state. All
/// channels share the same settings; a setter validates once and then applies
/// the same configuration to every channel, so a failed setter changes
/// nothing. Enabling or replacing a stage resets that stage's state for all
/// channels, which keeps runs deterministic.
///
/// [`Self::new`] allocates all per-channel storage. Steady-state
/// [`Self::process_block`] and [`Self::process_sample`] calls perform no
/// allocation, and an [`AudioBlock`] with a different channel count or sample
/// rate is rejected before any sample is processed.
#[derive(Clone, Debug)]
pub struct StripDsp {
    sample_rate_hz: f32,
    channels: Vec<ChannelDsp>,
}

impl StripDsp {
    /// Creates a strip with every stage bypassed and exactly `channels`
    /// independent channel states.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidSampleRate`] unless the rate is finite and
    /// greater than zero, and [`DspError::ChannelCountOutOfRange`] unless
    /// `channels` is in `1..=`[`MAX_CHANNELS`].
    pub fn new(sample_rate_hz: f32, channels: usize) -> Result<Self, DspError> {
        validate_sample_rate(sample_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(DspError::ChannelCountOutOfRange {
                actual: channels,
                maximum: MAX_CHANNELS,
            });
        }
        let channel = ChannelDsp {
            polarity: Polarity::Normal,
            eq: ParametricEq::new(sample_rate_hz)?,
            gate: None,
            compressor: None,
            limiter: None,
            meter: DspMeter::new(),
        };
        Ok(Self {
            sample_rate_hz,
            channels: vec![channel; channels],
        })
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> f32 {
        self.sample_rate_hz
    }

    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    #[must_use]
    pub fn polarity(&self) -> Polarity {
        self.channels
            .first()
            .map_or(Polarity::Normal, |channel| channel.polarity)
    }

    /// Sets the polarity for every channel.
    pub fn set_polarity(&mut self, polarity: Polarity) {
        for channel in &mut self.channels {
            channel.polarity = polarity;
        }
    }

    #[must_use]
    pub fn high_pass(&self) -> Option<BiquadSettings> {
        self.channels
            .first()
            .and_then(|channel| channel.eq.high_pass())
    }

    /// Installs the shared high-pass section for every channel.
    ///
    /// # Errors
    ///
    /// See [`BiquadCoefficients::high_pass`]. No channel is changed on error.
    pub fn set_high_pass(&mut self, frequency_hz: f32, q: f32) -> Result<(), DspError> {
        let coefficients = BiquadCoefficients::high_pass(self.sample_rate_hz, frequency_hz, q)?;
        let settings = BiquadSettings {
            kind: BiquadKind::HighPass,
            frequency_hz,
            q,
            gain_db: 0.0,
        };
        for channel in &mut self.channels {
            channel.eq.install_high_pass(settings, coefficients);
        }
        Ok(())
    }

    /// Removes the shared high-pass section from every channel.
    pub fn clear_high_pass(&mut self) {
        for channel in &mut self.channels {
            channel.eq.clear_high_pass();
        }
    }

    #[must_use]
    pub fn eq_band(&self, band: usize) -> Option<BiquadSettings> {
        self.channels
            .first()
            .and_then(|channel| channel.eq.band(band))
    }

    /// Installs or replaces one shared peaking band for every channel.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::BandIndexOutOfRange`] when `band` is at least
    /// [`MAX_EQ_BANDS`], and otherwise the errors documented by
    /// [`BiquadCoefficients::peaking_eq`]. No channel is changed on error.
    pub fn set_eq_band(
        &mut self,
        band: usize,
        frequency_hz: f32,
        q: f32,
        gain_db: f32,
    ) -> Result<(), DspError> {
        if band >= MAX_EQ_BANDS {
            return Err(DspError::BandIndexOutOfRange {
                band,
                bands: MAX_EQ_BANDS,
            });
        }
        let coefficients =
            BiquadCoefficients::peaking_eq(self.sample_rate_hz, frequency_hz, q, gain_db)?;
        let settings = BiquadSettings {
            kind: BiquadKind::PeakingEq,
            frequency_hz,
            q,
            gain_db,
        };
        for channel in &mut self.channels {
            channel.eq.install_band(band, settings, coefficients);
        }
        Ok(())
    }

    /// Removes one shared peaking band from every channel.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::BandIndexOutOfRange`] when `band` is at least
    /// [`MAX_EQ_BANDS`]. No channel is changed on error.
    pub fn clear_eq_band(&mut self, band: usize) -> Result<(), DspError> {
        if band >= MAX_EQ_BANDS {
            return Err(DspError::BandIndexOutOfRange {
                band,
                bands: MAX_EQ_BANDS,
            });
        }
        for channel in &mut self.channels {
            channel.eq.remove_band(band);
        }
        Ok(())
    }

    /// Enables or replaces the shared gate for every channel.
    ///
    /// # Errors
    ///
    /// See [`Gate::new`]. No channel is changed on error.
    pub fn set_gate(
        &mut self,
        threshold_db: f32,
        range_db: f32,
        attack_ms: f32,
        hold_ms: f32,
        release_ms: f32,
    ) -> Result<(), DspError> {
        let gate = Gate::new(
            self.sample_rate_hz,
            threshold_db,
            range_db,
            attack_ms,
            hold_ms,
            release_ms,
        )?;
        for channel in &mut self.channels {
            channel.gate = Some(gate);
        }
        Ok(())
    }

    /// Removes the shared gate from every channel.
    pub fn clear_gate(&mut self) {
        for channel in &mut self.channels {
            channel.gate = None;
        }
    }

    /// Enables or replaces the shared compressor for every channel.
    ///
    /// # Errors
    ///
    /// See [`Compressor::new`]. No channel is changed on error.
    pub fn set_compressor(
        &mut self,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        knee_db: f32,
        makeup_gain_db: f32,
    ) -> Result<(), DspError> {
        let compressor = Compressor::new(
            self.sample_rate_hz,
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            knee_db,
            makeup_gain_db,
        )?;
        for channel in &mut self.channels {
            channel.compressor = Some(compressor);
        }
        Ok(())
    }

    /// Removes the shared compressor from every channel.
    pub fn clear_compressor(&mut self) {
        for channel in &mut self.channels {
            channel.compressor = None;
        }
    }

    /// Enables or replaces the shared limiter for every channel.
    ///
    /// # Errors
    ///
    /// See [`Limiter::new`]. No channel is changed on error.
    pub fn set_limiter(&mut self, ceiling_db: f32, release_ms: f32) -> Result<(), DspError> {
        let limiter = Limiter::new(self.sample_rate_hz, ceiling_db, release_ms)?;
        for channel in &mut self.channels {
            channel.limiter = Some(limiter);
        }
        Ok(())
    }

    /// Removes the shared limiter from every channel.
    pub fn clear_limiter(&mut self) {
        for channel in &mut self.channels {
            channel.limiter = None;
        }
    }

    /// Processes one sample through one channel.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::ChannelIndexOutOfRange`] when `channel` is at least
    /// [`Self::channels`].
    pub fn process_sample(&mut self, channel: usize, sample: f32) -> Result<f32, DspError> {
        let channels = self.channels.len();
        let Some(state) = self.channels.get_mut(channel) else {
            return Err(DspError::ChannelIndexOutOfRange { channel, channels });
        };
        Ok(state.process(sample))
    }

    /// Processes a whole block in place.
    ///
    /// Validation completes before any sample changes, so a rejected block is
    /// left untouched and no channel state advances. Successful processing
    /// preserves the block format and sample count and performs no allocation.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::ChannelCountMismatch`] when the block channel count
    /// differs from [`Self::channels`], [`DspError::SampleRateMismatch`] when
    /// the block sample rate differs from the configured rate,
    /// [`DspError::SampleCountOutOfRange`] when the block exceeds
    /// [`MAX_SAMPLES_PER_BLOCK`], or [`DspError::NonFiniteSample`] when the
    /// block contains a non-finite sample.
    pub fn process_block(&mut self, block: &mut AudioBlock) -> Result<(), DspError> {
        if block.channels() != self.channels.len() {
            return Err(DspError::ChannelCountMismatch {
                expected: self.channels.len(),
                actual: block.channels(),
            });
        }
        let samples = block.samples();
        if samples > MAX_SAMPLES_PER_BLOCK {
            return Err(DspError::SampleCountOutOfRange {
                actual: samples,
                maximum: MAX_SAMPLES_PER_BLOCK,
            });
        }
        let actual_hz = block.format().sample_rate.hertz();
        let expected_hz = f64::from(self.sample_rate_hz);
        if (f64::from(actual_hz) - expected_hz).abs() > f64::EPSILON {
            return Err(DspError::SampleRateMismatch {
                expected_hz: self.sample_rate_hz,
                actual_hz,
            });
        }
        for (channel, plane) in block.planes.iter().enumerate() {
            if let Some(sample) = plane[..samples].iter().position(|value| !value.is_finite()) {
                return Err(DspError::NonFiniteSample { channel, sample });
            }
        }
        for (state, plane) in self.channels.iter_mut().zip(block.planes.iter_mut()) {
            for sample in &mut plane[..samples] {
                *sample = state.process(*sample);
            }
        }
        Ok(())
    }

    /// Clears filter, detector, and meter state for every channel while
    /// preserving configured settings.
    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }
    }

    /// Returns the post-chain peak/RMS window for one channel.
    #[must_use]
    pub fn output_meter(&self, channel: usize) -> Option<DspMeterReading> {
        self.channels
            .get(channel)
            .map(|state| state.meter.reading())
    }

    /// Returns the enabled compressor's last gain reduction for one channel.
    #[must_use]
    pub fn compressor_gain_reduction_db(&self, channel: usize) -> Option<f32> {
        self.channels
            .get(channel)?
            .compressor
            .as_ref()
            .map(Compressor::current_gain_reduction_db)
    }

    /// Returns the enabled limiter's last gain reduction for one channel.
    #[must_use]
    pub fn limiter_gain_reduction_db(&self, channel: usize) -> Option<f32> {
        self.channels
            .get(channel)?
            .limiter
            .as_ref()
            .map(Limiter::current_gain_reduction_db)
    }

    /// Returns the enabled gate's current attenuation for one channel.
    #[must_use]
    pub fn gate_attenuation_db(&self, channel: usize) -> Option<f32> {
        self.channels
            .get(channel)?
            .gate
            .as_ref()
            .map(Gate::current_attenuation_db)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn to_f32(value: f64) -> f32 {
    value as f32
}

#[allow(clippy::cast_precision_loss)]
const fn count_to_f64(count: u64) -> f64 {
    count as f64
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn hold_sample_count(hold_ms: f32, sample_rate_hz: f64) -> u64 {
    let samples = f64::from(hold_ms) / 1_000.0 * sample_rate_hz;
    samples.round().max(0.0) as u64
}

fn validate_sample_rate(sample_rate_hz: f32) -> Result<f64, DspError> {
    if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
        return Err(DspError::InvalidSampleRate(sample_rate_hz));
    }
    Ok(f64::from(sample_rate_hz))
}

fn validate_frequency(frequency_hz: f32, sample_rate_hz: f64) -> Result<f64, DspError> {
    let frequency = f64::from(frequency_hz);
    if !frequency_hz.is_finite() || frequency_hz <= 0.0 || frequency >= sample_rate_hz / 2.0 {
        return Err(DspError::InvalidFrequency(frequency_hz));
    }
    Ok(frequency)
}

fn validate_q(q: f32) -> Result<f64, DspError> {
    if !q.is_finite() || q <= 0.0 {
        return Err(DspError::InvalidQ(q));
    }
    Ok(f64::from(q))
}

fn eq_amplitude(gain_db: f32) -> Result<f64, DspError> {
    if !gain_db.is_finite() {
        return Err(DspError::InvalidEqGainDb(gain_db));
    }
    let amplitude = 10.0_f64.powf(f64::from(gain_db) / 40.0);
    if !amplitude.is_finite() || amplitude <= 0.0 {
        return Err(DspError::InvalidEqGainDb(gain_db));
    }
    Ok(amplitude)
}

fn in_range(value: f32, minimum: f32, maximum: f32) -> bool {
    value.is_finite() && (minimum..=maximum).contains(&value)
}

fn one_pole_coefficient(time_ms: f32, sample_rate_hz: f64) -> f64 {
    1.0 - (-1_000.0 / (f64::from(time_ms) * sample_rate_hz)).exp()
}

fn compressor_reduction_db(over_db: f64, ratio: f64, knee_db: f64) -> f64 {
    let slope = 1.0 - 1.0 / ratio;
    if knee_db <= 0.0 {
        return slope * over_db.max(0.0);
    }
    let half_knee = knee_db / 2.0;
    if over_db <= -half_knee {
        0.0
    } else if over_db >= half_knee {
        slope * over_db
    } else {
        slope * (over_db + half_knee).powi(2) / (2.0 * knee_db)
    }
}
