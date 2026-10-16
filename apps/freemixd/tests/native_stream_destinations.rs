//! OR-006 on the native daemon, end to end through a real process: five
//! simultaneous stream destinations with independent control.
//!
//! - Three RTMP destinations are authored in the project; a fourth RTMP and
//!   a local HLS destination are added live with `stream_target_set`.
//! - Program recording starts first, then one `stream_start_all` starts all
//!   five. Four in-test `FFmpeg` RTMP listeners (`-listen 1`) receive while
//!   the HLS directory fills.
//! - One listener is killed: that destination reconnects within its authored
//!   budget and then gives up (`Failed`), while the other four keep
//!   delivering and the recording keeps running. A `stream_retry` after a new
//!   listener appears brings it back.
//! - `stream_stop_all` stops every destination and leaves the recording
//!   running. A stopped destination is then edited live and restarted, and
//!   another is removed.
//! - No stream key ever appears on the wire, in a snapshot, or on stderr.
//!
//! The test needs a native GPU adapter and `FFmpeg`/`ffprobe`. Without an
//! adapter it is skipped unless `FM_REQUIRE_NATIVE_MEDIA=1`; without the
//! tools unless `FM_REQUIRE_FFMPEG=1`.

#![cfg(all(feature = "native-media", unix))]

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    AudioBus, Input, InputKind, Layer, LayerGeometry, MainMix, Output, Project, ProjectSettings,
    Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    StartupPolicy, StreamEndpoint, StreamKey, StreamProtocol, StreamReconnectPolicy, StreamTarget,
    StreamTargetId,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, HeartbeatMessage, Role, SecretEdit, ServerIdentity, SnapshotMessage,
    StreamRealizedState, StreamReconnect, StreamStatusSample, StreamTargetConfig,
    StreamTargetProtocol, WireMessage, WireOutputId, WireRecorderSlot, WireStreamTargetId,
    decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Well inside the daemon's session timeout.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(3);
const OUTPUT: u128 = 6_012;
/// The destination whose ingest dies.
const VICTIM: u128 = 2;
/// The HLS destination added live.
const HLS: u128 = 5;

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

fn wire_target(value: u128) -> WireStreamTargetId {
    WireStreamTargetId::new(id(value))
}

/// Destination `target`'s stream key: never echoed anywhere.
fn key(target: u128) -> String {
    format!("or006-secret-key-{target}")
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
        eprintln!("native stream destination test skipped: ffmpeg/ffprobe unavailable");
    }
    available
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The healthy destinations' reconnect policy: patient enough to ride out a
/// listener that is still starting.
fn patient() -> StreamReconnectPolicy {
    StreamReconnectPolicy::new(200, 1_000, Some(30)).unwrap()
}

/// The victim's policy: three quick retries, then give up.
fn impatient() -> StreamReconnectPolicy {
    StreamReconnectPolicy::new(100, 200, Some(3)).unwrap()
}

