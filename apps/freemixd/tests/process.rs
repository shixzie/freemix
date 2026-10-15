use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::{NonZeroU32, NonZeroU128},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use fm_client::{
    Client as ProtocolClient, ClientConfig, ClientError, ConnectionState, Intake, Outbound,
};
#[cfg(unix)]
use fm_client::{DisconnectCause, SessionEvent, TcpSession};
use fm_model::{
    AudioBus, Input, InputAudioStripState, InputGainMilliDb, InputKind, Layer, LayerGeometry,
    MainMix, Output, Project, ProjectSettings, RecorderSlot, RectMask, RestartPolicy, Rgba8,
    Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    StartupPolicy, StreamEndpoint, StreamKey, StreamProtocol, StreamTarget, StreamTargetId,
};
use fm_persistence::{
    ManualTransitionKind as PersistedManualTransitionKind, ProjectPosition, ProjectStore,
    RuntimeRouting, StoredProject,
};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    DiagnosticsRequest, DiagnosticsResponse, EngineIdentity, EventCursor, EventPayload,
    HandshakeOutcome, HandshakeRequest, HeartbeatMessage, ManualTransitionKind,
    ManualTransitionPosition, ManualTransitionStatus, ProtocolVersion, ResumeCursor, Role,
    RuntimeLifecycleEvent, ServerIdentity, SnapshotReason, StingerAudioPolicy,
    StingerMissingMediaFallback, WireInputId, WireMessage, WireStingerSlotId, WireStreamTargetId,
    decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::{ReadinessRecord, StatusReadinessRecord};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const U64_MAX_ID: u128 = 18_446_744_073_709_551_615;
const PROJECT_ID: u128 = U64_MAX_ID + 42;
const INPUT_ID_BASE: u128 = U64_MAX_ID + 100;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("freemixd-{}-{sequence}-{name}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn project_path(&self) -> PathBuf {
        self.0.join("show.freemix")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Daemon {
    child: Option<Child>,
    address: SocketAddr,
    project_id: ProjectId,
}

impl Daemon {
    fn start(project: &Path) -> Self {
        Self::start_with_once(project, true)
    }

    fn start_without_once(project: &Path) -> Self {
        Self::start_with_once(project, false)
    }

    fn start_with_diagnostic_stop_after(project: &Path, duration: &str) -> Self {
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .args(["--diagnostic-stop-after", duration])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = read_startup_lines(&mut child, 1);
        let readiness = match lines[0].parse::<ReadinessRecord>() {
            Ok(readiness) => readiness,
            Err(error) => startup_failure(&mut child, &lines, &error.to_string(), None),
        };
        Self {
            child: Some(child),
            address: readiness.address,
            project_id: readiness.project_id,
        }
    }

    #[cfg(unix)]
    fn start_web(project: &Path, token: &str) -> (Self, SocketAddr) {
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .args(["--listen", "127.0.0.1:0", "--web-listen", "127.0.0.1:0"])
            .env("FREEMIXD_WEB_TOKEN", token)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = read_startup_lines(&mut child, 2);
        let line = lines.remove(0);
        let web_line = lines.remove(0);
        let readiness = line.parse::<ReadinessRecord>().unwrap_or_else(|error| {
            startup_failure(
                &mut child,
                &[line.clone(), web_line.clone()],
                &error.to_string(),
                None,
            )
        });
        let web_address = web_line
            .strip_prefix("FREEMIXD_WEB_READY\tv=1\taddress=")
            .and_then(|line| line.strip_suffix('\n'))
            .and_then(|address| address.parse().ok())
            .unwrap_or_else(|| {
                startup_failure(&mut child, &[line, web_line], "invalid web readiness", None)
            });
        (
            Self {
                child: Some(child),
                address: readiness.address,
                project_id: readiness.project_id,
            },
            web_address,
        )
    }

    fn start_with_status(project: &Path, token: &str, once: bool) -> (Self, SocketAddr) {
        Self::start_with_status_and_web(project, token, once, false)
    }

    /// `web` also starts the WebSocket gateway, whose bounded shutdown keeps the
    /// daemon winding down long enough to observe a readiness transition.
    fn start_with_status_and_web(
        project: &Path,
        token: &str,
        once: bool,
        web: bool,
    ) -> (Self, SocketAddr) {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"));
        command
            .arg("serve")
            .arg(project)
            .args(["--listen", "127.0.0.1:0", "--status-listen", "127.0.0.1:0"])
            .env("FREEMIXD_STATUS_TOKEN", token);
        if once {
            command.arg("--once");
        }
        if web {
            command
                .args(["--web-listen", "127.0.0.1:0"])
                .env("FREEMIXD_WEB_TOKEN", token);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let expected = if web { 3 } else { 2 };
        let mut lines = read_startup_lines(&mut child, expected);
        let line = lines.remove(0);
        let status_line = lines.pop().unwrap();
        let readiness = line.parse::<ReadinessRecord>().unwrap_or_else(|error| {
            startup_failure(
                &mut child,
                &[line.clone(), status_line.clone()],
                &error.to_string(),
                None,
            )
        });
        let status = status_line
            .parse::<StatusReadinessRecord>()
            .unwrap_or_else(|error| {
                startup_failure(
                    &mut child,
                    &[line.clone(), status_line.clone()],
                    &error.to_string(),
                    None,
                )
            });
        (
            Self {
                child: Some(child),
                address: readiness.address,
                project_id: readiness.project_id,
            },
            status.address,
        )
    }

    fn start_with_once(project: &Path, once: bool) -> Self {
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"));
        command.arg("serve").arg(project);
        if once {
            command.arg("--once");
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = read_startup_lines(&mut child, 1);
        let readiness = match lines[0].parse::<ReadinessRecord>() {
            Ok(readiness) => readiness,
            Err(error) => startup_failure(&mut child, &lines, &error.to_string(), None),
        };
        Self {
            child: Some(child),
            address: readiness.address,
            project_id: readiness.project_id,
        }
    }

    fn connect(&self) -> Client {
        Client::connect(self.address)
    }

    fn wait_success(mut self) {
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => terminate_after_wait_failure(
                    &mut child,
                    "daemon did not exit within two seconds",
                ),
                Err(error) => {
                    terminate_after_wait_failure(
                        &mut child,
                        &format!("could not inspect daemon status: {error}"),
                    );
                }
            }
        };
        if !status.success() {
            let stderr = child_stderr(&mut child);
            panic!("daemon exited with {status}: {stderr}");
        }
    }

    fn stop(mut self) {
        let stopped = terminate_child(self.child.as_mut().unwrap());
        assert!(stopped, "daemon did not stop within one second");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = terminate_child(child);
        }
    }
}

fn terminate_child(child: &mut Child) -> bool {
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => return false,
        }
    }
}

fn read_startup_lines(child: &mut Child, count: usize) -> Vec<String> {
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::sync_channel::<std::io::Result<Vec<String>>>(1);
    let reader = std::thread::spawn(move || {
        let mut output = BufReader::new(stdout);
        let result = (|| {
            let mut lines = Vec::with_capacity(count);
            for _ in 0..count {
                let mut line = String::new();
                let read = output.read_line(&mut line).map_err(|error| {
                    std::io::Error::new(error.kind(), format!("{error}; stdout={lines:?}"))
                })?;
                if read == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("stdout ended after {lines:?}"),
                    ));
                }
                lines.push(line);
            }
            Ok(lines)
        })();
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    let result = match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(result) => result,
        Err(error) => startup_failure(child, &[], &error.to_string(), Some(reader)),
    };
    let lines = match result {
        Ok(lines) => lines,
        Err(error) => startup_failure(child, &[], &error.to_string(), Some(reader)),
    };
    if reader.join().is_err() {
        startup_failure(child, &lines, "startup stdout reader panicked", None);
    }
    lines
}

fn startup_failure(
    child: &mut Child,
    stdout: &[String],
    failure: &str,
    reader: Option<std::thread::JoinHandle<()>>,
) -> ! {
    let stopped = terminate_child(child);
    if stopped && let Some(reader) = reader {
        let _ = reader.join();
    }
    let stderr = stopped.then(|| child_stderr(child));
    panic!(
        "daemon startup failed: {failure}; stdout={stdout:?}; cleanup_stopped={stopped}; stderr={stderr:?}"
    );
}

fn terminate_after_wait_failure(child: &mut Child, failure: &str) -> ! {
    let stopped = terminate_child(child);
    let stderr = stopped.then(|| child_stderr(child));
    panic!("{failure}; cleanup_stopped={stopped}; stderr={stderr:?}");
}

fn child_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    stderr
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

struct TestHandshake {
    protocol: ProtocolVersion,
    engine: EngineIdentity,
    current_revision: u64,
    resume: bool,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        }
    }

    fn send(&mut self, message: &WireMessage) {
        let line = encode_line(message).unwrap();
        self.writer.write_all(line.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    fn receive(&mut self) -> WireMessage {
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed TCP stream"
        );
        decode_line(&line).unwrap()
    }

    fn handshake(&mut self, cursor: Option<EventCursor>) -> TestHandshake {
        self.handshake_as(Role::Operator, cursor)
    }

    fn handshake_as(&mut self, role: Role, cursor: Option<EventCursor>) -> TestHandshake {
        self.handshake_version_as(CURRENT_PROTOCOL_VERSION, role, cursor)
    }

    fn handshake_version(
        &mut self,
        version: ProtocolVersion,
        cursor: Option<EventCursor>,
    ) -> TestHandshake {
        self.handshake_version_as(version, Role::Operator, cursor)
    }

    fn handshake_version_as(
        &mut self,
        version: ProtocolVersion,
        role: Role,
        cursor: Option<EventCursor>,
    ) -> TestHandshake {
        let resume_cursor = cursor.map(|cursor| ResumeCursor {
            server: ServerIdentity {
                engine_id: cursor.engine.engine_id,
                project_id: PROJECT_ID.to_string(),
                state_epoch: cursor.engine.state_epoch,
                log_id: cursor.engine.log_id,
            },
            revision: cursor.revision,
        });
        self.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: version,
            build: "process-test".into(),
            client_type: ClientType::Integration,
            desired_role: role,
            resume_cursor,
        }));
        let WireMessage::HandshakeResponse(response) = self.receive() else {
            panic!("expected handshake response");
        };
        let resume = matches!(response.outcome, HandshakeOutcome::Resume { .. });
        TestHandshake {
            protocol: response.protocol,
            engine: EngineIdentity {
                engine_id: response.server.engine_id,
                state_epoch: response.server.state_epoch,
                log_id: response.server.log_id,
            },
            current_revision: response.current_revision,
            resume,
        }
    }

    fn next_result(&mut self) -> CommandResult {
        loop {
            if let WireMessage::CommandResult(result) = self.receive() {
                return result;
            }
        }
    }
}

fn connect_current_client(daemon: &Daemon, client: &mut ProtocolClient) -> Client {
    let mut transport = daemon.connect();
    transport
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    transport.send(&WireMessage::HandshakeRequest(
        client.transport_connected().unwrap(),
    ));
    assert_eq!(
        client.intake(transport.receive()).unwrap(),
        Intake::Handshake
    );
    assert_eq!(
        client.intake(transport.receive()).unwrap(),
        Intake::SnapshotApplied
    );
    assert_eq!(client.state(), &ConnectionState::Ready);
    transport
}

fn assert_heartbeat(client: &mut ProtocolClient, transport: &mut Client, sent_at_ms: u64) {
    let cursor = client.last_applied_cursor();
    let heartbeat = client.queue_heartbeat(sent_at_ms).unwrap();
    assert_eq!(heartbeat.last_applied, cursor);
    assert_eq!(
        client.pop_outbound(),
        Some(Outbound::Heartbeat(heartbeat.clone()))
    );
    transport.send(&WireMessage::Heartbeat(heartbeat.clone()));
    let WireMessage::HeartbeatAcknowledgement(acknowledgement) = transport.receive() else {
        panic!("expected heartbeat acknowledgement");
    };
    assert_eq!(acknowledgement.server, heartbeat.server);
    assert_eq!(acknowledgement.heartbeat_sequence, heartbeat.sequence);
    assert_eq!(
        client
            .intake(WireMessage::HeartbeatAcknowledgement(acknowledgement))
            .unwrap(),
        Intake::HeartbeatAcknowledged
    );
}

fn assert_fade_runtime_round_trip(protocol_client: &mut ProtocolClient, transport: &mut Client) {
    let command = protocol_client
        .queue_command(
            CommandPayload::Fade { duration_frames: 4 },
            "current-fade-key",
            Some(0),
            None,
        )
        .unwrap();
    assert_eq!(
        protocol_client.pop_outbound(),
        Some(Outbound::Command(command.clone()))
    );
    transport.send(&WireMessage::Command(command));

    let result = transport.receive();
    assert!(matches!(
        result,
        WireMessage::CommandResult(CommandResult::Accepted { revision: 1, .. })
    ));
    assert_eq!(
        protocol_client.intake(result).unwrap(),
        Intake::ResultReconciled
    );
    let event = transport.receive();
    let WireMessage::Event(durable_event) = &event else {
        panic!("expected durable event after command result");
    };
    let durable_revision = durable_event.cursor.revision;
    assert_eq!(durable_revision, 1);
    assert_eq!(protocol_client.intake(event).unwrap(), Intake::EventApplied);
    let durable_cursor = protocol_client.last_applied_cursor().unwrap();

    let runtime_event = transport.receive();
    let WireMessage::RuntimeEvent(runtime) = &runtime_event else {
        panic!("expected runtime event after durable event");
    };
    assert_eq!(runtime.revision, durable_revision);
    assert_eq!(runtime.generation, 1);
    assert_eq!(runtime.sequence, 1);
    assert!(matches!(
        &runtime.event,
        RuntimeLifecycleEvent::Realized { domain, .. } if domain == "switcher"
    ));
    assert_eq!(
        protocol_client.intake(runtime_event).unwrap(),
        Intake::RuntimeEventObserved
    );
    assert_eq!(protocol_client.last_applied_cursor(), Some(durable_cursor));
    let switcher = protocol_client.model().state().unwrap().switcher();
    assert_eq!(switcher.realized.program, input(2).to_domain());
    assert_eq!(switcher.realized.preview, input(1).to_domain());
    assert_eq!(switcher.runtime_generation, Some(1));
}

#[test]
fn daemon_acknowledges_only_valid_heartbeats() {
    let directory = TestDirectory::new("heartbeat-acknowledgement");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    let handshake = client.handshake(None);
    assert_eq!(handshake.protocol, CURRENT_PROTOCOL_VERSION);
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));
    let server = ServerIdentity {
        engine_id: handshake.engine.engine_id,
        project_id: PROJECT_ID.to_string(),
        state_epoch: handshake.engine.state_epoch,
        log_id: handshake.engine.log_id,
    };
    let heartbeat = |sequence| HeartbeatMessage {
        server: server.clone(),
        sequence,
        sent_at_ms: 1_234,
        last_applied: Some(ResumeCursor {
            server: server.clone(),
            revision: handshake.current_revision,
        }),
    };

    client.send(&WireMessage::Heartbeat(heartbeat(1)));
    let WireMessage::HeartbeatAcknowledgement(acknowledgement) = client.receive() else {
        panic!("expected heartbeat acknowledgement");
    };
    assert_eq!(acknowledgement.server, server);
    assert_eq!(acknowledgement.heartbeat_sequence, 1);
    assert!(acknowledgement.received_at_ms > 0);

    let mut invalid = heartbeat(2);
    invalid.server.engine_id = "wrong-engine".to_owned();
    invalid.last_applied.as_mut().unwrap().server.engine_id = "wrong-engine".to_owned();
    client.send(&WireMessage::Heartbeat(invalid));
    let WireMessage::Error(error) = client.receive() else {
        panic!("expected invalid heartbeat error");
    };
    assert_eq!(error.error.code, "invalid_heartbeat");

    client.send(&WireMessage::Heartbeat(heartbeat(3)));
    let WireMessage::HeartbeatAcknowledgement(acknowledgement) = client.receive() else {
        panic!("invalid heartbeat produced an acknowledgement");
    };
    assert_eq!(acknowledgement.heartbeat_sequence, 3);

    drop(client);
    daemon.wait_success();
}

const STATUS_TOKEN: &str = "status-token-0123456789abcdef0123456789";

/// Sends one raw HTTP/1.1 request and returns the whole response the listener
/// wrote before closing. Write failures are tolerated so a rejected request
/// still surfaces the daemon's response instead of a client-side panic.
fn status_request(address: SocketAddr, request: &[u8]) -> String {
    try_status_request(address, request).unwrap()
}

/// The same request, tolerating a refused connection so a probe loop can keep
/// running across a daemon that is on its way out.
fn try_status_request(address: SocketAddr, request: &[u8]) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1)).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let _ = stream.write_all(request);
    let _ = stream.flush();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    Some(String::from_utf8_lossy(&response).into_owned())
}

/// Request shapes a supervisor or a scanner actually sends: a HEAD probe, an
/// unsupported method, an absent route, a head past the cap, and a protected
/// route probed without credentials.
fn assert_status_request_shapes(status_address: SocketAddr) {
    // HEAD-default supervisors get the same headers a GET would, and no body.
    let probed = status_request(
        status_address,
        b"HEAD /healthz HTTP/1.1\r\nHost: status\r\n\r\n",
    );
    assert!(probed.starts_with("HTTP/1.1 200 OK\r\n"), "{probed}");
    assert!(probed.contains("Content-Length: "), "{probed}");
    assert!(!probed.contains("check=healthz"), "{probed}");

    let unsupported = status_request(
        status_address,
        b"POST /healthz HTTP/1.1\r\nHost: status\r\n\r\n",
    );
    assert!(
        unsupported.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"),
        "{unsupported}"
    );

    let unknown = status_request(
        status_address,
        b"GET /metrics HTTP/1.1\r\nHost: status\r\n\r\n",
    );
    assert!(
        unknown.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "{unknown}"
    );

    let mut oversized = String::from("GET /healthz HTTP/1.1\r\nHost: status\r\n");
    while oversized.len() < 8 * 1024 {
        oversized.push_str("X-Padding: 0123456789012345678901234567890123456789\r\n");
    }
    oversized.push_str("\r\n");
    let rejected = status_request(status_address, oversized.as_bytes());
    assert!(
        rejected.starts_with("HTTP/1.1 413 Content Too Large\r\n"),
        "{rejected}"
    );
    assert!(
        rejected.contains("check=request\tstatus=too-large\tlimit_bytes=4096"),
        "{rejected}"
    );

    // An unauthenticated caller must not be able to confirm the protected route
    // exists by probing it with a method the route does not serve.
    let probed_route = status_request(
        status_address,
        b"POST /v1/support-bundle HTTP/1.1\r\nHost: status\r\n\r\n",
    );
    assert!(
        probed_route.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "{probed_route}"
    );
}

/// RFC 7235 makes the scheme case-insensitive; the token itself stays exact.
fn assert_bundle_authentication_scheme(status_address: SocketAddr) {
    let lowercase_scheme = status_request(
        status_address,
        format!(
            "GET /v1/support-bundle HTTP/1.1\r\nHost: status\r\nAuthorization: bEaReR   {STATUS_TOKEN}\r\n\r\n"
        )
        .as_bytes(),
    );
    assert!(
        lowercase_scheme.starts_with("HTTP/1.1 200 OK\r\n"),
        "{lowercase_scheme}"
    );

    let mixed_case_token = status_request(
        status_address,
        format!(
            "GET /v1/support-bundle HTTP/1.1\r\nHost: status\r\nAuthorization: Bearer {}\r\n\r\n",
            STATUS_TOKEN.to_uppercase()
        )
        .as_bytes(),
    );
    assert!(
        mixed_case_token.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "{mixed_case_token}"
    );
}

