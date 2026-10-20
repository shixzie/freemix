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
//!   it ticks the engine idle up to the boundary, so the entry's input
//!   reaches Program on the scheduled frame; its transition then runs on
//!   the session loop's simulated frame loop, during which the clock waits.
//!   A client's playlist control first catches the engine up to the
//!   anchored wall-clock frame (see [`PlaylistClock::catch_up`]), so a Pause
//!   holds the entry where the show had played it to.
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
//!
//! An advance the show refuses for a reason its own frames can clear (a cue
//! frame held for later, a realization still settling, a journal that could
//! not record it) is retried on later frames without any other command, by
//! the policy every daemon clock shares ([`crate::clock_retry`]): prepared
//! and dropped while the refusal stands, so only the accepted retry is
//! journaled, naming the (late) frame it ran on. A refusal that cannot clear
//! by itself waits for another accepted command to change the show.

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
use crate::clock_retry::{Outcome, Retries, probe_refusal, retry_command};
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
    /// Simulated pacing only: the show's frame and the instant it was
    /// observed when the pending boundary was first seen.
    anchor: Option<(u64, Instant)>,
    /// The engine cursor at the previous pass, before any command executed
    /// since. The anchor starts there: the command that made a boundary
    /// pending (a Start) began on that frame at about this instant, and the
    /// frames it rendered at once (its transition) are part of the show's
    /// wall time, not a head start on it.
    previous_cursor: Option<u64>,
    /// The refused advance, while the show still waits on it at the same
    /// revision (see [`crate::clock_retry`]). Another accepted command (a
    /// new revision) drops it, so the next advance is a first attempt again.
    refused: Retries<()>,
}

/// One `playlist_advance` the clock submits: at `frame`, expecting
/// `revision`. `retry` counts the refused attempts before this one;
/// `reached` is the show frame it was due at, which paces the retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Advance {
    pub(super) frame: u64,
    pub(super) revision: u64,
    pub(super) retry: u32,
    reached: u64,
}

impl Advance {
    /// The command to execute; a retry carries its own idempotency key (see
    /// [`retry_command`]).
    pub(super) fn command(&self) -> CommandMessage {
        retry_command(advance_command(self.frame, self.revision), self.retry)
    }
}

impl PlaylistClock {
    /// Returns the advance due now: the first attempt once the show has
    /// reached the frame the engine accepts a `playlist_advance` for, or the
    /// next retry of a refused one once its backoff has passed.
    pub(super) fn due(
        &mut self,
        control: &ControlService<Policy>,
        pacing: Pacing,
    ) -> Option<Advance> {
        let cursor = control.frame_cursor();
        let previous = self.previous_cursor.replace(cursor);
        let Some(frame) = control.playlist_advance_frame() else {
            self.anchor = None;
            self.refused.clear();
            return None;
        };
        let reached = match pacing {
            Pacing::Wall { now, rate } => {
                let start = previous.map_or(cursor, |previous| previous.min(cursor));
                let (anchor_frame, anchor_at) = *self.anchor.get_or_insert((start, now));
                wall_frame(anchor_frame, anchor_at, now, rate)
            }
            // `frame` is never behind the cursor, so this holds exactly when
            // the boundary is the next frame the pacer realizes.
            Pacing::Engine => cursor,
        };
        let revision = control.diagnostics().current_revision;
        let retry = self.refused.attempt(&(), revision, reached)?;
        (reached >= frame).then_some(Advance {
            frame,
            revision,
            retry,
            reached,
        })
    }

    /// Simulated pacing only: follows the engine cursor on a pass the
    /// clock waits for the frame loop to settle (a timed transition the
    /// session loop's simulated pacer realizes at wall time), so a
    /// playlist boundary that becomes pending is anchored where those frames end,
    /// not before them as if they had been rendered at once.
    pub(super) fn track(&mut self, control: &ControlService<Policy>) {
        self.previous_cursor = Some(control.frame_cursor());
    }

