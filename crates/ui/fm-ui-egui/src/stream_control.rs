//! Per-target and group stream start/stop/retry gating (OR-006).
//!
//! Stream targets are started and stopped with the `stream_start` /
//! `stream_stop` protocol commands, reconnected at once with
//! `stream_retry`, and started or stopped together with `stream_start_all` /
//! `stream_stop_all`; the daemon admits all of them under the `transition`
//! permission, and none of them affects recording. Studio offers exactly one
//! Start/Stop action per target, chosen from the replicated desired-running
//! flag, offers Retry only while the target is desired running, and blocks a
//! second action on the same target while an earlier stream command for it
//! is still unresolved.

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
    /// Reconnects the addressed desired-running target now instead of
    /// waiting for its next backoff (Protocol 2.52).
    Retry(WireStreamTargetId),
}

impl StreamAction {
    /// The stream target this action addresses.
    #[must_use]
    pub const fn target(self) -> WireStreamTargetId {
        match self {
            Self::Start(target) | Self::Stop(target) | Self::Retry(target) => target,
        }
    }

    /// Whether this action starts (`true`) the addressed target.
    #[must_use]
    pub const fn starts(self) -> bool {
        matches!(self, Self::Start(_))
    }

    /// The single Start/Stop action offered for one roster entry: Stop
    /// while the authoritative desired flag is running, otherwise Start.
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

/// Starts or stops every configured destination at once (Protocol 2.52).
/// Neither affects recording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamGroupAction {
    StartAll,
    StopAll,
}

/// Whether an unresolved stream command addresses `target`.
#[must_use]
pub fn stream_command_pending(state: &StudioUiState, target: WireStreamTargetId) -> bool {
    state.stream_pending.contains(&target)
}

/// Whether the session permits stream transport control at all.
fn session_ok(state: &StudioUiState) -> bool {
    state.connection_status.controls_enabled() && state.view.is_some() && state.can_transition
}

/// Validates one requested stream action against the session gates, the
/// replicated roster, and the addressed target's authoritative desired flag,
/// returning it only when the current state permits exactly that action.
#[must_use]
pub fn stream_action(state: &StudioUiState, requested: StreamAction) -> Option<StreamAction> {
    if !session_ok(state) {
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
    match requested {
        StreamAction::Retry(_) => stream.desired_running.then_some(requested),
        StreamAction::Start(_) | StreamAction::Stop(_) => {
            (StreamAction::for_stream(stream) == requested).then_some(requested)
        }
    }
}

/// Validates one group action: the session gates, no group command still
/// unresolved, and at least one destination it would change.
#[must_use]
pub fn stream_group_action(
    state: &StudioUiState,
    requested: StreamGroupAction,
) -> Option<StreamGroupAction> {
    group_unavailable_reason(state, requested)
        .is_none()
        .then_some(requested)
}

/// Why `requested` is unavailable, or `None` when it is available.
pub(crate) fn group_unavailable_reason(
    state: &StudioUiState,
    requested: StreamGroupAction,
) -> Option<&'static str> {
    if !session_ok(state) {
        return Some(unavailable_reason(state, Capability::Transition));
    }
    if state.streams.is_empty() {
        return Some(Msg::ReasonNoStreamTargets.en());
    }
    if state.stream_group_pending {
        return Some(Msg::ReasonStreamGroupPending.en());
    }
    let any_running = state.streams.iter().any(|stream| stream.desired_running);
    let any_stopped = state.streams.iter().any(|stream| !stream.desired_running);
    match requested {
        StreamGroupAction::StartAll if !any_stopped => Some(Msg::ReasonAllStreamsStarted.en()),
        StreamGroupAction::StopAll if !any_running => Some(Msg::ReasonNoStreamsStarted.en()),
        _ => None,
    }
}

/// The first reason the offered Start/Stop action for `stream` is
/// unavailable.
pub(crate) fn stream_unavailable_reason(
    state: &StudioUiState,
    stream: &StreamStatus,
) -> &'static str {
    if session_ok(state) && stream_command_pending(state, WireStreamTargetId::new(stream.target)) {
        Msg::ReasonStreamPending.en()
    } else {
        unavailable_reason(state, Capability::Transition)
    }
}

/// The first reason Retry for `stream` is unavailable.
pub(crate) fn retry_unavailable_reason(
    state: &StudioUiState,
    stream: &StreamStatus,
) -> &'static str {
    if !session_ok(state) {
        unavailable_reason(state, Capability::Transition)
    } else if stream_command_pending(state, WireStreamTargetId::new(stream.target)) {
        Msg::ReasonStreamPending.en()
    } else if !stream.desired_running {
        Msg::ReasonStreamNotStarted.en()
    } else {
        Msg::ReasonStateDisallows.en()
    }
}

