#![cfg(feature = "native-media")]

//! Process-level native playlist automation: a native daemon's playlist
//! clock takes each scheduled entry to Program on exactly its frame, paced by
//! the native frame pacer; the journaled advances survive a SIGKILL so a
//! restarted native daemon resumes the playing playlist; and playlist
//! control (here Stop) is accepted on the native runtime. Idle frames the
//! pacer realizes after the last command survive a SIGKILL through the
//! journal's progress marker, so a restarted daemon resumes close to the
//! last frame it realized rather than where its last command ended.
//!
//! The test needs a native GPU adapter (Metal, Vulkan, or Direct3D 12; a
//! software Vulkan driver such as lavapipe suffices). Without one the daemon
//! refuses to start and the test is skipped, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    io::{BufRead, BufReader, Write},
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
    EventPayload, HandshakeRequest, MonitorFrameAssembler, MonitorTap, PlaylistEntrySpec,
    PlaylistLoopMode, PlaylistOperation, PlaylistRunState, PlaylistStart, PlaylistTransitionKind,
    Role, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Input 1's solid colour; input 2's is [`BLUE`].
const RED: [u8; 3] = [200, 30, 40];
const BLUE: [u8; 3] = [20, 60, 210];
/// Every entry's duration, in 25 fps frames.
const ENTRY_FRAMES: u64 = 25;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

fn solid(id: u128, name: &str, [red, green, blue]: [u8; 3]) -> Input {
    Input {
        id: input(id),
        name: name.into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(red, green, blue, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    }
}

/// A 320x180 project at 25 fps: red input 1 on Program, blue input 2 on
/// Preview.
fn save_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(42).unwrap()),
        "Native playlist",
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
    project.add_input(solid(1, "Red", RED));
    project.add_input(solid(2, "Blue", BLUE));
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
    /// The daemon's stderr lines, drained so it never blocks on the pipe.
    log: mpsc::Receiver<String>,
}

impl Drop for NativeDaemon {
    /// On a failure, reports whether the daemon exited by itself and
    /// replays its stderr (including any `FREEMIXD_SESSION` drop reason).
    fn drop(&mut self) {
        if thread::panicking() {
            thread::sleep(Duration::from_millis(500));
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

/// Starts `freemixd serve --native-media`, or explains why it did not
/// become ready. Stderr is drained so the daemon never blocks on it.
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
    let stderr = child.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
    });
    let (log_sender, log) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = log_sender.send(line);
        }
    });
    let line = receiver.recv_timeout(STARTUP_TIMEOUT).unwrap_or_default();
    if let Ok(readiness) = line.parse::<ReadinessRecord>() {
        return Ok(NativeDaemon {
            child,
            address: readiness.address,
            log,
        });
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut stderr = Vec::new();
    while let Ok(line) = log.recv_timeout(Duration::from_secs(5)) {
        stderr.push(line);
    }
    Err(format!(
        "native daemon did not become ready: {}",
        stderr.join("\n")
    ))
}

/// One Program monitor sample: the realized frame, its Program input, and
/// whether its centre pixel shows red, blue, or neither.
#[derive(Clone, Copy, Debug)]
struct Sample {
    frame: u64,
    input: WireInputId,
    color: Option<[u8; 3]>,
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    assembler: MonitorFrameAssembler,
    samples: Vec<Sample>,
}

