//! ITU-R BS.1770-4 / EBU R128 loudness and true-peak metering (AU-005).
//!
//! [`LoudnessMeter`] measures one bus (a strip's contribution to Master, or
//! Master itself) in its channel layout:
//!
//! - every channel passes the BS.1770 K-weighting (the "pre-filter" high
//!   shelf and the "RLB" high-pass), derived from the analog prototypes for
//!   the meter's sample rate, so 48 kHz reproduces the standard's
//!   coefficients;
//! - weighted mean squares are summed over channels with the BS.1770
//!   weights (1.0 for left, right, centre, and mono; 1.41 for surrounds;
//!   the LFE is excluded) and gathered in 100 ms steps;
//! - **momentary** loudness is the last 400 ms, **short-term** the last 3 s,
//!   each `-0.691 + 10 log10(z)` LUFS and reported only once its window is
//!   full;
//! - **integrated** loudness gates the 400 ms blocks (75 % overlap) at the
//!   absolute -70 LUFS and then at 10 LU below the ungated mean. The blocks
//!   are kept in a fixed 0.05 LU histogram from -70 to +10 LUFS (energy and
//!   count per bin), so the measurement runs for any length of programme in
//!   constant memory; the relative gate is resolved to the bin centre, well
//!   inside the EBU Tech 3341 tolerance;
//! - **loudness range** (LRA, EBU Tech 3342) takes the short-term (3 s)
//!   loudness every 100 ms, gates it at the absolute -70 LUFS and then at
//!   20 LU below the mean of what passed, and reports the spread between the
//!   10th and 95th percentiles of the rest. The short-term values are kept
//!   in the same kind of fixed 0.05 LU histogram, and the percentiles are
//!   resolved to bin centres;
//! - **true peak** is the largest magnitude of the signal upsampled four
//!   times with a 48-tap Kaiser-windowed sinc interpolator (BS.1770 Annex
//!   2), together with the sample peak.
//!
//! An operator reset ([`LoudnessMeter::reset_measurement`]) starts the
//! integrated loudness, the loudness range, and the maximum true peak over
//! without interrupting momentary and short-term loudness: only gating
//! blocks and short-term values measured wholly after the reset count.
//!
//! [`LoudnessMeter::new`] allocates; measuring, reading, and resetting do
//! not, so a meter can run in the real-time render.

use fm_types::{Channel, ChannelLayout};

use crate::dsp::DspError;

/// The loudness step momentary, short-term, and gating blocks advance by.
const HOP_SECONDS: f64 = 0.1;
/// Hops in one momentary window and gating block (400 ms).
const MOMENTARY_HOPS: usize = 4;
/// Hops in one short-term window (3 s).
const SHORT_TERM_HOPS: usize = 30;
/// The absolute gate, in LUFS.
pub const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// The relative gate below the absolute-gated mean, in LU.
const RELATIVE_GATE_LU: f64 = 10.0;
/// The loudness-range relative gate below the absolute-gated short-term
/// mean, in LU (EBU Tech 3342).
const RANGE_RELATIVE_GATE_LU: f64 = 20.0;
/// The loudness-range low and high percentiles (EBU Tech 3342).
const RANGE_LOW_PERCENTILE: f64 = 0.10;
const RANGE_HIGH_PERCENTILE: f64 = 0.95;
/// The integrated histogram's bin width and top, in LU / LUFS.
const BIN_WIDTH_LU: f64 = 0.05;
const HISTOGRAM_TOP_LUFS: f64 = 10.0;
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
const BINS: usize = ((HISTOGRAM_TOP_LUFS - ABSOLUTE_GATE_LUFS) / BIN_WIDTH_LU) as usize;
/// True-peak oversampling factor and taps per polyphase branch.
const OVERSAMPLE: usize = 4;
const TAPS_PER_PHASE: usize = 12;
/// The Kaiser window's shape for the true-peak interpolator.
const KAISER_BETA: f64 = 6.0;
/// The loudness offset of BS.1770.
const LOUDNESS_OFFSET: f64 = -0.691;

/// One loudness reading; `None` while a window is not yet full, the meter
/// has heard only silence, or nothing passed the gates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoudnessReading {
    /// The last 400 ms, in LUFS.
    pub momentary_lufs: Option<f64>,
    /// The last 3 s, in LUFS.
    pub short_term_lufs: Option<f64>,
    /// Gated programme loudness since the last reset, in LUFS.
    pub integrated_lufs: Option<f64>,
    /// The loudness range (EBU Tech 3342) since the last reset, in LU.
    pub loudness_range_lu: Option<f64>,
    /// The largest true peak since the last [`LoudnessMeter::take_reading`],
    /// in dBTP.
    pub true_peak_dbtp: Option<f64>,
    /// The largest true peak since the last reset, in dBTP.
    pub max_true_peak_dbtp: Option<f64>,
}

