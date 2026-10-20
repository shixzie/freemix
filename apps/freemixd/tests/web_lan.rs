//! The web gateway on the LAN (RC-007, RC-014, RC-008): plain HTTP is
//! refused on non-loopback addresses, TLS uses a generated certificate kept
//! across restarts, and phones pair with a one-time code for a device token
//! limited to its role, which the console lists and revokes. Pairing is
//! rate-limited, same-origin, and served only under allowed host names, and
//! no token or code ever reaches the daemon's output.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand},
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
    CURRENT_PROTOCOL_VERSION, ClientType, HandshakeOutcome, HandshakeRequest, Role,
    WebAccessAction, WebAccessMessage, WebAccessRefusal, WebAccessRequest, WebDeviceRole,
    WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;
use tungstenite::{Message, WebSocket, http::Request};

#[path = "support/heartbeat.rs"]
mod heartbeat;
use heartbeat::Heartbeat;

mod web_tls;

const PROJECT_ID: u128 = 0x1a40_0000;
const ID_BASE: u128 = 0x1a40_1000;
const TIMEOUT: Duration = Duration::from_secs(3);

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "freemixd-lan-{}-{sequence}-{name}",
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

fn domain_input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(ID_BASE + value).unwrap())
}

fn create_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(PROJECT_ID).unwrap()),
        "LAN Test",
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
                // Planar F32, so the native (windowed) daemon accepts it too.
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        },
    );
    for (number, name) in [(1, "Camera 1"), (2, "Slides")] {
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
    store.save(&stored).unwrap();
}

/// A daemon serving the gateway on every interface over TLS, its output
/// captured to files so the test can prove no secret was written.
struct Daemon {
    child: Child,
    control: SocketAddr,
    /// The gateway, reached on loopback (it listens on 0.0.0.0).
    web: SocketAddr,
    url: String,
    fingerprint: String,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl Daemon {
    fn start(directory: &Path, project: &Path, run: &str) -> Self {
        Self::start_with(directory, project, run, &[])
    }

    /// [`Self::start`] with `extra` daemon options.
    fn start_with(directory: &Path, project: &Path, run: &str, extra: &[&str]) -> Self {
        let stdout_path = directory.join(format!("{run}.stdout"));
        let stderr_path = directory.join(format!("{run}.stderr"));
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .args([
                "--listen",
                "127.0.0.1:0",
                "--web-listen",
                "0.0.0.0:0",
                "--web-tls",
            ])
            .arg("--web-state-dir")
            .arg(directory.join("web-state"))
            .args(extra)
            .env_remove("FREEMIXD_WEB_TOKEN")
            .stdout(fs::File::create(&stdout_path).unwrap())
            .stderr(fs::File::create(&stderr_path).unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let lines = loop {
            let text = fs::read_to_string(&stdout_path).unwrap_or_default();
            let lines: Vec<String> = text.lines().map(str::to_owned).collect();
            if lines.len() >= 3 {
                break lines;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "daemon exited early ({status}): {}",
                    fs::read_to_string(&stderr_path).unwrap_or_default()
                );
            }
            assert!(Instant::now() < deadline, "daemon never became ready");
            std::thread::sleep(Duration::from_millis(20));
        };
        let control = lines[0].parse::<ReadinessRecord>().unwrap().address;
        let bound: SocketAddr = lines[1]
            .strip_prefix("FREEMIXD_WEB_READY\tv=1\taddress=")
            .and_then(|line| line.parse().ok())
            .unwrap_or_else(|| panic!("invalid web readiness {:?}", lines[1]));
        assert!(bound.ip().is_unspecified(), "{bound}");
        let fields: Vec<&str> = lines[2].split('\t').collect();
        assert_eq!(fields[..2], ["FREEMIXD_WEB_TLS", "v=1"], "{:?}", lines[2]);
        let url = fields[2].strip_prefix("url=").unwrap().to_owned();
        let fingerprint = fields[3].strip_prefix("fingerprint=").unwrap().to_owned();
        assert!(url.starts_with("https://"), "{url}");
        assert!(url.ends_with(&format!(":{}", bound.port())), "{url}");
        assert!(fm_protocol::valid_web_fingerprint(&fingerprint));
        let web = SocketAddr::from(([127, 0, 0, 1], bound.port()));
        web_tls::pin(web, &fingerprint);
        Self {
            child,
            control,
            web,
            url,
            fingerprint,
            stdout_path,
            stderr_path,
        }
    }

    /// Everything the daemon printed so far.
    fn output(&self) -> String {
        format!(
            "{}{}",
            fs::read_to_string(&self.stdout_path).unwrap_or_default(),
            fs::read_to_string(&self.stderr_path).unwrap_or_default()
        )
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn hello(role: Role, client_type: ClientType) -> WireMessage {
    WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "freemix-lan-test".into(),
        client_type,
        desired_role: role,
        resume_cursor: None,
    })
}

/// A raw control session (the console's connection).
struct Console {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    heartbeat: Heartbeat,
    next: u64,
}

impl Console {
    fn connect(address: SocketAddr, role: Role) -> Self {
        let stream = TcpStream::connect_timeout(&address, TIMEOUT).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut console = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            heartbeat: Heartbeat::new(),
            next: 0,
        };
        console.send(&hello(role, ClientType::Studio));
        loop {
            if let WireMessage::HandshakeResponse(response) = console.receive() {
                assert_eq!(response.granted_role, role);
                break;
            }
        }
        console
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
    }

