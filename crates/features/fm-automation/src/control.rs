//! Transport-neutral control events shared by MIDI, OSC, and HID adapters.

use crate::midi::MidiChannel;

/// Identity of one physical control on one device.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ControlId {
    MidiNote {
        channel: MidiChannel,
        note: u8,
    },
    MidiCc {
        channel: MidiChannel,
        controller: u8,
    },
    /// A 14-bit controller pair identified by its MSB controller (`0..=31`).
    MidiCc14 {
        channel: MidiChannel,
        controller: u8,
    },
    MidiPitchBend {
        channel: MidiChannel,
    },
    MidiChannelPressure {
        channel: MidiChannel,
    },
    MidiPolyPressure {
        channel: MidiChannel,
        note: u8,
    },
    MidiProgram {
        channel: MidiChannel,
        program: u8,
    },
    /// An OSC address. Bindings hold concrete addresses; incoming events may
    /// carry an OSC address pattern that is matched against them.
    Osc(String),
    /// A HID usage, e.g. a jog wheel, shuttle ring, or gamepad button. HID
    /// report parsing is outside this crate; adapters supply typed values.
    Hid {
        usage_page: u16,
        usage: u16,
    },
}

/// A typed control value as decoded from the transport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlValue {
    /// A momentary button or note. MIDI Note On with velocity 0 decodes as a
    /// release.
    Button { pressed: bool, velocity: u8 },
    /// An absolute position `0..=max` (7-bit CC, 14-bit CC, pitch bend).
    Absolute { raw: u16, max: u16 },
    /// A signed integer (OSC `i`, HID shuttle position).
    Integer(i32),
    /// A float (OSC `f`).
    Float(f32),
    /// A signed relative step (HID jog wheel or a pre-decoded encoder).
    Delta(i32),
}

/// One decoded control event, before device association or mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlEvent {
    pub control: ControlId,
    pub value: ControlValue,
}