/// A 64x48 25 fps generator show with three stopped RTMP destinations on
/// `ports`.
#[allow(clippy::too_many_lines)]
fn save_project(path: &Path, ports: &[u16]) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(6_000)),
        "Native stream destinations test",
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
    for (input_id, video) in [
        (input(1), SimulatedVideo::Bars),
        (
            input(2),
            SimulatedVideo::Solid(SolidColor::new(24, 80, 160, 255)),
        ),
    ] {
        project.add_input(Input {
            id: input_id,
            name: format!("Generator {input_id}"),
            kind: InputKind::Simulated(SimulatedInput::new(video, SimulatedAudio::Silence)),
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(input(1), input(2)));
    let scene = SceneId::new(id(6_010));
    project.add_scene(Scene {
        id: scene,
        name: "Program scene".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            id: fm_model::LayerId::new(1).unwrap(),
            name: "Source".into(),
            source: SourceRef::Input(input(1)),
            enabled: true,
            geometry: LayerGeometry::new(0, 0, 64, 48, Rotation::Deg0),
            crop: None,
            mask: None,
            key: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
        animation: fm_model::SceneAnimation::default(),
    });
    let bus = BusId::new(id(6_011));
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    let output = OutputId::new(id(OUTPUT));
    project.add_output(Output {
        id: output,
        name: "Program output".into(),
        video_source: scene,
        audio_source: bus,
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: Vec::new(),
    });
    for (index, port) in ports.iter().enumerate() {
        let target = u128::try_from(index + 1).unwrap();
        let destination = StreamTarget::new(
            StreamTargetId::new(id(target)),
            format!("Destination {target}"),
            StreamProtocol::Rtmp,
            StreamEndpoint::parse(&format!("127.0.0.1:{port}/live")).unwrap(),
            Some(StreamKey::parse(&key(target)).unwrap()),
            output,
        )
        .unwrap()
        .with_reconnect(if target == VICTIM {
            impatient()
        } else {
            patient()
        })
        .set_running(false);
        project.add_stream_target_checked(destination).unwrap();
    }
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

/// One in-test RTMP ingest: an `FFmpeg` listener that accepts one publisher
/// and writes what it receives to an FLV file, flushing every packet so the
/// file grows while the stream runs.
struct Sink {
    child: Child,
    output: PathBuf,
}

impl Sink {
    fn spawn(directory: &Path, port: u16, target: u128, generation: u32) -> Self {
        let output = directory.join(format!("ingest-{target}-{generation}.flv"));
        let child = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-listen",
                "1",
            ])
            .arg("-i")
            .arg(format!("rtmp://127.0.0.1:{port}/live/{}", key(target)))
            .args(["-c", "copy", "-flush_packets", "1", "-f", "flv", "-y"])
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self { child, output }
    }

    fn bytes(&self) -> u64 {
        fs::metadata(&self.output).map_or(0, |metadata| metadata.len())
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.kill();
    }
}

struct DaemonProcess {
    child: Child,
    address: SocketAddr,
    stderr: mpsc::Receiver<String>,
}

impl DaemonProcess {
    /// Starts `freemixd serve --native-media --record-program <output>`, or
    /// returns `None` when this host has no native media runtime (unless
    /// `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path, output: &Path) -> Option<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .arg("--native-media")
            .arg("--record-program")
            .arg(output)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = drain(child.stderr.take().unwrap());
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (ready_sender, ready) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            let _ = stdout.read_line(&mut line);
            let _ = ready_sender.send(line);
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let line = ready.recv_timeout(PROCESS_TIMEOUT).unwrap_or_default();
        if let Ok(readiness) = line.trim_end().parse::<ReadinessRecord>() {
            return Some(Self {
                child,
                address: readiness.address,
                stderr,
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
        eprintln!("native stream destination test skipped: {message}");
        None
    }

    /// Cooperative stop; returns everything written to stderr.
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
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let stderr: String = self.stderr.try_iter().collect();
                panic!("daemon did not stop: {stderr}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let failing = thread::panicking();
        if failing {
            thread::sleep(Duration::from_millis(500));
            eprintln!("daemon exit: {:?}", self.child.try_wait());
        }
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

/// An admin Studio session that heartbeats while it waits, keeps the latest
/// stream status sample per destination, and checks every line it reads for
/// a stream key.
struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    server: ServerIdentity,
    heartbeats: u64,
    last_heartbeat: Instant,
    snapshot: Option<SnapshotMessage>,
    samples: BTreeMap<u128, StreamStatusSample>,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        let reader = BufReader::new(stream);
        writer
            .write_all(
                encode_line(&WireMessage::HandshakeRequest(HandshakeRequest {
                    protocol: CURRENT_PROTOCOL_VERSION,
                    build: "native-stream-destinations-test".into(),
                    client_type: ClientType::Studio,
                    desired_role: Role::Admin,
                    resume_cursor: None,
                }))
                .unwrap()
                .as_bytes(),
            )
            .unwrap();
        let mut client = Self {
            reader,
            writer,
            server: ServerIdentity {
                engine_id: String::new(),
                project_id: String::new(),
                state_epoch: 0,
                log_id: String::new(),
            },
            heartbeats: 0,
            last_heartbeat: Instant::now(),
            snapshot: None,
            samples: BTreeMap::new(),
        };
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "no handshake and snapshot");
            match client.read() {
                Some(WireMessage::HandshakeResponse(response)) => client.server = response.server,
                Some(WireMessage::Snapshot(snapshot)) => {
                    client.snapshot = Some(snapshot);
                    break;
                }
                _ => {}
            }
        }
        client
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    fn heartbeat_if_due(&mut self) {
        if self.server.engine_id.is_empty() || self.last_heartbeat.elapsed() < HEARTBEAT_EVERY {
            return;
        }
        self.heartbeats += 1;
        let heartbeat = WireMessage::Heartbeat(HeartbeatMessage {
            server: self.server.clone(),
            sequence: self.heartbeats,
            sent_at_ms: self.heartbeats,
            last_applied: None,
        });
        self.send(&heartbeat);
        self.last_heartbeat = Instant::now();
    }

    /// The next message, or `None` when none arrived within the read timeout.
    fn read(&mut self) -> Option<WireMessage> {
        self.heartbeat_if_due();
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => panic!("daemon closed the session"),
            Ok(_) => {
                for target in 1..=5 {
                    assert!(
                        !line.contains(&key(target)),
                        "a stream key reached the wire: {line}"
                    );
                }
                let message = decode_line(&line).unwrap();
                if let WireMessage::StreamStatus(status) = &message {
                    for sample in &status.samples {
                        self.samples
                            .insert(sample.target.get().get(), sample.clone());
                    }
                }
                Some(message)
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                assert!(line.is_empty(), "a partial line timed out: {line:?}");
                None
            }
            Err(error) => panic!("{error}"),
        }
    }

