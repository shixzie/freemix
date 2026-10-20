//! Replicated input lifecycle (Protocol 2.38, IN-026): categories, colour
//! labels, pause, close, runtime offline state, and the catalogue changes of
//! duplicate, batch import, and multi-input removal.

use std::collections::HashSet;

use fm_protocol::{
    InputCategoryStatus, InputLifecycleOperation, InputLifecycleStatus, WireInputId,
    WireInputSource,
};
use fm_types::InputId;

use crate::{
    ModelError, ProjectState, apply_input_added, apply_input_removed, desired_input_in_use,
    valid_input_name,
};

/// Validates a category list: nonzero unique ids and unique nonblank names.
pub(crate) fn valid_categories(categories: &[InputCategoryStatus]) -> bool {
    let mut ids = HashSet::with_capacity(categories.len());
    let mut names = HashSet::with_capacity(categories.len());
    categories.iter().all(|category| {
        category.id != 0
            && !category.name.trim().is_empty()
            && ids.insert(category.id)
            && names.insert(category.name.to_lowercase())
    })
}

fn known(state: &ProjectState, input: WireInputId) -> Result<InputId, ModelError> {
    let input = input.to_domain();
    if state.inputs.contains(&input) {
        Ok(input)
    } else {
        Err(ModelError::UnknownInput(input))
    }
}

/// Validates added inputs: new, unique, validly and uniquely named.
fn validate_added(state: &ProjectState, added: &[(InputId, String)]) -> Result<(), ModelError> {
    let mut inputs = HashSet::with_capacity(added.len());
    let mut names = HashSet::with_capacity(added.len());
    for (input, name) in added {
        if state.inputs.contains(input) || !inputs.insert(*input) {
            return Err(ModelError::DuplicateInput(*input));
        }
        if !valid_input_name(name) || state.input_names.contains(name) || !names.insert(name) {
            return Err(ModelError::InvalidInputNames);
        }
    }
    Ok(())
}

/// Validates an `input_lifecycle_changed` against the replicated project.
pub(crate) fn validate(
    operation: &InputLifecycleOperation,
    added: &[(InputId, String)],
    state: &ProjectState,
) -> Result<(), ModelError> {
    let invalid = Err(ModelError::InvalidInputLifecycleChange);
    match operation {
        InputLifecycleOperation::Duplicate { source, input, .. } => {
            known(state, *source)?;
            if added.len() != 1 || added[0].0 != input.to_domain() {
                return invalid;
            }
            return validate_added(state, added);
        }
        InputLifecycleOperation::Import { .. } => return validate_added(state, added),
        InputLifecycleOperation::ReplaceSource { input, .. }
        | InputLifecycleOperation::SetColor { input, .. }
        | InputLifecycleOperation::SetPaused { input, .. }
        | InputLifecycleOperation::SetClosed { input, .. } => {
            known(state, *input)?;
        }
        InputLifecycleOperation::SetCategory { input, category } => {
            known(state, *input)?;
            if category.is_some_and(|id| !state.input_categories.iter().any(|c| c.id == id)) {
                return invalid;
            }
        }
        InputLifecycleOperation::SetCategories { categories } => {
            let assigned_missing = state.input_lifecycle.values().any(|lifecycle| {
                lifecycle
                    .category
                    .is_some_and(|id| !categories.iter().any(|c| c.id == id))
            });
            if !valid_categories(categories) || assigned_missing {
                return invalid;
            }
        }
        InputLifecycleOperation::RemoveInputs { inputs } => {
            let mut seen = HashSet::with_capacity(inputs.len());
            for input in inputs {
                let input = known(state, *input)?;
                if !seen.insert(input) {
                    return invalid;
                }
                if desired_input_in_use(state, input) {
                    return Err(ModelError::InputInUse(input));
                }
            }
        }
    }
    if added.is_empty() { Ok(()) } else { invalid }
}

fn edit_lifecycle(
    state: &mut ProjectState,
    input: WireInputId,
    edit: impl FnOnce(&mut InputLifecycleStatus),
) {
    let input = input.to_domain();
    let mut lifecycle = state
        .input_lifecycle
        .get(&input)
        .copied()
        .unwrap_or_default();
    edit(&mut lifecycle);
    store_lifecycle(state, input, lifecycle);
}

pub(crate) fn store_lifecycle(
    state: &mut ProjectState,
    input: InputId,
    lifecycle: InputLifecycleStatus,
) {
    if lifecycle == InputLifecycleStatus::default() {
        state.input_lifecycle.remove(&input);
    } else {
        state.input_lifecycle.insert(input, lifecycle);
    }
}

/// Copies what a duplicate shares with its source: the audio strip, the
/// category and colour label, and any title or scene.
fn copy_duplicate(state: &mut ProjectState, source: InputId, input: InputId) {
    if let Some(strip) = state
        .input_audio_strips
        .iter()
        .find(|strip| strip.input == source)
        .copied()
        && let Some(copy) = state
            .input_audio_strips
            .iter_mut()
            .find(|strip| strip.input == input)
    {
        *copy = crate::InputAudioStripStatus { input, ..strip };
    }
    let labels = state
        .input_lifecycle
        .get(&source)
        .copied()
        .unwrap_or_default();
    store_lifecycle(
        state,
        input,
        InputLifecycleStatus {
            category: labels.category,
            color: labels.color,
            ..InputLifecycleStatus::default()
        },
    );
    if let Some(title) = state.titles.iter().find(|title| title.input == source) {
        let mut title = title.clone();
        title.input = input;
        let index = state.titles.partition_point(|other| other.input < input);
        state.titles.insert(index, title);
    }
    if let Some(scene) = state.scenes.iter().find(|scene| scene.input == source) {
        let mut scene = scene.clone();
        scene.input = input;
        let index = state.scenes.partition_point(|other| other.input < input);
        state.scenes.insert(index, scene);
    }
}

/// Applies a validated `input_lifecycle_changed`.
pub(crate) fn apply(
    state: &mut ProjectState,
    operation: InputLifecycleOperation,
    added: Vec<(InputId, String)>,
) {
    match operation {
        InputLifecycleOperation::Duplicate { source, input, .. } => {
            for (added, name) in added {
                apply_input_added(state, added, name);
            }
            copy_duplicate(state, source.to_domain(), input.to_domain());
            crate::media_lists::copy_duplicate(state, source.to_domain(), input.to_domain());
        }
        InputLifecycleOperation::Import { .. } => {
            for (input, name) in added {
                apply_input_added(state, input, name);
            }
        }
        InputLifecycleOperation::RemoveInputs { inputs } => {
            for input in inputs {
                apply_input_removed(state, input.to_domain());
            }
        }
        InputLifecycleOperation::ReplaceSource { input, source } => {
            if !matches!(source, WireInputSource::Media { .. }) {
                state.media.remove(&input.to_domain());
            }
        }
        InputLifecycleOperation::SetCategory { input, category } => {
            edit_lifecycle(state, input, |lifecycle| lifecycle.category = category);
        }
        InputLifecycleOperation::SetColor { input, color } => {
            edit_lifecycle(state, input, |lifecycle| lifecycle.color = color);
        }
        InputLifecycleOperation::SetPaused { input, paused } => {
            edit_lifecycle(state, input, |lifecycle| lifecycle.paused = paused);
        }
        InputLifecycleOperation::SetClosed { input, closed } => {
            edit_lifecycle(state, input, |lifecycle| lifecycle.closed = closed);
        }
        InputLifecycleOperation::SetCategories { categories } => {
            state.input_categories = categories;
        }
    }
}
