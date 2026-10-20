//! Process-level IN-026 on the simulated runtime: the input lifecycle
//! commands (category list, category, colour label, duplicate, batch
//! import of host media files, pause, close and reopen, source
//! replacement) are accepted, replicated through `input_lifecycle_changed`
//! and the snapshot, journaled, and persisted across restart; a paused
//! media input holds its frame, a closed one releases its source and
//! reopens on it, and a closed input on air is refused.

// Every test here drives the daemon through Unix signals and helpers.
#![cfg(unix)]

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
    Input, InputKind, MainMix, MediaPlaybackSettings, Project, ProjectSettings, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    AddedInput, CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload,
    CommandResult, EventPayload, HandshakeRequest, ImportItemSpec, ImportRefusal,
    InputCategoryStatus, InputColorLabel, InputLifecycleOperation, MediaOperation,
    MediaStateMessage, MediaTransportState, Role, SnapshotMessage, WireGeneratorVideo, WireInputId,
    WireInputSource, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
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
            "freemixd-lifecycle-{}-{sequence}-{name}",
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

/// Input 1 a solid generator on Program, input 2 the media input on
/// Preview (with `settings`), input 3 another solid generator; 25 fps.
fn save_project(path: &Path, settings: MediaPlaybackSettings) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(44).unwrap()),
        "Input lifecycle",
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
    let solid = |red| {
        InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(red, 34, 56, 255)),
            SimulatedAudio::Silence,
        ))
    };
    for (id, name, kind) in [
        (1, "Solid", solid(12)),
        (
            2,
            "Clip",
            InputKind::Media {
                asset_uri: "asset://clip.mkv".into(),
                playback: settings,
            },
        ),
        (3, "Other", solid(200)),
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

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Stops the daemon with SIGTERM, which folds the journal into the
    /// project manifest, and waits for its clean exit.
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
    heartbeat: Heartbeat,
    commands: u64,
}

