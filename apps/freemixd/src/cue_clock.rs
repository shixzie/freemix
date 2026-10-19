//! The daemon's cue clock (RC-017).
//!
//! A cue's actions after a wait run on an exact engine frame, and only the
//! journaled `cue_advance` for that frame runs them. This clock decides when
//! to submit it, paced exactly like the playlist clock (see
//! [`crate::playlist_clock`]): by wall time anchored to the engine cursor on
//! the simulated daemon, by the native pacer's next frame otherwise. The
//! advance is journaled like any accepted command, so replay and restart
//! reproduce the exact frames. On the simulated daemon a client's cue
//! control first catches the engine up to the show's wall-clock frame
//! ([`CueClock::catch_up`]), like a playlist control.
//!
//! The same advance runs waiting cues, armed GOs and the waiting script run
//! (list `0`). One the show refuses for a reason its own frames can clear (a
//! held playlist take or cue frame, a realization still settling, a journal
//! that could not record it) is retried on later frames without any other
//! command: on every frame at first, then every [`SLOW_RETRY_FRAMES`]. A
//! retry is prepared and dropped before it executes, so a refusal that still
//! stands records and journals nothing; only the accepted retry is journaled,
//! naming the (late) frame it ran on, so replay reproduces it as any late
//! advance. A refusal that cannot clear by itself waits, as before, for
//! another accepted command to change the show.
//!
//! A restarted daemon restores every list's standby pointer and pending
//! wait from the project and journal, and the wait still ends on its due
//! engine frame: show time is engine frames, so the time the daemon was
//! down does not count toward a wait. The native recorder startup barrier
//! runs this clock too. A wait whose due frame the restored cursor has
//! already passed (only when frames were realized without a clock pass)
//! runs on the next frame, exactly as a late advance does while running.

use std::time::Instant;

use fm_command::RejectionCode;
use fm_control::{ControlService, PrepareSubmitOutcome};
use fm_persistence::StoredProject;
use fm_protocol::{CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CommandResult};
use fm_types::FrameRate;

use super::{
    AppResult, NativeDaemon, Policy, ProcessShutdown, SharedControl, development_principal,
    execute_authority_command, now_millis,
};
use crate::journal::DurableStore;
use crate::playlist_clock::{Pacing, wall_frame};

/// Refused attempts the clock retries on the very next show frame before it
/// backs off to [`SLOW_RETRY_FRAMES`].
pub(super) const FAST_RETRIES: u32 = 25;

/// Show frames between retries once [`FAST_RETRIES`] are spent.
pub(super) const SLOW_RETRY_FRAMES: u64 = 25;

/// Paces waiting cues against the show's frame.
#[derive(Debug, Default)]
pub(super) struct CueClock {
    /// Simulated pacing only: the show's frame and the instant it was
    /// observed when the waiting cue was first seen.
    anchor: Option<(u64, Instant)>,
    /// The engine cursor at the previous pass (see the playlist clock).
    previous_cursor: Option<u64>,
    /// The refused advance, while the show still waits on it at the same
    /// revision. Another accepted command (a new revision) or a different
    /// due list drops it, so the next advance is a first attempt again.
    refused: Option<Refusal>,
}

/// An advance the show refused at `revision` for `list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Refusal {
    list: u64,
    revision: u64,
    /// Refused attempts so far, the first included.
    attempts: u32,
    /// The show frame from which the next retry may run; `None` for a
    /// refusal that cannot clear by itself, which waits for a new revision.
    retry_at: Option<u64>,
}

/// One `cue_advance` the clock submits: list `list` at `frame`, expecting
/// `revision`. `retry` counts the refused attempts before this one;
/// `reached` is the show frame it was due at, which paces the retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Advance {
    pub(super) list: u64,
    pub(super) frame: u64,
    pub(super) revision: u64,
    pub(super) retry: u32,
    reached: u64,
}

impl Advance {
    /// The command to execute. A retry carries its own idempotency key: the
    /// first attempt's refusal may hold a receipt under the first key, which
    /// would otherwise answer every retry with the same refusal.
    pub(super) fn command(&self) -> CommandMessage {
        let mut command = advance_command(self.list, self.frame, self.revision);
        if self.retry > 0 {
            let key = format!("{}-retry-{}", command.idempotency_key, self.retry);
            command.id.clone_from(&key);
            command.idempotency_key = key;
        }
        command
    }
}

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

