//! The native runtime telemetry: the counters the frame loop reports at
//! shutdown, the CPU/GPU/audio/camera samples, and the metric store behind
//! them.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(clippy::wildcard_imports)] // the binary's items and imports
use super::*;

#[cfg(feature = "native-media")]
pub(crate) struct NativeRuntimeTelemetry {
    pub(crate) origin: Instant,
    pub(crate) gpu_adapter: String,
    pub(crate) gpu_backend: NativeBackend,
    pub(crate) metrics: MetricStore,
    pub(crate) metric_errors: u64,
    pub(crate) gpu_support: fm_gpu::NativeFullscreenTimingSupport,
    pub(crate) gpu_pending_samples: u64,
    pub(crate) gpu_dropped_samples: u64,
    pub(crate) gpu_unavailable_samples: u64,
    pub(crate) audio_retained_blocks: u64,
    pub(crate) audio_peak_retained_blocks: u64,
    pub(crate) audio_retained_samples: u64,
    pub(crate) audio_peak_retained_samples: u64,
    pub(crate) audio_retained_bytes: u64,
    pub(crate) audio_peak_retained_bytes: u64,
    pub(crate) audio_reservation_requests: u64,
    pub(crate) audio_reserved_blocks: u64,
    pub(crate) audio_peak_reserved_blocks: u64,
    pub(crate) audio_reserved_samples: u64,
    pub(crate) audio_peak_reserved_samples: u64,
    pub(crate) audio_reserved_bytes: u64,
    pub(crate) audio_peak_reserved_bytes: u64,
    pub(crate) audio_source_stalls: u64,
    pub(crate) audio_positioned_blocks: u64,
    pub(crate) audio_positioned_samples: u64,
    pub(crate) audio_leading_silence_samples: u64,
    pub(crate) audio_eos_padding_blocks: u64,
    pub(crate) audio_eos_padding_samples: u64,
    pub(crate) audio_sink_depth: u64,
    pub(crate) audio_sink_peak_depth: u64,
    pub(crate) audio_sink_dropped: u64,
    pub(crate) camera: NativeCameraTelemetry,
    pub(crate) recorder_configured: bool,
    pub(crate) recorder_outstanding_pairs: u64,
    pub(crate) recorder_peak_outstanding_pairs: u64,
    pub(crate) recorder_retained_bytes: u64,
    pub(crate) recorder_peak_retained_bytes: u64,
    pub(crate) stream_enqueued_pairs_total: u64,
    pub(crate) stream_dropped_pairs_total: u64,
    /// Encoded monitor frames published, failed, and skipped per tap in
    /// [`MonitorTap::index`] order (Protocol 2.21 fallback); a failed
    /// monitor tap never fails the frame tick.
    pub(crate) monitor_taps: [NativeTapCounters; 2],
    /// Latest successfully sampled free bytes across the configured outputs:
    /// the recording output's parent and every local HLS output directory;
    /// `None` until the first sample.
    pub(crate) disk_free_bytes: Option<u64>,
    /// Disk free-space alert state at the latest sample; `None` until the
    /// first sample evaluated.
    pub(crate) disk_alert: Option<AlertState>,
    /// Host lateness and GPU pass observations, in milliseconds, since the
    /// statistics registry last drained them (RC-015); bounded by
    /// [`statistics::MAX_PENDING_OBSERVATIONS`] each.
    pub(crate) pending_lateness_ms: Vec<f64>,
    pub(crate) pending_gpu_pass_ms: Vec<f64>,
    /// Observations the full buffers refused.
    pub(crate) pending_dropped: u64,
}

#[cfg(feature = "native-media")]
impl NativeRuntimeTelemetry {
    const RETAINED_SAMPLES: usize = 256;

