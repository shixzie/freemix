//! Persisted audio plugin chains (AU-006, schema 91).
//!
//! Every input strip and the Master bus may carry an ordered chain of at
//! most [`MAX_PLUGIN_CHAIN_SLOTS`] plugins. A slot names one plugin by its
//! stable identifier, says whether it is bypassed, reports the latency it
//! adds in frames, and keeps the plugin's last opaque state snapshot. The
//! identifiers in a chain are unique, so a slot can be found by plugin.
//!
//! This is the durable form. The engine still renders through
//! `fm-plugin-host-api`'s [`PluginChain`] type; the daemon converts between
//! the two without loss (state snapshots carry version zero, the wire's
//! version) and refuses a chain this module would not accept.

use core::fmt;
use std::collections::BTreeMap;

use crate::base64url;

/// Most plugins one chain holds.
pub const MAX_PLUGIN_CHAIN_SLOTS: usize = 8;
/// Longest plugin identifier, in bytes.
pub const MAX_PLUGIN_ID_BYTES: usize = 128;
/// Largest opaque plugin state one slot carries, in bytes.
pub const MAX_PLUGIN_STATE_BYTES: usize = 64 * 1024;
/// Most frames of latency one slot may report.
pub const MAX_PLUGIN_LATENCY_FRAMES: u32 = 60_000;
/// Most named parameters one slot may hold.
pub const MAX_PLUGIN_PARAMETERS: usize = 64;
/// Longest plugin parameter name, in bytes.
pub const MAX_PLUGIN_PARAMETER_NAME_BYTES: usize = 64;

/// One plugin's place in a persisted chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSlotState {
    plugin: String,
    bypassed: bool,
    latency_frames: u32,
    state: Vec<u8>,
    /// Parameter values, stored as `f32` bit patterns so a slot stays `Eq`.
    parameters: BTreeMap<String, u32>,
}

impl PluginSlotState {
    /// An active slot for `plugin` with no latency and no saved state.
    #[must_use]
    pub fn new(plugin: impl Into<String>) -> Self {
        Self {
            plugin: plugin.into(),
            bypassed: false,
            latency_frames: 0,
            state: Vec::new(),
            parameters: BTreeMap::new(),
        }
    }

    /// The plugin this slot hosts.
    #[must_use]
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// Whether the slot is skipped.
    #[must_use]
    pub const fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// Sets whether the slot is skipped.
    pub const fn set_bypassed(&mut self, bypassed: bool) {
        self.bypassed = bypassed;
    }

    /// The latency the plugin adds while active, in frames.
    #[must_use]
    pub const fn latency_frames(&self) -> u32 {
        self.latency_frames
    }

    /// Sets the latency the plugin adds while active.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainStateError`] above
    /// [`MAX_PLUGIN_LATENCY_FRAMES`].
    pub fn set_latency(&mut self, frames: u32) -> Result<(), PluginChainStateError> {
        if frames > MAX_PLUGIN_LATENCY_FRAMES {
            return Err(PluginChainStateError {
                field: "latency_frames",
            });
        }
        self.latency_frames = frames;
        Ok(())
    }

    /// The plugin's opaque saved state, empty when it has none.
    #[must_use]
    pub fn state(&self) -> &[u8] {
        &self.state
    }

    /// Replaces the plugin's opaque saved state.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainStateError`] above [`MAX_PLUGIN_STATE_BYTES`].
    pub fn set_state(&mut self, state: impl Into<Vec<u8>>) -> Result<(), PluginChainStateError> {
        let state = state.into();
        if state.len() > MAX_PLUGIN_STATE_BYTES {
            return Err(PluginChainStateError { field: "state" });
        }
        self.state = state;
        Ok(())
    }

    /// The saved state as canonical unpadded base64url; empty for none.
    #[must_use]
    pub fn state_code(&self) -> String {
        base64url::encode(&self.state)
    }

    /// Parses [`Self::state_code`] strictly: canonical unpadded base64url.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainStateError`] for non-canonical base64url or a
    /// state larger than [`MAX_PLUGIN_STATE_BYTES`].
    pub fn set_state_code(&mut self, code: &str) -> Result<(), PluginChainStateError> {
        let state = base64url::decode(code).ok_or(PluginChainStateError { field: "state" })?;
        if base64url::encode(&state) != code {
            return Err(PluginChainStateError { field: "state" });
        }
        self.set_state(state)
    }

