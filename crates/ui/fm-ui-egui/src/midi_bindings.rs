//! MIDI controller bindings (RC-005): notes and controllers of a MIDI
//! controller drive the same console actions the key profiles give keys
//! (see [`crate::console_keymap`]), on this console only.
//!
//! The host reads the controllers (on Linux, the ALSA raw MIDI ports) and
//! hands the console each note and controller message as a [`MidiInput`].
//! A binding fires on a press: a note on with a nonzero velocity, or a
//! controller crossing to 64 or more from below (a button sending 127 and
//! 0, or a fader pushed past half); the release re-arms it. Learning an
//! action binds it to the next press, taking the trigger from any other
//! action that had it.
//!
//! The bindings are console preferences, saved with the key profiles:
//!
//! ```text
//! keymap.midi.note.1.60 = cut
//! keymap.midi.cc.10.7 = fade
//! keymap.midi.nrpn.1.1234 = cut
//! ```
//!
//! where the trigger is `note`, `cc`, `nrpn`, or `rpn`, the channel `1`
//! through `16`, and the note or controller number `0` through `127` (`0`
//! through `16383` for an NRPN or RPN parameter), and the value an
//! [`ConsoleAction::token`].

use std::collections::{BTreeMap, BTreeSet};

use crate::console_keymap::ConsoleAction;
use crate::l10n::{Arg, Locale, Msg};

/// Most MIDI bindings one console keeps.
pub const MAX_MIDI_BINDINGS: usize = 64;
const PREFIX: &str = "keymap.midi.";
const VALUE_PREFIX: &str = "keymap.midi.value.";

/// Which kind of MIDI message a trigger answers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MidiControlKind {
    /// A note (a pad or key).
    Note,
    /// A control change (a button, knob, or fader).
    Control,
    /// A non-registered parameter (a 14-bit controller number).
    Nrpn,
    /// A registered parameter (a 14-bit controller number).
    Rpn,
}

/// One note or controller of one MIDI channel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MidiTrigger {
    pub kind: MidiControlKind,
    /// The channel, `1..=16`.
    pub channel: u8,
    /// The note or controller number: `0..=127` for notes and controls,
    /// `0..=16383` for NRPN and RPN.
    pub number: u16,
}

impl MidiTrigger {
    /// A trigger, or `None` for a channel outside `1..=16` or a number
    /// above 127.
    #[must_use]
    pub const fn new(kind: MidiControlKind, channel: u8, number: u16) -> Option<Self> {
        let maximum = match kind {
            MidiControlKind::Note | MidiControlKind::Control => 127,
            MidiControlKind::Nrpn | MidiControlKind::Rpn => 16_383,
        };
        if channel == 0 || channel > 16 || number > maximum {
            return None;
        }
        Some(Self {
            kind,
            channel,
            number,
        })
    }

    /// The trigger's name in the preferences file: `note.1.60`, `cc.10.7`.
    #[must_use]
    pub fn token(self) -> String {
        let kind = match self.kind {
            MidiControlKind::Note => "note",
            MidiControlKind::Control => "cc",
            MidiControlKind::Nrpn => "nrpn",
            MidiControlKind::Rpn => "rpn",
        };
        format!("{kind}.{}.{}", self.channel, self.number)
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        let mut parts = token.split('.');
        let kind = match parts.next()? {
            "note" => MidiControlKind::Note,
            "cc" => MidiControlKind::Control,
            "nrpn" => MidiControlKind::Nrpn,
            "rpn" => MidiControlKind::Rpn,
            _ => return None,
        };
        let number = |text: &str| {
            let value: u16 = text.parse().ok()?;
            (value.to_string() == text).then_some(value)
        };
        let channel = u8::try_from(number(parts.next()?)?).ok()?;
        let value = number(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Self::new(kind, channel, value)
    }

    /// The trigger as the console shows it: `Note 60 · ch 1`.
    #[must_use]
    pub fn text(self, l: Locale) -> String {
        let msg = match self.kind {
            MidiControlKind::Note => Msg::MidiNote,
            MidiControlKind::Control => Msg::MidiControl,
            MidiControlKind::Nrpn => Msg::MidiNrpn,
            MidiControlKind::Rpn => Msg::MidiRpn,
        };
        l.format(
            msg,
            &[
                ("number", Arg::Count(u64::from(self.number))),
                ("channel", Arg::Count(u64::from(self.channel))),
            ],
        )
    }
}

/// One note or controller message a controller sent: the note's velocity
/// (0 is a note off) or the controller's value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MidiInput {
    pub trigger: MidiTrigger,
    pub value: u8,
}

/// One bound control's lamp: `value` 127 lights it, 0 darkens it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MidiFeedback {
    pub trigger: MidiTrigger,
    pub value: u8,
}

