//! The browser title editor (GX-006) against a native-media daemon: a
//! Graphics page on the plain `--web-listen` gateway uploads an image over
//! the `/v1/assets/images` HTTP API, sets an exposed image field to it while
//! Studio edits another field of the same title revision (per-field
//! revisions, Protocol 2.97), and sees the native renderer draw the upload
//! in its title preview, a draft image before it is saved, the refusal of a
//! stale edit, and a field Studio exposes arriving live in a fresh
//! `title_state` without a reconnect. Uploads the renderer could never draw
//! and framed upload bodies are refused before anything is stored.
//!
//! The test needs a native GPU adapter (a software Vulkan driver such as
//! lavapipe suffices). Without one the daemon refuses to start and the test
//! is skipped, unless `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

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
    Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor, TitleFieldEntry, TitleFieldId, TitleFieldValue as ModelValue,
    TitleInput,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventMessage, EventPayload, HandshakeRequest, Role, TitleAirRequirement, TitleFieldUpdate,
    TitleFieldValue, TitlePreviewRequest, TitlePreviewSample, TitleStateMessage, WireInputId,
    WireMessage, WireTitleFieldId, decode_line, encode_line,
};
use fm_titles::{
    Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldValue,
    ImageFilter, ImageFit, ImageSource, Style, TemplateId, TitleTemplate,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;
use tungstenite::{Message, WebSocket, http::Request};

#[path = "support/heartbeat.rs"]
mod heartbeat;
use heartbeat::Heartbeat;

const TOKEN: &str = "native-web-titles-token-0123456789abcdef";
const TEMPLATE_KEY: &str = "titles/native-web-title.fmtitle";
const RED_KEY: &str = "images/red.png";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const RECORD_TIMEOUT: Duration = Duration::from_secs(15);
const READ_POLL: Duration = Duration::from_millis(50);
const RED: [u8; 4] = [220, 20, 30, 255];
const GREEN: [u8; 4] = [20, 200, 40, 255];

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

fn field(value: u128) -> FieldId {
    FieldId::new(NonZeroU128::new(value).unwrap())
}

fn wire_field(value: u128) -> WireTitleFieldId {
    WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "freemixd-native-web-titles-{}-{name}",
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

/// A `width` x `height` PNG of one color.
fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

/// A 320x180 project: solid generators 1 and 3 on Program and Preview, and
/// title input 2 whose template stretches image field 2 ("Logo", red by
/// default) over the whole frame, with exposed text field 1 ("Name") and
/// text field 3 ("Producer note") hidden from browsers.
#[allow(clippy::too_many_lines)] // one flat project fixture
fn save_project(path: &Path) {
    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::MIN),
        name: "Web title".into(),
        width: 320,
        height: 180,
        background: Color::TRANSPARENT,
        fields: vec![
            FieldDefinition {
                id: field(1),
                name: "Name".into(),
                default: FieldValue::Text("Name".into()),
                exposed: true,
            },
            FieldDefinition {
                id: field(2),
                name: "Logo".into(),
                default: FieldValue::Image(ImageSource::new(format!("asset://{RED_KEY}"))),
                exposed: true,
            },
            FieldDefinition {
                id: field(3),
                name: "Producer note".into(),
                default: FieldValue::Text("Note".into()),
                exposed: false,
            },
        ],
        elements: vec![Element {
            id: ElementId::new(NonZeroU128::MIN),
            name: "Logo".into(),
            bounds: Bounds {
                x: 0,
                y: 0,
                width: 320,
                height: 180,
            },
            z_index: 0,
            visible: true,
            style: Style::default(),
            color_field: None,
            kind: ElementKind::Image {
                field: field(2),
                fit: ImageFit::Stretch,
                filter: ImageFilter::Nearest,
            },
        }],
        animation: fm_titles::TitleAnimation::default(),
    };
    let store = ProjectStore::new(path).unwrap();
    let document = fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
        template,
        fonts: Vec::new(),
        clocks: Vec::new(),
    })
    .unwrap();
    let template_path = store.assets_root().join(TEMPLATE_KEY);
    fs::create_dir_all(template_path.parent().unwrap()).unwrap();
    fs::write(&template_path, document).unwrap();
    let red_path = store.assets_root().join(RED_KEY);
    fs::create_dir_all(red_path.parent().unwrap()).unwrap();
    fs::write(&red_path, png(16, 9, RED)).unwrap();

    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(7_097).unwrap()),
        "Native web titles",
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
    for value in [1, 3] {
        project.add_input(Input {
            id: input(value),
            name: format!("Solid {value}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(12, 34, 56, 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    let entry = |id: u128, value: ModelValue| TitleFieldEntry {
        id: TitleFieldId::new(NonZeroU128::new(id).unwrap()),
        value,
        revision: 0,
    };
    project.add_input(Input {
        id: input(2),
        name: "Web title".into(),
        kind: InputKind::Title(TitleInput {
            template_uri: format!("asset://{TEMPLATE_KEY}"),
            field_revision: 0,
            fields: vec![
                entry(1, ModelValue::Text("Name".into())),
                entry(
                    2,
                    ModelValue::Image {
                        asset_uri: format!("asset://{RED_KEY}"),
                    },
                ),
                entry(3, ModelValue::Text("secret-note".into())),
            ],
        }),
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(input(1), input(3)));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(1)),
            realized_program_id: Some(input(1)),
            desired_preview_id: Some(input(3)),
            realized_preview_id: Some(input(3)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
}

/// A native daemon with the plain Web gateway, killed and reaped on drop.
struct Daemon {
    child: Child,
    control: SocketAddr,
    web: SocketAddr,
    /// The daemon's stderr lines, drained so it never blocks on the pipe.
    log: mpsc::Receiver<String>,
}

impl Daemon {
    /// Starts `freemixd serve --native-media --web-listen`, or returns
    /// `None` when this host has no native media runtime (unless
    /// `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path) -> Option<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .args(["--native-media", "--listen", "127.0.0.1:0"])
            .args(["--web-listen", "127.0.0.1:0"])
            .env("FREEMIXD_WEB_TOKEN", TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (log_sender, log) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = log_sender.send(line);
            }
        });
        let stdout = child.stdout.take().unwrap();
        let (ready_sender, ready) = mpsc::channel();
        thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            for _ in 0..2 {
                let mut line = String::new();
                let _ = stdout.read_line(&mut line);
                let _ = ready_sender.send(line);
            }
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let line = ready.recv_timeout(STARTUP_TIMEOUT).unwrap_or_default();
        if let Ok(readiness) = line.trim_end().parse::<ReadinessRecord>() {
            let web_line = ready.recv_timeout(STARTUP_TIMEOUT).unwrap();
            let web = web_line
                .strip_prefix("FREEMIXD_WEB_READY\tv=1\taddress=")
                .and_then(|line| line.trim_end().parse().ok())
                .unwrap_or_else(|| panic!("invalid web readiness {web_line:?}"));
            return Some(Self {
                child,
                control: readiness.address,
                web,
                log,
            });
        }
        let _ = child.kill();
        let _ = child.wait();
        let detail: Vec<String> = log.try_iter().collect();
        let message = format!("native daemon unavailable: {line:?}; stderr={detail:?}");
        assert!(
            std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
            "FM_REQUIRE_NATIVE_MEDIA=1: {message}"
        );
        eprintln!("native web titles test skipped: {message}");
        None
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let failing = thread::panicking();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if failing {
            while let Ok(line) = self.log.recv_timeout(Duration::from_secs(2)) {
                eprintln!("daemon: {line}");
            }
        }
    }
}

