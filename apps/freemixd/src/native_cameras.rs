//! The macOS camera inputs: capture sources, their supervisor threads, the
//! preflight frame handoff, and the telemetry they report.
//!
//! Extracted from `main.rs` as a mechanical move; the block is compiled only
//! for a native daemon on macOS.

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[allow(clippy::wildcard_imports)] // the binary's items and imports
use super::*;

pub(crate) struct NativeCameraInput {
    pub(crate) input: InputId,
    pub(crate) source: Option<CameraVideoSource>,
    pub(crate) worker: Option<NativeCameraWorker>,
    pub(crate) supervisor: Arc<Mutex<CameraSupervisorState>>,
    pub(crate) recovery_policy: CameraRecoveryPolicy,
    pub(crate) ingested_frames: u64,
    pub(crate) ingest_failed: u64,
    pub(crate) preflight_depth: u64,
    pub(crate) preflight_discarded: u64,
    pub(crate) last_ingested_sequence: Option<u64>,
    pub(crate) last_ingested_discontinuity: bool,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Default)]
pub(crate) struct CameraFrameSlot {
    pub(crate) frame: Option<CpuVideoFrame>,
    pub(crate) replacements: u64,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) struct CameraSupervisorState {
    pub(crate) frame: CameraFrameSlot,
    pub(crate) snapshot: CameraWorkerSnapshot,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl CameraFrameSlot {
    pub(crate) fn replace(&mut self, mut frame: CpuVideoFrame) {
        if self.frame.as_ref().is_some_and(|pending| {
            pending
                .timing()
                .flags()
                .contains(fm_frame::MediaFlags::DISCONTINUITY)
        }) && !frame
            .timing()
            .flags()
            .contains(fm_frame::MediaFlags::DISCONTINUITY)
        {
            frame = frame_with_discontinuity(frame);
        }
        if self.frame.replace(frame).is_some() {
            self.replacements = self.replacements.saturating_add(1);
        }
    }

    pub(crate) fn take(&mut self) -> Option<CpuVideoFrame> {
        self.frame.take()
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn frame_with_discontinuity(frame: CpuVideoFrame) -> CpuVideoFrame {
    let timing = frame
        .timing()
        .with_flags(frame.timing().flags() | fm_frame::MediaFlags::DISCONTINUITY);
    let metadata = frame.metadata();
    let frame = CpuVideoFrame::new(timing, frame.into_payload());
    if let Some(metadata) = metadata {
        frame.clone().with_metadata(metadata).unwrap_or(frame)
    } else {
        frame
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Clone, Copy)]
pub(crate) struct CameraRecoveryPolicy {
    pub(crate) max_attempts: u32,
    pub(crate) initial_backoff: Duration,
    pub(crate) max_backoff: Duration,
    pub(crate) rearm_backoff: Duration,
    pub(crate) shutdown_timeout: Duration,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CameraRecoveryOutcome {
    #[default]
    Never,
    Pending,
    Recovered,
    Exhausted,
    Rearming,
    WorkerFailed,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) struct NativeCameraWorker {
    pub(crate) handle: Option<JoinHandle<CameraWorkerResult>>,
    pub(crate) cancel: Arc<AtomicBool>,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) struct CameraWorkerResult {
    pub(crate) failure: Option<IoError>,
    pub(crate) cleanup_failure: Option<IoError>,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Clone, Copy)]
pub(crate) struct CameraWorkerSnapshot {
    pub(crate) telemetry: CameraTelemetry,
    pub(crate) lifecycle: LifecycleState,
    pub(crate) health: EndpointHealthState,
    pub(crate) recovery_attempts: u64,
    pub(crate) recovery_successes: u64,
    pub(crate) recovery_exhausted: u64,
    pub(crate) recovery_worker_failures: u64,
    pub(crate) recovery_outcome: CameraRecoveryOutcome,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl CameraWorkerSnapshot {
    pub(crate) fn running(source: &CameraVideoSource) -> Self {
        Self {
            telemetry: source.telemetry(),
            lifecycle: source.lifecycle(),
            health: source.health().state,
            recovery_attempts: 0,
            recovery_successes: 0,
            recovery_exhausted: 0,
            recovery_worker_failures: 0,
            recovery_outcome: CameraRecoveryOutcome::Never,
        }
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Default)]
pub(crate) struct NativeCameraInputs {
    pub(crate) inputs: Vec<NativeCameraInput>,
    pub(crate) telemetry_emitted: bool,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl NativeCameraInputs {
    #[cfg(test)]
    pub(crate) fn poll_into(
        &mut self,
        runtime: &NativeMediaRuntime,
        playback: &mut freemixd::native_media::NativeSourcePlayback,
    ) -> AppResult<()> {
        self.poll_with(|input, frame| {
            runtime.ingest_live_video_frame_blocking(playback, input, frame)?;
            Ok(())
        })
    }

    pub(crate) fn poll_with(
        &mut self,
        mut ingest: impl FnMut(InputId, CpuVideoFrame) -> AppResult<()>,
    ) -> AppResult<()> {
        for camera in &mut self.inputs {
            Self::collect_finished_worker(camera)?;
            let frame = camera
                .supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .frame
                .take();
            if let Some(frame) = frame {
                let timing = frame.timing();
                if let Err(error) = ingest(camera.input, frame) {
                    camera.ingest_failed = camera.ingest_failed.saturating_add(1);
                    return Err(error);
                }
                camera.ingested_frames = camera.ingested_frames.saturating_add(1);
                camera.last_ingested_sequence = Some(timing.sequence().get());
                camera.last_ingested_discontinuity =
                    timing.flags().contains(fm_frame::MediaFlags::DISCONTINUITY);
            }
        }
        Ok(())
    }

    pub(crate) fn collect_finished_worker(camera: &mut NativeCameraInput) -> AppResult<()> {
        let finished = camera
            .worker
            .as_ref()
            .and_then(|worker| worker.handle.as_ref())
            .is_some_and(JoinHandle::is_finished);
        if !finished {
            return Ok(());
        }
        let Some(mut worker) = camera.worker.take() else {
            return Ok(());
        };
        let Some(handle) = worker.handle.take() else {
            return Ok(());
        };
        if let Ok(result) = handle.join() {
            if let Some(failure) = result.failure.or(result.cleanup_failure) {
                Err(failure.into())
            } else {
                Err(AppFailure("camera worker exited unexpectedly".to_owned()).into())
            }
        } else {
            let mut snapshot = camera
                .supervisor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            snapshot.snapshot.recovery_worker_failures =
                snapshot.snapshot.recovery_worker_failures.saturating_add(1);
            snapshot.snapshot.recovery_outcome = CameraRecoveryOutcome::WorkerFailed;
            snapshot.snapshot.lifecycle = LifecycleState::Lost;
            snapshot.snapshot.health = EndpointHealthState::Failed;
            Err(AppFailure("camera worker panicked".to_owned()).into())
        }
    }

    pub(crate) fn start_workers(&mut self) -> AppResult<()> {
        for camera in &mut self.inputs {
            let Some(source) = camera.source.take() else {
                continue;
            };
            let supervisor = Arc::clone(&camera.supervisor);
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = Arc::clone(&cancel);
            let policy = camera.recovery_policy;
            let handle = thread::Builder::new()
                .name("freemix-camera-supervisor".to_owned())
                .spawn(move || supervise_camera(source, policy, &worker_cancel, &supervisor))?;
            camera.worker = Some(NativeCameraWorker {
                handle: Some(handle),
                cancel,
            });
        }
        Ok(())
    }

    pub(crate) fn cleanup_startup_sources(&mut self, timeout: Duration) -> AppResult<()> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut handles = Vec::new();
        for camera in &mut self.inputs {
            if let Some(mut source) = camera.source.take() {
                handles.push(thread::spawn(move || close_camera_source(&mut source)));
            }
        }
        while handles.iter().any(|handle| !handle.is_finished()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let mut failure = None;
        for handle in handles {
            if handle.is_finished() {
                match handle.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        failure.get_or_insert_with(|| "camera source cleanup failed".to_owned());
                    }
                    Err(_) => {
                        failure.get_or_insert_with(|| "camera cleanup worker panicked".to_owned());
                    }
                }
            } else {
                failure.get_or_insert_with(|| {
                    "camera source cleanup missed the aggregate deadline".to_owned()
                });
            }
        }
        failure.map_or(Ok(()), |detail| Err(AppFailure(detail).into()))
    }

    pub(crate) fn source_telemetry(&self) -> Vec<NativeCameraSourceTelemetry> {
        let mut sources = self
            .inputs
            .iter()
            .map(|camera| {
                let state = camera
                    .supervisor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let snapshot = state.snapshot;
                NativeCameraSourceTelemetry {
                    input: camera.input,
                    lifecycle: snapshot.lifecycle,
                    health: snapshot.health,
                    frames_received: snapshot.telemetry.received,
                    frames_ingested: camera.ingested_frames,
                    native_dropped: snapshot.telemetry.native_dropped,
                    queue_dropped: snapshot.telemetry.dropped,
                    queue_depth: saturating_u64(snapshot.telemetry.current),
                    queue_peak_depth: saturating_u64(snapshot.telemetry.peak),
                    continuity_rejected: snapshot.telemetry.continuity_rejected,
                    recovery_timeout_discarded: snapshot.telemetry.recovery_timeout_discarded,
                    terminal_error_discarded: snapshot.telemetry.terminal_error_discarded,
                    terminal_trigger_discarded: snapshot.telemetry.terminal_trigger_discarded,
                    ready_delivery_depth: snapshot.telemetry.ready_delivery_depth,
                    ready_delivery_discarded: snapshot.telemetry.ready_delivery_discarded,
                    cancellation_discarded: snapshot.telemetry.cancellation_discarded,
                    supervisor_slot_replaced: state.frame.replacements,
                    supervisor_slot_depth: u64::from(state.frame.frame.is_some()),
                    ingest_failed: camera.ingest_failed,
                    preflight_depth: camera.preflight_depth,
                    preflight_discarded: camera.preflight_discarded,
                    recovery_attempts: snapshot.recovery_attempts,
                    recovery_successes: snapshot.recovery_successes,
                    recovery_exhausted: snapshot.recovery_exhausted,
                    recovery_worker_failures: snapshot.recovery_worker_failures,
                    recovery_outcome: snapshot.recovery_outcome,
                }
            })
            .collect::<Vec<_>>();
        sort_camera_source_telemetry(&mut sources);
        sources
    }

    pub(crate) fn emit_source_telemetry(&mut self) -> Option<Vec<NativeCameraSourceTelemetry>> {
        if self.telemetry_emitted {
            return None;
        }
        let sources = self.source_telemetry();
        for source in &sources {
            eprintln!("{}", source.diagnostic());
        }
        self.telemetry_emitted = true;
        Some(sources)
    }

    pub(crate) fn mark_preflight_frames_ingested(&mut self) {
        for camera in &mut self.inputs {
            camera.ingested_frames = camera
                .ingested_frames
                .saturating_add(camera.preflight_depth);
            camera.preflight_depth = 0;
        }
    }

    pub(crate) fn discard_preflight_frames(&mut self) {
        for camera in &mut self.inputs {
            camera.preflight_discarded = camera
                .preflight_discarded
                .saturating_add(camera.preflight_depth);
            camera.preflight_depth = 0;
        }
    }

    pub(crate) fn shutdown(&mut self) -> AppResult<()> {
        for camera in &mut self.inputs {
            if let Some(worker) = &camera.worker {
                worker.cancel.store(true, AtomicOrdering::Release);
                if let Some(handle) = &worker.handle {
                    handle.thread().unpark();
                }
            }
        }
        let timeout = self
            .inputs
            .iter()
            .map(|camera| camera.recovery_policy.shutdown_timeout)
            .max()
            .unwrap_or(Duration::ZERO);
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        while self.inputs.iter().any(|camera| {
            camera
                .worker
                .as_ref()
                .and_then(|worker| worker.handle.as_ref())
                .is_some_and(|handle| !handle.is_finished())
        }) && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }

        let mut failure = None;
        for camera in &mut self.inputs {
            if let Some(mut worker) = camera.worker.take()
                && let Some(handle) = worker.handle.take()
            {
                if handle.is_finished() {
                    match handle.join() {
                        Ok(result) => {
                            if result.failure.is_some() || result.cleanup_failure.is_some() {
                                failure.get_or_insert_with(|| {
                                    "camera worker failed during shutdown".to_owned()
                                });
                            }
                        }
                        Err(_) => {
                            failure.get_or_insert_with(|| {
                                "camera worker failed during shutdown".to_owned()
                            });
                        }
                    }
                } else {
                    failure.get_or_insert_with(|| {
                        "camera workers missed the aggregate shutdown deadline".to_owned()
                    });
                }
            }
        }
        if self
            .cleanup_startup_sources(CAMERA_STARTUP_CLEANUP_TIMEOUT)
            .is_err()
        {
            failure.get_or_insert_with(|| "camera source cleanup failed".to_owned());
        }
        failure.map_or(Ok(()), |detail| Err(AppFailure(detail).into()))
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn supervise_camera(
    mut source: CameraVideoSource,
    policy: CameraRecoveryPolicy,
    cancel: &AtomicBool,
    supervisor: &Mutex<CameraSupervisorState>,
) -> CameraWorkerResult {
    loop {
        if cancel.load(AtomicOrdering::Acquire) {
            return finish_camera_worker(&mut source, None, supervisor);
        }
        update_camera_worker_snapshot(supervisor, &source);
        match source.try_receive() {
            Ok(Some(MediaTransfer::Live(next))) => {
                let mut state = supervisor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                update_camera_worker_snapshot_inner(&mut state.snapshot, &source);
                state.frame.replace(next);
                thread::park_timeout(NATIVE_IO_POLL_INTERVAL);
                continue;
            }
            Ok(None) => {
                thread::park_timeout(NATIVE_IO_POLL_INTERVAL);
                continue;
            }
            Ok(Some(MediaTransfer::Fallback { .. })) => {}
            Err(error) if recoverable_camera_error(&error) => {}
            Err(error) => return finish_camera_worker(&mut source, Some(error), supervisor),
        }

        loop {
            let mut attempts = 0_u32;
            let mut backoff = policy.initial_backoff;
            while attempts < policy.max_attempts {
                if wait_for_camera_worker(cancel, backoff) {
                    return finish_camera_worker(&mut source, None, supervisor);
                }
                attempts = attempts.saturating_add(1);
                {
                    let mut state = supervisor
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.snapshot.recovery_attempts =
                        state.snapshot.recovery_attempts.saturating_add(1);
                    state.snapshot.recovery_outcome = CameraRecoveryOutcome::Pending;
                    state.snapshot.lifecycle = LifecycleState::Recovering;
                    state.snapshot.health = EndpointHealthState::SignalLost;
                }
                let result = source
                    .begin_recovery()
                    .and_then(|()| source.finish_recovery());
                update_camera_worker_snapshot(supervisor, &source);
                match result {
                    Ok(()) => {
                        let mut state = supervisor
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.snapshot.recovery_successes =
                            state.snapshot.recovery_successes.saturating_add(1);
                        state.snapshot.recovery_outcome = CameraRecoveryOutcome::Recovered;
                        break;
                    }
                    Err(error) if recoverable_camera_error(&error) => {}
                    Err(error) => {
                        return finish_camera_worker(&mut source, Some(error), supervisor);
                    }
                }
                backoff = backoff.saturating_mul(2).min(policy.max_backoff);
            }
            if source.lifecycle() == LifecycleState::Running {
                break;
            }
            {
                let mut state = supervisor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.snapshot.recovery_exhausted =
                    state.snapshot.recovery_exhausted.saturating_add(1);
                state.snapshot.recovery_outcome = CameraRecoveryOutcome::Exhausted;
                state.snapshot.lifecycle = LifecycleState::Lost;
                state.snapshot.health = EndpointHealthState::SignalLost;
                state.snapshot.recovery_outcome = CameraRecoveryOutcome::Rearming;
            }
            if wait_for_camera_worker(cancel, policy.rearm_backoff) {
                return finish_camera_worker(&mut source, None, supervisor);
            }
        }
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) fn recoverable_camera_error(error: &IoError) -> bool {
    match error {
        IoError::SignalLost { .. } => true,
        IoError::AdapterFailure {
            remediation: Some(remediation),
            ..
        }
        | IoError::EndpointUnavailable { remediation }
        | IoError::DriverUnavailable { remediation } => matches!(
            remediation,
            Remediation::ReconnectDevice | Remediation::RestartAdapter
        ),
        IoError::InvalidState { .. }
        | IoError::UnsupportedFormat
        | IoError::UnsupportedClock
        | IoError::UnsupportedMemoryDomain
        | IoError::QueueCapacityUnsupported { .. }
        | IoError::PermissionDenied { .. }
        | IoError::MediaTooLarge { .. }
        | IoError::MalformedTimestamp(_)
        | IoError::AdapterFailure {
            remediation: None, ..
        } => false,
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn wait_for_camera_worker(cancel: &AtomicBool, duration: Duration) -> bool {
    let deadline = Instant::now()
        .checked_add(duration)
        .unwrap_or_else(Instant::now);
    while !cancel.load(AtomicOrdering::Acquire) && Instant::now() < deadline {
        thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    }
    cancel.load(AtomicOrdering::Acquire)
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn update_camera_worker_snapshot(
    supervisor: &Mutex<CameraSupervisorState>,
    source: &CameraVideoSource,
) {
    let mut state = supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    update_camera_worker_snapshot_inner(&mut state.snapshot, source);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) fn update_camera_worker_snapshot_inner(
    snapshot: &mut CameraWorkerSnapshot,
    source: &CameraVideoSource,
) {
    snapshot.telemetry = source.telemetry();
    snapshot.lifecycle = source.lifecycle();
    snapshot.health = source.health().state;
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn finish_camera_worker(
    source: &mut CameraVideoSource,
    failure: Option<IoError>,
    supervisor: &Mutex<CameraSupervisorState>,
) -> CameraWorkerResult {
    let cleanup_failure = close_camera_source(source).err();
    update_camera_worker_snapshot(supervisor, source);
    if failure.is_some() || cleanup_failure.is_some() {
        let mut state = supervisor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.snapshot.recovery_worker_failures =
            state.snapshot.recovery_worker_failures.saturating_add(1);
        state.snapshot.recovery_outcome = CameraRecoveryOutcome::WorkerFailed;
        state.snapshot.health = EndpointHealthState::Failed;
    }
    CameraWorkerResult {
        failure,
        cleanup_failure,
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn close_camera_source(source: &mut CameraVideoSource) -> Result<(), IoError> {
    if matches!(
        source.lifecycle(),
        LifecycleState::Running | LifecycleState::Recovering
    ) {
        source.stop()?;
    }
    if matches!(
        source.lifecycle(),
        LifecycleState::Open | LifecycleState::Lost
    ) {
        source.close()?;
    }
    Ok(())
}

#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NativeCameraTelemetry {
    pub(crate) configured_sources: u64,
    pub(crate) frames_received: u64,
    pub(crate) frames_ingested: u64,
    pub(crate) native_dropped: u64,
    pub(crate) queue_dropped: u64,
    pub(crate) queue_depth: u64,
    pub(crate) queue_peak_depth: u64,
    pub(crate) continuity_rejected: u64,
    pub(crate) recovery_timeout_discarded: u64,
    pub(crate) terminal_error_discarded: u64,
    pub(crate) terminal_trigger_discarded: u64,
    pub(crate) ready_delivery_depth: u64,
    pub(crate) ready_delivery_discarded: u64,
    pub(crate) cancellation_discarded: u64,
    pub(crate) supervisor_slot_replaced: u64,
    pub(crate) supervisor_slot_depth: u64,
    pub(crate) ingest_failed: u64,
    pub(crate) preflight_depth: u64,
    pub(crate) preflight_discarded: u64,
    pub(crate) recovery_attempts: u64,
    pub(crate) recovery_successes: u64,
    pub(crate) recovery_exhausted: u64,
    pub(crate) recovery_worker_failures: u64,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeCameraSourceTelemetry {
    pub(crate) input: InputId,
    pub(crate) lifecycle: LifecycleState,
    pub(crate) health: EndpointHealthState,
    pub(crate) frames_received: u64,
    pub(crate) frames_ingested: u64,
    pub(crate) native_dropped: u64,
    pub(crate) queue_dropped: u64,
    pub(crate) queue_depth: u64,
    pub(crate) queue_peak_depth: u64,
    pub(crate) continuity_rejected: u64,
    pub(crate) recovery_timeout_discarded: u64,
    pub(crate) terminal_error_discarded: u64,
    pub(crate) terminal_trigger_discarded: u64,
    pub(crate) ready_delivery_depth: u64,
    pub(crate) ready_delivery_discarded: u64,
    pub(crate) cancellation_discarded: u64,
    pub(crate) supervisor_slot_replaced: u64,
    pub(crate) supervisor_slot_depth: u64,
    pub(crate) ingest_failed: u64,
    pub(crate) preflight_depth: u64,
    pub(crate) preflight_discarded: u64,
    pub(crate) recovery_attempts: u64,
    pub(crate) recovery_successes: u64,
    pub(crate) recovery_exhausted: u64,
    pub(crate) recovery_worker_failures: u64,
    pub(crate) recovery_outcome: CameraRecoveryOutcome,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl NativeCameraSourceTelemetry {
    pub(crate) fn diagnostic(self) -> String {
        format!(
            "FREEMIXD_CAMERA_SOURCE\tv=1\tclassification=diagnostic-not-certification\tinput_id={}\tsample_phase=pre_cleanup\tsample_lifecycle={}\thealth={}\tframes_received={}\tframes_ingested={}\tnative_dropped={}\tqueue_depth={}\tqueue_peak_depth={}\tqueue_dropped={}\tcontinuity_rejected={}\trecovery_timeout_discarded={}\tterminal_error_discarded={}\tterminal_trigger_discarded={}\tready_delivery_depth={}\tready_delivery_discarded={}\tcancellation_discarded={}\tsupervisor_slot_replaced={}\tsupervisor_slot_depth={}\tingest_failed={}\tpreflight_depth={}\tpreflight_discarded={}\trecovery_attempts={}\trecovery_successes={}\trecovery_exhausted={}\trecovery_worker_failures={}\trecovery_outcome={}",
            self.input,
            lifecycle_label(self.lifecycle),
            health_label(self.health),
            self.frames_received,
            self.frames_ingested,
            self.native_dropped,
            self.queue_depth,
            self.queue_peak_depth,
            self.queue_dropped,
            self.continuity_rejected,
            self.recovery_timeout_discarded,
            self.terminal_error_discarded,
            self.terminal_trigger_discarded,
            self.ready_delivery_depth,
            self.ready_delivery_discarded,
            self.cancellation_discarded,
            self.supervisor_slot_replaced,
            self.supervisor_slot_depth,
            self.ingest_failed,
            self.preflight_depth,
            self.preflight_discarded,
            self.recovery_attempts,
            self.recovery_successes,
            self.recovery_exhausted,
            self.recovery_worker_failures,
            recovery_outcome_label(self.recovery_outcome),
        )
    }

    #[cfg(test)]
    pub(crate) fn accounted_frames(self) -> u64 {
        self.frames_ingested
            .saturating_add(self.queue_dropped)
            .saturating_add(self.continuity_rejected)
            .saturating_add(self.recovery_timeout_discarded)
            .saturating_add(self.terminal_error_discarded)
            .saturating_add(self.terminal_trigger_discarded)
            .saturating_add(self.ready_delivery_depth)
            .saturating_add(self.ready_delivery_discarded)
            .saturating_add(self.cancellation_discarded)
            .saturating_add(self.supervisor_slot_replaced)
            .saturating_add(self.queue_depth)
            .saturating_add(self.supervisor_slot_depth)
            .saturating_add(self.ingest_failed)
            .saturating_add(self.preflight_depth)
            .saturating_add(self.preflight_discarded)
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) fn aggregate_camera_telemetry(
    sources: &[NativeCameraSourceTelemetry],
) -> NativeCameraTelemetry {
    let mut aggregate = NativeCameraTelemetry {
        configured_sources: saturating_u64(sources.len()),
        ..NativeCameraTelemetry::default()
    };
    for source in sources {
        aggregate.frames_received = aggregate
            .frames_received
            .saturating_add(source.frames_received);
        aggregate.frames_ingested = aggregate
            .frames_ingested
            .saturating_add(source.frames_ingested);
        aggregate.native_dropped = aggregate
            .native_dropped
            .saturating_add(source.native_dropped);
        aggregate.queue_dropped = aggregate.queue_dropped.saturating_add(source.queue_dropped);
        aggregate.queue_depth = aggregate.queue_depth.saturating_add(source.queue_depth);
        aggregate.queue_peak_depth = aggregate
            .queue_peak_depth
            .saturating_add(source.queue_peak_depth);
        aggregate.continuity_rejected = aggregate
            .continuity_rejected
            .saturating_add(source.continuity_rejected);
        aggregate.recovery_timeout_discarded = aggregate
            .recovery_timeout_discarded
            .saturating_add(source.recovery_timeout_discarded);
        aggregate.terminal_error_discarded = aggregate
            .terminal_error_discarded
            .saturating_add(source.terminal_error_discarded);
        aggregate.terminal_trigger_discarded = aggregate
            .terminal_trigger_discarded
            .saturating_add(source.terminal_trigger_discarded);
        aggregate.ready_delivery_depth = aggregate
            .ready_delivery_depth
            .saturating_add(source.ready_delivery_depth);
        aggregate.ready_delivery_discarded = aggregate
            .ready_delivery_discarded
            .saturating_add(source.ready_delivery_discarded);
        aggregate.cancellation_discarded = aggregate
            .cancellation_discarded
            .saturating_add(source.cancellation_discarded);
        aggregate.supervisor_slot_replaced = aggregate
            .supervisor_slot_replaced
            .saturating_add(source.supervisor_slot_replaced);
        aggregate.supervisor_slot_depth = aggregate
            .supervisor_slot_depth
            .saturating_add(source.supervisor_slot_depth);
        aggregate.ingest_failed = aggregate.ingest_failed.saturating_add(source.ingest_failed);
        aggregate.preflight_depth = aggregate
            .preflight_depth
            .saturating_add(source.preflight_depth);
        aggregate.preflight_discarded = aggregate
            .preflight_discarded
            .saturating_add(source.preflight_discarded);
        aggregate.recovery_attempts = aggregate
            .recovery_attempts
            .saturating_add(source.recovery_attempts);
        aggregate.recovery_successes = aggregate
            .recovery_successes
            .saturating_add(source.recovery_successes);
        aggregate.recovery_exhausted = aggregate
            .recovery_exhausted
            .saturating_add(source.recovery_exhausted);
        aggregate.recovery_worker_failures = aggregate
            .recovery_worker_failures
            .saturating_add(source.recovery_worker_failures);
    }
    aggregate
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
const fn recovery_outcome_label(outcome: CameraRecoveryOutcome) -> &'static str {
    match outcome {
        CameraRecoveryOutcome::Never => "never",
        CameraRecoveryOutcome::Pending => "pending",
        CameraRecoveryOutcome::Recovered => "recovered",
        CameraRecoveryOutcome::Exhausted => "exhausted",
        CameraRecoveryOutcome::Rearming => "rearming",
        CameraRecoveryOutcome::WorkerFailed => "worker_failed",
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) fn sort_camera_source_telemetry(sources: &mut [NativeCameraSourceTelemetry]) {
    sources.sort_by_key(|source| source.input);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) const fn lifecycle_label(state: LifecycleState) -> &'static str {
    match state {
        LifecycleState::Closed => "closed",
        LifecycleState::Open => "open",
        LifecycleState::Running => "running",
        LifecycleState::Lost => "lost",
        LifecycleState::Recovering => "recovering",
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
pub(crate) const fn health_label(state: EndpointHealthState) -> &'static str {
    match state {
        EndpointHealthState::Healthy => "healthy",
        EndpointHealthState::Degraded => "degraded",
        EndpointHealthState::SignalLost => "signal_lost",
        EndpointHealthState::Failed => "failed",
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl Drop for NativeCameraInputs {
    fn drop(&mut self) {
        self.discard_preflight_frames();
        let _ = self.emit_source_telemetry();
        let _ = self.shutdown();
    }
}