/// A continuous console value a MIDI fader or knob can drive.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MidiValueTarget {
    /// The monitor bus's headphone level; 0 is the fader's floor and 127
    /// its ceiling.
    MonitorLevel,
}

impl MidiValueTarget {
    /// Every value target, in the order the profile editor lists them.
    #[must_use]
    pub const fn all() -> [Self; 1] {
        [Self::MonitorLevel]
    }

    /// The target's name in the preferences file.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::MonitorLevel => "monitor_level",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|target| target.token() == token)
    }
}

/// The console's MIDI bindings; see the module documentation.
#[derive(Clone, Debug, Default)]
#[allow(clippy::zero_sized_map_values)] // value targets stay unit enums
pub struct MidiBindings {
    bindings: BTreeMap<MidiTrigger, ConsoleAction>,
    /// Continuous controls bound to a value rather than an action.
    values: BTreeMap<MidiTrigger, MidiValueTarget>,
    /// Triggers held since their press (runtime only).
    held: BTreeSet<MidiTrigger>,
}

impl PartialEq for MidiBindings {
    fn eq(&self, other: &Self) -> bool {
        self.bindings == other.bindings
    }
}

impl Eq for MidiBindings {}

impl MidiBindings {
    /// Every binding, by trigger.
    pub fn bindings(&self) -> impl Iterator<Item = (MidiTrigger, ConsoleAction)> + '_ {
        self.bindings
            .iter()
            .map(|(trigger, action)| (*trigger, *action))
    }

    /// The trigger bound to `action`, if any.
    #[must_use]
    pub fn trigger_of(&self, action: ConsoleAction) -> Option<MidiTrigger> {
        self.bindings
            .iter()
            .find_map(|(trigger, bound)| (*bound == action).then_some(*trigger))
    }

    /// The action `trigger` drives, if any.
    #[must_use]
    pub fn action(&self, trigger: MidiTrigger) -> Option<ConsoleAction> {
        self.bindings.get(&trigger).copied()
    }

    /// Binds `trigger` to `action`, taking it from any other action or
    /// value and dropping `action`'s previous trigger; `false` (and nothing
    /// changes) when the console already keeps [`MAX_MIDI_BINDINGS`] other
    /// bindings.
    pub fn bind(&mut self, trigger: MidiTrigger, action: ConsoleAction) -> bool {
        let previous = self.trigger_of(action);
        let grows = previous.is_none() && !self.bindings.contains_key(&trigger);
        if grows && self.bindings.len() + self.values.len() >= MAX_MIDI_BINDINGS {
            return false;
        }
        if let Some(previous) = previous {
            self.bindings.remove(&previous);
        }
        self.values.remove(&trigger);
        self.bindings.insert(trigger, action);
        true
    }

    /// The trigger bound to value `target`, if any.
    #[must_use]
    pub fn value_trigger(&self, target: MidiValueTarget) -> Option<MidiTrigger> {
        self.values
            .iter()
            .find_map(|(trigger, bound)| (*bound == target).then_some(*trigger))
    }

    /// The value `trigger` drives, if any.
    #[must_use]
    pub fn value_target(&self, trigger: MidiTrigger) -> Option<MidiValueTarget> {
        self.values.get(&trigger).copied()
    }

    /// Every value binding, by trigger.
    pub fn value_bindings(&self) -> impl Iterator<Item = (MidiTrigger, MidiValueTarget)> + '_ {
        self.values
            .iter()
            .map(|(trigger, target)| (*trigger, *target))
    }

    /// Binds `trigger` to value `target`, taking it from any action or other
    /// value and dropping `target`'s previous trigger; `false` (and nothing
    /// changes) when the console already keeps [`MAX_MIDI_BINDINGS`] other
    /// bindings.
    pub fn bind_value(&mut self, trigger: MidiTrigger, target: MidiValueTarget) -> bool {
        let previous = self.value_trigger(target);
        let grows = previous.is_none() && !self.values.contains_key(&trigger);
        if grows && self.bindings.len() + self.values.len() >= MAX_MIDI_BINDINGS {
            return false;
        }
        if let Some(previous) = previous {
            self.values.remove(&previous);
        }
        self.bindings.remove(&trigger);
        self.values.insert(trigger, target);
        true
    }

    /// Drops `target`'s value binding; whether it had one.
    pub fn unbind_value(&mut self, target: MidiValueTarget) -> bool {
        match self.value_trigger(target) {
            Some(trigger) => self.values.remove(&trigger).is_some(),
            None => false,
        }
    }

    /// Drops `action`'s binding; whether it had one.
    pub fn unbind(&mut self, action: ConsoleAction) -> bool {
        match self.trigger_of(action) {
            Some(trigger) => self.bindings.remove(&trigger).is_some(),
            None => false,
        }
    }

    /// The presses among `inputs`, in order: a note on with a nonzero
    /// velocity or a controller reaching 64 from below, each until its
    /// release (a note off, a zero velocity, or a controller below 64).
    pub fn presses(&mut self, inputs: &[MidiInput]) -> Vec<MidiTrigger> {
        let mut presses = Vec::new();
        for input in inputs {
            let down = match input.trigger.kind {
                MidiControlKind::Note => input.value > 0,
                MidiControlKind::Control | MidiControlKind::Nrpn | MidiControlKind::Rpn => {
                    input.value >= 64
                }
            };
            if !down {
                self.held.remove(&input.trigger);
            } else if self.held.insert(input.trigger) {
                presses.push(input.trigger);
            }
        }
        presses
    }

    /// The bindings as preference lines, by trigger.
    #[must_use]
    pub fn preference_entries(&self) -> Vec<(String, String)> {
        self.bindings
            .iter()
            .map(|(trigger, action)| (format!("{PREFIX}{}", trigger.token()), action.token()))
            .chain(self.values.iter().map(|(trigger, target)| {
                (
                    format!("{VALUE_PREFIX}{}", trigger.token()),
                    target.token().to_owned(),
                )
            }))
            .collect()
    }

    /// Applies one preference line; `false` for a key that is not a MIDI
    /// binding or an unusable one.
    pub fn apply_preference(&mut self, key: &str, value: &str) -> bool {
        if let Some(rest) = key.strip_prefix(VALUE_PREFIX) {
            let Some(trigger) = MidiTrigger::from_token(rest) else {
                return false;
            };
            let Some(target) = MidiValueTarget::from_token(value) else {
                return false;
            };
            return self.bind_value(trigger, target);
        }
        let Some(trigger) = key.strip_prefix(PREFIX).and_then(MidiTrigger::from_token) else {
            return false;
        };
        let Some(action) = ConsoleAction::from_token(value) else {
            return false;
        };
        self.bind(trigger, action)
    }

    /// Whether `key` names a MIDI binding line.
    #[must_use]
    pub fn is_preference_key(key: &str) -> bool {
        key.starts_with(PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(channel: u8, number: u16) -> MidiTrigger {
        MidiTrigger::new(MidiControlKind::Note, channel, number).unwrap()
    }

    fn cc(channel: u8, number: u16) -> MidiTrigger {
        MidiTrigger::new(MidiControlKind::Control, channel, number).unwrap()
    }

    fn parameter(kind: MidiControlKind, channel: u8, number: u16) -> MidiTrigger {
        MidiTrigger::new(kind, channel, number).unwrap()
    }

    #[test]
    fn triggers_are_bounded_and_round_trip_their_tokens() {
        assert_eq!(MidiTrigger::new(MidiControlKind::Note, 0, 60), None);
        assert_eq!(MidiTrigger::new(MidiControlKind::Note, 17, 60), None);
        assert_eq!(MidiTrigger::new(MidiControlKind::Control, 1, 128), None);
        for trigger in [note(1, 0), note(16, 127), cc(10, 7)] {
            assert_eq!(MidiTrigger::from_token(&trigger.token()), Some(trigger));
        }
        assert_eq!(note(1, 60).token(), "note.1.60");
        assert_eq!(cc(10, 7).token(), "cc.10.7");
        for bad in ["note.1", "note.01.60", "pad.1.60", "cc.1.7.0", "note.0.1"] {
            assert_eq!(MidiTrigger::from_token(bad), None, "{bad}");
        }
        assert_eq!(note(1, 60).text(Locale::ENGLISH), "Note 60 · ch 1");
        assert_eq!(cc(10, 7).text(Locale::ENGLISH), "CC 7 · ch 10");
    }

    #[test]
    fn parameters_use_their_full_fourteen_bits() {
        let nrpn = parameter(MidiControlKind::Nrpn, 3, 0x1234);
        let rpn = parameter(MidiControlKind::Rpn, 16, 16_383);
        assert_eq!(nrpn.token(), "nrpn.3.4660");
        assert_eq!(rpn.token(), "rpn.16.16383");
        for trigger in [nrpn, rpn] {
            assert_eq!(MidiTrigger::from_token(&trigger.token()), Some(trigger));
        }
        assert_eq!(nrpn.text(Locale::ENGLISH), "NRPN 4,660 · ch 3");
        assert_eq!(rpn.text(Locale::ENGLISH), "RPN 16,383 · ch 16");
        // A note or controller stops at 127; a parameter stops at 16383.
        assert_eq!(MidiTrigger::new(MidiControlKind::Note, 1, 128), None);
        assert_eq!(MidiTrigger::new(MidiControlKind::Control, 1, 128), None);
        assert_eq!(MidiTrigger::new(MidiControlKind::Nrpn, 1, 16_384), None);
        assert_eq!(MidiTrigger::new(MidiControlKind::Rpn, 1, 16_384), None);
        assert_eq!(MidiTrigger::new(MidiControlKind::Nrpn, 0, 1), None);
        assert_eq!(MidiTrigger::new(MidiControlKind::Rpn, 17, 1), None);
        for bad in ["nrpn.1.16384", "rpn.01.1", "nrpn.1", "nrpn.1.1.1"] {
            assert_eq!(MidiTrigger::from_token(bad), None, "{bad}");
        }
    }

    #[test]
    fn presses_fire_once_until_released() {
        let mut bindings = MidiBindings::default();
        let input = |trigger, value| MidiInput { trigger, value };
        let pad = note(1, 60);
        let button = cc(1, 20);
        let presses = bindings.presses(&[
            input(pad, 100),
            input(pad, 90),
            input(button, 127),
            input(button, 80),
            input(pad, 0),
            input(pad, 1),
            input(button, 10),
            input(button, 64),
        ]);
        assert_eq!(presses, [pad, button, pad, button]);
    }

    #[test]
    fn binding_moves_a_trigger_and_round_trips_through_preferences() {
        let mut bindings = MidiBindings::default();
        assert!(bindings.bind(note(1, 60), ConsoleAction::Cut));
        assert!(bindings.bind(cc(10, 7), ConsoleAction::Fade));
        // The pad moves to Fade; Fade's controller and Cut's binding go.
        assert!(bindings.bind(note(1, 60), ConsoleAction::Fade));
        assert_eq!(bindings.action(note(1, 60)), Some(ConsoleAction::Fade));
        assert_eq!(bindings.trigger_of(ConsoleAction::Cut), None);
        assert_eq!(bindings.action(cc(10, 7)), None);
        assert!(bindings.bind(cc(10, 7), ConsoleAction::Preview(2)));
        let entries = bindings.preference_entries();
        assert_eq!(
            entries,
            [
                ("keymap.midi.note.1.60".to_owned(), "fade".to_owned()),
                ("keymap.midi.cc.10.7".to_owned(), "preview.2".to_owned()),
            ]
        );
        let mut read = MidiBindings::default();
        for (key, value) in &entries {
            assert!(read.apply_preference(key, value), "{key}");
        }
        assert_eq!(read, bindings);
        assert!(!read.apply_preference("keymap.midi.note.1.61", "warp"));
        assert!(!read.apply_preference("keymap.midi.drum.1.61", "cut"));
        assert!(read.unbind(ConsoleAction::Fade));
        assert!(!read.unbind(ConsoleAction::Fade));
        // The console keeps at most 64 bindings.
        let mut full = MidiBindings::default();
        for (number, action) in ConsoleAction::all().into_iter().enumerate() {
            let number = u16::try_from(number).unwrap();
            assert_eq!(
                full.bind(note(2, number), action),
                usize::from(number) < MAX_MIDI_BINDINGS
            );
        }
    }

    #[test]
    fn a_value_binding_round_trips_and_is_exclusive() {
        let mut bindings = MidiBindings::default();
        let trigger = cc(1, 7);
        assert!(bindings.bind_value(trigger, MidiValueTarget::MonitorLevel));
        assert_eq!(
            bindings.value_target(trigger),
            Some(MidiValueTarget::MonitorLevel)
        );
        assert_eq!(
            bindings.value_trigger(MidiValueTarget::MonitorLevel),
            Some(trigger)
        );
        assert_eq!(bindings.action(trigger), None);
        let entries = bindings.preference_entries();
        assert_eq!(
            entries,
            [(
                "keymap.midi.value.cc.1.7".to_owned(),
                "monitor_level".to_owned()
            )]
        );
        let mut read = MidiBindings::default();
        for (key, value) in &entries {
            assert!(read.apply_preference(key, value), "{key}");
        }
        assert_eq!(read, bindings);
        // An action and a value cannot share one control.
        assert!(bindings.bind(trigger, ConsoleAction::Cut));
        assert!(bindings.value_target(trigger).is_none());
        assert_eq!(bindings.action(trigger), Some(ConsoleAction::Cut));
        assert!(bindings.bind_value(trigger, MidiValueTarget::MonitorLevel));
        assert_eq!(bindings.action(trigger), None);
        assert!(bindings.unbind_value(MidiValueTarget::MonitorLevel));
        assert!(!bindings.unbind_value(MidiValueTarget::MonitorLevel));
    }
}
