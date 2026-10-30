#![cfg(all(feature = "native-media", unix))]

//! Process-level highlight reel export (OR-013): a native daemon whose show
//! records a source for replay captures two events, a reel cuts one clip
//! from each, and `reel_export` writes a file whose exact frame count and
//! H.264 video `ffprobe` confirms. A second test cancels a long export and
//! proves the destination never appears.
//!
//! The tests need `FFmpeg` and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them they are skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
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
    StartupPolicy,
};
use fm_persistence::{
    ProjectPosition, ProjectStore, RuntimeFadeToBlack, RuntimeManualTransitions, RuntimeOverlays,
    RuntimeRouting, StoredProject,
};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, HeartbeatMessage, ReelExportState, ReelOperation, ReelSpec,
    ReelStatusMessage, ReelStatusRequest, ReplayOperation, ReplaySettings, ReplayStatusMessage,
    Role, ServerIdentity, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
const CAMERA: u128 = 1;
const PREVIEW: u128 = 2;
const REEL: u64 = 1;
const JOB: u64 = 1;
/// Commands sent by every session of the tests.
static COMMANDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg/ffprobe are unavailable"
    );
    available
}

/// A 128x72 show with red input 1 on Program, blue input 2 on Preview, and
/// input 1 recorded for replay with a ten-second window in the daemon's
/// default root.
#[allow(clippy::too_many_lines)] // one fixture: inputs, scene, output, replay
fn save_project(path: &Path) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(9_500)),
        "Native reel",
        ProjectSettings {
            frame_rate: rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(128, 72).unwrap(),
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
    let bus = BusId::new(id(9_550));
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    // Camera 1 is a looping red clip with a 1 kHz tone, so exports carry
    // its recorded audio; camera 2 a silent blue generator.
    let store = ProjectStore::new(path).unwrap();
    std::fs::create_dir_all(store.assets_root()).unwrap();
    generate_tone_clip(&store.assets_root().join(CAMERA_CLIP));
    project.add_input(Input {
        id: input(CAMERA),
        name: "Camera 1".into(),
        kind: InputKind::Media {
            asset_uri: format!("asset://{CAMERA_CLIP}"),
            playback: fm_model::MediaPlaybackSettings {
                end_action: fm_model::MediaEndAction::Loop,
                ..fm_model::MediaPlaybackSettings::default()
            },
        },
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input(PREVIEW),
        name: "Camera 2".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(20, 30, 210, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    let scene = SceneId::new(id(9_510));
    project.add_scene(Scene {
        id: scene,
        name: "Program".into(),
        background: Rgba8::OPAQUE_BLACK,
        layers: vec![Layer {
            id: fm_model::LayerId::new(1).unwrap(),
            name: "Source".into(),
            source: SourceRef::Input(input(CAMERA)),
            enabled: true,
            geometry: LayerGeometry::new(0, 0, 128, 72, Rotation::Deg0),
            crop: None,
            mask: None,
            key: None,
            effects: Vec::new(),
            opacity: u8::MAX,
            z_order: 0,
        }],
        animation: fm_model::SceneAnimation::default(),
    });
    project.add_output(Output {
        id: OutputId::new(id(9_501)),
        name: "Program".into(),
        video_source: scene,
        audio_source: bus,
        startup: StartupPolicy::ReconcileDesiredState,
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(input(CAMERA), input(PREVIEW)));
    project
        .set_replay(ReplaySettings {
            sources: vec![input(CAMERA), input(PREVIEW)],
            retention_seconds: 10,
            storage_root: None,
            auto_return: None,
        })
        .unwrap();
    let stored = StoredProject::from_project_with_complete_runtime_state(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(CAMERA)),
            realized_program_id: Some(input(CAMERA)),
            desired_preview_id: Some(input(PREVIEW)),
            realized_preview_id: Some(input(PREVIEW)),
        },
        RuntimeManualTransitions::default(),
        RuntimeFadeToBlack::default(),
        RuntimeOverlays::default(),
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
}

/// The red camera's clip asset.
const CAMERA_CLIP: &str = "camera.mkv";

/// Thirty seconds of a red (210, 20, 20) 128x72 picture at 25 fps with a
/// 1 kHz tone.
fn generate_tone_clip(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=0xd21414:s=128x72:r=25:d=30,format=yuv420p,\
             setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:d=30",
        ])
        .args(["-map", "0:v", "-map", "1:a"])
        .args(["-c:v", "mpeg2video", "-g", "12", "-q:v", "2"])
        .args(["-pix_fmt", "yuv420p", "-c:a", "pcm_f32le", "-ac", "2"])
        .args([
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
        ])
        .args(["-f", "matroska", "-y"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "fixture generation failed: {status}");
}

/// The peak sample of `path`'s audio between `from` and `to` seconds.
fn audio_peak(path: &Path, from: f64, to: f64) -> f32 {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-ss", &from.to_string(), "-to", &to.to_string()])
        .args(["-map", "0:a", "-f", "f32le", "-ac", "2", "-"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    output
        .stdout
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()).abs())
        .fold(0.0, f32::max)
}

struct NativeDaemon {
    child: Child,
    address: SocketAddr,
    log: mpsc::Receiver<String>,
}

impl Drop for NativeDaemon {
    fn drop(&mut self) {
        if thread::panicking() {
            thread::sleep(Duration::from_secs(5));
            eprintln!("daemon exit: {:?}", self.child.try_wait());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if thread::panicking() {
            while let Ok(line) = self.log.recv_timeout(Duration::from_secs(2)) {
                eprintln!("daemon: {line}");
            }
        }
    }
}

fn start_native(project: &Path) -> Result<NativeDaemon, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project)
        .arg("--native-media")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot spawn native daemon: {error}"))?;
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let line = receiver.recv_timeout(STARTUP_TIMEOUT).unwrap_or_default();
    if let Ok(readiness) = line.parse::<ReadinessRecord>() {
        let (log_sender, log) = mpsc::channel();
        if let Some(stderr) = child.stderr.take() {
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ = log_sender.send(line);
                }
            });
        }
        return Ok(NativeDaemon {
            child,
            address: readiness.address,
            log,
        });
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(format!("native daemon did not become ready: {stderr}"))
}

