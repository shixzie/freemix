//! Phone tally and operator pages over the loopback web gateway: the served
//! pages, browser-style subprotocol authentication, a viewer tally that
//! follows an operator's cut, server-side role limits, edit padlocks,
//! heartbeats, reconnect, and the operator page's exact wire commands.

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
    StartupPolicy, StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId,
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
    create_project_with(path, false, false);
}

const STREAM_ID: u128 = ID_BASE + 400;

const TITLE_TEMPLATE_KEY: &str = "titles/tally-title.fmtitle";

/// Writes the stored template of a lower third with one text field.
fn write_title_template(store: &ProjectStore) {
    use fm_titles::{FieldDefinition, FieldId, FieldValue, TemplateId, TitleTemplate};

    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(1).unwrap()),
        name: "Lower third".into(),
        width: 1_280,
        height: 720,
        background: fm_titles::Color::TRANSPARENT,
        fields: vec![FieldDefinition {
            id: FieldId::new(NonZeroU128::new(1).unwrap()),
            name: "Name".into(),
            default: FieldValue::Text("Name".into()),
        }],
        elements: Vec::new(),
        animation: fm_titles::TitleAnimation::default(),
    };
    let document = fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
        template,
        fonts: Vec::new(),
        clocks: Vec::new(),
    })
    .unwrap();
    let template_path = store.assets_root().join(TITLE_TEMPLATE_KEY);
    fs::create_dir_all(template_path.parent().unwrap()).unwrap();
    fs::write(&template_path, document).unwrap();
}

/// The tally project; with `title`, title input 4 ("Lower third") too, and
/// with `stream`, one stopped stream destination on the program output.
#[allow(clippy::too_many_lines)] // one flat project fixture
fn create_project_with(path: &Path, title: bool, stream: bool) {
    let store = ProjectStore::new(path).unwrap();
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
    if title {
        write_title_template(&store);
        project.add_input(Input {
            id: domain_input(4),
            name: "Lower third".into(),
            kind: InputKind::Title(fm_model::TitleInput {
                template_uri: format!("asset://{TITLE_TEMPLATE_KEY}"),
                field_revision: 0,
                fields: vec![fm_model::TitleFieldEntry {
                    id: fm_model::TitleFieldId::new(NonZeroU128::new(1).unwrap()),
                    value: fm_model::TitleFieldValue::Text("Name".into()),
                }],
            }),
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(domain_input(1), domain_input(2)));
    let scene = SceneId::new(NonZeroU128::new(ID_BASE + 100).unwrap());
    project.add_scene(Scene {
        id: scene,
        name: "Program scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            id: fm_model::LayerId::new(1).unwrap(),
            name: "Source".into(),
            source: SourceRef::Input(domain_input(1)),
            enabled: true,
            geometry: LayerGeometry::new(0, 0, 1_280, 720, Rotation::Deg0),
            crop: None,
            mask: None,
            key: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
        animation: fm_model::SceneAnimation::default(),
    });
    let bus = BusId::new(NonZeroU128::new(ID_BASE + 200).unwrap());
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    let output = OutputId::new(NonZeroU128::new(ID_BASE + 300).unwrap());
    project.add_output(Output {
        id: output,
        name: "Program output".into(),
        video_source: scene,
        audio_source: bus,
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: vec!["simulation.output".into()],
    });
    if stream {
        let target = StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(STREAM_ID).unwrap()),
            "Main destination".to_owned(),
            StreamProtocol::Rtmp,
            StreamEndpoint::parse("127.0.0.1/live").unwrap(),
            Some(StreamKey::parse("live-key-0001").unwrap()),
            output,
        )
        .unwrap()
        .with_startup(StartupPolicy::Stopped)
        .set_running(false);
        project.add_stream_target_checked(target).unwrap();
    }
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
    store.save(&stored).unwrap();
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
    web_session(address, Role::Viewer)
}