    pub(crate) fn new(origin: Instant, adapter: &fm_gpu::NativeAdapterInfo) -> Self {
        Self {
            origin,
            gpu_adapter: adapter.name.clone(),
            gpu_backend: adapter.backend,
            metrics: MetricStore::new(Self::RETAINED_SAMPLES),
            metric_errors: 0,
            gpu_support: fm_gpu::NativeFullscreenTimingSupport::Unsupported,
            gpu_pending_samples: 0,
            gpu_dropped_samples: 0,
            gpu_unavailable_samples: 0,
            audio_retained_blocks: 0,
            audio_peak_retained_blocks: 0,
            audio_retained_samples: 0,
            audio_peak_retained_samples: 0,
            audio_retained_bytes: 0,
            audio_peak_retained_bytes: 0,
            audio_reservation_requests: 0,
            audio_reserved_blocks: 0,
            audio_peak_reserved_blocks: 0,
            audio_reserved_samples: 0,
            audio_peak_reserved_samples: 0,
            audio_reserved_bytes: 0,
            audio_peak_reserved_bytes: 0,
            audio_source_stalls: 0,
            audio_positioned_blocks: 0,
            audio_positioned_samples: 0,
            audio_leading_silence_samples: 0,
            audio_eos_padding_blocks: 0,
            audio_eos_padding_samples: 0,
            audio_sink_depth: 0,
            audio_sink_peak_depth: 0,
            audio_sink_dropped: 0,
            camera: NativeCameraTelemetry::default(),
            recorder_configured: false,
            recorder_outstanding_pairs: 0,
            recorder_peak_outstanding_pairs: 0,
            recorder_retained_bytes: 0,
            recorder_peak_retained_bytes: 0,
            stream_enqueued_pairs_total: 0,
            stream_dropped_pairs_total: 0,
            monitor_taps: [NativeTapCounters::default(); 2],
            disk_free_bytes: None,
            disk_alert: None,
            pending_lateness_ms: Vec::new(),
            pending_gpu_pass_ms: Vec::new(),
            pending_dropped: 0,
        }
    }

    /// Buffers one observation for the statistics registry, counting it
    /// dropped once the buffer is full.
    pub(crate) fn buffer_observation(buffer: &mut Vec<f64>, dropped: &mut u64, milliseconds: f64) {
        if buffer.len() < statistics::MAX_PENDING_OBSERVATIONS {
            buffer.push(milliseconds);
        } else {
            *dropped = dropped.saturating_add(1);
        }
    }

    /// Replaces the monitor tap counters with the realizer's snapshot.
    pub(crate) const fn set_monitor_taps(&mut self, taps: [NativeTapCounters; 2]) {
        self.monitor_taps = taps;
    }

    pub(crate) fn observe_host_lateness(&mut self, deadline: Instant, observed: Instant) {
        let milliseconds = observed.saturating_duration_since(deadline).as_secs_f64() * 1_000.0;
        Self::buffer_observation(
            &mut self.pending_lateness_ms,
            &mut self.pending_dropped,
            milliseconds,
        );
        let time = self.monotonic_millis();
        let result =
            self.metrics
                .observe_histogram(Metric::LatencyMilliseconds, time, milliseconds);
        self.record_metric_result(result);
    }

    pub(crate) fn observe_gpu(&mut self, context: &NativeContext) {
        let telemetry = context.take_fullscreen_timing_telemetry();
        self.gpu_support = telemetry.support;
        self.gpu_pending_samples = saturating_u64(telemetry.pending_samples);
        self.gpu_dropped_samples = telemetry.dropped_samples;
        self.gpu_unavailable_samples = telemetry.unavailable_samples;
        for sample in telemetry.completed_samples {
            let milliseconds = sample.duration_nanoseconds() / 1_000_000.0;
            Self::buffer_observation(
                &mut self.pending_gpu_pass_ms,
                &mut self.pending_dropped,
                milliseconds,
            );
            let time = self.monotonic_millis();
            let result =
                self.metrics
                    .observe_histogram(Metric::GpuTimeMilliseconds, time, milliseconds);
            self.record_metric_result(result);
        }
    }