struct Client {
    reader: BufReader<TcpStream>,
    pending: Vec<u8>,
    writer: TcpStream,
    server: Option<ServerIdentity>,
    heartbeats: u64,
    last_heartbeat: Instant,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            pending: Vec::new(),
            server: None,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-reel-test".into(),
            client_type: ClientType::Studio,
            desired_role: Role::Admin,
            resume_cursor: None,
        }));
        let WireMessage::HandshakeResponse(response) = client.wait_message() else {
            panic!("expected the handshake response");
        };
        client.server = Some(response.server);
        let WireMessage::Snapshot(_) = client.wait_message() else {
            panic!("expected the snapshot");
        };
        client
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    fn heartbeat(&mut self) {
        if let Some(server) = self.server.clone()
            && self.last_heartbeat.elapsed() >= Duration::from_secs(1)
        {
            self.heartbeats += 1;
            self.send(&WireMessage::Heartbeat(HeartbeatMessage {
                server,
                sequence: self.heartbeats,
                sent_at_ms: self.heartbeats,
                last_applied: None,
            }));
            self.last_heartbeat = Instant::now();
        }
    }

    fn poll(&mut self) -> Option<WireMessage> {
        self.heartbeat();
        match self.reader.read_until(b'\n', &mut self.pending) {
            Ok(0) => panic!("daemon closed"),
            Ok(_) if !self.pending.ends_with(b"\n") => None,
            Ok(_) => {
                let line = String::from_utf8(std::mem::take(&mut self.pending)).unwrap();
                Some(decode_line(&line).unwrap())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                None
            }
            Err(error) => panic!("read failed: {error}"),
        }
    }

    fn wait_message(&mut self) -> WireMessage {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for a message");
            if let Some(message) = self.poll() {
                return message;
            }
        }
    }

    fn wait_for<T>(
        &mut self,
        what: &str,
        mut found: impl FnMut(Option<WireMessage>) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let message = self.poll();
            if let Some(value) = found(message) {
                return value;
            }
        }
    }

    fn command(&mut self, payload: CommandPayload) {
        let id = format!(
            "reel-{}",
            COMMANDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
        );
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let result = self.wait_for("the command result", |message| match message {
            Some(WireMessage::CommandResult(result)) => Some(result),
            _ => None,
        });
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
    }

    fn replay(&mut self, operation: ReplayOperation) {
        self.command(CommandPayload::Replay(operation));
    }

    fn reel(&mut self, operation: ReelOperation) {
        self.command(CommandPayload::Reel(operation));
    }

    /// Waits for a pushed `replay_status` that satisfies `wanted`.
    fn status(
        &mut self,
        what: &str,
        wanted: impl Fn(&ReplayStatusMessage) -> bool,
    ) -> ReplayStatusMessage {
        self.wait_for(what, |message| match message {
            Some(WireMessage::ReplayStatus(status)) if wanted(&status) => Some(*status),
            _ => None,
        })
    }

    /// Asks for the reel status and waits for the answer.
    fn request_reel_status(&mut self) -> ReelStatusMessage {
        let request_id = format!(
            "reel-status-{}",
            COMMANDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
        );
        self.send(&WireMessage::ReelStatusRequest(ReelStatusRequest {
            request_id: request_id.clone(),
        }));
        self.wait_for("the reel status answer", |message| match message {
            Some(WireMessage::ReelStatus(status))
                if status.request_id.as_deref() == Some(request_id.as_str()) =>
            {
                Some(*status)
            }
            _ => None,
        })
    }

    /// Polls `reel_status` until it satisfies `wanted`.
    fn poll_reel_status(
        &mut self,
        what: &str,
        wanted: impl Fn(&ReelStatusMessage) -> bool,
    ) -> ReelStatusMessage {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let status = self.request_reel_status();
            if wanted(&status) {
                return status;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// One clip of `event` on the recorded camera, at real time.
fn clip(event: u32) -> fm_protocol::ReelClip {
    fm_protocol::ReelClip {
        event,
        angle: input(CAMERA),
        trim_in_frames: 0,
        trim_out_frames: 0,
        speed_percent: 100,
        dissolve_frames: 0,
    }
}

fn set_reel(spec: ReelSpec) -> ReelOperation {
    ReelOperation::Set(Box::new(spec))
}

/// Records until three segments exist, captures a five-second event, lets
/// the live edge move past it, then captures a second five-second event.
fn two_complete_events(client: &mut Client) -> (ReplayStatusMessage, ReplayStatusMessage) {
    client.status("three recorded segments", |status| {
        status
            .recorded
            .first()
            .is_some_and(|source| source.segments >= 3)
    });
    client.replay(ReplayOperation::CaptureLast { seconds: 5 });
    let first = client.status("a complete first event", |status| {
        status.events.first().is_some_and(|event| event.complete)
    });
    let first_out = first.events[0].out_frame;
    client.status("the live edge past the first event", |status| {
        status.live_frame.is_some_and(|live| live >= first_out + 25)
    });
    client.replay(ReplayOperation::CaptureLast { seconds: 5 });
    let second = client.status("a complete second event", |status| {
        status.events.len() >= 2 && status.events[1].complete
    });
    (first, second)
}

/// `ffprobe`'s `stream` fields for the selected stream, as `key=value` pairs.
fn probe(path: &Path, selector: &str) -> BTreeMap<String, String> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            selector,
            "-count_frames",
            "-show_entries",
            "stream=codec_name,pix_fmt,width,height,nb_read_frames,sample_rate,channels",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn part_of(destination: &Path) -> PathBuf {
    let mut part = destination.as_os_str().to_os_string();
    part.push(".part");
    PathBuf::from(part)
}

/// The four-character types of the top-level MP4 boxes in `path`.
fn top_level_boxes(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    let mut boxes = Vec::new();
    let mut at = 0_usize;
    while at + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        let size = match size {
            0 => bytes.len() - at,
            1 => usize::try_from(u64::from_be_bytes(
                bytes[at + 8..at + 16].try_into().unwrap(),
            ))
            .unwrap(),
            size => usize::try_from(size).unwrap(),
        };
        assert!(size >= 8, "a malformed box at byte {at}");
        boxes.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned());
        at += size;
    }
    assert_eq!(at, bytes.len(), "the boxes do not end with the file");
    boxes
}

