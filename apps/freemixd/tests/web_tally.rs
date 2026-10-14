//! Phone tally over the loopback web gateway: the served page, browser-style
//! subprotocol authentication, a viewer tally that follows an operator's cut,
//! server-side role limits, edit padlocks, heartbeats, and reconnect.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use fm_model::{
    AudioBus, Input, InputKind, Layer, LayerGeometry, MainMix, Output, Project, ProjectSettings,
    Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    StartupPolicy,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EditLockScope, EditLockStatus, EventMessage, EventPayload, HandshakeOutcome, HandshakeRequest,
    HeartbeatMessage, Role, RuntimeLifecycleEvent, ServerIdentity, SnapshotMessage, WireInputId,
    WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;
use tungstenite::{
    HandshakeError, Message, WebSocket,
    http::{Request, StatusCode},
};

const TOKEN: &str = "tally-token/+=0123456789abcdefghijkl";
const PROJECT_ID: u128 = 0x7a11_0000;
const ID_BASE: u128 = 0x7a11_1000;
const TIMEOUT: Duration = Duration::from_secs(2);

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "freemixd-tally-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Daemon {
    child: Child,
    control: SocketAddr,
    web: SocketAddr,
}

impl Daemon {
    fn start(project: &Path) -> Self {
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .args(["--listen", "127.0.0.1:0", "--web-listen", "127.0.0.1:0"])
            .env("FREEMIXD_WEB_TOKEN", TOKEN)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut lines = [String::new(), String::new()];
        for line in &mut lines {
            assert_ne!(stdout.read_line(line).unwrap(), 0, "daemon exited early");
        }
        let control = lines[0].parse::<ReadinessRecord>().unwrap().address;
        let web = lines[1]
            .strip_prefix("FREEMIXD_WEB_READY\tv=1\taddress=")
            .and_then(|line| line.trim_end().parse().ok())
            .unwrap_or_else(|| panic!("invalid web readiness {:?}", lines[1]));
        Self {
            child,
            control,
            web,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn domain_input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(ID_BASE + value).unwrap())
}

fn input(value: u128) -> WireInputId {
    WireInputId::from_domain(domain_input(value))
}

fn create_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(PROJECT_ID).unwrap()),
        "Tally Test",
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
                sample_rate: SampleRate::new(48_000).unwrap(),
                sample_format: SampleFormat::I24,
                channels: ChannelLayout::stereo(),
            },
        },
    );
    for (number, name) in [(1, "Camera 1"), (2, "Slides"), (3, "Guest")] {
        project.add_input(Input {
            id: domain_input(number),
            name: name.to_owned(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(10, 20, 30, 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: vec![format!("simulation.source.{number}")],
        });
    }
    project.set_main_mix(MainMix::new(domain_input(1), domain_input(2)));
    let scene = SceneId::new(NonZeroU128::new(ID_BASE + 100).unwrap());
    project.add_scene(Scene {
        id: scene,
        name: "Program scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            name: "Source".into(),
            source: SourceRef::Input(domain_input(1)),
            enabled: true,
            geometry: LayerGeometry::new(0, 0, 1_280, 720, Rotation::Deg0),
            crop: None,
            mask: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
    });
    let bus = BusId::new(NonZeroU128::new(ID_BASE + 200).unwrap());
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: OutputId::new(NonZeroU128::new(ID_BASE + 300).unwrap()),
        name: "Program output".into(),
        video_source: scene,
        audio_source: bus,
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: vec!["simulation.output".into()],
    });
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(domain_input(1)),
            realized_program_id: Some(domain_input(1)),
            desired_preview_id: Some(domain_input(2)),
            realized_preview_id: Some(domain_input(2)),
        },
        ProjectPosition {
            revision: 0,
            state_epoch: 1,
            event_sequence: 0,
            frames_rendered: 0,
            runtime_generation: 0,
            clock_time_nanos: 0,
        },
        Vec::new(),
    )
    .unwrap();
    ProjectStore::new(path).unwrap().save(&stored).unwrap();
}

