//! The live, click-free strip DSP chain the Master mixer runs (AU-005).
//!
//! [`LiveStripDsp`] runs one [`fm_types::strip_dsp::StripDsp`] chain
//! (`polarity -> high-pass -> 5-band EQ -> gate -> expander -> compressor ->
//! limiter`) over every
//! channel of a strip, with independent filter and detector state per
//! channel. It is built from the tested primitives in [`crate::dsp`].
//!
//! # Live edits without zipper noise
//!
//! [`LiveStripDsp::set_settings`] only retargets; nothing jumps:
//!
//! - EQ and high-pass frequencies glide in the log domain and gains and Qs
//!   glide linearly with a one-pole smoother (about 20 ms), and the filters
//!   are retuned every [`SUB_BLOCK`] samples *without* clearing their
//!   history ([`crate::Biquad::retune`]). A disabled band glides to 0 dB,
//!   where it is transparent. A band's shape (shelf or peak) changes the
//!   same way: the band glides to 0 dB, where every shape is the identity,
//!   takes its new shape there, and glides back to its gain.
//! - Dynamics thresholds, ranges, ratios, knees, makeup, and the limiter
//!   ceiling glide the same way; attack, hold, and release take effect at
//!   once (they shape the detector, not the signal).
//! - Enabling or disabling a stage, and bypassing the chain, crossfades
//!   between the stage's input and output over [`CROSSFADE_SECONDS`]. A
//!   stage at rest fully off is skipped and its state cleared, so it
//!   re-enters from silence.
//! - Inverting the polarity ramps the sign through zero over the same
//!   crossfade, so a flip mid-programme dips briefly instead of stepping.
//!
//! # Real-time safety
//!
//! [`LiveStripDsp::new`] allocates the per-channel state. Processing
//! ([`LiveStripDsp::process`]) and retargeting perform no allocation, take
//! no locks, and never panic for finite input; the working buffers live on
//! the stack, [`SUB_BLOCK`] samples at a time.
//!
//! # Gain-reduction metering
//!
//! Every processed block records the deepest gate attenuation, expander
//! attenuation, compressor gain reduction, and limiter gain reduction seen
//! on any channel
//! ([`LiveStripDsp::dynamics`]), for operators' GR meters.

use fm_types::strip_dsp::{EqBandKind, STRIP_EQ_BAND_COUNT, StripDsp as StripDspSettings};

use crate::dsp::{Biquad, BiquadCoefficients, Compressor, DspError, Expander, Gate, Limiter};

/// Samples between parameter glide steps and filter retunes.
pub const SUB_BLOCK: usize = 32;
/// Crossfade time for stage enables and the chain bypass, in seconds.
pub const CROSSFADE_SECONDS: f32 = 0.010;
/// Glide time constant for continuous parameters, in seconds.
pub const GLIDE_SECONDS: f32 = 0.020;
/// The high-pass Q (Butterworth, 12 dB/octave).
const HIGH_PASS_Q: f32 = core::f32::consts::FRAC_1_SQRT_2;

/// Peak gain reduction of each dynamics stage over the last processed block,
/// in positive dB; `None` while the stage is off or the chain bypassed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DspDynamicsReading {
    pub gate_db: Option<f32>,
    pub expander_db: Option<f32>,
    pub compressor_db: Option<f32>,
    pub limiter_db: Option<f32>,
}

/// Reads a strip's input and writes its processed output, one channel and
/// at most [`SUB_BLOCK`] samples at a time.
pub trait LiveDspIo {
    /// Fills `buffer` with channel `channel`'s input from sample `start`.
    fn read(&mut self, channel: usize, start: usize, buffer: &mut [f32]);
    /// Stores channel `channel`'s output from sample `start`.
    fn write(&mut self, channel: usize, start: usize, buffer: &[f32]);
}

/// Processes planes in place.
pub struct InPlacePlanes<'a>(pub &'a mut [Vec<f32>]);

impl LiveDspIo for InPlacePlanes<'_> {
    fn read(&mut self, channel: usize, start: usize, buffer: &mut [f32]) {
        buffer.copy_from_slice(&self.0[channel][start..start + buffer.len()]);
    }

    fn write(&mut self, channel: usize, start: usize, buffer: &[f32]) {
        self.0[channel][start..start + buffer.len()].copy_from_slice(buffer);
    }
}

/// A one-pole glide toward a target.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Glide {
    current: f32,
    target: f32,
}

impl Glide {
    const fn settled(value: f32) -> Self {
        Self {
            current: value,
            target: value,
        }
    }

    /// Steps toward the target; returns whether the value moved. A glide
    /// snaps exactly onto its target, so the equality test is exact.
    #[allow(clippy::float_cmp)]
    fn step(&mut self, coefficient: f32, epsilon: f32) -> bool {
        if self.current == self.target {
            return false;
        }
        self.current += (self.target - self.current) * coefficient;
        if (self.target - self.current).abs() <= epsilon {
            self.current = self.target;
        }
        true
    }
}

/// A linear 0..1 crossfade.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fade {
    current: f32,
    target: f32,
}

impl Fade {
    const fn settled(on: bool) -> Self {
        let value = if on { 1.0 } else { 0.0 };
        Self {
            current: value,
            target: value,
        }
    }

    const fn set(&mut self, on: bool) {
        self.target = if on { 1.0 } else { 0.0 };
    }

    /// Fully off and staying off: the stage can be skipped.
    fn is_off(self) -> bool {
        self.current == 0.0 && self.target == 0.0
    }

