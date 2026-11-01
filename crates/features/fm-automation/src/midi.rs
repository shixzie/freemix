//! Exact MIDI 1.0 byte-stream parsing and encoding plus a decoder that turns
//! channel-voice messages into typed control events.
//!
//! The parser is a pure state machine over caller-supplied bytes. It honours
//! running status, accepts real-time bytes anywhere (including between the
//! data bytes of another message and inside System Exclusive), and bounds
//! System Exclusive capture. It performs no device I/O and reads no clock.

use crate::control::{ControlEvent, ControlId, ControlValue};
use core::fmt;

/// Hard upper bound for a captured System Exclusive payload.
pub const MAX_MIDI_SYSEX_BYTES: usize = 4096;
/// Largest 14-bit value (pitch bend, 14-bit controller, song position).
pub const MIDI_14BIT_MAX: u16 = 0x3FFF;
/// Centre value of a 14-bit pitch bend.
pub const MIDI_PITCH_BEND_CENTER: u16 = 0x2000;

/// A zero-based MIDI channel (`0..=15`, displayed to users as 1..=16).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MidiChannel(u8);

impl MidiChannel {
    /// Creates a zero-based channel.
    ///
    /// # Errors
    ///
    /// Returns an error for indices above 15.
    pub const fn new(index: u8) -> Result<Self, MidiError> {
        if index > 15 {
            return Err(MidiError::InvalidChannel(index));
        }
        Ok(Self(index))
    }

    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    const fn from_status(status: u8) -> Self {
        Self(status & 0x0F)
    }
}

/// System real-time messages, which may interleave with any other bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MidiRealTime {
    TimingClock,
    Start,
    Continue,
    Stop,
    ActiveSensing,
    SystemReset,
}

impl MidiRealTime {
    const fn status(self) -> u8 {
        match self {
            Self::TimingClock => 0xF8,
            Self::Start => 0xFA,
            Self::Continue => 0xFB,
            Self::Stop => 0xFC,
            Self::ActiveSensing => 0xFE,
            Self::SystemReset => 0xFF,
        }
    }
}

/// A complete MIDI 1.0 message exactly as carried on the wire.
///
/// `NoteOn` with velocity zero is preserved here so encoding round-trips;
/// [`MidiControlDecoder`] interprets it as a release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MidiMessage {
    NoteOff {
        channel: MidiChannel,
        note: u8,
        velocity: u8,
    },
    NoteOn {
        channel: MidiChannel,
        note: u8,
        velocity: u8,
    },
    PolyPressure {
        channel: MidiChannel,
        note: u8,
        pressure: u8,
    },
    ControlChange {
        channel: MidiChannel,
        controller: u8,
        value: u8,
    },
    ProgramChange {
        channel: MidiChannel,
        program: u8,
    },
    ChannelPressure {
        channel: MidiChannel,
        pressure: u8,
    },
    /// 14-bit value; `0x2000` is centre.
    PitchBend {
        channel: MidiChannel,
        value: u16,
    },
    /// Captured payload without the `F0`/`F7` framing bytes.
    SysEx(Vec<u8>),
    TimeCodeQuarterFrame(u8),
    SongPosition(u16),
    SongSelect(u8),
    TuneRequest,
    RealTime(MidiRealTime),
}

