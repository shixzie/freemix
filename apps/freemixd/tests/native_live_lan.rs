#![cfg(all(feature = "native-media", unix))]

//! Process-level `LiveLAN` (OR-017, Protocol 2.57): a native daemon started
//! with `--live-lan-listen` serves a viewer page and, once a client enables
//! `LiveLAN`, a low-latency fragmented-MP4 HLS stream of Program or of one
//! chosen project output behind an optional PIN. The test fetches the page,
//! opens a viewer session with the PIN, downloads the playlist, the
//! initialization segment, and media segments over HTTP, probes them as the
//! fragmented MP4 a Media Source Extensions player appends, decodes them to
//! check which output they show, and follows the
//! `live_lan_status` runtime events (state, URL, viewer count). The PIN
//! never appears on the wire, in the daemon's logs, or in any HTTP reply.
//!
//! The test needs `FFmpeg` and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::Path,
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
    ProjectPosition, ProjectStore, RuntimeFadeToBlack, RuntimeManualTransitions,
    RuntimeOverlayChannel, RuntimeOverlays, RuntimeRouting, StoredProject,
};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventPayload, HandshakeRequest, HeartbeatMessage, LiveLanEntry, LiveLanSource, LiveLanState,
    LiveLanStatus, Role, RuntimeLifecycleEvent, SecretEdit, ServerIdentity, SnapshotMessage,
    WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
const PIN: &str = "90817263";
const RED: [u8; 3] = [210, 20, 20];
const BLUE: [u8; 3] = [20, 30, 210];
const PROGRAM_OUTPUT: u128 = 9_301;
const CLEAN_OUTPUT: u128 = 9_302;
/// Commands sent by every session of the test.
static COMMANDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
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

