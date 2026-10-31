//! Program frame pacing of a native daemon that records and streams while
//! sessions cut: how late each Program frame starts against its pacer
//! deadline, read from the daemon's opt-in `FREEMIXD_PACING_TRACE` record.
//!
//! Starting the recorder and the stream encoders must not make the first
//! Program frames late: their workers start off the frame loop, and the
//! pacer's first deadline is the first frame it realizes.
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
    Input, InputKind, Layer, LayerGeometry, MainMix, Output, Project, ProjectSettings,
    RecorderSlot, Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo,
    SolidColor, SourceRef, StartupPolicy, StreamEndpoint, StreamProtocol, StreamTarget,
    StreamTargetId,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeOutcome, HandshakeRequest, HeartbeatMessage, Role, ServerIdentity, SnapshotMessage,
    WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const RESULT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_POLL: Duration = Duration::from_millis(50);
/// Well inside the daemon's 15 s session heartbeat timeout.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(2);
const FRAME_RATE: u32 = 25;
const OUTPUT: u128 = 7_200;

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
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
        eprintln!("native pacing test skipped: ffmpeg/ffprobe unavailable");
    }
    available
}

/// A 64x48 25 fps generator show that records recorder 1 and streams two
/// local HLS destinations (`hls`) from startup.
#[allow(clippy::too_many_lines)]
fn save_project(path: &Path, hls: &[PathBuf]) {
    let rate = FrameRate::new(FRAME_RATE, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(7_100)),
        "Native pacing test",
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
    let scene = SceneId::new(id(7_110));
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
    let bus = BusId::new(id(7_111));
    project.add_audio_bus(fm_model::AudioBus {
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
    for (index, directory) in hls.iter().enumerate() {
        let target = u128::try_from(index + 1).unwrap();
        project
            .add_stream_target_checked(
                StreamTarget::new(
                    StreamTargetId::new(id(target)),
                    format!("HLS {target}"),
                    StreamProtocol::Hls,
                    StreamEndpoint::parse_for(
                        StreamProtocol::Hls,
                        &directory.display().to_string(),
                    )
                    .unwrap(),
                    None,
                    output,
                )
                .unwrap()
                .with_startup(StartupPolicy::ReconcileDesiredState)
                .set_running(true),
            )
            .unwrap();
    }
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
            "freemixd-native-pacing-{}-{name}",
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

/// The show of [`save_project`] in a fresh directory: the project, the
/// recording path, and the two HLS directories.
fn show(directory: &TestDirectory) -> (PathBuf, PathBuf) {
    let project = directory.0.join("show.freemix");
    let recording = directory.0.join("program.mp4");
    let hls = ["hls-1", "hls-2"].map(|name| directory.0.join(name));
    for path in &hls {
        fs::create_dir_all(path).unwrap();
    }
    save_project(&project, &hls);
    (project, recording)
}

struct DaemonProcess {
    child: Child,
    control: SocketAddr,
    stderr: mpsc::Receiver<String>,
}

impl DaemonProcess {
    /// Starts `freemixd serve --native-media [--record-program <output>]`
    /// with the pacing trace on, or returns `None` when this host has no
    /// native media runtime (unless `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path, output: Option<&Path>) -> Option<Self> {
        Self::start_with(project, output, &[])
    }

    /// [`Self::start`] with extra `environment` variables.
    fn start_with(
        project: &Path,
        output: Option<&Path>,
        environment: &[(&str, &str)],
    ) -> Option<Self> {
        // A measurement may compare another build of the daemon.
        let binary = std::env::var_os("FREEMIXD_PACING_BINARY").map_or_else(
            || PathBuf::from(env!("CARGO_BIN_EXE_freemixd")),
            PathBuf::from,
        );
        let mut command = Command::new(binary);
        command.arg("serve").arg(project).arg("--native-media");
        if let Some(output) = output {
            command.arg("--record-program").arg(output);
        }
        command.envs(environment.iter().copied());
        let mut child = command
            .env("FREEMIXD_PACING_TRACE", "1")
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
                control: readiness.address,
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
        eprintln!("native pacing test skipped: {message}");
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

impl DaemonProcess {
    /// Kills the daemon with SIGKILL, as a crash or power loss would stop
    /// it, and returns what it wrote to stderr that was not read yet.
    fn crash(mut self) -> String {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        let mut rest = String::new();
        while let Ok(chunk) = self.stderr.recv_timeout(Duration::from_secs(2)) {
            rest.push_str(&chunk);
        }
        rest
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

/// One raw operator session that cuts and heartbeats.
struct Session {
    name: String,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    /// The start of a line whose rest has not arrived yet: on a loaded host
    /// a line can arrive in pieces further apart than a read poll.
    pending: Vec<u8>,
    server: Option<ServerIdentity>,
    heartbeats: u64,
    last_heartbeat: Instant,
}

/// The daemon closed the session (it stopped or was killed).
#[derive(Debug)]
struct Closed;

impl Session {
    /// An operator session and the snapshot it was served.
    fn connect(name: &str, address: SocketAddr) -> (Self, SnapshotMessage) {
        Self::try_connect(name, address)
            .unwrap_or_else(|Closed| panic!("{name}: the daemon closed the session"))
    }

    /// [`Self::connect`], or `Closed` when the daemon stopped (or was
    /// killed) before it served the snapshot.
    fn try_connect(name: &str, address: SocketAddr) -> Result<(Self, SnapshotMessage), Closed> {
        let stream = TcpStream::connect(address).map_err(|_| Closed)?;
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(READ_POLL)).unwrap();
        let writer = stream.try_clone().unwrap();
        let mut session = Self {
            name: name.to_owned(),
            reader: BufReader::new(stream),
            writer,
            pending: Vec::new(),
            server: None,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
        };
        session.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-pacing-test".into(),
            client_type: ClientType::Cli,
            desired_role: Role::Operator,
            resume_cursor: None,
        }))?;
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "{name}: no snapshot");
            match session.read()? {
                Some(WireMessage::HandshakeResponse(response)) => {
                    assert!(
                        matches!(response.outcome, HandshakeOutcome::Snapshot { .. }),
                        "{name}: {response:?}"
                    );
                    session.server = Some(response.server);
                }
                Some(WireMessage::Snapshot(snapshot)) => return Ok((session, snapshot)),
                _ => {}
            }
        }
    }

    fn send(&mut self, message: &WireMessage) -> Result<(), Closed> {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(|_| Closed)
    }

    /// The next message, or `None` when no whole line arrived within a read
    /// poll (the start of a partial line is kept for the next read).
    fn read(&mut self) -> Result<Option<WireMessage>, Closed> {
        match self.reader.read_until(b'\n', &mut self.pending) {
            Ok(0) => Err(Closed),
            // The daemon closed the session in the middle of a line.
            Ok(_) if self.pending.last() != Some(&b'\n') => Err(Closed),
            Ok(_) => {
                let line = std::mem::take(&mut self.pending);
                let line = std::str::from_utf8(&line)
                    .unwrap_or_else(|error| panic!("{}: {error}", self.name));
                Ok(Some(decode_line(line).unwrap()))
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(_) => Err(Closed),
        }
    }

    fn heartbeat_if_due(&mut self) -> Result<(), Closed> {
        let Some(server) = self.server.clone() else {
            return Ok(());
        };
        if self.last_heartbeat.elapsed() < HEARTBEAT_EVERY {
            return Ok(());
        }
        self.heartbeats += 1;
        let heartbeat = WireMessage::Heartbeat(HeartbeatMessage {
            server,
            sequence: self.heartbeats,
            sent_at_ms: self.heartbeats,
            last_applied: None,
        });
        self.last_heartbeat = Instant::now();
        self.send(&heartbeat)
    }

    /// Cuts as command `id` and returns the revision it was accepted at.
    fn cut(&mut self, id: &str) -> Result<u64, Closed> {
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.into(),
            idempotency_key: format!("{}-{id}", self.name),
            expected_revision: None,
            deadline_ms: None,
            payload: CommandPayload::Cut,
        }))?;
        let deadline = Instant::now() + RESULT_TIMEOUT;
        while Instant::now() < deadline {
            self.heartbeat_if_due()?;
            if let Some(WireMessage::CommandResult(result)) = self.read()? {
                match &result {
                    CommandResult::Accepted {
                        id: found,
                        revision,
                        ..
                    } if found == id => return Ok(*revision),
                    CommandResult::Rejected { id: found, .. } if found == id => {
                        panic!("{}: {id} was refused: {result:?}", self.name)
                    }
                    _ => {}
                }
            }
        }
        panic!("{}: no result for {id}", self.name);
    }

    /// Reads and heartbeats for `duration`.
    fn idle(&mut self, duration: Duration) -> Result<(), Closed> {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.heartbeat_if_due()?;
            self.read()?;
        }
        Ok(())
    }
}

