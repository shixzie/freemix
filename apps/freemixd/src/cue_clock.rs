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
//! A restarted daemon restores every list's standby pointer and pending
//! wait from the project and journal, and the wait still ends on its due
//! engine frame: show time is engine frames, so the time the daemon was
//! down does not count toward a wait. The native recorder startup barrier
//! runs this clock too. A wait whose due frame the restored cursor has
//! already passed (only when frames were realized without a clock pass)
//! runs on the next frame, exactly as a late advance does while running.

use std::time::Instant;

use fm_command::RejectionCode;
use fm_control::ControlService;
use fm_persistence::StoredProject;
use fm_protocol::{CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CommandResult};
use fm_types::FrameRate;

use super::{
    AppResult, NativeDaemon, Policy, ProcessShutdown, SharedControl, development_principal,
    execute_authority_command, now_millis,
};
use crate::journal::DurableStore;
use crate::playlist_clock::{Pacing, wall_frame};

/// Paces waiting cues against the show's frame.
#[derive(Debug, Default)]
pub(super) struct CueClock {
    /// Simulated pacing only: the show's frame and the instant it was
    /// observed when the waiting cue was first seen.
    anchor: Option<(u64, Instant)>,
    /// The engine cursor at the previous pass (see the playlist clock).
    previous_cursor: Option<u64>,
    /// The revision at which the last advance was refused; the clock retries
    /// only after another accepted command changes the show.
    refused_at: Option<u64>,
}

impl CueClock {
    /// Returns the advance due now as `(list, frame, revision)`.
    pub(super) fn due(
        &mut self,
        control: &ControlService<Policy>,
        pacing: Pacing,
    ) -> Option<(u64, u64, u64)> {
        let cursor = control.frame_cursor();
        let previous = self.previous_cursor.replace(cursor);
        let Some((list, frame)) = control.cue_advance_frame() else {
            self.anchor = None;
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
        (reached >= frame && self.refused_at != Some(revision)).then_some((list, frame, revision))
    }

    /// Records the outcome of the advance submitted at `revision`: a
    /// refusal is logged once and not retried at the same revision.
    pub(super) fn observe(&mut self, list: u64, frame: u64, revision: u64, result: &CommandResult) {
        if let CommandResult::Rejected { code, message, .. } = result {
            self.refused_at = Some(revision);
            eprintln!(
                "FREEMIXD_CUE\tv=1\tlist={list}\tframe={frame}\trevision={revision}\tcode={code}\terror={message}"
            );
        }
    }

    /// Submits the waiting cue's advance once the show reaches its frame.
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
    ) -> AppResult<()> {
        let pacing = if native.is_some() {
            Pacing::Engine
        } else {
            Pacing::Wall {
                now,
                rate: durable.project().settings().frame_rate,
            }
        };
        let Some((list, frame, revision)) = self.due(&control.borrow(), pacing) else {
            return Ok(());
        };
        let delivery = execute_authority_command(
            control,
            store,
            durable,
            &development_principal()?,
            server,
            &advance_command(list, frame, revision),
            now_millis()?,
            native,
            process_shutdown,
        )?;
        self.observe(list, frame, revision, &delivery.result);
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
/// replays its receipt.
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