impl MidiMessage {
    /// Encodes the message with an explicit status byte (no running status).
    ///
    /// # Errors
    ///
    /// Returns an error when a data value exceeds its 7- or 14-bit range or a
    /// System Exclusive payload is oversized or contains a status byte.
    pub fn encode(&self) -> Result<Vec<u8>, MidiError> {
        let voice = |kind: u8, channel: MidiChannel, data: &[u8]| {
            let mut bytes = vec![kind | channel.index()];
            for &byte in data {
                bytes.push(data7(byte)?);
            }
            Ok(bytes)
        };
        match self {
            Self::NoteOff {
                channel,
                note,
                velocity,
            } => voice(0x80, *channel, &[*note, *velocity]),
            Self::NoteOn {
                channel,
                note,
                velocity,
            } => voice(0x90, *channel, &[*note, *velocity]),
            Self::PolyPressure {
                channel,
                note,
                pressure,
            } => voice(0xA0, *channel, &[*note, *pressure]),
            Self::ControlChange {
                channel,
                controller,
                value,
            } => voice(0xB0, *channel, &[*controller, *value]),
            Self::ProgramChange { channel, program } => voice(0xC0, *channel, &[*program]),
            Self::ChannelPressure { channel, pressure } => voice(0xD0, *channel, &[*pressure]),
            Self::PitchBend { channel, value } => {
                let (lsb, msb) = split14(*value)?;
                voice(0xE0, *channel, &[lsb, msb])
            }
            Self::SysEx(payload) => {
                if payload.len() > MAX_MIDI_SYSEX_BYTES {
                    return Err(MidiError::SysExOverflow {
                        limit: MAX_MIDI_SYSEX_BYTES,
                    });
                }
                let mut bytes = Vec::with_capacity(payload.len() + 2);
                bytes.push(0xF0);
                for &byte in payload {
                    bytes.push(data7(byte)?);
                }
                bytes.push(0xF7);
                Ok(bytes)
            }
            Self::TimeCodeQuarterFrame(value) => Ok(vec![0xF1, data7(*value)?]),
            Self::SongPosition(value) => {
                let (lsb, msb) = split14(*value)?;
                Ok(vec![0xF2, lsb, msb])
            }
            Self::SongSelect(song) => Ok(vec![0xF3, data7(*song)?]),
            Self::TuneRequest => Ok(vec![0xF6]),
            Self::RealTime(message) => Ok(vec![message.status()]),
        }
    }
}

fn data7(byte: u8) -> Result<u8, MidiError> {
    if byte > 0x7F {
        Err(MidiError::InvalidDataByte(byte))
    } else {
        Ok(byte)
    }
}

fn split14(value: u16) -> Result<(u8, u8), MidiError> {
    if value > MIDI_14BIT_MAX {
        return Err(MidiError::Invalid14BitValue(value));
    }
    let [low, high] = value.to_le_bytes();
    // value <= 0x3FFF, so the shifted high part always fits in seven bits.
    Ok((low & 0x7F, (high << 1) | (low >> 7)))
}

fn join14(lsb: u8, msb: u8) -> u16 {
    (u16::from(msb) << 7) | u16::from(lsb)
}

/// How the parser treats System Exclusive bodies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SysExPolicy {
    /// Consume and drop every System Exclusive message without storing bytes.
    Skip,
    /// Capture payloads up to `max_bytes`; longer messages are reported once
    /// with [`MidiError::SysExOverflow`] and the remainder is discarded.
    Capture { max_bytes: usize },
}

/// Explicit MIDI parse and construction errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MidiError {
    InvalidChannel(u8),
    InvalidDataByte(u8),
    Invalid14BitValue(u16),
    /// The controller cannot be a 14-bit MSB (must be `0..=31`).
    Invalid14BitController(u8),
    InvalidSysExLimit(usize),
    /// A data byte arrived with no status (and no running status) to apply.
    UnexpectedDataByte(u8),
    /// An undefined status (`F4`, `F5`, `F9`, `FD`) was received and ignored.
    UndefinedStatus(u8),
    /// A status byte interrupted a message before all data bytes arrived.
    IncompleteMessage {
        status: u8,
    },
    /// A non-real-time status byte terminated System Exclusive without `F7`.
    UnterminatedSysEx,
    /// `F7` arrived outside System Exclusive.
    StrayEndOfExclusive,
    SysExOverflow {
        limit: usize,
    },
}

