#![cfg(feature = "native-media")]

//! Process-level native document realization (IN-014): a native daemon
//! renders a stored PDF's current page as a live source on Program, turns
//! the page after a `paged_input` step, and adds a document input live.
//!
//! The test needs a native GPU adapter (Metal, Vulkan, or Direct3D 12; a
//! software Vulkan driver such as lavapipe suffices). Without one the daemon
//! refuses to start and the test is skipped, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

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
    DocumentInput, Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_presentations::sample::{SolidPage, solid_pages_pdf};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, ImportItemSpec, InputLifecycleOperation, MonitorFrameAssembler, MonitorImage,
    MonitorTap, PagedNavigation, Role, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const FRAME_TIMEOUT: Duration = Duration::from_secs(20);

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

/// A 320x180 project at 25 fps: input 1 a solid generator on Preview and
/// input 2 a document on Program showing page 1 of a 16:9 PDF whose pages
/// are red, then blue.
fn save_document_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let page = |rgb| SolidPage {
        width: 320,
        height: 180,
        rgb,
    };
    let deck = store.assets_root().join("deck.pdf");
    fs::create_dir_all(deck.parent().unwrap()).unwrap();
    fs::write(
        &deck,
        solid_pages_pdf(Some("Deck"), &[page([255, 0, 0]), page([0, 0, 255])]),
    )
    .unwrap();

    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(46).unwrap()),
        "Native documents",
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
        name: "Solid".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(12, 34, 56, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input(2),
        name: "Deck".into(),
        kind: InputKind::Document(DocumentInput {
            asset_uri: "asset://deck.pdf".into(),
            page: 0,
        }),
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(input(2), input(1)));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(2)),
            realized_program_id: Some(input(2)),
            desired_preview_id: Some(input(1)),
            realized_preview_id: Some(input(1)),
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
        .env("FREEMIX_SOFFICE", "")
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
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        }
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

    fn command(&mut self, id: &str, payload: CommandPayload) {
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.into(),
            idempotency_key: format!("{id}-key"),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let deadline = Instant::now() + FRAME_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "no result for {id}");
            if let WireMessage::CommandResult(result) = self.receive() {
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{id}: {result:?}"
                );
                return;
            }
        }
    }
}

/// Whether every pixel of `image` is within two steps of `color`.
fn filled_with(image: &MonitorImage, color: [u8; 3]) -> bool {
    image.rgba.as_chunks::<4>().0.iter().all(|pixel| {
        pixel[..3]
            .iter()
            .zip(color)
            .all(|(actual, expected)| actual.abs_diff(expected) <= 2)
    })
}

fn program_frame_filled(
    client: &mut Client,
    assembler: &mut MonitorFrameAssembler,
    color: [u8; 3],
    expected: &str,
) -> MonitorImage {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    while Instant::now() < deadline {
        let WireMessage::MonitorFrame(part) = client.receive() else {
            continue;
        };
        assert_eq!(part.tap, MonitorTap::Program);
        if let Some(image) = assembler.push(&part).unwrap()
            && filled_with(&image, color)
        {
            return image;
        }
    }
    panic!("timed out waiting for {expected}");
}

#[test]
fn native_daemon_shows_a_document_page_and_turns_it() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-document.freemix");
    save_document_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native document integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-document-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Admin,
        resume_cursor: None,
    }));
    assert!(matches!(
        client.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    let mut assembler = MonitorFrameAssembler::new();
    let first = program_frame_filled(&mut client, &mut assembler, [255, 0, 0], "page 1");
    assert_eq!(first.input, WireInputId::from_domain(input(2)));

    client.command(
        "next",
        CommandPayload::PagedInput {
            input: WireInputId::from_domain(input(2)),
            navigation: PagedNavigation::Next,
        },
    );
    program_frame_filled(&mut client, &mut assembler, [0, 0, 255], "page 2");

    // A document input added live joins the native catalogue.
    client.command(
        "import",
        CommandPayload::InputLifecycle(InputLifecycleOperation::Import {
            items: vec![ImportItemSpec {
                input: WireInputId::from_domain(input(3)),
                name: Some("Deck copy".into()),
                uri: "asset://deck.pdf".into(),
            }],
        }),
    );
    client.command(
        "cut-to-copy",
        CommandPayload::SelectPreview {
            input: WireInputId::from_domain(input(3)),
        },
    );
    client.command("cut", CommandPayload::Cut);
    let copy = program_frame_filled(&mut client, &mut assembler, [255, 0, 0], "the copy");
    assert_eq!(copy.input, WireInputId::from_domain(input(3)));
}
