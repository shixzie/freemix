//! The daemon's media clock (IN-003).
//!
//! The engine owns every media input's transport, and a transport only moves
//! when the engine renders a frame. The native media runtime's frame pacer
//! renders a frame on every deadline, so its media inputs play in real time
//! already. The simulated engine renders frames only while it executes
//! commands, so while any media transport is advancing (playing, waiting at a
//! growing source's live edge, or with a command still to realize) this clock
//! anchors the engine cursor to the wall clock and renders the idle frames
//! wall time has reached, so a playing clip's position, end, loop and end
//! trigger land on time without a client sending commands.
//!
//! It never renders onto a pending playlist boundary or a waiting cue's
//! frame: the playlist clock's journaled `playlist_advance` still takes the
//! next entry on its frame, and the cue clock's `cue_advance` runs the cue
//! on its own.
//! Journal replay renders the same idle frames before the next recorded
//! command (`replay_idle_frames`), exactly as it does for the native pacer.
//!
//! On either runtime, an end trigger fired by the media input the playlist
//! is airing advances the playlist: the authority submits a journaled
//! `playlist_control next`, so replay reproduces the cut without replaying
//! media. One the show refuses for a reason its own frames clear is retried
//! while that airing is still on ([`EndTriggerFeed`]).

use std::time::Instant;

use fm_control::{ControlService, MediaEndTrigger, MediaListEnd};
use fm_engine::fm_playback::MediaState;
use fm_persistence::StoredProject;
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CommandResult, PlaylistLoopMode,
    PlaylistOperation, PlaylistRunState, WireInputId,
};
use fm_types::{FrameRate, InputId};

use super::{
    AppResult, NativeDaemon, Policy, ProcessShutdown, SharedControl, development_principal,
    execute_authority_command, now_millis,
};
use crate::clock_retry::{Outcome, Retries, probe_refusal, retry_command};
use crate::journal::DurableStore;

/// Paces the simulated engine's media transports against the wall clock.
#[derive(Debug, Default)]
pub(super) struct MediaClock {
    /// The engine frame and the instant it was observed when a media
    /// transport started advancing.
    anchor: Option<(u64, Instant)>,
}

impl MediaClock {
    /// Simulated runtime only: renders the idle frames wall time has reached
    /// since the anchor while any media transport advances, at most one
    /// second of frames per pass and never onto a pending playlist boundary
    /// or a waiting cue's frame.
    /// A clock that fell further behind (a stalled loop) re-anchors instead
    /// of rendering the backlog in a burst.
    ///
    /// # Errors
    ///
    /// Returns an engine tick failure.
    pub(super) fn poll(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &fm_protocol::ServerIdentity,
        rate: FrameRate,
        now: Instant,
    ) -> AppResult<()> {
        // A slideshow advances its slides and Ken Burns like a transport
        // (IN-009).
        let advancing = |control: &ControlService<Policy>| {
            control.media_advancing() || control.slideshows_advancing()
        };
        if !advancing(control) {
            self.anchor = None;
            return Ok(());
        }
        let cursor = control.frame_cursor();
        let (anchor_frame, anchor_at) = *self.anchor.get_or_insert((cursor, now));
        let wall = crate::playlist_clock::wall_frame(anchor_frame, anchor_at, now, rate);
        let second = u64::from(rate.numerator()).div_ceil(u64::from(rate.denominator()));
        if wall > cursor.saturating_add(second) {
            self.anchor = Some((cursor, now));
            return Ok(());
        }
        let mut target = wall;
        if let Some(boundary) = control.playlist_advance_frame() {
            target = target.min(boundary.saturating_sub(1));
        }
        // Nor onto a waiting cue's frame. The cue clock anchors its wait to
        // the frame before the command that started it, this clock to the
        // cursor after it (a cue that rolls a clip and then waits), so this
        // clock can run frames ahead of it: rendering on would pass the
        // cue's frame first, and its advance would chase the cursor for as
        // long as media plays.
        if let Some((_, due)) = control.cue_advance_frame() {
            target = target.min(due.saturating_sub(1));
        }
        while control.frame_cursor() < target && advancing(control) {
            control.tick(server)?;
        }
        Ok(())
    }
}

