//! Realized program recorder status for sessions (Protocol 2.66).
//!
//! The native daemon realizes each recorder slot's desired flag on its own
//! (open worker, mux-readiness barrier, rotations, capture failures). This
//! module turns what each slot is doing into the lossy `recorder_status`
//! record: `starting` while a segment opens, `recording` with the time since
//! the slot's first segment became ready (kept across rotations), and
//! `failed` with a sanitized short code latched from the failure notice until
//! the next start or stop. Records are built at most every
//! [`RECORDER_STATUS_INTERVAL`], so the scheduler hands peers a bounded,
//! latest-wins feed.

use std::time::{Duration, Instant};

use fm_protocol::{
    MAX_STREAM_DETAIL_BYTES, RecorderRealizedState, RecorderStatusMessage, RecorderStatusSample,
    ServerIdentity, WireRecorderSlot,
};

/// How often a `recorder_status` record is built at most.
pub(crate) const RECORDER_STATUS_INTERVAL: Duration = Duration::from_millis(250);
/// Longest kept token of a failure code.
const MAX_FAILURE_TOKEN_BYTES: usize = 40;
/// The code a failure without a recognizable leading token reports.
const UNKNOWN_FAILURE: &str = "recorder_failure";

/// What one recorder slot is doing right now, as the daemon sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SlotView<'a> {
    /// No output is configured for the slot.
    Unconfigured,
    /// Configured, no segment open.
    Off,
    /// A segment is opening or awaiting mux readiness (a rotation's next
    /// segment too, which keeps the run going).
    Settling,
    /// A segment is open and writing.
    Running,
    /// The open segment's capture failed with this notice.
    Failed(&'a str),
}

#[derive(Debug, Default)]
struct SlotStatus {
    /// When the slot's current run first reached `recording`.
    recording_since: Option<Instant>,
    /// The latched failure code of the slot's last attempt.
    failure: Option<String>,
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

    /// `slot` lost its segment to `notice` (a `start:`, `startup:`,
    /// `rotate:`, ... failure); the slot reports `failed` until the next start
    /// or stop.
    pub(crate) fn failed(&mut self, slot: usize, notice: &str) {
        if let Some(status) = self.slots.get_mut(slot) {
            status.recording_since = None;
            status.failure = Some(failure_code(notice));
        }
    }

    /// The sample `slot` reports for `view` at `now`.
    fn sample(&mut self, slot: usize, view: SlotView<'_>, now: Instant) -> RecorderStatusSample {
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
            SlotView::Failed(notice) => {
                status.recording_since = None;
                status.failure = Some(failure_code(notice));
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
            recorder: WireRecorderSlot::new(u8::try_from(slot + 1).unwrap_or(u8::MAX))
                .expect("two recorder slots"),
            realized,
            elapsed_ms,
            failure: (realized == RecorderRealizedState::Failed).then(|| {
                status
                    .failure
                    .clone()
                    .unwrap_or_else(|| UNKNOWN_FAILURE.to_owned())
            }),
        }
    }

    /// Builds the next record when [`RECORDER_STATUS_INTERVAL`] has passed
    /// since the last one; otherwise only keeps the bookkeeping current.
    pub(crate) fn poll(
        &mut self,
        server: &ServerIdentity,
        views: [SlotView<'_>; 2],
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

/// A short, path-free code for a recorder failure notice: its leading
/// `:`-separated tokens while each is a plain identifier (at most
/// [`MAX_FAILURE_TOKEN_BYTES`] ASCII letters, digits, or `_`), so
/// `capture:missing_output` stays whole and `start:<os error on a path>`
/// becomes `start`.
pub(crate) fn failure_code(notice: &str) -> String {
    let mut code = String::new();
    for token in notice.split(':') {
        let plain = !token.is_empty()
            && token.len() <= MAX_FAILURE_TOKEN_BYTES
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        let separator = usize::from(!code.is_empty());
        if !plain || code.len() + separator + token.len() > MAX_STREAM_DETAIL_BYTES {
            break;
        }
        if separator == 1 {
            code.push(':');
        }
        code.push_str(token);
    }
    if code.is_empty() {
        UNKNOWN_FAILURE.to_owned()
    } else {
        code
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
    fn failure_codes_keep_plain_tokens_and_drop_paths() {
        for (notice, code) in [
            ("capture:missing_output", "capture:missing_output"),
            ("enqueue:QueueFull", "enqueue:QueueFull"),
            (
                "startup:mux_readiness_timeout",
                "startup:mux_readiness_timeout",
            ),
            ("start:No such file or directory (/srv/show/p.mp4)", "start"),
            ("rotate:/srv/secret/path", "rotate"),
            ("backend:Some(Failed)", "backend"),
            ("/abs/path:boom", "recorder_failure"),
            ("", "recorder_failure"),
        ] {
            assert_eq!(failure_code(notice), code, "{notice}");
        }
        let long = format!("a:{}:{}", "b".repeat(40), "c".repeat(40));
        assert!(failure_code(&long).len() <= MAX_STREAM_DETAIL_BYTES);
        assert_eq!(failure_code(&long), format!("a:{}", "b".repeat(40)));
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
                [SlotView::Failed("capture:missing_output"), SlotView::Off],
                failed_at,
            )
            .unwrap();
        assert_eq!(failed.samples[0].realized, RecorderRealizedState::Failed);
        assert_eq!(failed.samples[0].elapsed_ms, 0);
        assert_eq!(
            failed.samples[0].failure.as_deref(),
            Some("capture:missing_output")
        );
        tracker.failed(1, "start:/x/y: denied");
        let released = tracker
            .poll(
                &server(),
                [SlotView::Off, SlotView::Off],
                failed_at + RECORDER_STATUS_INTERVAL,
            )
            .unwrap();
        assert_eq!(released.samples[0].realized, RecorderRealizedState::Failed);
        assert_eq!(released.samples[1].failure.as_deref(), Some("start"));
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
