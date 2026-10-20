//! Timed transitions on the native daemon's frame loop (SW-001, SW-002).
//!
//! A native command realizes only its first frame; the pacer realizes the
//! rest of a timed transition one frame per tick, like a held T-bar
//! transition. While it runs, every session keeps being served: Studio
//! receives Program and Preview monitor frames and meters, another
//! client's commands are answered (or refused as the engine refuses them
//! during a transition), and a session that only heartbeats survives a
//! transition longer than the heartbeat timeout. The recorder still gets
//! every frame of the transition.
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
    Input, InputKind, MainMix, Project, ProjectSettings, RecorderSlot, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    AutoTransitionKind, CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload,
    CommandResult, HandshakeOutcome, HandshakeRequest, HeartbeatMessage, MonitorFrameAssembler,
    MonitorImage, MonitorTap, Role, ServerIdentity, WireInputId, WireMessage, decode_line,
    encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const FRAME_TIMEOUT: Duration = Duration::from_secs(15);
const READ_POLL: Duration = Duration::from_millis(50);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);
const FRAME_RATE: u32 = 25;
const RED: [u8; 3] = [220, 30, 30];
const BLUE: [u8; 3] = [24, 80, 160];
const GREEN: [u8; 3] = [30, 200, 60];
/// The recorded show's width: a 125-frame wipe moves its edge exactly four
/// columns per frame, so every transition frame is told from its
/// neighbours in the recording.
const WIPE_WIDTH: u16 = 500;
const HEIGHT: u16 = 48;
/// A five-second transition at 25 fps.
const TRANSITION_FRAMES: u32 = 125;
/// A twenty-second transition: longer than the 15 s heartbeat timeout.
const LONG_TRANSITION_FRAMES: u32 = 500;
/// Monitor frames of each tap and meter records Studio must receive while
/// the transition runs.
const DURING: usize = 3;

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
        eprintln!("native transition test skipped: ffmpeg/ffprobe unavailable");
    }
    available
}

/// A `width`x48 25 fps show of three solid generators: red on
/// Program, blue on Preview, and green; recorder 1 runs when `record`.
fn save_project(path: &Path, width: u16, record: bool) {
    let rate = FrameRate::new(FRAME_RATE, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(7_301).unwrap()),
        "Native transition test",
        ProjectSettings {
            frame_rate: rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(u32::from(width), u32::from(HEIGHT)).unwrap(),
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
    project.set_recording_desired(RecorderSlot::new(1).unwrap(), record);
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
    /// Starts `freemixd serve --native-media`, recording Program to
    /// `record` when given, or returns `None` when this host has no native
    /// media runtime (unless `FM_REQUIRE_NATIVE_MEDIA=1`). Its frames are
    /// traced (`FREEMIXD_PACING_TRACE`).
    fn start(project: &Path, record: Option<&Path>) -> Option<Self> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_freemixd"));
        command
            .arg("serve")
            .arg(project)
            .arg("--native-media")
            .env("FREEMIXD_PACING_TRACE", "1");
        if let Some(record) = record {
            command.arg("--record-program").arg(record);
        }
        let mut child = command
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
        eprintln!("native transition test skipped: {message}");
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

/// One raw TCP control session that heartbeats while it reads and notes
/// every monitor image, meter record, and heartbeat acknowledgement it
/// receives. Reading panics if the daemon closes the session.
struct Session {
    reader: BufReader<TcpStream>,
    pending: Vec<u8>,
    writer: TcpStream,
    server: ServerIdentity,
    name: &'static str,
    heartbeats: u64,
    last_heartbeat: Instant,
    assemblers: [MonitorFrameAssembler; 2],
    images: Vec<MonitorImage>,
    meter_frames: Vec<u64>,
    /// When each heartbeat acknowledgement arrived.
    acknowledged: Vec<Instant>,
    commands: u64,
}

impl Session {
    fn connect(
        address: SocketAddr,
        name: &'static str,
        client_type: ClientType,
        role: Role,
    ) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(READ_POLL)).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let hello = WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: format!("native-transitions-{name}"),
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
            assert!(Instant::now() < deadline, "{name}: no handshake response");
            if let Some(WireMessage::HandshakeResponse(response)) =
                Self::read_line(name, &mut reader, &mut pending)
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
            name,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
            assemblers: [MonitorFrameAssembler::new(), MonitorFrameAssembler::new()],
            images: Vec::new(),
            meter_frames: Vec::new(),
            acknowledged: Vec::new(),
            commands: 0,
        }
    }

    fn read_line(
        name: &str,
        reader: &mut BufReader<TcpStream>,
        pending: &mut Vec<u8>,
    ) -> Option<WireMessage> {
        match reader.read_until(b'\n', pending) {
            Ok(0) => panic!("{name}: the daemon closed the session"),
            Ok(_) if !pending.ends_with(b"\n") => None,
            Ok(_) => {
                let line = String::from_utf8(std::mem::take(pending)).unwrap();
                Some(decode_line(&line).unwrap())
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                None
            }
            Err(error) => panic!("{name}: {error}"),
        }
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
    }

    /// The next record (or `None` within the read poll), heartbeating as a
    /// real client does and noting what the record shows.
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
        let message = Self::read_line(self.name, &mut self.reader, &mut self.pending)?;
        match &message {
            WireMessage::MonitorFrame(part) => {
                if let Some(image) = self.assemblers[part.tap.index()].push(part).unwrap() {
                    self.images.push(image);
                }
            }
            WireMessage::AudioMeters(meters) => self.meter_frames.push(meters.frame),
            WireMessage::HeartbeatAcknowledgement(_) => self.acknowledged.push(Instant::now()),
            _ => {}
        }
        Some(message)
    }

    /// Sends `payload` and returns its result.
    fn command(&mut self, payload: CommandPayload) -> CommandResult {
        self.commands += 1;
        let id = format!("{}-{}", self.name, self.commands);
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
            if let Some(WireMessage::CommandResult(result)) = self.receive() {
                let found = match &result {
                    CommandResult::Accepted { id, .. } | CommandResult::Rejected { id, .. } => id,
                };
                if *found == id {
                    return result;
                }
            }
        }
    }

    /// Sends `payload` and returns the frame its accepted result realized
    /// it on.
    fn accepted(&mut self, payload: CommandPayload) -> u64 {
        match self.command(payload) {
            CommandResult::Accepted {
                scheduled_frame, ..
            } => scheduled_frame.expect("a scheduled frame"),
            rejected @ CommandResult::Rejected { .. } => {
                panic!("{}: refused: {rejected:?}", self.name)
            }
        }
    }

    /// Reads until a monitor image of `tap` at or after `frame` arrives.
    fn read_until_image(&mut self, tap: MonitorTap, frame: u64, what: &str) {
        let deadline = Instant::now() + FRAME_TIMEOUT + FRAME_TIMEOUT;
        while !self
            .images
            .iter()
            .any(|image| image.tap == tap && image.frame >= frame)
        {
            assert!(Instant::now() < deadline, "{}: never got {what}", self.name);
            self.receive();
        }
    }
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

