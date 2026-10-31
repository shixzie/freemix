//! Camera presets exposed as selectable virtual inputs (IN-028).
//!
//! A [`PresetVirtualInput`] names one camera preset an operator can switch to
//! like any other input, with an optional thumbnail and an enabled flag. The
//! registry is a bounded, deterministic map keyed by the existing
//! [`PresetVirtualInputId`]; switching validates the entry and returns the
//! camera and recall intent a caller applies to that camera's adapter.

use core::fmt;
use std::collections::BTreeMap;

use crate::{CameraId, PresetId, PresetVirtualInputId, PtzIntent};

/// Most preset virtual inputs one registry may hold.
pub const MAX_PRESET_VIRTUAL_INPUTS: usize = 256;
/// Longest virtual input name, in bytes.
pub const MAX_PRESET_VIRTUAL_INPUT_NAME_BYTES: usize = 64;
/// Longest thumbnail reference, in bytes.
pub const MAX_PRESET_THUMBNAIL_BYTES: usize = 1_024;

/// One camera preset exposed as a virtual input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresetVirtualInput {
    pub id: PresetVirtualInputId,
    pub name: String,
    /// Opaque thumbnail reference (an asset URI or a host path), if any.
    pub thumbnail: Option<String>,
    pub enabled: bool,
}

impl PresetVirtualInput {
    /// A disabled entry with no thumbnail.
    #[must_use]
    pub fn new(id: PresetVirtualInputId) -> Self {
        Self {
            id,
            name: String::new(),
            thumbnail: None,
            enabled: false,
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    #[must_use]
    pub fn with_thumbnail(mut self, thumbnail: impl Into<String>) -> Self {
        self.thumbnail = Some(thumbnail.into());
        self
    }

    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn validate(&self) -> Result<(), PresetVirtualInputError> {
        if self.name.is_empty() || self.name.len() > MAX_PRESET_VIRTUAL_INPUT_NAME_BYTES {
            return Err(PresetVirtualInputError::InvalidName(self.id.clone()));
        }
        if let Some(thumbnail) = &self.thumbnail
            && (thumbnail.is_empty()
                || thumbnail.len() > MAX_PRESET_THUMBNAIL_BYTES
                || thumbnail.chars().any(char::is_control))
        {
            return Err(PresetVirtualInputError::InvalidThumbnail(self.id.clone()));
        }
        Ok(())
    }
}

/// Why a preset virtual input could not be added or switched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresetVirtualInputError {
    TooMany,
    Duplicate(PresetVirtualInputId),
    Unknown(PresetVirtualInputId),
    InvalidName(PresetVirtualInputId),
    InvalidThumbnail(PresetVirtualInputId),
    Disabled(PresetVirtualInputId),
}

impl fmt::Display for PresetVirtualInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooMany => write!(
                formatter,
                "at most {MAX_PRESET_VIRTUAL_INPUTS} preset virtual inputs are supported"
            ),
            Self::Duplicate(id) => write!(formatter, "preset virtual input {id} already exists"),
            Self::Unknown(id) => write!(formatter, "unknown preset virtual input {id}"),
            Self::InvalidName(id) => write!(formatter, "preset virtual input {id} has no name"),
            Self::InvalidThumbnail(id) => {
                write!(formatter, "preset virtual input {id} has an invalid thumbnail")
            }
            Self::Disabled(id) => write!(formatter, "preset virtual input {id} is disabled"),
        }
    }
}

impl std::error::Error for PresetVirtualInputError {}

/// The camera and preset a switch recalls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresetSwitch {
    pub camera: CameraId,
    pub preset: PresetId,
}

impl PresetSwitch {
    /// The intent a camera adapter applies.
    #[must_use]
    pub const fn intent(&self) -> PtzIntent {
        PtzIntent::RecallPreset(self.preset)
    }
}

/// A bounded registry of preset virtual inputs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PresetVirtualInputs {
    entries: BTreeMap<PresetVirtualInputId, PresetVirtualInput>,
}

