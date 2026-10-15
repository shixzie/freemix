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
//! It never renders onto a pending playlist boundary: the playlist clock's
//! journaled `playlist_advance` still takes the next entry on its frame.
//! Journal replay renders the same idle frames before the next recorded
//! command (`replay_idle_frames`), exactly as it does for the native pacer.
//!
//! On either runtime, an end trigger fired by the media input the playlist
//! is airing advances the playlist: the authority submits a journaled
//! `playlist_control next`, so replay reproduces the cut without replaying
//! media.

use std::time::Instant;

use fm_control::{ControlService, MediaEndTrigger};
use fm_persistence::StoredProject;
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CommandResult, PlaylistOperation,
    PlaylistRunState, WireInputId,
};
use fm_types::FrameRate;

use super::{
    AppResult, NativeDaemon, Policy, ProcessShutdown, SharedControl, development_principal,
    execute_authority_command, now_millis,
};
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
    /// second of frames per pass and never onto a pending playlist boundary.
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
        if !control.media_advancing() {
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
        while control.frame_cursor() < target && control.media_advancing() {
            control.tick(server)?;
        }
        Ok(())
    }
}

/// The playlist command an end trigger feeds, when the playlist is airing
/// the trigger's input: `playlist_control next`, expecting `revision`. The
/// idempotency key names the input and the trigger frame, so a retried
/// trigger replays its receipt.
pub(super) fn end_trigger_command(
    control: &ControlService<Policy>,
    trigger: MediaEndTrigger,
) -> Option<CommandMessage> {
    let playlist = control.playlist_state()?;
    let PlaylistRunState::Playing { entry, .. } = playlist.state else {
        return None;
    };
    let airing = playlist
        .entries
        .iter()
        .find(|candidate| candidate.id == entry)?;
    if airing.input != WireInputId::from_domain(trigger.input) {
        return None;
    }
    let key = format!("media-end-{}-{}", trigger.input, trigger.frame);
    Some(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: key.clone(),
        idempotency_key: key,
        expected_revision: Some(control.diagnostics().current_revision),
        deadline_ms: None,
        payload: CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Next,
        },
    })
}

/// Feeds every end trigger fired since the last pass to playlist automation
/// (see [`end_trigger_command`]). A refused advance is logged, not retried.
///
/// # Errors
///
/// Returns a fatal daemon error from durable execution.
pub(super) fn feed_end_triggers(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    server: &fm_protocol::ServerIdentity,
    mut native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<()> {
    let triggers = control.borrow_mut().take_media_end_triggers();
    for trigger in triggers {
        let Some(command) = end_trigger_command(&control.borrow(), trigger) else {
            continue;
        };
        let delivery = execute_authority_command(
            control,
            store,
            durable,
            &development_principal()?,
            server,
            &command,
            now_millis()?,
            native.as_deref_mut(),
            process_shutdown,
        )?;
        if let CommandResult::Rejected { code, message, .. } = &delivery.result {
            eprintln!(
                "FREEMIXD_MEDIA\tv=1\tinput={}\tframe={}\tcode={code}\terror={message}",
                trigger.input, trigger.frame
            );
        }
    }
    Ok(())
}
