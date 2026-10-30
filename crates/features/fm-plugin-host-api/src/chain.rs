//! The ordered plugin chain of one audio or video path (AU-006).
//!
//! A chain holds up to [`MAX_PLUGIN_CHAIN_SLOTS`] plugins in processing
//! order. Each slot names one discovered plugin, may be bypassed, reports
//! the latency it adds in frames, and can keep the plugin's last opaque
//! state snapshot. Bypassing a slot skips it and drops its latency from the
//! chain's effective latency, which the engine uses to delay every parallel
//! path by the sum. A plugin appears at most once, so a slot is identifiable
//! by its plugin even after reordering.

use core::fmt;

use crate::{PluginId, StateSnapshot};

/// Most plugins one chain holds.
pub const MAX_PLUGIN_CHAIN_SLOTS: usize = 8;
/// Most frames of latency one slot may report.
pub const MAX_PLUGIN_LATENCY_FRAMES: u32 = 60_000;

/// One plugin's place in a chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSlot {
    plugin: PluginId,
    bypassed: bool,
    latency_frames: u32,
    snapshot: Option<StateSnapshot>,
}

impl PluginSlot {
    /// An active slot for `plugin` with no latency and no saved state.
    #[must_use]
    pub fn new(plugin: impl Into<PluginId>) -> Self {
        Self {
            plugin: plugin.into(),
            bypassed: false,
            latency_frames: 0,
            snapshot: None,
        }
    }

    /// The plugin this slot hosts.
    #[must_use]
    pub const fn plugin(&self) -> &PluginId {
        &self.plugin
    }

    /// Whether the slot is skipped.
    #[must_use]
    pub const fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// The latency the plugin adds while active, in frames.
    #[must_use]
    pub const fn latency_frames(&self) -> u32 {
        self.latency_frames
    }

    /// The plugin's last saved state, if any.
    #[must_use]
    pub const fn snapshot(&self) -> Option<&StateSnapshot> {
        self.snapshot.as_ref()
    }

    /// Sets whether the slot is skipped.
    pub const fn set_bypassed(&mut self, bypassed: bool) {
        self.bypassed = bypassed;
    }

    /// Sets the latency the plugin adds while active.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainError::LatencyTooLarge`] above
    /// [`MAX_PLUGIN_LATENCY_FRAMES`].
    pub fn set_latency(&mut self, frames: u32) -> Result<(), PluginChainError> {
        if frames > MAX_PLUGIN_LATENCY_FRAMES {
            return Err(PluginChainError::LatencyTooLarge {
                frames,
                maximum: MAX_PLUGIN_LATENCY_FRAMES,
            });
        }
        self.latency_frames = frames;
        Ok(())
    }

    /// Stores `snapshot` as the plugin's state.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainError::SnapshotPluginMismatch`] when the
    /// snapshot belongs to another plugin.
    pub fn store_snapshot(&mut self, snapshot: StateSnapshot) -> Result<(), PluginChainError> {
        if snapshot.plugin_id() != &self.plugin {
            return Err(PluginChainError::SnapshotPluginMismatch {
                expected: self.plugin.clone(),
                found: snapshot.plugin_id().clone(),
            });
        }
        self.snapshot = Some(snapshot);
        Ok(())
    }

    /// Drops the plugin's saved state.
    pub fn clear_snapshot(&mut self) {
        self.snapshot = None;
    }
}

/// The ordered plugins of one path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginChain {
    slots: Vec<PluginSlot>,
}

impl PluginChain {
    /// An empty chain.
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// The slots in processing order.
    #[must_use]
    pub fn slots(&self) -> &[PluginSlot] {
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

    /// Adds `slot` at `index` (0 through the length).
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainError::OutOfRange`] for an index past the end,
    /// [`PluginChainError::DuplicatePlugin`] when the plugin is already in
    /// the chain, or [`PluginChainError::TooManySlots`] at
    /// [`MAX_PLUGIN_CHAIN_SLOTS`].
    pub fn insert(&mut self, index: usize, slot: PluginSlot) -> Result<(), PluginChainError> {
        if index > self.slots.len() {
            return Err(PluginChainError::OutOfRange {
                index,
                len: self.slots.len(),
            });
        }
        if self.contains(slot.plugin()) {
            return Err(PluginChainError::DuplicatePlugin(slot.plugin().clone()));
        }
        if self.slots.len() >= MAX_PLUGIN_CHAIN_SLOTS {
            return Err(PluginChainError::TooManySlots {
                maximum: MAX_PLUGIN_CHAIN_SLOTS,
            });
        }
        self.slots.insert(index, slot);
        Ok(())
    }

    /// Appends `slot`.
    ///
    /// # Errors
    ///
    /// The same refusals as [`Self::insert`].
    pub fn push(&mut self, slot: PluginSlot) -> Result<(), PluginChainError> {
        self.insert(self.slots.len(), slot)
    }

    /// Removes and returns the slot at `index`.
    pub fn remove(&mut self, index: usize) -> Option<PluginSlot> {
        (index < self.slots.len()).then(|| self.slots.remove(index))
    }

    /// Moves the slot at `from` to `to`, shifting the slots between.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainError::OutOfRange`] for either index.
    pub fn move_to(&mut self, from: usize, to: usize) -> Result<(), PluginChainError> {
        if from >= self.slots.len() {
            return Err(PluginChainError::OutOfRange {
                index: from,
                len: self.slots.len(),
            });
        }
        if to >= self.slots.len() {
            return Err(PluginChainError::OutOfRange {
                index: to,
                len: self.slots.len(),
            });
        }
        if from != to {
            let slot = self.slots.remove(from);
            self.slots.insert(to, slot);
        }
        Ok(())
    }

