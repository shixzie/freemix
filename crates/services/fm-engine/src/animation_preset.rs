//! The project animation preset library (GX-011) edited through the
//! engine's command path.
//!
//! The show holds the project's [`AnimationPresetLibrary`]. Saving, renaming,
//! and deleting a preset are project edits ([`EngineAnimationPresetEdit`]):
//! the project padlock covers them and each is recorded in the undo history
//! with an exact inverse (an add is undone by removing the preset, a removal
//! by adding it back at its index, a rename by the old name). Nothing refers
//! to a preset: applying one to a title element or scene layer
//! ([`crate::TitleAnimationChange::Motion`],
//! [`crate::SceneAnimationChange::Motion`]) copies its keys into that
//! animation, recorded as the exact clip it produced, so deleting a preset
//! never changes an animation and undoing an application never needs the
//! preset.

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::{
    AnimationPresetError, AnimationPresetId, AnimationPresetLibrary, UserAnimationPreset,
    fm_animation::PhaseMotion,
};

use crate::ShowState;

/// One preset library edit. Every variant is a project edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineAnimationPresetEdit {
    /// Inserts a preset at `index` (the end when `None`).
    Add {
        preset: Box<UserAnimationPreset>,
        index: Option<usize>,
    },
    /// Renames a preset.
    Rename { id: AnimationPresetId, name: String },
    /// Removes a preset.
    Remove { id: AnimationPresetId },
}

impl EngineAnimationPresetEdit {
    /// The preset this edit names.
    #[must_use]
    pub fn id(&self) -> AnimationPresetId {
        match self {
            Self::Add { preset, .. } => preset.id,
            Self::Rename { id, .. } | Self::Remove { id } => *id,
        }
    }
}

/// Converts a refused library edit into a receipt rejection with a stable
/// field code.
pub(crate) fn preset_rejection(error: &AnimationPresetError) -> Rejection {
    let (code, field, issue) = match error {
        AnimationPresetError::UnknownPreset(_) => {
            (RejectionCode::NotFound, "preset", "unknown_preset")
        }
        AnimationPresetError::DuplicateId(_) => {
            (RejectionCode::Conflict, "preset", "duplicate_preset")
        }
        AnimationPresetError::DuplicateName(_) => {
            (RejectionCode::Conflict, "name", "duplicate_name")
        }
        AnimationPresetError::EmptyName
        | AnimationPresetError::NameTooLong { .. }
        | AnimationPresetError::NameControlCharacter => {
            (RejectionCode::InvalidCommand, "name", "invalid_name")
        }
        AnimationPresetError::TooManyPresets { .. } => {
            (RejectionCode::Conflict, "preset", "too_many_presets")
        }
        AnimationPresetError::IndexOutOfRange { .. } => {
            (RejectionCode::InvalidCommand, "index", "out_of_range")
        }
    };
    let message = error.to_string();
    Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message))
}

/// The motion of library preset `id`, for an animation edit applying it.
pub(crate) fn motion_of(
    state: &ShowState,
    id: AnimationPresetId,
) -> Result<&PhaseMotion, Rejection> {
    state
        .animation_presets()
        .get(id)
        .map(|preset| &preset.motion)
        .ok_or_else(|| preset_rejection(&AnimationPresetError::UnknownPreset(id)))
}

/// Applies one library edit.
pub(crate) fn apply(
    library: &mut AnimationPresetLibrary,
    edit: &EngineAnimationPresetEdit,
) -> Result<(), Rejection> {
    match edit {
        EngineAnimationPresetEdit::Add { preset, index } => {
            library.insert(*index, (**preset).clone())
        }
        EngineAnimationPresetEdit::Rename { id, name } => library.rename(*id, name.clone()),
        EngineAnimationPresetEdit::Remove { id } => library.remove(*id).map(|_| ()),
    }
    .map_err(|error| preset_rejection(&error))
}

/// `edit` with an implicit position made explicit against `state`: a preset
/// added without an index is added at the end of the current library.
pub(crate) fn normalized(
    state: &ShowState,
    edit: &EngineAnimationPresetEdit,
) -> EngineAnimationPresetEdit {
    match edit {
        EngineAnimationPresetEdit::Add {
            preset,
            index: None,
        } => EngineAnimationPresetEdit::Add {
            preset: preset.clone(),
            index: Some(state.animation_presets().len()),
        },
        edit => edit.clone(),
    }
}

/// The exact edit that reverts `edit` on `state`, or `None` when it targets
/// nothing there.
pub(crate) fn inverse(
    state: &ShowState,
    edit: &EngineAnimationPresetEdit,
) -> Option<EngineAnimationPresetEdit> {
    let library = state.animation_presets();
    match edit {
        EngineAnimationPresetEdit::Add { preset, .. } => library
            .get(preset.id)
            .is_none()
            .then_some(EngineAnimationPresetEdit::Remove { id: preset.id }),
        EngineAnimationPresetEdit::Rename { id, .. } => {
            library
                .get(*id)
                .map(|current| EngineAnimationPresetEdit::Rename {
                    id: *id,
                    name: current.name.clone(),
                })
        }
        EngineAnimationPresetEdit::Remove { id } => {
            library
                .index_of(*id)
                .map(|index| EngineAnimationPresetEdit::Add {
                    preset: Box::new(library.presets()[index].clone()),
                    index: Some(index),
                })
        }
    }
}
