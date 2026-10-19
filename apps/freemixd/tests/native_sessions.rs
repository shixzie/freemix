//! Several control sessions on one native-media daemon: a Studio-like raw
//! peer, a CLI raw peer, and Web pages through the `--web-listen` gateway
//! drive one show together, each within its own role, and see each other's
//! cuts in revision order; the raw and Web session bounds refuse the client
//! beyond them at once; a Web tally keeps up with a cut every frame; and a
//! Studio peer that stops reading delays neither the other sessions nor
//! Program pacing or the recording.
//!
//! The tests need a native GPU adapter and `FFmpeg`/`ffprobe`. Without an
//! adapter they are skipped unless `FM_REQUIRE_NATIVE_MEDIA=1`; without the
//! tools unless `FM_REQUIRE_FFMPEG=1`.

#![cfg(all(feature = "native-media", unix))]

use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, RecorderSlot, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventPayload, HandshakeOutcome, HandshakeRequest, HeartbeatMessage, InputThumbnailSubscription,
    Role, ServerIdentity, SnapshotMessage, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;
use tungstenite::{
    HandshakeError, Message, WebSocket,
    http::{Request, StatusCode},
};

const TOKEN: &str = "native-sessions-token-0123456789abcdef";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_POLL: Duration = Duration::from_millis(50);
/// Well inside the daemon's 15 s session heartbeat timeout.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(2);
/// The most Web clients the gateway admits at once.
const WEB_CLIENTS: usize = 16;
/// The most raw TCP sessions the daemon serves at once.
const RAW_SESSIONS: usize = 8;
const FRAME_RATE: u32 = 25;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

/// Program after revision `revision` of a show of cuts only: input 1 and
/// input 2 swap on every cut.
fn program_at(revision: u64) -> WireInputId {
    if revision.is_multiple_of(2) {
        wire(1)
    } else {
        wire(2)
    }
}

fn tools_available() -> bool {
    let available = ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg or ffprobe is unavailable"
    );
    if !available {
        eprintln!("native sessions test skipped: ffmpeg/ffprobe unavailable");
    }
    available
}

