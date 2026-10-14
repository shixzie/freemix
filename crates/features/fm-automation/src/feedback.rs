//! Activator feedback: typed production state drives exact outgoing MIDI and
//! OSC bytes (LEDs, motor faders, OSC surfaces), coalesced per control.
//!
//! Observations overwrite the latest value for each bound control; draining
//! emits only controls whose latest encoded bytes differ from what was last
//! sent to a connected device. Reattaching a device forces a full resend.

use crate::control_map::{BindingId, DeviceKey, Span};
use crate::controller::{ActivatorRule, TallySnapshot};
use crate::midi::{MIDI_14BIT_MAX, MidiChannel, MidiMessage};
use crate::osc::{
    OscArg, OscLimits, OscMessage, OscPacket, encode_osc_packet, validate_osc_address,
};
use core::fmt;
use std::collections::BTreeSet;

/// Maximum feedback bindings held by one [`FeedbackRouter`].
pub const MAX_FEEDBACK_BINDINGS: usize = 1024;

/// The production state a feedback binding follows.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedbackSource {
    /// On/off tally state (program, preview, overlay, audio, record, stream).
    Activator(ActivatorRule),
    /// A named continuous level (e.g. an input volume) in `range` units.
    Level { key: String, range: Span },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OscFeedbackFormat {
    /// `f` with the normalized value `0..=1`.
    Float,
    /// `i` with `round(normalized * max)`.
    Int { max: i32 },
    /// `T` when normalized `>= 0.5`, else `F`.
    Bool,
}

/// The device control a feedback binding writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeedbackTarget {
    /// Note On whose velocity interpolates `off_velocity..=on_velocity`
    /// (exactly one of the two for on/off sources).
    MidiNote {
        channel: MidiChannel,
        note: u8,
        on_velocity: u8,
        off_velocity: u8,
    },
    /// 7-bit Control Change interpolating `off_value..=on_value`.
    MidiCc {
        channel: MidiChannel,
        controller: u8,
        on_value: u8,
        off_value: u8,
    },
    /// 14-bit motor fader: MSB on `controller`, LSB on `controller + 32`.
    MidiCc14 {
        channel: MidiChannel,
        controller: u8,
    },
    /// 14-bit motor fader via pitch bend (Mackie-style).
    MidiPitchBend { channel: MidiChannel },
    Osc {
        address: String,
        format: OscFeedbackFormat,
    },
}

impl FeedbackTarget {
    fn validate(&self) -> Result<(), FeedbackError> {
        let ok = match self {
            Self::MidiNote {
                note,
                on_velocity,
                off_velocity,
                ..
            } => *note <= 127 && *on_velocity <= 127 && *off_velocity <= 127,
            Self::MidiCc {
                controller,
                on_value,
                off_value,
                ..
            } => *controller <= 127 && *on_value <= 127 && *off_value <= 127,
            Self::MidiCc14 { controller, .. } => *controller <= 31,
            Self::MidiPitchBend { .. } => true,
            Self::Osc { address, format } => {
                validate_osc_address(address).is_ok()
                    && !matches!(format, OscFeedbackFormat::Int { max } if *max <= 0)
            }
        };
        if ok {
            Ok(())
        } else {
            Err(FeedbackError::InvalidTarget)
        }
    }

