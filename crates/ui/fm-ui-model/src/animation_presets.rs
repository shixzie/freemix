//! The replicated project animation preset library (Protocol 2.34): the
//! user presets a snapshot's `animation_preset_state` records carry, kept
//! current by `animation_preset_changed`.

use core::num::NonZeroU32;

use fm_protocol::{
    AnimationPresetChange, AnimationPresetEntry, MAX_ANIMATION_PRESET_NAME_BYTES,
    MAX_ANIMATION_PRESETS,
};

/// The replicated animation preset library, in library order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnimationPresetsStatus {
    entries: Vec<AnimationPresetEntry>,
}

impl AnimationPresetsStatus {
    /// Every preset in library order.
    #[must_use]
    pub fn entries(&self) -> &[AnimationPresetEntry] {
        &self.entries
    }

    /// Whether the library holds no preset.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the library holds as many presets as a project may.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.entries.len() >= usize::from(MAX_ANIMATION_PRESETS)
    }

    /// The preset `preset`, if any.
    #[must_use]
    pub fn get(&self, preset: NonZeroU32) -> Option<&AnimationPresetEntry> {
        self.entries.iter().find(|entry| entry.preset == preset)
    }

    /// The preset named exactly `name`, if any.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&AnimationPresetEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// The smallest id above every preset's, or `None` when exhausted.
    #[must_use]
    pub fn next_id(&self) -> Option<NonZeroU32> {
        let highest = self
            .entries
            .iter()
            .map(|entry| entry.preset.get())
            .max()
            .unwrap_or(0);
        highest.checked_add(1).and_then(NonZeroU32::new)
    }

    /// Appends one snapshot `animation_preset_state` entry, refusing a
    /// duplicate id or name, an invalid name, or a full library.
    pub(crate) fn push_entry(&mut self, entry: AnimationPresetEntry) -> bool {
        if self.is_full()
            || !valid_name(&entry.name)
            || self.get(entry.preset).is_some()
            || self.by_name(&entry.name).is_some()
        {
            return false;
        }
        self.entries.push(entry);
        true
    }

    /// Whether `change` applies to this library: an added preset is new, at
    /// an index within the library, and uniquely named; a renamed preset
    /// keeps its index and motion and takes a name no other preset has; a
    /// removed preset exists.
    #[must_use]
    pub fn accepts(&self, change: &AnimationPresetChange) -> bool {
        match change {
            AnimationPresetChange::Set { index, entry } => {
                let index = usize::from(*index);
                if !valid_name(&entry.name)
                    || self
                        .entries
                        .iter()
                        .any(|other| other.preset != entry.preset && other.name == entry.name)
                {
                    return false;
                }
                match self
                    .entries
                    .iter()
                    .position(|other| other.preset == entry.preset)
                {
                    Some(current) => {
                        let existing = &self.entries[current];
                        current == index
                            && existing.phase == entry.phase
                            && existing.motion == entry.motion
                    }
                    None => !self.is_full() && index <= self.entries.len(),
                }
            }
            AnimationPresetChange::Removed { preset } => self.get(*preset).is_some(),
        }
    }

    /// Applies an accepted `change`.
    pub(crate) fn apply(&mut self, change: AnimationPresetChange) {
        match change {
            AnimationPresetChange::Set { index, entry } => {
                let index = usize::from(index);
                match self
                    .entries
                    .iter()
                    .position(|other| other.preset == entry.preset)
                {
                    Some(current) => self.entries[current] = entry,
                    None => self.entries.insert(index, entry),
                }
            }
            AnimationPresetChange::Removed { preset } => {
                self.entries.retain(|entry| entry.preset != preset);
            }
        }
    }
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_ANIMATION_PRESET_NAME_BYTES
        && !name.chars().any(char::is_control)
}
