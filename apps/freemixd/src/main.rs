#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::{
    cell::{Cell, RefCell},
    convert::Infallible,
    error::Error,
    fmt::{self, Write as _},
    io::Write,
    net::{SocketAddr, TcpListener},
    num::{NonZeroU32, NonZeroU128},
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(any(test, feature = "native-media"))]
use std::io::Read;
#[cfg(any(test, feature = "native-media"))]
use std::thread;
#[cfg(test)]
use std::{collections::VecDeque, net::TcpStream};

#[cfg(feature = "native-media")]
use std::fs::{self, File};
#[cfg(feature = "native-media")]
use std::sync::mpsc;
#[cfg(all(feature = "native-media", target_os = "macos"))]
use std::{collections::BTreeMap, num::NonZeroUsize, sync::Mutex, thread::JoinHandle};

use fm_auth::{Policy, Principal, Role as AuthRole, SessionId, UserId};
use fm_clock::{ClockDomainId, ClockTime};
use fm_command::{
    AcceptedReceipt, CommandId, CommandReceipt, EventSequence, IdempotencyKey, RejectedReceipt,
    Rejection, RejectionCode, Revision, RuntimeGeneration, StateEpoch,
};
use fm_control::{
    CommandSubmission, ControlLimits, ControlService, PrepareSubmitOutcome, ResultBeforeEvents,
    ResumeDecision,
};
use fm_daemon_arguments::{Command, RECORDER_SLOTS, parse_args, print_help};
use fm_daemon_render::{grading, monitor_tap, scene_render, stills, title_render};
#[cfg(feature = "native-media")]
use fm_engine::FrameResult;
use fm_engine::{
    Engine, EngineAcceptance, EngineInputAudioStripState, EngineRestoreState, EngineSnapshot,
    EngineTitle, ShowState,
};
#[cfg(feature = "native-media")]
use fm_io_api::display::DisplayOutputSource;
use fm_io_api::display::{DisplayOutputConfig, DisplayOutputSet, DisplayOutputSlot};
use fm_model::{
    InputAudioStripState, InputBalanceBasisPoints, InputDelaySamples, InputGainMilliDb, MainMix,
    RecorderSlot as ModelRecorderSlot, StingerAudioPolicy as ModelStingerAudioPolicy,
    StingerConfig, StingerMissingMediaFallback, StingerSlotNumber,
};
use fm_persistence::{
    FadeToBlackState as PersistedFadeToBlackState, IdempotencyReceipt,
    ManualTransitionKind as PersistedManualTransitionKind,
    ManualTransitionState as PersistedManualTransitionState, MutationBatch, ProjectPosition,
    ProjectStore, ReceiptOutcome, RuntimeFadeToBlack, RuntimeManualTransitions,
    RuntimeOverlayBorder, RuntimeOverlayChannel, RuntimeOverlayPosition, RuntimeOverlayTransition,
    RuntimeOverlays, RuntimeRouting, StoredProject,
};
use fm_protocol::{
    AUDIO_METER_LEVEL_SCALE, AudioMeterChannel, AudioMetersMessage, CURRENT_PROTOCOL_VERSION,
    CapabilityReportSummary, CommandMessage, CommandPayload, CommandResult, DiagnosticsRequest,
    DiagnosticsResponse, ErrorMessage, EventCursor, HandshakeOutcome as ProtocolHandshakeOutcome,
    HandshakeRequest, HandshakeResponse, HeartbeatMessage, InputAudioMeters, MonitorTap,
    ProtocolVersion, RecorderStatusMessage, RecordingStatus, ResumeCursor, RuntimeEventMessage,
    ServerHello, ServerIdentity, StreamStatus, StreamStatusMessage, StructuredError, WireInputId,
    WireMessage, WireStreamTargetId, choose_handshake_outcome, encode_line,
};
#[cfg(test)]
use fm_protocol::{CodecError, EventMessage, HeartbeatAcknowledgementMessage, LineDecoder};
#[cfg(feature = "native-media")]
use fm_protocol::{
    MAX_STREAM_LOCAL_SEGMENTS, MAX_STREAM_LOCAL_TARGET_DURATION_MS, MAX_STREAM_SAMPLES,
    OutputFailure, OutputFailureCode, StreamLocalStatus, StreamRealizedState, StreamStatusSample,
};
use fm_server::{
    AuthenticationMode, ControlPlane, DisconnectReason, HandshakeError, Heartbeat, InitialSync,
    Server, ServerConfig, ServerMode, ServiceStatus, Session, SessionError, SyncPayload,
};
use fm_switcher::{
    MissingMediaFallback, OverlayBorderPreset, OverlayChannelId, OverlayChannelState,
    OverlayPositionPreset, RecorderSlot, StingerAudioPolicy, StingerDescriptor, StingerSlotId,
    StreamTargetId as SwitcherStreamTargetId, SwitcherState, TBarPosition, TBarState,
    TransitionKind,
};
use fm_types::{InputId, ProjectId};
use freemixd::{ReadinessRecord, StatusReadinessRecord};

mod audio_output_devices;
// The free-space alert is constructed only by the native render loop; the
// preflight and its constants are used in every build.
mod clock_retry;
mod cue_clock;
#[cfg(feature = "native-media")]
mod desktop_capture;
#[cfg(feature = "native-media")]
mod desktop_windows;
use fm_daemon_journal::documents;
use fm_daemon_statistics::{disk, statistics};
mod hls_server;
mod input_thumbnails;
use fm_daemon_journal as journal;
mod latest_record;
use fm_daemon_web::live_lan;
mod media_clock;
mod media_lengths;
#[cfg(feature = "native-media")]
mod native_audio_realizer;
mod native_daemon;
use native_daemon::NativeDaemon;
mod native_stream_targets;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use native_stream_targets::*;
mod native_recorder;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use native_recorder::*;
mod native_sources;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use native_sources::*;
mod native_telemetry;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use native_telemetry::*;
mod native_monitor_tap;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use native_monitor_tap::*;
mod native_audio_meters;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use native_audio_meters::*;
mod recovery;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use recovery::*;
#[cfg(feature = "native-media")]
mod native_camera_telemetry;
#[cfg(feature = "native-media")]
#[allow(unused_imports, clippy::wildcard_imports)]
// the other modules name the counters through the root glob
use native_camera_telemetry::*;
#[cfg(all(feature = "native-media", target_os = "macos"))]
mod native_cameras;
#[cfg(feature = "native-media")]
mod native_captures;
#[cfg(all(feature = "native-media", target_os = "macos"))]
// The test module names several of these through its parent glob.
#[cfg_attr(not(test), allow(unused_imports))]
use native_cameras::{
    CameraFrameSlot, CameraRecoveryOutcome, CameraRecoveryPolicy, CameraSupervisorState,
    CameraWorkerResult, CameraWorkerSnapshot, NativeCameraInput, NativeCameraInputs,
    NativeCameraSourceTelemetry, NativeCameraWorker, aggregate_camera_telemetry, health_label,
    lifecycle_label, recoverable_camera_error, sort_camera_source_telemetry,
    update_camera_worker_snapshot_inner,
};
#[cfg(feature = "native-media")]
mod native_catalogue;
#[cfg(feature = "native-media")]
mod native_documents;
#[cfg(feature = "native-media")]
mod native_files;
#[cfg(feature = "native-media")]
mod native_live_lan;
#[cfg(feature = "native-media")]
mod native_portal_capture;
#[cfg(feature = "native-media")]
mod native_realizer;
#[cfg(feature = "native-media")]
mod native_reel;
#[cfg(feature = "native-media")]
mod native_replay;
#[cfg(feature = "native-media")]
mod native_screen_capture;
#[cfg(feature = "native-media")]
mod native_slides;
#[cfg(feature = "native-media")]
mod native_streams;
#[cfg(feature = "native-media")]
use freemixd::native_sync;
#[cfg(feature = "native-media")]
mod native_system_audio;
#[cfg(feature = "native-media")]
mod native_titles;
#[cfg(feature = "native-media")]
mod native_video_realizer;
#[cfg(feature = "native-media")]
mod output_failure;
#[cfg(feature = "native-media")]
mod pacing_trace;
// The inventory host and status records are wired into native daemons only;
// the parser and validation compile (and are unit-tested) in every build.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
mod display_outputs;
mod playlist_clock;
mod sessions;
mod sim_audio;
use fm_daemon_web::{web_feed, web_titles};
// The network preflight is wired only into the native daemon; every helper
// still compiles (and is unit-tested) in every build.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
mod preflight;
#[cfg(feature = "display-surface")]
mod program_surface;
#[cfg(feature = "native-media")]
mod recording_session;
mod replay_preview;
mod scene_preview;
mod screen_capture_targets;
#[cfg(feature = "native-media")]
mod source_wait;
use fm_daemon_status::status_listener;
// The recorder status tracker reports only a native daemon's recorders.
#[cfg_attr(not(feature = "native-media"), allow(unused_imports))]
use fm_daemon_status::recorder_status;
#[cfg(feature = "native-media")]
mod stinger_mutation;
#[cfg(feature = "native-media")]
mod stream_transport;
mod system_audio_devices;
mod title_preview;
use fm_daemon_web as web;

use hls_server::{HlsRoots, HlsServer};
use journal::{DurableJournal, DurableStore, ReplayedMutations};
use status_listener::{DaemonStatus, StatusListener};

#[cfg(feature = "native-media")]
use fm_codec_ffmpeg::{
    Adapter, Config as FfmpegConfig, StreamSelector, ToolAvailability,
    record::{
        CleanupStatus, EnqueueRejection, OutputFinalization, PairedFrame, RecordConfig,
        RecordFormat, Recorder, RecorderState, StopOutcome, StopReport,
    },
    stream::{EncoderSettings, StreamDestination, StreamTelemetry},
};
#[cfg(feature = "native-media")]
use fm_codec_image::{StillDecodeLimits, sniff_still_format};
#[cfg(all(feature = "native-media", target_os = "macos"))]
use fm_frame::CpuVideoFrame;
#[cfg(feature = "native-media")]
use fm_frame::{
    AudioBlock, ClockDomainId as MediaClockDomainId, CodecId, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, SequenceNumber, TimeBase,
};
#[cfg(feature = "native-media")]
use fm_gpu::{NativeBackend, NativeContext, NativeTexture};
#[cfg(all(feature = "native-media", target_os = "macos"))]
use fm_io_api::{
    EndpointHealthState, IoError, LifecycleState, MediaSource, MediaTransfer, MemoryDomain,
    OpenOptions as IoOpenOptions, Remediation, SignalLossPolicy,
};
#[cfg(all(feature = "native-media", target_os = "macos"))]
use fm_io_macos::{CameraTelemetry, CameraVideoSource, MacosCameraAdapter};
#[cfg(feature = "native-media")]
use fm_io_network::{
    AbrLadder, AbrVariant, AudioRendition, ColorDescription, ConnectionObservation,
    DestinationConfig, DestinationEnqueue, DestinationId, DestinationRenditions, DestinationState,
    Endpoint, EnqueueStatus, FrameRate, HlsLadder, HlsLadderRung, HlsPlaylistMetadata,
    NetworkTelemetry, OmtTransportSink, OutputPacket, OutputProtocol, OutputSet, PollEvent,
    QueueCapacity, ReconnectPolicy, RenditionId, RenditionPlan, RenditionPlanner, RenditionProfile,
    SinkError, SinkWrite, SrtOptions, TimingProfile, TransportSink, VideoRendition,
    parse_master_m3u8,
    rtmp::{FfmpegRtmpSink, RtmpSinkConfig, StreamKey, raw_pair_packet},
};
#[cfg(feature = "native-media")]
use fm_model::{InputKind, SimulatedAudio, SimulatedVideo, StartupPolicy};
#[cfg(feature = "native-media")]
use fm_observability::{AlertState, Metric, MetricStore};
#[cfg(feature = "native-media")]
use fm_scheduler::{FrameNumber, FramePacer};
#[cfg(feature = "native-media")]
use fm_sim::{Rgba8, SimulatedVideoSource, SourcePattern};
#[cfg(feature = "native-media")]
use freemixd::native_media::{
    NativeAudioLimits, NativeAudioMeters, NativeMasterError, NativeMasterRuntime,
    NativeMediaRuntime, NativeOutputFrame, NativeProgramReadback, NativeProjectLimits,
    NativeProjectPlan, NativeResolvedSource, NativeSourceLimits, NativeSourceRenderError,
};
#[cfg(feature = "native-media")]
use stinger_mutation::{NativeStingerMutation, NativeStingerRetirements};

/// The fake capture inventory of `--diagnostic-capture-inventory` (IN-001),
/// set once before the native runtime starts.
#[cfg(feature = "native-media")]
static DIAGNOSTIC_CAPTURE_INVENTORY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The capture backend native capture inputs use: the diagnostic fake
/// inventory when given, else this platform's (V4L2 on Linux). `None` on
/// platforms whose capture inputs are realized otherwise (macOS cameras) or
/// not at all.
#[cfg(feature = "native-media")]
fn native_capture_backend() -> Option<native_captures::CaptureBackend> {
    DIAGNOSTIC_CAPTURE_INVENTORY
        .get()
        .map(|path| native_captures::CaptureBackend::Fake(path.clone()))
        .or_else(native_captures::CaptureBackend::platform)
}
const PROTOCOL_VERSION: ProtocolVersion = CURRENT_PROTOCOL_VERSION;
const CAPABILITIES_DIGEST: &str = "phase1-simulated";
const NATIVE_MEDIA_CAPABILITIES_DIGEST: &str =
    "native-media-bounded-video-audio-master-meters-camera-telemetry-v6";
const FULLSCREEN_PROGRAM_CAPABILITIES_DIGEST: &str =
    "native-media-bounded-video-audio-master-meters-camera-fullscreen-sdr-telemetry-v4";
const PROGRAM_RECORDER_CAPABILITIES_DIGEST: &str =
    "native-media-bounded-video-audio-master-meters-camera-record-program-telemetry-v4";
const FULLSCREEN_PROGRAM_RECORDER_CAPABILITIES_DIGEST: &str = "native-media-bounded-video-audio-master-meters-camera-fullscreen-sdr-record-program-telemetry-v4";

type AppResult<T> = Result<T, Box<dyn Error>>;
type SharedControl = Rc<RefCell<ControlService<Policy>>>;

const NATIVE_IO_POLL_INTERVAL: Duration = Duration::from_millis(5);
/// How often a due frame that still waits for a source checks again.
#[cfg(feature = "native-media")]
const NATIVE_SOURCE_WAIT_POLL: Duration = Duration::from_millis(1);
const CLIENT_READ_POLL_INTERVAL: Duration = NATIVE_IO_POLL_INTERVAL;
#[cfg(all(feature = "native-media", target_os = "macos"))]
const CAMERA_INITIAL_FRAME_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(all(feature = "native-media", target_os = "macos"))]
const CAMERA_STARTUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(all(feature = "native-media", target_os = "macos"))]
const CAMERA_RECOVERY_POLICY: CameraRecoveryPolicy = CameraRecoveryPolicy {
    max_attempts: 3,
    initial_backoff: Duration::from_millis(100),
    max_backoff: Duration::from_millis(400),
    rearm_backoff: Duration::from_secs(10),
    shutdown_timeout: Duration::from_secs(15),
};
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(feature = "native-media")]
const PROGRAM_RECORDER_STOP_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(feature = "native-media")]
const PROGRAM_RECORDER_KILL_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(feature = "native-media")]
const PROGRAM_STREAM_STOP_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(feature = "native-media")]
const PROGRAM_STREAM_KILL_TIMEOUT: Duration = Duration::from_secs(2);
/// Reconnect backoff growth for every stream destination in the shared
/// [`fm_io_network::OutputSet`]: each retry waits twice the previous delay,
/// within the target's authored initial and maximum delays and attempt
/// budget (OR-006).
#[cfg(feature = "native-media")]
const PROGRAM_STREAM_RECONNECT_MULTIPLIER: u32 = 2;
/// The authored default reconnect policy, for unit-test destinations.
#[cfg(all(test, feature = "native-media"))]
const PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS: u64 = 250;
#[cfg(all(test, feature = "native-media"))]
const PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS: u64 = 2_000;
#[cfg(all(test, feature = "native-media"))]
const PROGRAM_STREAM_RECONNECT_ATTEMPTS: u32 = 40;
/// Every destination's feed queue depth in frame pairs. Four pairs ride out a
/// short reconnect without unbounding the render loop's enqueue cost.
#[cfg(feature = "native-media")]
const PROGRAM_STREAM_QUEUE_CAPACITY: usize = 4;
/// Free-space sampling cadence for the configured program outputs: the
/// recording output's parent and every local HLS output directory. The render
/// loop calls the sampler every frame, so the gate compares one `Instant` and
/// samples at most this often; five seconds bounds how long a filesystem
/// crossing the low watermark can go unreported without adding per-frame I/O.
#[cfg(feature = "native-media")]
const DISK_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
/// Bound and cadence of the operator-state text the control loop publishes to
/// the status listener. The listener applies its own 4 KiB cap as a backstop;
/// rendering here first keeps the published section deterministic and marked.
const OPERATOR_STATE_MAX_BYTES: usize = 4 * 1024;
const OPERATOR_STATE_TRUNCATION_MARKER: &str = "\n[TRUNCATED]\n";
/// The control loop polls every few milliseconds; one render per second is
/// more than enough for a support surface and keeps the pass cheap.
const OPERATOR_STATE_PUBLISH_INTERVAL: Duration = Duration::from_secs(1);
/// Newest idempotency receipts kept in `project.json`. The engine restores a
/// bounded suffix, so a long show neither grows the project file without
/// bound nor rewrites an ever-larger receipt list on every command.
const MAX_PERSISTED_RECEIPTS: usize = 1_024;
#[cfg(feature = "display-surface")]
const PROGRAM_CHECKPOINT_MARGIN: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct ProcessShutdown {
    requested: Arc<AtomicBool>,
    diagnostic_deadline: Option<Instant>,
    /// Snapshot published to the status listener; never read by the daemon.
    status: Arc<DaemonStatus>,
    /// The authoritative `fm-server` status the control loop republishes on
    /// every pass. The session loop only ever holds `&Server`, so the server
    /// cannot change its own status while the loop runs; this is the value the
    /// daemon knows, recorded by [`Self::observe_service`] at every point that
    /// can move it.
    service: ServiceStatus,
    /// Control handle for the bounded operator-state render, attached once the
    /// control service exists and a status listener is configured. The render
    /// never re-enters a borrow a caller already holds: `try_borrow` skips
    /// that pass instead of panicking.
    control: Option<SharedControl>,
    /// When the operator state was last published; gates rendering to one
    /// [`String`] per [`OPERATOR_STATE_PUBLISH_INTERVAL`].
    operator_published_at: Cell<Option<Instant>>,
}

impl ProcessShutdown {
    /// Records a new authoritative service status and publishes it at once.
    fn observe_service(&mut self, service: ServiceStatus) {
        self.service = service;
        self.status.publish(service);
    }

    /// Attaches the control handle the operator-state render reads.
    fn attach_control(&mut self, control: &SharedControl) {
        self.control = Some(Rc::clone(control));
    }

    /// Publishes one bounded, sanitized operator-state snapshot.
    ///
    /// Called from the control loop on every pass and self-gated to one render
    /// per [`OPERATOR_STATE_PUBLISH_INTERVAL`], so a pass that already holds a
    /// mutable control borrow is simply skipped: the status listener is a
    /// diagnostic surface and must never change how the control plane runs.
    /// The render itself reads the engine's protocol snapshot and the native
    /// daemon's realized state, then copies one bounded string into the
    /// listener cell.
    fn publish_operator_state(&self, native: Option<&NativeDaemon>) {
        let Some(control) = self.control.as_ref() else {
            return;
        };
        let now = Instant::now();
        if self.operator_published_at.get().is_some_and(|last| {
            now.saturating_duration_since(last) < OPERATOR_STATE_PUBLISH_INTERVAL
        }) {
            return;
        }
        let Ok(control) = control.try_borrow() else {
            return;
        };
        self.operator_published_at.set(Some(now));
        let text = operator_state_text(&control, native);
        self.status.publish_operator_state(&text);
    }

    /// Publishes the daemon's statistics text (RC-015) for the support
    /// bundle; rendered only while a status listener consumes it.
    fn publish_statistics(&self, render: impl FnOnce() -> String) {
        if self.control.is_some() {
            self.status.publish_statistics(&render());
        }
    }

    fn requested(&self) -> bool {
        self.requested.load(AtomicOrdering::Acquire)
            || self
                .diagnostic_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn set_diagnostic_deadline(&mut self, duration: Duration) -> AppResult<()> {
        self.diagnostic_deadline = Some(
            Instant::now()
                .checked_add(duration)
                .ok_or_else(|| AppFailure("diagnostic shutdown deadline overflow".into()))?,
        );
        Ok(())
    }
}

#[cfg(unix)]
fn register_process_shutdown() -> AppResult<ProcessShutdown> {
    use signal_hook::consts::signal::{SIGINT, SIGTERM};

    let requested = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&requested))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&requested))?;
    Ok(ProcessShutdown {
        requested,
        diagnostic_deadline: None,
        status: DaemonStatus::new(),
        service: ServiceStatus::new(),
        control: None,
        operator_published_at: Cell::new(None),
    })
}

#[cfg(windows)]
fn register_process_shutdown() -> AppResult<ProcessShutdown> {
    let requested = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&requested);
    ctrlc::set_handler(move || handler_flag.store(true, AtomicOrdering::Release))?;
    Ok(ProcessShutdown {
        requested,
        diagnostic_deadline: None,
        status: DaemonStatus::new(),
        service: ServiceStatus::new(),
        control: None,
        operator_published_at: Cell::new(None),
    })
}

#[cfg(not(any(unix, windows)))]
fn register_process_shutdown() -> AppResult<ProcessShutdown> {
    Ok(ProcessShutdown {
        requested: Arc::new(AtomicBool::new(false)),
        diagnostic_deadline: None,
        status: DaemonStatus::new(),
        service: ServiceStatus::new(),
        control: None,
        operator_published_at: Cell::new(None),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonShutdownReason {
    Once,
    ProcessSignal,
    ProgramSurface,
}

#[cfg(test)]
#[derive(Eq, PartialEq)]
enum OnceClientOutcome {
    Unserved,
    HandshakeResponseWritten,
}

/// Polled by every daemon loop, which makes it the control plane's liveness
/// beat as well as its cooperative stop check.
fn requested_daemon_shutdown(
    native: Option<&NativeDaemon>,
    process: Option<&ProcessShutdown>,
) -> Option<DaemonShutdownReason> {
    if let Some(process) = process {
        // Two atomic stores next to the beat. Republishing keeps the health
        // surface live: a degraded daemon shows up on `/healthz` within one
        // poll interval instead of staying frozen at whatever startup wrote.
        process.status.beat();
        process.status.publish(process.service);
        // One bounded render per second keeps the support bundle current
        // without making a poll pass cost a formatting pass every 5 ms.
        process.publish_operator_state(native);
    }
    let reason = if process.is_some_and(ProcessShutdown::requested) {
        Some(DaemonShutdownReason::ProcessSignal)
    } else if native.is_some_and(NativeDaemon::shutdown_requested) {
        Some(DaemonShutdownReason::ProgramSurface)
    } else {
        None
    };
    if reason.is_some()
        && let Some(process) = process
    {
        process.status.begin_draining();
    }
    reason
}

/// Renders the bounded operator-state section published to the status listener.
///
/// The section is line-oriented and fixed-order: the configured stream roster
/// with desired state, both recorder slots' desired flags, and — when a native
/// daemon runs — realized stream state, recorder configuration and activity,
/// and the disk alert. Only target ids, operator-authored names, booleans,
/// counts, and bounded labels are ever read, so URLs, stream keys, and
/// filesystem paths cannot leak. An oversized render is truncated at a UTF-8
/// boundary with an explicit marker.
fn operator_state_text(control: &ControlService<Policy>, native: Option<&NativeDaemon>) -> String {
    let snapshot = &control.snapshot().snapshot;
    render_operator_state(&snapshot.streams, &snapshot.recordings, native)
}

/// Renders the operator-state lines from the engine's protocol snapshot and
/// the native daemon's realized state, then applies the byte cap.
fn render_operator_state(
    streams: &[StreamStatus],
    recordings: &[RecordingStatus],
    native: Option<&NativeDaemon>,
) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "streams={}", streams.len());
    for stream in streams {
        let _ = write!(
            output,
            "stream target={} name={} desired={}",
            stream.target,
            operator_state_name(&stream.name),
            stream.desired_running
        );
        if let Some(realized) = native_stream_realized(native, stream.target) {
            let _ = write!(output, " realized={realized}");
        }
        output.push('\n');
    }
    let desired = |slot: usize| {
        recordings
            .get(slot)
            .is_some_and(|status| status.desired_active)
    };
    let _ = write!(output, "recorders=desired={},{}", desired(0), desired(1));
    #[cfg(feature = "native-media")]
    if let Some(native) = native {
        let _ = write!(
            output,
            " configured={},{} running={},{}",
            native.record_program.is_some(),
            native.record_program_2.is_some(),
            native.recorders[0].is_running(),
            native.recorders[1].is_running(),
        );
    }
    #[cfg(not(feature = "native-media"))]
    let _ = native;
    output.push('\n');
    #[cfg(feature = "native-media")]
    if let Some(native) = native {
        let _ = writeln!(
            output,
            "disk_alert={} free_bytes={}",
            disk_alert_label(native.disk_free_bytes.map(|_| native.disk_alert.state())),
            native.disk_free_bytes.unwrap_or(0)
        );
    }
    cap_operator_state(output)
}

/// Replaces control characters in an operator-authored stream name so one name
/// can never forge an extra line in the support bundle. The name itself is
/// safe by construction: the engine inventory carries only the authored label,
/// never a destination URL or key.
fn operator_state_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_control() {
            sanitized.push(' ');
        } else {
            sanitized.push(character);
        }
    }
    sanitized
}

/// The realized state label of one stream target as the native daemon sees it,
/// or `None` when no native daemon runs or the target has no sink.
#[cfg(feature = "native-media")]
fn native_stream_realized(
    native: Option<&NativeDaemon>,
    target: WireStreamTargetId,
) -> Option<&'static str> {
    let native = native?;
    let target = SwitcherStreamTargetId::from_non_zero(target.get());
    let stream = native
        .streams
        .iter()
        .find(|stream| stream.target == target)?;
    if stream.first_failure.is_some() {
        return Some("failed");
    }
    Some(match native.output_set.state(stream.destination_id) {
        None | Some(DestinationState::Stopped) => "stopped",
        Some(DestinationState::Connecting | DestinationState::AwaitingRandomAccess) => "starting",
        Some(DestinationState::Live) => "live",
        Some(DestinationState::WaitingToReconnect { .. }) => "waiting_to_reconnect",
        Some(DestinationState::Congested) => "congested",
        Some(DestinationState::Failed) => "failed",
    })
}

#[cfg(not(feature = "native-media"))]
const fn native_stream_realized(
    _native: Option<&NativeDaemon>,
    _target: WireStreamTargetId,
) -> Option<&'static str> {
    None
}

/// Applies the operator-state byte cap, marking an overlong render instead of
/// letting the status listener drop it.
fn cap_operator_state(text: String) -> String {
    if text.len() <= OPERATOR_STATE_MAX_BYTES {
        return text;
    }
    let mut capped = String::with_capacity(OPERATOR_STATE_MAX_BYTES);
    let prefix_bytes =
        OPERATOR_STATE_MAX_BYTES.saturating_sub(OPERATOR_STATE_TRUNCATION_MARKER.len());
    capped.push_str(utf8_prefix(&text, prefix_bytes));
    capped.push_str(OPERATOR_STATE_TRUNCATION_MARKER);
    capped
}

/// The longest UTF-8 prefix of `value` no longer than `maximum_bytes`.
fn utf8_prefix(value: &str, maximum_bytes: usize) -> &str {
    let mut end = maximum_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
#[cfg(feature = "native-media")]
struct NativeSourceResolution {
    sources: Vec<NativeResolvedSource>,
    #[cfg(target_os = "macos")]
    cameras: NativeCameraInputs,
}

/// A frame begun but not finished: both realizers have its jobs and the
/// control loop will collect them after servicing peers (structural item 1).
#[cfg(feature = "native-media")]
struct PendingFrame {
    deadline: Instant,
    started: Instant,
    command: bool,
    monitor_frame: Option<(u64, InputId)>,
    preview_source: Option<FrameResult>,
    shows_preview: bool,
    fade_to_black_excluded: Vec<fm_types::OutputId>,
    trace_before: Option<pacing_trace::CpuCounters>,
    events: Vec<RuntimeEventMessage>,
}

#[cfg(feature = "native-media")]
#[derive(Debug)]
enum NativeRealizationError {
    Video(NativeSourceRenderError),
    Audio(NativeMasterError),
    /// A realizer thread (mixer or compose) stopped or refused its command
    /// (structural item 1).
    RealizerThread(String),
}

#[cfg(feature = "native-media")]
impl fmt::Display for NativeRealizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Video(error) => error.fmt(formatter),
            Self::Audio(error) => error.fmt(formatter),
            Self::RealizerThread(error) => formatter.write_str(error),
        }
    }
}