impl CueClock {
    /// Returns the advance due now: the first attempt once the show
    /// reaches the waiting cue's frame, or the next retry of a refused one
    /// once its backoff has passed.
    pub(super) fn due(
        &mut self,
        control: &ControlService<Policy>,
        pacing: Pacing,
    ) -> Option<Advance> {
        let cursor = control.frame_cursor();
        let previous = self.previous_cursor.replace(cursor);
        let Some((list, frame)) = control.cue_advance_frame() else {
            self.anchor = None;
            self.refused = None;
            return None;
        };
        let reached = match pacing {
            Pacing::Wall { now, rate } => {
                let start = previous.map_or(cursor, |previous| previous.min(cursor));
                let (anchor_frame, anchor_at) = *self.anchor.get_or_insert((start, now));
                wall_frame(anchor_frame, anchor_at, now, rate)
            }
            Pacing::Engine => cursor,
        };
        let revision = control.diagnostics().current_revision;
        let retry = match self.refused {
            Some(refused) if refused.list == list && refused.revision == revision => {
                match refused.retry_at {
                    Some(at) if reached >= at => refused.attempts,
                    _ => return None,
                }
            }
            _ => {
                self.refused = None;
                0
            }
        };
        (reached >= frame).then_some(Advance {
            list,
            frame,
            revision,
            retry,
            reached,
        })
    }

    /// Returns the advance to execute now, if any (see [`Self::due`]).
    ///
    /// A retry waits for `retry_window` (no session command waiting this
    /// pass and the journal idle), so retries never take a frame or a
    /// journal turn from a client. It is then prepared and dropped first: a
    /// refusal the engine still gives is observed without executing it, so
    /// such a retry records no receipt, journals nothing and logs nothing.
    ///
    /// # Errors
    ///
    /// Returns a failure to name the clock's principal.
    pub(super) fn next(
        &mut self,
        control: &mut ControlService<Policy>,
        pacing: Pacing,
        retry_window: bool,
        now_millis: u64,
    ) -> AppResult<Option<Advance>> {
        let Some(advance) = self.due(control, pacing) else {
            return Ok(None);
        };
        if advance.retry == 0 {
            return Ok(Some(advance));
        }
        if !retry_window {
            return Ok(None);
        }
        let probed =
            control.prepare_submit(&development_principal()?, advance.command(), now_millis);
        let refused = match probed {
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
        if let Some(result) = refused {
            self.observe(&advance, &result);
            return Ok(None);
        }
        Ok(Some(advance))
    }

    /// Records the outcome of `advance`. A refusal is logged once (on the
    /// first attempt, or when a retry meets one that cannot clear) and
    /// retried after [`retry_delay`] when [`retries`] allows; an accepted
    /// retry logs how many refusals it took.
    pub(super) fn observe(&mut self, advance: &Advance, result: &CommandResult) {
        let Advance {
            list,
            frame,
            revision,
            retry,
            reached,
        } = *advance;
        match result {
            CommandResult::Rejected {
                code,
                message,
                retryable,
                ..
            } => {
                let retrying = retries(code, *retryable);
                let attempts = retry.saturating_add(1);
                if retry == 0 || !retrying {
                    eprintln!(
                        "FREEMIXD_CUE\tv=1\tlist={list}\tframe={frame}\trevision={revision}\tcode={code}\tretry={retrying}\terror={message}"
                    );
                }
                self.refused = Some(Refusal {
                    list,
                    revision,
                    attempts,
                    retry_at: retrying.then(|| reached.saturating_add(retry_delay(attempts))),
                });
            }
            CommandResult::Accepted { .. } => {
                if retry > 0 {
                    eprintln!(
                        "FREEMIXD_CUE\tv=1\tlist={list}\tframe={frame}\trevision={revision}\tretried={retry}\tadvanced=1"
                    );
                }
                self.refused = None;
            }
        }
    }

    /// Submits the waiting cue's advance once the show reaches its frame,
    /// and retries a refused one (see [`Self::next`]); `retry_window` is
    /// whether this pass leaves room for a retry.
    ///
    /// # Errors
    ///
    /// Returns a fatal daemon error from durable execution; a refused
    /// advance is only logged.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn poll(
        &mut self,
        control: &SharedControl,
        store: &dyn DurableStore,
        durable: &mut StoredProject,
        server: &fm_protocol::ServerIdentity,
        native: Option<&mut NativeDaemon>,
        process_shutdown: Option<&ProcessShutdown>,
        now: Instant,
        retry_window: bool,
    ) -> AppResult<()> {
        let pacing = if native.is_some() {
            Pacing::Engine
        } else {
            Pacing::Wall {
                now,
                rate: durable.project().settings().frame_rate,
            }
        };
        let Some(advance) = self.next(
            &mut control.borrow_mut(),
            pacing,
            retry_window && !store.busy(),
            now_millis()?,
        )?
        else {
            return Ok(());
        };
        let delivery = execute_authority_command(
            control,
            store,
            durable,
            &development_principal()?,
            server,
            &advance.command(),
            now_millis()?,
            native,
            process_shutdown,
        )?;
        self.observe(&advance, &delivery.result);
        Ok(())
    }
}