impl fmt::Display for MidiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannel(index) => write!(formatter, "MIDI channel {index} exceeds 15"),
            Self::InvalidDataByte(byte) => {
                write!(formatter, "MIDI data byte {byte:#04x} exceeds 0x7f")
            }
            Self::Invalid14BitValue(value) => {
                write!(formatter, "MIDI 14-bit value {value} exceeds 16383")
            }
            Self::Invalid14BitController(controller) => {
                write!(
                    formatter,
                    "MIDI controller {controller} cannot carry a 14-bit MSB"
                )
            }
            Self::InvalidSysExLimit(limit) => {
                write!(
                    formatter,
                    "MIDI SysEx capture limit {limit} is out of range"
                )
            }
            Self::UnexpectedDataByte(byte) => {
                write!(formatter, "MIDI data byte {byte:#04x} has no status")
            }
            Self::UndefinedStatus(status) => {
                write!(formatter, "MIDI status {status:#04x} is undefined")
            }
            Self::IncompleteMessage { status } => {
                write!(formatter, "MIDI message {status:#04x} was interrupted")
            }
            Self::UnterminatedSysEx => formatter.write_str("MIDI SysEx ended without F7"),
            Self::StrayEndOfExclusive => formatter.write_str("MIDI F7 outside SysEx"),
            Self::SysExOverflow { limit } => {
                write!(formatter, "MIDI SysEx exceeds {limit} bytes")
            }
        }
    }
}

impl std::error::Error for MidiError {}

#[derive(Clone, Debug)]
struct SysExState {
    payload: Vec<u8>,
    overflowed: bool,
}

/// Streaming MIDI 1.0 parser.
#[derive(Clone, Debug)]
pub struct MidiParser {
    policy: SysExPolicy,
    /// Current message status: a channel-voice status persists as running
    /// status; a system-common status is cleared once complete.
    status: Option<u8>,
    data: [u8; 2],
    len: usize,
    /// True once a status byte has been seen whose data is not yet complete.
    awaiting: bool,
    sysex: Option<SysExState>,
}

impl Default for MidiParser {
    fn default() -> Self {
        Self {
            policy: SysExPolicy::Skip,
            status: None,
            data: [0; 2],
            len: 0,
            awaiting: false,
            sysex: None,
        }
    }
}

const fn data_len(status: u8) -> usize {
    match status & 0xF0 {
        0xC0 | 0xD0 => 1,
        0xF0 => match status {
            0xF1 | 0xF3 => 1,
            0xF2 => 2,
            _ => 0,
        },
        _ => 2,
    }
}

impl MidiParser {
    /// Creates a parser with the given System Exclusive policy.
    ///
    /// # Errors
    ///
    /// Returns an error when a capture limit is zero or exceeds
    /// [`MAX_MIDI_SYSEX_BYTES`].
    pub fn new(policy: SysExPolicy) -> Result<Self, MidiError> {
        if let SysExPolicy::Capture { max_bytes } = policy
            && (max_bytes == 0 || max_bytes > MAX_MIDI_SYSEX_BYTES)
        {
            return Err(MidiError::InvalidSysExLimit(max_bytes));
        }
        Ok(Self {
            policy,
            ..Self::default()
        })
    }

    /// Forgets running status and any partial message or `SysEx`, e.g. after a
    /// port reconnect.
    pub fn reset(&mut self) {
        self.status = None;
        self.len = 0;
        self.awaiting = false;
        self.sysex = None;
    }

