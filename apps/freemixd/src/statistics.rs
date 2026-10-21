//! The daemon's named statistics (`RC-015`).
//!
//! Both daemons own one [`DaemonStatistics`] for the run. Once per
//! [`STATISTICS_INTERVAL`] the session loop samples it: the control plane's
//! subscriber count and revision on either daemon, and on a native daemon the
//! frame loop's telemetry drained since the previous sample (host frame
//! lateness and GPU pass times as histograms, audio, recorder, stream,
//! monitor, camera, and disk counters and gauges). Everything lands in one
//! bounded [`MetricRegistry`] under the stable names in [`names`].
//!
//! Each sample also measures utilization:
//!
//! - CPU: the daemon process's CPU time over the wall time since the
//!   previous sample, as a share of every logical CPU (see [`crate::cpu`]);
//!   unavailable off Linux.
//! - GPU: no vendor utilization API is used. The proxy is the GPU time the
//!   native compositor's timestamped fullscreen render passes took over the
//!   wall time since the previous sample, so it counts only those passes
//!   (not uploads, readbacks, encoders, or other processes) and reads low
//!   when timestamp samples are dropped. It is unavailable on the simulated
//!   daemon and on adapters without timestamp queries.
//!
//! The registry is an additional view: the `FREEMIXD_TELEMETRY` record the
//! native daemon prints at shutdown keeps its own whole-run aggregation and
//! fields unchanged.

use std::time::{Duration, Instant};

use fm_observability::{
    HistogramBuckets, LabelSet, MetricDescriptor, MetricId, MetricRegistry, RegistryLimits,
    SeriesKey, SeriesValue, StatisticsError, StatisticsSnapshot,
};

/// The shortest spacing of two statistics samples.
pub(crate) const STATISTICS_INTERVAL: Duration = Duration::from_secs(1);

/// The most frame lateness or GPU pass observations the native frame loop
/// buffers between two samples; later ones are counted as dropped.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
pub(crate) const MAX_PENDING_OBSERVATIONS: usize = 1_024;

/// Stable metric names.
pub(crate) mod names {
    /// The daemon process's CPU utilization, `0..=1` of every logical CPU.
    pub(crate) const CPU_UTILIZATION: &str = "cpu.process_utilization";
    /// Timestamped GPU render-pass time per wall time, `0..=1` (a proxy).
    pub(crate) const GPU_UTILIZATION: &str = "gpu.utilization";
    /// How late the host woke for each realized frame, in milliseconds.
    pub(crate) const FRAME_LATENESS_MS: &str = "frame.host_lateness_ms";
    /// Frames the native frame loop realized.
    pub(crate) const FRAMES_REALIZED: &str = "frame.realized_total";
    /// Realized frames whose host lateness reached half a frame interval.
    pub(crate) const FRAMES_LATE: &str = "frame.late_total";
    /// The share of the frames realized since the previous sample that were
    /// late, `0..=1`.
    pub(crate) const FRAME_LATE_RATIO: &str = "frame.late_ratio";
    /// Pictures discarded on the way to an output: stream A/V pairs refused
    /// at admission plus display output frames dropped.
    pub(crate) const FRAMES_DROPPED: &str = "frame.dropped_total";
    /// Lateness or GPU observations the frame loop could not buffer.
    pub(crate) const OBSERVATIONS_DROPPED: &str = "statistics.observations_dropped_total";
    /// One GPU pass, in milliseconds.
    pub(crate) const GPU_PASS_MS: &str = "gpu.pass_ms";
    /// Audio blocks waiting in the master sink.
    pub(crate) const AUDIO_SINK_DEPTH: &str = "audio.sink_depth";
    /// Audio blocks the master sink dropped.
    pub(crate) const AUDIO_SINK_DROPPED: &str = "audio.sink_dropped_total";
    /// Times an audio source stalled the master mix.
    pub(crate) const AUDIO_SOURCE_STALLS: &str = "audio.source_stalls_total";
    /// Bytes of audio the master mix retains.
    pub(crate) const AUDIO_RETAINED_BYTES: &str = "audio.retained_bytes";
    /// A/V pairs queued across the running recorders.
    pub(crate) const RECORDER_OUTSTANDING_PAIRS: &str = "recorder.outstanding_pairs";
    /// Bytes queued across the running recorders.
    pub(crate) const RECORDER_RETAINED_BYTES: &str = "recorder.retained_bytes";
    /// A/V pairs admitted to stream targets.
    pub(crate) const STREAM_ENQUEUED_PAIRS: &str = "stream.enqueued_pairs_total";
    /// A/V pairs stream targets refused.
    pub(crate) const STREAM_DROPPED_PAIRS: &str = "stream.dropped_pairs_total";
    /// Encoded monitor frames published, per `tap`.
    pub(crate) const MONITOR_PUBLISHED: &str = "monitor.frames_published_total";
    /// Monitor frames that failed to encode, per `tap`.
    pub(crate) const MONITOR_FAILED: &str = "monitor.frames_failed_total";
    /// Monitor frames skipped while a previous one was still in flight, per
    /// `tap`.
    pub(crate) const MONITOR_SKIPPED: &str = "monitor.frames_skipped_total";
    /// Camera frames dropped by the platform or the ingest queue.
    pub(crate) const CAMERA_DROPPED: &str = "camera.frames_dropped_total";
    /// Lowest free space across the configured output filesystems.
    pub(crate) const DISK_FREE_BYTES: &str = "disk.free_bytes";
    /// Outputs (stream targets and recorder slots) that latched a failure.
    pub(crate) const OUTPUTS_FAILED: &str = "output.failed";
    /// Sessions subscribed to the control plane.
    pub(crate) const CONTROL_SUBSCRIBERS: &str = "control.subscribers";
    /// The control plane's current revision.
    pub(crate) const CONTROL_REVISION: &str = "control.revision";
}

