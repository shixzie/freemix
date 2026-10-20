//! Process-level IN-014 on the simulated runtime: a host PDF and a
//! `PowerPoint` presentation import as document inputs; the PDF reports its
//! page count and title, steps page by page (clamped at both ends) through
//! `page_navigate`, answers presenter notes and page thumbnails, and lands
//! on the same page after a crash (journal replay) and a clean restart; a
//! presentation without `LibreOffice` still counts its slides and serves its
//! notes, and says how to show it; with a converter it converts and
//! becomes ready.

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
use fm_presentations::sample::{SampleSlide, SolidPage, pptx, solid_pages_pdf};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    DocumentState, EventPayload, HandshakeRequest, ImportItemSpec, InputLifecycleOperation,
    PagedNavigation, PagedPageContent, PagedPageRequest, PagedStatus, Role, SnapshotMessage,
    WireInputId, WireMessage, decode_line, decode_qoi_rgb, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(30);
const DECK: u128 = 10;
const TALK: u128 = 11;

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
            "freemixd-documents-{}-{sequence}-{name}",
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

/// Two solid generators on Program and Preview; 25 fps, 320x180.
fn save_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(45).unwrap()),
        "Documents",
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

/// A three-page 4:3 PDF: red, green, blue.
fn deck() -> Vec<u8> {
    let page = |rgb| SolidPage {
        width: 400,
        height: 300,
        rgb,
    };
    solid_pages_pdf(
        Some("Quarterly review"),
        &[page([255, 0, 0]), page([0, 255, 0]), page([0, 0, 255])],
    )
}

struct Daemon {
    child: Child,
    address: SocketAddr,
}

impl Daemon {
    /// Starts the simulated daemon on `project`, converting presentations
    /// with `converter` (an empty value disables conversion).
    fn start(project: &Path, converter: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .env("FREEMIX_SOFFICE", converter)
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
            .recv_timeout(Duration::from_secs(20))
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

    /// Kills the daemon without a checkpoint, so the next start replays
    /// the journal.
    fn crash(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Stops the daemon with SIGTERM, which folds the journal into the
    /// project manifest.
    #[cfg(unix)]
    fn terminate(mut self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "daemon exited with {status}");
                return;
            }
            assert!(Instant::now() < deadline, "daemon did not stop");
            std::thread::sleep(Duration::from_millis(10));
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
    commands: u64,
}

impl Client {
    /// Connects as an Admin integration client and reads the snapshot and
    /// every follow-on record through the paged states, the last ones.
    fn connect(daemon: &Daemon) -> (Self, SnapshotMessage, Vec<PagedStatus>) {
        let stream = TcpStream::connect(daemon.address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            commands: 0,
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "documents-test".into(),
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
        while states.len() < usize::from(snapshot.paged_states) {
            if let WireMessage::PagedState(state) = client.receive() {
                assert_eq!(state.revision, snapshot.revision);
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
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed"
        );
        decode_line(&line).unwrap()
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

    fn submit_message(&mut self, payload: &CommandPayload) {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload: payload.clone(),
        }));
    }