/// Connects and handshakes as a `role` tally, retrying while the gateway's
/// single Web slot is still releasing a previous session.
fn web_session(
    address: SocketAddr,
    role: Role,
) -> (Browser, ServerIdentity, Vec<String>, SnapshotMessage) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "tally never reconnected");
        let Some(mut socket) = browser_connect(address) else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        send(&mut socket, &hello(role));
        let Ok(Message::Text(text)) = socket.read() else {
            // The scheduler refused the duplicate Web peer; back off and retry.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let WireMessage::HandshakeResponse(response) = decode_line(text.as_str()).unwrap() else {
            panic!("expected a handshake response");
        };
        assert_eq!(response.granted_role, role);
        assert!(matches!(
            response.outcome,
            HandshakeOutcome::Snapshot { .. }
        ));
        let WireMessage::Snapshot(snapshot) = receive(&mut socket) else {
            panic!("expected a snapshot");
        };
        // Every project scene's layout follows (Protocol 2.35), after the
        // state records of a title project; the page ignores these records.
        let mut layouts = snapshot.layout_scenes.len();
        while layouts > 0 {
            match receive(&mut socket) {
                WireMessage::SceneLayoutState(_) => layouts -= 1,
                WireMessage::Event(_)
                | WireMessage::RuntimeEvent(_)
                | WireMessage::CommandResult(_) => panic!("expected snapshot records"),
                _ => {}
            }
        }
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
        let WireMessage::Snapshot(snapshot) = operator.receive() else {
            panic!("expected a snapshot");
        };
        // Every project scene's layout follows (Protocol 2.35), after the
        // state records of a title project.
        let mut layouts = snapshot.layout_scenes.len();
        while layouts > 0 {
            match operator.receive() {
                WireMessage::SceneLayoutState(_) => layouts -= 1,
                WireMessage::Event(_)
                | WireMessage::RuntimeEvent(_)
                | WireMessage::CommandResult(_) => panic!("expected snapshot records"),
                _ => {}
            }
        }
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
    // RC-017: GO and Standby follow the replicated cue lists, and a cue's
    // actions move the tally like any other Program change.
    assert!(page.contains("case \"cue_list_state\":"));
    assert!(page.contains("name === \"cue_run_changed\""));
    assert!(page.contains("name === \"cue_list_changed\""));
    assert!(page.contains("name === \"cue_list_removed\""));
    // RC-011: a script run moves Program like a cue; its desired state is
    // what the realized runtime event lights.
    assert!(page.contains("name === \"script_ran\""));
    assert!(page.contains("[\"payload\", \"cue_go\"]"));
    // A GO names the revision it was pressed at, so a second press (or a
    // second tally) before the first GO is replicated cannot fire the next
    // cue too.
    assert!(page.contains(
        "[\"payload\", \"cue_go\"], [\"list\", cues.id], [\"expected_revision\", String(link.revision)]"
    ));
    // A GO refused as stale is retried once, only after the page has caught
    // up to the revision it was refused at and only while the cue on
    // standby is still the one the operator saw; the retry names that cue,
    // so it can never fire another.
    assert!(page.contains("sent && !sent.retried && code === \"revision_conflict\""));
    assert!(page.contains("link.revision < goRetry.revision"));
    assert!(page.contains("cues.id === retry.list && cues.standby === retry.cue"));
    assert!(page.contains(
        "[\"payload\", \"cue_go\"], [\"list\", retry.list], [\"cue\", retry.cue], [\"expected_revision\", String(link.revision)]"
    ));
    assert!(page.contains("[\"payload\", \"cue_standby\"]"));
    assert!(page.contains("id=\"go\""));
    assert!(page.contains("id=\"standby\""));
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

/// Waits for the result of command `id` on a web peer, skipping events.
fn web_result(socket: &mut Browser, id: &str) -> CommandResult {
    loop {
        if let WireMessage::CommandResult(result) = receive(socket) {
            let (CommandResult::Accepted { id: found, .. }
            | CommandResult::Rejected { id: found, .. }) = &result;
            if found == id {
                return result;
            }
        }
    }
}

