//! HID controller bindings (RC-005): X-keys and similar HID button panels
//! drive the same console actions the key profiles and MIDI bindings do, on
//! this console only.
//!
//! A trigger is one device's Linux input key code; the daemon-side reader
//! ([`crate::hid`]) hands the console a press or release for each one. The
//! bindings are console preferences, saved with the key profiles:
//!
//! ```text
//! keymap.hid.9:X-keys-18.59 = replay_play_pause
//! ```
//!
//! where `<9>` is the device name's UTF-8 byte length, `X-keys-18` is the
//! device name from `/sys/class/input/*/device/name`, and `59` is the input
//! key code (Linux's `KEY_F1`). The value is a [`ConsoleAction::token`].

use std::collections::BTreeMap;

use crate::console_keymap::ConsoleAction;
use crate::l10n::{Arg, Locale, Msg};

/// Most HID bindings one console keeps.
pub const MAX_HID_BINDINGS: usize = 64;
/// Longest device name, in UTF-8 bytes.
pub const MAX_HID_DEVICE_BYTES: usize = 128;
const PREFIX: &str = "keymap.hid.";

/// One device's button: its name and Linux input key code.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HidTrigger {
    pub device: String,
    pub code: u16,
}

impl HidTrigger {
    /// A trigger, or `None` for a blank or overlong device name.
    #[must_use]
    pub fn new(device: impl Into<String>, code: u16) -> Option<Self> {
        let device = device.into();
        if device.trim().is_empty() || device.len() > MAX_HID_DEVICE_BYTES {
            None
        } else {
            Some(Self { device, code })
        }
    }

    /// The trigger in the preferences file: `<len>:<device>.<code>`.
    #[must_use]
    pub fn token(&self) -> String {
        format!("{}:{}.{}", self.device.len(), self.device, self.code)
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        let (length, rest) = token.split_once(':')?;
        let length: usize = length.parse().ok()?;
        if length == 0 || length > MAX_HID_DEVICE_BYTES || rest.len() < length + 1 {
            return None;
        }
        let (device, rest) = rest.split_at(length);
        let code = rest.strip_prefix('.')?.parse().ok()?;
        Self::new(device, code)
    }

    /// The trigger as the console shows it: `X-keys-18 · 59`.
    #[must_use]
    pub fn text(&self, l: Locale) -> String {
        l.format(
            Msg::HidTriggerText,
            &[
                ("device", Arg::Str(&self.device)),
                ("code", Arg::Count(u64::from(self.code))),
            ],
        )
    }
}

/// One HID press or release the reader reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HidInput {
    pub trigger: HidTrigger,
    pub pressed: bool,
}

/// The console's HID bindings; see the module documentation.
#[derive(Clone, Debug, Default)]
pub struct HidBindings {
    bindings: BTreeMap<HidTrigger, ConsoleAction>,
    /// Triggers held since their press (runtime only).
    held: std::collections::BTreeSet<HidTrigger>,
}

impl PartialEq for HidBindings {
    fn eq(&self, other: &Self) -> bool {
        self.bindings == other.bindings
    }
}

impl Eq for HidBindings {}