/// A 64x48 25 fps generator show that records recorder 1 from startup.
fn save_project(path: &Path) {
    let rate = FrameRate::new(FRAME_RATE, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(7_031).unwrap()),
        "Native sessions test",
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
    for (id, video) in [
        (input(1), SimulatedVideo::Bars),
        (
            input(2),
            SimulatedVideo::Solid(SolidColor::new(24, 80, 160, 255)),
        ),
    ] {
        project.add_input(Input {
            id,
            name: format!("Generator {id}"),
            kind: InputKind::Simulated(SimulatedInput::new(video, SimulatedAudio::Silence)),
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(input(1), input(2)));
    project.set_recording_desired(RecorderSlot::new(1).unwrap(), true);
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

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "freemixd-native-sessions-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct DaemonProcess {
    child: Child,
    control: SocketAddr,
    web: SocketAddr,
    stderr: mpsc::Receiver<String>,
    ready_at: Instant,
}

impl DaemonProcess {
    /// Starts `freemixd serve --native-media --record-program <output>
    /// --web-listen`, or returns `None` when this host has no native media
    /// runtime (unless `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path, output: &Path) -> Option<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .args(["--native-media", "--web-listen", "127.0.0.1:0"])
            .arg("--record-program")
            .arg(output)
            .env("FREEMIXD_WEB_TOKEN", TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = drain(child.stderr.take().unwrap());
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (ready_sender, ready) = mpsc::channel();
        thread::spawn(move || {
            for _ in 0..2 {
                let mut line = String::new();
                let _ = stdout.read_line(&mut line);
                let _ = ready_sender.send(line);
            }
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let line = ready.recv_timeout(PROCESS_TIMEOUT).unwrap_or_default();
        if let Ok(readiness) = line.trim_end().parse::<ReadinessRecord>() {
            let ready_at = Instant::now();
            let web_line = ready.recv_timeout(PROCESS_TIMEOUT).unwrap();
            let web = web_line
                .strip_prefix("FREEMIXD_WEB_READY\tv=1\taddress=")
                .and_then(|line| line.trim_end().parse().ok())
                .unwrap_or_else(|| panic!("invalid web readiness {web_line:?}"));
            return Some(Self {
                child,
                control: readiness.address,
                web,
                stderr,
                ready_at,
            });
        }
        let _ = child.kill();
        let _ = child.wait();
        let detail: String = stderr.try_iter().collect();
        let message = format!("native daemon unavailable: {line:?}; stderr={detail}");
        assert!(
            std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
            "FM_REQUIRE_NATIVE_MEDIA=1: {message}"
        );
        eprintln!("native sessions test skipped: {message}");
        None
    }

    /// Cooperative stop (SIGTERM); returns everything written to stderr.
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
            assert!(Instant::now() < deadline, "daemon did not stop");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let failing = thread::panicking();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if failing {
            while let Ok(chunk) = self.stderr.recv_timeout(Duration::from_secs(2)) {
                eprint!("{chunk}");
            }
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

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let mut value = 0_u32;
        for (index, byte) in chunk.iter().enumerate() {
            value |= u32::from(*byte) << (16 - 8 * index);
        }
        for index in 0..=chunk.len() {
            encoded.push(char::from(
                ALPHABET[((value >> (18 - 6 * index)) & 0x3f) as usize],
            ));
        }
    }
    encoded
}

/// The upgrade the served pages send.
fn browser_upgrade(address: SocketAddr) -> Request<()> {
    Request::builder()
        .uri(format!("ws://{address}/v1/control"))
        .header("Host", address.to_string())
        .header("Origin", format!("http://{address}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header(
            "Sec-WebSocket-Protocol",
            format!(
                "freemix.control.v1, freemix.bearer.{}",
                base64url(TOKEN.as_bytes())
            ),
        )
        .body(())
        .unwrap()
}

/// What one session was sent besides command results.
#[derive(Default)]
struct Observed {
    /// The Program each switcher event named, in arrival order.
    switcher: Vec<(u64, WireInputId)>,
    monitor_frames: usize,
    audio_meters: usize,
    thumbnails: usize,
}

enum Transport {
    Raw {
        reader: BufReader<TcpStream>,
        writer: TcpStream,
    },
    Web(Box<WebSocket<TcpStream>>),
}

/// One control session, raw TCP or Web, that heartbeats while it waits.
struct Session {
    name: &'static str,
    transport: Transport,
    server: Option<ServerIdentity>,
    heartbeats: u64,
    last_heartbeat: Instant,
    observed: Observed,
}

impl Session {
    fn hello(client_type: ClientType, role: Role) -> WireMessage {
        WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-sessions-test".into(),
            client_type,
            desired_role: role,
            resume_cursor: None,
        })
    }

    /// A raw TCP session of `client_type` holding `role`.
    fn raw(
        name: &'static str,
        address: SocketAddr,
        client_type: ClientType,
        role: Role,
    ) -> (Self, SnapshotMessage) {
        Self::try_raw(name, address, client_type, role)
            .unwrap_or_else(|| panic!("{name}: the daemon refused the raw session"))
    }

    /// [`Self::raw`], or `None` when every raw session slot is taken and the
    /// daemon refuses the handshake as `resource_exhausted`.
    fn try_raw(
        name: &'static str,
        address: SocketAddr,
        client_type: ClientType,
        role: Role,
    ) -> Option<(Self, SnapshotMessage)> {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(READ_POLL)).unwrap();
        let writer = stream.try_clone().unwrap();
        Self::establish(
            name,
            Transport::Raw {
                reader: BufReader::new(stream),
                writer,
            },
            client_type,
            role,
        )
    }

    /// A Web session through the gateway, as the tally and operator pages
    /// open one.
    fn web(name: &'static str, address: SocketAddr, role: Role) -> (Self, SnapshotMessage) {
        Self::try_web(name, address, role)
            .unwrap_or_else(|| panic!("{name}: the gateway refused the upgrade"))
    }

    /// [`Self::web`], or `None` when the gateway refuses the upgrade.
    fn try_web(
        name: &'static str,
        address: SocketAddr,
        role: Role,
    ) -> Option<(Self, SnapshotMessage)> {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(PROCESS_TIMEOUT)).unwrap();
        let (socket, _) = tungstenite::client::client(browser_upgrade(address), stream).ok()?;
        socket.get_ref().set_read_timeout(Some(READ_POLL)).unwrap();
        Self::establish(
            name,
            Transport::Web(Box::new(socket)),
            ClientType::Web,
            role,
        )
    }

    /// Handshakes on `transport`; `None` when the daemon refuses the session
    /// because every slot is taken.
    fn establish(
        name: &'static str,
        transport: Transport,
        client_type: ClientType,
        role: Role,
    ) -> Option<(Self, SnapshotMessage)> {
        let mut session = Self {
            name,
            transport,
            server: None,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
            observed: Observed::default(),
        };
        session.send(&Self::hello(client_type, role));
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let response = loop {
            assert!(Instant::now() < deadline, "{name}: no handshake response");
            match session.read() {
                Some(WireMessage::HandshakeResponse(response)) => break response,
                Some(other) => panic!("{name}: expected a handshake response, got {other:?}"),
                None => {}
            }
        };
        if let HandshakeOutcome::Rejected { error } = &response.outcome
            && error.code == "resource_exhausted"
        {
            assert!(error.retryable, "{name}: {error:?}");
            return None;
        }
        assert_eq!(response.granted_role, role, "{name}");
        assert!(
            matches!(response.outcome, HandshakeOutcome::Snapshot { .. }),
            "{name}"
        );
        session.server = Some(response.server);
        let snapshot = loop {
            assert!(Instant::now() < deadline, "{name}: no snapshot");
            if let Some(WireMessage::Snapshot(snapshot)) = session.receive() {
                break snapshot;
            }
        };
        Some((session, snapshot))
    }

    fn send(&mut self, message: &WireMessage) {
        let line = encode_line(message).unwrap();
        match &mut self.transport {
            Transport::Raw { writer, .. } => {
                writer.write_all(line.as_bytes()).unwrap();
                writer.flush().unwrap();
            }
            Transport::Web(socket) => socket.send(Message::text(line)).unwrap(),
        }
    }

    /// The next record, or `None` when none arrived within the read poll.
    fn read(&mut self) -> Option<WireMessage> {
        let name = self.name;
        match &mut self.transport {
            Transport::Raw { reader, .. } => {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => panic!("{name}: daemon closed the session"),
                    Ok(_) => Some(decode_line(&line).unwrap()),
                    Err(error)
                        if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                    {
                        assert!(line.is_empty(), "{name}: a partial line timed out");
                        None
                    }
                    Err(error) => panic!("{name}: {error}"),
                }
            }
            Transport::Web(socket) => match socket.read() {
                Ok(Message::Text(text)) => Some(decode_line(text.as_str()).unwrap()),
                Ok(Message::Ping(_) | Message::Pong(_)) => None,
                Ok(other) => panic!("{name}: unexpected WebSocket message {other:?}"),
                Err(tungstenite::Error::Io(error))
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    None
                }
                Err(error) => panic!("{name}: {error}"),
            },
        }
    }

    /// Keeps the session alive like a real client.
    fn heartbeat_if_due(&mut self) {
        let Some(server) = self.server.clone() else {
            return;
        };
        if self.last_heartbeat.elapsed() < HEARTBEAT_EVERY {
            return;
        }
        self.heartbeats += 1;
        let heartbeat = WireMessage::Heartbeat(HeartbeatMessage {
            server,
            sequence: self.heartbeats,
            sent_at_ms: self.heartbeats,
            last_applied: None,
        });
        self.send(&heartbeat);
        self.last_heartbeat = Instant::now();
    }

    /// The next record after noting what it shows.
    fn receive(&mut self) -> Option<WireMessage> {
        self.heartbeat_if_due();
        let message = self.read()?;
        match &message {
            WireMessage::Event(event) => {
                if let EventPayload::DesiredSwitcher { program, .. } = &event.payload {
                    self.observed
                        .switcher
                        .push((event.cursor.revision, *program));
                }
            }
            WireMessage::MonitorFrame(_) => self.observed.monitor_frames += 1,
            WireMessage::AudioMeters(_) => self.observed.audio_meters += 1,
            WireMessage::InputThumbnail(_) => self.observed.thumbnails += 1,
            _ => {}
        }
        Some(message)
    }

    /// Sends `payload` as command `id` and returns its result.
    fn command(&mut self, id: &str, payload: CommandPayload) -> CommandResult {
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.into(),
            idempotency_key: format!("{}-{id}", self.name),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(WireMessage::CommandResult(result)) = self.receive() {
                let found = match &result {
                    CommandResult::Accepted { id, .. } | CommandResult::Rejected { id, .. } => id,
                };
                if found == id {
                    return result;
                }
            }
        }
        panic!("{}: no result for {id}", self.name);
    }

    /// Cuts and returns the accepted revision.
    fn cut(&mut self, id: &str) -> u64 {
        match self.command(id, CommandPayload::Cut) {
            CommandResult::Accepted { revision, .. } => revision,
            other @ CommandResult::Rejected { .. } => {
                panic!("{}: {id} was not accepted: {other:?}", self.name)
            }
        }
    }

    /// Waits until the switcher event of `revision` arrived.
    fn sees(&mut self, revision: u64) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while self
            .observed
            .switcher
            .last()
            .is_none_or(|(seen, _)| *seen < revision)
        {
            assert!(
                Instant::now() < deadline,
                "{}: never saw revision {revision}",
                self.name
            );
            self.receive();
        }
    }

    /// Keeps the session served until `ready` holds.
    fn until(&mut self, what: &str, ready: impl Fn(&Observed) -> bool) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while !ready(&self.observed) {
            assert!(Instant::now() < deadline, "{}: never got {what}", self.name);
            self.receive();
        }
    }

    /// Asserts every switcher event arrived once, in revision order, and
    /// named the Program that revision's cut left on air.
    fn assert_in_order(&self, through: u64) {
        let revisions: Vec<u64> = self.observed.switcher.iter().map(|(at, _)| *at).collect();
        assert_eq!(
            revisions,
            (1..=through).collect::<Vec<_>>(),
            "{}: switcher events out of order",
            self.name
        );
        for (revision, program) in &self.observed.switcher {
            assert_eq!(*program, program_at(*revision), "{}", self.name);
        }
    }
}

