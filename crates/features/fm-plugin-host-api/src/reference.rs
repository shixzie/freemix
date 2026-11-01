//! Built-in reference processors for the plugin chain (AU-006).
//!
//! The plugin sandbox hosts real plugins; this module is the in-tree
//! reference set the engine and daemon render through while that sandbox is
//! wired. A reference plugin is addressed by its stable [`PluginId`] and its
//! configuration is the opaque [`StateSnapshot`] the chain slot carries:
//!
//! * `freemix.reference.trim` — a fader. Its state is one little-endian
//!   `i32` in millidB (`0` to four bytes; absent means unity), bounded like
//!   a mixer fader.
//! * `freemix.reference.polarity` — inverts every sample. No state.
//! * `freemix.reference.softclip` — a soft knee around a threshold. Its
//!   state is one little-endian `i32` in millidB (absent means 0 dBFS),
//!   bounded to `-60_000..=0`.
//!
//! A slot's named parameter overrides its opaque state: `gain_millidb` for the
//! trim and `threshold_millidb` for the soft clipper, as exact whole millidB.
//!
//! A rack is built from one [`PluginChain`]; bypassed slots are skipped,
//! unknown plugin identifiers are refused, and [`ReferenceRack::latency_frames`]
//! reports the chain's effective latency for a caller that delays parallel
//! paths.

use core::fmt;

use crate::{PluginChain, PluginId, PluginSlot};

/// Gain floor and ceiling a reference trim accepts, in millidB.
pub const MIN_REFERENCE_TRIM_MILLIDB: i32 = -96_000;
pub const MAX_REFERENCE_TRIM_MILLIDB: i32 = 24_000;
/// Quietest and loudest soft-clip threshold, in millidB.
pub const MIN_REFERENCE_CLIP_MILLIDB: i32 = -60_000;
pub const MAX_REFERENCE_CLIP_MILLIDB: i32 = 0;

/// The stable identifier of the reference trim.
pub const REFERENCE_TRIM: &str = "freemix.reference.trim";
/// The stable identifier of the reference polarity inverter.
pub const REFERENCE_POLARITY: &str = "freemix.reference.polarity";
/// The stable identifier of the reference soft clipper.
pub const REFERENCE_SOFTCLIP: &str = "freemix.reference.softclip";
/// The trim's named gain parameter, in millidB.
pub const PARAMETER_TRIM_GAIN_MILLIDB: &str = "gain_millidb";
/// The soft clipper's named threshold parameter, in millidB.
pub const PARAMETER_SOFTCLIP_THRESHOLD_MILLIDB: &str = "threshold_millidb";

/// Every plugin identifier this build renders.
pub const REFERENCE_PLUGINS: [&str; 3] = [REFERENCE_TRIM, REFERENCE_POLARITY, REFERENCE_SOFTCLIP];

/// Whether `plugin` is one of the built-in reference processors.
#[must_use]
pub fn is_reference_plugin(plugin: &PluginId) -> bool {
    REFERENCE_PLUGINS.contains(&plugin.as_str())
}

/// One prepared reference processor.
trait Processor: Send {
    fn process(&mut self, channel: usize, samples: &mut [f32]);
    fn clone_box(&self) -> Box<dyn Processor>;
}

impl Clone for Box<dyn Processor> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

#[derive(Clone, Copy, Debug)]
struct Trim {
    gain: f32,
}

impl Processor for Trim {
    fn process(&mut self, _channel: usize, samples: &mut [f32]) {
        if (self.gain - 1.0).abs() <= f32::EPSILON {
            return;
        }
        for sample in samples {
            *sample *= self.gain;
        }
    }

    fn clone_box(&self) -> Box<dyn Processor> {
        Box::new(*self)
    }
}

#[derive(Clone, Copy, Debug)]
struct Polarity;

impl Processor for Polarity {
    fn process(&mut self, _channel: usize, samples: &mut [f32]) {
        for sample in samples {
            *sample = -*sample;
        }
    }

    fn clone_box(&self) -> Box<dyn Processor> {
        Box::new(*self)
    }
}

#[derive(Clone, Copy, Debug)]
struct SoftClip {
    threshold: f32,
}

impl Processor for SoftClip {
    fn process(&mut self, _channel: usize, samples: &mut [f32]) {
        let threshold = self.threshold;
        for sample in samples {
            // A cubic soft knee: unity slope at zero, hard-limited outside
            // the threshold, continuous through it.
            let normalized = *sample / threshold;
            let clipped = if normalized.abs() <= 1.0 {
                normalized - normalized.powi(3) / 3.0
            } else {
                normalized.signum() * (2.0 / 3.0)
            };
            *sample = clipped * threshold * 1.5;
        }
    }

    fn clone_box(&self) -> Box<dyn Processor> {
        Box::new(*self)
    }
}

/// One built reference processor and the slot it came from.
#[derive(Clone)]
struct RackSlot {
    plugin: PluginId,
    processed: bool,
    processor: Box<dyn Processor>,
}

/// The prepared processors of one chain, in processing order.
#[derive(Clone, Default)]
pub struct ReferenceRack {
    slots: Vec<RackSlot>,
    latency_frames: u32,
}