/// Host lateness bucket edges, in milliseconds.
const LATENESS_EDGES_MS: [f64; 10] = [0.0, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 33.0, 66.0, 133.0];
/// GPU pass bucket edges, in milliseconds.
const GPU_PASS_EDGES_MS: [f64; 10] = [0.0, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 33.0, 66.0];

/// One monitor tap's whole-run totals.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TapTotals {
    pub(crate) published: u64,
    pub(crate) failed: u64,
    pub(crate) skipped: u64,
}

/// What the native frame loop observed since the previous sample (the
/// observation lists) and in total (everything else).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NativeStatisticsSample {
    /// The show's nominal frame interval, in milliseconds.
    pub(crate) frame_interval_ms: f64,
    /// Each realized frame's host lateness since the previous sample.
    pub(crate) lateness_ms: Vec<f64>,
    /// Each completed GPU pass since the previous sample.
    pub(crate) gpu_pass_ms: Vec<f64>,
    /// Whether the adapter timestamps GPU passes at all.
    pub(crate) gpu_timing: bool,
    /// Observations dropped because the buffers were full, in total.
    pub(crate) observations_dropped: u64,
    pub(crate) frames_dropped: u64,
    pub(crate) audio_sink_depth: u64,
    pub(crate) audio_sink_dropped: u64,
    pub(crate) audio_source_stalls: u64,
    pub(crate) audio_retained_bytes: u64,
    pub(crate) recorder_outstanding_pairs: u64,
    pub(crate) recorder_retained_bytes: u64,
    pub(crate) stream_enqueued_pairs: u64,
    pub(crate) stream_dropped_pairs: u64,
    /// Program then Preview.
    pub(crate) monitor: [TapTotals; 2],
    pub(crate) camera_dropped: u64,
    /// `None` until the first successful disk sample.
    pub(crate) disk_free_bytes: Option<u64>,
    /// Outputs currently latched failed.
    pub(crate) outputs_failed: u64,
}

/// What the control plane reports on either daemon.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ControlSample {
    pub(crate) subscribers: u64,
    pub(crate) revision: u64,
}

const MONITOR_TAPS: [&str; 2] = ["program", "preview"];

fn metric(name: &'static str) -> MetricId {
    MetricId::new(name).expect("static metric names keep the identifier grammar")
}

fn tap_labels(tap: &'static str) -> LabelSet {
    LabelSet::from_pairs([("tap", tap)]).expect("static tap labels are valid")
}

/// Converts a count to `f64` for a gauge; exact up to 2^53.
#[allow(clippy::cast_precision_loss)]
const fn gauge_value(value: u64) -> f64 {
    value as f64
}

