//! The daemon's named statistics (`RC-015`).
//!
//! Both daemons own one [`DaemonStatistics`] for the run. Once per
//! [`STATISTICS_INTERVAL`] the session loop samples it: the control plane's
//! subscriber count and revision on either daemon, and on a native daemon the
//! frame loop's telemetry drained since the previous sample (host frame
//! lateness and GPU pass times as histograms, audio, recorder, stream,
//! monitor, camera, and disk counters and gauges), plus bounded snapshots of
//! the stream targets' network state, the live sources' clock sync, and the
//! running encoders' queue depth. Everything lands in one bounded
//! [`MetricRegistry`] under the stable names in [`names`].
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
//! - Encoder: the fullest pair queue across the running recorder slots and
//!   started stream targets, as a share of its configured capacity. It is
//!   unavailable while no output reports a queue.
//!
//! An [`AlertEngine`] evaluates the [`DEFAULT_RULES`] after every sample.
//! Each fired, resolved, or reminder notification prints one
//! `FREEMIXD_ALERT` line on stderr and joins a bounded list of the
//! [`MAX_RECENT_NOTIFICATIONS`] most recent ones.
//!
//! The registry is an additional view: the `FREEMIXD_TELEMETRY` record the
//! native daemon prints at shutdown keeps its own whole-run aggregation and
//! fields unchanged.

use std::time::{Duration, Instant};

use std::collections::VecDeque;

use fm_observability::{
    AlertEngine, AlertEngineLimits, AlertNotification, AlertPolicy, AlertRule, AlertSeverity,
    NotificationKind, RuleCondition, RuleId, RuleReading, RuleState, ThresholdDirection,
};
use fm_observability::{
    HistogramBuckets, LabelSet, MetricDescriptor, MetricId, MetricRegistry, RegistryLimits,
    SeriesKey, SeriesValue, StatisticsError, StatisticsSnapshot,
};
use fm_protocol::{
    DaemonHealth, HealthAlert, HealthAlertRule, HealthNotification, HealthNotificationKind,
    HealthSeverity, HealthStatusMessage, MAX_UTILIZATION_PERMILLE, ServerIdentity,
    SourceClockState, SourceSyncSample,
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
    /// Started stream targets that have not latched a failure and were not
    /// deliberately stopped.
    pub(crate) const NETWORK_ACTIVE_TARGETS: &str = "network.active_targets";
    /// Active stream targets whose transport is delivering or awaiting its
    /// first random-access pair.
    pub(crate) const NETWORK_CONNECTED_TARGETS: &str = "network.connected_targets";
    /// Active stream targets waiting out a reconnect backoff.
    pub(crate) const NETWORK_RECONNECTING_TARGETS: &str = "network.reconnecting_targets";
    /// Encoded outgoing bitrate summed over the active stream targets.
    pub(crate) const NETWORK_BITRATE_KBPS: &str = "network.bitrate_kbps";
    /// The worst packet loss an active target's transport reported, in parts
    /// per million; absent while no transport reports one.
    pub(crate) const NETWORK_PACKET_LOSS_PPM: &str = "network.packet_loss_ppm";
    /// Running live sources.
    pub(crate) const SYNC_SOURCES: &str = "sync.sources";
    /// Live sources whose clock loop is not running healthy.
    pub(crate) const SYNC_DEGRADED_SOURCES: &str = "sync.degraded_sources";
    /// The largest absolute estimated drift across the live sources, in ppm.
    pub(crate) const SYNC_MAX_DRIFT_PPM: &str = "sync.max_drift_ppm";
    /// The largest audio backlog over its target across the live sources.
    pub(crate) const SYNC_MAX_BACKLOG_RATIO: &str = "sync.max_backlog_ratio";
    /// A/V pairs queued across the running encoders whose queue was read.
    pub(crate) const ENCODER_OUTSTANDING_PAIRS: &str = "encoder.outstanding_pairs";
    /// Those encoders' configured queue capacity, in pairs.
    pub(crate) const ENCODER_CAPACITY_PAIRS: &str = "encoder.capacity_pairs";
    /// The fullest running encoder queue, `0..=1`; absent while no output
    /// reports a queue.
    pub(crate) const ENCODER_QUEUE_UTILIZATION: &str = "encoder.queue_utilization";
    /// Sessions subscribed to the control plane.
    pub(crate) const CONTROL_SUBSCRIBERS: &str = "control.subscribers";
    /// The control plane's current revision.
    pub(crate) const CONTROL_REVISION: &str = "control.revision";
}

/// The most recent alert notifications kept for clients.
pub(crate) const MAX_RECENT_NOTIFICATIONS: usize = 16;

/// One default alert rule.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DefaultRule {
    /// The rule ID, which is also its wire token.
    pub(crate) id: &'static str,
    pub(crate) metric: &'static str,
    pub(crate) severity: AlertSeverity,
    /// `true` for a per-second rate of the metric, `false` for its value.
    pub(crate) rate: bool,
    pub(crate) direction: ThresholdDirection,
    pub(crate) trigger: f64,
    pub(crate) clear: f64,
    /// How long the trigger must hold before the rule fires.
    pub(crate) for_millis: u64,
}