/// The take of the playlist airing `trigger`'s input, while the playlist
/// plays an entry of that input: the airing an end trigger advances.
pub(super) fn airing_take(
    control: &ControlService<Policy>,
    trigger: MediaEndTrigger,
) -> Option<u64> {
    let playlist = control.playlist_state()?;
    let PlaylistRunState::Playing { entry, take, .. } = playlist.state else {
        return None;
    };
    let airing = playlist
        .entries
        .iter()
        .find(|candidate| candidate.id == entry)?;
    (airing.input == WireInputId::from_domain(trigger.input)).then_some(take)
}

/// Whether `input`'s clip is still at its end trigger: the transport is
/// stopped or holding its last frame, or its playhead is still within the
/// end-trigger lead of the playable end (at or after it when no lead is
/// set any more). An operator who seeks or rewinds the clip, or a loop that
/// starts its next pass, puts the playhead back before the trigger point,
/// so the clip is playing again and a refused trigger no longer applies to
/// it (a later pass fires its own).
///
/// The status projects the media commands accepted but not yet realized,
/// so a seek just accepted already counts. An input with no media transport
/// has no playhead to contradict the trigger.
pub(super) fn end_reached(control: &ControlService<Policy>, input: InputId) -> bool {
    let Some(status) = control.media_status(input) else {
        return true;
    };
    match status.state {
        MediaState::Stopped | MediaState::Ended => true,
        MediaState::Playing | MediaState::Paused | MediaState::Waiting => {
            let lead = status.settings.end_trigger_lead.map_or(0, u64::from);
            status.playable_end.saturating_sub(status.position) <= lead
        }
    }
}

/// Whether the playlist has an entry `playlist_control next` would take to
/// air: an enabled entry after the one on air, or with loop All any enabled
/// entry. Without one the engine refuses Next as a conflict that no frame of
/// the show clears, so an end trigger meeting it is final: the last clip of
/// a playlist that does not loop holds, as on a hardware switcher, and an
/// entry appended later is not cut to by a trigger that fired before it.
pub(super) fn has_next_entry(control: &ControlService<Policy>) -> bool {
    let Some(playlist) = control.playlist_state() else {
        return false;
    };
    let (PlaylistRunState::Playing { entry, .. } | PlaylistRunState::Paused { entry, .. }) =
        playlist.state
    else {
        return false;
    };
    let Some(index) = playlist
        .entries
        .iter()
        .position(|candidate| candidate.id == entry)
    else {
        return false;
    };
    playlist.entries[index + 1..]
        .iter()
        .any(|candidate| candidate.enabled)
        || (playlist.loop_mode == PlaylistLoopMode::All
            && playlist.entries.iter().any(|candidate| candidate.enabled))
}

/// The playlist command an end trigger feeds: `playlist_control next`,
/// expecting `revision`. The idempotency key names the input and the trigger
/// frame, so a retried trigger replays its receipt.
pub(super) fn end_trigger_command(trigger: MediaEndTrigger, revision: u64) -> CommandMessage {
    let key = format!("media-end-{}-{}", trigger.input, trigger.frame);
    CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: key.clone(),
        idempotency_key: key,
        expected_revision: Some(revision),
        deadline_ms: None,
        payload: CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Next,
        },
    }
}

/// The command a media list item that ended under Auto Next feeds (IN-006):
/// `media_list advance` from the item that ended. The key names the input,
/// the item, and the frame it ended on, so a repeated delivery replays its
/// receipt; the engine refuses an advance whose item is no longer active,
/// so one overtaken by the operator does nothing.
pub(super) fn media_list_end_command(end: MediaListEnd) -> CommandMessage {
    let key = format!("media-list-end-{}-{}-{}", end.input, end.item, end.frame);
    CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: key.clone(),
        idempotency_key: key,
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::MediaList {
            input: WireInputId::from_domain(end.input),
            operation: fm_protocol::MediaListOperation::Advance { from: end.item },
        },
    }
}

