#![cfg(feature = "native-media")]

//! Process-level audio-only media files on the native runtime (IN-005,
//! IN-003): a WAV music bed in the project and an MP3 sting imported live
//! (a batch import of a host path, as Studio's import sends it) play through
//! the engine's transport like any clip. Each reports its synthesized
//! source (frames at the program rate from the file's duration, one audio
//! track), shows as black on Program on its own (its picture is transparent
//! black), and is heard on Master only while it plays; the bed loops and
//! the sting ends.
//!
//! The test needs `FFmpeg` and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    fs,
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
    Input, InputKind, MainMix, MediaPlaybackSettings, Project, ProjectSettings, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, ImportItemSpec, InputLifecycleOperation, MediaEndAction, MediaEvent,
    MediaOperation, MediaSeekTarget, MediaTransportStatus, MonitorFrameAssembler, MonitorTap, Role,
    RuntimeLifecycleEvent, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
const BED: &str = "bed.wav";
/// A Master peak above this (millionths of full scale) is the tone.
const LOUD: u32 = 50_000;

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

/// `seconds` of a 440 Hz 48 kHz mono tone (amplitude 1/8) at `path`,
/// encoded as its extension says; no video stream.
fn generate_tone(path: &Path, seconds: u32) {
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:sample_rate=48000:d={seconds}"),
        ])
        .arg("-y")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "fixture generation failed: {status}");
}