/// GPU pass time over `wall_ms`, clamped to `0..=1`; `None` without wall
/// time to divide by.
fn gpu_utilization(passes_ms: &[f64], wall_ms: f64) -> Option<f64> {
    (wall_ms > 0.0).then(|| (passes_ms.iter().sum::<f64>() / wall_ms).clamp(0.0, 1.0))
}

/// The daemon's bounded metric registry and its sampling cadence.
#[derive(Debug)]
pub(crate) struct DaemonStatistics {
    origin: Instant,
    registry: MetricRegistry,
    sampled_at: Option<Instant>,
    /// Registry refusals (never fatal to the show).
    errors: u64,
    cpu: crate::cpu::CpuSampler,
    /// The latest utilization readings; `None` while unavailable.
    cpu_utilization: Option<f64>,
    gpu_utilization: Option<f64>,
}

impl DaemonStatistics {
    /// Registers every named metric.
    pub(crate) fn new(origin: Instant) -> Self {
        let mut registry =
            MetricRegistry::new(RegistryLimits::default()).expect("default limits are valid");
        let histogram = |edges: &[f64]| {
            MetricDescriptor::Histogram(
                HistogramBuckets::new(edges).expect("static bucket edges increase"),
            )
        };
        let descriptors = [
            (names::CPU_UTILIZATION, MetricDescriptor::Gauge),
            (names::GPU_UTILIZATION, MetricDescriptor::Gauge),
            (names::FRAME_LATENESS_MS, histogram(&LATENESS_EDGES_MS)),
            (names::FRAMES_REALIZED, MetricDescriptor::Counter),
            (names::FRAMES_LATE, MetricDescriptor::Counter),
            (names::FRAME_LATE_RATIO, MetricDescriptor::Gauge),
            (names::FRAMES_DROPPED, MetricDescriptor::Counter),
            (names::OBSERVATIONS_DROPPED, MetricDescriptor::Counter),
            (names::GPU_PASS_MS, histogram(&GPU_PASS_EDGES_MS)),
            (names::AUDIO_SINK_DEPTH, MetricDescriptor::Gauge),
            (names::AUDIO_SINK_DROPPED, MetricDescriptor::Counter),
            (names::AUDIO_SOURCE_STALLS, MetricDescriptor::Counter),
            (names::AUDIO_RETAINED_BYTES, MetricDescriptor::Gauge),
            (names::RECORDER_OUTSTANDING_PAIRS, MetricDescriptor::Gauge),
            (names::RECORDER_RETAINED_BYTES, MetricDescriptor::Gauge),
            (names::STREAM_ENQUEUED_PAIRS, MetricDescriptor::Counter),
            (names::STREAM_DROPPED_PAIRS, MetricDescriptor::Counter),
            (names::MONITOR_PUBLISHED, MetricDescriptor::Counter),
            (names::MONITOR_FAILED, MetricDescriptor::Counter),
            (names::MONITOR_SKIPPED, MetricDescriptor::Counter),
            (names::CAMERA_DROPPED, MetricDescriptor::Counter),
            (names::DISK_FREE_BYTES, MetricDescriptor::Gauge),
            (names::OUTPUTS_FAILED, MetricDescriptor::Gauge),
            (names::CONTROL_SUBSCRIBERS, MetricDescriptor::Gauge),
            (names::CONTROL_REVISION, MetricDescriptor::Gauge),
        ];
        for (name, descriptor) in descriptors {
            registry
                .register(metric(name), descriptor)
                .expect("the static metric set fits the default limits");
        }
        Self {
            origin,
            registry,
            sampled_at: None,
            errors: 0,
            cpu: crate::cpu::CpuSampler::new(),
            cpu_utilization: None,
            gpu_utilization: None,
        }
    }

