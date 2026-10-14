use std::collections::BTreeMap;

use crate::{
    CameraMove, CameraPose, CameraState, EasingCurve, MAX_MOVE_FRAMES, PlanError, PresetId,
};

/// Maximum presets in one store.
pub const MAX_PRESETS: usize = 64;
/// Maximum preset name length in UTF-8 bytes.
pub const MAX_PRESET_NAME_BYTES: usize = 64;

/// A named camera pose plus how the camera travels to it when recalled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtualSetPreset {
    id: PresetId,
    name: String,
    pose: CameraPose,
    move_frames: u32,
    easing: EasingCurve,
}

impl VirtualSetPreset {
    /// # Errors
    ///
    /// Rejects empty or overlong names and moves above [`MAX_MOVE_FRAMES`].
    pub fn new(
        id: PresetId,
        name: impl Into<String>,
        pose: CameraPose,
        move_frames: u32,
        easing: EasingCurve,
    ) -> Result<Self, PlanError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(PlanError::EmptyPresetName);
        }
        if name.len() > MAX_PRESET_NAME_BYTES {
            return Err(PlanError::PresetNameTooLong {
                bytes: name.len(),
                maximum: MAX_PRESET_NAME_BYTES,
            });
        }
        if move_frames > MAX_MOVE_FRAMES {
            return Err(PlanError::MoveTooLong {
                frames: move_frames,
                maximum: MAX_MOVE_FRAMES,
            });
        }
        Ok(Self {
            id,
            name,
            pose,
            move_frames,
            easing,
        })
    }

    #[must_use]
    pub const fn id(&self) -> PresetId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn pose(&self) -> CameraPose {
        self.pose
    }

    #[must_use]
    pub const fn move_frames(&self) -> u32 {
        self.move_frames
    }

    #[must_use]
    pub const fn easing(&self) -> &EasingCurve {
        &self.easing
    }
}

/// A bounded, id-ordered collection of validated presets.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PresetStore {
    presets: BTreeMap<PresetId, VirtualSetPreset>,
}

impl PresetStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// # Errors
    ///
    /// Rejects duplicate ids and inserts beyond [`MAX_PRESETS`].
    pub fn insert(&mut self, preset: VirtualSetPreset) -> Result<(), PlanError> {
        if self.presets.contains_key(&preset.id) {
            return Err(PlanError::DuplicatePreset(preset.id));
        }
        if self.presets.len() >= MAX_PRESETS {
            return Err(PlanError::TooManyPresets {
                maximum: MAX_PRESETS,
            });
        }
        self.presets.insert(preset.id, preset);
        Ok(())
    }

    /// Replaces an existing preset and returns the previous value.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::UnknownPreset`] when the id is not stored.
    pub fn replace(&mut self, preset: VirtualSetPreset) -> Result<VirtualSetPreset, PlanError> {
        let slot = self
            .presets
            .get_mut(&preset.id)
            .ok_or(PlanError::UnknownPreset(preset.id))?;
        Ok(core::mem::replace(slot, preset))
    }

    /// # Errors
    ///
    /// Returns [`PlanError::UnknownPreset`] when the id is not stored.
    pub fn remove(&mut self, id: PresetId) -> Result<VirtualSetPreset, PlanError> {
        self.presets.remove(&id).ok_or(PlanError::UnknownPreset(id))
    }

    #[must_use]
    pub fn get(&self, id: PresetId) -> Option<&VirtualSetPreset> {
        self.presets.get(&id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.presets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &VirtualSetPreset> {
        self.presets.values()
    }
}

/// A recall scheduled for a frame boundary. The preset is snapshotted when
/// the recall is requested, so later store edits cannot tear it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingRecall {
    pub at_frame: u64,
    pub preset: VirtualSetPreset,
}

/// Drives the virtual camera on the output frame timeline and applies preset
/// recalls atomically at their scheduled frame boundary.
///
/// A recall at frame `N` starts a new move at exactly `N` from the camera
/// state the previous move had at `N` (snapped to the
/// [`crate::CAMERA_STATE_QUANTUM`] grid), independent of how often or with
/// what gaps frames are evaluated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CameraDirector {
    active: CameraMove,
    active_preset: Option<PresetId>,
    pending: Option<PendingRecall>,
    last_frame: Option<u64>,
}

impl CameraDirector {
    #[must_use]
    pub fn new(initial: CameraPose, start_frame: u64) -> Self {
        Self {
            active: CameraMove::hold(initial.state(), start_frame),
            active_preset: None,
            pending: None,
            last_frame: None,
        }
    }

    #[must_use]
    pub const fn active_move(&self) -> &CameraMove {
        &self.active
    }

    #[must_use]
    pub const fn active_preset(&self) -> Option<PresetId> {
        self.active_preset
    }

    #[must_use]
    pub const fn pending(&self) -> Option<&PendingRecall> {
        self.pending.as_ref()
    }

    /// Schedules `preset_id` for `at_frame`, replacing (and returning) any
    /// recall that was already pending.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::UnknownPreset`] or [`PlanError::RecallInPast`]
    /// when `at_frame` is not after the last evaluated frame; the director is
    /// left unchanged on error.
    pub fn request_recall(
        &mut self,
        store: &PresetStore,
        preset_id: PresetId,
        at_frame: u64,
    ) -> Result<Option<PendingRecall>, PlanError> {
        let preset = store
            .get(preset_id)
            .ok_or(PlanError::UnknownPreset(preset_id))?
            .clone();
        if let Some(last) = self.last_frame
            && at_frame <= last
        {
            return Err(PlanError::RecallInPast {
                requested: at_frame,
                earliest: last.saturating_add(1),
            });
        }
        Ok(self.pending.replace(PendingRecall { at_frame, preset }))
    }

    pub fn cancel_recall(&mut self) -> Option<PendingRecall> {
        self.pending.take()
    }

    /// Evaluates the camera at `frame`, first applying a due recall.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::NonMonotonicFrame`] when `frame` precedes the
    /// last evaluated frame, or [`PlanError::ArithmeticOverflow`].
    pub fn state_at(&mut self, frame: u64) -> Result<CameraState, PlanError> {
        if let Some(last) = self.last_frame
            && frame < last
        {
            return Err(PlanError::NonMonotonicFrame {
                requested: frame,
                last,
            });
        }
        if let Some(pending) = self.pending.as_ref()
            && pending.at_frame <= frame
        {
            let start = self.active.state_at(pending.at_frame)?.quantized()?;
            let next = CameraMove::new(
                start,
                pending.preset.pose().state(),
                pending.at_frame,
                pending.preset.move_frames(),
                pending.preset.easing().clone(),
            )?;
            self.active = next;
            self.active_preset = Some(pending.preset.id());
            self.pending = None;
        }
        let state = self.active.state_at(frame)?;
        self.last_frame = Some(frame);
        Ok(state)
    }
}