/// One HTTP/1.1 exchange; returns status line, lower-cased headers, and body.
fn http(address: SocketAddr, request: &str) -> (String, Vec<(String, String)>, Vec<u8>) {
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("complete response head");
    let head = std::str::from_utf8(&response[..split]).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().to_owned();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.trim().to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    (status, headers, response[split + 4..].to_vec())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map_or_else(|| panic!("missing header {name}"), |(_, value)| value)
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

/// The exact upgrade a browser sends from the served page.
fn browser_upgrade(address: SocketAddr, origin: &str, token: &str) -> Request<()> {
    Request::builder()
        .uri(format!("ws://{address}/v1/control"))
        .header("Host", address.to_string())
        .header("Origin", origin)
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
                base64url(token.as_bytes())
            ),
        )
        .body(())
        .unwrap()
}

fn upgrade_status(address: SocketAddr, origin: &str, token: &str) -> StatusCode {
    let stream = TcpStream::connect_timeout(&address, TIMEOUT).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    match tungstenite::client::client(browser_upgrade(address, origin, token), stream) {
        Err(HandshakeError::Failure(tungstenite::Error::Http(response))) => response.status(),
        other => panic!("expected a refused upgrade, got {other:?}"),
    }
}

type Browser = WebSocket<TcpStream>;

fn browser_connect(address: SocketAddr) -> Option<Browser> {
    let stream = TcpStream::connect_timeout(&address, TIMEOUT).ok()?;
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let origin = format!("http://{address}");
    let (socket, response) =
        tungstenite::client::client(browser_upgrade(address, &origin, TOKEN), stream).ok()?;
    assert_eq!(
        response.headers()["sec-websocket-protocol"],
        "freemix.control.v1",
        "the gateway selects only the control subprotocol, never the credential"
    );
    Some(socket)
}

fn send(socket: &mut Browser, message: &WireMessage) {
    socket
        .send(Message::text(encode_line(message).unwrap()))
        .unwrap();
}

fn receive(socket: &mut Browser) -> WireMessage {
    loop {
        match socket.read().unwrap() {
            Message::Text(text) => return decode_line(text.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected WebSocket message {other:?}"),
        }
    }
}

/// The next durable event, skipping the runtime realizations around it.
fn next_event(socket: &mut Browser) -> EventMessage {
    loop {
        match receive(socket) {
            WireMessage::Event(event) => return event,
            WireMessage::RuntimeEvent(_) => {}
            other => panic!("expected a durable event, got {other:?}"),
        }
    }
}

fn hello(role: Role) -> WireMessage {
    WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "freemix-tally-web".into(),
        client_type: ClientType::Web,
        desired_role: role,
        resume_cursor: None,
    })
}

/// Connects and handshakes as a Viewer tally, retrying while the gateway's
/// single Web slot is still releasing a previous session.
fn viewer_session(address: SocketAddr) -> (Browser, ServerIdentity, Vec<String>, SnapshotMessage) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "tally never reconnected");
        let Some(mut socket) = browser_connect(address) else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        send(&mut socket, &hello(Role::Viewer));
        let Ok(Message::Text(text)) = socket.read() else {
            // The scheduler refused the duplicate Web peer; back off and retry.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let WireMessage::HandshakeResponse(response) = decode_line(text.as_str()).unwrap() else {
            panic!("expected a handshake response");
        };
        assert_eq!(response.granted_role, Role::Viewer);
        assert!(matches!(
            response.outcome,
            HandshakeOutcome::Snapshot { .. }
        ));
        let WireMessage::Snapshot(snapshot) = receive(&mut socket) else {
            panic!("expected a snapshot");
        };
        return (socket, response.server, response.permissions, snapshot);
    }
}

struct RawOperator {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl RawOperator {
    fn connect(address: SocketAddr) -> Self {
        Self::connect_as(address, Role::Operator)
    }

    fn connect_as(address: SocketAddr, role: Role) -> Self {
        let stream = TcpStream::connect_timeout(&address, TIMEOUT).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut operator = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        };
        let mut request = hello(role);
        if let WireMessage::HandshakeRequest(request) = &mut request {
            request.client_type = ClientType::Studio;
        }
        operator.send(&request);
        assert!(matches!(
            operator.receive(),
            WireMessage::HandshakeResponse(response) if response.granted_role == role
        ));
        assert!(matches!(operator.receive(), WireMessage::Snapshot(_)));
        operator
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
    }

    /// Waits for the result of command `id`, skipping the session's events.
    fn accepted(&mut self, id: &str) -> u64 {
        loop {
            if let WireMessage::CommandResult(result) = self.receive() {
                match result {
                    CommandResult::Accepted {
                        id: accepted,
                        revision,
                        ..
                    } if accepted == id => return revision,
                    other => panic!("command {id} was not accepted: {other:?}"),
                }
            }
        }
    }

