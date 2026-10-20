//! The native daemon's Preview monitor tap (SW-001, SW-002).
//!
//! A Studio peer on a native-media daemon receives Preview `monitor_frame`
//! groups next to Program: they carry the Preview source's own pixels,
//! follow a Preview change, and during a transition show the incoming
//! source, as most switchers' Preview does. The tap's readbacks never wait
//! for the GPU, so Program frames stay on time; and with no Studio peer
//! subscribed no Preview (or Program) monitor frame is composed or read back
//! at all.
//!
//! The tests need a native GPU adapter and `FFmpeg`/`ffprobe`. Without an
//! adapter they are skipped unless `FM_REQUIRE_NATIVE_MEDIA=1`; without the
//! tools unless `FM_REQUIRE_FFMPEG=1`.

#![cfg(all(feature = "native-media", unix))]

use std::{
    io::{BufRead, BufReader, ErrorKind, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    AutoTransitionKind, CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload,
    CommandResult, HandshakeOutcome, HandshakeRequest, HeartbeatMessage, MonitorFrameAssembler,
    MonitorFrameOrigin, MonitorImage, MonitorTap, Role, ServerIdentity, WireInputId, WireMessage,
    decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const FRAME_TIMEOUT: Duration = Duration::from_secs(15);
const READ_POLL: Duration = Duration::from_millis(50);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(2);
const FRAME_RATE: u32 = 25;
const RED: [u8; 3] = [220, 30, 30];
const BLUE: [u8; 3] = [24, 80, 160];
const GREEN: [u8; 3] = [30, 200, 60];
/// Frames of the timed transition the test takes.
const TRANSITION_FRAMES: u32 = 50;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
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
        eprintln!("native Preview tap test skipped: ffmpeg/ffprobe unavailable");
    }
    available
}

/// A 64x48 25 fps show of three solid generators: red on Program, blue on
/// Preview, and green.
fn save_project(path: &Path) {
    let rate = FrameRate::new(FRAME_RATE, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(7_041).unwrap()),
        "Native Preview tap test",
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
    for (id, [r, g, b]) in [(1, RED), (2, BLUE), (3, GREEN)] {
        project.add_input(Input {
            id: input(id),
            name: format!("Solid {id}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(r, g, b, 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
    }
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

fn drain(reader: impl std::io::Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if sender.send(format!("{line}\n")).is_err() {
                break;
            }
        }
    });
    receiver
}

struct DaemonProcess {
    child: Child,
    control: SocketAddr,
    stderr: mpsc::Receiver<String>,
}

impl DaemonProcess {
    /// Starts `freemixd serve --native-media`, or returns `None` when this
    /// host has no native media runtime (unless `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path) -> Option<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .arg("--native-media")
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
        eprintln!("native Preview tap test skipped: {message}");
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

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One raw TCP control session that heartbeats while it waits and
/// assembles monitor frames per tap.
struct Session {
    reader: BufReader<TcpStream>,
    pending: Vec<u8>,
    writer: TcpStream,
    server: ServerIdentity,
    heartbeats: u64,
    last_heartbeat: Instant,
    assemblers: [MonitorFrameAssembler; 2],
    monitor_parts: usize,
    commands: u64,
}

impl Session {
    fn connect(address: SocketAddr, client_type: ClientType, role: Role) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(READ_POLL)).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let hello = WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-preview-tap-test".into(),
            client_type,
            desired_role: role,
            resume_cursor: None,
        });
        writer
            .write_all(encode_line(&hello).unwrap().as_bytes())
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut pending = Vec::new();
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let server = loop {
            assert!(Instant::now() < deadline, "no handshake response");
            if let Some(WireMessage::HandshakeResponse(response)) =
                Self::read_line(&mut reader, &mut pending)
            {
                assert!(
                    matches!(response.outcome, HandshakeOutcome::Snapshot { .. }),
                    "{response:?}"
                );
                break response.server;
            }
        };
        Self {
            reader,
            pending,
            writer,
            server,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
            assemblers: [MonitorFrameAssembler::new(), MonitorFrameAssembler::new()],
            monitor_parts: 0,
            commands: 0,
        }
    }

    fn read_line(reader: &mut BufReader<TcpStream>, pending: &mut Vec<u8>) -> Option<WireMessage> {
        match reader.read_until(b'\n', pending) {
            Ok(0) => panic!("daemon closed the session"),
            Ok(_) if !pending.ends_with(b"\n") => None,
            Ok(_) => {
                let line = String::from_utf8(std::mem::take(pending)).unwrap();
                Some(decode_line(&line).unwrap())
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                None
            }
            Err(error) => panic!("{error}"),
        }
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
    }

    /// The next record (or `None` within the read poll), heartbeating as a
    /// real client does.
    fn receive(&mut self) -> Option<WireMessage> {
        if self.last_heartbeat.elapsed() >= HEARTBEAT_EVERY {
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
        let message = Self::read_line(&mut self.reader, &mut self.pending)?;
        if matches!(message, WireMessage::MonitorFrame(_)) {
            self.monitor_parts += 1;
        }
        Some(message)
    }

    /// The next completely assembled monitor image of either tap.
    fn next_image(&mut self, deadline: Instant, what: &str) -> MonitorImage {
        loop {
            assert!(Instant::now() < deadline, "never got {what}");
            if let Some(WireMessage::MonitorFrame(part)) = self.receive()
                && let Some(image) = self.assemblers[part.tap.index()].push(&part).unwrap()
            {
                return image;
            }
        }
    }

    /// Reads monitor images until `wanted` returns a value for one.
    fn image_until<T>(
        &mut self,
        what: &str,
        mut wanted: impl FnMut(&MonitorImage) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + FRAME_TIMEOUT;
        loop {
            let image = self.next_image(deadline, what);
            if let Some(found) = wanted(&image) {
                return found;
            }
        }
    }

    /// Sends `payload` and returns the frame its accepted result schedules.
    fn command(&mut self, payload: CommandPayload) -> u64 {
        self.commands += 1;
        let id = format!("preview-tap-{}", self.commands);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id.clone(),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let deadline = Instant::now() + FRAME_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "no result for {id}");
            match self.receive() {
                Some(WireMessage::CommandResult(CommandResult::Accepted {
                    id: found,
                    scheduled_frame,
                    ..
                })) if found == id => return scheduled_frame.expect("a scheduled frame"),
                Some(WireMessage::CommandResult(rejected @ CommandResult::Rejected { .. })) => {
                    panic!("{id} was refused: {rejected:?}")
                }
                // A monitor part read here still feeds its assembler.
                Some(WireMessage::MonitorFrame(part)) => {
                    let _ = self.assemblers[part.tap.index()].push(&part);
                }
                _ => {}
            }
        }
    }
}