/// One field of the shutdown `FREEMIXD_RECORDER` record.
fn recorder_field(stderr: &str, field: &str) -> u64 {
    let record = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_RECORDER\t"))
        .unwrap_or_else(|| panic!("no recorder record: {stderr}"));
    record
        .split('\t')
        .find_map(|entry| entry.strip_prefix(&format!("{field}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {field} in {record}"))
}

/// The middle row of every video frame of `path`, decoded as RGB.
fn decoded_rows(path: &Path, width: usize, height: usize) -> Vec<Vec<[u8; 3]>> {
    let decode = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
        ])
        .args(["-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        decode.status.success() && decode.stderr.is_empty(),
        "{} does not decode cleanly: {}",
        path.display(),
        String::from_utf8_lossy(&decode.stderr)
    );
    let frame_bytes = width * height * 3;
    assert_eq!(decode.stdout.len() % frame_bytes, 0);
    decode
        .stdout
        .chunks_exact(frame_bytes)
        .map(|frame| {
            let row = &frame[(height / 2) * width * 3..][..width * 3];
            row.as_chunks::<3>().0.to_vec()
        })
        .collect()
}

/// Every Program frame of the shutdown `FREEMIXD_PACING_TRACE` record, in
/// frame order: how late it started and how long realizing it took, in
/// milliseconds.
fn traced_frames(stderr: &str) -> Vec<(f64, f64)> {
    let record = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_PACING_TRACE\t"))
        .unwrap_or_else(|| panic!("no pacing trace: {stderr}"));
    record
        .split('\t')
        .find_map(|field| field.strip_prefix("samples="))
        .unwrap_or_else(|| panic!("no samples in {record}"))
        .split(',')
        .filter(|sample| !sample.is_empty())
        .map(|sample| {
            let mut fields = sample.split(':');
            let mut milliseconds = || fields.next().unwrap().parse::<f64>().unwrap() / 1_000.0;
            (milliseconds(), milliseconds())
        })
        .collect()
}

