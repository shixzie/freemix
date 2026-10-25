#![cfg(all(feature = "native-media", unix))]

//! Process-level instant replay (OR-012): a native daemon whose show
//! records a source for replay writes rolling segment files into the
//! bundle's `replay` directory and reports them in `replay_status` (pushed
//! to the Studio session and answered on request); a captured event is
//! complete once closed segments cover it, and a Replay input cued on it
//! plays it at half speed.
//!
//! The test needs `FFmpeg` and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    AudioBus, Input, InputKind, Layer, LayerGeometry, MainMix, Output, Project, ProjectSettings,
    Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    StartupPolicy,
};
use fm_persistence::{
    ProjectPosition, ProjectStore, RuntimeFadeToBlack, RuntimeManualTransitions, RuntimeOverlays,
    RuntimeRouting, StoredProject,
};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, HeartbeatMessage, ReplayFeed, ReplayOperation, ReplaySettings, ReplaySpeed,
    ReplayStatusMessage, ReplayStatusRequest, Role, ServerIdentity, WireInputId, WireMessage,
    decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
const CAMERA: u128 = 1;
const PLAYER: u128 = 3;
/// Commands sent by every session of the test.
static COMMANDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn wire(value: u128) -> WireInputId {
    WireInputId::new(id(value))
}

fn tools_available() -> bool {
    let available = ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg/ffprobe are unavailable"
    );
    available
}

/// A 128x72 show with red input 1 on Program, blue input 2 on Preview,
/// and Replay input 3; input 1 is recorded for replay with a ten-second
/// window in the daemon's default root.
fn save_project(path: &Path) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(9_400)),
        "Native replay",
        ProjectSettings {
            frame_rate: rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(128, 72).unwrap(),
                frame_rate: rate,
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
    let bus = BusId::new(id(9_450));
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    for (index, color) in [(1, [210, 20, 20]), (2, [20, 30, 210])] {
        project.add_input(Input {
            id: input(index),
            name: format!("Camera {index}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(color[0], color[1], color[2], 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    project.add_input(Input {
        id: input(PLAYER),
        name: "Replay".into(),
        kind: InputKind::Replay,
        required_capabilities: Vec::new(),
    });
    let scene = SceneId::new(id(9_410));
    project.add_scene(Scene {
        id: scene,
        name: "Program".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            id: fm_model::LayerId::new(1).unwrap(),
            name: "Source".into(),
            source: SourceRef::Input(input(CAMERA)),
            enabled: true,
            geometry: LayerGeometry::new(0, 0, 128, 72, Rotation::Deg0),
            crop: None,
            mask: None,
            key: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_output(Output {
        id: OutputId::new(id(9_401)),
        name: "Program".into(),
        video_source: scene,
        audio_source: bus,
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(input(1), input(2)));
    project
        .set_replay(ReplaySettings {
            sources: vec![input(CAMERA)],
            retention_seconds: 10,
            storage_root: None,
        })
        .unwrap();
    let stored = StoredProject::from_project_with_complete_runtime_state(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(1)),
            realized_program_id: Some(input(1)),
            desired_preview_id: Some(input(2)),
            realized_preview_id: Some(input(2)),
        },
        RuntimeManualTransitions::default(),
        RuntimeFadeToBlack::default(),
        RuntimeOverlays::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    ProjectStore::new(path).unwrap().save(&stored).unwrap();
}

struct NativeDaemon {
    child: Child,
    address: SocketAddr,
    log: mpsc::Receiver<String>,
}

impl Drop for NativeDaemon {
    fn drop(&mut self) {
        if thread::panicking() {
            thread::sleep(Duration::from_secs(5));
            eprintln!("daemon exit: {:?}", self.child.try_wait());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if thread::panicking() {
            while let Ok(line) = self.log.recv_timeout(Duration::from_secs(2)) {
                eprintln!("daemon: {line}");
            }
        }
    }
}

fn start_native(project: &Path) -> Result<NativeDaemon, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project)
        .arg("--native-media")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot spawn native daemon: {error}"))?;
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let line = receiver.recv_timeout(STARTUP_TIMEOUT).unwrap_or_default();
    if let Ok(readiness) = line.parse::<ReadinessRecord>() {
        let (log_sender, log) = mpsc::channel();
        if let Some(stderr) = child.stderr.take() {
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ = log_sender.send(line);
                }
            });
        }
        return Ok(NativeDaemon {
            child,
            address: readiness.address,
            log,
        });
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(format!("native daemon did not become ready: {stderr}"))
}

struct Client {
    reader: BufReader<TcpStream>,
    pending: Vec<u8>,
    writer: TcpStream,
    server: Option<ServerIdentity>,
    heartbeats: u64,
    last_heartbeat: Instant,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            pending: Vec::new(),
            server: None,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-replay-test".into(),
            client_type: ClientType::Studio,
            desired_role: Role::Admin,
            resume_cursor: None,
        }));
        let WireMessage::HandshakeResponse(response) = client.wait_message() else {
            panic!("expected the handshake response");
        };
        client.server = Some(response.server);
        let WireMessage::Snapshot(_) = client.wait_message() else {
            panic!("expected the snapshot");
        };
        client
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    fn heartbeat(&mut self) {
        if let Some(server) = self.server.clone()
            && self.last_heartbeat.elapsed() >= Duration::from_secs(1)
        {
            self.heartbeats += 1;
            self.send(&WireMessage::Heartbeat(HeartbeatMessage {
                server,
                sequence: self.heartbeats,
                sent_at_ms: self.heartbeats,
                last_applied: None,
            }));
            self.last_heartbeat = Instant::now();
        }
    }

    fn poll(&mut self) -> Option<WireMessage> {
        self.heartbeat();
        match self.reader.read_until(b'\n', &mut self.pending) {
            Ok(0) => panic!("daemon closed"),
            Ok(_) if !self.pending.ends_with(b"\n") => None,
            Ok(_) => {
                let line = String::from_utf8(std::mem::take(&mut self.pending)).unwrap();
                Some(decode_line(&line).unwrap())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                None
            }
            Err(error) => panic!("read failed: {error}"),
        }
    }

    fn wait_message(&mut self) -> WireMessage {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for a message");
            if let Some(message) = self.poll() {
                return message;
            }
        }
    }

    fn wait_for<T>(
        &mut self,
        what: &str,
        mut found: impl FnMut(Option<WireMessage>) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let message = self.poll();
            if let Some(value) = found(message) {
                return value;
            }
        }
    }

    /// Waits for a pushed `replay_status` that satisfies `wanted`.
    fn status(
        &mut self,
        what: &str,
        wanted: impl Fn(&ReplayStatusMessage) -> bool,
    ) -> ReplayStatusMessage {
        self.wait_for(what, |message| match message {
            Some(WireMessage::ReplayStatus(status)) if wanted(&status) => Some(*status),
            _ => None,
        })
    }

    fn replay(&mut self, operation: ReplayOperation) {
        let id = format!(
            "replay-{}",
            COMMANDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
        );
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload: CommandPayload::Replay(operation),
        }));
        let result = self.wait_for("the command result", |message| match message {
            Some(WireMessage::CommandResult(result)) => Some(result),
            _ => None,
        });
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
    }
}

