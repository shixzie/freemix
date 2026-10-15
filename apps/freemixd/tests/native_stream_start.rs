//! A runtime `StreamStart` must never stall the native daemon loop.
//!
//! Starting a stream destination spawns `FFmpeg` and waits up to the
//! transport's connect timeout for the child's first input request; every
//! reconnect does the same, and a stop drains the session. That I/O used to
//! run on the daemon loop, freezing frame realization, monitor frames and
//! every session. This test holds the gated `FFmpeg`'s start open behind a
//! gate the test controls and proves, through a command round trip sent only
//! after that very child was spawned, that the loop answered while the child
//! was still alive and unconnected. The blocking loop could only answer after
//! its connect timeout had killed the child, so the ordering is decided by
//! process liveness, not by a wall-clock threshold.

#![cfg(all(feature = "native-media", unix))]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    AudioBus, Input, InputKind, Layer, LayerGeometry, MainMix, Output, Project, ProjectSettings,
    Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    StartupPolicy, StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, MonitorFrameAssembler, Role, WireMessage, WireStreamTargetId, decode_line,
    encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
/// Fresh monitor frames the daemon must publish while the gated child is
/// still waiting to connect.
const FRAMES_DURING_START: usize = 3;
const STREAM_TARGET: u128 = 8_101;
/// Never echoed by the daemon: every diagnostic redacts it.
const STREAM_KEY: &str = "stream-start-key-0001";

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

/// A 64x48 generator show with one RTMP destination that is configured but
/// not desired, so nothing connects until a runtime `StreamStart`.
fn save_streaming_project(path: &Path) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(8_100)),
        "Native stream start test",
        ProjectSettings {
            frame_rate: rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(64, 48).unwrap(),
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
    for (input_id, video) in [
        (input(1), SimulatedVideo::Bars),
        (
            input(2),
            SimulatedVideo::Solid(SolidColor::new(24, 80, 160, 255)),
        ),
    ] {
        project.add_input(Input {
            id: input_id,
            name: format!("Generator {input_id}"),
            kind: InputKind::Simulated(SimulatedInput::new(video, SimulatedAudio::Silence)),
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(input(1), input(2)));
    let scene = SceneId::new(id(8_110));
    project.add_scene(Scene {
        id: scene,
        name: "Program scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            id: fm_model::LayerId::new(1).unwrap(),
            name: "Source".into(),
            source: SourceRef::Input(input(1)),
            enabled: true,
            geometry: LayerGeometry::new(0, 0, 64, 48, Rotation::Deg0),
            crop: None,
            mask: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
        animation: fm_model::SceneAnimation::default(),
    });
    let bus = BusId::new(id(8_111));
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    let output = OutputId::new(id(8_112));
    project.add_output(Output {
        id: output,
        name: "Program output".into(),
        video_source: scene,
        audio_source: bus,
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: Vec::new(),
    });
    let target = StreamTarget::new(
        StreamTargetId::new(id(STREAM_TARGET)),
        "Destination 1".to_owned(),
        StreamProtocol::Rtmp,
        // Nothing listens here; the gated child never contacts it anyway.
        StreamEndpoint::parse("127.0.0.1:9/live").unwrap(),
        Some(StreamKey::parse(STREAM_KEY).unwrap()),
        output,
    )
    .unwrap()
    .with_startup(StartupPolicy::ReconcileDesiredState)
    .set_running(false);
    project.add_stream_target_checked(target).unwrap();
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(1)),
            realized_program_id: Some(input(1)),
            desired_preview_id: Some(input(2)),
            realized_preview_id: Some(input(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    ProjectStore::new(path).unwrap().save(&stored).unwrap();
}

/// Installs an `ffmpeg` that publishes its pid to `started`, then never
/// requests its inputs until `gate` exists, and finally exits without output.
fn install_gated_ffmpeg(bin: &Path, started: &Path, gate: &Path) {
    fs::create_dir_all(bin).unwrap();
    let script = bin.join("ffmpeg");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > '{started}.tmp' && mv '{started}.tmp' '{started}'\n\
             while [ ! -e '{gate}' ]; do sleep 0.01; done\nexit 7\n",
            started = started.display(),
            gate = gate.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
}

fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn wait_for_pid(started: &Path) -> u32 {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Ok(text) = fs::read_to_string(started)
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the stream start never spawned ffmpeg"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

struct DaemonProcess {
    child: Child,
    address: SocketAddr,
    stderr: mpsc::Receiver<String>,
}

impl DaemonProcess {
    /// Starts a headless native daemon, or returns `None` when this host has
    /// no native media runtime (unless `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path, bin: &Path) -> Option<Self> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut search = vec![bin.to_path_buf()];
        search.extend(std::env::split_paths(&path));
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .arg("--native-media")
            .env("PATH", std::env::join_paths(search).unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = drain(child.stderr.take().unwrap());
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (ready_sender, ready) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            let _ = stdout.read_line(&mut line);
            let _ = ready_sender.send(line);
            // Keep draining so the daemon never blocks on a full pipe.
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let line = ready.recv_timeout(PROCESS_TIMEOUT).unwrap_or_default();
        match line.trim_end().parse::<ReadinessRecord>() {
            Ok(readiness) => Some(Self {
                child,
                address: readiness.address,
                stderr,
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let detail: String = stderr.try_iter().collect();
                let message = format!("native daemon unavailable: {error}; stderr={detail}");
                assert!(
                    std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                    "FM_REQUIRE_NATIVE_MEDIA=1: {message}"
                );
                eprintln!("native stream start test skipped: {message}");
                None
            }
        }
    }

    /// Requests a cooperative stop and returns everything written to stderr.
    fn stop(mut self) -> String {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let stderr: String = self.stderr.iter().collect();
                assert!(status.success(), "daemon failed: {stderr}");
                return stderr;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let stderr: String = self.stderr.try_iter().collect();
                panic!("daemon did not stop: {stderr}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn drain(mut reader: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0
                || sender
                    .send(String::from_utf8_lossy(&buffer[..read]).into_owned())
                    .is_err()
            {
                break;
            }
        }
    });
    receiver
}

struct StudioClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl StudioClient {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(PROCESS_TIMEOUT)).unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        }
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
    }

    fn receive(&mut self) -> WireMessage {
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed the session"
        );
        decode_line(&line).unwrap()
    }

    fn handshake(&mut self) {
        self.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-stream-start-test".into(),
            client_type: ClientType::Studio,
            desired_role: Role::Operator,
            resume_cursor: None,
        }));
        while !matches!(self.receive(), WireMessage::Snapshot(_)) {}
    }

    fn command(&mut self, command_id: &str, target: u128) {
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: command_id.into(),
            idempotency_key: format!("{command_id}-key"),
            expected_revision: None,
            deadline_ms: None,
            payload: CommandPayload::StreamStart {
                target: WireStreamTargetId::new(id(target)),
            },
        }));
    }

    /// Reads until the result of `command_id`, skipping everything else.
    fn result(&mut self, command_id: &str) -> CommandResult {
        loop {
            if let WireMessage::CommandResult(result) = self.receive() {
                let matches = match &result {
                    CommandResult::Accepted { id, .. } | CommandResult::Rejected { id, .. } => {
                        id == command_id
                    }
                };
                if matches {
                    return result;
                }
            }
        }
    }
}