#[cfg(feature = "native-media")]
impl Error for NativeRealizationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Video(error) => Some(error),
            Self::Audio(error) => Some(error),
            Self::RealizerThread(_) => None,
        }
    }
}

/// Time a command needs before its frame's deadline besides its journal
/// append: preparing, staging, and committing it.
#[cfg(feature = "native-media")]
const COMMAND_STAGING_MARGIN: Duration = Duration::from_millis(2);

/// The decision of [`NativeDaemon::command_window`]: open with `left` of a
/// frame `interval` before the next frame (`next_frame`) is due and recent
/// journal appends taking up to `append_peak`, given the frame a command
/// was first held back for (`deferred`, updated).
#[cfg(feature = "native-media")]
fn command_window_open(
    left: Duration,
    interval: Duration,
    append_peak: Option<Duration>,
    next_frame: FrameNumber,
    deferred: &mut Option<FrameNumber>,
) -> bool {
    let needed = append_peak.map_or(interval / 2, |peak| {
        peak.saturating_add(COMMAND_STAGING_MARGIN).min(interval)
    });
    if left >= needed && left > Duration::ZERO {
        *deferred = None;
        return true;
    }
    match *deferred {
        Some(held) if next_frame > held => {
            *deferred = None;
            true
        }
        Some(_) => false,
        None => {
            *deferred = Some(next_frame);
            false
        }
    }
}

#[cfg(feature = "native-media")]
fn validate_native_stinger_sources(
    stored: &StoredProject,
    sources: &[NativeResolvedSource],
) -> AppResult<()> {
    for config in stored
        .project()
        .stingers()
        .iter()
        .filter(|config| config.preload)
    {
        let source = sources
            .iter()
            .find(|source| source.input() == config.media_input)
            .ok_or_else(|| {
                AppFailure(format!(
                    "native Stinger slot {} media input {} is unavailable",
                    config.slot.number(),
                    config.media_input
                ))
            })?;
        match source {
            NativeResolvedSource::RetainedFrame { .. }
            | NativeResolvedSource::LocalVideo { .. } => {}
            NativeResolvedSource::LiveFrame { .. } => {
                return Err(AppFailure(format!(
                    "native Stinger slot {} media input {} is live and cannot be preloaded",
                    config.slot.number(),
                    config.media_input
                ))
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(feature = "native-media")]
fn partition_native_source_limits(
    width: u32,
    height: u32,
    stinger_sources: usize,
) -> AppResult<(NativeSourceLimits, NativeSourceLimits)> {
    let defaults = NativeSourceLimits::default();
    let stinger_ring_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(8))
        .and_then(|bytes| bytes.checked_mul(u64::from(defaults.max_video_frames_per_source.get())))
        .and_then(|bytes| bytes.checked_mul(u64::try_from(stinger_sources).unwrap_or(u64::MAX)))
        .ok_or_else(|| AppFailure("native Stinger ring byte partition overflow".into()))?;
    if stinger_ring_bytes > defaults.max_retained_rgba16f_bytes {
        return Err(AppFailure(format!(
            "native Stinger rings require {stinger_ring_bytes} retained bytes, exceeding aggregate limit {}",
            defaults.max_retained_rgba16f_bytes
        ))
        .into());
    }
    Ok((
        NativeSourceLimits {
            max_retained_rgba16f_bytes: defaults
                .max_retained_rgba16f_bytes
                .saturating_sub(stinger_ring_bytes),
            ..defaults
        },
        NativeSourceLimits {
            max_retained_rgba16f_bytes: stinger_ring_bytes,
            ..defaults
        },
    ))
}

#[cfg(feature = "native-media")]
fn preflight_native_video(
    runtime: &NativeMediaRuntime,
    adapter: Option<&Adapter>,
    sources: Vec<NativeResolvedSource>,
    stinger_sources: &[NativeResolvedSource],
    stored: &StoredProject,
) -> AppResult<(
    freemixd::native_media::NativeSourcePlayback,
    freemixd::native_media::NativeSourcePlayback,
)> {
    let (stingers, playback_limit) =
        preflight_native_stinger_video(runtime, adapter, stinger_sources, stored)?;
    let playback = runtime.preflight_resolved_source_playback_mixed_blocking(
        adapter,
        sources,
        native_clock_domain(),
        StreamSelector::Best,
        NativeSourceLimits {
            max_retained_rgba16f_bytes: playback_limit,
            ..NativeSourceLimits::default()
        },
    )?;
    let expected = stored.project().settings().video.dimensions;
    validate_native_video_dimensions("media source", playback.registry(), expected)?;
    Ok((playback, stingers))
}

/// Starts the capture backend and a supervisor for every capture input of
/// `stored` (IN-001), or `None` without a capture backend. A device that is
/// absent or fails is reported through its input's status; none stops the
/// daemon.
#[cfg(feature = "native-media")]
fn start_native_captures(
    stored: &StoredProject,
) -> AppResult<Option<native_captures::NativeCaptures>> {
    let Some(backend) = native_capture_backend() else {
        return Ok(None);
    };
    let project = stored.project();
    let settings = project.settings();
    let inputs = project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::Device {
                stable_key,
                capture,
            } => Some(native_captures::CaptureInput {
                input: input.id,
                stable_key: stable_key.clone(),
                settings: capture.clone(),
                closed: project.input_lifecycle(input.id).closed,
            }),
            _ => None,
        })
        .collect();
    let dimensions = settings.video.dimensions;
    native_captures::NativeCaptures::start(
        &backend,
        inputs,
        settings.frame_rate,
        native_clock_domain(),
        (dimensions.width(), dimensions.height()),
        settings.audio.sample_rate,
    )
    .map(Some)
    .map_err(|error| AppFailure(error).into())
}

/// Starts a receiver for every open network stream input of `stored`
/// (IN-015). A receiver that cannot reach its stream keeps reconnecting;
/// none stops the daemon.
#[cfg(feature = "native-media")]
fn start_native_network_streams(stored: &StoredProject) -> native_streams::NativeNetworkStreams {
    let project = stored.project();
    let settings = project.settings();
    let inputs = project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::NetworkStream(config) => Some(native_streams::NetworkStreamInput {
                input: input.id,
                config: config.clone(),
                closed: project.input_lifecycle(input.id).closed,
            }),
            _ => None,
        })
        .collect();
    let dimensions = settings.video.dimensions;
    native_streams::NativeNetworkStreams::start(
        inputs,
        settings.frame_rate,
        native_clock_domain(),
        (dimensions.width(), dimensions.height()),
        settings.audio.sample_rate,
    )
}

/// Starts a receiver for every open system audio input of `stored`
/// (AU-011). A device that does not open keeps reopening; none stops the
/// daemon.
#[cfg(feature = "native-media")]
fn start_native_system_audio(stored: &StoredProject) -> native_system_audio::NativeSystemAudio {
    let project = stored.project();
    let settings = project.settings();
    let inputs = project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::SystemAudio(config) => Some(native_system_audio::SystemAudioInput {
                input: input.id,
                config: config.clone(),
                closed: project.input_lifecycle(input.id).closed,
            }),
            _ => None,
        })
        .collect();
    let dimensions = settings.video.dimensions;
    native_system_audio::NativeSystemAudio::start(
        inputs,
        settings.frame_rate,
        native_clock_domain(),
        (dimensions.width(), dimensions.height()),
        settings.audio.sample_rate,
    )
}

/// Starts watching the desktop for every screen capture input of `stored`
/// (IN-011). A target that is not on the desktop waits for it; none stops
/// the daemon.
#[cfg(feature = "native-media")]
fn start_native_screen_capture(
    stored: &StoredProject,
) -> native_screen_capture::NativeScreenCapture {
    let project = stored.project();
    let settings = project.settings();
    let inputs = project
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::ScreenCapture(config) => Some(native_screen_capture::ScreenCaptureInput {
                input: input.id,
                config: config.clone(),
                closed: project.input_lifecycle(input.id).closed,
            }),
            _ => None,
        })
        .collect();
    let dimensions = settings.video.dimensions;
    let mut capture = native_screen_capture::NativeScreenCapture::start(
        desktop_capture::DesktopBackend::platform(),
        inputs,
        settings.frame_rate,
        native_clock_domain(),
        (dimensions.width(), dimensions.height()),
        settings.audio.sample_rate,
    );
    // A Wayland desktop shares the targets it granted before again.
    capture.set_restore_tokens(project.screen_capture_restore_tokens().clone());
    capture
}