/// Operator-facing description of one stream action.
pub(crate) fn stream_action_text(l: Locale, action: StreamAction, name: &str) -> String {
    l.format(
        match action {
            StreamAction::Start(_) => Msg::StartStreamTarget,
            StreamAction::Stop(_) => Msg::StopStreamTarget,
            StreamAction::Retry(_) => Msg::StreamRetryHover,
        },
        &[("name", Arg::Str(name))],
    )
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::StreamRealizedState;

    use super::*;

    fn target(value: u128) -> WireStreamTargetId {
        WireStreamTargetId::new(NonZeroU128::new(value).unwrap())
    }

    fn stream(value: u128, desired_running: bool) -> StreamStatus {
        StreamStatus {
            target: NonZeroU128::new(value).unwrap(),
            name: format!("Target {value}"),
            desired_running,
            realized: StreamRealizedState::Stopped,
            detail: None,
        }
    }

    fn state(roster: Vec<StreamStatus>) -> StudioUiState {
        let mut state = crate::stream_targets::tests::fixture(&[]);
        state.view.as_mut().unwrap().streams.clone_from(&roster);
        state.streams = roster;
        state
    }

    #[test]
    fn retry_is_offered_only_while_desired_running_and_not_pending() {
        let mut state = state(vec![stream(1, true), stream(2, false)]);
        assert_eq!(
            stream_action(&state, StreamAction::Retry(target(1))),
            Some(StreamAction::Retry(target(1)))
        );
        assert_eq!(stream_action(&state, StreamAction::Retry(target(2))), None);
        assert_eq!(
            retry_unavailable_reason(&state, &state.streams[1]),
            Msg::ReasonStreamNotStarted.en()
        );
        // Unknown targets are never offered.
        assert_eq!(stream_action(&state, StreamAction::Retry(target(3))), None);
        state.stream_pending = vec![target(1)];
        assert_eq!(stream_action(&state, StreamAction::Retry(target(1))), None);
        assert_eq!(
            retry_unavailable_reason(&state, &state.streams[0]),
            Msg::ReasonStreamPending.en()
        );
        state.stream_pending.clear();
        state.can_transition = false;
        assert_eq!(stream_action(&state, StreamAction::Retry(target(1))), None);
        assert_eq!(
            retry_unavailable_reason(&state, &state.streams[0]),
            Msg::ReasonNoTransition.en()
        );
        assert_eq!(StreamAction::Retry(target(4)).target(), target(4));
        assert!(!StreamAction::Retry(target(4)).starts());
        assert_eq!(
            stream_action_text(Locale::ENGLISH, StreamAction::Retry(target(1)), "Twitch"),
            "Reconnect \"Twitch\" now instead of waiting for its next retry"
        );
    }

    #[test]
    fn group_actions_need_something_to_change_and_no_group_pending() {
        let mixed = state(vec![stream(1, true), stream(2, false)]);
        for action in [StreamGroupAction::StartAll, StreamGroupAction::StopAll] {
            assert_eq!(stream_group_action(&mixed, action), Some(action));
        }
        let all_running = state(vec![stream(1, true), stream(2, true)]);
        assert_eq!(
            group_unavailable_reason(&all_running, StreamGroupAction::StartAll),
            Some(Msg::ReasonAllStreamsStarted.en())
        );
        assert!(stream_group_action(&all_running, StreamGroupAction::StopAll).is_some());
        let all_stopped = state(vec![stream(1, false)]);
        assert_eq!(
            group_unavailable_reason(&all_stopped, StreamGroupAction::StopAll),
            Some(Msg::ReasonNoStreamsStarted.en())
        );
        assert_eq!(
            group_unavailable_reason(&state(Vec::new()), StreamGroupAction::StartAll),
            Some(Msg::ReasonNoStreamTargets.en())
        );
        let mut pending = mixed.clone();
        pending.stream_group_pending = true;
        assert_eq!(
            group_unavailable_reason(&pending, StreamGroupAction::StartAll),
            Some(Msg::ReasonStreamGroupPending.en())
        );
        // A per-target pending command does not block the group.
        let mut target_pending = mixed.clone();
        target_pending.stream_pending = vec![target(1)];
        assert!(stream_group_action(&target_pending, StreamGroupAction::StopAll).is_some());
        let mut viewer = mixed;
        viewer.can_transition = false;
        assert_eq!(
            group_unavailable_reason(&viewer, StreamGroupAction::StopAll),
            Some(Msg::ReasonNoTransition.en())
        );
    }
}