/// Every default rule, in [`HealthAlertRule::ALL`] order (which is also the
/// wire order), so the engine's insertion order matches the protocol.
#[allow(clippy::cast_precision_loss)] // the disk thresholds are exact in an f64
pub(crate) const DEFAULT_RULES: [DefaultRule; 9] = [
    // The process keeps most of the machine busy for ten seconds.
    DefaultRule {
        id: "cpu_saturated",
        metric: names::CPU_UTILIZATION,
        severity: AlertSeverity::Warning,
        rate: false,
        direction: ThresholdDirection::Above,
        trigger: 0.9,
        clear: 0.75,
        for_millis: 10_000,
    },
    // The configured outputs' filesystem runs low, at the native daemon's
    // fixed free-space band.
    DefaultRule {
        id: "disk_low",
        metric: names::DISK_FREE_BYTES,
        severity: AlertSeverity::Critical,
        rate: false,
        direction: ThresholdDirection::Below,
        trigger: crate::disk::DISK_LOW_FREE_BYTES as f64,
        clear: crate::disk::DISK_CLEAR_FREE_BYTES as f64,
        for_millis: 0,
    },
    // A quarter of the realized frames woke half a frame late for five
    // seconds.
    DefaultRule {
        id: "frame_late",
        metric: names::FRAME_LATE_RATIO,
        severity: AlertSeverity::Warning,
        rate: false,
        direction: ThresholdDirection::Above,
        trigger: 0.25,
        clear: 0.05,
        for_millis: 5_000,
    },
    // Outputs keep dropping at least a picture a second for three seconds.
    DefaultRule {
        id: "frames_dropped",
        metric: names::FRAMES_DROPPED,
        severity: AlertSeverity::Warning,
        rate: true,
        direction: ThresholdDirection::Above,
        trigger: 1.0,
        clear: 0.0,
        for_millis: 3_000,
    },
    // The timestamped GPU passes keep the GPU nearly busy for five seconds.
    DefaultRule {
        id: "gpu_saturated",
        metric: names::GPU_UTILIZATION,
        severity: AlertSeverity::Warning,
        rate: false,
        direction: ThresholdDirection::Above,
        trigger: 0.9,
        clear: 0.7,
        for_millis: 5_000,
    },
    // A stream target or recorder slot latched a failure.
    DefaultRule {
        id: "output_failed",
        metric: names::OUTPUTS_FAILED,
        severity: AlertSeverity::Critical,
        rate: false,
        direction: ThresholdDirection::Above,
        trigger: 1.0,
        clear: 0.0,
        for_millis: 0,
    },
    // An encoder's queue stayed nearly full for five seconds.
    DefaultRule {
        id: "encoder_saturated",
        metric: names::ENCODER_QUEUE_UTILIZATION,
        severity: AlertSeverity::Warning,
        rate: false,
        direction: ThresholdDirection::Above,
        trigger: 0.9,
        clear: 0.6,
        for_millis: 5_000,
    },
    // A stream target kept waiting out reconnect backoff for ten seconds.
    DefaultRule {
        id: "stream_reconnecting",
        metric: names::NETWORK_RECONNECTING_TARGETS,
        severity: AlertSeverity::Warning,
        rate: false,
        direction: ThresholdDirection::Above,
        trigger: 1.0,
        clear: 0.0,
        for_millis: 10_000,
    },
    // A live source's clock loop stayed degraded for ten seconds.
    DefaultRule {
        id: "sync_degraded",
        metric: names::SYNC_DEGRADED_SOURCES,
        severity: AlertSeverity::Warning,
        rate: false,
        direction: ThresholdDirection::Above,
        trigger: 1.0,
        clear: 0.0,
        for_millis: 10_000,
    },
];

/// One firing alert.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActiveAlert {
    pub(crate) rule: RuleId,
    pub(crate) severity: AlertSeverity,
    /// When it fired, in statistics milliseconds.
    pub(crate) since_ms: u64,
    /// The latest reading (a rate for rate rules).
    pub(crate) value: Option<f64>,
}

fn default_engine(registry: &MetricRegistry) -> AlertEngine {
    let mut engine = AlertEngine::new(AlertEngineLimits::default()).expect("default limits");
    for rule in DEFAULT_RULES {
        let policy = AlertPolicy::new(rule.direction, rule.trigger, rule.clear)
            .expect("default rules form hysteresis bands");
        let condition = if rule.rate {
            RuleCondition::RateOfChange(policy)
        } else {
            RuleCondition::Threshold(policy)
        };
        engine
            .add_rule(
                AlertRule::new(
                    RuleId::new(rule.id).expect("default rule IDs keep the grammar"),
                    rule.severity,
                    SeriesKey::new(metric(rule.metric), LabelSet::new()),
                    RuleReading::Value,
                    condition,
                )
                .for_duration(rule.for_millis),
                registry,
            )
            .expect("default rules read registered metrics");
    }
    engine
}

/// The `FREEMIXD_ALERT` stderr line of one notification.
pub(crate) fn alert_record(notification: &AlertNotification) -> String {
    format!(
        "FREEMIXD_ALERT\tv=1\trule={}\tseverity={}\tkind={}\tat_ms={}\tvalue={}",
        notification.rule,
        notification.severity.as_str(),
        notification.kind.as_str(),
        notification.at_millis,
        notification
            .value
            .map_or_else(|| "none".to_owned(), |value| format!("{value:.3}")),
    )
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

/// The active stream targets' aggregate network state, from the shared
/// output set's bounded per-target telemetry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct NetworkSample {
    /// Started targets that have not latched a failure and were not
    /// deliberately stopped.
    pub(crate) active_targets: u64,
    /// Active targets whose transport is delivering or awaiting its first
    /// random-access pair.
    pub(crate) connected_targets: u64,
    /// Active targets waiting out a reconnect backoff.
    pub(crate) reconnecting_targets: u64,
    /// Encoded outgoing bitrate summed over the active targets, in kilobits
    /// per second.
    pub(crate) bitrate_kbps: u64,
    /// The worst packet loss an active target's transport reported, in parts
    /// per million; `None` while none reports one.
    pub(crate) packet_loss_ppm: Option<u32>,
}

/// The running live sources' clock-sync health (Protocol 2.76).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SyncSample {
    /// Running live sources.
    pub(crate) sources: u64,
    /// Sources whose clock loop is not running healthy: not
    /// [`SourceClockState::Running`], drifting more than
    /// [`SYNC_DEGRADED_DRIFT_PPM`], or holding more than
    /// [`SYNC_DEGRADED_BACKLOG_MULTIPLIER`] times their target backlog.
    pub(crate) degraded_sources: u64,
    /// The largest absolute estimated drift across the sources, in ppm.
    pub(crate) max_drift_ppm: f64,
    /// The largest audio backlog over its target across the sources, as a
    /// ratio; zero when no source reports a target.
    pub(crate) max_backlog_ratio: f64,
}

