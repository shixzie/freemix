//! Mixer strip labels and VCA-style strip groups (AU-001, schema 39).
//!
//! - Every input's Master strip carries an optional operator **label**
//!   (empty means "show the input name") and an optional **group**.
//! - A project keeps exactly [`AUDIO_GROUP_COUNT`] groups. A group has a
//!   name, a fader, and a mute but no audio path of its own: it scales and
//!   mutes the strips assigned to it. A member's effective gain is its own
//!   fader plus the group fader in dB, and a member is audible only while
//!   neither the strip nor its group is muted.
//! - Labels and group names are at most [`MAX_AUDIO_LABEL_BYTES`] bytes and
//!   contain no control characters; a group name is never blank.

use core::fmt;

use crate::InputGainMilliDb;

/// Number of strip groups in every project.
pub const AUDIO_GROUP_COUNT: usize = 4;
/// Longest strip label or group name, in UTF-8 bytes.
pub const MAX_AUDIO_LABEL_BYTES: usize = 32;

/// One strip group, numbered `1..=AUDIO_GROUP_COUNT`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AudioGroupNumber(u8);

impl AudioGroupNumber {
    /// Every group in ascending order.
    pub const ALL: [Self; AUDIO_GROUP_COUNT] = [Self(1), Self(2), Self(3), Self(4)];

    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number as usize <= AUDIO_GROUP_COUNT {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize - 1
    }
}

impl fmt::Display for AudioGroupNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "group {}", self.0)
    }
}

/// A strip group's name, fader, and mute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioGroup {
    pub name: String,
    pub gain: InputGainMilliDb,
    pub muted: bool,
}

impl AudioGroup {
    /// The default group `number`: named "Group N", at unity, unmuted.
    #[must_use]
    pub fn default_for(number: AudioGroupNumber) -> Self {
        Self {
            name: format!("Group {}", number.get()),
            gain: InputGainMilliDb::UNITY,
            muted: false,
        }
    }
}

/// The project's [`AUDIO_GROUP_COUNT`] strip groups, group 1 first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioGroups([AudioGroup; AUDIO_GROUP_COUNT]);

impl Default for AudioGroups {
    fn default() -> Self {
        Self(AudioGroupNumber::ALL.map(AudioGroup::default_for))
    }
}

impl AudioGroups {
    /// Groups from explicit values, group 1 first; validated by
    /// [`crate::Project::validate`].
    #[must_use]
    pub const fn new(groups: [AudioGroup; AUDIO_GROUP_COUNT]) -> Self {
        Self(groups)
    }

    #[must_use]
    pub const fn get(&self, group: AudioGroupNumber) -> &AudioGroup {
        &self.0[group.index()]
    }

    pub fn set(&mut self, group: AudioGroupNumber, value: AudioGroup) {
        self.0[group.index()] = value;
    }

    /// Every group with its number, group 1 first.
    pub fn iter(&self) -> impl Iterator<Item = (AudioGroupNumber, &AudioGroup)> {
        AudioGroupNumber::ALL.into_iter().zip(self.0.iter())
    }
}

/// Why a strip label or group name is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioLabelError {
    TooLong,
    ControlCharacter,
    /// A group name is blank.
    Empty,
}

impl fmt::Display for AudioLabelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => write!(
                formatter,
                "audio label must be at most {MAX_AUDIO_LABEL_BYTES} bytes"
            ),
            Self::ControlCharacter => {
                formatter.write_str("audio label must not contain control characters")
            }
            Self::Empty => formatter.write_str("audio group name must not be blank"),
        }
    }
}

impl std::error::Error for AudioLabelError {}

/// Checks a strip label; an empty label (show the input name) is valid.
///
/// # Errors
///
/// Returns [`AudioLabelError::TooLong`] or
/// [`AudioLabelError::ControlCharacter`].
pub fn validate_audio_strip_label(label: &str) -> Result<(), AudioLabelError> {
    if label.len() > MAX_AUDIO_LABEL_BYTES {
        return Err(AudioLabelError::TooLong);
    }
    if label.chars().any(char::is_control) {
        return Err(AudioLabelError::ControlCharacter);
    }
    Ok(())
}

/// Checks a group name, which must not be blank.
///
/// # Errors
///
/// Returns [`AudioLabelError::Empty`] for a blank name, otherwise as
/// [`validate_audio_strip_label`].
pub fn validate_audio_group_name(name: &str) -> Result<(), AudioLabelError> {
    if name.trim().is_empty() {
        return Err(AudioLabelError::Empty);
    }
    validate_audio_strip_label(name)
}