    /// Feeds bytes and returns every completed message or error in stream
    /// order. The output holds at most one entry per input byte plus one.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Result<MidiMessage, MidiError>> {
        let mut out = Vec::new();
        for &byte in bytes {
            self.push(byte, &mut out);
        }
        out
    }

    fn push(&mut self, byte: u8, out: &mut Vec<Result<MidiMessage, MidiError>>) {
        if byte >= 0xF8 {
            out.push(match byte {
                0xF8 => Ok(MidiMessage::RealTime(MidiRealTime::TimingClock)),
                0xFA => Ok(MidiMessage::RealTime(MidiRealTime::Start)),
                0xFB => Ok(MidiMessage::RealTime(MidiRealTime::Continue)),
                0xFC => Ok(MidiMessage::RealTime(MidiRealTime::Stop)),
                0xFE => Ok(MidiMessage::RealTime(MidiRealTime::ActiveSensing)),
                0xFF => Ok(MidiMessage::RealTime(MidiRealTime::SystemReset)),
                _ => Err(MidiError::UndefinedStatus(byte)),
            });
            return;
        }
        if let Some(sysex) = self.sysex.as_mut() {
            if byte < 0x80 {
                if let SysExPolicy::Capture { max_bytes } = self.policy
                    && !sysex.overflowed
                {
                    if sysex.payload.len() == max_bytes {
                        sysex.overflowed = true;
                        sysex.payload = Vec::new();
                        out.push(Err(MidiError::SysExOverflow { limit: max_bytes }));
                    } else {
                        sysex.payload.push(byte);
                    }
                }
                return;
            }
            let Some(finished) = self.sysex.take() else {
                return;
            };
            if byte == 0xF7 {
                if matches!(self.policy, SysExPolicy::Capture { .. }) && !finished.overflowed {
                    out.push(Ok(MidiMessage::SysEx(finished.payload)));
                }
                return;
            }
            out.push(Err(MidiError::UnterminatedSysEx));
            // Fall through: the interrupting status starts its own message.
        }
        if byte >= 0x80 {
            self.status_byte(byte, out);
        } else {
            self.data_byte(byte, out);
        }
    }

    fn status_byte(&mut self, byte: u8, out: &mut Vec<Result<MidiMessage, MidiError>>) {
        if self.awaiting
            && let Some(status) = self.status
        {
            out.push(Err(MidiError::IncompleteMessage { status }));
        }
        self.len = 0;
        self.awaiting = false;
        match byte {
            0x80..=0xEF | 0xF1..=0xF3 => {
                self.status = Some(byte);
                self.awaiting = true;
            }
            0xF0 => {
                self.status = None;
                self.sysex = Some(SysExState {
                    payload: Vec::new(),
                    overflowed: false,
                });
            }
            0xF6 => {
                self.status = None;
                out.push(Ok(MidiMessage::TuneRequest));
            }
            0xF7 => {
                self.status = None;
                out.push(Err(MidiError::StrayEndOfExclusive));
            }
            _ => {
                // F4/F5: undefined system common; ignore and clear running status.
                self.status = None;
                out.push(Err(MidiError::UndefinedStatus(byte)));
            }
        }
    }

    fn data_byte(&mut self, byte: u8, out: &mut Vec<Result<MidiMessage, MidiError>>) {
        let Some(status) = self.status else {
            out.push(Err(MidiError::UnexpectedDataByte(byte)));
            return;
        };
        self.data[self.len] = byte;
        self.len += 1;
        self.awaiting = true;
        if self.len < data_len(status) {
            return;
        }
        self.len = 0;
        self.awaiting = false;
        let [first, second] = self.data;
        let channel = MidiChannel::from_status(status);
        let message = match status & 0xF0 {
            0x80 => MidiMessage::NoteOff {
                channel,
                note: first,
                velocity: second,
            },
            0x90 => MidiMessage::NoteOn {
                channel,
                note: first,
                velocity: second,
            },
            0xA0 => MidiMessage::PolyPressure {
                channel,
                note: first,
                pressure: second,
            },
            0xB0 => MidiMessage::ControlChange {
                channel,
                controller: first,
                value: second,
            },
            0xC0 => MidiMessage::ProgramChange {
                channel,
                program: first,
            },
            0xD0 => MidiMessage::ChannelPressure {
                channel,
                pressure: first,
            },
            0xE0 => MidiMessage::PitchBend {
                channel,
                value: join14(first, second),
            },
            _ => {
                // System common carries no running status.
                self.status = None;
                match status {
                    0xF1 => MidiMessage::TimeCodeQuarterFrame(first),
                    0xF2 => MidiMessage::SongPosition(join14(first, second)),
                    _ => MidiMessage::SongSelect(first),
                }
            }
        };
        out.push(Ok(message));
    }
}

