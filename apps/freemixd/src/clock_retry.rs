//! The retry policy the daemon's clocks share for a refused advance.
//!
//! The playlist clock ([`crate::playlist_clock`]), the cue clock
//! ([`crate::cue_clock`], which also runs the waiting script) and the media
//! end-trigger feed ([`crate::media_clock`]) each submit an engine command
//! no client sends. The show may refuse one for a reason its own frames
//! clear (a held playlist take or cue frame, a realization still settling, a
//! full scheduler, a journal that could not record it); without a retry the
//! clock would stall until some other command happened to be accepted.
//!
//! A refusal [`retries`] allows is retried on later show frames, without any
//! other command: on every frame for the first [`FAST_RETRIES`], then every
//! [`SLOW_RETRY_FRAMES`]. A refusal that cannot clear by itself waits for a
//! new revision, as does every refusal of the same advance once the show
//! changes. A retry:
//!
//! - runs only on a pass that leaves room for it (no session command ready,
//!   the journal idle and, under native pacing, enough of the frame interval
//!   left for its journal append, as a client command waits for), so it
//!   never takes a client's frame or command turn nor makes a frame late;
//! - is prepared and dropped first ([`probe_refusal`]), so while the engine
//!   still refuses it nothing is executed: no receipt, no journal entry, no
//!   log line;
//! - carries its own idempotency key ([`retry_command`]), since the first
//!   attempt's refusal may hold a receipt under the base key;
//! - when accepted, is journaled like any late advance, so replay reproduces
//!   it exactly.

use fm_command::RejectionCode;
use fm_control::{ControlService, PrepareSubmitOutcome};
use fm_protocol::{CommandMessage, CommandResult};

use super::{AppResult, Policy, development_principal};

/// Refused attempts a clock retries on the very next show frame before it
/// backs off to [`SLOW_RETRY_FRAMES`].
pub(super) const FAST_RETRIES: u32 = 25;

/// Show frames between retries once [`FAST_RETRIES`] are spent.
pub(super) const SLOW_RETRY_FRAMES: u64 = 25;

/// Whether a refused advance may clear without another command, so the
/// clock retries it: the refusal says so, or it is a conflict with runtime
/// state (a held playlist take or cue frame, a realization still settling)
/// or a full scheduler, which the show's own frames resolve. Anything else
/// (a refusal the same show would only repeat, such as a result that cannot
/// be persisted) waits for another accepted command to change the show.
pub(super) fn retries(code: &str, retryable: bool) -> bool {
    retryable
        || code == RejectionCode::Conflict.as_str()
        || code == RejectionCode::ResourceExhausted.as_str()
}

/// The show frames after `attempts` refusals until the next retry: every
/// frame for the first [`FAST_RETRIES`], then every [`SLOW_RETRY_FRAMES`].
pub(super) const fn retry_delay(attempts: u32) -> u64 {
    if attempts <= FAST_RETRIES {
        1
    } else {
        SLOW_RETRY_FRAMES
    }
}

/// `command` as retry `retry` of a refused advance: a retry carries its own
/// idempotency key (`<key>-retry-<retry>`), since the first attempt's
/// refusal may hold a receipt under the first key, which would otherwise
/// answer every retry with the same refusal. Retry `0` is `command` itself.
pub(super) fn retry_command(mut command: CommandMessage, retry: u32) -> CommandMessage {
    if retry > 0 {
        let key = format!("{}-retry-{retry}", command.idempotency_key);
        command.id.clone_from(&key);
        command.idempotency_key = key;
    }
    command
}

/// Prepares `command` and drops it: returns the refusal the engine still
/// gives, observed without executing it (no receipt, journal entry or log
/// line), or `None` when it would now be accepted (or fails in the engine,
/// which its execution then classifies).
///
/// # Errors
///
/// Returns a failure to name the clock's principal.
pub(super) fn probe_refusal(
    control: &mut ControlService<Policy>,
    command: CommandMessage,
    now_millis: u64,
) -> AppResult<Option<CommandResult>> {
    let refused = match control.prepare_submit(&development_principal()?, command, now_millis) {
        Ok(PrepareSubmitOutcome::Prepared(prepared)) => {
            let result = &prepared.output().result;
            matches!(result, CommandResult::Rejected { .. }).then(|| result.clone())
        }
        Ok(PrepareSubmitOutcome::Replayed(submission)) => {
            let result = submission.output.result;
            matches!(result, CommandResult::Rejected { .. }).then_some(result)
        }
        // An engine failure: the execution classifies it.
        Err(_) => None,
    };
    Ok(refused)
}

/// A clock's refused advance, while the show still waits on it: the
/// advance's `key` (which list, which airing) at `revision`. Another
/// accepted command (a new revision) or a different key drops it, so the
/// next advance is a first attempt again.
#[derive(Debug)]
pub(super) struct Retries<K> {
    refused: Option<Refusal<K>>,
}