/// Every traced frame's lateness in milliseconds, in frame order.
fn traced_lateness(stderr: &str) -> Vec<f64> {
    traced_frames(stderr)
        .into_iter()
        .map(|(lateness, _)| lateness)
        .collect()
}

/// Every traced frame's lateness in milliseconds and whether a command was
/// realized on it, in frame order.
fn traced_frames(stderr: &str) -> Vec<(f64, bool)> {
    traced_samples(stderr)
        .into_iter()
        .map(|sample| (sample.lateness, sample.command))
        .collect()
}

/// One traced Program frame, in milliseconds.
#[derive(Clone, Copy, Debug)]
struct FrameSample {
    /// How late the frame started against its pacer deadline.
    lateness: f64,
    /// How long realizing it took.
    realize: f64,
    /// How long the frame loop's thread ran on a CPU while realizing it,
    /// where the host reports it.
    loop_cpu: Option<f64>,
    /// How long the daemon's threads, summed, waited for a CPU since the
    /// frame before it was realized (for the first frame, since the pacer
    /// started), where the host reports it.
    queued: Option<f64>,
    /// Whether a command was realized on it.
    command: bool,
}

/// Every traced frame, in frame order.
fn traced_samples(stderr: &str) -> Vec<FrameSample> {
    let record = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_PACING_TRACE\t"))
        .unwrap_or_else(|| panic!("no pacing trace: {stderr}"));
    let samples = record
        .split('\t')
        .find_map(|field| field.strip_prefix("samples="))
        .unwrap_or_else(|| panic!("no samples in {record}"));
    samples
        .split(',')
        .filter(|sample| !sample.is_empty())
        .map(|sample| {
            let mut fields = sample.split(':');
            let mut milliseconds = || match fields.next().unwrap() {
                "-" => None,
                micros => Some(micros.parse::<f64>().unwrap() / 1_000.0),
            };
            let lateness = milliseconds().unwrap();
            let realize = milliseconds().unwrap();
            let loop_cpu = milliseconds();
            let queued = milliseconds();
            FrameSample {
                lateness,
                realize,
                loop_cpu,
                queued,
                command: fields.next() == Some("c"),
            }
        })
        .collect()
}