impl Client {
    /// Connects, handshakes as an admin Studio, and returns the snapshot's
    /// playlist state when the snapshot announces one.
    fn connect(address: SocketAddr) -> (Self, Option<fm_protocol::PlaylistStateMessage>) {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            assembler: MonitorFrameAssembler::new(),
            samples: Vec::new(),
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-playlist-test".into(),
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
        let playlist = snapshot.playlist_follows.then(|| match client.receive() {
            WireMessage::PlaylistState(state) => state,
            other => panic!("expected playlist_state after the snapshot, got {other:?}"),
        });
        (client, playlist)
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    fn command(&mut self, id: &str, payload: CommandPayload) {
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.into(),
            idempotency_key: format!("{id}-key"),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
    }

    /// Receives one message, recording any completed Program monitor frame.
    fn receive(&mut self) -> WireMessage {
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed"
        );
        let message = decode_line(&line).unwrap();
        // The native daemon also sends a Preview tap; only Program is checked.
        if let WireMessage::MonitorFrame(part) = &message
            && part.tap == MonitorTap::Program
            && let Some(image) = self.assembler.push(part).unwrap()
        {
            let width = usize::from(image.width);
            let centre = (usize::from(image.height) / 2 * width + width / 2) * 4;
            let pixel = &image.rgba[centre..centre + 3];
            let color = [RED, BLUE].into_iter().find(|expected| {
                pixel
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.abs_diff(*expected) <= 2)
            });
            self.samples.push(Sample {
                frame: image.frame,
                input: image.input,
                color,
            });
        }
        message
    }

    /// The accepted revision of command `id`.
    fn accepted(&mut self, id: &str) -> u64 {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            if let WireMessage::CommandResult(result) = self.receive() {
                match result {
                    CommandResult::Accepted {
                        id: accepted,
                        revision,
                        ..
                    } if accepted == id => return revision,
                    other => panic!("expected {id} to be accepted, got {other:?}"),
                }
            }
        }
        panic!("no result for {id}");
    }

    /// The next `playlist_changed` event: its frame, run state, and Program.
    fn playlist_changed(&mut self) -> (u64, PlaylistRunState, WireInputId) {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            if let WireMessage::Event(event) = self.receive()
                && let EventPayload::PlaylistChanged {
                    frame,
                    state,
                    program,
                    ..
                } = event.payload
            {
                return (frame, state, program);
            }
        }
        panic!("timed out waiting for playlist_changed");
    }
}

impl Client {
    /// Receives until a Program monitor frame at or past `frame` arrives,
    /// returning its frame.
    fn monitor_frame_at_least(&mut self, frame: u64) -> u64 {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            self.receive();
            if let Some(sample) = self.samples.last()
                && sample.frame >= frame
            {
                return sample.frame;
            }
        }
        panic!("no monitor frame reached {frame}");
    }
}

fn playing(entry: u64, started_frame: u64) -> impl Fn(&PlaylistRunState) -> bool {
    move |state| {
        matches!(state, PlaylistRunState::Playing { entry: on, started_frame: at, .. }
            if *on == entry && *at == started_frame)
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_playlist_clock_cuts_program_on_scheduled_frames_and_survives_sigkill() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-playlist.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native playlist integration skipped: {error}");
            return;
        }
    };

    let (mut client, playlist) = Client::connect(daemon.address);
    assert!(playlist.is_none(), "an empty, stopped playlist is not sent");
    let entries = vec![
        PlaylistEntrySpec {
            id: 1,
            input: wire(2),
            duration_frames: ENTRY_FRAMES,
            transition: PlaylistTransitionKind::Cut,
            transition_frames: 0,
            enabled: true,
        },
        PlaylistEntrySpec {
            id: 2,
            input: wire(1),
            duration_frames: ENTRY_FRAMES,
            transition: PlaylistTransitionKind::Cut,
            transition_frames: 0,
            enabled: true,
        },
    ];
    client.command(
        "set",
        CommandPayload::SetPlaylist {
            loop_mode: PlaylistLoopMode::All,
            entries,
        },
    );
    assert_eq!(client.accepted("set"), 1);
    assert_eq!(client.playlist_changed().1, PlaylistRunState::Stopped);

    // Playlist control is accepted on the native runtime.
    client.command(
        "start",
        CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Start {
                at: PlaylistStart::AfterFrames(ENTRY_FRAMES),
                from: None,
            },
        },
    );
    assert_eq!(client.accepted("start"), 2);
    let (_, armed, _) = client.playlist_changed();
    let PlaylistRunState::Armed { start_frame, .. } = armed else {
        panic!("expected the armed start, got {armed:?}");
    };

    // The native clock takes each entry on air on exactly its frame.
    let (frame, state, program) = client.playlist_changed();
    assert_eq!((frame, program), (start_frame, wire(2)));
    assert!(playing(1, start_frame)(&state), "{state:?}");
    let second = start_frame + ENTRY_FRAMES;
    let (frame, state, program) = client.playlist_changed();
    assert_eq!((frame, program), (second, wire(1)));
    assert!(playing(2, second)(&state), "{state:?}");
    let third = second + ENTRY_FRAMES;
    let (frame, state, program) = client.playlist_changed();
    assert_eq!((frame, program), (third, wire(2)));
    assert!(playing(1, third)(&state), "{state:?}");

    // Every Program monitor frame the native renderer produced shows the
    // input the playlist had on air at that frame.
    let expected = |frame: u64| {
        if frame < start_frame || (second..third).contains(&frame) {
            (wire(1), RED)
        } else {
            (wire(2), BLUE)
        }
    };
    let samples = std::mem::take(&mut client.samples);
    assert!(
        samples.iter().any(|sample| sample.frame < start_frame)
            && samples
                .iter()
                .any(|sample| (start_frame..second).contains(&sample.frame))
            && samples
                .iter()
                .any(|sample| (second..third).contains(&sample.frame)),
        "monitor frames cover every entry: {samples:?}"
    );
    for sample in &samples {
        let (input, color) = expected(sample.frame);
        assert_eq!(
            (sample.input, sample.color),
            (input, Some(color)),
            "Program at frame {} (entries start at {start_frame}, {second}, {third})",
            sample.frame
        );
    }

    // SIGKILL mid-entry: the advances are journaled, so the restarted
    // native daemon replays them and resumes the playing playlist.
    drop(client);
    drop(daemon);
    let daemon = start_native(&project_path).expect("the native daemon restarts after SIGKILL");
    let (mut client, playlist) = Client::connect(daemon.address);
    let playlist = playlist.expect("a playing playlist is sent after the snapshot");
    assert_eq!(playlist.revision, 5);
    assert!(playing(1, third)(&playlist.state), "{playlist:?}");
    assert!(playlist.frame > third, "{playlist:?}");
    let fourth = third + ENTRY_FRAMES;
    let (frame, state, program) = client.playlist_changed();
    assert_eq!((frame, program), (fourth, wire(1)));
    assert!(playing(2, fourth)(&state), "{state:?}");

    // Stop is accepted on the native runtime and holds Program.
    client.command(
        "stop",
        CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Stop,
        },
    );
    assert_eq!(client.accepted("stop"), 7);
    let (_, state, program) = client.playlist_changed();
    assert_eq!((state, program), (PlaylistRunState::Stopped, wire(1)));

    // A client may never issue the clock's advance.
    client.command(
        "client-advance",
        CommandPayload::PlaylistAdvance { frame: fourth + 1 },
    );
    loop {
        if let WireMessage::CommandResult(result) = client.receive() {
            assert!(
                matches!(&result, CommandResult::Rejected { code, .. } if code == "unavailable"),
                "{result:?}"
            );
            break;
        }
    }
    drop(client);
    drop(daemon);
}