impl<K> Default for Retries<K> {
    fn default() -> Self {
        Self { refused: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Refusal<K> {
    key: K,
    revision: u64,
    /// Refused attempts so far, the first included.
    attempts: u32,
    /// The show frame from which the next retry may run; `None` for a
    /// refusal that cannot clear by itself, which waits for a new revision.
    retry_at: Option<u64>,
}

/// What a clock logs for an observed outcome (see [`Retries::observe`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome<'a> {
    /// Nothing to log: an accepted first attempt, or a retry still refused.
    Quiet,
    /// A first attempt refused, or a retry meeting a refusal that cannot
    /// clear by itself; `retrying` is whether the clock retries it.
    Refused {
        code: &'a str,
        message: &'a str,
        retrying: bool,
    },
    /// A retry accepted after `refusals` refused attempts.
    Advanced { refusals: u32 },
}

impl<K: PartialEq> Retries<K> {
    /// Forgets any refusal: nothing is pending any more.
    pub(super) fn clear(&mut self) {
        self.refused = None;
    }

    /// The attempt the advance `key` at `revision` may make when the show
    /// has reached frame `reached`: `Some(0)` for a first attempt,
    /// `Some(n)` for retry `n` once its backoff has passed, `None` while a
    /// refusal of it still stands.
    pub(super) fn attempt(&mut self, key: &K, revision: u64, reached: u64) -> Option<u32> {
        match &self.refused {
            Some(refused) if refused.key == *key && refused.revision == revision => {
                match refused.retry_at {
                    Some(at) if reached >= at => Some(refused.attempts),
                    _ => None,
                }
            }
            _ => {
                self.refused = None;
                Some(0)
            }
        }
    }

    /// Records the outcome of attempt `retry` of the advance `key` at
    /// `revision`, made when the show had reached frame `reached`. A
    /// refusal is retried after [`retry_delay`] when [`retries`] allows; the
    /// outcome says what the clock logs: a refusal once (on the first
    /// attempt, or when a retry meets one that cannot clear), and an
    /// accepted retry with how many refusals it took.
    pub(super) fn observe<'r>(
        &mut self,
        key: K,
        revision: u64,
        retry: u32,
        reached: u64,
        result: &'r CommandResult,
    ) -> Outcome<'r> {
        self.observe_with(key, revision, retry, reached, result, true)
    }