    pub(crate) fn observe_audio(&mut self, snapshot: native_audio_realizer::AudioSnapshot) {
        let audio = snapshot.telemetry;
        self.audio_retained_blocks = saturating_u64(snapshot.retained_blocks);
        self.audio_peak_retained_blocks = self
            .audio_peak_retained_blocks
            .max(self.audio_retained_blocks)
            .max(saturating_u64(audio.peak_retained_blocks));
        self.audio_retained_samples = saturating_u64(snapshot.retained_samples);
        self.audio_peak_retained_samples = self
            .audio_peak_retained_samples
            .max(self.audio_retained_samples)
            .max(saturating_u64(audio.peak_retained_samples));
        self.audio_retained_bytes = saturating_u64(snapshot.retained_bytes);
        self.audio_peak_retained_bytes = self
            .audio_peak_retained_bytes
            .max(self.audio_retained_bytes)
            .max(saturating_u64(audio.peak_retained_bytes));
        self.audio_reservation_requests = audio.reservation_requests;
        self.audio_reserved_blocks = saturating_u64(audio.reserved_blocks);
        self.audio_peak_reserved_blocks = saturating_u64(audio.peak_reserved_blocks);
        self.audio_reserved_samples = saturating_u64(audio.reserved_samples);
        self.audio_peak_reserved_samples = saturating_u64(audio.peak_reserved_samples);
        self.audio_reserved_bytes = saturating_u64(audio.reserved_bytes);
        self.audio_peak_reserved_bytes = saturating_u64(audio.peak_reserved_bytes);
        self.audio_source_stalls = audio.source_stalls;
        self.audio_positioned_blocks = audio.positioned_blocks;
        self.audio_positioned_samples = audio.positioned_samples;
        self.audio_leading_silence_samples = audio.leading_silence_samples;
        self.audio_eos_padding_blocks = audio.eos_padding_blocks;
        self.audio_eos_padding_samples = audio.eos_padding_samples;
        self.audio_sink_depth = saturating_u64(snapshot.sink_depth);
        self.audio_sink_peak_depth = saturating_u64(snapshot.sink.high_watermark());
        self.audio_sink_dropped = snapshot.sink.dropped();
    }

