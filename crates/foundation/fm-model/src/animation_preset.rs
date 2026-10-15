//! The project's animation preset library (GX-011): named In or Out
//! motions an operator saved from one title element or scene layer and
//! applies to any other.
//!
//! Each [`UserAnimationPreset`] keeps one phase of one target as an
//! [`fm_animation::PhaseMotion`]: its tracks relative to the target's
//! resting pose, its length and timebase, and the box its crop keys were
//! measured against. Applying it retimes it to the requested length and
//! sizes its crop to the new target (see [`fm_animation::PhaseMotion`]).
//! The library is ordered as authored, holds at most
//! [`MAX_PROJECT_ANIMATION_PRESETS`] presets with unique ids and unique,
//! nonblank names of at most [`MAX_ANIMATION_PRESET_NAME_BYTES`] bytes.

use core::{fmt, num::NonZeroU32};

use fm_animation::{Phase, PhaseMotion};

use crate::grade::grade_name_error;
use crate::{GradeEditError, LayerId, Scene, SceneAnimationError, scene_animates};

/// Most presets one project library may hold.
pub const MAX_PROJECT_ANIMATION_PRESETS: usize = 64;
/// Longest preset name, in UTF-8 bytes.
pub const MAX_ANIMATION_PRESET_NAME_BYTES: usize = 128;

/// Project-local identity of a library animation preset.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AnimationPresetId(NonZeroU32);

impl AnimationPresetId {
    #[must_use]
    pub const fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU32 {
        self.0
    }
}

impl fmt::Display for AnimationPresetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A named motion in the project library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserAnimationPreset {
    pub id: AnimationPresetId,
    pub name: String,
    /// The phase it was saved from: an entrance (In) or an exit (Out). It
    /// can be applied to either phase; pickers list it with its own.
    pub phase: Phase,
    pub motion: PhaseMotion,
}

/// A refused preset library edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimationPresetError {
    UnknownPreset(AnimationPresetId),
    DuplicateId(AnimationPresetId),
    DuplicateName(String),
    EmptyName,
    NameTooLong {
        bytes: usize,
        maximum: usize,
    },
    NameControlCharacter,
    TooManyPresets {
        maximum: usize,
    },
    /// An insertion index past the end of the library.
    IndexOutOfRange {
        index: usize,
        len: usize,
    },
}

impl fmt::Display for AnimationPresetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPreset(id) => write!(formatter, "animation preset {id} does not exist"),
            Self::DuplicateId(id) => write!(formatter, "animation preset id {id} is already used"),
            Self::DuplicateName(name) => write!(formatter, "name `{name}` is already used"),
            Self::EmptyName => formatter.write_str("name must not be blank"),
            Self::NameTooLong { bytes, maximum } => {
                write!(formatter, "name is {bytes} bytes; the maximum is {maximum}")
            }
            Self::NameControlCharacter => {
                formatter.write_str("name must not contain control characters")
            }
            Self::TooManyPresets { maximum } => write!(
                formatter,
                "the animation preset library holds at most {maximum} presets"
            ),
            Self::IndexOutOfRange { index, len } => {
                write!(formatter, "index {index} is past the end of {len} entries")
            }
        }
    }
}

impl std::error::Error for AnimationPresetError {}

/// Checks a preset name: nonblank, bounded, and free of control characters.
fn name_error(name: &str) -> Option<AnimationPresetError> {
    grade_name_error(name, MAX_ANIMATION_PRESET_NAME_BYTES).map(|error| match error {
        GradeEditError::NameTooLong { bytes, maximum } => {
            AnimationPresetError::NameTooLong { bytes, maximum }
        }
        GradeEditError::NameControlCharacter => AnimationPresetError::NameControlCharacter,
        _ => AnimationPresetError::EmptyName,
    })
}

/// The project's animation presets, in authored order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnimationPresetLibrary {
    presets: Vec<UserAnimationPreset>,
}

impl AnimationPresetLibrary {
    /// A library holding `presets` as given, unchecked;
    /// [`crate::Project::validate`] (or [`Self::checked`]) checks it.
    #[must_use]
    pub const fn from_presets_unchecked(presets: Vec<UserAnimationPreset>) -> Self {
        Self { presets }
    }

