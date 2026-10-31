//! Camera presets exposed as selectable virtual inputs (IN-028).
//!
//! A [`PresetVirtualInput`] names one camera preset an operator can switch to
//! like any other input, with an optional thumbnail and an enabled flag. The
//! registry is a bounded, deterministic map keyed by the existing
//! [`PresetVirtualInputId`]; switching validates the entry and returns the
//! camera and recall intent a caller applies to that camera's adapter.

use core::fmt;
use std::collections::BTreeMap;

/// Largest canonical preset virtual-input document, in bytes.
pub const MAX_PRESET_VIRTUAL_INPUT_DOCUMENT_BYTES: usize = 256 * 1024;

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
                write!(
                    formatter,
                    "preset virtual input {id} has an invalid thumbnail"
                )
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
    pub fn insert(&mut self, input: PresetVirtualInput) -> Result<(), PresetVirtualInputError> {
        if !self.entries.contains_key(&input.id) && self.entries.len() >= MAX_PRESET_VIRTUAL_INPUTS
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

/// Why a preset virtual-input document is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresetVirtualInputDocumentError {
    TooLarge { maximum: usize },
    Version { found: String },
    Line { index: usize },
    Invalid(PresetVirtualInputError),
}

impl fmt::Display for PresetVirtualInputDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { maximum } => {
                write!(
                    formatter,
                    "preset virtual-input document exceeds {maximum} bytes"
                )
            }
            Self::Version { found } => {
                write!(
                    formatter,
                    "preset virtual-input document version {found:?} is not 1"
                )
            }
            Self::Line { index } => {
                write!(formatter, "preset virtual-input line {index} is malformed")
            }
            Self::Invalid(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PresetVirtualInputDocumentError {}

impl PresetVirtualInputs {
    /// Serializes the registry as a strict, versioned text document.
    #[must_use]
    pub fn to_document(&self) -> String {
        use core::fmt::Write as _;
        let mut output = String::from(
            "freemix-ptz-presets 1
",
        );
        let _ = writeln!(output, "count {}", self.entries.len());
        for input in self.entries.values() {
            let _ = writeln!(
                output,
                "preset {} {} {} {} {}",
                encode_text(input.id.camera_id().as_str()),
                input.id.preset_id().get(),
                u8::from(input.enabled),
                encode_text(&input.name),
                input
                    .thumbnail
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |thumbnail| encode_text(thumbnail)),
            );
        }
        output
    }

    /// Parses a strict preset virtual-input document.
    ///
    /// # Errors
    ///
    /// Returns [`PresetVirtualInputDocumentError`] for an oversized,
    /// malformed, or out-of-contract document without accepting a partial
    /// result.
    pub fn from_document(document: &str) -> Result<Self, PresetVirtualInputDocumentError> {
        if document.len() > MAX_PRESET_VIRTUAL_INPUT_DOCUMENT_BYTES {
            return Err(PresetVirtualInputDocumentError::TooLarge {
                maximum: MAX_PRESET_VIRTUAL_INPUT_DOCUMENT_BYTES,
            });
        }
        let mut lines = document.lines().enumerate();
        let Some((_, header)) = lines.next() else {
            return Err(PresetVirtualInputDocumentError::Line { index: 1 });
        };
        if header != "freemix-ptz-presets 1" {
            return Err(PresetVirtualInputDocumentError::Version {
                found: header.to_owned(),
            });
        }
        let Some((_, count_line)) = lines.next() else {
            return Err(PresetVirtualInputDocumentError::Line { index: 2 });
        };
        let Some(count) = count_line.strip_prefix("count ") else {
            return Err(PresetVirtualInputDocumentError::Line { index: 2 });
        };
        let count: usize = count
            .parse()
            .map_err(|_| PresetVirtualInputDocumentError::Line { index: 2 })?;
        if count > MAX_PRESET_VIRTUAL_INPUTS {
            return Err(PresetVirtualInputDocumentError::Invalid(
                PresetVirtualInputError::TooMany,
            ));
        }
        let mut inputs = Self::new();
        let mut index = 2;
        for _ in 0..count {
            let Some((line_index, line)) = lines.next() else {
                return Err(PresetVirtualInputDocumentError::Line { index: index + 1 });
            };
            index = line_index + 2;
            let bad = || PresetVirtualInputDocumentError::Line { index };
            let parts: Vec<&str> = line.split(' ').collect();
            let [prefix, camera, preset, enabled, name, thumbnail] = parts[..] else {
                return Err(bad());
            };
            if prefix != "preset" {
                return Err(bad());
            }
            let camera = CameraId::new(decode_text(camera).ok_or_else(bad)?).map_err(|_| bad())?;
            let preset: u16 = preset.parse().map_err(|_| bad())?;
            let enabled = match enabled {
                "0" => false,
                "1" => true,
                _ => return Err(bad()),
            };
            let name = decode_text(name).ok_or_else(bad)?;
            let thumbnail = match thumbnail {
                "none" => None,
                thumbnail => Some(decode_text(thumbnail).ok_or_else(bad)?),
            };
            let input = PresetVirtualInput {
                id: PresetVirtualInputId::new(camera, PresetId::new(preset)),
                name,
                thumbnail,
                enabled,
            };
            inputs
                .insert(input)
                .map_err(PresetVirtualInputDocumentError::Invalid)?;
        }
        if lines.next().is_some() {
            return Err(PresetVirtualInputDocumentError::Line { index: index + 1 });
        }
        Ok(inputs)
    }
}

