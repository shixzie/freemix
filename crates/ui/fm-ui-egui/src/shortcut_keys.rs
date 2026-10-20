//! This console's own keys for project shortcuts (RC-004).
//!
//! A project shortcut carries the key the show gives it, shared by every
//! console. An operator may give a shortcut another key on this console,
//! or no key here at all, without touching the project: the override is a
//! console preference, kept per project and shortcut id, saved with the
//! other preferences and never sent to the daemon.
//!
//! On this console a shortcut answers its override when it has one, else
//! its project key. An override wins over another shortcut's project key
//! (that shortcut then has no key here until the clash is gone); a key the
//! console keeps for itself never fires a shortcut.
//!
//! In the preferences file each override is one line,
//! `shortcut.key.<project id>.<shortcut id> = <key>`, where the key is
//! written as the console shows it (`Ctrl+Shift+Alt+F9`, modifiers in that
//! order) or `none` for no key on this console.

use std::collections::BTreeMap;

use fm_protocol::WireShortcutKey;
use fm_types::ProjectId;

/// Most overrides one console keeps, over every project.
pub const MAX_KEY_OVERRIDES: usize = 1024;

/// The preference key prefix of an override.
const PREFIX: &str = "shortcut.key.";
/// The preference value of "no key on this console".
const NONE: &str = "none";

/// The console's key overrides, by project and shortcut id.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShortcutKeyOverrides {
    entries: BTreeMap<(u128, u64), Option<WireShortcutKey>>,
}

impl ShortcutKeyOverrides {
    /// The override of `shortcut` in `project`: `None` when it has none,
    /// `Some(&None)` for no key on this console.
    #[must_use]
    pub fn get(&self, project: ProjectId, shortcut: u64) -> Option<&Option<WireShortcutKey>> {
        self.entries.get(&(project.get().get(), shortcut))
    }

    /// Gives `shortcut` in `project` the key `key` (or no key) on this
    /// console. Returns `false`, changing nothing, when the key is not a
    /// valid single, unscoped shortcut key or the console already keeps
    /// [`MAX_KEY_OVERRIDES`] others.
    pub fn set(&mut self, project: ProjectId, shortcut: u64, key: Option<WireShortcutKey>) -> bool {
        if key.as_ref().is_some_and(|key| {
            !fm_protocol::shortcut_key_is_valid(key) || key.then.is_some() || key.scope.is_some()
        }) || shortcut == 0
        {
            return false;
        }
        let entry = (project.get().get(), shortcut);
        if !self.entries.contains_key(&entry) && self.entries.len() >= MAX_KEY_OVERRIDES {
            return false;
        }
        self.entries.insert(entry, key);
        true
    }

    /// Returns `shortcut` in `project` to its project key on this console.
    pub fn clear(&mut self, project: ProjectId, shortcut: u64) {
        self.entries.remove(&(project.get().get(), shortcut));
    }

    /// How many overrides the console keeps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the console keeps no override.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The preference lines that reproduce these overrides, in order.
    #[must_use]
    pub fn preference_entries(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .map(|((project, shortcut), key)| {
                (
                    format!("{PREFIX}{project}.{shortcut}"),
                    key.as_ref()
                        .map_or_else(|| NONE.to_owned(), crate::user_shortcuts::key_text),
                )
            })
            .collect()
    }

    /// Takes in one preference line; `false` when `key` is not an override
    /// line or `value` does not parse (the line is then ignored).
    pub fn apply_preference(&mut self, key: &str, value: &str) -> bool {
        let Some((project, shortcut)) = key
            .strip_prefix(PREFIX)
            .and_then(|rest| rest.split_once('.'))
        else {
            return false;
        };
        let (Some(project), Some(shortcut)) = (
            canonical::<u128>(project)
                .and_then(core::num::NonZeroU128::new)
                .map(ProjectId::new),
            canonical::<u64>(shortcut),
        ) else {
            return false;
        };
        let key = if value == NONE {
            None
        } else {
            match parse_key_text(value) {
                Some(key) => Some(key),
                None => return false,
            }
        };
        self.set(project, shortcut, key)
    }
}