/// The `percent` percentile of `samples` (nearest rank).
fn percentile(samples: &[f64], percent: usize) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percent * sorted.len()).div_ceil(100);
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn summary(label: &str, samples: &[f64]) -> String {
    format!(
        "{label}: frames={} p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms late>1frame={}",
        samples.len(),
        percentile(samples, 50),
        percentile(samples, 95),
        percentile(samples, 99),
        samples.iter().copied().fold(0.0, f64::max),
        samples
            .iter()
            .filter(|&&lateness| lateness > 1_000.0 / f64::from(FRAME_RATE))
            .count()
    )
}

/// Starting the recorder and two HLS stream encoders never makes the first
/// Program frames late. The recorder's `FFmpeg` used to start on the frame
/// loop after the pacer's clock had started, so the first frames were 50 to
/// 100 ms late (over a second in one release run) and were realized back to
/// back to catch up. The pacer's first deadline is now the first frame the
/// loop asks for, and the segment opens on its worker while the frames
/// realized meanwhile queue in its backlog: the first frames are on time and
/// the recording still holds every realized frame. On a host too loaded to
/// realize frames at the show's rate, frames are late however the daemon
/// starts; there the test proves that no lateness comes from startup work on
/// the frame loop or inside a frame's realization.
#[test]
fn recorder_and_stream_startup_never_make_program_frames_late() {
    /// The frames the startup stall used to make late.
    const FIRST_FRAMES: usize = 5;

    if !tools_available() {
        return;
    }
    let directory = TestDirectory::new("startup");
    let (project, recording) = show(&directory);
    let Some(daemon) = DaemonProcess::start(&project, Some(&recording)) else {
        return;
    };
    let (mut session, _) = Session::connect("cli", daemon.control);
    session.idle(Duration::from_millis(500)).unwrap();
    assert_eq!(session.cut("cut-1").unwrap(), 1);
    session.idle(Duration::from_secs(1)).unwrap();
    drop(session);
    let stderr = daemon.stop();
    let frames = traced_samples(&stderr);
    let lateness = traced_lateness(&stderr);
    let startup = &lateness[..lateness.len().min(2 * FRAME_RATE as usize)];
    eprintln!("{}", summary("startup", startup));
    let frame_ms = 1_000.0 / f64::from(FRAME_RATE);
    assert!(
        frames.len() > 2 * FIRST_FRAMES,
        "too few frames traced: {frames:?}"
    );
    let (first, steady) = frames.split_at(FIRST_FRAMES);
    // A host whose cores are all busy makes frames late and stretches their
    // realization, the startup ones most, while the encoders start
    // alongside them, by far more than the bounds below (hundreds of
    // milliseconds) without the daemon doing anything more. The daemon's
    // trace tells those frames apart where the host reports scheduling
    // statistics: its threads waited a frame interval or more for a CPU
    // since the frame before. Such a frame is not held to the wall-clock
    // bounds on its start and realization; every startup frame is held to a
    // bound on the frame loop's own CPU time, which a busy host does not
    // stretch, so work the loop does at startup is still caught however
    // loaded the host is.
    let contended = |frame: &FrameSample| frame.queued.is_some_and(|queued| queued >= frame_ms);
    // A software adapter compiles its pipelines on the first realization
    // (hundreds of milliseconds) and every frame behind it is late by that
    // overrun. That is the adapter warming up, not daemon startup work, so
    // the wall-clock realization and lateness bounds do not apply there;
    // the frame loop's own CPU time and the startup stalls still do.
    let software = ["llvmpipe", "lavapipe", "swiftshader", "software"]
        .iter()
        .any(|name| stderr.to_ascii_lowercase().contains(name));
    if software {
        eprintln!("software GPU adapter: skipping the wall-clock realization and lateness bounds");
    }
    // The pacer's clock starts at the first frame it realizes.
    assert!(
        contended(&first[0]) || first[0].lateness < frame_ms / 2.0,
        "the first Program frame started {} ms late: {first:?}",
        first[0].lateness
    );
    // A frame is late for two reasons: the loop did something else between
    // frames (its own lateness minus what the previous frame's realization
    // overran the interval by), or realizing the frames before it took
    // longer than the interval. A stall on the frame loop, the recorder's
    // and the encoders' start included, is the first; a host too loaded to
    // realize frames at the show's rate (a software adapter competing for
    // the CPU) the second, which the frames after startup show as well.
    let mut on_loop = vec![(0, first[0].lateness)];
    for index in 1..FIRST_FRAMES {
        let overrun = (first[index - 1].lateness + first[index - 1].realize - frame_ms).max(0.0);
        on_loop.push((index, first[index].lateness - overrun));
    }
    eprintln!("startup frames: {first:?}; loop stalls (frame, ms): {on_loop:?}");
    let stalled: Vec<(usize, f64)> = on_loop
        .into_iter()
        .filter(|&(_, stall)| stall >= frame_ms)
        .collect();
    assert!(
        stalled.is_empty(),
        "startup stalled the frame loop a frame or more (frame, ms): {stalled:?}; {first:?}"
    );
    // Startup work inside a frame's realization: a startup frame takes no
    // longer than a frame interval, or than four times what the frames after
    // startup take on this host. The first realization also warms the GPU up
    // (pipelines, buffers): two to three times a later frame on an idle
    // host. So the first frame alone gets twice the bound, which still
    // catches an encoder or segment start inside it (a process spawn, tens
    // to hundreds of milliseconds). A contended frame (see above) is held to
    // the same bound on the loop's CPU time instead.
    let mut realize: Vec<f64> = steady.iter().map(|frame| frame.realize).collect();
    realize.sort_by(f64::total_cmp);
    let typical = realize[realize.len() / 2];
    let bound = frame_ms.max(4.0 * typical);
    let cold_bound = 2.0 * bound;
    let slow: Vec<(usize, f64)> = first
        .iter()
        .enumerate()
        .filter(|&(index, frame)| {
            !contended(frame)
                && !(software && index == 0)
                && frame.realize >= if index == 0 { cold_bound } else { bound }
        })
        .map(|(index, frame)| (index, frame.realize))
        .collect();
    assert!(
        slow.is_empty(),
        "startup frames took {bound:.2} ms ({cold_bound:.2} ms for the first) or more to \
         realize (frame, ms) while later frames took {typical:.2} ms: {slow:?}; {first:?}"
    );
    assert_startup_loop_cpu(first, steady);
    // On a host that keeps pace with the show (the frames after startup
    // realize in under half an interval), no startup frame is a frame late
    // unless the host was contended while it was due (see above).
    let keeps_pace = percentile(&realize, 95) < frame_ms / 2.0;
    let late: Vec<(usize, f64)> = first
        .iter()
        .enumerate()
        .filter(|&(_, frame)| {
            keeps_pace && !software && !contended(frame) && frame.lateness >= frame_ms
        })
        .map(|(index, frame)| (index, frame.lateness))
        .collect();
    assert!(
        late.is_empty(),
        "startup made Program frames a frame or more late (frame, ms): {late:?}; {first:?}"
    );
    // Every realized frame reached the segment, those realized while it
    // was still opening included.
    let report = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_RECORDER\t"))
        .unwrap_or_else(|| panic!("the recorder never reported: {stderr}"));
    assert!(report.contains("\toutcome=Clean\t"), "{report}");
    assert!(
        report.contains(&format!("\taccepted_pairs={}\t", lateness.len())),
        "{} frames were realized: {report}",
        lateness.len()
    );
}

