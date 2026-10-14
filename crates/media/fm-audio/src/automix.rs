//! Deterministic gain-sharing (Dugan-style) microphone automixer.
//!
//! Each block, the automixer measures the mean-square power of every enabled
//! input, applies per-channel weights and priority ducking, and derives a
//! power share `share_i = w_i * P_i / sum_j(w_j * P_j)`. The target amplitude
//! gain is `sqrt(share_i)`, so the summed power of all target gains is one
//! (constant total gain regardless of how many microphones are open).
//!
//! Applied gains move linearly toward their block target: by at most
//! `1 / attack_samples` per sample when rising and `1 / release_samples` per
//! sample when falling (a count of zero jumps immediately). Each sample first
//! advances the gain and then multiplies the input by the new gain.
//!
//! When no enabled channel is above the activity threshold, the automixer
//! either holds the last non-silent shares (last-mic-hold) or falls back to
//! equal shares across enabled channels. The optional NOM attenuation
//! (`1 / sqrt(open_mics)`) is applied only to the optional summed mix output,
//! never to the per-channel outputs.
//!
//! Control-plane methods may allocate; [`Automixer::process`] never allocates
//! and validates all inputs before mutating state or writing outputs.

use std::fmt;

use crate::{MAX_SAMPLES_PER_BLOCK, MIN_GAIN_DB};

/// Maximum number of microphone channels handled by one [`Automixer`].
pub const MAX_AUTOMIX_CHANNELS: usize = 32;
/// Maximum attack or release duration, in samples.
pub const MAX_AUTOMIX_SMOOTHING_SAMPLES: u32 = 480_000;
/// Maximum linear weight accepted for one channel.
pub const MAX_AUTOMIX_WEIGHT: f32 = 16.0;

/// Errors returned by the automixer.
#[derive(Clone, Debug, PartialEq)]
pub enum AutomixError {
    ChannelCountOutOfRange {
        actual: usize,
        maximum: usize,
    },
    ChannelIndexOutOfRange {
        channel: usize,
        channels: usize,
    },
    InvalidWeight(f32),
    InvalidActivityThresholdDb(f32),
    InvalidPriorityDuckDb(f32),
    SmoothingOutOfRange {
        actual: u32,
        maximum: u32,
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
    MixLengthMismatch {
        expected: usize,
        actual: usize,
    },
    SampleCountOutOfRange {
        actual: usize,
        maximum: usize,
    },
    NonFiniteSample {
        channel: usize,
        sample: usize,
    },
}

impl fmt::Display for AutomixError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelCountOutOfRange { actual, maximum } => write!(
                formatter,
                "automix channel count {actual} is outside 1..={maximum}"
            ),
            Self::ChannelIndexOutOfRange { channel, channels } => write!(
                formatter,
                "automix channel {channel} is out of range for {channels} channels"
            ),
            Self::InvalidWeight(weight) => write!(
                formatter,
                "automix weight {weight} is outside 0..={MAX_AUTOMIX_WEIGHT}"
            ),
            Self::InvalidActivityThresholdDb(db) => write!(
                formatter,
                "automix activity threshold {db} dBFS is outside {MIN_GAIN_DB}..=0"
            ),
            Self::InvalidPriorityDuckDb(db) => write!(
                formatter,
                "automix priority duck {db} dB is outside {MIN_GAIN_DB}..=0"
            ),
            Self::SmoothingOutOfRange { actual, maximum } => write!(
                formatter,
                "automix smoothing of {actual} samples exceeds {maximum}"
            ),
            Self::PlaneCountMismatch { expected, actual } => write!(
                formatter,
                "automix expected {expected} planes but received {actual}"
            ),
            Self::PlaneLengthMismatch {
                plane,
                expected,
                actual,
            } => write!(
                formatter,
                "automix plane {plane} has {actual} samples; expected {expected}"
            ),
            Self::MixLengthMismatch { expected, actual } => write!(
                formatter,
                "automix mix output has {actual} samples; expected {expected}"
            ),
            Self::SampleCountOutOfRange { actual, maximum } => write!(
                formatter,
                "automix sample count {actual} is outside 1..={maximum}"
            ),
            Self::NonFiniteSample { channel, sample } => write!(
                formatter,
                "automix input channel {channel} sample {sample} is not finite"
            ),
        }
    }
}

impl std::error::Error for AutomixError {}

