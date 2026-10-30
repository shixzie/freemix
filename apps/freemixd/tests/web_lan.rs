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
    ScreenCaptureTargetsRequest, WebAccessAction, WebAccessMessage, WebAccessRefusal,
    WebAccessRequest, WebDeviceRole, WireMessage, decode_line, encode_line,
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
        Self::try_start_with(directory, project, run, extra)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// [`Self::start_with`], reporting a daemon that exits before it is
    /// ready instead of panicking, so a test can skip where the daemon
    /// cannot run at all.
    fn try_start_with(
        directory: &Path,
        project: &Path,
        run: &str,
        extra: &[&str],
    ) -> Result<Self, String> {
        let mut options = vec!["--web-tls"];
        options.extend(extra);
        Self::try_launch(directory, project, run, &options)
    }

    /// A gateway on every interface with `options` choosing its TLS.
    fn launch(directory: &Path, project: &Path, run: &str, extra: &[&str]) -> Self {
        Self::try_launch(directory, project, run, extra).unwrap_or_else(|error| panic!("{error}"))
    }

    fn try_launch(
        directory: &Path,
        project: &Path,
        run: &str,
        extra: &[&str],
    ) -> Result<Self, String> {
        let stdout_path = directory.join(format!("{run}.stdout"));
        let stderr_path = directory.join(format!("{run}.stderr"));
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .args(["--listen", "127.0.0.1:0", "--web-listen", "0.0.0.0:0"])
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
                return Err(format!(
                    "daemon exited early ({status}): {}",
                    fs::read_to_string(&stderr_path).unwrap_or_default()
                ));
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
        Ok(Self {
            child,
            control,
            web,
            url,
            fingerprint,
            stdout_path,
            stderr_path,
        })
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
    // Nor list the server desktop's windows: their titles are for whoever
    // may add a screen capture input, which an Operator device may not.
    phone
        .send(Message::text(
            encode_line(&WireMessage::ScreenCaptureTargetsRequest(
                ScreenCaptureTargetsRequest {
                    request_id: "phone-targets".into(),
                },
            ))
            .unwrap(),
        ))
        .unwrap();
    let refusal = loop {
        let Message::Text(text) = phone.read().unwrap() else {
            continue;
        };
        match decode_line(text.as_str()).unwrap() {
            WireMessage::Error(error) => break error,
            WireMessage::ScreenCaptureTargets(listing) => {
                panic!("an Operator device was shown the desktop: {listing:?}")
            }
            _ => {}
        }
    };
    assert_eq!(refusal.request_id.as_deref(), Some("phone-targets"));
    assert_eq!(refusal.error.code, "permission_denied");
    // The admin console is answered as before.
    console.send(&WireMessage::ScreenCaptureTargetsRequest(
        ScreenCaptureTargetsRequest {
            request_id: "console-targets".into(),
        },
    ));
    loop {
        match console.receive() {
            WireMessage::ScreenCaptureTargets(listing) => {
                assert_eq!(listing.request_id, "console-targets");
                break;
            }
            WireMessage::Error(error) => panic!("the admin console was refused: {error:?}"),
            _ => {}
        }
    }

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
/// `FM_REQUIRE_DISPLAY_SURFACE=1` the presenter must open. Native media
/// needs a GPU adapter even headless, so without one (a bare CI runner)
/// the test is skipped, unless `FM_REQUIRE_NATIVE_MEDIA=1`.
#[cfg(feature = "display-surface")]
#[test]
fn the_windowed_program_daemon_serves_the_gateway() {
    use fm_protocol::DisplayInventoryRequest;
    let directory = TestDirectory::new("windowed");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = match Daemon::try_start_with(
        &directory.0,
        &project,
        "windowed",
        &["--native-media", "--display-presenter-if-available"],
    ) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1")
                    && std::env::var("FM_REQUIRE_DISPLAY_SURFACE").as_deref() != Ok("1"),
                "a native daemon is required: {error}"
            );
            eprintln!("windowed program daemon test skipped: {error}");
            return;
        }
    };
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

/// The TLS readiness record's `name=value` field.
fn readiness_field(daemon: &Daemon, name: &str) -> Option<String> {
    let stdout = fs::read_to_string(&daemon.stdout_path).unwrap();
    let line = stdout
        .lines()
        .find(|line| line.starts_with("FREEMIXD_WEB_TLS\t"))?
        .to_owned();
    line.split('\t')
        .find_map(|field| field.strip_prefix(&format!("{name}=")).map(str::to_owned))
}

/// A client trusting exactly the roots in `roots` (DER), as a phone that
/// installed them does, checking the name like a browser.
fn root_client(roots: &[Vec<u8>]) -> std::sync::Arc<rustls::ClientConfig> {
    let mut store = rustls::RootCertStore::empty();
    for root in roots {
        store
            .add(rustls::pki_types::CertificateDer::from(root.clone()))
            .unwrap();
    }
    std::sync::Arc::new(
        rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(store)
        .with_no_client_auth(),
    )
}