    /// Whether two targets write the same physical control.
    fn overlaps(&self, other: &Self) -> bool {
        use FeedbackTarget::{MidiCc, MidiCc14, MidiNote, MidiPitchBend, Osc};
        let cc_overlap = |channel: MidiChannel, wide: u8, other_channel: MidiChannel, cc: u8| {
            channel == other_channel && (cc == wide || cc == wide + 32)
        };
        match (self, other) {
            (
                MidiNote { channel, note, .. },
                MidiNote {
                    channel: c2,
                    note: n2,
                    ..
                },
            ) => channel == c2 && note == n2,
            (
                MidiCc {
                    channel,
                    controller,
                    ..
                },
                MidiCc {
                    channel: c2,
                    controller: k2,
                    ..
                },
            )
            | (
                MidiCc14 {
                    channel,
                    controller,
                },
                MidiCc14 {
                    channel: c2,
                    controller: k2,
                },
            ) => channel == c2 && controller == k2,
            (
                MidiCc14 {
                    channel,
                    controller,
                },
                MidiCc {
                    channel: c2,
                    controller: k2,
                    ..
                },
            )
            | (
                MidiCc {
                    channel: c2,
                    controller: k2,
                    ..
                },
                MidiCc14 {
                    channel,
                    controller,
                },
            ) => cc_overlap(*channel, *controller, *c2, *k2),
            (MidiPitchBend { channel }, MidiPitchBend { channel: c2 }) => channel == c2,
            (Osc { address, .. }, Osc { address: a2, .. }) => address == a2,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FeedbackBinding {
    pub id: BindingId,
    pub device: DeviceKey,
    pub source: FeedbackSource,
    pub target: FeedbackTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeedbackPayload {
    /// One or more complete MIDI messages with explicit status bytes.
    Midi(Vec<u8>),
    /// One encoded OSC message packet.
    Osc(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedbackMessage {
    pub device: DeviceKey,
    pub binding: BindingId,
    pub payload: FeedbackPayload,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FeedbackError {
    InvalidTarget,
    InvalidLevelKey(String),
    DuplicateBinding(BindingId),
    TooManyBindings {
        limit: usize,
    },
    /// The target control is already driven by another binding.
    TargetConflict(BindingId),
    NonFiniteValue,
    Encode(String),
}

impl fmt::Display for FeedbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget => formatter.write_str("feedback target is invalid"),
            Self::InvalidLevelKey(key) => {
                write!(formatter, "feedback level key {key:?} is invalid")
            }
            Self::DuplicateBinding(id) => {
                write!(formatter, "feedback binding {} exists", id.as_str())
            }
            Self::TooManyBindings { limit } => {
                write!(formatter, "feedback bindings exceed {limit}")
            }
            Self::TargetConflict(id) => {
                write!(formatter, "feedback target is driven by {}", id.as_str())
            }
            Self::NonFiniteValue => formatter.write_str("feedback level must be finite"),
            Self::Encode(reason) => write!(formatter, "feedback encode failed: {reason}"),
        }
    }
}

impl std::error::Error for FeedbackError {}

fn rule_active(rule: ActivatorRule, tally: &TallySnapshot) -> bool {
    match rule {
        ActivatorRule::Program(input) => tally.program == Some(input),
        ActivatorRule::Preview(input) => tally.preview == Some(input),
        ActivatorRule::Overlay(input) => tally.overlays.contains(&input),
        ActivatorRule::AudioActive(input) => tally.audio_active.contains(&input),
        ActivatorRule::Recording => tally.recording,
        ActivatorRule::Streaming => tally.streaming,
    }
}

/// Linear interpolation from `low` (n = 0) to `high` (n = 1), rounded half
/// away from zero.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn interpolate(normalized: f64, low: u16, high: u16) -> u16 {
    let value = f64::from(low) + normalized.clamp(0.0, 1.0) * (f64::from(high) - f64::from(low));
    // Bounded by `low..=high` (both u16), so the cast is exact after rounding.
    value.round().clamp(0.0, f64::from(u16::MAX)) as u16
}

fn seven(value: u16) -> u8 {
    u8::try_from(value).unwrap_or(127).min(127)
}

#[allow(clippy::cast_possible_truncation)]
fn encode(target: &FeedbackTarget, normalized: f64) -> Result<FeedbackPayload, FeedbackError> {
    let midi = |message: MidiMessage| {
        message
            .encode()
            .map_err(|error| FeedbackError::Encode(error.to_string()))
    };
    Ok(match target {
        FeedbackTarget::MidiNote {
            channel,
            note,
            on_velocity,
            off_velocity,
        } => FeedbackPayload::Midi(midi(MidiMessage::NoteOn {
            channel: *channel,
            note: *note,
            velocity: seven(interpolate(
                normalized,
                u16::from(*off_velocity),
                u16::from(*on_velocity),
            )),
        })?),
        FeedbackTarget::MidiCc {
            channel,
            controller,
            on_value,
            off_value,
        } => FeedbackPayload::Midi(midi(MidiMessage::ControlChange {
            channel: *channel,
            controller: *controller,
            value: seven(interpolate(
                normalized,
                u16::from(*off_value),
                u16::from(*on_value),
            )),
        })?),
        FeedbackTarget::MidiCc14 {
            channel,
            controller,
        } => {
            let value = interpolate(normalized, 0, MIDI_14BIT_MAX);
            let mut bytes = midi(MidiMessage::ControlChange {
                channel: *channel,
                controller: *controller,
                value: seven(value >> 7),
            })?;
            bytes.extend(midi(MidiMessage::ControlChange {
                channel: *channel,
                controller: controller + 32,
                value: seven(value & 0x7F),
            })?);
            FeedbackPayload::Midi(bytes)
        }
        FeedbackTarget::MidiPitchBend { channel } => {
            FeedbackPayload::Midi(midi(MidiMessage::PitchBend {
                channel: *channel,
                value: interpolate(normalized, 0, MIDI_14BIT_MAX),
            })?)
        }
        FeedbackTarget::Osc { address, format } => {
            let arg = match format {
                // Normalized values are in 0..=1, exactly representable
                // enough for f32 control surfaces.
                OscFeedbackFormat::Float => OscArg::Float(normalized as f32),
                OscFeedbackFormat::Int { max } => {
                    let scaled = (normalized * f64::from(*max)).round();
                    OscArg::Int(scaled.clamp(0.0, f64::from(*max)) as i32)
                }
                OscFeedbackFormat::Bool if normalized >= 0.5 => OscArg::True,
                OscFeedbackFormat::Bool => OscArg::False,
            };
            let packet = OscPacket::Message(OscMessage::new(address.clone(), vec![arg]));
            FeedbackPayload::Osc(
                encode_osc_packet(&packet, &OscLimits::default())
                    .map_err(|error| FeedbackError::Encode(error.to_string()))?,
            )
        }
    })
}

#[derive(Clone, Debug)]
struct Slot {
    binding: FeedbackBinding,
    current: Option<FeedbackPayload>,
    sent: Option<FeedbackPayload>,
}

/// Derives coalesced outgoing feedback from typed state observations.
#[derive(Clone, Debug, Default)]
pub struct FeedbackRouter {
    slots: Vec<Slot>,
    connected: BTreeSet<DeviceKey>,
}

impl FeedbackRouter {
    /// Adds a feedback binding.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid targets or level keys, duplicate
    /// identifiers, a control already driven on the same device, or the
    /// binding bound.
    pub fn add(&mut self, binding: FeedbackBinding) -> Result<(), FeedbackError> {
        binding.target.validate()?;
        if let FeedbackSource::Level { key, .. } = &binding.source
            && BindingId::new(key.clone()).is_err()
        {
            return Err(FeedbackError::InvalidLevelKey(key.clone()));
        }
        for slot in &self.slots {
            if slot.binding.id == binding.id {
                return Err(FeedbackError::DuplicateBinding(binding.id));
            }
            if slot.binding.device == binding.device
                && slot.binding.target.overlaps(&binding.target)
            {
                return Err(FeedbackError::TargetConflict(slot.binding.id.clone()));
            }
        }
        if self.slots.len() >= MAX_FEEDBACK_BINDINGS {
            return Err(FeedbackError::TooManyBindings {
                limit: MAX_FEEDBACK_BINDINGS,
            });
        }
        self.slots.push(Slot {
            binding,
            current: None,
            sent: None,
        });
        Ok(())
    }

    /// Removes a binding; nothing further is emitted for it.
    pub fn remove(&mut self, id: &BindingId) -> Option<FeedbackBinding> {
        let index = self.slots.iter().position(|slot| &slot.binding.id == id)?;
        Some(self.slots.remove(index).binding)
    }

    /// Records the latest tally for every activator binding.
    ///
    /// # Errors
    ///
    /// Returns an error if a payload cannot be encoded (not expected for
    /// validated targets).
    pub fn observe_tally(&mut self, tally: &TallySnapshot) -> Result<(), FeedbackError> {
        for slot in &mut self.slots {
            if let FeedbackSource::Activator(rule) = slot.binding.source {
                let normalized = if rule_active(rule, tally) { 1.0 } else { 0.0 };
                slot.current = Some(encode(&slot.binding.target, normalized)?);
            }
        }
        Ok(())
    }

    /// Records the latest value of a named level; returns how many bindings
    /// follow it.
    ///
    /// # Errors
    ///
    /// Returns an error for non-finite values.
    pub fn observe_level(&mut self, key: &str, value: f64) -> Result<usize, FeedbackError> {
        if !value.is_finite() {
            return Err(FeedbackError::NonFiniteValue);
        }
        let mut count = 0;
        for slot in &mut self.slots {
            if let FeedbackSource::Level { key: bound, range } = &slot.binding.source
                && bound == key
            {
                slot.current = Some(encode(&slot.binding.target, range.normalize(value))?);
                count += 1;
            }
        }
        Ok(count)
    }

    /// Marks a device connected and forgets what it was sent, so the next
    /// drain resends every known value for it.
    pub fn attach(&mut self, device: &DeviceKey) {
        self.connected.insert(device.clone());
        for slot in &mut self.slots {
            if &slot.binding.device == device {
                slot.sent = None;
            }
        }
    }

    /// Marks a device disconnected; its changes stay pending.
    pub fn detach(&mut self, device: &DeviceKey) {
        self.connected.remove(device);
    }

    fn is_due(&self, slot: &Slot) -> bool {
        slot.current.is_some()
            && slot.current != slot.sent
            && self.connected.contains(&slot.binding.device)
    }

    /// Number of messages the next unbounded drain would emit.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.slots.iter().filter(|slot| self.is_due(slot)).count()
    }

    /// Emits at most `max` coalesced messages in binding order; the rest stay
    /// pending.
    pub fn drain(&mut self, max: usize) -> Vec<FeedbackMessage> {
        let mut out = Vec::new();
        for index in 0..self.slots.len() {
            if out.len() >= max {
                break;
            }
            if !self.is_due(&self.slots[index]) {
                continue;
            }
            let slot = &mut self.slots[index];
            let Some(payload) = slot.current.clone() else {
                continue;
            };
            slot.sent = Some(payload.clone());
            out.push(FeedbackMessage {
                device: slot.binding.device.clone(),
                binding: slot.binding.id.clone(),
                payload,
            });
        }
        out
    }
}