/// A decimal number without sign or leading zeros.
fn canonical<T: core::str::FromStr>(text: &str) -> Option<T> {
    (!text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0')))
    .then(|| text.parse().ok())
    .flatten()
}

/// Parses a key written by [`crate::user_shortcuts::key_text`]
/// (`Ctrl+Shift+Alt+F9`, each modifier at most once, in that order).
#[must_use]
pub fn parse_key_text(text: &str) -> Option<WireShortcutKey> {
    let mut rest = text;
    let mut take = |name: &str| {
        let after = rest
            .strip_prefix(name)
            .and_then(|after| after.strip_prefix('+'));
        if let Some(after) = after {
            rest = after;
        }
        after.is_some()
    };
    let command = take("Ctrl");
    let shift = take("Shift");
    let alt = take("Alt");
    let key = WireShortcutKey {
        key: rest.to_owned(),
        command,
        shift,
        alt,
        then: None,
        scope: None,
    };
    fm_protocol::shortcut_key_is_valid(&key).then_some(key)
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    fn project(value: u128) -> ProjectId {
        ProjectId::new(NonZeroU128::new(value).unwrap())
    }

    fn key(name: &str, command: bool, shift: bool, alt: bool) -> WireShortcutKey {
        WireShortcutKey {
            key: name.into(),
            command,
            shift,
            alt,
            then: None,
            scope: None,
        }
    }

    #[test]
    fn overrides_are_kept_per_project_and_shortcut() {
        let mut overrides = ShortcutKeyOverrides::default();
        assert!(overrides.set(project(7), 1, Some(key("F2", true, false, false))));
        assert!(overrides.set(project(7), 2, None));
        assert_eq!(
            overrides.get(project(7), 1),
            Some(&Some(key("F2", true, false, false)))
        );
        assert_eq!(overrides.get(project(7), 2), Some(&None));
        assert_eq!(overrides.get(project(8), 1), None);
        overrides.clear(project(7), 2);
        assert_eq!(overrides.get(project(7), 2), None);
        // Not a key a shortcut may hold, or shortcut id zero.
        assert!(!overrides.set(project(7), 3, Some(key("F+", false, false, false))));
        assert!(!overrides.set(project(7), 0, None));
        // A console key is a single key that answers anywhere.
        let mut chord = key("K", true, false, false);
        chord.then = Some(Box::new(key("1", false, false, false)));
        assert!(!overrides.set(project(7), 3, Some(chord)));
        let mut scoped = key("K", true, false, false);
        scoped.scope = Some("cues".into());
        assert!(!overrides.set(project(7), 3, Some(scoped)));
        assert_eq!(overrides.len(), 1);
    }

    #[test]
    fn overrides_round_trip_through_preference_lines() {
        let mut overrides = ShortcutKeyOverrides::default();
        overrides.set(project(42), 3, Some(key("F9", true, true, true)));
        overrides.set(project(42), 4, Some(key("J", false, false, true)));
        overrides.set(project(u128::MAX), 9, None);
        let lines = overrides.preference_entries();
        assert_eq!(
            lines,
            vec![
                (
                    "shortcut.key.42.3".to_owned(),
                    "Ctrl+Shift+Alt+F9".to_owned()
                ),
                ("shortcut.key.42.4".to_owned(), "Alt+J".to_owned()),
                (format!("shortcut.key.{}.9", u128::MAX), "none".to_owned()),
            ]
        );
        let mut restored = ShortcutKeyOverrides::default();
        for (key, value) in &lines {
            assert!(restored.apply_preference(key, value), "{key}");
        }
        assert_eq!(restored, overrides);
        // Malformed lines are ignored.
        let mut ignored = ShortcutKeyOverrides::default();
        for (key, value) in [
            ("shortcut.key.0.3", "F9"),
            ("shortcut.key.42.03", "F9"),
            ("shortcut.key.42", "F9"),
            ("shortcut.key.x.3", "F9"),
            ("shortcut.key.42.3", "Shift+Ctrl+F9"),
            ("shortcut.key.42.3", "Ctrl+"),
            ("shortcut.key.42.3", ""),
            ("layout.panel", "F9"),
        ] {
            assert!(!ignored.apply_preference(key, value), "{key} = {value}");
        }
        assert!(ignored.is_empty());
    }

    #[test]
    fn the_console_keeps_a_bounded_number_of_overrides() {
        let mut overrides = ShortcutKeyOverrides::default();
        for shortcut in 1..=MAX_KEY_OVERRIDES as u64 {
            assert!(overrides.set(project(1), shortcut, None));
        }
        assert!(!overrides.set(project(2), 1, None));
        // Changing one it keeps is still allowed.
        assert!(overrides.set(project(1), 1, Some(key("F1", false, false, false))));
    }
}