/// Frames between two progress markers at the test's 25 fps (one second).
const MARKER_FRAMES: u64 = 25;

#[test]
fn native_idle_progress_survives_sigkill_through_the_progress_marker() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-idle.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native idle progress integration skipped: {error}");
            return;
        }
    };

    let (mut client, _) = Client::connect(daemon.address);
    client.command(
        "set",
        CommandPayload::SetPlaylist {
            loop_mode: PlaylistLoopMode::Off,
            entries: vec![PlaylistEntrySpec {
                id: 1,
                input: wire(2),
                duration_frames: 100_000,
                transition: PlaylistTransitionKind::Cut,
                transition_frames: 0,
                enabled: true,
            }],
        },
    );
    assert_eq!(client.accepted("set"), 1);
    client.command(
        "start",
        CommandPayload::PlaylistControl {
            operation: PlaylistOperation::Start {
                at: PlaylistStart::AfterFrames(5),
                from: None,
            },
        },
    );
    assert_eq!(client.accepted("start"), 2);
    let (frame, state, program) = loop {
        let changed = client.playlist_changed();
        if matches!(changed.1, PlaylistRunState::Playing { .. }) {
            break changed;
        }
    };
    assert_eq!(program, wire(2));
    assert!(playing(1, frame)(&state), "{state:?}");
    let started = frame;

    // Four seconds of idle frames after the last journaled command (the
    // clock's advance), then SIGKILL.
    let reached = client.monitor_frame_at_least(started + 4 * MARKER_FRAMES);
    drop(client);
    drop(daemon);

    let daemon = start_native(&project_path).expect("the native daemon restarts after SIGKILL");
    let (_client, playlist) = Client::connect(daemon.address);
    let playlist = playlist.expect("a playing playlist is sent after the snapshot");
    assert_eq!(playlist.revision, 3, "no command ran after the advance");
    assert!(playing(1, started)(&playlist.state), "{playlist:?}");
    // The restart resumes at or after the newest marker, which is at most
    // one marker interval (plus the frame in flight) behind the last frame
    // the killed daemon realized; without markers it would resume right
    // after the advance at `started`.
    assert!(
        playlist.frame + MARKER_FRAMES + 1 >= reached,
        "resumed at {} after realizing {reached} (entry started at {started})",
        playlist.frame
    );
    assert!(playlist.frame > started + 2 * MARKER_FRAMES, "{playlist:?}");
}