/// The center pixel of `image`.
fn center(image: &MonitorImage) -> [u8; 3] {
    let (width, height) = (usize::from(image.width), usize::from(image.height));
    let offset = ((height / 2) * width + width / 2) * 4;
    [
        image.rgba[offset],
        image.rgba[offset + 1],
        image.rgba[offset + 2],
    ]
}

/// Whether every pixel of `image` is `color` within a small tolerance for
/// the SDR transform's rounding.
fn solid(image: &MonitorImage, color: [u8; 3]) -> bool {
    image.rgba.as_chunks::<4>().0.iter().all(|pixel| {
        pixel[..3]
            .iter()
            .zip(color)
            .all(|(&actual, expected)| actual.abs_diff(expected) <= 6)
    })
}

/// One field of the shutdown `FREEMIXD_TELEMETRY` record.
fn telemetry_field(stderr: &str, field: &str) -> f64 {
    let record = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_TELEMETRY\t"))
        .unwrap_or_else(|| panic!("no telemetry record: {stderr}"));
    record
        .split('\t')
        .find_map(|entry| entry.strip_prefix(&format!("{field}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {field} in {record}"))
}

/// A subscribed Studio peer sees Preview's own pixels, Preview following a
/// change, and the incoming source on Preview throughout a transition,
/// while Program stays red, then the swapped Preview after it; Program
/// frames are not made late.
#[test]
#[allow(clippy::too_many_lines)]
fn native_preview_tap_shows_the_preview_source_and_follows_it_without_late_program_frames() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("preview-tap.freemix");
    save_project(&project);
    let Some(daemon) = DaemonProcess::start(&project) else {
        return;
    };
    let mut studio = Session::connect(daemon.control, ClientType::Studio, Role::Admin);

    // Preview carries the Preview source's pixels, Program its own.
    let preview = studio.image_until("a native Preview frame", |image| {
        (image.tap == MonitorTap::Preview).then(|| image.clone())
    });
    assert_eq!(preview.origin, MonitorFrameOrigin::Native);
    assert_eq!(preview.input, wire(2));
    assert_eq!((preview.width, preview.height), (64, 48));
    assert!(solid(&preview, BLUE), "Preview is {:?}", center(&preview));
    let program = studio.image_until("a native Program frame", |image| {
        (image.tap == MonitorTap::Program).then(|| image.clone())
    });
    assert_eq!(program.input, wire(1));
    assert!(solid(&program, RED), "Program is {:?}", center(&program));

    // Preview follows a Preview change.
    let selected = studio.command(CommandPayload::SelectPreview { input: wire(3) });
    let changed = studio.image_until("the green Preview", |image| {
        (image.tap == MonitorTap::Preview && image.frame >= selected).then(|| image.clone())
    });
    assert_eq!(changed.input, wire(3));
    assert!(solid(&changed, GREEN), "Preview is {:?}", center(&changed));

    // During a transition Preview shows the incoming source.
    let start = studio.command(CommandPayload::Transition {
        kind: AutoTransitionKind::Fade,
        duration_frames: TRANSITION_FRAMES,
    });
    let end = start + u64::from(TRANSITION_FRAMES);
    let mut during = 0;
    let mut mixed_program = false;
    let after = studio.image_until("the Preview after the transition", |image| {
        if image.frame < start {
            return None;
        }
        match image.tap {
            MonitorTap::Preview if image.frame + 1 < end => {
                assert_eq!(image.input, wire(3), "frame {}", image.frame);
                assert!(
                    solid(image, GREEN),
                    "Preview at frame {} is {:?} during the transition",
                    image.frame,
                    center(image)
                );
                during += 1;
                None
            }
            MonitorTap::Preview if image.frame > end => Some(image.clone()),
            MonitorTap::Program if image.frame + 1 < end => {
                mixed_program |= !solid(image, RED) && !solid(image, GREEN);
                None
            }
            _ => None,
        }
    });
    assert!(during > 0, "no Preview frame arrived during the transition");
    assert!(mixed_program, "no Program frame showed the fade");
    assert_eq!(after.input, wire(1));
    assert!(solid(&after, RED), "Preview is {:?}", center(&after));
    drop(studio);

    let stderr = daemon.stop();
    assert!(telemetry_field(&stderr, "preview_monitor_frames_published") > 0.0);
    assert_eq!(
        telemetry_field(&stderr, "preview_monitor_frames_failed"),
        0.0,
        "{stderr}"
    );
    assert_eq!(
        telemetry_field(&stderr, "monitor_frames_failed"),
        0.0,
        "{stderr}"
    );
    let frame_ms = 1_000.0 / f64::from(FRAME_RATE);
    let p50 = telemetry_field(&stderr, "host_lateness_p50_ms");
    let p95 = telemetry_field(&stderr, "host_lateness_p95_ms");
    assert!(
        p50 < frame_ms / 2.0 && p95 < frame_ms,
        "Program frames were late with the Preview tap: host lateness p50 {p50} ms p95 {p95} ms"
    );
}

/// With only a CLI session (not eligible for monitor frames) the daemon
/// composes and reads back no monitor frame of either tap.
#[test]
fn native_preview_tap_is_not_produced_without_a_studio_subscriber() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("preview-tap-idle.freemix");
    save_project(&project);
    let Some(daemon) = DaemonProcess::start(&project) else {
        return;
    };
    let mut cli = Session::connect(daemon.control, ClientType::Cli, Role::Operator);
    let _ = cli.command(CommandPayload::SelectPreview { input: wire(3) });
    let served = Instant::now() + Duration::from_secs(2);
    while Instant::now() < served {
        cli.receive();
    }
    assert_eq!(
        cli.monitor_parts, 0,
        "a CLI session received monitor frames"
    );
    drop(cli);

    let stderr = daemon.stop();
    for field in [
        "preview_monitor_frames_published",
        "preview_monitor_frames_failed",
        "preview_monitor_frames_skipped",
        "monitor_frames_published",
    ] {
        assert_eq!(telemetry_field(&stderr, field), 0.0, "{field}: {stderr}");
    }
}
