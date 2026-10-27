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
//! ```
//!
//! where the trigger is `note` or `cc`, the channel `1` through `16`, and
//! the note or controller number `0` through `127`, and the value an
//! [`ConsoleAction::token`].

use std::collections::{BTreeMap, BTreeSet};

use crate::console_keymap::ConsoleAction;
use crate::l10n::{Arg, Locale, Msg};

/// Most MIDI bindings one console keeps.
pub const MAX_MIDI_BINDINGS: usize = 64;
const PREFIX: &str = "keymap.midi.";

/// Which kind of MIDI message a trigger answers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MidiControlKind {
    /// A note (a pad or key).
    Note,
    /// A control change (a button, knob, or fader).
    Control,
}

/// One note or controller of one MIDI channel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MidiTrigger {
    pub kind: MidiControlKind,
    /// The channel, `1..=16`.
    pub channel: u8,
    /// The note or controller number, `0..=127`.
    pub number: u8,
}

impl MidiTrigger {
    /// A trigger, or `None` for a channel outside `1..=16` or a number
    /// above 127.
    #[must_use]
    pub const fn new(kind: MidiControlKind, channel: u8, number: u8) -> Option<Self> {
        if channel == 0 || channel > 16 || number > 127 {
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
            _ => return None,
        };
        let number = |text: &str| {
            let value: u8 = text.parse().ok()?;
            (value.to_string() == text).then_some(value)
        };
        let channel = number(parts.next()?)?;
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

/// The console's MIDI bindings; see the module documentation.
#[derive(Clone, Debug, Default)]
pub struct MidiBindings {
    bindings: BTreeMap<MidiTrigger, ConsoleAction>,
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

    /// Binds `trigger` to `action`, taking it from any other action and
    /// dropping `action`'s previous trigger; `false` (and nothing changes)
    /// when the console already keeps [`MAX_MIDI_BINDINGS`] other bindings.
    pub fn bind(&mut self, trigger: MidiTrigger, action: ConsoleAction) -> bool {
        let previous = self.trigger_of(action);
        let grows = previous.is_none() && !self.bindings.contains_key(&trigger);
        if grows && self.bindings.len() >= MAX_MIDI_BINDINGS {
            return false;
        }
        if let Some(previous) = previous {
            self.bindings.remove(&previous);
        }
        self.bindings.insert(trigger, action);
        true
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
                MidiControlKind::Control => input.value >= 64,
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
            .collect()
    }

    /// Applies one preference line; `false` for a key that is not a MIDI
    /// binding or an unusable one.
    pub fn apply_preference(&mut self, key: &str, value: &str) -> bool {
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

    fn note(channel: u8, number: u8) -> MidiTrigger {
        MidiTrigger::new(MidiControlKind::Note, channel, number).unwrap()
    }

    fn cc(channel: u8, number: u8) -> MidiTrigger {
        MidiTrigger::new(MidiControlKind::Control, channel, number).unwrap()
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
            let number = u8::try_from(number).unwrap();
            assert_eq!(
                full.bind(note(2, number), action),
                usize::from(number) < MAX_MIDI_BINDINGS
            );
        }
    }
}
