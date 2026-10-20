//! Process-level IN-006 on the simulated runtime: a media list input is
//! created over the wire, plays its items from the engine's synthetic
//! source in wall time, and the daemon journals an Auto Next `advance` when
//! the active item reaches its out mark. The list (items, marks, options,
//! active item) persists in `project.json`, the journaled advance replays
//! after a restart, and a reconnecting client receives one
//! `media_list_state` per list after the snapshot.

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
    EventPayload, HandshakeRequest, MediaListItemSpec, MediaListNavigation, MediaListOperation,
    MediaListOptionsSpec, MediaListStateMessage, MediaListStatus, MediaOperation, Role,
    SnapshotMessage, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(20);
const LIST: u128 = 9;

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
            "freemixd-media-list-{}-{sequence}-{name}",
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

/// Two solid generators on Program and Preview; 25 fps.
fn save_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(46).unwrap()),
        "Media lists",
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

    #[cfg(unix)]
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
    commands: u64,
}

impl Client {
    /// Connects as an Admin integration client and reads the snapshot and
    /// its `media_list_state` records.
    fn connect(daemon: &Daemon) -> (Self, SnapshotMessage, Vec<MediaListStateMessage>) {
        let stream = TcpStream::connect(daemon.address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            commands: 0,
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "media-list-test".into(),
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
        while states.len() < usize::from(snapshot.media_list_states) {
            if let WireMessage::MediaListState(state) = client.receive() {
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

    fn list(&mut self, operation: MediaListOperation) -> MediaListStatus {
        match self.command(&CommandPayload::MediaList {
            input: wire(LIST),
            operation,
        }) {
            EventPayload::MediaListChanged { status, .. } => *status,
            other => panic!("expected media_list_changed, got {other:?}"),
        }
    }
}

/// Three items of a clip, a still-free WAV sting, and another clip; the
/// first two are ten frames long.
fn items() -> Vec<MediaListItemSpec> {
    vec![
        MediaListItemSpec {
            id: 1,
            uri: "asset://opener.mkv".into(),
            title: Some("Opener".into()),
            mark_in: 20,
            mark_out: Some(30),
        },
        MediaListItemSpec {
            id: 2,
            uri: "asset://sting.wav".into(),
            title: None,
            mark_in: 0,
            mark_out: Some(10),
        },
        MediaListItemSpec {
            id: 3,
            uri: "asset://closer.mov".into(),
            title: Some("Closer".into()),
            mark_in: 0,
            mark_out: None,
        },
    ]
}

#[cfg(unix)]
#[test]
#[allow(clippy::too_many_lines)]
fn a_media_list_auto_advances_in_wall_time_persists_and_replays() {
    let directory = TestDirectory::new("auto-next");
    let project = directory.project();
    save_project(&project);
    let daemon = Daemon::start(&project);
    let (mut client, snapshot, states) = Client::connect(&daemon);
    assert_eq!(snapshot.media_list_states, 0);
    assert_eq!(states.len(), 0);

    let created = client.command(&CommandPayload::MediaList {
        input: wire(LIST),
        operation: MediaListOperation::Create {
            name: "Walk-in".into(),
            items: items(),
            options: MediaListOptionsSpec {
                auto_next: true,
                ..MediaListOptionsSpec::default()
            },
        },
    });
    let EventPayload::MediaListChanged {
        operation,
        created,
        status,
        ..
    } = created
    else {
        panic!("expected media_list_changed, got {created:?}");
    };
    assert_eq!((operation, created.as_deref()), ("create", Some("Walk-in")));
    assert_eq!((status.active, status.order.clone()), (1, vec![1, 2, 3]));

    // Playing the opener runs its ten frames in wall time; at its out mark
    // the daemon journals the advance and the sting plays.
    let started = Instant::now();
    client.command(&CommandPayload::Media {
        input: wire(LIST),
        operation: MediaOperation::Play,
    });
    let advanced = client.wait_for("the Auto Next advance", |message| {
        let WireMessage::Event(event) = message else {
            return None;
        };
        match event.payload {
            EventPayload::MediaListChanged {
                operation: "advance",
                status,
                ..
            } => Some(*status),
            _ => None,
        }
    });
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "ten frames at 25 fps ended in {:?}",
        started.elapsed()
    );
    assert_eq!(advanced.active, 2);
    // The sting ends too, and the closer follows.
    let advanced = client.wait_for("the second advance", |message| {
        let WireMessage::Event(event) = message else {
            return None;
        };
        match event.payload {
            EventPayload::MediaListChanged {
                operation: "advance",
                status,
                ..
            } => Some(*status),
            _ => None,
        }
    });
    assert_eq!(advanced.active, 3);

    // The operator stops, steps back to the sting, and edits its marks.
    client.command(&CommandPayload::Media {
        input: wire(LIST),
        operation: MediaOperation::Stop,
    });
    let back = client.list(MediaListOperation::Navigate(MediaListNavigation::Previous));
    assert_eq!(back.active, 2);
    let mut edited = items();
    edited[1].mark_in = 2;
    let set = client.list(MediaListOperation::Set {
        items: edited.clone(),
        options: MediaListOptionsSpec {
            auto_next: false,
            loop_list: true,
            ..MediaListOptionsSpec::default()
        },
    });
    assert_eq!((set.active, set.items.clone()), (2, edited.clone()));
    drop(client);
    daemon.terminate();

    // The list persists with the sting active, which is the input's source.
    let stored = ProjectStore::new(&project).unwrap().load().unwrap();
    let list = stored.project().media_list(input(LIST)).unwrap();
    assert_eq!(list.active, 2);
    assert_eq!(list.items[1].mark_in, 2);
    assert!(list.options.loop_list && !list.options.auto_next);
    let kind = &stored
        .project()
        .inputs()
        .iter()
        .find(|candidate| candidate.id == input(LIST))
        .unwrap()
        .kind;
    assert!(
        matches!(kind, InputKind::Media { asset_uri, .. } if asset_uri == "asset://sting.wav"),
        "{kind:?}"
    );

    // A reconnecting client receives the list after the snapshot.
    let daemon = Daemon::start(&project);
    let (_client, snapshot, states) = Client::connect(&daemon);
    assert_eq!(snapshot.media_list_states, 1);
    assert_eq!(states[0].status.input, wire(LIST));
    assert_eq!(states[0].status.active, 2);
    assert_eq!(states[0].status.items, edited);
    daemon.stop();
}

#[test]
fn a_journaled_auto_next_advance_replays_after_a_crash() {
    let directory = TestDirectory::new("replay");
    let project = directory.project();
    save_project(&project);
    let daemon = Daemon::start(&project);
    let (mut client, _, _) = Client::connect(&daemon);
    client.list(MediaListOperation::Create {
        name: "Loop".into(),
        items: items(),
        options: MediaListOptionsSpec {
            auto_next: true,
            shuffle_seed: Some(7),
            ..MediaListOptionsSpec::default()
        },
    });
    let first = client.list(MediaListOperation::Navigate(MediaListNavigation::First));
    client.command(&CommandPayload::Media {
        input: wire(LIST),
        operation: MediaOperation::Play,
    });
    let advanced = client.wait_for("the Auto Next advance", |message| {
        let WireMessage::Event(event) = message else {
            return None;
        };
        match event.payload {
            EventPayload::MediaListChanged {
                operation: "advance",
                status,
                ..
            } => Some(*status),
            _ => None,
        }
    });
    assert_eq!(advanced.order, first.order);
    assert_eq!(advanced.position, 1);
    drop(client);
    // Killed without folding the journal: the advance replays from it.
    daemon.stop();
    let daemon = Daemon::start(&project);
    let (_client, _, states) = Client::connect(&daemon);
    assert!(states[0].status.position >= 1, "{:?}", states[0].status);
    assert_eq!(states[0].status.order, first.order);
    daemon.stop();
}