impl HidBindings {
    /// Every binding, by trigger.
    pub fn bindings(&self) -> impl Iterator<Item = (&HidTrigger, ConsoleAction)> + '_ {
        self.bindings.iter().map(|(trigger, action)| (trigger, *action))
    }

    /// The trigger bound to `action`, if any.
    #[must_use]
    pub fn trigger_of(&self, action: ConsoleAction) -> Option<&HidTrigger> {
        self.bindings
            .iter()
            .find_map(|(trigger, bound)| (*bound == action).then_some(trigger))
    }

    /// The action `trigger` drives, if any.
    #[must_use]
    pub fn action(&self, trigger: &HidTrigger) -> Option<ConsoleAction> {
        self.bindings.get(trigger).copied()
    }

    /// Binds `trigger` to `action`, dropping `action`'s previous trigger;
    /// `false` (and nothing changes) when the console already keeps
    /// [`MAX_HID_BINDINGS`] other bindings.
    pub fn bind(&mut self, trigger: HidTrigger, action: ConsoleAction) -> bool {
        let previous = self.trigger_of(action).cloned();
        let grows = previous.is_none() && !self.bindings.contains_key(&trigger);
        if grows && self.bindings.len() >= MAX_HID_BINDINGS {
            return false;
        }
        if let Some(previous) = previous {
            self.bindings.remove(&previous);
        }
        self.bindings.insert(trigger, action);
        true
    }

    /// Unbinds `action`; `false` when it had no binding.
    pub fn unbind(&mut self, action: ConsoleAction) -> bool {
        let Some(trigger) = self.trigger_of(action).cloned() else {
            return false;
        };
        self.bindings.remove(&trigger);
        true
    }

    /// The triggers pressed since the last call, in order.
    pub fn presses(&mut self, inputs: &[HidInput]) -> Vec<HidTrigger> {
        let mut presses = Vec::new();
        for input in inputs {
            if !input.pressed {
                self.held.remove(&input.trigger);
            } else if self.held.insert(input.trigger.clone()) {
                presses.push(input.trigger.clone());
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

    /// Applies one preference line; `false` for a key that is not an HID
    /// binding or an unusable one.
    pub fn apply_preference(&mut self, key: &str, value: &str) -> bool {
        let Some(trigger) = key.strip_prefix(PREFIX).and_then(HidTrigger::from_token) else {
            return false;
        };
        let Some(action) = ConsoleAction::from_token(value) else {
            return false;
        };
        self.bind(trigger, action)
    }

    /// Whether `key` names an HID binding line.
    #[must_use]
    pub fn is_preference_key(key: &str) -> bool {
        key.starts_with(PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(device: &str, code: u16) -> HidTrigger {
        HidTrigger::new(device, code).unwrap()
    }

    #[test]
    fn triggers_are_bounded_and_round_trip_their_tokens() {
        assert_eq!(HidTrigger::new("", 59), None);
        assert_eq!(HidTrigger::new("  ", 59), None);
        assert_eq!(
            HidTrigger::new("d".repeat(MAX_HID_DEVICE_BYTES + 1), 59),
            None
        );
        let value = trigger("X-keys-18", 59);
        assert_eq!(value.token(), "9:X-keys-18.59");
        assert_eq!(HidTrigger::from_token(&value.token()), Some(value));
        assert_eq!(
            HidTrigger::from_token("10:X-keys-18.59"),
            None,
            "the length must match"
        );
        assert_eq!(HidTrigger::from_token("9:X-keys-18"), None);
        assert_eq!(HidTrigger::from_token("0:.59"), None);
    }

    #[test]
    fn bindings_round_trip_preferences_and_are_exclusive_per_action() {
        let mut bindings = HidBindings::default();
        assert!(bindings.bind(trigger("X-keys-18", 59), ConsoleAction::ReplayPlayPause));
        assert!(bindings.bind(trigger("X-keys-18", 60), ConsoleAction::Cut));
        let entries = bindings.preference_entries();
        assert_eq!(
            entries,
            [
                (
                    "keymap.hid.9:X-keys-18.59".to_owned(),
                    "replay_play_pause".to_owned()
                ),
                ("keymap.hid.9:X-keys-18.60".to_owned(), "cut".to_owned()),
            ]
        );
        let mut read = HidBindings::default();
        for (key, value) in &entries {
            assert!(read.apply_preference(key, value), "{key}");
        }
        assert_eq!(read, bindings);
        // Rebinding an action drops its old trigger.
        assert!(bindings.bind(trigger("X-keys-18", 61), ConsoleAction::Cut));
        assert_eq!(
            bindings.trigger_of(ConsoleAction::Cut),
            Some(&trigger("X-keys-18", 61))
        );
        assert!(bindings.unbind(ConsoleAction::Cut));
        assert!(!bindings.unbind(ConsoleAction::Cut));
        assert!(!read.apply_preference("keymap.hid.bad", "cut"));
        assert!(!read.apply_preference("keymap.midi.note.1.60", "cut"));
    }

    #[test]
    fn presses_fire_once_until_released() {
        let value = trigger("X-keys-18", 59);
        let mut bindings = HidBindings::default();
        let press = HidInput {
            trigger: value.clone(),
            pressed: true,
        };
        assert_eq!(bindings.presses(&[press.clone()]), [value.clone()]);
        assert_eq!(bindings.presses(&[press.clone()]), []);
        assert_eq!(
            bindings.presses(&[HidInput {
                trigger: value.clone(),
                pressed: false
            }]),
            []
        );
        assert_eq!(bindings.presses(&[press]), [value]);
    }
}
