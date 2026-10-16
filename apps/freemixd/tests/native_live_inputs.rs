#![cfg(feature = "native-media")]

//! Process-level live input catalogue changes on the native runtime
//! (IN-003, IN-015, IN-026): a running native daemon takes a colour input
//! (`input_add`), a media file (a batch import of an `ffmpeg`-generated
//! clip), a scene (a duplicated Scene input), and a network stream (MPEG-TS
//! over loopback UDP from an `ffmpeg` sender) without a restart. Each one
//! is cut to Program and shows its picture there; the clip and the stream
//! are heard on Master. A source replacement shows on air at once, and
//! undo and redo of an add, a duplicate, and a replacement apply live too.
//! A file that does not decode is accepted and faults its input. Removing
//! the inputs takes their sound off Master while Program keeps running.
//!
//! The test needs `FFmpeg` and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket},
    num::NonZeroU128,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, Rgba8, Scene, SceneAnimation,
    SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, ImportItemSpec, InputLifecycleOperation, LiveInputSource, MediaOperation,
    MonitorFrameAssembler, MonitorTap, Role, RuntimeLifecycleEvent, WireGeneratorVideo,
    WireInputId, WireInputSource, WireMessage, decode_line, encode_line,
};
use fm_types::network_stream::{NetworkStreamConfig, NetworkStreamState};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);

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