/// While a runtime `StreamStart`'s `FFmpeg` child is still waiting to
/// connect, the daemon answers commands sent after that child was spawned and
/// keeps publishing fresh monitor frames.
#[test]
fn runtime_stream_start_keeps_the_daemon_loop_serving_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("stream-start.freemix");
    save_streaming_project(&project);
    let bin = directory.path().join("bin");
    let started = directory.path().join("ffmpeg-started");
    let gate = directory.path().join("ffmpeg-gate");
    install_gated_ffmpeg(&bin, &started, &gate);
    let Some(daemon) = DaemonProcess::start(&project, &bin) else {
        return;
    };

    let mut studio = StudioClient::connect(daemon.address);
    studio.handshake();
    let mut assembler = MonitorFrameAssembler::new();
    // Monitor demand is established once the first frame arrives.
    let mut last_frame = loop {
        if let WireMessage::MonitorFrame(part) = studio.receive()
            && let Some(image) = assembler.push(&part).unwrap()
        {
            break image.frame;
        }
    };

    studio.command("stream-start", STREAM_TARGET);
    let result = studio.result("stream-start");
    assert!(
        matches!(&result, CommandResult::Accepted { .. }),
        "{result:?}"
    );

    // The next frame boundary begins the connect. Once its child exists, a
    // command sent now can only be answered by a loop that is not waiting
    // for that child.
    let pid = wait_for_pid(&started);
    studio.command("probe", STREAM_TARGET);
    studio.result("probe");
    let mut frames_during_start = 0;
    while frames_during_start < FRAMES_DURING_START {
        if let WireMessage::MonitorFrame(part) = studio.receive()
            && let Some(image) = assembler.push(&part).unwrap()
        {
            assert!(image.frame > last_frame, "monitor frames must be fresh");
            last_frame = image.frame;
            frames_during_start += 1;
        }
    }
    // The same child is still waiting at the gate: the loop served the probe
    // and every frame above while the connect was in flight, instead of after
    // the connect deadline killed the child.
    assert_eq!(
        fs::read_to_string(&started).unwrap().trim(),
        pid.to_string(),
        "the gated ffmpeg was replaced before the daemon answered"
    );
    assert!(
        process_alive(pid),
        "the daemon answered only after the gated ffmpeg died"
    );

    // Let every attempt fail; shutdown still settles the transport and
    // reports the target once.
    fs::write(&gate, b"").unwrap();
    drop(studio);
    let stderr = daemon.stop();
    let record = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_STREAM\t"))
        .unwrap_or_else(|| panic!("missing stream record: {stderr}"));
    assert!(
        record.contains(&format!("\ttarget={STREAM_TARGET}\t")),
        "{record}"
    );
    assert!(
        !stderr.contains(STREAM_KEY),
        "the stream key leaked: {stderr}"
    );
}
