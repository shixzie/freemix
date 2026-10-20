//! Replicated media lists (IN-006): each list input's items, options, and
//! the active item, from the snapshot's `media_list_state` records and every
//! `media_list_changed` event.

use std::collections::HashSet;

use fm_protocol::MediaListStatus;
use fm_types::InputId;

use crate::{ModelError, ProjectState};

/// Returns whether `status` is a coherent list: unique items within the
/// bounds and a play order that is a permutation of them with the active
/// item at the position. The codec already refuses anything else; the model
/// checks again so a locally built status cannot desynchronize it.
pub(crate) fn status_is_valid(status: &MediaListStatus) -> bool {
    let ids: HashSet<u32> = status.items.iter().map(|item| item.id).collect();
    let order: HashSet<u32> = status.order.iter().copied().collect();
    !status.items.is_empty()
        && status.items.len() <= fm_protocol::MAX_MEDIA_LIST_ITEMS
        && ids.len() == status.items.len()
        && order == ids
        && status.order.len() == status.items.len()
        && usize::try_from(status.position)
            .ok()
            .and_then(|position| status.order.get(position))
            == Some(&status.active)
        && status.lengths.len() == status.items.len()
        && status
            .lengths
            .iter()
            .flatten()
            .all(|length| length.is_valid())
}

/// Validates a `media_list_changed`: a creation adds a new, uniquely named
/// input; every other operation names an existing list input.
pub(crate) fn validate_change(
    state: &ProjectState,
    created: Option<&String>,
    status: &MediaListStatus,
) -> Result<(), ModelError> {
    let input = status.input.to_domain();
    let invalid = Err(ModelError::InvalidMediaListChange(input));
    if !status_is_valid(status) || !state.media_lists_complete() {
        return invalid;
    }
    match created {
        Some(name) => {
            if state.inputs.contains(&input) {
                return Err(ModelError::DuplicateInput(input));
            }
            if !crate::valid_input_name(name) || state.input_names.contains(name) {
                return Err(ModelError::InvalidInputNames);
            }
            if state.media_lists.len() >= usize::from(fm_protocol::MAX_MEDIA_LIST_STATES) {
                return invalid;
            }
        }
        None if !state.media_lists.contains_key(&input) => return invalid,
        None => {}
    }
    Ok(())
}

/// Applies a validated `media_list_changed`.
pub(crate) fn apply_change(
    state: &mut ProjectState,
    created: Option<String>,
    status: MediaListStatus,
) {
    let input = status.input.to_domain();
    if let Some(name) = created {
        crate::apply_input_added(state, input, name);
    }
    state.media_lists.insert(input, status);
}

/// Copies a duplicated list input's list onto its duplicate.
pub(crate) fn copy_duplicate(state: &mut ProjectState, source: InputId, input: InputId) {
    if let Some(list) = state.media_lists.get(&source) {
        let mut list = list.clone();
        list.input = fm_protocol::WireInputId::from_domain(input);
        state.media_lists.insert(input, list);
    }
}

impl crate::ClientModel {
    /// Takes a list's probed item lengths from a `media_list` runtime event
    /// (Protocol 2.91). Runtime events never move the durable cursor, so
    /// only the lengths are taken, and only while the replicated list has
    /// exactly the event's items; an event about other items (one a durable
    /// change already superseded) is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::InvalidMediaListChange`] for an incoherent status or an
    /// input that is not a media list.
    pub fn apply_runtime_media_list(&mut self, status: MediaListStatus) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = status.input.to_domain();
        if !status_is_valid(&status) {
            return Err(ModelError::InvalidMediaListChange(input));
        }
        let Some(list) = state.media_lists.get_mut(&input) else {
            return Err(ModelError::InvalidMediaListChange(input));
        };
        if list.items == status.items {
            list.lengths = status.lengths;
        }
        Ok(())
    }
}
