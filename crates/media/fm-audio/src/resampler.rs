//! Deterministic polyphase windowed-sinc kernel for sample-rate conversion.
//!
//! The kernel is a Kaiser-windowed sinc low-pass sampled at
//! [`SINC_PHASES`] fractional phases; coefficients between adjacent phases are
//! linearly interpolated, so any fractional read position (including the
//! slowly varying positions produced by drift compensation) is supported.
//!
//! Quality, as designed and as measured by `tests/sample_rate_sync.rs`:
//!
//! * cutoff at [`SINC_CUTOFF_FRACTION`] of the lower Nyquist frequency
//!   (about 20.1 kHz for 44.1 kHz and 21.8 kHz for 48/96 kHz into 48 kHz);
//! * [`SINC_ZERO_CROSSINGS`] zero crossings per side, so 72 taps for 44.1 or
//!   48 kHz input and 142 taps for 96 kHz input into 48 kHz;
//! * Kaiser beta [`SINC_KAISER_BETA`], roughly 90 dB of side-lobe rejection.
//!
//! Measured (fixed ratio, 0.5-amplitude tones at 1, 10, and 17 kHz): SNR
//! (THD+N) of 105.9-110.2 dB for 44.1 kHz input, limited by linear
//! interpolation between phases, and about 150 dB for 48 and 96 kHz input,
//! whose phases fall exactly on table rows; passband gain error below
//! 0.001 dB up to 17 kHz; 96 kHz tones at 26-40 kHz alias at -107 dB or lower.
//!
//! Construction is the only allocating step. All arithmetic is plain IEEE
//! `f32`/`f64` without fused operations, so identical input produces identical
//! output on a given platform; the table itself uses the platform `sin`.

use core::fmt;

use fm_types::SampleRate;

/// Default engine (Master) audio clock: 48 kHz.
pub const ENGINE_SAMPLE_RATE_HZ: u32 = 48_000;
/// Default engine (Master) audio clock as a typed rate.
pub const ENGINE_SAMPLE_RATE: SampleRate = match SampleRate::new(ENGINE_SAMPLE_RATE_HZ) {
    Some(rate) => rate,
    None => panic!("engine sample rate must be non-zero"),
};
/// Lowest input or output rate accepted by the resampler.
pub const MIN_RESAMPLER_RATE_HZ: u32 = 8_000;
/// Highest input or output rate accepted by the resampler.
pub const MAX_RESAMPLER_RATE_HZ: u32 = 192_000;
/// Largest accepted `input / output` rate ratio. Bounds kernel length.
pub const MAX_RESAMPLER_DECIMATION: u32 = 4;
/// Sinc zero crossings on each side of the kernel centre.
pub const SINC_ZERO_CROSSINGS: usize = 32;
/// Number of tabulated fractional phases (one extra row closes the interval).
pub const SINC_PHASES: usize = 512;
/// Kaiser window shape parameter.
pub const SINC_KAISER_BETA: f64 = 9.0;
/// Cutoff as a fraction of the lower of the input and output Nyquist rates.
pub const SINC_CUTOFF_FRACTION: f64 = 0.91;

/// Errors from resampler kernel construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResamplerError {
    InputRateOutOfRange(u32),
    OutputRateOutOfRange(u32),
    DecimationTooLarge { input_hz: u32, output_hz: u32 },
}

impl fmt::Display for ResamplerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputRateOutOfRange(rate) => write!(
                formatter,
                "input rate {rate} Hz is outside {MIN_RESAMPLER_RATE_HZ}..={MAX_RESAMPLER_RATE_HZ} Hz"
            ),
            Self::OutputRateOutOfRange(rate) => write!(
                formatter,
                "output rate {rate} Hz is outside {MIN_RESAMPLER_RATE_HZ}..={MAX_RESAMPLER_RATE_HZ} Hz"
            ),
            Self::DecimationTooLarge {
                input_hz,
                output_hz,
            } => write!(
                formatter,
                "input rate {input_hz} Hz exceeds {MAX_RESAMPLER_DECIMATION}x output rate {output_hz} Hz"
            ),
        }
    }
}

impl std::error::Error for ResamplerError {}

/// Precomputed polyphase windowed-sinc table for one input/output rate pair.
#[derive(Clone, Debug)]
pub struct SincKernel {
    input_rate: SampleRate,
    output_rate: SampleRate,
    half_width: usize,
    taps: usize,
    cutoff: f64,
    table: Vec<f32>,
}

