//! Clock drift records of the live sources a native daemon ingests
//! (AU-004).
//!
//! Every capture input, network stream, and system audio input runs on its
//! own clock; its receiver converts its audio into the engine clock with a
//! drift-correcting loop and keeps its pictures following that audio (see
//! `fm_codec_ffmpeg::network_input`). Each source's loop is reported on
//! standard error as a `FREEMIXD_SOURCE_SYNC` record: once when it first
//! runs, at once whenever it hard-resyncs or underruns, and otherwise every
//! [`SYNC_INTERVAL`]. A record carries the estimated drift and the applied
//! correction in ppm, the audio backlog against its target in
//! milliseconds, and the resync, underrun, overrun, and picture
//! drop/repeat counts.
//!
//! The same loops reach clients as `source_sync_status` records (Protocol
//! 2.76): [`SourceSyncTracker`] builds one at most every
//! [`SOURCE_SYNC_STATUS_INTERVAL`] while any live source runs (and once
//! more, empty, after the last one stops), and `diagnostics_response`
//! carries the current samples.

use std::time::{Duration, Instant};

use fm_audio::SourceSyncState;
use fm_codec_ffmpeg::network_input::LiveSourceTelemetry;
use fm_protocol::{
    MAX_SOURCE_SYNC_MPPM, MAX_SOURCE_SYNC_SAMPLES, ServerIdentity, SourceClockState,
    SourceSyncSample, SourceSyncStatusMessage, WireInputId,
};
use fm_types::InputId;

/// How often a steadily running source is reported.
pub(crate) const SYNC_INTERVAL: Duration = Duration::from_secs(60);

/// The shortest spacing of two `source_sync_status` records.
pub(crate) const SOURCE_SYNC_STATUS_INTERVAL: Duration = Duration::from_secs(1);

/// The kind of live source a record names.
pub(crate) use fm_protocol::SourceSyncKind as SourceKind;

/// One source's loop as a wire sample; `None` when the telemetry cannot
/// keep the wire contract (no rate yet).
pub(crate) fn wire_sample(
    kind: SourceKind,
    input: InputId,
    telemetry: &LiveSourceTelemetry,
) -> Option<SourceSyncSample> {
    let sync = &telemetry.sync;
    #[allow(clippy::cast_possible_truncation)] // clamped to the wire bound first
    let mppm = |ppm: f64| {
        let limit = MAX_SOURCE_SYNC_MPPM as f64;
        if ppm.is_finite() {
            (ppm * 1_000.0).round().clamp(-limit, limit) as i64
        } else {
            0
        }
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // non-negative, finite
    let micros = |ms: f64| {
        if ms.is_finite() && ms > 0.0 {
            (ms * 1_000.0).round().min(u64::MAX as f64) as u64
        } else {
            0
        }
    };
    let sample = SourceSyncSample {
        input: WireInputId::from_domain(input),
        kind,
        state: match sync.state {
            SourceSyncState::Priming => SourceClockState::Priming,
            SourceSyncState::Running => SourceClockState::Running,
            SourceSyncState::Recovering => SourceClockState::Recovering,
        },
        input_rate_hz: sync.input_rate_hz,
        output_rate_hz: sync.output_rate_hz,
        drift_mppm: mppm(sync.estimated_drift_ppm),
        correction_mppm: mppm(sync.applied_correction_ppm),
        buffer_us: micros(telemetry.buffer_ms()),
        target_us: micros(telemetry.target_ms()),
        resyncs: sync.resyncs,
        underruns: sync.underruns,
        overruns: sync.overruns,
        picture_drops: telemetry.picture_drops,
        picture_repeats: telemetry.picture_repeats,
        restarts: telemetry.restarts,
    };
    sample.is_valid().then_some(sample)
}

/// Orders samples by input and keeps the wire bound.
pub(crate) fn wire_samples(mut samples: Vec<SourceSyncSample>) -> Vec<SourceSyncSample> {
    samples.sort_by_key(|sample| sample.input);
    samples.dedup_by_key(|sample| sample.input);
    samples.truncate(MAX_SOURCE_SYNC_SAMPLES);
    samples
}

/// Builds the lossy `source_sync_status` records (Protocol 2.76).
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceSyncTracker {
    sequence: u64,
    built_at: Option<Instant>,
    /// The last record carried at least one source.
    reported_sources: bool,
}

impl SourceSyncTracker {
    /// The next record when one is due at `now`: at most every
    /// [`SOURCE_SYNC_STATUS_INTERVAL`] while any source runs, and one empty
    /// record after the last source stops.
    pub(crate) fn poll(
        &mut self,
        server: &ServerIdentity,
        samples: impl FnOnce() -> Vec<SourceSyncSample>,
        now: Instant,
    ) -> Option<SourceSyncStatusMessage> {
        if self
            .built_at
            .is_some_and(|built| now.saturating_duration_since(built) < SOURCE_SYNC_STATUS_INTERVAL)
        {
            return None;
        }
        let sources = wire_samples(samples());
        if sources.is_empty() && !self.reported_sources {
            return None;
        }
        self.built_at = Some(now);
        self.reported_sources = !sources.is_empty();
        self.sequence = self.sequence.checked_add(1)?;
        Some(SourceSyncStatusMessage {
            server: server.clone(),
            sequence: self.sequence,
            sources,
        })
    }
}

/// When one source was last reported, and its fault counts then.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SyncLog {
    last: Option<(Instant, u64, u64)>,
}