/// The segment files under `root`, in every session directory.
fn segment_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(sessions) = std::fs::read_dir(root) else {
        return files;
    };
    for session in sessions.flatten() {
        let Ok(runs) = std::fs::read_dir(session.path()) else {
            continue;
        };
        for run in runs.flatten() {
            if let Ok(segments) = std::fs::read_dir(run.path()) {
                files.extend(segments.flatten().map(|segment| segment.path()));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn native_daemon_records_replay_segments_and_plays_an_event_at_half_speed() {
    if !tools_available() {
        eprintln!("native replay integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("replay.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native replay integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    let root = project_path.join("replay");

    // The camera is recorded into rolling two-second segment files.
    let status = client.status("two recorded segments", |status| {
        status
            .recorded
            .first()
            .is_some_and(|source| source.segments >= 2)
    });
    assert_eq!(status.feed, ReplayFeed::Recorded);
    assert_eq!(Path::new(&status.root), root);
    assert_eq!(status.recorded[0].input, wire(CAMERA));
    let files = segment_files(&root);
    assert!(
        files
            .iter()
            .any(|file| file.ends_with(format!("{CAMERA}-1/segment-000001.mp4"))),
        "{files:?}"
    );

    // The last five seconds become an event, complete once closed
    // segments cover it.
    client.replay(ReplayOperation::CaptureLast { seconds: 5 });
    let status = client.status("a complete event", |status| {
        status.events.first().is_some_and(|event| event.complete)
    });
    let event = status.events[0].clone();
    assert_eq!(event.out_frame - event.in_frame, 125);
    assert_eq!(event.angles, vec![wire(CAMERA)]);

    // The Replay input plays it at half speed: its frame advances half as
    // fast as the live edge.
    client.replay(ReplayOperation::Cue {
        input: wire(PLAYER),
        event: event.id,
        angle: None,
    });
    client.replay(ReplayOperation::Play {
        input: wire(PLAYER),
        speed: ReplaySpeed::Half,
    });
    let playing = |status: &ReplayStatusMessage| {
        status
            .players
            .first()
            .is_some_and(|player| player.playing && player.speed == ReplaySpeed::Half)
    };
    let first = client.status("the player playing", playing);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        client.poll();
    }
    let second = client.status("the player still playing", playing);
    let live = second.live_frame.unwrap() - first.live_frame.unwrap();
    let played = second.players[0].frame.unwrap() - first.players[0].frame.unwrap();
    assert!(live >= 25, "{live}");
    assert!(
        played * 10 >= live * 3 && played * 10 <= live * 7,
        "played {played} of {live}"
    );

    // Any session may ask for the status.
    client.send(&WireMessage::ReplayStatusRequest(ReplayStatusRequest {
        request_id: "replay-test".to_owned(),
    }));
    let answered = client.status("the answered status", |status| {
        status.request_id.as_deref() == Some("replay-test")
    });
    assert_eq!(answered.events.len(), 1);
    let failures: Vec<String> = std::iter::from_fn(|| daemon.log.try_recv().ok())
        .filter(|line| line.contains("FREEMIXD_REPLAY_FAILURE"))
        .collect();
    assert!(failures.is_empty(), "{failures:?}");
}