/// How long the frame loop held each traced frame back on its own: the
/// frame's lateness less what realizing the frame before it overran the
/// interval by.
fn loop_stalls(frames: &[(f64, f64)]) -> Vec<f64> {
    let frame_ms = 1_000.0 / f64::from(FRAME_RATE);
    frames
        .iter()
        .enumerate()
        .map(|(index, &(lateness, _))| {
            let overrun = index
                .checked_sub(1)
                .map_or(0.0, |previous| {
                    let (lateness, realize) = frames[previous];
                    lateness + realize - frame_ms
                })
                .max(0.0);
            (lateness - overrun).max(0.0)
        })
        .collect()
}

/// The `percent` percentile of `samples` (nearest rank); 0 for none.
fn percentile(samples: &[f64], percent: usize) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (percent * sorted.len()).div_ceil(100);
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// How many pixels of `row` are nearer to blue than to red: the columns a
/// red-to-blue wipe has uncovered.
fn wiped_columns(row: &[[u8; 3]]) -> usize {
    let distance = |pixel: &[u8; 3], color: [u8; 3]| -> u32 {
        pixel
            .iter()
            .zip(color)
            .map(|(&actual, expected)| u32::from(actual.abs_diff(expected)).pow(2))
            .sum()
    };
    row.iter()
        .filter(|pixel| distance(pixel, BLUE) < distance(pixel, RED))
        .count()
}

/// A timed transition runs on the frame loop: its command is answered once
/// it started, Studio keeps receiving Program and Preview monitor frames
/// and meters through it, another client's command is answered mid
/// transition (and a Cut refused, as the engine refuses one while a
/// transition is in flight), and the recording holds every transition
/// frame, each with the wipe edge where that frame puts it.
#[test]
#[allow(clippy::too_many_lines)]
fn a_timed_transition_keeps_sessions_served_and_records_every_frame() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("transitions.freemix");
    save_project(&project, WIPE_WIDTH, true);
    let recording = directory.path().join("program.mp4");
    let Some(daemon) = DaemonProcess::start(&project, Some(&recording)) else {
        return;
    };
    let mut studio = Session::connect(daemon.control, "studio", ClientType::Studio, Role::Admin);
    let mut cli = Session::connect(daemon.control, "cli", ClientType::Cli, Role::Admin);
    studio.read_until_image(MonitorTap::Program, 0, "a first Program frame");

    // The other client waits for the transition to be running, then edits
    // and tries to cut.
    let (started_sender, started) = mpsc::channel::<()>();
    let other = thread::spawn(move || {
        started.recv().unwrap();
        thread::sleep(Duration::from_millis(500));
        let renamed = cli.accepted(CommandPayload::RenameInput {
            input: wire(3),
            name: "Renamed mid transition".into(),
        });
        let cut = cli.command(CommandPayload::Cut);
        // Kept alive (and heartbeating) until the transition is over.
        let deadline = Instant::now() + FRAME_TIMEOUT;
        while Instant::now() < deadline {
            cli.receive();
        }
        (renamed, cut)
    });

    let sent = Instant::now();
    let start = studio.accepted(CommandPayload::Transition {
        kind: AutoTransitionKind::Wipe,
        duration_frames: TRANSITION_FRAMES,
    });
    let answered = sent.elapsed();
    started_sender.send(()).unwrap();
    let end = start + u64::from(TRANSITION_FRAMES);
    assert!(
        answered < Duration::from_secs(4),
        "the transition was answered after {answered:?}, not once it started"
    );
    studio.read_until_image(
        MonitorTap::Program,
        end + 1,
        "the Program after the transition",
    );

    // Studio received both taps and meters while the transition ran; the
    // last transition frame swaps the buses, so it is left out.
    let during = |frame: u64| frame > start && frame + 1 < end;
    let program: Vec<&MonitorImage> = studio
        .images
        .iter()
        .filter(|image| image.tap == MonitorTap::Program && during(image.frame))
        .collect();
    let preview: Vec<&MonitorImage> = studio
        .images
        .iter()
        .filter(|image| image.tap == MonitorTap::Preview && during(image.frame))
        .collect();
    let meters = studio
        .meter_frames
        .iter()
        .filter(|&&frame| during(frame))
        .count();
    assert!(
        program.len() >= DURING && preview.len() >= DURING && meters >= DURING,
        "during the transition Studio got {} Program and {} Preview frames and {meters} meters",
        program.len(),
        preview.len()
    );
    for image in &program {
        assert!(
            !solid(image, RED) && !solid(image, BLUE),
            "Program at frame {} is not mid wipe",
            image.frame
        );
    }
    for image in &preview {
        assert_eq!(image.input, wire(2), "Preview at frame {}", image.frame);
        assert!(solid(image, BLUE), "Preview at frame {}", image.frame);
    }

    let (renamed, cut) = other.join().unwrap();
    assert!(
        renamed > start && renamed < end,
        "the other client's edit was realized on frame {renamed}, not inside {start}..{end}"
    );
    match cut {
        CommandResult::Rejected { code, .. } => assert_eq!(code, "conflict"),
        accepted @ CommandResult::Accepted { .. } => {
            panic!("a Cut during the transition was accepted: {accepted:?}")
        }
    }
    drop(studio);
    let stderr = daemon.stop();

    // Serving the sessions through the transition never held the frame
    // loop back: the typical frame starts on time but for what the frames
    // before it overran (see `loop_stalls`), however loaded the host.
    let frame_ms = 1_000.0 / f64::from(FRAME_RATE);
    let stall_p50 = percentile(&loop_stalls(&traced_frames(&stderr)), 50);
    assert!(
        stall_p50 < frame_ms / 2.0,
        "the frame loop was held back: stall p50 {stall_p50:.2} ms"
    );

    // Every realized frame reached the recording, and the transition's
    // frames are all there in order: the wipe edge moves exactly four
    // columns per frame, from four columns on its second frame to 496 on
    // its last.
    let rows = decoded_rows(&recording, usize::from(WIPE_WIDTH), usize::from(HEIGHT));
    let completed = recorder_field(&stderr, "completed_pairs");
    assert_eq!(u64::try_from(rows.len()).unwrap(), completed, "{stderr}");
    let wiped: Vec<usize> = rows.iter().map(|row| wiped_columns(row)).collect();
    let first = wiped
        .iter()
        .position(|&columns| columns > 0)
        .expect("the recording holds the wipe");
    let width = usize::from(WIPE_WIDTH);
    let run: Vec<usize> = wiped[first..]
        .iter()
        .copied()
        .take_while(|&columns| columns < width)
        .collect();
    let step = width / usize::try_from(TRANSITION_FRAMES).unwrap();
    assert_eq!(
        run.len(),
        usize::try_from(TRANSITION_FRAMES).unwrap() - 1,
        "the recorded wipe has {} partial frames: {run:?}",
        run.len()
    );
    for (index, &columns) in run.iter().enumerate() {
        let expected = step * (index + 1);
        assert!(
            columns.abs_diff(expected) < step / 2 + 1,
            "recorded wipe frame {index} uncovers {columns} columns, not {expected}: {run:?}"
        );
    }
    assert!(
        wiped[first + run.len()..]
            .iter()
            .all(|&columns| columns == width),
        "the wipe did not end on Preview: {wiped:?}"
    );
}