/// RC-017 authority over the web tally's GO path: an Operator tally's GO
/// needs the permission of every action of the chain it would fire, here a
/// title text the Operator role may not edit, two cues down an auto-follow.
/// The refusal is the typed permission denial naming the missing
/// permission; nothing fires, and a chain within the role still fires.
#[test]
fn web_tally_go_needs_the_permissions_of_the_chain_it_fires() {
    use fm_protocol::{CueListSpec, CueOperation, WireCue, WireCueAction, WireTitleFieldId};

    let directory = TestDirectory::new("go-authority");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);
    let (mut tally, _, permissions, _) = web_session(daemon.web, Role::Operator);
    assert!(permissions.iter().any(|granted| granted == "transition"));
    assert!(!permissions.iter().any(|granted| granted == "edit_project"));

    let cut = |value: u128| WireCueAction::Take {
        input: Some(input(value)),
        effect: fm_protocol::TransitionFavoriteEffect::Cut,
        duration_frames: 0,
    };
    let cue = |id: u64, auto_follow: bool, actions: Vec<WireCueAction>| WireCue {
        id,
        name: format!("Cue {id}"),
        auto_follow,
        actions,
    };
    let mut admin = RawOperator::connect_as(daemon.control, Role::Admin);
    admin.send(&command(
        "set-cues",
        CommandPayload::SetCueList {
            list: 7,
            spec: Box::new(CueListSpec {
                name: "Show".into(),
                cues: vec![
                    cue(10, true, vec![cut(2)]),
                    cue(
                        20,
                        false,
                        vec![WireCueAction::TitleText {
                            input: input(4),
                            field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
                            text: "Live".into(),
                        }],
                    ),
                    cue(30, false, vec![cut(3)]),
                ],
            }),
        },
    ));
    let revision = admin.accepted("set-cues");

    // The tally's GO, exactly as the page sends it.
    let go = |cue: Option<u64>| CommandPayload::CueControl {
        list: 7,
        operation: CueOperation::Go { cue },
    };
    send(&mut tally, &command("tally-go", go(None)));
    match web_result(&mut tally, "tally-go") {
        CommandResult::Rejected {
            code,
            message,
            reason,
            current_revision,
            ..
        } => {
            assert_eq!(code, "permission_denied");
            assert_eq!(reason, Some(fm_protocol::RefusalReason::PermissionDenied));
            assert!(message.contains("EditProject"), "{message}");
            assert_eq!(current_revision, revision);
        }
        other @ CommandResult::Accepted { .. } => panic!("expected a permission denial: {other:?}"),
    }

    // A cue whose chain stays within the Operator role fires.
    send(&mut tally, &command("tally-go-30", go(Some(30))));
    assert!(matches!(
        web_result(&mut tally, "tally-go-30"),
        CommandResult::Accepted { revision: fired, .. } if fired == revision + 1
    ));

    // Full control is unchanged: Admin fires the whole chain.
    admin.send(&command("admin-go", go(Some(10))));
    assert_eq!(admin.accepted("admin-go"), revision + 2);
}

