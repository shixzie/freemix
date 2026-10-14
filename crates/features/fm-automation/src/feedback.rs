//! Activator feedback: typed production state drives exact outgoing MIDI and
//! OSC bytes (LEDs, motor faders, OSC surfaces), coalesced per control.
//!
//! Observations overwrite the latest value for each bound control; draining
//! emits only controls whose latest encoded bytes differ from what was last
//! sent to a connected device. Reattaching a device forces a full resend.
//!
//! For high-rate surfaces, [`FeedbackRouter::drain_tick`] adds a per-device
//! message budget per tick (round-robin, so no control starves), a minimum
//! resend interval per binding, and touch-aware echo suppression: a fader the
//! operator is holding receives no motor feedback, and its authoritative value
//! is resent once on release. [`FeedbackRouter::acknowledge_level`] records a
//! value the device already shows (because the operator just moved it), so
//! the resulting state change is not echoed back.

use crate::control_map::{BindingId, DeviceKey, Span};
use crate::controller::{ActivatorRule, TallySnapshot};
use crate::midi::{MIDI_14BIT_MAX, MidiChannel, MidiMessage};
use crate::osc::{
    OscArg, OscLimits, OscMessage, OscPacket, encode_osc_packet, validate_osc_address,
};
use core::fmt;
use core::num::NonZeroUsize;
use fm_types::{MediaDuration, MediaTimestamp};
use std::collections::{BTreeMap, BTreeSet};

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
    UnknownBinding(BindingId),
    /// The binding does not follow a level, so it cannot acknowledge one.
    NotALevel(BindingId),
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
            Self::UnknownBinding(id) => {
                write!(formatter, "feedback binding {} is unknown", id.as_str())
            }
            Self::NotALevel(id) => {
                write!(
                    formatter,
                    "feedback binding {} follows no level",
                    id.as_str()
                )
            }
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
    /// The operator is holding the physical control; feedback is withheld.
    touched: bool,
    last_sent_at: Option<MediaTimestamp>,
}

/// Pacing applied by [`FeedbackRouter::drain_tick`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeedbackSchedule {
    /// Maximum messages emitted to one device per tick.
    pub per_device_budget: NonZeroUsize,
    /// Minimum caller ticks between two sends of one binding (`0` disables).
    /// Changes inside the interval coalesce and go out once it elapses.
    pub min_interval: MediaDuration,
}

/// What one [`FeedbackRouter::drain_tick`] emitted and withheld.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedbackTick {
    pub messages: Vec<FeedbackMessage>,
    /// Changed bindings left pending because their device's budget ran out.
    pub deferred_budget: usize,
    /// Changed bindings left pending inside their minimum interval.
    pub deferred_interval: usize,
    /// Changed bindings withheld because the operator is touching them.
    pub withheld_touched: usize,
}

/// Derives coalesced outgoing feedback from typed state observations.
#[derive(Clone, Debug, Default)]
pub struct FeedbackRouter {
    slots: Vec<Slot>,
    connected: BTreeSet<DeviceKey>,
    /// Round-robin start position per device, as an index into that
    /// device's bindings in binding order.
    cursors: BTreeMap<DeviceKey, usize>,
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
            touched: false,
            last_sent_at: None,
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
    /// drain resends every known value for it. Touch state and resend
    /// intervals restart.
    pub fn attach(&mut self, device: &DeviceKey) {
        self.connected.insert(device.clone());
        self.cursors.remove(device);
        for slot in &mut self.slots {
            if &slot.binding.device == device {
                slot.sent = None;
                slot.touched = false;
                slot.last_sent_at = None;
            }
        }
    }

    /// Marks a device disconnected; its changes stay pending.
    pub fn detach(&mut self, device: &DeviceKey) {
        self.connected.remove(device);
        for slot in &mut self.slots {
            if &slot.binding.device == device {
                slot.touched = false;
            }
        }
    }

    fn slot_mut(&mut self, id: &BindingId) -> Result<&mut Slot, FeedbackError> {
        self.slots
            .iter_mut()
            .find(|slot| &slot.binding.id == id)
            .ok_or_else(|| FeedbackError::UnknownBinding(id.clone()))
    }