/// A source counts as degraded past this absolute drift, in ppm.
const SYNC_DEGRADED_DRIFT_PPM: u64 = 1_000;
/// A source counts as degraded past this multiple of its target backlog.
const SYNC_DEGRADED_BACKLOG_MULTIPLIER: u64 = 2;

impl SyncSample {
    /// Aggregates one bounded wire sample per running live source.
    #[cfg_attr(not(feature = "native-media"), allow(dead_code))]
    pub(crate) fn from_samples(samples: &[SourceSyncSample]) -> Self {
        let mut sync = Self {
            sources: u64::try_from(samples.len()).unwrap_or(u64::MAX),
            ..Self::default()
        };
        for sample in samples {
            let drift_ppm = sample.drift_ppm().abs();
            let degraded_backlog = sample
                .target_us
                .saturating_mul(SYNC_DEGRADED_BACKLOG_MULTIPLIER);
            if sample.state != SourceClockState::Running
                || sample.drift_mppm.unsigned_abs() > SYNC_DEGRADED_DRIFT_PPM * 1_000
                || sample.buffer_us > degraded_backlog
            {
                sync.degraded_sources = sync.degraded_sources.saturating_add(1);
            }
            sync.max_drift_ppm = sync.max_drift_ppm.max(drift_ppm);
            // A source without a target has no backlog to judge; it does not
            // fabricate a ratio of zero for the others.
            if sample.target_us > 0 {
                sync.max_backlog_ratio = sync
                    .max_backlog_ratio
                    .max(sample.buffer_ms() / sample.target_ms());
            }
        }
        sync
    }
}

/// The running encoders' bounded queue state, from the running recorder
/// slots and started stream targets.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct EncoderSample {
    /// A/V pairs queued across the outputs whose queue was readable.
    pub(crate) outstanding_pairs: u64,
    /// Those outputs' configured queue capacity, in pairs.
    pub(crate) capacity_pairs: u64,
    /// The fullest of those queues, `0..=1`; `None` when no output reported
    /// a queue this sample.
    pub(crate) queue_utilization: Option<f64>,
}

impl EncoderSample {
    /// Folds one running output's queue into the aggregate. An output whose
    /// queue cannot be read this sample is skipped by the caller, never
    /// folded as a fabricated empty queue.
    #[cfg_attr(not(feature = "native-media"), allow(dead_code))]
    pub(crate) fn observe(&mut self, outstanding_pairs: u64, capacity_pairs: u64) {
        self.outstanding_pairs = self.outstanding_pairs.saturating_add(outstanding_pairs);
        self.capacity_pairs = self.capacity_pairs.saturating_add(capacity_pairs);
        if capacity_pairs == 0 {
            return;
        }
        #[allow(clippy::cast_precision_loss)] // queue depths are bounded far below 2^53
        let utilization = (outstanding_pairs as f64 / capacity_pairs as f64).clamp(0.0, 1.0);
        self.queue_utilization = Some(
            self.queue_utilization
                .map_or(utilization, |fullest| fullest.max(utilization)),
        );
    }
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
    /// The active stream targets' aggregate network state.
    pub(crate) network: NetworkSample,
    /// The running live sources' clock-sync health.
    pub(crate) sync: SyncSample,
    /// The running recorders' and stream targets' encoder queues.
    pub(crate) encoder: EncoderSample,
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

/// A `0..=1` share in permille.
fn permille(share: f64) -> u16 {
    // Clamped to 0..=1000 first, so the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = (share * 1_000.0)
        .round()
        .clamp(0.0, f64::from(MAX_UTILIZATION_PERMILLE)) as u16;
    value
}

/// A reading times 1000, saturating (the cast saturates; NaN reads 0).
#[allow(clippy::cast_possible_truncation)]
fn milli(value: f64) -> i64 {
    (value * 1_000.0).round() as i64
}

const fn wire_severity(severity: AlertSeverity) -> HealthSeverity {
    match severity {
        AlertSeverity::Info => HealthSeverity::Info,
        AlertSeverity::Warning => HealthSeverity::Warning,
        AlertSeverity::Critical => HealthSeverity::Critical,
    }
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
    alerts: AlertEngine,
    /// The most recent notifications, oldest first.
    recent: VecDeque<AlertNotification>,
    /// The last `health_status` sequence sent (Protocol 2.100).
    health_sequence: u64,
    /// The whole-run total each counter last advanced to, by series.
    totals: std::collections::BTreeMap<(&'static str, LabelSet), u64>,
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
            (names::NETWORK_ACTIVE_TARGETS, MetricDescriptor::Gauge),
            (names::NETWORK_CONNECTED_TARGETS, MetricDescriptor::Gauge),
            (names::NETWORK_RECONNECTING_TARGETS, MetricDescriptor::Gauge),
            (names::NETWORK_BITRATE_KBPS, MetricDescriptor::Gauge),
            (names::NETWORK_PACKET_LOSS_PPM, MetricDescriptor::Gauge),
            (names::SYNC_SOURCES, MetricDescriptor::Gauge),
            (names::SYNC_DEGRADED_SOURCES, MetricDescriptor::Gauge),
            (names::SYNC_MAX_DRIFT_PPM, MetricDescriptor::Gauge),
            (names::SYNC_MAX_BACKLOG_RATIO, MetricDescriptor::Gauge),
            (names::ENCODER_OUTSTANDING_PAIRS, MetricDescriptor::Gauge),
            (names::ENCODER_CAPACITY_PAIRS, MetricDescriptor::Gauge),
            (names::ENCODER_QUEUE_UTILIZATION, MetricDescriptor::Gauge),
            (names::CONTROL_SUBSCRIBERS, MetricDescriptor::Gauge),
            (names::CONTROL_REVISION, MetricDescriptor::Gauge),
        ];
        for (name, descriptor) in descriptors {
            registry
                .register(metric(name), descriptor)
                .expect("the static metric set fits the default limits");
        }
        let alerts = default_engine(&registry);
        Self {
            origin,
            alerts,
            recent: VecDeque::with_capacity(MAX_RECENT_NOTIFICATIONS),
            health_sequence: 0,
            totals: std::collections::BTreeMap::new(),
            registry,
            sampled_at: None,
            errors: 0,
            cpu: crate::cpu::CpuSampler::new(),
            cpu_utilization: None,
            gpu_utilization: None,
        }
    }