    /// Aggregates the running recorder slots into the whole-run sample.
    ///
    /// Outstanding pairs and retained bytes sum across every running slot and
    /// both peaks track the aggregate. Called only while at least one slot
    /// runs, so an idle daemon keeps its last observation instead of
    /// fabricating a zero sample from no recorders.
    pub(crate) fn observe_recorders<'a>(
        &mut self,
        recorders: impl IntoIterator<Item = &'a NativeProgramRecorder>,
    ) {
        let mut outstanding_pairs = 0_u64;
        let mut retained_bytes = 0_u64;
        let mut observed = false;
        for recorder in recorders {
            let (pairs, bytes) = recorder.pipeline_load();
            outstanding_pairs = outstanding_pairs.saturating_add(pairs);
            retained_bytes = retained_bytes.saturating_add(bytes);
            observed = true;
        }
        if !observed {
            return;
        }
        self.recorder_configured = true;
        self.recorder_outstanding_pairs = outstanding_pairs;
        self.recorder_peak_outstanding_pairs =
            self.recorder_peak_outstanding_pairs.max(outstanding_pairs);
        self.recorder_retained_bytes = retained_bytes;
        self.recorder_peak_retained_bytes = self.recorder_peak_retained_bytes.max(retained_bytes);
    }

    /// Additive v4 aggregation: whole-run stream pair admission counters. The
    /// record stays key=value, so existing consumers that look fields up by
    /// name keep parsing unchanged.
    pub(crate) fn observe_streams(&mut self, enqueued_pairs: u64, dropped_pairs: u64) {
        self.stream_enqueued_pairs_total = enqueued_pairs;
        self.stream_dropped_pairs_total = dropped_pairs;
    }

    pub(crate) fn emit(&self, presentation: Option<fm_gpu::PresentationTelemetry>) {
        eprintln!("{}", self.diagnostic(presentation));
    }

    pub(crate) fn diagnostic(&self, presentation: Option<fm_gpu::PresentationTelemetry>) -> String {
        let host = self
            .metrics
            .series(Metric::LatencyMilliseconds)
            .histogram_summary()
            .unwrap_or_default();
        let gpu = self
            .metrics
            .series(Metric::GpuTimeMilliseconds)
            .histogram_summary()
            .unwrap_or_default();
        let presentation_active = presentation.is_some();
        let presentation = presentation.unwrap_or_default();
        format!(
            "FREEMIXD_TELEMETRY\tv=4\thost_lateness_samples_total={}\thost_lateness_samples_retained={}\thost_lateness_metric_samples_dropped={}\thost_lateness_p50_ms={}\thost_lateness_p95_ms={}\thost_lateness_p99_ms={}\taudio_retained_blocks={}\taudio_observed_peak_retained_blocks={}\taudio_retained_samples={}\taudio_observed_peak_retained_samples={}\taudio_retained_bytes={}\taudio_observed_peak_retained_bytes={}\taudio_reservation_requests={}\taudio_reserved_blocks={}\taudio_observed_peak_reserved_blocks={}\taudio_reserved_samples={}\taudio_observed_peak_reserved_samples={}\taudio_reserved_bytes={}\taudio_observed_peak_reserved_bytes={}\taudio_source_stalls={}\taudio_positioned_blocks={}\taudio_positioned_samples={}\taudio_leading_silence_samples={}\taudio_eos_padding_blocks={}\taudio_eos_padding_samples={}\taudio_sink_depth={}\taudio_sink_peak_depth={}\taudio_sink_dropped={}\tcamera_configured_sources={}\tcamera_frames_received={}\tcamera_frames_ingested={}\tcamera_native_dropped={}\tcamera_queue_depth={}\tcamera_queue_peak_depth={}\tcamera_queue_dropped={}\tcamera_continuity_rejected={}\tcamera_recovery_timeout_discarded={}\tcamera_terminal_error_discarded={}\tcamera_terminal_trigger_discarded={}\tcamera_ready_delivery_depth={}\tcamera_ready_delivery_discarded={}\tcamera_cancellation_discarded={}\tcamera_supervisor_slot_replaced={}\tcamera_supervisor_slot_depth={}\tcamera_ingest_failed={}\tcamera_preflight_depth={}\tcamera_preflight_discarded={}\tcamera_recovery_attempts={}\tcamera_recovery_successes={}\tcamera_recovery_exhausted={}\tcamera_recovery_worker_failures={}\tpresentation_active={}\tpresentation_pending_depth={}\tpresentation_peak_pending_depth={}\tpresentation_dropped={}\trecorder_configured={}\trecorder_outstanding_pairs={}\trecorder_observed_peak_outstanding_pairs={}\trecorder_retained_bytes={}\trecorder_observed_peak_retained_bytes={}\tstream_pairs_enqueued_total={}\tstream_pairs_dropped_total={}\tmonitor_frames_published={}\tmonitor_frames_failed={}\tmonitor_frames_skipped={}\tpreview_monitor_frames_published={}\tpreview_monitor_frames_failed={}\tpreview_monitor_frames_skipped={}\tgpu_backend={:?}\tgpu_adapter={}\tgpu_timing={:?}\tgpu_pass_samples_total={}\tgpu_pass_samples_retained={}\tgpu_pass_metric_samples_dropped={}\tgpu_pass_p50_ms={}\tgpu_pass_p95_ms={}\tgpu_pass_p99_ms={}\tgpu_samples_pending={}\tgpu_samples_dropped={}\tgpu_samples_unavailable={}\tmetric_errors={}\tmetric_samples_dropped={}\tdisk_alert={}\tdisk_free_bytes={}",
            host.count,
            host.retained_samples,
            host.dropped_samples,
            metric_value(host.p50),
            metric_value(host.p95),
            metric_value(host.p99),
            self.audio_retained_blocks,
            self.audio_peak_retained_blocks,
            self.audio_retained_samples,
            self.audio_peak_retained_samples,
            self.audio_retained_bytes,
            self.audio_peak_retained_bytes,
            self.audio_reservation_requests,
            self.audio_reserved_blocks,
            self.audio_peak_reserved_blocks,
            self.audio_reserved_samples,
            self.audio_peak_reserved_samples,
            self.audio_reserved_bytes,
            self.audio_peak_reserved_bytes,
            self.audio_source_stalls,
            self.audio_positioned_blocks,
            self.audio_positioned_samples,
            self.audio_leading_silence_samples,
            self.audio_eos_padding_blocks,
            self.audio_eos_padding_samples,
            self.audio_sink_depth,
            self.audio_sink_peak_depth,
            self.audio_sink_dropped,
            self.camera.configured_sources,
            self.camera.frames_received,
            self.camera.frames_ingested,
            self.camera.native_dropped,
            self.camera.queue_depth,
            self.camera.queue_peak_depth,
            self.camera.queue_dropped,
            self.camera.continuity_rejected,
            self.camera.recovery_timeout_discarded,
            self.camera.terminal_error_discarded,
            self.camera.terminal_trigger_discarded,
            self.camera.ready_delivery_depth,
            self.camera.ready_delivery_discarded,
            self.camera.cancellation_discarded,
            self.camera.supervisor_slot_replaced,
            self.camera.supervisor_slot_depth,
            self.camera.ingest_failed,
            self.camera.preflight_depth,
            self.camera.preflight_discarded,
            self.camera.recovery_attempts,
            self.camera.recovery_successes,
            self.camera.recovery_exhausted,
            self.camera.recovery_worker_failures,
            presentation_active,
            presentation.pending_depth,
            presentation.peak_pending_depth,
            presentation.frames_dropped,
            self.recorder_configured,
            self.recorder_outstanding_pairs,
            self.recorder_peak_outstanding_pairs,
            self.recorder_retained_bytes,
            self.recorder_peak_retained_bytes,
            self.stream_enqueued_pairs_total,
            self.stream_dropped_pairs_total,
            self.monitor_taps[MonitorTap::Program.index()].published,
            self.monitor_taps[MonitorTap::Program.index()].failed,
            self.monitor_taps[MonitorTap::Program.index()].skipped,
            self.monitor_taps[MonitorTap::Preview.index()].published,
            self.monitor_taps[MonitorTap::Preview.index()].failed,
            self.monitor_taps[MonitorTap::Preview.index()].skipped,
            self.gpu_backend,
            diagnostic_field(&self.gpu_adapter),
            self.gpu_support,
            gpu.count,
            gpu.retained_samples,
            gpu.dropped_samples,
            metric_value(gpu.p50),
            metric_value(gpu.p95),
            metric_value(gpu.p99),
            self.gpu_pending_samples,
            self.gpu_dropped_samples,
            self.gpu_unavailable_samples,
            self.metric_errors,
            self.metrics.dropped_samples(),
            disk_alert_label(self.disk_alert),
            self.disk_free_bytes.unwrap_or(0),
        )
    }

    pub(crate) fn monotonic_millis(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(crate) fn record_metric_result(
        &mut self,
        result: Result<(), fm_observability::MetricError>,
    ) {
        if result.is_err() {
            self.metric_errors = self.metric_errors.saturating_add(1);
        }
    }
}

#[cfg(feature = "native-media")]
pub(crate) fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(feature = "native-media")]
pub(crate) fn metric_value(value: Option<f64>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| format!("{value:.3}"))
}

/// Renders the telemetry label for the recording free-space alert; `None`
/// means no sample has ever succeeded.
#[cfg(feature = "native-media")]
pub(crate) const fn disk_alert_label(state: Option<AlertState>) -> &'static str {
    match state {
        Some(AlertState::Firing) => "low",
        Some(AlertState::Inactive) => "normal",
        None => "unknown",
    }
}