    /// Every named parameter value, in name order.
    pub fn parameters(&self) -> impl Iterator<Item = (&str, f32)> {
        self.parameters
            .iter()
            .map(|(name, bits)| (name.as_str(), f32::from_bits(*bits)))
    }

    /// Parameter `name`, if set.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<f32> {
        self.parameters.get(name).map(|bits| f32::from_bits(*bits))
    }

    /// How many parameters the slot holds.
    #[must_use]
    pub fn parameter_count(&self) -> usize {
        self.parameters.len()
    }

    /// Sets parameter `name` to `value`.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainStateError`] for an empty or malformed name, a
    /// non-finite value, or a new name above [`MAX_PLUGIN_PARAMETERS`].
    pub fn set_parameter(&mut self, name: &str, value: f32) -> Result<(), PluginChainStateError> {
        if !plugin_parameter_name_is_valid(name) {
            return Err(PluginChainStateError { field: "parameter" });
        }
        if !value.is_finite() {
            return Err(PluginChainStateError {
                field: "parameter_value",
            });
        }
        if !self.parameters.contains_key(name) && self.parameters.len() >= MAX_PLUGIN_PARAMETERS {
            return Err(PluginChainStateError {
                field: "parameters",
            });
        }
        self.parameters.insert(name.to_owned(), value.to_bits());
        Ok(())
    }

    /// Removes parameter `name`, returning its value.
    pub fn clear_parameter(&mut self, name: &str) -> Option<f32> {
        self.parameters.remove(name).map(f32::from_bits)
    }
}

/// The ordered plugins of one persisted path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginChainState {
    slots: Vec<PluginSlotState>,
}

impl PluginChainState {
    /// An empty chain.
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// A chain of `slots`, validated.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainStateError`] for too many slots, a duplicate or
    /// malformed plugin identifier, an out-of-range latency, or oversized
    /// state.
    pub fn from_slots(slots: Vec<PluginSlotState>) -> Result<Self, PluginChainStateError> {
        let chain = Self { slots };
        chain.validate()?;
        Ok(chain)
    }

    /// The slots in processing order.
    #[must_use]
    pub fn slots(&self) -> &[PluginSlotState] {
        &self.slots
    }

    /// How many slots the chain holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the chain holds no slot.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Appends `slot`.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainStateError`] when the chain would then be
    /// invalid; the chain is unchanged.
    pub fn push(&mut self, slot: PluginSlotState) -> Result<(), PluginChainStateError> {
        if self.slots.len() >= MAX_PLUGIN_CHAIN_SLOTS {
            return Err(PluginChainStateError { field: "slots" });
        }
        if self
            .slots
            .iter()
            .any(|existing| existing.plugin == slot.plugin)
        {
            return Err(PluginChainStateError { field: "plugin" });
        }
        self.slots.push(slot);
        self.validate()
    }

    /// Drops every slot.
    pub fn clear(&mut self) {
        self.slots.clear();
    }

    /// Validates every bound and the uniqueness of the plugin identifiers.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainStateError`] naming the first offending field.
    pub fn validate(&self) -> Result<(), PluginChainStateError> {
        if self.slots.len() > MAX_PLUGIN_CHAIN_SLOTS {
            return Err(PluginChainStateError { field: "slots" });
        }
        for (index, slot) in self.slots.iter().enumerate() {
            if !plugin_id_is_valid(&slot.plugin) {
                return Err(PluginChainStateError { field: "plugin" });
            }
            if self.slots[..index]
                .iter()
                .any(|earlier| earlier.plugin == slot.plugin)
            {
                return Err(PluginChainStateError { field: "plugin" });
            }
            if slot.latency_frames > MAX_PLUGIN_LATENCY_FRAMES {
                return Err(PluginChainStateError {
                    field: "latency_frames",
                });
            }
            if slot.state.len() > MAX_PLUGIN_STATE_BYTES {
                return Err(PluginChainStateError { field: "state" });
            }
            if slot.parameters.len() > MAX_PLUGIN_PARAMETERS {
                return Err(PluginChainStateError {
                    field: "parameters",
                });
            }
            for (name, bits) in &slot.parameters {
                if !plugin_parameter_name_is_valid(name) || !f32::from_bits(*bits).is_finite() {
                    return Err(PluginChainStateError { field: "parameter" });
                }
            }
        }
        Ok(())
    }
}

