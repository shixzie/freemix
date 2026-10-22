//! Phone tally and operator pages over the web gateway, served over TLS with
//! a generated certificate the client pins by fingerprint: the served
//! pages, browser-style subprotocol authentication, a viewer tally that
//! follows an operator's cut, server-side role limits, edit padlocks,
//! heartbeats, reconnect, the operator page's exact wire commands, several
//! Web clients at once, and the pages' English and Spanish string tables.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
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

mod web_tls;

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
    /// Starts the daemon with the gateway on TLS: a self-signed certificate
    /// generated in a web state directory next to the project, pinned by the
    /// fingerprint the daemon prints.
    fn start(project: &Path) -> Self {
        let state = project.with_extension("web-state");
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .args(["--listen", "127.0.0.1:0", "--web-listen", "127.0.0.1:0"])
            .arg("--web-tls")
            .arg("--web-state-dir")
            .arg(&state)
            .env("FREEMIXD_WEB_TOKEN", TOKEN)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut lines = [String::new(), String::new(), String::new()];
        for line in &mut lines {
            assert_ne!(stdout.read_line(line).unwrap(), 0, "daemon exited early");
        }
        let control = lines[0].parse::<ReadinessRecord>().unwrap().address;
        let web: SocketAddr = lines[1]
            .strip_prefix("FREEMIXD_WEB_READY\tv=1\taddress=")
            .and_then(|line| line.trim_end().parse().ok())
            .unwrap_or_else(|| panic!("invalid web readiness {:?}", lines[1]));
        let fingerprint = lines[2]
            .trim_end()
            .split('\t')
            .find_map(|field| field.strip_prefix("fingerprint="))
            .map_or_else(
                || panic!("invalid TLS readiness {:?}", lines[2]),
                ToOwned::to_owned,
            );
        assert!(
            lines[2].starts_with(&format!("FREEMIXD_WEB_TLS\tv=1\turl=https://{web}\t")),
            "{:?}",
            lines[2]
        );
        web_tls::pin(web, &fingerprint);
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

/// Writes the stored template of a lower third with two text fields: the
/// name, and a producer note hidden from browsers (GX-006).
fn write_title_template(store: &ProjectStore) {
    write_template(store, TITLE_TEMPLATE_KEY, false);
}

/// Writes the lower third template at `key`, its note exposed to browsers
/// or hidden from them.
fn write_template(store: &ProjectStore, key: &str, note_exposed: bool) {
    use fm_titles::{FieldDefinition, FieldId, FieldValue, TemplateId, TitleTemplate};

    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(1).unwrap()),
        name: "Lower third".into(),
        width: 1_280,
        height: 720,
        background: fm_titles::Color::TRANSPARENT,
        fields: vec![
            FieldDefinition {
                id: FieldId::new(NonZeroU128::new(1).unwrap()),
                name: "Name".into(),
                default: FieldValue::Text("Name".into()),
                exposed: true,
            },
            FieldDefinition {
                id: FieldId::new(NonZeroU128::new(2).unwrap()),
                name: "Producer note".into(),
                default: FieldValue::Text("Note".into()),
                exposed: note_exposed,
            },
        ],
        elements: Vec::new(),
        animation: fm_titles::TitleAnimation::default(),
    };
    let document = fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
        template,
        fonts: Vec::new(),
        clocks: Vec::new(),
    })
    .unwrap();
    let template_path = store.assets_root().join(key);
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
                fields: vec![
                    fm_model::TitleFieldEntry {
                        id: fm_model::TitleFieldId::new(NonZeroU128::new(1).unwrap()),
                        value: fm_model::TitleFieldValue::Text("Name".into()),
                        revision: 0,
                    },
                    fm_model::TitleFieldEntry {
                        id: fm_model::TitleFieldId::new(NonZeroU128::new(2).unwrap()),
                        value: fm_model::TitleFieldValue::Text("secret-note".into()),
                        revision: 0,
                    },
                ],
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
    web_tls::https(address, request, TIMEOUT)
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
        .uri(format!("wss://{address}/v1/control"))
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
    let stream = web_tls::connect(address, TIMEOUT).unwrap();
    match tungstenite::client::client(browser_upgrade(address, origin, token), stream) {
        Err(HandshakeError::Failure(tungstenite::Error::Http(response))) => response.status(),
        other => panic!("expected a refused upgrade, got {other:?}"),
    }
}

type Browser = WebSocket<web_tls::TlsStream>;

fn browser_connect(address: SocketAddr) -> Option<Browser> {
    let stream = web_tls::connect(address, TIMEOUT).ok()?;
    let origin = format!("https://{address}");
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

/// Connects and handshakes as a Viewer tally, retrying while every gateway
/// client slot is still taken.
fn viewer_session(address: SocketAddr) -> (Browser, ServerIdentity, Vec<String>, SnapshotMessage) {
    web_session(address, Role::Viewer)
}

/// Connects and handshakes as a `role` tally, retrying while every gateway
/// client slot is still taken.
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
            // The session closed before it was answered; back off and retry.
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
            // Studio peers also receive lossy monitor telemetry and title
            // phases; skip them.
            if !matches!(
                message,
                WireMessage::MonitorFrame(_)
                    | WireMessage::AudioMeters(_)
                    | WireMessage::TitlePhases(_)
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
        policy.contains(&format!("connect-src 'self' wss://{};", daemon.web)),
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
    // Protocol 2.65: an Operator page arms the cue it shows on standby for
    // the chosen delay, disarms it, and counts the armed GO down from the
    // state's sixth part.
    assert!(page.contains(
        "[\"payload\", \"cue_arm\"], [\"list\", cues.id], [\"cue\", cues.standby], [\"in_ms\", String(delay * 1000)]"
    ));
    assert!(page.contains("[\"payload\", \"cue_disarm\"], [\"list\", cues.id]"));
    assert!(page.contains("if (parts[5] === \"-\") return null;"));
    // Protocol 2.89: the state has a seventh part (the run's bound
    // targets), and an armed GO may name its own.
    assert!(page.contains("if (parts.length !== 7) throw new Error(\"malformed cue state\");"));
    // RC-004: a GO or arm carries this tally's input as the selected
    // input, explained in English and Spanish hover help.
    assert!(page.contains("return input ? [[\"selected\", input]] : [];"));
    assert_eq!(page.matches("...selectedField()]);").count(), 3);
    assert!(page.contains("ui.go.title = own ? tr(\"goSelectedHover\""));
    assert!(page.contains("goSelectedHover: \"GO: fire the cue on standby (G). Its actions on the selected input act on {input}, this tally's input\""));
    assert!(page.contains("goSelectedHover: \"GO: disparar el cue en espera (G). Sus acciones sobre la entrada seleccionada actúan sobre {input}, la entrada de este tally\""));
    assert!(page.contains("id=\"arm-delay\""));
    assert!(page.contains("id=\"arm\""));
    assert!(page.contains("id=\"disarm\""));
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
        &format!(
            "POST /tally HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\n\r\n",
            daemon.web
        ),
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
    let origin = format!("https://{}", daemon.web);

    // Browser credentials: wrong token, foreign origin, missing credential.
    assert_eq!(
        upgrade_status(daemon.web, &origin, "wrong-token-0123456789abcdefghijkl"),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        upgrade_status(daemon.web, "http://evil.example", TOKEN),
        StatusCode::FORBIDDEN
    );
    // A plain-HTTP origin is another origin than the TLS page's.
    assert_eq!(
        upgrade_status(daemon.web, &format!("http://{}", daemon.web), TOKEN),
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
        input: Some(fm_types::InputTarget::Fixed(input(value))),
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
                            input: fm_types::InputTarget::Fixed(input(4)),
                            field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
                            text: "Live".into(),
                        }],
                    ),
                    cue(30, false, vec![cut(3)]),
                ],
            }),
            position: None,
        },
    ));
    let revision = admin.accepted("set-cues");

    // The tally's GO, exactly as the page sends it.
    let go = |cue: Option<u64>| CommandPayload::CueControl {
        list: 7,
        operation: CueOperation::Go {
            cue,
            targets: fm_protocol::WireTargetBindings::default(),
        },
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

/// RC-017 (Protocol 2.65): the tally page arms the cue on standby for a
/// later frame and disarms it with the exact records it writes. Arming
/// needs exactly a GO's permissions (the chain's title number update needs
/// `edit_project`, which an Operator page lacks), a Viewer page may do
/// neither, and the armed GO is due the chosen delay after the command's
/// frame at the show rate.
#[test]
fn web_tally_arms_and_disarms_a_timed_go_within_its_role() {
    use fm_protocol::{CueListSpec, WireCue, WireCueAction, WireTitleFieldId};

    let directory = TestDirectory::new("arm");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);
    let cue = |id: u64, actions: Vec<WireCueAction>| WireCue {
        id,
        name: format!("Cue {id}"),
        auto_follow: false,
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
                    cue(
                        10,
                        vec![WireCueAction::Take {
                            input: Some(fm_types::InputTarget::Fixed(input(2))),
                            effect: fm_protocol::TransitionFavoriteEffect::Cut,
                            duration_frames: 0,
                        }],
                    ),
                    cue(
                        20,
                        vec![WireCueAction::TitleNumber {
                            input: fm_types::InputTarget::Fixed(input(4)),
                            field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
                            add: true,
                            value: 1,
                        }],
                    ),
                ],
            }),
            position: None,
        },
    ));
    admin.accepted("set-cues");

    let (mut viewer, _, _, _) = web_session(daemon.web, Role::Viewer);
    for (id, payload) in [
        ("viewer-arm", "payload=cue_arm\tlist=7\tcue=10\tin_ms=60000"),
        ("viewer-disarm", "payload=cue_disarm\tlist=7"),
    ] {
        assert_eq!(
            page_command(&mut viewer, id, None, payload).unwrap_err().0,
            "permission_denied",
            "{id}"
        );
    }
    viewer.close(None).unwrap();
    drop(viewer);

    let (mut page, _, permissions, _) = web_session(daemon.web, Role::Operator);
    assert!(!permissions.iter().any(|granted| granted == "edit_project"));
    // Cue 20 updates a title number: arming it needs what its GO needs.
    assert_eq!(
        page_command(
            &mut page,
            "arm-title",
            None,
            "payload=cue_arm\tlist=7\tcue=20\tin_ms=60000"
        )
        .unwrap_err()
        .0,
        "permission_denied"
    );
    let (_, event) = page_command(
        &mut page,
        "arm",
        None,
        "payload=cue_arm\tlist=7\tcue=10\tin_ms=60000",
    )
    .unwrap();
    let EventPayload::CueRunChanged(change) = event else {
        panic!("an arm replicates the run state: {event:?}");
    };
    let armed = change.state.armed.expect("cue 10 is armed");
    assert_eq!(armed.cue, 10);
    // One minute at 25 frames per second after the command's frame.
    assert_eq!(armed.due_frame, change.frame + 1_500);
    let (_, event) = page_command(&mut page, "disarm", None, "payload=cue_disarm\tlist=7").unwrap();
    let EventPayload::CueRunChanged(change) = event else {
        panic!("a disarm replicates the run state: {event:?}");
    };
    assert_eq!(change.state.armed, None);
}