/// The status of an authorized upgrade the gateway refuses.
fn refused_upgrade(address: SocketAddr) -> StatusCode {
    let stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(PROCESS_TIMEOUT)).unwrap();
    match tungstenite::client::client(browser_upgrade(address), stream) {
        Err(HandshakeError::Failure(tungstenite::Error::Http(response))) => response.status(),
        other => panic!("expected a refused upgrade, got {:?}", other.map(|_| ())),
    }
}

/// Video frames in a segment, after proving it decodes without complaint.
fn decoded_video_frames(path: &Path) -> u64 {
    let decode = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        decode.status.success() && decode.stderr.is_empty(),
        "{} does not decode cleanly: {}",
        path.display(),
        String::from_utf8_lossy(&decode.stderr)
    );
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(probe.status.success());
    String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .unwrap()
}

/// One field of the shutdown `FREEMIXD_TELEMETRY` record.
fn telemetry_field(stderr: &str, field: &str) -> f64 {
    let record = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_TELEMETRY\t"))
        .unwrap_or_else(|| panic!("no telemetry record: {stderr}"));
    record
        .split('\t')
        .find_map(|entry| entry.strip_prefix(&format!("{field}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {field} in {record}"))
}

/// A Studio-like admin peer and a CLI operator peer on raw TCP, an Operator
/// page and a Viewer tally on the Web gateway: all four hold their own
/// sessions on one native daemon, each command is authorized against the
/// session that sent it, and every session sees every cut, whoever made it,
/// in revision order. A ninth raw peer and the seventeenth Web client are
/// refused at once until a session leaves. The recording keeps
/// running throughout and the daemon drains every session on shutdown.
#[test]
#[allow(clippy::too_many_lines)]
fn studio_cli_and_web_pages_drive_one_native_show_together() {
    if !tools_available() {
        return;
    }
    let directory = TestDirectory::new("together");
    let project = directory.0.join("show.freemix");
    let recording = directory.0.join("program.mp4");
    save_project(&project);
    let Some(daemon) = DaemonProcess::start(&project, &recording) else {
        return;
    };

    let (mut studio, snapshot) =
        Session::raw("studio", daemon.control, ClientType::Studio, Role::Admin);
    assert_eq!(snapshot.revision, 0);
    studio.send(&WireMessage::InputThumbnailSubscription(
        InputThumbnailSubscription { enabled: true },
    ));
    let (mut cli, _) = Session::raw("cli", daemon.control, ClientType::Cli, Role::Operator);
    let (mut operator, _) = Session::web("operator", daemon.web, Role::Operator);
    let (mut tally, _) = Session::web("tally", daemon.web, Role::Viewer);
    assert_eq!(operator.server, studio.server);
    assert_eq!(tally.server, cli.server);

    // Each session's role is its own: the tally cannot cut.
    let refused = tally.command("tally-cut", CommandPayload::Cut);
    assert!(
        matches!(&refused, CommandResult::Rejected { code, .. } if code == "permission_denied"),
        "{refused:?}"
    );

    // Cuts from every operating session, in turn; everyone sees each one.
    let mut slowest = Duration::ZERO;
    for cut in 1..=12_u64 {
        let started = Instant::now();
        let id = format!("cut-{cut}");
        let revision = match cut % 3 {
            0 => studio.cut(&id),
            1 => cli.cut(&id),
            _ => operator.cut(&id),
        };
        assert_eq!(revision, cut);
        for session in [&mut studio, &mut cli, &mut operator, &mut tally] {
            session.sees(revision);
        }
        slowest = slowest.max(started.elapsed());
    }
    assert!(
        slowest < Duration::from_secs(2),
        "a cut took {slowest:?} to reach every session"
    );
    for session in [&studio, &cli, &operator, &tally] {
        session.assert_in_order(12);
    }

    // The Studio peer gets the lossy side channels; Web pages never do.
    studio.until("monitor frames, meters, and thumbnails", |observed| {
        observed.monitor_frames > 0 && observed.audio_meters > 0 && observed.thumbnails > 0
    });
    for session in [&operator, &tally] {
        assert_eq!(session.observed.monitor_frames, 0);
        assert_eq!(session.observed.audio_meters, 0);
        assert_eq!(session.observed.thumbnails, 0);
    }

    // Fill every raw slot: a ninth raw connection is refused at once with a
    // retryable `resource_exhausted` handshake rejection, never left waiting.
    let mut raw_viewers = Vec::new();
    while raw_viewers.len() < RAW_SESSIONS - 2 {
        raw_viewers
            .push(Session::raw("raw-viewer", daemon.control, ClientType::Cli, Role::Viewer).0);
    }
    let refused_at = Instant::now();
    assert!(
        Session::try_raw("ninth", daemon.control, ClientType::Cli, Role::Viewer).is_none(),
        "a ninth raw peer was served"
    );
    assert!(refused_at.elapsed() < Duration::from_secs(2));

    // Fill every Web slot: the next authorized browser is refused with 503.
    let mut viewers = Vec::new();
    while viewers.len() < WEB_CLIENTS - 2 {
        viewers.push(Session::web("viewer", daemon.web, Role::Viewer).0);
    }
    assert_eq!(refused_upgrade(daemon.web), StatusCode::SERVICE_UNAVAILABLE);
    let revision = cli.cut("cut-13");
    for session in viewers
        .iter_mut()
        .chain([&mut studio, &mut operator, &mut tally])
    {
        session.sees(revision);
    }
    drop(viewers.pop());
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let (mut late, snapshot) = loop {
        if let Some(session) = Session::try_web("late", daemon.web, Role::Viewer) {
            break session;
        }
        assert!(Instant::now() < deadline, "no Web slot freed");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(snapshot.revision, 13);
    assert_eq!(snapshot.realized_program, program_at(13));

    // The CLI leaves; a new raw peer is served the current show once the
    // daemon has seen the CLI go.
    drop(cli);
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let (_returning, snapshot) = loop {
        if let Some(session) =
            Session::try_raw("returning", daemon.control, ClientType::Cli, Role::Viewer)
        {
            break session;
        }
        assert!(Instant::now() < deadline, "no raw slot freed");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(snapshot.revision, 13);
    drop(raw_viewers);

    let revision = operator.cut("cut-14");
    for session in [&mut studio, &mut tally, &mut late] {
        session.sees(revision);
    }
    studio.assert_in_order(14);
    tally.assert_in_order(14);

    let stderr = daemon.stop();
    assert!(decoded_video_frames(&recording) > 0, "{stderr}");
}

/// A Studio peer that subscribes to everything and then never reads again
/// (until its heartbeat timeout closes it) delays neither a CLI peer's nor
/// the Web pages' commands and events, nor the native Program: the pacer
/// realizes its typical frame on time and the recording holds every frame
/// of the run.
#[test]
#[allow(clippy::too_many_lines)]
fn a_stalled_studio_reader_never_delays_other_sessions_or_program_pacing() {
    const OBSERVATION: Duration = Duration::from_secs(8);
    const ROUND_TRIP_LIMIT: Duration = Duration::from_secs(1);

    if !tools_available() {
        return;
    }
    let directory = TestDirectory::new("stalled");
    let project = directory.0.join("show.freemix");
    let recording = directory.0.join("program.mp4");
    save_project(&project);
    let Some(daemon) = DaemonProcess::start(&project, &recording) else {
        return;
    };

    // Monitor frames, meters, stream status, thumbnails, and every event pile
    // up for this peer, which never reads a byte.
    let mut stalled = TcpStream::connect(daemon.control).unwrap();
    for message in [
        Session::hello(ClientType::Studio, Role::Admin),
        WireMessage::InputThumbnailSubscription(InputThumbnailSubscription { enabled: true }),
    ] {
        stalled
            .write_all(encode_line(&message).unwrap().as_bytes())
            .unwrap();
    }
    let (mut cli, _) = Session::raw("cli", daemon.control, ClientType::Cli, Role::Operator);
    let (mut operator, _) = Session::web("operator", daemon.web, Role::Operator);
    let (mut tally, _) = Session::web("tally", daemon.web, Role::Viewer);

    let started = Instant::now();
    let mut cuts = 0_u64;
    let mut slowest = Duration::ZERO;
    while started.elapsed() < OBSERVATION {
        let round_trip = Instant::now();
        cuts += 1;
        let id = format!("cut-{cuts}");
        let revision = if cuts.is_multiple_of(2) {
            cli.cut(&id)
        } else {
            operator.cut(&id)
        };
        assert_eq!(revision, cuts);
        for session in [&mut cli, &mut operator, &mut tally] {
            session.sees(revision);
        }
        slowest = slowest.max(round_trip.elapsed());
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        slowest < ROUND_TRIP_LIMIT,
        "a stalled Studio peer delayed the others by {slowest:?}"
    );
    for session in [&cli, &operator, &tally] {
        session.assert_in_order(cuts);
    }
    drop(stalled);

    let served = daemon.ready_at.elapsed();
    let stderr = daemon.stop();
    // Each accepted command journals before its frame, so the tail of the
    // lateness population belongs to the disk, in this and a one-session
    // run alike; the typical frame is realized on time.
    let p50 = telemetry_field(&stderr, "host_lateness_p50_ms");
    let frame_ms = 1_000.0 / f64::from(FRAME_RATE);
    assert!(
        p50 < frame_ms / 2.0,
        "Program pacing slipped: host lateness p50 {p50} ms"
    );
    // The recorder started before readiness, so it holds at least every
    // frame paced while the sessions were served.
    let frames = decoded_video_frames(&recording);
    let paced = served.as_secs_f64() * f64::from(FRAME_RATE);
    assert!(
        f64::from(u32::try_from(frames).unwrap()) >= paced * 0.95,
        "the recording holds {frames} frames of about {paced:.0} paced: {stderr}"
    );
}

/// A Viewer tally page follows a CLI that cuts as fast as the native daemon
/// takes commands (one per frame): it sees every cut, in revision order, and
/// is never dropped as a slow client. An Operator page that sends a burst of
/// commands larger than the gateway and session queues is slowed by
/// backpressure, never dropped, and every command is answered in order.
#[test]
fn web_pages_keep_up_with_a_cut_every_frame() {
    const CUTS: u64 = 100;
    const BURST: usize = 40;

    if !tools_available() {
        return;
    }
    let directory = TestDirectory::new("web-pace");
    let project = directory.0.join("show.freemix");
    let recording = directory.0.join("program.mp4");
    save_project(&project);
    let Some(daemon) = DaemonProcess::start(&project, &recording) else {
        return;
    };

    let (mut cli, _) = Session::raw("cli", daemon.control, ClientType::Cli, Role::Operator);
    let (mut tally, _) = Session::web("tally", daemon.web, Role::Viewer);
    let follower = thread::spawn(move || {
        tally.sees(CUTS);
        tally
    });
    for cut in 1..=CUTS {
        assert_eq!(cli.cut(&format!("cut-{cut}")), cut);
    }
    let tally = follower
        .join()
        .expect("the tally fell behind or was dropped");
    tally.assert_in_order(CUTS);
    // The journal is folded into the manifest right after the frame of the
    // 48th and the 96th cut, not in front of the 65th cut's frame (which
    // would leave the manifest at revision 64).
    let manifest = ProjectStore::new(&project).unwrap().load().unwrap();
    assert_eq!(manifest.position().revision, 96);

    let (mut operator, _) = Session::web("operator", daemon.web, Role::Operator);
    for index in 0..BURST {
        operator.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: format!("burst-{index}"),
            idempotency_key: format!("operator-burst-{index}"),
            expected_revision: None,
            deadline_ms: None,
            payload: CommandPayload::Cut,
        }));
    }
    let mut answered = Vec::new();
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while answered.len() < BURST {
        assert!(Instant::now() < deadline, "only {answered:?} were answered");
        if let Some(WireMessage::CommandResult(CommandResult::Accepted { id, .. })) =
            operator.receive()
        {
            answered.push(id);
        }
    }
    assert_eq!(
        answered,
        (0..BURST)
            .map(|index| format!("burst-{index}"))
            .collect::<Vec<_>>()
    );

    let stderr = daemon.stop();
    assert!(
        !stderr.contains("closed="),
        "a session was dropped: {stderr}"
    );
}
