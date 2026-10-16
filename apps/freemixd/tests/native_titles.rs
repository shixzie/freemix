#![cfg(feature = "native-media")]

//! Process-level native title realization: a native daemon renders a stored
//! title template as a live source, publishes it in the Program monitor
//! frame, and re-renders it after a `SetTitleFields` command.
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
    Input, InputKind, MainMix, Project, ProjectSettings, Rgba8, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor, TitleFieldEntry, TitleFieldId, TitleFieldValue, TitleInput,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, MonitorFrameAssembler, MonitorImage, MonitorTap, Role, TitleAirRequirement,
    TitleFieldUpdate, WireInputId, WireMessage, WireTitleFieldId, decode_line, encode_line,
};
use fm_titles::{
    Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldValue, Style,
    TemplateId, TitleTemplate,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const TEMPLATE_KEY: &str = "titles/native-title.fmtitle";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const FRAME_TIMEOUT: Duration = Duration::from_secs(20);

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn color_fields(bar: [u8; 3]) -> Vec<TitleFieldEntry> {
    vec![TitleFieldEntry {
        id: TitleFieldId::new(NonZeroU128::MIN),
        value: TitleFieldValue::Color(Rgba8::new(bar[0], bar[1], bar[2], 255)),
    }]
}

/// A 320x180 project at 25 fps: input 1 is a silent solid generator on
/// Preview and input 2 a title on Program whose template draws one
/// rectangle at (80, 40) sized 160x60, tinted by color field 1, on a
/// transparent background.
fn save_title_project(path: &Path) {
    let field = FieldId::new(NonZeroU128::MIN);
    let template = TitleTemplate {
        id: TemplateId::new(NonZeroU128::MIN),
        name: "Native title".into(),
        width: 320,
        height: 180,
        background: Color::TRANSPARENT,
        fields: vec![FieldDefinition {
            id: field,
            name: "Bar".into(),
            default: FieldValue::Color(Color::new(1, 2, 3, 255)),
        }],
        elements: vec![Element {
            id: ElementId::new(NonZeroU128::MIN),
            name: "Bar".into(),
            bounds: Bounds {
                x: 80,
                y: 40,
                width: 160,
                height: 60,
            },
            z_index: 0,
            visible: true,
            style: Style::default(),
            color_field: Some(field),
            kind: ElementKind::Rectangle,
        }],
        animation: fm_titles::TitleAnimation::default(),
    };
    let store = ProjectStore::new(path).unwrap();
    let document = fm_persistence::encode_title_template(&fm_persistence::StoredTitleTemplate {
        template,
        fonts: Vec::new(),
        clocks: Vec::new(),
    })
    .unwrap();
    let template_path = store.assets_root().join(TEMPLATE_KEY);
    fs::create_dir_all(template_path.parent().unwrap()).unwrap();
    fs::write(&template_path, document).unwrap();

    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(42).unwrap()),
        "Native titles",
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
        name: "Lower third".into(),
        kind: InputKind::Title(TitleInput {
            template_uri: format!("asset://{TEMPLATE_KEY}"),
            field_revision: 0,
            fields: color_fields([230, 20, 140]),
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
/// become ready.
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
}

/// Whether every pixel of `image` is within one step of `inside` on the
/// `x0..x1` x `y0..y1` rectangle and of `outside` everywhere else.
fn shows_bar(
    image: &MonitorImage,
    (x0, y0, x1, y1): (usize, usize, usize, usize),
    inside: [u8; 3],
    outside: [u8; 3],
) -> bool {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .all(|(index, pixel)| {
            let x = index % usize::from(image.width);
            let y = index / usize::from(image.width);
            let expected = if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                inside
            } else {
                outside
            };
            pixel[..3]
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.abs_diff(expected) <= 1)
        })
}

fn program_frame_showing(
    client: &mut Client,
    assembler: &mut MonitorFrameAssembler,
    bar: [u8; 3],
    expected: &str,
) -> MonitorImage {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    while Instant::now() < deadline {
        let WireMessage::MonitorFrame(part) = client.receive() else {
            continue;
        };
        assert_eq!(part.tap, MonitorTap::Program);
        if let Some(image) = assembler.push(&part).unwrap()
            && shows_bar(&image, (80, 40, 240, 100), bar, [0, 0, 0])
        {
            return image;
        }
    }
    panic!("timed out waiting for {expected}");
}

/// Adds to the title project scene 1 (Scene input 3: the solid generator
/// under the title, both full canvas) and scene 2 (Scene input 4: scene 1
/// nested full canvas).
fn add_title_scenes(path: &Path) {
    use fm_model::{Layer, LayerGeometry, LayerId, Rotation, Scene, SceneAnimation, SourceRef};
    use fm_types::SceneId;

    let scene = |value: u128| SceneId::new(NonZeroU128::new(value).unwrap());
    let layer = |id: u32, source: SourceRef| Layer {
        id: LayerId::new(id).unwrap(),
        name: format!("Layer {id}"),
        source,
        enabled: true,
        geometry: LayerGeometry::new(0, 0, 320, 180, Rotation::Deg0),
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: u8::MAX,
        z_order: i32::try_from(id).unwrap(),
    };
    let store = ProjectStore::new(path).unwrap();
    let stored = store.load().unwrap();
    let mut project = stored.project().clone();
    for (id, input_id, layers) in [
        (
            1,
            3,
            vec![
                layer(1, SourceRef::Input(input(1))),
                layer(2, SourceRef::Input(input(2))),
            ],
        ),
        (2, 4, vec![layer(1, SourceRef::Scene(scene(1)))]),
    ] {
        project.add_scene(Scene {
            id: scene(id),
            name: format!("Scene {id}"),
            background: Rgba8::OPAQUE_BLACK,
            layers,
            animation: SceneAnimation::default(),
        });
        project.add_input(Input {
            id: input(input_id),
            name: format!("Scene {id}"),
            kind: InputKind::Scene {
                scene_id: scene(id),
                audio_source: None,
            },
            required_capabilities: Vec::new(),
        });
    }
    let routing = RuntimeRouting {
        desired_program_id: Some(input(2)),
        realized_program_id: Some(input(2)),
        desired_preview_id: Some(input(1)),
        realized_preview_id: Some(input(1)),
    };
    store
        .save(
            &StoredProject::from_project(project, routing, stored.position(), Vec::new()).unwrap(),
        )
        .unwrap();
}