fn hello(client_type: ClientType, role: Role) -> WireMessage {
    WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-web-titles-test".into(),
        client_type,
        desired_role: role,
        resume_cursor: None,
    })
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

fn set_fields(id: &str, base_revision: u64, fields: &[(u128, TitleFieldValue)]) -> WireMessage {
    command(
        id,
        CommandPayload::SetTitleFields {
            input: wire(2),
            base_revision,
            require: TitleAirRequirement::OffAir,
            fields: fields
                .iter()
                .map(|(id, value)| TitleFieldUpdate {
                    field: wire_field(*id),
                    value: value.clone(),
                })
                .collect(),
        },
    )
}

fn image_value(uri: &str) -> TitleFieldValue {
    TitleFieldValue::Image {
        asset_uri: uri.into(),
    }
}

/// One session: a Studio peer on the raw control port or a browser page on
/// the gateway, kept alive with heartbeats.
enum Transport {
    Raw {
        reader: BufReader<TcpStream>,
        writer: TcpStream,
        pending: Vec<u8>,
    },
    Web(Box<WebSocket<TcpStream>>),
}

struct Session {
    transport: Transport,
    heartbeat: Heartbeat,
}

impl Session {
    fn studio(address: SocketAddr) -> (Self, Vec<String>) {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(READ_POLL)).unwrap();
        Self::establish(
            Transport::Raw {
                writer: stream.try_clone().unwrap(),
                reader: BufReader::new(stream),
                pending: Vec::new(),
            },
            &hello(ClientType::Studio, Role::Admin),
        )
    }

    /// A Graphics page and every record of its initial sync, as written,
    /// through the title's `title_state`.
    fn page(address: SocketAddr) -> (Self, Vec<String>) {
        let stream = TcpStream::connect(address).unwrap();
        let request = Request::builder()
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
            .unwrap();
        let (socket, _) = tungstenite::client::client(request, stream).unwrap();
        socket.get_ref().set_read_timeout(Some(READ_POLL)).unwrap();
        Self::establish(
            Transport::Web(Box::new(socket)),
            &hello(ClientType::Web, Role::Graphics),
        )
    }

    fn establish(transport: Transport, hello: &WireMessage) -> (Self, Vec<String>) {
        let mut session = Self {
            transport,
            heartbeat: Heartbeat::new(),
        };
        session.send(hello);
        let mut sync = Vec::new();
        session.until(|line, message| {
            sync.push(line.to_owned());
            matches!(message, WireMessage::TitleState(state) if state.input == wire(2))
                .then_some(())
        });
        let Ok(WireMessage::HandshakeResponse(response)) = decode_line(&sync[0]) else {
            panic!("expected a handshake response: {:?}", sync[0]);
        };
        assert!(
            matches!(
                response.outcome,
                fm_protocol::HandshakeOutcome::Snapshot { .. }
            ),
            "{response:?}"
        );
        (session, sync)
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

    /// The next record as written, or `None` when none arrived in a poll.
    fn read(&mut self) -> Option<String> {
        match &mut self.transport {
            Transport::Raw {
                reader, pending, ..
            } => match reader.read_until(b'\n', pending) {
                Ok(0) => panic!("the daemon closed the session"),
                Ok(_) if !pending.ends_with(b"\n") => None,
                Ok(_) => Some(String::from_utf8(std::mem::take(pending)).unwrap()),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    None
                }
                Err(error) => panic!("{error}"),
            },
            Transport::Web(socket) => match socket.read() {
                Ok(Message::Text(text)) => Some(text.as_str().to_owned()),
                Ok(Message::Ping(_) | Message::Pong(_)) => None,
                Ok(other) => panic!("unexpected WebSocket message {other:?}"),
                Err(tungstenite::Error::Io(error))
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    None
                }
                Err(error) => panic!("{error}"),
            },
        }
    }

    /// Reads (heartbeating) until `found` picks a record.
    fn until<T>(&mut self, mut found: impl FnMut(&str, &WireMessage) -> Option<T>) -> T {
        let deadline = Instant::now() + RECORD_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "the record never arrived");
            if let Some(beat) = self.heartbeat.due() {
                self.send(&beat);
            }
            let Some(line) = self.read() else {
                continue;
            };
            let message = decode_line(&line).unwrap();
            if self.heartbeat.observe(&message) {
                continue;
            }
            if let Some(value) = found(&line, &message) {
                return value;
            }
        }
    }

    /// The result of command `id`.
    fn result(&mut self, id: &str) -> CommandResult {
        self.until(|_, message| match message {
            WireMessage::CommandResult(
                result @ (CommandResult::Accepted { id: answered, .. }
                | CommandResult::Rejected { id: answered, .. }),
            ) if answered == id => Some(result.clone()),
            _ => None,
        })
    }

    fn accepted(&mut self, message: &WireMessage) -> u64 {
        let WireMessage::Command(sent) = message else {
            panic!("not a command");
        };
        let id = sent.id.clone();
        self.send(message);
        match self.result(&id) {
            CommandResult::Accepted { revision, .. } => revision,
            refused @ CommandResult::Rejected { .. } => panic!("{id} was refused: {refused:?}"),
        }
    }

    /// The center pixel of the title preview answering `id`, and its draft
    /// flag.
    fn preview(&mut self, id: &str, fields: &[(u128, TitleFieldValue)]) -> ([u8; 4], bool) {
        self.send(&WireMessage::TitlePreviewRequest(TitlePreviewRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: id.into(),
            input: wire(2),
            sample: TitlePreviewSample::Hold,
            width: 320,
            height: 180,
            clip: None,
            fields: fields
                .iter()
                .map(|(field, value)| TitleFieldUpdate {
                    field: wire_field(*field),
                    value: value.clone(),
                })
                .collect(),
        }));
        let mut data = Vec::new();
        self.until(|line, message| match message {
            WireMessage::TitlePreviewFrame(frame) if frame.request_id == id => {
                data.extend_from_slice(&frame.data);
                (frame.part + 1 == frame.parts).then(|| {
                    let pixels = fm_protocol::decode_qoi_rgba(frame.width, frame.height, &data)
                        .expect("a whole QOI image");
                    let at = (usize::from(frame.height / 2) * usize::from(frame.width)
                        + usize::from(frame.width / 2))
                        * 4;
                    (pixels[at..at + 4].try_into().unwrap(), frame.draft)
                })
            }
            WireMessage::Error(error) if error.request_id.as_deref() == Some(id) => {
                panic!("preview {id} was refused: {line}")
            }
            _ => None,
        })
    }
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

