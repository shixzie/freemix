#![cfg(feature = "native-media")]

//! Process-level IN-015 on the native runtime: a native daemon receives a
//! network stream input (MPEG-TS over UDP from a local `ffmpeg` sender) on
//! Program. Its receive status reaches clients over the wire as
//! `network_stream_status` runtime events: it goes live and Program shows
//! the stream's picture; when the sender dies the input stalls to its black
//! slate and reconnects; when the sender returns it recovers. Changing the
//! input's settings (`input_stream_set`) reconnects it. The URL's secret
//! never appears on the wire.
//!
//! IN-016/IN-017: a native daemon's RTMP listener takes an `ffmpeg` push
//! with its stream key onto Program, and, reconfigured live as an encrypted
//! SRT listener, an `ffmpeg` SRT caller with the passphrase; neither secret
//! reaches the wire or the daemon's log.
//!
//! The test needs `FFmpeg` and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket},
    num::NonZeroU128,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    DiagnosticsRequest, EventPayload, HandshakeRequest, HeartbeatMessage, InputThumbnailMessage,
    InputThumbnailSubscription, MonitorFrameAssembler, MonitorFrameOrigin, MonitorImage,
    MonitorTap, Role, RuntimeLifecycleEvent, ServerIdentity, SourceClockState, SourceSyncKind,
    WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::network_stream::{
    NetworkStreamConfig, NetworkStreamSettings, NetworkStreamState, NetworkStreamStatus,
    SecretEdit, SrtKeyLength, StallSlate, StreamMode, StreamUrl,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
const SECRET: &str = "s3cr3t";

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn camera() -> WireInputId {
    WireInputId::from_domain(input(2))
}

fn ffmpeg_available() -> bool {
    let available = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg is unavailable"
    );
    available
}

fn stream_url(port: u16) -> String {
    format!("udp://127.0.0.1:{port}?token={SECRET}")
}

/// Input 1 is a silent solid generator on Preview, input 2 the network
/// stream on Program.
fn save_project(path: &Path, port: u16) {
    let mut config = NetworkStreamConfig::new(stream_url(port));
    config.settings.jitter_buffer_ms = 200;
    config.settings.timeout_ms = 1_000;
    config.settings.reconnect.backoff_min_ms = 200;
    config.settings.reconnect.backoff_max_ms = 1_000;
    config.settings.stall = StallSlate::Black;
    save_project_with(path, config);
}

/// [`save_project`] with input 2 receiving `config`.
fn save_project_with(path: &Path, config: NetworkStreamConfig) {
    save_project_routed(path, config, 2, 1);
}

/// [`save_project_with`] with inputs `program` and `preview` on the buses.
fn save_project_routed(path: &Path, config: NetworkStreamConfig, program: u128, preview: u128) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(44).unwrap()),
        "Native network stream",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(320, 180).unwrap(),
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
        id: input(1),
        name: "Solid".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(12, 34, 56, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input(2),
        name: "Camera".into(),
        kind: InputKind::NetworkStream(config),
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(input(program), input(preview)));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(program)),
            realized_program_id: Some(input(program)),
            desired_preview_id: Some(input(preview)),
            realized_preview_id: Some(input(preview)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
}

/// A red test signal with a tone, sent as MPEG-TS over UDP; killed on drop.
struct Sender(Child);