    /// Submits `payload` and waits for its acceptance and its durable event.
    fn command(&mut self, payload: &CommandPayload) -> EventPayload {
        self.submit_message(payload);
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

    /// Steps `id` and returns the status the daemon replicated.
    fn step(&mut self, id: u128, navigation: PagedNavigation) -> PagedStatus {
        match self.command(&CommandPayload::PageNavigate {
            input: wire(id),
            navigation,
        }) {
            EventPayload::PagedChanged {
                navigation: stepped,
                status,
            } => {
                assert_eq!(stepped, navigation);
                *status
            }
            other => panic!("expected paged_changed, got {other:?}"),
        }
    }

    /// Asks for one page's notes or thumbnail and waits for the answer.
    fn page(&mut self, id: u128, page: u32, thumbnail: bool) -> PagedPageContent {
        let request_id = format!("page-{id}-{page}-{}", u8::from(thumbnail));
        self.send(&WireMessage::PagedPageRequest(PagedPageRequest {
            request_id: request_id.clone(),
            input: wire(id),
            page,
            thumbnail,
        }));
        self.wait_for("the page answer", |message| match message {
            WireMessage::PagedPage(answer) if answer.request_id == request_id => {
                assert_eq!((answer.input, answer.page), (wire(id), page));
                Some(answer.content)
            }
            _ => None,
        })
    }

    fn import(&mut self, items: Vec<ImportItemSpec>) {
        match self.command(&CommandPayload::InputLifecycle(
            InputLifecycleOperation::Import { items },
        )) {
            EventPayload::InputLifecycleChanged { refused, .. } => {
                assert_eq!(refused, Vec::new());
            }
            other => panic!("expected input_lifecycle_changed, got {other:?}"),
        }
    }
}

/// Reconnects until document input `id` satisfies `done`.
fn wait_for_document(
    daemon: &Daemon,
    id: u128,
    what: &str,
    mut done: impl FnMut(&PagedStatus) -> bool,
) -> PagedStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, _, states) = Client::connect(daemon);
        if let Some(status) = states.iter().find(|status| status.input == wire(id))
            && done(status)
        {
            return status.clone();
        }
        assert!(Instant::now() < deadline, "{what}: {states:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The colour at the centre of a page thumbnail.
fn centre(content: &PagedPageContent) -> [u8; 3] {
    let PagedPageContent::Thumbnail {
        width,
        height,
        data,
    } = content
    else {
        panic!("expected a thumbnail, got {content:?}");
    };
    let rgb = decode_qoi_rgb(*width, *height, data).unwrap();
    let index = (usize::from(*height / 2) * usize::from(*width) + usize::from(*width / 2)) * 3;
    [rgb[index], rgb[index + 1], rgb[index + 2]]
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)] // one import, navigation, crash, and restart run
fn a_pdf_steps_page_by_page_and_survives_replay_and_restart() {
    let directory = TestDirectory::new("pdf");
    let project = directory.project();
    save_project(&project);
    let host_deck = directory.0.join("Quarterly Deck.pdf");
    fs::write(&host_deck, deck()).unwrap();
    let no_converter = PathBuf::new();

    let daemon = Daemon::start(&project, &no_converter);
    let (mut client, snapshot, states) = Client::connect(&daemon);
    assert_eq!((snapshot.paged_states, states.len()), (0, 0));
    client.import(vec![ImportItemSpec {
        input: wire(DECK),
        name: None,
        uri: host_deck.display().to_string(),
    }]);
    let ready = wait_for_document(&daemon, DECK, "the deck loads", |status| {
        status.state == DocumentState::Ready
    });
    assert_eq!((ready.page, ready.count), (0, 3));
    assert_eq!(ready.title.as_deref(), Some("Quarterly review"));

    // Navigation clamps at both ends.
    assert_eq!(client.step(DECK, PagedNavigation::Previous).page, 0);
    assert_eq!(client.step(DECK, PagedNavigation::Next).page, 1);
    assert_eq!(client.step(DECK, PagedNavigation::Goto(2)).page, 2);
    let last = client.step(DECK, PagedNavigation::Next);
    assert_eq!(
        (last.page, last.count, last.state),
        (2, 3, DocumentState::Ready)
    );

    // Notes (a PDF has none) and thumbnails of any page.
    assert_eq!(
        client.page(DECK, 0, false),
        PagedPageContent::Notes(String::new())
    );
    assert_eq!(centre(&client.page(DECK, 0, true)), [255, 0, 0]);
    assert_eq!(centre(&client.page(DECK, 2, true)), [0, 0, 255]);
    assert_eq!(client.page(DECK, 7, true), PagedPageContent::Unavailable);
    assert_eq!(client.page(1, 0, false), PagedPageContent::Unavailable);

    // A non-document refuses navigation.
    client.submit_message(&CommandPayload::PageNavigate {
        input: wire(1),
        navigation: PagedNavigation::Next,
    });
    let refused = client.wait_for("the refusal", |message| match message {
        WireMessage::CommandResult(result) => Some(result),
        _ => None,
    });
    assert!(
        matches!(&refused, CommandResult::Rejected { fields, .. } if fields[0].code == "not_paged"),
        "{refused:?}"
    );

    // A crash replays the journal: the import and every step land again.
    client.step(DECK, PagedNavigation::Previous);
    drop(client);
    daemon.crash();
    let daemon = Daemon::start(&project, &no_converter);
    let replayed = wait_for_document(&daemon, DECK, "the replayed deck", |status| {
        status.state == DocumentState::Ready
    });
    assert_eq!((replayed.page, replayed.count), (1, 3));

    // A clean stop keeps the page in the project.
    daemon.terminate();
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    let document = stored.project().document(input(DECK)).unwrap();
    assert_eq!(document.page, 1);
    assert!(document.asset_uri.starts_with("asset://"));
    let daemon = Daemon::start(&project, &no_converter);
    let restarted = wait_for_document(&daemon, DECK, "the restarted deck", |status| {
        status.state == DocumentState::Ready
    });
    assert_eq!(restarted.page, 1);
}

#[cfg(unix)]
#[test]
fn a_presentation_counts_and_serves_notes_without_libreoffice_and_converts_with_it() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TestDirectory::new("pptx");
    let project = directory.project();
    save_project(&project);
    let host_talk = directory.0.join("Launch Talk.pptx");
    let slides = [
        SampleSlide {
            notes: Some("Welcome everyone.\nIntroduce the team."),
            hidden: false,
        },
        SampleSlide {
            notes: None,
            hidden: true,
        },
        SampleSlide {
            notes: Some("Thank you."),
            hidden: false,
        },
    ];
    fs::write(&host_talk, pptx(Some("Launch"), &slides)).unwrap();