impl fmt::Debug for ReferenceRack {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReferenceRack")
            .field(
                "plugins",
                &self
                    .slots
                    .iter()
                    .map(|slot| &slot.plugin)
                    .collect::<Vec<_>>(),
            )
            .field("latency_frames", &self.latency_frames)
            .finish()
    }
}

impl ReferenceRack {
    /// Builds the processors of `chain` at `sample_rate_hz`.
    ///
    /// # Errors
    ///
    /// Returns [`ReferenceError::UnsupportedPlugin`] for a plugin this build
    /// does not render, [`ReferenceError::InvalidSampleRate`] unless the rate
    /// is finite and positive, and [`ReferenceError::InvalidState`] for a
    /// state that is not this plugin's configuration.
    pub fn new(chain: &PluginChain, sample_rate_hz: f32) -> Result<Self, ReferenceError> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(ReferenceError::InvalidSampleRate);
        }
        let mut slots = Vec::with_capacity(chain.len());
        for slot in chain.slots() {
            let processor = build(slot)?;
            slots.push(RackSlot {
                plugin: slot.plugin().clone(),
                processed: !slot.is_bypassed(),
                processor,
            });
        }
        Ok(Self {
            slots,
            latency_frames: chain.effective_latency_frames(),
        })
    }

    /// The plugin identifiers that process media, in order.
    pub fn active_plugins(&self) -> impl Iterator<Item = &PluginId> {
        self.slots
            .iter()
            .filter(|slot| slot.processed)
            .map(|slot| &slot.plugin)
    }

    /// The chain's effective latency in frames.
    #[must_use]
    pub const fn latency_frames(&self) -> u32 {
        self.latency_frames
    }

    /// Whether no slot processes media.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.slots.iter().any(|slot| slot.processed)
    }

    /// Processes `planes` (one slice per channel) up to `samples`.
    pub fn process(&mut self, planes: &mut [Vec<f32>], samples: usize) {
        for slot in &mut self.slots {
            if !slot.processed {
                continue;
            }
            for (channel, plane) in planes.iter_mut().enumerate() {
                let end = samples.min(plane.len());
                slot.processor.process(channel, &mut plane[..end]);
            }
        }
    }
}

fn build(slot: &PluginSlot) -> Result<Box<dyn Processor>, ReferenceError> {
    match slot.plugin().as_str() {
        REFERENCE_TRIM => {
            let milli_db = match parameter_i32(
                slot,
                PARAMETER_TRIM_GAIN_MILLIDB,
                MIN_REFERENCE_TRIM_MILLIDB,
                MAX_REFERENCE_TRIM_MILLIDB,
            )? {
                Some(value) => value,
                None => state_i32(slot, MIN_REFERENCE_TRIM_MILLIDB, MAX_REFERENCE_TRIM_MILLIDB)?,
            };
            #[allow(clippy::cast_precision_loss)]
            let gain = 10_f32.powf(milli_db as f32 / 20_000.0);
            Ok(Box::new(Trim { gain }))
        }
        REFERENCE_POLARITY => {
            refuse_state(slot)?;
            Ok(Box::new(Polarity))
        }
        REFERENCE_SOFTCLIP => {
            let milli_db = match parameter_i32(
                slot,
                PARAMETER_SOFTCLIP_THRESHOLD_MILLIDB,
                MIN_REFERENCE_CLIP_MILLIDB,
                MAX_REFERENCE_CLIP_MILLIDB,
            )? {
                Some(value) => value,
                None => state_i32(slot, MIN_REFERENCE_CLIP_MILLIDB, MAX_REFERENCE_CLIP_MILLIDB)?,
            };
            #[allow(clippy::cast_precision_loss)]
            let threshold = 10_f32.powf(milli_db as f32 / 20_000.0);
            Ok(Box::new(SoftClip { threshold }))
        }
        _ => Err(ReferenceError::UnsupportedPlugin(slot.plugin().clone())),
    }
}

fn refuse_state(slot: &PluginSlot) -> Result<(), ReferenceError> {
    match slot.snapshot() {
        Some(snapshot) if !snapshot.data().is_empty() => {
            Err(ReferenceError::InvalidState(slot.plugin().clone()))
        }
        _ => Ok(()),
    }
}

/// A named whole-millidB parameter, when the slot carries it.
fn parameter_i32(
    slot: &PluginSlot,
    parameter: &'static str,
    minimum: i32,
    maximum: i32,
) -> Result<Option<i32>, ReferenceError> {
    let Some(value) = slot.parameter(parameter) else {
        return Ok(None);
    };
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(ReferenceError::InvalidParameter {
            plugin: slot.plugin().clone(),
            parameter,
        });
    }
    let numeric = f64::from(value);
    if numeric < f64::from(minimum) || numeric > f64::from(maximum) {
        return Err(ReferenceError::InvalidParameter {
            plugin: slot.plugin().clone(),
            parameter,
        });
    }
    // The value is finite, integral, and inside `minimum..=maximum`, so the
    // conversion cannot truncate.
    #[allow(clippy::cast_possible_truncation)]
    Ok(Some(value as i32))
}