impl Sender {
    fn start(port: u16) -> Self {
        Self(
            Command::new("ffmpeg")
                .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-re"])
                .args(["-f", "lavfi", "-i", "color=c=red:s=320x180:r=25"])
                .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
                .args(["-c:v", "mpeg2video", "-g", "10", "-b:v", "400k"])
                .args(["-c:a", "mp2", "-f", "mpegts"])
                .arg(format!("udp://127.0.0.1:{port}?pkt_size=1316"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct NativeDaemon {
    child: Child,
    address: SocketAddr,
    log: mpsc::Receiver<String>,
}

impl Drop for NativeDaemon {
    /// On a failure, reports whether the daemon exited by itself and
    /// replays its stderr (including any `FREEMIXD_SESSION` drop reason).
    fn drop(&mut self) {
        if thread::panicking() {
            thread::sleep(Duration::from_millis(500));
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
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
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

/// How often the client heartbeats: well inside the daemon's 15 s session
/// heartbeat timeout, which a loaded host can otherwise outlast mid-test.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    assembler: MonitorFrameAssembler,
    commands: u64,
    /// The daemon's identity once its handshake response arrived; only then
    /// can the client heartbeat.
    server: Option<ServerIdentity>,
    heartbeats: u64,
    last_heartbeat: Instant,
    /// The camera's statuses in arrival order, not yet looked at by
    /// [`Client::status`]: a status can arrive while another wait (a
    /// command result, an event) reads the wire, and the snapshot's status
    /// leads them all, as a state reached before the handshake is not sent
    /// again.
    statuses: VecDeque<NetworkStreamStatus>,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            assembler: MonitorFrameAssembler::new(),
            commands: 0,
            server: None,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
            statuses: VecDeque::new(),
        }
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    /// Keeps the session alive like a real client: the daemon closes a
    /// session that has not heartbeat within its timeout, however busy the
    /// connection is otherwise.
    fn heartbeat_if_due(&mut self) {
        let Some(server) = self.server.clone() else {
            return;
        };
        if self.last_heartbeat.elapsed() < HEARTBEAT_EVERY {
            return;
        }
        self.heartbeats += 1;
        self.send(&WireMessage::Heartbeat(HeartbeatMessage {
            server,
            sequence: self.heartbeats,
            sent_at_ms: self.heartbeats,
            last_applied: None,
        }));
        self.last_heartbeat = Instant::now();
    }

    /// The next message; no line on the wire ever carries the URL secret.
    fn receive(&mut self) -> WireMessage {
        self.heartbeat_if_due();
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed"
        );
        assert!(!line.contains(SECRET), "the URL secret leaked: {line}");
        let message = decode_line(&line).unwrap();
        match &message {
            WireMessage::HandshakeResponse(response) => {
                self.server = Some(response.server.clone());
            }
            WireMessage::Snapshot(snapshot) => {
                self.statuses.extend(
                    snapshot
                        .network_streams
                        .iter()
                        .filter(|entry| entry.input == camera())
                        .map(|entry| entry.status.clone()),
                );
            }
            WireMessage::RuntimeEvent(event) => {
                if let RuntimeLifecycleEvent::NetworkStreamStatus { input, status } = &event.event
                    && *input == camera()
                {
                    self.statuses.push_back((**status).clone());
                }
            }
            _ => {}
        }
        assert!(
            !matches!(&message, WireMessage::Error(_)),
            "daemon error: {message:?}"
        );
        message
    }

    fn wait_for<T>(
        &mut self,
        what: &str,
        mut found: impl FnMut(&mut Self, WireMessage) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let message = self.receive();
            if let Some(value) = found(self, message) {
                return value;
            }
        }
    }

    /// The Program monitor picture `message` completes, if any.
    fn program(&mut self, message: WireMessage) -> Option<MonitorImage> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        let image = self.assembler.push(&part).unwrap()?;
        (image.tap == MonitorTap::Program).then_some(image)
    }

    /// Waits for a camera status that satisfies `wanted`, starting with the
    /// statuses that arrived (or came in the snapshot) since the last one
    /// this looked at.
    fn status(
        &mut self,
        what: &str,
        wanted: impl Fn(&NetworkStreamStatus) -> bool,
    ) -> NetworkStreamStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            while let Some(status) = self.statuses.pop_front() {
                if wanted(&status) {
                    return status;
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.receive();
        }
    }

    /// Waits until Program shows a picture that satisfies `wanted`.
    fn shows(&mut self, what: &str, wanted: impl Fn([u8; 4]) -> bool) {
        self.wait_for(what, |client, message| {
            let image = client.program(message)?;
            let width = usize::from(image.width);
            let at = (usize::from(image.height) / 2 * width + width / 2) * 4;
            let pixel = [
                image.rgba[at],
                image.rgba[at + 1],
                image.rgba[at + 2],
                image.rgba[at + 3],
            ];
            wanted(pixel).then_some(())
        });
    }

    fn accept(&mut self, payload: CommandPayload) -> Vec<EventPayload> {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        let described = format!("{payload:?}");
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let mut events = Vec::new();
        self.wait_for("the command result", |_, message| match message {
            WireMessage::Event(event) => {
                events.push(event.payload);
                None
            }
            WireMessage::CommandResult(result) => {
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{described}: {result:?}"
                );
                Some(())
            }
            _ => None,
        });
        events
    }
}

/// A red test signal with a tone, published to `url` in `format`; stopped
/// on drop.
///
/// An `ffmpeg` publisher does not retry, so one that exits before it is
/// dropped is started again, as a real encoder reconnects. (A listening
/// receiver reports `listening` only once its port accepts, and an RTMP
/// listener's port stays bound between publishers; an SRT caller that
/// starts before a session's next child binds the port retransmits its
/// handshake until its connect timeout.)
struct Publisher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Publisher {
    fn start(format: &str, url: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let (format, url) = (format.to_owned(), url.to_owned());
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut child = Self::spawn(&format, &url);
                while !stopping.load(Ordering::Acquire) && matches!(child.try_wait(), Ok(None)) {
                    thread::sleep(Duration::from_millis(20));
                }
                let _ = child.kill();
                let _ = child.wait();
                if !stopping.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn spawn(format: &str, url: &str) -> Child {
        Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-re"])
            .args(["-f", "lavfi", "-i", "color=c=red:s=320x180:r=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
            // FLV carries `flv1`; MPEG-TS needs an MPEG video codec.
            .args(["-c:v", if format == "flv" { "flv" } else { "mpeg2video" }])
            .args(["-g", "10", "-b:v", "400k", "-c:a", "aac", "-f", format, url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Whether `ffmpeg` has `libsrt`; its absence fails only under
/// `FM_REQUIRE_FFMPEG_SRT=1`.
fn srt_available() -> bool {
    let available = Command::new("ffmpeg")
        .args(["-hide_banner", "-protocols"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim() == "srt")
        });
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG_SRT").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG_SRT=1 but ffmpeg has no libsrt"
    );
    available
}

fn red(pixel: [u8; 4]) -> bool {
    pixel[0] > 160 && pixel[1] < 90 && pixel[2] < 90
}

fn black(pixel: [u8; 4]) -> bool {
    pixel[0] < 30 && pixel[1] < 30 && pixel[2] < 30
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_receives_a_network_stream_and_reports_its_status_over_the_wire() {
    if !ffmpeg_available() {
        eprintln!("native network stream integration skipped: FFmpeg unavailable");
        return;
    }
    let port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-network-stream.freemix");
    save_project(&project_path, port);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native network stream integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-network-stream-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Admin,
        resume_cursor: None,
    }));
    assert!(matches!(
        client.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    let WireMessage::Snapshot(snapshot) = client.receive() else {
        panic!("expected the snapshot");
    };
    let [entry] = snapshot.network_streams.as_slice() else {
        panic!("one network stream: {:?}", snapshot.network_streams);
    };
    assert_eq!(entry.input, camera());
    assert_eq!(
        entry.config.url,
        format!("udp://127.0.0.1:{port}?token=***"),
        "the snapshot redacts the URL"
    );
    assert_eq!(entry.config.settings.stall, StallSlate::Black);

    // Nothing is sent yet: the receiver connects and waits. On a loaded
    // host its first attempt can give up on the silence (after its timeout
    // and connect grace) before this client subscribes, so it may already
    // be reconnecting.
    let _ = client.status("a connecting status", |status| {
        matches!(
            status.state,
            NetworkStreamState::Connecting | NetworkStreamState::Reconnecting { .. }
        )
    });

    let sender = Sender::start(port);
    let live = client.status("live", |status| status.state == NetworkStreamState::Live);
    assert!(live.buffer_ms <= 400, "{live:?}");
    client.shows("the stream's red picture on Program", red);
    let _ = client.status("a measured bitrate", |status| {
        status.state == NetworkStreamState::Live && status.bitrate_bps > 0
    });

    // The sender dies: the input stalls to its black slate and reconnects.
    drop(sender);
    let reconnecting = client.status("reconnecting", |status| {
        matches!(status.state, NetworkStreamState::Reconnecting { .. })
    });
    assert!(reconnecting.reconnects >= 1, "{reconnecting:?}");
    assert!(reconnecting.last_error.is_some(), "{reconnecting:?}");
    client.shows("the black stall slate on Program", black);

    // The sender returns: the input recovers.
    let sender = Sender::start(port);
    client.status("live again", |status| {
        status.state == NetworkStreamState::Live
    });
    client.shows("the red picture again", red);

    // The stream's drift loop reaches the Studio as `source_sync_status`
    // (AU-004, Protocol 2.76) and answers diagnostics.
    let synced = client.wait_for("the stream's drift telemetry", |_, message| {
        let WireMessage::SourceSyncStatus(status) = message else {
            return None;
        };
        status
            .sources
            .into_iter()
            .find(|sample| sample.input == camera() && sample.state == SourceClockState::Running)
    });
    assert_eq!(synced.kind, SourceSyncKind::Stream, "{synced:?}");
    assert!(synced.target_us > 0, "{synced:?}");
    assert!(synced.drift_ppm().abs() <= 1_000.0, "{synced:?}");
    assert!(
        (synced.resampling_ratio() - 1.0).abs() < 0.01,
        "a 48 kHz source into the 48 kHz engine: {synced:?}"
    );
    client.send(&WireMessage::DiagnosticsRequest(DiagnosticsRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "diagnostics-sync".into(),
    }));
    let diagnostics = client.wait_for("the diagnostics response", |_, message| match message {
        WireMessage::DiagnosticsResponse(response) => Some(response),
        _ => None,
    });
    assert!(
        diagnostics
            .source_sync
            .iter()
            .any(|sample| sample.input == camera() && sample.kind == SourceSyncKind::Stream),
        "{diagnostics:?}"
    );

    // A settings change reconnects with the new jitter buffer, and its
    // event redacts the URL the edit kept.
    let mut settings = entry.config.settings;
    settings.jitter_buffer_ms = 400;
    let mut events = client.accept(CommandPayload::SetNetworkStream {
        input: camera(),
        url: None,
        settings,
        stream_id: None,
        stream_key: SecretEdit::Keep,
        passphrase: SecretEdit::Keep,
    });
    let changed = client.wait_for("a network_stream_changed event", |_, message| {
        if let WireMessage::Event(event) = message {
            events.push(event.payload);
        }
        events.iter().find_map(|event| match event {
            EventPayload::NetworkStreamChanged { input, config } if *input == camera() => {
                Some(config.clone())
            }
            _ => None,
        })
    });
    assert_eq!(changed.settings.jitter_buffer_ms, 400);
    assert!(changed.url.ends_with("?token=***"), "{}", changed.url);
    let live = client.status("live after the change", |status| {
        status.state == NetworkStreamState::Live && status.buffer_ms > 200
    });
    assert!(live.buffer_ms <= 800, "{live:?}");
    drop(sender);
    drop(client);

    // The daemon logged the state changes with the URL redacted.
    let logged = daemon.log.try_iter().collect::<Vec<_>>();
    assert!(
        logged
            .iter()
            .any(|line| line.starts_with("FREEMIXD_NETWORK") && line.contains("state=live")),
        "{logged:?}"
    );
    assert!(
        logged.iter().all(|line| !line.contains(SECRET)),
        "{logged:?}"
    );
    // The stream's audio ran through its drift-correcting loop (AU-004),
    // which reported itself once running.
    assert!(
        logged.iter().any(|line| {
            line.starts_with(&format!("FREEMIXD_SOURCE_SYNC\tv=1\tinput={}\t", input(2)))
                && line.contains("\tkind=stream\tstate=running\t")
                && line.contains("\tresyncs=0\t")
        }),
        "{logged:?}"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_listens_for_an_rtmp_push_and_an_encrypted_srt_caller() {
    if !ffmpeg_available() {
        eprintln!("native RTMP/SRT integration skipped: FFmpeg unavailable");
        return;
    }
    let free_tcp = || {
        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let rtmp_port = free_tcp();
    let stream_key = format!("live_{SECRET}");
    let passphrase = format!("pass-{SECRET}-phrase");
    let mut config = NetworkStreamConfig::new(format!("rtmp://127.0.0.1:{rtmp_port}/live"));
    config.settings.mode = StreamMode::Listener;
    config.settings.jitter_buffer_ms = 200;
    config.settings.timeout_ms = 2_000;
    config.settings.reconnect.backoff_min_ms = 200;
    config.settings.reconnect.backoff_max_ms = 1_000;
    config.stream_key = Some(stream_key.clone());
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-rtmp-srt.freemix");
    save_project_with(&project_path, config.clone());
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native RTMP/SRT integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-rtmp-srt-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Admin,
        resume_cursor: None,
    }));
    assert!(matches!(
        client.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    let WireMessage::Snapshot(snapshot) = client.receive() else {
        panic!("expected the snapshot");
    };
    let [entry] = snapshot.network_streams.as_slice() else {
        panic!("one network stream: {:?}", snapshot.network_streams);
    };
    assert_eq!(entry.config.stream_key.as_deref(), Some("***"));
    assert_eq!(entry.config.settings.mode, StreamMode::Listener);

    // The listener waits for its publisher without reconnecting.
    let listening = client.status("listening", |status| {
        status.state == NetworkStreamState::Listening
    });
    assert_eq!(listening.reconnects, 0, "{listening:?}");

    // `listening` means the port accepts the push.
    let publisher = Publisher::start(
        "flv",
        &format!("rtmp://127.0.0.1:{rtmp_port}/live/{stream_key}"),
    );
    client.status("live from the RTMP push", |status| {
        status.state == NetworkStreamState::Live
    });
    client.shows("the pushed red picture on Program", red);
    drop(publisher);
    client.status("listening again after the publisher left", |status| {
        status.state == NetworkStreamState::Listening
    });

    if srt_available() {
        // Reconfigured live as an encrypted SRT listener: the key is
        // cleared, the passphrase set; the event masks the passphrase.
        let srt_port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let settings = NetworkStreamSettings {
            mode: StreamMode::Listener,
            key_length: SrtKeyLength::Aes256,
            latency_ms: 120,
            ..config.settings
        };
        let mut events = client.accept(CommandPayload::SetNetworkStream {
            input: camera(),
            url: Some(StreamUrl::new(format!("srt://127.0.0.1:{srt_port}"))),
            settings,
            stream_id: Some("cam1".into()),
            stream_key: SecretEdit::Clear,
            passphrase: SecretEdit::Set(passphrase.clone()),
        });
        let changed = client.wait_for("the SRT network_stream_changed event", |_, message| {
            if let WireMessage::Event(event) = message {
                events.push(event.payload);
            }
            events.iter().find_map(|event| match event {
                EventPayload::NetworkStreamChanged { input, config } if *input == camera() => {
                    Some(config.clone())
                }
                _ => None,
            })
        });
        assert_eq!(changed.passphrase.as_deref(), Some("***"));
        assert_eq!(changed.stream_key, None);
        assert_eq!(changed.stream_id.as_deref(), Some("cam1"));
        client.status("the SRT listener listening", |status| {
            status.state == NetworkStreamState::Listening
        });
        let _caller = Publisher::start(
            "mpegts",
            &format!(
                "srt://127.0.0.1:{srt_port}?mode=caller&passphrase={passphrase}&pbkeylen=32&streamid=cam1"
            ),
        );
        let live = client.status("live from the SRT caller", |status| {
            status.state == NetworkStreamState::Live && status.bitrate_bps > 0
        });
        assert_eq!(live.source_channels, 1, "{live:?}");
        client.shows("the SRT caller's red picture on Program", red);
    }
    drop(client);

    let logged = daemon.log.try_iter().collect::<Vec<_>>();
    assert!(
        logged.iter().all(|line| !line.contains(SECRET)),
        "a secret reached the log: {logged:?}"
    );
}

/// Protocol 2.59: a native daemon sends a subscribed Studio session live
/// thumbnails of its inputs. The network stream sits on Preview, and its red
/// `input_thumbnail` comes from the input's own feed, not the Preview monitor
/// tap. Nothing arrives before the subscription or after it
/// ends, and each input's thumbnails stay within the 2 fps cadence.
#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_sends_live_input_thumbnails_to_a_subscribed_studio() {
    if !ffmpeg_available() {
        eprintln!("native input thumbnail integration skipped: FFmpeg unavailable");
        return;
    }
    let port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-input-thumbnails.freemix");
    let mut config = NetworkStreamConfig::new(stream_url(port));
    config.settings.jitter_buffer_ms = 200;
    config.settings.timeout_ms = 1_000;
    config.settings.reconnect.backoff_min_ms = 200;
    config.settings.reconnect.backoff_max_ms = 1_000;
    save_project_routed(&project_path, config, 1, 2);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native input thumbnail integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-input-thumbnails-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Admin,
        resume_cursor: None,
    }));
    assert!(matches!(
        client.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    let sender = Sender::start(port);
    client.status("live", |status| status.state == NetworkStreamState::Live);
    // Unsubscribed, the session carries no thumbnails.
    let quiet = Instant::now() + Duration::from_secs(2);
    while Instant::now() < quiet {
        assert!(
            !matches!(client.receive(), WireMessage::InputThumbnail(_)),
            "a thumbnail arrived before the subscription"
        );
    }

    client.send(&WireMessage::InputThumbnailSubscription(
        InputThumbnailSubscription { enabled: true },
    ));
    let thumbnail = |message: WireMessage| match message {
        WireMessage::InputThumbnail(thumbnail) => Some(thumbnail),
        _ => None,
    };
    let center = |thumbnail: &InputThumbnailMessage| {
        let image = fm_protocol::decode_input_thumbnail(thumbnail).unwrap();
        let width = usize::from(image.width);
        let at = (usize::from(image.height) / 2 * width + width / 2) * 4;
        [image.rgba[at], image.rgba[at + 1], image.rgba[at + 2]]
    };
    let camera_thumbnail = client.wait_for("the stream's red thumbnail", |_, message| {
        let thumbnail = thumbnail(message)?;
        (thumbnail.input == camera()
            && red({
                let [r, g, b] = center(&thumbnail);
                [r, g, b, 255]
            }))
        .then_some(thumbnail)
    });
    assert_eq!(camera_thumbnail.origin, MonitorFrameOrigin::Native);
    assert_eq!(
        (camera_thumbnail.width, camera_thumbnail.height),
        (160, 90),
        "320x180 fits 160x90"
    );
    assert!(camera_thumbnail.age_ms < 2_000, "{camera_thumbnail:?}");
    let solid = client.wait_for("the solid's thumbnail", |_, message| {
        thumbnail(message).filter(|thumbnail| thumbnail.input == WireInputId::from_domain(input(1)))
    });
    assert_eq!(center(&solid), [12, 34, 56]);

    // Each input's thumbnails stay within the 2 fps cadence, in order.
    let window = Instant::now();
    let mut sequences = Vec::new();
    while window.elapsed() < Duration::from_secs(3) {
        if let Some(thumbnail) = thumbnail(client.receive())
            && thumbnail.input == camera()
        {
            sequences.push(thumbnail.sequence);
        }
    }
    assert!(!sequences.is_empty(), "the live stream keeps sending");
    assert!(
        sequences.len() <= 8,
        "{} thumbnails in 3 s",
        sequences.len()
    );
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));

    // Unsubscribed again, thumbnails stop (one already in flight may land).
    client.send(&WireMessage::InputThumbnailSubscription(
        InputThumbnailSubscription { enabled: false },
    ));
    let settle = Instant::now() + Duration::from_millis(500);
    while Instant::now() < settle {
        client.receive();
    }
    let quiet = Instant::now() + Duration::from_secs(2);
    while Instant::now() < quiet {
        assert!(
            !matches!(client.receive(), WireMessage::InputThumbnail(_)),
            "a thumbnail arrived after unsubscribing"
        );
    }
    drop(sender);
    drop(daemon);
}