#[test]
fn status_listener_serves_health_readiness_and_guarded_support_bundle() {
    let directory = TestDirectory::new("status-listener");
    let project_path = directory.project_path();
    create_project(&project_path);

    let (daemon, status_address) = Daemon::start_with_status(&project_path, STATUS_TOKEN, false);

    let health = status_request(
        status_address,
        b"GET /healthz HTTP/1.1\r\nHost: status\r\n\r\n",
    );
    assert!(health.starts_with("HTTP/1.1 200 OK\r\n"), "{health}");
    assert!(
        health.contains("FREEMIXD_STATUS\tv=1\tcheck=healthz\tstatus=live\t"),
        "{health}"
    );

    let ready = status_request(
        status_address,
        b"GET /readyz HTTP/1.1\r\nHost: status\r\n\r\n",
    );
    assert!(ready.starts_with("HTTP/1.1 200 OK\r\n"), "{ready}");
    assert!(
        ready.contains(
            "check=readyz\tstatus=ready\treadiness=ready\thealth=healthy\tliveness=live\t"
        ),
        "{ready}"
    );

    assert_status_request_shapes(status_address);

    let anonymous = status_request(
        status_address,
        b"GET /v1/support-bundle HTTP/1.1\r\nHost: status\r\n\r\n",
    );
    assert!(
        anonymous.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "{anonymous}"
    );
    assert!(!anonymous.contains(STATUS_TOKEN), "{anonymous}");

    let wrong = status_request(
        status_address,
        b"GET /v1/support-bundle HTTP/1.1\r\nHost: status\r\nAuthorization: Bearer status-token-0123456789abcdef012345678\r\n\r\n",
    );
    assert!(
        wrong.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "{wrong}"
    );

    let bundle = status_request(
        status_address,
        format!(
            "GET /v1/support-bundle HTTP/1.1\r\nHost: status\r\nAuthorization: Bearer {STATUS_TOKEN}\r\n\r\n"
        )
        .as_bytes(),
    );
    assert!(bundle.starts_with("HTTP/1.1 200 OK\r\n"), "{bundle}");
    assert!(
        bundle.contains("check=support-bundle\tstatus=ok\t"),
        "{bundle}"
    );
    assert!(bundle.contains("\"schema\":\"fm-support-v1\""), "{bundle}");
    assert_bundle_authentication_scheme(status_address);
    assert!(!bundle.contains(STATUS_TOKEN), "{bundle}");
    assert!(
        !bundle.contains(&project_path.display().to_string()),
        "{bundle}"
    );
    assert!(!bundle.contains(&status_address.to_string()), "{bundle}");

    daemon.stop();
}

/// Liveness and readiness are answered by the accept thread, so connections
/// that occupy the listener without ever sending a request must not be able to
/// delay or fail a supervisor probe. Sixteen of forty probes failed here before
/// the accept thread stopped handing probe sockets to the request workers.
#[test]
fn status_listener_answers_probes_while_silent_connections_flood_it() {
    const HELD: usize = 50;
    const FLOOD_THREADS: usize = 4;
    const PROBES: usize = 40;

    let directory = TestDirectory::new("status-listener-flood");
    let project_path = directory.project_path();
    create_project(&project_path);

    let (daemon, status_address) = Daemon::start_with_status(&project_path, STATUS_TOKEN, true);

    let held: Vec<TcpStream> = (0..HELD)
        .filter_map(|_| TcpStream::connect_timeout(&status_address, Duration::from_secs(1)).ok())
        .collect();
    assert_eq!(held.len(), HELD, "could not open the silent connections");

    let flooding = Arc::new(AtomicBool::new(true));
    let threads: Vec<_> = (0..FLOOD_THREADS)
        .map(|_| {
            let flooding = Arc::clone(&flooding);
            std::thread::spawn(move || {
                let mut sockets = Vec::new();
                while flooding.load(Ordering::Acquire) {
                    if let Ok(socket) =
                        TcpStream::connect_timeout(&status_address, Duration::from_secs(1))
                    {
                        sockets.push(socket);
                    }
                    if sockets.len() >= 16 {
                        sockets.clear();
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        })
        .collect();

    let mut answered = 0_usize;
    let mut slowest = Duration::ZERO;
    let mut failure = String::new();
    for _ in 0..PROBES {
        let started = Instant::now();
        let health = status_request(
            status_address,
            b"GET /healthz HTTP/1.1\r\nHost: status\r\n\r\n",
        );
        slowest = slowest.max(started.elapsed());
        if health.starts_with("HTTP/1.1 200 OK\r\n") {
            answered += 1;
        } else if failure.is_empty() {
            failure = health;
        }
    }

    flooding.store(false, Ordering::Release);
    for thread in threads {
        thread.join().unwrap();
    }
    drop(held);

    assert_eq!(
        answered, PROBES,
        "flooded listener failed a probe: {failure}"
    );
    assert!(
        slowest < Duration::from_millis(250),
        "slowest probe under flood took {slowest:?}"
    );

    let mut client = daemon.connect();
    client.handshake(None);
    drop(client);
    daemon.wait_success();
}

/// The published readiness must track the daemon, not the value startup wrote.
///
/// SIGTERM moves the daemon from ready to draining while the status listener is
/// still up, which is exactly the interval a supervisor has to be able to tell
/// apart from a crash. The gateway is enabled so the daemon spends a bounded
/// but measurable interval winding its other subsystems down; probing across
/// the signal must observe `readiness=draining`, which a readiness frozen at
/// startup never reports.
#[cfg(unix)]
#[test]
fn readiness_reports_draining_after_a_cooperative_stop_request() {
    let directory = TestDirectory::new("status-listener-draining");
    let project_path = directory.project_path();
    create_project(&project_path);

    let (daemon, status_address) =
        Daemon::start_with_status_and_web(&project_path, STATUS_TOKEN, false, true);
    let ready = status_request(
        status_address,
        b"GET /readyz HTTP/1.1\r\nHost: status\r\n\r\n",
    );
    assert!(ready.contains("status=ready\treadiness=ready\t"), "{ready}");

    let probing = Arc::new(AtomicBool::new(true));
    let drained = Arc::new(AtomicBool::new(false));
    let probes: Vec<_> = (0..8)
        .map(|_| {
            let probing = Arc::clone(&probing);
            let drained = Arc::clone(&drained);
            std::thread::spawn(move || {
                while probing.load(Ordering::Acquire) {
                    let Some(readiness) = try_status_request(
                        status_address,
                        b"GET /readyz HTTP/1.1\r\nHost: status\r\n\r\n",
                    ) else {
                        continue;
                    };
                    if readiness.contains("status=not-ready\treadiness=draining\t") {
                        assert!(
                            readiness.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
                            "{readiness}"
                        );
                        drained.store(true, Ordering::Release);
                    }
                }
            })
        })
        .collect();

    let status = ProcessCommand::new("/bin/kill")
        .args(["-TERM", &daemon.child.as_ref().unwrap().id().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "SIGTERM command failed: {status}");
    daemon.wait_success();

    probing.store(false, Ordering::Release);
    for probe in probes {
        probe.join().unwrap();
    }
    assert!(
        drained.load(Ordering::Acquire),
        "/readyz never reported the draining transition"
    );
}

#[test]
fn simulated_diagnostic_deadline_exits_cleanly() {
    let directory = TestDirectory::new("simulated-diagnostic-deadline");
    let project_path = directory.project_path();
    create_project(&project_path);

    Daemon::start_with_diagnostic_stop_after(&project_path, "50ms").wait_success();
}

#[test]
fn once_daemon_replaces_disconnected_peer_but_admits_one_live_peer() {
    let directory = TestDirectory::new("once-single-live-peer");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start(&project_path);
    drop(TcpStream::connect(daemon.address).unwrap());

    let mut first = daemon.connect();
    first
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut second = daemon.connect();
    second
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    second.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "process-test".into(),
        client_type: ClientType::Integration,
        desired_role: Role::Operator,
        resume_cursor: None,
    }));

    first.handshake(None);
    assert!(matches!(first.receive(), WireMessage::Snapshot(_)));
    drop(first);

    daemon.wait_success();

    let mut line = String::new();
    match second.reader.read_line(&mut line) {
        Ok(0) => {}
        Ok(_) => panic!("second live peer received a daemon response: {line}"),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
            ) => {}
        Err(error) => panic!("could not inspect second live peer: {error}"),
    }
}

#[test]
fn malformed_post_handshake_record_does_not_stop_daemon() {
    let directory = TestDirectory::new("malformed-post-handshake-record");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start_without_once(&project_path);
    let mut malformed_client = daemon.connect();
    malformed_client
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    malformed_client.handshake(None);
    assert!(matches!(
        malformed_client.receive(),
        WireMessage::Snapshot(_)
    ));
    malformed_client
        .writer
        .write_all(b"malformed-record\n")
        .unwrap();
    malformed_client.writer.flush().unwrap();

    let mut closed = String::new();
    assert_eq!(malformed_client.reader.read_line(&mut closed).unwrap(), 0);

    let mut next_client = daemon.connect();
    next_client
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    next_client.handshake(None);
    assert!(matches!(next_client.receive(), WireMessage::Snapshot(_)));

    drop(next_client);
    drop(malformed_client);
    daemon.stop();
}

/// Regression: a command whose staged result the project schema refuses
/// (a blank idempotency key, which no persisted receipt may carry) stopped the
/// whole daemon. It is answered with a rejection and the same session, and
/// the daemon, keep serving commands.
#[test]
fn a_command_that_cannot_be_persisted_is_rejected_and_the_daemon_keeps_serving() {
    let directory = TestDirectory::new("unpersistable-command");
    let project_path = directory.project_path();
    create_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);
    let mut client = daemon.connect();
    client
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client.handshake(None);
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    client.send(&command("blank-key", "   ", CommandPayload::Cut));
    assert!(matches!(
        client.next_result(),
        CommandResult::Rejected {
            id,
            code,
            current_revision: 0,
            retryable: false,
            ..
        } if id == "blank-key" && code == "invalid_command"
    ));

    client.send(&command("after", "after-key", CommandPayload::Cut));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { id, revision: 1, .. } if id == "after"
    ));
    drop(client);
    daemon.stop();
}

#[test]
fn idle_viewer_receives_operator_events_without_blocking() {
    let directory = TestDirectory::new("idle-viewer-live-events");
    let project_path = directory.project_path();
    create_project(&project_path);

    let viewer_id = "idle-viewer";
    let operator_id = "show-operator";
    assert_ne!(viewer_id, operator_id);
    let mut viewer_client = ProtocolClient::new(ClientConfig::new(
        "process-test",
        ClientType::Integration,
        Role::Viewer,
        viewer_id,
        project_id(),
    ))
    .unwrap();
    let mut operator_client = ProtocolClient::new(ClientConfig::new(
        "process-test",
        ClientType::Integration,
        Role::Operator,
        operator_id,
        project_id(),
    ))
    .unwrap();
    viewer_client.start_connect().unwrap();

    let daemon = Daemon::start_without_once(&project_path);
    let mut viewer = connect_current_client(&daemon, &mut viewer_client);
    assert_eq!(viewer_client.session().unwrap().granted_role, Role::Viewer);

    operator_client.start_connect().unwrap();
    let mut operator = connect_current_client(&daemon, &mut operator_client);
    assert_eq!(
        operator_client.session().unwrap().granted_role,
        Role::Operator
    );

    let command = operator_client
        .queue_command(CommandPayload::Cut, "two-peer-cut", Some(0), None)
        .unwrap();
    assert_eq!(command.id, "show-operator:1");
    assert_eq!(
        operator_client.pop_outbound(),
        Some(Outbound::Command(command.clone()))
    );
    operator.send(&WireMessage::Command(command.clone()));

    let result = operator.receive();
    let WireMessage::CommandResult(CommandResult::Accepted { id, revision, .. }) = &result else {
        panic!("expected accepted command result");
    };
    assert_eq!(id, &command.id);
    assert_eq!(*revision, 1);
    assert_eq!(
        operator_client.intake(result).unwrap(),
        Intake::ResultReconciled
    );

    let operator_event = operator.receive();
    let WireMessage::Event(event) = &operator_event else {
        panic!("expected durable event after command result");
    };
    assert_eq!(event.cursor.revision, 1);
    assert!(matches!(
        &event.payload,
        fm_protocol::EventPayload::DesiredSwitcher {
            program,
            preview,
            ..
        } if *program == input(2) && *preview == input(1)
    ));
    assert_eq!(
        operator_client.intake(operator_event.clone()).unwrap(),
        Intake::EventApplied
    );
    let operator_runtime = operator.receive();
    let WireMessage::RuntimeEvent(runtime) = &operator_runtime else {
        panic!("expected runtime event after durable event");
    };
    assert_eq!(runtime.revision, 1);
    assert!(matches!(
        &runtime.event,
        RuntimeLifecycleEvent::Realized { domain, .. } if domain == "switcher"
    ));
    assert_eq!(
        operator_client.intake(operator_runtime.clone()).unwrap(),
        Intake::RuntimeEventObserved
    );

    let viewer_event = viewer.receive();
    assert_eq!(viewer_event, operator_event);
    assert_eq!(
        viewer_client.intake(viewer_event).unwrap(),
        Intake::EventApplied
    );
    let viewer_runtime = viewer.receive();
    assert_eq!(viewer_runtime, operator_runtime);
    assert_eq!(
        viewer_client.intake(viewer_runtime).unwrap(),
        Intake::RuntimeEventObserved
    );

    assert_heartbeat(&mut viewer_client, &mut viewer, 1_234);
    assert_heartbeat(&mut operator_client, &mut operator, 1_235);

    drop(operator);
    drop(viewer);
    daemon.stop();
}

#[test]
fn current_client_handshake_heartbeat_and_resume_use_ordered_wire_records() {
    let directory = TestDirectory::new("current-client");
    let project_path = directory.project_path();
    create_project(&project_path);

    let mut protocol_client = ProtocolClient::new(ClientConfig::new(
        "process-test",
        ClientType::Integration,
        Role::Operator,
        "process-client",
        project_id(),
    ))
    .unwrap();
    protocol_client.start_connect().unwrap();

    let daemon = Daemon::start(&project_path);
    assert_eq!(daemon.project_id, project_id());
    let mut transport = daemon.connect();
    let request = protocol_client.transport_connected().unwrap();
    assert_eq!(request.resume_cursor, None);
    transport.send(&WireMessage::HandshakeRequest(request));

    let WireMessage::HandshakeResponse(response) = transport.receive() else {
        panic!("expected current handshake response");
    };
    assert_eq!(
        response.outcome,
        HandshakeOutcome::Snapshot {
            reason: SnapshotReason::NoCursor
        }
    );
    assert_eq!(response.server.project_id, PROJECT_ID.to_string());
    assert_eq!(
        protocol_client
            .intake(WireMessage::HandshakeResponse(response))
            .unwrap(),
        Intake::Handshake
    );
    let snapshot = transport.receive();
    assert!(matches!(snapshot, WireMessage::Snapshot(_)));
    assert_eq!(
        protocol_client.intake(snapshot).unwrap(),
        Intake::SnapshotApplied
    );
    assert_eq!(protocol_client.state(), &ConnectionState::Ready);

    let heartbeat = protocol_client.queue_heartbeat(1_234).unwrap();
    assert!(heartbeat.last_applied.is_some());
    assert_eq!(
        protocol_client.pop_outbound(),
        Some(Outbound::Heartbeat(heartbeat.clone()))
    );
    transport.send(&WireMessage::Heartbeat(heartbeat));
    let acknowledgement = transport.receive();
    assert!(matches!(
        acknowledgement,
        WireMessage::HeartbeatAcknowledgement(_)
    ));
    assert_eq!(
        protocol_client.intake(acknowledgement).unwrap(),
        Intake::HeartbeatAcknowledged
    );
    assert_fade_runtime_round_trip(&mut protocol_client, &mut transport);

    drop(transport);
    daemon.wait_success();
    let _ = protocol_client.transport_disconnected();
    protocol_client.start_connect().unwrap();

    let daemon = Daemon::start(&project_path);
    let mut transport = daemon.connect();
    let request = protocol_client.transport_connected().unwrap();
    let resume_cursor = request.resume_cursor.clone().unwrap();
    assert_eq!(resume_cursor.revision, 1);
    transport.send(&WireMessage::HandshakeRequest(request));
    let WireMessage::HandshakeResponse(response) = transport.receive() else {
        panic!("expected current resume response");
    };
    assert_eq!(
        response.outcome,
        HandshakeOutcome::Resume {
            cursor: resume_cursor
        }
    );
    assert_eq!(response.server.project_id, PROJECT_ID.to_string());
    protocol_client
        .intake(WireMessage::HandshakeResponse(response))
        .unwrap();
    assert_eq!(protocol_client.state(), &ConnectionState::Ready);

    drop(transport);
    daemon.wait_success();
}

#[test]
fn current_client_receives_structured_handshake_rejection() {
    let directory = TestDirectory::new("current-rejection");
    let project_path = directory.project_path();
    create_project(&project_path);
    let mut protocol_client = ProtocolClient::new(ClientConfig::new(
        "process-test",
        ClientType::Integration,
        Role::Replay,
        "rejected-client",
        project_id(),
    ))
    .unwrap();
    protocol_client.start_connect().unwrap();

    let daemon = Daemon::start(&project_path);
    let mut transport = daemon.connect();
    transport.send(&WireMessage::HandshakeRequest(
        protocol_client.transport_connected().unwrap(),
    ));
    let response = transport.receive();
    assert!(matches!(
        &response,
        WireMessage::HandshakeResponse(response)
            if matches!(
                &response.outcome,
                HandshakeOutcome::Rejected { error }
                    if error.code == "permission_denied"
            )
    ));
    assert!(matches!(
        protocol_client.intake(response),
        Err(ClientError::HandshakeRejected(error)) if error.code == "permission_denied"
    ));

    drop(transport);
    daemon.wait_success();
}