/// Starts the media-file inputs of `stored` that play through their engine
/// transport (every local-video media input no Stinger names) and swaps each
/// one's resolved source for a live source seeded with its startup picture.
/// Returns the inputs, whose Master audio becomes pushed.
#[cfg(feature = "native-media")]
fn resolve_native_file_sources(
    adapter: Option<&Adapter>,
    stored: &StoredProject,
    sources: &mut [NativeResolvedSource],
) -> AppResult<(native_files::NativeMediaFiles, Vec<InputId>)> {
    let project = stored.project();
    let stinger_media = project
        .stingers()
        .iter()
        .map(|config| config.media_input)
        .collect::<std::collections::BTreeSet<_>>();
    let inputs = sources
        .iter()
        .filter_map(|source| match source {
            NativeResolvedSource::LocalVideo { input, path } if !stinger_media.contains(input) => {
                let settings = *project.media_playback(*input)?;
                Some(native_files::MediaFileInput {
                    input: *input,
                    path: path.clone(),
                    settings,
                    closed: project.input_lifecycle(*input).closed,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let ids = inputs.iter().map(|input| input.input).collect::<Vec<_>>();
    let dimensions = project.settings().video.dimensions;
    let (files, seeds) = match adapter {
        Some(adapter) => native_files::NativeMediaFiles::start(
            adapter,
            inputs,
            project.settings().frame_rate,
            native_clock_domain(),
            (dimensions.width(), dimensions.height()),
        )
        .map_err(AppFailure)?,
        None if inputs.is_empty() => native_files::NativeMediaFiles::start_empty(
            project.settings().frame_rate,
            native_clock_domain(),
            (dimensions.width(), dimensions.height()),
        ),
        None => {
            return Err(AppFailure("native media files require ffmpeg".into()).into());
        }
    };
    for (input, frame) in seeds {
        if let Some(source) = sources.iter_mut().find(|source| source.input() == input) {
            *source = NativeResolvedSource::LiveFrame { input, frame };
        }
    }
    Ok((files, ids))
}

#[cfg(feature = "native-media")]
fn preflight_native_stinger_video(
    runtime: &NativeMediaRuntime,
    adapter: Option<&Adapter>,
    sources: &[NativeResolvedSource],
    stored: &StoredProject,
) -> AppResult<(freemixd::native_media::NativeSourcePlayback, u64)> {
    let stinger_inputs = stored
        .project()
        .stingers()
        .iter()
        .filter(|config| config.preload)
        .map(|config| config.media_input)
        .fold(Vec::new(), |mut inputs, input| {
            if !inputs.contains(&input) {
                inputs.push(input);
            }
            inputs
        });
    let stinger_sources = sources
        .iter()
        .filter(|source| stinger_inputs.contains(&source.input()))
        .cloned()
        .collect::<Vec<_>>();
    let expected = stored.project().settings().video.dimensions;
    let (playback_limits, stinger_limits) =
        partition_native_source_limits(expected.width(), expected.height(), stinger_sources.len())?;
    let mut stingers = runtime.preflight_resolved_source_playback_mixed_blocking(
        adapter,
        stinger_sources,
        native_clock_domain(),
        StreamSelector::Best,
        stinger_limits,
    )?;
    for input in stinger_inputs {
        stingers.enable_stinger_source(input)?;
    }
    validate_native_video_dimensions("Stinger", stingers.registry(), expected)?;
    Ok((stingers, playback_limits.max_retained_rgba16f_bytes))
}

#[cfg(feature = "native-media")]
fn validate_native_video_dimensions(
    label: &str,
    registry: &freemixd::native_media::NativeSourceRegistry,
    expected: fm_frame::VideoDimensions,
) -> AppResult<()> {
    if registry
        .dimensions()
        .is_some_and(|dimensions| dimensions != (expected.width(), expected.height()))
    {
        return Err(AppFailure(format!(
            "native {label} dimensions must match project output {}x{}",
            expected.width(),
            expected.height()
        ))
        .into());
    }
    Ok(())
}

#[cfg(feature = "native-media")]
fn validate_native_audio_modes(stored: &StoredProject) -> AppResult<()> {
    if let Some(input) = stored.project().inputs().iter().find(|input| {
        matches!(
            &input.kind,
            InputKind::Simulated(fm_model::SimulatedInput {
                audio: SimulatedAudio::Sine { .. },
                ..
            })
        )
    }) {
        return Err(AppFailure(format!(
            "native generator input {} requires unsupported simulated sine audio",
            input.id
        ))
        .into());
    }
    Ok(())
}

/// Every live source's drift loop, unordered (Protocol 2.76).
#[cfg(feature = "native-media")]
fn live_sync_samples(
    network: &native_streams::NativeNetworkStreams,
    system_audio: &native_system_audio::NativeSystemAudio,
    captures: Option<&native_captures::NativeCaptures>,
    screen_capture: &native_screen_capture::NativeScreenCapture,
) -> Vec<fm_protocol::SourceSyncSample> {
    let mut samples = network.sync_samples();
    samples.extend(system_audio.sync_samples());
    samples.extend(screen_capture.sync_samples());
    if let Some(captures) = captures {
        samples.extend(captures.sync_samples());
    }
    samples
}

/// The one picture generator input `input` shows: bars or a solid colour
/// at the project's size, dated sequence zero.
#[cfg(feature = "native-media")]
fn native_generator_frame(
    input: InputId,
    simulated: &fm_model::SimulatedInput,
    settings: &fm_model::ProjectSettings,
) -> AppResult<fm_frame::CpuVideoFrame> {
    let pattern = match simulated.video {
        SimulatedVideo::Bars => SourcePattern::Bars,
        SimulatedVideo::Solid(color) => {
            SourcePattern::Solid(Rgba8::new(color.red, color.green, color.blue, color.alpha))
        }
    };
    let dimensions = settings.video.dimensions;
    let mut source = SimulatedVideoSource::new(
        dimensions.width(),
        dimensions.height(),
        settings.frame_rate,
        native_clock_domain(),
        pattern,
    )
    .map_err(|error| {
        AppFailure(format!(
            "native generator construction failed for input {input}: {error}"
        ))
    })?;
    source
        .next_frame()
        .map_err(|error| {
            AppFailure(format!(
                "native generator frame failed for input {input}: {error}"
            ))
        })?
        .ok_or_else(|| {
            AppFailure(format!(
                "native generator produced signal loss for input {input}"
            ))
            .into()
        })
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
const fn platform_native_backend() -> NativeBackend {
    NativeBackend::Metal
}

#[cfg(all(feature = "native-media", target_os = "windows"))]
const fn platform_native_backend() -> NativeBackend {
    NativeBackend::Dx12
}

#[cfg(all(feature = "native-media", target_os = "linux"))]
const fn platform_native_backend() -> NativeBackend {
    NativeBackend::Vulkan
}

#[cfg(feature = "native-media")]
fn host_deadline_offset(pacer: &FramePacer) -> Result<Duration, fm_scheduler::PacingError> {
    Ok(Duration::from_nanos(pacer.next_deadline()?.at_ns))
}

#[cfg(feature = "native-media")]
fn native_clock_domain() -> MediaClockDomainId {
    MediaClockDomainId::new(NonZeroU128::MIN)
}

/// Validates a segment path and returns its canonical parent directory.
///
/// The segment must name a final `.mp4` file whose parent is an existing
/// directory. The session creates the file itself, exclusively: an existing
/// file, directory, or symlink at the path is never overwritten.
#[cfg(feature = "native-media")]
fn record_output_parent(path: &Path) -> AppResult<PathBuf> {
    path.file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppFailure("recording output must name a final .mp4 file".into()))?;
    if path.extension().and_then(|extension| extension.to_str()) != Some("mp4") {
        return Err(
            AppFailure("recording output must have the final extension `.mp4`".into()).into(),
        );
    }
    let parent = recording_parent(path);
    let parent = fs::canonicalize(parent).map_err(|error| {
        AppFailure(format!(
            "recording output parent is not an existing canonical directory: {:?}",
            error.kind()
        ))
    })?;
    if !parent.is_dir() {
        return Err(AppFailure(
            "recording output parent is not an existing canonical directory".into(),
        )
        .into());
    }
    Ok(parent)
}

#[cfg(feature = "native-media")]
fn diagnostic_field(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\t' | '\r' | '\n' => ' ',
            character if character.is_control() => '?',
            character => character,
        })
        .collect()
}

/// One sanitized `FREEMIXD_RECORDER` record for a finished segment.
///
/// Every existing field keeps its name and order; the 1-based `slot` names
/// which configured recorder wrote the segment and follows `v=1`
/// immediately. `video_bitrate_kbps` and `audio_bitrate_kbps` render the
/// slot's configured encoder bitrate, or `unset` when it keeps the encoder
/// default.
#[cfg(feature = "native-media")]
#[allow(clippy::too_many_arguments)]
fn recorder_report(
    slot: usize,
    report: &StopReport,
    duration_ms: u64,
    free_bytes: u64,
    backlog: RecorderBacklog,
    app_capture_failure: bool,
    failure: &str,
    video_bitrate_kbps: Option<u32>,
    audio_bitrate_kbps: Option<u32>,
) -> String {
    format!(
        "FREEMIXD_RECORDER\tv=1\tslot={slot}\tstate={:?}\toutcome={:?}\taccepted_pairs={}\tcompleted_pairs={}\toutput_bytes={}\twav_bytes={}\tvideo_bitrate_kbps={}\taudio_bitrate_kbps={}\tduration_ms={duration_ms}\tfree_bytes={free_bytes}\toutput_finalization={:?}\tcleanup={:?}\tbacklogged_pairs={}\tbacklog_peak_bytes={}\tapp_capture_failure={app_capture_failure}\tfailure={}",
        report.telemetry.state,
        report.outcome,
        report.telemetry.accepted_pairs,
        report.telemetry.completed_pairs,
        report.telemetry.output_bytes,
        report.telemetry.wav_bytes,
        record_bitrate_field(video_bitrate_kbps),
        record_bitrate_field(audio_bitrate_kbps),
        report.output,
        report.cleanup,
        backlog.pairs,
        backlog.peak_bytes,
        diagnostic_field(failure),
    )
}

/// How much a segment leaned on the daemon-side backlog: pairs that waited
/// there because the segment was opening or the recorder fell behind, and
/// the most bytes it held at once. Nonzero values on a running segment are
/// the visible backpressure a stalled `FFmpeg` or disk caused before any loss.
#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RecorderBacklog {
    pairs: u64,
    peak_bytes: usize,
}

/// Renders one configured recorder bitrate for `FREEMIXD_RECORDER`; `None`
/// means the slot keeps the encoder's own default and reports `unset`.
#[cfg(feature = "native-media")]
fn record_bitrate_field(kbps: Option<u32>) -> String {
    kbps.map_or_else(|| "unset".to_owned(), |kbps| kbps.to_string())
}

/// One sanitized, slot-annotated immediate failure notice for a latch that
/// just took the addressed recorder slot out of the capture path.
#[cfg(feature = "native-media")]
fn recorder_failure_notice(slot: usize, failure: &str, app_capture_failure: bool) -> String {
    format!(
        "FREEMIXD_RECORDER_FAILURE\tv=1\tslot={slot}\tapp_capture_failure={app_capture_failure}\tfailure={}",
        diagnostic_field(failure),
    )
}

fn capabilities_digest(native: bool, fullscreen: bool, recorder: bool) -> &'static str {
    match (native, fullscreen, recorder) {
        (true, true, true) => FULLSCREEN_PROGRAM_RECORDER_CAPABILITIES_DIGEST,
        (true, true, false) => FULLSCREEN_PROGRAM_CAPABILITIES_DIGEST,
        (true, false, true) => PROGRAM_RECORDER_CAPABILITIES_DIGEST,
        (true, false, false) => NATIVE_MEDIA_CAPABILITIES_DIGEST,
        (false, _, _) => CAPABILITIES_DIGEST,
    }
}

#[derive(Clone)]
struct ControlHandle(SharedControl);

impl ControlPlane for ControlHandle {
    type Error = Infallible;

    fn initial_sync(&self, cursor: Option<&EventCursor>) -> Result<InitialSync, Self::Error> {
        let control = self.0.borrow();
        let payload = match cursor {
            Some(cursor) => match control.resume(cursor) {
                ResumeDecision::Events(events) => SyncPayload::Resume(events),
                ResumeDecision::Snapshot(record) => {
                    SyncPayload::Snapshot(Box::new(record.snapshot))
                }
            },
            None => SyncPayload::Snapshot(Box::new(control.snapshot().snapshot.clone())),
        };
        let diagnostics = control.diagnostics();
        Ok(InitialSync {
            engine: diagnostics.engine,
            current_revision: diagnostics.current_revision,
            payload,
        })
    }
}

fn main() -> ExitCode {
    let command = match parse_args(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("run `freemixd help` for usage");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = run(command) {
        eprintln!("error: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Installs the diagnostic capture inventory before the runtime starts.
#[cfg_attr(not(feature = "native-media"), allow(clippy::needless_pass_by_value))]
fn install_capture_inventory(path: Option<PathBuf>) {
    #[cfg(feature = "native-media")]
    if let Some(path) = path {
        let _ = DIAGNOSTIC_CAPTURE_INVENTORY.set(path);
    }
    #[cfg(not(feature = "native-media"))]
    let _ = path;
}

#[allow(clippy::too_many_lines)]
fn run(command: Command) -> AppResult<()> {
    match command {
        Command::Serve {
            project,
            listen,
            web_listen,
            web,
            status_listen,
            hls_listen,
            live_lan_listen,
            once,
            native_media,
            fullscreen_program,
            display_outputs,
            display_inventory,
            presenter_fallback,
            camera_helper,
            capture_inventory,
            record_program,
            record_output,
            record_program_2,
            record_output_2,
            record_wav,
            record_segment_seconds,
            record_bitrate_kbps,
            record_bitrate_kbps_2,
            record_audio_bitrate_kbps,
            record_audio_bitrate_kbps_2,
            record_fallback_dir,
            diagnostic_stop_after,
            recover_to_checkpoint,
        } => {
            install_capture_inventory(capture_inventory);
            if !display_outputs.is_empty() {
                DisplayOutputSet::new(display_outputs.clone())
                    .map_err(|error| AppFailure(format!("invalid display outputs: {error}")))?;
            }
            if fullscreen_program {
                let result = run_fullscreen_program(
                    project.clone(),
                    listen,
                    web_listen,
                    web.clone(),
                    once,
                    display_outputs.clone(),
                    camera_helper.clone(),
                    record_program.clone(),
                    record_output,
                    record_program_2.clone(),
                    record_output_2,
                    record_wav,
                    record_segment_seconds,
                    record_bitrate_kbps,
                    record_bitrate_kbps_2,
                    record_audio_bitrate_kbps,
                    record_audio_bitrate_kbps_2,
                );
                match result {
                    Err(error) if presenter_fallback && error.is::<PresenterUnavailable>() => {
                        // Serve headless instead: the slots wait for a
                        // presenter and the next start retries the windows.
                        eprintln!("{}", presenter_unavailable_record(&error.to_string()));
                    }
                    result => return result,
                }
            }
            serve_inner(
                &project,
                ServeListeners {
                    listen,
                    web_listen,
                    web,
                    status_listen,
                    hls_listen,
                    live_lan_listen,
                },
                once,
                if native_media {
                    NativeServeMode::Headless {
                        camera_helper,
                        display_inventory,
                        display_overrides: display_outputs,
                    }
                } else {
                    NativeServeMode::Disabled
                },
                record_program,
                record_output,
                record_program_2,
                record_output_2,
                record_wav,
                record_segment_seconds,
                record_bitrate_kbps,
                record_bitrate_kbps_2,
                record_audio_bitrate_kbps,
                record_audio_bitrate_kbps_2,
                record_fallback_dir,
                diagnostic_stop_after,
                recover_to_checkpoint,
            )
        }
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Version => {
            println!("freemixd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

#[cfg(feature = "display-surface")]
#[allow(clippy::too_many_arguments)]
fn run_fullscreen_program(
    project: PathBuf,
    listen: SocketAddr,
    web_listen: Option<SocketAddr>,
    web: web::WebSettings,
    once: bool,
    display_overrides: Vec<DisplayOutputConfig>,
    camera_helper: Option<PathBuf>,
    record_program: Option<PathBuf>,
    record_output: Option<fm_types::OutputId>,
    record_program_2: Option<PathBuf>,
    record_output_2: Option<fm_types::OutputId>,
    record_wav: bool,
    record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    record_bitrate_kbps: Option<u32>,
    record_bitrate_kbps_2: Option<u32>,
    record_audio_bitrate_kbps: Option<u32>,
    record_audio_bitrate_kbps_2: Option<u32>,
) -> AppResult<()> {
    program_surface::run(program_surface::ProgramWorkerOptions {
        project,
        listen,
        web_listen,
        web,
        once,
        display_overrides,
        camera_helper,
        record_program,
        record_output,
        record_program_2,
        record_output_2,
        record_wav,
        record_segment_seconds,
        record_bitrate_kbps,
        record_bitrate_kbps_2,
        record_audio_bitrate_kbps,
        record_audio_bitrate_kbps_2,
    })
}

#[cfg(not(feature = "display-surface"))]
#[allow(clippy::too_many_arguments)]
fn run_fullscreen_program(
    _project: PathBuf,
    _listen: SocketAddr,
    _web_listen: Option<SocketAddr>,
    _web: web::WebSettings,
    _once: bool,
    _display_overrides: Vec<DisplayOutputConfig>,
    _camera_helper: Option<PathBuf>,
    _record_program: Option<PathBuf>,
    _record_output: Option<fm_types::OutputId>,
    _record_program_2: Option<PathBuf>,
    _record_output_2: Option<fm_types::OutputId>,
    _record_wav: bool,
    _record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    _record_bitrate_kbps: Option<u32>,
    _record_bitrate_kbps_2: Option<u32>,
    _record_audio_bitrate_kbps: Option<u32>,
    _record_audio_bitrate_kbps_2: Option<u32>,
) -> AppResult<()> {
    Err(AppFailure(
        "--fullscreen-program, --display-output, and --display-presenter require a build with \
         feature `display-surface`"
            .into(),
    )
    .into())
}

enum NativeServeMode {
    Disabled,
    Headless {
        camera_helper: Option<PathBuf>,
        /// Presents the display outputs to a diagnostic inventory host.
        display_inventory: Option<PathBuf>,
        /// `--display-output` slots replacing the project's on load.
        display_overrides: Vec<DisplayOutputConfig>,
    },
    #[cfg(feature = "display-surface")]
    Program(Box<ProgramServeSetup>),
}

impl NativeServeMode {
    /// The `--display-output` slots replacing the project's on load.
    fn display_overrides(&self) -> &[DisplayOutputConfig] {
        match self {
            Self::Disabled => &[],
            Self::Headless {
                display_overrides, ..
            } => display_overrides,
            #[cfg(feature = "display-surface")]
            Self::Program(setup) => &setup.display_overrides,
        }
    }
}

#[cfg(feature = "display-surface")]
struct ProgramServeSetup {
    context: NativeContext,
    outputs: Vec<program_surface::DisplayPresentationSetup>,
    windowed: program_surface::WindowedDisplays,
    display_overrides: Vec<DisplayOutputConfig>,
    camera_helper: Option<PathBuf>,
}

/// The daemon's bound sockets: control, optional web gateway, optional status,
/// and optional HLS.
#[derive(Clone)]
struct ServeListeners {
    listen: SocketAddr,
    web_listen: Option<SocketAddr>,
    /// The gateway's TLS, state directory, and host names.
    web: web::WebSettings,
    status_listen: Option<SocketAddr>,
    hls_listen: Option<SocketAddr>,
    live_lan_listen: Option<SocketAddr>,
}

#[cfg(feature = "display-surface")]
#[allow(clippy::too_many_arguments)]
fn serve_program_worker(
    path: &Path,
    listen: SocketAddr,
    web_listen: Option<SocketAddr>,
    web: web::WebSettings,
    once: bool,
    context: NativeContext,
    outputs: Vec<program_surface::DisplayPresentationSetup>,
    windowed: program_surface::WindowedDisplays,
    display_overrides: Vec<DisplayOutputConfig>,
    camera_helper: Option<PathBuf>,
    record_program: Option<PathBuf>,
    record_output: Option<fm_types::OutputId>,
    record_program_2: Option<PathBuf>,
    record_output_2: Option<fm_types::OutputId>,
    record_wav: bool,
    record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    record_bitrate_kbps: Option<u32>,
    record_bitrate_kbps_2: Option<u32>,
    record_audio_bitrate_kbps: Option<u32>,
    record_audio_bitrate_kbps_2: Option<u32>,
) -> AppResult<()> {
    serve_inner(
        path,
        ServeListeners {
            listen,
            web_listen,
            web,
            status_listen: None,
            hls_listen: None,
            live_lan_listen: None,
        },
        once,
        NativeServeMode::Program(Box::new(ProgramServeSetup {
            context,
            outputs,
            windowed,
            display_overrides,
            camera_helper,
        })),
        record_program,
        record_output,
        record_program_2,
        record_output_2,
        record_wav,
        record_segment_seconds,
        record_bitrate_kbps,
        record_bitrate_kbps_2,
        record_audio_bitrate_kbps,
        record_audio_bitrate_kbps_2,
        // `--record-fallback-dir` is refused with `--fullscreen-program`.
        None,
        None,
        // `--recover-to-checkpoint` is refused with `--fullscreen-program`.
        false,
    )
}

/// Replaces the project's display-output slots with the `--display-output`
/// overrides, slot 1 first, when any are given (OR-011). The engine then
/// starts with them; [`persist_display_output_overrides`] saves them.
///
/// # Errors
///
/// Returns an [`AppFailure`] when an override shows an output the project
/// does not declare or shares a display with another.
fn apply_display_output_overrides(
    stored: StoredProject,
    overrides: &[DisplayOutputConfig],
) -> AppResult<StoredProject> {
    if overrides.is_empty() {
        return Ok(stored);
    }
    let mut project = stored.project().clone();
    for slot in DisplayOutputSlot::ALL {
        project.clear_display_output(slot);
    }
    for (slot, config) in DisplayOutputSlot::ALL.into_iter().zip(overrides) {
        project
            .set_display_output(slot, config.clone())
            .map_err(|error| AppFailure(format!("--display-output {slot}: {error}")))?;
    }
    Ok(stored
        .with_project(project)
        .map_err(|error| AppFailure(format!("--display-output: {error}")))?)
}

/// Applies the `--display-output` overrides (see
/// [`apply_display_output_overrides`]) and checkpoints the manifest when
/// they changed it, before any command is journaled: commands accepted
/// against the overridden slots must replay against them after a crash.
///
/// # Errors
///
/// Returns the override refusal or a checkpoint failure.
fn persist_display_output_overrides(
    store: &dyn DurableStore,
    stored: &StoredProject,
    overrides: &[DisplayOutputConfig],
) -> AppResult<StoredProject> {
    let overridden = apply_display_output_overrides(stored.clone(), overrides)?;
    if overridden.project().display_outputs() != stored.project().display_outputs() {
        store.checkpoint(&overridden)?;
    }
    Ok(overridden)
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]
fn serve_inner(
    path: &Path,
    listeners: ServeListeners,
    once: bool,
    mode: NativeServeMode,
    record_program: Option<PathBuf>,
    record_output: Option<fm_types::OutputId>,
    record_program_2: Option<PathBuf>,
    record_output_2: Option<fm_types::OutputId>,
    record_wav: bool,
    record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    record_bitrate_kbps: Option<u32>,
    record_bitrate_kbps_2: Option<u32>,
    record_audio_bitrate_kbps: Option<u32>,
    record_audio_bitrate_kbps_2: Option<u32>,
    record_fallback_dir: Option<PathBuf>,
    diagnostic_stop_after: Option<Duration>,
    recover_to_checkpoint: bool,
) -> AppResult<()> {
    let ServeListeners {
        listen,
        web_listen,
        web: web_settings,
        status_listen,
        hls_listen,
        live_lan_listen,
    } = listeners;
    if !listen.ip().is_loopback() {
        return Err(AppFailure(format!(
            "development mode requires a loopback listen address, got {}",
            listen.ip()
        ))
        .into());
    }
    web_settings
        .validate(web_listen)
        .map_err(|error| AppFailure(error.to_string()))?;
    if web_listen.is_some() && once {
        return Err(AppFailure("--web-listen cannot be combined with --once".into()).into());
    }
    if status_listen.is_some_and(|address| !address.ip().is_loopback()) {
        return Err(AppFailure("--status-listen must use a loopback address".into()).into());
    }
    if hls_listen.is_some_and(|address| !address.ip().is_loopback()) {
        return Err(AppFailure("--hls-listen must use a loopback address".into()).into());
    }

    let store = ProjectStore::new(path)?;
    let project = load_and_recover(&store, recover_to_checkpoint)?;
    let bundle_assets = load_bundle_assets(&store, project.project())?;
    // Opened once and held for the run: this process is the journal's only
    // writer, and reopening it per command is what made an accepted command
    // cost what the whole show had cost so far.
    let journal = DurableJournal::open(&store)?;
    let project = persist_display_output_overrides(&journal, &project, mode.display_overrides())?;
    let project_id = project.project().id();
    let engine = restore_engine_with_titles(&project, &bundle_assets)?;
    let identity = format!("project-{project_id}");
    let control = Rc::new(RefCell::new(ControlService::new(
        engine,
        Policy::development(),
        identity.clone(),
        format!("{identity}-log"),
        ControlLimits::default(),
    )));
    // The simulated engine plays every media input from a synthetic source;
    // the native runtime opens each file's decoder.
    if matches!(&mode, NativeServeMode::Disabled) {
        attach_synthetic_media(&mut control.borrow_mut(), project.project())?;
    }
    #[cfg(feature = "display-surface")]
    let fullscreen_active = matches!(&mode, NativeServeMode::Program(_));
    let mut native = match mode {
        NativeServeMode::Disabled => None,
        NativeServeMode::Headless {
            camera_helper,
            display_inventory,
            display_overrides: _,
        } => {
            let mut native = NativeDaemon::start(&store, &project, camera_helper.as_deref())?;
            if let Some(inventory) = display_inventory {
                native.attach_display_inventory(inventory)?;
            }
            Some(native)
        }
        #[cfg(feature = "display-surface")]
        NativeServeMode::Program(setup) => {
            let ProgramServeSetup {
                context,
                outputs,
                windowed,
                display_overrides: _,
                camera_helper,
            } = *setup;
            Some(NativeDaemon::start_with_program(
                &store,
                &project,
                context,
                outputs,
                windowed,
                camera_helper.as_deref(),
            )?)
        }
    };
    if let Some(native) = native.as_mut() {
        native.attach_media_sources(&mut control.borrow_mut())?;
        // Replay records beside the journal unless the show names a root.
        native.attach_replay(
            &mut control.borrow_mut(),
            store.root().join("replay"),
            project.project().settings(),
        );
        native.attach_reel(project.project().settings());
        // Document inputs show their pages from the start (IN-014).
        #[cfg(feature = "native-media")]
        if let Some(pages) = journal.documents() {
            native.documents.observe(project.project(), pages);
        }
        #[cfg(feature = "native-media")]
        native.sync_media_list_prefetch(project.project());
    }
    // Every authored local HLS output is a configured show destination, so it
    // fails closed before serving exactly like the configured recording
    // output below: too little free space in any local output directory
    // refuses startup. The failure names the authored stream target and
    // carries free-space counts and I/O kinds only, never the directory.
    #[cfg(feature = "native-media")]
    if let Some(native) = native.as_ref() {
        native.preflight_local_outputs()?;
    }
    let mut process_shutdown = register_process_shutdown()?;
    let authority = control_server_identity(&control.borrow(), project_id);
    let mut durable = project;
    if record_program.is_some() || record_program_2.is_some() {
        if native.is_none() {
            let flag = if record_program.is_some() {
                "--record-program"
            } else {
                "--record-program-2"
            };
            return Err(AppFailure(format!("{flag} requires --native-media")).into());
        }
        validate_recording_output_selections(durable.project(), record_output, record_output_2)?;
        // Configured recording fails closed before any recorder exists. Each
        // preflight error carries free-space counts and I/O kinds only, never
        // the operator's paths.
        preflight_recording_outputs([
            ("--record-program", record_program.as_deref()),
            ("--record-program-2", record_program_2.as_deref()),
        ])?;
        validate_recording_sessions(
            [
                ("--record-program", record_program.as_deref()),
                ("--record-program-2", record_program_2.as_deref()),
            ],
            record_fallback_dir.as_deref(),
        )?;
        let Some(native) = native.as_mut() else {
            return Err("recording requires the native media runtime".into());
        };
        native.configure_recordings(
            record_program.clone(),
            record_output,
            record_program_2.clone(),
            record_output_2,
            record_wav,
            record_segment_seconds,
            record_bitrate_kbps,
            record_audio_bitrate_kbps,
            record_bitrate_kbps_2,
            record_audio_bitrate_kbps_2,
        );
        native.configure_record_fallback(record_fallback_dir);
        // A crash may have left sessions of these slots open: repair them
        // before anything records next to them or derives a name from them.
        native.recover_recording_sessions();
        // Startup reconciliation is deliberate: each configured recorder only
        // auto-starts when its own restored desired flag says so, so a slot
        // whose flag is false stays closed even when the other is recording.
        // The capability digest below keeps advertising configured support
        // either way; runtime RecordStart commands open segments later on
        // demand.
        if durable
            .project()
            .recording_desired_slots()
            .iter()
            .any(|&active| active)
        {
            // The playlist and cue clocks run inside the startup barrier
            // too: a restored playlist keeps cutting on its scheduled frames
            // and a restored cue wait still ends on its due frame while the
            // recorder starts, exactly as the session loop's clocks do.
            let stored = durable.clone();
            let principal = development_principal()?;
            let mut clock = playlist_clock::PlaylistClock::default();
            let mut cues = cue_clock::CueClock::default();
            let mut before_frame =
                |native: &mut NativeDaemon, control: &mut ControlService<Policy>| {
                    // As in the session loop: the clocks wait while the
                    // frame loop realizes a transition or title phase.
                    if control.work_in_flight() {
                        return Ok(());
                    }
                    if let Some(advance) = clock.next(
                        control,
                        playlist_clock::Pacing::Engine,
                        || !journal.busy() && native.retry_window(journal.recent_append_peak()),
                        now_millis()?,
                    )? {
                        let delivery = execute_native_authority_command(
                            control,
                            &journal,
                            &mut durable,
                            &principal,
                            &authority,
                            &advance.command(),
                            now_millis()?,
                            native,
                            Some(&process_shutdown),
                        )?;
                        clock.observe(&advance, &delivery.result);
                    }
                    if let Some(advance) = cues.next(
                        control,
                        playlist_clock::Pacing::Engine,
                        || !journal.busy() && native.retry_window(journal.recent_append_peak()),
                        now_millis()?,
                    )? {
                        let delivery = execute_native_authority_command(
                            control,
                            &journal,
                            &mut durable,
                            &principal,
                            &authority,
                            &advance.command(),
                            now_millis()?,
                            native,
                            Some(&process_shutdown),
                        )?;
                        cues.observe(&advance, &delivery.result);
                    }
                    Ok(())
                };
            let primed = native.start_and_prime_configured_recorders(
                &stored,
                &mut control.borrow_mut(),
                &authority,
                Some(&process_shutdown),
                &mut before_frame,
            );
            match primed {
                Ok(true) => {}
                Ok(false) => {
                    checkpoint_native(&control, &authority, &journal, &mut durable)?;
                    native.finalize_recorder()?;
                    return Ok(());
                }
                Err(error) => {
                    // Fail closed for configured recording; whatever already
                    // started is finalized and checkpointed through the same
                    // shutdown path this branch always used.
                    let _ = native.finalize_recorder();
                    return Err(error);
                }
            }
        }
    }
    // Every configured network stream endpoint is probed exactly once here,
    // after the disk preflights and before readiness is advertised. The probe
    // is bounded and never refuses startup, changes destination state, or
    // touches recorder or engine state: an ingest may legitimately come up
    // later, and the shared output set owns reconnection. Each probed endpoint
    // emits one sanitized FREEMIXD_PREFLIGHT record.
    #[cfg(feature = "native-media")]
    if let Some(native) = native.as_ref() {
        for record in native.network_preflight_records() {
            eprintln!("{record}");
        }
    }
    #[cfg(not(feature = "display-surface"))]
    let fullscreen_active = false;
    // Reconciled stream targets start here, after all preflight succeeded and
    // before readiness is advertised. A refusing destination latches its own
    // target and is reported; it never keeps the daemon from becoming ready.
    if let Some(native) = native.as_mut() {
        native.prime_startup_streams(&durable);
    }
    // The immutable handshake digest advertises successfully configured
    // startup support. Late recorder health is reported by FREEMIXD_RECORDER.
    let capabilities_digest = capabilities_digest(
        native.is_some(),
        fullscreen_active,
        native
            .as_ref()
            .is_some_and(NativeDaemon::recording_configured),
    );
    let config = ServerConfig::new(
        ServerMode::Development,
        AuthenticationMode::Development,
        listen.ip(),
        capabilities_digest,
    );
    let mut server = Server::new(config, ControlHandle(Rc::clone(&control)))?;
    let principal = development_principal()?;
    let listener = TcpListener::bind(listen)?;
    listener.set_nonblocking(true)?;
    let web_gateway = if let Some(address) = web_listen {
        let web_listener = TcpListener::bind(address)?;
        web_listener.set_nonblocking(true)?;
        Some(web::WebGateway::bind(
            web_listener,
            &web_settings,
            Some(store.assets_root()),
        )?)
    } else {
        None
    };
    let operator_status = status_listen
        .map(|address| StatusListener::bind(address, &process_shutdown.status))
        .transpose()?;
    if operator_status.is_some() {
        // Operator state is rendered only when a listener consumes it; the
        // attach happens after the bind so a refused listener never leaves a
        // half-wired publisher behind.
        process_shutdown.attach_control(&control);
    }
    // The HLS listener is a read-only view of the compiled `hls://` destination
    // directories. It is refused when no such destination is configured, and
    // its token is required before readiness is advertised, exactly like the
    // status listener's.
    let hls_listener = match hls_listen {
        Some(address) => {
            if native.is_none() {
                return Err(AppFailure("--hls-listen requires --native-media".into()).into());
            }
            #[cfg(feature = "native-media")]
            let roots = hls_roots(native.as_ref()).map_err(|error| {
                AppFailure(format!(
                    "--hls-listen could not map hls:// stream destinations: {error}"
                ))
            })?;
            #[cfg(not(feature = "native-media"))]
            let roots = HlsRoots::try_from_targets(std::iter::empty::<(u128, PathBuf)>()).map_err(
                |error| {
                    AppFailure(format!(
                        "--hls-listen could not map hls:// stream destinations: {error}"
                    ))
                },
            )?;
            if roots.is_empty() {
                return Err(AppFailure(
                    "--hls-listen requires at least one hls:// stream destination".into(),
                )
                .into());
            }
            let token = hls_server::token_from_environment()?;
            let listener =
                HlsServer::start(address, token, roots, Arc::new(AtomicBool::new(false)))?;
            #[cfg(feature = "native-media")]
            if let Some(native) = native.as_mut() {
                native.hls_routes = Some(listener.routes());
            }
            Some(listener)
        }
        None => None,
    };
    // The LiveLAN viewer listener (OR-017) serves whatever the project's
    // LiveLAN settings allow; it may bind a LAN address.
    let live_lan_listener = match live_lan_listen {
        Some(address) => {
            if native.is_none() {
                return Err(AppFailure("--live-lan-listen requires --native-media".into()).into());
            }
            let listener = live_lan::LiveLanServer::start(address)?;
            #[cfg(feature = "native-media")]
            if let Some(native) = native.as_mut() {
                native.attach_live_lan(listener.handle(), durable.project().settings().clone());
            }
            Some(listener)
        }
        None => None,
    };
    server.mark_ready()?;
    process_shutdown.observe_service(server.status());
    let readiness = ReadinessRecord {
        address: listener.local_addr()?,
        project_id,
    };
    println!("{readiness}");
    std::io::stdout().flush()?;
    if let Some(web) = web_gateway.as_ref() {
        println!("FREEMIXD_WEB_READY\tv=1\taddress={}", web.address());
        if let Some(record) = web.tls_record() {
            println!("{record}");
        }
        std::io::stdout().flush()?;
        web.start_accepting();
    }
    if let Some(listener) = operator_status.as_ref() {
        println!(
            "{}",
            StatusReadinessRecord {
                address: listener.address()
            }
        );
        std::io::stdout().flush()?;
    }
    if let Some(listener) = hls_listener.as_ref() {
        println!("FREEMIXD_HLS_READY\tv=1\taddress={}", listener.address());
        std::io::stdout().flush()?;
    }
    if let Some(listener) = live_lan_listener.as_ref() {
        println!(
            "FREEMIXD_LIVE_LAN_READY\tv=1\taddress={}",
            listener.address()
        );
        std::io::stdout().flush()?;
    }
    if let Some(duration) = diagnostic_stop_after {
        process_shutdown.set_diagnostic_deadline(duration)?;
    }

    // Title templates and fonts load once, before serving, for the simulated
    // monitor tap and for exact title previews (which native sessions serve
    // too). Native media loads its own for its title render worker (see
    // `native_titles`).
    let titles = Arc::new(title_render::TitleAssets::load(&store, durable.project()));
    let session_result = sessions::run(
        &listener,
        &server,
        &control,
        &journal,
        &mut durable,
        &principal,
        native.as_mut(),
        &authority,
        &titles,
        &process_shutdown,
        once,
        web_gateway.as_ref(),
    );
    process_shutdown.status.begin_draining();
    let gateway_result = web_gateway.map(web::WebGateway::shutdown);
    let status_result = operator_status.map(StatusListener::shutdown);
    let hls_result = hls_listener.map(HlsServer::shutdown);
    let live_lan_result = live_lan_listener.map(live_lan::LiveLanServer::shutdown);
    let _shutdown_reason = session_result?;
    if let Some(result) = gateway_result {
        result?;
    }
    if let Some(result) = status_result {
        result?;
    }
    if let Some(result) = hls_result {
        result?;
    }
    if let Some(result) = live_lan_result {
        result?;
    }
    if native.is_some() {
        checkpoint_native(&control, &authority, &journal, &mut durable)?;
    } else {
        // A clean stop is the safest checkpoint there is: no command is in
        // flight, so the manifest ends the run holding everything the journal
        // recorded and the next start has nothing to replay.
        journal.settle(&durable)?;
    }
    #[cfg(feature = "display-surface")]
    if fullscreen_active {
        let frames_presented = native
            .as_ref()
            .and_then(NativeDaemon::program_telemetry)
            .map(|telemetry| telemetry.frames_presented)
            .unwrap_or_default();
        eprintln!("FREEMIXD_PROGRAM\tv=1\tframes_presented={frames_presented}");
    }
    #[cfg(feature = "native-media")]
    if let Some((frames_presented, failures, frames_dropped)) = native
        .as_ref()
        .and_then(NativeDaemon::display_output_totals)
    {
        eprintln!(
            "FREEMIXD_DISPLAY_OUTPUTS\tv=1\tframes_presented={frames_presented}\tfailures={failures}\tframes_dropped={frames_dropped}"
        );
    }
    if let Some(native) = &mut native {
        // Streams stop before the recorder so the final records reflect the
        // same shutdown instant, and their results report after the recorder's
        // like camera results do.
        let stream_results = native.finalize_streams();
        let recorder_result = native.finalize_recorder();
        native.emit_camera_source_telemetry();
        let camera_result = native.finalize_cameras();
        native.emit_telemetry();
        recorder_result?;
        camera_result?;
        for result in stream_results {
            result?;
        }
    }
    Ok(())
}

/// Work a native frame loop outside the session loop runs before each frame
/// it lets the pacer realize.
type BeforeNativeFrame<'a> =
    dyn FnMut(&mut NativeDaemon, &mut ControlService<Policy>) -> AppResult<()> + 'a;

fn checkpoint_native(
    control: &SharedControl,
    server: &ServerIdentity,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
) -> AppResult<()> {
    // A transition or title phase the frame loop was still realizing
    // settles unrendered, as the frames of a command interrupted by
    // shutdown always did, up to where the durable project says it ends.
    {
        let mut control = control.borrow_mut();
        while control.work_in_flight()
            && control.frame_cursor() < durable.position().frames_rendered
        {
            control.tick_for_shutdown(server)?;
        }
    }
    let snapshot = control.borrow().idle_engine_snapshot()?;
    *durable = stored_project_checkpoint(durable, &snapshot)?;
    // The idle snapshot advances the runtime position without advancing the
    // revision, so it is not a mutation: it belongs in the manifest directly.
    store.checkpoint(durable)?;
    Ok(())
}

/// Records a native daemon's idle progress (see [`journal::ProgressCadence`]).
///
/// The session loop calls [`Self::poll`] once per pass, right after the
/// pacer's frame. The journal writes a marker or fold on its persistence
/// worker, so neither delays a frame. A marker is one small journal commit per
/// [`journal::PROGRESS_MARKER_SECONDS`]; a fold is a manifest checkpoint per
/// [`journal::PROGRESS_FOLD_SECONDS`]. Progress is only recorded at an idle
/// frame boundary — the only position replay (and a checkpoint) can
/// reproduce — so a pass with work in flight records nothing and the next
/// pass tries again. A failed write is reported once and never stops the
/// show: it loses at most idle progress, never an acknowledged command.
pub(crate) struct NativeProgress {
    cadence: journal::ProgressCadence,
    failing: bool,
    /// The durable project changed outside a command (screen capture
    /// restore tokens, IN-011): the next pass folds it.
    fold_requested: bool,
}

impl NativeProgress {
    pub(crate) fn new(durable: &StoredProject) -> Self {
        Self {
            cadence: journal::ProgressCadence::new(
                durable.project().settings().frame_rate,
                durable.position().frames_rendered,
            ),
            failing: false,
            fold_requested: false,
        }
    }

    /// Folds the durable project on the next pass that can, whatever the
    /// cadence says. Only a native daemon's screen capture grants ask.
    #[cfg(feature = "native-media")]
    pub(crate) fn request_fold(&mut self) {
        self.fold_requested = true;
    }

    pub(crate) fn poll(
        &mut self,
        control: &SharedControl,
        store: &dyn DurableStore,
        durable: &mut StoredProject,
    ) {
        let control = control.borrow();
        let cursor = control.frame_cursor();
        let mut due = self.cadence.due(cursor, durable.position().frames_rendered);
        if self.fold_requested {
            due = journal::ProgressDue::Fold;
        }
        // One background write at a time: a marker or fold due while the
        // previous one (or a checkpoint) is still being written waits for a
        // later pass instead of queueing behind it.
        if due == journal::ProgressDue::Nothing || store.busy() {
            return;
        }
        let Ok(snapshot) = control.idle_engine_snapshot() else {
            return;
        };
        drop(control);
        let result = if due == journal::ProgressDue::Fold {
            self.fold_requested = false;
            self.cadence.folded(cursor);
            // The idle snapshot moves the runtime position without moving
            // the revision, exactly as the shutdown checkpoint does.
            stored_project_checkpoint(durable, &snapshot).and_then(|folded| {
                store.fold_progress(&folded)?;
                *durable = folded;
                Ok(())
            })
        } else {
            self.cadence.marked(cursor);
            store.record_progress(snapshot_position(&snapshot))
        };
        match (result, self.failing) {
            (Ok(()), true) => {
                self.failing = false;
                eprintln!("FREEMIXD_JOURNAL\tv=1\tprogress=recovered\tframe={cursor}");
            }
            (Err(error), false) => {
                self.failing = true;
                eprintln!("FREEMIXD_JOURNAL\tv=1\tprogress=failed\tframe={cursor}\terror={error}");
            }
            // Reported once per failing streak.
            (Ok(()), false) | (Err(_), true) => {}
        }
    }
}

/// The durable coordinates of an idle engine snapshot.
fn snapshot_position(snapshot: &EngineSnapshot) -> ProjectPosition {
    ProjectPosition {
        revision: snapshot.revision().get(),
        state_epoch: snapshot.state_epoch().get(),
        event_sequence: snapshot.event_sequence().get(),
        frames_rendered: snapshot.frames_rendered(),
        runtime_generation: snapshot.runtime_generation().get(),
        clock_time_nanos: snapshot.clock_time().as_nanos(),
    }
}

/// Maps one validated model recorder slot onto the switcher's slot domain.
///
/// Both domains cover exactly `1..=2`, so this conversion cannot fail for a
/// slot the model itself constructed.
fn switcher_recorder_slot(slot: ModelRecorderSlot) -> RecorderSlot {
    RecorderSlot::new(slot.get()).unwrap_or(RecorderSlot::ALL[0])
}

/// The stored template of every title input, keyed by input id.
type TitleTemplates = std::collections::BTreeMap<InputId, fm_titles::TitleTemplate>;

/// What a restored engine needs from the project bundle besides
/// `project.json`: every title input's template and every imported LUT's
/// text.
#[derive(Default)]
struct BundleAssets {
    titles: TitleTemplates,
    luts: std::collections::BTreeMap<fm_model::LutId, fm_engine::EngineLut>,
    /// Where document assets are read from (IN-014); `None` without a
    /// bundle.
    assets_root: Option<PathBuf>,
}

/// Loads the title templates (see [`load_title_templates`]) and the LUTs
/// (see [`grading::load_grade_luts`]) of `project`.
fn load_bundle_assets(
    store: &ProjectStore,
    project: &fm_model::Project,
) -> AppResult<BundleAssets> {
    Ok(BundleAssets {
        titles: load_title_templates(store, project)?,
        luts: grading::load_grade_luts(store, project),
        assets_root: Some(store.assets_root()),
    })
}

/// Loads the stored template of every title input in `project`.
///
/// Title field state is restored against its template, so a title whose
/// template does not load, or whose names do not fit the wire bounds of a
/// `title_state` record, makes the daemon refuse to start: serving it would
/// mean inventing or dropping the operator's field values.
fn load_title_templates(
    store: &ProjectStore,
    project: &fm_model::Project,
) -> AppResult<TitleTemplates> {
    let mut templates = TitleTemplates::new();
    for input in project.inputs() {
        let fm_model::InputKind::Title(title) = &input.kind else {
            continue;
        };
        let stored = store
            .load_title_template(&title.template_uri)
            .map_err(|error| {
                AppFailure(format!(
                    "title input {} template `{}` could not be loaded: {error}",
                    input.id, title.template_uri
                ))
            })?;
        let template = stored.template;
        if template.name.trim().is_empty()
            || template.name.len() > fm_protocol::MAX_TITLE_NAME_BYTES
        {
            return Err(AppFailure(format!(
                "title input {} template name must be nonblank and at most {} bytes",
                input.id,
                fm_protocol::MAX_TITLE_NAME_BYTES
            ))
            .into());
        }
        let names_bytes = template
            .fields
            .iter()
            .map(|field| field.name.len())
            .fold(0_usize, usize::saturating_add);
        if template
            .fields
            .iter()
            .any(|field| field.name.trim().is_empty())
            || names_bytes > fm_protocol::MAX_TITLE_FIELD_NAMES_BYTES
        {
            return Err(AppFailure(format!(
                "title input {} template field names must be nonblank and total at most {} bytes",
                input.id,
                fm_protocol::MAX_TITLE_FIELD_NAMES_BYTES
            ))
            .into());
        }
        // Every snapshot carries the animation in a `title_animation_state`
        // record, so one without a wire encoding would lock out every
        // session.
        let animation_bytes = fm_protocol::title_clip_wire_bytes(&fm_control::title_clip_to_wire(
            &template.animation.clip,
        ));
        if animation_bytes > fm_protocol::MAX_TITLE_ANIMATION_BYTES {
            return Err(AppFailure(format!(
                "title input {} template animation encodes to {animation_bytes} bytes, more \
                 than the {} a title_animation_state record carries",
                input.id,
                fm_protocol::MAX_TITLE_ANIMATION_BYTES
            ))
            .into());
        }
        templates.insert(input.id, template);
    }
    Ok(templates)
}

/// Seeds every title input's persisted field values and revisions into the
/// restored show, checked against its template.
fn restore_titles(
    show: &mut ShowState,
    project: &fm_model::Project,
    templates: &TitleTemplates,
) -> AppResult<()> {
    for input in project.inputs() {
        let fm_model::InputKind::Title(title) = &input.kind else {
            continue;
        };
        let template = templates.get(&input.id).ok_or_else(|| {
            AppFailure(format!("title input {} has no loaded template", input.id))
        })?;
        let fields = fm_titles::TitleFieldSet::restore_revisioned(
            template,
            title.field_revision,
            title.fields.iter().map(|entry| {
                (
                    fm_titles::FieldId::new(entry.id.get()),
                    title_render::field_value(&entry.value),
                    entry.revision,
                )
            }),
        )
        .map_err(|error| {
            AppFailure(format!(
                "title input {} fields do not match template `{}`: {error}",
                input.id, title.template_uri
            ))
        })?;
        // Every title input naming the same stored template shares one
        // animation, keyed by its URI (GX-011).
        show.set_title(
            input.id,
            EngineTitle::new(template.name.clone(), fields)
                .with_template(title.template_uri.clone(), template.clone()),
        )?;
    }
    Ok(())
}

#[cfg(test)]
fn restore_engine(project: &StoredProject) -> AppResult<Engine> {
    restore_engine_with_titles(project, &BundleAssets::default())
}

#[allow(clippy::too_many_lines)] // one restore step per project part
/// Opens every media input of `project` on its synthetic source (the
/// simulated engine's stand-in for the file's decoder, see
/// [`fm_engine::EngineMediaSource::synthetic`]), unless its source is
/// already open or the input is closed (IN-026). After a command this
/// opens imported, duplicated, replaced, and reopened media inputs.
fn attach_synthetic_media(
    control: &mut ControlService<Policy>,
    project: &fm_model::Project,
) -> AppResult<()> {
    let rate = project.settings().frame_rate;
    for input in project.inputs() {
        if project.input_lifecycle(input.id).closed || control.media_source_open(input.id) {
            continue;
        }
        if let fm_model::InputKind::Media { playback, .. } = &input.kind {
            control
                .set_media_source(
                    input.id,
                    Some(fm_engine::EngineMediaSource::synthetic(playback, rate)),
                )
                .map_err(|error| {
                    AppFailure(format!("media input {} cannot open: {error}", input.id))
                })?;
        }
    }
    Ok(())
}

/// The animation preset library, the favourite transition buttons, and
/// each media input's playback settings (IN-003).
fn restore_presets_favorites_and_media(
    show: &mut ShowState,
    canonical: &fm_model::Project,
) -> AppResult<()> {
    show.restore_animation_presets(canonical.animation_presets().clone())
        .map_err(|error| {
            AppFailure(format!(
                "project animation presets cannot be restored: {error}"
            ))
        })?;
    show.restore_transition_favorites(*canonical.transition_favorites());
    for input in canonical.inputs() {
        if let fm_model::InputKind::Media { playback, .. } = &input.kind {
            show.set_media_input(input.id, *playback)?;
        }
        // Each input's replaceable source, for replace and duplicate
        // (IN-026).
        if let Some(source) = fm_model::InputSourceSpec::from_kind(&input.kind) {
            show.set_input_source(input.id, source)?;
        }
        if let fm_model::InputKind::Device { capture, .. } = &input.kind {
            show.set_capture_settings(input.id, capture.clone())?;
        }
        // A document opens on the page it was left on (IN-014).
        if let fm_model::InputKind::Document(document) = &input.kind {
            show.set_paged_page(input.id, document.page)?;
        }
    }
    show.restore_input_lifecycle(
        canonical.input_categories().to_vec(),
        canonical.input_lifecycles(),
    )?;
    // What is recorded for replay (OR-012), once every input's source is
    // known.
    show.set_replay(canonical.replay().clone())
        .map_err(|error| AppFailure(format!("project replay cannot be restored: {error}")))?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn restore_engine_with_titles(
    project: &StoredProject,
    bundle_assets: &BundleAssets,
) -> AppResult<Engine> {
    let canonical = project.project();
    let inputs = canonical
        .inputs()
        .iter()
        .map(|input| (input.id, input.name.clone()))
        .collect::<Vec<_>>();
    let input_ids = inputs.iter().map(|(input, _)| *input).collect::<Vec<_>>();
    let main_mix = canonical
        .main_mix()
        .ok_or_else(|| AppFailure("project is missing desired main mix routing".into()))?;
    let routing = project.runtime_routing();
    let realized_program = required_routing(routing.realized_program_id, "realized program")?;
    let realized_preview = required_routing(routing.realized_preview_id, "realized preview")?;
    // The stream inventory and desired running flags are seeded from the
    // project in vector order, so the restored show keeps the authored order.
    // Duplicates, oversized inventories, and blank names fail closed through
    // the switcher's own validation errors.
    let stream_inventory = canonical
        .stream_targets()
        .iter()
        .map(|target| {
            (
                SwitcherStreamTargetId::from_non_zero(target.id().get()),
                target.name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let mut show = ShowState::new(
        canonical.name(),
        inputs,
        main_mix.desired_program,
        main_mix.desired_preview,
    )?
    .with_outputs(
        canonical
            .outputs()
            .iter()
            .map(|output| (output.id, output.name.clone()))
            .collect(),
    )?
    .with_stream_targets(canonical.stream_targets())?;
    restore_input_audio_strips(&mut show, canonical)?;

    restore_display_outputs(&mut show, canonical)?;
    let mut realized = SwitcherState::new(input_ids, realized_program, realized_preview)?
        .with_streams(stream_inventory)?;
    // The Fade-to-Black output selection is project configuration, seeded
    // into both switchers like the recording flags below.
    for &output in canonical.fade_to_black_excluded_outputs() {
        let _ = realized.set_fade_to_black_output(output, false);
    }
    for (&output, &duration) in canonical.fade_to_black_output_durations() {
        realized
            .set_fade_to_black_output_duration(output, Some(duration))
            .map_err(|error| AppFailure(format!("invalid Fade-to-Black duration: {error}")))?;
    }
    let _ = realized.set_fade_to_black_curve(canonical.fade_to_black_curve());
    for target in canonical
        .stream_targets()
        .iter()
        .filter(|target| target.running())
    {
        let target_id = SwitcherStreamTargetId::from_non_zero(target.id().get());
        show.set_stream_running(target_id, true)?;
        // Idle restore requires desired and realized to agree on stream state;
        // realization itself is daemon-side runtime work, not engine history.
        realized.set_stream_running(target_id, true)?;
    }
    // The recording flags are engine-owned desired state, so they are seeded
    // into BOTH switchers slot by slot: idle restore demands desired/realized
    // agreement, while realization of each desire is daemon-side recorder
    // work.
    for slot in ModelRecorderSlot::ALL {
        let active = canonical.recording_desired(slot);
        let recorder = switcher_recorder_slot(slot);
        show.desired_switcher_mut()
            .set_recording_desired(recorder, active)?;
        realized.set_recording_desired(recorder, active)?;
    }
    for config in canonical.stingers() {
        restore_stinger(&mut show, &mut realized, *config)?;
    }
    let manual = project.runtime_manual_transitions();
    if let Some(state) = manual.desired {
        show.restore_manual_transition(restored_t_bar(state)?)?;
    }
    if let Some(state) = manual.realized {
        realized.restore_t_bar(restored_t_bar(state)?)?;
    }
    let fade_to_black = project.runtime_fade_to_black();
    show.restore_fade_to_black(fade_to_black.desired.target_active);
    realized.restore_settled_fade_to_black(fade_to_black.realized.target_active);
    restore_overlays(&mut show, &mut realized, project.runtime_overlays())?;
    restore_edit_locks(&mut show, canonical)?;
    restore_titles(&mut show, canonical, &bundle_assets.titles)?;
    restore_scenes(&mut show, canonical)?;
    show.set_playlist(fm_engine::playlist_from_model(canonical.playlist())?)?;
    show.restore_grading(canonical.grading().clone(), bundle_assets.luts.clone())
        .map_err(|error| AppFailure(format!("project grading cannot be restored: {error}")))?;
    restore_presets_favorites_and_media(&mut show, canonical)?;
    // Each list input's active item is its media source (IN-006).
    show.restore_media_lists(canonical.media_lists().clone())?;
    // Checked against the restored titles, media inputs, and scripts (a cue
    // may run one, schema 55).
    show.set_scripts(canonical.scripts().to_vec())?;
    show.set_cue_lists(canonical.cue_lists().to_vec())?;
    // A run paused at a wait resumes on its due frame (schema 56).
    show.set_script_run(
        canonical.script_run().clone(),
        canonical.settings().frame_rate,
    )?;
    show.set_shortcuts(canonical.shortcuts().to_vec())?;
    let position = project.position();
    let mut engine = Engine::restore_persisted(
        show,
        realized,
        canonical.settings().frame_rate,
        clock_domain(),
        EngineRestoreState {
            state_epoch: StateEpoch::new(position.state_epoch),
            revision: Revision::new(position.revision),
            event_sequence: EventSequence::new(position.event_sequence),
            runtime_generation: RuntimeGeneration::new(position.runtime_generation),
            clock_time: ClockTime::from_nanos(position.clock_time_nanos),
            frame_cursor: position.frames_rendered.into(),
            receipts: project
                .idempotency_receipts()
                .iter()
                .map(runtime_receipt)
                .collect::<AppResult<Vec<_>>>()?,
        },
    )?;
    // Every shown title continues its timeline (GX-001, schema 68).
    engine.restore_title_timelines(canonical.title_timelines())?;
    Ok(engine)
}

/// Seeds every Scene input's scene (layers and animation) into the restored
/// show, so scene animation edits and on-air phases are engine state
/// (GX-011 stage 5), and every other project scene, so its layer grades can
/// be edited and every snapshot lays it out (SW-019, Protocol 2.35). Every
/// snapshot carries each shown scene's animation in a
/// `scene_animation_state` record, so a scene whose clip has no wire
/// encoding fails closed rather than locking out every session. Every
/// alias input's target is seeded too, so a live layer edit's cycle check
/// follows aliases (SW-006).
fn restore_scenes(show: &mut ShowState, project: &fm_model::Project) -> AppResult<()> {
    let dimensions = project.settings().video.dimensions;
    for scene in project.scenes() {
        show.set_scene(scene.clone(), dimensions.width(), dimensions.height());
    }
    for input in project.inputs() {
        if let fm_model::InputKind::Alias { target, .. } = input.kind {
            show.set_input_alias(input.id, target)?;
        }
    }
    for input in project.inputs() {
        let fm_model::InputKind::Scene { scene_id, .. } = &input.kind else {
            continue;
        };
        let scene = project.scene(*scene_id).ok_or_else(|| {
            AppFailure(format!(
                "scene input {} names missing scene {scene_id}",
                input.id
            ))
        })?;
        let animation_bytes = fm_protocol::scene_clip_wire_bytes(&fm_control::scene_clip_to_wire(
            &scene.animation.clip,
        ));
        if animation_bytes > fm_protocol::MAX_SCENE_ANIMATION_BYTES {
            return Err(AppFailure(format!(
                "scene {scene_id} animation encodes to {animation_bytes} bytes, more than the \
                 {} a scene_animation_state record carries",
                fm_protocol::MAX_SCENE_ANIMATION_BYTES
            ))
            .into());
        }
        show.set_scene_input(
            input.id,
            scene.clone(),
            dimensions.width(),
            dimensions.height(),
        )?;
    }
    Ok(())
}

/// Persists every engine scene's layers (live scene layer edits, SW-006)
/// and animation back into the project. Layers go first: an animation track
/// may name a layer an edit added.
fn sync_project_scenes(project: &mut fm_model::Project, show: &ShowState) -> AppResult<()> {
    for scene in show.scenes().values() {
        let layers_unchanged = project
            .scene(scene.id())
            .is_some_and(|stored| stored.layers == scene.scene.layers);
        if !layers_unchanged {
            project
                .set_scene_layers(scene.id(), scene.scene.layers.clone())
                .map_err(|error| {
                    AppFailure(format!(
                        "scene {} layers could not be persisted: {error}",
                        scene.id()
                    ))
                })?;
        }
    }
    for scene in show.scenes().values() {
        let unchanged = project
            .scene(scene.id())
            .is_some_and(|stored| stored.animation == *scene.animation());
        if unchanged {
            continue;
        }
        project
            .set_scene_animation(scene.id(), scene.animation().clone())
            .map_err(|error| {
                AppFailure(format!(
                    "scene {} animation could not be persisted: {error}",
                    scene.id()
                ))
            })?;
    }
    Ok(())
}

/// Seeds the persisted edit padlock into the restored show, so a checkpoint
/// restore and a journal replay from that checkpoint start from the same lock
/// state. Validation already guarantees every locked input exists.
fn restore_edit_locks(show: &mut ShowState, project: &fm_model::Project) -> AppResult<()> {
    show.set_project_locked(project.project_locked());
    for &input in project.locked_inputs() {
        show.set_input_locked(input, true)?;
    }
    Ok(())
}

fn restore_stinger(
    show: &mut ShowState,
    realized: &mut SwitcherState,
    config: StingerConfig,
) -> AppResult<()> {
    let slot = StingerSlotId::new(config.slot.number()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a validated Stinger slot is in the switcher range",
        )
    })?;
    let descriptor = StingerDescriptor::new(
        config.media_input,
        config.preload,
        config.cut_point_frames,
        match config.audio_policy {
            ModelStingerAudioPolicy::Muted => StingerAudioPolicy::Muted,
            ModelStingerAudioPolicy::StingerOnly => StingerAudioPolicy::StingerOnly,
            ModelStingerAudioPolicy::MixWithProgram => StingerAudioPolicy::MixWithProgram,
        },
        match config.missing_media_fallback {
            StingerMissingMediaFallback::Cut => MissingMediaFallback::Cut,
            StingerMissingMediaFallback::Fade => MissingMediaFallback::Fade,
            StingerMissingMediaFallback::KeepProgram => MissingMediaFallback::KeepProgram,
        },
    );
    show.configure_stinger(slot, descriptor)?;
    realized.configure_stinger(slot, descriptor)?;

    if config.preload {
        // Native startup resolves every requested source before the first tick.
        // Until live source-health events are projected into the engine, a
        // successfully restored input is the deterministic ready state.
        let _ = show.preload_stinger(slot, true)?;
        let _ = realized.preload_stinger(slot, true)?;
    }
    Ok(())
}

/// The engine's plugin chain for a persisted one (AU-006, schema 91).
fn engine_plugin_chain(
    state: &fm_types::audio_plugins::PluginChainState,
) -> AppResult<fm_plugin_host_api::PluginChain> {
    let limits = fm_plugin_host_api::ProtocolLimits::default();
    let mut chain = fm_plugin_host_api::PluginChain::new();
    for slot in state.slots() {
        let plugin = fm_plugin_host_api::PluginId::new(slot.plugin());
        let mut engine_slot = fm_plugin_host_api::PluginSlot::new(plugin.clone());
        engine_slot.set_bypassed(slot.is_bypassed());
        engine_slot
            .set_latency(slot.latency_frames())
            .map_err(|error| AppFailure(format!("project plugin chain is invalid: {error}")))?;
        if !slot.state().is_empty() {
            let snapshot = fm_plugin_host_api::StateSnapshot::new(
                plugin,
                fm_plugin_host_api::StateVersion::new(0),
                slot.state().to_vec(),
                &limits,
            )
            .map_err(|error| AppFailure(format!("project plugin state is invalid: {error}")))?;
            engine_slot
                .store_snapshot(snapshot)
                .map_err(|error| AppFailure(format!("project plugin state is invalid: {error}")))?;
        }
        chain
            .push(engine_slot)
            .map_err(|error| AppFailure(format!("project plugin chain is invalid: {error}")))?;
    }
    Ok(chain)
}

/// The persisted form of the engine's plugin chain (AU-006, schema 91).
fn model_plugin_chain(
    chain: &fm_plugin_host_api::PluginChain,
) -> AppResult<fm_types::audio_plugins::PluginChainState> {
    let mut state = fm_types::audio_plugins::PluginChainState::new();
    for slot in chain.slots() {
        let mut model = fm_types::audio_plugins::PluginSlotState::new(slot.plugin().as_str());
        model.set_bypassed(slot.is_bypassed());
        model
            .set_latency(slot.latency_frames())
            .map_err(|error| AppFailure(format!("plugin chain is invalid: {error}")))?;
        if let Some(snapshot) = slot.snapshot() {
            model
                .set_state(snapshot.data().to_vec())
                .map_err(|error| AppFailure(format!("plugin chain is invalid: {error}")))?;
        }
        state
            .push(model)
            .map_err(|error| AppFailure(format!("plugin chain is invalid: {error}")))?;
    }
    Ok(state)
}

#[cfg(test)]
mod audio_plugin_chain_tests {
    use super::{engine_plugin_chain, model_plugin_chain};
    use fm_types::audio_plugins::{PluginChainState, PluginSlotState};

    #[test]
    fn persisted_and_engine_plugin_chains_round_trip() {
        let mut trim = PluginSlotState::new("freemix.reference.trim");
        trim.set_bypassed(true);
        trim.set_latency(7).unwrap();
        trim.set_state([1_u8, 2, 3]).unwrap();
        let mut polarity = PluginSlotState::new("freemix.reference.polarity");
        polarity.set_state([9_u8]).unwrap();
        let persisted = PluginChainState::from_slots(vec![trim, polarity]).expect("a valid chain");
        let engine = engine_plugin_chain(&persisted).expect("valid persisted chain");
        assert_eq!(engine.len(), 2);
        assert!(engine.slots()[0].is_bypassed());
        assert_eq!(engine.slots()[0].latency_frames(), 7);
        assert_eq!(
            engine.slots()[0]
                .snapshot()
                .map(fm_plugin_host_api::StateSnapshot::data),
            Some([1_u8, 2, 3].as_slice())
        );
        assert_eq!(
            model_plugin_chain(&engine).expect("valid engine chain"),
            persisted
        );
        let empty = PluginChainState::new();
        assert!(engine_plugin_chain(&empty).unwrap().is_empty());
        assert_eq!(
            model_plugin_chain(&fm_plugin_host_api::PluginChain::new()).unwrap(),
            empty
        );
    }
}

fn restore_input_audio_strips(show: &mut ShowState, project: &fm_model::Project) -> AppResult<()> {
    for strip in project.input_audio_strips() {
        show.set_input_audio_strip(
            strip.input,
            EngineInputAudioStripState {
                gain_millidb: strip.state.gain.get(),
                balance_basis_points: strip.state.balance.get(),
                pan_basis_points: strip.state.pan.get(),
                muted: strip.state.muted,
                soloed: strip.state.soloed,
                follow_video: strip.state.follow_video,
                delay_samples: strip.state.delay_samples.get(),
                sends: strip.state.sends,
            },
        )?;
        show.set_input_audio_label(strip.input, strip.label.clone())?;
        show.set_input_audio_group(strip.input, strip.group)?;
        show.set_input_audio_dsp(strip.input, strip.dsp)?;
        show.set_input_audio_plugins(strip.input, engine_plugin_chain(&strip.plugins)?);
    }
    show.set_master_audio_dsp(*project.master_audio_dsp());
    show.set_master_audio_plugins(engine_plugin_chain(project.master_audio_plugins())?);
    for (group, value) in project.audio_groups().iter() {
        show.set_audio_group(
            group,
            fm_engine::EngineAudioGroup {
                name: value.name.clone(),
                state: fm_engine::EngineAudioGroupState {
                    gain_millidb: value.gain.get(),
                    muted: value.muted,
                },
                soloed: value.soloed,
                parent: value.parent,
            },
        )?;
    }
    for (bus, value) in project.aux_buses().iter() {
        show.set_aux_bus(
            bus,
            fm_engine::EngineAuxBus {
                name: value.name.clone(),
                state: fm_engine::EngineAudioGroupState {
                    gain_millidb: value.gain.get(),
                    muted: value.muted,
                },
                dsp: value.dsp,
            },
        )?;
    }
    for (&output, &bus) in project.output_aux_buses() {
        show.set_output_aux_bus(output, Some(bus));
    }
    show.set_audio_solo_mode(project.audio_solo_mode());
    let monitor = project.audio_monitor_output();
    show.set_audio_monitor(fm_engine::EngineAudioMonitor {
        device: monitor.device.clone(),
        state: fm_engine::EngineAudioGroupState {
            gain_millidb: monitor.gain.get(),
            muted: monitor.muted,
        },
    })?;
    show.restore_fade_to_black_excluded_outputs(
        project.fade_to_black_excluded_outputs().iter().copied(),
    )?;
    show.restore_fade_to_black_timing(
        project
            .fade_to_black_output_durations()
            .iter()
            .map(|(output, duration)| (*output, *duration)),
        project.fade_to_black_curve(),
    )?;
    Ok(())
}

/// Seeds the show's display-output slots (OR-011) and `LiveLAN`
/// configuration (OR-017) from the project.
fn restore_display_outputs(show: &mut ShowState, project: &fm_model::Project) -> AppResult<()> {
    for (&slot, config) in project.display_outputs() {
        show.set_display_output(slot, Some(config.clone()))?;
    }
    show.set_live_lan(project.live_lan().clone())?;
    for setting in project.alert_rules() {
        show.set_alert_rule(*setting)?;
    }
    Ok(())
}

/// Folds the show's `LiveLAN` configuration (OR-017) and display-output
/// slots (OR-011) into the stored project.
/// Every slot is cleared first, so a display moving between slots is never
/// claimed twice.
fn sync_project_display_outputs(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    if project.alert_rules() != show.alert_rules() {
        for rule in fm_types::alert_rules::HealthAlertRule::ALL {
            project
                .set_alert_rule(fm_types::alert_rules::effective_setting(
                    show.alert_rules(),
                    rule,
                ))
                .map_err(|error| AppFailure(format!("project cannot take alert rules: {error}")))?;
        }
    }
    if project.live_lan() != show.live_lan() {
        project
            .set_live_lan(show.live_lan().clone())
            .map_err(|error| AppFailure(format!("project cannot take LiveLAN: {error}")))?;
    }
    if project.replay() != show.replay() {
        project
            .set_replay(show.replay().clone())
            .map_err(|error| AppFailure(format!("project cannot take replay: {error}")))?;
    }
    if project.display_outputs() == show.display_outputs() {
        return Ok(());
    }
    for slot in DisplayOutputSlot::ALL {
        project.clear_display_output(slot);
    }
    for (&slot, config) in show.display_outputs() {
        project
            .set_display_output(slot, config.clone())
            .map_err(|error| {
                AppFailure(format!(
                    "project cannot take display output {slot}: {error}"
                ))
            })?;
    }
    Ok(())
}

/// Folds the show's strip labels, groups, and Fade-to-Black output
/// selection (AU-001, SW-004) into the stored project.
#[allow(clippy::too_many_lines)]
fn sync_project_audio_mixer(project: &mut fm_model::Project, show: &ShowState) -> AppResult<()> {
    for &input in show.inputs() {
        let label = show.input_audio_label(input);
        if project.input_audio_label(input) != Some(label) {
            project
                .set_input_audio_label(input, label.to_owned())
                .map_err(|error| {
                    AppFailure(format!(
                        "project cannot take input {input}'s strip label: {error}"
                    ))
                })?;
        }
        if !project.set_input_audio_group(input, show.input_audio_group(input)) {
            return Err(
                AppFailure(format!("project is missing audio strip for input {input}")).into(),
            );
        }
        project
            .set_input_audio_dsp(input, show.input_audio_dsp(input))
            .map_err(|error| {
                AppFailure(format!(
                    "project cannot take input {input}'s strip DSP: {error}"
                ))
            })?;
        project
            .set_input_audio_plugins(input, model_plugin_chain(show.input_audio_plugins(input))?)
            .map_err(|error| {
                AppFailure(format!(
                    "project cannot take input {input}'s plugin chain: {error}"
                ))
            })?;
    }
    project
        .set_master_audio_dsp(*show.master_audio_dsp())
        .map_err(|error| AppFailure(format!("project cannot take the Master DSP: {error}")))?;
    project
        .set_master_audio_plugins(model_plugin_chain(show.master_audio_plugins())?)
        .map_err(|error| {
            AppFailure(format!(
                "project cannot take the Master plugin chain: {error}"
            ))
        })?;
    // Parents are cleared first so an old parent never closes a transient
    // cycle with a new one.
    for nested in [false, true] {
        for (group, value) in fm_model::AudioGroupNumber::ALL
            .into_iter()
            .zip(show.audio_groups())
        {
            let gain =
                InputGainMilliDb::new(value.state.gain_millidb).unwrap_or(InputGainMilliDb::UNITY);
            project
                .set_audio_group(
                    group,
                    fm_model::AudioGroup {
                        name: value.name.clone(),
                        gain,
                        muted: value.state.muted,
                        soloed: value.soloed,
                        parent: value.parent.filter(|_| nested),
                    },
                )
                .map_err(|error| {
                    AppFailure(format!("project cannot take audio group {group}: {error}"))
                })?;
        }
    }
    for (bus, value) in fm_model::AuxBusNumber::ALL
        .into_iter()
        .zip(show.aux_buses())
    {
        project
            .set_aux_bus(
                bus,
                fm_model::AuxBus {
                    name: value.name.clone(),
                    gain: InputGainMilliDb::new(value.state.gain_millidb)
                        .unwrap_or(InputGainMilliDb::UNITY),
                    muted: value.state.muted,
                    dsp: value.dsp,
                },
            )
            .map_err(|error| AppFailure(format!("project cannot take aux {bus}: {error}")))?;
    }
    let outputs = project
        .outputs()
        .iter()
        .map(|output| output.id)
        .collect::<Vec<_>>();
    for output in outputs {
        project.set_output_aux_bus(output, show.output_aux_bus(output));
    }
    project.set_audio_solo_mode(show.audio_solo_mode());
    let monitor = show.audio_monitor();
    project
        .set_audio_monitor_output(fm_model::AudioMonitorOutput {
            device: monitor.device.clone(),
            gain: InputGainMilliDb::new(monitor.state.gain_millidb)
                .unwrap_or(InputGainMilliDb::UNITY),
            muted: monitor.state.muted,
        })
        .map_err(|error| AppFailure(format!("project cannot take the monitor output: {error}")))?;
    let excluded = show.desired_switcher().fade_to_black_excluded_outputs();
    let outputs = project
        .outputs()
        .iter()
        .map(|output| output.id)
        .collect::<Vec<_>>();
    for output in outputs {
        project.set_fade_to_black_output(output, !excluded.contains(&output));
        project.set_fade_to_black_output_duration(
            output,
            show.desired_switcher()
                .fade_to_black_output_duration(output),
        );
    }
    project.set_fade_to_black_curve(show.desired_switcher().fade_to_black_curve());
    Ok(())
}

fn model_audio_strip_state(state: EngineInputAudioStripState) -> InputAudioStripState {
    InputAudioStripState {
        gain: InputGainMilliDb::new(state.gain_millidb).unwrap_or(InputGainMilliDb::UNITY),
        balance: InputBalanceBasisPoints::new(state.balance_basis_points).unwrap_or_default(),
        pan: fm_model::InputPanBasisPoints::new(state.pan_basis_points).unwrap_or_default(),
        muted: state.muted,
        soloed: state.soloed,
        follow_video: state.follow_video,
        delay_samples: InputDelaySamples::new(state.delay_samples).unwrap_or_default(),
        sends: state.sends,
    }
}

#[cfg(test)]
fn configure_client_socket(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(CLIENT_READ_POLL_INTERVAL))?;
    stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn handle_client(
    stream: TcpStream,
    server: &Server<ControlHandle>,
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    authority: &ServerIdentity,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
    once_client_outcome: &mut OnceClientOutcome,
) -> AppResult<()> {
    let handshake_deadline = Instant::now()
        .checked_add(Duration::from_millis(
            server.config().session_limits.heartbeat_timeout_ms,
        ))
        .ok_or_else(|| AppFailure("handshake deadline exceeds Instant range".into()))?;
    configure_client_socket(&stream)?;
    let mut writer = stream.try_clone()?;
    let mut reader = MessageReader::new(stream);
    let mut native = native;
    let first = match read_client_message(
        &mut reader,
        control,
        authority,
        &mut native,
        process_shutdown,
        || Ok(Instant::now() < handshake_deadline),
    )? {
        ClientRead::Message(message) => message,
        ClientRead::Closed | ClientRead::DaemonShutdown(_) => return Ok(()),
    };
    let (hello, handshake_outcome) = if let WireMessage::HandshakeRequest(request) = first {
        current_handshake(&request, control, durable.project().id())
    } else {
        write_error(
            &mut writer,
            "handshake_required",
            "first message must be handshake_request",
        )?;
        return Ok(());
    };

    let handshake = match server.handshake(&hello, principal, now_millis()?) {
        Ok(handshake) => handshake,
        Err(error) => {
            write_message(
                &mut writer,
                &WireMessage::HandshakeResponse(rejected_handshake_response(
                    server,
                    control,
                    durable.project().id(),
                    &hello,
                    handshake_code(&error),
                    &error.to_string(),
                )),
            )?;
            *once_client_outcome = OnceClientOutcome::HandshakeResponseWritten;
            return Ok(());
        }
    };
    let mut session = handshake.session;
    let session_identity = server_identity(&handshake.server_hello, durable.project().id());
    let response = WireMessage::HandshakeResponse(handshake_response(
        &handshake.server_hello,
        session_identity.clone(),
        reconciled_handshake_outcome(handshake_outcome, &handshake.sync),
    ));
    write_session_message(&mut writer, &mut session, &response)?;
    *once_client_outcome = OnceClientOutcome::HandshakeResponseWritten;
    match handshake.sync {
        SyncPayload::Snapshot(snapshot) => {
            write_session_message(&mut writer, &mut session, &WireMessage::Snapshot(*snapshot))?;
            for state in control.borrow().title_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::TitleState(state))?;
            }
            let playlist = control.borrow().playlist_state();
            if let Some(state) = playlist {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::PlaylistState(state),
                )?;
            }
            for state in control.borrow().grade_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::GradeState(state))?;
            }
            for state in control.borrow().title_animation_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::TitleAnimationState(state),
                )?;
            }
            for state in control.borrow().title_layout_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::TitleLayoutState(state),
                )?;
            }
            for state in control.borrow().scene_animation_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::SceneAnimationState(state),
                )?;
            }
            for state in control.borrow().scene_layout_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::SceneLayoutState(state),
                )?;
            }
            for state in control.borrow().animation_preset_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::AnimationPresetState(state),
                )?;
            }
            for state in control.borrow().media_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::MediaState(state))?;
            }
            for state in control.borrow().cue_list_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::CueListState(state),
                )?;
            }
            for state in control.borrow().script_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::ScriptState(state))?;
            }
            for state in control.borrow().shortcut_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::ShortcutState(state),
                )?;
            }
            for state in control.borrow().still_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::StillState(state))?;
            }
            for state in control.borrow().media_list_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::MediaListState(state),
                )?;
            }
            for state in control.borrow().strip_dsp_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::AudioStripDspState(state),
                )?;
            }
            for state in control.borrow().strip_send_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::AudioStripSendState(state),
                )?;
            }
            for state in control.borrow().paged_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::PagedState(state))?;
            }
        }
        SyncPayload::Resume(events) => {
            for event in events {
                write_session_message(&mut writer, &mut session, &WireMessage::Event(event))?;
            }
        }
    }

    loop {
        let message = match read_client_message(
            &mut reader,
            control,
            authority,
            &mut native,
            process_shutdown,
            || session_heartbeat_active(&mut session),
        )? {
            ClientRead::Message(message) => message,
            ClientRead::DaemonShutdown(DaemonShutdownReason::ProcessSignal) => {
                let _ = write_session_message(&mut writer, &mut session, &shutdown_message());
                session.disconnect(DisconnectReason::ServerShutdown);
                break;
            }
            ClientRead::Closed | ClientRead::DaemonShutdown(_) => break,
        };
        match message {
            WireMessage::Command(command) => process_command(
                &mut writer,
                &mut session,
                control,
                store,
                durable,
                principal,
                &session_identity,
                &command,
                native.as_deref_mut(),
                process_shutdown,
            )?,
            WireMessage::Heartbeat(heartbeat) => {
                match record_heartbeat(&mut session, control, &session_identity, &heartbeat) {
                    Ok(received_at_ms) => write_session_message(
                        &mut writer,
                        &mut session,
                        &WireMessage::HeartbeatAcknowledgement(HeartbeatAcknowledgementMessage {
                            server: session_identity.clone(),
                            heartbeat_sequence: heartbeat.sequence,
                            received_at_ms,
                        }),
                    )?,
                    Err(message) => write_session_error(
                        &mut writer,
                        &mut session,
                        "invalid_heartbeat",
                        &message,
                    )?,
                }
            }
            WireMessage::DiagnosticsRequest(request) => {
                let encoded_bytes =
                    encode_line(&WireMessage::DiagnosticsRequest(request.clone()))?.len();
                if let Err(error) =
                    session.admit_diagnostics(&request, encoded_bytes, now_millis()?)
                {
                    let current_revision = control.borrow().diagnostics().current_revision;
                    write_session_message(
                        &mut writer,
                        &mut session,
                        &WireMessage::Error(ErrorMessage {
                            request_id: Some(request.request_id),
                            current_revision: Some(current_revision),
                            error: structured_session_error(&error),
                        }),
                    )?;
                } else {
                    write_session_message(
                        &mut writer,
                        &mut session,
                        &WireMessage::DiagnosticsResponse(diagnostics_response(
                            control,
                            request,
                            native
                                .as_deref()
                                .map_or_else(Vec::new, NativeDaemon::source_sync_samples),
                            fm_protocol::DaemonHealth::default(),
                        )?),
                    )?;
                }
            }
            WireMessage::SlideshowSlidesRequest(request) => {
                let slides = control.borrow().slideshow_slides(&request);
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::SlideshowSlides(slides),
                )?;
            }
            _ => write_session_error(
                &mut writer,
                &mut session,
                "unexpected_message",
                "only command and heartbeat messages are accepted after the handshake",
            )?,
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::large_enum_variant)]
enum ClientRead {
    Message(WireMessage),
    Closed,
    DaemonShutdown(DaemonShutdownReason),
}

