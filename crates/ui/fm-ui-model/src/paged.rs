//! Paged inputs in the replicated model (IN-014): each document input's
//! page, page count, and source state.

use fm_protocol::{PagedStateMessage, PagedStatus};
use fm_types::InputId;

use crate::{ClientModel, ModelError, ProjectState, Revision};

impl ProjectState {
    /// Paged input `input`'s replicated status (IN-014).
    #[must_use]
    pub fn paged_status(&self, input: InputId) -> Option<&PagedStatus> {
        self.paged.get(&input)
    }

    /// Every paged input's replicated status, ascending by input.
    pub fn paged_statuses(&self) -> impl Iterator<Item = &PagedStatus> {
        self.paged.values()
    }

    /// Whether every `paged_state` the snapshot announced arrived.
    #[must_use]
    pub const fn paged_complete(&self) -> bool {
        self.paged_states_pending == 0
    }
}

impl ClientModel {
    /// Adds one of the `paged_state` records the installed snapshot
    /// announced (IN-014), in ascending input order.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::PagedStateMismatch`] when the record is at another
    /// revision, unannounced, out of input order, or for an unknown input;
    /// the model is then unchanged and the caller must resynchronize.
    pub fn apply_paged_state(&mut self, message: PagedStateMessage) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.status.input.to_domain();
        if Revision::new(message.revision) != revision
            || state.paged_states_pending == 0
            || !state.inputs.contains(&input)
            || state
                .paged
                .last_key_value()
                .is_some_and(|(last, _)| last.get() >= input.get())
        {
            return Err(ModelError::PagedStateMismatch);
        }
        state.paged.insert(input, message.status);
        state.paged_states_pending -= 1;
        Ok(())
    }

    /// Replaces a paged input's status from a `paged` runtime event
    /// (IN-014). Runtime events never move the durable cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::InvalidPagedChange`] for an input outside the project.
    pub fn apply_runtime_paged(&mut self, status: PagedStatus) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = status.input.to_domain();
        if !state.inputs.contains(&input) {
            return Err(ModelError::InvalidPagedChange(input));
        }
        state.paged.insert(input, status);
        Ok(())
    }
}