    /// A library built by inserting `presets` in order.
    ///
    /// # Errors
    ///
    /// The first [`AnimationPresetError`] an insertion reports.
    pub fn checked(presets: Vec<UserAnimationPreset>) -> Result<Self, AnimationPresetError> {
        let mut library = Self::default();
        for preset in presets {
            library.insert(None, preset)?;
        }
        Ok(library)
    }

    #[must_use]
    pub fn presets(&self) -> &[UserAnimationPreset] {
        &self.presets
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.presets.len()
    }

    #[must_use]
    pub fn get(&self, id: AnimationPresetId) -> Option<&UserAnimationPreset> {
        self.presets.iter().find(|preset| preset.id == id)
    }

    /// The position of preset `id` in the library.
    #[must_use]
    pub fn index_of(&self, id: AnimationPresetId) -> Option<usize> {
        self.presets.iter().position(|preset| preset.id == id)
    }

    /// The preset named exactly `name`.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&UserAnimationPreset> {
        self.presets.iter().find(|preset| preset.name == name)
    }

    /// The id a new preset gets: see [`crate::next_free_id`].
    #[must_use]
    pub fn next_id(&self) -> Option<AnimationPresetId> {
        crate::next_free_id(self.presets.iter().map(|preset| preset.id.get()), |_| false)
            .map(AnimationPresetId::new)
    }

    /// `name`, or `name (2)`, `name (3)`, ... : the first that no preset
    /// uses (cut to the name bound on a character boundary).
    #[must_use]
    pub fn unused_name(&self, name: &str) -> String {
        let fits = |candidate: &str, suffix: &str| {
            let mut end = MAX_ANIMATION_PRESET_NAME_BYTES.saturating_sub(suffix.len());
            while !candidate.is_char_boundary(end.min(candidate.len())) {
                end -= 1;
            }
            format!("{}{suffix}", &candidate[..end.min(candidate.len())])
        };
        let first = fits(name, "");
        if self.by_name(&first).is_none() {
            return first;
        }
        // At most 64 presets use 65 candidates, so one is free.
        (2..=MAX_PROJECT_ANIMATION_PRESETS + 2)
            .map(|count| fits(name, &format!(" ({count})")))
            .find(|candidate| self.by_name(candidate).is_none())
            .unwrap_or(first)
    }

    /// Inserts a preset at `index` (the end when `None`).
    ///
    /// # Errors
    ///
    /// Refuses a full library, a used id or name, an invalid name, and an
    /// index past the end; nothing changes on error.
    pub fn insert(
        &mut self,
        index: Option<usize>,
        preset: UserAnimationPreset,
    ) -> Result<(), AnimationPresetError> {
        if self.presets.len() >= MAX_PROJECT_ANIMATION_PRESETS {
            return Err(AnimationPresetError::TooManyPresets {
                maximum: MAX_PROJECT_ANIMATION_PRESETS,
            });
        }
        if self.get(preset.id).is_some() {
            return Err(AnimationPresetError::DuplicateId(preset.id));
        }
        if let Some(error) = name_error(&preset.name) {
            return Err(error);
        }
        if self.by_name(&preset.name).is_some() {
            return Err(AnimationPresetError::DuplicateName(preset.name));
        }
        let index = index.unwrap_or(self.presets.len());
        if index > self.presets.len() {
            return Err(AnimationPresetError::IndexOutOfRange {
                index,
                len: self.presets.len(),
            });
        }
        self.presets.insert(index, preset);
        Ok(())
    }

    /// Renames preset `id`.
    ///
    /// # Errors
    ///
    /// Refuses an unknown id and an invalid name or one another preset
    /// uses.
    pub fn rename(
        &mut self,
        id: AnimationPresetId,
        name: String,
    ) -> Result<(), AnimationPresetError> {
        if let Some(error) = name_error(&name) {
            return Err(error);
        }
        if self
            .presets
            .iter()
            .any(|preset| preset.id != id && preset.name == name)
        {
            return Err(AnimationPresetError::DuplicateName(name));
        }
        let preset = self
            .presets
            .iter_mut()
            .find(|preset| preset.id == id)
            .ok_or(AnimationPresetError::UnknownPreset(id))?;
        preset.name = name;
        Ok(())
    }

    /// Removes preset `id` and returns its former index with it. Nothing
    /// refers to a preset: applying one copies its keys.
    ///
    /// # Errors
    ///
    /// Refuses an unknown id.
    pub fn remove(
        &mut self,
        id: AnimationPresetId,
    ) -> Result<(usize, UserAnimationPreset), AnimationPresetError> {
        let index = self
            .index_of(id)
            .ok_or(AnimationPresetError::UnknownPreset(id))?;
        Ok((index, self.presets.remove(index)))
    }

    /// Every violation of the library rules, as `(preset, error)` in
    /// library order.
    #[must_use]
    pub fn violations(&self) -> Vec<(AnimationPresetId, AnimationPresetError)> {
        let mut violations = Vec::new();
        if self.presets.len() > MAX_PROJECT_ANIMATION_PRESETS
            && let Some(first) = self.presets.first()
        {
            violations.push((
                first.id,
                AnimationPresetError::TooManyPresets {
                    maximum: MAX_PROJECT_ANIMATION_PRESETS,
                },
            ));
        }
        for (index, preset) in self.presets.iter().enumerate() {
            let previous = &self.presets[..index];
            if previous.iter().any(|other| other.id == preset.id) {
                violations.push((preset.id, AnimationPresetError::DuplicateId(preset.id)));
            }
            if previous.iter().any(|other| other.name == preset.name) {
                violations.push((
                    preset.id,
                    AnimationPresetError::DuplicateName(preset.name.clone()),
                ));
            }
            if let Some(error) = name_error(&preset.name) {
                violations.push((preset.id, error));
            }
        }
        violations
    }
}