/// One HTTP exchange with the gateway, which closes after its answer: the
/// status line and the body.
fn http(address: SocketAddr, request: &[u8]) -> (String, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(RECORD_TIMEOUT)).unwrap();
    stream.write_all(request).unwrap();
    let mut response = Vec::new();
    // The gateway may close before reading an unread refused body.
    let _ = stream.read_to_end(&mut response);
    let response = String::from_utf8_lossy(&response).into_owned();
    let (head, body) = response
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no HTTP response: {response:?}"));
    (head.lines().next().unwrap().to_owned(), body.to_owned())
}

/// Uploads `bytes` as a PNG named `name`, with `extra` headers.
fn upload(address: SocketAddr, name: &str, extra: &str, bytes: &[u8]) -> (String, String) {
    let mut request = format!(
        "POST /v1/assets/images?name={name} HTTP/1.1\r\nHost: {address}\r\n\
         Authorization: Bearer {TOKEN}\r\nContent-Type: image/png\r\n\
         Content-Length: {}\r\n{extra}\r\n",
        bytes.len()
    )
    .into_bytes();
    request.extend_from_slice(bytes);
    http(address, &request)
}

fn close_to(pixel: [u8; 4], color: [u8; 4]) -> bool {
    pixel.iter().zip(color).all(|(a, b)| a.abs_diff(b) <= 2)
}

