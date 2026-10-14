//! Transient toast notifications for command rejections, notices, and
//! terminally uncertain commands.
//!
//! Toasts announce a message when it first appears in the replicated UI
//! state; the status bar keeps showing the latest message after a toast
//! expires. The queue is bounded and ages only by the frame time passed in.

use std::collections::VecDeque;

use crate::l10n::{Arg, Locale, Msg};
use crate::{StudioUiState, TerminalUncertaintyNotice};

/// Maximum toasts shown at once; the oldest is dropped first.
pub(crate) const TOAST_CAPACITY: usize = 4;
/// Lifetime of an informational notice.
pub(crate) const NOTICE_TOAST_MS: u32 = 5_000;
/// Lifetime of an error such as a command rejection.
pub(crate) const ERROR_TOAST_MS: u32 = 8_000;
/// Lifetime of a terminal-uncertainty notice.
pub(crate) const UNCERTAIN_TOAST_MS: u32 = 10_000;
/// Lifetime of a pending-command toast; its outcome normally replaces it
/// much sooner.
pub(crate) const PENDING_TOAST_MS: u32 = 20_000;
/// Lifetime of a confirmed-outcome toast.
pub(crate) const SUCCESS_TOAST_MS: u32 = 5_000;
/// Toast key shared by every "Queued N command(s)" notice, so rapid clicks
/// update one toast instead of stacking one per count.
pub(crate) const QUEUED_COMMANDS_KEY: &str = "queued-commands";

/// Severity of one toast.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToastKind {
    Notice,
    Error,
    Uncertain,
    /// A command was sent and its outcome is not known yet.
    Pending,
    /// A command's effect was confirmed by replicated state.
    Success,
}

impl ToastKind {
    const fn lifetime_ms(self) -> u32 {
        match self {
            Self::Notice => NOTICE_TOAST_MS,
            Self::Error => ERROR_TOAST_MS,
            Self::Uncertain => UNCERTAIN_TOAST_MS,
            Self::Pending => PENDING_TOAST_MS,
            Self::Success => SUCCESS_TOAST_MS,
        }
    }

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Notice => Msg::ToastNotice.en(),
            Self::Error => Msg::ToastError.en(),
            Self::Uncertain => Msg::ToastUncertain.en(),
            Self::Pending => Msg::Pending.en(),
            Self::Success => Msg::ToastDone.en(),
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
    /// Toasts with the same key replace each other in place.
    pub(crate) key: Option<String>,
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
    /// Language of the messages this queue words itself.
    pub(crate) locale: Locale,
}

impl ToastQueue {
    /// Pushes one toast, dropping the oldest when the queue is full.
    pub(crate) fn push(&mut self, kind: ToastKind, message: String) {
        self.push_keyed(None, kind, message);
    }

    /// Replaces the toast with `key` in place (new kind, message, and a full
    /// lifetime), or pushes a new keyed toast when none is visible.
    pub(crate) fn upsert(&mut self, key: &str, kind: ToastKind, message: String) {
        if let Some(toast) = self
            .toasts
            .iter_mut()
            .find(|toast| toast.key.as_deref() == Some(key))
        {
            let lifetime_ms = kind.lifetime_ms();
            toast.kind = kind;
            toast.message = message;
            toast.remaining_ms = lifetime_ms;
            toast.lifetime_ms = lifetime_ms;
            return;
        }
        self.push_keyed(Some(key.to_owned()), kind, message);
    }

    /// Marks the current error as already announced, so a message another
    /// toast already explains (a refused input add or removal) is not
    /// toasted twice.
    pub(crate) fn acknowledge_error(&mut self, error: Option<&String>) {
        self.seen_error = error.cloned();
    }