/// Static automixer configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutomixConfig {
    /// Number of microphone channels, `1..=MAX_AUTOMIX_CHANNELS`.
    pub channels: usize,
    /// Samples for a full `0 -> 1` gain rise; zero jumps immediately.
    pub attack_samples: u32,
    /// Samples for a full `1 -> 0` gain fall; zero jumps immediately.
    pub release_samples: u32,
    /// Block RMS level (dBFS) at or above which a channel counts as open.
    pub activity_threshold_db: f32,
    /// Gain applied to lower-priority channels while a higher-priority channel
    /// is open. `None` removes them from the share entirely.
    pub priority_duck_db: Option<f32>,
    /// Hold the last non-silent shares during silence instead of equal shares.
    pub last_mic_hold: bool,
    /// Apply `1 / sqrt(open_mics)` to the optional summed mix output.
    pub nom_attenuation: bool,
}

impl AutomixConfig {
    /// Returns a configuration with immediate gains, a -60 dBFS threshold,
    /// full priority ducking, last-mic-hold enabled, and NOM attenuation off.
    #[must_use]
    pub const fn new(channels: usize) -> Self {
        Self {
            channels,
            attack_samples: 0,
            release_samples: 0,
            activity_threshold_db: -60.0,
            priority_duck_db: None,
            last_mic_hold: true,
            nom_attenuation: false,
        }
    }
}

/// Per-channel automix settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutomixChannelSettings {
    /// Linear weight in `0..=MAX_AUTOMIX_WEIGHT`, applied to measured power.
    pub weight: f32,
    /// Higher values win: while any open channel has a higher priority, this
    /// channel is ducked by [`AutomixConfig::priority_duck_db`].
    pub priority: u8,
    /// Disabled channels receive zero gain and are excluded from sharing.
    pub enabled: bool,
}

impl Default for AutomixChannelSettings {
    fn default() -> Self {
        Self {
            weight: 1.0,
            priority: 0,
            enabled: true,
        }
    }
}

/// Per-channel telemetry for the most recent block.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AutomixChannelTelemetry {
    /// Mean-square input power of the block.
    pub mean_square: f32,
    /// Whether the channel was enabled and at or above the activity threshold.
    pub active: bool,
    /// Whether a higher-priority open channel ducked this channel.
    pub ducked: bool,
    /// Power share in `0..=1`; enabled shares sum to one (or all zero).
    pub share: f32,
    /// Target amplitude gain, `sqrt(share)`.
    pub target_gain: f32,
    /// Applied amplitude gain after the last sample of the block.
    pub gain: f32,
}

/// Deterministic gain-sharing telemetry for the most recent block.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomixTelemetry {
    /// Zero-based index of the block, counted from construction.
    pub block_index: u64,
    /// Samples per channel processed in the block.
    pub samples: usize,
    /// Number of open (active) channels.
    pub open_mics: usize,
    /// Whether the shares were held from the last non-silent block.
    pub held: bool,
    /// `sum_j(w_j * P_j)` after priority ducking.
    pub total_weighted_power: f32,
    /// Gain applied to the summed mix output (NOM attenuation).
    pub nom_gain: f32,
    channels: Vec<AutomixChannelTelemetry>,
}

impl AutomixTelemetry {
    /// Per-channel telemetry in channel order.
    #[must_use]
    pub fn channels(&self) -> &[AutomixChannelTelemetry] {
        &self.channels
    }
}

/// A bounded, allocation-free gain-sharing automixer.
#[derive(Clone, Debug)]
pub struct Automixer {
    config: AutomixConfig,
    activity_threshold_ms: f64,
    duck_power: f64,
    settings: Vec<AutomixChannelSettings>,
    gains: Vec<f32>,
    held_shares: Vec<f32>,
    blocks: u64,
    telemetry: AutomixTelemetry,
}

impl Automixer {
    /// Creates an automixer with default channel settings and zero gains.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-range channel count, smoothing length,
    /// activity threshold, or priority duck.
    pub fn new(config: AutomixConfig) -> Result<Self, AutomixError> {
        if config.channels == 0 || config.channels > MAX_AUTOMIX_CHANNELS {
            return Err(AutomixError::ChannelCountOutOfRange {
                actual: config.channels,
                maximum: MAX_AUTOMIX_CHANNELS,
            });
        }
        for smoothing in [config.attack_samples, config.release_samples] {
            if smoothing > MAX_AUTOMIX_SMOOTHING_SAMPLES {
                return Err(AutomixError::SmoothingOutOfRange {
                    actual: smoothing,
                    maximum: MAX_AUTOMIX_SMOOTHING_SAMPLES,
                });
            }
        }
        let threshold = config.activity_threshold_db;
        if !threshold.is_finite() || !(MIN_GAIN_DB..=0.0).contains(&threshold) {
            return Err(AutomixError::InvalidActivityThresholdDb(threshold));
        }
        let duck_power = match config.priority_duck_db {
            None => 0.0,
            Some(db) if db.is_finite() && (MIN_GAIN_DB..=0.0).contains(&db) => {
                10.0_f64.powf(f64::from(db) / 10.0)
            }
            Some(db) => return Err(AutomixError::InvalidPriorityDuckDb(db)),
        };
        let channels = config.channels;
        let equal = equal_share(channels);
        Ok(Self {
            config,
            activity_threshold_ms: 10.0_f64.powf(f64::from(threshold) / 10.0),
            duck_power,
            settings: vec![AutomixChannelSettings::default(); channels],
            gains: vec![0.0; channels],
            held_shares: vec![equal; channels],
            blocks: 0,
            telemetry: AutomixTelemetry {
                block_index: 0,
                samples: 0,
                open_mics: 0,
                held: false,
                total_weighted_power: 0.0,
                nom_gain: 1.0,
                channels: vec![AutomixChannelTelemetry::default(); channels],
            },
        })
    }