    /// Fills `values` with the per-sample mix and advances.
    fn fill(&mut self, step: f32, values: &mut [f32]) {
        for value in values {
            if self.current < self.target {
                self.current = (self.current + step).min(self.target);
            } else if self.current > self.target {
                self.current = (self.current - step).max(self.target);
            }
            *value = self.current;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BandGlide {
    /// The shape the band's filter has now.
    kind: EqBandKind,
    /// A new shape the band takes once its gain has glided to 0 dB.
    pending_kind: Option<EqBandKind>,
    log_frequency: Glide,
    gain_db: Glide,
    q: Glide,
}

#[derive(Clone, Copy, Debug)]
struct ChannelState {
    high_pass: Biquad,
    bands: [Biquad; STRIP_EQ_BAND_COUNT],
    gate: Gate,
    expander: Expander,
    compressor: Compressor,
    limiter: Limiter,
}

/// Stage indices into the crossfades. The chain bypass is last, so
/// `..CHAIN` is every stage.
const HIGH_PASS: usize = 0;
const EQ: usize = 1;
const GATE: usize = 2;
const EXPANDER: usize = 3;
const COMPRESSOR: usize = 4;
const LIMITER: usize = 5;
const POLARITY: usize = 6;
const CHAIN: usize = 7;
const FADES: usize = 8;

/// One strip's live DSP chain; see the module documentation.
#[derive(Clone, Debug)]
pub struct LiveStripDsp {
    sample_rate_hz: f32,
    settings: StripDspSettings,
    high_pass_log_frequency: Glide,
    bands: [BandGlide; STRIP_EQ_BAND_COUNT],
    high_pass_coefficients: BiquadCoefficients,
    band_coefficients: [BiquadCoefficients; STRIP_EQ_BAND_COUNT],
    gate_threshold: Glide,
    gate_range: Glide,
    expander_threshold: Glide,
    expander_ratio: Glide,
    expander_range: Glide,
    compressor_threshold: Glide,
    compressor_ratio: Glide,
    compressor_knee: Glide,
    compressor_makeup: Glide,
    limiter_ceiling: Glide,
    fades: [Fade; FADES],
    glide_coefficient: f32,
    fade_step: f32,
    channels: Vec<ChannelState>,
    dynamics: DspDynamicsReading,
}

fn db(millidb: i32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let value = millidb as f32;
    value / 1_000.0
}

fn udb(millidb: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let value = millidb as f32;
    value / 1_000.0
}

fn ms(micros: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let value = micros as f32;
    value / 1_000.0
}

fn milli(value: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let value = value as f32;
    value / 1_000.0
}

fn hz_log(hz: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let value = hz as f32;
    value.ln()
}

fn band_coefficients(
    sample_rate_hz: f32,
    kind: EqBandKind,
    log_frequency: f32,
    gain_db: f32,
    q: f32,
) -> Result<BiquadCoefficients, DspError> {
    let frequency = log_frequency.exp().min(sample_rate_hz * 0.45);
    match kind {
        EqBandKind::LowShelf => {
            BiquadCoefficients::low_shelf(sample_rate_hz, frequency, q, gain_db)
        }
        EqBandKind::Peak => BiquadCoefficients::peaking_eq(sample_rate_hz, frequency, q, gain_db),
        EqBandKind::HighShelf => {
            BiquadCoefficients::high_shelf(sample_rate_hz, frequency, q, gain_db)
        }
    }
}

fn high_pass_coefficients(
    sample_rate_hz: f32,
    log_frequency: f32,
) -> Result<BiquadCoefficients, DspError> {
    BiquadCoefficients::high_pass(
        sample_rate_hz,
        log_frequency.exp().min(sample_rate_hz * 0.45),
        HIGH_PASS_Q,
    )
}

/// The gain a band glides toward: its own while it and the EQ are enabled.
fn band_target_gain(settings: &StripDspSettings, band: usize) -> f32 {
    let band = &settings.eq.bands[band];
    if band.enabled {
        db(band.gain_millidb)
    } else {
        0.0
    }
}

impl LiveStripDsp {
    /// Creates a chain settled at `settings` for `channels` channels.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidSampleRate`] for an unusable rate,
    /// [`DspError::ChannelCountOutOfRange`] unless `channels` is in
    /// `1..=`[`crate::MAX_CHANNELS`], or a parameter error for an
    /// out-of-range chain (validate it first).
    pub fn new(
        sample_rate_hz: f32,
        channels: usize,
        settings: &StripDspSettings,
    ) -> Result<Self, DspError> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(DspError::InvalidSampleRate(sample_rate_hz));
        }
        if !(1..=crate::MAX_CHANNELS).contains(&channels) {
            return Err(DspError::ChannelCountOutOfRange {
                actual: channels,
                maximum: crate::MAX_CHANNELS,
            });
        }
        let high_pass_log_frequency = Glide::settled(hz_log(settings.high_pass.frequency_hz));
        let bands = core::array::from_fn(|index| {
            let band = &settings.eq.bands[index];
            BandGlide {
                kind: band.kind,
                pending_kind: None,
                log_frequency: Glide::settled(hz_log(band.frequency_hz)),
                gain_db: Glide::settled(band_target_gain(settings, index)),
                q: Glide::settled(milli(band.q_milli)),
            }
        });
        let high_pass_coefficients =
            high_pass_coefficients(sample_rate_hz, high_pass_log_frequency.current)?;
        let mut band_coefficients = [BiquadCoefficients::identity(); STRIP_EQ_BAND_COUNT];
        for (coefficients, band) in band_coefficients.iter_mut().zip(&bands) {
            *coefficients = band_coefficients_of(sample_rate_hz, band)?;
        }
        let gate = &settings.gate;
        let expander = &settings.expander;
        let compressor = &settings.compressor;
        let limiter = &settings.limiter;
        let channel = ChannelState {
            high_pass: Biquad::new(high_pass_coefficients),
            bands: band_coefficients.map(Biquad::new),
            gate: Gate::new(
                sample_rate_hz,
                db(gate.threshold_millidb),
                udb(gate.range_millidb),
                ms(gate.attack_us),
                ms(gate.hold_us),
                ms(gate.release_us),
            )?,
            expander: Expander::new(
                sample_rate_hz,
                db(expander.threshold_millidb),
                milli(expander.ratio_milli),
                ms(expander.attack_us),
                ms(expander.release_us),
                udb(expander.range_millidb),
            )?,
            compressor: Compressor::new(
                sample_rate_hz,
                db(compressor.threshold_millidb),
                milli(compressor.ratio_milli),
                ms(compressor.attack_us),
                ms(compressor.release_us),
                udb(compressor.knee_millidb),
                udb(compressor.makeup_millidb),
            )?,
            limiter: Limiter::new(
                sample_rate_hz,
                db(limiter.ceiling_millidb),
                ms(limiter.release_us),
            )?,
        };
        #[allow(clippy::cast_precision_loss)]
        let sub_block = SUB_BLOCK as f32;
        let mut dsp = Self {
            sample_rate_hz,
            settings: *settings,
            high_pass_log_frequency,
            bands,
            high_pass_coefficients,
            band_coefficients,
            gate_threshold: Glide::settled(db(gate.threshold_millidb)),
            gate_range: Glide::settled(udb(gate.range_millidb)),
            expander_threshold: Glide::settled(db(expander.threshold_millidb)),
            expander_ratio: Glide::settled(milli(expander.ratio_milli)),
            expander_range: Glide::settled(udb(expander.range_millidb)),
            compressor_threshold: Glide::settled(db(compressor.threshold_millidb)),
            compressor_ratio: Glide::settled(milli(compressor.ratio_milli)),
            compressor_knee: Glide::settled(udb(compressor.knee_millidb)),
            compressor_makeup: Glide::settled(udb(compressor.makeup_millidb)),
            limiter_ceiling: Glide::settled(db(limiter.ceiling_millidb)),
            fades: [Fade::settled(false); FADES],
            glide_coefficient: 1.0 - (-sub_block / (GLIDE_SECONDS * sample_rate_hz)).exp(),
            fade_step: 1.0 / (CROSSFADE_SECONDS * sample_rate_hz).max(1.0),
            channels: vec![channel; channels],
            dynamics: DspDynamicsReading::default(),
        };
        for (fade, on) in dsp.fades.iter_mut().zip(stage_switches(settings)) {
            *fade = Fade::settled(on);
        }
        Ok(dsp)
    }

    #[must_use]
    pub const fn settings(&self) -> &StripDspSettings {
        &self.settings
    }

    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> f32 {
        self.sample_rate_hz
    }

    /// The peak gain reduction of the last processed block.
    #[must_use]
    pub const fn dynamics(&self) -> DspDynamicsReading {
        self.dynamics
    }

    /// Whether processing changes nothing and nothing is in flight: the
    /// chain is fully bypassed or every stage is fully off.
    #[must_use]
    pub fn is_transparent(&self) -> bool {
        self.fades[CHAIN].is_off() || self.fades[..CHAIN].iter().all(|fade| fade.is_off())
    }

    /// Retargets the chain; changes glide or crossfade (see the module
    /// documentation). Performs no allocation.
    ///
    /// # Errors
    ///
    /// Returns a parameter error for an out-of-range chain; nothing changes.
    pub fn set_settings(&mut self, settings: &StripDspSettings) -> Result<(), DspError> {
        // Validate every detector setting before touching any channel.
        let gate = &settings.gate;
        let expander = &settings.expander;
        let compressor = &settings.compressor;
        let limiter = &settings.limiter;
        let mut probe = self
            .channels
            .first()
            .copied()
            .ok_or(DspError::ChannelCountOutOfRange {
                actual: 0,
                maximum: crate::MAX_CHANNELS,
            })?;
        probe.gate.set_attack_ms(ms(gate.attack_us))?;
        probe.gate.set_hold_ms(ms(gate.hold_us))?;
        probe.gate.set_release_ms(ms(gate.release_us))?;
        probe.gate.set_threshold_db(db(gate.threshold_millidb))?;
        probe.gate.set_range_db(udb(gate.range_millidb))?;
        probe.expander.set_attack_ms(ms(expander.attack_us))?;
        probe.expander.set_release_ms(ms(expander.release_us))?;
        probe
            .expander
            .set_threshold_db(db(expander.threshold_millidb))?;
        probe.expander.set_ratio(milli(expander.ratio_milli))?;
        probe.expander.set_range_db(udb(expander.range_millidb))?;
        probe.compressor.set_attack_ms(ms(compressor.attack_us))?;
        probe.compressor.set_release_ms(ms(compressor.release_us))?;
        probe
            .compressor
            .set_threshold_db(db(compressor.threshold_millidb))?;
        probe.compressor.set_ratio(milli(compressor.ratio_milli))?;
        probe.compressor.set_knee_db(udb(compressor.knee_millidb))?;
        probe
            .compressor
            .set_makeup_gain_db(udb(compressor.makeup_millidb))?;
        probe.limiter.set_release_ms(ms(limiter.release_us))?;
        probe.limiter.set_ceiling_db(db(limiter.ceiling_millidb))?;
        for (index, band) in settings.eq.bands.iter().enumerate() {
            band_coefficients(
                self.sample_rate_hz,
                band.kind,
                hz_log(band.frequency_hz),
                band_target_gain(settings, index),
                milli(band.q_milli),
            )?;
        }
        high_pass_coefficients(self.sample_rate_hz, hz_log(settings.high_pass.frequency_hz))?;

        for channel in &mut self.channels {
            // Validated above; these cannot fail.
            let _ = channel.gate.set_attack_ms(ms(gate.attack_us));
            let _ = channel.gate.set_hold_ms(ms(gate.hold_us));
            let _ = channel.gate.set_release_ms(ms(gate.release_us));
            let _ = channel.expander.set_attack_ms(ms(expander.attack_us));
            let _ = channel.expander.set_release_ms(ms(expander.release_us));
            let _ = channel.compressor.set_attack_ms(ms(compressor.attack_us));
            let _ = channel.compressor.set_release_ms(ms(compressor.release_us));
            let _ = channel.limiter.set_release_ms(ms(limiter.release_us));
        }
        self.high_pass_log_frequency.target = hz_log(settings.high_pass.frequency_hz);
        for (index, (glide, band)) in self.bands.iter_mut().zip(&settings.eq.bands).enumerate() {
            glide.log_frequency.target = hz_log(band.frequency_hz);
            glide.q.target = milli(band.q_milli);
            // Every shape is the identity at 0 dB: a band already there
            // takes its new shape at once, any other glides there first.
            #[allow(clippy::float_cmp)]
            let transparent = glide.gain_db.current == 0.0;
            if band.kind == glide.kind || transparent {
                glide.kind = band.kind;
                glide.pending_kind = None;
                glide.gain_db.target = band_target_gain(settings, index);
            } else {
                glide.pending_kind = Some(band.kind);
                glide.gain_db.target = 0.0;
            }
        }
        self.gate_threshold.target = db(gate.threshold_millidb);
        self.gate_range.target = udb(gate.range_millidb);
        self.expander_threshold.target = db(expander.threshold_millidb);
        self.expander_ratio.target = milli(expander.ratio_milli);
        self.expander_range.target = udb(expander.range_millidb);
        self.compressor_threshold.target = db(compressor.threshold_millidb);
        self.compressor_ratio.target = milli(compressor.ratio_milli);
        self.compressor_knee.target = udb(compressor.knee_millidb);
        self.compressor_makeup.target = udb(compressor.makeup_millidb);
        self.limiter_ceiling.target = db(limiter.ceiling_millidb);
        for (fade, on) in self.fades.iter_mut().zip(stage_switches(settings)) {
            fade.set(on);
        }
        // Retune a shape taken at once so the coefficients match it.
        for (coefficients, band) in self.band_coefficients.iter_mut().zip(&self.bands) {
            if let Ok(updated) = band_coefficients_of(self.sample_rate_hz, band) {
                *coefficients = updated;
            }
        }
        for channel in &mut self.channels {
            for (filter, coefficients) in channel.bands.iter_mut().zip(&self.band_coefficients) {
                filter.retune(*coefficients);
            }
        }
        self.settings = *settings;
        Ok(())
    }

    /// Clears every filter and detector history and settles every glide and
    /// crossfade at its target, keeping the settings.
    pub fn reset(&mut self) {
        for glide in [
            &mut self.high_pass_log_frequency,
            &mut self.gate_threshold,
            &mut self.gate_range,
            &mut self.expander_threshold,
            &mut self.expander_ratio,
            &mut self.expander_range,
            &mut self.compressor_threshold,
            &mut self.compressor_ratio,
            &mut self.compressor_knee,
            &mut self.compressor_makeup,
            &mut self.limiter_ceiling,
        ] {
            glide.current = glide.target;
        }
        for (index, band) in self.bands.iter_mut().enumerate() {
            if let Some(kind) = band.pending_kind.take() {
                band.kind = kind;
                band.gain_db.target = band_target_gain(&self.settings, index);
            }
            band.log_frequency.current = band.log_frequency.target;
            band.gain_db.current = band.gain_db.target;
            band.q.current = band.q.target;
        }
        for fade in &mut self.fades {
            fade.current = fade.target;
        }
        self.apply_glides(true);
        for channel in &mut self.channels {
            channel.high_pass.reset();
            for band in &mut channel.bands {
                band.reset();
            }
            channel.gate.reset();
            channel.expander.reset();
            channel.compressor.reset();
            channel.limiter.reset();
        }
        self.dynamics = DspDynamicsReading::default();
    }

    /// Copies the complete runtime state of an identically shaped chain
    /// without allocating.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::ChannelCountMismatch`] when the channel counts
    /// differ; nothing changes.
    pub fn copy_state_from(&mut self, other: &Self) -> Result<(), DspError> {
        if self.channels.len() != other.channels.len() {
            return Err(DspError::ChannelCountMismatch {
                expected: self.channels.len(),
                actual: other.channels.len(),
            });
        }
        self.sample_rate_hz = other.sample_rate_hz;
        self.settings = other.settings;
        self.high_pass_log_frequency = other.high_pass_log_frequency;
        self.bands = other.bands;
        self.high_pass_coefficients = other.high_pass_coefficients;
        self.band_coefficients = other.band_coefficients;
        self.gate_threshold = other.gate_threshold;
        self.gate_range = other.gate_range;
        self.expander_threshold = other.expander_threshold;
        self.expander_ratio = other.expander_ratio;
        self.expander_range = other.expander_range;
        self.compressor_threshold = other.compressor_threshold;
        self.compressor_ratio = other.compressor_ratio;
        self.compressor_knee = other.compressor_knee;
        self.compressor_makeup = other.compressor_makeup;
        self.limiter_ceiling = other.limiter_ceiling;
        self.fades = other.fades;
        self.glide_coefficient = other.glide_coefficient;
        self.fade_step = other.fade_step;
        self.channels.copy_from_slice(&other.channels);
        self.dynamics = other.dynamics;
        Ok(())
    }

    /// The gliding dynamics parameters now, in [`set_dynamics`] order.
    const fn dynamics_values(&self) -> [f32; DYNAMICS] {
        [
            self.gate_threshold.current,
            self.gate_range.current,
            self.expander_threshold.current,
            self.expander_ratio.current,
            self.expander_range.current,
            self.compressor_threshold.current,
            self.compressor_ratio.current,
            self.compressor_knee.current,
            self.compressor_makeup.current,
            self.limiter_ceiling.current,
        ]
    }

    /// Steps every glide one sub-block of `length` samples and returns the
    /// moves to spread over it.
    fn glide(&mut self, length: usize) -> Ramps {
        let high_pass_from = self.high_pass_coefficients;
        let bands_from = self.band_coefficients;
        let dynamics_from = self.dynamics_values();
        self.apply_glides(false);
        let dynamics_to = self.dynamics_values();
        let high_pass = (high_pass_from != self.high_pass_coefficients)
            .then_some((high_pass_from, self.high_pass_coefficients));
        let bands = core::array::from_fn(|band| {
            (bands_from[band] != self.band_coefficients[band])
                .then_some((bands_from[band], self.band_coefficients[band]))
        });
        #[allow(clippy::float_cmp)]
        let dynamics = dynamics_from != dynamics_to;
        Ramps {
            length,
            high_pass,
            filters: high_pass.is_some() || bands.iter().any(Option::is_some),
            bands,
            dynamics_from,
            dynamics_to,
            dynamics,
        }
    }

    /// Steps every glide one sub-block and retunes what moved (`force`
    /// retunes everything).
    fn apply_glides(&mut self, force: bool) {
        let coefficient = self.glide_coefficient;
        if (self.high_pass_log_frequency.step(coefficient, 1.0e-4) || force)
            && let Ok(updated) =
                high_pass_coefficients(self.sample_rate_hz, self.high_pass_log_frequency.current)
        {
            self.high_pass_coefficients = updated;
            for channel in &mut self.channels {
                channel.high_pass.retune(updated);
            }
        }
        for index in 0..STRIP_EQ_BAND_COUNT {
            let band = &mut self.bands[index];
            let mut moved = band.log_frequency.step(coefficient, 1.0e-4)
                | band.gain_db.step(coefficient, 1.0e-3)
                | band.q.step(coefficient, 1.0e-4);
            // At 0 dB (where every shape is the identity) a pending shape is
            // taken and the band glides back to its gain.
            #[allow(clippy::float_cmp)]
            if band.gain_db.current == 0.0
                && let Some(kind) = band.pending_kind.take()
            {
                band.kind = kind;
                band.gain_db.target = band_target_gain(&self.settings, index);
                moved = true;
            }
            if (moved || force)
                && let Ok(updated) = band_coefficients_of(self.sample_rate_hz, band)
            {
                self.band_coefficients[index] = updated;
                for channel in &mut self.channels {
                    channel.bands[index].retune(updated);
                }
            }
        }
        if self.gate_threshold.step(coefficient, 1.0e-3) || force {
            let value = self.gate_threshold.current;
            for channel in &mut self.channels {
                let _ = channel.gate.set_threshold_db(value);
            }
        }
        if self.gate_range.step(coefficient, 1.0e-3) || force {
            let value = self.gate_range.current;
            for channel in &mut self.channels {
                let _ = channel.gate.set_range_db(value);
            }
        }
        if self.expander_threshold.step(coefficient, 1.0e-3) || force {
            let value = self.expander_threshold.current;
            for channel in &mut self.channels {
                let _ = channel.expander.set_threshold_db(value);
            }
        }
        if self.expander_ratio.step(coefficient, 1.0e-4) || force {
            let value = self.expander_ratio.current;
            for channel in &mut self.channels {
                let _ = channel.expander.set_ratio(value);
            }
        }
        if self.expander_range.step(coefficient, 1.0e-3) || force {
            let value = self.expander_range.current;
            for channel in &mut self.channels {
                let _ = channel.expander.set_range_db(value);
            }
        }
        if self.compressor_threshold.step(coefficient, 1.0e-3) || force {
            let value = self.compressor_threshold.current;
            for channel in &mut self.channels {
                let _ = channel.compressor.set_threshold_db(value);
            }
        }
        if self.compressor_ratio.step(coefficient, 1.0e-4) || force {
            let value = self.compressor_ratio.current;
            for channel in &mut self.channels {
                let _ = channel.compressor.set_ratio(value);
            }
        }
        if self.compressor_knee.step(coefficient, 1.0e-3) || force {
            let value = self.compressor_knee.current;
            for channel in &mut self.channels {
                let _ = channel.compressor.set_knee_db(value);
            }
        }
        if self.compressor_makeup.step(coefficient, 1.0e-3) || force {
            let value = self.compressor_makeup.current;
            for channel in &mut self.channels {
                let _ = channel.compressor.set_makeup_gain_db(value);
            }
        }
        if self.limiter_ceiling.step(coefficient, 1.0e-3) || force {
            let value = self.limiter_ceiling.current;
            for channel in &mut self.channels {
                let _ = channel.limiter.set_ceiling_db(value);
            }
        }
    }

    /// Processes `samples` samples of every channel through `io`.
    ///
    /// Performs no allocation. A fully transparent chain copies input to
    /// output unchanged.
    pub fn process(&mut self, samples: usize, io: &mut impl LiveDspIo) {
        let mut reading = [0.0_f32; 4];
        let mut buffer = [0.0_f32; SUB_BLOCK];
        let mut mixes = [[0.0_f32; SUB_BLOCK]; FADES];
        let mut start = 0;
        while start < samples {
            let length = SUB_BLOCK.min(samples - start);
            let ramps = self.glide(length);
            // A stage fully off is skipped; it re-enters with clear state.
            let mut running = [false; FADES];
            for (stage, fade) in self.fades.iter_mut().enumerate() {
                running[stage] = !fade.is_off();
                if running[stage] && fade.current == 0.0 {
                    for channel in &mut self.channels {
                        match stage {
                            HIGH_PASS => channel.high_pass.reset(),
                            EQ => channel.bands.iter_mut().for_each(Biquad::reset),
                            GATE => channel.gate.reset(),
                            EXPANDER => channel.expander.reset(),
                            COMPRESSOR => channel.compressor.reset(),
                            LIMITER => channel.limiter.reset(),
                            _ => {}
                        }
                    }
                }
                fade.fill(self.fade_step, &mut mixes[stage][..length]);
            }
            let chain_running = running[CHAIN] && running[..CHAIN].iter().any(|on| *on);
            for (index, channel) in self.channels.iter_mut().enumerate() {
                let buffer = &mut buffer[..length];
                io.read(index, start, buffer);
                if chain_running {
                    for (offset, sample) in buffer.iter_mut().enumerate() {
                        let dry = *sample;
                        let mut value = dry;
                        ramps.apply(channel, offset);
                        if running[POLARITY] {
                            // Ramps the sign: +1 at rest normal, -1 inverted.
                            value *= 1.0 - 2.0 * mixes[POLARITY][offset];
                        }
                        if running[HIGH_PASS] {
                            let wet = channel.high_pass.process(value);
                            value += mixes[HIGH_PASS][offset] * (wet - value);
                        }
                        if running[EQ] {
                            let wet = channel
                                .bands
                                .iter_mut()
                                .fold(value, |value, band| band.process(value));
                            value += mixes[EQ][offset] * (wet - value);
                        }
                        if running[GATE] {
                            let wet = channel.gate.process(value);
                            value += mixes[GATE][offset] * (wet - value);
                        }
                        if running[EXPANDER] {
                            let wet = channel.expander.process(value);
                            value += mixes[EXPANDER][offset] * (wet - value);
                        }
                        if running[COMPRESSOR] {
                            let wet = channel.compressor.process(value);
                            value += mixes[COMPRESSOR][offset] * (wet - value);
                        }
                        if running[LIMITER] {
                            let wet = channel.limiter.process(value);
                            value += mixes[LIMITER][offset] * (wet - value);
                        }
                        *sample = dry + mixes[CHAIN][offset] * (value - dry);
                    }
                    if running[GATE] {
                        reading[0] = reading[0].max(channel.gate.current_attenuation_db());
                    }
                    if running[EXPANDER] {
                        reading[1] = reading[1].max(channel.expander.current_gain_reduction_db());
                    }
                    if running[COMPRESSOR] {
                        reading[2] = reading[2].max(channel.compressor.current_gain_reduction_db());
                    }
                    if running[LIMITER] {
                        reading[3] = reading[3].max(channel.limiter.current_gain_reduction_db());
                    }
                }
                io.write(index, start, buffer);
            }
            start += length;
        }
        let on = |stage: usize| self.fades[CHAIN].target > 0.0 && self.fades[stage].target > 0.0;
        self.dynamics = DspDynamicsReading {
            gate_db: on(GATE).then_some(reading[0]),
            expander_db: on(EXPANDER).then_some(reading[1]),
            compressor_db: on(COMPRESSOR).then_some(reading[2]),
            limiter_db: on(LIMITER).then_some(reading[3]),
        };
    }
}

/// One sub-block's parameter moves, spread sample by sample: a filter
/// retuned by a glide blends from its previous coefficients, and a gliding
/// dynamics parameter moves linearly, because a step at the sub-block
/// boundary would kink the waveform (zipper noise).
struct Ramps {
    length: usize,
    high_pass: Option<(BiquadCoefficients, BiquadCoefficients)>,
    bands: [Option<(BiquadCoefficients, BiquadCoefficients)>; STRIP_EQ_BAND_COUNT],
    filters: bool,
    dynamics_from: [f32; DYNAMICS],
    dynamics_to: [f32; DYNAMICS],
    dynamics: bool,
}

impl Ramps {
    /// Sets `channel`'s parameters for sample `offset` of the sub-block;
    /// the last sample lands exactly on the glides' new values.
    fn apply(&self, channel: &mut ChannelState, offset: usize) {
        #[allow(clippy::cast_precision_loss)]
        let fraction = (offset + 1) as f64 / self.length as f64;
        if self.filters {
            if let Some((from, to)) = self.high_pass {
                channel.high_pass.retune(from.blend(to, fraction));
            }
            for (filter, ramp) in channel.bands.iter_mut().zip(&self.bands) {
                if let Some((from, to)) = ramp {
                    filter.retune(from.blend(*to, fraction));
                }
            }
        }
        if self.dynamics {
            #[allow(clippy::cast_possible_truncation)]
            let fraction = fraction as f32;
            for (parameter, (from, to)) in
                self.dynamics_from.iter().zip(&self.dynamics_to).enumerate()
            {
                #[allow(clippy::float_cmp)]
                if from != to {
                    let value = if offset + 1 == self.length {
                        *to
                    } else {
                        from + (to - from) * fraction
                    };
                    set_dynamics(channel, parameter, value);
                }
            }
        }
    }
}

/// How many dynamics parameters glide.
const DYNAMICS: usize = 10;

/// Sets gliding dynamics parameter `index` (in
/// [`LiveStripDsp::dynamics_values`] order) on one channel. Values between
/// two validated settings are in range, so the setters cannot fail.
fn set_dynamics(channel: &mut ChannelState, index: usize, value: f32) {
    let _ = match index {
        0 => channel.gate.set_threshold_db(value),
        1 => channel.gate.set_range_db(value),
        2 => channel.expander.set_threshold_db(value),
        3 => channel.expander.set_ratio(value),
        4 => channel.expander.set_range_db(value),
        5 => channel.compressor.set_threshold_db(value),
        6 => channel.compressor.set_ratio(value),
        7 => channel.compressor.set_knee_db(value),
        8 => channel.compressor.set_makeup_gain_db(value),
        _ => channel.limiter.set_ceiling_db(value),
    };
}

fn band_coefficients_of(
    sample_rate_hz: f32,
    band: &BandGlide,
) -> Result<BiquadCoefficients, DspError> {
    band_coefficients(
        sample_rate_hz,
        band.kind,
        band.log_frequency.current,
        band.gain_db.current,
        band.q.current,
    )
}

/// Each crossfade's target: high-pass, EQ, gate, expander, compressor,
/// limiter, polarity, chain.
const fn stage_switches(settings: &StripDspSettings) -> [bool; FADES] {
    [
        settings.high_pass.enabled,
        settings.eq.enabled,
        settings.gate.enabled,
        settings.expander.enabled,
        settings.compressor.enabled,
        settings.limiter.enabled,
        settings.invert_polarity,
        !settings.bypass,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_types::strip_dsp::StripDspPreset;

    fn sine(frequency: f32, amplitude: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|index| {
                #[allow(clippy::cast_precision_loss)]
                let time = index as f32 / 48_000.0;
                amplitude * (core::f32::consts::TAU * frequency * time).sin()
            })
            .collect()
    }

    fn peak(plane: &[f32]) -> f32 {
        plane
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn default_chain_is_transparent() {
        let mut dsp = LiveStripDsp::new(48_000.0, 2, &StripDspSettings::DEFAULT).unwrap();
        assert!(dsp.is_transparent());
        let input = sine(440.0, 0.5, 4_800);
        let mut planes = vec![input.clone(), input.clone()];
        dsp.process(4_800, &mut InPlacePlanes(&mut planes));
        assert_eq!(planes[0], input);
        assert_eq!(dsp.dynamics(), DspDynamicsReading::default());
    }

    #[test]
    fn eq_boost_glides_without_a_step() {
        let mut dsp = LiveStripDsp::new(48_000.0, 1, &StripDspSettings::DEFAULT).unwrap();
        let mut settings = StripDspSettings::DEFAULT;
        settings.eq.enabled = true;
        settings.eq.bands[2].gain_millidb = 12_000;
        dsp.set_settings(&settings).unwrap();
        let input = sine(1_000.0, 0.1, 48_000);
        let mut planes = vec![input];
        dsp.process(48_000, &mut InPlacePlanes(&mut planes));
        // Settled: +12 dB at the band centre (x3.98).
        let settled = peak(&planes[0][40_000..]);
        assert!((settled / 0.1 - 3.98).abs() < 0.1, "{settled}");
        // No sample-to-sample jump larger than the sine's own slope allows.
        let slope = planes[0]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            slope < 0.4 * core::f32::consts::TAU * 1_000.0 / 48_000.0 * 1.2,
            "{slope}"
        );
    }

    #[test]
    fn changing_a_band_shape_mid_stream_does_not_click() {
        let mut settings = StripDspSettings::DEFAULT;
        settings.eq.enabled = true;
        for band in &mut settings.eq.bands {
            band.enabled = false;
        }
        settings.eq.bands[0].enabled = true;
        settings.eq.bands[0].kind = EqBandKind::LowShelf;
        settings.eq.bands[0].frequency_hz = 200;
        settings.eq.bands[0].gain_millidb = 18_000;
        let mut dsp = LiveStripDsp::new(48_000.0, 1, &settings).unwrap();
        let input = sine(60.0, 0.05, 96_000);
        let mut planes = vec![input[..48_000].to_vec()];
        dsp.process(48_000, &mut InPlacePlanes(&mut planes));
        let settled = peak(&planes[0][40_000..]);
        assert!(settled > 0.3, "the low shelf boosts: {settled}");

        settings.eq.bands[0].kind = EqBandKind::HighShelf;
        dsp.set_settings(&settings).unwrap();
        let mut after = vec![input[48_000..].to_vec()];
        dsp.process(48_000, &mut InPlacePlanes(&mut after));
        // Around the edit, no step beyond the boosted sine's own steepest.
        let mut joined = planes[0][47_999..].to_vec();
        joined.extend_from_slice(&after[0][..256]);
        let natural = settled * core::f32::consts::TAU * 60.0 / 48_000.0;
        let slope = joined
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(slope < natural * 1.2, "{slope} vs {natural}");
        // Settled on the new shape: a high shelf leaves 60 Hz alone.
        let settled = peak(&after[0][40_000..]);
        assert!((settled - 0.05).abs() < 0.01, "{settled}");
    }

    #[test]
    fn a_shelf_gain_glide_has_no_sub_block_steps() {
        for kind in [EqBandKind::LowShelf, EqBandKind::HighShelf] {
            let mut settings = StripDspSettings::DEFAULT;
            settings.eq.enabled = true;
            for band in &mut settings.eq.bands {
                band.enabled = false;
            }
            settings.eq.bands[0].enabled = true;
            settings.eq.bands[0].kind = kind;
            settings.eq.bands[0].frequency_hz = 200;
            let mut dsp = LiveStripDsp::new(48_000.0, 1, &settings).unwrap();
            let input = sine(60.0, 0.05, 48_000);
            let mut planes = vec![input[..4_800].to_vec()];
            dsp.process(4_800, &mut InPlacePlanes(&mut planes));
            settings.eq.bands[0].gain_millidb = 18_000;
            dsp.set_settings(&settings).unwrap();
            let mut after = vec![input[4_800..].to_vec()];
            dsp.process(43_200, &mut InPlacePlanes(&mut after));
            let mut joined = planes[0].clone();
            joined.extend_from_slice(&after[0]);
            // A sine's second difference is at most a * w^2; a coefficient
            // step shows up as a far larger kink.
            let omega = core::f32::consts::TAU * 60.0 / 48_000.0;
            let bound = 10.0 * peak(&joined) * omega * omega;
            let kink = joined
                .windows(3)
                .map(|three| (three[2] - 2.0 * three[1] + three[0]).abs())
                .fold(0.0_f32, f32::max);
            assert!(kink < bound, "{kind:?}: {kink} vs {bound}");
        }
    }

    #[test]
    fn dynamics_glides_have_no_sub_block_steps() {
        type Edit = fn(&mut StripDspSettings);
        let edits: [(&str, Edit); 4] = [
            ("makeup", |settings| {
                settings.compressor.makeup_millidb = 18_000;
            }),
            ("threshold", |settings| {
                settings.compressor.threshold_millidb = -40_000;
            }),
            ("ceiling", |settings| {
                settings.limiter.ceiling_millidb = -30_000;
            }),
            ("gate range", |settings| settings.gate.range_millidb = 0),
        ];
        for (name, edit) in edits {
            let mut settings = StripDspSettings::DEFAULT;
            settings.compressor.enabled = true;
            settings.compressor.threshold_millidb = -20_000;
            settings.compressor.knee_millidb = 0;
            settings.limiter.enabled = true;
            settings.limiter.ceiling_millidb = -6_000;
            settings.gate.enabled = true;
            settings.gate.threshold_millidb = 0;
            settings.gate.range_millidb = 40_000;
            let mut dsp = LiveStripDsp::new(48_000.0, 1, &settings).unwrap();
            let input = sine(60.0, 0.4, 48_000);
            let mut planes = vec![input[..24_000].to_vec()];
            dsp.process(24_000, &mut InPlacePlanes(&mut planes));
            edit(&mut settings);
            dsp.set_settings(&settings).unwrap();
            let mut after = vec![input[24_000..].to_vec()];
            dsp.process(24_000, &mut InPlacePlanes(&mut after));
            let mut joined = planes[0][20_000..].to_vec();
            joined.extend_from_slice(&after[0]);
            let omega = core::f32::consts::TAU * 60.0 / 48_000.0;
            let bound = 10.0 * peak(&joined) * omega * omega;
            let kink = joined
                .windows(3)
                .map(|three| (three[2] - 2.0 * three[1] + three[0]).abs())
                .fold(0.0_f32, f32::max);
            assert!(kink < bound, "{name}: {kink} vs {bound}");
        }
    }

    #[test]
    fn compressor_and_limiter_report_gain_reduction() {
        let settings = StripDspPreset::Master.chain();
        let mut dsp = LiveStripDsp::new(48_000.0, 1, &settings).unwrap();
        let mut planes = vec![sine(200.0, 0.99, 9_600)];
        dsp.process(9_600, &mut InPlacePlanes(&mut planes));
        let reading = dsp.dynamics();
        assert!(reading.compressor_db.unwrap() > 1.0);
        assert!(reading.limiter_db.is_some());
        assert!(reading.gate_db.is_none());
        assert!(reading.expander_db.is_none());
        assert!(peak(&planes[0]) <= 10.0_f32.powf(-1.0 / 20.0) + 1.0e-6);
    }

    #[test]
    fn polarity_invert_ramps_through_zero_and_settles_exactly_negated() {
        let mut dsp = LiveStripDsp::new(48_000.0, 2, &StripDspSettings::DEFAULT).unwrap();
        let mut settings = StripDspSettings::DEFAULT;
        settings.invert_polarity = true;
        dsp.set_settings(&settings).unwrap();
        assert!(!dsp.is_transparent());
        let input = vec![0.5_f32; 4_800];
        let mut planes = vec![input.clone(), input];
        dsp.process(4_800, &mut InPlacePlanes(&mut planes));
        // The flip ramps over the 10 ms crossfade (480 samples) ...
        let largest_step = planes[0]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(largest_step < 0.01, "{largest_step}");
        // ... and then is the exact negation on every channel.
        for plane in &planes {
            assert!(
                plane[600..]
                    .iter()
                    .all(|sample| sample.to_bits() == (-0.5_f32).to_bits())
            );
        }
        assert_eq!(dsp.dynamics(), DspDynamicsReading::default());
    }

    #[test]
    fn expander_turns_quiet_signals_down_by_ratio_within_range() {
        let mut settings = StripDspSettings::DEFAULT;
        settings.expander.enabled = true;
        settings.expander.threshold_millidb = -30_000;
        settings.expander.ratio_milli = 2_000;
        settings.expander.range_millidb = 12_000;
        // Loud material above the threshold passes untouched.
        let mut dsp = LiveStripDsp::new(48_000.0, 1, &settings).unwrap();
        let mut planes = vec![sine(1_000.0, 0.5, 9_600)];
        dsp.process(9_600, &mut InPlacePlanes(&mut planes));
        let loud = peak(&planes[0][4_800..]);
        assert!((loud - 0.5).abs() < 0.01, "{loud}");
        assert!(dsp.dynamics().expander_db.unwrap() < 0.1);
        // 6 dB under the threshold at 2:1 is turned down by about 6 dB more.
        let mut dsp = LiveStripDsp::new(48_000.0, 1, &settings).unwrap();
        let amplitude = 10.0_f32.powf(-36.0 / 20.0);
        let mut planes = vec![sine(1_000.0, amplitude, 48_000)];
        dsp.process(48_000, &mut InPlacePlanes(&mut planes));
        let gain_db = 20.0 * (peak(&planes[0][40_000..]) / amplitude).log10();
        assert!((-7.5..=-5.0).contains(&gain_db), "{gain_db}");
        // The meter reads the deepest attenuation of the last block; a
        // settled block reads the steady state.
        let mut settled = vec![sine(1_000.0, amplitude, 48_000)[..4_800].to_vec()];
        dsp.process(4_800, &mut InPlacePlanes(&mut settled));
        let reading = dsp.dynamics().expander_db.unwrap();
        assert!((5.0..=8.0).contains(&reading), "{reading}");
        // Far below the threshold the range caps the attenuation.
        let mut dsp = LiveStripDsp::new(48_000.0, 1, &settings).unwrap();
        let amplitude = 10.0_f32.powf(-70.0 / 20.0);
        let mut planes = vec![sine(1_000.0, amplitude, 48_000)];
        dsp.process(48_000, &mut InPlacePlanes(&mut planes));
        let gain_db = 20.0 * (peak(&planes[0][40_000..]) / amplitude).log10();
        assert!((gain_db + 12.0).abs() < 0.2, "{gain_db}");
    }
}