/// A refused end trigger's later attempt `retry` at `revision`: its key
/// names the revision too, so no later attempt (a retry, or a first attempt
/// at a new revision) meets the receipt of an earlier refusal; see
/// [`retry_command`].
pub(super) fn end_trigger_retry_command(
    trigger: MediaEndTrigger,
    revision: u64,
    retry: u32,
) -> CommandMessage {
    let mut command = end_trigger_command(trigger, revision);
    let key = format!("{}-r{revision}", command.idempotency_key);
    command.id.clone_from(&key);
    command.idempotency_key = key;
    retry_command(command, retry)
}

/// Feeds every end trigger fired since the last pass to playlist automation
/// (see [`end_trigger_command`]), and retries one the show refused while it
/// still applies, by the policy every daemon clock shares
/// ([`crate::clock_retry`]): a refusal that clears by itself (a cue frame or
/// playlist take held for later, a journal that could not record it) no
/// longer leaves the playlist stuck on the ended clip.
///
/// A pending trigger still applies while the airing it ends is on, the clip
/// is still at its end ([`end_reached`]: not sought or rewound back into
/// play) and the playlist has a next entry ([`has_next_entry`]). A refusal
/// met with no next entry is final: the trigger is dropped, not retried, so
/// the last clip of a playlist that does not loop holds, and an entry the
/// operator appends later is not cut to by a trigger that fired before it.
#[derive(Debug, Default)]
pub(super) struct EndTriggerFeed {
    /// The refused trigger the feed still owes the playlist: the airing
    /// (`take`) it ends, and the show frame and instant of its refusal, which
    /// pace its retries on the simulated runtime.
    pending: Option<PendingTrigger>,
    /// The pending trigger's refusal, keyed by the take it ends.
    refused: Retries<u64>,
}

#[derive(Clone, Copy, Debug)]
struct PendingTrigger {
    trigger: MediaEndTrigger,
    take: u64,
    anchor: (u64, Instant),
}

/// One end-trigger attempt: `retry` counts the refused attempts before it
/// at `revision`; `reached` is the show frame it ran at.
#[derive(Clone, Copy, Debug)]
struct Attempt {
    trigger: MediaEndTrigger,
    take: u64,
    revision: u64,
    retry: u32,
    reached: u64,
}

