//! Transient toast notifications for command rejections, notices, and
//! terminally uncertain commands.
//!
//! Toasts announce a message when it first appears in the replicated UI
//! state; the status bar keeps showing the latest message after a toast
//! expires. The queue is bounded and ages only by the frame time passed in.

use std::collections::VecDeque;

use crate::{StudioUiState, TerminalUncertaintyNotice};

/// Maximum toasts shown at once; the oldest is dropped first.
pub(crate) const TOAST_CAPACITY: usize = 4;
/// Lifetime of an informational notice.
pub(crate) const NOTICE_TOAST_MS: u32 = 5_000;
/// Lifetime of an error such as a command rejection.
pub(crate) const ERROR_TOAST_MS: u32 = 8_000;
/// Lifetime of a terminal-uncertainty notice.
pub(crate) const UNCERTAIN_TOAST_MS: u32 = 10_000;

/// Severity of one toast.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToastKind {
    Notice,
    Error,
    Uncertain,
}

impl ToastKind {
    const fn lifetime_ms(self) -> u32 {
        match self {
            Self::Notice => NOTICE_TOAST_MS,
            Self::Error => ERROR_TOAST_MS,
            Self::Uncertain => UNCERTAIN_TOAST_MS,
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Notice => "NOTICE",
            Self::Error => "ERROR",
            Self::Uncertain => "OUTCOME UNCERTAIN",
        }
    }
}

/// One visible toast.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Toast {
    pub(crate) kind: ToastKind,
    pub(crate) message: String,
    pub(crate) remaining_ms: u32,
    pub(crate) lifetime_ms: u32,
    pub(crate) id: u64,
}

/// Bounded toast queue plus the messages already announced.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ToastQueue {
    toasts: VecDeque<Toast>,
    seen_error: Option<String>,
    seen_notice: Option<String>,
    seen_external_notice: Option<String>,
    seen_uncertainties: Vec<TerminalUncertaintyNotice>,
    next_id: u64,
}

impl ToastQueue {
    /// Pushes one toast, dropping the oldest when the queue is full.
    pub(crate) fn push(&mut self, kind: ToastKind, message: String) {
        if self.toasts.len() == TOAST_CAPACITY {
            self.toasts.pop_front();
        }
        let lifetime_ms = kind.lifetime_ms();
        self.toasts.push_back(Toast {
            kind,
            message,
            remaining_ms: lifetime_ms,
            lifetime_ms,
            id: self.next_id,
        });
        self.next_id = self.next_id.wrapping_add(1);
    }

    /// Announces messages that are new since the previous frame.
    ///
    /// A message that stays in the state is announced once; when it clears and
    /// later reappears it is announced again. Errors and notices are toasted
    /// only while the console is Ready: during connection loss the centered
    /// connection card and the status bar already carry them, and one toast
    /// per reconnect attempt would bury the console.
    pub(crate) fn observe(&mut self, state: &StudioUiState) {
        let ready = state.connection_status.controls_enabled();
        if let Some(error) = announce(&mut self.seen_error, state.error.as_ref())
            && ready
        {
            self.push(ToastKind::Error, error);
        }
        if let Some(notice) = announce(&mut self.seen_notice, state.notice.as_ref())
            && ready
        {
            self.push(ToastKind::Notice, notice);
        }
        if let Some(notice) = announce(
            &mut self.seen_external_notice,
            state.external_notice.as_ref(),
        ) && ready
        {
            self.push(ToastKind::Notice, notice);
        }
        let fresh: Vec<_> = state
            .terminal_uncertainties
            .iter()
            .filter(|notice| !self.seen_uncertainties.contains(notice))
            .cloned()
            .collect();
        self.seen_uncertainties
            .clone_from(&state.terminal_uncertainties);
        for notice in fresh {
            self.push(
                ToastKind::Uncertain,
                format!(
                    "Command {} may or may not have applied (server receipt {}). Check Program before retrying.",
                    notice.command_id, notice.received_command_id
                ),
            );
        }
    }

    /// Ages every toast by `dt_ms` and drops expired ones.
    pub(crate) fn tick(&mut self, dt_ms: u32) {
        for toast in &mut self.toasts {
            toast.remaining_ms = toast.remaining_ms.saturating_sub(dt_ms);
        }
        self.toasts.retain(|toast| toast.remaining_ms > 0);
    }