impl CueClock {
    /// Simulated runtime only: before a client's cue control executes,
    /// realizes the idle frames wall time has played since a cue started
    /// waiting, so the control lands on the show's frame (as the playlist
    /// clock's catch-up does). Without it a Stop, Skip, or GO issued
    /// mid-wait ran at the engine cursor, which on the simulated engine only
    /// moves while commands execute, so it was timed as if it came right
    /// after the waiting cue fired.
    ///
    /// Never ticks onto the pending cue's due frame or a pending playlist
    /// boundary, so their clocks still run them on their frames. Journal
    /// replay ticks the same idle frames before the command (see
    /// `replay_idle_frames`).
    ///
    /// # Errors
    ///
    /// Returns an engine tick failure.
    pub(super) fn catch_up(
        &self,
        control: &mut ControlService<Policy>,
        server: &fm_protocol::ServerIdentity,
        rate: FrameRate,
        now: Instant,
    ) -> AppResult<()> {
        let Some((_, due)) = control.cue_advance_frame() else {
            return Ok(());
        };
        let mut target = self
            .observed_frame(control.frame_cursor(), rate, now)
            .min(due.saturating_sub(1));
        if let Some(boundary) = control.playlist_advance_frame() {
            target = target.min(boundary.saturating_sub(1));
        }
        while control.frame_cursor() < target {
            control.tick(server)?;
        }
        Ok(())
    }

    /// The engine frame wall time has reached: the anchored wall-clock frame
    /// while a cue waits (never behind `cursor`), else `cursor`. Under
    /// native pacing no anchor is ever set, so this is `cursor`.
    pub(super) fn observed_frame(&self, cursor: u64, rate: FrameRate, now: Instant) -> u64 {
        self.anchor.map_or(cursor, |(frame, at)| {
            wall_frame(frame, at, now, rate).max(cursor)
        })
    }
}

/// The clock's `cue_advance` for list `list` at `frame`, expecting
/// `revision`. The idempotency key names all three, so a retried advance
/// replays its receipt; the clock's own retries of a refused advance add
/// their attempt (see [`Advance::command`]).
pub(super) fn advance_command(list: u64, frame: u64, revision: u64) -> CommandMessage {
    let key = format!("cue-clock-{revision}-{list}-{frame}");
    CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: key.clone(),
        idempotency_key: key,
        expected_revision: Some(revision),
        deadline_ms: None,
        payload: CommandPayload::CueAdvance { list, frame },
    }
}

/// Refuses the cue command a client may not send: the daemon's clock alone
/// issues `cue_advance`.
pub(super) fn cue_command_refusal(
    command: &CommandMessage,
    current_revision: u64,
) -> Option<CommandResult> {
    if !matches!(command.payload, CommandPayload::CueAdvance { .. }) {
        return None;
    }
    Some(CommandResult::Rejected {
        id: command.id.clone(),
        code: RejectionCode::Unavailable.as_str().to_owned(),
        message: "cue_advance is issued by the daemon's cue clock".to_owned(),
        fields: Vec::new(),
        current_revision,
        retryable: false,
        reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_retry_carries_its_own_idempotency_key() {
        let first = Advance {
            list: 7,
            frame: 40,
            revision: 3,
            retry: 0,
            reached: 40,
        };
        let command = first.command();
        assert_eq!(command, advance_command(7, 40, 3));
        let retry = Advance { retry: 2, ..first }.command();
        assert_eq!(retry.idempotency_key, "cue-clock-3-7-40-retry-2");
        assert_eq!(retry.id, retry.idempotency_key);
        assert_eq!(retry.expected_revision, Some(3));
        assert_eq!(retry.payload, command.payload);
    }
}