/// A session that only heartbeats is served throughout a transition
/// longer than the heartbeat timeout: its heartbeats are acknowledged
/// while the transition runs, and it is still connected after it.
#[test]
fn a_heartbeat_only_session_survives_a_transition_longer_than_its_timeout() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("long-transition.freemix");
    save_project(&project, 64, false);
    let Some(daemon) = DaemonProcess::start(&project, None) else {
        return;
    };
    let mut quiet = Session::connect(daemon.control, "quiet", ClientType::Cli, Role::Viewer);
    let mut operator =
        Session::connect(daemon.control, "operator", ClientType::Cli, Role::Operator);
    let start = operator.accepted(CommandPayload::Transition {
        kind: AutoTransitionKind::Fade,
        duration_frames: LONG_TRANSITION_FRAMES,
    });
    let started = Instant::now();
    let operator = thread::spawn(move || {
        // The operator waits for the end of the transition, then cuts.
        let length = Duration::from_secs(u64::from(LONG_TRANSITION_FRAMES / FRAME_RATE));
        while started.elapsed() < length + Duration::from_secs(2) {
            operator.receive();
        }
        operator.accepted(CommandPayload::Cut)
    });
    let length = Duration::from_secs(u64::from(LONG_TRANSITION_FRAMES / FRAME_RATE));
    while started.elapsed() < length + Duration::from_secs(3) {
        quiet.receive();
    }
    let cut = operator.join().unwrap();
    assert!(
        cut >= start + u64::from(LONG_TRANSITION_FRAMES),
        "the Cut on frame {cut} was taken inside the transition from {start}"
    );
    // Acknowledged throughout: no gap anywhere near the timeout.
    let mut previous = started;
    for &at in &quiet.acknowledged {
        if at < started {
            continue;
        }
        assert!(
            at.duration_since(previous) < Duration::from_secs(5),
            "no heartbeat was acknowledged for {:?} during the transition",
            at.duration_since(previous)
        );
        previous = at;
    }
    assert!(
        previous.duration_since(started) > length,
        "no heartbeat was acknowledged after the transition"
    );
}
