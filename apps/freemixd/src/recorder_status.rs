//! Realized program recorder status for sessions (Protocol 2.66).
//!
//! The native daemon realizes each recorder slot's desired flag on its own
//! (open worker, mux-readiness barrier, rotations, capture failures). This
//! module turns what each slot is doing into the lossy `recorder_status`
//! record: `starting` while a segment opens, `recording` with the time since
//! the slot's first segment became ready (kept across rotations), and
//! `failed` with a machine-readable [`OutputFailure`] (Protocol 2.67) latched
//! until the next start or stop. Records are built at most every
//! [`RECORDER_STATUS_INTERVAL`], so the scheduler hands peers a bounded,
//! latest-wins feed.

use std::time::{Duration, Instant};

use fm_protocol::{
    OutputFailure, OutputFailureCode, RecorderRealizedState, RecorderStatusMessage,
    RecorderStatusSample, ServerIdentity, WireRecorderSlot,
};

/// How often a `recorder_status` record is built at most.
pub(crate) const RECORDER_STATUS_INTERVAL: Duration = Duration::from_millis(250);

/// What one recorder slot is doing right now, as the daemon sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SlotView {
    /// No output is configured for the slot.
    Unconfigured,
    /// Configured, no segment open.
    Off,
    /// A segment is opening or awaiting mux readiness (a rotation's next
    /// segment too, which keeps the run going).
    Settling,
    /// A segment is open and writing.
    Running,
    /// The open segment failed for this reason.
    Failed(OutputFailure),
}

#[derive(Debug, Default)]
struct SlotStatus {
    /// When the slot's current run first reached `recording`.
    recording_since: Option<Instant>,
    /// The latched failure of the slot's last attempt.
    failure: Option<OutputFailure>,
}

/// Per-slot realized recorder bookkeeping and the record sequence.
#[derive(Debug, Default)]
pub(crate) struct RecorderStatusTracker {
    slots: [SlotStatus; 2],
    sequence: u64,
    built_at: Option<Instant>,
}

impl RecorderStatusTracker {
    /// A start was requested for `slot` (zero-based): a latched failure is
    /// cleared so the slot reports its new attempt.
    pub(crate) fn started(&mut self, slot: usize) {
        if let Some(status) = self.slots.get_mut(slot) {
            status.failure = None;
        }
    }

    /// The operator stopped `slot`: it reports `off`.
    pub(crate) fn stopped(&mut self, slot: usize) {
        if let Some(status) = self.slots.get_mut(slot) {
            *status = SlotStatus::default();
        }
    }

    /// `slot` lost its segment to `failure`; the slot reports `failed` until
    /// the next start or stop.
    pub(crate) fn failed(&mut self, slot: usize, failure: OutputFailure) {
        if let Some(status) = self.slots.get_mut(slot) {
            status.recording_since = None;
            status.failure = Some(failure);
        }
    }

    /// The sample `slot` reports for `view` at `now`.
    fn sample(&mut self, slot: usize, view: SlotView, now: Instant) -> RecorderStatusSample {
        let status = &mut self.slots[slot];
        let realized = match view {
            SlotView::Unconfigured => {
                *status = SlotStatus::default();
                RecorderRealizedState::Unavailable
            }
            SlotView::Off => {
                status.recording_since = None;
                if status.failure.is_some() {
                    RecorderRealizedState::Failed
                } else {
                    RecorderRealizedState::Off
                }
            }
            // A rotation's next segment opens while the run goes on.
            SlotView::Settling if status.recording_since.is_some() => {
                RecorderRealizedState::Recording
            }
            SlotView::Settling => RecorderRealizedState::Starting,
            SlotView::Running => {
                status.recording_since.get_or_insert(now);
                status.failure = None;
                RecorderRealizedState::Recording
            }
            SlotView::Failed(failure) => {
                status.recording_since = None;
                status.failure = Some(failure);
                RecorderRealizedState::Failed
            }
        };
        let elapsed_ms = match (realized, status.recording_since) {
            (RecorderRealizedState::Recording, Some(since)) => {
                u64::try_from(now.saturating_duration_since(since).as_millis()).unwrap_or(u64::MAX)
            }
            _ => 0,
        };
        RecorderStatusSample {
            recorder: WireRecorderSlot::new(u8::try_from(slot + 1).unwrap_or(1))
                .unwrap_or(WireRecorderSlot::FIRST),
            realized,
            elapsed_ms,
            failure: (realized == RecorderRealizedState::Failed).then(|| {
                status
                    .failure
                    .unwrap_or(OutputFailure::new(OutputFailureCode::Unknown))
            }),
        }
    }