    /// [`Self::observe`], where `clears` is the clock's own word on whether
    /// a refusal could clear: `false` when the show's state says the advance
    /// cannot succeed whatever the refusal's code (an end trigger's
    /// `playlist_control next` with no next entry, which the engine reports
    /// as an ordinary conflict), making it final like any refusal
    /// [`retries`] does not allow.
    pub(super) fn observe_with<'r>(
        &mut self,
        key: K,
        revision: u64,
        retry: u32,
        reached: u64,
        result: &'r CommandResult,
        clears: bool,
    ) -> Outcome<'r> {
        match result {
            CommandResult::Rejected {
                code,
                message,
                retryable,
                ..
            } => {
                let retrying = clears && retries(code, *retryable);
                let attempts = retry.saturating_add(1);
                self.refused = Some(Refusal {
                    key,
                    revision,
                    attempts,
                    retry_at: retrying.then(|| reached.saturating_add(retry_delay(attempts))),
                });
                if retry == 0 || !retrying {
                    Outcome::Refused {
                        code,
                        message,
                        retrying,
                    }
                } else {
                    Outcome::Quiet
                }
            }
            CommandResult::Accepted { .. } => {
                self.refused = None;
                if retry > 0 {
                    Outcome::Advanced { refusals: retry }
                } else {
                    Outcome::Quiet
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fm_protocol::CURRENT_PROTOCOL_VERSION;

    use super::*;

    fn refusal(code: &str, retryable: bool) -> CommandResult {
        CommandResult::Rejected {
            id: "advance".into(),
            code: code.into(),
            message: "refused".into(),
            fields: Vec::new(),
            current_revision: 0,
            retryable,
            reason: None,
        }
    }

    #[test]
    fn retry_policy_retries_conflicts_and_retryable_refusals_only() {
        for code in [RejectionCode::Conflict, RejectionCode::ResourceExhausted] {
            assert!(retries(code.as_str(), false), "{code:?}");
        }
        // A journal that could not record the advance says it is retryable.
        assert!(retries(RejectionCode::Unavailable.as_str(), true));
        for code in [
            RejectionCode::Unavailable,
            RejectionCode::InvalidCommand,
            RejectionCode::PermissionDenied,
        ] {
            assert!(!retries(code.as_str(), false), "{code:?}");
        }
    }

    #[test]
    fn retries_run_every_frame_then_back_off() {
        for attempts in 1..=FAST_RETRIES {
            assert_eq!(retry_delay(attempts), 1, "attempt {attempts}");
        }
        for attempts in [FAST_RETRIES + 1, FAST_RETRIES + 2, u32::MAX] {
            assert_eq!(
                retry_delay(attempts),
                SLOW_RETRY_FRAMES,
                "attempt {attempts}"
            );
        }
        // Over the first second at 25 fps every frame is tried, and after
        // that one retry a second: a refusal that lasts a minute costs
        // 25 + 59 probes, not 1,500.
        let mut frame = 0;
        let mut probes = 0_u32;
        while frame < 25 * 60 {
            probes += 1;
            frame += retry_delay(probes);
        }
        assert_eq!(probes, 25 + 59);
    }

    #[test]
    fn a_refusal_the_clock_knows_cannot_clear_is_final_whatever_its_code() {
        let mut retries = Retries::<u64>::default();
        let conflict = refusal("conflict", false);
        assert_eq!(
            retries.observe_with(7, 3, 0, 40, &conflict, false),
            Outcome::Refused {
                code: "conflict",
                message: "refused",
                retrying: false
            }
        );
        assert_eq!(retries.attempt(&7, 3, u64::MAX), None);
        // A retry meeting it is logged as final too.
        assert_eq!(retries.attempt(&7, 4, 41), Some(0));
        assert!(matches!(
            retries.observe_with(7, 4, 0, 41, &conflict, true),
            Outcome::Refused { retrying: true, .. }
        ));
        assert_eq!(retries.attempt(&7, 4, 42), Some(1));
        assert!(matches!(
            retries.observe_with(7, 4, 1, 42, &conflict, false),
            Outcome::Refused {
                retrying: false,
                ..
            }
        ));
        assert_eq!(retries.attempt(&7, 4, u64::MAX), None);
    }

    #[test]
    fn a_retry_carries_its_own_idempotency_key() {
        let command = CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: "clock-3-40".into(),
            idempotency_key: "clock-3-40".into(),
            expected_revision: Some(3),
            deadline_ms: None,
            payload: fm_protocol::CommandPayload::PlaylistAdvance { frame: 40 },
        };
        assert_eq!(retry_command(command.clone(), 0), command);
        let retry = retry_command(command.clone(), 2);
        assert_eq!(retry.idempotency_key, "clock-3-40-retry-2");
        assert_eq!(retry.id, retry.idempotency_key);
        assert_eq!(retry.expected_revision, Some(3));
        assert_eq!(retry.payload, command.payload);
    }

    #[test]
    fn retries_follow_the_backoff_and_a_final_refusal_waits_for_a_new_revision() {
        let mut retries = Retries::<u64>::default();
        assert_eq!(retries.attempt(&7, 3, 40), Some(0));
        // Final: logged, then nothing at this revision however late.
        assert_eq!(
            retries.observe(7, 3, 0, 40, &refusal("invalid_command", false)),
            Outcome::Refused {
                code: "invalid_command",
                message: "refused",
                retrying: false
            }
        );
        assert_eq!(retries.attempt(&7, 3, u64::MAX), None);
        // A new revision, or another key, is a first attempt again.
        assert_eq!(retries.attempt(&7, 4, 41), Some(0));
        let refused = refusal("conflict", false);
        assert!(matches!(
            retries.observe(7, 4, 0, 41, &refused),
            Outcome::Refused { retrying: true, .. }
        ));
        assert_eq!(retries.attempt(&7, 4, 41), None, "not on the same frame");
        let mut frame = 41;
        for retry in 1..=FAST_RETRIES + 2 {
            let delay = if retry <= FAST_RETRIES {
                1
            } else {
                SLOW_RETRY_FRAMES
            };
            assert_eq!(retries.attempt(&7, 4, frame + delay - 1), None, "early");
            frame += delay;
            assert_eq!(retries.attempt(&7, 4, frame), Some(retry), "retry {retry}");
            // A still-refused retry is quiet.
            assert_eq!(
                retries.observe(7, 4, retry, frame, &refused),
                Outcome::Quiet
            );
        }
        let accepted = CommandResult::Accepted {
            id: "advance".into(),
            revision: 5,
            scheduled_frame: None,
        };
        let retry = FAST_RETRIES + 3;
        assert_eq!(
            retries.observe(7, 4, retry, frame + SLOW_RETRY_FRAMES, &accepted),
            Outcome::Advanced { refusals: retry }
        );
        assert_eq!(retries.attempt(&7, 4, frame), Some(0));
        assert_eq!(retries.attempt(&8, 4, frame), Some(0));
    }
}