/// Holds every startup frame's frame-loop CPU time, where the trace reports
/// it, to four times that of the frames after startup (twice that for the
/// first frame, which warms the GPU up), or an eighth of a frame interval.
fn assert_startup_loop_cpu(first: &[FrameSample], steady: &[FrameSample]) {
    let frame_ms = 1_000.0 / f64::from(FRAME_RATE);
    let loop_cpu: Option<Vec<f64>> = steady.iter().map(|frame| frame.loop_cpu).collect();
    if let Some(mut loop_cpu) = loop_cpu {
        loop_cpu.sort_by(f64::total_cmp);
        let typical = loop_cpu[loop_cpu.len() / 2];
        // A floor keeps a loop that does almost nothing per frame from
        // failing on scheduler noise.
        let bound = (frame_ms / 8.0).max(4.0 * typical);
        let cold_bound = 2.0 * bound;
        let busy: Vec<(usize, f64)> = first
            .iter()
            .enumerate()
            .filter_map(|(index, frame)| Some((index, frame.loop_cpu?)))
            .filter(|&(index, cpu)| cpu >= if index == 0 { cold_bound } else { bound })
            .collect();
        assert!(
            busy.is_empty(),
            "the frame loop spent {bound:.2} ms ({cold_bound:.2} ms for the first) or more \
             on a CPU realizing startup frames (frame, ms) while later frames took \
             {typical:.2} ms: {busy:?}; {first:?}"
        );
    }
}