impl SyncLog {
    /// Logs `telemetry` when due at `now`; returns the record it logged.
    pub(crate) fn report(
        &mut self,
        kind: SourceKind,
        input: InputId,
        telemetry: Option<&LiveSourceTelemetry>,
        now: Instant,
    ) -> Option<String> {
        let telemetry = telemetry?;
        let faults = (telemetry.hard_resyncs(), telemetry.sync.underruns);
        let due = match self.last {
            None => telemetry.sync.state == SourceSyncState::Running,
            Some((at, resyncs, underruns)) => {
                (resyncs, underruns) != faults || now.saturating_duration_since(at) >= SYNC_INTERVAL
            }
        };
        if !due {
            return None;
        }
        self.last = Some((now, faults.0, faults.1));
        let line = record(kind, input, telemetry);
        eprintln!("{line}");
        Some(line)
    }
}

/// One `FREEMIXD_SOURCE_SYNC` record.
pub(crate) fn record(kind: SourceKind, input: InputId, telemetry: &LiveSourceTelemetry) -> String {
    let sync = &telemetry.sync;
    format!(
        "FREEMIXD_SOURCE_SYNC\tv=1\tinput={input}\tkind={}\tstate={}\tdrift_ppm={:+.2}\tcorrection_ppm={:+.2}\tbuffer_ms={:.1}\ttarget_ms={:.1}\tresyncs={}\tunderruns={}\toverruns={}\tpicture_drops={}\tpicture_repeats={}\trestarts={}",
        kind.token(),
        match sync.state {
            SourceSyncState::Priming => "priming",
            SourceSyncState::Running => "running",
            SourceSyncState::Recovering => "recovering",
        },
        sync.estimated_drift_ppm,
        sync.applied_correction_ppm,
        telemetry.buffer_ms(),
        telemetry.target_ms(),
        sync.resyncs,
        sync.underruns,
        sync.overruns,
        telemetry.picture_drops,
        telemetry.picture_repeats,
        telemetry.restarts,
    )
}

#[cfg(test)]
mod tests {
    use fm_audio::{LiveSourceClock, LiveSourceConfig};
    use fm_protocol::SourceSyncKind;
    use fm_types::{FrameRate, SampleRate};

    use super::*;

    fn server() -> ServerIdentity {
        ServerIdentity {
            engine_id: "engine".into(),
            project_id: "project".into(),
            state_epoch: 1,
            log_id: "log".into(),
        }
    }

    fn input(id: u128) -> InputId {
        InputId::new(core::num::NonZeroU128::new(id).unwrap())
    }

    #[test]
    fn a_running_source_becomes_an_exact_wire_sample() {
        let mut telemetry = running_clock().telemetry();
        telemetry.sync.estimated_drift_ppm = -12.3456;
        telemetry.sync.applied_correction_ppm = 7.0004;
        telemetry.sync.underruns = 2;
        telemetry.picture_repeats = 3;
        telemetry.restarts = 1;
        let sample = wire_sample(SourceKind::Stream, input(7), &telemetry).unwrap();
        assert_eq!(sample.input, WireInputId::from_domain(input(7)));
        assert_eq!(sample.kind, SourceSyncKind::Stream);
        assert_eq!(sample.state, SourceClockState::Running);
        assert_eq!(
            (sample.input_rate_hz, sample.output_rate_hz),
            (48_000, 48_000)
        );
        assert_eq!(sample.drift_mppm, -12_346);
        assert_eq!(sample.correction_mppm, 7_000);
        assert_eq!(sample.target_us, 80_000);
        assert_eq!(
            sample.buffer_us,
            (telemetry.buffer_ms() * 1_000.0).round() as u64
        );
        assert_eq!(
            (sample.underruns, sample.picture_repeats, sample.restarts),
            (2, 3, 1)
        );
        telemetry.sync.estimated_drift_ppm = f64::INFINITY;
        telemetry.sync.applied_correction_ppm = 1e12;
        let clamped = wire_sample(SourceKind::Stream, input(7), &telemetry).unwrap();
        assert_eq!(clamped.drift_mppm, 0);
        assert_eq!(clamped.correction_mppm, MAX_SOURCE_SYNC_MPPM);
        telemetry.sync.input_rate_hz = 0;
        assert!(wire_sample(SourceKind::Stream, input(7), &telemetry).is_none());
    }