impl EndTriggerFeed {
    /// Feeds the new end triggers, then retries a pending refused one when
    /// its backoff has passed and `retry_window` (no session command
    /// waiting this pass) and an idle journal leave room. A retry is
    /// prepared and dropped first, so one the show still refuses records,
    /// journals and logs nothing.
    ///
    /// # Errors
    ///
    /// Returns a fatal daemon error from durable execution.
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
        // Auto Next (IN-006): every list item that ended advances its list
        // by a journaled command, so replay reaches the same item on the
        // same frame. A refusal (the operator moved on, or the list ended)
        // is final.
        let ends = control.borrow_mut().take_media_list_ends();
        for end in ends {
            execute_authority_command(
                control,
                store,
                durable,
                &development_principal()?,
                server,
                &media_list_end_command(end),
                now_millis()?,
                native.as_deref_mut(),
                process_shutdown,
            )?;
        }
        let triggers = control.borrow_mut().take_media_end_triggers();
        let fed = !triggers.is_empty();
        for trigger in triggers {
            let (take, revision, reached) = {
                let control = control.borrow();
                let Some(take) = airing_take(&control, trigger) else {
                    continue;
                };
                (
                    take,
                    control.diagnostics().current_revision,
                    control.frame_cursor(),
                )
            };
            let attempt = Attempt {
                trigger,
                take,
                revision,
                retry: 0,
                reached,
            };
            let delivery = execute_authority_command(
                control,
                store,
                durable,
                &development_principal()?,
                server,
                &end_trigger_command(trigger, revision),
                now_millis()?,
                native.as_deref_mut(),
                process_shutdown,
            )?;
            let clears = has_next_entry(&control.borrow());
            self.observe(&attempt, &delivery.result, now, clears);
        }
        if fed || !retry_window || store.busy() {
            return Ok(());
        }
        let Some(attempt) = self.due(&control.borrow(), native.is_some(), durable, now) else {
            return Ok(());
        };
        // Like a client command, an accepted retry is journaled before its
        // frame: under native pacing it waits for room before the deadline.
        if let Some(native) = native.as_deref_mut()
            && !native.retry_window(store.recent_append_peak())
        {
            return Ok(());
        }
        let command = end_trigger_retry_command(attempt.trigger, attempt.revision, attempt.retry);
        if let Some(result) =
            probe_refusal(&mut control.borrow_mut(), command.clone(), now_millis()?)?
        {
            let clears = has_next_entry(&control.borrow());
            self.observe(&attempt, &result, now, clears);
            return Ok(());
        }
        let delivery = execute_authority_command(
            control,
            store,
            durable,
            &development_principal()?,
            server,
            &command,
            now_millis()?,
            native,
            process_shutdown,
        )?;
        let clears = has_next_entry(&control.borrow());
        self.observe(&attempt, &delivery.result, now, clears);
        Ok(())
    }

    /// The pending trigger's next attempt, once its backoff has passed: the
    /// show frame is the engine cursor under native pacing, else wall time
    /// since the refusal. A trigger that no longer applies (its airing is
    /// off, its clip plays again, or the playlist has no next entry) is
    /// dropped.
    fn due(
        &mut self,
        control: &ControlService<Policy>,
        native: bool,
        durable: &StoredProject,
        now: Instant,
    ) -> Option<Attempt> {
        let pending = self.pending?;
        let input = pending.trigger.input;
        if airing_take(control, pending.trigger) != Some(pending.take)
            || !end_reached(control, input)
            || !has_next_entry(control)
        {
            self.pending = None;
            self.refused.clear();
            return None;
        }
        let cursor = control.frame_cursor();
        let reached = if native {
            cursor
        } else {
            let (frame, at) = pending.anchor;
            crate::playlist_clock::wall_frame(
                frame,
                at,
                now,
                durable.project().settings().frame_rate,
            )
            .max(cursor)
        };
        let revision = control.diagnostics().current_revision;
        let retry = self.refused.attempt(&pending.take, revision, reached)?;
        Some(Attempt {
            trigger: pending.trigger,
            take: pending.take,
            revision,
            retry,
            reached,
        })
    }

    /// Records the outcome of the first attempt to feed `trigger`, which ends
    /// airing `take`, made at `revision` on show frame `reached` (as
    /// [`Self::poll`] does for a new trigger, `control` being the show after
    /// it).
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn observe_first(
        &mut self,
        control: &ControlService<Policy>,
        trigger: MediaEndTrigger,
        take: u64,
        revision: u64,
        reached: u64,
        result: &CommandResult,
        now: Instant,
    ) {
        let attempt = Attempt {
            trigger,
            take,
            revision,
            retry: 0,
            reached,
        };
        self.observe(&attempt, result, now, has_next_entry(control));
    }

    /// Whether a trigger is pending: refused, and still owed a retry.
    #[cfg(test)]
    pub(super) const fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Records the outcome of `attempt` and logs what it says to (see
    /// [`Retries::observe_with`]): a refused trigger stays pending, unless
    /// the refusal cannot clear (`clears` is false when the playlist has no
    /// next entry), and an accepted one is done.
    fn observe(&mut self, attempt: &Attempt, result: &CommandResult, now: Instant, clears: bool) {
        let Attempt {
            trigger,
            take,
            revision,
            retry,
            reached,
        } = *attempt;
        if matches!(result, CommandResult::Rejected { .. }) && clears {
            let same = self
                .pending
                .is_some_and(|pending| pending.trigger == trigger && pending.take == take);
            if !same {
                self.pending = Some(PendingTrigger {
                    trigger,
                    take,
                    anchor: (reached, now),
                });
            }
        } else {
            self.pending = None;
        }
        let (input, frame) = (trigger.input, trigger.frame);
        let outcome = self
            .refused
            .observe_with(take, revision, retry, reached, result, clears);
        if self.pending.is_none() {
            self.refused.clear();
        }
        match outcome {
            Outcome::Quiet => {}
            Outcome::Refused {
                code,
                message,
                retrying,
            } => eprintln!(
                "FREEMIXD_MEDIA\tv=1\tinput={input}\tframe={frame}\trevision={revision}\tcode={code}\tretry={retrying}\terror={message}"
            ),
            Outcome::Advanced { refusals } => eprintln!(
                "FREEMIXD_MEDIA\tv=1\tinput={input}\tframe={frame}\trevision={revision}\tretried={refusals}\tadvanced=1"
            ),
        }
    }
}