#[test]
#[allow(clippy::too_many_lines)]
fn remote_input_rename_is_authorized_replicated_replay_safe_and_survives_restart() {
    let directory = TestDirectory::new("remote-input-rename");
    let project_path = directory.project_path();
    create_rename_project(&project_path);

    let daemon = Daemon::start_without_once(&project_path);
    let mut unauthorized = daemon.connect();
    unauthorized.handshake_as(Role::Operator, None);
    assert!(matches!(unauthorized.receive(), WireMessage::Snapshot(_)));
    unauthorized.send(&command(
        "reorder-denied",
        "reorder-denied-key",
        CommandPayload::ReorderInputs {
            inputs: vec![input(2), input(1)],
        },
    ));
    assert!(matches!(
        unauthorized.next_result(),
        CommandResult::Rejected {
            code,
            current_revision: 0,
            ..
        } if code == "permission_denied"
    ));
    drop(unauthorized);

    let mut graphics = daemon.connect();
    graphics.handshake_as(Role::Graphics, None);
    assert!(matches!(graphics.receive(), WireMessage::Snapshot(_)));
    graphics.send(&command(
        "rename-accepted",
        "rename-accepted-key",
        CommandPayload::RenameInput {
            input: input(1),
            name: "Camera Left".into(),
        },
    ));
    let accepted = graphics.receive();
    let WireMessage::CommandResult(CommandResult::Accepted {
        id,
        revision,
        scheduled_frame,
    }) = &accepted
    else {
        panic!("unexpected rename result: {accepted:?}");
    };
    assert_eq!(
        (id.as_str(), *revision, *scheduled_frame),
        ("rename-accepted", 1, Some(0))
    );
    assert!(matches!(
        graphics.receive(),
        WireMessage::Event(event)
            if event.cursor.revision == 1
                && matches!(
                    event.payload,
                    fm_protocol::EventPayload::InputRenamed { input: renamed, ref name }
                        if renamed == input(1) && name == "Camera Left"
                )
    ));

    assert!(matches!(
        graphics.receive(),
        WireMessage::RuntimeEvent(runtime)
            if runtime.revision == 1
                && runtime.generation == 1
                && matches!(
                    runtime.event,
                    RuntimeLifecycleEvent::Realized { ref domain, .. }
                        if domain == "project"
                )
    ));

    graphics.send(&command(
        "reorder-accepted",
        "reorder-accepted-key",
        CommandPayload::ReorderInputs {
            inputs: vec![input(2), input(1)],
        },
    ));
    assert!(matches!(
        graphics.next_result(),
        CommandResult::Accepted {
            id,
            revision: 2,
            scheduled_frame: Some(1),
        } if id == "reorder-accepted"
    ));
    assert!(matches!(
        graphics.receive(),
        WireMessage::Event(event)
            if event.cursor.revision == 2
                && matches!(
                    event.payload,
                    fm_protocol::EventPayload::InputOrderChanged { ref inputs }
                        if inputs == &vec![input(2), input(1)]
                )
    ));
    graphics.send(&command(
        "rename-replay",
        "rename-accepted-key",
        CommandPayload::RenameInput {
            input: input(1),
            name: "Replay must not apply".into(),
        },
    ));
    assert!(matches!(
        graphics.next_result(),
        CommandResult::Accepted {
            id,
            revision: 1,
            ..
        } if id == "rename-accepted"
    ));
    graphics.send(&command(
        "reorder-replay",
        "reorder-accepted-key",
        CommandPayload::ReorderInputs {
            inputs: vec![input(1), input(2)],
        },
    ));
    assert!(matches!(
        graphics.next_result(),
        CommandResult::Accepted {
            id,
            revision: 2,
            ..
        } if id == "reorder-accepted"
    ));
    drop(graphics);

    let mut snapshot_client = daemon.connect();
    let handshake = snapshot_client.handshake_as(Role::Graphics, None);
    assert_eq!(handshake.current_revision, 2);
    let WireMessage::Snapshot(snapshot) = snapshot_client.receive() else {
        panic!("expected snapshot after input rename");
    };
    assert_eq!(
        snapshot
            .inputs
            .iter()
            .map(|status| (status.input, status.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(input(2), "Input 2"), (input(1), "Camera Left")]
    );
    drop(snapshot_client);
    daemon.stop();

    // The daemon was killed, not stopped: these revisions live in the journal
    // and only reach the manifest when the next daemon recovers them.
    let daemon = Daemon::start(&project_path);
    let mut restarted = daemon.connect();
    let handshake = restarted.handshake_as(Role::Graphics, None);
    assert_eq!(handshake.current_revision, 2);
    let WireMessage::Snapshot(snapshot) = restarted.receive() else {
        panic!("expected snapshot after restart");
    };
    assert_eq!(
        snapshot
            .inputs
            .iter()
            .map(|status| (status.input, status.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(input(2), "Input 2"), (input(1), "Camera Left")]
    );
    drop(restarted);
    daemon.wait_success();

    let persisted = ProjectStore::new(&project_path).unwrap().load().unwrap();
    assert_eq!(persisted.position().revision, 2);
    assert_eq!(persisted.position().frames_rendered, 2);
    assert_eq!(persisted.idempotency_receipts().len(), 2);
    assert_eq!(
        persisted
            .project()
            .inputs()
            .iter()
            .map(|input| (input.id, input.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (domain_input(2), "Input 2"),
            (domain_input(1), "Camera Left")
        ]
    );
    assert_eq!(
        persisted
            .project()
            .input_audio_strips()
            .iter()
            .find(|strip| strip.input == domain_input(1))
            .unwrap()
            .state
            .gain
            .get(),
        -1_000
    );
    assert_eq!(
        persisted
            .project()
            .input_audio_strips()
            .iter()
            .find(|strip| strip.input == domain_input(2))
            .unwrap()
            .state
            .gain
            .get(),
        -2_000
    );
}

/// `StreamStart` is an authorized transition-class command: its acceptance
/// publishes a durable event, an unknown target is rejected without state
/// change, and the desired running flag survives a kill plus journal recovery.
#[test]
#[allow(clippy::too_many_lines)]
fn stream_start_is_replicated_rejects_unknown_targets_and_survives_restart() {
    let directory = TestDirectory::new("stream-reconcile");
    let project_path = directory.project_path();
    create_stream_project(&project_path);

    let daemon = Daemon::start_without_once(&project_path);
    let mut client = daemon.connect();
    client.handshake(None);
    let WireMessage::Snapshot(snapshot) = client.receive() else {
        panic!("expected initial snapshot");
    };
    assert_eq!(snapshot.streams.len(), 2);
    assert!(
        snapshot
            .streams
            .iter()
            .all(|stream| !stream.desired_running)
    );
    assert!(
        snapshot
            .streams
            .iter()
            .all(|stream| matches!(stream.realized, fm_protocol::StreamRealizedState::Stopped),)
    );

    client.send(&command(
        "stream-start",
        "stream-start-key",
        CommandPayload::StreamStart {
            target: stream_target(1),
        },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 1, .. }
    ));
    let WireMessage::Event(event) = client.receive() else {
        panic!("expected durable stream event after acceptance");
    };
    assert_eq!(event.cursor.revision, 1);
    let EventPayload::StreamsChanged { streams } = &event.payload else {
        panic!(
            "expected streams_changed durable event: {:?}",
            event.payload
        );
    };
    assert!(
        streams
            .iter()
            .find(|s| s.target == stream_target(1))
            .unwrap()
            .desired_running
    );
    assert!(
        !streams
            .iter()
            .find(|s| s.target == stream_target(2))
            .unwrap()
            .desired_running
    );

    client.send(&command(
        "stream-unknown",
        "stream-unknown-key",
        CommandPayload::StreamStart {
            target: stream_target(9),
        },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Rejected { ref code, current_revision: 1, .. } if code == "not_found"
    ));

    // The daemon is killed with the acknowledgement in hand and no checkpoint:
    // revision 1 exists only as a journal batch.
    drop(client);
    daemon.stop();
    assert_eq!(
        ProjectStore::new(&project_path)
            .unwrap()
            .load()
            .unwrap()
            .position()
            .revision,
        0
    );

    let daemon = Daemon::start(&project_path);
    let mut restarted = daemon.connect();
    let handshake = restarted.handshake(None);
    assert_eq!(handshake.current_revision, 1);
    let WireMessage::Snapshot(snapshot) = restarted.receive() else {
        panic!("expected snapshot after restart");
    };
    assert!(
        snapshot
            .streams
            .iter()
            .find(|stream| stream.target == stream_target(1))
            .expect("restarted snapshot keeps the stream inventory")
            .desired_running
    );
    assert!(
        !snapshot
            .streams
            .iter()
            .find(|stream| stream.target == stream_target(2))
            .expect("restarted snapshot keeps the stream inventory")
            .desired_running
    );
    drop(restarted);
    daemon.wait_success();

    let persisted = ProjectStore::new(&project_path).unwrap().load().unwrap();
    assert_eq!(persisted.position().revision, 1);
    let targets = persisted.project().stream_targets();
    assert_eq!(targets.len(), 2);
    assert!(targets[0].running());
    assert!(!targets[1].running());
}

#[test]
fn commands_survive_restart_resume_and_duplicate_replay() {
    let directory = TestDirectory::new("restart");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    let initial = client.handshake(None);
    assert!(!initial.resume);
    assert_eq!(initial.current_revision, 0);
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    client.send(&command(
        "preview-command",
        "preview-key",
        CommandPayload::SelectPreview { input: input(3) },
    ));
    let first_after_command = client.receive();
    assert!(matches!(
        first_after_command,
        WireMessage::CommandResult(CommandResult::Accepted { revision: 1, .. })
    ));

    client.send(&command("cut-command", "cut-key", CommandPayload::Cut));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 2, .. }
    ));

    client.send(&command(
        "fade-command",
        "fade-key",
        CommandPayload::Fade { duration_frames: 4 },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 3, .. }
    ));
    drop(client);
    daemon.wait_success();

    let store = ProjectStore::new(&project_path).unwrap();
    let persisted = store.load().unwrap();
    assert_eq!(persisted.position().revision, 3);
    assert_eq!(persisted.position().event_sequence, 3);
    assert_eq!(persisted.position().runtime_generation, 3);
    assert_eq!(persisted.position().frames_rendered, 6);
    assert_eq!(persisted.position().clock_time_nanos, 200_000_000);
    assert_eq!(
        persisted.runtime_routing().desired_program_id,
        Some(domain_input(1))
    );
    assert_eq!(
        persisted.runtime_routing().realized_program_id,
        Some(domain_input(1))
    );
    assert_eq!(
        persisted.runtime_routing().desired_preview_id,
        Some(domain_input(3))
    );
    assert_eq!(
        persisted.runtime_routing().realized_preview_id,
        Some(domain_input(3))
    );
    assert_eq!(persisted.idempotency_receipts().len(), 3);
    assert_eq!(
        persisted.project().scenes()[0].layers[0].mask,
        Some(RectMask::new(100, 50, 1_000, 600).inverted(true))
    );
    let mut expected_project = canonical_project();
    expected_project.set_main_mix(MainMix::new(domain_input(1), domain_input(3)));
    assert_eq!(persisted.project(), &expected_project);

    let cursor = EventCursor {
        engine: initial.engine,
        revision: 3,
    };
    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    let resumed = client.handshake(Some(cursor));
    assert!(resumed.resume);
    assert_eq!(resumed.current_revision, 3);

    client.send(&command(
        "duplicate-command",
        "fade-key",
        CommandPayload::Cut,
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted {
            id,
            revision: 3,
            ..
        } if id == "fade-command"
    ));
    drop(client);
    daemon.wait_success();

    assert_eq!(store.load().unwrap(), persisted);
}

/// The whole point of the journal: a command the operator was told succeeded
/// must survive `SIGKILL`.
///
/// The daemon is killed with the acknowledgement already in the client's hand
/// and no chance to shut down, flush or checkpoint. The manifest is still at
/// revision 0 at that instant, so nothing here can pass because a file happened
/// to be written — the two revisions exist only as journal batches, and only
/// recovery can bring them back.
#[test]
fn acknowledged_commands_survive_sigkill_before_any_checkpoint() {
    let directory = TestDirectory::new("sigkill-durability");
    let project_path = directory.project_path();
    create_project(&project_path);
    let store = ProjectStore::new(&project_path).unwrap();

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    assert_eq!(client.handshake(None).current_revision, 0);
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    client.send(&command(
        "killed-preview",
        "killed-preview-key",
        CommandPayload::SelectPreview { input: input(3) },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 1, .. }
    ));
    client.send(&command(
        "killed-cut",
        "killed-cut-key",
        CommandPayload::Cut,
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 2, .. }
    ));

    // Both acknowledgements are in hand. Kill the process outright.
    daemon.stop();

    // Nothing acknowledged is in the manifest, so the recovery below cannot be
    // reading a per-command manifest rewrite.
    assert_eq!(store.load().unwrap().position().revision, 0);
    let crashed = store.scan_journal().unwrap();
    assert_eq!(crashed.batches().len(), 2);
    assert_eq!(crashed.checkpoint_sequence(), 0);
    assert_eq!(crashed.observations(), []);

    let daemon = Daemon::start(&project_path);
    let mut restarted = daemon.connect();
    assert_eq!(restarted.handshake(None).current_revision, 2);
    assert!(matches!(restarted.receive(), WireMessage::Snapshot(_)));
    // The receipts came back too, so a client that retries after the crash is
    // answered from the original acceptance instead of cutting the show again.
    restarted.send(&command(
        "killed-cut-retry",
        "killed-cut-key",
        CommandPayload::Cut,
    ));
    assert!(matches!(
        restarted.next_result(),
        CommandResult::Accepted {
            id,
            revision: 2,
            ..
        } if id == "killed-cut"
    ));
    drop(restarted);
    daemon.wait_success();

    let persisted = store.load().unwrap();
    assert_eq!(persisted.position().revision, 2);
    assert_eq!(persisted.idempotency_receipts().len(), 2);
    // The cut swapped the preview selected at revision 1 onto program.
    assert_eq!(
        persisted.runtime_routing().realized_program_id,
        Some(domain_input(3))
    );
    assert_eq!(
        persisted.runtime_routing().desired_program_id,
        Some(domain_input(3))
    );
}

/// Checkpoints must bound journal growth on their own, and recovery must land
/// on the exact state the daemon held, not merely a plausible one.
///
/// A run of `CHECKPOINTED_COMMANDS` commands is executed twice against
/// identical projects: once stopped cleanly, once killed and recovered. The two
/// projects must end byte-for-byte identical as `StoredProject` values —
/// revision, event sequence, frames rendered, runtime generation, clock time,
/// routing and every receipt.
#[test]
fn checkpoints_compact_the_journal_and_recovery_reproduces_the_exact_state() {
    /// One more than the daemon's checkpoint interval, so a checkpoint is
    /// forced mid-run without a clean shutdown.
    const CHECKPOINTED_COMMANDS: u32 = 65;

    fn cut_repeatedly(daemon: &Daemon, count: u32) {
        let mut client = daemon.connect();
        client.handshake(None);
        assert!(matches!(client.receive(), WireMessage::Snapshot(_)));
        for revision in 1..=count {
            client.send(&command(
                &format!("cut-{revision}"),
                &format!("cut-key-{revision}"),
                CommandPayload::Cut,
            ));
            match client.next_result() {
                CommandResult::Accepted { revision: got, .. } => {
                    assert_eq!(got, u64::from(revision));
                }
                CommandResult::Rejected { code, message, .. } => {
                    panic!("command {revision} was rejected as {code}: {message}")
                }
            }
        }
    }

    let directory = TestDirectory::new("checkpoint-clean");
    let clean_path = directory.project_path();
    create_project(&clean_path);
    let clean_store = ProjectStore::new(&clean_path).unwrap();
    let daemon = Daemon::start(&clean_path);
    cut_repeatedly(&daemon, CHECKPOINTED_COMMANDS);
    daemon.wait_success();
    let clean = clean_store.load().unwrap();
    assert_eq!(clean.position().revision, u64::from(CHECKPOINTED_COMMANDS));

    let crash_directory = TestDirectory::new("checkpoint-crash");
    let crash_path = crash_directory.project_path();
    create_project(&crash_path);
    let crash_store = ProjectStore::new(&crash_path).unwrap();
    let daemon = Daemon::start(&crash_path);
    cut_repeatedly(&daemon, CHECKPOINTED_COMMANDS);
    daemon.stop();

    // The bounded checkpoint fired mid-run: the manifest advanced without a
    // clean shutdown, and only the commands after it were left to replay.
    let crashed = crash_store.load().unwrap();
    assert!(
        crashed.position().revision > 0 && crashed.position().revision < clean.position().revision,
        "a checkpoint must have advanced the manifest mid-run, found revision {}",
        crashed.position().revision
    );
    let scan = crash_store.scan_journal().unwrap();
    assert_eq!(scan.checkpoint_revision(), crashed.position().revision);
    assert_eq!(
        u64::from(CHECKPOINTED_COMMANDS) - crashed.position().revision,
        scan.batches().len() as u64,
        "every command after the checkpoint must still be in the journal"
    );

    let daemon = Daemon::start(&crash_path);
    let mut client = daemon.connect();
    assert_eq!(
        client.handshake(None).current_revision,
        u64::from(CHECKPOINTED_COMMANDS)
    );
    drop(client);
    daemon.wait_success();

    assert_eq!(crash_store.load().unwrap(), clean);
    // Checkpointing discarded what it applied instead of retaining it forever.
    let settled = crash_store.scan_journal().unwrap();
    assert_eq!(settled.batches(), []);
    assert_eq!(
        settled.checkpoint_revision(),
        u64::from(CHECKPOINTED_COMMANDS)
    );
}

#[test]
fn live_stinger_slot_mutations_fire_immediately_and_survive_restart() {
    let directory = TestDirectory::new("live-stinger-configuration");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    assert_eq!(
        client
            .handshake_version(CURRENT_PROTOCOL_VERSION, None)
            .protocol,
        CURRENT_PROTOCOL_VERSION
    );
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    configure_and_fire_stinger(&mut client);
    replace_live_stinger(&mut client);
    drop(client);
    daemon.wait_success();

    let persisted = ProjectStore::new(&project_path).unwrap().load().unwrap();
    assert_eq!(persisted.position().revision, 3);
    assert_eq!(persisted.idempotency_receipts().len(), 3);
    assert_eq!(
        (
            persisted.runtime_routing().desired_program_id,
            persisted.runtime_routing().realized_program_id,
            persisted.runtime_routing().desired_preview_id,
            persisted.runtime_routing().realized_preview_id,
        ),
        (
            Some(domain_input(2)),
            Some(domain_input(2)),
            Some(domain_input(1)),
            Some(domain_input(1)),
        )
    );
    let replacement = persisted.project().stingers()[0];
    assert_eq!(replacement.slot.number(), 8);
    assert_eq!(replacement.media_input, domain_input(2));
    assert!(!replacement.preload);
    assert_eq!(replacement.cut_point_frames, 7);
    assert_eq!(
        replacement.audio_policy,
        fm_model::StingerAudioPolicy::Muted
    );
    assert_eq!(
        replacement.missing_media_fallback,
        fm_model::StingerMissingMediaFallback::KeepProgram
    );

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    assert_eq!(
        client
            .handshake_version(CURRENT_PROTOCOL_VERSION, None)
            .current_revision,
        3
    );
    let WireMessage::Snapshot(snapshot) = client.receive() else {
        panic!("expected restarted snapshot");
    };
    let restarted = &snapshot.stingers[0];
    assert_eq!(restarted.slot.number(), 8);
    assert_eq!(restarted.media_input, input(2));
    assert!(!restarted.preload);
    assert_eq!(restarted.cut_point_frames, 7);
    assert_eq!(restarted.audio_policy, StingerAudioPolicy::Muted);
    assert_eq!(
        restarted.missing_media_fallback,
        StingerMissingMediaFallback::KeepProgram
    );
    assert_eq!(
        restarted.readiness,
        fm_protocol::StingerReadiness::NotRequested
    );

    remove_live_stinger(&mut client);
    drop(client);
    daemon.wait_success();

    let persisted = ProjectStore::new(&project_path).unwrap().load().unwrap();
    assert_eq!(persisted.position().revision, 4);
    assert_eq!(persisted.idempotency_receipts().len(), 4);
    assert_eq!(persisted.project().stingers(), []);

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    assert_eq!(
        client
            .handshake_version(CURRENT_PROTOCOL_VERSION, None)
            .current_revision,
        4
    );
    let WireMessage::Snapshot(snapshot) = client.receive() else {
        panic!("expected second restarted snapshot");
    };
    assert_eq!(snapshot.stingers, []);
    drop(client);
    daemon.wait_success();
}