    fn push_keyed(&mut self, key: Option<String>, kind: ToastKind, message: String) {
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
            key,
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
            match notice_key(&notice) {
                Some(key) => self.upsert(key, ToastKind::Notice, notice),
                None => self.push(ToastKind::Notice, notice),
            }
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
                self.locale.format(
                    Msg::ToastUncertainCommand,
                    &[
                        ("command", Arg::Str(&notice.command_id)),
                        ("receipt", Arg::Str(&notice.received_command_id)),
                    ],
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

/// The coalescing key of a notice whose newer versions supersede older ones.
fn notice_key(notice: &str) -> Option<&'static str> {
    (notice.starts_with("Queued ") && notice.contains(" command(s)")).then_some(QUEUED_COMMANDS_KEY)
}

/// The operator-FIFO depth of the runtime's `Queued N command(s) in
/// operator FIFO` notice.
fn queued_commands(notice: &str) -> Option<u64> {
    notice
        .strip_prefix("Queued ")?
        .strip_suffix(" command(s) in operator FIFO")?
        .parse()
        .ok()
}

/// Words a runtime notice in the operator's language. The Studio runtime
/// publishes notices as English text; known ones are re-worded (with real
/// plurals), and any other text is shown as published.
pub(crate) fn localize_notice(locale: Locale, notice: &str) -> String {
    match queued_commands(notice) {
        Some(count) => locale.format(Msg::QueuedCommands, &[("count", Arg::Count(count))]),
        None => notice.to_owned(),
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
    fn queued_command_notices_update_one_toast() {
        let mut queue = ToastQueue::default();
        let mut state = state();
        for count in 1..=5 {
            state.notice = Some(format!("Queued {count} command(s) in operator FIFO"));
            queue.observe(&state);
            queue.tick(400);
        }
        let toasts: Vec<_> = queue.iter().collect();
        assert_eq!(toasts.len(), 1, "rapid clicks share one toast");
        assert_eq!(toasts[0].message, "Queued 5 command(s) in operator FIFO");
        // The latest count restarts the lifetime.
        assert_eq!(toasts[0].remaining_ms, NOTICE_TOAST_MS - 400);
        // Other notices still stack beside it.
        state.notice = Some("OSC malformed=1 rejected=0 overflow=0".to_owned());
        queue.observe(&state);
        assert_eq!(queue.iter().count(), 2);
        // Once the queued toast expires a new count raises a fresh one.
        queue.tick(NOTICE_TOAST_MS);
        state.notice = Some("Queued 1 command(s) in operator FIFO".to_owned());
        queue.observe(&state);
        assert_eq!(queue.iter().count(), 1);
    }

    #[test]
    fn keyed_toasts_are_replaced_in_place() {
        let mut queue = ToastQueue::default();
        queue.push(ToastKind::Notice, "first".to_owned());
        queue.upsert("add-7", ToastKind::Pending, "Adding Bars 1…".to_owned());
        queue.push(ToastKind::Notice, "last".to_owned());
        queue.tick(1_000);
        queue.upsert("add-7", ToastKind::Success, "Added Bars 1".to_owned());
        let toasts: Vec<_> = queue
            .iter()
            .map(|toast| (toast.kind, toast.message.as_str()))
            .collect();
        assert_eq!(
            toasts,
            [
                (ToastKind::Notice, "first"),
                (ToastKind::Success, "Added Bars 1"),
                (ToastKind::Notice, "last"),
            ]
        );
        let replaced = queue.iter().nth(1).unwrap();
        assert_eq!(replaced.remaining_ms, SUCCESS_TOAST_MS);
        assert_eq!(replaced.lifetime_ms, SUCCESS_TOAST_MS);
    }

    #[test]
    fn acknowledged_errors_are_not_toasted_again() {
        let mut queue = ToastQueue::default();
        let mut state = state();
        state.error = Some("Command rejected (conflict): in use".to_owned());
        queue.acknowledge_error(state.error.as_ref());
        queue.observe(&state);
        assert!(queue.is_empty());
    }

    #[test]
    fn queued_notices_are_reworded_with_plurals() {
        use fm_ui_model::i18n::Language;
        let spanish = Locale::new(Language::Spanish);
        let notice = |count: u32| format!("Queued {count} command(s) in operator FIFO");
        assert_eq!(
            localize_notice(Locale::ENGLISH, &notice(1)),
            "1 command queued in the operator FIFO"
        );
        assert_eq!(
            localize_notice(Locale::ENGLISH, &notice(1_200)),
            "1,200 commands queued in the operator FIFO"
        );
        assert_eq!(
            localize_notice(spanish, &notice(2)),
            "2 comandos en la cola del operador"
        );
        assert_eq!(
            localize_notice(spanish, "OSC receive failed"),
            "OSC receive failed"
        );
    }

    #[test]
    fn uncertainty_toasts_use_the_queue_locale() {
        let mut queue = ToastQueue {
            locale: Locale::new(fm_ui_model::i18n::Language::Spanish),
            ..ToastQueue::default()
        };
        let mut state = state();
        state.terminal_uncertainties = vec![TerminalUncertaintyNotice {
            command_id: "c1".to_owned(),
            received_command_id: "r1".to_owned(),
        }];
        queue.observe(&state);
        assert_eq!(
            queue.iter().next().unwrap().message,
            "El comando c1 pudo aplicarse o no (recibo del servidor r1). Revise Programa antes de reintentar."
        );
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
