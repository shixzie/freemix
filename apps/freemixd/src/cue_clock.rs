//! The daemon's cue clock (RC-017).
//!
//! A cue's actions after a wait run on an exact engine frame, and only the
//! journaled `cue_advance` for that frame runs them. This clock decides when
//! to submit it, paced exactly like the playlist clock (see
//! [`crate::playlist_clock`]): by wall time anchored to the engine cursor on
//! the simulated daemon, by the native pacer's next frame otherwise. The
//! advance is journaled like any accepted command, so replay and restart
//! reproduce the exact frames.

use std::time::Instant;

use fm_command::RejectionCode;
use fm_control::ControlService;
use fm_persistence::StoredProject;
use fm_protocol::{CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CommandResult};

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