fn encode_text(text: &str) -> String {
    use core::fmt::Write as _;
    let mut output = String::from("x");
    for byte in text.bytes() {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn decode_text(token: &str) -> Option<String> {
    let hex = token.strip_prefix('x')?;
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let pair = core::str::from_utf8(chunk).ok()?;
        bytes.push(u8::from_str_radix(pair, 16).ok()?);
    }
    String::from_utf8(bytes).ok()
}

/// Why switching a preset virtual input failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresetSwitchError {
    Input(PresetVirtualInputError),
    Adapter(crate::AdapterError),
}

impl fmt::Display for PresetSwitchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => error.fmt(formatter),
            Self::Adapter(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PresetSwitchError {}

impl From<PresetVirtualInputError> for PresetSwitchError {
    fn from(error: PresetVirtualInputError) -> Self {
        Self::Input(error)
    }
}

impl PresetVirtualInputs {
    /// Switches `id` on `adapter`: resolves the enabled entry to its camera
    /// and applies the preset recall.
    ///
    /// # Errors
    ///
    /// Returns a [`PresetSwitchError::Input`] for an unknown or disabled
    /// entry, or [`PresetSwitchError::Adapter`] when the camera refuses the
    /// recall.
    pub fn switch_camera(
        &self,
        id: &PresetVirtualInputId,
        adapter: &mut impl crate::PtzAdapter,
    ) -> Result<crate::CommandOutcome, PresetSwitchError> {
        let switch = self.switch(id)?;
        adapter
            .execute(&switch.camera, switch.intent())
            .map_err(PresetSwitchError::Adapter)
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
            inputs
                .list()
                .map(|input| input.name.as_str())
                .collect::<Vec<_>>(),
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

        assert!(
            inputs
                .insert(PresetVirtualInput::new(id("cam-a", 3)))
                .is_err()
        );
        assert!(
            inputs
                .insert(
                    PresetVirtualInput::new(id("cam-a", 3))
                        .with_name("x")
                        .with_thumbnail("bad\nthumbnail")
                )
                .is_err()
        );
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

    #[test]
    fn documents_round_trip_and_reject_non_canonical_or_malformed_input() {
        let mut inputs = PresetVirtualInputs::new();
        inputs
            .insert(entry("cam-a", 1, "Wide").with_thumbnail("asset://thumbs/wide.png"))
            .unwrap();
        inputs.insert(entry("cam-b", 2, "Close")).unwrap();
        let document = inputs.to_document();
        assert_eq!(
            PresetVirtualInputs::from_document(&document).unwrap(),
            inputs
        );
        for malformed in [
            document.replacen("freemix-ptz-presets 1", "freemix-ptz-presets 2", 1),
            document.replacen("count 2", "count 1", 1),
            document.replacen("count 2", "count 3", 1),
            document.replacen("preset x", "preset", 1),
            document.replacen("count 2", "count 2 ", 1),
            format!("{document}extra\n"),
        ] {
            assert!(
                PresetVirtualInputs::from_document(&malformed).is_err(),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn an_enabled_entry_switches_its_camera_preset() {
        use crate::{FakeViscaAdapter, FakeViscaCamera, PresetId, PtzAdapter, PtzIntent};
        let mut inputs = PresetVirtualInputs::new();
        inputs.insert(entry("cam-a", 3, "Wide")).unwrap();
        let mut adapter =
            FakeViscaAdapter::new([FakeViscaCamera::new(CameraId::new("cam-a").unwrap(), "Cam")]);
        adapter
            .execute(
                &CameraId::new("cam-a").unwrap(),
                PtzIntent::SavePreset {
                    id: PresetId::new(3),
                    name: "Wide".into(),
                },
            )
            .unwrap();
        let outcome = inputs.switch_camera(&id("cam-a", 3), &mut adapter).unwrap();
        assert_eq!(outcome.applied, PtzIntent::RecallPreset(PresetId::new(3)));
        inputs.set_enabled(&id("cam-a", 3), false).unwrap();
        assert!(matches!(
            inputs.switch_camera(&id("cam-a", 3), &mut adapter),
            Err(PresetSwitchError::Input(PresetVirtualInputError::Disabled(
                _
            )))
        ));
    }
}