fn configure_and_fire_stinger(client: &mut Client) {
    client.send(&command_version(
        CURRENT_PROTOCOL_VERSION,
        "configure-stinger",
        "configure-stinger-key",
        CommandPayload::ConfigureStinger {
            slot: WireStingerSlotId::new(8).unwrap(),
            media_input: input(3),
            preload: true,
            cut_point_frames: 1,
            audio_policy: StingerAudioPolicy::MixWithProgram,
            missing_media_fallback: StingerMissingMediaFallback::Cut,
        },
    ));
    assert!(matches!(
        client.receive(),
        WireMessage::CommandResult(CommandResult::Accepted { revision: 1, .. })
    ));
    assert!(matches!(
        client.receive(),
        WireMessage::Event(fm_protocol::EventMessage {
            payload: fm_protocol::EventPayload::StingerSlotsChanged { .. },
            ..
        })
    ));
    assert!(matches!(
        client.receive(),
        WireMessage::RuntimeEvent(fm_protocol::RuntimeEventMessage { revision: 1, .. })
    ));
    client.send(&command_version(
        CURRENT_PROTOCOL_VERSION,
        "fire-stinger",
        "fire-stinger-key",
        CommandPayload::Stinger {
            slot: WireStingerSlotId::new(8).unwrap(),
            duration_frames: 2,
        },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 2, .. }
    ));
}

fn replace_live_stinger(client: &mut Client) {
    client.send(&command_version(
        CURRENT_PROTOCOL_VERSION,
        "replace-stinger",
        "replace-stinger-key",
        CommandPayload::ConfigureStinger {
            slot: WireStingerSlotId::new(8).unwrap(),
            media_input: input(2),
            preload: false,
            cut_point_frames: 7,
            audio_policy: StingerAudioPolicy::Muted,
            missing_media_fallback: StingerMissingMediaFallback::KeepProgram,
        },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 3, .. }
    ));
}

fn remove_live_stinger(client: &mut Client) {
    client.send(&command_version(
        CURRENT_PROTOCOL_VERSION,
        "remove-stinger",
        "remove-stinger-key",
        CommandPayload::RemoveStinger {
            slot: WireStingerSlotId::new(8).unwrap(),
        },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 4, .. }
    ));
}

#[test]
fn slide_command_settles_and_survives_daemon_restart() {
    let directory = TestDirectory::new("slide-restart");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    let initial = client.handshake_version(CURRENT_PROTOCOL_VERSION, None);
    assert_eq!(initial.protocol, CURRENT_PROTOCOL_VERSION);
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    client.send(&command_version(
        CURRENT_PROTOCOL_VERSION,
        "slide-command",
        "slide-key",
        CommandPayload::Slide { duration_frames: 3 },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 1, .. }
    ));
    drop(client);
    daemon.wait_success();

    let store = ProjectStore::new(&project_path).unwrap();
    let persisted = store.load().unwrap();
    assert_eq!(persisted.position().revision, 1);
    assert_eq!(persisted.position().event_sequence, 1);
    assert_eq!(persisted.position().runtime_generation, 1);
    assert_eq!(persisted.position().frames_rendered, 3);
    assert_eq!(
        persisted.runtime_routing().realized_program_id,
        Some(domain_input(2))
    );
    assert_eq!(
        persisted.runtime_routing().realized_preview_id,
        Some(domain_input(1))
    );

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    let resumed = client.handshake_version(
        CURRENT_PROTOCOL_VERSION,
        Some(EventCursor {
            engine: initial.engine,
            revision: 1,
        }),
    );
    assert!(resumed.resume);
    assert_eq!(resumed.current_revision, 1);
    drop(client);
    daemon.wait_success();
    assert_eq!(store.load().unwrap(), persisted);
}

#[test]
fn zoom_command_settles_and_survives_daemon_restart() {
    let directory = TestDirectory::new("zoom-restart");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    let initial = client.handshake_version(CURRENT_PROTOCOL_VERSION, None);
    assert_eq!(initial.protocol, CURRENT_PROTOCOL_VERSION);
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    client.send(&command_version(
        CURRENT_PROTOCOL_VERSION,
        "zoom-command",
        "zoom-key",
        CommandPayload::Zoom { duration_frames: 3 },
    ));
    assert!(matches!(
        client.next_result(),
        CommandResult::Accepted { revision: 1, .. }
    ));
    drop(client);
    daemon.wait_success();

    let store = ProjectStore::new(&project_path).unwrap();
    let persisted = store.load().unwrap();
    assert_eq!(persisted.position().revision, 1);
    assert_eq!(persisted.position().event_sequence, 1);
    assert_eq!(persisted.position().runtime_generation, 1);
    assert_eq!(persisted.position().frames_rendered, 3);
    assert_eq!(
        persisted.runtime_routing().realized_program_id,
        Some(domain_input(2))
    );
    assert_eq!(
        persisted.runtime_routing().realized_preview_id,
        Some(domain_input(1))
    );

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    let resumed = client.handshake_version(
        CURRENT_PROTOCOL_VERSION,
        Some(EventCursor {
            engine: initial.engine,
            revision: 1,
        }),
    );
    assert!(resumed.resume);
    assert_eq!(resumed.current_revision, 1);
    drop(client);
    daemon.wait_success();
    assert_eq!(store.load().unwrap(), persisted);
}

#[test]
#[allow(clippy::too_many_lines)]
fn manual_alpha_fade_state_and_receipts_survive_restart_through_commit_and_cancel() {
    for (name, terminal, swaps_routes) in [
        (
            "manual-commit-restart",
            CommandPayload::CommitManualTransition,
            true,
        ),
        (
            "manual-cancel-restart",
            CommandPayload::CancelManualTransition,
            false,
        ),
    ] {
        let directory = TestDirectory::new(name);
        let project_path = directory.project_path();
        create_project(&project_path);

        let daemon = Daemon::start(&project_path);
        let mut client = daemon.connect();
        let hello = client.handshake_version(CURRENT_PROTOCOL_VERSION, None);
        assert_eq!(hello.protocol, CURRENT_PROTOCOL_VERSION);
        assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

        client.send(&command_version(
            CURRENT_PROTOCOL_VERSION,
            "manual-start",
            "manual-start-key",
            CommandPayload::StartManualTransition {
                kind: ManualTransitionKind::AlphaFade,
            },
        ));
        assert!(matches!(
            client.next_result(),
            CommandResult::Accepted { revision: 1, .. }
        ));
        client.send(&command_version(
            CURRENT_PROTOCOL_VERSION,
            "manual-set",
            "manual-set-key",
            CommandPayload::SetManualTransitionPosition {
                position: ManualTransitionPosition::new(6_250).unwrap(),
            },
        ));
        assert!(matches!(
            client.next_result(),
            CommandResult::Accepted { revision: 2, .. }
        ));
        drop(client);
        daemon.wait_success();

        let store = ProjectStore::new(&project_path).unwrap();
        let checkpoint = store.load().unwrap();
        let manual = checkpoint.runtime_manual_transitions();
        let desired = manual
            .desired
            .expect("desired manual state must be durable");
        let realized = manual
            .realized
            .expect("realized manual state must be durable");
        assert_eq!(desired.kind, PersistedManualTransitionKind::AlphaFade);
        assert_eq!(desired.interval_start_basis_points, 0);
        assert_eq!(desired.position_basis_points, 6_250);
        assert_eq!(realized.interval_start_basis_points, 6_250);
        assert_eq!(realized.position_basis_points, 6_250);
        assert_eq!(checkpoint.idempotency_receipts().len(), 2);

        let daemon = Daemon::start(&project_path);
        let mut client = daemon.connect();
        let hello = client.handshake_version(CURRENT_PROTOCOL_VERSION, None);
        assert_eq!(hello.current_revision, 2);
        let WireMessage::Snapshot(snapshot) = client.receive() else {
            panic!("restart must return a snapshot");
        };
        assert!(matches!(
            snapshot.desired_manual_transition,
            ManualTransitionStatus::Active(state)
                if state.kind == ManualTransitionKind::AlphaFade
                    && state.interval_start == ManualTransitionPosition::START
                    && state.position.basis_points() == 6_250
        ));
        assert!(matches!(
            snapshot.realized_manual_transition,
            ManualTransitionStatus::Active(state)
                if state.interval_start.basis_points() == 6_250
                    && state.position.basis_points() == 6_250
        ));

        client.send(&command_version(
            CURRENT_PROTOCOL_VERSION,
            "manual-replay-different-command",
            "manual-set-key",
            CommandPayload::CancelManualTransition,
        ));
        assert!(matches!(
            client.next_result(),
            CommandResult::Accepted {
                id,
                revision: 2,
                ..
            } if id == "manual-set"
        ));
        client.send(&command_version(
            CURRENT_PROTOCOL_VERSION,
            "manual-terminal",
            "manual-terminal-key",
            terminal,
        ));
        assert!(matches!(
            client.next_result(),
            CommandResult::Accepted { revision: 3, .. }
        ));
        drop(client);
        daemon.wait_success();

        let final_state = store.load().unwrap();
        assert_eq!(final_state.runtime_manual_transitions().desired, None);
        assert_eq!(final_state.runtime_manual_transitions().realized, None);
        assert_eq!(final_state.idempotency_receipts().len(), 3);
        let routing = final_state.runtime_routing();
        let expected_program = if swaps_routes {
            domain_input(2)
        } else {
            domain_input(1)
        };
        let expected_preview = if swaps_routes {
            domain_input(1)
        } else {
            domain_input(2)
        };
        assert_eq!(routing.desired_program_id, Some(expected_program));
        assert_eq!(routing.realized_program_id, Some(expected_program));
        assert_eq!(routing.desired_preview_id, Some(expected_preview));
        assert_eq!(routing.realized_preview_id, Some(expected_preview));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn manual_slide_state_survives_restart_through_commit_and_cancel() {
    for (name, terminal, swaps_routes) in [
        (
            "manual-slide-commit-restart",
            CommandPayload::CommitManualTransition,
            true,
        ),
        (
            "manual-slide-cancel-restart",
            CommandPayload::CancelManualTransition,
            false,
        ),
    ] {
        let directory = TestDirectory::new(name);
        let project_path = directory.project_path();
        create_project(&project_path);

        let daemon = Daemon::start(&project_path);
        let mut client = daemon.connect();
        client.handshake_version(CURRENT_PROTOCOL_VERSION, None);
        assert!(matches!(client.receive(), WireMessage::Snapshot(_)));
        for (id, key, payload, revision) in [
            (
                "manual-slide-start",
                "manual-slide-start-key",
                CommandPayload::StartManualTransition {
                    kind: ManualTransitionKind::Slide,
                },
                1,
            ),
            (
                "manual-slide-position",
                "manual-slide-position-key",
                CommandPayload::SetManualTransitionPosition {
                    position: ManualTransitionPosition::new(6_250).unwrap(),
                },
                2,
            ),
        ] {
            client.send(&command_version(CURRENT_PROTOCOL_VERSION, id, key, payload));
            assert!(matches!(
                client.next_result(),
                CommandResult::Accepted {
                    revision: accepted,
                    ..
                } if accepted == revision
            ));
        }
        drop(client);
        daemon.wait_success();

        let store = ProjectStore::new(&project_path).unwrap();
        let checkpoint = store.load().unwrap();
        let manual = checkpoint.runtime_manual_transitions();
        assert!(matches!(
            (manual.desired, manual.realized),
            (Some(desired), Some(realized))
                if desired.kind == PersistedManualTransitionKind::Slide
                    && desired.interval_start_basis_points == 0
                    && desired.position_basis_points == 6_250
                    && realized.kind == PersistedManualTransitionKind::Slide
                    && realized.interval_start_basis_points == 6_250
                    && realized.position_basis_points == 6_250
        ));

        let daemon = Daemon::start(&project_path);
        let mut client = daemon.connect();
        let hello = client.handshake_version(CURRENT_PROTOCOL_VERSION, None);
        assert_eq!(hello.current_revision, 2);
        let WireMessage::Snapshot(snapshot) = client.receive() else {
            panic!("restart must return a snapshot");
        };
        assert!(matches!(
            (
                snapshot.desired_manual_transition,
                snapshot.realized_manual_transition,
            ),
            (
                ManualTransitionStatus::Active(desired),
                ManualTransitionStatus::Active(realized),
            ) if desired.kind == ManualTransitionKind::Slide
                && desired.interval_start == ManualTransitionPosition::START
                && desired.position.basis_points() == 6_250
                && realized.kind == ManualTransitionKind::Slide
                && realized.interval_start.basis_points() == 6_250
                && realized.position.basis_points() == 6_250
        ));
        client.send(&command_version(
            CURRENT_PROTOCOL_VERSION,
            "manual-slide-terminal",
            "manual-slide-terminal-key",
            terminal,
        ));
        assert!(matches!(
            client.next_result(),
            CommandResult::Accepted { revision: 3, .. }
        ));
        drop(client);
        daemon.wait_success();

        let final_state = store.load().unwrap();
        assert_eq!(
            final_state.runtime_manual_transitions(),
            fm_persistence::RuntimeManualTransitions::default()
        );
        let routing = final_state.runtime_routing();
        let expected = if swaps_routes {
            (domain_input(2), domain_input(1))
        } else {
            (domain_input(1), domain_input(2))
        };
        assert_eq!(routing.desired_program_id, Some(expected.0));
        assert_eq!(routing.realized_program_id, Some(expected.0));
        assert_eq!(routing.desired_preview_id, Some(expected.1));
        assert_eq!(routing.realized_preview_id, Some(expected.1));
    }
}

#[test]
fn non_current_handshake_returns_protocol_mismatch() {
    let directory = TestDirectory::new("protocol-mismatch");
    let project_path = directory.project_path();
    create_project(&project_path);
    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: ProtocolVersion::new(99, 0),
        build: "future".into(),
        client_type: ClientType::Integration,
        desired_role: Role::Operator,
        resume_cursor: None,
    }));
    assert!(matches!(
        client.receive(),
        WireMessage::HandshakeResponse(response)
            if matches!(&response.outcome, HandshakeOutcome::Rejected { error } if error.code == "protocol_mismatch")
    ));
    drop(client);
    daemon.wait_success();
}

#[test]
fn viewer_diagnostics_query_is_read_only_and_correlated() {
    let directory = TestDirectory::new("viewer-diagnostics");
    let project_path = directory.project_path();
    create_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);
    let mut client = daemon.connect();
    let handshake = client.handshake_as(Role::Viewer, None);
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));
    client.send(&WireMessage::DiagnosticsRequest(DiagnosticsRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: "diagnostics-1".into(),
    }));
    let response = client.receive();
    let WireMessage::DiagnosticsResponse(DiagnosticsResponse {
        protocol,
        request_id,
        engine,
        current_revision,
        oldest_retained_revision,
        newest_retained_revision,
        subscriber_count,
        retained_events_limit,
        subscriber_limit,
        subscriber_queue_limit,
    }) = response
    else {
        panic!("expected diagnostics response");
    };
    assert_eq!(protocol, CURRENT_PROTOCOL_VERSION);
    assert_eq!(request_id, "diagnostics-1");
    assert_eq!(engine, handshake.engine);
    assert_eq!(current_revision, handshake.current_revision);
    assert_eq!(oldest_retained_revision, None);
    assert_eq!(newest_retained_revision, None);
    assert_eq!(subscriber_count, 1);
    assert!(retained_events_limit > 0);
    assert!(subscriber_limit > 0);
    assert!(subscriber_queue_limit > 0);
    drop(client);
    let persisted = ProjectStore::new(&project_path).unwrap().load().unwrap();
    assert_eq!(persisted.position().revision, handshake.current_revision);
    assert_eq!(persisted.idempotency_receipts(), []);
    daemon.stop();
}

#[test]
fn non_loopback_bind_is_rejected_before_listening() {
    let directory = TestDirectory::new("exposed-bind");
    let project_path = directory.project_path();
    create_project(&project_path);
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project_path)
        .arg("--listen")
        .arg("0.0.0.0:0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("requires a loopback")
    );
}

