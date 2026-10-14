//! The daemon's playlist clock.
//!
//! The playlist engine never reads a clock: a boundary is an engine frame,
//! and only the journaled `playlist_advance` for exactly that frame moves the
//! playlist past it. This clock decides when to submit that advance; how it
//! learns which frame the show has reached depends on the runtime.
//!
//! - The simulated engine renders frames only while it executes commands, so
//!   wall time does not move its frame cursor. Once a playlist boundary is
//!   pending the clock anchors the engine cursor to the wall clock, and when
//!   wall time reaches the boundary frame it submits the advance. Executing
//!   it ticks the engine up to the boundary, so the entry's input reaches
//!   Program on the scheduled frame and its transition runs to completion.
//! - The native media runtime's frame pacer realizes every engine frame on
//!   its deadline, so the engine cursor *is* the show's frame. The clock
//!   submits the advance once the boundary is the next frame the pacer will
//!   realize, before the session loop realizes it; the advance then renders
//!   that frame (and the rest of its transition) through the same native
//!   path as any client transition. The check is two integer comparisons per
//!   loop pass; nothing is computed on the frame path itself.
//!
//! Either way the advance is journaled like any accepted command, so journal
//! replay and restart reproduce the exact frames.

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

/// How the clock learns which engine frame the show has reached.
#[derive(Clone, Copy, Debug)]
pub(super) enum Pacing {
    /// The simulated engine: wall time at `now`, anchored to the engine
    /// cursor when the boundary became pending, counted at the show's `rate`.
    Wall { now: Instant, rate: FrameRate },
    /// The native frame pacer: the engine cursor, which the pacer advances
    /// one realized frame at a time.
    Engine,
}

/// Paces pending playlist boundaries against the show's frame.
#[derive(Debug, Default)]
pub(super) struct PlaylistClock {
    /// Simulated pacing only: the engine frame cursor and the instant it was
    /// observed when the pending boundary was first seen.
    anchor: Option<(u64, Instant)>,
    /// The revision at which the last advance was refused; the clock retries
    /// only after another accepted command changes the show.
    refused_at: Option<u64>,
}

impl PlaylistClock {
    /// Returns the advance due now as `(frame, revision)`: the frame the
    /// engine accepts a `playlist_advance` for, once the show has reached it,
    /// and the revision the advance must expect.
    pub(super) fn due(
        &mut self,
        control: &ControlService<Policy>,
        pacing: Pacing,
    ) -> Option<(u64, u64)> {
        let Some(frame) = control.playlist_advance_frame() else {
            self.anchor = None;
            return None;
        };
        let cursor = control.frame_cursor();
        let reached = match pacing {
            Pacing::Wall { now, rate } => {
                let (anchor_frame, anchor_at) = *self.anchor.get_or_insert((cursor, now));
                wall_frame(anchor_frame, anchor_at, now, rate)
            }
            // `frame` is never behind the cursor, so this holds exactly when
            // the boundary is the next frame the pacer realizes.
            Pacing::Engine => cursor,
        };
        let revision = control.diagnostics().current_revision;
        (reached >= frame && self.refused_at != Some(revision)).then_some((frame, revision))
    }

    /// Records the outcome of the advance submitted for `(frame, revision)`:
    /// a refusal is logged once and not retried at the same revision.
    pub(super) fn observe(&mut self, frame: u64, revision: u64, result: &CommandResult) {
        if let CommandResult::Rejected { code, message, .. } = result {
            self.refused_at = Some(revision);
            eprintln!(
                "FREEMIXD_PLAYLIST\tv=1\tframe={frame}\trevision={revision}\tcode={code}\terror={message}"
            );
        }
    }

    /// Submits the pending playlist advance once the show reaches it: by
    /// wall time on the simulated daemon, by the native pacer's frame when
    /// `native` is present (executing through the native realization path).
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
        let Some((frame, revision)) = self.due(&control.borrow(), pacing) else {
            return Ok(());
        };
        let delivery = execute_authority_command(
            control,
            store,
            durable,
            &development_principal()?,
            server,
            &advance_command(frame, revision),
            now_millis()?,
            native,
            process_shutdown,
        )?;
        self.observe(frame, revision, &delivery.result);
        Ok(())
    }

    /// The engine frame wall time has reached: the anchored wall-clock frame
    /// while a boundary is pending (never behind `cursor`), else `cursor`.
    /// Under native pacing no anchor is ever set, so this is `cursor`.
    pub(super) fn observed_frame(&self, cursor: u64, rate: FrameRate, now: Instant) -> u64 {
        self.anchor.map_or(cursor, |(frame, at)| {
            wall_frame(frame, at, now, rate).max(cursor)
        })
    }
}

/// The clock's `playlist_advance` for `frame`, expecting `revision`. The
/// idempotency key names both, so a retried advance replays its receipt.
pub(super) fn advance_command(frame: u64, revision: u64) -> CommandMessage {
    let key = format!("playlist-clock-{revision}-{frame}");
    CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: key.clone(),
        idempotency_key: key,
        expected_revision: Some(revision),
        deadline_ms: None,
        payload: CommandPayload::PlaylistAdvance { frame },
    }
}

/// The engine frame wall time has reached since the anchor.
fn wall_frame(anchor_frame: u64, anchor_at: Instant, now: Instant, rate: FrameRate) -> u64 {
    let elapsed = now.saturating_duration_since(anchor_at).as_nanos();
    let frames =
        elapsed * u128::from(rate.numerator()) / (u128::from(rate.denominator()) * 1_000_000_000);
    anchor_frame.saturating_add(u64::try_from(frames).unwrap_or(u64::MAX))
}

/// Refuses playlist commands a client may not send: the daemon's clock alone
/// issues `playlist_advance`. Playlist control and editing are accepted on
/// both the simulated and the native runtime.
pub(super) fn playlist_command_refusal(
    command: &CommandMessage,
    current_revision: u64,
) -> Option<CommandResult> {
    if !matches!(command.payload, CommandPayload::PlaylistAdvance { .. }) {
        return None;
    }
    Some(CommandResult::Rejected {
        id: command.id.clone(),
        code: RejectionCode::Unavailable.as_str().to_owned(),
        message: "playlist_advance is issued by the daemon's playlist clock".to_owned(),
        fields: Vec::new(),
        current_revision,
        retryable: false,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn wall_frame_counts_whole_frames_from_the_anchor() {
        let rate = FrameRate::new(30_000, 1_001).unwrap();
        let start = Instant::now();
        assert_eq!(wall_frame(7, start, start, rate), 7);
        assert_eq!(
            wall_frame(7, start, start + Duration::from_millis(1_000), rate),
            7 + 29
        );
        assert_eq!(
            wall_frame(7, start, start + Duration::from_millis(1_001), rate),
            7 + 30
        );
        // An instant before the anchor never moves the frame backwards.
        assert_eq!(
            wall_frame(7, start + Duration::from_secs(1), start, rate),
            7
        );
    }

    #[test]
    fn observed_frame_follows_the_anchor_and_never_trails_the_cursor() {
        let rate = FrameRate::new(25, 1).unwrap();
        let start = Instant::now();
        let mut clock = PlaylistClock::default();
        assert_eq!(clock.observed_frame(12, rate, start), 12);
        clock.anchor = Some((10, start));
        let later = start + Duration::from_secs(2);
        assert_eq!(clock.observed_frame(12, rate, later), 60);
        assert_eq!(clock.observed_frame(80, rate, later), 80);
    }
}