    /// Returns the static configuration.
    #[must_use]
    pub const fn config(&self) -> &AutomixConfig {
        &self.config
    }

    /// Replaces the settings of one channel.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-range channel or invalid weight.
    pub fn set_channel(
        &mut self,
        channel: usize,
        settings: AutomixChannelSettings,
    ) -> Result<(), AutomixError> {
        let channels = self.config.channels;
        let slot = self
            .settings
            .get_mut(channel)
            .ok_or(AutomixError::ChannelIndexOutOfRange { channel, channels })?;
        if !settings.weight.is_finite() || !(0.0..=MAX_AUTOMIX_WEIGHT).contains(&settings.weight) {
            return Err(AutomixError::InvalidWeight(settings.weight));
        }
        *slot = settings;
        Ok(())
    }

    /// Returns the settings of one channel.
    #[must_use]
    pub fn channel(&self, channel: usize) -> Option<AutomixChannelSettings> {
        self.settings.get(channel).copied()
    }

    /// Returns the currently applied amplitude gains in channel order.
    #[must_use]
    pub fn gains(&self) -> &[f32] {
        &self.gains
    }

    /// Returns telemetry for the most recent processed block.
    #[must_use]
    pub const fn telemetry(&self) -> &AutomixTelemetry {
        &self.telemetry
    }

    /// Resets applied gains to zero, held shares to equal, and the block count.
    pub fn reset(&mut self) {
        let equal = equal_share(self.config.channels);
        self.gains.fill(0.0);
        self.held_shares.fill(equal);
        self.blocks = 0;
    }

    /// Processes one block into caller-owned per-channel outputs and an
    /// optional summed mix.
    ///
    /// All structural and sample validation completes before any state or
    /// output changes. This method never allocates.
    ///
    /// # Errors
    ///
    /// Returns an error for plane-count or length mismatches, an out-of-range
    /// sample count, or a non-finite input sample.
    pub fn process(
        &mut self,
        inputs: &[&[f32]],
        outputs: &mut [&mut [f32]],
        mix: Option<&mut [f32]>,
    ) -> Result<&AutomixTelemetry, AutomixError> {
        let samples = self.validate(inputs, outputs, mix.as_deref())?;
        self.compute_targets(inputs, samples);
        self.apply(inputs, outputs, samples);
        if let Some(mix) = mix {
            let nom_gain = self.telemetry.nom_gain;
            for (index, slot) in mix.iter_mut().enumerate() {
                let mut sum = 0.0_f32;
                for output in outputs.iter() {
                    sum += output[index];
                }
                *slot = sum * nom_gain;
            }
        }
        Ok(&self.telemetry)
    }

