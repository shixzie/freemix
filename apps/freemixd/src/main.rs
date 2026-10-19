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
#[cfg(feature = "native-media")]
use fm_engine::FrameResult;
use fm_engine::{
    Engine, EngineAcceptance, EngineInputAudioStripState, EngineRestoreState, EngineSnapshot,
    EngineTitle, ShowState,
};
#[cfg(feature = "native-media")]
use fm_io_api::display::DisplayOutputSource;
use fm_io_api::display::{
    DisplayOutputConfig, DisplayOutputSet, DisplayOutputSlot, DisplaySelector,
};
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
#[cfg(feature = "native-media")]
use fm_protocol::{
    AUDIO_METER_LEVEL_SCALE, AudioMeterChannel, InputAudioMeters, MAX_STREAM_LOCAL_SEGMENTS,
    MAX_STREAM_LOCAL_TARGET_DURATION_MS, MAX_STREAM_SAMPLES, StreamLocalStatus,
    StreamRealizedState, StreamStatusSample, WireInputId,
};
use fm_protocol::{
    AudioMetersMessage, CURRENT_PROTOCOL_VERSION, CapabilityReportSummary, CommandMessage,
    CommandPayload, CommandResult, DiagnosticsRequest, DiagnosticsResponse, ErrorMessage,
    EventCursor, HandshakeOutcome as ProtocolHandshakeOutcome, HandshakeRequest, HandshakeResponse,
    HeartbeatMessage, MonitorTap, ProtocolVersion, RecordingStatus, ResumeCursor,
    RuntimeEventMessage, ServerHello, ServerIdentity, StreamStatus, StreamStatusMessage,
    StructuredError, WireMessage, WireStreamTargetId, choose_handshake_outcome, encode_line,
};
#[cfg(test)]
use fm_protocol::{CodecError, EventMessage, HeartbeatAcknowledgementMessage, LineDecoder};
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

// The free-space alert is constructed only by the native render loop; the
// preflight and its constants are used in every build.
mod cue_clock;
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
mod disk;
mod grading;
mod hls_server;
mod journal;
mod latest_record;
mod media_clock;
mod monitor_tap;
#[cfg(feature = "native-media")]
mod native_captures;
#[cfg(feature = "native-media")]
mod native_catalogue;
#[cfg(feature = "native-media")]
mod native_files;
#[cfg(feature = "native-media")]
mod native_streams;
#[cfg(feature = "native-media")]
mod native_system_audio;
#[cfg(feature = "native-media")]
mod native_titles;
// The inventory host and status records are wired into native daemons only;
// the parser and validation compile (and are unit-tested) in every build.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
mod display_outputs;
mod non_native_sessions;
mod playlist_clock;
// The network preflight is wired only into the native daemon; every helper
// still compiles (and is unit-tested) in every build.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
mod preflight;
#[cfg(feature = "display-surface")]
mod program_surface;
#[cfg(feature = "native-media")]
mod recording_session;
mod scene_preview;
mod scene_render;
mod status_listener;
#[cfg(feature = "native-media")]
mod stinger_mutation;
#[cfg(feature = "native-media")]
mod stream_transport;
mod title_preview;
mod title_render;
mod web;

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
    stream::{EncoderSettings, StreamDestination},
};
#[cfg(feature = "native-media")]
use fm_codec_image::{StillDecodeLimits, decode_still, sniff_still_format};
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
    AbrLadder, AbrVariant, AudioRendition, ColorDescription, DestinationConfig, DestinationEnqueue,
    DestinationId, DestinationRenditions, DestinationState, Endpoint, EnqueueStatus, FrameRate,
    HlsLadder, HlsLadderRung, HlsPlaylistMetadata, NetworkTelemetry, OutputPacket, OutputProtocol,
    OutputSet, PollEvent, QueueCapacity, ReconnectPolicy, RenditionId, RenditionPlan,
    RenditionPlanner, RenditionProfile, SrtOptions, TimingProfile, VideoRendition,
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

const DEFAULT_LISTEN: &str = "127.0.0.1:0";

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
#[allow(clippy::struct_excessive_bools)] // each bool is a distinct native-mode operator or media option
struct NativeDaemon {
    pacer: FramePacer,
    pacer_start_offset: Duration,
    origin: Instant,
    latest_output: Option<NativeTexture>,
    latest_project_outputs: Vec<NativeOutputFrame>,
    master: NativeMasterRuntime,
    project_plan: NativeProjectPlan,
    /// Compiled grades per engine revision and their GPU uploads, attached
    /// to [`Self::project_plan`] before every render.
    grade_cache: grading::GradeCache,
    prepared_grades: Option<(
        Arc<fm_engine::CompiledProjectGrades>,
        freemixd::native_media::NativeProjectGrades,
    )>,
    playback: freemixd::native_media::NativeSourcePlayback,
    stingers: freemixd::native_media::NativeSourcePlayback,
    projected_frame: Option<FrameResult>,
    runtime: Arc<NativeMediaRuntime>,
    resolved_sources: Arc<Vec<NativeResolvedSource>>,
    assets_root: PathBuf,
    pending_stinger_mutation: Option<NativeStingerMutation>,
    stinger_retirements: NativeStingerRetirements,
    /// Realization state of every configured recorder slot: slot 0 is
    /// `--record-program` and slot 1 is `--record-program-2`. Each slot owns
    /// its own segment, sidecar, failure latch, and report.
    recorders: [ProgramRecording; RECORDER_SLOTS],
    /// Configured `--record-program` output path; recording configuration is
    /// independent of whether a segment is currently open.
    record_program: Option<PathBuf>,
    /// Configured `--record-program-2` output path: the second recorder slot,
    /// configured exactly like [`Self::record_program`] and deriving its own
    /// segment names.
    record_program_2: Option<PathBuf>,
    /// Optional project output selection for each recorder slot (`None` selects
    /// the first realized project output, or the unbound output when the
    /// project declares no outputs).
    record_outputs: [Option<fm_types::OutputId>; RECORDER_SLOTS],
    /// Whether each segment also writes a `--record-wav` linear-PCM sidecar
    /// next to the MP4. Applies to every configured recorder slot.
    record_wav: bool,
    /// Optional per-slot frame-boundary segment rotation target in seconds
    /// (`--record-segment-seconds` is slot 0 and `--record-segment-seconds-2`
    /// is slot 1). A slot whose entry is `None` never rotates automatically,
    /// even when the other slot is configured to.
    record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    /// Optional per-slot `libx264` video bitrate in kbps
    /// (`--record-bitrate-kbps` is slot 0 and `--record-bitrate-kbps-2` is
    /// slot 1). `None` keeps that slot's encoder default.
    record_bitrates: [Option<u32>; RECORDER_SLOTS],
    /// Optional per-slot AAC audio bitrate in kbps
    /// (`--record-audio-bitrate-kbps` is slot 0 and
    /// `--record-audio-bitrate-kbps-2` is slot 1). `None` keeps that slot's
    /// encoder default.
    record_audio_bitrates: [Option<u32>; RECORDER_SLOTS],
    /// Optional `--record-fallback-dir` plus the segment names still being
    /// started off the loop; every slot's name derivation consults it.
    segment_names: recording_session::SegmentNames,
    /// Hysteretic free-space alert for the configured program outputs.
    disk_alert: disk::DiskSpaceAlert,
    /// Latest successfully sampled free bytes across the configured outputs:
    /// every configured recording output's parent and every local HLS output
    /// directory; `None` until the first sample succeeds.
    disk_free_bytes: Option<u64>,
    /// When the last disk sample was attempted; `None` makes the first sample
    /// evaluate immediately instead of after one full interval.
    disk_sampled_at: Option<Instant>,
    /// Absolute output directory of every authored local HLS stream target, in
    /// authored order; empty when the show has no local outputs. The HLS sink
    /// creates each leaf directory later, so preflight and sampling resolve the
    /// nearest existing ancestor. Bounded by the engine's five-target cap.
    local_output_dirs: Vec<PathBuf>,
    /// Latest sampled local HLS output health per destination slot, indexed by
    /// the slot's one-based [`DestinationId`] minus one. Network targets and
    /// local outputs whose playlist cannot currently be read stay `None`, and
    /// their next wire sample carries no local field.
    local_output_status: [Option<StreamLocalStatus>; fm_switcher::MAX_STREAM_COUNT],
    /// The `--hls-listen` route table, when the listener runs: every stream
    /// destination edit (OR-006) republishes it from the realized
    /// destinations, so a local HLS destination added while running is served
    /// at once and a removed one is not served any longer.
    hls_routes: Option<hls_server::HlsRouteTable>,
    /// When the last local HLS output sample was attempted; `None` makes the
    /// first sample evaluate immediately instead of after one full interval.
    local_output_sampled_at: Option<Instant>,
    /// Segments stopped off the render path, awaiting bounded finalization.
    recording_finalizers: Vec<ProgramRecordingFinalizer>,
    streams: Vec<NativeProgramStream>,
    /// One group per distinct encoder-relevant identity (output protocol plus
    /// authored video bitrate): its members share one readback owner, one
    /// packed pair per frame, and one sequence cursor, fanned out through the
    /// shared [`OutputSet`]'s rendition planner.
    stream_groups: Vec<StreamRenditionGroup>,
    /// Shared failover state machine for every stream target; per-target
    /// transports stay owned by each [`NativeProgramStream`].
    output_set: OutputSet,
    /// `StreamStart` acceptances waiting for the next frame boundary.
    pending_stream_starts: Vec<SwitcherStreamTargetId>,
    streams_finalized: bool,
    telemetry: NativeRuntimeTelemetry,
    telemetry_emitted: bool,
    audio_meter_sequence: u64,
    pending_audio_meters: Option<AudioMetersMessage>,
    /// The headphone output: the monitor bus on a local audio device at its
    /// own level (AU-001).
    monitor_output: freemixd::monitor_output::MonitorOutputSlot,
    stream_status_sequence: u64,
    pending_stream_status: Option<StreamStatusMessage>,
    /// Encoded-fallback Program monitor tap; see [`NativeMonitorTap`].
    monitor: NativeMonitorTap,
    /// Title inputs realized as live source frames rendered off the loop;
    /// see [`native_titles`].
    titles: native_titles::NativeTitleSources,
    /// Transport-controlled media-file inputs decoded off the loop; see
    /// [`native_files`].
    files: native_files::NativeMediaFiles,
    /// Network stream inputs and their receivers (IN-015).
    network: native_streams::NativeNetworkStreams,
    /// System audio inputs and their audio-only receivers (AU-011).
    system_audio: native_system_audio::NativeSystemAudio,
    /// Local camera and capture-card inputs and their device backend
    /// (IN-001); `None` without a capture backend.
    captures: Option<native_captures::NativeCaptures>,
    /// The source realized for every input, which live catalogue changes
    /// follow (see [`native_catalogue`]).
    catalogue: std::collections::BTreeMap<InputId, native_catalogue::NativeSourceKey>,
    /// Still images of inputs added live, decoding off the loop.
    stills: Vec<native_catalogue::PendingStill>,
    /// The project's picture size, in pixels.
    picture_size: (u32, u32),
    #[cfg(target_os = "macos")]
    cameras: NativeCameraInputs,
    #[cfg(target_os = "macos")]
    camera_telemetry_frozen: bool,
    /// Borderless fullscreen display outputs, one presentation per slot
    /// window (empty without the windowed presenter).
    #[cfg(feature = "display-surface")]
    displays: Vec<program_surface::ProgramPresentation>,
    /// The windowed presenter's live configuration and report channel.
    #[cfg(feature = "display-surface")]
    windowed: Option<program_surface::WindowedDisplays>,
    /// Display outputs presented to the diagnostic inventory host.
    display_inventory: Option<display_outputs::InventoryDisplayHost>,
    /// What the presenter was last told to show (OR-011).
    display_sync: display_outputs::DisplayOutputSync,
    /// Which realized frames the display outputs present.
    display_pacing: display_outputs::PresentationPacing,
    /// The Preview composite, rendered only while a display output shows
    /// Preview.
    latest_preview: Option<NativeTexture>,
    /// Whether the last Preview composite failed (reported once per run of
    /// failures).
    preview_failed: bool,
}

#[cfg(feature = "native-media")]
struct NativeSourceResolution {
    sources: Vec<NativeResolvedSource>,
    #[cfg(target_os = "macos")]
    cameras: NativeCameraInputs,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
struct NativeCameraInput {
    input: InputId,
    source: Option<CameraVideoSource>,
    worker: Option<NativeCameraWorker>,
    supervisor: Arc<Mutex<CameraSupervisorState>>,
    recovery_policy: CameraRecoveryPolicy,
    ingested_frames: u64,
    ingest_failed: u64,
    preflight_depth: u64,
    preflight_discarded: u64,
    last_ingested_sequence: Option<u64>,
    last_ingested_discontinuity: bool,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Default)]
struct CameraFrameSlot {
    frame: Option<CpuVideoFrame>,
    replacements: u64,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
struct CameraSupervisorState {
    frame: CameraFrameSlot,
    snapshot: CameraWorkerSnapshot,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl CameraFrameSlot {
    fn replace(&mut self, mut frame: CpuVideoFrame) {
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

    fn take(&mut self) -> Option<CpuVideoFrame> {
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
        frame
            .with_metadata(metadata)
            .expect("existing camera metadata remains valid")
    } else {
        frame
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Clone, Copy)]
struct CameraRecoveryPolicy {
    max_attempts: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    rearm_backoff: Duration,
    shutdown_timeout: Duration,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CameraRecoveryOutcome {
    #[default]
    Never,
    Pending,
    Recovered,
    Exhausted,
    Rearming,
    WorkerFailed,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
struct NativeCameraWorker {
    handle: Option<JoinHandle<CameraWorkerResult>>,
    cancel: Arc<AtomicBool>,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
struct CameraWorkerResult {
    failure: Option<IoError>,
    cleanup_failure: Option<IoError>,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Clone, Copy)]
struct CameraWorkerSnapshot {
    telemetry: CameraTelemetry,
    lifecycle: LifecycleState,
    health: EndpointHealthState,
    recovery_attempts: u64,
    recovery_successes: u64,
    recovery_exhausted: u64,
    recovery_worker_failures: u64,
    recovery_outcome: CameraRecoveryOutcome,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl CameraWorkerSnapshot {
    fn running(source: &CameraVideoSource) -> Self {
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
struct NativeCameraInputs {
    inputs: Vec<NativeCameraInput>,
    telemetry_emitted: bool,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl NativeCameraInputs {
    fn poll_into(
        &mut self,
        runtime: &NativeMediaRuntime,
        playback: &mut freemixd::native_media::NativeSourcePlayback,
    ) -> AppResult<()> {
        self.poll_with(|input, frame| {
            runtime.ingest_live_video_frame_blocking(playback, input, frame)?;
            Ok(())
        })
    }

    fn poll_with(
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

    fn collect_finished_worker(camera: &mut NativeCameraInput) -> AppResult<()> {
        let finished = camera
            .worker
            .as_ref()
            .and_then(|worker| worker.handle.as_ref())
            .is_some_and(JoinHandle::is_finished);
        if !finished {
            return Ok(());
        }
        let mut worker = camera.worker.take().expect("camera worker was checked");
        let handle = worker.handle.take().expect("camera worker is owned");
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

    fn start_workers(&mut self) -> AppResult<()> {
        for camera in &mut self.inputs {
            let source = camera
                .source
                .take()
                .expect("started camera source is available");
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

    fn cleanup_startup_sources(&mut self, timeout: Duration) -> AppResult<()> {
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

    fn source_telemetry(&self) -> Vec<NativeCameraSourceTelemetry> {
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

    fn emit_source_telemetry(&mut self) -> Option<Vec<NativeCameraSourceTelemetry>> {
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

    fn mark_preflight_frames_ingested(&mut self) {
        for camera in &mut self.inputs {
            camera.ingested_frames = camera
                .ingested_frames
                .saturating_add(camera.preflight_depth);
            camera.preflight_depth = 0;
        }
    }

    fn discard_preflight_frames(&mut self) {
        for camera in &mut self.inputs {
            camera.preflight_discarded = camera
                .preflight_discarded
                .saturating_add(camera.preflight_depth);
            camera.preflight_depth = 0;
        }
    }

    fn shutdown(&mut self) -> AppResult<()> {
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
fn recoverable_camera_error(error: &IoError) -> bool {
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
fn update_camera_worker_snapshot_inner(
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
struct NativeCameraTelemetry {
    configured_sources: u64,
    frames_received: u64,
    frames_ingested: u64,
    native_dropped: u64,
    queue_dropped: u64,
    queue_depth: u64,
    queue_peak_depth: u64,
    continuity_rejected: u64,
    recovery_timeout_discarded: u64,
    terminal_error_discarded: u64,
    terminal_trigger_discarded: u64,
    ready_delivery_depth: u64,
    ready_delivery_discarded: u64,
    cancellation_discarded: u64,
    supervisor_slot_replaced: u64,
    supervisor_slot_depth: u64,
    ingest_failed: u64,
    preflight_depth: u64,
    preflight_discarded: u64,
    recovery_attempts: u64,
    recovery_successes: u64,
    recovery_exhausted: u64,
    recovery_worker_failures: u64,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeCameraSourceTelemetry {
    input: InputId,
    lifecycle: LifecycleState,
    health: EndpointHealthState,
    frames_received: u64,
    frames_ingested: u64,
    native_dropped: u64,
    queue_dropped: u64,
    queue_depth: u64,
    queue_peak_depth: u64,
    continuity_rejected: u64,
    recovery_timeout_discarded: u64,
    terminal_error_discarded: u64,
    terminal_trigger_discarded: u64,
    ready_delivery_depth: u64,
    ready_delivery_discarded: u64,
    cancellation_discarded: u64,
    supervisor_slot_replaced: u64,
    supervisor_slot_depth: u64,
    ingest_failed: u64,
    preflight_depth: u64,
    preflight_discarded: u64,
    recovery_attempts: u64,
    recovery_successes: u64,
    recovery_exhausted: u64,
    recovery_worker_failures: u64,
    recovery_outcome: CameraRecoveryOutcome,
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
impl NativeCameraSourceTelemetry {
    fn diagnostic(self) -> String {
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
    fn accounted_frames(self) -> u64 {
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
fn aggregate_camera_telemetry(sources: &[NativeCameraSourceTelemetry]) -> NativeCameraTelemetry {
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
fn sort_camera_source_telemetry(sources: &mut [NativeCameraSourceTelemetry]) {
    sources.sort_by_key(|source| source.input);
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
const fn lifecycle_label(state: LifecycleState) -> &'static str {
    match state {
        LifecycleState::Closed => "closed",
        LifecycleState::Open => "open",
        LifecycleState::Running => "running",
        LifecycleState::Lost => "lost",
        LifecycleState::Recovering => "recovering",
    }
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
const fn health_label(state: EndpointHealthState) -> &'static str {
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

#[cfg(feature = "native-media")]
struct NativeRuntimeTelemetry {
    origin: Instant,
    gpu_adapter: String,
    gpu_backend: NativeBackend,
    metrics: MetricStore,
    metric_errors: u64,
    gpu_support: fm_gpu::NativeFullscreenTimingSupport,
    gpu_pending_samples: u64,
    gpu_dropped_samples: u64,
    gpu_unavailable_samples: u64,
    audio_retained_blocks: u64,
    audio_peak_retained_blocks: u64,
    audio_retained_samples: u64,
    audio_peak_retained_samples: u64,
    audio_retained_bytes: u64,
    audio_peak_retained_bytes: u64,
    audio_reservation_requests: u64,
    audio_reserved_blocks: u64,
    audio_peak_reserved_blocks: u64,
    audio_reserved_samples: u64,
    audio_peak_reserved_samples: u64,
    audio_reserved_bytes: u64,
    audio_peak_reserved_bytes: u64,
    audio_source_stalls: u64,
    audio_positioned_blocks: u64,
    audio_positioned_samples: u64,
    audio_leading_silence_samples: u64,
    audio_eos_padding_blocks: u64,
    audio_eos_padding_samples: u64,
    audio_sink_depth: u64,
    audio_sink_peak_depth: u64,
    audio_sink_dropped: u64,
    camera: NativeCameraTelemetry,
    recorder_configured: bool,
    recorder_outstanding_pairs: u64,
    recorder_peak_outstanding_pairs: u64,
    recorder_retained_bytes: u64,
    recorder_peak_retained_bytes: u64,
    stream_enqueued_pairs_total: u64,
    stream_dropped_pairs_total: u64,
    /// Encoded Program monitor frames published (Protocol 2.21 fallback).
    monitor_frames_published: u64,
    /// Program monitor readbacks or encodes that failed and were skipped;
    /// a failed monitor tap never fails the frame tick.
    monitor_frames_failed: u64,
    /// Latest successfully sampled free bytes across the configured outputs:
    /// the recording output's parent and every local HLS output directory;
    /// `None` until the first sample.
    disk_free_bytes: Option<u64>,
    /// Disk free-space alert state at the latest sample; `None` until the
    /// first sample evaluated.
    disk_alert: Option<AlertState>,
}

#[cfg(feature = "native-media")]
impl NativeRuntimeTelemetry {
    const RETAINED_SAMPLES: usize = 256;

    fn new(origin: Instant, adapter: &fm_gpu::NativeAdapterInfo) -> Self {
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
            monitor_frames_published: 0,
            monitor_frames_failed: 0,
            disk_free_bytes: None,
            disk_alert: None,
        }
    }

    fn observe_host_lateness(&mut self, deadline: Instant, observed: Instant) {
        let milliseconds = observed.saturating_duration_since(deadline).as_secs_f64() * 1_000.0;
        let time = self.monotonic_millis();
        let result =
            self.metrics
                .observe_histogram(Metric::LatencyMilliseconds, time, milliseconds);
        self.record_metric_result(result);
    }

    fn observe_gpu(&mut self, context: &NativeContext) {
        let telemetry = context.take_fullscreen_timing_telemetry();
        self.gpu_support = telemetry.support;
        self.gpu_pending_samples = saturating_u64(telemetry.pending_samples);
        self.gpu_dropped_samples = telemetry.dropped_samples;
        self.gpu_unavailable_samples = telemetry.unavailable_samples;
        for sample in telemetry.completed_samples {
            let milliseconds = sample.duration_nanoseconds() / 1_000_000.0;
            let time = self.monotonic_millis();
            let result =
                self.metrics
                    .observe_histogram(Metric::GpuTimeMilliseconds, time, milliseconds);
            self.record_metric_result(result);
        }
    }

    fn observe_audio(&mut self, master: &NativeMasterRuntime) {
        let audio = master.audio_telemetry();
        self.audio_retained_blocks = saturating_u64(master.retained_blocks());
        self.audio_peak_retained_blocks = self
            .audio_peak_retained_blocks
            .max(self.audio_retained_blocks)
            .max(saturating_u64(audio.peak_retained_blocks));
        self.audio_retained_samples = saturating_u64(master.retained_samples());
        self.audio_peak_retained_samples = self
            .audio_peak_retained_samples
            .max(self.audio_retained_samples)
            .max(saturating_u64(audio.peak_retained_samples));
        self.audio_retained_bytes = saturating_u64(master.retained_bytes());
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
        self.audio_sink_depth = saturating_u64(master.sink_len());
        let sink = master.sink_telemetry();
        self.audio_sink_peak_depth = saturating_u64(sink.high_watermark());
        self.audio_sink_dropped = sink.dropped();
    }

    /// Aggregates the running recorder slots into the whole-run sample.
    ///
    /// Outstanding pairs and retained bytes sum across every running slot and
    /// both peaks track the aggregate. Called only while at least one slot
    /// runs, so an idle daemon keeps its last observation instead of
    /// fabricating a zero sample from no recorders.
    fn observe_recorders<'a>(
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
    fn observe_streams(&mut self, enqueued_pairs: u64, dropped_pairs: u64) {
        self.stream_enqueued_pairs_total = enqueued_pairs;
        self.stream_dropped_pairs_total = dropped_pairs;
    }

    fn emit(&self, presentation: Option<fm_gpu::PresentationTelemetry>) {
        eprintln!("{}", self.diagnostic(presentation));
    }

    fn diagnostic(&self, presentation: Option<fm_gpu::PresentationTelemetry>) -> String {
        let host = self
            .metrics
            .series(Metric::LatencyMilliseconds)
            .histogram_summary()
            .expect("host lateness is a histogram");
        let gpu = self
            .metrics
            .series(Metric::GpuTimeMilliseconds)
            .histogram_summary()
            .expect("GPU time is a histogram");
        let presentation_active = presentation.is_some();
        let presentation = presentation.unwrap_or_default();
        format!(
            "FREEMIXD_TELEMETRY\tv=4\thost_lateness_samples_total={}\thost_lateness_samples_retained={}\thost_lateness_metric_samples_dropped={}\thost_lateness_p50_ms={}\thost_lateness_p95_ms={}\thost_lateness_p99_ms={}\taudio_retained_blocks={}\taudio_observed_peak_retained_blocks={}\taudio_retained_samples={}\taudio_observed_peak_retained_samples={}\taudio_retained_bytes={}\taudio_observed_peak_retained_bytes={}\taudio_reservation_requests={}\taudio_reserved_blocks={}\taudio_observed_peak_reserved_blocks={}\taudio_reserved_samples={}\taudio_observed_peak_reserved_samples={}\taudio_reserved_bytes={}\taudio_observed_peak_reserved_bytes={}\taudio_source_stalls={}\taudio_positioned_blocks={}\taudio_positioned_samples={}\taudio_leading_silence_samples={}\taudio_eos_padding_blocks={}\taudio_eos_padding_samples={}\taudio_sink_depth={}\taudio_sink_peak_depth={}\taudio_sink_dropped={}\tcamera_configured_sources={}\tcamera_frames_received={}\tcamera_frames_ingested={}\tcamera_native_dropped={}\tcamera_queue_depth={}\tcamera_queue_peak_depth={}\tcamera_queue_dropped={}\tcamera_continuity_rejected={}\tcamera_recovery_timeout_discarded={}\tcamera_terminal_error_discarded={}\tcamera_terminal_trigger_discarded={}\tcamera_ready_delivery_depth={}\tcamera_ready_delivery_discarded={}\tcamera_cancellation_discarded={}\tcamera_supervisor_slot_replaced={}\tcamera_supervisor_slot_depth={}\tcamera_ingest_failed={}\tcamera_preflight_depth={}\tcamera_preflight_discarded={}\tcamera_recovery_attempts={}\tcamera_recovery_successes={}\tcamera_recovery_exhausted={}\tcamera_recovery_worker_failures={}\tpresentation_active={}\tpresentation_pending_depth={}\tpresentation_peak_pending_depth={}\tpresentation_dropped={}\trecorder_configured={}\trecorder_outstanding_pairs={}\trecorder_observed_peak_outstanding_pairs={}\trecorder_retained_bytes={}\trecorder_observed_peak_retained_bytes={}\tstream_pairs_enqueued_total={}\tstream_pairs_dropped_total={}\tmonitor_frames_published={}\tmonitor_frames_failed={}\tgpu_backend={:?}\tgpu_adapter={}\tgpu_timing={:?}\tgpu_pass_samples_total={}\tgpu_pass_samples_retained={}\tgpu_pass_metric_samples_dropped={}\tgpu_pass_p50_ms={}\tgpu_pass_p95_ms={}\tgpu_pass_p99_ms={}\tgpu_samples_pending={}\tgpu_samples_dropped={}\tgpu_samples_unavailable={}\tmetric_errors={}\tmetric_samples_dropped={}\tdisk_alert={}\tdisk_free_bytes={}",
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
            self.monitor_frames_published,
            self.monitor_frames_failed,
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

    fn monotonic_millis(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn record_metric_result(&mut self, result: Result<(), fm_observability::MetricError>) {
        if result.is_err() {
            self.metric_errors = self.metric_errors.saturating_add(1);
        }
    }
}

#[cfg(feature = "native-media")]
fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(feature = "native-media")]
fn metric_value(value: Option<f64>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| format!("{value:.3}"))
}

/// Renders the telemetry label for the recording free-space alert; `None`
/// means no sample has ever succeeded.
#[cfg(feature = "native-media")]
const fn disk_alert_label(state: Option<AlertState>) -> &'static str {
    match state {
        Some(AlertState::Firing) => "low",
        Some(AlertState::Inactive) => "normal",
        None => "unknown",
    }
}

#[cfg(feature = "native-media")]
struct NativeProgramRecorder {
    readback: NativeProgramReadback,
    format: RecordFormat,
    sink: RecorderSink,
    /// Pairs captured while the segment is still opening, or that the open
    /// recorder has not accepted yet, in sequence order. Every newer pair
    /// queues behind them, so the segment stays gap-free from its first
    /// sequence; once the backlog drains the recorder is fed directly again.
    backlog: std::collections::VecDeque<PairedFrame>,
    /// Bytes retained by [`Self::backlog`].
    backlog_bytes: usize,
    /// Upper bound for [`Self::backlog_bytes`]: the recorder's own retained
    /// byte budget, so an opening segment never holds more than a running one
    /// may.
    backlog_limit: usize,
    capture: RecorderCapturePolicy,
    finalization_clean: Option<bool>,
    startup_pair_timeout: Duration,
    /// Mux-readiness deadline of a runtime `RecordStart` segment that has not
    /// produced muxed output yet. The command's acknowledgement is held until
    /// this clears, exactly like the former blocking barrier, but the daemon
    /// loop keeps running meanwhile.
    priming: Option<Instant>,
    /// Whether the segment must reach mux readiness once it opens: runtime
    /// starts do, rotations never did.
    prime_on_open: bool,
    /// Parent directory of the open segment, sampled for free space when the
    /// final report is frozen.
    parent: PathBuf,
    /// 1-based recorder slot number this segment belongs to; every report and
    /// failure notice this recorder emits is annotated with it.
    slot: usize,
    /// Configured `libx264` video bitrate for this segment; `None` keeps the
    /// encoder default. Reported in `FREEMIXD_RECORDER`.
    video_bitrate_kbps: Option<u32>,
    /// Configured AAC audio bitrate for this segment; `None` keeps the encoder
    /// default. Reported in `FREEMIXD_RECORDER`.
    audio_bitrate_kbps: Option<u32>,
    /// Estimated recording bitrate, for the free-space preflight.
    bitrate_bits_per_second: u64,
    /// WAV sidecar bitrate with `--record-wav`, which also gives the segment
    /// its WAV session.
    wav_bits_per_second: Option<u64>,
    /// The crash-safe session the open recorder writes into; reported in
    /// `FREEMIXD_RECORDER_SESSION` and watched for failovers.
    session: Option<RecorderSession>,
}

/// The fault-tolerant session behind one open segment.
#[cfg(feature = "native-media")]
struct RecorderSession {
    name: String,
    status: recording_session::SessionStatus,
    failover: recording_session::FailoverWatch,
    remaining: recording_session::RemainingTimeWatch,
}

/// A started recorder and the session it writes into.
#[cfg(feature = "native-media")]
struct OpenedRecorder {
    recorder: Recorder,
    session: RecorderSession,
}

/// Starts a planned session and a recorder writing into it.
///
/// Runs on the open worker for runtime starts and rotations, and inline for
/// configured startup recording. A refused recorder start removes the
/// session's manifests again when no segment was written, so the refused
/// name is reused by the next start exactly like the former refused file.
/// With `--record-wav` the WAV sidecar streams into its own session through
/// [`Recorder::start_with_sinks`], so it fails over with the video.
#[cfg(feature = "native-media")]
fn start_session_recorder(
    planned: recording_session::PlannedSession,
    config: RecordConfig,
) -> Result<OpenedRecorder, String> {
    let recording_session::OpenedSession {
        output,
        wav,
        status,
    } = recording_session::open(planned)?;
    let started = match wav {
        Some(wav) => Recorder::start_with_sinks(Box::new(output), Box::new(wav), config),
        None => Recorder::start_with_sink(Box::new(output), config),
    };
    match started {
        Ok(recorder) => Ok(OpenedRecorder {
            recorder,
            session: RecorderSession {
                name: status.name().to_owned(),
                status,
                failover: recording_session::FailoverWatch::default(),
                remaining: recording_session::RemainingTimeWatch::default(),
            },
        }),
        Err(error) => {
            recording_session::discard_unused(&status);
            Err(error.to_string())
        }
    }
}

/// Where an open segment's pairs go.
///
/// A runtime start spawns `FFmpeg` and waits for it to connect, which takes
/// from hundreds of milliseconds up to the recorder's connect timeout. That
/// happens on a dedicated worker, never on the daemon loop: the segment is
/// [`Self::Opening`] until the worker hands over the started [`Recorder`].
#[cfg(feature = "native-media")]
enum RecorderSink {
    /// The worker is starting the segment's session and `FFmpeg`.
    Opening {
        opened: mpsc::Receiver<Result<OpenedRecorder, String>>,
        /// Failure-notice prefix if the open fails (`start` or `rotate`).
        failure_label: &'static str,
        /// A cancellation requested before the recorder existed; applied as
        /// soon as it arrives.
        cancel_requested: bool,
    },
    /// The recorder is running.
    Open(Box<Recorder>),
    /// The open failed; the failure was already reported and no segment
    /// exists, so no `FREEMIXD_RECORDER` report is emitted.
    Unopened,
}

/// Per-segment options for a segment opened off the daemon loop.
#[cfg(feature = "native-media")]
#[derive(Clone, Copy)]
struct SegmentOpening {
    wav: bool,
    video_bitrate_kbps: Option<u32>,
    audio_bitrate_kbps: Option<u32>,
    /// 1-based recorder slot number.
    slot: usize,
    /// Failure-notice prefix if the open fails (`start` or `rotate`).
    failure_label: &'static str,
    /// Whether the acknowledgement-holding mux-readiness barrier applies.
    prime_on_open: bool,
}

/// Progress of an opening segment, observed without blocking.
#[cfg(feature = "native-media")]
enum OpenPoll {
    /// The segment is open (or was already).
    Open,
    /// The worker is still starting the recorder.
    Pending,
    /// The open failed; the slot must be released and this notice reported.
    Failed(String),
}

#[cfg(feature = "native-media")]
#[derive(Default)]
struct RecorderCapturePolicy {
    first_failure: Option<String>,
    app_capture_failure: bool,
}

#[cfg(feature = "native-media")]
impl RecorderCapturePolicy {
    fn active(&self) -> bool {
        self.first_failure.is_none()
    }

    fn fail(&mut self, failure: String, app_capture_failure: bool) -> Option<(&str, bool)> {
        if self.first_failure.is_some() {
            return None;
        }
        self.first_failure = Some(failure);
        self.app_capture_failure = app_capture_failure;
        Some((
            self.first_failure.as_deref().expect("failure was stored"),
            app_capture_failure,
        ))
    }
}

/// Minimum positive frame count covering at least `seconds` at `rate`:
/// `ceil(seconds * rate.numerator() / rate.denominator())`.
#[cfg(feature = "native-media")]
fn segment_frame_limit(seconds: NonZeroU32, rate: fm_types::FrameRate) -> u64 {
    let frames = u128::from(seconds.get())
        .saturating_mul(u128::from(rate.numerator()))
        .div_ceil(u128::from(rate.denominator()))
        .max(1);
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// Non-drop-frame start timecode for the absolute engine frame index
/// `first_sequence` at `rate`, in the project's nominal frame-rate domain.
///
/// The nominal frames per second is the exact integer rounding of the
/// rational rate, `(numerator + denominator / 2) / denominator`, clamped to
/// the timecode's representable `1..=255`. Components wrap modulo their
/// conventional bases (24 hours, 60 minutes, 60 seconds). `None` is returned
/// only if [`fm_types::Timecode::new`] rejects a component, which the wrap and
/// the nominal clamp make unreachable but which stays typed rather than
/// panicking.
#[cfg(feature = "native-media")]
fn recording_start_timecode(
    first_sequence: u64,
    rate: fm_types::FrameRate,
) -> Option<fm_types::Timecode> {
    let numerator = u64::from(rate.numerator());
    let denominator = u64::from(rate.denominator());
    let nominal = u8::try_from(((numerator + denominator / 2) / denominator).clamp(1, 255)).ok()?;
    let frames_per_second = u64::from(nominal);
    let total_seconds = first_sequence / frames_per_second;
    fm_types::Timecode::new(
        u8::try_from((total_seconds / 3_600) % 24).ok()?,
        u8::try_from((total_seconds / 60) % 60).ok()?,
        u8::try_from(total_seconds % 60).ok()?,
        u8::try_from(first_sequence % frames_per_second).ok()?,
        nominal,
    )
    .ok()
}

#[cfg(feature = "native-media")]
impl NativeProgramRecorder {
    /// The recording format of a segment opened at the stored position.
    fn stored_format(stored: &StoredProject) -> AppResult<RecordFormat> {
        let settings = stored.project().settings();
        let dimensions = settings.video.dimensions;
        Ok(RecordFormat::new(
            dimensions.width(),
            dimensions.height(),
            settings.frame_rate,
            settings.audio.sample_rate,
            settings.audio.channels.clone(),
            SequenceNumber::new(stored.position().frames_rendered),
        )?)
    }

    /// Opens a segment synchronously. Used only for configured startup
    /// recording, which completes before readiness is advertised.
    #[allow(clippy::too_many_arguments)]
    fn start(
        runtime: &NativeMediaRuntime,
        stored: &StoredProject,
        path: &Path,
        names: &recording_session::SegmentNames,
        wav: bool,
        video_bitrate_kbps: Option<u32>,
        audio_bitrate_kbps: Option<u32>,
        slot: usize,
    ) -> AppResult<Self> {
        let format = Self::stored_format(stored)?;
        let timecode = recording_start_timecode(format.first_sequence().get(), format.frame_rate());
        let (mut recorder, config) = Self::prepare(
            runtime,
            format,
            timecode,
            path,
            wav,
            video_bitrate_kbps,
            audio_bitrate_kbps,
            slot,
        )?;
        let planned = recorder.plan_session(path, names)?;
        let opened = start_session_recorder(planned, config).map_err(AppFailure)?;
        recorder.sink = RecorderSink::Open(Box::new(opened.recorder));
        recorder.session = Some(opened.session);
        Ok(recorder)
    }

    /// Plans this segment's crash-safe session on the daemon loop. The name
    /// is reserved until the session's manifest exists, so the next segment
    /// name derived while this one is still opening never collides with it.
    fn plan_session(
        &self,
        path: &Path,
        names: &recording_session::SegmentNames,
    ) -> AppResult<recording_session::PlannedSession> {
        let parent = record_output_parent(path)?;
        recording_session::plan(
            path,
            &parent,
            names,
            self.slot,
            self.bitrate_bits_per_second,
            self.wav_bits_per_second,
        )
        .map_err(|error| AppFailure(error).into())
    }

    /// Opens a segment for an accepted runtime `RecordStart` without blocking
    /// the daemon loop: the segment file and `FFmpeg` start on a worker while
    /// captured pairs queue in the backlog, and mux readiness is awaited once
    /// the recorder arrives.
    #[allow(clippy::too_many_arguments)]
    fn open(
        runtime: &NativeMediaRuntime,
        stored: &StoredProject,
        path: &Path,
        names: &recording_session::SegmentNames,
        wav: bool,
        video_bitrate_kbps: Option<u32>,
        audio_bitrate_kbps: Option<u32>,
        slot: usize,
    ) -> AppResult<Self> {
        let format = Self::stored_format(stored)?;
        let timecode = recording_start_timecode(format.first_sequence().get(), format.frame_rate());
        Self::open_with_format(
            runtime,
            format,
            timecode,
            path,
            names,
            SegmentOpening {
                wav,
                video_bitrate_kbps,
                audio_bitrate_kbps,
                slot,
                failure_label: "start",
                prime_on_open: true,
            },
        )
    }

    /// Opens a segment on a worker; see [`Self::open`].
    fn open_with_format(
        runtime: &NativeMediaRuntime,
        format: RecordFormat,
        timecode: Option<fm_types::Timecode>,
        path: &Path,
        names: &recording_session::SegmentNames,
        opening: SegmentOpening,
    ) -> AppResult<Self> {
        let (mut recorder, config) = Self::prepare(
            runtime,
            format,
            timecode,
            path,
            opening.wav,
            opening.video_bitrate_kbps,
            opening.audio_bitrate_kbps,
            opening.slot,
        )?;
        // The session is planned and its name reserved here, so the next
        // segment name derived while this one is still opening never collides
        // with it; the free-space preflight, the session manifest, and the
        // FFmpeg start run on the worker.
        let planned = recorder.plan_session(path, names)?;
        let (sender, opened) = mpsc::sync_channel(1);
        let spawned = thread::Builder::new()
            .name(format!("freemix-native-recording-open-{}", opening.slot))
            .spawn(move || {
                let started = start_session_recorder(planned, config);
                // The segment always waits for this hand-over before it is
                // finalized, so the send only fails if the daemon is gone; the
                // dropped recorder then stops itself.
                let _ = sender.send(started);
            });
        spawned?;
        recorder.sink = RecorderSink::Opening {
            opened,
            failure_label: opening.failure_label,
            cancel_requested: false,
        };
        recorder.prime_on_open = opening.prime_on_open;
        Ok(recorder)
    }

    /// Everything a segment needs except its started recorder: the program
    /// readback, the recorder configuration, and the report metadata.
    #[allow(clippy::too_many_arguments)]
    fn prepare(
        runtime: &NativeMediaRuntime,
        format: RecordFormat,
        timecode: Option<fm_types::Timecode>,
        path: &Path,
        wav: bool,
        video_bitrate_kbps: Option<u32>,
        audio_bitrate_kbps: Option<u32>,
        slot: usize,
    ) -> AppResult<(Self, RecordConfig)> {
        let dimensions = format.dimensions();
        let readback = runtime.create_program_readback_blocking(
            NonZeroU32::new(dimensions.width()).expect("project width is nonzero"),
            NonZeroU32::new(dimensions.height()).expect("project height is nonzero"),
        )?;
        let parent = recording_parent(path).to_path_buf();
        let mut config = RecordConfig::new(format.clone());
        config.limits.stop_timeout = PROGRAM_RECORDER_STOP_TIMEOUT;
        config.limits.kill_timeout = PROGRAM_RECORDER_KILL_TIMEOUT;
        // The WAV sidecar goes to its own session sink, never to a path.
        let wav_bits_per_second = wav.then(|| {
            recording_session::wav_bitrate(
                format.sample_rate().hertz(),
                format.channel_layout().channels().len(),
            )
        });
        config.video_bitrate_kbps = video_bitrate_kbps;
        config.audio_bitrate_kbps = audio_bitrate_kbps;
        config.timecode = timecode;
        let startup_pair_timeout = config
            .limits
            .connect_timeout
            .checked_add(config.limits.no_progress_timeout)
            .ok_or_else(|| AppFailure("Program recorder startup timeout overflow".into()))?;
        let backlog_limit = config.limits.max_retained_bytes;
        let bitrate_bits_per_second = recording_session::estimated_bitrate(
            dimensions.width(),
            dimensions.height(),
            format.frame_rate(),
            format.channel_layout().channels().len(),
            video_bitrate_kbps,
            audio_bitrate_kbps,
        );
        Ok((
            Self {
                readback,
                format,
                sink: RecorderSink::Unopened,
                backlog: std::collections::VecDeque::new(),
                backlog_bytes: 0,
                backlog_limit,
                capture: RecorderCapturePolicy::default(),
                finalization_clean: None,
                startup_pair_timeout,
                priming: None,
                prime_on_open: false,
                parent,
                slot,
                video_bitrate_kbps,
                audio_bitrate_kbps,
                bitrate_bits_per_second,
                wav_bits_per_second,
                session: None,
            },
            config,
        ))
    }

    /// The running recorder, once the segment is open.
    fn recorder(&self) -> Option<&Recorder> {
        match &self.sink {
            RecorderSink::Open(recorder) => Some(recorder.as_ref()),
            RecorderSink::Opening { .. } | RecorderSink::Unopened => None,
        }
    }

    /// Whether this segment is still being realized: opening, or open but
    /// not yet confirmed mux-ready. A `RecordStart` acknowledgement waits for
    /// this to clear.
    fn is_settling(&self) -> bool {
        matches!(self.sink, RecorderSink::Opening { .. }) || self.priming.is_some()
    }

    /// Observes an opening segment without blocking.
    fn poll_open(&mut self) -> OpenPoll {
        let RecorderSink::Opening { opened, .. } = &self.sink else {
            return OpenPoll::Open;
        };
        let received = match opened.try_recv() {
            Ok(received) => received,
            Err(mpsc::TryRecvError::Empty) => return OpenPoll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("recorder open worker exited without a result".to_owned())
            }
        };
        self.finish_open(received)
    }

    /// Waits for an opening segment. Only finalization paths call this: a
    /// detached finalizer, or shutdown.
    fn wait_open(&mut self) -> OpenPoll {
        let RecorderSink::Opening { opened, .. } = &self.sink else {
            return OpenPoll::Open;
        };
        let received = opened
            .recv()
            .unwrap_or_else(|_| Err("recorder open worker exited without a result".to_owned()));
        self.finish_open(received)
    }

    fn finish_open(&mut self, received: Result<OpenedRecorder, String>) -> OpenPoll {
        let RecorderSink::Opening {
            failure_label,
            cancel_requested,
            ..
        } = &self.sink
        else {
            return OpenPoll::Open;
        };
        let (failure_label, cancel_requested) = (*failure_label, *cancel_requested);
        match received {
            Ok(OpenedRecorder {
                mut recorder,
                session,
            }) => {
                self.session = Some(session);
                if cancel_requested {
                    recorder.request_cancel();
                } else if self.prime_on_open {
                    // The barrier budget starts when the recorder exists,
                    // exactly as it did when the start blocked.
                    self.priming = Some(
                        Instant::now()
                            .checked_add(self.startup_pair_timeout)
                            .unwrap_or_else(Instant::now),
                    );
                }
                self.sink = RecorderSink::Open(Box::new(recorder));
                self.pump_backlog();
                OpenPoll::Open
            }
            Err(error) => {
                self.sink = RecorderSink::Unopened;
                self.clear_backlog();
                OpenPoll::Failed(format!("{failure_label}:{error}"))
            }
        }
    }

    /// Reports a directory the session just failed over from or returned
    /// to, and remaining-recording-time threshold crossings. The session
    /// keeps recording on the next usable directory, so these are notices,
    /// not recorder failures.
    fn report_failover(&mut self) {
        if let Some(session) = &mut self.session
            && let Some(record) =
                session
                    .failover
                    .observe(self.slot, &session.name, &session.status.get())
        {
            eprintln!("{record}");
        }
        if let Some(session) = &mut self.session
            && let Some(record) =
                session
                    .remaining
                    .observe(self.slot, &session.status, Instant::now())
        {
            eprintln!("{record}");
        }
    }

    fn clear_backlog(&mut self) {
        self.backlog.clear();
        self.backlog_bytes = 0;
    }

    /// Hands backlog pairs to the open recorder, oldest first, until it has
    /// no room for more. A full queue is not a failure here: the backlog keeps
    /// the pair and retries on the next pass.
    fn pump_backlog(&mut self) {
        let failure = {
            let RecorderSink::Open(recorder) = &mut self.sink else {
                return;
            };
            let mut failure = None;
            while let Some(pair) = self.backlog.pop_front() {
                let bytes = pair.retained_bytes();
                match recorder.enqueue(pair) {
                    Ok(()) => self.backlog_bytes = self.backlog_bytes.saturating_sub(bytes),
                    Err(error)
                        if matches!(
                            error.reason,
                            EnqueueRejection::QueueFull | EnqueueRejection::RetainedByteLimit
                        ) =>
                    {
                        self.backlog.push_front(error.into_frame());
                        break;
                    }
                    Err(error) => {
                        let app_capture_failure =
                            !matches!(&error.reason, EnqueueRejection::Failed(_));
                        failure =
                            Some((format!("enqueue:{:?}", error.reason), app_capture_failure));
                        break;
                    }
                }
            }
            failure
        };
        if let Some((failure, app_capture_failure)) = failure {
            self.fail(failure, app_capture_failure);
        }
    }

    /// Outstanding pairs and retained bytes across the recorder and the
    /// backlog, for whole-run telemetry.
    fn pipeline_load(&self) -> (u64, u64) {
        let (pairs, bytes) = self.recorder().map_or((0, 0), |recorder| {
            let telemetry = recorder.telemetry();
            (telemetry.outstanding_pairs, telemetry.retained_bytes)
        });
        (
            saturating_u64(pairs).saturating_add(saturating_u64(self.backlog.len())),
            saturating_u64(bytes).saturating_add(saturating_u64(self.backlog_bytes)),
        )
    }

    fn should_rotate(&self, segment_seconds: Option<NonZeroU32>) -> bool {
        self.capture.active()
            && self.backlog.is_empty()
            && segment_seconds.is_some_and(|seconds| {
                self.recorder().is_some_and(|recorder| {
                    recorder.telemetry().accepted_pairs
                        >= segment_frame_limit(seconds, self.format.frame_rate())
                })
            })
    }

    fn capture(
        &mut self,
        runtime: &NativeMediaRuntime,
        program: &NativeTexture,
        audio: AudioBlock,
    ) {
        if !self.capture.active() {
            return;
        }
        if let Some(telemetry) = self.recorder().map(Recorder::telemetry)
            && telemetry.state == RecorderState::Failed
        {
            self.fail(format!("backend:{:?}", telemetry.failure), false);
            return;
        }
        let sequence = audio.timing().sequence();
        // This is the existing synchronous diagnostic readback path, not a
        // nonblocking or zero-copy encoder bridge.
        let readback = match runtime.readback_program_blocking(&mut self.readback, program) {
            Ok(readback) => readback,
            Err(error) => {
                self.fail(format!("readback:{error}"), true);
                return;
            }
        };
        let Some(expected_stride) = readback.width.checked_mul(4) else {
            self.fail("readback:stride_overflow".to_owned(), true);
            return;
        };
        if readback.width != self.readback.width()
            || readback.height != self.readback.height()
            || readback.stride != expected_stride
        {
            self.fail("readback:invalid_tight_layout".to_owned(), true);
            return;
        }
        let pair = match PairedFrame::new(&self.format, sequence, readback.rgba, audio) {
            Ok(pair) => pair,
            Err(error) => {
                self.fail(format!("frame:{error}"), true);
                return;
            }
        };
        if self.backlog.is_empty()
            && let RecorderSink::Open(recorder) = &mut self.sink
        {
            if let Err(error) = recorder.enqueue(pair) {
                let app_capture_failure = !matches!(&error.reason, EnqueueRejection::Failed(_));
                self.fail(format!("enqueue:{:?}", error.reason), app_capture_failure);
            }
            return;
        }
        // The segment is still opening, or older pairs are still waiting:
        // queue behind them so the segment stays in sequence order.
        let bytes = pair.retained_bytes();
        if self.backlog_bytes.saturating_add(bytes) > self.backlog_limit {
            self.fail("enqueue:open_backlog_full".to_owned(), true);
            return;
        }
        self.backlog_bytes += bytes;
        self.backlog.push_back(pair);
        self.pump_backlog();
    }

    fn fail(&mut self, failure: String, app_capture_failure: bool) {
        if let Some((failure, app_capture_failure)) =
            self.capture.fail(failure, app_capture_failure)
        {
            let notice = recorder_failure_notice(self.slot, failure, app_capture_failure);
            eprintln!("{notice}");
            self.cancel_sink();
        }
    }

    /// Cancels whatever sink exists now, or the one still opening as soon as
    /// it arrives, and discards the backlog.
    fn cancel_sink(&mut self) {
        self.clear_backlog();
        match &mut self.sink {
            RecorderSink::Open(recorder) => recorder.request_cancel(),
            RecorderSink::Opening {
                cancel_requested, ..
            } => *cancel_requested = true,
            RecorderSink::Unopened => {}
        }
    }

    fn startup_decision(&self) -> AppResult<StartupPairDecision> {
        let decision = match self.recorder() {
            Some(recorder) => {
                let telemetry = recorder.telemetry();
                startup_pair_decision(
                    telemetry.state,
                    telemetry.completed_pairs,
                    telemetry.output_bytes,
                    telemetry.failure.is_some() || self.capture.first_failure.is_some(),
                )
            }
            None if self.capture.first_failure.is_some() => StartupPairDecision::Failed,
            None => StartupPairDecision::Pending,
        };
        if decision != StartupPairDecision::Failed {
            return Ok(decision);
        }
        let failure = self
            .capture
            .first_failure
            .clone()
            .or_else(|| {
                self.recorder()
                    .and_then(|recorder| recorder.telemetry().failure)
                    .map(|failure| format!("{failure:?}"))
            })
            .unwrap_or_else(|| "unknown".to_owned());
        Err(AppFailure(format!(
            "Program recorder failed before readiness: {}",
            diagnostic_field(&failure)
        ))
        .into())
    }

    fn fail_startup_timeout(&mut self) -> Box<dyn Error> {
        self.fail("startup:mux_readiness_timeout".to_owned(), false);
        AppFailure("Program recorder mux output timed out before readiness".into()).into()
    }

    /// Cancels the sink without waiting: future enqueues are refused and the
    /// child is asked to terminate within the kill budget.
    fn request_cancel(&mut self) {
        if self.finalization_clean.is_none() {
            self.cancel_sink();
        }
    }

    /// Feeds every backlog pair to the recorder before it is stopped, so a
    /// segment stopped right after it opened still holds every frame it
    /// accepted. Bounded by the recorder's own stop budget.
    fn drain_backlog_for_stop(&mut self) {
        let deadline = Instant::now().checked_add(PROGRAM_RECORDER_STOP_TIMEOUT);
        loop {
            self.pump_backlog();
            if self.backlog.is_empty() || self.recorder().is_none() {
                return;
            }
            if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                self.fail("enqueue:QueueFull".to_owned(), true);
                return;
            }
            thread::sleep(NATIVE_IO_POLL_INTERVAL);
        }
    }

    fn stop_and_report(&mut self) -> AppResult<()> {
        if let Some(clean) = self.finalization_clean {
            return if clean {
                Ok(())
            } else {
                Err(AppFailure("Program recording did not finalize cleanly".into()).into())
            };
        }
        if let OpenPoll::Failed(failure) = self.wait_open() {
            eprintln!("{}", recorder_failure_notice(self.slot, &failure, false));
        }
        self.drain_backlog_for_stop();
        let RecorderSink::Open(recorder) = &mut self.sink else {
            // The segment never opened: its failure was already reported and
            // there is no file to report on.
            self.finalization_clean = Some(false);
            return Err(AppFailure("Program recording did not finalize cleanly".into()).into());
        };
        let report = recorder.stop();
        // A fragment muxed while the recorder drained may have failed over.
        self.report_failover();
        let duration_ms =
            recorded_duration_ms(report.telemetry.completed_pairs, self.format.frame_rate());
        let free_bytes = disk::available_bytes(&self.parent).unwrap_or(0);
        let app_capture_failure = self.capture.app_capture_failure;
        let failure = self
            .capture
            .first_failure
            .clone()
            .or_else(|| {
                report
                    .telemetry
                    .failure
                    .as_ref()
                    .map(|value| format!("{value:?}"))
            })
            .unwrap_or_else(|| "none".to_owned());
        eprintln!(
            "{}",
            recorder_report(
                self.slot,
                &report,
                duration_ms,
                free_bytes,
                app_capture_failure,
                &failure,
                self.video_bitrate_kbps,
                self.audio_bitrate_kbps,
            )
        );
        if let Some(session) = &self.session {
            eprintln!(
                "{}",
                recording_session::session_report(self.slot, &session.name, &session.status.get())
            );
        }
        let complete = report.telemetry.state == RecorderState::Stopped
            && report.outcome == StopOutcome::Clean
            && report.output == OutputFinalization::Synced
            && report.cleanup == CleanupStatus::Complete
            && report.telemetry.accepted_pairs == report.telemetry.completed_pairs
            && report.telemetry.failed_pairs == 0
            && !app_capture_failure;
        self.finalization_clean = Some(complete);
        if complete {
            Ok(())
        } else {
            Err(AppFailure("Program recording did not finalize cleanly".into()).into())
        }
    }
}

#[cfg(feature = "native-media")]
impl Drop for NativeProgramRecorder {
    fn drop(&mut self) {
        let _ = self.stop_and_report();
    }
}

#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupPairDecision {
    Pending,
    Ready,
    Failed,
}

#[cfg(feature = "native-media")]
fn startup_pair_decision(
    state: RecorderState,
    completed_pairs: u64,
    output_bytes: u64,
    failed: bool,
) -> StartupPairDecision {
    if failed || state == RecorderState::Failed {
        StartupPairDecision::Failed
    } else if completed_pairs > 0 && output_bytes > 0 {
        StartupPairDecision::Ready
    } else {
        StartupPairDecision::Pending
    }
}

/// Wall duration of `pairs` completed frames at `rate`, in whole milliseconds.
///
/// The recorder knows only admitted and completed pair counts; the captured
/// cadence turns those into the media span the segment actually covers. The
/// arithmetic is exact integer math, saturating rather than wrapping.
#[cfg(feature = "native-media")]
fn recorded_duration_ms(pairs: u64, rate: fm_types::FrameRate) -> u64 {
    let numerator = u128::from(rate.numerator());
    let denominator = u128::from(rate.denominator());
    let millis = u128::from(pairs)
        .saturating_mul(1_000)
        .saturating_mul(denominator)
        / numerator;
    u64::try_from(millis).unwrap_or(u64::MAX)
}

/// Realization state of one configured recorder slot.
///
/// Recording is configured by `--record-program` (slot 0) or
/// `--record-program-2` (slot 1) but only OPENED while the engine's desired
/// flag is set, mirroring how stream sinks realize desired running flags. Each
/// slot is independent: one open segment per configured output, boxed to keep
/// this enum small and moved as a unit at every state change.
#[cfg(feature = "native-media")]
enum ProgramRecording {
    /// Recording is configured, but no segment is open.
    Off,
    /// One open segment capturing the program feed.
    Running(Box<NativeProgramRecorder>),
}

#[cfg(feature = "native-media")]
impl ProgramRecording {
    /// Whether this slot currently owns an open segment.
    fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
    }

    /// Whether this slot's segment is still opening or awaiting mux
    /// readiness.
    fn is_settling(&self) -> bool {
        matches!(self, Self::Running(recorder) if recorder.is_settling())
    }
}

/// The daemon's fixed number of recorder slots: slot 0 is `--record-program`
/// and slot 1 is `--record-program-2`. A second slot doubles the independent
/// destinations behind the engine's per-slot desired recording flags. The
/// count also sizes the per-slot CLI configuration arrays, so it stays
/// available in every build.
const RECORDER_SLOTS: usize = 2;

/// Selects the configured recorder slots whose restored desired flag asks for
/// an open segment at startup.
///
/// A slot is selected only when it is both configured and desired: an
/// unconfigured slot has no destination to open, and a configured slot whose
/// persisted flag is false stays closed even when the other slot is
/// recording. Runtime `RecordStart` commands open a slot later regardless of
/// this startup decision.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
fn startup_recording_slots(
    configured: [bool; RECORDER_SLOTS],
    desired: [bool; RECORDER_SLOTS],
) -> [bool; RECORDER_SLOTS] {
    std::array::from_fn(|slot| configured[slot] && desired[slot])
}

/// Resolves an accepted recording command to its addressed slot.
///
/// Returns `(active, slot_index)` for a `RecordStart`/`RecordStop` and `None`
/// for every other command. `WireRecorderSlot` only constructs in `1..=2`, so
/// the index always names exactly one of the daemon's two recorder slots and
/// realization never touches the other one.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
fn recording_command_slot(payload: &CommandPayload) -> Option<(bool, usize)> {
    match payload {
        CommandPayload::RecordStart { recorder } => Some((true, usize::from(recorder.get()) - 1)),
        CommandPayload::RecordStop { recorder } => Some((false, usize::from(recorder.get()) - 1)),
        _ => None,
    }
}

/// The recorder and stream commands an accepted cue GO or advance ran
/// (RC-017), in the order its actions ran them, as the direct commands the
/// native runtime realizes: a cue that starts REC 1 opens a segment exactly
/// like `record_start`, and one that stops a stream retires its worker
/// exactly like `stream_stop`. Every other command yields nothing; the
/// direct record and stream commands are observed as themselves.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
fn cue_worker_commands(
    command: &CommandMessage,
    submission: &CommandSubmission,
) -> Vec<CommandMessage> {
    if !matches!(
        command.payload,
        CommandPayload::CueControl { .. } | CommandPayload::CueAdvance { .. }
    ) {
        return Vec::new();
    }
    let Some(accepted) = &submission.accepted else {
        return Vec::new();
    };
    accepted
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            fm_engine::EngineEvent::CueListChanged { applied, .. } => Some(applied),
            _ => None,
        })
        .flatten()
        .filter_map(|action| {
            let wire_recorder = |recorder: RecorderSlot| {
                fm_protocol::WireRecorderSlot::new(recorder.get())
                    .expect("switcher recorder slots match the wire")
            };
            let payload = match action {
                fm_engine::EngineCommand::RecordStart { recorder } => CommandPayload::RecordStart {
                    recorder: wire_recorder(*recorder),
                },
                fm_engine::EngineCommand::RecordStop { recorder } => CommandPayload::RecordStop {
                    recorder: wire_recorder(*recorder),
                },
                fm_engine::EngineCommand::StreamStart { target } => CommandPayload::StreamStart {
                    target: WireStreamTargetId::new(target.get()),
                },
                fm_engine::EngineCommand::StreamStop { target } => CommandPayload::StreamStop {
                    target: WireStreamTargetId::new(target.get()),
                },
                _ => return None,
            };
            Some(CommandMessage {
                payload,
                ..command.clone()
            })
        })
        .collect()
}

/// One segment handed off the render path for bounded finalization.
///
/// The worker owns the recorder and emits exactly one `FREEMIXD_RECORDER`
/// stderr report; shutdown waits for completion within the recorder's own
/// stop budget and detaches rather than blocking forever. The completion
/// signal is a plain flag because the detailed report already reached stderr
/// and [`AppResult`] is not `Send`.
#[cfg(feature = "native-media")]
struct ProgramRecordingFinalizer {
    done: mpsc::Receiver<bool>,
    worker: thread::JoinHandle<()>,
}

/// Upper bound for waiting on a detached finalizer at shutdown. The worker's
/// own `stop()` drains within [`PROGRAM_RECORDER_STOP_TIMEOUT`] and kills
/// within [`PROGRAM_RECORDER_KILL_TIMEOUT`], so this adds only scheduler
/// slack; past it the worker is detached like stinger retirement cleanup.
#[cfg(feature = "native-media")]
const PROGRAM_RECORDER_FINALIZE_JOIN_TIMEOUT: Duration = Duration::from_secs(35);

/// First-segment ordinal for derived recording paths; later segments count up
/// from 002 so the configured path itself stays segment 001.
#[cfg(feature = "native-media")]
const RECORD_SEGMENT_LAST_ORDINAL: u32 = 999;

#[cfg(feature = "native-media")]
fn next_record_segment_path(
    base: &Path,
    mut exists: impl FnMut(&Path) -> bool,
) -> AppResult<PathBuf> {
    if base.extension().and_then(|extension| extension.to_str()) != Some("mp4") {
        return Err(
            AppFailure("recording output must have the final extension `.mp4`".into()).into(),
        );
    }
    let file_name = base
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppFailure("recording output must name a final .mp4 file".into()))?;
    if !exists(base) {
        return Ok(base.to_path_buf());
    }
    // `to_string_lossy` keeps non-UTF8 stems refused instead of silently
    // re-encoded: a path that cannot be named deterministically is never
    // created.
    let stem = base
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AppFailure("recording output stem is not valid Unicode".into()))?;
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    for ordinal in 2..=RECORD_SEGMENT_LAST_ORDINAL {
        let candidate = parent.join(format!("{stem}-{ordinal:03}.mp4"));
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(AppFailure(format!(
        "recording segments are exhausted past {RECORD_SEGMENT_LAST_ORDINAL} for {}",
        diagnostic_field(&file_name.to_string_lossy())
    ))
    .into())
}

/// Resolves the next segment path for one recorder slot.
///
/// Slot 0 derives from `--record-program` and slot 1 from
/// `--record-program-2`; an unconfigured slot derives nothing. Derivation
/// stays independent per slot, so two recorders writing into the same
/// directory fill their own `<stem>-NNN.mp4` gaps.
///
/// # Errors
///
/// Returns the same refusal [`next_record_segment_path`] produces for a
/// configured path that cannot be segmented.
#[cfg(feature = "native-media")]
fn next_slot_segment_path(
    slot: usize,
    primary: Option<&Path>,
    secondary: Option<&Path>,
    exists: impl FnMut(&Path) -> bool,
) -> AppResult<Option<PathBuf>> {
    let configured = match slot {
        0 => primary,
        1 => secondary,
        _ => None,
    };
    configured
        .map(|base| next_record_segment_path(base, exists))
        .transpose()
}

/// Selects the configured rotation duration addressed to one recorder slot.
///
/// Slot 0 follows `--record-segment-seconds` and slot 1 follows
/// `--record-segment-seconds-2`; `None` means that slot never rotates
/// automatically, even when the other slot is configured to rotate.
#[cfg(feature = "native-media")]
fn slot_segment_seconds(
    segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    slot: usize,
) -> Option<NonZeroU32> {
    segment_seconds[slot]
}

/// Per-target bookkeeping for the native streaming feed path.
///
/// The ledger is separated from the sink so the counting rules — advance the
/// admitted cursor only on a successful enqueue, count every refusal, never
/// block or panic — are testable without a GPU runtime or an `FFmpeg` child.
/// Gating lives on the rendition group's shared cursor; this per-target
/// cursor records how far each target's own admissions reached.
#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StreamPairLedger {
    /// One past the highest sequence this target actually admitted.
    next_sequence: u64,
    enqueued_pairs: u64,
    dropped_pairs: u64,
}

#[cfg(feature = "native-media")]
impl StreamPairLedger {
    fn note_admitted(&mut self, sequence: SequenceNumber) {
        self.enqueued_pairs = self.enqueued_pairs.saturating_add(1);
        self.next_sequence = sequence.get().saturating_add(1);
    }

    fn note_dropped(&mut self) {
        self.dropped_pairs = self.dropped_pairs.saturating_add(1);
    }
}

/// Planner-facing state of one shared-rendition group: the destinations whose
/// encoder-relevant settings — authored project output, output protocol, plus
/// authored video bitrate — are identical, so they share ONE readback, ONE
/// packed pair per frame, and ONE sequence cursor. Record format, audio, GOP,
/// and every other encoder input derive from project settings that are
/// identical across targets by construction.
///
/// `variants` pins the group's rendition id inside planner-produced plans:
/// [`RenditionPlanner`] numbers renditions by first-seen unique profile, so
/// the group's real profile sits at exactly its group position among inert
/// sibling rungs (see [`pinned_rendition_variants`]). Sinks refuse packets
/// claiming any other rendition, which makes the id part of the sink
/// contract and the reason the pinning exists.
#[cfg(feature = "native-media")]
struct StreamRenditionCore {
    /// Authored project output this rendition group captures from.
    output: fm_types::OutputId,
    /// Member destination slots in authored order.
    members: Vec<DestinationId>,
    rendition: RenditionId,
    variants: Vec<AbrVariant>,
    /// Smallest sequence the group may still pack; advances once per packed
    /// frame no matter how many member routes take it.
    next_sequence: u64,
}

/// One rendition group's GPU-side state: the single synchronous program
/// readback owner every member shares, and the format the group's pairs pack
/// with.
#[cfg(feature = "native-media")]
struct StreamRenditionGroup {
    core: StreamRenditionCore,
    format: RecordFormat,
    readback: NativeProgramReadback,
    /// The encoder identity every member shares: authored output, output
    /// protocol, and authored video bitrate.
    key: StreamGroupKey,
}

/// A rendition group's encoder identity: authored project output, output
/// protocol, and authored video bitrate in kbps.
#[cfg(feature = "native-media")]
type StreamGroupKey = (fm_types::OutputId, OutputProtocol, u32);

/// Builds one group's exact rendition identity from the project-wide format
/// plus its authored video bitrate. `bitrate_step` offsets the video bitrate
/// by one bit per step for the inert ladder rungs that pin plan numbering;
/// authored bitrates start at 1000 kbps, so a few negative steps never reach
/// zero.
///
/// # Errors
///
/// Returns any identity-validation failure from the planner crate.
#[cfg(feature = "native-media")]
fn rendition_profile(
    format: &RecordFormat,
    video_bitrate_kbps: u32,
    bitrate_step: i32,
) -> AppResult<RenditionProfile> {
    let cadence = format.frame_rate();
    let authored_bps = i64::from(video_bitrate_kbps) * 1_000 + i64::from(bitrate_step);
    assert!(
        authored_bps > 0,
        "authored stream bitrates leave room for ladder-pinning steps"
    );
    let video = VideoRendition::new(
        CodecId::new("video/h264")?,
        "high",
        format.dimensions(),
        FrameRate::new(cadence.numerator(), cadence.denominator())?,
        ColorDescription::Rec709Limited,
        u64::try_from(authored_bps).expect("bitrate was checked positive above"),
        gop_frames(cadence),
    )?;
    let channels = u16::try_from(format.channel_layout().channels().len()).unwrap_or(u16::MAX - 1);
    let audio = AudioRendition::new(
        CodecId::new("audio/aac")?,
        "lc",
        u64::from(EncoderSettings::default().audio_bitrate_kbps) * 1_000,
        format.sample_rate().hertz(),
        channels,
        (0..channels).collect(),
    )?;
    Ok(RenditionProfile::new(
        video,
        audio,
        TimingProfile::new(
            TimeBase::new(cadence.numerator(), cadence.denominator())?,
            0,
        ),
    ))
}

/// Two seconds of frames at the project cadence, at least one.
#[cfg(feature = "native-media")]
fn gop_frames(cadence: fm_types::FrameRate) -> u32 {
    let frames = u64::from(cadence.numerator()) * 2 / u64::from(cadence.denominator());
    u32::try_from(frames.max(1)).expect("two seconds of frames fit a GOP length")
}

/// Builds one group's planner request variants: with a single rendition
/// group the plan is the plain single-rendition request; with several groups,
/// each group's plan must still number its real rendition at the group's
/// compile-time id because sinks refuse packets from any other rendition, so
/// the real profile embeds at exactly its position among inert sibling rungs
/// whose bitrates strictly increase by one bit per step around the authored
/// one. Only the group's own rung is ever dispatched:
/// [`OutputSet::enqueue_rendition`] fans out solely along the packed
/// rendition's route.
///
/// # Errors
///
/// Returns any identity-validation failure from the planner crate.
#[cfg(feature = "native-media")]
fn pinned_rendition_variants(
    format: &RecordFormat,
    video_bitrate_kbps: u32,
    group_index: usize,
    group_count: usize,
) -> AppResult<Vec<AbrVariant>> {
    let rung = |step: i32, name: String| -> AppResult<AbrVariant> {
        Ok(AbrVariant::new(
            name,
            rendition_profile(format, video_bitrate_kbps, step)?,
        )?)
    };
    if group_count == 1 {
        return Ok(vec![rung(0, "program".to_owned())?]);
    }
    let mut variants = Vec::with_capacity(group_count);
    for index in 0..group_count {
        let step = i32::try_from(index).expect("rendition group counts fit i32")
            - i32::try_from(group_index).expect("rendition group counts fit i32");
        variants.push(rung(step, format!("shared-rung-{}", index + 1))?);
    }
    AbrLadder::new(variants.clone())
        .expect("pinned rendition rung bitrates strictly increase by one step");
    Ok(variants)
}

/// One configured stream target's native realization: its own transport,
/// per-target feed bookkeeping, and membership in one shared-rendition
/// [`StreamRenditionGroup`] that owns the readback, the format, and the
/// packing step for every target sharing its encoder identity.
///
/// Mirrors [`NativeProgramRecorder`]: failures latch per target, are reported
/// once as a sanitized record, and degrade that target only — they never abort
/// daemon startup or the render loop. Connection policy (retry budget,
/// backoff, primary/backup failover) is not owned here at all: every target
/// routes through the daemon's shared [`OutputSet`], and this struct owns only
/// the transport that the set drives.
#[cfg(feature = "native-media")]
struct NativeProgramStream {
    target: SwitcherStreamTargetId,
    /// This target's slot in the shared [`OutputSet`] (1..=N in authored
    /// order).
    destination_id: DestinationId,
    /// Redacted primary destination for diagnostics; mirrors what
    /// `StreamDestination::redacted` renders.
    redacted_destination: String,
    /// The `FFmpeg` transport, whose blocking connects and stops run on a
    /// dedicated worker so the daemon loop never waits on them.
    sink: stream_transport::BackgroundTransport<FfmpegRtmpSink>,
    ledger: StreamPairLedger,
    first_failure: Option<String>,
    /// Set when a frame pair was refused with capacity backpressure since the
    /// last status sample; consumed (and cleared) by the sampler.
    enqueue_backpressure: bool,
    /// Whether this target was ever asked to run. Never-started targets stay
    /// off the wire entirely.
    started: bool,
    /// Uptime and encoded bitrate for the status record.
    meter: StreamLinkMeter,
    /// The authored configuration this runtime realizes, with its running
    /// flag cleared: a destination edit that changes it rebuilds the runtime
    /// (OR-006).
    authored: fm_model::StreamTarget,
}

/// Compiles one target's authored video bitrate into the sink's encoder
/// settings; every other encoder knob keeps its sink default.
#[cfg(feature = "native-media")]
fn stream_encoder_settings(video_bitrate_kbps: u32) -> EncoderSettings {
    EncoderSettings {
        video_bitrate_kbps,
        ..EncoderSettings::default()
    }
}

/// One target's authored reconnect policy (OR-006) as the shared output
/// set's backoff: the first retry after the authored initial delay, doubling
/// up to the authored maximum, and giving up after the authored attempt
/// budget (none retries forever), which latches the target failed.
#[cfg(feature = "native-media")]
fn stream_reconnect_policy(policy: fm_model::StreamReconnectPolicy) -> ReconnectPolicy {
    ReconnectPolicy::new(
        u64::from(policy.initial_delay_ms()),
        u64::from(policy.max_delay_ms()),
        PROGRAM_STREAM_RECONNECT_MULTIPLIER,
        policy.max_attempts(),
    )
    .expect("the model bounds every authored reconnect policy")
}

/// The record format every stream pair packs with: the project picture,
/// cadence, and audio layout, numbering from `first_sequence`.
#[cfg(feature = "native-media")]
fn stream_record_format(
    settings: &fm_model::ProjectSettings,
    first_sequence: u64,
) -> AppResult<RecordFormat> {
    Ok(RecordFormat::new(
        settings.video.dimensions.width(),
        settings.video.dimensions.height(),
        settings.frame_rate,
        settings.audio.sample_rate,
        settings.audio.channels.clone(),
        SequenceNumber::new(first_sequence),
    )?)
}

/// Compiles one SRT target's authored latency and optional passphrase into
/// the sink's caller-mode options; `None` for every other protocol.
///
/// `fm-model` and the sink validate the same bounds, so a rejected conversion
/// is unreachable; dropping the passphrase instead would publish an
/// encrypted destination in the clear.
#[cfg(feature = "native-media")]
fn stream_srt_options(target: &fm_model::StreamTarget) -> Option<SrtOptions> {
    let latency_ms = target.srt_latency_ms()?;
    Some(
        SrtOptions::new(
            latency_ms,
            target
                .srt_passphrase()
                .map(fm_model::SrtPassphrase::expose_secret),
        )
        .expect("the authored SRT options already satisfy the sink's rules"),
    )
}

/// Compiles one target's authored HLS ladder into the sink's own rung type.
///
/// `fm-model` and the sink validate the identical rule set, and the authored
/// value cannot change between them, so a rejected conversion is unreachable;
/// dropping the ladder instead would silently publish one rendition of a
/// multi-bitrate destination.
#[cfg(feature = "native-media")]
fn stream_ladder(target: &fm_model::StreamTarget) -> Option<HlsLadder> {
    target.ladder().map(|ladder| {
        HlsLadder::new(
            ladder
                .rungs()
                .iter()
                .map(|rung| HlsLadderRung {
                    video_bitrate_kbps: rung.video_bitrate_kbps(),
                    width: rung.width(),
                    height: rung.height(),
                })
                .collect(),
        )
        .expect("the authored ladder already satisfies the sink's rules")
    })
}

/// Validates one authored target's primary and backup URLs with the codec
/// parser — validity stays where it has always been — and splits them into
/// the endpoint pieces a [`DestinationConfig`] carries.
///
/// # Errors
///
/// Returns an error when either URL would be refused at connect time; startup
/// fails closed rather than latching every target failed at air time.
#[cfg(feature = "native-media")]
fn split_target_endpoints(
    target: &fm_model::StreamTarget,
) -> AppResult<(
    OutputProtocol,
    Endpoint,
    Option<StreamKey>,
    Option<Endpoint>,
)> {
    let primary_url = target.expose_url();
    StreamDestination::parse(&primary_url).map_err(|error| {
        AppFailure(format!(
            "stream target {} destination was refused before startup: {error:?}",
            target.id()
        ))
    })?;
    let backup_url = target.expose_backup_url();
    if let Some(backup) = &backup_url {
        StreamDestination::parse(backup).map_err(|error| {
            AppFailure(format!(
                "stream target {} backup destination was refused before startup: {error:?}",
                target.id()
            ))
        })?;
    }
    let (protocol, endpoint, stream_key) = split_destination_url(&primary_url)?;
    let backup_endpoint = backup_url
        .as_deref()
        .map(split_destination_url)
        .transpose()?
        .map(|(_, endpoint, _)| endpoint);
    Ok((protocol, endpoint, stream_key, backup_endpoint))
}

/// Splits one full destination URL into the pieces a [`DestinationConfig`]
/// carries, using the one convention that round-trips through
/// `fm_codec_ffmpeg::StreamDestination::parse` for every protocol:
/// RTMP/RTMPS compose back as `{scheme}://{host}:{port}{path}/{key}` with the
/// application path stored in `Endpoint::path` *without* its final key
/// segment, SRT composes back as `srt://{host}:{port}?streamid={key}` with no
/// path at all, so its mandatory `Endpoint::path` field holds the inert
/// placeholder `"/"`, which the SRT branch ignores, and HLS composes back as
/// `hls://{absolute/dir}` with the directory stored in `Endpoint::path`, no
/// key at all, and the placeholder host and port a local destination never
/// renders. Every network endpoint always names an explicit port because an
/// `Endpoint` cannot be built without one; an authored URL that omits it fails
/// compilation here instead of being guessed at air time. The stream key is
/// revalidated as the redactable single path segment the transport expects.
///
/// # Errors
///
/// Returns an error for any URL shape that cannot be split losslessly under
/// those rules. No error carries endpoint or key text.
#[cfg(feature = "native-media")]
fn split_destination_url(url: &str) -> AppResult<(OutputProtocol, Endpoint, Option<StreamKey>)> {
    let unsupported = || -> AppResult<(OutputProtocol, Endpoint, Option<StreamKey>)> {
        Err(AppFailure("stream destination URL scheme is unsupported".into()).into())
    };
    let (protocol, rest) = if let Some(rest) = url.strip_prefix("rtmp://") {
        (OutputProtocol::Rtmp, rest)
    } else if let Some(rest) = url.strip_prefix("rtmps://") {
        (OutputProtocol::Rtmps, rest)
    } else if let Some(rest) = url.strip_prefix("srt://") {
        (OutputProtocol::Srt, rest)
    } else if let Some(rest) = url.strip_prefix("hls://") {
        (OutputProtocol::Hls, rest)
    } else {
        return unsupported();
    };
    // A local HLS destination is one absolute directory and carries no key:
    // the endpoint stores the directory in `path`, and the placeholder host
    // and port are never rendered by any transport.
    if protocol == OutputProtocol::Hls {
        let endpoint = Endpoint::local_directory(rest).map_err(|error| {
            AppFailure(format!(
                "stream destination endpoint was refused: {error:?}"
            ))
        })?;
        return Ok((OutputProtocol::Hls, endpoint, None));
    }
    let (authority, endpoint_path, key_text) = if protocol == OutputProtocol::Srt {
        match rest.split_once('?') {
            Some((authority, query)) => {
                let key = match query.strip_prefix("streamid=") {
                    Some(key) => Some(key),
                    None => {
                        return Err(AppFailure(
                            "stream destination query is not a stream id".into(),
                        )
                        .into());
                    }
                };
                (authority, None, key)
            }
            None => (rest, None, None),
        }
    } else {
        match rest.split_once('/') {
            Some((authority, path)) => {
                // The final path segment is the stream key; what precedes it
                // is the application path.
                let (application, key) = path.rsplit_once('/').ok_or_else(|| {
                    AppFailure("stream destination endpoint has no application path".into())
                })?;
                (authority, Some(format!("/{application}")), Some(key))
            }
            None => {
                return Err(AppFailure(
                    "stream destination endpoint has no application path".into(),
                )
                .into());
            }
        }
    };
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| AppFailure("stream destination endpoint has no explicit port".into()))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| AppFailure("stream destination endpoint port is invalid".into()))?;
    let endpoint =
        Endpoint::new(host, port, endpoint_path.as_deref().unwrap_or("/")).map_err(|error| {
            AppFailure(format!(
                "stream destination endpoint was refused: {error:?}"
            ))
        })?;
    // The key is validated for the protocol that will carry it: an SRT
    // stream id is one query value and may not split into more parameters.
    // Only the typed refusal is rendered, never the key text.
    let stream_key = key_text
        .map(|key| StreamKey::for_protocol(protocol, key))
        .transpose()
        .map_err(|error| AppFailure(format!("stream destination key was refused: {error}")))?;
    Ok((protocol, endpoint, stream_key))
}

/// Renders the destination with its key masked, byte-for-byte like
/// `StreamDestination::redacted` renders the same URL. An HLS destination has
/// no key: its absolute directory is operator-chosen local storage rather than
/// a secret, so it is rendered verbatim as `hls:///absolute/path`.
#[cfg(feature = "native-media")]
fn redacted_destination_url(protocol: OutputProtocol, endpoint: &Endpoint, keyed: bool) -> String {
    match protocol {
        OutputProtocol::Rtmp | OutputProtocol::Rtmps => {
            let scheme = if protocol == OutputProtocol::Rtmp {
                "rtmp"
            } else {
                "rtmps"
            };
            if keyed {
                format!(
                    "{scheme}://{}:{}{}/****",
                    endpoint.host(),
                    endpoint.port(),
                    endpoint.path()
                )
            } else {
                format!(
                    "{scheme}://{}:{}{}",
                    endpoint.host(),
                    endpoint.port(),
                    endpoint.path()
                )
            }
        }
        OutputProtocol::Srt => {
            let mut url = format!("srt://{}:{}", endpoint.host(), endpoint.port());
            if keyed {
                url.push_str("?streamid=****");
            }
            url
        }
        // A local HLS destination is its absolute directory: there is no key
        // to mask, and the placeholder host and port are never rendered.
        OutputProtocol::Hls => format!("hls://{}", endpoint.path()),
        // LAN outputs have no daemon streaming transport yet.
        OutputProtocol::LiveLan => String::new(),
    }
}

/// One target's validated destination pieces, split before grouping so
/// rendition grouping happens over complete, already-validated inputs, plus
/// its rendition group's position.
#[cfg(feature = "native-media")]
struct SplitStreamTarget<'a> {
    target: &'a fm_model::StreamTarget,
    protocol: OutputProtocol,
    endpoint: Endpoint,
    stream_key: Option<StreamKey>,
    backup_endpoint: Option<Endpoint>,
    group_index: usize,
}

/// Grouped target splits plus the distinct encoder-identity keys in
/// first-appearance order.
#[cfg(feature = "native-media")]
struct SplitTargets<'a> {
    splits: Vec<SplitStreamTarget<'a>>,
    group_keys: Vec<(fm_types::OutputId, OutputProtocol, u32)>,
}

/// Splits every target's endpoints and assigns rendition groups by first
/// appearance of the encoder-relevant key — authored project output, output
/// protocol, plus authored video bitrate.
///
/// # Errors
///
/// Returns any destination URL that would be refused at connect time.
#[cfg(feature = "native-media")]
fn split_and_group_targets(targets: &[fm_model::StreamTarget]) -> AppResult<SplitTargets<'_>> {
    let mut splits = Vec::with_capacity(targets.len());
    for target in targets {
        let (protocol, endpoint, stream_key, backup_endpoint) = split_target_endpoints(target)?;
        splits.push(SplitStreamTarget {
            target,
            protocol,
            endpoint,
            stream_key,
            backup_endpoint,
            group_index: 0,
        });
    }
    let mut group_keys: Vec<(fm_types::OutputId, OutputProtocol, u32)> = Vec::new();
    for split in &mut splits {
        let key = (
            split.target.output(),
            split.protocol,
            split.target.video_bitrate_kbps(),
        );
        split.group_index = group_keys
            .iter()
            .position(|existing| *existing == key)
            .unwrap_or_else(|| {
                group_keys.push(key);
                group_keys.len() - 1
            });
    }
    Ok(SplitTargets { splits, group_keys })
}

/// The absolute directory of every authored local HLS target, in authored
/// order.
///
/// An HLS target splits into one absolute directory instead of a network
/// endpoint, and the sink creates that leaf directory later, so both the
/// startup preflight and the runtime free-space sampler measure the nearest
/// existing ancestor. Bounded by the engine's five-target cap.
#[cfg(feature = "native-media")]
fn local_output_directories(splits: &[SplitStreamTarget<'_>]) -> Vec<PathBuf> {
    splits
        .iter()
        .filter(|split| split.protocol == OutputProtocol::Hls)
        .map(|split| PathBuf::from(split.endpoint.path()))
        .collect()
}

/// Filters a compiled destination listing down to the local `hls://` roots,
/// keyed by the engine stream target id.
///
/// Only local HLS destinations contribute a root; every network destination is
/// skipped. Duplicate target ids are refused rather than letting the last one
/// win, because two roots under one id would serve the wrong ladder.
///
/// # Errors
///
/// Returns [`hls_server::HlsRouteError::DuplicateTarget`] when two HLS
/// destinations name the same target id.
#[cfg(feature = "native-media")]
fn hls_roots_from_destinations(
    destinations: impl IntoIterator<Item = (OutputProtocol, u128, PathBuf)>,
) -> Result<HlsRoots, hls_server::HlsRouteError> {
    HlsRoots::try_from_targets(
        destinations
            .into_iter()
            .filter_map(|(protocol, target, root)| {
                (protocol == OutputProtocol::Hls).then_some((target, root))
            }),
    )
}

/// Builds the HLS route table from the daemon's compiled stream destinations.
///
/// Each local `hls://` destination is keyed by the same engine stream target id
/// that `FREEMIXD_PREFLIGHT` and `FREEMIXD_STREAM` records carry.
///
/// # Errors
///
/// Returns [`hls_server::HlsRouteError::DuplicateTarget`] if two compiled
/// destinations name the same target id, which the engine's target inventory
/// should already prevent.
#[cfg(feature = "native-media")]
fn hls_roots(native: Option<&NativeDaemon>) -> Result<HlsRoots, hls_server::HlsRouteError> {
    let destinations = native.into_iter().flat_map(|native| {
        native.streams.iter().filter_map(|stream| {
            let config = native.output_set.config(stream.destination_id)?;
            Some((
                config.protocol(),
                stream.target.get().get(),
                PathBuf::from(config.endpoint().path()),
            ))
        })
    });
    hls_roots_from_destinations(destinations)
}

/// Builds the pending network probe for one compiled destination's endpoint,
/// or `None` when that endpoint cannot or should not be probed.
///
/// A local HLS destination is a directory write with no peer, so it yields
/// `None`. The label must be [`preflight::PRIMARY_ENDPOINT`] or
/// [`preflight::BACKUP_ENDPOINT`]; a backup probe exists only when the
/// destination configured a backup endpoint. The returned probe owns a copy
/// of the host string, and no record ever renders it: the preflight record
/// carries the target id and endpoint label only.
#[cfg(feature = "native-media")]
fn pending_network_probe(
    config: &DestinationConfig,
    target: u128,
    endpoint: &'static str,
) -> Option<preflight::PendingProbe> {
    if config.protocol() == OutputProtocol::Hls {
        return None;
    }
    let endpoint_config = if endpoint == preflight::PRIMARY_ENDPOINT {
        Some(config.endpoint())
    } else if endpoint == preflight::BACKUP_ENDPOINT {
        config.backup_endpoint()
    } else {
        None
    }?;
    Some(preflight::PendingProbe {
        target,
        endpoint,
        host: endpoint_config.host().to_owned(),
        port: endpoint_config.port(),
        timeout: preflight::NETWORK_PREFLIGHT_TIMEOUT,
    })
}

/// Builds one shared state bundle per rendition group: one readback owner for
/// the whole group and the planner variants that pin the group's rendition id
/// inside planner-produced plans. Group ids are their one-based positions.
///
/// # Errors
///
/// Returns any GPU readback-owner or rendition-identity failure.
#[cfg(feature = "native-media")]
fn build_stream_groups(
    runtime: &NativeMediaRuntime,
    format: &RecordFormat,
    width: NonZeroU32,
    height: NonZeroU32,
    group_keys: &[(fm_types::OutputId, OutputProtocol, u32)],
    next_sequence: u64,
) -> AppResult<Vec<StreamRenditionGroup>> {
    let mut groups = Vec::with_capacity(group_keys.len());
    for (group_index, key) in group_keys.iter().enumerate() {
        groups.push(new_stream_group(
            runtime,
            format,
            (width, height),
            *key,
            (group_index, group_keys.len()),
            next_sequence,
        )?);
    }
    Ok(groups)
}

/// Builds one rendition group: its own readback owner, and the planner
/// variants pinning its rendition id at `position.0 + 1` among
/// `position.1` groups.
///
/// # Errors
///
/// Returns any GPU readback-owner or rendition-identity failure.
#[cfg(feature = "native-media")]
fn new_stream_group(
    runtime: &NativeMediaRuntime,
    format: &RecordFormat,
    (width, height): (NonZeroU32, NonZeroU32),
    key: StreamGroupKey,
    (group_index, group_count): (usize, usize),
    next_sequence: u64,
) -> AppResult<StreamRenditionGroup> {
    let (output, _, video_bitrate_kbps) = key;
    let readback = runtime.create_program_readback_blocking(width, height)?;
    Ok(StreamRenditionGroup {
        core: StreamRenditionCore {
            output,
            members: Vec::new(),
            rendition: RenditionId::new(
                NonZeroU32::new(
                    u32::try_from(group_index + 1)
                        .expect("bounded group count fits a rendition ordinal"),
                )
                .expect("rendition ordinals start at one"),
            ),
            variants: pinned_rendition_variants(
                format,
                video_bitrate_kbps,
                group_index,
                group_count,
            )?,
            next_sequence,
        },
        format: format.clone(),
        readback,
        key,
    })
}

/// The Program audio of one realized frame: the faded mix every output Fade
/// to Black affects hears, while it fades and excludes some output the mix
/// before the fade, and the mix of every output fading over its own
/// duration on a different trajectory (SW-004).
#[cfg(feature = "native-media")]
struct CaptureAudio<'a> {
    faded: AudioBlock,
    live: Option<AudioBlock>,
    excluded: &'a [fm_types::OutputId],
    overrides: &'a [(fm_types::OutputId, AudioBlock)],
}

#[cfg(feature = "native-media")]
impl CaptureAudio<'_> {
    /// The mix a capture of `selected` hears.
    fn for_output(
        &self,
        selected: Option<fm_types::OutputId>,
        project_outputs: &[NativeOutputFrame],
    ) -> &AudioBlock {
        let output = capture_output_id(selected, project_outputs);
        if let Some((_, block)) = self
            .overrides
            .iter()
            .find(|(candidate, _)| Some(*candidate) == output)
        {
            return block;
        }
        match &self.live {
            Some(live) if output.is_some_and(|output| self.excluded.contains(&output)) => live,
            _ => &self.faded,
        }
    }
}

/// The project output a capture of `selected` follows: the selection, or
/// the first realized output when it selects none.
#[cfg(feature = "native-media")]
fn capture_output_id(
    selected: Option<fm_types::OutputId>,
    project_outputs: &[NativeOutputFrame],
) -> Option<fm_types::OutputId> {
    selected.or_else(|| project_outputs.first().map(NativeOutputFrame::output))
}

/// Resolves an optional project output selection against the realized per-output
/// entries for a frame, falling back to `unbound_output` when the project
/// declares no outputs.
#[cfg(any(test, feature = "native-media"))]
fn resolve_project_output_entry<'a, F, T: ?Sized>(
    selected: Option<fm_types::OutputId>,
    project_outputs: &'a [F],
    unbound_output: Option<&'a T>,
    output_id: impl Fn(&F) -> fm_types::OutputId,
    extract: impl Fn(&'a F) -> &'a T,
) -> Option<&'a T> {
    match selected {
        Some(target) => {
            if project_outputs.is_empty() {
                unbound_output
            } else {
                project_outputs
                    .iter()
                    .find(|frame| output_id(frame) == target)
                    .map(extract)
            }
        }
        None => project_outputs.first().map(extract).or(unbound_output),
    }
}

/// Resolves an optional project output selection to the realized texture for
/// the current frame.
#[cfg(feature = "native-media")]
fn resolve_project_output_texture<'a>(
    selected: Option<fm_types::OutputId>,
    project_outputs: &'a [NativeOutputFrame],
    unbound_output: Option<&'a NativeTexture>,
) -> Option<&'a NativeTexture> {
    resolve_project_output_entry(
        selected,
        project_outputs,
        unbound_output,
        NativeOutputFrame::output,
        NativeOutputFrame::texture,
    )
}

/// Resolves a display output's source to the realized texture for the
/// current frame. Program is the first realized project output (or the
/// unbound output); Preview is the Preview composite rendered for display
/// outputs.
#[cfg(feature = "native-media")]
fn resolve_display_source_texture<'a>(
    source: DisplayOutputSource,
    project_outputs: &'a [NativeOutputFrame],
    unbound_output: Option<&'a NativeTexture>,
    preview: Option<&'a NativeTexture>,
) -> Option<&'a NativeTexture> {
    match source {
        DisplayOutputSource::Program => {
            resolve_project_output_texture(None, project_outputs, unbound_output)
        }
        DisplayOutputSource::Output(output) => {
            resolve_project_output_texture(Some(output), project_outputs, unbound_output)
        }
        DisplayOutputSource::Preview => preview,
    }
}

/// `frame` as Preview shows it: the realized Preview input cut in, with no
/// transition, Stinger, overlay, title, or Fade to Black. Scenes and media
/// keep the frame's poses and transport positions.
#[cfg(feature = "native-media")]
fn preview_frame(mut frame: FrameResult, preview: InputId) -> FrameResult {
    frame.program = fm_switcher::ProgramFrame {
        primary: preview,
        secondary: None,
        transition_kind: None,
        mix_numerator: 0,
        mix_denominator: 1,
        mix_start_numerator: 0,
        mix_end_numerator: 0,
    };
    frame.fade_to_black = fm_switcher::FadeToBlackFrame::LIVE;
    frame.fade_to_black_overrides.clear();
    frame.manual_transition = false;
    frame.overlays = std::array::from_fn(|_| fm_switcher::OverlayChannelState::empty());
    frame.titles.clear();
    frame.events.clear();
    frame.input_audio_strip_updates.clear();
    frame.audio_mixer_updates.clear();
    frame
}

#[cfg(feature = "native-media")]
impl NativeProgramStream {
    /// Compiles one runtime per configured project destination in project
    /// vector order, registering each target in the shared output set as
    /// destination slot 1..=N, and groups targets whose encoder-relevant
    /// settings — output protocol plus authored video bitrate — match into
    /// one shared-rendition group owning one readback and one packed pair
    /// per frame.
    ///
    /// Also yields the absolute directory of every authored local HLS target,
    /// in authored order, for the daemon's startup preflight and runtime
    /// free-space sampler.
    ///
    /// Startup fails closed on more targets than the engine inventories or the
    /// output set can carry, or a destination URL that would be refused at
    /// connect time; all three are authoring errors the model validation
    /// mirrors, so refusing before any process spawns keeps them visible
    /// instead of latching every target failed at air time.
    fn compile(
        runtime: &NativeMediaRuntime,
        stored: &StoredProject,
        output_set: &mut OutputSet,
    ) -> AppResult<(Vec<Self>, Vec<StreamRenditionGroup>, Vec<PathBuf>)> {
        let targets = stored.project().stream_targets();
        if targets.len() > fm_switcher::MAX_STREAM_COUNT {
            return Err(AppFailure(format!(
                "stream target count {} exceeds the engine inventory limit {}",
                targets.len(),
                fm_switcher::MAX_STREAM_COUNT
            ))
            .into());
        }
        if targets.len() > fm_io_network::MAX_DESTINATIONS {
            return Err(AppFailure(format!(
                "stream target count {} exceeds the output set limit {}",
                targets.len(),
                fm_io_network::MAX_DESTINATIONS
            ))
            .into());
        }
        let settings = stored.project().settings();
        let frames_rendered = stored.position().frames_rendered;
        // The record format derives from project settings alone, so it is
        // identical across every target by construction; each group packs its
        // pairs with a clone of this one format.
        let format = stream_record_format(settings, frames_rendered)?;
        let split_targets = split_and_group_targets(targets)?;
        let local_output_dirs = local_output_directories(&split_targets.splits);
        let mut groups = build_stream_groups(
            runtime,
            &format,
            NonZeroU32::new(settings.video.dimensions.width()).expect("project width is nonzero"),
            NonZeroU32::new(settings.video.dimensions.height()).expect("project height is nonzero"),
            &split_targets.group_keys,
            frames_rendered,
        )?;
        // One transport per target under its group's rendition identity;
        // sinks refuse packets claiming any other rendition.
        let mut streams = Vec::with_capacity(split_targets.splits.len());
        for (index, split) in split_targets.splits.into_iter().enumerate() {
            // Destination slots are 1..=N in authored order; both bounds were
            // checked above.
            let ordinal =
                u32::try_from(index + 1).expect("target count fits the destination slots");
            let destination_id = DestinationId::new(
                u8::try_from(ordinal).expect("bounded target count fits a slot"),
            )
            .expect("bounded target count yields a valid slot");
            let group = &mut groups[split.group_index];
            streams.push(Self::realize(
                split,
                destination_id,
                group,
                &format,
                frames_rendered,
                output_set,
            ));
        }
        Ok((streams, groups, local_output_dirs))
    }

    /// Realizes one validated target as destination slot `destination_id`
    /// in `group`: its own sink under the group's rendition identity, its own
    /// bounded queue and reconnect policy in the shared output set, and fresh
    /// feed bookkeeping starting at `next_sequence`.
    fn realize(
        split: SplitStreamTarget<'_>,
        destination_id: DestinationId,
        group: &mut StreamRenditionGroup,
        format: &RecordFormat,
        next_sequence: u64,
        output_set: &mut OutputSet,
    ) -> Self {
        group.core.members.push(destination_id);
        let mut sink_config = RtmpSinkConfig::new(group.core.rendition, format.clone());
        sink_config.limits.stop_timeout = PROGRAM_STREAM_STOP_TIMEOUT;
        sink_config.limits.kill_timeout = PROGRAM_STREAM_KILL_TIMEOUT;
        sink_config.encoder = stream_encoder_settings(split.target.video_bitrate_kbps());
        sink_config.stream_key = split.stream_key;
        sink_config.ladder = stream_ladder(split.target);
        sink_config.srt = stream_srt_options(split.target);
        output_set
            .add_destination(
                DestinationConfig::new(
                    destination_id,
                    split.protocol,
                    split.endpoint.clone(),
                    split.backup_endpoint,
                    // TLS policy was already settled by the protocol rules:
                    // plain RTMP and SRT refuse it and RTMPS uses the
                    // child's default trust, so no daemon-side TLS exists.
                    None,
                    None,
                    QueueCapacity::new(PROGRAM_STREAM_QUEUE_CAPACITY)
                        .expect("the feed queue capacity is nonzero"),
                    stream_reconnect_policy(split.target.reconnect()),
                )
                .expect("compiled destinations satisfy every configuration rule"),
            )
            .expect("one free output-set slot per realized target");
        Self {
            target: SwitcherStreamTargetId::from_non_zero(split.target.id().get()),
            destination_id,
            redacted_destination: redacted_destination_url(
                split.protocol,
                &split.endpoint,
                sink_config.stream_key.is_some(),
            ),
            sink: stream_transport::BackgroundTransport::new(
                FfmpegRtmpSink::new(sink_config),
                format!("freemix-native-stream-{}", destination_id.get()),
            ),
            ledger: StreamPairLedger {
                next_sequence,
                ..StreamPairLedger::default()
            },
            first_failure: None,
            enqueue_backpressure: false,
            started: false,
            meter: StreamLinkMeter::default(),
            authored: split.target.clone().set_running(false),
        }
    }

    /// Clears a latched failure and reconnects this target from the first
    /// attempt (OR-006 retry): the transport stops, and the next frame
    /// boundary starts it again with a fresh reconnect budget.
    fn retry(&mut self, output_set: &mut OutputSet) {
        self.stop_requested(output_set);
    }

    /// Marks this target running in the shared output set. Connection trouble
    /// does not latch here: the set owns retrying and failover, and only its
    /// own budget exhaustion latches the target through [`Self::poll`].
    fn start(&mut self, output_set: &mut OutputSet) {
        if self.first_failure.is_some() {
            return;
        }
        self.started = true;
        if let Err(error) = output_set.start(self.destination_id) {
            self.fail(&format!("start:{error}"), output_set);
        }
    }

    /// Stops this target off the render path: the set discards queued pairs
    /// and disconnects the transport under its bounded deadlines.
    /// Bookkeeping stays with the target so a later `StreamStart` can relaunch
    /// it and shutdown reports whole-run counters.
    fn stop_live(&mut self, output_set: &mut OutputSet) {
        let _ = output_set.stop(self.destination_id, &mut self.sink);
    }

    /// Stops this target because the operator asked to. The deliberate stop
    /// also clears a latched failure, so a later `StreamStart` relaunches the
    /// target instead of leaving it failed for the rest of the run.
    fn stop_requested(&mut self, output_set: &mut OutputSet) {
        self.stop_live(output_set);
        self.first_failure = None;
    }

    /// Whether any transport child this target started was not reaped within
    /// its deadline; shutdown reports this instead of abandoning it silently.
    ///
    /// Waits for the transport worker's outstanding stop, which the sink
    /// bounds by its own stop and kill deadlines; only shutdown calls this.
    fn shutdown_error(&mut self) -> Option<AppResult<()>> {
        let Some(sink) = self.sink.settle() else {
            return Some(Err(AppFailure(format!(
                "stream target {} did not shut down cleanly: transport worker exited",
                self.target
            ))
            .into()));
        };
        let unconfirmed = sink.unconfirmed_cleanups();
        if unconfirmed == 0 {
            return None;
        }
        Some(Err(AppFailure(format!(
            "stream target {} did not shut down cleanly: unconfirmed_cleanups={unconfirmed}",
            self.target
        ))
        .into()))
    }

    /// Advances this target one step in the shared output set after the
    /// frame's enqueues: connects, backs off, fails over, and writes.
    ///
    /// The first failure the set cannot recover from latches the target with
    /// one sanitized notice, exactly like every other feed failure.
    ///
    /// A connect the set is about to make runs on the transport worker: until
    /// it is realized this destination is not polled at all, so the set stays
    /// `Connecting` (or due to reconnect) and the daemon loop keeps running.
    fn poll(&mut self, output_set: &mut OutputSet, now_ms: u64) {
        self.sink.advance();
        if self.first_failure.is_some() {
            return;
        }
        if let Some(ready) = stream_transport::connect_due(output_set, self.destination_id, now_ms)
            .map(|(config, endpoint)| self.sink.prepare_connect(config, endpoint))
            && !ready
        {
            return;
        }
        match output_set.poll(self.destination_id, now_ms, &mut self.sink) {
            Ok(PollEvent::Failed) => {
                let failure = output_set
                    .telemetry(self.destination_id)
                    .and_then(|telemetry| telemetry.latest_failure())
                    .map_or_else(
                        || "transport:budget_exhausted".to_owned(),
                        |failure| format!("transport:{:?}:{}", failure.stage, failure.message),
                    );
                self.fail(&failure, output_set);
            }
            Ok(_) => {}
            Err(error) => self.fail(&format!("transport:{error}"), output_set),
        }
    }

    /// Latches the first failure per target and stops its transport under the
    /// bounded stop deadline.
    fn fail(&mut self, failure: &str, output_set: &mut OutputSet) {
        if self.first_failure.is_some() {
            return;
        }
        self.first_failure = Some(failure.to_owned());
        eprintln!(
            "FREEMIXD_STREAM_FAILURE\tv=1\ttarget={}\tdestination={}\tfailure={}",
            self.target,
            diagnostic_field(&self.redacted_destination),
            diagnostic_field(failure),
        );
        let _ = output_set.stop(self.destination_id, &mut self.sink);
    }

    /// Emits the single sanitized end-of-run record for this target from the
    /// shared output set's frozen state and telemetry plus the feed ledger.
    ///
    /// Only redacted destinations and network telemetry fields reach the
    /// record: neither the stream key nor child stderr URL text can appear.
    fn emit_final_record(&self, output_set: &OutputSet) {
        let state = output_set.state(self.destination_id);
        let telemetry = output_set.telemetry(self.destination_id);
        let muxed_bytes = telemetry.map_or(0, NetworkTelemetry::bytes_sent);
        let out_time_us = telemetry
            .and_then(fm_io_network::NetworkTelemetry::last_successful_media_timestamp)
            .map_or(0, |timestamp| {
                u64::try_from(timestamp.as_nanos() / 1_000).unwrap_or(u64::MAX)
            });
        let connected = matches!(
            state,
            Some(
                DestinationState::Live
                    | DestinationState::Congested
                    | DestinationState::AwaitingRandomAccess
            )
        );
        let state_name = match state {
            _ if self.first_failure.is_some() => "Failed",
            None => "NotStarted",
            Some(DestinationState::Failed) => "Failed",
            Some(DestinationState::Stopped) => "Stopped",
            Some(DestinationState::Connecting | DestinationState::AwaitingRandomAccess) => {
                "Starting"
            }
            Some(DestinationState::WaitingToReconnect { .. }) => "WaitingToReconnect",
            Some(DestinationState::Congested) => "Congested",
            Some(DestinationState::Live) => "Live",
        };
        let failure_text = self
            .first_failure
            .clone()
            .or_else(|| {
                telemetry
                    .and_then(|telemetry| telemetry.latest_failure())
                    .map(|failure| format!("transport:{:?}:{}", failure.stage, failure.message))
            })
            .unwrap_or_else(|| "none".to_owned());
        eprintln!(
            "FREEMIXD_STREAM\tv=1\ttarget={}\tdestination={}\tstate={state_name}\tmuxed_bytes={muxed_bytes}\tout_time_us={out_time_us}\tconnected={connected}\tenqueued_pairs={}\tdropped_pairs={}\tfailure={}",
            self.target,
            diagnostic_field(&self.redacted_destination),
            self.ledger.enqueued_pairs,
            self.ledger.dropped_pairs,
            diagnostic_field(&failure_text),
        );
    }
}

/// Whether a target's route joins this frame's rendition fan-out: it was
/// asked to run, never latched a failure, and was not deliberately stopped.
/// Stopped and unstarted destinations are excluded from the plan itself, so
/// nothing is ever queued for a route nobody drains.
#[cfg(feature = "native-media")]
fn route_active(stream: &NativeProgramStream, output_set: &OutputSet) -> bool {
    stream.started
        && stream.first_failure.is_none()
        && !matches!(
            output_set.state(stream.destination_id),
            Some(DestinationState::Stopped)
        )
}

/// One rendition group's share of the frame pipeline.
#[cfg(feature = "native-media")]
enum GroupFrameOutcome {
    /// Nothing to send: the group has no active member route, or this
    /// frame's sequence was already consumed.
    Idle,
    /// One packed pair plus the plan that routes it to the active members.
    Packed {
        plan: RenditionPlan,
        packet: OutputPacket,
    },
    /// The shared step failed; every active member latches its text.
    Failed(Vec<(DestinationId, String)>),
}

/// Gates, plans, and packs one rendition group's pair for this frame — split
/// from the GPU-owning daemon so hermetic tests drive the exact production
/// sequencing with a scripted pack step instead of a readback.
///
/// A group with no active member packs nothing (its readback idles). The
/// sequence cursor advances past this frame once the group commits to
/// packing: a packed pair is never repacked because sinks pad gaps to hold
/// the media clock. Plans cover exactly the active members; planner failures
/// and pack failures return per-member texts to latch.
#[cfg(feature = "native-media")]
fn plan_and_pack_group_frame(
    members: &[DestinationId],
    variants: &[AbrVariant],
    next_sequence: &mut u64,
    sequence: SequenceNumber,
    active: &[bool],
    pack: impl FnOnce() -> Result<OutputPacket, String>,
) -> GroupFrameOutcome {
    let active_members: Vec<DestinationId> = members
        .iter()
        .zip(active)
        .filter_map(|(member, &is_active)| is_active.then_some(*member))
        .collect();
    if active_members.is_empty() || sequence.get() < *next_sequence {
        return GroupFrameOutcome::Idle;
    }
    *next_sequence = sequence.get().saturating_add(1);
    let plan = match build_group_plan(members, variants, active) {
        Ok(plan) => plan,
        Err(error) => {
            return GroupFrameOutcome::Failed(
                active_members
                    .into_iter()
                    .map(|destination| (destination, format!("planner:{error}")))
                    .collect(),
            );
        }
    };
    match pack() {
        Ok(packet) => GroupFrameOutcome::Packed { plan, packet },
        Err(failure) => GroupFrameOutcome::Failed(
            active_members
                .into_iter()
                .map(|destination| (destination, failure.clone()))
                .collect(),
        ),
    }
}

/// Builds the planner request set for one group over its active members:
/// every active member requests the group's variant ladder, so the planner
/// deduplicates them into renditions whose ids match the compile-time group
/// ids (the real profile sits at the group's pinned position).
///
/// # Errors
///
/// Returns [`fm_io_network::RenditionError`] for an empty request set or any
/// other planner rule violation.
#[cfg(feature = "native-media")]
fn build_group_plan(
    members: &[DestinationId],
    variants: &[AbrVariant],
    active: &[bool],
) -> Result<RenditionPlan, fm_io_network::RenditionError> {
    let mut requests = Vec::new();
    for (member, is_active) in members.iter().zip(active) {
        if !is_active {
            continue;
        }
        let request = match variants {
            [only] => DestinationRenditions::single(*member, only.profile().clone()),
            many => DestinationRenditions::ladder(
                *member,
                AbrLadder::new(many.to_vec())
                    .expect("compiled rendition variants stay a valid ladder"),
            ),
        };
        requests.push(request);
    }
    RenditionPlanner::plan(&requests)
}

/// Maps one fanned-out packet's per-route results back onto the member
/// ledgers: an accepted route advances that member's counter only, a refused
/// route drops that member's counter and flags its backpressure, and a route
/// to a destination missing from the set drops without touching anything
/// else. A refused pair is gone — never retried — because the sink pads its
/// gap to hold the media clock.
#[cfg(feature = "native-media")]
fn admit_rendition_outcomes(
    outcomes: &[DestinationEnqueue],
    sequence: SequenceNumber,
    routes: &mut [(DestinationId, &mut StreamPairLedger, &mut bool)],
) {
    for outcome in outcomes {
        let Some((_, ledger, enqueue_backpressure)) = routes
            .iter_mut()
            .find(|(destination, _, _)| *destination == outcome.destination)
        else {
            continue;
        };
        match outcome.status {
            EnqueueStatus::Accepted => ledger.note_admitted(sequence),
            EnqueueStatus::Backpressure(_) => {
                ledger.note_dropped();
                **enqueue_backpressure = true;
            }
            EnqueueStatus::DestinationUnavailable(_) => ledger.note_dropped(),
        }
    }
}

/// The production pack step for one group: ONE synchronous program readback
/// through the shared owner, layout validation, and ONE raw pair packed under
/// the group's rendition identity. This is the existing synchronous
/// diagnostic readback contract, not a nonblocking or zero-copy encoder
/// bridge; the failure texts mirror the per-target path they replaced.
#[cfg(feature = "native-media")]
fn pack_group_pair(
    runtime: &NativeMediaRuntime,
    program: &NativeTexture,
    readback: &mut NativeProgramReadback,
    format: &RecordFormat,
    rendition: RenditionId,
    sequence: SequenceNumber,
    audio: &AudioBlock,
) -> Result<OutputPacket, String> {
    // This is the expensive synchronous step sharing removes duplicates of.
    let frame = runtime
        .readback_program_blocking(readback, program)
        .map_err(|error| format!("readback:{error}"))?;
    let Some(expected_stride) = frame.width.checked_mul(4) else {
        return Err("readback:stride_overflow".to_owned());
    };
    if frame.width != readback.width()
        || frame.height != readback.height()
        || frame.stride != expected_stride
    {
        return Err("readback:invalid_tight_layout".to_owned());
    }
    raw_pair_packet(
        rendition,
        format,
        sequence.get(),
        &frame.rgba,
        &interleaved_audio_bytes(audio),
    )
    .map_err(|error| format!("frame:{error}"))
}

/// Serializes one [`AudioBlock`] into the sample-major `f32le` span the raw
/// pair packet carries: per sample index, every channel's `f32` back to back.
/// Planes are equal-length by construction.
#[cfg(feature = "native-media")]
fn interleaved_audio_bytes(block: &AudioBlock) -> Vec<u8> {
    let planes = block.planes();
    let samples = planes.first().map_or(0, Vec::len);
    let mut bytes = Vec::with_capacity(samples * planes.len() * size_of::<f32>());
    for index in 0..samples {
        for plane in planes {
            bytes.extend_from_slice(&plane[index].to_le_bytes());
        }
    }
    bytes
}

/// Plain sampling inputs for one configured stream target, extracted so the
/// status mapping stays testable without a GPU runtime or an `FFmpeg` child.
#[cfg(feature = "native-media")]
struct StreamRuntimeSnapshot<'a> {
    target: SwitcherStreamTargetId,
    ledger: StreamPairLedger,
    /// Capacity backpressure observed since the previous sample.
    congested: bool,
    first_failure: Option<&'a str>,
    /// Whether this target was ever asked to run.
    started: bool,
    /// The shared output set's view of this target.
    state: Option<DestinationState>,
    network: Option<NetworkTelemetry>,
    /// Latest cached local HLS output health for this target's slot; always
    /// `None` for a network target.
    local: Option<StreamLocalStatus>,
    /// How long the current session has delivered media, in milliseconds.
    uptime_ms: u64,
    /// The encoded outgoing bitrate over the last meter window.
    bitrate_kbps: u32,
}

/// How long one bitrate window spans: two seconds smooths the encoder's
/// progress reports without hiding a stalled link for long.
#[cfg(feature = "native-media")]
const STREAM_BITRATE_WINDOW_MS: u64 = 2_000;

/// Per-target link meter for the lossy status record (OR-006): when the
/// current session started delivering media, and the encoded bitrate the
/// child reports muxing over the last window. Everything resets when the
/// session stops delivering.
#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StreamLinkMeter {
    live_since_ms: Option<u64>,
    /// `(at_ms, muxed_bytes)` at the start of the current window.
    window: Option<(u64, u64)>,
    bitrate_kbps: u32,
}

#[cfg(feature = "native-media")]
impl StreamLinkMeter {
    /// Advances the meter to `now_ms` and returns `(uptime_ms, bitrate_kbps)`.
    /// `muxed_bytes` is the open session's cumulative encoded output; a value
    /// below the window's start (a new session) restarts the window.
    fn observe(&mut self, now_ms: u64, delivering: bool, muxed_bytes: Option<u64>) -> (u64, u32) {
        if !delivering {
            *self = Self::default();
            return (0, 0);
        }
        let since = *self.live_since_ms.get_or_insert(now_ms);
        if let Some(bytes) = muxed_bytes {
            match self.window {
                Some((at, start)) if bytes >= start => {
                    let elapsed = now_ms.saturating_sub(at);
                    if elapsed >= STREAM_BITRATE_WINDOW_MS {
                        // Bits per millisecond is kilobits per second.
                        let kbps = (bytes - start).saturating_mul(8) / elapsed;
                        self.bitrate_kbps = u32::try_from(kbps).unwrap_or(u32::MAX);
                        self.window = Some((now_ms, bytes));
                    }
                }
                _ => self.window = Some((now_ms, bytes)),
            }
        }
        (now_ms.saturating_sub(since), self.bitrate_kbps)
    }
}

/// One local HLS output's latest bounded health, or `None` when its published
/// playlist cannot be read or parsed.
///
/// A single-rendition sink publishes `<directory>/index.m3u8`. A ladder sink
/// publishes a master playlist at that path whose first variant is a
/// `<rung>/index.m3u8` inside the same directory, so a master is followed to
/// its first variant before parsing. The variant path is validated against the
/// same plain shapes the HLS listener serves — `index.m3u8` or one rung digit
/// `0..=7` plus `index.m3u8` — so `..`, absolute paths, and backslashes are
/// refused before any read.
///
/// `segments` and `target_duration_ms` come from the bounded M3U8 parser and
/// saturate to the wire bounds; `free_bytes` measures the directory's nearest
/// existing ancestor and is zero when the query is unavailable. Every read,
/// parse, or shape failure returns `None` without latching: a missing or
/// malformed playlist is a lossy sample gap, never a target failure.
#[cfg(feature = "native-media")]
fn local_hls_status(directory: &Path) -> Option<StreamLocalStatus> {
    local_hls_status_with(directory, |ancestor| disk::available_bytes(ancestor).ok())
}

/// [`local_hls_status`] with the free-space query `available`, which is
/// given the directory's nearest existing ancestor. A test pins it: other
/// writers on the same file system change the free space between any two
/// real queries.
#[cfg(feature = "native-media")]
fn local_hls_status_with(
    directory: &Path,
    available: impl FnOnce(&Path) -> Option<u64>,
) -> Option<StreamLocalStatus> {
    let text = std::fs::read_to_string(directory.join("index.m3u8")).ok()?;
    let text = match parse_master_m3u8(&text) {
        Ok(variants) => {
            let variant = variants.first()?;
            if !valid_local_hls_variant_path(variant.playlist_uri()) {
                return None;
            }
            std::fs::read_to_string(directory.join(variant.playlist_uri())).ok()?
        }
        Err(_) => text,
    };
    let rendition = RenditionId::new(NonZeroU32::new(1).expect("the probe rendition is nonzero"));
    let metadata = HlsPlaylistMetadata::parse_m3u8(&text, rendition, 1).ok()?;
    let segments = u32::try_from(metadata.segments().len()).unwrap_or(u32::MAX);
    let target_duration_ms =
        u32::try_from(metadata.target_duration().as_nanos() / 1_000_000).unwrap_or(u32::MAX);
    Some(StreamLocalStatus {
        segments: segments.min(MAX_STREAM_LOCAL_SEGMENTS),
        target_duration_ms: target_duration_ms.min(MAX_STREAM_LOCAL_TARGET_DURATION_MS),
        free_bytes: disk::existing_ancestor(directory)
            .and_then(available)
            .unwrap_or(0),
    })
}

/// Whether one master-playlist variant path names a playlist inside the output
/// directory: `index.m3u8` or `<rung>/index.m3u8` with one rung digit `0..=7`,
/// exactly the shapes the HLS listener serves. Empty names, `..`, absolute
/// paths, and backslashes are refused.
#[cfg(feature = "native-media")]
fn valid_local_hls_variant_path(relative: &str) -> bool {
    match relative.split_once('/') {
        None => relative == "index.m3u8",
        Some((rung, leaf)) => {
            let mut bytes = rung.bytes();
            matches!(
                (bytes.next(), bytes.next()),
                (Some(byte), None) if byte.is_ascii_digit() && byte <= b'7'
            ) && leaf == "index.m3u8"
        }
    }
}

/// The cached local HLS health for one destination slot, or `None` for a
/// network target or a slot that has no sampled health yet.
#[cfg(feature = "native-media")]
fn cached_local_hls_status(
    output_set: &OutputSet,
    local_output_status: &[Option<StreamLocalStatus>; fm_switcher::MAX_STREAM_COUNT],
    destination: DestinationId,
) -> Option<StreamLocalStatus> {
    let config = output_set.config(destination)?;
    if config.protocol() != OutputProtocol::Hls {
        return None;
    }
    local_output_status
        .get(usize::from(destination.get()) - 1)
        .copied()
        .flatten()
}

/// Assembles the per-pass status record: `None` when no target is active or
/// latched, otherwise the message with its monotonic sequence advanced by one
/// from the previous publish (the first publish is sequence 1).
#[cfg(feature = "native-media")]
fn stream_status_message(
    server: &ServerIdentity,
    previous_sequence: u64,
    samples: Vec<StreamStatusSample>,
) -> AppResult<Option<StreamStatusMessage>> {
    if samples.is_empty() {
        return Ok(None);
    }
    let sequence = previous_sequence
        .checked_add(1)
        .ok_or_else(|| AppFailure("stream status sequence exhausted".into()))?;
    Ok(Some(StreamStatusMessage {
        server: server.clone(),
        sequence,
        samples,
    }))
}

#[cfg(feature = "native-media")]
fn native_stream_snapshots<'a>(
    output_set: &'a OutputSet,
    streams: &'a mut [NativeProgramStream],
    local_output_status: &[Option<StreamLocalStatus>; fm_switcher::MAX_STREAM_COUNT],
    now_ms: u64,
) -> Vec<StreamRuntimeSnapshot<'a>> {
    streams
        .iter_mut()
        .map(|stream| {
            let state = output_set.state(stream.destination_id);
            let delivering = stream.first_failure.is_none()
                && matches!(
                    state,
                    Some(DestinationState::Live | DestinationState::Congested)
                );
            let muxed_bytes = stream
                .sink
                .idle_sink()
                .and_then(FfmpegRtmpSink::stream_telemetry)
                .map(|telemetry| telemetry.muxed_bytes);
            let (uptime_ms, bitrate_kbps) = stream.meter.observe(now_ms, delivering, muxed_bytes);
            StreamRuntimeSnapshot {
                congested: std::mem::take(&mut stream.enqueue_backpressure),
                target: stream.target,
                ledger: stream.ledger,
                first_failure: stream.first_failure.as_deref(),
                started: stream.started,
                state,
                network: output_set.telemetry(stream.destination_id).cloned(),
                local: cached_local_hls_status(
                    output_set,
                    local_output_status,
                    stream.destination_id,
                ),
                uptime_ms,
                bitrate_kbps,
            }
        })
        .collect()
}

/// Builds one lossy status sample per active or latched stream runtime.
///
/// Targets that never started (and never latched a failure) are omitted, so
/// an all-inactive set yields no samples and the daemon publishes nothing.
/// Output is bounded to [`MAX_STREAM_SAMPLES`] and sorted into the strictly
/// ascending target order the wire contract requires.
#[cfg(feature = "native-media")]
fn stream_status_samples<'a>(
    runtimes: impl IntoIterator<Item = StreamRuntimeSnapshot<'a>>,
) -> Vec<StreamStatusSample> {
    let mut samples: Vec<_> = runtimes
        .into_iter()
        .filter_map(stream_status_sample)
        .collect();
    samples.sort_by_key(|sample| sample.target.get());
    samples.truncate(MAX_STREAM_SAMPLES);
    samples
}

/// Shortens a sanitized failure to the wire's detail bound on a character
/// boundary: a latched transport failure reads
/// `transport:<stage>:<message>` and can exceed it, and one oversized sample
/// would otherwise make the whole lossy status record unencodable.
#[cfg(feature = "native-media")]
fn clamp_stream_failure(mut failure: String) -> String {
    if failure.len() > fm_protocol::MAX_STREAM_DETAIL_BYTES {
        let mut end = fm_protocol::MAX_STREAM_DETAIL_BYTES;
        while !failure.is_char_boundary(end) {
            end -= 1;
        }
        failure.truncate(end);
    }
    failure
}

/// Maps one target's runtime onto its protocol sample.
///
/// Counters come from the per-target feed ledger; `connected` and muxed bytes
/// come from the shared output set's telemetry. The first latched failure is
/// sticky and always wins over whatever the transport last reported, mirroring
/// `emit_final_record`. `local` is the cached local HLS output health the
/// snapshot already filtered to HLS targets.
#[cfg(feature = "native-media")]
fn stream_status_sample(runtime: StreamRuntimeSnapshot<'_>) -> Option<StreamStatusSample> {
    let StreamRuntimeSnapshot {
        target,
        ledger,
        congested,
        first_failure,
        started,
        state,
        network,
        local,
        uptime_ms,
        bitrate_kbps,
    } = runtime;
    if !started && first_failure.is_none() {
        // Configured but never started: an inactive target stays off the wire.
        return None;
    }
    let network = network.as_ref();
    let failure = first_failure
        .map(str::to_owned)
        .or_else(|| {
            network
                .and_then(|telemetry| telemetry.latest_failure())
                .map(|failure| format!("{:?}:{}", failure.stage, failure.message))
        })
        .map(clamp_stream_failure);
    Some(StreamStatusSample {
        target: WireStreamTargetId::new(target.get()),
        realized: realized_stream_state(first_failure.is_some(), congested, state),
        connected: matches!(
            state,
            Some(
                DestinationState::Live
                    | DestinationState::Congested
                    | DestinationState::AwaitingRandomAccess
            )
        ) || network.is_some_and(|telemetry| telemetry.packets_sent() > 0),
        muxed_bytes: network.map_or(0, NetworkTelemetry::bytes_sent),
        enqueued_pairs: ledger.enqueued_pairs,
        dropped_pairs: ledger.dropped_pairs,
        reconnects: network.map_or(0, NetworkTelemetry::reconnects),
        uptime_ms,
        bitrate_kbps,
        failure,
        local,
    })
}

/// Maps the shared output set's state onto [`StreamRealizedState`].
///
/// Mapping choices, deliberately:
///
/// - A latched failure wins outright: `Failed`. The set's own `Failed` — a
///   spent retry budget or a terminal cause — maps the same way.
/// - `Starting` covers both the connecting slot and a reconnected route still
///   waiting for its first random-access pair.
/// - A live route is `Live`, or `Congested` when either the transport reports
///   congestion or the feed observed enqueue backpressure this interval.
/// - `WaitingToReconnect` is now reachable wire state: every target routes
///   through the shared output set, whose bounded backoff sits between a lost
///   destination and its next attempt. A stopped slot reports `Stopped`; the
///   cleanly stopped targets then leave the active set entirely.
#[cfg(feature = "native-media")]
fn realized_stream_state(
    failed: bool,
    congested: bool,
    state: Option<DestinationState>,
) -> StreamRealizedState {
    if failed {
        return StreamRealizedState::Failed;
    }
    match state {
        None | Some(DestinationState::Stopped) => StreamRealizedState::Stopped,
        Some(DestinationState::Connecting | DestinationState::AwaitingRandomAccess) => {
            StreamRealizedState::Starting
        }
        Some(DestinationState::Failed) => StreamRealizedState::Failed,
        Some(DestinationState::WaitingToReconnect { .. }) => {
            StreamRealizedState::WaitingToReconnect
        }
        Some(DestinationState::Congested) => StreamRealizedState::Congested,
        Some(DestinationState::Live) if congested => StreamRealizedState::Congested,
        Some(DestinationState::Live) => StreamRealizedState::Live,
    }
}

#[cfg(feature = "native-media")]
#[derive(Debug)]
enum NativeRealizationError {
    Video(NativeSourceRenderError),
    Audio(NativeMasterError),
}

#[cfg(feature = "native-media")]
impl fmt::Display for NativeRealizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Video(error) => error.fmt(formatter),
            Self::Audio(error) => error.fmt(formatter),
        }
    }
}

#[cfg(feature = "native-media")]
impl Error for NativeRealizationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Video(error) => Some(error),
            Self::Audio(error) => Some(error),
        }
    }
}

#[cfg(feature = "native-media")]
impl NativeDaemon {
    fn invalidate_projection(&mut self) {
        self.projected_frame = None;
    }

    fn start(
        store: &ProjectStore,
        stored: &StoredProject,
        camera_helper: Option<&Path>,
    ) -> AppResult<Self> {
        Self::start_with_optional_context(store, stored, None, camera_helper)
    }

    #[allow(clippy::too_many_lines)]
    fn start_with_optional_context(
        store: &ProjectStore,
        stored: &StoredProject,
        context: Option<NativeContext>,
        camera_helper: Option<&Path>,
    ) -> AppResult<Self> {
        let project_plan =
            NativeProjectPlan::compile(stored.project(), NativeProjectLimits::default())?;
        validate_native_audio_modes(stored)?;
        #[cfg(target_os = "macos")]
        let mut resolution = resolve_native_sources(store, stored, camera_helper)?;
        #[cfg(not(target_os = "macos"))]
        let resolution = resolve_native_sources(store, stored, camera_helper)?;
        let mut sources = resolution.sources;
        let titles = resolve_native_title_sources(store, stored, &mut sources)?;
        validate_native_stinger_sources(stored, &sources)?;
        let requires_ffmpeg = sources
            .iter()
            .any(|source| matches!(source, NativeResolvedSource::LocalVideo { .. }));
        let adapter = requires_ffmpeg
            .then(|| {
                Adapter::new(FfmpegConfig {
                    allowed_root: Some(store.assets_root()),
                    ..FfmpegConfig::default()
                })
            })
            .transpose()?;
        if let Some(adapter) = &adapter {
            let capabilities = adapter.capabilities();
            if !matches!(capabilities.ffmpeg, ToolAvailability::Available { .. })
                || !matches!(capabilities.ffprobe, ToolAvailability::Available { .. })
            {
                return Err(AppFailure(
                    "native video playback requires available ffmpeg and ffprobe capabilities"
                        .into(),
                )
                .into());
            }
        }
        // Stinger clips (and hot Stinger mutations) keep their clip-local
        // sequential decode; every other media file plays through its
        // engine-driven transport as a live source.
        let resolved_sources = sources.clone();
        let (files, file_inputs) =
            resolve_native_file_sources(adapter.as_ref(), stored, &mut sources)?;

        let master = NativeMasterRuntime::preflight_project_local_blocking(
            adapter.as_ref(),
            &sources,
            &project_plan,
            &stored.project().settings().audio,
            stored.project().settings().frame_rate,
            native_clock_domain(),
            stored.position().frames_rendered,
            NativeAudioLimits::default(),
        )?;
        let runtime = Arc::new(match context {
            Some(context) => NativeMediaRuntime::from_context_blocking(context)?,
            None => NativeMediaRuntime::new_blocking([platform_native_backend()])?,
        });
        let mut master = master;
        for input in file_inputs {
            master.attach_pushed_audio(input)?;
        }
        let network = start_native_network_streams(stored);
        for input in network.inputs() {
            master.attach_pushed_audio(input)?;
        }
        let system_audio = start_native_system_audio(stored);
        for input in system_audio.inputs() {
            master.attach_pushed_audio(input)?;
        }
        let captures = start_native_captures(stored)?;
        for input in captures
            .iter()
            .flat_map(native_captures::NativeCaptures::inputs)
        {
            master.attach_pushed_audio(input)?;
        }
        let (playback, stingers) = preflight_native_video(
            &runtime,
            adapter.as_ref(),
            sources,
            &resolved_sources,
            stored,
        )?;
        // Stream runtimes compile after video/audio preflight so a destination
        // problem is reported against an otherwise healthy show. Starting one
        // is deferred to readiness; failures there latch per target.
        let mut output_set = OutputSet::new();
        let (streams, stream_groups, local_output_dirs) =
            NativeProgramStream::compile(&runtime, stored, &mut output_set)?;
        #[cfg(target_os = "macos")]
        resolution.cameras.mark_preflight_frames_ingested();
        let pacer = FramePacer::restore(
            stored.project().settings().frame_rate,
            0,
            FrameNumber::new(stored.position().frames_rendered),
        )?;
        let pacer_start_offset = host_deadline_offset(&pacer)?;
        let origin = Instant::now();
        let telemetry = NativeRuntimeTelemetry::new(origin, runtime.context().adapter_info());
        Ok(Self {
            pacer,
            pacer_start_offset,
            origin,
            latest_output: None,
            latest_project_outputs: Vec::new(),
            master,
            project_plan,
            grade_cache: grading::GradeCache::default(),
            prepared_grades: None,
            playback,
            stingers,
            projected_frame: None,
            runtime,
            resolved_sources: Arc::new(resolved_sources),
            assets_root: store.assets_root().clone(),
            pending_stinger_mutation: None,
            stinger_retirements: NativeStingerRetirements::start()?,
            recorders: [ProgramRecording::Off, ProgramRecording::Off],
            record_program: None,
            record_program_2: None,
            record_outputs: [None, None],
            record_wav: false,
            record_segment_seconds: [None; RECORDER_SLOTS],
            record_bitrates: [None, None],
            record_audio_bitrates: [None, None],
            segment_names: recording_session::SegmentNames::default(),
            disk_alert: disk::DiskSpaceAlert::new(),
            disk_free_bytes: None,
            disk_sampled_at: None,
            local_output_dirs,
            local_output_status: [None; fm_switcher::MAX_STREAM_COUNT],
            hls_routes: None,
            local_output_sampled_at: None,
            recording_finalizers: Vec::new(),
            streams,
            stream_groups,
            output_set,
            pending_stream_starts: Vec::new(),
            streams_finalized: false,
            telemetry,
            telemetry_emitted: false,
            audio_meter_sequence: 0,
            pending_audio_meters: None,
            monitor_output: {
                let monitor = stored.project().audio_monitor_output();
                freemixd::monitor_output::MonitorOutputSlot::new(
                    "ffmpeg".into(),
                    monitor.device.clone(),
                    monitor.gain.get(),
                    monitor.muted,
                )
            },
            stream_status_sequence: 0,
            pending_stream_status: None,
            monitor: NativeMonitorTap::new(stored.project().settings()),
            titles,
            files,
            network,
            system_audio,
            captures,
            catalogue: native_catalogue::catalogue_keys(stored.project()),
            stills: Vec::new(),
            picture_size: {
                let dimensions = stored.project().settings().video.dimensions;
                (dimensions.width(), dimensions.height())
            },
            #[cfg(target_os = "macos")]
            cameras: resolution.cameras,
            #[cfg(target_os = "macos")]
            camera_telemetry_frozen: false,
            #[cfg(feature = "display-surface")]
            displays: Vec::new(),
            #[cfg(feature = "display-surface")]
            windowed: None,
            display_inventory: None,
            display_sync: display_outputs::DisplayOutputSync::default(),
            display_pacing: display_outputs::PresentationPacing::default(),
            latest_preview: None,
            preview_failed: false,
        })
    }

    #[cfg(feature = "display-surface")]
    fn start_with_program(
        store: &ProjectStore,
        stored: &StoredProject,
        context: NativeContext,
        outputs: Vec<program_surface::DisplayPresentationSetup>,
        windowed: program_surface::WindowedDisplays,
        camera_helper: Option<&Path>,
    ) -> AppResult<Self> {
        let mut native =
            Self::start_with_optional_context(store, stored, Some(context), camera_helper)?;
        native.displays = outputs
            .into_iter()
            .map(program_surface::ProgramPresentation::new)
            .collect();
        native.windowed = Some(windowed);
        Ok(native)
    }

    /// Presents the project's display outputs to the diagnostic display
    /// inventory read from `inventory` instead of real windows.
    #[allow(clippy::unnecessary_wraps)] // matches the non-native stub
    fn attach_display_inventory(&mut self, inventory: PathBuf) -> AppResult<()> {
        self.display_inventory = Some(display_outputs::InventoryDisplayHost::new(
            display_outputs::InventoryFile::new(inventory),
        ));
        Ok(())
    }

    /// What the presenter last observed, or `None` without a presenter.
    fn display_report(&self) -> Option<display_outputs::DisplayReport> {
        #[cfg(feature = "display-surface")]
        if let Some(windowed) = &self.windowed {
            return Some(windowed.report());
        }
        self.display_inventory
            .as_ref()
            .map(|host| host.report().clone())
    }

    /// Brings the presenter to the project's display-output slots and tells
    /// the control service what it shows (OR-011). Without a presenter every
    /// slot keeps waiting with `no-presenter`.
    fn sync_display_outputs(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<()> {
        let desired = control.snapshot().snapshot.display_outputs.clone();
        for (slot, config) in self.display_sync.changes(&desired) {
            #[cfg(feature = "display-surface")]
            if let Some(windowed) = &self.windowed {
                if let Some(display) = self.displays.get_mut(slot.index()) {
                    display.set_config(config.as_ref());
                }
                windowed.configure(slot, config.clone());
            }
            if let Some(host) = &mut self.display_inventory {
                host.configure(slot, config);
            }
        }
        let Some(report) = self.display_report() else {
            return Ok(());
        };
        for (slot, state) in self.display_sync.wire_states(&report) {
            control
                .set_display_output_status(server, slot, state)
                .map_err(|error| AppFailure(format!("display output {slot} status: {error}")))?;
        }
        if let Some(inventory) = self.display_sync.inventory(&report) {
            control
                .set_display_inventory(server, inventory)
                .map_err(|error| AppFailure(format!("display inventory: {error}")))?;
        }
        Ok(())
    }

    /// Reports each network stream input's receive status as it changes
    /// (IN-015): a state change at once, other changes at most once a
    /// second.
    fn report_network_streams(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<()> {
        for (input, status) in self.network.statuses(Instant::now()) {
            control
                .set_network_stream_status(server, input, status)
                .map_err(|error| AppFailure(format!("network input {input} status: {error}")))?;
        }
        // System audio inputs (AU-011): each input's capture status.
        for (input, status) in self.system_audio.statuses(Instant::now()) {
            control
                .set_system_audio_status(server, input, &status)
                .map_err(|error| {
                    AppFailure(format!("system audio input {input} status: {error}"))
                })?;
        }
        // Capture inputs (IN-001): each input's acquisition status, and the
        // device inventory whenever it changes.
        if let Some(captures) = &mut self.captures {
            for (input, status) in captures.statuses(Instant::now()) {
                control
                    .set_capture_status(server, input, &status)
                    .map_err(|error| {
                        AppFailure(format!("capture input {input} status: {error}"))
                    })?;
            }
            if let Some(inventory) = captures.inventory() {
                control
                    .set_capture_inventory(server, &inventory)
                    .map_err(|error| AppFailure(format!("capture inventory: {error}")))?;
            }
        }
        Ok(())
    }

    /// Whether an active display output shows Preview, which then needs
    /// its own composite; a slot waiting for its display (or a presenter)
    /// shows nothing.
    fn display_shows_preview(&self) -> bool {
        #[cfg(feature = "display-surface")]
        if let Some(windowed) = &self.windowed
            && self.displays.iter().enumerate().any(|(index, display)| {
                display.source() == Some(DisplayOutputSource::Preview)
                    && DisplayOutputSlot::ALL
                        .get(index)
                        .is_some_and(|slot| windowed.active(*slot))
            })
        {
            return true;
        }
        self.display_inventory
            .as_ref()
            .is_some_and(|host| host.shows(DisplayOutputSource::Preview))
    }

    /// Composites Preview for display outputs: `frame` with the realized
    /// Preview input cut in, without transitions, overlays, titles, or
    /// Fade to Black, through the same compositor path Program uses. It runs
    /// after Program reached its recorders, streams, and monitor.
    fn render_display_preview(&mut self, frame: Option<FrameResult>, preview: InputId) {
        let Some(frame) = frame else {
            self.latest_preview = None;
            return;
        };
        let frame = preview_frame(frame, preview);
        match self
            .runtime
            .render_project_frame_result_with_stingers_blocking(
                self.playback.registry(),
                self.stingers.registry(),
                &self.project_plan,
                &frame,
            ) {
            Ok(texture) => {
                self.latest_preview = Some(texture);
                self.preview_failed = false;
            }
            Err(error) => {
                if !self.preview_failed {
                    eprintln!(
                        "FREEMIXD_DISPLAY_PREVIEW\tv=1\terror={}",
                        error.to_string().replace(['\t', '\n', '\r'], "_")
                    );
                }
                self.preview_failed = true;
                self.latest_preview = None;
            }
        }
    }

    /// Presents every display output's own source after the frame.
    #[cfg_attr(
        not(feature = "display-surface"),
        allow(clippy::unnecessary_wraps) // windowed presentation can fail
    )]
    fn present_display_outputs(&mut self) -> AppResult<()> {
        #[cfg(feature = "display-surface")]
        for display in &mut self.displays {
            // An empty slot's window stays hidden; its surface is still
            // serviced every pass by `tick_if_due_collect`.
            let Some(source) = display.source() else {
                continue;
            };
            let Some(texture) = resolve_display_source_texture(
                source,
                &self.latest_project_outputs,
                self.latest_output.as_ref(),
                self.latest_preview.as_ref(),
            ) else {
                continue;
            };
            display
                .present_latest(self.runtime.context(), texture)
                .map_err(|error| -> Box<dyn Error> { Box::new(AppFailure(error)) })?;
        }
        if let Some(host) = &mut self.display_inventory {
            host.poll(&self.runtime, Instant::now());
            let project_outputs = &self.latest_project_outputs;
            let unbound = self.latest_output.as_ref();
            let preview = self.latest_preview.as_ref();
            host.present(&self.runtime, |source| {
                resolve_display_source_texture(source, project_outputs, unbound, preview)
            });
        }
        Ok(())
    }

    /// Frames presented by every display output, failed display GPU
    /// operations, and realized frames the display outputs dropped because
    /// the frame loop was behind, or `None` without display outputs.
    fn display_output_totals(&self) -> Option<(u64, u64, u64)> {
        let dropped = self.display_pacing.dropped();
        #[cfg(feature = "display-surface")]
        if !self.displays.is_empty() {
            return Some((
                self.displays
                    .iter()
                    .map(|display| display.telemetry().frames_presented)
                    .fold(0, u64::saturating_add),
                0,
                dropped,
            ));
        }
        self.display_inventory.as_ref().map(|host| {
            let (presented, failures) = host.totals();
            (presented, failures, dropped)
        })
    }

    /// Whether any display output is active and so presents frames.
    fn display_presenting(&self) -> bool {
        #[cfg(feature = "display-surface")]
        if let Some(windowed) = &self.windowed
            && self.displays.iter().enumerate().any(|(index, display)| {
                display.source().is_some()
                    && DisplayOutputSlot::ALL
                        .get(index)
                        .is_some_and(|slot| windowed.active(*slot))
            })
        {
            return true;
        }
        self.display_inventory
            .as_ref()
            .is_some_and(display_outputs::InventoryDisplayHost::presenting)
    }

    /// Opens one slot's next segment for a configured output.
    ///
    /// The slot index is zero-based (`--record-program` is 0 and
    /// `--record-program-2` is 1); the recorder itself reports the matching
    /// 1-based slot number.
    fn start_recorder(
        &mut self,
        slot: usize,
        stored: &StoredProject,
        path: &Path,
    ) -> AppResult<()> {
        let names = self.segment_names.clone();
        let segment = next_record_segment_path(path, |candidate| names.taken(candidate))?;
        self.recorders[slot] = ProgramRecording::Running(Box::new(NativeProgramRecorder::start(
            &self.runtime,
            stored,
            &segment,
            &names,
            self.record_wav,
            self.record_bitrates[slot],
            self.record_audio_bitrates[slot],
            slot + 1,
        )?));
        Ok(())
    }

    /// Records the optional `--record-fallback-dir`: every slot's sessions
    /// fail over to it, and its files count when deriving segment names.
    fn configure_record_fallback(&mut self, fallback: Option<PathBuf>) {
        self.segment_names = recording_session::SegmentNames::new(fallback);
    }

    /// Repairs every recording session of the configured slots that a crash
    /// left open, reporting each one on stderr as `FREEMIXD_RECORDING_RECOVERY`.
    ///
    /// Runs once at startup, after the recording configuration is known and
    /// before any recorder starts, so a recovered session's name is never
    /// reused and its torn segment is repaired before anything records next to
    /// it. Recovery never refuses startup: a session that cannot be recovered
    /// is reported and keeps its name.
    fn recover_recording_sessions(&self) {
        let outputs = [
            self.record_program.as_deref(),
            self.record_program_2.as_deref(),
        ];
        let scopes = outputs
            .into_iter()
            .enumerate()
            .filter_map(|(slot, output)| {
                output.map(|output| recording_session::RecoveryScope {
                    slot: slot + 1,
                    output,
                })
            })
            .collect::<Vec<_>>();
        for record in recording_session::recover_interrupted(&scopes, self.segment_names.fallback())
        {
            eprintln!("{record}");
        }
    }

    /// Records the configured output paths and optional project output
    /// selections so runtime `RecordStart` commands can derive later segment
    /// names from them even when startup itself does not open a segment.
    ///
    /// Slot 0 is `--record-program` (`--record-output`) and slot 1 is
    /// `--record-program-2` (`--record-output-2`); the shared `record_wav`
    /// flag applies to every configured slot, while each segment duration and
    /// bitrate pair applies only to its own slot. The whole-run telemetry
    /// reports recording as configured whenever at least one slot has a
    /// destination.
    #[allow(clippy::too_many_arguments)]
    fn configure_recordings(
        &mut self,
        primary: Option<PathBuf>,
        primary_output: Option<fm_types::OutputId>,
        secondary: Option<PathBuf>,
        secondary_output: Option<fm_types::OutputId>,
        wav: bool,
        segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
        primary_bitrate: Option<u32>,
        primary_audio_bitrate: Option<u32>,
        secondary_bitrate: Option<u32>,
        secondary_audio_bitrate: Option<u32>,
    ) {
        self.record_program = primary;
        self.record_program_2 = secondary;
        self.record_outputs = [primary_output, secondary_output];
        self.record_wav = wav;
        self.record_segment_seconds = segment_seconds;
        self.record_bitrates = [primary_bitrate, secondary_bitrate];
        self.record_audio_bitrates = [primary_audio_bitrate, secondary_audio_bitrate];
        self.telemetry.recorder_configured = self.recording_configured();
    }

    /// Whether program recording support is CONFIGURED for any slot.
    ///
    /// Deliberately independent of segment activity: the immutable handshake
    /// digest advertises configured startup support, while `FREEMIXD_RECORDER`
    /// reports per-segment runtime health.
    fn recording_configured(&self) -> bool {
        self.record_program.is_some() || self.record_program_2.is_some()
    }

    /// Starts and primes every configured recorder slot whose restored
    /// desired flag is true for daemon startup.
    ///
    /// Startup recording is fail-closed exactly like the single-slot daemon:
    /// the first slot that cannot start or reach mux readiness fails startup,
    /// and the caller finalizes whatever already started through the regular
    /// shutdown path. A configured slot whose restored flag is false stays
    /// closed even when the other slot is recording. `Ok(false)` means a
    /// requested shutdown interrupted a barrier.
    /// `before_frame` runs before every frame the startup barrier lets the
    /// pacer realize, so the playlist clock keeps cutting on schedule while a
    /// recorder starts.
    fn start_and_prime_configured_recorders(
        &mut self,
        stored: &StoredProject,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        shutdown: Option<&ProcessShutdown>,
        before_frame: &mut BeforeNativeFrame<'_>,
    ) -> AppResult<bool> {
        let paths = [self.record_program.clone(), self.record_program_2.clone()];
        let configured = paths.each_ref().map(Option::is_some);
        let selected =
            startup_recording_slots(configured, stored.project().recording_desired_slots());
        for (slot, path) in paths.into_iter().enumerate() {
            let Some(path) = path else {
                continue;
            };
            if !selected[slot] {
                continue;
            }
            self.start_recorder(slot, stored, &path)?;
        }
        for slot in 0..RECORDER_SLOTS {
            if self.recorders[slot].is_running()
                && !self.prime_recorder(slot, control, server, shutdown, before_frame)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Preflights every authored local HLS output directory, refusing startup
    /// with a sanitized failure when one has too little free space.
    ///
    /// A local output is a configured show destination, so it is held to the
    /// same floor as the configured recording output. The directory is
    /// measured through its nearest existing ancestor because the HLS sink
    /// creates the leaf later. The failure names the authored stream target id
    /// and carries free-space counts or an I/O kind only; it never renders the
    /// directory.
    fn preflight_local_outputs(&self) -> AppResult<()> {
        for stream in &self.streams {
            let config = self
                .output_set
                .config(stream.destination_id)
                .expect("every compiled stream owns one output-set slot");
            if config.protocol() != OutputProtocol::Hls {
                continue;
            }
            disk::preflight_local_output(Path::new(config.endpoint().path())).map_err(|error| {
                match error {
                    disk::DiskPreflightError::BelowMinimum { available } => AppFailure(format!(
                        "stream target {} local output has insufficient free space: requires at \
                         least {} free bytes, found {available}",
                        stream.target,
                        disk::RECORDING_MINIMUM_FREE_BYTES
                    )),
                    disk::DiskPreflightError::Unavailable(kind) => AppFailure(format!(
                        "stream target {} local output free space is unavailable: {kind}",
                        stream.target
                    )),
                }
            })?;
        }
        Ok(())
    }

    /// Probes every configured network stream endpoint exactly once, in
    /// authored order with each target's primary before its backup, and
    /// renders one sanitized record per probed endpoint.
    ///
    /// The probe is bounded and purely informational: it reads the compiled
    /// destination configuration, never mutates output-set, recorder, or
    /// engine state, and cannot fail startup. A destination that cannot be
    /// reached right now is reported and left alone — an ingest may
    /// legitimately come up later, and the shared output set owns
    /// reconnection. Local HLS destinations have no peer and contribute
    /// nothing. The record set is bounded by twice the five-target cap.
    fn network_preflight_records(&self) -> Vec<String> {
        let mut pending = Vec::with_capacity(self.streams.len().saturating_mul(2));
        for stream in &self.streams {
            let Some(config) = self.output_set.config(stream.destination_id) else {
                continue;
            };
            let target = stream.target.get().get();
            for endpoint in [preflight::PRIMARY_ENDPOINT, preflight::BACKUP_ENDPOINT] {
                if let Some(probe) = pending_network_probe(config, target, endpoint) {
                    pending.push(probe);
                }
            }
        }
        preflight::probe_targets(pending)
            .iter()
            .map(preflight::probe_record)
            .collect()
    }

    /// Samples the free space shared by the configured outputs at a bounded
    /// cadence and reports alert edges.
    ///
    /// The render loop calls this every frame; the gate is two optional field
    /// checks plus one `Instant` comparison, and a sample reaches the
    /// filesystem at most once per [`DISK_SAMPLE_INTERVAL`]. Every configured
    /// recording output contributes its parent directory, and every authored
    /// local HLS output contributes the nearest existing ancestor of its
    /// directory — the sink creates the leaf later — with the minimum across
    /// the queries that succeed driving the alert. Query failures stay silent:
    /// a failed query is skipped, and when no query succeeds the last sampled
    /// value and alert state stay in place. The first call while a recording
    /// or a local output is configured evaluates immediately, so a low value
    /// present at startup fires at the first frame boundary.
    fn sample_disk_space(&mut self, now: Instant) {
        let recording_parents = [
            self.record_program.as_deref(),
            self.record_program_2.as_deref(),
        ];
        if recording_parents.iter().all(Option::is_none) && self.local_output_dirs.is_empty() {
            return;
        }
        if self
            .disk_sampled_at
            .is_some_and(|last| now.saturating_duration_since(last) < DISK_SAMPLE_INTERVAL)
        {
            return;
        }
        self.disk_sampled_at = Some(now);
        let recording_parents = recording_parents
            .into_iter()
            .flatten()
            .map(recording_parent);
        let local_ancestors = self
            .local_output_dirs
            .iter()
            .filter_map(|directory| disk::existing_ancestor(directory));
        let mut minimum: Option<u64> = None;
        for path in recording_parents.chain(local_ancestors) {
            if let Ok(available) = disk::available_bytes(path) {
                minimum = Some(minimum.map_or(available, |current| current.min(available)));
            }
        }
        let Some(available) = minimum else {
            return;
        };
        self.disk_free_bytes = Some(available);
        match self.disk_alert.observe(available) {
            fm_observability::AlertTransition::Fired => {
                eprintln!("FREEMIXD_DISK\tv=1\tstate=low\tfree_bytes={available}");
            }
            fm_observability::AlertTransition::Cleared => {
                eprintln!("FREEMIXD_DISK\tv=1\tstate=normal\tfree_bytes={available}");
            }
            fm_observability::AlertTransition::None => {}
        }
    }

    /// Samples every local HLS target's published playlist health at the same
    /// bounded cadence as [`Self::sample_disk_space`].
    ///
    /// The render loop calls this every frame; the gate is one empty check plus
    /// one `Instant` comparison, and a playlist reaches the filesystem at most
    /// once per [`DISK_SAMPLE_INTERVAL`]. Each local `hls://` destination is
    /// read through [`local_hls_status`] into its destination slot; a target
    /// whose playlist cannot be read now stores `None`, so its next wire sample
    /// simply carries no local field. Network targets are skipped and keep
    /// `None` entries.
    fn sample_local_output_status(&mut self, now: Instant) {
        if self.local_output_dirs.is_empty() {
            return;
        }
        if self
            .local_output_sampled_at
            .is_some_and(|last| now.saturating_duration_since(last) < DISK_SAMPLE_INTERVAL)
        {
            return;
        }
        self.local_output_sampled_at = Some(now);
        for stream in &self.streams {
            let Some(config) = self.output_set.config(stream.destination_id) else {
                continue;
            };
            if config.protocol() != OutputProtocol::Hls {
                continue;
            }
            let slot = usize::from(stream.destination_id.get()) - 1;
            if let Some(entry) = self.local_output_status.get_mut(slot) {
                *entry = local_hls_status(Path::new(config.endpoint().path()));
            }
        }
    }

    /// Waits for one recorder slot's first muxed pair within that slot's own
    /// startup timeout.
    ///
    /// Returns `Ok(true)` when the slot is ready or was never started,
    /// `Ok(false)` when a requested shutdown interrupted the barrier, and an
    /// error when the slot failed or exhausted its deadline. Every failure
    /// latches only the addressed slot. Only configured startup recording,
    /// which is fail-closed and completes before readiness, blocks on this;
    /// a runtime `RecordStart` is primed by [`Self::advance_recordings`]
    /// without stalling the daemon loop.
    fn prime_recorder(
        &mut self,
        slot: usize,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        shutdown: Option<&ProcessShutdown>,
        before_frame: &mut BeforeNativeFrame<'_>,
    ) -> AppResult<bool> {
        let ProgramRecording::Running(recorder) = &self.recorders[slot] else {
            return Ok(true);
        };
        let startup_timeout = recorder.startup_pair_timeout;
        let deadline = Instant::now()
            .checked_add(startup_timeout)
            .ok_or_else(|| AppFailure("Program recorder startup deadline overflow".into()))?;
        loop {
            if requested_daemon_shutdown(Some(&*self), shutdown).is_some() {
                return Ok(false);
            }
            let decision = match &self.recorders[slot] {
                ProgramRecording::Running(recorder) => recorder.startup_decision(),
                ProgramRecording::Off => unreachable!("recorder presence was checked"),
            }?;
            match decision {
                StartupPairDecision::Ready => return Ok(true),
                StartupPairDecision::Failed => unreachable!("failure is returned as an error"),
                StartupPairDecision::Pending => {}
            }
            if Instant::now() >= deadline {
                return Err(match &mut self.recorders[slot] {
                    ProgramRecording::Running(recorder) => recorder.fail_startup_timeout(),
                    ProgramRecording::Off => unreachable!("recorder presence was checked"),
                });
            }
            before_frame(self, control)?;
            self.tick_if_due(control, server)?;
            thread::sleep(NATIVE_IO_POLL_INTERVAL);
        }
    }

    /// Starts sinks for every target whose authored intent is to be running
    /// once the daemon reconciles desired state.
    ///
    /// Called at readiness, mirroring the recorder's startup priming but
    /// deliberately without a barrier: a destination that refuses to open must
    /// degrade its own target, not delay or abort daemon startup.
    fn prime_startup_streams(&mut self, stored: &StoredProject) {
        for target in stored.project().stream_targets() {
            if !target.running() || target.startup() != StartupPolicy::ReconcileDesiredState {
                continue;
            }
            let target_id = SwitcherStreamTargetId::from_non_zero(target.id().get());
            if let Some(stream) = self
                .streams
                .iter_mut()
                .find(|stream| stream.target == target_id)
            {
                stream.start(&mut self.output_set);
            }
        }
    }

    /// Schedules live sink changes for an accepted StreamStart/StreamStop.
    ///
    /// Starts wait for the next frame boundary; stops take effect off the
    /// render path immediately: the shared output set drops queued pairs and
    /// disconnects the transport under its bounded deadlines.
    ///
    /// The group start and stop (OR-006) do exactly that for every
    /// destination and nothing else: recorders are separate workers no stream
    /// command reaches. A retry stops the destination now, clearing a latched
    /// failure, and starts it again at the next frame boundary with a fresh
    /// reconnect budget.
    fn observe_stream_command(&mut self, command: &CommandMessage, result: &CommandResult) {
        if !matches!(result, CommandResult::Accepted { .. }) {
            return;
        }
        let target_id =
            |target: WireStreamTargetId| SwitcherStreamTargetId::from_non_zero(target.get());
        match command.payload {
            CommandPayload::StreamStart { target } => self.request_stream_start(target_id(target)),
            CommandPayload::StreamStop { target } => self.request_stream_stop(target_id(target)),
            CommandPayload::StreamRetry { target } => {
                let target = target_id(target);
                if let Some(stream) = self
                    .streams
                    .iter_mut()
                    .find(|stream| stream.target == target)
                {
                    stream.retry(&mut self.output_set);
                }
                self.request_stream_start(target);
            }
            CommandPayload::StreamStartAll => {
                let targets: Vec<_> = self.streams.iter().map(|stream| stream.target).collect();
                for target in targets {
                    self.request_stream_start(target);
                }
            }
            CommandPayload::StreamStopAll => {
                let targets: Vec<_> = self.streams.iter().map(|stream| stream.target).collect();
                for target in targets {
                    self.request_stream_stop(target);
                }
            }
            _ => {}
        }
    }

    /// Queues one destination's start for the next frame boundary.
    fn request_stream_start(&mut self, target: SwitcherStreamTargetId) {
        if !self.pending_stream_starts.contains(&target) {
            self.pending_stream_starts.push(target);
        }
    }

    /// Stops one destination off the render path now.
    fn request_stream_stop(&mut self, target: SwitcherStreamTargetId) {
        self.pending_stream_starts
            .retain(|pending| *pending != target);
        if let Some(stream) = self
            .streams
            .iter_mut()
            .find(|stream| stream.target == target)
        {
            stream.stop_requested(&mut self.output_set);
        }
    }

    /// Follows the stored project's stream destinations after a committed
    /// command (OR-006): a removed or edited destination's runtime retires —
    /// the engine refuses both while it is desired running, so its transport
    /// is already stopping — and every destination without a runtime gets
    /// one in a free output-set slot, joining the rendition group of its
    /// encoder identity or a new one. Running destinations, their groups, and
    /// recorders are untouched.
    ///
    /// An authored destination the codec parser refuses is unreachable (the
    /// model validates the same rules) and is reported and left unrealized
    /// rather than stopping the show.
    fn sync_stream_targets(
        &mut self,
        project: &fm_model::Project,
        frames_rendered: u64,
    ) -> AppResult<()> {
        let targets = project.stream_targets();
        let mut index = 0;
        while index < self.streams.len() {
            let current = &self.streams[index];
            if targets
                .iter()
                .any(|target| target.clone().set_running(false) == current.authored)
            {
                index += 1;
                continue;
            }
            let mut retired = self.streams.remove(index);
            self.pending_stream_starts
                .retain(|pending| *pending != retired.target);
            retired.stop_live(&mut self.output_set);
            if retired.started {
                retired.emit_final_record(&self.output_set);
            }
            let _ = self.output_set.remove_destination(retired.destination_id);
            self.local_output_status[usize::from(retired.destination_id.get()) - 1] = None;
            for group in &mut self.stream_groups {
                group
                    .core
                    .members
                    .retain(|member| *member != retired.destination_id);
            }
            // Dropping the transport hands any outstanding stop to its worker
            // thread, which reaps the child off the daemon loop.
        }
        for target in targets {
            let switcher_id = SwitcherStreamTargetId::from_non_zero(target.id().get());
            if self
                .streams
                .iter()
                .any(|stream| stream.target == switcher_id)
            {
                continue;
            }
            let (protocol, endpoint, stream_key, backup_endpoint) =
                match split_target_endpoints(target) {
                    Ok(split) => split,
                    Err(error) => {
                        eprintln!(
                            "FREEMIXD_STREAM_FAILURE\tv=1\ttarget={switcher_id}\tfailure={}",
                            diagnostic_field(&format!("configure:{error}"))
                        );
                        continue;
                    }
                };
            let Some(destination_id) = (1..=fm_io_network::MAX_DESTINATIONS)
                .filter_map(|slot| DestinationId::new(u8::try_from(slot).ok()?).ok())
                .find(|slot| !self.output_set.contains(*slot))
            else {
                // The engine caps destinations at the output set's size.
                continue;
            };
            let format = stream_record_format(project.settings(), frames_rendered)?;
            let key = (target.output(), protocol, target.video_bitrate_kbps());
            let group_index = self.stream_group_for(key, &format, frames_rendered)?;
            let stream = NativeProgramStream::realize(
                SplitStreamTarget {
                    target,
                    protocol,
                    endpoint,
                    stream_key,
                    backup_endpoint,
                    group_index,
                },
                destination_id,
                &mut self.stream_groups[group_index],
                &format,
                frames_rendered,
                &mut self.output_set,
            );
            self.streams.push(stream);
        }
        self.local_output_dirs = self.local_hls_directories();
        self.publish_hls_routes();
        Ok(())
    }

    /// Republishes the `--hls-listen` route table from the realized
    /// destinations, if the listener runs. The engine's unique target ids
    /// make a duplicate unreachable; one is reported and keeps the previous
    /// table rather than serving the wrong directory.
    fn publish_hls_routes(&self) {
        let Some(routes) = &self.hls_routes else {
            return;
        };
        match hls_roots(Some(self)) {
            Ok(roots) => routes.replace(roots),
            Err(error) => eprintln!(
                "FREEMIXD_STREAM_FAILURE\tv=1\ttarget=hls\tfailure={}",
                diagnostic_field(&format!("hls_routes:{error}"))
            ),
        }
    }

    /// The group a new destination with encoder identity `key` joins: the
    /// group already carrying that identity, else an emptied group rebuilt
    /// for it in place, else a new group. Rendition ids stay each group's
    /// one-based position, so running members never change rendition.
    fn stream_group_for(
        &mut self,
        key: StreamGroupKey,
        format: &RecordFormat,
        frames_rendered: u64,
    ) -> AppResult<usize> {
        if let Some(index) = self.stream_groups.iter().position(|group| group.key == key) {
            return Ok(index);
        }
        let (width, height) = self.picture_size;
        let size = (
            NonZeroU32::new(width).expect("project width is nonzero"),
            NonZeroU32::new(height).expect("project height is nonzero"),
        );
        if let Some(index) = self
            .stream_groups
            .iter()
            .position(|group| group.core.members.is_empty())
        {
            self.stream_groups[index] = new_stream_group(
                &self.runtime,
                format,
                size,
                key,
                (index, index + 1),
                frames_rendered,
            )?;
            return Ok(index);
        }
        let index = self.stream_groups.len();
        self.stream_groups.push(new_stream_group(
            &self.runtime,
            format,
            size,
            key,
            (index, index + 1),
            frames_rendered,
        )?);
        Ok(index)
    }

    /// The absolute directory of every realized local HLS destination.
    fn local_hls_directories(&self) -> Vec<PathBuf> {
        self.streams
            .iter()
            .filter_map(|stream| self.output_set.config(stream.destination_id))
            .filter(|config| config.protocol() == OutputProtocol::Hls)
            .map(|config| PathBuf::from(config.endpoint().path()))
            .collect()
    }

    /// Consumes pending `StreamStart` requests at a frame boundary.
    fn start_pending_streams(&mut self) {
        for target in std::mem::take(&mut self.pending_stream_starts) {
            if let Some(stream) = self
                .streams
                .iter_mut()
                .find(|stream| stream.target == target)
            {
                stream.start(&mut self.output_set);
            }
        }
    }

    /// Stops every stream under a bounded deadline and emits exactly one
    /// sanitized final record per configured target. Collected failures are
    /// returned so shutdown can report them after the recorder result instead
    /// of abandoning the rest of teardown.
    fn finalize_streams(&mut self) -> Vec<AppResult<()>> {
        if self.streams_finalized {
            return Vec::new();
        }
        self.streams_finalized = true;
        let mut results = Vec::new();
        for index in 0..self.streams.len() {
            self.streams[index].stop_live(&mut self.output_set);
            if let Some(error) = self.streams[index].shutdown_error() {
                results.push(error);
            }
            self.streams[index].emit_final_record(&self.output_set);
        }
        results
    }

    fn tick_if_due(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<()> {
        let _ = self.tick_if_due_collect(control, server)?;
        Ok(())
    }

    fn take_audio_meters(&mut self) -> Option<AudioMetersMessage> {
        self.pending_audio_meters.take()
    }

    /// Consumes the latest-wins stream status record built at the last frame
    /// boundary, mirroring [`Self::take_audio_meters`].
    fn take_stream_status(&mut self) -> Option<StreamStatusMessage> {
        self.pending_stream_status.take()
    }

    /// Whether any eligible session currently wants monitor frames; without
    /// demand no readback runs.
    fn set_monitor_demand(&mut self, demand: bool) {
        self.monitor.demand = demand;
    }

    /// A snapshot of the newest CPU frame of every Program source, for an
    /// exact scene preview composed off the loop (shared frames, no copy,
    /// no GPU work).
    fn live_source_frames(&self) -> scene_preview::LiveSourceFrames {
        scene_preview::LiveSourceFrames::snapshot(&self.playback.latest_frames())
    }

    /// Consumes the latest encoded Program monitor frame group.
    fn take_monitor_frame(&mut self) -> Option<(MonitorTap, Vec<u8>)> {
        self.monitor
            .pending
            .take()
            .map(|bytes| (MonitorTap::Program, bytes))
    }

    /// Monotonic milliseconds since the native runtime's origin — the clock
    /// the shared output set schedules reconnects and backoff on.
    fn stream_clock_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Feeds the selected program texture and audio block to every running
    /// recorder slot.
    ///
    /// Each running slot resolves its own selected [`fm_types::OutputId`]
    /// independently; a slot whose output is missing latches with
    /// `"capture:missing_output"`. Every running slot that resolves a
    /// texture receives a clone of the block its output hears, mirroring
    /// how the stream fan-out clones one block per target. An idle or
    /// missing-output slot clones nothing, and each slot keeps its own
    /// readback owner. Takes the recorder
    /// pieces separately so the caller can hold immutable borrows of other
    /// daemon fields.
    fn capture_recorders(
        recorders: &mut [ProgramRecording; RECORDER_SLOTS],
        record_outputs: &[Option<fm_types::OutputId>; RECORDER_SLOTS],
        runtime: &NativeMediaRuntime,
        project_outputs: &[NativeOutputFrame],
        unbound_output: Option<&NativeTexture>,
        audio: &CaptureAudio<'_>,
    ) {
        let mut resolved: [Option<&NativeTexture>; RECORDER_SLOTS] = [None, None];
        for (slot_index, slot) in recorders.iter_mut().enumerate() {
            let ProgramRecording::Running(recorder) = slot else {
                continue;
            };
            if let Some(texture) = resolve_project_output_texture(
                record_outputs[slot_index],
                project_outputs,
                unbound_output,
            ) {
                resolved[slot_index] = Some(texture);
            } else {
                recorder.fail("capture:missing_output".to_owned(), true);
            }
        }
        // A slot recording an output Fade to Black leaves live records the
        // mix before the fade, one fading over its own duration its own mix,
        // and every other slot the faded mix (SW-004).
        for (slot_index, slot) in recorders.iter_mut().enumerate() {
            let ProgramRecording::Running(recorder) = slot else {
                continue;
            };
            let Some(program) = resolved[slot_index] else {
                continue;
            };
            let block = audio
                .for_output(record_outputs[slot_index], project_outputs)
                .clone();
            recorder.capture(runtime, program, block);
        }
    }

    /// Packs one pair per rendition group per frame and fans it out through
    /// the planner: destinations whose encoder-relevant settings match cost
    /// one readback and one packed pair in total, while every member route
    /// stays independently queued, counted, and flagged in the shared output
    /// set.
    ///
    /// Sequential by design: the recorder captures first, each group performs
    /// its one synchronous readback, and no path here blocks the render loop
    /// beyond the existing diagnostic readback contract. A shared-step failure
    /// latches every active member of that group; a refused route degrades
    /// only its own target's counters. Takes the stream pieces separately so
    /// the caller can hold immutable borrows of other daemon fields.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn capture_streams(
        streams: &mut [NativeProgramStream],
        stream_groups: &mut [StreamRenditionGroup],
        output_set: &mut OutputSet,
        runtime: &NativeMediaRuntime,
        project_outputs: &[NativeOutputFrame],
        unbound_output: Option<&NativeTexture>,
        capture_audio: &CaptureAudio<'_>,
    ) {
        let sequence = capture_audio.faded.timing().sequence();
        for group in stream_groups.iter_mut() {
            // A target streaming an output Fade to Black leaves live streams
            // the mix before the fade (SW-004).
            let audio = capture_audio.for_output(Some(group.core.output), project_outputs);
            let members = group.core.members.clone();
            // A route joins this frame only when it was asked to run, never
            // latched a failure, and was not deliberately stopped.
            let active: Vec<bool> = members
                .iter()
                .map(|destination| {
                    streams
                        .iter()
                        .find(|stream| stream.destination_id == *destination)
                        .is_some_and(|stream| route_active(stream, output_set))
                })
                .collect();
            if !active.iter().any(|&is_active| is_active) {
                continue;
            }
            let Some(program) = resolve_project_output_texture(
                Some(group.core.output),
                project_outputs,
                unbound_output,
            ) else {
                for (destination, &is_active) in members.iter().zip(&active) {
                    if is_active
                        && let Some(stream) = streams
                            .iter_mut()
                            .find(|stream| stream.destination_id == *destination)
                    {
                        stream.fail("capture:missing_output", output_set);
                    }
                }
                continue;
            };
            let rendition = group.core.rendition;
            let outcome = {
                let StreamRenditionGroup {
                    core,
                    readback,
                    format,
                    ..
                } = group;
                let StreamRenditionCore {
                    members,
                    variants,
                    next_sequence,
                    ..
                } = core;
                plan_and_pack_group_frame(
                    members,
                    variants,
                    next_sequence,
                    sequence,
                    &active,
                    || {
                        pack_group_pair(
                            runtime, program, readback, format, rendition, sequence, audio,
                        )
                    },
                )
            };
            match outcome {
                GroupFrameOutcome::Idle => {}
                GroupFrameOutcome::Failed(failures) => {
                    for (destination, failure) in failures {
                        if let Some(stream) = streams
                            .iter_mut()
                            .find(|stream| stream.destination_id == destination)
                        {
                            stream.fail(&failure, output_set);
                        }
                    }
                }
                GroupFrameOutcome::Packed { plan, packet } => {
                    let mut routes: Vec<(DestinationId, &mut StreamPairLedger, &mut bool)> =
                        streams
                            .iter_mut()
                            .filter(|stream| members.contains(&stream.destination_id))
                            .map(|stream| {
                                (
                                    stream.destination_id,
                                    &mut stream.ledger,
                                    &mut stream.enqueue_backpressure,
                                )
                            })
                            .collect();
                    match output_set.enqueue_rendition(&plan, &packet) {
                        Ok(outcomes) => admit_rendition_outcomes(&outcomes, sequence, &mut routes),
                        Err(error) => {
                            // Unreachable short of a bug: every planned
                            // destination exists in this set, so degrade the
                            // group's members instead of panicking on air.
                            let fanned: Vec<DestinationId> = routes
                                .iter()
                                .map(|(destination, _, _)| *destination)
                                .collect();
                            for destination in fanned {
                                if let Some(stream) = streams
                                    .iter_mut()
                                    .find(|stream| stream.destination_id == destination)
                                {
                                    stream.fail(&format!("fanout:{error}"), output_set);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Advances every target one step in the shared output set after each
    /// frame's enqueue loop, so connects, backoff, failover, and writes
    /// progress at the render cadence, then builds the latest-wins stream
    /// status record. An empty active set publishes no record at all.
    fn advance_stream_routes(&mut self, server: &ServerIdentity) -> AppResult<()> {
        let now_ms = self.stream_clock_ms();
        for stream in &mut self.streams {
            stream.poll(&mut self.output_set, now_ms);
        }
        let status_samples = stream_status_samples(native_stream_snapshots(
            &self.output_set,
            &mut self.streams,
            &self.local_output_status,
            now_ms,
        ));
        if let Some(status) =
            stream_status_message(server, self.stream_status_sequence, status_samples)?
        {
            self.stream_status_sequence = status.sequence;
            self.pending_stream_status = Some(status);
        }
        Ok(())
    }

    /// Schedules recording realization for an accepted RecordStart/RecordStop
    /// addressed to one recorder slot.
    ///
    /// A start begins opening the next segment of the addressed configured
    /// slot without blocking: the segment file and `FFmpeg` start on a worker,
    /// and [`Self::advance_recordings`] then awaits the same bounded mux
    /// readiness barrier used at startup while the daemon loop keeps
    /// realizing frames and serving every session. The session loop holds the
    /// command's acknowledgement until [`Self::recording_settling`] clears, so
    /// durability and the engine commit have already settled and a client is
    /// merely delayed, never acknowledged for unrealized state. The addressed
    /// slot is realized independently of the other: a failure latches only
    /// that slot's attempt unavailable and reports its own sanitized notice
    /// without aborting the show or the other slot.
    ///
    /// A stop retires only the addressed slot's open segment off the hot path:
    /// the slot drops to [`ProgramRecording::Off`] at once and finalization
    /// runs on a dedicated worker, exactly like stinger retirement, with
    /// shutdown joining within the recorder's own stop budget. The other
    /// slot's segment keeps running.
    fn observe_recording_command(
        &mut self,
        command: &CommandMessage,
        result: &CommandResult,
        stored: &StoredProject,
    ) {
        let Some((active, slot)) = recording_command_slot(&command.payload) else {
            return;
        };
        if !matches!(result, CommandResult::Accepted { .. }) {
            return;
        }
        self.reap_finished_recording_finalizers();
        if active {
            self.open_pending_recording(slot, stored);
        } else {
            self.retire_running_recording(slot, None);
        }
    }

    /// Whether `slot`'s segment is still opening or awaiting mux readiness;
    /// an accepted `RecordStart` for it is not acknowledged until this clears.
    fn recording_settling(&self, slot: usize) -> bool {
        self.recorders
            .get(slot)
            .is_some_and(ProgramRecording::is_settling)
    }

    /// Advances every opening or priming segment without blocking: adopts a
    /// recorder the open worker handed over, feeds it the backlog, and
    /// settles the mux-readiness barrier. Called on every daemon loop pass.
    ///
    /// A failed open releases the slot with its `start`/`rotate` notice; a
    /// failed or timed-out barrier cancels the segment and finalizes it off
    /// the loop, reporting `startup:` exactly as the blocking barrier did.
    fn advance_recordings(&mut self) {
        for slot in 0..RECORDER_SLOTS {
            let ProgramRecording::Running(recorder) = &mut self.recorders[slot] else {
                continue;
            };
            if let OpenPoll::Failed(failure) = recorder.poll_open() {
                self.recorders[slot] = ProgramRecording::Off;
                eprintln!("{}", recorder_failure_notice(slot + 1, &failure, false));
                continue;
            }
            recorder.pump_backlog();
            recorder.report_failover();
            let Some(deadline) = recorder.priming else {
                continue;
            };
            let failure = match recorder.startup_decision() {
                Ok(StartupPairDecision::Ready) => {
                    recorder.priming = None;
                    continue;
                }
                Ok(StartupPairDecision::Pending) if Instant::now() < deadline => continue,
                Ok(StartupPairDecision::Pending) => recorder.fail_startup_timeout(),
                Ok(StartupPairDecision::Failed) => {
                    unreachable!("failure is returned as an error")
                }
                Err(error) => error,
            };
            // The barrier failed; latch this slot's attempt unavailable. The
            // failure notice was already emitted by the capture policy or the
            // timeout helper; cancel and report the partial segment.
            recorder.priming = None;
            recorder.request_cancel();
            self.retire_running_recording(
                slot,
                Some(recorder_failure_notice(
                    slot + 1,
                    &format!("startup:{failure}"),
                    false,
                )),
            );
        }
    }

    /// Begins opening one slot's next segment after an accepted `RecordStart`.
    ///
    /// An unconfigured slot derives nothing and stays untouched. Every failure
    /// latches only THIS slot's attempt unavailable and reports a sanitized
    /// notice naming the slot; the show and the other slot keep running,
    /// exactly like stream sink failures.
    fn open_pending_recording(&mut self, slot: usize, stored: &StoredProject) {
        if self.recorders[slot].is_running() {
            return;
        }
        let names = self.segment_names.clone();
        let segment = match next_slot_segment_path(
            slot,
            self.record_program.as_deref(),
            self.record_program_2.as_deref(),
            |candidate| names.taken(candidate),
        ) {
            Ok(Some(segment)) => segment,
            Ok(None) => return,
            Err(error) => {
                eprintln!(
                    "{}",
                    recorder_failure_notice(slot + 1, &format!("start:{error}"), false)
                );
                return;
            }
        };
        let opened = next_record_segment_path(&segment, |candidate| names.taken(candidate))
            .and_then(|segment| {
                NativeProgramRecorder::open(
                    &self.runtime,
                    stored,
                    &segment,
                    &names,
                    self.record_wav,
                    self.record_bitrates[slot],
                    self.record_audio_bitrates[slot],
                    slot + 1,
                )
            });
        match opened {
            Ok(recorder) => self.recorders[slot] = ProgramRecording::Running(Box::new(recorder)),
            Err(error) => {
                self.recorders[slot] = ProgramRecording::Off;
                eprintln!(
                    "{}",
                    recorder_failure_notice(slot + 1, &format!("start:{error}"), false)
                );
            }
        }
    }

    /// Retires one slot's open segment off the render path after an accepted
    /// `RecordStop`, a failed readiness barrier, or a rotation. The worker
    /// drains the sink cleanly unless it was cancelled, so the finalized file
    /// keeps its full `FREEMIXD_RECORDER` report; `notice` follows that report.
    fn retire_running_recording(&mut self, slot: usize, notice: Option<String>) {
        let ProgramRecording::Running(recorder) =
            std::mem::replace(&mut self.recorders[slot], ProgramRecording::Off)
        else {
            return;
        };
        let mut recorder = recorder;
        // The worker waits for its segment so a failed spawn leaves the
        // recorder here for a synchronous bounded stop.
        let (handoff_sender, handoff_receiver) =
            mpsc::sync_channel::<Box<NativeProgramRecorder>>(1);
        let (done_sender, done) = mpsc::sync_channel(1);
        let worker_notice = notice.clone();
        let spawned = thread::Builder::new()
            .name(format!("freemix-native-recording-finalizer-{}", slot + 1))
            .spawn(move || {
                let Ok(mut recorder) = handoff_receiver.recv() else {
                    return;
                };
                let clean = recorder.stop_and_report().is_ok();
                if let Some(notice) = worker_notice {
                    eprintln!("{notice}");
                }
                let _ = done_sender.send(clean);
            });
        match spawned {
            Ok(worker) => {
                let _ = handoff_sender.send(recorder);
                self.recording_finalizers
                    .push(ProgramRecordingFinalizer { done, worker });
            }
            Err(error) => {
                // No worker available: fall back to a synchronous stop that
                // stays within the same stop/kill budget constants the
                // shutdown path uses.
                eprintln!(
                    "{}",
                    recorder_failure_notice(slot + 1, &format!("finalize:{error}"), false)
                );
                let _ = recorder.stop_and_report();
                if let Some(notice) = notice {
                    eprintln!("{notice}");
                }
            }
        }
    }

    /// Reaps detached finalizers that already reported, so repeated
    /// record-stop/start cycles cannot accumulate workers.
    fn reap_finished_recording_finalizers(&mut self) {
        self.recording_finalizers.retain(|finalizer| {
            !matches!(
                finalizer.done.try_recv(),
                Ok(_) | Err(mpsc::TryRecvError::Disconnected)
            )
        });
    }

    /// Rotates every running recorder slot that has reached its own
    /// [`Self::record_segment_seconds`] entry before capturing `sequence`.
    ///
    /// Completed segments are handed off the render path via
    /// [`Self::retire_running_recording`], finished finalizers are reaped to
    /// keep [`Self::recording_finalizers`] bounded, and the newly opened
    /// segment starts at `first_sequence = sequence` so the current frame is
    /// captured immediately with zero dropped frames between segments.
    fn rotate_due_recorders(&mut self, sequence: SequenceNumber) {
        for slot in 0..RECORDER_SLOTS {
            self.rotate_running_recording(slot, sequence);
        }
    }

    fn rotate_running_recording(&mut self, slot: usize, sequence: SequenceNumber) {
        let (next_format, still_priming) = match &self.recorders[slot] {
            ProgramRecording::Running(recorder)
                if recorder
                    .should_rotate(slot_segment_seconds(self.record_segment_seconds, slot)) =>
            {
                // A segment rotated before it confirmed mux readiness hands
                // the pending barrier to its successor, so the held start
                // acknowledgement still waits for realized output.
                let old = &recorder.format;
                (
                    RecordFormat::new(
                        old.dimensions().width(),
                        old.dimensions().height(),
                        old.frame_rate(),
                        old.sample_rate(),
                        old.channel_layout().clone(),
                        sequence,
                    ),
                    recorder.priming.is_some(),
                )
            }
            ProgramRecording::Running(_) | ProgramRecording::Off => return,
        };
        self.reap_finished_recording_finalizers();
        self.retire_running_recording(slot, None);
        let rotated = next_format.map_err(Into::into).and_then(|format| {
            let timecode = recording_start_timecode(sequence.get(), format.frame_rate());
            let names = &self.segment_names;
            let segment = next_slot_segment_path(
                slot,
                self.record_program.as_deref(),
                self.record_program_2.as_deref(),
                |candidate| names.taken(candidate),
            )?
            .ok_or_else(|| AppFailure("rotating recorder slot has no configured output".into()))?;
            // The next segment opens off the render path; the frame captured
            // right after this rotation waits in its backlog until FFmpeg is
            // connected, so no frame is dropped between segments.
            NativeProgramRecorder::open_with_format(
                &self.runtime,
                format,
                timecode,
                &segment,
                names,
                SegmentOpening {
                    wav: self.record_wav,
                    video_bitrate_kbps: self.record_bitrates[slot],
                    audio_bitrate_kbps: self.record_audio_bitrates[slot],
                    slot: slot + 1,
                    failure_label: "rotate",
                    prime_on_open: still_priming,
                },
            )
        });
        match rotated {
            Ok(recorder) => {
                self.recorders[slot] = ProgramRecording::Running(Box::new(recorder));
            }
            Err(error) => {
                self.recorders[slot] = ProgramRecording::Off;
                eprintln!(
                    "{}",
                    recorder_failure_notice(slot + 1, &format!("rotate:{error}"), false)
                );
            }
        }
    }

    fn tick_if_due_collect(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<Option<Vec<RuntimeEventMessage>>> {
        #[cfg(feature = "display-surface")]
        for display in &mut self.displays {
            display
                .service_control(self.runtime.context())
                .map_err(|error| -> Box<dyn Error> { Box::new(AppFailure(error)) })?;
        }
        self.apply_file_reports(control)?;
        let covered = self.service_playback(control)?;
        let host_deadline = self.next_deadline()?;
        let now = Instant::now();
        if now < host_deadline {
            return Ok(None);
        }
        if !covered {
            return Ok(None);
        }
        self.telemetry.observe_host_lateness(host_deadline, now);
        self.realize_one(control, server).map(Some)
    }

    fn wait_and_tick(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        process_shutdown: Option<&ProcessShutdown>,
    ) -> AppResult<Option<Vec<RuntimeEventMessage>>> {
        let (host_deadline, observed) = loop {
            if requested_daemon_shutdown(Some(&*self), process_shutdown).is_some() {
                return Ok(None);
            }
            self.apply_file_reports(control)?;
            let covered = self.service_playback(control)?;
            let host_deadline = self.next_deadline()?;
            let now = Instant::now();
            if covered && now >= host_deadline {
                break (host_deadline, now);
            }
            let host_wait = host_deadline.saturating_duration_since(now);
            thread::sleep(host_wait.min(NATIVE_IO_POLL_INTERVAL));
        };
        self.telemetry
            .observe_host_lateness(host_deadline, observed);
        self.realize_one(control, server).map(Some)
    }

    /// Hands the engine every media-file growth, completion, and fault the
    /// file workers reported. A change voids the projected frame, whose media
    /// ticks it moves.
    fn apply_file_reports(&mut self, control: &mut ControlService<Policy>) -> AppResult<()> {
        self.apply_live_stills(control)?;
        let reports = self.files.take_reports();
        if reports.is_empty() {
            return Ok(());
        }
        for (input, report) in reports {
            let applied = match report {
                native_files::FileReport::Source { frames, complete } => {
                    control.update_media_source(input, frames, complete)
                }
                native_files::FileReport::Fault(fault) => {
                    eprintln!(
                        "FREEMIXD_MEDIA\tv=1\tinput={input}\tfault={}",
                        fault.token()
                    );
                    control.set_media_fault(input, fault)
                }
                // A file that reopened opens the transport of an input that
                // had no source at startup (closed or offline then); one
                // with a startup source reopened it already.
                native_files::FileReport::Opened(source) => {
                    if control.input_lifecycle(input).closed || control.media_source_open(input) {
                        Ok(())
                    } else {
                        control.set_media_source(input, Some(source))
                    }
                }
            };
            applied.map_err(|error| {
                AppFailure(format!(
                    "media input {input} cannot take its source report: {error}"
                ))
            })?;
        }
        self.invalidate_projection();
        Ok(())
    }

    /// Opens every media-file input's transport on its decoder's source.
    fn attach_media_sources(&mut self, control: &mut ControlService<Policy>) -> AppResult<()> {
        for (input, source) in self.files.sources() {
            control
                .set_media_source(*input, Some(source.clone()))
                .map_err(|error| AppFailure(format!("media input {input} cannot open: {error}")))?;
        }
        self.sync_input_lifecycle(control)
    }

    /// Releases the decoder of every closed media-file input and reopens
    /// it, with its engine transport, once the input reopens (IN-026). A
    /// paused input holds its frame: the engine stops moving its transport.
    ///
    /// Only reopening reopens the transport: a source a fault closed stays
    /// closed (and its input offline) until the input is closed and
    /// reopened, so later commands do not bring back a transport whose
    /// worker no longer decodes.
    ///
    /// An input without a startup source (closed or offline at startup)
    /// reopens its transport once its worker reports the file open, and an
    /// input whose worker failed stays offline.
    fn sync_input_lifecycle(&mut self, control: &mut ControlService<Policy>) -> AppResult<()> {
        self.network
            .sync(&control.network_stream_configs(), |input| {
                control.input_lifecycle(input).closed
            });
        self.system_audio
            .sync(&control.system_audio_inputs(), |input| {
                control.input_lifecycle(input).closed
            });
        if let Some(captures) = &mut self.captures {
            let inputs = control
                .capture_inputs()
                .into_iter()
                .map(|(input, config)| (input, config.stable_key, config.settings))
                .collect::<Vec<_>>();
            captures.sync(&inputs, |input| control.input_lifecycle(input).closed);
        }
        for input in self.files.inputs() {
            let closed = control.input_lifecycle(input).closed;
            let reopened = self.files.is_closed(input) && !closed;
            self.files.set_closed(input, closed);
            if reopened
                && !self.files.is_failed(input)
                && !control.media_source_open(input)
                && let Some(source) = self.files.source(input).cloned()
            {
                control
                    .set_media_source(input, Some(source))
                    .map_err(|error| {
                        AppFailure(format!("media input {input} cannot reopen: {error}"))
                    })?;
            }
        }
        Ok(())
    }

    fn service_playback(&mut self, control: &ControlService<Policy>) -> AppResult<bool> {
        #[cfg(target_os = "macos")]
        self.cameras.poll_into(&self.runtime, &mut self.playback)?;
        self.titles.set_frame(self.pacer.next_frame().get());
        for (input, frame) in self.titles.take_completed() {
            // A title removed live keeps rendering until the worker learns
            // of it; nothing of it is ingested.
            if !self.playback.registry().contains(input) {
                continue;
            }
            self.runtime
                .ingest_live_video_frame_blocking(&mut self.playback, input, frame)?;
        }
        for presented in self.files.take_completed() {
            if let Some(frame) = presented.video {
                self.runtime.ingest_live_video_frame_blocking(
                    &mut self.playback,
                    presented.input,
                    frame,
                )?;
            }
            if let Some(audio) = &presented.audio {
                self.master
                    .push_frame_audio(presented.input, presented.frame, audio)?;
            }
        }
        let deadline = control.next_frame_deadline()?;
        let video_covered = self
            .runtime
            .service_source_playback_blocking(&mut self.playback, deadline)
            .map_err(Box::<dyn Error>::from)?;
        if self.projected_frame.is_none() {
            self.projected_frame = Some(control.project_next_frame()?);
        }
        let projected = self
            .projected_frame
            .as_ref()
            .expect("next frame projection was initialized");
        // Network streams hand over the projected frame's picture and audio
        // from their jitter buffers, once per frame; they never wait.
        let captured = self
            .captures
            .as_mut()
            .map(|captures| captures.pull(projected.frame.get()))
            .unwrap_or_default();
        let system_audio = self.system_audio.pull(projected.frame.get());
        for pulled in self
            .network
            .pull(projected.frame.get())
            .into_iter()
            .chain(captured)
            .chain(system_audio)
        {
            if let Some(frame) = pulled.video {
                self.runtime.ingest_live_video_frame_blocking(
                    &mut self.playback,
                    pulled.input,
                    frame,
                )?;
            }
            if let Some(audio) = &pulled.audio {
                self.master
                    .push_frame_audio(pulled.input, projected.frame.get(), audio)?;
            }
        }
        self.titles.observe_frame(projected);
        self.files.observe_frame(projected);
        // An animating title holds the frame until its projected pose is
        // ingested (bounded), so each In and Out frame airs as projected;
        // a media file until its projected tick is presented (bounded).
        let titles_covered = self.titles.covered(projected, self.next_deadline()?);
        let files_covered = self.files.covered(projected, self.next_deadline()?);
        let audio_covered = self
            .master
            .service_project_next_frame(projected, &self.project_plan)?;
        let stinger_covered = match self.project_plan.stinger_frame_request(projected)? {
            Some(request) => self
                .runtime
                .service_source_playback_for_input_blocking(
                    &mut self.stingers,
                    request.input,
                    request.deadline,
                )
                .map_err(Box::<dyn Error>::from)?,
            None => true,
        };
        Ok(audio_covered && video_covered && stinger_covered && titles_covered && files_covered)
    }

    /// Attaches the grades of `control`'s current revision to the project
    /// plan, baking and uploading them only when they changed.
    fn refresh_grades(&mut self, control: &ControlService<Policy>) -> AppResult<()> {
        let compiled = self.grade_cache.at(control);
        let current = match &self.prepared_grades {
            Some((cached, prepared)) if Arc::ptr_eq(cached, &compiled) => prepared.clone(),
            _ => {
                let revision = control.snapshot().snapshot.revision;
                let prepared = self
                    .runtime
                    .prepare_project_grades_blocking(revision, &compiled)
                    .map_err(Box::<dyn Error>::from)?;
                self.prepared_grades = Some((compiled, prepared.clone()));
                prepared
            }
        };
        if *self.project_plan.grades() != current {
            self.project_plan.set_grades(current);
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn realize_one(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
    ) -> AppResult<Vec<RuntimeEventMessage>> {
        // Accepted StreamStart commands start on this frame boundary, before
        // the frame is produced, so the first fed pair is the next real frame.
        self.start_pending_streams();
        self.refresh_grades(control)?;
        let shows_preview = self.display_shows_preview();
        self.project_plan
            .refresh_scene_models(|scene| control.scene_model(scene));
        let runtime = &self.runtime;
        let registry = self.playback.registry();
        let stinger_registry = self.stingers.registry();
        let master = &mut self.master;
        let monitor_output = &mut self.monitor_output;
        let project_plan = &mut self.project_plan;
        let projected_frame = self.projected_frame.as_ref();
        let latest_output = &mut self.latest_output;
        let latest_project_outputs = &mut self.latest_project_outputs;
        let mut audio = None;
        let mut live_audio = None;
        let mut override_audio = Vec::new();
        let mut fade_to_black_excluded = Vec::new();
        let mut monitor_frame = None;
        let mut preview_source = None;
        let outcome = control.tick_with_realizer(server, |frame| {
            debug_assert_eq!(projected_frame, Some(frame));
            monitor_frame = Some((frame.frame.get(), frame.program.primary));
            if shows_preview {
                preview_source = Some(frame.clone());
            }
            for &(input, state) in &frame.input_audio_strip_updates {
                let strip = model_audio_strip_state(state);
                master
                    .set_input_audio_strip(input, strip)
                    .map_err(NativeRealizationError::Audio)?;
                project_plan
                    .set_input_audio_strip(input, strip)
                    .map_err(NativeRealizationError::Audio)?;
            }
            for update in &frame.audio_mixer_updates {
                master
                    .apply_audio_mixer_update(update)
                    .map_err(NativeRealizationError::Audio)?;
                match *update {
                    fm_engine::EngineAudioMixerUpdate::Membership { input, group } => project_plan
                        .set_input_audio_group(input, group)
                        .map_err(NativeRealizationError::Audio)?,
                    fm_engine::EngineAudioMixerUpdate::GroupLevel { group, state } => {
                        project_plan.set_audio_group_level(group, state.gain_millidb, state.muted);
                    }
                    fm_engine::EngineAudioMixerUpdate::GroupSolo { group, soloed } => {
                        project_plan.set_audio_group_solo(group, soloed);
                    }
                    fm_engine::EngineAudioMixerUpdate::GroupParent { group, parent } => {
                        project_plan.set_audio_group_parent(group, parent);
                    }
                    fm_engine::EngineAudioMixerUpdate::SoloMode(mode) => {
                        project_plan.set_audio_solo_mode(mode);
                    }
                    fm_engine::EngineAudioMixerUpdate::MonitorDevice(ref device) => {
                        monitor_output.set_device(device.clone());
                    }
                    fm_engine::EngineAudioMixerUpdate::MonitorLevel(state) => {
                        monitor_output.set_level(state.gain_millidb, state.muted);
                    }
                }
            }
            if project_plan.output_ids().is_empty() {
                let output = runtime
                    .render_project_frame_result_with_stingers_blocking(
                        registry,
                        stinger_registry,
                        project_plan,
                        frame,
                    )
                    .map_err(NativeRealizationError::Video)?;
                *latest_output = Some(output);
                latest_project_outputs.clear();
            } else {
                let outputs = runtime
                    .render_project_outputs_with_stingers_blocking(
                        registry,
                        stinger_registry,
                        project_plan,
                        frame,
                    )
                    .map_err(NativeRealizationError::Video)?;
                *latest_project_outputs = outputs;
                *latest_output = None;
            }
            let block = master
                .render_project_frame_audio(frame, project_plan)
                .map_err(NativeRealizationError::Audio)?;
            audio = Some(block);
            live_audio = master.take_live_audio();
            override_audio = master.take_override_audio();
            fade_to_black_excluded.clone_from(&frame.fade_to_black_excluded_outputs);
            Ok::<(), NativeRealizationError>(())
        })?;
        if let Some(meters) = master.take_audio_meters() {
            let sequence = self
                .audio_meter_sequence
                .checked_add(1)
                .ok_or_else(|| AppFailure("audio meter sequence exhausted".into()))?;
            let samples = usize::try_from(meters.end_sample - meters.start_sample)
                .map_err(|_| AppFailure("audio meter interval exceeds memory".into()))?;
            self.monitor_output
                .push(meters.monitor_planes, samples, meters.sample_rate);
            let output = self.monitor_output.state();
            self.pending_audio_meters =
                Some(audio_meters_message(server, sequence, &meters, output));
            self.audio_meter_sequence = sequence;
        }
        self.install_stinger_mutation()?;
        self.projected_frame = None;
        self.pacer.advance()?;
        let stream_audio = if self.streams.is_empty() {
            None
        } else {
            audio
                .clone()
                .map(|audio| (audio, live_audio.clone(), override_audio.clone()))
        };
        if let Some(audio) = audio {
            self.rotate_due_recorders(audio.timing().sequence());
            Self::capture_recorders(
                &mut self.recorders,
                &self.record_outputs,
                &self.runtime,
                &self.latest_project_outputs,
                self.latest_output.as_ref(),
                &CaptureAudio {
                    faded: audio,
                    live: live_audio,
                    excluded: &fade_to_black_excluded,
                    overrides: &override_audio,
                },
            );
        }
        if let Some((audio, live, overrides)) = stream_audio {
            Self::capture_streams(
                &mut self.streams,
                &mut self.stream_groups,
                &mut self.output_set,
                &self.runtime,
                &self.latest_project_outputs,
                self.latest_output.as_ref(),
                &CaptureAudio {
                    faded: audio,
                    live,
                    excluded: &fade_to_black_excluded,
                    overrides: &overrides,
                },
            );
        }
        if let Some((frame, input)) = monitor_frame {
            self.monitor.capture(
                &self.runtime,
                resolve_project_output_texture(
                    None,
                    &self.latest_project_outputs,
                    self.latest_output.as_ref(),
                ),
                server,
                frame,
                input,
                &mut self.telemetry,
            );
        }
        // Display outputs follow Program's own consumers, so a Preview
        // composite never delays them. A loop that is behind its frame
        // deadlines drops display frames rather than delaying the next
        // Program frame and the sessions served between frames.
        let started = Instant::now();
        if !self.display_presenting()
            || self
                .display_pacing
                .due(started, started >= self.next_deadline()?)
        {
            let preview = control.snapshot().snapshot.realized_preview.to_domain();
            self.render_display_preview(preview_source, preview);
            self.present_display_outputs()?;
            self.display_pacing.presented(started, Instant::now());
        } else if let Some(host) = &mut self.display_inventory {
            // Hot-plug is still followed on a dropped frame.
            host.poll(&self.runtime, started);
        }
        let now = Instant::now();
        self.sample_local_output_status(now);
        self.advance_stream_routes(server)?;
        self.sample_disk_space(now);
        self.observe_native_telemetry();
        Ok(outcome.runtime_events)
    }

    #[allow(clippy::unused_self)]
    fn shutdown_requested(&self) -> bool {
        #[cfg(feature = "display-surface")]
        if let Some(display) = self.displays.first() {
            return display.shutdown_requested();
        }
        false
    }

    /// Finalizes every open segment, then joins every detached finalizer
    /// within the recorder's own stop budget so each runtime-stopped segment
    /// has emitted its `FREEMIXD_RECORDER` report before exit.
    ///
    /// Slots finalize independently: one slot's failure is collected and
    /// returned after every other slot has had its bounded stop, never
    /// aborting the remaining teardown.
    fn finalize_recorder(&mut self) -> AppResult<()> {
        let mut first_error = None;
        for recorder in std::mem::take(&mut self.recording_finalizers) {
            let result = match recorder
                .done
                .recv_timeout(PROGRAM_RECORDER_FINALIZE_JOIN_TIMEOUT)
            {
                Ok(clean) => {
                    let ProgramRecordingFinalizer { worker, .. } = recorder;
                    let _ = worker.join();
                    if clean {
                        Ok(())
                    } else {
                        Err(AppFailure("Program recording did not finalize cleanly".into()).into())
                    }
                }
                Err(_) => {
                    // The worker is still inside its bounded stop budget;
                    // detach rather than block shutdown past the budget.
                    Err(AppFailure("program recording finalization did not complete".into()).into())
                }
            };
            if result.is_err() {
                first_error.get_or_insert(result);
            }
        }
        let mut stopped = Vec::new();
        for slot in &mut self.recorders {
            if let ProgramRecording::Running(mut recorder) =
                std::mem::replace(slot, ProgramRecording::Off)
            {
                if let Err(error) = recorder.stop_and_report() {
                    first_error.get_or_insert(Err(error));
                }
                stopped.push(recorder);
            }
        }
        if !stopped.is_empty() {
            self.telemetry
                .observe_recorders(stopped.iter().map(|recorder| &**recorder));
        }
        match first_error {
            Some(result) => result,
            None => Ok(()),
        }
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn finalize_cameras(&mut self) -> AppResult<()> {
        #[cfg(target_os = "macos")]
        {
            self.cameras.shutdown()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(())
        }
    }

    #[cfg(feature = "display-surface")]
    fn program_telemetry(&self) -> Option<fm_gpu::PresentationTelemetry> {
        self.displays
            .first()
            .map(program_surface::ProgramPresentation::telemetry)
    }

    fn observe_native_telemetry(&mut self) {
        self.telemetry.observe_audio(&self.master);
        #[cfg(target_os = "macos")]
        if !self.camera_telemetry_frozen {
            self.telemetry.camera = aggregate_camera_telemetry(&self.cameras.source_telemetry());
        }
        let running_recorders = self.recorders.iter().filter_map(|slot| match slot {
            ProgramRecording::Running(recorder) => Some(&**recorder),
            ProgramRecording::Off => None,
        });
        self.telemetry.observe_recorders(running_recorders);
        let streams_enqueued = self
            .streams
            .iter()
            .map(|stream| stream.ledger.enqueued_pairs)
            .fold(0_u64, u64::saturating_add);
        let streams_dropped = self
            .streams
            .iter()
            .map(|stream| stream.ledger.dropped_pairs)
            .fold(0_u64, u64::saturating_add);
        self.telemetry
            .observe_streams(streams_enqueued, streams_dropped);
        // The disk sample lags telemetry by at most one frame; `None` until
        // the first sample keeps the record honest about "never sampled".
        self.telemetry.disk_free_bytes = self.disk_free_bytes;
        self.telemetry.disk_alert = self.disk_free_bytes.map(|_| self.disk_alert.state());
        self.telemetry.observe_gpu(self.runtime.context());
    }

    fn emit_telemetry(&mut self) {
        if self.telemetry_emitted {
            return;
        }
        self.emit_camera_source_telemetry();
        self.observe_native_telemetry();
        if !self.files.inputs().is_empty() {
            eprintln!(
                "FREEMIXD_MEDIA_FILES\tv=1\tinputs={}\tlate_frames={}",
                self.files.inputs().len(),
                self.files.late_frames()
            );
        }
        #[cfg(feature = "display-surface")]
        let presentation = self
            .displays
            .first_mut()
            .map(program_surface::ProgramPresentation::telemetry_for_shutdown);
        #[cfg(feature = "display-surface")]
        for display in self.displays.iter_mut().skip(1) {
            let _ = display.telemetry_for_shutdown();
        }
        #[cfg(not(feature = "display-surface"))]
        let presentation = None;
        self.telemetry.emit(presentation);
        self.telemetry_emitted = true;
    }

    #[allow(clippy::unused_self)]
    fn emit_camera_source_telemetry(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if self.camera_telemetry_frozen {
                return;
            }
            if let Some(sources) = self.cameras.emit_source_telemetry() {
                self.telemetry.camera = aggregate_camera_telemetry(&sources);
            }
            self.camera_telemetry_frozen = true;
        }
    }

    fn next_deadline(&self) -> AppResult<Instant> {
        let elapsed_offset = host_deadline_offset(&self.pacer)?
            .checked_sub(self.pacer_start_offset)
            .ok_or_else(|| {
                AppFailure("native media pacer moved before its restored cursor".into())
            })?;
        self.origin.checked_add(elapsed_offset).ok_or_else(|| {
            AppFailure("native media host deadline exceeds Instant range".into()).into()
        })
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

/// Encoded-fallback Program monitor tap (Protocol 2.21).
///
/// While an eligible session wants frames, every `interval`-th realized frame
/// is transformed and downscaled on the GPU into a lazily created readback
/// target at the fitted monitor size, read back to the CPU, QOI-encoded, and
/// held as one atomic latest-wins `monitor_frame` group. The engine produces
/// no native Preview image, so there is no native Preview tap. Readback or
/// encode failures skip the frame and are counted in telemetry; they never
/// fail the frame tick.
#[cfg(feature = "native-media")]
struct NativeMonitorTap {
    dimensions: Option<(u16, u16)>,
    interval: u64,
    readback: Option<NativeProgramReadback>,
    demand: bool,
    sequence: u64,
    pending: Option<Vec<u8>>,
}

#[cfg(feature = "native-media")]
impl NativeMonitorTap {
    /// Native monitor frames target at most about ten per second.
    const TARGET_FPS: u64 = 10;

    fn new(settings: &fm_model::ProjectSettings) -> Self {
        let dimensions = settings.video.dimensions;
        let rate = settings.frame_rate;
        let whole_fps = u64::from(rate.numerator()).div_ceil(u64::from(rate.denominator().max(1)));
        Self {
            dimensions: monitor_tap::fit_monitor(dimensions.width(), dimensions.height()),
            interval: whole_fps.div_ceil(Self::TARGET_FPS).max(1),
            readback: None,
            demand: false,
            sequence: 0,
            pending: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn capture(
        &mut self,
        runtime: &NativeMediaRuntime,
        program: Option<&NativeTexture>,
        server: &ServerIdentity,
        frame: u64,
        input: InputId,
        telemetry: &mut NativeRuntimeTelemetry,
    ) {
        if !self.demand || !frame.is_multiple_of(self.interval) {
            return;
        }
        let (Some((width, height)), Some(program)) = (self.dimensions, program) else {
            return;
        };
        if self.encode(runtime, program, server, frame, input, width, height) {
            telemetry.monitor_frames_published =
                telemetry.monitor_frames_published.saturating_add(1);
        } else {
            telemetry.monitor_frames_failed = telemetry.monitor_frames_failed.saturating_add(1);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn encode(
        &mut self,
        runtime: &NativeMediaRuntime,
        program: &NativeTexture,
        server: &ServerIdentity,
        frame: u64,
        input: InputId,
        width: u16,
        height: u16,
    ) -> bool {
        if self.readback.is_none() {
            let (Some(target_width), Some(target_height)) = (
                NonZeroU32::new(u32::from(width)),
                NonZeroU32::new(u32::from(height)),
            ) else {
                return false;
            };
            match runtime.create_program_readback_blocking(target_width, target_height) {
                Ok(readback) => self.readback = Some(readback),
                Err(_) => return false,
            }
        }
        let Some(readback) = self.readback.as_mut() else {
            return false;
        };
        // The synchronous diagnostic readback path, bounded by its timeout.
        let Ok(pixels) = runtime.readback_program_blocking(readback, program) else {
            return false;
        };
        let (Ok(stride), Some(sequence)) =
            (usize::try_from(pixels.stride), self.sequence.checked_add(1))
        else {
            return false;
        };
        if pixels.width != u32::from(width) || pixels.height != u32::from(height) {
            return false;
        }
        let picture = monitor_tap::MonitorPicture {
            tap: MonitorTap::Program,
            sequence,
            frame,
            input,
            origin: fm_protocol::MonitorFrameOrigin::Native,
            width,
            height,
            pixels: &pixels.rgba,
            stride,
            bytes_per_pixel: 4,
        };
        let Some(bytes) = monitor_tap::encode_picture(server, &picture) else {
            return false;
        };
        self.sequence = sequence;
        self.pending = Some(bytes);
        true
    }
}

#[cfg(feature = "native-media")]
fn audio_meters_message(
    server: &ServerIdentity,
    sequence: u64,
    meters: &NativeAudioMeters<'_>,
    output: fm_protocol::AudioMonitorOutputState,
) -> AudioMetersMessage {
    AudioMetersMessage {
        server: server.clone(),
        sequence,
        frame: meters.frame,
        start_sample: meters.start_sample,
        end_sample: meters.end_sample,
        master: meters
            .master
            .iter()
            .copied()
            .map(protocol_audio_meter)
            .collect(),
        inputs: meters
            .inputs
            .iter()
            .zip(meters.input_meters.chunks_exact(meters.channels))
            .map(|(&input, channels)| InputAudioMeters {
                input: WireInputId::from_domain(input),
                channels: channels.iter().copied().map(protocol_audio_meter).collect(),
            })
            .collect(),
        monitor: Some(fm_protocol::AudioMonitorMeters {
            solo: meters.monitor_solo,
            output,
            channels: meters
                .monitor
                .iter()
                .copied()
                .map(protocol_audio_meter)
                .collect(),
        }),
    }
}

#[cfg(feature = "native-media")]
fn protocol_audio_meter(meter: fm_audio::ChannelMeter) -> AudioMeterChannel {
    let peak_millionths = meter_level_millionths(meter.peak);
    AudioMeterChannel {
        peak_millionths,
        rms_millionths: meter_level_millionths(meter.rms).min(peak_millionths),
    }
}

#[cfg(feature = "native-media")]
// Levels are clamped to [0, u32::MAX] before the cast; truncation is not reachable.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn meter_level_millionths(level: f32) -> u32 {
    (f64::from(level.max(0.0)) * f64::from(AUDIO_METER_LEVEL_SCALE))
        .round()
        .min(f64::from(u32::MAX)) as u32
}

#[cfg(feature = "native-media")]
impl Drop for NativeDaemon {
    fn drop(&mut self) {
        if let Some(mutation) = self.pending_stinger_mutation.take() {
            let _ = self.stinger_retirements.discard(mutation);
        }
        for result in self.finalize_streams() {
            // Drop must never panic; the serve path reports these results.
            drop(result);
        }
        let _ = self.finalize_recorder();
        self.emit_camera_source_telemetry();
        #[cfg(target_os = "macos")]
        let _ = self.cameras.shutdown();
        self.emit_telemetry();
    }
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

#[cfg(not(feature = "native-media"))]
struct NativeDaemon;

#[cfg(not(feature = "native-media"))]
impl NativeDaemon {
    #[allow(clippy::unused_self)]
    fn invalidate_projection(&mut self) {}

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn attach_media_sources(&mut self, _control: &mut ControlService<Policy>) -> AppResult<()> {
        Ok(())
    }

    fn start(
        _store: &ProjectStore,
        _stored: &StoredProject,
        _camera_helper: Option<&Path>,
    ) -> AppResult<Self> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    fn attach_display_inventory(&mut self, _inventory: PathBuf) -> AppResult<()> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn sync_display_outputs(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
    ) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn report_network_streams(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
    ) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self)]
    fn tick_if_due(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
    ) -> AppResult<()> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self)]
    fn take_audio_meters(&mut self) -> Option<AudioMetersMessage> {
        None
    }

    #[allow(clippy::unused_self)]
    fn take_stream_status(&mut self) -> Option<StreamStatusMessage> {
        None
    }

    #[allow(clippy::unused_self)]
    fn set_monitor_demand(&mut self, _demand: bool) {}

    #[allow(clippy::unused_self)]
    fn live_source_frames(&self) -> scene_preview::LiveSourceFrames {
        scene_preview::LiveSourceFrames::default()
    }

    #[allow(clippy::unused_self)]
    fn take_monitor_frame(&mut self) -> Option<(MonitorTap, Vec<u8>)> {
        None
    }

    #[allow(clippy::unused_self)]
    fn wait_and_tick(
        &mut self,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
        _process_shutdown: Option<&ProcessShutdown>,
    ) -> AppResult<Option<Vec<RuntimeEventMessage>>> {
        Err(AppFailure("native-media support was not compiled in".into()).into())
    }

    #[allow(clippy::unused_self)]
    fn shutdown_requested(&self) -> bool {
        false
    }

    #[allow(clippy::unused_self)]
    fn recording_settling(&self, _slot: usize) -> bool {
        false
    }

    #[allow(clippy::unused_self)]
    fn advance_recordings(&mut self) {}

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn start_and_prime_configured_recorders(
        &mut self,
        _stored: &StoredProject,
        _control: &mut ControlService<Policy>,
        _server: &ServerIdentity,
        _shutdown: Option<&ProcessShutdown>,
        _before_frame: &mut BeforeNativeFrame<'_>,
    ) -> AppResult<bool> {
        Ok(true)
    }

    #[allow(clippy::unused_self, clippy::too_many_arguments)]
    fn configure_recordings(
        &mut self,
        _primary: Option<PathBuf>,
        _primary_output: Option<fm_types::OutputId>,
        _secondary: Option<PathBuf>,
        _secondary_output: Option<fm_types::OutputId>,
        _wav: bool,
        _segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
        _primary_bitrate: Option<u32>,
        _primary_audio_bitrate: Option<u32>,
        _secondary_bitrate: Option<u32>,
        _secondary_audio_bitrate: Option<u32>,
    ) {
    }

    #[allow(clippy::unused_self)]
    fn recording_configured(&self) -> bool {
        false
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    fn configure_record_fallback(&mut self, _fallback: Option<PathBuf>) {}

    #[allow(clippy::unused_self)]
    fn recover_recording_sessions(&self) {}

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn finalize_recorder(&mut self) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn prime_startup_streams(&mut self, _stored: &StoredProject) {}

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn finalize_streams(&mut self) -> Vec<AppResult<()>> {
        Vec::new()
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn finalize_cameras(&mut self) -> AppResult<()> {
        Ok(())
    }

    #[allow(clippy::unused_self)]
    fn emit_camera_source_telemetry(&mut self) {}

    #[allow(clippy::unused_self)]
    fn emit_telemetry(&mut self) {}
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

#[cfg(feature = "native-media")]
#[allow(clippy::too_many_lines)]
fn resolve_native_sources(
    store: &ProjectStore,
    stored: &StoredProject,
    camera_helper: Option<&Path>,
) -> AppResult<NativeSourceResolution> {
    let settings = stored.project().settings();
    let captures = native_capture_backend().is_some();
    let mut sources = Vec::with_capacity(stored.project().inputs().len());
    for input in stored.project().inputs() {
        let source = match &input.kind {
            // A capture input starts on a blank picture; its supervisor
            // ingests every picture after it (IN-001).
            InputKind::Device { .. } if captures => {
                let dimensions = settings.video.dimensions;
                native_streams::startup_picture(
                    (dimensions.width(), dimensions.height()),
                    settings.frame_rate,
                    native_clock_domain(),
                )
                .map(|frame| NativeResolvedSource::LiveFrame {
                    input: input.id,
                    frame,
                })
                .ok_or_else(|| {
                    AppFailure(format!("native capture input {} cannot be timed", input.id)).into()
                })
            }
            InputKind::Media { asset_uri, .. } => resolve_native_media_source(
                store,
                input.id,
                asset_uri,
                native_clock_domain(),
                settings.video.dimensions,
            ),
            InputKind::Simulated(simulated) => {
                native_generator_frame(input.id, simulated, settings).map(|frame| {
                    NativeResolvedSource::RetainedFrame {
                        input: input.id,
                        frame,
                    }
                })
            }
            InputKind::Color => Err(AppFailure(format!(
                "native color input {} has no configured RGBA value",
                input.id
            ))
            .into()),
            // Title inputs are rendered by [`resolve_native_title_sources`].
            InputKind::Device { .. } | InputKind::Scene { .. } | InputKind::Title(_) => continue,
            // A network stream starts on a blank picture; its receiver
            // ingests every picture after it (IN-015). A system audio
            // input keeps its blank picture (AU-011).
            InputKind::NetworkStream(_) | InputKind::SystemAudio(_) => {
                let dimensions = settings.video.dimensions;
                native_streams::startup_picture(
                    (dimensions.width(), dimensions.height()),
                    settings.frame_rate,
                    native_clock_domain(),
                )
                .map(|frame| NativeResolvedSource::LiveFrame {
                    input: input.id,
                    frame,
                })
                .ok_or_else(|| {
                    AppFailure(format!("native network input {} cannot be timed", input.id)).into()
                })
            }
            // Model-only so far: refuse rather than play some other picture.
            InputKind::Alias { .. } | InputKind::MixFeed(_) => Err(AppFailure(format!(
                "native alias or program/preview feed input {} is not yet realized",
                input.id
            ))
            .into()),
        }?;
        sources.push(source);
    }

    #[cfg(target_os = "macos")]
    {
        if captures {
            return Ok(NativeSourceResolution {
                sources,
                cameras: NativeCameraInputs::default(),
            });
        }
        let (mut device_sources, cameras) = resolve_macos_camera_sources(stored, camera_helper)?;
        sources.append(&mut device_sources);
        Ok(NativeSourceResolution { sources, cameras })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = camera_helper;
        if !captures
            && let Some(input) = stored
                .project()
                .inputs()
                .iter()
                .find(|input| matches!(input.kind, InputKind::Device { .. }))
        {
            return Err(AppFailure(format!(
                "native device input {} has no adapter on this platform",
                input.id
            ))
            .into());
        }
        Ok(NativeSourceResolution { sources })
    }
}

/// Renders the first frame of every title input and appends it to `sources`
/// as a live frame, returning the worker that re-renders them off the loop.
#[cfg(feature = "native-media")]
fn resolve_native_title_sources(
    store: &ProjectStore,
    stored: &StoredProject,
    sources: &mut Vec<NativeResolvedSource>,
) -> AppResult<native_titles::NativeTitleSources> {
    let project = stored.project();
    let assets = if project
        .inputs()
        .iter()
        .any(|input| matches!(input.kind, InputKind::Title(_)))
    {
        title_render::TitleAssets::load(store, project)
    } else {
        title_render::TitleAssets::default()
    };
    let (titles, seeds) =
        native_titles::NativeTitleSources::start(Arc::new(assets), project, native_clock_domain())
            .map_err(AppFailure)?;
    sources.extend(
        seeds
            .into_iter()
            .map(|(input, frame)| NativeResolvedSource::LiveFrame { input, frame }),
    );
    Ok(titles)
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[allow(clippy::too_many_lines)]
fn resolve_macos_camera_sources(
    stored: &StoredProject,
    camera_helper: Option<&Path>,
) -> AppResult<(Vec<NativeResolvedSource>, NativeCameraInputs)> {
    resolve_macos_camera_sources_with_policy(stored, camera_helper, CAMERA_RECOVERY_POLICY)
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
#[allow(clippy::too_many_lines)]
fn resolve_macos_camera_sources_with_policy(
    stored: &StoredProject,
    camera_helper: Option<&Path>,
    recovery_policy: CameraRecoveryPolicy,
) -> AppResult<(Vec<NativeResolvedSource>, NativeCameraInputs)> {
    let device_inputs = stored
        .project()
        .inputs()
        .iter()
        .filter_map(|input| match &input.kind {
            InputKind::Device { stable_key, .. } => Some((input.id, stable_key.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    if device_inputs.is_empty() {
        return Ok((Vec::new(), NativeCameraInputs::default()));
    }
    if device_inputs.len() > NativeSourceLimits::DEFAULT_MAX_MEDIA_INPUTS {
        return Err(AppFailure(format!(
            "camera input count {} exceeds native source limit {}",
            device_inputs.len(),
            NativeSourceLimits::DEFAULT_MAX_MEDIA_INPUTS
        ))
        .into());
    }
    let mut keys = std::collections::BTreeSet::new();
    if let Some((input, key)) = device_inputs.iter().find(|(_, key)| !keys.insert(*key)) {
        return Err(AppFailure(format!(
            "camera input {input} duplicates stable key `{}`",
            diagnostic_field(key)
        ))
        .into());
    }

    let adapter = match camera_helper {
        Some(path) => MacosCameraAdapter::discover_with_helper(path)?,
        None => MacosCameraAdapter::discover()?,
    };
    if !adapter.permission().is_granted() {
        return Err(AppFailure(
            "camera permission is not granted; use `freemix-capture-node cameras --request-permission` from an interactive desktop session"
                .into(),
        )
        .into());
    }
    let frame_rate = stored.project().settings().frame_rate;
    let dimensions = stored.project().settings().video.dimensions;
    let mut cameras = NativeCameraInputs::default();
    for (input, stable_key) in device_inputs {
        let mut source = adapter.open_video_source_by_stable_key(stable_key)?;
        let format = source
            .exact_video_format_at_rate(dimensions.width(), dimensions.height(), frame_rate)
            .ok_or_else(|| {
                AppFailure(format!(
                    "camera input {input} has no exact {}x{} at {}/{} fps BGRA mode",
                    dimensions.width(),
                    dimensions.height(),
                    frame_rate.numerator(),
                    frame_rate.denominator()
                ))
            })?;
        let clock_domain = source
            .descriptor()
            .capabilities
            .clocks
            .first()
            .ok_or_else(|| AppFailure(format!("camera input {input} has no advertised clock")))?
            .domain;
        source.open(IoOpenOptions {
            format,
            clock_domain,
            memory_domain: MemoryDomain::Cpu,
            queue_capacity: NonZeroUsize::MIN,
            signal_loss: SignalLossPolicy::Hold,
        })?;
        cameras.inputs.push(NativeCameraInput {
            input,
            supervisor: Arc::new(Mutex::new(CameraSupervisorState {
                frame: CameraFrameSlot::default(),
                snapshot: CameraWorkerSnapshot::running(&source),
            })),
            source: Some(source),
            worker: None,
            recovery_policy,
            ingested_frames: 0,
            ingest_failed: 0,
            preflight_depth: 0,
            preflight_discarded: 0,
            last_ingested_sequence: None,
            last_ingested_discontinuity: false,
        });
    }

    let start_results = thread::scope(|scope| -> AppResult<Vec<(InputId, Result<(), String>)>> {
        let mut handles = Vec::with_capacity(cameras.inputs.len());
        for camera in &mut cameras.inputs {
            let input = camera.input;
            let source = camera.source.as_mut().expect("camera source is configured");
            handles.push(
                thread::Builder::new()
                    .name(format!("freemix-camera-start-{input}"))
                    .spawn_scoped(scope, move || {
                        (input, source.start().map_err(|error| error.to_string()))
                    })?,
            );
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| AppFailure("camera startup worker panicked".into()).into())
            })
            .collect()
    });
    let start_results = match start_results {
        Ok(results) => results,
        Err(error) => {
            return Err(camera_startup_failure(
                &mut cameras,
                format!("camera startup coordination failed: {error}"),
            ));
        }
    };
    if let Some((input, Err(detail))) = start_results
        .into_iter()
        .find(|(_, result)| result.is_err())
    {
        return Err(camera_startup_failure(
            &mut cameras,
            format!("camera input {input} failed to start: {detail}"),
        ));
    }

    let deadline = Instant::now()
        .checked_add(CAMERA_INITIAL_FRAME_TIMEOUT)
        .ok_or_else(|| AppFailure("camera initial-frame deadline overflow".into()))?;
    let mut initial_frames = BTreeMap::new();
    while initial_frames.len() < cameras.inputs.len() {
        for camera in &mut cameras.inputs {
            if initial_frames.contains_key(&camera.input) {
                continue;
            }
            let transfer = {
                let source = camera.source.as_mut().expect("camera source is configured");
                let transfer = source.try_receive();
                let mut state = camera
                    .supervisor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                update_camera_worker_snapshot_inner(&mut state.snapshot, source);
                transfer
            };
            let transfer = match transfer {
                Ok(transfer) => transfer,
                Err(error) => {
                    let detail = format!(
                        "camera input {} initial-frame acquisition failed: {error}",
                        camera.input
                    );
                    return Err(camera_startup_failure(&mut cameras, detail));
                }
            };
            match transfer {
                Some(MediaTransfer::Live(frame)) => {
                    camera.preflight_depth = 1;
                    initial_frames.insert(camera.input, frame);
                }
                Some(MediaTransfer::Fallback { .. }) => {
                    let detail = format!(
                        "camera input {} returned fallback media during startup",
                        camera.input
                    );
                    return Err(camera_startup_failure(&mut cameras, detail));
                }
                None => {}
            }
        }
        if initial_frames.len() == cameras.inputs.len() {
            break;
        }
        if Instant::now() >= deadline {
            let detail = format!(
                "camera initial-frame acquisition timed out with {}/{} sources ready",
                initial_frames.len(),
                cameras.inputs.len()
            );
            return Err(camera_startup_failure(&mut cameras, detail));
        }
        thread::sleep(Duration::from_millis(2));
    }
    let sources = cameras
        .inputs
        .iter()
        .map(|camera| NativeResolvedSource::LiveFrame {
            input: camera.input,
            frame: initial_frames
                .remove(&camera.input)
                .expect("all camera inputs produced an initial frame"),
        })
        .collect();
    cameras.start_workers()?;
    Ok((sources, cameras))
}

#[cfg(all(feature = "native-media", target_os = "macos"))]
fn camera_startup_failure(cameras: &mut NativeCameraInputs, mut detail: String) -> Box<dyn Error> {
    cameras.discard_preflight_frames();
    if cameras
        .cleanup_startup_sources(CAMERA_STARTUP_CLEANUP_TIMEOUT)
        .is_err()
    {
        detail.push_str("; camera startup cleanup also failed");
    }
    Box::new(AppFailure(detail))
}

#[cfg(feature = "native-media")]
fn resolve_native_media_source(
    store: &ProjectStore,
    input: InputId,
    asset_uri: &str,
    clock_domain: MediaClockDomainId,
    output_dimensions: fm_types::VideoDimensions,
) -> AppResult<NativeResolvedSource> {
    // A media file missing from the bundle starts its input offline
    // (`source_missing`) as a media-file input, instead of stopping the
    // daemon: the file worker reports the fault and retries the file when
    // the input reopens (IN-026). A broken bundle still stops startup.
    let missing = || {
        let key = asset_uri.strip_prefix(fm_model::MEDIA_ASSET_URI_PREFIX)?;
        fm_model::validate_media_asset_uri(asset_uri).ok()?;
        Some(NativeResolvedSource::LocalVideo {
            input,
            path: store.assets_root().join(key),
        })
    };
    let path = match store.resolve_asset_uri(asset_uri) {
        Ok(path) => path,
        Err(fm_persistence::AssetResolveError::AssetUnavailable(error))
            if error.kind() == std::io::ErrorKind::NotFound
                && let Some(source) = missing() =>
        {
            return Ok(source);
        }
        Err(error) => {
            return Err(AppFailure(format!(
                "native media asset resolution failed for input {input}: {error}"
            ))
            .into());
        }
    };
    let mut file = File::open(&path).map_err(|error| {
        AppFailure(format!(
            "native media read failed for input {input}: {:?}",
            error.kind()
        ))
    })?;
    let mut prefix = Vec::with_capacity(8);
    Read::by_ref(&mut file)
        .take(8)
        .read_to_end(&mut prefix)
        .map_err(|error| {
            AppFailure(format!(
                "native media read failed for input {input}: {:?}",
                error.kind()
            ))
        })?;
    if sniff_still_format(&prefix).is_err() {
        return Ok(NativeResolvedSource::LocalVideo { input, path });
    }

    let limits = still_limits(output_dimensions)?;
    let mut encoded = prefix;
    let remaining = limits
        .max_encoded_bytes
        .saturating_add(1)
        .saturating_sub(encoded.len());
    file.take(u64::try_from(remaining).unwrap_or(u64::MAX))
        .read_to_end(&mut encoded)
        .map_err(|error| {
            AppFailure(format!(
                "native still read failed for input {input}: {:?}",
                error.kind()
            ))
        })?;
    let decoded =
        decode_still(&encoded, retained_frame_timing(clock_domain)?, limits).map_err(|error| {
            AppFailure(format!(
                "native still decode failed for input {input}: {error}"
            ))
        })?;
    Ok(NativeResolvedSource::RetainedFrame {
        input,
        frame: decoded.frame,
    })
}

#[cfg(feature = "native-media")]
fn still_limits(dimensions: fm_types::VideoDimensions) -> AppResult<StillDecodeLimits> {
    let maximum_axis = dimensions.width().max(dimensions.height());
    let decoded_bytes = usize::try_from(dimensions.width())
        .ok()
        .and_then(|width| width.checked_mul(4))
        .and_then(|stride| {
            stride.checked_mul(usize::try_from(dimensions.height()).unwrap_or(usize::MAX))
        })
        .ok_or_else(|| AppFailure("native still output dimensions overflow".into()))?;
    Ok(StillDecodeLimits {
        max_width: maximum_axis,
        max_height: maximum_axis,
        max_decoded_rgba_bytes: decoded_bytes,
        ..StillDecodeLimits::default()
    })
}

#[cfg(feature = "native-media")]
fn retained_frame_timing(clock_domain: MediaClockDomainId) -> AppResult<MediaTiming> {
    let time_base = fm_types::TimeBase::new(1, 1_000_000_000)
        .map_err(|error| AppFailure(format!("native still time base is invalid: {error}")))?;
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(0), time_base),
        NormalizedTimestamp::from_nanos(0),
        NormalizedDuration::from_nanos(1)
            .map_err(|error| AppFailure(format!("native still duration is invalid: {error}")))?,
        clock_domain,
        SequenceNumber::new(0),
    )
    .map_err(|error| AppFailure(format!("native still timing is invalid: {error}")).into())
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
    MediaClockDomainId::new(NonZeroU128::new(1).expect("one is nonzero"))
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
    app_capture_failure: bool,
    failure: &str,
    video_bitrate_kbps: Option<u32>,
    audio_bitrate_kbps: Option<u32>,
) -> String {
    format!(
        "FREEMIXD_RECORDER\tv=1\tslot={slot}\tstate={:?}\toutcome={:?}\taccepted_pairs={}\tcompleted_pairs={}\toutput_bytes={}\twav_bytes={}\tvideo_bitrate_kbps={}\taudio_bitrate_kbps={}\tduration_ms={duration_ms}\tfree_bytes={free_bytes}\toutput_finalization={:?}\tcleanup={:?}\tapp_capture_failure={app_capture_failure}\tfailure={}",
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
        diagnostic_field(failure),
    )
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

const MAX_RECORD_SEGMENT_SECONDS: u32 = 86_400;
/// CLI envelope of `--record-bitrate-kbps`/`--record-bitrate-kbps-2`, matching
/// `fm_codec_ffmpeg::record::{MIN,MAX}_VIDEO_BITRATE_KBPS`. The daemon must
/// accept these options in builds without the optional codec dependency, so
/// the bounds are restated here rather than imported.
const MIN_RECORD_VIDEO_BITRATE_KBPS: u32 = 1_000;
const MAX_RECORD_VIDEO_BITRATE_KBPS: u32 = 100_000;
/// CLI envelope of `--record-audio-bitrate-kbps`/`--record-audio-bitrate-kbps-2`,
/// matching `fm_codec_ffmpeg::record::{MIN,MAX}_AUDIO_BITRATE_KBPS`.
const MIN_RECORD_AUDIO_BITRATE_KBPS: u32 = 8;
const MAX_RECORD_AUDIO_BITRATE_KBPS: u32 = 1_024;

// The parse result is built once and matched once; keeping the Serve fields
// inline beats indirection for a value that never lives in a collection.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Serve {
        project: PathBuf,
        listen: SocketAddr,
        web_listen: Option<SocketAddr>,
        status_listen: Option<SocketAddr>,
        hls_listen: Option<SocketAddr>,
        once: bool,
        native_media: bool,
        /// True when display outputs open real borderless windows.
        fullscreen_program: bool,
        /// One or two display outputs, from `--display-output` or the
        /// `--fullscreen-program [--fullscreen-display N]` shorthand. They
        /// replace the project's display-output slots on load.
        display_outputs: Vec<DisplayOutputConfig>,
        /// Diagnostic inventory file presenting display outputs headlessly.
        display_inventory: Option<PathBuf>,
        /// `--display-presenter-if-available`: a presenter that cannot open
        /// (no window system or GPU) falls back to headless native serving
        /// with every slot waiting for a presenter.
        presenter_fallback: bool,
        camera_helper: Option<PathBuf>,
        /// `--diagnostic-capture-inventory`: capture inputs acquire the fake
        /// devices listed in this file instead of real ones (IN-001).
        capture_inventory: Option<PathBuf>,
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
    },
    Help,
    Version,
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

/// Resolves `--display-output`, the `--fullscreen-program
/// [--fullscreen-display N]` shorthand, `--display-presenter`, and
/// `--diagnostic-display-inventory` into a validated list of at most two
/// display outputs.
fn resolve_display_outputs(
    native_media: bool,
    fullscreen_program: bool,
    fullscreen_display: Option<usize>,
    display_presenter: bool,
    mut display_outputs: Vec<DisplayOutputConfig>,
    display_inventory: Option<&Path>,
) -> AppResult<Vec<DisplayOutputConfig>> {
    if !display_outputs.is_empty() && !native_media {
        return Err(AppFailure("--display-output requires --native-media".into()).into());
    }
    if display_presenter && !native_media {
        return Err(AppFailure("--display-presenter requires --native-media".into()).into());
    }
    if display_inventory.is_some() && !native_media {
        return Err(
            AppFailure("--diagnostic-display-inventory requires --native-media".into()).into(),
        );
    }
    if !display_outputs.is_empty() && (fullscreen_program || fullscreen_display.is_some()) {
        return Err(AppFailure(
            "--display-output replaces --fullscreen-program and --fullscreen-display; \
             use display=index:<n> instead"
                .into(),
        )
        .into());
    }
    if fullscreen_program {
        display_outputs.push(DisplayOutputConfig::program_on(DisplaySelector::Index(
            fullscreen_display.unwrap_or(0),
        )));
    }
    if display_inventory.is_some() && (fullscreen_program || display_presenter) {
        return Err(AppFailure(
            "--diagnostic-display-inventory presents headlessly and cannot be combined \
             with --fullscreen-program or --display-presenter"
                .into(),
        )
        .into());
    }
    if display_outputs.is_empty() {
        return Ok(display_outputs);
    }
    DisplayOutputSet::new(display_outputs)
        .map(|set| set.outputs().to_vec())
        .map_err(|error| AppFailure(format!("invalid display outputs: {error}")).into())
}

#[allow(clippy::too_many_lines)]
fn parse_args(arguments: impl IntoIterator<Item = String>) -> AppResult<Command> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Err(AppFailure("a command is required".into()).into());
    };
    match command.as_str() {
        "serve" => {
            let project = arguments
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| AppFailure("missing project path".into()))?;
            let mut listen = DEFAULT_LISTEN.parse()?;
            let mut listen_set = false;
            let mut web_listen: Option<SocketAddr> = None;
            let mut status_listen: Option<SocketAddr> = None;
            let mut hls_listen: Option<SocketAddr> = None;
            let mut once = false;
            let mut native_media = false;
            let mut fullscreen_program = false;
            let mut fullscreen_display = None;
            let mut display_outputs: Vec<DisplayOutputConfig> = Vec::new();
            let mut display_inventory: Option<PathBuf> = None;
            let mut capture_inventory: Option<PathBuf> = None;
            let mut display_presenter = false;
            let mut presenter_if_available = false;
            let mut camera_helper = None;
            let mut record_program = None;
            let mut record_output = None;
            let mut record_program_2 = None;
            let mut record_output_2 = None;
            let mut record_wav = false;
            let mut record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS] =
                [None; RECORDER_SLOTS];
            let mut record_bitrate_kbps = None;
            let mut record_bitrate_kbps_2 = None;
            let mut record_audio_bitrate_kbps = None;
            let mut record_audio_bitrate_kbps_2 = None;
            let mut record_fallback_dir: Option<PathBuf> = None;
            let mut diagnostic_stop_after = None;
            let mut recover_to_checkpoint = false;
            while let Some(option) = arguments.next() {
                if let Some(value) = option.strip_prefix("--record-program=") {
                    if record_program.is_some() {
                        return Err(AppFailure("duplicate option `--record-program`".into()).into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure("missing value for --record-program".into()).into());
                    }
                    record_program = Some(PathBuf::from(value));
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-output=") {
                    if record_output.is_some() {
                        return Err(AppFailure("duplicate option `--record-output`".into()).into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure("missing value for --record-output".into()).into());
                    }
                    record_output = Some(parse_record_output_id("--record-output", value)?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-fallback-dir=") {
                    if record_fallback_dir.is_some() {
                        return Err(
                            AppFailure("duplicate option `--record-fallback-dir`".into()).into(),
                        );
                    }
                    if value.is_empty() {
                        return Err(
                            AppFailure("missing value for --record-fallback-dir".into()).into()
                        );
                    }
                    record_fallback_dir = Some(PathBuf::from(value));
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-program-2=") {
                    if record_program_2.is_some() {
                        return Err(
                            AppFailure("duplicate option `--record-program-2`".into()).into()
                        );
                    }
                    if value.is_empty() {
                        return Err(
                            AppFailure("missing value for --record-program-2".into()).into()
                        );
                    }
                    record_program_2 = Some(PathBuf::from(value));
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-output-2=") {
                    if record_output_2.is_some() {
                        return Err(
                            AppFailure("duplicate option `--record-output-2`".into()).into()
                        );
                    }
                    if value.is_empty() {
                        return Err(AppFailure("missing value for --record-output-2".into()).into());
                    }
                    record_output_2 = Some(parse_record_output_id("--record-output-2", value)?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-segment-seconds=") {
                    if record_segment_seconds[0].is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-segment-seconds`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure(
                            "missing value for --record-segment-seconds".into(),
                        )
                        .into());
                    }
                    record_segment_seconds[0] = Some(parse_record_segment_seconds(
                        "--record-segment-seconds",
                        value,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-segment-seconds-2=") {
                    if record_segment_seconds[1].is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-segment-seconds-2`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure(
                            "missing value for --record-segment-seconds-2".into(),
                        )
                        .into());
                    }
                    record_segment_seconds[1] = Some(parse_record_segment_seconds(
                        "--record-segment-seconds-2",
                        value,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-bitrate-kbps=") {
                    if record_bitrate_kbps.is_some() {
                        return Err(
                            AppFailure("duplicate option `--record-bitrate-kbps`".into()).into(),
                        );
                    }
                    if value.is_empty() {
                        return Err(
                            AppFailure("missing value for --record-bitrate-kbps".into()).into()
                        );
                    }
                    record_bitrate_kbps = Some(parse_record_bitrate_kbps(
                        "--record-bitrate-kbps",
                        value,
                        MIN_RECORD_VIDEO_BITRATE_KBPS,
                        MAX_RECORD_VIDEO_BITRATE_KBPS,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-bitrate-kbps-2=") {
                    if record_bitrate_kbps_2.is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-bitrate-kbps-2`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(
                            AppFailure("missing value for --record-bitrate-kbps-2".into()).into(),
                        );
                    }
                    record_bitrate_kbps_2 = Some(parse_record_bitrate_kbps(
                        "--record-bitrate-kbps-2",
                        value,
                        MIN_RECORD_VIDEO_BITRATE_KBPS,
                        MAX_RECORD_VIDEO_BITRATE_KBPS,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-audio-bitrate-kbps=") {
                    if record_audio_bitrate_kbps.is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-audio-bitrate-kbps`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure(
                            "missing value for --record-audio-bitrate-kbps".into(),
                        )
                        .into());
                    }
                    record_audio_bitrate_kbps = Some(parse_record_bitrate_kbps(
                        "--record-audio-bitrate-kbps",
                        value,
                        MIN_RECORD_AUDIO_BITRATE_KBPS,
                        MAX_RECORD_AUDIO_BITRATE_KBPS,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-audio-bitrate-kbps-2=") {
                    if record_audio_bitrate_kbps_2.is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-audio-bitrate-kbps-2`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure(
                            "missing value for --record-audio-bitrate-kbps-2".into(),
                        )
                        .into());
                    }
                    record_audio_bitrate_kbps_2 = Some(parse_record_bitrate_kbps(
                        "--record-audio-bitrate-kbps-2",
                        value,
                        MIN_RECORD_AUDIO_BITRATE_KBPS,
                        MAX_RECORD_AUDIO_BITRATE_KBPS,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--camera-helper=") {
                    if camera_helper.is_some() {
                        return Err(AppFailure("duplicate option `--camera-helper`".into()).into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure("missing value for --camera-helper".into()).into());
                    }
                    camera_helper = Some(PathBuf::from(value));
                    continue;
                }
                if let Some(value) = option.strip_prefix("--diagnostic-stop-after=") {
                    if diagnostic_stop_after.is_some() {
                        return Err(AppFailure(
                            "duplicate option `--diagnostic-stop-after`".into(),
                        )
                        .into());
                    }
                    diagnostic_stop_after = Some(parse_diagnostic_duration(value)?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--web-listen=") {
                    if web_listen.is_some() {
                        return Err(AppFailure("duplicate option `--web-listen`".into()).into());
                    }
                    web_listen = Some(value.parse().map_err(|error| {
                        AppFailure(format!("invalid web listen address: {error}"))
                    })?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--status-listen=") {
                    if status_listen.is_some() {
                        return Err(AppFailure("duplicate option `--status-listen`".into()).into());
                    }
                    status_listen = Some(value.parse().map_err(|error| {
                        AppFailure(format!("invalid status listen address: {error}"))
                    })?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--hls-listen=") {
                    if hls_listen.is_some() {
                        return Err(AppFailure("duplicate option `--hls-listen`".into()).into());
                    }
                    hls_listen = Some(value.parse().map_err(|error| {
                        AppFailure(format!("invalid hls listen address: {error}"))
                    })?);
                    continue;
                }
                match option.as_str() {
                    "--listen" if listen_set => {
                        return Err(AppFailure("duplicate option `--listen`".into()).into());
                    }
                    "--listen" => {
                        listen_set = true;
                        listen = arguments
                            .next()
                            .ok_or_else(|| AppFailure("missing value for --listen".into()))?
                            .parse()
                            .map_err(|error| {
                                AppFailure(format!("invalid listen address: {error}"))
                            })?;
                    }
                    "--web-listen" => {
                        if web_listen.is_some() {
                            return Err(AppFailure("duplicate option `--web-listen`".into()).into());
                        }
                        let value = arguments
                            .next()
                            .ok_or_else(|| AppFailure("missing value for --web-listen".into()))?;
                        web_listen = Some(value.parse().map_err(|error| {
                            AppFailure(format!("invalid web listen address: {error}"))
                        })?);
                    }
                    "--status-listen" => {
                        if status_listen.is_some() {
                            return Err(
                                AppFailure("duplicate option `--status-listen`".into()).into()
                            );
                        }
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --status-listen".into())
                        })?;
                        status_listen = Some(value.parse().map_err(|error| {
                            AppFailure(format!("invalid status listen address: {error}"))
                        })?);
                    }
                    "--hls-listen" => {
                        if hls_listen.is_some() {
                            return Err(AppFailure("duplicate option `--hls-listen`".into()).into());
                        }
                        let value = arguments
                            .next()
                            .ok_or_else(|| AppFailure("missing value for --hls-listen".into()))?;
                        hls_listen = Some(value.parse().map_err(|error| {
                            AppFailure(format!("invalid hls listen address: {error}"))
                        })?);
                    }
                    "--once" => once = true,
                    "--native-media" if native_media => {
                        return Err(AppFailure("duplicate option `--native-media`".into()).into());
                    }
                    "--native-media" => native_media = true,
                    "--recover-to-checkpoint" if recover_to_checkpoint => {
                        return Err(AppFailure(
                            "duplicate option `--recover-to-checkpoint`".into(),
                        )
                        .into());
                    }
                    "--recover-to-checkpoint" => recover_to_checkpoint = true,
                    "--fullscreen-program" if fullscreen_program => {
                        return Err(
                            AppFailure("duplicate option `--fullscreen-program`".into()).into()
                        );
                    }
                    "--fullscreen-program" => fullscreen_program = true,
                    "--display-presenter" if display_presenter => {
                        return Err(
                            AppFailure("duplicate option `--display-presenter`".into()).into()
                        );
                    }
                    "--display-presenter" => display_presenter = true,
                    "--display-presenter-if-available" if presenter_if_available => {
                        return Err(AppFailure(
                            "duplicate option `--display-presenter-if-available`".into(),
                        )
                        .into());
                    }
                    "--display-presenter-if-available" => presenter_if_available = true,
                    "--fullscreen-display" if fullscreen_display.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--fullscreen-display`".into()).into()
                        );
                    }
                    "--fullscreen-display" => {
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --fullscreen-display".into())
                        })?;
                        fullscreen_display = Some(value.parse::<usize>().map_err(|_| {
                            AppFailure(format!(
                                "invalid fullscreen display index `{value}`; expected a zero-based non-negative integer"
                            ))
                        })?);
                    }
                    "--camera-helper" if camera_helper.is_some() => {
                        return Err(AppFailure("duplicate option `--camera-helper`".into()).into());
                    }
                    "--camera-helper" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --camera-helper".into())
                            })?;
                        camera_helper = Some(PathBuf::from(value));
                    }
                    "--record-program" if record_program.is_some() => {
                        return Err(AppFailure("duplicate option `--record-program`".into()).into());
                    }
                    "--record-program" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-program".into())
                            })?;
                        record_program = Some(PathBuf::from(value));
                    }
                    "--record-output" if record_output.is_some() => {
                        return Err(AppFailure("duplicate option `--record-output`".into()).into());
                    }
                    "--record-output" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-output".into())
                            })?;
                        record_output = Some(parse_record_output_id("--record-output", &value)?);
                    }
                    "--record-program-2" if record_program_2.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--record-program-2`".into()).into()
                        );
                    }
                    "--record-program-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-program-2".into())
                            })?;
                        record_program_2 = Some(PathBuf::from(value));
                    }
                    "--record-output-2" if record_output_2.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--record-output-2`".into()).into()
                        );
                    }
                    "--record-output-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-output-2".into())
                            })?;
                        record_output_2 =
                            Some(parse_record_output_id("--record-output-2", &value)?);
                    }
                    "--record-fallback-dir" if record_fallback_dir.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--record-fallback-dir`".into()).into(),
                        );
                    }
                    "--record-fallback-dir" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-fallback-dir".into())
                            })?;
                        record_fallback_dir = Some(PathBuf::from(value));
                    }
                    "--record-wav" if record_wav => {
                        return Err(AppFailure("duplicate option `--record-wav`".into()).into());
                    }
                    "--record-wav" => record_wav = true,
                    "--record-segment-seconds" if record_segment_seconds[0].is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-segment-seconds`".into(),
                        )
                        .into());
                    }
                    "--record-segment-seconds" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-segment-seconds".into())
                            })?;
                        record_segment_seconds[0] = Some(parse_record_segment_seconds(
                            "--record-segment-seconds",
                            &value,
                        )?);
                    }
                    "--record-segment-seconds-2" if record_segment_seconds[1].is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-segment-seconds-2`".into(),
                        )
                        .into());
                    }
                    "--record-segment-seconds-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-segment-seconds-2".into())
                            })?;
                        record_segment_seconds[1] = Some(parse_record_segment_seconds(
                            "--record-segment-seconds-2",
                            &value,
                        )?);
                    }
                    "--record-bitrate-kbps" if record_bitrate_kbps.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--record-bitrate-kbps`".into()).into(),
                        );
                    }
                    "--record-bitrate-kbps" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-bitrate-kbps".into())
                            })?;
                        record_bitrate_kbps = Some(parse_record_bitrate_kbps(
                            "--record-bitrate-kbps",
                            &value,
                            MIN_RECORD_VIDEO_BITRATE_KBPS,
                            MAX_RECORD_VIDEO_BITRATE_KBPS,
                        )?);
                    }
                    "--record-bitrate-kbps-2" if record_bitrate_kbps_2.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-bitrate-kbps-2`".into(),
                        )
                        .into());
                    }
                    "--record-bitrate-kbps-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-bitrate-kbps-2".into())
                            })?;
                        record_bitrate_kbps_2 = Some(parse_record_bitrate_kbps(
                            "--record-bitrate-kbps-2",
                            &value,
                            MIN_RECORD_VIDEO_BITRATE_KBPS,
                            MAX_RECORD_VIDEO_BITRATE_KBPS,
                        )?);
                    }
                    "--record-audio-bitrate-kbps" if record_audio_bitrate_kbps.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-audio-bitrate-kbps`".into(),
                        )
                        .into());
                    }
                    "--record-audio-bitrate-kbps" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-audio-bitrate-kbps".into())
                            })?;
                        record_audio_bitrate_kbps = Some(parse_record_bitrate_kbps(
                            "--record-audio-bitrate-kbps",
                            &value,
                            MIN_RECORD_AUDIO_BITRATE_KBPS,
                            MAX_RECORD_AUDIO_BITRATE_KBPS,
                        )?);
                    }
                    "--record-audio-bitrate-kbps-2" if record_audio_bitrate_kbps_2.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-audio-bitrate-kbps-2`".into(),
                        )
                        .into());
                    }
                    "--record-audio-bitrate-kbps-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-audio-bitrate-kbps-2".into())
                            })?;
                        record_audio_bitrate_kbps_2 = Some(parse_record_bitrate_kbps(
                            "--record-audio-bitrate-kbps-2",
                            &value,
                            MIN_RECORD_AUDIO_BITRATE_KBPS,
                            MAX_RECORD_AUDIO_BITRATE_KBPS,
                        )?);
                    }
                    "--display-output" => {
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --display-output".into())
                        })?;
                        display_outputs.push(value.parse::<DisplayOutputConfig>().map_err(
                            |error| AppFailure(format!("invalid --display-output: {error}")),
                        )?);
                    }
                    "--diagnostic-display-inventory" if display_inventory.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--diagnostic-display-inventory`".into(),
                        )
                        .into());
                    }
                    "--diagnostic-display-inventory" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.is_empty() && !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure(
                                    "missing value for --diagnostic-display-inventory".into(),
                                )
                            })?;
                        display_inventory = Some(PathBuf::from(value));
                    }
                    "--diagnostic-capture-inventory" if capture_inventory.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--diagnostic-capture-inventory`".into(),
                        )
                        .into());
                    }
                    "--diagnostic-capture-inventory" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.is_empty() && !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure(
                                    "missing value for --diagnostic-capture-inventory".into(),
                                )
                            })?;
                        capture_inventory = Some(PathBuf::from(value));
                    }
                    "--diagnostic-stop-after" if diagnostic_stop_after.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--diagnostic-stop-after`".into(),
                        )
                        .into());
                    }
                    "--diagnostic-stop-after" => {
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --diagnostic-stop-after".into())
                        })?;
                        diagnostic_stop_after = Some(parse_diagnostic_duration(&value)?);
                    }
                    _ => return Err(AppFailure(format!("unknown option `{option}`")).into()),
                }
            }
            // A build with display surfaces opens the presenter (natively)
            // unless an option the windowed daemon does not serve is given;
            // otherwise, and when the presenter cannot open, the daemon
            // serves exactly as without the option (its slots wait with
            // `no-presenter`).
            let presenter_fallback = presenter_if_available
                && cfg!(feature = "display-surface")
                && !display_presenter
                && display_outputs.is_empty()
                && !fullscreen_program
                && display_inventory.is_none()
                && web_listen.is_none()
                && status_listen.is_none()
                && hls_listen.is_none()
                && record_fallback_dir.is_none()
                && diagnostic_stop_after.is_none()
                && !recover_to_checkpoint;
            if presenter_fallback {
                display_presenter = true;
            }
            if fullscreen_program && !native_media {
                return Err(
                    AppFailure("--fullscreen-program requires --native-media".into()).into(),
                );
            }
            if fullscreen_display.is_some() && !fullscreen_program {
                return Err(AppFailure(
                    "--fullscreen-display requires --fullscreen-program".into(),
                )
                .into());
            }
            let display_outputs = resolve_display_outputs(
                native_media || presenter_fallback,
                fullscreen_program,
                fullscreen_display,
                display_presenter,
                display_outputs,
                display_inventory.as_deref(),
            )?;
            // Real borderless windows open for `--display-output` and
            // `--display-presenter` unless the outputs are presented to the
            // diagnostic inventory host.
            let fullscreen_program =
                (!display_outputs.is_empty() || display_presenter) && display_inventory.is_none();
            if record_program.is_some() && !native_media {
                return Err(AppFailure("--record-program requires --native-media".into()).into());
            }
            if record_program_2.is_some() && !native_media {
                return Err(AppFailure("--record-program-2 requires --native-media".into()).into());
            }
            if record_output.is_some() && record_program.is_none() {
                return Err(AppFailure("--record-output requires --record-program".into()).into());
            }
            if record_output_2.is_some() && record_program_2.is_none() {
                return Err(
                    AppFailure("--record-output-2 requires --record-program-2".into()).into(),
                );
            }
            if record_bitrate_kbps.is_some() && record_program.is_none() {
                return Err(
                    AppFailure("--record-bitrate-kbps requires --record-program".into()).into(),
                );
            }
            if record_bitrate_kbps_2.is_some() && record_program_2.is_none() {
                return Err(AppFailure(
                    "--record-bitrate-kbps-2 requires --record-program-2".into(),
                )
                .into());
            }
            if record_audio_bitrate_kbps.is_some() && record_program.is_none() {
                return Err(AppFailure(
                    "--record-audio-bitrate-kbps requires --record-program".into(),
                )
                .into());
            }
            if record_audio_bitrate_kbps_2.is_some() && record_program_2.is_none() {
                return Err(AppFailure(
                    "--record-audio-bitrate-kbps-2 requires --record-program-2".into(),
                )
                .into());
            }
            if record_wav && record_program.is_none() && record_program_2.is_none() {
                return Err(AppFailure(
                    "--record-wav requires --record-program or --record-program-2".into(),
                )
                .into());
            }
            if record_fallback_dir.is_some()
                && record_program.is_none()
                && record_program_2.is_none()
            {
                return Err(AppFailure(
                    "--record-fallback-dir requires --record-program or --record-program-2".into(),
                )
                .into());
            }
            if record_fallback_dir.is_some() && fullscreen_program {
                return Err(AppFailure(
                    "--record-fallback-dir currently supports headless native mode only".into(),
                )
                .into());
            }
            if record_segment_seconds[0].is_some() && record_program.is_none() {
                return Err(AppFailure(
                    "--record-segment-seconds requires --record-program".into(),
                )
                .into());
            }
            if record_segment_seconds[1].is_some() && record_program_2.is_none() {
                return Err(AppFailure(
                    "--record-segment-seconds-2 requires --record-program-2".into(),
                )
                .into());
            }
            if camera_helper.is_some() && !native_media {
                return Err(AppFailure("--camera-helper requires --native-media".into()).into());
            }
            if recover_to_checkpoint && fullscreen_program {
                return Err(AppFailure(
                    "--recover-to-checkpoint cannot be combined with --fullscreen-program; \
                     abandon the batches with one headless start, then start fullscreen"
                        .into(),
                )
                .into());
            }
            if diagnostic_stop_after.is_some() && once {
                return Err(AppFailure(
                    "--diagnostic-stop-after cannot be combined with --once".into(),
                )
                .into());
            }
            if diagnostic_stop_after.is_some() && fullscreen_program {
                return Err(AppFailure(
                    "--diagnostic-stop-after currently supports headless native mode only".into(),
                )
                .into());
            }
            if let Some(address) = web_listen {
                if !address.ip().is_loopback() {
                    return Err(
                        AppFailure("--web-listen must use a loopback address".into()).into(),
                    );
                }
                if once {
                    return Err(
                        AppFailure("--web-listen cannot be combined with --once".into()).into(),
                    );
                }
                if native_media {
                    return Err(AppFailure(
                        "--web-listen cannot be combined with --native-media".into(),
                    )
                    .into());
                }
            }
            if let Some(address) = status_listen {
                if !address.ip().is_loopback() {
                    return Err(
                        AppFailure("--status-listen must use a loopback address".into()).into(),
                    );
                }
                if fullscreen_program {
                    return Err(AppFailure(
                        "--status-listen cannot be combined with --fullscreen-program".into(),
                    )
                    .into());
                }
            }
            if let Some(address) = hls_listen {
                if !address.ip().is_loopback() {
                    return Err(
                        AppFailure("--hls-listen must use a loopback address".into()).into(),
                    );
                }
                if !native_media {
                    return Err(AppFailure("--hls-listen requires --native-media".into()).into());
                }
                if fullscreen_program {
                    return Err(AppFailure(
                        "--hls-listen cannot be combined with --fullscreen-program".into(),
                    )
                    .into());
                }
            }
            if capture_inventory.is_some() && !native_media {
                return Err(AppFailure(
                    "--diagnostic-capture-inventory requires --native-media".into(),
                )
                .into());
            }
            Ok(Command::Serve {
                project,
                listen,
                web_listen,
                status_listen,
                hls_listen,
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
            })
        }
        "help" | "--help" | "-h" => {
            reject_extra(arguments)?;
            Ok(Command::Help)
        }
        "--version" | "version" => {
            reject_extra(arguments)?;
            Ok(Command::Version)
        }
        _ => Err(AppFailure(format!("unknown command `{command}`")).into()),
    }
}

/// Parses one recorder segment-duration option as an integer in
/// `1..=MAX_RECORD_SEGMENT_SECONDS`.
///
/// # Errors
///
/// Returns an [`AppFailure`] naming `option` when `value` is not an integer
/// in `1..=86400`.
fn parse_record_segment_seconds(option: &str, value: &str) -> AppResult<NonZeroU32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|&seconds| seconds <= MAX_RECORD_SEGMENT_SECONDS)
        .and_then(NonZeroU32::new)
        .ok_or_else(|| {
            AppFailure(format!(
                "invalid value for {option} `{value}`; expected an integer in 1..=86400"
            ))
            .into()
        })
}

/// Parses one recorder bitrate option as an integer in `min..=max` kbps.
///
/// # Errors
///
/// Returns an [`AppFailure`] when `value` is not an integer in the inclusive
/// range.
fn parse_record_bitrate_kbps(option: &str, value: &str, min: u32, max: u32) -> AppResult<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|&kbps| (min..=max).contains(&kbps))
        .ok_or_else(|| {
            AppFailure(format!(
                "invalid value for {option} `{value}`; expected an integer in {min}..={max}"
            ))
            .into()
        })
}

/// Parses a nonzero [`fm_types::OutputId`] for `--record-output` or
/// `--record-output-2`.
///
/// # Errors
///
/// Returns an [`AppFailure`] when `value` is not a positive `u128`.
fn parse_record_output_id(flag: &str, value: &str) -> AppResult<fm_types::OutputId> {
    let parsed = value
        .parse::<u128>()
        .ok()
        .and_then(NonZeroU128::new)
        .ok_or_else(|| {
            AppFailure(format!(
                "invalid value for {flag} `{value}`; expected a positive integer output id"
            ))
        })?;
    Ok(fm_types::OutputId::new(parsed))
}

fn parse_diagnostic_duration(value: &str) -> AppResult<Duration> {
    let (amount, unit) = if let Some(amount) = value.strip_suffix("ms") {
        (amount, "ms")
    } else if let Some(amount) = value.strip_suffix('s') {
        (amount, "s")
    } else if let Some(amount) = value.strip_suffix('m') {
        (amount, "m")
    } else if let Some(amount) = value.strip_suffix('h') {
        (amount, "h")
    } else {
        return Err(AppFailure(format!(
            "invalid diagnostic duration `{value}`; expected a positive integer followed by ms, s, m, or h"
        ))
        .into());
    };
    let amount = amount.parse::<u64>().map_err(|_| {
        AppFailure(format!(
            "invalid diagnostic duration `{value}`; expected a positive integer followed by ms, s, m, or h"
        ))
    })?;
    if amount == 0 {
        return Err(AppFailure("diagnostic duration must be nonzero".into()).into());
    }
    let duration = match unit {
        "ms" => Duration::from_millis(amount),
        "s" => Duration::from_secs(amount),
        "m" => Duration::from_secs(
            amount
                .checked_mul(60)
                .ok_or_else(|| AppFailure("diagnostic duration overflow".into()))?,
        ),
        "h" => Duration::from_secs(
            amount
                .checked_mul(3_600)
                .ok_or_else(|| AppFailure("diagnostic duration overflow".into()))?,
        ),
        _ => unreachable!("duration units are matched above"),
    };
    if duration > Duration::from_hours(24) {
        return Err(AppFailure("diagnostic duration cannot exceed 24h".into()).into());
    }
    Ok(duration)
}

fn reject_extra(mut arguments: impl Iterator<Item = String>) -> AppResult<()> {
    if let Some(argument) = arguments.next() {
        Err(AppFailure(format!("unexpected argument `{argument}`")).into())
    } else {
        Ok(())
    }
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
            status_listen,
            hls_listen,
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
                    status_listen,
                    hls_listen,
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
#[derive(Clone, Copy)]
struct ServeListeners {
    listen: SocketAddr,
    web_listen: Option<SocketAddr>,
    status_listen: Option<SocketAddr>,
    hls_listen: Option<SocketAddr>,
}

#[cfg(feature = "display-surface")]
#[allow(clippy::too_many_arguments)]
fn serve_program_worker(
    path: &Path,
    listen: SocketAddr,
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
            web_listen: None,
            status_listen: None,
            hls_listen: None,
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

/// Validates that every configured recorder slot's output selection names an
/// output declared by `project`.
///
/// # Errors
///
/// Returns an [`AppFailure`] naming the CLI flag and the unknown
/// [`fm_types::OutputId`] when a slot selects an output absent from `project`.
fn validate_recording_output_selections(
    project: &fm_model::Project,
    record_output: Option<fm_types::OutputId>,
    record_output_2: Option<fm_types::OutputId>,
) -> AppResult<()> {
    for (flag, selected) in [
        ("--record-output", record_output),
        ("--record-output-2", record_output_2),
    ] {
        let Some(output_id) = selected else {
            continue;
        };
        if !project
            .outputs()
            .iter()
            .any(|output| output.id == output_id)
        {
            return Err(
                AppFailure(format!("{flag} selects unknown project output {output_id}")).into(),
            );
        }
    }
    Ok(())
}

/// Parent directory of a configured recording output.
///
/// A bare file name resolves to the working directory the recorder writes to,
/// mirroring how [`next_record_segment_path`] keeps derived segment paths
/// relative.
fn recording_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Preflights every configured recording output's parent directory, refusing
/// startup when one has too little free space.
///
/// Every configured recorder slot is a configured show destination, so each
/// parent is held to the same floor; the whole-run sampler later takes the
/// minimum across these parents and every local HLS output.
///
/// # Errors
///
/// Returns a sanitized [`AppFailure`] naming the flag when any configured
/// parent cannot be queried or falls below
/// [`disk::RECORDING_MINIMUM_FREE_BYTES`]. The message carries free-space
/// counts or an I/O kind only; it never renders the operator's path.
fn preflight_recording_outputs<'a>(
    outputs: impl IntoIterator<Item = (&'static str, Option<&'a Path>)>,
) -> AppResult<()> {
    for (flag, path) in outputs {
        let Some(path) = path else {
            continue;
        };
        disk::preflight_recording_parent(recording_parent(path)).map_err(|error| {
            AppFailure(match error {
                disk::DiskPreflightError::BelowMinimum { available } => format!(
                    "{flag} requires at least {} free bytes, found {available}",
                    disk::RECORDING_MINIMUM_FREE_BYTES
                ),
                disk::DiskPreflightError::Unavailable(kind) => {
                    format!("{flag} free space is unavailable: {kind}")
                }
            })
        })?;
    }
    Ok(())
}

/// Checks every configured recording output and the optional fallback
/// directory against the crash-safe session contract before any recorder
/// exists: each output stem must yield valid session names, and the fallback
/// must not be an output's own directory.
///
/// # Errors
///
/// Returns an [`AppFailure`] naming the flag; it never renders a path.
#[cfg_attr(
    not(feature = "native-media"),
    allow(clippy::unnecessary_wraps, clippy::needless_pass_by_value)
)]
fn validate_recording_sessions<'a>(
    outputs: impl IntoIterator<Item = (&'static str, Option<&'a Path>)>,
    fallback: Option<&Path>,
) -> AppResult<()> {
    #[cfg(feature = "native-media")]
    for (flag, path) in outputs {
        let Some(path) = path else {
            continue;
        };
        recording_session::validate_output_stem(path)
            .map_err(|reason| AppFailure(format!("{flag} {reason}")))?;
        if let Some(fallback) = fallback
            && let (Ok(parent), Ok(fallback)) = (
                fs::canonicalize(recording_parent(path)),
                fs::canonicalize(fallback),
            )
            && parent == fallback
        {
            return Err(AppFailure(format!(
                "--record-fallback-dir must differ from the {flag} output directory"
            ))
            .into());
        }
    }
    // Recording requires native media; without it nothing is configured.
    #[cfg(not(feature = "native-media"))]
    let _ = (outputs, fallback);
    Ok(())
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
        status_listen,
        hls_listen,
    } = listeners;
    if !listen.ip().is_loopback() {
        return Err(AppFailure(format!(
            "development mode requires a loopback listen address, got {}",
            listen.ip()
        ))
        .into());
    }
    if let Some(address) = web_listen {
        if !address.ip().is_loopback() {
            return Err(AppFailure("--web-listen must use a loopback address".into()).into());
        }
        if once {
            return Err(AppFailure("--web-listen cannot be combined with --once".into()).into());
        }
        if !matches!(&mode, NativeServeMode::Disabled) {
            return Err(
                AppFailure("--web-listen requires non-native simulated mode".into()).into(),
            );
        }
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
    let journal = DurableJournal::new(store.open_journal_writer()?);
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
        let native = native.as_mut().expect("recording requires native state");
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
                    if let Some((frame, revision)) =
                        clock.due(control, playlist_clock::Pacing::Engine)
                    {
                        let delivery = execute_native_authority_command(
                            control,
                            &journal,
                            &mut durable,
                            &principal,
                            &authority,
                            &playlist_clock::advance_command(frame, revision),
                            now_millis()?,
                            native,
                            Some(&process_shutdown),
                        )?;
                        clock.observe(frame, revision, &delivery.result);
                    }
                    if let Some((list, frame, revision)) =
                        cues.due(control, playlist_clock::Pacing::Engine)
                    {
                        let delivery = execute_native_authority_command(
                            control,
                            &journal,
                            &mut durable,
                            &principal,
                            &authority,
                            &cue_clock::advance_command(list, frame, revision),
                            now_millis()?,
                            native,
                            Some(&process_shutdown),
                        )?;
                        cues.observe(list, frame, revision, &delivery.result);
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
                    checkpoint_native(&control, &journal, &mut durable)?;
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
        Some(web::WebGateway::bind(web_listener)?)
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
    if let Some(duration) = diagnostic_stop_after {
        process_shutdown.set_diagnostic_deadline(duration)?;
    }

    // Title templates and fonts load once, before serving, for the simulated
    // monitor tap and for exact title previews (which native sessions serve
    // too). Native media loads its own for its title render worker (see
    // `native_titles`).
    let titles = Arc::new(title_render::TitleAssets::load(&store, durable.project()));
    let session_result = non_native_sessions::run(
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
    if native.is_some() {
        checkpoint_native(&control, &journal, &mut durable)?;
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
    store: &dyn DurableStore,
    durable: &mut StoredProject,
) -> AppResult<()> {
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
/// pacer's frame, so a write lands in the slack before the next deadline
/// rather than in front of a frame. A marker is one small journal commit per
/// [`journal::PROGRESS_MARKER_SECONDS`]; a fold is a manifest checkpoint per
/// [`journal::PROGRESS_FOLD_SECONDS`]. Progress is only recorded at an idle
/// frame boundary — the only position replay (and a checkpoint) can
/// reproduce — so a pass with work in flight records nothing and the next
/// pass tries again. A failed write is reported once and never stops the
/// show: it loses at most idle progress, never an acknowledged command.
pub(crate) struct NativeProgress {
    cadence: journal::ProgressCadence,
    failing: bool,
}

impl NativeProgress {
    pub(crate) fn new(durable: &StoredProject) -> Self {
        Self {
            cadence: journal::ProgressCadence::new(
                durable.project().settings().frame_rate,
                durable.position().frames_rendered,
            ),
            failing: false,
        }
    }

    pub(crate) fn poll(
        &mut self,
        control: &SharedControl,
        store: &dyn DurableStore,
        durable: &mut StoredProject,
    ) {
        let control = control.borrow();
        let cursor = control.frame_cursor();
        let due = self.cadence.due(cursor, durable.position().frames_rendered);
        if due == journal::ProgressDue::Nothing {
            return;
        }
        let Ok(snapshot) = control.idle_engine_snapshot() else {
            return;
        };
        drop(control);
        let result = if due == journal::ProgressDue::Fold {
            self.cadence.folded(cursor);
            // The idle snapshot moves the runtime position without moving
            // the revision, exactly as the shutdown checkpoint does.
            stored_project_checkpoint(durable, &snapshot).and_then(|folded| {
                store.checkpoint(&folded)?;
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

/// Loads the manifest and replays whatever the journal recorded after it.
///
/// The manifest is the last checkpointed state; every mutation accepted since
/// then is in the journal. Recovery therefore loads the manifest, re-executes
/// the recorded commands through the same path that first accepted them, and
/// checkpoints the result, so the daemon opens at the revision the operator was
/// last acknowledged at rather than at the last checkpoint.
///
/// Opening also creates the journal when the project has never had one, so the
/// daemon starts owning a journal whose checkpoint agrees with the manifest and
/// the first command can be appended without re-reading it.
///
/// `recover_to_checkpoint` abandons that history instead of replaying it. See
/// [`abandon_to_checkpoint`].
fn load_and_recover(store: &ProjectStore, recover_to_checkpoint: bool) -> AppResult<StoredProject> {
    let manifest = store.load()?;
    // Finish any checkpoint that saved the manifest but did not commit its
    // staged title animations yet, so templates load as the manifest has
    // them; animations staged later wait for their replayed batches.
    journal::commit_staged_title_animations(store, &manifest, false)?;
    let durable = recover_from_manifest(store, manifest, recover_to_checkpoint)?;
    // Commit what replay folded into the manifest and drop staged copies of
    // batches that were never written or were abandoned.
    journal::commit_staged_title_animations(store, &durable, true)?;
    Ok(durable)
}

fn recover_from_manifest(
    store: &ProjectStore,
    manifest: StoredProject,
    recover_to_checkpoint: bool,
) -> AppResult<StoredProject> {
    if recover_to_checkpoint {
        return abandon_to_checkpoint(store, manifest);
    }
    let scan = store.recover_journal()?;
    for observation in scan.observations() {
        eprintln!("FREEMIXD_JOURNAL\tv=1\tobservation={observation}");
    }
    let progress = scan
        .progress()
        .map(|progress| journal::decode_progress(progress.payload()))
        .transpose()?
        // A marker at or behind the manifest (a fold or a clean stop wrote
        // the manifest after it) has nothing left to replay.
        .filter(|progress| {
            !scan.batches().is_empty()
                || progress.frames_rendered > manifest.position().frames_rendered
        });
    if scan.batches().is_empty() && progress.is_none() {
        return Ok(manifest);
    }
    let journal_head = scan
        .batches()
        .last()
        .map_or_else(|| scan.checkpoint_sequence(), MutationBatch::sequence);

    let bundle_assets = load_bundle_assets(store, manifest.project())?;
    let durable = replay_journal(&manifest, &bundle_assets, scan.batches(), progress)?;
    // Fold the replayed mutations into the manifest and discard them, so the
    // next crash replays only what happened after this point and a second
    // recovery of the same batches is impossible.
    store.checkpoint_and_compact(&durable, journal_head)?;
    eprintln!(
        "FREEMIXD_JOURNAL\tv=1\trecovered_batches={}\tfrom_revision={}\tto_revision={}\tframe={}",
        scan.batches().len(),
        manifest.position().revision,
        durable.position().revision,
        durable.position().frames_rendered
    );
    Ok(durable)
}

/// Throws away everything the journal recorded after the manifest.
///
/// Refusing a replay that does not reproduce what was acknowledged is right,
/// and it leaves an operator minutes from air with a bundle that will not open.
/// Deleting the journal directory was the only way back on air, and it discards
/// the same work with no record that it ever existed. This is that act made
/// deliberate and legible: it is asked for by name on the command line, and
/// every batch it destroys is printed with its sequence, its revision and the
/// command it carried — including the ones too damaged to decode — before the
/// daemon serves anything.
fn abandon_to_checkpoint(
    store: &ProjectStore,
    manifest: StoredProject,
) -> AppResult<StoredProject> {
    let abandoned = store.abandon_unapplied_batches()?;
    for batch in abandoned.batches() {
        let command = journal::decode_mutation(batch.payload()).map_or_else(
            |error| format!("<unreadable: {error}>"),
            |recorded| recorded.command.id,
        );
        eprintln!(
            "FREEMIXD_JOURNAL\tv=1\tabandoned_batch={}\tbase_revision={}\trevision={}\tcommand={command}",
            batch.sequence(),
            batch.base_revision(),
            batch.revision()
        );
    }
    eprintln!(
        "FREEMIXD_JOURNAL\tv=1\trecover_to_checkpoint=1\tabandoned_batches={}\tserving_revision={}",
        abandoned.batches().len(),
        manifest.position().revision
    );
    Ok(manifest)
}

/// Rebuilds the pre-crash state by re-running the recorded commands.
///
/// Nothing in this workspace applies an event to a project: the engine is
/// command-sourced, so the only faithful way to reach the pre-crash state is
/// the path that produced it. Each batch therefore carries the command and the
/// instant it was submitted, and replay runs `execute_durable_command` against
/// an engine restored from the manifest, recording nothing and publishing to
/// nobody.
///
/// Frames a native daemon realized between two commands are ticked idle
/// before the later one (see [`replay_idle_frames`]), and the frames it
/// realized after the last one are ticked idle up to the journal's progress
/// marker, `progress`, when that is ahead (see [`journal::ProgressCadence`]).
/// Every idle frame counts against one bound, [`IdleReplayBudget`], so a
/// damaged frame number refuses startup instead of ticking forever.
///
/// Every batch must land on exactly the position it recorded — not just its
/// revision. A refused command advances no revision and a frame that elapsed
/// between two commands advances no revision either, so the revision alone
/// cannot see a replay whose frame cursor, clock or event stream came back
/// different from the one the operator was acknowledged against. A divergence
/// means the replay is not that history, so it is reported instead of silently
/// becoming the project's state.
fn replay_journal(
    manifest: &StoredProject,
    bundle_assets: &BundleAssets,
    batches: &[MutationBatch],
    progress: Option<ProjectPosition>,
) -> AppResult<StoredProject> {
    let mut durable = manifest.clone();
    let project_id = durable.project().id();
    let identity = format!("project-{project_id}");
    let mut control = ControlService::new(
        restore_engine_with_titles(&durable, bundle_assets)?,
        Policy::development(),
        identity.clone(),
        format!("{identity}-log"),
        ControlLimits::default(),
    );
    let server = control_server_identity(&control, project_id);
    let principal = development_principal()?;
    let mut budget = IdleReplayBudget::new(durable.project().settings().frame_rate);
    for batch in batches {
        let recorded = journal::decode_mutation(batch.payload())?;
        if recorded.position.revision != batch.revision() {
            return Err(AppFailure(format!(
                "journal batch {} claims revision {} but its record holds revision {}",
                batch.sequence(),
                batch.revision(),
                recorded.position.revision
            ))
            .into());
        }
        replay_idle_frames(&mut control, &principal, &server, &recorded, &mut budget).map_err(
            |error| {
                AppFailure(format!(
                    "journal batch {} (command `{}`): {error}",
                    batch.sequence(),
                    recorded.command.id
                ))
            },
        )?;
        execute_durable_command(
            &mut control,
            &ReplayedMutations,
            &mut durable,
            &principal,
            &server,
            &recorded.command,
            recorded.submitted_at_millis,
        )?;
        if durable.position() != recorded.position {
            return Err(AppFailure(format!(
                "journal batch {} replayed command `{}` to {:?} instead of the acknowledged {:?}",
                batch.sequence(),
                recorded.command.id,
                durable.position(),
                recorded.position
            ))
            .into());
        }
    }
    if let Some(progress) = progress {
        replay_progress(&mut control, &server, &mut durable, progress, &mut budget)?;
    }
    Ok(durable)
}

/// Ticks the engine idle up to the journal's progress marker.
///
/// The marker records the position a native daemon reached after the last
/// recorded command, so the frames between are idle frames, ticked exactly
/// as the pacer realized them; the result must land on the whole recorded
/// position, like a replayed batch. A marker at or behind the replay names
/// progress a later checkpoint already holds, and is skipped.
fn replay_progress(
    control: &mut ControlService<Policy>,
    server: &ServerIdentity,
    durable: &mut StoredProject,
    progress: ProjectPosition,
    budget: &mut IdleReplayBudget,
) -> AppResult<()> {
    if progress.revision != durable.position().revision
        || progress.frames_rendered <= durable.position().frames_rendered
    {
        return Ok(());
    }
    budget
        .tick_until(control, server, progress.frames_rendered)
        .map_err(|error| AppFailure(format!("journal progress marker: {error}")))?;
    let snapshot = control.idle_engine_snapshot().map_err(|error| {
        AppFailure(format!(
            "journal progress marker at frame {} is not an idle frame boundary: {error}",
            progress.frames_rendered
        ))
    })?;
    let advanced = stored_project_checkpoint(durable, &snapshot)?;
    if advanced.position() != progress {
        return Err(AppFailure(format!(
            "journal progress marker replayed to {:?} instead of the recorded {progress:?}",
            advanced.position()
        ))
        .into());
    }
    *durable = advanced;
    Ok(())
}

/// How many idle frames journal replay may still tick.
///
/// Idle frames carry no command, so replay reaches them only from recorded
/// frame numbers. A damaged journal can name any frame, and ticking to it
/// would hold startup for hours or forever, so the whole replay shares one
/// budget of [`journal::MAX_REPLAY_IDLE_SECONDS`] of show at the project's
/// frame rate, checked *before* ticking: a request past it refuses startup
/// immediately with the frames it asked for.
struct IdleReplayBudget {
    remaining: u64,
    limit: u64,
}

impl IdleReplayBudget {
    fn new(rate: fm_types::FrameRate) -> Self {
        let limit = journal::frames_in(journal::MAX_REPLAY_IDLE_SECONDS, rate);
        Self {
            remaining: limit,
            limit,
        }
    }

    /// Ticks the engine idle until its cursor reaches `target`, refusing a
    /// target the remaining budget does not cover.
    fn tick_until(
        &mut self,
        control: &mut ControlService<Policy>,
        server: &ServerIdentity,
        target: u64,
    ) -> AppResult<()> {
        let from = control.frame_cursor();
        self.check(from, target)?;
        while control.frame_cursor() < target {
            control.tick(server)?;
        }
        self.charge(control.frame_cursor().saturating_sub(from));
        Ok(())
    }

    /// Charges `frames` ticked idle frames.
    const fn charge(&mut self, frames: u64) {
        self.remaining = self.remaining.saturating_sub(frames);
    }

    /// Refuses ticking from `cursor` up to `target` when the remaining
    /// budget does not cover it.
    fn check(&self, cursor: u64, target: u64) -> AppResult<()> {
        let frames = target.saturating_sub(cursor);
        if frames > self.remaining {
            return Err(AppFailure(format!(
                "replay would tick {frames} idle frames (frame {cursor} to {target}), past the \
                 bound of {} idle frames ({} hours of show) per recovery; the recorded frame is \
                 implausible, so the journal is damaged (--recover-to-checkpoint discards it)",
                self.limit,
                journal::MAX_REPLAY_IDLE_SECONDS / 3_600
            ))
            .into());
        }
        Ok(())
    }
}

/// Realizes the idle frames the daemon rendered before `recorded` executed.
///
/// The native media runtime's pacer realizes a frame on every deadline, also
/// between commands, so a command can begin at a later frame cursor than the
/// previous command's own ticks leave a replay at. Those frames carry no
/// command, so the journal records only where each command ended; the frame
/// it began at is that end minus the frames the command itself realizes.
/// Replay ticks the engine idle up to there, exactly as the pacer did. A
/// simulated daemon renders idle frames only to catch a client's playlist
/// control up to the show's wall-clock frame
/// (`playlist_clock::PlaylistClock::catch_up`); replay ticks those the same
/// way, and nothing for any other command.
///
/// The clock's `playlist_advance` is accepted only for the playlist's
/// pending boundary, or for the next frame once that boundary has passed: a
/// native advance issued late (the boundary passed while frames were realized
/// without a clock pass, e.g. across a restart) names the frame it ran on, so
/// replay first ticks idle up to that frame.
///
/// A replay that still lands elsewhere is reported by the position check.
fn replay_idle_frames(
    control: &mut ControlService<Policy>,
    principal: &Principal,
    server: &ServerIdentity,
    recorded: &journal::RecordedMutation,
    budget: &mut IdleReplayBudget,
) -> AppResult<()> {
    if let CommandPayload::CueAdvance { list, frame } = recorded.command.payload {
        // As for a playlist advance: ticks stop at the due frame when the
        // advance was on time.
        let from = control.frame_cursor();
        budget.check(from, frame)?;
        while control.frame_cursor() < frame && control.cue_advance_frame() != Some((list, frame)) {
            control.tick(server)?;
        }
        budget.charge(control.frame_cursor().saturating_sub(from));
    }
    if let CommandPayload::PlaylistAdvance { frame } = recorded.command.payload {
        // Ticks stop at the boundary when the advance was on time, so
        // `frame` only bounds them; the budget is charged what was ticked.
        let from = control.frame_cursor();
        budget.check(from, frame)?;
        while control.frame_cursor() < frame && control.playlist_advance_frame() != Some(frame) {
            control.tick(server)?;
        }
        budget.charge(control.frame_cursor().saturating_sub(from));
    }
    let ticks = match control.prepare_submit(
        principal,
        recorded.command.clone(),
        recorded.submitted_at_millis,
    )? {
        PrepareSubmitOutcome::Prepared(prepared) if prepared.submission().is_accepted() => {
            command_ticks(&recorded.command, &prepared)
        }
        PrepareSubmitOutcome::Prepared(_) | PrepareSubmitOutcome::Replayed(_) => return Ok(()),
    };
    let began = recorded.position.frames_rendered.saturating_sub(ticks);
    budget.tick_until(control, server, began)
}

/// Maps one validated model recorder slot onto the switcher's slot domain.
///
/// Both domains cover exactly `1..=2`, so this conversion cannot fail for a
/// slot the model itself constructed.
fn switcher_recorder_slot(slot: ModelRecorderSlot) -> RecorderSlot {
    RecorderSlot::new(slot.get()).expect("model recorder slots are in the switcher range")
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

/// Seeds every title input's persisted field values and revision into the
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
        let fields = fm_titles::TitleFieldSet::restore(
            template,
            title.field_revision,
            title.fields.iter().map(|entry| {
                (
                    fm_titles::FieldId::new(entry.id.get()),
                    title_render::field_value(&entry.value),
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
    }
    show.restore_input_lifecycle(
        canonical.input_categories().to_vec(),
        canonical.input_lifecycles(),
    )?;
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
    // Checked against the restored titles and media inputs.
    show.set_cue_lists(canonical.cue_lists().to_vec())?;
    let position = project.position();
    Ok(Engine::restore_persisted(
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
    )?)
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
    let slot = StingerSlotId::new(config.slot.number())
        .expect("validated model Stinger slots are in the switcher range");
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
            },
        )?;
        show.set_input_audio_label(strip.input, strip.label.clone())?;
        show.set_input_audio_group(strip.input, strip.group)?;
    }
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

/// Seeds the show's display-output slots from the project (OR-011).
fn restore_display_outputs(show: &mut ShowState, project: &fm_model::Project) -> AppResult<()> {
    for (&slot, config) in project.display_outputs() {
        show.set_display_output(slot, Some(config.clone()))?;
    }
    Ok(())
}

/// Folds the show's display-output slots (OR-011) into the stored project.
/// Every slot is cleared first, so a display moving between slots is never
/// claimed twice.
fn sync_project_display_outputs(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
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
    }
    // Parents are cleared first so an old parent never closes a transient
    // cycle with a new one.
    for nested in [false, true] {
        for (group, value) in fm_model::AudioGroupNumber::ALL
            .into_iter()
            .zip(show.audio_groups())
        {
            let gain = InputGainMilliDb::new(value.state.gain_millidb)
                .expect("engine group gain is bounded by the model contract");
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
    project.set_audio_solo_mode(show.audio_solo_mode());
    let monitor = show.audio_monitor();
    project
        .set_audio_monitor_output(fm_model::AudioMonitorOutput {
            device: monitor.device.clone(),
            gain: InputGainMilliDb::new(monitor.state.gain_millidb)
                .expect("engine monitor gain is bounded by the model contract"),
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
        gain: InputGainMilliDb::new(state.gain_millidb)
            .expect("engine input audio gain is bounded by the model contract"),
        balance: InputBalanceBasisPoints::new(state.balance_basis_points)
            .expect("engine input audio balance is bounded by the model contract"),
        pan: fm_model::InputPanBasisPoints::new(state.pan_basis_points)
            .expect("engine input audio pan is bounded by the model contract"),
        muted: state.muted,
        soloed: state.soloed,
        follow_video: state.follow_video,
        delay_samples: InputDelaySamples::new(state.delay_samples)
            .expect("engine input audio delay is bounded by the model contract"),
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
                        &WireMessage::DiagnosticsResponse(diagnostics_response(control, request)?),
                    )?;
                }
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
    let encoded_bytes = encode_line(&WireMessage::Command(command.clone()))?.len();
    let now = now_millis()?;
    // The session authorizes its scoped role against the classes fm-control
    // resolves now, including the edit an undo or redo would revert or reapply.
    let required = control.borrow().required_classes(&command.payload);
    if let Err(error) = session.admit_command(command, required, encoded_bytes, now) {
        let result = session_rejection(
            command,
            &error,
            control.borrow().diagnostics().current_revision,
        );
        return Ok(CommandDelivery {
            result,
            #[cfg(test)]
            events: Vec::new(),
            #[cfg(test)]
            runtime_events: Vec::new(),
        });
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
    let execution = execute_durable_command(
        &mut control,
        store,
        durable,
        principal,
        server,
        command,
        now,
    )?;
    imported.discard_unused(store, durable.project());
    // Imported, duplicated, replaced and reopened media inputs play from a
    // synthetic source as soon as they exist (IN-026).
    attach_synthetic_media(&mut control, durable.project())?;
    Ok(durable_delivery(execution))
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
            |control, server| {
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
    native.observe_recording_command(command, &execution.submission.output.result, durable);
    // A cue's record and stream actions start and stop the same workers
    // (RC-017); its acknowledgement is not held for a segment's readiness.
    #[cfg(feature = "native-media")]
    for worker in cue_worker_commands(command, &execution.submission) {
        native.observe_stream_command(&worker, &execution.submission.output.result);
        native.observe_recording_command(&worker, &execution.submission.output.result, durable);
    }
    // Changed title fields re-render on the title worker; this only
    // compares and hands over state.
    #[cfg(feature = "native-media")]
    native.titles.observe(durable.project());
    #[cfg(feature = "native-media")]
    native.titles.observe_templates(&control.title_templates());
    #[cfg(feature = "native-media")]
    native.sync_input_catalogue(control, durable.project())?;
    // Destination edits (OR-006) rebuild only the edited destination.
    #[cfg(feature = "native-media")]
    native.sync_stream_targets(durable.project(), durable.position().frames_rendered)?;
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
            let used = project.inputs().iter().any(|input| {
                matches!(&input.kind, fm_model::InputKind::Media { asset_uri, .. } if asset_uri == uri)
            });
            if !used && let Err(error) = store.discard_imported_media(uri) {
                eprintln!("FREEMIXD_IMPORT\tv=1\tasset={uri}\terror={error}");
            }
        }
    }
}

/// The host path `uri` names when a batch import or a source replacement
/// should bring it into the bundle: an absolute path to a regular media file.
fn importable_host_path(uri: &str) -> Option<&Path> {
    let path = Path::new(uri);
    (!uri.starts_with(fm_model::MEDIA_ASSET_URI_PREFIX)
        && path.is_absolute()
        && fm_model::is_media_file_name(uri)
        && path.is_file())
    .then_some(path)
}

/// The media URIs of a batch import (every item) or a media source
/// replacement (one), mutably.
fn host_media_uris(payload: &mut CommandPayload) -> Vec<&mut String> {
    match payload {
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::Import { items }) => {
            items.iter_mut().map(|item| &mut item.uri).collect()
        }
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::ReplaceSource {
            source: fm_protocol::WireInputSource::Media { uri },
            ..
        }) => vec![uri],
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
    for (index, uri) in host_media_uris(&mut provisional.payload)
        .into_iter()
        .enumerate()
    {
        if let Some(path) = importable_host_path(uri) {
            hosts.push((index, path.to_path_buf()));
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
                        .map(|(index, _)| *index)
                        .filter(|index| !refused.contains(index))
                        .collect(),
                )
            }
            Err(_) => None,
        };
    let mut rewritten = provisional;
    let mut copied = Vec::new();
    let mut uris = host_media_uris(&mut rewritten.payload);
    for (index, path) in hosts {
        if kept.as_ref().is_some_and(|kept| !kept.contains(&index)) {
            // Refused as prepared; the stand-in keeps its reason.
            continue;
        }
        let uri = &mut *uris[index];
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
            .project(0)
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
        if let Err(error) = store.record(command, now_millis, durable, &updated) {
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
        .project(1)
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
            .project(0)
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
        if let Err(error) = store.record(command, now_millis, durable, &updated) {
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
        .project(1)
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
    if candidate.project().stingers() != updated.project().stingers()
        || native
            .playback
            .validate_retained_byte_limit(mutation.ordinary_video_limit)
            .is_err()
    {
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
    if let Err(error) = store.record(command, now_millis, durable, &updated) {
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
#[derive(Debug)]
struct CommandRefusal {
    code: RejectionCode,
    retryable: bool,
    message: String,
    /// The typed reason clients word in their language (Protocol 2.45).
    reason: Option<fm_protocol::RefusalReason>,
}

impl fmt::Display for CommandRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl Error for CommandRefusal {}

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
        |control, server| Ok(control.tick(server)?.runtime_events),
    )
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
#[allow(clippy::too_many_arguments)]
fn execute_durable_command_with_ticks(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now_millis: u64,
    vet: Option<&ProjectVet<'_>>,
    mut tick: impl FnMut(
        &mut ControlService<Policy>,
        &ServerIdentity,
    ) -> AppResult<Vec<RuntimeEventMessage>>,
) -> AppResult<DurableExecution> {
    // Nothing commits before `record`, so a refusal reports this revision.
    let current_revision = control.diagnostics().current_revision;
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

    let ticks = if prepared.submission().is_accepted() {
        command_ticks(command, &prepared)
    } else {
        0
    };
    // An accepted undo or redo is journaled as the edit it resolved to:
    // replay restores an engine whose session history is empty. The project
    // sync reads the same resolved command, so an undone input removal and a
    // live input addition both know the input kind they persist.
    let journaled = prepared.replay_command(command);
    let staged = prepared
        .project(ticks)
        .map_err(Into::into)
        .and_then(|projected| {
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
    let animations = staged_title_animations(&prepared, &updated);
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
    if let Err(error) = store.record(&journaled, now_millis, durable, &updated) {
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
    let mut runtime_events = Vec::new();
    for _ in 0..ticks {
        runtime_events.extend(tick(control, server)?);
    }
    *durable = updated;

    Ok(DurableExecution {
        submission,
        runtime_events,
    })
}

/// The template animations an accepted command leaves `updated` with: one
/// per `title_animation_changed` event, keyed by the stored template URI
/// its titles name.
fn staged_title_animations(
    prepared: &fm_control::PreparedSubmission<'_, Policy>,
    updated: &StoredProject,
) -> Vec<(String, fm_titles::TitleAnimation)> {
    if !prepared.submission().is_accepted() {
        return Vec::new();
    }
    prepared
        .output()
        .events
        .iter()
        .filter_map(|event| {
            let fm_protocol::EventPayload::TitleAnimationChanged {
                inputs,
                animation_revision,
                clip,
            } = &event.payload
            else {
                return None;
            };
            let title =
                title_render::title_of_input(updated.project(), inputs.first()?.to_domain())?;
            let clip = fm_control::title_clip_from_wire(clip).ok()?;
            Some((
                title.template_uri.clone(),
                fm_titles::TitleAnimation::new(clip, *animation_revision),
            ))
        })
        .collect()
}

/// The frames an accepted command realizes: its own (see
/// [`command_base_ticks`]), extended until every scene phase it started
/// has played (GX-011 stage 5), as a title take realizes its whole In
/// phase.
fn command_ticks(
    command: &CommandMessage,
    prepared: &fm_control::PreparedSubmission<'_, Policy>,
) -> u64 {
    prepared.scene_settle_ticks(command_base_ticks(command, prepared))
}

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
    // A cue operation may target a later frame and start transitions.
    if matches!(
        command.payload,
        CommandPayload::CueControl { .. } | CommandPayload::CueAdvance { .. }
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
                OverlayChannelId::new(channel.number()).expect("wire overlay channels are bounded");
            prepared.overlay_transition_ticks(channel)
        }
        // A title take realizes its whole In phase, as a fade realizes its
        // whole duration.
        CommandPayload::TakeTitle { input, channel, .. } => {
            let channel =
                OverlayChannelId::new(channel.number()).expect("wire overlay channels are bounded");
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
        | CommandPayload::InputLifecycle(_)
        | CommandPayload::SceneLayer(_)
        | CommandPayload::AudioMixer(_)
        | CommandPayload::SetFadeToBlackOutput { .. }
        | CommandPayload::SetFadeToBlackOutputDuration { .. }
        | CommandPayload::SetFadeToBlackCurve { .. }
        | CommandPayload::SetDisplayOutput { .. }
        | CommandPayload::ClearDisplayOutput { .. }
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
        | CommandPayload::SetTitleAnimation { .. }
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
        | CommandPayload::ApplySceneAnimationUserPreset { .. } => 1,
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
        if !matches!(result, CommandResult::Rejected { code, .. } if code == "permission_denied") {
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
            Some(CommandPayload::InputLifecycle(
                fm_protocol::InputLifecycleOperation::Import { .. },
            )) => {
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

/// Persists every title's live field values and revision.
fn sync_project_titles(project: &mut fm_model::Project, show: &ShowState) -> AppResult<()> {
    for (&input, title) in show.titles() {
        let fields = title
            .fields
            .values()
            .iter()
            .map(|(field, value)| fm_model::TitleFieldEntry {
                id: fm_model::TitleFieldId::new(field.get()),
                value: title_render::model_field_value(value),
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
        .expect("every overlay transition has a persisted token"),
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
        let channel = OverlayChannelId::from_index(index).expect("overlay index is bounded");
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
                .expect("every persisted overlay transition has a wire token"),
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
    for number in 1..=u8::try_from(StingerSlotNumber::COUNT)
        .expect("Stinger slot count fits the operator-facing number")
    {
        let model_slot = StingerSlotNumber::new(number).expect("Stinger slot number is bounded");
        let slot = StingerSlotId::new(number).expect("Stinger slot number is bounded");
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
        u16::try_from(state.position.numerator())
            .expect("engine fade-to-black numerator uses the u16 contract"),
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
    PersistedManualTransitionState::new(
        kind,
        state.from(),
        state.to(),
        state.interval_start().basis_points(),
        state.position().basis_points(),
    )
    .expect("engine manual transition positions are bounded")
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
    ClockDomainId::new(NonZeroU128::new(1).expect("one is nonzero"))
}

fn required_routing(value: Option<InputId>, field: &'static str) -> AppResult<InputId> {
    value.ok_or_else(|| AppFailure(format!("project is missing {field} routing")).into())
}

fn now_millis() -> AppResult<u64> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    u64::try_from(millis)
        .map_err(|_| AppFailure("system time exceeds protocol range".into()).into())
}

fn print_help() {
    println!(
        "FreeMix headless production daemon\n\n\
Usage:\n  freemixd serve <show.freemix> [--listen 127.0.0.1:0] [--web-listen 127.0.0.1:0] [--status-listen 127.0.0.1:0] [--hls-listen 127.0.0.1:0] [--once] [--native-media [--camera-helper PATH]] [--record-program output.mp4 [--record-output <output-id>]] [--record-program-2 output-2.mp4 [--record-output-2 <output-id>]] [--record-wav] [--record-segment-seconds <1..=86400>] [--record-segment-seconds-2 <1..=86400>] [--record-bitrate-kbps <1000..=100000>] [--record-bitrate-kbps-2 <1000..=100000>] [--record-audio-bitrate-kbps <8..=1024>] [--record-audio-bitrate-kbps-2 <8..=1024>] [--record-fallback-dir DIR] [--diagnostic-stop-after 10m] [--recover-to-checkpoint] [--fullscreen-program [--fullscreen-display 0]] [--display-output <spec> [--display-output <spec>]] [--display-presenter | --display-presenter-if-available] [--diagnostic-display-inventory FILE] [--diagnostic-capture-inventory FILE]\n  freemixd help\n  freemixd --version\n\n\
Native media is opt-in; without it the daemon uses simulated frame realization.\n\
--camera-helper overrides the developer AVFoundation helper path for exact macOS Device inputs; it never requests permission.\n\
Program recording requires native media and an existing output parent; segments are created exclusively, never overwritten. The first segment uses the configured path itself and later record-start segments use <stem>-<NNN>.mp4 names through 999. Both --record-program and --record-program-2 follow the engine's recording desired flag, and each slot derives its own segment names, latches its own failures, and emits its own slot-annotated FREEMIXD_RECORDER report. --record-output and --record-output-2 select which project output each slot records.\n\
--record-wav additionally writes a 16-bit linear-PCM WAV sidecar for each segment of every configured recorder, sharing its stem (`program-002.mp4` is accompanied by `program-002.wav`) and following the program's channel layout. The sidecar is its own crash-safe session (<stem>.wav.fmsession) that fails over with the video: a write or sync failure continues whole sample frames in <stem>-<NNNN>.wav on the same directory or on --record-fallback-dir. Header sizes are fixed when each file closes, and startup recovery repairs the sizes of sidecars a crash left open; FREEMIXD_RECORDER_WAV_SESSION and FREEMIXD_RECORDING_WAV_RECOVERY report them, and a sidecar failure is reported as failure=WavOutput(...).\n\
--record-segment-seconds and --record-segment-seconds-2 rotate the active --record-program and --record-program-2 slot respectively to its next <stem>-<NNN>.mp4 segment on a frame boundary after at least the configured number of seconds; each accepts <option> <value> or <option>=<value> and requires its own recorder, and a slot without its own duration never rotates automatically.\n\
--record-bitrate-kbps and --record-bitrate-kbps-2 set each slot's libx264 video bitrate in kbps (1000..=100000), and --record-audio-bitrate-kbps and --record-audio-bitrate-kbps-2 set its AAC audio bitrate in kbps (8..=1024); each accepts <option> <value> or <option>=<value>, applies only to its own --record-program/--record-program-2 slot, and leaves that slot's encoder defaults untouched when unset.\n\
Every program segment is a crash-safe recording session with a <stem>.fmsession manifest beside it, so output stems must be 1 to 60 ASCII letters, digits, `-`, `_`, or `.`. A write or sync failure re-homes the fragment in flight into <stem>-<NNNN>.mp4 on the same directory, or, when that directory is full, read-only, missing, or forbidden, on --record-fallback-dir (accepts <option> <value> or <option>=<value>, applies to every slot, must differ from each output directory, and is unavailable with --fullscreen-program); the recording continues instead of failing. Startup recovers every session of the configured slots that a crash left open, cutting torn segments back to their last complete fragment, before anything records. FREEMIXD_RECORDER_SPACE reports each segment's free-space preflight (a start is refused only when every measured directory is exhausted), FREEMIXD_RECORDER_FAILOVER each failover, FREEMIXD_RECORDER_SESSION each finished session, and FREEMIXD_RECORDING_RECOVERY what each recovery kept and lost.\n\
Configured network stream endpoints are probed once at startup and reported as FREEMIXD_PREFLIGHT; the probe is bounded and never blocks or refuses startup, and it is a reachability check rather than a bandwidth or health guarantee.\n\
Use --record-program=<path> or --record-program-2=<path> when the output name begins with --. Recorder capability digests describe configured startup support; FREEMIXD_RECORDER reports runtime health.\n\
Fullscreen display selection is a zero-based index ordered by desktop position, then name, then platform display id; --fullscreen-program --fullscreen-display N is shorthand for --display-output display=index:N.\n\
--display-output replaces the project's display-output slots (slot 1, then slot 2) and opens one borderless, cursor-hidden fullscreen output per occurrence (at most two, each on a distinct display); it requires --native-media plus a build with feature `display-surface`. --display-presenter opens the project's own display-output slots the same way. --display-presenter-if-available (what Studio's supervised daemon uses) does so (the presenter always runs native media) in a build with feature `display-surface` unless --web-listen, --status-listen, --hls-listen, --record-fallback-dir, --diagnostic-stop-after, or --recover-to-checkpoint is given; when no window system, window, or GPU is available it emits FREEMIXD_DISPLAY_PRESENTER state=unavailable and serves as without the option, which is also all it does in any other build. Slots are project configuration edited live by set_display_output/clear_display_output commands (Protocol 2.42); a daemon without a presenter keeps them waiting with reason no-presenter. A spec is comma-separated key=value fields: display=index:<n>|name:<name>|id:<platform-id> (required), source=program|preview|output:<output-id> (default program; preview is composited only while an output showing it is active), mode=current|WIDTHxHEIGHT[@HZ] (default current; an unsupported mode falls back to the current one and is reported), scaling=fit|fill|stretch|1:1 (default fit), and hdr=off|auto (default off; outputs present SDR and report why). A missing display leaves its output waiting; it reattaches when the display returns, matched by platform id and then by name. Each state change emits FREEMIXD_DISPLAY_OUTPUT and shutdown emits FREEMIXD_DISPLAY_OUTPUTS.\n\
--diagnostic-display-inventory FILE presents the project's display outputs headlessly to off-screen rasters sized to a tab-separated display inventory re-read from FILE (one display per line: id=, mode=, optional name=, x=, y=, modes=, hdr=; a missing file means no display is connected) and emits sampled FREEMIXD_DISPLAY_FRAME probes. It is a test and diagnostic aid, not an operator output.\n\
--diagnostic-capture-inventory FILE (requires --native-media) replaces the capture backend with a deterministic fake whose devices follow a tab-separated inventory re-read from FILE (one device per line: key=, path=, formats=, optional name=, rgba=, audio=, stalled=, no_signal=; a missing file means no device is connected). Capture inputs acquire solid pictures from it and report capture_status as with real devices. It is a test and diagnostic aid.\n\
--diagnostic-stop-after schedules cooperative simulated or headless native shutdown after readiness; accepted units are ms, s, m, and h up to 24h.\n\
--recover-to-checkpoint opens the project at its last checkpoint and permanently discards every acknowledged command the journal recorded after it, printing each abandoned sequence, revision and command id first. It loses work; use it only when a replay is refused and the show must go on. It is unavailable with --fullscreen-program.\n\
--web-listen enables the loopback-only `/v1/control` WebSocket listener and requires FREEMIXD_WEB_TOKEN.\n\
--status-listen enables the loopback-only operator status listener serving /healthz, /readyz, and the token-guarded /v1/support-bundle; it requires FREEMIXD_STATUS_TOKEN and is unavailable with --fullscreen-program.\n\
--hls-listen enables the loopback-only HLS listener for local hls:// stream destinations. It serves GET /v1/hls/<target>/index.m3u8 and GET /v1/hls/<target>/segment-NNNNN.ts, plus the same shapes under rung directories 0..=7 for a multi-bitrate ladder, and requires both --native-media and FREEMIXD_HLS_TOKEN; TLS, LAN binding, Web Controller roles, and per-viewer capacity remain future work.\n\
Native mode continues across client disconnects; close the Program window or press Escape for bounded shutdown."
    );
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
mod tests {
    use std::{
        cell::Cell,
        fs,
        panic::{AssertUnwindSafe, catch_unwind},
        sync::atomic::{AtomicU64, Ordering},
        sync::mpsc::TryRecvError,
    };

    use fm_control::{LiveEvent, Subscription};
    #[cfg(all(feature = "native-media", target_os = "macos"))]
    use fm_io_macos::{CameraIdKind, deterministic_camera_id};
    use fm_model::{
        Input, InputAudioStripState, InputBalanceBasisPoints, InputGainMilliDb, InputKind, Project,
        ProjectSettings, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, StingerConfig,
        StingerSlotNumber,
    };
    #[cfg(feature = "native-media")]
    use fm_model::{Rgba8 as ModelRgba8, Scene};
    use fm_protocol::{ClientType, Role, StreamRealizedState};
    #[cfg(feature = "native-media")]
    use fm_types::SceneId;
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, SampleFormat,
        SampleRate, ScanMode, VideoDimensions, VideoFormat,
    };
    #[cfg(feature = "native-media")]
    use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

    use super::*;

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn camera_discovery(permission: u8) -> Vec<u8> {
        camera_discovery_for(permission, &[("fake-camera", "Fake Camera")])
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn camera_discovery_for(permission: u8, cameras: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = b"FMCAMD2\0".to_vec();
        bytes.push(permission);
        bytes.extend_from_slice(&u32::try_from(cameras.len()).unwrap().to_le_bytes());
        for &(native_id, name) in cameras {
            for value in [native_id, name] {
                bytes.extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
                bytes.extend_from_slice(value.as_bytes());
            }
            bytes.extend_from_slice(&1_u32.to_le_bytes());
            bytes.extend_from_slice(&1_u32.to_le_bytes());
            bytes.extend_from_slice(&1_u32.to_le_bytes());
            bytes.extend_from_slice(&30_000_u32.to_le_bytes());
            bytes.extend_from_slice(&1_001_u32.to_le_bytes());
        }
        bytes
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn camera_frame(sequence: u64, pts_micros: i64) -> Vec<u8> {
        let mut bytes = b"FMCAMF3\0".to_vec();
        bytes.extend_from_slice(&62_u32.to_le_bytes());
        bytes.extend_from_slice(&sequence.to_le_bytes());
        bytes.extend_from_slice(&2_u64.to_le_bytes());
        bytes.extend_from_slice(&pts_micros.to_le_bytes());
        bytes.extend_from_slice(&1_000_i32.to_le_bytes());
        bytes.extend_from_slice(&1_001_i64.to_le_bytes());
        bytes.extend_from_slice(&30_000_i32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 2]);
        bytes.extend_from_slice(&[3, 5, 7, 255]);
        bytes
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn camera_slot_test_frame(sequence: u64, discontinuity: bool) -> CpuVideoFrame {
        let mut source = SimulatedVideoSource::new(
            1,
            1,
            FrameRate::new(30, 1).unwrap(),
            native_clock_domain(),
            SourcePattern::Solid(Rgba8::new(1, 2, 3, 255)),
        )
        .unwrap();
        let frame = source.next_frame().unwrap().unwrap();
        let original = frame.timing();
        let mut timing = MediaTiming::new(
            original.original_timestamp(),
            original.presentation_timestamp(),
            original.duration(),
            original.clock_domain(),
            SequenceNumber::new(sequence),
        )
        .unwrap();
        if discontinuity {
            timing = timing.with_flags(fm_frame::MediaFlags::DISCONTINUITY);
        }
        let metadata = frame.metadata();
        let frame = CpuVideoFrame::new(timing, frame.into_payload());
        if let Some(metadata) = metadata {
            frame.with_metadata(metadata).unwrap()
        } else {
            frame
        }
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn shell_octal(bytes: &[u8]) -> String {
        use fmt::Write as _;

        let mut output = String::new();
        for byte in bytes {
            write!(output, "\\{byte:03o}").unwrap();
        }
        output
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn fake_camera_helper(directory: &Path, permission: u8) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let helper = directory.join("camera-helper.sh");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture) printf '%s\\n' \"$@\" > \"$0.capture\"; printf '%s' \"$$\" > \"$0.pid\"; printf '{}'; exec sleep 30 ;;\n  request-permission) touch \"$0.permission\"; exit 91 ;;\n  *) exit 90 ;;\nesac\n",
            shell_octal(&camera_discovery(permission)),
            shell_octal(&camera_frame(7, 1_000)),
        );
        fs::write(&helper, script).unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&helper, permissions).unwrap();
        helper
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn recovering_camera_helper(
        directory: &Path,
        recover_on_capture: Option<u32>,
        initial_exit: i32,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let helper = directory.join("recovering-camera-helper.sh");
        let recovery_case = recover_on_capture.map_or_else(String::new, |capture| {
            format!(
                "  {capture}) sleep 0.05; printf '{}'; exec sleep 30 ;;\n",
                shell_octal(&camera_frame(0, 2_000))
            )
        });
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    count=0\n    if test -f \"$0.count\"; then read count < \"$0.count\"; fi\n    count=$((count + 1))\n    printf '%s' \"$count\" > \"$0.count\"\n    printf '%s\\n' \"$@\" >> \"$0.capture\"\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    case \"$count\" in\n  1) printf '{}'; sleep 0.10; exit {initial_exit} ;;\n{recovery_case}  *) printf '{}'; sleep 0.03; exit 20 ;;\n    esac ;;\n  *) exit 90 ;;\nesac\n",
            shell_octal(&camera_discovery(0)),
            shell_octal(&camera_frame(7, 1_000)),
            shell_octal(b"FMCAMF3\0"),
        );
        fs::write(&helper, script).unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&helper, permissions).unwrap();
        helper
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn hanging_recovery_camera_helper(directory: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let helper = directory.join("hanging-recovery-camera-helper.sh");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    count=0\n    if test -f \"$0.count\"; then read count < \"$0.count\"; fi\n    count=$((count + 1))\n    printf '%s' \"$count\" > \"$0.count\"\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    case \"$count\" in\n      1) printf '{}'; sleep 0.10; exit 20 ;;\n      *) printf '{}'; exec sleep 30 ;;\n    esac ;;\n  *) exit 90 ;;\nesac\n",
            shell_octal(&camera_discovery(0)),
            shell_octal(&camera_frame(7, 1_000)),
            shell_octal(b"FMCAMF3\0"),
        );
        fs::write(&helper, script).unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&helper, permissions).unwrap();
        helper
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn malformed_runtime_camera_helper(directory: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let helper = directory.join("malformed-runtime-camera-helper.sh");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    count=0\n    if test -f \"$0.count\"; then read count < \"$0.count\"; fi\n    count=$((count + 1))\n    printf '%s' \"$count\" > \"$0.count\"\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    printf '{}'; sleep 0.15; printf '\\001\\000\\000\\000'; exec sleep 30 ;;\n  *) exit 90 ;;\nesac\n",
            shell_octal(&camera_discovery(0)),
            shell_octal(&camera_frame(7, 1_000)),
        );
        fs::write(&helper, script).unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&helper, permissions).unwrap();
        helper
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn many_camera_startup_failure_helper(directory: &Path, count: usize) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let helper = directory.join("many-camera-startup-failure-helper.sh");
        let records = (0..count)
            .map(|index| {
                (
                    format!("fake-camera-{index}"),
                    format!("Fake Camera {index}"),
                )
            })
            .collect::<Vec<_>>();
        let borrowed = records
            .iter()
            .map(|(id, name)| (id.as_str(), name.as_str()))
            .collect::<Vec<_>>();
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    case \"$2\" in\n      fake-camera-0) printf 'BADFRAME'; exec sleep 30 ;;\n      *) printf '{}'; exec sleep 30 ;;\n    esac ;;\n  *) exit 90 ;;\nesac\n",
            shell_octal(&camera_discovery_for(0, &borrowed)),
            shell_octal(&camera_frame(7, 1_000)),
        );
        fs::write(&helper, script).unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&helper, permissions).unwrap();
        helper
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn two_camera_helper(directory: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let helper = directory.join("two-camera-helper.sh");
        let discovery = camera_discovery_for(
            0,
            &[
                ("fake-camera-a", "Fake Camera A"),
                ("fake-camera-b", "Fake Camera B"),
            ],
        );
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  discover) printf '{}' ;;\n  capture)\n    printf '%s\\n' \"$$\" >> \"$0.pids\"\n    printf '%s\\n' \"$@\" >> \"$0.capture\"\n    case \"$2\" in\n      fake-camera-a)\n        count=0\n        if test -f \"$0.a.count\"; then read count < \"$0.a.count\"; fi\n        count=$((count + 1))\n        printf '%s' \"$count\" > \"$0.a.count\"\n        case \"$count\" in\n          1) printf '{}'; sleep 0.10; exit 20 ;;\n          *) sleep 0.05; printf '{}'; exec sleep 30 ;;\n        esac ;;\n      fake-camera-b) printf '{}'; exec sleep 30 ;;\n      *) exit 89 ;;\n    esac ;;\n  *) exit 90 ;;\nesac\n",
            shell_octal(&discovery),
            shell_octal(&camera_frame(7, 1_000)),
            shell_octal(&camera_frame(0, 2_000)),
            shell_octal(&camera_frame(11, 1_000)),
        );
        fs::write(&helper, script).unwrap();
        let mut permissions = fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&helper, permissions).unwrap();
        helper
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn test_camera_recovery_policy(
        max_attempts: u32,
        rearm_backoff: Duration,
    ) -> CameraRecoveryPolicy {
        CameraRecoveryPolicy {
            max_attempts,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(10),
            rearm_backoff,
            shutdown_timeout: Duration::from_secs(4),
        }
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn helper_process_exists(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn assert_helper_processes_reaped(path: &Path) {
        let pids = fs::read_to_string(path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && pids.lines().any(helper_process_exists) {
            thread::sleep(Duration::from_millis(10));
        }
        for pid in pids.lines() {
            assert!(
                !helper_process_exists(pid),
                "camera helper {pid} survived cleanup"
            );
        }
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn camera_test_project(stable_key: &str) -> StoredProject {
        let frame_rate = FrameRate::new(30_000, 1_001).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(43).unwrap()),
            "Camera Unit Test",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(1, 1).unwrap(),
                    frame_rate,
                    pixel_format: PixelFormat::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ColorMetadata::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::F32,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        project.add_input(Input {
            id: test_input_id(1),
            name: "Camera".into(),
            kind: InputKind::Device {
                stable_key: stable_key.into(),
                capture: fm_types::capture::CaptureSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: test_input_id(2),
            name: "Bars".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
        StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(test_input_id(1)),
                realized_program_id: Some(test_input_id(1)),
                desired_preview_id: Some(test_input_id(2)),
                realized_preview_id: Some(test_input_id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap()
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn two_camera_test_project(first_key: &str, second_key: &str) -> StoredProject {
        let frame_rate = FrameRate::new(30_000, 1_001).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(44).unwrap()),
            "Two Camera Unit Test",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(1, 1).unwrap(),
                    frame_rate,
                    pixel_format: PixelFormat::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ColorMetadata::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::F32,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        for (value, key) in [(1, first_key), (2, second_key)] {
            project.add_input(Input {
                id: test_input_id(value),
                name: format!("Camera {value}"),
                kind: InputKind::Device {
                    stable_key: key.to_owned(),
                    capture: fm_types::capture::CaptureSettings::default(),
                },
                required_capabilities: Vec::new(),
            });
        }
        project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
        StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(test_input_id(1)),
                realized_program_id: Some(test_input_id(1)),
                desired_preview_id: Some(test_input_id(2)),
                realized_preview_id: Some(test_input_id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap()
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn many_camera_test_project(keys: &[String]) -> StoredProject {
        let frame_rate = FrameRate::new(30_000, 1_001).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(45).unwrap()),
            "Many Camera Unit Test",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(1, 1).unwrap(),
                    frame_rate,
                    pixel_format: PixelFormat::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ColorMetadata::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(48_000).unwrap(),
                    sample_format: SampleFormat::F32,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        for (index, key) in keys.iter().enumerate() {
            let value = u128::try_from(index + 1).unwrap();
            project.add_input(Input {
                id: test_input_id(value),
                name: format!("Camera {value}"),
                kind: InputKind::Device {
                    stable_key: key.clone(),
                    capture: fm_types::capture::CaptureSettings::default(),
                },
                required_capabilities: Vec::new(),
            });
        }
        project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
        StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(test_input_id(1)),
                realized_program_id: Some(test_input_id(1)),
                desired_preview_id: Some(test_input_id(2)),
                realized_preview_id: Some(test_input_id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap()
    }

    struct FailingSaver;

    impl DurableStore for FailingSaver {
        fn record(
            &self,
            _command: &CommandMessage,
            _now_millis: u64,
            _previous: &StoredProject,
            _updated: &StoredProject,
        ) -> AppResult<()> {
            Err(AppFailure("injected save failure".into()).into())
        }

        fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
            Err(AppFailure("injected save failure".into()).into())
        }
    }

    struct ObservingSaver<'a> {
        subscription: &'a Subscription,
        saved: RefCell<Option<StoredProject>>,
    }

    impl ObservingSaver<'_> {
        fn observe(&self, project: &StoredProject) {
            assert_eq!(self.subscription.try_recv(), Err(TryRecvError::Empty));
            self.saved.replace(Some(project.clone()));
        }
    }

    impl DurableStore for ObservingSaver<'_> {
        fn record(
            &self,
            _command: &CommandMessage,
            _now_millis: u64,
            _previous: &StoredProject,
            updated: &StoredProject,
        ) -> AppResult<()> {
            self.observe(updated);
            Ok(())
        }

        fn checkpoint(&self, project: &StoredProject) -> AppResult<()> {
            self.observe(project);
            Ok(())
        }
    }

    #[derive(Default)]
    struct CountingSaver(Cell<u32>);

    impl DurableStore for CountingSaver {
        fn record(
            &self,
            _command: &CommandMessage,
            _now_millis: u64,
            _previous: &StoredProject,
            _updated: &StoredProject,
        ) -> AppResult<()> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }

        fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
    }

    struct CrashAfterSave<'a>(&'a ProjectStore);

    impl CrashAfterSave<'_> {
        fn save_then_crash(&self, project: &StoredProject) -> AppResult<()> {
            ProjectStore::save(self.0, project)?;
            panic!("simulated crash after save");
        }
    }

    impl DurableStore for CrashAfterSave<'_> {
        fn record(
            &self,
            _command: &CommandMessage,
            _now_millis: u64,
            _previous: &StoredProject,
            updated: &StoredProject,
        ) -> AppResult<()> {
            self.save_then_crash(updated)
        }

        fn checkpoint(&self, project: &StoredProject) -> AppResult<()> {
            self.save_then_crash(project)
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    /// `serve show.freemix --native-media` plus the given recorder options.
    fn record_args(extra: &[&str]) -> Vec<String> {
        let mut values = strings(&["serve", "show.freemix", "--native-media"]);
        values.extend(extra.iter().map(ToString::to_string));
        values
    }

    #[test]
    fn parses_serve_options() {
        assert_eq!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--listen",
                "127.0.0.1:9123",
                "--once",
            ]))
            .unwrap(),
            Command::Serve {
                project: "show.freemix".into(),
                listen: "127.0.0.1:9123".parse().unwrap(),
                web_listen: None,
                status_listen: None,
                hls_listen: None,
                once: true,
                native_media: false,
                fullscreen_program: false,
                display_outputs: Vec::new(),
                display_inventory: None,
                presenter_fallback: false,
                camera_helper: None,
                capture_inventory: None,
                record_program: None,
                record_output: None,
                record_program_2: None,
                record_output_2: None,
                record_wav: false,
                record_segment_seconds: [None; RECORDER_SLOTS],
                record_bitrate_kbps: None,
                record_bitrate_kbps_2: None,
                record_audio_bitrate_kbps: None,
                record_audio_bitrate_kbps_2: None,
                record_fallback_dir: None,
                diagnostic_stop_after: None,
                recover_to_checkpoint: false,
            }
        );
        let web = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--web-listen",
            "127.0.0.1:9234",
        ]))
        .unwrap();
        assert!(matches!(
            web,
            Command::Serve {
                web_listen: Some(address),
                once: false,
                native_media: false,
                ..
            } if address == "127.0.0.1:9234".parse::<SocketAddr>().unwrap()
        ));
        assert!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--web-listen",
                "127.0.0.1:9234",
                "--once",
            ]))
            .is_err()
        );
        assert!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--web-listen",
                "127.0.0.1:9234",
                "--native-media",
            ]))
            .is_err()
        );
    }

    #[test]
    fn default_serve_is_ephemeral_loopback() {
        let command = parse_args(strings(&["serve", "show.freemix"])).unwrap();
        assert!(matches!(
            command,
            Command::Serve { listen, once: false, native_media: false, .. }
                if listen == DEFAULT_LISTEN.parse::<SocketAddr>().unwrap()
        ));
    }

    #[test]
    fn parses_hls_listen_only_as_a_loopback_native_option() {
        for arguments in [
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen",
                "127.0.0.1:0",
            ]),
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen=127.0.0.1:9432",
            ]),
        ] {
            assert!(matches!(
                parse_args(arguments).unwrap(),
                Command::Serve {
                    hls_listen: Some(address),
                    native_media: true,
                    ..
                } if address.ip().is_loopback()
            ));
        }
        for (arguments, expected) in [
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--hls-listen",
                    "0.0.0.0:9000",
                ]),
                "--hls-listen must use a loopback address",
            ),
            (
                strings(&["serve", "show.freemix", "--hls-listen", "127.0.0.1:9000"]),
                "--hls-listen requires --native-media",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--fullscreen-program",
                    "--hls-listen",
                    "127.0.0.1:9000",
                ]),
                "--hls-listen cannot be combined with --fullscreen-program",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--hls-listen",
                    "127.0.0.1:9001",
                    "--hls-listen",
                    "127.0.0.1:9002",
                ]),
                "duplicate option `--hls-listen`",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--hls-listen=127.0.0.1:9001",
                    "--hls-listen=127.0.0.1:9002",
                ]),
                "duplicate option `--hls-listen`",
            ),
            (
                strings(&["serve", "show.freemix", "--native-media", "--hls-listen"]),
                "missing value for --hls-listen",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
        // Like `--status-listen`, the `=` form parses its value directly, so an
        // empty value is an invalid address rather than a missing argument.
        assert!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--hls-listen=",
            ]))
            .unwrap_err()
            .to_string()
            .contains("invalid hls listen address")
        );
    }

    #[test]
    fn hls_token_bounds_helper_requires_bounded_ascii_graphic_bytes() {
        let short = [b'a'; hls_server::HLS_TOKEN_MIN_BYTES - 1];
        assert!(!hls_server::valid_token(&short));
        let minimum = [b'a'; hls_server::HLS_TOKEN_MIN_BYTES];
        assert!(hls_server::valid_token(&minimum));
        let maximum = [b'a'; hls_server::HLS_TOKEN_MAX_BYTES];
        assert!(hls_server::valid_token(&maximum));
        let long = [b'a'; hls_server::HLS_TOKEN_MAX_BYTES + 1];
        assert!(!hls_server::valid_token(&long));
        for byte in [b' ', b'\n', b'\r', 0x7f] {
            let mut non_graphic = minimum;
            non_graphic[0] = byte;
            assert!(!hls_server::valid_token(&non_graphic), "{byte:#04x}");
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn hls_roots_helper_skips_non_hls_destinations_and_keys_hls_targets() {
        let roots = hls_roots_from_destinations([
            (OutputProtocol::Rtmp, 1, PathBuf::from("/srv/rtmp")),
            (OutputProtocol::Hls, 3, PathBuf::from("/srv/hls-a")),
            (OutputProtocol::Srt, 2, PathBuf::from("/srv/srt")),
            (OutputProtocol::Hls, 4, PathBuf::from("/srv/hls-b")),
        ])
        .unwrap();
        assert_eq!(
            roots.resolve(3, "index.m3u8"),
            Ok(PathBuf::from("/srv/hls-a/index.m3u8"))
        );
        assert_eq!(
            roots.resolve(4, "7/segment-00042.ts"),
            Ok(PathBuf::from("/srv/hls-b/7/segment-00042.ts"))
        );
        assert_eq!(
            roots.resolve(1, "index.m3u8"),
            Err(hls_server::HlsRouteError::NotFound)
        );
        assert_eq!(
            roots.resolve(2, "index.m3u8"),
            Err(hls_server::HlsRouteError::NotFound)
        );
        assert!(matches!(
            hls_roots_from_destinations([
                (OutputProtocol::Hls, 5, PathBuf::from("/srv/one")),
                (OutputProtocol::Hls, 5, PathBuf::from("/srv/two")),
            ]),
            Err(hls_server::HlsRouteError::DuplicateTarget)
        ));
    }

    #[test]
    fn parses_native_media_as_an_opt_in() {
        assert!(matches!(
            parse_args(strings(&["serve", "show.freemix", "--native-media"])).unwrap(),
            Command::Serve {
                native_media: true,
                ..
            }
        ));
    }

    #[test]
    fn rejects_duplicate_listen_option() {
        let error = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--listen",
            "127.0.0.1:4100",
            "--listen",
            "127.0.0.1:4200",
        ]))
        .unwrap_err();
        assert_eq!(error.to_string(), "duplicate option `--listen`");
    }

    #[test]
    fn rejects_duplicate_native_media_option() {
        let error = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--native-media",
        ]))
        .unwrap_err();
        assert_eq!(error.to_string(), "duplicate option `--native-media`");
    }

    #[test]
    fn parses_camera_helper_only_with_native_media() {
        for arguments in [
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--camera-helper",
                "helper-bin",
            ]),
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--camera-helper=helper-bin",
            ]),
        ] {
            assert!(matches!(
                parse_args(arguments).unwrap(),
                Command::Serve {
                    camera_helper: Some(path),
                    ..
                } if path == Path::new("helper-bin")
            ));
        }
        assert_eq!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--camera-helper",
                "helper-bin",
            ]))
            .unwrap_err()
            .to_string(),
            "--camera-helper requires --native-media"
        );
        for arguments in [
            strings(&["serve", "show.freemix", "--native-media", "--camera-helper"]),
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--camera-helper=",
            ]),
        ] {
            assert_eq!(
                parse_args(arguments).unwrap_err().to_string(),
                "missing value for --camera-helper"
            );
        }
        assert_eq!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--camera-helper=a",
                "--camera-helper",
                "b",
            ]))
            .unwrap_err()
            .to_string(),
            "duplicate option `--camera-helper`"
        );
    }

    #[test]
    fn parses_bounded_diagnostic_stop_duration() {
        assert_eq!(
            parse_diagnostic_duration("1500ms").unwrap(),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            parse_diagnostic_duration("2s").unwrap(),
            Duration::from_secs(2)
        );
        assert_eq!(
            parse_diagnostic_duration("3m").unwrap(),
            Duration::from_mins(3)
        );
        assert_eq!(
            parse_diagnostic_duration("24h").unwrap(),
            Duration::from_hours(24)
        );
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--diagnostic-stop-after=10m",
            ]))
            .unwrap(),
            Command::Serve {
                diagnostic_stop_after: Some(duration),
                ..
            } if duration == Duration::from_mins(10)
        ));
    }

    #[test]
    fn rejects_invalid_diagnostic_stop_options() {
        for (arguments, expected) in [
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--once",
                    "--diagnostic-stop-after",
                    "1s",
                ]),
                "--diagnostic-stop-after cannot be combined with --once",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--fullscreen-program",
                    "--diagnostic-stop-after",
                    "1s",
                ]),
                "--diagnostic-stop-after currently supports headless native mode only",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--diagnostic-stop-after",
                    "0s",
                ]),
                "diagnostic duration must be nonzero",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--diagnostic-stop-after",
                    "25h",
                ]),
                "diagnostic duration cannot exceed 24h",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--diagnostic-stop-after",
                    "1s",
                    "--diagnostic-stop-after=2s",
                ]),
                "duplicate option `--diagnostic-stop-after`",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn parses_program_recording_with_once_and_fullscreen() {
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--record-program",
                "capture.mp4",
                "--once",
            ]))
            .unwrap(),
            Command::Serve {
                once: true,
                native_media: true,
                fullscreen_program: true,
                record_program: Some(path),
                ..
            } if path == Path::new("capture.mp4")
        ));
    }

    #[test]
    fn parses_equals_program_recording_with_option_like_file_name() {
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program=--capture.mp4",
            ]))
            .unwrap(),
            Command::Serve {
                record_program: Some(path),
                ..
            } if path == Path::new("--capture.mp4")
        ));
    }

    #[test]
    fn rejects_invalid_program_recording_option_relationships() {
        for (arguments, expected) in [
            (
                strings(&["serve", "show.freemix", "--record-program", "capture.mp4"]),
                "--record-program requires --native-media",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                ]),
                "missing value for --record-program",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "--once",
                ]),
                "missing value for --record-program",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "one.mp4",
                    "--record-program",
                    "two.mp4",
                ]),
                "duplicate option `--record-program`",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program=",
                ]),
                "missing value for --record-program",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program=one.mp4",
                    "--record-program=two.mp4",
                ]),
                "duplicate option `--record-program`",
            ),
            (
                strings(&["serve", "show.freemix", "--native-media", "--record-wav"]),
                "--record-wav requires --record-program or --record-program-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-wav",
                    "--record-wav",
                ]),
                "duplicate option `--record-wav`",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn rejects_invalid_second_program_recording_options() {
        for (arguments, expected) in [
            (
                strings(&["serve", "show.freemix", "--record-program-2", "capture.mp4"]),
                "--record-program-2 requires --native-media",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                ]),
                "missing value for --record-program-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "--once",
                ]),
                "missing value for --record-program-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "one.mp4",
                    "--record-program-2",
                    "two.mp4",
                ]),
                "duplicate option `--record-program-2`",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2=",
                ]),
                "missing value for --record-program-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2=one.mp4",
                    "--record-program-2=two.mp4",
                ]),
                "duplicate option `--record-program-2`",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn parses_second_program_recording_slot() {
        for arguments in [
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
            ]),
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2=backup.mp4",
            ]),
        ] {
            assert!(matches!(
                parse_args(arguments).unwrap(),
                Command::Serve {
                    record_program: None,
                    record_program_2: Some(path),
                    ..
                } if path == Path::new("backup.mp4")
            ));
        }
        // An option-like file name stays reachable through the `=` form.
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2=--backup.mp4",
            ]))
            .unwrap(),
            Command::Serve {
                record_program_2: Some(path),
                ..
            } if path == Path::new("--backup.mp4")
        ));
        // Both slots configure side by side from one desired flag.
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-program-2",
                "backup.mp4",
            ]))
            .unwrap(),
            Command::Serve {
                record_program: Some(primary),
                record_program_2: Some(secondary),
                ..
            } if primary == Path::new("capture.mp4") && secondary == Path::new("backup.mp4")
        ));
    }

    #[test]
    fn parses_wav_sidecar_for_the_second_recorder_slot() {
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-wav",
            ]))
            .unwrap(),
            Command::Serve {
                record_program: None,
                record_program_2: Some(path),
                record_wav: true,
                ..
            } if path == Path::new("backup.mp4")
        ));
    }

    #[test]
    fn parses_program_wav_sidecar_recording() {
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-wav",
            ]))
            .unwrap(),
            Command::Serve {
                record_program: Some(path),
                record_wav: true,
                ..
            } if path == Path::new("capture.mp4")
        ));
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
            ]))
            .unwrap(),
            Command::Serve {
                record_wav: false,
                ..
            }
        ));
    }

    #[test]
    fn parses_record_output_for_both_recorder_slots() {
        let output_a = fm_types::OutputId::new(NonZeroU128::new(10).unwrap());
        let output_b = fm_types::OutputId::new(NonZeroU128::new(20).unwrap());
        for arguments in [
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
                "--record-output",
                "10",
                "--record-program-2",
                "backup.mp4",
                "--record-output-2",
                "20",
            ]),
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program=capture.mp4",
                "--record-output=10",
                "--record-program-2=backup.mp4",
                "--record-output-2=20",
            ]),
        ] {
            assert!(matches!(
                parse_args(arguments).unwrap(),
                Command::Serve {
                    record_program: Some(primary),
                    record_output: Some(primary_output),
                    record_program_2: Some(secondary),
                    record_output_2: Some(secondary_output),
                    ..
                } if primary == Path::new("capture.mp4")
                    && primary_output == output_a
                    && secondary == Path::new("backup.mp4")
                    && secondary_output == output_b
            ));
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn rejects_invalid_record_output_options_and_values() {
        for (arguments, expected) in [
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-output",
                    "1",
                ]),
                "--record-output requires --record-program",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-output-2",
                    "2",
                ]),
                "--record-output-2 requires --record-program-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-output",
                    "1",
                ]),
                "--record-output requires --record-program",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-output",
                ]),
                "missing value for --record-output",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-output",
                    "--once",
                ]),
                "missing value for --record-output",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-output=",
                ]),
                "missing value for --record-output",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-output-2",
                ]),
                "missing value for --record-output-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-output-2",
                    "--once",
                ]),
                "missing value for --record-output-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-output-2=",
                ]),
                "missing value for --record-output-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-output",
                    "1",
                    "--record-output",
                    "2",
                ]),
                "duplicate option `--record-output`",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-output=1",
                    "--record-output=2",
                ]),
                "duplicate option `--record-output`",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-output-2",
                    "1",
                    "--record-output-2",
                    "2",
                ]),
                "duplicate option `--record-output-2`",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-output-2=1",
                    "--record-output-2=2",
                ]),
                "duplicate option `--record-output-2`",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-output",
                    "0",
                ]),
                "invalid value for --record-output `0`; expected a positive integer output id",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-output=abc",
                ]),
                "invalid value for --record-output `abc`; expected a positive integer output id",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-output-2",
                    "0",
                ]),
                "invalid value for --record-output-2 `0`; expected a positive integer output id",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-output-2=-5",
                ]),
                "invalid value for --record-output-2 `-5`; expected a positive integer output id",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn parses_record_segment_seconds_per_slot_with_either_form_and_inclusive_bounds() {
        let seconds = |value| NonZeroU32::new(value).unwrap();
        for (arguments, expected) in [
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds",
                    "1",
                ]),
                [Some(seconds(1)), None],
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds=86400",
                ]),
                [Some(seconds(86_400)), None],
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2",
                    "1",
                ]),
                [None, Some(seconds(1))],
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2=86400",
                ]),
                [None, Some(seconds(86_400))],
            ),
            (
                record_args(&[
                    "--record-program=capture.mp4",
                    "--record-program-2=backup.mp4",
                    "--record-segment-seconds",
                    "300",
                    "--record-segment-seconds-2=600",
                ]),
                [Some(seconds(300)), Some(seconds(600))],
            ),
        ] {
            assert!(matches!(
                parse_args(arguments).unwrap(),
                Command::Serve {
                    record_segment_seconds,
                    ..
                } if record_segment_seconds == expected
            ));
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn rejects_invalid_record_segment_seconds_options_and_values() {
        for (arguments, expected) in [
            (
                record_args(&["--record-segment-seconds", "60"]),
                "--record-segment-seconds requires --record-program",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds",
                    "60",
                ]),
                "--record-segment-seconds requires --record-program",
            ),
            (
                record_args(&["--record-segment-seconds-2", "60"]),
                "--record-segment-seconds-2 requires --record-program-2",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds-2",
                    "60",
                ]),
                "--record-segment-seconds-2 requires --record-program-2",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds",
                ]),
                "missing value for --record-segment-seconds",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds",
                    "--once",
                ]),
                "missing value for --record-segment-seconds",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds=",
                ]),
                "missing value for --record-segment-seconds",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2",
                ]),
                "missing value for --record-segment-seconds-2",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2",
                    "--once",
                ]),
                "missing value for --record-segment-seconds-2",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2=",
                ]),
                "missing value for --record-segment-seconds-2",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds",
                    "10",
                    "--record-segment-seconds",
                    "20",
                ]),
                "duplicate option `--record-segment-seconds`",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds=10",
                    "--record-segment-seconds=20",
                ]),
                "duplicate option `--record-segment-seconds`",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2",
                    "10",
                    "--record-segment-seconds-2",
                    "20",
                ]),
                "duplicate option `--record-segment-seconds-2`",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2=10",
                    "--record-segment-seconds-2=20",
                ]),
                "duplicate option `--record-segment-seconds-2`",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds",
                    "0",
                ]),
                "invalid value for --record-segment-seconds `0`; expected an integer in 1..=86400",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds=86401",
                ]),
                "invalid value for --record-segment-seconds `86401`; expected an integer in 1..=86400",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds=abc",
                ]),
                "invalid value for --record-segment-seconds `abc`; expected an integer in 1..=86400",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2",
                    "0",
                ]),
                "invalid value for --record-segment-seconds-2 `0`; expected an integer in 1..=86400",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2=86401",
                ]),
                "invalid value for --record-segment-seconds-2 `86401`; expected an integer in 1..=86400",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2=abc",
                ]),
                "invalid value for --record-segment-seconds-2 `abc`; expected an integer in 1..=86400",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-segment-seconds=-5",
                ]),
                "invalid value for --record-segment-seconds `-5`; expected an integer in 1..=86400",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-segment-seconds-2=-5",
                ]),
                "invalid value for --record-segment-seconds-2 `-5`; expected an integer in 1..=86400",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn slot_segment_seconds_selects_each_slots_own_rotation_duration() {
        let first_only: [Option<NonZeroU32>; RECORDER_SLOTS] =
            [Some(NonZeroU32::new(10).unwrap()), None];
        assert_eq!(
            slot_segment_seconds(first_only, 0).map(NonZeroU32::get),
            Some(10)
        );
        assert_eq!(slot_segment_seconds(first_only, 1), None);

        let second_only: [Option<NonZeroU32>; RECORDER_SLOTS] =
            [None, Some(NonZeroU32::new(86_400).unwrap())];
        assert_eq!(slot_segment_seconds(second_only, 0), None);
        assert_eq!(
            slot_segment_seconds(second_only, 1).map(NonZeroU32::get),
            Some(86_400)
        );
    }

    #[test]
    fn parses_record_bitrate_options_with_either_form_and_inclusive_bounds() {
        for (arguments, video, video_2, audio, audio_2) in [
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-bitrate-kbps",
                    "2000",
                    "--record-audio-bitrate-kbps",
                    "96",
                ]),
                Some(2_000),
                None,
                Some(96),
                None,
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program-2",
                    "backup.mp4",
                    "--record-bitrate-kbps-2=3000",
                    "--record-audio-bitrate-kbps-2=128",
                ]),
                None,
                Some(3_000),
                None,
                Some(128),
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program=capture.mp4",
                    "--record-program-2=backup.mp4",
                    "--record-bitrate-kbps=1000",
                    "--record-bitrate-kbps-2",
                    "100000",
                    "--record-audio-bitrate-kbps",
                    "8",
                    "--record-audio-bitrate-kbps-2=1024",
                ]),
                Some(MIN_RECORD_VIDEO_BITRATE_KBPS),
                Some(MAX_RECORD_VIDEO_BITRATE_KBPS),
                Some(MIN_RECORD_AUDIO_BITRATE_KBPS),
                Some(MAX_RECORD_AUDIO_BITRATE_KBPS),
            ),
        ] {
            assert!(matches!(
                parse_args(arguments).unwrap(),
                Command::Serve {
                    record_bitrate_kbps,
                    record_bitrate_kbps_2,
                    record_audio_bitrate_kbps,
                    record_audio_bitrate_kbps_2,
                    ..
                } if record_bitrate_kbps == video
                    && record_bitrate_kbps_2 == video_2
                    && record_audio_bitrate_kbps == audio
                    && record_audio_bitrate_kbps_2 == audio_2
            ));
        }
    }

    #[test]
    fn parses_and_validates_the_record_fallback_directory() {
        for flag in [
            &["--record-fallback-dir", "spare"][..],
            &["--record-fallback-dir=spare"][..],
        ] {
            let mut arguments = vec![
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program",
                "capture.mp4",
            ];
            arguments.extend_from_slice(flag);
            assert!(matches!(
                parse_args(strings(&arguments)).unwrap(),
                Command::Serve {
                    record_fallback_dir: Some(directory),
                    ..
                } if directory == Path::new("spare")
            ));
        }
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--record-program-2",
                "backup.mp4",
                "--record-fallback-dir",
                "spare",
            ]))
            .unwrap(),
            Command::Serve {
                record_fallback_dir: Some(_),
                ..
            }
        ));
        for (arguments, expected) in [
            (
                strings(&["serve", "show.freemix", "--record-fallback-dir", "spare"]),
                "--record-fallback-dir requires --record-program or --record-program-2",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-fallback-dir",
                ]),
                "missing value for --record-fallback-dir",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-fallback-dir=",
                ]),
                "missing value for --record-fallback-dir",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--record-program",
                    "capture.mp4",
                    "--record-fallback-dir",
                    "a",
                    "--record-fallback-dir=b",
                ]),
                "duplicate option `--record-fallback-dir`",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--fullscreen-program",
                    "--record-program",
                    "capture.mp4",
                    "--record-fallback-dir",
                    "spare",
                ]),
                "--record-fallback-dir currently supports headless native mode only",
            ),
        ] {
            let error = parse_args(arguments).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recording_sessions_refuse_invalid_stems_and_a_fallback_in_the_output_directory() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("capture.mp4");
        validate_recording_sessions([("--record-program", Some(output.as_path()))], None).unwrap();
        let spaced = directory.path().join("my show.mp4");
        let error =
            validate_recording_sessions([("--record-program-2", Some(spaced.as_path()))], None)
                .unwrap_err()
                .to_string();
        assert!(
            error.starts_with("--record-program-2 output stem must be"),
            "{error}"
        );
        let error = validate_recording_sessions(
            [("--record-program", Some(output.as_path()))],
            Some(directory.path()),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "--record-fallback-dir must differ from the --record-program output directory"
        );
        assert!(!error.contains(&*directory.path().to_string_lossy()));
        validate_recording_sessions(
            [("--record-program", Some(output.as_path()))],
            Some(&directory.path().join("spare")),
        )
        .unwrap();
    }

    #[test]
    fn rejects_record_bitrate_options_without_their_recorder_slot_or_value() {
        for (arguments, expected) in [
            (
                record_args(&["--record-bitrate-kbps", "2000"]),
                "--record-bitrate-kbps requires --record-program",
            ),
            (
                record_args(&["--record-bitrate-kbps-2=2000"]),
                "--record-bitrate-kbps-2 requires --record-program-2",
            ),
            (
                record_args(&["--record-audio-bitrate-kbps", "96"]),
                "--record-audio-bitrate-kbps requires --record-program",
            ),
            (
                record_args(&["--record-audio-bitrate-kbps-2=96"]),
                "--record-audio-bitrate-kbps-2 requires --record-program-2",
            ),
            (
                record_args(&["--record-program", "capture.mp4", "--record-bitrate-kbps"]),
                "missing value for --record-bitrate-kbps",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-bitrate-kbps",
                    "--once",
                ]),
                "missing value for --record-bitrate-kbps",
            ),
            (
                record_args(&["--record-program", "capture.mp4", "--record-bitrate-kbps="]),
                "missing value for --record-bitrate-kbps",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-bitrate-kbps-2",
                ]),
                "missing value for --record-bitrate-kbps-2",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-bitrate-kbps-2=",
                ]),
                "missing value for --record-bitrate-kbps-2",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-audio-bitrate-kbps",
                ]),
                "missing value for --record-audio-bitrate-kbps",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-audio-bitrate-kbps=",
                ]),
                "missing value for --record-audio-bitrate-kbps",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-audio-bitrate-kbps-2",
                ]),
                "missing value for --record-audio-bitrate-kbps-2",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-audio-bitrate-kbps-2=",
                ]),
                "missing value for --record-audio-bitrate-kbps-2",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn rejects_out_of_range_record_bitrate_options() {
        for (arguments, expected) in [
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-bitrate-kbps",
                    "999",
                ]),
                "invalid value for --record-bitrate-kbps `999`; expected an integer in 1000..=100000",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-bitrate-kbps=100001",
                ]),
                "invalid value for --record-bitrate-kbps `100001`; expected an integer in 1000..=100000",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-bitrate-kbps=abc",
                ]),
                "invalid value for --record-bitrate-kbps `abc`; expected an integer in 1000..=100000",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-bitrate-kbps-2",
                    "100001",
                ]),
                "invalid value for --record-bitrate-kbps-2 `100001`; expected an integer in 1000..=100000",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-audio-bitrate-kbps",
                    "7",
                ]),
                "invalid value for --record-audio-bitrate-kbps `7`; expected an integer in 8..=1024",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-audio-bitrate-kbps=1025",
                ]),
                "invalid value for --record-audio-bitrate-kbps `1025`; expected an integer in 8..=1024",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-audio-bitrate-kbps-2=-5",
                ]),
                "invalid value for --record-audio-bitrate-kbps-2 `-5`; expected an integer in 8..=1024",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn rejects_duplicate_record_bitrate_options() {
        for (arguments, expected) in [
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-bitrate-kbps",
                    "2000",
                    "--record-bitrate-kbps",
                    "3000",
                ]),
                "duplicate option `--record-bitrate-kbps`",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-bitrate-kbps=2000",
                    "--record-bitrate-kbps=3000",
                ]),
                "duplicate option `--record-bitrate-kbps`",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-bitrate-kbps-2",
                    "2000",
                    "--record-bitrate-kbps-2=3000",
                ]),
                "duplicate option `--record-bitrate-kbps-2`",
            ),
            (
                record_args(&[
                    "--record-program",
                    "capture.mp4",
                    "--record-audio-bitrate-kbps",
                    "96",
                    "--record-audio-bitrate-kbps=128",
                ]),
                "duplicate option `--record-audio-bitrate-kbps`",
            ),
            (
                record_args(&[
                    "--record-program-2",
                    "backup.mp4",
                    "--record-audio-bitrate-kbps-2=96",
                    "--record-audio-bitrate-kbps-2=128",
                ]),
                "duplicate option `--record-audio-bitrate-kbps-2`",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn segment_frame_limit_rounds_up_to_cover_requested_seconds_at_integer_and_fractional_rates() {
        let sec = |value| NonZeroU32::new(value).unwrap();
        let rate_25 = FrameRate::new(25, 1).unwrap();
        let rate_30 = FrameRate::new(30, 1).unwrap();
        let rate_60 = FrameRate::new(60, 1).unwrap();
        let rate_23_976 = FrameRate::new(24_000, 1_001).unwrap();
        let rate_29_97 = FrameRate::new(30_000, 1_001).unwrap();
        let rate_59_94 = FrameRate::new(60_000, 1_001).unwrap();

        assert_eq!(segment_frame_limit(sec(1), rate_25), 25);
        assert_eq!(segment_frame_limit(sec(2), rate_30), 60);
        assert_eq!(segment_frame_limit(sec(10), rate_60), 600);
        assert_eq!(segment_frame_limit(sec(86_400), rate_60), 86_400 * 60);

        // Fractional NTSC rates round up so the segment spans at least the
        // requested wall duration and hits an exact frame boundary at 1001 s.
        assert_eq!(segment_frame_limit(sec(1), rate_23_976), 24);
        assert_eq!(segment_frame_limit(sec(1), rate_29_97), 30);
        assert_eq!(segment_frame_limit(sec(1), rate_59_94), 60);
        assert_eq!(segment_frame_limit(sec(1_001), rate_23_976), 24_000);
        assert_eq!(segment_frame_limit(sec(1_001), rate_29_97), 30_000);
        assert_eq!(segment_frame_limit(sec(1_001), rate_59_94), 60_000);
    }

    #[test]
    fn recording_output_selection_validation_accepts_known_and_rejects_unknown_outputs() {
        let project = test_project_with_streams([false, false]).project().clone();
        let known_output = fm_types::OutputId::new(NonZeroU128::new(902).unwrap());
        let unknown_output = fm_types::OutputId::new(NonZeroU128::new(999).unwrap());

        validate_recording_output_selections(&project, None, None).unwrap();
        validate_recording_output_selections(&project, Some(known_output), Some(known_output))
            .unwrap();
        assert_eq!(
            validate_recording_output_selections(&project, Some(unknown_output), None)
                .unwrap_err()
                .to_string(),
            "--record-output selects unknown project output 999"
        );
        assert_eq!(
            validate_recording_output_selections(
                &project,
                Some(known_output),
                Some(unknown_output)
            )
            .unwrap_err()
            .to_string(),
            "--record-output-2 selects unknown project output 999"
        );
    }

    #[test]
    fn resolve_project_output_entry_selects_distinct_outputs_and_defaults_to_first() {
        fn resolve<'a>(
            selected: Option<fm_types::OutputId>,
            frames: &'a [(fm_types::OutputId, &'static str)],
            unbound: Option<&'a str>,
        ) -> Option<&'a str> {
            resolve_project_output_entry(
                selected,
                frames,
                unbound,
                |(id, _)| *id,
                |(_, label)| *label,
            )
        }

        let out_1 = fm_types::OutputId::new(NonZeroU128::new(1).unwrap());
        let out_2 = fm_types::OutputId::new(NonZeroU128::new(2).unwrap());
        let out_missing = fm_types::OutputId::new(NonZeroU128::new(3).unwrap());
        let outputs = [(out_1, "program-main"), (out_2, "program-clean")];

        assert_eq!(
            resolve(None, &outputs, Some("unbound")),
            Some("program-main")
        );
        assert_eq!(
            resolve(Some(out_1), &outputs, Some("unbound")),
            Some("program-main")
        );
        assert_eq!(
            resolve(Some(out_2), &outputs, Some("unbound")),
            Some("program-clean")
        );
        assert_eq!(resolve(Some(out_missing), &outputs, Some("unbound")), None);
        assert_eq!(resolve(None, &[], Some("unbound")), Some("unbound"));
        assert_eq!(resolve(Some(out_1), &[], Some("unbound")), Some("unbound"));
        assert_eq!(resolve(None, &[], None), None);
    }

    #[test]
    fn capability_digest_distinguishes_recorder_modes() {
        assert_eq!(
            capabilities_digest(false, false, false),
            CAPABILITIES_DIGEST
        );
        assert_eq!(
            capabilities_digest(true, false, false),
            NATIVE_MEDIA_CAPABILITIES_DIGEST
        );
        assert_eq!(
            capabilities_digest(true, false, true),
            PROGRAM_RECORDER_CAPABILITIES_DIGEST
        );
        assert_eq!(
            capabilities_digest(true, true, false),
            FULLSCREEN_PROGRAM_CAPABILITIES_DIGEST
        );
        assert_eq!(
            capabilities_digest(true, true, true),
            FULLSCREEN_PROGRAM_RECORDER_CAPABILITIES_DIGEST
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_telemetry_diagnostic_names_scoped_metrics_and_unavailable_gpu_time() {
        let origin = Instant::now();
        let adapter = fm_gpu::NativeAdapterInfo {
            name: "Test Adapter".to_owned(),
            backend: NativeBackend::Metal,
        };
        let mut telemetry = NativeRuntimeTelemetry::new(origin, &adapter);
        telemetry.audio_retained_bytes = 10;
        telemetry.audio_peak_retained_bytes = 11;
        telemetry.audio_reservation_requests = 12;
        telemetry.audio_reserved_blocks = 13;
        telemetry.audio_peak_reserved_blocks = 14;
        telemetry.audio_reserved_samples = 15;
        telemetry.audio_peak_reserved_samples = 16;
        telemetry.audio_reserved_bytes = 17;
        telemetry.audio_peak_reserved_bytes = 18;
        telemetry.audio_source_stalls = 19;
        telemetry.audio_positioned_blocks = 20;
        telemetry.audio_positioned_samples = 21;
        telemetry.audio_leading_silence_samples = 22;
        telemetry.audio_eos_padding_blocks = 23;
        telemetry.audio_eos_padding_samples = 24;
        let deadline = origin
            .checked_sub(Duration::from_millis(10))
            .expect("test deadline is representable");
        telemetry.observe_host_lateness(deadline, origin);
        let diagnostic = telemetry.diagnostic(Some(fm_gpu::PresentationTelemetry {
            pending_depth: 1,
            peak_pending_depth: 1,
            frames_dropped: 2,
            ..fm_gpu::PresentationTelemetry::default()
        }));

        assert!(diagnostic.starts_with("FREEMIXD_TELEMETRY\tv=4\t"));
        assert!(diagnostic.contains("\thost_lateness_samples_total=1\t"));
        assert!(diagnostic.contains("\thost_lateness_samples_retained=1\t"));
        assert!(diagnostic.contains("\thost_lateness_p50_ms=10.000\t"));
        assert!(diagnostic.contains("\taudio_retained_bytes=10\t"));
        assert!(diagnostic.contains("\taudio_observed_peak_retained_bytes=11\t"));
        assert!(diagnostic.contains("\taudio_reservation_requests=12\t"));
        assert!(diagnostic.contains("\taudio_reserved_blocks=13\t"));
        assert!(diagnostic.contains("\taudio_observed_peak_reserved_blocks=14\t"));
        assert!(diagnostic.contains("\taudio_reserved_samples=15\t"));
        assert!(diagnostic.contains("\taudio_observed_peak_reserved_samples=16\t"));
        assert!(diagnostic.contains("\taudio_reserved_bytes=17\t"));
        assert!(diagnostic.contains("\taudio_observed_peak_reserved_bytes=18\t"));
        assert!(diagnostic.contains("\taudio_source_stalls=19\t"));
        assert!(diagnostic.contains("\taudio_positioned_blocks=20\t"));
        assert!(diagnostic.contains("\taudio_positioned_samples=21\t"));
        assert!(diagnostic.contains("\taudio_leading_silence_samples=22\t"));
        assert!(diagnostic.contains("\taudio_eos_padding_blocks=23\t"));
        assert!(diagnostic.contains("\taudio_eos_padding_samples=24\t"));
        assert!(diagnostic.contains("\tpresentation_active=true\t"));
        assert!(diagnostic.contains("\tpresentation_pending_depth=1\t"));
        assert!(diagnostic.contains("\tpresentation_dropped=2\t"));
        assert!(diagnostic.contains("\trecorder_configured=false\t"));
        assert!(diagnostic.contains("\tcamera_configured_sources=0\t"));
        assert!(diagnostic.contains("\tcamera_frames_received=0\t"));
        assert!(diagnostic.contains("\tcamera_frames_ingested=0\t"));
        assert!(diagnostic.contains("\tgpu_backend=Metal\t"));
        assert!(diagnostic.contains("\tgpu_adapter=Test Adapter\t"));
        assert!(diagnostic.contains("\tgpu_timing=Unsupported\t"));
        assert!(diagnostic.contains("\tgpu_pass_samples_total=0\t"));
        assert!(diagnostic.contains("\tgpu_pass_samples_retained=0\t"));
        assert!(diagnostic.contains("\tgpu_pass_p50_ms=none\t"));
        assert!(diagnostic.contains("\tmetric_errors=0\t"));
        assert!(diagnostic.contains("\tdisk_alert=unknown\t"));
        assert!(diagnostic.ends_with("\tdisk_free_bytes=0"));
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn disk_alert_label_maps_alert_states() {
        assert_eq!(disk_alert_label(None), "unknown");
        assert_eq!(disk_alert_label(Some(AlertState::Inactive)), "normal");
        assert_eq!(disk_alert_label(Some(AlertState::Firing)), "low");
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    fn camera_source_test_telemetry() -> NativeCameraSourceTelemetry {
        NativeCameraSourceTelemetry {
            input: test_input_id(1),
            lifecycle: LifecycleState::Running,
            health: EndpointHealthState::Healthy,
            frames_received: 20,
            frames_ingested: 6,
            native_dropped: 2,
            queue_dropped: 1,
            queue_depth: 1,
            queue_peak_depth: 2,
            continuity_rejected: 0,
            recovery_timeout_discarded: 0,
            terminal_error_discarded: 1,
            terminal_trigger_discarded: 1,
            ready_delivery_depth: 1,
            ready_delivery_discarded: 1,
            cancellation_discarded: 1,
            supervisor_slot_replaced: 3,
            supervisor_slot_depth: 1,
            ingest_failed: 1,
            preflight_depth: 1,
            preflight_discarded: 1,
            recovery_attempts: 2,
            recovery_successes: 1,
            recovery_exhausted: 0,
            recovery_worker_failures: 0,
            recovery_outcome: CameraRecoveryOutcome::Recovered,
        }
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn camera_source_diagnostics_are_stable_bounded_and_saturating() {
        assert_eq!(lifecycle_label(LifecycleState::Closed), "closed");
        assert_eq!(lifecycle_label(LifecycleState::Open), "open");
        assert_eq!(lifecycle_label(LifecycleState::Running), "running");
        assert_eq!(lifecycle_label(LifecycleState::Lost), "lost");
        assert_eq!(lifecycle_label(LifecycleState::Recovering), "recovering");
        assert_eq!(health_label(EndpointHealthState::Healthy), "healthy");
        assert_eq!(health_label(EndpointHealthState::Degraded), "degraded");
        assert_eq!(health_label(EndpointHealthState::SignalLost), "signal_lost");
        assert_eq!(health_label(EndpointHealthState::Failed), "failed");

        let source = camera_source_test_telemetry();
        let diagnostic = source.diagnostic();
        assert!(diagnostic.starts_with(
            "FREEMIXD_CAMERA_SOURCE\tv=1\tclassification=diagnostic-not-certification\t"
        ));
        assert!(diagnostic.contains("\tsample_phase=pre_cleanup\t"));
        assert!(diagnostic.contains("\tsample_lifecycle=running\thealth=healthy\t"));
        assert!(diagnostic.contains("\trecovery_attempts=2\t"));
        assert!(diagnostic.contains("\tsupervisor_slot_replaced=3\t"));
        assert!(diagnostic.contains("\tsupervisor_slot_depth=1\t"));
        assert!(diagnostic.contains("\tterminal_error_discarded=1\t"));
        assert!(diagnostic.contains("\tterminal_trigger_discarded=1\t"));
        assert!(diagnostic.contains("\tready_delivery_depth=1\t"));
        assert!(diagnostic.contains("\tready_delivery_discarded=1\t"));
        assert!(diagnostic.contains("\tcancellation_discarded=1\t"));
        assert!(diagnostic.contains("\tingest_failed=1\t"));
        assert!(diagnostic.contains("\tpreflight_depth=1\t"));
        assert!(diagnostic.contains("\tpreflight_discarded=1\t"));
        assert!(diagnostic.contains("\trecovery_outcome=recovered"));
        assert!(diagnostic.len() < 768);
        assert_eq!(source.frames_received, source.accounted_frames());

        let mut second = source;
        second.input = test_input_id(2);
        let mut unsorted = [second, source];
        sort_camera_source_telemetry(&mut unsorted);
        assert_eq!(
            unsorted.map(|sample| sample.input),
            [test_input_id(1), test_input_id(2)]
        );

        let mut saturated = source;
        saturated.frames_received = u64::MAX;
        saturated.frames_ingested = u64::MAX;
        saturated.native_dropped = u64::MAX;
        saturated.queue_dropped = u64::MAX;
        saturated.queue_depth = u64::MAX;
        saturated.queue_peak_depth = u64::MAX;
        saturated.continuity_rejected = u64::MAX;
        saturated.recovery_timeout_discarded = u64::MAX;
        saturated.terminal_error_discarded = u64::MAX;
        saturated.terminal_trigger_discarded = u64::MAX;
        saturated.ready_delivery_depth = u64::MAX;
        saturated.ready_delivery_discarded = u64::MAX;
        saturated.cancellation_discarded = u64::MAX;
        saturated.supervisor_slot_replaced = u64::MAX;
        saturated.supervisor_slot_depth = u64::MAX;
        saturated.ingest_failed = u64::MAX;
        saturated.preflight_depth = u64::MAX;
        saturated.preflight_discarded = u64::MAX;
        saturated.recovery_attempts = u64::MAX;
        saturated.recovery_successes = u64::MAX;
        saturated.recovery_exhausted = u64::MAX;
        saturated.recovery_worker_failures = u64::MAX;
        let aggregate = aggregate_camera_telemetry(&[saturated, source]);
        assert_eq!(aggregate.configured_sources, 2);
        assert_eq!(aggregate.frames_received, u64::MAX);
        assert_eq!(aggregate.frames_ingested, u64::MAX);
        assert_eq!(aggregate.native_dropped, u64::MAX);
        assert_eq!(aggregate.queue_dropped, u64::MAX);
        assert_eq!(aggregate.queue_depth, u64::MAX);
        assert_eq!(aggregate.queue_peak_depth, u64::MAX);
        assert_eq!(aggregate.continuity_rejected, u64::MAX);
        assert_eq!(aggregate.recovery_timeout_discarded, u64::MAX);
        assert_eq!(aggregate.terminal_error_discarded, u64::MAX);
        assert_eq!(aggregate.terminal_trigger_discarded, u64::MAX);
        assert_eq!(aggregate.ready_delivery_depth, u64::MAX);
        assert_eq!(aggregate.ready_delivery_discarded, u64::MAX);
        assert_eq!(aggregate.cancellation_discarded, u64::MAX);
        assert_eq!(aggregate.supervisor_slot_replaced, u64::MAX);
        assert_eq!(aggregate.supervisor_slot_depth, u64::MAX);
        assert_eq!(aggregate.ingest_failed, u64::MAX);
        assert_eq!(aggregate.preflight_depth, u64::MAX);
        assert_eq!(aggregate.preflight_discarded, u64::MAX);
        assert_eq!(aggregate.recovery_attempts, u64::MAX);
        assert_eq!(aggregate.recovery_successes, u64::MAX);
        assert_eq!(aggregate.recovery_exhausted, u64::MAX);
        assert_eq!(aggregate.recovery_worker_failures, u64::MAX);
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn camera_latest_slot_carries_discontinuity_across_replacement_race() {
        let mut slot = CameraFrameSlot::default();
        slot.replace(camera_slot_test_frame(8, true));
        slot.replace(camera_slot_test_frame(9, false));
        slot.replace(camera_slot_test_frame(10, false));

        let newest = slot.take().unwrap();
        assert_eq!(newest.timing().sequence().get(), 10);
        assert!(
            newest
                .timing()
                .flags()
                .contains(fm_frame::MediaFlags::DISCONTINUITY)
        );
        assert_eq!(slot.replacements, 2);
        assert!(slot.take().is_none());
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn camera_ingest_failure_is_an_exact_accounted_drop() {
        let mut frame = CameraFrameSlot::default();
        frame.replace(camera_slot_test_frame(7, false));
        let supervisor = Arc::new(Mutex::new(CameraSupervisorState {
            frame,
            snapshot: CameraWorkerSnapshot {
                telemetry: CameraTelemetry {
                    received: 1,
                    ..CameraTelemetry::default()
                },
                lifecycle: LifecycleState::Running,
                health: EndpointHealthState::Healthy,
                recovery_attempts: 0,
                recovery_successes: 0,
                recovery_exhausted: 0,
                recovery_worker_failures: 0,
                recovery_outcome: CameraRecoveryOutcome::Never,
            },
        }));
        let mut cameras = NativeCameraInputs {
            inputs: vec![NativeCameraInput {
                input: test_input_id(1),
                source: None,
                worker: None,
                supervisor,
                recovery_policy: test_camera_recovery_policy(1, Duration::from_millis(10)),
                ingested_frames: 0,
                ingest_failed: 0,
                preflight_depth: 0,
                preflight_discarded: 0,
                last_ingested_sequence: None,
                last_ingested_discontinuity: false,
            }],
            telemetry_emitted: false,
        };

        let error = cameras
            .poll_with(|_, _| Err(AppFailure("injected ingest failure".to_owned()).into()))
            .unwrap_err();
        assert_eq!(error.to_string(), "injected ingest failure");
        let telemetry = cameras.source_telemetry()[0];
        assert_eq!(telemetry.frames_received, 1);
        assert_eq!(telemetry.frames_ingested, 0);
        assert_eq!(telemetry.ingest_failed, 1);
        assert_eq!(telemetry.queue_depth, 0);
        assert_eq!(telemetry.supervisor_slot_depth, 0);
        assert_eq!(telemetry.frames_received, telemetry.accounted_frames());
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recorder_capability_is_configured_support_not_live_health() {
        let digest = capabilities_digest(true, false, true);
        let mut policy = RecorderCapturePolicy::default();
        assert!(policy.fail("backend:failed".to_owned(), false).is_some());
        assert_eq!(digest, capabilities_digest(true, false, true));
        assert!(!policy.active());
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn startup_pair_barrier_requires_mux_output_and_prioritizes_failure() {
        assert_eq!(
            startup_pair_decision(RecorderState::Recording, 0, 0, false),
            StartupPairDecision::Pending
        );
        assert_eq!(
            startup_pair_decision(RecorderState::Recording, 1, 0, false),
            StartupPairDecision::Pending
        );
        assert_eq!(
            startup_pair_decision(RecorderState::Recording, 1, 1, false),
            StartupPairDecision::Ready
        );
        assert_eq!(
            startup_pair_decision(RecorderState::Recording, 1, 1, true),
            StartupPairDecision::Failed
        );
        assert_eq!(
            startup_pair_decision(RecorderState::Failed, 1, 1, true),
            StartupPairDecision::Failed
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn next_record_segment_path_prefers_the_original_and_skips_existing_names() {
        let directory = tempfile::tempdir().unwrap();
        let base = |name: &str| directory.path().join(name);
        let exists = |path: &Path| path.exists();

        // Segment 1 is the configured path itself.
        assert_eq!(
            next_record_segment_path(&base("show.mp4"), exists).unwrap(),
            base("show.mp4")
        );
        std::fs::write(base("show.mp4"), b"").unwrap();
        // Later segments are zero-padded `<stem>-<NNN>.mp4`, first free wins.
        assert_eq!(
            next_record_segment_path(&base("show.mp4"), exists).unwrap(),
            base("show-002.mp4")
        );
        std::fs::write(base("show-002.mp4"), b"").unwrap();
        assert_eq!(
            next_record_segment_path(&base("show.mp4"), exists).unwrap(),
            base("show-003.mp4")
        );
        // Gaps are filled by the first free name, not the last.
        std::fs::create_dir_all(directory.path().join("nested")).unwrap();
        let nested = |name: &str| directory.path().join("nested").join(name);
        for name in ["take.mp4", "take-002.mp4", "take-004.mp4"] {
            std::fs::write(nested(name), b"").unwrap();
        }
        assert_eq!(
            next_record_segment_path(&nested("take.mp4"), exists).unwrap(),
            nested("take-003.mp4")
        );
        // Compound stems keep everything before the final extension.
        std::fs::write(base("a.b.mp4"), b"").unwrap();
        assert_eq!(
            next_record_segment_path(&base("a.b.mp4"), exists).unwrap(),
            base("a.b-002.mp4")
        );
        // Non-.mp4 outputs and unnamed paths are refused outright.
        for refused in ["show.mkv", "show", ".mp4"] {
            assert!(
                next_record_segment_path(&base(refused), exists).is_err(),
                "{refused} must be refused"
            );
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn next_record_segment_path_refuses_exhaustion() {
        const LAST: u32 = RECORD_SEGMENT_LAST_ORDINAL;
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("show.mp4");
        std::fs::write(&base, b"").unwrap();
        for ordinal in 2..=LAST {
            std::fs::write(directory.path().join(format!("show-{ordinal:03}.mp4")), b"").unwrap();
        }
        let error = next_record_segment_path(&base, Path::exists).unwrap_err();
        assert!(error.to_string().contains("exhausted"));
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn second_recorder_slot_derives_segments_from_its_own_configured_path() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("program.mp4");
        let secondary = directory.path().join("backup.mp4");
        std::fs::write(&primary, b"").unwrap();
        std::fs::write(&secondary, b"").unwrap();
        let derive = |slot| {
            next_slot_segment_path(slot, Some(&primary), Some(&secondary), Path::exists).unwrap()
        };
        assert_eq!(derive(0).unwrap(), directory.path().join("program-002.mp4"));
        // Slot 1 derives from `--record-program-2` alone, so two recorders
        // writing into one directory fill their own gaps.
        assert_eq!(derive(1).unwrap(), directory.path().join("backup-002.mp4"));
        // An unconfigured slot derives nothing instead of borrowing the
        // other slot's base path.
        assert_eq!(
            next_slot_segment_path(1, Some(&primary), None, Path::exists).unwrap(),
            None
        );
        assert_eq!(
            next_slot_segment_path(0, None, Some(&secondary), Path::exists).unwrap(),
            None
        );
    }

    #[test]
    fn recording_parent_falls_back_to_the_working_directory() {
        assert_eq!(recording_parent(Path::new("capture.mp4")), Path::new("."));
        assert_eq!(
            recording_parent(Path::new("out/capture.mp4")),
            Path::new("out")
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn record_wav_sidecar_shares_each_segment_stem() {
        let directory = tempfile::tempdir().unwrap();
        let names = recording_session::SegmentNames::new(None);
        for (segment, sidecar) in [
            ("program.mp4", "program.wav"),
            ("program-002.mp4", "program-002.wav"),
        ] {
            let planned = recording_session::plan(
                &directory.path().join(segment),
                directory.path(),
                &names,
                1,
                1,
                Some(1),
            )
            .unwrap();
            assert_eq!(
                planned.wav_config().unwrap().segment_path(0, 1).unwrap(),
                directory.path().join(sidecar)
            );
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recorded_duration_is_exact_for_integer_and_fractional_rates() {
        let thirty = fm_types::FrameRate::new(30, 1).unwrap();
        assert_eq!(recorded_duration_ms(0, thirty), 0);
        assert_eq!(recorded_duration_ms(90, thirty), 3_000);
        let drop_frame = fm_types::FrameRate::new(30_000, 1_001).unwrap();
        assert_eq!(recorded_duration_ms(30, drop_frame), 1_001);
        assert_eq!(recorded_duration_ms(60, drop_frame), 2_002);
        let sixty = fm_types::FrameRate::new(60, 1).unwrap();
        assert_eq!(recorded_duration_ms(1, sixty), 16);
        assert_eq!(recorded_duration_ms(u64::MAX, thirty), u64::MAX);
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recording_start_timecode_derives_components_at_integer_and_fractional_rates() {
        let components = |sequence: u64, rate: fm_types::FrameRate| {
            recording_start_timecode(sequence, rate).map(fm_types::Timecode::components)
        };
        let thirty = fm_types::FrameRate::new(30, 1).unwrap();
        assert_eq!(components(0, thirty), Some((0, 0, 0, 0)));
        assert_eq!(components(29, thirty), Some((0, 0, 0, 29)));
        assert_eq!(components(30, thirty), Some((0, 0, 1, 0)));
        assert_eq!(components(30 * 3_600, thirty), Some((1, 0, 0, 0)));
        // The hour component wraps modulo 24 at exactly 24 hours and at 25
        // hours, whose minute, second, and frame components restart at zero.
        assert_eq!(components(30 * 3_600 * 24, thirty), Some((0, 0, 0, 0)));
        assert_eq!(components(30 * 3_600 * 25, thirty), Some((1, 0, 0, 0)));

        // Fractional NTSC rates derive the rounded nominal integer cadence.
        let rate_29_97 = fm_types::FrameRate::new(30_000, 1_001).unwrap();
        let rate_23_976 = fm_types::FrameRate::new(24_000, 1_001).unwrap();
        let rate_59_94 = fm_types::FrameRate::new(60_000, 1_001).unwrap();
        assert_eq!(components(29, rate_29_97), Some((0, 0, 0, 29)));
        assert_eq!(components(30, rate_29_97), Some((0, 0, 1, 0)));
        assert_eq!(components(23, rate_23_976), Some((0, 0, 0, 23)));
        assert_eq!(components(24, rate_23_976), Some((0, 0, 1, 0)));
        assert_eq!(components(59, rate_59_94), Some((0, 0, 0, 59)));
        assert_eq!(components(60, rate_59_94), Some((0, 0, 1, 0)));
        assert_eq!(components(30 * 3_600, rate_29_97), Some((1, 0, 0, 0)));
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn stream_encoder_settings_carry_the_authored_video_bitrate() {
        use fm_model::{StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId};

        let target = StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(7_100).unwrap()),
            "Bitrate".to_owned(),
            StreamProtocol::Rtmp,
            StreamEndpoint::parse("ingest.example.com/live").unwrap(),
            Some(StreamKey::parse("unit-secret-key").unwrap()),
            fm_types::OutputId::new(NonZeroU128::new(7_101).unwrap()),
        )
        .unwrap();
        let authored = target.clone().with_video_bitrate(9_000).unwrap();
        assert_ne!(authored.video_bitrate_kbps(), target.video_bitrate_kbps());
        assert_eq!(
            stream_encoder_settings(authored.video_bitrate_kbps()).video_bitrate_kbps,
            9_000
        );
        // The model default maps onto the sink default unchanged.
        let settings = stream_encoder_settings(target.video_bitrate_kbps());
        assert_eq!(
            settings.video_bitrate_kbps,
            EncoderSettings::default().video_bitrate_kbps
        );
        assert_eq!(
            settings.audio_bitrate_kbps,
            EncoderSettings::default().audio_bitrate_kbps
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn stream_ladder_maps_authored_rungs_and_ignores_keyed_destinations() {
        use fm_model::{
            StreamEndpoint, StreamKey, StreamLadder, StreamLadderRung, StreamProtocol,
            StreamTarget, StreamTargetId,
        };

        let output = fm_types::OutputId::new(NonZeroU128::new(7_111).unwrap());
        let (protocol, endpoint) = StreamEndpoint::parse_url("hls:///srv/freemix/live").unwrap();
        let ladder = StreamLadder::new(vec![
            StreamLadderRung::new(1_000, 640, 360).unwrap(),
            StreamLadderRung::new(2_500, 1_280, 720).unwrap(),
        ])
        .unwrap();
        let laddered = StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(7_110).unwrap()),
            "Ladder".to_owned(),
            protocol,
            endpoint,
            None,
            output,
        )
        .unwrap()
        .with_ladder(Some(ladder))
        .unwrap();
        let mapped = stream_ladder(&laddered).unwrap();
        assert_eq!(mapped.rungs().len(), 2);
        assert_eq!(mapped.top_bitrate_kbps(), 2_500);
        assert_eq!(mapped.rungs()[0].video_bitrate_kbps, 1_000);
        assert_eq!(mapped.rungs()[0].width, 640);
        assert_eq!(mapped.rungs()[0].height, 360);
        assert_eq!(mapped.rungs()[1].video_bitrate_kbps, 2_500);
        assert_eq!(mapped.rungs()[1].width, 1_280);
        assert_eq!(mapped.rungs()[1].height, 720);
        // The base bitrate a laddered target advertises is its top rung.
        assert_eq!(laddered.video_bitrate_kbps(), 2_500);

        let keyed = StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(7_112).unwrap()),
            "Plain".to_owned(),
            StreamProtocol::Rtmp,
            StreamEndpoint::parse("ingest.example.com/live").unwrap(),
            Some(StreamKey::parse("unit-secret-key").unwrap()),
            output,
        )
        .unwrap();
        assert!(stream_ladder(&keyed).is_none());
    }

    /// Work that is not durable is never acknowledged.
    ///
    /// The journal is the only thing standing between an accepted command and
    /// its acknowledgement, so while it cannot record, every command must come
    /// back refused and retryable with no revision consumed, the daemon must
    /// stay up and keep serving, and once recording works again the next
    /// command takes the very next revision. The fault stands in for a failing
    /// or full show disk at the exact seam the daemon hands its journal; file
    /// surgery under a running daemon cannot inject it, because the journal
    /// database is opened once, at startup, and held for the daemon's run.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn unavailable_journal_refuses_commands_and_the_session_keeps_serving() {
        struct SwitchableJournal {
            available: Arc<AtomicBool>,
            recorded: AtomicU64,
        }

        impl DurableStore for SwitchableJournal {
            fn record(
                &self,
                _command: &CommandMessage,
                _now_millis: u64,
                _previous: &StoredProject,
                _updated: &StoredProject,
            ) -> AppResult<()> {
                if !self.available.load(Ordering::SeqCst) {
                    return Err(AppFailure("injected journal unavailability".into()).into());
                }
                self.recorded.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }

            fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
                if !self.available.load(Ordering::SeqCst) {
                    return Err(AppFailure("injected journal unavailability".into()).into());
                }
                Ok(())
            }
        }

        fn next_command_result(reader: &mut MessageReader) -> CommandResult {
            loop {
                match reader.read_message_with_idle(|| Ok(false)).unwrap() {
                    Some(WireMessage::CommandResult(result)) => return result,
                    Some(_) => {}
                    None => panic!("expected a command result"),
                }
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let available = Arc::new(AtomicBool::new(true));
        let journal_available = Arc::clone(&available);
        let (served_tx, served_rx) = std::sync::mpsc::sync_channel(0);
        let server_thread = thread::spawn(move || {
            let journal = SwitchableJournal {
                available: journal_available,
                recorded: AtomicU64::new(0),
            };
            let mut durable = test_project();
            let project_id = durable.project().id();
            let control = Rc::new(RefCell::new(test_control(&durable)));
            let authority = control_server_identity(&control.borrow(), project_id);
            let config = ServerConfig::new(
                ServerMode::Development,
                AuthenticationMode::Development,
                address.ip(),
                CAPABILITIES_DIGEST,
            );
            let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
            server.mark_ready().unwrap();
            let principal = development_principal().unwrap();

            let (stream, _) = listener.accept().unwrap();
            handle_client(
                stream,
                &server,
                &control,
                &journal,
                &mut durable,
                &principal,
                &authority,
                None,
                None,
                &mut OnceClientOutcome::Unserved,
            )
            .unwrap();
            served_tx.send(()).unwrap();
        });

        let stream = TcpStream::connect(address).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let (mut reader, _) = complete_test_handshake(&stream);

        write_message(
            &mut writer,
            &WireMessage::Command(test_command(
                "durable-cut",
                "durable-cut-key",
                CommandPayload::Cut,
            )),
        )
        .unwrap();
        assert!(matches!(
            next_command_result(&mut reader),
            CommandResult::Accepted { revision: 1, .. }
        ));

        available.store(false, Ordering::SeqCst);
        for id in ["refused-first", "refused-second"] {
            write_message(
                &mut writer,
                &WireMessage::Command(test_command(id, &format!("{id}-key"), CommandPayload::Cut)),
            )
            .unwrap();
            match next_command_result(&mut reader) {
                CommandResult::Rejected {
                    id: rejected,
                    code,
                    current_revision,
                    retryable,
                    ..
                } => {
                    assert_eq!(rejected, id);
                    assert_eq!(code, "unavailable");
                    assert_eq!(current_revision, 1, "a refused command takes no revision");
                    assert!(retryable);
                }
                CommandResult::Accepted { revision, .. } => {
                    panic!("{id} must not be acknowledged, yet it took revision {revision}")
                }
            }
        }

        available.store(true, Ordering::SeqCst);
        write_message(
            &mut writer,
            &WireMessage::Command(test_command(
                "after-repair",
                "after-repair-key",
                CommandPayload::Cut,
            )),
        )
        .unwrap();
        assert!(
            matches!(
                next_command_result(&mut reader),
                CommandResult::Accepted { revision: 2, .. }
            ),
            "the refused commands left no gap in the revision or the journal"
        );
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        while reader
            .read_message_with_idle(|| Ok(false))
            .unwrap()
            .is_some()
        {}
        served_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        server_thread.join().unwrap();
    }

    #[test]
    fn client_socket_configuration_sets_read_and_write_timeouts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let _client = TcpStream::connect(address).unwrap();
        let (server_stream, _) = listener.accept().unwrap();

        configure_client_socket(&server_stream).unwrap();

        // The kernel may round a socket timeout up to its timer granularity
        // (jiffies on Linux, for example 4 ms ticks), so each configured value
        // is bounded by the request plus one coarse tick rather than compared
        // exactly.
        let read_timeout = server_stream.read_timeout().unwrap().unwrap();
        assert!(read_timeout >= CLIENT_READ_POLL_INTERVAL);
        assert!(read_timeout <= CLIENT_READ_POLL_INTERVAL + Duration::from_millis(10));
        let write_timeout = server_stream.write_timeout().unwrap().unwrap();
        assert!(write_timeout >= CLIENT_WRITE_TIMEOUT);
        assert!(write_timeout <= CLIENT_WRITE_TIMEOUT + Duration::from_millis(10));
    }

    #[test]
    fn client_write_timeouts_are_disconnects() {
        for kind in [std::io::ErrorKind::TimedOut, std::io::ErrorKind::WouldBlock] {
            let error = client_write_error(std::io::Error::from(kind));
            assert!(is_client_disconnect(error.as_ref()));
        }
    }

    fn complete_test_handshake(stream: &TcpStream) -> (MessageReader, ServerIdentity) {
        write_message(
            &mut stream.try_clone().unwrap(),
            &WireMessage::HandshakeRequest(HandshakeRequest {
                protocol: PROTOCOL_VERSION,
                build: "control-timeout-test".into(),
                client_type: ClientType::Integration,
                desired_role: Role::Operator,
                resume_cursor: None,
            }),
        )
        .unwrap();
        let mut reader = MessageReader::new(stream.try_clone().unwrap());
        let Some(WireMessage::HandshakeResponse(response)) =
            reader.read_message_with_idle(|| Ok(false)).unwrap()
        else {
            panic!("expected handshake response");
        };
        assert!(matches!(
            reader.read_message_with_idle(|| Ok(false)).unwrap(),
            Some(WireMessage::Snapshot(_))
        ));
        (reader, response.server)
    }

    #[test]
    fn incomplete_pre_handshake_peer_releases_accept_loop() {
        const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(100);

        let directory = tempfile::tempdir().unwrap();
        let project_path = directory.path().join("show.freemix");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::sync_channel(1);
        let (expired_tx, expired_rx) = std::sync::mpsc::sync_channel(1);
        let server_thread = thread::spawn(move || {
            let store = ProjectStore::new(project_path).unwrap();
            let mut durable = test_project();
            store.save(&durable).unwrap();
            let journal = DurableJournal::new(store.open_journal_writer().unwrap());
            let project_id = durable.project().id();
            let control = Rc::new(RefCell::new(test_control(&durable)));
            let authority = control_server_identity(&control.borrow(), project_id);
            let config = ServerConfig::new(
                ServerMode::Development,
                AuthenticationMode::Development,
                address.ip(),
                CAPABILITIES_DIGEST,
            )
            .with_session_limits(fm_server::SessionLimits {
                heartbeat_timeout_ms: u64::try_from(HANDSHAKE_TIMEOUT.as_millis()).unwrap(),
                ..fm_server::SessionLimits::default()
            });
            let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
            server.mark_ready().unwrap();
            let principal = development_principal().unwrap();

            for client_index in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                if client_index == 0 {
                    accepted_tx.send(Instant::now()).unwrap();
                }
                handle_client(
                    stream,
                    &server,
                    &control,
                    &journal,
                    &mut durable,
                    &principal,
                    &authority,
                    None,
                    None,
                    &mut OnceClientOutcome::Unserved,
                )
                .unwrap();
                if client_index == 0 {
                    expired_tx.send(Instant::now()).unwrap();
                }
            }
        });

        let mut incomplete_peer = TcpStream::connect(address).unwrap();
        let accepted_at = accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let deadline_lower_bound = accepted_at.checked_add(HANDSHAKE_TIMEOUT).unwrap();
        let mut wrote_partial_bytes = false;
        while Instant::now() < deadline_lower_bound {
            assert_eq!(incomplete_peer.write(b"x").unwrap(), 1);
            wrote_partial_bytes = true;
            thread::sleep(Duration::from_millis(10));
        }
        assert!(wrote_partial_bytes);
        let expired_at = expired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(expired_at >= deadline_lower_bound);
        let next_peer = TcpStream::connect(address).unwrap();
        next_peer
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        complete_test_handshake(&next_peer);

        drop(next_peer);
        server_thread.join().unwrap();
    }

    #[test]
    fn outbound_rate_limited_client_releases_accept_loop() {
        let directory = tempfile::tempdir().unwrap();
        let project_path = directory.path().join("show.freemix");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (terminated_tx, terminated_rx) = std::sync::mpsc::sync_channel(1);
        let (completed_tx, completed_rx) = std::sync::mpsc::sync_channel(1);
        let server_thread = thread::spawn(move || {
            let store = ProjectStore::new(project_path).unwrap();
            let mut durable = test_project();
            store.save(&durable).unwrap();
            let journal = DurableJournal::new(store.open_journal_writer().unwrap());
            let project_id = durable.project().id();
            let control = Rc::new(RefCell::new(test_control(&durable)));
            let authority = control_server_identity(&control.borrow(), project_id);
            let config = ServerConfig::new(
                ServerMode::Development,
                AuthenticationMode::Development,
                address.ip(),
                CAPABILITIES_DIGEST,
            )
            .with_session_limits(fm_server::SessionLimits {
                outbound_messages: fm_server::RateLimit::new(2, 60_000),
                ..fm_server::SessionLimits::default()
            });
            let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
            server.mark_ready().unwrap();
            let principal = development_principal().unwrap();

            let (stream, _) = listener.accept().unwrap();
            let error = handle_client(
                stream,
                &server,
                &control,
                &journal,
                &mut durable,
                &principal,
                &authority,
                None,
                None,
                &mut OnceClientOutcome::Unserved,
            )
            .unwrap_err();
            assert!(!is_client_disconnect(error.as_ref()));
            assert!(!is_client_protocol_error(error.as_ref()));
            assert!(is_client_session_termination(error.as_ref()));
            terminated_tx.send(()).unwrap();

            let (stream, _) = listener.accept().unwrap();
            handle_client(
                stream,
                &server,
                &control,
                &journal,
                &mut durable,
                &principal,
                &authority,
                None,
                None,
                &mut OnceClientOutcome::Unserved,
            )
            .unwrap();
            completed_tx.send(()).unwrap();
        });

        let client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let (mut reader, mut server) = complete_test_handshake(&client);
        server.project_id = "wrong-project".into();
        write_message(
            &mut client.try_clone().unwrap(),
            &WireMessage::Heartbeat(HeartbeatMessage {
                server,
                sequence: 1,
                sent_at_ms: 1_234,
                last_applied: None,
            }),
        )
        .unwrap();
        terminated_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut eof = [0_u8; 1];
        assert_eq!(reader.stream.read(&mut eof).unwrap(), 0);
        drop(client);

        let next_client = TcpStream::connect(address).unwrap();
        next_client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        next_client
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        complete_test_handshake(&next_client);
        drop(next_client);

        completed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        server_thread.join().unwrap();
    }

    #[test]
    fn expired_tcp_session_is_reclaimed_for_next_client() {
        const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(600);
        const HEARTBEAT_DELAY: Duration = Duration::from_millis(200);
        // The server expires sessions on absolute wall-clock deadlines while
        // this measurement is monotonic, so a small system-clock step may
        // legitimately fire expiry early against this instant.
        const CLOCK_SKEW_TOLERANCE: Duration = Duration::from_millis(250);

        let directory = tempfile::tempdir().unwrap();
        let project_path = directory.path().join("show.freemix");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (expired_tx, expired_rx) = std::sync::mpsc::sync_channel(1);
        let server_thread = thread::spawn(move || {
            let store = ProjectStore::new(project_path).unwrap();
            let mut durable = test_project();
            store.save(&durable).unwrap();
            let journal = DurableJournal::new(store.open_journal_writer().unwrap());
            let project_id = durable.project().id();
            let control = Rc::new(RefCell::new(test_control(&durable)));
            let authority = control_server_identity(&control.borrow(), project_id);
            let config = ServerConfig::new(
                ServerMode::Development,
                AuthenticationMode::Development,
                address.ip(),
                CAPABILITIES_DIGEST,
            )
            .with_session_limits(fm_server::SessionLimits {
                heartbeat_timeout_ms: u64::try_from(HEARTBEAT_TIMEOUT.as_millis()).unwrap(),
                ..fm_server::SessionLimits::default()
            });
            let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
            server.mark_ready().unwrap();
            let principal = development_principal().unwrap();

            for client_index in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                handle_client(
                    stream,
                    &server,
                    &control,
                    &journal,
                    &mut durable,
                    &principal,
                    &authority,
                    None,
                    None,
                    &mut OnceClientOutcome::Unserved,
                )
                .unwrap();
                if client_index == 0 {
                    expired_tx.send(()).unwrap();
                }
            }
        });

        let client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let (mut reader, server) = complete_test_handshake(&client);
        let original_deadline = Instant::now() + HEARTBEAT_TIMEOUT;
        assert!(matches!(
            expired_rx.recv_timeout(HEARTBEAT_DELAY),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));

        let heartbeat_sent_at = Instant::now();
        write_message(
            &mut client.try_clone().unwrap(),
            &WireMessage::Heartbeat(HeartbeatMessage {
                server: server.clone(),
                sequence: 1,
                sent_at_ms: 1_234,
                last_applied: None,
            }),
        )
        .unwrap();
        let Some(WireMessage::HeartbeatAcknowledgement(acknowledgement)) =
            reader.read_message_with_idle(|| Ok(false)).unwrap()
        else {
            panic!("expected heartbeat acknowledgement");
        };
        assert_eq!(acknowledgement.server, server);
        assert_eq!(acknowledgement.heartbeat_sequence, 1);

        assert!(matches!(
            expired_rx.recv_timeout(original_deadline.saturating_duration_since(Instant::now())),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(Instant::now() >= original_deadline);
        expired_rx.recv_timeout(HEARTBEAT_TIMEOUT).unwrap();
        assert!(
            heartbeat_sent_at.elapsed() + CLOCK_SKEW_TOLERANCE >= HEARTBEAT_TIMEOUT,
            "the session expired before its heartbeat deadline"
        );

        let next_client = TcpStream::connect(address).unwrap();
        next_client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        complete_test_handshake(&next_client);
        drop(next_client);

        server_thread.join().unwrap();
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recorder_capture_failure_policy_formats_one_sanitized_notice() {
        let mut policy = RecorderCapturePolicy::default();
        let Some((failure, app_capture_failure)) =
            policy.fail("readback:first\tline\nend".to_owned(), true)
        else {
            panic!("first failure must emit a notice");
        };
        assert_eq!(
            recorder_failure_notice(2, failure, app_capture_failure),
            "FREEMIXD_RECORDER_FAILURE\tv=1\tslot=2\tapp_capture_failure=true\tfailure=readback:first line end",
        );
        assert!(policy.fail("enqueue:second".to_owned(), false).is_none());
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recorder_report_annotates_the_slot_immediately_after_the_version() {
        let report = StopReport {
            outcome: StopOutcome::Clean,
            exit_status: Some(0),
            output: OutputFinalization::Synced,
            cleanup: CleanupStatus::Complete,
            wav_bytes: 0,
            telemetry: fm_codec_ffmpeg::record::RecordTelemetry {
                state: RecorderState::Stopped,
                failure: None,
                accepted_pairs: 30,
                completed_pairs: 30,
                failed_pairs: 0,
                outstanding_pairs: 0,
                retained_bytes: 0,
                output_bytes: 1_024,
                wav_bytes: 512,
                stderr_tail: String::new(),
                stderr_truncated: false,
            },
        };
        assert_eq!(
            recorder_report(1, &report, 1_000, 2_048, false, "none", None, None),
            "FREEMIXD_RECORDER\tv=1\tslot=1\tstate=Stopped\toutcome=Clean\taccepted_pairs=30\tcompleted_pairs=30\toutput_bytes=1024\twav_bytes=512\tvideo_bitrate_kbps=unset\taudio_bitrate_kbps=unset\tduration_ms=1000\tfree_bytes=2048\toutput_finalization=Synced\tcleanup=Complete\tapp_capture_failure=false\tfailure=none",
        );
        // The second slot is annotated by number, never by position in a
        // combined record, so a mixed run keeps the two segments separable.
        assert!(
            recorder_report(2, &report, 0, 0, false, "none", None, None)
                .starts_with("FREEMIXD_RECORDER\tv=1\tslot=2\tstate=Stopped\t")
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recorder_report_renders_configured_and_unset_bitrates() {
        let report = StopReport {
            outcome: StopOutcome::Clean,
            exit_status: Some(0),
            output: OutputFinalization::Synced,
            cleanup: CleanupStatus::Complete,
            wav_bytes: 0,
            telemetry: fm_codec_ffmpeg::record::RecordTelemetry {
                state: RecorderState::Stopped,
                failure: None,
                accepted_pairs: 1,
                completed_pairs: 1,
                failed_pairs: 0,
                outstanding_pairs: 0,
                retained_bytes: 0,
                output_bytes: 128,
                wav_bytes: 0,
                stderr_tail: String::new(),
                stderr_truncated: false,
            },
        };
        assert!(
            recorder_report(1, &report, 0, 0, false, "none", Some(2_000), Some(96)).contains(
                "\twav_bytes=0\tvideo_bitrate_kbps=2000\taudio_bitrate_kbps=96\tduration_ms=0\t"
            )
        );
        assert!(
            recorder_report(2, &report, 0, 0, false, "none", None, None)
                .contains("\twav_bytes=0\tvideo_bitrate_kbps=unset\taudio_bitrate_kbps=unset\t")
        );
        // Each field is independent: one slot can set video while keeping the
        // encoder's default audio.
        assert!(
            recorder_report(2, &report, 0, 0, false, "none", Some(1_000), None)
                .contains("\tvideo_bitrate_kbps=1000\taudio_bitrate_kbps=unset\t")
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recording_output_requires_mp4_and_existing_directory_parent() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            record_output_parent(&directory.path().join("capture.mov"))
                .unwrap_err()
                .to_string()
                .contains("final extension `.mp4`")
        );
        assert!(
            record_output_parent(&directory.path().join("missing/capture.mp4"))
                .unwrap_err()
                .to_string()
                .contains("existing canonical directory")
        );
        let not_directory = directory.path().join("parent-file");
        fs::write(&not_directory, b"parent").unwrap();
        assert!(
            record_output_parent(&not_directory.join("capture.mp4"))
                .unwrap_err()
                .to_string()
                .contains("existing canonical directory")
        );
        assert_eq!(
            record_output_parent(&directory.path().join("capture.mp4")).unwrap(),
            fs::canonicalize(directory.path()).unwrap()
        );
    }

    #[cfg(feature = "native-media")]
    fn planned_session(
        path: &Path,
        names: &recording_session::SegmentNames,
    ) -> recording_session::PlannedSession {
        let parent = record_output_parent(path).unwrap();
        recording_session::plan(path, &parent, names, 1, 1_000_000, None).unwrap()
    }

    /// A recorder that refuses to start after its sessions were created must
    /// not leave the video or WAV session behind: here an out-of-range
    /// bitrate makes the real recorder refuse, and the next start may reuse
    /// the same name. A stale WAV sidecar never reaches a start: it takes
    /// the name.
    #[cfg(feature = "native-media")]
    #[test]
    fn refused_recorder_start_removes_the_created_session() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("show.mp4");
        let wav = directory.path().join("show.wav");
        let mut config = RecordConfig::new(
            RecordFormat::new(
                64,
                36,
                fm_types::FrameRate::new(30, 1).unwrap(),
                fm_types::SampleRate::new(48_000).unwrap(),
                fm_types::ChannelLayout::stereo(),
                SequenceNumber::new(0),
            )
            .unwrap(),
        );
        config.video_bitrate_kbps = Some(1);
        let names = recording_session::SegmentNames::new(None);
        let parent = record_output_parent(&path).unwrap();
        let planned =
            recording_session::plan(&path, &parent, &names, 1, 1_000_000, Some(1)).unwrap();

        assert!(
            start_session_recorder(planned, config).is_err(),
            "an invalid bitrate refuses the recorder start"
        );
        assert!(!path.exists(), "the refused start left its segment behind");
        assert!(!wav.exists(), "the refused start left its sidecar behind");
        for manifest in ["show.fmsession", "show.wav.fmsession"] {
            assert!(
                !directory.path().join(manifest).exists(),
                "the refused start left {manifest} behind"
            );
        }
        assert!(!names.taken(&path), "the refused name is free again");

        fs::write(&wav, b"stale").unwrap();
        assert!(names.taken(&path), "a stale sidecar takes the name");
        assert_eq!(
            next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
            directory.path().join("show-002.mp4")
        );
        assert_eq!(fs::read(wav).unwrap(), b"stale");
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recording_output_is_exclusive_and_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let names = recording_session::SegmentNames::new(None);
        let path = directory.path().join("capture.mp4");
        fs::write(&path, b"keep-me").unwrap();
        // Name derivation skips an existing file...
        assert!(names.taken(&path));
        assert_eq!(
            next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
            directory.path().join("capture-002.mp4")
        );
        // ...and a session forced onto it never overwrites: segment creation
        // is exclusive, so the session moves on to its next segment name.
        let opened = recording_session::open(planned_session(&path, &names)).unwrap();
        drop(opened.output);
        let (status, config) = (&opened.status, opened.status.config().clone());
        assert_eq!(status.get().video.segments.len(), 0);
        let second = fm_record::RecordingSession::start(
            config.clone(),
            fm_record::FileStorage::new(fm_record::UnknownSpace),
        );
        assert!(
            matches!(
                second,
                Err(fm_record::SessionError::ManifestExists { root: 0 })
            ),
            "a second session of the same name is refused while its manifest exists"
        );
        recording_session::discard_unused(status);
        let mut session = fm_record::RecordingSession::start(
            config,
            fm_record::FileStorage::new(fm_record::UnknownSpace),
        )
        .unwrap();
        let opened = session.open_segment().unwrap();
        assert_eq!(opened.index, 2);
        assert_eq!(
            opened.path,
            fs::canonicalize(directory.path())
                .unwrap()
                .join("capture-0002.mp4")
        );
        assert_eq!(fs::read(&path).unwrap(), b"keep-me");

        let occupied_directory = directory.path().join("occupied.mp4");
        fs::create_dir(&occupied_directory).unwrap();
        assert!(names.taken(&occupied_directory));
    }

    #[cfg(all(feature = "native-media", unix))]
    #[test]
    fn recording_output_rejects_existing_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::write(&target, b"keep-me").unwrap();
        let output = directory.path().join("capture.mp4");
        symlink(&target, &output).unwrap();
        let names = recording_session::SegmentNames::new(None);
        assert!(names.taken(&output));
        let opened = recording_session::open(planned_session(&output, &names)).unwrap();
        drop(opened.output);
        recording_session::discard_unused(&opened.status);
        let mut session = fm_record::RecordingSession::start(
            opened.status.config().clone(),
            fm_record::FileStorage::new(fm_record::UnknownSpace),
        )
        .unwrap();
        let opened = session.open_segment().unwrap();
        assert_ne!(
            opened.path.file_name(),
            output.file_name(),
            "the symlinked name is never opened"
        );
        assert_eq!(fs::read(target).unwrap(), b"keep-me");
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn a_started_session_reserves_its_name_until_its_manifest_exists() {
        let directory = tempfile::tempdir().unwrap();
        let names = recording_session::SegmentNames::new(None);
        let path = directory.path().join("capture.mp4");
        let planned = planned_session(&path, &names);
        assert!(names.taken(&path), "planning reserves the name");
        assert_eq!(
            next_record_segment_path(&path, |candidate| names.taken(candidate)).unwrap(),
            directory.path().join("capture-002.mp4")
        );
        let sink = recording_session::open(planned).unwrap().output;
        assert!(names.taken(&path), "the manifest keeps the name taken");
        drop(sink);
        assert!(names.taken(&path), "a stopped session keeps its name");
    }

    #[test]
    fn parses_fullscreen_program_display_opt_in() {
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--fullscreen-display",
                "2",
            ]))
            .unwrap(),
            Command::Serve {
                native_media: true,
                fullscreen_program: true,
                display_outputs,
                display_inventory: None,
                ..
            } if display_outputs == [DisplayOutputConfig::program_on(DisplaySelector::Index(2))]
        ));
    }

    #[test]
    fn fullscreen_display_defaults_to_zero() {
        assert!(matches!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
            ]))
            .unwrap(),
            Command::Serve {
                fullscreen_program: true,
                display_outputs,
                ..
            } if display_outputs == [DisplayOutputConfig::program_on(DisplaySelector::Index(0))]
        ));
    }

    #[test]
    fn parses_two_independent_display_outputs() {
        let Command::Serve {
            fullscreen_program,
            display_outputs,
            display_inventory,
            ..
        } = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--display-output",
            "display=name:Left,source=program,scaling=fit",
            "--display-output",
            "display=id:x:66,source=output:2,mode=1920x1080@60,scaling=1:1,hdr=auto",
        ]))
        .unwrap()
        else {
            panic!("expected serve");
        };
        assert!(fullscreen_program);
        assert_eq!(display_inventory, None);
        assert_eq!(display_outputs.len(), 2);
        assert_eq!(
            display_outputs[1].source,
            fm_io_api::display::DisplayOutputSource::Output(fm_types::OutputId::new(
                NonZeroU128::new(2).unwrap()
            ))
        );
        assert_eq!(
            display_outputs[1].scaling,
            fm_types::OutputScaling::OneToOne
        );

        let Command::Serve {
            fullscreen_program,
            display_inventory,
            ..
        } = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--display-output",
            "display=0",
            "--diagnostic-display-inventory",
            "displays.tsv",
        ]))
        .unwrap()
        else {
            panic!("expected serve");
        };
        assert!(!fullscreen_program, "the inventory host is headless");
        assert_eq!(display_inventory, Some(PathBuf::from("displays.tsv")));
    }

    #[test]
    fn diagnostic_capture_inventory_requires_native_media_and_one_file() {
        let Command::Serve {
            capture_inventory, ..
        } = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--diagnostic-capture-inventory",
            "captures.tsv",
        ]))
        .unwrap()
        else {
            panic!("expected serve");
        };
        assert_eq!(capture_inventory, Some(PathBuf::from("captures.tsv")));
        for arguments in [
            &[
                "serve",
                "show.freemix",
                "--diagnostic-capture-inventory",
                "a",
            ][..],
            &[
                "serve",
                "show.freemix",
                "--native-media",
                "--diagnostic-capture-inventory",
                "a",
                "--diagnostic-capture-inventory",
                "b",
            ][..],
            &[
                "serve",
                "show.freemix",
                "--native-media",
                "--diagnostic-capture-inventory",
            ][..],
        ] {
            assert!(parse_args(strings(arguments)).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn display_presenter_if_available_follows_the_build() {
        let Command::Serve {
            native_media,
            fullscreen_program,
            presenter_fallback,
            display_outputs,
            ..
        } = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--display-presenter-if-available",
        ]))
        .unwrap()
        else {
            panic!("expected serve");
        };
        let surfaces = cfg!(feature = "display-surface");
        assert!(!native_media, "without a presenter it serves simulated");
        assert_eq!(fullscreen_program, surfaces, "the presenter opens windows");
        assert_eq!(presenter_fallback, surfaces);
        assert!(display_outputs.is_empty(), "the project's slots are kept");

        // An explicit presenter or diagnostic inventory keeps its own
        // behavior, and options the windowed daemon does not serve keep the
        // daemon headless.
        for options in [
            &["--native-media", "--diagnostic-display-inventory", "d.tsv"][..],
            &["--web-listen", "127.0.0.1:0"][..],
        ] {
            let mut arguments = vec!["serve", "show.freemix", "--display-presenter-if-available"];
            arguments.extend(options);
            let Command::Serve {
                fullscreen_program,
                presenter_fallback,
                ..
            } = parse_args(strings(&arguments)).unwrap()
            else {
                panic!("expected serve");
            };
            assert!(!fullscreen_program, "{options:?}");
            assert!(!presenter_fallback, "{options:?}");
        }
        let error = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--display-presenter-if-available",
            "--display-presenter-if-available",
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("duplicate option"), "{error}");
    }

    #[test]
    fn display_presenter_opens_the_project_slots_in_windows() {
        let Command::Serve {
            fullscreen_program,
            display_outputs,
            display_inventory,
            ..
        } = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--display-presenter",
        ]))
        .unwrap()
        else {
            panic!("expected serve");
        };
        assert!(fullscreen_program, "the presenter opens real windows");
        assert!(display_outputs.is_empty(), "the project's slots are kept");
        assert_eq!(display_inventory, None);

        // The diagnostic inventory host presents the project's slots alone.
        let Command::Serve {
            fullscreen_program,
            display_outputs,
            ..
        } = parse_args(strings(&[
            "serve",
            "show.freemix",
            "--native-media",
            "--diagnostic-display-inventory",
            "displays.tsv",
        ]))
        .unwrap()
        else {
            panic!("expected serve");
        };
        assert!(!fullscreen_program);
        assert_eq!(display_outputs, Vec::new());
    }

    #[test]
    fn display_output_overrides_replace_the_project_slots_on_load() {
        let slot = |number| DisplayOutputSlot::new(number).unwrap();
        let stored = test_project();
        let mut project = stored.project().clone();
        project
            .set_display_output(slot(2), "display=index:4".parse().unwrap())
            .unwrap();
        let stored = stored.with_project(project).unwrap();
        let unchanged = apply_display_output_overrides(stored.clone(), &[]).unwrap();
        assert_eq!(unchanged.project().display_outputs().len(), 1);

        let overrides = ["display=index:0,source=preview"
            .parse::<DisplayOutputConfig>()
            .unwrap()];
        let replaced = apply_display_output_overrides(stored.clone(), &overrides).unwrap();
        assert_eq!(replaced.project().display_outputs().len(), 1);
        assert_eq!(
            replaced.project().display_output(slot(1)),
            Some(&overrides[0])
        );
        assert_eq!(replaced.project().display_output(slot(2)), None);

        let unknown = ["display=index:0,source=output:4242".parse().unwrap()];
        let error = apply_display_output_overrides(stored, &unknown).unwrap_err();
        assert!(
            error.to_string().contains("output 4242 does not exist"),
            "{error}"
        );
    }

    /// `--display-output` overrides reach the manifest before any command
    /// is journaled, so an edit accepted against the overridden slots
    /// replays after a crash instead of conflicting with the old slots.
    #[test]
    fn display_output_overrides_are_durable_before_journaled_edits() {
        let slot = |number| DisplayOutputSlot::new(number).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let stored = test_project();
        let mut project = stored.project().clone();
        project
            .set_display_output(slot(1), "display=name:Wall".parse().unwrap())
            .unwrap();
        let stored = stored.with_project(project).unwrap();
        store.save(&stored).unwrap();

        let recovered = load_and_recover(&store, false).unwrap();
        let journal = DurableJournal::new(store.open_journal_writer().unwrap());
        let overrides = ["display=index:0".parse::<DisplayOutputConfig>().unwrap()];
        let mut durable =
            persist_display_output_overrides(&journal, &recovered, &overrides).unwrap();
        let mut control = test_control(&durable);
        let server = test_server(&control);
        // Slot 2 takes the display slot 1 named before the override.
        let execution = execute_durable_command(
            &mut control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "wall",
                "wall-key",
                CommandPayload::SetDisplayOutput {
                    slot: slot(2),
                    config: Box::new("display=name:Wall".parse().unwrap()),
                },
            ),
            0,
        )
        .unwrap();
        assert!(
            matches!(
                execution.submission.output.result,
                CommandResult::Accepted { .. }
            ),
            "{:?}",
            execution.submission.output.result
        );
        drop(journal);

        // A crash here recovers from the manifest and the journal.
        let recovered = load_and_recover(&store, false).unwrap();
        assert_eq!(
            recovered.project().display_outputs(),
            durable.project().display_outputs()
        );
        assert_eq!(
            recovered.project().display_output(slot(1)),
            Some(&overrides[0])
        );
    }

    #[test]
    fn rejects_invalid_display_output_combinations() {
        for (arguments, expected) in [
            (
                vec!["--display-output", "display=0"],
                "--display-output requires --native-media",
            ),
            (
                vec![
                    "--native-media",
                    "--display-output",
                    "display=0,scaling=zoom",
                ],
                "invalid --display-output",
            ),
            (
                vec![
                    "--native-media",
                    "--display-output",
                    "display=0",
                    "--display-output",
                    "display=1",
                    "--display-output",
                    "display=2",
                ],
                "at most 2",
            ),
            (
                vec![
                    "--native-media",
                    "--display-output",
                    "display=0",
                    "--display-output",
                    "display=index:0,source=output:2",
                ],
                "same display",
            ),
            (
                vec![
                    "--native-media",
                    "--fullscreen-program",
                    "--display-output",
                    "display=1",
                ],
                "--display-output replaces --fullscreen-program",
            ),
            (
                vec!["--diagnostic-display-inventory", "d.tsv"],
                "--diagnostic-display-inventory requires --native-media",
            ),
            (
                vec!["--display-presenter"],
                "--display-presenter requires --native-media",
            ),
            (
                vec![
                    "--native-media",
                    "--display-presenter",
                    "--diagnostic-display-inventory",
                    "d.tsv",
                ],
                "cannot be combined with --fullscreen-program or --display-presenter",
            ),
            (
                vec![
                    "--native-media",
                    "--fullscreen-program",
                    "--diagnostic-display-inventory",
                    "d.tsv",
                ],
                "cannot be combined with --fullscreen-program",
            ),
            (
                vec![
                    "--native-media",
                    "--display-output",
                    "display=0",
                    "--status-listen",
                    "127.0.0.1:0",
                ],
                "--status-listen cannot be combined with --fullscreen-program",
            ),
        ] {
            let mut full = strings(&["serve", "show.freemix"]);
            full.extend(arguments.iter().map(|value| (*value).to_owned()));
            let error = parse_args(full).unwrap_err().to_string();
            assert!(error.contains(expected), "{arguments:?}: {error}");
        }
    }

    #[test]
    fn rejects_invalid_fullscreen_option_relationships_and_values() {
        for (arguments, expected) in [
            (
                strings(&["serve", "show.freemix", "--fullscreen-program"]),
                "--fullscreen-program requires --native-media",
            ),
            (
                strings(&["serve", "show.freemix", "--fullscreen-display", "0"]),
                "--fullscreen-display requires --fullscreen-program",
            ),
            (
                strings(&[
                    "serve",
                    "show.freemix",
                    "--native-media",
                    "--fullscreen-program",
                    "--fullscreen-display",
                ]),
                "missing value for --fullscreen-display",
            ),
        ] {
            assert_eq!(parse_args(arguments).unwrap_err().to_string(), expected);
        }
        assert!(
            parse_args(strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--fullscreen-display",
                "-1",
            ]))
            .unwrap_err()
            .to_string()
            .contains("invalid fullscreen display index")
        );
    }

    #[test]
    fn rejects_duplicate_fullscreen_options() {
        for arguments in [
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--fullscreen-program",
            ]),
            strings(&[
                "serve",
                "show.freemix",
                "--native-media",
                "--fullscreen-program",
                "--fullscreen-display",
                "0",
                "--fullscreen-display",
                "1",
            ]),
        ] {
            assert!(parse_args(arguments).is_err());
        }
    }

    #[cfg(not(feature = "native-media"))]
    #[test]
    fn native_media_opt_in_fails_when_support_is_not_compiled() {
        let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
        let error = NativeDaemon::start(&store, &test_project(), None)
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "native-media support was not compiled in"
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_mode_rejects_unimplemented_simulated_sine_audio() {
        let error = validate_native_audio_modes(&test_project()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("requires unsupported simulated sine audio")
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_source_extraction_builds_retained_generators_without_assets() {
        let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
        let resolution = resolve_native_sources(&store, &test_project(), None).unwrap();
        let sources = resolution.sources;
        assert_eq!(sources.len(), 2);
        for (index, source) in sources.iter().enumerate() {
            let NativeResolvedSource::RetainedFrame { input, frame } = source else {
                panic!("simulated source must be retained")
            };
            assert_eq!(*input, test_input_id(u128::try_from(index + 1).unwrap()));
            assert_eq!(frame.payload().dimensions().width(), 1_280);
            assert_eq!(frame.payload().dimensions().height(), 720);
            assert_eq!(frame.timing().sequence().get(), 0);
            assert_eq!(frame.timing().presentation_timestamp().as_nanos(), 0);
            assert!(frame.metadata().is_some());
        }
        let NativeResolvedSource::RetainedFrame { frame, .. } = &sources[0] else {
            unreachable!()
        };
        assert_eq!(
            &frame.payload().plane(0).unwrap().bytes()[..4],
            &[7, 11, 13, 255]
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_source_extraction_keeps_scene_inputs_out_of_physical_sources() {
        let store = ProjectStore::new("unloaded-test-project.freemix").unwrap();
        let resolution = resolve_native_sources(&store, &scene_test_project(), None).unwrap();

        assert_eq!(resolution.sources.len(), 1);
        assert_eq!(resolution.sources[0].input(), test_input_id(2));
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn macos_camera_resolution_is_exact_non_prompting_and_reaps_helper() {
        let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
        let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");

        let directory = tempfile::tempdir().unwrap();
        let helper = fake_camera_helper(directory.path(), 0);
        let capture_marker = PathBuf::from(format!("{}.capture", helper.display()));
        let pid_file = PathBuf::from(format!("{}.pid", helper.display()));
        let permission_marker = PathBuf::from(format!("{}.permission", helper.display()));
        let (sources, mut cameras) =
            resolve_macos_camera_sources(&camera_test_project(&stable_key), Some(helper.as_path()))
                .unwrap();
        assert_eq!(sources.len(), 1);
        let NativeResolvedSource::LiveFrame { input, frame } = &sources[0] else {
            panic!("camera source must enter the live frame lane")
        };
        assert_eq!(*input, test_input_id(1));
        assert_eq!(frame.timing().sequence().get(), 7);
        assert_eq!(
            frame.timing().presentation_timestamp().as_nanos(),
            1_000_000_000
        );
        assert_eq!(frame.payload().plane(0).unwrap().bytes(), &[3, 5, 7, 255]);
        assert_eq!(
            frame.metadata().unwrap().color().transfer,
            fm_types::TransferFunction::Bt709
        );
        let pending = cameras.source_telemetry()[0];
        assert_eq!(pending.frames_received, 1);
        assert_eq!(pending.frames_ingested, 0);
        assert_eq!(pending.preflight_depth, 1);
        assert_eq!(pending.preflight_discarded, 0);
        assert_eq!(pending.frames_received, pending.preflight_depth);
        drop(sources);
        cameras.discard_preflight_frames();
        let discarded = cameras.source_telemetry()[0];
        assert_eq!(discarded.preflight_depth, 0);
        assert_eq!(discarded.preflight_discarded, 1);
        assert_eq!(discarded.frames_received, discarded.preflight_discarded);
        assert_eq!(
            fs::read_to_string(&capture_marker).unwrap(),
            "capture\nfake-camera\n1\n1\n30000\n1001\n"
        );
        assert!(!permission_marker.exists());
        let pid = fs::read_to_string(&pid_file).unwrap();
        drop(cameras);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline
            && std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success()),
            "camera helper {pid} survived source cleanup"
        );

        let unknown_directory = tempfile::tempdir().unwrap();
        let unknown_helper = fake_camera_helper(unknown_directory.path(), 0);
        let unknown_capture = PathBuf::from(format!("{}.capture", unknown_helper.display()));
        let error = resolve_macos_camera_sources(
            &camera_test_project("macos.avfoundation.camera.v1.999"),
            Some(unknown_helper.as_path()),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("unknown camera stable key"));
        assert!(!unknown_capture.exists(), "unknown key invoked capture");

        let prompt_directory = tempfile::tempdir().unwrap();
        let prompt_helper = fake_camera_helper(prompt_directory.path(), 1);
        let prompt_capture = PathBuf::from(format!("{}.capture", prompt_helper.display()));
        let prompt_request = PathBuf::from(format!("{}.permission", prompt_helper.display()));
        let error = resolve_macos_camera_sources(
            &camera_test_project(&stable_key),
            Some(prompt_helper.as_path()),
        )
        .err()
        .unwrap();
        assert!(
            error
                .to_string()
                .contains("camera permission is not granted")
        );
        assert!(
            !prompt_capture.exists(),
            "permission preflight invoked capture"
        );
        assert!(
            !prompt_request.exists(),
            "daemon requested camera permission"
        );
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn many_camera_startup_failure_uses_aggregate_cleanup_and_reaps_all_helpers() {
        const CAMERA_COUNT: usize = 16;

        let keys = (0..CAMERA_COUNT)
            .map(|index| {
                let source_id =
                    deterministic_camera_id(CameraIdKind::Source, &format!("fake-camera-{index}"));
                format!("macos.avfoundation.camera.v1.{source_id}")
            })
            .collect::<Vec<_>>();
        let project = many_camera_test_project(&keys);
        let directory = tempfile::tempdir().unwrap();
        let helper = many_camera_startup_failure_helper(directory.path(), CAMERA_COUNT);
        let pid_log = PathBuf::from(format!("{}.pids", helper.display()));

        let started = Instant::now();
        let error = resolve_macos_camera_sources(&project, Some(helper.as_path()))
            .err()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(error.to_string().contains("failed to start"));
        assert_eq!(
            fs::read_to_string(&pid_log).unwrap().lines().count(),
            CAMERA_COUNT
        );
        assert_helper_processes_reaped(&pid_log);
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn camera_poll_into_ingests_recovered_mapped_discontinuity_while_ticks_continue() {
        let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
        let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
        let project = camera_test_project(&stable_key);
        let directory = tempfile::tempdir().unwrap();
        let helper = recovering_camera_helper(directory.path(), Some(3), 20);
        let capture_log = PathBuf::from(format!("{}.capture", helper.display()));
        let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
        let runtime = NativeMediaRuntime::new_blocking([platform_native_backend()]).unwrap();
        let (sources, mut cameras) = resolve_macos_camera_sources_with_policy(
            &project,
            Some(helper.as_path()),
            test_camera_recovery_policy(3, Duration::from_millis(100)),
        )
        .unwrap();
        let NativeResolvedSource::LiveFrame { input, frame } = &sources[0] else {
            panic!("camera source must be live")
        };
        assert_eq!(*input, test_input_id(1));
        assert_eq!(frame.timing().sequence().get(), 7);
        let mut playback = runtime
            .preflight_resolved_source_playback_mixed_blocking(
                None,
                sources,
                native_clock_domain(),
                StreamSelector::Best,
                NativeSourceLimits::default(),
            )
            .unwrap();
        cameras.mark_preflight_frames_ingested();
        let committed = cameras.source_telemetry()[0];
        assert_eq!(committed.frames_ingested, 1);
        assert_eq!(committed.preflight_depth, 0);
        assert_eq!(committed.preflight_discarded, 0);
        assert_eq!(committed.frames_received, committed.accounted_frames());

        let mut control = test_control(&project);
        let server = test_server(&control);
        let mut rendered = 0_u64;
        let mut checkpointed = 0_u64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while cameras.inputs[0].ingested_frames == 1 && Instant::now() < deadline {
            cameras.poll_into(&runtime, &mut playback).unwrap();
            control.tick(&server).unwrap();
            rendered = rendered.saturating_add(1);
            let snapshot = control.idle_engine_snapshot().unwrap();
            stored_project_checkpoint(&project, &snapshot).unwrap();
            checkpointed = checkpointed.saturating_add(1);
            thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(cameras.inputs[0].ingested_frames, 2);
        assert_eq!(cameras.inputs[0].last_ingested_sequence, Some(8));
        assert!(cameras.inputs[0].last_ingested_discontinuity);
        assert!(rendered > 10, "render loop stalled during camera recovery");
        assert_eq!(checkpointed, rendered);
        let telemetry = cameras.source_telemetry();
        assert_eq!(telemetry[0].recovery_attempts, 2);
        assert_eq!(telemetry[0].recovery_successes, 1);
        assert_eq!(telemetry[0].recovery_exhausted, 0);
        assert_eq!(telemetry[0].recovery_worker_failures, 0);
        assert_eq!(
            telemetry[0].frames_received,
            telemetry[0].accounted_frames()
        );
        assert_eq!(
            telemetry[0].recovery_outcome,
            CameraRecoveryOutcome::Recovered
        );
        assert!(!telemetry[0].diagnostic().contains("fake-camera"));

        let captures = fs::read_to_string(capture_log).unwrap();
        let expected = ["capture", "fake-camera", "1", "1", "30000", "1001"];
        let captures = captures.lines().collect::<Vec<_>>();
        assert_eq!(captures.len(), 18);
        for invocation in captures.chunks_exact(expected.len()) {
            assert_eq!(invocation, expected);
        }

        cameras.shutdown().unwrap();
        assert_helper_processes_reaped(&pid_log);
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn camera_recovery_rearms_after_exhaustion_and_eventually_recovers() {
        let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
        let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
        let project = camera_test_project(&stable_key);
        let directory = tempfile::tempdir().unwrap();
        let helper = recovering_camera_helper(directory.path(), Some(4), 20);
        let capture_count = PathBuf::from(format!("{}.count", helper.display()));
        let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
        let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
            &project,
            Some(helper.as_path()),
            test_camera_recovery_policy(2, Duration::from_millis(20)),
        )
        .unwrap();

        let mut control = test_control(&project);
        let server = test_server(&control);
        let mut rendered = 0_u64;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cameras.poll_with(|_, _| Ok(())).unwrap();
            control.tick(&server).unwrap();
            rendered = rendered.saturating_add(1);
            let telemetry = cameras.source_telemetry();
            if telemetry[0].recovery_successes == 1 || Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let telemetry = cameras.source_telemetry();
        assert_eq!(telemetry[0].recovery_attempts, 3);
        assert_eq!(telemetry[0].recovery_successes, 1);
        assert_eq!(telemetry[0].recovery_exhausted, 1);
        assert_eq!(telemetry[0].recovery_worker_failures, 0);
        assert!(
            rendered > 10,
            "render loop stopped on recoverable camera loss"
        );
        assert_eq!(fs::read_to_string(capture_count).unwrap(), "4");
        cameras.shutdown().unwrap();
        assert_helper_processes_reaped(&pid_log);
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn generic_camera_helper_exit_is_recoverable_but_malformed_contract_is_fatal() {
        let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
        let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
        let project = camera_test_project(&stable_key);
        let directory = tempfile::tempdir().unwrap();
        let helper = recovering_camera_helper(directory.path(), Some(2), 21);
        let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
        let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
            &project,
            Some(helper.as_path()),
            test_camera_recovery_policy(2, Duration::from_millis(20)),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while cameras.inputs[0].ingested_frames == 0 && Instant::now() < deadline {
            let poll_started = Instant::now();
            cameras.poll_with(|_, _| Ok(())).unwrap();
            assert!(
                poll_started.elapsed() < Duration::from_millis(100),
                "render-thread camera poll performed blocking cleanup"
            );
            thread::sleep(Duration::from_millis(1));
        }
        let telemetry = cameras.source_telemetry();
        assert_eq!(telemetry[0].recovery_attempts, 1);
        assert_eq!(telemetry[0].recovery_successes, 1);
        assert_eq!(telemetry[0].recovery_exhausted, 0);
        assert_eq!(telemetry[0].recovery_worker_failures, 0);
        assert!(!recoverable_camera_error(&IoError::MalformedTimestamp(
            fm_io_api::TimestampValidationError::OriginalTimestampOverflow,
        )));
        cameras.shutdown().unwrap();
        assert_helper_processes_reaped(&pid_log);
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn malformed_camera_frame_bytes_are_fatal_and_never_restart_helper() {
        let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
        let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
        let project = camera_test_project(&stable_key);
        let directory = tempfile::tempdir().unwrap();
        let helper = malformed_runtime_camera_helper(directory.path());
        let capture_count = PathBuf::from(format!("{}.count", helper.display()));
        let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
        let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
            &project,
            Some(helper.as_path()),
            test_camera_recovery_policy(2, Duration::from_millis(20)),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let error = loop {
            match cameras.poll_with(|_, _| Ok(())) {
                Ok(()) => {}
                Err(error) => break error,
            }
            assert!(
                Instant::now() < deadline,
                "malformed frame bytes were swallowed"
            );
            thread::sleep(Duration::from_millis(1));
        };
        assert!(matches!(
            error.downcast_ref::<IoError>(),
            Some(IoError::AdapterFailure {
                remediation: None,
                ..
            })
        ));
        assert_eq!(fs::read_to_string(capture_count).unwrap(), "1");
        let telemetry = cameras.source_telemetry();
        assert_eq!(telemetry[0].recovery_attempts, 0);
        assert_eq!(telemetry[0].recovery_exhausted, 0);
        assert_eq!(telemetry[0].recovery_worker_failures, 1);
        cameras.shutdown().unwrap();
        assert_helper_processes_reaped(&pid_log);
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn fatal_camera_contract_error_propagates_without_counting_as_exhaustion() {
        let supervisor = Arc::new(Mutex::new(CameraSupervisorState {
            frame: CameraFrameSlot::default(),
            snapshot: CameraWorkerSnapshot {
                telemetry: CameraTelemetry::default(),
                lifecycle: LifecycleState::Lost,
                health: EndpointHealthState::Failed,
                recovery_attempts: 0,
                recovery_successes: 0,
                recovery_exhausted: 0,
                recovery_worker_failures: 1,
                recovery_outcome: CameraRecoveryOutcome::WorkerFailed,
            },
        }));
        let handle = thread::spawn(|| CameraWorkerResult {
            failure: Some(IoError::MalformedTimestamp(
                fm_io_api::TimestampValidationError::OriginalTimestampOverflow,
            )),
            cleanup_failure: None,
        });
        let mut cameras = NativeCameraInputs {
            inputs: vec![NativeCameraInput {
                input: test_input_id(1),
                source: None,
                worker: Some(NativeCameraWorker {
                    handle: Some(handle),
                    cancel: Arc::new(AtomicBool::new(false)),
                }),
                supervisor,
                recovery_policy: test_camera_recovery_policy(1, Duration::from_millis(10)),
                ingested_frames: 0,
                ingest_failed: 0,
                preflight_depth: 0,
                preflight_discarded: 0,
                last_ingested_sequence: None,
                last_ingested_discontinuity: false,
            }],
            telemetry_emitted: false,
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        let error = loop {
            if let Err(error) = cameras.poll_with(|_, _| Ok(())) {
                break error;
            }
            assert!(
                Instant::now() < deadline,
                "fatal camera error was swallowed"
            );
            thread::sleep(Duration::from_millis(1));
        };
        assert!(error.to_string().contains("malformed timestamp"));
        let telemetry = cameras.source_telemetry();
        assert_eq!(telemetry[0].recovery_exhausted, 0);
        assert_eq!(telemetry[0].recovery_worker_failures, 1);
        assert_eq!(
            telemetry[0].recovery_outcome,
            CameraRecoveryOutcome::WorkerFailed
        );
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn one_recovering_camera_does_not_interrupt_second_camera() {
        let first_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera-a");
        let second_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera-b");
        let first_key = format!("macos.avfoundation.camera.v1.{first_id}");
        let second_key = format!("macos.avfoundation.camera.v1.{second_id}");
        let project = two_camera_test_project(&first_key, &second_key);
        let directory = tempfile::tempdir().unwrap();
        let helper = two_camera_helper(directory.path());
        let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
        let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
            &project,
            Some(helper.as_path()),
            test_camera_recovery_policy(2, Duration::from_millis(20)),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut recovered_input = None;
        while recovered_input.is_none() && Instant::now() < deadline {
            cameras
                .poll_with(|input, frame| {
                    if frame
                        .timing()
                        .flags()
                        .contains(fm_frame::MediaFlags::DISCONTINUITY)
                    {
                        recovered_input = Some(input);
                    }
                    Ok(())
                })
                .unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(recovered_input, Some(test_input_id(1)));
        let telemetry = cameras.source_telemetry();
        assert_eq!(telemetry[0].recovery_successes, 1);
        assert_eq!(telemetry[0].recovery_worker_failures, 0);
        assert_eq!(telemetry[1].lifecycle, LifecycleState::Running);
        assert_eq!(telemetry[1].recovery_attempts, 0);
        assert_eq!(telemetry[1].recovery_worker_failures, 0);
        cameras.shutdown().unwrap();
        assert_helper_processes_reaped(&pid_log);
    }

    #[cfg(all(feature = "native-media", target_os = "macos"))]
    #[test]
    fn camera_shutdown_during_recovery_is_aggregate_bounded_and_reaps_helper() {
        let source_id = deterministic_camera_id(CameraIdKind::Source, "fake-camera");
        let stable_key = format!("macos.avfoundation.camera.v1.{source_id}");
        let project = camera_test_project(&stable_key);
        let directory = tempfile::tempdir().unwrap();
        let helper = hanging_recovery_camera_helper(directory.path());
        let pid_log = PathBuf::from(format!("{}.pids", helper.display()));
        let (_, mut cameras) = resolve_macos_camera_sources_with_policy(
            &project,
            Some(helper.as_path()),
            test_camera_recovery_policy(3, Duration::from_millis(20)),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while cameras.source_telemetry()[0].recovery_attempts == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(cameras.source_telemetry()[0].recovery_attempts, 1);

        let shutdown_started = Instant::now();
        cameras.shutdown().unwrap();
        assert!(shutdown_started.elapsed() < Duration::from_secs(4));
        assert_helper_processes_reaped(&pid_log);
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_source_extraction_keeps_asset_resolution_errors_path_free() {
        let store = ProjectStore::new("secret-native-project-marker.freemix").unwrap();
        let error = resolve_native_sources(
            &store,
            &media_test_project("asset://secret-uri-marker/../clip.mkv"),
            None,
        )
        .err()
        .unwrap();
        let message = error.to_string();
        assert!(message.contains("invalid project asset URI"));
        assert!(!message.contains("secret-native-project-marker"));
        assert!(!message.contains("secret-uri-marker"));
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_source_extraction_routes_png_to_one_timed_retained_frame() {
        let directory = tempfile::tempdir().unwrap();
        let project_path = directory.path().join("still-project.freemix");
        let assets = project_path.join("assets");
        fs::create_dir_all(&assets).unwrap();
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[1, 2, 3, 4], 1, 1, ExtendedColorType::Rgba8)
            .unwrap();
        fs::write(assets.join("still.data"), png).unwrap();

        let sources = resolve_native_sources(
            &ProjectStore::new(&project_path).unwrap(),
            &media_test_project("asset://still.data"),
            None,
        )
        .unwrap()
        .sources;
        assert_eq!(sources.len(), 2);
        for source in sources {
            let NativeResolvedSource::RetainedFrame { frame, .. } = source else {
                panic!("PNG signature must route to retained still decode")
            };
            assert_eq!(
                frame.payload().dimensions(),
                VideoDimensions::new(1, 1).unwrap()
            );
            assert_eq!(frame.payload().plane(0).unwrap().bytes(), &[1, 2, 3, 4]);
            assert_eq!(frame.timing().presentation_timestamp().as_nanos(), 0);
            assert_eq!(frame.timing().duration().as_nanos(), 1);
            assert_eq!(frame.timing().sequence().get(), 0);
            assert_eq!(frame.timing().clock_domain(), native_clock_domain());
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn recognized_corrupt_png_does_not_fall_back_to_ffmpeg() {
        let directory = tempfile::tempdir().unwrap();
        let project_path = directory.path().join("corrupt-still.freemix");
        let assets = project_path.join("assets");
        fs::create_dir_all(&assets).unwrap();
        fs::write(assets.join("bad.bin"), b"\x89PNG\r\n\x1a\ncorrupt").unwrap();

        let error = resolve_native_sources(
            &ProjectStore::new(&project_path).unwrap(),
            &media_test_project("asset://bad.bin"),
            None,
        )
        .err()
        .unwrap();
        let message = error.to_string();
        assert!(message.contains("native still decode failed"));
        assert!(!message.contains("corrupt-still"));
        assert!(!message.contains("bad.bin"));
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn host_pacer_offsets_use_exact_rational_deadlines() {
        let rate = FrameRate::new(30_000, 1_001).unwrap();
        let mut pacer = FramePacer::new(rate, 0);
        assert_eq!(host_deadline_offset(&pacer).unwrap(), Duration::ZERO);
        pacer.advance().unwrap();
        assert_eq!(host_deadline_offset(&pacer).unwrap().as_nanos(), 33_366_666);
        for _ in 1..30_000 {
            pacer.advance().unwrap();
        }
        assert_eq!(host_deadline_offset(&pacer).unwrap().as_secs(), 1_001);
    }

    /// A command that could not be made durable is refused, not acknowledged
    /// and not half-applied: the engine keeps its revision, publishes nothing,
    /// and installs no receipt, so the same command may simply be sent again.
    #[test]
    fn a_command_that_cannot_be_made_durable_is_refused() {
        let mut durable = test_project();
        let initial_engine = restore_engine(&durable).unwrap().snapshot().unwrap();
        let mut control = test_control(&durable);
        let subscription = control.subscribe().unwrap();
        let before_diagnostics = control.diagnostics();
        let before_snapshot = control.snapshot().clone();
        let server = test_server(&control);
        let command = test_command("failed-save", "failed-save-key", CommandPayload::Cut);

        let execution = execute_durable_command(
            &mut control,
            &FailingSaver,
            &mut durable,
            &operator(),
            &server,
            &command,
            0,
        )
        .expect("a durability failure is refused, not propagated");

        assert!(matches!(
            &execution.submission.output.result,
            CommandResult::Rejected {
                code,
                retryable: true,
                current_revision: 0,
                ..
            } if code == "unavailable"
        ));
        assert_eq!(execution.submission.output.events, []);
        assert!(execution.submission.accepted.is_none());
        assert!(!execution.submission.replayed);
        assert_eq!(execution.runtime_events, []);
        assert_eq!(durable, test_project());
        assert_eq!(control.diagnostics(), before_diagnostics);
        assert_eq!(control.snapshot(), &before_snapshot);
        assert_eq!(live_engine_snapshot(&mut control), initial_engine);
        assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));
        let prepared = control
            .prepare_submit(&operator(), command, 0)
            .unwrap()
            .prepared()
            .expect("failed save must not install a receipt");
        prepared.abort();
    }

    /// Regression: a command whose staged result the project schema refuses
    /// (here a blank idempotency key, which a receipt may not carry) stopped
    /// the whole daemon. It is refused, nothing is recorded or committed, and
    /// the next command runs normally.
    #[test]
    fn a_command_whose_result_cannot_be_persisted_is_refused_not_fatal() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);
        let before_durable = durable.clone();
        let before_engine = live_engine_snapshot(&mut control);
        let subscription = control.subscribe().unwrap();
        let saver = CountingSaver::default();

        let refused = execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &operator(),
            &server,
            &test_command("blank-key", "   ", CommandPayload::Cut),
            0,
        )
        .expect("a command-caused staging failure refuses the command");

        assert!(matches!(
            &refused.submission.output.result,
            CommandResult::Rejected {
                id,
                code,
                retryable: false,
                current_revision: 0,
                ..
            } if id == "blank-key" && code == "invalid_command"
        ));
        assert!(refused.submission.accepted.is_none());
        assert_eq!(refused.submission.output.events, []);
        assert_eq!(refused.runtime_events, []);
        assert_eq!(saver.0.get(), 0, "nothing was journaled");
        assert_eq!(durable, before_durable);
        assert_eq!(live_engine_snapshot(&mut control), before_engine);
        assert_eq!(subscription.try_recv(), Err(TryRecvError::Empty));

        let accepted = execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &operator(),
            &server,
            &test_command("after", "after-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
        assert!(matches!(
            accepted.submission.output.result,
            CommandResult::Accepted { revision: 1, .. }
        ));
        assert_eq!(saver.0.get(), 1);
    }

    /// Regression: persisting a Stinger slot whose realization had not caught
    /// up stopped the daemon. It is a retryable conflict.
    #[test]
    fn a_divergent_stinger_slot_refuses_the_command_as_a_retryable_conflict() {
        let durable = test_project();
        let inputs = vec![test_input_id(1), test_input_id(2)];
        let mut desired =
            SwitcherState::new(inputs.clone(), test_input_id(1), test_input_id(2)).unwrap();
        let realized = SwitcherState::new(inputs, test_input_id(1), test_input_id(2)).unwrap();
        let slot = StingerSlotId::new(2).unwrap();
        desired
            .configure_stinger(
                slot,
                StingerDescriptor::new(
                    test_input_id(2),
                    false,
                    5,
                    StingerAudioPolicy::Muted,
                    MissingMediaFallback::Cut,
                ),
            )
            .unwrap();
        let mut project = durable.project().clone();

        let error = sync_project_stingers(&mut project, &desired, &realized).unwrap_err();
        let refusal = staging_refusal(error).expect("divergence is not fatal");
        assert_eq!(refusal.code, RejectionCode::Conflict);
        assert!(refusal.retryable);
        assert!(refusal.message.contains("Stinger slot 2"));

        // Once realization matches, the slot persists.
        let mut settled = realized.clone();
        settled
            .configure_stinger(slot, *desired.stinger(slot).descriptor().unwrap())
            .unwrap();
        sync_project_stingers(&mut project, &desired, &settled).unwrap();
        assert_eq!(project.stingers().len(), 1);
    }

    /// The classification boundary: staging failures caused by the command or
    /// by recoverable runtime state refuse it; failures that mean the daemon's
    /// own state can no longer be trusted stay fatal.
    #[test]
    fn staging_failures_are_classified_into_refusals_and_fatal_errors() {
        let refused = |error: Box<dyn Error>| staging_refusal(error).unwrap();
        let fatal = |error: Box<dyn Error>| staging_refusal(error).unwrap_err().to_string();

        let in_flight = refused(
            fm_control::ControlError::Engine(fm_engine::EngineError::Snapshot(
                fm_engine::SnapshotError::WorkInFlight,
            ))
            .into(),
        );
        assert_eq!(
            (in_flight.code, in_flight.retryable),
            (RejectionCode::Conflict, true)
        );
        let unpersistable =
            refused(fm_persistence::ProjectValidationError::EmptyIdempotencyKey.into());
        assert_eq!(
            (unpersistable.code, unpersistable.retryable),
            (RejectionCode::InvalidCommand, false)
        );

        // An engine that cannot advance or holds stale authority.
        assert_ne!(
            fatal(
                fm_control::ControlError::Engine(fm_engine::EngineError::StalePreparation).into()
            ),
            ""
        );
        assert_ne!(
            fatal(fm_control::ControlError::RuntimeSequenceExhausted.into()),
            ""
        );
        // The stored project disagreeing with the engine.
        assert_eq!(
            fatal(AppFailure("project is missing audio strip for input 9".into()).into()),
            "project is missing audio strip for input 9"
        );
    }

    /// An admitted command whose execution fails must still release its
    /// in-flight slot; a leaked slot would eventually refuse every command.
    #[test]
    fn failed_command_execution_releases_its_inflight_slot() {
        let mut durable = test_project();
        let project_id = durable.project().id();
        let control = Rc::new(RefCell::new(test_control(&durable)));
        let config = ServerConfig::new(
            ServerMode::Development,
            AuthenticationMode::Development,
            std::net::IpAddr::from([127, 0, 0, 1]),
            CAPABILITIES_DIGEST,
        );
        let mut server = Server::new(config, ControlHandle(Rc::clone(&control))).unwrap();
        server.mark_ready().unwrap();
        let principal = development_principal().unwrap();
        let (hello, _) = current_handshake(
            &HandshakeRequest {
                protocol: PROTOCOL_VERSION,
                build: "inflight-leak-test".into(),
                client_type: ClientType::Integration,
                desired_role: Role::Operator,
                resume_cursor: None,
            },
            &control,
            project_id,
        );
        let mut session = server
            .handshake(&hello, &principal, now_millis().unwrap())
            .unwrap()
            .session;
        // A foreign authority makes the post-commit tick of a timed transition
        // fail, so execution returns an error after admission.
        let mut foreign = control_server_identity(&control.borrow(), project_id);
        foreign.log_id = "foreign-log".into();

        let failed = execute_session_command(
            &mut session,
            &control,
            &CountingSaver::default(),
            &mut durable,
            &principal,
            &foreign,
            &test_command(
                "failing-fade",
                "failing-fade-key",
                CommandPayload::Fade { duration_frames: 2 },
            ),
            None,
            None,
        );

        assert!(failed.is_err());
        assert_eq!(session.accounting().inbound_commands_admitted_total, 1);
        assert_eq!(session.accounting().inbound_commands_inflight, 0);
    }

    #[test]
    fn a_refused_manual_position_preserves_restart_state_and_stays_retryable() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);
        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "manual-start",
                "manual-start-key",
                CommandPayload::StartManualTransition {
                    kind: fm_protocol::ManualTransitionKind::Wipe,
                },
            ),
            0,
        )
        .unwrap();
        let before_durable = durable.clone();
        let before_engine = control.idle_engine_snapshot().unwrap();
        let position_command = test_command(
            "manual-position",
            "manual-position-key",
            CommandPayload::SetManualTransitionPosition {
                position: fm_protocol::ManualTransitionPosition::new(6_250).unwrap(),
            },
        );

        let refused = execute_durable_command(
            &mut control,
            &FailingSaver,
            &mut durable,
            &operator(),
            &server,
            &position_command,
            0,
        )
        .expect("a durability failure is refused, not propagated");

        assert!(matches!(
            &refused.submission.output.result,
            CommandResult::Rejected { code, retryable: true, .. } if code == "unavailable"
        ));
        assert_eq!(durable, before_durable);
        assert_eq!(control.idle_engine_snapshot().unwrap(), before_engine);

        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &position_command,
            0,
        )
        .unwrap();
        let manual = durable.runtime_manual_transitions();
        assert_eq!(manual.desired.unwrap().position_basis_points, 6_250);
        assert_eq!(manual.realized.unwrap().interval_start_basis_points, 6_250);
        assert_eq!(durable.idempotency_receipts().len(), 2);
    }

    #[test]
    fn save_precedes_commit_ticks_and_returned_result() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let subscription = control.subscribe().unwrap();
        let saver = ObservingSaver {
            subscription: &subscription,
            saved: RefCell::new(None),
        };
        let server = test_server(&control);

        let execution = execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "ordered-fade",
                "ordered-fade-key",
                CommandPayload::Fade { duration_frames: 4 },
            ),
            0,
        )
        .unwrap();

        assert!(matches!(
            execution.submission.output.result,
            CommandResult::Accepted { revision: 1, .. }
        ));
        assert_eq!(saver.saved.borrow().as_ref(), Some(&durable));
        assert_eq!(durable.position().frames_rendered, 4);
        assert!(matches!(
            subscription.try_recv().unwrap(),
            LiveEvent::Durable(_)
        ));
        assert!(matches!(
            subscription.try_recv().unwrap(),
            LiveEvent::Runtime(_)
        ));
    }

    /// A cue list whose cue runs a media input's transport restores: the
    /// media inputs are known to the show before its cue lists are checked
    /// (RC-017).
    #[test]
    fn a_cue_list_with_a_media_action_restores() {
        let baseline = test_project();
        let mut project = Project::new(
            baseline.project().id(),
            "Cue Media Restore",
            baseline.project().settings().clone(),
        );
        project.add_input(Input {
            id: test_input_id(1),
            name: "Camera".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: test_input_id(2),
            name: "Clip".into(),
            kind: InputKind::Media {
                asset_uri: "asset://clip.mov".into(),
                playback: fm_model::MediaPlaybackSettings::default(),
            },
            required_capabilities: Vec::new(),
        });
        project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
        project.set_cue_lists(vec![fm_model::CueList {
            id: 1,
            name: "Show".into(),
            cues: vec![fm_model::Cue {
                id: 1,
                name: "Roll".into(),
                actions: vec![fm_model::CueAction::Media {
                    input: test_input_id(2),
                    operation: fm_model::CueMediaOperation::Play,
                }],
                auto_follow: false,
            }],
            state: fm_model::CueRunState {
                standby: Some(1),
                ..fm_model::CueRunState::default()
            },
        }]);
        let durable = StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(test_input_id(1)),
                realized_program_id: Some(test_input_id(1)),
                desired_preview_id: Some(test_input_id(2)),
                realized_preview_id: Some(test_input_id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap();
        let engine = restore_engine(&durable).unwrap();
        assert_eq!(engine.show().cue_lists().len(), 1);
    }

    #[test]
    fn projected_project_equals_committed_settled_engine() {
        let mut durable = test_project();
        let original_project = durable.project().clone();
        let mut control = test_control(&durable);
        let server = test_server(&control);

        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "settled-fade",
                "settled-fade-key",
                CommandPayload::Fade { duration_frames: 7 },
            ),
            0,
        )
        .unwrap();

        let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
        assert_eq!(live_engine_snapshot(&mut control), restored);
        assert_eq!(durable.position().frames_rendered, 7);
        assert_eq!(durable.position().runtime_generation, 1);
        assert_eq!(durable.position().clock_time_nanos, 240_000_000);
        let mut expected_project = original_project;
        expected_project.set_main_mix(durable.project().main_mix().unwrap());
        assert_eq!(durable.project(), &expected_project);
    }

    #[test]
    fn queued_fade_take_next_settles_before_checkpoint() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let saver = CountingSaver::default();
        let server = test_server(&control);
        let channel = fm_protocol::WireOverlayChannelId::new(1).unwrap();
        let mut execute = |id, key, payload| {
            execute_durable_command(
                &mut control,
                &saver,
                &mut durable,
                &operator(),
                &server,
                &test_command(id, key, payload),
                0,
            )
        };

        execute(
            "overlay-transition",
            "overlay-transition-key",
            CommandPayload::ConfigureOverlayTransition {
                channel,
                transition: fm_protocol::OverlayTransitionKind::Fade,
                duration_frames: 4,
            },
        )
        .unwrap();
        execute(
            "overlay-queue",
            "overlay-queue-key",
            CommandPayload::QueueOverlay {
                channel,
                source: fm_protocol::WireInputId::from_domain(test_input_id(2)),
            },
        )
        .unwrap();
        let next = execute(
            "overlay-next",
            "overlay-next-key",
            CommandPayload::TakeNextOverlay { channel },
        )
        .unwrap();
        // Dropping the closure ends its captured mutable borrows before the assertions below.
        #[allow(clippy::drop_non_drop)]
        drop(execute);

        assert!(matches!(
            next.submission.output.result,
            CommandResult::Accepted { .. }
        ));
        assert_eq!(durable.position().frames_rendered, 6);
        let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
        let live = live_engine_snapshot(&mut control);
        assert_eq!(live, restored);
        let channel = OverlayChannelId::new(channel.number()).unwrap();
        let desired = live.show().desired_switcher().overlay(channel);
        let realized = live.realized_switcher().overlay(channel);
        assert_eq!(desired, realized);
        assert_eq!(desired.source(), Some(test_input_id(2)));
        assert!(desired.is_active());
        assert_eq!(desired.opacity(), u8::MAX);
        assert_eq!(desired.queued_sources(), []);
    }

    #[test]
    fn restore_engine_honors_durable_stinger_preload_intent() {
        let baseline = test_project();
        let mut project = baseline.project().clone();
        project.add_stinger(StingerConfig::new(
            StingerSlotNumber::new(8).unwrap(),
            test_input_id(2),
            false,
            11,
            ModelStingerAudioPolicy::MixWithProgram,
            StingerMissingMediaFallback::KeepProgram,
        ));
        project.add_stinger(StingerConfig::new(
            StingerSlotNumber::new(1).unwrap(),
            test_input_id(2),
            true,
            1,
            ModelStingerAudioPolicy::Muted,
            StingerMissingMediaFallback::Cut,
        ));
        let durable = StoredProject::from_project(
            project,
            baseline.runtime_routing(),
            baseline.position(),
            baseline.idempotency_receipts().to_vec(),
        )
        .unwrap();

        let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();
        let deferred_slot = StingerSlotId::new(8).unwrap();
        let deferred = StingerDescriptor::new(
            test_input_id(2),
            false,
            11,
            StingerAudioPolicy::MixWithProgram,
            MissingMediaFallback::KeepProgram,
        );
        let preloaded_slot = StingerSlotId::new(1).unwrap();
        let preloaded = StingerDescriptor::new(
            test_input_id(2),
            true,
            1,
            StingerAudioPolicy::Muted,
            MissingMediaFallback::Cut,
        );
        for state in [
            snapshot.show().desired_switcher(),
            snapshot.realized_switcher(),
        ] {
            assert_eq!(state.stinger(deferred_slot).descriptor(), Some(&deferred));
            assert_eq!(
                state.stinger(deferred_slot).preload_state(),
                fm_switcher::StingerPreloadState::NotRequested
            );
            assert_eq!(state.stinger(preloaded_slot).descriptor(), Some(&preloaded));
            assert_eq!(
                state.stinger(preloaded_slot).preload_state(),
                fm_switcher::StingerPreloadState::Ready
            );
        }
    }

    #[test]
    fn missing_cut_stinger_uses_one_checkpoint_frame() {
        let baseline = test_project();
        let mut project = baseline.project().clone();
        project.add_stinger(StingerConfig::new(
            StingerSlotNumber::new(1).unwrap(),
            test_input_id(2),
            false,
            1,
            ModelStingerAudioPolicy::Muted,
            StingerMissingMediaFallback::Cut,
        ));
        let mut durable = StoredProject::from_project(
            project,
            baseline.runtime_routing(),
            baseline.position(),
            baseline.idempotency_receipts().to_vec(),
        )
        .unwrap();
        let mut control = test_control(&durable);
        let server = test_server(&control);

        let execution = execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "missing-cut-stinger",
                "missing-cut-stinger-key",
                CommandPayload::Stinger {
                    slot: fm_protocol::WireStingerSlotId::new(1).unwrap(),
                    duration_frames: 7,
                },
            ),
            0,
        )
        .unwrap();

        assert!(matches!(
            execution.submission.output.result,
            CommandResult::Accepted { .. }
        ));
        assert_eq!(durable.position().frames_rendered, 1);
        assert_eq!(durable.position().runtime_generation, 1);
        let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
        assert_eq!(live_engine_snapshot(&mut control), restored);
        for state in [
            restored.show().desired_switcher(),
            restored.realized_switcher(),
        ] {
            assert_eq!(
                (state.program(), state.preview()),
                (test_input_id(2), test_input_id(1))
            );
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_video_ring_partition_preserves_one_aggregate_gpu_limit() {
        let defaults = NativeSourceLimits::default();
        let (ordinary, stingers) = partition_native_source_limits(1_920, 1_080, 1).unwrap();
        let expected_stinger =
            1_920_u64 * 1_080 * 8 * u64::from(defaults.max_video_frames_per_source.get());

        assert_eq!(stingers.max_retained_rgba16f_bytes, expected_stinger);
        assert_eq!(
            ordinary.max_retained_rgba16f_bytes + stingers.max_retained_rgba16f_bytes,
            defaults.max_retained_rgba16f_bytes
        );
        assert_eq!(
            partition_native_source_limits(1_920, 1_080, 5)
                .unwrap_err()
                .to_string(),
            format!(
                "native Stinger rings require {} retained bytes, exceeding aggregate limit {}",
                expected_stinger * 5,
                defaults.max_retained_rgba16f_bytes
            )
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_stinger_retirement_preflight_gate_is_bounded() {
        let retirements = NativeStingerRetirements::start().unwrap();
        let limit = stinger_mutation::retirement_limit_for_test();
        retirements.set_pending_for_test(limit - 1);
        assert!(retirements.can_accept());
        retirements.set_pending_for_test(limit);
        assert!(!retirements.can_accept());
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_stinger_source_validation_bounds_preload_kinds_and_audio() {
        let baseline = test_project();
        let mut project = baseline.project().clone();
        project.add_stinger(StingerConfig::new(
            StingerSlotNumber::new(1).unwrap(),
            test_input_id(2),
            true,
            1,
            ModelStingerAudioPolicy::Muted,
            StingerMissingMediaFallback::Cut,
        ));
        let durable = StoredProject::from_project(
            project,
            baseline.runtime_routing(),
            baseline.position(),
            Vec::new(),
        )
        .unwrap();
        let mut source = SimulatedVideoSource::new(
            1,
            1,
            FrameRate::new(25, 1).unwrap(),
            native_clock_domain(),
            SourcePattern::Solid(Rgba8::new(0, 0, 0, 0)),
        )
        .unwrap();
        let retained = NativeResolvedSource::RetainedFrame {
            input: test_input_id(2),
            frame: source.next_frame().unwrap().unwrap(),
        };
        validate_native_stinger_sources(&durable, &[retained]).unwrap();

        let local = NativeResolvedSource::LocalVideo {
            input: test_input_id(2),
            path: PathBuf::from("/secret/stinger.mov"),
        };
        validate_native_stinger_sources(&durable, std::slice::from_ref(&local)).unwrap();
        assert!(stinger_mutation::native_stinger_requires_ffmpeg(
            &durable,
            std::slice::from_ref(&local)
        ));
        assert!(!stinger_mutation::native_stinger_requires_ffmpeg(
            &baseline,
            std::slice::from_ref(&local)
        ));

        let mut audible_project = baseline.project().clone();
        audible_project.add_stinger(StingerConfig::new(
            StingerSlotNumber::new(1).unwrap(),
            test_input_id(2),
            true,
            1,
            ModelStingerAudioPolicy::StingerOnly,
            StingerMissingMediaFallback::Cut,
        ));
        let audible = StoredProject::from_project(
            audible_project,
            baseline.runtime_routing(),
            baseline.position(),
            Vec::new(),
        )
        .unwrap();
        validate_native_stinger_sources(
            &audible,
            &[NativeResolvedSource::LocalVideo {
                input: test_input_id(2),
                path: PathBuf::from("/secret/stinger.mov"),
            }],
        )
        .unwrap();

        let error = validate_native_stinger_sources(
            &durable,
            &[NativeResolvedSource::LiveFrame {
                input: test_input_id(2),
                frame: source.next_frame().unwrap().unwrap(),
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("is live and cannot be preloaded"));

        let mut deferred_project = baseline.project().clone();
        deferred_project.add_stinger(StingerConfig::new(
            StingerSlotNumber::new(1).unwrap(),
            test_input_id(2),
            false,
            1,
            ModelStingerAudioPolicy::Muted,
            StingerMissingMediaFallback::Cut,
        ));
        let deferred = StoredProject::from_project(
            deferred_project,
            baseline.runtime_routing(),
            baseline.position(),
            baseline.idempotency_receipts().to_vec(),
        )
        .unwrap();
        validate_native_stinger_sources(&deferred, std::slice::from_ref(&local)).unwrap();
        assert!(!stinger_mutation::native_stinger_requires_ffmpeg(
            &deferred,
            &[local]
        ));
    }

    #[test]
    fn alpha_fade_settles_before_checkpoint_and_restores_exact_routing() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);

        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "settled-alpha-fade",
                "settled-alpha-fade-key",
                CommandPayload::AlphaFade { duration_frames: 3 },
            ),
            0,
        )
        .unwrap();

        let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
        assert_eq!(live_engine_snapshot(&mut control), restored);
        assert_eq!(durable.position().frames_rendered, 3);
        assert_eq!(durable.position().runtime_generation, 1);
        assert_eq!(
            (
                restored.realized_switcher().program(),
                restored.realized_switcher().preview(),
            ),
            (test_input_id(2), test_input_id(1))
        );
    }

    #[test]
    fn slide_settles_before_checkpoint_and_restores_exact_routing() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);

        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "settled-slide",
                "settled-slide-key",
                CommandPayload::Slide { duration_frames: 3 },
            ),
            0,
        )
        .unwrap();

        let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
        assert_eq!(live_engine_snapshot(&mut control), restored);
        assert_eq!(durable.position().frames_rendered, 3);
        assert_eq!(durable.position().runtime_generation, 1);
        assert_eq!(
            (
                restored.realized_switcher().program(),
                restored.realized_switcher().preview(),
            ),
            (test_input_id(2), test_input_id(1))
        );
    }

    #[test]
    fn zoom_settles_before_checkpoint_and_restores_exact_routing() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);

        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "settled-zoom",
                "settled-zoom-key",
                CommandPayload::Zoom { duration_frames: 3 },
            ),
            0,
        )
        .unwrap();

        let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
        assert_eq!(live_engine_snapshot(&mut control), restored);
        assert_eq!(durable.position().frames_rendered, 3);
        assert_eq!(durable.position().runtime_generation, 1);
        assert_eq!(
            (
                restored.realized_switcher().program(),
                restored.realized_switcher().preview(),
            ),
            (test_input_id(2), test_input_id(1))
        );
    }

    #[test]
    fn fade_to_black_commands_persist_exact_settled_restart_state() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);

        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "fade-to-black",
                "fade-to-black-key",
                CommandPayload::FadeToBlack {
                    active: true,
                    duration_frames: 4,
                },
            ),
            0,
        )
        .unwrap();

        assert_eq!(
            durable.runtime_fade_to_black(),
            RuntimeFadeToBlack {
                desired: PersistedFadeToBlackState::BLACK,
                realized: PersistedFadeToBlackState::BLACK,
            }
        );
        let black = restore_engine(&durable).unwrap().snapshot().unwrap();
        assert!(black.desired_fade_to_black().active);
        assert_eq!(
            black.desired_fade_to_black().position,
            fm_switcher::FadeToBlackPosition::BLACK
        );
        assert_eq!(live_engine_snapshot(&mut control), black);

        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "fade-from-black",
                "fade-from-black-key",
                CommandPayload::FadeToBlack {
                    active: false,
                    duration_frames: 3,
                },
            ),
            0,
        )
        .unwrap();

        assert_eq!(
            durable.runtime_fade_to_black(),
            RuntimeFadeToBlack::default()
        );
        let live = restore_engine(&durable).unwrap().snapshot().unwrap();
        assert_eq!(
            live.realized_fade_to_black().position,
            fm_switcher::FadeToBlackPosition::LIVE
        );
        assert_eq!(live_engine_snapshot(&mut control), live);
    }

    #[test]
    fn idle_snapshot_checkpoint_survives_project_store_restart() {
        let root = std::env::temp_dir().join(format!(
            "freemixd-checkpoint-{}-{}.freemix",
            std::process::id(),
            now_millis().unwrap()
        ));
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);
        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command("checkpoint-cut", "checkpoint-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
        control.tick(&server).unwrap();

        let checkpoint =
            stored_project_checkpoint(&durable, &control.idle_engine_snapshot().unwrap()).unwrap();
        ProjectStore::new(&root).unwrap().save(&checkpoint).unwrap();
        let restarted = ProjectStore::new(&root).unwrap().load().unwrap();

        assert_eq!(restarted.project(), durable.project());
        assert_eq!(
            restarted.project().input_audio_strip(test_input_id(1)),
            durable.project().input_audio_strip(test_input_id(1))
        );
        assert_eq!(restarted.runtime_routing(), durable.runtime_routing());
        assert_eq!(
            restarted.idempotency_receipts(),
            durable.idempotency_receipts()
        );
        assert_eq!(
            restarted.position().frames_rendered,
            durable.position().frames_rendered + 1
        );
        assert_eq!(restarted.position().revision, durable.position().revision);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn audio_strip_command_checkpoints_every_control() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);
        execute_durable_command(
            &mut control,
            &CountingSaver::default(),
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "audio-strip",
                "audio-strip-key",
                CommandPayload::SetInputAudioStrip {
                    pan_basis_points: 0,
                    input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                    gain_millidb: -9_000,
                    balance_basis_points: 2_500,
                    muted: true,
                    soloed: true,
                    follow_video: false,
                    delay_samples: 1_200,
                },
            ),
            0,
        )
        .unwrap();
        control.tick(&server).unwrap();

        let checkpoint =
            stored_project_checkpoint(&durable, &control.idle_engine_snapshot().unwrap()).unwrap();
        assert_eq!(
            checkpoint.project().input_audio_strip(test_input_id(1)),
            Some(InputAudioStripState {
                pan: fm_model::InputPanBasisPoints::CENTER,
                gain: InputGainMilliDb::new(-9_000).unwrap(),
                balance: InputBalanceBasisPoints::new(2_500).unwrap(),
                muted: true,
                soloed: true,
                follow_video: false,
                delay_samples: InputDelaySamples::new(1_200).unwrap(),
            })
        );
    }

    #[test]
    fn authorization_denial_is_saved_before_becoming_replayable() {
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let saver = CountingSaver::default();
        let server = test_server(&control);
        let command = test_command("denied", "denied-key", CommandPayload::Cut);

        let denied = execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &viewer(),
            &server,
            &command,
            0,
        )
        .unwrap();

        assert!(matches!(
            denied.submission.output.result,
            CommandResult::Rejected { ref code, .. } if code == "permission_denied"
        ));
        assert_eq!(saver.0.get(), 1);
        assert_eq!(durable.idempotency_receipts().len(), 1);
        let replay = execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &viewer(),
            &server,
            &command,
            0,
        )
        .unwrap();
        assert!(replay.submission.replayed);
        assert_eq!(saver.0.get(), 1);
        assert_eq!(replay.runtime_events, []);

        let mut restarted = test_control(&durable);
        let restarted_server = test_server(&restarted);
        let mut restarted_durable = durable.clone();
        let replay_after_restart = execute_durable_command(
            &mut restarted,
            &saver,
            &mut restarted_durable,
            &operator(),
            &restarted_server,
            &command,
            0,
        )
        .unwrap();
        assert!(replay_after_restart.submission.replayed);
        assert_eq!(
            replay_after_restart.submission.output.result,
            denied.submission.output.result
        );
        assert_eq!(saver.0.get(), 1);
        assert_eq!(restarted_durable, durable);
    }

    #[test]
    fn crash_after_save_restarts_from_settled_receipt_without_reexecution() {
        let root = std::env::temp_dir().join(format!(
            "freemixd-{}-{}.freemix",
            std::process::id(),
            now_millis().unwrap()
        ));
        let store = ProjectStore::new(&root).unwrap();
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);
        let command = test_command(
            "crash-fade",
            "crash-fade-key",
            CommandPayload::Fade { duration_frames: 4 },
        );

        let crashed = catch_unwind(AssertUnwindSafe(|| {
            let _ = execute_durable_command(
                &mut control,
                &CrashAfterSave(&store),
                &mut durable,
                &operator(),
                &server,
                &command,
                0,
            );
        }));
        assert!(crashed.is_err());
        assert_eq!(control.diagnostics().current_revision, 0);
        assert_eq!(durable.position().revision, 0);

        let saved = store.load().unwrap();
        assert_eq!(saved.position().revision, 1);
        assert_eq!(saved.position().frames_rendered, 4);
        assert_eq!(saved.idempotency_receipts().len(), 1);
        let mut restarted = test_control(&saved);
        let restarted_server = test_server(&restarted);
        let save_counter = CountingSaver::default();
        let mut restarted_durable = saved.clone();
        let replay = execute_durable_command(
            &mut restarted,
            &save_counter,
            &mut restarted_durable,
            &operator(),
            &restarted_server,
            &command,
            0,
        )
        .unwrap();
        assert!(replay.submission.replayed);
        assert_eq!(replay.runtime_events, []);
        assert_eq!(save_counter.0.get(), 0);
        assert_eq!(restarted_durable, saved);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn long_show_persists_a_bounded_receipt_window_that_restarts_and_accepts_work() {
        let root = std::env::temp_dir().join(format!(
            "freemixd-receipts-{}-{}.freemix",
            std::process::id(),
            now_millis().unwrap()
        ));
        let store = ProjectStore::new(&root).unwrap();
        let mut durable = test_project();
        let mut control = test_control(&durable);
        let server = test_server(&control);
        let commands = MAX_PERSISTED_RECEIPTS + 6;
        let mut project_bytes_at_limit = 0;
        for index in 0..commands {
            execute_durable_command(
                &mut control,
                &CountingSaver::default(),
                &mut durable,
                &operator(),
                &server,
                &test_command(
                    &format!("cut-{index}"),
                    &format!("cut-key-{index:05}"),
                    CommandPayload::Cut,
                ),
                0,
            )
            .unwrap();
            assert!(durable.idempotency_receipts().len() <= MAX_PERSISTED_RECEIPTS);
            if index + 1 == MAX_PERSISTED_RECEIPTS {
                store.save(&durable).unwrap();
                project_bytes_at_limit = fs::metadata(root.join("project.json")).unwrap().len();
            }
        }
        assert_eq!(durable.position().revision, commands as u64);
        assert_eq!(durable.idempotency_receipts().len(), MAX_PERSISTED_RECEIPTS);
        let keys = durable
            .idempotency_receipts()
            .iter()
            .map(IdempotencyReceipt::key)
            .collect::<std::collections::BTreeSet<_>>();
        // The oldest receipts were dropped; the newest are all retained.
        for index in 0..6 {
            assert!(!keys.contains(format!("cut-key-{index:05}").as_str()));
        }
        for index in 6..commands {
            assert!(keys.contains(format!("cut-key-{index:05}").as_str()));
        }

        // The bounded project file does not grow past the full window.
        store.save(&durable).unwrap();
        let project_bytes = fs::metadata(root.join("project.json")).unwrap().len();
        assert!(
            project_bytes <= project_bytes_at_limit + 64,
            "{project_bytes} > {project_bytes_at_limit}"
        );

        // A restart restores the window, replays a retained key, and accepts
        // new work with a fresh receipt that keeps the window bounded.
        let saved = store.load().unwrap();
        assert_eq!(saved.idempotency_receipts(), durable.idempotency_receipts());
        let mut restarted = test_control(&saved);
        let restarted_server = test_server(&restarted);
        let mut restarted_durable = saved.clone();
        let newest = format!("cut-key-{:05}", commands - 1);
        let replay = execute_durable_command(
            &mut restarted,
            &CountingSaver::default(),
            &mut restarted_durable,
            &operator(),
            &restarted_server,
            &test_command(
                &format!("cut-{}", commands - 1),
                &newest,
                CommandPayload::Cut,
            ),
            0,
        )
        .unwrap();
        assert!(replay.submission.replayed);
        let fresh = execute_durable_command(
            &mut restarted,
            &CountingSaver::default(),
            &mut restarted_durable,
            &operator(),
            &restarted_server,
            &test_command("after-restart", "after-restart-key", CommandPayload::Cut),
            0,
        )
        .unwrap();
        assert!(!fresh.submission.replayed);
        assert!(matches!(
            fresh.submission.output.result,
            CommandResult::Accepted { revision, .. } if revision == commands as u64 + 1
        ));
        assert_eq!(
            restarted_durable.idempotency_receipts().len(),
            MAX_PERSISTED_RECEIPTS
        );
        assert!(
            restarted_durable
                .idempotency_receipts()
                .iter()
                .any(|receipt| receipt.key() == "after-restart-key")
        );
        store.save(&restarted_durable).unwrap();
        assert_eq!(
            store.load().unwrap().idempotency_receipts().len(),
            MAX_PERSISTED_RECEIPTS
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_retention_ranks_rejections_after_their_revision_and_keeps_the_pinned_key() {
        let mut receipts = (1..=MAX_PERSISTED_RECEIPTS as u64)
            .map(|revision| {
                IdempotencyReceipt::accepted(format!("a-{revision:05}"), "id", revision, revision)
            })
            .collect::<Vec<_>>();
        // A rejection at revision 1 is newer than the accept that produced 1.
        receipts.push(IdempotencyReceipt::rejected(
            "r-1", "id", 1, "conflict", "stale", false,
        ));
        let retained = retained_receipts(receipts.clone(), None);
        assert_eq!(retained.len(), MAX_PERSISTED_RECEIPTS);
        assert!(!retained.iter().any(|receipt| receipt.key() == "a-00001"));
        assert!(retained.iter().any(|receipt| receipt.key() == "r-1"));

        // An older rejection drops first, unless it is the pinned key.
        receipts.push(IdempotencyReceipt::rejected(
            "r-0", "id", 0, "conflict", "stale", false,
        ));
        let retained = retained_receipts(receipts.clone(), None);
        assert!(!retained.iter().any(|receipt| receipt.key() == "r-0"));
        let retained = retained_receipts(receipts, Some("r-0"));
        assert_eq!(retained.len(), MAX_PERSISTED_RECEIPTS);
        assert!(retained.iter().any(|receipt| receipt.key() == "r-0"));
        // Pinning displaces the next-oldest receipt instead.
        assert!(!retained.iter().any(|receipt| receipt.key() == "r-1"));
        assert!(retained.iter().any(|receipt| receipt.key() == "a-00002"));
    }

    fn test_project() -> StoredProject {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        let mut project = Project::new(
            ProjectId::new(NonZeroU128::new(42).unwrap()),
            "Unit Test",
            ProjectSettings {
                frame_rate,
                video: VideoFormat {
                    dimensions: VideoDimensions::new(1_280, 720).unwrap(),
                    frame_rate,
                    pixel_format: PixelFormat::Rgba8,
                    scan: ScanMode::Progressive,
                    color: ColorMetadata::default(),
                },
                audio: AudioFormat {
                    sample_rate: SampleRate::new(44_100).unwrap(),
                    sample_format: SampleFormat::I16,
                    channels: ChannelLayout::stereo(),
                },
            },
        );
        project.add_input(Input {
            id: test_input_id(1),
            name: "Custom solid and tone".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(7, 11, 13, 255)),
                SimulatedAudio::Sine { frequency_hz: 997 },
            )),
            required_capabilities: vec!["simulation.custom".into()],
        });
        project.add_input(Input {
            id: test_input_id(2),
            name: "Bars".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        assert!(project.set_input_audio_strip(
            test_input_id(1),
            InputAudioStripState {
                pan: fm_model::InputPanBasisPoints::CENTER,
                gain: InputGainMilliDb::new(-3_000).unwrap(),
                balance: InputBalanceBasisPoints::CENTER,
                delay_samples: InputDelaySamples::default(),
                muted: false,
                soloed: false,
                follow_video: true,
            },
        ));
        project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
        StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(test_input_id(1)),
                realized_program_id: Some(test_input_id(1)),
                desired_preview_id: Some(test_input_id(2)),
                realized_preview_id: Some(test_input_id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap()
    }

    #[cfg(feature = "native-media")]
    fn media_test_project(asset_uri: &str) -> StoredProject {
        let baseline = test_project();
        let mut project = Project::new(
            baseline.project().id(),
            "Media Unit Test",
            baseline.project().settings().clone(),
        );
        for value in [1, 2] {
            project.add_input(Input {
                id: test_input_id(value),
                name: format!("Media {value}"),
                kind: InputKind::Media {
                    asset_uri: asset_uri.into(),
                    playback: fm_model::MediaPlaybackSettings::default(),
                },
                required_capabilities: Vec::new(),
            });
        }
        project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
        StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(test_input_id(1)),
                realized_program_id: Some(test_input_id(1)),
                desired_preview_id: Some(test_input_id(2)),
                realized_preview_id: Some(test_input_id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap()
    }

    #[cfg(feature = "native-media")]
    fn scene_test_project() -> StoredProject {
        let baseline = test_project();
        let mut project = Project::new(
            baseline.project().id(),
            "Scene Unit Test",
            baseline.project().settings().clone(),
        );
        let scene_id = SceneId::new(NonZeroU128::new(1).unwrap());
        project.add_input(Input {
            id: test_input_id(1),
            name: "Scene".into(),
            kind: InputKind::Scene {
                scene_id,
                audio_source: Some(test_input_id(2)),
            },
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: test_input_id(2),
            name: "Audio".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        project.add_scene(Scene {
            id: scene_id,
            name: "Program".into(),
            background: ModelRgba8::OPAQUE_BLACK,
            layers: Vec::new(),
            animation: fm_model::SceneAnimation::default(),
        });
        project.set_main_mix(MainMix::new(test_input_id(1), test_input_id(2)));
        StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(test_input_id(1)),
                realized_program_id: Some(test_input_id(1)),
                desired_preview_id: Some(test_input_id(2)),
                realized_preview_id: Some(test_input_id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap()
    }

    fn test_input_id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    /// Live input add and remove fold into the stored project before they
    /// are journaled: an added input persists with the kind its command
    /// carried, and a removal the engine allows is still refused, without
    /// journaling anything, while a persisted reference the engine does not
    /// model (here an alias target) names the input.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn live_input_catalogue_changes_sync_the_project_and_refuse_alias_targets() {
        let baseline = test_project();
        let mut project = baseline.project().clone();
        project.add_input(Input {
            id: test_input_id(3),
            name: "Camera".into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        project.add_input(Input {
            id: test_input_id(4),
            name: "Camera alias".into(),
            kind: InputKind::Alias {
                target: test_input_id(3),
                crop: None,
            },
            required_capabilities: Vec::new(),
        });
        let mut durable = StoredProject::from_project(
            project,
            RuntimeRouting {
                desired_program_id: Some(test_input_id(1)),
                realized_program_id: Some(test_input_id(1)),
                desired_preview_id: Some(test_input_id(2)),
                realized_preview_id: Some(test_input_id(2)),
            },
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap();
        let mut control = test_control(&durable);
        let server = test_server(&control);
        let saver = CountingSaver::default();
        let before = durable.clone();

        let refused = execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "remove-target",
                "remove-target-key",
                CommandPayload::RemoveInput {
                    input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
                },
            ),
            0,
        )
        .unwrap();
        assert!(matches!(
            &refused.submission.output.result,
            CommandResult::Rejected {
                code,
                message,
                retryable: false,
                current_revision: 0,
                ..
            } if code == "conflict"
                && message.contains("cannot be removed: it is the target of alias input")
        ));
        // The persisted reference travels as a typed reason (Protocol 2.45).
        let CommandResult::Rejected { reason, .. } = &refused.submission.output.result else {
            unreachable!()
        };
        assert!(
            matches!(
                reason,
                Some(fm_protocol::RefusalReason::InputInUse {
                    input: 3,
                    usage: fm_protocol::InputUse::AliasTarget { .. },
                })
            ),
            "{reason:?}"
        );
        assert_eq!(saver.0.get(), 0, "a refused removal journals nothing");
        assert_eq!(durable, before);

        let added = execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "add-guest",
                "add-guest-key",
                CommandPayload::AddInput {
                    input: fm_protocol::WireInputId::from_domain(test_input_id(5)),
                    name: "Guest".into(),
                    source: fm_protocol::LiveInputSource::Solid {
                        red: 1,
                        green: 2,
                        blue: 3,
                        alpha: 255,
                    },
                },
            ),
            0,
        )
        .unwrap();
        assert!(matches!(
            added.submission.output.result,
            CommandResult::Accepted { revision: 1, .. }
        ));
        let guest = durable
            .project()
            .inputs()
            .iter()
            .find(|input| input.id == test_input_id(5))
            .expect("the added input is persisted");
        assert_eq!(guest.name, "Guest");
        assert_eq!(
            guest.kind,
            InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(1, 2, 3, 255)),
                SimulatedAudio::Silence,
            ))
        );
        assert!(
            durable
                .project()
                .input_audio_strips()
                .iter()
                .any(|strip| strip.input == test_input_id(5))
        );

        let removed = execute_durable_command(
            &mut control,
            &saver,
            &mut durable,
            &operator(),
            &server,
            &test_command(
                "remove-alias",
                "remove-alias-key",
                CommandPayload::RemoveInput {
                    input: fm_protocol::WireInputId::from_domain(test_input_id(4)),
                },
            ),
            0,
        )
        .unwrap();
        assert!(matches!(
            removed.submission.output.result,
            CommandResult::Accepted { revision: 2, .. }
        ));
        assert_eq!(
            durable
                .project()
                .inputs()
                .iter()
                .map(|input| input.id)
                .collect::<Vec<_>>(),
            [1, 2, 3, 5].map(test_input_id)
        );
        assert_eq!(saver.0.get(), 2);
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn native_stinger_preflight_rejections_are_path_free_and_retryable_by_resubmission() {
        let slot = fm_protocol::WireStingerSlotId::new(8).unwrap();
        for payload in [
            CommandPayload::ConfigureStinger {
                slot,
                media_input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                preload: true,
                cut_point_frames: 3,
                audio_policy: fm_protocol::StingerAudioPolicy::Muted,
                missing_media_fallback: fm_protocol::StingerMissingMediaFallback::Cut,
            },
            CommandPayload::RemoveStinger { slot },
        ] {
            let result = native_stinger_preflight_rejection(
                &test_command("native", "native-key", payload),
                7,
            );
            assert!(matches!(
                result,
                CommandResult::Rejected {
                    ref code,
                    ref message,
                    current_revision: 7,
                    retryable: false,
                    ..
                } if code == "unavailable"
                    && message == "native Stinger resources could not be prepared"
            ));
        }
    }

    fn test_control(project: &StoredProject) -> ControlService<Policy> {
        ControlService::new(
            restore_engine(project).unwrap(),
            Policy::development(),
            "unit-engine",
            "unit-log",
            ControlLimits::default(),
        )
    }

    fn test_server(control: &ControlService<Policy>) -> ServerIdentity {
        let engine = control.diagnostics().engine;
        ServerIdentity {
            engine_id: engine.engine_id,
            project_id: "42".into(),
            state_epoch: engine.state_epoch,
            log_id: engine.log_id,
        }
    }

    fn test_command(id: &str, key: &str, payload: CommandPayload) -> CommandMessage {
        CommandMessage {
            protocol: PROTOCOL_VERSION,
            id: id.into(),
            idempotency_key: key.into(),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }
    }

    fn operator() -> Principal {
        development_principal().unwrap()
    }

    fn viewer() -> Principal {
        Principal::authenticated(
            UserId::new("unit-viewer").unwrap(),
            SessionId::new("unit-session").unwrap(),
            [AuthRole::Viewer],
        )
    }

    fn live_engine_snapshot(control: &mut ControlService<Policy>) -> EngineSnapshot {
        let prepared = control
            .prepare_submit(
                &viewer(),
                test_command("snapshot-probe", "snapshot-probe-key", CommandPayload::Cut),
                0,
            )
            .unwrap()
            .prepared()
            .unwrap();
        let snapshot = prepared.project(0).unwrap();
        prepared.abort();
        snapshot
    }

    const STREAM_TARGET_A: u128 = 7_001;
    const STREAM_TARGET_B: u128 = 7_002;

    /// A project with one routed output and two stream destinations, the first
    /// carrying an authored desired-running intent when `running[0]` is true.
    fn test_project_with_streams(running: [bool; 2]) -> StoredProject {
        use fm_model::{
            AudioBus, Output, Rgba8 as TestRgba8, Scene as TestScene,
            StartupPolicy as ModelStartup, StreamEndpoint, StreamKey, StreamProtocol, StreamTarget,
            StreamTargetId,
        };
        use fm_types::{BusId as TestBusId, OutputId as TestOutputId, SceneId as TestSceneId};

        let baseline = test_project();
        let mut project = baseline.project().clone();
        let scene_id = TestSceneId::new(NonZeroU128::new(900).unwrap());
        let bus_id = TestBusId::new(NonZeroU128::new(901).unwrap());
        let output_id = TestOutputId::new(NonZeroU128::new(902).unwrap());
        project.add_scene(TestScene {
            id: scene_id,
            name: "Program".into(),
            background: TestRgba8::OPAQUE_BLACK,
            layers: Vec::new(),
            animation: fm_model::SceneAnimation::default(),
        });
        project.add_audio_bus(AudioBus {
            id: bus_id,
            name: "Program bus".into(),
            sends: Vec::new(),
        });
        project.add_output(Output {
            id: output_id,
            name: "Program output".into(),
            video_source: scene_id,
            audio_source: bus_id,
            startup: ModelStartup::ReconcileDesiredState,
            required_capabilities: Vec::new(),
        });
        for (index, number) in [STREAM_TARGET_A, STREAM_TARGET_B].into_iter().enumerate() {
            let target = StreamTarget::new(
                StreamTargetId::new(NonZeroU128::new(number).unwrap()),
                format!(
                    "Destination {}",
                    u128::from(u8::try_from(index).unwrap() + 1)
                ),
                StreamProtocol::Rtmp,
                StreamEndpoint::parse("ingest.example.com/live").unwrap(),
                Some(StreamKey::parse("unit-secret-key").unwrap()),
                output_id,
            )
            .unwrap()
            .with_startup(ModelStartup::ReconcileDesiredState)
            .set_running(running[index]);
            project.add_stream_target_checked(target).unwrap();
        }
        StoredProject::from_project(
            project,
            baseline.runtime_routing(),
            baseline.position(),
            baseline.idempotency_receipts().to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn restore_engine_seeds_stream_inventory_and_desired_running() {
        let durable = test_project_with_streams([true, false]);
        let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();

        for switcher in [
            snapshot.show().desired_switcher(),
            snapshot.realized_switcher(),
        ] {
            assert_eq!(switcher.streams().len(), 2);
            // Project vector order is preserved.
            assert_eq!(switcher.streams()[0].name(), "Destination 1");
            assert_eq!(
                switcher.streams()[1].id(),
                SwitcherStreamTargetId::from_non_zero(NonZeroU128::new(STREAM_TARGET_B).unwrap())
            );
            assert_eq!(switcher.streams()[1].name(), "Destination 2");
            assert_eq!(
                switcher.stream_running(SwitcherStreamTargetId::from_non_zero(
                    NonZeroU128::new(STREAM_TARGET_A).unwrap()
                )),
                Some(true)
            );
            assert_eq!(
                switcher.stream_running(SwitcherStreamTargetId::from_non_zero(
                    NonZeroU128::new(STREAM_TARGET_B).unwrap()
                )),
                Some(false)
            );
            assert_eq!(
                switcher.running_stream_targets(),
                &std::collections::BTreeSet::from([SwitcherStreamTargetId::from_non_zero(
                    NonZeroU128::new(STREAM_TARGET_A).unwrap()
                )])
            );
        }
        // Authored running flags survive a restore round trip unchanged.
        assert!(durable.project().stream_targets()[0].running());
        assert!(!durable.project().stream_targets()[1].running());
    }

    #[test]
    fn stream_commands_persist_desired_running_and_reject_unknown_targets() {
        let mut durable = test_project_with_streams([false, false]);
        let mut control = test_control(&durable);
        let server = test_server(&control);
        let wire_target =
            || fm_protocol::WireStreamTargetId::new(NonZeroU128::new(STREAM_TARGET_A).unwrap());
        let execute = |control: &mut ControlService<Policy>,
                       durable: &mut StoredProject,
                       id: &str,
                       key: &str,
                       payload| {
            execute_durable_command(
                control,
                &CountingSaver::default(),
                durable,
                &operator(),
                &server,
                &test_command(id, key, payload),
                0,
            )
        };

        let started = execute(
            &mut control,
            &mut durable,
            "stream-on",
            "stream-on-key",
            CommandPayload::StreamStart {
                target: wire_target(),
            },
        )
        .unwrap();
        assert!(matches!(
            started.submission.output.result,
            CommandResult::Accepted { revision: 1, .. }
        ));
        assert!(matches!(
            started.submission.output.events.as_slice(),
            [fm_protocol::EventMessage {
                payload: fm_protocol::EventPayload::StreamsChanged { .. },
                ..
            }]
        ));
        assert_eq!(durable.position().frames_rendered, 1);
        assert!(durable.project().stream_targets()[0].running());

        let rejected = execute(
            &mut control,
            &mut durable,
            "stream-unknown",
            "stream-unknown-key",
            CommandPayload::StreamStart {
                target: fm_protocol::WireStreamTargetId::new(NonZeroU128::new(9_999).unwrap()),
            },
        )
        .unwrap();
        assert!(matches!(
            rejected.submission.output.result,
            CommandResult::Rejected { ref code, .. } if code == "not_found"
        ));
        assert!(!durable.project().stream_targets()[1].running());

        let stopped = execute(
            &mut control,
            &mut durable,
            "stream-off",
            "stream-off-key",
            CommandPayload::StreamStop {
                target: wire_target(),
            },
        )
        .unwrap();
        assert!(matches!(
            stopped.submission.output.result,
            CommandResult::Accepted { revision: 2, .. }
        ));
        assert!(!durable.project().stream_targets()[0].running());
        assert_eq!(durable.position().frames_rendered, 2);

        // The replayed start answers from its original receipt and changes
        // nothing about the stored project.
        let replayed = execute(
            &mut control,
            &mut durable,
            "stream-replay",
            "stream-on-key",
            CommandPayload::StreamStart {
                target: wire_target(),
            },
        )
        .unwrap();
        assert!(matches!(
            replayed.submission.output.result,
            CommandResult::Accepted { ref id, revision: 1, .. } if id == "stream-on"
        ));
        assert!(!durable.project().stream_targets()[0].running());

        // The persisted project alone reproduces the live engine's desired
        // stream state on restore.
        let restored = restore_engine(&durable).unwrap().snapshot().unwrap();
        assert_eq!(live_engine_snapshot(&mut control), restored);
    }

    /// A plain project whose authored per-slot desired recording flags are
    /// `desired` (slot 1 then slot 2).
    fn test_project_with_recording(desired: [bool; 2]) -> StoredProject {
        let baseline = test_project();
        let mut project = baseline.project().clone();
        for (slot, active) in ModelRecorderSlot::ALL.into_iter().zip(desired) {
            project.set_recording_desired(slot, active);
        }
        StoredProject::from_project(
            project,
            baseline.runtime_routing(),
            baseline.position(),
            baseline.idempotency_receipts().to_vec(),
        )
        .unwrap()
    }

    /// The wire recorder slot for `number`, which tests only call with 1 or 2.
    fn wire_recorder(number: u8) -> fm_protocol::WireRecorderSlot {
        fm_protocol::WireRecorderSlot::new(number).expect("test recorder slots are 1 or 2")
    }

    #[test]
    fn operator_state_text_lists_every_stream_and_both_recorder_flags() {
        let control = test_control(&test_project_with_streams([true, false]));
        let text = operator_state_text(&control, None);

        assert!(text.starts_with("streams=2\n"));
        assert!(text.contains(&format!(
            "stream target={STREAM_TARGET_A} name=Destination 1 desired=true"
        )));
        assert!(text.contains(&format!(
            "stream target={STREAM_TARGET_B} name=Destination 2 desired=false"
        )));
        assert!(text.contains("recorders=desired=false,false"));
        for leaked in ["ingest.example.com", "unit-secret-key", "/live"] {
            assert!(!text.contains(leaked), "operator state leaked {leaked}");
        }
    }

    #[test]
    fn operator_state_text_carries_both_desired_recorder_flags() {
        let control = test_control(&test_project_with_recording([true, false]));
        let text = operator_state_text(&control, None);
        assert!(text.contains("recorders=desired=true,false"));

        let control = test_control(&test_project_with_recording([false, true]));
        let text = operator_state_text(&control, None);
        assert!(text.contains("recorders=desired=false,true"));
    }

    #[test]
    fn operator_state_text_truncates_over_the_cap_with_a_marker() {
        let streams: Vec<StreamStatus> = (0..64)
            .map(|index| StreamStatus {
                target: WireStreamTargetId::new(NonZeroU128::new(10_000 + index).unwrap()),
                name: "D".repeat(128),
                desired_running: index % 2 == 0,
                realized: StreamRealizedState::Stopped,
                detail: None,
            })
            .collect();
        let text = render_operator_state(&streams, &[], None);
        assert!(text.len() <= OPERATOR_STATE_MAX_BYTES);
        assert!(text.ends_with(OPERATOR_STATE_TRUNCATION_MARKER));
    }

    #[test]
    fn operator_state_publishes_only_when_a_listener_is_attached() {
        let control = Rc::new(RefCell::new(test_control(&test_project_with_streams([
            true, false,
        ]))));
        let mut process = register_process_shutdown().unwrap();
        process.publish_operator_state(None);
        assert!(process.status.operator_state().is_none());

        process.attach_control(&control);
        process.publish_operator_state(None);
        let published = process
            .status
            .operator_state()
            .expect("operator state was published");
        assert!(published.contains(&format!(
            "stream target={STREAM_TARGET_A} name=Destination 1 desired=true"
        )));
    }

    #[test]
    fn startup_reconcile_opens_only_slots_with_a_true_desired_flag() {
        // An unconfigured slot is never selected, even when desired.
        assert_eq!(
            startup_recording_slots([false, false], [true, true]),
            [false, false]
        );
        // A configured slot whose own flag is false stays closed even when
        // the other slot is recording.
        assert_eq!(
            startup_recording_slots([true, true], [false, false]),
            [false, false]
        );
        assert_eq!(
            startup_recording_slots([true, true], [true, false]),
            [true, false]
        );
        assert_eq!(
            startup_recording_slots([true, true], [false, true]),
            [false, true]
        );
        assert_eq!(
            startup_recording_slots([true, true], [true, true]),
            [true, true]
        );
        // An unconfigured slot cannot be selected by its desired flag alone.
        assert_eq!(
            startup_recording_slots([true, false], [true, true]),
            [true, false]
        );
    }

    #[test]
    fn recording_commands_route_to_only_the_addressed_slot() {
        assert_eq!(
            recording_command_slot(&CommandPayload::RecordStart {
                recorder: wire_recorder(1),
            }),
            Some((true, 0))
        );
        assert_eq!(
            recording_command_slot(&CommandPayload::RecordStop {
                recorder: wire_recorder(1),
            }),
            Some((false, 0))
        );
        assert_eq!(
            recording_command_slot(&CommandPayload::RecordStart {
                recorder: wire_recorder(2),
            }),
            Some((true, 1))
        );
        assert_eq!(
            recording_command_slot(&CommandPayload::RecordStop {
                recorder: wire_recorder(2),
            }),
            Some((false, 1))
        );
        // Every other command family leaves recording realization untouched.
        assert_eq!(recording_command_slot(&CommandPayload::Cut), None);
        assert_eq!(
            recording_command_slot(&CommandPayload::StreamStart {
                target: fm_protocol::WireStreamTargetId::new(NonZeroU128::new(7).unwrap()),
            }),
            None
        );
    }

    #[test]
    fn restore_engine_seeds_each_recording_desired_flag_into_both_switchers() {
        for desired_flags in [[false, false], [true, false], [false, true], [true, true]] {
            let durable = test_project_with_recording(desired_flags);
            let snapshot = restore_engine(&durable).unwrap().snapshot().unwrap();
            // Idle restore demands desired/realized agreement, so BOTH
            // switchers must carry every persisted slot flag or startup would
            // fail.
            for (index, slot) in ModelRecorderSlot::ALL.into_iter().enumerate() {
                let recorder = switcher_recorder_slot(slot);
                assert_eq!(
                    snapshot
                        .show()
                        .desired_switcher()
                        .recording_desired(recorder),
                    desired_flags[index],
                    "slot {} desired flag was not seeded into the show switcher",
                    slot.get()
                );
                assert_eq!(
                    snapshot.realized_switcher().recording_desired(recorder),
                    desired_flags[index],
                    "slot {} desired flag was not seeded into the realized switcher",
                    slot.get()
                );
            }
        }
    }

    /// Executes `command` as the native daemon does, realizing each of its
    /// frames one pacer frame at a time and noting the Program input of each.
    fn native_paced_command(
        control: &mut ControlService<Policy>,
        store: &dyn DurableStore,
        durable: &mut StoredProject,
        server: &ServerIdentity,
        command: &CommandMessage,
        realized: &mut Vec<(u64, InputId, Option<InputId>)>,
    ) -> CommandResult {
        execute_durable_command_with_ticks(
            control,
            store,
            durable,
            &operator(),
            server,
            command,
            0,
            None,
            |control, server| {
                let outcome = control.tick(server)?;
                realized.push((
                    outcome.frame.frame.get(),
                    outcome.frame.program.primary,
                    outcome.frame.program.secondary,
                ));
                Ok(outcome.runtime_events)
            },
        )
        .unwrap()
        .submission
        .output
        .result
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn playlist_clock_cuts_program_on_scheduled_frames_under_native_pacing() {
        use fm_protocol::{
            PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
            PlaylistTransitionKind,
        };
        use journal::DurableJournal;
        use playlist_clock::{Pacing, PlaylistClock, advance_command};

        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("native-playlist.freemix")).unwrap();
        let manifest = test_project();
        store.save(&manifest).unwrap();
        let mut control = test_control(&manifest);
        let server = test_server(&control);
        let mut durable = manifest.clone();
        let mut realized = Vec::new();
        let mut clock = PlaylistClock::default();
        let start_frame;
        let mut advances = Vec::new();
        {
            let journal = DurableJournal::new(store.open_journal_writer().unwrap());
            let entries = vec![
                PlaylistEntrySpec {
                    id: 1,
                    input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                    duration_frames: 10,
                    transition: PlaylistTransitionKind::Cut,
                    transition_frames: 0,
                    enabled: true,
                },
                PlaylistEntrySpec {
                    id: 2,
                    input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                    duration_frames: 10,
                    transition: PlaylistTransitionKind::Fade,
                    transition_frames: 5,
                    enabled: true,
                },
            ];
            let set = native_paced_command(
                &mut control,
                &journal,
                &mut durable,
                &server,
                &test_command(
                    "set",
                    "set-key",
                    CommandPayload::SetPlaylist {
                        loop_mode: PlaylistLoopMode::Off,
                        entries,
                    },
                ),
                &mut realized,
            );
            assert!(matches!(set, CommandResult::Accepted { .. }), "{set:?}");
            // The native pacer realizes idle frames between commands; nothing
            // is pending, so the clock stays quiet.
            for _ in 0..3 {
                assert_eq!(clock.due(&control, Pacing::Engine), None);
                let frame = control.tick(&server).unwrap().frame;
                realized.push((
                    frame.frame.get(),
                    frame.program.primary,
                    frame.program.secondary,
                ));
            }
            let start = native_paced_command(
                &mut control,
                &journal,
                &mut durable,
                &server,
                &test_command(
                    "start",
                    "start-key",
                    CommandPayload::PlaylistControl {
                        operation: PlaylistOperation::Start {
                            at: PlaylistStart::AfterFrames(10),
                            from: None,
                        },
                    },
                ),
                &mut realized,
            );
            assert!(matches!(start, CommandResult::Accepted { .. }), "{start:?}");
            start_frame = control
                .playlist_advance_frame()
                .expect("the start is armed");
            assert!(start_frame > control.frame_cursor());

            // One pass per pacer frame: the clock submits the advance only
            // when its boundary is the next frame to realize, else the
            // pacer realizes an idle frame.
            while control.frame_cursor() < start_frame + 40 {
                if let Some((frame, revision)) = clock.due(&control, Pacing::Engine) {
                    assert_eq!(frame, control.frame_cursor(), "never early, never late");
                    let result = native_paced_command(
                        &mut control,
                        &journal,
                        &mut durable,
                        &server,
                        &advance_command(frame, revision),
                        &mut realized,
                    );
                    clock.observe(frame, revision, &result);
                    assert!(
                        matches!(result, CommandResult::Accepted { .. }),
                        "{result:?}"
                    );
                    advances.push(frame);
                } else {
                    let frame = control.tick(&server).unwrap().frame;
                    realized.push((
                        frame.frame.get(),
                        frame.program.primary,
                        frame.program.secondary,
                    ));
                }
            }
        }
        assert_eq!(
            advances,
            [start_frame, start_frame + 10, start_frame + 20],
            "entry 1 airs, entry 2 airs, the playlist finishes"
        );
        // Frames are realized once each, in order, with no gap.
        assert!(realized.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1));
        let program_at = |frame: u64| {
            realized
                .iter()
                .find(|(realized, ..)| *realized == frame)
                .map(|&(_, primary, secondary)| (primary, secondary))
                .unwrap()
        };
        let (one, two) = (test_input_id(1), test_input_id(2));
        assert_eq!(program_at(start_frame - 1), (one, None));
        // Entry 1 cuts input 2 to Program on exactly its frame.
        assert_eq!(program_at(start_frame), (two, None));
        assert_eq!(program_at(start_frame + 9), (two, None));
        // Entry 2 fades input 1 in from its frame over five frames.
        assert_eq!(program_at(start_frame + 10).1, Some(one));
        assert_eq!(program_at(start_frame + 15), (one, None));
        assert_eq!(program_at(start_frame + 30), (one, None));
        assert_eq!(clock.due(&control, Pacing::Engine), None);
        assert!(matches!(
            control.playlist_state().unwrap().state,
            fm_protocol::PlaylistRunState::Finished { last: Some(2) }
        ));

        // The journal holds the clock's advances, so replay alone reproduces
        // the playlist and Program routing the operator saw.
        let scan = store.scan_journal().unwrap();
        assert_eq!(scan.batches().len(), 5);
        let replayed =
            replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
        assert_eq!(replayed.project().playlist(), durable.project().playlist());
        assert_eq!(replayed.runtime_routing(), durable.runtime_routing());
        assert_eq!(replayed.position(), durable.position());
    }

    #[test]
    fn a_late_native_playlist_advance_replays_on_the_frame_it_ran() {
        use fm_protocol::{
            PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
            PlaylistTransitionKind,
        };
        use journal::DurableJournal;
        use playlist_clock::{Pacing, PlaylistClock, advance_command};

        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("late-playlist.freemix")).unwrap();
        let manifest = test_project();
        store.save(&manifest).unwrap();
        let mut control = test_control(&manifest);
        let server = test_server(&control);
        let mut durable = manifest.clone();
        let mut realized = Vec::new();
        let late_frame;
        {
            let journal = DurableJournal::new(store.open_journal_writer().unwrap());
            for (id, payload) in [
                (
                    "set",
                    CommandPayload::SetPlaylist {
                        loop_mode: PlaylistLoopMode::Off,
                        entries: vec![PlaylistEntrySpec {
                            id: 1,
                            input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                            duration_frames: 50,
                            transition: PlaylistTransitionKind::Cut,
                            transition_frames: 0,
                            enabled: true,
                        }],
                    },
                ),
                (
                    "start",
                    CommandPayload::PlaylistControl {
                        operation: PlaylistOperation::Start {
                            at: PlaylistStart::AfterFrames(5),
                            from: None,
                        },
                    },
                ),
            ] {
                let result = native_paced_command(
                    &mut control,
                    &journal,
                    &mut durable,
                    &server,
                    &test_command(id, &format!("{id}-key"), payload),
                    &mut realized,
                );
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{result:?}"
                );
            }
            let due = control.playlist_advance_frame().unwrap();
            // The pacer realizes frames past the boundary without a clock
            // pass (as the recorder startup barrier does), so the advance
            // runs on the next frame instead.
            while control.frame_cursor() < due + 7 {
                control.tick(&server).unwrap();
            }
            let mut clock = PlaylistClock::default();
            let (frame, revision) = clock.due(&control, Pacing::Engine).unwrap();
            assert_eq!(frame, due + 7);
            late_frame = frame;
            let result = native_paced_command(
                &mut control,
                &journal,
                &mut durable,
                &server,
                &advance_command(frame, revision),
                &mut realized,
            );
            assert!(
                matches!(result, CommandResult::Accepted { .. }),
                "{result:?}"
            );
        }
        // The entry keeps its scheduled start, so later boundaries do not
        // shift; only its cut lands late.
        assert!(matches!(
            control.playlist_state().unwrap().state,
            fm_protocol::PlaylistRunState::Playing { entry: 1, started_frame, .. }
                if started_frame == late_frame - 7
        ));
        assert_eq!(realized.last().unwrap().0, late_frame);
        assert_eq!(realized.last().unwrap().1, test_input_id(2));
        let scan = store.scan_journal().unwrap();
        let replayed =
            replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
        assert_eq!(replayed.position(), durable.position());
        assert_eq!(replayed.project().playlist(), durable.project().playlist());
    }

    /// Idle frames a native daemon realizes after its last command survive a
    /// crash through the progress marker: markers land one second of frames
    /// apart, replay and recovery tick idle up to the newest one and land on
    /// its whole position, and a fold moves idle progress into the manifest.
    #[test]
    fn idle_progress_markers_replay_past_the_last_command_and_fold_into_the_manifest() {
        use fm_protocol::{PlaylistEntrySpec, PlaylistLoopMode, PlaylistTransitionKind};
        use journal::{DurableJournal, PROGRESS_FOLD_SECONDS, decode_progress, frames_in};

        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("progress.freemix")).unwrap();
        let manifest = test_project();
        store.save(&manifest).unwrap();
        let rate = manifest.project().settings().frame_rate;
        let control = Rc::new(RefCell::new(test_control(&manifest)));
        let server = test_server(&control.borrow());
        let mut durable = manifest.clone();
        let mut realized = Vec::new();
        let marker_frames = frames_in(journal::PROGRESS_MARKER_SECONDS, rate);
        {
            let journal = DurableJournal::new(store.open_journal_writer().unwrap());
            let result = native_paced_command(
                &mut control.borrow_mut(),
                &journal,
                &mut durable,
                &server,
                &test_command(
                    "set",
                    "set-key",
                    CommandPayload::SetPlaylist {
                        loop_mode: PlaylistLoopMode::Off,
                        entries: vec![PlaylistEntrySpec {
                            id: 1,
                            input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                            duration_frames: 50,
                            transition: PlaylistTransitionKind::Cut,
                            transition_frames: 0,
                            enabled: true,
                        }],
                    },
                ),
                &mut realized,
            );
            assert!(
                matches!(result, CommandResult::Accepted { .. }),
                "{result:?}"
            );
            let command_end = durable.position().frames_rendered;
            let mut progress = NativeProgress::new(&durable);
            // The pacer realizes idle frames; the loop polls after each.
            let mut markers = Vec::new();
            for _ in 0..(marker_frames * 2 + 7) {
                control.borrow_mut().tick(&server).unwrap();
                progress.poll(&control, &journal, &mut durable);
                if let Some(marker) = store_progress(&store)
                    && markers.last() != Some(&marker.frames_rendered)
                {
                    markers.push(marker.frames_rendered);
                }
            }
            assert_eq!(
                markers,
                [command_end + marker_frames, command_end + 2 * marker_frames],
                "one marker per second of realized frames"
            );
            assert_eq!(
                durable.position().frames_rendered,
                command_end,
                "markers leave the manifest alone"
            );
        }

        // Replay (and so startup recovery) resumes at the newest marker, on
        // its whole position, not where the last command ended.
        let scan = store.scan_journal().unwrap();
        let marker = decode_progress(scan.progress().unwrap().payload()).unwrap();
        let replayed = replay_journal(
            &manifest,
            &BundleAssets::default(),
            scan.batches(),
            Some(marker),
        )
        .unwrap();
        assert_eq!(replayed.position(), marker);
        assert_eq!(replayed.project().playlist(), durable.project().playlist());
        let recovered = load_and_recover(&store, false).unwrap();
        assert_eq!(recovered.position(), marker);
        assert_eq!(store.load().unwrap().position(), marker);
        // The recovered manifest holds the marker, so it has nothing left to
        // replay and a second recovery changes nothing.
        assert_eq!(load_and_recover(&store, false).unwrap().position(), marker);

        // A fold checkpoints idle progress into the manifest itself.
        let control = Rc::new(RefCell::new(test_control(&recovered)));
        let server = test_server(&control.borrow());
        let mut durable = recovered;
        let journal = DurableJournal::new(store.open_journal_writer().unwrap());
        let mut progress = NativeProgress::new(&durable);
        let fold_at = marker.frames_rendered + frames_in(PROGRESS_FOLD_SECONDS, rate);
        while control.borrow().frame_cursor() < fold_at {
            control.borrow_mut().tick(&server).unwrap();
            progress.poll(&control, &journal, &mut durable);
        }
        assert_eq!(durable.position().frames_rendered, fold_at);
        assert_eq!(store.load().unwrap().position(), durable.position());
    }

    fn store_progress(store: &ProjectStore) -> Option<ProjectPosition> {
        // The newest current marker, read beside the open writer.
        store
            .scan_journal()
            .ok()?
            .progress()
            .map(|progress| journal::decode_progress(progress.payload()).unwrap())
    }

    /// Replay refuses, before ticking anything, a journal that asks it to
    /// tick idle frames past its bound: a damaged frame number becomes a
    /// clear startup error instead of a startup that never finishes.
    #[test]
    fn replay_refuses_idle_frames_past_its_bound() {
        let manifest = test_project();
        let rate = manifest.project().settings().frame_rate;
        let limit = journal::frames_in(journal::MAX_REPLAY_IDLE_SECONDS, rate);
        let mut budget = IdleReplayBudget::new(rate);
        assert!(budget.check(10, 10 + limit).is_ok());
        assert!(budget.check(10, 11 + limit).is_err());
        budget.charge(limit - 5);
        assert!(budget.check(0, 5).is_ok());
        assert!(budget.check(0, 6).is_err());

        let damaged = ProjectPosition {
            frames_rendered: u64::MAX / 2,
            ..manifest.position()
        };
        let started = Instant::now();
        let error = replay_journal(&manifest, &BundleAssets::default(), &[], Some(damaged))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("progress marker") && error.contains("idle frames"),
            "{error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "refused before ticking"
        );
    }

    /// A refused playlist advance at `revision`.
    fn refused_advance(revision: u64) -> CommandResult {
        CommandResult::Rejected {
            id: "advance".into(),
            code: "conflict".into(),
            message: "refused".into(),
            fields: Vec::new(),
            current_revision: revision,
            retryable: false,
            reason: None,
        }
    }

    #[test]
    fn playlist_clock_does_not_retry_a_refused_advance_at_the_same_revision() {
        use fm_protocol::{
            PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
            PlaylistTransitionKind,
        };
        use playlist_clock::{Pacing, PlaylistClock};

        let mut control = test_control(&test_project());
        let principal = operator();
        let set = control
            .submit(
                &principal,
                test_command(
                    "set",
                    "set-key",
                    CommandPayload::SetPlaylist {
                        loop_mode: PlaylistLoopMode::Off,
                        entries: vec![PlaylistEntrySpec {
                            id: 1,
                            input: fm_protocol::WireInputId::from_domain(test_input_id(2)),
                            duration_frames: 10,
                            transition: PlaylistTransitionKind::Cut,
                            transition_frames: 0,
                            enabled: true,
                        }],
                    },
                ),
                0,
            )
            .unwrap();
        assert!(set.is_accepted());
        let mut clock = PlaylistClock::default();
        // Nothing pending: under native pacing no wall-clock anchor exists,
        // so snapshots report the engine cursor itself.
        assert_eq!(clock.due(&control, Pacing::Engine), None);
        let rate = FrameRate::new(25, 1).unwrap();
        let start = control
            .submit(
                &principal,
                test_command(
                    "start",
                    "start-key",
                    CommandPayload::PlaylistControl {
                        operation: PlaylistOperation::Start {
                            at: PlaylistStart::Now,
                            from: None,
                        },
                    },
                ),
                0,
            )
            .unwrap();
        assert!(start.is_accepted());
        let server = test_server(&control);
        control.tick(&server).unwrap();
        let due_frame = control.playlist_advance_frame().unwrap();
        while control.frame_cursor() < due_frame {
            assert_eq!(clock.due(&control, Pacing::Engine), None, "not yet due");
            control.tick(&server).unwrap();
        }
        let (frame, revision) = clock.due(&control, Pacing::Engine).unwrap();
        assert_eq!(frame, due_frame);
        assert_eq!(
            clock.observed_frame(control.frame_cursor(), rate, Instant::now()),
            control.frame_cursor()
        );
        clock.observe(frame, revision, &refused_advance(revision));
        assert_eq!(clock.due(&control, Pacing::Engine), None);
        // Another accepted command changes the revision, so the clock tries
        // again; Stop, Pause and Set are accepted on native daemons too.
        let stop = test_command(
            "stop",
            "stop-key",
            CommandPayload::PlaylistControl {
                operation: PlaylistOperation::Stop,
            },
        );
        assert_eq!(
            playlist_clock::playlist_command_refusal(&stop, revision),
            None
        );
        assert!(control.submit(&principal, stop, 0).unwrap().is_accepted());
        assert_eq!(clock.due(&control, Pacing::Engine), None, "stopped");
        assert!(
            playlist_clock::playlist_command_refusal(
                &test_command(
                    "client",
                    "client-key",
                    CommandPayload::PlaylistAdvance { frame }
                ),
                revision,
            )
            .is_some(),
            "only the clock advances the playlist"
        );
    }

    #[test]
    fn record_commands_project_both_slot_flags_across_replay_and_checkpoint() {
        use journal::DurableJournal;

        let directory = tempfile::tempdir().unwrap();
        let project_path = directory.path().join("record-roundtrip.freemix");
        let store = ProjectStore::new(&project_path).unwrap();
        let durable = test_project_with_recording([true, false]);
        store.save(&durable).unwrap();

        // An accepted RecordStop for slot 1 flips only that slot's durable
        // flag and stays only in the journal: a crash right now must still
        // restart with slot 1 stopped and slot 2 untouched.
        let mut control = test_control(&durable);
        let server = test_server(&control);
        let mut durable = durable;
        {
            let journal = DurableJournal::new(store.open_journal_writer().unwrap());
            let stopped = execute_durable_command(
                &mut control,
                &journal,
                &mut durable,
                &operator(),
                &server,
                &test_command(
                    "rec-stop",
                    "rec-stop-key",
                    CommandPayload::RecordStop {
                        recorder: wire_recorder(1),
                    },
                ),
                0,
            )
            .unwrap();
            assert!(matches!(
                stopped.submission.output.result,
                CommandResult::Accepted { revision: 1, .. }
            ));
            assert_eq!(durable.project().recording_desired_slots(), [false, false]);
        }
        assert_eq!(store.load().unwrap().position().revision, 0);

        // Journal replay alone reproduces the per-slot stopped state.
        let reloaded = ProjectStore::new(&project_path).unwrap();
        let manifest = reloaded.load().unwrap();
        let scan = reloaded.scan_journal().unwrap();
        assert_eq!(scan.batches().len(), 1);
        let replayed =
            replay_journal(&manifest, &BundleAssets::default(), scan.batches(), None).unwrap();
        assert_eq!(replayed.position().revision, 1);
        assert_eq!(replayed.project().recording_desired_slots(), [false, false]);

        // A later RecordStart for slot 2 leaves slot 1 stopped, and both flags
        // survive a full checkpoint: the durable mirror never rewrites a slot
        // the command did not address.
        let mut control = test_control(&replayed);
        let server = test_server(&control);
        let mut durable = replayed;
        {
            let journal = DurableJournal::new(reloaded.open_journal_writer().unwrap());
            let started = execute_durable_command(
                &mut control,
                &journal,
                &mut durable,
                &operator(),
                &server,
                &test_command(
                    "rec-start",
                    "rec-start-key",
                    CommandPayload::RecordStart {
                        recorder: wire_recorder(2),
                    },
                ),
                0,
            )
            .unwrap();
            assert!(matches!(
                started.submission.output.result,
                CommandResult::Accepted { revision: 2, .. }
            ));
            assert_eq!(durable.project().recording_desired_slots(), [false, true]);
            journal.settle(&durable).unwrap();
        }
        let checkpointed = ProjectStore::new(&project_path).unwrap();
        let final_state = checkpointed.load().unwrap();
        assert_eq!(final_state.position().revision, 2);
        assert_eq!(
            checkpointed.scan_journal().unwrap().batches().len(),
            0,
            "checkpoint must have compacted the journal"
        );
        assert_eq!(
            final_state.project().recording_desired_slots(),
            [false, true]
        );
        let restored = restore_engine(&final_state).unwrap().snapshot().unwrap();
        assert_eq!(live_engine_snapshot(&mut control), restored);
        let desired = restored.show().desired_switcher();
        assert_eq!(
            (
                desired.recording_desired(switcher_recorder_slot(ModelRecorderSlot::ALL[0])),
                desired.recording_desired(switcher_recorder_slot(ModelRecorderSlot::ALL[1])),
            ),
            (false, true)
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn stream_pair_ledger_advances_only_on_successful_enqueue() {
        let mut ledger = StreamPairLedger {
            next_sequence: 7,
            ..StreamPairLedger::default()
        };

        ledger.note_admitted(SequenceNumber::new(9));
        assert_eq!(ledger.enqueued_pairs, 1);
        assert_eq!(ledger.next_sequence, 10);

        // Refusals only count; they never move the admitted cursor.
        ledger.note_dropped();
        assert_eq!(ledger.dropped_pairs, 1);
        assert_eq!(ledger.next_sequence, 10);

        let mut saturated = StreamPairLedger {
            next_sequence: u64::MAX,
            enqueued_pairs: u64::MAX,
            dropped_pairs: u64::MAX,
        };
        saturated.note_admitted(SequenceNumber::new(u64::MAX));
        saturated.note_dropped();
        assert_eq!(saturated.next_sequence, u64::MAX);
        assert_eq!(saturated.enqueued_pairs, u64::MAX);
        assert_eq!(saturated.dropped_pairs, u64::MAX);
    }

    #[cfg(feature = "native-media")]
    const MOCK_RENDITION: RenditionId = RenditionId::new(std::num::NonZeroU32::new(1).unwrap());

    #[cfg(feature = "native-media")]
    fn stream_runtime(
        target: u128,
        ledger: StreamPairLedger,
        congested: bool,
        first_failure: Option<&str>,
        started: bool,
        state: Option<DestinationState>,
        network: Option<NetworkTelemetry>,
    ) -> StreamRuntimeSnapshot<'_> {
        StreamRuntimeSnapshot {
            target: SwitcherStreamTargetId::from_non_zero(
                NonZeroU128::new(target).expect("test target is nonzero"),
            ),
            ledger,
            congested,
            first_failure,
            started,
            state,
            network,
            local: None,
            uptime_ms: 0,
            bitrate_kbps: 0,
        }
    }

    #[cfg(feature = "native-media")]
    fn local_stream_runtime(
        target: u128,
        local: StreamLocalStatus,
    ) -> StreamRuntimeSnapshot<'static> {
        StreamRuntimeSnapshot {
            target: SwitcherStreamTargetId::from_non_zero(
                NonZeroU128::new(target).expect("test target is nonzero"),
            ),
            ledger: StreamPairLedger::default(),
            congested: false,
            first_failure: None,
            started: true,
            state: Some(DestinationState::Live),
            network: None,
            local: Some(local),
            uptime_ms: 0,
            bitrate_kbps: 0,
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn stream_status_maps_each_reachable_output_set_state() {
        let no_failure: Option<String> = None;
        let sample = |congested, state, network| {
            stream_status_sample(stream_runtime(
                7,
                StreamPairLedger {
                    enqueued_pairs: 30,
                    dropped_pairs: 4,
                    ..StreamPairLedger::default()
                },
                congested,
                no_failure.as_deref(),
                true,
                state,
                network,
            ))
            .expect("started runtimes always sample")
        };

        // Connecting and awaiting random access both mean "starting"; the
        // reconnected route is already connected, just not yet resuming.
        let connecting = sample(false, Some(DestinationState::Connecting), None);
        assert_eq!(connecting.realized, StreamRealizedState::Starting);
        assert!(!connecting.connected);
        let awaiting = sample(false, Some(DestinationState::AwaitingRandomAccess), None);
        assert_eq!(awaiting.realized, StreamRealizedState::Starting);
        assert!(awaiting.connected);

        // Producing: counters come from the ledger and bytes from telemetry.
        let live_set = driven_live_output_set();
        let id = DestinationId::new(1).unwrap();
        let network = live_set.telemetry(id).unwrap().clone();
        let live = sample(false, Some(DestinationState::Live), Some(network.clone()));
        assert_eq!(live.realized, StreamRealizedState::Live);
        assert!(live.connected);
        assert_eq!(
            live.muxed_bytes,
            network.bytes_sent(),
            "muxed bytes are the transport's own counter"
        );
        assert_eq!(live.enqueued_pairs, 30);
        assert_eq!(live.dropped_pairs, 4);
        assert_eq!(live.failure, None);

        // Backpressure observed this interval wins over Live.
        let congested = sample(true, Some(DestinationState::Live), Some(network));
        assert_eq!(congested.realized, StreamRealizedState::Congested);
        // The set's own congestion is congestion on the wire too.
        let transport_congested = sample(
            false,
            Some(DestinationState::Congested),
            Some(NetworkTelemetry::default()),
        );
        assert_eq!(transport_congested.realized, StreamRealizedState::Congested);

        // Waiting to reconnect is now a reachable wire state.
        let waiting = sample(
            false,
            Some(DestinationState::WaitingToReconnect {
                attempt: 2,
                retry_at_ms: 900,
            }),
            None,
        );
        assert_eq!(waiting.realized, StreamRealizedState::WaitingToReconnect);

        // Terminal states.
        let failed_by_state = sample(false, Some(DestinationState::Failed), None);
        assert_eq!(failed_by_state.realized, StreamRealizedState::Failed);
        let failing_set = driven_failing_output_set();
        let failing_network = failing_set.telemetry(id).unwrap().clone();
        let waiting_with_failure = sample(false, failing_set.state(id), Some(failing_network));
        assert_eq!(
            waiting_with_failure.realized,
            StreamRealizedState::WaitingToReconnect,
            "a recorded transient failure does not fail the target"
        );
        assert_eq!(
            waiting_with_failure.failure.as_deref(),
            Some("Write:muxed media clock stalled"),
            "transport failures are sanitized stage:message codes"
        );
        let stopped = sample(false, Some(DestinationState::Stopped), None);
        assert_eq!(stopped.realized, StreamRealizedState::Stopped);
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn latched_failure_is_sticky_over_live_state_and_survives_a_gone_slot() {
        let latched = Some("enqueue:FormatMismatch".to_owned());
        // The route still streams, but the feed already latched its failure.
        let sample = stream_status_sample(stream_runtime(
            9,
            StreamPairLedger {
                enqueued_pairs: 2,
                ..StreamPairLedger::default()
            },
            false,
            latched.as_deref(),
            true,
            Some(DestinationState::Live),
            None,
        ))
        .expect("latched runtimes stay on the wire");
        assert_eq!(sample.realized, StreamRealizedState::Failed);
        assert_eq!(sample.failure.as_deref(), Some("enqueue:FormatMismatch"));

        // A start-time failure leaves nothing running; counters still report.
        let start_failed = stream_status_sample(stream_runtime(
            11,
            StreamPairLedger {
                dropped_pairs: 1,
                ..StreamPairLedger::default()
            },
            false,
            Some("start:NotFound"),
            true,
            Some(DestinationState::Stopped),
            None,
        ))
        .expect("latched runtimes stay on the wire");
        assert_eq!(start_failed.realized, StreamRealizedState::Failed);
        assert!(!start_failed.connected);
        assert_eq!(start_failed.muxed_bytes, 0);
        assert_eq!(start_failed.dropped_pairs, 1);
        assert_eq!(start_failed.failure.as_deref(), Some("start:NotFound"));
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn inactive_targets_are_omitted_and_samples_sort_and_stay_bounded() {
        // A target that was never asked to run never appears.
        let empty = stream_status_samples([stream_runtime(
            5,
            StreamPairLedger::default(),
            false,
            None,
            false,
            Some(DestinationState::Stopped),
            None,
        )]);
        assert!(empty.is_empty(), "an all-inactive set publishes nothing");

        // Mixed set: unsorted input comes out in strictly ascending wire
        // order and inactive targets drop out of the middle.
        let latched = Some("backend:Some(ChildExited { status: Some(1) })".to_owned());
        let mixed = stream_status_samples([
            stream_runtime(
                90,
                StreamPairLedger::default(),
                false,
                None,
                false,
                Some(DestinationState::Stopped),
                None,
            ),
            stream_runtime(
                40,
                StreamPairLedger {
                    enqueued_pairs: 1,
                    ..StreamPairLedger::default()
                },
                false,
                None,
                true,
                Some(DestinationState::Live),
                None,
            ),
            stream_runtime(
                20,
                StreamPairLedger::default(),
                false,
                latched.as_deref(),
                true,
                Some(DestinationState::Stopped),
                None,
            ),
            stream_runtime(
                10,
                StreamPairLedger::default(),
                false,
                None,
                false,
                None,
                None,
            ),
        ]);
        let targets: Vec<_> = mixed
            .iter()
            .map(|sample| sample.target.get().get())
            .collect();
        assert_eq!(targets, [20, 40]);

        // The bound holds even if a project ever exceeded the engine cap.
        let flooded: Vec<_> = (0..=u128::try_from(MAX_STREAM_SAMPLES).unwrap())
            .map(|index| {
                stream_runtime(
                    index + 1,
                    StreamPairLedger::default(),
                    false,
                    latched.as_deref(),
                    true,
                    Some(DestinationState::Stopped),
                    None,
                )
            })
            .collect();
        assert_eq!(flooded.len(), MAX_STREAM_SAMPLES + 1);
        assert_eq!(stream_status_samples(flooded).len(), MAX_STREAM_SAMPLES);
    }

    /// One hand-written single-rendition media playlist with `segments` whole
    /// two-second segments.
    #[cfg(feature = "native-media")]
    fn media_playlist(segments: u32) -> String {
        use core::fmt::Write as _;
        let mut playlist = String::from(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n",
        );
        for index in 0..segments {
            let _ = write!(playlist, "#EXTINF:2.000000,\nsegment-{index:05}.ts\n");
        }
        playlist
    }

    /// One local HLS destination config over an absolute probe directory.
    #[cfg(feature = "native-media")]
    fn hls_destination(id: u8, directory: &str) -> DestinationConfig {
        DestinationConfig::new(
            DestinationId::new(id).unwrap(),
            OutputProtocol::Hls,
            Endpoint::local_directory(directory).unwrap(),
            None,
            None,
            None,
            QueueCapacity::new(1).unwrap(),
            ReconnectPolicy::new(
                PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS,
                PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS,
                PROGRAM_STREAM_RECONNECT_MULTIPLIER,
                Some(PROGRAM_STREAM_RECONNECT_ATTEMPTS),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn local_hls_status_reads_a_single_rendition_playlist() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("index.m3u8"), media_playlist(3)).unwrap();
        let status = local_hls_status(directory.path()).expect("a valid playlist samples");
        assert_eq!(status.segments, 3);
        assert_eq!(status.target_duration_ms, 2_000);
        // Free space is measured on the directory itself, through a pinned
        // query: concurrent tests write to the same file system, so two
        // real queries need not agree.
        let mut queried = None;
        let pinned = local_hls_status_with(directory.path(), |ancestor| {
            queried = Some(ancestor.to_path_buf());
            Some(123_456_789)
        })
        .expect("a valid playlist samples");
        assert_eq!(queried.as_deref(), Some(directory.path()));
        assert_eq!(pinned.free_bytes, 123_456_789);
        let unavailable =
            local_hls_status_with(directory.path(), |_| None).expect("a valid playlist samples");
        assert_eq!(unavailable.free_bytes, 0, "an unavailable query is zero");
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn local_hls_status_follows_the_first_master_variant() {
        use fm_frame::VideoDimensions;
        use fm_io_network::{HlsAbrCoordinator, HlsPlaylistType, HlsVariantMetadata};

        let directory = tempfile::tempdir().unwrap();
        let low = test_rendition(1);
        let high = test_rendition(2);
        let coordinator = HlsAbrCoordinator::new(
            vec![
                HlsVariantMetadata::new(
                    low,
                    "low",
                    1_000_000,
                    VideoDimensions::new(640, 360).unwrap(),
                    "avc1.64001e,mp4a.40.2",
                    "0/index.m3u8",
                )
                .unwrap(),
                HlsVariantMetadata::new(
                    high,
                    "high",
                    2_000_000,
                    VideoDimensions::new(1_280, 720).unwrap(),
                    "avc1.64002a,mp4a.40.2",
                    "1/index.m3u8",
                )
                .unwrap(),
            ],
            6,
            NormalizedDuration::from_nanos(2_000_000_000).unwrap(),
            0,
            6,
            HlsPlaylistType::Live,
        )
        .unwrap();
        fs::write(
            directory.path().join("index.m3u8"),
            coordinator.render_master_m3u8(),
        )
        .unwrap();

        // The first variant renders through the real playlist renderer; the
        // second carries different values to prove only the first is followed.
        let first = HlsPlaylistMetadata::parse_m3u8(&media_playlist(2), low, 1).unwrap();
        fs::create_dir(directory.path().join("0")).unwrap();
        fs::write(directory.path().join("0/index.m3u8"), first.render_m3u8()).unwrap();
        let second = HlsPlaylistMetadata::parse_m3u8(&media_playlist(5), high, 1).unwrap();
        fs::create_dir(directory.path().join("1")).unwrap();
        fs::write(directory.path().join("1/index.m3u8"), second.render_m3u8()).unwrap();

        let status =
            local_hls_status(directory.path()).expect("the master's first variant samples");
        assert_eq!(status.segments, 2);
        assert_eq!(status.target_duration_ms, 2_000);
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn local_hls_status_is_none_for_missing_or_invalid_playlists() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            local_hls_status(directory.path()),
            None,
            "a missing index.m3u8 is a sample gap"
        );

        fs::write(directory.path().join("index.m3u8"), "not a playlist\n").unwrap();
        assert_eq!(
            local_hls_status(directory.path()),
            None,
            "unparseable text is a sample gap"
        );

        // A segment longer than the target duration fails the bounded parser.
        fs::write(
            directory.path().join("index.m3u8"),
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:2.000000,\nsegment-00000.ts\n",
        )
        .unwrap();
        assert_eq!(
            local_hls_status(directory.path()),
            None,
            "an invalid playlist is a sample gap"
        );

        // A master whose first variant playlist is missing is also a gap.
        fs::write(
            directory.path().join("index.m3u8"),
            concat!(
                "#EXTM3U\n#EXT-X-VERSION:6\n",
                "#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360\n",
                "0/index.m3u8\n",
            ),
        )
        .unwrap();
        assert_eq!(
            local_hls_status(directory.path()),
            None,
            "a missing variant is a sample gap"
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn local_hls_status_rejects_escaping_master_variants() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("hls");
        fs::create_dir(&directory).unwrap();
        // A valid playlist outside the output directory proves the escaping
        // path is refused before any read, not merely because it is missing.
        fs::write(root.path().join("escape"), media_playlist(4)).unwrap();
        for variant in ["../escape", "/absolute/index.m3u8", "0\\index.m3u8"] {
            fs::write(
                directory.join("index.m3u8"),
                format!(
                    "#EXTM3U\n#EXT-X-VERSION:6\n\
                     #EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360\n{variant}\n"
                ),
            )
            .unwrap();
            assert_eq!(local_hls_status(&directory), None, "{variant}");
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn cached_local_health_is_projected_only_for_hls_targets() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        set.add_destination(hls_destination(2, "/srv/hls-probe"))
            .unwrap();
        let local = StreamLocalStatus {
            segments: 6,
            target_duration_ms: 6_000,
            free_bytes: 1_024,
        };
        let mut cache = [None; fm_switcher::MAX_STREAM_COUNT];
        cache[0] = Some(local);
        cache[1] = Some(local);
        assert_eq!(
            cached_local_hls_status(&set, &cache, DestinationId::new(1).unwrap()),
            None,
            "a network target never projects local health"
        );
        assert_eq!(
            cached_local_hls_status(&set, &cache, DestinationId::new(2).unwrap()),
            Some(local)
        );

        // The mapping carries the filtered cache entry onto the wire sample.
        let samples = stream_status_samples([
            stream_runtime(
                3,
                StreamPairLedger::default(),
                false,
                None,
                true,
                Some(DestinationState::Live),
                None,
            ),
            local_stream_runtime(4, local),
        ]);
        assert_eq!(samples[0].local, None);
        assert_eq!(samples[1].local, Some(local));
    }

    /// One scripted [`fm_io_network::TransportSink`] against the same public
    /// trait the real `FFmpeg` transport implements. Hermetic tests drive the
    /// daemon's actual output-set wiring through it: scripted connect and
    /// write results, disconnect counts, and which host each attempt chose.
    #[cfg(feature = "native-media")]
    #[derive(Default)]
    struct MockTransportSink {
        connect_results: std::collections::VecDeque<
            Result<fm_io_network::ConnectionObservation, fm_io_network::SinkError>,
        >,
        write_results:
            std::collections::VecDeque<Result<fm_io_network::SinkWrite, fm_io_network::SinkError>>,
        hosts: Vec<String>,
        disconnects: usize,
        /// Every packet the route attempted to write, cloned on arrival, so
        /// rendition-sharing tests can compare payloads across sinks.
        written: Vec<fm_io_network::OutputPacket>,
    }

    #[cfg(feature = "native-media")]
    impl fm_io_network::TransportSink for MockTransportSink {
        fn connect(
            &mut self,
            _config: &DestinationConfig,
            endpoint: &Endpoint,
        ) -> Result<fm_io_network::ConnectionObservation, fm_io_network::SinkError> {
            self.hosts.push(endpoint.host().to_owned());
            self.connect_results
                .pop_front()
                .unwrap_or_else(|| Ok(fm_io_network::ConnectionObservation::default()))
        }

        fn write(
            &mut self,
            packet: &fm_io_network::OutputPacket,
        ) -> Result<fm_io_network::SinkWrite, fm_io_network::SinkError> {
            self.written.push(packet.clone());
            self.write_results.pop_front().unwrap_or_else(|| {
                Ok(fm_io_network::SinkWrite::Sent(
                    fm_io_network::SendObservation::default(),
                ))
            })
        }

        fn disconnect(&mut self) {
            self.disconnects += 1;
        }
    }

    #[cfg(feature = "native-media")]
    fn mock_destination(id: u8) -> DestinationConfig {
        DestinationConfig::new(
            DestinationId::new(id).unwrap(),
            OutputProtocol::Rtmp,
            Endpoint::new("primary.example.test", 19_35, "/live").unwrap(),
            Some(Endpoint::new("backup.example.test", 19_35, "/live").unwrap()),
            None,
            None,
            QueueCapacity::new(1).unwrap(),
            ReconnectPolicy::new(
                PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS,
                PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS,
                PROGRAM_STREAM_RECONNECT_MULTIPLIER,
                Some(PROGRAM_STREAM_RECONNECT_ATTEMPTS),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[cfg(feature = "native-media")]
    fn mock_packet(sequence: u64) -> fm_io_network::OutputPacket {
        mock_packet_for(MOCK_RENDITION, sequence, 7)
    }

    #[cfg(feature = "native-media")]
    fn mock_packet_for(
        rendition: RenditionId,
        sequence: u64,
        payload_byte: u8,
    ) -> fm_io_network::OutputPacket {
        use fm_frame::{NormalizedDuration, NormalizedTimestamp};
        fm_io_network::OutputPacket::new(
            rendition,
            sequence,
            NormalizedTimestamp::from_nanos(i64::try_from(sequence).unwrap() * 33_333_333),
            NormalizedDuration::from_nanos(33_333_333).unwrap(),
            true,
            vec![payload_byte; 16],
        )
        .unwrap()
    }

    /// One rendition group's planner state over hermetic project-format
    /// settings, mirroring what `NativeProgramStream::compile` builds.
    #[cfg(feature = "native-media")]
    fn test_group(
        rendition_value: u32,
        members: &[u8],
        video_bitrate_kbps: u32,
        group_index: usize,
        group_count: usize,
    ) -> StreamRenditionCore {
        StreamRenditionCore {
            output: fm_types::OutputId::new(NonZeroU128::new(1).expect("test output is nonzero")),
            members: members
                .iter()
                .map(|id| DestinationId::new(*id).expect("test member fits a slot"))
                .collect(),
            rendition: RenditionId::new(
                NonZeroU32::new(rendition_value).expect("test rendition is nonzero"),
            ),
            variants: pinned_rendition_variants(
                &test_stream_format(),
                video_bitrate_kbps,
                group_index,
                group_count,
            )
            .expect("test rendition variants validate"),
            next_sequence: 0,
        }
    }

    /// Drives one group through the exact production frame pipeline — gate,
    /// plan, pack, fan out, admit — with the pack step scripted.
    #[cfg(feature = "native-media")]
    fn run_test_group_frame(
        core: &mut StreamRenditionCore,
        output_set: &mut OutputSet,
        streams: &mut [(DestinationId, StreamPairLedger, bool)],
        sequence: SequenceNumber,
        active: &[bool],
        pack: impl FnOnce() -> Result<fm_io_network::OutputPacket, String>,
    ) -> GroupFrameOutcome {
        let outcome = plan_and_pack_group_frame(
            &core.members,
            &core.variants,
            &mut core.next_sequence,
            sequence,
            active,
            pack,
        );
        if let GroupFrameOutcome::Packed { plan, packet } = outcome {
            let outcomes = output_set
                .enqueue_rendition(&plan, &packet)
                .expect("the packed rendition is always planned");
            let mut routes: Vec<(DestinationId, &mut StreamPairLedger, &mut bool)> = streams
                .iter_mut()
                .filter(|(destination, _, _)| core.members.contains(destination))
                .map(|(destination, ledger, backpressure)| (*destination, ledger, backpressure))
                .collect();
            admit_rendition_outcomes(&outcomes, sequence, &mut routes);
            return GroupFrameOutcome::Packed { plan, packet };
        }
        outcome
    }

    /// Drives one real output set to Live with one sent packet, so status
    /// assertions read genuine telemetry instead of invented values.
    #[cfg(feature = "native-media")]
    fn driven_live_output_set() -> OutputSet {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let id = DestinationId::new(1).unwrap();
        let mut sink = MockTransportSink::default();
        set.start(id).unwrap();
        set.poll(id, 0, &mut sink).unwrap();
        set.enqueue(id, mock_packet(0)).unwrap();
        set.poll(id, 1, &mut sink).unwrap();
        set
    }

    /// Drives one real output set into a recorded transient write failure.
    #[cfg(feature = "native-media")]
    fn driven_failing_output_set() -> OutputSet {
        use fm_io_network::{FailureStage, SinkError};
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let id = DestinationId::new(1).unwrap();
        let mut sink = MockTransportSink {
            write_results: std::collections::VecDeque::from([Err(SinkError::new(
                FailureStage::Write,
                None,
                "muxed media clock stalled",
                true,
            ))]),
            ..MockTransportSink::default()
        };
        set.start(id).unwrap();
        set.poll(id, 0, &mut sink).unwrap();
        set.enqueue(id, mock_packet(1)).unwrap();
        set.poll(id, 10, &mut sink).unwrap();
        set
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn output_set_reaches_live_through_a_started_target() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let id = DestinationId::new(1).unwrap();
        let mut sink = MockTransportSink::default();

        // Before start the slot is stopped; start arms it for connecting.
        assert_eq!(set.state(id), Some(DestinationState::Stopped));
        set.start(id).unwrap();
        assert_eq!(set.state(id), Some(DestinationState::Connecting));
        assert_eq!(
            set.poll(id, 0, &mut sink).unwrap(),
            PollEvent::Connected,
            "the scripted sink accepts the first connect"
        );
        assert_eq!(set.state(id), Some(DestinationState::Live));

        set.enqueue(id, mock_packet(0)).unwrap();
        assert_eq!(
            set.poll(id, 1, &mut sink).unwrap(),
            PollEvent::PacketSent { sequence: 0 }
        );
        assert_eq!(set.telemetry(id).unwrap().packets_sent(), 1);
        assert_eq!(set.telemetry(id).unwrap().bytes_sent(), 16);
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn scripted_write_failure_schedules_reconnection_on_the_backup_endpoint() {
        use fm_io_network::ConnectionTarget;
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let id = DestinationId::new(1).unwrap();
        let mut sink = MockTransportSink {
            write_results: std::collections::VecDeque::from([Err(fm_io_network::SinkError::new(
                fm_io_network::FailureStage::Write,
                Some(104),
                "connection reset by peer",
                true,
            ))]),
            ..MockTransportSink::default()
        };
        set.start(id).unwrap();
        set.poll(id, 0, &mut sink).unwrap();
        set.enqueue(id, mock_packet(1)).unwrap();

        // The write failure costs one attempt and flips to the backup.
        let event = set.poll(id, 10, &mut sink).unwrap();
        let PollEvent::ReconnectScheduled { retry_at_ms } = event else {
            panic!("expected a scheduled reconnect, got {event:?}");
        };
        assert_eq!(retry_at_ms, 10 + PROGRAM_STREAM_RECONNECT_INITIAL_DELAY_MS);
        assert_eq!(
            set.state(id),
            Some(DestinationState::WaitingToReconnect {
                attempt: 1,
                retry_at_ms
            })
        );
        assert_eq!(set.connection_target(id), Some(ConnectionTarget::Backup));

        // Backoff holds until the retry instant, then the backup endpoint is
        // contacted and the queued pair replays after it reopens.
        assert_eq!(
            set.poll(id, retry_at_ms - 1, &mut sink).unwrap(),
            PollEvent::WaitingToReconnect { retry_at_ms }
        );
        assert_eq!(
            set.poll(id, retry_at_ms, &mut sink).unwrap(),
            PollEvent::AwaitingRandomAccess {
                dropped_packets: 0,
                dropped_bytes: 0,
            },
            "a recovery replays from the next random-access packet"
        );
        assert_eq!(
            sink.hosts,
            ["primary.example.test", "backup.example.test"],
            "the first failure must move the destination to its backup"
        );
        // Connecting is not yet proof: the recovery counts and the retry
        // budget clears only once the queued pair is delivered.
        assert_eq!(set.telemetry(id).unwrap().reconnects(), 0);
        assert_eq!(
            set.poll(id, retry_at_ms, &mut sink).unwrap(),
            PollEvent::PacketSent { sequence: 1 }
        );
        assert_eq!(set.telemetry(id).unwrap().reconnects(), 1);
    }

    /// A target that latched a failure must come back when the operator stops
    /// and starts it again, exactly like a recorder slot does.
    #[cfg(feature = "native-media")]
    #[test]
    fn operator_stop_then_start_relaunches_a_failed_stream_target() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let destination_id = DestinationId::new(1).unwrap();
        let mut stream = NativeProgramStream {
            target: SwitcherStreamTargetId::from_non_zero(NonZeroU128::new(7).unwrap()),
            destination_id,
            redacted_destination: "rtmp://primary.example.test:1935/live".to_owned(),
            sink: stream_transport::BackgroundTransport::new(
                FfmpegRtmpSink::new(RtmpSinkConfig::new(test_rendition(1), test_stream_format())),
                "freemix-native-stream-test".to_owned(),
            ),
            ledger: StreamPairLedger::default(),
            first_failure: None,
            enqueue_backpressure: false,
            started: false,
            meter: StreamLinkMeter::default(),
            authored: test_authored_target(7),
        };
        stream.start(&mut set);
        stream.fail("transport:budget_exhausted", &mut set);
        assert!(!route_active(&stream, &set));

        stream.stop_requested(&mut set);
        stream.start(&mut set);

        assert_eq!(
            set.state(destination_id),
            Some(DestinationState::Connecting)
        );
        assert!(route_active(&stream, &set));
    }

    /// One authored RTMP destination for runtime unit tests.
    #[cfg(feature = "native-media")]
    fn test_authored_target(id: u128) -> fm_model::StreamTarget {
        fm_model::StreamTarget::new(
            fm_model::StreamTargetId::new(NonZeroU128::new(id).unwrap()),
            format!("Destination {id}"),
            fm_model::StreamProtocol::Rtmp,
            fm_model::StreamEndpoint::parse_for(
                fm_model::StreamProtocol::Rtmp,
                "primary.example.test:1935/live",
            )
            .unwrap(),
            Some(fm_model::StreamKey::parse("live-key-0001").unwrap()),
            fm_types::OutputId::new(NonZeroU128::new(30).unwrap()),
        )
        .unwrap()
    }

    #[cfg(feature = "native-media")]
    fn test_stream_format() -> RecordFormat {
        RecordFormat::new(
            16,
            16,
            FrameRate::new(25, 1).unwrap(),
            SampleRate::new(44_100).unwrap(),
            ChannelLayout::stereo(),
            SequenceNumber::new(0),
        )
        .unwrap()
    }

    #[cfg(feature = "native-media")]
    fn test_rendition(value: u32) -> RenditionId {
        RenditionId::new(NonZeroU32::new(value).expect("test rendition is nonzero"))
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn queue_full_refusals_are_dropped_once_and_reported_as_congestion() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let id = DestinationId::new(1).unwrap();
        let mut sink = MockTransportSink::default();
        set.start(id).unwrap();
        set.poll(id, 0, &mut sink).unwrap();

        let mut core = test_group(1, &[1], 4_500, 0, 1);
        let mut rows = vec![(id, StreamPairLedger::default(), false)];

        // Capacity is one: the first pair rides, the second is refused,
        // counted as a drop, flagged as backpressure, and never retried.
        run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(0),
            &[true],
            || Ok(mock_packet(0)),
        );
        run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(1),
            &[true],
            || Ok(mock_packet(1)),
        );

        let (_, ledger, backpressure) = rows[0];
        assert!(backpressure);
        assert_eq!(ledger.enqueued_pairs, 1);
        assert_eq!(ledger.dropped_pairs, 1);
        assert_eq!(set.queue_depth(id), Some(1));
        assert_eq!(set.state(id), Some(DestinationState::Congested));
        let realized = realized_stream_state(false, backpressure, set.state(id));
        assert_eq!(realized, StreamRealizedState::Congested);

        // The group consumed one sequence per frame even though the second
        // pair was refused; the member's admitted cursor advanced only once.
        assert_eq!(core.next_sequence, 2);
        assert_eq!(ledger.next_sequence, 1);
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn behind_cursor_frames_are_never_packed_or_fanned_out() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let id = DestinationId::new(1).unwrap();
        let mut sink = MockTransportSink::default();
        set.start(id).unwrap();
        set.poll(id, 0, &mut sink).unwrap();

        let mut core = test_group(1, &[1], 4_500, 0, 1);
        core.next_sequence = 9;
        let mut rows = vec![(id, StreamPairLedger::default(), false)];

        // A frame behind the shared cursor costs nothing at all: no pack, no
        // fan-out, no counter movement.
        let outcome = run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(8),
            &[true],
            || panic!("a consumed sequence must never be packed again"),
        );
        assert!(matches!(outcome, GroupFrameOutcome::Idle));
        assert_eq!(sink.written.len(), 0);
        assert_eq!(set.queue_depth(id), Some(0));
        assert_eq!(rows[0].1.enqueued_pairs, 0);
        assert_eq!(rows[0].1.dropped_pairs, 0);
        assert!(!rows[0].2);
        assert_eq!(core.next_sequence, 9, "an idle frame leaves the cursor");

        // The current sequence packs and queues normally.
        let packed = run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(9),
            &[true],
            || Ok(mock_packet(9)),
        );
        assert!(matches!(packed, GroupFrameOutcome::Packed { .. }));
        assert_eq!(
            sink.written.len(),
            0,
            "the pair waits for the route to poll"
        );
        assert_eq!(set.queue_depth(id), Some(1));
        assert_eq!(core.next_sequence, 10);
        assert_eq!(rows[0].1.enqueued_pairs, 1);
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn stop_disconnects_exactly_once_and_freezes_the_slot() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let id = DestinationId::new(1).unwrap();
        let mut sink = MockTransportSink::default();
        set.start(id).unwrap();
        set.poll(id, 0, &mut sink).unwrap();
        set.enqueue(id, mock_packet(0)).unwrap();

        set.stop(id, &mut sink).unwrap();
        assert_eq!(sink.disconnects, 1);
        assert_eq!(set.state(id), Some(DestinationState::Stopped));
        assert_eq!(set.queue_depth(id), Some(0));
        // Telemetry survives the stop so final records keep whole-run counts.
        assert_eq!(set.telemetry(id).unwrap().packets_accepted(), 1);
    }

    /// Two destinations sharing their protocol and authored bitrate form ONE
    /// rendition group: the expensive pack step runs exactly once per frame,
    /// both sinks receive byte-identical payloads, and each member's ledger
    /// advances while the group consumes one sequence per frame.
    #[cfg(feature = "native-media")]
    #[test]
    fn same_bitrate_destinations_share_one_pack_per_frame() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        set.add_destination(mock_destination(2)).unwrap();
        let id_a = DestinationId::new(1).unwrap();
        let id_b = DestinationId::new(2).unwrap();
        let mut sink_a = MockTransportSink::default();
        let mut sink_b = MockTransportSink::default();
        set.start(id_a).unwrap();
        set.start(id_b).unwrap();
        set.poll(id_a, 0, &mut sink_a).unwrap();
        set.poll(id_b, 0, &mut sink_b).unwrap();

        let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
        let mut rows = vec![
            (id_a, StreamPairLedger::default(), false),
            (id_b, StreamPairLedger::default(), false),
        ];
        let mut packs = Vec::new();

        for sequence in [41_u64, 42] {
            let packed = run_test_group_frame(
                &mut core,
                &mut set,
                &mut rows,
                SequenceNumber::new(sequence),
                &[true, true],
                || {
                    packs.push(sequence);
                    Ok(mock_packet_for(
                        MOCK_RENDITION,
                        sequence,
                        u8::try_from(sequence).unwrap_or(u8::MAX),
                    ))
                },
            );
            assert!(matches!(packed, GroupFrameOutcome::Packed { .. }));
            // Mirrors advance_stream_routes: the shared set drains each route
            // after the frame's fan-out.
            let now_ms = sequence * 33;
            set.poll(id_a, now_ms, &mut sink_a).unwrap();
            set.poll(id_b, now_ms, &mut sink_b).unwrap();
        }

        // One pack per frame feeds the whole group.
        assert_eq!(packs, [41, 42]);
        for (sink, name) in [(&sink_a, "a"), (&sink_b, "b")] {
            assert_eq!(sink.written.len(), 2, "{name}");
            for (index, expected) in [41_u64, 42].into_iter().enumerate() {
                let packet = &sink.written[index];
                let expected_byte = u8::try_from(expected).unwrap_or(u8::MAX);
                assert_eq!(packet.sequence(), expected, "{name}");
                assert_eq!(packet.rendition(), MOCK_RENDITION, "{name}");
                assert!(
                    packet.payload().iter().all(|&byte| byte == expected_byte),
                    "{name} frame {expected} carries the scripted payload"
                );
            }
        }
        assert_eq!(
            sink_a
                .written
                .iter()
                .map(fm_io_network::OutputPacket::payload)
                .collect::<Vec<_>>(),
            sink_b
                .written
                .iter()
                .map(fm_io_network::OutputPacket::payload)
                .collect::<Vec<_>>(),
            "both members receive the very same packed pair"
        );

        // Both members' ledgers advanced per frame; nobody dropped anything.
        for (destination, ledger, backpressure) in &rows {
            assert_eq!(ledger.enqueued_pairs, 2, "{destination}");
            assert_eq!(ledger.dropped_pairs, 0, "{destination}");
            assert!(!backpressure, "{destination}");
            assert_eq!(ledger.next_sequence, 43, "{destination}");
        }
        assert_eq!(core.next_sequence, 43, "one sequence consumed per frame");
    }

    /// Distinct authored bitrates stay independent: two groups pack once each
    /// per frame, every plan numbers its real rendition at the compile-time
    /// group id (the inert sibling rungs exist purely to hold that position),
    /// and neither sink ever sees the other group's payload.
    #[cfg(feature = "native-media")]
    #[test]
    fn distinct_bitrates_pack_into_two_independent_groups() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        set.add_destination(mock_destination(2)).unwrap();
        let id_a = DestinationId::new(1).unwrap();
        let id_b = DestinationId::new(2).unwrap();
        let mut sink_a = MockTransportSink::default();
        let mut sink_b = MockTransportSink::default();
        set.start(id_a).unwrap();
        set.start(id_b).unwrap();
        set.poll(id_a, 0, &mut sink_a).unwrap();
        set.poll(id_b, 0, &mut sink_b).unwrap();

        // Authored order: target A at 3000 kbps is group one, target B at
        // 6000 kbps is group two.
        let mut low = test_group(1, &[1], 3_000, 0, 2);
        let mut high = test_group(2, &[2], 6_000, 1, 2);
        let mut rows = vec![
            (id_a, StreamPairLedger::default(), false),
            (id_b, StreamPairLedger::default(), false),
        ];
        let (mut low_packs, mut high_packs) = (0_usize, 0_usize);

        let low_outcome = run_test_group_frame(
            &mut low,
            &mut set,
            &mut rows,
            SequenceNumber::new(7),
            &[true],
            || {
                low_packs += 1;
                Ok(mock_packet_for(test_rendition(1), 7, 1))
            },
        );
        let high_outcome = run_test_group_frame(
            &mut high,
            &mut set,
            &mut rows,
            SequenceNumber::new(7),
            &[true],
            || {
                high_packs += 1;
                Ok(mock_packet_for(test_rendition(2), 7, 2))
            },
        );
        assert_eq!(
            (low_packs, high_packs),
            (1, 1),
            "each group packs its own pair"
        );
        // Mirrors advance_stream_routes after the frame's fan-out.
        set.poll(id_a, 7, &mut sink_a).unwrap();
        set.poll(id_b, 7, &mut sink_b).unwrap();

        let GroupFrameOutcome::Packed { plan: low_plan, .. } = low_outcome else {
            panic!("the low group packed");
        };
        assert_eq!(
            low_plan.destinations_for(test_rendition(1)),
            Some([id_a].as_slice())
        );
        let GroupFrameOutcome::Packed {
            plan: high_plan, ..
        } = high_outcome
        else {
            panic!("the high group packed");
        };
        assert_eq!(
            high_plan.destinations_for(test_rendition(2)),
            Some([id_b].as_slice()),
            "the inert sibling rung pins the real rendition at the compile-time id"
        );

        // Every sink saw exactly its own group's payload and rendition.
        assert_eq!(sink_a.written.len(), 1);
        assert_eq!(sink_a.written[0].rendition(), test_rendition(1));
        assert!(sink_a.written[0].payload().iter().all(|&byte| byte == 1));
        assert_eq!(sink_b.written.len(), 1);
        assert_eq!(sink_b.written[0].rendition(), test_rendition(2));
        assert!(sink_b.written[0].payload().iter().all(|&byte| byte == 2));

        assert_eq!(rows[0].1.enqueued_pairs, 1);
        assert_eq!(rows[1].1.enqueued_pairs, 1);
        assert!(!rows[0].2 && !rows[1].2);
    }

    /// One member stopping mid-show leaves the plan, keeps its ledger frozen,
    /// and never sees another packet; restarting it resumes under the same
    /// group identity with the next deliverable sequences while the surviving
    /// member never interrupted.
    #[cfg(feature = "native-media")]
    #[test]
    fn stopped_member_is_skipped_and_restart_resumes_the_group() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        set.add_destination(mock_destination(2)).unwrap();
        let id_a = DestinationId::new(1).unwrap();
        let id_b = DestinationId::new(2).unwrap();
        let mut sink_a = MockTransportSink::default();
        let mut sink_b = MockTransportSink::default();
        set.start(id_a).unwrap();
        set.start(id_b).unwrap();
        set.poll(id_a, 0, &mut sink_a).unwrap();
        set.poll(id_b, 0, &mut sink_b).unwrap();

        let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
        let mut rows = vec![
            (id_a, StreamPairLedger::default(), false),
            (id_b, StreamPairLedger::default(), false),
        ];

        // Frame five reaches both members.
        run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(5),
            &[true, true],
            || Ok(mock_packet(5)),
        );
        set.poll(id_a, 5, &mut sink_a).unwrap();
        set.poll(id_b, 5, &mut sink_b).unwrap();
        assert_eq!(sink_a.written.len(), 1);
        assert_eq!(sink_b.written.len(), 1);

        // Target B stops mid-show: it leaves the plan entirely, its ledger
        // freezes, and A keeps streaming without interruption.
        set.stop(id_b, &mut sink_b).unwrap();
        let packed = run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(6),
            &[true, false],
            || Ok(mock_packet(6)),
        );
        let GroupFrameOutcome::Packed { plan, .. } = packed else {
            panic!("the surviving member keeps the group packing");
        };
        assert_eq!(
            plan.destinations_for(MOCK_RENDITION),
            Some([id_a].as_slice()),
            "the plan covers exactly the still-active members"
        );
        set.poll(id_a, 6, &mut sink_a).unwrap();
        assert_eq!(sink_a.written.len(), 2);
        assert_eq!(sink_b.written.len(), 1, "a stopped route receives nothing");
        assert_eq!(rows[0].1.enqueued_pairs, 2);
        assert_eq!(rows[1].1.enqueued_pairs, 1, "the stopped ledger freezes");
        assert_eq!(rows[1].1.dropped_pairs, 0);
        assert!(!rows[1].2);

        // Restarting B resumes under the same group identity with the next
        // deliverable sequences; the sink pads the gap frame six skipped.
        set.start(id_b).unwrap();
        set.poll(id_b, 10, &mut sink_b).unwrap();
        run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(7),
            &[true, true],
            || Ok(mock_packet(7)),
        );
        set.poll(id_a, 7, &mut sink_a).unwrap();
        set.poll(id_b, 7, &mut sink_b).unwrap();
        assert_eq!(sink_b.written.len(), 2);
        assert_eq!(sink_b.written.last().unwrap().sequence(), 7);
        assert_eq!(rows[1].1.enqueued_pairs, 2);
        assert_eq!(rows[1].1.next_sequence, 8);
        assert_eq!(core.next_sequence, 8);
    }

    /// Backpressure on one member's route flags and congests only that
    /// target; the other member of the same group streams clean.
    #[cfg(feature = "native-media")]
    #[test]
    fn a_congested_route_flags_only_that_target() {
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        set.add_destination(mock_destination(2)).unwrap();
        let id_a = DestinationId::new(1).unwrap();
        let id_b = DestinationId::new(2).unwrap();
        let mut sink_a = MockTransportSink::default();
        let mut sink_b = MockTransportSink::default();
        set.start(id_a).unwrap();
        set.start(id_b).unwrap();
        set.poll(id_a, 0, &mut sink_a).unwrap();
        set.poll(id_b, 0, &mut sink_b).unwrap();

        let mut core = test_group(1, &[1, 2], 4_500, 0, 1);
        let mut rows = vec![
            (id_a, StreamPairLedger::default(), false),
            (id_b, StreamPairLedger::default(), false),
        ];

        // Frame zero fills both single-slot queues.
        run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(0),
            &[true, true],
            || Ok(mock_packet(0)),
        );

        // Only B drains between frames; A stays saturated.
        set.poll(id_b, 1, &mut sink_b).unwrap();

        run_test_group_frame(
            &mut core,
            &mut set,
            &mut rows,
            SequenceNumber::new(1),
            &[true, true],
            || Ok(mock_packet(1)),
        );

        let (_, ledger_a, flag_a) = rows[0];
        let (_, ledger_b, flag_b) = rows[1];
        assert!(flag_a);
        assert!(!flag_b, "only the saturated route flags backpressure");
        assert_eq!(ledger_a.enqueued_pairs, 1);
        assert_eq!(ledger_a.dropped_pairs, 1);
        assert_eq!(ledger_b.enqueued_pairs, 2);
        assert_eq!(ledger_b.dropped_pairs, 0);

        // Congestion is per-target on the wire too.
        assert_eq!(set.state(id_a), Some(DestinationState::Congested));
        assert_eq!(
            realized_stream_state(false, flag_a, set.state(id_a)),
            StreamRealizedState::Congested
        );
        assert_eq!(
            realized_stream_state(false, flag_b, set.state(id_b)),
            StreamRealizedState::Live
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn exhausted_retry_budget_latches_the_target_failed() {
        use fm_io_network::{FailureStage, SinkError};
        let mut set = OutputSet::new();
        set.add_destination(mock_destination(1)).unwrap();
        let id = DestinationId::new(1).unwrap();
        let mut sink = MockTransportSink {
            connect_results: (0..=PROGRAM_STREAM_RECONNECT_ATTEMPTS)
                .map(|_| {
                    Err(SinkError::new(
                        FailureStage::Connect,
                        Some(111),
                        "connection refused",
                        true,
                    ))
                })
                .collect(),
            ..MockTransportSink::default()
        };
        set.start(id).unwrap();
        let mut now_ms = 0_u64;
        loop {
            if set.poll(id, now_ms, &mut sink).unwrap() == PollEvent::Failed {
                break;
            }
            now_ms += PROGRAM_STREAM_RECONNECT_MAX_DELAY_MS;
        }
        assert_eq!(set.state(id), Some(DestinationState::Failed));
        let telemetry = set.telemetry(id).unwrap();
        assert_eq!(
            telemetry.connect_attempts(),
            u64::from(PROGRAM_STREAM_RECONNECT_ATTEMPTS) + 1,
            "the initial attempt plus every budgeted retry ran"
        );
        assert!(
            telemetry.latest_failure().unwrap().retryable,
            "refused connections stay transient; only the budget ends them"
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn split_destination_url_round_trips_every_protocol_convention() {
        // (authored URL, protocol, host, port, endpoint path, key tail). The
        // key is restated here because the split result deliberately cannot
        // be read back in the clear.
        for (url, protocol, host, port, path, key) in [
            (
                "rtmp://ingest.example.test:1935/live_app/live-key-0001",
                OutputProtocol::Rtmp,
                "ingest.example.test",
                1_935_u16,
                "/live_app",
                Some("live-key-0001"),
            ),
            (
                "rtmps://ingest.example.test:1936/a/b/key12345?token=abcdef",
                OutputProtocol::Rtmps,
                "ingest.example.test",
                1_936,
                "/a/b",
                Some("key12345?token=abcdef"),
            ),
            (
                "srt://ingest.example.test:9710?streamid=live-key-0001",
                OutputProtocol::Srt,
                "ingest.example.test",
                9_710,
                "/",
                Some("live-key-0001"),
            ),
            (
                "srt://ingest.example.test:9710",
                OutputProtocol::Srt,
                "ingest.example.test",
                9_710,
                "/",
                None,
            ),
            (
                "hls:///var/lib/freemix/live",
                OutputProtocol::Hls,
                // A local destination has no peer: the placeholder host and
                // port are never rendered, and the directory is the path.
                "localhost",
                1,
                "/var/lib/freemix/live",
                None,
            ),
        ] {
            let (split_protocol, endpoint, split_key) = split_destination_url(url).expect(url);
            assert_eq!(split_protocol, protocol, "{url:?}");
            assert_eq!(endpoint.host(), host);
            assert_eq!(u16::from(endpoint.port()), port);
            assert_eq!(endpoint.path(), path);
            assert_eq!(
                split_key.is_some(),
                key.is_some(),
                "{url:?} keeps its key presence"
            );

            // The convention round-trips: composing the pieces back together
            // under the transport's rules yields a URL the codec parser — the
            // single validity source — accepts again.
            let composed = match (protocol, key) {
                (OutputProtocol::Rtmp, Some(key)) => {
                    format!("rtmp://{host}:{port}{path}/{key}")
                }
                (OutputProtocol::Rtmps, Some(key)) => {
                    format!("rtmps://{host}:{port}{path}/{key}")
                }
                (OutputProtocol::Srt, Some(key)) => format!("srt://{host}:{port}?streamid={key}"),
                (OutputProtocol::Srt, None) => format!("srt://{host}:{port}"),
                (OutputProtocol::Hls, None) => format!("hls://{path}"),
                (OutputProtocol::Hls, Some(_)) => {
                    unreachable!("HLS destinations carry no stream key")
                }
                (OutputProtocol::LiveLan, _) => {
                    unreachable!("no daemon streaming transport for LAN outputs")
                }
                (_, None) => unreachable!("network destinations always carry a key here"),
            };
            fm_codec_ffmpeg::stream::StreamDestination::parse(&composed)
                .unwrap_or_else(|error| panic!("{composed:?} does not re-parse: {error:?}"));
        }

        // Shapes that cannot be represented fail compilation instead of being
        // guessed at air time.
        for url in [
            "rtmp://host.example.test/app/key12345",
            "rtmp://host.example.test:1935",
            "srt://host.example.test?param=key12345",
            "http://host.example.test:1935/app/key12345",
            "srt://host.example.test:70000?streamid=key12345",
            "hls://relative/dir",
            "hls:///srv/../etc",
            "hls:///srv//live",
            "hls:///srv/live/",
            "hls://",
        ] {
            assert!(
                split_destination_url(url).is_err(),
                "{url:?} must be refused"
            );
        }
    }

    /// An SRT stream id is one `?streamid=` query value, so query separators
    /// in it are refused with the typed reason and never echoed; the same key
    /// text stays legal as an RTMP path segment.
    #[cfg(feature = "native-media")]
    #[test]
    fn split_destination_url_validates_the_key_for_its_protocol() {
        for url in [
            "srt://ingest.example.test:9710?streamid=live-key&latency=1",
            "srt://ingest.example.test:9710?streamid=live-key?0001",
        ] {
            let Err(error) = split_destination_url(url) else {
                panic!("{url:?} must be refused");
            };
            let message = error.to_string();
            assert!(
                message.contains("stream destination key was refused")
                    && message.contains("SRT stream id"),
                "{message}"
            );
            assert!(!message.contains("live-key"), "{message}");
        }
        let (_, _, key) =
            split_destination_url("rtmp://ingest.example.test:1935/live/live-key&x=1").unwrap();
        assert!(key.is_some());
        // A key no protocol can carry keeps its own reason.
        let error = split_destination_url("srt://ingest.example.test:9710?streamid=abc")
            .unwrap_err()
            .to_string();
        assert!(error.contains("redactable"), "{error}");
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn redacted_destination_url_masks_keys_and_renders_hls_paths_verbatim() {
        let network = Endpoint::new("ingest.example.test", 1_935, "/live_app").unwrap();
        assert_eq!(
            redacted_destination_url(OutputProtocol::Rtmp, &network, true),
            "rtmp://ingest.example.test:1935/live_app/****"
        );
        assert_eq!(
            redacted_destination_url(OutputProtocol::Rtmp, &network, false),
            "rtmp://ingest.example.test:1935/live_app"
        );
        assert_eq!(
            redacted_destination_url(OutputProtocol::Rtmps, &network, true),
            "rtmps://ingest.example.test:1935/live_app/****"
        );
        let srt = Endpoint::new("ingest.example.test", 9_710, "/").unwrap();
        assert_eq!(
            redacted_destination_url(OutputProtocol::Srt, &srt, true),
            "srt://ingest.example.test:9710?streamid=****"
        );
        assert_eq!(
            redacted_destination_url(OutputProtocol::Srt, &srt, false),
            "srt://ingest.example.test:9710"
        );
        // A local HLS directory is operator-chosen storage, not a secret: it
        // renders verbatim no matter what key presence the caller reports.
        let hls = Endpoint::local_directory("/var/lib/freemix/live").unwrap();
        assert_eq!(
            redacted_destination_url(OutputProtocol::Hls, &hls, false),
            "hls:///var/lib/freemix/live"
        );
        assert_eq!(
            redacted_destination_url(OutputProtocol::Hls, &hls, true),
            "hls:///var/lib/freemix/live"
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn pending_network_probe_skips_hls_and_maps_network_endpoints() {
        let rtmp = DestinationConfig::new(
            DestinationId::new(1).unwrap(),
            OutputProtocol::Rtmp,
            Endpoint::new("ingest.example.test", 1_935, "/live").unwrap(),
            Some(Endpoint::new("backup.example.test", 1_935, "/live").unwrap()),
            None,
            None,
            QueueCapacity::new(1).unwrap(),
            ReconnectPolicy::disabled(),
        )
        .unwrap();

        let primary = pending_network_probe(&rtmp, 7, preflight::PRIMARY_ENDPOINT)
            .expect("an RTMP destination has a primary endpoint to probe");
        assert_eq!(primary.target, 7);
        assert_eq!(primary.endpoint, preflight::PRIMARY_ENDPOINT);
        assert_eq!(primary.host, "ingest.example.test");
        assert_eq!(u16::from(primary.port), 1_935);
        assert_eq!(primary.timeout, preflight::NETWORK_PREFLIGHT_TIMEOUT);

        let backup = pending_network_probe(&rtmp, 7, preflight::BACKUP_ENDPOINT)
            .expect("a configured backup endpoint is probed too");
        assert_eq!(backup.target, 7);
        assert_eq!(backup.endpoint, preflight::BACKUP_ENDPOINT);
        assert_eq!(backup.host, "backup.example.test");
        assert_eq!(u16::from(backup.port), 1_935);

        // A local HLS destination is a directory write, not a peer, so it has
        // no network endpoint to probe on either slot.
        let hls = DestinationConfig::new(
            DestinationId::new(2).unwrap(),
            OutputProtocol::Hls,
            Endpoint::local_directory("/var/lib/freemix/live").unwrap(),
            None,
            None,
            None,
            QueueCapacity::new(1).unwrap(),
            ReconnectPolicy::disabled(),
        )
        .unwrap();
        assert_eq!(
            pending_network_probe(&hls, 8, preflight::PRIMARY_ENDPOINT),
            None
        );
        assert_eq!(
            pending_network_probe(&hls, 8, preflight::BACKUP_ENDPOINT),
            None
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn split_and_group_targets_gives_hls_its_own_rendition_group() {
        use fm_model::{
            DEFAULT_VIDEO_BITRATE_KBPS, StreamEndpoint as ModelStreamEndpoint,
            StreamKey as ModelStreamKey, StreamProtocol, StreamTarget, StreamTargetId,
        };
        use fm_types::OutputId as ModelOutputId;

        let output = ModelOutputId::new(NonZeroU128::new(30).unwrap());
        let target = |id: u128, protocol: StreamProtocol, endpoint: &str| {
            let endpoint = ModelStreamEndpoint::parse_for(protocol, endpoint).unwrap();
            let key = match protocol {
                StreamProtocol::Hls => None,
                StreamProtocol::Rtmp | StreamProtocol::Rtmps | StreamProtocol::Srt => {
                    Some(ModelStreamKey::parse("live-key-0001").unwrap())
                }
            };
            StreamTarget::new(
                StreamTargetId::new(NonZeroU128::new(id).unwrap()),
                format!("Destination {id}"),
                protocol,
                endpoint,
                key,
                output,
            )
            .unwrap()
        };
        let targets = [
            target(1, StreamProtocol::Rtmp, "ingest.example.test:1935/live"),
            target(2, StreamProtocol::Rtmp, "backup.example.test:1935/live"),
            target(3, StreamProtocol::Hls, "/var/lib/freemix/live"),
            target(4, StreamProtocol::Hls, "/var/lib/freemix/live-alt"),
        ];
        let grouped = split_and_group_targets(&targets).unwrap();
        // One group per distinct output/protocol/bitrate identity in
        // first-appearance order: both RTMP targets share the first, both HLS
        // targets the second, and every HLS target splits with no stream key.
        assert_eq!(
            grouped.group_keys,
            vec![
                (output, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
                (output, OutputProtocol::Hls, DEFAULT_VIDEO_BITRATE_KBPS),
            ]
        );
        assert_eq!(
            grouped
                .splits
                .iter()
                .map(|split| split.group_index)
                .collect::<Vec<_>>(),
            vec![0, 0, 1, 1]
        );
        for split in &grouped.splits {
            assert_eq!(
                split.stream_key.is_some(),
                split.protocol != OutputProtocol::Hls,
                "HLS targets carry no stream key"
            );
            assert_eq!(split.backup_endpoint, None);
        }
        assert_eq!(grouped.splits[2].endpoint.path(), "/var/lib/freemix/live");
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn split_and_group_targets_partitions_rendition_groups_by_output_id() {
        use fm_model::{
            DEFAULT_VIDEO_BITRATE_KBPS, StreamEndpoint as ModelStreamEndpoint,
            StreamKey as ModelStreamKey, StreamProtocol, StreamTarget, StreamTargetId,
        };
        use fm_types::OutputId as ModelOutputId;

        let output_a = ModelOutputId::new(NonZeroU128::new(30).unwrap());
        let output_b = ModelOutputId::new(NonZeroU128::new(31).unwrap());
        let target = |id: u128, output: ModelOutputId, endpoint: &str| {
            StreamTarget::new(
                StreamTargetId::new(NonZeroU128::new(id).unwrap()),
                format!("Destination {id}"),
                StreamProtocol::Rtmp,
                ModelStreamEndpoint::parse_for(StreamProtocol::Rtmp, endpoint).unwrap(),
                Some(ModelStreamKey::parse("live-key-0001").unwrap()),
                output,
            )
            .unwrap()
        };
        let targets = [
            target(1, output_a, "ingest-a1.example.test:1935/live"),
            target(2, output_a, "ingest-a2.example.test:1935/live"),
            target(3, output_b, "ingest-b1.example.test:1935/live"),
        ];
        let grouped = split_and_group_targets(&targets).unwrap();

        assert_eq!(
            grouped.group_keys,
            vec![
                (output_a, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
                (output_b, OutputProtocol::Rtmp, DEFAULT_VIDEO_BITRATE_KBPS),
            ]
        );
        assert_eq!(
            grouped
                .splits
                .iter()
                .map(|split| split.group_index)
                .collect::<Vec<_>>(),
            vec![0, 0, 1]
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn local_output_directories_extracts_the_hls_directory_of_each_authored_target() {
        use fm_model::{
            StreamEndpoint as ModelStreamEndpoint, StreamKey as ModelStreamKey, StreamProtocol,
            StreamTarget, StreamTargetId,
        };
        use fm_types::OutputId as ModelOutputId;

        let output = ModelOutputId::new(NonZeroU128::new(31).unwrap());
        let target = |id: u128, protocol: StreamProtocol, endpoint: &str| {
            let endpoint = ModelStreamEndpoint::parse_for(protocol, endpoint).unwrap();
            let key = match protocol {
                StreamProtocol::Hls => None,
                StreamProtocol::Rtmp | StreamProtocol::Rtmps | StreamProtocol::Srt => {
                    Some(ModelStreamKey::parse("live-key-0001").unwrap())
                }
            };
            StreamTarget::new(
                StreamTargetId::new(NonZeroU128::new(id).unwrap()),
                format!("Destination {id}"),
                protocol,
                endpoint,
                key,
                output,
            )
            .unwrap()
        };
        let targets = [
            target(1, StreamProtocol::Rtmp, "ingest.example.test:1935/live"),
            target(2, StreamProtocol::Hls, "/var/lib/freemix/live"),
            target(3, StreamProtocol::Rtmp, "backup.example.test:1935/live"),
            target(4, StreamProtocol::Hls, "/var/lib/freemix/live-alt"),
        ];
        let grouped = split_and_group_targets(&targets).unwrap();

        // Only local HLS targets contribute a directory, in authored order,
        // and the compiled endpoint already carries it as one absolute path
        // that the sink creates later.
        assert_eq!(
            local_output_directories(&grouped.splits),
            vec![
                PathBuf::from("/var/lib/freemix/live"),
                PathBuf::from("/var/lib/freemix/live-alt"),
            ]
        );
    }

    /// A latched transport failure longer than the wire's detail bound is
    /// shortened on a character boundary instead of making the status record
    /// unencodable (OR-006).
    #[cfg(feature = "native-media")]
    #[test]
    fn stream_failures_are_clamped_to_the_wire_detail_bound() {
        let long = "transport:Connect:ffmpeg exited: ChildExited { status: Some(145) }";
        assert!(long.len() > fm_protocol::MAX_STREAM_DETAIL_BYTES);
        let clamped = clamp_stream_failure(long.to_owned());
        assert_eq!(clamped.len(), fm_protocol::MAX_STREAM_DETAIL_BYTES);
        assert!(long.starts_with(&clamped));
        let wide = "é".repeat(40);
        let clamped = clamp_stream_failure(wide.clone());
        assert!(clamped.len() <= fm_protocol::MAX_STREAM_DETAIL_BYTES);
        assert!(wide.starts_with(&clamped));
        assert_eq!(
            clamp_stream_failure("connect:refused".into()),
            "connect:refused"
        );
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn stream_status_message_sequence_starts_at_one_and_skips_empty_passes() {
        let server = ServerIdentity {
            engine_id: "engine".to_owned(),
            project_id: "project".to_owned(),
            state_epoch: 1,
            log_id: "log".to_owned(),
        };
        assert!(
            stream_status_message(&server, 0, Vec::new())
                .unwrap()
                .is_none(),
            "an empty active set publishes no record"
        );
        let first = stream_status_message(&server, 0, vec![stream_status_placeholder_sample()])
            .unwrap()
            .expect("non-empty set publishes");
        assert_eq!(first.sequence, 1);
        assert_eq!(first.server, server);
        let second = stream_status_message(
            &server,
            first.sequence,
            vec![stream_status_placeholder_sample()],
        )
        .unwrap()
        .expect("non-empty set publishes");
        assert_eq!(second.sequence, 2);
        assert!(encode_line(&WireMessage::StreamStatus(second)).is_ok());
    }

    #[cfg(feature = "native-media")]
    fn stream_status_placeholder_sample() -> StreamStatusSample {
        StreamStatusSample {
            target: WireStreamTargetId::new(NonZeroU128::new(3).unwrap()),
            realized: StreamRealizedState::Starting,
            connected: false,
            muxed_bytes: 0,
            enqueued_pairs: 0,
            dropped_pairs: 0,
            reconnects: 0,
            uptime_ms: 0,
            bitrate_kbps: 0,
            failure: None,
            local: None,
        }
    }

    #[cfg(feature = "native-media")]
    #[test]
    fn stream_feed_builds_valid_paired_frames_from_cloned_audio() {
        // Mirrors NativeProgramStream::compile: format from project settings,
        // pairs built with each frame's audio sequence and a cloned block.
        let format = RecordFormat::new(
            16,
            16,
            FrameRate::new(25, 1).unwrap(),
            SampleRate::new(44_100).unwrap(),
            ChannelLayout::stereo(),
            SequenceNumber::new(0),
        )
        .unwrap();
        let build_pair = |sequence: SequenceNumber| {
            // The engine's absolute-boundary audio contract, restated here
            // because the codec keeps its timing oracle private:
            // sample bounds are floor(n * rate * den / num).
            let hertz = u128::from(format.sample_rate().hertz());
            let boundary = |frames: u64| -> u128 {
                u128::from(frames) * hertz * u128::from(format.frame_rate().denominator())
                    / u128::from(format.frame_rate().numerator())
            };
            let start_sample = boundary(sequence.get());
            let end_sample = boundary(sequence.get().checked_add(1).expect("sequence fits"));
            let start_nanos = start_sample * 1_000_000_000 / hertz;
            let duration_nanos = (end_sample * 1_000_000_000 / hertz) - start_nanos;
            let timing = MediaTiming::new(
                OriginalTimestamp::new(
                    MediaTimestamp::new(i64::try_from(start_sample).unwrap()),
                    fm_frame::TimeBase::new(1, format.sample_rate().hertz()).unwrap(),
                ),
                NormalizedTimestamp::from_nanos(i64::try_from(start_nanos).unwrap()),
                NormalizedDuration::from_nanos(u64::try_from(duration_nanos).unwrap()).unwrap(),
                native_clock_domain(),
                sequence,
            )
            .unwrap();
            let samples = usize::try_from(end_sample - start_sample).unwrap();
            AudioBlock::new(
                timing,
                format.sample_rate(),
                format.channel_layout().clone(),
                vec![vec![0.25; samples], vec![-0.25; samples]],
            )
            .unwrap()
        };
        for sequence in [SequenceNumber::new(0), SequenceNumber::new(41)] {
            let audio = build_pair(sequence);
            let cloned = audio.clone();
            let pair = PairedFrame::new(
                &format,
                sequence,
                vec![0; format.rgba_bytes_per_frame()],
                cloned,
            )
            .expect("cloned project audio must pair");
            assert_eq!(pair.sequence(), sequence);
            assert_eq!(pair.format(), &format);
        }
    }

    /// Regression: the compiled grades are shared across revisions that do
    /// not change them, so a cut or a fader move never re-bakes and
    /// re-uploads every native grade lattice on the frame loop.
    #[test]
    fn grade_cache_keeps_grades_across_commands_that_do_not_change_them() {
        let project = test_project();
        let mut control = test_control(&project);
        let principal = operator();
        let graded = control
            .submit(
                &principal,
                test_command(
                    "grade",
                    "grade-key",
                    CommandPayload::SetInputGrade {
                        input: fm_protocol::WireInputId::from_domain(test_input_id(1)),
                        source: Some(fm_protocol::WireGradeSource::Inline(Box::new(
                            fm_grade::Grade {
                                saturation_permille: 0,
                                ..fm_grade::Grade::IDENTITY
                            },
                        ))),
                    },
                ),
                0,
            )
            .unwrap();
        assert!(graded.is_accepted());
        let mut cache = grading::GradeCache::default();
        let before = cache.at(&control);
        let cut = control
            .submit(
                &principal,
                test_command("cut", "cut-key", CommandPayload::Cut),
                0,
            )
            .unwrap();
        assert!(cut.is_accepted());
        let after = cache.at(&control);
        assert!(Arc::ptr_eq(&before, &after));
    }

    const ANIMATED_TITLE_URI: &str = "asset://titles/unit.fmtitle";

    fn animated_title_template(
        elements: u128,
        clip: fm_titles::TitleClip,
    ) -> fm_titles::TitleTemplate {
        fm_titles::TitleTemplate {
            id: fm_titles::TemplateId::new(NonZeroU128::new(9).unwrap()),
            name: "Lower".into(),
            width: 32,
            height: 18,
            background: fm_titles::Color::TRANSPARENT,
            fields: Vec::new(),
            elements: (1..=elements)
                .map(|element| fm_titles::Element {
                    id: fm_titles::ElementId::new(NonZeroU128::new(element).unwrap()),
                    name: format!("box {element}"),
                    bounds: fm_titles::Bounds {
                        x: 2,
                        y: 2,
                        width: 8,
                        height: 8,
                    },
                    z_index: 0,
                    visible: true,
                    style: fm_titles::Style::default(),
                    color_field: None,
                    kind: fm_titles::ElementKind::Rectangle,
                })
                .collect(),
            animation: fm_titles::TitleAnimation::new(clip, 0),
        }
    }

    /// [`test_project`] with title input 3 naming [`ANIMATED_TITLE_URI`].
    fn animated_title_project() -> StoredProject {
        let baseline = test_project();
        let mut project = baseline.project().clone();
        project.add_input(Input {
            id: test_input_id(3),
            name: "Lower third".into(),
            kind: InputKind::Title(fm_model::TitleInput {
                template_uri: ANIMATED_TITLE_URI.into(),
                field_revision: 0,
                fields: Vec::new(),
            }),
            required_capabilities: Vec::new(),
        });
        StoredProject::from_project(
            project,
            baseline.runtime_routing(),
            ProjectPosition::default(),
            Vec::new(),
        )
        .unwrap()
    }

    /// Refuses every journal append and every discard of a staged copy.
    struct UndiscardableStager {
        discarded: Cell<bool>,
    }

    impl DurableStore for UndiscardableStager {
        fn record(
            &self,
            _command: &CommandMessage,
            _now_millis: u64,
            _previous: &StoredProject,
            _updated: &StoredProject,
        ) -> AppResult<()> {
            Err(AppFailure("injected journal failure".into()).into())
        }

        fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
            Ok(())
        }

        fn discard_staged_title_animations(
            &self,
            _revision: u64,
            uris: &[String],
        ) -> AppResult<()> {
            assert_eq!(uris, [ANIMATED_TITLE_URI.to_owned()]);
            self.discarded.set(true);
            Err(AppFailure("injected discard failure".into()).into())
        }
    }

    /// Regression: an animation edit refused after its template was staged
    /// left the staged copy behind when discarding it failed. The next
    /// command reuses the revision, and the checkpoint covering it committed
    /// the refused animation over the template. A copy that cannot be
    /// discarded now stops the daemon, whose next start discards it.
    #[test]
    fn a_staged_title_animation_that_cannot_be_discarded_stops_the_daemon() {
        let rate = FrameRate::new(25, 1).unwrap();
        let empty =
            fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 0, 0)
                .unwrap();
        let mut durable = animated_title_project();
        let assets = BundleAssets {
            titles: [(test_input_id(3), animated_title_template(0, empty))].into(),
            luts: std::collections::BTreeMap::new(),
        };
        let mut control = ControlService::new(
            restore_engine_with_titles(&durable, &assets).unwrap(),
            Policy::development(),
            "unit-engine",
            "unit-log",
            ControlLimits::default(),
        );
        let server = test_server(&control);
        let longer =
            fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 10, 0)
                .unwrap();
        let command = test_command(
            "animate",
            "animate-key",
            CommandPayload::SetTitleAnimation {
                input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
                base_revision: 0,
                clip: Box::new(fm_control::title_clip_to_wire(&longer)),
            },
        );
        let store = UndiscardableStager {
            discarded: Cell::new(false),
        };
        let outcome = execute_durable_command(
            &mut control,
            &store,
            &mut durable,
            &operator(),
            &server,
            &command,
            0,
        );
        assert!(store.discarded.get());
        assert!(
            outcome.is_err(),
            "an undiscardable staged copy must stop the daemon"
        );
        assert_eq!(control.diagnostics().current_revision, 0);
    }

    /// Regression: an animation edit stays staged beside its template until
    /// a checkpoint covers it, and checkpoints committed only the templates
    /// a title still named. When the only title naming the template was
    /// removed after the edit, the copy was never committed or discarded, and
    /// a title added offline later picked the stale edit up on the next
    /// start. Every staged copy now commits by the same durable-revision
    /// rule, at the checkpoint and at startup recovery alike.
    #[test]
    fn a_staged_animation_of_a_template_no_title_names_any_more_still_commits() {
        use journal::DurableJournal;

        let rate = FrameRate::new(25, 1).unwrap();
        let empty =
            fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 0, 0)
                .unwrap();
        let longer =
            fm_titles::fm_animation::Clip::new(fm_titles::fm_animation::Timebase::new(rate), 10, 0)
                .unwrap();
        for checkpoint in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
            let template_path = store.assets_root().join("titles/unit.fmtitle");
            fs::create_dir_all(template_path.parent().unwrap()).unwrap();
            fs::write(
                &template_path,
                fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
                    template: animated_title_template(0, empty.clone()),
                    fonts: Vec::new(),
                    clocks: Vec::new(),
                })
                .unwrap(),
            )
            .unwrap();
            let manifest = animated_title_project();
            store.save(&manifest).unwrap();
            let assets = BundleAssets {
                titles: [(test_input_id(3), animated_title_template(0, empty.clone()))].into(),
                luts: std::collections::BTreeMap::new(),
            };
            let mut control = ControlService::new(
                restore_engine_with_titles(&manifest, &assets).unwrap(),
                Policy::development(),
                "unit-engine",
                "unit-log",
                ControlLimits::default(),
            );
            let server = test_server(&control);
            let mut durable = manifest.clone();
            {
                let journal = DurableJournal::new(store.open_journal_writer().unwrap());
                for command in [
                    test_command(
                        "animate",
                        "animate-key",
                        CommandPayload::SetTitleAnimation {
                            input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
                            base_revision: 0,
                            clip: Box::new(fm_control::title_clip_to_wire(&longer)),
                        },
                    ),
                    test_command(
                        "remove",
                        "remove-key",
                        CommandPayload::RemoveInput {
                            input: fm_protocol::WireInputId::from_domain(test_input_id(3)),
                        },
                    ),
                ] {
                    let outcome = execute_durable_command(
                        &mut control,
                        &journal,
                        &mut durable,
                        &operator(),
                        &server,
                        &command,
                        0,
                    )
                    .unwrap();
                    let result = &outcome.submission.output.result;
                    assert!(
                        matches!(result, CommandResult::Accepted { .. }),
                        "{result:?}"
                    );
                }
                if checkpoint {
                    journal.settle(&durable).unwrap();
                }
            }
            if !checkpoint {
                load_and_recover(&store, false).unwrap();
            }
            let titles: Vec<String> = fs::read_dir(template_path.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(titles, ["unit.fmtitle"], "checkpoint={checkpoint}");
            assert_eq!(
                store
                    .load_title_template(ANIMATED_TITLE_URI)
                    .unwrap()
                    .template
                    .animation
                    .clip,
                longer,
                "checkpoint={checkpoint}"
            );
        }
    }

    /// Regression: a client could import a LUT under the id of a removed
    /// one, replacing its bundled file, so an older manifest, journal, or
    /// autosave naming that id loaded different content. Bundled LUT files
    /// are immutable: the import is refused as a non-retryable conflict and
    /// the file is untouched; the same text under the same id is accepted.
    #[test]
    fn a_lut_import_never_replaces_a_bundled_file_with_different_content() {
        use journal::DurableJournal;

        const INVERT: &str =
            "LUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";
        const IDENTITY: &str =
            "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let manifest = test_project();
        store.save(&manifest).unwrap();
        // LUT 1 was imported and removed before: its file stays.
        let one = NonZeroU32::new(1).unwrap();
        store
            .write_lut_source(fm_model::LutId::new(one), INVERT)
            .unwrap();
        let mut control = test_control(&manifest);
        let server = test_server(&control);
        let mut durable = manifest.clone();
        let journal = DurableJournal::new(store.open_journal_writer().unwrap());
        let import = |key: &str, data: &str| {
            test_command(
                key,
                &format!("{key}-key"),
                CommandPayload::ImportGradeLut {
                    lut: one,
                    name: "Look".into(),
                    data: data.into(),
                },
            )
        };

        let refused = execute_durable_command(
            &mut control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &import("reuse", IDENTITY),
            0,
        )
        .unwrap();
        let CommandResult::Rejected {
            code, retryable, ..
        } = &refused.submission.output.result
        else {
            panic!("{:?}", refused.submission.output.result);
        };
        assert_eq!(code, RejectionCode::Conflict.as_str());
        assert!(!retryable);
        assert_eq!(control.diagnostics().current_revision, 0);
        assert_eq!(
            fs::read_to_string(store.assets_root().join("luts/1.cube")).unwrap(),
            INVERT
        );

        let same = execute_durable_command(
            &mut control,
            &journal,
            &mut durable,
            &operator(),
            &server,
            &import("same", INVERT),
            0,
        )
        .unwrap();
        assert!(
            matches!(
                same.submission.output.result,
                CommandResult::Accepted { .. }
            ),
            "{:?}",
            same.submission.output.result
        );
    }

    /// Regression: a stored template whose animation has no wire encoding
    /// (beyond `MAX_TITLE_ANIMATION_BYTES`) made every snapshot's
    /// `title_animation_state` unencodable, so no session could connect.
    /// Like names beyond the `title_state` bounds, it refuses to start.
    #[test]
    fn templates_whose_animation_has_no_wire_encoding_refuse_to_start() {
        use fm_titles::fm_animation::{
            Clip, CubicBezier, Easing, Keyframe, MAX_PHASE_FRAMES, Phase, Property, Timebase,
            Track, TrackKey, Value,
        };

        let bezier =
            Easing::CubicBezier(CubicBezier::new(65_536, -65_536, 65_536, 131_072).unwrap());
        let tracks = 16_u128;
        let clip = Clip::from_tracks(
            Timebase::new(FrameRate::new(25, 1).unwrap()),
            MAX_PHASE_FRAMES,
            0,
            (1..=tracks)
                .map(|element| {
                    Track::new(
                        TrackKey::new(
                            fm_titles::ElementId::new(NonZeroU128::new(element).unwrap()),
                            Phase::In,
                            Property::PositionX,
                        ),
                        (0..256_u32)
                            .map(|frame| {
                                Keyframe::new(
                                    frame * 100,
                                    Value::Scalar(i64::from(i32::MIN)),
                                    bezier,
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
        )
        .unwrap();
        assert!(
            fm_protocol::title_clip_wire_bytes(&fm_control::title_clip_to_wire(&clip))
                > fm_protocol::MAX_TITLE_ANIMATION_BYTES
        );
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("show.freemix")).unwrap();
        let document =
            fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
                template: animated_title_template(tracks, clip),
                fonts: Vec::new(),
                clocks: Vec::new(),
            })
            .unwrap();
        let path = store.assets_root().join("titles/unit.fmtitle");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, document).unwrap();

        let error = load_title_templates(&store, animated_title_project().project())
            .expect_err("an animation without a wire encoding refuses to start");
        assert!(error.to_string().contains("animation"), "{error}");
    }
}
