//! Process-level IN-008 and IN-009 on the simulated runtime: an image input
//! and a folder slideshow named by host paths are copied into the project,
//! announced with their sources, replicated through `still_state` records
//! after the snapshot, and stepped with `page_navigate`; the server follows
//! the slideshow's project folder live and answers slide list requests.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventPayload, HandshakeRequest, ImageInputConfig, LiveInputSource, PagedNavigation, Role,
    RuntimeLifecycleEvent, SlideOrder, SlideTransitionStyle, SlideshowConfig, SlideshowPage,
    SlideshowPageStatus, SlideshowSlidesRequest, SlideshowSource, SnapshotMessage,
    StillStateMessage, WireInputId, WireMessage, WireStillSource, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputScaling, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

#[path = "support/heartbeat.rs"]
mod heartbeat;
use heartbeat::Heartbeat;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(20);

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "freemixd-stills-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn project(&self) -> PathBuf {
        self.0.join("show.freemix")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes a `width`x`height` PNG of one colour.
fn png(path: &Path, width: u32, height: u32, rgba: [u8; 4]) {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
    image.save(path).unwrap();
}

/// Two solid generators, 1 on Program and 2 on Preview; 25 fps, 320x180.
fn save_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(45).unwrap()),
        "Stills",
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
    for (id, red) in [(1, 12), (2, 200)] {
        project.add_input(Input {
            id: input(id),
            name: format!("Solid {id}"),
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

struct Daemon {
    child: Child,
    address: SocketAddr,
}

impl Daemon {
    fn start(project: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = sender.send(line);
        });
        let line = receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_default();
        let Ok(readiness) = line.parse::<ReadinessRecord>() else {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr = String::new();
            let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
            panic!("daemon did not start: {line:?} {stderr}");
        };
        Self {
            child,
            address: readiness.address,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    heartbeat: Heartbeat,
    commands: u64,
}

impl Client {
    /// Connects as an Admin integration client; returns the snapshot and
    /// its `still_state` records, the last follow-on records.
    fn connect(daemon: &Daemon) -> (Self, SnapshotMessage, Vec<StillStateMessage>) {
        let stream = TcpStream::connect(daemon.address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            heartbeat: Heartbeat::new(),
            commands: 0,
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "stills-test".into(),
            client_type: ClientType::Integration,
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
        while states.len() < usize::from(snapshot.still_states) {
            if let WireMessage::StillState(state) = client.receive() {
                states.push(state);
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

    fn wait_for<T>(&mut self, what: &str, mut found: impl FnMut(WireMessage) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            if let Some(value) = found(self.receive()) {
                return value;
            }
        }
    }

    /// Submits `payload` and waits for its acceptance and its durable event.
    fn command(&mut self, payload: &CommandPayload) -> EventPayload {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id.clone(),
            expected_revision: None,
            deadline_ms: None,
            payload: payload.clone(),
        }));
        let mut revision = None;
        let mut event = None;
        self.wait_for("the command's event", |message| {
            match message {
                WireMessage::CommandResult(CommandResult::Accepted { revision: at, .. }) => {
                    revision = Some(at);
                }
                WireMessage::CommandResult(result) => panic!("{payload:?}: {result:?}"),
                WireMessage::Event(message)
                    if revision.is_none_or(|at| at == message.cursor.revision)
                        && event.is_none() =>
                {
                    event = Some((message.cursor.revision, message.payload));
                }
                _ => {}
            }
            match (&revision, &event) {
                (Some(at), Some((event_at, payload))) if at == event_at => Some(payload.clone()),
                _ => None,
            }
        })
    }

    /// Waits for a `slideshow_page` runtime event of input `id` satisfying
    /// `done`.
    fn wait_for_page(
        &mut self,
        id: u128,
        what: &str,
        done: impl Fn(&SlideshowPageStatus) -> bool,
    ) -> SlideshowPageStatus {
        self.wait_for(what, |message| match message {
            WireMessage::RuntimeEvent(event) => match event.event {
                RuntimeLifecycleEvent::SlideshowPage { input, status }
                    if input == wire(id) && done(&status) =>
                {
                    Some(*status)
                }
                _ => None,
            },
            _ => None,
        })
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn images_and_folder_slideshows_import_replicate_step_and_follow_their_folder() {
    let directory = TestDirectory::new("stills");
    let project = directory.project();
    save_project(&project);
    let host = directory.0.join("host");
    let slides = host.join("Event Slides");
    fs::create_dir_all(&slides).unwrap();
    png(&host.join("logo.png"), 64, 32, [255, 0, 0, 128]);
    png(&slides.join("10.png"), 16, 9, [0, 0, 255, 255]);
    png(&slides.join("2.png"), 16, 9, [0, 255, 0, 255]);
    png(&slides.join("1.png"), 16, 9, [255, 255, 0, 255]);
    fs::write(slides.join("notes.txt"), "not a slide").unwrap();

    let daemon = Daemon::start(&project);
    let (mut client, snapshot, states) = Client::connect(&daemon);
    assert_eq!((snapshot.still_states, states.len()), (0, 0));

    // An image named by a host path is copied into the project.
    let image = ImageInputConfig::new(host.join("logo.png").to_string_lossy(), OutputScaling::Fill);
    let added = client.command(&CommandPayload::AddInput {
        input: wire(5),
        name: "Logo".into(),
        source: LiveInputSource::Image(Box::new(image)),
    });
    let EventPayload::InputAdded { input, still, .. } = added else {
        panic!("expected input_added, got {added:?}");
    };
    assert_eq!(input, wire(5));
    assert_eq!(
        still.as_deref(),
        Some(&WireStillSource::Image(ImageInputConfig::new(
            "asset://logo.png",
            OutputScaling::Fill
        )))
    );
    assert!(project.join("assets/logo.png").is_file());

    // A folder named by a host path becomes a new project folder of its
    // pictures, which the slideshow then follows.
    let mut config = SlideshowConfig::new(SlideshowSource::Folder {
        asset_uri: slides.to_string_lossy().into_owned(),
    });
    config.transition = SlideTransitionStyle::Cut;
    config.auto_advance = false;
    config.loop_show = false;
    config.order = SlideOrder::Ordered;
    let added = client.command(&CommandPayload::AddInput {
        input: wire(6),
        name: "Slides".into(),
        source: LiveInputSource::Slideshow(Box::new(config.clone())),
    });
    let EventPayload::InputAdded { still, .. } = added else {
        panic!("expected input_added, got {added:?}");
    };
    let Some(WireStillSource::Slideshow(added)) = still.as_deref() else {
        panic!("expected a slideshow source, got {still:?}");
    };
    let folder_uri = "asset://Event Slides";
    assert_eq!(
        added.source,
        SlideshowSource::Folder {
            asset_uri: folder_uri.into()
        }
    );
    let folder = project.join("assets/Event Slides");
    let mut copied = fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    copied.sort();
    assert_eq!(copied, ["1.png", "10.png", "2.png"]);

    // The server lists the folder in natural order and reports the first
    // slide.
    let first = client.wait_for_page(6, "the listed folder", |status| status.page.count == 3);
    assert_eq!(first.page, SlideshowPage { index: 0, count: 3 });
    assert_eq!(first.label, "1.png");

    // Next steps one slide, as a live operation with its own event.
    let navigated = client.command(&CommandPayload::PageNavigate {
        input: wire(6),
        navigation: PagedNavigation::Next,
    });
    assert!(matches!(
        navigated,
        EventPayload::PageNavigated {
            input,
            navigation: PagedNavigation::Next,
            ..
        } if input == wire(6)
    ));
    let second = client.wait_for_page(6, "the second slide", |status| status.page.index == 1);
    assert_eq!(second.label, "2.png");

    // The slide list in windows.
    client.send(&WireMessage::SlideshowSlidesRequest(
        SlideshowSlidesRequest {
            request_id: "slides-1".into(),
            input: wire(6),
            offset: 1,
            limit: 8,
        },
    ));
    let listing = client.wait_for("the slide list", |message| match message {
        WireMessage::SlideshowSlides(listing) => Some(listing),
        _ => None,
    });
    assert_eq!(listing.request_id, "slides-1");
    assert_eq!((listing.offset, listing.total), (1, 3));
    assert_eq!(
        listing
            .slides
            .iter()
            .map(|slide| slide.asset_uri.as_str())
            .collect::<Vec<_>>(),
        ["asset://Event Slides/2.png", "asset://Event Slides/10.png"]
    );

    // A picture added to the project folder joins the show live.
    png(&folder.join("3.png"), 16, 9, [9, 9, 9, 255]);
    let grown = client.wait_for_page(6, "the grown folder", |status| status.page.count == 4);
    assert_eq!(grown.label, "2.png", "the slide on air stays");

    // A new session receives both inputs' still states after the snapshot.
    drop(client);
    let (_client, snapshot, states) = Client::connect(&daemon);
    assert_eq!(snapshot.still_states, 2);
    assert_eq!(states[0].input, wire(5));
    assert!(matches!(states[0].source, WireStillSource::Image(_)));
    assert_eq!(states[1].input, wire(6));
    assert_eq!(states[1].status.page, SlideshowPage { index: 1, count: 4 });
    assert_eq!(states[1].status.label, "2.png");
}

#[test]
fn a_missing_host_picture_is_refused_and_not_copied() {
    let directory = TestDirectory::new("refused");
    let project = directory.project();
    save_project(&project);
    let daemon = Daemon::start(&project);
    let (mut client, _, _) = Client::connect(&daemon);
    // A host path that is no file stays as it is, which the engine refuses.
    client.commands += 1;
    client.send(&WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "missing".into(),
        idempotency_key: "missing".into(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::AddInput {
            input: wire(5),
            name: "Missing".into(),
            source: LiveInputSource::Image(Box::new(ImageInputConfig::new(
                directory.0.join("missing.png").to_string_lossy(),
                OutputScaling::Fit,
            ))),
        },
    }));
    let result = client.wait_for("the refusal", |message| match message {
        WireMessage::CommandResult(result) => Some(result),
        _ => None,
    });
    assert!(
        matches!(result, CommandResult::Rejected { .. }),
        "{result:?}"
    );
    assert!(!project.join("assets/missing.png").exists());
}