/// The RGB of the centre pixel of video frame `index` of `path`.
fn centre_pixel(path: &Path, index: u64) -> [u8; 3] {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-vf",
            &format!("select=eq(n\\,{index})"),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 128x72 RGB: the centre is row 36, column 64.
    let at = (36 * 128 + 64) * 3;
    assert!(output.stdout.len() >= at + 3, "frame {index} is missing");
    [
        output.stdout[at],
        output.stdout[at + 1],
        output.stdout[at + 2],
    ]
}

fn assert_near(found: [u8; 3], wanted: [u8; 3], what: &str) {
    assert!(
        found
            .iter()
            .zip(wanted)
            .all(|(found, wanted)| found.abs_diff(wanted) <= 24),
        "{what}: {found:?}, wanted about {wanted:?}"
    );
}

fn assert_no_reel_failures(daemon: &NativeDaemon) {
    let failures: Vec<String> = std::iter::from_fn(|| daemon.log.try_recv().ok())
        .filter(|line| line.contains("FREEMIXD_REEL_FAILURE"))
        .collect();
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn native_daemon_exports_a_reel_with_the_exact_frame_count() {
    if !tools_available() {
        eprintln!("native reel integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("reel.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native reel integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.command(CommandPayload::Media {
        input: fm_protocol::WireInputId::from_domain(input(CAMERA)),
        operation: fm_protocol::MediaOperation::Play,
    });
    let (first, second) = two_complete_events(&mut client);
    // Protocol 2.125: the daemon samples the replay root and predicts the
    // recording time left from the segments it has written.
    let storage = first.storage.expect("the daemon samples its replay root");
    assert!(storage.free_bytes > 0);
    assert!(storage.bytes_per_second > 0);
    assert!(storage.remaining_seconds.is_some());
    let first_event = &first.events[0];
    let second_event = second
        .events
        .iter()
        .find(|event| event.id != first_event.id)
        .expect("the second event");
    assert_eq!(first_event.out_frame - first_event.in_frame, 125);
    assert_eq!(second_event.out_frame - second_event.in_frame, 125);
    let expected = (first_event.out_frame - first_event.in_frame)
        + (second_event.out_frame - second_event.in_frame);

    client.reel(set_reel(ReelSpec {
        id: REEL,
        name: "Highlights".into(),
        clips: vec![clip(first_event.id), clip(second_event.id)],
        music: None,
    }));
    let planned = client.poll_reel_status("the reel plan", |status| {
        status
            .reels
            .first()
            .is_some_and(|reel| reel.total_frames == Some(expected))
    });
    assert_eq!(planned.reels[0].error, None);

    let destination = directory.path().join("highlights.mp4");
    client.reel(ReelOperation::Export {
        job: JOB,
        reel: REEL,
        destination: destination.to_string_lossy().into_owned(),
    });
    let completed = client.poll_reel_status("the export completed", |status| {
        status
            .exports
            .iter()
            .any(|export| export.job == JOB && export.state == ReelExportState::Completed)
    });
    let export = completed
        .exports
        .iter()
        .find(|export| export.job == JOB)
        .unwrap();
    assert_eq!(export.reel, REEL);
    assert_eq!(export.completed_frames, expected);
    assert_eq!(export.total_frames, Some(expected));
    assert_eq!(export.failure, None);
    let bytes = std::fs::metadata(&destination).unwrap().len();
    assert_eq!(export.bytes, Some(bytes));
    assert!(bytes > 0);

    let video = probe(&destination, "v:0");
    assert_eq!(video.get("codec_name").map(String::as_str), Some("h264"));
    assert_eq!(video.get("pix_fmt").map(String::as_str), Some("yuv420p"));
    assert_eq!(video.get("width").map(String::as_str), Some("128"));
    assert_eq!(video.get("height").map(String::as_str), Some("72"));
    let expected_text = expected.to_string();
    assert_eq!(
        video.get("nb_read_frames").map(String::as_str),
        Some(expected_text.as_str())
    );
    let audio = probe(&destination, "a:0");
    assert_eq!(audio.get("codec_name").map(String::as_str), Some("aac"));
    assert_eq!(audio.get("sample_rate").map(String::as_str), Some("48000"));
    assert_eq!(audio.get("channels").map(String::as_str), Some("2"));
    // Faststart: one index ahead of the media, and no fragments.
    let boxes = top_level_boxes(&destination);
    let moov = boxes.iter().position(|kind| kind == "moov");
    let mdat = boxes.iter().position(|kind| kind == "mdat");
    assert!(moov.is_some() && mdat.is_some() && moov < mdat, "{boxes:?}");
    assert!(!boxes.iter().any(|kind| kind == "moof"), "{boxes:?}");
    assert!(!part_of(&destination).exists());
    let mut remuxed = destination.as_os_str().to_os_string();
    remuxed.push(".faststart.part");
    assert!(!Path::new(&remuxed).exists());
    assert_no_reel_failures(&daemon);
}

/// Schema 84: a reel that dissolves from one recorded angle (red) into
/// another (blue) is that many frames shorter, and its overlap blends the
/// two pictures linearly: red before, an even mix halfway, blue after.
#[test]
#[allow(clippy::too_many_lines)] // the reel, its export, then pictures and audio
fn native_daemon_exports_a_dissolve_between_two_angles() {
    if !tools_available() {
        eprintln!("native reel dissolve skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("dissolve.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native reel dissolve skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.command(CommandPayload::Media {
        input: fm_protocol::WireInputId::from_domain(input(CAMERA)),
        operation: fm_protocol::MediaOperation::Play,
    });
    let (first, second) = two_complete_events(&mut client);
    // Protocol 2.125: the daemon samples the replay root and predicts the
    // recording time left from the segments it has written.
    let storage = first.storage.expect("the daemon samples its replay root");
    assert!(storage.free_bytes > 0);
    assert!(storage.bytes_per_second > 0);
    assert!(storage.remaining_seconds.is_some());
    let first_event = &first.events[0];
    let second_event = second
        .events
        .iter()
        .find(|event| event.id != first_event.id)
        .expect("the second event");
    assert_eq!(first_event.out_frame - first_event.in_frame, 125);
    assert_eq!(second_event.out_frame - second_event.in_frame, 125);
    let dissolve: u32 = 25;
    let expected = 125 + 125 - u64::from(dissolve);

    client.reel(set_reel(ReelSpec {
        id: REEL,
        name: "Dissolve".into(),
        clips: vec![
            clip(first_event.id),
            fm_protocol::ReelClip {
                angle: input(PREVIEW),
                dissolve_frames: dissolve,
                ..clip(second_event.id)
            },
        ],
        music: None,
    }));
    let planned = client.poll_reel_status("the reel plan", |status| {
        status
            .reels
            .first()
            .is_some_and(|reel| reel.total_frames == Some(expected))
    });
    assert_eq!(planned.reels[0].error, None);

    let destination = directory.path().join("dissolve.mp4");
    client.reel(ReelOperation::Export {
        job: JOB,
        reel: REEL,
        destination: destination.to_string_lossy().into_owned(),
    });
    let completed = client.poll_reel_status("the export completed", |status| {
        status.exports.iter().any(|export| {
            export.job == JOB
                && matches!(
                    export.state,
                    ReelExportState::Completed | ReelExportState::Failed
                )
        })
    });
    let export = &completed.exports[0];
    assert_eq!(export.failure, None);
    assert_eq!(export.state, ReelExportState::Completed);
    assert_eq!(export.completed_frames, expected);
    let video = probe(&destination, "v:0");
    let expected_text = expected.to_string();
    assert_eq!(
        video.get("nb_read_frames").map(String::as_str),
        Some(expected_text.as_str())
    );
    // The overlap is output frames 100..125: frame 112 weights each angle
    // 13/26, an even mix of the red clip and the blue camera.
    let before = centre_pixel(&destination, 50);
    let after = centre_pixel(&destination, 180);
    assert!(
        before[0] > 150 && before[2] < 80,
        "before: {before:?} is not red"
    );
    assert_near(after, [20, 30, 210], "after");
    let average: Vec<u8> = before
        .iter()
        .zip(after)
        .map(|(red, blue)| red.midpoint(blue))
        .collect();
    assert_near(
        centre_pixel(&destination, 112),
        average.try_into().unwrap(),
        "halfway",
    );
    // The red camera's recorded tone plays before the dissolve, and the
    // silent blue camera after it.
    let before = audio_peak(&destination, 0.5, 3.5);
    let after = audio_peak(&destination, 6.0, 8.5);
    assert!(
        before > 0.05,
        "no recorded audio before the dissolve: {before}"
    );
    assert!(after < 0.01, "audio after the dissolve: {after}");
    assert_no_reel_failures(&daemon);
}

/// A mono 44.1 kHz WAV of a 300 Hz sine at half scale, `seconds` long.
fn generate_music_bed(path: &Path, seconds: u32) {
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=300:sample_rate=44100:d={seconds}"),
        ])
        .args(["-af", "volume=4", "-ac", "1", "-c:a", "pcm_s16le", "-y"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "fixture generation failed: {status}");
}

/// Schema 86: a reel's music bed plays under its clips at its level,
/// ducks while a real-time clip's recorded audio plays, and fades out at
/// the reel's end; a bed that cannot be opened fails the export.
#[test]
#[allow(clippy::too_many_lines)] // the reel, two exports, then the audio
fn native_daemon_mixes_a_ducked_music_bed_under_a_reel() {
    if !tools_available() {
        eprintln!("native reel integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("reel.freemix");
    save_project(&project_path);
    let bed = directory.path().join("bed.wav");
    generate_music_bed(&bed, 30);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native reel integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.command(CommandPayload::Media {
        input: fm_protocol::WireInputId::from_domain(input(CAMERA)),
        operation: fm_protocol::MediaOperation::Play,
    });
    let (first, second) = two_complete_events(&mut client);
    // Protocol 2.125: the daemon samples the replay root and predicts the
    // recording time left from the segments it has written.
    let storage = first.storage.expect("the daemon samples its replay root");
    assert!(storage.free_bytes > 0);
    assert!(storage.bytes_per_second > 0);
    assert!(storage.remaining_seconds.is_some());
    let first_event = &first.events[0];
    let second_event = second
        .events
        .iter()
        .find(|event| event.id != first_event.id)
        .expect("the second event");
    // Five seconds in real time (the tone plays), then ten at half speed
    // (slow motion is silent, so only the bed plays).
    let expected = 125 + 250;
    let music = fm_protocol::ReelMusic {
        path: bed.to_string_lossy().into_owned(),
        level_percent: 80,
        duck_percent: 25,
    };
    client.reel(set_reel(ReelSpec {
        id: REEL,
        name: "Music".into(),
        clips: vec![
            clip(first_event.id),
            fm_protocol::ReelClip {
                speed_percent: 50,
                ..clip(second_event.id)
            },
        ],
        music: Some(music.clone()),
    }));
    let planned = client.poll_reel_status("the reel plan", |status| {
        status.reels.first().is_some_and(|reel| {
            reel.total_frames == Some(expected) && reel.spec.music.as_ref() == Some(&music)
        })
    });
    assert_eq!(planned.reels[0].error, None);

    let destination = directory.path().join("music.mp4");
    client.reel(ReelOperation::Export {
        job: JOB,
        reel: REEL,
        destination: destination.to_string_lossy().into_owned(),
    });
    let completed = client.poll_reel_status("the export completed", |status| {
        status
            .exports
            .iter()
            .any(|export| export.job == JOB && export.state == ReelExportState::Completed)
    });
    let export = completed
        .exports
        .iter()
        .find(|export| export.job == JOB)
        .unwrap();
    assert_eq!(export.completed_frames, expected);
    assert_no_reel_failures(&daemon);

    // The bed alone at 80 % of its half-scale sine.
    let alone = audio_peak(&destination, 6.0, 13.5);
    assert!((0.3..0.5).contains(&alone), "bed alone {alone}");
    // Under the tone (1/8 scale) it is ducked to a quarter: far below the
    // 0.525 the two would reach undocked.
    let ducked = audio_peak(&destination, 1.0, 4.5);
    assert!((0.1..0.3).contains(&ducked), "ducked {ducked}");
    // The last quarter second fades toward silence.
    let faded = audio_peak(&destination, 14.75, 14.95);
    assert!(faded < 0.16, "faded {faded}");

    // A bed that cannot be opened fails the export.
    client.reel(set_reel(ReelSpec {
        id: REEL,
        name: "Music".into(),
        clips: vec![clip(first_event.id)],
        music: Some(fm_protocol::ReelMusic {
            path: directory
                .path()
                .join("missing.wav")
                .to_string_lossy()
                .into_owned(),
            ..music
        }),
    }));
    let missing = directory.path().join("missing.mp4");
    client.reel(ReelOperation::Export {
        job: JOB + 1,
        reel: REEL,
        destination: missing.to_string_lossy().into_owned(),
    });
    let failed = client.poll_reel_status("the export failed", |status| {
        status
            .exports
            .iter()
            .any(|export| export.job == JOB + 1 && export.state == ReelExportState::Failed)
    });
    let export = failed
        .exports
        .iter()
        .find(|export| export.job == JOB + 1)
        .unwrap();
    assert!(
        export
            .failure
            .as_deref()
            .is_some_and(|failure| failure.contains("music:open")),
        "{:?}",
        export.failure
    );
    assert!(!missing.exists());
    assert!(!part_of(&missing).exists());
}

#[test]
fn native_daemon_cancels_a_reel_export_without_a_destination() {
    if !tools_available() {
        eprintln!("native reel integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("reel.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native reel integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.command(CommandPayload::Media {
        input: fm_protocol::WireInputId::from_domain(input(CAMERA)),
        operation: fm_protocol::MediaOperation::Play,
    });
    client.status("three recorded segments", |status| {
        status
            .recorded
            .first()
            .is_some_and(|source| source.segments >= 3)
    });
    client.replay(ReplayOperation::CaptureLast { seconds: 5 });
    let captured = client.status("a complete event", |status| {
        status.events.first().is_some_and(|event| event.complete)
    });
    let event = captured.events[0].clone();
    let event_frames = event.out_frame - event.in_frame;
    // Sixty-four clips at quarter speed: long enough that the cancel lands
    // while the encoder is still running.
    let clips: Vec<fm_protocol::ReelClip> = (0..64)
        .map(|_| fm_protocol::ReelClip {
            speed_percent: 25,
            ..clip(event.id)
        })
        .collect();
    let expected = event_frames * 64 * 4;
    client.reel(set_reel(ReelSpec {
        id: REEL,
        name: "Long reel".into(),
        clips,
        music: None,
    }));
    client.poll_reel_status("the long reel plan", |status| {
        status
            .reels
            .first()
            .is_some_and(|reel| reel.total_frames == Some(expected))
    });

    let destination = directory.path().join("cancelled.mp4");
    let part = part_of(&destination);
    client.reel(ReelOperation::Export {
        job: JOB,
        reel: REEL,
        destination: destination.to_string_lossy().into_owned(),
    });
    client.poll_reel_status("the export running", |status| {
        status
            .exports
            .iter()
            .any(|export| export.job == JOB && export.state == ReelExportState::Running)
    });
    client.reel(ReelOperation::CancelExport { job: JOB });
    client.poll_reel_status("the export cancelled", |status| {
        status
            .exports
            .iter()
            .any(|export| export.job == JOB && export.state == ReelExportState::Cancelled)
    });

    let deadline = Instant::now() + Duration::from_secs(15);
    while (destination.exists() || part.exists()) && Instant::now() < deadline {
        client.poll();
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!destination.exists(), "{destination:?} was written");
    assert!(!part.exists(), "{part:?} was left behind");
    assert_no_reel_failures(&daemon);
}