/// A 20 s, 25 fps, 320x180 BT.709 red clip with a loud 440 Hz tone.
fn generate_clip(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=320x180:r=25:d=20,\
             setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:d=20",
        ])
        .args([
            "-c:v",
            "mpeg2video",
            "-g",
            "12",
            "-q:v",
            "2",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(["-color_primaries", "bt709", "-color_trc", "bt709"])
        .args(["-colorspace", "bt709"])
        .args(["-c:a", "pcm_f32le", "-f", "matroska", "-y"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "fixture generation failed: {status}");
}

/// Input 1 is a silent blue generator on Program; input 2 a Scene input
/// showing scene 1 (magenta background, no layers) on Preview.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(46).unwrap()),
        "Native live inputs",
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
    let scene = SceneId::new(NonZeroU128::new(1).unwrap());
    project.add_input(Input {
        id: input(2),
        name: "Scene".into(),
        kind: InputKind::Scene {
            scene_id: scene,
            audio_source: None,
        },
        required_capabilities: Vec::new(),
    });
    project.add_scene(Scene {
        id: scene,
        name: "Magenta".into(),
        background: Rgba8::new(220, 20, 200, 255),
        layers: Vec::new(),
        animation: SceneAnimation::default(),
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

/// A yellow test signal with a tone, sent as MPEG-TS over UDP; killed on
/// drop.
struct Sender(Child);

impl Sender {
    fn start(port: u16) -> Self {
        Self(
            Command::new("ffmpeg")
                .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-re"])
                .args(["-f", "lavfi", "-i", "color=c=yellow:s=320x180:r=25"])
                .args(["-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000"])
                .args(["-c:v", "mpeg2video", "-g", "10", "-b:v", "400k"])
                .args(["-c:a", "mp2", "-f", "mpegts"])
                .arg(format!("udp://127.0.0.1:{port}?pkt_size=1316"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct NativeDaemon {
    child: Child,
    address: SocketAddr,
    log: mpsc::Receiver<String>,
}

impl Drop for NativeDaemon {
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

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    assembler: MonitorFrameAssembler,
    commands: u64,
}

type Pixel = [u8; 4];

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
            build: "native-live-inputs-test".into(),
            client_type: ClientType::Studio,
            desired_role: Role::Admin,
            resume_cursor: None,
        }));
        assert!(matches!(
            client.receive(),
            WireMessage::HandshakeResponse(_)
        ));
        assert!(matches!(client.receive(), WireMessage::Snapshot(_)));
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

    /// The centre pixel of the Program monitor picture `message`
    /// completes, if any.
    fn program(&mut self, message: WireMessage) -> Option<Pixel> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        let image = self.assembler.push(&part).unwrap()?;
        if image.tap != MonitorTap::Program {
            return None;
        }
        let width = usize::from(image.width);
        let at = (usize::from(image.height) / 2 * width + width / 2) * 4;
        Some([
            image.rgba[at],
            image.rgba[at + 1],
            image.rgba[at + 2],
            image.rgba[at + 3],
        ])
    }

    /// Waits until Program shows a picture that satisfies `wanted`.
    fn shows(&mut self, what: &str, wanted: impl Fn(Pixel) -> bool) {
        self.wait_for(what, |client, message| {
            client.program(message).filter(|pixel| wanted(*pixel))
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

    fn submit(&mut self, payload: CommandPayload) -> CommandResult {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        self.wait_for("the command result", |_, message| match message {
            WireMessage::CommandResult(result) => Some(result),
            _ => None,
        })
    }

    fn accept(&mut self, payload: CommandPayload) {
        let described = format!("{payload:?}");
        let result = self.submit(payload);
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{described}: {result:?}"
        );
    }

    /// Puts `input` on Program with a cut through Preview.
    fn take(&mut self, input: u128) {
        self.accept(CommandPayload::SelectPreview { input: wire(input) });
        self.accept(CommandPayload::Cut);
    }
}

fn near(pixel: Pixel, rgb: [u8; 3]) -> bool {
    pixel
        .iter()
        .zip(rgb)
        .all(|(actual, expected)| actual.abs_diff(expected) <= 40)
}

fn blue(pixel: Pixel) -> bool {
    near(pixel, [20, 40, 220])
}

fn red(pixel: Pixel) -> bool {
    pixel[0] > 160 && pixel[1] < 90 && pixel[2] < 90
}

fn yellow(pixel: Pixel) -> bool {
    pixel[0] > 160 && pixel[1] > 160 && pixel[2] < 90
}

fn magenta(pixel: Pixel) -> bool {
    near(pixel, [220, 20, 200])
}

fn solid(red: u8, green: u8, blue: u8) -> LiveInputSource {
    LiveInputSource::Solid {
        red,
        green,
        blue,
        alpha: 255,
    }
}

const LOUD: u32 = 50_000;

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_adds_and_removes_inputs_while_it_runs() {
    if !tools_available() {
        eprintln!("native live input integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let clip = directory.path().join("clip.mkv");
    generate_clip(&clip);
    let broken = directory.path().join("broken.mp4");
    std::fs::write(&broken, b"not a media file at all").unwrap();
    let project_path = directory.path().join("native-live-inputs.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native live input integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.shows("the blue generator on Program", blue);
    client.master("silence on Master", |peak| peak == 0);

    // A colour input added live airs at once.
    client.accept(CommandPayload::AddInput {
        input: wire(10),
        name: "Green".into(),
        source: solid(30, 200, 60),
    });
    client.take(10);
    client.shows("the added colour on Program", |pixel| {
        near(pixel, [30, 200, 60])
    });

    // Replacing its source on air shows the new colour; undo restores it,
    // redo replaces it again.
    client.accept(CommandPayload::InputLifecycle(
        InputLifecycleOperation::ReplaceSource {
            input: wire(10),
            source: WireInputSource::Generator {
                video: WireGeneratorVideo::Solid {
                    rgba: [240, 240, 240, 255],
                },
                tone_hz: None,
            },
        },
    ));
    client.shows("the replaced source on Program", |pixel| {
        near(pixel, [240, 240, 240])
    });
    client.accept(CommandPayload::Undo {
        expected_edit: None,
    });
    client.shows("the undone replacement on Program", |pixel| {
        near(pixel, [30, 200, 60])
    });
    client.accept(CommandPayload::Redo {
        expected_edit: None,
    });
    client.shows("the redone replacement on Program", |pixel| {
        near(pixel, [240, 240, 240])
    });

    // A generator with a test tone is refused with the typed reason; the
    // native runtime does not realize the tone.
    let refused = client.submit(CommandPayload::InputLifecycle(
        InputLifecycleOperation::ReplaceSource {
            input: wire(10),
            source: WireInputSource::Generator {
                video: WireGeneratorVideo::Bars,
                tone_hz: Some(1_000),
            },
        },
    ));
    assert!(
        matches!(
            &refused,
            CommandResult::Rejected {
                reason: Some(fm_protocol::RefusalReason::RuntimeUnsupported),
                ..
            }
        ),
        "{refused:?}"
    );

    // A media file imported live opens off the loop, airs its picture,
    // and is heard on Master once it plays.
    client.accept(CommandPayload::InputLifecycle(
        InputLifecycleOperation::Import {
            items: vec![ImportItemSpec {
                input: wire(11),
                name: Some("Clip".into()),
                uri: clip.to_str().unwrap().to_owned(),
            }],
        },
    ));
    client.take(11);
    client.shows("the imported clip on Program", red);
    client.accept(CommandPayload::Media {
        input: wire(11),
        operation: MediaOperation::Play,
    });
    client.master("the clip's tone on Master", |peak| peak > LOUD);

    // A file that does not decode is accepted, and its input faults.
    client.accept(CommandPayload::InputLifecycle(
        InputLifecycleOperation::Import {
            items: vec![ImportItemSpec {
                input: wire(15),
                name: Some("Broken".into()),
                uri: broken.to_str().unwrap().to_owned(),
            }],
        },
    ));
    client.wait_for("the broken file's fault", |_, message| {
        let WireMessage::RuntimeEvent(event) = message else {
            return None;
        };
        let RuntimeLifecycleEvent::Media { status, .. } = event.event else {
            return None;
        };
        (status.input == wire(15) && status.transport.fault.is_some()).then_some(())
    });

    // A duplicated Scene input airs its scene; undo removes the duplicate
    // and redo brings it back.
    client.accept(CommandPayload::InputLifecycle(
        InputLifecycleOperation::Duplicate {
            source: wire(2),
            input: wire(12),
            name: None,
        },
    ));
    client.accept(CommandPayload::Undo {
        expected_edit: None,
    });
    let unknown = client.submit(CommandPayload::SelectPreview { input: wire(12) });
    assert!(
        matches!(unknown, CommandResult::Rejected { .. }),
        "{unknown:?}"
    );
    client.accept(CommandPayload::Redo {
        expected_edit: None,
    });
    client.take(12);
    client.shows("the duplicated scene on Program", magenta);

    // A network stream added live connects and airs, with its sound.
    let port = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = NetworkStreamConfig::new(format!("udp://127.0.0.1:{port}"));
    config.settings.jitter_buffer_ms = 200;
    config.settings.timeout_ms = 2_000;
    client.accept(CommandPayload::AddInput {
        input: wire(13),
        name: "Stream".into(),
        source: LiveInputSource::NetworkStream(Box::new(config)),
    });
    let sender = Sender::start(port);
    client.wait_for("the stream going live", |_, message| {
        let WireMessage::RuntimeEvent(event) = message else {
            return None;
        };
        let RuntimeLifecycleEvent::NetworkStreamStatus { input, status } = event.event else {
            return None;
        };
        (input == wire(13) && status.state == NetworkStreamState::Live).then_some(())
    });
    client.take(13);
    client.shows("the stream on Program", yellow);
    client.master("the stream's tone on Master", |peak| peak > LOUD);

    // Undo of an add removes the input live; redo adds it back.
    client.accept(CommandPayload::AddInput {
        input: wire(14),
        name: "Grey".into(),
        source: solid(128, 128, 128),
    });
    client.accept(CommandPayload::Undo {
        expected_edit: None,
    });
    let unknown = client.submit(CommandPayload::SelectPreview { input: wire(14) });
    assert!(
        matches!(unknown, CommandResult::Rejected { .. }),
        "{unknown:?}"
    );
    client.accept(CommandPayload::Redo {
        expected_edit: None,
    });
    client.take(14);
    client.shows("the redone add on Program", |pixel| {
        near(pixel, [128, 128, 128])
    });

    // Back on the blue generator, every added input is removed live: the
    // clip and the stream leave Master, and Program keeps running.
    client.take(1);
    client.accept(CommandPayload::SelectPreview { input: wire(2) });
    client.accept(CommandPayload::InputLifecycle(
        InputLifecycleOperation::RemoveInputs {
            inputs: vec![wire(10), wire(11), wire(12), wire(13), wire(14), wire(15)],
        },
    ));
    client.shows("the blue generator on Program", blue);
    client.master("silence on Master after removal", |peak| peak == 0);
    drop(sender);
    client.shows("Program still running", blue);
    drop(client);

    let logged = daemon.log.try_iter().collect::<Vec<_>>();
    for id in [10, 11, 12, 13, 14, 15] {
        assert!(
            logged
                .iter()
                .any(|line| line == &format!("FREEMIXD_INPUT\tv=1\tinput={id}\tretired")),
            "input {id} was retired: {logged:?}"
        );
    }
}