    let daemon = Daemon::start(&project, Path::new(""));
    let (mut client, _, _) = Client::connect(&daemon);
    client.import(vec![ImportItemSpec {
        input: wire(TALK),
        name: None,
        uri: host_talk.display().to_string(),
    }]);
    let unavailable = wait_for_document(&daemon, TALK, "the talk is probed", |status| {
        status.count > 0
    });
    assert_eq!(unavailable.count, 2, "a hidden slide is not shown");
    assert_eq!(unavailable.state, DocumentState::Unavailable);
    assert_eq!(
        unavailable.message.as_deref(),
        Some(fm_presentations::CONVERTER_UNAVAILABLE)
    );
    assert_eq!(unavailable.title.as_deref(), Some("Launch"));
    assert_eq!(
        client.page(TALK, 0, false),
        PagedPageContent::Notes("Welcome everyone.\nIntroduce the team.".into())
    );
    assert_eq!(
        client.page(TALK, 1, false),
        PagedPageContent::Notes("Thank you.".into())
    );
    assert_eq!(client.page(TALK, 0, true), PagedPageContent::Unavailable);
    // Its slides step even though they cannot be shown.
    assert_eq!(client.step(TALK, PagedNavigation::Last).page, 1);
    drop(client);
    daemon.terminate();

    // A converter (a stand-in that writes a PDF where LibreOffice would)
    // makes it ready.
    let converted_pdf = directory.0.join("converted.pdf");
    fs::write(&converted_pdf, deck()).unwrap();
    let converter = directory.0.join("soffice");
    fs::write(
        &converter,
        format!(
            "#!/bin/sh\nwhile [ \"$1\" != \"--outdir\" ]; do shift; done\ncp '{}' \"$2/presentation.pdf\"\n",
            converted_pdf.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&converter, fs::Permissions::from_mode(0o755)).unwrap();
    let daemon = Daemon::start(&project, &converter);
    let ready = wait_for_document(&daemon, TALK, "the talk converts", |status| {
        status.state == DocumentState::Ready
    });
    assert_eq!((ready.page, ready.count), (1, 2));
    let (mut client, _, _) = Client::connect(&daemon);
    assert_eq!(centre(&client.page(TALK, 1, true)), [0, 255, 0]);
}