#[cfg(unix)]
#[test]
fn sigterm_notifies_established_client_then_exits_cleanly() {
    let directory = TestDirectory::new("sigterm-notifies-client");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start_without_once(&project_path);
    let mut session = TcpSession::new(
        ProtocolClient::new(ClientConfig::new(
            "process-test",
            ClientType::Integration,
            Role::Operator,
            "process-client",
            project_id(),
        ))
        .unwrap(),
    );
    assert!(matches!(
        session
            .connect(daemon.address, Duration::from_secs(1))
            .unwrap(),
        SessionEvent::Connected { .. }
    ));
    let status = ProcessCommand::new("/bin/kill")
        .args(["-TERM", &daemon.child.as_ref().unwrap().id().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "SIGTERM command failed: {status}");
    let deadline = Instant::now() + Duration::from_secs(1);
    assert!(matches!(
        session
            .receive_cancellable(Duration::from_millis(25), || Instant::now() >= deadline)
            .unwrap(),
        SessionEvent::Disconnected {
            cause: DisconnectCause::ServerShutdown,
            backoff,
        } if backoff.attempt == 1
    ));
    daemon.wait_success();
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn websocket_control_is_authenticated_ordered_and_raw_compatible() {
    let directory = TestDirectory::new("websocket-control");
    let project_path = directory.project_path();
    create_project(&project_path);
    let configured_token = "configured-token-0123456789abcdef";
    let presented_token = "presented-token-0123456789abcdef";
    let (daemon, web_address) = Daemon::start_web(&project_path, configured_token);
    let timeout = Duration::from_secs(1);
    let request = |token: &str| {
        tungstenite::http::Request::builder()
            .uri(format!("ws://{web_address}/v1/control"))
            .header("Host", web_address.to_string())
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Authorization", format!("Bearer {token}"))
            .body(())
            .unwrap()
    };
    let configure = |stream: &TcpStream| {
        stream.set_read_timeout(Some(timeout)).unwrap();
        stream.set_write_timeout(Some(timeout)).unwrap();
    };

    let unauthorized_stream = TcpStream::connect_timeout(&web_address, timeout).unwrap();
    configure(&unauthorized_stream);
    let unauthorized_request = request(presented_token);
    match tungstenite::client::client(unauthorized_request, unauthorized_stream) {
        Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response))) => {
            assert_eq!(
                response.status(),
                tungstenite::http::StatusCode::UNAUTHORIZED
            );
            let response = format!("{response:?}");
            assert!(!response.contains(configured_token));
            assert!(!response.contains(presented_token));
        }
        result => panic!("unauthorized WebSocket upgrade result: {result:?}"),
    }

    let websocket_stream = TcpStream::connect_timeout(&web_address, timeout).unwrap();
    configure(&websocket_stream);
    let websocket_request = request(configured_token);
    let (mut websocket, _) =
        tungstenite::client::client(websocket_request, websocket_stream).unwrap();
    let receive = |websocket: &mut tungstenite::WebSocket<TcpStream>| -> WireMessage {
        match websocket.read().unwrap() {
            tungstenite::Message::Text(text) => {
                assert!(text.as_str().ends_with('\n'));
                decode_line(text.as_str()).unwrap()
            }
            message => panic!("expected WebSocket text frame, got {message:?}"),
        }
    };
    let send = |websocket: &mut tungstenite::WebSocket<TcpStream>, message: &WireMessage| {
        websocket
            .send(tungstenite::Message::text(encode_line(message).unwrap()))
            .unwrap();
    };
    send(
        &mut websocket,
        &WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "process-test".into(),
            client_type: ClientType::Integration,
            desired_role: Role::Operator,
            resume_cursor: None,
        }),
    );
    assert!(matches!(
        receive(&mut websocket),
        WireMessage::HandshakeResponse(response)
            if response.protocol == CURRENT_PROTOCOL_VERSION
                && response.granted_role == Role::Operator
    ));
    assert!(matches!(receive(&mut websocket), WireMessage::Snapshot(_)));

    let mut raw_session = TcpSession::new(
        ProtocolClient::new(ClientConfig::new(
            "process-test",
            ClientType::Integration,
            Role::Operator,
            "raw-process-client",
            project_id(),
        ))
        .unwrap(),
    );
    assert!(matches!(
        raw_session.connect(daemon.address, timeout).unwrap(),
        SessionEvent::Connected { .. }
    ));
    assert_eq!(raw_session.client().state(), &ConnectionState::Ready);

    send(
        &mut websocket,
        &command_version(
            CURRENT_PROTOCOL_VERSION,
            "web-cut",
            "web-cut-key",
            CommandPayload::Cut,
        ),
    );
    assert!(matches!(
        receive(&mut websocket),
        WireMessage::CommandResult(CommandResult::Accepted { id, revision: 1, .. })
            if id == "web-cut"
    ));
    let WireMessage::Event(event) = receive(&mut websocket) else {
        panic!("expected durable switcher event");
    };
    assert_eq!(event.cursor.revision, 1);
    assert!(matches!(
        event.payload,
        EventPayload::DesiredSwitcher { program, preview, .. }
            if program == input(2) && preview == input(1)
    ));
    let WireMessage::RuntimeEvent(runtime) = receive(&mut websocket) else {
        panic!("expected switcher runtime event");
    };
    assert_eq!(runtime.revision, 1);
    assert_eq!(runtime.generation, 1);
    assert_eq!(runtime.sequence, 1);
    assert!(matches!(
        runtime.event,
        RuntimeLifecycleEvent::Realized { domain, .. } if domain == "switcher"
    ));

    let status = ProcessCommand::new("/bin/kill")
        .args(["-TERM", &daemon.child.as_ref().unwrap().id().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "SIGTERM command failed: {status}");
    assert!(matches!(
        receive(&mut websocket),
        WireMessage::Error(error) if error.error.code == "server_shutting_down"
    ));
    match websocket.read() {
        Ok(tungstenite::Message::Close(_))
        | Err(
            tungstenite::Error::ConnectionClosed
            | tungstenite::Error::Protocol(
                tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
            ),
        ) => {}
        Err(tungstenite::Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
        }
        result => panic!("WebSocket did not terminate cleanly: {result:?}"),
    }
    let deadline = Instant::now() + timeout;
    loop {
        match raw_session
            .receive_cancellable(Duration::from_millis(25), || Instant::now() >= deadline)
            .unwrap()
        {
            SessionEvent::Disconnected {
                cause: DisconnectCause::ServerShutdown,
                ..
            } => break,
            SessionEvent::Disconnected { cause, .. } => {
                panic!("raw client disconnected unexpectedly: {cause:?}")
            }
            _ => assert!(Instant::now() < deadline, "raw client shutdown timed out"),
        }
    }
    drop(websocket);
    drop(raw_session);
    daemon.wait_success();

    let stored = ProjectStore::new(&project_path).unwrap().load().unwrap();
    assert_eq!(stored.position().revision, 1);
    assert!(stored.idempotency_receipts().iter().any(|receipt| {
        receipt.key() == "web-cut-key"
            && receipt.command_id() == "web-cut"
            && matches!(
                receipt.outcome(),
                fm_persistence::ReceiptOutcome::Accepted { revision: 1, .. }
            )
    }));
}

#[test]
fn help_documents_program_output_and_fullscreen_selection() {
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--fullscreen-program"));
    assert!(stdout.contains("--fullscreen-display 0"));
    assert!(stdout.contains("--record-program output.mp4"));
    assert!(stdout.contains("--camera-helper PATH"));
    assert!(stdout.contains("never requests permission"));
    assert!(stdout.contains("--record-program=<path>"));
    assert!(stdout.contains("segments are created exclusively, never overwritten"));
    assert!(stdout.contains("later record-start segments use"));
    assert!(stdout.contains("configured startup support"));
    assert!(stdout.contains("FREEMIXD_RECORDER reports runtime health"));
    assert!(stdout.contains("zero-based index"));
}

#[test]
fn recording_without_native_media_fails_before_readiness() {
    let directory = TestDirectory::new("recording-requires-native");
    let project_path = directory.project_path();
    create_project(&project_path);
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project_path)
        .args(["--record-program", "capture.mp4"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("FREEMIXD_READY")
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--record-program requires --native-media")
    );
}

fn create_project(path: &Path) {
    let project = StoredProject::from_project(
        canonical_project(),
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
    ProjectStore::new(path).unwrap().save(&project).unwrap();
}

fn create_rename_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        project_id(),
        "Rename Test",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(16, 16).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(44_100).unwrap(),
                sample_format: SampleFormat::I24,
                channels: ChannelLayout::stereo(),
            },
        },
    );
    for number in 1..=2 {
        project.add_input(Input {
            id: domain_input(number),
            name: format!("Input {number}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    assert!(project.set_input_audio_strip(
        domain_input(1),
        InputAudioStripState {
            gain: InputGainMilliDb::new(-1_000).unwrap(),
            ..InputAudioStripState::default()
        }
    ));
    assert!(project.set_input_audio_strip(
        domain_input(2),
        InputAudioStripState {
            gain: InputGainMilliDb::new(-2_000).unwrap(),
            ..InputAudioStripState::default()
        }
    ));
    project.set_main_mix(MainMix::new(domain_input(1), domain_input(2)));
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

fn create_stream_project(path: &Path) {
    const STREAM_ID_BASE: u128 = U64_MAX_ID + 500;
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        project_id(),
        "Stream Reconcile",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(16, 16).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(44_100).unwrap(),
                sample_format: SampleFormat::I24,
                channels: ChannelLayout::stereo(),
            },
        },
    );
    for number in 1..=2 {
        project.add_input(Input {
            id: domain_input(number),
            name: format!("Input {number}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Bars,
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(domain_input(1), domain_input(2)));
    let scene = scene_id(1);
    project.add_scene(Scene {
        id: scene,
        name: "Program scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            name: "Source".into(),
            source: SourceRef::Input(domain_input(1)),
            enabled: true,
            geometry: LayerGeometry::new(0, 0, 16, 16, Rotation::Deg0),
            crop: None,
            mask: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
    });
    let bus = bus_id(1);
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: output_id(1),
        name: "Program output".into(),
        video_source: scene,
        audio_source: bus,
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: vec!["simulation.output".into()],
    });
    for (index, offset) in [1_u128, 2].into_iter().enumerate() {
        let target = StreamTarget::new(
            StreamTargetId::new(NonZeroU128::new(STREAM_ID_BASE + offset).unwrap()),
            format!("Destination {}", index + 1),
            StreamProtocol::Rtmp,
            StreamEndpoint::parse("127.0.0.1/live").unwrap(),
            Some(StreamKey::parse("live-key-0001").unwrap()),
            output_id(1),
        )
        .unwrap()
        .with_startup(StartupPolicy::ReconcileDesiredState)
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
    ProjectStore::new(path).unwrap().save(&stored).unwrap();
}

fn canonical_project() -> Project {
    canonical_project_at(1_280, 720)
}

fn canonical_project_at(width: u32, height: u32) -> Project {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        project_id(),
        "Process Test",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(width, height).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(44_100).unwrap(),
                sample_format: SampleFormat::I24,
                channels: ChannelLayout::stereo(),
            },
        },
    )
    .with_restart_policy(RestartPolicy::Always)
    // Recorder process tests start daemons with --record-program against this
    // fixture, so its authored desired recording flag is active. Without a
    // configured recorder the flag is inert durable state.
    .with_recording_desired(RecorderSlot::new(1).unwrap(), true);
    for (index, input) in [
        SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(12, 34, 56, 255)),
            SimulatedAudio::Sine { frequency_hz: 997 },
        ),
        SimulatedInput::new(SimulatedVideo::Bars, SimulatedAudio::Silence),
        SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(210, 90, 30, 255)),
            SimulatedAudio::Sine { frequency_hz: 440 },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let number = u128::try_from(index + 1).unwrap();
        project.add_input(Input {
            id: domain_input(number),
            name: format!("Simulated {number}"),
            kind: InputKind::Simulated(input),
            required_capabilities: vec![format!("simulation.source.{number}")],
        });
    }
    project.set_main_mix(MainMix::new(domain_input(1), domain_input(2)));
    project.add_scene(Scene {
        id: scene_id(1),
        name: "Program scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            name: "Source".into(),
            source: SourceRef::Input(domain_input(1)),
            enabled: true,
            geometry: LayerGeometry::new(0, 0, 1_280, 720, Rotation::Deg0),
            crop: None,
            mask: Some(RectMask::new(100, 50, 1_000, 600).inverted(true)),
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
    });
    project.add_audio_bus(AudioBus {
        id: bus_id(1),
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    project.add_output(Output {
        id: output_id(1),
        name: "Program output".into(),
        video_source: scene_id(1),
        audio_source: bus_id(1),
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: vec!["simulation.output".into()],
    });
    project
}

fn command(id: &str, key: &str, payload: CommandPayload) -> WireMessage {
    command_version(CURRENT_PROTOCOL_VERSION, id, key, payload)
}

fn command_version(
    protocol: ProtocolVersion,
    id: &str,
    key: &str,
    payload: CommandPayload,
) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol,
        id: id.into(),
        idempotency_key: key.into(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    })
}

fn project_id() -> ProjectId {
    ProjectId::new(NonZeroU128::new(PROJECT_ID).unwrap())
}

fn input(value: u128) -> WireInputId {
    WireInputId::from_domain(domain_input(value))
}

fn domain_input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(INPUT_ID_BASE + value).unwrap())
}

fn scene_id(value: u128) -> SceneId {
    SceneId::new(NonZeroU128::new(INPUT_ID_BASE + 100 + value).unwrap())
}

fn bus_id(value: u128) -> BusId {
    BusId::new(NonZeroU128::new(INPUT_ID_BASE + 200 + value).unwrap())
}

fn output_id(value: u128) -> OutputId {
    OutputId::new(NonZeroU128::new(INPUT_ID_BASE + 300 + value).unwrap())
}

fn stream_target(offset: u128) -> WireStreamTargetId {
    WireStreamTargetId::new(NonZeroU128::new(U64_MAX_ID + 500 + offset).unwrap())
}

/// A client that pipelines its commands and then half-closes its write side is
/// still owed an answer to every command the daemon already received.
#[test]
fn pipelined_commands_before_a_write_half_close_are_all_answered() {
    let directory = TestDirectory::new("half-close-pipelined");
    let project_path = directory.project_path();
    create_project(&project_path);

    let daemon = Daemon::start(&project_path);
    let mut client = daemon.connect();
    client
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    assert_eq!(client.handshake(None).current_revision, 0);
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    let mut pipelined = encode_line(&command(
        "pipelined-preview",
        "pipelined-preview-key",
        CommandPayload::SelectPreview { input: input(3) },
    ))
    .unwrap();
    pipelined.push_str(
        &encode_line(&command(
            "pipelined-cut",
            "pipelined-cut-key",
            CommandPayload::Cut,
        ))
        .unwrap(),
    );
    client.writer.write_all(pipelined.as_bytes()).unwrap();
    client.writer.flush().unwrap();
    client.writer.shutdown(std::net::Shutdown::Write).unwrap();

    let mut results = Vec::new();
    loop {
        let mut line = String::new();
        match client.reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if let WireMessage::CommandResult(result) = decode_line(&line).unwrap() {
                    results.push(result);
                }
            }
            Err(error) => panic!("could not read daemon answers: {error}"),
        }
    }
    assert!(
        matches!(
            results.as_slice(),
            [
                CommandResult::Accepted { revision: 1, .. },
                CommandResult::Accepted { revision: 2, .. }
            ]
        ),
        "every pipelined command must be answered: {results:?}"
    );
    drop(client);
    daemon.wait_success();
    assert_eq!(
        ProjectStore::new(&project_path)
            .unwrap()
            .load()
            .unwrap()
            .position()
            .revision,
        2
    );
}

/// Reads records until `found` yields a value, failing after `timeout`.
fn receive_within<T>(
    client: &mut Client,
    timeout: Duration,
    expected: &str,
    mut found: impl FnMut(WireMessage) -> Option<T>,
) -> T {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(value) = found(client.receive()) {
            return value;
        }
    }
    panic!("timed out waiting for {expected}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn simulated_studio_peer_receives_monitor_frames_and_integration_peer_none() {
    use fm_protocol::{MonitorFrameAssembler, MonitorFrameOrigin, MonitorImage, MonitorTap};

    let directory = TestDirectory::new("simulated-monitor-frames");
    let project_path = directory.project_path();
    create_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);

    let mut integration = daemon.connect();
    integration.handshake(None);
    assert!(matches!(integration.receive(), WireMessage::Snapshot(_)));

    let mut studio = daemon.connect();
    studio
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    studio.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "process-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Operator,
        resume_cursor: None,
    }));
    let WireMessage::HandshakeResponse(response) = studio.receive() else {
        panic!("expected handshake response");
    };
    assert!(matches!(
        response.outcome,
        HandshakeOutcome::Snapshot { .. }
    ));
    let server = response.server.clone();
    assert!(matches!(studio.receive(), WireMessage::Snapshot(_)));

    let mut assemblers = [MonitorFrameAssembler::new(), MonitorFrameAssembler::new()];
    let mut assemble = |message: WireMessage| -> Option<MonitorImage> {
        let WireMessage::MonitorFrame(part) = message else {
            // Any other record mid-group would be an interleave violation.
            assert!(
                assemblers
                    .iter()
                    .all(|assembler| !assembler.is_in_progress())
            );
            return None;
        };
        assert_eq!(part.server, server);
        assert!(
            !assemblers[1 - part.tap.index()].is_in_progress(),
            "monitor frame groups interleaved"
        );
        assemblers[part.tap.index()].push(&part).unwrap()
    };

    let mut latest: [Option<MonitorImage>; 2] = [None, None];
    while latest.iter().any(Option::is_none) {
        let image = receive_within(&mut studio, Duration::from_secs(5), "monitor frames", |m| {
            assemble(m)
        });
        let index = image.tap.index();
        latest[index] = Some(image);
    }
    let [Some(preview), Some(program)] = latest else {
        unreachable!("both taps were received");
    };
    for image in [&preview, &program] {
        assert_eq!(image.origin, MonitorFrameOrigin::Simulated);
        assert_eq!((image.width, image.height), (640, 360));
    }
    assert_eq!(preview.input, input(2));
    assert_eq!(program.input, input(1));
    assert!(
        program
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == [12, 34, 56, 255])
    );
    // The bars' first column is 75% white.
    assert_eq!(&preview.rgba[..4], &[191, 191, 191, 255]);

    studio.send(&command(
        "monitor-cut",
        "monitor-cut-key",
        CommandPayload::Cut,
    ));
    let cut = receive_within(
        &mut studio,
        Duration::from_secs(5),
        "program frame after Cut",
        |message| {
            assemble(message)
                .filter(|image| image.tap == MonitorTap::Program && image.input == input(2))
        },
    );
    assert!(cut.sequence > program.sequence);
    assert_eq!(&cut.rgba[..4], &[191, 191, 191, 255]);

    // The Integration peer saw the Cut's events but never a monitor frame.
    integration
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut integration_records = 0;
    loop {
        let mut line = String::new();
        match integration.reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let message = decode_line(&line).unwrap();
                assert!(
                    !matches!(message, WireMessage::MonitorFrame(_)),
                    "integration peers must not receive monitor frames"
                );
                integration_records += 1;
            }
        }
    }
    assert!(
        integration_records > 0,
        "the Cut was replicated to the integration peer"
    );

    drop(studio);
    drop(integration);
    daemon.stop();
}

/// A Studio peer whose Program and Preview monitor images can be awaited.
struct GradedMonitor {
    client: Client,
    assemblers: [fm_protocol::MonitorFrameAssembler; 2],
}

impl GradedMonitor {
    /// Connects a Studio peer as `role` and returns it with every record
    /// that followed its snapshot before the first monitor frame.
    fn connect(daemon: &Daemon, role: Role) -> (Self, fm_protocol::SnapshotMessage) {
        let mut client = daemon.connect();
        client
            .reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "process-test".into(),
            client_type: ClientType::Studio,
            desired_role: role,
            resume_cursor: None,
        }));
        assert!(matches!(
            client.receive(),
            WireMessage::HandshakeResponse(_)
        ));
        let WireMessage::Snapshot(snapshot) = client.receive() else {
            panic!("expected snapshot");
        };
        (
            Self {
                client,
                assemblers: [
                    fm_protocol::MonitorFrameAssembler::new(),
                    fm_protocol::MonitorFrameAssembler::new(),
                ],
            },
            snapshot,
        )
    }

    /// The next `tap` image `accept` takes.
    fn image(
        &mut self,
        tap: fm_protocol::MonitorTap,
        expected: &str,
        accept: impl Fn(&fm_protocol::MonitorImage) -> bool,
    ) -> fm_protocol::MonitorImage {
        let assemblers = &mut self.assemblers;
        receive_within(
            &mut self.client,
            Duration::from_secs(10),
            expected,
            |message| {
                let WireMessage::MonitorFrame(part) = message else {
                    return None;
                };
                assemblers[part.tap.index()]
                    .push(&part)
                    .unwrap()
                    .filter(|image| image.tap == tap && accept(image))
            },
        )
    }
}

fn grey(pixel: [u8; 4]) -> bool {
    let [red, green, blue, _] = pixel.map(i16::from);
    (red - green).abs() <= 1 && (green - blue).abs() <= 1
}

fn all_pixels(image: &fm_protocol::MonitorImage, accept: impl Fn([u8; 4]) -> bool) -> bool {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| accept(*pixel))
}