    /// Builds the next record when [`RECORDER_STATUS_INTERVAL`] has passed
    /// since the last one; otherwise only keeps the bookkeeping current.
    pub(crate) fn poll(
        &mut self,
        server: &ServerIdentity,
        views: [SlotView; 2],
        now: Instant,
    ) -> Option<RecorderStatusMessage> {
        let samples: Vec<_> = views
            .into_iter()
            .enumerate()
            .map(|(slot, view)| self.sample(slot, view, now))
            .collect();
        if self
            .built_at
            .is_some_and(|built| now.saturating_duration_since(built) < RECORDER_STATUS_INTERVAL)
        {
            return None;
        }
        self.built_at = Some(now);
        self.sequence = self.sequence.checked_add(1)?;
        Some(RecorderStatusMessage {
            server: server.clone(),
            sequence: self.sequence,
            samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> ServerIdentity {
        ServerIdentity {
            engine_id: "engine".into(),
            project_id: "project".into(),
            state_epoch: 1,
            log_id: "log".into(),
        }
    }

    #[test]
    fn slots_report_starting_recording_failed_and_off() {
        let mut tracker = RecorderStatusTracker::default();
        let start = Instant::now();
        let first = tracker
            .poll(
                &server(),
                [SlotView::Settling, SlotView::Unconfigured],
                start,
            )
            .unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(first.samples[0].realized, RecorderRealizedState::Starting);
        assert_eq!(
            first.samples[1].realized,
            RecorderRealizedState::Unavailable
        );
        // Within the interval nothing is built, but the run starts counting.
        let ready = start + Duration::from_millis(100);
        assert!(
            tracker
                .poll(&server(), [SlotView::Running, SlotView::Off], ready)
                .is_none()
        );
        let later = ready + Duration::from_millis(1_500);
        let recording = tracker
            .poll(&server(), [SlotView::Running, SlotView::Off], later)
            .unwrap();
        assert_eq!(recording.sequence, 2);
        assert_eq!(
            recording.samples[0].realized,
            RecorderRealizedState::Recording
        );
        assert_eq!(recording.samples[0].elapsed_ms, 1_500);
        assert_eq!(recording.samples[1].realized, RecorderRealizedState::Off);
        // A rotation's next segment keeps the run (and its elapsed time).
        let rotating = tracker
            .poll(
                &server(),
                [SlotView::Settling, SlotView::Off],
                later + Duration::from_millis(500),
            )
            .unwrap();
        assert_eq!(
            rotating.samples[0].realized,
            RecorderRealizedState::Recording
        );
        assert_eq!(rotating.samples[0].elapsed_ms, 2_000);
        // A capture failure, then the slot is released: still failed.
        let failed_at = later + Duration::from_secs(1);
        let failed = tracker
            .poll(
                &server(),
                [
                    SlotView::Failed(OutputFailure::new(OutputFailureCode::CaptureFailed)),
                    SlotView::Off,
                ],
                failed_at,
            )
            .unwrap();
        assert_eq!(failed.samples[0].realized, RecorderRealizedState::Failed);
        assert_eq!(failed.samples[0].elapsed_ms, 0);
        assert_eq!(
            failed.samples[0].failure,
            Some(OutputFailure::new(OutputFailureCode::CaptureFailed))
        );
        tracker.failed(1, OutputFailure::new(OutputFailureCode::FolderNotWritable));
        let released = tracker
            .poll(
                &server(),
                [SlotView::Off, SlotView::Off],
                failed_at + RECORDER_STATUS_INTERVAL,
            )
            .unwrap();
        assert_eq!(released.samples[0].realized, RecorderRealizedState::Failed);
        assert_eq!(
            released.samples[1].failure,
            Some(OutputFailure::new(OutputFailureCode::FolderNotWritable))
        );
        // A new start clears the failure; a stop reports off.
        tracker.started(0);
        tracker.stopped(1);
        let cleared = tracker
            .poll(
                &server(),
                [SlotView::Settling, SlotView::Off],
                failed_at + 2 * RECORDER_STATUS_INTERVAL,
            )
            .unwrap();
        assert_eq!(cleared.samples[0].realized, RecorderRealizedState::Starting);
        assert_eq!(cleared.samples[1].realized, RecorderRealizedState::Off);
        for message in [first, recording, rotating, failed, released, cleared] {
            let line = fm_protocol::encode_line(&fm_protocol::WireMessage::RecorderStatus(
                message.clone(),
            ))
            .unwrap();
            assert_eq!(
                fm_protocol::decode_line(&line).unwrap(),
                fm_protocol::WireMessage::RecorderStatus(message)
            );
        }
    }
}