    /// The latest process CPU utilization, `0..=1`; `None` while unavailable.
    pub(crate) const fn cpu_utilization(&self) -> Option<f64> {
        self.cpu_utilization
    }

    /// The latest GPU utilization proxy, `0..=1`; `None` while unavailable.
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
        StatisticsSnapshot::new(&self.registry)
            .with_alerts(&self.alerts)
            .render()
    }

    /// Every firing alert, in rule ID order. The wire orders them by
    /// [`HealthAlertRule`] instead; [`Self::health`] sorts them.
    pub(crate) fn active_alerts(&self) -> Vec<ActiveAlert> {
        self.alerts
            .statuses()
            .filter_map(|status| match status.state {
                RuleState::Firing { since_millis } => Some(ActiveAlert {
                    rule: status.rule.id.clone(),
                    severity: status.rule.severity,
                    since_ms: since_millis,
                    value: status.last_value,
                }),
                _ => None,
            })
            .collect()
    }

    /// The most recent notifications, oldest first.
    pub(crate) fn recent_notifications(&self) -> impl ExactSizeIterator<Item = &AlertNotification> {
        self.recent.iter()
    }

    /// The wire health sample at `now` (Protocol 2.100).
    pub(crate) fn health(&self, now: Instant) -> DaemonHealth {
        let uptime_ms = self.millis(now);
        let mut alerts: Vec<HealthAlert> = self
            .active_alerts()
            .into_iter()
            .filter_map(|alert| {
                Some(HealthAlert {
                    rule: HealthAlertRule::from_token(alert.rule.as_str())?,
                    severity: wire_severity(alert.severity),
                    since_ms: alert.since_ms.min(uptime_ms),
                    value_milli: alert.value.map(milli),
                })
            })
            .collect();
        // The engine reports in rule ID order, but the wire requires the
        // protocol's rule order; the two differ where a newer rule ID sorts
        // before an older one (`encoder_saturated` before `frame_late`).
        alerts.sort_unstable_by_key(|alert| alert.rule);
        let notifications = self
            .recent_notifications()
            .filter_map(|notification| {
                Some(HealthNotification {
                    sequence: notification.sequence,
                    rule: HealthAlertRule::from_token(notification.rule.as_str())?,
                    severity: wire_severity(notification.severity),
                    kind: match notification.kind {
                        NotificationKind::Firing => HealthNotificationKind::Firing,
                        NotificationKind::Resolved => HealthNotificationKind::Resolved,
                        NotificationKind::Reminder => HealthNotificationKind::Reminder,
                    },
                    at_ms: notification.at_millis.min(uptime_ms),
                    value_milli: notification.value.map(milli),
                })
            })
            .collect();
        DaemonHealth {
            uptime_ms,
            cpu_permille: self.cpu_utilization().map(permille),
            gpu_permille: self.gpu_utilization().map(permille),
            alerts,
            notifications,
        }
    }

    /// The next `health_status` record (Protocol 2.100) for the sample just
    /// taken at `now`.
    pub(crate) fn health_status(
        &mut self,
        server: &ServerIdentity,
        now: Instant,
    ) -> Option<HealthStatusMessage> {
        self.health_sequence = self.health_sequence.checked_add(1)?;
        Some(HealthStatusMessage {
            server: server.clone(),
            sequence: self.health_sequence,
            health: self.health(now),
        })
    }

    /// Evaluates every rule at `at` and keeps what it notified.
    fn evaluate_alerts(&mut self, at: u64) -> Vec<AlertNotification> {
        if self.alerts.evaluate(&self.registry, at).is_err() {
            self.errors = self.errors.saturating_add(1);
        }
        let notifications = self.alerts.drain_notifications();
        for notification in &notifications {
            if self.recent.len() == MAX_RECENT_NOTIFICATIONS {
                self.recent.pop_front();
            }
            self.recent.push_back(notification.clone());
        }
        notifications
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
        // A utilization that is unavailable now clears its gauge rather
        // than keeping the last reading, so its saturation rule reads
        // unknown (it holds its state with no value) instead of firing on
        // an old value.
        self.cpu_utilization = cpu;
        self.optional_gauge(names::CPU_UTILIZATION, at, cpu);
        self.gpu_utilization = native
            .as_ref()
            .filter(|native| native.gpu_timing)
            .and_then(|native| gpu_utilization(&native.gpu_pass_ms, wall_ms?));
        self.optional_gauge(names::GPU_UTILIZATION, at, self.gpu_utilization);
        self.gauge(
            names::CONTROL_SUBSCRIBERS,
            at,
            gauge_value(control.subscribers),
        );
        self.gauge(names::CONTROL_REVISION, at, gauge_value(control.revision));
        if let Some(native) = native {
            self.record_native(at, &native);
        }
        for notification in self.evaluate_alerts(at) {
            eprintln!("{}", alert_record(&notification));
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
        self.gauge(
            names::NETWORK_ACTIVE_TARGETS,
            at,
            gauge_value(native.network.active_targets),
        );
        self.gauge(
            names::NETWORK_CONNECTED_TARGETS,
            at,
            gauge_value(native.network.connected_targets),
        );
        self.gauge(
            names::NETWORK_RECONNECTING_TARGETS,
            at,
            gauge_value(native.network.reconnecting_targets),
        );
        self.gauge(
            names::NETWORK_BITRATE_KBPS,
            at,
            gauge_value(native.network.bitrate_kbps),
        );
        // An unreported packet loss has no reading; the series is removed so
        // a stale value never stands in for it.
        self.optional_gauge(
            names::NETWORK_PACKET_LOSS_PPM,
            at,
            native.network.packet_loss_ppm.map(f64::from),
        );
        self.gauge(names::SYNC_SOURCES, at, gauge_value(native.sync.sources));
        self.gauge(
            names::SYNC_DEGRADED_SOURCES,
            at,
            gauge_value(native.sync.degraded_sources),
        );
        self.gauge(names::SYNC_MAX_DRIFT_PPM, at, native.sync.max_drift_ppm);
        self.gauge(
            names::SYNC_MAX_BACKLOG_RATIO,
            at,
            native.sync.max_backlog_ratio,
        );
        self.gauge(
            names::ENCODER_OUTSTANDING_PAIRS,
            at,
            gauge_value(native.encoder.outstanding_pairs),
        );
        self.gauge(
            names::ENCODER_CAPACITY_PAIRS,
            at,
            gauge_value(native.encoder.capacity_pairs),
        );
        // No readable output queue means no utilization; the series is
        // removed so the saturation rule reads unknown and holds its state.
        self.optional_gauge(
            names::ENCODER_QUEUE_UTILIZATION,
            at,
            native.encoder.queue_utilization,
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

    /// Sets a gauge, or removes it while the reading is unavailable.
    fn optional_gauge(&mut self, name: &'static str, at: u64, value: Option<f64>) {
        match value {
            Some(value) => self.gauge(name, at, value),
            None => {
                self.registry
                    .remove_series(&SeriesKey::new(metric(name), LabelSet::new()));
            }
        }
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

    /// Advances a counter by the growth of a whole-run `total` since the
    /// previous sample. A total below the previous one (a source that
    /// restarted its count, or an output that stopped and left a sum) adds
    /// nothing, and later growth from there still counts; the counter
    /// still updates, so rate rules see a zero rate.
    fn total(&mut self, name: &'static str, labels: &LabelSet, at: u64, total: u64) {
        let previous = self
            .totals
            .insert((name, labels.clone()), total)
            .unwrap_or(0);
        let result = self.registry.increment_counter(
            &metric(name),
            labels,
            at,
            total.saturating_sub(previous),
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

    fn rule_ids(alerts: &[ActiveAlert]) -> Vec<&str> {
        alerts.iter().map(|alert| alert.rule.as_str()).collect()
    }

    #[test]
    fn health_reports_utilization_alerts_and_notifications_on_the_wire() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let server = ServerIdentity {
            engine_id: "engine".into(),
            project_id: "project".into(),
            state_epoch: 1,
            log_id: "log".into(),
        };
        statistics.record_sample(origin, ControlSample::default(), None, None);
        let quiet = statistics.health_status(&server, origin).unwrap();
        assert_eq!(quiet.sequence, 1);
        assert_eq!(quiet.health, DaemonHealth::default());

        let mut sample = native(vec![1.0]);
        sample.outputs_failed = 2;
        statistics.record_sample(
            origin + STATISTICS_INTERVAL,
            ControlSample::default(),
            Some(sample),
            Some(0.4567),
        );
        let now = origin + Duration::from_millis(1_500);
        let status = statistics.health_status(&server, now).unwrap();
        assert_eq!(status.sequence, 2);
        let health = status.health;
        assert!(health.is_valid());
        assert_eq!(health.uptime_ms, 1_500);
        assert_eq!(health.cpu_permille, Some(457));
        // 5 ms of passes over one second, on a native daemon.
        assert_eq!(health.gpu_permille, Some(5));
        assert_eq!(
            health.alerts,
            [
                HealthAlert {
                    rule: HealthAlertRule::DiskLow,
                    severity: HealthSeverity::Critical,
                    since_ms: 1_000,
                    value_milli: Some(4_096_000),
                },
                HealthAlert {
                    rule: HealthAlertRule::OutputFailed,
                    severity: HealthSeverity::Critical,
                    since_ms: 1_000,
                    value_milli: Some(2_000),
                },
            ]
        );
        let kinds: Vec<_> = health
            .notifications
            .iter()
            .map(|notification| (notification.rule, notification.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                (HealthAlertRule::DiskLow, HealthNotificationKind::Firing),
                (
                    HealthAlertRule::OutputFailed,
                    HealthNotificationKind::Firing
                ),
            ]
        );
        // Every default rule has a wire token.
        for rule in DEFAULT_RULES {
            assert!(
                HealthAlertRule::from_token(rule.id).is_some(),
                "{}",
                rule.id
            );
        }
        assert_eq!(DEFAULT_RULES.len(), HealthAlertRule::ALL.len());
    }

    #[test]
    fn default_rules_follow_wire_order_and_are_valid() {
        let ids: Vec<_> = DEFAULT_RULES.iter().map(|rule| rule.id).collect();
        let expected: Vec<_> = HealthAlertRule::ALL
            .iter()
            .copied()
            .map(HealthAlertRule::token)
            .collect();
        assert_eq!(ids, expected);
        for rule in DEFAULT_RULES {
            // A trigger at zero would fire on an idle daemon.
            assert!(rule.trigger > 0.0, "{} triggers at zero", rule.id);
            match rule.direction {
                ThresholdDirection::Above => {
                    assert!(rule.clear < rule.trigger, "{} has no band", rule.id);
                }
                ThresholdDirection::Below => {
                    assert!(rule.clear > rule.trigger, "{} has no band", rule.id);
                }
            }
        }
        // The RC-015 rules: one degraded output or a 90%-full encoder queue,
        // each held before firing.
        let rule = |id: &str| DEFAULT_RULES.iter().find(|rule| rule.id == id).unwrap();
        let encoder = rule("encoder_saturated");
        assert_eq!(encoder.metric, names::ENCODER_QUEUE_UTILIZATION);
        assert_eq!(
            (encoder.trigger, encoder.clear, encoder.for_millis),
            (0.9, 0.6, 5_000)
        );
        let reconnecting = rule("stream_reconnecting");
        assert_eq!(reconnecting.metric, names::NETWORK_RECONNECTING_TARGETS);
        assert_eq!(
            (
                reconnecting.trigger,
                reconnecting.clear,
                reconnecting.for_millis
            ),
            (1.0, 0.0, 10_000)
        );
        let sync = rule("sync_degraded");
        assert_eq!(sync.metric, names::SYNC_DEGRADED_SOURCES);
        assert_eq!(
            (sync.trigger, sync.clear, sync.for_millis),
            (1.0, 0.0, 10_000)
        );
        let statistics = DaemonStatistics::new(Instant::now());
        assert_eq!(statistics.alerts.statuses().len(), DEFAULT_RULES.len());
        assert_eq!(statistics.active_alerts().len(), 0);
    }

    #[test]
    fn idle_native_reads_zero_while_simulated_reads_unknown() {
        let zero_on_native = [
            names::NETWORK_ACTIVE_TARGETS,
            names::NETWORK_CONNECTED_TARGETS,
            names::NETWORK_RECONNECTING_TARGETS,
            names::NETWORK_BITRATE_KBPS,
            names::SYNC_SOURCES,
            names::SYNC_DEGRADED_SOURCES,
            names::SYNC_MAX_DRIFT_PPM,
            names::SYNC_MAX_BACKLOG_RATIO,
            names::ENCODER_OUTSTANDING_PAIRS,
            names::ENCODER_CAPACITY_PAIRS,
        ];
        let origin = Instant::now();
        let mut native_daemon = DaemonStatistics::new(origin);
        native_daemon.record_sample(
            origin,
            ControlSample::default(),
            Some(NativeStatisticsSample::default()),
            None,
        );
        for name in zero_on_native {
            assert_eq!(native_daemon.value(name), Some(0.0), "{name}");
        }
        // An idle native daemon has no queue and no reported loss: those read
        // unknown, not a fabricated zero.
        assert_eq!(native_daemon.value(names::NETWORK_PACKET_LOSS_PPM), None);
        assert_eq!(native_daemon.value(names::ENCODER_QUEUE_UTILIZATION), None);

        // A simulated daemon reports no native sample at all: the same series
        // are absent rather than zero.
        let mut simulated = DaemonStatistics::new(origin);
        simulated.record_sample(origin, ControlSample::default(), None, None);
        for name in zero_on_native {
            assert_eq!(simulated.value(name), None, "{name}");
        }
        assert_eq!(simulated.value(names::NETWORK_PACKET_LOSS_PPM), None);
        assert_eq!(simulated.value(names::ENCODER_QUEUE_UTILIZATION), None);
    }

    #[test]
    fn encoder_utilization_is_the_fullest_output_queue() {
        let mut encoder = EncoderSample::default();
        assert_eq!(encoder.queue_utilization, None);
        encoder.observe(1, 4);
        assert_eq!(encoder.outstanding_pairs, 1);
        assert_eq!(encoder.capacity_pairs, 4);
        assert_eq!(encoder.queue_utilization, Some(0.25));
        // A second output with a fuller queue sets the aggregate reading.
        encoder.observe(3, 4);
        assert_eq!(encoder.outstanding_pairs, 4);
        assert_eq!(encoder.capacity_pairs, 8);
        assert_eq!(encoder.queue_utilization, Some(0.75));
        // A reading deeper than the configured capacity still reads full.
        encoder.observe(8, 4);
        assert_eq!(encoder.queue_utilization, Some(1.0));
        // A zero-capacity output contributes no utilization.
        let mut empty = EncoderSample::default();
        empty.observe(2, 0);
        assert_eq!(empty.outstanding_pairs, 2);
        assert_eq!(empty.capacity_pairs, 0);
        assert_eq!(empty.queue_utilization, None);
    }

    #[test]
    fn sync_sample_counts_degraded_sources() {
        let sample = |state, drift_mppm, buffer_us, target_us| SourceSyncSample {
            input: fm_protocol::WireInputId::new(std::num::NonZeroU128::new(1).unwrap()),
            kind: fm_protocol::SourceSyncKind::Capture,
            state,
            input_rate_hz: 48_000,
            output_rate_hz: 48_000,
            drift_mppm,
            correction_mppm: 0,
            buffer_us,
            target_us,
            resyncs: 0,
            underruns: 0,
            overruns: 0,
            picture_drops: 0,
            picture_repeats: 0,
            restarts: 0,
        };
        assert_eq!(SyncSample::from_samples(&[]), SyncSample::default());
        let sync = SyncSample::from_samples(&[
            // Healthy: running, well inside the drift band, backlog at its
            // target.
            sample(SourceClockState::Running, 250_000, 40_000, 40_000),
            // Priming is not running yet.
            sample(SourceClockState::Priming, 0, 40_000, 40_000),
            // Drifting past 1000 ppm.
            sample(SourceClockState::Running, -1_500_000, 40_000, 40_000),
            // Backlog past twice its target.
            sample(SourceClockState::Running, 0, 121_000, 60_000),
        ]);
        assert_eq!(sync.sources, 4);
        assert_eq!(sync.degraded_sources, 3);
        assert!((sync.max_drift_ppm - 1_500.0).abs() < 1e-9);
        assert!((2.0..2.1).contains(&sync.max_backlog_ratio));
    }

    #[test]
    fn degraded_stream_sync_and_encoder_samples_raise_and_clear_their_alerts() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let at = |seconds: u64| origin + Duration::from_secs(seconds);
        // One target reconnecting, one degraded live source, and a 90%-full
        // encoder queue, sampled once a second. The disk gauge stays absent
        // and no output has failed, so only the three new rules are in play.
        let degraded = |statistics: &mut DaemonStatistics, seconds: u64| {
            let mut sample = native(Vec::new());
            sample.disk_free_bytes = None;
            sample.network.active_targets = 2;
            sample.network.connected_targets = 1;
            sample.network.reconnecting_targets = 1;
            sample.sync.sources = 2;
            sample.sync.degraded_sources = 1;
            sample.sync.max_drift_ppm = 1_500.0;
            sample.sync.max_backlog_ratio = 2.5;
            sample.encoder.outstanding_pairs = 9;
            sample.encoder.capacity_pairs = 10;
            sample.encoder.queue_utilization = Some(0.9);
            statistics.record_sample(at(seconds), ControlSample::default(), Some(sample), None);
        };
        for seconds in 0..5 {
            degraded(&mut statistics, seconds);
            assert!(
                statistics.active_alerts().is_empty(),
                "pending at {seconds} s"
            );
        }
        // The encoder holds for five seconds; the other two for ten.
        degraded(&mut statistics, 5);
        assert_eq!(rule_ids(&statistics.active_alerts()), ["encoder_saturated"]);
        for seconds in 6..10 {
            degraded(&mut statistics, seconds);
            assert_eq!(rule_ids(&statistics.active_alerts()), ["encoder_saturated"]);
        }
        degraded(&mut statistics, 10);
        assert_eq!(
            rule_ids(&statistics.active_alerts()),
            ["encoder_saturated", "stream_reconnecting", "sync_degraded"]
        );
        let health = statistics.health(at(10));
        assert!(health.is_valid());
        assert_eq!(
            health.alerts,
            [
                HealthAlert {
                    rule: HealthAlertRule::EncoderSaturated,
                    severity: HealthSeverity::Warning,
                    since_ms: 5_000,
                    value_milli: Some(900),
                },
                HealthAlert {
                    rule: HealthAlertRule::StreamReconnecting,
                    severity: HealthSeverity::Warning,
                    since_ms: 10_000,
                    value_milli: Some(1_000),
                },
                HealthAlert {
                    rule: HealthAlertRule::SyncDegraded,
                    severity: HealthSeverity::Warning,
                    since_ms: 10_000,
                    value_milli: Some(1_000),
                },
            ]
        );

        // Every reading recovers: the queue falls into the hysteresis band's
        // clear side, and the target and source counts reach zero.
        let mut healthy = native(Vec::new());
        healthy.disk_free_bytes = None;
        healthy.network.active_targets = 2;
        healthy.network.connected_targets = 2;
        healthy.encoder.capacity_pairs = 10;
        healthy.encoder.queue_utilization = Some(0.5);
        statistics.record_sample(at(11), ControlSample::default(), Some(healthy), None);
        assert!(statistics.active_alerts().is_empty());
        let health = statistics.health(at(11));
        assert!(health.is_valid());
        assert!(health.alerts.is_empty());
        let notifications: Vec<_> = health
            .notifications
            .iter()
            .map(|notification| (notification.rule, notification.kind))
            .collect();
        assert_eq!(
            notifications,
            [
                (
                    HealthAlertRule::EncoderSaturated,
                    HealthNotificationKind::Firing
                ),
                (
                    HealthAlertRule::StreamReconnecting,
                    HealthNotificationKind::Firing
                ),
                (
                    HealthAlertRule::SyncDegraded,
                    HealthNotificationKind::Firing
                ),
                (
                    HealthAlertRule::EncoderSaturated,
                    HealthNotificationKind::Resolved
                ),
                (
                    HealthAlertRule::StreamReconnecting,
                    HealthNotificationKind::Resolved
                ),
                (
                    HealthAlertRule::SyncDegraded,
                    HealthNotificationKind::Resolved
                ),
            ]
        );
    }

    /// The engine reports in rule ID order, where `encoder_saturated` sorts
    /// before `frame_late`; the protocol orders `frame_late` first, and the
    /// wire rejects a health sample whose alerts are out of that order.
    #[test]
    fn health_orders_alerts_by_protocol_rule_not_id() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let second = |seconds| origin + Duration::from_secs(seconds);
        for seconds in 0..=5 {
            let mut sample = native(vec![40.0]);
            sample.disk_free_bytes = None;
            sample.encoder.capacity_pairs = 10;
            sample.encoder.queue_utilization = Some(0.95);
            statistics.record_sample(
                second(seconds),
                ControlSample::default(),
                Some(sample),
                None,
            );
        }
        // Both hold for five seconds and fire together.
        assert_eq!(
            rule_ids(&statistics.active_alerts()),
            ["encoder_saturated", "frame_late"]
        );
        let health = statistics.health(second(5));
        assert!(health.is_valid());
        let rules: Vec<_> = health.alerts.iter().map(|alert| alert.rule).collect();
        assert_eq!(
            rules,
            [
                HealthAlertRule::FrameLate,
                HealthAlertRule::EncoderSaturated
            ]
        );
    }

    #[test]
    fn sustained_cpu_saturation_fires_and_clears() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let second = |seconds| origin + Duration::from_secs(seconds);
        for seconds in 0..10 {
            statistics.record_sample(second(seconds), ControlSample::default(), None, Some(0.95));
            assert!(
                statistics.active_alerts().is_empty(),
                "pending at {seconds} s"
            );
        }
        statistics.record_sample(second(10), ControlSample::default(), None, Some(0.95));
        let active = statistics.active_alerts();
        assert_eq!(rule_ids(&active), ["cpu_saturated"]);
        assert_eq!(active[0].severity, AlertSeverity::Warning);
        assert_eq!(active[0].since_ms, 10_000);
        assert_eq!(active[0].value, Some(0.95));
        // Inside the hysteresis band it keeps firing; below it, it clears.
        statistics.record_sample(second(11), ControlSample::default(), None, Some(0.8));
        assert_eq!(rule_ids(&statistics.active_alerts()), ["cpu_saturated"]);
        statistics.record_sample(second(12), ControlSample::default(), None, Some(0.5));
        assert_eq!(statistics.active_alerts().len(), 0);
        let kinds: Vec<_> = statistics
            .recent_notifications()
            .map(|notification| (notification.rule.as_str(), notification.kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [("cpu_saturated", "firing"), ("cpu_saturated", "resolved")]
        );
        let record = alert_record(statistics.recent_notifications().next().unwrap());
        assert_eq!(
            record,
            "FREEMIXD_ALERT\tv=1\trule=cpu_saturated\tseverity=warning\tkind=firing\tat_ms=10000\tvalue=0.950"
        );
    }

    /// A CPU or GPU reading that becomes unavailable clears its gauge: the
    /// saturation rule no longer reads the old value (it reads unknown,
    /// holding its state with no value) and resolves once a reading is
    /// back below its band.
    #[test]
    fn an_unavailable_utilization_stops_feeding_its_alert() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let second = |seconds| origin + Duration::from_secs(seconds);
        let cpu = SeriesKey::new(metric(names::CPU_UTILIZATION), LabelSet::new());
        let gpu = SeriesKey::new(metric(names::GPU_UTILIZATION), LabelSet::new());
        for seconds in 0..=10 {
            let mut sample = native(Vec::new());
            sample.gpu_timing = true;
            sample.gpu_pass_ms = vec![950.0];
            sample.disk_free_bytes = None;
            sample.outputs_failed = 0;
            statistics.record_sample(
                second(seconds),
                ControlSample::default(),
                Some(sample),
                Some(0.95),
            );
        }
        assert_eq!(
            rule_ids(&statistics.active_alerts()),
            ["cpu_saturated", "gpu_saturated"]
        );
        // Both readings become unavailable.
        for seconds in 11..30 {
            statistics.record_sample(
                second(seconds),
                ControlSample::default(),
                Some(NativeStatisticsSample {
                    gpu_timing: false,
                    disk_free_bytes: None,
                    outputs_failed: 0,
                    ..native(Vec::new())
                }),
                None,
            );
        }
        assert!(statistics.registry().get(&cpu).is_none());
        assert!(statistics.registry().get(&gpu).is_none());
        let health = statistics.health(second(29));
        assert_eq!(health.cpu_permille, None);
        assert_eq!(health.gpu_permille, None);
        assert!(
            health
                .alerts
                .iter()
                .all(|alert| alert.value_milli.is_none()),
            "{:?}",
            health.alerts
        );
        assert!(
            statistics
                .active_alerts()
                .iter()
                .all(|alert| alert.value.is_none())
        );
        // A reading back below the band resolves the alert.
        statistics.record_sample(second(30), ControlSample::default(), None, Some(0.2));
        assert_eq!(rule_ids(&statistics.active_alerts()), ["gpu_saturated"]);
    }

    #[test]
    fn native_rules_cover_disk_outputs_lateness_and_drops() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let second = |seconds| origin + Duration::from_secs(seconds);
        // Everything late, dropping two pictures a second, low on disk, and
        // one failed output.
        for seconds in 0..=5 {
            let mut sample = native(vec![40.0, 50.0]);
            sample.frames_dropped = 2 * seconds;
            sample.disk_free_bytes = Some(100 * 1024 * 1024);
            sample.outputs_failed = 1;
            statistics.record_sample(
                second(seconds),
                ControlSample::default(),
                Some(sample),
                None,
            );
        }
        assert_eq!(
            rule_ids(&statistics.active_alerts()),
            ["disk_low", "frame_late", "frames_dropped", "output_failed"]
        );
        // Recovery clears all four.
        let mut sample = native(vec![0.0, 1.0]);
        sample.frames_dropped = 10;
        sample.disk_free_bytes = Some(8 * 1024 * 1024 * 1024);
        statistics.record_sample(second(6), ControlSample::default(), Some(sample), None);
        assert_eq!(statistics.active_alerts().len(), 0);
        assert!(
            statistics
                .render()
                .contains("alert output_failed severity=critical state=resolved")
        );
    }

    #[test]
    fn recent_notifications_are_bounded() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let toggle = |statistics: &mut DaemonStatistics, seconds: u64| {
            let mut sample = native(Vec::new());
            sample.outputs_failed = seconds % 2;
            sample.disk_free_bytes = None;
            statistics.record_sample(
                origin + Duration::from_secs(seconds),
                ControlSample::default(),
                Some(sample),
                None,
            );
        };
        // Odd seconds fire, even ones resolve: seconds 1..=15 notify 15 times.
        for seconds in 0..16 {
            toggle(&mut statistics, seconds);
        }
        assert_eq!(
            statistics.recent_notifications().len(),
            MAX_RECENT_NOTIFICATIONS - 1
        );
        for seconds in 16..20 {
            toggle(&mut statistics, seconds);
        }
        assert_eq!(
            statistics.recent_notifications().len(),
            MAX_RECENT_NOTIFICATIONS
        );
        let first = statistics.recent_notifications().next().unwrap();
        assert_eq!(first.at_millis, 4_000);
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

    /// A whole-run total summed over the outputs running now falls when one
    /// stops; the drops after that still count and still fire the rule.
    #[test]
    fn a_total_that_falls_keeps_counting_its_growth() {
        let origin = Instant::now();
        let mut statistics = DaemonStatistics::new(origin);
        let at = |seconds| origin + Duration::from_secs(seconds);
        let sample = |statistics: &mut DaemonStatistics, seconds: u64, dropped: u64| {
            let mut sample = native(Vec::new());
            sample.frames_dropped = dropped;
            sample.disk_free_bytes = None;
            statistics.record_sample(at(seconds), ControlSample::default(), Some(sample), None);
        };
        // A stream that dropped 500 pictures stops; a new one starts at 0.
        sample(&mut statistics, 0, 500);
        sample(&mut statistics, 1, 0);
        assert_eq!(statistics.value(names::FRAMES_DROPPED), Some(500.0));
        // The new one drops two pictures a second.
        for seconds in 2..=6 {
            sample(&mut statistics, seconds, 2 * (seconds - 1));
        }
        assert_eq!(statistics.value(names::FRAMES_DROPPED), Some(510.0));
        assert_eq!(rule_ids(&statistics.active_alerts()), ["frames_dropped"]);
    }
}