#[test]
fn operator_page_is_served_with_the_tally_security_model() {
    let directory = TestDirectory::new("operator-page");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start(&project);

    let (status, headers, body) = http(
        daemon.web,
        &format!("GET /operator HTTP/1.1\r\nHost: {}\r\n\r\n", daemon.web),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
    assert_eq!(header(&headers, "cache-control"), "no-store");
    assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
    assert_eq!(header(&headers, "x-frame-options"), "DENY");
    assert_eq!(header(&headers, "referrer-policy"), "no-referrer");
    let policy = header(&headers, "content-security-policy");
    assert!(policy.starts_with("default-src 'none';"), "{policy}");
    assert!(
        policy.contains(&format!("connect-src 'self' ws://{};", daemon.web)),
        "{policy}"
    );
    assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
    let page = String::from_utf8(body).unwrap();
    assert!(page.starts_with("<!doctype html>"));
    assert!(
        page.contains(&format!("const PROTOCOL = \"{CURRENT_PROTOCOL_VERSION}\";")),
        "operator.html must be updated with the protocol identifier"
    );
    assert!(page.contains("\"freemix.control.v1\""));
    assert!(page.contains("\"freemix.bearer.\""));
    assert!(!page.contains(TOKEN), "the page never embeds the token");
    assert!(page.contains("history.replaceState(null, \"\", location.pathname + location.search)"));

    // The exact wire commands driven end to end below.
    for payload in [
        "[\"payload\", \"select_preview\"]",
        "[\"payload\", \"cut\"]",
        "[\"payload\", choice.kind], [\"duration_frames\", String(durationFrames())]",
        "[\"payload\", \"fade_to_black\"], [\"active\", active ? \"1\" : \"0\"]",
        "on ? \"record_stop\" : \"record_start\"",
        "stream.desired ? \"stream_stop\" : \"stream_start\"",
        "[\"payload\", \"input_audio_strip\"]",
    ] {
        assert!(page.contains(payload), "{payload}");
    }
    // AUTO offers every automatic kind (freemix-web's OperatorTransition).
    for kind in ["fade", "alpha_fade", "slide", "zoom", "wipe"] {
        assert!(page.contains(&format!("[\"{kind}\", ")), "{kind}");
    }
    // A mute names the revision its strip values came from; one refused as
    // stale is retried once, after the page catches up, only while the
    // strip still holds the values it was pressed on.
    assert!(page.contains("], link.revision);"));
    assert!(page.contains("mute && !mute.retried && code === \"revision_conflict\""));
    assert!(page.contains("link.revision < retry.revision"));
    assert!(page.contains("sameStrip(show.strips.get(retry.input), retry.strip)"));
    // Disabled reasons (freemix-web's OperatorBlock::reason).
    for reason in [
        "notLive: \"Not connected to the show\"",
        "\"This session lacks the \" + name + \" permission\"",
        "alreadyOnPreview: \"Already on Preview\"",
        "notInShow: \"No longer in the show\"",
        "noAudioStrip: \"Waiting for this input's audio state\"",
    ] {
        assert!(page.contains(reason), "{reason}");
    }
    // State the page follows, including cue and script runs.
    for event in [
        "\"desired_switcher\", \"stinger_slots_changed\", \"cue_run_changed\", \"script_ran\"",
        "name === \"playlist_changed\"",
        "name === \"streams_changed\" || name === \"stream_target_changed\"",
        "name === \"recording_changed\"",
        "name === \"input_added\"",
        "name === \"input_removed\"",
        "\"audio_group_solo\"",
    ] {
        assert!(page.contains(event), "{event}");
    }
    // Accessible markup: labelled sections, live status, pressed toggles.
    for markup in [
        "aria-labelledby=\"preview-heading\"",
        "role=\"log\" aria-live=\"polite\"",
        "aria-describedby=\"why-transition\"",
        "setAttribute(\"aria-pressed\"",
        "aria-keyshortcuts=\"C\"",
    ] {
        assert!(page.contains(markup), "{markup}");
    }

    // The tally reads only the label part of `id~name~lifecycle`.
    let (_, _, tally) = http(
        daemon.web,
        &format!("GET /tally HTTP/1.1\r\nHost: {}\r\n\r\n", daemon.web),
    );
    let tally = String::from_utf8(tally).unwrap();
    assert!(tally.contains("label: decodeURIComponent(parts[1])"));

    let (status, _, body) = http(
        daemon.web,
        &format!("HEAD /operator HTTP/1.1\r\nHost: {}\r\n\r\n", daemon.web),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, b"");
}

/// Sends one command record exactly as the operator page writes it.
fn send_page_command(socket: &mut Browser, id: &str, expected: Option<u64>, payload: &str) {
    let expected = expected.map_or_else(String::new, |revision| {
        format!("\texpected_revision={revision}")
    });
    socket
        .send(Message::text(format!(
            "command\tprotocol={CURRENT_PROTOCOL_VERSION}\tid={id}\tidempotency_key={id}{expected}\t{payload}\n"
        )))
        .unwrap();
}

/// The result of command `id` and, when accepted, its durable event.
fn page_command(
    socket: &mut Browser,
    id: &str,
    expected: Option<u64>,
    payload: &str,
) -> Result<(u64, EventPayload), (String, u64)> {
    send_page_command(socket, id, expected, payload);
    let mut events = Vec::new();
    let mut accepted = None;
    loop {
        match receive(socket) {
            WireMessage::Event(event) => {
                if accepted == Some(event.cursor.revision) {
                    return Ok((event.cursor.revision, event.payload));
                }
                events.push(event);
            }
            WireMessage::CommandResult(CommandResult::Accepted {
                id: found,
                revision,
                ..
            }) if found == id => {
                if let Some(event) = events
                    .iter()
                    .find(|event| event.cursor.revision == revision)
                {
                    return Ok((revision, event.payload.clone()));
                }
                accepted = Some(revision);
            }
            WireMessage::CommandResult(CommandResult::Rejected {
                id: found,
                code,
                current_revision,
                ..
            }) if found == id => return Err((code, current_revision)),
            _ => {}
        }
    }
}

fn strip(strips: &[fm_protocol::InputAudioStripStatus], value: u128) -> bool {
    strips
        .iter()
        .find(|strip| strip.input == input(value))
        .unwrap()
        .muted
}

/// RC-014 operator page end to end: the page's exact wire records switch,
/// fade, record, stream, and mute; every role limit is the server's; a
/// stale mute is refused rather than reverting a newer strip; and a
/// reconnect snapshot carries everything the page shows.
#[test]
#[allow(clippy::too_many_lines)]
fn operator_page_commands_drive_the_show_within_the_session_role() {
    let directory = TestDirectory::new("operator");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, false, true);
    let daemon = Daemon::start(&project);
    let mute = |value: u128, muted: u8| {
        format!(
            "payload=input_audio_strip\tinput={}\tgain_millidb=0\tbalance_basis_points=0\tpan_basis_points=0\tmuted={muted}\tsoloed=0\tfollow_video=1\tdelay_samples=0",
            input(value)
        )
    };

    // A Viewer page sees the show but every command is refused by the server.
    let (mut page, _, permissions, snapshot) = web_session(daemon.web, Role::Viewer);
    assert_eq!(permissions, ["view_status"]);
    assert_eq!(snapshot.streams.len(), 1);
    assert_eq!(snapshot.streams[0].name, "Main destination");
    assert!(!snapshot.streams[0].desired_running);
    for (id, payload) in [
        ("viewer-cut", "payload=cut".to_owned()),
        (
            "viewer-preview",
            format!("payload=select_preview\tinput={}", input(3)),
        ),
        ("viewer-mute", mute(1, 1)),
    ] {
        let expected = (id == "viewer-mute").then_some(snapshot.revision);
        assert_eq!(
            page_command(&mut page, id, expected, &payload)
                .unwrap_err()
                .0,
            "permission_denied",
            "{id}"
        );
    }
    page.close(None).unwrap();
    drop(page);

    // An Audio page mutes but cannot switch.
    let (mut page, _, permissions, snapshot) = web_session(daemon.web, Role::Audio);
    assert_eq!(permissions, ["view_status", "control_audio"]);
    let (revision, event) = page_command(
        &mut page,
        "audio-mute",
        Some(snapshot.revision),
        &mute(1, 1),
    )
    .unwrap();
    let EventPayload::DesiredSwitcher {
        input_audio_strips, ..
    } = event
    else {
        panic!("a mute replicates the whole switcher state: {event:?}");
    };
    assert!(strip(&input_audio_strips, 1));
    assert!(!strip(&input_audio_strips, 2));
    assert_eq!(
        page_command(&mut page, "audio-cut", None, "payload=cut")
            .unwrap_err()
            .0,
        "permission_denied"
    );
    // A mute built from an older revision is refused, not applied.
    assert_eq!(
        page_command(&mut page, "audio-stale", Some(revision - 1), &mute(1, 0)).unwrap_err(),
        ("revision_conflict".to_owned(), revision)
    );
    page.close(None).unwrap();
    drop(page);

    // An Operator page drives the show.
    let (mut page, _, permissions, snapshot) = web_session(daemon.web, Role::Operator);
    for permission in ["select_preview", "transition", "control_audio"] {
        assert!(permissions.iter().any(|granted| granted == permission));
    }
    assert!(strip(&snapshot.input_audio_strips, 1), "the mute survived");
    let (_, event) = page_command(
        &mut page,
        "op-preview",
        None,
        &format!("payload=select_preview\tinput={}", input(3)),
    )
    .unwrap();
    assert!(matches!(
        event,
        EventPayload::DesiredSwitcher { program, preview, .. }
            if program == input(1) && preview == input(3)
    ));
    for kind in ["fade", "alpha_fade", "slide", "zoom", "wipe"] {
        let (_, event) = page_command(
            &mut page,
            &format!("op-auto-{kind}"),
            None,
            &format!("payload={kind}\tduration_frames=13"),
        )
        .unwrap_or_else(|refusal| panic!("{kind} refused: {refusal:?}"));
        assert!(
            matches!(event, EventPayload::DesiredSwitcher { .. }),
            "{kind}: {event:?}"
        );
    }
    let (_, event) = page_command(&mut page, "op-cut", None, "payload=cut").unwrap();
    assert!(matches!(event, EventPayload::DesiredSwitcher { .. }));
    let (_, event) = page_command(
        &mut page,
        "op-ftb",
        None,
        "payload=fade_to_black\tactive=1\tduration_frames=25",
    )
    .unwrap();
    assert!(matches!(
        event,
        EventPayload::DesiredSwitcher { fade_to_black, .. } if fade_to_black.target_active
    ));
    let (_, event) = page_command(
        &mut page,
        "op-rec",
        None,
        "payload=record_start\trecorder=1",
    )
    .unwrap();
    assert!(matches!(
        event,
        EventPayload::RecordingChanged { recorder, active: true } if recorder.get() == 1
    ));
    let (_, event) = page_command(
        &mut page,
        "op-stream",
        None,
        &format!("payload=stream_start\ttarget={STREAM_ID}"),
    )
    .unwrap();
    let EventPayload::StreamsChanged { streams } = event else {
        panic!("expected the stream roster: {event:?}");
    };
    assert!(streams[0].desired_running);
    // An unmute from the snapshot's strip values is stale after those
    // commands; the page retries only from the revision it has caught up to.
    let (code, current) =
        page_command(&mut page, "op-unmute", Some(snapshot.revision), &mute(1, 0)).unwrap_err();
    assert_eq!(code, "revision_conflict");
    let (revision, event) =
        page_command(&mut page, "op-unmute-retry", Some(current), &mute(1, 0)).unwrap();
    let EventPayload::DesiredSwitcher {
        input_audio_strips, ..
    } = event
    else {
        panic!("{event:?}");
    };
    assert!(!strip(&input_audio_strips, 1));
    page.close(None).unwrap();
    drop(page);

    // A reconnect snapshot carries every state the page shows.
    let (mut page, _, _, snapshot) = web_session(daemon.web, Role::Operator);
    assert_eq!(snapshot.revision, revision);
    assert!(snapshot.desired_fade_to_black.target_active);
    assert!(snapshot.recordings[0].desired_active);
    assert!(!snapshot.recordings[1].desired_active);
    assert!(snapshot.streams[0].desired_running);
    assert!(!strip(&snapshot.input_audio_strips, 1));
    // Lifting the fade and stopping the outputs are the same toggles.
    for (id, payload) in [
        (
            "op-ftb-off",
            "payload=fade_to_black\tactive=0\tduration_frames=25".to_owned(),
        ),
        ("op-rec-stop", "payload=record_stop\trecorder=1".to_owned()),
        (
            "op-stream-stop",
            format!("payload=stream_stop\ttarget={STREAM_ID}"),
        ),
    ] {
        page_command(&mut page, id, None, &payload)
            .unwrap_or_else(|refusal| panic!("{id} refused: {refusal:?}"));
    }
}