    /// Sends `payload` as command `id` and returns its result.
    fn submit(&mut self, id: &str, payload: CommandPayload) -> CommandResult {
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.into(),
            idempotency_key: format!("{id}-key"),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(WireMessage::CommandResult(result)) = self.read() {
                let matches = match &result {
                    CommandResult::Accepted { id: result_id, .. }
                    | CommandResult::Rejected { id: result_id, .. } => result_id == id,
                };
                if matches {
                    return result;
                }
            }
        }
        panic!("no result for {id}");
    }

    fn command(&mut self, id: &str, payload: CommandPayload) {
        let result = self.submit(id, payload);
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "expected {id} to be accepted, got {result:?}"
        );
    }

    fn idle(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.read();
        }
    }

    /// Serves the session until `done` holds, failing after `EVENT_TIMEOUT`.
    fn wait_for(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; samples={:?}",
                self.samples
            );
            self.read();
        }
    }

    fn state(&self, target: u128) -> Option<StreamRealizedState> {
        self.samples.get(&target).map(|sample| sample.realized)
    }

    fn muxed(&self, target: u128) -> u64 {
        self.samples
            .get(&target)
            .map_or(0, |sample| sample.muxed_bytes)
    }
}

fn rtmp_config(target: u128, port: u16, reconnect: StreamReconnectPolicy) -> StreamTargetConfig {
    StreamTargetConfig {
        name: format!("Destination {target}"),
        protocol: StreamTargetProtocol::Rtmp,
        endpoint: format!("127.0.0.1:{port}/live"),
        backup_endpoint: None,
        output: WireOutputId::new(id(OUTPUT)),
        reconcile_on_startup: false,
        video_bitrate_kbps: 2_500,
        reconnect: StreamReconnect {
            initial_delay_ms: reconnect.initial_delay_ms(),
            max_delay_ms: reconnect.max_delay_ms(),
            max_attempts: reconnect.max_attempts(),
        },
    }
}

fn mp4_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "mp4"))
        .collect();
    files.sort();
    files
}

fn directory_bytes(directory: &Path) -> u64 {
    fs::read_dir(directory).map_or(0, |entries| {
        entries
            .flatten()
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum()
    })
}

/// Video frames in a recording, after proving it decodes.
fn video_frames(path: &Path) -> u64 {
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
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|error| panic!("{}: no frame count: {error}", path.display()))
}

const RTMP: [u128; 4] = [1, 2, 3, 4];
const ALL: [u128; 5] = [1, 2, 3, 4, 5];

