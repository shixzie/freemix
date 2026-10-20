#![cfg(feature = "native-media")]

//! Process-level IN-006 on the native runtime: a media list created from
//! host files imports them into the bundle, airs its active item through the
//! input's one media transport (decoded by the native file player), and
//! when the item reaches its out mark the daemon journals the Auto Next
//! advance and Program shows the next file from its in mark. The advance
//! replays after a crash.
//!
//! The fixtures draw each frame's number (plus a per-file offset) as bit
//! blocks, so the Program monitor tells which file and frame is on air.
//!
//! The test needs `FFmpeg` tools and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    io::{BufRead, BufReader, Read, Write},
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
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventPayload, HandshakeRequest, MediaListItemSpec, MediaListOperation, MediaListOptionsSpec,
    MediaListStatus, MediaOperation, MonitorFrameAssembler, MonitorImage, Role, SnapshotMessage,
    WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

#[path = "support/heartbeat.rs"]
mod heartbeat;
use heartbeat::Heartbeat;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(30);
const LIST: u128 = 9;
/// The second file's frames draw their number plus this offset.
const SECOND: u64 = 128;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn list() -> WireInputId {
    WireInputId::from_domain(input(LIST))
}

/// Whether `FFmpeg` tools are on `PATH`; `FM_REQUIRE_FFMPEG=1` makes their
/// absence a failure.
fn ffmpeg_available() -> bool {
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

/// A 2 s, 25 fps, 320x180 Matroska clip whose frame N draws `N + offset`
/// (mod 256) as eight 40-pixel-wide black/white bit blocks, low bit left,
/// with a mono 48 kHz PCM track.
fn generate_clip(path: &Path, offset: u64) {
    let video = format!(
        "color=c=black:s=320x180:r=25:d=2,format=gray,\
         geq=lum='if(mod(floor((N+{offset})/pow(2\\,floor(X/40)))\\,2)\\,235\\,16)',\
         setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv"
    );
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", &video])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:d=2",
        ])
        .args(["-map", "0:v", "-map", "1:a"])
        .args(["-c:v", "mpeg2video", "-g", "12", "-bf", "2", "-q:v", "2"])
        .args(["-pix_fmt", "yuv420p", "-c:a", "pcm_f32le"])
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

/// Two silent solid generators on Program and Preview.
fn save_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(47).unwrap()),
        "Native media lists",
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
    for (id, name, red) in [(1, "Solid", 12), (2, "Other", 200)] {
        project.add_input(Input {
            id: input(id),
            name: name.into(),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(red, 34, 56, 255)),
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

/// A started native daemon, killed and reaped on drop.
struct NativeDaemon {
    child: Child,
    address: SocketAddr,
    log: mpsc::Receiver<String>,
}

impl Drop for NativeDaemon {
    fn drop(&mut self) {
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
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
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

fn start_or_skip(project: &Path) -> Option<NativeDaemon> {
    match start_native(project) {
        Ok(daemon) => Some(daemon),
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native media list integration skipped: {error}");
            None
        }
    }
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    heartbeat: Heartbeat,
    assembler: MonitorFrameAssembler,
    commands: u64,
}

impl Client {
    /// Connects as Studio (which receives the Program monitor) and returns
    /// the snapshot with its `media_list_state` records.
    fn connect(address: SocketAddr) -> (Self, SnapshotMessage, Vec<MediaListStatus>) {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            heartbeat: Heartbeat::new(),
            assembler: MonitorFrameAssembler::new(),
            commands: 0,
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-media-lists-test".into(),
            client_type: ClientType::Studio,
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
        let mut states = Vec::new();
        while states.len() < usize::from(snapshot.media_list_states) {
            if let WireMessage::MediaListState(state) = client.receive() {
                states.push(state.status);
            }
        }
        (client, snapshot, states)
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
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
                "daemon closed"
            );
            let message = decode_line(&line).unwrap();
            if !self.heartbeat.observe(&message) {
                return message;
            }
        }
    }

    fn wait_for<T>(
        &mut self,
        what: &str,
        mut found: impl FnMut(&mut Self, WireMessage) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let message = self.receive();
            if let Some(value) = found(self, message) {
                return value;
            }
        }
    }

    /// The frame number the next Program monitor picture of the list shows.
    fn shown_frame(&mut self, message: WireMessage) -> Option<u64> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        let image = self.assembler.push(&part).unwrap()?;
        (image.input == list()).then(|| frame_number(&image))
    }

    /// Submits `payload`, waits for its acceptance, and returns the list
    /// status of its `media_list_changed` event when it has one.
    fn accept(&mut self, payload: CommandPayload) -> Option<MediaListStatus> {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        let described = format!("{payload:?}");
        let lists = matches!(payload, CommandPayload::MediaList { .. });
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let mut accepted = false;
        let mut status = None;
        self.wait_for("the command", |_, message| {
            match message {
                WireMessage::CommandResult(result) => {
                    assert!(
                        matches!(result, CommandResult::Accepted { .. }),
                        "{described}: {result:?}"
                    );
                    accepted = true;
                }
                WireMessage::Event(event) => {
                    if let EventPayload::MediaListChanged {
                        status: changed, ..
                    } = event.payload
                    {
                        status = Some(*changed);
                    }
                }
                _ => {}
            }
            (accepted && (!lists || status.is_some())).then(|| status.clone())
        })
    }
}