/// A 128x72 show with red input 1 on Program and two project outputs. A
/// full-frame overlay of blue input 2 is on air and included only in output
/// 9301 (Program, the first output), so output 9302 is a red clean feed.
fn save_project(path: &Path) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(9_300)),
        "Native LiveLAN",
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
    let bus = BusId::new(id(9_350));
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    for (index, color, output) in [(1, RED, PROGRAM_OUTPUT), (2, BLUE, CLEAN_OUTPUT)] {
        // Both outputs carry Program's mix; only the overlay differs.
        project.add_input(Input {
            id: input(index),
            name: format!("Solid {index}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(color[0], color[1], color[2], 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        let scene = SceneId::new(id(9_310 + index));
        project.add_scene(Scene {
            id: scene,
            name: format!("Scene {index}"),
            background: Rgba8::OPAQUE_BLACK,
            layers: vec![Layer {
                id: fm_model::LayerId::new(1).unwrap(),
                name: "Source".into(),
                source: SourceRef::Input(input(index)),
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
            id: OutputId::new(id(output)),
            name: format!("Output {index}"),
            video_source: scene,
            audio_source: bus,
            startup: StartupPolicy::ReconcileDesiredState,
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(input(1), input(2)));
    let mut overlays = RuntimeOverlays::default();
    let channel = RuntimeOverlayChannel {
        source: Some(input(2)),
        active: true,
        included_outputs: vec![OutputId::new(id(PROGRAM_OUTPUT))],
        ..RuntimeOverlayChannel::empty()
    };
    overlays.desired[0] = channel.clone();
    overlays.realized[0] = channel;
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
        overlays,
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    ProjectStore::new(path).unwrap().save(&stored).unwrap();
}

struct NativeDaemon {
    child: Child,
    address: SocketAddr,
    viewer: SocketAddr,
    log: mpsc::Receiver<String>,
}

impl NativeDaemon {
    /// Every log line so far.
    fn drain_log(&self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(line) = self.log.recv_timeout(Duration::from_millis(200)) {
            lines.push(line);
        }
        lines
    }
}

impl Drop for NativeDaemon {
    fn drop(&mut self) {
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
        .arg("--live-lan-listen")
        .arg("127.0.0.1:0")
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
        let viewer = loop {
            let line = receiver
                .recv_timeout(STARTUP_TIMEOUT)
                .expect("the LiveLAN readiness line");
            if let Some(address) = line.strip_prefix("FREEMIXD_LIVE_LAN_READY\tv=1\taddress=") {
                break address.parse().unwrap();
            }
        };
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
            viewer,
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
    writer: TcpStream,
    server: Option<ServerIdentity>,
    heartbeats: u64,
    last_heartbeat: Instant,
}

impl Client {
    fn connect(address: SocketAddr) -> (Self, SnapshotMessage) {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            server: None,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-live-lan-test".into(),
            client_type: ClientType::Studio,
            desired_role: Role::Admin,
            resume_cursor: None,
        }));
        let WireMessage::HandshakeResponse(response) = client.wait_message() else {
            panic!("expected the handshake response");
        };
        client.server = Some(response.server);
        let WireMessage::Snapshot(snapshot) = client.wait_message() else {
            panic!("expected the snapshot");
        };
        (client, snapshot)
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    /// Sends a heartbeat once a second so the session stays open however
    /// long the test waits.
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

    /// The next message within half a second, if any. No line from the
    /// daemon ever carries the PIN.
    fn poll(&mut self) -> Option<WireMessage> {
        self.heartbeat();
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => panic!("daemon closed"),
            Ok(_) => {
                assert!(!line.contains(PIN), "the PIN reached the wire: {line}");
                Some(decode_line(&line).unwrap())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                assert!(line.is_empty(), "a partial line timed out");
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

    /// Polls until `found` returns a value, keeping the session alive.
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

    /// Waits for a `live_lan_status` runtime event that satisfies `wanted`.
    fn status(&mut self, what: &str, wanted: impl Fn(&LiveLanStatus) -> bool) -> LiveLanStatus {
        self.wait_for(what, |message| match message {
            Some(WireMessage::RuntimeEvent(event)) => match event.event {
                RuntimeLifecycleEvent::LiveLanStatus(status) if wanted(&status) => Some(status),
                _ => None,
            },
            _ => None,
        })
    }

    /// Submits `payload` and returns its result with the events that
    /// arrived before it.
    fn submit(&mut self, payload: CommandPayload) -> (CommandResult, Vec<EventPayload>) {
        // Identifiers stay unique across sessions: the daemon answers a
        // reused idempotency key with the first command's receipt.
        let id = format!(
            "live-lan-{}",
            COMMANDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
        );
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let mut events = Vec::new();
        let result = self.wait_for("the command result", |message| match message {
            Some(WireMessage::CommandResult(result)) => Some(result),
            Some(WireMessage::Event(event)) => {
                events.push(event.payload);
                None
            }
            _ => None,
        });
        (result, events)
    }

    fn set_live_lan(&mut self, config: LiveLanEntry, pin: SecretEdit) -> Vec<EventPayload> {
        let (result, events) = self.submit(CommandPayload::SetLiveLan { config, pin });
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
        events
    }
}

/// One HTTP exchange; the reply's head and body.
fn http(address: SocketAddr, request: &str) -> (String, Vec<u8>) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a complete HTTP head");
    let head = String::from_utf8(reply[..split].to_vec()).unwrap();
    assert!(!head.contains(PIN), "the PIN reached an HTTP reply: {head}");
    (head, reply[split + 4..].to_vec())
}

fn get(address: SocketAddr, path: &str, cookie: Option<&str>) -> (String, Vec<u8>) {
    let cookie = cookie.map_or_else(String::new, |cookie| format!("Cookie: {cookie}\r\n"));
    http(
        address,
        &format!("GET {path} HTTP/1.1\r\nHost: freemix\r\n{cookie}\r\n"),
    )
}

/// Posts `pin`, returning the reply head and the session cookie, if any.
fn open_session(address: SocketAddr, pin: &str) -> (String, Option<String>) {
    let body = format!("pin={pin}");
    let (head, _) = http(
        address,
        &format!(
            "POST /v1/live/session HTTP/1.1\r\nHost: freemix\r\nContent-Type: \
             application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    let cookie = head.lines().find_map(|line| {
        let value = line.strip_prefix("Set-Cookie: ")?;
        Some(value.split(';').next()?.to_owned())
    });
    (head, cookie)
}

/// Fetches the playlist until it lists two segments, then the
/// initialization segment it maps and those two, as a Media Source
/// Extensions player would: the init segment followed by the media
/// segments, as one fragmented MP4.
fn fetch_segment(client: &mut Client, viewer: SocketAddr, cookie: &str) -> Vec<u8> {
    let segments = client.wait_for("two published segments", |_| {
        let (head, body) = get(viewer, "/live/index.m3u8", Some(cookie));
        if !head.starts_with("HTTP/1.1 200") {
            return None;
        }
        assert!(head.contains("application/vnd.apple.mpegurl"), "{head}");
        let playlist = String::from_utf8(body).unwrap();
        assert!(playlist.starts_with("#EXTM3U"), "{playlist}");
        assert!(
            playlist.contains("#EXT-X-MAP:URI=\"init.mp4\""),
            "{playlist}"
        );
        let segments = playlist
            .lines()
            .filter(|line| is_segment(line))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        (segments.len() >= 2).then(|| segments[segments.len() - 2..].to_vec())
    });
    let (head, mut media) = get(viewer, "/live/init.mp4", Some(cookie));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("Content-Type: video/mp4"), "{head}");
    assert_eq!(&media[4..8], b"ftyp", "the init segment is an MP4");
    for segment in segments {
        let (head, body) = get(viewer, &format!("/live/{segment}"), Some(cookie));
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("Content-Type: video/mp4"), "{head}");
        assert!(
            body.windows(4).any(|window| window == b"moof"),
            "a media segment is a movie fragment"
        );
        media.extend(body);
    }
    media
}

/// The streams `ffprobe` finds in fragmented MP4 `media`, as
/// `codec_type:codec_name` lines.
fn probe_streams(directory: &Path, media: &[u8]) -> String {
    let path = directory.join("probe.mp4");
    std::fs::write(&path, media).unwrap();
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name",
            "-of",
            "csv=p=0:nk=1",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// The average colour of the first decoded frame of fragmented MP4 `media`
/// (an init segment followed by media segments).
fn segment_color(directory: &Path, segment: &[u8]) -> [u8; 3] {
    let path = directory.join("segment.mp4");
    std::fs::write(&path, segment).unwrap();
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pixels = output.stdout.len() / 3;
    assert_eq!(pixels, 128 * 72);
    let mut sums = [0_u64; 3];
    for pixel in output.stdout.as_chunks::<3>().0 {
        for (sum, value) in sums.iter_mut().zip(pixel) {
            *sum += u64::from(*value);
        }
    }
    sums.map(|sum| u8::try_from(sum / pixels as u64).unwrap())
}

fn close_to(actual: [u8; 3], expected: [u8; 3]) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(actual, expected)| actual.abs_diff(expected) <= 40)
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_serves_live_lan_viewers_over_http() {
    if !tools_available() {
        eprintln!("native LiveLAN integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-live-lan.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native LiveLAN integration skipped: {error}");
            return;
        }
    };
    let viewer = daemon.viewer;

    // The page is served while LiveLAN is off; sessions are not.
    let (head, page) = get(viewer, "/", None);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("Content-Security-Policy"), "{head}");
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains("/v1/live/session") && page.contains("/live/index.m3u8"));
    assert!(open_session(viewer, "").0.starts_with("HTTP/1.1 503"));

    let (mut client, snapshot) = Client::connect(daemon.address);
    assert_eq!(snapshot.live_lan, LiveLanEntry::default());
    assert_eq!(snapshot.live_lan_status, LiveLanStatus::default());

    // Enabling LiveLAN of Program with a PIN and one viewer goes live at
    // the listener's URL.
    let program = LiveLanEntry {
        enabled: true,
        source: LiveLanSource::Program,
        max_viewers: 1,
        has_pin: false,
    };
    let events = client.set_live_lan(program, SecretEdit::Set(PIN.into()));
    let changed = EventPayload::LiveLanChanged(LiveLanEntry {
        has_pin: true,
        ..program
    });
    if !events.contains(&changed) {
        client.wait_for("the LiveLAN change", |message| match message {
            Some(WireMessage::Event(event)) if event.payload == changed => Some(()),
            _ => None,
        });
    }
    let live = client.status("LiveLAN live", |status| status.state == LiveLanState::Live);
    assert_eq!(live.url, format!("http://{viewer}/"));

    // A wrong PIN is refused; the right one opens a session whose cookie
    // fetches the playlist and a segment of Program (blue overlay).
    let (head, cookie) = open_session(viewer, "1357");
    assert!(
        head.starts_with("HTTP/1.1 401") && cookie.is_none(),
        "{head}"
    );
    assert!(
        get(viewer, "/live/index.m3u8", None)
            .0
            .starts_with("HTTP/1.1 401")
    );
    let (head, cookie) = open_session(viewer, PIN);
    assert!(head.starts_with("HTTP/1.1 204"), "{head}");
    assert!(head.contains("HttpOnly"), "{head}");
    let cookie = cookie.expect("a session cookie");
    let segment = fetch_segment(&mut client, viewer, &cookie);
    let streams = probe_streams(directory.path(), &segment);
    assert!(
        streams.contains("h264,video") || streams.contains("video,h264"),
        "{streams}"
    );
    assert!(
        streams.contains("aac,audio") || streams.contains("audio,aac"),
        "{streams}"
    );
    let color = segment_color(directory.path(), &segment);
    assert!(close_to(color, BLUE), "Program is blue, got {color:?}");

    // One viewer is watching: the status counts it, and a second viewer is
    // refused while the first watches.
    client.status("one viewer", |status| {
        status.state == LiveLanState::Live && status.viewers == 1
    });
    let (head, _) = open_session(viewer, PIN);
    assert!(head.starts_with("HTTP/1.1 503"), "{head}");

    // Streaming output 9302 instead restarts the encoder: the old session
    // ends, and a new one sees the red clean feed.
    let clean = LiveLanEntry {
        source: LiveLanSource::Output(OutputId::new(id(CLEAN_OUTPUT))),
        ..program
    };
    client.set_live_lan(clean, SecretEdit::Keep);
    client.wait_for("the old session ended", |_| {
        get(viewer, "/live/index.m3u8", Some(&cookie))
            .0
            .starts_with("HTTP/1.1 401")
            .then_some(())
    });
    let (head, cookie) = client.wait_for("a new session", |_| {
        let (head, cookie) = open_session(viewer, PIN);
        head.starts_with("HTTP/1.1 204").then_some((head, cookie))
    });
    assert!(head.contains("Set-Cookie"));
    let cookie = cookie.unwrap();
    let segment = client.wait_for("a segment of output 9302", |_| {
        let segment = fetch_segment_once(viewer, &cookie)?;
        let color = segment_color(directory.path(), &segment);
        close_to(color, RED).then_some(color)
    });
    assert!(close_to(segment, RED));

    // An unknown output is refused and changes nothing.
    let (result, _) = client.submit(CommandPayload::SetLiveLan {
        config: LiveLanEntry {
            source: LiveLanSource::Output(OutputId::new(id(9_399))),
            ..program
        },
        pin: SecretEdit::Keep,
    });
    assert!(
        matches!(&result, CommandResult::Rejected { .. }),
        "{result:?}"
    );

    // Disabling turns the stream off and ends every session.
    let events = client.set_live_lan(
        LiveLanEntry {
            enabled: false,
            ..clean
        },
        SecretEdit::Keep,
    );
    let disabled = EventPayload::LiveLanChanged(LiveLanEntry {
        enabled: false,
        has_pin: true,
        ..clean
    });
    if !events.contains(&disabled) {
        client.wait_for("LiveLAN off", |message| match message {
            Some(WireMessage::Event(event)) if event.payload == disabled => Some(()),
            _ => None,
        });
    }
    client.wait_for("the listener off", |_| {
        open_session(viewer, PIN)
            .0
            .starts_with("HTTP/1.1 503")
            .then_some(())
    });
    assert!(
        get(viewer, "/live/index.m3u8", Some(&cookie))
            .0
            .starts_with("HTTP/1.1 503")
    );

    // A new session's snapshot keeps the settings, with only whether a PIN
    // is set (the native daemon serves one session at a time).
    drop(client);
    let (_client, snapshot) = Client::connect(daemon.address);
    assert_eq!(snapshot.live_lan_status, LiveLanStatus::default());
    assert_eq!(
        snapshot.live_lan,
        LiveLanEntry {
            enabled: false,
            has_pin: true,
            ..clean
        }
    );
    for line in daemon.drain_log() {
        assert!(!line.contains(PIN), "the PIN reached the log: {line}");
    }
}

/// The init segment and the newest segment the playlist lists, if they are
/// published.
fn fetch_segment_once(viewer: SocketAddr, cookie: &str) -> Option<Vec<u8>> {
    let (head, body) = get(viewer, "/live/index.m3u8", Some(cookie));
    if !head.starts_with("HTTP/1.1 200") {
        return None;
    }
    let playlist = String::from_utf8(body).ok()?;
    let segment = playlist.lines().rfind(|line| is_segment(line))?;
    let (head, mut media) = get(viewer, "/live/init.mp4", Some(cookie));
    if !head.starts_with("HTTP/1.1 200") {
        return None;
    }
    let (head, body) = get(viewer, &format!("/live/{segment}"), Some(cookie));
    media.extend(body);
    head.starts_with("HTTP/1.1 200").then_some(media)
}

/// Whether a playlist line names a published `segment-<digits>.m4s`.
fn is_segment(line: &str) -> bool {
    line.strip_prefix("segment-")
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(digits, extension)| {
            extension == "m4s" && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}