#[test]
#[allow(clippy::too_many_lines)]
fn five_destinations_run_together_fail_alone_and_never_touch_the_recording() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("destinations.freemix");
    let recordings = directory.path().join("recordings");
    let ingest = directory.path().join("ingest");
    let hls = directory.path().join("hls");
    fs::create_dir_all(&recordings).unwrap();
    fs::create_dir_all(&ingest).unwrap();
    let ports: Vec<u16> = RTMP.iter().map(|_| free_port()).collect();
    save_project(&project, &ports[..3]);
    let Some(daemon) = DaemonProcess::start(&project, &recordings.join("program.mp4")) else {
        return;
    };
    let mut client = Client::connect(daemon.address);
    let authored = &client.snapshot.as_ref().unwrap().stream_targets;
    assert_eq!(authored.len(), 3);
    assert!(authored.iter().all(|entry| entry.has_key));

    // Two more destinations join live: a fourth RTMP ingest and local HLS.
    client.command(
        "add-4",
        CommandPayload::SetStreamTarget {
            target: wire_target(4),
            config: Box::new(rtmp_config(4, ports[3], patient())),
            stream_key: SecretEdit::Set(key(4)),
        },
    );
    client.command(
        "add-hls",
        CommandPayload::SetStreamTarget {
            target: wire_target(HLS),
            config: Box::new(StreamTargetConfig {
                name: "Local HLS".into(),
                protocol: StreamTargetProtocol::Hls,
                endpoint: hls.display().to_string(),
                backup_endpoint: None,
                output: WireOutputId::new(id(OUTPUT)),
                reconcile_on_startup: false,
                video_bitrate_kbps: 2_500,
                reconnect: StreamReconnect::DEFAULT,
            }),
            stream_key: SecretEdit::Keep,
        },
    );
    let sixth = client.submit(
        "add-6",
        CommandPayload::SetStreamTarget {
            target: wire_target(6),
            config: Box::new(rtmp_config(6, ports[3], patient())),
            stream_key: SecretEdit::Set(key(4)),
        },
    );
    assert!(
        matches!(&sixth, CommandResult::Rejected { code, .. } if code == "invalid_command"),
        "{sixth:?}"
    );

    // Recording first, then every destination with one group start.
    let recording_started = Instant::now();
    client.command(
        "record",
        CommandPayload::RecordStart {
            recorder: WireRecorderSlot::new(1).unwrap(),
        },
    );
    let mut sinks: BTreeMap<u128, Sink> = RTMP
        .iter()
        .zip(&ports)
        .map(|(target, port)| (*target, Sink::spawn(&ingest, *port, *target, 1)))
        .collect();
    client.idle(Duration::from_millis(500));
    client.command("start-all", CommandPayload::StreamStartAll);
    client.wait_for("every destination live and delivering", |client| {
        ALL.iter().all(|target| {
            client.state(*target) == Some(StreamRealizedState::Live) && client.muxed(*target) > 0
        }) && sinks.values().all(|sink| sink.bytes() > 0)
            && directory_bytes(&hls) > 0
    });
    assert_eq!(mp4_files(&recordings).len(), 1, "one segment records");
    let recording = mp4_files(&recordings).remove(0);

    // The victim's ingest dies: it retries within its budget, then gives up.
    let before_kill: BTreeMap<u128, (u64, u64)> = RTMP
        .iter()
        .map(|target| (*target, (sinks[target].bytes(), client.muxed(*target))))
        .collect();
    let hls_before = client.muxed(HLS);
    sinks.get_mut(&VICTIM).unwrap().kill();
    client.wait_for("the victim to give up", |client| {
        client.state(VICTIM) == Some(StreamRealizedState::Failed)
    });
    client.idle(Duration::from_secs(2));
    for target in RTMP.iter().filter(|target| **target != VICTIM) {
        let (sink_bytes, muxed) = before_kill[target];
        assert!(
            sinks[target].bytes() > sink_bytes,
            "destination {target}'s ingest stopped receiving when another died"
        );
        assert!(
            client.muxed(*target) > muxed,
            "destination {target} stalled"
        );
        assert_eq!(client.state(*target), Some(StreamRealizedState::Live));
    }
    assert!(
        client.muxed(HLS) > hls_before,
        "the HLS destination stalled"
    );
    let victim = &client.samples[&VICTIM];
    assert!(victim.failure.is_some(), "{victim:?}");
    assert!(
        !victim.failure.as_deref().unwrap().contains(&key(VICTIM)),
        "{victim:?}"
    );

    // A new ingest appears; the retry reconnects with a fresh budget.
    sinks.insert(VICTIM, Sink::spawn(&ingest, ports[1], VICTIM, 2));
    client.idle(Duration::from_millis(500));
    client.command(
        "retry",
        CommandPayload::StreamRetry {
            target: wire_target(VICTIM),
        },
    );
    client.wait_for("the victim to deliver again", |client| {
        client.state(VICTIM) == Some(StreamRealizedState::Live) && sinks[&VICTIM].bytes() > 0
    });

    // The group stop reaches every destination and no recorder.
    let recording_bytes = fs::metadata(&recording).unwrap().len();
    client.command("stop-all", CommandPayload::StreamStopAll);
    client.wait_for("every destination stopped", |client| {
        ALL.iter()
            .all(|target| client.state(*target) == Some(StreamRealizedState::Stopped))
    });
    client.idle(Duration::from_secs(3));
    assert!(
        fs::metadata(&recording).unwrap().len() > recording_bytes,
        "the recording stopped growing with the streams"
    );

    // A stopped destination is edited live (its stored key kept) and
    // restarted on a new ingest; another is removed.
    let new_port = free_port();
    client.command(
        "edit-3",
        CommandPayload::SetStreamTarget {
            target: wire_target(3),
            config: Box::new(StreamTargetConfig {
                name: "Destination 3 moved".into(),
                ..rtmp_config(3, new_port, patient())
            }),
            stream_key: SecretEdit::Keep,
        },
    );
    client.command(
        "remove-hls",
        CommandPayload::RemoveStreamTarget {
            target: wire_target(HLS),
        },
    );
    let moved = Sink::spawn(&ingest, new_port, 3, 2);
    client.idle(Duration::from_millis(500));
    client.command(
        "start-3",
        CommandPayload::StreamStart {
            target: wire_target(3),
        },
    );
    client.wait_for("the edited destination to deliver", |client| {
        client.state(3) == Some(StreamRealizedState::Live) && moved.bytes() > 0
    });
    let running = client.submit(
        "edit-running",
        CommandPayload::RemoveStreamTarget {
            target: wire_target(3),
        },
    );
    assert!(
        matches!(&running, CommandResult::Rejected { .. }),
        "{running:?}"
    );

    // A fresh session sees the configuration, never a key. The daemon serves
    // one session at a time, so the operator session leaves first.
    drop(client);
    let viewer = Client::connect(daemon.address);
    let entries = &viewer.snapshot.as_ref().unwrap().stream_targets;
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                entry.target.get().get(),
                entry.config.name.as_str(),
                entry.has_key
            ))
            .collect::<Vec<_>>(),
        [
            (1, "Destination 1", true),
            (2, "Destination 2", true),
            (3, "Destination 3 moved", true),
            (4, "Destination 4", true),
        ]
    );
    assert_eq!(entries[1].config.reconnect.max_attempts, Some(3));
    drop(viewer);

    let mut client = Client::connect(daemon.address);
    client.command(
        "stop-3",
        CommandPayload::StreamStop {
            target: wire_target(3),
        },
    );
    client.command(
        "record-stop",
        CommandPayload::RecordStop {
            recorder: WireRecorderSlot::new(1).unwrap(),
        },
    );
    let recorded_for = recording_started.elapsed();
    drop(client);
    let stderr = daemon.stop();
    drop(sinks);
    drop(moved);

    for target in ALL {
        assert!(
            !stderr.contains(&key(target)),
            "stream key {target} leaked: {stderr}"
        );
    }
    let reports: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("FREEMIXD_RECORDER\t"))
        .collect();
    assert_eq!(reports.len(), 1, "one uninterrupted segment: {stderr}");
    assert!(reports[0].contains("\toutcome=Clean\t"), "{}", reports[0]);
    assert_eq!(mp4_files(&recordings), std::slice::from_ref(&recording));
    // The recording spans the whole run: no stream event paused it.
    let frames = video_frames(&recording);
    let expected = u64::try_from(recorded_for.as_millis()).unwrap() * 25 / 1_000;
    assert!(
        frames * 10 >= expected * 6,
        "{frames} frames recorded over {recorded_for:?}"
    );
}