/// A K-weighting biquad in transposed direct form II.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct BiquadState {
    z1: f64,
    z2: f64,
}

impl BiquadState {
    fn process(&mut self, filter: &Biquad, input: f64) -> f64 {
        let output = filter.b[0] * input + self.z1;
        self.z1 = filter.b[1] * input - filter.a[0] * output + self.z2;
        self.z2 = filter.b[2] * input - filter.a[1] * output;
        output
    }
}

/// One channel's filter, true-peak history, and weight.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ChannelState {
    weight: f64,
    shelf: BiquadState,
    high_pass: BiquadState,
    /// The last [`TAPS_PER_PHASE`] input samples, written twice so a
    /// contiguous window always starts at `position`.
    history: [f32; 2 * TAPS_PER_PHASE],
}

/// A BS.1770 loudness and true-peak meter; see the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct LoudnessMeter {
    shelf: Biquad,
    high_pass: Biquad,
    /// Polyphase interpolator taps, newest sample first.
    phases: [[f32; TAPS_PER_PHASE]; OVERSAMPLE],
    channels: Vec<ChannelState>,
    position: usize,
    hop_samples: usize,
    hop_count: usize,
    hop_energy: f64,
    /// Mean weighted square of the last [`SHORT_TERM_HOPS`] hops.
    hops: [f64; SHORT_TERM_HOPS],
    hops_filled: usize,
    hop_next: usize,
    bin_counts: Box<[u32]>,
    bin_energy: Box<[f64]>,
    /// Short-term values since the reset, per 0.05 LU bin.
    range_counts: Box<[u32]>,
    range_energy: Box<[f64]>,
    /// Hops completed wholly after the last reset (saturating).
    measured_hops: usize,
    /// Whether the hop in progress began before the last reset.
    partial_hop: bool,
    interval_peak: f32,
    max_peak: f32,
}

/// The BS.1770 weight of a channel.
const fn channel_weight(channel: Channel) -> f64 {
    match channel {
        Channel::LowFrequency => 0.0,
        Channel::LeftSurround
        | Channel::RightSurround
        | Channel::LeftRearSurround
        | Channel::RightRearSurround => 1.41,
        _ => 1.0,
    }
}

/// The zeroth-order modified Bessel function, for the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..50_u32 {
        let k = f64::from(k);
        term *= (half / k) * (half / k);
        sum += term;
        if term < sum * 1e-15 {
            break;
        }
    }
    sum
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn interpolator() -> [[f32; TAPS_PER_PHASE]; OVERSAMPLE] {
    let taps = OVERSAMPLE * TAPS_PER_PHASE;
    let center = (taps as f64 - 1.0) / 2.0;
    let mut phases = [[0.0_f32; TAPS_PER_PHASE]; OVERSAMPLE];
    for (phase, branch) in phases.iter_mut().enumerate() {
        let mut values = [0.0_f64; TAPS_PER_PHASE];
        for (k, value) in values.iter_mut().enumerate() {
            let n = (k * OVERSAMPLE + phase) as f64;
            let t = (n - center) / OVERSAMPLE as f64;
            let sinc = if t.abs() < 1e-12 {
                1.0
            } else {
                (core::f64::consts::PI * t).sin() / (core::f64::consts::PI * t)
            };
            let ratio = (n - center) / (center + 0.5);
            let window = bessel_i0(KAISER_BETA * (1.0 - ratio * ratio).max(0.0).sqrt())
                / bessel_i0(KAISER_BETA);
            *value = sinc * window;
        }
        // Each branch passes DC at unity.
        let sum: f64 = values.iter().sum();
        for (tap, value) in branch.iter_mut().zip(values) {
            *tap = (value / sum) as f32;
        }
    }
    phases
}

fn loudness_of(mean_square: f64) -> Option<f64> {
    (mean_square > 0.0 && mean_square.is_finite())
        .then(|| LOUDNESS_OFFSET + 10.0 * mean_square.log10())
}

/// The histogram bin of a mean square above the absolute gate.
fn gated_bin(mean_square: f64) -> Option<usize> {
    let loudness = loudness_of(mean_square)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (loudness > ABSOLUTE_GATE_LUFS)
        .then(|| (((loudness - ABSOLUTE_GATE_LUFS) / BIN_WIDTH_LU) as usize).min(BINS - 1))
}

