//! Operator-facing explanations for disabled controls.
//!
//! Every gated control shows its action (and shortcut) when enabled and the
//! first failing gate when disabled, so a greyed-out key is never a mystery.

use egui::Response;

use crate::{StudioConnectionStatus, StudioUiState, TransitionGate};

/// The session capability a control depends on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Capability {
    SelectPreview,
    Transition,
    Audio,
    EditProject,
}

/// Returns the first gate that blocks `capability`, in the order an operator
/// would fix them: connection, synchronized state, permission, then a manual
/// transition in flight. Falls back to a state-specific explanation when every
/// session gate passes (for example Fade-to-Black is already at its target).
pub(crate) fn unavailable_reason(state: &StudioUiState, capability: Capability) -> &'static str {
    match state.connection_status {
        StudioConnectionStatus::Ready => {}
        StudioConnectionStatus::ProtocolMismatch => {
            return "the daemon did not select the current protocol";
        }
        StudioConnectionStatus::Failed => return "the control connection failed",
        StudioConnectionStatus::Disconnected => return "there is no control connection",
        StudioConnectionStatus::Launching
        | StudioConnectionStatus::Connecting
        | StudioConnectionStatus::Synchronizing
        | StudioConnectionStatus::Backoff => return "the control connection is not ready yet",
    }
    if state.view.is_none() {
        return "project state is not synchronized yet";
    }
    let permitted = match capability {
        Capability::SelectPreview => state.can_select_preview,
        Capability::Transition => state.can_transition,
        Capability::Audio => state.can_control_audio,
        Capability::EditProject => state.can_edit_project,
    };
    if !permitted {
        return match capability {
            Capability::SelectPreview => "this session lacks the select_preview permission",
            Capability::Transition => "this session lacks the transition permission",
            Capability::Audio => "this session lacks the control_audio permission",
            Capability::EditProject => "this session lacks the edit_project permission",
        };
    }
    if matches!(
        capability,
        Capability::SelectPreview | Capability::Transition
    ) && TransitionGate::from_state(state).manual_transition_in_flight
    {
        return "a manual T-bar transition is in progress";
    }
    "the current state does not allow it"
}

/// Formats the disabled tooltip for one action.
pub(crate) fn disabled_text(action: &str, reason: &str) -> String {
    format!("{action}\nUnavailable: {reason}")
}

/// Tooltip helpers shared by every gated control.
pub(crate) trait Hint {
    /// Shows `action` while enabled and `action` plus `reason` while disabled.
    fn hint(self, action: &str, reason: &str) -> Self;
}

impl Hint for Response {
    fn hint(self, action: &str, reason: &str) -> Self {
        self.on_hover_text(action)
            .on_disabled_hover_text(disabled_text(action, reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_follow_connection_then_view_then_permission() {
        let state = StudioUiState::new(StudioConnectionStatus::Connecting);
        assert_eq!(
            unavailable_reason(&state, Capability::Transition),
            "the control connection is not ready yet"
        );
        let state = StudioUiState::new(StudioConnectionStatus::ProtocolMismatch);
        assert!(unavailable_reason(&state, Capability::Audio).contains("protocol"));
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert_eq!(
            unavailable_reason(&state, Capability::EditProject),
            "project state is not synchronized yet"
        );
    }

    #[test]
    fn disabled_text_names_action_and_reason() {
        assert_eq!(
            disabled_text("Cut  [C]", "no permission"),
            "Cut  [C]\nUnavailable: no permission"
        );
    }
}
