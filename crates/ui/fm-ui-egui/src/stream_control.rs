//! Per-target stream start/stop gating.
//!
//! Stream targets are started and stopped with the existing `stream_start` /
//! `stream_stop` protocol commands, which the daemon admits under the
//! `transition` permission. Studio offers exactly one action per target, chosen
//! from the replicated desired-running flag, and blocks a second action on the
//! same target while an earlier stream command for it is still unresolved.

use fm_protocol::WireStreamTargetId;
use fm_ui_model::StreamStatus;

use crate::StudioUiState;
use crate::gating::{Capability, unavailable_reason};
use crate::l10n::{Arg, Locale, Msg};

/// Stream operator actions emitted by the Streams rows and the Output menu.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamAction {
    /// Starts the addressed configured stream target.
    Start(WireStreamTargetId),
    /// Stops the addressed configured stream target.
    Stop(WireStreamTargetId),
}

impl StreamAction {
    /// The stream target this action addresses.
    #[must_use]
    pub const fn target(self) -> WireStreamTargetId {
        match self {
            Self::Start(target) | Self::Stop(target) => target,
        }
    }

    /// Whether this action starts (`true`) or stops the addressed target.
    #[must_use]
    pub const fn starts(self) -> bool {
        matches!(self, Self::Start(_))
    }

    /// The single action offered for one roster entry: Stop while the
    /// authoritative desired flag is running, otherwise Start.
    #[must_use]
    pub const fn for_stream(stream: &StreamStatus) -> Self {
        let target = WireStreamTargetId::new(stream.target);
        if stream.desired_running {
            Self::Stop(target)
        } else {
            Self::Start(target)
        }
    }
}

/// Whether an unresolved stream command addresses `target`.
#[must_use]
pub fn stream_command_pending(state: &StudioUiState, target: WireStreamTargetId) -> bool {
    state.stream_pending.contains(&target)
}

/// Validates one requested stream action against the session gates, the
/// replicated roster, and the addressed target's authoritative desired flag,
/// returning it only when the current state permits exactly that action.
#[must_use]
pub fn stream_action(state: &StudioUiState, requested: StreamAction) -> Option<StreamAction> {
    if !(state.connection_status.controls_enabled() && state.view.is_some() && state.can_transition)
    {
        return None;
    }
    let target = requested.target();
    if stream_command_pending(state, target) {
        return None;
    }
    let stream = state
        .streams
        .iter()
        .find(|stream| stream.target == target.get())?;
    (StreamAction::for_stream(stream) == requested).then_some(requested)
}

/// The first reason the offered action for `stream` is unavailable.
pub(crate) fn stream_unavailable_reason(
    state: &StudioUiState,
    stream: &StreamStatus,
) -> &'static str {
    let session_ok =
        state.connection_status.controls_enabled() && state.view.is_some() && state.can_transition;
    if session_ok && stream_command_pending(state, WireStreamTargetId::new(stream.target)) {
        Msg::ReasonStreamPending.en()
    } else {
        unavailable_reason(state, Capability::Transition)
    }
}

/// Operator-facing description of one stream action.
pub(crate) fn stream_action_text(l: Locale, action: StreamAction, name: &str) -> String {
    l.format(
        if action.starts() {
            Msg::StartStreamTarget
        } else {
            Msg::StopStreamTarget
        },
        &[("name", Arg::Str(name))],
    )
}