/// RC-007 trust: `--web-tls` issues the gateway's leaf from a per-install
/// local root that phones download from `/freemix-ca.crt` and install once;
/// a client trusting only that root (no fingerprint pin) then reaches the
/// gateway with full certificate and name checks. The root and leaf survive
/// restarts, key files stay private, and no key reaches the logs.
#[test]
fn a_phone_that_installs_the_local_root_trusts_the_gateway() {
    let directory = TestDirectory::new("local-ca");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let mut daemon = Daemon::start_with(&directory.0, &project, "first", &["--no-web-mdns"]);
    assert_eq!(
        readiness_field(&daemon, "trust").as_deref(),
        Some("local_ca")
    );
    let ca_fingerprint = readiness_field(&daemon, "ca_fingerprint").unwrap();
    assert!(fm_protocol::valid_web_fingerprint(&ca_fingerprint));
    assert_ne!(ca_fingerprint, daemon.fingerprint);

    // The root downloads without a credential, as DER, from the pinned
    // gateway (the phone has not installed it yet).
    let host = daemon.web.to_string();
    let (status, headers, root) = web_tls::https(
        daemon.web,
        &format!("GET /freemix-ca.crt HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        TIMEOUT,
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(headers.contains(&("content-type".into(), "application/x-x509-ca-cert".into())));
    assert_eq!(web_tls::fingerprint(&root), ca_fingerprint);

    // The console sees the same trust.
    let mut console = Console::connect(daemon.control, Role::Admin);
    let gateway = console.web_access(WebAccessAction::Status).gateway.unwrap();
    assert_eq!(gateway.trust, fm_protocol::WebTrust::LocalCa);
    assert_eq!(
        gateway.ca_fingerprint.as_deref(),
        Some(ca_fingerprint.as_str())
    );
    assert_eq!(
        gateway.ca_link(),
        Some(format!("{}/freemix-ca.crt", daemon.url))
    );
    assert!(gateway.cert_not_after_ms.is_some());
    assert_eq!(gateway.cert_problem, None, "the leaf names the URL host");
    assert_eq!(gateway.mdns, None, "--no-web-mdns advertises nothing");

    // A phone that trusts only the root opens the start page, which offers
    // the root; another root is refused.
    let trusted = root_client(std::slice::from_ref(&root));
    let mut stream = web_tls::connect_with(daemon.web, TIMEOUT, &trusted).unwrap();
    stream
        .write_all(format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
        .unwrap();
    let mut page = String::new();
    stream.read_to_string(&mut page).unwrap();
    assert!(page.starts_with("HTTP/1.1 200 OK\r\n"), "{page}");
    assert!(page.contains("href=\"/freemix-ca.crt\""), "{page}");
    let stranger = rcgen::CertificateParams::new(Vec::<String>::new())
        .unwrap()
        .self_signed(&rcgen::KeyPair::generate().unwrap())
        .unwrap();
    let untrusted = root_client(&[stranger.der().to_vec()]);
    let mut refused = web_tls::connect_with(daemon.web, TIMEOUT, &untrusted).unwrap();
    assert!(
        refused.write_all(b"GET / HTTP/1.1\r\n\r\n").is_err()
            || refused.read(&mut [0; 16]).is_err(),
        "another root must not vouch for the gateway"
    );

    // Key material is private and never printed.
    #[cfg(unix)]
    let state = directory.0.join("web-state");
    #[cfg(unix)]
    for file in [
        "web-ca-key.pem",
        "web-ca-cert.pem",
        "web-tls-key.pem",
        "web-tls-cert.pem",
    ] {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(state.join(file)).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{file}: {mode:o}");
    }
    let leaf = daemon.fingerprint.clone();
    daemon.stop();
    assert!(!daemon.output().contains("PRIVATE KEY"));

    // A restart keeps the root phones installed and the leaf.
    let again = Daemon::start_with(&directory.0, &project, "second", &["--no-web-mdns"]);
    assert_eq!(
        readiness_field(&again, "ca_fingerprint").as_deref(),
        Some(ca_fingerprint.as_str())
    );
    assert_eq!(again.fingerprint, leaf);
}

fn hsts(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .find(|(name, _)| name == "strict-transport-security")
        .map(|(_, value)| value.as_str())
}

/// RC-007 hardening: HTTPS answers for a DNS name the served certificate
/// names (here a `--web-host` under `home.arpa`, inside the local root's
/// name constraints) carry a one-week HSTS for that host only; answers for
/// an address (which browsers ignore HSTS for) or `localhost` (HSTS would
/// pin every port on this computer) carry none. Pages keep their hardening
/// headers.
#[test]
fn hsts_pins_only_names_the_certificate_covers() {
    let directory = TestDirectory::new("hsts");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let daemon = Daemon::start_with(
        &directory.0,
        &project,
        "hsts",
        &["--no-web-mdns", "--web-host", "studio.home.arpa"],
    );
    let port = daemon.web.port();
    for path in [
        "/",
        "/tally",
        "/operator",
        "/titles",
        "/freemix-ca.crt",
        "/v1/whoami",
    ] {
        let (status, headers, _) = web_tls::https(
            daemon.web,
            &format!("GET {path} HTTP/1.1\r\nHost: studio.home.arpa:{port}\r\n\r\n"),
            TIMEOUT,
        );
        assert!(status.starts_with("HTTP/1.1 "), "{path}: {status}");
        assert_eq!(hsts(&headers), Some("max-age=604800"), "{path}");
    }
    for host in [daemon.web.to_string(), format!("localhost:{port}")] {
        let (status, headers, _) = https(
            &daemon,
            &format!("GET /tally HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert_eq!(status, "HTTP/1.1 200 OK", "{host}");
        assert_eq!(hsts(&headers), None, "{host}");
    }
    // A refused host learns nothing, HSTS included.
    let (status, headers, _) = https(
        &daemon,
        &format!("GET /tally HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n"),
    );
    assert_eq!(status, "HTTP/1.1 421 Misdirected Request");
    assert_eq!(hsts(&headers), None);
    // The pages keep their hardening headers.
    for page in ["/", "/tally", "/operator", "/titles"] {
        let (_, headers, body) = https(
            &daemon,
            &format!("GET {page} HTTP/1.1\r\nHost: studio.home.arpa:{port}\r\n\r\n"),
        );
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_eq!(header("x-content-type-options"), "nosniff", "{page}");
        assert_eq!(header("referrer-policy"), "no-referrer", "{page}");
        assert_eq!(header("x-frame-options"), "DENY", "{page}");
        let policy = header("content-security-policy");
        assert!(
            policy.contains("frame-ancestors 'none'"),
            "{page}: {policy}"
        );
        // Inline page scripts stay allowed by the policy.
        if body.contains("<script") {
            assert!(
                policy.contains("script-src 'unsafe-inline'"),
                "{page}: {policy}"
            );
        }
    }
}

/// An mkcert-like certificate for `names`, signed by a fresh root.
fn operator_certificate(names: &[&str]) -> (String, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let certificate = params.self_signed(&key).unwrap();
    (certificate.pem(), key.serialize_pem())
}

fn write_key(path: &Path, pem: &str) {
    fs::write(path, pem).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// Polls the console until `done` holds for the gateway status.
fn await_status(
    console: &mut Console,
    done: impl Fn(&fm_protocol::WebGatewayStatus) -> bool,
) -> fm_protocol::WebGatewayStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = console.web_access(WebAccessAction::Status).gateway.unwrap();
        if done(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "status never settled: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// RC-007 trust: the operator's own certificate and key are validated
/// before use, reloaded when the files change (no restart), kept when a
/// replacement is broken, and refused when the key is readable by others.
#[test]
#[allow(clippy::too_many_lines)] // one daemon walks load, HSTS, reload, and refusal
fn the_operators_certificate_is_validated_and_hot_reloaded() {
    let directory = TestDirectory::new("operator-cert");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let cert = directory.0.join("studio.pem");
    let key = directory.0.join("studio-key.pem");
    let (first_cert, first_key) = operator_certificate(&["127.0.0.1", "localhost"]);
    fs::write(&cert, &first_cert).unwrap();
    write_key(&key, &first_key);
    let (cert_arg, key_arg) = (cert.to_str().unwrap(), key.to_str().unwrap());
    let options = [
        "--web-tls-cert",
        cert_arg,
        "--web-tls-key",
        key_arg,
        "--no-web-mdns",
        "--web-host",
        "cam.lan",
    ];
    let daemon = Daemon::launch(&directory.0, &project, "operator", &options);
    assert_eq!(
        readiness_field(&daemon, "trust").as_deref(),
        Some("operator")
    );
    assert_eq!(readiness_field(&daemon, "ca_fingerprint"), None);
    let mut console = Console::connect(daemon.control, Role::Admin);
    let status = console.web_access(WebAccessAction::Status).gateway.unwrap();
    assert_eq!(status.trust, fm_protocol::WebTrust::Operator);
    assert_eq!(status.ca_fingerprint, None);
    assert_eq!(
        status.fingerprint.as_deref(),
        Some(daemon.fingerprint.as_str())
    );
    // There is no local root to download.
    let host = daemon.web.to_string();
    let (status_line, _, _) = https(
        &daemon,
        &format!("GET /freemix-ca.crt HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert_eq!(status_line, "HTTP/1.1 404 Not Found");

    // New files are served without a restart.
    let (second_cert, second_key) = operator_certificate(&["127.0.0.1", "localhost", "cam.lan"]);
    write_key(&key, &second_key);
    fs::write(&cert, &second_cert).unwrap();
    let second = web_tls::fingerprint(
        rustls::pki_types::pem::PemObject::from_pem_slice(second_cert.as_bytes())
            .map(|der: rustls::pki_types::CertificateDer<'static>| der.to_vec())
            .unwrap()
            .as_slice(),
    );
    let status = await_status(&mut console, |status| {
        status.fingerprint.as_deref() == Some(second.as_str())
    });
    assert_eq!(status.trust, fm_protocol::WebTrust::Operator);
    web_tls::pin(daemon.web, &second);
    let (status_line, headers, _) = https(
        &daemon,
        &format!("HEAD /tally HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert_eq!(status_line, "HTTP/1.1 200 OK");
    assert_eq!(hsts(&headers), None, "no HSTS for an address");
    // The operator's validated certificate names `cam.lan`: HSTS pins it.
    let port = daemon.web.port();
    let (status_line, headers, _) = https(
        &daemon,
        &format!("HEAD /tally HTTP/1.1\r\nHost: cam.lan:{port}\r\n\r\n"),
    );
    assert_eq!(status_line, "HTTP/1.1 200 OK");
    assert_eq!(hsts(&headers), Some("max-age=604800"));

    // A broken replacement keeps the served certificate and is reported.
    fs::write(
        &cert,
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let status = await_status(&mut console, |status| {
        status.cert_problem == Some(fm_protocol::WebCertProblem::ReloadFailed)
    });
    assert_eq!(status.fingerprint.as_deref(), Some(second.as_str()));
    let (status_line, _, _) = https(
        &daemon,
        &format!("HEAD /tally HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert_eq!(status_line, "HTTP/1.1 200 OK");
    let output = daemon.output();
    assert!(
        output.contains("FREEMIXD_WEB_TLS_RELOAD\tv=1\tstate=ok"),
        "{output}"
    );
    assert!(
        output.contains("FREEMIXD_WEB_TLS_RELOAD\tv=1\tstate=failed"),
        "{output}"
    );
    assert!(!output.contains("PRIVATE KEY"), "{output}");
    drop(daemon);

    // A key other users can read is refused before anything is served.
    fs::write(&cert, &first_cert).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(&project)
            .args(["--listen", "127.0.0.1:0", "--web-listen", "0.0.0.0:0"])
            .args(options)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("other users"), "{stderr}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("FREEMIXD_WEB_READY"));
    }
}

/// RC-007 reach: on a LAN address the gateway advertises its `.local`
/// name over mDNS / DNS-SD and serves requests that name it; the console
/// lists it with the other origins. Where multicast is unavailable (some
/// sandboxes) the daemon says so and serves by address alone.
#[test]
fn the_gateway_advertises_its_mdns_name() {
    let directory = TestDirectory::new("mdns");
    let project = directory.0.join("show.freemix");
    create_project(&project);
    let label = format!("freemix-test-{}", std::process::id());
    let daemon = Daemon::start_with(&directory.0, &project, "mdns", &["--web-mdns-name", &label]);
    let host = format!("{label}.local");
    let deadline = Instant::now() + Duration::from_secs(5);
    let advertised = loop {
        let output = daemon.output();
        if output.contains(&format!(
            "FREEMIXD_WEB_MDNS\tv=1\tstate=advertised\thost={host}"
        )) {
            break true;
        }
        if output.contains("FREEMIXD_WEB_MDNS\tv=1\tstate=unavailable") {
            break false;
        }
        assert!(Instant::now() < deadline, "no mDNS record: {output}");
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut console = Console::connect(daemon.control, Role::Admin);
    let status = console.web_access(WebAccessAction::Status).gateway.unwrap();
    let origin = format!("https://{host}:{}", daemon.web.port());
    let (status_line, _, _) = https(
        &daemon,
        &format!(
            "HEAD / HTTP/1.1\r\nHost: {host}:{}\r\n\r\n",
            daemon.web.port()
        ),
    );
    if advertised {
        assert_eq!(status.mdns.as_deref(), Some(host.as_str()));
        assert_eq!(status.mdns_url(), Some(origin.as_str()));
        assert_eq!(
            readiness_field(&daemon, "mdns").as_deref(),
            Some(host.as_str())
        );
        assert_eq!(status_line, "HTTP/1.1 200 OK");
    } else {
        eprintln!("mDNS unavailable here: checked the fallback");
        assert_eq!(status.mdns, None);
        assert!(!status.urls.contains(&origin));
        assert_eq!(status_line, "HTTP/1.1 421 Misdirected Request");
    }
}