/// Measurement, not a gate: 25 s of a recording, two HLS streams, and about
/// 7.5 cuts a second from two sessions, reporting Program frame lateness for
/// the first two seconds and for the rest. Run with `--ignored --nocapture`.
#[test]
#[ignore = "measurement; run explicitly with --ignored --nocapture"]
fn measure_program_pacing_while_recording_streaming_and_cutting() {
    const RUN: Duration = Duration::from_secs(25);
    /// Two sessions each cutting every 267 ms: about 7.5 cuts a second.
    const CUT_EVERY: Duration = Duration::from_millis(267);

    if !tools_available() {
        return;
    }
    let directory = TestDirectory::new("measure");
    let (project, recording) = show(&directory);
    let Some(daemon) = DaemonProcess::start(&project, Some(&recording)) else {
        return;
    };
    let address = daemon.control;
    let started = Instant::now();
    let cutters: Vec<_> = (0..2)
        .map(|index| {
            thread::spawn(move || {
                let (mut session, _) = Session::connect(&format!("cutter-{index}"), address);
                // The second session cuts half a period after the first.
                session.idle(CUT_EVERY / 2 * index).unwrap();
                let mut cuts = 0_u32;
                let mut latencies = Vec::new();
                while started.elapsed() < RUN {
                    let next = Instant::now() + CUT_EVERY;
                    cuts += 1;
                    let sent = Instant::now();
                    session.cut(&format!("cut-{cuts}")).unwrap();
                    latencies.push(sent.elapsed().as_secs_f64() * 1_000.0);
                    session
                        .idle(next.saturating_duration_since(Instant::now()))
                        .unwrap();
                }
                latencies
            })
        })
        .collect();
    let mut latencies = Vec::new();
    for cutter in cutters {
        latencies.extend(cutter.join().unwrap());
    }
    let stderr = daemon.stop();
    let frames = traced_frames(&stderr);
    let lateness: Vec<f64> = frames.iter().map(|&(lateness, _)| lateness).collect();
    let split = lateness.len().min(2 * FRAME_RATE as usize);
    eprintln!("{}", summary("first 2 s", &lateness[..split]));
    eprintln!("{}", summary("after 2 s", &lateness[split..]));
    for (label, command) in [
        ("after 2 s, command frames", true),
        ("after 2 s, idle frames", false),
    ] {
        let selected: Vec<f64> = frames[split..]
            .iter()
            .filter(|&&(_, on_command)| on_command == command)
            .map(|&(lateness, _)| lateness)
            .collect();
        eprintln!("{}", summary(label, &selected));
    }
    eprintln!(
        "cut round trip: cuts={} p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms",
        latencies.len(),
        percentile(&latencies, 50),
        percentile(&latencies, 95),
        percentile(&latencies, 99),
        latencies.iter().copied().fold(0.0, f64::max),
    );
    if let Some(telemetry) = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_TELEMETRY\t"))
    {
        let gpu = telemetry
            .split('\t')
            .filter(|field| field.starts_with("gpu_adapter=") || field.starts_with("gpu_backend="))
            .collect::<Vec<_>>();
        eprintln!("{gpu:?}");
    }
}