/// SW-019 stage 3 end to end on the simulated path: a LUT imported over
/// Protocol 2.28 and an input grade colour that input wherever it airs, a
/// program grade colours the composed Program monitor, every grade edit is
/// journaled, and after a restart the snapshot announces the same grading,
/// which renders the same picture from the bundled LUT file.
#[test]
#[allow(clippy::too_many_lines)]
fn grade_commands_change_monitor_output_and_survive_restart() {
    use fm_protocol::{GradeEntry, MonitorTap, WireGradeSource};

    const INVERT_CUBE: &str =
        "LUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

    let directory = TestDirectory::new("grade-commands");
    let project_path = directory.project_path();
    create_project(&project_path);
    let lut = NonZeroU32::new(3).unwrap();
    let inverted = fm_grade::Grade {
        lut: Some(fm_grade::GradeLut::new(
            fm_model::lut_asset_uri(fm_model::LutId::new(lut)),
            1_000,
        )),
        ..fm_grade::Grade::IDENTITY
    };
    let monochrome = fm_grade::Grade {
        saturation_permille: 0,
        ..fm_grade::Grade::IDENTITY
    };

    let graded_program = {
        let daemon = Daemon::start_without_once(&project_path);
        let (mut monitor, snapshot) = GradedMonitor::connect(&daemon, Role::Graphics);
        assert_eq!(snapshot.grade_states, 0);
        // Program is the solid (12, 34, 56) input 1; Preview is bars.
        let program = monitor.image(MonitorTap::Program, "ungraded program", |_| true);
        assert!(all_pixels(&program, |pixel| pixel == [12, 34, 56, 255]));
        let preview = monitor.image(MonitorTap::Preview, "ungraded preview", |_| true);
        assert!(!all_pixels(&preview, grey), "bars have colour");

        // Viewers cannot grade.
        let mut viewer = daemon.connect();
        viewer.handshake_as(Role::Viewer, None);
        permission_denied(
            &mut viewer,
            "viewer-grade",
            CommandPayload::SetProgramGrade { source: None },
        );

        for (id, payload) in [
            (
                "import",
                CommandPayload::ImportGradeLut {
                    lut,
                    name: "Invert".into(),
                    data: INVERT_CUBE.into(),
                },
            ),
            (
                "input-1",
                CommandPayload::SetInputGrade {
                    input: input(1),
                    source: Some(WireGradeSource::Inline(Box::new(inverted.clone()))),
                },
            ),
            (
                "input-2",
                CommandPayload::SetInputGrade {
                    input: input(2),
                    source: Some(WireGradeSource::Inline(Box::new(monochrome.clone()))),
                },
            ),
        ] {
            monitor
                .client
                .send(&command(id, &format!("{id}-key"), payload));
        }
        // The inverted input makes Program warm instead of blue; the
        // monochrome input grade makes the bars grey.
        let program = monitor.image(MonitorTap::Program, "inverted program", |image| {
            all_pixels(image, |[red, _, blue, _]| red > 200 && blue < 210)
        });
        assert_eq!(program.input, input(1));
        monitor.image(MonitorTap::Preview, "grey preview", |image| {
            all_pixels(image, grey)
        });

        monitor.client.send(&command(
            "program",
            "program-key",
            CommandPayload::SetProgramGrade {
                source: Some(WireGradeSource::Inline(Box::new(monochrome.clone()))),
            },
        ));
        let program = monitor.image(MonitorTap::Program, "monochrome program", |image| {
            all_pixels(image, grey)
        });
        drop(monitor);
        drop(viewer);
        daemon.stop();
        program.rgba[..4].to_vec()
    };

    // The manifest names the LUT and its file holds the imported text.
    let store = ProjectStore::new(&project_path).unwrap();
    let lut_file = store.assets_root().join("luts/3.cube");
    assert_eq!(std::fs::read_to_string(lut_file).unwrap(), INVERT_CUBE);

    let daemon = Daemon::start_without_once(&project_path);
    let (mut monitor, snapshot) = GradedMonitor::connect(&daemon, Role::Graphics);
    assert_eq!(snapshot.grade_states, 4);
    let mut entries = Vec::new();
    while entries.len() < 4 {
        if let WireMessage::GradeState(state) = monitor.client.receive() {
            assert_eq!(state.revision, snapshot.revision);
            entries.push(state.entry);
        }
    }
    assert!(matches!(
        &entries[0],
        GradeEntry::Lut { lut: id, size: 2, .. } if *id == lut
    ));
    assert_eq!(
        entries[1..],
        [
            GradeEntry::Input {
                input: input(1),
                source: WireGradeSource::Inline(Box::new(inverted)),
            },
            GradeEntry::Input {
                input: input(2),
                source: WireGradeSource::Inline(Box::new(monochrome)),
            },
            GradeEntry::Program {
                source: WireGradeSource::Inline(Box::new(fm_grade::Grade {
                    saturation_permille: 0,
                    ..fm_grade::Grade::IDENTITY
                })),
            },
        ]
    );
    let program = monitor.image(MonitorTap::Program, "restored program", |_| true);
    assert_eq!(&program.rgba[..4], &graded_program[..]);
    assert!(all_pixels(&program, grey));

    // Clearing the program grade is journaled like any edit and undoable.
    monitor.client.send(&command(
        "clear",
        "clear-key",
        CommandPayload::SetProgramGrade { source: None },
    ));
    monitor.image(MonitorTap::Program, "ungraded program again", |image| {
        all_pixels(image, |[red, _, blue, _]| red > blue + 20)
    });
    monitor.client.send(&command(
        "undo",
        "undo-key",
        CommandPayload::Undo {
            expected_edit: None,
        },
    ));
    monitor.image(
        MonitorTap::Program,
        "program grade restored by undo",
        |image| all_pixels(image, grey),
    );
    drop(monitor);
    daemon.stop();
}

const TITLE_TEMPLATE_KEY: &str = "titles/process-title.fmtitle";

/// The canonical 1280x720 project plus title input 4 (realized Preview)
/// whose stored template draws one rectangle at (200, 100) sized 400x200,
/// tinted by color field 1, on a transparent background. No font is needed.
fn create_title_project(path: &Path) {
    use fm_titles::{
        Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldValue,
        Style, TemplateId, TitleTemplate,
    };

    let field = FieldId::new(NonZeroU128::new(1).unwrap());
    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(1).unwrap()),
        name: "Process title".into(),
        width: 1_280,
        height: 720,
        background: Color::TRANSPARENT,
        fields: vec![FieldDefinition {
            id: field,
            name: "Bar".into(),
            default: FieldValue::Color(Color::new(1, 2, 3, 255)),
        }],
        elements: vec![Element {
            id: ElementId::new(NonZeroU128::new(1).unwrap()),
            name: "Bar".into(),
            bounds: Bounds {
                x: 200,
                y: 100,
                width: 400,
                height: 200,
            },
            z_index: 0,
            visible: true,
            style: Style::default(),
            color_field: Some(field),
            kind: ElementKind::Rectangle,
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
    let template_path = store.assets_root().join(TITLE_TEMPLATE_KEY);
    fs::create_dir_all(template_path.parent().unwrap()).unwrap();
    fs::write(&template_path, document).unwrap();

    let mut project = canonical_project();
    project.add_input(Input {
        id: domain_input(4),
        name: "Lower third".into(),
        kind: InputKind::Title(fm_model::TitleInput {
            template_uri: format!("asset://{TITLE_TEMPLATE_KEY}"),
            field_revision: 3,
            fields: vec![fm_model::TitleFieldEntry {
                id: fm_model::TitleFieldId::new(NonZeroU128::new(1).unwrap()),
                value: fm_model::TitleFieldValue::Color(Rgba8::new(230, 20, 140, 255)),
            }],
        }),
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(domain_input(1), domain_input(4)));
    let project = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(domain_input(1)),
            realized_program_id: Some(domain_input(1)),
            desired_preview_id: Some(domain_input(4)),
            realized_preview_id: Some(domain_input(4)),
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
    store.save(&project).unwrap();
}

/// Whether `image` is `inside` exactly on `x0..x1` x `y0..y1` and `outside`
/// everywhere else.
fn shows_rectangle(
    image: &fm_protocol::MonitorImage,
    (x0, y0, x1, y1): (usize, usize, usize, usize),
    inside: [u8; 4],
    outside: [u8; 4],
) -> bool {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .all(|(index, pixel)| {
            let x = index % usize::from(image.width);
            let y = index / usize::from(image.width);
            let expected = if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                inside
            } else {
                outside
            };
            *pixel == expected
        })
}

#[test]
fn simulated_monitor_shows_title_inputs_and_title_overlays() {
    use fm_protocol::{MonitorFrameAssembler, MonitorImage, MonitorTap, WireOverlayChannelId};

    let directory = TestDirectory::new("simulated-title-monitor");
    let project_path = directory.project_path();
    create_title_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);

    let mut studio = daemon.connect();
    studio
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    studio.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "process-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Operator,
        resume_cursor: None,
    }));
    assert!(matches!(
        studio.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    assert!(matches!(studio.receive(), WireMessage::Snapshot(_)));

    let mut assemblers = [MonitorFrameAssembler::new(), MonitorFrameAssembler::new()];
    let mut assemble = |message: WireMessage| -> Option<MonitorImage> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        assemblers[part.tap.index()].push(&part).unwrap()
    };
    // The 1280x720 template rectangle lands on (100, 50)..(300, 150) of the
    // 640x360 monitor picture.
    let bar = (100, 50, 300, 150);
    let title = [230, 20, 140, 255];

    // Preview shows the title's persisted field values over black.
    let preview = receive_within(
        &mut studio,
        Duration::from_secs(10),
        "title preview frame",
        |message| assemble(message).filter(|image| image.tap == MonitorTap::Preview),
    );
    assert_eq!(preview.input, input(4));
    assert_eq!((preview.width, preview.height), (640, 360));
    assert!(shows_rectangle(&preview, bar, title, [0, 0, 0, 255]));

    // Taking the title on overlay channel 1 composites it over Program.
    studio.send(&command(
        "title-overlay",
        "title-overlay-key",
        CommandPayload::TakeOverlay {
            channel: WireOverlayChannelId::new(1).unwrap(),
            source: input(4),
        },
    ));
    let program = receive_within(
        &mut studio,
        Duration::from_secs(10),
        "program frame with the title overlay",
        |message| {
            assemble(message).filter(|image| {
                image.tap == MonitorTap::Program
                    && shows_rectangle(image, bar, title, [12, 34, 56, 255])
            })
        },
    );
    assert_eq!(program.input, input(1));

    drop(studio);
    daemon.stop();
}

/// A 1080p project whose Program is a Scene input over simulated sources, so
/// every Studio monitor picture of Program is a CPU Scene composition.
fn create_scene_program_project(path: &Path) {
    let mut project = canonical_project_at(1_920, 1_080);
    project.add_input(Input {
        id: domain_input(4),
        name: "Program scene".into(),
        kind: InputKind::Scene {
            scene_id: scene_id(1),
            audio_source: None,
        },
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(domain_input(4), domain_input(2)));
    let project = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(domain_input(4)),
            realized_program_id: Some(domain_input(4)),
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
    ProjectStore::new(path).unwrap().save(&project).unwrap();
}

/// Receives the next state-bearing session event before `deadline`, skipping
/// lossy monitor frame parts.
#[cfg(unix)]
fn next_session_event(session: &mut TcpSession, deadline: Instant, expected: &str) -> SessionEvent {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for {expected}; the session loop was blocked"
        );
        match session
            .receive_timeout(remaining.min(Duration::from_millis(25)))
            .unwrap()
        {
            None | Some(SessionEvent::MonitorFrame { .. }) => {}
            Some(event) => return event,
        }
    }
}

/// Regression for a Studio session drop on Stinger ASSIGN against a
/// simulated daemon: with a Scene on Program, the session loop composed the
/// Scene's monitor picture (seconds of CPU work in a debug build) between
/// executing the command and writing its result. Studio's bounded command
/// wait expired, it treated the silent session as dead and reconnected. The
/// acknowledgement, its durable event, and its runtime event must never wait
/// for monitor rendering, and the session must stay up afterwards.
#[cfg(unix)]
#[test]
fn studio_stinger_assignment_is_acknowledged_while_a_scene_monitor_composes() {
    // Well under Studio's own command wait; the blocking composition alone
    // exceeded it.
    const ACKNOWLEDGEMENT_BOUND: Duration = Duration::from_secs(2);

    let directory = TestDirectory::new("studio-stinger-assignment");
    let project_path = directory.project_path();
    create_scene_program_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);
    let mut session = TcpSession::new(
        ProtocolClient::new(ClientConfig::new(
            "process-test",
            ClientType::Studio,
            Role::Operator,
            "studio-client",
            project_id(),
        ))
        .unwrap(),
    );
    assert!(matches!(
        session
            .connect(daemon.address, Duration::from_secs(5))
            .unwrap(),
        SessionEvent::Connected { .. }
    ));

    // A composed Program picture proves monitor demand is active, so the
    // accepted command below changes the revision a composition depends on.
    let first_frame = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(Instant::now() < first_frame, "no Program monitor frame");
        if let Some(SessionEvent::MonitorFrame {
            tap: fm_protocol::MonitorTap::Program,
            complete: true,
        }) = session.receive_timeout(Duration::from_millis(25)).unwrap()
        {
            break;
        }
    }

    let command = session
        .queue_command(
            CommandPayload::ConfigureStinger {
                slot: WireStingerSlotId::new(1).unwrap(),
                media_input: input(3),
                preload: true,
                cut_point_frames: 15,
                audio_policy: StingerAudioPolicy::MixWithProgram,
                missing_media_fallback: StingerMissingMediaFallback::Cut,
            },
            "studio-stinger-assign",
            Some(0),
            None,
        )
        .unwrap();
    session.flush().unwrap();
    let deadline = Instant::now() + ACKNOWLEDGEMENT_BOUND;

    let SessionEvent::CommandResult {
        result: CommandResult::Accepted { id, revision, .. },
        ..
    } = next_session_event(&mut session, deadline, "the assignment result")
    else {
        panic!("expected the assignment to be accepted");
    };
    assert_eq!((id.as_str(), revision), (command.id.as_str(), 1));
    assert!(matches!(
        next_session_event(&mut session, deadline, "the Stinger slot event"),
        SessionEvent::Event { event, .. }
            if event.cursor.revision == 1
                && matches!(
                    &event.payload,
                    EventPayload::StingerSlotsChanged { stingers, .. }
                        if stingers.len() == 1 && stingers[0].media_input == input(3)
                )
    ));
    assert!(matches!(
        next_session_event(&mut session, deadline, "the realization event"),
        SessionEvent::RuntimeEvent { event, .. } if event.revision == 1
    ));

    // The same session keeps answering: it was never dropped.
    let sent_at_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let heartbeat = session.queue_heartbeat(sent_at_ms).unwrap();
    session.flush().unwrap();
    let deadline = Instant::now() + ACKNOWLEDGEMENT_BOUND;
    assert!(matches!(
        next_session_event(&mut session, deadline, "the heartbeat acknowledgement"),
        SessionEvent::HeartbeatAcknowledged { acknowledgement }
            if acknowledgement.heartbeat_sequence == heartbeat.sequence
    ));
    assert!(session.is_connected());
    assert_eq!(
        session.client().model().view().unwrap().stingers.len(),
        1,
        "the replicated view holds the assigned slot"
    );
    daemon.stop();
}

/// Reads until the durable event at `revision`, skipping runtime records.
fn durable_event_at(client: &mut Client, revision: u64) -> fm_protocol::EventMessage {
    loop {
        match client.receive() {
            WireMessage::Event(event) if event.cursor.revision == revision => return event,
            WireMessage::Event(event) => {
                assert!(
                    event.cursor.revision < revision,
                    "skipped past revision {revision}: {event:?}"
                );
            }
            _ => {}
        }
    }
}

fn accepted_revision(client: &mut Client, expected_id: &str) -> u64 {
    match client.next_result() {
        CommandResult::Accepted { id, revision, .. } if id == expected_id => revision,
        other => panic!("expected {expected_id} to be accepted, got {other:?}"),
    }
}

fn snapshot_of(daemon: &Daemon, role: Role) -> fm_protocol::SnapshotMessage {
    let mut client = daemon.connect();
    client.handshake_as(role, None);
    let WireMessage::Snapshot(snapshot) = client.receive() else {
        panic!("expected snapshot");
    };
    snapshot
}

fn input_name(snapshot: &fm_protocol::SnapshotMessage, id: u128) -> String {
    snapshot
        .inputs
        .iter()
        .find(|status| status.input == input(id))
        .unwrap()
        .name
        .clone()
}