impl SincKernel {
    /// Builds the table for converting `input_rate` to `output_rate`.
    ///
    /// # Errors
    ///
    /// Returns a typed error for rates outside the supported range or a
    /// decimation ratio above [`MAX_RESAMPLER_DECIMATION`].
    pub fn new(input_rate: SampleRate, output_rate: SampleRate) -> Result<Self, ResamplerError> {
        let input_hz = input_rate.hertz();
        let output_hz = output_rate.hertz();
        if !(MIN_RESAMPLER_RATE_HZ..=MAX_RESAMPLER_RATE_HZ).contains(&input_hz) {
            return Err(ResamplerError::InputRateOutOfRange(input_hz));
        }
        if !(MIN_RESAMPLER_RATE_HZ..=MAX_RESAMPLER_RATE_HZ).contains(&output_hz) {
            return Err(ResamplerError::OutputRateOutOfRange(output_hz));
        }
        if input_hz > output_hz * MAX_RESAMPLER_DECIMATION {
            return Err(ResamplerError::DecimationTooLarge {
                input_hz,
                output_hz,
            });
        }
        // Normalised cutoff relative to the input Nyquist frequency.
        let cutoff =
            SINC_CUTOFF_FRACTION * f64::from(input_hz.min(output_hz)) / f64::from(input_hz);
        let half_width = ceil_to_usize(usize_to_f64(SINC_ZERO_CROSSINGS) / cutoff);
        let taps = half_width * 2;
        let mut table = vec![0.0_f32; (SINC_PHASES + 1) * taps];
        let mut row = vec![0.0_f64; taps];
        let window_norm = bessel_i0(SINC_KAISER_BETA);
        let centre = usize_to_f64(half_width - 1);
        let width = usize_to_f64(half_width);
        for phase in 0..=SINC_PHASES {
            let offset = usize_to_f64(phase) / usize_to_f64(SINC_PHASES);
            let mut sum = 0.0;
            for (tap, value) in row.iter_mut().enumerate() {
                let tau = usize_to_f64(tap) - centre - offset;
                let x = tau / width;
                let window = if x.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(SINC_KAISER_BETA * (1.0 - x * x).sqrt()) / window_norm
                };
                *value = cutoff * sinc(cutoff * tau) * window;
                sum += *value;
            }
            let destination = &mut table[phase * taps..(phase + 1) * taps];
            for (out, value) in destination.iter_mut().zip(&row) {
                *out = f64_to_f32(value / sum);
            }
        }
        Ok(Self {
            input_rate,
            output_rate,
            half_width,
            taps,
            cutoff,
            table,
        })
    }

    #[must_use]
    pub const fn input_rate(&self) -> SampleRate {
        self.input_rate
    }

    #[must_use]
    pub const fn output_rate(&self) -> SampleRate {
        self.output_rate
    }

    /// Input samples needed on each side of a read position. One output reads
    /// `half_width - 1` samples before and `half_width` samples after the
    /// integer read index.
    #[must_use]
    pub const fn half_width(&self) -> usize {
        self.half_width
    }

    /// Total taps per output sample.
    #[must_use]
    pub const fn taps(&self) -> usize {
        self.taps
    }

    /// Designed -6 dB cutoff in hertz.
    #[must_use]
    pub fn cutoff_hz(&self) -> f64 {
        self.cutoff * f64::from(self.input_rate.hertz()) / 2.0
    }

    /// Writes the interpolated coefficients for fractional read phase
    /// `numerator / denominator` (which must be `< 1`) into `out`.
    pub(crate) fn coefficients_into(&self, numerator: u64, denominator: u64, out: &mut [f32]) {
        debug_assert!(numerator < denominator);
        debug_assert_eq!(out.len(), self.taps);
        let scaled = u128::from(numerator) * SINC_PHASES as u128;
        let denominator = u128::from(denominator);
        let phase = u128_to_usize(scaled / denominator);
        let alpha = f64_to_f32(u128_to_f64(scaled % denominator) / u128_to_f64(denominator));
        let lower = &self.table[phase * self.taps..(phase + 1) * self.taps];
        let upper = &self.table[(phase + 1) * self.taps..(phase + 2) * self.taps];
        for ((coefficient, a), b) in out.iter_mut().zip(lower).zip(upper) {
            *coefficient = a + alpha * (b - a);
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let arg = core::f64::consts::PI * x;
        arg.sin() / arg
    }
}

/// Zeroth-order modified Bessel function of the first kind (power series).
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..64_u32 {
        let factor = half / f64::from(k);
        term *= factor * factor;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[allow(clippy::cast_precision_loss)]
pub(crate) const fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

#[allow(clippy::cast_precision_loss)]
pub(crate) const fn u128_to_f64(value: u128) -> f64 {
    value as f64
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) const fn f64_to_f32(value: f64) -> f32 {
    value as f32
}

#[allow(clippy::cast_possible_truncation)]
const fn u128_to_usize(value: u128) -> usize {
    value as usize
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ceil_to_usize(value: f64) -> usize {
    value.ceil() as usize
}
