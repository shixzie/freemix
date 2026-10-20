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
//! drop/repeat counts. There is no protocol event for it (yet): the record
//! is the operator's and the test suite's telemetry.

use std::time::{Duration, Instant};

use fm_audio::SourceSyncState;
use fm_codec_ffmpeg::network_input::LiveSourceTelemetry;
use fm_types::InputId;

/// How often a steadily running source is reported.
pub(crate) const SYNC_INTERVAL: Duration = Duration::from_secs(60);

/// The kind of live source a record names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceKind {
    Capture,
    Stream,
    SystemAudio,
}

impl SourceKind {
    const fn token(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Stream => "stream",
            Self::SystemAudio => "system_audio",
        }
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
    use fm_types::{FrameRate, SampleRate};

    use super::*;

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