impl PresetVirtualInputs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: &PresetVirtualInputId) -> Option<&PresetVirtualInput> {
        self.entries.get(id)
    }

    /// Entries in ascending id order.
    pub fn list(&self) -> impl Iterator<Item = &PresetVirtualInput> {
        self.entries.values()
    }

    /// Adds or replaces one entry after validating it.
    ///
    /// # Errors
    ///
    /// Returns [`PresetVirtualInputError::TooMany`] past the bound (an
    /// existing id may still be replaced) or an invalid name/thumbnail.
    pub fn insert(
        &mut self,
        input: PresetVirtualInput,
    ) -> Result<(), PresetVirtualInputError> {
        if !self.entries.contains_key(&input.id)
            && self.entries.len() >= MAX_PRESET_VIRTUAL_INPUTS
        {
            return Err(PresetVirtualInputError::TooMany);
        }
        input.validate()?;
        self.entries.insert(input.id.clone(), input);
        Ok(())
    }

    /// Removes one entry, returning it.
    pub fn remove(&mut self, id: &PresetVirtualInputId) -> Option<PresetVirtualInput> {
        self.entries.remove(id)
    }

    /// Enables or disables one entry.
    ///
    /// # Errors
    ///
    /// Returns [`PresetVirtualInputError::Unknown`] for an id that is not
    /// registered.
    pub fn set_enabled(
        &mut self,
        id: &PresetVirtualInputId,
        enabled: bool,
    ) -> Result<(), PresetVirtualInputError> {
        let input = self
            .entries
            .get_mut(id)
            .ok_or_else(|| PresetVirtualInputError::Unknown(id.clone()))?;
        input.enabled = enabled;
        Ok(())
    }

    /// Resolves a switch to `id`.
    ///
    /// # Errors
    ///
    /// Returns [`PresetVirtualInputError::Unknown`] or
    /// [`PresetVirtualInputError::Disabled`].
    pub fn switch(
        &self,
        id: &PresetVirtualInputId,
    ) -> Result<PresetSwitch, PresetVirtualInputError> {
        let input = self
            .entries
            .get(id)
            .ok_or_else(|| PresetVirtualInputError::Unknown(id.clone()))?;
        if !input.enabled {
            return Err(PresetVirtualInputError::Disabled(id.clone()));
        }
        Ok(PresetSwitch {
            camera: id.camera_id().clone(),
            preset: id.preset_id(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(camera: &str, preset: u16) -> PresetVirtualInputId {
        PresetVirtualInputId::new(CameraId::new(camera).unwrap(), PresetId::new(preset))
    }

    fn entry(camera: &str, preset: u16, name: &str) -> PresetVirtualInput {
        PresetVirtualInput::new(id(camera, preset))
            .with_name(name)
            .enabled(true)
    }

    #[test]
    fn entries_are_bounded_named_and_switchable() {
        let mut inputs = PresetVirtualInputs::new();
        inputs.insert(entry("cam-a", 1, "Wide")).unwrap();
        inputs
            .insert(entry("cam-b", 2, "Close").with_thumbnail("asset://thumbs/close.png"))
            .unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(
            inputs.list().map(|input| input.name.as_str()).collect::<Vec<_>>(),
            vec!["Wide", "Close"]
        );
        let switch = inputs.switch(&id("cam-b", 2)).unwrap();
        assert_eq!(switch.camera.as_str(), "cam-b");
        assert_eq!(switch.preset, PresetId::new(2));
        assert_eq!(switch.intent(), PtzIntent::RecallPreset(PresetId::new(2)));

        inputs.set_enabled(&id("cam-a", 1), false).unwrap();
        assert_eq!(
            inputs.switch(&id("cam-a", 1)),
            Err(PresetVirtualInputError::Disabled(id("cam-a", 1)))
        );
        assert_eq!(
            inputs.switch(&id("cam-z", 9)),
            Err(PresetVirtualInputError::Unknown(id("cam-z", 9)))
        );

        assert!(inputs
            .insert(PresetVirtualInput::new(id("cam-a", 3)))
            .is_err());
        assert!(inputs
            .insert(
                PresetVirtualInput::new(id("cam-a", 3))
                    .with_name("x")
                    .with_thumbnail("bad\nthumbnail")
            )
            .is_err());
        assert_eq!(inputs.remove(&id("cam-a", 1)).unwrap().name, "Wide");
        assert_eq!(inputs.len(), 1);
    }

    #[test]
    fn the_registry_stops_at_its_bound() {
        let mut inputs = PresetVirtualInputs::new();
        for preset in 0..u16::try_from(MAX_PRESET_VIRTUAL_INPUTS).unwrap() {
            inputs
                .insert(entry("cam", preset, &format!("Preset {preset}")))
                .unwrap();
        }
        assert_eq!(
            inputs.insert(entry("cam", 999, "Overflow")),
            Err(PresetVirtualInputError::TooMany)
        );
    }
}