/// Converts channel-voice messages into typed control events.
///
/// Controllers `0..=31` can be marked 14-bit. For a 14-bit controller the MSB
/// (controller `n`) is held, emitting nothing, until the paired LSB
/// (controller `n + 32`) arrives; the LSB then emits the combined
/// `msb << 7 | lsb` using the latest MSB, defaulting to zero. Holding the MSB
/// across ticks keeps a pair that straddles a tick boundary from surfacing the
/// `msb << 7` intermediate.
#[derive(Clone, Debug, Default)]
pub struct MidiControlDecoder {
    high_resolution: [u32; 16],
    msb: [[u8; 32]; 16],
}

impl MidiControlDecoder {
    /// Marks controller `msb_controller` (and its `+32` LSB) as a 14-bit pair.
    ///
    /// # Errors
    ///
    /// Returns an error for controllers above 31.
    pub fn enable_14bit(
        &mut self,
        channel: MidiChannel,
        msb_controller: u8,
    ) -> Result<(), MidiError> {
        if msb_controller > 31 {
            return Err(MidiError::Invalid14BitController(msb_controller));
        }
        self.high_resolution[usize::from(channel.index())] |= 1 << msb_controller;
        Ok(())
    }

    fn is_high_resolution(&self, channel: MidiChannel, controller: u8) -> bool {
        controller < 32
            && self.high_resolution[usize::from(channel.index())] & (1 << controller) != 0
    }

    /// Forgets stored MSBs (configuration is retained), e.g. on reconnect.
    pub fn reset(&mut self) {
        self.msb = [[0; 32]; 16];
    }

    /// Decodes one message into at most one control event. System, `SysEx`,
    /// and real-time messages produce no control event.
    pub fn decode(&mut self, message: &MidiMessage) -> Option<ControlEvent> {
        let event = |control, value| Some(ControlEvent { control, value });
        match *message {
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => event(
                ControlId::MidiNote { channel, note },
                ControlValue::Button {
                    pressed: velocity != 0,
                    velocity,
                },
            ),
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => event(
                ControlId::MidiNote { channel, note },
                ControlValue::Button {
                    pressed: false,
                    velocity,
                },
            ),
            MidiMessage::PolyPressure {
                channel,
                note,
                pressure,
            } => event(
                ControlId::MidiPolyPressure { channel, note },
                ControlValue::Absolute {
                    raw: u16::from(pressure),
                    max: 127,
                },
            ),
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => self.control_change(channel, controller, value),
            MidiMessage::ProgramChange { channel, program } => event(
                ControlId::MidiProgram { channel, program },
                ControlValue::Button {
                    pressed: true,
                    velocity: 127,
                },
            ),
            MidiMessage::ChannelPressure { channel, pressure } => event(
                ControlId::MidiChannelPressure { channel },
                ControlValue::Absolute {
                    raw: u16::from(pressure),
                    max: 127,
                },
            ),
            MidiMessage::PitchBend { channel, value } => event(
                ControlId::MidiPitchBend { channel },
                ControlValue::Absolute {
                    raw: value,
                    max: MIDI_14BIT_MAX,
                },
            ),
            _ => None,
        }
    }

    fn control_change(
        &mut self,
        channel: MidiChannel,
        controller: u8,
        value: u8,
    ) -> Option<ControlEvent> {
        let slot = usize::from(channel.index());
        if self.is_high_resolution(channel, controller) {
            // Hold the MSB until its LSB arrives, even across ticks: emitting
            // `msb << 7` here would jump the control by up to 127 steps and
            // back on the next message.
            self.msb[slot][usize::from(controller)] = value;
            return None;
        }
        if (32..64).contains(&controller) && self.is_high_resolution(channel, controller - 32) {
            let msb_controller = controller - 32;
            let msb = self.msb[slot][usize::from(msb_controller)];
            return Some(ControlEvent {
                control: ControlId::MidiCc14 {
                    channel,
                    controller: msb_controller,
                },
                value: ControlValue::Absolute {
                    raw: join14(value, msb),
                    max: MIDI_14BIT_MAX,
                },
            });
        }
        Some(ControlEvent {
            control: ControlId::MidiCc {
                channel,
                controller,
            },
            value: ControlValue::Absolute {
                raw: u16::from(value),
                max: 127,
            },
        })
    }
}