    /// The latest process CPU utilization, `0..=1`; `None` while unavailable.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn cpu_utilization(&self) -> Option<f64> {
        self.cpu_utilization
    }

    /// The latest GPU utilization proxy, `0..=1`; `None` while unavailable.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn gpu_utilization(&self) -> Option<f64> {
        self.gpu_utilization
    }

    /// The registry every metric lives in.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn registry(&self) -> &MetricRegistry {
        &self.registry
    }

    /// The registry as `fm-statistics-v1` text for the support bundle.
    pub(crate) fn render(&self) -> String {
        StatisticsSnapshot::new(&self.registry).render()
    }

    /// Registry refusals so far.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn errors(&self) -> u64 {
        self.errors
    }

    /// Milliseconds since the daemon's statistics origin.
    pub(crate) fn millis(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.origin).as_millis()).unwrap_or(u64::MAX)
    }

    /// Whether a sample is due at `now`.
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.sampled_at
            .is_none_or(|at| now.saturating_duration_since(at) >= STATISTICS_INTERVAL)
    }

    /// Records one sample taken at `now`: the control plane's, and the
    /// native frame loop's when this is a native daemon.
    pub(crate) fn sample(
        &mut self,
        now: Instant,
        control: ControlSample,
        native: Option<NativeStatisticsSample>,
    ) {
        let cpu = self.cpu.sample(now);
        self.record_sample(now, control, native, cpu);
    }

    /// [`Self::sample`] with the CPU reading already taken.
    fn record_sample(
        &mut self,
        now: Instant,
        control: ControlSample,
        native: Option<NativeStatisticsSample>,
        cpu: Option<f64>,
    ) {
        let wall_ms = self
            .sampled_at
            .map(|at| now.saturating_duration_since(at).as_secs_f64() * 1_000.0);
        self.sampled_at = Some(now);
        let at = self.millis(now);
        self.cpu_utilization = cpu;
        if let Some(cpu) = cpu {
            self.gauge(names::CPU_UTILIZATION, at, cpu);
        }
        self.gpu_utilization = native
            .as_ref()
            .filter(|native| native.gpu_timing)
            .and_then(|native| gpu_utilization(&native.gpu_pass_ms, wall_ms?));
        if let Some(gpu) = self.gpu_utilization {
            self.gauge(names::GPU_UTILIZATION, at, gpu);
        }
        self.gauge(
            names::CONTROL_SUBSCRIBERS,
            at,
            gauge_value(control.subscribers),
        );
        self.gauge(names::CONTROL_REVISION, at, gauge_value(control.revision));
        if let Some(native) = native {
            self.record_native(at, &native);
        }
    }

    /// Frame lateness and GPU pass observations, and the late ratio.
    fn record_frames(&mut self, at: u64, native: &NativeStatisticsSample) {
        let half_interval = native.frame_interval_ms / 2.0;
        let late = native
            .lateness_ms
            .iter()
            .filter(|&&lateness| lateness >= half_interval)
            .count();
        let realized = native.lateness_ms.len();
        for &lateness in &native.lateness_ms {
            self.observe(names::FRAME_LATENESS_MS, at, lateness);
        }
        for &pass in &native.gpu_pass_ms {
            self.observe(names::GPU_PASS_MS, at, pass);
        }
        self.add(
            names::FRAMES_REALIZED,
            at,
            u64::try_from(realized).unwrap_or(u64::MAX),
        );
        self.add(
            names::FRAMES_LATE,
            at,
            u64::try_from(late).unwrap_or(u64::MAX),
        );
        // A pass that realized no frame (the loop waited on its sources)
        // leaves the ratio where it was rather than reading as "all on time".
        if realized > 0 {
            #[allow(clippy::cast_precision_loss)] // bounded by the buffer
            let ratio = late as f64 / realized as f64;
            self.gauge(names::FRAME_LATE_RATIO, at, ratio);
        }
    }

    fn record_native(&mut self, at: u64, native: &NativeStatisticsSample) {
        self.record_frames(at, native);
        let unlabelled = LabelSet::new();
        self.total(
            names::OBSERVATIONS_DROPPED,
            &unlabelled,
            at,
            native.observations_dropped,
        );
        self.total(
            names::FRAMES_DROPPED,
            &unlabelled,
            at,
            native.frames_dropped,
        );
        self.gauge(
            names::AUDIO_SINK_DEPTH,
            at,
            gauge_value(native.audio_sink_depth),
        );
        self.total(
            names::AUDIO_SINK_DROPPED,
            &unlabelled,
            at,
            native.audio_sink_dropped,
        );
        self.total(
            names::AUDIO_SOURCE_STALLS,
            &unlabelled,
            at,
            native.audio_source_stalls,
        );
        self.gauge(
            names::AUDIO_RETAINED_BYTES,
            at,
            gauge_value(native.audio_retained_bytes),
        );
        self.gauge(
            names::RECORDER_OUTSTANDING_PAIRS,
            at,
            gauge_value(native.recorder_outstanding_pairs),
        );
        self.gauge(
            names::RECORDER_RETAINED_BYTES,
            at,
            gauge_value(native.recorder_retained_bytes),
        );
        self.total(
            names::STREAM_ENQUEUED_PAIRS,
            &unlabelled,
            at,
            native.stream_enqueued_pairs,
        );
        self.total(
            names::STREAM_DROPPED_PAIRS,
            &unlabelled,
            at,
            native.stream_dropped_pairs,
        );
        for (tap, totals) in MONITOR_TAPS.into_iter().zip(native.monitor) {
            let labels = tap_labels(tap);
            self.total(names::MONITOR_PUBLISHED, &labels, at, totals.published);
            self.total(names::MONITOR_FAILED, &labels, at, totals.failed);
            self.total(names::MONITOR_SKIPPED, &labels, at, totals.skipped);
        }
        self.total(
            names::CAMERA_DROPPED,
            &unlabelled,
            at,
            native.camera_dropped,
        );
        if let Some(free) = native.disk_free_bytes {
            self.gauge(names::DISK_FREE_BYTES, at, gauge_value(free));
        }
        self.gauge(
            names::OUTPUTS_FAILED,
            at,
            gauge_value(native.outputs_failed),
        );
    }

    /// The current value of an unlabelled counter or gauge.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn value(&self, name: &'static str) -> Option<f64> {
        self.value_with(name, &LabelSet::new())
    }

    fn value_with(&self, name: &'static str, labels: &LabelSet) -> Option<f64> {
        let key = SeriesKey::new(metric(name), labels.clone());
        match &self.registry.get(&key)?.value {
            SeriesValue::Counter(total) => Some(gauge_value(*total)),
            SeriesValue::Gauge(value) => Some(*value),
            SeriesValue::Histogram(_) => None,
        }
    }

    fn record(&mut self, result: Result<(), StatisticsError>) {
        if result.is_err() {
            self.errors = self.errors.saturating_add(1);
        }
    }

    fn gauge(&mut self, name: &'static str, at: u64, value: f64) {
        let result = self
            .registry
            .set_gauge(&metric(name), &LabelSet::new(), at, value);
        self.record(result);
    }

    fn observe(&mut self, name: &'static str, at: u64, value: f64) {
        let result = self
            .registry
            .observe_histogram(&metric(name), &LabelSet::new(), at, value);
        self.record(result);
    }

    fn add(&mut self, name: &'static str, at: u64, amount: u64) {
        let result = self
            .registry
            .increment_counter(&metric(name), &LabelSet::new(), at, amount);
        self.record(result);
    }

    /// Advances a counter to a whole-run `total`. A total below the
    /// counter (a source that restarted its count) adds nothing; the counter
    /// still updates, so rate rules see a zero rate.
    fn total(&mut self, name: &'static str, labels: &LabelSet, at: u64, total: u64) {
        let current = self.value_with(name, labels).map_or(0, |value| {
            // Counters convert exactly well past any count reachable here.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let value = value as u64;
            value
        });
        let result = self.registry.increment_counter(
            &metric(name),
            labels,
            at,
            total.saturating_sub(current),
        );
        self.record(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(lateness_ms: Vec<f64>) -> NativeStatisticsSample {
        NativeStatisticsSample {
            frame_interval_ms: 1_000.0 / 30.0,
            lateness_ms,
            gpu_pass_ms: vec![2.0, 3.0],
            gpu_timing: true,
            stream_enqueued_pairs: 10,
            stream_dropped_pairs: 2,
            monitor: [
                TapTotals {
                    published: 5,
                    failed: 1,
                    skipped: 0,
                },
                TapTotals::default(),
            ],
            disk_free_bytes: Some(4_096),
            ..NativeStatisticsSample::default()
        }
    }

    #[test]
    fn samples_at_most_once_per_interval() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        assert!(statistics.due(origin));
        statistics.record_sample(origin, ControlSample::default(), None, None);
        assert!(!statistics.due(origin + Duration::from_millis(999)));
        assert!(statistics.due(origin + STATISTICS_INTERVAL));
    }

    #[test]
    fn gpu_proxy_is_pass_time_over_wall_time() {
        assert_eq!(gpu_utilization(&[4.0, 6.0], 100.0), Some(0.1));
        assert_eq!(gpu_utilization(&[400.0], 100.0), Some(1.0));
        assert_eq!(gpu_utilization(&[], 100.0), Some(0.0));
        assert_eq!(gpu_utilization(&[1.0], 0.0), None);

        // Adapters without timestamps report no proxy.
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let mut sample = native(vec![1.0]);
        sample.gpu_timing = false;
        statistics.record_sample(origin, ControlSample::default(), Some(sample.clone()), None);
        statistics.record_sample(
            origin + STATISTICS_INTERVAL,
            ControlSample::default(),
            Some(sample),
            None,
        );
        assert_eq!(statistics.gpu_utilization(), None);
    }

    #[test]
    fn simulated_daemon_records_only_the_control_plane() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        statistics.record_sample(
            origin,
            ControlSample {
                subscribers: 3,
                revision: 42,
            },
            None,
            Some(0.125),
        );
        assert_eq!(statistics.value(names::CPU_UTILIZATION), Some(0.125));
        assert_eq!(statistics.cpu_utilization(), Some(0.125));
        // No GPU proxy without the native compositor.
        assert_eq!(statistics.gpu_utilization(), None);
        assert_eq!(statistics.value(names::GPU_UTILIZATION), None);
        assert_eq!(statistics.value(names::CONTROL_SUBSCRIBERS), Some(3.0));
        assert_eq!(statistics.value(names::CONTROL_REVISION), Some(42.0));
        assert_eq!(statistics.value(names::FRAMES_REALIZED), None);
        assert_eq!(statistics.value(names::DISK_FREE_BYTES), None);
        assert_eq!(statistics.errors(), 0);
    }

    #[test]
    fn native_telemetry_lands_under_named_metrics() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        // One of four frames woke more than half a 30 fps interval late.
        statistics.record_sample(
            origin,
            ControlSample::default(),
            Some(native(vec![0.5, 1.0, 20.0, 2.0])),
            None,
        );
        // The first sample has no wall time to measure the GPU proxy over.
        assert_eq!(statistics.gpu_utilization(), None);
        assert_eq!(statistics.value(names::FRAMES_REALIZED), Some(4.0));
        assert_eq!(statistics.value(names::FRAMES_LATE), Some(1.0));
        assert_eq!(statistics.value(names::FRAME_LATE_RATIO), Some(0.25));
        assert_eq!(statistics.value(names::STREAM_DROPPED_PAIRS), Some(2.0));
        assert_eq!(statistics.value(names::DISK_FREE_BYTES), Some(4_096.0));
        assert_eq!(
            statistics.value_with(names::MONITOR_FAILED, &tap_labels("program")),
            Some(1.0)
        );
        let lateness = SeriesKey::new(metric(names::FRAME_LATENESS_MS), LabelSet::new());
        let SeriesValue::Histogram(histogram) =
            &statistics.registry().get(&lateness).unwrap().value
        else {
            panic!("lateness is a histogram");
        };
        assert_eq!(histogram.count(), 4);
        let gpu = SeriesKey::new(metric(names::GPU_PASS_MS), LabelSet::new());
        let SeriesValue::Histogram(histogram) = &statistics.registry().get(&gpu).unwrap().value
        else {
            panic!("GPU passes are a histogram");
        };
        assert_eq!(histogram.count(), 2);

        // Whole-run totals advance the counters by their growth only, and
        // a sample without frames keeps the late ratio.
        let mut next = native(Vec::new());
        next.stream_dropped_pairs = 5;
        statistics.record_sample(
            origin + STATISTICS_INTERVAL,
            ControlSample::default(),
            Some(next),
            None,
        );
        // 5 ms of timestamped passes over one wall second.
        assert_eq!(statistics.gpu_utilization(), Some(0.005));
        assert_eq!(statistics.value(names::GPU_UTILIZATION), Some(0.005));
        assert_eq!(statistics.cpu_utilization(), None);
        assert_eq!(statistics.value(names::STREAM_DROPPED_PAIRS), Some(5.0));
        assert_eq!(statistics.value(names::FRAMES_REALIZED), Some(4.0));
        assert_eq!(statistics.value(names::FRAME_LATE_RATIO), Some(0.25));
        assert_eq!(statistics.errors(), 0);
    }
}