/// The Program a show of cuts only leaves on air at `revision`: input 1 and
/// input 2 swap on every cut.
fn program_at(revision: u64) -> WireInputId {
    WireInputId::from_domain(if revision.is_multiple_of(2) {
        input(1)
    } else {
        input(2)
    })
}

/// When a crash round kills the daemon.
#[derive(Clone, Copy, Debug)]
enum Kill {
    /// This long after the first cut was acknowledged.
    After(Duration),
    /// As soon as the persistence worker reports it began this background
    /// write (`checkpoint` or `marker`), with cuts acknowledged before it.
    During(&'static str),
}

/// Waits until a cut past revision `base` was acknowledged.
fn wait_for_acknowledgement(acknowledged: &std::sync::atomic::AtomicU64, base: u64, label: &str) {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    while acknowledged.load(std::sync::atomic::Ordering::SeqCst) == base {
        assert!(
            Instant::now() < deadline,
            "{label}: no cut was acknowledged"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// One crash round over `project`: two sessions cut as fast as they are
/// answered (with idle frames between cuts when `kill` waits for a progress
/// marker) while the daemon records `segment` and streams, the daemon is
/// killed with SIGKILL as `kill` says, and it is restarted. The recovered
/// show holds every cut a client was told was accepted (a command is durable
/// before it is acknowledged, however it waited for its frame and whatever
/// background write was in flight), holds no cut that was never sent, and
/// its Program is the one its revision's cut left on air. Returns the
/// recovered revision and, for [`Kill::During`], whether the write was still
/// unfinished when the daemon died; `None` when the host has no native
/// media.
fn crash_round(
    project: &Path,
    segment: &Path,
    previous: u64,
    kill: Kill,
    label: &str,
) -> Option<(u64, bool)> {
    let daemon =
        DaemonProcess::start_with(project, Some(segment), &[("FREEMIXD_JOURNAL_TRACE", "1")])?;
    let address = daemon.control;
    let (_, snapshot) = Session::connect("observer", address);
    let base = snapshot.revision;
    assert!(base >= previous, "{base} < {previous}");
    // A progress marker records idle progress, so none is due while every
    // frame realizes a cut, which two sessions cutting as fast as they are
    // answered manage on a quiet host: for a marker round they pause three
    // frame intervals after each cut, leaving idle frames between cuts.
    let pause = match kill {
        Kill::During("marker") => Duration::from_millis(3_000 / u64::from(FRAME_RATE)),
        Kill::During(_) | Kill::After(_) => Duration::ZERO,
    };
    let acknowledged = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(base));
    let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let cutters: Vec<_> = (0..2)
        .map(|index| {
            let acknowledged = std::sync::Arc::clone(&acknowledged);
            let sent = std::sync::Arc::clone(&sent);
            let name = format!("{label}-{index}");
            thread::spawn(move || {
                // On a loaded host the kill may land before this session
                // was served its snapshot.
                let Ok((mut session, _)) = Session::try_connect(&name, address) else {
                    return;
                };
                for cut in 1_u64.. {
                    sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    match session.cut(&format!("cut-{cut}")) {
                        Ok(revision) => {
                            acknowledged.fetch_max(revision, std::sync::atomic::Ordering::SeqCst);
                        }
                        Err(Closed) => return,
                    }
                    if session.idle(pause).is_err() {
                        return;
                    }
                }
            })
        })
        .collect();
    let mut seen = String::new();
    let begun = match kill {
        Kill::After(delay) => {
            // Measured from the first acknowledgement rather than from the
            // cutters' start: on a loaded host connecting can take longer
            // than the delay, and the kill must land with cuts acknowledged.
            wait_for_acknowledgement(&acknowledged, base, label);
            thread::sleep(delay);
            None
        }
        Kill::During(write) => {
            let begin = format!("\t{write}=begin\t");
            let deadline = Instant::now() + PROCESS_TIMEOUT;
            loop {
                assert!(
                    Instant::now() < deadline,
                    "{label}: no background {write} began: {seen}"
                );
                if let Ok(chunk) = daemon.stderr.recv_timeout(READ_POLL) {
                    seen.push_str(&chunk);
                }
                // The begin line of a write that started after a cut was
                // acknowledged, so the kill lands with acknowledged cuts on
                // both sides of it. Only whole lines: stderr arrives in
                // chunks that can end in the middle of one.
                let whole = &seen[..seen.rfind('\n').map_or(0, |end| end + 1)];
                let found = whole.lines().rev().find(|line| line.contains(&begin));
                if let Some(line) = found
                    && acknowledged.load(std::sync::atomic::Ordering::SeqCst) > base
                {
                    break Some(line.replace(&begin, &format!("\t{write}=end\t")));
                }
            }
        }
    };
    let rest = daemon.crash();
    for cutter in cutters {
        cutter.join().unwrap();
    }
    seen.push_str(&rest);
    // Unfinished when the daemon died: its end line was never written.
    let unfinished = begun.is_some_and(|end| !seen.lines().any(|line| line == end));
    let acknowledged = acknowledged.load(std::sync::atomic::Ordering::SeqCst);
    let sent = sent.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        acknowledged > base,
        "{label}: no cut was acknowledged before the kill"
    );

    let daemon = DaemonProcess::start(project, None)?;
    let (_, recovered) = Session::connect("recovered", daemon.control);
    assert!(
        recovered.revision >= acknowledged,
        "{label} ({kill:?}): recovered revision {} lost acknowledged revision {acknowledged}",
        recovered.revision
    );
    assert!(
        recovered.revision <= base + sent,
        "{label}: recovered revision {} holds cuts never sent ({sent} after {base})",
        recovered.revision
    );
    assert_eq!(
        recovered.realized_program,
        program_at(recovered.revision),
        "{label}: recovered Program"
    );
    daemon.stop();
    Some((recovered.revision, unfinished))
}

/// Crash consistency of the command path while frames are paced: the daemon
/// is killed at an arbitrary moment, three times (see [`crash_round`]).
#[test]
fn acknowledged_cuts_survive_sigkill_at_any_moment() {
    const KILLS: u64 = 3;

    if !tools_available() {
        return;
    }
    let directory = TestDirectory::new("crash");
    let (project, recording) = show(&directory);
    // A different moment in each run, spread over two frame intervals.
    let seed = u64::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_micros(),
    );
    let mut recovered = 0_u64;
    for kill in 0..KILLS {
        let segment = recording.with_file_name(format!("program-{kill}.mp4"));
        let delay = Duration::from_millis(700 + (seed.wrapping_mul(kill + 1) % 80));
        let label = format!("crash-{kill}");
        let Some((revision, _)) =
            crash_round(&project, &segment, recovered, Kill::After(delay), &label)
        else {
            return;
        };
        recovered = revision;
    }
}

/// The journal folds into the manifest and writes progress markers on its
/// persistence worker while the frame loop keeps acknowledging cuts. Killing
/// the daemon while a checkpoint (manifest rewrite, journal checkpoint and
/// compaction) or a progress marker is being written must lose no
/// acknowledged cut and recover the Program its revision left on air. Each
/// write is killed twice; the kill lands as the worker reports the write
/// began, which is almost always before it finished (reported per kill).
#[test]
fn acknowledged_cuts_survive_sigkill_during_a_checkpoint_or_progress_marker() {
    if !tools_available() {
        return;
    }
    let directory = TestDirectory::new("crash-background");
    let (project, recording) = show(&directory);
    let mut recovered = 0_u64;
    for (round, write) in ["checkpoint", "marker", "checkpoint", "marker"]
        .into_iter()
        .enumerate()
    {
        let segment = recording.with_file_name(format!("program-{round}.mp4"));
        let label = format!("{write}-{round}");
        let Some((revision, unfinished)) =
            crash_round(&project, &segment, recovered, Kill::During(write), &label)
        else {
            return;
        };
        eprintln!(
            "{label}: killed {} the {write} finished; recovered revision {revision}",
            if unfinished { "before" } else { "after" }
        );
        recovered = revision;
    }
}