impl Scene {
    /// Applies a saved motion to one layer over `frames` frames of `phase`
    /// (see [`fm_animation::Clip::apply_motion`]): crop keys are sized to
    /// the layer's box (`geometry.width` x `geometry.height`, its own frame)
    /// and only the properties scene layers animate are applied. Atomic;
    /// returns the new animation revision.
    ///
    /// # Errors
    ///
    /// [`SceneAnimationError`]: an unknown layer, a motion that animates
    /// nothing a scene layer can (reported for its first property), or a
    /// refused clip edit. Nothing changes on error.
    pub fn apply_animation_motion(
        &mut self,
        layer: LayerId,
        phase: Phase,
        motion: &PhaseMotion,
        frames: u32,
    ) -> Result<u64, SceneAnimationError> {
        let geometry = self
            .layer_by_id(layer)
            .map(|layer| layer.geometry)
            .ok_or(SceneAnimationError::UnknownLayer(layer))?;
        if let Some(property) = motion.properties().next()
            && !motion.properties().any(scene_animates)
        {
            return Err(SceneAnimationError::UnsupportedProperty { layer, property });
        }
        self.edit_animation(|clip| {
            clip.apply_motion(
                layer,
                phase,
                motion,
                frames,
                geometry.width.max(1),
                geometry.height.max(1),
                scene_animates,
            )
            .map(|_| ())
        })
    }

    /// Saves `layer`'s tracks in `phase` as a motion measured against its
    /// box.
    ///
    /// # Errors
    ///
    /// [`fm_animation::MotionError`] when the layer is unknown (reported as
    /// [`fm_animation::MotionError::Empty`]) or has nothing to save.
    pub fn capture_animation_motion(
        &self,
        layer: LayerId,
        phase: Phase,
    ) -> Result<PhaseMotion, fm_animation::MotionError> {
        let geometry = self
            .layer_by_id(layer)
            .map(|layer| layer.geometry)
            .ok_or(fm_animation::MotionError::Empty)?;
        PhaseMotion::capture(
            &self.animation.clip,
            layer,
            phase,
            geometry.width.max(1),
            geometry.height.max(1),
        )
    }
}