/// Whether `name` is a usable persisted plugin parameter name:
/// `[A-Za-z0-9._-]`, 1 through [`MAX_PLUGIN_PARAMETER_NAME_BYTES`] bytes.
#[must_use]
pub fn plugin_parameter_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PLUGIN_PARAMETER_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether `plugin` is a usable persisted plugin identifier:
/// `[A-Za-z0-9._-]`, 1 through [`MAX_PLUGIN_ID_BYTES`] bytes.
#[must_use]
pub fn plugin_id_is_valid(plugin: &str) -> bool {
    !plugin.is_empty()
        && plugin.len() <= MAX_PLUGIN_ID_BYTES
        && plugin
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// An out-of-range or malformed plugin chain field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginChainStateError {
    /// The offending field.
    pub field: &'static str,
}

impl fmt::Display for PluginChainStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "audio plugin chain {} is invalid", self.field)
    }
}

impl std::error::Error for PluginChainStateError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_is_bounded_and_unique() {
        let mut chain = PluginChainState::new();
        for index in 0..MAX_PLUGIN_CHAIN_SLOTS {
            chain
                .push(PluginSlotState::new(format!(
                    "freemix.reference.plugin{index}"
                )))
                .unwrap();
        }
        assert_eq!(chain.len(), MAX_PLUGIN_CHAIN_SLOTS);
        assert!(chain.push(PluginSlotState::new("one.too.many")).is_err());
        let mut duplicate = PluginChainState::new();
        duplicate
            .push(PluginSlotState::new("freemix.reference.trim"))
            .unwrap();
        assert!(
            duplicate
                .push(PluginSlotState::new("freemix.reference.trim"))
                .is_err()
        );
        for bad in ["", "has space", "sla/sh", "plus+", "quote\""] {
            assert!(PluginChainState::from_slots(vec![PluginSlotState::new(bad)]).is_err());
        }
        assert!(plugin_id_is_valid("freemix.reference.trim-v2_A"));
    }

    #[test]
    fn slot_parameters_are_bounded_and_round_trip() {
        let mut slot = PluginSlotState::new("freemix.reference.gain");
        slot.set_parameter("gain", -6.0).unwrap();
        slot.set_parameter("mix", 0.5).unwrap();
        assert_eq!(slot.parameter("gain"), Some(-6.0));
        assert_eq!(slot.parameter("missing"), None);
        assert_eq!(slot.parameter_count(), 2);
        assert_eq!(
            slot.parameters().collect::<Vec<_>>(),
            vec![("gain", -6.0), ("mix", 0.5)]
        );
        assert!(slot.set_parameter("", 1.0).is_err());
        assert!(slot.set_parameter("has space", 1.0).is_err());
        assert!(slot.set_parameter("nan", f32::NAN).is_err());
        assert!(slot.set_parameter("infinite", f32::INFINITY).is_err());
        for index in 0..MAX_PLUGIN_PARAMETERS - 2 {
            slot.set_parameter(&format!("p{index}"), 1.0).unwrap();
        }
        assert!(slot.set_parameter("one.too.many", 1.0).is_err());
        assert_eq!(slot.clear_parameter("mix"), Some(0.5));
        assert_eq!(slot.clear_parameter("mix"), None);
        let chain = PluginChainState::from_slots(vec![slot]).unwrap();
        assert_eq!(chain.slots()[0].parameter("gain"), Some(-6.0));
    }

    #[test]
    fn slot_bounds_and_state_codes_round_trip() {
        let mut slot = PluginSlotState::new("freemix.reference.delay");
        slot.set_bypassed(true);
        slot.set_latency(MAX_PLUGIN_LATENCY_FRAMES).unwrap();
        assert!(slot.set_latency(MAX_PLUGIN_LATENCY_FRAMES + 1).is_err());
        slot.set_state([0_u8, 255, 1, 2]).unwrap();
        assert_eq!(slot.state_code(), "AP8BAg");
        let mut parsed = PluginSlotState::new("freemix.reference.delay");
        parsed.set_state_code("AP8BAg").unwrap();
        assert_eq!(parsed.state(), [0_u8, 255, 1, 2]);
        assert!(parsed.set_state_code("AP8BA").is_err());
        assert!(slot.set_state(vec![0; MAX_PLUGIN_STATE_BYTES + 1]).is_err());
        let chain = PluginChainState::from_slots(vec![slot]).unwrap();
        let rendered = format!("{chain:?}");
        assert!(rendered.contains("freemix.reference.delay"));
    }
}