    fn validate(
        &self,
        inputs: &[&[f32]],
        outputs: &[&mut [f32]],
        mix: Option<&[f32]>,
    ) -> Result<usize, AutomixError> {
        let expected = self.config.channels;
        for actual in [inputs.len(), outputs.len()] {
            if actual != expected {
                return Err(AutomixError::PlaneCountMismatch { expected, actual });
            }
        }
        let samples = inputs[0].len();
        if samples == 0 || samples > MAX_SAMPLES_PER_BLOCK {
            return Err(AutomixError::SampleCountOutOfRange {
                actual: samples,
                maximum: MAX_SAMPLES_PER_BLOCK,
            });
        }
        let lengths = inputs
            .iter()
            .map(|plane| plane.len())
            .chain(outputs.iter().map(|plane| plane.len()));
        for (plane, actual) in lengths.enumerate() {
            if actual != samples {
                return Err(AutomixError::PlaneLengthMismatch {
                    plane: plane % expected,
                    expected: samples,
                    actual,
                });
            }
        }
        if let Some(mix) = mix
            && mix.len() != samples
        {
            return Err(AutomixError::MixLengthMismatch {
                expected: samples,
                actual: mix.len(),
            });
        }
        for (channel, plane) in inputs.iter().enumerate() {
            if let Some(sample) = plane.iter().position(|value| !value.is_finite()) {
                return Err(AutomixError::NonFiniteSample { channel, sample });
            }
        }
        Ok(samples)
    }

    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn compute_targets(&mut self, inputs: &[&[f32]], samples: usize) {
        let telemetry = &mut self.telemetry;
        let mut top_priority: Option<u8> = None;
        let mut open_mics = 0_usize;
        for ((plane, settings), channel) in inputs
            .iter()
            .zip(&self.settings)
            .zip(telemetry.channels.iter_mut())
        {
            let energy: f64 = plane
                .iter()
                .map(|sample| f64::from(*sample) * f64::from(*sample))
                .sum();
            let mean_square = energy / samples as f64;
            let active = settings.enabled && mean_square >= self.activity_threshold_ms;
            if active {
                open_mics += 1;
                top_priority = top_priority.max(Some(settings.priority));
            }
            *channel = AutomixChannelTelemetry {
                mean_square: mean_square as f32,
                active,
                ..AutomixChannelTelemetry::default()
            };
        }

        let mut total = 0.0_f64;
        for (settings, channel) in self.settings.iter().zip(telemetry.channels.iter_mut()) {
            if !settings.enabled {
                continue;
            }
            channel.ducked = top_priority.is_some_and(|top| settings.priority < top);
            let duck = if channel.ducked { self.duck_power } else { 1.0 };
            total += f64::from(settings.weight) * f64::from(channel.mean_square) * duck;
        }

        let silent = open_mics == 0 || total <= 0.0;
        let mut held = false;
        if silent {
            let equal = enabled_equal_share(&self.settings);
            let held_total: f32 = self
                .settings
                .iter()
                .zip(&self.held_shares)
                .filter(|(settings, _)| settings.enabled)
                .map(|(_, share)| *share)
                .sum();
            held = self.config.last_mic_hold && held_total > 0.0;
            for ((settings, channel), held_share) in self
                .settings
                .iter()
                .zip(telemetry.channels.iter_mut())
                .zip(self.held_shares.iter())
            {
                channel.share = if !settings.enabled {
                    0.0
                } else if held {
                    *held_share / held_total
                } else {
                    equal
                };
            }
        } else {
            for ((settings, channel), held_share) in self
                .settings
                .iter()
                .zip(telemetry.channels.iter_mut())
                .zip(self.held_shares.iter_mut())
            {
                let share = if settings.enabled {
                    let duck = if channel.ducked { self.duck_power } else { 1.0 };
                    let weighted =
                        f64::from(settings.weight) * f64::from(channel.mean_square) * duck;
                    (weighted / total) as f32
                } else {
                    0.0
                };
                channel.share = share;
                *held_share = share;
            }
        }
        for channel in &mut telemetry.channels {
            channel.target_gain = channel.share.sqrt();
        }

        telemetry.block_index = self.blocks;
        telemetry.samples = samples;
        telemetry.open_mics = open_mics;
        telemetry.held = held;
        telemetry.total_weighted_power = total as f32;
        telemetry.nom_gain = if self.config.nom_attenuation && open_mics > 1 {
            (1.0 / (open_mics as f64).sqrt()) as f32
        } else {
            1.0
        };
        self.blocks = self.blocks.wrapping_add(1);
    }

    #[allow(clippy::cast_precision_loss)]
    fn apply(&mut self, inputs: &[&[f32]], outputs: &mut [&mut [f32]], samples: usize) {
        let rise = step(self.config.attack_samples);
        let fall = step(self.config.release_samples);
        for (((input, output), gain), channel) in inputs
            .iter()
            .zip(outputs.iter_mut())
            .zip(self.gains.iter_mut())
            .zip(self.telemetry.channels.iter_mut())
        {
            let target = channel.target_gain;
            for (source, slot) in input.iter().zip(output.iter_mut()).take(samples) {
                *gain = if *gain < target {
                    rise.map_or(target, |rise| (*gain + rise).min(target))
                } else {
                    fall.map_or(target, |fall| (*gain - fall).max(target))
                };
                *slot = *source * *gain;
            }
            channel.gain = *gain;
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn step(samples: u32) -> Option<f32> {
    (samples > 0).then(|| 1.0 / samples as f32)
}

#[allow(clippy::cast_precision_loss)]
fn equal_share(channels: usize) -> f32 {
    1.0 / channels as f32
}

fn enabled_equal_share(settings: &[AutomixChannelSettings]) -> f32 {
    let enabled = settings.iter().filter(|settings| settings.enabled).count();
    if enabled == 0 {
        0.0
    } else {
        equal_share(enabled)
    }
}
