//! Process-level IN-015 on the simulated runtime: a network stream input is
//! added live with its configuration (`input_add`, `kind=network_stream`),
//! edited (`input_stream_set`), refused when invalid, replicated through
//! `input_added`, `network_stream_changed`, and the snapshot with its URL
//! redacted, and persisted (credentials included) across restart. The
//! simulated runtime receives nothing: every stream reports `idle`.
//!
//! IN-016/IN-017: an RTMP listener's stream key and an SRT input's
//! passphrase never reach the wire (`***` in events and the snapshot) yet
//! persist across restart, and a second input listening on a taken port is
//! refused with `listen_port_in_use`.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventPayload, HandshakeRequest, LiveInputSource, Role, SnapshotMessage, WireInputId,
    WireMessage, decode_line, encode_line,
};
use fm_types::network_stream::{
    NetworkStreamConfig, NetworkStreamSettings, NetworkStreamState, RtspTransport, SecretEdit,
    StallSlate, StreamMode, StreamUrl,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(20);
const SECRETS: [&str; 5] = [
    "hunter2",
    "n3wpass",
    "s1gnature",
    "sk_live_9zQ",
    "srt-passphrase-xyz",
];

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "freemixd-network-streams-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn project(&self) -> PathBuf {
        self.0.join("show.freemix")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn solid(id: u128) -> Input {
    Input {
        id: input(id),
        name: format!("Solid {id}"),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(12, 34, 56, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    }
}

/// Inputs 1 (Program) and 2 (Preview) are solid generators; 25 fps.
fn save_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(46).unwrap()),
        "Network streams",
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
    project.add_input(solid(1));
    project.add_input(solid(2));
    project.set_main_mix(MainMix::new(input(1), input(2)));
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

struct Daemon {
    child: Child,
    address: SocketAddr,
}

impl Daemon {
    fn start(project: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = sender.send(line);
        });
        let line = receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_default();
        let Ok(readiness) = line.parse::<ReadinessRecord>() else {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr = String::new();
            let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
            panic!("daemon did not start: {line:?} {stderr}");
        };
        Self {
            child,
            address: readiness.address,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    commands: u64,
}

impl Client {
    fn connect(daemon: &Daemon) -> (Self, SnapshotMessage) {
        let stream = TcpStream::connect(daemon.address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            commands: 0,
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "network-streams-test".into(),
            client_type: ClientType::Integration,
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
        (client, snapshot)
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    /// The next message; no line on the wire carries a URL secret.
    fn receive(&mut self) -> WireMessage {
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed"
        );
        for secret in SECRETS {
            assert!(!line.contains(secret), "a URL secret leaked: {line}");
        }
        decode_line(&line).unwrap()
    }

    /// Submits `payload` and returns its result and, for an accepted
    /// command, the events of its revision.
    fn submit(&mut self, payload: &CommandPayload) -> (CommandResult, Vec<EventPayload>) {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload: payload.clone(),
        }));
        let deadline = Instant::now() + WAIT;
        let mut events = Vec::new();
        let mut result = None;
        loop {
            assert!(Instant::now() < deadline, "timed out on {payload:?}");
            match self.receive() {
                WireMessage::CommandResult(done) => result = Some(done),
                WireMessage::Event(event) => events.push((event.cursor.revision, event.payload)),
                _ => {}
            }
            match &result {
                Some(CommandResult::Accepted { revision, .. }) => {
                    let of_revision = events
                        .iter()
                        .filter(|(at, _)| at == revision)
                        .map(|(_, event)| event.clone())
                        .collect::<Vec<_>>();
                    if !of_revision.is_empty() {
                        return (result.unwrap(), of_revision);
                    }
                }
                Some(done) => return (done.clone(), Vec::new()),
                None => {}
            }
        }
    }

    /// Submits `payload`, which must be accepted, and returns its event.
    fn accept(&mut self, payload: &CommandPayload) -> EventPayload {
        let (result, mut events) = self.submit(payload);
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{payload:?}: {result:?}"
        );
        events.pop().expect("the command's event")
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn network_stream_inputs_replicate_redacted_and_persist() {
    let directory = TestDirectory::new();
    let project = directory.project();
    save_project(&project);
    let daemon = Daemon::start(&project);
    let (mut client, snapshot) = Client::connect(&daemon);
    assert_eq!(snapshot.network_streams, []);

    // Added live with its configuration; the event redacts the URL.
    let mut config = NetworkStreamConfig::new("rtsp://admin:hunter2@cam.test:554/live");
    config.settings.transport = RtspTransport::Tcp;
    config.settings.jitter_buffer_ms = 500;
    config.settings.stall = StallSlate::Black;
    let added = client.accept(&CommandPayload::AddInput {
        input: wire(5),
        name: "Camera".into(),
        source: LiveInputSource::NetworkStream(Box::new(config.clone())),
    });
    let EventPayload::InputAdded {
        input: added_input,
        network_stream: Some(added_config),
        ..
    } = added
    else {
        panic!("expected input_added with its stream: {added:?}");
    };
    assert_eq!(added_input, wire(5));
    assert_eq!(added_config.url, "rtsp://***@cam.test:554/live");
    assert_eq!(added_config.settings, config.settings);

    // A settings edit keeps the URL; the event redacts it.
    let mut settings = config.settings;
    settings.jitter_buffer_ms = 800;
    settings.reconnect.backoff_max_ms = 30_000;
    let changed = client.accept(&CommandPayload::SetNetworkStream {
        input: wire(5),
        url: None,
        settings,
        stream_id: None,
        stream_key: SecretEdit::Keep,
        passphrase: SecretEdit::Keep,
    });
    let EventPayload::NetworkStreamChanged {
        input: changed_input,
        config: changed_config,
    } = changed
    else {
        panic!("expected network_stream_changed: {changed:?}");
    };
    assert_eq!(changed_input, wire(5));
    assert_eq!(changed_config.url, "rtsp://***@cam.test:554/live");
    assert_eq!(changed_config.settings, settings);

    // A new URL with a secret query value.
    let changed = client.accept(&CommandPayload::SetNetworkStream {
        input: wire(5),
        url: Some(StreamUrl::new(
            "rtsp://admin:n3wpass@cam.test:554/live2?signature=s1gnature",
        )),
        settings,
        stream_id: None,
        stream_key: SecretEdit::Keep,
        passphrase: SecretEdit::Keep,
    });
    let EventPayload::NetworkStreamChanged {
        config: changed, ..
    } = changed
    else {
        panic!("expected network_stream_changed: {changed:?}");
    };
    assert_eq!(changed.url, "rtsp://***@cam.test:554/live2?signature=***");

    // Invalid edits are refused and change nothing (the codec already
    // refuses an unsupported scheme or an out-of-range setting).
    let mut udp_only = settings;
    udp_only.transport = RtspTransport::Udp;
    for refused in [
        CommandPayload::SetNetworkStream {
            input: wire(5),
            url: Some(StreamUrl::new("udp://@:9000")),
            settings: udp_only,
            stream_id: None,
            stream_key: SecretEdit::Keep,
            passphrase: SecretEdit::Keep,
        },
        CommandPayload::SetNetworkStream {
            input: wire(1),
            url: None,
            settings,
            stream_id: None,
            stream_key: SecretEdit::Keep,
            passphrase: SecretEdit::Keep,
        },
        CommandPayload::SetNetworkStream {
            input: wire(9),
            url: None,
            settings,
            stream_id: None,
            stream_key: SecretEdit::Keep,
            passphrase: SecretEdit::Keep,
        },
    ] {
        let (result, events) = client.submit(&refused);
        assert!(
            matches!(result, CommandResult::Rejected { .. }),
            "{refused:?}: {result:?}"
        );
        assert!(events.is_empty(), "{events:?}");
    }

    // A fresh client's snapshot carries the redacted stream, idle.
    drop(client);
    let (client, snapshot) = Client::connect(&daemon);
    let [entry] = snapshot.network_streams.as_slice() else {
        panic!("{:?}", snapshot.network_streams);
    };
    assert_eq!(entry.input, wire(5));
    assert_eq!(
        entry.config.url,
        "rtsp://***@cam.test:554/live2?signature=***"
    );
    assert_eq!(entry.config.settings, settings);
    assert_eq!(entry.status.state, NetworkStreamState::Idle);
    assert_eq!(entry.status.last_error, None);
    drop(client);
    drop(daemon);

    // Restarted from its journal, the daemon keeps the full URL for the
    // receiver and still redacts it on the wire.
    let daemon = Daemon::start(&project);
    let (_client, snapshot) = Client::connect(&daemon);
    let [entry] = snapshot.network_streams.as_slice() else {
        panic!("{:?}", snapshot.network_streams);
    };
    assert_eq!(
        entry.config.url,
        "rtsp://***@cam.test:554/live2?signature=***"
    );
    assert_eq!(entry.config.settings.jitter_buffer_ms, 800);
}