    /// Removes one toast by ID (operator dismissal).
    pub(crate) fn dismiss(&mut self, id: u64) {
        self.toasts.retain(|toast| toast.id != id);
    }

    /// Visible toasts, oldest first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.toasts.iter()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }
}

/// Returns the current message when it differs from the last announced one.
fn announce(seen: &mut Option<String>, current: Option<&String>) -> Option<String> {
    if seen.as_ref() == current {
        return None;
    }
    seen.clone_from(&current.cloned());
    current.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StudioConnectionStatus;

    fn state() -> StudioUiState {
        StudioUiState::new(StudioConnectionStatus::Ready)
    }

    #[test]
    fn new_messages_are_announced_once() {
        let mut queue = ToastQueue::default();
        let mut state = state();
        queue.observe(&state);
        assert!(queue.is_empty());
        state.error = Some("Command rejected (stale): revision moved".to_owned());
        queue.observe(&state);
        queue.observe(&state);
        assert_eq!(queue.iter().count(), 1);
        assert_eq!(queue.iter().next().unwrap().kind, ToastKind::Error);
        // Clearing and reappearing announces again.
        state.error = None;
        queue.observe(&state);
        state.error = Some("Command rejected (stale): revision moved".to_owned());
        queue.observe(&state);
        assert_eq!(queue.iter().count(), 2);
    }

    #[test]
    fn connection_errors_are_not_toasted_while_not_ready() {
        let mut queue = ToastQueue::default();
        let mut state = StudioUiState::new(StudioConnectionStatus::Backoff);
        state.error = Some("Reconnect failed; attempt 2".to_owned());
        queue.observe(&state);
        assert!(queue.is_empty());
        // The same message is not replayed once the console becomes Ready.
        state.connection_status = StudioConnectionStatus::Ready;
        queue.observe(&state);
        assert!(queue.is_empty());
        state.terminal_uncertainties = vec![TerminalUncertaintyNotice {
            command_id: "c".to_owned(),
            received_command_id: "r".to_owned(),
        }];
        state.connection_status = StudioConnectionStatus::Backoff;
        queue.observe(&state);
        assert_eq!(queue.iter().count(), 1);
    }

    #[test]
    fn uncertainties_are_announced_per_command() {
        let mut queue = ToastQueue::default();
        let mut state = state();
        let notice = |id: &str| TerminalUncertaintyNotice {
            command_id: id.to_owned(),
            received_command_id: format!("r-{id}"),
        };
        state.terminal_uncertainties = vec![notice("a")];
        queue.observe(&state);
        state.terminal_uncertainties = vec![notice("a"), notice("b")];
        queue.observe(&state);
        let toasts: Vec<_> = queue.iter().collect();
        assert_eq!(toasts.len(), 2);
        assert!(toasts[1].message.contains("Command b"));
        assert_eq!(toasts[1].kind, ToastKind::Uncertain);
    }

    #[test]
    fn queue_is_bounded_and_expires_by_frame_time() {
        let mut queue = ToastQueue::default();
        for index in 0..6 {
            queue.push(ToastKind::Notice, format!("n{index}"));
        }
        let messages: Vec<_> = queue.iter().map(|toast| toast.message.as_str()).collect();
        assert_eq!(messages, ["n2", "n3", "n4", "n5"]);
        queue.push(ToastKind::Error, "e".to_owned());
        queue.tick(NOTICE_TOAST_MS - 1);
        assert_eq!(queue.iter().count(), TOAST_CAPACITY);
        queue.tick(1);
        let remaining: Vec<_> = queue.iter().map(|toast| toast.kind).collect();
        assert_eq!(remaining, [ToastKind::Error]);
        queue.tick(ERROR_TOAST_MS);
        assert!(queue.is_empty());
    }

    #[test]
    fn dismissal_removes_one_toast() {
        let mut queue = ToastQueue::default();
        queue.push(ToastKind::Notice, "a".to_owned());
        queue.push(ToastKind::Notice, "b".to_owned());
        let first = queue.iter().next().unwrap().id;
        queue.dismiss(first);
        assert_eq!(
            queue
                .iter()
                .map(|toast| toast.message.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
    }
}