fn dbtp_of(peak: f32) -> Option<f64> {
    (peak > 0.0 && peak.is_finite()).then(|| 20.0 * f64::from(peak).log10())
}

impl LoudnessMeter {
    /// Creates a meter for `layout` at `sample_rate_hz`.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidSampleRate`] below 8 kHz or above 768 kHz.
    pub fn new(sample_rate_hz: u32, layout: &ChannelLayout) -> Result<Self, DspError> {
        if !(8_000..=768_000).contains(&sample_rate_hz) {
            #[allow(clippy::cast_precision_loss)]
            return Err(DspError::InvalidSampleRate(sample_rate_hz as f32));
        }
        let rate = f64::from(sample_rate_hz);
        // The pre-filter: a high shelf of about +4 dB above 1.5 kHz.
        let shelf = {
            let f0 = 1_681.974_450_955_533;
            let gain_db = 3.999_843_853_973_347;
            let q = 0.707_175_236_955_419_6;
            let k = (core::f64::consts::PI * f0 / rate).tan();
            let vh = 10.0_f64.powf(gain_db / 20.0);
            let vb = vh.powf(0.499_666_774_154_541_6);
            let a0 = 1.0 + k / q + k * k;
            Biquad {
                b: [
                    (vh + vb * k / q + k * k) / a0,
                    2.0 * (k * k - vh) / a0,
                    (vh - vb * k / q + k * k) / a0,
                ],
                a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            }
        };
        // The RLB weighting: a second-order high-pass near 38 Hz.
        let high_pass = {
            let f0 = 38.135_470_876_024_44;
            let q = 0.500_327_037_323_877_3;
            let k = (core::f64::consts::PI * f0 / rate).tan();
            let a0 = 1.0 + k / q + k * k;
            Biquad {
                b: [1.0, -2.0, 1.0],
                a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            }
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let hop_samples = (rate * HOP_SECONDS).round().max(1.0) as usize;
        Ok(Self {
            shelf,
            high_pass,
            phases: interpolator(),
            channels: layout
                .channels()
                .iter()
                .map(|channel| ChannelState {
                    weight: channel_weight(*channel),
                    shelf: BiquadState::default(),
                    high_pass: BiquadState::default(),
                    history: [0.0; 2 * TAPS_PER_PHASE],
                })
                .collect(),
            position: 0,
            hop_samples,
            hop_count: 0,
            hop_energy: 0.0,
            hops: [0.0; SHORT_TERM_HOPS],
            hops_filled: 0,
            hop_next: 0,
            bin_counts: vec![0; BINS].into_boxed_slice(),
            bin_energy: vec![0.0; BINS].into_boxed_slice(),
            range_counts: vec![0; BINS].into_boxed_slice(),
            range_energy: vec![0.0; BINS].into_boxed_slice(),
            measured_hops: 0,
            partial_hop: false,
            interval_peak: 0.0,
            max_peak: 0.0,
        })
    }

    /// The channels the meter measures.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Forgets everything measured: the windows, the integrated history,
    /// and both true peaks.
    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.shelf = BiquadState::default();
            channel.high_pass = BiquadState::default();
            channel.history = [0.0; 2 * TAPS_PER_PHASE];
        }
        self.position = 0;
        self.hop_count = 0;
        self.hop_energy = 0.0;
        self.hops = [0.0; SHORT_TERM_HOPS];
        self.hops_filled = 0;
        self.hop_next = 0;
        self.interval_peak = 0.0;
        self.reset_measurement();
    }

    /// The operator reset: starts the integrated loudness, the loudness
    /// range, and the maximum true peak over. Momentary and short-term
    /// loudness keep running; integrated loudness counts only gating blocks
    /// and the loudness range only short-term values that begin after the
    /// reset.
    pub fn reset_measurement(&mut self) {
        self.bin_counts.fill(0);
        self.bin_energy.fill(0.0);
        self.range_counts.fill(0);
        self.range_energy.fill(0.0);
        self.measured_hops = 0;
        self.partial_hop = self.hop_count > 0;
        self.max_peak = 0.0;
    }

    /// Measures one sample frame: one value per channel, in layout order.
    /// Missing channels read as silence and extra values are ignored; a
    /// non-finite value is measured as silence.
    pub fn push_frame(&mut self, frame: &[f32]) {
        let position = self.position;
        let mut energy = 0.0;
        let mut peak = 0.0_f32;
        for (index, channel) in self.channels.iter_mut().enumerate() {
            let sample = frame
                .get(index)
                .copied()
                .filter(|sample| sample.is_finite())
                .unwrap_or(0.0);
            let shelved = channel.shelf.process(&self.shelf, f64::from(sample));
            let weighted = channel.high_pass.process(&self.high_pass, shelved);
            energy += channel.weight * weighted * weighted;
            channel.history[position] = sample;
            channel.history[position + TAPS_PER_PHASE] = sample;
            peak = peak.max(sample.abs());
            // Newest first: the window ends at `position + TAPS_PER_PHASE`.
            let window = &channel.history[position + 1..=position + TAPS_PER_PHASE];
            for branch in &self.phases {
                let value = branch
                    .iter()
                    .zip(window.iter().rev())
                    .fold(0.0_f32, |sum, (tap, sample)| sum + tap * sample);
                peak = peak.max(value.abs());
            }
        }
        self.position = (position + 1) % TAPS_PER_PHASE;
        self.interval_peak = self.interval_peak.max(peak);
        self.max_peak = self.max_peak.max(peak);
        self.hop_energy += energy;
        self.hop_count += 1;
        if self.hop_count == self.hop_samples {
            self.finish_hop();
        }
    }

    /// Measures `samples` frames of silence.
    pub fn push_silence(&mut self, samples: usize) {
        for _ in 0..samples {
            self.push_frame(&[]);
        }
    }

    /// Measures the first `samples` samples of `planes` (one per channel).
    pub fn process_planes(&mut self, planes: &[Vec<f32>], samples: usize) {
        let mut frame = [0.0_f32; crate::MAX_CHANNELS];
        let channels = self.channels.len().min(planes.len()).min(frame.len());
        for sample in 0..samples {
            for (value, plane) in frame[..channels].iter_mut().zip(planes) {
                *value = plane.get(sample).copied().unwrap_or(0.0);
            }
            self.push_frame(&frame[..channels]);
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn finish_hop(&mut self) {
        let mean = self.hop_energy / self.hop_samples as f64;
        self.hops[self.hop_next] = mean;
        self.hop_next = (self.hop_next + 1) % SHORT_TERM_HOPS;
        self.hops_filled = (self.hops_filled + 1).min(SHORT_TERM_HOPS);
        self.hop_energy = 0.0;
        self.hop_count = 0;
        if self.partial_hop {
            // The hop began before a reset: it opens no measured window.
            self.partial_hop = false;
            return;
        }
        self.measured_hops = self.measured_hops.saturating_add(1);
        // Every completed hop closes a 400 ms gating block and a 3 s
        // short-term value; each counts once it lies wholly after a reset.
        if self.measured_hops >= MOMENTARY_HOPS
            && let Some(block) = self.window_mean(MOMENTARY_HOPS)
            && let Some(bin) = gated_bin(block)
        {
            self.bin_counts[bin] = self.bin_counts[bin].saturating_add(1);
            self.bin_energy[bin] += block;
        }
        if self.measured_hops >= SHORT_TERM_HOPS
            && let Some(short_term) = self.window_mean(SHORT_TERM_HOPS)
            && let Some(bin) = gated_bin(short_term)
        {
            self.range_counts[bin] = self.range_counts[bin].saturating_add(1);
            self.range_energy[bin] += short_term;
        }
    }

    /// The mean of the last `hops` hops, once that many have completed.
    #[allow(clippy::cast_precision_loss)]
    fn window_mean(&self, hops: usize) -> Option<f64> {
        if self.hops_filled < hops {
            return None;
        }
        let sum: f64 = (1..=hops)
            .map(|back| self.hops[(self.hop_next + SHORT_TERM_HOPS - back) % SHORT_TERM_HOPS])
            .sum();
        Some(sum / hops as f64)
    }

    /// The gated integrated loudness, in LUFS.
    #[allow(clippy::cast_precision_loss)]
    fn integrated(&self) -> Option<f64> {
        let (count, energy) = self.bin_counts.iter().zip(self.bin_energy.iter()).fold(
            (0_u64, 0.0),
            |(count, energy), (bin_count, bin_energy)| {
                (count + u64::from(*bin_count), energy + bin_energy)
            },
        );
        if count == 0 {
            return None;
        }
        let gate = loudness_of(energy / count as f64)? - RELATIVE_GATE_LU;
        let (count, energy) = self
            .bin_counts
            .iter()
            .zip(self.bin_energy.iter())
            .enumerate()
            .filter(|(bin, _)| ABSOLUTE_GATE_LUFS + (*bin as f64 + 0.5) * BIN_WIDTH_LU > gate)
            .fold(
                (0_u64, 0.0),
                |(count, energy), (_, (bin_count, bin_energy))| {
                    (count + u64::from(*bin_count), energy + bin_energy)
                },
            );
        if count == 0 {
            return None;
        }
        loudness_of(energy / count as f64)
    }

    /// The loudness range (EBU Tech 3342), in LU: the absolute-gated
    /// short-term values, gated again 20 LU below their mean, spread
    /// between the 10th and 95th percentiles.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn loudness_range(&self) -> Option<f64> {
        let (count, energy) = self.range_counts.iter().zip(self.range_energy.iter()).fold(
            (0_u64, 0.0),
            |(count, energy), (bin_count, bin_energy)| {
                (count + u64::from(*bin_count), energy + bin_energy)
            },
        );
        if count == 0 {
            return None;
        }
        let gate = loudness_of(energy / count as f64)? - RANGE_RELATIVE_GATE_LU;
        let centre = |bin: usize| ABSOLUTE_GATE_LUFS + (bin as f64 + 0.5) * BIN_WIDTH_LU;
        let first = (0..BINS).find(|bin| centre(*bin) > gate)?;
        let gated: u64 = self.range_counts[first..]
            .iter()
            .map(|count| u64::from(*count))
            .sum();
        if gated == 0 {
            return None;
        }
        // The nearest-rank percentile of the gated values, in ascending
        // order (as libebur128 and the EBU reference do).
        let rank = |percentile: f64| ((gated - 1) as f64 * percentile + 0.5) as u64;
        let value_at = |rank: u64| {
            let mut seen = 0_u64;
            for (offset, count) in self.range_counts[first..].iter().enumerate() {
                seen += u64::from(*count);
                if seen > rank {
                    return centre(first + offset);
                }
            }
            centre(BINS - 1)
        };
        let low = value_at(rank(RANGE_LOW_PERCENTILE));
        let high = value_at(rank(RANGE_HIGH_PERCENTILE));
        Some((high - low).max(0.0))
    }

    /// The current reading, leaving the interval true peak running.
    #[must_use]
    pub fn reading(&self) -> LoudnessReading {
        LoudnessReading {
            momentary_lufs: self.window_mean(MOMENTARY_HOPS).and_then(loudness_of),
            short_term_lufs: self.window_mean(SHORT_TERM_HOPS).and_then(loudness_of),
            integrated_lufs: self.integrated(),
            loudness_range_lu: self.loudness_range(),
            true_peak_dbtp: dbtp_of(self.interval_peak),
            max_true_peak_dbtp: dbtp_of(self.max_peak),
        }
    }

    /// The current reading; the interval true peak starts over.
    pub fn take_reading(&mut self) -> LoudnessReading {
        let reading = self.reading();
        self.interval_peak = 0.0;
        reading
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo() -> ChannelLayout {
        ChannelLayout::new(vec![Channel::Left, Channel::Right]).unwrap()
    }

    #[test]
    fn the_48_khz_filters_match_the_bs1770_coefficients() {
        let meter = LoudnessMeter::new(48_000, &stereo()).unwrap();
        let shelf = [
            1.535_124_859_586_97,
            -2.691_696_189_406_38,
            1.198_392_810_852_85,
        ];
        let shelf_a = [-1.690_659_293_182_41, 0.732_480_774_215_85];
        for (actual, expected) in meter.shelf.b.iter().zip(shelf) {
            assert!((actual - expected).abs() < 1e-8, "{actual} {expected}");
        }
        for (actual, expected) in meter.shelf.a.iter().zip(shelf_a) {
            assert!((actual - expected).abs() < 1e-8, "{actual} {expected}");
        }
        let high_pass_a = [-1.990_047_454_833_98, 0.990_072_250_366_21];
        for (actual, expected) in meter.high_pass.a.iter().zip(high_pass_a) {
            assert!((actual - expected).abs() < 1e-8, "{actual} {expected}");
        }
    }

    #[test]
    fn the_interpolator_branches_pass_dc_at_unity() {
        for branch in interpolator() {
            let sum: f32 = branch.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn nothing_is_reported_before_a_window_fills_or_for_silence() {
        let mut meter = LoudnessMeter::new(48_000, &stereo()).unwrap();
        assert_eq!(meter.reading(), LoudnessReading::default());
        meter.push_silence(48_000 * 4);
        assert_eq!(meter.reading(), LoudnessReading::default());
    }
}
