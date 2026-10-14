//! The simulated daemon's playlist clock.
//!
//! The simulated engine renders frames only while it executes commands, so
//! wall time does not move its frame cursor. The clock bridges the two: once
//! a playlist boundary is pending it anchors the engine cursor to the wall
//! clock, and when wall time reaches the boundary frame it submits the
//! journaled `playlist_advance` for exactly that frame. Executing it ticks the
//! engine up to the boundary, so the entry's input reaches Program on the
//! scheduled frame and its transition runs to completion, exactly as journal
//! replay reproduces it.

use std::time::Instant;

use fm_command::RejectionCode;
use fm_persistence::StoredProject;
use fm_protocol::{CURRENT_PROTOCOL_VERSION, CommandMessage, CommandPayload, CommandResult};
use fm_types::FrameRate;

use super::{AppResult, SharedControl, development_principal, execute_durable_command, now_millis};
use crate::journal::DurableStore;

/// Paces pending playlist boundaries against the wall clock.
#[derive(Debug, Default)]
pub(super) struct PlaylistClock {
    /// The engine frame cursor and the instant it was observed when the
    /// pending boundary was first seen.
    anchor: Option<(u64, Instant)>,
    /// The revision at which the last advance was refused; the clock retries
    /// only after another accepted command changes the show.
    refused_at: Option<u64>,
}

impl PlaylistClock {
    /// Submits the pending playlist advance once wall time reaches it.
    ///
    /// # Errors
    ///
    /// Returns a fatal daemon error from durable execution; a refused
    /// advance is only logged.
    pub(super) fn poll(
        &mut self,
        control: &SharedControl,
        store: &dyn DurableStore,
        durable: &mut StoredProject,
        server: &fm_protocol::ServerIdentity,
        now: Instant,
    ) -> AppResult<()> {
        let rate = durable.project().settings().frame_rate;
        let (frame, revision) = {
            let control = control.borrow();
            let Some(frame) = control.playlist_advance_frame() else {
                self.anchor = None;
                return Ok(());
            };
            let cursor = control.frame_cursor();
            let revision = control.diagnostics().current_revision;
            let (anchor_frame, anchor_at) = *self.anchor.get_or_insert((cursor, now));
            if wall_frame(anchor_frame, anchor_at, now, rate) < frame
                || self.refused_at == Some(revision)
            {
                return Ok(());
            }
            (frame, revision)
        };
        let key = format!("playlist-clock-{revision}-{frame}");
        let command = CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: key.clone(),
            idempotency_key: key,
            expected_revision: Some(revision),
            deadline_ms: None,
            payload: CommandPayload::PlaylistAdvance { frame },
        };
        let execution = execute_durable_command(
            &mut control.borrow_mut(),
            store,
            durable,
            &development_principal()?,
            server,
            &command,
            now_millis()?,
        )?;
        if let CommandResult::Rejected { code, message, .. } = &execution.submission.output.result {
            self.refused_at = Some(revision);
            eprintln!(
                "FREEMIXD_PLAYLIST\tv=1\tframe={frame}\trevision={revision}\tcode={code}\terror={message}"
            );
        }
        Ok(())
    }
}

/// The engine frame wall time has reached since the anchor.
fn wall_frame(anchor_frame: u64, anchor_at: Instant, now: Instant, rate: FrameRate) -> u64 {
    let elapsed = now.saturating_duration_since(anchor_at).as_nanos();
    let frames =
        elapsed * u128::from(rate.numerator()) / (u128::from(rate.denominator()) * 1_000_000_000);
    anchor_frame.saturating_add(u64::try_from(frames).unwrap_or(u64::MAX))
}

/// Refuses playlist commands a client may not send to this daemon: the
/// clock alone issues `playlist_advance`, and the native media runtime has no
/// playlist clock, so it refuses playlist control too. Editing the list is
/// always allowed.
pub(super) fn playlist_command_refusal(
    command: &CommandMessage,
    native: bool,
    current_revision: u64,
) -> Option<CommandResult> {
    let message = match command.payload {
        CommandPayload::PlaylistAdvance { .. } => {
            "playlist_advance is issued by the daemon's playlist clock"
        }
        CommandPayload::PlaylistControl { .. } if native => {
            "the native media runtime does not run the playlist clock; \
             playlists play on the simulated daemon"
        }
        _ => return None,
    };
    Some(CommandResult::Rejected {
        id: command.id.clone(),
        code: RejectionCode::Unavailable.as_str().to_owned(),
        message: message.to_owned(),
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
}