    fn receive(&mut self) -> WireMessage {
        loop {
            let mut line = String::new();
            assert_ne!(self.reader.read_line(&mut line).unwrap(), 0);
            let message = decode_line(&line).unwrap();
            // Studio peers also receive lossy monitor telemetry; skip it.
            if !matches!(
                message,
                WireMessage::MonitorFrame(_) | WireMessage::AudioMeters(_)
            ) {
                return message;
            }
        }
    }
}

fn command(id: &str, payload: CommandPayload) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: id.into(),
        idempotency_key: id.into(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    })
}

#[test]
fn tally_page_is_served_uncached_from_the_gateway_origin() {
    let directory = TestDirectory::new("page");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start(&project);

    let request = format!(
        "GET /tally HTTP/1.1\r\nHost: {}\r\nAccept: text/html\r\n\r\n",
        daemon.web
    );
    let (status, headers, body) = http(daemon.web, &request);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
    assert_eq!(header(&headers, "cache-control"), "no-store");
    assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
    assert_eq!(header(&headers, "referrer-policy"), "no-referrer");
    assert_eq!(header(&headers, "x-frame-options"), "DENY");
    assert_eq!(header(&headers, "connection"), "close");
    assert_eq!(header(&headers, "content-length"), body.len().to_string());
    let policy = header(&headers, "content-security-policy");
    assert!(policy.starts_with("default-src 'none';"), "{policy}");
    assert!(
        policy.contains(&format!("connect-src 'self' ws://{};", daemon.web)),
        "{policy}"
    );
    assert!(policy.contains("frame-ancestors 'none'"), "{policy}");

    let page = String::from_utf8(body).unwrap();
    assert!(page.starts_with("<!doctype html>"));
    // The browser client is pinned to the exact current protocol identifier.
    assert!(
        page.contains(&format!("const PROTOCOL = \"{CURRENT_PROTOCOL_VERSION}\";")),
        "tally.html must be updated with the protocol identifier"
    );
    // Live input add and remove keep the input board in step.
    assert!(page.contains("name === \"input_added\""));
    assert!(page.contains("name === \"input_removed\""));
    // Edit padlocks come from the snapshot and follow durable lock events.
    assert!(page.contains("required(record, \"project_locked\")"));
    assert!(page.contains("required(record, \"locked_inputs\")"));
    assert!(page.contains("name === \"edit_lock_changed\""));
    assert!(page.contains("id=\"lock\""));
    assert!(page.contains("\"freemix.control.v1\""));
    assert!(page.contains("\"freemix.bearer.\""));
    assert!(!page.contains(TOKEN), "the page never embeds the token");

    let (status, headers, body) = http(
        daemon.web,
        &format!(
            "HEAD /tally?input=1 HTTP/1.1\r\nHost: {}\r\n\r\n",
            daemon.web
        ),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(header(&headers, "cache-control"), "no-store");
    assert_eq!(body, b"");

    let (status, headers, _) = http(
        daemon.web,
        "POST /tally HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
    );
    assert_eq!(status, "HTTP/1.1 405 Method Not Allowed");
    assert_eq!(header(&headers, "allow"), "GET, HEAD");
}

#[test]
#[allow(clippy::too_many_lines)]
fn browser_tally_follows_realized_cuts_with_role_limits_and_reconnects() {
    let directory = TestDirectory::new("session");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start(&project);
    let origin = format!("http://{}", daemon.web);

    // Browser credentials: wrong token, foreign origin, missing credential.
    assert_eq!(
        upgrade_status(daemon.web, &origin, "wrong-token-0123456789abcdefghijkl"),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        upgrade_status(daemon.web, "http://evil.example", TOKEN),
        StatusCode::FORBIDDEN
    );

    let (mut tally, server, permissions, snapshot) = viewer_session(daemon.web);
    assert_eq!(permissions, ["view_status"]);
    let labels = snapshot
        .inputs
        .iter()
        .map(|status| (status.input, status.name.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        [
            (input(1), "Camera 1"),
            (input(2), "Slides"),
            (input(3), "Guest")
        ]
    );
    assert_eq!(snapshot.realized_program, input(1));
    assert_eq!(snapshot.realized_preview, input(2));

    // Role limits are enforced by the server, not only hidden by the page.
    send(
        &mut tally,
        &command("tally-viewer-cut", CommandPayload::Cut),
    );
    assert!(matches!(
        receive(&mut tally),
        WireMessage::CommandResult(CommandResult::Rejected { id, .. }) if id == "tally-viewer-cut"
    ));

    // An operator cuts on another client; the tally sees intent, then air.
    let mut operator = RawOperator::connect(daemon.control);
    operator.send(&command("operator-cut", CommandPayload::Cut));
    assert!(matches!(
        operator.receive(),
        WireMessage::CommandResult(CommandResult::Accepted { revision: 1, .. })
    ));
    let WireMessage::Event(event) = receive(&mut tally) else {
        panic!("expected the durable switcher event");
    };
    assert_eq!(event.cursor.revision, 1);
    assert!(matches!(
        event.payload,
        EventPayload::DesiredSwitcher { program, preview, .. }
            if program == input(2) && preview == input(1)
    ));
    let WireMessage::RuntimeEvent(runtime) = receive(&mut tally) else {
        panic!("expected the switcher realization");
    };
    assert_eq!(runtime.revision, 1);
    assert_eq!(runtime.server, server);
    assert!(matches!(
        runtime.event,
        RuntimeLifecycleEvent::Realized { ref domain, .. } if domain == "switcher"
    ));

    // The page's heartbeat keeps the session alive.
    send(
        &mut tally,
        &WireMessage::Heartbeat(HeartbeatMessage {
            server: server.clone(),
            sequence: 1,
            sent_at_ms: 1,
            last_applied: None,
        }),
    );
    assert!(matches!(
        receive(&mut tally),
        WireMessage::HeartbeatAcknowledgement(ack)
            if ack.heartbeat_sequence == 1 && ack.server == server
    ));

    // Drop the phone's connection; a fresh one resynchronizes the new air state.
    tally.close(None).unwrap();
    drop(tally);
    let (mut tally, _, _, snapshot) = viewer_session(daemon.web);
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.realized_program, input(2));
    assert_eq!(snapshot.realized_preview, input(1));
    assert_eq!(snapshot.edit_locks, EditLockStatus::default());

    // An admin engages the project padlock and Guest's own padlock; the tally
    // receives both durable lock events, then a reconnect snapshot carries
    // them, so the page can badge the locks without a live event history.
    drop(operator);
    let mut admin = RawOperator::connect_as(daemon.control, Role::Admin);
    for (id, scope, revision) in [
        ("lock-project", EditLockScope::Project, 2),
        ("lock-guest", EditLockScope::Input(input(3)), 3),
    ] {
        admin.send(&command(
            id,
            CommandPayload::SetEditLock {
                scope,
                locked: true,
            },
        ));
        assert_eq!(admin.accepted(id), revision);
        let event = next_event(&mut tally);
        assert_eq!(event.cursor.revision, revision);
        assert_eq!(
            event.payload,
            EventPayload::EditLockChanged {
                scope,
                locked: true
            }
        );
    }
    tally.close(None).unwrap();
    drop(tally);
    let (mut tally, _, _, snapshot) = viewer_session(daemon.web);
    assert_eq!(snapshot.revision, 3);
    assert_eq!(
        snapshot.edit_locks,
        EditLockStatus {
            project: true,
            inputs: vec![input(3)],
        }
    );
    // Releasing the project padlock leaves Guest's lock engaged.
    admin.send(&command(
        "unlock-project",
        CommandPayload::SetEditLock {
            scope: EditLockScope::Project,
            locked: false,
        },
    ));
    assert_eq!(admin.accepted("unlock-project"), 4);
    assert_eq!(
        next_event(&mut tally).payload,
        EventPayload::EditLockChanged {
            scope: EditLockScope::Project,
            locked: false
        }
    );

    // An operator-role tally is granted the shortcut permissions.
    tally.close(None).unwrap();
    drop(tally);
    let deadline = Instant::now() + Duration::from_secs(5);
    let response = loop {
        assert!(Instant::now() < deadline, "operator tally never connected");
        let Some(mut socket) = browser_connect(daemon.web) else {
            continue;
        };
        send(&mut socket, &hello(Role::Operator));
        if let Ok(Message::Text(text)) = socket.read()
            && let WireMessage::HandshakeResponse(response) = decode_line(text.as_str()).unwrap()
        {
            break response;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(response.granted_role, Role::Operator);
    for permission in ["select_preview", "transition"] {
        assert!(
            response
                .permissions
                .iter()
                .any(|granted| granted == permission)
        );
    }
}
