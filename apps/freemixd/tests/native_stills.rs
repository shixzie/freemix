#![cfg(feature = "native-media")]

//! Process-level IN-008 and IN-009 on the native runtime: an image input in
//! the saved project starts placed with its fit, a still media file larger
//! than the show starts fitted (it used to stop startup), and an image and
//! a slideshow added live air on Program; stepping the slideshow shows its
//! next slide, composed on the GPU from slides uploaded once.
//!
//! The test needs a native GPU adapter (a software Vulkan driver such as
//! lavapipe suffices). Without one it is skipped, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

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
    Input, InputKind, MainMix, MediaPlaybackSettings, Project, ProjectSettings, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, ImageInputConfig, LiveInputSource, MonitorFrameAssembler, MonitorTap,
    PagedNavigation, Role, SlideTransitionStyle, SlideshowConfig, SlideshowItem, SlideshowSource,
    WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputScaling, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
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

fn png(path: &Path, width: u32, height: u32, rgba: [u8; 4]) {
    image::RgbaImage::from_pixel(width, height, image::Rgba(rgba))
        .save(path)
        .unwrap();
}

/// Input 1 a blue generator on Program; input 2 a 4:1 red image placed
/// with Fit on Preview; input 3 a 640x360 green still media file, twice
/// the show's size; 25 fps, 320x180.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(47).unwrap()),
        "Native stills",
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
    for (id, name, kind) in [
        (
            1,
            "Blue",
            InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(20, 40, 220, 255)),
                SimulatedAudio::Silence,
            )),
        ),
        (
            2,
            "Banner",
            InputKind::Image(ImageInputConfig::new(
                "asset://banner.png",
                OutputScaling::Fit,
            )),
        ),
        (
            3,
            "Large still",
            InputKind::Media {
                asset_uri: "asset://large.png".into(),
                playback: MediaPlaybackSettings::default(),
            },
        ),
    ] {
        project.add_input(Input {
            id: input(id),
            name: name.into(),
            kind,
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
    store.save(&stored).unwrap();
    let assets = store.assets_root();
    std::fs::create_dir_all(&assets).unwrap();
    png(&assets.join("banner.png"), 64, 16, [230, 20, 20, 255]);
    png(&assets.join("large.png"), 640, 360, [20, 210, 40, 255]);
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

type Pixel = [u8; 4];

/// The Program picture's pixels at the centre and near the top centre.
#[derive(Clone, Copy, Debug)]
struct Program {
    centre: Pixel,
    top: Pixel,
}

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
            build: "native-stills-test".into(),
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

    fn program(&mut self, message: WireMessage) -> Option<Program> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        let image = self.assembler.push(&part).unwrap()?;
        if image.tap != MonitorTap::Program {
            return None;
        }
        let width = usize::from(image.width);
        let pixel = |row: usize| {
            let at = (row * width + width / 2) * 4;
            [
                image.rgba[at],
                image.rgba[at + 1],
                image.rgba[at + 2],
                image.rgba[at + 3],
            ]
        };
        let height = usize::from(image.height);
        Some(Program {
            centre: pixel(height / 2),
            top: pixel(height / 10),
        })
    }

    fn shows(&mut self, what: &str, wanted: impl Fn(Program) -> bool) {
        self.wait_for(what, |client, message| {
            client.program(message).filter(|program| wanted(*program))
        });
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
        let result = self.wait_for("the command result", |_, message| match message {
            WireMessage::CommandResult(result) => Some(result),
            _ => None,
        });
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{described}: {result:?}"
        );
    }

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

fn black(pixel: Pixel) -> bool {
    pixel[..3].iter().all(|channel| *channel < 30)
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_shows_images_and_steps_slideshows() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-stills.freemix");
    save_project(&project_path);
    let host = directory.path().join("host");
    std::fs::create_dir(&host).unwrap();
    png(&host.join("yellow.png"), 32, 32, [230, 220, 20, 255]);
    png(&host.join("magenta.png"), 48, 27, [220, 20, 200, 255]);
    png(&host.join("cyan.png"), 48, 27, [20, 210, 220, 255]);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native still integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.shows("the blue generator on Program", |program| {
        near(program.centre, [20, 40, 220])
    });

    // The 4:1 banner fits the 16:9 show: red across the middle, empty
    // (black on Program) above and below.
    client.take(2);
    client.shows("the fitted banner on Program", |program| {
        near(program.centre, [230, 20, 20]) && black(program.top)
    });

    // The still twice the show's size is fitted, not refused.
    client.take(3);
    client.shows("the large still on Program", |program| {
        near(program.centre, [20, 210, 40]) && near(program.top, [20, 210, 40])
    });

    // A square image added live with Fill covers the frame.
    client.accept(CommandPayload::AddInput {
        input: wire(4),
        name: "Yellow".into(),
        source: LiveInputSource::Image(Box::new(ImageInputConfig::new(
            host.join("yellow.png").to_string_lossy(),
            OutputScaling::Fill,
        ))),
    });
    client.take(4);
    client.shows("the added image on Program", |program| {
        near(program.centre, [230, 220, 20]) && near(program.top, [230, 220, 20])
    });

    // A slideshow added live shows its first slide, and Next its second.
    let mut slideshow = SlideshowConfig::new(SlideshowSource::List(vec![
        SlideshowItem::new(host.join("magenta.png").to_string_lossy()),
        SlideshowItem::new(host.join("cyan.png").to_string_lossy()),
    ]));
    slideshow.transition = SlideTransitionStyle::Cut;
    slideshow.auto_advance = false;
    client.accept(CommandPayload::AddInput {
        input: wire(5),
        name: "Slides".into(),
        source: LiveInputSource::Slideshow(Box::new(slideshow)),
    });
    client.take(5);
    client.shows("the first slide on Program", |program| {
        near(program.centre, [220, 20, 200])
    });
    client.accept(CommandPayload::PageNavigate {
        input: wire(5),
        navigation: PagedNavigation::Next,
    });
    client.shows("the second slide on Program", |program| {
        near(program.centre, [20, 210, 220])
    });
    client.accept(CommandPayload::PageNavigate {
        input: wire(5),
        navigation: PagedNavigation::Previous,
    });
    client.shows("the first slide again", |program| {
        near(program.centre, [220, 20, 200])
    });
}