    /// Records whether the operator is touching the control behind `id`
    /// (e.g. a motor fader's touch sense). While touched, nothing is sent to
    /// it, so the motor never fights the hand and the operator's own moves
    /// are not echoed. On release the latest authoritative value is resent
    /// once, even if it equals what was last sent, because the physical
    /// position has drifted from it. Returns whether the touch state changed.
    ///
    /// # Errors
    ///
    /// Returns [`FeedbackError::UnknownBinding`] for an unknown binding.
    pub fn set_touched(&mut self, id: &BindingId, touched: bool) -> Result<bool, FeedbackError> {
        let slot = self.slot_mut(id)?;
        if slot.touched == touched {
            return Ok(false);
        }
        slot.touched = touched;
        if !touched {
            slot.sent = None;
            slot.last_sent_at = None;
        }
        Ok(true)
    }

    /// Whether the control behind `id` is currently touched.
    #[must_use]
    pub fn is_touched(&self, id: &BindingId) -> bool {
        self.slots
            .iter()
            .any(|slot| &slot.binding.id == id && slot.touched)
    }

    /// Records that the device already shows `value` (in the binding's level
    /// units) on the control behind `id`, typically because the operator just
    /// moved that control. A later [`Self::observe_level`] of the same value
    /// then encodes identically and is not echoed back; a different value
    /// (clamping, another operator) is still sent.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown or non-level bindings and non-finite
    /// values.
    pub fn acknowledge_level(&mut self, id: &BindingId, value: f64) -> Result<(), FeedbackError> {
        if !value.is_finite() {
            return Err(FeedbackError::NonFiniteValue);
        }
        let slot = self.slot_mut(id)?;
        let FeedbackSource::Level { range, .. } = &slot.binding.source else {
            return Err(FeedbackError::NotALevel(id.clone()));
        };
        slot.sent = Some(encode(&slot.binding.target, range.normalize(value))?);
        Ok(())
    }

    fn is_due(&self, slot: &Slot) -> bool {
        slot.current.is_some()
            && slot.current != slot.sent
            && !slot.touched
            && self.connected.contains(&slot.binding.device)
    }

    /// Emits one tick of paced feedback at caller time `now`.
    ///
    /// Each connected device receives at most `schedule.per_device_budget`
    /// messages. Its bindings are visited round-robin from where the previous
    /// tick stopped, so a busy control cannot starve the others. A binding
    /// sent less than `schedule.min_interval` ago stays pending (still
    /// coalescing) until the interval elapses; time running backwards makes
    /// it due. Touched bindings are withheld. Output is grouped by device in
    /// key order.
    pub fn drain_tick(&mut self, now: MediaTimestamp, schedule: FeedbackSchedule) -> FeedbackTick {
        let mut tick = FeedbackTick::default();
        let mut by_device: BTreeMap<DeviceKey, Vec<usize>> = BTreeMap::new();
        for (index, slot) in self.slots.iter().enumerate() {
            if self.connected.contains(&slot.binding.device) {
                by_device
                    .entry(slot.binding.device.clone())
                    .or_default()
                    .push(index);
            }
        }
        let interval = i128::from(schedule.min_interval.ticks());
        for (device, indices) in by_device {
            let start = self.cursors.get(&device).copied().unwrap_or(0) % indices.len();
            let mut emitted = 0;
            let mut next_cursor = None;
            for offset in 0..indices.len() {
                let position = (start + offset) % indices.len();
                let slot = &self.slots[indices[position]];
                if slot.current.is_none() || slot.current == slot.sent {
                    continue;
                }
                if slot.touched {
                    tick.withheld_touched += 1;
                    continue;
                }
                let waiting = slot.last_sent_at.is_some_and(|sent| {
                    let elapsed = i128::from(now.ticks()) - i128::from(sent.ticks());
                    (0..interval).contains(&elapsed)
                });
                if waiting {
                    tick.deferred_interval += 1;
                    continue;
                }
                if emitted >= schedule.per_device_budget.get() {
                    tick.deferred_budget += 1;
                    continue;
                }
                let slot = &mut self.slots[indices[position]];
                let Some(payload) = slot.current.clone() else {
                    continue;
                };
                slot.sent = Some(payload.clone());
                slot.last_sent_at = Some(now);
                emitted += 1;
                next_cursor = Some(position + 1);
                tick.messages.push(FeedbackMessage {
                    device: slot.binding.device.clone(),
                    binding: slot.binding.id.clone(),
                    payload,
                });
            }
            if let Some(cursor) = next_cursor {
                self.cursors.insert(device, cursor % indices.len());
            }
        }
        tick
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