/// Input 1 is a silent blue generator on Program; input 2 a 4 s WAV bed on
/// Preview.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    fs::create_dir_all(store.assets_root()).unwrap();
    generate_tone(&store.assets_root().join(BED), 4);
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(47).unwrap()),
        "Native audio-only media",
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
    project.add_input(Input {
        id: input(1),
        name: "Blue".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(20, 40, 220, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input(2),
        name: "Bed".into(),
        kind: InputKind::Media {
            asset_uri: format!("asset://{BED}"),
            playback: MediaPlaybackSettings::default(),
        },
        required_capabilities: Vec::new(),
    });
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
    store.save(&stored).unwrap();
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

type Pixel = [u8; 4];

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    assembler: MonitorFrameAssembler,
    commands: u64,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            assembler: MonitorFrameAssembler::new(),
            commands: 0,
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-audio-only-media-test".into(),
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
        assert_eq!(snapshot.media_states, 1);
        client
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    fn receive(&mut self) -> WireMessage {
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed"
        );
        decode_line(&line).unwrap()
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

    /// The input on Program and the centre pixel of the Program monitor
    /// picture `message` completes, if any.
    fn program(&mut self, message: WireMessage) -> Option<(WireInputId, Pixel)> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        let image = self.assembler.push(&part).unwrap()?;
        if image.tap != MonitorTap::Program {
            return None;
        }
        let width = usize::from(image.width);
        let at = (usize::from(image.height) / 2 * width + width / 2) * 4;
        Some((
            image.input,
            [
                image.rgba[at],
                image.rgba[at + 1],
                image.rgba[at + 2],
                image.rgba[at + 3],
            ],
        ))
    }

    /// Waits until Program shows `input` with a black centre.
    fn shows_black(&mut self, what: &str, input: u128) {
        self.wait_for(what, |client, message| {
            let (shown, pixel) = client.program(message)?;
            (shown == wire(input) && pixel[..3].iter().all(|channel| *channel < 8)).then_some(())
        });
    }

    /// Waits until the Master meters satisfy `wanted` (peak, millionths).
    fn master(&mut self, what: &str, wanted: impl Fn(u32) -> bool) {
        self.wait_for(what, |_, message| {
            let WireMessage::AudioMeters(meters) = message else {
                return None;
            };
            let peak = meters
                .master
                .iter()
                .map(|channel| channel.peak_millionths)
                .max()
                .unwrap_or(0);
            wanted(peak).then_some(())
        });
    }

    /// The next `media` runtime event of `input` that `wanted` accepts.
    fn media_event(
        &mut self,
        what: &str,
        input: u128,
        wanted: impl Fn(&MediaTransportStatus, &[MediaEvent]) -> bool,
    ) -> MediaTransportStatus {
        self.wait_for(what, |_, message| {
            let status = match message {
                WireMessage::RuntimeEvent(event) => match event.event {
                    RuntimeLifecycleEvent::Media { status, events, .. } => {
                        (status.input == wire(input) && wanted(&status.transport, &events))
                            .then_some(*status)
                    }
                    _ => None,
                },
                WireMessage::MediaState(state) => (state.status.input == wire(input)
                    && wanted(&state.status.transport, &[]))
                .then_some(state.status),
                _ => None,
            }?;
            Some(status.transport)
        })
    }

    fn accept(&mut self, payload: CommandPayload) {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        let described = format!("{payload:?}");
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        self.wait_for("the command result", |_, message| match message {
            WireMessage::CommandResult(result) => {
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{described}: {result:?}"
                );
                Some(())
            }
            _ => None,
        });
    }

    fn media(&mut self, input: u128, operation: MediaOperation) {
        self.accept(CommandPayload::Media {
            input: wire(input),
            operation,
        });
    }

    /// Puts `input` on Program with a cut through Preview.
    fn take(&mut self, input: u128) {
        self.accept(CommandPayload::SelectPreview { input: wire(input) });
        self.accept(CommandPayload::Cut);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_plays_audio_only_media_files() {
    if !tools_available() {
        eprintln!("native audio-only media integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let sting = directory.path().join("sting.mp3");
    generate_tone(&sting, 1);
    let project_path = directory.path().join("native-audio-only-media.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native audio-only media integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);

    // The bed's source: 4 s at the program rate, one audio track, cued and
    // stopped, with no fault.
    let bed = client.media_event("the bed's media state", 2, |transport, _| {
        transport.available
    });
    assert_eq!((bed.position, bed.frames), (0, 100), "{bed:?}");
    assert!(bed.complete);
    assert_eq!(bed.source_rate, Some(FrameRate::new(25, 1).unwrap()));
    assert_eq!(bed.fault, None);
    let tracks = bed
        .tracks
        .iter()
        .map(|track| track.stream_index)
        .collect::<Vec<_>>();
    assert_eq!(tracks, [0]);
    client.master("silence on Master", |peak| peak == 0);

    // On Program on its own the bed is black; stopped, it is silent.
    client.take(2);
    client.shows_black("the bed on Program", 2);
    client.master("silence while the bed is stopped", |peak| peak == 0);

    // Playing it puts the tone on Master through its strip.
    client.media(2, MediaOperation::Play);
    client.master("the bed's tone on Master", |peak| peak > LOUD);

    // Looping at the end wraps it to the start and keeps it on Master.
    client.media(2, MediaOperation::EndAction(MediaEndAction::Loop));
    client.media(2, MediaOperation::Seek(MediaSeekTarget::Frame(95)));
    client.media_event("the bed looping", 2, |_, events| {
        events.contains(&MediaEvent::Looped { count: 1 })
    });
    client.master("the looped bed on Master", |peak| peak > LOUD);
    client.shows_black("the looped bed still black on Program", 2);

    // Stopping it silences Master.
    client.media(2, MediaOperation::Stop);
    client.master("silence after the bed stops", |peak| peak == 0);

    // An MP3 sting imported live from a host path (as Studio imports) opens
    // off the loop with its synthesized source.
    client.accept(CommandPayload::InputLifecycle(
        InputLifecycleOperation::Import {
            items: vec![ImportItemSpec {
                input: wire(3),
                name: None,
                uri: sting.to_str().unwrap().to_owned(),
            }],
        },
    ));
    let opened = client.media_event("the sting's source", 3, |transport, _| {
        transport.available || transport.fault.is_some()
    });
    assert_eq!(opened.fault, None, "{opened:?}");
    // One second; MP3 adds a little encoder padding.
    assert!((25..=27).contains(&opened.frames), "{opened:?}");
    assert_eq!(opened.tracks.len(), 1);

    // Played on Program it is heard, then ends on its own and falls silent.
    client.take(3);
    client.shows_black("the sting on Program", 3);
    client.media(3, MediaOperation::Play);
    client.master("the sting on Master", |peak| peak > LOUD);
    client.media_event("the sting ending", 3, |_, events| {
        events
            .iter()
            .any(|event| matches!(event, MediaEvent::Ended { .. }))
    });
    client.master("silence after the sting", |peak| peak == 0);
    drop(client);
    drop(daemon);
}