#[cfg(test)]
fn read_client_message(
    reader: &mut MessageReader,
    control: &SharedControl,
    server: &ServerIdentity,
    native: &mut Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
    mut idle: impl FnMut() -> AppResult<bool>,
) -> AppResult<ClientRead> {
    if let Some(reason) = requested_daemon_shutdown(native.as_deref(), process_shutdown) {
        return Ok(ClientRead::DaemonShutdown(reason));
    }
    if !idle()? {
        return Ok(ClientRead::Closed);
    }
    let message = reader.read_message_with_idle(|| {
        if requested_daemon_shutdown(native.as_deref(), process_shutdown).is_some() || !idle()? {
            return Ok(false);
        }
        if let Some(native) = native.as_deref_mut() {
            native.tick_if_due(&mut control.borrow_mut(), server)?;
        }
        Ok(true)
    })?;
    if let Some(reason) = requested_daemon_shutdown(native.as_deref(), process_shutdown) {
        Ok(ClientRead::DaemonShutdown(reason))
    } else if message.is_some() && !idle()? {
        Ok(ClientRead::Closed)
    } else {
        Ok(message.map_or(ClientRead::Closed, ClientRead::Message))
    }
}

fn session_heartbeat_active(session: &mut Session) -> AppResult<bool> {
    match session.check_heartbeat(now_millis()?) {
        Ok(()) => Ok(true),
        Err(SessionError::Disconnected(DisconnectReason::HeartbeatTimeout)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn reconciled_handshake_outcome(
    outcome: ProtocolHandshakeOutcome,
    sync: &SyncPayload,
) -> ProtocolHandshakeOutcome {
    if matches!(&outcome, ProtocolHandshakeOutcome::Resume { .. })
        && matches!(sync, SyncPayload::Snapshot(_))
    {
        ProtocolHandshakeOutcome::Snapshot {
            reason: fm_protocol::SnapshotReason::HistoryUnavailable,
        }
    } else {
        outcome
    }
}

fn current_handshake(
    request: &HandshakeRequest,
    control: &SharedControl,
    project_id: ProjectId,
) -> (HandshakeRequest, ProtocolHandshakeOutcome) {
    let diagnostics = control.borrow().diagnostics();
    let server = ServerIdentity {
        engine_id: diagnostics.engine.engine_id.clone(),
        project_id: project_id.to_string(),
        state_epoch: diagnostics.engine.state_epoch,
        log_id: diagnostics.engine.log_id.clone(),
    };
    let available_from_revision = diagnostics
        .oldest_retained_revision
        .unwrap_or_else(|| diagnostics.current_revision.saturating_add(1));
    let outcome = choose_handshake_outcome(
        &server,
        diagnostics.current_revision,
        available_from_revision,
        request.resume_cursor.as_ref(),
    );
    let mut reconciled = request.clone();
    if !matches!(outcome, ProtocolHandshakeOutcome::Resume { .. }) {
        reconciled.resume_cursor = None;
    }
    (reconciled, outcome)
}

fn handshake_response(
    hello: &ServerHello,
    server: ServerIdentity,
    outcome: ProtocolHandshakeOutcome,
) -> HandshakeResponse {
    HandshakeResponse {
        protocol: hello.protocol,
        granted_role: hello.granted_role,
        permissions: hello.permissions.clone(),
        capabilities: CapabilityReportSummary {
            digest: hello.capabilities_digest.clone(),
            total: 0,
            available: 0,
            degraded: 0,
            unavailable: 0,
        },
        server,
        current_revision: hello.current_revision,
        outcome,
    }
}

fn rejected_handshake_response(
    server: &Server<ControlHandle>,
    control: &SharedControl,
    project_id: ProjectId,
    hello: &HandshakeRequest,
    code: &str,
    message: &str,
) -> HandshakeResponse {
    let diagnostics = control.borrow().diagnostics();
    HandshakeResponse {
        protocol: PROTOCOL_VERSION,
        granted_role: hello.desired_role,
        permissions: Vec::new(),
        capabilities: CapabilityReportSummary {
            digest: server.config().capabilities_digest.clone(),
            total: 0,
            available: 0,
            degraded: 0,
            unavailable: 0,
        },
        server: ServerIdentity {
            engine_id: diagnostics.engine.engine_id,
            project_id: project_id.to_string(),
            state_epoch: diagnostics.engine.state_epoch,
            log_id: diagnostics.engine.log_id,
        },
        current_revision: diagnostics.current_revision,
        outcome: ProtocolHandshakeOutcome::Rejected {
            error: StructuredError {
                code: code.into(),
                message: message.into(),
                fields: Vec::new(),
                retryable: false,
            },
        },
    }
}

fn server_identity(hello: &ServerHello, project_id: ProjectId) -> ServerIdentity {
    ServerIdentity {
        engine_id: hello.engine.engine_id.clone(),
        project_id: project_id.to_string(),
        state_epoch: hello.engine.state_epoch,
        log_id: hello.engine.log_id.clone(),
    }
}

fn control_server_identity(
    control: &ControlService<Policy>,
    project_id: ProjectId,
) -> ServerIdentity {
    let engine = control.diagnostics().engine;
    ServerIdentity {
        engine_id: engine.engine_id,
        project_id: project_id.to_string(),
        state_epoch: engine.state_epoch,
        log_id: engine.log_id,
    }
}

fn event_cursor(cursor: &ResumeCursor) -> EventCursor {
    EventCursor {
        engine: fm_protocol::EngineIdentity {
            engine_id: cursor.server.engine_id.clone(),
            state_epoch: cursor.server.state_epoch,
            log_id: cursor.server.log_id.clone(),
        },
        revision: cursor.revision,
    }
}

fn record_heartbeat(
    session: &mut Session,
    control: &SharedControl,
    server: &ServerIdentity,
    heartbeat: &HeartbeatMessage,
) -> Result<u64, String> {
    if heartbeat.server != *server {
        return Err("heartbeat server identity does not match the session".into());
    }
    let last_applied = if let Some(cursor) = &heartbeat.last_applied {
        if cursor.server != *server {
            return Err("heartbeat cursor identity does not match the session".into());
        }
        if cursor.revision > control.borrow().diagnostics().current_revision {
            return Err("heartbeat cursor is ahead of the server revision".into());
        }
        Some(event_cursor(cursor))
    } else {
        None
    };
    let received_at_ms = now_millis().map_err(|error| error.to_string())?;
    session
        .record_heartbeat(
            Heartbeat {
                last_applied,
                clock_sample_ms: heartbeat.sent_at_ms,
            },
            received_at_ms,
        )
        .map_err(|error| error.to_string())?;
    Ok(received_at_ms)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn process_command(
    writer: &mut TcpStream,
    session: &mut Session,
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<()> {
    let delivery = execute_session_command(
        session,
        control,
        store,
        durable,
        principal,
        server,
        command,
        native,
        process_shutdown,
    )?;
    write_session_message(
        writer,
        session,
        &WireMessage::CommandResult(delivery.result),
    )?;
    for event in delivery.events {
        write_session_message(writer, session, &WireMessage::Event(event))?;
    }
    for event in delivery.runtime_events {
        write_session_message(writer, session, &WireMessage::RuntimeEvent(event))?;
    }
    Ok(())
}

struct CommandDelivery {
    result: CommandResult,
    #[cfg(test)]
    events: Vec<EventMessage>,
    #[cfg(test)]
    runtime_events: Vec<RuntimeEventMessage>,
}

impl CommandDelivery {
    fn answer(result: CommandResult) -> Self {
        Self {
            result,
            #[cfg(test)]
            events: Vec::new(),
            #[cfg(test)]
            runtime_events: Vec::new(),
        }
    }
}

/// Admits `command` to `session`'s in-flight and rate budgets, or returns
/// the session's refusal of it. An admitted command holds an in-flight slot
/// until [`Session::command_completed`].
fn admit_session_command(
    session: &mut Session,
    control: &ControlService<Policy>,
    command: &CommandMessage,
    now: u64,
) -> AppResult<Option<CommandResult>> {
    let encoded_bytes = encode_line(&WireMessage::Command(command.clone()))?.len();
    // The session authorizes its scoped role against the classes fm-control
    // resolves now, including the edit an undo or redo would revert or reapply.
    let required = control.required_classes(&command.payload);
    Ok(session
        .admit_command(command, required, encoded_bytes, now)
        .err()
        .map(|error| session_rejection(command, &error, control.diagnostics().current_revision)))
}

/// Whether a session command may join a group commit
/// ([`execute_command_group`]). Commands that write files outside the
/// journal transaction as part of their append (a LUT import) or before it
/// (host media copied into the bundle) are executed on their own, exactly
/// as before group commit, so that their refusals and clean-ups stay those
/// of a single command.
fn group_commit_eligible(command: &CommandMessage) -> bool {
    !matches!(
        command.payload,
        CommandPayload::ImportGradeLut { .. }
            | CommandPayload::InputLifecycle(
                fm_protocol::InputLifecycleOperation::Import { .. }
                    | fm_protocol::InputLifecycleOperation::ReplaceSource {
                        source: fm_protocol::WireInputSource::Media { .. },
                        ..
                    }
            )
    )
}

/// Whether a session command may join a native daemon's group commit: a
/// [`group_commit_eligible`] one that the native runtime follows from the
/// project and engine state the group leaves alone. Stinger edits (which
/// preflight their clips on the native runtime), recorder and stream
/// commands, and cue, script and shortcut fires (which may start or stop
/// recorders and streams) hand work to the native runtime per command, and
/// a recording start's acknowledgement may be held, so they are executed on
/// their own, exactly as before group commit.
fn native_group_eligible(command: &CommandMessage) -> bool {
    group_commit_eligible(command)
        && !is_stinger_mutation(command)
        && recording_command_slot(&command.payload).is_none()
        && !matches!(
            command.payload,
            CommandPayload::StreamStart { .. }
                | CommandPayload::StreamStop { .. }
                | CommandPayload::StreamRetry { .. }
                | CommandPayload::StreamStartAll
                | CommandPayload::StreamStopAll
                | CommandPayload::CueControl { .. }
                | CommandPayload::CueAdvance { .. }
                | CommandPayload::RunScript { .. }
                | CommandPayload::FireShortcut { .. }
        )
}

/// The native runtime's refusal of a group member that leaves `candidate`
/// from `current`, or `None`. The runtime follows the catalogue only once
/// the group is durable, so `budget` (taken before the group) still
/// describes the catalogue of `base`, the project before the group: a member
/// that changes the catalogue is vetted for every change the group made up
/// to it, as one-by-one execution vets each against the catalogue the
/// command before it left. A member that changes no input is never refused.
#[cfg(feature = "native-media")]
fn native_group_vet(
    budget: &native_catalogue::NativeCatalogueBudget,
    base: &StoredProject,
    current: &StoredProject,
    candidate: &StoredProject,
) -> Option<CommandRefusal> {
    if native_catalogue::catalogue_keys(current.project())
        == native_catalogue::catalogue_keys(candidate.project())
    {
        return None;
    }
    budget.refusal(base.project(), candidate.project())
}

/// Executes one member of a native group commit on the group's control
/// copy: as [`execute_native_authority_command`] does (host media it names
/// comes into the bundle first, through `store`, which a group that cannot
/// be made durable discards), but its first frame is not realized here. `frame_due` is set when the command would have
/// realized one, and [`realize_native_group`] realizes it for the whole
/// group once the group is durable.
fn execute_native_group_member(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    member: &GroupMember,
    now: u64,
    vet: Option<&ProjectVet<'_>>,
    frame_due: &mut bool,
) -> AppResult<CommandDelivery> {
    let command = &member.command;
    let current_revision = control.diagnostics().current_revision;
    if let Some(result) = playlist_clock::playlist_command_refusal(command, current_revision)
        .or_else(|| cue_clock::cue_command_refusal(command, current_revision))
    {
        return Ok(CommandDelivery::answer(result));
    }
    let imported = import_host_media(control, store, &member.principal, command, now);
    let command = imported.command.as_ref().unwrap_or(command);
    let execution = execute_durable_command_with_ticks(
        control,
        store,
        durable,
        &member.principal,
        &member.server,
        command,
        now,
        vet,
        Realize::First,
        |_, _| {
            *frame_due = true;
            Ok(Vec::new())
        },
    )?;
    imported.discard_unused(store, durable.project());
    Ok(durable_delivery(execution))
}

/// Realizes what a durable native group commit leaves, on the live service
/// (`control`, which adopted the group) and `durable` (the project after
/// its last member): the frame its members began on, when any of them
/// realizes one (waiting for the pacer's deadline, as a single command
/// does), then the native runtime's sources, titles, documents and stream
/// targets follow the project, as after a single command.
#[cfg_attr(not(feature = "native-media"), allow(unused_variables))]
fn realize_native_group(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &StoredProject,
    native: &mut NativeDaemon,
    server: &ServerIdentity,
    frame_due: bool,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<()> {
    native.invalidate_projection();
    if frame_due {
        // A media list stepped to the item its standby opened switches on
        // this very frame (IN-006).
        #[cfg(feature = "native-media")]
        native.adopt_prefetched_media(control);
        if native
            .wait_and_tick(control, server, process_shutdown)?
            .is_none()
        {
            control.tick_for_shutdown(server)?;
        }
    }
    #[cfg(feature = "native-media")]
    {
        native.titles.observe(durable.project());
        native.titles.observe_templates(&control.title_templates());
        if let Some(pages) = store.documents() {
            native.documents.observe(durable.project(), pages);
        }
        native.sync_input_catalogue(control, durable.project())?;
        native.sync_media_list_prefetch(durable.project());
        native.sync_stream_targets(durable.project(), control.frame_cursor())?;
        native.sync_input_lifecycle(control)?;
    }
    Ok(())
}

/// One client command of a group commit and who sent it.
struct GroupMember {
    principal: Principal,
    server: ServerIdentity,
    command: CommandMessage,
}

/// What a group commit answers one member.
struct GroupOutcome {
    delivery: CommandDelivery,
    /// The session admitted the command, so it holds an in-flight slot the
    /// caller releases.
    admitted: bool,
}

/// Executes several clients' commands as one group commit: one journal
/// append and one disk sync for all of them, where executing them one by
/// one costs one each.
///
/// The members are executed in order, exactly as one-by-one execution
/// would (each sees the state the previous one left), but on a speculative
/// copy of the control service ([`ControlService::speculate`]) that
/// publishes nothing, and their journal records are collected instead of
/// appended ([`GroupRecorder`]). Then:
///
/// - the whole group is appended in one transaction
///   ([`DurableStore::record_group`]); only once it is durable does the
///   copy replace the live service ([`ControlService::adopt`]), which then
///   publishes the members' events in order, and are the members answered;
/// - if the append fails, the copy is dropped: no member applied, nothing
///   was published, no receipt was kept, and every admitted member is
///   refused as not durable (retryable), as a single command whose append
///   failed is. Members refused or answered from a receipt the group made
///   are refused the same way, since their answers were decided against
///   the group's unrecorded state. A member answered from the receipt of a
///   command durable before the group keeps that answer (it touched no
///   journal), and a member the session refused keeps its refusal, at the
///   live revision.
///
/// On a native daemon (`native`), the members ([`native_group_eligible`]
/// ones) all begin on the frame the pacer realizes next: none realizes a
/// frame of its own, so each is prepared on the state the one before it
/// left, on the same frame cursor, and journaled with that frame, which is
/// exactly what replay reproduces (it ticks no idle frame between records
/// that began on one frame). Once the group is durable and adopted, that
/// frame is realized once for all of them, as a single command's is
/// ([`realize_native_group`]), before anyone is answered. A group that could
/// not be made durable realizes nothing; the pacer realizes the frame idle.
///
/// A refused command is never journaled: its receipt rides the next
/// checkpoint, as for one command. `admit` runs before each member on the
/// copy (catch-up and session admission) and returns the session's refusal
/// of a member it did not admit. An error executing any member stops the
/// daemon, as it does for one command; no member of the group was made
/// durable or answered then.
#[allow(clippy::too_many_lines)]
#[cfg_attr(not(feature = "native-media"), allow(unused_mut))]
fn execute_command_group(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    members: &[GroupMember],
    mut native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
    mut admit: impl FnMut(
        usize,
        &mut ControlService<Policy>,
        &StoredProject,
        u64,
    ) -> AppResult<Option<CommandResult>>,
) -> AppResult<Vec<GroupOutcome>> {
    let speculation: SharedControl = Rc::new(RefCell::new(control.borrow().speculate()));
    let recorder = GroupRecorder::new(store);
    let mut staged = durable.clone();
    let mut outcomes = Vec::with_capacity(members.len());
    // A native group's members are vetted against the catalogue the native
    // runtime holds now, before any of them: it follows their changes only
    // once the group is durable (see `native_group_vet`).
    #[cfg(feature = "native-media")]
    let budget = native.as_deref_mut().map(NativeDaemon::catalogue_budget);
    #[cfg(feature = "native-media")]
    let base = native.is_some().then(|| durable.clone());
    #[cfg(feature = "native-media")]
    let vet = |current: &StoredProject, candidate: &StoredProject| {
        native_group_vet(budget.as_ref()?, base.as_ref()?, current, candidate)
    };
    #[cfg(feature = "native-media")]
    let vet: Option<&ProjectVet<'_>> = Some(&vet);
    #[cfg(not(feature = "native-media"))]
    let vet: Option<&ProjectVet<'_>> = None;
    // Whether a native member's first frame is due: the group realizes it
    // once, after its append (see `realize_native_group`).
    let mut frame_due = false;
    for (index, member) in members.iter().enumerate() {
        let now = now_millis()?;
        if let Some(result) = admit(index, &mut speculation.borrow_mut(), &staged, now)? {
            outcomes.push(GroupOutcome {
                delivery: CommandDelivery::answer(result),
                admitted: false,
            });
            continue;
        }
        let delivery = if native.is_some() {
            execute_native_group_member(
                &mut speculation.borrow_mut(),
                &recorder,
                &mut staged,
                member,
                now,
                vet,
                &mut frame_due,
            )?
        } else {
            execute_admitted_command(
                &speculation,
                &recorder,
                &mut staged,
                &member.principal,
                &member.server,
                &member.command,
                now,
                None,
                process_shutdown,
            )?
        };
        outcomes.push(GroupOutcome {
            delivery,
            admitted: true,
        });
    }
    let records = recorder.records.take();
    let durability = if records.is_empty() {
        Ok(())
    } else {
        store.record_group(durable, &records, &staged)
    };
    match durability {
        Ok(()) => {
            let speculation = Rc::try_unwrap(speculation)
                .map_err(|_| AppFailure("the group commit's control copy is still shared".into()))?
                .into_inner();
            let _ = control.borrow_mut().adopt(speculation);
            *durable = staged;
            if let (Some(native), Some(member)) = (native, members.first()) {
                realize_native_group(
                    &mut control.borrow_mut(),
                    store,
                    durable,
                    native,
                    &member.server,
                    frame_due,
                    process_shutdown,
                )?;
            }
        }
        Err(error) => {
            drop(speculation);
            recorder.discard()?;
            let current_revision = control.borrow().diagnostics().current_revision;
            let base_revision = durable.position().revision;
            let attempted = staged.position().revision;
            for (outcome, member) in outcomes.iter_mut().zip(members) {
                match &mut outcome.delivery.result {
                    // Answered from the receipt of a command durable before
                    // the group: exactly the answer it gets alone, which
                    // touches no journal. Denying it now would contradict
                    // the acknowledgement it already had.
                    CommandResult::Accepted { revision, .. } if *revision <= base_revision => {
                        continue;
                    }
                    // A session's refusal stands, but at the revision the
                    // daemon is really at, not one the dropped group reached.
                    CommandResult::Rejected {
                        current_revision: reported,
                        ..
                    } if !outcome.admitted => {
                        *reported = current_revision;
                    }
                    _ => {}
                }
                if outcome.admitted {
                    eprintln!(
                        "FREEMIXD_DURABILITY\tv=1\tcommand={}\trevision={attempted}\terror={error}",
                        member.command.id
                    );
                    outcome.delivery = CommandDelivery::answer(
                        refused_submission(&member.command, current_revision, error.as_ref())
                            .output
                            .result,
                    );
                }
            }
        }
    }
    Ok(outcomes)
}

/// The store a group commit's members execute against: it collects each
/// member's journal record for [`DurableStore::record_group`] instead of
/// appending it, and hands everything else (staging template animations,
/// importing media) to the daemon's store, remembering what it wrote so a
/// group that cannot be made durable removes it again.
struct GroupRecorder<'a> {
    store: &'a dyn DurableStore,
    records: RefCell<Vec<journal::GroupRecord>>,
    staged: RefCell<Vec<(u64, Vec<String>)>>,
    imported: RefCell<Vec<String>>,
}

impl<'a> GroupRecorder<'a> {
    fn new(store: &'a dyn DurableStore) -> Self {
        Self {
            store,
            records: RefCell::new(Vec::new()),
            staged: RefCell::new(Vec::new()),
            imported: RefCell::new(Vec::new()),
        }
    }

    /// Removes what the group staged and imported, as each member would
    /// have on its own failed append.
    fn discard(&self) -> AppResult<()> {
        for asset in self.imported.take() {
            if let Err(error) = self.store.discard_imported_media(&asset) {
                eprintln!("FREEMIXD_IMPORT\tv=1\tasset={asset}\terror={error}");
            }
        }
        for (revision, uris) in self.staged.take() {
            self.store
                .discard_staged_title_animations(revision, &uris)?;
        }
        Ok(())
    }
}

impl DurableStore for GroupRecorder<'_> {
    fn record(
        &self,
        command: &CommandMessage,
        now_millis: u64,
        began_frame: u64,
        previous: &StoredProject,
        updated: &StoredProject,
    ) -> AppResult<()> {
        self.records.borrow_mut().push(journal::GroupRecord {
            command: command.clone(),
            now_millis,
            began_frame,
            base_revision: previous.position().revision,
            position: updated.position(),
        });
        Ok(())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        Err(AppFailure("a group commit checkpoints only when it is recorded".into()).into())
    }

    fn busy(&self) -> bool {
        self.store.busy()
    }

    fn documents(&self) -> Option<&Arc<documents::DocumentPages>> {
        self.store.documents()
    }

    fn stage_title_animations(
        &self,
        revision: u64,
        animations: &[(String, journal::StagedTemplate)],
    ) -> AppResult<()> {
        if animations.is_empty() {
            return Ok(());
        }
        self.staged.borrow_mut().push((
            revision,
            animations.iter().map(|(uri, _)| uri.clone()).collect(),
        ));
        self.store.stage_title_animations(revision, animations)
    }

    fn discard_staged_title_animations(&self, revision: u64, uris: &[String]) -> AppResult<()> {
        self.staged
            .borrow_mut()
            .retain(|(staged, _)| *staged != revision);
        self.store.discard_staged_title_animations(revision, uris)
    }

    fn recent_append_peak(&self) -> Option<Duration> {
        self.store.recent_append_peak()
    }

    fn import_media_path(&self, path: &Path) -> AppResult<Option<(String, bool)>> {
        let imported = self.store.import_media_path(path)?;
        if let Some((asset, true)) = &imported {
            self.imported.borrow_mut().push(asset.clone());
        }
        Ok(imported)
    }

    fn discard_imported_media(&self, asset_uri: &str) -> AppResult<()> {
        self.imported
            .borrow_mut()
            .retain(|asset| asset != asset_uri);
        self.store.discard_imported_media(asset_uri)
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_session_command(
    session: &mut Session,
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<CommandDelivery> {
    // A live fire's Preview and Program are the authority's to bind
    // (RC-004); only the console's selection comes from the client.
    let stripped =
        fm_control::client_fire_payload(&command.payload).map(|payload| CommandMessage {
            payload,
            ..command.clone()
        });
    let command = stripped.as_ref().unwrap_or(command);
    let now = now_millis()?;
    if let Some(result) = admit_session_command(session, &control.borrow(), command, now)? {
        return Ok(CommandDelivery::answer(result));
    }
    // Admission reserved an in-flight slot; it is released whether execution
    // succeeds, is refused, or fails, so no exit path can leak the slot.
    let delivery = execute_admitted_command(
        control,
        store,
        durable,
        principal,
        server,
        command,
        now,
        native,
        process_shutdown,
    );
    let completed = session.command_completed();
    let delivery = delivery?;
    completed?;
    Ok(delivery)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]
fn execute_admitted_command(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now: u64,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<CommandDelivery> {
    let current_revision = control.borrow().diagnostics().current_revision;
    let refusal = playlist_clock::playlist_command_refusal(command, current_revision)
        .or_else(|| cue_clock::cue_command_refusal(command, current_revision));
    if let Some(result) = refusal {
        return Ok(CommandDelivery {
            result,
            #[cfg(test)]
            events: Vec::new(),
            #[cfg(test)]
            runtime_events: Vec::new(),
        });
    }
    execute_authority_command(
        control,
        store,
        durable,
        principal,
        server,
        command,
        now,
        native,
        process_shutdown,
    )
}

/// Executes one command a client may send, or one the daemon itself issues
/// (the playlist clock's `playlist_advance`), through the simulated or native
/// realization path.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]
fn execute_authority_command(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now: u64,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<CommandDelivery> {
    let mut control = control.borrow_mut();
    if let Some(native) = native {
        return execute_native_authority_command(
            &mut control,
            store,
            durable,
            principal,
            server,
            command,
            now,
            native,
            process_shutdown,
        );
    }
    let imported = import_host_media(&mut control, store, principal, command, now);
    let command = imported.command.as_ref().unwrap_or(command);
    // As on the native daemon, the command realizes only its first frame
    // and is answered then; the session loop's simulated pacer realizes
    // the rest of a timed transition or title phase at the show's frame
    // rate (see `sessions::SimulatedPacer`). A clock's advance first
    // realizes the idle frames wall time has already played, so its first
    // frame is the one it is due on.
    catch_up_simulated_advance(&mut control, server, &command.payload)?;
    let execution = execute_durable_command_with_ticks(
        &mut control,
        store,
        durable,
        principal,
        server,
        command,
        now,
        None,
        Realize::First,
        |control, server| Ok(control.tick(server)?.runtime_events),
    )?;
    imported.discard_unused(store, durable.project());
    // Imported, duplicated, replaced and reopened media inputs play from a
    // synthetic source as soon as they exist (IN-026).
    attach_synthetic_media(&mut control, durable.project())?;
    Ok(durable_delivery(execution))
}

/// Simulated daemon only: before a clock's `playlist_advance` or
/// `cue_advance` for `frame` executes, ticks the engine idle until `frame`
/// is the next frame it renders. The simulated engine renders no idle frames, so its cursor
/// trails the show's wall-clock frame the clock fired at; those frames were
/// played already and are realized at once (never onto another pending
/// boundary or waiting cue), and the advance's own first frame is `frame`.
/// Its journal record names the frame it was committed at, so replay ticks
/// the same frames.
fn catch_up_simulated_advance(
    control: &mut ControlService<Policy>,
    server: &ServerIdentity,
    payload: &CommandPayload,
) -> AppResult<()> {
    let (CommandPayload::PlaylistAdvance { frame } | CommandPayload::CueAdvance { frame, .. }) =
        *payload
    else {
        return Ok(());
    };
    // The cursor is the next frame to render: it stops on `frame`, and on
    // any other boundary or cue frame before it, unrendered.
    let mut target = frame;
    if let Some(boundary) = control.playlist_advance_frame() {
        target = target.min(boundary);
    }
    if let Some((_, due)) = control.cue_advance_frame() {
        target = target.min(due);
    }
    while control.frame_cursor() < target {
        control.tick(server)?;
    }
    Ok(())
}

/// Executes one command through the native realization path, with the
/// control service already borrowed: from the session loop, and from frame
/// loops that run before it (the playlist clock during the recorder startup
/// barrier).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]
fn execute_native_authority_command(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now: u64,
    native: &mut NativeDaemon,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<CommandDelivery> {
    // Live catalogue changes (IN-026): host media an import or a source
    // replacement names comes into the bundle first, and the native
    // runtime vets the catalogue a command would leave before it is staged.
    let imported = import_host_media(control, store, principal, command, now);
    let command = imported.command.as_ref().unwrap_or(command);
    #[cfg(feature = "native-media")]
    let budget = native.catalogue_budget();
    #[cfg(feature = "native-media")]
    let vet = move |current: &StoredProject, candidate: &StoredProject| {
        budget.refusal(current.project(), candidate.project())
    };
    #[cfg(feature = "native-media")]
    let vet: Option<&ProjectVet<'_>> = Some(&vet);
    #[cfg(not(feature = "native-media"))]
    let vet: Option<&ProjectVet<'_>> = None;
    native.invalidate_projection();
    let execution = if is_stinger_mutation(command) {
        match execute_native_stinger_mutation(
            control,
            store,
            durable,
            principal,
            server,
            command,
            now,
            native,
            process_shutdown,
        )? {
            Ok(execution) => execution,
            Err(failure) => {
                return Ok(CommandDelivery {
                    result: failure.result,
                    #[cfg(test)]
                    events: Vec::new(),
                    #[cfg(test)]
                    runtime_events: failure.runtime_events,
                });
            }
        }
    } else {
        execute_durable_command_with_ticks(
            control,
            store,
            durable,
            principal,
            server,
            command,
            now,
            vet,
            Realize::First,
            |control, server| {
                // A media list stepped to the item its standby opened
                // switches on this very frame (IN-006).
                #[cfg(feature = "native-media")]
                native.adopt_prefetched_media(control);
                native
                    .wait_and_tick(control, server, process_shutdown)?
                    .map_or_else(|| Ok(control.tick_for_shutdown(server)?.runtime_events), Ok)
            },
        )?
    };
    // Live sink changes and recording segments are realized by the
    // native runtime right here, after durability and the engine
    // commit have settled but before the acknowledgement is queued.
    // A recording start only begins here; the session loop holds its
    // acknowledgement until the segment is mux-ready.
    #[cfg(feature = "native-media")]
    native.observe_stream_command(command, &execution.submission.output.result);
    #[cfg(feature = "native-media")]
    native.observe_recording_command(
        command,
        &execution.submission.output.result,
        durable,
        control.frame_cursor(),
    );
    // A cue's record and stream actions start and stop the same workers
    // (RC-017); its acknowledgement is not held for a segment's readiness.
    #[cfg(feature = "native-media")]
    for worker in cue_worker_commands(command, &execution.submission) {
        native.observe_stream_command(&worker, &execution.submission.output.result);
        native.observe_recording_command(
            &worker,
            &execution.submission.output.result,
            durable,
            control.frame_cursor(),
        );
    }
    // Changed title fields re-render on the title worker; this only
    // compares and hands over state.
    #[cfg(feature = "native-media")]
    native.titles.observe(durable.project());
    #[cfg(feature = "native-media")]
    native.titles.observe_templates(&control.title_templates());
    // A document input's page renders on the page worker (IN-014).
    #[cfg(feature = "native-media")]
    if let Some(pages) = store.documents() {
        native.documents.observe(durable.project(), pages);
    }
    #[cfg(feature = "native-media")]
    native.sync_input_catalogue(control, durable.project())?;
    #[cfg(feature = "native-media")]
    native.sync_media_list_prefetch(durable.project());
    // Destination edits (OR-006) rebuild only the edited destination.
    #[cfg(feature = "native-media")]
    native.sync_stream_targets(durable.project(), control.frame_cursor())?;
    #[cfg(feature = "native-media")]
    native.sync_input_lifecycle(control)?;
    imported.discard_unused(store, durable.project());
    Ok(durable_delivery(execution))
}

#[allow(clippy::used_underscore_binding)]
fn durable_delivery(execution: DurableExecution) -> CommandDelivery {
    let DurableExecution {
        submission,
        runtime_events: _runtime_events,
    } = execution;
    CommandDelivery {
        result: submission.output.result,
        #[cfg(test)]
        events: submission.output.events,
        #[cfg(test)]
        runtime_events: _runtime_events,
    }
}

fn is_stinger_mutation(command: &CommandMessage) -> bool {
    matches!(
        command.payload,
        CommandPayload::ConfigureStinger { .. } | CommandPayload::RemoveStinger { .. }
    )
}

#[cfg(feature = "native-media")]
fn native_stinger_preflight_rejection(
    command: &CommandMessage,
    current_revision: u64,
) -> CommandResult {
    CommandResult::Rejected {
        id: command.id.clone(),
        code: RejectionCode::Unavailable.as_str().to_owned(),
        message: "native Stinger resources could not be prepared".to_owned(),
        fields: Vec::new(),
        current_revision,
        retryable: false,
        reason: None,
    }
}

/// The host media files one batch import or source replacement brought
/// into the bundle (IN-026): the command to execute with their `asset://`
/// URIs, and the assets newly written for it.
#[derive(Default)]
struct HostMediaImport {
    /// The rewritten command, or `None` to execute the command as sent.
    command: Option<CommandMessage>,
    /// Assets this import wrote (not ones it found already in the bundle).
    copied: Vec<String>,
}

impl HostMediaImport {
    /// Removes every asset this import wrote that `project` (the project
    /// after the command) does not name: the command was refused, or an
    /// item it was copied for was. A copy that cannot be removed is only
    /// reported; it is an unreferenced file, never a broken project.
    fn discard_unused(&self, store: &dyn DurableStore, project: &fm_model::Project) {
        for uri in &self.copied {
            let used = project.inputs().iter().any(|input| match &input.kind {
                fm_model::InputKind::Media { asset_uri, .. }
                | fm_model::InputKind::Slideshow(fm_types::still::SlideshowConfig {
                    source: fm_types::still::SlideshowSource::Folder { asset_uri },
                    ..
                }) => asset_uri == uri,
                kind => kind.file_asset_uris().contains(&uri.as_str()),
            }) || project
                .media_lists()
                .values()
                .any(|list| list.items.iter().any(|item| item.asset_uri == *uri));
            if !used && let Err(error) = store.discard_imported_media(uri) {
                eprintln!("FREEMIXD_IMPORT\tv=1\tasset={uri}\terror={error}");
            }
        }
    }
}

/// The host path `uri` names when a batch import or a source replacement
/// should bring it into the bundle: an absolute path to a regular media or
/// picture file.
fn importable_host_path(uri: &str) -> Option<&Path> {
    let path = Path::new(uri);
    (!uri.starts_with(fm_model::MEDIA_ASSET_URI_PREFIX)
        && path.is_absolute()
        && (fm_model::is_importable_file_name(uri) || fm_types::still::is_image_file_name(uri))
        && path.is_file())
    .then_some(path)
}

/// The host folder a folder slideshow names when it should be brought into
/// the bundle: an absolute path to a folder (IN-009).
fn importable_host_folder(uri: &str) -> Option<&Path> {
    let path = Path::new(uri);
    (!uri.starts_with(fm_model::MEDIA_ASSET_URI_PREFIX) && path.is_absolute() && path.is_dir())
        .then_some(path)
}

/// The media URIs of a batch import (every item), a media, image, or
/// slideshow source replacement, or an image or slideshow input addition,
/// mutably, each with whether it names a folder.
fn host_media_uris(payload: &mut CommandPayload) -> Vec<(&mut String, bool)> {
    match payload {
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::Import { items }) => {
            items
                .iter_mut()
                .map(|item| (&mut item.uri, false))
                .collect()
        }
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::ReplaceSource {
            source: fm_protocol::WireInputSource::Media { uri },
            ..
        }) => vec![(uri, false)],
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::ReplaceSource {
            source: fm_protocol::WireInputSource::Image(image),
            ..
        }) => vec![(&mut image.asset_uri, false)],
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::ReplaceSource {
            source: fm_protocol::WireInputSource::Slideshow(slideshow),
            ..
        }) => match &mut slideshow.source {
            fm_types::still::SlideshowSource::Folder { asset_uri } => vec![(asset_uri, true)],
            fm_types::still::SlideshowSource::List(items) => items
                .iter_mut()
                .map(|item| (&mut item.asset_uri, false))
                .collect(),
        },
        CommandPayload::AddInput {
            source: fm_protocol::LiveInputSource::Image(image),
            ..
        } => vec![(&mut image.asset_uri, false)],
        CommandPayload::AddInput {
            source: fm_protocol::LiveInputSource::Slideshow(slideshow),
            ..
        } => match &mut slideshow.source {
            fm_types::still::SlideshowSource::Folder { asset_uri } => vec![(asset_uri, true)],
            fm_types::still::SlideshowSource::List(items) => items
                .iter_mut()
                .map(|item| (&mut item.asset_uri, false))
                .collect(),
        },
        // Every item of a media list being created or edited (IN-006).
        CommandPayload::MediaList {
            operation:
                fm_protocol::MediaListOperation::Create { items, .. }
                | fm_protocol::MediaListOperation::Set { items, .. },
            ..
        } => items
            .iter_mut()
            .map(|item| (&mut item.uri, false))
            .collect(),
        _ => Vec::new(),
    }
}

/// Brings the host media files a batch import or a source replacement names
/// into the bundle (IN-026): each absolute host path to a regular media file
/// becomes the `asset://` URI of its imported copy, and the command is
/// executed (and journaled) with those URIs. A path that cannot be imported
/// is left as it is, and the engine refuses that item as unavailable.
///
/// Nothing is copied for a command the engine would refuse, nor for an
/// import item it would refuse: the command is first prepared (and
/// abandoned) with each host path standing in as the URI its import tries
/// first. A refused command then executes with those stand-ins, so it is
/// refused exactly as prepared, with its own reason. A stand-in can only
/// differ from the final URI by a numeric suffix, so the prepared refusals
/// are the executed ones (a stand-in shared by two items is refused as a
/// duplicate source, so such an item is copied and the engine decides). The
/// caller discards whatever the executed command does not keep
/// ([`HostMediaImport::discard_unused`]).
///
/// Copying runs on the command loop, like every other durable write before
/// the journal append: the command is journaled with its final URIs, so it
/// cannot be acknowledged before its files are in the bundle. Only files the
/// command keeps are copied.
fn import_host_media(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    principal: &Principal,
    command: &CommandMessage,
    now_millis: u64,
) -> HostMediaImport {
    let mut provisional = command.clone();
    let mut hosts = Vec::new();
    for (index, (uri, folder)) in host_media_uris(&mut provisional.payload)
        .into_iter()
        .enumerate()
    {
        let path = if folder {
            importable_host_folder(uri)
        } else {
            importable_host_path(uri)
        };
        if let Some(path) = path {
            hosts.push((index, path.to_path_buf(), folder));
            *uri = fm_persistence::media_import_asset_uri(path);
        }
    }
    if hosts.is_empty() {
        return HostMediaImport::default();
    }
    // Which of the importable items the engine would keep; `None` when it
    // cannot tell (copy them all, as the executed command decides).
    let kept: Option<Vec<usize>> =
        match control.prepare_submit(principal, provisional.clone(), now_millis) {
            Ok(PrepareSubmitOutcome::Replayed(_)) => {
                // A replay answers from its receipt whatever the payload.
                return HostMediaImport::default();
            }
            Ok(PrepareSubmitOutcome::Prepared(prepared))
                if !prepared.submission().is_accepted() =>
            {
                drop(prepared);
                return HostMediaImport {
                    command: Some(provisional),
                    copied: Vec::new(),
                };
            }
            Ok(PrepareSubmitOutcome::Prepared(prepared)) => {
                let refused: Vec<usize> = prepared
                    .output()
                    .events
                    .iter()
                    .filter_map(|event| match &event.payload {
                        fm_protocol::EventPayload::InputLifecycleChanged { refused, .. } => {
                            Some(refused)
                        }
                        _ => None,
                    })
                    .flatten()
                    .filter(|(_, refusal)| *refusal != fm_protocol::ImportRefusal::DuplicateSource)
                    .map(|(index, _)| usize::from(*index))
                    .collect();
                Some(
                    hosts
                        .iter()
                        .map(|(index, _, _)| *index)
                        .filter(|index| !refused.contains(index))
                        .collect(),
                )
            }
            Err(_) => None,
        };
    let mut rewritten = provisional;
    let mut copied = Vec::new();
    let mut uris = host_media_uris(&mut rewritten.payload);
    for (index, path, folder) in hosts {
        if kept.as_ref().is_some_and(|kept| !kept.contains(&index)) {
            // Refused as prepared; the stand-in keeps its reason.
            continue;
        }
        let uri = &mut *uris[index].0;
        if folder {
            // A folder is always copied anew, so the bundle folder it
            // becomes is the slideshow's own to follow.
            match store.import_picture_folder_path(&path) {
                Ok(Some(asset)) => {
                    copied.push(asset.clone());
                    *uri = asset;
                }
                Ok(None) | Err(_) => path.to_str().unwrap_or_default().clone_into(uri),
            }
            continue;
        }
        match store.import_media_path(&path) {
            Ok(Some((asset, wrote))) => {
                if wrote {
                    copied.push(asset.clone());
                }
                *uri = asset;
            }
            // Not imported: the host path makes the engine refuse the item
            // as unavailable.
            Ok(None) | Err(_) => path.to_str().unwrap_or_default().clone_into(uri),
        }
    }
    HostMediaImport {
        command: Some(rewritten),
        copied,
    }
}

struct NativeMutationFailure {
    result: CommandResult,
    #[cfg(test)]
    runtime_events: Vec<RuntimeEventMessage>,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
#[cfg(feature = "native-media")]
fn execute_native_stinger_mutation(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now_millis: u64,
    native: &mut NativeDaemon,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<Result<DurableExecution, NativeMutationFailure>> {
    // Every step before a `record` only stages; failures there refuse the
    // command as `staging_refusal` classifies them (dropping the staged
    // native resources), failures from a commit on are fatal.
    let current_revision = control.diagnostics().current_revision;
    let began_frame = control.frame_cursor();
    let first_preparation = match control.prepare_submit(principal, command.clone(), now_millis) {
        Ok(preparation) => preparation,
        Err(error) => {
            return refuse_staged_command(command, current_revision, error.into(), Vec::new())
                .map(Ok);
        }
    };
    let first_prepared = match first_preparation {
        PrepareSubmitOutcome::Replayed(submission) => {
            return Ok(Ok(DurableExecution {
                submission,
                runtime_events: Vec::new(),
            }));
        }
        PrepareSubmitOutcome::Prepared(prepared) => prepared,
    };
    if !first_prepared.submission().is_accepted() {
        let staged = first_prepared
            .project_settled(0)
            .1
            .map_err(Into::into)
            .and_then(|projected| {
                stored_project_from_snapshot(
                    durable,
                    &projected,
                    command,
                    command,
                    &first_prepared.output().result,
                )
            });
        let updated = match staged {
            Ok(updated) => updated,
            Err(error) => {
                drop(first_prepared);
                return refuse_staged_command(command, current_revision, error, Vec::new()).map(Ok);
            }
        };
        // A durability failure refuses the command; it does not take the daemon
        // down. Dropping the preparation abandons it without touching the
        // engine, exactly as the simulated path does.
        if let Err(error) = store.record(command, now_millis, began_frame, durable, &updated) {
            drop(first_prepared);
            return Ok(Ok(DurableExecution {
                submission: refuse_undurable(
                    command,
                    control.diagnostics().current_revision,
                    updated.position().revision,
                    error.as_ref(),
                ),
                runtime_events: Vec::new(),
            }));
        }
        let submission = first_prepared.commit()?;
        *durable = updated;
        return Ok(Ok(DurableExecution {
            submission,
            runtime_events: Vec::new(),
        }));
    }

    let staged = first_prepared
        .project_settled(1)
        .1
        .map_err(Into::into)
        .and_then(|projected| {
            stored_project_from_snapshot(
                durable,
                &projected,
                command,
                command,
                &first_prepared.output().result,
            )
        });
    drop(first_prepared);
    let candidate = match staged {
        Ok(candidate) => candidate,
        Err(error) => {
            return refuse_staged_command(command, current_revision, error, Vec::new()).map(Ok);
        }
    };

    let (mutation, mut preflight_runtime_events) = match native
        .preflight_stinger_mutation_with_ticks(
            candidate.clone(),
            control,
            server,
            process_shutdown,
        )? {
        Some((mutation, events)) => (mutation, events),
        None => (Err(()), Vec::new()),
    };
    let Ok(mutation) = mutation else {
        return Ok(Err(NativeMutationFailure {
            result: native_stinger_preflight_rejection(
                command,
                control.diagnostics().current_revision,
            ),
            #[cfg(test)]
            runtime_events: preflight_runtime_events,
        }));
    };

    // The preflight realized frames: the command commits on this one.
    let began_frame = control.frame_cursor();
    let second_preparation = match control.prepare_submit(principal, command.clone(), now_millis) {
        Ok(preparation) => preparation,
        Err(error) => {
            native.stinger_retirements.discard(mutation)?;
            return refuse_staged_command(
                command,
                current_revision,
                error.into(),
                preflight_runtime_events,
            )
            .map(Ok);
        }
    };
    let second_prepared = match second_preparation {
        PrepareSubmitOutcome::Replayed(submission) => {
            native.stinger_retirements.discard(mutation)?;
            return Ok(Ok(DurableExecution {
                submission,
                runtime_events: preflight_runtime_events,
            }));
        }
        PrepareSubmitOutcome::Prepared(prepared) => prepared,
    };
    if !second_prepared.submission().is_accepted() {
        native.stinger_retirements.discard(mutation)?;
        let staged = second_prepared
            .project_settled(0)
            .1
            .map_err(Into::into)
            .and_then(|projected| {
                stored_project_from_snapshot(
                    durable,
                    &projected,
                    command,
                    command,
                    &second_prepared.output().result,
                )
            });
        let updated = match staged {
            Ok(updated) => updated,
            Err(error) => {
                drop(second_prepared);
                return refuse_staged_command(
                    command,
                    current_revision,
                    error,
                    preflight_runtime_events,
                )
                .map(Ok);
            }
        };
        if let Err(error) = store.record(command, now_millis, began_frame, durable, &updated) {
            drop(second_prepared);
            return Ok(Ok(DurableExecution {
                submission: refuse_undurable(
                    command,
                    control.diagnostics().current_revision,
                    updated.position().revision,
                    error.as_ref(),
                ),
                runtime_events: preflight_runtime_events,
            }));
        }
        let submission = second_prepared.commit()?;
        *durable = updated;
        return Ok(Ok(DurableExecution {
            submission,
            runtime_events: preflight_runtime_events,
        }));
    }

    let staged = second_prepared
        .project_settled(1)
        .1
        .map_err(Into::into)
        .and_then(|projected| {
            stored_project_from_snapshot(
                durable,
                &projected,
                command,
                command,
                &second_prepared.output().result,
            )
        });
    let updated = match staged {
        Ok(updated) => updated,
        Err(error) => {
            drop(second_prepared);
            native.stinger_retirements.discard(mutation)?;
            return refuse_staged_command(
                command,
                current_revision,
                error,
                preflight_runtime_events,
            )
            .map(Ok);
        }
    };
    let retained = native
        .video
        .validate_retained_byte_limit(mutation.ordinary_video_limit)
        .unwrap_or(false);
    if candidate.project().stingers() != updated.project().stingers() || !retained {
        drop(second_prepared);
        native.stinger_retirements.discard(mutation)?;
        return Ok(Err(NativeMutationFailure {
            result: native_stinger_preflight_rejection(
                command,
                control.diagnostics().current_revision,
            ),
            #[cfg(test)]
            runtime_events: preflight_runtime_events,
        }));
    }
    if let Err(error) = store.record(command, now_millis, began_frame, durable, &updated) {
        // The staged native resources are released with the preparation: the
        // command was never applied, so retrying it prepares them again.
        drop(second_prepared);
        native.stinger_retirements.discard(mutation)?;
        return Ok(Ok(DurableExecution {
            submission: refuse_undurable(
                command,
                control.diagnostics().current_revision,
                updated.position().revision,
                error.as_ref(),
            ),
            runtime_events: preflight_runtime_events,
        }));
    }
    let submission = second_prepared.commit()?;
    // The preflight's ticks projected the next frame between realizations;
    // the command just committed realizes on it, so that projection is
    // stale (as it is after any other command, which invalidates before it
    // executes).
    native.invalidate_projection();
    native.stage_stinger_mutation(mutation);
    let runtime_events = match native.wait_and_tick(control, server, process_shutdown)? {
        Some(events) => events,
        None => control.tick_for_shutdown(server)?.runtime_events,
    };
    preflight_runtime_events.extend(runtime_events);
    *durable = updated;
    Ok(Ok(DurableExecution {
        submission,
        runtime_events: preflight_runtime_events,
    }))
}

#[allow(clippy::too_many_arguments)]
#[cfg(not(feature = "native-media"))]
fn execute_native_stinger_mutation(
    _control: &mut ControlService<Policy>,
    _store: &dyn DurableStore,
    _durable: &mut StoredProject,
    _principal: &Principal,
    _server: &ServerIdentity,
    _command: &CommandMessage,
    _now_millis: u64,
    _native: &mut NativeDaemon,
    _process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<Result<DurableExecution, NativeMutationFailure>> {
    Err(AppFailure("native-media support was not compiled in".into()).into())
}

struct DurableExecution {
    submission: CommandSubmission,
    runtime_events: Vec<RuntimeEventMessage>,
}

/// Refuses a command that could not be made durable, and says so.
///
/// Every native command path answers a durability failure the same way,
/// because it is the same failure: a full disk during a show must cost one
/// refused command the operator can retry, never the daemon.
#[cfg(feature = "native-media")]
fn refuse_undurable(
    command: &CommandMessage,
    current_revision: u64,
    attempted_revision: u64,
    error: &(dyn Error + 'static),
) -> CommandSubmission {
    eprintln!(
        "FREEMIXD_DURABILITY\tv=1\tcommand={}\trevision={attempted_revision}\terror={error}",
        command.id
    );
    refused_submission(command, current_revision, error)
}

/// The answer to a command that could not be made durable.
///
/// It is refused, not failed: the engine never saw it, so retrying it is safe
/// and the operator is told which command did not take effect instead of being
/// told nothing or being told it succeeded. A store that decided the refusal
/// itself (a [`CommandRefusal`], such as a LUT import that would replace a
/// bundled file) is answered with that refusal instead.
fn refused_submission(
    command: &CommandMessage,
    current_revision: u64,
    error: &(dyn Error + 'static),
) -> CommandSubmission {
    if let Some(refusal) = error.downcast_ref::<CommandRefusal>() {
        return rejected_submission(command, current_revision, refusal);
    }
    rejected_submission(
        command,
        current_revision,
        &CommandRefusal {
            code: RejectionCode::Unavailable,
            retryable: true,
            message: format!("command was refused because it could not be made durable: {error}"),
            reason: Some(fm_protocol::RefusalReason::NotDurable),
        },
    )
}

/// The answer to a command refused before anything about it was recorded:
/// no receipt, no revision, no events, so resubmitting it runs it afresh.
fn rejected_submission(
    command: &CommandMessage,
    current_revision: u64,
    refusal: &CommandRefusal,
) -> CommandSubmission {
    CommandSubmission {
        output: ResultBeforeEvents {
            result: CommandResult::Rejected {
                id: command.id.clone(),
                code: refusal.code.as_str().to_owned(),
                message: refusal.message.clone(),
                fields: Vec::new(),
                current_revision,
                retryable: refusal.retryable,
                reason: refusal.reason.clone(),
            },
            events: Vec::new(),
        },
        replayed: false,
        accepted: None,
        subscriber_failures: Vec::new(),
    }
}

/// A command refused while it was being staged, before anything about it was
/// made durable or committed to the engine.
///
/// Raised directly where the refusal is decided (a Stinger slot still
/// realizing) or derived by [`staging_refusal`] from a staging error.
use fm_daemon_journal::CommandRefusal;

/// Classifies an error raised while staging a command, before
/// `DurableStore::record`, into a refusal of that command, or returns the
/// error itself when it must stay fatal.
///
/// Every staging step only works on a preparation, which the caller drops on
/// failure, so a refusal leaves the engine, its receipts and event log, the
/// journal, and the in-memory project exactly as they were: refusing is
/// always safe for durability. What stays fatal is what says the daemon's
/// own state can no longer be trusted, so it fails closed:
///
/// * Refused, the daemon keeps running:
///   - [`CommandRefusal`]: decided where it was raised (for example a Stinger
///     slot whose desired and realized state still differ: `conflict`,
///     retryable once realization settles);
///   - the engine could not settle this command's realization to an idle
///     frame within the ticks it was given (`SnapshotError`): `conflict`,
///     retryable;
///   - the engine's scheduler could not take the command's action
///     (`EngineError::Schedule`): `resource_exhausted`, retryable;
///   - the realized switcher refused the command's action while it was
///     projected (`EngineError::RuntimeSwitcher`): `conflict`;
///   - the persisted-project schema refused the staged project
///     (`ProjectValidationError`), for example a receipt with a blank command
///     id: the command cannot be made durable, so it is `invalid_command`.
/// * Fatal, the daemon stops:
///   - an engine that cannot advance or whose authority is stale (clock,
///     tick, counter exhaustion, stale preparation, runtime identity or
///     sequencing errors): every later command would fail the same way;
///   - the stored project disagreeing with the engine about state the command
///     did not stage (a missing staged receipt, audio strip, or stream target,
///     a rename or reorder the project refuses after the engine accepted it):
///     the durable project no longer describes the engine, and anything
///     persisted from it would be corrupt.
///
/// Anything that fails after `record` returned is outside this function and
/// always fatal: the command is journaled, so it must never be reported as
/// rejected, and a restart replays it.
fn staging_refusal(error: Box<dyn Error>) -> Result<CommandRefusal, Box<dyn Error>> {
    let error = match error.downcast::<CommandRefusal>() {
        Ok(refusal) => return Ok(*refusal),
        Err(error) => error,
    };
    if let Some(validation) = error.downcast_ref::<fm_persistence::ProjectValidationError>() {
        return Ok(CommandRefusal {
            code: RejectionCode::InvalidCommand,
            retryable: false,
            message: format!(
                "command was refused because its result cannot be persisted: {validation}"
            ),
            reason: None,
        });
    }
    let Some(fm_control::ControlError::Engine(engine)) =
        error.downcast_ref::<fm_control::ControlError>()
    else {
        return Err(error);
    };
    let (code, retryable, reason) = match engine {
        fm_engine::EngineError::Snapshot(_) => (
            RejectionCode::Conflict,
            true,
            "its realization does not settle to an idle frame",
        ),
        fm_engine::EngineError::Schedule(_) => (
            RejectionCode::ResourceExhausted,
            true,
            "the engine scheduler cannot take its action",
        ),
        fm_engine::EngineError::RuntimeSwitcher(_) => (
            RejectionCode::Conflict,
            false,
            "the realized switcher refuses its action",
        ),
        // Clock, tick, counter exhaustion, stale preparation, and anything
        // added later: fail closed.
        _ => return Err(error),
    };
    Ok(CommandRefusal {
        code,
        retryable,
        message: format!("command was refused because {reason}: {engine}"),
        reason: None,
    })
}

/// Answers a staging failure: a refused command, or the fatal error.
fn refuse_staged_command(
    command: &CommandMessage,
    current_revision: u64,
    error: Box<dyn Error>,
    runtime_events: Vec<RuntimeEventMessage>,
) -> AppResult<DurableExecution> {
    let refusal = staging_refusal(error)?;
    Ok(DurableExecution {
        submission: rejected_submission(command, current_revision, &refusal),
        runtime_events,
    })
}

/// Executes `command` realizing every frame it projects at once: the
/// tests' way to drive a command to where it settles.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn execute_durable_command(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now_millis: u64,
) -> AppResult<DurableExecution> {
    execute_durable_command_with_ticks(
        control,
        store,
        durable,
        principal,
        server,
        command,
        now_millis,
        None,
        Realize::All,
        |control, server| Ok(control.tick(server)?.runtime_events),
    )
}

/// How many of the frames an accepted command projects
/// [`execute_durable_command_with_ticks`] realizes before it returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Realize {
    /// Every one: tests that drive a command to where it settles.
    #[cfg(test)]
    All,
    /// Only its first, so the command is realized on the frame right after
    /// its append and answered then; the frame loop realizes the rest (a
    /// timed transition, a title phase) one frame per pacer tick, serving
    /// every session in between: the native pacer, or the simulated
    /// daemon's wall-clock pacer in the session loop.
    First,
    /// None: journal replay, which ticks up to each recorded command's
    /// first frame itself (see [`replay_idle_frames`]).
    None,
}

impl Realize {
    /// The frames realized now of the `projected` an accepted (or not)
    /// command settles after. A refusal realizes nothing: it changes no
    /// realization, and projects past frames only while earlier work is
    /// in flight, which the frame loop realizes.
    fn now(self, projected: u64, accepted: bool) -> u64 {
        match self {
            _ if !accepted => 0,
            #[cfg(test)]
            Self::All => projected,
            Self::First => projected.min(1),
            Self::None => 0,
        }
    }
}

/// Refuses an accepted command by the project it would leave (the durable
/// project, then the staged one) before anything is recorded.
type ProjectVet<'a> = dyn Fn(&StoredProject, &StoredProject) -> Option<CommandRefusal> + 'a;

/// Stages, records, commits, and realizes one command.
///
/// `vet` may refuse an accepted command by the project it would leave;
/// nothing is staged or recorded for a refused one.
///
/// Failures before `store.record` refuse the command as classified by
/// [`staging_refusal`]; a failing `record` refuses it as undurable; failures
/// from the commit on are fatal because the command is already journaled.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn execute_durable_command_with_ticks(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now_millis: u64,
    vet: Option<&ProjectVet<'_>>,
    realize: Realize,
    mut tick: impl FnMut(
        &mut ControlService<Policy>,
        &ServerIdentity,
    ) -> AppResult<Vec<RuntimeEventMessage>>,
) -> AppResult<DurableExecution> {
    // Every document's page count is known before the command runs, live
    // and in replay alike, so a page step resolves the same way (IN-014).
    if let Some(documents) = store.documents() {
        documents::sync(control, server, documents)?;
    }
    // Nothing commits before `record`, so a refusal reports this revision.
    let current_revision = control.diagnostics().current_revision;
    let began_frame = control.frame_cursor();
    // What a template edit leaves unchanged (shared `Arc`s, so cheap).
    let templates_before = control.title_templates();
    let preparation = match control.prepare_submit(principal, command.clone(), now_millis) {
        Ok(preparation) => preparation,
        Err(error) => {
            return refuse_staged_command(command, current_revision, error.into(), Vec::new());
        }
    };
    let prepared = match preparation {
        PrepareSubmitOutcome::Replayed(submission) => {
            return Ok(DurableExecution {
                submission,
                runtime_events: Vec::new(),
            });
        }
        PrepareSubmitOutcome::Prepared(prepared) => prepared,
    };

    // The command is durable as the project it leaves once it settles: its
    // own frames, extended until its scene and title phases end and, while
    // an earlier command's work is still in flight, until that ends (see
    // `fm_control::PreparedSubmission::settle_ticks`).
    let accepted = prepared.submission().is_accepted();
    let (ticks, projected) = prepared.project_settled(if accepted {
        command_base_ticks(command, &prepared)
    } else {
        0
    });
    // An accepted undo or redo is journaled as the edit it resolved to:
    // replay restores an engine whose session history is empty. The project
    // sync reads the same resolved command, so an undone input removal and a
    // live input addition both know the input kind they persist.
    let journaled = prepared.replay_command(command);
    let staged = projected.map_err(Into::into).and_then(|projected| {
        stored_project_from_snapshot(
            durable,
            &projected,
            command,
            &journaled,
            &prepared.output().result,
        )
    });
    let updated = match staged {
        Ok(updated) => updated,
        Err(error) => {
            // Dropping the preparation abandons it without touching the
            // engine, exactly as a failed `record` does below.
            drop(prepared);
            return refuse_staged_command(command, current_revision, error, Vec::new());
        }
    };
    if prepared.submission().is_accepted()
        && let Some(refusal) = vet.and_then(|vet| vet(durable, &updated))
    {
        drop(prepared);
        return Ok(DurableExecution {
            submission: rejected_submission(command, current_revision, &refusal),
            runtime_events: Vec::new(),
        });
    }
    // An animation edit's template is staged before its batch, so whatever
    // replays the batch finds the template the manifest it lands on needs.
    let animations = staged_title_animations(&prepared, &updated, &templates_before);
    let staged_uris: Vec<String> = animations.iter().map(|(uri, _)| uri.clone()).collect();
    let revision = updated.position().revision;
    if let Err(error) = store.stage_title_animations(revision, &animations) {
        drop(prepared);
        // A staged copy left behind would be committed by the checkpoint
        // after the next command at this revision: stop instead.
        store.discard_staged_title_animations(revision, &staged_uris)?;
        eprintln!(
            "FREEMIXD_DURABILITY\tv=1\tcommand={}\trevision={revision}\terror={error}",
            command.id
        );
        return Ok(DurableExecution {
            submission: refused_submission(
                command,
                control.diagnostics().current_revision,
                error.as_ref(),
            ),
            runtime_events: Vec::new(),
        });
    }
    // Durability comes first: the mutation survives a crash before the engine
    // commits it, before its events are published, and before the acceptance is
    // returned to the caller for acknowledgement.
    if let Err(error) = store.record(&journaled, now_millis, began_frame, durable, &updated) {
        store.discard_staged_title_animations(revision, &staged_uris)?;
        // Nothing was made durable, so nothing may be acknowledged. Dropping
        // the preparation abandons it without touching the engine: no revision,
        // no receipt, no events, so the same command may simply be sent again.
        drop(prepared);
        eprintln!(
            "FREEMIXD_DURABILITY\tv=1\tcommand={}\trevision={}\terror={error}",
            command.id,
            updated.position().revision
        );
        return Ok(DurableExecution {
            submission: refused_submission(
                command,
                control.diagnostics().current_revision,
                error.as_ref(),
            ),
            runtime_events: Vec::new(),
        });
    }

    let submission = prepared.commit()?;
    // A document the command added or replaced reports its pages now.
    if let Some(documents) = store.documents() {
        documents::sync(control, server, documents)?;
    }
    let mut runtime_events = Vec::new();
    for _ in 0..realize.now(ticks, accepted) {
        runtime_events.extend(tick(control, server)?);
    }
    // The project as it settles: a command whose later frames the native
    // frame loop realizes is durable (and checkpointed) as it ends.
    *durable = updated;

    Ok(DurableExecution {
        submission,
        runtime_events,
    })
}

/// The templates an accepted command leaves `updated` with: one per
/// `title_animation_changed` or `title_exposure_changed` event, keyed by the
/// stored template URI its titles name. A staged template is whole, so the
/// part the event does not change (the hidden fields of an animation edit,
/// the animation of an exposure edit) comes from `before`, the engine's
/// templates as the command found them.
fn staged_title_animations(
    prepared: &fm_control::PreparedSubmission<'_, Policy>,
    updated: &StoredProject,
    before: &[fm_engine::EngineTitleTemplate],
) -> Vec<(String, journal::StagedTemplate)> {
    if !prepared.submission().is_accepted() {
        return Vec::new();
    }
    let template = |uri: &str| {
        before
            .iter()
            .find(|template| &*template.key == uri)
            .map(|template| &template.template)
    };
    let uri_of = |inputs: &[fm_protocol::WireInputId]| {
        title_render::title_of_input(updated.project(), inputs.first()?.to_domain())
            .map(|title| title.template_uri.clone())
    };
    prepared
        .output()
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            fm_protocol::EventPayload::TitleAnimationChanged {
                inputs,
                animation_revision,
                clip,
            } => {
                let uri = uri_of(inputs)?;
                let clip = fm_control::title_clip_from_wire(clip).ok()?;
                let hidden = template(&uri).map_or_else(Vec::new, |template| {
                    template
                        .fields
                        .iter()
                        .filter(|field| !field.exposed)
                        .map(|field| field.id)
                        .collect()
                });
                let animation = fm_titles::TitleAnimation::new(clip, *animation_revision);
                Some((uri, journal::StagedTemplate { animation, hidden }))
            }
            fm_protocol::EventPayload::TitleExposureChanged { inputs, fields } => {
                let uri = uri_of(inputs)?;
                let animation = template(&uri)?.animation.clone();
                let hidden = fields
                    .iter()
                    .filter(|item| !item.exposed)
                    .map(|item| fm_titles::FieldId::new(item.field.get()))
                    .collect();
                Some((uri, journal::StagedTemplate { animation, hidden }))
            }
            _ => None,
        })
        .collect()
}

/// The frames an accepted command realizes on its own: its transition,
/// title phase, or target frame. Its projection extends them until every
/// scene and title phase it started has played (GX-011 stage 5) and any
/// work in flight before it has ended (see
/// `fm_control::PreparedSubmission::settle_ticks`).
#[allow(clippy::too_many_lines)] // one arm per command payload
fn command_base_ticks(
    command: &CommandMessage,
    prepared: &fm_control::PreparedSubmission<'_, Policy>,
) -> u64 {
    // A playlist command may target a later frame and start a Program
    // transition there.
    if matches!(
        command.payload,
        CommandPayload::SetPlaylist { .. }
            | CommandPayload::PlaylistControl { .. }
            | CommandPayload::PlaylistAdvance { .. }
    ) {
        return prepared.playlist_ticks();
    }
    // A cue operation or script run may target a later frame and start
    // transitions.
    if matches!(
        command.payload,
        CommandPayload::CueControl { .. }
            | CommandPayload::CueAdvance { .. }
            | CommandPayload::RunScript { .. }
            | CommandPayload::FireShortcut { .. }
    ) {
        return prepared.cue_ticks();
    }
    u64::from(match command.payload {
        CommandPayload::Fade { duration_frames }
        | CommandPayload::AlphaFade { duration_frames }
        | CommandPayload::Slide { duration_frames }
        | CommandPayload::Zoom { duration_frames }
        | CommandPayload::Wipe { duration_frames }
        | CommandPayload::Transition {
            duration_frames, ..
        }
        | CommandPayload::FadeToBlack {
            duration_frames, ..
        } => duration_frames,
        CommandPayload::Stinger {
            duration_frames, ..
        } => {
            if prepared.project(1).is_ok() {
                1
            } else {
                duration_frames
            }
        }
        CommandPayload::TakeOverlay { channel, .. }
        | CommandPayload::OverlayOff { channel }
        | CommandPayload::TakeNextOverlay { channel } => {
            let channel =
                OverlayChannelId::new(channel.number()).unwrap_or(OverlayChannelId::ALL[0]);
            prepared.overlay_transition_ticks(channel)
        }
        // A title take realizes its whole In phase, as a fade realizes its
        // whole duration.
        CommandPayload::TakeTitle { input, channel, .. } => {
            let channel =
                OverlayChannelId::new(channel.number()).unwrap_or(OverlayChannelId::ALL[0]);
            prepared.title_take_ticks(input.to_domain(), channel)
        }
        CommandPayload::TitleOffAir { input } => prepared.title_off_air_ticks(input.to_domain()),
        CommandPayload::RenameInput { .. }
        | CommandPayload::SetTitleFields { .. }
        | CommandPayload::ReorderInputs { .. }
        | CommandPayload::SetInputAudioStrip { .. }
        | CommandPayload::SelectPreview { .. }
        | CommandPayload::Cut
        | CommandPayload::ConfigureStinger { .. }
        | CommandPayload::RemoveStinger { .. }
        | CommandPayload::SetTransitionFavorite { .. }
        | CommandPayload::Media { .. }
        | CommandPayload::PageNavigate { .. }
        | CommandPayload::MediaList { .. }
        | CommandPayload::InputLifecycle(_)
        | CommandPayload::SceneLayer(_)
        | CommandPayload::AudioMixer(_)
        | CommandPayload::SetFadeToBlackOutput { .. }
        | CommandPayload::SetFadeToBlackOutputDuration { .. }
        | CommandPayload::SetFadeToBlackCurve { .. }
        | CommandPayload::SetDisplayOutput { .. }
        | CommandPayload::ClearDisplayOutput { .. }
        | CommandPayload::SetLiveLan { .. }
        | CommandPayload::SetAlertRule(_)
        | CommandPayload::Replay(_)
        | CommandPayload::Reel(_)
        | CommandPayload::SetNetworkStream { .. }
        | CommandPayload::SetCaptureSettings { .. }
        | CommandPayload::UpdateOverlay { .. }
        | CommandPayload::SetOverlayOutputInclusion { .. }
        | CommandPayload::ConfigureOverlayTransition { .. }
        | CommandPayload::ConfigureOverlayAppearance { .. }
        | CommandPayload::QueueOverlay { .. }
        | CommandPayload::StartManualTransition { .. }
        | CommandPayload::SetManualTransitionPosition { .. }
        | CommandPayload::CommitManualTransition
        | CommandPayload::CancelManualTransition
        | CommandPayload::StreamStart { .. }
        | CommandPayload::StreamStop { .. }
        | CommandPayload::StreamRetry { .. }
        | CommandPayload::StreamStartAll
        | CommandPayload::StreamStopAll
        | CommandPayload::SetStreamTarget { .. }
        | CommandPayload::RemoveStreamTarget { .. }
        | CommandPayload::RecordStart { .. }
        | CommandPayload::RecordStop { .. }
        | CommandPayload::Undo { .. }
        | CommandPayload::Redo { .. }
        | CommandPayload::SetEditLock { .. }
        | CommandPayload::AddInput { .. }
        | CommandPayload::RemoveInput { .. }
        | CommandPayload::SetPlaylist { .. }
        | CommandPayload::PlaylistControl { .. }
        | CommandPayload::PlaylistAdvance { .. }
        | CommandPayload::SetCueList { .. }
        | CommandPayload::RemoveCueList { .. }
        | CommandPayload::CueControl { .. }
        | CommandPayload::CueAdvance { .. }
        | CommandPayload::SetScript { .. }
        | CommandPayload::RemoveScript { .. }
        | CommandPayload::RunScript { .. }
        | CommandPayload::StopScript { .. }
        | CommandPayload::SetTitleAnimation { .. }
        | CommandPayload::SetTitleFieldExposure { .. }
        | CommandPayload::ApplyTitleAnimationPreset { .. }
        | CommandPayload::StretchTitleAnimation { .. }
        | CommandPayload::SetSceneAnimation { .. }
        | CommandPayload::ApplySceneAnimationPreset { .. }
        | CommandPayload::StretchSceneAnimation { .. }
        | CommandPayload::SetInputGrade { .. }
        | CommandPayload::SetLayerGrade { .. }
        | CommandPayload::SetProgramGrade { .. }
        | CommandPayload::AddLibraryGrade { .. }
        | CommandPayload::UpdateLibraryGrade { .. }
        | CommandPayload::RemoveLibraryGrade { .. }
        | CommandPayload::ImportGradeLut { .. }
        | CommandPayload::RemoveGradeLut { .. }
        | CommandPayload::AddAnimationPreset { .. }
        | CommandPayload::RenameAnimationPreset { .. }
        | CommandPayload::RemoveAnimationPreset { .. }
        | CommandPayload::ApplyTitleAnimationUserPreset { .. }
        | CommandPayload::ApplySceneAnimationUserPreset { .. }
        | CommandPayload::SetShortcut { .. }
        | CommandPayload::RemoveShortcut { .. }
        | CommandPayload::FireShortcut { .. } => 1,
    })
}

fn stored_project_from_snapshot(
    durable: &StoredProject,
    snapshot: &EngineSnapshot,
    command: &CommandMessage,
    journaled: &CommandMessage,
    result: &CommandResult,
) -> AppResult<StoredProject> {
    let mut receipts = snapshot
        .receipts()
        .iter()
        .map(persisted_engine_receipt)
        .collect::<Vec<_>>();

    if !receipts
        .iter()
        .any(|receipt| receipt.key() == command.idempotency_key)
    {
        // Refused before the engine: an authorization denial, or an edit
        // carrying a hidden title value marker (Protocol 2.104).
        if !matches!(
            result,
            CommandResult::Rejected { code, .. }
                if code == "permission_denied" || code == "invalid_command"
        ) {
            return Err(AppFailure(
                "projected engine snapshot is missing the staged command receipt".into(),
            )
            .into());
        }
        receipts.push(persisted_result(command, result)?);
    }

    stored_project_with_receipts(
        durable,
        snapshot,
        retained_receipts(receipts, Some(&command.idempotency_key)),
        Some(journaled),
    )
}

fn stored_project_checkpoint(
    durable: &StoredProject,
    snapshot: &EngineSnapshot,
) -> AppResult<StoredProject> {
    stored_project_with_receipts(
        durable,
        snapshot,
        retained_receipts(
            snapshot
                .receipts()
                .iter()
                .map(persisted_engine_receipt)
                .collect(),
            None,
        ),
        None,
    )
}

/// Keeps the newest [`MAX_PERSISTED_RECEIPTS`] receipts.
///
/// Recency is the revision a receipt was recorded at; a rejection recorded at
/// revision `r` happened after the command that produced `r`, so it ranks just
/// above that accepted receipt. Ties break by key so retention is
/// deterministic. The receipt for `pinned` (the command being made durable)
/// is always kept, so its answer stays replayable after a restart.
fn retained_receipts(
    mut receipts: Vec<IdempotencyReceipt>,
    pinned: Option<&str>,
) -> Vec<IdempotencyReceipt> {
    if receipts.len() <= MAX_PERSISTED_RECEIPTS {
        return receipts;
    }
    receipts.sort_by(|left, right| {
        receipt_recency(left, pinned)
            .cmp(&receipt_recency(right, pinned))
            .then_with(|| left.key().cmp(right.key()))
    });
    receipts.split_off(receipts.len() - MAX_PERSISTED_RECEIPTS)
}

fn receipt_recency(receipt: &IdempotencyReceipt, pinned: Option<&str>) -> (bool, u64, bool) {
    let (revision, rejected) = match receipt.outcome() {
        ReceiptOutcome::Accepted { revision, .. } => (*revision, false),
        ReceiptOutcome::Rejected {
            current_revision, ..
        } => (*current_revision, true),
    };
    (pinned == Some(receipt.key()), revision, rejected)
}

#[allow(clippy::too_many_lines)]
fn stored_project_with_receipts(
    durable: &StoredProject,
    snapshot: &EngineSnapshot,
    receipts: Vec<IdempotencyReceipt>,
    command: Option<&CommandMessage>,
) -> AppResult<StoredProject> {
    let show = snapshot.show();
    let desired = show.desired_switcher();
    let realized = snapshot.realized_switcher();
    let mut project = durable.project().clone();
    project.set_main_mix(MainMix::new(desired.program(), desired.preview()));
    sync_project_inputs(&mut project, show, command)?;
    if project
        .inputs()
        .iter()
        .map(|input| input.id)
        .ne(show.inputs().iter().copied())
    {
        project.reorder_inputs(show.inputs())?;
    }
    for (&input, name) in show.inputs().iter().zip(show.input_names()) {
        if project
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input)
            .is_none_or(|candidate| candidate.name != *name)
        {
            project.rename_input(input, name.clone())?;
        }
    }
    for (&input, &state) in show.input_audio_strips() {
        if !project.set_input_audio_strip(input, model_audio_strip_state(state)) {
            return Err(
                AppFailure(format!("project is missing audio strip for input {input}")).into(),
            );
        }
    }
    sync_project_stingers(&mut project, desired, realized)?;
    sync_project_streams(&mut project, show)?;
    sync_project_recording(&mut project, desired);
    sync_project_edit_locks(&mut project, show)?;
    sync_project_titles(&mut project, show)?;
    sync_project_scenes(&mut project, show)?;
    sync_project_input_lifecycle(&mut project, show)?;
    sync_project_audio_mixer(&mut project, show)?;
    sync_project_display_outputs(&mut project, show)?;
    project.set_playlist(fm_engine::playlist_to_model(show.playlist()));
    project.set_cue_lists(show.cue_lists().to_vec());
    project.set_scripts(show.scripts().to_vec());
    project.set_script_run(show.script_run().clone());
    project.set_shortcuts(show.shortcuts().to_vec());
    project.set_media_lists(show.media_lists().clone());
    project.set_title_timelines(snapshot.title_timelines().clone());
    project.set_grading(show.grading().clone());
    project.set_animation_presets(show.animation_presets().clone());
    project.set_transition_favorites(*show.transition_favorites());
    for (&input, settings) in show.media_inputs() {
        if project.media_playback(input) != Some(settings) {
            project
                .set_media_playback(input, *settings)
                .map_err(|error| {
                    AppFailure(format!(
                        "project cannot take media input {input}'s playback: {error}"
                    ))
                })?;
        }
    }
    // A document's page lives beside its asset (IN-014).
    for (&input, &page) in show.paged_inputs() {
        if project
            .document(input)
            .is_some_and(|document| document.page != page)
            && !project.set_document_page(input, page)
        {
            return Err(
                AppFailure(format!("project cannot take document input {input}'s page")).into(),
            );
        }
    }
    // Capture settings live beside the device source (IN-001).
    for (input, _, settings) in show.capture_inputs() {
        if project.capture_input(input).map(|(_, current)| current) != Some(settings) {
            project
                .set_capture_settings(input, settings.clone())
                .map_err(|error| {
                    AppFailure(format!(
                        "project cannot take capture input {input}'s settings: {error}"
                    ))
                })?;
        }
    }
    StoredProject::from_project_with_complete_runtime_state(
        project,
        RuntimeRouting {
            desired_program_id: Some(desired.program()),
            realized_program_id: Some(realized.program()),
            desired_preview_id: Some(desired.preview()),
            realized_preview_id: Some(realized.preview()),
        },
        RuntimeManualTransitions {
            desired: desired.t_bar().map(persisted_t_bar),
            realized: realized.t_bar().map(persisted_t_bar),
        },
        RuntimeFadeToBlack {
            desired: persisted_fade_to_black(snapshot.desired_fade_to_black()),
            realized: persisted_fade_to_black(snapshot.realized_fade_to_black()),
        },
        persisted_overlays(desired, realized),
        snapshot_position(snapshot),
        receipts,
    )
    .map_err(Into::into)
}

/// The typed usage of a persisted project reference that keeps an input
/// from being removed.
fn input_reference_use(reference: fm_model::InputReference) -> fm_protocol::InputUse {
    use fm_model::InputReference;
    use fm_protocol::InputUse;
    match reference {
        InputReference::Program => InputUse::Program,
        InputReference::Preview => InputUse::Preview,
        InputReference::Stinger(slot) => InputUse::Stinger {
            slot: slot.number(),
        },
        InputReference::SceneLayer(scene) => InputUse::SceneLayer {
            scene: scene.get().get(),
        },
        InputReference::SceneAudioSource(scene_input) => InputUse::SceneAudioSource {
            scene_input: scene_input.get().get(),
        },
        InputReference::AliasTarget(alias) => InputUse::AliasTarget {
            alias: alias.get().get(),
        },
        InputReference::PlaylistEntry(entry) => InputUse::PlaylistEntry { entry },
        InputReference::CueAction { list, cue } => InputUse::CueAction { list, cue },
        // The wire has no reel-clip use yet; a reel angle is recorded for
        // replay, so report the closest existing use rather than widening
        // the protocol in the model-only slice.
        InputReference::ReplaySource | InputReference::ReelClip { .. } => InputUse::ReplaySource,
    }
}

/// Folds a live input addition or removal into the stored project.
///
/// The engine's show owns the input catalogue but not the input kinds, so an
/// addition reads its kind from the command being made durable (always a
/// journaled `input_add`, also when an undo resolved to it). A removal is
/// checked against the persisted references the engine does not model, Scene
/// layers, Scene audio sources and alias targets, and refused with the reason
/// when one still names the input. Any other divergence means the show and the
/// project disagree, which is fatal.
/// The refusal for removing an input the stored project still references.
fn input_in_use_refusal(input: InputId, reference: fm_model::InputReference) -> CommandRefusal {
    CommandRefusal {
        code: RejectionCode::Conflict,
        retryable: false,
        message: format!("input {input} cannot be removed: it is {reference}"),
        reason: Some(fm_protocol::RefusalReason::InputInUse {
            input: input.get().get(),
            usage: input_reference_use(reference),
        }),
    }
}

#[allow(clippy::too_many_lines)] // one arm per added input source
fn sync_project_inputs(
    project: &mut fm_model::Project,
    show: &ShowState,
    command: Option<&CommandMessage>,
) -> AppResult<()> {
    let removed = project
        .inputs()
        .iter()
        .map(|input| input.id)
        .filter(|input| !show.inputs().contains(input))
        .collect::<Vec<_>>();
    for input in removed {
        if let Some(reference) = project.input_reference(input) {
            return Err(input_in_use_refusal(input, reference).into());
        }
        project
            .remove_input(input)
            .map_err(|error| AppFailure(format!("input {input} could not be removed: {error}")))?;
    }
    let added = show
        .inputs()
        .iter()
        .zip(show.input_names())
        .filter(|(input, _)| !project.inputs().iter().any(|known| known.id == **input))
        .map(|(&input, name)| (input, name.clone()))
        .collect::<Vec<_>>();
    for (input, name) in added {
        let source = match command.map(|command| &command.payload) {
            Some(CommandPayload::AddInput {
                input: added,
                source,
                ..
            }) if added.to_domain() == input => source.clone(),
            Some(CommandPayload::InputLifecycle(
                fm_protocol::InputLifecycleOperation::Duplicate {
                    source,
                    input: added,
                    ..
                },
            )) if added.to_domain() == input => {
                project
                    .duplicate_input(source.to_domain(), input, name)
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be duplicated: {error}"))
                    })?;
                continue;
            }
            // A media list input (IN-006), created or recreated by a redo:
            // a media input playing its active item.
            Some(
                CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::Import {
                    ..
                })
                | CommandPayload::MediaList { .. }
                | CommandPayload::Redo { .. },
            ) if show.media_list(input).is_some()
                || matches!(
                    command.map(|command| &command.payload),
                    Some(CommandPayload::InputLifecycle(
                        fm_protocol::InputLifecycleOperation::Import { .. }
                    ))
                ) =>
            {
                let source = show
                    .input_source(input)
                    .ok_or_else(|| AppFailure(format!("imported input {input} has no source")))?;
                let playback = show.media_settings(input).copied().unwrap_or_default();
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: source.to_kind(playback),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be imported: {error}"))
                    })?;
                continue;
            }
            _ => {
                return Err(AppFailure(format!(
                    "projected engine snapshot adds input {input} without an input_add command"
                ))
                .into());
            }
        };
        let video = match source {
            fm_protocol::LiveInputSource::Solid {
                red,
                green,
                blue,
                alpha,
            } => {
                fm_model::SimulatedVideo::Solid(fm_model::SolidColor::new(red, green, blue, alpha))
            }
            fm_protocol::LiveInputSource::Bars => fm_model::SimulatedVideo::Bars,
            fm_protocol::LiveInputSource::NetworkStream(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::NetworkStream(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::Capture(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::Device {
                            stable_key: config.stable_key,
                            capture: config.settings,
                        },
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::SystemAudio(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::SystemAudio(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::Image(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::Image(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::Slideshow(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::Slideshow(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::ScreenCapture(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::ScreenCapture(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::Replay => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::Replay,
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
        };
        project
            .add_input_checked(fm_model::Input {
                id: input,
                name,
                kind: fm_model::InputKind::Simulated(fm_model::SimulatedInput::new(
                    video,
                    fm_model::SimulatedAudio::Silence,
                )),
                required_capabilities: Vec::new(),
            })
            .map_err(|error| AppFailure(format!("input {input} could not be added: {error}")))?;
    }
    Ok(())
}

/// Folds replaced input sources, the category list, and every input's
/// lifecycle into the stored project (IN-026). The engine's show owns all
/// three; a source the project does not hold is a replacement.
fn sync_project_input_lifecycle(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    for (&input, source) in show.input_sources() {
        let Some(current) = project
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input)
        else {
            continue;
        };
        if fm_model::InputSourceSpec::from_kind(&current.kind).as_ref() == Some(source) {
            continue;
        }
        let playback = show.media_settings(input).copied().unwrap_or_default();
        let capabilities = current.required_capabilities.clone();
        project
            .replace_input_source(input, source.to_kind(playback), capabilities)
            .map_err(|error| {
                AppFailure(format!(
                    "input {input}'s source could not be replaced: {error}"
                ))
            })?;
    }
    let stale = project
        .input_lifecycles()
        .keys()
        .copied()
        .filter(|input| !show.input_lifecycles().contains_key(input))
        .collect::<Vec<_>>();
    for input in stale {
        project.restore_input_lifecycle(input, fm_model::InputLifecycle::default());
    }
    project.restore_input_categories(show.input_categories().to_vec());
    for (&input, &lifecycle) in show.input_lifecycles() {
        project.restore_input_lifecycle(input, lifecycle);
    }
    Ok(())
}

/// Folds the show's stream destinations and their desired running flags into
/// the stored project.
///
/// StreamStart/StreamStop, the group start/stop, and destination edits
/// (OR-006) are engine commands, so the projected show is the authority for
/// both the configurations and the desired switcher's running flags;
/// mirroring them here makes an accepted command survive restart through
/// journal replay and checkpoint, exactly as renames and audio strips do.
/// Removed destinations leave first, then every remaining one is replaced or
/// appended in the show's order.
fn sync_project_streams(project: &mut fm_model::Project, show: &ShowState) -> AppResult<()> {
    let desired = show.desired_switcher();
    let stale = project
        .stream_targets()
        .iter()
        .map(fm_model::StreamTarget::id)
        .filter(|id| {
            !show
                .stream_targets()
                .iter()
                .any(|target| target.id() == *id)
        })
        .collect::<Vec<_>>();
    for target_id in stale {
        project.remove_stream_target(target_id).map_err(|error| {
            AppFailure(format!(
                "stream target {target_id} could not be removed: {error}"
            ))
        })?;
    }
    for target in show.stream_targets() {
        let target_id = SwitcherStreamTargetId::from_non_zero(target.id().get());
        let running = desired.stream_running(target_id).ok_or_else(|| {
            AppFailure(format!(
                "projected engine snapshot is missing stream target {target_id}"
            ))
        })?;
        let target = target.clone().set_running(running);
        let target_id = target.id();
        match project
            .stream_targets()
            .iter()
            .find(|existing| existing.id() == target_id)
        {
            Some(existing) if *existing == target => {}
            Some(_) => project.replace_stream_target(target).map_err(|error| {
                AppFailure(format!(
                    "stream target {target_id} could not be persisted: {error}"
                ))
            })?,
            None => project.add_stream_target_checked(target).map_err(|error| {
                AppFailure(format!(
                    "stream target {target_id} could not be persisted: {error}"
                ))
            })?,
        }
    }
    // The show's order is authoritative: an undone removal comes back at its
    // old place, not at the end where an add appends it.
    for (index, target) in show.stream_targets().iter().enumerate() {
        if project
            .stream_targets()
            .get(index)
            .map(fm_model::StreamTarget::id)
            != Some(target.id())
        {
            project
                .move_stream_target(target.id(), index)
                .map_err(|error| {
                    AppFailure(format!(
                        "stream target {} could not be reordered: {error}",
                        target.id()
                    ))
                })?;
        }
    }
    Ok(())
}

/// Folds both slots' desired recording flags into the stored project before
/// the accepted command is acknowledged.
///
/// RecordStart/RecordStop are engine commands, so the projected snapshot's
/// desired switcher is the authority for every slot; mirroring the whole
/// roster here makes an accepted command survive restart through journal
/// replay and checkpoint, exactly as the stream roster does. A slot the
/// command did not address keeps its previously projected flag, so one
/// slot's start never rewrites the other's stop.
fn sync_project_recording(project: &mut fm_model::Project, desired: &SwitcherState) {
    for slot in ModelRecorderSlot::ALL {
        let active = desired.recording_desired(switcher_recorder_slot(slot));
        project.set_recording_desired(slot, active);
    }
}

/// Folds the engine's edit padlock into the stored project.
///
/// `SetEditLock` is a durable engine command, so the projected show is the
/// authority for both the project flag and the per-input set; mirroring the
/// whole state makes an accepted lock change survive restart through journal
/// replay and checkpoint alike.
fn sync_project_edit_locks(project: &mut fm_model::Project, show: &ShowState) -> AppResult<()> {
    project.set_project_locked(show.is_project_locked());
    let stale = project
        .locked_inputs()
        .difference(show.locked_inputs())
        .copied()
        .collect::<Vec<_>>();
    for input in stale {
        project.set_input_locked(input, false);
    }
    for &input in show.locked_inputs() {
        project
            .set_input_locked_checked(input, true)
            .map_err(|error| AppFailure(format!("edit lock could not be persisted: {error}")))?;
    }
    Ok(())
}

/// Persists every title's live field values and revisions (the title's and each field's).
fn sync_project_titles(project: &mut fm_model::Project, show: &ShowState) -> AppResult<()> {
    for (&input, title) in show.titles() {
        let fields = title
            .fields
            .values()
            .iter()
            .map(|(field, value)| fm_model::TitleFieldEntry {
                id: fm_model::TitleFieldId::new(field.get()),
                value: title_render::model_field_value(value),
                revision: title.fields.field_revision(*field).unwrap_or(0),
            })
            .collect();
        project
            .set_title_fields(input, title.fields.revision(), fields)
            .map_err(|error| {
                AppFailure(format!(
                    "title input {input} fields could not be persisted: {error:?}"
                ))
            })?;
    }
    Ok(())
}

fn persisted_overlays(desired: &SwitcherState, realized: &SwitcherState) -> RuntimeOverlays {
    RuntimeOverlays {
        desired: std::array::from_fn(|index| persisted_overlay(&desired.overlays()[index])),
        realized: std::array::from_fn(|index| persisted_overlay(&realized.overlays()[index])),
    }
}

fn persisted_overlay(channel: &OverlayChannelState) -> RuntimeOverlayChannel {
    RuntimeOverlayChannel {
        source: channel.source(),
        active: channel.is_active(),
        transition: RuntimeOverlayTransition::from_token(
            fm_control::wire_overlay_transition(channel.transition()).token(),
        )
        .unwrap_or_default(),
        duration_frames: channel.duration_frames(),
        position: persisted_overlay_position(channel.position()),
        border: persisted_overlay_border(channel.border()),
        queued_sources: channel.queued_sources().to_vec(),
        included_outputs: channel.included_outputs().to_vec(),
    }
}

fn persisted_overlay_position(position: OverlayPositionPreset) -> RuntimeOverlayPosition {
    match position {
        OverlayPositionPreset::FullFrame => RuntimeOverlayPosition::FullFrame,
        OverlayPositionPreset::TopLeft => RuntimeOverlayPosition::TopLeft,
        OverlayPositionPreset::TopRight => RuntimeOverlayPosition::TopRight,
        OverlayPositionPreset::BottomLeft => RuntimeOverlayPosition::BottomLeft,
        OverlayPositionPreset::BottomRight => RuntimeOverlayPosition::BottomRight,
    }
}

fn restored_overlay_position(position: RuntimeOverlayPosition) -> OverlayPositionPreset {
    match position {
        RuntimeOverlayPosition::FullFrame => OverlayPositionPreset::FullFrame,
        RuntimeOverlayPosition::TopLeft => OverlayPositionPreset::TopLeft,
        RuntimeOverlayPosition::TopRight => OverlayPositionPreset::TopRight,
        RuntimeOverlayPosition::BottomLeft => OverlayPositionPreset::BottomLeft,
        RuntimeOverlayPosition::BottomRight => OverlayPositionPreset::BottomRight,
    }
}

fn persisted_overlay_border(border: OverlayBorderPreset) -> RuntimeOverlayBorder {
    match border {
        OverlayBorderPreset::None => RuntimeOverlayBorder::None,
        OverlayBorderPreset::ThinWhite => RuntimeOverlayBorder::ThinWhite,
        OverlayBorderPreset::ThickWhite => RuntimeOverlayBorder::ThickWhite,
    }
}

fn restored_overlay_border(border: RuntimeOverlayBorder) -> OverlayBorderPreset {
    match border {
        RuntimeOverlayBorder::None => OverlayBorderPreset::None,
        RuntimeOverlayBorder::ThinWhite => OverlayBorderPreset::ThinWhite,
        RuntimeOverlayBorder::ThickWhite => OverlayBorderPreset::ThickWhite,
    }
}

fn restore_overlays(
    show: &mut ShowState,
    realized: &mut SwitcherState,
    overlays: &RuntimeOverlays,
) -> AppResult<()> {
    for (index, (desired, realized_state)) in
        overlays.desired.iter().zip(&overlays.realized).enumerate()
    {
        let Some(channel) = OverlayChannelId::from_index(index) else {
            continue;
        };
        restore_overlay(show.desired_switcher_mut(), channel, desired)?;
        restore_overlay(realized, channel, realized_state)?;
    }
    Ok(())
}

fn restore_overlay(
    switcher: &mut SwitcherState,
    channel: OverlayChannelId,
    state: &RuntimeOverlayChannel,
) -> AppResult<()> {
    switcher.configure_overlay_transition(
        channel,
        fm_control::domain_overlay_transition(
            fm_protocol::OverlayTransitionKind::from_token(state.transition.token())
                .unwrap_or_default(),
        ),
        state.duration_frames,
    )?;
    let _ = switcher.configure_overlay_appearance(
        channel,
        restored_overlay_position(state.position),
        restored_overlay_border(state.border),
    );
    if let Some(source) = state.source {
        if state.active {
            switcher.take_overlay(channel, source)?;
        } else {
            switcher.update_overlay(channel, source)?;
        }
    }
    for source in &state.queued_sources {
        switcher.queue_overlay(channel, *source)?;
    }
    for output in &state.included_outputs {
        let _ = switcher.set_overlay_output_inclusion(channel, *output, true);
    }
    Ok(())
}

fn sync_project_stingers(
    project: &mut fm_model::Project,
    desired: &SwitcherState,
    realized: &SwitcherState,
) -> AppResult<()> {
    let Ok(last) = u8::try_from(StingerSlotNumber::COUNT) else {
        return Ok(());
    };
    for number in 1..=last {
        let (Some(model_slot), Some(slot)) =
            (StingerSlotNumber::new(number), StingerSlotId::new(number))
        else {
            continue;
        };
        let desired_state = desired.stinger(slot);
        if desired_state != realized.stinger(slot) {
            // Realization has not caught up with the desired slot yet. That
            // is runtime state, not corruption: refuse the command so it can
            // be retried once the slot settles, instead of persisting a slot
            // that was never realized.
            return Err(CommandRefusal {
                code: RejectionCode::Conflict,
                retryable: true,
                message: format!(
                    "command was refused because Stinger slot {number} has not finished \
                     realizing; retry once it has"
                ),
                reason: Some(fm_protocol::RefusalReason::StingerRealizing { slot: number }),
            }
            .into());
        }
        let _ = project.remove_stinger(model_slot);
        let Some(descriptor) = desired_state.descriptor() else {
            continue;
        };
        project.set_stinger(StingerConfig::new(
            model_slot,
            descriptor.media_input,
            descriptor.preload,
            descriptor.cut_point_frames,
            match descriptor.audio_policy {
                StingerAudioPolicy::Muted => ModelStingerAudioPolicy::Muted,
                StingerAudioPolicy::StingerOnly => ModelStingerAudioPolicy::StingerOnly,
                StingerAudioPolicy::MixWithProgram => ModelStingerAudioPolicy::MixWithProgram,
            },
            match descriptor.missing_media_fallback {
                MissingMediaFallback::Cut => StingerMissingMediaFallback::Cut,
                MissingMediaFallback::Fade => StingerMissingMediaFallback::Fade,
                MissingMediaFallback::KeepProgram => StingerMissingMediaFallback::KeepProgram,
            },
        ));
    }
    Ok(())
}

fn persisted_fade_to_black(state: fm_engine::EngineFadeToBlackState) -> PersistedFadeToBlackState {
    PersistedFadeToBlackState::new(
        state.active,
        u16::try_from(state.position.numerator()).unwrap_or(u16::MAX),
    )
}

fn persisted_t_bar(state: TBarState) -> PersistedManualTransitionState {
    let kind = match state.kind() {
        TransitionKind::Fade => PersistedManualTransitionKind::Fade,
        TransitionKind::Wipe => PersistedManualTransitionKind::Wipe,
        TransitionKind::AlphaFade => PersistedManualTransitionKind::AlphaFade,
        TransitionKind::Slide => PersistedManualTransitionKind::Slide,
        TransitionKind::VerticalWipe => PersistedManualTransitionKind::VerticalWipe,
        TransitionKind::VerticalSlide => PersistedManualTransitionKind::VerticalSlide,
        TransitionKind::Zoom => PersistedManualTransitionKind::Zoom,
        TransitionKind::Fly => PersistedManualTransitionKind::Fly,
        TransitionKind::FlyRotate => PersistedManualTransitionKind::FlyRotate,
        TransitionKind::Cube => PersistedManualTransitionKind::Cube,
        TransitionKind::CubeZoom => PersistedManualTransitionKind::CubeZoom,
        TransitionKind::CrossZoom => PersistedManualTransitionKind::CrossZoom,
        TransitionKind::Merge => PersistedManualTransitionKind::Merge,
        TransitionKind::Stinger(slot) => PersistedManualTransitionKind::Stinger {
            slot: slot.number(),
            clip_frames: state.clip_frames(),
        },
    };
    PersistedManualTransitionState {
        kind,
        from_id: state.from(),
        to_id: state.to(),
        interval_start_basis_points: state
            .interval_start()
            .basis_points()
            .min(PersistedManualTransitionState::MAX_POSITION),
        position_basis_points: state
            .position()
            .basis_points()
            .min(PersistedManualTransitionState::MAX_POSITION),
    }
}

fn restored_t_bar(state: PersistedManualTransitionState) -> AppResult<TBarState> {
    let kind = match state.kind {
        PersistedManualTransitionKind::Fade => TransitionKind::Fade,
        PersistedManualTransitionKind::Wipe => TransitionKind::Wipe,
        PersistedManualTransitionKind::AlphaFade => TransitionKind::AlphaFade,
        PersistedManualTransitionKind::Slide => TransitionKind::Slide,
        PersistedManualTransitionKind::VerticalWipe => TransitionKind::VerticalWipe,
        PersistedManualTransitionKind::VerticalSlide => TransitionKind::VerticalSlide,
        PersistedManualTransitionKind::Zoom => TransitionKind::Zoom,
        PersistedManualTransitionKind::Fly => TransitionKind::Fly,
        PersistedManualTransitionKind::FlyRotate => TransitionKind::FlyRotate,
        PersistedManualTransitionKind::Cube => TransitionKind::Cube,
        PersistedManualTransitionKind::CubeZoom => TransitionKind::CubeZoom,
        PersistedManualTransitionKind::CrossZoom => TransitionKind::CrossZoom,
        PersistedManualTransitionKind::Merge => TransitionKind::Merge,
        PersistedManualTransitionKind::Stinger { slot, .. } => TransitionKind::Stinger(
            fm_switcher::StingerSlotId::new(slot)
                .ok_or_else(|| AppFailure("invalid persisted manual Stinger slot".into()))?,
        ),
    };
    let interval_start = TBarPosition::new(state.interval_start_basis_points)
        .ok_or_else(|| AppFailure("invalid persisted manual-transition interval start".into()))?;
    let position = TBarPosition::new(state.position_basis_points)
        .ok_or_else(|| AppFailure("invalid persisted manual-transition position".into()))?;
    Ok(
        TBarState::restore(kind, state.from_id, state.to_id, interval_start, position)
            .with_clip_frames(state.kind.clip_frames()),
    )
}

fn persisted_engine_receipt(
    (key, receipt): &(IdempotencyKey, CommandReceipt<EngineAcceptance>),
) -> IdempotencyReceipt {
    match receipt {
        CommandReceipt::Accepted {
            command_id,
            acceptance,
        } => IdempotencyReceipt::accepted(
            key.as_str(),
            command_id.as_str(),
            acceptance.revision.get(),
            acceptance.result.target_frame.get(),
        ),
        CommandReceipt::Rejected {
            command_id,
            rejection,
        } => IdempotencyReceipt::rejected(
            key.as_str(),
            command_id.as_str(),
            rejection.current_revision.get(),
            rejection.rejection.code.as_str(),
            &rejection.rejection.message,
            rejection.rejection.retryable,
        ),
    }
}

fn persisted_result(
    command: &CommandMessage,
    result: &CommandResult,
) -> AppResult<IdempotencyReceipt> {
    match result {
        CommandResult::Accepted {
            id,
            revision,
            scheduled_frame: Some(frame),
        } => Ok(IdempotencyReceipt::accepted(
            &command.idempotency_key,
            id,
            *revision,
            *frame,
        )),
        CommandResult::Accepted {
            scheduled_frame: None,
            ..
        } => Err(AppFailure("accepted engine command has no scheduled frame".into()).into()),
        CommandResult::Rejected {
            id,
            code,
            message,
            current_revision,
            retryable,
            ..
        } => Ok(IdempotencyReceipt::rejected(
            &command.idempotency_key,
            id,
            *current_revision,
            code,
            message,
            *retryable,
        )),
    }
}

fn runtime_receipt(
    receipt: &IdempotencyReceipt,
) -> AppResult<(IdempotencyKey, CommandReceipt<EngineAcceptance>)> {
    let command_id = CommandId::new(receipt.command_id());
    let runtime = match receipt.outcome() {
        ReceiptOutcome::Accepted {
            revision,
            target_frame,
        } => CommandReceipt::Accepted {
            command_id,
            acceptance: AcceptedReceipt {
                revision: Revision::new(*revision),
                result: EngineAcceptance {
                    target_frame: (*target_frame).into(),
                },
            },
        },
        ReceiptOutcome::Rejected {
            current_revision,
            code,
            message,
            retryable,
        } => CommandReceipt::Rejected {
            command_id,
            rejection: RejectedReceipt {
                rejection: {
                    let code = runtime_rejection_code(code)?;
                    // Persisted receipts keep code and message; the reasons
                    // a code alone implies are restored (Protocol 2.45).
                    let rejection = Rejection::new(code, message).retryable(*retryable);
                    match fm_command::RefusalReason::implied_by(code, *current_revision) {
                        Some(reason) => rejection.with_reason(reason),
                        None => rejection,
                    }
                },
                current_revision: Revision::new(*current_revision),
            },
        },
    };
    Ok((IdempotencyKey::new(receipt.key()), runtime))
}

fn runtime_rejection_code(code: &str) -> AppResult<RejectionCode> {
    match code {
        "permission_denied" => Ok(RejectionCode::PermissionDenied),
        "deadline_exceeded" => Ok(RejectionCode::DeadlineExceeded),
        "revision_conflict" => Ok(RejectionCode::RevisionConflict),
        "invalid_command" => Ok(RejectionCode::InvalidCommand),
        "not_found" => Ok(RejectionCode::NotFound),
        "conflict" => Ok(RejectionCode::Conflict),
        "unavailable" => Ok(RejectionCode::Unavailable),
        "resource_exhausted" => Ok(RejectionCode::ResourceExhausted),
        "internal" => Ok(RejectionCode::Internal),
        _ => Err(AppFailure(format!("project contains unknown rejection code `{code}`")).into()),
    }
}

fn session_rejection(
    command: &CommandMessage,
    error: &SessionError,
    current_revision: u64,
) -> CommandResult {
    let (code, retryable, reason) = match error {
        SessionError::Authorization(_) => (
            "permission_denied",
            false,
            Some(fm_protocol::RefusalReason::PermissionDenied),
        ),
        SessionError::ProtocolMismatch { .. } => ("protocol_mismatch", false, None),
        SessionError::CommandTooLarge { .. }
        | SessionError::TooManyInflightCommands
        | SessionError::InboundRateLimited
        | SessionError::TitlePreviewRateLimited
        | SessionError::OutboundRateLimited => (
            "resource_exhausted",
            true,
            Some(fm_protocol::RefusalReason::RateLimited),
        ),
        _ => ("session_error", false, None),
    };
    CommandResult::Rejected {
        id: command.id.clone(),
        code: code.into(),
        message: error.to_string(),
        fields: Vec::new(),
        current_revision,
        retryable,
        reason,
    }
}

fn diagnostics_response(
    control: &SharedControl,
    request: DiagnosticsRequest,
    source_sync: Vec<fm_protocol::SourceSyncSample>,
    health: fm_protocol::DaemonHealth,
) -> AppResult<DiagnosticsResponse> {
    let diagnostics = control.borrow().diagnostics();
    Ok(DiagnosticsResponse {
        protocol: request.protocol,
        request_id: request.request_id,
        engine: diagnostics.engine,
        current_revision: diagnostics.current_revision,
        oldest_retained_revision: diagnostics.oldest_retained_revision,
        newest_retained_revision: diagnostics.newest_retained_revision,
        subscriber_count: u32::try_from(diagnostics.subscriber_count)?,
        retained_events_limit: u32::try_from(diagnostics.limits.retained_events)?,
        subscriber_limit: u32::try_from(diagnostics.limits.max_subscribers)?,
        subscriber_queue_limit: u32::try_from(diagnostics.limits.subscriber_queue)?,
        source_sync,
        health,
    })
}

fn structured_session_error(error: &SessionError) -> StructuredError {
    let (code, retryable) = match error {
        SessionError::Authorization(_) => ("permission_denied", false),
        SessionError::ProtocolMismatch { .. } => ("protocol_mismatch", false),
        SessionError::CommandTooLarge { .. }
        | SessionError::InboundRateLimited
        | SessionError::TitlePreviewRateLimited
        | SessionError::OutboundRateLimited => ("resource_exhausted", true),
        _ => ("session_error", false),
    };
    StructuredError {
        code: code.into(),
        message: error.to_string(),
        fields: Vec::new(),
        retryable,
    }
}

#[cfg(test)]
fn write_session_message(
    writer: &mut TcpStream,
    session: &mut Session,
    message: &WireMessage,
) -> AppResult<()> {
    let line = encode_line(message)?;
    session.queue_outbound(line.len(), now_millis()?)?;
    write_client_bytes(writer, line.as_bytes())?;
    session.outbound_delivered()?;
    Ok(())
}

#[cfg(test)]
fn write_session_error(
    writer: &mut TcpStream,
    session: &mut Session,
    code: &str,
    message: &str,
) -> AppResult<()> {
    write_session_message(writer, session, &error_message(code, message))
}

#[cfg(test)]
fn write_error(writer: &mut TcpStream, code: &str, message: &str) -> AppResult<()> {
    write_message(writer, &error_message(code, message))
}

fn error_message(code: &str, message: &str) -> WireMessage {
    WireMessage::Error(ErrorMessage {
        request_id: None,
        current_revision: None,
        error: StructuredError {
            code: code.into(),
            message: message.into(),
            fields: Vec::new(),
            retryable: false,
        },
    })
}

fn shutdown_message() -> WireMessage {
    WireMessage::Error(ErrorMessage {
        request_id: None,
        current_revision: None,
        error: StructuredError {
            code: "server_shutting_down".into(),
            message: "server is shutting down".into(),
            fields: Vec::new(),
            retryable: true,
        },
    })
}

#[cfg(test)]
fn write_message(writer: &mut TcpStream, message: &WireMessage) -> AppResult<()> {
    let line = encode_line(message)?;
    write_client_bytes(writer, line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
fn write_client_bytes(writer: &mut TcpStream, bytes: &[u8]) -> AppResult<()> {
    let mut pending = PendingWrite::new(bytes.to_vec());
    while !pending.write_once(writer).map_err(client_write_error)? {}
    writer.flush().map_err(client_write_error)?;
    Ok(())
}

struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
}

impl PendingWrite {
    fn new(bytes: Vec<u8>) -> Self {
        assert!(!bytes.is_empty(), "encoded records are nonempty");
        Self { bytes, offset: 0 }
    }

    fn write_once(&mut self, writer: &mut impl Write) -> std::io::Result<bool> {
        let written = writer.write(&self.bytes[self.offset..])?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        self.offset += written;
        Ok(self.offset == self.bytes.len())
    }

    fn started(&self) -> bool {
        self.offset != 0
    }
}

#[cfg(test)]
fn client_write_error(error: std::io::Error) -> Box<dyn Error> {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        Box::new(ClientWriteTimeout(error.kind()))
    } else {
        Box::new(error)
    }
}

#[test]
fn pending_write_preserves_offset_across_partial_writes() {
    struct PartialWriter {
        bytes: Vec<u8>,
        limit: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let written = bytes.len().min(self.limit);
            self.bytes.extend_from_slice(&bytes[..written]);
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let expected = b"encoded record";
    let mut pending = PendingWrite::new(expected.to_vec());
    let mut writer = PartialWriter {
        bytes: Vec::new(),
        limit: 3,
    };

    assert!(!pending.write_once(&mut writer).unwrap());
    assert!(!pending.write_once(&mut writer).unwrap());
    assert!(!pending.write_once(&mut writer).unwrap());
    assert!(!pending.write_once(&mut writer).unwrap());
    assert!(pending.write_once(&mut writer).unwrap());
    assert_eq!(writer.bytes, expected);
}

fn handshake_code(error: &HandshakeError<Infallible>) -> &'static str {
    match error {
        HandshakeError::ProtocolMismatch => "protocol_mismatch",
        HandshakeError::NotReady(_) => "not_ready",
        HandshakeError::DevelopmentPrincipalDenied | HandshakeError::RoleDenied(_) => {
            "permission_denied"
        }
        HandshakeError::InvalidControlSync => "invalid_control_sync",
        HandshakeError::Control(never) => match *never {},
    }
}

#[cfg(test)]
struct MessageReader {
    stream: TcpStream,
    decoder: LineDecoder,
    pending: VecDeque<WireMessage>,
}

#[cfg(test)]
impl MessageReader {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            decoder: LineDecoder::new(),
            pending: VecDeque::new(),
        }
    }

    fn read_message_with_idle(
        &mut self,
        mut idle: impl FnMut() -> AppResult<bool>,
    ) -> AppResult<Option<WireMessage>> {
        if let Some(message) = self.pending.pop_front() {
            return Ok(Some(message));
        }
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            let read = match self.stream.read(&mut chunk) {
                Ok(read) => read,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if !idle()? {
                        return Ok(None);
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if read == 0 {
                let decoder = std::mem::replace(&mut self.decoder, LineDecoder::new());
                decoder.finish().map_err(ClientProtocolError)?;
                return Ok(None);
            }
            self.pending.extend(
                self.decoder
                    .push(&chunk[..read])
                    .map_err(ClientProtocolError)?,
            );
            if let Some(message) = self.pending.pop_front() {
                return Ok(Some(message));
            }
            if !idle()? {
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
fn is_client_disconnect(error: &(dyn Error + 'static)) -> bool {
    if error.downcast_ref::<ClientWriteTimeout>().is_some() {
        return true;
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::UnexpectedEof
        );
    }
    error.source().is_some_and(is_client_disconnect)
}

#[cfg(test)]
fn is_client_protocol_error(error: &(dyn Error + 'static)) -> bool {
    error.downcast_ref::<ClientProtocolError>().is_some()
}

fn is_client_session_termination(error: &(dyn Error + 'static)) -> bool {
    if let Some(error) = error.downcast_ref::<SessionError>() {
        return matches!(
            error,
            SessionError::OutboundRateLimited
                | SessionError::Disconnected(DisconnectReason::SlowClient)
        );
    }
    error.source().is_some_and(is_client_session_termination)
}

fn development_principal() -> AppResult<Principal> {
    Ok(Principal::development(
        UserId::new("local-operator")?,
        SessionId::new("local-session")?,
        [AuthRole::Admin],
    ))
}

fn clock_domain() -> ClockDomainId {
    ClockDomainId::new(NonZeroU128::MIN)
}

fn required_routing(value: Option<InputId>, field: &'static str) -> AppResult<InputId> {
    value.ok_or_else(|| AppFailure(format!("project is missing {field} routing")).into())
}

fn now_millis() -> AppResult<u64> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    u64::try_from(millis)
        .map_err(|_| AppFailure("system time exceeds protocol range".into()).into())
}

#[derive(Debug)]
struct AppFailure(String);

#[cfg(test)]
#[derive(Debug)]
struct ClientWriteTimeout(std::io::ErrorKind);

#[cfg(test)]
#[derive(Debug)]
struct ClientProtocolError(CodecError);

#[cfg(test)]
impl fmt::Display for ClientWriteTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "client write timed out: {:?}", self.0)
    }
}

#[cfg(test)]
impl Error for ClientWriteTimeout {}

#[cfg(test)]
impl fmt::Display for ClientProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "client protocol error: {}", self.0)
    }
}

#[cfg(test)]
impl Error for ClientProtocolError {}

impl fmt::Display for AppFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for AppFailure {}

/// The display presenter could not open (no window system, window, or GPU)
/// before it started serving, so the daemon may still serve headless.
#[derive(Debug)]
#[cfg_attr(not(feature = "display-surface"), allow(dead_code))]
struct PresenterUnavailable(String);

impl fmt::Display for PresenterUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "display presenter unavailable: {}", self.0)
    }
}

impl Error for PresenterUnavailable {}

/// The `FREEMIXD_DISPLAY_PRESENTER` record a daemon started with
/// `--display-presenter-if-available` emits when it serves without one.
fn presenter_unavailable_record(error: &str) -> String {
    let error = error
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    format!("FREEMIXD_DISPLAY_PRESENTER\tv=1\tstate=unavailable\terror={error}")
}

#[cfg(test)]
mod tests;