    #[test]
    fn status_records_are_paced_ordered_and_end_with_one_empty_record() {
        let telemetry = running_clock().telemetry();
        let samples = || {
            vec![
                wire_sample(SourceKind::Capture, input(9), &telemetry).unwrap(),
                wire_sample(SourceKind::Stream, input(3), &telemetry).unwrap(),
            ]
        };
        let mut tracker = SourceSyncTracker::default();
        let start = Instant::now();
        assert!(tracker.poll(&server(), Vec::new, start).is_none());
        let first = tracker.poll(&server(), samples, start).unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(
            first.sources.iter().map(|s| s.input).collect::<Vec<_>>(),
            [
                WireInputId::from_domain(input(3)),
                WireInputId::from_domain(input(9))
            ]
        );
        assert!(
            tracker
                .poll(&server(), samples, start + SOURCE_SYNC_STATUS_INTERVAL / 2)
                .is_none()
        );
        let second = tracker
            .poll(&server(), samples, start + SOURCE_SYNC_STATUS_INTERVAL)
            .unwrap();
        assert_eq!(second.sequence, 2);
        let empty = tracker
            .poll(&server(), Vec::new, start + SOURCE_SYNC_STATUS_INTERVAL * 2)
            .unwrap();
        assert!(empty.sources.is_empty());
        assert_eq!(empty.sequence, 3);
        assert!(
            tracker
                .poll(&server(), Vec::new, start + SOURCE_SYNC_STATUS_INTERVAL * 3)
                .is_none()
        );
        let many = (1..=100)
            .map(|id| wire_sample(SourceKind::Stream, input(id), &telemetry).unwrap())
            .collect();
        assert_eq!(wire_samples(many).len(), MAX_SOURCE_SYNC_SAMPLES);
    }

    fn running_clock() -> LiveSourceClock {
        let mut clock = LiveSourceClock::new(LiveSourceConfig::new(
            SampleRate::new(48_000).unwrap(),
            2,
            FrameRate::new(25, 1).unwrap(),
            2,
        ))
        .unwrap();
        let block = vec![0.0_f32; 1_920];
        let mut out = vec![vec![0.0_f32; 1_920]; 2];
        for frame in 0..10_u32 {
            let at = f64::from(frame) / 25.0;
            clock.push(&[&block, &block], at).unwrap();
            let mut planes: Vec<&mut [f32]> = out.iter_mut().map(Vec::as_mut_slice).collect();
            clock.pull(&mut planes, None, at + 0.02).unwrap();
        }
        clock
    }

    #[test]
    fn a_running_source_is_reported_once_then_every_interval_and_on_faults() {
        let input = InputId::new(core::num::NonZeroU128::new(7).unwrap());
        let clock = running_clock();
        let telemetry = clock.telemetry();
        assert_eq!(telemetry.sync.state, SourceSyncState::Running);
        let mut log = SyncLog::default();
        let start = Instant::now();
        let line = log
            .report(SourceKind::Stream, input, Some(&telemetry), start)
            .unwrap();
        assert!(line.starts_with("FREEMIXD_SOURCE_SYNC\tv=1\tinput=7\tkind=stream\tstate=running\tdrift_ppm=+0.00\tcorrection_ppm=+0.00\tbuffer_ms="));
        assert!(line.contains("\ttarget_ms=80.0\tresyncs=0\tunderruns=0\toverruns=0\tpicture_drops=0\tpicture_repeats=0\trestarts=0"));
        assert!(
            log.report(
                SourceKind::Stream,
                input,
                Some(&telemetry),
                start + SYNC_INTERVAL / 2
            )
            .is_none()
        );
        let mut faulted = telemetry;
        faulted.sync.underruns += 1;
        assert!(
            log.report(
                SourceKind::Stream,
                input,
                Some(&faulted),
                start + SYNC_INTERVAL / 2
            )
            .is_some()
        );
        assert!(
            log.report(
                SourceKind::Stream,
                input,
                Some(&faulted),
                start + SYNC_INTERVAL
            )
            .is_none()
        );
        assert!(
            log.report(
                SourceKind::Stream,
                input,
                Some(&faulted),
                start + SYNC_INTERVAL * 3 / 2
            )
            .is_some()
        );
        assert!(log.report(SourceKind::Stream, input, None, start).is_none());
    }

    #[test]
    fn a_priming_source_waits_for_its_first_record() {
        let clock = LiveSourceClock::new(LiveSourceConfig::new(
            SampleRate::new(48_000).unwrap(),
            2,
            FrameRate::new(25, 1).unwrap(),
            2,
        ))
        .unwrap();
        let mut log = SyncLog::default();
        assert!(
            log.report(
                SourceKind::Capture,
                InputId::new(core::num::NonZeroU128::new(1).unwrap()),
                Some(&clock.telemetry()),
                Instant::now()
            )
            .is_none()
        );
    }
}