/// Requests a 64x36 Hold preview of Scene input `scene` and assembles the
/// `scene_preview_frame` parts that answer it, skipping everything else.
fn scene_preview(client: &mut Client, scene: u128) -> fm_protocol::ScenePreviewImage {
    let request_id = format!("scene-{scene}");
    client.send(&WireMessage::ScenePreviewRequest(
        fm_protocol::ScenePreviewRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: request_id.clone(),
            input: WireInputId::from_domain(input(scene)),
            sample: fm_protocol::TitlePreviewSample::Hold,
            width: 64,
            height: 36,
            clip: None,
        },
    ));
    let mut assembler = fm_protocol::TitlePreviewAssembler::new();
    let deadline = Instant::now() + FRAME_TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "no preview of scene {scene}");
        match client.receive() {
            WireMessage::ScenePreviewFrame(part) => {
                assert_eq!(part.request_id, request_id);
                if let Some(image) = assembler.push(&part).unwrap() {
                    return image;
                }
            }
            WireMessage::Error(error) if error.request_id.as_deref() == Some(&request_id) => {
                panic!("scene {scene} preview refused: {error:?}");
            }
            _ => {}
        }
    }
}

/// GX-011: a native daemon's exact scene preview composes the newest frames
/// its media pipeline holds: here a live title over a generator, directly
/// and inside a nested scene.
#[test]
fn native_daemon_previews_scenes_from_live_source_frames() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-title-scenes.freemix");
    save_title_project(&project_path);
    add_title_scenes(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native scene preview integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-scene-preview-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Admin,
        resume_cursor: None,
    }));
    assert!(matches!(
        client.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));
    // The title is realized once the Program monitor shows it.
    let mut assembler = MonitorFrameAssembler::new();
    program_frame_showing(
        &mut client,
        &mut assembler,
        [230, 20, 140],
        "the title on Program",
    );

    let direct = scene_preview(&mut client, 3);
    assert_eq!((direct.width, direct.height), (64, 36));
    let at = |x: usize, y: usize| {
        let offset = (y * 64 + x) * 4;
        [
            direct.rgba[offset],
            direct.rgba[offset + 1],
            direct.rgba[offset + 2],
            direct.rgba[offset + 3],
        ]
    };
    let close = |actual: [u8; 4], expected: [u8; 4]| {
        actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.abs_diff(expected) <= 1)
    };
    // The title's bar (80, 40)..(240, 100) maps to (16, 8)..(48, 20) of the
    // 64x36 preview; the generator shows around it.
    assert!(close(at(30, 14), [230, 20, 140, 255]), "{:?}", at(30, 14));
    assert!(close(at(4, 30), [12, 34, 56, 255]), "{:?}", at(4, 30));
    let nested = scene_preview(&mut client, 4);
    assert_eq!(nested.rgba, direct.rgba);
    drop(client);
    drop(daemon);
}

#[test]
fn native_daemon_realizes_a_title_input_and_re_renders_it_after_a_field_change() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-title.freemix");
    save_title_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native title integration skipped: {error}");
            return;
        }
    };

    let mut client = Client::connect(daemon.address);
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-title-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Admin,
        resume_cursor: None,
    }));
    assert!(matches!(
        client.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    assert!(matches!(client.receive(), WireMessage::Snapshot(_)));

    // The 320x180 Program monitor frame shows the title's persisted color
    // on its rectangle, over black elsewhere.
    let mut assembler = MonitorFrameAssembler::new();
    let first = program_frame_showing(
        &mut client,
        &mut assembler,
        [230, 20, 140],
        "the persisted title on Program",
    );
    assert_eq!((first.width, first.height), (320, 180));
    assert_eq!(first.input, WireInputId::from_domain(input(2)));

    client.send(&WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "title-fields".into(),
        idempotency_key: "title-fields-key".into(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::SetTitleFields {
            input: WireInputId::from_domain(input(2)),
            base_revision: 0,
            require: TitleAirRequirement::OnAir,
            fields: vec![TitleFieldUpdate {
                field: WireTitleFieldId::new(NonZeroU128::MIN),
                value: fm_protocol::TitleFieldValue::Color {
                    red: 20,
                    green: 200,
                    blue: 90,
                    alpha: 255,
                },
            }],
        },
    }));
    let deadline = Instant::now() + FRAME_TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "no command result");
        if let WireMessage::CommandResult(result) = client.receive() {
            assert!(
                matches!(result, CommandResult::Accepted { .. }),
                "{result:?}"
            );
            break;
        }
    }
    program_frame_showing(
        &mut client,
        &mut assembler,
        [20, 200, 90],
        "the re-rendered title on Program",
    );
    drop(client);
    drop(daemon);
}
