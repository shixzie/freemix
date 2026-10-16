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
/// Most refused command ids remembered as already announced.
const SEEN_REFUSALS: usize = 64;
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
    /// Command ids of the refusals already announced, oldest first.
    seen_refusals: VecDeque<String>,
    /// Command ids whose refusal another toast (the input catalogue's)
    /// already words.
    acknowledged_refusals: VecDeque<String>,
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

    /// Marks the refusal of `command_id` as already worded (by the input
    /// catalogue's own refusal toast); error text joined onto it is still
    /// announced.
    pub(crate) fn acknowledge_refusal(&mut self, command_id: Option<&str>) {
        if let Some(command_id) = command_id {
            remember(&mut self.acknowledged_refusals, command_id);
        }
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
        self.observe_refusals(state, ready);
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

    /// Announces each new command refusal with its own text, in arrival
    /// order: worded in the console language when it can be, else the
    /// worker's English line. The refusal whose line the current error
    /// holds also carries whatever else that error says, and the error is
    /// then announced.
    fn observe_refusals(&mut self, state: &StudioUiState, ready: bool) {
        let error = state.error.as_deref();
        let error_is_new = self.seen_error.as_deref() != error;
        let mut explained = false;
        for refusal in &state.command_refusals {
            if self.seen_refusals.contains(&refusal.command_id) {
                continue;
            }
            remember(&mut self.seen_refusals, &refusal.command_id);
            let remainder = error.and_then(|error| refusal.remainder_of(error));
            explained |= remainder.is_some();
            if !ready {
                continue;
            }
            let own = (!self.acknowledged_refusals.contains(&refusal.command_id))
                .then(|| refusal.toast_text(self.locale, state.view.as_ref()));
            let rest = remainder.flatten().filter(|_| error_is_new);
            let message = match (own, rest) {
                (Some(own), Some(rest)) => Some(format!("{own}; {rest}")),
                (own, rest) => own.or(rest),
            };
            if let Some(message) = message {
                self.push(ToastKind::Error, message);
            }
        }
        if explained {
            self.seen_error.clone_from(&state.error);
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

/// Remembers `id` in a bounded most-recent list.
fn remember(seen: &mut VecDeque<String>, id: &str) {
    if seen.iter().any(|seen| seen == id) {
        return;
    }
    if seen.len() == SEEN_REFUSALS {
        seen.pop_front();
    }
    seen.push_back(id.to_owned());
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

    /// The notice of a rejected command, as the worker builds it.
    fn refused(
        id: &str,
        code: &str,
        message: &str,
        issue: Option<&str>,
        reason: Option<fm_protocol::RefusalReason>,
    ) -> crate::CommandRefusalNotice {
        crate::CommandRefusalNotice::from_result(&fm_protocol::CommandResult::Rejected {
            id: id.to_owned(),
            code: code.to_owned(),
            message: message.to_owned(),
            fields: issue
                .map(|issue| fm_protocol::FieldIssue {
                    field: "url".to_owned(),
                    code: issue.to_owned(),
                    message: message.to_owned(),
                })
                .into_iter()
                .collect(),
            current_revision: 1,
            retryable: false,
            reason,
        })
        .unwrap()
    }

    fn scene_layer_refusal(id: &str) -> crate::CommandRefusalNotice {
        use fm_protocol::{InputUse, RefusalReason};
        refused(
            id,
            "conflict",
            "input 1 cannot be removed: it is a layer source of scene 100",
            None,
            Some(RefusalReason::InputInUse {
                input: 1,
                usage: InputUse::SceneLayer { scene: 100 },
            }),
        )
    }

    const SCENE_LAYER_ES: &str = "Comando rechazado (input_in_use): «Entrada 1» es una fuente \
                                  de capa de la escena 100. Quite esa capa de la escena primero.";

    fn spanish_queue() -> ToastQueue {
        ToastQueue {
            locale: Locale::new(fm_ui_model::i18n::Language::Spanish),
            ..ToastQueue::default()
        }
    }

    #[test]
    fn typed_refusals_toast_in_the_console_language_instead_of_daemon_english() {
        let mut queue = spanish_queue();
        let mut state = state();
        let refusal = scene_layer_refusal("c-1");
        state.error = Some(refusal.error.clone());
        state.command_refusals = vec![refusal];
        queue.observe(&state);
        queue.observe(&state);
        let toasts = queue.iter().collect::<Vec<_>>();
        assert_eq!(toasts.len(), 1, "{toasts:?}");
        assert_eq!(toasts[0].kind, ToastKind::Error);
        assert_eq!(toasts[0].message, SCENE_LAYER_ES);
        // The status bar words it the same way.
        let (status, _) = crate::chrome::status_message(queue.locale, &state);
        assert!(status.contains("es una fuente de capa"), "{status}");
        assert!(!status.contains("layer source"), "{status}");

        // A refusal without a typed reason falls back to the daemon's text.
        let boom = refused("c-2", "internal", "boom", None, None);
        state.error = Some(boom.error.clone());
        state.command_refusals = vec![boom];
        queue.observe(&state);
        assert_eq!(queue.iter().count(), 2);
        assert_eq!(
            queue.iter().last().unwrap().message,
            "Command rejected (internal): boom"
        );
    }

    #[test]
    fn two_refusals_before_one_frame_each_show_their_own_text() {
        // The worker published a typed refusal and then an untyped one
        // before the UI's next frame: the mailbox queues both, and the
        // error is the second's.
        let mut queue = spanish_queue();
        let mut state = state();
        let first = scene_layer_refusal("c-1");
        let second = refused("c-2", "internal", "disk full", None, None);
        state.error = Some(second.error.clone());
        state.command_refusals = vec![first, second];
        queue.observe(&state);
        let messages = queue
            .iter()
            .map(|toast| toast.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            [SCENE_LAYER_ES, "Command rejected (internal): disk full"]
        );
        // The status bar shows the current (second) error, not the first
        // refusal's wording.
        let (status, _) = crate::chrome::status_message(queue.locale, &state);
        assert!(status.contains("disk full"), "{status}");
        assert!(!status.contains("fuente de capa"), "{status}");

        // Two typed refusals: both worded, in order.
        let mut queue = spanish_queue();
        let third = scene_layer_refusal("c-3");
        let fourth = refused(
            "c-4",
            "conflict",
            "too many grades",
            None,
            Some(fm_protocol::RefusalReason::bounds("grades", 64)),
        );
        state.error = Some(fourth.error.clone());
        state.command_refusals = vec![third, fourth];
        queue.observe(&state);
        let messages = queue
            .iter()
            .map(|toast| toast.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            [
                SCENE_LAYER_ES,
                "Comando rechazado (bounds_exceeded): Correcciones de color: el límite es 64."
            ]
        );
    }

    #[test]
    fn a_typed_refusal_keeps_the_text_joined_onto_its_error() {
        let mut queue = spanish_queue();
        let mut state = state();
        let refusal = scene_layer_refusal("c-1");
        state.error = Some(format!(
            "{}; Recording to /media/a failed; Rejected 2 command(s): Studio deferred command \
             queue reached capacity 64",
            refusal.error
        ));
        state.command_refusals = vec![refusal];
        queue.observe(&state);
        queue.observe(&state);
        let toasts = queue.iter().collect::<Vec<_>>();
        assert_eq!(toasts.len(), 1, "{toasts:?}");
        assert_eq!(
            toasts[0].message,
            format!(
                "{SCENE_LAYER_ES}; Recording to /media/a failed; Rejected 2 command(s): Studio \
                 deferred command queue reached capacity 64"
            )
        );
        let (status, _) = crate::chrome::status_message(queue.locale, &state);
        assert!(status.contains("es una fuente de capa"), "{status}");
        assert!(status.contains("Recording to /media/a failed"), "{status}");
        assert!(status.contains("Rejected 2 command(s)"), "{status}");
        assert!(!status.contains("layer source"), "{status}");
    }

    #[test]
    fn an_acknowledged_refusal_still_shows_the_rest_of_its_error() {
        let mut queue = spanish_queue();
        let mut state = state();
        let refusal = scene_layer_refusal("c-1");
        state.error = Some(format!("{}; Recording to /media/a failed", refusal.error));
        state.command_refusals = vec![refusal];
        queue.acknowledge_refusal(Some("c-1"));
        queue.observe(&state);
        let messages = queue
            .iter()
            .map(|toast| toast.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, ["Recording to /media/a failed"]);
    }

    #[test]
    fn an_unsupported_scheme_is_named_in_the_console_language() {
        let mut queue = spanish_queue();
        let mut state = state();
        let refusal = refused(
            "c-1",
            "invalid_command",
            "unsupported scheme (allowed: rtsp, rtp, udp, tcp, http, https, hls)",
            Some("unsupported_scheme"),
            None,
        );
        state.error = Some(refusal.error.clone());
        state.command_refusals = vec![refusal.clone()];
        queue.observe(&state);
        assert_eq!(
            queue.iter().next().unwrap().message,
            "Comando rechazado (unsupported_scheme): esquema no admitido (admitidos: rtsp, rtp, \
             udp, tcp, http, https, hls)"
        );
        assert_eq!(
            refusal.text(Locale::ENGLISH, None),
            "Command refused (unsupported_scheme): unsupported scheme (allowed: rtsp, rtp, udp, \
             tcp, http, https, hls)"
        );
    }

    #[test]
    fn a_refusal_line_is_found_anywhere_in_a_joined_error() {
        let refusal = refused("c-1", "internal", "a; b", None, None);
        let line = refusal.error.clone();
        assert_eq!(refusal.remainder_of(&line), Some(None));
        assert_eq!(
            refusal.remainder_of(&format!("{line}; tail")),
            Some(Some("tail".to_owned()))
        );
        assert_eq!(
            refusal.remainder_of(&format!("head; {line}; tail")),
            Some(Some("head; tail".to_owned()))
        );
        assert_eq!(refusal.remainder_of("something else"), None);
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
