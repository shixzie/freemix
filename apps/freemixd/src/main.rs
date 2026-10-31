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
mod restore;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use restore::*;
mod command_window;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use command_window::*;
mod serve;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use serve::*;
mod client;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use client::*;
mod project_sync;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use project_sync::*;
mod lifecycle;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use lifecycle::*;
mod native_startup;
#[allow(unused_imports, clippy::wildcard_imports)]
// the test module names these through its parent glob
use native_startup::*;
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
use journal::{CommandRefusal, DurableJournal, DurableStore, ReplayedMutations};
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

#[cfg(feature = "native-media")]
/// Validates a segment path and returns its canonical parent directory.
///
/// The segment must name a final `.mp4` file whose parent is an existing
/// directory. The session creates the file itself, exclusively: an existing
/// file, directory, or symlink at the path is never overwritten.
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