    fn receive(&mut self) -> WireMessage {
        loop {
            if let Some(beat) = self.heartbeat.due() {
                self.send(&beat);
            }
            let mut line = String::new();
            assert_ne!(
                self.reader.read_line(&mut line).unwrap(),
                0,
                "console closed"
            );
            let message = decode_line(&line).unwrap();
            if !self.heartbeat.observe(&message) {
                return message;
            }
        }
    }

    fn web_access(&mut self, action: WebAccessAction) -> WebAccessMessage {
        self.next += 1;
        let request_id = format!("wa-{}", self.next);
        self.send(&WireMessage::WebAccessRequest(WebAccessRequest {
            request_id: request_id.clone(),
            action,
        }));
        loop {
            if let WireMessage::WebAccess(answer) = self.receive() {
                assert_eq!(answer.request_id, request_id);
                return answer;
            }
        }
    }
}

fn https(daemon: &Daemon, request: &str) -> (String, Vec<(String, String)>, String) {
    let (status, headers, body) = web_tls::https(daemon.web, request, TIMEOUT);
    (status, headers, String::from_utf8(body).unwrap())
}

fn pair_request(host: &str, origin: Option<&str>, code: &str) -> String {
    let body = format!("code={code}&name=Floor+phone");
    let origin = origin.map_or_else(String::new, |origin| format!("Origin: {origin}\r\n"));
    format!(
        "POST /v1/pair HTTP/1.1\r\nHost: {host}\r\n{origin}Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// The JSON string value of `field` in a flat object.
fn json_field(body: &str, field: &str) -> Option<String> {
    let start = body.find(&format!("\"{field}\":\""))? + field.len() + 4;
    let end = body[start..].find('"')? + start;
    Some(body[start..end].to_owned())
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

type Browser = WebSocket<web_tls::TlsStream>;

/// A browser upgrade from the served page's origin carrying `token`.
fn browser(daemon: &Daemon, token: &str, role: Role) -> Result<(Browser, WireMessage), String> {
    let request = Request::builder()
        .uri(format!("wss://{}/v1/control", daemon.web))
        .header("Host", daemon.web.to_string())
        .header("Origin", format!("https://{}", daemon.web))
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
        .unwrap();
    let stream = web_tls::connect(daemon.web, TIMEOUT).unwrap();
    let (mut socket, _) =
        tungstenite::client::client(request, stream).map_err(|error| error.to_string())?;
    socket
        .send(Message::text(
            encode_line(&hello(role, ClientType::Web)).unwrap(),
        ))
        .unwrap();
    let Message::Text(text) = socket.read().map_err(|error| error.to_string())? else {
        return Err("expected a text record".into());
    };
    Ok((socket, decode_line(text.as_str()).unwrap()))
}

fn granted(answer: &WireMessage, role: Role) -> bool {
    matches!(
        answer,
        WireMessage::HandshakeResponse(response)
            if response.granted_role == role
                && matches!(response.outcome, HandshakeOutcome::Snapshot { .. })
    )
}

#[test]
fn plain_http_on_every_interface_is_refused() {
    let directory = TestDirectory::new("plain");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    for address in ["0.0.0.0:0", "[::]:0"] {
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(&project)
            .args(["--listen", "127.0.0.1:0", "--web-listen", address])
            .env(
                "FREEMIXD_WEB_TOKEN",
                "plain-token-0123456789abcdefghijklmnop",
            )
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{address} was served in the clear"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stdout.contains("FREEMIXD_WEB_READY"),
            "{address} must never listen: {stdout}"
        );
        assert!(stderr.contains("requires TLS"), "{address}: {stderr}");
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one pairing lifecycle end to end
fn a_phone_pairs_over_tls_within_its_role_until_revoked() {
    let directory = TestDirectory::new("pair");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let mut daemon = Daemon::start(&directory.0, &project, "first");
    let host = daemon.web.to_string();
    let origin = format!("https://{host}");

    // TLS: the pinned certificate handshakes; another pin does not.
    let (status, _, _) = https(
        &daemon,
        &format!("HEAD /operator HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    let wrong = web_tls::client_config(&["00"; 32].join(":"));
    let mut stranger = web_tls::connect_with(daemon.web, TIMEOUT, &wrong).unwrap();
    assert!(
        stranger
            .write_all(b"GET /operator HTTP/1.1\r\n\r\n")
            .is_err()
            || stranger.read(&mut [0; 16]).is_err(),
        "an unpinned certificate must fail the handshake"
    );

    // Only an admin console manages web access.
    let mut operator_console = Console::connect(daemon.control, Role::Operator);
    let refused = operator_console.web_access(WebAccessAction::Status);
    assert_eq!(refused.refusal, Some(WebAccessRefusal::PermissionDenied));
    assert_eq!(refused.gateway, None);
    let mut console = Console::connect(daemon.control, Role::Admin);
    let status = console.web_access(WebAccessAction::Status).gateway.unwrap();
    assert!(status.tls);
    assert_eq!(
        status.fingerprint.as_deref(),
        Some(daemon.fingerprint.as_str())
    );
    assert_eq!(status.url, daemon.url);
    assert!(status.pairing.is_none() && status.devices.is_empty());

    let pairing = console
        .web_access(WebAccessAction::Pair {
            role: WebDeviceRole::Operator,
        })
        .gateway
        .unwrap();
    let code = pairing.pairing.as_ref().unwrap().code.clone();
    let link_code = pairing.pairing.as_ref().unwrap().link_code.clone();
    let link = pairing.pairing_link().unwrap();
    assert!(
        link.starts_with(&format!("{}/operator#pair=", daemon.url)),
        "{link}"
    );
    assert!(
        link.ends_with(&daemon.fingerprint.replace(':', "")),
        "{link}"
    );
    let wrong_code = if code == "000000" { "000001" } else { "000000" };

    // Pairing is same-origin, served only under allowed host names.
    let (status, _, _) = https(
        &daemon,
        &pair_request(&host, Some("https://evil.example"), &code),
    );
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    let (status, _, _) = https(&daemon, &pair_request(&host, None, &code));
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    let (status, _, _) = https(
        &daemon,
        &pair_request("evil.example", Some("https://evil.example"), &code),
    );
    assert_eq!(status, "HTTP/1.1 421 Misdirected Request");
    let (status, _, _) = https(
        &daemon,
        &format!(
            "GET /operator HTTP/1.1\r\nHost: rebind.evil.example:{}\r\n\r\n",
            daemon.web.port()
        ),
    );
    assert_eq!(status, "HTTP/1.1 421 Misdirected Request");

    // A wrong code is refused (attempt 1), the right one pairs (attempt 2).
    let (status, _, body) = https(&daemon, &pair_request(&host, Some(&origin), wrong_code));
    assert_eq!(status, "HTTP/1.1 401 Unauthorized", "{body}");
    let (status, headers, body) = https(&daemon, &pair_request(&host, Some(&origin), &code));
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    assert!(headers.contains(&("cache-control".into(), "no-store".into())));
    let token = json_field(&body, "token").unwrap();
    let device = json_field(&body, "device").unwrap();
    assert_eq!(json_field(&body, "role").as_deref(), Some("operator"));
    assert_eq!(json_field(&body, "name").as_deref(), Some("Floor phone"));
    // One use: the code is spent (attempt 3).
    let (status, _, _) = https(&daemon, &pair_request(&host, Some(&origin), &code));
    assert_eq!(status, "HTTP/1.1 410 Gone");

    // The device token opens a session in its own role, never above it.
    let (mut phone, answer) = browser(&daemon, &token, Role::Operator).unwrap();
    assert!(granted(&answer, Role::Operator), "{answer:?}");
    let (_, answer) = browser(&daemon, &token, Role::Admin).unwrap();
    assert!(!granted(&answer, Role::Admin), "{answer:?}");
    let (status, _, body) = https(
        &daemon,
        &format!(
            "GET /v1/whoami HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\r\n"
        ),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        json_field(&body, "device").as_deref(),
        Some(device.as_str())
    );
    // A web session cannot manage web access, even as an Operator device.
    phone
        .send(Message::text(
            encode_line(&WireMessage::WebAccessRequest(WebAccessRequest {
                request_id: "phone-1".into(),
                action: WebAccessAction::Pair {
                    role: WebDeviceRole::Operator,
                },
            }))
            .unwrap(),
        ))
        .unwrap();
    let refusal = loop {
        let Message::Text(text) = phone.read().unwrap() else {
            continue;
        };
        if let WireMessage::WebAccess(answer) = decode_line(text.as_str()).unwrap() {
            break answer;
        }
    };
    assert_eq!(refusal.refusal, Some(WebAccessRefusal::WebSession));
    assert_eq!(refusal.gateway, None);

    // The console lists the device; a cancelled pairing redeems nothing.
    let listed = console.web_access(WebAccessAction::Status).gateway.unwrap();
    assert_eq!(listed.devices.len(), 1);
    assert_eq!(listed.devices[0].id, device);
    assert_eq!(listed.devices[0].role, WebDeviceRole::Operator);
    assert!(listed.devices[0].last_seen_ms.is_some());
    console.web_access(WebAccessAction::Pair {
        role: WebDeviceRole::Viewer,
    });
    let cancelled = console.web_access(WebAccessAction::CancelPairing);
    assert!(cancelled.gateway.unwrap().pairing.is_none());
    // Attempts 4 and 5 find nothing outstanding; attempt 6 is rate limited.
    for _ in 0..2 {
        let (status, _, _) = https(&daemon, &pair_request(&host, Some(&origin), &link_code));
        assert_eq!(status, "HTTP/1.1 410 Gone");
    }
    let (status, headers, body) = https(&daemon, &pair_request(&host, Some(&origin), &link_code));
    assert_eq!(status, "HTTP/1.1 429 Too Many Requests", "{body}");
    assert!(headers.iter().any(|(name, _)| name == "retry-after"));
    assert!(body.contains("\"retry_after_s\":"), "{body}");

    // Revoking ends the live session and every later use of the token.
    let revoked = console.web_access(WebAccessAction::Revoke {
        device: device.clone(),
    });
    assert_eq!(revoked.refusal, None);
    assert_eq!(revoked.gateway.unwrap().devices, []);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match phone.read() {
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => assert!(Instant::now() < deadline, "the revoked session stayed open"),
        }
    }
    assert!(browser(&daemon, &token, Role::Operator).is_err());
    let (status, _, _) = https(
        &daemon,
        &format!(
            "GET /v1/whoami HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\r\n"
        ),
    );
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    let unknown = console.web_access(WebAccessAction::Revoke { device });
    assert_eq!(unknown.refusal, Some(WebAccessRefusal::UnknownDevice));

    // Neither the token nor a code was ever printed, and the store keeps
    // only digests.
    let output = daemon.output();
    for secret in [&token, &code, &link_code] {
        assert!(
            !output.contains(secret.as_str()),
            "a secret reached the log"
        );
    }
    assert!(
        output.contains("FREEMIXD_WEB_PAIRED\tv=1\tdevice="),
        "{output}"
    );
    let store = fs::read_to_string(directory.0.join("web-state").join("web-devices.txt")).unwrap();
    assert!(!store.contains(&token));

    // A restarted daemon keeps its certificate (and so its fingerprint).
    let fingerprint = daemon.fingerprint.clone();
    daemon.stop();
    drop(console);
    drop(operator_console);
    let restarted = Daemon::start(&directory.0, &project, "second");
    assert_eq!(restarted.fingerprint, fingerprint);
}

/// Studio's supervised daemon (`--display-presenter-if-available`) is the
/// windowed program daemon whenever a window system and GPU are there, and
/// it serves the web gateway with the same settings, pairing, and roles as
/// the headless daemon. Without a window system the daemon falls back to
/// headless serving, which must serve the gateway all the same; with
/// `FM_REQUIRE_DISPLAY_SURFACE=1` the presenter must open.
#[cfg(feature = "display-surface")]
#[test]
fn the_windowed_program_daemon_serves_the_gateway() {
    use fm_protocol::DisplayInventoryRequest;
    let directory = TestDirectory::new("windowed");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start_with(
        &directory.0,
        &project,
        "windowed",
        &["--native-media", "--display-presenter-if-available"],
    );
    let mut console = Console::connect(daemon.control, Role::Admin);
    console.send(&WireMessage::DisplayInventoryRequest(
        DisplayInventoryRequest {
            request_id: "presenter".into(),
        },
    ));
    let presenter = loop {
        if let WireMessage::DisplayInventory(reply) = console.receive()
            && reply.request_id == "presenter"
        {
            break reply.inventory.presenter;
        }
    };
    let required = std::env::var("FM_REQUIRE_DISPLAY_SURFACE").as_deref() == Ok("1");
    assert!(presenter || !required, "the presenter did not open");
    if !presenter {
        eprintln!("no window system or GPU: checking the headless fallback's gateway");
    }

    // The pages are served over TLS.
    let host = daemon.web.to_string();
    let (status, _, _) = https(
        &daemon,
        &format!("HEAD /tally HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");

    // Pairing and roles work exactly as on the headless daemon.
    let status = console.web_access(WebAccessAction::Status).gateway.unwrap();
    assert_eq!(
        status.fingerprint.as_deref(),
        Some(daemon.fingerprint.as_str())
    );
    let pairing = console
        .web_access(WebAccessAction::Pair {
            role: WebDeviceRole::Viewer,
        })
        .gateway
        .unwrap();
    let code = pairing.pairing.unwrap().code;
    let origin = format!("https://{host}");
    let (status, _, body) = https(&daemon, &pair_request(&host, Some(&origin), &code));
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let token = json_field(&body, "token").unwrap();
    let (_, answer) = browser(&daemon, &token, Role::Viewer).unwrap();
    assert!(granted(&answer, Role::Viewer), "{answer:?}");
    let (_, answer) = browser(&daemon, &token, Role::Operator).unwrap();
    assert!(!granted(&answer, Role::Operator), "{answer:?}");
    assert!(
        !daemon.output().contains(&token),
        "a token reached the logs"
    );
}