#[test]
#[allow(clippy::too_many_lines)] // one browser editing session, in order
fn a_browser_uploads_and_edits_a_native_title_beside_studio() {
    let directory = TestDirectory::new("editor");
    let project = directory.0.join("show.freemix");
    save_project(&project);
    let Some(daemon) = Daemon::start(&project) else {
        return;
    };
    let (mut studio, _) = Session::studio(daemon.control);
    let (mut page, sync) = Session::page(daemon.web);

    // The page holds only the exposed fields, each at revision 0.
    for line in &sync {
        assert!(
            !line.contains("secret-note") && !line.contains("Producer"),
            "a hidden field reached the browser: {line}"
        );
    }
    let WireMessage::TitleState(state) = decode_line(sync.last().unwrap()).unwrap() else {
        unreachable!()
    };
    assert_eq!(state.field_revision, 0);
    let names: Vec<_> = state.fields.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["Name", "Logo"]);
    assert!(state.fields.iter().all(|item| item.revision == 0));
    // The native renderer draws the stored red image.
    let (pixel, draft) = page.preview("initial", &[]);
    assert!(close_to(pixel, RED), "{pixel:?}");
    assert!(!draft);

    // Uploads the renderer could never draw, and framed bodies, are refused;
    // a green picture is stored.
    let (status, body) = upload(daemon.web, "huge.png", "", &png(4_097, 1_024, GREEN));
    assert_eq!(status, "HTTP/1.1 422 Unprocessable Content", "{body}");
    let green = png(32, 18, GREEN);
    let (status, _) = upload(
        daemon.web,
        "framed.png",
        "Transfer-Encoding: chunked\r\n",
        &green,
    );
    assert_eq!(status, "HTTP/1.1 400 Bad Request");
    assert!(
        !project.join("assets").join("uploads").exists()
            || fs::read_dir(project.join("assets").join("uploads"))
                .unwrap()
                .next()
                .is_none(),
        "a refused upload was stored"
    );
    let (status, body) = upload(daemon.web, "green.png", "", &green);
    assert_eq!(status, "HTTP/1.1 201 Created", "{body}");
    let uri = body.split('"').nth(3).unwrap().to_owned();
    assert!(uri.starts_with("asset://uploads/green-"), "{body}");

    // A draft of the upload renders before anything is saved.
    let (pixel, draft) = page.preview("draft", &[(2, image_value(&uri))]);
    assert!(close_to(pixel, GREEN), "{pixel:?}");
    assert!(draft);

    // Both saw revision 0: the page sets the logo, Studio the note.
    assert_eq!(
        page.accepted(&set_fields("page-logo", 0, &[(2, image_value(&uri))])),
        1
    );
    assert_eq!(
        studio.accepted(&set_fields(
            "studio-note",
            0,
            &[(3, TitleFieldValue::Text("cue".into()))]
        )),
        2
    );
    // A stale edit of the logo is refused and changes nothing.
    studio.send(&set_fields(
        "studio-stale-logo",
        0,
        &[(2, image_value(&format!("asset://{RED_KEY}")))],
    ));
    let CommandResult::Rejected { code, fields, .. } = studio.result("studio-stale-logo") else {
        panic!("a stale logo edit was accepted");
    };
    assert_eq!(code, "conflict");
    assert_eq!(fields[0].code, "title_revision_conflict");
    // The page renders its saved upload.
    let (pixel, draft) = page.preview("saved", &[]);
    assert!(close_to(pixel, GREEN), "{pixel:?}");
    assert!(!draft);

    // Studio exposes the note: the page gets a fresh title_state at once.
    studio.accepted(&command(
        "studio-expose",
        CommandPayload::SetTitleFieldExposure {
            input: wire(2),
            field: wire_field(3),
            exposed: true,
        },
    ));
    page.until(|_, message| {
        matches!(
            message,
            WireMessage::Event(EventMessage {
                payload: EventPayload::TitleExposureChanged { .. },
                ..
            })
        )
        .then_some(())
    });
    let state: TitleStateMessage = page.until(|line, message| match message {
        WireMessage::TitleState(state) => Some(state.clone()),
        WireMessage::Event(_) | WireMessage::CommandResult(_) => {
            panic!("expected the title_state after the exposure change: {line}")
        }
        _ => None,
    });
    assert_eq!(state.field_revision, 2);
    let revisions: Vec<_> = state
        .fields
        .iter()
        .map(|item| (item.name.as_str(), item.revision))
        .collect();
    assert_eq!(
        revisions,
        [("Name", 0), ("Logo", 1), ("Producer note", 2)],
        "{state:?}"
    );
    assert_eq!(state.fields[2].value, TitleFieldValue::Text("cue".into()));
}