#[test]
#[allow(clippy::too_many_lines)] // one page: served, secured, and every contract it names
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
        policy.contains(&format!("connect-src 'self' wss://{};", daemon.web)),
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
        "[\"payload\", \"shortcut_fire\"], [\"shortcut\", id]",
        "[\"payload\", \"audio_loudness_reset\"], [\"target\", target]",
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
        "permission: \"This session lacks the {name} permission\"",
        "alreadyOnPreview: \"Already on Preview\"",
        "notInShow: \"No longer in the show\"",
        "noAudioStrip: \"Waiting for this input's audio state\"",
        "shortcutDisabled: \"This shortcut is disabled\"",
    ] {
        assert!(page.contains(reason), "{reason}");
    }
    // Project shortcut buttons (RC-004): the shortcuts follow the snapshot
    // and their events; a button needs `transition` and the permission of
    // each cue action it runs (freemix-web's OperatorSurface::shortcut_gate).
    for reason in [
        "case \"shortcut_state\":",
        "name === \"shortcut_changed\"",
        "name === \"shortcut_removed\"",
        "\"shortcut_fired\"",
        "preview: \"select_preview\", title_text: \"edit_project\", title_number: \"edit_project\"",
        "audio_gain: \"control_audio\", audio_mute: \"control_audio\"",
        "id=\"shortcut-buttons\"",
    ] {
        assert!(page.contains(reason), "{reason}");
    }
    // RC-004: the page selects an input (its tile under "Selected input",
    // marked by an accent bar, with English and Spanish hover help) and
    // every shortcut fire carries it; a shortcut acting on the selected
    // input is held without one (freemix-web's OperatorBlock::NoSelection).
    for selection in [
        "id=\"select-bus\"",
        "[\"shortcut\", id], ...(selected ? [[\"selected\", selected]] : [])",
        "token.split(\".\").includes(\"selected\")",
        "noSelection: \"This shortcut acts on the selected input: select an input first\"",
        // RC-004 dynamic values: a gain read from the firing control is
        // held (freemix-web's OperatorBlock::NeedsValue); steps and toggles
        // need control_audio like a fixed gain.
        "token.startsWith(\"audio_gain_fired.\"))) return tr(\"needsValue\")",
        "needsValue: \"This shortcut takes its gain from the control that fires it: fire it from OSC or a fader\"",
        "needsValue: \"Este atajo toma su ganancia del control que lo dispara: dispárelo desde OSC o un fader\"",
        "audio_gain_step: \"control_audio\", audio_gain_fired: \"control_audio\", audio_mute_toggle: \"control_audio\"",
        "selectTile: \"Select {input}: shortcut actions on the selected input act on it\"",
        "selectTile: \"Seleccionar {input}: las acciones de atajo sobre la entrada seleccionada actúan sobre ella\"",
        "button.title = hover;",
        "[data-selected=\"1\"]::after",
    ] {
        assert!(page.contains(selection), "{selection}");
    }
    // A daemon that reports realized recorder state (`recorder_status`,
    // Protocol 2.66; the native daemon) is shown as such, with the running
    // time or the failure; one that does not (this simulated daemon) leaves
    // only the requested state, so the badge says so instead of claiming a
    // file is being written.
    for wording in [
        "recorderRequestedOn: \"REQUESTED: REC\"",
        "recorderRequestedOff: \"REQUESTED: OFF\"",
        "recorderNote: \"Recorders show the requested state: this daemon does not report whether a file is being written.\"",
        "tr(on ? \"recorderRequestedOn\" : \"recorderRequestedOff\")",
        "recorderRecording: \"REC {time}\"",
        "recorderFailed: \"FAILED\"",
        "recorderUnavailable: \"NOT CONFIGURED\"",
        "streamLastFailure: \"Last error: {detail}\"",
        // Failures arrive as codes (Protocol 2.67) and read as short
        // reasons on their own line; a slot without a folder cannot be
        // started and says why.
        "failureText(realized.failure, true)",
        "failureText(sample.failure, false)",
        "connect_failed: \"failConnectFailed\"",
        "failWithExitCode: \"{reason} (FFmpeg exited with code {code})\"",
        "setDisabled(button, transitionReason || shown.blocked);",
        "recorderNotConfiguredHint: \"No recording folder is set for this slot; start freemixd with {option} to use it.\"",
        "const RECORDER_OPTION = [\"--record-program\", \"--record-program-2\"];",
        "ui.recorderNote.hidden = !live || reported;",
    ] {
        assert!(page.contains(wording), "{wording}");
    }
    assert!(!page.contains("\"REC\" : \"STOPPED\""));
    // The lossy status feed (Protocol 2.66): realized recorders, stream
    // samples, and Master meters, each checked against the session's server
    // and its own sequence, repainting only their part of the page.
    for source in [
        "case \"recorder_status\":",
        "case \"stream_status\":",
        "case \"audio_meters\":",
        "freshLossy(record, telemetry.recorderSequence)",
        "required(record, \"master\")",
        "parseLoudness(required(record, \"loudness\"))",
        "id=\"loudness-reset\"",
        "resetLoudness(\"master\")",
        "if (full) render(); else renderOutputs();",
        "const TELEMETRY_STALE_MS = 2000;",
        "const METER_HOLD_MS = 1500;",
        "id=\"meters\"",
    ] {
        assert!(page.contains(source), "{source}");
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
    // Media lists (IN-006), read only: each list's status follows the
    // snapshot records, its events, and the runtime lengths (Protocol 2.91,
    // taken only while the items are the same); the page shows every
    // item's played length through its marks, the list total, the current
    // item, and the list transition (Protocol 2.95), with hover help in
    // English and Spanish.
    for source in [
        "case \"media_list_state\":",
        "name === \"media_list_changed\"",
        "required(record, \"event\") === \"media_list\"",
        "takeMediaListLengths(record)",
        "required(record, \"transition_frames\")",
        "/^(\\d+)@(\\d+)\\/(\\d+)$/",
        "const ML_TRANSITION_KEY = { fade: \"mlFade\", dip: \"mlDip\", wipe: \"mlWipe\" };",
        "mlTransitionTimed: \"{kind} between items, {frames} frames\"",
        "mlTransitionTimed: \"{kind} entre elementos, {frames} cuadros\"",
        "mlTotalPartial: \"At least {duration}\"",
        "mlTotalPartial: \"Al menos {duration}\"",
        "row.setAttribute(\"aria-current\", \"true\")",
        "id=\"media-lists-panel\" hidden aria-labelledby=\"media-lists-heading\"",
    ] {
        assert!(page.contains(source), "{source}");
    }
    // Live source sync (AU-004, Protocol 2.101 on the Web), read only:
    // each source's drift, correction, ratio, buffer, and recovery counts
    // from the lossy feed, with hover help in English and Spanish.
    for source in [
        "case \"source_sync_status\":",
        "freshLossy(record, telemetry.syncSequence)",
        "parseSourceSync(required(record, \"sources\"))",
        "parts.length !== 15",
        "screen_capture: \"syncKindScreenCapture\"",
        "id=\"sync-panel\" hidden aria-labelledby=\"sync-heading\"",
        "sourceSync: \"Source sync\"",
        "sourceSync: \"Sincronía de fuentes\"",
        "syncDriftHover: \"Estimated drift of {input}'s clock",
        "syncDriftHover: \"Deriva estimada del reloj de {input}",
        "syncFaultsHover: \"{input}: {resyncs} hard resyncs",
        "syncFaultsHover: \"{input}: {resyncs} resincronizaciones",
        "readout.title = figure.hover;",
    ] {
        assert!(page.contains(source), "{source}");
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
    // The tally shows what records and streams, and a Program level, from
    // the same lossy feed (Protocol 2.66); it only displays them.
    for source in [
        "case \"recorder_status\":",
        "case \"stream_status\":",
        "case \"audio_meters\":",
        "recRunning: \"REC {slot} {time}\"",
        "streamRunning: \"{name} LIVE {time}\"",
        "id=\"outputs\" role=\"status\" aria-live=\"off\"",
        "id=\"vu\" aria-hidden=\"true\"",
    ] {
        assert!(tally.contains(source), "{source}");
    }

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
        (
            "viewer-loudness",
            "payload=audio_loudness_reset\ttarget=master".to_owned(),
        ),
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
    // The page's loudness resets (Protocol 2.94 on the Web): Program's and
    // one strip's, live audio commands an Audio page may send.
    for (id, target, wire) in [
        (
            "audio-loudness-master",
            "master".to_owned(),
            fm_protocol::AudioDspTarget::Master,
        ),
        (
            "audio-loudness-strip",
            input(2).to_string(),
            fm_protocol::AudioDspTarget::Input(input(2)),
        ),
    ] {
        let (_, event) = page_command(
            &mut page,
            id,
            None,
            &format!("payload=audio_loudness_reset\ttarget={target}"),
        )
        .unwrap();
        assert_eq!(
            event,
            EventPayload::AudioMixerChanged(fm_protocol::AudioMixerOperation::ResetLoudness {
                target: wire
            }),
            "{id}"
        );
    }
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
        let (revision, event) = page_command(
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
        // The frame loop realizes the transition after its answer; the
        // next one is accepted once it has ended.
        realized(&mut page, revision);
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

/// Waits on `socket` for the realization of `revision`, skipping the
/// durable events and results around it.
fn realized(socket: &mut Browser, revision: u64) {
    loop {
        if let WireMessage::RuntimeEvent(runtime) = receive(socket)
            && runtime.revision == revision
            && matches!(
                runtime.event,
                RuntimeLifecycleEvent::Realized { ref domain, .. } if domain == "switcher"
            )
        {
            return;
        }
    }
}

/// The most Web clients the gateway admits at once (`MAX_WEB_CLIENTS`).
const WEB_CLIENTS: usize = 16;

/// Several Web clients at once: a Viewer tally, an Operator page, and an
/// Audio page each hold their own session with the role they asked for; a
/// client that stops reading and a page request that never completes delay
/// none of them nor the raw control session; and the gateway refuses the
/// client beyond its bound with 503 until a slot frees.
#[test]
#[allow(clippy::too_many_lines)]
fn several_web_clients_hold_their_own_sessions_at_once() {
    let directory = TestDirectory::new("several");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start(&project);

    let (mut tally, server, permissions, _) = web_session(daemon.web, Role::Viewer);
    assert_eq!(permissions, ["view_status"]);
    let (mut operator, operator_server, permissions, _) = web_session(daemon.web, Role::Operator);
    assert_eq!(operator_server, server);
    for permission in ["select_preview", "transition", "control_audio"] {
        assert!(permissions.iter().any(|granted| granted == permission));
    }
    let (mut audio, _, permissions, snapshot) = web_session(daemon.web, Role::Audio);
    assert_eq!(permissions, ["view_status", "control_audio"]);
    assert_eq!(snapshot.revision, 0);

    // Each session's role is its own: the Audio page and the tally cannot
    // cut while the Operator page can.
    assert_eq!(
        page_command(&mut audio, "audio-cut", None, "payload=cut")
            .unwrap_err()
            .0,
        "permission_denied"
    );
    send(&mut tally, &command("tally-cut", CommandPayload::Cut));
    assert!(matches!(
        receive(&mut tally),
        WireMessage::CommandResult(CommandResult::Rejected { id, .. }) if id == "tally-cut"
    ));
    let (revision, _) = page_command(&mut operator, "op-cut-1", None, "payload=cut").unwrap();
    assert_eq!(revision, 1);
    realized(&mut tally, 1);
    realized(&mut audio, 1);

    // A client that handshakes and then never reads again, and a page
    // request whose head never completes.
    let (_stalled_client, _, _, _) = web_session(daemon.web, Role::Viewer);
    let mut stalled_page = web_tls::connect(daemon.web, TIMEOUT).unwrap();
    stalled_page
        .write_all(b"GET /tally HTTP/1.1\r\nHost: web\r\n")
        .unwrap();
    let mut raw = RawOperator::connect(daemon.control);

    // Every cut reaches the other pages promptly while those two stall.
    let mut slowest = Duration::ZERO;
    for cut in 0..12_u64 {
        let started = Instant::now();
        let revision = if cut % 3 == 2 {
            // The raw control session keeps its own pace too.
            let id = format!("raw-cut-{cut}");
            raw.send(&command(&id, CommandPayload::Cut));
            raw.accepted(&id)
        } else {
            page_command(
                &mut operator,
                &format!("op-cut-{cut}-x"),
                None,
                "payload=cut",
            )
            .unwrap()
            .0
        };
        assert_eq!(revision, cut + 2);
        realized(&mut tally, revision);
        realized(&mut audio, revision);
        slowest = slowest.max(started.elapsed());
    }
    assert!(
        slowest < Duration::from_secs(1),
        "a stalled client delayed the others by {slowest:?}"
    );
    drop(stalled_page);

    // The Audio page mutes; the Operator page sees the strip change.
    let mute = format!(
        "payload=input_audio_strip\tinput={}\tgain_millidb=0\tbalance_basis_points=0\tpan_basis_points=0\tmuted=1\tsoloed=0\tfollow_video=1\tdelay_samples=0",
        input(1)
    );
    let (revision, _) = page_command(&mut audio, "audio-mute", Some(13), &mute).unwrap();
    assert_eq!(revision, 14);
    let event = loop {
        let event = next_event(&mut operator);
        if event.cursor.revision == 14 {
            break event;
        }
    };
    let EventPayload::DesiredSwitcher {
        input_audio_strips, ..
    } = event.payload
    else {
        panic!("a mute replicates the whole switcher state");
    };
    assert!(strip(&input_audio_strips, 1));

    // Fill every client slot: the next authorized browser is refused with
    // 503 (and the page retries with backoff) until one closes.
    let mut viewers = Vec::new();
    while viewers.len() < WEB_CLIENTS - 4 {
        viewers.push(web_session(daemon.web, Role::Viewer).0);
    }
    let origin = format!("https://{}", daemon.web);
    assert_eq!(
        upgrade_status(daemon.web, &origin, TOKEN),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // Credentials are still checked first: a wrong token is never told
    // about capacity.
    assert_eq!(
        upgrade_status(daemon.web, &origin, "wrong-token-0123456789abcdefghijkl"),
        StatusCode::UNAUTHORIZED
    );
    // Pages are still served while every client slot is taken.
    let (status, _, _) = http(
        daemon.web,
        &format!("GET /operator HTTP/1.1\r\nHost: {}\r\n\r\n", daemon.web),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    let mut closed = viewers.pop().unwrap();
    closed.close(None).unwrap();
    drop(closed);
    let (mut late, _, permissions, snapshot) = web_session(daemon.web, Role::Viewer);
    assert_eq!(permissions, ["view_status"]);
    assert_eq!(snapshot.revision, 14);

    // Every admitted session is still served.
    let (revision, _) = page_command(&mut operator, "op-cut-last", None, "payload=cut").unwrap();
    for socket in std::iter::once(&mut late)
        .chain(std::iter::once(&mut tally))
        .chain(&mut viewers)
    {
        realized(socket, revision);
    }
}

/// The `STRINGS` table of a page: each language's keys and texts.
fn strings_table(page: &str, language: &str) -> Vec<(String, String)> {
    let header = format!("\n    {language}: {{\n");
    let start = page
        .find(&header)
        .unwrap_or_else(|| panic!("no {language} table"));
    let body = &page[start + header.len()..];
    let end = body.find("\n    },").expect("table end");
    body[..end]
        .lines()
        .map(|line| {
            let (key, text) = line.trim().split_once(": ").expect("key: text");
            (key.to_owned(), text.trim_end_matches(',').to_owned())
        })
        .collect()
}

/// Both pages keep every user-visible string in one table per language:
/// English and Spanish define the same keys, every key the page uses is in
/// the table, the language follows the browser with a saved manual toggle,
/// and the Spanish words are Studio's.
#[test]
#[allow(clippy::too_many_lines)]
fn pages_carry_complete_english_and_spanish_string_tables() {
    let directory = TestDirectory::new("i18n");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start(&project);
    let studio = include_str!("../../../crates/ui/fm-ui-model/src/i18n/es.rs");

    // (page text, the Studio catalog text it follows)
    let tally: &[(&str, &str)] = &[
        ("preview: \"Previo\"", "\"Previo\""),
        ("cut: \"Corte\"", "t(\"Corte\")"),
        ("standby: \"En espera\"", "\"En espera: {name}\""),
        ("arm: \"Armar GO\"", "t(\"Armar GO\")"),
        ("disarm: \"Desarmar\"", "t(\"Desarmar\")"),
        (
            "delaySeconds: \"Disparar en {count} s\"",
            "t(\"Disparar en\")",
        ),
        (
            "cueNothing: \"{list} — Nada en espera\"",
            "t(\"Nada en espera\")",
        ),
        ("stateProgram: \"AL AIRE\"", "\"AL AIRE\""),
        ("statePreview: \"PREVIO\"", "t(\"PREVIO\")"),
        ("stateOffline: \"DESCONECTADO\"", "t(\"DESCONECTADO\")"),
        (
            "cutLabel: \"Cortar Previo a Programa (C)\"",
            "t(\"Cortar Previo a Programa\")",
        ),
        (
            "statusReconnecting: \"Reconectando (intento {attempt})\"",
            "t(\"Reconectando\")",
        ),
    ];
    let operator: &[(&str, &str)] = &[
        ("program: \"Programa\"", "t(\"Programa\")"),
        ("preview: \"Previo\"", "\"Previo\""),
        ("cut: \"CORTE\"", "t(\"CORTE\")"),
        ("fadeToBlack: \"Fundido a negro\"", "t(\"Fundido a negro\")"),
        ("transitionFade: \"Fundido\"", "t(\"Fundido\")"),
        ("transitionWipe: \"Cortina\"", "t(\"Cortina\")"),
        (
            "transitionAlphaFade: \"Fundido alfa\"",
            "t(\"Fundido alfa\")",
        ),
        ("transitionSlide: \"Deslizar\"", "t(\"Deslizar\")"),
        (
            "ftbFading: \"FUNDIENDO A NEGRO\"",
            "\"FUNDIENDO A NEGRO {percent}\"",
        ),
        (
            "startRecorder: \"Iniciar grabador {slot}\"",
            "t(\"Iniciar grabador {slot}\")",
        ),
        (
            "stopRecorder: \"Detener grabador {slot}\"",
            "t(\"Detener grabador {slot}\")",
        ),
        (
            "streamReconnecting: \"Reconectando\"",
            "t(\"RECONECTANDO\")",
        ),
        (
            "muteLabel: \"Silenciar {input}\"",
            "t(\"Silenciar esta entrada\")",
        ),
        (
            "permission: \"Esta sesión no tiene el permiso {name}\"",
            "esta sesión no tiene el permiso transition",
        ),
        (
            "noStreams: \"No hay destinos de transmisión configurados.\"",
            "t(\"Sin destinos de transmisión\")",
        ),
        ("recorderRequestedOn: \"PEDIDO: REC\"", "PEDIDO"),
        (
            "meterStale: \"MEDIDORES OBSOLETOS\"",
            "t(\"MEDIDORES OBSOLETOS\")",
        ),
        (
            "meterLegend: \"RMS · PICO · RETENCIÓN · CLIP\"",
            "\"RMS · RETENCIÓN · CLIP\"",
        ),
        ("recorderFailed: \"FALLIDO\"", "t(\"FALLIDO\")"),
        (
            "streamLastFailure: \"Último error: {detail}\"",
            "t(\"Último error: {failure}\")",
        ),
    ];
    let titles: &[(&str, &str)] = &[
        ("titles: \"Títulos\"", "t(\"Títulos\")"),
        ("take: \"Tomar\"", "t(\"Tomar\")"),
        (
            "update: \"Actualizar en vivo\"",
            "t(\"Actualizar en vivo\")",
        ),
        ("offAir: \"Retirar del aire\"", "t(\"Retirar del aire\")"),
        ("save: \"Guardar\"", "t(\"Guardar\")"),
        ("channel: \"Canal\"", "t(\"Canal\")"),
        ("editedBadge: \"EDITADO\"", "t(\"EDITADO\")"),
        ("offBadge: \"FUERA DEL AIRE\"", "t(\"FUERA DEL AIRE\")"),
        ("previewBadge: \"PREVIO\"", "t(\"PREVIO\")"),
        (
            "permission: \"Esta sesión no tiene el permiso {name}\"",
            "esta sesión no tiene el permiso transition",
        ),
    ];
    for (path, spanish) in [
        ("/tally", tally),
        ("/operator", operator),
        ("/titles", titles),
    ] {
        let (_, _, body) = http(
            daemon.web,
            &format!("GET {path} HTTP/1.1\r\nHost: {}\r\n\r\n", daemon.web),
        );
        let page = String::from_utf8(body).unwrap();
        let english = strings_table(&page, "en");
        let spanish_table = strings_table(&page, "es");
        let keys = |table: &[(String, String)]| {
            table.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            keys(&english),
            keys(&spanish_table),
            "{path}: English and Spanish define the same keys in the same order"
        );
        // Every placeholder of an English string appears in its Spanish one.
        for ((key, en), (_, es)) in english.iter().zip(&spanish_table) {
            for (placeholder, _) in en
                .split('{')
                .skip(1)
                .filter_map(|part| part.split_once('}'))
            {
                assert!(
                    es.contains(&format!("{{{placeholder}}}")),
                    "{path}: {key} lacks {{{placeholder}}} in Spanish"
                );
            }
        }
        // Every key the markup or the script names is in the table.
        let known = keys(&english);
        for marker in ["data-i18n=\"", "data-i18n-label=\"", "tr(\""] {
            for used in page.split(marker).skip(1) {
                let key = &used[..used.find('"').unwrap()];
                assert!(
                    known.iter().any(|known| known == key),
                    "{path}: {marker}{key}"
                );
            }
        }
        // The language follows a `#lang=`, the saved toggle, then the browser.
        for source in [
            "params.get(\"lang\") || savedLanguage()",
            "navigator.languages",
            "const LANGUAGE_KEY = \"freemix.web.language\";",
            "document.documentElement.lang = language;",
            "id=\"language\" lang=\"es\"",
            "addEventListener(\"click\", toggleLanguage)",
        ] {
            assert!(page.contains(source), "{path}: {source}");
        }
        for (text, catalog) in spanish {
            assert!(page.contains(text), "{path}: {text}");
            assert!(
                studio.contains(catalog),
                "Studio's es catalog lacks {catalog}"
            );
        }
    }
}

/// Both pages name every output failure code (Protocol 2.67) and word each
/// one exactly as Studio does, in English and Spanish: the same reason
/// reads the same on every client, and no page shows a raw code.
#[test]
fn pages_word_every_output_failure_like_studio() {
    // rustfmt wraps long catalog entries, so compare with all whitespace
    // removed on both sides.
    let squash = |text: &str| text.split_whitespace().collect::<String>();
    let english_catalog = squash(include_str!(
        "../../../crates/ui/fm-ui-model/src/i18n/messages.rs"
    ));
    let spanish_catalog = squash(include_str!(
        "../../../crates/ui/fm-ui-model/src/i18n/es.rs"
    ));
    for (name, page) in [
        ("operator", include_str!("../src/web/operator.html")),
        ("tally", include_str!("../src/web/tally.html")),
    ] {
        for code in fm_protocol::OutputFailureCode::ALL {
            assert!(
                page.contains(&format!("    {}: ", code.token()))
                    || page.contains(&format!(" {}: ", code.token())),
                "{name}: FAILURE_KEY lacks {}",
                code.token()
            );
        }
        let english = strings_table(page, "en");
        let spanish = strings_table(page, "es");
        let mut reasons = 0;
        for ((key, en), (_, es)) in english.iter().zip(&spanish) {
            let Some(rest) = key.strip_prefix("fail") else {
                continue;
            };
            reasons += 1;
            let msg = format!("Fail{rest}");
            assert!(
                english_catalog.contains(&squash(&format!("{msg} => t({en})"))),
                "{name}: {key} differs from Studio's English"
            );
            assert!(
                spanish_catalog.contains(&squash(&format!("(Msg::{msg}, t({es})"))),
                "{name}: {key} differs from Studio's Spanish"
            );
        }
        // One reason per code (two folder codes have a recorder and a
        // stream wording) plus the exit-code template.
        assert_eq!(
            reasons,
            fm_protocol::OutputFailureCode::ALL.len() + 3,
            "{name}"
        );
    }
}

/// Every served page's inline script parses: `node --check` on each when
/// Node.js is on the `PATH` (skipped otherwise; the pages ship no build
/// step, so this is their only syntax gate besides a browser).
#[test]
fn page_scripts_parse_with_node_when_available() {
    if ProcessCommand::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("node unavailable: page script syntax check skipped");
        return;
    }
    let directory = TestDirectory::new("node-check");
    for (name, page) in [
        ("tally", include_str!("../src/web/tally.html")),
        ("operator", include_str!("../src/web/operator.html")),
        ("titles", include_str!("../src/web/titles.html")),
        ("live_lan", include_str!("../src/web/live_lan.html")),
    ] {
        let scripts: Vec<&str> = page
            .split("<script>")
            .skip(1)
            .map(|rest| rest.split_once("</script>").expect("closed script").0)
            .collect();
        assert!(!scripts.is_empty(), "{name} has no inline script");
        let path = directory.0.join(format!("{name}.js"));
        fs::write(&path, scripts.join("\n")).unwrap();
        let output = ProcessCommand::new("node")
            .arg("--check")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// The operator page's strip DSP chain codec block (AU-005), which uses
/// nothing else on the page.
fn page_strip_dsp_codec(page: &str) -> &str {
    let begin = "  // ---- Strip DSP chain codec (AU-005) begin ----\n";
    let start = page.find(begin).expect("codec block begin");
    let end = page[start..]
        .find("  // ---- Strip DSP chain codec (AU-005) end ----")
        .expect("codec block end");
    &page[start..start + end]
}

fn base64url_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = Vec::new();
    for chunk in text.as_bytes().chunks(4) {
        let mut value = 0_u32;
        for (index, byte) in chunk.iter().enumerate() {
            let sextet = ALPHABET
                .iter()
                .position(|candidate| candidate == byte)
                .unwrap();
            value |= u32::try_from(sextet).unwrap() << (18 - 6 * index);
        }
        for index in 0..chunk.len() - 1 {
            bytes.push(u8::try_from((value >> (16 - 8 * index)) & 0xff).unwrap());
        }
    }
    bytes
}

/// A chain as the page's JavaScript object literal (field names as in Rust).
fn chain_literal(chain: &fm_types::strip_dsp::StripDsp) -> String {
    let bands: Vec<String> = chain
        .eq
        .bands
        .iter()
        .map(|band| {
            format!(
                "{{enabled:{},kind:\"{}\",frequency_hz:{},gain_millidb:{},q_milli:{}}}",
                band.enabled,
                band.kind.token(),
                band.frequency_hz,
                band.gain_millidb,
                band.q_milli
            )
        })
        .collect();
    let (gate, expander, compressor, limiter) = (
        &chain.gate,
        &chain.expander,
        &chain.compressor,
        &chain.limiter,
    );
    format!(
        "{{bypass:{},delay:{{enabled:{},delay_us:{}}},invert_polarity:{},high_pass:{{enabled:{},frequency_hz:{}}},eq:{{enabled:{},bands:[{}]}},gate:{{enabled:{},threshold_millidb:{},range_millidb:{},attack_us:{},hold_us:{},release_us:{}}},expander:{{enabled:{},threshold_millidb:{},ratio_milli:{},attack_us:{},release_us:{},range_millidb:{}}},compressor:{{enabled:{},threshold_millidb:{},ratio_milli:{},attack_us:{},release_us:{},knee_millidb:{},makeup_millidb:{}}},limiter:{{enabled:{},ceiling_millidb:{},release_us:{}}}}}",
        chain.bypass,
        chain.delay.enabled,
        chain.delay.delay_us,
        chain.invert_polarity,
        chain.high_pass.enabled,
        chain.high_pass.frequency_hz,
        chain.eq.enabled,
        bands.join(","),
        gate.enabled,
        gate.threshold_millidb,
        gate.range_millidb,
        gate.attack_us,
        gate.hold_us,
        gate.release_us,
        expander.enabled,
        expander.threshold_millidb,
        expander.ratio_milli,
        expander.attack_us,
        expander.release_us,
        expander.range_millidb,
        compressor.enabled,
        compressor.threshold_millidb,
        compressor.ratio_milli,
        compressor.attack_us,
        compressor.release_us,
        compressor.knee_millidb,
        compressor.makeup_millidb,
        limiter.enabled,
        limiter.ceiling_millidb,
        limiter.release_us,
    )
}

/// Chains whose every field differs from the default somewhere: extremes,
/// every flag bit, every band kind, and the delay.
fn strip_dsp_vectors() -> Vec<fm_types::strip_dsp::StripDsp> {
    use fm_types::strip_dsp::{
        EqBandKind, MAX_DELAY_US, StripDelay, StripDsp, StripDspPreset, StripExpander,
    };
    let mut chains: Vec<StripDsp> = StripDspPreset::ALL
        .into_iter()
        .map(StripDspPreset::chain)
        .collect();
    let mut extremes = StripDspPreset::Podcast.chain();
    extremes.bypass = true;
    extremes.invert_polarity = true;
    extremes.delay = StripDelay {
        enabled: true,
        delay_us: MAX_DELAY_US,
    };
    extremes.high_pass.frequency_hz = 2_000;
    extremes.eq.bands[0].gain_millidb = -24_000;
    extremes.eq.bands[0].q_milli = 100;
    extremes.eq.bands[1].kind = EqBandKind::HighShelf;
    extremes.eq.bands[1].frequency_hz = 20_000;
    extremes.eq.bands[1].enabled = false;
    extremes.eq.bands[2].kind = EqBandKind::LowShelf;
    extremes.eq.bands[2].frequency_hz = 20;
    extremes.eq.bands[2].gain_millidb = 24_000;
    extremes.eq.bands[2].q_milli = 10_000;
    extremes.eq.bands[3].enabled = false;
    extremes.gate.threshold_millidb = -80_000;
    extremes.gate.range_millidb = 80_000;
    extremes.gate.attack_us = 100;
    extremes.gate.hold_us = 0;
    extremes.gate.release_us = 5_000_000;
    extremes.expander = StripExpander {
        enabled: true,
        threshold_millidb: 0,
        ratio_milli: 20_000,
        attack_us: 1_000_000,
        release_us: 100,
        range_millidb: 80_000,
    };
    extremes.compressor.threshold_millidb = -60_000;
    extremes.compressor.ratio_milli = 20_000;
    extremes.compressor.knee_millidb = 24_000;
    extremes.compressor.makeup_millidb = 24_000;
    extremes.limiter.ceiling_millidb = -60_000;
    extremes.limiter.release_us = 1_000_000;
    chains.push(extremes);
    let mut odd = StripDsp::DEFAULT;
    odd.delay = StripDelay {
        enabled: false,
        delay_us: 41_667,
    };
    odd.eq.enabled = true;
    odd.eq.bands[4].gain_millidb = -1;
    odd.gate.threshold_millidb = -1;
    odd.compressor.threshold_millidb = -59_999;
    odd.limiter.enabled = true;
    odd.limiter.ceiling_millidb = 0;
    chains.push(odd);
    chains
}

/// AU-005 Web chain editing: the operator page's JavaScript chain codec is
/// the Rust codec. Under Node.js (skipped when it is not on the `PATH`),
/// the page's codec block encodes chains the Rust codec rendered to the
/// same codes, decodes those codes back to the same chains, builds each
/// preset's exact chain, and refuses every code the Rust codec refuses.
#[test]
fn operator_page_strip_dsp_codec_matches_the_rust_codec() {
    use std::fmt::Write as _;

    use fm_types::strip_dsp::{STRIP_DSP_CODE_LEN, StripDsp, StripDspPreset};
    let page = include_str!("../src/web/operator.html");
    let codec = page_strip_dsp_codec(page);
    // The block stands alone: it names no other part of the page.
    for outside in ["tr(", "show.", "link.", "ui.", "document", "send("] {
        assert!(!codec.contains(outside), "the codec block uses {outside}");
    }

    let vectors = strip_dsp_vectors();
    let default_code = StripDsp::DEFAULT.code();
    assert_eq!(default_code.len(), STRIP_DSP_CODE_LEN);
    // Codes the Rust codec refuses: the wrong length, a character outside
    // base64url, an unknown flag bit, non-canonical trailing bits, an
    // unknown band kind, and values out of bounds.
    let mut invalid = vec![
        default_code[1..].to_owned(),
        format!("{default_code}A"),
        format!("+{}", &default_code[1..]),
        format!("_{}", &default_code[1..]),
        format!("{}B", &default_code[..STRIP_DSP_CODE_LEN - 1]),
    ];
    let packed = base64url_decode(&default_code);
    assert_eq!(packed.len(), 107);
    for (offset, bytes) in [
        // High-pass at 10 Hz.
        (2, vec![0x00, 0x0a]),
        // Band 1 of kind 3.
        (4, vec![0x03]),
        // Band 1 gain +24.001 dB.
        (7, vec![0x5d, 0xc1]),
        // Gate threshold +1 mdB.
        (39, vec![0x00, 0x00, 0x00, 0x01]),
        // Delay one microsecond past a second.
        (103, vec![0x00, 0x0f, 0x42, 0x41]),
    ] {
        let mut changed = packed.clone();
        changed[offset..offset + bytes.len()].copy_from_slice(&bytes);
        invalid.push(base64url(&changed));
    }
    for code in &invalid {
        assert!(StripDsp::from_code(code).is_err(), "Rust accepts {code}");
    }

    let mut script = String::from(codec);
    script.push_str("\nconst failures = [];\n");
    script.push_str(
        "const check = (ok, what) => { if (!ok) failures.push(what); };\n\
         const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);\n",
    );
    let _ = writeln!(
        script,
        "check(STRIP_DSP.encode(STRIP_DSP.defaults()) === \"{default_code}\", \"default\");"
    );
    for (index, chain) in vectors.iter().enumerate() {
        let code = chain.code();
        let literal = chain_literal(chain);
        let _ = writeln!(
            script,
            "check(STRIP_DSP.encode({literal}) === \"{code}\", \"encode {index}\");\n\
             check(same(STRIP_DSP.decode(\"{code}\"), {literal}), \"decode {index}\");"
        );
    }
    for preset in StripDspPreset::ALL {
        let _ = writeln!(
            script,
            "check(STRIP_DSP.encode(STRIP_DSP.preset(\"{}\")) === \"{}\", \"preset {}\");",
            preset.token(),
            preset.chain().code(),
            preset.token()
        );
    }
    let _ = writeln!(
        script,
        "check(same(STRIP_DSP.PRESETS, {:?}), \"preset tokens\");",
        StripDspPreset::ALL.map(StripDspPreset::token)
    );
    for (index, code) in invalid.iter().enumerate() {
        let _ = writeln!(
            script,
            "check(STRIP_DSP.decode(\"{code}\") === null, \"refuse {index}\");"
        );
    }
    // An out-of-range chain object is refused before it is encoded.
    script.push_str(
        "const wide = STRIP_DSP.defaults(); wide.compressor.ratio_milli = 999;\n\
         check(STRIP_DSP.encode(wide) === null && STRIP_DSP.validate(wide) === \"compressor.ratio_milli\", \"bounds\");\n\
         const half = STRIP_DSP.defaults(); half.eq.bands[0].gain_millidb = 0.5;\n\
         check(STRIP_DSP.encode(half) === null, \"integers\");\n\
         if (failures.length) { console.error(failures.join(\"\\n\")); process.exit(1); }\n\
         console.log(\"ok\");\n",
    );

    if ProcessCommand::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("node unavailable: the page's strip DSP codec check skipped");
        return;
    }
    let directory = TestDirectory::new("strip-dsp-codec");
    let path = directory.0.join("codec.js");
    fs::write(&path, script).unwrap();
    let output = ProcessCommand::new("node").arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

/// The operator page's Strip FX gain reduction block (AU-005, Protocol
/// 2.103), which uses nothing else on the page.
fn page_fx_gain_reduction(page: &str) -> &str {
    let begin = "  // ---- Strip FX gain reduction (AU-005) begin ----\n";
    let start = page.find(begin).expect("gain reduction block begin");
    let end = page[start..]
        .find("  // ---- Strip FX gain reduction (AU-005) end ----")
        .expect("gain reduction block end");
    &page[start..start + end]
}

/// The Web `audio_meters` `dynamics` (Protocol 2.103) on the operator
/// page: each FX disclosure shows its switched-on dynamics stages' gain
/// reduction as a bar with the value in dB and hover help, dimmed while the
/// meters are stale; the page's parser reads exactly what the Rust codec
/// renders and refuses malformed entries (checked under Node.js when it is
/// on the `PATH`).
#[test]
#[allow(clippy::too_many_lines)]
fn operator_page_shows_strip_fx_gain_reduction_from_the_meter_feed() {
    use std::fmt::Write as _;

    use fm_protocol::{AudioDspTarget, AudioDynamicsMeter, AudioMetersMessage, ServerIdentity};
    let page = include_str!("../src/web/operator.html");
    for source in [
        "telemetry.dynamics = FX_GR.parse(required(record, \"dynamics\"));",
        "telemetry.dynamics = new Map();",
        "    paintLoudness();\n    paintFxDynamics();\n",
        "function paintFxDynamics() { for (const panel of fxPanels.values()) panel.paintGr(); }",
        "const stale = !live || !telemetry.metersAt || Date.now() - telemetry.metersAt > METER_STALE_MS;\n      const reading = live ? telemetry.dynamics.get(target) : undefined;",
        "gr.element.hidden = chain.bypass || !fxGet(chain, gr.toggle);",
        "gr.element.dataset.stale = view.dim ? \"1\" : \"0\";",
        ".fx .gr[data-stale=\"1\"] .fill, .fx .gr[data-stale=\"1\"] output { opacity: .35; }",
        "data-i18n=\"fxGainReductionNote\"",
    ] {
        assert!(page.contains(source), "{source}");
    }
    for language in ["en", "es"] {
        let table = strings_table(page, language);
        for key in [
            "fxGainReductionNote",
            "fxGrLegend",
            "fxGrNone",
            "fxGrHover",
            "fxGrIdleHover",
            "fxGrStaleHover",
        ] {
            assert!(
                table.iter().any(|(name, _)| name == key),
                "{language} {key}"
            );
        }
    }
    assert!(!page.contains("fxNoGainReduction"));
    let block = page_fx_gain_reduction(page);
    for outside in [
        "tr(",
        "show.",
        "link.",
        "ui.",
        "telemetry.",
        "document",
        "send(",
    ] {
        assert!(
            !block.contains(outside),
            "the gain reduction block uses {outside}"
        );
    }

    // A record as a native daemon's Web feed renders it.
    let meter = |target, gate, expander, compressor, limiter| AudioDynamicsMeter {
        target,
        gate_millidb: gate,
        expander_millidb: expander,
        compressor_millidb: compressor,
        limiter_millidb: limiter,
    };
    let record = AudioMetersMessage {
        server: ServerIdentity {
            engine_id: "engine".to_owned(),
            project_id: "project".to_owned(),
            state_epoch: 1,
            log_id: "log".to_owned(),
        },
        sequence: 3,
        frame: 4,
        start_sample: 0,
        end_sample: 480,
        master: vec![fm_protocol::AudioMeterChannel {
            peak_millionths: 1,
            rms_millionths: 1,
        }],
        inputs: Vec::new(),
        monitor: None,
        dynamics: vec![
            meter(AudioDspTarget::Master, None, None, Some(4_250), Some(0)),
            meter(
                AudioDspTarget::Input(input(1)),
                Some(30_000),
                Some(1),
                None,
                None,
            ),
            meter(
                AudioDspTarget::Input(input(2)),
                None,
                None,
                Some(200_000),
                None,
            ),
        ],
        loudness: Vec::new(),
    };
    let line = fm_protocol::encode_line(&WireMessage::AudioMeters(record)).unwrap();
    // The field's value as the page's record decoder unescapes it.
    let escaped = line
        .trim_end()
        .split('\t')
        .find_map(|field| field.strip_prefix("dynamics="))
        .expect("dynamics field");
    let mut dynamics = String::new();
    let mut rest = escaped;
    while let Some((before, after)) = rest.split_once('%') {
        dynamics.push_str(before);
        dynamics.push(char::from(u8::from_str_radix(&after[..2], 16).unwrap()));
        rest = &after[2..];
    }
    dynamics.push_str(rest);

    let mut script = String::from(block);
    script.push_str(
        "\nconst failures = [];\n\
         const check = (ok, what) => { if (!ok) failures.push(what); };\n\
         const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);\n",
    );
    let _ = writeln!(
        script,
        "const read = FX_GR.parse(\"{dynamics}\");\n\
         check(same([...read.keys()], [\"master\", \"{one}\", \"{two}\"]), \"targets\");\n\
         check(same(read.get(\"master\"), {{ gate: null, expander: null, compressor: 4.25, limiter: 0 }}), \"master\");\n\
         check(same(read.get(\"{one}\"), {{ gate: 30, expander: 0.001, compressor: null, limiter: null }}), \"strip\");\n\
         check(same(FX_GR.view(read.get(\"master\"), \"compressor\", false), {{ db: 4.25, fraction: 4.25 / 24, dim: false }}), \"bar\");\n\
         check(same(FX_GR.view(read.get(\"master\"), \"limiter\", false), {{ db: 0, fraction: 0, dim: false }}), \"zero\");\n\
         check(same(FX_GR.view(read.get(\"{one}\"), \"gate\", false), {{ db: 30, fraction: 1, dim: false }}), \"full\");\n\
         check(same(FX_GR.view(read.get(\"{two}\"), \"compressor\", true), {{ db: 200, fraction: 1, dim: true }}), \"stale\");\n\
         check(same(FX_GR.view(read.get(\"master\"), \"gate\", false), {{ db: null, fraction: 0, dim: false }}), \"off\");\n\
         check(same(FX_GR.view(undefined, \"gate\", true), {{ db: null, fraction: 0, dim: true }}), \"absent\");\n\
         check(FX_GR.parse(\"\").size === 0, \"empty\");\n\
         check(same(FX_GR.STAGES, [\"gate\", \"expander\", \"compressor\", \"limiter\"]), \"stages\");",
        one = input(1),
        two = input(2),
    );
    for (index, malformed) in [
        "master~1~2~3",
        "master~1~2~3~4~5",
        "~1~-~-~-",
        "master~01~-~-~-",
        "master~200001~-~-~-",
        "master~-1~-~-~-",
        "master~1.5~-~-~-",
        "master~~-~-~-",
    ]
    .iter()
    .enumerate()
    {
        let _ = writeln!(
            script,
            "try {{ FX_GR.parse(\"{malformed}\"); failures.push(\"accepted {index}\"); }} catch (error) {{}}"
        );
    }
    script.push_str(
        "if (failures.length) { console.error(failures.join(\"\\n\")); process.exit(1); }\n\
         console.log(\"ok\");\n",
    );

    if ProcessCommand::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("node unavailable: the page's gain reduction check skipped");
        return;
    }
    let directory = TestDirectory::new("fx-gain-reduction");
    let path = directory.0.join("gain_reduction.js");
    fs::write(&path, script).unwrap();
    let output = ProcessCommand::new("node").arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

/// AU-005 Web chain editing end to end: the operator page's exact
/// `audio_strip_dsp` records set a strip's and Master's chains within the
/// session's role, a drag's sets share one gesture (one undo step), a set
/// based on an older revision is refused rather than overwriting a newer
/// chain, and a reconnecting page learns every chain from its snapshot.
#[test]
#[allow(clippy::too_many_lines)]
fn operator_page_edits_strip_fx_within_the_session_role() {
    use std::fmt::Write as _;

    use fm_protocol::{AudioDspTarget, AudioMixerOperation};
    use fm_types::strip_dsp::{StripDsp, StripDspPreset};
    let page = include_str!("../src/web/operator.html");
    for source in [
        "[\"payload\", \"audio_strip_dsp\"], [\"target\", target], [\"chain\", queued.code]",
        "[\"gesture\", queued.gesture.token], [\"continues\", queued.gesture.sent ? \"true\" : \"false\"]",
        "[\"expected_revision\", String(link.revision)]",
        "const FX_SEND_INTERVAL_MS = 100;",
        "input.addEventListener(\"change\", () => take(\"release\"));",
        "case \"audio_strip_dsp_state\":",
        "fxTakeSnapshot(record);",
        "required(record, \"audio_master_dsp\")",
        "required(record, \"operation\") === \"audio_strip_dsp\"",
        "code === \"revision_conflict\" && !inflight.retried",
        "fxServerCode(target) !== edit.retry.base",
        "function fxGate(target) { return gate(\"audio\", target === \"master\" ? undefined : target); }",
        "id=\"fx-panel\"",
    ] {
        assert!(page.contains(source), "{source}");
    }

    let directory = TestDirectory::new("strip-fx");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start(&project);
    let set = |target: &str, chain: &StripDsp, gesture: Option<(u64, bool)>| {
        let mut payload = format!(
            "payload=audio_strip_dsp\ttarget={target}\tchain={}",
            chain.code()
        );
        if let Some((token, continues)) = gesture {
            let _ = write!(payload, "\tgesture={token}\tcontinues={continues}");
        }
        payload
    };
    let strip = input(1).to_string();
    let voice = StripDspPreset::Voice.chain();

    // A Viewer page is refused.
    let (mut page, _, _, snapshot) = web_session(daemon.web, Role::Viewer);
    assert_eq!(snapshot.audio_mixer.master_dsp, StripDsp::DEFAULT);
    assert_eq!(
        page_command(
            &mut page,
            "viewer-fx",
            Some(snapshot.revision),
            &set(&strip, &voice, None)
        )
        .unwrap_err()
        .0,
        "permission_denied"
    );
    page.close(None).unwrap();
    drop(page);

    // An Audio page sets a strip's chain.
    let (mut page, _, permissions, snapshot) = web_session(daemon.web, Role::Audio);
    assert_eq!(permissions, ["view_status", "control_audio"]);
    let (revision, event) = page_command(
        &mut page,
        "audio-fx-preset",
        Some(snapshot.revision),
        &set(&strip, &voice, None),
    )
    .unwrap();
    assert_eq!(
        event,
        EventPayload::AudioMixerChanged(AudioMixerOperation::SetStripDsp {
            target: AudioDspTarget::Input(input(1)),
            chain: Box::new(voice),
            gesture: None,
        })
    );
    // A drag: its first set opens a gesture, the later ones continue it.
    let mut dragged = voice;
    let mut revision = revision;
    for (step, continues) in [(0, false), (1, true), (2, true)] {
        dragged.compressor.threshold_millidb = -20_000 - 1_000 * step;
        let (next, _) = page_command(
            &mut page,
            &format!("audio-fx-drag-{step}"),
            Some(revision),
            &set(&strip, &dragged, Some((9_007_199_254_740_993, continues))),
        )
        .unwrap();
        revision = next;
    }
    // A set built from an older revision is refused, not applied.
    assert_eq!(
        page_command(
            &mut page,
            "audio-fx-stale",
            Some(revision - 1),
            &set(&strip, &StripDsp::DEFAULT, None)
        )
        .unwrap_err(),
        ("revision_conflict".to_owned(), revision)
    );
    // Master's chain.
    let master = StripDspPreset::Master.chain();
    let (revision, _) = page_command(
        &mut page,
        "audio-fx-master",
        Some(revision),
        &set("master", &master, None),
    )
    .unwrap();
    page.close(None).unwrap();
    drop(page);

    // Studio's Undo reverts the whole drag in one step, after Master's set.
    let mut studio = RawOperator::connect_as(daemon.control, Role::Admin);
    for id in ["undo-master", "undo-drag"] {
        studio.send(&command(
            id,
            CommandPayload::Undo {
                expected_edit: None,
            },
        ));
        studio.accepted(id);
    }

    // A reconnecting page learns every chain from its snapshot.
    let (mut page, lines) = web_sync(daemon.web, Role::Audio);
    let WireMessage::Snapshot(snapshot) = decode_line(&lines[0]).unwrap() else {
        panic!("expected a snapshot");
    };
    assert_eq!(snapshot.revision, revision + 2);
    assert_eq!(snapshot.audio_mixer.master_dsp, StripDsp::DEFAULT);
    assert_eq!(snapshot.audio_mixer.strip_dsp_states, 1);
    let state = web_until(&mut page, |_, message| match message {
        WireMessage::AudioStripDspState(state) => Some(state),
        _ => None,
    });
    assert_eq!(state.input, input(1));
    assert_eq!(state.chain, voice, "the drag was one undo step");
}

/// GX-006: the browser title editor is served like the other pages and its
/// exact wire records edit, take, update, and take off a title, each within
/// the session's role: a Graphics page edits text but cannot go to air, an
/// Operator page takes and clears but cannot change text, and a stale edit
/// is refused rather than overwriting a newer one.
#[test]
#[allow(clippy::too_many_lines)]
fn titles_page_edits_and_takes_a_title_within_the_session_role() {
    let directory = TestDirectory::new("titles-page");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);

    let (status, headers, body) = http(
        daemon.web,
        &format!("GET /titles HTTP/1.1\r\nHost: {}\r\n\r\n", daemon.web),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
    assert_eq!(header(&headers, "cache-control"), "no-store");
    assert_eq!(header(&headers, "x-frame-options"), "DENY");
    let policy = header(&headers, "content-security-policy");
    assert!(policy.starts_with("default-src 'none';"), "{policy}");
    let page = String::from_utf8(body).unwrap();
    assert!(page.starts_with("<!doctype html>"));
    assert!(
        page.contains(&format!("const PROTOCOL = \"{CURRENT_PROTOCOL_VERSION}\";")),
        "titles.html must be updated with the protocol identifier"
    );
    assert!(!page.contains(TOKEN), "the page never embeds the token");
    assert!(page.contains("history.replaceState(null, \"\", location.pathname + location.search)"));
    // The exact title commands driven below, and the pairing role.
    for source in [
        "[\"payload\", \"title_fields\"], [\"input\", title.id], [\"base_revision\", String(title.fieldRevision)],\n      [\"require\", \"off_air\"]",
        "[\"require\", \"on_air\"], [\"fields\", changesKey(list)]",
        "[\"payload\", \"title_take\"], [\"input\", title.id], [\"channel\", String(channel)]",
        "[\"payload\", \"title_off_air\"], [\"input\", title.id]",
        "const DEVICE_ROLES = [\"viewer\", \"audio\", \"operator\", \"graphics\"];",
        "case \"title_state\":",
        "name === \"title_changed\"",
        "issue === \"title_revision_conflict\"",
        "const COMMAND_TIMEOUT_MS = 8000;",
        // Protocol 2.90: the rendered preview, live phases, exposure, and
        // the image picker.
        "[\"phase\", \"hold\"], [\"frame\", \"0\"],\n      [\"width\", String(PREVIEW_WIDTH)], [\"height\", String(PREVIEW_HEIGHT)]",
        "if (list.length) fields.push([\"fields\", changesKey(list)]);",
        "case \"title_preview_frame\":",
        "case \"title_phases\":",
        "name === \"title_exposure_changed\"",
        "fetch(\"/v1/assets/images?name=\" + encodeURIComponent(file.name)",
        "headers: authorized({ \"Content-Type\": file.type }), body: file,",
    ] {
        assert!(page.contains(source), "{source}");
    }
    // The other pages pair a Graphics device too and link the editor.
    for (path, needle) in [
        (
            "/operator",
            "<a href=\"/titles\" data-i18n=\"titlesLink\">Titles</a>",
        ),
        ("/tally", "\"operator\", \"graphics\"]"),
    ] {
        let (_, _, body) = http(
            daemon.web,
            &format!("GET {path} HTTP/1.1\r\nHost: {}\r\n\r\n", daemon.web),
        );
        assert!(String::from_utf8(body).unwrap().contains(needle), "{path}");
    }

    let title = input(4);
    let fields = |text: &str| format!("1%3At%3A{}%3A{text}", text.len());
    let edit = |base: u64, require: &str, text: &str| {
        format!(
            "payload=title_fields\tinput={title}\tbase_revision={base}\trequire={require}\tfields={}",
            fields(text)
        )
    };
    let take = |base: u64, text: &str| {
        format!(
            "payload=title_take\tinput={title}\tchannel=1\tbase_revision={base}\tfields={}",
            if text.is_empty() {
                String::new()
            } else {
                fields(text)
            }
        )
    };
    let off_air = format!("payload=title_off_air\tinput={title}");

    // A Graphics page edits the text of an off-air title.
    let (mut graphics, _, permissions, snapshot) = web_session(daemon.web, Role::Graphics);
    assert_eq!(permissions, ["view_status", "edit_project"]);
    assert_eq!(snapshot.titles.len(), 1);
    assert_eq!(snapshot.titles[0].input, title);
    assert_eq!(snapshot.titles[0].field_revision, 0);
    let (_, event) = page_command(
        &mut graphics,
        "graphics-save",
        None,
        &edit(0, "off_air", "Ana"),
    )
    .unwrap();
    let EventPayload::TitleChanged {
        input: changed,
        field_revision,
        ..
    } = event
    else {
        panic!("a save is a title change: {event:?}");
    };
    assert_eq!((changed, field_revision), (title, 1));
    // A save on the old revision is refused, not applied over the newer one.
    assert_eq!(
        page_command(
            &mut graphics,
            "graphics-stale",
            None,
            &edit(0, "off_air", "Old")
        )
        .unwrap_err()
        .0,
        "conflict"
    );
    // Nor may it take the title to air or update it as if live.
    assert_eq!(
        page_command(&mut graphics, "graphics-take", None, &take(1, ""))
            .unwrap_err()
            .0,
        "permission_denied"
    );
    assert_eq!(
        page_command(
            &mut graphics,
            "graphics-live",
            None,
            &edit(1, "on_air", "Bea")
        )
        .unwrap_err()
        .0,
        "conflict"
    );

    // An Operator page takes the title as saved but cannot change its text.
    let (mut operator, _, permissions, _) = web_session(daemon.web, Role::Operator);
    assert!(!permissions.iter().any(|granted| granted == "edit_project"));
    assert_eq!(
        page_command(&mut operator, "operator-take-text", None, &take(1, "Cy"))
            .unwrap_err()
            .0,
        "permission_denied"
    );
    page_command(&mut operator, "operator-take", None, &take(1, "")).unwrap();

    // On air, the Graphics page updates it live and still cannot clear it.
    let (_, event) = page_command(
        &mut graphics,
        "graphics-update",
        None,
        &edit(1, "on_air", "Bea"),
    )
    .unwrap();
    assert!(
        matches!(
            event,
            EventPayload::TitleChanged {
                field_revision: 2,
                ..
            }
        ),
        "{event:?}"
    );
    assert_eq!(
        page_command(&mut graphics, "graphics-off", None, &off_air)
            .unwrap_err()
            .0,
        "permission_denied"
    );
    page_command(&mut operator, "operator-off", None, &off_air).unwrap();
}

/// RC-004 operator page shortcut buttons end to end: a page that connects
/// receives every project shortcut after its snapshot (`shortcut_state`),
/// and its `shortcut_fire`, exactly as the page writes it, fires within the
/// session role: an Operator page runs a shortcut that keys an overlay but
/// is refused one that edits a title (`edit_project`), a disabled shortcut
/// fires nothing, and an Audio page may fire none.
#[test]
#[allow(clippy::too_many_lines)]
fn operator_page_fires_project_shortcuts_within_the_session_role() {
    use fm_protocol::{
        ShortcutSpec, WireCueAction, WireOverlayChannelId, WireShortcutAction, WireTitleFieldId,
    };

    let directory = TestDirectory::new("operator-shortcuts");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);

    let shortcut = |name: &str, enabled: bool, action: WireCueAction| ShortcutSpec {
        name: name.into(),
        enabled,
        key: None,
        osc: None,
        action: WireShortcutAction::Actions(vec![action]),
    };
    let overlay = WireCueAction::OverlayOn {
        channel: WireOverlayChannelId::new(1).unwrap(),
        input: fm_types::InputTarget::Fixed(input(3)),
    };
    let score = WireCueAction::TitleNumber {
        input: fm_types::InputTarget::Fixed(input(4)),
        field: WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
        add: true,
        value: 1,
    };
    let mut admin = RawOperator::connect_as(daemon.control, Role::Admin);
    for (id, spec) in [
        (1, shortcut("Guest box", true, overlay.clone())),
        (2, shortcut("Score", true, score)),
        (3, shortcut("Spare", false, overlay)),
    ] {
        let key = format!("set-shortcut-{id}");
        admin.send(&command(
            &key,
            CommandPayload::SetShortcut {
                shortcut: id,
                spec: Box::new(spec),
                position: None,
            },
        ));
        admin.accepted(&key);
    }

    // A page connecting now gets the shortcuts with its snapshot.
    let connect = |role: Role| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "page never connected");
            let Some(mut socket) = browser_connect(daemon.web) else {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            send(&mut socket, &hello(role));
            let WireMessage::HandshakeResponse(response) = receive(&mut socket) else {
                panic!("expected a handshake response");
            };
            let WireMessage::Snapshot(snapshot) = receive(&mut socket) else {
                panic!("expected a snapshot");
            };
            assert_eq!(snapshot.shortcut_states, 3);
            let mut names = Vec::new();
            let mut layouts = snapshot.layout_scenes.len();
            while names.len() < 3 || layouts > 0 {
                match receive(&mut socket) {
                    WireMessage::ShortcutState(state) => names.push(state.shortcut.spec.name),
                    WireMessage::SceneLayoutState(_) => layouts -= 1,
                    WireMessage::Event(_)
                    | WireMessage::RuntimeEvent(_)
                    | WireMessage::CommandResult(_) => panic!("expected snapshot records"),
                    _ => {}
                }
            }
            assert_eq!(names, ["Guest box", "Score", "Spare"]);
            return (socket, response.permissions);
        }
    };
    let (mut page, permissions) = connect(Role::Operator);
    assert!(permissions.iter().any(|granted| granted == "transition"));
    assert!(!permissions.iter().any(|granted| granted == "edit_project"));

    match page_command(
        &mut page,
        "op-shortcut-1",
        None,
        "payload=shortcut_fire\tshortcut=1",
    ) {
        Ok((_, EventPayload::ShortcutFired(change))) => {
            assert_eq!(change.shortcut, 1);
            assert_eq!(change.actions, 1);
            assert!(
                change
                    .overlays
                    .iter()
                    .any(|overlay| overlay.source == Some(input(3))),
                "{:?}",
                change.overlays
            );
        }
        other => panic!("expected the shortcut to fire: {other:?}"),
    }
    let Err((code, _)) = page_command(
        &mut page,
        "op-shortcut-2",
        None,
        "payload=shortcut_fire\tshortcut=2",
    ) else {
        panic!("a title edit needs edit_project");
    };
    assert_eq!(code, "permission_denied");
    assert!(
        page_command(
            &mut page,
            "op-shortcut-3",
            None,
            "payload=shortcut_fire\tshortcut=3"
        )
        .is_err(),
        "a disabled shortcut fires nothing"
    );
    drop(page);

    let (mut audio, _) = connect(Role::Audio);
    let Err((code, _)) = page_command(
        &mut audio,
        "audio-shortcut-1",
        None,
        "payload=shortcut_fire\tshortcut=1",
    ) else {
        panic!("an Audio page may not fire shortcuts");
    };
    assert_eq!(code, "permission_denied");
}

/// RC-004: a web page's fire names the input selected on it. A shortcut
/// fire from the operator page keys the selected input, and one without a
/// selection is refused; a tally page's GO binds its input for the run,
/// which the cue status reports (Protocol 2.89), and an arm reports the
/// targets it bound.
#[test]
#[allow(clippy::too_many_lines)]
fn web_pages_fire_with_the_input_selected_on_them() {
    use fm_protocol::{
        CueListSpec, ShortcutSpec, WireCue, WireCueAction, WireOverlayChannelId, WireShortcutAction,
    };

    let directory = TestDirectory::new("web-selection");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);
    let mut admin = RawOperator::connect_as(daemon.control, Role::Admin);
    admin.send(&command(
        "set-shortcut",
        CommandPayload::SetShortcut {
            shortcut: 1,
            spec: Box::new(ShortcutSpec {
                name: "Key selected".into(),
                enabled: true,
                key: None,
                osc: None,
                action: WireShortcutAction::Actions(vec![WireCueAction::OverlayOn {
                    channel: WireOverlayChannelId::new(1).unwrap(),
                    input: fm_types::InputTarget::Selected,
                }]),
            }),
            position: None,
        },
    ));
    admin.accepted("set-shortcut");
    admin.send(&command(
        "set-cues",
        CommandPayload::SetCueList {
            list: 7,
            spec: Box::new(CueListSpec {
                name: "Show".into(),
                cues: vec![
                    WireCue {
                        id: 10,
                        name: "Take selected".into(),
                        auto_follow: false,
                        actions: vec![WireCueAction::Take {
                            input: Some(fm_types::InputTarget::Selected),
                            effect: fm_protocol::TransitionFavoriteEffect::Cut,
                            duration_frames: 0,
                        }],
                    },
                    WireCue {
                        id: 20,
                        name: "Preview selected".into(),
                        auto_follow: false,
                        actions: vec![WireCueAction::Preview {
                            input: fm_types::InputTarget::Selected,
                        }],
                    },
                ],
            }),
            position: None,
        },
    ));
    admin.accepted("set-cues");

    let (mut page, _, _, _) = web_session(daemon.web, Role::Operator);
    let Err((code, _)) = page_command(
        &mut page,
        "fire-unselected",
        None,
        "payload=shortcut_fire\tshortcut=1",
    ) else {
        panic!("a fire acting on the selected input needs a selection");
    };
    assert_eq!(code, "invalid_command");
    match page_command(
        &mut page,
        "fire-selected",
        None,
        &format!("payload=shortcut_fire\tshortcut=1\tselected={}", input(3)),
    ) {
        Ok((_, EventPayload::ShortcutFired(change))) => assert!(
            change
                .overlays
                .iter()
                .any(|overlay| overlay.source == Some(input(3))),
            "{:?}",
            change.overlays
        ),
        other => panic!("expected the shortcut to key input 3: {other:?}"),
    }

    // The tally page's GO selects its own input; the run reports the
    // inputs it bound.
    let Err((code, _)) = page_command(&mut page, "go-unselected", None, "payload=cue_go\tlist=7")
    else {
        panic!("a GO acting on the selected input needs a selection");
    };
    assert_eq!(code, "invalid_command");
    let (_, event) = page_command(
        &mut page,
        "go-selected",
        None,
        &format!("payload=cue_go\tlist=7\tselected={}", input(2)),
    )
    .unwrap();
    let EventPayload::CueRunChanged(change) = event else {
        panic!("a GO replicates the run state: {event:?}");
    };
    let bound = change.state.targets.expect("the GO bound its targets");
    assert_eq!(bound.selected, Some(input(2)));
    assert!(bound.preview.is_some() && bound.program.is_some());
    assert_eq!(change.state.armed, None);

    let (_, event) = page_command(
        &mut page,
        "arm-selected",
        None,
        &format!(
            "payload=cue_arm\tlist=7\tcue=20\tin_ms=60000\tselected={}",
            input(3)
        ),
    )
    .unwrap();
    let EventPayload::CueRunChanged(change) = event else {
        panic!("an arm replicates the run state: {event:?}");
    };
    let armed = change.state.armed.expect("cue 20 is armed");
    assert_eq!(armed.cue, 20);
    let armed_targets = armed.targets.expect("the arm bound its targets");
    assert_eq!(armed_targets.selected, Some(input(3)));
    // The run keeps the targets its GO bound.
    assert_eq!(change.state.targets, Some(bound));
}

/// A small real PNG.
fn png_bytes(seed: u8) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(24, 12, |x, y| {
        image::Rgba([
            seed,
            u8::try_from(x).unwrap(),
            u8::try_from(y).unwrap(),
            255,
        ])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

/// One upload to the gateway: status and body.
fn upload(
    daemon: &Daemon,
    token: Option<&str>,
    name: &str,
    content_type: &str,
    bytes: &[u8],
) -> (String, String) {
    let authorization = token.map_or_else(String::new, |token| {
        format!("Authorization: Bearer {token}\r\n")
    });
    let mut request = format!(
        "POST /v1/assets/images?name={name} HTTP/1.1\r\nHost: {}\r\n{authorization}Content-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        daemon.web,
        bytes.len()
    )
    .into_bytes();
    request.extend_from_slice(bytes);
    let (status, _, body) = web_tls::https_bytes(daemon.web, &request, TIMEOUT);
    (status, String::from_utf8(body).unwrap())
}

fn asset_get(
    daemon: &Daemon,
    token: &str,
    target: &str,
) -> (String, Vec<(String, String)>, Vec<u8>) {
    http(
        daemon.web,
        &format!(
            "GET {target} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {token}\r\n\r\n",
            daemon.web
        ),
    )
}

#[test]
fn title_editor_images_upload_into_the_bundle_and_list() {
    let directory = TestDirectory::new("asset-upload");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start(&project);
    let picture = png_bytes(3);

    // Without a token nothing is listed or stored.
    let (status, _) = upload(&daemon, None, "logo.png", "image/png", &picture);
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    let (status, _, _) = http(
        daemon.web,
        &format!(
            "GET /v1/assets/images HTTP/1.1\r\nHost: {}\r\n\r\n",
            daemon.web
        ),
    );
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");

    // A traversing name is reduced to a stem inside assets/uploads/.
    let (status, body) = upload(
        &daemon,
        Some(TOKEN),
        "..%2F..%2F..%2Fproject.json",
        "image/png",
        &picture,
    );
    assert_eq!(status, "HTTP/1.1 201 Created", "{body}");
    assert!(
        body.starts_with(r#"{"uri":"asset://uploads/project-"#),
        "{body}"
    );
    assert!(body.ends_with(r#""width":24,"height":12}"#), "{body}");
    let uri = body.split('"').nth(3).unwrap().to_owned();
    let stored = project
        .join("assets")
        .join(uri.strip_prefix("asset://").unwrap());
    assert_eq!(fs::read(&stored).unwrap(), picture);
    // The same picture again is the stored one.
    let (status, again) = upload(&daemon, Some(TOKEN), "copy.png", "image/png", &picture);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(again.contains(&uri));

    // Mismatched type, markup, and a corrupt picture are refused.
    let (status, _) = upload(&daemon, Some(TOKEN), "a.jpg", "image/jpeg", &picture);
    assert_eq!(status, "HTTP/1.1 415 Unsupported Media Type");
    let (status, _) = upload(&daemon, Some(TOKEN), "a.svg", "image/svg+xml", b"<svg/>");
    assert_eq!(status, "HTTP/1.1 415 Unsupported Media Type");
    let (status, _) = upload(
        &daemon,
        Some(TOKEN),
        "a.png",
        "image/png",
        &picture[..picture.len() / 2],
    );
    assert_eq!(status, "HTTP/1.1 422 Unprocessable Content");

    // Listed, and served back with its sniffed type.
    let (status, _, list) = asset_get(&daemon, TOKEN, "/v1/assets/images");
    assert_eq!(status, "HTTP/1.1 200 OK");
    let list = String::from_utf8(list).unwrap();
    assert!(
        list.contains(&format!(r#"{{"uri":"{uri}","bytes":{}}}"#, picture.len())),
        "{list}"
    );
    assert!(list.contains(r#""uploaded":1"#), "{list}");
    let (status, headers, served) = asset_get(
        &daemon,
        TOKEN,
        &format!("/v1/assets/image?uri={}", uri.replace("://", "%3A%2F%2F")),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(header(&headers, "content-type"), "image/png");
    assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
    assert_eq!(served, picture);
    // Nothing outside the assets directory is served.
    for target in [
        "/v1/assets/image?uri=asset%3A%2F%2F..%2Fproject.json",
        "/v1/assets/image?uri=asset%3A%2F%2Fuploads%2F..%2F..%2Fproject.json",
        "/v1/assets/image?uri=..%2Fproject.json",
    ] {
        let (status, _, _) = asset_get(&daemon, TOKEN, target);
        assert_eq!(status, "HTTP/1.1 404 Not Found", "{target}");
    }
    assert!(project.join("project.json").is_file());
}

/// Connects and handshakes as a `role` page, returning every record of its
/// initial sync (snapshot through the scene layouts) as written.
fn web_sync(address: SocketAddr, role: Role) -> (Browser, Vec<String>) {
    let mut socket = browser_connect(address).expect("gateway connection");
    send(&mut socket, &hello(role));
    let mut lines = Vec::new();
    let read = |socket: &mut Browser| loop {
        match socket.read().unwrap() {
            Message::Text(text) => return text.as_str().to_owned(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected WebSocket message {other:?}"),
        }
    };
    let WireMessage::HandshakeResponse(response) = decode_line(&read(&mut socket)).unwrap() else {
        panic!("expected a handshake response");
    };
    assert_eq!(response.granted_role, role);
    let snapshot = read(&mut socket);
    let WireMessage::Snapshot(decoded) = decode_line(&snapshot).unwrap() else {
        panic!("expected a snapshot");
    };
    lines.push(snapshot);
    let mut layouts = decoded.layout_scenes.len();
    while layouts > 0 {
        let line = read(&mut socket);
        if matches!(
            decode_line(&line).unwrap(),
            WireMessage::SceneLayoutState(_)
        ) {
            layouts -= 1;
        }
        lines.push(line);
    }
    (socket, lines)
}

/// The `title_state` of input 4 among `lines`.
fn title_state_of(lines: &[String]) -> fm_protocol::TitleStateMessage {
    lines
        .iter()
        .find_map(|line| match decode_line(line).unwrap() {
            WireMessage::TitleState(state) if state.input == input(4) => Some(state),
            _ => None,
        })
        .expect("a title_state for the title")
}

/// Reads `socket` until `found` picks a record, failing after 5 s.
fn web_until<T>(socket: &mut Browser, mut found: impl FnMut(&str, WireMessage) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "the record never arrived");
        match socket.read().unwrap() {
            Message::Text(text) => {
                if let Some(value) = found(text.as_str(), decode_line(text.as_str()).unwrap()) {
                    return value;
                }
            }
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected WebSocket message {other:?}"),
        }
    }
}

/// The next record, which must be a `title_state` (one follows each
/// `title_exposure_changed` a Web session gets, Protocol 2.96); lossy
/// status records in between are skipped.
fn next_title_state(socket: &mut Browser) -> fm_protocol::TitleStateMessage {
    web_until(socket, |line, message| match message {
        WireMessage::TitleState(state) => Some(state),
        WireMessage::Event(_) | WireMessage::CommandResult(_) => {
            panic!("expected the title_state after the exposure change: {line}")
        }
        _ => None,
    })
}

fn preview_request(id: &str, width: u16, height: u16, fields: &[(u128, &str)]) -> String {
    encode_line(&WireMessage::TitlePreviewRequest(
        fm_protocol::TitlePreviewRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: id.into(),
            input: input(4),
            sample: fm_protocol::TitlePreviewSample::Hold,
            width,
            height,
            clip: None,
            fields: fields
                .iter()
                .map(|(field, text)| fm_protocol::TitleFieldUpdate {
                    field: fm_protocol::WireTitleFieldId::new(NonZeroU128::new(*field).unwrap()),
                    value: fm_protocol::TitleFieldValue::Text((*text).into()),
                })
                .collect(),
        },
    ))
    .unwrap()
}

/// The rendered preview answering `id` (its size and draft flag), or the
/// error code it met.
fn preview_answer(socket: &mut Browser, id: &str) -> Result<(u16, u16, bool), String> {
    let mut data = Vec::new();
    web_until(socket, |_, message| match message {
        WireMessage::TitlePreviewFrame(frame) if frame.request_id == id => {
            data.extend_from_slice(&frame.data);
            (frame.part + 1 == frame.parts).then(|| {
                let pixels = fm_protocol::decode_qoi_rgba(frame.width, frame.height, &data)
                    .expect("a whole QOI image");
                assert_eq!(
                    pixels.len(),
                    usize::from(frame.width) * usize::from(frame.height) * 4
                );
                Ok((frame.width, frame.height, frame.draft))
            })
        }
        WireMessage::Error(error) if error.request_id.as_deref() == Some(id) => {
            Some(Err(error.error.code))
        }
        _ => None,
    })
}

/// GX-006 through the gateway: a browser never receives a field its
/// template hides (not in its `title_state`, not in a `title_changed`), may
/// not name one or change exposure, previews its unsaved drafts as a small
/// server-rendered image, and follows title phases live; Studio changes
/// exposure, which survives a restart.
#[test]
#[allow(clippy::too_many_lines)]
fn web_title_editor_hides_fields_previews_drafts_and_follows_phases() {
    let directory = TestDirectory::new("web-title-editor");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let mut daemon = Daemon::start(&project);
    let title = input(4);

    // Studio (raw) sees both fields; a Graphics page only the exposed one.
    let mut studio = RawOperator::connect_as(daemon.control, Role::Admin);
    let (mut graphics, sync) = web_sync(daemon.web, Role::Graphics);
    for line in &sync {
        assert!(
            !line.contains("secret-note") && !line.contains("Producer"),
            "a hidden field reached the browser: {line}"
        );
    }
    let state = title_state_of(&sync);
    assert_eq!(state.fields.len(), 1);
    assert_eq!(state.fields[0].name, "Name");
    assert!(state.fields[0].exposed);

    // The page may not name the hidden field nor change exposure.
    let note = |text: &str| format!("2%3At%3A{}%3A{text}", text.len());
    assert_eq!(
        page_command(
            &mut graphics,
            "web-hidden",
            None,
            &format!(
                "payload=title_fields\tinput={title}\tbase_revision=0\trequire=off_air\tfields={}",
                note("x")
            ),
        )
        .unwrap_err()
        .0,
        "not_found"
    );
    assert_eq!(
        page_command(
            &mut graphics,
            "web-expose",
            None,
            &format!("payload=title_field_exposure\tinput={title}\tfield=2\texposed=1"),
        )
        .unwrap_err()
        .0,
        "permission_denied"
    );

    // Studio edits both fields; the page's event carries only the name.
    studio.send(&command(
        "studio-edit",
        CommandPayload::SetTitleFields {
            input: title,
            base_revision: 0,
            require: fm_protocol::TitleAirRequirement::OffAir,
            fields: [(1, "Ana"), (2, "new-secret")]
                .map(|(field, text)| fm_protocol::TitleFieldUpdate {
                    field: fm_protocol::WireTitleFieldId::new(NonZeroU128::new(field).unwrap()),
                    value: fm_protocol::TitleFieldValue::Text(text.into()),
                })
                .to_vec(),
        },
    ));
    studio.accepted("studio-edit");
    let changed = web_until(&mut graphics, |line, message| match message {
        WireMessage::Event(EventMessage {
            payload: EventPayload::TitleChanged { fields, .. },
            ..
        }) => Some((line.to_owned(), fields)),
        _ => None,
    });
    assert!(!changed.0.contains("new-secret"), "{}", changed.0);
    assert_eq!(changed.1.len(), 1);

    // A draft renders as a small image without saving anything.
    graphics
        .send(Message::text(preview_request(
            "p1",
            320,
            180,
            &[(1, "Draft")],
        )))
        .unwrap();
    assert_eq!(preview_answer(&mut graphics, "p1"), Ok((320, 180, true)));
    graphics
        .send(Message::text(preview_request("p2", 160, 90, &[])))
        .unwrap();
    assert_eq!(preview_answer(&mut graphics, "p2"), Ok((160, 90, false)));
    // Larger than a browser preview, or a hidden draft, is refused.
    graphics
        .send(Message::text(preview_request("p3", 640, 360, &[])))
        .unwrap();
    assert_eq!(
        preview_answer(&mut graphics, "p3"),
        Err("invalid_request".into())
    );
    graphics
        .send(Message::text(preview_request("p4", 320, 180, &[(2, "x")])))
        .unwrap();
    assert_eq!(preview_answer(&mut graphics, "p4"), Err("not_found".into()));
    // The request exactly as the titles page writes it.
    graphics
        .send(Message::text(format!(
            "title_preview_request\tprotocol={CURRENT_PROTOCOL_VERSION}\trequest_id=preview-page-1\t\
             input={title}\tphase=hold\tframe=0\twidth=320\theight=180\tfields=1%3At%3A5%3ADraft\n"
        )))
        .unwrap();
    assert_eq!(
        preview_answer(&mut graphics, "preview-page-1"),
        Ok((320, 180, true))
    );

    // Taking the title reaches the page as a live phase record.
    studio.send(&command(
        "studio-take",
        CommandPayload::TakeTitle {
            input: title,
            channel: fm_protocol::WireOverlayChannelId::new(1).unwrap(),
            base_revision: 1,
            fields: Vec::new(),
        },
    ));
    studio.accepted("studio-take");
    web_until(&mut graphics, |_, message| match message {
        WireMessage::TitlePhases(phases) => phases
            .titles
            .iter()
            .any(|status| {
                status.input == title
                    && status.phase != fm_protocol::TitlePhase::Idle
                    && status.timeline.is_some()
            })
            .then_some(()),
        _ => None,
    });

    // Studio exposes the note: the page is told, and after a restart the
    // template still exposes it.
    studio.send(&command(
        "studio-expose",
        CommandPayload::SetTitleFieldExposure {
            input: title,
            field: fm_protocol::WireTitleFieldId::new(NonZeroU128::new(2).unwrap()),
            exposed: true,
        },
    ));
    studio.accepted("studio-expose");
    let exposed = web_until(&mut graphics, |_, message| match message {
        WireMessage::Event(EventMessage {
            payload: EventPayload::TitleExposureChanged { inputs, fields },
            ..
        }) => Some((inputs, fields)),
        _ => None,
    });
    assert_eq!(exposed.0, [title]);
    assert_eq!(exposed.1.len(), 2);
    assert!(exposed.1.iter().all(|item| item.exposed));
    // Protocol 2.96: the page's title follows at once, with the newly
    // exposed field's name, value, and revision, without a reconnect.
    let resynced = next_title_state(&mut graphics);
    assert_eq!((resynced.input, resynced.field_revision), (title, 1));
    assert_eq!(resynced.fields.len(), 2, "{resynced:?}");
    assert_eq!(resynced.fields[1].name, "Producer note");
    assert_eq!(
        resynced.fields[1].value,
        fm_protocol::TitleFieldValue::Text("new-secret".into())
    );
    assert_eq!(resynced.fields[1].revision, 1);
    // Hidden again, it disappears live: neither the event nor the state
    // that follows names it.
    for (key, exposed) in [("studio-hide", false), ("studio-reexpose", true)] {
        studio.send(&command(
            key,
            CommandPayload::SetTitleFieldExposure {
                input: title,
                field: fm_protocol::WireTitleFieldId::new(NonZeroU128::new(2).unwrap()),
                exposed,
            },
        ));
        studio.accepted(key);
        let (line, fields) = web_until(&mut graphics, |line, message| match message {
            WireMessage::Event(EventMessage {
                payload: EventPayload::TitleExposureChanged { fields, .. },
                ..
            }) => Some((line.to_owned(), fields)),
            _ => None,
        });
        let state = next_title_state(&mut graphics);
        let shown = if exposed { 2 } else { 1 };
        assert_eq!(fields.len(), shown, "{line}");
        assert_eq!(state.fields.len(), shown, "{state:?}");
        if !exposed {
            assert!(!line.contains("2%3A0"), "{line}");
            let encoded = encode_line(&WireMessage::TitleState(state)).unwrap();
            assert!(!encoded.contains("new-secret"), "{encoded}");
        }
    }
    drop((studio, graphics));
    drop(daemon);
    daemon = Daemon::start(&project);
    let (_graphics, sync) = web_sync(daemon.web, Role::Graphics);
    let state = title_state_of(&sync);
    assert_eq!(state.fields.len(), 2, "{state:?}");
    assert_eq!(
        state.fields[1].value,
        fm_protocol::TitleFieldValue::Text("new-secret".into())
    );
}

/// GX-006 per-field revisions (Protocol 2.96): Studio and a browser that
/// both saw the same title revision edit different fields of it and both
/// edits apply; a stale edit of a field someone else changed is refused as
/// `title_revision_conflict`, from another browser and from Studio alike,
/// and changes nothing.
#[test]
fn studio_and_browsers_edit_different_fields_of_a_title_without_clobbering() {
    let directory = TestDirectory::new("title-field-revisions");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);
    let title = input(4);
    let mut studio = RawOperator::connect_as(daemon.control, Role::Admin);
    let (mut page, _, _, _) = web_session(daemon.web, Role::Graphics);
    let (mut other_page, _, _, _) = web_session(daemon.web, Role::Graphics);
    let page_edit = |base: u64, text: &str| {
        format!(
            "payload=title_fields\tinput={title}\tbase_revision={base}\trequire=off_air\t\
             fields=1%3At%3A{}%3A{text}",
            text.len()
        )
    };
    let studio_edit = |field: u128, text: &str| {
        command(
            &format!("studio-{field}-{text}"),
            CommandPayload::SetTitleFields {
                input: title,
                base_revision: 0,
                require: fm_protocol::TitleAirRequirement::OffAir,
                fields: vec![fm_protocol::TitleFieldUpdate {
                    field: fm_protocol::WireTitleFieldId::new(NonZeroU128::new(field).unwrap()),
                    value: fm_protocol::TitleFieldValue::Text(text.into()),
                }],
            },
        )
    };

    // Both saw revision 0: the page sets the name, Studio the note.
    page_command(&mut page, "page-name", None, &page_edit(0, "Ana")).unwrap();
    studio.send(&studio_edit(2, "cue"));
    studio.accepted("studio-2-cue");

    // Another browser still on revision 0 may not overwrite the name ...
    assert_eq!(
        page_command(&mut other_page, "other-name", None, &page_edit(0, "Bea"))
            .unwrap_err()
            .0,
        "conflict"
    );
    // ... nor may Studio; the refusal names the field and its revision.
    studio.send(&studio_edit(1, "Cy"));
    let refusal = loop {
        if let WireMessage::CommandResult(result) = studio.receive() {
            break result;
        }
    };
    let CommandResult::Rejected { code, fields, .. } = &refusal else {
        panic!("a stale edit of the name is refused: {refusal:?}");
    };
    assert_eq!(code, "conflict");
    assert_eq!(fields[0].code, "title_revision_conflict");
    assert!(
        fields[0].message.contains("field 1 changed at revision 1"),
        "{refusal:?}"
    );
    // On the newest revision the other browser's name edit applies.
    page_command(&mut other_page, "other-name-2", None, &page_edit(2, "Bea")).unwrap();

    // A fresh page sees every edit with the revision that made it.
    let (_socket, sync) = web_sync(daemon.web, Role::Graphics);
    let state = title_state_of(&sync);
    assert_eq!(state.field_revision, 3);
    assert_eq!(
        state.fields[0].value,
        fm_protocol::TitleFieldValue::Text("Bea".into())
    );
    assert_eq!(state.fields[0].revision, 3);
    // Studio sees the hidden note's revision too.
    drop(studio);
    let mut studio = RawOperator::connect_as(daemon.control, Role::Admin);
    studio.send(&studio_edit(2, "stale-note"));
    let refusal = loop {
        if let WireMessage::CommandResult(result) = studio.receive() {
            break result;
        }
    };
    assert!(
        matches!(&refusal, CommandResult::Rejected { code, .. } if code == "conflict"),
        "the note changed at revision 2: {refusal:?}"
    );
}

/// GX-006: a cue that sets a title's fields reaches a browser title editor
/// as that title's new state. The page learns title values only from
/// `title_changed` and `title_state`, so the run's `cue_run_changed` is
/// followed by a `title_state` of each title it set, holding the new value
/// and field revision (without the hidden field) the page needs to save on.
#[test]
fn a_cue_title_text_resynchronizes_the_web_title_editor() {
    use fm_protocol::{CueListSpec, CueOperation, WireCue, WireCueAction, WireTitleFieldId};

    let directory = TestDirectory::new("cue-title-resync");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);
    let field = |value: u128| WireTitleFieldId::new(NonZeroU128::new(value).unwrap());
    let mut admin = RawOperator::connect_as(daemon.control, Role::Admin);
    admin.send(&command(
        "set-cues",
        CommandPayload::SetCueList {
            list: 7,
            spec: Box::new(CueListSpec {
                name: "Show".into(),
                cues: vec![WireCue {
                    id: 1,
                    name: "Name".into(),
                    auto_follow: false,
                    actions: vec![
                        WireCueAction::TitleText {
                            input: fm_types::InputTarget::Fixed(input(4)),
                            field: field(1),
                            text: "Bea".into(),
                        },
                        WireCueAction::TitleText {
                            input: fm_types::InputTarget::Fixed(input(4)),
                            field: field(2),
                            text: "cue-secret".into(),
                        },
                    ],
                }],
            }),
            position: None,
        },
    ));
    admin.accepted("set-cues");
    let (mut page, _, _, _) = web_session(daemon.web, Role::Graphics);

    admin.send(&command(
        "go",
        CommandPayload::CueControl {
            list: 7,
            operation: CueOperation::Go {
                cue: Some(1),
                targets: fm_protocol::WireTargetBindings::default(),
            },
        },
    ));
    admin.accepted("go");

    let mut leaked = Vec::new();
    let state = web_until(&mut page, |line, message| {
        // No record names it: not the run's, nor (Protocol 2.104) the cue
        // list's own spec, as text or as the hex it travels in there.
        if carries(line, "cue-secret") {
            leaked.push(line.to_owned());
        }
        match message {
            WireMessage::TitleState(state) if state.input == input(4) => Some(state),
            _ => None,
        }
    });
    assert_eq!(leaked, Vec::<String>::new());
    assert!(state.field_revision > 0);
    assert_eq!(state.fields.len(), 1, "only the exposed field: {state:?}");
    assert_eq!(state.fields[0].field, field(1));
    assert_eq!(
        state.fields[0].value,
        fm_protocol::TitleFieldValue::Text("Bea".into())
    );
    // Each cue action is its own update: the name changed before the note.
    assert!(state.fields[0].revision > 0 && state.fields[0].revision < state.field_revision);

    // The page saves on the revision it was sent.
    page_command(
        &mut page,
        "page-name",
        None,
        &format!(
            "payload=title_fields\tinput={}\tbase_revision={}\trequire=off_air\t\
             fields=1%3At%3A3%3ACyd",
            input(4),
            state.field_revision
        ),
    )
    .unwrap();
}

/// The title project with a second title, input 5 ("Open third"), of a
/// template like input 4's that shows its note (field 2) to browsers.
fn create_project_with_two_titles(path: &Path) {
    const OPEN_TEMPLATE_KEY: &str = "titles/open-title.fmtitle";
    create_project_with(path, true, false);
    let store = ProjectStore::new(path).unwrap();
    write_template(&store, OPEN_TEMPLATE_KEY, true);
    let stored = store.load().unwrap();
    let mut project = stored.project().clone();
    project.add_input(Input {
        id: domain_input(5),
        name: "Open third".into(),
        kind: InputKind::Title(fm_model::TitleInput {
            template_uri: format!("asset://{OPEN_TEMPLATE_KEY}"),
            field_revision: 0,
            fields: vec![
                fm_model::TitleFieldEntry {
                    id: fm_model::TitleFieldId::new(NonZeroU128::new(1).unwrap()),
                    value: fm_model::TitleFieldValue::Text("Name".into()),
                    revision: 0,
                },
                fm_model::TitleFieldEntry {
                    id: fm_model::TitleFieldId::new(NonZeroU128::new(2).unwrap()),
                    value: fm_model::TitleFieldValue::Text("open-note".into()),
                    revision: 0,
                },
            ],
        }),
        required_capabilities: Vec::new(),
    });
    store.save(&stored.with_project(project).unwrap()).unwrap();
}

/// GX-006, Protocol 2.104: a Web session cannot read back a value hidden
/// from it by firing a cue or shortcut whose `preview` action sets a field
/// id another title hides, after arranging for Preview to show a title
/// exposing that field. The whole run is refused `permission_denied` and
/// nothing is written; the same fire is accepted while Preview is the
/// title hiding the field, a fixed action writing a hidden value into the
/// hidden field runs, and Studio's fire is never checked.
#[test]
#[allow(clippy::too_many_lines)] // one end-to-end scenario
fn a_web_fire_cannot_write_a_hidden_value_where_the_page_can_read_it() {
    use fm_protocol::{
        CueListSpec, CueOperation, ShortcutSpec, WireCue, WireCueAction, WireShortcutAction,
        WireTargetBindings, WireTitleFieldId,
    };
    use fm_types::InputTarget;

    let directory = TestDirectory::new("web-fire-hidden");
    let project = directory.0.join("show.freemix");
    create_project_with_two_titles(&project);
    let daemon = Daemon::start(&project);
    let field = |value: u128| WireTitleFieldId::new(NonZeroU128::new(value).unwrap());
    let text = |target, id, value: &str| WireCueAction::TitleText {
        input: target,
        field: field(id),
        text: value.into(),
    };
    let mut admin = RawOperator::connect_as(daemon.control, Role::Admin);
    let mut sent = 0;
    let mut admin_accept = |admin: &mut RawOperator, payload: CommandPayload| {
        sent += 1;
        let id = format!("admin-{sent}");
        admin.send(&command(&id, payload));
        admin.accepted(&id);
    };
    admin_accept(
        &mut admin,
        CommandPayload::SetCueList {
            list: 7,
            spec: Box::new(CueListSpec {
                name: "Show".into(),
                cues: vec![
                    WireCue {
                        id: 1,
                        name: "Preview note".into(),
                        auto_follow: false,
                        actions: vec![text(InputTarget::Preview, 2, "dyn-secret")],
                    },
                    WireCue {
                        id: 2,
                        name: "Fixed note".into(),
                        auto_follow: false,
                        actions: vec![text(InputTarget::Fixed(input(4)), 2, "fixed-secret")],
                    },
                ],
            }),
            position: None,
        },
    );
    admin_accept(
        &mut admin,
        CommandPayload::SetShortcut {
            shortcut: 3,
            spec: Box::new(ShortcutSpec {
                name: "Note".into(),
                enabled: true,
                key: None,
                osc: None,
                action: WireShortcutAction::Actions(vec![text(
                    InputTarget::Preview,
                    2,
                    "short-secret",
                )]),
            }),
            position: None,
        },
    );
    let go = |cue| CommandPayload::CueControl {
        list: 7,
        operation: CueOperation::Go {
            cue: Some(cue),
            targets: WireTargetBindings::default(),
        },
    };
    let arm = CommandPayload::CueControl {
        list: 7,
        operation: CueOperation::Arm {
            cue: Some(1),
            in_ms: 60_000,
            targets: WireTargetBindings::default(),
        },
    };
    let fire = CommandPayload::FireShortcut {
        shortcut: 3,
        targets: WireTargetBindings::default(),
        value: None,
    };

    let mut seen = Vec::new();
    // Even an Admin page is a Web session: it may fire cues and title
    // actions, but not read a value hidden from browsers.
    let (mut page, _, _, _) = web_session(daemon.web, Role::Admin);
    // The title exposing the note goes on Preview (Studio puts it there,
    // or the page waits for it), and the page fires.
    admin_accept(
        &mut admin,
        CommandPayload::SelectPreview { input: input(5) },
    );
    for (id, payload) in [
        ("page-go", go(1)),
        ("page-arm", arm.clone()),
        ("page-fire", fire.clone()),
    ] {
        assert_eq!(
            scanned_result(&mut page, &mut seen, id, payload),
            "permission_denied",
            "{id}"
        );
    }
    // With the title hiding the note on Preview, the same fires run, as
    // does a fixed action into the hidden field.
    admin_accept(
        &mut admin,
        CommandPayload::SelectPreview { input: input(4) },
    );
    for (id, payload) in [
        ("page-go-hidden", go(1)),
        ("page-fire-hidden", fire.clone()),
        ("page-go-fixed", go(2)),
    ] {
        assert_eq!(
            scanned_result(&mut page, &mut seen, id, payload),
            "accepted",
            "{id}"
        );
    }
    for line in &seen {
        for secret in ["dyn-secret", "short-secret", "fixed-secret"] {
            assert!(!carries(line, secret), "{secret} reached the page: {line}");
        }
    }
    // Studio's fire onto the exposed note is not checked.
    admin_accept(
        &mut admin,
        CommandPayload::SelectPreview { input: input(5) },
    );
    admin_accept(&mut admin, go(1));
    admin_accept(&mut admin, fire);
}

/// GX-006, Protocols 2.96 and 2.104: a raw Web client that resumes past an
/// exposure change converges as a live one does. After the replayed
/// `title_exposure_changed` it gets the title's state with the newly
/// exposed field, and the cue list and shortcut that set it, now with the
/// value they carried as the hidden marker before.
#[test]
#[allow(clippy::too_many_lines)] // one end-to-end scenario
fn a_resumed_web_session_converges_after_an_exposure_change() {
    use fm_protocol::{
        CueListSpec, ResumeCursor, ShortcutSpec, WireCue, WireCueAction, WireShortcutAction,
        WireTitleFieldId,
    };
    use fm_types::InputTarget;

    let directory = TestDirectory::new("web-resume-exposure");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);
    let field = |value: u128| WireTitleFieldId::new(NonZeroU128::new(value).unwrap());
    let note = WireCueAction::TitleText {
        input: InputTarget::Fixed(input(4)),
        field: field(2),
        text: "cue-secret".into(),
    };
    let mut admin = RawOperator::connect_as(daemon.control, Role::Admin);
    for (id, payload) in [
        (
            "set-cues",
            CommandPayload::SetCueList {
                list: 7,
                spec: Box::new(CueListSpec {
                    name: "Show".into(),
                    cues: vec![WireCue {
                        id: 1,
                        name: "Note".into(),
                        auto_follow: false,
                        actions: vec![note.clone()],
                    }],
                }),
                position: None,
            },
        ),
        (
            "set-shortcut",
            CommandPayload::SetShortcut {
                shortcut: 3,
                spec: Box::new(ShortcutSpec {
                    name: "Note".into(),
                    enabled: true,
                    key: None,
                    osc: None,
                    action: WireShortcutAction::Actions(vec![note.clone()]),
                }),
                position: None,
            },
        ),
    ] {
        admin.send(&command(id, payload));
        admin.accepted(id);
    }

    // A page syncs, then goes away.
    let (page, server, _, snapshot) = web_session(daemon.web, Role::Graphics);
    drop(page);
    // Studio exposes the note meanwhile.
    admin.send(&command(
        "expose",
        CommandPayload::SetTitleFieldExposure {
            input: input(4),
            field: field(2),
            exposed: true,
        },
    ));
    admin.accepted("expose");

    // The client resumes from where it was.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut socket = loop {
        assert!(
            Instant::now() < deadline,
            "the gateway never took the client"
        );
        if let Some(socket) = browser_connect(daemon.web) {
            break socket;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    send(
        &mut socket,
        &WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "freemix-raw-web".into(),
            client_type: ClientType::Web,
            desired_role: Role::Graphics,
            resume_cursor: Some(ResumeCursor {
                server,
                revision: snapshot.revision,
            }),
        }),
    );
    let WireMessage::HandshakeResponse(response) = receive(&mut socket) else {
        panic!("expected a handshake response");
    };
    assert!(
        matches!(response.outcome, HandshakeOutcome::Resume { .. }),
        "{:?}",
        response.outcome
    );
    let mut exposure = false;
    let mut state = None;
    let mut list = None;
    let mut shortcut = None;
    web_until(&mut socket, |_, message| {
        match message {
            WireMessage::Event(EventMessage {
                payload: EventPayload::TitleExposureChanged { .. },
                ..
            }) => exposure = true,
            WireMessage::TitleState(found) if found.input == input(4) => {
                assert!(exposure, "the title state follows the replayed event");
                state = Some(found);
            }
            WireMessage::CueListState(found) => list = Some(found.list),
            WireMessage::ShortcutState(found) => shortcut = Some(found.shortcut),
            _ => {}
        }
        (state.is_some() && list.is_some() && shortcut.is_some()).then_some(())
    });
    let state = state.unwrap();
    assert!(
        state.fields.iter().any(|status| status.field == field(2)
            && status.value == fm_protocol::TitleFieldValue::Text("secret-note".into())),
        "{state:?}"
    );
    assert_eq!(list.unwrap().spec.cues[0].actions, [note.clone()]);
    assert_eq!(
        shortcut.unwrap().spec.action,
        WireShortcutAction::Actions(vec![note])
    );
}

/// Whether `line` carries `secret` as written or as the lowercase hex cue
/// texts and script sources travel in.
fn carries(line: &str, secret: &str) -> bool {
    let hex: String = secret.bytes().fold(String::new(), |mut hex, byte| {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}").unwrap();
        hex
    });
    line.contains(secret) || line.contains(&hex)
}

/// Reads `page` until a record starting with `kind`, keeping every record
/// in `seen`.
fn next_kind(page: &mut Browser, seen: &mut Vec<String>, kind: &str) -> WireMessage {
    web_until(page, |line, message| {
        seen.push(line.to_owned());
        line.starts_with(kind).then_some(message)
    })
}

/// Sends `payload` from `page` as command `id` and waits for its result,
/// keeping every record in `seen`; the result's code or `accepted`.
fn scanned_result(
    page: &mut Browser,
    seen: &mut Vec<String>,
    id: &str,
    payload: CommandPayload,
) -> String {
    send(page, &command(id, payload));
    web_until(page, |line, message| {
        seen.push(line.to_owned());
        match message {
            WireMessage::CommandResult(CommandResult::Accepted { id: found, .. })
                if found == id =>
            {
                Some("accepted".to_owned())
            }
            WireMessage::CommandResult(CommandResult::Rejected {
                id: found, code, ..
            }) if found == id => Some(code),
            _ => None,
        }
    })
}

/// GX-006, Protocol 2.104: a browser never receives the value of a field
/// hidden from it inside a cue list, shortcut, or script. Every record a
/// Graphics page gets (initial sync, its own edits' events, refusals) is
/// scanned; actions setting a hidden field carry the marker, a role target
/// that may reach one too, and scripts carry no source. The page's edits
/// that send the markers back keep the stored values; an edit setting a
/// hidden field, a misplaced marker, and a script edit are refused, as is a
/// marker from Studio. An exposure change re-sends the lists and shortcuts.
#[test]
#[allow(clippy::too_many_lines)] // one end-to-end scenario
fn web_sessions_never_see_hidden_values_in_cue_lists_shortcuts_or_scripts() {
    use fm_protocol::{
        CueListSpec, ScriptSpec, ShortcutSpec, WireCue, WireCueAction, WireShortcutAction,
        WireTitleFieldId,
    };
    use fm_types::InputTarget;

    let directory = TestDirectory::new("web-hidden-definitions");
    let project = directory.0.join("show.freemix");
    create_project_with(&project, true, false);
    let daemon = Daemon::start(&project);
    let field = |value: u128| WireTitleFieldId::new(NonZeroU128::new(value).unwrap());
    let title = InputTarget::Fixed(input(4));
    let text = |target, id, value: &str| WireCueAction::TitleText {
        input: target,
        field: field(id),
        text: value.into(),
    };
    let marker = |target, id| WireCueAction::TitleHidden {
        input: target,
        field: field(id),
        number: false,
    };
    let set_list = |spec: CueListSpec| CommandPayload::SetCueList {
        list: 7,
        spec: Box::new(spec),
        position: None,
    };

    let mut admin = RawOperator::connect_as(daemon.control, Role::Admin);
    let list = CueListSpec {
        name: "Show".into(),
        cues: vec![
            WireCue {
                id: 1,
                name: "Names".into(),
                auto_follow: false,
                actions: vec![text(title, 1, "Bea"), text(title, 2, "cue-secret")],
            },
            WireCue {
                id: 2,
                name: "Dynamic".into(),
                auto_follow: false,
                actions: vec![text(InputTarget::Preview, 2, "dyn-secret")],
            },
        ],
    };
    let shortcut = ShortcutSpec {
        name: "Note".into(),
        enabled: true,
        key: None,
        osc: None,
        action: WireShortcutAction::Actions(vec![text(title, 2, "short-secret")]),
    };
    for (id, payload) in [
        ("set-cues", set_list(list.clone())),
        (
            "set-shortcut",
            CommandPayload::SetShortcut {
                shortcut: 3,
                spec: Box::new(shortcut.clone()),
                position: None,
            },
        ),
        (
            "set-script",
            CommandPayload::SetScript {
                script: 5,
                spec: Box::new(ScriptSpec {
                    name: "Note".into(),
                    source: "title_text \"Lower third\", 2, \"script-secret\"\n".into(),
                }),
                position: None,
            },
        ),
    ] {
        admin.send(&command(id, payload));
        admin.accepted(id);
    }

    // Every record the page gets is kept and scanned.
    let mut seen = Vec::new();
    let (mut page, _, _, _) = web_session(daemon.web, Role::Graphics);
    let WireMessage::CueListState(state) = next_kind(&mut page, &mut seen, "cue_list_state") else {
        unreachable!()
    };
    let web_list = state.list;
    assert_eq!(
        web_list.spec.cues[0].actions,
        [text(title, 1, "Bea"), marker(title, 2)]
    );
    assert_eq!(
        web_list.spec.cues[1].actions,
        [marker(InputTarget::Preview, 2)]
    );
    let WireMessage::ScriptState(script) = next_kind(&mut page, &mut seen, "script_state") else {
        unreachable!()
    };
    assert!(script.script.source_hidden && script.script.spec.source.is_empty());
    assert_eq!(script.script.spec.name, "Note");
    let WireMessage::ShortcutState(state) = next_kind(&mut page, &mut seen, "shortcut_state")
    else {
        unreachable!()
    };
    let web_shortcut = state.shortcut;
    assert_eq!(
        web_shortcut.spec.action,
        WireShortcutAction::Actions(vec![marker(title, 2)])
    );

    // The page's edits round-trip: what it sends back keeps the values.
    let mut renamed = web_list.spec.clone();
    renamed.name = "Renamed".into();
    let mut disabled = web_shortcut.spec.clone();
    disabled.enabled = false;
    assert_eq!(
        scanned_result(&mut page, &mut seen, "page-rename", set_list(renamed)),
        "accepted"
    );
    assert_eq!(
        scanned_result(
            &mut page,
            &mut seen,
            "page-disable",
            CommandPayload::SetShortcut {
                shortcut: 3,
                spec: Box::new(disabled),
                position: None,
            },
        ),
        "accepted"
    );
    let stored_list = loop {
        if let WireMessage::Event(EventMessage {
            payload: EventPayload::CueListChanged(list),
            ..
        }) = admin.receive()
        {
            break *list;
        }
    };
    let mut expected = list.clone();
    expected.name = "Renamed".into();
    assert_eq!(stored_list.spec, expected);
    let stored_shortcut = loop {
        if let WireMessage::Event(EventMessage {
            payload: EventPayload::ShortcutChanged(shortcut),
            ..
        }) = admin.receive()
        {
            break *shortcut;
        }
    };
    assert!(!stored_shortcut.spec.enabled);
    assert_eq!(stored_shortcut.spec.action, shortcut.action);

    // Refused: a value of the page's own for the hidden field, a marker
    // moved elsewhere, and a script edit.
    let mut guess = web_list.spec.clone();
    guess.cues[0].actions[1] = text(title, 2, "guess");
    assert_eq!(
        scanned_result(&mut page, &mut seen, "page-guess", set_list(guess)),
        "permission_denied"
    );
    let mut moved = web_list.spec.clone();
    moved.cues[0].actions.swap(0, 1);
    assert_eq!(
        scanned_result(&mut page, &mut seen, "page-moved", set_list(moved)),
        "invalid_command"
    );
    assert_eq!(
        scanned_result(
            &mut page,
            &mut seen,
            "page-script",
            CommandPayload::SetScript {
                script: 5,
                spec: Box::new(ScriptSpec {
                    name: "Note".into(),
                    source: "cut 2\n".into(),
                }),
                position: None,
            },
        ),
        "permission_denied"
    );
    // Studio has no use for the marker: it stands for nothing there.
    admin.send(&command("studio-marker", set_list(web_list.spec.clone())));
    let refused = loop {
        if let WireMessage::CommandResult(result) = admin.receive() {
            break result;
        }
    };
    assert!(
        matches!(&refused, CommandResult::Rejected { code, .. } if code == "invalid_command"),
        "{refused:?}"
    );

    // Studio exposes the note: the list and the shortcut follow with it.
    let expose = |admin: &mut RawOperator, id: &str, exposed| {
        admin.send(&command(
            id,
            CommandPayload::SetTitleFieldExposure {
                input: input(4),
                field: field(2),
                exposed,
            },
        ));
        admin.accepted(id);
    };
    expose(&mut admin, "studio-expose", true);
    let WireMessage::CueListState(state) = next_kind(&mut page, &mut seen, "cue_list_state") else {
        unreachable!()
    };
    let exposed_list = state.list;
    let WireMessage::ShortcutState(state) = next_kind(&mut page, &mut seen, "shortcut_state")
    else {
        unreachable!()
    };
    // Up to the exposure the page never saw a hidden value.
    let before = seen
        .iter()
        .position(|line| line.contains("\tevent=title_exposure_changed\t"))
        .expect("the exposure change reached the page");
    for line in &seen[..before] {
        for secret in [
            "cue-secret",
            "dyn-secret",
            "short-secret",
            "script-secret",
            "secret-note",
        ] {
            assert!(!carries(line, secret), "{secret} reached the page: {line}");
        }
    }
    assert_eq!(exposed_list.spec, expected);
    assert_eq!(state.shortcut.spec.action, shortcut.action);
    // Hidden again, they follow without it.
    expose(&mut admin, "studio-hide", false);
    let WireMessage::CueListState(state) = next_kind(&mut page, &mut seen, "cue_list_state") else {
        unreachable!()
    };
    assert_eq!(
        state.list.spec.cues[0].actions,
        [text(title, 1, "Bea"), marker(title, 2)]
    );
    let line = seen.last().unwrap();
    assert!(!carries(line, "cue-secret"), "{line}");
}