fn state_i32(slot: &PluginSlot, minimum: i32, maximum: i32) -> Result<i32, ReferenceError> {
    let Some(snapshot) = slot.snapshot() else {
        return Ok(0);
    };
    let value = match snapshot.data() {
        [] => 0,
        [a, b, c, d] => i32::from_le_bytes([*a, *b, *c, *d]),
        _ => return Err(ReferenceError::InvalidState(slot.plugin().clone())),
    };
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(ReferenceError::InvalidState(slot.plugin().clone()))
    }
}

/// One reference state as the little-endian bytes a chain slot carries.
#[must_use]
pub fn state_bytes(milli_db: i32) -> Vec<u8> {
    milli_db.to_le_bytes().to_vec()
}

/// Why a reference rack could not be built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceError {
    /// The plugin is not one of [`REFERENCE_PLUGINS`].
    UnsupportedPlugin(PluginId),
    /// The plugin's state is not its configuration.
    InvalidState(PluginId),
    /// A named parameter is not a finite whole value in the processor's
    /// range.
    InvalidParameter {
        plugin: PluginId,
        parameter: &'static str,
    },
    /// The sample rate is not finite and positive.
    InvalidSampleRate,
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlugin(plugin) => {
                write!(formatter, "plugin {plugin} has no reference processor")
            }
            Self::InvalidState(plugin) => {
                write!(formatter, "plugin {plugin} holds an invalid state")
            }
            Self::InvalidParameter { plugin, parameter } => {
                write!(
                    formatter,
                    "plugin {plugin} parameter {parameter} is invalid"
                )
            }
            Self::InvalidSampleRate => write!(formatter, "the sample rate is not positive"),
        }
    }
}

impl std::error::Error for ReferenceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginChain, PluginSlot, ProtocolLimits, StateSnapshot, StateVersion};

    fn built(slot: PluginSlot) -> ReferenceRack {
        let mut chain = PluginChain::new();
        chain.push(slot).unwrap();
        ReferenceRack::new(&chain, 48_000.0).unwrap()
    }

    fn process(rack: &mut ReferenceRack, sample: f32) -> f32 {
        let mut planes = vec![vec![sample]];
        rack.process(&mut planes, 1);
        planes[0][0]
    }

    #[test]
    fn named_parameters_drive_the_trim_and_override_state() {
        let expected = 10_f32.powf(-6000.0 / 20_000.0);

        let mut trim = PluginSlot::new(PluginId::new(REFERENCE_TRIM));
        trim.set_parameter(PARAMETER_TRIM_GAIN_MILLIDB, -6000.0)
            .unwrap();
        let mut rack = built(trim);
        assert!((process(&mut rack, 1.0) - expected).abs() < 1e-5);

        // The opaque state still configures a slot without a parameter.
        let mut trim = PluginSlot::new(PluginId::new(REFERENCE_TRIM));
        let snapshot = StateSnapshot::new(
            PluginId::new(REFERENCE_TRIM),
            StateVersion::new(0),
            state_bytes(-6000),
            &ProtocolLimits::default(),
        )
        .unwrap();
        trim.store_snapshot(snapshot).unwrap();
        let mut rack = built(trim);
        assert!((process(&mut rack, 1.0) - expected).abs() < 1e-5);

        // A named parameter wins over a conflicting state.
        let mut trim = PluginSlot::new(PluginId::new(REFERENCE_TRIM));
        let snapshot = StateSnapshot::new(
            PluginId::new(REFERENCE_TRIM),
            StateVersion::new(0),
            state_bytes(-12_000),
            &ProtocolLimits::default(),
        )
        .unwrap();
        trim.store_snapshot(snapshot).unwrap();
        trim.set_parameter(PARAMETER_TRIM_GAIN_MILLIDB, 0.0)
            .unwrap();
        let mut rack = built(trim);
        assert!((process(&mut rack, 1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn invalid_named_parameters_are_refused() {
        for value in [24_001.0, -96_001.0, 0.5] {
            let mut trim = PluginSlot::new(PluginId::new(REFERENCE_TRIM));
            trim.set_parameter(PARAMETER_TRIM_GAIN_MILLIDB, value)
                .unwrap();
            let mut chain = PluginChain::new();
            chain.push(trim).unwrap();
            assert_eq!(
                ReferenceRack::new(&chain, 48_000.0).unwrap_err(),
                ReferenceError::InvalidParameter {
                    plugin: PluginId::new(REFERENCE_TRIM),
                    parameter: PARAMETER_TRIM_GAIN_MILLIDB,
                }
            );
        }
    }

    #[test]
    fn softclip_reads_its_named_threshold() {
        let mut slot = PluginSlot::new(PluginId::new(REFERENCE_SOFTCLIP));
        slot.set_parameter(PARAMETER_SOFTCLIP_THRESHOLD_MILLIDB, -6000.0)
            .unwrap();
        let mut rack = built(slot);
        let output = process(&mut rack, 1.0);
        assert!(output > 0.0 && output < 1.0, "{output}");
    }
}