#[test]
#[allow(clippy::too_many_lines)]
fn rtmp_and_srt_secrets_never_reach_the_wire_and_listen_ports_are_exclusive() {
    let directory = TestDirectory::new();
    let project = directory.project();
    save_project(&project);
    let daemon = Daemon::start(&project);
    let (mut client, _) = Client::connect(&daemon);

    let mut rtmp = NetworkStreamConfig::new("rtmp://0.0.0.0:19350/live");
    rtmp.settings.mode = StreamMode::Listener;
    rtmp.stream_key = Some("sk_live_9zQ".into());
    let added = client.accept(&CommandPayload::AddInput {
        input: wire(5),
        name: "OBS".into(),
        source: LiveInputSource::NetworkStream(Box::new(rtmp)),
    });
    let EventPayload::InputAdded {
        network_stream: Some(added),
        ..
    } = added
    else {
        panic!("expected input_added with its stream: {added:?}");
    };
    assert_eq!(added.stream_key.as_deref(), Some("***"));
    assert_eq!(added.settings.mode, StreamMode::Listener);

    let mut srt = NetworkStreamConfig::new("srt://0.0.0.0:19350");
    srt.settings.mode = StreamMode::Listener;
    srt.settings.latency_ms = 250;
    srt.passphrase = Some("srt-passphrase-xyz".into());
    srt.stream_id = Some("cam1".into());
    client.accept(&CommandPayload::AddInput {
        input: wire(6),
        name: "Remote".into(),
        source: LiveInputSource::NetworkStream(Box::new(srt)),
    });

    // A second RTMP listener on TCP 19350 is refused.
    let mut taken = NetworkStreamConfig::new("rtmp://127.0.0.1:19350/other");
    taken.settings.mode = StreamMode::Listener;
    let (result, events) = client.submit(&CommandPayload::AddInput {
        input: wire(7),
        name: "Taken".into(),
        source: LiveInputSource::NetworkStream(Box::new(taken)),
    });
    let CommandResult::Rejected { fields, .. } = &result else {
        panic!("a taken port is refused: {result:?}");
    };
    assert_eq!(fields[0].code, "listen_port_in_use", "{result:?}");
    assert_eq!(events, []);

    // Keeping the key across a settings edit; the event masks it.
    let changed = client.accept(&CommandPayload::SetNetworkStream {
        input: wire(5),
        url: None,
        settings: NetworkStreamSettings {
            mode: StreamMode::Listener,
            jitter_buffer_ms: 600,
            ..NetworkStreamSettings::default()
        },
        stream_id: None,
        stream_key: SecretEdit::Keep,
        passphrase: SecretEdit::Keep,
    });
    let EventPayload::NetworkStreamChanged { config, .. } = changed else {
        panic!("expected network_stream_changed: {changed:?}");
    };
    assert_eq!(config.stream_key.as_deref(), Some("***"));
    drop(client);
    drop(daemon);

    // Restarted, both secrets persisted (set, masked) and never shown.
    let daemon = Daemon::start(&project);
    let (_client, snapshot) = Client::connect(&daemon);
    let [rtmp, srt] = snapshot.network_streams.as_slice() else {
        panic!("{:?}", snapshot.network_streams);
    };
    assert_eq!(rtmp.config.stream_key.as_deref(), Some("***"));
    assert_eq!(rtmp.config.settings.jitter_buffer_ms, 600);
    assert_eq!(srt.config.passphrase.as_deref(), Some("***"));
    assert_eq!(srt.config.stream_id.as_deref(), Some("cam1"));
    assert_eq!(srt.config.settings.latency_ms, 250);
    assert_eq!(srt.status.state, NetworkStreamState::Idle);
}
