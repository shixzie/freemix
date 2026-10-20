//! Operator-facing explanations for disabled controls.
//!
//! Every gated control shows its action (and shortcut) when enabled and the
//! first failing gate when disabled, so a greyed-out key is never a mystery.

use egui::Response;

use crate::l10n::{self, Msg};
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
/// or timed transition in flight. Falls back to a state-specific explanation when every
/// session gate passes (for example Fade-to-Black is already at its target).
pub(crate) fn unavailable_reason(state: &StudioUiState, capability: Capability) -> &'static str {
    match state.connection_status {
        StudioConnectionStatus::Ready => {}
        StudioConnectionStatus::ProtocolMismatch => {
            return Msg::ReasonProtocol.en();
        }
        StudioConnectionStatus::Failed => return Msg::ReasonConnectionFailed.en(),
        StudioConnectionStatus::Disconnected => return Msg::ReasonNoConnection.en(),
        StudioConnectionStatus::Launching
        | StudioConnectionStatus::Connecting
        | StudioConnectionStatus::Synchronizing
        | StudioConnectionStatus::Backoff => return Msg::ReasonNotReady.en(),
    }
    if state.view.is_none() {
        return Msg::ReasonNotSynchronized.en();
    }
    let permitted = match capability {
        Capability::SelectPreview => state.can_select_preview,
        Capability::Transition => state.can_transition,
        Capability::Audio => state.can_control_audio,
        Capability::EditProject => state.can_edit_project,
    };
    if !permitted {
        return match capability {
            Capability::SelectPreview => Msg::ReasonNoSelectPreview.en(),
            Capability::Transition => Msg::ReasonNoTransition.en(),
            Capability::Audio => Msg::ReasonNoControlAudio.en(),
            Capability::EditProject => Msg::ReasonNoEditProject.en(),
        };
    }
    if matches!(
        capability,
        Capability::SelectPreview | Capability::Transition
    ) {
        let gate = TransitionGate::from_state(state);
        if gate.manual_transition_in_flight {
            return Msg::ReasonManualInFlight.en();
        }
        if gate.transition_in_flight {
            return Msg::ReasonTransitionInFlight.en();
        }
    }
    Msg::ReasonStateDisallows.en()
}

/// Formats the disabled tooltip for one action in English; drawing code
/// uses [`l10n::disabled_text`] with the frame's locale.
#[cfg(test)]
pub(crate) fn disabled_text(action: &str, reason: &str) -> String {
    l10n::disabled_text(l10n::Locale::ENGLISH, action, reason)
}

/// Tooltip helpers shared by every gated control.
pub(crate) trait Hint {
    /// Shows `action` while enabled and `action` plus `reason` while disabled.
    fn hint(self, action: &str, reason: &str) -> Self;

    /// [`Self::hint`] worded in `locale` instead of the frame's locale, for
    /// widgets that callers can pin to a language.
    fn hint_in(self, locale: l10n::Locale, action: &str, reason: &str) -> Self;
}

impl Hint for Response {
    fn hint(self, action: &str, reason: &str) -> Self {
        let locale = l10n::locale(&self.ctx);
        self.hint_in(locale, action, reason)
    }

    fn hint_in(self, locale: l10n::Locale, action: &str, reason: &str) -> Self {
        self.on_hover_text(action)
            .on_disabled_hover_text(l10n::disabled_text(locale, action, reason))
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