    /// Whether `plugin` is in the chain.
    #[must_use]
    pub fn contains(&self, plugin: &PluginId) -> bool {
        self.slots.iter().any(|slot| slot.plugin() == plugin)
    }

    /// The slot hosting `plugin`.
    #[must_use]
    pub fn slot(&self, plugin: &PluginId) -> Option<&PluginSlot> {
        self.slots.iter().find(|slot| slot.plugin() == plugin)
    }

    /// The slot hosting `plugin`, mutably.
    pub fn slot_mut(&mut self, plugin: &PluginId) -> Option<&mut PluginSlot> {
        self.slots.iter_mut().find(|slot| slot.plugin() == plugin)
    }

    /// Bypasses or re-enables the slot at `index`.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainError::OutOfRange`] for an index past the end.
    pub fn set_bypassed(&mut self, index: usize, bypassed: bool) -> Result<(), PluginChainError> {
        let len = self.slots.len();
        self.slots
            .get_mut(index)
            .ok_or(PluginChainError::OutOfRange { index, len })?
            .set_bypassed(bypassed);
        Ok(())
    }

    /// Sets the latency of the slot at `index`.
    ///
    /// # Errors
    ///
    /// Returns [`PluginChainError::OutOfRange`] for an index past the end or
    /// the latency refusal of [`PluginSlot::set_latency`].
    pub fn set_latency(&mut self, index: usize, frames: u32) -> Result<(), PluginChainError> {
        let len = self.slots.len();
        self.slots
            .get_mut(index)
            .ok_or(PluginChainError::OutOfRange { index, len })?
            .set_latency(frames)
    }

    /// The latency every parallel path must be delayed by: the sum of the
    /// active slots' latencies.
    #[must_use]
    pub fn effective_latency_frames(&self) -> u32 {
        self.slots
            .iter()
            .filter(|slot| !slot.is_bypassed())
            .fold(0_u32, |total, slot| {
                total.saturating_add(slot.latency_frames())
            })
    }

    /// The plugins that process media, in order.
    pub fn active_plugins(&self) -> impl Iterator<Item = &PluginId> {
        self.slots
            .iter()
            .filter(|slot| !slot.is_bypassed())
            .map(PluginSlot::plugin)
    }
}

/// Why a chain edit was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginChainError {
    /// The chain already holds [`MAX_PLUGIN_CHAIN_SLOTS`] slots.
    TooManySlots { maximum: usize },
    /// The plugin already has a slot.
    DuplicatePlugin(PluginId),
    /// The index is past the end of the chain.
    OutOfRange { index: usize, len: usize },
    /// A slot's latency is above [`MAX_PLUGIN_LATENCY_FRAMES`].
    LatencyTooLarge { frames: u32, maximum: u32 },
    /// A state snapshot belongs to another plugin.
    SnapshotPluginMismatch { expected: PluginId, found: PluginId },
}

impl fmt::Display for PluginChainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManySlots { maximum } => {
                write!(formatter, "a plugin chain holds at most {maximum} slots")
            }
            Self::DuplicatePlugin(plugin) => {
                write!(formatter, "plugin {plugin} already has a chain slot")
            }
            Self::OutOfRange { index, len } => {
                write!(formatter, "chain index {index} is outside 0..{len}")
            }
            Self::LatencyTooLarge { frames, maximum } => write!(
                formatter,
                "a plugin slot adds at most {maximum} frames of latency, not {frames}"
            ),
            Self::SnapshotPluginMismatch { expected, found } => write!(
                formatter,
                "a {found} snapshot cannot be stored in the {expected} slot"
            ),
        }
    }
}

impl std::error::Error for PluginChainError {}