/// Protocol 2.22 undo/redo and the edit padlock, end to end through the
/// daemon: a rename is undone and redone (each carrying the post-revision
/// history summary), the padlock refuses project edits with a stable reason
/// while live switching still cuts, and the journaled history survives a
/// kill plus recovery as the resolved edits (session history does not).
#[test]
#[allow(clippy::too_many_lines)]
fn undo_redo_and_padlock_round_trip_through_the_daemon() {
    use fm_protocol::{EditHistoryStatus, EditLockScope};

    let directory = TestDirectory::new("undo-redo-padlock");
    let project_path = directory.project_path();
    create_rename_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);

    let initial = snapshot_of(&daemon, Role::Viewer);
    assert_eq!(initial.edit_history, EditHistoryStatus::EMPTY);
    assert!(!initial.edit_locks.project);

    let mut admin = daemon.connect();
    admin.handshake_as(Role::Admin, None);
    assert!(matches!(admin.receive(), WireMessage::Snapshot(_)));

    // An empty history refuses undo with the stable engine code.
    admin.send(&command(
        "undo-empty",
        "undo-empty-key",
        CommandPayload::Undo {
            expected_edit: None,
        },
    ));
    assert!(matches!(
        admin.next_result(),
        CommandResult::Rejected { fields, .. }
            if fields.first().map(|field| field.code.as_str()) == Some("nothing_to_undo")
    ));

    admin.send(&command(
        "rename",
        "rename-key",
        CommandPayload::RenameInput {
            input: input(1),
            name: "Camera Left".into(),
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "rename"), 1);
    let renamed = durable_event_at(&mut admin, 1);
    assert_eq!(
        renamed.edit_history,
        EditHistoryStatus {
            undo_depth: 1,
            next_undo: Some(1),
            redo_depth: 0,
            next_redo: None,
        }
    );

    admin.send(&command(
        "undo",
        "undo-key",
        CommandPayload::Undo {
            expected_edit: Some(1),
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "undo"), 2);
    let undone = durable_event_at(&mut admin, 2);
    assert!(matches!(
        undone.payload,
        EventPayload::InputRenamed { input: id, ref name } if id == input(1) && name == "Input 1"
    ));
    assert_eq!(
        undone.edit_history,
        EditHistoryStatus {
            undo_depth: 0,
            next_undo: None,
            redo_depth: 1,
            next_redo: Some(1),
        }
    );

    admin.send(&command(
        "redo",
        "redo-key",
        CommandPayload::Redo {
            expected_edit: Some(1),
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "redo"), 3);
    let redone = durable_event_at(&mut admin, 3);
    assert!(matches!(
        redone.payload,
        EventPayload::InputRenamed { input: id, ref name }
            if id == input(1) && name == "Camera Left"
    ));
    assert_eq!(redone.edit_history.undo_depth, 1);
    assert_eq!(redone.edit_history.redo_depth, 0);

    admin.send(&command(
        "lock",
        "lock-key",
        CommandPayload::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "lock"), 4);
    let locked = durable_event_at(&mut admin, 4);
    assert!(matches!(
        locked.payload,
        EventPayload::EditLockChanged {
            scope: EditLockScope::Project,
            locked: true,
        }
    ));
    let locked_snapshot = snapshot_of(&daemon, Role::Viewer);
    assert!(locked_snapshot.edit_locks.project);
    assert_eq!(locked_snapshot.edit_history.undo_depth, 1);

    // The padlock refuses project edits and history with stable codes...
    for (id, payload) in [
        (
            "rename-locked",
            CommandPayload::RenameInput {
                input: input(1),
                name: "Refused".into(),
            },
        ),
        (
            "undo-locked",
            CommandPayload::Undo {
                expected_edit: None,
            },
        ),
    ] {
        admin.send(&command(id, &format!("{id}-key"), payload));
        assert!(matches!(
            admin.next_result(),
            CommandResult::Rejected { fields, current_revision: 4, .. }
                if fields.first().map(|field| field.code.as_str()) == Some("project_locked")
        ));
    }

    // ...but never live switching.
    admin.send(&command(
        "cut-locked",
        "cut-locked-key",
        CommandPayload::Cut,
    ));
    assert_eq!(accepted_revision(&mut admin, "cut-locked"), 5);
    durable_event_at(&mut admin, 5);

    admin.send(&command(
        "unlock",
        "unlock-key",
        CommandPayload::SetEditLock {
            scope: EditLockScope::Project,
            locked: false,
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "unlock"), 6);
    durable_event_at(&mut admin, 6);
    admin.send(&command(
        "rename-unlocked",
        "rename-unlocked-key",
        CommandPayload::RenameInput {
            input: input(2),
            name: "Slides".into(),
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "rename-unlocked"), 7);
    drop(admin);

    let settled = snapshot_of(&daemon, Role::Viewer);
    assert!(!settled.edit_locks.project);
    assert_eq!(input_name(&settled, 1), "Camera Left");
    assert_eq!(input_name(&settled, 2), "Slides");
    assert_eq!(settled.edit_history.next_undo, Some(7));
    daemon.stop();

    // Journal recovery replays the undo and redo as the edits they resolved
    // to; session history itself starts empty after restart.
    let daemon = Daemon::start(&project_path);
    let recovered = snapshot_of(&daemon, Role::Viewer);
    assert_eq!(input_name(&recovered, 1), "Camera Left");
    assert_eq!(input_name(&recovered, 2), "Slides");
    assert_eq!(recovered.edit_history, EditHistoryStatus::EMPTY);
    assert!(!recovered.edit_locks.project);
    daemon.wait_success();
}

/// Protocol 2.23 live input add and remove, end to end through the daemon:
/// an added solid input is announced, gets an audio strip, renders in the
/// Preview monitor tap and can be cut to Program; removing it is refused
/// while it is on Program and accepted once it is off air, and the
/// resulting catalogue (including a second, still present bars input)
/// survives a kill before any checkpoint, journal replay and the clean
/// checkpoint into project.json with its kind.
#[test]
#[allow(clippy::too_many_lines)]
fn live_input_add_and_remove_round_trip_through_kill_and_recovery() {
    use fm_protocol::{LiveInputSource, MonitorFrameAssembler, MonitorImage, MonitorTap};

    fn input_ids(snapshot: &fm_protocol::SnapshotMessage) -> Vec<WireInputId> {
        snapshot.inputs.iter().map(|status| status.input).collect()
    }
    fn strip_ids(snapshot: &fm_protocol::SnapshotMessage) -> Vec<WireInputId> {
        snapshot
            .input_audio_strips
            .iter()
            .map(|strip| strip.input)
            .collect()
    }

    let directory = TestDirectory::new("live-input-catalogue");
    let project_path = directory.project_path();
    create_rename_project(&project_path);
    let store = ProjectStore::new(&project_path).unwrap();
    let daemon = Daemon::start_without_once(&project_path);

    let mut admin = daemon.connect();
    admin.handshake_as(Role::Admin, None);
    assert!(matches!(admin.receive(), WireMessage::Snapshot(_)));

    admin.send(&command(
        "add-guest",
        "add-guest-key",
        CommandPayload::AddInput {
            input: input(3),
            name: "Guest".into(),
            source: LiveInputSource::Solid {
                red: 12,
                green: 34,
                blue: 56,
                alpha: 255,
            },
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "add-guest"), 1);
    let added = durable_event_at(&mut admin, 1);
    assert!(matches!(
        added.payload,
        EventPayload::InputAdded { input: id, ref name } if id == input(3) && name == "Guest"
    ));
    let with_guest = snapshot_of(&daemon, Role::Viewer);
    assert_eq!(input_ids(&with_guest), vec![input(1), input(2), input(3)]);
    assert_eq!(input_name(&with_guest, 3), "Guest");
    assert_eq!(strip_ids(&with_guest), vec![input(1), input(2), input(3)]);

    // A duplicate id is refused without advancing the revision.
    admin.send(&command(
        "add-duplicate",
        "add-duplicate-key",
        CommandPayload::AddInput {
            input: input(3),
            name: "Other".into(),
            source: LiveInputSource::Bars,
        },
    ));
    assert!(matches!(
        admin.next_result(),
        CommandResult::Rejected { fields, current_revision: 1, .. }
            if fields.first().map(|field| field.code.as_str()) == Some("duplicate_input")
    ));

    // A Studio peer watches the monitor taps. The simulated daemon admits
    // only two raw peers and leaves a third connection unaccepted, so this
    // peer is dropped before `snapshot_of` opens another one.
    let mut studio = daemon.connect();
    studio
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    studio.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "process-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Viewer,
        resume_cursor: None,
    }));
    assert!(matches!(
        studio.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    assert!(matches!(studio.receive(), WireMessage::Snapshot(_)));

    // The new input renders in the Preview tap once it is selected.
    admin.send(&command(
        "preview-guest",
        "preview-guest-key",
        CommandPayload::SelectPreview { input: input(3) },
    ));
    assert_eq!(accepted_revision(&mut admin, "preview-guest"), 2);
    let mut assemblers = [MonitorFrameAssembler::new(), MonitorFrameAssembler::new()];
    let mut assemble = |message: WireMessage| -> Option<MonitorImage> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        assemblers[part.tap.index()].push(&part).unwrap()
    };
    let guest_preview = receive_within(
        &mut studio,
        Duration::from_secs(5),
        "Preview frame of the added input",
        |message| {
            assemble(message)
                .filter(|image| image.tap == MonitorTap::Preview && image.input == input(3))
        },
    );
    assert!(
        guest_preview
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == [12, 34, 56, 255])
    );

    admin.send(&command("cut-guest", "cut-guest-key", CommandPayload::Cut));
    assert_eq!(accepted_revision(&mut admin, "cut-guest"), 3);
    let guest_program = receive_within(
        &mut studio,
        Duration::from_secs(5),
        "Program frame of the added input",
        |message| {
            assemble(message)
                .filter(|image| image.tap == MonitorTap::Program && image.input == input(3))
        },
    );
    assert_eq!(&guest_program.rgba[..4], &[12, 34, 56, 255]);
    drop(studio);

    // On Program, removal is refused with the stable reason.
    admin.send(&command(
        "remove-on-air",
        "remove-on-air-key",
        CommandPayload::RemoveInput { input: input(3) },
    ));
    match admin.next_result() {
        CommandResult::Rejected {
            fields,
            current_revision,
            message,
            ..
        } => {
            assert_eq!(current_revision, 3);
            assert_eq!(
                fields.first().map(|field| field.code.as_str()),
                Some("input_in_use")
            );
            assert!(message.contains("on Program"), "{message}");
        }
        other @ CommandResult::Accepted { .. } => {
            panic!("expected the on-air removal to be refused, got {other:?}")
        }
    }

    // Cut away (Guest moves to Preview), move Preview off it, then remove.
    admin.send(&command("cut-away", "cut-away-key", CommandPayload::Cut));
    assert_eq!(accepted_revision(&mut admin, "cut-away"), 4);
    admin.send(&command(
        "preview-away",
        "preview-away-key",
        CommandPayload::SelectPreview { input: input(2) },
    ));
    assert_eq!(accepted_revision(&mut admin, "preview-away"), 5);
    admin.send(&command(
        "remove-guest",
        "remove-guest-key",
        CommandPayload::RemoveInput { input: input(3) },
    ));
    assert_eq!(accepted_revision(&mut admin, "remove-guest"), 6);
    let removed = durable_event_at(&mut admin, 6);
    assert!(matches!(
        removed.payload,
        EventPayload::InputRemoved { input: id } if id == input(3)
    ));

    // A second, bars input stays in the catalogue across the kill.
    admin.send(&command(
        "add-bars",
        "add-bars-key",
        CommandPayload::AddInput {
            input: input(4),
            name: "Bars".into(),
            source: LiveInputSource::Bars,
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "add-bars"), 7);
    durable_event_at(&mut admin, 7);
    let settled = snapshot_of(&daemon, Role::Viewer);
    assert_eq!(input_ids(&settled), vec![input(1), input(2), input(4)]);
    assert_eq!(strip_ids(&settled), vec![input(1), input(2), input(4)]);
    drop(admin);
    daemon.stop();

    // The catalogue changes exist only as journal batches at this instant.
    let crashed = store.load().unwrap();
    assert_eq!(crashed.position().revision, 0);
    assert_eq!(crashed.project().inputs().len(), 2);
    assert_eq!(store.scan_journal().unwrap().batches().len(), 7);

    // Journal replay restores the catalogue, strips and routing.
    let daemon = Daemon::start(&project_path);
    let recovered = snapshot_of(&daemon, Role::Viewer);
    assert_eq!(recovered.revision, 7);
    assert_eq!(input_ids(&recovered), vec![input(1), input(2), input(4)]);
    assert_eq!(input_name(&recovered, 4), "Bars");
    assert_eq!(strip_ids(&recovered), vec![input(1), input(2), input(4)]);
    assert_eq!(recovered.desired_program, input(1));
    assert_eq!(recovered.desired_preview, input(2));
    daemon.wait_success();

    // The clean shutdown checkpointed the catalogue with the added kind.
    let checkpointed = store.load().unwrap();
    assert_eq!(checkpointed.position().revision, 7);
    assert_eq!(store.scan_journal().unwrap().batches(), []);
    let inputs = checkpointed.project().inputs();
    assert_eq!(
        inputs.iter().map(|input| input.id).collect::<Vec<_>>(),
        vec![domain_input(1), domain_input(2), domain_input(4)]
    );
    assert_eq!(
        inputs[2].kind,
        InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        ))
    );
}

/// Asserts that `payload` is refused with the stable edit-lock `code` at
/// `revision` without advancing it.
fn assert_lock_refusal(
    client: &mut Client,
    id: &str,
    payload: CommandPayload,
    code: &str,
    revision: u64,
) {
    client.send(&command(id, &format!("{id}-key"), payload));
    match client.next_result() {
        CommandResult::Rejected {
            fields,
            current_revision,
            ..
        } => {
            assert_eq!(current_revision, revision, "{id} moved the revision");
            assert_eq!(
                fields.first().map(|field| field.code.as_str()),
                Some(code),
                "{id} was refused for the wrong reason"
            );
        }
        other @ CommandResult::Accepted { .. } => {
            panic!("expected {id} to be refused as {code}, got {other:?}")
        }
    }
}

/// The edit padlock is project state: a project lock and an input lock
/// survive a kill before any checkpoint (journal replay), then a clean
/// checkpoint (manifest restore), and keep refusing edits until released.
#[test]
#[allow(clippy::too_many_lines)]
fn edit_locks_survive_kill_recovery_and_checkpoint_restore() {
    use fm_protocol::{EditLockScope, EditLockStatus};

    fn rename(name: &str) -> CommandPayload {
        CommandPayload::RenameInput {
            input: input(1),
            name: name.into(),
        }
    }

    let directory = TestDirectory::new("edit-locks-durable");
    let project_path = directory.project_path();
    create_rename_project(&project_path);
    let store = ProjectStore::new(&project_path).unwrap();
    let both_locked = EditLockStatus {
        project: true,
        inputs: vec![input(1)],
    };

    // Engage both padlocks, then kill the daemon before any checkpoint.
    let daemon = Daemon::start_without_once(&project_path);
    let mut admin = daemon.connect();
    admin.handshake_as(Role::Admin, None);
    assert!(matches!(admin.receive(), WireMessage::Snapshot(_)));
    for (id, scope) in [
        ("lock-project", EditLockScope::Project),
        ("lock-input", EditLockScope::Input(input(1))),
    ] {
        admin.send(&command(
            id,
            &format!("{id}-key"),
            CommandPayload::SetEditLock {
                scope,
                locked: true,
            },
        ));
        accepted_revision(&mut admin, id);
    }
    assert_eq!(snapshot_of(&daemon, Role::Viewer).edit_locks, both_locked);
    drop(admin);
    daemon.stop();

    // The locks exist only as journal batches at this instant.
    let crashed = store.load().unwrap();
    assert_eq!(crashed.position().revision, 0);
    assert!(!crashed.project().project_locked());
    assert!(crashed.project().locked_inputs().is_empty());
    assert_eq!(store.scan_journal().unwrap().batches().len(), 2);

    // Journal replay restores both locks, and they still refuse edits.
    let daemon = Daemon::start(&project_path);
    let mut admin = daemon.connect();
    assert_eq!(admin.handshake_as(Role::Admin, None).current_revision, 2);
    let WireMessage::Snapshot(replayed) = admin.receive() else {
        panic!("expected snapshot");
    };
    assert_eq!(replayed.edit_locks, both_locked);
    assert_lock_refusal(
        &mut admin,
        "rename-replayed",
        rename("Refused"),
        "project_locked",
        2,
    );
    drop(admin);
    daemon.wait_success();

    // The clean shutdown checkpointed the locks into project.json.
    let checkpointed = store.load().unwrap();
    assert_eq!(checkpointed.position().revision, 2);
    assert!(checkpointed.project().project_locked());
    assert_eq!(
        checkpointed
            .project()
            .locked_inputs()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![domain_input(1)]
    );
    assert_eq!(store.scan_journal().unwrap().batches(), []);
    let manifest = std::fs::read_to_string(store.manifest_path()).unwrap();
    assert!(manifest.contains(&format!(
        "\"edit_locks\": {{\"project\": true, \"inputs\": [{}]}}",
        domain_input(1)
    )));

    // Restoring from that checkpoint alone yields the identical lock state.
    let daemon = Daemon::start_without_once(&project_path);
    let restored = snapshot_of(&daemon, Role::Viewer);
    assert_eq!(restored.edit_locks, replayed.edit_locks);
    let mut admin = daemon.connect();
    admin.handshake_as(Role::Admin, None);
    assert!(matches!(admin.receive(), WireMessage::Snapshot(_)));
    assert_lock_refusal(
        &mut admin,
        "rename-restored",
        rename("Refused"),
        "project_locked",
        2,
    );

    // Releasing the project lock leaves the input lock in force.
    admin.send(&command(
        "unlock-project",
        "unlock-project-key",
        CommandPayload::SetEditLock {
            scope: EditLockScope::Project,
            locked: false,
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "unlock-project"), 3);
    assert_lock_refusal(
        &mut admin,
        "rename-input-locked",
        rename("Refused"),
        "input_locked",
        3,
    );
    admin.send(&command(
        "unlock-input",
        "unlock-input-key",
        CommandPayload::SetEditLock {
            scope: EditLockScope::Input(input(1)),
            locked: false,
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "unlock-input"), 4);
    admin.send(&command(
        "rename-unlocked",
        "rename-unlocked-key",
        rename("Camera"),
    ));
    assert_eq!(accepted_revision(&mut admin, "rename-unlocked"), 5);
    drop(admin);
    daemon.stop();

    // The releases replay too: the recovered show is unlocked and renamed.
    let daemon = Daemon::start(&project_path);
    let mut viewer = daemon.connect();
    assert_eq!(viewer.handshake_as(Role::Viewer, None).current_revision, 5);
    let WireMessage::Snapshot(released) = viewer.receive() else {
        panic!("expected snapshot");
    };
    assert_eq!(
        released.edit_locks,
        EditLockStatus {
            project: false,
            inputs: Vec::new(),
        }
    );
    assert_eq!(input_name(&released, 1), "Camera");
    drop(viewer);
    daemon.wait_success();
    let settled = store.load().unwrap();
    assert!(!settled.project().project_locked());
    assert!(settled.project().locked_inputs().is_empty());
}

/// A raw heartbeat for `server` whose cursor is at `revision`.
fn heartbeat_at(server: &ServerIdentity, sequence: u64, revision: u64) -> WireMessage {
    WireMessage::Heartbeat(HeartbeatMessage {
        server: server.clone(),
        sequence,
        sent_at_ms: sequence,
        last_applied: Some(ResumeCursor {
            server: server.clone(),
            revision,
        }),
    })
}

/// A Studio peer that completes its handshake and then never touches its
/// socket again must not delay or disconnect another peer. Monitor frames and
/// live events pile up for it. The admin peer keeps getting heartbeat
/// acknowledgements and command results within a bounded time, for longer
/// than the 15 s session heartbeat timeout, while only the stalled peer is
/// closed (by its own heartbeat timeout). A full kernel buffer on the stalled
/// side is covered by the session loop's unit tests, because loopback
/// buffers here grow to tens of megabytes.
///
/// The simulated daemon admits two raw peers, so this test opens no third
/// connection while the stalled peer holds its slot.
#[test]
#[allow(clippy::too_many_lines)]
fn a_non_reading_studio_peer_never_stalls_another_peer() {
    const ROUND_TRIP_LIMIT: Duration = Duration::from_secs(2);
    // Past the stalled peer's 15 s heartbeat timeout and its close.
    const OBSERVATION: Duration = Duration::from_millis(16_500);

    let directory = TestDirectory::new("non-reading-studio-peer");
    let project_path = directory.project_path();
    create_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);

    let mut studio = daemon.connect();
    studio
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    studio.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "process-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Operator,
        resume_cursor: None,
    }));
    let WireMessage::HandshakeResponse(response) = studio.receive() else {
        panic!("expected studio handshake response");
    };
    assert!(matches!(
        response.outcome,
        HandshakeOutcome::Snapshot { .. }
    ));
    let server = response.server.clone();
    assert!(matches!(studio.receive(), WireMessage::Snapshot(_)));
    // From here on the Studio peer neither reads nor writes.

    let mut admin = daemon.connect();
    admin
        .reader
        .get_ref()
        .set_read_timeout(Some(ROUND_TRIP_LIMIT))
        .unwrap();
    let handshake = admin.handshake_as(Role::Admin, None);
    assert!(matches!(admin.receive(), WireMessage::Snapshot(_)));
    assert_eq!(handshake.engine.engine_id, server.engine_id);

    let mut sequence = 0_u64;
    let mut revision = handshake.current_revision;
    let mut heartbeat = |admin: &mut Client, revision: u64| -> Duration {
        sequence += 1;
        let sent = Instant::now();
        admin.send(&heartbeat_at(&server, sequence, revision));
        let acknowledged = receive_within(
            admin,
            ROUND_TRIP_LIMIT,
            "admin heartbeat acknowledgement",
            |message| match message {
                WireMessage::HeartbeatAcknowledgement(ack) => Some(ack.heartbeat_sequence),
                _ => None,
            },
        );
        assert_eq!(acknowledged, sequence);
        sent.elapsed()
    };

    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    let mut cuts = 0_u32;
    while started.elapsed() < OBSERVATION {
        slowest = slowest.max(heartbeat(&mut admin, revision));
        // Each Cut also publishes events (and new monitor frames) for the
        // stalled peer.
        cuts += 1;
        let id = format!("admin-cut-{cuts}");
        let sent = Instant::now();
        admin.send(&command(&id, &format!("{id}-key"), CommandPayload::Cut));
        revision = receive_within(
            &mut admin,
            ROUND_TRIP_LIMIT,
            "admin command result",
            |message| match message {
                WireMessage::CommandResult(CommandResult::Accepted {
                    id: result_id,
                    revision,
                    ..
                }) if result_id == id => Some(revision),
                WireMessage::CommandResult(other) => {
                    panic!("admin command was not accepted: {other:?}")
                }
                _ => None,
            },
        );
        slowest = slowest.max(sent.elapsed());
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        slowest < ROUND_TRIP_LIMIT,
        "the admin peer waited {slowest:?} for an answer"
    );
    assert_eq!(revision, handshake.current_revision + u64::from(cuts));

    // The stalled peer was closed on its own: what the daemon had buffered
    // for it drains and then the stream ends.
    let mut sink = [0_u8; 8 * 1024];
    loop {
        match studio.reader.read(&mut sink) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(error) => panic!("the stalled studio peer was never closed: {error}"),
        }
    }

    // The admin peer is still served after the stalled peer is gone.
    assert!(heartbeat(&mut admin, revision) < ROUND_TRIP_LIMIT);

    drop(admin);
    drop(studio);
    daemon.stop();
}