impl Client {
    /// Connects as an Admin integration client and reads the snapshot and
    /// every follow-on record through the media states.
    fn connect(daemon: &Daemon) -> (Self, SnapshotMessage, Vec<MediaStateMessage>) {
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
            build: "input-lifecycle-test".into(),
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
        while states.len() < usize::from(snapshot.media_states) {
            if let WireMessage::MediaState(state) = client.receive() {
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
}

impl Client {
    /// Submits `payload` and returns its result without waiting for events.
    fn submit(&mut self, payload: &CommandPayload) -> CommandResult {
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
        self.wait_for("the command result", |message| match message {
            WireMessage::CommandResult(result) => Some(result),
            _ => None,
        })
    }

    /// Submits a lifecycle operation and returns its `input_lifecycle_changed`
    /// added inputs and refused items.
    fn lifecycle(
        &mut self,
        operation: InputLifecycleOperation,
    ) -> (Vec<AddedInput>, Vec<(u16, ImportRefusal)>) {
        let payload = CommandPayload::InputLifecycle(operation);
        match self.command(&payload) {
            EventPayload::InputLifecycleChanged {
                operation: changed,
                added,
                refused,
            } => {
                assert_eq!(CommandPayload::InputLifecycle(changed), payload);
                (added, refused)
            }
            other => panic!("expected input_lifecycle_changed, got {other:?}"),
        }
    }
}

fn status(snapshot: &SnapshotMessage, id: u128) -> &fm_protocol::InputStatus {
    snapshot
        .inputs
        .iter()
        .find(|status| status.input == wire(id))
        .unwrap_or_else(|| panic!("input {id} is missing from the snapshot"))
}

/// Reconnects until media input `id` satisfies `done` (given a fresh
/// snapshot and the input's availability, transport state, and position),
/// failing with `what` after a deadline: a media transport advances on the
/// daemon's own frame clock, which a loaded host can delay well past any
/// fixed sleep.
fn wait_for_media(
    daemon: &Daemon,
    id: u128,
    what: &str,
    mut done: impl FnMut(&SnapshotMessage, (bool, MediaTransportState, u64)) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, snapshot, states) = Client::connect(daemon);
        let media = media_position(&states, id);
        if done(&snapshot, media) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {media:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn media_position(states: &[MediaStateMessage], id: u128) -> (bool, MediaTransportState, u64) {
    let state = states
        .iter()
        .find(|state| state.status.input == wire(id))
        .unwrap_or_else(|| panic!("media input {id} has no media_state"));
    let transport = &state.status.transport;
    (transport.available, transport.state, transport.position)
}

#[test]
#[allow(clippy::too_many_lines)]
fn input_lifecycle_commands_replicate_journal_and_persist() {
    let directory = TestDirectory::new("lifecycle");
    let project = directory.project();
    save_project(&project, MediaPlaybackSettings::default());
    let host_clip = directory.0.join("Interview Take.mov");
    fs::write(&host_clip, b"not decoded by the simulated runtime").unwrap();

    let daemon = Daemon::start(&project);
    let (mut client, snapshot, _) = Client::connect(&daemon);
    assert_eq!(snapshot.input_categories, Vec::new());
    assert!(
        snapshot
            .inputs
            .iter()
            .all(|status| status.lifecycle == fm_protocol::InputLifecycleStatus::default())
    );

    let categories = vec![
        InputCategoryStatus {
            id: 1,
            name: "Cameras".into(),
        },
        InputCategoryStatus {
            id: 2,
            name: "Clips".into(),
        },
    ];
    client.lifecycle(InputLifecycleOperation::SetCategories {
        categories: categories.clone(),
    });
    client.lifecycle(InputLifecycleOperation::SetCategory {
        input: wire(3),
        category: Some(1),
    });
    client.lifecycle(InputLifecycleOperation::SetColor {
        input: wire(3),
        color: Some(InputColorLabel::Teal),
    });
    // A category still assigned cannot be dropped.
    assert!(matches!(
        client.submit(&CommandPayload::InputLifecycle(
            InputLifecycleOperation::SetCategories {
                categories: categories[1..].to_vec(),
            }
        )),
        CommandResult::Rejected { .. }
    ));

    // Duplicate copies the labels under a fresh name.
    let (added, refused) = client.lifecycle(InputLifecycleOperation::Duplicate {
        source: wire(3),
        input: wire(10),
        name: None,
    });
    assert_eq!(refused, Vec::new());
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].input, wire(10));
    assert_ne!(added[0].name, "Other");
    assert!(added[0].name.starts_with("Other"), "{}", added[0].name);

    // Batch import: the host file is copied into the bundle; a missing
    // file is refused per item without refusing the batch.
    let (added, refused) = {
        let operation = InputLifecycleOperation::Import {
            items: vec![
                ImportItemSpec {
                    input: wire(11),
                    name: None,
                    uri: host_clip.to_str().unwrap().to_owned(),
                },
                ImportItemSpec {
                    input: wire(12),
                    name: None,
                    uri: directory.0.join("missing.mov").to_str().unwrap().to_owned(),
                },
            ],
        };
        match client.command(&CommandPayload::InputLifecycle(operation)) {
            EventPayload::InputLifecycleChanged {
                operation: InputLifecycleOperation::Import { items },
                added,
                refused,
            } => {
                assert!(items[0].uri.starts_with("asset://"), "{}", items[0].uri);
                (added, refused)
            }
            other => panic!("expected input_lifecycle_changed, got {other:?}"),
        }
    };
    assert_eq!(
        added,
        [AddedInput {
            input: wire(11),
            name: "Interview Take".into(),
        }]
    );
    assert_eq!(refused, [(1, ImportRefusal::Unavailable)]);

    // Program cannot be closed; a source can be replaced.
    assert!(matches!(
        client.submit(&CommandPayload::InputLifecycle(
            InputLifecycleOperation::SetClosed {
                input: wire(1),
                closed: true,
            }
        )),
        CommandResult::Rejected { .. }
    ));
    client.lifecycle(InputLifecycleOperation::ReplaceSource {
        input: wire(3),
        source: WireInputSource::Generator {
            video: WireGeneratorVideo::Bars,
            tone_hz: None,
        },
    });

    // Pause holds a playing media input's frame.
    client.command(&CommandPayload::Media {
        input: wire(2),
        operation: MediaOperation::Play,
    });
    wait_for_media(
        &daemon,
        2,
        "the played input never advanced",
        |_, (_, _, position)| position > 0,
    );
    client.lifecycle(InputLifecycleOperation::SetPaused {
        input: wire(2),
        paused: true,
    });
    let (_, first, first_states) = Client::connect(&daemon);
    assert!(status(&first, 2).lifecycle.paused);
    let (_, _, before) = media_position(&first_states, 2);
    std::thread::sleep(Duration::from_millis(400));
    let (_, _, second_states) = Client::connect(&daemon);
    let (available, state, after) = media_position(&second_states, 2);
    assert!(available);
    assert_eq!(state, MediaTransportState::Playing);
    assert!(before > 0);
    assert_eq!(before, after, "a paused input holds its frame");

    // Close releases the imported clip's source; reopen opens it again.
    client.lifecycle(InputLifecycleOperation::SetClosed {
        input: wire(11),
        closed: true,
    });
    wait_for_media(
        &daemon,
        11,
        "a closed input has no source",
        |snapshot, (available, _, _)| {
            assert!(status(snapshot, 11).lifecycle.closed);
            !available
        },
    );
    client.lifecycle(InputLifecycleOperation::SetClosed {
        input: wire(11),
        closed: false,
    });
    let (_, reopened, reopened_states) = Client::connect(&daemon);
    assert!(!status(&reopened, 11).lifecycle.closed);
    assert!(
        media_position(&reopened_states, 11).0,
        "a reopened input plays again"
    );
    client.lifecycle(InputLifecycleOperation::SetClosed {
        input: wire(10),
        closed: true,
    });
    drop(client);
    daemon.terminate();

    // Everything persists in the project.
    let store = ProjectStore::new(&project).unwrap();
    let stored = store.load().unwrap();
    let model = stored.project();
    assert_eq!(model.input_categories().len(), 2);
    let labelled = model.input_lifecycle(input(3));
    assert_eq!(
        labelled.category.map(fm_model::InputCategoryId::get),
        Some(1)
    );
    assert_eq!(labelled.color, Some(fm_model::InputColorLabel::Teal));
    assert!(model.input_lifecycle(input(2)).paused);
    assert!(model.input_lifecycle(input(10)).closed);
    let kind = |id| {
        model
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input(id))
            .map(|candidate| candidate.kind.clone())
            .unwrap()
    };
    assert!(matches!(
        kind(3),
        InputKind::Simulated(SimulatedInput {
            video: SimulatedVideo::Bars,
            ..
        })
    ));
    assert!(matches!(kind(10), InputKind::Simulated(_)));
    let InputKind::Media { asset_uri, .. } = kind(11) else {
        panic!("the imported input is a media input");
    };
    let key = asset_uri.strip_prefix("asset://").unwrap();
    assert!(project.join("assets").join(key).is_file());

    // And the restarted daemon replicates it.
    let daemon = Daemon::start(&project);
    let (_client, snapshot, _) = Client::connect(&daemon);
    assert_eq!(snapshot.input_categories, categories);
    let three = status(&snapshot, 3).lifecycle;
    assert_eq!(
        (three.category, three.color),
        (Some(1), Some(InputColorLabel::Teal))
    );
    assert!(status(&snapshot, 2).lifecycle.paused);
    assert!(status(&snapshot, 10).lifecycle.closed);
    assert_eq!(status(&snapshot, 11).name, "Interview Take");
    daemon.stop();
}

/// The files in the bundle's `assets/` directory, sorted.
fn asset_files(project: &Path) -> Vec<String> {
    let mut names = fs::read_dir(project.join("assets"))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A refused import or source replacement leaves no copy of its host files
/// in the bundle: the command is validated before anything is copied, and
/// only accepted items are brought in.
#[test]
#[allow(clippy::too_many_lines)]
fn refused_imports_and_replacements_leave_no_copies() {
    let directory = TestDirectory::new("refused-imports");
    let project = directory.project();
    save_project(&project, MediaPlaybackSettings::default());
    let host = |name: &str| {
        let path = directory.0.join(name);
        fs::write(&path, name.as_bytes()).unwrap();
        path.to_str().unwrap().to_owned()
    };
    let locked_clip = host("Locked.mov");
    let taken_id = host("Taken Id.mov");
    let accepted = host("Accepted.mov");
    let unknown_target = host("Unknown Target.mov");
    let generator_target = host("Generator Target.mov");
    let locked_replacement = host("Locked Replacement.mov");

    let daemon = Daemon::start(&project);
    let (mut client, _, _) = Client::connect(&daemon);
    let before = asset_files(&project);

    // Under the project padlock the whole import is refused.
    client.command(&CommandPayload::SetEditLock {
        scope: fm_protocol::EditLockScope::Project,
        locked: true,
    });
    let refused = client.submit(&CommandPayload::InputLifecycle(
        InputLifecycleOperation::Import {
            items: vec![ImportItemSpec {
                input: wire(20),
                name: None,
                uri: locked_clip,
            }],
        },
    ));
    assert!(
        matches!(refused, CommandResult::Rejected { .. }),
        "{refused:?}"
    );
    let refused = client.submit(&CommandPayload::InputLifecycle(
        InputLifecycleOperation::ReplaceSource {
            input: wire(2),
            source: WireInputSource::Media {
                uri: locked_replacement,
            },
        },
    ));
    assert!(
        matches!(refused, CommandResult::Rejected { .. }),
        "{refused:?}"
    );
    assert_eq!(asset_files(&project), before, "a refused command copied");
    client.command(&CommandPayload::SetEditLock {
        scope: fm_protocol::EditLockScope::Project,
        locked: false,
    });

    // An item refused for its input id is not copied; the accepted one is.
    let (added, refused) = client.lifecycle_rewritten(InputLifecycleOperation::Import {
        items: vec![
            ImportItemSpec {
                input: wire(1),
                name: None,
                uri: taken_id,
            },
            ImportItemSpec {
                input: wire(21),
                name: None,
                uri: accepted,
            },
        ],
    });
    assert_eq!(refused, [(0, ImportRefusal::DuplicateInput)]);
    assert_eq!(added.len(), 1);
    let mut expected = before.clone();
    expected.push("Accepted.mov".to_owned());
    expected.sort();
    assert_eq!(asset_files(&project), expected);

    // A replacement refused by the engine copies nothing: an unknown input,
    // and an input without a replaceable source (a title, scene, or colour).
    let refused = client.submit(&CommandPayload::InputLifecycle(
        InputLifecycleOperation::ReplaceSource {
            input: wire(99),
            source: WireInputSource::Media {
                uri: unknown_target,
            },
        },
    ));
    assert!(
        matches!(refused, CommandResult::Rejected { .. }),
        "{refused:?}"
    );
    assert_eq!(asset_files(&project), expected);

    // The refusal keeps its own reason, not `unavailable`.
    let CommandResult::Rejected { fields, .. } = refused else {
        unreachable!();
    };
    assert!(
        fields.iter().all(|field| field.code != "unavailable"),
        "{fields:?}"
    );

    // An accepted replacement still brings its file in.
    client.command(&CommandPayload::InputLifecycle(
        InputLifecycleOperation::ReplaceSource {
            input: wire(3),
            source: WireInputSource::Media {
                uri: generator_target,
            },
        },
    ));
    expected.push("Generator Target.mov".to_owned());
    expected.sort();
    assert_eq!(asset_files(&project), expected);
    drop(client);
    daemon.stop();
}

impl Client {
    /// Like [`Self::lifecycle`] for an operation `freemixd` rewrites (host
    /// paths become `asset://` URIs), so the event's operation differs.
    fn lifecycle_rewritten(
        &mut self,
        operation: InputLifecycleOperation,
    ) -> (Vec<AddedInput>, Vec<(u16, ImportRefusal)>) {
        match self.command(&CommandPayload::InputLifecycle(operation)) {
            EventPayload::InputLifecycleChanged { added, refused, .. } => (added, refused),
            other => panic!("expected input_lifecycle_changed, got {other:?}"),
        }
    }
}
