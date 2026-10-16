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
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket},
    num::NonZeroU128,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventPayload, HandshakeRequest, HeartbeatMessage, MonitorFrameAssembler, MonitorImage,
    MonitorTap, Role, RuntimeLifecycleEvent, ServerIdentity, WireInputId, WireMessage, decode_line,
    encode_line,
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
    project.set_main_mix(MainMix::new(input(2), input(1)));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(2)),
            realized_program_id: Some(input(2)),
            desired_preview_id: Some(input(1)),
            realized_preview_id: Some(input(1)),
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
        if let WireMessage::HandshakeResponse(response) = &message {
            self.server = Some(response.server.clone());
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

    /// Waits for a camera status that satisfies `wanted`.
    fn status(
        &mut self,
        what: &str,
        wanted: impl Fn(&NetworkStreamStatus) -> bool,
    ) -> NetworkStreamStatus {
        self.wait_for(what, |_, message| {
            let WireMessage::RuntimeEvent(event) = message else {
                return None;
            };
            let RuntimeLifecycleEvent::NetworkStreamStatus { input, status } = event.event else {
                return None;
            };
            (input == camera() && wanted(&status)).then_some(*status)
        })
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

/// A red test signal with a tone, published to `url` in `format`; killed
/// on drop.
struct Publisher(Child);

impl Publisher {
    fn start(format: &str, url: &str) -> Self {
        Self(
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
                .unwrap(),
        )
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// How long a listening receiver's child is given to bind its port.
const LISTEN_SETTLE: Duration = Duration::from_secs(2);

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

    // The status turns `listening` as the child starts; give it a moment
    // to bind before the publisher (which does not retry) connects.
    thread::sleep(LISTEN_SETTLE);
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
        thread::sleep(LISTEN_SETTLE);
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