/// Reads the snapshot and the `title_state` records that complete it.
fn title_sync(
    client: &mut Client,
) -> (
    fm_protocol::SnapshotMessage,
    Vec<fm_protocol::TitleStateMessage>,
) {
    let WireMessage::Snapshot(snapshot) = client.receive() else {
        panic!("expected snapshot");
    };
    let states = snapshot
        .titles
        .iter()
        .map(|_| match client.receive() {
            WireMessage::TitleState(state) => state,
            other => panic!("expected title_state right after the snapshot, got {other:?}"),
        })
        .collect();
    (snapshot, states)
}

/// Joins as a viewer and reads the snapshot with the `playlist_state` it
/// announces.
fn playlist_sync(
    daemon: &Daemon,
) -> (
    fm_protocol::SnapshotMessage,
    fm_protocol::PlaylistStateMessage,
) {
    let mut client = daemon.connect();
    client.handshake_as(Role::Viewer, None);
    let (snapshot, _) = title_sync(&mut client);
    assert!(
        snapshot.playlist_follows,
        "the snapshot announces no playlist"
    );
    let WireMessage::PlaylistState(state) = client.receive() else {
        panic!("expected playlist_state right after the snapshot");
    };
    (snapshot, state)
}

fn title_color(red: u8, green: u8, blue: u8) -> Vec<fm_protocol::TitleFieldUpdate> {
    vec![fm_protocol::TitleFieldUpdate {
        field: fm_protocol::WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
        value: fm_protocol::TitleFieldValue::Color {
            red,
            green,
            blue,
            alpha: 255,
        },
    }]
}

fn assert_title_state(
    state: &fm_protocol::TitleStateMessage,
    revision: u64,
    field_revision: u64,
    color: (u8, u8, u8),
) {
    assert_eq!(
        state,
        &fm_protocol::TitleStateMessage {
            revision,
            input: input(4),
            name: "Process title".into(),
            field_revision,
            phase: fm_protocol::TitlePhase::Idle,
            phase_frame: 0,
            phase_frames: 0,
            fields: vec![fm_protocol::TitleFieldStatus {
                field: fm_protocol::WireTitleFieldId::new(NonZeroU128::new(1).unwrap()),
                name: "Bar".into(),
                value: fm_protocol::TitleFieldValue::Color {
                    red: color.0,
                    green: color.1,
                    blue: color.2,
                    alpha: 255,
                },
            }],
        }
    );
}

fn permission_denied(client: &mut Client, id: &str, payload: CommandPayload) {
    client.send(&command(id, &format!("{id}-key"), payload));
    assert!(
        matches!(
            client.next_result(),
            CommandResult::Rejected { ref code, .. } if code == "permission_denied"
        ),
        "{id} must be refused for this role"
    );
}

/// Protocol 2.24 titles end to end through the daemon: snapshots are
/// completed by `title_state`, a field save is optimistic-concurrency
/// checked, a take fades the title onto channel 2 of Program, the roles are
/// enforced, and the field values and revision survive a kill (journal
/// recovery) and a further restart (checkpointed manifest).
#[test]
#[allow(clippy::too_many_lines)]
fn title_fields_take_and_off_air_round_trip_through_the_daemon() {
    use fm_protocol::{
        MonitorFrameAssembler, MonitorImage, MonitorTap, OverlayTransitionKind,
        TitleAirRequirement, TitleSummary, WireOverlayChannelId,
    };

    let directory = TestDirectory::new("title-commands");
    let project_path = directory.project_path();
    create_title_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);

    // The previous protocol is refused; the current one is 2.28.
    assert_eq!(CURRENT_PROTOCOL_VERSION, ProtocolVersion::new(2, 28));
    let mut old = daemon.connect();
    old.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: ProtocolVersion::new(2, 27),
        build: "process-test".into(),
        client_type: ClientType::Integration,
        desired_role: Role::Viewer,
        resume_cursor: None,
    }));
    assert!(matches!(
        old.receive(),
        WireMessage::HandshakeResponse(response)
            if matches!(&response.outcome, HandshakeOutcome::Rejected { error } if error.code == "protocol_mismatch")
    ));
    drop(old);

    let mut viewer = daemon.connect();
    assert_eq!(
        viewer.handshake_as(Role::Viewer, None).protocol,
        ProtocolVersion::new(2, 28)
    );
    let (snapshot, states) = title_sync(&mut viewer);
    assert_eq!(
        snapshot.titles,
        [TitleSummary {
            input: input(4),
            field_revision: 3,
        }]
    );
    assert_title_state(&states[0], 0, 3, (230, 20, 140));

    let mut graphics = daemon.connect();
    graphics.handshake_as(Role::Graphics, None);
    title_sync(&mut graphics);
    graphics.send(&command(
        "save",
        "save-key",
        CommandPayload::SetTitleFields {
            input: input(4),
            base_revision: 3,
            require: TitleAirRequirement::OffAir,
            fields: title_color(10, 200, 30),
        },
    ));
    assert_eq!(accepted_revision(&mut graphics, "save"), 1);
    assert!(matches!(
        durable_event_at(&mut graphics, 1).payload,
        EventPayload::TitleChanged { field_revision: 4, ref fields, .. }
            if *fields == title_color(10, 200, 30)
    ));
    // A save against the old field revision is a stale conflict.
    graphics.send(&command(
        "stale",
        "stale-key",
        CommandPayload::SetTitleFields {
            input: input(4),
            base_revision: 3,
            require: TitleAirRequirement::OffAir,
            fields: title_color(1, 1, 1),
        },
    ));
    assert!(matches!(
        graphics.next_result(),
        CommandResult::Rejected { ref code, current_revision: 1, .. } if code != "permission_denied"
    ));
    let take = |fields| CommandPayload::TakeTitle {
        input: input(4),
        channel: WireOverlayChannelId::new(2).unwrap(),
        base_revision: 4,
        fields,
    };
    permission_denied(&mut graphics, "graphics-take", take(Vec::new()));
    permission_denied(
        &mut graphics,
        "graphics-off",
        CommandPayload::TitleOffAir { input: input(4) },
    );
    permission_denied(
        &mut viewer,
        "viewer-save",
        CommandPayload::SetTitleFields {
            input: input(4),
            base_revision: 4,
            require: TitleAirRequirement::OffAir,
            fields: title_color(1, 1, 1),
        },
    );
    permission_denied(&mut viewer, "viewer-take", take(Vec::new()));
    // The simulated daemon serves two peers at a time.
    drop(viewer);

    // A Studio operator receives monitor frames.
    let mut studio = daemon.connect();
    studio
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    studio.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "process-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Operator,
        resume_cursor: None,
    }));
    assert!(matches!(
        studio.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    let (_, states) = title_sync(&mut studio);
    assert_title_state(&states[0], 1, 4, (10, 200, 30));

    // An operator may not edit fields, even as part of a take.
    permission_denied(
        &mut studio,
        "operator-take-fields",
        take(title_color(1, 1, 1)),
    );

    studio.send(&command(
        "fade",
        "fade-key",
        CommandPayload::ConfigureOverlayTransition {
            channel: WireOverlayChannelId::new(2).unwrap(),
            transition: OverlayTransitionKind::Fade,
            duration_frames: 6,
        },
    ));
    assert_eq!(accepted_revision(&mut studio, "fade"), 2);
    studio.send(&command("take", "take-key", take(Vec::new())));
    assert_eq!(accepted_revision(&mut studio, "take"), 3);

    let mut assembler = MonitorFrameAssembler::new();
    let bar = (100, 50, 300, 150);
    let saved = [10, 200, 30, 255];
    let mut taken = false;
    let mut realized = false;
    let mut composited = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !(taken && realized && composited) {
        assert!(
            Instant::now() < deadline,
            "take incomplete: event={taken} realized={realized} monitor={composited}"
        );
        match studio.receive() {
            WireMessage::Event(event) if event.cursor.revision == 3 => {
                let EventPayload::TitleChanged {
                    input: changed,
                    field_revision,
                    overlays,
                    ..
                } = event.payload
                else {
                    panic!("a take publishes title_changed: {event:?}");
                };
                assert_eq!((changed, field_revision), (input(4), 4));
                assert_eq!(overlays[1].source, Some(input(4)));
                assert!(overlays[1].active);
                assert_eq!(overlays[1].transition, OverlayTransitionKind::Fade);
                taken = true;
            }
            WireMessage::RuntimeEvent(runtime) if runtime.revision == 3 => {
                if let RuntimeLifecycleEvent::Realized { domain, .. } = &runtime.event {
                    assert_eq!(domain, "switcher");
                    realized = true;
                }
            }
            WireMessage::MonitorFrame(part) if part.tap == MonitorTap::Program => {
                if let Some(image) = assembler.push(&part).unwrap() {
                    let image: MonitorImage = image;
                    composited |= shows_rectangle(&image, bar, saved, [12, 34, 56, 255]);
                }
            }
            _ => {}
        }
    }
    // On air, a save that requires the title off air is refused.
    graphics.send(&command(
        "save-on-air",
        "save-on-air-key",
        CommandPayload::SetTitleFields {
            input: input(4),
            base_revision: 4,
            require: TitleAirRequirement::OffAir,
            fields: title_color(1, 1, 1),
        },
    ));
    assert!(matches!(
        graphics.next_result(),
        CommandResult::Rejected { ref code, current_revision: 3, .. } if code != "permission_denied"
    ));
    drop(graphics);
    let on_air = snapshot_of(&daemon, Role::Viewer);
    assert_eq!(on_air.realized_overlays[1].source, Some(input(4)));
    assert_eq!(on_air.realized_overlays[1].opacity, u8::MAX);

    studio.send(&command(
        "off",
        "off-key",
        CommandPayload::TitleOffAir { input: input(4) },
    ));
    assert_eq!(accepted_revision(&mut studio, "off"), 4);
    let EventPayload::DesiredSwitcher { overlays, .. } = durable_event_at(&mut studio, 4).payload
    else {
        panic!("title_off_air publishes desired_switcher");
    };
    assert!(!overlays[1].active);
    drop(studio);

    let mut graphics = daemon.connect();
    graphics.handshake_as(Role::Graphics, None);
    title_sync(&mut graphics);
    graphics.send(&command(
        "save-again",
        "save-again-key",
        CommandPayload::SetTitleFields {
            input: input(4),
            base_revision: 4,
            require: TitleAirRequirement::OffAir,
            fields: title_color(5, 6, 7),
        },
    ));
    assert_eq!(accepted_revision(&mut graphics, "save-again"), 5);
    drop(graphics);
    daemon.stop();

    // Journal recovery restores the values and field revision, and the
    // recovered checkpoint keeps them across a further restart.
    for _ in 0..2 {
        let daemon = Daemon::start(&project_path);
        let mut client = daemon.connect();
        client.handshake_as(Role::Viewer, None);
        let (snapshot, states) = title_sync(&mut client);
        assert_eq!(snapshot.revision, 5);
        assert_eq!(
            snapshot.titles,
            [TitleSummary {
                input: input(4),
                field_revision: 5,
            }]
        );
        assert_eq!(states.len(), 1);
        assert_title_state(&states[0], 5, 5, (5, 6, 7));
        drop(client);
        daemon.wait_success();
    }
}

/// Undo and redo through the daemon need the session role to hold the class
/// of the edit they would revert or reapply, not only the project-edit
/// class: Graphics may undo a rename but not a Stinger slot change, exactly
/// as fm-control authorizes it.
#[test]
fn undo_and_redo_need_the_targeted_edit_permission_through_the_daemon() {
    let directory = TestDirectory::new("undo-targeted-permission");
    let project_path = directory.project_path();
    create_rename_project(&project_path);
    let daemon = Daemon::start_without_once(&project_path);

    let mut admin = daemon.connect();
    admin.handshake_as(Role::Admin, None);
    assert!(matches!(admin.receive(), WireMessage::Snapshot(_)));
    admin.send(&command(
        "rename",
        "rename-key",
        CommandPayload::RenameInput {
            input: input(1),
            name: "Camera Left".into(),
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "rename"), 1);
    admin.send(&command(
        "stinger",
        "stinger-key",
        CommandPayload::ConfigureStinger {
            slot: WireStingerSlotId::new(8).unwrap(),
            media_input: input(2),
            preload: false,
            cut_point_frames: 7,
            audio_policy: StingerAudioPolicy::Muted,
            missing_media_fallback: StingerMissingMediaFallback::KeepProgram,
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "stinger"), 2);

    let undo = || CommandPayload::Undo {
        expected_edit: None,
    };
    let redo = || CommandPayload::Redo {
        expected_edit: None,
    };
    // Two peers at most: the session-level matrix is covered in fm-server.
    let mut graphics = daemon.connect();
    graphics.handshake_as(Role::Graphics, None);
    // Graphics may use history but may not change Stinger slots.
    permission_denied(&mut graphics, "graphics-undo-stinger", undo());

    admin.send(&command("admin-undo", "admin-undo-key", undo()));
    assert_eq!(accepted_revision(&mut admin, "admin-undo"), 3);

    // Redo would reapply the Stinger change; the rename beneath is Graphics'.
    permission_denied(&mut graphics, "graphics-redo-stinger", redo());
    graphics.send(&command("graphics-undo", "graphics-undo-key", undo()));
    assert_eq!(accepted_revision(&mut graphics, "graphics-undo"), 4);
    drop(graphics);
    drop(admin);
    let settled = snapshot_of(&daemon, Role::Viewer);
    assert_eq!(input_name(&settled, 1), "Input 1");
    assert_eq!(settled.stingers, []);
}

#[test]
#[allow(clippy::too_many_lines)]
fn playlist_clock_takes_entries_on_their_frames_and_survives_kill_and_recovery() {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistRunState, PlaylistStart,
        PlaylistTransitionKind,
    };

    fn playlist_event(
        client: &mut Client,
        revision: u64,
    ) -> (u64, PlaylistRunState, WireInputId, WireInputId) {
        match durable_event_at(client, revision).payload {
            EventPayload::PlaylistChanged {
                frame,
                state,
                program,
                preview,
                ..
            } => (frame, state, program, preview),
            other => panic!("expected playlist_changed at {revision}, got {other:?}"),
        }
    }

    let directory = TestDirectory::new("playlist-clock");
    let project_path = directory.project_path();
    create_rename_project(&project_path);
    let store = ProjectStore::new(&project_path).unwrap();
    let daemon = Daemon::start_without_once(&project_path);

    let mut admin = daemon.connect();
    admin
        .reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    admin.handshake_as(Role::Admin, None);
    assert!(matches!(admin.receive(), WireMessage::Snapshot(_)));

    let entries = vec![
        PlaylistEntrySpec {
            id: 1,
            input: input(2),
            duration_frames: 10,
            transition: PlaylistTransitionKind::Cut,
            transition_frames: 0,
            enabled: true,
        },
        PlaylistEntrySpec {
            id: 2,
            input: input(1),
            duration_frames: 10,
            transition: PlaylistTransitionKind::Fade,
            transition_frames: 5,
            enabled: true,
        },
    ];
    admin.send(&command(
        "set-playlist",
        "set-playlist-key",
        CommandPayload::SetPlaylist {
            loop_mode: PlaylistLoopMode::Off,
            entries: entries.clone(),
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "set-playlist"), 1);
    match durable_event_at(&mut admin, 1).payload {
        EventPayload::PlaylistChanged {
            frame: 0,
            loop_mode: PlaylistLoopMode::Off,
            entries: replicated,
            state: PlaylistRunState::Stopped,
            ..
        } => assert_eq!(replicated, entries),
        other => panic!("expected the stopped playlist, got {other:?}"),
    }

    // Only the daemon's clock may advance the playlist.
    admin.send(&command(
        "client-advance",
        "client-advance-key",
        CommandPayload::PlaylistAdvance { frame: 11 },
    ));
    assert!(matches!(
        admin.next_result(),
        CommandResult::Rejected { code, current_revision: 1, .. } if code == "unavailable"
    ));

    // Started on frame 1, the first entry is due ten frames later.
    let started_at = Instant::now();
    admin.send(&command(
        "start-playlist",
        "start-playlist-key",
        CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Start {
                at: PlaylistStart::AfterFrames(10),
                from: None,
            },
        },
    ));
    assert_eq!(accepted_revision(&mut admin, "start-playlist"), 2);
    assert_eq!(
        playlist_event(&mut admin, 2),
        (
            1,
            PlaylistRunState::Armed {
                start_frame: 11,
                from: None
            },
            input(1),
            input(2)
        )
    );

    // The clock takes each entry to Program on exactly its frame, paced by
    // wall time (25 fps), and finishes after the last one.
    let (frame, state, program, preview) = playlist_event(&mut admin, 3);
    assert!(
        started_at.elapsed() >= Duration::from_millis(300),
        "the first entry aired after {:?}",
        started_at.elapsed()
    );
    assert_eq!((frame, program, preview), (11, input(2), input(1)));
    assert!(matches!(
        state,
        PlaylistRunState::Playing {
            entry: 1,
            started_frame: 11,
            ..
        }
    ));
    let (frame, state, program, preview) = playlist_event(&mut admin, 4);
    assert_eq!((frame, program, preview), (21, input(1), input(2)));
    assert!(matches!(
        state,
        PlaylistRunState::Playing {
            entry: 2,
            started_frame: 21,
            ..
        }
    ));
    assert_eq!(
        playlist_event(&mut admin, 5),
        (
            31,
            PlaylistRunState::Finished { last: Some(2) },
            input(1),
            input(2)
        )
    );
    // A client joining late learns the whole playlist from the
    // `playlist_state` that completes its snapshot (Protocol 2.26).
    let (finished, playlist) = playlist_sync(&daemon);
    assert_eq!(finished.revision, 5);
    assert_eq!(finished.realized_program, input(1));
    assert!(finished.playlist_follows);
    assert_eq!(
        finished.frame_rate,
        fm_types::FrameRate::new(25, 1).unwrap()
    );
    assert_eq!(
        (
            playlist.revision,
            playlist.loop_mode,
            &playlist.entries,
            playlist.state
        ),
        (
            5,
            PlaylistLoopMode::Off,
            &entries,
            PlaylistRunState::Finished { last: Some(2) }
        )
    );
    assert!(playlist.frame >= 31, "{}", playlist.frame);
    drop(admin);
    daemon.stop();

    // The clock's advances are journaled like any command, so replay
    // reproduces the exact frames and the finished playlist.
    assert_eq!(store.scan_journal().unwrap().batches().len(), 5);
    let daemon = Daemon::start(&project_path);
    let (recovered, playlist) = playlist_sync(&daemon);
    assert_eq!(recovered.revision, 5);
    assert_eq!(recovered.desired_program, input(1));
    assert_eq!(playlist.entries, entries);
    assert_eq!(playlist.state, PlaylistRunState::Finished { last: Some(2) });
    daemon.wait_success();

    let checkpointed = store.load().unwrap();
    assert_eq!(checkpointed.position().revision, 5);
    let playlist = checkpointed.project().playlist();
    assert_eq!(playlist.loop_mode, fm_model::PlaylistLoop::Off);
    assert_eq!(
        playlist
            .entries
            .iter()
            .map(|entry| (entry.id, entry.input, entry.duration_frames))
            .collect::<Vec<_>>(),
        vec![(1, domain_input(2), 10), (2, domain_input(1), 10)]
    );
    assert_eq!(
        playlist.state,
        fm_model::PlaylistRunState::Finished { last: Some(2) }
    );
}