/// The frame number (mod 256) the picture's bit blocks encode.
fn frame_number(image: &MonitorImage) -> u64 {
    let width = usize::from(image.width);
    (0..8).fold(0, |value, bit| {
        let x = bit * 40 + 20;
        let red = image.rgba[(90 * width + x) * 4];
        if red > 128 { value | (1 << bit) } else { value }
    })
}

#[test]
#[allow(clippy::too_many_lines)]
fn a_native_media_list_airs_host_files_and_auto_advances_frame_accurately() {
    if !ffmpeg_available() {
        eprintln!("native media list integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-media-lists.freemix");
    save_project(&project_path);
    let host = directory.path().join("host");
    std::fs::create_dir(&host).unwrap();
    let (first, second) = (host.join("first.mkv"), host.join("second.mkv"));
    generate_clip(&first, 0);
    generate_clip(&second, SECOND);
    let Some(daemon) = start_or_skip(&project_path) else {
        return;
    };
    let (mut client, snapshot, _) = Client::connect(daemon.address);
    assert_eq!(snapshot.media_list_states, 0);

    // Created from host paths: both files are imported into the bundle.
    let created = client
        .accept(CommandPayload::MediaList {
            input: list(),
            operation: MediaListOperation::Create {
                name: "Walk-in".into(),
                items: vec![
                    MediaListItemSpec {
                        id: 1,
                        uri: first.to_str().unwrap().into(),
                        title: None,
                        mark_in: 10,
                        mark_out: Some(20),
                    },
                    MediaListItemSpec {
                        id: 2,
                        uri: second.to_str().unwrap().into(),
                        title: None,
                        mark_in: 5,
                        mark_out: None,
                    },
                ],
                options: MediaListOptionsSpec {
                    auto_next: true,
                    ..MediaListOptionsSpec::default()
                },
            },
        })
        .unwrap();
    assert!(
        created
            .items
            .iter()
            .all(|item| item.uri.starts_with("asset://")),
        "{created:?}"
    );
    let assets = ProjectStore::new(&project_path).unwrap().assets_root();
    assert_eq!(std::fs::read_dir(&assets).unwrap().count(), 2);

    // On Program the list shows its first item cued at the in mark.
    client.accept(CommandPayload::SelectPreview { input: list() });
    client.accept(CommandPayload::Cut);
    client.wait_for("the first item's in mark on Program", |client, message| {
        (client.shown_frame(message)? == 10).then_some(())
    });

    // Playing runs the first item to its out mark, then the daemon advances
    // and the second file plays from its in mark.
    client.accept(CommandPayload::Media {
        input: list(),
        operation: MediaOperation::Play,
    });
    let mut shown = Vec::new();
    let mut advanced = None;
    client.wait_for("the second item on Program", |client, message| {
        if let WireMessage::Event(event) = &message
            && let EventPayload::MediaListChanged {
                operation: "advance",
                status,
                ..
            } = &event.payload
        {
            advanced = Some(status.active);
        }
        let frame = client.shown_frame(message)?;
        shown.push(frame);
        (frame >= SECOND + 8).then_some(())
    });
    assert_eq!(advanced, Some(2));
    let from_first: Vec<u64> = shown.iter().copied().filter(|f| *f < SECOND).collect();
    // The swap holds the first file's last picture (never black) until the
    // second file presents.
    assert!(
        from_first.iter().all(|frame| (10..20).contains(frame)),
        "the first item played outside its marks: {shown:?}"
    );
    // The monitor samples every few frames; the second file's first sample
    // is within one sampling interval of its in mark, never before it.
    let from_second: Vec<u64> = shown.iter().copied().filter(|f| *f >= SECOND).collect();
    assert!(
        from_second
            .first()
            .is_some_and(|frame| (SECOND + 5..=SECOND + 9).contains(frame)),
        "the second item did not start at its in mark: {shown:?}"
    );
    assert!(
        from_second.windows(2).all(|pair| pair[0] <= pair[1]),
        "{shown:?}"
    );
    drop(client);
    drop(daemon);

    // The journaled advance replays: the second item is active and cued.
    let Some(daemon) = start_or_skip(&project_path) else {
        return;
    };
    let (mut client, snapshot, states) = Client::connect(daemon.address);
    let _daemon = daemon;
    assert_eq!(snapshot.media_list_states, 1);
    assert_eq!(states[0].active, 2);
    client.wait_for("the second item cued on Program", |client, message| {
        (client.shown_frame(message)? == SECOND + 5).then_some(())
    });
}