    /// Returns the advance to execute now, if any (see [`Self::due`]).
    ///
    /// A retry waits for `retry_window` (no session command waiting this
    /// pass, the journal idle and, under native pacing, room before the next
    /// frame's deadline for the retry's journal append), so retries never
    /// take a frame or a journal turn from a client nor make a frame late.
    /// The window is asked only for a retry: a first attempt keeps its
    /// scheduled frame. It is then prepared and dropped first
    /// ([`probe_refusal`]): a refusal the engine still gives is observed
    /// without executing it, so such a retry records no receipt, journals
    /// nothing and logs nothing.
    ///
    /// # Errors
    ///
    /// Returns a failure to name the clock's principal.
    pub(super) fn next(
        &mut self,
        control: &mut ControlService<Policy>,
        pacing: Pacing,
        retry_window: impl FnOnce() -> bool,
        now_millis: u64,
    ) -> AppResult<Option<Advance>> {
        let Some(advance) = self.due(control, pacing) else {
            return Ok(None);
        };
        if advance.retry == 0 {
            return Ok(Some(advance));
        }
        if !retry_window() {
            return Ok(None);
        }
        if let Some(result) = probe_refusal(control, advance.command(), now_millis)? {
            self.observe(&advance, &result);
            return Ok(None);
        }
        Ok(Some(advance))
    }

    /// Records the outcome of `advance` (see [`Retries::observe`]): a
    /// refusal is logged once, with whether the clock retries it, and an
    /// accepted retry logs how many refusals it took.
    pub(super) fn observe(&mut self, advance: &Advance, result: &CommandResult) {
        let Advance {
            frame,
            revision,
            retry,
            reached,
        } = *advance;
        match self.refused.observe((), revision, retry, reached, result) {
            Outcome::Quiet => {}
            Outcome::Refused {
                code,
                message,
                retrying,
            } => eprintln!(
                "FREEMIXD_PLAYLIST\tv=1\tframe={frame}\trevision={revision}\tcode={code}\tretry={retrying}\terror={message}"
            ),
            Outcome::Advanced { refusals } => eprintln!(
                "FREEMIXD_PLAYLIST\tv=1\tframe={frame}\trevision={revision}\tretried={refusals}\tadvanced=1"
            ),
        }
    }

    /// Submits the pending playlist advance once the show reaches it: by
    /// wall time on the simulated daemon, by the native pacer's frame when
    /// `native` is present (executing through the native realization path),
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
        mut native: Option<&mut NativeDaemon>,
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
            || {
                retry_window
                    && !store.busy()
                    && native
                        .as_deref_mut()
                        .is_none_or(|native| native.retry_window(store.recent_append_peak()))
            },
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

    /// Simulated runtime only: before a client's playlist control executes,
    /// realizes the idle frames wall time has played since the anchor, so
    /// the command lands on the show's frame. Without this a Pause recorded
    /// the entry's position at the engine cursor, which on the simulated
    /// engine only moves while commands execute (often just the entry's
    /// transition frames), so a long-running entry paused near its start;
    /// Next, Previous, Jump, and Stop were timed the same way.
    ///
    /// Never ticks onto the pending boundary or a waiting cue's frame, so
    /// the clocks' own advances still take the next entry and run the cue
    /// on their frames. Journal replay ticks the
    /// same idle frames before the command (see `replay_idle_frames`).
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
        let Some(boundary) = control.playlist_advance_frame() else {
            return Ok(());
        };
        let mut target = self
            .observed_frame(control.frame_cursor(), rate, now)
            .min(boundary.saturating_sub(1));
        // Nor onto a waiting cue's frame, which only the cue clock's advance
        // may run (as the cue clock's own catch-up holds).
        if let Some((_, due)) = control.cue_advance_frame() {
            target = target.min(due.saturating_sub(1));
        }
        while control.frame_cursor() < target {
            control.tick(server)?;
        }
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
/// idempotency key names both, so a retried advance replays its receipt;
/// the clock's own retries of a refused advance add their attempt (see
/// [`Advance::command`]).
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
pub(super) fn wall_frame(
    anchor_frame: u64,
    anchor_at: Instant,
    now: Instant,
    rate: FrameRate,
) -> u64 {
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
        reason: None,
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
